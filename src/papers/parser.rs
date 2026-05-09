use std::collections::BTreeMap;

use super::models::Section;

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
    let mut current_title: Option<String> = None;
    let mut current_level = 1usize;
    let mut current_body: Vec<String> = Vec::new();

    for line in markdown.lines() {
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

fn parse_simple_yaml(lines: &[String]) -> BTreeMap<String, String> {
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
        let first = value.chars().next().unwrap();
        let last = value.chars().last().unwrap();
        if (first == '"' && last == '"') || (first == '\'' && last == '\'') {
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
    fn parses_sections() {
        let sections = parse_sections("# Title\nA\n\n## Abstract\nB");
        let titles: Vec<_> = sections
            .iter()
            .map(|section| section.title.as_str())
            .collect();
        assert_eq!(titles, vec!["Title", "Abstract"]);
    }
}
