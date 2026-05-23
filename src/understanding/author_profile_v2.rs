use std::collections::{BTreeMap, BTreeSet};

use anyhow::{Result, anyhow};
use serde_json::Value;
use sha2::{Digest, Sha256};

use crate::schemas::author_profile::{
    AUTHOR_PROFILE_V2_SCHEMA_VERSION, AuthorProfileV2, AuthorProfileV2Claim,
    AuthorProfileV2SupportRef, AuthorRepresentativeWorkV2, AuthorResearchThemeV2,
};
use crate::schemas::paper_profile::{PaperProfileV2, PaperProfileV2Evidence, PaperProfileV2Fact};
use crate::storage::PaperProfileV2Record;

use super::json_utils::parse_json_object;
use super::llm::OpenAiCompatibleClient;
use super::prompts::{AUTHOR_PROFILE_V2_PROMPT_VERSION, author_profile_v2_synthesis_messages};

#[derive(Debug, Clone)]
pub struct AuthorProfileV2Seed {
    pub author: String,
    pub profiles: Vec<Value>,
}

#[derive(Debug, Clone)]
struct ThemeAccumulator {
    label: String,
    supporting_papers: BTreeSet<String>,
    support_refs: Vec<AuthorProfileV2SupportRef>,
    methods: Vec<AuthorProfileV2Claim>,
    key_results: Vec<AuthorProfileV2Claim>,
    limitations: Vec<AuthorProfileV2Claim>,
    years: BTreeSet<String>,
    titles: Vec<String>,
}

pub fn build_author_profile_v2(seed: AuthorProfileV2Seed) -> Result<Value> {
    let mut profile = deterministic_profile(seed)?;
    AuthorProfileV2::from_value(profile.clone())?.validate(
        profile
            .get("author")
            .and_then(Value::as_str)
            .unwrap_or_default(),
    )?;
    if let Some(object) = profile.as_object_mut() {
        object.insert(
            "profile_schema_version".to_string(),
            AUTHOR_PROFILE_V2_SCHEMA_VERSION.into(),
        );
    }
    Ok(profile)
}

pub fn build_author_profile_v2_with_llm(
    seed: AuthorProfileV2Seed,
    llm: &OpenAiCompatibleClient,
) -> Result<Value> {
    let mut profile = build_author_profile_v2(seed)?;
    let response = llm.chat(author_profile_v2_synthesis_messages(&profile), 0.1, 2600)?;
    let synthesis = parse_json_object(&response);
    apply_synthesis(&mut profile, &synthesis)?;
    let author = profile
        .get("author")
        .and_then(Value::as_str)
        .unwrap_or_default();
    AuthorProfileV2::from_value(profile.clone())?.validate(author)?;
    Ok(profile)
}

pub fn author_profile_v2_source_hash(records: &[PaperProfileV2Record]) -> Result<String> {
    let mut sorted = records.to_vec();
    sorted.sort_by(|left, right| left.paper_key.cmp(&right.paper_key));
    let mut digest = Sha256::new();
    for record in sorted {
        digest.update(record.paper_key.as_bytes());
        digest.update(b"\0");
        digest.update(record.profile_schema_version.to_string().as_bytes());
        digest.update(b"\0");
        digest.update(record.builder_version.as_bytes());
        digest.update(b"\0");
        digest.update(record.model_id.as_bytes());
        digest.update(b"\0");
        digest.update(record.source_fact_hash.as_bytes());
        digest.update(b"\0");
        digest.update(serde_json::to_string(&record.profile_json)?.as_bytes());
        digest.update(b"\n");
    }
    Ok(format!("author-profile-v2-source:{:x}", digest.finalize()))
}

fn deterministic_profile(seed: AuthorProfileV2Seed) -> Result<Value> {
    if seed.profiles.is_empty() {
        return Err(anyhow!(
            "AuthorProfileV2 requires at least one PaperProfileV2"
        ));
    }
    let mut paper_profiles = seed
        .profiles
        .into_iter()
        .map(PaperProfileV2::from_value)
        .collect::<Result<Vec<_>>>()?;
    for profile in &paper_profiles {
        profile.validate()?;
    }
    paper_profiles.sort_by(|left, right| {
        right
            .year
            .cmp(&left.year)
            .then_with(|| left.title.cmp(&right.title))
            .then_with(|| left.paper_key.cmp(&right.paper_key))
    });

    let mut source_profile_keys = paper_profiles
        .iter()
        .map(|profile| profile.paper_key.clone())
        .collect::<Vec<_>>();
    source_profile_keys.sort();
    source_profile_keys.dedup();

    let mut themes: BTreeMap<String, ThemeAccumulator> = BTreeMap::new();
    let mut representative_works = Vec::new();
    let mut evolution = Vec::new();
    let mut methodological_strengths = Vec::new();

    for profile in &paper_profiles {
        let theme_key = theme_key(profile);
        let theme_label = theme_label(profile, &theme_key);
        let facts_by_uid = profile
            .factual_objects
            .iter()
            .map(|fact| (fact.claim_uid.as_str(), fact))
            .collect::<BTreeMap<_, _>>();
        let representative_ref = representative_support_ref(profile)?;
        let representative_claim = AuthorProfileV2Claim {
            claim: format!(
                "{} ({}) contributes to {}.",
                profile.title,
                empty_as(&profile.year, "unknown year"),
                theme_label
            ),
            support_refs: vec![representative_ref.clone()],
        };
        evolution.push(representative_claim);
        representative_works.push(AuthorRepresentativeWorkV2 {
            paper_key: profile.paper_key.clone(),
            title: profile.title.clone(),
            doi: profile.doi.clone(),
            year: profile.year.clone(),
            reason: profile.one_sentence_summary.clone(),
            support_refs: vec![representative_ref.clone()],
        });

        let accumulator = themes.entry(theme_key).or_insert_with(|| ThemeAccumulator {
            label: theme_label.clone(),
            supporting_papers: BTreeSet::new(),
            support_refs: Vec::new(),
            methods: Vec::new(),
            key_results: Vec::new(),
            limitations: Vec::new(),
            years: BTreeSet::new(),
            titles: Vec::new(),
        });
        accumulator
            .supporting_papers
            .insert(profile.paper_key.clone());
        accumulator.support_refs.push(representative_ref);
        if !profile.year.trim().is_empty() {
            accumulator.years.insert(profile.year.clone());
        }
        accumulator.titles.push(profile.title.clone());

        for fact in &profile.factual_objects {
            let claim = AuthorProfileV2Claim {
                claim: fact.claim.clone(),
                support_refs: support_refs_for_fact(profile, fact),
            };
            match fact.fact_type.as_str() {
                "method" => {
                    accumulator.methods.push(claim.clone());
                    methodological_strengths.push(claim);
                }
                "result" | "metric" | "mechanism" | "dataset" => {
                    accumulator.key_results.push(claim);
                }
                "limitation" => accumulator.limitations.push(claim),
                _ => {}
            }
        }
        if accumulator.key_results.is_empty() {
            for contribution in &profile.main_contributions {
                let Some(fact) = facts_by_uid.get(contribution.claim_uid.as_str()) else {
                    continue;
                };
                accumulator.key_results.push(AuthorProfileV2Claim {
                    claim: contribution.claim.clone(),
                    support_refs: support_refs_for_fact(profile, fact),
                });
            }
        }
    }

    let mut research_themes = themes
        .into_values()
        .map(theme_from_accumulator)
        .collect::<Vec<_>>();
    research_themes.sort_by(|left, right| {
        right
            .supporting_papers
            .len()
            .cmp(&left.supporting_papers.len())
            .then_with(|| left.theme.cmp(&right.theme))
    });
    truncate_claims(&mut evolution, 12);
    truncate_claims(&mut methodological_strengths, 12);
    representative_works.truncate(12);

    let profile = AuthorProfileV2 {
        author: seed.author.clone(),
        total_profiled_papers: paper_profiles.len(),
        research_themes,
        research_evolution: evolution,
        methodological_strengths,
        representative_works,
        source_profile_keys,
        builder_version: AUTHOR_PROFILE_V2_PROMPT_VERSION.to_string(),
        extra: BTreeMap::new(),
    };
    let value = serde_json::to_value(profile)?;
    AuthorProfileV2::from_value(value.clone())?.validate(&seed.author)?;
    Ok(value)
}

fn theme_from_accumulator(mut accumulator: ThemeAccumulator) -> AuthorResearchThemeV2 {
    truncate_refs(&mut accumulator.support_refs, 12);
    truncate_claims(&mut accumulator.methods, 8);
    truncate_claims(&mut accumulator.key_results, 8);
    truncate_claims(&mut accumulator.limitations, 8);
    let supporting_papers = accumulator
        .supporting_papers
        .into_iter()
        .collect::<Vec<_>>();
    let time_span = years_to_span(accumulator.years);
    let title_preview = accumulator
        .titles
        .iter()
        .take(3)
        .cloned()
        .collect::<Vec<_>>()
        .join("; ");
    let summary = if supporting_papers.len() == 1 {
        format!("This theme is supported by {title_preview}.")
    } else {
        format!(
            "This theme is supported by {} profiled papers, including {title_preview}.",
            supporting_papers.len()
        )
    };
    AuthorResearchThemeV2 {
        theme: accumulator.label,
        summary,
        supporting_papers,
        support_refs: accumulator.support_refs,
        methods: accumulator.methods,
        key_results: accumulator.key_results,
        limitations_or_open_questions: accumulator.limitations,
        time_span,
        confidence: if title_preview.is_empty() {
            "low".to_string()
        } else {
            "medium".to_string()
        },
    }
}

fn apply_synthesis(profile: &mut Value, synthesis: &Value) -> Result<()> {
    let deterministic = AuthorProfileV2::from_value(profile.clone())?;
    let support_refs = support_ref_map(&deterministic);
    let Some(object) = profile.as_object_mut() else {
        return Err(anyhow!("AuthorProfileV2 profile is not a JSON object"));
    };
    if let Some(themes) = synthesized_themes(synthesis.get("research_themes"), &support_refs) {
        object.insert("research_themes".to_string(), serde_json::to_value(themes)?);
    }
    if let Some(claims) = synthesized_claims(synthesis.get("research_evolution"), &support_refs, 12)
    {
        object.insert(
            "research_evolution".to_string(),
            serde_json::to_value(claims)?,
        );
    }
    if let Some(claims) =
        synthesized_claims(synthesis.get("methodological_strengths"), &support_refs, 12)
    {
        object.insert(
            "methodological_strengths".to_string(),
            serde_json::to_value(claims)?,
        );
    }
    if let Some(works) =
        synthesized_representative_works(synthesis.get("representative_works"), &support_refs)
    {
        object.insert(
            "representative_works".to_string(),
            serde_json::to_value(works)?,
        );
    }
    Ok(())
}

fn synthesized_themes(
    value: Option<&Value>,
    support_refs: &BTreeMap<String, AuthorProfileV2SupportRef>,
) -> Option<Vec<AuthorResearchThemeV2>> {
    let mut themes = value?
        .as_array()?
        .iter()
        .filter_map(|item| {
            let theme = nonempty_str(item.get("theme")?)?;
            let summary = nonempty_str(item.get("summary")?)?;
            let refs = refs_from_uids(item.get("support_uids"), support_refs, 12)?;
            let supporting_papers = refs
                .iter()
                .map(|reference| reference.paper_key.clone())
                .collect::<BTreeSet<_>>()
                .into_iter()
                .collect::<Vec<_>>();
            let years = refs
                .iter()
                .filter_map(|reference| {
                    if reference.year.trim().is_empty() {
                        None
                    } else {
                        Some(reference.year.clone())
                    }
                })
                .collect::<BTreeSet<_>>();
            Some(AuthorResearchThemeV2 {
                theme: theme.to_string(),
                summary: summary.to_string(),
                supporting_papers,
                support_refs: refs,
                methods: synthesized_claims(item.get("methods"), support_refs, 8)
                    .unwrap_or_default(),
                key_results: synthesized_claims(item.get("key_results"), support_refs, 8)
                    .unwrap_or_default(),
                limitations_or_open_questions: synthesized_claims(
                    item.get("limitations_or_open_questions"),
                    support_refs,
                    8,
                )
                .unwrap_or_default(),
                time_span: string_array(item.get("time_span"))
                    .unwrap_or_else(|| years_to_span(years)),
                confidence: item
                    .get("confidence")
                    .and_then(Value::as_str)
                    .map(str::trim)
                    .filter(|value| !value.is_empty())
                    .unwrap_or("medium")
                    .to_string(),
            })
        })
        .collect::<Vec<_>>();
    if themes.is_empty() {
        None
    } else {
        themes.truncate(12);
        Some(themes)
    }
}

fn synthesized_claims(
    value: Option<&Value>,
    support_refs: &BTreeMap<String, AuthorProfileV2SupportRef>,
    limit: usize,
) -> Option<Vec<AuthorProfileV2Claim>> {
    let mut claims = value?
        .as_array()?
        .iter()
        .filter_map(|item| {
            let claim = nonempty_str(item.get("claim")?)?;
            let refs = refs_from_uids(item.get("support_uids"), support_refs, 8)?;
            Some(AuthorProfileV2Claim {
                claim: claim.to_string(),
                support_refs: refs,
            })
        })
        .collect::<Vec<_>>();
    if claims.is_empty() {
        None
    } else {
        claims.truncate(limit);
        Some(claims)
    }
}

fn synthesized_representative_works(
    value: Option<&Value>,
    support_refs: &BTreeMap<String, AuthorProfileV2SupportRef>,
) -> Option<Vec<AuthorRepresentativeWorkV2>> {
    let mut works = value?
        .as_array()?
        .iter()
        .filter_map(|item| {
            let reason = nonempty_str(item.get("reason")?)?;
            let refs = refs_from_uids(item.get("support_uids"), support_refs, 8)?;
            let first_ref = refs.first()?;
            let paper_key = item
                .get("paper_key")
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .unwrap_or(first_ref.paper_key.as_str());
            if paper_key != first_ref.paper_key {
                return None;
            }
            Some(AuthorRepresentativeWorkV2 {
                paper_key: paper_key.to_string(),
                title: item
                    .get("title")
                    .and_then(Value::as_str)
                    .map(str::trim)
                    .filter(|value| !value.is_empty())
                    .unwrap_or(first_ref.title.as_str())
                    .to_string(),
                doi: first_ref.doi.clone(),
                year: first_ref.year.clone(),
                reason: reason.to_string(),
                support_refs: refs,
            })
        })
        .collect::<Vec<_>>();
    if works.is_empty() {
        None
    } else {
        works.truncate(12);
        Some(works)
    }
}

fn support_ref_map(profile: &AuthorProfileV2) -> BTreeMap<String, AuthorProfileV2SupportRef> {
    let mut refs = BTreeMap::new();
    for theme in &profile.research_themes {
        collect_refs(&theme.support_refs, &mut refs);
        for claim in theme
            .methods
            .iter()
            .chain(theme.key_results.iter())
            .chain(theme.limitations_or_open_questions.iter())
        {
            collect_refs(&claim.support_refs, &mut refs);
        }
    }
    for claim in profile
        .research_evolution
        .iter()
        .chain(profile.methodological_strengths.iter())
    {
        collect_refs(&claim.support_refs, &mut refs);
    }
    for work in &profile.representative_works {
        collect_refs(&work.support_refs, &mut refs);
    }
    refs
}

fn collect_refs(
    source: &[AuthorProfileV2SupportRef],
    target: &mut BTreeMap<String, AuthorProfileV2SupportRef>,
) {
    for reference in source {
        target.insert(reference.support_uid.clone(), reference.clone());
    }
}

fn refs_from_uids(
    value: Option<&Value>,
    support_refs: &BTreeMap<String, AuthorProfileV2SupportRef>,
    limit: usize,
) -> Option<Vec<AuthorProfileV2SupportRef>> {
    let mut refs = value?
        .as_array()?
        .iter()
        .filter_map(Value::as_str)
        .filter_map(|uid| support_refs.get(uid).cloned())
        .collect::<Vec<_>>();
    refs.sort_by(|left, right| left.support_uid.cmp(&right.support_uid));
    refs.dedup_by(|left, right| left.support_uid == right.support_uid);
    if refs.is_empty() {
        None
    } else {
        refs.truncate(limit);
        Some(refs)
    }
}

fn support_refs_for_fact(
    profile: &PaperProfileV2,
    fact: &PaperProfileV2Fact,
) -> Vec<AuthorProfileV2SupportRef> {
    fact.evidence
        .iter()
        .map(|evidence| support_ref_from_evidence(profile, evidence))
        .collect()
}

fn support_ref_from_evidence(
    profile: &PaperProfileV2,
    evidence: &PaperProfileV2Evidence,
) -> AuthorProfileV2SupportRef {
    AuthorProfileV2SupportRef {
        support_uid: format!(
            "{}#{}#{}",
            profile.paper_key, evidence.claim_uid, evidence.chunk_fact_id
        ),
        paper_key: profile.paper_key.clone(),
        title: profile.title.clone(),
        doi: profile.doi.clone(),
        year: profile.year.clone(),
        claim_uid: evidence.claim_uid.clone(),
        chunk_fact_id: evidence.chunk_fact_id,
        chunk_id: evidence.chunk_id,
        section: evidence.section.clone(),
        source_hash: evidence.source_hash.clone(),
        chunk_hash: evidence.chunk_hash.clone(),
    }
}

fn representative_support_ref(profile: &PaperProfileV2) -> Result<AuthorProfileV2SupportRef> {
    if let Some(claim) = profile.main_contributions.first()
        && let Some(reference) = claim.support_refs.first()
    {
        return Ok(support_ref_from_evidence(profile, reference));
    }
    let fact = profile
        .factual_objects
        .first()
        .ok_or_else(|| anyhow!("PaperProfileV2 has no factual_objects"))?;
    fact.evidence
        .first()
        .map(|evidence| support_ref_from_evidence(profile, evidence))
        .ok_or_else(|| anyhow!("PaperProfileV2 factual object has no evidence"))
}

fn theme_key(profile: &PaperProfileV2) -> String {
    profile
        .topic_keywords
        .first()
        .or_else(|| profile.contribution_types.first())
        .map(|value| normalize_theme(value))
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| "general research".to_string())
}

fn theme_label(profile: &PaperProfileV2, theme_key: &str) -> String {
    profile
        .topic_keywords
        .first()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| theme_key.to_string())
}

fn normalize_theme(value: &str) -> String {
    value
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .to_lowercase()
}

fn years_to_span(years: BTreeSet<String>) -> Vec<String> {
    let years = years
        .into_iter()
        .filter(|year| !year.trim().is_empty())
        .collect::<Vec<_>>();
    match (years.first(), years.last()) {
        (Some(first), Some(last)) if first != last => vec![first.clone(), last.clone()],
        (Some(year), _) => vec![year.clone()],
        _ => vec!["unknown".to_string()],
    }
}

fn truncate_claims(claims: &mut Vec<AuthorProfileV2Claim>, limit: usize) {
    claims.truncate(limit);
}

fn truncate_refs(refs: &mut Vec<AuthorProfileV2SupportRef>, limit: usize) {
    refs.sort_by(|left, right| left.support_uid.cmp(&right.support_uid));
    refs.dedup_by(|left, right| left.support_uid == right.support_uid);
    refs.truncate(limit);
}

fn nonempty_str(value: &Value) -> Option<&str> {
    value
        .as_str()
        .map(str::trim)
        .filter(|value| !value.is_empty())
}

fn string_array(value: Option<&Value>) -> Option<Vec<String>> {
    let values = value?
        .as_array()?
        .iter()
        .filter_map(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
        .collect::<Vec<_>>();
    if values.is_empty() {
        None
    } else {
        Some(values)
    }
}

fn empty_as<'a>(value: &'a str, fallback: &'a str) -> &'a str {
    if value.trim().is_empty() {
        fallback
    } else {
        value
    }
}

#[cfg(test)]
mod tests {
    use serde_json::{Value, json};

    use super::{AuthorProfileV2Seed, apply_synthesis, build_author_profile_v2};
    use crate::schemas::author_profile::AuthorProfileV2;

    struct PaperProfileFixture<'a> {
        paper_key: &'a str,
        title: &'a str,
        year: &'a str,
        keyword: &'a str,
        fact_type: &'a str,
        claim: &'a str,
        chunk_fact_id: i64,
        claim_uid: &'a str,
    }

    #[test]
    fn builds_author_profile_v2_with_theme_support_refs() {
        let profile = build_author_profile_v2(AuthorProfileV2Seed {
            author: "Alice".to_string(),
            profiles: vec![
                paper_profile(PaperProfileFixture {
                    paper_key: "Alice/paper-a",
                    title: "MOF Catalysis",
                    year: "2024",
                    keyword: "MOF",
                    fact_type: "method",
                    claim: "The method screens solvents for MOF catalysis.",
                    chunk_fact_id: 1,
                    claim_uid: "fact-a",
                }),
                paper_profile(PaperProfileFixture {
                    paper_key: "Alice/paper-b",
                    title: "MOF Conversion",
                    year: "2025",
                    keyword: "MOF",
                    fact_type: "result",
                    claim: "The best condition reaches 82% conversion.",
                    chunk_fact_id: 2,
                    claim_uid: "fact-b",
                }),
            ],
        })
        .unwrap();
        let profile = AuthorProfileV2::from_value(profile).unwrap();

        profile.validate("Alice").unwrap();
        assert_eq!(profile.total_profiled_papers, 2);
        assert_eq!(profile.research_themes.len(), 1);
        assert_eq!(profile.research_themes[0].supporting_papers.len(), 2);
        assert!(!profile.research_themes[0].support_refs.is_empty());
        assert!(!profile.representative_works[0].support_refs.is_empty());
    }

    #[test]
    fn synthesis_can_only_use_existing_support_uids() {
        let mut profile = build_author_profile_v2(AuthorProfileV2Seed {
            author: "Alice".to_string(),
            profiles: vec![paper_profile(PaperProfileFixture {
                paper_key: "Alice/paper-a",
                title: "MOF Catalysis",
                year: "2024",
                keyword: "MOF",
                fact_type: "method",
                claim: "The method screens solvents for MOF catalysis.",
                chunk_fact_id: 1,
                claim_uid: "fact-a",
            })],
        })
        .unwrap();
        apply_synthesis(
            &mut profile,
            &json!({
                "research_themes": [{
                    "theme": "Unsupported",
                    "summary": "Should be ignored.",
                    "support_uids": ["missing"]
                }],
                "methodological_strengths": [{
                    "claim": "This rewrite is grounded.",
                    "support_uids": ["Alice/paper-a#fact-a#1"]
                }]
            }),
        )
        .unwrap();
        let profile = AuthorProfileV2::from_value(profile).unwrap();

        profile.validate("Alice").unwrap();
        assert_eq!(profile.research_themes[0].theme, "MOF");
        assert_eq!(
            profile.methodological_strengths[0].claim,
            "This rewrite is grounded."
        );
    }

    fn paper_profile(fixture: PaperProfileFixture<'_>) -> Value {
        let paper_key = fixture.paper_key;
        let title = fixture.title;
        let year = fixture.year;
        let keyword = fixture.keyword;
        let fact_type = fixture.fact_type;
        let claim = fixture.claim;
        let chunk_fact_id = fixture.chunk_fact_id;
        let claim_uid = fixture.claim_uid;
        json!({
            "paper_key": paper_key,
            "title": title,
            "doi": "10.1/test",
            "year": year,
            "one_sentence_summary": claim,
            "contribution_types": [fact_type],
            "topic_keywords": [keyword],
            "main_contributions": [{
                "claim_uid": claim_uid,
                "chunk_fact_id": chunk_fact_id,
                "claim": claim,
                "support_refs": [{
                    "paper_key": paper_key,
                    "chunk_fact_id": chunk_fact_id,
                    "claim_uid": claim_uid,
                    "chunk_id": chunk_fact_id + 10,
                    "chunk_index": chunk_fact_id,
                    "section": "Abstract",
                    "source_hash": "source",
                    "chunk_hash": format!("chunk-{chunk_fact_id}")
                }]
            }],
            "limitations_or_open_questions": [],
            "factual_objects": [{
                "claim_uid": claim_uid,
                "chunk_fact_id": chunk_fact_id,
                "fact_type": fact_type,
                "claim": claim,
                "confidence": "medium",
                "section": "Abstract",
                "chunk_index": chunk_fact_id,
                "evidence": [{
                    "paper_key": paper_key,
                    "chunk_fact_id": chunk_fact_id,
                    "claim_uid": claim_uid,
                    "chunk_id": chunk_fact_id + 10,
                    "chunk_index": chunk_fact_id,
                    "section": "Abstract",
                    "source_hash": "source",
                    "chunk_hash": format!("chunk-{chunk_fact_id}")
                }],
                "source_text_excerpt": claim
            }],
            "source_fact_uids": [claim_uid],
            "builder_version": "paper-profile-v2-s3"
        })
    }
}
