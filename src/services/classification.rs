use std::collections::BTreeMap;

use anyhow::Result;

use crate::storage::{NewChunkClassification, Storage};
use crate::understanding::chunk_classifier::{CHUNK_CLASSIFIER_VERSION, classify_chunk};

#[derive(Debug, Clone, Copy, Default)]
pub struct ClassificationOptions {
    pub limit: Option<usize>,
    pub force: bool,
    pub dry_run: bool,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ClassificationReport {
    pub chunks_scanned: usize,
    pub classified: usize,
    pub changed: usize,
    pub skipped_current: usize,
    pub by_kind: BTreeMap<String, usize>,
    pub skip_reasons: BTreeMap<String, usize>,
    pub dry_run: bool,
}

pub struct ClassificationService<'a> {
    storage: &'a Storage,
}

impl<'a> ClassificationService<'a> {
    pub fn new(storage: &'a Storage) -> Self {
        Self { storage }
    }

    pub fn classify_author(
        &self,
        author: &str,
        options: ClassificationOptions,
    ) -> Result<ClassificationReport> {
        let chunks = self.storage.all_chunks_for_author(author, options.limit)?;
        let mut report = ClassificationReport {
            chunks_scanned: chunks.len(),
            dry_run: options.dry_run,
            ..ClassificationReport::default()
        };
        for chunk in chunks {
            let decision = classify_chunk(&chunk);
            *report
                .by_kind
                .entry(decision.chunk_kind.to_string())
                .or_default() += 1;
            if let Some(reason) = decision.skip_reason {
                *report.skip_reasons.entry(reason.to_string()).or_default() += 1;
            }

            if !options.force
                && self
                    .storage
                    .has_current_chunk_classification(&chunk, CHUNK_CLASSIFIER_VERSION)?
            {
                report.skipped_current += 1;
                continue;
            }

            report.classified += 1;
            if options.dry_run {
                continue;
            }
            if self
                .storage
                .save_chunk_classification(NewChunkClassification {
                    chunk_id: chunk.id,
                    paper_key: &chunk.paper_key,
                    chunk_kind: decision.chunk_kind,
                    usefulness_score: decision.usefulness_score,
                    skip_reason: decision.skip_reason,
                    classifier_version: CHUNK_CLASSIFIER_VERSION,
                    source_hash: &chunk.source_hash,
                    chunk_hash: &chunk.chunk_hash,
                })?
            {
                report.changed += 1;
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

    use super::{ClassificationOptions, ClassificationService};
    use crate::papers::models::{Paper, Section};
    use crate::retrieval::chunker::chunk_paper;
    use crate::storage::Storage;

    #[test]
    fn dry_run_does_not_persist_classifications() {
        let dir = tempdir().unwrap();
        let mut storage = Storage::open(&dir.path().join("test.sqlite")).unwrap();
        seed_paper(&mut storage, dir.path());

        let report = ClassificationService::new(&storage)
            .classify_author(
                "Alice",
                ClassificationOptions {
                    dry_run: true,
                    ..ClassificationOptions::default()
                },
            )
            .unwrap();

        assert_eq!(report.chunks_scanned, 2);
        assert_eq!(report.classified, 2);
        assert_eq!(report.changed, 0);
        let chunk = storage.all_chunks_for_author("Alice", Some(1)).unwrap()[0].clone();
        assert!(storage.chunk_classification(chunk.id).unwrap().is_none());
    }

    #[test]
    fn skips_current_classifications_unless_forced() {
        let dir = tempdir().unwrap();
        let mut storage = Storage::open(&dir.path().join("test.sqlite")).unwrap();
        seed_paper(&mut storage, dir.path());
        let service = ClassificationService::new(&storage);

        let first = service
            .classify_author("Alice", ClassificationOptions::default())
            .unwrap();
        let second = service
            .classify_author("Alice", ClassificationOptions::default())
            .unwrap();
        let forced = service
            .classify_author(
                "Alice",
                ClassificationOptions {
                    force: true,
                    ..ClassificationOptions::default()
                },
            )
            .unwrap();

        assert_eq!(first.changed, 2);
        assert_eq!(second.classified, 0);
        assert_eq!(second.skipped_current, 2);
        assert_eq!(forced.classified, 2);
        assert_eq!(forced.changed, 0);
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
                    content: "The method uses in situ infrared tracking.".to_string(),
                },
                Section {
                    title: "References".to_string(),
                    level: 2,
                    content: "Smith J. Journal of Catalysis. 2020.".to_string(),
                },
            ],
        };
        let chunks = chunk_paper(&paper, 3200, 350);
        storage.upsert_paper(&paper, &chunks).unwrap();
    }
}
