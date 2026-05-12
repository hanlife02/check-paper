use std::time::Instant;

use anyhow::Result;
use serde_json::json;
use sha2::{Digest, Sha256};

use crate::qa::renderer::render_qa_answer;
use crate::qa::verifier::{parse_qa_answer, verify_qa_answer};
use crate::retrieval::embedding::OpenAiCompatibleEmbeddingClient;
use crate::retrieval::profile_route::search_profiles_for_query;
use crate::schemas::qa_answer::{QA_ANSWER_SCHEMA_VERSION, signals_insufficient};
use crate::storage::{QaLogMetadata, Storage};
use crate::understanding::llm::{LlmUsage, OpenAiCompatibleClient};
use crate::understanding::prompts::{QA_PROMPT_VERSION, qa_messages, qa_repair_messages};

const PROFILE_CONTEXT_LIMIT: usize = 8;
const DEFAULT_CHUNK_LIMIT: usize = 8;
const RETRY_CHUNK_LIMIT: usize = 10;
const QA_CHUNK_LIMIT_ENV: &str = "CHECK_PAPER_QA_CHUNK_LIMIT";

pub struct Answerer<'a> {
    storage: &'a Storage,
    llm: OpenAiCompatibleClient,
    embedding: Option<OpenAiCompatibleEmbeddingClient>,
}

impl<'a> Answerer<'a> {
    pub fn new(storage: &'a Storage, llm: OpenAiCompatibleClient) -> Self {
        Self {
            storage,
            llm,
            embedding: None,
        }
    }

    pub fn new_with_embedding(
        storage: &'a Storage,
        llm: OpenAiCompatibleClient,
        embedding: Option<OpenAiCompatibleEmbeddingClient>,
    ) -> Self {
        Self {
            storage,
            llm,
            embedding,
        }
    }

    pub fn answer(&self, author: &str, question: &str) -> Result<String> {
        let started = Instant::now();
        let profiles =
            search_profiles_for_query(self.storage, author, question, PROFILE_CONTEXT_LIMIT)?;
        let chunk_limit = qa_chunk_limit();
        let (mut chunks, mut retrieval_trace) =
            self.context_chunks(author, question, &profiles, chunk_limit)?;

        let first =
            self.llm
                .chat_with_usage(qa_messages(question, &profiles, &chunks), 0.2, 2200)?;
        if should_retry_with_more_chunks(&first.content, &chunks) {
            (chunks, retrieval_trace) =
                self.context_chunks(author, question, &profiles, retry_chunk_limit(chunk_limit))?;
            let second =
                self.llm
                    .chat_with_usage(qa_messages(question, &profiles, &chunks), 0.2, 2600)?;
            let repaired = match self.valid_or_repaired_answer(&second.content, &chunks) {
                Ok(answer) => answer,
                Err(err) => {
                    self.log_failed_answer(
                        author,
                        question,
                        &profiles,
                        &chunks,
                        &retrieval_trace,
                        &second.content,
                        &err.to_string(),
                        started,
                        2600,
                        Some(&second.usage),
                    )?;
                    return Err(err);
                }
            };
            self.log_answer(
                author,
                question,
                &profiles,
                &chunks,
                &retrieval_trace,
                &repaired,
                started,
                2600,
                Some(&second.usage),
            )?;
            return Ok(render_qa_answer(&repaired));
        }
        let repaired = match self.valid_or_repaired_answer(&first.content, &chunks) {
            Ok(answer) => answer,
            Err(err) => {
                self.log_failed_answer(
                    author,
                    question,
                    &profiles,
                    &chunks,
                    &retrieval_trace,
                    &first.content,
                    &err.to_string(),
                    started,
                    2200,
                    Some(&first.usage),
                )?;
                return Err(err);
            }
        };
        self.log_answer(
            author,
            question,
            &profiles,
            &chunks,
            &retrieval_trace,
            &repaired,
            started,
            2200,
            Some(&first.usage),
        )?;
        Ok(render_qa_answer(&repaired))
    }

    pub async fn answer_stream<F>(
        &self,
        author: &str,
        question: &str,
        mut on_delta: F,
    ) -> Result<String>
    where
        F: FnMut(&str) -> Result<()>,
    {
        let started = Instant::now();
        let profiles =
            search_profiles_for_query(self.storage, author, question, PROFILE_CONTEXT_LIMIT)?;
        let chunk_limit = qa_chunk_limit();
        let (mut chunks, mut retrieval_trace) =
            self.context_chunks(author, question, &profiles, chunk_limit)?;

        let first = self
            .llm
            .chat_stream(
                qa_messages(question, &profiles, &chunks),
                0.2,
                2200,
                |delta| on_delta(delta),
            )
            .await?;
        if should_retry_with_more_chunks(&first, &chunks) {
            (chunks, retrieval_trace) =
                self.context_chunks(author, question, &profiles, retry_chunk_limit(chunk_limit))?;
            let second = self
                .llm
                .chat_stream(
                    qa_messages(question, &profiles, &chunks),
                    0.2,
                    2600,
                    |delta| on_delta(delta),
                )
                .await?;
            let repaired = match self.valid_or_repaired_answer(&second, &chunks) {
                Ok(answer) => answer,
                Err(err) => {
                    self.log_failed_answer(
                        author,
                        question,
                        &profiles,
                        &chunks,
                        &retrieval_trace,
                        &second,
                        &err.to_string(),
                        started,
                        2600,
                        None,
                    )?;
                    return Err(err);
                }
            };
            self.log_answer(
                author,
                question,
                &profiles,
                &chunks,
                &retrieval_trace,
                &repaired,
                started,
                2600,
                None,
            )?;
            return Ok(render_qa_answer(&repaired));
        }
        let repaired = match self.valid_or_repaired_answer(&first, &chunks) {
            Ok(answer) => answer,
            Err(err) => {
                self.log_failed_answer(
                    author,
                    question,
                    &profiles,
                    &chunks,
                    &retrieval_trace,
                    &first,
                    &err.to_string(),
                    started,
                    2200,
                    None,
                )?;
                return Err(err);
            }
        };
        self.log_answer(
            author,
            question,
            &profiles,
            &chunks,
            &retrieval_trace,
            &repaired,
            started,
            2200,
            None,
        )?;
        Ok(render_qa_answer(&repaired))
    }

    fn context_chunks(
        &self,
        author: &str,
        question: &str,
        profiles: &[serde_json::Value],
        limit: usize,
    ) -> Result<(Vec<crate::storage::SourceChunk>, serde_json::Value)> {
        let (mut chunks, mut trace) = self.search_chunks(author, question, limit)?;
        if chunks.len() < limit {
            let paper_keys = profiles
                .iter()
                .filter_map(|profile| profile.get("paper_key").and_then(serde_json::Value::as_str))
                .map(str::to_string)
                .collect::<Vec<_>>();
            let remaining = limit - chunks.len();
            let profile_chunks = self.storage.chunks_for_paper_keys(&paper_keys, remaining)?;
            if let Some(object) = trace.as_object_mut() {
                object.insert(
                    "profile_fill_count".to_string(),
                    profile_chunks.len().into(),
                );
            }
            append_unique_chunks(&mut chunks, profile_chunks, limit);
        }
        if chunks.is_empty() {
            chunks = self.storage.recent_chunks(author, limit)?;
            trace = json!({
                "routes": {
                    "recent_fallback": chunks.iter().enumerate().map(|(rank, chunk)| json!({
                        "rank": rank + 1,
                        "chunk_id": chunk.id,
                        "paper_key": chunk.paper_key,
                        "chunk_index": chunk.chunk_index,
                        "section": chunk.section,
                    })).collect::<Vec<_>>()
                },
                "fusion": []
            });
        }
        Ok((chunks, trace))
    }

    fn search_chunks(
        &self,
        author: &str,
        question: &str,
        limit: usize,
    ) -> Result<(Vec<crate::storage::SourceChunk>, serde_json::Value)> {
        let Some(embedding) = self.embedding.as_ref() else {
            return self
                .storage
                .search_chunks_with_trace(author, question, limit);
        };
        let query_vectors = embedding.embed(&[question.to_string()])?;
        let Some(query_vector) = query_vectors.first() else {
            return self
                .storage
                .search_chunks_with_trace(author, question, limit);
        };
        self.storage.search_chunks_with_dense_vector_trace(
            author,
            question,
            limit,
            embedding.model_name(),
            embedding.model_version(),
            query_vector,
        )
    }

    fn log_answer(
        &self,
        author: &str,
        question: &str,
        profiles: &[serde_json::Value],
        chunks: &[crate::storage::SourceChunk],
        retrieval_trace: &serde_json::Value,
        raw_answer: &str,
        started: Instant,
        max_tokens: i64,
        usage: Option<&LlmUsage>,
    ) -> Result<()> {
        let retrieval = json!({
            "profile_count": profiles.len(),
            "chunks": chunks.iter().map(|chunk| json!({
                "paper_key": chunk.paper_key,
                "title": chunk.title,
                "doi": chunk.doi,
                "year": chunk.year,
                "chunk_id": chunk.id,
                "chunk_index": chunk.chunk_index,
                "section": chunk.section,
                "section_kind": chunk.section_kind,
                "caption_label": chunk.caption_label,
                "source_hash": chunk.source_hash,
                "chunk_hash": stored_chunk_hash(chunk),
                "chunker_version": chunk.chunker_version,
            })).collect::<Vec<_>>(),
            "trace": retrieval_trace,
        });
        let mut answer_json = parse_qa_answer(raw_answer)
            .map(|answer| {
                serde_json::to_value(answer).unwrap_or_else(|_| json!({ "raw": raw_answer }))
            })
            .unwrap_or_else(|| json!({ "raw": raw_answer }));
        let snapshot = evidence_snapshot(&answer_json, chunks);
        if let Some(object) = answer_json.as_object_mut() {
            object.insert(
                "answer_schema_version".to_string(),
                QA_ANSWER_SCHEMA_VERSION.into(),
            );
            object.insert("evidence_snapshot".to_string(), snapshot);
        }
        self.storage.save_qa_log_with_metadata(
            author,
            question,
            &retrieval,
            &answer_json,
            self.llm.model_name(),
            started.elapsed().as_millis() as i64,
            Some(QaLogMetadata {
                answer_schema_version: Some(QA_ANSWER_SCHEMA_VERSION),
                qa_prompt_version: Some(QA_PROMPT_VERSION),
                temperature: Some(0.2),
                max_tokens: Some(max_tokens),
                prompt_tokens: usage.and_then(|usage| usage.prompt_tokens),
                completion_tokens: usage.and_then(|usage| usage.completion_tokens),
                total_tokens: usage.and_then(|usage| usage.total_tokens),
                cost_usd: usage.and_then(|usage| self.llm.estimate_cost_usd(usage)),
                error_code: None,
            }),
        )
    }

    fn log_failed_answer(
        &self,
        author: &str,
        question: &str,
        profiles: &[serde_json::Value],
        chunks: &[crate::storage::SourceChunk],
        retrieval_trace: &serde_json::Value,
        raw_answer: &str,
        error: &str,
        started: Instant,
        max_tokens: i64,
        usage: Option<&LlmUsage>,
    ) -> Result<()> {
        let retrieval = json!({
            "profile_count": profiles.len(),
            "chunks": chunks.iter().map(|chunk| json!({
                "paper_key": chunk.paper_key,
                "title": chunk.title,
                "doi": chunk.doi,
                "year": chunk.year,
                "chunk_id": chunk.id,
                "chunk_index": chunk.chunk_index,
                "section": chunk.section,
                "section_kind": chunk.section_kind,
                "caption_label": chunk.caption_label,
                "source_hash": chunk.source_hash,
                "chunk_hash": stored_chunk_hash(chunk),
                "chunker_version": chunk.chunker_version,
            })).collect::<Vec<_>>(),
            "trace": retrieval_trace,
        });
        let answer_json = json!({
            "raw": raw_answer,
            "error": error,
            "answer_schema_version": QA_ANSWER_SCHEMA_VERSION,
            "evidence_snapshot": [],
        });
        self.storage.save_qa_log_with_metadata(
            author,
            question,
            &retrieval,
            &answer_json,
            self.llm.model_name(),
            started.elapsed().as_millis() as i64,
            Some(QaLogMetadata {
                answer_schema_version: Some(QA_ANSWER_SCHEMA_VERSION),
                qa_prompt_version: Some(QA_PROMPT_VERSION),
                temperature: Some(0.2),
                max_tokens: Some(max_tokens),
                prompt_tokens: usage.and_then(|usage| usage.prompt_tokens),
                completion_tokens: usage.and_then(|usage| usage.completion_tokens),
                total_tokens: usage.and_then(|usage| usage.total_tokens),
                cost_usd: usage.and_then(|usage| self.llm.estimate_cost_usd(usage)),
                error_code: Some("evidence_invalid"),
            }),
        )
    }

    fn valid_or_repaired_answer(
        &self,
        raw_answer: &str,
        chunks: &[crate::storage::SourceChunk],
    ) -> Result<String> {
        match verify_qa_answer(raw_answer, chunks) {
            Ok(_) => Ok(raw_answer.to_string()),
            Err(error) => {
                let repaired = self.llm.chat(
                    qa_repair_messages(raw_answer, &error.to_string(), chunks),
                    0.0,
                    1600,
                )?;
                verify_qa_answer(&repaired, chunks)?;
                Ok(repaired)
            }
        }
    }
}

fn should_retry_with_more_chunks(answer: &str, chunks: &[crate::storage::SourceChunk]) -> bool {
    (signals_insufficient(answer) || chunks.is_empty())
        && chunks.len() < retry_chunk_limit(qa_chunk_limit())
}

fn qa_chunk_limit() -> usize {
    parse_qa_chunk_limit(std::env::var(QA_CHUNK_LIMIT_ENV).ok())
}

fn parse_qa_chunk_limit(value: Option<String>) -> usize {
    value
        .and_then(|value| value.parse::<usize>().ok())
        .filter(|value| (1..=30).contains(value))
        .unwrap_or(DEFAULT_CHUNK_LIMIT)
}

fn retry_chunk_limit(chunk_limit: usize) -> usize {
    (chunk_limit + 2).max(RETRY_CHUNK_LIMIT)
}

fn append_unique_chunks(
    target: &mut Vec<crate::storage::SourceChunk>,
    candidates: Vec<crate::storage::SourceChunk>,
    limit: usize,
) {
    for chunk in candidates {
        if target.len() >= limit {
            break;
        }
        if !target.iter().any(|existing| existing.id == chunk.id) {
            target.push(chunk);
        }
    }
}

fn evidence_snapshot(
    answer_json: &serde_json::Value,
    chunks: &[crate::storage::SourceChunk],
) -> serde_json::Value {
    let Some(evidence) = answer_json
        .get("evidence")
        .and_then(serde_json::Value::as_array)
    else {
        return json!([]);
    };
    let items = evidence
        .iter()
        .filter_map(|item| {
            let chunk_id = item.get("chunk_id").and_then(serde_json::Value::as_i64)?;
            let chunk = chunks.iter().find(|chunk| chunk.id == chunk_id)?;
            Some(json!({
                "paper_key": chunk.paper_key,
                "chunk_id": chunk.id,
                "chunk_index": chunk.chunk_index,
                "source_hash": chunk.source_hash,
                "chunk_hash": stored_chunk_hash(chunk),
                "chunker_version": chunk.chunker_version,
                "title": chunk.title,
                "doi": chunk.doi,
                "year": chunk.year,
                "section": chunk.section,
                "section_kind": chunk.section_kind,
                "caption_label": chunk.caption_label,
                "text_excerpt": text_excerpt(&chunk.text, 500),
            }))
        })
        .collect::<Vec<_>>();
    json!(items)
}

fn chunk_hash(text: &str) -> String {
    let mut digest = Sha256::new();
    digest.update(text.as_bytes());
    format!("{:x}", digest.finalize())
}

fn stored_chunk_hash(chunk: &crate::storage::SourceChunk) -> String {
    if chunk.chunk_hash.trim().is_empty() {
        chunk_hash(&chunk.text)
    } else {
        chunk.chunk_hash.clone()
    }
}

fn text_excerpt(text: &str, max_chars: usize) -> String {
    text.chars().take(max_chars).collect()
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::{evidence_snapshot, parse_qa_chunk_limit};
    use crate::qa::renderer::render_qa_answer;
    use crate::qa::verifier::verify_qa_answer;
    use crate::storage::SourceChunk;

    #[test]
    fn renders_structured_qa_answer_with_evidence() {
        let raw = r#"{
            "answer": "这是答案。",
            "evidence": [{
                "paper_key": "Alice/paper-a",
                "title": "A Paper",
                "doi": "10.1/test",
                "year": "2024",
                "chunk_id": 7,
                "section": "Results",
                "quote_or_summary": "结果支持该结论"
            }],
            "uncertainty": "仅覆盖给定片段。",
            "followup_queries": ["继续问方法"]
        }"#;

        let rendered = render_qa_answer(raw);
        assert!(rendered.contains("这是答案。"));
        assert!(rendered.contains("[1] 2024 A Paper 10.1/test section=Results chunk=7"));
        assert!(rendered.contains("不确定性：仅覆盖给定片段。"));
        assert!(rendered.contains("- 继续问方法"));
    }

    #[test]
    fn keeps_unstructured_answer_as_is() {
        assert_eq!(render_qa_answer("plain answer"), "plain answer");
    }

    #[test]
    fn rejects_evidence_not_present_in_source_chunks() {
        let chunks = vec![SourceChunk {
            id: 7,
            paper_key: "Alice/paper-a".to_string(),
            chunk_index: 0,
            section: "Results".to_string(),
            text: "text".to_string(),
            title: "A Paper".to_string(),
            doi: "10.1/test".to_string(),
            year: "2024".to_string(),
            source_hash: "hash".to_string(),
            chunk_hash: "chunk-hash".to_string(),
            chunker_version: "section-char-v1".to_string(),
            section_kind: "body".to_string(),
            caption_label: None,
        }];
        let raw = r#"{
            "answer": "这是答案。",
            "evidence": [{
                "paper_key": "Alice/paper-a",
                "title": "A Paper",
                "doi": "10.1/test",
                "year": "2024",
                "chunk_id": 8,
                "section": "Results",
                "quote_or_summary": "结果支持该结论"
            }],
            "uncertainty": "",
            "followup_queries": []
        }"#;

        assert!(verify_qa_answer(raw, &chunks).is_err());
    }

    #[test]
    fn rejects_factual_answer_without_evidence() {
        let chunks = vec![SourceChunk {
            id: 7,
            paper_key: "Alice/paper-a".to_string(),
            chunk_index: 0,
            section: "Results".to_string(),
            text: "text".to_string(),
            title: "A Paper".to_string(),
            doi: "10.1/test".to_string(),
            year: "2024".to_string(),
            source_hash: "hash".to_string(),
            chunk_hash: "chunk-hash".to_string(),
            chunker_version: "section-char-v1".to_string(),
            section_kind: "body".to_string(),
            caption_label: None,
        }];
        let raw = r#"{
            "answer": "这是一个事实性结论。",
            "evidence": [],
            "uncertainty": "",
            "followup_queries": []
        }"#;

        assert!(verify_qa_answer(raw, &chunks).is_err());
    }

    #[test]
    fn accepts_insufficient_answer_without_evidence() {
        let raw = r#"{
            "answer": "证据不足，无法回答。",
            "evidence": [],
            "uncertainty": "没有检索到相关片段。",
            "followup_queries": []
        }"#;

        verify_qa_answer(raw, &[]).unwrap();
    }

    #[test]
    fn evidence_snapshot_captures_chunk_metadata_and_excerpt() {
        let chunks = vec![SourceChunk {
            id: 7,
            paper_key: "Alice/paper-a".to_string(),
            chunk_index: 3,
            section: "Results".to_string(),
            text: "The catalyst reached 82% conversion under mild conditions.".to_string(),
            title: "A Paper".to_string(),
            doi: "10.1/test".to_string(),
            year: "2024".to_string(),
            source_hash: "source-hash".to_string(),
            chunk_hash: "chunk-hash".to_string(),
            chunker_version: "section-char-v1".to_string(),
            section_kind: "body".to_string(),
            caption_label: None,
        }];
        let snapshot = evidence_snapshot(
            &json!({
                "evidence": [{
                    "paper_key": "Alice/paper-a",
                    "chunk_id": 7,
                    "quote_or_summary": "82% conversion"
                }]
            }),
            &chunks,
        );

        assert_eq!(snapshot[0]["source_hash"], "source-hash");
        assert_eq!(snapshot[0]["chunk_hash"], "chunk-hash");
        assert_eq!(snapshot[0]["chunker_version"], "section-char-v1");
        assert_eq!(snapshot[0]["chunk_index"], 3);
        assert_eq!(snapshot[0]["section_kind"], "body");
        assert!(snapshot[0]["caption_label"].is_null());
        assert!(
            snapshot[0]["text_excerpt"]
                .as_str()
                .unwrap()
                .contains("82% conversion")
        );
    }

    #[test]
    fn qa_chunk_limit_defaults_to_configurable_prompt_size() {
        assert_eq!(parse_qa_chunk_limit(None), 8);
        assert_eq!(parse_qa_chunk_limit(Some("12".to_string())), 12);
        assert_eq!(parse_qa_chunk_limit(Some("0".to_string())), 8);
        assert_eq!(parse_qa_chunk_limit(Some("31".to_string())), 8);
    }
}
