use std::collections::{BTreeMap, BTreeSet};

use anyhow::{Result, anyhow};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

use crate::schemas::paper_profile::{
    PAPER_PROFILE_V2_SCHEMA_VERSION, PaperProfileV2, PaperProfileV2Claim, PaperProfileV2Evidence,
    PaperProfileV2Fact,
};
use crate::storage::ChunkFact;

use super::json_utils::parse_json_object;
use super::llm::OpenAiCompatibleClient;
use super::prompts::{PAPER_PROFILE_V2_PROMPT_VERSION, paper_profile_v2_synthesis_messages};

#[derive(Debug, Clone)]
pub struct PaperProfileV2Seed {
    pub paper_key: String,
    pub title: String,
    pub doi: String,
    pub year: String,
    pub facts: Vec<ChunkFact>,
}

pub fn build_paper_profile_v2(seed: PaperProfileV2Seed) -> Result<Value> {
    let mut profile = deterministic_profile(seed)?;
    PaperProfileV2::from_value(profile.clone())?.validate()?;
    if let Some(object) = profile.as_object_mut() {
        object.insert(
            "profile_schema_version".to_string(),
            PAPER_PROFILE_V2_SCHEMA_VERSION.into(),
        );
    }
    Ok(profile)
}

pub fn build_paper_profile_v2_with_llm(
    seed: PaperProfileV2Seed,
    llm: &OpenAiCompatibleClient,
) -> Result<Value> {
    let mut profile = build_paper_profile_v2(seed)?;
    let response = llm.chat(paper_profile_v2_synthesis_messages(&profile), 0.1, 1800)?;
    let synthesis = parse_json_object(&response);
    apply_synthesis(&mut profile, &synthesis)?;
    PaperProfileV2::from_value(profile.clone())?.validate()?;
    Ok(profile)
}

pub fn source_fact_hash(facts: &[ChunkFact]) -> Result<String> {
    let mut digest = Sha256::new();
    for fact in facts {
        digest.update(fact.claim_uid.as_bytes());
        digest.update(b"\0");
        digest.update(fact.chunk_fact_id.to_string().as_bytes());
        digest.update(b"\0");
        digest.update(fact.source_hash.as_bytes());
        digest.update(b"\0");
        digest.update(fact.chunk_hash.as_bytes());
        digest.update(b"\0");
        digest.update(fact.fact_json.as_bytes());
        digest.update(b"\n");
    }
    Ok(format!("paper-profile-v2-source:{:x}", digest.finalize()))
}

fn deterministic_profile(seed: PaperProfileV2Seed) -> Result<Value> {
    if seed.facts.is_empty() {
        return Err(anyhow!("PaperProfileV2 requires at least one chunk fact"));
    }
    let facts = dedupe_and_rank_facts(seed.facts)?;
    let source_fact_uids = facts
        .iter()
        .map(|fact| fact.claim_uid.clone())
        .collect::<Vec<_>>();
    let contribution_types = contribution_types(&facts);
    let topic_keywords = topic_keywords(&seed.title, &facts);
    let main_contributions = facts
        .iter()
        .filter(|fact| !matches!(fact.fact_type.as_str(), "limitation" | "context"))
        .take(6)
        .map(claim_from_fact)
        .collect::<Vec<_>>();
    let limitations = facts
        .iter()
        .filter(|fact| fact.fact_type == "limitation")
        .take(6)
        .map(claim_from_fact)
        .collect::<Vec<_>>();
    let summary = deterministic_summary(&seed.title, &facts);
    let profile = PaperProfileV2 {
        paper_key: seed.paper_key,
        title: seed.title,
        doi: seed.doi,
        year: seed.year,
        one_sentence_summary: summary,
        contribution_types,
        topic_keywords,
        main_contributions,
        limitations_or_open_questions: limitations,
        factual_objects: facts,
        source_fact_uids,
        builder_version: PAPER_PROFILE_V2_PROMPT_VERSION.to_string(),
        extra: BTreeMap::new(),
    };
    serde_json::to_value(profile).map_err(Into::into)
}

fn dedupe_and_rank_facts(facts: Vec<ChunkFact>) -> Result<Vec<PaperProfileV2Fact>> {
    let mut seen = BTreeSet::new();
    let mut output = Vec::new();
    for fact in facts {
        let fact_json: Value = serde_json::from_str(&fact.fact_json)?;
        let claim = fact_json
            .get("claim")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .trim()
            .to_string();
        if claim.is_empty() {
            continue;
        }
        let dedupe_key = format!("{}\0{}", fact.fact_type, normalize_claim(&claim));
        if !seen.insert(dedupe_key) {
            continue;
        }
        let chunk_index = fact_json
            .get("chunk_index")
            .and_then(Value::as_i64)
            .unwrap_or_default();
        let section = fact_json
            .get("section")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string();
        let evidence = PaperProfileV2Evidence {
            paper_key: fact.paper_key.clone(),
            chunk_fact_id: fact.chunk_fact_id,
            claim_uid: fact.claim_uid.clone(),
            chunk_id: fact.chunk_id,
            chunk_index,
            section: section.clone(),
            source_hash: fact.source_hash.clone(),
            chunk_hash: fact.chunk_hash.clone(),
        };
        let source_text_excerpt = fact_json
            .get("source_text")
            .and_then(Value::as_str)
            .map(|text| take_chars(text, 500))
            .unwrap_or_default();
        output.push(PaperProfileV2Fact {
            claim_uid: fact.claim_uid,
            chunk_fact_id: fact.chunk_fact_id,
            fact_type: fact.fact_type,
            claim,
            confidence: fact.confidence.unwrap_or_default(),
            section,
            chunk_index,
            evidence: vec![evidence],
            source_text_excerpt,
        });
    }
    output.sort_by(|left, right| {
        fact_rank(&right.fact_type)
            .cmp(&fact_rank(&left.fact_type))
            .then(left.chunk_index.cmp(&right.chunk_index))
            .then(left.claim_uid.cmp(&right.claim_uid))
    });
    Ok(output)
}

fn apply_synthesis(profile: &mut Value, synthesis: &Value) -> Result<()> {
    let fact_map = factual_object_map(profile)?;
    let Some(object) = profile.as_object_mut() else {
        return Err(anyhow!("PaperProfileV2 profile is not a JSON object"));
    };
    if let Some(summary) = synthesis
        .get("one_sentence_summary")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
    {
        object.insert("one_sentence_summary".to_string(), summary.into());
    }
    for field in ["contribution_types", "topic_keywords"] {
        if let Some(values) = string_array(synthesis.get(field)) {
            object.insert(field.to_string(), json!(values));
        }
    }
    if let Some(claims) = synthesized_claims(synthesis.get("main_contributions"), &fact_map) {
        object.insert(
            "main_contributions".to_string(),
            serde_json::to_value(claims)?,
        );
    }
    if let Some(claims) =
        synthesized_claims(synthesis.get("limitations_or_open_questions"), &fact_map)
    {
        object.insert(
            "limitations_or_open_questions".to_string(),
            serde_json::to_value(claims)?,
        );
    }
    Ok(())
}

fn factual_object_map(profile: &Value) -> Result<BTreeMap<String, PaperProfileV2Fact>> {
    let profile = PaperProfileV2::from_value(profile.clone())?;
    Ok(profile
        .factual_objects
        .into_iter()
        .map(|fact| (fact.claim_uid.clone(), fact))
        .collect())
}

fn synthesized_claims(
    value: Option<&Value>,
    fact_map: &BTreeMap<String, PaperProfileV2Fact>,
) -> Option<Vec<PaperProfileV2Claim>> {
    let items = value?.as_array()?;
    let claims = items
        .iter()
        .filter_map(|item| {
            let claim_uid = item.get("claim_uid")?.as_str()?;
            let fact = fact_map.get(claim_uid)?;
            let claim = item
                .get("claim")
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .unwrap_or(&fact.claim);
            Some(PaperProfileV2Claim {
                claim_uid: fact.claim_uid.clone(),
                chunk_fact_id: fact.chunk_fact_id,
                claim: claim.to_string(),
                support_refs: fact.evidence.clone(),
            })
        })
        .collect::<Vec<_>>();
    if claims.is_empty() {
        None
    } else {
        Some(claims)
    }
}

fn claim_from_fact(fact: &PaperProfileV2Fact) -> PaperProfileV2Claim {
    PaperProfileV2Claim {
        claim_uid: fact.claim_uid.clone(),
        chunk_fact_id: fact.chunk_fact_id,
        claim: fact.claim.clone(),
        support_refs: fact.evidence.clone(),
    }
}

fn contribution_types(facts: &[PaperProfileV2Fact]) -> Vec<String> {
    let mut values = facts
        .iter()
        .filter(|fact| !matches!(fact.fact_type.as_str(), "context"))
        .map(|fact| fact.fact_type.clone())
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect::<Vec<_>>();
    values.sort_by(|left, right| {
        fact_rank(right)
            .cmp(&fact_rank(left))
            .then_with(|| left.cmp(right))
    });
    values
}

fn topic_keywords(title: &str, facts: &[PaperProfileV2Fact]) -> Vec<String> {
    let mut words = BTreeSet::new();
    for text in std::iter::once(title).chain(facts.iter().map(|fact| fact.claim.as_str())) {
        for word in text.split(|ch: char| !ch.is_alphanumeric() && ch != '-') {
            let word = word.trim();
            if word.len() >= 4
                && !matches!(
                    word.to_lowercase().as_str(),
                    "this" | "that" | "with" | "from" | "under" | "paper" | "study" | "using"
                )
            {
                words.insert(word.to_string());
            }
            if words.len() >= 12 {
                return words.into_iter().collect();
            }
        }
    }
    words.into_iter().collect()
}

fn deterministic_summary(title: &str, facts: &[PaperProfileV2Fact]) -> String {
    let preferred = facts
        .iter()
        .find(|fact| fact.fact_type == "context")
        .or_else(|| facts.iter().find(|fact| fact.fact_type == "result"))
        .or_else(|| facts.first());
    if let Some(fact) = preferred {
        format!("{title}: {}", fact.claim)
    } else {
        title.to_string()
    }
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

fn fact_rank(fact_type: &str) -> i32 {
    match fact_type {
        "result" => 90,
        "method" => 80,
        "mechanism" => 75,
        "dataset" => 70,
        "metric" => 65,
        "limitation" => 60,
        "context" => 40,
        "figure_caption" | "table_caption" => 30,
        _ => 10,
    }
}

fn normalize_claim(value: &str) -> String {
    value.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn take_chars(value: &str, max_chars: usize) -> String {
    value.chars().take(max_chars).collect()
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::{PaperProfileV2Seed, apply_synthesis, build_paper_profile_v2};
    use crate::schemas::paper_profile::PaperProfileV2;
    use crate::storage::ChunkFact;

    #[test]
    fn builds_paper_profile_v2_with_fact_ids_and_evidence() {
        let profile = build_paper_profile_v2(PaperProfileV2Seed {
            paper_key: "Alice/paper-a".to_string(),
            title: "A Paper".to_string(),
            doi: "10.1/test".to_string(),
            year: "2024".to_string(),
            facts: vec![
                fact(1, "context", "This paper studies MOF catalysis.", 10),
                fact(
                    2,
                    "result",
                    "The best condition reports 82% conversion.",
                    11,
                ),
            ],
        })
        .unwrap();

        let profile = PaperProfileV2::from_value(profile).unwrap();
        profile.validate().unwrap();
        assert_eq!(profile.factual_objects.len(), 2);
        assert_eq!(profile.main_contributions[0].chunk_fact_id, 2);
        assert_eq!(profile.main_contributions[0].support_refs[0].chunk_id, 11);
    }

    #[test]
    fn synthesis_can_rewrite_claims_only_for_known_fact_uids() {
        let mut profile = build_paper_profile_v2(PaperProfileV2Seed {
            paper_key: "Alice/paper-a".to_string(),
            title: "A Paper".to_string(),
            doi: "10.1/test".to_string(),
            year: "2024".to_string(),
            facts: vec![fact(
                2,
                "result",
                "The best condition reports 82% conversion.",
                11,
            )],
        })
        .unwrap();

        apply_synthesis(
            &mut profile,
            &json!({
                "one_sentence_summary": "A Paper reports a strong conversion result.",
                "contribution_types": ["result"],
                "main_contributions": [
                    {"claim_uid": "claim-2", "claim": "Reports 82% conversion."},
                    {"claim_uid": "not-allowed", "claim": "This should be ignored."}
                ]
            }),
        )
        .unwrap();

        let profile = PaperProfileV2::from_value(profile).unwrap();
        profile.validate().unwrap();
        assert_eq!(
            profile.one_sentence_summary,
            "A Paper reports a strong conversion result."
        );
        assert_eq!(profile.main_contributions.len(), 1);
        assert_eq!(profile.main_contributions[0].claim_uid, "claim-2");
        assert_eq!(profile.main_contributions[0].chunk_fact_id, 2);
        assert_eq!(
            profile.main_contributions[0].claim,
            "Reports 82% conversion."
        );
    }

    fn fact(id: i64, fact_type: &str, claim: &str, chunk_id: i64) -> ChunkFact {
        ChunkFact {
            chunk_fact_id: id,
            claim_uid: format!("claim-{id}"),
            paper_key: "Alice/paper-a".to_string(),
            chunk_id,
            fact_type: fact_type.to_string(),
            fact_json: json!({
                "claim": claim,
                "section": "Results",
                "chunk_index": id - 1,
                "source_text": claim
            })
            .to_string(),
            confidence: Some("high".to_string()),
            extractor: "chunk_fact_extractor".to_string(),
            extractor_version: "chunk-facts-v1".to_string(),
            source_hash: "source".to_string(),
            chunk_hash: format!("chunk-{id}"),
            created_at: "now".to_string(),
        }
    }
}
