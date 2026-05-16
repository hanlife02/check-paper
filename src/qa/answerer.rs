use std::time::Instant;

use anyhow::Result;
use serde_json::{Map, Value, json};
use sha2::{Digest, Sha256};

use crate::qa::planner::should_use_source_chunks;
use crate::qa::renderer::render_qa_answer_for_question;
use crate::qa::verifier::{parse_qa_answer, verify_qa_answer};
use crate::retrieval::embedding::OpenAiCompatibleEmbeddingClient;
use crate::retrieval::profile_route::rank_profiles;
use crate::retrieval::query::query_terms;
use crate::schemas::author_profile::AUTHOR_PROFILE_SCHEMA_VERSION;
use crate::schemas::qa_answer::{QA_ANSWER_SCHEMA_VERSION, signals_insufficient};
use crate::storage::{QaLogEntry, QaLogMetadata, SourceChunk, Storage};
use crate::understanding::llm::{LlmUsage, OpenAiCompatibleClient};
use crate::understanding::prompts::{
    AUTHOR_PROFILE_PROMPT_VERSION, QA_PROMPT_VERSION, qa_messages, qa_repair_messages,
};

const PROFILE_CONTEXT_LIMIT: usize = 8;
const DEFAULT_CHUNK_LIMIT: usize = 8;
const RETRY_CHUNK_LIMIT: usize = 10;
const QA_CHUNK_LIMIT_ENV: &str = "CHECK_PAPER_QA_CHUNK_LIMIT";

struct AnswerLogContext<'a> {
    author: &'a str,
    question: &'a str,
    author_profile: Option<&'a serde_json::Value>,
    profiles: &'a [serde_json::Value],
    chunks: &'a [SourceChunk],
    retrieval_trace: &'a serde_json::Value,
    started: Instant,
    max_tokens: i64,
    usage: Option<&'a LlmUsage>,
}

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
        let (author_profile, profiles) = self.profile_context(author, question)?;
        let chunk_limit = qa_chunk_limit();
        let (mut chunks, mut retrieval_trace) =
            self.context_chunks(author, question, &profiles, chunk_limit)?;

        let first = self.llm.chat_with_usage(
            qa_messages(question, author_profile.as_ref(), &profiles, &chunks),
            0.2,
            2200,
        )?;
        if should_retry_with_more_chunks(&first.content, &chunks) {
            (chunks, retrieval_trace) =
                self.context_chunks(author, question, &profiles, retry_chunk_limit(chunk_limit))?;
            let second = self.llm.chat_with_usage(
                qa_messages(question, author_profile.as_ref(), &profiles, &chunks),
                0.2,
                2600,
            )?;
            let repaired = match self.valid_or_repaired_answer(&second.content, &chunks) {
                Ok(answer) => answer,
                Err(err) => {
                    self.log_failed_answer(
                        AnswerLogContext {
                            author,
                            question,
                            author_profile: author_profile.as_ref(),
                            profiles: &profiles,
                            chunks: &chunks,
                            retrieval_trace: &retrieval_trace,
                            started,
                            max_tokens: 2600,
                            usage: Some(&second.usage),
                        },
                        &second.content,
                        &err.to_string(),
                    )?;
                    return Err(err);
                }
            };
            self.log_answer(
                AnswerLogContext {
                    author,
                    question,
                    author_profile: author_profile.as_ref(),
                    profiles: &profiles,
                    chunks: &chunks,
                    retrieval_trace: &retrieval_trace,
                    started,
                    max_tokens: 2600,
                    usage: Some(&second.usage),
                },
                &repaired,
            )?;
            return Ok(render_qa_answer_for_question(&repaired, question));
        }
        let repaired = match self.valid_or_repaired_answer(&first.content, &chunks) {
            Ok(answer) => answer,
            Err(err) => {
                self.log_failed_answer(
                    AnswerLogContext {
                        author,
                        question,
                        author_profile: author_profile.as_ref(),
                        profiles: &profiles,
                        chunks: &chunks,
                        retrieval_trace: &retrieval_trace,
                        started,
                        max_tokens: 2200,
                        usage: Some(&first.usage),
                    },
                    &first.content,
                    &err.to_string(),
                )?;
                return Err(err);
            }
        };
        self.log_answer(
            AnswerLogContext {
                author,
                question,
                author_profile: author_profile.as_ref(),
                profiles: &profiles,
                chunks: &chunks,
                retrieval_trace: &retrieval_trace,
                started,
                max_tokens: 2200,
                usage: Some(&first.usage),
            },
            &repaired,
        )?;
        Ok(render_qa_answer_for_question(&repaired, question))
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
        let (author_profile, profiles) = self.profile_context(author, question)?;
        let chunk_limit = qa_chunk_limit();
        let (mut chunks, mut retrieval_trace) =
            self.context_chunks(author, question, &profiles, chunk_limit)?;

        let first = self
            .llm
            .chat_stream(
                qa_messages(question, author_profile.as_ref(), &profiles, &chunks),
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
                    qa_messages(question, author_profile.as_ref(), &profiles, &chunks),
                    0.2,
                    2600,
                    |delta| on_delta(delta),
                )
                .await?;
            let repaired = match self.valid_or_repaired_answer(&second, &chunks) {
                Ok(answer) => answer,
                Err(err) => {
                    self.log_failed_answer(
                        AnswerLogContext {
                            author,
                            question,
                            author_profile: author_profile.as_ref(),
                            profiles: &profiles,
                            chunks: &chunks,
                            retrieval_trace: &retrieval_trace,
                            started,
                            max_tokens: 2600,
                            usage: None,
                        },
                        &second,
                        &err.to_string(),
                    )?;
                    return Err(err);
                }
            };
            self.log_answer(
                AnswerLogContext {
                    author,
                    question,
                    author_profile: author_profile.as_ref(),
                    profiles: &profiles,
                    chunks: &chunks,
                    retrieval_trace: &retrieval_trace,
                    started,
                    max_tokens: 2600,
                    usage: None,
                },
                &repaired,
            )?;
            return Ok(render_qa_answer_for_question(&repaired, question));
        }
        let repaired = match self.valid_or_repaired_answer(&first, &chunks) {
            Ok(answer) => answer,
            Err(err) => {
                self.log_failed_answer(
                    AnswerLogContext {
                        author,
                        question,
                        author_profile: author_profile.as_ref(),
                        profiles: &profiles,
                        chunks: &chunks,
                        retrieval_trace: &retrieval_trace,
                        started,
                        max_tokens: 2200,
                        usage: None,
                    },
                    &first,
                    &err.to_string(),
                )?;
                return Err(err);
            }
        };
        self.log_answer(
            AnswerLogContext {
                author,
                question,
                author_profile: author_profile.as_ref(),
                profiles: &profiles,
                chunks: &chunks,
                retrieval_trace: &retrieval_trace,
                started,
                max_tokens: 2200,
                usage: None,
            },
            &repaired,
        )?;
        Ok(render_qa_answer_for_question(&repaired, question))
    }

    fn profile_context(
        &self,
        author: &str,
        question: &str,
    ) -> Result<(Option<serde_json::Value>, Vec<serde_json::Value>)> {
        let all_profiles = self.storage.paper_profiles(author, None)?;
        let author_profile = if all_profiles.is_empty() {
            None
        } else {
            let source_profile_hash = profile_source_hash(&all_profiles)?;
            if self.storage.author_profile_is_current(
                author,
                AUTHOR_PROFILE_SCHEMA_VERSION,
                AUTHOR_PROFILE_PROMPT_VERSION,
                self.llm.model_name(),
                &source_profile_hash,
            )? {
                self.storage.get_author_profile(author)?
            } else {
                None
            }
        };
        let terms = query_terms(question);
        Ok((
            author_profile,
            rank_profiles(all_profiles, &terms, PROFILE_CONTEXT_LIMIT),
        ))
    }

    fn context_chunks(
        &self,
        author: &str,
        question: &str,
        profiles: &[serde_json::Value],
        limit: usize,
    ) -> Result<(Vec<crate::storage::SourceChunk>, serde_json::Value)> {
        if !should_use_source_chunks(question, profiles.len()) {
            let chunks = self.profile_grounding_chunks(profiles, limit)?;
            if !chunks.is_empty() {
                let trace = json!({
                    "routes": {
                        "profile_grounding": chunks.iter().enumerate().map(|(rank, chunk)| json!({
                            "rank": rank + 1,
                            "chunk_id": chunk.id,
                            "paper_key": chunk.paper_key,
                            "chunk_index": chunk.chunk_index,
                            "section": chunk.section,
                            "section_kind": chunk.section_kind,
                            "caption_label": chunk.caption_label,
                        })).collect::<Vec<_>>()
                    },
                    "fusion": []
                });
                return Ok((chunks, trace));
            }
        }
        let (mut chunks, mut trace) = self.search_chunks(author, question, limit)?;
        if chunks.len() < limit {
            let remaining = limit - chunks.len();
            let profile_chunks = self.profile_grounding_chunks(profiles, remaining)?;
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

    fn profile_grounding_chunks(
        &self,
        profiles: &[serde_json::Value],
        limit: usize,
    ) -> Result<Vec<crate::storage::SourceChunk>> {
        let paper_keys = profiles
            .iter()
            .filter_map(|profile| profile.get("paper_key").and_then(serde_json::Value::as_str))
            .map(str::to_string)
            .collect::<Vec<_>>();
        self.storage.chunks_for_paper_keys(&paper_keys, limit)
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

    fn log_answer(&self, context: AnswerLogContext<'_>, raw_answer: &str) -> Result<()> {
        let retrieval = json!({
            "author_profile_present": context.author_profile.is_some(),
            "profile_count": context.profiles.len(),
            "chunks": context.chunks.iter().map(|chunk| json!({
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
            "trace_summary": retrieval_trace_summary(context.retrieval_trace),
            "trace": context.retrieval_trace,
        });
        let mut answer_json = parse_qa_answer(raw_answer)
            .map(|answer| {
                serde_json::to_value(answer).unwrap_or_else(|_| json!({ "raw": raw_answer }))
            })
            .unwrap_or_else(|| json!({ "raw": raw_answer }));
        let snapshot = evidence_snapshot(&answer_json, context.chunks);
        if let Some(object) = answer_json.as_object_mut() {
            object.insert(
                "answer_schema_version".to_string(),
                QA_ANSWER_SCHEMA_VERSION.into(),
            );
            object.insert("evidence_snapshot".to_string(), snapshot);
        }
        self.storage.save_qa_log_with_metadata(QaLogEntry {
            author: context.author,
            question: context.question,
            retrieval: &retrieval,
            answer: &answer_json,
            model: self.llm.model_name(),
            latency_ms: context.started.elapsed().as_millis() as i64,
            metadata: Some(QaLogMetadata {
                answer_schema_version: Some(QA_ANSWER_SCHEMA_VERSION),
                qa_prompt_version: Some(QA_PROMPT_VERSION),
                temperature: Some(0.2),
                max_tokens: Some(context.max_tokens),
                prompt_tokens: context.usage.and_then(|usage| usage.prompt_tokens),
                completion_tokens: context.usage.and_then(|usage| usage.completion_tokens),
                total_tokens: context.usage.and_then(|usage| usage.total_tokens),
                cost_usd: context
                    .usage
                    .and_then(|usage| self.llm.estimate_cost_usd(usage)),
                error_code: None,
            }),
        })
    }

    fn log_failed_answer(
        &self,
        context: AnswerLogContext<'_>,
        raw_answer: &str,
        error: &str,
    ) -> Result<()> {
        let retrieval = json!({
            "author_profile_present": context.author_profile.is_some(),
            "profile_count": context.profiles.len(),
            "chunks": context.chunks.iter().map(|chunk| json!({
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
            "trace_summary": retrieval_trace_summary(context.retrieval_trace),
            "trace": context.retrieval_trace,
        });
        let answer_json = json!({
            "raw": raw_answer,
            "error": error,
            "answer_schema_version": QA_ANSWER_SCHEMA_VERSION,
            "evidence_snapshot": [],
        });
        self.storage.save_qa_log_with_metadata(QaLogEntry {
            author: context.author,
            question: context.question,
            retrieval: &retrieval,
            answer: &answer_json,
            model: self.llm.model_name(),
            latency_ms: context.started.elapsed().as_millis() as i64,
            metadata: Some(QaLogMetadata {
                answer_schema_version: Some(QA_ANSWER_SCHEMA_VERSION),
                qa_prompt_version: Some(QA_PROMPT_VERSION),
                temperature: Some(0.2),
                max_tokens: Some(context.max_tokens),
                prompt_tokens: context.usage.and_then(|usage| usage.prompt_tokens),
                completion_tokens: context.usage.and_then(|usage| usage.completion_tokens),
                total_tokens: context.usage.and_then(|usage| usage.total_tokens),
                cost_usd: context
                    .usage
                    .and_then(|usage| self.llm.estimate_cost_usd(usage)),
                error_code: Some("evidence_invalid"),
            }),
        })
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

fn evidence_snapshot(answer_json: &Value, chunks: &[crate::storage::SourceChunk]) -> Value {
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

fn retrieval_trace_summary(trace: &Value) -> Value {
    let mut routes = Map::new();
    if let Some(route_map) = trace.get("routes").and_then(Value::as_object) {
        for (route, candidates) in route_map {
            let candidate_items = candidates.as_array();
            let top = candidate_items.and_then(|items| items.first());
            routes.insert(
                route.clone(),
                json!({
                    "candidate_count": candidate_items.map_or(0, Vec::len),
                    "top_paper_key": top
                        .and_then(|item| item.get("paper_key"))
                        .and_then(Value::as_str),
                    "top_chunk_id": top
                        .and_then(|item| item.get("chunk_id"))
                        .and_then(Value::as_i64),
                    "top_score": top
                        .and_then(|item| item.get("score"))
                        .and_then(Value::as_f64),
                }),
            );
        }
    }
    let fusion_items = trace
        .get("fusion")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .take(8)
        .map(|item| {
            json!({
                "rank": item.get("rank").and_then(Value::as_i64),
                "paper_key": item.get("paper_key").and_then(Value::as_str),
                "chunk_id": item.get("chunk_id").and_then(Value::as_i64),
                "score": item.get("score").and_then(Value::as_f64),
            })
        })
        .collect::<Vec<_>>();
    json!({
        "routes": routes,
        "fusion_count": trace
            .get("fusion")
            .and_then(Value::as_array)
            .map_or(0, Vec::len),
        "fusion": fusion_items,
    })
}

fn chunk_hash(text: &str) -> String {
    let mut digest = Sha256::new();
    digest.update(text.as_bytes());
    format!("{:x}", digest.finalize())
}

fn profile_source_hash(profiles: &[serde_json::Value]) -> Result<String> {
    Ok(chunk_hash(&serde_json::to_string(profiles)?))
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
    use std::io::{Read, Write};
    use std::net::{TcpListener, TcpStream};
    use std::thread::{self, JoinHandle};
    use std::time::Duration;

    use serde_json::json;
    use tempfile::tempdir;

    use super::{
        Answerer, evidence_snapshot, parse_qa_chunk_limit, profile_source_hash,
        retrieval_trace_summary,
    };
    use crate::papers::models::Paper;
    use crate::qa::renderer::render_qa_answer;
    use crate::qa::verifier::verify_qa_answer;
    use crate::retrieval::chunker::chunk_paper;
    use crate::schemas::author_profile::AUTHOR_PROFILE_SCHEMA_VERSION;
    use crate::schemas::paper_profile::PAPER_PROFILE_SCHEMA_VERSION;
    use crate::storage::{PaperProfileMetadata, SourceChunk, Storage};
    use crate::understanding::llm::{LlmConfig, OpenAiCompatibleClient};
    use crate::understanding::prompts::{
        AUTHOR_PROFILE_PROMPT_VERSION, PAPER_PROFILE_PROMPT_VERSION,
    };

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
    fn retrieval_trace_summary_keeps_route_counts_and_fusion_scores() {
        let summary = retrieval_trace_summary(&json!({
            "routes": {
                "fts": [
                    {
                        "rank": 1,
                        "score": 0.016,
                        "chunk_id": 7,
                        "paper_key": "Alice/paper-a"
                    }
                ],
                "like": []
            },
            "fusion": [
                {
                    "rank": 1,
                    "score": 0.032,
                    "chunk_id": 7,
                    "paper_key": "Alice/paper-a"
                }
            ]
        }));

        assert_eq!(summary["routes"]["fts"]["candidate_count"], 1);
        assert_eq!(summary["routes"]["fts"]["top_paper_key"], "Alice/paper-a");
        assert_eq!(summary["routes"]["like"]["candidate_count"], 0);
        assert_eq!(summary["fusion_count"], 1);
        assert_eq!(summary["fusion"][0]["chunk_id"], 7);
        assert!(summary["fusion"][0]["score"].as_f64().unwrap() > 0.0);
    }

    #[test]
    fn qa_chunk_limit_defaults_to_configurable_prompt_size() {
        assert_eq!(parse_qa_chunk_limit(None), 8);
        assert_eq!(parse_qa_chunk_limit(Some("12".to_string())), 12);
        assert_eq!(parse_qa_chunk_limit(Some("0".to_string())), 8);
        assert_eq!(parse_qa_chunk_limit(Some("31".to_string())), 8);
    }

    #[test]
    fn profile_context_uses_only_current_author_profile() {
        let dir = tempdir().unwrap();
        let mut storage = Storage::open(&dir.path().join("test.sqlite")).unwrap();
        let paper = Paper {
            author: "Alice".to_string(),
            paper_id: "paper-a".to_string(),
            paper_dir: dir.path().to_path_buf(),
            article_path: dir.path().join("article.md"),
            fetch_result_path: None,
            source_hash: "source-a".to_string(),
            metadata: std::collections::BTreeMap::from([
                ("title".to_string(), "MOF Paper".to_string()),
                ("doi".to_string(), "10.1/test".to_string()),
                ("year".to_string(), "2024".to_string()),
            ]),
            fetch_result: json!({}),
            raw_body: String::new(),
            clean_text: "MOF catalysis result.".to_string(),
            sections: Vec::new(),
        };
        let chunks = chunk_paper(&paper, 3200, 350);
        storage.upsert_paper(&paper, &chunks).unwrap();
        storage
            .save_paper_profile_with_metadata(
                &paper.key(),
                &json!({
                    "paper_key": paper.key(),
                    "title": "MOF Paper",
                    "doi": "10.1/test",
                    "year": "2024",
                    "one_sentence_summary": "MOF catalysis.",
                    "methods": [{"method": "tested MOFs", "evidence_chunks": [0]}],
                    "topic_keywords": ["MOF"]
                }),
                PaperProfileMetadata {
                    source_hash: &paper.source_hash,
                    schema_version: PAPER_PROFILE_SCHEMA_VERSION,
                    prompt_version: PAPER_PROFILE_PROMPT_VERSION,
                    model_id: "test-model",
                    chunker_version: "section-char-v1",
                },
            )
            .unwrap();
        let profiles = storage.paper_profiles("Alice", None).unwrap();
        let source_profile_hash = profile_source_hash(&profiles).unwrap();
        storage
            .save_author_profile_with_metadata(
                "Alice",
                &json!({"author": "Alice", "answer_scope": ["MOF catalysis"]}),
                AUTHOR_PROFILE_SCHEMA_VERSION,
                AUTHOR_PROFILE_PROMPT_VERSION,
                "test-model",
                &source_profile_hash,
            )
            .unwrap();
        let answerer = Answerer::new(&storage, test_llm("test-model"));

        let (author_profile, ranked_profiles) = answerer.profile_context("Alice", "MOF").unwrap();

        assert!(author_profile.is_some());
        assert_eq!(ranked_profiles.len(), 1);

        storage
            .save_author_profile_with_metadata(
                "Alice",
                &json!({"author": "Alice", "answer_scope": ["stale"]}),
                AUTHOR_PROFILE_SCHEMA_VERSION,
                AUTHOR_PROFILE_PROMPT_VERSION,
                "old-model",
                &source_profile_hash,
            )
            .unwrap();
        let (author_profile, _) = answerer.profile_context("Alice", "MOF").unwrap();

        assert!(author_profile.is_none());
    }

    #[test]
    fn broad_profile_question_grounds_in_article_body_not_metadata() {
        let dir = tempdir().unwrap();
        let mut storage = Storage::open(&dir.path().join("test.sqlite")).unwrap();
        let paper = Paper {
            author: "Alice".to_string(),
            paper_id: "paper-a".to_string(),
            paper_dir: dir.path().to_path_buf(),
            article_path: dir.path().join("article.md"),
            fetch_result_path: None,
            source_hash: "source-a".to_string(),
            metadata: std::collections::BTreeMap::from([
                ("title".to_string(), "MOF Paper".to_string()),
                ("doi".to_string(), "10.1/test".to_string()),
                ("year".to_string(), "2024".to_string()),
            ]),
            fetch_result: json!({}),
            raw_body: String::new(),
            clean_text: String::new(),
            sections: vec![
                crate::papers::models::Section {
                    title: "MOF Paper".to_string(),
                    level: 1,
                    content: "- DOI: `10.1/test`\n- Year: `2024`".to_string(),
                },
                crate::papers::models::Section {
                    title: "Article Text".to_string(),
                    level: 2,
                    content: "Abstract\nDeveloping scalable methods for MOF catalysis is the central topic."
                        .to_string(),
                },
            ],
        };
        let chunks = chunk_paper(&paper, 3200, 350);
        storage.upsert_paper(&paper, &chunks).unwrap();
        storage
            .save_paper_profile_with_metadata(
                &paper.key(),
                &json!({
                    "paper_key": paper.key(),
                    "title": "MOF Paper",
                    "doi": "10.1/test",
                    "year": "2024",
                    "one_sentence_summary": "This paper studies MOF catalysis.",
                    "methods": [{"method": "scalable methods", "evidence_chunks": [1]}],
                    "topic_keywords": ["MOF catalysis"]
                }),
                PaperProfileMetadata {
                    source_hash: &paper.source_hash,
                    schema_version: PAPER_PROFILE_SCHEMA_VERSION,
                    prompt_version: PAPER_PROFILE_PROMPT_VERSION,
                    model_id: "test-model",
                    chunker_version: "section-char-v1",
                },
            )
            .unwrap();
        let profiles = storage.paper_profiles("Alice", None).unwrap();
        let answerer = Answerer::new(&storage, test_llm("test-model"));

        let (chunks, trace) = answerer
            .context_chunks(
                "Alice",
                "请用一句话概括目前已分析论文的主要方向",
                &profiles,
                8,
            )
            .unwrap();

        assert_eq!(chunks.len(), 1);
        assert_eq!(chunks[0].chunk_index, 1);
        assert_eq!(chunks[0].section, "Article Text");
        assert!(chunks[0].text.contains("Developing scalable methods"));
        assert!(!chunks[0].text.contains("DOI:"));
        assert_eq!(trace["routes"]["profile_grounding"][0]["chunk_index"], 1);
    }

    #[test]
    fn answer_flow_uses_current_author_profile_and_logs_grounded_sources() {
        let dir = tempdir().unwrap();
        let mut storage = Storage::open(&dir.path().join("test.sqlite")).unwrap();
        let paper = Paper {
            author: "Alice".to_string(),
            paper_id: "paper-a".to_string(),
            paper_dir: dir.path().to_path_buf(),
            article_path: dir.path().join("article.md"),
            fetch_result_path: None,
            source_hash: "source-a".to_string(),
            metadata: std::collections::BTreeMap::from([
                ("title".to_string(), "MOF Paper".to_string()),
                ("doi".to_string(), "10.1/test".to_string()),
                ("year".to_string(), "2024".to_string()),
            ]),
            fetch_result: json!({}),
            raw_body: String::new(),
            clean_text: String::new(),
            sections: vec![crate::papers::models::Section {
                title: "Abstract".to_string(),
                level: 1,
                content: "This paper studies MOF catalysis with solvent screening.".to_string(),
            }],
        };
        let chunks = chunk_paper(&paper, 3200, 350);
        storage.upsert_paper(&paper, &chunks).unwrap();
        let source_chunk = storage.all_chunks_for_author("Alice", Some(1)).unwrap()[0].clone();
        storage
            .save_paper_profile_with_metadata(
                &paper.key(),
                &json!({
                    "paper_key": paper.key(),
                    "title": "MOF Paper",
                    "doi": "10.1/test",
                    "year": "2024",
                    "one_sentence_summary": "This paper studies MOF catalysis.",
                    "methods": [{"method": "solvent screening", "evidence_chunks": [0]}],
                    "topic_keywords": ["MOF", "catalysis"]
                }),
                PaperProfileMetadata {
                    source_hash: &paper.source_hash,
                    schema_version: PAPER_PROFILE_SCHEMA_VERSION,
                    prompt_version: PAPER_PROFILE_PROMPT_VERSION,
                    model_id: "test-model",
                    chunker_version: "section-char-v1",
                },
            )
            .unwrap();
        let profiles = storage.paper_profiles("Alice", None).unwrap();
        let source_profile_hash = profile_source_hash(&profiles).unwrap();
        storage
            .save_author_profile_with_metadata(
                "Alice",
                &json!({"author": "Alice", "answer_scope": ["MOF catalysis"]}),
                AUTHOR_PROFILE_SCHEMA_VERSION,
                AUTHOR_PROFILE_PROMPT_VERSION,
                "test-model",
                &source_profile_hash,
            )
            .unwrap();
        let raw_answer = json!({
            "answer": "这篇论文研究 MOF catalysis。",
            "claims": [{
                "claim": "这篇论文研究 MOF catalysis。",
                "evidence_indices": [0],
                "support": "strong"
            }],
            "evidence": [{
                "paper_key": "Alice/paper-a",
                "title": "MOF Paper",
                "doi": "10.1/test",
                "year": "2024",
                "chunk_id": source_chunk.id,
                "section": "Abstract",
                "quote_or_summary": "MOF catalysis"
            }],
            "uncertainty": "",
            "followup_queries": []
        })
        .to_string();
        let (base_url, request_rx, handle) = start_chat_completion_server(raw_answer);
        let answerer = Answerer::new(&storage, test_llm_at("test-model", &base_url));

        let rendered = answerer
            .answer("Alice", "What does the MOF catalysis paper study?")
            .unwrap();
        let request = request_rx
            .recv_timeout(Duration::from_secs(2))
            .expect("mock LLM should receive one request");

        assert!(request.contains("author_profile"));
        assert!(request.contains("source_chunks"));
        assert!(request.contains("MOF catalysis"));
        assert!(rendered.contains("这篇论文研究 MOF catalysis。"));
        let logged = storage.latest_qa_answer(Some("Alice")).unwrap().unwrap();
        assert_eq!(logged["evidence_snapshot"][0]["chunk_id"], source_chunk.id);
        assert_eq!(logged["evidence_snapshot"][0]["section"], "Abstract");
        handle.join().unwrap();
    }

    fn test_llm(model: &str) -> OpenAiCompatibleClient {
        test_llm_at(model, "http://127.0.0.1:9/v1")
    }

    fn test_llm_at(model: &str, base_url: &str) -> OpenAiCompatibleClient {
        OpenAiCompatibleClient::new(LlmConfig {
            base_url: base_url.to_string(),
            api_key: Some("test-key".to_string()),
            model: model.to_string(),
            proxy: None,
            timeout_secs: 1,
            tls_backend: "rustls".to_string(),
            prompt_cost_per_1k: None,
            completion_cost_per_1k: None,
        })
        .unwrap()
    }

    fn start_chat_completion_server(
        answer: String,
    ) -> (String, std::sync::mpsc::Receiver<String>, JoinHandle<()>) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let (tx, rx) = std::sync::mpsc::channel();
        let handle = thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let request = read_http_request(&mut stream);
            tx.send(request).unwrap();
            let body = json!({
                "choices": [{"message": {"content": answer}}],
                "usage": {"prompt_tokens": 11, "completion_tokens": 7, "total_tokens": 18}
            })
            .to_string();
            let response = format!(
                "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{}",
                body.len(),
                body
            );
            stream.write_all(response.as_bytes()).unwrap();
        });
        (format!("http://{address}/v1"), rx, handle)
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
