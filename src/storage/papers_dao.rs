use anyhow::Result;
use rusqlite::{OptionalExtension, params};
use sha2::{Digest, Sha256};

use crate::papers::cleaner::CLEANER_VERSION;
use crate::papers::models::Paper;
use crate::papers::parser::PARSER_VERSION;
use crate::retrieval::chunker::Chunk;
use crate::retrieval::embedding::{
    LOCAL_HASH_EMBEDDING_DIM, LOCAL_HASH_EMBEDDING_MODEL, encode_f32_vector, local_hash_embedding,
};

use super::{AnalysisCandidate, LibraryStatus, Storage};

impl Storage {
    pub fn upsert_paper(&mut self, paper: &Paper, chunks: &[Chunk]) -> Result<bool> {
        self.upsert_paper_with_chunker(paper, chunks, "section-char-v1", 3200, 350)
    }

    pub fn upsert_paper_with_chunker(
        &mut self,
        paper: &Paper,
        chunks: &[Chunk],
        chunker_version: &str,
        max_chars: usize,
        overlap: usize,
    ) -> Result<bool> {
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
                article_path, fetch_result_path, parser_version, cleaner_version,
                metadata_json, fetch_result_json, updated_at
            )
            VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, CURRENT_TIMESTAMP)
            ON CONFLICT(paper_key) DO UPDATE SET
                title = excluded.title,
                doi = excluded.doi,
                year = excluded.year,
                source_hash = excluded.source_hash,
                article_path = excluded.article_path,
                fetch_result_path = excluded.fetch_result_path,
                parser_version = excluded.parser_version,
                cleaner_version = excluded.cleaner_version,
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
                PARSER_VERSION,
                CLEANER_VERSION,
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
                r#"
                DELETE FROM embeddings
                WHERE target_type = 'chunk'
                  AND target_id IN (
                    SELECT CAST(id AS TEXT) FROM chunks WHERE paper_key = ?
                  )
                "#,
                params![paper.key()],
            )?;
            tx.execute(
                "DELETE FROM paper_facts WHERE paper_key = ?",
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
                    r#"
                    INSERT INTO chunks (
                        paper_key, chunk_index, section, text, chunk_hash,
                        chunker_version, max_chars, overlap, source_hash, section_path,
                        section_kind, caption_label
                    )
                    VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
                    "#,
                    params![
                        chunk.paper_key,
                        chunk.chunk_index as i64,
                        chunk.section,
                        chunk.text,
                        content_hash(&chunk.text),
                        chunker_version,
                        max_chars as i64,
                        overlap as i64,
                        paper.source_hash,
                        chunk.section,
                        chunk.section_kind,
                        chunk.caption_label
                    ],
                )?;
                let chunk_id = tx.last_insert_rowid();
                let _ = tx.execute(
                    "INSERT INTO chunks_fts (text, paper_key, chunk_id, title, doi, author) VALUES (?, ?, ?, ?, ?, ?)",
                    params![chunk.text, paper.key(), chunk_id, paper.title(), paper.doi(), paper.author],
                );
                let vector = local_hash_embedding(&format!(
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
                        LOCAL_HASH_EMBEDDING_MODEL,
                        LOCAL_HASH_EMBEDDING_DIM as i64,
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
        profile_schema_version: i64,
        profile_prompt_version: &str,
        profile_model_id: &str,
        profile_chunker_version: &str,
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
            WHERE author = ? AND (
                profile_json IS NULL
                OR analyzed_hash IS NULL
                OR analyzed_hash != source_hash
                OR COALESCE(profile_schema_version, 0) != ?
                OR COALESCE(profile_prompt_version, '') != ?
                OR COALESCE(profile_model_id, '') != ?
                OR COALESCE(profile_chunker_version, '') != ?
                OR COALESCE(profile_status, '') != 'succeeded'
            )
            ORDER BY year DESC, paper_id DESC
            "#
        };
        let mut stmt = self.conn.prepare(sql)?;
        let rows = if force {
            stmt.query_map(params![author], analysis_candidate_from_row)?
        } else {
            stmt.query_map(
                params![
                    author,
                    profile_schema_version,
                    profile_prompt_version,
                    profile_model_id,
                    profile_chunker_version
                ],
                analysis_candidate_from_row,
            )?
        };
        rows.collect::<rusqlite::Result<Vec<_>>>()
            .map_err(Into::into)
    }

    pub fn failed_analysis_candidates(&self, author: &str) -> Result<Vec<AnalysisCandidate>> {
        let mut stmt = self.conn.prepare(
            r#"
            SELECT DISTINCT p.paper_key, p.author, p.paper_id, p.title, p.doi,
                   p.year, p.source_hash, p.article_path
            FROM papers p
            LEFT JOIN analysis_jobs j ON j.paper_key = p.paper_key
            WHERE p.author = ?
              AND (p.profile_status = 'failed' OR j.status = 'failed')
            ORDER BY p.year DESC, p.paper_id DESC
            "#,
        )?;
        let rows = stmt.query_map(params![author], analysis_candidate_from_row)?;
        rows.collect::<rusqlite::Result<Vec<_>>>()
            .map_err(Into::into)
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
        let stale_papers = self.count_stale_papers(author)?;
        let failed_jobs = self.count_analysis_jobs(author, "failed")?;
        let queued_jobs = self.count_analysis_jobs(author, "queued")?;
        let running_jobs = self.count_analysis_jobs(author, "running")?;
        let retry_waiting_jobs = self.count_analysis_jobs(author, "retry_waiting")?;
        let cancelled_jobs = self.count_analysis_jobs(author, "cancelled")?;
        let (qa_logs, avg_qa_latency_ms, total_qa_tokens, total_qa_cost_usd) =
            self.qa_log_stats(author)?;
        Ok(LibraryStatus {
            papers,
            analyzed,
            stale_papers,
            failed_jobs,
            queued_jobs,
            running_jobs,
            retry_waiting_jobs,
            cancelled_jobs,
            qa_logs,
            avg_qa_latency_ms,
            total_qa_tokens,
            total_qa_cost_usd,
        })
    }

    fn count_stale_papers(&self, author: Option<&str>) -> Result<i64> {
        let condition = r#"
            (
                profile_json IS NULL
                OR analyzed_hash IS NULL
                OR analyzed_hash != source_hash
                OR COALESCE(profile_status, '') != 'succeeded'
            )
        "#;
        if let Some(author) = author {
            self.conn
                .query_row(
                    &format!("SELECT COUNT(*) FROM papers WHERE author = ? AND {condition}"),
                    params![author],
                    |row| row.get(0),
                )
                .map_err(Into::into)
        } else {
            self.conn
                .query_row(
                    &format!("SELECT COUNT(*) FROM papers WHERE {condition}"),
                    [],
                    |row| row.get(0),
                )
                .map_err(Into::into)
        }
    }
}

fn analysis_candidate_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<AnalysisCandidate> {
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
}

fn content_hash(text: &str) -> String {
    let mut digest = Sha256::new();
    digest.update(text.as_bytes());
    format!("{:x}", digest.finalize())
}
