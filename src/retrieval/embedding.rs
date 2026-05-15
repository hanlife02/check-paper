use std::error::Error;
use std::time::Duration;

use anyhow::{Result, anyhow};
use reqwest::Proxy;
use reqwest::blocking::{Client, ClientBuilder};
use serde::{Deserialize, Serialize};

use crate::retrieval::query::query_terms;
use crate::storage::SourceChunk;

pub const LOCAL_HASH_EMBEDDING_MODEL: &str = "local-hash-v1";
pub const LOCAL_HASH_EMBEDDING_DIM: usize = 64;

#[derive(Debug, Clone)]
pub struct EmbeddingConfig {
    pub provider: String,
    pub base_url: String,
    pub api_key: Option<String>,
    pub model: String,
    pub model_version: Option<String>,
    pub proxy: Option<String>,
    pub timeout_secs: u64,
    pub tls_backend: String,
    pub batch_size: usize,
}

#[derive(Clone)]
pub struct OpenAiCompatibleEmbeddingClient {
    config: EmbeddingConfig,
    http: Client,
}

pub trait EmbeddingProvider {
    fn model_name(&self) -> &str;
    fn model_version(&self) -> Option<&str>;
    fn batch_size(&self) -> usize;
    fn embed(&self, input: &[String]) -> Result<Vec<Vec<f32>>>;
}

impl OpenAiCompatibleEmbeddingClient {
    pub fn new(config: EmbeddingConfig) -> Result<Self> {
        if config.provider != "openai-compatible" {
            return Err(anyhow!(
                "unsupported embedding provider `{}`; expected `openai-compatible`",
                config.provider
            ));
        }
        if config.model.trim().is_empty() {
            return Err(anyhow!("missing CHECK_PAPER_EMBEDDING_MODEL"));
        }
        let http = http_client(
            config.proxy.as_deref(),
            config.timeout_secs,
            &config.tls_backend,
        )?;
        Ok(Self { config, http })
    }

    pub fn model_name(&self) -> &str {
        &self.config.model
    }

    pub fn model_version(&self) -> Option<&str> {
        self.config.model_version.as_deref()
    }

    pub fn batch_size(&self) -> usize {
        self.config.batch_size.clamp(1, 256)
    }

    pub fn embed(&self, input: &[String]) -> Result<Vec<Vec<f32>>> {
        let api_key = self
            .config
            .api_key
            .as_deref()
            .ok_or_else(|| anyhow!("missing CHECK_PAPER_EMBEDDING_API_KEY"))?;
        if input.is_empty() {
            return Ok(Vec::new());
        }
        let endpoint = embeddings_endpoint(&self.config.base_url);
        let response = self
            .http
            .post(&endpoint)
            .bearer_auth(api_key)
            .json(&EmbeddingRequest {
                model: self.config.model.clone(),
                input,
            })
            .send()
            .map_err(|error| embedding_send_error(&endpoint, error))?;
        let status = response.status();
        let body = response
            .text()
            .map_err(|error| embedding_read_error(&endpoint, error))?;
        if !status.is_success() {
            return Err(anyhow!(
                "Embedding API returned HTTP {status}: {endpoint}. Body preview: {}",
                preview_body(&body)
            ));
        }
        let mut parsed: EmbeddingResponse = serde_json::from_str(&body).map_err(|error| {
            anyhow!(
                "Embedding API response JSON parse failed: {endpoint}. Error: {error}. Body preview: {}",
                preview_body(&body)
            )
        })?;
        parsed.data.sort_by_key(|item| item.index);
        Ok(parsed.data.into_iter().map(|item| item.embedding).collect())
    }
}

impl EmbeddingProvider for OpenAiCompatibleEmbeddingClient {
    fn model_name(&self) -> &str {
        OpenAiCompatibleEmbeddingClient::model_name(self)
    }

    fn model_version(&self) -> Option<&str> {
        OpenAiCompatibleEmbeddingClient::model_version(self)
    }

    fn batch_size(&self) -> usize {
        OpenAiCompatibleEmbeddingClient::batch_size(self)
    }

    fn embed(&self, input: &[String]) -> Result<Vec<Vec<f32>>> {
        OpenAiCompatibleEmbeddingClient::embed(self, input)
    }
}

pub(crate) fn local_hash_embedding(text: &str) -> Vec<f32> {
    let terms = query_terms(text);
    let mut vector = vec![0.0; LOCAL_HASH_EMBEDDING_DIM];
    for term in terms {
        let mut hash = 1469598103934665603u64;
        for byte in term.to_lowercase().as_bytes() {
            hash ^= *byte as u64;
            hash = hash.wrapping_mul(1099511628211);
        }
        let index = (hash as usize) % LOCAL_HASH_EMBEDDING_DIM;
        vector[index] += 1.0;
    }
    normalize_vector(&mut vector);
    vector
}

pub(crate) fn cosine_similarity(left: &[f32], right: &[f32]) -> f32 {
    left.iter()
        .zip(right.iter())
        .map(|(left, right)| left * right)
        .sum()
}

pub(crate) fn encode_f32_vector(vector: &[f32]) -> Vec<u8> {
    let mut bytes = Vec::with_capacity(std::mem::size_of_val(vector));
    for value in vector {
        bytes.extend_from_slice(&value.to_le_bytes());
    }
    bytes
}

pub(crate) fn decode_f32_vector(bytes: &[u8]) -> Option<Vec<f32>> {
    if !bytes.len().is_multiple_of(std::mem::size_of::<f32>()) {
        return None;
    }
    bytes
        .chunks_exact(std::mem::size_of::<f32>())
        .map(|chunk| Some(f32::from_le_bytes(chunk.try_into().ok()?)))
        .collect()
}

pub(crate) fn rank_vector_chunks_with_scores(
    rows: Vec<(SourceChunk, Vec<u8>)>,
    query_vector: &[f32],
    limit: usize,
    require_same_dim: bool,
) -> Vec<(SourceChunk, f64)> {
    let mut scored = Vec::new();
    for (chunk, vector) in rows {
        if let Some(vector) = decode_f32_vector(&vector) {
            if require_same_dim && vector.len() != query_vector.len() {
                continue;
            }
            let score = cosine_similarity(query_vector, &vector);
            if score > 0.0 {
                scored.push((score as f64, chunk));
            }
        }
    }
    scored.sort_by(|left, right| {
        right
            .0
            .partial_cmp(&left.0)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| left.1.id.cmp(&right.1.id))
    });
    scored
        .into_iter()
        .take(limit)
        .map(|(score, chunk)| (chunk, score))
        .collect()
}

fn normalize_vector(vector: &mut [f32]) {
    let norm = vector
        .iter()
        .map(|value| (*value as f64) * (*value as f64))
        .sum::<f64>()
        .sqrt() as f32;
    if norm > 0.0 {
        for value in vector {
            *value /= norm;
        }
    }
}

fn http_client(proxy: Option<&str>, timeout_secs: u64, tls_backend: &str) -> Result<Client> {
    let mut builder = ClientBuilder::new().timeout(Duration::from_secs(timeout_secs));
    builder = match tls_backend.trim().to_lowercase().as_str() {
        "" | "rustls" => builder.use_rustls_tls(),
        "native" | "native-tls" => builder.use_native_tls(),
        other => {
            return Err(anyhow!(
                "invalid CHECK_PAPER_EMBEDDING_TLS_BACKEND `{other}`; expected `rustls` or `native`"
            ));
        }
    };
    if let Some(proxy) = proxy {
        builder = builder.proxy(Proxy::all(proxy)?);
    }
    Ok(builder.build()?)
}

fn embeddings_endpoint(base_url: &str) -> String {
    format!("{}/embeddings", base_url.trim_end_matches('/'))
}

fn embedding_send_error(endpoint: &str, error: reqwest::Error) -> anyhow::Error {
    let kind = if error.is_timeout() {
        "timeout"
    } else if error.is_connect() {
        "connect failed"
    } else {
        "send failed"
    };
    anyhow!(
        "Embedding API {kind}: {endpoint}. Check CHECK_PAPER_EMBEDDING_BASE_URL, CHECK_PAPER_PROXY, network/DNS, and provider availability. Error: {}",
        format_error_chain(&error)
    )
}

fn embedding_read_error(endpoint: &str, error: reqwest::Error) -> anyhow::Error {
    anyhow!(
        "Embedding API response read failed: {endpoint}. Error: {}",
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
    parts.join("; ")
}

fn preview_body(body: &str) -> String {
    let mut preview: String = body.chars().take(1000).collect();
    if body.chars().count() > 1000 {
        preview.push_str("...");
    }
    if preview.trim().is_empty() {
        "<empty body>".to_string()
    } else {
        preview
    }
}

#[derive(Serialize)]
struct EmbeddingRequest<'a> {
    model: String,
    input: &'a [String],
}

#[derive(Deserialize)]
struct EmbeddingResponse {
    data: Vec<EmbeddingItem>,
}

#[derive(Deserialize)]
struct EmbeddingItem {
    index: usize,
    embedding: Vec<f32>,
}

#[cfg(test)]
mod tests {
    use super::{
        decode_f32_vector, embeddings_endpoint, encode_f32_vector, local_hash_embedding,
        rank_vector_chunks_with_scores,
    };
    use crate::storage::SourceChunk;

    fn chunk(id: i64) -> SourceChunk {
        SourceChunk {
            id,
            paper_key: format!("Alice/paper-{id}"),
            chunk_index: 0,
            section: "Results".to_string(),
            text: format!("chunk {id}"),
            title: "Paper".to_string(),
            doi: String::new(),
            year: "2024".to_string(),
            source_hash: "hash".to_string(),
            chunk_hash: "chunk-hash".to_string(),
            chunker_version: "section-char-v1".to_string(),
            section_kind: "body".to_string(),
            caption_label: None,
        }
    }

    #[test]
    fn builds_embeddings_endpoint_without_double_slashes() {
        assert_eq!(
            embeddings_endpoint("https://api.example.com/v1/"),
            "https://api.example.com/v1/embeddings"
        );
    }

    #[test]
    fn local_hash_embedding_round_trips_through_blob() {
        let vector = local_hash_embedding("MOF catalyst conversion");
        let decoded = decode_f32_vector(&encode_f32_vector(&vector)).unwrap();
        assert_eq!(decoded.len(), vector.len());
        assert!((decoded[0] - vector[0]).abs() < f32::EPSILON);
    }

    #[test]
    fn rank_vector_chunks_orders_by_cosine_similarity() {
        let query = vec![1.0, 0.0];
        let ranked = rank_vector_chunks_with_scores(
            vec![
                (chunk(1), encode_f32_vector(&[0.1, 0.9])),
                (chunk(2), encode_f32_vector(&[0.9, 0.1])),
            ],
            &query,
            5,
            true,
        );

        assert_eq!(ranked[0].0.id, 2);
    }

    #[test]
    fn rank_vector_chunks_can_return_similarity_scores() {
        let query = vec![1.0, 0.0];
        let ranked = rank_vector_chunks_with_scores(
            vec![(chunk(1), encode_f32_vector(&[0.25, 0.75]))],
            &query,
            5,
            true,
        );

        assert_eq!(ranked[0].0.id, 1);
        assert!((ranked[0].1 - 0.25).abs() < f64::EPSILON);
    }
}
