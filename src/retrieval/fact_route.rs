use anyhow::Result;

use crate::storage::{FactRouteCandidate, SourceChunk, Storage};

pub(crate) fn search_fact_route(
    storage: &Storage,
    author: &str,
    terms: &[String],
    limit: usize,
) -> Result<Vec<SourceChunk>> {
    let candidates = storage.fact_route_candidates(author)?;
    Ok(rank_fact_chunks(candidates, terms, limit))
}

pub fn fact_type_intents(terms: &[String]) -> Vec<String> {
    let query = terms.join(" ").to_lowercase();
    let mut types = Vec::new();
    for (fact_type, needles) in [
        (
            "method",
            &["方法", "怎么做", "method", "synthesis", "characterization"][..],
        ),
        (
            "result",
            &["结果", "性能", "提升", "conversion", "capacity", "result"][..],
        ),
        ("limitation", &["局限", "不足", "limitation", "limits"][..]),
        ("dataset", &["数据集", "数据", "dataset", "data"][..]),
        ("metric", &["指标", "metric", "accuracy", "efficiency"][..]),
        (
            "figure_caption",
            &["图", "figure", "fig", "fig.", "caption"][..],
        ),
        ("table_caption", &["表", "table", "caption"][..]),
        (
            "experiment_condition",
            &["实验条件", "temperature", "pressure", "condition"][..],
        ),
    ] {
        if needles
            .iter()
            .any(|needle| query_matches_needle(&query, needle))
        {
            types.push(fact_type.to_string());
        }
    }
    types
}

fn query_matches_needle(query: &str, needle: &str) -> bool {
    if needle
        .chars()
        .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '-' | '_'))
    {
        return query
            .split(|ch: char| !ch.is_ascii_alphanumeric() && !matches!(ch, '-' | '_'))
            .any(|token| token == needle || token.strip_suffix('s') == Some(needle));
    }
    query.contains(needle)
}

pub(crate) fn rank_fact_chunks(
    candidates: Vec<FactRouteCandidate>,
    terms: &[String],
    limit: usize,
) -> Vec<SourceChunk> {
    let intent_types = fact_type_intents(terms);
    let mut scored = Vec::new();
    for candidate in candidates {
        if !intent_types.is_empty() && !intent_types.iter().any(|item| item == &candidate.fact_type)
        {
            continue;
        }
        let blob = format!(
            "{} {} {} {} {} {}",
            candidate.fact_type,
            candidate.fact_json,
            candidate.chunk.title,
            candidate.chunk.doi,
            candidate.chunk.section,
            candidate.chunk.text
        )
        .to_lowercase();
        let score: usize = terms
            .iter()
            .map(|term| blob.matches(&term.to_lowercase()).count())
            .sum::<usize>()
            + if intent_types.iter().any(|item| item == &candidate.fact_type) {
                3
            } else {
                0
            };
        if score > 0 {
            scored.push((score, candidate.chunk));
        }
    }
    scored.sort_by(|left, right| {
        right
            .0
            .cmp(&left.0)
            .then_with(|| left.1.id.cmp(&right.1.id))
    });
    scored
        .into_iter()
        .take(limit)
        .map(|(_, chunk)| chunk)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::{FactRouteCandidate, fact_type_intents, rank_fact_chunks};
    use crate::storage::SourceChunk;

    fn chunk(id: i64) -> SourceChunk {
        SourceChunk {
            id,
            paper_key: format!("Alice/paper-{id}"),
            chunk_index: 0,
            section: "Results".to_string(),
            text: "reported capacity and efficiency".to_string(),
            title: "Battery paper".to_string(),
            doi: String::new(),
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
        }
    }

    #[test]
    fn detects_common_fact_intents() {
        assert!(fact_type_intents(&["有哪些局限".to_string()]).contains(&"limitation".to_string()));
        assert!(
            fact_type_intents(&["temperature condition".to_string()])
                .contains(&"experiment_condition".to_string())
        );
        assert!(
            fact_type_intents(&["Figure 1".to_string()]).contains(&"figure_caption".to_string())
        );
        assert!(
            fact_type_intents(&["Table S1".to_string()]).contains(&"table_caption".to_string())
        );
        assert!(
            !fact_type_intents(&["stable catalyst".to_string()])
                .contains(&"table_caption".to_string())
        );
    }

    #[test]
    fn rank_fact_chunks_prefers_matching_intent_type() {
        let ranked = rank_fact_chunks(
            vec![
                FactRouteCandidate {
                    chunk: chunk(1),
                    fact_type: "method".to_string(),
                    fact_json: r#"{"text":"synthesis"}"#.to_string(),
                },
                FactRouteCandidate {
                    chunk: chunk(2),
                    fact_type: "metric".to_string(),
                    fact_json: r#"{"text":"efficiency reached 99%"}"#.to_string(),
                },
            ],
            &["metric".to_string(), "efficiency".to_string()],
            5,
        );

        assert_eq!(ranked[0].id, 2);
    }
}
