use anyhow::Result;
use rusqlite::{OptionalExtension, params};
use serde_json::{Value, json};

use super::{
    FactRouteCandidate, SOURCE_CHUNK_COLUMN_COUNT, SOURCE_CHUNK_SELECT_COLUMNS, Storage,
    source_chunk_from_row,
};

impl Storage {
    pub fn save_paper_facts(&self, paper_key: &str, facts: &[Value]) -> Result<()> {
        for fact in facts {
            let chunk_index = fact.get("chunk_id").and_then(Value::as_i64);
            let (source_hash, chunk_hash) = self.chunk_metadata(paper_key, chunk_index)?;
            self.conn.execute(
                r#"
                INSERT INTO paper_facts
                    (paper_key, chunk_id, section, fact_type, fact_json, profile_schema_version,
                     source_hash, chunk_hash, extractor, extractor_version)
                VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
                "#,
                params![
                    paper_key,
                    chunk_index,
                    fact.get("section")
                        .and_then(Value::as_str)
                        .unwrap_or_default(),
                    fact.get("fact_type")
                        .and_then(Value::as_str)
                        .unwrap_or("unknown"),
                    serde_json::to_string(fact)?,
                    1i64,
                    source_hash,
                    chunk_hash,
                    "section_fact_extractor",
                    "section-facts-v1",
                ],
            )?;
        }
        Ok(())
    }

    pub(crate) fn save_profile_facts(
        &self,
        paper_key: &str,
        profile: &Value,
        profile_schema_version: i64,
    ) -> Result<()> {
        self.conn.execute(
            "DELETE FROM paper_facts WHERE paper_key = ?",
            params![paper_key],
        )?;
        for (field, fact_type, text_key) in [
            ("methods", "method", "method"),
            ("key_results", "result", "claim"),
            ("limitations", "limitation", "limitation"),
        ] {
            let Some(items) = profile.get(field).and_then(Value::as_array) else {
                continue;
            };
            for item in items {
                let chunk_ids = item
                    .get("evidence_chunks")
                    .and_then(Value::as_array)
                    .cloned()
                    .unwrap_or_default();
                let section = item
                    .get("section")
                    .and_then(Value::as_str)
                    .unwrap_or_default();
                let text = item
                    .get(text_key)
                    .and_then(Value::as_str)
                    .unwrap_or_default();
                let fact_json = json!({
                    "text": text,
                    "source_field": field,
                    "raw": item,
                });
                for chunk_id in chunk_ids {
                    let chunk_index = chunk_id.as_i64();
                    let (source_hash, chunk_hash) = self.chunk_metadata(paper_key, chunk_index)?;
                    self.conn.execute(
                        r#"
                        INSERT INTO paper_facts
                            (paper_key, chunk_id, section, fact_type, fact_json,
                             profile_schema_version, source_hash, chunk_hash, extractor,
                             extractor_version)
                        VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
                        "#,
                        params![
                            paper_key,
                            chunk_index,
                            section,
                            fact_type,
                            serde_json::to_string(&fact_json)?,
                            profile_schema_version,
                            source_hash,
                            chunk_hash,
                            "paper_profile",
                            "paper-profile-v1",
                        ],
                    )?;
                }
            }
        }
        Ok(())
    }

    fn chunk_metadata(
        &self,
        paper_key: &str,
        chunk_index: Option<i64>,
    ) -> Result<(Option<String>, Option<String>)> {
        let Some(chunk_index) = chunk_index else {
            return Ok((None, None));
        };
        let metadata = self
            .conn
            .query_row(
                r#"
                SELECT p.source_hash, c.chunk_hash
                FROM papers p
                LEFT JOIN chunks c ON c.paper_key = p.paper_key AND c.chunk_index = ?
                WHERE p.paper_key = ?
                "#,
                params![chunk_index, paper_key],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()?;
        Ok(metadata.unwrap_or((None, None)))
    }

    pub(crate) fn fact_route_candidates(&self, author: &str) -> Result<Vec<FactRouteCandidate>> {
        let mut stmt = self.conn.prepare(&format!(
            r#"
            SELECT {SOURCE_CHUNK_SELECT_COLUMNS},
                   f.fact_type, f.fact_json
            FROM paper_facts f
            JOIN chunks c ON c.paper_key = f.paper_key AND c.chunk_index = COALESCE(f.chunk_id, c.chunk_index)
            JOIN papers p ON p.paper_key = c.paper_key
            WHERE p.author = ?
            "#,
        ))?;
        let rows = stmt.query_map(params![author], fact_route_candidate_from_row)?;
        rows.collect::<rusqlite::Result<Vec<_>>>()
            .map_err(Into::into)
    }
}

fn fact_route_candidate_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<FactRouteCandidate> {
    Ok(FactRouteCandidate {
        chunk: source_chunk_from_row(row)?,
        fact_type: row.get(SOURCE_CHUNK_COLUMN_COUNT)?,
        fact_json: row.get(SOURCE_CHUNK_COLUMN_COUNT + 1)?,
    })
}
