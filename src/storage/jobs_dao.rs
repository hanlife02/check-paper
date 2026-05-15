use anyhow::Result;
use chrono::{Duration, Utc};
use rusqlite::{OptionalExtension, params};

use super::{
    AnalysisCandidate, AnalysisJobMetadata, AnalysisJobSummary, AnalysisJobTask, Storage,
    sqlite_timestamp,
};

impl Storage {
    pub(super) fn count_analysis_jobs(&self, author: Option<&str>, status: &str) -> Result<i64> {
        if let Some(author) = author {
            self.conn
                .query_row(
                    r#"
                    SELECT COUNT(*)
                    FROM analysis_jobs j
                    LEFT JOIN papers p ON p.paper_key = j.paper_key
                    WHERE j.status = ? AND (p.author = ? OR j.paper_key LIKE ?)
                    "#,
                    params![status, author, format!("{author}/%")],
                    |row| row.get(0),
                )
                .map_err(Into::into)
        } else {
            self.conn
                .query_row(
                    "SELECT COUNT(*) FROM analysis_jobs WHERE status = ?",
                    params![status],
                    |row| row.get(0),
                )
                .map_err(Into::into)
        }
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

    pub fn record_analysis_job_with_metadata(
        &self,
        row: &AnalysisCandidate,
        metadata: AnalysisJobMetadata<'_>,
    ) -> Result<()> {
        self.conn.execute(
            r#"
            INSERT INTO analysis_jobs (
                paper_key, job_type, status, error, source_hash, profile_schema_version,
                prompt_version, model_id, last_error_code, attempt_count, updated_at
            )
            VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, 0, CURRENT_TIMESTAMP)
            "#,
            params![
                row.paper_key,
                metadata.job_type,
                metadata.status,
                metadata.error,
                row.source_hash,
                metadata.profile_schema_version,
                metadata.prompt_version,
                metadata.model_id,
                metadata.error_code
            ],
        )?;
        if metadata.status == "failed" {
            self.conn.execute(
                r#"
                UPDATE papers
                SET profile_status = 'failed', profile_error_code = ?
                WHERE paper_key = ?
                "#,
                params![metadata.error_code, row.paper_key],
            )?;
        }
        Ok(())
    }

    pub fn enqueue_analysis_jobs(
        &self,
        rows: &[AnalysisCandidate],
        job_type: &str,
        profile_schema_version: i64,
        prompt_version: &str,
        model_id: &str,
        max_attempts: i64,
    ) -> Result<usize> {
        let mut count = 0usize;
        for row in rows {
            let active: Option<i64> = self
                .conn
                .query_row(
                    r#"
                    SELECT id
                    FROM analysis_jobs
                    WHERE paper_key = ?
                      AND job_type = ?
                      AND status IN ('queued', 'running', 'retry_waiting')
                      AND source_hash = ?
                      AND COALESCE(profile_schema_version, 0) = ?
                      AND COALESCE(prompt_version, '') = ?
                      AND COALESCE(model_id, '') = ?
                    LIMIT 1
                    "#,
                    params![
                        row.paper_key,
                        job_type,
                        row.source_hash,
                        profile_schema_version,
                        prompt_version,
                        model_id
                    ],
                    |row| row.get(0),
                )
                .optional()?;
            if active.is_some() {
                continue;
            }
            self.conn.execute(
                r#"
                INSERT INTO analysis_jobs (
                    paper_key, job_type, status, source_hash, profile_schema_version,
                    prompt_version, model_id, attempt_count, max_attempts, updated_at
                )
                VALUES (?, ?, 'queued', ?, ?, ?, ?, 0, ?, CURRENT_TIMESTAMP)
                "#,
                params![
                    row.paper_key,
                    job_type,
                    row.source_hash,
                    profile_schema_version,
                    prompt_version,
                    model_id,
                    max_attempts,
                ],
            )?;
            self.conn.execute(
                r#"
                UPDATE papers
                SET profile_status = 'queued', profile_error_code = NULL
                WHERE paper_key = ?
                "#,
                params![row.paper_key],
            )?;
            count += 1;
        }
        Ok(count)
    }

    pub fn claim_next_analysis_job(
        &self,
        author: &str,
        job_type: &str,
        worker_id: &str,
        lock_seconds: i64,
    ) -> Result<Option<AnalysisJobTask>> {
        let task = {
            let mut stmt = self.conn.prepare(
                r#"
                SELECT j.id, p.paper_key, p.author, p.paper_id, p.title, p.doi,
                       p.year, p.source_hash, p.article_path,
                       j.attempt_count, j.max_attempts
                FROM analysis_jobs j
                JOIN papers p ON p.paper_key = j.paper_key
                WHERE p.author = ?
                  AND j.job_type = ?
                  AND (
                    j.status IN ('queued', 'retry_waiting')
                    OR (j.status = 'running' AND COALESCE(j.lock_until, '') <= CURRENT_TIMESTAMP)
                  )
                  AND (j.next_retry_at IS NULL OR j.next_retry_at <= CURRENT_TIMESTAMP)
                ORDER BY j.id ASC
                LIMIT 1
                "#,
            )?;
            stmt.query_row(params![author, job_type], |row| {
                Ok(AnalysisJobTask {
                    id: row.get(0)?,
                    candidate: AnalysisCandidate {
                        paper_key: row.get(1)?,
                        author: row.get(2)?,
                        paper_id: row.get(3)?,
                        title: row.get(4)?,
                        doi: row.get(5)?,
                        year: row.get(6)?,
                        source_hash: row.get(7)?,
                        article_path: row.get(8)?,
                    },
                    attempt_count: row.get(9)?,
                    max_attempts: row.get(10)?,
                })
            })
            .optional()?
        };
        let Some(task) = task else {
            return Ok(None);
        };
        let lock_until = sqlite_timestamp(Utc::now() + Duration::seconds(lock_seconds));
        self.conn.execute(
            r#"
            UPDATE analysis_jobs
            SET status = 'running',
                locked_by = ?,
                lock_until = ?,
                attempt_count = attempt_count + 1,
                updated_at = CURRENT_TIMESTAMP
            WHERE id = ?
            "#,
            params![worker_id, lock_until, task.id],
        )?;
        self.conn.execute(
            r#"
            UPDATE papers
            SET profile_status = 'running', profile_error_code = NULL
            WHERE paper_key = ?
            "#,
            params![task.candidate.paper_key],
        )?;
        Ok(Some(task))
    }

    pub fn complete_analysis_job(&self, job_id: i64, paper_key: &str) -> Result<()> {
        self.conn.execute(
            r#"
            UPDATE analysis_jobs
            SET status = 'succeeded',
                last_error_code = NULL,
                error = NULL,
                next_retry_at = NULL,
                locked_by = NULL,
                lock_until = NULL,
                updated_at = CURRENT_TIMESTAMP
            WHERE id = ?
            "#,
            params![job_id],
        )?;
        self.conn.execute(
            r#"
            UPDATE papers
            SET profile_status = 'succeeded', profile_error_code = NULL
            WHERE paper_key = ?
            "#,
            params![paper_key],
        )?;
        Ok(())
    }

    pub fn fail_analysis_job(
        &self,
        job_id: i64,
        paper_key: &str,
        error_code: &str,
        error: &str,
    ) -> Result<String> {
        let (attempt_count, max_attempts): (i64, i64) = self.conn.query_row(
            "SELECT attempt_count, max_attempts FROM analysis_jobs WHERE id = ?",
            params![job_id],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )?;
        let (status, next_retry_at) = if attempt_count < max_attempts {
            (
                "retry_waiting",
                Some(sqlite_timestamp(Utc::now() + Duration::seconds(60))),
            )
        } else {
            ("failed", None)
        };
        self.conn.execute(
            r#"
            UPDATE analysis_jobs
            SET status = ?,
                last_error_code = ?,
                error = ?,
                next_retry_at = ?,
                locked_by = NULL,
                lock_until = NULL,
                updated_at = CURRENT_TIMESTAMP
            WHERE id = ?
            "#,
            params![status, error_code, error, next_retry_at, job_id],
        )?;
        self.conn.execute(
            r#"
            UPDATE papers
            SET profile_status = ?, profile_error_code = ?
            WHERE paper_key = ?
            "#,
            params![status, error_code, paper_key],
        )?;
        Ok(status.to_string())
    }

    pub fn analysis_jobs(
        &self,
        author: Option<&str>,
        status: Option<&str>,
        limit: usize,
    ) -> Result<Vec<AnalysisJobSummary>> {
        let mut sql = r#"
            SELECT j.id, j.paper_key, j.job_type, j.status, j.last_error_code,
                   j.error, j.model_id, j.updated_at
            FROM analysis_jobs j
            LEFT JOIN papers p ON p.paper_key = j.paper_key
            WHERE 1 = 1
        "#
        .to_string();
        let mut values = Vec::new();
        if let Some(author) = author {
            sql.push_str(" AND (p.author = ? OR j.paper_key LIKE ?)");
            values.push(author.to_string());
            values.push(format!("{author}/%"));
        }
        if let Some(status) = status {
            sql.push_str(" AND j.status = ?");
            values.push(status.to_string());
        }
        sql.push_str(" ORDER BY j.id DESC LIMIT ?");
        values.push(limit.to_string());
        let params = rusqlite::params_from_iter(values.iter());
        let mut stmt = self.conn.prepare(&sql)?;
        let rows = stmt.query_map(params, |row| {
            Ok(AnalysisJobSummary {
                id: row.get(0)?,
                paper_key: row.get(1)?,
                job_type: row.get(2)?,
                status: row.get(3)?,
                error_code: row.get(4)?,
                error: row.get(5)?,
                model_id: row.get(6)?,
                updated_at: row.get(7)?,
            })
        })?;
        rows.collect::<rusqlite::Result<Vec<_>>>()
            .map_err(Into::into)
    }

    pub fn analysis_job_error_counts(
        &self,
        author: Option<&str>,
        status: Option<&str>,
    ) -> Result<Vec<(String, i64)>> {
        let mut sql = r#"
            SELECT j.last_error_code, COUNT(*)
            FROM analysis_jobs j
            LEFT JOIN papers p ON p.paper_key = j.paper_key
            WHERE j.last_error_code IS NOT NULL AND j.last_error_code != ''
        "#
        .to_string();
        let mut values = Vec::new();
        if let Some(author) = author {
            sql.push_str(" AND (p.author = ? OR j.paper_key LIKE ?)");
            values.push(author.to_string());
            values.push(format!("{author}/%"));
        }
        if let Some(status) = status {
            sql.push_str(" AND j.status = ?");
            values.push(status.to_string());
        }
        sql.push_str(" GROUP BY j.last_error_code ORDER BY COUNT(*) DESC, j.last_error_code ASC");
        let params = rusqlite::params_from_iter(values.iter());
        let mut stmt = self.conn.prepare(&sql)?;
        let rows = stmt.query_map(params, |row| Ok((row.get(0)?, row.get(1)?)))?;
        rows.collect::<rusqlite::Result<Vec<_>>>()
            .map_err(Into::into)
    }

    pub fn cancel_analysis_job(&self, job_id: i64) -> Result<()> {
        let paper_key: Option<String> = self
            .conn
            .query_row(
                "SELECT paper_key FROM analysis_jobs WHERE id = ?",
                params![job_id],
                |row| row.get(0),
            )
            .optional()?
            .flatten();
        self.conn.execute(
            r#"
            UPDATE analysis_jobs
            SET status = 'cancelled',
                locked_by = NULL,
                lock_until = NULL,
                updated_at = CURRENT_TIMESTAMP
            WHERE id = ?
            "#,
            params![job_id],
        )?;
        if let Some(paper_key) = paper_key {
            self.conn.execute(
                r#"
                UPDATE papers
                SET profile_status = 'cancelled'
                WHERE paper_key = ?
                "#,
                params![paper_key],
            )?;
        }
        Ok(())
    }

    pub fn retry_failed_analysis_jobs(&self, author: Option<&str>) -> Result<usize> {
        let failed = self.analysis_jobs(author, Some("failed"), 1_000_000)?;
        let mut count = 0usize;
        for job in failed {
            self.conn.execute(
                r#"
                UPDATE analysis_jobs
                SET status = 'queued',
                    attempt_count = 0,
                    last_error_code = NULL,
                    error = NULL,
                    next_retry_at = NULL,
                    locked_by = NULL,
                    lock_until = NULL,
                    updated_at = CURRENT_TIMESTAMP
                WHERE id = ?
                "#,
                params![job.id],
            )?;
            if let Some(paper_key) = job.paper_key.as_deref() {
                self.conn.execute(
                    r#"
                    UPDATE papers
                    SET profile_status = 'queued', profile_error_code = NULL
                    WHERE paper_key = ?
                    "#,
                    params![paper_key],
                )?;
            }
            count += 1;
        }
        Ok(count)
    }

    pub fn retry_failed_analysis_jobs_for_candidates(
        &self,
        rows: &[AnalysisCandidate],
        job_type: &str,
        profile_schema_version: i64,
        prompt_version: &str,
        model_id: &str,
        max_attempts: i64,
    ) -> Result<usize> {
        let mut count = 0usize;
        for row in rows {
            let job_id: Option<i64> = self
                .conn
                .query_row(
                    r#"
                    SELECT id
                    FROM analysis_jobs
                    WHERE paper_key = ? AND job_type = ? AND status = 'failed'
                    ORDER BY id DESC
                    LIMIT 1
                    "#,
                    params![row.paper_key, job_type],
                    |row| row.get(0),
                )
                .optional()?;
            let Some(job_id) = job_id else {
                continue;
            };
            self.conn.execute(
                r#"
                UPDATE analysis_jobs
                SET status = 'queued',
                    source_hash = ?,
                    profile_schema_version = ?,
                    prompt_version = ?,
                    model_id = ?,
                    attempt_count = 0,
                    max_attempts = ?,
                    last_error_code = NULL,
                    error = NULL,
                    next_retry_at = NULL,
                    locked_by = NULL,
                    lock_until = NULL,
                    updated_at = CURRENT_TIMESTAMP
                WHERE id = ?
                "#,
                params![
                    row.source_hash,
                    profile_schema_version,
                    prompt_version,
                    model_id,
                    max_attempts,
                    job_id
                ],
            )?;
            self.conn.execute(
                r#"
                UPDATE papers
                SET profile_status = 'queued', profile_error_code = NULL
                WHERE paper_key = ?
                "#,
                params![row.paper_key],
            )?;
            count += 1;
        }
        Ok(count)
    }

    pub fn retry_failed_analysis_jobs_for_paper_keys(
        &self,
        paper_keys: &[String],
    ) -> Result<usize> {
        let mut count = 0usize;
        for paper_key in paper_keys {
            let job_id: Option<i64> = self
                .conn
                .query_row(
                    r#"
                    SELECT id
                    FROM analysis_jobs
                    WHERE paper_key = ? AND status = 'failed'
                    ORDER BY id DESC
                    LIMIT 1
                    "#,
                    params![paper_key],
                    |row| row.get(0),
                )
                .optional()?;
            let Some(job_id) = job_id else {
                continue;
            };
            self.conn.execute(
                r#"
                UPDATE analysis_jobs
                SET status = 'queued',
                    attempt_count = 0,
                    last_error_code = NULL,
                    error = NULL,
                    next_retry_at = NULL,
                    locked_by = NULL,
                    lock_until = NULL,
                    updated_at = CURRENT_TIMESTAMP
                WHERE id = ?
                "#,
                params![job_id],
            )?;
            self.conn.execute(
                r#"
                UPDATE papers
                SET profile_status = 'queued', profile_error_code = NULL
                WHERE paper_key = ?
                "#,
                params![paper_key],
            )?;
            count += 1;
        }
        Ok(count)
    }
}
