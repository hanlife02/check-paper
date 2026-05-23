use anyhow::{Result, anyhow};
use serde_json::Value;
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use crate::retrieval::embedding::OpenAiCompatibleEmbeddingClient;
use crate::services::analysis::{AnalysisQueueOptions, AnalysisQueuePlan, AnalysisService};
use crate::services::comprehension::{
    ComprehensionService, S3ComprehensionOptions, S3ComprehensionReport,
    S4AuthorComprehensionOptions, S4AuthorComprehensionReport,
};
use crate::services::embedding::{EmbeddingRunOptions, EmbeddingRunReport, EmbeddingService};
use crate::services::jobs::JobService;
use crate::services::profile::{AuthorProfileLookup, AuthorProfileRebuild, ProfileService};
use crate::services::qa::{QaProfileVersionPreference, QaService};
use crate::services::sources::SourcesService;
use crate::services::status::StatusService;
use crate::services::sync::{SyncRunOptions, SyncRunReport, SyncService};
use crate::storage::{
    AnalysisJobSummary, AuthorSummary, LibraryStatus, NewTelegramDeliveryLog,
    NewTelegramPendingAuthorSelection, Storage, TelegramPendingAuthorSelection,
};
use crate::understanding::llm::OpenAiCompatibleClient;

const MAX_TELEGRAM_USER_TEXT_CHARS: usize = 4000;
const AUTHOR_CHOICE_LIMIT: usize = 30;
const PENDING_ACTION_SET_DEFAULT: &str = "set_default";
const PENDING_ACTION_ASK: &str = "ask";
const DEFAULT_CHUNKER_VERSION: &str = "section-char-v1";
const DEFAULT_CHUNK_MAX_CHARS: usize = 3200;
const DEFAULT_CHUNK_OVERLAP: usize = 350;
pub const TELEGRAM_HEARTBEAT_NAME: &str = "telegram_polling";

#[derive(Clone)]
pub struct BotHandlers {
    db_path: PathBuf,
    llm: OpenAiCompatibleClient,
    embedding: Option<OpenAiCompatibleEmbeddingClient>,
    default_author: Option<String>,
    paper_root: Option<PathBuf>,
    chunker_version: String,
    chunk_max_chars: usize,
    chunk_overlap: usize,
    qa_profile_version: QaProfileVersionPreference,
    chat_authors: Arc<Mutex<HashMap<i64, String>>>,
}

#[derive(Clone)]
pub struct BotRuntimeSettings {
    pub paper_root: Option<PathBuf>,
    pub chunker_version: String,
    pub chunk_max_chars: usize,
    pub chunk_overlap: usize,
    pub qa_profile_version: QaProfileVersionPreference,
}

impl Default for BotRuntimeSettings {
    fn default() -> Self {
        Self {
            paper_root: None,
            chunker_version: DEFAULT_CHUNKER_VERSION.to_string(),
            chunk_max_chars: DEFAULT_CHUNK_MAX_CHARS,
            chunk_overlap: DEFAULT_CHUNK_OVERLAP,
            qa_profile_version: QaProfileVersionPreference::V1,
        }
    }
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
        Self::new_with_chunker_version(
            db_path,
            llm,
            embedding,
            default_author,
            DEFAULT_CHUNKER_VERSION.to_string(),
        )
    }

    pub fn new_with_chunker_version(
        db_path: PathBuf,
        llm: OpenAiCompatibleClient,
        embedding: Option<OpenAiCompatibleEmbeddingClient>,
        default_author: Option<String>,
        chunker_version: String,
    ) -> Self {
        let runtime_settings = BotRuntimeSettings {
            chunker_version,
            ..BotRuntimeSettings::default()
        };
        Self::new_with_runtime_settings(db_path, llm, embedding, default_author, runtime_settings)
    }

    pub fn new_with_runtime_settings(
        db_path: PathBuf,
        llm: OpenAiCompatibleClient,
        embedding: Option<OpenAiCompatibleEmbeddingClient>,
        default_author: Option<String>,
        runtime_settings: BotRuntimeSettings,
    ) -> Self {
        Self {
            db_path,
            llm,
            embedding,
            default_author,
            paper_root: runtime_settings.paper_root,
            chunker_version: runtime_settings.chunker_version,
            chunk_max_chars: runtime_settings.chunk_max_chars,
            chunk_overlap: runtime_settings.chunk_overlap,
            qa_profile_version: runtime_settings.qa_profile_version,
            chat_authors: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    pub fn handle_text(&self, chat_id: i64, text: &str) -> Result<String> {
        let storage = Storage::open(&self.db_path)?;
        let stripped = text.trim();
        if stripped.chars().count() > MAX_TELEGRAM_USER_TEXT_CHARS {
            return Ok(format!(
                "消息过长，请控制在 {MAX_TELEGRAM_USER_TEXT_CHARS} 个字符以内。"
            ));
        }
        if let Some(outcome) = self.resolve_pending_author_selection(&storage, chat_id, stripped)? {
            return self.reply_for_author_selection(&storage, outcome);
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
            return self.reply_for_author_selection(&storage, outcome);
        }
        if stripped.starts_with("/use_author") {
            let author = stripped.trim_start_matches("/use_author").trim();
            if author.is_empty() {
                let outcome = self.start_author_selection(
                    &storage,
                    chat_id,
                    PendingAuthorAction::SetDefault,
                )?;
                return self.reply_for_author_selection(&storage, outcome);
            }
            self.set_chat_author(&storage, chat_id, author)?;
            return Ok(format!("当前 chat 默认作者已设置为：{author}"));
        }
        if stripped.starts_with("/current_author") {
            return Ok(self
                .chat_author(&storage, chat_id)?
                .map(|author| format!("当前默认作者：{author}"))
                .unwrap_or_else(|| "当前 chat 没有默认作者。".to_string()));
        }
        if let Some(body) = command_body(stripped, "/profile") {
            let args = parse_telegram_profile_args(body);
            if args.rebuild {
                return self.rebuild_profile(&storage, chat_id, args.author);
            }
            let configured_author = self.chat_author(&storage, chat_id)?;
            let author = args.author.as_deref().or(configured_author.as_deref());
            return self.profile(&storage, author);
        }
        if let Some(body) = rebuild_profile_command_body(stripped) {
            return self.rebuild_profile(
                &storage,
                chat_id,
                non_empty_trimmed(body).map(str::to_string),
            );
        }
        if let Some(body) = command_body(stripped, "/sync") {
            let args = parse_telegram_sync_args(body)?;
            return self.sync_papers(&storage, chat_id, args);
        }
        if let Some(body) = embed_command_body(stripped) {
            let args = parse_telegram_embed_args(body)?;
            return self.embed_chunks(&storage, chat_id, args);
        }
        if let Some(body) = command_body(stripped, "/analyze") {
            let args = parse_telegram_analyze_args(body)?;
            return self.enqueue_analysis(&storage, chat_id, args);
        }
        if let Some(body) = command_body(stripped, "/comprehend") {
            let args = parse_telegram_comprehend_args(body)?;
            return self.comprehend(&storage, chat_id, args);
        }
        if stripped.starts_with("/sources") {
            let (author, full) = self.parse_sources_args(
                &storage,
                chat_id,
                stripped.trim_start_matches("/sources").trim(),
            )?;
            return self.sources(&storage, author.as_deref(), full);
        }
        if stripped.starts_with("/status") {
            let (author, detail) = self.parse_status_args(
                &storage,
                chat_id,
                stripped.trim_start_matches("/status").trim(),
            )?;
            return self.status(&storage, author.as_deref(), detail);
        }
        if stripped.starts_with("/jobs") {
            let (author, status) = self.parse_jobs_args(
                &storage,
                chat_id,
                stripped.trim_start_matches("/jobs").trim(),
            )?;
            return self.jobs(&storage, author.as_deref(), status.as_deref());
        }
        if stripped.starts_with("/cancel") {
            return self.cancel_job(&storage, stripped.trim_start_matches("/cancel").trim());
        }
        if stripped.starts_with("/ask") {
            let body = stripped.trim_start_matches("/ask").trim();
            let outcome = self.resolve_author_question(&storage, chat_id, body)?;
            return self.reply_for_author_selection(&storage, outcome);
        }
        if let Some(command) = telegram_command_token(stripped) {
            return Ok(unknown_command_message(command));
        }
        let outcome = self.resolve_plain_question(&storage, chat_id, stripped)?;
        self.reply_for_author_selection(&storage, outcome)
    }

    pub async fn handle_text_stream<F>(
        &self,
        chat_id: i64,
        job_id: i64,
        text: &str,
        on_delta: F,
    ) -> Result<String>
    where
        F: FnMut(&str) -> Result<()>,
    {
        let storage = Storage::open(&self.db_path)?;
        let stripped = text.trim();
        if stripped.chars().count() > MAX_TELEGRAM_USER_TEXT_CHARS {
            return Ok(format!(
                "消息过长，请控制在 {MAX_TELEGRAM_USER_TEXT_CHARS} 个字符以内。"
            ));
        }
        if let Some(outcome) = self.resolve_pending_author_selection(&storage, chat_id, stripped)? {
            return self
                .reply_for_author_selection_stream(&storage, chat_id, job_id, outcome, on_delta)
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
                .reply_for_author_selection_stream(&storage, chat_id, job_id, outcome, on_delta)
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
                    .reply_for_author_selection_stream(&storage, chat_id, job_id, outcome, on_delta)
                    .await;
            }
            self.set_chat_author(&storage, chat_id, author)?;
            return Ok(format!("当前 chat 默认作者已设置为：{author}"));
        }
        if stripped.starts_with("/current_author") {
            return Ok(self
                .chat_author(&storage, chat_id)?
                .map(|author| format!("当前默认作者：{author}"))
                .unwrap_or_else(|| "当前 chat 没有默认作者。".to_string()));
        }
        if let Some(body) = command_body(stripped, "/profile") {
            let args = parse_telegram_profile_args(body);
            if args.rebuild {
                return self.rebuild_profile(&storage, chat_id, args.author);
            }
            let configured_author = self.chat_author(&storage, chat_id)?;
            let author = args.author.as_deref().or(configured_author.as_deref());
            return self.profile(&storage, author);
        }
        if let Some(body) = rebuild_profile_command_body(stripped) {
            return self.rebuild_profile(
                &storage,
                chat_id,
                non_empty_trimmed(body).map(str::to_string),
            );
        }
        if let Some(body) = command_body(stripped, "/sync") {
            let args = parse_telegram_sync_args(body)?;
            return self.sync_papers(&storage, chat_id, args);
        }
        if let Some(body) = embed_command_body(stripped) {
            let args = parse_telegram_embed_args(body)?;
            return self.embed_chunks(&storage, chat_id, args);
        }
        if let Some(body) = command_body(stripped, "/analyze") {
            let args = parse_telegram_analyze_args(body)?;
            return self.enqueue_analysis(&storage, chat_id, args);
        }
        if let Some(body) = command_body(stripped, "/comprehend") {
            let args = parse_telegram_comprehend_args(body)?;
            return self.comprehend(&storage, chat_id, args);
        }
        if stripped.starts_with("/sources") {
            let (author, full) = self.parse_sources_args(
                &storage,
                chat_id,
                stripped.trim_start_matches("/sources").trim(),
            )?;
            return self.sources(&storage, author.as_deref(), full);
        }
        if stripped.starts_with("/status") {
            let (author, detail) = self.parse_status_args(
                &storage,
                chat_id,
                stripped.trim_start_matches("/status").trim(),
            )?;
            return self.status(&storage, author.as_deref(), detail);
        }
        if stripped.starts_with("/jobs") {
            let (author, status) = self.parse_jobs_args(
                &storage,
                chat_id,
                stripped.trim_start_matches("/jobs").trim(),
            )?;
            return self.jobs(&storage, author.as_deref(), status.as_deref());
        }
        if stripped.starts_with("/cancel") {
            return self.cancel_job(&storage, stripped.trim_start_matches("/cancel").trim());
        }
        if stripped.starts_with("/ask") {
            let body = stripped.trim_start_matches("/ask").trim();
            let outcome = self.resolve_author_question(&storage, chat_id, body)?;
            return self
                .reply_for_author_selection_stream(&storage, chat_id, job_id, outcome, on_delta)
                .await;
        }
        if let Some(command) = telegram_command_token(stripped) {
            return Ok(unknown_command_message(command));
        }
        let outcome = self.resolve_plain_question(&storage, chat_id, stripped)?;
        self.reply_for_author_selection_stream(&storage, chat_id, job_id, outcome, on_delta)
            .await
    }

    pub fn record_telegram_heartbeat(&self, status: &str) -> Result<()> {
        let storage = Storage::open(&self.db_path)?;
        storage.save_runtime_heartbeat(TELEGRAM_HEARTBEAT_NAME, status)
    }

    pub fn record_telegram_delivery(&self, entry: NewTelegramDeliveryLog<'_>) -> Result<()> {
        let storage = Storage::open(&self.db_path)?;
        storage.save_telegram_delivery_log(entry)
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

    fn rebuild_profile(
        &self,
        storage: &Storage,
        chat_id: i64,
        author: Option<String>,
    ) -> Result<String> {
        let Some(author) = author.or(self.chat_author(storage, chat_id)?) else {
            return Ok("请指定作者，例如 `/profile --rebuild Ruqiang ZOU`。".to_string());
        };
        match ProfileService::new(storage).rebuild_author_profile(&author, &self.llm, true)? {
            AuthorProfileRebuild::NoPaperProfiles => Ok(format!(
                "还没有可用于重建作者画像的论文画像：{author}。请先运行 analyze 或 sync。"
            )),
            AuthorProfileRebuild::Current { profile_count }
            | AuthorProfileRebuild::Rebuilt { profile_count } => Ok(format!(
                "已重建作者画像：{author}，使用 {profile_count} 个 paper profiles。"
            )),
        }
    }

    fn comprehend(
        &self,
        storage: &Storage,
        chat_id: i64,
        args: TelegramComprehendArgs,
    ) -> Result<String> {
        let Some(author) = args.author.or(self.chat_author(storage, chat_id)?) else {
            return Ok("请指定作者，例如 `/comprehend Ruqiang ZOU`。".to_string());
        };
        let service = ComprehensionService::new(storage);
        let llm = (!args.deterministic).then_some(&self.llm);
        if args.author_profile {
            if args.profiled_only {
                return Ok("`--profiled-only` 只适用于 V2 paper comprehension。".to_string());
            }
            let report = service.comprehend_author_profile_v2(
                &author,
                S4AuthorComprehensionOptions {
                    limit: args.limit,
                    force: args.force,
                    dry_run: args.dry_run,
                },
                llm,
            )?;
            return Ok(format_s4_author_comprehension_report(&report));
        }
        let report = service.comprehend_author_v2(
            &author,
            S3ComprehensionOptions {
                limit: args.limit,
                force: args.force,
                dry_run: args.dry_run,
                profiled_only: args.profiled_only,
            },
            llm,
        )?;
        Ok(format_s3_comprehension_report(&report))
    }

    fn embed_chunks(
        &self,
        storage: &Storage,
        chat_id: i64,
        args: TelegramEmbedArgs,
    ) -> Result<String> {
        let Some(client) = self.embedding.as_ref() else {
            return Ok(
                "Embedding 未配置；请先设置 embedding provider、model 和 API key。".to_string(),
            );
        };
        let Some(author) = args.author.or(self.chat_author(storage, chat_id)?) else {
            return Ok("请指定作者，例如 `/embed Ruqiang ZOU`。".to_string());
        };
        let report = EmbeddingService::new(storage).embed_author(
            &author,
            client,
            EmbeddingRunOptions {
                limit: args.limit,
                force: args.force,
                max_attempts: args.max_attempts,
            },
        )?;
        Ok(format_embedding_run_report(&author, &report))
    }

    fn enqueue_analysis(
        &self,
        storage: &Storage,
        chat_id: i64,
        args: TelegramAnalyzeArgs,
    ) -> Result<String> {
        let Some(author) = args.author.or(self.chat_author(storage, chat_id)?) else {
            return Ok("请指定作者，例如 `/analyze Ruqiang ZOU`。".to_string());
        };
        let plan = AnalysisService::new(storage).enqueue_author(
            &author,
            AnalysisQueueOptions {
                failed_only: args.failed_only,
                stale_only: args.stale_only,
                force: args.force,
                limit: args.limit,
                max_attempts: args.max_attempts,
                model_id: self.llm.model_name(),
                chunker_version: &self.chunker_version,
            },
        )?;
        Ok(format_analysis_queue_plan(&author, &plan))
    }

    fn sync_papers(
        &self,
        storage: &Storage,
        chat_id: i64,
        args: TelegramAnalyzeArgs,
    ) -> Result<String> {
        let Some(paper_root) = self.paper_root.as_deref() else {
            return Ok(
                "Sync 未配置 paper root；请用完整配置启动 `ppc serve-telegram`。".to_string(),
            );
        };
        let Some(author) = args.author.or(self.chat_author(storage, chat_id)?) else {
            return Ok("请指定作者，例如 `/sync Ruqiang ZOU`。".to_string());
        };
        let mut sync_storage = Storage::open(&self.db_path)?;
        let report = SyncService::new(&mut sync_storage).sync_author(SyncRunOptions {
            paper_root,
            author: &author,
            limit: args.limit,
            chunk_max_chars: self.chunk_max_chars,
            chunk_overlap: self.chunk_overlap,
            analysis: AnalysisQueueOptions {
                failed_only: args.failed_only,
                stale_only: args.stale_only,
                force: args.force,
                limit: args.limit,
                max_attempts: args.max_attempts,
                model_id: self.llm.model_name(),
                chunker_version: &self.chunker_version,
            },
        })?;
        Ok(format_sync_run_report(&author, &report))
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
        storage: &Storage,
        outcome: AuthorSelectionOutcome,
    ) -> Result<String> {
        match outcome {
            AuthorSelectionOutcome::Message(message) => Ok(message),
            AuthorSelectionOutcome::Ask { author, question } => self
                .qa_service(storage, &author)?
                .answer(&author, &question),
        }
    }

    async fn reply_for_author_selection_stream<F>(
        &self,
        storage: &Storage,
        chat_id: i64,
        job_id: i64,
        outcome: AuthorSelectionOutcome,
        on_delta: F,
    ) -> Result<String>
    where
        F: FnMut(&str) -> Result<()>,
    {
        match outcome {
            AuthorSelectionOutcome::Message(message) => Ok(message),
            AuthorSelectionOutcome::Ask { author, question } => {
                let qa = self.qa_service(storage, &author)?;
                qa.answer_stream_with_telegram_context(
                    &author, &question, chat_id, job_id, on_delta,
                )
                .await
            }
        }
    }

    fn qa_service<'a>(&self, storage: &'a Storage, author: &str) -> Result<QaService<'a>> {
        QaService::new_with_profile_preference(
            storage,
            self.llm.clone(),
            self.embedding.clone(),
            author,
            self.qa_profile_version,
        )
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
        if let Some(author) = self.chat_author(storage, chat_id)? {
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
        if let Some(author) = self.chat_author(storage, chat_id)? {
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
        if summaries.len() == 1 && matches!(action, PendingAuthorAction::Ask { .. }) {
            let author = summaries[0].author.clone();
            self.set_chat_author(storage, chat_id, &author)?;
            let PendingAuthorAction::Ask { question } = action else {
                unreachable!("checked action above")
            };
            return Ok(AuthorSelectionOutcome::Ask { author, question });
        }

        let authors = summaries
            .iter()
            .map(|summary| summary.author.clone())
            .collect::<Vec<_>>();
        storage.save_telegram_pending_author_selection(NewTelegramPendingAuthorSelection {
            chat_id,
            action: pending_action_name(&action),
            question: pending_action_question(&action),
            authors: &authors,
        })?;
        Ok(AuthorSelectionOutcome::Message(format_author_choices(
            &summaries, &action, has_more,
        )))
    }

    fn resolve_pending_author_selection(
        &self,
        storage: &Storage,
        chat_id: i64,
        text: &str,
    ) -> Result<Option<AuthorSelectionOutcome>> {
        let Some(index) = parse_author_selection(text) else {
            return Ok(None);
        };
        let selection = storage.telegram_pending_author_selection(chat_id)?;
        let Some(selection) = selection else {
            return Ok(None);
        };
        if index >= selection.authors.len() {
            return Ok(Some(AuthorSelectionOutcome::Message(format!(
                "请选择 1-{} 之间的序号。",
                selection.authors.len()
            ))));
        }

        storage.clear_telegram_pending_author_selection(chat_id)?;
        let author = selection.authors[index].clone();
        self.set_chat_author(storage, chat_id, &author)?;
        Ok(Some(match pending_action_from_record(selection)? {
            PendingAuthorAction::SetDefault => {
                AuthorSelectionOutcome::Message(format!("当前 chat 默认作者已设置为：{author}"))
            }
            PendingAuthorAction::Ask { question } => {
                AuthorSelectionOutcome::Ask { author, question }
            }
        }))
    }

    fn parse_sources_args(
        &self,
        storage: &Storage,
        chat_id: i64,
        body: &str,
    ) -> Result<(Option<String>, bool)> {
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
            self.chat_author(storage, chat_id)?
        } else {
            Some(author.to_string())
        };
        Ok((author, full))
    }

    fn parse_status_args(
        &self,
        storage: &Storage,
        chat_id: i64,
        body: &str,
    ) -> Result<(Option<String>, bool)> {
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
            self.chat_author(storage, chat_id)?
        } else {
            Some(author.to_string())
        };
        Ok((author, detail))
    }

    fn parse_jobs_args(
        &self,
        storage: &Storage,
        chat_id: i64,
        body: &str,
    ) -> Result<(Option<String>, Option<String>)> {
        let body = body.trim();
        if body.is_empty() {
            return Ok((self.chat_author(storage, chat_id)?, None));
        }
        let mut parts = body.split_whitespace();
        let first = parts.next().unwrap_or_default();
        if let Some(status) = normalize_job_status(first) {
            let author = body
                .strip_prefix(first)
                .map(str::trim)
                .filter(|author| !author.is_empty())
                .map(str::to_string);
            let author = match author {
                Some(author) => Some(author),
                None => self.chat_author(storage, chat_id)?,
            };
            return Ok((author, Some(status.to_string())));
        }
        Ok((Some(body.to_string()), None))
    }

    fn set_chat_author(&self, storage: &Storage, chat_id: i64, author: &str) -> Result<()> {
        storage.save_telegram_chat_author(chat_id, author)?;
        let mut authors = self
            .chat_authors
            .lock()
            .map_err(|_| anyhow!("chat author state is unavailable"))?;
        authors.insert(chat_id, author.to_string());
        storage.clear_telegram_pending_author_selection(chat_id)?;
        Ok(())
    }

    fn chat_author(&self, storage: &Storage, chat_id: i64) -> Result<Option<String>> {
        if let Some(author) = self
            .chat_authors
            .lock()
            .ok()
            .and_then(|authors| authors.get(&chat_id).cloned())
        {
            return Ok(Some(author));
        }
        if let Some(author) = storage.telegram_chat_author(chat_id)? {
            let mut authors = self
                .chat_authors
                .lock()
                .map_err(|_| anyhow!("chat author state is unavailable"))?;
            authors.insert(chat_id, author.clone());
            return Ok(Some(author));
        }
        Ok(self.default_author.clone())
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
        "/profile --rebuild [AUTHOR] - Rebuild the author profile from paper profiles.",
        "/rebuild_profile [AUTHOR] - Rebuild the author profile from paper profiles.",
        "/sync [AUTHOR] - Ingest local papers and queue analysis.",
        "/analyze [AUTHOR] - Queue papers for analysis.",
        "/embed [AUTHOR] - Create or refresh chunk embeddings.",
        "/comprehend [--author-profile] [AUTHOR] - Build V2 profiles from extracted facts.",
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

fn telegram_command_token(text: &str) -> Option<&str> {
    let token = text.split_whitespace().next()?;
    token.starts_with('/').then_some(token)
}

fn unknown_command_message(command: &str) -> String {
    format!("未知命令：{command}。使用 /help 查看可用命令。")
}

fn pending_action_name(action: &PendingAuthorAction) -> &'static str {
    match action {
        PendingAuthorAction::SetDefault => PENDING_ACTION_SET_DEFAULT,
        PendingAuthorAction::Ask { .. } => PENDING_ACTION_ASK,
    }
}

fn pending_action_question(action: &PendingAuthorAction) -> Option<&str> {
    match action {
        PendingAuthorAction::SetDefault => None,
        PendingAuthorAction::Ask { question } => Some(question),
    }
}

fn pending_action_from_record(
    selection: TelegramPendingAuthorSelection,
) -> Result<PendingAuthorAction> {
    match selection.action.as_str() {
        PENDING_ACTION_SET_DEFAULT => Ok(PendingAuthorAction::SetDefault),
        PENDING_ACTION_ASK => Ok(PendingAuthorAction::Ask {
            question: selection.question.unwrap_or_default(),
        }),
        action => Err(anyhow!("unknown pending author selection action: {action}")),
    }
}

#[derive(Debug, PartialEq, Eq)]
struct TelegramProfileArgs {
    author: Option<String>,
    rebuild: bool,
}

#[derive(Debug, Default, PartialEq, Eq)]
struct TelegramComprehendArgs {
    author: Option<String>,
    limit: Option<usize>,
    force: bool,
    dry_run: bool,
    profiled_only: bool,
    author_profile: bool,
    deterministic: bool,
}

#[derive(Debug, PartialEq, Eq)]
struct TelegramEmbedArgs {
    author: Option<String>,
    limit: Option<usize>,
    force: bool,
    max_attempts: usize,
}

#[derive(Debug, PartialEq, Eq)]
struct TelegramAnalyzeArgs {
    author: Option<String>,
    limit: Option<usize>,
    force: bool,
    failed_only: bool,
    stale_only: bool,
    max_attempts: i64,
}

fn parse_telegram_profile_args(body: &str) -> TelegramProfileArgs {
    let mut rebuild = false;
    let mut author_parts = Vec::new();
    for part in body.split_whitespace() {
        if matches!(part, "--rebuild" | "rebuild") {
            rebuild = true;
        } else {
            author_parts.push(part);
        }
    }
    TelegramProfileArgs {
        author: non_empty_trimmed(&author_parts.join(" ")).map(str::to_string),
        rebuild,
    }
}

fn parse_telegram_analyze_args(body: &str) -> Result<TelegramAnalyzeArgs> {
    parse_telegram_analysis_queue_args(body, "/analyze")
}

fn parse_telegram_sync_args(body: &str) -> Result<TelegramAnalyzeArgs> {
    parse_telegram_analysis_queue_args(body, "/sync")
}

fn parse_telegram_analysis_queue_args(body: &str, command: &str) -> Result<TelegramAnalyzeArgs> {
    let parts = body.split_whitespace().collect::<Vec<_>>();
    let mut limit = None;
    let mut force = false;
    let mut failed_only = false;
    let mut stale_only = false;
    let mut max_attempts = 3;
    let mut author_parts = Vec::new();
    let mut index = 0;
    while index < parts.len() {
        let part = parts[index];
        match part {
            "--force" => force = true,
            "--failed-only" => failed_only = true,
            "--stale-only" => stale_only = true,
            "--limit" => {
                index += 1;
                let Some(value) = parts.get(index) else {
                    return Err(anyhow!("`--limit` 需要数字参数"));
                };
                limit = Some(parse_positive_usize(value, "--limit")?);
            }
            "--max-attempts" => {
                index += 1;
                let Some(value) = parts.get(index) else {
                    return Err(anyhow!("`--max-attempts` 需要数字参数"));
                };
                max_attempts = parse_positive_i64(value, "--max-attempts")?;
            }
            "--author" => {
                index += 1;
                while index < parts.len() && !parts[index].starts_with("--") {
                    author_parts.push(parts[index]);
                    index += 1;
                }
                index = index.saturating_sub(1);
            }
            _ if part.starts_with("--limit=") => {
                limit = Some(parse_positive_usize(
                    part.trim_start_matches("--limit="),
                    "--limit",
                )?);
            }
            _ if part.starts_with("--max-attempts=") => {
                max_attempts = parse_positive_i64(
                    part.trim_start_matches("--max-attempts="),
                    "--max-attempts",
                )?;
            }
            _ if part.starts_with("--") => {
                return Err(anyhow!("不支持的 {command} 参数：{part}"));
            }
            _ => author_parts.push(part),
        }
        index += 1;
    }
    Ok(TelegramAnalyzeArgs {
        author: non_empty_trimmed(&author_parts.join(" ")).map(str::to_string),
        limit,
        force,
        failed_only,
        stale_only,
        max_attempts,
    })
}

fn parse_telegram_embed_args(body: &str) -> Result<TelegramEmbedArgs> {
    let parts = body.split_whitespace().collect::<Vec<_>>();
    let mut limit = None;
    let mut force = false;
    let mut max_attempts = 3;
    let mut author_parts = Vec::new();
    let mut index = 0;
    while index < parts.len() {
        let part = parts[index];
        match part {
            "--force" => force = true,
            "--limit" => {
                index += 1;
                let Some(value) = parts.get(index) else {
                    return Err(anyhow!("`--limit` 需要数字参数"));
                };
                limit = Some(parse_positive_usize(value, "--limit")?);
            }
            "--max-attempts" => {
                index += 1;
                let Some(value) = parts.get(index) else {
                    return Err(anyhow!("`--max-attempts` 需要数字参数"));
                };
                max_attempts = parse_positive_usize(value, "--max-attempts")?;
            }
            "--author" => {
                index += 1;
                while index < parts.len() && !parts[index].starts_with("--") {
                    author_parts.push(parts[index]);
                    index += 1;
                }
                index = index.saturating_sub(1);
            }
            _ if part.starts_with("--limit=") => {
                limit = Some(parse_positive_usize(
                    part.trim_start_matches("--limit="),
                    "--limit",
                )?);
            }
            _ if part.starts_with("--max-attempts=") => {
                max_attempts = parse_positive_usize(
                    part.trim_start_matches("--max-attempts="),
                    "--max-attempts",
                )?;
            }
            _ if part.starts_with("--") => {
                return Err(anyhow!("不支持的 /embed 参数：{part}"));
            }
            _ => author_parts.push(part),
        }
        index += 1;
    }
    Ok(TelegramEmbedArgs {
        author: non_empty_trimmed(&author_parts.join(" ")).map(str::to_string),
        limit,
        force,
        max_attempts,
    })
}

fn parse_telegram_comprehend_args(body: &str) -> Result<TelegramComprehendArgs> {
    let parts = body.split_whitespace().collect::<Vec<_>>();
    let mut args = TelegramComprehendArgs::default();
    let mut author_parts = Vec::new();
    let mut index = 0;
    while index < parts.len() {
        let part = parts[index];
        match part {
            "--v2" => {}
            "--force" => args.force = true,
            "--dry-run" => args.dry_run = true,
            "--profiled-only" => args.profiled_only = true,
            "--author-profile" => args.author_profile = true,
            "--deterministic" => args.deterministic = true,
            "--limit" => {
                index += 1;
                let Some(value) = parts.get(index) else {
                    return Err(anyhow!("`--limit` 需要数字参数"));
                };
                args.limit = Some(parse_positive_usize(value, "--limit")?);
            }
            "--author" => {
                index += 1;
                while index < parts.len() && !parts[index].starts_with("--") {
                    author_parts.push(parts[index]);
                    index += 1;
                }
                index = index.saturating_sub(1);
            }
            _ if part.starts_with("--limit=") => {
                let value = part.trim_start_matches("--limit=");
                args.limit = Some(parse_positive_usize(value, "--limit")?);
            }
            _ if part.starts_with("--") => {
                return Err(anyhow!("不支持的 /comprehend 参数：{part}"));
            }
            _ => author_parts.push(part),
        }
        index += 1;
    }
    args.author = non_empty_trimmed(&author_parts.join(" ")).map(str::to_string);
    Ok(args)
}

fn parse_positive_usize(value: &str, flag: &str) -> Result<usize> {
    let parsed = value
        .parse::<usize>()
        .map_err(|_| anyhow!("{flag} 需要数字参数"))?;
    if parsed == 0 {
        return Err(anyhow!("{flag} 必须大于 0"));
    }
    Ok(parsed)
}

fn parse_positive_i64(value: &str, flag: &str) -> Result<i64> {
    let parsed = value
        .parse::<i64>()
        .map_err(|_| anyhow!("{flag} 需要数字参数"))?;
    if parsed <= 0 {
        return Err(anyhow!("{flag} 必须大于 0"));
    }
    Ok(parsed)
}

fn command_body<'a>(text: &'a str, command: &str) -> Option<&'a str> {
    let text = text.trim();
    let rest = text.strip_prefix(command)?;
    if rest.is_empty() || rest.starts_with(char::is_whitespace) {
        Some(rest.trim())
    } else {
        None
    }
}

fn rebuild_profile_command_body(text: &str) -> Option<&str> {
    command_body(text, "/rebuild_profile")
        .or_else(|| command_body(text, "/profile_rebuild"))
        .or_else(|| command_body(text, "/rebuild"))
}

fn embed_command_body(text: &str) -> Option<&str> {
    command_body(text, "/embed").or_else(|| command_body(text, "/embedding"))
}

fn non_empty_trimmed(value: &str) -> Option<&str> {
    let value = value.trim();
    (!value.is_empty()).then_some(value)
}

fn format_embedding_run_report(author: &str, report: &EmbeddingRunReport) -> String {
    let mut lines = vec![
        "embedding:".to_string(),
        format!("author: {author}"),
        format!("model: {}", report.model),
    ];
    if let Some(model_version) = report.model_version.as_deref() {
        lines.push(format!("model_version: {model_version}"));
    }
    lines.push(format!("pending: {}", report.pending));
    lines.push(format!("embedded: {}", report.embedded));
    lines.push(format!("failed: {}", report.failed));
    lines.join("\n")
}

fn format_analysis_queue_plan(author: &str, plan: &AnalysisQueuePlan) -> String {
    [
        "analysis queue:".to_string(),
        format!("author: {author}"),
        format!("papers_needing_analysis: {}", plan.candidates.len()),
        format!("newly_queued: {}", plan.queued),
    ]
    .join("\n")
}

fn format_sync_run_report(author: &str, report: &SyncRunReport) -> String {
    [
        "sync:".to_string(),
        format!("author: {author}"),
        format!("paper_dirs: {}", report.paper_dirs),
        format!("ingested: {}", report.ingested),
        format!("changed: {}", report.changed),
        format!(
            "papers_needing_analysis: {}",
            report.analysis.candidates.len()
        ),
        format!("newly_queued: {}", report.analysis.queued),
    ]
    .join("\n")
}

fn format_s3_comprehension_report(report: &S3ComprehensionReport) -> String {
    let mut lines = Vec::new();
    let mode = if report.dry_run {
        "v2 paper comprehension dry run:"
    } else {
        "v2 paper comprehension:"
    };
    lines.push(mode.to_string());
    lines.push(format!("model_id: {}", report.model_id));
    lines.push(format!("papers_scanned: {}", report.papers_scanned));
    lines.push(format!("built: {}", report.built));
    lines.push(format!("changed: {}", report.changed));
    lines.push(format!("skipped_current: {}", report.skipped_current));
    lines.push(format!(
        "missing_chunk_facts: {}",
        report.missing_chunk_facts
    ));
    lines.push(format!("failed: {}", report.failed));
    lines.join("\n")
}

fn format_s4_author_comprehension_report(report: &S4AuthorComprehensionReport) -> String {
    let mut lines = Vec::new();
    let mode = if report.dry_run {
        "v2 author comprehension dry run:"
    } else {
        "v2 author comprehension:"
    };
    lines.push(mode.to_string());
    lines.push(format!("model_id: {}", report.model_id));
    lines.push(format!(
        "paper_profiles_scanned: {}",
        report.paper_profiles_scanned
    ));
    lines.push(format!("built: {}", report.built));
    lines.push(format!("changed: {}", report.changed));
    lines.push(format!("skipped_current: {}", report.skipped_current));
    lines.push(format!(
        "missing_paper_profiles: {}",
        report.missing_paper_profiles
    ));
    lines.push(format!("research_themes: {}", report.research_themes));
    lines.join("\n")
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
        BotHandlers, BotRuntimeSettings, PendingAuthorAction, format_author_choices, format_jobs,
        format_sources, format_status, help_message, normalize_job_status, parse_author_selection,
        parse_telegram_analyze_args, parse_telegram_comprehend_args, parse_telegram_embed_args,
        parse_telegram_profile_args, parse_telegram_sync_args,
    };
    use crate::papers::models::Paper;
    use crate::retrieval::embedding::{EmbeddingConfig, OpenAiCompatibleEmbeddingClient};
    use crate::services::qa::QaProfileVersionPreference;
    use crate::storage::{AnalysisJobSummary, AuthorSummary, LibraryStatus};
    use crate::understanding::llm::{LlmConfig, OpenAiCompatibleClient};

    #[test]
    fn help_lists_available_commands_with_purpose() {
        let text = help_message();

        assert!(text.contains("Available commands:"));
        assert!(text.contains("/help - Show this command list."));
        assert!(text.contains("/authors - List available authors and select one by number."));
        assert!(text.contains("/use_author [NAME] - Set or choose the default author"));
        assert!(text.contains("/profile --rebuild [AUTHOR] - Rebuild the author profile"));
        assert!(text.contains("/rebuild_profile [AUTHOR] - Rebuild the author profile"));
        assert!(text.contains("/sync [AUTHOR] - Ingest local papers and queue analysis."));
        assert!(text.contains("/analyze [AUTHOR] - Queue papers for analysis."));
        assert!(text.contains("/embed [AUTHOR] - Create or refresh chunk embeddings."));
        assert!(text.contains("/comprehend [--author-profile] [AUTHOR]"));
        assert!(text.contains("/status [detail] [AUTHOR] - Show library and job status."));
        assert!(text.contains("/ask AUTHOR | QUESTION - Ask about a specific author."));
    }

    #[test]
    fn parses_profile_rebuild_arguments() {
        let args = parse_telegram_profile_args("--rebuild Ruqiang ZOU");
        assert!(args.rebuild);
        assert_eq!(args.author.as_deref(), Some("Ruqiang ZOU"));

        let args = parse_telegram_profile_args("Ruqiang ZOU rebuild");
        assert!(args.rebuild);
        assert_eq!(args.author.as_deref(), Some("Ruqiang ZOU"));

        let args = parse_telegram_profile_args("Ruqiang ZOU");
        assert!(!args.rebuild);
        assert_eq!(args.author.as_deref(), Some("Ruqiang ZOU"));
    }

    #[test]
    fn parses_comprehend_arguments() {
        let args =
            parse_telegram_comprehend_args("--v2 --limit 3 --force --dry-run Ruqiang ZOU").unwrap();
        assert_eq!(args.author.as_deref(), Some("Ruqiang ZOU"));
        assert_eq!(args.limit, Some(3));
        assert!(args.force);
        assert!(args.dry_run);
        assert!(!args.author_profile);

        let args = parse_telegram_comprehend_args(
            "--author-profile --profiled-only --deterministic --author Ruqiang ZOU",
        )
        .unwrap();
        assert_eq!(args.author.as_deref(), Some("Ruqiang ZOU"));
        assert!(args.author_profile);
        assert!(args.profiled_only);
        assert!(args.deterministic);

        assert!(parse_telegram_comprehend_args("--limit 0 Alice").is_err());
        assert!(parse_telegram_comprehend_args("--unknown Alice").is_err());
    }

    #[test]
    fn parses_embed_arguments() {
        let args =
            parse_telegram_embed_args("--limit 5 --force --max-attempts=2 Ruqiang ZOU").unwrap();

        assert_eq!(args.author.as_deref(), Some("Ruqiang ZOU"));
        assert_eq!(args.limit, Some(5));
        assert_eq!(args.max_attempts, 2);
        assert!(args.force);

        let args = parse_telegram_embed_args("--author Ruqiang ZOU --limit=3").unwrap();
        assert_eq!(args.author.as_deref(), Some("Ruqiang ZOU"));
        assert_eq!(args.limit, Some(3));
        assert_eq!(args.max_attempts, 3);

        assert!(parse_telegram_embed_args("--limit 0 Alice").is_err());
        assert!(parse_telegram_embed_args("--unknown Alice").is_err());
    }

    #[test]
    fn parses_analyze_arguments() {
        let args =
            parse_telegram_analyze_args("--limit 5 --force --max-attempts=2 Ruqiang ZOU").unwrap();

        assert_eq!(args.author.as_deref(), Some("Ruqiang ZOU"));
        assert_eq!(args.limit, Some(5));
        assert_eq!(args.max_attempts, 2);
        assert!(args.force);
        assert!(!args.failed_only);
        assert!(!args.stale_only);

        let args = parse_telegram_analyze_args(
            "--author Ruqiang ZOU --failed-only --stale-only --limit=3",
        )
        .unwrap();
        assert_eq!(args.author.as_deref(), Some("Ruqiang ZOU"));
        assert!(args.failed_only);
        assert!(args.stale_only);
        assert_eq!(args.limit, Some(3));

        assert!(parse_telegram_analyze_args("--limit 0 Alice").is_err());
        assert!(parse_telegram_analyze_args("--unknown Alice").is_err());
    }

    #[test]
    fn parses_sync_arguments() {
        let args = parse_telegram_sync_args("--limit=2 --force Alice").unwrap();

        assert_eq!(args.author.as_deref(), Some("Alice"));
        assert_eq!(args.limit, Some(2));
        assert!(args.force);

        let error = parse_telegram_sync_args("--unknown Alice")
            .unwrap_err()
            .to_string();
        assert!(error.contains("不支持的 /sync 参数"));
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
    fn unknown_slash_command_does_not_fall_through_to_qa() {
        let dir = tempdir().unwrap();
        let db_path = dir.path().join("test.sqlite");
        let storage = crate::storage::Storage::open(&db_path).unwrap();
        drop(storage);
        let handlers = BotHandlers::new(db_path, test_llm(), None, None);

        let reply = handlers.handle_text(7, "/unknown Alice").unwrap();

        assert_eq!(reply, "未知命令：/unknown。使用 /help 查看可用命令。");
    }

    #[test]
    fn profile_rebuild_command_reports_missing_paper_profiles() {
        let dir = tempdir().unwrap();
        let db_path = dir.path().join("test.sqlite");
        let storage = crate::storage::Storage::open(&db_path).unwrap();
        drop(storage);
        let handlers = BotHandlers::new(db_path, test_llm(), None, None);

        let reply = handlers
            .handle_text(7, "/profile --rebuild Ruqiang ZOU")
            .unwrap();

        assert!(reply.contains("还没有可用于重建作者画像的论文画像：Ruqiang ZOU"));
    }

    #[test]
    fn rebuild_profile_command_uses_chat_author() {
        let dir = tempdir().unwrap();
        let db_path = dir.path().join("test.sqlite");
        let storage = crate::storage::Storage::open(&db_path).unwrap();
        drop(storage);
        let handlers = BotHandlers::new(db_path, test_llm(), None, None);

        let selected = handlers.handle_text(7, "/use_author Alice").unwrap();
        let reply = handlers.handle_text(7, "/rebuild_profile").unwrap();

        assert_eq!(selected, "当前 chat 默认作者已设置为：Alice");
        assert!(reply.contains("还没有可用于重建作者画像的论文画像：Alice"));
    }

    #[test]
    fn comprehend_command_reports_empty_work_without_llm() {
        let dir = tempdir().unwrap();
        let db_path = dir.path().join("test.sqlite");
        let storage = crate::storage::Storage::open(&db_path).unwrap();
        drop(storage);
        let handlers = BotHandlers::new(db_path, test_llm(), None, None);

        let reply = handlers
            .handle_text(7, "/comprehend --deterministic Alice")
            .unwrap();

        assert!(reply.contains("v2 paper comprehension:"));
        assert!(reply.contains("model_id: deterministic"));
        assert!(reply.contains("papers_scanned: 0"));
    }

    #[test]
    fn embed_command_explains_missing_embedding_config() {
        let dir = tempdir().unwrap();
        let db_path = dir.path().join("test.sqlite");
        let storage = crate::storage::Storage::open(&db_path).unwrap();
        drop(storage);
        let handlers = BotHandlers::new(db_path, test_llm(), None, None);

        let reply = handlers.handle_text(7, "/embed Alice").unwrap();

        assert!(reply.contains("Embedding 未配置"));
    }

    #[test]
    fn embed_command_reports_empty_work_without_api_call() {
        let dir = tempdir().unwrap();
        let db_path = dir.path().join("test.sqlite");
        let storage = crate::storage::Storage::open(&db_path).unwrap();
        drop(storage);
        let handlers = BotHandlers::new(db_path, test_llm(), Some(test_embedding()), None);

        let reply = handlers.handle_text(7, "/embed --limit 1 Alice").unwrap();

        assert!(reply.contains("embedding:"));
        assert!(reply.contains("author: Alice"));
        assert!(reply.contains("model: embed-model"));
        assert!(reply.contains("pending: 0"));
        assert!(reply.contains("embedded: 0"));
    }

    #[test]
    fn analyze_command_queues_papers_without_processing_llm() {
        let dir = tempdir().unwrap();
        let db_path = dir.path().join("test.sqlite");
        let mut storage = crate::storage::Storage::open(&db_path).unwrap();
        storage
            .upsert_paper(&test_paper(dir.path(), "Alice", "paper-a"), &[])
            .unwrap();
        drop(storage);
        let handlers = BotHandlers::new_with_chunker_version(
            db_path.clone(),
            test_llm(),
            None,
            None,
            "section-char-v1".to_string(),
        );

        let reply = handlers.handle_text(7, "/analyze --limit 1 Alice").unwrap();

        assert!(reply.contains("analysis queue:"));
        assert!(reply.contains("author: Alice"));
        assert!(reply.contains("papers_needing_analysis: 1"));
        assert!(reply.contains("newly_queued: 1"));
        let storage = crate::storage::Storage::open(&db_path).unwrap();
        let jobs = storage
            .analysis_jobs(Some("Alice"), Some("queued"), 10)
            .unwrap();
        assert_eq!(jobs.len(), 1);
    }

    #[test]
    fn sync_command_ingests_local_papers_and_queues_analysis_without_processing_llm() {
        let dir = tempdir().unwrap();
        let db_path = dir.path().join("test.sqlite");
        let paper_root = dir.path().join("paper");
        write_test_article(&paper_root, "Alice", "paper-a");
        let handlers = BotHandlers::new_with_runtime_settings(
            db_path.clone(),
            test_llm(),
            None,
            None,
            BotRuntimeSettings {
                paper_root: Some(paper_root),
                chunker_version: "section-char-v1".to_string(),
                chunk_max_chars: 3200,
                chunk_overlap: 350,
                qa_profile_version: QaProfileVersionPreference::V1,
            },
        );

        let reply = handlers.handle_text(7, "/sync --limit 1 Alice").unwrap();

        assert!(reply.contains("sync:"));
        assert!(reply.contains("author: Alice"));
        assert!(reply.contains("paper_dirs: 1"));
        assert!(reply.contains("ingested: 1"));
        assert!(reply.contains("changed: 1"));
        assert!(reply.contains("papers_needing_analysis: 1"));
        assert!(reply.contains("newly_queued: 1"));
        let storage = crate::storage::Storage::open(&db_path).unwrap();
        assert_eq!(storage.authors().unwrap()[0].author, "Alice");
        let jobs = storage
            .analysis_jobs(Some("Alice"), Some("queued"), 10)
            .unwrap();
        assert_eq!(jobs.len(), 1);
    }

    #[test]
    fn analyze_command_asks_for_author_when_missing_context() {
        let dir = tempdir().unwrap();
        let db_path = dir.path().join("test.sqlite");
        let storage = crate::storage::Storage::open(&db_path).unwrap();
        drop(storage);
        let handlers = BotHandlers::new(db_path, test_llm(), None, None);

        let reply = handlers.handle_text(7, "/analyze").unwrap();

        assert_eq!(reply, "请指定作者，例如 `/analyze Ruqiang ZOU`。");
    }

    #[test]
    fn comprehend_author_profile_command_uses_chat_author() {
        let dir = tempdir().unwrap();
        let db_path = dir.path().join("test.sqlite");
        let storage = crate::storage::Storage::open(&db_path).unwrap();
        drop(storage);
        let handlers = BotHandlers::new(db_path, test_llm(), None, None);

        handlers.handle_text(7, "/use_author Alice").unwrap();
        let reply = handlers
            .handle_text(7, "/comprehend --author-profile --deterministic")
            .unwrap();

        assert!(reply.contains("v2 author comprehension:"));
        assert!(reply.contains("model_id: deterministic"));
        assert!(reply.contains("missing_paper_profiles: 1"));
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
        let reloaded_handlers = BotHandlers::new(
            handlers.db_path.clone(),
            test_llm(),
            None,
            Some("Fallback".to_string()),
        );
        let selected = reloaded_handlers.handle_text(7, "2").unwrap();
        let current = reloaded_handlers.handle_text(7, "/current_author").unwrap();
        let restarted_again = BotHandlers::new(
            handlers.db_path.clone(),
            test_llm(),
            None,
            Some("Fallback".to_string()),
        );
        let current_after_restart = restarted_again.handle_text(7, "/current_author").unwrap();

        assert!(prompt.contains("1. Alice (1 papers)"));
        assert!(prompt.contains("2. Bob (1 papers)"));
        assert_eq!(selected, "当前 chat 默认作者已设置为：Bob");
        assert_eq!(current, "当前默认作者：Bob");
        assert_eq!(current_after_restart, "当前默认作者：Bob");
    }

    #[test]
    fn use_author_without_argument_lists_single_author_before_selecting() {
        let dir = tempdir().unwrap();
        let db_path = dir.path().join("test.sqlite");
        let mut storage = crate::storage::Storage::open(&db_path).unwrap();
        storage
            .upsert_paper(&test_paper(dir.path(), "Ruqiang ZOU", "paper-a"), &[])
            .unwrap();
        drop(storage);
        let handlers = BotHandlers::new(db_path, test_llm(), None, None);

        let prompt = handlers.handle_text(7, "/use_author").unwrap();
        let selected = handlers.handle_text(7, "1").unwrap();

        assert!(prompt.contains("请选择当前 chat 的默认作者："));
        assert!(prompt.contains("1. Ruqiang ZOU (1 papers)"));
        assert_eq!(selected, "当前 chat 默认作者已设置为：Ruqiang ZOU");
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

    fn test_embedding() -> OpenAiCompatibleEmbeddingClient {
        OpenAiCompatibleEmbeddingClient::new(EmbeddingConfig {
            provider: "openai-compatible".to_string(),
            base_url: "http://127.0.0.1:1/v1".to_string(),
            api_key: None,
            model: "embed-model".to_string(),
            model_version: None,
            proxy: None,
            timeout_secs: 1,
            tls_backend: "rustls".to_string(),
            batch_size: 8,
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

    fn write_test_article(root: &std::path::Path, author: &str, paper_id: &str) {
        let paper_dir = root.join(author).join(paper_id);
        std::fs::create_dir_all(&paper_dir).unwrap();
        std::fs::write(
            paper_dir.join("article.md"),
            r#"---
title: "A Paper"
year: "2024"
---
# Abstract
This paper studies MOF catalysis.

## Methods
The method uses solvent screening.
"#,
        )
        .unwrap();
    }
}
