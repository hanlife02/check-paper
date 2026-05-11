use std::error::Error;
use std::time::Duration;

use anyhow::{Result, anyhow};
use reqwest::Proxy;
use reqwest::blocking::{Client, ClientBuilder};
use reqwest::{Client as AsyncClient, ClientBuilder as AsyncClientBuilder};
use serde::{Deserialize, Serialize};
use serde_json::Value;

const ERROR_BODY_PREVIEW_CHARS: usize = 1000;

#[derive(Debug, Clone)]
pub struct LlmConfig {
    pub base_url: String,
    pub api_key: Option<String>,
    pub model: String,
    pub proxy: Option<String>,
    pub timeout_secs: u64,
    pub tls_backend: String,
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
    async_http: AsyncClient,
}

impl OpenAiCompatibleClient {
    pub fn new(config: LlmConfig) -> Result<Self> {
        let http = http_client(
            config.proxy.as_deref(),
            config.timeout_secs,
            &config.tls_backend,
        )?;
        let async_http = async_http_client(
            config.proxy.as_deref(),
            config.timeout_secs,
            &config.tls_backend,
        )?;
        Ok(Self {
            config,
            http,
            async_http,
        })
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

    pub async fn chat_stream<F>(
        &self,
        messages: Vec<ChatMessage>,
        temperature: f32,
        max_tokens: u32,
        mut on_delta: F,
    ) -> Result<String>
    where
        F: FnMut(&str) -> Result<()>,
    {
        let api_key = self
            .config
            .api_key
            .as_deref()
            .ok_or_else(|| anyhow!("Missing CHECK_PAPER_LLM_API_KEY."))?;
        if self.config.model.trim().is_empty() {
            return Err(anyhow!("Missing CHECK_PAPER_LLM_MODEL."));
        }

        let endpoint = chat_completions_endpoint(&self.config.base_url);
        let mut response = self
            .async_http
            .post(&endpoint)
            .bearer_auth(api_key)
            .json(&ChatCompletionStreamRequest {
                model: self.config.model.clone(),
                messages,
                temperature,
                max_tokens,
                stream: true,
            })
            .send()
            .await
            .map_err(|error| llm_send_error(&endpoint, error))?;
        let status = response.status();
        if !status.is_success() {
            let body = response
                .text()
                .await
                .map_err(|error| llm_read_error(&endpoint, error))?;
            return Err(anyhow!(
                "LLM API 返回 HTTP {status}：{endpoint}。响应片段：{}",
                preview_body(&body)
            ));
        }

        let mut parser = SseParser::default();
        let mut full = String::new();
        while let Some(chunk) = response
            .chunk()
            .await
            .map_err(|error| llm_read_error(&endpoint, error))?
        {
            for event in parser.push(&chunk)? {
                match parse_chat_stream_event(&event, &endpoint)? {
                    StreamEvent::Delta(delta) => {
                        full.push_str(&delta);
                        on_delta(&delta)?;
                    }
                    StreamEvent::Done => return Ok(full),
                    StreamEvent::Empty => {}
                }
            }
        }
        for event in parser.finish()? {
            match parse_chat_stream_event(&event, &endpoint)? {
                StreamEvent::Delta(delta) => {
                    full.push_str(&delta);
                    on_delta(&delta)?;
                }
                StreamEvent::Done | StreamEvent::Empty => {}
            }
        }
        Ok(full)
    }
}

fn http_client(proxy: Option<&str>, timeout_secs: u64, tls_backend: &str) -> Result<Client> {
    let mut builder = ClientBuilder::new().timeout(Duration::from_secs(timeout_secs));
    builder = match tls_backend.trim().to_lowercase().as_str() {
        "" | "rustls" => builder.use_rustls_tls(),
        "native" | "native-tls" => builder.use_native_tls(),
        other => {
            return Err(anyhow!(
                "invalid CHECK_PAPER_LLM_TLS_BACKEND `{other}`; expected `rustls` or `native`"
            ));
        }
    };
    if let Some(proxy) = proxy {
        builder = builder.proxy(Proxy::all(proxy)?);
    }
    Ok(builder.build()?)
}

fn async_http_client(
    proxy: Option<&str>,
    timeout_secs: u64,
    tls_backend: &str,
) -> Result<AsyncClient> {
    let mut builder = AsyncClientBuilder::new().timeout(Duration::from_secs(timeout_secs));
    builder = match tls_backend.trim().to_lowercase().as_str() {
        "" | "rustls" => builder.use_rustls_tls(),
        "native" | "native-tls" => builder.use_native_tls(),
        other => {
            return Err(anyhow!(
                "invalid CHECK_PAPER_LLM_TLS_BACKEND `{other}`; expected `rustls` or `native`"
            ));
        }
    };
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

#[derive(Serialize)]
struct ChatCompletionStreamRequest {
    model: String,
    messages: Vec<ChatMessage>,
    temperature: f32,
    max_tokens: u32,
    stream: bool,
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

#[derive(Default)]
struct SseParser {
    buffer: Vec<u8>,
}

impl SseParser {
    fn push(&mut self, chunk: &[u8]) -> Result<Vec<String>> {
        self.buffer.extend_from_slice(chunk);
        self.drain_events(false)
    }

    fn finish(&mut self) -> Result<Vec<String>> {
        if self.buffer.is_empty() {
            Ok(Vec::new())
        } else {
            self.drain_events(true)
        }
    }

    fn drain_events(&mut self, include_partial: bool) -> Result<Vec<String>> {
        let mut events = Vec::new();
        while let Some((index, separator_len)) = find_sse_separator(&self.buffer) {
            let raw = String::from_utf8(self.buffer[..index].to_vec())
                .map_err(|error| anyhow!("LLM API stream returned invalid UTF-8: {error}"))?;
            self.buffer.drain(..index + separator_len);
            if let Some(event) = sse_data(&raw) {
                events.push(event);
            }
        }
        if include_partial {
            let raw = std::mem::take(&mut self.buffer);
            let raw = String::from_utf8(raw)
                .map_err(|error| anyhow!("LLM API stream returned invalid UTF-8: {error}"))?;
            if let Some(event) = sse_data(&raw) {
                events.push(event);
            }
        }
        Ok(events)
    }
}

fn find_sse_separator(buffer: &[u8]) -> Option<(usize, usize)> {
    if let Some(index) = buffer.windows(4).position(|window| window == b"\r\n\r\n") {
        Some((index, 4))
    } else {
        buffer
            .windows(2)
            .position(|window| window == b"\n\n")
            .map(|index| (index, 2))
    }
}

fn sse_data(raw: &str) -> Option<String> {
    let data = raw
        .lines()
        .filter_map(|line| line.strip_prefix("data:"))
        .map(str::trim_start)
        .collect::<Vec<_>>()
        .join("\n");
    if data.is_empty() { None } else { Some(data) }
}

enum StreamEvent {
    Delta(String),
    Done,
    Empty,
}

fn parse_chat_stream_event(data: &str, endpoint: &str) -> Result<StreamEvent> {
    if data.trim() == "[DONE]" {
        return Ok(StreamEvent::Done);
    }
    let value: Value = serde_json::from_str(data).map_err(|error| {
        anyhow!(
            "LLM API stream JSON 解析失败：{endpoint}。底层错误：{error}。响应片段：{}",
            preview_body(data)
        )
    })?;
    let Some(delta) = value
        .get("choices")
        .and_then(Value::as_array)
        .and_then(|choices| choices.first())
        .and_then(|choice| choice.get("delta"))
        .and_then(|delta| delta.get("content"))
        .and_then(Value::as_str)
    else {
        return Ok(StreamEvent::Empty);
    };
    Ok(StreamEvent::Delta(delta.to_string()))
}

#[cfg(test)]
mod tests {
    use std::io::{Read, Write};
    use std::net::{TcpListener, TcpStream};
    use std::thread::{self, JoinHandle};
    use std::time::Duration;

    use super::{
        ChatMessage, LlmConfig, OpenAiCompatibleClient, SseParser, chat_completions_endpoint,
        preview_body,
    };

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

    #[test]
    fn parses_sse_events_across_chunks() {
        let mut parser = SseParser::default();
        assert!(parser.push(b"data: {\"a\"").unwrap().is_empty());
        assert_eq!(
            parser.push(b":1}\n\ndata: [DONE]\n\n").unwrap(),
            vec!["{\"a\":1}", "[DONE]"]
        );
    }

    #[test]
    fn streams_chat_completion_from_local_server() {
        let (base_url, handle) = start_mock_server(concat!(
            "data: {\"choices\":[{\"delta\":{\"content\":\"hel\"}}]}\n\n",
            "data: {\"choices\":[{\"delta\":{\"content\":\"lo\"}}]}\n\n",
            "data: [DONE]\n\n"
        ));
        let llm = OpenAiCompatibleClient::new(LlmConfig {
            base_url,
            api_key: Some("test-key".to_string()),
            model: "test-model".to_string(),
            proxy: None,
            timeout_secs: 5,
            tls_backend: "rustls".to_string(),
        })
        .unwrap();
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let mut deltas = Vec::new();
        let full = runtime
            .block_on(llm.chat_stream(
                vec![ChatMessage {
                    role: "user".to_string(),
                    content: "hello".to_string(),
                }],
                0.0,
                8,
                |delta| {
                    deltas.push(delta.to_string());
                    Ok(())
                },
            ))
            .unwrap();

        assert_eq!(full, "hello");
        assert_eq!(deltas, vec!["hel", "lo"]);
        handle.join().unwrap();
    }

    fn start_mock_server(body: &'static str) -> (String, JoinHandle<()>) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let handle = thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let request = read_http_request(&mut stream);
            assert!(request.starts_with("POST /chat/completions "));
            assert!(request.contains("authorization: Bearer test-key"));
            assert!(request.contains("\"stream\":true"));

            let response = format!(
                "HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{}",
                body.len(),
                body
            );
            stream.write_all(response.as_bytes()).unwrap();
        });
        (format!("http://{address}"), handle)
    }

    fn read_http_request(stream: &mut TcpStream) -> String {
        stream
            .set_read_timeout(Some(Duration::from_secs(2)))
            .unwrap();
        let mut request = Vec::new();
        let mut buffer = [0u8; 4096];
        loop {
            let count = stream.read(&mut buffer).unwrap();
            if count == 0 {
                break;
            }
            request.extend_from_slice(&buffer[..count]);
            if request_body_complete(&request) {
                break;
            }
        }
        String::from_utf8_lossy(&request).to_string()
    }

    fn request_body_complete(request: &[u8]) -> bool {
        let Some(header_end) = request.windows(4).position(|window| window == b"\r\n\r\n") else {
            return false;
        };
        let headers = String::from_utf8_lossy(&request[..header_end]);
        let content_length = headers
            .lines()
            .find_map(|line| {
                line.to_ascii_lowercase()
                    .strip_prefix("content-length:")
                    .and_then(|value| value.trim().parse::<usize>().ok())
            })
            .unwrap_or(0);
        request.len() >= header_end + 4 + content_length
    }
}
