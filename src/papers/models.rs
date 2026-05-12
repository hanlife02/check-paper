use std::collections::BTreeMap;
use std::path::PathBuf;

use serde_json::Value;

#[derive(Debug, Clone)]
pub struct Section {
    pub title: String,
    pub level: usize,
    pub content: String,
}

impl Section {
    pub fn section_kind(&self) -> &'static str {
        section_kind_from_title(&self.title)
    }

    pub fn caption_label(&self) -> Option<String> {
        caption_label_from_title(&self.title)
    }
}

#[derive(Debug, Clone)]
pub struct Paper {
    pub author: String,
    pub paper_id: String,
    pub paper_dir: PathBuf,
    pub article_path: PathBuf,
    pub fetch_result_path: Option<PathBuf>,
    pub source_hash: String,
    pub metadata: BTreeMap<String, String>,
    pub fetch_result: Value,
    pub raw_body: String,
    pub clean_text: String,
    pub sections: Vec<Section>,
}

impl Paper {
    pub fn key(&self) -> String {
        format!("{}/{}", self.author, self.paper_id)
    }

    pub fn title(&self) -> String {
        clean_title(
            self.metadata
                .get("title")
                .cloned()
                .or_else(|| {
                    self.fetch_result
                        .get("title")
                        .and_then(json_scalar_to_string)
                })
                .or_else(|| {
                    self.fetch_result
                        .get("record")
                        .and_then(|record| record.get("title"))
                        .and_then(json_scalar_to_string)
                })
                .unwrap_or_else(|| self.paper_id.clone()),
        )
    }

    pub fn doi(&self) -> String {
        self.metadata
            .get("doi")
            .cloned()
            .or_else(|| self.fetch_result.get("doi").and_then(json_scalar_to_string))
            .or_else(|| {
                self.fetch_result
                    .get("record")
                    .and_then(|record| record.get("doi"))
                    .and_then(json_scalar_to_string)
            })
            .unwrap_or_default()
            .trim()
            .to_string()
    }

    pub fn year(&self) -> String {
        self.metadata
            .get("year")
            .cloned()
            .or_else(|| {
                self.fetch_result
                    .get("year")
                    .and_then(json_scalar_to_string)
            })
            .or_else(|| {
                self.fetch_result
                    .get("record")
                    .and_then(|record| record.get("year"))
                    .and_then(json_scalar_to_string)
            })
            .unwrap_or_default()
            .trim()
            .to_string()
    }

    pub fn source(&self) -> String {
        self.metadata
            .get("source")
            .cloned()
            .or_else(|| {
                self.fetch_result
                    .get("source")
                    .and_then(json_scalar_to_string)
            })
            .unwrap_or_default()
    }
}

fn json_scalar_to_string(value: &Value) -> Option<String> {
    match value {
        Value::String(value) => Some(value.clone()),
        Value::Number(value) => Some(value.to_string()),
        Value::Bool(value) => Some(value.to_string()),
        _ => None,
    }
}

fn clean_title(mut title: String) -> String {
    for noise in [
        "Click to copy article linkArticle link copied!",
        "Click to copy section linkSection link copied!",
    ] {
        title = title.replace(noise, "");
    }
    title.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn section_kind_from_title(title: &str) -> &'static str {
    let lower = title.to_lowercase();
    if is_caption_title(&lower, "figure") || lower.starts_with("fig ") || lower.starts_with("图 ")
    {
        "figure_caption"
    } else if is_caption_title(&lower, "table") || lower.starts_with("表 ") {
        "table_caption"
    } else {
        "body"
    }
}

fn is_caption_title(lower: &str, prefix: &str) -> bool {
    lower.starts_with(prefix) && lower.contains("caption")
}

fn caption_label_from_title(title: &str) -> Option<String> {
    let label = title.trim().strip_suffix(" Caption")?.trim();
    if label.is_empty() {
        None
    } else {
        Some(label.to_string())
    }
}
