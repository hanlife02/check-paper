use serde_json::{Value, json};
use sha2::{Digest, Sha256};

use crate::storage::{ChunkClassification, SourceChunk};

pub const CHUNK_FACT_EXTRACTOR: &str = "chunk_fact_extractor";
pub const CHUNK_FACT_EXTRACTOR_VERSION: &str = "chunk-facts-v1";

#[derive(Debug, Clone, PartialEq)]
pub struct ChunkFactDraft {
    pub claim_uid: String,
    pub fact_type: &'static str,
    pub fact_json: Value,
    pub confidence: &'static str,
}

pub fn extract_chunk_fact(
    chunk: &SourceChunk,
    classification: &ChunkClassification,
) -> Option<ChunkFactDraft> {
    if classification.skip_reason.is_some() || classification.usefulness_score < 0.5 {
        return None;
    }
    let fact_type = fact_type_for_chunk_kind(&classification.chunk_kind)?;
    let claim = normalized_claim(&chunk.text);
    if claim.is_empty() {
        return None;
    }
    let confidence = confidence_for_fact_type(fact_type);
    let claim_uid = claim_uid(&chunk.paper_key, fact_type, &claim, &[chunk.chunk_index]);
    let fact_json = json!({
        "schema_version": 1,
        "claim_uid": claim_uid,
        "paper_key": chunk.paper_key,
        "chunk_id": chunk.id,
        "chunk_index": chunk.chunk_index,
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
        "chunk_kind": classification.chunk_kind,
        "fact_type": fact_type,
        "claim": claim,
        "evidence": {
            "chunk_id": chunk.id,
            "chunk_index": chunk.chunk_index,
            "source_hash": chunk.source_hash,
            "chunk_hash": chunk.chunk_hash,
        },
        "source_text": chunk.text,
    });
    Some(ChunkFactDraft {
        claim_uid,
        fact_type,
        fact_json,
        confidence,
    })
}

fn fact_type_for_chunk_kind(chunk_kind: &str) -> Option<&'static str> {
    match chunk_kind {
        "methods" => Some("method"),
        "results" => Some("result"),
        "limitation" => Some("limitation"),
        "dataset" => Some("dataset"),
        "metric" => Some("metric"),
        "mechanism" => Some("mechanism"),
        "figure_caption" => Some("figure_caption"),
        "table_caption" => Some("table_caption"),
        "abstract" | "introduction" | "background" | "discussion" | "conclusion" | "unknown" => {
            Some("context")
        }
        _ => None,
    }
}

fn confidence_for_fact_type(fact_type: &str) -> &'static str {
    match fact_type {
        "context" => "medium",
        _ => "high",
    }
}

fn normalized_claim(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn claim_uid(
    paper_key: &str,
    fact_type: &str,
    normalized_claim: &str,
    evidence_chunk_ids: &[i64],
) -> String {
    let evidence = evidence_chunk_ids
        .iter()
        .map(i64::to_string)
        .collect::<Vec<_>>()
        .join(",");
    let mut digest = Sha256::new();
    digest.update(paper_key.as_bytes());
    digest.update(b"\0");
    digest.update(fact_type.as_bytes());
    digest.update(b"\0");
    digest.update(normalized_claim.as_bytes());
    digest.update(b"\0");
    digest.update(evidence.as_bytes());
    format!("chunk-fact-v1:{:x}", digest.finalize())
}

#[cfg(test)]
mod tests {
    use super::extract_chunk_fact;
    use crate::storage::{ChunkClassification, SourceChunk};

    #[test]
    fn extracts_deterministic_fact_with_evidence_metadata() {
        let chunk = chunk("Results", "The best condition reports 82% conversion.");
        let classification = classification("results", None);

        let first = extract_chunk_fact(&chunk, &classification).unwrap();
        let second = extract_chunk_fact(&chunk, &classification).unwrap();

        assert_eq!(first.claim_uid, second.claim_uid);
        assert_eq!(first.fact_type, "result");
        assert_eq!(first.confidence, "high");
        assert_eq!(first.fact_json["evidence"]["chunk_index"], 0);
        assert_eq!(
            first.fact_json["source_text"],
            "The best condition reports 82% conversion."
        );
    }

    #[test]
    fn skips_classification_with_skip_reason() {
        let chunk = chunk("References", "Smith J. Journal of Catalysis. 2020.");
        let classification = classification("bibliography_or_reference", Some("reference_section"));

        assert!(extract_chunk_fact(&chunk, &classification).is_none());
    }

    #[test]
    fn maps_abstract_to_context_fact() {
        let chunk = chunk("Abstract", "This paper studies MOF catalysis.");
        let classification = classification("abstract", None);

        let fact = extract_chunk_fact(&chunk, &classification).unwrap();

        assert_eq!(fact.fact_type, "context");
        assert_eq!(fact.confidence, "medium");
    }

    fn chunk(section: &str, text: &str) -> SourceChunk {
        SourceChunk {
            id: 10,
            paper_key: "Alice/paper-a".to_string(),
            chunk_index: 0,
            section: section.to_string(),
            text: text.to_string(),
            title: "A Paper".to_string(),
            doi: "10.1/test".to_string(),
            year: "2024".to_string(),
            source_hash: "source".to_string(),
            chunk_hash: "chunk".to_string(),
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
        }
    }

    fn classification(chunk_kind: &str, skip_reason: Option<&str>) -> ChunkClassification {
        ChunkClassification {
            chunk_id: 10,
            paper_key: "Alice/paper-a".to_string(),
            chunk_kind: chunk_kind.to_string(),
            usefulness_score: if skip_reason.is_some() { 0.0 } else { 0.9 },
            skip_reason: skip_reason.map(str::to_string),
            classifier_version: "chunk-classifier-v1".to_string(),
            source_hash: "source".to_string(),
            chunk_hash: "chunk".to_string(),
            classified_at: "now".to_string(),
        }
    }
}
