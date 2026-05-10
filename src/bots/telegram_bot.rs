use std::thread;
use std::time::Duration;

use anyhow::Result;
use reqwest::Proxy;
use reqwest::blocking::{Client, ClientBuilder};
use serde::Deserialize;

use super::handlers::BotHandlers;

const TELEGRAM_POLL_TIMEOUT_SECS: u64 = 10;
const TELEGRAM_REQUEST_TIMEOUT_SECS: u64 = TELEGRAM_POLL_TIMEOUT_SECS + 20;
const TELEGRAM_POLL_RETRY_DELAY_SECS: u64 = 3;
const TELEGRAM_MESSAGE_PREVIEW_CHARS: usize = 80;

pub struct TelegramBot<'a> {
    token: String,
    allowed_chat_ids: Vec<i64>,
    handlers: BotHandlers<'a>,
    http: Client,
}

impl<'a> TelegramBot<'a> {
    pub fn new(
        token: String,
        allowed_chat_ids: Vec<i64>,
        proxy: Option<String>,
        handlers: BotHandlers<'a>,
    ) -> Result<Self> {
        Ok(Self {
            token,
            allowed_chat_ids,
            handlers,
            http: http_client(proxy.as_deref())?,
        })
    }

    pub fn run_polling(&self) -> Result<()> {
        let bot_username = self.get_me()?.username;
        eprintln!(
            "Telegram bot started as {}; allowed chats: {}",
            format_bot_username(bot_username.as_deref()),
            format_allowed_chat_ids(&self.allowed_chat_ids)
        );
        let mut offset = 0i64;
        loop {
            let updates = match self.get_updates(offset) {
                Ok(updates) => updates,
                Err(err) if is_recoverable_poll_error(&err) => {
                    eprintln!("Telegram polling temporarily failed: {err}");
                    thread::sleep(Duration::from_secs(TELEGRAM_POLL_RETRY_DELAY_SECS));
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
                let text = strip_bot_mention(&text, bot_username.as_deref());
                let reply = self
                    .handlers
                    .handle_text(&text)
                    .unwrap_or_else(|err| format!("处理失败：{err}"));
                self.send_message(message.chat.id, &reply)?;
            }
            thread::sleep(Duration::from_millis(1000));
        }
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

        if bot_username.is_some_and(|username| mentions_bot(text, username))
            || is_untargeted_bot_command(text)
        {
            return None;
        }

        Some("group message must mention this bot or use /start, /profile, or /ask")
    }

    fn get_me(&self) -> Result<User> {
        let response: TelegramResponse<User> = self
            .http
            .get(format!("https://api.telegram.org/bot{}/getMe", self.token))
            .send()?
            .error_for_status()?
            .json()?;
        Ok(response.result)
    }

    fn get_updates(&self, offset: i64) -> Result<Vec<Update>> {
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
            .send()?
            .error_for_status()?
            .json()?;
        Ok(response.result)
    }

    fn send_message(&self, chat_id: i64, text: &str) -> Result<()> {
        let truncated: String = text.chars().take(3900).collect();
        self.http
            .post(format!(
                "https://api.telegram.org/bot{}/sendMessage",
                self.token
            ))
            .form(&[
                ("chat_id", chat_id.to_string()),
                ("text", truncated),
                ("disable_web_page_preview", "true".to_string()),
            ])
            .send()?
            .error_for_status()?;
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

fn is_untargeted_bot_command(text: &str) -> bool {
    matches!(
        text.split_whitespace().next(),
        Some("/start" | "/profile" | "/ask")
    )
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
    use super::{is_untargeted_bot_command, mentions_bot, strip_bot_mention};

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
    fn detects_untargeted_commands() {
        assert!(is_untargeted_bot_command("/ask 问题"));
        assert!(is_untargeted_bot_command("/profile"));
        assert!(!is_untargeted_bot_command("/ask@OtherBot 问题"));
        assert!(!is_untargeted_bot_command("普通问题"));
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
}
