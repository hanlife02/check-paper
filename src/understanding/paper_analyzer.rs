use anyhow::Result;
use serde_json::Value;

use crate::papers::models::Paper;

use super::json_utils::parse_json_object;
use super::llm::OpenAiCompatibleClient;
use super::prompts::paper_analysis_messages;

pub fn analyze_paper(
    paper: &Paper,
    llm: &OpenAiCompatibleClient,
    max_chars: usize,
) -> Result<Value> {
    let context = build_analysis_context(paper, max_chars);
    let content = llm.chat(paper_analysis_messages(paper, &context), 0.1, 2400)?;
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
    Ok(profile)
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
