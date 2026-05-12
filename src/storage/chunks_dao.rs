use anyhow::Result;
use rusqlite::params;
use serde_json::Value;

use crate::retrieval::hybrid;

use super::{SourceChunk, Storage, source_chunk_from_row};

impl Storage {
    pub fn chunks_for_paper_keys(
        &self,
        paper_keys: &[String],
        limit: usize,
    ) -> Result<Vec<SourceChunk>> {
        let mut chunks = Vec::new();
        for paper_key in paper_keys {
            if chunks.len() >= limit {
                break;
            }
            let mut stmt = self.conn.prepare(
                r#"
                SELECT c.id, c.paper_key, c.chunk_index, c.section, c.text,
                       p.title, p.doi, p.year, p.source_hash,
                       COALESCE(c.chunk_hash, ''), COALESCE(c.chunker_version, ''),
                       COALESCE(c.section_kind, 'body'), c.caption_label
                FROM chunks c
                JOIN papers p ON p.paper_key = c.paper_key
                WHERE c.paper_key = ?
                ORDER BY c.chunk_index ASC
                LIMIT 1
                "#,
            )?;
            let rows = stmt.query_map(params![paper_key], source_chunk_from_row)?;
            for row in rows {
                chunks.push(row?);
            }
        }
        Ok(chunks)
    }

    pub fn recent_chunks(&self, author: &str, limit: usize) -> Result<Vec<SourceChunk>> {
        let mut stmt = self.conn.prepare(
            r#"
            SELECT c.id, c.paper_key, c.chunk_index, c.section, c.text,
                   p.title, p.doi, p.year, p.source_hash,
                   COALESCE(c.chunk_hash, ''), COALESCE(c.chunker_version, ''),
                   COALESCE(c.section_kind, 'body'), c.caption_label
            FROM chunks c
            JOIN papers p ON p.paper_key = c.paper_key
            WHERE p.author = ?
            ORDER BY p.year DESC, p.title ASC, c.chunk_index ASC
            LIMIT ?
            "#,
        )?;
        let rows = stmt.query_map(params![author, limit as i64], source_chunk_from_row)?;
        rows.collect::<rusqlite::Result<Vec<_>>>()
            .map_err(Into::into)
    }

    pub fn all_chunks_for_author(
        &self,
        author: &str,
        limit: Option<usize>,
    ) -> Result<Vec<SourceChunk>> {
        let mut sql = r#"
            SELECT c.id, c.paper_key, c.chunk_index, c.section, c.text,
                   p.title, p.doi, p.year, p.source_hash,
                   COALESCE(c.chunk_hash, ''), COALESCE(c.chunker_version, ''),
                   COALESCE(c.section_kind, 'body'), c.caption_label
            FROM chunks c
            JOIN papers p ON p.paper_key = c.paper_key
            WHERE p.author = ?
            ORDER BY p.year DESC, p.title ASC, c.chunk_index ASC
        "#
        .to_string();
        if limit.is_some() {
            sql.push_str(" LIMIT ?");
        }
        let mut chunks = Vec::new();
        if let Some(limit) = limit {
            let mut stmt = self.conn.prepare(&sql)?;
            let rows = stmt.query_map(params![author, limit as i64], source_chunk_from_row)?;
            for row in rows {
                chunks.push(row?);
            }
        } else {
            let mut stmt = self.conn.prepare(&sql)?;
            let rows = stmt.query_map(params![author], source_chunk_from_row)?;
            for row in rows {
                chunks.push(row?);
            }
        }
        Ok(chunks)
    }

    pub fn search_chunks(
        &self,
        author: &str,
        query: &str,
        limit: usize,
    ) -> Result<Vec<SourceChunk>> {
        self.search_chunks_with_trace(author, query, limit)
            .map(|(chunks, _)| chunks)
    }

    pub fn search_chunks_with_trace(
        &self,
        author: &str,
        query: &str,
        limit: usize,
    ) -> Result<(Vec<SourceChunk>, Value)> {
        hybrid::search_chunks_with_trace(self, author, query, limit)
    }

    pub fn search_chunks_with_dense_vector(
        &self,
        author: &str,
        query: &str,
        limit: usize,
        model: &str,
        model_version: Option<&str>,
        query_vector: &[f32],
    ) -> Result<Vec<SourceChunk>> {
        self.search_chunks_with_dense_vector_trace(
            author,
            query,
            limit,
            model,
            model_version,
            query_vector,
        )
        .map(|(chunks, _)| chunks)
    }

    pub fn search_chunks_with_dense_vector_trace(
        &self,
        author: &str,
        query: &str,
        limit: usize,
        model: &str,
        model_version: Option<&str>,
        query_vector: &[f32],
    ) -> Result<(Vec<SourceChunk>, Value)> {
        hybrid::search_chunks_with_dense_vector_trace(
            self,
            author,
            query,
            limit,
            model,
            model_version,
            query_vector,
        )
    }

    pub(crate) fn search_chunks_fts(
        &self,
        author: &str,
        match_query: &str,
        limit: usize,
    ) -> Result<Vec<SourceChunk>> {
        let mut stmt = self.conn.prepare(
            r#"
            SELECT c.id, c.paper_key, c.chunk_index, c.section, c.text,
                   p.title, p.doi, p.year, p.source_hash,
                   COALESCE(c.chunk_hash, ''), COALESCE(c.chunker_version, ''),
                   COALESCE(c.section_kind, 'body'), c.caption_label
            FROM chunks_fts f
            JOIN chunks c ON c.id = f.chunk_id
            JOIN papers p ON p.paper_key = c.paper_key
            WHERE f.author = ? AND chunks_fts MATCH ?
            ORDER BY bm25(chunks_fts)
            LIMIT ?
            "#,
        )?;
        let rows = stmt.query_map(
            params![author, match_query, limit as i64],
            source_chunk_from_row,
        )?;
        rows.collect::<rusqlite::Result<Vec<_>>>()
            .map_err(Into::into)
    }
}
