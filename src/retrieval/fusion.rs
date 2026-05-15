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
    let merged = rrf_merge_chunks(ranked_lists, limit);
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

#[cfg(test)]
mod tests {
    use super::{RankedChunk, retrieval_trace, rrf_merge_chunks, unscored_route};
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
