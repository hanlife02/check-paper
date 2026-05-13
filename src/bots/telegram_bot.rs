use std::collections::HashSet;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use anyhow::{Result, anyhow};
use reqwest::header::HeaderMap;
use reqwest::{Client, ClientBuilder, Proxy};
use serde::Deserialize;
use tokio::sync::Semaphore;
use tokio::sync::mpsc::{UnboundedReceiver, UnboundedSender, unbounded_channel};
use tokio::time::{Instant, sleep};

use super::dispatcher::{ChatDispatcher, DispatchAction};
use super::handlers::BotHandlers;

const TELEGRAM_POLL_TIMEOUT_SECS: u64 = 10;
const TELEGRAM_REQUEST_TIMEOUT_SECS: u64 = TELEGRAM_POLL_TIMEOUT_SECS + 20;
const TELEGRAM_POLL_RETRY_DELAY_SECS: u64 = 3;
const TELEGRAM_MESSAGE_PREVIEW_CHARS: usize = 80;
const TELEGRAM_MAX_MESSAGE_CHARS: usize = 3900;
const TELEGRAM_STREAM_EDIT_INTERVAL_MS: u64 = 1100;
const TELEGRAM_STREAM_PLACEHOLDER: &str = "处理中...";
const TELEGRAM_DEFAULT_429_BACKOFF_SECS: u64 = 3;
const TELEGRAM_LLM_CONCURRENCY: usize = 2;

#[derive(Clone)]
pub struct TelegramBot {
    token: String,
    allowed_chat_ids: Vec<i64>,
    handlers: BotHandlers,
    http: Client,
    cancelled_jobs: Arc<Mutex<HashSet<u64>>>,
    llm_semaphore: Arc<Semaphore>,
}

impl TelegramBot {
    pub fn new(
        token: String,
        allowed_chat_ids: Vec<i64>,
        proxy: Option<String>,
        handlers: BotHandlers,
    ) -> Result<Self> {
        Ok(Self {
            token,
            allowed_chat_ids,
            handlers,
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
        let bot_username = self.get_me().await?.username;
        eprintln!(
            "Telegram bot started as {}; allowed chats: {}",
            format_bot_username(bot_username.as_deref()),
            format_allowed_chat_ids(&self.allowed_chat_ids)
        );
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
                Err(err) if is_recoverable_poll_error(&err) => {
                    eprintln!("Telegram polling temporarily failed: {err}");
                    sleep(Duration::from_secs(TELEGRAM_POLL_RETRY_DELAY_SECS)).await;
                    continue;
                }
                Err(err) => return Err(err),
            };
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
                let text = strip_bot_mention(text, bot_username.as_deref());
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
            let reply = self
                .handlers
                .handle_text(chat_id, &text)
                .unwrap_or_else(|err| format!("处理失败：{err}"));
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
        let editor = TelegramMessageEditor {
            token: self.token.clone(),
            http: self.http.clone(),
            chat_id,
            message_id: placeholder.message_id,
        };
        let (tx, rx) = unbounded_channel();
        let edit_task = tokio::spawn(stream_message_updates(editor.clone(), rx));
        let stream_tx = tx.clone();
        let cancelled_jobs = self.cancelled_jobs.clone();
        let reply = self
            .handlers
            .handle_text_stream(chat_id, text, move |delta| {
                if cancelled_jobs
                    .lock()
                    .is_ok_and(|cancelled| cancelled.contains(&job_id))
                {
                    return Err(anyhow!("cancelled"));
                }
                let _ = stream_tx.send(delta.to_string());
                Ok(())
            })
            .await
            .unwrap_or_else(|err| format!("处理失败：{err}"));
        drop(tx);

        let edit_state = match edit_task.await {
            Ok(state) => state,
            Err(err) => {
                eprintln!("Telegram stream edit task failed: {err}");
                StreamEditState::default()
            }
        };
        if self.is_job_cancelled(job_id) {
            return Ok(());
        }
        let final_text = telegram_message_text(&reply);
        if final_text != edit_state.last_sent {
            if let Err(err) = editor.edit(&final_text).await {
                eprintln!("Telegram final edit failed, sending a new message instead: {err}");
                self.send_long_message(chat_id, &reply).await?;
            }
        } else {
            let pages = telegram_message_pages(&reply);
            for page in pages.into_iter().skip(1) {
                if self.is_job_cancelled(job_id) {
                    break;
                }
                self.send_message(chat_id, &page).await?;
            }
        }
        Ok(())
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

    async fn send_long_message(&self, chat_id: i64, text: &str) -> Result<()> {
        for page in telegram_message_pages(text) {
            self.send_message(chat_id, &page).await?;
            sleep(Duration::from_millis(1050)).await;
        }
        Ok(())
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

#[derive(Clone)]
struct TelegramMessageEditor {
    token: String,
    http: Client,
    chat_id: i64,
    message_id: i64,
}

impl TelegramMessageEditor {
    async fn edit(&self, text: &str) -> Result<()> {
        let request = || {
            self.http
                .post(format!(
                    "https://api.telegram.org/bot{}/editMessageText",
                    self.token
                ))
                .form(&[
                    ("chat_id", self.chat_id.to_string()),
                    ("message_id", self.message_id.to_string()),
                    ("text", telegram_message_text(text)),
                    ("disable_web_page_preview", "true".to_string()),
                ])
        };
        send_with_telegram_backoff(request).await?;
        Ok(())
    }
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

#[derive(Default)]
struct StreamEditState {
    buffer: String,
    last_sent: String,
}

impl StreamEditState {
    fn new() -> Self {
        Self {
            buffer: String::new(),
            last_sent: TELEGRAM_STREAM_PLACEHOLDER.to_string(),
        }
    }
}

async fn stream_message_updates(
    editor: TelegramMessageEditor,
    mut rx: UnboundedReceiver<String>,
) -> StreamEditState {
    let mut state = StreamEditState::new();
    let mut next_edit_at = Instant::now();
    while let Some(delta) = rx.recv().await {
        state.buffer.push_str(&delta);
        if Instant::now() < next_edit_at {
            continue;
        }

        let text = telegram_message_text(&state.buffer);
        if text != state.last_sent {
            if let Err(err) = editor.edit(&text).await {
                eprintln!("Telegram stream edit failed: {err}");
            } else {
                state.last_sent = text;
            }
        }
        next_edit_at = Instant::now() + Duration::from_millis(TELEGRAM_STREAM_EDIT_INTERVAL_MS);
    }

    let text = telegram_message_text(&state.buffer);
    if !text.is_empty() && text != state.last_sent {
        if let Err(err) = editor.edit(&text).await {
            eprintln!("Telegram stream edit failed: {err}");
        } else {
            state.last_sent = text;
        }
    }
    state
}

fn should_stream_text(text: &str) -> bool {
    let stripped = text.trim();
    !stripped.starts_with("/start")
        && !stripped.starts_with("/profile")
        && !stripped.starts_with("/sources")
        && !stripped.starts_with("/status")
        && !stripped.starts_with("/jobs")
        && !stripped.starts_with("/cancel")
        && !stripped.starts_with("/use_author")
        && !stripped.starts_with("/current_author")
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

fn trim_start_chars(text: &str, count: usize) -> &str {
    let byte_index = text
        .char_indices()
        .nth(count)
        .map(|(index, _)| index)
        .unwrap_or(text.len());
    &text[byte_index..]
}

fn is_recoverable_poll_error(error: &anyhow::Error) -> bool {
    let Some(error) = error.downcast_ref::<reqwest::Error>() else {
        return false;
    };

    if error.is_timeout() || error.is_connect() {
        return true;
    }

    error.status().is_some_and(|status| {
        status.is_server_error() || status == reqwest::StatusCode::TOO_MANY_REQUESTS
    })
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
        TELEGRAM_MAX_MESSAGE_CHARS, is_dispatcher_cancel_command, mentions_bot, should_stream_text,
        strip_bot_mention, telegram_message_pages, telegram_retry_after,
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
        assert!(!should_stream_text("/status"));
        assert!(!should_stream_text("/jobs"));
        assert!(!should_stream_text("/cancel"));
        assert!(should_stream_text("/ask 问题"));
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
    fn llm_concurrency_limit_has_small_default() {
        assert_eq!(super::TELEGRAM_LLM_CONCURRENCY, 2);
    }
}
