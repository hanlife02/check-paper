use anyhow::Result;

use crate::schemas::qa_answer::QaAnswerV1;
use crate::storage::SourceChunk;
use crate::understanding::json_utils::parse_json_object;

pub fn parse_qa_answer(content: &str) -> Option<QaAnswerV1> {
    serde_json::from_value(parse_json_object(content)).ok()
}

pub fn verify_qa_answer(content: &str, chunks: &[SourceChunk]) -> Result<QaAnswerV1> {
    let answer: QaAnswerV1 = serde_json::from_value(parse_json_object(content))?;
    answer.validate(chunks)?;
    Ok(answer)
}
