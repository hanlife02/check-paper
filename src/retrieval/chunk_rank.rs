use crate::storage::SourceChunk;

pub(crate) fn best_chunk_for_terms(
    chunks: Vec<SourceChunk>,
    terms: &[String],
) -> Option<SourceChunk> {
    let terms = chunk_rank_terms(terms);
    chunks.into_iter().max_by(|left, right| {
        chunk_match_score(left, &terms)
            .cmp(&chunk_match_score(right, &terms))
            .then_with(|| fallback_chunk_rank(right).cmp(&fallback_chunk_rank(left)))
            .then_with(|| right.chunk_index.cmp(&left.chunk_index))
    })
}

pub(crate) fn representative_chunk(chunks: Vec<SourceChunk>) -> Option<SourceChunk> {
    chunks.into_iter().min_by(|left, right| {
        representative_chunk_rank(left)
            .cmp(&representative_chunk_rank(right))
            .then_with(|| left.chunk_index.cmp(&right.chunk_index))
    })
}

fn chunk_match_score(chunk: &SourceChunk, terms: &[String]) -> usize {
    let text =
        normalize_chunk_rank_text(&format!("{} {} {}", chunk.title, chunk.section, chunk.text));
    let score = terms
        .iter()
        .map(|term| text.matches(term).count() * chunk_rank_term_weight(term))
        .sum::<usize>();
    if is_reference_like_chunk(&text) {
        score / 8
    } else {
        score
    }
}

fn chunk_rank_term_weight(term: &str) -> usize {
    if term.split_whitespace().count() >= 2 {
        4
    } else {
        1
    }
}

fn fallback_chunk_rank(chunk: &SourceChunk) -> usize {
    let section_text = normalize_chunk_rank_text(&format!("{} {}", chunk.section, chunk.text));
    if chunk.section_kind != "figure_caption"
        && chunk.section_kind != "table_caption"
        && chunk.chunk_index > 0
        && section_text.contains("abstract")
    {
        0
    } else if chunk.section_kind != "figure_caption"
        && chunk.section_kind != "table_caption"
        && chunk.chunk_index > 0
        && section_text.contains("introduction")
    {
        1
    } else if chunk.section_kind != "figure_caption"
        && chunk.section_kind != "table_caption"
        && chunk.chunk_index > 0
    {
        2
    } else if chunk.section_kind != "figure_caption" && chunk.section_kind != "table_caption" {
        3
    } else {
        4
    }
}

fn representative_chunk_rank(chunk: &SourceChunk) -> usize {
    let section_text = normalize_chunk_rank_text(&format!("{} {}", chunk.section, chunk.text));
    if chunk.section_kind != "figure_caption"
        && chunk.section_kind != "table_caption"
        && chunk.chunk_index > 0
        && section_text.contains("abstract")
    {
        0
    } else if chunk.section_kind != "figure_caption"
        && chunk.section_kind != "table_caption"
        && chunk.chunk_index > 0
        && section_text.contains("introduction")
    {
        1
    } else if chunk.section_kind != "figure_caption"
        && chunk.section_kind != "table_caption"
        && chunk.chunk_index > 0
    {
        2
    } else if chunk.section_kind != "figure_caption"
        && chunk.section_kind != "table_caption"
        && section_text.contains("abstract")
    {
        3
    } else if chunk.section_kind != "figure_caption" && chunk.section_kind != "table_caption" {
        4
    } else {
        5
    }
}

fn chunk_rank_terms(terms: &[String]) -> Vec<String> {
    let mut ranked_terms = Vec::new();
    for term in terms {
        let normalized = normalize_chunk_rank_text(term);
        if normalized.chars().count() < 3 || is_chunk_rank_stop_term(&normalized) {
            continue;
        }
        push_chunk_rank_term(&mut ranked_terms, &normalized);
        if normalized == "mof" {
            push_chunk_rank_term(&mut ranked_terms, "metal organic framework");
            push_chunk_rank_term(&mut ranked_terms, "metal organic frameworks");
        }
        if normalized == "oer" {
            push_chunk_rank_term(&mut ranked_terms, "oxygen evolution");
        }
        if matches!(
            normalized.as_str(),
            "wearable" | "thermoregulation" | "phase change" | "phase change materials"
        ) {
            push_chunk_rank_term(&mut ranked_terms, "smart garments");
            push_chunk_rank_term(&mut ranked_terms, "smart garment");
            push_chunk_rank_term(&mut ranked_terms, "phase change fabrics");
            push_chunk_rank_term(&mut ranked_terms, "phase change garment");
        }
        if let Some(stripped) = normalized.strip_suffix('s') {
            if stripped.chars().count() >= 4 {
                push_chunk_rank_term(&mut ranked_terms, stripped);
            }
        } else {
            push_chunk_rank_term(&mut ranked_terms, &format!("{normalized}s"));
        }
    }
    ranked_terms
}

fn push_chunk_rank_term(terms: &mut Vec<String>, term: &str) {
    if !terms.iter().any(|existing| existing == term) {
        terms.push(term.to_string());
    }
}

fn normalize_chunk_rank_text(text: &str) -> String {
    text.to_lowercase()
        .chars()
        .map(|ch| match ch {
            '-' | '‐' | '‑' | '–' | '—' | '/' | '_' => ' ',
            _ => ch,
        })
        .collect::<String>()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

fn is_chunk_rank_stop_term(term: &str) -> bool {
    matches!(
        term,
        "the"
            | "and"
            | "for"
            | "with"
            | "from"
            | "into"
            | "what"
            | "which"
            | "paper"
            | "papers"
            | "review"
            | "perspective"
            | "reports"
            | "reported"
            | "uses"
            | "use"
            | "cover"
            | "covers"
            | "connect"
            | "connects"
            | "about"
            | "does"
            | "how"
            | "2026"
    )
}

fn is_reference_like_chunk(text: &str) -> bool {
    text.matches("google scholar").count() >= 2
        || text.matches("pubmed").count() >= 2
        || text.matches("article cas").count() >= 2
}

#[cfg(test)]
mod tests {
    use super::{best_chunk_for_terms, representative_chunk};
    use crate::storage::SourceChunk;

    #[test]
    fn best_chunk_for_terms_prefers_relevant_non_reference_body() {
        let chunks = vec![
            chunk(
                1,
                0,
                "References",
                "Google Scholar PubMed Article CAS metal organic frameworks smart garments",
            ),
            chunk(
                2,
                7,
                "Results",
                "The phase change fabrics support thermoregulation in smart garments.",
            ),
            chunk(
                3,
                1,
                "Abstract",
                "General overview without the target term.",
            ),
        ];

        let best = best_chunk_for_terms(
            chunks,
            &["wearable".to_string(), "thermoregulation".to_string()],
        )
        .unwrap();

        assert_eq!(best.id, 2);
    }

    #[test]
    fn representative_chunk_prefers_body_context_over_captions_and_metadata() {
        let best = representative_chunk(vec![
            chunk_with_kind(1, 0, "Title", "Metadata only.", "body"),
            chunk_with_kind(
                2,
                8,
                "Figure 1 Caption",
                "Abstract graphical preview.",
                "figure_caption",
            ),
            chunk_with_kind(3, 2, "Abstract", "Real abstract body context.", "body"),
            chunk_with_kind(4, 3, "Introduction", "Introductory context.", "body"),
        ])
        .unwrap();

        assert_eq!(best.id, 3);
    }

    fn chunk(id: i64, chunk_index: i64, section: &str, text: &str) -> SourceChunk {
        chunk_with_kind(id, chunk_index, section, text, "body")
    }

    fn chunk_with_kind(
        id: i64,
        chunk_index: i64,
        section: &str,
        text: &str,
        section_kind: &str,
    ) -> SourceChunk {
        SourceChunk {
            id,
            paper_key: "Alice/paper-a".to_string(),
            chunk_index,
            section: section.to_string(),
            text: text.to_string(),
            title: "A Paper".to_string(),
            doi: "10.1/test".to_string(),
            year: "2026".to_string(),
            source_hash: "source".to_string(),
            chunk_hash: "chunk".to_string(),
            chunker_version: "section-char-v1".to_string(),
            section_kind: section_kind.to_string(),
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
}
