use anyhow::Result;
use rusqlite::{OptionalExtension, params};

use super::{ChunkClassification, NewChunkClassification, SourceChunk, Storage};

impl Storage {
    pub fn save_chunk_classification(
        &self,
        classification: NewChunkClassification<'_>,
    ) -> Result<bool> {
        let changed = self.chunk_classification_changed(classification)?;
        self.conn.execute(
            r#"
            INSERT INTO chunk_classifications (
                chunk_id, paper_key, chunk_kind, usefulness_score, skip_reason,
                classifier_version, source_hash, chunk_hash, classified_at
            )
            VALUES (?, ?, ?, ?, ?, ?, ?, ?, CURRENT_TIMESTAMP)
            ON CONFLICT(chunk_id) DO UPDATE SET
                paper_key = excluded.paper_key,
                chunk_kind = excluded.chunk_kind,
                usefulness_score = excluded.usefulness_score,
                skip_reason = excluded.skip_reason,
                classifier_version = excluded.classifier_version,
                source_hash = excluded.source_hash,
                chunk_hash = excluded.chunk_hash,
                classified_at = CURRENT_TIMESTAMP
            "#,
            params![
                classification.chunk_id,
                classification.paper_key,
                classification.chunk_kind,
                classification.usefulness_score,
                classification.skip_reason,
                classification.classifier_version,
                classification.source_hash,
                classification.chunk_hash,
            ],
        )?;
        Ok(changed)
    }

    pub fn chunk_classification(&self, chunk_id: i64) -> Result<Option<ChunkClassification>> {
        self.conn
            .query_row(
                r#"
                SELECT chunk_id, paper_key, chunk_kind, usefulness_score, skip_reason,
                       classifier_version, source_hash, chunk_hash, classified_at
                FROM chunk_classifications
                WHERE chunk_id = ?
                "#,
                params![chunk_id],
                chunk_classification_from_row,
            )
            .optional()
            .map_err(Into::into)
    }

    pub fn has_current_chunk_classification(
        &self,
        chunk: &SourceChunk,
        classifier_version: &str,
    ) -> Result<bool> {
        let current: Option<i64> = self
            .conn
            .query_row(
                r#"
                SELECT 1
                FROM chunk_classifications
                WHERE chunk_id = ?
                  AND classifier_version = ?
                  AND source_hash = ?
                  AND chunk_hash = ?
                "#,
                params![
                    chunk.id,
                    classifier_version,
                    chunk.source_hash,
                    chunk.chunk_hash
                ],
                |row| row.get(0),
            )
            .optional()?;
        Ok(current.is_some())
    }

    fn chunk_classification_changed(
        &self,
        classification: NewChunkClassification<'_>,
    ) -> Result<bool> {
        let existing: Option<(String, f64, Option<String>, String, String, String)> = self
            .conn
            .query_row(
                r#"
                SELECT chunk_kind, usefulness_score, skip_reason, classifier_version,
                       source_hash, chunk_hash
                FROM chunk_classifications
                WHERE chunk_id = ?
                "#,
                params![classification.chunk_id],
                |row| {
                    Ok((
                        row.get(0)?,
                        row.get(1)?,
                        row.get(2)?,
                        row.get(3)?,
                        row.get(4)?,
                        row.get(5)?,
                    ))
                },
            )
            .optional()?;
        let Some(existing) = existing else {
            return Ok(true);
        };
        Ok(existing.0 != classification.chunk_kind
            || (existing.1 - classification.usefulness_score).abs() > f64::EPSILON
            || existing.2.as_deref() != classification.skip_reason
            || existing.3 != classification.classifier_version
            || existing.4 != classification.source_hash
            || existing.5 != classification.chunk_hash)
    }
}

fn chunk_classification_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<ChunkClassification> {
    Ok(ChunkClassification {
        chunk_id: row.get(0)?,
        paper_key: row.get(1)?,
        chunk_kind: row.get(2)?,
        usefulness_score: row.get(3)?,
        skip_reason: row.get(4)?,
        classifier_version: row.get(5)?,
        source_hash: row.get(6)?,
        chunk_hash: row.get(7)?,
        classified_at: row.get(8)?,
    })
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use serde_json::json;
    use tempfile::tempdir;

    use super::*;
    use crate::papers::models::Paper;
    use crate::retrieval::chunker::chunk_paper;

    #[test]
    fn saves_and_checks_current_chunk_classification() {
        let dir = tempdir().unwrap();
        let mut storage = Storage::open(&dir.path().join("test.sqlite")).unwrap();
        let paper = Paper {
            author: "Alice".to_string(),
            paper_id: "paper-a".to_string(),
            paper_dir: dir.path().to_path_buf(),
            article_path: dir.path().join("article.md"),
            fetch_result_path: None,
            source_hash: "source-a".to_string(),
            metadata: BTreeMap::from([
                ("title".to_string(), "A Paper".to_string()),
                ("year".to_string(), "2024".to_string()),
            ]),
            fetch_result: json!({}),
            raw_body: String::new(),
            clean_text: "Catalyst conversion improved.".to_string(),
            sections: Vec::new(),
        };
        let chunks = chunk_paper(&paper, 3200, 350);
        storage.upsert_paper(&paper, &chunks).unwrap();
        let chunk = storage.all_chunks_for_author("Alice", Some(1)).unwrap()[0].clone();

        let changed = storage
            .save_chunk_classification(NewChunkClassification {
                chunk_id: chunk.id,
                paper_key: &chunk.paper_key,
                chunk_kind: "results",
                usefulness_score: 0.9,
                skip_reason: None,
                classifier_version: "classifier-v1",
                source_hash: &chunk.source_hash,
                chunk_hash: &chunk.chunk_hash,
            })
            .unwrap();

        assert!(changed);
        assert!(
            storage
                .has_current_chunk_classification(&chunk, "classifier-v1")
                .unwrap()
        );
        assert!(
            !storage
                .has_current_chunk_classification(&chunk, "classifier-v2")
                .unwrap()
        );
        let saved = storage.chunk_classification(chunk.id).unwrap().unwrap();
        assert_eq!(saved.chunk_kind, "results");
        assert_eq!(saved.skip_reason, None);
    }

    #[test]
    fn unchanged_chunk_classification_reports_not_changed() {
        let dir = tempdir().unwrap();
        let mut storage = Storage::open(&dir.path().join("test.sqlite")).unwrap();
        let paper = Paper {
            author: "Alice".to_string(),
            paper_id: "paper-a".to_string(),
            paper_dir: dir.path().to_path_buf(),
            article_path: dir.path().join("article.md"),
            fetch_result_path: None,
            source_hash: "source".to_string(),
            metadata: BTreeMap::from([
                ("title".to_string(), "A Paper".to_string()),
                ("year".to_string(), "2024".to_string()),
            ]),
            fetch_result: json!({}),
            raw_body: String::new(),
            clean_text: "Catalyst conversion improved.".to_string(),
            sections: Vec::new(),
        };
        let chunks = chunk_paper(&paper, 3200, 350);
        storage.upsert_paper(&paper, &chunks).unwrap();
        let chunk = storage.all_chunks_for_author("Alice", Some(1)).unwrap()[0].clone();
        storage
            .save_chunk_classification(NewChunkClassification {
                chunk_id: chunk.id,
                paper_key: &chunk.paper_key,
                chunk_kind: "methods",
                usefulness_score: 0.9,
                skip_reason: None,
                classifier_version: "v1",
                source_hash: &chunk.source_hash,
                chunk_hash: &chunk.chunk_hash,
            })
            .unwrap();

        let changed = storage
            .save_chunk_classification(NewChunkClassification {
                chunk_id: chunk.id,
                paper_key: &chunk.paper_key,
                chunk_kind: "methods",
                usefulness_score: 0.9,
                skip_reason: None,
                classifier_version: "v1",
                source_hash: &chunk.source_hash,
                chunk_hash: &chunk.chunk_hash,
            })
            .unwrap();

        assert!(!changed);
    }
}
