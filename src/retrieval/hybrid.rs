use anyhow::Result;
use serde_json::Value;

use crate::retrieval::dense_route::{
    search_dense_route_with_scores, search_local_hash_route_with_scores,
};
use crate::retrieval::fact_route::search_fact_route;
use crate::retrieval::fts_route::search_fts_route;
use crate::retrieval::fusion::{
    ChunkRoute, empty_retrieval_trace, merge_chunk_routes, unscored_route,
};
use crate::retrieval::like_route::search_like_route;
use crate::retrieval::profile_route::search_profile_route;
use crate::retrieval::query::query_terms;
use crate::storage::{SourceChunk, Storage};

pub(crate) fn search_chunks_with_trace(
    storage: &Storage,
    author: &str,
    query: &str,
    limit: usize,
) -> Result<(Vec<SourceChunk>, Value)> {
    let terms = query_terms(query);
    if terms.is_empty() {
        return Ok((Vec::new(), empty_retrieval_trace()));
    }
    let mut ranked_routes = base_routes(storage, author, query, &terms)?;
    let embedding_rows = search_local_hash_route_with_scores(storage, author, query, 30)?;
    if !embedding_rows.is_empty() {
        ranked_routes.push(("local_embedding", embedding_rows));
    }
    add_profile_route(storage, author, &terms, &mut ranked_routes)?;
    Ok(merge_chunk_routes(&ranked_routes, limit))
}

pub(crate) fn search_chunks_with_dense_vector_trace(
    storage: &Storage,
    author: &str,
    query: &str,
    limit: usize,
    model: &str,
    model_version: Option<&str>,
    query_vector: &[f32],
) -> Result<(Vec<SourceChunk>, Value)> {
    let terms = query_terms(query);
    if terms.is_empty() {
        return Ok((Vec::new(), empty_retrieval_trace()));
    }
    let mut ranked_routes = base_routes(storage, author, query, &terms)?;
    let dense_rows =
        search_dense_route_with_scores(storage, author, model, model_version, query_vector, 30)?;
    if !dense_rows.is_empty() {
        ranked_routes.push(("dense", dense_rows));
    }
    add_profile_route(storage, author, &terms, &mut ranked_routes)?;
    Ok(merge_chunk_routes(&ranked_routes, limit))
}

fn base_routes(
    storage: &Storage,
    author: &str,
    query: &str,
    terms: &[String],
) -> Result<Vec<ChunkRoute>> {
    let mut ranked_routes = Vec::new();
    if let Ok(rows) = search_fts_route(storage, author, terms, 30) {
        if !rows.is_empty() {
            ranked_routes.push(unscored_route("fts", rows));
        }
    }
    let like_rows = search_like_route(storage, author, terms, 30)?;
    if !like_rows.is_empty() {
        ranked_routes.push(unscored_route("like", like_rows));
    }
    let fact_rows = search_fact_route(storage, author, terms, 30)?;
    if !fact_rows.is_empty() {
        ranked_routes.push(unscored_route("fact", fact_rows));
    }
    if ranked_routes.is_empty() && query.trim().is_empty() {
        return Ok(Vec::new());
    }
    Ok(ranked_routes)
}

fn add_profile_route(
    storage: &Storage,
    author: &str,
    terms: &[String],
    ranked_routes: &mut Vec<ChunkRoute>,
) -> Result<()> {
    let profiles = search_profile_route(storage, author, terms, 20)?;
    let paper_keys = profiles
        .iter()
        .filter_map(|profile| profile.get("paper_key").and_then(Value::as_str))
        .map(str::to_string)
        .collect::<Vec<_>>();
    let profile_rows = storage.chunks_for_paper_keys(&paper_keys, 20)?;
    if !profile_rows.is_empty() {
        ranked_routes.push(unscored_route("profile", profile_rows));
    }
    Ok(())
}
