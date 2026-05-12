use std::collections::{HashMap, HashSet};

use serde_json::{Value, json};

use crate::storage::SourceChunk;

pub type ChunkRoute = (&'static str, Vec<SourceChunk>);

pub fn rrf_merge_chunks(ranked_lists: Vec<Vec<SourceChunk>>, limit: usize) -> Vec<SourceChunk> {
    const RRF_K: f64 = 60.0;
    let mut scores: HashMap<i64, f64> = HashMap::new();
    let mut first_seen: HashMap<i64, SourceChunk> = HashMap::new();
    for list in ranked_lists {
        let mut seen_in_list = HashSet::new();
        for (rank, chunk) in list.into_iter().enumerate() {
            if !seen_in_list.insert(chunk.id) {
                continue;
            }
            first_seen.entry(chunk.id).or_insert_with(|| chunk.clone());
            *scores.entry(chunk.id).or_default() += 1.0 / (RRF_K + rank as f64 + 1.0);
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
        .map(|(_, rows)| rows.clone())
        .collect::<Vec<_>>();
    let merged = rrf_merge_chunks(ranked_lists, limit);
    let trace = retrieval_trace(ranked_routes, &merged);
    (merged, trace)
}

pub fn empty_retrieval_trace() -> Value {
    json!({ "routes": {}, "fusion": [] })
}

pub fn retrieval_trace(ranked_routes: &[ChunkRoute], merged: &[SourceChunk]) -> Value {
    let routes = ranked_routes
        .iter()
        .map(|(route, chunks)| {
            let candidates = chunks
                .iter()
                .enumerate()
                .map(|(rank, chunk)| {
                    json!({
                        "rank": rank + 1,
                        "chunk_id": chunk.id,
                        "paper_key": chunk.paper_key,
                        "chunk_index": chunk.chunk_index,
                        "section": chunk.section,
                        "section_kind": chunk.section_kind,
                        "caption_label": chunk.caption_label,
                    })
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

#[cfg(test)]
mod tests {
    use super::rrf_merge_chunks;
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
}
