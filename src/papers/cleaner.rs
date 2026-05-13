use super::models::Section;

pub const CLEANER_VERSION: &str = "source-cleaner-v4";

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

const ACS_REPLACEMENTS: &[(&str, &str)] = &[];

const ELSEVIER_REPLACEMENTS: &[(&str, &str)] = &[];

const RSC_REPLACEMENTS: &[(&str, &str)] = &[];

const WILEY_REPLACEMENTS: &[(&str, &str)] = &[];

const COMMON_NOISE_LINES: &[&str] = &[
    "advertisement",
    "download pdf",
    "download citation",
    "metrics",
    "altmetric",
    "crossmark",
];

const COMMON_COUNT_PREFIX_LINES: &[&str] = &["download pdf", "metrics"];

const ACS_NOISE_LINES: &[&str] = &[
    "get e-alerts",
    "add to favorites",
    "supporting information",
    "article views",
];

const ELSEVIER_NOISE_LINES: &[&str] = &[
    "recommended articles",
    "cited by",
    "view pdf",
    "article preview",
    "author links open overlay panel",
    "show more",
    "show less",
];

const ELSEVIER_COUNT_PREFIX_LINES: &[&str] = &["recommended articles", "cited by"];

const RSC_NOISE_LINES: &[&str] = &[
    "article information",
    "back to tab navigation",
    "permissions",
    "social activity",
    "supplementary information",
];

const WILEY_NOISE_LINES: &[&str] = &[
    "citing literature",
    "email alerts",
    "figures",
    "get access",
    "sections",
    "share",
    "supporting information",
    "tools",
];

const WILEY_COUNT_PREFIX_LINES: &[&str] = &["citing literature", "figures", "sections"];

const SPRINGER_NOISE_LINES: &[&str] = &[
    "about this article",
    "access this article",
    "article metrics",
    "publisher's note",
    "reprints and permissions",
    "rights and permissions",
];

const SPRINGER_COUNT_PREFIX_LINES: &[&str] = &["about this article", "article metrics"];

const NATURE_NOISE_LINES: &[&str] = &[
    "about this article",
    "access through your institution",
    "rights and permissions",
    "subjects",
];

const NATURE_COUNT_PREFIX_LINES: &[&str] = &["about this article", "subjects"];

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
    clean_text_for_source(text, "")
}

pub fn clean_text_for_source(text: &str, source: &str) -> String {
    let mut cleaned = text.replace("\r\n", "\n").replace('\r', "\n");
    for (old, new) in NOISE_REPLACEMENTS {
        cleaned = cleaned.replace(old, new);
    }
    for replacements in source_replacement_layers(source) {
        for (old, new) in replacements {
            cleaned = cleaned.replace(old, new);
        }
    }
    cleaned = remove_noise_lines(&cleaned, source);
    collapse_whitespace(&cleaned)
}

pub fn clean_sections(sections: Vec<Section>) -> Vec<Section> {
    clean_sections_for_source(sections, "")
}

pub fn clean_sections_for_source(sections: Vec<Section>, source: &str) -> Vec<Section> {
    sections
        .into_iter()
        .filter(|section| !is_reference_section(&section.title))
        .filter_map(|section| {
            let content = clean_text_for_source(&section.content, source);
            if content.is_empty() {
                None
            } else {
                Some(Section { content, ..section })
            }
        })
        .collect()
}

fn source_replacement_layers(source: &str) -> Vec<&'static [(&'static str, &'static str)]> {
    let source = source.to_lowercase();
    let mut layers = Vec::new();
    if source.contains("acs") || source.contains("american chemical society") {
        layers.push(ACS_REPLACEMENTS);
    }
    if source.contains("elsevier") || source.contains("sciencedirect") {
        layers.push(ELSEVIER_REPLACEMENTS);
    }
    if source.contains("royal society of chemistry")
        || source.contains("rsc publishing")
        || source_has_token(&source, "rsc")
    {
        layers.push(RSC_REPLACEMENTS);
    }
    if source.contains("wiley") {
        layers.push(WILEY_REPLACEMENTS);
    }
    layers
}

fn source_noise_line_layers(source: &str) -> Vec<&'static [&'static str]> {
    let source = source.to_lowercase();
    let mut layers = vec![COMMON_NOISE_LINES];
    if source.contains("acs") || source.contains("american chemical society") {
        layers.push(ACS_NOISE_LINES);
    }
    if source.contains("elsevier") || source.contains("sciencedirect") {
        layers.push(ELSEVIER_NOISE_LINES);
    }
    if source.contains("royal society of chemistry")
        || source.contains("rsc publishing")
        || source_has_token(&source, "rsc")
    {
        layers.push(RSC_NOISE_LINES);
    }
    if source.contains("wiley") {
        layers.push(WILEY_NOISE_LINES);
    }
    if source.contains("springer") {
        layers.push(SPRINGER_NOISE_LINES);
    }
    if source.contains("nature") {
        layers.push(NATURE_NOISE_LINES);
    }
    layers
}

fn source_count_prefix_layers(source: &str) -> Vec<&'static [&'static str]> {
    let source = source.to_lowercase();
    let mut layers = vec![COMMON_COUNT_PREFIX_LINES];
    if source.contains("elsevier") || source.contains("sciencedirect") {
        layers.push(ELSEVIER_COUNT_PREFIX_LINES);
    }
    if source.contains("wiley") {
        layers.push(WILEY_COUNT_PREFIX_LINES);
    }
    if source.contains("springer") {
        layers.push(SPRINGER_COUNT_PREFIX_LINES);
    }
    if source.contains("nature") {
        layers.push(NATURE_COUNT_PREFIX_LINES);
    }
    layers
}

fn source_has_token(source: &str, token: &str) -> bool {
    source
        .split(|ch: char| !ch.is_alphanumeric())
        .any(|part| part == token)
}

fn remove_noise_lines(text: &str, source: &str) -> String {
    text.lines()
        .filter(|line| !is_noise_line(line, source))
        .collect::<Vec<_>>()
        .join("\n")
}

fn is_noise_line(line: &str, source: &str) -> bool {
    let normalized = normalize_noise_line(line);
    !normalized.is_empty()
        && (source_noise_line_layers(source)
            .iter()
            .any(|layer| layer.iter().any(|noise| *noise == normalized))
            || source_count_prefix_layers(source).iter().any(|layer| {
                layer
                    .iter()
                    .any(|prefix| matches_count_prefix_line(&normalized, prefix))
            }))
}

fn matches_count_prefix_line(line: &str, prefix: &str) -> bool {
    let Some(suffix) = line.strip_prefix(prefix) else {
        return false;
    };
    suffix.trim().chars().all(|ch| {
        ch.is_ascii_digit()
            || ch.is_whitespace()
            || matches!(ch, '(' | ')' | '[' | ']' | ':' | '-' | '|' | ',')
    })
}

fn normalize_noise_line(line: &str) -> String {
    line.trim()
        .trim_matches(|ch: char| matches!(ch, '#' | '*' | '_' | ':' | '|' | '-'))
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .to_lowercase()
}

fn is_reference_section(title: &str) -> bool {
    let normalized = title
        .trim()
        .trim_matches(|ch: char| !ch.is_alphanumeric())
        .to_lowercase();
    matches!(
        normalized.as_str(),
        "references"
            | "reference"
            | "bibliography"
            | "literature cited"
            | "cited literature"
            | "参考文献"
    )
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

    #[test]
    fn applies_source_specific_noise_rules() {
        let cleaned = clean_text_for_source("Result\nRecommended articles\nView PDF", "Elsevier");
        assert!(cleaned.contains("Result"));
        assert!(!cleaned.contains("Recommended articles"));
        assert!(!cleaned.contains("View PDF"));
    }

    #[test]
    fn removes_layered_publisher_noise_lines() {
        let cleaned = clean_text_for_source(
            "Result retained\nDownload PDF\nBack to tab navigation\nArticle information",
            "Royal Society of Chemistry",
        );

        assert_eq!(cleaned, "Result retained");
    }

    #[test]
    fn applies_multiple_source_layers() {
        let cleaned = clean_text_for_source(
            "Finding\nPublisher's note\nAccess through your institution\nRights and permissions",
            "Springer Nature",
        );

        assert_eq!(cleaned, "Finding");
    }

    #[test]
    fn keeps_noise_words_inside_real_lines() {
        let cleaned = clean_text_for_source(
            "The interface mentions Download PDF in a methods audit.\nDownload PDF",
            "Springer",
        );

        assert!(cleaned.contains("The interface mentions Download PDF"));
        assert!(!cleaned.lines().any(|line| line == "Download PDF"));
    }

    #[test]
    fn keeps_publisher_labels_inside_real_evidence_lines() {
        let cleaned = clean_text_for_source(
            "The Supporting Information dataset was used for validation.\nSupporting Information",
            "American Chemical Society",
        );

        assert!(cleaned.contains("The Supporting Information dataset"));
        assert!(!cleaned.lines().any(|line| line == "Supporting Information"));
    }

    #[test]
    fn removes_publisher_ui_count_lines_without_dropping_real_sentences() {
        let cleaned = clean_text_for_source(
            "Result retained\nRecommended articles (4)\nCited by (12)\nCited by zeolite catalysts in prior work.",
            "ScienceDirect",
        );

        assert!(cleaned.contains("Result retained"));
        assert!(!cleaned.contains("Recommended articles"));
        assert!(!cleaned.contains("Cited by (12)"));
        assert!(cleaned.contains("Cited by zeolite catalysts in prior work."));
    }

    #[test]
    fn removes_wiley_count_navigation_lines() {
        let cleaned = clean_text_for_source(
            "Finding\nFigures (3)\nSections (7)\nFigures reveal catalyst morphology.",
            "Wiley",
        );

        assert_eq!(cleaned, "Finding\nFigures reveal catalyst morphology.");
    }

    #[test]
    fn removes_reference_sections() {
        let sections = clean_sections(vec![
            Section {
                title: "Results".to_string(),
                level: 2,
                content: "Useful evidence".to_string(),
            },
            Section {
                title: "References".to_string(),
                level: 2,
                content: "[1] cited paper".to_string(),
            },
        ]);

        assert_eq!(sections.len(), 1);
        assert_eq!(sections[0].title, "Results");
    }
}
