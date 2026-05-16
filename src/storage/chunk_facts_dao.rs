use anyhow::Result;
use rusqlite::{OptionalExtension, params};

use super::{ChunkFact, NewChunkFact, SourceChunk, Storage};

impl Storage {
    pub fn save_chunk_fact(&self, fact: NewChunkFact<'_>) -> Result<bool> {
        self.conn.execute(
            r#"
            DELETE FROM chunk_facts
            WHERE chunk_id = ?
              AND extractor = ?
              AND extractor_version = ?
              AND claim_uid != ?
            "#,
            params![
                fact.chunk_id,
                fact.extractor,
                fact.extractor_version,
                fact.claim_uid
            ],
        )?;
        let changed = self.chunk_fact_changed(fact)?;
        self.conn.execute(
            r#"
            INSERT INTO chunk_facts (
                claim_uid, paper_key, chunk_id, fact_type, fact_json, confidence,
                extractor, extractor_version, source_hash, chunk_hash, created_at
            )
            VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, CURRENT_TIMESTAMP)
            ON CONFLICT(claim_uid) DO UPDATE SET
                paper_key = excluded.paper_key,
                chunk_id = excluded.chunk_id,
                fact_type = excluded.fact_type,
                fact_json = excluded.fact_json,
                confidence = excluded.confidence,
                extractor = excluded.extractor,
                extractor_version = excluded.extractor_version,
                source_hash = excluded.source_hash,
                chunk_hash = excluded.chunk_hash
            "#,
            params![
                fact.claim_uid,
                fact.paper_key,
                fact.chunk_id,
                fact.fact_type,
                fact.fact_json,
                fact.confidence,
                fact.extractor,
                fact.extractor_version,
                fact.source_hash,
                fact.chunk_hash,
            ],
        )?;
        Ok(changed)
    }

    pub fn chunk_fact(&self, claim_uid: &str) -> Result<Option<ChunkFact>> {
        self.conn
            .query_row(
                r#"
                SELECT chunk_fact_id, claim_uid, paper_key, chunk_id, fact_type, fact_json,
                       confidence, extractor, extractor_version, source_hash, chunk_hash,
                       created_at
                FROM chunk_facts
                WHERE claim_uid = ?
                "#,
                params![claim_uid],
                chunk_fact_from_row,
            )
            .optional()
            .map_err(Into::into)
    }

    pub fn has_current_chunk_fact(
        &self,
        chunk: &SourceChunk,
        extractor: &str,
        extractor_version: &str,
    ) -> Result<bool> {
        let current: Option<i64> = self
            .conn
            .query_row(
                r#"
                SELECT 1
                FROM chunk_facts
                WHERE chunk_id = ?
                  AND extractor = ?
                  AND extractor_version = ?
                  AND source_hash = ?
                  AND chunk_hash = ?
                "#,
                params![
                    chunk.id,
                    extractor,
                    extractor_version,
                    chunk.source_hash,
                    chunk.chunk_hash
                ],
                |row| row.get(0),
            )
            .optional()?;
        Ok(current.is_some())
    }

    pub fn chunk_fact_count_for_author(&self, author: &str) -> Result<usize> {
        let count: i64 = self.conn.query_row(
            r#"
            SELECT COUNT(*)
            FROM chunk_facts f
            JOIN papers p ON p.paper_key = f.paper_key
            WHERE p.author = ?
            "#,
            params![author],
            |row| row.get(0),
        )?;
        Ok(count as usize)
    }

    pub fn record_chunk_fact_failure(
        &self,
        chunk: &SourceChunk,
        extractor: &str,
        extractor_version: &str,
        error: &str,
    ) -> Result<()> {
        self.conn.execute(
            r#"
            INSERT INTO chunk_fact_failures (
                paper_key, chunk_id, extractor, extractor_version, source_hash,
                chunk_hash, error, created_at, updated_at
            )
            VALUES (?, ?, ?, ?, ?, ?, ?, CURRENT_TIMESTAMP, CURRENT_TIMESTAMP)
            ON CONFLICT(chunk_id, extractor, extractor_version) DO UPDATE SET
                paper_key = excluded.paper_key,
                source_hash = excluded.source_hash,
                chunk_hash = excluded.chunk_hash,
                error = excluded.error,
                updated_at = CURRENT_TIMESTAMP
            "#,
            params![
                chunk.paper_key,
                chunk.id,
                extractor,
                extractor_version,
                chunk.source_hash,
                chunk.chunk_hash,
                error,
            ],
        )?;
        Ok(())
    }

    pub fn clear_chunk_fact_failure(
        &self,
        chunk: &SourceChunk,
        extractor: &str,
        extractor_version: &str,
    ) -> Result<()> {
        self.conn.execute(
            r#"
            DELETE FROM chunk_fact_failures
            WHERE chunk_id = ?
              AND extractor = ?
              AND extractor_version = ?
            "#,
            params![chunk.id, extractor, extractor_version],
        )?;
        Ok(())
    }

    pub fn failed_chunk_fact_chunk_ids(
        &self,
        author: &str,
        extractor: &str,
        extractor_version: &str,
        limit: Option<usize>,
    ) -> Result<Vec<i64>> {
        let mut sql = r#"
            SELECT f.chunk_id
            FROM chunk_fact_failures f
            JOIN papers p ON p.paper_key = f.paper_key
            JOIN chunks c ON c.id = f.chunk_id
            WHERE p.author = ?
              AND f.extractor = ?
              AND f.extractor_version = ?
              AND f.source_hash = c.source_hash
              AND f.chunk_hash = c.chunk_hash
            ORDER BY f.updated_at ASC, f.id ASC
        "#
        .to_string();
        if limit.is_some() {
            sql.push_str(" LIMIT ?");
        }
        let mut ids = Vec::new();
        if let Some(limit) = limit {
            let mut stmt = self.conn.prepare(&sql)?;
            let rows = stmt.query_map(
                params![author, extractor, extractor_version, limit as i64],
                |row| row.get(0),
            )?;
            for row in rows {
                ids.push(row?);
            }
        } else {
            let mut stmt = self.conn.prepare(&sql)?;
            let rows = stmt.query_map(params![author, extractor, extractor_version], |row| {
                row.get(0)
            })?;
            for row in rows {
                ids.push(row?);
            }
        }
        Ok(ids)
    }

    fn chunk_fact_changed(&self, fact: NewChunkFact<'_>) -> Result<bool> {
        let existing: Option<ExistingChunkFact> = self
            .conn
            .query_row(
                r#"
                SELECT paper_key, chunk_id, fact_type, fact_json, confidence,
                       extractor, extractor_version, source_hash, chunk_hash
                FROM chunk_facts
                WHERE claim_uid = ?
                "#,
                params![fact.claim_uid],
                |row| {
                    Ok(ExistingChunkFact {
                        paper_key: row.get(0)?,
                        chunk_id: row.get(1)?,
                        fact_type: row.get(2)?,
                        fact_json: row.get(3)?,
                        confidence: row.get(4)?,
                        extractor: row.get(5)?,
                        extractor_version: row.get(6)?,
                        source_hash: row.get(7)?,
                        chunk_hash: row.get(8)?,
                    })
                },
            )
            .optional()?;
        let Some(existing) = existing else {
            return Ok(true);
        };
        Ok(existing.paper_key != fact.paper_key
            || existing.chunk_id != fact.chunk_id
            || existing.fact_type != fact.fact_type
            || existing.fact_json != fact.fact_json
            || existing.confidence.as_deref() != fact.confidence
            || existing.extractor != fact.extractor
            || existing.extractor_version != fact.extractor_version
            || existing.source_hash != fact.source_hash
            || existing.chunk_hash != fact.chunk_hash)
    }
}

struct ExistingChunkFact {
    paper_key: String,
    chunk_id: i64,
    fact_type: String,
    fact_json: String,
    confidence: Option<String>,
    extractor: String,
    extractor_version: String,
    source_hash: String,
    chunk_hash: String,
}

fn chunk_fact_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<ChunkFact> {
    Ok(ChunkFact {
        chunk_fact_id: row.get(0)?,
        claim_uid: row.get(1)?,
        paper_key: row.get(2)?,
        chunk_id: row.get(3)?,
        fact_type: row.get(4)?,
        fact_json: row.get(5)?,
        confidence: row.get(6)?,
        extractor: row.get(7)?,
        extractor_version: row.get(8)?,
        source_hash: row.get(9)?,
        chunk_hash: row.get(10)?,
        created_at: row.get(11)?,
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
    fn saves_and_checks_current_chunk_fact() {
        let dir = tempdir().unwrap();
        let mut storage = Storage::open(&dir.path().join("test.sqlite")).unwrap();
        let chunk = seed_chunk(&mut storage, dir.path());

        let changed = storage
            .save_chunk_fact(NewChunkFact {
                claim_uid: "claim-a",
                paper_key: &chunk.paper_key,
                chunk_id: chunk.id,
                fact_type: "result",
                fact_json: r#"{"claim":"conversion improved"}"#,
                confidence: Some("high"),
                extractor: "chunk_fact_extractor",
                extractor_version: "chunk-facts-v1",
                source_hash: &chunk.source_hash,
                chunk_hash: &chunk.chunk_hash,
            })
            .unwrap();

        assert!(changed);
        assert!(
            storage
                .has_current_chunk_fact(&chunk, "chunk_fact_extractor", "chunk-facts-v1")
                .unwrap()
        );
        let fact = storage.chunk_fact("claim-a").unwrap().unwrap();
        assert_eq!(fact.fact_type, "result");
        assert_eq!(fact.confidence.as_deref(), Some("high"));
    }

    #[test]
    fn unchanged_chunk_fact_reports_not_changed() {
        let dir = tempdir().unwrap();
        let mut storage = Storage::open(&dir.path().join("test.sqlite")).unwrap();
        let chunk = seed_chunk(&mut storage, dir.path());
        let fact = NewChunkFact {
            claim_uid: "claim-a",
            paper_key: &chunk.paper_key,
            chunk_id: chunk.id,
            fact_type: "method",
            fact_json: r#"{"claim":"tested catalysts"}"#,
            confidence: Some("high"),
            extractor: "chunk_fact_extractor",
            extractor_version: "chunk-facts-v1",
            source_hash: &chunk.source_hash,
            chunk_hash: &chunk.chunk_hash,
        };
        storage.save_chunk_fact(fact).unwrap();

        let changed = storage.save_chunk_fact(fact).unwrap();

        assert!(!changed);
    }

    #[test]
    fn new_claim_replaces_previous_claim_for_same_chunk() {
        let dir = tempdir().unwrap();
        let mut storage = Storage::open(&dir.path().join("test.sqlite")).unwrap();
        let chunk = seed_chunk(&mut storage, dir.path());
        storage
            .save_chunk_fact(NewChunkFact {
                claim_uid: "claim-a",
                paper_key: &chunk.paper_key,
                chunk_id: chunk.id,
                fact_type: "method",
                fact_json: r#"{"claim":"old"}"#,
                confidence: Some("medium"),
                extractor: "chunk_fact_extractor",
                extractor_version: "chunk-facts-v1",
                source_hash: &chunk.source_hash,
                chunk_hash: &chunk.chunk_hash,
            })
            .unwrap();

        storage
            .save_chunk_fact(NewChunkFact {
                claim_uid: "claim-b",
                paper_key: &chunk.paper_key,
                chunk_id: chunk.id,
                fact_type: "result",
                fact_json: r#"{"claim":"new"}"#,
                confidence: Some("high"),
                extractor: "chunk_fact_extractor",
                extractor_version: "chunk-facts-v1",
                source_hash: &chunk.source_hash,
                chunk_hash: &chunk.chunk_hash,
            })
            .unwrap();

        assert!(storage.chunk_fact("claim-a").unwrap().is_none());
        assert!(storage.chunk_fact("claim-b").unwrap().is_some());
    }

    #[test]
    fn records_lists_and_clears_chunk_fact_failures() {
        let dir = tempdir().unwrap();
        let mut storage = Storage::open(&dir.path().join("test.sqlite")).unwrap();
        let chunk = seed_chunk(&mut storage, dir.path());
        storage
            .record_chunk_fact_failure(
                &chunk,
                "chunk_fact_extractor",
                "chunk-facts-v1",
                "transient error",
            )
            .unwrap();

        let ids = storage
            .failed_chunk_fact_chunk_ids("Alice", "chunk_fact_extractor", "chunk-facts-v1", None)
            .unwrap();
        assert_eq!(ids, vec![chunk.id]);

        storage
            .clear_chunk_fact_failure(&chunk, "chunk_fact_extractor", "chunk-facts-v1")
            .unwrap();
        let ids = storage
            .failed_chunk_fact_chunk_ids("Alice", "chunk_fact_extractor", "chunk-facts-v1", None)
            .unwrap();
        assert!(ids.is_empty());
    }

    fn seed_chunk(storage: &mut Storage, root: &std::path::Path) -> SourceChunk {
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
            clean_text: "Catalyst conversion improved under mild conditions.".to_string(),
            sections: Vec::new(),
        };
        let chunks = chunk_paper(&paper, 3200, 350);
        storage.upsert_paper(&paper, &chunks).unwrap();
        storage.all_chunks_for_author("Alice", Some(1)).unwrap()[0].clone()
    }
}
