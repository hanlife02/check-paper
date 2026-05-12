use anyhow::{Result, anyhow};
use serde_json::Value;
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use crate::retrieval::embedding::OpenAiCompatibleEmbeddingClient;
use crate::services::jobs::JobService;
use crate::services::profile::{AuthorProfileLookup, ProfileService};
use crate::services::qa::QaService;
use crate::services::sources::SourcesService;
use crate::services::status::StatusService;
use crate::storage::{AnalysisJobSummary, LibraryStatus, Storage};
use crate::understanding::llm::OpenAiCompatibleClient;

const MAX_TELEGRAM_USER_TEXT_CHARS: usize = 4000;

#[derive(Clone)]
pub struct BotHandlers {
    db_path: PathBuf,
    llm: OpenAiCompatibleClient,
    embedding: Option<OpenAiCompatibleEmbeddingClient>,
    default_author: Option<String>,
    chat_authors: Arc<Mutex<HashMap<i64, String>>>,
}

impl BotHandlers {
    pub fn new(
        db_path: PathBuf,
        llm: OpenAiCompatibleClient,
        embedding: Option<OpenAiCompatibleEmbeddingClient>,
        default_author: Option<String>,
    ) -> Self {
        Self {
            db_path,
            llm,
            embedding,
            default_author,
            chat_authors: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    pub fn handle_text(&self, chat_id: i64, text: &str) -> Result<String> {
        let storage = Storage::open(&self.db_path)?;
        let qa = QaService::new(&storage, self.llm.clone(), self.embedding.clone());
        let stripped = text.trim();
        if stripped.chars().count() > MAX_TELEGRAM_USER_TEXT_CHARS {
            return Ok(format!(
                "消息过长，请控制在 {} 个字符以内。",
                MAX_TELEGRAM_USER_TEXT_CHARS
            ));
        }
        if stripped.starts_with("/start") {
            return Ok(start_message());
        }
        if stripped.starts_with("/use_author") {
            let author = stripped.trim_start_matches("/use_author").trim();
            if author.is_empty() {
                return Ok("请指定作者，例如 `/use_author Ruqiang ZOU`。".to_string());
            }
            self.set_chat_author(chat_id, author)?;
            return Ok(format!("当前 chat 默认作者已设置为：{author}"));
        }
        if stripped.starts_with("/current_author") {
            return Ok(self
                .chat_author(chat_id)
                .map(|author| format!("当前默认作者：{author}"))
                .unwrap_or_else(|| "当前 chat 没有默认作者。".to_string()));
        }
        if stripped.starts_with("/profile") {
            let author = stripped.trim_start_matches("/profile").trim();
            let configured_author = self.chat_author(chat_id);
            let author = if author.is_empty() {
                configured_author.as_deref()
            } else {
                Some(author)
            };
            return self.profile(&storage, author);
        }
        if stripped.starts_with("/sources") {
            let (author, full) =
                self.parse_sources_args(chat_id, stripped.trim_start_matches("/sources").trim());
            return self.sources(&storage, author.as_deref(), full);
        }
        if stripped.starts_with("/status") {
            let (author, detail) =
                self.parse_status_args(chat_id, stripped.trim_start_matches("/status").trim());
            return self.status(&storage, author.as_deref(), detail);
        }
        if stripped.starts_with("/jobs") {
            let (author, status) =
                self.parse_jobs_args(chat_id, stripped.trim_start_matches("/jobs").trim());
            return self.jobs(&storage, author.as_deref(), status.as_deref());
        }
        if stripped.starts_with("/cancel") {
            return self.cancel_job(&storage, stripped.trim_start_matches("/cancel").trim());
        }
        if stripped.starts_with("/ask") {
            let body = stripped.trim_start_matches("/ask").trim();
            let (author, question) = self.parse_author_question(chat_id, body)?;
            return qa.answer(&author, &question);
        }
        let Some(author) = self.chat_author(chat_id) else {
            return Ok(
                "请先设置 CHECK_PAPER_DEFAULT_AUTHOR，或使用 `/ask 作者 | 问题`。".to_string(),
            );
        };
        qa.answer(&author, stripped)
    }

    pub async fn handle_text_stream<F>(
        &self,
        chat_id: i64,
        text: &str,
        on_delta: F,
    ) -> Result<String>
    where
        F: FnMut(&str) -> Result<()>,
    {
        let storage = Storage::open(&self.db_path)?;
        let qa = QaService::new(&storage, self.llm.clone(), self.embedding.clone());
        let stripped = text.trim();
        if stripped.chars().count() > MAX_TELEGRAM_USER_TEXT_CHARS {
            return Ok(format!(
                "消息过长，请控制在 {} 个字符以内。",
                MAX_TELEGRAM_USER_TEXT_CHARS
            ));
        }
        if stripped.starts_with("/start") {
            return Ok(start_message());
        }
        if stripped.starts_with("/use_author") {
            let author = stripped.trim_start_matches("/use_author").trim();
            if author.is_empty() {
                return Ok("请指定作者，例如 `/use_author Ruqiang ZOU`。".to_string());
            }
            self.set_chat_author(chat_id, author)?;
            return Ok(format!("当前 chat 默认作者已设置为：{author}"));
        }
        if stripped.starts_with("/current_author") {
            return Ok(self
                .chat_author(chat_id)
                .map(|author| format!("当前默认作者：{author}"))
                .unwrap_or_else(|| "当前 chat 没有默认作者。".to_string()));
        }
        if stripped.starts_with("/profile") {
            let author = stripped.trim_start_matches("/profile").trim();
            let configured_author = self.chat_author(chat_id);
            let author = if author.is_empty() {
                configured_author.as_deref()
            } else {
                Some(author)
            };
            return self.profile(&storage, author);
        }
        if stripped.starts_with("/sources") {
            let (author, full) =
                self.parse_sources_args(chat_id, stripped.trim_start_matches("/sources").trim());
            return self.sources(&storage, author.as_deref(), full);
        }
        if stripped.starts_with("/status") {
            let (author, detail) =
                self.parse_status_args(chat_id, stripped.trim_start_matches("/status").trim());
            return self.status(&storage, author.as_deref(), detail);
        }
        if stripped.starts_with("/jobs") {
            let (author, status) =
                self.parse_jobs_args(chat_id, stripped.trim_start_matches("/jobs").trim());
            return self.jobs(&storage, author.as_deref(), status.as_deref());
        }
        if stripped.starts_with("/cancel") {
            return self.cancel_job(&storage, stripped.trim_start_matches("/cancel").trim());
        }
        if stripped.starts_with("/ask") {
            let body = stripped.trim_start_matches("/ask").trim();
            let (author, question) = self.parse_author_question(chat_id, body)?;
            return qa.answer_stream(&author, &question, on_delta).await;
        }
        let Some(author) = self.chat_author(chat_id) else {
            return Ok(
                "请先设置 CHECK_PAPER_DEFAULT_AUTHOR，或使用 `/ask 作者 | 问题`。".to_string(),
            );
        };
        qa.answer_stream(&author, stripped, on_delta).await
    }

    fn profile(&self, storage: &Storage, author: Option<&str>) -> Result<String> {
        let Some(author) = author else {
            return Ok("请指定作者，例如 `/profile Ruqiang ZOU`。".to_string());
        };
        match ProfileService::new(storage).author_profile(author)? {
            AuthorProfileLookup::Found(profile) => Ok(format_profile(&profile)),
            AuthorProfileLookup::Missing { paper_count } => Ok(format!(
                "还没有作者画像。当前已入库 {paper_count} 篇论文；请先运行 analyze 或 sync。"
            )),
        }
    }

    fn sources(&self, storage: &Storage, author: Option<&str>, full: bool) -> Result<String> {
        let Some(answer) = SourcesService::new(storage).latest_answer(author)? else {
            return Ok("还没有可显示的上一轮引用。".to_string());
        };
        Ok(format_sources(&answer, full))
    }

    fn status(&self, storage: &Storage, author: Option<&str>, detail: bool) -> Result<String> {
        let report = StatusService::new(storage).report(author, detail, 5)?;
        Ok(format_status(&report.status, detail, &report.failed_jobs))
    }

    fn jobs(
        &self,
        storage: &Storage,
        author: Option<&str>,
        status: Option<&str>,
    ) -> Result<String> {
        let jobs = JobService::new(storage).list(author, status, 10)?;
        Ok(format_jobs(&jobs, status))
    }

    fn cancel_job(&self, storage: &Storage, job_id: &str) -> Result<String> {
        if job_id.is_empty() {
            return Ok(
                "当前没有可取消的回答。若要取消分析任务，请使用 `/cancel job_id`。".to_string(),
            );
        }
        let job_id = job_id
            .parse::<i64>()
            .map_err(|_| anyhow!("job_id 必须是数字"))?;
        JobService::new(storage).cancel(job_id)?;
        Ok(format!("已取消任务 #{job_id}。"))
    }

    fn parse_author_question(&self, chat_id: i64, body: &str) -> Result<(String, String)> {
        if let Some((author, question)) = body.split_once('|') {
            let author = author.trim();
            let question = question.trim();
            if !author.is_empty() && !question.is_empty() {
                return Ok((author.to_string(), question.to_string()));
            }
        }
        let Some(author) = self.chat_author(chat_id) else {
            return Err(anyhow!(
                "请使用 `/ask 作者 | 问题`，或设置 CHECK_PAPER_DEFAULT_AUTHOR。"
            ));
        };
        if body.trim().is_empty() {
            return Err(anyhow!("请在 /ask 后输入问题。"));
        }
        Ok((author, body.trim().to_string()))
    }

    fn parse_sources_args(&self, chat_id: i64, body: &str) -> (Option<String>, bool) {
        let body = body.trim();
        let (full, author) = if body == "full" || body == "--full" {
            (true, "")
        } else if let Some(author) = body.strip_prefix("full ") {
            (true, author.trim())
        } else if let Some(author) = body.strip_prefix("--full ") {
            (true, author.trim())
        } else {
            (false, body)
        };
        let author = if author.is_empty() {
            self.chat_author(chat_id)
        } else {
            Some(author.to_string())
        };
        (author, full)
    }

    fn parse_status_args(&self, chat_id: i64, body: &str) -> (Option<String>, bool) {
        let body = body.trim();
        let (detail, author) = if body == "detail" || body == "--detail" {
            (true, "")
        } else if let Some(author) = body.strip_prefix("detail ") {
            (true, author.trim())
        } else if let Some(author) = body.strip_prefix("--detail ") {
            (true, author.trim())
        } else {
            (false, body)
        };
        let author = if author.is_empty() {
            self.chat_author(chat_id)
        } else {
            Some(author.to_string())
        };
        (author, detail)
    }

    fn parse_jobs_args(&self, chat_id: i64, body: &str) -> (Option<String>, Option<String>) {
        let body = body.trim();
        if body.is_empty() {
            return (self.chat_author(chat_id), None);
        }
        let mut parts = body.split_whitespace();
        let first = parts.next().unwrap_or_default();
        if let Some(status) = normalize_job_status(first) {
            let author = body
                .strip_prefix(first)
                .map(str::trim)
                .filter(|author| !author.is_empty())
                .map(str::to_string)
                .or_else(|| self.chat_author(chat_id));
            return (author, Some(status.to_string()));
        }
        (Some(body.to_string()), None)
    }

    fn set_chat_author(&self, chat_id: i64, author: &str) -> Result<()> {
        let mut authors = self
            .chat_authors
            .lock()
            .map_err(|_| anyhow!("chat author state is unavailable"))?;
        authors.insert(chat_id, author.to_string());
        Ok(())
    }

    fn chat_author(&self, chat_id: i64) -> Option<String> {
        self.chat_authors
            .lock()
            .ok()
            .and_then(|authors| authors.get(&chat_id).cloned())
            .or_else(|| self.default_author.clone())
    }
}

fn start_message() -> String {
    "check-paper 已启动。\n用法：\n/use_author Ruqiang ZOU\n/current_author\n/profile\n/status\n/status detail\n/jobs\n/jobs failed\n/sources\n/sources full\n/cancel\n/cancel job_id\n/ask 你的问题\n/ask Ruqiang ZOU | 你的问题".to_string()
}

fn format_status(
    status: &LibraryStatus,
    detail: bool,
    failed_jobs: &[AnalysisJobSummary],
) -> String {
    let latency = status
        .avg_qa_latency_ms
        .map(|value| format!("\nQA 平均延迟：{value:.0} ms"))
        .unwrap_or_default();
    let tokens = status
        .total_qa_tokens
        .map(|value| format!("\nQA 总 tokens：{value}"))
        .unwrap_or_default();
    let cost = status
        .total_qa_cost_usd
        .map(|value| format!("\nQA 总成本：${value:.6}"))
        .unwrap_or_default();
    let mut text = format!(
        "论文数：{}\n已分析：{}\n待分析/过期：{}\n队列任务：{}\n运行任务：{}\n等待重试：{}\n失败任务：{}\n已取消：{}\nQA 日志：{}{}{}{}",
        status.papers,
        status.analyzed,
        status.stale_papers,
        status.queued_jobs,
        status.running_jobs,
        status.retry_waiting_jobs,
        status.failed_jobs,
        status.cancelled_jobs,
        status.qa_logs,
        latency,
        tokens,
        cost
    );
    if detail {
        text.push_str("\n\n");
        text.push_str(&format_jobs(failed_jobs, Some("failed")));
    }
    text
}

fn format_jobs(jobs: &[AnalysisJobSummary], status: Option<&str>) -> String {
    if jobs.is_empty() {
        return status
            .map(|status| format!("当前没有 {status} 任务。"))
            .unwrap_or_else(|| "当前没有任务。".to_string());
    }
    let header = status
        .map(|status| format!("最近 {status} 任务："))
        .unwrap_or_else(|| "最近任务：".to_string());
    let mut lines = vec![header];
    for job in jobs {
        lines.push(format!(
            "#{} [{}] {} {} model={} updated={}",
            job.id,
            job.status,
            job.job_type,
            job.paper_key.as_deref().unwrap_or("-"),
            job.model_id.as_deref().unwrap_or("-"),
            job.updated_at
        ));
        if let Some(error_code) = job.error_code.as_deref() {
            lines.push(format!("  error_code={error_code}"));
        }
    }
    lines.join("\n")
}

fn normalize_job_status(value: &str) -> Option<&'static str> {
    match value.to_lowercase().as_str() {
        "queued" => Some("queued"),
        "running" => Some("running"),
        "failed" => Some("failed"),
        "succeeded" => Some("succeeded"),
        "cancelled" | "canceled" => Some("cancelled"),
        "retry" | "retry_waiting" | "retry-waiting" => Some("retry_waiting"),
        "stale" => Some("stale"),
        _ => None,
    }
}

fn format_sources(answer: &Value, full: bool) -> String {
    if let Some(snapshot) = answer.get("evidence_snapshot").and_then(Value::as_array) {
        if !snapshot.is_empty() {
            let mut lines = vec!["上一轮依据：".to_string()];
            for (index, item) in snapshot.iter().enumerate() {
                lines.push(format!(
                    "[{}] {} {} {} section={} chunk={} hash={}",
                    index + 1,
                    item.get("year").and_then(Value::as_str).unwrap_or(""),
                    item.get("title").and_then(Value::as_str).unwrap_or(""),
                    item.get("doi").and_then(Value::as_str).unwrap_or(""),
                    item.get("section").and_then(Value::as_str).unwrap_or(""),
                    item.get("chunk_id")
                        .and_then(Value::as_i64)
                        .unwrap_or_default(),
                    item.get("source_hash")
                        .and_then(Value::as_str)
                        .unwrap_or("")
                        .chars()
                        .take(12)
                        .collect::<String>()
                ));
                if full {
                    lines.push(format!(
                        "  chunk_index={} chunk_hash={} chunker={}",
                        item.get("chunk_index")
                            .and_then(Value::as_i64)
                            .unwrap_or_default(),
                        item.get("chunk_hash")
                            .and_then(Value::as_str)
                            .unwrap_or("")
                            .chars()
                            .take(12)
                            .collect::<String>(),
                        item.get("chunker_version")
                            .and_then(Value::as_str)
                            .unwrap_or("")
                    ));
                    if let Some(excerpt) = item.get("text_excerpt").and_then(Value::as_str) {
                        if !excerpt.trim().is_empty() {
                            lines.push(format!("  {}", excerpt.trim()));
                        }
                    }
                }
            }
            return lines.join("\n");
        }
    }
    let Some(evidence) = answer.get("evidence").and_then(Value::as_array) else {
        return "上一轮回答没有结构化 evidence。".to_string();
    };
    if evidence.is_empty() {
        return "上一轮回答没有引用来源。".to_string();
    }
    let mut lines = vec!["上一轮依据：".to_string()];
    for (index, item) in evidence.iter().enumerate() {
        lines.push(format!(
            "[{}] {} {} {} section={} chunk={}",
            index + 1,
            item.get("year").and_then(Value::as_str).unwrap_or(""),
            item.get("title").and_then(Value::as_str).unwrap_or(""),
            item.get("doi").and_then(Value::as_str).unwrap_or(""),
            item.get("section").and_then(Value::as_str).unwrap_or(""),
            item.get("chunk_id")
                .and_then(Value::as_i64)
                .unwrap_or_default()
        ));
    }
    lines.join("\n")
}

fn format_profile(profile: &Value) -> String {
    if let Some(areas) = profile.get("research_areas").and_then(Value::as_array) {
        let mut lines = vec![format!(
            "作者：{}",
            profile.get("author").and_then(Value::as_str).unwrap_or("")
        )];
        if !areas.is_empty() {
            let area_text = areas
                .iter()
                .take(8)
                .filter_map(Value::as_str)
                .collect::<Vec<_>>()
                .join("；");
            lines.push(format!("研究方向：{area_text}"));
        }
        if let Some(works) = profile
            .get("representative_works")
            .and_then(Value::as_array)
        {
            if !works.is_empty() {
                lines.push("代表论文：".to_string());
                for work in works.iter().take(6) {
                    lines.push(format!(
                        "- {} {} {}",
                        work.get("year").and_then(Value::as_str).unwrap_or(""),
                        work.get("title").and_then(Value::as_str).unwrap_or(""),
                        work.get("doi").and_then(Value::as_str).unwrap_or("")
                    ));
                }
            }
        }
        lines.join("\n")
    } else {
        let text = serde_json::to_string_pretty(profile).unwrap_or_else(|_| profile.to_string());
        text.chars().take(3500).collect()
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::{format_jobs, format_sources, format_status, normalize_job_status};
    use crate::storage::{AnalysisJobSummary, LibraryStatus};

    #[test]
    fn formats_structured_sources() {
        let text = format_sources(
            &json!({
                "answer": "ok",
                "evidence": [{
                    "paper_key": "Alice/paper-a",
                    "title": "A Paper",
                    "doi": "10.1/test",
                    "year": "2024",
                    "chunk_id": 7,
                    "section": "Methods"
                }]
            }),
            false,
        );

        assert!(text.contains("上一轮依据："));
        assert!(text.contains("[1] 2024 A Paper 10.1/test section=Methods chunk=7"));
    }

    #[test]
    fn explains_missing_structured_sources() {
        assert_eq!(
            format_sources(&json!({ "answer": "plain" }), false),
            "上一轮回答没有结构化 evidence。"
        );
    }

    #[test]
    fn formats_snapshot_sources_full() {
        let text = format_sources(
            &json!({
                "evidence_snapshot": [{
                    "paper_key": "Alice/paper-a",
                    "title": "A Paper",
                    "doi": "10.1/test",
                    "year": "2024",
                    "chunk_id": 7,
                    "chunk_index": 3,
                    "section": "Methods",
                    "source_hash": "source-hash-abcdef",
                    "chunk_hash": "chunk-hash-abcdef",
                    "chunker_version": "section-char-v1",
                    "text_excerpt": "Detailed source excerpt."
                }]
            }),
            true,
        );

        assert!(text.contains("chunk_index=3"));
        assert!(text.contains("chunk_hash=chunk-hash-a"));
        assert!(text.contains("chunker=section-char-v1"));
        assert!(text.contains("Detailed source excerpt."));
    }

    #[test]
    fn formats_filtered_jobs() {
        let text = format_jobs(
            &[AnalysisJobSummary {
                id: 3,
                paper_key: Some("Alice/paper-a".to_string()),
                job_type: "analyze".to_string(),
                status: "failed".to_string(),
                error_code: Some("schema_error".to_string()),
                error: None,
                model_id: Some("model-a".to_string()),
                updated_at: "2026-05-12 12:00:00".to_string(),
            }],
            Some("failed"),
        );

        assert!(text.contains("最近 failed 任务："));
        assert!(text.contains("#3 [failed] analyze Alice/paper-a model=model-a"));
        assert!(text.contains("error_code=schema_error"));
        assert_eq!(format_jobs(&[], Some("queued")), "当前没有 queued 任务。");
    }

    #[test]
    fn normalizes_job_status_aliases() {
        assert_eq!(normalize_job_status("retry"), Some("retry_waiting"));
        assert_eq!(normalize_job_status("canceled"), Some("cancelled"));
        assert_eq!(normalize_job_status("unknown"), None);
    }

    #[test]
    fn formats_status_detail_with_failed_jobs() {
        let text = format_status(
            &LibraryStatus {
                papers: 5,
                analyzed: 4,
                stale_papers: 1,
                failed_jobs: 1,
                queued_jobs: 2,
                running_jobs: 0,
                retry_waiting_jobs: 1,
                cancelled_jobs: 0,
                qa_logs: 3,
                avg_qa_latency_ms: Some(1250.0),
                total_qa_tokens: Some(2400),
                total_qa_cost_usd: Some(0.012345),
            },
            true,
            &[AnalysisJobSummary {
                id: 9,
                paper_key: Some("Alice/paper-a".to_string()),
                job_type: "analyze".to_string(),
                status: "failed".to_string(),
                error_code: Some("schema_error".to_string()),
                error: None,
                model_id: Some("model-a".to_string()),
                updated_at: "2026-05-12 12:00:00".to_string(),
            }],
        );

        assert!(text.contains("论文数：5"));
        assert!(text.contains("QA 平均延迟：1250 ms"));
        assert!(text.contains("QA 总 tokens：2400"));
        assert!(text.contains("QA 总成本：$0.012345"));
        assert!(text.contains("最近 failed 任务："));
        assert!(text.contains("#9 [failed] analyze Alice/paper-a"));
    }
}
