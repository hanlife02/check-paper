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

    pub fn caption_metadata(&self) -> Option<CaptionMetadata> {
        caption_metadata_from_label(&self.caption_label()?)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CaptionMetadata {
    pub object_type: String,
    pub object_label: String,
    pub panel_labels: Vec<String>,
    pub target_labels: Vec<String>,
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

fn caption_metadata_from_label(label: &str) -> Option<CaptionMetadata> {
    let (prefix, tail) = label.trim().split_once(' ')?;
    let object_type = caption_object_type(prefix)?;
    let tail = tail.trim();
    if tail.is_empty() {
        return None;
    }
    let parsed = parse_caption_tail_metadata(tail);
    Some(CaptionMetadata {
        object_type: object_type.to_string(),
        object_label: parsed.object_label,
        panel_labels: parsed.panel_labels,
        target_labels: parsed
            .target_labels
            .into_iter()
            .map(|target| format!("{prefix} {target}"))
            .collect(),
    })
}

fn caption_object_type(prefix: &str) -> Option<&'static str> {
    match prefix.to_lowercase().as_str() {
        "figure" | "fig" => Some("figure"),
        "table" => Some("table"),
        _ if prefix == "图" => Some("figure"),
        _ if prefix == "表" => Some("table"),
        _ => None,
    }
}

struct ParsedCaptionTail {
    object_label: String,
    panel_labels: Vec<String>,
    target_labels: Vec<String>,
}

fn parse_caption_tail_metadata(tail: &str) -> ParsedCaptionTail {
    let tail = tail.trim();
    if let Some(parsed) = parse_panel_list(tail) {
        return parsed;
    }
    if let Some(parsed) = parse_panel_range(tail) {
        return parsed;
    }
    if let Some(parsed) = parse_object_range(tail) {
        return parsed;
    }
    if let Some((base, panel)) = split_trailing_panel(tail) {
        return ParsedCaptionTail {
            object_label: base.clone(),
            panel_labels: vec![panel.clone()],
            target_labels: vec![format!("{base}{panel}")],
        };
    }
    ParsedCaptionTail {
        object_label: tail.to_string(),
        panel_labels: Vec::new(),
        target_labels: vec![tail.to_string()],
    }
}

fn parse_panel_list(tail: &str) -> Option<ParsedCaptionTail> {
    let parts = tail
        .split(',')
        .map(str::trim)
        .filter(|part| !part.is_empty())
        .collect::<Vec<_>>();
    if parts.len() < 2 {
        return None;
    }
    let (base, first_panel) = split_trailing_panel(parts[0])?;
    if !parts[1..].iter().all(|part| is_panel_label(part)) {
        return None;
    }
    let mut panel_labels = vec![first_panel];
    panel_labels.extend(parts[1..].iter().map(|part| (*part).to_string()));
    let target_labels = panel_labels
        .iter()
        .map(|panel| format!("{base}{panel}"))
        .collect();
    Some(ParsedCaptionTail {
        object_label: base,
        panel_labels,
        target_labels,
    })
}

fn parse_panel_range(tail: &str) -> Option<ParsedCaptionTail> {
    let (left, right) = tail.split_once('-')?;
    let (base, first_panel) = split_trailing_panel(left.trim())?;
    let right = right.trim();
    if !is_panel_label(right) {
        return None;
    }
    let panel_labels = expand_panel_range(&first_panel, right);
    let target_labels = panel_labels
        .iter()
        .map(|panel| format!("{base}{panel}"))
        .collect();
    Some(ParsedCaptionTail {
        object_label: base,
        panel_labels,
        target_labels,
    })
}

fn parse_object_range(tail: &str) -> Option<ParsedCaptionTail> {
    let (left, right) = tail.split_once('-')?;
    let left = parse_object_id(left.trim())?;
    let mut right = parse_object_id(right.trim())?;
    if right.prefix.is_empty() {
        right.prefix = left.prefix.clone();
    }
    if left.prefix != right.prefix || left.number > right.number || right.number - left.number > 50
    {
        return None;
    }
    let target_labels = (left.number..=right.number)
        .map(|number| format!("{}{}", left.prefix, number))
        .collect();
    Some(ParsedCaptionTail {
        object_label: tail.to_string(),
        panel_labels: Vec::new(),
        target_labels,
    })
}

#[derive(Debug)]
struct ObjectId {
    prefix: String,
    number: i64,
}

fn parse_object_id(value: &str) -> Option<ObjectId> {
    let mut prefix = String::new();
    let mut digits = String::new();
    for ch in value.chars() {
        if digits.is_empty() && ch.is_ascii_alphabetic() {
            prefix.push(ch);
        } else if ch.is_ascii_digit() {
            digits.push(ch);
        } else {
            return None;
        }
    }
    if digits.is_empty() {
        return None;
    }
    Some(ObjectId {
        prefix,
        number: digits.parse().ok()?,
    })
}

fn split_trailing_panel(value: &str) -> Option<(String, String)> {
    let mut split_at = value.len();
    for (index, ch) in value.char_indices().rev() {
        if ch.is_ascii_alphabetic() {
            split_at = index;
        } else {
            break;
        }
    }
    if split_at == value.len() || split_at == 0 {
        return None;
    }
    let base = &value[..split_at];
    if !base.chars().any(|ch| ch.is_ascii_digit()) {
        return None;
    }
    Some((base.to_string(), value[split_at..].to_string()))
}

fn is_panel_label(value: &str) -> bool {
    !value.is_empty() && value.chars().all(|ch| ch.is_ascii_alphabetic())
}

fn expand_panel_range(first: &str, last: &str) -> Vec<String> {
    let Some(start) = single_ascii_letter(first) else {
        return vec![first.to_string(), last.to_string()];
    };
    let Some(end) = single_ascii_letter(last) else {
        return vec![first.to_string(), last.to_string()];
    };
    if start.is_ascii_uppercase() != end.is_ascii_uppercase()
        || start > end
        || end as u8 - start as u8 > 25
    {
        return vec![first.to_string(), last.to_string()];
    }
    (start as u8..=end as u8)
        .map(|letter| (letter as char).to_string())
        .collect()
}

fn single_ascii_letter(value: &str) -> Option<char> {
    let mut chars = value.chars();
    let first = chars.next()?;
    if chars.next().is_none() && first.is_ascii_alphabetic() {
        Some(first)
    } else {
        None
    }
}
