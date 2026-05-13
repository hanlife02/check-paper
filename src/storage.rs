use std::path::Path;

use anyhow::Result;
use chrono::Utc;
use rusqlite::Connection;
use serde_json::Value;

mod chunks_dao;
mod embeddings_dao;
mod facts_dao;
mod jobs_dao;
mod migrations;
mod papers_dao;
mod profiles_dao;
mod qa_logs_dao;

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
    pub source_hash: String,
    pub chunk_hash: String,
    pub chunker_version: String,
    pub section_kind: String,
    pub caption_label: Option<String>,
}

pub struct Storage {
    conn: Connection,
}

#[derive(Debug, Clone)]
pub struct LibraryStatus {
    pub papers: i64,
    pub analyzed: i64,
    pub stale_papers: i64,
    pub failed_jobs: i64,
    pub queued_jobs: i64,
    pub running_jobs: i64,
    pub retry_waiting_jobs: i64,
    pub cancelled_jobs: i64,
    pub qa_logs: i64,
    pub avg_qa_latency_ms: Option<f64>,
    pub total_qa_tokens: Option<i64>,
    pub total_qa_cost_usd: Option<f64>,
}

#[derive(Debug, Clone)]
pub struct AnalysisJobSummary {
    pub id: i64,
    pub paper_key: Option<String>,
    pub job_type: String,
    pub status: String,
    pub error_code: Option<String>,
    pub error: Option<String>,
    pub model_id: Option<String>,
    pub updated_at: String,
}

#[derive(Debug, Clone)]
pub struct QaLogSummary {
    pub id: i64,
    pub author: String,
    pub question: String,
    pub model: Option<String>,
    pub latency_ms: Option<i64>,
    pub prompt_tokens: Option<i64>,
    pub completion_tokens: Option<i64>,
    pub total_tokens: Option<i64>,
    pub cost_usd: Option<f64>,
    pub error_code: Option<String>,
    pub created_at: String,
}

#[derive(Debug, Clone)]
pub struct AnalysisJobTask {
    pub id: i64,
    pub candidate: AnalysisCandidate,
    pub attempt_count: i64,
    pub max_attempts: i64,
}

#[derive(Debug, Clone, Copy, Default)]
pub struct QaLogMetadata<'a> {
    pub answer_schema_version: Option<i64>,
    pub qa_prompt_version: Option<&'a str>,
    pub temperature: Option<f64>,
    pub max_tokens: Option<i64>,
    pub prompt_tokens: Option<i64>,
    pub completion_tokens: Option<i64>,
    pub total_tokens: Option<i64>,
    pub cost_usd: Option<f64>,
    pub error_code: Option<&'a str>,
}

#[derive(Debug, Clone, Copy)]
pub struct PaperProfileMetadata<'a> {
    pub source_hash: &'a str,
    pub schema_version: i64,
    pub prompt_version: &'a str,
    pub model_id: &'a str,
    pub chunker_version: &'a str,
}

#[derive(Debug, Clone, Copy)]
pub struct AnalysisJobMetadata<'a> {
    pub job_type: &'a str,
    pub status: &'a str,
    pub error_code: Option<&'a str>,
    pub error: Option<&'a str>,
    pub profile_schema_version: i64,
    pub prompt_version: &'a str,
    pub model_id: &'a str,
}

#[derive(Debug, Clone, Copy)]
pub struct QaLogEntry<'a> {
    pub author: &'a str,
    pub question: &'a str,
    pub retrieval: &'a Value,
    pub answer: &'a Value,
    pub model: &'a str,
    pub latency_ms: i64,
    pub metadata: Option<QaLogMetadata<'a>>,
}

#[derive(Debug, Clone, Copy)]
pub(super) struct QaLogStats {
    pub count: i64,
    pub avg_latency_ms: Option<f64>,
    pub total_tokens: Option<i64>,
    pub total_cost_usd: Option<f64>,
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
        source_hash: row.get(8)?,
        chunk_hash: row.get(9)?,
        chunker_version: row.get(10)?,
        section_kind: row.get(11)?,
        caption_label: row.get(12)?,
    })
}

fn sqlite_timestamp(time: chrono::DateTime<Utc>) -> String {
    time.format("%Y-%m-%d %H:%M:%S").to_string()
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use serde_json::json;
    use tempfile::tempdir;

    use crate::papers::models::{Paper, Section};
    use crate::retrieval::chunker::chunk_paper;
    use crate::retrieval::fusion::rrf_merge_chunks;
    use crate::retrieval::query::query_terms;
    use rusqlite::Connection;

    use super::{QaLogEntry, QaLogMetadata, SourceChunk, Storage};

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
    fn records_qa_log_usage_metadata() {
        let dir = tempdir().unwrap();
        let storage = Storage::open(&dir.path().join("test.sqlite")).unwrap();
        storage
            .save_qa_log_with_metadata(QaLogEntry {
                author: "Alice",
                question: "question",
                retrieval: &json!({ "chunks": [] }),
                answer: &json!({ "answer": "ok" }),
                model: "test-model",
                latency_ms: 12,
                metadata: Some(QaLogMetadata {
                    answer_schema_version: Some(1),
                    qa_prompt_version: Some("qa-v1"),
                    temperature: Some(0.2),
                    max_tokens: Some(2200),
                    prompt_tokens: Some(11),
                    completion_tokens: Some(7),
                    total_tokens: Some(18),
                    cost_usd: Some(0.001),
                    error_code: None,
                }),
            })
            .unwrap();

        let (prompt_tokens, completion_tokens, total_tokens): (i64, i64, i64) = storage
            .conn
            .query_row(
                "SELECT prompt_tokens, completion_tokens, total_tokens FROM qa_logs LIMIT 1",
                [],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .unwrap();
        assert_eq!(
            (prompt_tokens, completion_tokens, total_tokens),
            (11, 7, 18)
        );
        let logs = storage.qa_logs(Some("Alice"), 5).unwrap();
        assert_eq!(logs.len(), 1);
        assert_eq!(logs[0].prompt_tokens, Some(11));
        assert_eq!(logs[0].completion_tokens, Some(7));
        assert_eq!(logs[0].total_tokens, Some(18));
        let status = storage.library_status(Some("Alice")).unwrap();
        assert_eq!(status.total_qa_tokens, Some(18));
        assert!((status.total_qa_cost_usd.unwrap() - 0.001).abs() < f64::EPSILON);
    }

    #[test]
    fn summarizes_qa_error_counts() {
        let dir = tempdir().unwrap();
        let storage = Storage::open(&dir.path().join("test.sqlite")).unwrap();
        for (author, error_code) in [
            ("Alice", "evidence_invalid"),
            ("Alice", "evidence_invalid"),
            ("Bob", "verifier_failed"),
        ] {
            storage
                .save_qa_log_with_metadata(QaLogEntry {
                    author,
                    question: "question",
                    retrieval: &json!({ "chunks": [] }),
                    answer: &json!({ "answer": "failed" }),
                    model: "test-model",
                    latency_ms: 12,
                    metadata: Some(QaLogMetadata {
                        error_code: Some(error_code),
                        ..Default::default()
                    }),
                })
                .unwrap();
        }

        assert_eq!(
            storage.qa_error_counts(Some("Alice")).unwrap(),
            vec![("evidence_invalid".to_string(), 2)]
        );
        assert_eq!(
            storage.qa_error_counts(None).unwrap(),
            vec![
                ("evidence_invalid".to_string(), 2),
                ("verifier_failed".to_string(), 1)
            ]
        );
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
        assert_eq!(migration_count, 2);
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
    fn can_cancel_and_retry_failed_jobs() {
        let dir = tempdir().unwrap();
        let storage = Storage::open(&dir.path().join("test.sqlite")).unwrap();
        storage
            .record_analysis_job("Alice/paper-a", "analyze", "failed", Some("boom"))
            .unwrap();
        storage.cancel_analysis_job(1).unwrap();
        let cancelled = storage.analysis_jobs(None, Some("cancelled"), 10).unwrap();
        assert_eq!(cancelled.len(), 1);

        storage
            .record_analysis_job("Alice/paper-b", "analyze", "failed", Some("boom"))
            .unwrap();
        let queued = storage.retry_failed_analysis_jobs(None).unwrap();
        assert_eq!(queued, 1);
        let queued_jobs = storage.analysis_jobs(None, Some("queued"), 10).unwrap();
        assert_eq!(queued_jobs.len(), 1);
        assert_eq!(queued_jobs[0].paper_key.as_deref(), Some("Alice/paper-b"));
    }

    #[test]
    fn summarizes_analysis_job_error_counts() {
        let dir = tempdir().unwrap();
        let storage = Storage::open(&dir.path().join("test.sqlite")).unwrap();
        for (paper_key, status, error_code) in [
            ("Alice/paper-a", "failed", "schema_error"),
            ("Alice/paper-b", "retry_waiting", "schema_error"),
            ("Bob/paper-c", "failed", "http_timeout"),
        ] {
            storage
                .conn
                .execute(
                    r#"
                    INSERT INTO analysis_jobs (
                        paper_key, job_type, status, last_error_code
                    )
                    VALUES (?, 'analyze', ?, ?)
                    "#,
                    rusqlite::params![paper_key, status, error_code],
                )
                .unwrap();
        }

        assert_eq!(
            storage
                .analysis_job_error_counts(Some("Alice"), None)
                .unwrap(),
            vec![("schema_error".to_string(), 2)]
        );
        assert_eq!(
            storage
                .analysis_job_error_counts(None, Some("failed"))
                .unwrap(),
            vec![
                ("http_timeout".to_string(), 1),
                ("schema_error".to_string(), 1),
            ]
        );
    }

    #[test]
    fn finds_failed_analysis_candidates() {
        let dir = tempdir().unwrap();
        let storage = Storage::open(&dir.path().join("test.sqlite")).unwrap();
        storage
            .conn
            .execute(
                r#"
                INSERT INTO papers (
                    paper_key, author, paper_id, title, doi, year, source_hash,
                    article_path, metadata_json, fetch_result_json, profile_status
                )
                VALUES ('Alice/paper-a', 'Alice', 'paper-a', 'A Paper', '10.1/test',
                        '2024', 'hash', 'article.md', '{}', '{}', 'failed')
                "#,
                [],
            )
            .unwrap();

        let rows = storage.failed_analysis_candidates("Alice").unwrap();

        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].paper_key, "Alice/paper-a");
    }

    #[test]
    fn can_enqueue_claim_and_finish_analysis_jobs() {
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
        let rows = storage
            .papers_needing_analysis("Alice", false, 1, "prompt-v1", "model-a", "chunker-v1")
            .unwrap();
        assert_eq!(rows.len(), 1);

        let queued = storage
            .enqueue_analysis_jobs(&rows, "analyze", 1, "prompt-v1", "model-a", 2)
            .unwrap();
        assert_eq!(queued, 1);
        assert_eq!(
            storage
                .enqueue_analysis_jobs(&rows, "analyze", 1, "prompt-v1", "model-a", 2)
                .unwrap(),
            0
        );

        let task = storage
            .claim_next_analysis_job("Alice", "analyze", "worker-a", 60)
            .unwrap()
            .unwrap();
        assert_eq!(task.candidate.paper_key, "Alice/paper-a");
        assert_eq!(task.attempt_count, 0);
        let running = storage.analysis_jobs(None, Some("running"), 10).unwrap();
        assert_eq!(running.len(), 1);

        let status = storage
            .fail_analysis_job(task.id, "Alice/paper-a", "schema_error", "bad json")
            .unwrap();
        assert_eq!(status, "retry_waiting");
        let waiting = storage
            .analysis_jobs(None, Some("retry_waiting"), 10)
            .unwrap();
        assert_eq!(waiting.len(), 1);

        storage.retry_failed_analysis_jobs(None).unwrap();
        storage
            .conn
            .execute(
                "UPDATE analysis_jobs SET status = 'queued', next_retry_at = NULL WHERE id = ?",
                rusqlite::params![task.id],
            )
            .unwrap();
        let task = storage
            .claim_next_analysis_job("Alice", "analyze", "worker-a", 60)
            .unwrap()
            .unwrap();
        storage
            .complete_analysis_job(task.id, "Alice/paper-a")
            .unwrap();
        let succeeded = storage.analysis_jobs(None, Some("succeeded"), 10).unwrap();
        assert_eq!(succeeded.len(), 1);
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
    fn author_profile_current_check_uses_version_model_and_source_hash() {
        let dir = tempdir().unwrap();
        let storage = Storage::open(&dir.path().join("test.sqlite")).unwrap();
        storage
            .save_author_profile_with_metadata(
                "Alice",
                &json!({"author": "Alice", "answer_scope": ["catalysis"]}),
                1,
                "prompt-v1",
                "model-a",
                "profiles-hash",
            )
            .unwrap();

        assert!(
            storage
                .author_profile_is_current("Alice", 1, "prompt-v1", "model-a", "profiles-hash")
                .unwrap()
        );
        assert!(
            !storage
                .author_profile_is_current("Alice", 1, "prompt-v2", "model-a", "profiles-hash")
                .unwrap()
        );
        assert!(
            !storage
                .author_profile_is_current("Alice", 1, "prompt-v1", "model-a", "changed-hash")
                .unwrap()
        );
    }

    #[test]
    fn search_chunks_uses_paper_facts_as_candidate_route() {
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
                ("title".to_string(), "Battery paper".to_string()),
                ("doi".to_string(), "10.1/test".to_string()),
                ("year".to_string(), "2024".to_string()),
            ]),
            fetch_result: serde_json::json!({}),
            raw_body: String::new(),
            clean_text: "The reported measurement is described in the results.".to_string(),
            sections: vec![],
        };
        let chunks = chunk_paper(&paper, 3200, 350);
        storage.upsert_paper(&paper, &chunks).unwrap();
        storage
            .save_paper_facts(
                "Alice/paper-a",
                &[json!({
                    "chunk_id": 0,
                    "section": "Results",
                    "fact_type": "metric",
                    "text": "coulombic efficiency reached 99%"
                })],
            )
            .unwrap();

        let chunks = storage
            .search_chunks("Alice", "metric coulombic efficiency", 5)
            .unwrap();
        let (source_hash, chunk_hash): (Option<String>, Option<String>) = storage
            .conn
            .query_row(
                "SELECT source_hash, chunk_hash FROM paper_facts LIMIT 1",
                [],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .unwrap();

        assert!(
            chunks
                .iter()
                .any(|chunk| chunk.paper_key == "Alice/paper-a")
        );
        assert_eq!(source_hash.as_deref(), Some("hash"));
        assert!(chunk_hash.is_some());
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
            source_hash: "hash".to_string(),
            chunk_hash: "chunk-hash".to_string(),
            chunker_version: "section-char-v1".to_string(),
            section_kind: "body".to_string(),
            caption_label: None,
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
    fn chunk_embeddings_are_versioned_and_jobs_are_logged() {
        let dir = tempdir().unwrap();
        let storage = Storage::open(&dir.path().join("test.sqlite")).unwrap();
        let chunk = SourceChunk {
            id: 7,
            paper_key: "Alice/paper-a".to_string(),
            chunk_index: 0,
            section: "Results".to_string(),
            text: "chunk text".to_string(),
            title: "A Paper".to_string(),
            doi: "10.1/test".to_string(),
            year: "2024".to_string(),
            source_hash: "source-hash".to_string(),
            chunk_hash: "chunk-hash".to_string(),
            chunker_version: "section-char-v1".to_string(),
            section_kind: "body".to_string(),
            caption_label: None,
        };

        storage
            .save_chunk_embedding(&chunk, "embed-model", Some("v1"), &[0.1, 0.2], "hash-a")
            .unwrap();
        storage
            .save_chunk_embedding(&chunk, "embed-model", Some("v2"), &[0.2, 0.3], "hash-a")
            .unwrap();
        storage
            .record_embedding_job(
                &chunk,
                "embed-model",
                Some("v2"),
                "hash-a",
                "succeeded",
                None,
            )
            .unwrap();

        let embedding_count: i64 = storage
            .conn
            .query_row(
                "SELECT COUNT(*) FROM embeddings WHERE target_id = '7'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        let job_count: i64 = storage
            .conn
            .query_row("SELECT COUNT(*) FROM embedding_jobs", [], |row| row.get(0))
            .unwrap();
        assert_eq!(embedding_count, 2);
        assert_eq!(job_count, 1);
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
        let (parser_version, cleaner_version): (String, String) = storage
            .conn
            .query_row(
                "SELECT parser_version, cleaner_version FROM papers WHERE paper_key = 'Alice/paper-a'",
                [],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .unwrap();
        assert_eq!(parser_version, crate::papers::parser::PARSER_VERSION);
        assert_eq!(cleaner_version, crate::papers::cleaner::CLEANER_VERSION);
        let rows = crate::retrieval::dense_route::search_local_hash_route(
            &storage,
            "Alice",
            "Zeolite conversion",
            5,
        )
        .unwrap();
        assert_eq!(rows[0].paper_key, "Alice/paper-a");
    }

    #[test]
    fn upsert_paper_stores_caption_chunk_metadata() {
        let dir = tempdir().unwrap();
        let mut storage = Storage::open(&dir.path().join("test.sqlite")).unwrap();
        let paper = Paper {
            author: "Alice".to_string(),
            paper_id: "paper-a".to_string(),
            paper_dir: dir.path().to_path_buf(),
            article_path: dir.path().join("article.md"),
            fetch_result_path: None,
            source_hash: "hash".to_string(),
            metadata: BTreeMap::from([
                ("title".to_string(), "Caption Paper".to_string()),
                ("year".to_string(), "2024".to_string()),
            ]),
            fetch_result: json!({}),
            raw_body: String::new(),
            clean_text: String::new(),
            sections: vec![Section {
                title: "Table S1 Caption".to_string(),
                level: 2,
                content: "Table S1: catalyst metrics".to_string(),
            }],
        };
        let chunks = chunk_paper(&paper, 3200, 350);

        storage.upsert_paper(&paper, &chunks).unwrap();
        let stored = storage.all_chunks_for_author("Alice", None).unwrap();

        assert_eq!(stored[0].section_kind, "table_caption");
        assert_eq!(stored[0].caption_label.as_deref(), Some("Table S1"));
    }
}
