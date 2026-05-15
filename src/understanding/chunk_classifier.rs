use crate::storage::SourceChunk;

pub const CHUNK_CLASSIFIER_VERSION: &str = "chunk-classifier-v1";

#[derive(Debug, Clone, PartialEq)]
pub struct ChunkClassificationDecision {
    pub chunk_kind: &'static str,
    pub usefulness_score: f64,
    pub skip_reason: Option<&'static str>,
}

pub fn classify_chunk(chunk: &SourceChunk) -> ChunkClassificationDecision {
    let section = normalized(&chunk.section);
    let text = normalized(&chunk.text);
    let text_len = chunk.text.chars().filter(|ch| !ch.is_whitespace()).count();

    match chunk.section_kind.as_str() {
        "figure_caption" => return useful("figure_caption", 0.95),
        "table_caption" => return useful("table_caption", 0.95),
        _ => {}
    }

    if is_reference_section(&section) {
        return skipped("bibliography_or_reference", "reference_section");
    }
    if is_publisher_noise(&chunk.text) {
        return skipped("publisher_noise", "publisher_noise");
    }
    if text_len < 40 {
        return skipped("low_information", "very_short");
    }

    if section.contains("abstract") {
        useful("abstract", 0.95)
    } else if contains_any(
        &section,
        &["method", "experiment", "synthesis", "preparation"],
    ) {
        useful("methods", 0.95)
    } else if contains_any(&section, &["result", "finding", "performance", "benchmark"]) {
        useful("results", 0.9)
    } else if contains_any(&section, &["discussion", "analysis"]) {
        useful("discussion", 0.8)
    } else if contains_any(&section, &["conclusion", "summary"]) {
        useful("conclusion", 0.8)
    } else if contains_any(&section, &["limitation", "future work"]) {
        useful("limitation", 0.85)
    } else if contains_any(&section, &["dataset", "data set", "database", "data"]) {
        useful("dataset", 0.85)
    } else if contains_any(&section, &["introduction", "overview"]) {
        useful("introduction", 0.65)
    } else if contains_any(&section, &["background", "literature review"]) {
        useful("background", 0.55)
    } else if contains_any(&text, &["mechanism", "pathway", "active site", "vacancy"]) {
        useful("mechanism", 0.8)
    } else if contains_metric_signal(&text) {
        useful("metric", 0.8)
    } else {
        useful("unknown", 0.5)
    }
}

fn useful(chunk_kind: &'static str, usefulness_score: f64) -> ChunkClassificationDecision {
    ChunkClassificationDecision {
        chunk_kind,
        usefulness_score,
        skip_reason: None,
    }
}

fn skipped(chunk_kind: &'static str, skip_reason: &'static str) -> ChunkClassificationDecision {
    ChunkClassificationDecision {
        chunk_kind,
        usefulness_score: 0.0,
        skip_reason: Some(skip_reason),
    }
}

fn normalized(value: &str) -> String {
    value
        .to_lowercase()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

fn is_reference_section(section: &str) -> bool {
    contains_any(
        section,
        &[
            "reference",
            "references",
            "bibliography",
            "literature cited",
            "works cited",
        ],
    )
}

fn is_publisher_noise(text: &str) -> bool {
    let noise_lines = text
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .filter(|line| {
            contains_any(
                &line.to_lowercase(),
                &[
                    "download pdf",
                    "view article",
                    "rights and permissions",
                    "article metrics",
                    "related articles",
                    "sign in",
                    "subscribe",
                    "cookie",
                    "privacy policy",
                ],
            )
        })
        .count();
    noise_lines > 0 && noise_lines >= text.lines().filter(|line| !line.trim().is_empty()).count()
}

fn contains_metric_signal(text: &str) -> bool {
    text.contains('%')
        || contains_any(
            text,
            &[
                "mae",
                "rmse",
                "conversion",
                "selectivity",
                "yield",
                "accuracy",
                "capacity",
                "conductivity",
                "turnover frequency",
                "tof",
                "ph ",
            ],
        )
}

fn contains_any(value: &str, needles: &[&str]) -> bool {
    needles.iter().any(|needle| value.contains(needle))
}

#[cfg(test)]
mod tests {
    use serde::Deserialize;

    use super::classify_chunk;
    use crate::storage::SourceChunk;

    #[derive(Deserialize)]
    struct FixtureCase {
        section: String,
        section_kind: String,
        text: String,
        expected_kind: String,
        expected_skip_reason: Option<String>,
    }

    #[test]
    fn fixture_cases_classify_as_expected() {
        let cases: Vec<FixtureCase> = serde_json::from_str(include_str!(
            "../../tests/fixtures/chunk_classification.json"
        ))
        .unwrap();
        for case in cases {
            let decision = classify_chunk(&SourceChunk {
                id: 1,
                paper_key: "Alice/paper-a".to_string(),
                chunk_index: 0,
                section: case.section,
                text: case.text,
                title: "Paper".to_string(),
                doi: String::new(),
                year: "2024".to_string(),
                source_hash: "source".to_string(),
                chunk_hash: "chunk".to_string(),
                chunker_version: "section-char-v1".to_string(),
                section_kind: case.section_kind,
                caption_label: None,
            });
            assert_eq!(decision.chunk_kind, case.expected_kind);
            assert_eq!(decision.skip_reason, case.expected_skip_reason.as_deref());
        }
    }

    #[test]
    fn captions_are_useful_before_short_text_filter() {
        let decision = classify_chunk(&SourceChunk {
            id: 1,
            paper_key: "Alice/paper-a".to_string(),
            chunk_index: 0,
            section: "Figure 1 Caption".to_string(),
            text: "Figure 1: activity trend.".to_string(),
            title: "Paper".to_string(),
            doi: String::new(),
            year: "2024".to_string(),
            source_hash: "source".to_string(),
            chunk_hash: "chunk".to_string(),
            chunker_version: "section-char-v1".to_string(),
            section_kind: "figure_caption".to_string(),
            caption_label: Some("Figure 1".to_string()),
        });

        assert_eq!(decision.chunk_kind, "figure_caption");
        assert!(decision.skip_reason.is_none());
    }

    #[test]
    fn mixed_real_content_is_not_publisher_noise() {
        let decision = classify_chunk(&SourceChunk {
            id: 1,
            paper_key: "Alice/paper-a".to_string(),
            chunk_index: 0,
            section: "Results".to_string(),
            text: "The catalyst reaches 82% conversion under mild conditions.\nDownload PDF"
                .to_string(),
            title: "Paper".to_string(),
            doi: String::new(),
            year: "2024".to_string(),
            source_hash: "source".to_string(),
            chunk_hash: "chunk".to_string(),
            chunker_version: "section-char-v1".to_string(),
            section_kind: "body".to_string(),
            caption_label: None,
        });

        assert_eq!(decision.chunk_kind, "results");
        assert!(decision.skip_reason.is_none());
    }
}
