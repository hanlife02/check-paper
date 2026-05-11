use std::collections::{HashMap, HashSet};
use std::path::Path;

use anyhow::{Result, anyhow};
use rusqlite::{Connection, OptionalExtension, params};
use serde_json::{Value, json};

use crate::papers::models::Paper;
use crate::retrieval::chunker::Chunk;

const LOCAL_EMBEDDING_MODEL: &str = "local-hash-v1";
const LOCAL_EMBEDDING_DIM: usize = 64;

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

#[derive(Debug, Clone)]
pub struct LibraryStatus {
    pub papers: i64,
    pub analyzed: i64,
    pub failed_jobs: i64,
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
        self.apply_migrations()?;
        Ok(())
    }

    fn apply_migrations(&self) -> Result<()> {
        self.conn.execute_batch(
            r#"
            CREATE TABLE IF NOT EXISTS schema_migrations (
                version INTEGER PRIMARY KEY,
                name TEXT NOT NULL,
                applied_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP
            );
            "#,
        )?;
        self.apply_migration(
            1,
            "job_and_qa_logs",
            r#"
            CREATE TABLE IF NOT EXISTS analysis_jobs (
                id INTEGER PRIMARY KEY,
                paper_key TEXT,
                job_type TEXT NOT NULL,
                status TEXT NOT NULL,
                error TEXT,
                created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
                updated_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP
            );

            CREATE TABLE IF NOT EXISTS qa_logs (
                id INTEGER PRIMARY KEY,
                author TEXT NOT NULL,
                question TEXT NOT NULL,
                retrieval_json TEXT NOT NULL,
                answer_json TEXT NOT NULL,
                model TEXT,
                latency_ms INTEGER,
                created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP
            );

            CREATE TABLE IF NOT EXISTS paper_facts (
                id INTEGER PRIMARY KEY,
                paper_key TEXT NOT NULL,
                chunk_id INTEGER,
                section TEXT,
                fact_type TEXT NOT NULL,
                fact_json TEXT NOT NULL,
                profile_schema_version INTEGER,
                created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP
            );

            CREATE TABLE IF NOT EXISTS embeddings (
                target_type TEXT NOT NULL,
                target_id TEXT NOT NULL,
                model TEXT NOT NULL,
                dim INTEGER NOT NULL,
                vector BLOB NOT NULL,
                source_hash TEXT,
                created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
                PRIMARY KEY(target_type, target_id, model)
            );
            "#,
        )?;
        if !self.column_exists("papers", "profile_schema_version")? {
            self.conn.execute(
                "ALTER TABLE papers ADD COLUMN profile_schema_version INTEGER",
                [],
            )?;
        }
        Ok(())
    }

    fn apply_migration(&self, version: i64, name: &str, sql: &str) -> Result<()> {
        let applied: Option<i64> = self
            .conn
            .query_row(
                "SELECT version FROM schema_migrations WHERE version = ?",
                params![version],
                |row| row.get(0),
            )
            .optional()?;
        if applied.is_some() {
            return Ok(());
        }
        self.conn.execute_batch(sql)?;
        self.conn.execute(
            "INSERT INTO schema_migrations (version, name) VALUES (?, ?)",
            params![version, name],
        )?;
        Ok(())
    }

    fn column_exists(&self, table: &str, column: &str) -> Result<bool> {
        let mut stmt = self.conn.prepare(&format!("PRAGMA table_info({table})"))?;
        let rows = stmt.query_map([], |row| row.get::<_, String>(1))?;
        for row in rows {
            if row? == column {
                return Ok(true);
            }
        }
        Ok(false)
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
                let vector = local_embedding(&format!(
                    "{} {} {} {}",
                    paper.title(),
                    paper.doi(),
                    chunk.section,
                    chunk.text
                ));
                tx.execute(
                    r#"
                    INSERT OR REPLACE INTO embeddings
                        (target_type, target_id, model, dim, vector, source_hash)
                    VALUES ('chunk', ?, ?, ?, ?, ?)
                    "#,
                    params![
                        chunk_id.to_string(),
                        LOCAL_EMBEDDING_MODEL,
                        LOCAL_EMBEDDING_DIM as i64,
                        encode_f32_vector(&vector),
                        paper.source_hash
                    ],
                )?;
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
            SET profile_json = ?, analyzed_hash = ?, analyzed_at = CURRENT_TIMESTAMP,
                profile_schema_version = COALESCE(profile_schema_version, 1)
            WHERE paper_key = ?
            "#,
            params![serde_json::to_string(profile)?, source_hash, paper_key],
        )?;
        self.save_profile_facts(paper_key, profile, 1)?;
        Ok(())
    }

    pub fn save_paper_facts(&self, paper_key: &str, facts: &[Value]) -> Result<()> {
        for fact in facts {
            self.conn.execute(
                r#"
                INSERT INTO paper_facts
                    (paper_key, chunk_id, section, fact_type, fact_json, profile_schema_version)
                VALUES (?, ?, ?, ?, ?, ?)
                "#,
                params![
                    paper_key,
                    fact.get("chunk_id").and_then(Value::as_i64),
                    fact.get("section")
                        .and_then(Value::as_str)
                        .unwrap_or_default(),
                    fact.get("fact_type")
                        .and_then(Value::as_str)
                        .unwrap_or("unknown"),
                    serde_json::to_string(fact)?,
                    1i64,
                ],
            )?;
        }
        Ok(())
    }

    fn save_profile_facts(
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
                    self.conn.execute(
                        r#"
                        INSERT INTO paper_facts
                            (paper_key, chunk_id, section, fact_type, fact_json, profile_schema_version)
                        VALUES (?, ?, ?, ?, ?, ?)
                        "#,
                        params![
                            paper_key,
                            chunk_id.as_i64(),
                            section,
                            fact_type,
                            serde_json::to_string(&fact_json)?,
                            profile_schema_version,
                        ],
                    )?;
                }
            }
        }
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
            let score = weighted_profile_score(&profile, &terms);
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
                       p.title, p.doi, p.year
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
                   p.title, p.doi, p.year
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
        let mut ranked_lists = Vec::new();
        if let Ok(rows) = self.search_chunks_fts(author, &match_query, 30) {
            if !rows.is_empty() {
                ranked_lists.push(rows);
            }
        }
        let like_rows = self.search_chunks_like(author, &terms, 30)?;
        if !like_rows.is_empty() {
            ranked_lists.push(like_rows);
        }
        let embedding_rows = self.search_chunks_embedding(author, query, 30)?;
        if !embedding_rows.is_empty() {
            ranked_lists.push(embedding_rows);
        }
        let profiles = self.search_profiles(author, query, 20)?;
        let paper_keys = profiles
            .iter()
            .filter_map(|profile| profile.get("paper_key").and_then(Value::as_str))
            .map(str::to_string)
            .collect::<Vec<_>>();
        let profile_rows = self.chunks_for_paper_keys(&paper_keys, 20)?;
        if !profile_rows.is_empty() {
            ranked_lists.push(profile_rows);
        }
        if ranked_lists.is_empty() {
            Ok(Vec::new())
        } else {
            Ok(rrf_merge_chunks(ranked_lists, limit))
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

    pub fn library_status(&self, author: Option<&str>) -> Result<LibraryStatus> {
        let papers = self.count_papers(author)?;
        let analyzed = if let Some(author) = author {
            self.conn.query_row(
                "SELECT COUNT(*) FROM papers WHERE author = ? AND profile_json IS NOT NULL",
                params![author],
                |row| row.get(0),
            )?
        } else {
            self.conn.query_row(
                "SELECT COUNT(*) FROM papers WHERE profile_json IS NOT NULL",
                [],
                |row| row.get(0),
            )?
        };
        let failed_jobs = if let Some(author) = author {
            self.conn.query_row(
                r#"
                SELECT COUNT(*)
                FROM analysis_jobs j
                LEFT JOIN papers p ON p.paper_key = j.paper_key
                WHERE j.status = 'failed' AND (p.author = ? OR j.paper_key IS NULL)
                "#,
                params![author],
                |row| row.get(0),
            )?
        } else {
            self.conn.query_row(
                "SELECT COUNT(*) FROM analysis_jobs WHERE status = 'failed'",
                [],
                |row| row.get(0),
            )?
        };
        Ok(LibraryStatus {
            papers,
            analyzed,
            failed_jobs,
        })
    }

    pub fn record_analysis_job(
        &self,
        paper_key: &str,
        job_type: &str,
        status: &str,
        error: Option<&str>,
    ) -> Result<()> {
        self.conn.execute(
            r#"
            INSERT INTO analysis_jobs (paper_key, job_type, status, error, updated_at)
            VALUES (?, ?, ?, ?, CURRENT_TIMESTAMP)
            "#,
            params![paper_key, job_type, status, error],
        )?;
        Ok(())
    }

    pub fn save_qa_log(
        &self,
        author: &str,
        question: &str,
        retrieval: &Value,
        answer: &Value,
        model: &str,
        latency_ms: i64,
    ) -> Result<()> {
        self.conn.execute(
            r#"
            INSERT INTO qa_logs (author, question, retrieval_json, answer_json, model, latency_ms)
            VALUES (?, ?, ?, ?, ?, ?)
            "#,
            params![
                author,
                question,
                serde_json::to_string(retrieval)?,
                serde_json::to_string(answer)?,
                model,
                latency_ms,
            ],
        )?;
        Ok(())
    }

    pub fn latest_qa_answer(&self, author: Option<&str>) -> Result<Option<Value>> {
        let answer: Option<String> = if let Some(author) = author {
            self.conn
                .query_row(
                    "SELECT answer_json FROM qa_logs WHERE author = ? ORDER BY id DESC LIMIT 1",
                    params![author],
                    |row| row.get(0),
                )
                .optional()?
        } else {
            self.conn
                .query_row(
                    "SELECT answer_json FROM qa_logs ORDER BY id DESC LIMIT 1",
                    [],
                    |row| row.get(0),
                )
                .optional()?
        };
        answer
            .map(|text| serde_json::from_str(&text).map_err(|err| anyhow!(err)))
            .transpose()
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

    fn search_chunks_embedding(
        &self,
        author: &str,
        query: &str,
        limit: usize,
    ) -> Result<Vec<SourceChunk>> {
        let query_vector = local_embedding(query);
        let mut stmt = self.conn.prepare(
            r#"
            SELECT c.id, c.paper_key, c.chunk_index, c.section, c.text,
                   p.title, p.doi, p.year, e.vector
            FROM embeddings e
            JOIN chunks c ON c.id = CAST(e.target_id AS INTEGER)
            JOIN papers p ON p.paper_key = c.paper_key
            WHERE e.target_type = 'chunk' AND e.model = ? AND p.author = ?
            "#,
        )?;
        let rows = stmt.query_map(params![LOCAL_EMBEDDING_MODEL, author], |row| {
            let chunk = SourceChunk {
                id: row.get(0)?,
                paper_key: row.get(1)?,
                chunk_index: row.get(2)?,
                section: row.get(3)?,
                text: row.get(4)?,
                title: row.get(5)?,
                doi: row.get(6)?,
                year: row.get(7)?,
            };
            let vector: Vec<u8> = row.get(8)?;
            Ok((chunk, vector))
        })?;
        let mut scored = Vec::new();
        for row in rows {
            let (chunk, vector) = row?;
            if let Some(vector) = decode_f32_vector(&vector) {
                let score = cosine_similarity(&query_vector, &vector);
                if score > 0.0 {
                    scored.push((score, chunk));
                }
            }
        }
        scored.sort_by(|left, right| {
            right
                .0
                .partial_cmp(&left.0)
                .unwrap_or(std::cmp::Ordering::Equal)
                .then_with(|| left.1.id.cmp(&right.1.id))
        });
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

fn weighted_profile_score(profile: &Value, terms: &[String]) -> usize {
    [
        ("doi", 10usize),
        ("title", 5),
        ("topic_keywords", 4),
        ("key_results", 4),
        ("methods", 3),
        ("limitations", 3),
        ("one_sentence_summary", 2),
    ]
    .iter()
    .map(|(field, weight)| {
        profile
            .get(*field)
            .map(|value| value_match_count(value, terms) * weight)
            .unwrap_or(0)
    })
    .sum::<usize>()
        + value_match_count(profile, terms)
}

fn value_match_count(value: &Value, terms: &[String]) -> usize {
    let text = value_text(value).to_lowercase();
    terms
        .iter()
        .map(|term| text.matches(&term.to_lowercase()).count())
        .sum()
}

fn value_text(value: &Value) -> String {
    match value {
        Value::String(text) => text.clone(),
        Value::Array(items) => items.iter().map(value_text).collect::<Vec<_>>().join(" "),
        Value::Object(object) => object
            .values()
            .map(value_text)
            .collect::<Vec<_>>()
            .join(" "),
        _ => value.to_string(),
    }
}

fn rrf_merge_chunks(ranked_lists: Vec<Vec<SourceChunk>>, limit: usize) -> Vec<SourceChunk> {
    const RRF_K: f64 = 60.0;
    let mut scores: HashMap<i64, f64> = HashMap::new();
    let mut first_seen: HashMap<i64, SourceChunk> = HashMap::new();
    for list in ranked_lists {
        let mut seen_in_list = HashSet::new();
        for (rank, chunk) in list.into_iter().enumerate() {
            if !seen_in_list.insert(chunk.id) {
                continue;
            }
            first_seen.entry(chunk.id).or_insert_with(|| chunk.clone());
            *scores.entry(chunk.id).or_default() += 1.0 / (RRF_K + rank as f64 + 1.0);
        }
    }
    let mut scored = scores.into_iter().collect::<Vec<_>>();
    scored.sort_by(|left, right| {
        right
            .1
            .partial_cmp(&left.1)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| left.0.cmp(&right.0))
    });
    scored
        .into_iter()
        .take(limit)
        .filter_map(|(id, _)| first_seen.remove(&id))
        .collect()
}

fn local_embedding(text: &str) -> Vec<f32> {
    let terms = query_terms(text);
    let mut vector = vec![0.0; LOCAL_EMBEDDING_DIM];
    for term in terms {
        let mut hash = 1469598103934665603u64;
        for byte in term.to_lowercase().as_bytes() {
            hash ^= *byte as u64;
            hash = hash.wrapping_mul(1099511628211);
        }
        let index = (hash as usize) % LOCAL_EMBEDDING_DIM;
        vector[index] += 1.0;
    }
    normalize_vector(&mut vector);
    vector
}

fn normalize_vector(vector: &mut [f32]) {
    let norm = vector
        .iter()
        .map(|value| (*value as f64) * (*value as f64))
        .sum::<f64>()
        .sqrt() as f32;
    if norm > 0.0 {
        for value in vector {
            *value /= norm;
        }
    }
}

fn cosine_similarity(left: &[f32], right: &[f32]) -> f32 {
    left.iter()
        .zip(right.iter())
        .map(|(left, right)| left * right)
        .sum()
}

fn encode_f32_vector(vector: &[f32]) -> Vec<u8> {
    let mut bytes = Vec::with_capacity(std::mem::size_of_val(vector));
    for value in vector {
        bytes.extend_from_slice(&value.to_le_bytes());
    }
    bytes
}

fn decode_f32_vector(bytes: &[u8]) -> Option<Vec<f32>> {
    if !bytes.len().is_multiple_of(std::mem::size_of::<f32>()) {
        return None;
    }
    bytes
        .chunks_exact(std::mem::size_of::<f32>())
        .map(|chunk| Some(f32::from_le_bytes(chunk.try_into().ok()?)))
        .collect()
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
    enrich_query_terms(&mut terms, query);
    terms
}

fn enrich_query_terms(terms: &mut Vec<String>, query: &str) {
    for token in query.split_whitespace() {
        let normalized = token.trim_matches(|ch: char| {
            !ch.is_ascii_alphanumeric() && !matches!(ch, '.' | '/' | '-' | '_')
        });
        if normalized.starts_with("10.") && normalized.contains('/') {
            push_unique_term(terms, normalized);
        }
        if normalized.chars().any(|ch| ch.is_ascii_digit()) {
            push_unique_term(terms, normalized);
        }
    }

    let quoted = query
        .split('"')
        .skip(1)
        .step_by(2)
        .map(str::trim)
        .filter(|item| item.chars().count() > 2)
        .collect::<Vec<_>>();
    for phrase in quoted {
        push_unique_term(terms, phrase);
    }

    let english_phrase = query
        .split(|ch: char| !ch.is_ascii_alphanumeric() && ch != '-' && ch != ' ')
        .map(str::trim)
        .filter(|item| item.split_whitespace().count() >= 2)
        .collect::<Vec<_>>();
    for phrase in english_phrase {
        if phrase.chars().count() <= 80 {
            push_unique_term(terms, phrase);
        }
    }
}

fn push_term(terms: &mut Vec<String>, current: &mut String) {
    let term = current.trim();
    if !term.is_empty()
        && (term.chars().count() > 1 || term.chars().all(|ch| ch.is_ascii_alphanumeric()))
    {
        push_unique_term(terms, term);
    }
    current.clear();
}

fn push_unique_term(terms: &mut Vec<String>, term: &str) {
    let term = term.trim();
    if !term.is_empty()
        && !terms
            .iter()
            .any(|existing| existing.eq_ignore_ascii_case(term))
    {
        terms.push(term.to_string());
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;
    use tempfile::tempdir;

    use crate::papers::models::Paper;
    use crate::retrieval::chunker::chunk_paper;
    use rusqlite::Connection;

    use super::{
        SourceChunk, Storage, decode_f32_vector, encode_f32_vector, local_embedding, query_terms,
        rrf_merge_chunks, weighted_profile_score,
    };

    #[test]
    fn weighted_profile_score_prioritizes_structured_fields() {
        let terms = query_terms("mof");
        let title_match = json!({ "title": "MOF catalyst" });
        let raw_match = json!({ "notes": "MOF catalyst" });

        assert!(
            weighted_profile_score(&title_match, &terms)
                > weighted_profile_score(&raw_match, &terms)
        );
    }

    #[test]
    fn query_terms_rewrite_keeps_doi_numbers_and_phrases() {
        let terms = query_terms("Compare \"MOF catalyst\" 82% DOI 10.1000/paper-a");
        assert!(terms.iter().any(|term| term == "10.1000/paper-a"));
        assert!(terms.iter().any(|term| term.contains("82")));
        assert!(terms.iter().any(|term| term == "MOF catalyst"));
    }

    #[test]
    fn initializes_migrations_and_records_qa_logs() {
        let dir = tempdir().unwrap();
        let storage = Storage::open(&dir.path().join("test.sqlite")).unwrap();
        let status = storage.library_status(None).unwrap();
        assert_eq!(status.papers, 0);

        let answer = json!({
            "answer": "ok",
            "evidence": [{
                "paper_key": "Alice/paper-a",
                "title": "A Paper",
                "doi": "10.1/test",
                "year": "2024",
                "chunk_id": 1,
                "section": "Results",
                "quote_or_summary": "evidence"
            }]
        });
        storage
            .save_qa_log(
                "Alice",
                "question",
                &json!({ "chunks": [] }),
                &answer,
                "test-model",
                12,
            )
            .unwrap();

        let latest = storage.latest_qa_answer(Some("Alice")).unwrap().unwrap();
        assert_eq!(latest["answer"], "ok");
        assert_eq!(latest["evidence"][0]["chunk_id"], 1);
    }

    #[test]
    fn opens_legacy_database_and_applies_migrations() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("legacy.sqlite");
        {
            let conn = Connection::open(&path).unwrap();
            conn.execute_batch(
                r#"
                CREATE TABLE papers (
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
                CREATE TABLE chunks (
                    id INTEGER PRIMARY KEY AUTOINCREMENT,
                    paper_key TEXT NOT NULL,
                    chunk_index INTEGER NOT NULL,
                    section TEXT NOT NULL,
                    text TEXT NOT NULL
                );
                CREATE TABLE author_profiles (
                    author TEXT PRIMARY KEY,
                    profile_json TEXT NOT NULL,
                    updated_at TEXT DEFAULT CURRENT_TIMESTAMP
                );
                "#,
            )
            .unwrap();
        }

        let storage = Storage::open(&path).unwrap();
        assert!(
            storage
                .column_exists("papers", "profile_schema_version")
                .unwrap()
        );
        let migration_count: i64 = storage
            .conn
            .query_row("SELECT COUNT(*) FROM schema_migrations", [], |row| {
                row.get(0)
            })
            .unwrap();
        assert_eq!(migration_count, 1);
    }

    #[test]
    fn records_analysis_job_status_history() {
        let dir = tempdir().unwrap();
        let storage = Storage::open(&dir.path().join("test.sqlite")).unwrap();
        storage
            .record_analysis_job("Alice/paper-a", "analyze", "running", None)
            .unwrap();
        storage
            .record_analysis_job("Alice/paper-a", "analyze", "succeeded", None)
            .unwrap();

        let statuses = {
            let mut stmt = storage
                .conn
                .prepare("SELECT status FROM analysis_jobs ORDER BY id")
                .unwrap();
            stmt.query_map([], |row| row.get::<_, String>(0))
                .unwrap()
                .collect::<rusqlite::Result<Vec<_>>>()
                .unwrap()
        };

        assert_eq!(statuses, vec!["running", "succeeded"]);
    }

    #[test]
    fn save_paper_profile_extracts_claim_facts() {
        let dir = tempdir().unwrap();
        let storage = Storage::open(&dir.path().join("test.sqlite")).unwrap();
        storage
            .conn
            .execute(
                r#"
                INSERT INTO papers (
                    paper_key, author, paper_id, title, doi, year, source_hash,
                    article_path, metadata_json, fetch_result_json
                )
                VALUES ('Alice/paper-a', 'Alice', 'paper-a', 'A Paper', '10.1/test',
                        '2024', 'hash', 'article.md', '{}', '{}')
                "#,
                [],
            )
            .unwrap();
        storage
            .save_paper_profile(
                "Alice/paper-a",
                "hash",
                &json!({
                    "methods": [{"method": "tested catalysts", "evidence_chunks": [1], "section": "Methods"}],
                    "key_results": [{"claim": "improved conversion", "evidence_chunks": [2], "section": "Results"}],
                    "limitations": [{"limitation": "small sample", "evidence_chunks": [3], "section": "Discussion"}]
                }),
            )
            .unwrap();

        let count: i64 = storage
            .conn
            .query_row("SELECT COUNT(*) FROM paper_facts", [], |row| row.get(0))
            .unwrap();
        assert_eq!(count, 3);
    }

    #[test]
    fn save_paper_facts_persists_section_facts() {
        let dir = tempdir().unwrap();
        let storage = Storage::open(&dir.path().join("test.sqlite")).unwrap();
        storage
            .save_paper_facts(
                "Alice/paper-a",
                &[json!({
                    "chunk_id": 1,
                    "section": "Methods",
                    "fact_type": "method",
                    "text": "method fact"
                })],
            )
            .unwrap();

        let fact_type: String = storage
            .conn
            .query_row("SELECT fact_type FROM paper_facts LIMIT 1", [], |row| {
                row.get(0)
            })
            .unwrap();
        assert_eq!(fact_type, "method");
    }

    #[test]
    fn rrf_merge_promotes_chunks_seen_by_multiple_rankers() {
        let chunk = |id| SourceChunk {
            id,
            paper_key: format!("Alice/paper-{id}"),
            chunk_index: 0,
            section: "Results".to_string(),
            text: format!("chunk {id}"),
            title: format!("Paper {id}"),
            doi: String::new(),
            year: "2024".to_string(),
        };
        let merged = rrf_merge_chunks(
            vec![
                vec![chunk(1), chunk(2), chunk(3)],
                vec![chunk(2), chunk(4), chunk(5)],
            ],
            3,
        );

        assert_eq!(merged[0].id, 2);
        assert_eq!(merged.len(), 3);
    }

    #[test]
    fn local_embedding_round_trips_through_blob() {
        let vector = local_embedding("MOF catalyst conversion");
        let decoded = decode_f32_vector(&encode_f32_vector(&vector)).unwrap();
        assert_eq!(decoded.len(), vector.len());
        assert!((decoded[0] - vector[0]).abs() < f32::EPSILON);
    }

    #[test]
    fn upsert_paper_indexes_chunk_embeddings_for_hybrid_search() {
        let dir = tempdir().unwrap();
        let mut storage = Storage::open(&dir.path().join("test.sqlite")).unwrap();
        let paper = Paper {
            author: "Alice".to_string(),
            paper_id: "paper-a".to_string(),
            paper_dir: dir.path().join("paper-a"),
            article_path: dir.path().join("paper-a/article.md"),
            fetch_result_path: None,
            source_hash: "hash".to_string(),
            metadata: std::collections::BTreeMap::from([
                ("title".to_string(), "Catalyst paper".to_string()),
                ("doi".to_string(), "10.1/test".to_string()),
                ("year".to_string(), "2024".to_string()),
            ]),
            fetch_result: json!({}),
            raw_body: String::new(),
            clean_text: "Zeolite improves conversion under mild conditions.".to_string(),
            sections: vec![],
        };
        let chunks = chunk_paper(&paper, 3200, 350);
        storage.upsert_paper(&paper, &chunks).unwrap();

        let count: i64 = storage
            .conn
            .query_row("SELECT COUNT(*) FROM embeddings", [], |row| row.get(0))
            .unwrap();
        assert_eq!(count, 1);
        let rows = storage
            .search_chunks_embedding("Alice", "Zeolite conversion", 5)
            .unwrap();
        assert_eq!(rows[0].paper_key, "Alice/paper-a");
    }
}
