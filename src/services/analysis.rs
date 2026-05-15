use anyhow::{Result, anyhow};

use crate::schemas::paper_profile::PAPER_PROFILE_SCHEMA_VERSION;
use crate::storage::{AnalysisCandidate, Storage};
use crate::understanding::prompts::PAPER_PROFILE_PROMPT_VERSION;

#[derive(Debug, Clone)]
pub struct AnalysisQueueOptions<'a> {
    pub failed_only: bool,
    pub stale_only: bool,
    pub force: bool,
    pub limit: Option<usize>,
    pub max_attempts: i64,
    pub model_id: &'a str,
    pub chunker_version: &'a str,
}

#[derive(Debug, Clone)]
pub struct AnalysisQueuePlan {
    pub candidates: Vec<AnalysisCandidate>,
    pub queued: usize,
}

pub struct AnalysisService<'a> {
    storage: &'a Storage,
}

impl<'a> AnalysisService<'a> {
    pub fn new(storage: &'a Storage) -> Self {
        Self { storage }
    }

    pub fn enqueue_author(
        &self,
        author: &str,
        options: AnalysisQueueOptions<'_>,
    ) -> Result<AnalysisQueuePlan> {
        if options.force && options.stale_only {
            return Err(anyhow!("--force and --stale-only cannot be used together"));
        }
        let mut candidates = if options.failed_only {
            self.storage.failed_analysis_candidates(author)?
        } else {
            self.storage.papers_needing_analysis(
                author,
                options.force && !options.stale_only,
                PAPER_PROFILE_SCHEMA_VERSION,
                PAPER_PROFILE_PROMPT_VERSION,
                options.model_id,
                options.chunker_version,
            )?
        };
        if let Some(limit) = options.limit {
            candidates.truncate(limit);
        }
        let queued = self.storage.enqueue_analysis_jobs(
            &candidates,
            "analyze",
            PAPER_PROFILE_SCHEMA_VERSION,
            PAPER_PROFILE_PROMPT_VERSION,
            options.model_id,
            options.max_attempts.max(1),
        )?;
        Ok(AnalysisQueuePlan { candidates, queued })
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use serde_json::json;
    use tempfile::tempdir;

    use super::{AnalysisQueueOptions, AnalysisService};
    use crate::papers::models::Paper;
    use crate::retrieval::chunker::chunk_paper;
    use crate::schemas::paper_profile::PAPER_PROFILE_SCHEMA_VERSION;
    use crate::storage::{AnalysisCandidate, AnalysisJobMetadata, PaperProfileMetadata, Storage};
    use crate::understanding::prompts::PAPER_PROFILE_PROMPT_VERSION;

    #[test]
    fn enqueue_author_creates_jobs_for_stale_papers() {
        let dir = tempdir().unwrap();
        let mut storage = Storage::open(&dir.path().join("test.sqlite")).unwrap();
        let paper = Paper {
            author: "Alice".to_string(),
            paper_id: "paper-a".to_string(),
            paper_dir: dir.path().to_path_buf(),
            article_path: dir.path().join("article.md"),
            fetch_result_path: None,
            source_hash: "hash".to_string(),
            metadata: BTreeMap::from([
                ("title".to_string(), "A Paper".to_string()),
                ("doi".to_string(), "10.1/test".to_string()),
                ("year".to_string(), "2024".to_string()),
            ]),
            fetch_result: json!({}),
            raw_body: String::new(),
            clean_text: "Catalyst conversion improved.".to_string(),
            sections: Vec::new(),
        };
        let chunks = chunk_paper(&paper, 3200, 350);
        storage.upsert_paper(&paper, &chunks).unwrap();

        let plan = AnalysisService::new(&storage)
            .enqueue_author(
                "Alice",
                AnalysisQueueOptions {
                    failed_only: false,
                    stale_only: false,
                    force: false,
                    limit: None,
                    max_attempts: 2,
                    model_id: "model-a",
                    chunker_version: "chunker-v1",
                },
            )
            .unwrap();

        assert_eq!(plan.candidates.len(), 1);
        assert_eq!(plan.queued, 1);
        assert_eq!(
            storage
                .analysis_jobs(Some("Alice"), Some("queued"), 10)
                .unwrap()
                .len(),
            1
        );
    }

    #[test]
    fn failed_only_queues_only_failed_analysis_candidates() {
        let dir = tempdir().unwrap();
        let mut storage = Storage::open(&dir.path().join("test.sqlite")).unwrap();
        seed_analysis_papers(&mut storage, dir.path());

        let plan = AnalysisService::new(&storage)
            .enqueue_author("Alice", queue_options(true, false, false))
            .unwrap();

        assert_eq!(paper_ids(&plan), vec!["paper-c"]);
        assert_eq!(plan.queued, 1);
    }

    #[test]
    fn stale_only_excludes_current_papers() {
        let dir = tempdir().unwrap();
        let mut storage = Storage::open(&dir.path().join("test.sqlite")).unwrap();
        seed_analysis_papers(&mut storage, dir.path());

        let plan = AnalysisService::new(&storage)
            .enqueue_author("Alice", queue_options(false, true, false))
            .unwrap();

        assert_eq!(paper_ids(&plan), vec!["paper-b", "paper-c"]);
        assert_eq!(plan.queued, 2);
    }

    #[test]
    fn force_queues_current_and_stale_papers() {
        let dir = tempdir().unwrap();
        let mut storage = Storage::open(&dir.path().join("test.sqlite")).unwrap();
        seed_analysis_papers(&mut storage, dir.path());

        let plan = AnalysisService::new(&storage)
            .enqueue_author("Alice", queue_options(false, false, true))
            .unwrap();

        assert_eq!(paper_ids(&plan), vec!["paper-a", "paper-b", "paper-c"]);
        assert_eq!(plan.queued, 3);
    }

    #[test]
    fn force_and_stale_only_are_rejected() {
        let dir = tempdir().unwrap();
        let storage = Storage::open(&dir.path().join("test.sqlite")).unwrap();
        let err = AnalysisService::new(&storage)
            .enqueue_author("Alice", queue_options(false, true, true))
            .unwrap_err()
            .to_string();

        assert!(err.contains("--force and --stale-only"));
    }

    fn queue_options(
        failed_only: bool,
        stale_only: bool,
        force: bool,
    ) -> AnalysisQueueOptions<'static> {
        AnalysisQueueOptions {
            failed_only,
            stale_only,
            force,
            limit: None,
            max_attempts: 2,
            model_id: "model-a",
            chunker_version: "chunker-v1",
        }
    }

    fn seed_analysis_papers(storage: &mut Storage, root: &std::path::Path) {
        let current = test_paper(root, "paper-a", "2024", "hash-a");
        let stale = test_paper(root, "paper-b", "2023", "hash-b");
        let failed = test_paper(root, "paper-c", "2022", "hash-c");
        for paper in [&current, &stale, &failed] {
            let chunks = chunk_paper(paper, 3200, 350);
            storage.upsert_paper(paper, &chunks).unwrap();
        }
        storage
            .save_paper_profile_with_metadata(
                &current.key(),
                &json!({
                    "paper_key": current.key(),
                    "title": current.title(),
                    "doi": current.doi(),
                    "year": current.year(),
                    "one_sentence_summary": "summary",
                    "methods": [{"method": "method", "evidence_chunks": [0]}]
                }),
                PaperProfileMetadata {
                    source_hash: &current.source_hash,
                    schema_version: PAPER_PROFILE_SCHEMA_VERSION,
                    prompt_version: PAPER_PROFILE_PROMPT_VERSION,
                    model_id: "model-a",
                    chunker_version: "chunker-v1",
                },
            )
            .unwrap();
        storage
            .record_analysis_job_with_metadata(
                &analysis_candidate(&failed),
                AnalysisJobMetadata {
                    job_type: "analyze",
                    status: "failed",
                    error_code: Some("schema_error"),
                    error: Some("bad json"),
                    profile_schema_version: PAPER_PROFILE_SCHEMA_VERSION,
                    prompt_version: PAPER_PROFILE_PROMPT_VERSION,
                    model_id: "model-a",
                },
            )
            .unwrap();
    }

    fn paper_ids(plan: &super::AnalysisQueuePlan) -> Vec<&str> {
        plan.candidates
            .iter()
            .map(|candidate| candidate.paper_id.as_str())
            .collect()
    }

    fn analysis_candidate(paper: &Paper) -> AnalysisCandidate {
        AnalysisCandidate {
            paper_key: paper.key(),
            author: paper.author.clone(),
            paper_id: paper.paper_id.clone(),
            title: paper.title().to_string(),
            doi: paper.doi().to_string(),
            year: paper.year().to_string(),
            source_hash: paper.source_hash.clone(),
            article_path: paper.article_path.display().to_string(),
        }
    }

    fn test_paper(root: &std::path::Path, paper_id: &str, year: &str, source_hash: &str) -> Paper {
        Paper {
            author: "Alice".to_string(),
            paper_id: paper_id.to_string(),
            paper_dir: root.join("Alice").join(paper_id),
            article_path: root.join("Alice").join(paper_id).join("article.md"),
            fetch_result_path: None,
            source_hash: source_hash.to_string(),
            metadata: BTreeMap::from([
                ("title".to_string(), format!("A Paper {paper_id}")),
                ("doi".to_string(), format!("10.1/{paper_id}")),
                ("year".to_string(), year.to_string()),
            ]),
            fetch_result: json!({}),
            raw_body: String::new(),
            clean_text: "Catalyst conversion improved.".to_string(),
            sections: Vec::new(),
        }
    }
}
