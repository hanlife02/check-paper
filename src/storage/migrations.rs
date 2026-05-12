use anyhow::Result;
use rusqlite::{OptionalExtension, params};

use super::Storage;

impl Storage {
    pub(super) fn init_schema(&self) -> Result<()> {
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
                parser_version TEXT,
                cleaner_version TEXT,
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
        self.apply_metadata_migrations()?;
        self.apply_embedding_contract_migration()?;
        Ok(())
    }

    fn apply_metadata_migrations(&self) -> Result<()> {
        for (table, column, definition) in [
            ("embeddings", "model_version", "TEXT"),
            ("embeddings", "chunk_hash", "TEXT"),
            ("paper_facts", "source_hash", "TEXT"),
            ("paper_facts", "chunk_hash", "TEXT"),
            ("paper_facts", "confidence", "TEXT"),
            ("paper_facts", "extractor", "TEXT"),
            ("paper_facts", "extractor_version", "TEXT"),
            ("qa_logs", "answer_schema_version", "INTEGER"),
            ("qa_logs", "qa_prompt_version", "TEXT"),
            ("qa_logs", "temperature", "REAL"),
            ("qa_logs", "max_tokens", "INTEGER"),
            ("qa_logs", "prompt_tokens", "INTEGER"),
            ("qa_logs", "completion_tokens", "INTEGER"),
            ("qa_logs", "total_tokens", "INTEGER"),
            ("qa_logs", "cost_usd", "REAL"),
            ("qa_logs", "error_code", "TEXT"),
            ("qa_logs", "retrieval_trace_json", "TEXT"),
            ("analysis_jobs", "source_hash", "TEXT"),
            ("analysis_jobs", "profile_schema_version", "INTEGER"),
            ("analysis_jobs", "prompt_version", "TEXT"),
            ("analysis_jobs", "model_id", "TEXT"),
            (
                "analysis_jobs",
                "attempt_count",
                "INTEGER NOT NULL DEFAULT 0",
            ),
            (
                "analysis_jobs",
                "max_attempts",
                "INTEGER NOT NULL DEFAULT 3",
            ),
            ("analysis_jobs", "last_error_code", "TEXT"),
            ("analysis_jobs", "next_retry_at", "TEXT"),
            ("analysis_jobs", "locked_by", "TEXT"),
            ("analysis_jobs", "lock_until", "TEXT"),
            ("papers", "profile_prompt_version", "TEXT"),
            ("papers", "profile_model_id", "TEXT"),
            ("papers", "profile_generated_at", "TEXT"),
            ("papers", "profile_chunker_version", "TEXT"),
            ("papers", "profile_status", "TEXT"),
            ("papers", "profile_error_code", "TEXT"),
            ("papers", "parser_version", "TEXT"),
            ("papers", "cleaner_version", "TEXT"),
            ("author_profiles", "profile_schema_version", "INTEGER"),
            ("author_profiles", "profile_prompt_version", "TEXT"),
            ("author_profiles", "profile_model_id", "TEXT"),
            ("author_profiles", "source_profile_hash", "TEXT"),
            ("chunks", "chunk_hash", "TEXT"),
            ("chunks", "chunker_version", "TEXT"),
            ("chunks", "max_chars", "INTEGER"),
            ("chunks", "overlap", "INTEGER"),
            ("chunks", "source_hash", "TEXT"),
            ("chunks", "section_path", "TEXT"),
            ("chunks", "section_kind", "TEXT"),
            ("chunks", "caption_label", "TEXT"),
        ] {
            if !self.column_exists(table, column)? {
                self.conn.execute(
                    &format!("ALTER TABLE {table} ADD COLUMN {column} {definition}"),
                    [],
                )?;
            }
        }
        Ok(())
    }

    fn apply_embedding_contract_migration(&self) -> Result<()> {
        self.apply_migration(
            2,
            "embedding_model_version_contract",
            r#"
            CREATE TABLE IF NOT EXISTS embeddings_v2 (
                target_type TEXT NOT NULL,
                target_id TEXT NOT NULL,
                model TEXT NOT NULL,
                model_version TEXT NOT NULL DEFAULT '',
                dim INTEGER NOT NULL,
                vector BLOB NOT NULL,
                source_hash TEXT,
                chunk_hash TEXT,
                created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
                PRIMARY KEY(target_type, target_id, model, model_version)
            );

            INSERT OR IGNORE INTO embeddings_v2 (
                target_type, target_id, model, model_version, dim, vector,
                source_hash, chunk_hash, created_at
            )
            SELECT target_type, target_id, model, COALESCE(model_version, ''),
                   dim, vector, source_hash, chunk_hash, created_at
            FROM embeddings;

            DROP TABLE embeddings;
            ALTER TABLE embeddings_v2 RENAME TO embeddings;

            CREATE TABLE IF NOT EXISTS embedding_jobs (
                id INTEGER PRIMARY KEY,
                target_type TEXT NOT NULL,
                target_id TEXT NOT NULL,
                model TEXT NOT NULL,
                model_version TEXT NOT NULL DEFAULT '',
                source_hash TEXT,
                chunk_hash TEXT,
                status TEXT NOT NULL,
                attempt_count INTEGER NOT NULL DEFAULT 0,
                last_error TEXT,
                created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
                updated_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP
            );
            "#,
        )?;
        self.conn.execute_batch(
            r#"
            CREATE TABLE IF NOT EXISTS embedding_jobs (
                id INTEGER PRIMARY KEY,
                target_type TEXT NOT NULL,
                target_id TEXT NOT NULL,
                model TEXT NOT NULL,
                model_version TEXT NOT NULL DEFAULT '',
                source_hash TEXT,
                chunk_hash TEXT,
                status TEXT NOT NULL,
                attempt_count INTEGER NOT NULL DEFAULT 0,
                last_error TEXT,
                created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
                updated_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP
            );
            "#,
        )?;
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

    pub(crate) fn column_exists(&self, table: &str, column: &str) -> Result<bool> {
        let mut stmt = self.conn.prepare(&format!("PRAGMA table_info({table})"))?;
        let rows = stmt.query_map([], |row| row.get::<_, String>(1))?;
        for row in rows {
            if row? == column {
                return Ok(true);
            }
        }
        Ok(false)
    }
}
