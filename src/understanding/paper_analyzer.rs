use anyhow::Result;
use serde_json::Value;
use serde_json::json;

use crate::papers::models::Paper;
use crate::retrieval::chunker::chunk_paper;

use super::json_utils::parse_json_object;
use super::llm::OpenAiCompatibleClient;
use super::prompts::{paper_analysis_messages_with_chunks, paper_profile_repair_messages};

pub fn analyze_paper(
    paper: &Paper,
    llm: &OpenAiCompatibleClient,
    max_chars: usize,
) -> Result<Value> {
    let context = build_analysis_context(paper, max_chars);
    let chunks = chunk_paper(paper, 3200, 350);
    let content = llm.chat(
        paper_analysis_messages_with_chunks(paper, &context, &chunks),
        0.1,
        2400,
    )?;
    let mut profile = parse_json_object(&content);
    if let Some(object) = profile.as_object_mut() {
        object.entry("paper_key").or_insert(paper.key().into());
        object.entry("title").or_insert(paper.title().into());
        object.entry("doi").or_insert(paper.doi().into());
        object.entry("year").or_insert(paper.year().into());
        object
            .entry("source_hash")
            .or_insert(paper.source_hash.clone().into());
    }
    if let Err(error) = validate_paper_profile(&profile, chunks.len()) {
        let repaired = llm.chat(
            paper_profile_repair_messages(&content, &error.to_string(), &chunks),
            0.0,
            2400,
        )?;
        profile = parse_json_object(&repaired);
        if let Some(object) = profile.as_object_mut() {
            object.entry("paper_key").or_insert(paper.key().into());
            object.entry("title").or_insert(paper.title().into());
            object.entry("doi").or_insert(paper.doi().into());
            object.entry("year").or_insert(paper.year().into());
            object
                .entry("source_hash")
                .or_insert(paper.source_hash.clone().into());
        }
        validate_paper_profile(&profile, chunks.len())?;
    }
    Ok(profile)
}

fn validate_paper_profile(profile: &Value, chunk_count: usize) -> Result<()> {
    for field in ["paper_key", "title", "doi", "year", "one_sentence_summary"] {
        if !profile
            .get(field)
            .and_then(Value::as_str)
            .is_some_and(|value| !value.trim().is_empty())
        {
            return Err(anyhow::anyhow!("PaperProfileV1 missing non-empty {field}"));
        }
    }
    validate_profile_evidence_chunks(profile, chunk_count)
}

fn validate_profile_evidence_chunks(profile: &Value, chunk_count: usize) -> Result<()> {
    for field in ["methods", "key_results", "limitations"] {
        let Some(items) = profile.get(field).and_then(Value::as_array) else {
            continue;
        };
        for item in items {
            let chunks = item
                .get("evidence_chunks")
                .and_then(Value::as_array)
                .ok_or_else(|| anyhow::anyhow!("{field} item missing evidence_chunks"))?;
            if chunks.is_empty() {
                return Err(anyhow::anyhow!("{field} item has empty evidence_chunks"));
            }
            for chunk_id in chunks {
                let Some(chunk_id) = chunk_id.as_u64() else {
                    return Err(anyhow::anyhow!("{field} evidence chunk is not an integer"));
                };
                if chunk_id as usize >= chunk_count {
                    return Err(anyhow::anyhow!(
                        "{field} evidence chunk {chunk_id} is outside available chunk range"
                    ));
                }
            }
        }
    }
    Ok(())
}

pub fn build_analysis_context(paper: &Paper, max_chars: usize) -> String {
    let preferred = [
        "abstract",
        "introduction",
        "results",
        "results and discussion",
        "discussion",
        "conclusions",
        "conclusion",
    ];
    let mut selected = Vec::new();
    for section in &paper.sections {
        if preferred
            .iter()
            .any(|title| *title == section.title.to_lowercase())
        {
            selected.push(format!("## {}\n{}", section.title, section.content));
        }
    }
    if selected.is_empty() {
        selected.push(paper.clean_text.clone());
    } else if let Some(tail) = tail_context(&paper.clean_text, 5000) {
        selected.push(format!("## Tail Context\n{tail}"));
    }

    take_chars(&selected.join("\n\n"), max_chars)
}

pub fn extract_section_facts(paper: &Paper) -> Vec<Value> {
    let chunks = chunk_paper(paper, 3200, 350);
    chunks
        .iter()
        .filter_map(|chunk| {
            let fact_type = section_fact_type(&chunk.section)?;
            Some(json!({
                "paper_key": chunk.paper_key,
                "chunk_id": chunk.chunk_index,
                "section": chunk.section,
                "fact_type": fact_type,
                "text": chunk.text,
            }))
        })
        .collect()
}

fn section_fact_type(section: &str) -> Option<&'static str> {
    let section = section.to_lowercase();
    if section.contains("method") || section.contains("experiment") {
        Some("method")
    } else if section.contains("result")
        || section.contains("discussion")
        || section.contains("conclusion")
    {
        Some("result")
    } else if section.contains("limitation") {
        Some("limitation")
    } else if section.contains("dataset") || section.contains("data") {
        Some("dataset")
    } else if section.contains("metric") {
        Some("metric")
    } else {
        None
    }
}

fn tail_context(text: &str, max_chars: usize) -> Option<String> {
    let chars: Vec<char> = text.chars().collect();
    if chars.len() <= max_chars {
        return None;
    }
    Some(chars[chars.len() - max_chars..].iter().collect())
}

fn take_chars(text: &str, max_chars: usize) -> String {
    text.chars().take(max_chars).collect()
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use crate::papers::models::{Paper, Section};

    use super::{extract_section_facts, validate_paper_profile, validate_profile_evidence_chunks};

    #[test]
    fn accepts_profile_claims_with_valid_chunk_ids() {
        let profile = json!({
            "methods": [{"method": "m", "evidence_chunks": [0]}],
            "key_results": [{"claim": "r", "evidence_chunks": [1]}],
            "limitations": [{"limitation": "l", "evidence_chunks": [0]}]
        });
        validate_profile_evidence_chunks(&profile, 2).unwrap();
    }

    #[test]
    fn rejects_profile_claims_without_valid_chunk_ids() {
        let profile = json!({
            "key_results": [{"claim": "r", "evidence_chunks": [3]}]
        });
        assert!(validate_profile_evidence_chunks(&profile, 2).is_err());
    }

    #[test]
    fn validates_paper_profile_v1_minimum_shape() {
        let profile = json!({
            "paper_key": "Alice/paper-a",
            "title": "A Paper",
            "doi": "10.1/test",
            "year": "2024",
            "one_sentence_summary": "summary",
            "methods": [{"method": "m", "evidence_chunks": [0]}]
        });
        validate_paper_profile(&profile, 1).unwrap();
    }

    #[test]
    fn rejects_paper_profile_missing_required_fields() {
        let profile = json!({
            "paper_key": "Alice/paper-a",
            "methods": [{"method": "m", "evidence_chunks": [0]}]
        });
        assert!(validate_paper_profile(&profile, 1).is_err());
    }

    #[test]
    fn extracts_section_facts_from_methods_results_and_dataset_sections() {
        let paper = Paper {
            author: "Alice".to_string(),
            paper_id: "paper-a".to_string(),
            paper_dir: std::path::PathBuf::new(),
            article_path: std::path::PathBuf::new(),
            fetch_result_path: None,
            source_hash: "hash".to_string(),
            metadata: Default::default(),
            fetch_result: json!({}),
            raw_body: String::new(),
            clean_text: String::new(),
            sections: vec![
                Section {
                    title: "Methods".to_string(),
                    level: 2,
                    content: "method text".to_string(),
                },
                Section {
                    title: "Dataset".to_string(),
                    level: 2,
                    content: "dataset text".to_string(),
                },
                Section {
                    title: "Results".to_string(),
                    level: 2,
                    content: "result text".to_string(),
                },
            ],
        };
        let facts = extract_section_facts(&paper);
        let fact_types = facts
            .iter()
            .filter_map(|fact| fact.get("fact_type").and_then(serde_json::Value::as_str))
            .collect::<Vec<_>>();

        assert!(fact_types.contains(&"method"));
        assert!(fact_types.contains(&"dataset"));
        assert!(fact_types.contains(&"result"));
    }
}
