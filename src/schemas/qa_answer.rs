use anyhow::{Result, anyhow};
use serde::{Deserialize, Serialize};

use crate::storage::SourceChunk;

pub const QA_ANSWER_SCHEMA_VERSION: i64 = 1;

#[derive(Debug, Deserialize, Serialize)]
pub struct QaAnswerV1 {
    pub answer: String,
    #[serde(default)]
    pub claims: Vec<QaClaim>,
    #[serde(default)]
    pub evidence: Vec<QaEvidence>,
    #[serde(default)]
    pub uncertainty: String,
    #[serde(default)]
    pub followup_queries: Vec<String>,
}

#[derive(Debug, Deserialize, Serialize)]
pub struct QaClaim {
    pub claim: String,
    #[serde(default)]
    pub evidence_indices: Vec<usize>,
    #[serde(default)]
    pub support: String,
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

impl QaAnswerV1 {
    pub fn validate(&self, chunks: &[SourceChunk]) -> Result<()> {
        for item in &self.evidence {
            let chunk = chunks
                .iter()
                .find(|chunk| chunk.id == item.chunk_id)
                .ok_or_else(|| {
                    anyhow!(
                        "evidence chunk_id {} is not present in provided source_chunks",
                        item.chunk_id
                    )
                })?;
            if chunk.paper_key != item.paper_key {
                return Err(anyhow!(
                    "evidence paper_key {} does not match chunk {} paper_key {}",
                    item.paper_key,
                    item.chunk_id,
                    chunk.paper_key
                ));
            }
            validate_evidence_metadata(item, chunk)?;
            validate_quote_or_summary(item, chunk)?;
        }
        if self.evidence.is_empty() && !signals_insufficient(&self.answer) {
            return Err(anyhow!(
                "answer has no evidence and does not explicitly state insufficient evidence"
            ));
        }
        for claim in &self.claims {
            if claim.claim.trim().is_empty() {
                return Err(anyhow!("QaAnswerV1 claim is empty"));
            }
            let support = claim.support.trim().to_lowercase();
            if !support.is_empty() && !matches!(support.as_str(), "strong" | "partial" | "weak") {
                return Err(anyhow!("QaAnswerV1 claim has invalid support value"));
            }
            if claim.evidence_indices.is_empty() && !signals_insufficient(&self.answer) {
                return Err(anyhow!("QaAnswerV1 claim has no evidence_indices"));
            }
            for index in &claim.evidence_indices {
                if *index >= self.evidence.len() {
                    return Err(anyhow!(
                        "QaAnswerV1 claim references missing evidence index"
                    ));
                }
            }
            if support == "weak" && !signals_uncertainty(&self.answer, &self.uncertainty) {
                return Err(anyhow!(
                    "QaAnswerV1 weak claim must be marked uncertain in answer or uncertainty"
                ));
            }
        }
        Ok(())
    }

    pub fn render(&self) -> String {
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

pub fn signals_insufficient(answer: &str) -> bool {
    let lowered = answer.to_lowercase();
    lowered.contains("insufficient_context")
        || answer.contains("证据不足")
        || answer.contains("信息不足")
}

fn signals_uncertainty(answer: &str, uncertainty: &str) -> bool {
    if !uncertainty.trim().is_empty() {
        return true;
    }
    let lowered = answer.to_lowercase();
    lowered.contains("uncertain")
        || lowered.contains("limited evidence")
        || lowered.contains("partial support")
        || answer.contains("不确定")
        || answer.contains("证据有限")
        || answer.contains("可能")
}

fn validate_quote_or_summary(item: &QaEvidence, chunk: &SourceChunk) -> Result<()> {
    let text = item.quote_or_summary.trim();
    if text.is_empty() {
        return Ok(());
    }
    let chunk_text = normalize_text(&chunk.text);
    let evidence_text = normalize_text(text);
    if evidence_text.chars().count() >= 12 && chunk_text.contains(&evidence_text) {
        return Ok(());
    }
    for number in numeric_tokens(text) {
        if !chunk.text.contains(&number) {
            return Err(anyhow!(
                "evidence quote_or_summary contains number `{number}` not present in chunk {}",
                chunk.id
            ));
        }
    }
    let overlap = meaningful_terms(text)
        .iter()
        .filter(|term| chunk_text.contains(term.as_str()))
        .count();
    if overlap == 0 && text.chars().count() >= 12 {
        return Err(anyhow!(
            "evidence quote_or_summary is not grounded in chunk {}",
            chunk.id
        ));
    }
    Ok(())
}

fn validate_evidence_metadata(item: &QaEvidence, chunk: &SourceChunk) -> Result<()> {
    if !item.doi.trim().is_empty()
        && !chunk.doi.trim().is_empty()
        && item.doi.trim() != chunk.doi.trim()
    {
        return Err(anyhow!(
            "evidence DOI {} does not match chunk {} DOI {}",
            item.doi,
            chunk.id,
            chunk.doi
        ));
    }
    if !item.year.trim().is_empty()
        && !chunk.year.trim().is_empty()
        && item.year.trim() != chunk.year.trim()
    {
        return Err(anyhow!(
            "evidence year {} does not match chunk {} year {}",
            item.year,
            chunk.id,
            chunk.year
        ));
    }
    if !item.section.trim().is_empty()
        && !chunk.section.trim().is_empty()
        && !item
            .section
            .trim()
            .eq_ignore_ascii_case(chunk.section.trim())
    {
        return Err(anyhow!(
            "evidence section {} does not match chunk {} section {}",
            item.section,
            chunk.id,
            chunk.section
        ));
    }
    Ok(())
}

fn normalize_text(text: &str) -> String {
    text.to_lowercase()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

fn numeric_tokens(text: &str) -> Vec<String> {
    text.split(|ch: char| !ch.is_ascii_digit() && ch != '.')
        .filter(|item| item.chars().any(|ch| ch.is_ascii_digit()))
        .map(str::to_string)
        .collect()
}

fn meaningful_terms(text: &str) -> Vec<String> {
    text.split(|ch: char| !ch.is_alphanumeric())
        .map(str::trim)
        .filter(|item| item.chars().count() >= 3)
        .take(12)
        .map(|item| item.to_lowercase())
        .collect()
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::QaAnswerV1;
    use crate::storage::SourceChunk;

    fn chunk() -> SourceChunk {
        SourceChunk {
            id: 7,
            paper_key: "Alice/paper-a".to_string(),
            chunk_index: 0,
            section: "Results".to_string(),
            text: "The catalyst reached 82% conversion under mild conditions.".to_string(),
            title: "A Paper".to_string(),
            doi: "10.1/test".to_string(),
            year: "2024".to_string(),
            source_hash: "hash".to_string(),
            chunk_hash: "chunk-hash".to_string(),
            chunker_version: "section-char-v1".to_string(),
            section_kind: "body".to_string(),
            caption_label: None,
        }
    }

    #[test]
    fn rejects_invalid_claim_support() {
        let answer: QaAnswerV1 = serde_json::from_value(json!({
            "answer": "The catalyst reached 82% conversion.",
            "claims": [{
                "claim": "The catalyst reached 82% conversion.",
                "evidence_indices": [0],
                "support": "certain"
            }],
            "evidence": [{
                "paper_key": "Alice/paper-a",
                "chunk_id": 7,
                "quote_or_summary": "82% conversion"
            }]
        }))
        .unwrap();

        assert!(answer.validate(&[chunk()]).is_err());
    }

    #[test]
    fn weak_claim_requires_uncertainty_signal() {
        let answer: QaAnswerV1 = serde_json::from_value(json!({
            "answer": "The catalyst probably transfers to other conditions.",
            "claims": [{
                "claim": "The catalyst transfers to other conditions.",
                "evidence_indices": [0],
                "support": "weak"
            }],
            "evidence": [{
                "paper_key": "Alice/paper-a",
                "chunk_id": 7,
                "quote_or_summary": "mild conditions"
            }]
        }))
        .unwrap();

        assert!(answer.validate(&[chunk()]).is_err());

        let answer_with_uncertainty: QaAnswerV1 = serde_json::from_value(json!({
            "answer": "The catalyst transfer is uncertain.",
            "uncertainty": "Only one condition is covered.",
            "claims": [{
                "claim": "The catalyst transfers to other conditions.",
                "evidence_indices": [0],
                "support": "weak"
            }],
            "evidence": [{
                "paper_key": "Alice/paper-a",
                "chunk_id": 7,
                "quote_or_summary": "mild conditions"
            }]
        }))
        .unwrap();

        answer_with_uncertainty.validate(&[chunk()]).unwrap();
    }

    #[test]
    fn rejects_evidence_metadata_mismatch() {
        let answer: QaAnswerV1 = serde_json::from_value(json!({
            "answer": "The catalyst reached 82% conversion.",
            "claims": [{
                "claim": "The catalyst reached 82% conversion.",
                "evidence_indices": [0],
                "support": "strong"
            }],
            "evidence": [{
                "paper_key": "Alice/paper-a",
                "doi": "10.1/wrong",
                "year": "2024",
                "section": "Results",
                "chunk_id": 7,
                "quote_or_summary": "82% conversion"
            }]
        }))
        .unwrap();

        assert!(answer.validate(&[chunk()]).is_err());
    }
}
