use std::collections::{BTreeMap, BTreeSet};

use anyhow::Result;
use serde_json::Value;

use crate::schemas::author_profile::{AUTHOR_PROFILE_V2_SCHEMA_VERSION, AuthorProfileV2};
use crate::schemas::paper_profile::{PAPER_PROFILE_V2_SCHEMA_VERSION, PaperProfileV2};
use crate::storage::{NewAuthorProfileV2, NewPaperProfileV2, Storage};
use crate::understanding::author_profile_v2::{
    AuthorProfileV2Seed, author_profile_v2_source_hash, build_author_profile_v2,
    build_author_profile_v2_with_llm,
};
use crate::understanding::chunk_fact_extractor::{
    CHUNK_FACT_EXTRACTOR, CHUNK_FACT_EXTRACTOR_VERSION,
};
use crate::understanding::llm::OpenAiCompatibleClient;
use crate::understanding::paper_profile_v2::{
    PaperProfileV2Seed, build_paper_profile_v2, build_paper_profile_v2_with_llm, source_fact_hash,
};
use crate::understanding::prompts::{
    AUTHOR_PROFILE_V2_PROMPT_VERSION, PAPER_PROFILE_V2_PROMPT_VERSION,
};

const DETERMINISTIC_MODEL_ID: &str = "deterministic";

#[derive(Debug, Clone, Copy, Default)]
pub struct S3ComprehensionOptions {
    pub limit: Option<usize>,
    pub force: bool,
    pub dry_run: bool,
    pub profiled_only: bool,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct S3ComprehensionReport {
    pub papers_scanned: usize,
    pub built: usize,
    pub changed: usize,
    pub skipped_current: usize,
    pub missing_chunk_facts: usize,
    pub failed: usize,
    pub by_fact_type: BTreeMap<String, usize>,
    pub dry_run: bool,
    pub model_id: String,
}

#[derive(Debug, Clone, Copy, Default)]
pub struct S4AuthorComprehensionOptions {
    pub limit: Option<usize>,
    pub force: bool,
    pub dry_run: bool,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct S4AuthorComprehensionReport {
    pub paper_profiles_scanned: usize,
    pub built: usize,
    pub changed: usize,
    pub skipped_current: usize,
    pub missing_paper_profiles: usize,
    pub research_themes: usize,
    pub dry_run: bool,
    pub model_id: String,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ProfileDiffReport {
    pub papers_with_v1: usize,
    pub papers_with_v2: usize,
    pub missing_v2: Vec<String>,
    pub missing_v1: Vec<String>,
    pub changed_summaries: Vec<ProfileSummaryDiff>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProfileSummaryDiff {
    pub paper_key: String,
    pub title: String,
    pub v1_summary: String,
    pub v2_summary: String,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ProfileGateReport {
    pub ready: bool,
    pub papers_with_v1: usize,
    pub papers_with_v2: usize,
    pub missing_v2: Vec<String>,
    pub missing_v1: Vec<String>,
    pub invalid_v2_profiles: Vec<ProfileGateIssue>,
    pub author_profile_v2_present: bool,
    pub author_profile_v2_valid: bool,
    pub author_profile_v2_error: Option<String>,
    pub factual_objects: usize,
    pub claims_with_support_refs: usize,
    pub support_refs: usize,
    pub blockers: Vec<String>,
    pub warnings: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProfileGateIssue {
    pub paper_key: String,
    pub error: String,
}

pub struct ComprehensionService<'a> {
    storage: &'a Storage,
}

impl<'a> ComprehensionService<'a> {
    pub fn new(storage: &'a Storage) -> Self {
        Self { storage }
    }

    pub fn comprehend_author_v2(
        &self,
        author: &str,
        options: S3ComprehensionOptions,
        llm: Option<&OpenAiCompatibleClient>,
    ) -> Result<S3ComprehensionReport> {
        let mut paper_keys = self.storage.paper_keys_with_current_chunk_facts(
            author,
            CHUNK_FACT_EXTRACTOR,
            CHUNK_FACT_EXTRACTOR_VERSION,
            options.limit,
        )?;
        if options.profiled_only {
            let profiled_keys = self
                .storage
                .paper_profiles(author, None)?
                .into_iter()
                .filter_map(|profile| {
                    profile
                        .get("paper_key")
                        .and_then(Value::as_str)
                        .map(str::to_string)
                })
                .collect::<BTreeSet<_>>();
            paper_keys.retain(|paper_key| profiled_keys.contains(paper_key));
        }
        let model_id = llm
            .map(OpenAiCompatibleClient::model_name)
            .unwrap_or(DETERMINISTIC_MODEL_ID)
            .to_string();
        let mut report = S3ComprehensionReport {
            papers_scanned: paper_keys.len(),
            dry_run: options.dry_run,
            model_id: model_id.clone(),
            ..S3ComprehensionReport::default()
        };
        for paper_key in paper_keys {
            let facts = self.storage.current_chunk_facts_for_paper(
                &paper_key,
                CHUNK_FACT_EXTRACTOR,
                CHUNK_FACT_EXTRACTOR_VERSION,
            )?;
            if facts.is_empty() {
                report.missing_chunk_facts += 1;
                continue;
            }
            for fact in &facts {
                *report
                    .by_fact_type
                    .entry(fact.fact_type.clone())
                    .or_default() += 1;
            }
            let source_hash = source_fact_hash(&facts)?;
            if !options.force
                && self.storage.paper_profile_v2_is_current(
                    &paper_key,
                    PAPER_PROFILE_V2_SCHEMA_VERSION,
                    PAPER_PROFILE_V2_PROMPT_VERSION,
                    &model_id,
                    &source_hash,
                )?
            {
                report.skipped_current += 1;
                continue;
            }
            let seed = seed_from_facts(&paper_key, facts);
            let profile = match llm {
                Some(llm) => build_paper_profile_v2_with_llm(seed, llm),
                None => build_paper_profile_v2(seed),
            };
            let profile = match profile {
                Ok(profile) => profile,
                Err(_) => {
                    report.failed += 1;
                    continue;
                }
            };
            report.built += 1;
            if options.dry_run {
                continue;
            }
            if self.storage.save_paper_profile_v2(NewPaperProfileV2 {
                paper_key: &paper_key,
                profile_json: &profile,
                profile_schema_version: PAPER_PROFILE_V2_SCHEMA_VERSION,
                builder_version: PAPER_PROFILE_V2_PROMPT_VERSION,
                model_id: &model_id,
                source_fact_hash: &source_hash,
            })? {
                report.changed += 1;
            }
        }
        Ok(report)
    }

    pub fn comprehend_author_profile_v2(
        &self,
        author: &str,
        options: S4AuthorComprehensionOptions,
        llm: Option<&OpenAiCompatibleClient>,
    ) -> Result<S4AuthorComprehensionReport> {
        let records = self
            .storage
            .paper_profiles_v2_for_author(author, options.limit)?;
        let model_id = llm
            .map(OpenAiCompatibleClient::model_name)
            .unwrap_or(DETERMINISTIC_MODEL_ID)
            .to_string();
        let mut report = S4AuthorComprehensionReport {
            paper_profiles_scanned: records.len(),
            dry_run: options.dry_run,
            model_id: model_id.clone(),
            ..S4AuthorComprehensionReport::default()
        };
        if records.is_empty() {
            report.missing_paper_profiles = 1;
            return Ok(report);
        }
        let source_hash = author_profile_v2_source_hash(&records)?;
        if !options.force
            && self.storage.author_profile_v2_is_current(
                author,
                AUTHOR_PROFILE_V2_SCHEMA_VERSION,
                AUTHOR_PROFILE_V2_PROMPT_VERSION,
                &model_id,
                &source_hash,
            )?
        {
            report.skipped_current = 1;
            return Ok(report);
        }
        let profiles = records
            .iter()
            .map(|record| record.profile_json.clone())
            .collect::<Vec<_>>();
        let seed = AuthorProfileV2Seed {
            author: author.to_string(),
            profiles,
        };
        let profile = match llm {
            Some(llm) => build_author_profile_v2_with_llm(seed, llm)?,
            None => build_author_profile_v2(seed)?,
        };
        report.research_themes = profile
            .get("research_themes")
            .and_then(Value::as_array)
            .map_or(0, Vec::len);
        report.built = 1;
        if options.dry_run {
            return Ok(report);
        }
        if self.storage.save_author_profile_v2(NewAuthorProfileV2 {
            author,
            profile_json: &profile,
            profile_schema_version: AUTHOR_PROFILE_V2_SCHEMA_VERSION,
            builder_version: AUTHOR_PROFILE_V2_PROMPT_VERSION,
            model_id: &model_id,
            source_profile_hash: &source_hash,
        })? {
            report.changed = 1;
        }
        Ok(report)
    }

    pub fn profile_diff(&self, author: &str) -> Result<ProfileDiffReport> {
        let v1_profiles = self.storage.paper_profiles(author, None)?;
        let v2_profiles = self.storage.paper_profiles_v2_for_author(author, None)?;
        let mut v1_by_key = BTreeMap::new();
        for profile in v1_profiles {
            if let Some(paper_key) = profile.get("paper_key").and_then(Value::as_str) {
                v1_by_key.insert(paper_key.to_string(), profile);
            }
        }
        let mut v2_by_key = BTreeMap::new();
        for record in v2_profiles {
            v2_by_key.insert(record.paper_key, record.profile_json);
        }
        let v1_keys = v1_by_key.keys().cloned().collect::<BTreeSet<_>>();
        let v2_keys = v2_by_key.keys().cloned().collect::<BTreeSet<_>>();
        let mut report = ProfileDiffReport {
            papers_with_v1: v1_keys.len(),
            papers_with_v2: v2_keys.len(),
            missing_v2: v1_keys.difference(&v2_keys).cloned().collect(),
            missing_v1: v2_keys.difference(&v1_keys).cloned().collect(),
            changed_summaries: Vec::new(),
        };
        for paper_key in v1_keys.intersection(&v2_keys) {
            let Some(v1) = v1_by_key.get(paper_key) else {
                continue;
            };
            let Some(v2) = v2_by_key.get(paper_key) else {
                continue;
            };
            let v1_summary = v1
                .get("one_sentence_summary")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .trim()
                .to_string();
            let v2_summary = v2
                .get("one_sentence_summary")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .trim()
                .to_string();
            if v1_summary != v2_summary {
                report.changed_summaries.push(ProfileSummaryDiff {
                    paper_key: paper_key.clone(),
                    title: v2
                        .get("title")
                        .or_else(|| v1.get("title"))
                        .and_then(Value::as_str)
                        .unwrap_or(paper_key)
                        .to_string(),
                    v1_summary,
                    v2_summary,
                });
            }
        }
        Ok(report)
    }

    pub fn profile_gate(&self, author: &str) -> Result<ProfileGateReport> {
        let diff = self.profile_diff(author)?;
        let v2_profiles = self.storage.paper_profiles_v2_for_author(author, None)?;
        let mut report = ProfileGateReport {
            papers_with_v1: diff.papers_with_v1,
            papers_with_v2: diff.papers_with_v2,
            missing_v2: diff.missing_v2,
            missing_v1: diff.missing_v1,
            ..ProfileGateReport::default()
        };

        for record in v2_profiles {
            match PaperProfileV2::from_value(record.profile_json).and_then(|profile| {
                profile.validate()?;
                Ok(profile)
            }) {
                Ok(profile) => {
                    report.factual_objects += profile.factual_objects.len();
                    for fact in &profile.factual_objects {
                        report.support_refs += fact.evidence.len();
                    }
                    for claim in profile
                        .main_contributions
                        .iter()
                        .chain(profile.limitations_or_open_questions.iter())
                    {
                        if !claim.support_refs.is_empty() {
                            report.claims_with_support_refs += 1;
                            report.support_refs += claim.support_refs.len();
                        }
                    }
                }
                Err(error) => {
                    report.invalid_v2_profiles.push(ProfileGateIssue {
                        paper_key: record.paper_key,
                        error: error.to_string(),
                    });
                }
            }
        }

        match self.storage.author_profile_v2(author)? {
            Some(record) => {
                report.author_profile_v2_present = true;
                match AuthorProfileV2::from_value(record.profile_json)
                    .and_then(|profile| profile.validate(author))
                {
                    Ok(()) => {
                        report.author_profile_v2_valid = true;
                    }
                    Err(error) => {
                        report.author_profile_v2_error = Some(error.to_string());
                    }
                }
            }
            None => {
                report.author_profile_v2_error = Some(
                    "missing AuthorProfileV2; run `ppc comprehend --v2 --author-profile`"
                        .to_string(),
                );
            }
        }

        if report.papers_with_v1 == 0 {
            report
                .blockers
                .push("no V1 paper profiles available for comparison".to_string());
        }
        if !report.missing_v2.is_empty() {
            report.blockers.push(format!(
                "{} V1 paper profiles are missing V2 profiles",
                report.missing_v2.len()
            ));
        }
        if !report.invalid_v2_profiles.is_empty() {
            report.blockers.push(format!(
                "{} V2 paper profiles failed schema/evidence validation",
                report.invalid_v2_profiles.len()
            ));
        }
        if !report.author_profile_v2_valid {
            report
                .blockers
                .push("AuthorProfileV2 is missing or invalid".to_string());
        }
        if report.support_refs == 0 {
            report
                .blockers
                .push("no V2 support refs found across paper profiles".to_string());
        }
        if !report.missing_v1.is_empty() {
            report.warnings.push(format!(
                "{} V2 profiles do not have matching V1 profiles",
                report.missing_v1.len()
            ));
        }
        if !diff.changed_summaries.is_empty() {
            report.warnings.push(format!(
                "{} paper summaries differ and need review",
                diff.changed_summaries.len()
            ));
        }
        report.ready = report.blockers.is_empty();
        Ok(report)
    }
}

fn seed_from_facts(paper_key: &str, facts: Vec<crate::storage::ChunkFact>) -> PaperProfileV2Seed {
    let first_json = facts
        .first()
        .and_then(|fact| serde_json::from_str::<Value>(&fact.fact_json).ok());
    PaperProfileV2Seed {
        paper_key: paper_key.to_string(),
        title: first_json
            .as_ref()
            .and_then(|value| value.get("title"))
            .and_then(Value::as_str)
            .unwrap_or(paper_key)
            .to_string(),
        doi: first_json
            .as_ref()
            .and_then(|value| value.get("doi"))
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string(),
        year: first_json
            .as_ref()
            .and_then(|value| value.get("year"))
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string(),
        facts,
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use serde_json::json;
    use tempfile::tempdir;

    use super::{ComprehensionService, S3ComprehensionOptions, S4AuthorComprehensionOptions};
    use crate::papers::models::{Paper, Section};
    use crate::retrieval::chunker::chunk_paper;
    use crate::services::classification::{ClassificationOptions, ClassificationService};
    use crate::services::extraction::{ExtractionService, V2ExtractionOptions};
    use crate::storage::Storage;

    #[test]
    fn dry_run_builds_profiles_without_persisting() {
        let dir = tempdir().unwrap();
        let mut storage = Storage::open(&dir.path().join("test.sqlite")).unwrap();
        seed_extracted_facts(&mut storage, dir.path());

        let report = ComprehensionService::new(&storage)
            .comprehend_author_v2(
                "Alice",
                S3ComprehensionOptions {
                    dry_run: true,
                    ..S3ComprehensionOptions::default()
                },
                None,
            )
            .unwrap();

        assert_eq!(report.papers_scanned, 1);
        assert_eq!(report.built, 1);
        assert_eq!(report.changed, 0);
        assert!(storage.paper_profile_v2("Alice/paper-a").unwrap().is_none());
    }

    #[test]
    fn persists_and_skips_current_profiles() {
        let dir = tempdir().unwrap();
        let mut storage = Storage::open(&dir.path().join("test.sqlite")).unwrap();
        seed_extracted_facts(&mut storage, dir.path());
        let service = ComprehensionService::new(&storage);

        let first = service
            .comprehend_author_v2("Alice", S3ComprehensionOptions::default(), None)
            .unwrap();
        let second = service
            .comprehend_author_v2("Alice", S3ComprehensionOptions::default(), None)
            .unwrap();
        let forced = service
            .comprehend_author_v2(
                "Alice",
                S3ComprehensionOptions {
                    force: true,
                    ..S3ComprehensionOptions::default()
                },
                None,
            )
            .unwrap();

        assert_eq!(first.changed, 1);
        assert_eq!(second.built, 0);
        assert_eq!(second.skipped_current, 1);
        assert_eq!(forced.built, 1);
        assert_eq!(forced.changed, 0);
        let profile = storage.paper_profile_v2("Alice/paper-a").unwrap().unwrap();
        assert_eq!(profile.profile_json["paper_key"], "Alice/paper-a");
        assert_eq!(
            profile.profile_json["factual_objects"][0]["chunk_fact_id"],
            2
        );
    }

    #[test]
    fn builds_author_profile_v2_from_paper_profiles() {
        let dir = tempdir().unwrap();
        let mut storage = Storage::open(&dir.path().join("test.sqlite")).unwrap();
        seed_extracted_facts(&mut storage, dir.path());
        let service = ComprehensionService::new(&storage);
        service
            .comprehend_author_v2("Alice", S3ComprehensionOptions::default(), None)
            .unwrap();

        let dry_run = service
            .comprehend_author_profile_v2(
                "Alice",
                S4AuthorComprehensionOptions {
                    dry_run: true,
                    ..S4AuthorComprehensionOptions::default()
                },
                None,
            )
            .unwrap();
        let first = service
            .comprehend_author_profile_v2("Alice", S4AuthorComprehensionOptions::default(), None)
            .unwrap();
        let second = service
            .comprehend_author_profile_v2("Alice", S4AuthorComprehensionOptions::default(), None)
            .unwrap();

        assert_eq!(dry_run.built, 1);
        assert_eq!(dry_run.changed, 0);
        assert_eq!(first.changed, 1);
        assert_eq!(second.built, 0);
        assert_eq!(second.skipped_current, 1);
        let profile = storage.author_profile_v2("Alice").unwrap().unwrap();
        assert_eq!(profile.profile_json["author"], "Alice");
        assert!(profile.profile_json["research_themes"].as_array().is_some());
    }

    #[test]
    fn profile_gate_blocks_until_v2_profiles_and_author_profile_are_ready() {
        let dir = tempdir().unwrap();
        let mut storage = Storage::open(&dir.path().join("test.sqlite")).unwrap();
        seed_extracted_facts(&mut storage, dir.path());
        seed_v1_profile(&storage);
        let service = ComprehensionService::new(&storage);

        let initial = service.profile_gate("Alice").unwrap();
        assert!(!initial.ready);
        assert_eq!(initial.papers_with_v1, 1);
        assert_eq!(initial.papers_with_v2, 0);
        assert_eq!(initial.missing_v2, vec!["Alice/paper-a"]);

        service
            .comprehend_author_v2("Alice", S3ComprehensionOptions::default(), None)
            .unwrap();
        let without_author = service.profile_gate("Alice").unwrap();
        assert!(!without_author.ready);
        assert_eq!(without_author.papers_with_v2, 1);
        assert_eq!(without_author.factual_objects, 2);
        assert!(without_author.support_refs > 0);
        assert!(
            without_author
                .blockers
                .contains(&"AuthorProfileV2 is missing or invalid".to_string())
        );

        service
            .comprehend_author_profile_v2("Alice", S4AuthorComprehensionOptions::default(), None)
            .unwrap();
        let ready = service.profile_gate("Alice").unwrap();
        assert!(ready.ready);
        assert!(ready.author_profile_v2_present);
        assert!(ready.author_profile_v2_valid);
        assert!(ready.blockers.is_empty());
    }

    fn seed_v1_profile(storage: &Storage) {
        storage
            .save_paper_profile(
                "Alice/paper-a",
                "source-a",
                &json!({
                    "paper_key": "Alice/paper-a",
                    "title": "A Paper",
                    "doi": "10.1/test",
                    "year": "2024",
                    "one_sentence_summary": "A V1 summary.",
                    "topic_keywords": ["MOF", "catalysis"],
                    "reliable_answer_scope": ["MOF catalysis"],
                    "evidence_notes": ["grounded in extracted facts"]
                }),
            )
            .unwrap();
    }

    fn seed_extracted_facts(storage: &mut Storage, root: &std::path::Path) {
        let paper = Paper {
            author: "Alice".to_string(),
            paper_id: "paper-a".to_string(),
            paper_dir: root.to_path_buf(),
            article_path: root.join("article.md"),
            fetch_result_path: None,
            source_hash: "source-a".to_string(),
            metadata: BTreeMap::from([
                ("title".to_string(), "A Paper".to_string()),
                ("doi".to_string(), "10.1/test".to_string()),
                ("year".to_string(), "2024".to_string()),
            ]),
            fetch_result: json!({}),
            raw_body: String::new(),
            clean_text: String::new(),
            sections: vec![
                Section {
                    title: "Abstract".to_string(),
                    level: 1,
                    content: "This paper studies MOF catalysis in a controlled reactor and explains the target reaction scope for later performance evaluation.".to_string(),
                },
                Section {
                    title: "Results".to_string(),
                    level: 2,
                    content: "The best condition reports 82% conversion under mild conditions and remains the strongest measured outcome in the benchmark.".to_string(),
                },
            ],
        };
        let chunks = chunk_paper(&paper, 3200, 350);
        storage.upsert_paper(&paper, &chunks).unwrap();
        ClassificationService::new(storage)
            .classify_author("Alice", ClassificationOptions::default())
            .unwrap();
        ExtractionService::new(storage)
            .extract_author_v2("Alice", V2ExtractionOptions::default())
            .unwrap();
    }
}
