use crate::qa::verifier::parse_qa_answer;

pub fn render_qa_answer(content: &str) -> String {
    parse_qa_answer(content)
        .map(|answer| answer.render())
        .unwrap_or_else(|| content.to_string())
}
