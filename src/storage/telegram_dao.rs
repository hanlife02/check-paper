use anyhow::Result;
use rusqlite::{OptionalExtension, params};

use super::{
    NewTelegramDeliveryLog, NewTelegramPendingAuthorSelection, Storage, TelegramDeliveryLog,
    TelegramDeliverySummary, TelegramDeliveryTrend, TelegramPendingAuthorSelection,
    TelegramQaDeliveryLog,
};

impl Storage {
    pub fn save_telegram_chat_author(&self, chat_id: i64, author: &str) -> Result<()> {
        self.conn.execute(
            r#"
            INSERT INTO telegram_chat_settings (chat_id, default_author, updated_at)
            VALUES (?, ?, CURRENT_TIMESTAMP)
            ON CONFLICT(chat_id) DO UPDATE SET
                default_author = excluded.default_author,
                updated_at = CURRENT_TIMESTAMP
            "#,
            params![chat_id, author],
        )?;
        Ok(())
    }

    pub fn telegram_chat_author(&self, chat_id: i64) -> Result<Option<String>> {
        self.conn
            .query_row(
                "SELECT default_author FROM telegram_chat_settings WHERE chat_id = ?",
                params![chat_id],
                |row| row.get(0),
            )
            .optional()
            .map_err(Into::into)
    }

    pub(crate) fn save_telegram_pending_author_selection(
        &self,
        selection: NewTelegramPendingAuthorSelection<'_>,
    ) -> Result<()> {
        let authors_json = serde_json::to_string(selection.authors)?;
        self.conn.execute(
            r#"
            INSERT INTO telegram_pending_author_selections
                (chat_id, action, question, authors_json, updated_at)
            VALUES (?, ?, ?, ?, CURRENT_TIMESTAMP)
            ON CONFLICT(chat_id) DO UPDATE SET
                action = excluded.action,
                question = excluded.question,
                authors_json = excluded.authors_json,
                updated_at = CURRENT_TIMESTAMP
            "#,
            params![
                selection.chat_id,
                selection.action,
                selection.question,
                authors_json,
            ],
        )?;
        Ok(())
    }

    pub(crate) fn telegram_pending_author_selection(
        &self,
        chat_id: i64,
    ) -> Result<Option<TelegramPendingAuthorSelection>> {
        self.conn
            .query_row(
                r#"
                SELECT chat_id, action, question, authors_json, updated_at
                FROM telegram_pending_author_selections
                WHERE chat_id = ?
                "#,
                params![chat_id],
                |row| {
                    let authors_json: String = row.get(3)?;
                    let authors =
                        serde_json::from_str::<Vec<String>>(&authors_json).map_err(|err| {
                            rusqlite::Error::FromSqlConversionFailure(
                                3,
                                rusqlite::types::Type::Text,
                                Box::new(err),
                            )
                        })?;
                    Ok(TelegramPendingAuthorSelection {
                        chat_id: row.get(0)?,
                        action: row.get(1)?,
                        question: row.get(2)?,
                        authors,
                        updated_at: row.get(4)?,
                    })
                },
            )
            .optional()
            .map_err(Into::into)
    }

    pub(crate) fn clear_telegram_pending_author_selection(&self, chat_id: i64) -> Result<()> {
        self.conn.execute(
            "DELETE FROM telegram_pending_author_selections WHERE chat_id = ?",
            params![chat_id],
        )?;
        Ok(())
    }

    pub fn save_telegram_delivery_log(&self, entry: NewTelegramDeliveryLog<'_>) -> Result<()> {
        self.conn.execute(
            r#"
            INSERT INTO telegram_delivery_logs (
                chat_id, job_id, final_delivery, preview_edit_attempts,
                preview_edit_successes, preview_edit_failures, preview_last_chars,
                reply_chars, cancelled, error_code
            )
            VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
            "#,
            params![
                entry.chat_id,
                entry.job_id,
                entry.final_delivery,
                entry.preview_edit_attempts,
                entry.preview_edit_successes,
                entry.preview_edit_failures,
                entry.preview_last_chars,
                entry.reply_chars,
                entry.cancelled,
                entry.error_code,
            ],
        )?;
        Ok(())
    }

    pub fn telegram_delivery_logs(
        &self,
        chat_id: Option<i64>,
        limit: usize,
    ) -> Result<Vec<TelegramDeliveryLog>> {
        let mut sql = r#"
            SELECT id, chat_id, job_id, final_delivery, preview_edit_attempts,
                   preview_edit_successes, preview_edit_failures, preview_last_chars,
                   reply_chars, cancelled, error_code, created_at
            FROM telegram_delivery_logs
            WHERE 1 = 1
        "#
        .to_string();
        let mut values = Vec::new();
        if let Some(chat_id) = chat_id {
            sql.push_str(" AND chat_id = ?");
            values.push(chat_id.to_string());
        }
        sql.push_str(" ORDER BY id DESC LIMIT ?");
        values.push(limit.to_string());
        let params = rusqlite::params_from_iter(values.iter());
        let mut stmt = self.conn.prepare(&sql)?;
        let rows = stmt.query_map(params, |row| {
            Ok(TelegramDeliveryLog {
                id: row.get(0)?,
                chat_id: row.get(1)?,
                job_id: row.get(2)?,
                final_delivery: row.get(3)?,
                preview_edit_attempts: row.get(4)?,
                preview_edit_successes: row.get(5)?,
                preview_edit_failures: row.get(6)?,
                preview_last_chars: row.get(7)?,
                reply_chars: row.get(8)?,
                cancelled: row.get(9)?,
                error_code: row.get(10)?,
                created_at: row.get(11)?,
            })
        })?;
        rows.collect::<rusqlite::Result<Vec<_>>>()
            .map_err(Into::into)
    }

    pub fn telegram_delivery_summary(
        &self,
        chat_id: Option<i64>,
    ) -> Result<Vec<TelegramDeliverySummary>> {
        let mut sql = r#"
            SELECT final_delivery, error_code, COUNT(*), SUM(cancelled),
                   SUM(preview_edit_attempts), SUM(preview_edit_successes),
                   SUM(preview_edit_failures), SUM(reply_chars)
            FROM telegram_delivery_logs
            WHERE 1 = 1
        "#
        .to_string();
        let mut values = Vec::new();
        if let Some(chat_id) = chat_id {
            sql.push_str(" AND chat_id = ?");
            values.push(chat_id.to_string());
        }
        sql.push_str(
            r#"
            GROUP BY final_delivery, error_code
            ORDER BY COUNT(*) DESC, final_delivery ASC, COALESCE(error_code, '') ASC
            "#,
        );
        let params = rusqlite::params_from_iter(values.iter());
        let mut stmt = self.conn.prepare(&sql)?;
        let rows = stmt.query_map(params, |row| {
            Ok(TelegramDeliverySummary {
                final_delivery: row.get(0)?,
                error_code: row.get(1)?,
                total: row.get(2)?,
                cancelled: row.get(3)?,
                preview_edit_attempts: row.get(4)?,
                preview_edit_successes: row.get(5)?,
                preview_edit_failures: row.get(6)?,
                reply_chars: row.get(7)?,
            })
        })?;
        rows.collect::<rusqlite::Result<Vec<_>>>()
            .map_err(Into::into)
    }

    pub fn telegram_delivery_trend(
        &self,
        chat_id: Option<i64>,
        days: usize,
    ) -> Result<Vec<TelegramDeliveryTrend>> {
        let mut sql = r#"
            SELECT date(t.created_at) AS day,
                   COUNT(*),
                   SUM(CASE WHEN t.cancelled = 1 THEN 1 ELSE 0 END),
                   SUM(CASE WHEN t.final_delivery = 'failed' OR (t.error_code IS NOT NULL AND t.error_code != '') THEN 1 ELSE 0 END),
                   SUM(CASE WHEN t.final_delivery = 'edited_placeholder' THEN 1 ELSE 0 END),
                   SUM(CASE WHEN t.final_delivery = 'sent_fallback' THEN 1 ELSE 0 END),
                   SUM(CASE WHEN q.id IS NOT NULL THEN 1 ELSE 0 END),
                   SUM(t.preview_edit_attempts),
                   SUM(t.preview_edit_failures),
                   SUM(t.reply_chars)
            FROM telegram_delivery_logs t
            LEFT JOIN qa_logs q
                ON q.telegram_chat_id = t.chat_id
               AND q.telegram_job_id = t.job_id
            WHERE 1 = 1
        "#
        .to_string();
        let mut values = Vec::new();
        if let Some(chat_id) = chat_id {
            sql.push_str(" AND t.chat_id = ?");
            values.push(chat_id.to_string());
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
            Ok(TelegramDeliveryTrend {
                day: row.get(0)?,
                total: row.get(1)?,
                cancelled: row.get(2)?,
                failed: row.get(3)?,
                edited_placeholder: row.get(4)?,
                sent_fallback: row.get(5)?,
                matched_qa: row.get(6)?,
                preview_edit_attempts: row.get(7)?,
                preview_edit_failures: row.get(8)?,
                reply_chars: row.get(9)?,
            })
        })?;
        rows.collect::<rusqlite::Result<Vec<_>>>()
            .map_err(Into::into)
    }

    pub fn telegram_delivery_logs_with_qa(
        &self,
        chat_id: Option<i64>,
        limit: usize,
    ) -> Result<Vec<TelegramQaDeliveryLog>> {
        let mut sql = r#"
            SELECT t.id, t.chat_id, t.job_id, t.final_delivery, t.cancelled,
                   t.error_code, t.created_at,
                   q.id, q.author, q.question, q.error_code, q.qa_mode,
                   q.route_reason, q.streaming_finalized
            FROM telegram_delivery_logs t
            LEFT JOIN qa_logs q
                ON q.telegram_chat_id = t.chat_id
               AND q.telegram_job_id = t.job_id
            WHERE 1 = 1
        "#
        .to_string();
        let mut values = Vec::new();
        if let Some(chat_id) = chat_id {
            sql.push_str(" AND t.chat_id = ?");
            values.push(chat_id.to_string());
        }
        sql.push_str(" ORDER BY t.id DESC LIMIT ?");
        values.push(limit.to_string());
        let params = rusqlite::params_from_iter(values.iter());
        let mut stmt = self.conn.prepare(&sql)?;
        let rows = stmt.query_map(params, |row| {
            Ok(TelegramQaDeliveryLog {
                id: row.get(0)?,
                chat_id: row.get(1)?,
                job_id: row.get(2)?,
                final_delivery: row.get(3)?,
                cancelled: row.get(4)?,
                error_code: row.get(5)?,
                created_at: row.get(6)?,
                qa_log_id: row.get(7)?,
                qa_author: row.get(8)?,
                qa_question: row.get(9)?,
                qa_error_code: row.get(10)?,
                qa_mode: row.get(11)?,
                route_reason: row.get(12)?,
                streaming_finalized: row.get(13)?,
            })
        })?;
        rows.collect::<rusqlite::Result<Vec<_>>>()
            .map_err(Into::into)
    }
}

#[cfg(test)]
mod tests {
    use tempfile::tempdir;

    use crate::storage::{
        NewTelegramDeliveryLog, NewTelegramPendingAuthorSelection, QaLogEntry, QaLogMetadata,
        Storage,
    };

    #[test]
    fn persists_telegram_chat_author() {
        let dir = tempdir().unwrap();
        let db_path = dir.path().join("test.sqlite");
        let storage = Storage::open(&db_path).unwrap();

        storage.save_telegram_chat_author(7, "Alice").unwrap();
        storage.save_telegram_chat_author(7, "Bob").unwrap();

        drop(storage);
        let reopened = Storage::open(&db_path).unwrap();
        assert_eq!(
            reopened.telegram_chat_author(7).unwrap().as_deref(),
            Some("Bob")
        );
    }

    #[test]
    fn persists_telegram_pending_author_selection() {
        let dir = tempdir().unwrap();
        let db_path = dir.path().join("test.sqlite");
        let storage = Storage::open(&db_path).unwrap();
        let authors = vec!["Alice".to_string(), "Bob".to_string()];

        storage
            .save_telegram_pending_author_selection(NewTelegramPendingAuthorSelection {
                chat_id: 7,
                action: "ask",
                question: Some("What changed?"),
                authors: &authors,
            })
            .unwrap();

        drop(storage);
        let reopened = Storage::open(&db_path).unwrap();
        let selection = reopened
            .telegram_pending_author_selection(7)
            .unwrap()
            .unwrap();
        assert_eq!(selection.action, "ask");
        assert_eq!(selection.question.as_deref(), Some("What changed?"));
        assert_eq!(selection.authors, authors);

        reopened.clear_telegram_pending_author_selection(7).unwrap();
        assert!(
            reopened
                .telegram_pending_author_selection(7)
                .unwrap()
                .is_none()
        );
    }

    #[test]
    fn records_telegram_delivery_logs() {
        let dir = tempdir().unwrap();
        let db_path = dir.path().join("test.sqlite");
        let storage = Storage::open(&db_path).unwrap();

        storage
            .save_telegram_delivery_log(NewTelegramDeliveryLog {
                chat_id: 7,
                job_id: 42,
                final_delivery: "edited_placeholder",
                preview_edit_attempts: 3,
                preview_edit_successes: 2,
                preview_edit_failures: 1,
                preview_last_chars: 120,
                reply_chars: 240,
                cancelled: false,
                error_code: None,
            })
            .unwrap();
        storage
            .save_telegram_delivery_log(NewTelegramDeliveryLog {
                chat_id: 8,
                job_id: 43,
                final_delivery: "skipped_cancelled",
                preview_edit_attempts: 1,
                preview_edit_successes: 1,
                preview_edit_failures: 0,
                preview_last_chars: 80,
                reply_chars: 160,
                cancelled: true,
                error_code: Some("cancelled"),
            })
            .unwrap();

        let all = storage.telegram_delivery_logs(None, 10).unwrap();
        assert_eq!(all.len(), 2);
        assert_eq!(all[0].chat_id, 8);
        assert!(all[0].cancelled);
        assert_eq!(all[0].error_code.as_deref(), Some("cancelled"));

        let filtered = storage.telegram_delivery_logs(Some(7), 10).unwrap();
        assert_eq!(filtered.len(), 1);
        assert_eq!(filtered[0].job_id, 42);
        assert_eq!(filtered[0].preview_edit_successes, 2);
        assert_eq!(filtered[0].final_delivery, "edited_placeholder");
    }

    #[test]
    fn summarizes_telegram_delivery_logs() {
        let dir = tempdir().unwrap();
        let storage = Storage::open(&dir.path().join("test.sqlite")).unwrap();

        for (chat_id, job_id, final_delivery, cancelled, error_code) in [
            (7, 1, "edited_placeholder", false, None),
            (7, 2, "edited_placeholder", false, None),
            (7, 3, "skipped_cancelled", true, Some("cancelled")),
            (8, 4, "failed", false, Some("final_delivery_failed")),
        ] {
            storage
                .save_telegram_delivery_log(NewTelegramDeliveryLog {
                    chat_id,
                    job_id,
                    final_delivery,
                    preview_edit_attempts: 2,
                    preview_edit_successes: 1,
                    preview_edit_failures: 1,
                    preview_last_chars: 80,
                    reply_chars: 100,
                    cancelled,
                    error_code,
                })
                .unwrap();
        }

        let summary = storage.telegram_delivery_summary(Some(7)).unwrap();

        assert_eq!(summary.len(), 2);
        assert_eq!(summary[0].final_delivery, "edited_placeholder");
        assert_eq!(summary[0].total, 2);
        assert_eq!(summary[0].cancelled, 0);
        assert_eq!(summary[0].preview_edit_attempts, 4);
        assert_eq!(summary[1].final_delivery, "skipped_cancelled");
        assert_eq!(summary[1].error_code.as_deref(), Some("cancelled"));
        assert_eq!(summary[1].cancelled, 1);
    }

    #[test]
    fn joins_telegram_delivery_logs_to_qa_logs() {
        let dir = tempdir().unwrap();
        let storage = Storage::open(&dir.path().join("test.sqlite")).unwrap();

        storage
            .save_qa_log_with_metadata(QaLogEntry {
                author: "Alice",
                question: "What changed?",
                retrieval: &serde_json::json!({}),
                answer: &serde_json::json!({ "answer": "ok" }),
                model: "model",
                latency_ms: 42,
                metadata: Some(QaLogMetadata {
                    qa_mode: Some("source_evidence"),
                    route_reason: Some("detail_keyword"),
                    delivery_mode: Some("streaming"),
                    streaming_finalized: Some(true),
                    telegram_chat_id: Some(7),
                    telegram_job_id: Some(42),
                    ..Default::default()
                }),
            })
            .unwrap();
        storage
            .save_telegram_delivery_log(NewTelegramDeliveryLog {
                chat_id: 7,
                job_id: 42,
                final_delivery: "edited_placeholder",
                preview_edit_attempts: 2,
                preview_edit_successes: 2,
                preview_edit_failures: 0,
                preview_last_chars: 80,
                reply_chars: 160,
                cancelled: false,
                error_code: None,
            })
            .unwrap();

        let rows = storage.telegram_delivery_logs_with_qa(Some(7), 10).unwrap();

        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].final_delivery, "edited_placeholder");
        assert!(rows[0].qa_log_id.is_some());
        assert_eq!(rows[0].qa_author.as_deref(), Some("Alice"));
        assert_eq!(rows[0].qa_question.as_deref(), Some("What changed?"));
        assert_eq!(rows[0].qa_mode.as_deref(), Some("source_evidence"));
        assert_eq!(rows[0].route_reason.as_deref(), Some("detail_keyword"));
        assert_eq!(rows[0].streaming_finalized, Some(true));

        let trend = storage.telegram_delivery_trend(Some(7), 7).unwrap();
        assert_eq!(trend.len(), 1);
        assert_eq!(trend[0].total, 1);
        assert_eq!(trend[0].matched_qa, 1);
        assert_eq!(trend[0].edited_placeholder, 1);
        assert_eq!(trend[0].sent_fallback, 0);
        assert_eq!(trend[0].cancelled, 0);
        assert_eq!(trend[0].failed, 0);
        assert_eq!(trend[0].preview_edit_attempts, 2);
        assert_eq!(trend[0].preview_edit_failures, 0);
        assert_eq!(trend[0].reply_chars, 160);
    }
}
