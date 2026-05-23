use anyhow::Result;

use crate::storage::{SourceChunk, Storage};

pub(crate) fn search_fts_route(
    storage: &Storage,
    author: &str,
    terms: &[String],
    limit: usize,
) -> Result<Vec<SourceChunk>> {
    let match_query = fts_match_query(terms);
    if match_query.is_empty() {
        return Ok(Vec::new());
    }
    storage.fts_route_candidates(author, &match_query, limit)
}

pub fn fts_match_query(terms: &[String]) -> String {
    terms
        .iter()
        .take(12)
        .map(|term| format!("\"{}\"", term.replace('"', "\"\"")))
        .collect::<Vec<_>>()
        .join(" OR ")
}

#[cfg(test)]
mod tests {
    use super::fts_match_query;

    #[test]
    fn fts_match_query_quotes_terms() {
        assert_eq!(
            fts_match_query(&["zeolite".to_string(), "mof".to_string()]),
            "\"zeolite\" OR \"mof\""
        );
    }
}
