use std::time::Instant;

use anyhow::{Result, anyhow};
use serde_json::{Map, Value, json};
use sha2::{Digest, Sha256};

use crate::qa::planner::{plan_qa_route, should_use_source_chunks};
use crate::qa::renderer::render_qa_answer_for_question;
use crate::qa::verifier::{parse_qa_answer, verify_qa_answer};
use crate::retrieval::embedding::OpenAiCompatibleEmbeddingClient;
use crate::retrieval::hybrid;
use crate::retrieval::profile_route::{
    profile_grounding_chunks as retrieval_profile_grounding_chunks,
    profile_grounding_chunks_matching_terms as retrieval_profile_grounding_chunks_matching_terms,
    rank_profiles,
};
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
const SOURCE_PROFILE_GROUNDING_RESERVE: usize = 2;
const QA_DELIVERY_BLOCKING: &str = "blocking";
const QA_DELIVERY_STREAMING: &str = "streaming";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum QaProfileVersion {
    V1,
    V2,
}

impl QaProfileVersion {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::V1 => "v1",
            Self::V2 => "v2",
        }
    }
}

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
    delivery_mode: &'static str,
    streaming_finalized: Option<bool>,
    stream_stats: Option<AnswerStreamLogStats>,
    telegram_context: Option<TelegramLogContext>,
}

#[derive(Debug, Clone, Copy, Default)]
struct AnswerStreamLogStats {
    delta_count: i64,
    streamed_chars: i64,
    first_delta_ms: Option<i64>,
    duration_ms: i64,
}

#[derive(Debug, Clone, Copy)]
struct TelegramLogContext {
    chat_id: i64,
    job_id: i64,
}

pub struct Answerer<'a> {
    storage: &'a Storage,
    llm: OpenAiCompatibleClient,
    embedding: Option<OpenAiCompatibleEmbeddingClient>,
    profile_version: QaProfileVersion,
}

impl<'a> Answerer<'a> {
    pub fn new(storage: &'a Storage, llm: OpenAiCompatibleClient) -> Self {
        Self {
            storage,
            llm,
            embedding: None,
            profile_version: QaProfileVersion::V1,
        }
    }

    pub fn new_with_profile_version(
        storage: &'a Storage,
        llm: OpenAiCompatibleClient,
        profile_version: QaProfileVersion,
    ) -> Self {
        Self {
            storage,
            llm,
            embedding: None,
            profile_version,
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
            profile_version: QaProfileVersion::V1,
        }
    }

    pub fn new_with_embedding_and_profile_version(
        storage: &'a Storage,
        llm: OpenAiCompatibleClient,
        embedding: Option<OpenAiCompatibleEmbeddingClient>,
        profile_version: QaProfileVersion,
    ) -> Self {
        Self {
            storage,
            llm,
            embedding,
            profile_version,
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
            let repaired = match self.valid_or_repaired_answer(question, &second.content, &chunks) {
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
                            delivery_mode: QA_DELIVERY_BLOCKING,
                            streaming_finalized: None,
                            stream_stats: None,
                            telegram_context: None,
                        },
                        &second.content,
                        &err.to_string(),
                        "evidence_invalid",
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
                    delivery_mode: QA_DELIVERY_BLOCKING,
                    streaming_finalized: None,
                    stream_stats: None,
                    telegram_context: None,
                },
                &repaired,
            )?;
            return Ok(render_qa_answer_for_question(&repaired, question));
        }
        let repaired = match self.valid_or_repaired_answer(question, &first.content, &chunks) {
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
                        delivery_mode: QA_DELIVERY_BLOCKING,
                        streaming_finalized: None,
                        stream_stats: None,
                        telegram_context: None,
                    },
                    &first.content,
                    &err.to_string(),
                    "evidence_invalid",
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
                delivery_mode: QA_DELIVERY_BLOCKING,
                streaming_finalized: None,
                stream_stats: None,
                telegram_context: None,
            },
            &repaired,
        )?;
        Ok(render_qa_answer_for_question(&repaired, question))
    }

    pub async fn answer_stream<F>(
        &self,
        author: &str,
        question: &str,
        on_delta: F,
    ) -> Result<String>
    where
        F: FnMut(&str) -> Result<()>,
    {
        self.answer_stream_inner(author, question, None, on_delta)
            .await
    }

    pub async fn answer_stream_with_telegram_context<F>(
        &self,
        author: &str,
        question: &str,
        telegram_chat_id: i64,
        telegram_job_id: i64,
        on_delta: F,
    ) -> Result<String>
    where
        F: FnMut(&str) -> Result<()>,
    {
        self.answer_stream_inner(
            author,
            question,
            Some(TelegramLogContext {
                chat_id: telegram_chat_id,
                job_id: telegram_job_id,
            }),
            on_delta,
        )
        .await
    }

    async fn answer_stream_inner<F>(
        &self,
        author: &str,
        question: &str,
        telegram_context: Option<TelegramLogContext>,
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
        let mut stream_stats = AnswerStreamLogStats::default();

        let stream_started = Instant::now();
        let first_result = self
            .llm
            .chat_stream(
                qa_messages(question, author_profile.as_ref(), &profiles, &chunks),
                0.2,
                2200,
                |delta| {
                    record_stream_delta(&mut stream_stats, &started, delta);
                    on_delta(delta)
                },
            )
            .await;
        stream_stats.duration_ms += stream_started.elapsed().as_millis() as i64;
        let first = match first_result {
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
                        delivery_mode: QA_DELIVERY_STREAMING,
                        streaming_finalized: Some(false),
                        stream_stats: Some(stream_stats),
                        telegram_context,
                    },
                    "",
                    &err.to_string(),
                    "stream_failed",
                )?;
                return Err(err);
            }
        };
        if should_retry_with_more_chunks(&first, &chunks) {
            (chunks, retrieval_trace) =
                self.context_chunks(author, question, &profiles, retry_chunk_limit(chunk_limit))?;
            let stream_started = Instant::now();
            let second_result = self
                .llm
                .chat_stream(
                    qa_messages(question, author_profile.as_ref(), &profiles, &chunks),
                    0.2,
                    2600,
                    |delta| {
                        record_stream_delta(&mut stream_stats, &started, delta);
                        on_delta(delta)
                    },
                )
                .await;
            stream_stats.duration_ms += stream_started.elapsed().as_millis() as i64;
            let second = match second_result {
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
                            delivery_mode: QA_DELIVERY_STREAMING,
                            streaming_finalized: Some(false),
                            stream_stats: Some(stream_stats),
                            telegram_context,
                        },
                        "",
                        &err.to_string(),
                        "stream_failed",
                    )?;
                    return Err(err);
                }
            };
            let repaired = match self.valid_or_repaired_answer(question, &second, &chunks) {
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
                            delivery_mode: QA_DELIVERY_STREAMING,
                            streaming_finalized: Some(false),
                            stream_stats: Some(stream_stats),
                            telegram_context,
                        },
                        &second,
                        &err.to_string(),
                        "evidence_invalid",
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
                    delivery_mode: QA_DELIVERY_STREAMING,
                    streaming_finalized: Some(true),
                    stream_stats: Some(stream_stats),
                    telegram_context,
                },
                &repaired,
            )?;
            return Ok(render_qa_answer_for_question(&repaired, question));
        }
        let repaired = match self.valid_or_repaired_answer(question, &first, &chunks) {
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
                        delivery_mode: QA_DELIVERY_STREAMING,
                        streaming_finalized: Some(false),
                        stream_stats: Some(stream_stats),
                        telegram_context,
                    },
                    &first,
                    &err.to_string(),
                    "evidence_invalid",
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
                delivery_mode: QA_DELIVERY_STREAMING,
                streaming_finalized: Some(true),
                stream_stats: Some(stream_stats),
                telegram_context,
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
        match self.profile_version {
            QaProfileVersion::V1 => self.profile_context_v1(author, question),
            QaProfileVersion::V2 => self.profile_context_v2(author, question),
        }
    }

    fn profile_context_v1(
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
        let profiles = if should_use_source_chunks(question, all_profiles.len()) {
            rank_profiles(all_profiles, &terms, PROFILE_CONTEXT_LIMIT)
        } else {
            all_profiles
                .into_iter()
                .take(PROFILE_CONTEXT_LIMIT)
                .collect()
        };
        Ok((author_profile, profiles))
    }

    fn profile_context_v2(
        &self,
        author: &str,
        question: &str,
    ) -> Result<(Option<serde_json::Value>, Vec<serde_json::Value>)> {
        let records = self.storage.paper_profiles_v2_for_author(author, None)?;
        if records.is_empty() {
            return Err(anyhow!(
                "no V2 paper profiles for {author}; run `ppc comprehend --author {} --v2` first",
                quote_profile_author(author)
            ));
        }
        let all_profiles = records
            .into_iter()
            .map(|record| record.profile_json)
            .collect::<Vec<_>>();
        let author_profile = self
            .storage
            .author_profile_v2(author)?
            .map(|record| record.profile_json);
        let terms = query_terms(question);
        let profiles = if should_use_source_chunks(question, all_profiles.len()) {
            rank_profiles(all_profiles, &terms, PROFILE_CONTEXT_LIMIT)
        } else {
            all_profiles
                .into_iter()
                .take(PROFILE_CONTEXT_LIMIT)
                .collect()
        };
        Ok((author_profile, profiles))
    }

    fn context_chunks(
        &self,
        author: &str,
        question: &str,
        profiles: &[serde_json::Value],
        limit: usize,
    ) -> Result<(Vec<crate::storage::SourceChunk>, serde_json::Value)> {
        let plan = plan_qa_route(question, profiles.len());
        if !plan.use_source_chunks {
            let chunks = self.profile_grounding_chunks(profiles, limit)?;
            if !chunks.is_empty() {
                let trace = json!({
                    "qa_profile_version": self.profile_version.as_str(),
                    "qa_mode": plan.qa_mode,
                    "route_reason": plan.route_reason,
                    "routes": {
                        "profile_grounding": chunks.iter().enumerate().map(|(rank, chunk)| json!({
                            "rank": rank + 1,
                            "chunk_id": chunk.id,
                            "paper_key": chunk.paper_key,
                            "chunk_index": chunk.chunk_index,
                            "section": chunk.section,
                            "section_kind": chunk.section_kind,
                            "caption_label": chunk.caption_label,
                            "caption_object_type": chunk.caption_object_type,
                            "caption_object_label": chunk.caption_object_label,
                            "caption_panel_labels": chunk.caption_panel_labels_value(),
                            "caption_target_labels": chunk.caption_target_labels_value(),
                            "caption_panel_details": chunk.caption_panel_details_value(),
                            "caption_measurements": chunk.caption_measurements_value(),
                            "caption_conditions": chunk.caption_conditions_value(),
                            "caption_values": chunk.caption_values_value(),
                        })).collect::<Vec<_>>()
                    },
                    "fusion": []
                });
                return Ok((chunks, trace));
            }
        }
        let mut qa_mode = if plan.use_source_chunks {
            plan.qa_mode
        } else {
            "source_evidence"
        };
        let mut route_reason = if plan.use_source_chunks {
            plan.route_reason
        } else {
            "profile_grounding_empty"
        };
        let (mut chunks, mut trace) = self.search_chunks(author, question, limit)?;
        let terms = query_terms(question);
        let excluded_chunk_ids = chunks.iter().map(|chunk| chunk.id).collect::<Vec<_>>();
        let profile_chunks = self.profile_grounding_chunks_matching_terms(
            profiles,
            &terms,
            &excluded_chunk_ids,
            limit,
        )?;
        let profile_fill_count = append_unique_chunks_with_tail_reserve(
            &mut chunks,
            profile_chunks,
            limit,
            SOURCE_PROFILE_GROUNDING_RESERVE,
        );
        if let Some(object) = trace.as_object_mut() {
            object.insert("profile_fill_count".to_string(), profile_fill_count.into());
        }
        if profile_fill_count > 0 {
            qa_mode = "hybrid";
            route_reason = "source_with_profile_fill";
        }
        if chunks.is_empty() {
            chunks = self.storage.recent_chunks(author, limit)?;
            trace = json!({
                "qa_mode": "fallback_recent",
                "route_reason": "no_retrieval_candidates",
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
        } else {
            annotate_route_trace(&mut trace, qa_mode, route_reason);
        }
        self.annotate_profile_version(&mut trace);
        Ok((chunks, trace))
    }

    fn profile_grounding_chunks(
        &self,
        profiles: &[serde_json::Value],
        limit: usize,
    ) -> Result<Vec<crate::storage::SourceChunk>> {
        retrieval_profile_grounding_chunks(self.storage, profiles, limit)
    }

    fn profile_grounding_chunks_matching_terms(
        &self,
        profiles: &[serde_json::Value],
        terms: &[String],
        excluded_chunk_ids: &[i64],
        limit: usize,
    ) -> Result<Vec<crate::storage::SourceChunk>> {
        retrieval_profile_grounding_chunks_matching_terms(
            self.storage,
            profiles,
            terms,
            excluded_chunk_ids,
            limit,
        )
    }

    fn search_chunks(
        &self,
        author: &str,
        question: &str,
        limit: usize,
    ) -> Result<(Vec<crate::storage::SourceChunk>, serde_json::Value)> {
        let Some(embedding) = self.embedding.as_ref() else {
            let (chunks, mut trace) =
                hybrid::search_chunks_with_trace(self.storage, author, question, limit)?;
            self.annotate_profile_version(&mut trace);
            return Ok((chunks, trace));
        };
        let query_vectors = embedding.embed(&[question.to_string()])?;
        let Some(query_vector) = query_vectors.first() else {
            let (chunks, mut trace) =
                hybrid::search_chunks_with_trace(self.storage, author, question, limit)?;
            self.annotate_profile_version(&mut trace);
            return Ok((chunks, trace));
        };
        let (chunks, mut trace) = hybrid::search_chunks_with_dense_vector_trace(
            self.storage,
            author,
            question,
            limit,
            embedding.model_name(),
            embedding.model_version(),
            query_vector,
        )?;
        self.annotate_profile_version(&mut trace);
        Ok((chunks, trace))
    }

    fn annotate_profile_version(&self, trace: &mut serde_json::Value) {
        if let Some(object) = trace.as_object_mut() {
            object.insert(
                "qa_profile_version".to_string(),
                self.profile_version.as_str().into(),
            );
        }
    }

    fn log_answer(&self, context: AnswerLogContext<'_>, raw_answer: &str) -> Result<()> {
        let telegram_chat_id = context.telegram_context.map(|context| context.chat_id);
        let telegram_job_id = context.telegram_context.map(|context| context.job_id);
        let retrieval = json!({
            "author_profile_present": context.author_profile.is_some(),
            "qa_profile_version": self.profile_version.as_str(),
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
                "caption_object_type": chunk.caption_object_type,
                "caption_object_label": chunk.caption_object_label,
                "caption_panel_labels": chunk.caption_panel_labels_value(),
                "caption_target_labels": chunk.caption_target_labels_value(),
                "caption_panel_details": chunk.caption_panel_details_value(),
                "caption_measurements": chunk.caption_measurements_value(),
                "caption_conditions": chunk.caption_conditions_value(),
                "caption_values": chunk.caption_values_value(),
                "source_hash": chunk.source_hash,
                "chunk_hash": stored_chunk_hash(chunk),
                "chunker_version": chunk.chunker_version,
            })).collect::<Vec<_>>(),
            "qa_mode": route_trace_field(context.retrieval_trace, "qa_mode"),
            "route_reason": route_trace_field(context.retrieval_trace, "route_reason"),
            "delivery_mode": context.delivery_mode,
            "streaming_finalized": context.streaming_finalized,
            "stream_stats": stream_stats_json(context.stream_stats),
            "telegram_chat_id": telegram_chat_id,
            "telegram_job_id": telegram_job_id,
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
                qa_profile_version: Some(self.profile_version.as_str()),
                qa_mode: route_trace_field(context.retrieval_trace, "qa_mode"),
                route_reason: route_trace_field(context.retrieval_trace, "route_reason"),
                delivery_mode: Some(context.delivery_mode),
                streaming_finalized: context.streaming_finalized,
                stream_delta_count: context.stream_stats.map(|stats| stats.delta_count),
                streamed_chars: context.stream_stats.map(|stats| stats.streamed_chars),
                stream_first_delta_ms: context.stream_stats.and_then(|stats| stats.first_delta_ms),
                stream_duration_ms: context.stream_stats.map(|stats| stats.duration_ms),
                telegram_chat_id,
                telegram_job_id,
            }),
        })
    }

    fn log_failed_answer(
        &self,
        context: AnswerLogContext<'_>,
        raw_answer: &str,
        error: &str,
        error_code: &str,
    ) -> Result<()> {
        let telegram_chat_id = context.telegram_context.map(|context| context.chat_id);
        let telegram_job_id = context.telegram_context.map(|context| context.job_id);
        let retrieval = json!({
            "author_profile_present": context.author_profile.is_some(),
            "qa_profile_version": self.profile_version.as_str(),
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
                "caption_object_type": chunk.caption_object_type,
                "caption_object_label": chunk.caption_object_label,
                "caption_panel_labels": chunk.caption_panel_labels_value(),
                "caption_target_labels": chunk.caption_target_labels_value(),
                "caption_panel_details": chunk.caption_panel_details_value(),
                "caption_measurements": chunk.caption_measurements_value(),
                "caption_conditions": chunk.caption_conditions_value(),
                "caption_values": chunk.caption_values_value(),
                "source_hash": chunk.source_hash,
                "chunk_hash": stored_chunk_hash(chunk),
                "chunker_version": chunk.chunker_version,
            })).collect::<Vec<_>>(),
            "qa_mode": route_trace_field(context.retrieval_trace, "qa_mode"),
            "route_reason": route_trace_field(context.retrieval_trace, "route_reason"),
            "delivery_mode": context.delivery_mode,
            "streaming_finalized": context.streaming_finalized,
            "stream_stats": stream_stats_json(context.stream_stats),
            "telegram_chat_id": telegram_chat_id,
            "telegram_job_id": telegram_job_id,
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
                error_code: Some(error_code),
                qa_profile_version: Some(self.profile_version.as_str()),
                qa_mode: route_trace_field(context.retrieval_trace, "qa_mode"),
                route_reason: route_trace_field(context.retrieval_trace, "route_reason"),
                delivery_mode: Some(context.delivery_mode),
                streaming_finalized: context.streaming_finalized,
                stream_delta_count: context.stream_stats.map(|stats| stats.delta_count),
                streamed_chars: context.stream_stats.map(|stats| stats.streamed_chars),
                stream_first_delta_ms: context.stream_stats.and_then(|stats| stats.first_delta_ms),
                stream_duration_ms: context.stream_stats.map(|stats| stats.duration_ms),
                telegram_chat_id,
                telegram_job_id,
            }),
        })
    }

    fn valid_or_repaired_answer(
        &self,
        question: &str,
        raw_answer: &str,
        chunks: &[crate::storage::SourceChunk],
    ) -> Result<String> {
        match verify_qa_answer(raw_answer, chunks) {
            Ok(_) => Ok(raw_answer.to_string()),
            Err(error) => {
                let repaired = self.llm.chat(
                    qa_repair_messages(question, raw_answer, &error.to_string(), chunks),
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

fn record_stream_delta(stats: &mut AnswerStreamLogStats, started: &Instant, delta: &str) {
    stats.delta_count += 1;
    stats.streamed_chars += delta.chars().count() as i64;
    if stats.first_delta_ms.is_none() {
        stats.first_delta_ms = Some(started.elapsed().as_millis() as i64);
    }
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

fn append_unique_chunks_with_tail_reserve(
    target: &mut Vec<crate::storage::SourceChunk>,
    candidates: Vec<crate::storage::SourceChunk>,
    limit: usize,
    reserve: usize,
) -> usize {
    let reserve = reserve.min(limit);
    let mut additions = Vec::new();
    for chunk in candidates {
        if additions.len() >= reserve {
            break;
        }
        if target.iter().any(|existing| existing.id == chunk.id)
            || additions
                .iter()
                .any(|existing: &crate::storage::SourceChunk| existing.id == chunk.id)
        {
            continue;
        }
        additions.push(chunk);
    }
    let added = additions.len();
    if added == 0 {
        return 0;
    }
    target.truncate(limit.saturating_sub(added));
    target.extend(additions);
    added
}

fn annotate_route_trace(trace: &mut Value, qa_mode: &str, route_reason: &str) {
    if let Some(object) = trace.as_object_mut() {
        object.insert("qa_mode".to_string(), qa_mode.into());
        object.insert("route_reason".to_string(), route_reason.into());
    }
}

fn route_trace_field<'a>(trace: &'a Value, field: &str) -> Option<&'a str> {
    trace.get(field).and_then(Value::as_str)
}

fn stream_stats_json(stats: Option<AnswerStreamLogStats>) -> Value {
    match stats {
        Some(stats) => json!({
            "delta_count": stats.delta_count,
            "streamed_chars": stats.streamed_chars,
            "first_delta_ms": stats.first_delta_ms,
            "duration_ms": stats.duration_ms,
        }),
        None => Value::Null,
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
                "caption_object_type": chunk.caption_object_type,
                "caption_object_label": chunk.caption_object_label,
                "caption_panel_labels": chunk.caption_panel_labels_value(),
                "caption_target_labels": chunk.caption_target_labels_value(),
                "caption_panel_details": chunk.caption_panel_details_value(),
                "caption_measurements": chunk.caption_measurements_value(),
                "caption_conditions": chunk.caption_conditions_value(),
                "caption_values": chunk.caption_values_value(),
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
        "qa_profile_version": trace.get("qa_profile_version").and_then(Value::as_str),
        "qa_mode": trace.get("qa_mode").and_then(Value::as_str),
        "route_reason": trace.get("route_reason").and_then(Value::as_str),
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

fn quote_profile_author(author: &str) -> String {
    format!("\"{}\"", author.replace('"', "\\\""))
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
        Answerer, QaProfileVersion, evidence_snapshot, parse_qa_chunk_limit, profile_source_hash,
        retrieval_trace_summary,
    };
    use crate::papers::models::Paper;
    use crate::qa::renderer::render_qa_answer;
    use crate::qa::verifier::verify_qa_answer;
    use crate::retrieval::chunker::chunk_paper;
    use crate::schemas::author_profile::AUTHOR_PROFILE_SCHEMA_VERSION;
    use crate::schemas::paper_profile::PAPER_PROFILE_SCHEMA_VERSION;
    use crate::storage::{
        NewAuthorProfileV2, NewPaperProfileV2, PaperProfileMetadata, SourceChunk, Storage,
    };
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
            caption_object_type: None,
            caption_object_label: None,
            caption_panel_labels_json: None,
            caption_target_labels_json: None,
            caption_panel_details_json: None,
            caption_measurements_json: None,
            caption_conditions_json: None,
            caption_values_json: None,
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
            caption_object_type: None,
            caption_object_label: None,
            caption_panel_labels_json: None,
            caption_target_labels_json: None,
            caption_panel_details_json: None,
            caption_measurements_json: None,
            caption_conditions_json: None,
            caption_values_json: None,
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
            caption_object_type: None,
            caption_object_label: None,
            caption_panel_labels_json: None,
            caption_target_labels_json: None,
            caption_panel_details_json: None,
            caption_measurements_json: None,
            caption_conditions_json: None,
            caption_values_json: None,
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
    fn v2_profile_context_uses_saved_v2_profiles_and_author_profile() {
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
                ("title".to_string(), "V2 Paper".to_string()),
                ("doi".to_string(), "10.1/test".to_string()),
                ("year".to_string(), "2026".to_string()),
            ]),
            fetch_result: json!({}),
            raw_body: String::new(),
            clean_text: "V2 result.".to_string(),
            sections: Vec::new(),
        };
        storage.upsert_paper(&paper, &[]).unwrap();
        storage
            .save_paper_profile_v2(NewPaperProfileV2 {
                paper_key: "Alice/paper-a",
                profile_json: &json!({
                    "paper_key": "Alice/paper-a",
                    "title": "V2 Paper",
                    "one_sentence_summary": "V2 supported summary.",
                    "factual_objects": [{
                        "claim_uid": "claim-a",
                        "chunk_fact_id": 1,
                        "fact_type": "result",
                        "claim": "V2 result",
                        "evidence": [{
                            "paper_key": "Alice/paper-a",
                            "chunk_fact_id": 1,
                            "claim_uid": "claim-a",
                            "chunk_id": 7,
                            "chunk_index": 0,
                            "source_hash": "source-a",
                            "chunk_hash": "chunk-a"
                        }]
                    }]
                }),
                profile_schema_version: 2,
                builder_version: "paper-profile-v2-test",
                model_id: "test-model",
                source_fact_hash: "facts-a",
            })
            .unwrap();
        storage
            .save_author_profile_v2(NewAuthorProfileV2 {
                author: "Alice",
                profile_json: &json!({
                    "author": "Alice",
                    "total_profiled_papers": 1,
                    "research_themes": [{
                        "theme": "V2 theme",
                        "summary": "V2 author summary",
                        "support_refs": [{
                            "support_uid": "support-a",
                            "paper_key": "Alice/paper-a",
                            "title": "V2 Paper",
                            "claim_uid": "claim-a",
                            "chunk_fact_id": 1,
                            "chunk_id": 7,
                            "source_hash": "source-a",
                            "chunk_hash": "chunk-a"
                        }],
                        "confidence": "medium"
                    }],
                    "source_profile_keys": ["Alice/paper-a"],
                    "builder_version": "author-profile-v2-test"
                }),
                profile_schema_version: 2,
                builder_version: "author-profile-v2-test",
                model_id: "test-model",
                source_profile_hash: "profiles-a",
            })
            .unwrap();
        let answerer = Answerer::new_with_profile_version(
            &storage,
            test_llm("test-model"),
            QaProfileVersion::V2,
        );

        let (author_profile, profiles) = answerer.profile_context("Alice", "V2").unwrap();

        assert_eq!(author_profile.unwrap()["author"], "Alice");
        assert_eq!(profiles.len(), 1);
        assert_eq!(profiles[0]["paper_key"], "Alice/paper-a");
        assert_eq!(profiles[0]["factual_objects"][0]["claim_uid"], "claim-a");
    }

    #[test]
    fn v2_profile_context_explains_missing_profiles() {
        let dir = tempdir().unwrap();
        let storage = Storage::open(&dir.path().join("test.sqlite")).unwrap();
        let answerer = Answerer::new_with_profile_version(
            &storage,
            test_llm("test-model"),
            QaProfileVersion::V2,
        );

        let error = answerer
            .profile_context("Alice", "overview")
            .unwrap_err()
            .to_string();

        assert!(error.contains("no V2 paper profiles for Alice"));
        assert!(error.contains("ppc comprehend --author \"Alice\" --v2"));
    }

    #[test]
    fn non_detail_profile_context_keeps_all_profiled_papers() {
        let dir = tempdir().unwrap();
        let mut storage = Storage::open(&dir.path().join("test.sqlite")).unwrap();
        for (paper_id, title, keyword) in [
            ("paper-a", "A Paper", "topic-a"),
            ("paper-b", "B Paper", "topic-b"),
            ("paper-c", "C Paper", "topic-c"),
        ] {
            let paper = Paper {
                author: "Alice".to_string(),
                paper_id: paper_id.to_string(),
                paper_dir: dir.path().to_path_buf(),
                article_path: dir.path().join(format!("{paper_id}.md")),
                fetch_result_path: None,
                source_hash: format!("{paper_id}-source"),
                metadata: std::collections::BTreeMap::from([
                    ("title".to_string(), title.to_string()),
                    ("doi".to_string(), format!("10.1/{paper_id}")),
                    ("year".to_string(), "2026".to_string()),
                ]),
                fetch_result: json!({}),
                raw_body: String::new(),
                clean_text: String::new(),
                sections: vec![crate::papers::models::Section {
                    title: "Article Text".to_string(),
                    level: 2,
                    content: format!("Abstract\nThis paper covers {keyword}."),
                }],
            };
            let chunks = chunk_paper(&paper, 3200, 350);
            storage.upsert_paper(&paper, &chunks).unwrap();
            storage
                .save_paper_profile_with_metadata(
                    &paper.key(),
                    &json!({
                        "paper_key": paper.key(),
                        "title": title,
                        "doi": format!("10.1/{paper_id}"),
                        "year": "2026",
                        "one_sentence_summary": format!("This paper covers {keyword}."),
                        "methods": [{"method": keyword, "evidence_chunks": [0]}],
                        "topic_keywords": [keyword]
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
        }
        let answerer = Answerer::new(&storage, test_llm("test-model"));

        let (_, profiles) = answerer
            .profile_context("Alice", "这三篇文献的题目+时间+简要内容是什么")
            .unwrap();
        let (chunks, trace) = answerer
            .context_chunks(
                "Alice",
                "这三篇文献的题目+时间+简要内容是什么",
                &profiles,
                8,
            )
            .unwrap();

        assert_eq!(profiles.len(), 3);
        assert_eq!(chunks.len(), 3);
        assert_eq!(trace["qa_mode"], "profile_first");
        assert_eq!(trace["route_reason"], "broad_profile_context");
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
        assert_eq!(trace["qa_mode"], "profile_first");
        assert_eq!(trace["route_reason"], "broad_profile_context");
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
        let qa_logs = storage.qa_logs(Some("Alice"), 1).unwrap();
        assert_eq!(qa_logs[0].qa_profile_version.as_deref(), Some("v1"));
        assert_eq!(qa_logs[0].qa_mode.as_deref(), Some("profile_first"));
        assert_eq!(
            qa_logs[0].route_reason.as_deref(),
            Some("broad_profile_context")
        );
        assert_eq!(qa_logs[0].delivery_mode.as_deref(), Some("blocking"));
        assert_eq!(qa_logs[0].streaming_finalized, None);
        handle.join().unwrap();
    }

    #[test]
    fn answer_stream_logs_failed_stream_state() {
        let dir = tempdir().unwrap();
        let storage = Storage::open(&dir.path().join("test.sqlite")).unwrap();
        let answerer = Answerer::new(&storage, test_llm("test-model"));
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();

        let error = runtime
            .block_on(answerer.answer_stream("Alice", "question", |_| Ok(())))
            .unwrap_err()
            .to_string();

        assert!(!error.trim().is_empty());
        let qa_logs = storage.qa_logs(Some("Alice"), 1).unwrap();
        assert_eq!(qa_logs.len(), 1);
        assert_eq!(qa_logs[0].error_code.as_deref(), Some("stream_failed"));
        assert_eq!(qa_logs[0].delivery_mode.as_deref(), Some("streaming"));
        assert_eq!(qa_logs[0].streaming_finalized, Some(false));
        assert_eq!(qa_logs[0].stream_delta_count, Some(0));
        assert_eq!(qa_logs[0].streamed_chars, Some(0));
        assert_eq!(qa_logs[0].stream_first_delta_ms, None);
        assert!(qa_logs[0].stream_duration_ms.is_some());
    }

    #[test]
    fn answer_stream_logs_finalized_stream_state() {
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
        let (base_url, request_rx, handle) = start_chat_stream_server(raw_answer);
        let answerer = Answerer::new(&storage, test_llm_at("test-model", &base_url));
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let mut deltas = Vec::new();

        let rendered = runtime
            .block_on(answerer.answer_stream_with_telegram_context(
                "Alice",
                "What does the MOF catalysis paper study?",
                7,
                42,
                |delta| {
                    deltas.push(delta.to_string());
                    Ok(())
                },
            ))
            .unwrap();
        let request = request_rx
            .recv_timeout(Duration::from_secs(2))
            .expect("mock LLM should receive one streaming request");

        assert!(request.contains("\"stream\":true"));
        assert!(!deltas.is_empty());
        assert!(rendered.contains("这篇论文研究 MOF catalysis。"));
        let qa_logs = storage.qa_logs(Some("Alice"), 1).unwrap();
        assert_eq!(qa_logs[0].delivery_mode.as_deref(), Some("streaming"));
        assert_eq!(qa_logs[0].streaming_finalized, Some(true));
        assert_eq!(qa_logs[0].stream_delta_count, Some(1));
        assert_eq!(
            qa_logs[0].streamed_chars,
            Some(
                deltas
                    .iter()
                    .map(|delta| delta.chars().count() as i64)
                    .sum()
            )
        );
        assert!(qa_logs[0].stream_first_delta_ms.is_some());
        assert!(qa_logs[0].stream_duration_ms.is_some());
        assert_eq!(qa_logs[0].error_code, None);
        assert_eq!(qa_logs[0].telegram_chat_id, Some(7));
        assert_eq!(qa_logs[0].telegram_job_id, Some(42));
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

    fn start_chat_stream_server(
        answer: String,
    ) -> (String, std::sync::mpsc::Receiver<String>, JoinHandle<()>) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let (tx, rx) = std::sync::mpsc::channel();
        let handle = thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let request = read_http_request(&mut stream);
            tx.send(request).unwrap();
            let event = json!({
                "choices": [{"delta": {"content": answer}}]
            })
            .to_string();
            let body = format!("data: {event}\n\ndata: [DONE]\n\n");
            let response = format!(
                "HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{}",
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
