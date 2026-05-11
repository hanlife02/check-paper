use std::error::Error;
use std::time::Duration;

use anyhow::{Result, anyhow};
use reqwest::Proxy;
use reqwest::blocking::{Client, ClientBuilder};
use serde::{Deserialize, Serialize};

const ERROR_BODY_PREVIEW_CHARS: usize = 1000;

#[derive(Debug, Clone)]
pub struct LlmConfig {
    pub base_url: String,
    pub api_key: Option<String>,
    pub model: String,
    pub proxy: Option<String>,
    pub timeout_secs: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChatMessage {
    pub role: String,
    pub content: String,
}

#[derive(Clone)]
pub struct OpenAiCompatibleClient {
    config: LlmConfig,
    http: Client,
}

impl OpenAiCompatibleClient {
    pub fn new(config: LlmConfig) -> Result<Self> {
        let http = http_client(config.proxy.as_deref(), config.timeout_secs)?;
        Ok(Self { config, http })
    }

    pub fn chat(
        &self,
        messages: Vec<ChatMessage>,
        temperature: f32,
        max_tokens: u32,
    ) -> Result<String> {
        let api_key = self
            .config
            .api_key
            .as_deref()
            .ok_or_else(|| anyhow!("Missing CHECK_PAPER_LLM_API_KEY."))?;
        if self.config.model.trim().is_empty() {
            return Err(anyhow!("Missing CHECK_PAPER_LLM_MODEL."));
        }

        let endpoint = chat_completions_endpoint(&self.config.base_url);
        let response = self
            .http
            .post(&endpoint)
            .bearer_auth(api_key)
            .json(&ChatCompletionRequest {
                model: self.config.model.clone(),
                messages,
                temperature,
                max_tokens,
            })
            .send()
            .map_err(|error| llm_send_error(&endpoint, error))?;
        let status = response.status();
        let body = response
            .text()
            .map_err(|error| llm_read_error(&endpoint, error))?;
        if !status.is_success() {
            return Err(anyhow!(
                "LLM API 返回 HTTP {status}：{endpoint}。响应片段：{}",
                preview_body(&body)
            ));
        }

        let response: ChatCompletionResponse = serde_json::from_str(&body).map_err(|error| {
            anyhow!(
                "LLM API 响应 JSON 解析失败：{endpoint}。底层错误：{error}。响应片段：{}",
                preview_body(&body)
            )
        })?;
        response
            .choices
            .into_iter()
            .next()
            .map(|choice| choice.message.content)
            .ok_or_else(|| anyhow!("LLM API returned no choices"))
    }
}

fn http_client(proxy: Option<&str>, timeout_secs: u64) -> Result<Client> {
    let mut builder = ClientBuilder::new().timeout(Duration::from_secs(timeout_secs));
    if let Some(proxy) = proxy {
        builder = builder.proxy(Proxy::all(proxy)?);
    }
    Ok(builder.build()?)
}

fn chat_completions_endpoint(base_url: &str) -> String {
    format!("{}/chat/completions", base_url.trim_end_matches('/'))
}

fn llm_send_error(endpoint: &str, error: reqwest::Error) -> anyhow::Error {
    let kind = if error.is_timeout() {
        "请求超时"
    } else if error.is_connect() {
        "无法连接"
    } else if error.is_request() {
        "请求构造失败"
    } else {
        "请求发送失败"
    };
    anyhow!(
        "LLM API {kind}：{endpoint}。请检查 CHECK_PAPER_LLM_BASE_URL、CHECK_PAPER_PROXY、生产环境网络/DNS 和服务可用性。底层错误：{}",
        format_error_chain(&error)
    )
}

fn llm_read_error(endpoint: &str, error: reqwest::Error) -> anyhow::Error {
    anyhow!(
        "LLM API 响应读取失败：{endpoint}。底层错误：{}",
        format_error_chain(&error)
    )
}

fn format_error_chain(error: &dyn Error) -> String {
    let mut parts = vec![error.to_string()];
    let mut source = error.source();
    while let Some(error) = source {
        parts.push(error.to_string());
        source = error.source();
    }
    parts.join("；")
}

fn preview_body(body: &str) -> String {
    let mut preview: String = body.chars().take(ERROR_BODY_PREVIEW_CHARS).collect();
    if body.chars().count() > ERROR_BODY_PREVIEW_CHARS {
        preview.push_str("...");
    }
    if preview.trim().is_empty() {
        "<empty body>".to_string()
    } else {
        preview
    }
}

#[derive(Serialize)]
struct ChatCompletionRequest {
    model: String,
    messages: Vec<ChatMessage>,
    temperature: f32,
    max_tokens: u32,
}

#[derive(Deserialize)]
struct ChatCompletionResponse {
    choices: Vec<ChatChoice>,
}

#[derive(Deserialize)]
struct ChatChoice {
    message: ChatChoiceMessage,
}

#[derive(Deserialize)]
struct ChatChoiceMessage {
    content: String,
}

#[cfg(test)]
mod tests {
    use super::{chat_completions_endpoint, preview_body};

    #[test]
    fn builds_chat_completions_endpoint_without_double_slashes() {
        assert_eq!(
            chat_completions_endpoint("https://api.example.com/v1/"),
            "https://api.example.com/v1/chat/completions"
        );
        assert_eq!(
            chat_completions_endpoint("https://api.example.com/v1"),
            "https://api.example.com/v1/chat/completions"
        );
    }

    #[test]
    fn previews_empty_and_long_error_bodies() {
        assert_eq!(preview_body("  "), "<empty body>");
        let long = "a".repeat(1005);
        assert_eq!(preview_body(&long).chars().count(), 1003);
    }
}
