use anyhow::{Result, anyhow};
use rusqlite::{OptionalExtension, params};
use serde_json::Value;

use super::{QaLogEntry, QaLogStats, QaLogSummary, QaLogTrend, Storage};

impl Storage {
    pub(super) fn qa_log_stats(&self, author: Option<&str>) -> Result<QaLogStats> {
        if let Some(author) = author {
            self.conn
                .query_row(
                    "SELECT COUNT(*), AVG(latency_ms), SUM(total_tokens), SUM(cost_usd) FROM qa_logs WHERE author = ?",
                    params![author],
                    |row| {
                        Ok(QaLogStats {
                            count: row.get(0)?,
                            avg_latency_ms: row.get(1)?,
                            total_tokens: row.get(2)?,
                            total_cost_usd: row.get(3)?,
                        })
                    },
                )
                .map_err(Into::into)
        } else {
            self.conn
                .query_row(
                    "SELECT COUNT(*), AVG(latency_ms), SUM(total_tokens), SUM(cost_usd) FROM qa_logs",
                    [],
                    |row| {
                        Ok(QaLogStats {
                            count: row.get(0)?,
                            avg_latency_ms: row.get(1)?,
                            total_tokens: row.get(2)?,
                            total_cost_usd: row.get(3)?,
                        })
                    },
                )
                .map_err(Into::into)
        }
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
        self.save_qa_log_with_metadata(QaLogEntry {
            author,
            question,
            retrieval,
            answer,
            model,
            latency_ms,
            metadata: None,
        })
    }

    pub fn save_qa_log_with_metadata(&self, entry: QaLogEntry<'_>) -> Result<()> {
        let retrieval_trace_json = entry
            .retrieval
            .get("trace")
            .map(serde_json::to_string)
            .transpose()?;
        let metadata = entry.metadata.unwrap_or_default();
        self.conn.execute(
            r#"
            INSERT INTO qa_logs (
                author, question, retrieval_json, answer_json, model, latency_ms,
                retrieval_trace_json, answer_schema_version, qa_prompt_version,
                temperature, max_tokens, prompt_tokens, completion_tokens,
                total_tokens, cost_usd, error_code, qa_profile_version, qa_mode, route_reason,
                delivery_mode, streaming_finalized, stream_delta_count, streamed_chars,
                stream_first_delta_ms, stream_duration_ms, telegram_chat_id, telegram_job_id
            )
            VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
            "#,
            params![
                entry.author,
                entry.question,
                serde_json::to_string(entry.retrieval)?,
                serde_json::to_string(entry.answer)?,
                entry.model,
                entry.latency_ms,
                retrieval_trace_json,
                metadata.answer_schema_version,
                metadata.qa_prompt_version,
                metadata.temperature,
                metadata.max_tokens,
                metadata.prompt_tokens,
                metadata.completion_tokens,
                metadata.total_tokens,
                metadata.cost_usd,
                metadata.error_code,
                metadata.qa_profile_version,
                metadata.qa_mode,
                metadata.route_reason,
                metadata.delivery_mode,
                metadata.streaming_finalized,
                metadata.stream_delta_count,
                metadata.streamed_chars,
                metadata.stream_first_delta_ms,
                metadata.stream_duration_ms,
                metadata.telegram_chat_id,
                metadata.telegram_job_id,
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

    pub fn qa_logs(&self, author: Option<&str>, limit: usize) -> Result<Vec<QaLogSummary>> {
        let mut sql = r#"
            SELECT id, author, question, model, latency_ms, prompt_tokens,
                   completion_tokens, total_tokens, cost_usd, error_code,
                   qa_profile_version, qa_mode, route_reason, delivery_mode,
                   streaming_finalized, stream_delta_count, streamed_chars,
                   stream_first_delta_ms, stream_duration_ms,
                   telegram_chat_id, telegram_job_id, created_at
            FROM qa_logs
            WHERE 1 = 1
        "#
        .to_string();
        let mut values = Vec::new();
        if let Some(author) = author {
            sql.push_str(" AND author = ?");
            values.push(author.to_string());
        }
        sql.push_str(" ORDER BY id DESC LIMIT ?");
        values.push(limit.to_string());
        let params = rusqlite::params_from_iter(values.iter());
        let mut stmt = self.conn.prepare(&sql)?;
        let rows = stmt.query_map(params, |row| {
            Ok(QaLogSummary {
                id: row.get(0)?,
                author: row.get(1)?,
                question: row.get(2)?,
                model: row.get(3)?,
                latency_ms: row.get(4)?,
                prompt_tokens: row.get(5)?,
                completion_tokens: row.get(6)?,
                total_tokens: row.get(7)?,
                cost_usd: row.get(8)?,
                error_code: row.get(9)?,
                qa_profile_version: row.get(10)?,
                qa_mode: row.get(11)?,
                route_reason: row.get(12)?,
                delivery_mode: row.get(13)?,
                streaming_finalized: row.get(14)?,
                stream_delta_count: row.get(15)?,
                streamed_chars: row.get(16)?,
                stream_first_delta_ms: row.get(17)?,
                stream_duration_ms: row.get(18)?,
                telegram_chat_id: row.get(19)?,
                telegram_job_id: row.get(20)?,
                created_at: row.get(21)?,
            })
        })?;
        rows.collect::<rusqlite::Result<Vec<_>>>()
            .map_err(Into::into)
    }

    pub fn qa_log_trend(&self, author: Option<&str>, days: usize) -> Result<Vec<QaLogTrend>> {
        let mut sql = r#"
            SELECT date(created_at) AS day,
                   COUNT(*),
                   SUM(CASE WHEN error_code IS NOT NULL AND error_code != '' THEN 1 ELSE 0 END),
                   AVG(latency_ms),
                   SUM(total_tokens),
                   SUM(cost_usd),
                   SUM(CASE WHEN delivery_mode = 'streaming' THEN 1 ELSE 0 END),
                   SUM(CASE WHEN streaming_finalized = 1 THEN 1 ELSE 0 END),
                   SUM(CASE WHEN telegram_chat_id IS NOT NULL AND telegram_job_id IS NOT NULL THEN 1 ELSE 0 END)
            FROM qa_logs
            WHERE 1 = 1
        "#
        .to_string();
        let mut values = Vec::new();
        if let Some(author) = author {
            sql.push_str(" AND author = ?");
            values.push(author.to_string());
        }
        sql.push_str(
            r#"
            GROUP BY day
            ORDER BY day DESC
            LIMIT ?
            "#,
        );
        values.push(days.to_string());
        let params = rusqlite::params_from_iter(values.iter());
        let mut stmt = self.conn.prepare(&sql)?;
        let rows = stmt.query_map(params, |row| {
            Ok(QaLogTrend {
                day: row.get(0)?,
                total: row.get(1)?,
                errors: row.get(2)?,
                avg_latency_ms: row.get(3)?,
                total_tokens: row.get(4)?,
                total_cost_usd: row.get(5)?,
                streaming: row.get(6)?,
                streaming_finalized: row.get(7)?,
                telegram: row.get(8)?,
            })
        })?;
        rows.collect::<rusqlite::Result<Vec<_>>>()
            .map_err(Into::into)
    }

    pub fn qa_error_counts(&self, author: Option<&str>) -> Result<Vec<(String, i64)>> {
        if let Some(author) = author {
            let mut stmt = self.conn.prepare(
                r#"
                SELECT error_code, COUNT(*)
                FROM qa_logs
                WHERE author = ? AND error_code IS NOT NULL AND error_code != ''
                GROUP BY error_code
                ORDER BY COUNT(*) DESC, error_code ASC
                "#,
            )?;
            let rows = stmt.query_map(params![author], |row| Ok((row.get(0)?, row.get(1)?)))?;
            return rows
                .collect::<rusqlite::Result<Vec<_>>>()
                .map_err(Into::into);
        }
        let mut stmt = self.conn.prepare(
            r#"
            SELECT error_code, COUNT(*)
            FROM qa_logs
            WHERE error_code IS NOT NULL AND error_code != ''
            GROUP BY error_code
            ORDER BY COUNT(*) DESC, error_code ASC
            "#,
        )?;
        let rows = stmt.query_map([], |row| Ok((row.get(0)?, row.get(1)?)))?;
        rows.collect::<rusqlite::Result<Vec<_>>>()
            .map_err(Into::into)
    }
}
