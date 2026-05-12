use anyhow::Result;
use serde_json::Value;

use crate::retrieval::query::query_terms;
use crate::storage::Storage;

pub(crate) fn search_profiles_for_query(
    storage: &Storage,
    author: &str,
    query: &str,
    limit: usize,
) -> Result<Vec<Value>> {
    let terms = query_terms(query);
    search_profile_route(storage, author, &terms, limit)
}

pub(crate) fn search_profile_route(
    storage: &Storage,
    author: &str,
    terms: &[String],
    limit: usize,
) -> Result<Vec<Value>> {
    let profiles = storage.paper_profiles(author, None)?;
    Ok(rank_profiles(profiles, terms, limit))
}

pub fn rank_profiles(profiles: Vec<Value>, terms: &[String], limit: usize) -> Vec<Value> {
    if terms.is_empty() {
        return profiles.into_iter().take(limit).collect();
    }

    let mut fallback = Vec::new();
    let mut scored = Vec::new();
    for profile in profiles {
        if fallback.len() < limit {
            fallback.push(profile.clone());
        }
        let score = weighted_profile_score(&profile, terms);
        if score > 0 {
            scored.push((score, profile));
        }
    }
    scored.sort_by(|left, right| right.0.cmp(&left.0));
    if scored.is_empty() {
        fallback
    } else {
        scored
            .into_iter()
            .take(limit)
            .map(|(_, profile)| profile)
            .collect()
    }
}

fn weighted_profile_score(profile: &Value, terms: &[String]) -> usize {
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
            .map(|value| value_match_count(value, terms) * weight)
            .unwrap_or(0)
    })
    .sum::<usize>()
        + value_match_count(profile, terms)
}

fn value_match_count(value: &Value, terms: &[String]) -> usize {
    let text = value_text(value).to_lowercase();
    terms
        .iter()
        .map(|term| text.matches(&term.to_lowercase()).count())
        .sum()
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
}
