use super::models::Section;

const NOISE_REPLACEMENTS: &[(&str, &str)] = &[
    ("Click to copy article linkArticle link copied!", ""),
    ("Click to copy section linkSection link copied!", ""),
    ("High Resolution ImageDownload MS PowerPoint Slide", ""),
    ("Download Hi-Res ImageDownload to MS-PowerPoint", ""),
    ("CloseNextPrevious", ""),
    ("Request reuse permissions", ""),
    ("<redacted_base64>", ""),
    ("[url]", " "),
];

pub fn select_primary_body(body: &str) -> String {
    if let Some(index) = body.find("\n## Article Body") {
        return body[index + 1..].to_string();
    }
    if body.starts_with("## Article Body") {
        return body.to_string();
    }
    body.to_string()
}

pub fn clean_text(text: &str) -> String {
    let mut cleaned = text.replace("\r\n", "\n").replace('\r', "\n");
    for (old, new) in NOISE_REPLACEMENTS {
        cleaned = cleaned.replace(old, new);
    }
    collapse_whitespace(&cleaned)
}

pub fn clean_sections(sections: Vec<Section>) -> Vec<Section> {
    sections
        .into_iter()
        .filter_map(|section| {
            let content = clean_text(&section.content);
            if content.is_empty() {
                None
            } else {
                Some(Section { content, ..section })
            }
        })
        .collect()
}

fn collapse_whitespace(text: &str) -> String {
    let mut output = String::with_capacity(text.len());
    let mut blank_lines = 0usize;
    for raw_line in text.lines() {
        let line = raw_line.split_whitespace().collect::<Vec<_>>().join(" ");
        if line.is_empty() {
            blank_lines += 1;
            if blank_lines <= 1 {
                output.push('\n');
            }
        } else {
            blank_lines = 0;
            output.push_str(&line);
            output.push('\n');
        }
    }
    output.trim().to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn selects_article_body() {
        let body = "## Abstract\nNoise\n\n## Article Body\nReal";
        assert!(select_primary_body(body).starts_with("## Article Body"));
    }

    #[test]
    fn removes_known_noise() {
        let cleaned =
            clean_text("TitleClick to copy article linkArticle link copied!\n[url]\nA  B");
        assert!(!cleaned.contains("Click to copy"));
        assert!(!cleaned.contains("[url]"));
    }
}
