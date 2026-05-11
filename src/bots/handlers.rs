use anyhow::{Result, anyhow};
use serde_json::Value;

use crate::qa::answerer::Answerer;
use crate::storage::Storage;

pub struct BotHandlers<'a> {
    storage: &'a Storage,
    answerer: Answerer<'a>,
    default_author: Option<String>,
}

impl<'a> BotHandlers<'a> {
    pub fn new(
        storage: &'a Storage,
        answerer: Answerer<'a>,
        default_author: Option<String>,
    ) -> Self {
        Self {
            storage,
            answerer,
            default_author,
        }
    }

    pub fn handle_text(&self, text: &str) -> Result<String> {
        let stripped = text.trim();
        if stripped.starts_with("/start") {
            return Ok(start_message());
        }
        if stripped.starts_with("/profile") {
            let author = stripped.trim_start_matches("/profile").trim();
            let author = if author.is_empty() {
                self.default_author.as_deref()
            } else {
                Some(author)
            };
            return self.profile(author);
        }
        if stripped.starts_with("/ask") {
            let body = stripped.trim_start_matches("/ask").trim();
            let (author, question) = self.parse_author_question(body)?;
            return self.answerer.answer(&author, &question);
        }
        let Some(author) = self.default_author.as_deref() else {
            return Ok(
                "请先设置 CHECK_PAPER_DEFAULT_AUTHOR，或使用 `/ask 作者 | 问题`。".to_string(),
            );
        };
        self.answerer.answer(author, stripped)
    }

    pub async fn handle_text_stream<F>(&self, text: &str, on_delta: F) -> Result<String>
    where
        F: FnMut(&str) -> Result<()>,
    {
        let stripped = text.trim();
        if stripped.starts_with("/start") {
            return Ok(start_message());
        }
        if stripped.starts_with("/profile") {
            let author = stripped.trim_start_matches("/profile").trim();
            let author = if author.is_empty() {
                self.default_author.as_deref()
            } else {
                Some(author)
            };
            return self.profile(author);
        }
        if stripped.starts_with("/ask") {
            let body = stripped.trim_start_matches("/ask").trim();
            let (author, question) = self.parse_author_question(body)?;
            return self
                .answerer
                .answer_stream(&author, &question, on_delta)
                .await;
        }
        let Some(author) = self.default_author.as_deref() else {
            return Ok(
                "请先设置 CHECK_PAPER_DEFAULT_AUTHOR，或使用 `/ask 作者 | 问题`。".to_string(),
            );
        };
        self.answerer
            .answer_stream(author, stripped, on_delta)
            .await
    }

    fn profile(&self, author: Option<&str>) -> Result<String> {
        let Some(author) = author else {
            return Ok("请指定作者，例如 `/profile Ruqiang ZOU`。".to_string());
        };
        if let Some(profile) = self.storage.get_author_profile(author)? {
            Ok(format_profile(&profile))
        } else {
            let count = self.storage.count_papers(Some(author))?;
            Ok(format!(
                "还没有作者画像。当前已入库 {count} 篇论文；请先运行 analyze 或 sync。"
            ))
        }
    }

    fn parse_author_question(&self, body: &str) -> Result<(String, String)> {
        if let Some((author, question)) = body.split_once('|') {
            let author = author.trim();
            let question = question.trim();
            if !author.is_empty() && !question.is_empty() {
                return Ok((author.to_string(), question.to_string()));
            }
        }
        let Some(author) = self.default_author.as_ref() else {
            return Err(anyhow!(
                "请使用 `/ask 作者 | 问题`，或设置 CHECK_PAPER_DEFAULT_AUTHOR。"
            ));
        };
        if body.trim().is_empty() {
            return Err(anyhow!("请在 /ask 后输入问题。"));
        }
        Ok((author.clone(), body.trim().to_string()))
    }
}

fn start_message() -> String {
    "check-paper 已启动。\n用法：\n/profile\n/ask 你的问题\n/ask Ruqiang ZOU | 你的问题".to_string()
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
