use serde_json::{Value, json};

use crate::papers::models::Paper;
use crate::retrieval::chunker::Chunk;
use crate::storage::SourceChunk;

use super::llm::ChatMessage;

const PAPER_ANALYSIS_SYSTEM: &str = r#"你是严谨的科研论文分析助手，擅长从论文全文中提取可复用的结构化理解。
要求：
1. 只基于给定论文内容，不编造。
2. 忽略网页导航、下载按钮、复制链接、被引列表等出版商页面噪声。
3. 输出必须是 JSON，不要 Markdown。
4. 如果某项信息在材料中找不到，用空数组或空字符串。
5. methods、key_results、limitations 中的每一条 claim 必须引用 paper_chunks 中真实存在的 chunk_id。"#;

const AUTHOR_PROFILE_SYSTEM: &str = r#"你是科研成果分析助手，需要根据多篇论文的结构化理解，归纳作者研究画像。
要求：
1. 只基于给定 paper profiles。
2. 输出必须是 JSON，不要 Markdown。
3. 结论要能服务后续问答，并保留代表论文依据。"#;

const QA_SYSTEM: &str = r#"你是基于本地论文库的科研问答助手。
要求：
1. paper_profiles 只能用于路由和摘要，最终事实依据必须来自 source_chunks。
2. 不要使用外部知识编造答案。
3. 如果 source_chunks 不足以支持结论，直接说明证据不足。
4. 输出必须是 JSON，不要 Markdown，不要代码块。
5. evidence 只能引用给定 source_chunks 中真实存在的 paper_key 和 chunk_id。"#;

pub fn paper_analysis_messages(paper: &Paper, context: &str) -> Vec<ChatMessage> {
    paper_analysis_messages_with_chunks(paper, context, &[])
}

pub fn paper_profile_repair_messages(
    raw: &str,
    validation_error: &str,
    chunks: &[Chunk],
) -> Vec<ChatMessage> {
    let allowed_chunks = chunks
        .iter()
        .map(|chunk| {
            json!({
                "chunk_id": chunk.chunk_index,
                "section": chunk.section,
            })
        })
        .collect::<Vec<_>>();
    let user = json!({
        "task": "把 raw_profile 修复为 PaperProfileV1 JSON。只能输出 JSON，不要 Markdown。",
        "validation_error": validation_error,
        "allowed_chunks": allowed_chunks,
        "required_fields": ["paper_key", "title", "doi", "year", "one_sentence_summary"],
        "provenance_fields": ["methods.evidence_chunks", "key_results.evidence_chunks", "limitations.evidence_chunks"],
        "raw_profile": raw,
    });
    vec![
        ChatMessage {
            role: "system".to_string(),
            content: PAPER_ANALYSIS_SYSTEM.to_string(),
        },
        ChatMessage {
            role: "user".to_string(),
            content: user.to_string(),
        },
    ]
}

pub fn paper_analysis_messages_with_chunks(
    paper: &Paper,
    context: &str,
    chunks: &[Chunk],
) -> Vec<ChatMessage> {
    let paper_chunks = chunks
        .iter()
        .map(|chunk| {
            json!({
                "chunk_id": chunk.chunk_index,
                "section": chunk.section,
                "text": chunk.text,
            })
        })
        .collect::<Vec<_>>();
    let user = json!({
        "task": "请为这篇论文生成第一层理解，供后续问答复用。",
        "metadata": {
            "author": paper.author,
            "paper_id": paper.paper_id,
            "title": paper.title(),
            "doi": paper.doi(),
            "year": paper.year(),
            "source": paper.source(),
        },
        "output_schema": {
            "paper_key": paper.key(),
            "title": "string",
            "doi": "string",
            "year": "string",
            "one_sentence_summary": "string",
            "research_question": "string",
            "core_contributions": ["string"],
            "methods": [
                {
                    "method": "string",
                    "evidence_chunks": [0],
                    "section": "string"
                }
            ],
            "key_results": [
                {
                    "claim": "string",
                    "evidence_chunks": [0],
                    "section": "string",
                    "confidence": "high|medium|low"
                }
            ],
            "limitations": [
                {
                    "limitation": "string",
                    "evidence_chunks": [0],
                    "section": "string"
                }
            ],
            "topic_keywords": ["string"],
            "reliable_answer_scope": ["string"],
            "evidence_notes": ["string"]
        },
        "paper_chunks": paper_chunks,
        "paper_text": context,
    });
    vec![
        ChatMessage {
            role: "system".to_string(),
            content: PAPER_ANALYSIS_SYSTEM.to_string(),
        },
        ChatMessage {
            role: "user".to_string(),
            content: user.to_string(),
        },
    ]
}

pub fn author_profile_messages(author: &str, profiles: &[Value]) -> Vec<ChatMessage> {
    let user = json!({
        "task": "请根据这些论文理解生成作者级研究画像。",
        "author": author,
        "output_schema": {
            "author": author,
            "research_areas": ["string"],
            "research_evolution": ["string"],
            "representative_works": [
                {"year": "string", "title": "string", "doi": "string", "reason": "string"}
            ],
            "methodological_strengths": ["string"],
            "answer_scope": ["string"]
        },
        "paper_profiles": profiles,
    });
    vec![
        ChatMessage {
            role: "system".to_string(),
            content: AUTHOR_PROFILE_SYSTEM.to_string(),
        },
        ChatMessage {
            role: "user".to_string(),
            content: user.to_string(),
        },
    ]
}

pub fn author_profile_repair_messages(
    author: &str,
    raw: &str,
    validation_error: &str,
) -> Vec<ChatMessage> {
    let user = json!({
        "task": "把 raw_profile 修复为 AuthorProfileV1 JSON。只能输出 JSON，不要 Markdown。",
        "validation_error": validation_error,
        "required_author": author,
        "required_fields": ["author", "answer_scope"],
        "raw_profile": raw,
    });
    vec![
        ChatMessage {
            role: "system".to_string(),
            content: AUTHOR_PROFILE_SYSTEM.to_string(),
        },
        ChatMessage {
            role: "user".to_string(),
            content: user.to_string(),
        },
    ]
}

pub fn qa_messages(question: &str, profiles: &[Value], chunks: &[SourceChunk]) -> Vec<ChatMessage> {
    let source_chunks: Vec<Value> = chunks
        .iter()
        .map(|chunk| {
            json!({
                "paper_key": chunk.paper_key,
                "chunk_id": chunk.id,
                "chunk_index": chunk.chunk_index,
                "title": chunk.title,
                "doi": chunk.doi,
                "year": chunk.year,
                "section": chunk.section,
                "text": chunk.text,
            })
        })
        .collect();
    let user = json!({
        "question": question,
        "output_schema": {
            "answer": "string",
            "evidence": [
                {
                    "paper_key": "author/paper_id",
                    "title": "string",
                    "doi": "string",
                    "year": "string",
                    "chunk_id": 123,
                    "section": "string",
                    "quote_or_summary": "string"
                }
            ],
            "uncertainty": "string",
            "followup_queries": ["string"]
        },
        "paper_profiles": profiles,
        "source_chunks": source_chunks,
    });
    vec![
        ChatMessage {
            role: "system".to_string(),
            content: QA_SYSTEM.to_string(),
        },
        ChatMessage {
            role: "user".to_string(),
            content: user.to_string(),
        },
    ]
}

pub fn qa_repair_messages(
    raw: &str,
    validation_error: &str,
    chunks: &[SourceChunk],
) -> Vec<ChatMessage> {
    let allowed_evidence = chunks
        .iter()
        .map(|chunk| {
            json!({
                "paper_key": chunk.paper_key,
                "chunk_id": chunk.id,
                "title": chunk.title,
                "doi": chunk.doi,
                "year": chunk.year,
                "section": chunk.section,
            })
        })
        .collect::<Vec<_>>();
    let user = json!({
        "task": "把 raw_answer 修复为符合 QaAnswerV1 的 JSON。只能输出 JSON，不要 Markdown。",
        "validation_error": validation_error,
        "output_schema": {
            "answer": "string",
            "evidence": [
                {
                    "paper_key": "author/paper_id",
                    "title": "string",
                    "doi": "string",
                    "year": "string",
                    "chunk_id": 123,
                    "section": "string",
                    "quote_or_summary": "string"
                }
            ],
            "uncertainty": "string",
            "followup_queries": ["string"]
        },
        "allowed_evidence": allowed_evidence,
        "raw_answer": raw,
    });
    vec![
        ChatMessage {
            role: "system".to_string(),
            content: QA_SYSTEM.to_string(),
        },
        ChatMessage {
            role: "user".to_string(),
            content: user.to_string(),
        },
    ]
}

#[cfg(test)]
mod tests {
    use crate::retrieval::chunker::Chunk;

    use super::{author_profile_repair_messages, paper_profile_repair_messages};

    #[test]
    fn paper_profile_repair_prompt_includes_error_and_allowed_chunks() {
        let messages = paper_profile_repair_messages(
            "{\"raw\":true}",
            "missing field",
            &[Chunk {
                paper_key: "Alice/paper-a".to_string(),
                chunk_index: 3,
                section: "Results".to_string(),
                text: "text".to_string(),
            }],
        );
        let user = &messages[1].content;
        assert!(user.contains("missing field"));
        assert!(user.contains("\"chunk_id\":3"));
        assert!(user.contains("{\\\"raw\\\":true}"));
    }

    #[test]
    fn author_profile_repair_prompt_includes_required_author() {
        let messages = author_profile_repair_messages("Alice", "{\"raw\":true}", "missing scope");
        let user = &messages[1].content;
        assert!(user.contains("Alice"));
        assert!(user.contains("missing scope"));
        assert!(user.contains("AuthorProfileV1"));
    }
}
