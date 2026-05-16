use crate::qa::verifier::parse_qa_answer;
use crate::schemas::qa_answer::QaAnswerV1;

pub fn render_qa_answer(content: &str) -> String {
    render_qa_answer_with_options(content, RenderOptions::full())
}

pub fn render_qa_answer_for_question(content: &str, question: &str) -> String {
    render_qa_answer_with_options(
        content,
        RenderOptions {
            show_evidence: asks_for_evidence(question),
            show_uncertainty: asks_for_evidence(question),
            show_followups: false,
        },
    )
}

#[derive(Debug, Clone, Copy)]
struct RenderOptions {
    show_evidence: bool,
    show_uncertainty: bool,
    show_followups: bool,
}

impl RenderOptions {
    fn full() -> Self {
        Self {
            show_evidence: true,
            show_uncertainty: true,
            show_followups: true,
        }
    }
}

fn render_qa_answer_with_options(content: &str, options: RenderOptions) -> String {
    parse_qa_answer(content)
        .map(|answer| render_structured_answer(&answer, options))
        .unwrap_or_else(|| content.to_string())
}

fn render_structured_answer(answer: &QaAnswerV1, options: RenderOptions) -> String {
    let mut lines = vec![answer.answer.trim().to_string()];
    if options.show_evidence && !answer.evidence.is_empty() {
        lines.push(String::new());
        lines.push("依据：".to_string());
        for (index, item) in answer.evidence.iter().enumerate() {
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
    if options.show_uncertainty && !answer.uncertainty.trim().is_empty() {
        lines.push(String::new());
        lines.push(format!("不确定性：{}", answer.uncertainty.trim()));
    }
    if options.show_followups && !answer.followup_queries.is_empty() {
        lines.push(String::new());
        lines.push("可继续追问：".to_string());
        for query in answer
            .followup_queries
            .iter()
            .filter(|query| !query.trim().is_empty())
        {
            lines.push(format!("- {}", query.trim()));
        }
    }
    lines.join("\n")
}

fn asks_for_evidence(question: &str) -> bool {
    let lowered = question.to_lowercase();
    [
        "依据",
        "证据",
        "来源",
        "出处",
        "原文",
        "引用",
        "在哪",
        "在哪里",
        "哪一段",
        "哪篇",
        "chunk",
        "source",
        "evidence",
        "citation",
        "cite",
        "quote",
        "reference",
    ]
    .iter()
    .any(|keyword| lowered.contains(keyword))
}

#[cfg(test)]
mod tests {
    use super::{render_qa_answer, render_qa_answer_for_question};

    const RAW: &str = r#"{
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

    #[test]
    fn default_question_renders_concise_answer_only() {
        assert_eq!(
            render_qa_answer_for_question(RAW, "这篇论文讲什么？"),
            "这是答案。"
        );
    }

    #[test]
    fn evidence_question_renders_sources_and_uncertainty() {
        let rendered = render_qa_answer_for_question(RAW, "依据在哪里？");

        assert!(rendered.contains("这是答案。"));
        assert!(rendered.contains("依据："));
        assert!(rendered.contains("[1] 2024 A Paper 10.1/test section=Results chunk=7"));
        assert!(rendered.contains("不确定性：仅覆盖给定片段。"));
        assert!(!rendered.contains("可继续追问"));
    }

    #[test]
    fn full_render_keeps_legacy_sections() {
        let rendered = render_qa_answer(RAW);

        assert!(rendered.contains("依据："));
        assert!(rendered.contains("不确定性：仅覆盖给定片段。"));
        assert!(rendered.contains("- 继续问方法"));
    }
}
