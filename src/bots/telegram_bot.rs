use std::collections::HashSet;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use anyhow::{Result, anyhow};
use reqwest::header::HeaderMap;
use reqwest::{Client, ClientBuilder, Proxy};
use serde::Deserialize;
use tokio::sync::Semaphore;
use tokio::sync::mpsc::{UnboundedSender, unbounded_channel};
use tokio::time::{Instant, sleep};

use super::dispatcher::{ChatDispatcher, DispatchAction};
use super::handlers::BotHandlers;
use crate::storage::NewTelegramDeliveryLog;

const TELEGRAM_POLL_TIMEOUT_SECS: u64 = 10;
const TELEGRAM_REQUEST_TIMEOUT_SECS: u64 = TELEGRAM_POLL_TIMEOUT_SECS + 20;
const TELEGRAM_POLL_RETRY_DELAY_SECS: u64 = 3;
const TELEGRAM_MESSAGE_PREVIEW_CHARS: usize = 80;
const TELEGRAM_MAX_MESSAGE_CHARS: usize = 3900;
const TELEGRAM_STREAM_PLACEHOLDER: &str = "处理中...";
const TELEGRAM_DEFAULT_429_BACKOFF_SECS: u64 = 3;
const TELEGRAM_LLM_CONCURRENCY: usize = 2;
const TELEGRAM_STREAM_EDIT_INTERVAL_MS: u64 = 1200;
const TELEGRAM_STREAM_MIN_EDIT_CHARS: usize = 24;

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
struct StreamPreviewStats {
    edit_attempts: usize,
    edit_successes: usize,
    edit_failures: usize,
    last_preview_chars: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum FinalDelivery {
    Empty,
    EditedPlaceholder,
    SentFallback,
    SkippedCancelled,
    Failed,
}

impl FinalDelivery {
    fn as_str(self) -> &'static str {
        match self {
            Self::Empty => "empty",
            Self::EditedPlaceholder => "edited_placeholder",
            Self::SentFallback => "sent_fallback",
            Self::SkippedCancelled => "skipped_cancelled",
            Self::Failed => "failed",
        }
    }
}

#[derive(Debug, Clone, Copy)]
struct TelegramDeliveryEvent<'a> {
    chat_id: i64,
    job_id: u64,
    final_delivery: FinalDelivery,
    preview_stats: StreamPreviewStats,
    reply_chars: usize,
    cancelled: bool,
    error_code: Option<&'a str>,
}

#[derive(Clone)]
pub struct TelegramBot {
    token: String,
    allowed_chat_ids: Vec<i64>,
    admin_user_ids: Vec<i64>,
    handlers: Arc<BotHandlers>,
    http: Client,
    cancelled_jobs: Arc<Mutex<HashSet<u64>>>,
    llm_semaphore: Arc<Semaphore>,
}

impl TelegramBot {
    pub fn new(
        token: String,
        allowed_chat_ids: Vec<i64>,
        admin_user_ids: Vec<i64>,
        proxy: Option<String>,
        handlers: BotHandlers,
    ) -> Result<Self> {
        Ok(Self {
            token,
            allowed_chat_ids,
            admin_user_ids,
            handlers: Arc::new(handlers),
            http: http_client(proxy.as_deref())?,
            cancelled_jobs: Arc::new(Mutex::new(HashSet::new())),
            llm_semaphore: Arc::new(Semaphore::new(TELEGRAM_LLM_CONCURRENCY)),
        })
    }

    pub fn run_polling(&self) -> Result<()> {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()?;
        let local = tokio::task::LocalSet::new();
        runtime.block_on(local.run_until(self.run_polling_async()))
    }

    async fn run_polling_async(&self) -> Result<()> {
        let bot_username = self.get_bot_username_with_retry().await?;
        eprintln!(
            "Telegram bot started as {}; allowed chats: {}",
            format_bot_username(bot_username.as_deref()),
            format_allowed_chat_ids(&self.allowed_chat_ids)
        );
        self.record_heartbeat("started");
        let mut offset = 0i64;
        let mut dispatcher = ChatDispatcher::default();
        let (done_tx, mut done_rx) = unbounded_channel::<i64>();
        loop {
            while let Ok(chat_id) = done_rx.try_recv() {
                if let Some(action) = dispatcher.finish(chat_id) {
                    self.apply_dispatch_action(action, done_tx.clone()).await?;
                }
            }
            let updates = match self.get_updates(offset).await {
                Ok(updates) => updates,
                Err(err) if is_recoverable_telegram_api_error(&err) => {
                    eprintln!(
                        "Telegram polling temporarily failed: {}",
                        telegram_error_message(&err, &self.token)
                    );
                    self.record_heartbeat("temporary_failed");
                    sleep(Duration::from_secs(TELEGRAM_POLL_RETRY_DELAY_SECS)).await;
                    continue;
                }
                Err(err) => {
                    return Err(anyhow!(
                        "Telegram polling failed: {}",
                        telegram_error_message(&err, &self.token)
                    ));
                }
            };
            self.record_heartbeat("polling");
            for update in updates {
                offset = offset.max(update.update_id + 1);
                let Some(message) = update.message.or(update.edited_message) else {
                    continue;
                };
                let Some(text) = message.text.as_deref() else {
                    eprintln!(
                        "Telegram update {} ignored: non-text message in chat {} ({})",
                        update.update_id, message.chat.id, message.chat.kind
                    );
                    continue;
                };
                eprintln!(
                    "Telegram update {} received: chat {} ({}) text=\"{}\"",
                    update.update_id,
                    message.chat.id,
                    message.chat.kind,
                    preview_text(text)
                );
                if let Some(reason) = self.skip_reason(&message, text, bot_username.as_deref()) {
                    eprintln!("Telegram update {} ignored: {reason}", update.update_id);
                    continue;
                }
                let text = handler_text_after_bot_mention(text, bot_username.as_deref());
                if is_dispatcher_cancel_command(text.trim()) {
                    match dispatcher.cancel(message.chat.id) {
                        DispatchAction::Cancelled { active_job_id, .. } => {
                            if let Some(job_id) = active_job_id {
                                self.mark_job_cancelled(job_id);
                            }
                            self.send_message(message.chat.id, "已取消当前回答。")
                                .await?;
                        }
                        DispatchAction::NothingToCancel { .. } => {
                            self.send_message(message.chat.id, "当前没有可取消的回答。")
                                .await?;
                        }
                        _ => {}
                    }
                    continue;
                }
                let action = dispatcher.submit(message.chat.id, text);
                self.apply_dispatch_action(action, done_tx.clone()).await?;
            }
            sleep(Duration::from_millis(1000)).await;
        }
    }

    fn record_heartbeat(&self, status: &str) {
        if let Err(err) = self.handlers.record_telegram_heartbeat(status) {
            eprintln!("Telegram heartbeat write failed: {err}");
        }
    }

    async fn get_bot_username_with_retry(&self) -> Result<Option<String>> {
        loop {
            match self.get_me().await {
                Ok(user) => return Ok(user.username),
                Err(err) if is_recoverable_telegram_api_error(&err) => {
                    eprintln!(
                        "Telegram getMe temporarily failed: {}",
                        telegram_error_message(&err, &self.token)
                    );
                    sleep(Duration::from_secs(TELEGRAM_POLL_RETRY_DELAY_SECS)).await;
                }
                Err(err) => {
                    return Err(anyhow!(
                        "Telegram getMe failed: {}",
                        telegram_error_message(&err, &self.token)
                    ));
                }
            }
        }
    }

    async fn apply_dispatch_action(
        &self,
        action: DispatchAction,
        done_tx: UnboundedSender<i64>,
    ) -> Result<()> {
        match action {
            DispatchAction::Start {
                chat_id,
                job_id,
                text,
            } => {
                let bot = self.clone();
                tokio::task::spawn_local(async move {
                    if let Err(err) = bot
                        .process_message(chat_id, job_id, text, done_tx.clone())
                        .await
                    {
                        eprintln!("Telegram message processing failed: {err}");
                        let _ = done_tx.send(chat_id);
                    }
                });
            }
            DispatchAction::Queued { chat_id, queue_len } => {
                self.send_message(chat_id, &format!("已排队，前面还有 {queue_len} 条。"))
                    .await?;
            }
            DispatchAction::Cancelled {
                chat_id,
                active_job_id,
            } => {
                if let Some(job_id) = active_job_id {
                    self.mark_job_cancelled(job_id);
                }
                self.send_message(chat_id, "已取消当前回答。").await?;
            }
            DispatchAction::NothingToCancel { chat_id } => {
                self.send_message(chat_id, "当前没有可取消的回答。").await?;
            }
        }
        Ok(())
    }

    async fn process_message(
        &self,
        chat_id: i64,
        job_id: u64,
        text: String,
        done_tx: UnboundedSender<i64>,
    ) -> Result<()> {
        let _permit = self.llm_semaphore.clone().acquire_owned().await?;
        if should_stream_text(&text) {
            self.send_streaming_reply(chat_id, job_id, &text).await?;
        } else {
            let reply = self.handle_text_blocking(chat_id, text).await?;
            if !self.is_job_cancelled(job_id) {
                self.send_long_message(chat_id, &reply).await?;
            }
        }
        let _ = done_tx.send(chat_id);
        Ok(())
    }

    async fn send_streaming_reply(&self, chat_id: i64, job_id: u64, text: &str) -> Result<()> {
        let placeholder = self
            .send_message(chat_id, TELEGRAM_STREAM_PLACEHOLDER)
            .await?;
        eprintln!(
            "Telegram placeholder sent: chat {chat_id} message {}",
            placeholder.message_id
        );
        let (delta_tx, delta_rx) = unbounded_channel::<String>();
        let preview_bot = self.clone();
        let preview_message_id = placeholder.message_id;
        let preview_task = tokio::task::spawn_local(async move {
            preview_bot
                .stream_preview_updates(chat_id, preview_message_id, delta_rx)
                .await
        });
        let reply = self
            .handle_text_streaming(chat_id, job_id, text.to_string(), move |delta| {
                let _ = delta_tx.send(delta.to_string());
                Ok(())
            })
            .await?;
        let preview_stats = match preview_task.await {
            Ok(stats) => stats,
            Err(err) => {
                eprintln!("Telegram stream preview task failed: {err}");
                StreamPreviewStats::default()
            }
        };
        eprintln!(
            "Telegram handler completed: chat {chat_id} reply_chars={} preview_edit_attempts={} preview_edit_successes={} preview_edit_failures={} preview_last_chars={}",
            reply.chars().count(),
            preview_stats.edit_attempts,
            preview_stats.edit_successes,
            preview_stats.edit_failures,
            preview_stats.last_preview_chars,
        );

        if self.is_job_cancelled(job_id) {
            eprintln!(
                "Telegram final reply skipped: chat {chat_id} job_cancelled=true preview_edit_attempts={} preview_edit_successes={} preview_edit_failures={}",
                preview_stats.edit_attempts,
                preview_stats.edit_successes,
                preview_stats.edit_failures,
            );
            self.record_delivery_log(TelegramDeliveryEvent {
                chat_id,
                job_id,
                final_delivery: FinalDelivery::SkippedCancelled,
                preview_stats,
                reply_chars: reply.chars().count(),
                cancelled: true,
                error_code: Some("cancelled"),
            });
            return Ok(());
        }
        let final_delivery = match self
            .replace_message_with_long_text(chat_id, placeholder.message_id, &reply)
            .await
        {
            Ok(final_delivery) => final_delivery,
            Err(err) => {
                self.record_delivery_log(TelegramDeliveryEvent {
                    chat_id,
                    job_id,
                    final_delivery: FinalDelivery::Failed,
                    preview_stats,
                    reply_chars: reply.chars().count(),
                    cancelled: false,
                    error_code: Some("final_delivery_failed"),
                });
                return Err(err);
            }
        };
        eprintln!(
            "Telegram final reply sent: chat {chat_id} final_delivery={} preview_edit_attempts={} preview_edit_successes={} preview_edit_failures={}",
            final_delivery.as_str(),
            preview_stats.edit_attempts,
            preview_stats.edit_successes,
            preview_stats.edit_failures,
        );
        self.record_delivery_log(TelegramDeliveryEvent {
            chat_id,
            job_id,
            final_delivery,
            preview_stats,
            reply_chars: reply.chars().count(),
            cancelled: false,
            error_code: None,
        });
        Ok(())
    }

    fn record_delivery_log(&self, event: TelegramDeliveryEvent<'_>) {
        if let Err(err) = self
            .handlers
            .record_telegram_delivery(NewTelegramDeliveryLog {
                chat_id: event.chat_id,
                job_id: i64::try_from(event.job_id).unwrap_or(i64::MAX),
                final_delivery: event.final_delivery.as_str(),
                preview_edit_attempts: event.preview_stats.edit_attempts as i64,
                preview_edit_successes: event.preview_stats.edit_successes as i64,
                preview_edit_failures: event.preview_stats.edit_failures as i64,
                preview_last_chars: event.preview_stats.last_preview_chars as i64,
                reply_chars: event.reply_chars as i64,
                cancelled: event.cancelled,
                error_code: event.error_code,
            })
        {
            eprintln!("Telegram delivery log save failed: {err}");
        }
    }

    async fn stream_preview_updates(
        &self,
        chat_id: i64,
        message_id: i64,
        mut delta_rx: tokio::sync::mpsc::UnboundedReceiver<String>,
    ) -> StreamPreviewStats {
        let mut stats = StreamPreviewStats::default();
        let mut raw = String::new();
        let mut last_sent = String::new();
        let mut last_edit = Instant::now() - Duration::from_secs(60);
        while let Some(delta) = delta_rx.recv().await {
            raw.push_str(&delta);
            let Some(preview) = streaming_answer_preview(&raw) else {
                continue;
            };
            let preview = first_message_page(&format!("{}\n\n...", preview.trim()));
            if preview.is_empty() || preview == last_sent {
                continue;
            }
            let new_chars = preview
                .chars()
                .count()
                .saturating_sub(last_sent.chars().count());
            if last_edit.elapsed() < Duration::from_millis(TELEGRAM_STREAM_EDIT_INTERVAL_MS)
                && new_chars < TELEGRAM_STREAM_MIN_EDIT_CHARS
            {
                continue;
            }
            stats.edit_attempts += 1;
            match self.edit_message(chat_id, message_id, &preview).await {
                Ok(_) => {
                    stats.edit_successes += 1;
                    stats.last_preview_chars = preview.chars().count();
                    last_sent = preview;
                    last_edit = Instant::now();
                }
                Err(err) => {
                    stats.edit_failures += 1;
                    eprintln!("Telegram stream preview edit failed: {err}");
                }
            }
        }
        stats
    }

    async fn handle_text_blocking(&self, chat_id: i64, text: String) -> Result<String> {
        let handlers = self.handlers.clone();
        tokio::task::spawn_blocking(move || handlers.handle_text(chat_id, &text))
            .await
            .map_err(|err| anyhow!("Telegram blocking handler failed: {err}"))
            .map(|reply| reply.unwrap_or_else(|err| format!("处理失败：{err}")))
    }

    async fn handle_text_streaming<F>(
        &self,
        chat_id: i64,
        job_id: u64,
        text: String,
        on_delta: F,
    ) -> Result<String>
    where
        F: FnMut(&str) -> Result<()>,
    {
        Ok(self
            .handlers
            .handle_text_stream(
                chat_id,
                i64::try_from(job_id).unwrap_or(i64::MAX),
                &text,
                on_delta,
            )
            .await
            .unwrap_or_else(|err| format!("处理失败：{err}")))
    }

    fn mark_job_cancelled(&self, job_id: u64) {
        if let Ok(mut cancelled) = self.cancelled_jobs.lock() {
            cancelled.insert(job_id);
        }
    }

    fn is_job_cancelled(&self, job_id: u64) -> bool {
        self.cancelled_jobs
            .lock()
            .is_ok_and(|cancelled| cancelled.contains(&job_id))
    }

    fn skip_reason(
        &self,
        message: &Message,
        text: &str,
        bot_username: Option<&str>,
    ) -> Option<&'static str> {
        if !self.allowed_chat_ids.is_empty() && !self.allowed_chat_ids.contains(&message.chat.id) {
            return Some("chat is not in TELEGRAM_CHAT_IDS");
        }

        if message.chat.is_private() {
            return None;
        }

        if bot_username.is_some_and(|username| mentions_bot(text, username)) {
            if let Some(reason) =
                group_permission_skip_reason(message, text, bot_username, &self.admin_user_ids)
            {
                return Some(reason);
            }
            return None;
        }

        Some("group message must mention this bot")
    }

    async fn get_me(&self) -> Result<User> {
        let response: TelegramResponse<User> = self
            .http
            .get(format!("https://api.telegram.org/bot{}/getMe", self.token))
            .send()
            .await?
            .error_for_status()?
            .json()
            .await?;
        Ok(response.result)
    }

    async fn get_updates(&self, offset: i64) -> Result<Vec<Update>> {
        let response: TelegramResponse<Vec<Update>> = self
            .http
            .get(format!(
                "https://api.telegram.org/bot{}/getUpdates",
                self.token
            ))
            .query(&[
                ("timeout", TELEGRAM_POLL_TIMEOUT_SECS.to_string()),
                ("offset", offset.to_string()),
            ])
            .send()
            .await?
            .error_for_status()?
            .json()
            .await?;
        Ok(response.result)
    }

    async fn send_message(&self, chat_id: i64, text: &str) -> Result<SentMessage> {
        let request = || {
            self.http
                .post(format!(
                    "https://api.telegram.org/bot{}/sendMessage",
                    self.token
                ))
                .form(&[
                    ("chat_id", chat_id.to_string()),
                    ("text", telegram_message_text(text)),
                    ("disable_web_page_preview", "true".to_string()),
                ])
        };
        let response = send_with_telegram_backoff(request).await?;
        let response: TelegramResponse<SentMessage> = response.json().await?;
        Ok(response.result)
    }

    async fn edit_message(&self, chat_id: i64, message_id: i64, text: &str) -> Result<SentMessage> {
        let request = || {
            self.http
                .post(format!(
                    "https://api.telegram.org/bot{}/editMessageText",
                    self.token
                ))
                .form(&[
                    ("chat_id", chat_id.to_string()),
                    ("message_id", message_id.to_string()),
                    ("text", telegram_message_text(text)),
                    ("disable_web_page_preview", "true".to_string()),
                ])
        };
        let response = send_with_telegram_backoff(request).await?;
        let response: TelegramResponse<SentMessage> = response.json().await?;
        Ok(response.result)
    }

    async fn send_long_message(&self, chat_id: i64, text: &str) -> Result<()> {
        for page in telegram_message_pages(text) {
            self.send_message(chat_id, &page).await?;
            sleep(Duration::from_millis(1050)).await;
        }
        Ok(())
    }

    async fn replace_message_with_long_text(
        &self,
        chat_id: i64,
        message_id: i64,
        text: &str,
    ) -> Result<FinalDelivery> {
        let pages = telegram_message_pages(text);
        let Some(first) = pages.first() else {
            return Ok(FinalDelivery::Empty);
        };
        if let Err(err) = self.edit_message(chat_id, message_id, first).await {
            eprintln!("Telegram final edit failed, sending a new message instead: {err}");
            self.send_long_message(chat_id, text).await?;
            return Ok(FinalDelivery::SentFallback);
        }
        for page in pages.iter().skip(1) {
            sleep(Duration::from_millis(1050)).await;
            self.send_message(chat_id, page).await?;
        }
        Ok(FinalDelivery::EditedPlaceholder)
    }
}

fn http_client(proxy: Option<&str>) -> Result<Client> {
    let mut builder =
        ClientBuilder::new().timeout(Duration::from_secs(TELEGRAM_REQUEST_TIMEOUT_SECS));
    if let Some(proxy) = proxy {
        builder = builder.proxy(Proxy::all(proxy)?);
    }
    Ok(builder.build()?)
}

async fn send_with_telegram_backoff<F>(request: F) -> Result<reqwest::Response>
where
    F: Fn() -> reqwest::RequestBuilder,
{
    let response = request().send().await?;
    if response.status() == reqwest::StatusCode::TOO_MANY_REQUESTS {
        let delay = telegram_retry_after(response.headers());
        sleep(delay).await;
        return Ok(request().send().await?.error_for_status()?);
    }
    Ok(response.error_for_status()?)
}

fn group_permission_skip_reason(
    message: &Message,
    text: &str,
    bot_username: Option<&str>,
    admin_user_ids: &[i64],
) -> Option<&'static str> {
    let handler_text = handler_text_after_bot_mention(text, bot_username);
    if !is_admin_only_telegram_command(handler_text.trim()) {
        return None;
    }
    let sender_id = message.from.as_ref().map(|user| user.id);
    if sender_id.is_some_and(|sender_id| admin_user_ids.contains(&sender_id)) {
        return None;
    }
    Some("group admin-only command requires TELEGRAM_ADMIN_USER_IDS")
}

fn is_admin_only_telegram_command(text: &str) -> bool {
    let Some(command) = telegram_command_name(text) else {
        return false;
    };
    if matches!(
        command,
        "/sync"
            | "/analyze"
            | "/embed"
            | "/embedding"
            | "/comprehend"
            | "/rebuild"
            | "/rebuild_profile"
            | "/profile_rebuild"
    ) {
        return true;
    }
    command == "/profile"
        && text
            .split_whitespace()
            .skip(1)
            .any(|arg| matches!(arg, "--rebuild" | "rebuild"))
}

fn telegram_command_name(text: &str) -> Option<&str> {
    let token = text.split_whitespace().next()?;
    token
        .starts_with('/')
        .then_some(token.split('@').next().unwrap_or(token))
}

fn should_stream_text(text: &str) -> bool {
    let stripped = text.trim();
    stripped.starts_with("/ask") || !stripped.starts_with('/')
}

fn is_dispatcher_cancel_command(text: &str) -> bool {
    let mut parts = text.split_whitespace();
    matches!(parts.next(), Some("/cancel")) && parts.next().is_none()
}

fn telegram_message_text(text: &str) -> String {
    let truncated: String = first_message_page(text);
    if truncated.trim().is_empty() {
        TELEGRAM_STREAM_PLACEHOLDER.to_string()
    } else {
        truncated
    }
}

fn telegram_message_pages(text: &str) -> Vec<String> {
    let mut pages = Vec::new();
    let mut rest = text.trim();
    while !rest.is_empty() {
        let page = first_message_page(rest);
        let page_len = page.chars().count();
        if page_len == 0 {
            break;
        }
        pages.push(page);
        rest = trim_start_chars(rest, page_len).trim_start();
    }
    if pages.is_empty() {
        pages.push(TELEGRAM_STREAM_PLACEHOLDER.to_string());
    }
    pages
}

fn first_message_page(text: &str) -> String {
    let trimmed = text.trim();
    if trimmed.chars().count() <= TELEGRAM_MAX_MESSAGE_CHARS {
        return trimmed.to_string();
    }
    let limited: String = trimmed.chars().take(TELEGRAM_MAX_MESSAGE_CHARS).collect();
    if let Some(index) = limited.rfind("\n\n") {
        if index > TELEGRAM_MAX_MESSAGE_CHARS / 2 {
            return limited[..index].trim().to_string();
        }
    }
    if let Some(index) = limited.rfind('\n') {
        if index > TELEGRAM_MAX_MESSAGE_CHARS / 2 {
            return limited[..index].trim().to_string();
        }
    }
    limited.trim().to_string()
}

fn streaming_answer_preview(raw: &str) -> Option<String> {
    partial_json_string_field(raw, "answer").filter(|answer| !answer.trim().is_empty())
}

fn partial_json_string_field(raw: &str, field: &str) -> Option<String> {
    let needle = format!("\"{field}\"");
    let field_start = raw.find(&needle)?;
    let after_field = &raw[field_start + needle.len()..];
    let colon = after_field.find(':')?;
    let after_colon = after_field[colon + 1..].trim_start();
    let mut chars = after_colon.chars();
    if chars.next()? != '"' {
        return None;
    }
    let mut value = String::new();
    let mut escaped = false;
    let mut unicode_escape = String::new();
    let mut reading_unicode = false;
    for ch in chars {
        if reading_unicode {
            unicode_escape.push(ch);
            if unicode_escape.len() == 4 {
                if let Ok(codepoint) = u32::from_str_radix(&unicode_escape, 16) {
                    if let Some(decoded) = char::from_u32(codepoint) {
                        value.push(decoded);
                    }
                }
                unicode_escape.clear();
                reading_unicode = false;
            }
            continue;
        }
        if escaped {
            match ch {
                '"' => value.push('"'),
                '\\' => value.push('\\'),
                '/' => value.push('/'),
                'b' => value.push('\u{0008}'),
                'f' => value.push('\u{000c}'),
                'n' => value.push('\n'),
                'r' => value.push('\r'),
                't' => value.push('\t'),
                'u' => reading_unicode = true,
                other => value.push(other),
            }
            escaped = false;
            continue;
        }
        match ch {
            '\\' => escaped = true,
            '"' => return Some(value),
            other => value.push(other),
        }
    }
    Some(value)
}

fn trim_start_chars(text: &str, count: usize) -> &str {
    let byte_index = text
        .char_indices()
        .nth(count)
        .map(|(index, _)| index)
        .unwrap_or(text.len());
    &text[byte_index..]
}

fn is_recoverable_telegram_api_error(error: &anyhow::Error) -> bool {
    let Some(error) = error.downcast_ref::<reqwest::Error>() else {
        return false;
    };

    match error.status() {
        Some(status) => {
            status.is_server_error() || status == reqwest::StatusCode::TOO_MANY_REQUESTS
        }
        None => error.is_timeout() || error.is_connect() || error.is_request() || error.is_body(),
    }
}

fn telegram_error_message(error: &anyhow::Error, token: &str) -> String {
    redact_telegram_token(&format!("{error:#}"), token)
}

fn redact_telegram_token(text: &str, token: &str) -> String {
    if token.is_empty() {
        text.to_string()
    } else {
        text.replace(token, "<redacted-telegram-token>")
    }
}

fn telegram_retry_after(headers: &HeaderMap) -> Duration {
    headers
        .get("retry-after")
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.trim().parse::<u64>().ok())
        .map(Duration::from_secs)
        .unwrap_or_else(|| Duration::from_secs(TELEGRAM_DEFAULT_429_BACKOFF_SECS))
}

fn format_bot_username(bot_username: Option<&str>) -> String {
    bot_username
        .map(|username| format!("@{username}"))
        .unwrap_or_else(|| "<unknown username>".to_string())
}

fn format_allowed_chat_ids(allowed_chat_ids: &[i64]) -> String {
    if allowed_chat_ids.is_empty() {
        "all".to_string()
    } else {
        allowed_chat_ids
            .iter()
            .map(i64::to_string)
            .collect::<Vec<_>>()
            .join(",")
    }
}

fn preview_text(text: &str) -> String {
    let mut preview: String = text.chars().take(TELEGRAM_MESSAGE_PREVIEW_CHARS).collect();
    if text.chars().count() > TELEGRAM_MESSAGE_PREVIEW_CHARS {
        preview.push_str("...");
    }
    preview
}

#[derive(Deserialize)]
struct TelegramResponse<T> {
    result: T,
}

#[derive(Deserialize)]
struct Update {
    update_id: i64,
    message: Option<Message>,
    edited_message: Option<Message>,
}

#[derive(Deserialize)]
struct Message {
    from: Option<User>,
    chat: Chat,
    text: Option<String>,
}

#[derive(Deserialize)]
struct SentMessage {
    message_id: i64,
}

#[derive(Deserialize)]
struct Chat {
    id: i64,
    #[serde(rename = "type")]
    kind: String,
}

impl Chat {
    fn is_private(&self) -> bool {
        self.kind == "private"
    }
}

#[derive(Deserialize)]
struct User {
    id: i64,
    username: Option<String>,
}

fn mentions_bot(text: &str, bot_username: &str) -> bool {
    let mention = format!("@{}", bot_username.to_ascii_lowercase());
    text.split_whitespace()
        .any(|word| token_has_bot_mention(word, &mention))
}

fn strip_bot_mention(text: &str, bot_username: Option<&str>) -> String {
    let Some(bot_username) = bot_username else {
        return text.to_string();
    };
    let mention = format!("@{}", bot_username.to_ascii_lowercase());
    text.split_whitespace()
        .map(|word| strip_token_bot_mention(word, &mention))
        .filter(|word| !word.is_empty())
        .collect::<Vec<_>>()
        .join(" ")
}

fn handler_text_after_bot_mention(text: &str, bot_username: Option<&str>) -> String {
    let stripped = strip_bot_mention(text, bot_username);
    if stripped.trim().is_empty()
        && bot_username.is_some_and(|username| mentions_bot(text, username))
    {
        "/help".to_string()
    } else {
        stripped
    }
}

fn token_has_bot_mention(token: &str, mention: &str) -> bool {
    strip_token_bot_mention(token, mention) != token
}

fn strip_token_bot_mention<'a>(token: &'a str, mention: &str) -> &'a str {
    if let Some(index) = token.to_ascii_lowercase().find(mention) {
        let end = index + mention.len();
        if token[end..]
            .chars()
            .next()
            .is_none_or(|ch| !ch.is_ascii_alphanumeric() && ch != '_')
        {
            return token[..index].trim_end_matches(is_command_boundary);
        }
    }
    token
}

fn is_command_boundary(c: char) -> bool {
    !c.is_alphanumeric() && c != '_' && c != '/'
}

#[cfg(test)]
mod tests {
    use super::{
        Chat, FinalDelivery, Message, TELEGRAM_MAX_MESSAGE_CHARS, User,
        group_permission_skip_reason, handler_text_after_bot_mention,
        is_admin_only_telegram_command, is_dispatcher_cancel_command, mentions_bot,
        partial_json_string_field, redact_telegram_token, should_stream_text,
        streaming_answer_preview, strip_bot_mention, telegram_command_name, telegram_message_pages,
        telegram_retry_after,
    };
    use reqwest::header::{HeaderMap, HeaderValue};

    #[test]
    fn detects_plain_group_mention() {
        assert!(mentions_bot(
            "@PaperCheckBot 这篇论文讲什么",
            "PaperCheckBot"
        ));
        assert!(mentions_bot(
            "这篇论文讲什么 @papercheckbot?",
            "PaperCheckBot"
        ));
        assert!(!mentions_bot("这篇论文讲什么", "PaperCheckBot"));
    }

    #[test]
    fn detects_command_mentions() {
        assert!(mentions_bot("/ask@PaperCheckBot 问题", "PaperCheckBot"));
        assert_eq!(
            strip_bot_mention("/ask@PaperCheckBot 问题", Some("PaperCheckBot")),
            "/ask 问题"
        );
    }

    #[test]
    fn bare_group_commands_do_not_count_as_mentions() {
        assert!(!mentions_bot("/ask 问题", "PaperCheckBot"));
        assert!(!mentions_bot("/authors", "PaperCheckBot"));
        assert!(!mentions_bot("/profile", "PaperCheckBot"));
        assert!(!mentions_bot("/sources", "PaperCheckBot"));
        assert!(!mentions_bot("/status", "PaperCheckBot"));
        assert!(!mentions_bot("/jobs", "PaperCheckBot"));
        assert!(!mentions_bot("/cancel", "PaperCheckBot"));
        assert!(!mentions_bot("/ask@OtherBot 问题", "PaperCheckBot"));
    }

    #[test]
    fn strips_plain_group_mention() {
        assert_eq!(
            strip_bot_mention("@PaperCheckBot 这篇论文讲什么", Some("PaperCheckBot")),
            "这篇论文讲什么"
        );
        assert_eq!(
            strip_bot_mention("这篇论文讲什么 @PaperCheckBot", Some("PaperCheckBot")),
            "这篇论文讲什么"
        );
    }

    #[test]
    fn bare_bot_mention_routes_to_help() {
        assert_eq!(
            handler_text_after_bot_mention("@PaperCheckBot", Some("PaperCheckBot")),
            "/help"
        );
        assert_eq!(
            handler_text_after_bot_mention("@PaperCheckBot   ", Some("PaperCheckBot")),
            "/help"
        );
        assert_eq!(
            handler_text_after_bot_mention("@PaperCheckBot /status", Some("PaperCheckBot")),
            "/status"
        );
        assert_eq!(
            handler_text_after_bot_mention("plain text", Some("PaperCheckBot")),
            "plain text"
        );
    }

    #[test]
    fn recognizes_admin_only_group_commands() {
        assert!(is_admin_only_telegram_command("/analyze"));
        assert!(is_admin_only_telegram_command(
            "/analyze@PaperCheckBot Alice"
        ));
        assert!(is_admin_only_telegram_command("/profile --rebuild Alice"));
        assert!(!is_admin_only_telegram_command("/ask question"));
        assert!(!is_admin_only_telegram_command("/status detail"));
        assert_eq!(
            telegram_command_name("/analyze@PaperCheckBot Alice"),
            Some("/analyze")
        );
    }

    #[test]
    fn group_admin_only_commands_require_admin_user_id() {
        let message = Message {
            from: Some(User {
                id: 100,
                username: Some("ordinary".to_string()),
            }),
            chat: Chat {
                id: -7,
                kind: "supergroup".to_string(),
            },
            text: Some("/analyze@PaperCheckBot Alice".to_string()),
        };

        assert_eq!(
            group_permission_skip_reason(
                &message,
                "/analyze@PaperCheckBot Alice",
                Some("PaperCheckBot"),
                &[42],
            ),
            Some("group admin-only command requires TELEGRAM_ADMIN_USER_IDS")
        );
        assert_eq!(
            group_permission_skip_reason(
                &message,
                "/analyze@PaperCheckBot Alice",
                Some("PaperCheckBot"),
                &[100],
            ),
            None
        );
        assert_eq!(
            group_permission_skip_reason(
                &message,
                "/ask@PaperCheckBot question",
                Some("PaperCheckBot"),
                &[]
            ),
            None
        );
    }

    #[test]
    fn paginates_long_messages_without_truncating() {
        let text = format!(
            "{}\n\n{}",
            "A".repeat(TELEGRAM_MAX_MESSAGE_CHARS - 10),
            "B".repeat(50)
        );
        let pages = telegram_message_pages(&text);
        assert_eq!(
            pages.concat().chars().filter(|ch| *ch == 'A').count(),
            TELEGRAM_MAX_MESSAGE_CHARS - 10
        );
        assert_eq!(pages.concat().chars().filter(|ch| *ch == 'B').count(), 50);
        assert!(
            pages
                .iter()
                .all(|page| page.chars().count() <= TELEGRAM_MAX_MESSAGE_CHARS)
        );
    }

    #[test]
    fn does_not_stream_lightweight_commands() {
        assert!(!should_stream_text("/sources"));
        assert!(!should_stream_text("/help"));
        assert!(!should_stream_text("/authors"));
        assert!(!should_stream_text("/status"));
        assert!(!should_stream_text("/jobs"));
        assert!(!should_stream_text("/cancel"));
        assert!(!should_stream_text("/sync Alice"));
        assert!(!should_stream_text("/analyze Alice"));
        assert!(should_stream_text("/ask 问题"));
        assert!(should_stream_text("这篇论文讲什么"));
    }

    #[test]
    fn streaming_preview_extracts_only_answer_field() {
        let raw = r#"{"answer":"这三篇论文分别研究材料合成、COF 和热调节纤维。","claims":[]}"#;

        assert_eq!(
            streaming_answer_preview(raw).as_deref(),
            Some("这三篇论文分别研究材料合成、COF 和热调节纤维。")
        );
    }

    #[test]
    fn streaming_preview_accepts_partial_answer_json() {
        let raw = r#"{"answer":"正在生成一段还没有闭合的回答"#;

        assert_eq!(
            partial_json_string_field(raw, "answer").as_deref(),
            Some("正在生成一段还没有闭合的回答")
        );
    }

    #[test]
    fn final_delivery_labels_are_stable_for_logs() {
        assert_eq!(FinalDelivery::Empty.as_str(), "empty");
        assert_eq!(
            FinalDelivery::EditedPlaceholder.as_str(),
            "edited_placeholder"
        );
        assert_eq!(FinalDelivery::SentFallback.as_str(), "sent_fallback");
        assert_eq!(
            FinalDelivery::SkippedCancelled.as_str(),
            "skipped_cancelled"
        );
        assert_eq!(FinalDelivery::Failed.as_str(), "failed");
    }

    #[test]
    fn only_plain_cancel_targets_active_chat_job() {
        assert!(is_dispatcher_cancel_command("/cancel"));
        assert!(!is_dispatcher_cancel_command("/cancel 12"));
    }

    #[test]
    fn parses_telegram_retry_after_header() {
        let mut headers = HeaderMap::new();
        headers.insert("retry-after", HeaderValue::from_static("7"));
        assert_eq!(telegram_retry_after(&headers).as_secs(), 7);
        assert_eq!(telegram_retry_after(&HeaderMap::new()).as_secs(), 3);
    }

    #[test]
    fn redacts_telegram_token_from_error_messages() {
        let text = redact_telegram_token(
            "https://api.telegram.org/bot123:secret/getUpdates failed",
            "123:secret",
        );

        assert_eq!(
            text,
            "https://api.telegram.org/bot<redacted-telegram-token>/getUpdates failed"
        );
    }

    #[test]
    fn llm_concurrency_limit_has_small_default() {
        assert_eq!(super::TELEGRAM_LLM_CONCURRENCY, 2);
    }
}
