use std::collections::BTreeMap;

use super::models::Section;

pub const PARSER_VERSION: &str = "markdown-parser-v4";

#[derive(Debug, Clone)]
struct Caption {
    label: String,
    text: String,
}

impl Caption {
    fn title(&self) -> String {
        format!("{} Caption", self.label)
    }

    fn content(&self) -> String {
        if self.text.is_empty() {
            self.label.clone()
        } else {
            format!("{}: {}", self.label, self.text)
        }
    }
}

pub fn parse_frontmatter(markdown: &str) -> (BTreeMap<String, String>, String) {
    let mut lines = markdown.lines();
    if lines.next().map(str::trim) != Some("---") {
        return (BTreeMap::new(), markdown.to_string());
    }

    let mut metadata_lines = Vec::new();
    let mut body_lines = Vec::new();
    let mut in_metadata = true;
    for line in lines {
        if in_metadata && line.trim() == "---" {
            in_metadata = false;
            continue;
        }
        if in_metadata {
            metadata_lines.push(line.to_string());
        } else {
            body_lines.push(line.to_string());
        }
    }

    if in_metadata {
        return (BTreeMap::new(), markdown.to_string());
    }

    (
        parse_simple_yaml(&metadata_lines),
        body_lines.join("\n").trim().to_string(),
    )
}

pub fn parse_sections(markdown: &str) -> Vec<Section> {
    let mut sections = Vec::new();
    let mut figure_captions = Vec::new();
    let mut table_captions = Vec::new();
    let mut current_title: Option<String> = None;
    let mut current_level = 1usize;
    let mut current_body: Vec<String> = Vec::new();

    for line in markdown.lines() {
        let trimmed = line.trim();
        if let Some(caption) = parse_figure_caption(trimmed) {
            figure_captions.push(caption);
        } else if let Some(caption) = parse_table_caption(trimmed) {
            table_captions.push(caption);
        }
        if let Some((level, title)) = parse_heading(line) {
            if let Some(title) = current_title.take() {
                let content = current_body.join("\n").trim().to_string();
                sections.push(Section {
                    title,
                    level: current_level,
                    content,
                });
                current_body.clear();
            }
            current_title = Some(strip_inline_noise(title));
            current_level = level;
        } else {
            current_body.push(line.to_string());
        }
    }

    if let Some(title) = current_title {
        let content = current_body.join("\n").trim().to_string();
        sections.push(Section {
            title,
            level: current_level,
            content,
        });
    }

    if sections.is_empty() && !markdown.trim().is_empty() {
        sections.push(Section {
            title: "Body".to_string(),
            level: 1,
            content: markdown.trim().to_string(),
        });
    }

    for caption in figure_captions {
        sections.push(Section {
            title: caption.title(),
            level: 2,
            content: caption.content(),
        });
    }
    for caption in table_captions {
        sections.push(Section {
            title: caption.title(),
            level: 2,
            content: caption.content(),
        });
    }

    sections
        .into_iter()
        .filter(|section| !section.content.trim().is_empty())
        .collect()
}

fn parse_heading(line: &str) -> Option<(usize, &str)> {
    let trimmed = line.trim_start();
    let level = trimmed.chars().take_while(|ch| *ch == '#').count();
    if level == 0 || level > 6 {
        return None;
    }
    let rest = trimmed[level..].trim_start();
    if rest.is_empty() {
        None
    } else {
        Some((level, rest.trim()))
    }
}

fn parse_figure_caption(line: &str) -> Option<Caption> {
    parse_english_caption(line, "figure", "fig", "Figure").or_else(|| parse_cjk_caption(line, '图'))
}

fn parse_table_caption(line: &str) -> Option<Caption> {
    parse_english_caption(line, "table", "table", "Table").or_else(|| parse_cjk_caption(line, '表'))
}

fn parse_english_caption(
    line: &str,
    long_prefix: &str,
    short_prefix: &str,
    canonical_prefix: &str,
) -> Option<Caption> {
    let line = normalize_caption_line(line);
    let lower = line.to_lowercase();
    let after_prefix = if starts_with_caption_prefix(&lower, &line, long_prefix) {
        &line[long_prefix.len()..]
    } else if short_prefix != long_prefix && starts_with_caption_prefix(&lower, &line, short_prefix)
    {
        &line[short_prefix.len()..]
    } else {
        return None;
    };
    let after_prefix = after_prefix.trim_start_matches('.').trim_start();
    parse_caption_tail(after_prefix).map(|(label, text)| Caption {
        label: format!("{canonical_prefix} {label}"),
        text,
    })
}

fn starts_with_caption_prefix(lower: &str, original: &str, prefix: &str) -> bool {
    if !lower.starts_with(prefix) {
        return false;
    }
    original[prefix.len()..]
        .chars()
        .next()
        .is_some_and(|ch| ch == '.' || ch.is_whitespace() || ch.is_ascii_digit())
}

fn parse_cjk_caption(line: &str, marker: char) -> Option<Caption> {
    let line = normalize_caption_line(line);
    let after_marker = line.strip_prefix(marker)?.trim_start();
    parse_caption_tail(after_marker).map(|(label, text)| Caption {
        label: format!("{marker} {label}"),
        text,
    })
}

fn parse_caption_tail(value: &str) -> Option<(String, String)> {
    let mut label_end = 0usize;
    for (index, ch) in value.char_indices() {
        if is_caption_label_char(ch) || is_caption_label_comma(value, index) {
            label_end = index + ch.len_utf8();
        } else {
            break;
        }
    }
    if label_end == 0 {
        return None;
    }
    let label = value[..label_end].to_string();
    if !label.chars().any(|ch| ch.is_ascii_digit()) {
        return None;
    }
    let text = value[label_end..]
        .trim_start()
        .trim_start_matches(['.', ':', '-', '|', ')'])
        .trim_start()
        .to_string();
    Some((label, text))
}

fn normalize_caption_line(value: &str) -> String {
    normalize_caption_punctuation(&strip_inline_noise(value))
        .replace('\u{00a0}', " ")
        .trim_start_matches(['-', '*', ' '])
        .replace("**", "")
        .replace("__", "")
        .trim()
        .to_string()
}

fn normalize_caption_punctuation(value: &str) -> String {
    value
        .chars()
        .map(|ch| match ch {
            '‐' | '‑' | '–' | '—' => '-',
            _ => ch,
        })
        .collect()
}

fn is_caption_label_char(ch: char) -> bool {
    ch.is_alphanumeric() || ch == '-'
}

fn is_caption_label_comma(value: &str, comma_index: usize) -> bool {
    let previous_is_label = value[..comma_index]
        .chars()
        .next_back()
        .is_some_and(|ch| ch.is_alphanumeric());
    let next_is_label = value[comma_index + ','.len_utf8()..]
        .chars()
        .next()
        .is_some_and(|ch| ch.is_alphanumeric());
    previous_is_label && next_is_label
}

fn parse_simple_yaml(lines: &[String]) -> BTreeMap<String, String> {
    parse_yaml_metadata(lines).unwrap_or_else(|| parse_line_based_metadata(lines))
}

fn parse_yaml_metadata(lines: &[String]) -> Option<BTreeMap<String, String>> {
    let text = lines.join("\n");
    let value: serde_yaml::Value = serde_yaml::from_str(&text).ok()?;
    let object = value.as_mapping()?;
    let mut metadata = BTreeMap::new();
    for (key, value) in object {
        let Some(key) = key.as_str() else {
            continue;
        };
        metadata.insert(key.trim().to_string(), yaml_scalar_to_string(value));
    }
    Some(metadata)
}

fn yaml_scalar_to_string(value: &serde_yaml::Value) -> String {
    match value {
        serde_yaml::Value::Null => String::new(),
        serde_yaml::Value::Bool(value) => value.to_string(),
        serde_yaml::Value::Number(value) => value.to_string(),
        serde_yaml::Value::String(value) => value.clone(),
        serde_yaml::Value::Sequence(items) => items
            .iter()
            .map(yaml_scalar_to_string)
            .collect::<Vec<_>>()
            .join(", "),
        _ => serde_yaml::to_string(value)
            .unwrap_or_default()
            .trim()
            .to_string(),
    }
}

fn parse_line_based_metadata(lines: &[String]) -> BTreeMap<String, String> {
    let mut metadata = BTreeMap::new();
    for line in lines {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        if let Some((key, value)) = line.split_once(':') {
            metadata.insert(key.trim().to_string(), parse_scalar(value.trim()));
        }
    }
    metadata
}

fn parse_scalar(value: &str) -> String {
    let value = value.trim();
    if value.len() >= 2 {
        let first = value.chars().next();
        let last = value.chars().last();
        if (first == Some('"') && last == Some('"')) || (first == Some('\'') && last == Some('\''))
        {
            return value[1..value.len() - 1].to_string();
        }
    }
    value.to_string()
}

fn strip_inline_noise(value: &str) -> String {
    value
        .replace("Click to copy section linkSection link copied!", "")
        .replace("Click to copy article linkArticle link copied!", "")
        .trim()
        .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_frontmatter() {
        let (metadata, body) =
            parse_frontmatter("---\ntitle: \"A Paper\"\nyear: \"2024\"\n---\n# Body\nText");
        assert_eq!(metadata["title"], "A Paper");
        assert_eq!(metadata["year"], "2024");
        assert!(body.starts_with("# Body"));
    }

    #[test]
    fn parses_yaml_frontmatter_sequences() {
        let (metadata, _) =
            parse_frontmatter("---\ntitle: A Paper\nauthors:\n  - Alice\n  - Bob\n---\nBody");
        assert_eq!(metadata["title"], "A Paper");
        assert_eq!(metadata["authors"], "Alice, Bob");
    }

    #[test]
    fn parses_sections() {
        let sections = parse_sections("# Title\nA\n\n## Abstract\nB");
        let titles: Vec<_> = sections
            .iter()
            .map(|section| section.title.as_str())
            .collect();
        assert_eq!(titles, vec!["Title", "Abstract"]);
    }

    #[test]
    fn preserves_figure_and_table_captions_as_sections() {
        let sections =
            parse_sections("# Results\nFigure 1. Conversion trend.\nTable 2. Catalyst metrics.");
        assert!(
            sections
                .iter()
                .any(|section| section.title == "Figure 1 Caption"
                    && section.section_kind() == "figure_caption"
                    && section.caption_label().as_deref() == Some("Figure 1")
                    && section.content == "Figure 1: Conversion trend.")
        );
        assert!(
            sections
                .iter()
                .any(|section| section.title == "Table 2 Caption"
                    && section.section_kind() == "table_caption"
                    && section.caption_label().as_deref() == Some("Table 2")
                    && section.content == "Table 2: Catalyst metrics.")
        );
    }

    #[test]
    fn extracts_compact_caption_labels() {
        let sections = parse_sections("Fig.1| Fast conversion.\n**Table S1.** Catalyst metrics.");

        assert!(
            sections
                .iter()
                .any(|section| section.title == "Figure 1 Caption"
                    && section.content == "Figure 1: Fast conversion.")
        );
        assert!(
            sections
                .iter()
                .any(|section| section.title == "Table S1 Caption"
                    && section.content == "Table S1: Catalyst metrics.")
        );
    }

    #[test]
    fn extracts_compound_caption_labels() {
        let sections = parse_sections(
            "Fig. S1a,b. Schematic and SEM images.\nFigure 2A–C: Stability series.\nTable S2–S4. Catalyst metrics.",
        );

        assert!(
            sections
                .iter()
                .any(|section| section.title == "Figure S1a,b Caption"
                    && section.caption_label().as_deref() == Some("Figure S1a,b")
                    && section.content == "Figure S1a,b: Schematic and SEM images.")
        );
        assert!(
            sections
                .iter()
                .any(|section| section.title == "Figure 2A-C Caption"
                    && section.caption_label().as_deref() == Some("Figure 2A-C")
                    && section.content == "Figure 2A-C: Stability series.")
        );
        assert!(
            sections
                .iter()
                .any(|section| section.title == "Table S2-S4 Caption"
                    && section.caption_label().as_deref() == Some("Table S2-S4")
                    && section.content == "Table S2-S4: Catalyst metrics.")
        );
    }

    #[test]
    fn extracts_structured_caption_metadata() {
        let sections = parse_sections(
            "Fig. S1a,b. Schematic and SEM images.\nFigure 2A–C: Stability series.\nTable S2–S4. Catalyst metrics.",
        );

        let figure_panels = sections
            .iter()
            .find(|section| section.title == "Figure S1a,b Caption")
            .and_then(|section| section.caption_metadata())
            .unwrap();
        assert_eq!(figure_panels.object_type, "figure");
        assert_eq!(figure_panels.object_label, "S1");
        assert_eq!(figure_panels.panel_labels, vec!["a", "b"]);
        assert_eq!(
            figure_panels.target_labels,
            vec!["Figure S1a", "Figure S1b"]
        );

        let figure_range = sections
            .iter()
            .find(|section| section.title == "Figure 2A-C Caption")
            .and_then(|section| section.caption_metadata())
            .unwrap();
        assert_eq!(figure_range.object_type, "figure");
        assert_eq!(figure_range.object_label, "2");
        assert_eq!(figure_range.panel_labels, vec!["A", "B", "C"]);
        assert_eq!(
            figure_range.target_labels,
            vec!["Figure 2A", "Figure 2B", "Figure 2C"]
        );

        let table_range = sections
            .iter()
            .find(|section| section.title == "Table S2-S4 Caption")
            .and_then(|section| section.caption_metadata())
            .unwrap();
        assert_eq!(table_range.object_type, "table");
        assert_eq!(table_range.object_label, "S2-S4");
        assert!(table_range.panel_labels.is_empty());
        assert_eq!(
            table_range.target_labels,
            vec!["Table S2", "Table S3", "Table S4"]
        );
    }
}
