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
use crate::storage::{AnalysisJobSummary, AuthorSummary, LibraryStatus, Storage};
use crate::understanding::llm::OpenAiCompatibleClient;

const MAX_TELEGRAM_USER_TEXT_CHARS: usize = 4000;
const AUTHOR_CHOICE_LIMIT: usize = 30;

#[derive(Clone)]
pub struct BotHandlers {
    db_path: PathBuf,
    llm: OpenAiCompatibleClient,
    embedding: Option<OpenAiCompatibleEmbeddingClient>,
    default_author: Option<String>,
    chat_authors: Arc<Mutex<HashMap<i64, String>>>,
    pending_author_choices: Arc<Mutex<HashMap<i64, PendingAuthorSelection>>>,
}

#[derive(Clone)]
struct PendingAuthorSelection {
    authors: Vec<String>,
    action: PendingAuthorAction,
}

#[derive(Clone)]
enum PendingAuthorAction {
    SetDefault,
    Ask { question: String },
}

enum AuthorSelectionOutcome {
    Message(String),
    Ask { author: String, question: String },
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
            pending_author_choices: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    pub fn handle_text(&self, chat_id: i64, text: &str) -> Result<String> {
        let storage = Storage::open(&self.db_path)?;
        let qa = QaService::new(&storage, self.llm.clone(), self.embedding.clone());
        let stripped = text.trim();
        if stripped.chars().count() > MAX_TELEGRAM_USER_TEXT_CHARS {
            return Ok(format!(
                "消息过长，请控制在 {MAX_TELEGRAM_USER_TEXT_CHARS} 个字符以内。"
            ));
        }
        if let Some(outcome) = self.resolve_pending_author_selection(chat_id, stripped)? {
            return self.reply_for_author_selection(&qa, outcome);
        }
        if stripped.starts_with("/start") {
            return Ok(start_message());
        }
        if stripped.starts_with("/help") {
            return Ok(help_message());
        }
        if stripped.starts_with("/authors") {
            let outcome =
                self.start_author_selection(&storage, chat_id, PendingAuthorAction::SetDefault)?;
            return self.reply_for_author_selection(&qa, outcome);
        }
        if stripped.starts_with("/use_author") {
            let author = stripped.trim_start_matches("/use_author").trim();
            if author.is_empty() {
                let outcome = self.start_author_selection(
                    &storage,
                    chat_id,
                    PendingAuthorAction::SetDefault,
                )?;
                return self.reply_for_author_selection(&qa, outcome);
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
            let outcome = self.resolve_author_question(&storage, chat_id, body)?;
            return self.reply_for_author_selection(&qa, outcome);
        }
        let outcome = self.resolve_plain_question(&storage, chat_id, stripped)?;
        self.reply_for_author_selection(&qa, outcome)
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
                "消息过长，请控制在 {MAX_TELEGRAM_USER_TEXT_CHARS} 个字符以内。"
            ));
        }
        if let Some(outcome) = self.resolve_pending_author_selection(chat_id, stripped)? {
            return self
                .reply_for_author_selection_stream(&qa, outcome, on_delta)
                .await;
        }
        if stripped.starts_with("/start") {
            return Ok(start_message());
        }
        if stripped.starts_with("/help") {
            return Ok(help_message());
        }
        if stripped.starts_with("/authors") {
            let outcome =
                self.start_author_selection(&storage, chat_id, PendingAuthorAction::SetDefault)?;
            return self
                .reply_for_author_selection_stream(&qa, outcome, on_delta)
                .await;
        }
        if stripped.starts_with("/use_author") {
            let author = stripped.trim_start_matches("/use_author").trim();
            if author.is_empty() {
                let outcome = self.start_author_selection(
                    &storage,
                    chat_id,
                    PendingAuthorAction::SetDefault,
                )?;
                return self
                    .reply_for_author_selection_stream(&qa, outcome, on_delta)
                    .await;
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
            let outcome = self.resolve_author_question(&storage, chat_id, body)?;
            return self
                .reply_for_author_selection_stream(&qa, outcome, on_delta)
                .await;
        }
        let outcome = self.resolve_plain_question(&storage, chat_id, stripped)?;
        self.reply_for_author_selection_stream(&qa, outcome, on_delta)
            .await
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

    fn reply_for_author_selection(
        &self,
        qa: &QaService<'_>,
        outcome: AuthorSelectionOutcome,
    ) -> Result<String> {
        match outcome {
            AuthorSelectionOutcome::Message(message) => Ok(message),
            AuthorSelectionOutcome::Ask { author, question } => qa.answer(&author, &question),
        }
    }

    async fn reply_for_author_selection_stream<F>(
        &self,
        qa: &QaService<'_>,
        outcome: AuthorSelectionOutcome,
        on_delta: F,
    ) -> Result<String>
    where
        F: FnMut(&str) -> Result<()>,
    {
        match outcome {
            AuthorSelectionOutcome::Message(message) => Ok(message),
            AuthorSelectionOutcome::Ask { author, question } => {
                qa.answer_stream(&author, &question, on_delta).await
            }
        }
    }

    fn resolve_author_question(
        &self,
        storage: &Storage,
        chat_id: i64,
        body: &str,
    ) -> Result<AuthorSelectionOutcome> {
        if let Some((author, question)) = body.split_once('|') {
            let author = author.trim();
            let question = question.trim();
            if !author.is_empty() && !question.is_empty() {
                return Ok(AuthorSelectionOutcome::Ask {
                    author: author.to_string(),
                    question: question.to_string(),
                });
            }
            return Err(anyhow!("请使用 `/ask 作者 | 问题`。"));
        }
        if body.trim().is_empty() {
            return Err(anyhow!("请在 /ask 后输入问题。"));
        }
        if let Some(author) = self.chat_author(chat_id) {
            return Ok(AuthorSelectionOutcome::Ask {
                author,
                question: body.trim().to_string(),
            });
        }
        self.start_author_selection(
            storage,
            chat_id,
            PendingAuthorAction::Ask {
                question: body.trim().to_string(),
            },
        )
    }

    fn resolve_plain_question(
        &self,
        storage: &Storage,
        chat_id: i64,
        question: &str,
    ) -> Result<AuthorSelectionOutcome> {
        if let Some(author) = self.chat_author(chat_id) {
            return Ok(AuthorSelectionOutcome::Ask {
                author,
                question: question.to_string(),
            });
        }
        self.start_author_selection(
            storage,
            chat_id,
            PendingAuthorAction::Ask {
                question: question.to_string(),
            },
        )
    }

    fn start_author_selection(
        &self,
        storage: &Storage,
        chat_id: i64,
        action: PendingAuthorAction,
    ) -> Result<AuthorSelectionOutcome> {
        let mut summaries = storage.authors()?;
        if summaries.is_empty() {
            return Ok(AuthorSelectionOutcome::Message(
                "还没有入库作者。请先运行 `ppc ingest --author ...` 或 `ppc sync --author ...`。"
                    .to_string(),
            ));
        }
        let has_more = summaries.len() > AUTHOR_CHOICE_LIMIT;
        summaries.truncate(AUTHOR_CHOICE_LIMIT);
        if summaries.len() == 1 {
            let author = summaries[0].author.clone();
            self.set_chat_author(chat_id, &author)?;
            return Ok(match action {
                PendingAuthorAction::SetDefault => {
                    AuthorSelectionOutcome::Message(format!("当前 chat 默认作者已设置为：{author}"))
                }
                PendingAuthorAction::Ask { question } => {
                    AuthorSelectionOutcome::Ask { author, question }
                }
            });
        }

        let authors = summaries
            .iter()
            .map(|summary| summary.author.clone())
            .collect::<Vec<_>>();
        let mut pending = self
            .pending_author_choices
            .lock()
            .map_err(|_| anyhow!("pending author selection state is unavailable"))?;
        pending.insert(
            chat_id,
            PendingAuthorSelection {
                authors,
                action: action.clone(),
            },
        );
        Ok(AuthorSelectionOutcome::Message(format_author_choices(
            &summaries, &action, has_more,
        )))
    }

    fn resolve_pending_author_selection(
        &self,
        chat_id: i64,
        text: &str,
    ) -> Result<Option<AuthorSelectionOutcome>> {
        let Some(index) = parse_author_selection(text) else {
            return Ok(None);
        };
        let selection = {
            let pending = self
                .pending_author_choices
                .lock()
                .map_err(|_| anyhow!("pending author selection state is unavailable"))?;
            pending.get(&chat_id).cloned()
        };
        let Some(selection) = selection else {
            return Ok(None);
        };
        if index >= selection.authors.len() {
            return Ok(Some(AuthorSelectionOutcome::Message(format!(
                "请选择 1-{} 之间的序号。",
                selection.authors.len()
            ))));
        }

        {
            let mut pending = self
                .pending_author_choices
                .lock()
                .map_err(|_| anyhow!("pending author selection state is unavailable"))?;
            pending.remove(&chat_id);
        }
        let author = selection.authors[index].clone();
        self.set_chat_author(chat_id, &author)?;
        Ok(Some(match selection.action {
            PendingAuthorAction::SetDefault => {
                AuthorSelectionOutcome::Message(format!("当前 chat 默认作者已设置为：{author}"))
            }
            PendingAuthorAction::Ask { question } => {
                AuthorSelectionOutcome::Ask { author, question }
            }
        }))
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
        let mut pending = self
            .pending_author_choices
            .lock()
            .map_err(|_| anyhow!("pending author selection state is unavailable"))?;
        pending.remove(&chat_id);
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
    "check-paper 已启动。使用 /help 查看可用命令。".to_string()
}

fn help_message() -> String {
    [
        "Available commands:",
        "/help - Show this command list.",
        "/start - Confirm the bot is running.",
        "/authors - List available authors and select one by number.",
        "/use_author [NAME] - Set or choose the default author for this chat.",
        "/current_author - Show the current default author.",
        "/profile [AUTHOR] - Show the author profile.",
        "/status [detail] [AUTHOR] - Show library and job status.",
        "/jobs [STATUS] [AUTHOR] - Show recent analysis jobs.",
        "/sources [full] - Show sources from the last answer.",
        "/cancel - Cancel the current answer.",
        "/cancel JOB_ID - Cancel an analysis job.",
        "/ask QUESTION - Ask about the current author.",
        "/ask AUTHOR | QUESTION - Ask about a specific author.",
    ]
    .join("\n")
}

fn format_author_choices(
    summaries: &[AuthorSummary],
    action: &PendingAuthorAction,
    has_more: bool,
) -> String {
    let mut lines = match action {
        PendingAuthorAction::SetDefault => {
            vec!["请选择当前 chat 的默认作者：".to_string()]
        }
        PendingAuthorAction::Ask { .. } => {
            vec!["请选择作者；选中后我会继续回答刚才的问题：".to_string()]
        }
    };
    for (index, summary) in summaries.iter().enumerate() {
        lines.push(format!(
            "{}. {} ({} papers)",
            index + 1,
            summary.author,
            summary.paper_count
        ));
    }
    if has_more {
        lines.push(format!("仅显示前 {AUTHOR_CHOICE_LIMIT} 位作者。"));
    }
    lines.push(
        "回复序号选择作者，例如 `1`；群聊中也需要 @ bot，例如 `@你的Bot用户名 1`。".to_string(),
    );
    lines.join("\n")
}

fn parse_author_selection(text: &str) -> Option<usize> {
    let text = text.trim();
    if text.starts_with('/') {
        return None;
    }
    let number = text.parse::<usize>().ok()?;
    number.checked_sub(1)
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
    use std::collections::BTreeMap;

    use serde_json::json;
    use tempfile::tempdir;

    use super::{
        BotHandlers, PendingAuthorAction, format_author_choices, format_jobs, format_sources,
        format_status, help_message, normalize_job_status, parse_author_selection,
    };
    use crate::papers::models::Paper;
    use crate::storage::{AnalysisJobSummary, AuthorSummary, LibraryStatus};
    use crate::understanding::llm::{LlmConfig, OpenAiCompatibleClient};

    #[test]
    fn help_lists_available_commands_with_purpose() {
        let text = help_message();

        assert!(text.contains("Available commands:"));
        assert!(text.contains("/help - Show this command list."));
        assert!(text.contains("/authors - List available authors and select one by number."));
        assert!(text.contains("/use_author [NAME] - Set or choose the default author"));
        assert!(text.contains("/status [detail] [AUTHOR] - Show library and job status."));
        assert!(text.contains("/ask AUTHOR | QUESTION - Ask about a specific author."));
    }

    #[test]
    fn formats_author_choices_with_numbered_selection() {
        let text = format_author_choices(
            &[
                AuthorSummary {
                    author: "Alice".to_string(),
                    paper_count: 5,
                },
                AuthorSummary {
                    author: "Bob".to_string(),
                    paper_count: 2,
                },
            ],
            &PendingAuthorAction::SetDefault,
            false,
        );

        assert!(text.contains("请选择当前 chat 的默认作者："));
        assert!(text.contains("1. Alice (5 papers)"));
        assert!(text.contains("2. Bob (2 papers)"));
        assert!(text.contains("回复序号选择作者"));
    }

    #[test]
    fn parses_author_number_selection() {
        assert_eq!(parse_author_selection("1"), Some(0));
        assert_eq!(parse_author_selection(" 2 "), Some(1));
        assert_eq!(parse_author_selection("/status"), None);
        assert_eq!(parse_author_selection("Alice"), None);
        assert_eq!(parse_author_selection("0"), None);
    }

    #[test]
    fn use_author_without_argument_lists_and_accepts_number_selection() {
        let dir = tempdir().unwrap();
        let db_path = dir.path().join("test.sqlite");
        let mut storage = crate::storage::Storage::open(&db_path).unwrap();
        storage
            .upsert_paper(&test_paper(dir.path(), "Alice", "paper-a"), &[])
            .unwrap();
        storage
            .upsert_paper(&test_paper(dir.path(), "Bob", "paper-a"), &[])
            .unwrap();
        drop(storage);
        let handlers = BotHandlers::new(db_path, test_llm(), None, None);

        let prompt = handlers.handle_text(7, "/use_author").unwrap();
        let selected = handlers.handle_text(7, "2").unwrap();
        let current = handlers.handle_text(7, "/current_author").unwrap();

        assert!(prompt.contains("1. Alice (1 papers)"));
        assert!(prompt.contains("2. Bob (1 papers)"));
        assert_eq!(selected, "当前 chat 默认作者已设置为：Bob");
        assert_eq!(current, "当前默认作者：Bob");
    }

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

    fn test_llm() -> OpenAiCompatibleClient {
        OpenAiCompatibleClient::new(LlmConfig {
            base_url: "http://127.0.0.1:1/v1".to_string(),
            api_key: None,
            model: "test-model".to_string(),
            proxy: None,
            timeout_secs: 1,
            tls_backend: "rustls".to_string(),
            prompt_cost_per_1k: None,
            completion_cost_per_1k: None,
        })
        .unwrap()
    }

    fn test_paper(root: &std::path::Path, author: &str, paper_id: &str) -> Paper {
        Paper {
            author: author.to_string(),
            paper_id: paper_id.to_string(),
            paper_dir: root.join(author).join(paper_id),
            article_path: root.join(author).join(paper_id).join("article.md"),
            fetch_result_path: None,
            source_hash: format!("{author}-{paper_id}-hash"),
            metadata: BTreeMap::from([
                ("title".to_string(), format!("{author} {paper_id}")),
                ("year".to_string(), "2024".to_string()),
            ]),
            fetch_result: json!({}),
            raw_body: String::new(),
            clean_text: String::new(),
            sections: vec![],
        }
    }
}
