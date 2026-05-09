use std::collections::{BTreeMap, HashMap};

use anyhow::Result;
use serde_json::{Value, json};

use super::json_utils::parse_json_object;
use super::llm::OpenAiCompatibleClient;
use super::prompts::author_profile_messages;

pub fn build_author_profile(
    author: &str,
    profiles: &[Value],
    llm: Option<&OpenAiCompatibleClient>,
) -> Result<Value> {
    let deterministic = deterministic_profile(author, profiles);
    let Some(llm) = llm else {
        return Ok(deterministic);
    };
    if profiles.is_empty() {
        return Ok(deterministic);
    }
    let compact: Vec<Value> = profiles.iter().take(80).map(compact_profile).collect();
    let content = llm.chat(author_profile_messages(author, &compact), 0.1, 2400)?;
    let mut profile = parse_json_object(&content);
    if let Some(object) = profile.as_object_mut() {
        object.entry("author").or_insert(author.into());
        object
            .entry("total_profiled_papers")
            .or_insert((profiles.len() as u64).into());
        object
            .entry("keyword_overview")
            .or_insert(deterministic["keyword_overview"].clone());
    }
    Ok(profile)
}

fn deterministic_profile(author: &str, profiles: &[Value]) -> Value {
    let mut years = profiles
        .iter()
        .filter_map(|profile| profile.get("year").and_then(Value::as_str))
        .map(str::to_string)
        .collect::<Vec<_>>();
    years.sort();
    years.dedup();

    let mut keywords: HashMap<String, usize> = HashMap::new();
    let mut representative = Vec::new();
    for profile in profiles {
        if let Some(items) = profile.get("topic_keywords").and_then(Value::as_array) {
            for item in items {
                if let Some(keyword) = item.as_str() {
                    *keywords.entry(keyword.to_string()).or_default() += 1;
                }
            }
        }
        representative.push(json!({
            "year": profile.get("year").cloned().unwrap_or_default(),
            "title": profile.get("title").cloned().unwrap_or_default(),
            "doi": profile.get("doi").cloned().unwrap_or_default(),
            "summary": profile.get("one_sentence_summary").cloned().unwrap_or_default(),
        }));
    }
    representative.truncate(12);

    let mut keyword_counts: Vec<_> = keywords.into_iter().collect();
    keyword_counts.sort_by(|left, right| right.1.cmp(&left.1).then_with(|| left.0.cmp(&right.0)));
    let keyword_overview: Vec<String> = keyword_counts
        .into_iter()
        .take(20)
        .map(|(keyword, _)| keyword)
        .collect();

    json!({
        "author": author,
        "total_profiled_papers": profiles.len(),
        "year_span": if years.is_empty() { Vec::<String>::new() } else { vec![years[0].clone(), years[years.len() - 1].clone()] },
        "keyword_overview": keyword_overview,
        "representative_recent_works": representative,
    })
}

fn compact_profile(profile: &Value) -> Value {
    let mut object = BTreeMap::new();
    for key in ["year", "title", "doi", "one_sentence_summary"] {
        if let Some(value) = profile.get(key) {
            object.insert(key.to_string(), value.clone());
        }
    }
    for (target, source, limit) in [
        ("contributions", "core_contributions", 3usize),
        ("methods", "methods", 3),
        ("key_results", "key_results", 3),
        ("keywords", "topic_keywords", 8),
    ] {
        let values = profile
            .get(source)
            .and_then(Value::as_array)
            .map(|items| items.iter().take(limit).cloned().collect::<Vec<_>>())
            .unwrap_or_default();
        object.insert(target.to_string(), Value::Array(values));
    }
    serde_json::to_value(object).unwrap_or_default()
}
