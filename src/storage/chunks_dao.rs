use anyhow::Result;
use rusqlite::params;

use super::{SOURCE_CHUNK_SELECT_COLUMNS, SourceChunk, Storage, source_chunk_from_row};

impl Storage {
    fn chunks_for_single_paper(&self, paper_key: &str) -> Result<Vec<SourceChunk>> {
        let mut stmt = self.conn.prepare(&format!(
            r#"
            SELECT {SOURCE_CHUNK_SELECT_COLUMNS}
            FROM chunks c
            JOIN papers p ON p.paper_key = c.paper_key
            WHERE c.paper_key = ?
            ORDER BY c.chunk_index ASC
            "#,
        ))?;
        let rows = stmt.query_map(params![paper_key], source_chunk_from_row)?;
        rows.collect::<rusqlite::Result<Vec<_>>>()
            .map_err(Into::into)
    }

    pub fn recent_chunks(&self, author: &str, limit: usize) -> Result<Vec<SourceChunk>> {
        let mut stmt = self.conn.prepare(&format!(
            r#"
            SELECT {SOURCE_CHUNK_SELECT_COLUMNS}
            FROM chunks c
            JOIN papers p ON p.paper_key = c.paper_key
            WHERE p.author = ?
            ORDER BY p.year DESC, p.title ASC, c.chunk_index ASC
            LIMIT ?
            "#,
        ))?;
        let rows = stmt.query_map(params![author, limit as i64], source_chunk_from_row)?;
        rows.collect::<rusqlite::Result<Vec<_>>>()
            .map_err(Into::into)
    }

    pub fn all_chunks_for_author(
        &self,
        author: &str,
        limit: Option<usize>,
    ) -> Result<Vec<SourceChunk>> {
        let mut sql = format!(
            r#"
            SELECT {SOURCE_CHUNK_SELECT_COLUMNS}
            FROM chunks c
            JOIN papers p ON p.paper_key = c.paper_key
            WHERE p.author = ?
            ORDER BY p.year DESC, p.title ASC, c.chunk_index ASC
        "#
        );
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

    pub(crate) fn like_route_candidates(&self, author: &str) -> Result<Vec<SourceChunk>> {
        self.all_chunks_for_author(author, None)
    }

    pub(crate) fn profile_grounding_chunk_candidates_for_paper(
        &self,
        paper_key: &str,
    ) -> Result<Vec<SourceChunk>> {
        self.chunks_for_single_paper(paper_key)
    }

    pub(crate) fn fts_route_candidates(
        &self,
        author: &str,
        match_query: &str,
        limit: usize,
    ) -> Result<Vec<SourceChunk>> {
        let mut stmt = self.conn.prepare(&format!(
            r#"
            SELECT {SOURCE_CHUNK_SELECT_COLUMNS}
            FROM chunks_fts f
            JOIN chunks c ON c.id = f.chunk_id
            JOIN papers p ON p.paper_key = c.paper_key
            WHERE f.author = ? AND chunks_fts MATCH ?
            ORDER BY bm25(chunks_fts)
            LIMIT ?
            "#,
        ))?;
        let rows = stmt.query_map(
            params![author, match_query, limit as i64],
            source_chunk_from_row,
        )?;
        rows.collect::<rusqlite::Result<Vec<_>>>()
            .map_err(Into::into)
    }
}
