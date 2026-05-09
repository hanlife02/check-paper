use std::thread;
use std::time::Duration;

use anyhow::Result;
use reqwest::Proxy;
use reqwest::blocking::{Client, ClientBuilder};
use serde::Deserialize;

use super::handlers::BotHandlers;

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
        let mut offset = 0i64;
        loop {
            let updates = self.get_updates(offset)?;
            for update in updates {
                offset = offset.max(update.update_id + 1);
                let Some(message) = update.message.or(update.edited_message) else {
                    continue;
                };
                let Some(text) = message.text.as_deref() else {
                    continue;
                };
                if !self.can_handle_message(&message, &text, bot_username.as_deref()) {
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

    fn can_handle_message(
        &self,
        message: &Message,
        text: &str,
        bot_username: Option<&str>,
    ) -> bool {
        if !self.allowed_chat_ids.is_empty() && !self.allowed_chat_ids.contains(&message.chat.id) {
            return false;
        }

        if message.chat.is_private() {
            return true;
        }

        bot_username.is_some_and(|username| mentions_bot(text, username))
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
                ("timeout", "30".to_string()),
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
    let mut builder = ClientBuilder::new();
    if let Some(proxy) = proxy {
        builder = builder.proxy(Proxy::all(proxy)?);
    }
    Ok(builder.build()?)
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
    use super::{mentions_bot, strip_bot_mention};

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
