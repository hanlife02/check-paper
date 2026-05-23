use std::collections::{HashMap, HashSet};

use serde_json::{Value, json};

use crate::storage::SourceChunk;

pub type ChunkRoute = (&'static str, Vec<RankedChunk>);

#[derive(Debug, Clone)]
pub struct RankedChunk {
    pub chunk: SourceChunk,
    pub route_score: Option<f64>,
}

impl RankedChunk {
    pub fn unscored(chunk: SourceChunk) -> Self {
        Self {
            chunk,
            route_score: None,
        }
    }

    pub fn scored(chunk: SourceChunk, route_score: f64) -> Self {
        Self {
            chunk,
            route_score: Some(route_score),
        }
    }
}

const RRF_K: f64 = 60.0;
const LEXICAL_SEED_ROUTE: &str = "fts";
const LEXICAL_SEED_LIMIT: usize = 3;

pub fn rrf_merge_chunks(ranked_lists: Vec<Vec<SourceChunk>>, limit: usize) -> Vec<SourceChunk> {
    let mut scores: HashMap<i64, f64> = HashMap::new();
    let mut first_seen: HashMap<i64, SourceChunk> = HashMap::new();
    for list in ranked_lists {
        let mut seen_in_list = HashSet::new();
        for (rank, chunk) in list.into_iter().enumerate() {
            if !seen_in_list.insert(chunk.id) {
                continue;
            }
            first_seen.entry(chunk.id).or_insert_with(|| chunk.clone());
            *scores.entry(chunk.id).or_default() += rrf_rank_score(rank);
        }
    }
    let mut scored = scores.into_iter().collect::<Vec<_>>();
    scored.sort_by(|left, right| {
        right
            .1
            .partial_cmp(&left.1)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| left.0.cmp(&right.0))
    });
    scored
        .into_iter()
        .take(limit)
        .filter_map(|(id, _)| first_seen.remove(&id))
        .collect()
}

pub fn merge_chunk_routes(ranked_routes: &[ChunkRoute], limit: usize) -> (Vec<SourceChunk>, Value) {
    if ranked_routes.is_empty() {
        return (Vec::new(), empty_retrieval_trace());
    }
    let ranked_lists = ranked_routes
        .iter()
        .map(|(_, rows)| {
            rows.iter()
                .map(|candidate| candidate.chunk.clone())
                .collect()
        })
        .collect::<Vec<_>>();
    let merged =
        preserve_lexical_seed_chunks(ranked_routes, rrf_merge_chunks(ranked_lists, limit), limit);
    let trace = retrieval_trace(ranked_routes, &merged);
    (merged, trace)
}

pub fn unscored_route(route: &'static str, chunks: Vec<SourceChunk>) -> ChunkRoute {
    (
        route,
        chunks.into_iter().map(RankedChunk::unscored).collect(),
    )
}

pub fn empty_retrieval_trace() -> Value {
    json!({ "routes": {}, "fusion": [] })
}

pub fn retrieval_trace(ranked_routes: &[ChunkRoute], merged: &[SourceChunk]) -> Value {
    let fusion_scores = rrf_scores(ranked_routes);
    let routes = ranked_routes
        .iter()
        .map(|(route, chunks)| {
            let candidates = chunks
                .iter()
                .enumerate()
                .map(|(rank, chunk)| {
                    let mut candidate = json!({
                        "rank": rank + 1,
                        "score": rrf_rank_score(rank),
                        "chunk_id": chunk.chunk.id,
                        "paper_key": chunk.chunk.paper_key,
                        "chunk_index": chunk.chunk.chunk_index,
                        "section": chunk.chunk.section,
                        "section_kind": chunk.chunk.section_kind,
                        "caption_label": chunk.chunk.caption_label,
                        "caption_object_type": chunk.chunk.caption_object_type,
                        "caption_object_label": chunk.chunk.caption_object_label,
                        "caption_panel_labels": chunk.chunk.caption_panel_labels_value(),
                        "caption_target_labels": chunk.chunk.caption_target_labels_value(),
                        "caption_panel_details": chunk.chunk.caption_panel_details_value(),
                        "caption_measurements": chunk.chunk.caption_measurements_value(),
                        "caption_conditions": chunk.chunk.caption_conditions_value(),
                        "caption_values": chunk.chunk.caption_values_value(),
                    });
                    if let Some(route_score) = chunk.route_score {
                        candidate["route_score"] = json!(route_score);
                    }
                    candidate
                })
                .collect::<Vec<_>>();
            ((*route).to_string(), Value::Array(candidates))
        })
        .collect::<serde_json::Map<_, _>>();
    let fusion = merged
        .iter()
        .enumerate()
        .map(|(rank, chunk)| {
            json!({
                "rank": rank + 1,
                "score": fusion_scores.get(&chunk.id).copied().unwrap_or_default(),
                "chunk_id": chunk.id,
                "paper_key": chunk.paper_key,
                "chunk_index": chunk.chunk_index,
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
            })
        })
        .collect::<Vec<_>>();
    json!({
        "routes": routes,
        "fusion": fusion,
    })
}

fn rrf_scores(ranked_routes: &[ChunkRoute]) -> HashMap<i64, f64> {
    let mut scores: HashMap<i64, f64> = HashMap::new();
    for (_, chunks) in ranked_routes {
        let mut seen_in_list = HashSet::new();
        for (rank, chunk) in chunks.iter().enumerate() {
            if !seen_in_list.insert(chunk.chunk.id) {
                continue;
            }
            *scores.entry(chunk.chunk.id).or_default() += rrf_rank_score(rank);
        }
    }
    scores
}

fn rrf_rank_score(rank: usize) -> f64 {
    1.0 / (RRF_K + rank as f64 + 1.0)
}

fn preserve_lexical_seed_chunks(
    ranked_routes: &[ChunkRoute],
    merged: Vec<SourceChunk>,
    limit: usize,
) -> Vec<SourceChunk> {
    if limit == 0 {
        return Vec::new();
    }

    let seeds = ranked_routes
        .iter()
        .filter(|(route, _)| *route == LEXICAL_SEED_ROUTE)
        .flat_map(|(_, chunks)| chunks.iter().map(|candidate| candidate.chunk.clone()))
        .take(LEXICAL_SEED_LIMIT)
        .collect::<Vec<_>>();
    if seeds.is_empty() {
        return merged;
    }

    let mut seeded = Vec::with_capacity(limit);
    let mut seen_ids = HashSet::new();
    for seed in seeds {
        if seen_ids.contains(&seed.id) {
            continue;
        }
        seen_ids.insert(seed.id);
        seeded.push(seed);
        if seeded.len() >= limit {
            return seeded;
        }
    }

    for chunk in merged {
        if seen_ids.contains(&chunk.id) {
            continue;
        }
        seen_ids.insert(chunk.id);
        seeded.push(chunk);
        if seeded.len() >= limit {
            break;
        }
    }
    seeded
}

#[cfg(test)]
mod tests {
    use super::{
        RankedChunk, merge_chunk_routes, retrieval_trace, rrf_merge_chunks, unscored_route,
    };
    use crate::storage::SourceChunk;

    #[test]
    fn rrf_merge_promotes_chunks_seen_by_multiple_rankers() {
        let chunk = |id| SourceChunk {
            id,
            paper_key: format!("Alice/paper-{id}"),
            chunk_index: 0,
            section: "Results".to_string(),
            text: format!("chunk {id}"),
            title: format!("Paper {id}"),
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
        };
        let merged = rrf_merge_chunks(
            vec![
                vec![chunk(1), chunk(2), chunk(3)],
                vec![chunk(2), chunk(4), chunk(5)],
            ],
            3,
        );

        assert_eq!(merged[0].id, 2);
        assert_eq!(merged.len(), 3);
    }

    #[test]
    fn retrieval_trace_includes_route_and_fusion_scores() {
        let chunk = |id| SourceChunk {
            id,
            paper_key: format!("Alice/paper-{id}"),
            chunk_index: 0,
            section: "Results".to_string(),
            text: format!("chunk {id}"),
            title: format!("Paper {id}"),
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
        };
        let first = chunk(1);
        let second = chunk(2);
        let trace = retrieval_trace(
            &[
                unscored_route("fts", vec![first.clone(), second.clone()]),
                unscored_route("like", vec![first.clone()]),
            ],
            &[first, second],
        );

        let route_score = trace["routes"]["fts"][0]["score"].as_f64().unwrap();
        let first_score = trace["fusion"][0]["score"].as_f64().unwrap();
        let second_score = trace["fusion"][1]["score"].as_f64().unwrap();

        assert!(route_score > 0.0);
        assert!(first_score > second_score);
    }

    #[test]
    fn merge_chunk_routes_preserves_top_fts_candidates() {
        let chunk = |id| SourceChunk {
            id,
            paper_key: format!("Alice/paper-{id}"),
            chunk_index: 0,
            section: "Results".to_string(),
            text: format!("chunk {id}"),
            title: format!("Paper {id}"),
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
        };
        let exact = chunk(1);
        let repeated = chunk(2);
        let filler = chunk(3);
        let (merged, _) = merge_chunk_routes(
            &[
                unscored_route("fts", vec![exact.clone(), filler.clone()]),
                unscored_route("like", vec![repeated.clone(), filler.clone()]),
                unscored_route("local_embedding", vec![repeated]),
            ],
            2,
        );

        assert!(merged.iter().any(|chunk| chunk.id == exact.id));
        assert_eq!(merged[0].id, exact.id);
    }

    #[test]
    fn retrieval_trace_includes_optional_route_similarity_score() {
        let chunk = SourceChunk {
            id: 1,
            paper_key: "Alice/paper-a".to_string(),
            chunk_index: 0,
            section: "Results".to_string(),
            text: "chunk".to_string(),
            title: "Paper".to_string(),
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
        };
        let trace = retrieval_trace(
            &[("dense", vec![RankedChunk::scored(chunk.clone(), 0.42)])],
            &[chunk],
        );

        assert_eq!(
            trace["routes"]["dense"][0]["route_score"].as_f64().unwrap(),
            0.42
        );
    }
}
