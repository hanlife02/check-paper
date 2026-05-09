use std::path::Path;

use anyhow::Result;
use rusqlite::{Connection, OptionalExtension, params};
use serde_json::{Value, json};

use crate::papers::models::Paper;
use crate::retrieval::chunker::Chunk;

#[derive(Debug, Clone)]
pub struct AnalysisCandidate {
    pub paper_key: String,
    pub author: String,
    pub paper_id: String,
    pub title: String,
    pub doi: String,
    pub year: String,
    pub source_hash: String,
    pub article_path: String,
}

#[derive(Debug, Clone)]
pub struct SourceChunk {
    pub id: i64,
    pub paper_key: String,
    pub chunk_index: i64,
    pub section: String,
    pub text: String,
    pub title: String,
    pub doi: String,
    pub year: String,
}

pub struct Storage {
    conn: Connection,
}

impl Storage {
    pub fn open(path: &Path) -> Result<Self> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let conn = Connection::open(path)?;
        let storage = Self { conn };
        storage.init_schema()?;
        Ok(storage)
    }

    fn init_schema(&self) -> Result<()> {
        self.conn.execute_batch(
            r#"
            CREATE TABLE IF NOT EXISTS papers (
                paper_key TEXT PRIMARY KEY,
                author TEXT NOT NULL,
                paper_id TEXT NOT NULL,
                title TEXT NOT NULL,
                doi TEXT,
                year TEXT,
                source_hash TEXT NOT NULL,
                article_path TEXT NOT NULL,
                fetch_result_path TEXT,
                metadata_json TEXT NOT NULL,
                fetch_result_json TEXT NOT NULL,
                profile_json TEXT,
                analyzed_hash TEXT,
                analyzed_at TEXT,
                updated_at TEXT DEFAULT CURRENT_TIMESTAMP
            );

            CREATE TABLE IF NOT EXISTS chunks (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                paper_key TEXT NOT NULL,
                chunk_index INTEGER NOT NULL,
                section TEXT NOT NULL,
                text TEXT NOT NULL,
                FOREIGN KEY (paper_key) REFERENCES papers(paper_key) ON DELETE CASCADE
            );

            CREATE TABLE IF NOT EXISTS author_profiles (
                author TEXT PRIMARY KEY,
                profile_json TEXT NOT NULL,
                updated_at TEXT DEFAULT CURRENT_TIMESTAMP
            );
            "#,
        )?;
        let _ = self.conn.execute_batch(
            r#"
            CREATE VIRTUAL TABLE IF NOT EXISTS chunks_fts USING fts5(
                text,
                paper_key UNINDEXED,
                chunk_id UNINDEXED,
                title UNINDEXED,
                doi UNINDEXED,
                author UNINDEXED
            );
            "#,
        );
        Ok(())
    }

    pub fn upsert_paper(&mut self, paper: &Paper, chunks: &[Chunk]) -> Result<bool> {
        let existing: Option<String> = self
            .conn
            .query_row(
                "SELECT source_hash FROM papers WHERE paper_key = ?",
                params![paper.key()],
                |row| row.get(0),
            )
            .optional()?;
        let changed = existing.as_deref() != Some(paper.source_hash.as_str());

        let tx = self.conn.transaction()?;
        tx.execute(
            r#"
            INSERT INTO papers (
                paper_key, author, paper_id, title, doi, year, source_hash,
                article_path, fetch_result_path, metadata_json, fetch_result_json, updated_at
            )
            VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, CURRENT_TIMESTAMP)
            ON CONFLICT(paper_key) DO UPDATE SET
                title = excluded.title,
                doi = excluded.doi,
                year = excluded.year,
                source_hash = excluded.source_hash,
                article_path = excluded.article_path,
                fetch_result_path = excluded.fetch_result_path,
                metadata_json = excluded.metadata_json,
                fetch_result_json = excluded.fetch_result_json,
                updated_at = CURRENT_TIMESTAMP
            "#,
            params![
                paper.key(),
                paper.author,
                paper.paper_id,
                paper.title(),
                paper.doi(),
                paper.year(),
                paper.source_hash,
                paper.article_path.to_string_lossy(),
                paper
                    .fetch_result_path
                    .as_ref()
                    .map(|path| path.to_string_lossy().to_string()),
                serde_json::to_string(&paper.metadata)?,
                serde_json::to_string(&paper.fetch_result)?,
            ],
        )?;

        if changed {
            tx.execute(
                "UPDATE papers SET profile_json = NULL, analyzed_hash = NULL, analyzed_at = NULL WHERE paper_key = ?",
                params![paper.key()],
            )?;
            tx.execute(
                "DELETE FROM chunks WHERE paper_key = ?",
                params![paper.key()],
            )?;
            let _ = tx.execute(
                "DELETE FROM chunks_fts WHERE paper_key = ?",
                params![paper.key()],
            );
            for chunk in chunks {
                tx.execute(
                    "INSERT INTO chunks (paper_key, chunk_index, section, text) VALUES (?, ?, ?, ?)",
                    params![chunk.paper_key, chunk.chunk_index as i64, chunk.section, chunk.text],
                )?;
                let chunk_id = tx.last_insert_rowid();
                let _ = tx.execute(
                    "INSERT INTO chunks_fts (text, paper_key, chunk_id, title, doi, author) VALUES (?, ?, ?, ?, ?, ?)",
                    params![chunk.text, paper.key(), chunk_id, paper.title(), paper.doi(), paper.author],
                );
            }
        }
        tx.commit()?;
        Ok(changed)
    }

    pub fn papers_needing_analysis(
        &self,
        author: &str,
        force: bool,
    ) -> Result<Vec<AnalysisCandidate>> {
        let sql = if force {
            r#"
            SELECT paper_key, author, paper_id, title, doi, year, source_hash, article_path
            FROM papers
            WHERE author = ?
            ORDER BY year DESC, paper_id DESC
            "#
        } else {
            r#"
            SELECT paper_key, author, paper_id, title, doi, year, source_hash, article_path
            FROM papers
            WHERE author = ? AND (profile_json IS NULL OR analyzed_hash IS NULL OR analyzed_hash != source_hash)
            ORDER BY year DESC, paper_id DESC
            "#
        };
        let mut stmt = self.conn.prepare(sql)?;
        let rows = stmt.query_map(params![author], |row| {
            Ok(AnalysisCandidate {
                paper_key: row.get(0)?,
                author: row.get(1)?,
                paper_id: row.get(2)?,
                title: row.get(3)?,
                doi: row.get(4)?,
                year: row.get(5)?,
                source_hash: row.get(6)?,
                article_path: row.get(7)?,
            })
        })?;
        rows.collect::<rusqlite::Result<Vec<_>>>()
            .map_err(Into::into)
    }

    pub fn save_paper_profile(
        &self,
        paper_key: &str,
        source_hash: &str,
        profile: &Value,
    ) -> Result<()> {
        self.conn.execute(
            r#"
            UPDATE papers
            SET profile_json = ?, analyzed_hash = ?, analyzed_at = CURRENT_TIMESTAMP
            WHERE paper_key = ?
            "#,
            params![serde_json::to_string(profile)?, source_hash, paper_key],
        )?;
        Ok(())
    }

    pub fn paper_profiles(&self, author: &str, limit: Option<usize>) -> Result<Vec<Value>> {
        let mut sql = r#"
            SELECT paper_key, title, doi, year, profile_json
            FROM papers
            WHERE author = ? AND profile_json IS NOT NULL
            ORDER BY year DESC, title ASC
        "#
        .to_string();
        if limit.is_some() {
            sql.push_str(" LIMIT ?");
        }

        let mut profiles = Vec::new();
        if let Some(limit) = limit {
            let mut stmt = self.conn.prepare(&sql)?;
            let rows = stmt.query_map(params![author, limit as i64], parse_profile_row)?;
            for row in rows {
                profiles.push(row?);
            }
        } else {
            let mut stmt = self.conn.prepare(&sql)?;
            let rows = stmt.query_map(params![author], parse_profile_row)?;
            for row in rows {
                profiles.push(row?);
            }
        }
        Ok(profiles)
    }

    pub fn save_author_profile(&self, author: &str, profile: &Value) -> Result<()> {
        self.conn.execute(
            r#"
            INSERT INTO author_profiles (author, profile_json, updated_at)
            VALUES (?, ?, CURRENT_TIMESTAMP)
            ON CONFLICT(author) DO UPDATE SET
                profile_json = excluded.profile_json,
                updated_at = CURRENT_TIMESTAMP
            "#,
            params![author, serde_json::to_string(profile)?],
        )?;
        Ok(())
    }

    pub fn get_author_profile(&self, author: &str) -> Result<Option<Value>> {
        let value: Option<String> = self
            .conn
            .query_row(
                "SELECT profile_json FROM author_profiles WHERE author = ?",
                params![author],
                |row| row.get(0),
            )
            .optional()?;
        Ok(value.and_then(|text| serde_json::from_str(&text).ok()))
    }

    pub fn search_profiles(&self, author: &str, query: &str, limit: usize) -> Result<Vec<Value>> {
        let terms = query_terms(query);
        let profiles = self.paper_profiles(author, None)?;
        if terms.is_empty() {
            return Ok(profiles.into_iter().take(limit).collect());
        }
        let mut scored = Vec::new();
        for profile in profiles {
            let blob = profile.to_string().to_lowercase();
            let score: usize = terms
                .iter()
                .map(|term| blob.matches(&term.to_lowercase()).count())
                .sum();
            if score > 0 {
                scored.push((score, profile));
            }
        }
        scored.sort_by(|left, right| right.0.cmp(&left.0));
        if scored.is_empty() {
            Ok(self.paper_profiles(author, Some(limit))?)
        } else {
            Ok(scored
                .into_iter()
                .take(limit)
                .map(|(_, profile)| profile)
                .collect())
        }
    }

    pub fn search_chunks(
        &self,
        author: &str,
        query: &str,
        limit: usize,
    ) -> Result<Vec<SourceChunk>> {
        let terms = query_terms(query);
        if terms.is_empty() {
            return Ok(Vec::new());
        }
        let match_query = terms
            .iter()
            .take(12)
            .map(|term| format!("\"{}\"", term.replace('"', "\"\"")))
            .collect::<Vec<_>>()
            .join(" OR ");
        let fts_result = self.search_chunks_fts(author, &match_query, limit);
        match fts_result {
            Ok(rows) if !rows.is_empty() => Ok(rows),
            _ => self.search_chunks_like(author, &terms, limit),
        }
    }

    pub fn count_papers(&self, author: Option<&str>) -> Result<i64> {
        let count = if let Some(author) = author {
            self.conn.query_row(
                "SELECT COUNT(*) FROM papers WHERE author = ?",
                params![author],
                |row| row.get(0),
            )?
        } else {
            self.conn
                .query_row("SELECT COUNT(*) FROM papers", [], |row| row.get(0))?
        };
        Ok(count)
    }

    fn search_chunks_fts(
        &self,
        author: &str,
        match_query: &str,
        limit: usize,
    ) -> Result<Vec<SourceChunk>> {
        let mut stmt = self.conn.prepare(
            r#"
            SELECT c.id, c.paper_key, c.chunk_index, c.section, c.text,
                   p.title, p.doi, p.year
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

    fn search_chunks_like(
        &self,
        author: &str,
        terms: &[String],
        limit: usize,
    ) -> Result<Vec<SourceChunk>> {
        let mut stmt = self.conn.prepare(
            r#"
            SELECT c.id, c.paper_key, c.chunk_index, c.section, c.text,
                   p.title, p.doi, p.year
            FROM chunks c
            JOIN papers p ON p.paper_key = c.paper_key
            WHERE p.author = ?
            "#,
        )?;
        let rows = stmt.query_map(params![author], source_chunk_from_row)?;
        let mut scored = Vec::new();
        for row in rows {
            let row = row?;
            let blob =
                format!("{} {} {} {}", row.title, row.doi, row.section, row.text).to_lowercase();
            let score: usize = terms
                .iter()
                .map(|term| blob.matches(&term.to_lowercase()).count())
                .sum();
            if score > 0 {
                scored.push((score, row));
            }
        }
        scored.sort_by(|left, right| right.0.cmp(&left.0));
        Ok(scored.into_iter().take(limit).map(|(_, row)| row).collect())
    }
}

fn parse_profile_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<Value> {
    let paper_key: String = row.get(0)?;
    let title: String = row.get(1)?;
    let doi: String = row.get(2)?;
    let year: String = row.get(3)?;
    let profile_json: String = row.get(4)?;
    let mut profile: Value = serde_json::from_str(&profile_json)
        .unwrap_or_else(|_| json!({ "raw_profile": profile_json }));
    if let Some(object) = profile.as_object_mut() {
        object
            .entry("paper_key")
            .or_insert(Value::String(paper_key));
        object.entry("title").or_insert(Value::String(title));
        object.entry("doi").or_insert(Value::String(doi));
        object.entry("year").or_insert(Value::String(year));
    }
    Ok(profile)
}

fn source_chunk_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<SourceChunk> {
    Ok(SourceChunk {
        id: row.get(0)?,
        paper_key: row.get(1)?,
        chunk_index: row.get(2)?,
        section: row.get(3)?,
        text: row.get(4)?,
        title: row.get(5)?,
        doi: row.get(6)?,
        year: row.get(7)?,
    })
}

fn query_terms(query: &str) -> Vec<String> {
    let mut terms = Vec::new();
    let mut current = String::new();
    let mut current_is_cjk = false;

    for ch in query.chars() {
        let is_cjk = ('\u{4e00}'..='\u{9fff}').contains(&ch);
        let is_word = ch.is_alphanumeric() || matches!(ch, '-' | '_' | '/' | '.');
        if is_word {
            if !current.is_empty() && current_is_cjk != is_cjk {
                push_term(&mut terms, &mut current);
            }
            current_is_cjk = is_cjk;
            current.push(ch);
        } else {
            push_term(&mut terms, &mut current);
        }
    }
    push_term(&mut terms, &mut current);

    if terms.is_empty() && !query.trim().is_empty() {
        terms.push(query.trim().to_string());
    }
    terms
}

fn push_term(terms: &mut Vec<String>, current: &mut String) {
    let term = current.trim();
    if !term.is_empty()
        && (term.chars().count() > 1 || term.chars().all(|ch| ch.is_ascii_alphanumeric()))
        && !terms
            .iter()
            .any(|existing| existing.eq_ignore_ascii_case(term))
    {
        terms.push(term.to_string());
    }
    current.clear();
}
