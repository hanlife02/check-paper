use std::time::Instant;

use anyhow::Result;
use serde::{Deserialize, Serialize};
use serde_json::json;

use crate::storage::Storage;
use crate::understanding::json_utils::parse_json_object;
use crate::understanding::llm::OpenAiCompatibleClient;
use crate::understanding::prompts::{qa_messages, qa_repair_messages};

const PROFILE_CONTEXT_LIMIT: usize = 8;
const DEFAULT_CHUNK_LIMIT: usize = 8;
const RETRY_CHUNK_LIMIT: usize = 10;
const QA_CHUNK_LIMIT_ENV: &str = "CHECK_PAPER_QA_CHUNK_LIMIT";

pub struct Answerer<'a> {
    storage: &'a Storage,
    llm: OpenAiCompatibleClient,
}

impl<'a> Answerer<'a> {
    pub fn new(storage: &'a Storage, llm: OpenAiCompatibleClient) -> Self {
        Self { storage, llm }
    }

    pub fn answer(&self, author: &str, question: &str) -> Result<String> {
        let started = Instant::now();
        let profiles = self
            .storage
            .search_profiles(author, question, PROFILE_CONTEXT_LIMIT)?;
        let chunk_limit = qa_chunk_limit();
        let mut chunks = self.context_chunks(author, question, &profiles, chunk_limit)?;

        let first = self
            .llm
            .chat(qa_messages(question, &profiles, &chunks), 0.2, 2200)?;
        if should_retry_with_more_chunks(&first, &chunks) {
            chunks =
                self.context_chunks(author, question, &profiles, retry_chunk_limit(chunk_limit))?;
            let second = self
                .llm
                .chat(qa_messages(question, &profiles, &chunks), 0.2, 2600)?;
            let repaired = self.valid_or_repaired_answer(&second, &chunks)?;
            self.log_answer(author, question, &profiles, &chunks, &repaired, started)?;
            return Ok(render_qa_answer(&repaired));
        }
        let repaired = self.valid_or_repaired_answer(&first, &chunks)?;
        self.log_answer(author, question, &profiles, &chunks, &repaired, started)?;
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
        let profiles = self
            .storage
            .search_profiles(author, question, PROFILE_CONTEXT_LIMIT)?;
        let chunk_limit = qa_chunk_limit();
        let mut chunks = self.context_chunks(author, question, &profiles, chunk_limit)?;

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
            chunks =
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
            let repaired = self.valid_or_repaired_answer(&second, &chunks)?;
            self.log_answer(author, question, &profiles, &chunks, &repaired, started)?;
            return Ok(render_qa_answer(&repaired));
        }
        let repaired = self.valid_or_repaired_answer(&first, &chunks)?;
        self.log_answer(author, question, &profiles, &chunks, &repaired, started)?;
        Ok(render_qa_answer(&repaired))
    }

    fn context_chunks(
        &self,
        author: &str,
        question: &str,
        profiles: &[serde_json::Value],
        limit: usize,
    ) -> Result<Vec<crate::storage::SourceChunk>> {
        let mut chunks = self.storage.search_chunks(author, question, limit)?;
        if chunks.len() < limit {
            let paper_keys = profiles
                .iter()
                .filter_map(|profile| profile.get("paper_key").and_then(serde_json::Value::as_str))
                .map(str::to_string)
                .collect::<Vec<_>>();
            let remaining = limit - chunks.len();
            let profile_chunks = self.storage.chunks_for_paper_keys(&paper_keys, remaining)?;
            append_unique_chunks(&mut chunks, profile_chunks, limit);
        }
        if chunks.is_empty() {
            chunks = self.storage.recent_chunks(author, limit)?;
        }
        Ok(chunks)
    }

    fn log_answer(
        &self,
        author: &str,
        question: &str,
        profiles: &[serde_json::Value],
        chunks: &[crate::storage::SourceChunk],
        raw_answer: &str,
        started: Instant,
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
            })).collect::<Vec<_>>()
        });
        let answer_json = parse_qa_answer(raw_answer)
            .map(|answer| {
                serde_json::to_value(answer).unwrap_or_else(|_| json!({ "raw": raw_answer }))
            })
            .unwrap_or_else(|| json!({ "raw": raw_answer }));
        self.storage.save_qa_log(
            author,
            question,
            &retrieval,
            &answer_json,
            self.llm.model_name(),
            started.elapsed().as_millis() as i64,
        )
    }

    fn valid_or_repaired_answer(
        &self,
        raw_answer: &str,
        chunks: &[crate::storage::SourceChunk],
    ) -> Result<String> {
        match parse_and_validate_qa_answer(raw_answer, chunks) {
            Ok(_) => Ok(raw_answer.to_string()),
            Err(error) => {
                let repaired = self.llm.chat(
                    qa_repair_messages(raw_answer, &error.to_string(), chunks),
                    0.0,
                    1600,
                )?;
                parse_and_validate_qa_answer(&repaired, chunks)?;
                Ok(repaired)
            }
        }
    }
}

#[derive(Debug, Deserialize, Serialize)]
pub struct QaAnswerV1 {
    pub answer: String,
    #[serde(default)]
    pub evidence: Vec<QaEvidence>,
    #[serde(default)]
    pub uncertainty: String,
    #[serde(default)]
    pub followup_queries: Vec<String>,
}

#[derive(Debug, Deserialize, Serialize)]
pub struct QaEvidence {
    pub paper_key: String,
    #[serde(default)]
    pub title: String,
    #[serde(default)]
    pub doi: String,
    #[serde(default)]
    pub year: String,
    pub chunk_id: i64,
    #[serde(default)]
    pub section: String,
    #[serde(default)]
    pub quote_or_summary: String,
}

pub fn render_qa_answer(content: &str) -> String {
    parse_qa_answer(content)
        .map(|answer| answer.render())
        .unwrap_or_else(|| content.to_string())
}

fn parse_qa_answer(content: &str) -> Option<QaAnswerV1> {
    serde_json::from_value(parse_json_object(content)).ok()
}

fn parse_and_validate_qa_answer(
    content: &str,
    chunks: &[crate::storage::SourceChunk],
) -> Result<QaAnswerV1> {
    let answer: QaAnswerV1 = serde_json::from_value(parse_json_object(content))?;
    let valid_chunk_ids = chunks.iter().map(|chunk| chunk.id).collect::<Vec<_>>();
    for item in &answer.evidence {
        if !valid_chunk_ids.contains(&item.chunk_id) {
            anyhow::bail!(
                "evidence chunk_id {} is not present in provided source_chunks",
                item.chunk_id
            );
        }
        let Some(chunk) = chunks.iter().find(|chunk| chunk.id == item.chunk_id) else {
            continue;
        };
        if chunk.paper_key != item.paper_key {
            anyhow::bail!(
                "evidence paper_key {} does not match chunk {} paper_key {}",
                item.paper_key,
                item.chunk_id,
                chunk.paper_key
            );
        }
    }
    if answer.evidence.is_empty() && !signals_insufficient(&answer.answer) {
        anyhow::bail!("answer has no evidence and does not explicitly state insufficient evidence");
    }
    Ok(answer)
}

impl QaAnswerV1 {
    fn render(&self) -> String {
        let mut lines = vec![self.answer.trim().to_string()];
        if !self.evidence.is_empty() {
            lines.push(String::new());
            lines.push("依据：".to_string());
            for (index, item) in self.evidence.iter().enumerate() {
                let mut line = format!(
                    "[{}] {} {} {} section={} chunk={}",
                    index + 1,
                    item.year,
                    item.title,
                    item.doi,
                    item.section,
                    item.chunk_id
                );
                if !item.quote_or_summary.trim().is_empty() {
                    line.push_str(&format!("：{}", item.quote_or_summary.trim()));
                }
                lines.push(line);
            }
        }
        if !self.uncertainty.trim().is_empty() {
            lines.push(String::new());
            lines.push(format!("不确定性：{}", self.uncertainty.trim()));
        }
        if !self.followup_queries.is_empty() {
            lines.push(String::new());
            lines.push("可继续追问：".to_string());
            for query in self
                .followup_queries
                .iter()
                .filter(|query| !query.trim().is_empty())
            {
                lines.push(format!("- {}", query.trim()));
            }
        }
        lines.join("\n")
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

fn signals_insufficient(answer: &str) -> bool {
    let lowered = answer.to_lowercase();
    lowered.contains("insufficient_context")
        || answer.contains("证据不足")
        || answer.contains("信息不足")
}

#[cfg(test)]
mod tests {
    use super::{parse_and_validate_qa_answer, parse_qa_chunk_limit, render_qa_answer};
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

        assert!(parse_and_validate_qa_answer(raw, &chunks).is_err());
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
        }];
        let raw = r#"{
            "answer": "这是一个事实性结论。",
            "evidence": [],
            "uncertainty": "",
            "followup_queries": []
        }"#;

        assert!(parse_and_validate_qa_answer(raw, &chunks).is_err());
    }

    #[test]
    fn accepts_insufficient_answer_without_evidence() {
        let raw = r#"{
            "answer": "证据不足，无法回答。",
            "evidence": [],
            "uncertainty": "没有检索到相关片段。",
            "followup_queries": []
        }"#;

        parse_and_validate_qa_answer(raw, &[]).unwrap();
    }

    #[test]
    fn qa_chunk_limit_defaults_to_configurable_prompt_size() {
        assert_eq!(parse_qa_chunk_limit(None), 8);
        assert_eq!(parse_qa_chunk_limit(Some("12".to_string())), 12);
        assert_eq!(parse_qa_chunk_limit(Some("0".to_string())), 8);
        assert_eq!(parse_qa_chunk_limit(Some("31".to_string())), 8);
    }
}
