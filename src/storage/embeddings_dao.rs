use anyhow::Result;
use rusqlite::{OptionalExtension, params};

use crate::retrieval::embedding::encode_f32_vector;

use super::{
    SOURCE_CHUNK_COLUMN_COUNT, SOURCE_CHUNK_SELECT_COLUMNS, SourceChunk, Storage,
    source_chunk_from_row,
};

impl Storage {
    pub fn has_current_chunk_embedding(
        &self,
        chunk_id: i64,
        model: &str,
        model_version: Option<&str>,
        source_hash: &str,
        chunk_hash: &str,
    ) -> Result<bool> {
        let current: Option<i64> = self
            .conn
            .query_row(
                r#"
                SELECT 1
                FROM embeddings
                WHERE target_type = 'chunk'
                  AND target_id = ?
                  AND model = ?
                  AND COALESCE(model_version, '') = COALESCE(?, '')
                  AND COALESCE(source_hash, '') = ?
                  AND COALESCE(chunk_hash, '') = ?
                LIMIT 1
                "#,
                params![
                    chunk_id.to_string(),
                    model,
                    model_version,
                    source_hash,
                    chunk_hash
                ],
                |row| row.get(0),
            )
            .optional()?;
        Ok(current.is_some())
    }

    pub fn save_chunk_embedding(
        &self,
        chunk: &SourceChunk,
        model: &str,
        model_version: Option<&str>,
        vector: &[f32],
        chunk_hash: &str,
    ) -> Result<()> {
        self.conn.execute(
            r#"
            INSERT OR REPLACE INTO embeddings
                (target_type, target_id, model, model_version, dim, vector, source_hash, chunk_hash)
            VALUES ('chunk', ?, ?, ?, ?, ?, ?, ?)
            "#,
            params![
                chunk.id.to_string(),
                model,
                model_version.unwrap_or(""),
                vector.len() as i64,
                encode_f32_vector(vector),
                chunk.source_hash,
                chunk_hash,
            ],
        )?;
        Ok(())
    }

    pub fn record_embedding_job(
        &self,
        chunk: &SourceChunk,
        model: &str,
        model_version: Option<&str>,
        chunk_hash: &str,
        status: &str,
        error: Option<&str>,
    ) -> Result<()> {
        self.conn.execute(
            r#"
            INSERT INTO embedding_jobs (
                target_type, target_id, model, model_version, source_hash,
                chunk_hash, status, attempt_count, last_error, updated_at
            )
            VALUES ('chunk', ?, ?, ?, ?, ?, ?, 1, ?, CURRENT_TIMESTAMP)
            "#,
            params![
                chunk.id.to_string(),
                model,
                model_version.unwrap_or(""),
                chunk.source_hash,
                chunk_hash,
                status,
                error,
            ],
        )?;
        Ok(())
    }

    pub(crate) fn embedding_route_candidates(
        &self,
        author: &str,
        model: &str,
        model_version: Option<&str>,
    ) -> Result<Vec<(SourceChunk, Vec<u8>)>> {
        let mut stmt = self.conn.prepare(&format!(
            r#"
            SELECT {SOURCE_CHUNK_SELECT_COLUMNS},
                   e.vector
            FROM embeddings e
            JOIN chunks c ON c.id = CAST(e.target_id AS INTEGER)
            JOIN papers p ON p.paper_key = c.paper_key
            WHERE e.target_type = 'chunk'
              AND e.model = ?
              AND COALESCE(e.model_version, '') = COALESCE(?, '')
              AND p.author = ?
            "#,
        ))?;
        let rows = stmt.query_map(
            params![model, model_version, author],
            embedding_candidate_from_row,
        )?;
        rows.collect::<rusqlite::Result<Vec<_>>>()
            .map_err(Into::into)
    }
}

fn embedding_candidate_from_row(
    row: &rusqlite::Row<'_>,
) -> rusqlite::Result<(SourceChunk, Vec<u8>)> {
    let vector = row.get(SOURCE_CHUNK_COLUMN_COUNT)?;
    Ok((source_chunk_from_row(row)?, vector))
}
