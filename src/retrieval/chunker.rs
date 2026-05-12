use super::super::papers::models::{Paper, Section};

#[derive(Debug, Clone)]
pub struct Chunk {
    pub paper_key: String,
    pub chunk_index: usize,
    pub section: String,
    pub section_kind: String,
    pub caption_label: Option<String>,
    pub text: String,
}

pub fn chunk_paper(paper: &Paper, max_chars: usize, overlap: usize) -> Vec<Chunk> {
    let fallback;
    let sections = if paper.sections.is_empty() {
        fallback = vec![Section {
            title: "Body".to_string(),
            level: 1,
            content: paper.clean_text.clone(),
        }];
        &fallback
    } else {
        &paper.sections
    };

    let mut chunks = Vec::new();
    for section in sections {
        for piece in split_text(&section.content, max_chars, overlap) {
            chunks.push(Chunk {
                paper_key: paper.key(),
                chunk_index: chunks.len(),
                section: section.title.clone(),
                section_kind: section.section_kind().to_string(),
                caption_label: section.caption_label(),
                text: piece,
            });
        }
    }
    chunks
}

pub fn split_text(text: &str, max_chars: usize, overlap: usize) -> Vec<String> {
    let normalized = text.trim();
    if normalized.is_empty() {
        return Vec::new();
    }
    if normalized.chars().count() <= max_chars {
        return vec![normalized.to_string()];
    }

    let chars: Vec<char> = normalized.chars().collect();
    let mut pieces = Vec::new();
    let mut start = 0usize;
    while start < chars.len() {
        let mut end = (start + max_chars).min(chars.len());
        if end < chars.len() {
            let lower_bound = start + max_chars / 2;
            for index in (lower_bound..end).rev() {
                if chars[index] == '\n' || chars[index] == '。' || chars[index] == '.' {
                    end = index + 1;
                    break;
                }
            }
        }
        let piece: String = chars[start..end].iter().collect();
        if !piece.trim().is_empty() {
            pieces.push(piece.trim().to_string());
        }
        if end >= chars.len() {
            break;
        }
        start = end.saturating_sub(overlap).max(start + 1);
    }
    pieces
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn splits_long_text() {
        let chunks = split_text(&"A".repeat(1000), 300, 50);
        assert!(chunks.len() > 1);
        assert!(chunks.iter().all(|chunk| chunk.chars().count() <= 300));
    }

    #[test]
    fn carries_caption_metadata_into_chunks() {
        let paper = Paper {
            author: "Alice".to_string(),
            paper_id: "paper-a".to_string(),
            paper_dir: std::path::PathBuf::new(),
            article_path: std::path::PathBuf::new(),
            fetch_result_path: None,
            source_hash: "hash".to_string(),
            metadata: Default::default(),
            fetch_result: serde_json::json!({}),
            raw_body: String::new(),
            clean_text: String::new(),
            sections: vec![Section {
                title: "Figure 2 Caption".to_string(),
                level: 2,
                content: "Figure 2: conversion trend".to_string(),
            }],
        };

        let chunks = chunk_paper(&paper, 3200, 350);

        assert_eq!(chunks[0].section_kind, "figure_caption");
        assert_eq!(chunks[0].caption_label.as_deref(), Some("Figure 2"));
    }
}
