use std::collections::BTreeSet;
use std::collections::HashSet;

use anyhow::Result;
use serde_json::Value;

use crate::retrieval::chunk_rank::{best_chunk_for_terms, representative_chunk};
use crate::storage::{SourceChunk, Storage};

pub(crate) fn search_profile_route(
    storage: &Storage,
    author: &str,
    terms: &[String],
    limit: usize,
) -> Result<Vec<Value>> {
    let profiles = storage.profile_route_candidates(author)?;
    Ok(rank_profiles(profiles, terms, limit))
}

pub(crate) fn profile_grounding_chunks(
    storage: &Storage,
    profiles: &[Value],
    limit: usize,
) -> Result<Vec<SourceChunk>> {
    let paper_keys = profile_paper_keys(profiles);
    profile_grounding_chunks_for_keys(storage, &paper_keys, limit)
}

pub(crate) fn profile_grounding_chunks_matching_terms(
    storage: &Storage,
    profiles: &[Value],
    terms: &[String],
    excluded_chunk_ids: &[i64],
    limit: usize,
) -> Result<Vec<SourceChunk>> {
    let paper_keys = profile_paper_keys(profiles);
    profile_grounding_chunks_for_keys_matching_terms(
        storage,
        &paper_keys,
        terms,
        excluded_chunk_ids,
        limit,
    )
}

pub(crate) fn profile_grounding_chunks_for_keys(
    storage: &Storage,
    paper_keys: &[String],
    limit: usize,
) -> Result<Vec<SourceChunk>> {
    let mut chunks = Vec::new();
    for paper_key in paper_keys {
        if chunks.len() >= limit {
            break;
        }
        let Some(chunk) =
            representative_chunk(storage.profile_grounding_chunk_candidates_for_paper(paper_key)?)
        else {
            continue;
        };
        chunks.push(chunk);
    }
    Ok(chunks)
}

pub(crate) fn profile_grounding_chunks_for_keys_matching_terms(
    storage: &Storage,
    paper_keys: &[String],
    terms: &[String],
    excluded_chunk_ids: &[i64],
    limit: usize,
) -> Result<Vec<SourceChunk>> {
    let excluded_chunk_ids = excluded_chunk_ids.iter().copied().collect::<HashSet<_>>();
    let mut chunks = Vec::new();
    for paper_key in paper_keys {
        if chunks.len() >= limit {
            break;
        }
        let paper_chunks = storage
            .profile_grounding_chunk_candidates_for_paper(paper_key)?
            .into_iter()
            .filter(|chunk| !excluded_chunk_ids.contains(&chunk.id))
            .collect::<Vec<_>>();
        let Some(chunk) = best_chunk_for_terms(paper_chunks, terms) else {
            continue;
        };
        chunks.push(chunk);
    }
    Ok(chunks)
}

fn profile_paper_keys(profiles: &[Value]) -> Vec<String> {
    profiles
        .iter()
        .filter_map(|profile| profile.get("paper_key").and_then(Value::as_str))
        .map(str::to_string)
        .collect()
}

pub fn rank_profiles(profiles: Vec<Value>, terms: &[String], limit: usize) -> Vec<Value> {
    if terms.is_empty() {
        return profiles.into_iter().take(limit).collect();
    }

    let profiles = profiles.into_iter().enumerate().collect::<Vec<_>>();
    let mut scored = Vec::new();
    for (index, profile) in &profiles {
        let score = weighted_profile_score(profile, terms);
        if score > 0 {
            scored.push((score, *index));
        }
    }
    if scored.is_empty() {
        return profiles
            .into_iter()
            .take(limit)
            .map(|(_, profile)| profile)
            .collect();
    }

    scored.sort_by(|left, right| right.0.cmp(&left.0).then_with(|| left.1.cmp(&right.1)));
    let mut used = BTreeSet::new();
    let mut ranked = Vec::new();
    for (_, index) in scored {
        if ranked.len() >= limit {
            break;
        }
        used.insert(index);
        ranked.push(profiles[index].1.clone());
    }
    for (index, profile) in profiles {
        if ranked.len() >= limit {
            break;
        }
        if used.insert(index) {
            ranked.push(profile);
        }
    }
    ranked
}

fn weighted_profile_score(profile: &Value, terms: &[String]) -> usize {
    let terms = profile_rank_terms(terms);
    [
        ("doi", 10usize),
        ("title", 5),
        ("topic_keywords", 4),
        ("key_results", 4),
        ("methods", 3),
        ("limitations", 3),
        ("one_sentence_summary", 2),
    ]
    .iter()
    .map(|(field, weight)| {
        profile
            .get(*field)
            .map(|value| value_match_count(value, &terms) * weight)
            .unwrap_or(0)
    })
    .sum::<usize>()
        + value_match_count(profile, &terms)
}

fn value_match_count(value: &Value, terms: &[String]) -> usize {
    let text = normalize_profile_text(&value_text(value));
    terms.iter().map(|term| text.matches(term).count()).sum()
}

fn profile_rank_terms(terms: &[String]) -> Vec<String> {
    let mut ranked_terms = Vec::new();
    for term in terms {
        let normalized = normalize_profile_text(term);
        if normalized.chars().count() < 3 || is_profile_stop_term(&normalized) {
            continue;
        }
        push_rank_term(&mut ranked_terms, &normalized);
        if normalized == "mof" {
            push_rank_term(&mut ranked_terms, "metal organic framework");
            push_rank_term(&mut ranked_terms, "metal organic frameworks");
            push_rank_term(&mut ranked_terms, "metal-organic framework");
            push_rank_term(&mut ranked_terms, "metal-organic frameworks");
        }
        if normalized == "oer" {
            push_rank_term(&mut ranked_terms, "oxygen evolution");
        }
        if let Some(stripped) = normalized.strip_suffix('s') {
            if stripped.chars().count() >= 4 {
                push_rank_term(&mut ranked_terms, stripped);
            }
        } else {
            push_rank_term(&mut ranked_terms, &format!("{normalized}s"));
        }
    }
    ranked_terms
}

fn push_rank_term(terms: &mut Vec<String>, term: &str) {
    if !terms.iter().any(|existing| existing == term) {
        terms.push(term.to_string());
    }
}

fn normalize_profile_text(text: &str) -> String {
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

fn is_profile_stop_term(term: &str) -> bool {
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

fn value_text(value: &Value) -> String {
    match value {
        Value::String(text) => text.clone(),
        Value::Array(items) => items.iter().map(value_text).collect::<Vec<_>>().join(" "),
        Value::Object(object) => object
            .values()
            .map(value_text)
            .collect::<Vec<_>>()
            .join(" "),
        _ => value.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::{rank_profiles, weighted_profile_score};
    use crate::retrieval::query::query_terms;

    #[test]
    fn weighted_profile_score_prioritizes_structured_fields() {
        let terms = query_terms("mof");
        let title_match = json!({ "title": "MOF catalyst" });
        let raw_match = json!({ "notes": "MOF catalyst" });

        assert!(
            weighted_profile_score(&title_match, &terms)
                > weighted_profile_score(&raw_match, &terms)
        );
    }

    #[test]
    fn rank_profiles_falls_back_to_stable_order_without_hits() {
        let ranked = rank_profiles(
            vec![
                json!({ "paper_key": "Alice/paper-a", "title": "A" }),
                json!({ "paper_key": "Alice/paper-b", "title": "B" }),
            ],
            &query_terms("unmatched"),
            1,
        );

        assert_eq!(ranked[0]["paper_key"], "Alice/paper-a");
        assert_eq!(ranked.len(), 1);
    }

    #[test]
    fn rank_profiles_prioritizes_hits_and_backfills_to_limit() {
        let ranked = rank_profiles(
            vec![
                json!({ "paper_key": "Alice/paper-a", "title": "General synthesis" }),
                json!({ "paper_key": "Alice/paper-b", "title": "MOF catalyst" }),
                json!({ "paper_key": "Alice/paper-c", "title": "Battery review" }),
            ],
            &query_terms("MOF"),
            3,
        );

        assert_eq!(ranked[0]["paper_key"], "Alice/paper-b");
        assert_eq!(ranked[1]["paper_key"], "Alice/paper-a");
        assert_eq!(ranked[2]["paper_key"], "Alice/paper-c");
    }

    #[test]
    fn rank_profiles_filters_question_words_and_expands_mof_aliases() {
        let ranked = rank_profiles(
            vec![
                json!({ "paper_key": "Alice/noisy", "title": "Which 2026 papers report general batteries" }),
                json!({ "paper_key": "Alice/mof", "title": "Metal-organic frameworks for stabilizing metal anodes in rechargeable batteries" }),
                json!({ "paper_key": "Alice/oer", "title": "Ni-MOF D-Band for Alkaline OER" }),
            ],
            &query_terms(
                "Which 2026 papers connect MOF platforms to alkaline OER and metal-anode stabilization in rechargeable batteries?",
            ),
            3,
        );

        assert_eq!(ranked[0]["paper_key"], "Alice/mof");
        assert_eq!(ranked[1]["paper_key"], "Alice/oer");
    }
}
