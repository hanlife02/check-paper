use std::collections::{BTreeMap, HashSet};

use anyhow::Result;

use crate::storage::{NewChunkFact, Storage};
use crate::understanding::chunk_classifier::CHUNK_CLASSIFIER_VERSION;
use crate::understanding::chunk_fact_extractor::{
    CHUNK_FACT_EXTRACTOR, CHUNK_FACT_EXTRACTOR_VERSION, extract_chunk_fact,
};

#[derive(Debug, Clone, Copy, Default)]
pub struct V2ExtractionOptions {
    pub limit: Option<usize>,
    pub force: bool,
    pub dry_run: bool,
    pub failed_only: bool,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct V2ExtractionReport {
    pub chunks_scanned: usize,
    pub extracted: usize,
    pub changed: usize,
    pub skipped_current: usize,
    pub skipped_by_classification: usize,
    pub missing_current_classification: usize,
    pub failed: usize,
    pub by_fact_type: BTreeMap<String, usize>,
    pub dry_run: bool,
    pub failed_only: bool,
}

pub struct ExtractionService<'a> {
    storage: &'a Storage,
}

impl<'a> ExtractionService<'a> {
    pub fn new(storage: &'a Storage) -> Self {
        Self { storage }
    }

    pub fn extract_author_v2(
        &self,
        author: &str,
        options: V2ExtractionOptions,
    ) -> Result<V2ExtractionReport> {
        let chunks = if options.failed_only {
            let failed_ids = self.storage.failed_chunk_fact_chunk_ids(
                author,
                CHUNK_FACT_EXTRACTOR,
                CHUNK_FACT_EXTRACTOR_VERSION,
                options.limit,
            )?;
            let failed_ids = failed_ids.into_iter().collect::<HashSet<_>>();
            self.storage
                .all_chunks_for_author(author, None)?
                .into_iter()
                .filter(|chunk| failed_ids.contains(&chunk.id))
                .collect()
        } else {
            self.storage.all_chunks_for_author(author, options.limit)?
        };
        let mut report = V2ExtractionReport {
            chunks_scanned: chunks.len(),
            dry_run: options.dry_run,
            failed_only: options.failed_only,
            ..V2ExtractionReport::default()
        };

        for chunk in chunks {
            let Some(classification) = self.storage.chunk_classification(chunk.id)? else {
                report.missing_current_classification += 1;
                continue;
            };
            if classification.classifier_version != CHUNK_CLASSIFIER_VERSION
                || classification.source_hash != chunk.source_hash
                || classification.chunk_hash != chunk.chunk_hash
            {
                report.missing_current_classification += 1;
                continue;
            }

            let Some(fact) = extract_chunk_fact(&chunk, &classification) else {
                report.skipped_by_classification += 1;
                self.storage.clear_chunk_fact_failure(
                    &chunk,
                    CHUNK_FACT_EXTRACTOR,
                    CHUNK_FACT_EXTRACTOR_VERSION,
                )?;
                continue;
            };
            *report
                .by_fact_type
                .entry(fact.fact_type.to_string())
                .or_default() += 1;

            if !options.force
                && self.storage.has_current_chunk_fact(
                    &chunk,
                    CHUNK_FACT_EXTRACTOR,
                    CHUNK_FACT_EXTRACTOR_VERSION,
                )?
            {
                report.skipped_current += 1;
                continue;
            }

            report.extracted += 1;
            if options.dry_run {
                continue;
            }

            let fact_json = serde_json::to_string(&fact.fact_json)?;
            match self.storage.save_chunk_fact(NewChunkFact {
                claim_uid: &fact.claim_uid,
                paper_key: &chunk.paper_key,
                chunk_id: chunk.id,
                fact_type: fact.fact_type,
                fact_json: &fact_json,
                confidence: Some(fact.confidence),
                extractor: CHUNK_FACT_EXTRACTOR,
                extractor_version: CHUNK_FACT_EXTRACTOR_VERSION,
                source_hash: &chunk.source_hash,
                chunk_hash: &chunk.chunk_hash,
            }) {
                Ok(changed) => {
                    self.storage.clear_chunk_fact_failure(
                        &chunk,
                        CHUNK_FACT_EXTRACTOR,
                        CHUNK_FACT_EXTRACTOR_VERSION,
                    )?;
                    if changed {
                        report.changed += 1;
                    }
                }
                Err(error) => {
                    report.failed += 1;
                    self.storage.record_chunk_fact_failure(
                        &chunk,
                        CHUNK_FACT_EXTRACTOR,
                        CHUNK_FACT_EXTRACTOR_VERSION,
                        &error.to_string(),
                    )?;
                }
            }
        }
        Ok(report)
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use serde_json::json;
    use tempfile::tempdir;

    use super::{ExtractionService, V2ExtractionOptions};
    use crate::papers::models::{Paper, Section};
    use crate::retrieval::chunker::chunk_paper;
    use crate::services::classification::{ClassificationOptions, ClassificationService};
    use crate::storage::Storage;

    #[test]
    fn extraction_requires_current_classification() {
        let dir = tempdir().unwrap();
        let mut storage = Storage::open(&dir.path().join("test.sqlite")).unwrap();
        seed_paper(&mut storage, dir.path());

        let report = ExtractionService::new(&storage)
            .extract_author_v2("Alice", V2ExtractionOptions::default())
            .unwrap();

        assert_eq!(report.chunks_scanned, 2);
        assert_eq!(report.extracted, 0);
        assert_eq!(report.missing_current_classification, 2);
    }

    #[test]
    fn dry_run_does_not_persist_chunk_facts() {
        let dir = tempdir().unwrap();
        let mut storage = Storage::open(&dir.path().join("test.sqlite")).unwrap();
        seed_paper(&mut storage, dir.path());
        ClassificationService::new(&storage)
            .classify_author("Alice", ClassificationOptions::default())
            .unwrap();

        let report = ExtractionService::new(&storage)
            .extract_author_v2(
                "Alice",
                V2ExtractionOptions {
                    dry_run: true,
                    ..V2ExtractionOptions::default()
                },
            )
            .unwrap();

        assert_eq!(report.extracted, 2);
        assert_eq!(report.changed, 0);
        assert_eq!(storage.chunk_fact_count_for_author("Alice").unwrap(), 0);
    }

    #[test]
    fn skips_current_chunk_facts_unless_forced() {
        let dir = tempdir().unwrap();
        let mut storage = Storage::open(&dir.path().join("test.sqlite")).unwrap();
        seed_paper(&mut storage, dir.path());
        ClassificationService::new(&storage)
            .classify_author("Alice", ClassificationOptions::default())
            .unwrap();
        let service = ExtractionService::new(&storage);

        let first = service
            .extract_author_v2("Alice", V2ExtractionOptions::default())
            .unwrap();
        let second = service
            .extract_author_v2("Alice", V2ExtractionOptions::default())
            .unwrap();
        let forced = service
            .extract_author_v2(
                "Alice",
                V2ExtractionOptions {
                    force: true,
                    ..V2ExtractionOptions::default()
                },
            )
            .unwrap();

        assert_eq!(first.changed, 2);
        assert_eq!(second.extracted, 0);
        assert_eq!(second.skipped_current, 2);
        assert_eq!(forced.extracted, 2);
        assert_eq!(forced.changed, 0);
        assert_eq!(storage.chunk_fact_count_for_author("Alice").unwrap(), 2);
    }

    #[test]
    fn failed_only_processes_recorded_failed_chunks() {
        let dir = tempdir().unwrap();
        let mut storage = Storage::open(&dir.path().join("test.sqlite")).unwrap();
        seed_paper(&mut storage, dir.path());
        ClassificationService::new(&storage)
            .classify_author("Alice", ClassificationOptions::default())
            .unwrap();
        let chunk = storage.all_chunks_for_author("Alice", Some(1)).unwrap()[0].clone();
        storage
            .record_chunk_fact_failure(
                &chunk,
                crate::understanding::chunk_fact_extractor::CHUNK_FACT_EXTRACTOR,
                crate::understanding::chunk_fact_extractor::CHUNK_FACT_EXTRACTOR_VERSION,
                "previous failure",
            )
            .unwrap();

        let report = ExtractionService::new(&storage)
            .extract_author_v2(
                "Alice",
                V2ExtractionOptions {
                    failed_only: true,
                    ..V2ExtractionOptions::default()
                },
            )
            .unwrap();

        assert_eq!(report.chunks_scanned, 1);
        assert_eq!(report.extracted, 1);
        assert_eq!(storage.chunk_fact_count_for_author("Alice").unwrap(), 1);
        assert!(
            storage
                .failed_chunk_fact_chunk_ids(
                    "Alice",
                    crate::understanding::chunk_fact_extractor::CHUNK_FACT_EXTRACTOR,
                    crate::understanding::chunk_fact_extractor::CHUNK_FACT_EXTRACTOR_VERSION,
                    None,
                )
                .unwrap()
                .is_empty()
        );
    }

    fn seed_paper(storage: &mut Storage, root: &std::path::Path) {
        let paper = Paper {
            author: "Alice".to_string(),
            paper_id: "paper-a".to_string(),
            paper_dir: root.to_path_buf(),
            article_path: root.join("article.md"),
            fetch_result_path: None,
            source_hash: "source-a".to_string(),
            metadata: BTreeMap::from([
                ("title".to_string(), "A Paper".to_string()),
                ("year".to_string(), "2024".to_string()),
            ]),
            fetch_result: json!({}),
            raw_body: String::new(),
            clean_text: String::new(),
            sections: vec![
                Section {
                    title: "Methods".to_string(),
                    level: 2,
                    content:
                        "The method uses in situ infrared tracking with temperature comparison."
                            .to_string(),
                },
                Section {
                    title: "Results".to_string(),
                    level: 2,
                    content: "The best condition reports 82% conversion under mild conditions."
                        .to_string(),
                },
            ],
        };
        let chunks = chunk_paper(&paper, 3200, 350);
        storage.upsert_paper(&paper, &chunks).unwrap();
    }
}
