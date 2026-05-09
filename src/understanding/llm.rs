use anyhow::{Result, anyhow};
use reqwest::Proxy;
use reqwest::blocking::{Client, ClientBuilder};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone)]
pub struct LlmConfig {
    pub base_url: String,
    pub api_key: Option<String>,
    pub model: String,
    pub proxy: Option<String>,
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
        let http = http_client(config.proxy.as_deref())?;
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

        let response: ChatCompletionResponse = self
            .http
            .post(format!("{}/chat/completions", self.config.base_url))
            .bearer_auth(api_key)
            .json(&ChatCompletionRequest {
                model: self.config.model.clone(),
                messages,
                temperature,
                max_tokens,
            })
            .send()?
            .error_for_status()?
            .json()?;
        response
            .choices
            .into_iter()
            .next()
            .map(|choice| choice.message.content)
            .ok_or_else(|| anyhow!("LLM API returned no choices"))
    }
}

fn http_client(proxy: Option<&str>) -> Result<Client> {
    let mut builder = ClientBuilder::new();
    if let Some(proxy) = proxy {
        builder = builder.proxy(Proxy::all(proxy)?);
    }
    Ok(builder.build()?)
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
