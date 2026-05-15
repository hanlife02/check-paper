use anyhow::Result;

use crate::retrieval::embedding::{
    LOCAL_HASH_EMBEDDING_MODEL, local_hash_embedding, rank_vector_chunks_with_scores,
};
use crate::retrieval::fusion::RankedChunk;
use crate::storage::Storage;

pub(crate) fn search_local_hash_route_with_scores(
    storage: &Storage,
    author: &str,
    query: &str,
    limit: usize,
) -> Result<Vec<RankedChunk>> {
    let query_vector = local_hash_embedding(query);
    let rows = storage.chunk_embeddings_for_model(author, LOCAL_HASH_EMBEDDING_MODEL, None)?;
    Ok(
        rank_vector_chunks_with_scores(rows, &query_vector, limit, false)
            .into_iter()
            .map(|(chunk, score)| RankedChunk::scored(chunk, score))
            .collect(),
    )
}

pub(crate) fn search_dense_route_with_scores(
    storage: &Storage,
    author: &str,
    model: &str,
    model_version: Option<&str>,
    query_vector: &[f32],
    limit: usize,
) -> Result<Vec<RankedChunk>> {
    let rows = storage.chunk_embeddings_for_model(author, model, model_version)?;
    Ok(
        rank_vector_chunks_with_scores(rows, query_vector, limit, true)
            .into_iter()
            .map(|(chunk, score)| RankedChunk::scored(chunk, score))
            .collect(),
    )
}
