use anyhow::Result;

use crate::storage::SourceChunk;
use crate::storage::Storage;

pub(crate) fn search_like_route(
    storage: &Storage,
    author: &str,
    terms: &[String],
    limit: usize,
) -> Result<Vec<SourceChunk>> {
    let chunks = storage.all_chunks_for_author(author, None)?;
    Ok(rank_like_chunks(chunks, terms, limit))
}

pub fn rank_like_chunks(
    chunks: Vec<SourceChunk>,
    terms: &[String],
    limit: usize,
) -> Vec<SourceChunk> {
    let mut scored = Vec::new();
    for chunk in chunks {
        let blob = format!(
            "{} {} {} {}",
            chunk.title, chunk.doi, chunk.section, chunk.text
        )
        .to_lowercase();
        let score: usize = terms
            .iter()
            .map(|term| blob.matches(&term.to_lowercase()).count())
            .sum();
        if score > 0 {
            scored.push((score, chunk));
        }
    }
    scored.sort_by(|left, right| right.0.cmp(&left.0));
    scored
        .into_iter()
        .take(limit)
        .map(|(_, chunk)| chunk)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::rank_like_chunks;
    use crate::storage::SourceChunk;

    fn chunk(id: i64, text: &str) -> SourceChunk {
        SourceChunk {
            id,
            paper_key: format!("Alice/paper-{id}"),
            chunk_index: 0,
            section: "Results".to_string(),
            text: text.to_string(),
            title: "Paper".to_string(),
            doi: String::new(),
            year: "2024".to_string(),
            source_hash: "hash".to_string(),
            chunk_hash: "chunk-hash".to_string(),
            chunker_version: "section-char-v1".to_string(),
            section_kind: "body".to_string(),
            caption_label: None,
        }
    }

    #[test]
    fn rank_like_chunks_orders_by_term_frequency() {
        let ranked = rank_like_chunks(
            vec![
                chunk(1, "conversion"),
                chunk(2, "conversion conversion catalyst"),
            ],
            &["conversion".to_string(), "catalyst".to_string()],
            5,
        );

        assert_eq!(ranked[0].id, 2);
    }
}
