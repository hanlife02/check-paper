use serde_json::{Value, json};

use crate::papers::models::Paper;
use crate::storage::SourceChunk;

use super::llm::ChatMessage;

const PAPER_ANALYSIS_SYSTEM: &str = r#"你是严谨的科研论文分析助手，擅长从论文全文中提取可复用的结构化理解。
要求：
1. 只基于给定论文内容，不编造。
2. 忽略网页导航、下载按钮、复制链接、被引列表等出版商页面噪声。
3. 输出必须是 JSON，不要 Markdown。
4. 如果某项信息在材料中找不到，用空数组或空字符串。"#;

const AUTHOR_PROFILE_SYSTEM: &str = r#"你是科研成果分析助手，需要根据多篇论文的结构化理解，归纳作者研究画像。
要求：
1. 只基于给定 paper profiles。
2. 输出必须是 JSON，不要 Markdown。
3. 结论要能服务后续问答，并保留代表论文依据。"#;

const QA_SYSTEM: &str = r#"你是基于本地论文库的科研问答助手。
要求：
1. 优先使用给定的论文理解；如果给了原文片段，可以用原文片段补充细节。
2. 不要使用外部知识编造答案。
3. 如果证据不足，直接说明不足，并指出需要回到哪类原文信息。
4. 回答应包含依据，至少列出相关论文的年份、标题和 DOI。"#;

pub fn paper_analysis_messages(paper: &Paper, context: &str) -> Vec<ChatMessage> {
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
            "methods": ["string"],
            "key_results": ["string"],
            "limitations": ["string"],
            "topic_keywords": ["string"],
            "reliable_answer_scope": ["string"],
            "evidence_notes": ["string"]
        },
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

pub fn qa_messages(question: &str, profiles: &[Value], chunks: &[SourceChunk]) -> Vec<ChatMessage> {
    let source_chunks: Vec<Value> = chunks
        .iter()
        .map(|chunk| {
            json!({
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
