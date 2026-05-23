use super::models::Section;

pub const CLEANER_VERSION: &str = "source-cleaner-v13";

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

const ACS_REPLACEMENTS: &[(&str, &str)] = &[
    ("View Author Information", " "),
    ("Click to copy citationCitation copied!", " "),
    ("Open PDF", " "),
    ("PDFSupporting Information", " Supporting Information "),
    ("ShareShare", "Share"),
    ("toExpandCollapse", " "),
    ("Cite This:", " "),
    ("Cite this:", " "),
    ("Publication History", " "),
    ("research-article", " "),
    ("More by", " "),
];

const ELSEVIER_REPLACEMENTS: &[(&str, &str)] = &[
    ("Author links open overlay panel", " "),
    ("Show more", " "),
    ("Add to Mendeley", " "),
    ("ShareCite[url] rights and content", " "),
    ("Cite[url] rights and content", " "),
    ("Full text access", " "),
    ("Previous article in issue", " "),
    ("Next article in issue", " "),
    ("Recommended articles", " "),
    ("View Abstract", " "),
];

const RSC_REPLACEMENTS: &[(&str, &str)] = &[("Show Compounds", " "), ("Show Chemical Terms", " ")];

const WILEY_REPLACEMENTS: &[(&str, &str)] = &[];

const COMMON_NOISE_LINES: &[&str] = &[
    "advertisement",
    "download pdf",
    "download citation",
    "jump to",
    "metrics",
    "altmetric",
    "crossmark",
    "share",
    "share icon",
    "skip to figshare navigation",
    "skip to main content",
    "view metrics",
];

const COMMON_COUNT_PREFIX_LINES: &[&str] = &["download pdf", "metrics"];

const ACS_NOISE_LINES: &[&str] = &[
    "get e-alerts",
    "add to favorites",
    "supporting information",
    "article views",
];

const ACS_COUNT_PREFIX_LINES: &[&str] = &["article views", "citations"];
const ACS_NOISE_PREFIX_LINES: &[&str] = &[
    "this article is cited by",
    "copyright ©",
    "this publication is licensed under",
    "request reuse permissions.acs publicationscopyright ©",
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
    "show chemical terms",
    "show compounds",
    "social activity",
    "supplementary information",
];

const RSC_NOISE_PREFIX_LINES: &[&str] = &[
    "footnote† electronic supplementary information",
    "footnotes† electronic supplementary information",
    "this journal is © the royal society of chemistry",
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
const WILEY_NOISE_PREFIX_LINES: &[&str] = &["article metrics"];

const SPRINGER_NOISE_LINES: &[&str] = &[
    "about this article",
    "access this article",
    "article metrics",
    "about nature portfolio",
    "nature awards",
    "nature careers",
    "nature index",
    "nature masterclasses",
    "nature portfolio policies",
    "provided by the springer nature sharedit content-sharing initiative",
    "publisher's note",
    "reprints and permissions",
    "rights and permissions",
    "share this article",
    "supplementary information",
];

const SPRINGER_COUNT_PREFIX_LINES: &[&str] = &["about this article", "article metrics"];
const SPRINGER_NOISE_PREFIX_LINES: &[&str] = &[
    "description of additional supplementary files",
    "publisher's note springer nature remains neutral",
    "publisher’s note springer nature remains neutral",
    "springer nature remains neutral with regard",
    "supplementary file",
    "supplementary movie",
    "supplementary video",
];

const NATURE_NOISE_LINES: &[&str] = &[
    "about this article",
    "access through your institution",
    "provided by the springer nature sharedit content-sharing initiative",
    "rights and permissions",
    "share this article",
    "subjects",
    "supplementary information",
];

const NATURE_COUNT_PREFIX_LINES: &[&str] = &["about this article", "subjects"];

const MDPI_NOISE_LINES: &[&str] = &[
    "article versions notes",
    "edit a special issue",
    "for authors",
    "for editors",
    "for librarians",
    "for publishers",
    "for reviewers",
    "for societies",
    "journal browser",
    "review for this journal",
    "share and cite",
    "submit to this journal",
];

const MDPI_COUNT_PREFIX_LINES: &[&str] = &["article metrics", "cited by"];

const PLOS_NOISE_LINES: &[&str] = &[
    "article metrics",
    "citation",
    "figures",
    "media coverage",
    "peer review",
    "reader comments",
    "related content",
];

const PLOS_COUNT_PREFIX_LINES: &[&str] = &["article metrics", "figures"];

const FRONTIERS_NOISE_LINES: &[&str] = &[
    "about frontiers",
    "all articles",
    "download article",
    "download pdf",
    "frontiers in",
    "impact",
    "open access",
    "original research",
    "people also looked at",
    "share on",
    "view article impact",
];

const FRONTIERS_COUNT_PREFIX_LINES: &[&str] = &["citations", "views"];
const FRONTIERS_NOISE_PREFIX_LINES: &[&str] = &[
    "copyright ©",
    "correspondence:",
    "edited by:",
    "published:",
    "received:",
    "reviewed by:",
];

const TAYLOR_FRANCIS_NOISE_LINES: &[&str] = &[
    "advanced search",
    "article metrics",
    "browse journals by subject",
    "figures & data",
    "full article",
    "get access",
    "latest articles",
    "log in | register",
    "most cited",
    "most read",
    "people also read",
    "recommended articles",
    "related research",
    "reprints & permissions",
    "search in:",
    "view author publications",
];

const TAYLOR_FRANCIS_COUNT_PREFIX_LINES: &[&str] = &["altmetric", "citations", "views"];

const IEEE_NOISE_LINES: &[&str] = &[
    "alerts",
    "export to",
    "ieee account",
    "institutional sign in",
    "personal sign in",
    "purchase pdf",
    "related articles",
    "view all authors",
    "view document",
];

const IEEE_COUNT_PREFIX_LINES: &[&str] = &["cited by", "metrics"];

const OXFORD_NOISE_LINES: &[&str] = &[
    "article navigation",
    "email alerts",
    "get help with access",
    "issue section",
    "oxford academic",
    "sign in",
    "supplementary data",
];

const OXFORD_COUNT_PREFIX_LINES: &[&str] = &["citing articles", "views"];

const SAGE_NOISE_LINES: &[&str] = &[
    "access options",
    "article information",
    "figures and tables",
    "get access",
    "information, rights and permissions",
    "metrics and citations",
    "permissions",
    "share options",
    "supplemental material",
    "view all access and purchase options",
];

const SAGE_COUNT_PREFIX_LINES: &[&str] = &["article usage", "citations"];

const CELL_PRESS_NOISE_LINES: &[&str] = &[
    "article info",
    "cell press",
    "figures",
    "graphical abstract",
    "highlights",
    "in brief",
    "open access",
    "preview",
    "related articles",
    "recommended articles",
];

const CELL_PRESS_COUNT_PREFIX_LINES: &[&str] = &["cited by", "metrics"];
const CELL_PRESS_NOISE_PREFIX_LINES: &[&str] = &[
    "copyright ©",
    "published by cell press",
    "published by elsevier",
];

const PNAS_NOISE_LINES: &[&str] = &[
    "article figures & data",
    "alerts",
    "citation",
    "figures",
    "pnas",
    "related content",
    "share",
    "sign up for alerts",
];

const PNAS_COUNT_PREFIX_LINES: &[&str] = &["article metrics", "cited by"];
const PNAS_NOISE_PREFIX_LINES: &[&str] = &[
    "copyright ©",
    "published under the pnas license",
    "published under the pnas license.",
];

const ELIFE_NOISE_LINES: &[&str] = &[
    "article and author information",
    "copy to clipboard",
    "download article",
    "figures and data",
    "metrics",
    "share this article",
];

const ELIFE_COUNT_PREFIX_LINES: &[&str] = &["citations", "downloads", "views"];
const ELIFE_NOISE_PREFIX_LINES: &[&str] = &["copyright ©", "for correspondence:"];

const AAAS_NOISE_LINES: &[&str] = &[
    "advertisement",
    "alerts",
    "article tools",
    "download citation",
    "figures & data",
    "metrics & citations",
    "permissions",
    "related content",
    "share",
    "sign up for alerts",
];

const AAAS_COUNT_PREFIX_LINES: &[&str] = &["cited by", "views"];
const AAAS_NOISE_PREFIX_LINES: &[&str] = &[
    "copyright ©",
    "published by the american association for the advancement of science",
];

const IOP_NOISE_LINES: &[&str] = &[
    "article lookup",
    "download article pdf",
    "export citation",
    "figures",
    "iopscience",
    "metrics",
    "related content",
    "sign in",
];

const IOP_COUNT_PREFIX_LINES: &[&str] = &["downloads", "total citations", "views"];
const IOP_NOISE_PREFIX_LINES: &[&str] = &[
    "published under licence by iop publishing",
    "published under licence by iop publishing ltd",
];

const AIP_NOISE_LINES: &[&str] = &[
    "article navigation",
    "article tools",
    "download pdf",
    "export citation",
    "metrics",
    "related content",
    "scitation",
    "sign in",
    "tools",
];

const AIP_COUNT_PREFIX_LINES: &[&str] = &["cited by", "views"];
const AIP_NOISE_PREFIX_LINES: &[&str] = &["copyright ©", "published by aip publishing"];

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
    cleaned = remove_download_image_fragments(cleaned);
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
    if source.contains("mdpi") {
        layers.push(MDPI_NOISE_LINES);
    }
    if source.contains("plos") || source.contains("public library of science") {
        layers.push(PLOS_NOISE_LINES);
    }
    if source.contains("frontiers") {
        layers.push(FRONTIERS_NOISE_LINES);
    }
    if source.contains("taylor")
        || source.contains("francis")
        || source.contains("tandfonline")
        || source.contains("taylor & francis")
    {
        layers.push(TAYLOR_FRANCIS_NOISE_LINES);
    }
    if source.contains("ieee") || source.contains("xplore") {
        layers.push(IEEE_NOISE_LINES);
    }
    if source.contains("oxford")
        || source.contains("oup")
        || source.contains("academic.oup")
        || source.contains("oxford academic")
    {
        layers.push(OXFORD_NOISE_LINES);
    }
    if source.contains("sage") {
        layers.push(SAGE_NOISE_LINES);
    }
    if source.contains("cell press") || source.contains("cell.com") {
        layers.push(CELL_PRESS_NOISE_LINES);
    }
    if source.contains("pnas") || source.contains("proceedings of the national academy") {
        layers.push(PNAS_NOISE_LINES);
    }
    if source.contains("elife") || source.contains("e-life") {
        layers.push(ELIFE_NOISE_LINES);
    }
    if is_aaas_source(&source) {
        layers.push(AAAS_NOISE_LINES);
    }
    if is_iop_source(&source) {
        layers.push(IOP_NOISE_LINES);
    }
    if is_aip_source(&source) {
        layers.push(AIP_NOISE_LINES);
    }
    layers
}

fn source_count_prefix_layers(source: &str) -> Vec<&'static [&'static str]> {
    let source = source.to_lowercase();
    let mut layers = vec![COMMON_COUNT_PREFIX_LINES];
    if source.contains("elsevier") || source.contains("sciencedirect") {
        layers.push(ELSEVIER_COUNT_PREFIX_LINES);
    }
    if source.contains("acs") || source.contains("american chemical society") {
        layers.push(ACS_COUNT_PREFIX_LINES);
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
    if source.contains("mdpi") {
        layers.push(MDPI_COUNT_PREFIX_LINES);
    }
    if source.contains("plos") || source.contains("public library of science") {
        layers.push(PLOS_COUNT_PREFIX_LINES);
    }
    if source.contains("frontiers") {
        layers.push(FRONTIERS_COUNT_PREFIX_LINES);
    }
    if source.contains("taylor")
        || source.contains("francis")
        || source.contains("tandfonline")
        || source.contains("taylor & francis")
    {
        layers.push(TAYLOR_FRANCIS_COUNT_PREFIX_LINES);
    }
    if source.contains("ieee") || source.contains("xplore") {
        layers.push(IEEE_COUNT_PREFIX_LINES);
    }
    if source.contains("oxford")
        || source.contains("oup")
        || source.contains("academic.oup")
        || source.contains("oxford academic")
    {
        layers.push(OXFORD_COUNT_PREFIX_LINES);
    }
    if source.contains("sage") {
        layers.push(SAGE_COUNT_PREFIX_LINES);
    }
    if source.contains("cell press") || source.contains("cell.com") {
        layers.push(CELL_PRESS_COUNT_PREFIX_LINES);
    }
    if source.contains("pnas") || source.contains("proceedings of the national academy") {
        layers.push(PNAS_COUNT_PREFIX_LINES);
    }
    if source.contains("elife") || source.contains("e-life") {
        layers.push(ELIFE_COUNT_PREFIX_LINES);
    }
    if is_aaas_source(&source) {
        layers.push(AAAS_COUNT_PREFIX_LINES);
    }
    if is_iop_source(&source) {
        layers.push(IOP_COUNT_PREFIX_LINES);
    }
    if is_aip_source(&source) {
        layers.push(AIP_COUNT_PREFIX_LINES);
    }
    layers
}

fn source_noise_prefix_layers(source: &str) -> Vec<&'static [&'static str]> {
    let source = source.to_lowercase();
    let mut layers = Vec::new();
    if source.contains("acs") || source.contains("american chemical society") {
        layers.push(ACS_NOISE_PREFIX_LINES);
    }
    if source.contains("royal society of chemistry")
        || source.contains("rsc publishing")
        || source_has_token(&source, "rsc")
    {
        layers.push(RSC_NOISE_PREFIX_LINES);
    }
    if source.contains("wiley") {
        layers.push(WILEY_NOISE_PREFIX_LINES);
    }
    if source.contains("springer") {
        layers.push(SPRINGER_NOISE_PREFIX_LINES);
    }
    if source.contains("nature") {
        layers.push(SPRINGER_NOISE_PREFIX_LINES);
    }
    if source.contains("frontiers") {
        layers.push(FRONTIERS_NOISE_PREFIX_LINES);
    }
    if source.contains("cell press") || source.contains("cell.com") {
        layers.push(CELL_PRESS_NOISE_PREFIX_LINES);
    }
    if source.contains("pnas") || source.contains("proceedings of the national academy") {
        layers.push(PNAS_NOISE_PREFIX_LINES);
    }
    if source.contains("elife") || source.contains("e-life") {
        layers.push(ELIFE_NOISE_PREFIX_LINES);
    }
    if is_aaas_source(&source) {
        layers.push(AAAS_NOISE_PREFIX_LINES);
    }
    if is_iop_source(&source) {
        layers.push(IOP_NOISE_PREFIX_LINES);
    }
    if is_aip_source(&source) {
        layers.push(AIP_NOISE_PREFIX_LINES);
    }
    layers
}

fn is_aaas_source(source: &str) -> bool {
    source.contains("aaas")
        || source.contains("science.org")
        || source.contains("science magazine")
        || source.contains("science advances")
        || source.contains("science signaling")
        || source.contains("science translational medicine")
        || source.trim() == "science"
}

fn is_iop_source(source: &str) -> bool {
    source.contains("iopscience")
        || source.contains("iop publishing")
        || source.contains("institute of physics")
        || source_has_token(source, "iop")
}

fn is_aip_source(source: &str) -> bool {
    source.contains("aip publishing")
        || source.contains("aip.scitation")
        || source.contains("aip advances")
        || source_has_token(source, "aip")
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

fn remove_download_image_fragments(mut text: String) -> String {
    for prefix in [
        "Download: Download high-res image",
        "Download: Download full-size image",
    ] {
        while let Some(start) = text.find(prefix) {
            let mut end = start + prefix.len();
            if text[end..].starts_with(" (")
                && let Some(close_index) = text[end..].find(')')
            {
                end += close_index + 1;
            }
            text.replace_range(start..end, " ");
        }
    }
    text
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
            })
            || source_noise_prefix_layers(source).iter().any(|layer| {
                layer
                    .iter()
                    .any(|prefix| matches_noise_prefix_line(&normalized, prefix))
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

fn matches_noise_prefix_line(line: &str, prefix: &str) -> bool {
    if !line.starts_with(prefix) {
        return false;
    }
    match prefix {
        "article metrics" => {
            line == prefix
                || line.contains("altmetrics")
                || line.contains("citations (crossref)")
                || line.contains("full text views")
                || line.contains("qr code")
                || line.contains("how to cite")
        }
        _ => true,
    }
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
    fn removes_elsevier_inline_navigation_without_dropping_body_text() {
        let cleaned = clean_text_for_source(
            "Journal of Energy ChemistryAuthor links open overlay panelLei GaoShow moreAdd to MendeleyShareCite[url] rights and contentFull text accessAbstractThe electrolyte improves ionic conductivity.Graphical abstractDownload: Download high-res image (137KB)Download: Download full-size imagePrevious article in issueNext article in issueKeywordsAntiperovskiteRecommended articles1These authors contributed equally.View Abstract",
            "ScienceDirect",
        );

        assert!(cleaned.contains("The electrolyte improves ionic conductivity."));
        assert!(!cleaned.contains("Author links open overlay panel"));
        assert!(!cleaned.contains("Add to Mendeley"));
        assert!(!cleaned.contains("Download: Download"));
        assert!(!cleaned.contains("Previous article in issue"));
        assert!(!cleaned.contains("Recommended articles"));
        assert!(!cleaned.contains("View Abstract"));
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
    fn removes_common_browser_ui_lines_without_dropping_real_sentences() {
        let cleaned = clean_text(
            "Finding retained\nShare Icon\nShare\nView Metrics\nSkip to Main Content\nJump to\nThe authors share catalyst data and view metrics as a bibliometric object.",
        );

        assert!(cleaned.contains("Finding retained"));
        assert!(!cleaned.lines().any(|line| line == "Share Icon"));
        assert!(!cleaned.lines().any(|line| line == "Share"));
        assert!(!cleaned.lines().any(|line| line == "View Metrics"));
        assert!(!cleaned.lines().any(|line| line == "Skip to Main Content"));
        assert!(!cleaned.lines().any(|line| line == "Jump to"));
        assert!(cleaned.contains("The authors share catalyst data"));
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
    fn removes_wiley_article_metrics_ui_without_dropping_real_sentences() {
        let cleaned = clean_text_for_source(
            "Finding retained\nArticle MetricsFull text views:1851Total unique accesses.More metric informationCitations (CrossRef): 11Altmetrics:Scite metricsShare QR CodeHow to cite\nArticle metrics can be used as a research object in bibliometrics.",
            "browser_wiley_full_remaining_text_sliced_mcp_export",
        );

        assert!(cleaned.contains("Finding retained"));
        assert!(!cleaned.contains("Full text views"));
        assert!(!cleaned.contains("How to cite"));
        assert!(cleaned.contains("Article metrics can be used as a research object"));
    }

    #[test]
    fn removes_mdpi_navigation_without_dropping_real_sentences() {
        let cleaned = clean_text_for_source(
            "Finding retained\nArticle Versions Notes\nShare and Cite\nSubmit to this Journal\nArticle Metrics (3)\nCited by (12)\nThe authors cite MDPI datasets when comparing catalyst stability.",
            "MDPI",
        );

        assert!(cleaned.contains("Finding retained"));
        assert!(!cleaned.contains("Article Versions Notes"));
        assert!(!cleaned.contains("Share and Cite"));
        assert!(!cleaned.contains("Submit to this Journal"));
        assert!(!cleaned.contains("Article Metrics (3)"));
        assert!(!cleaned.contains("Cited by (12)"));
        assert!(cleaned.contains("The authors cite MDPI datasets"));
    }

    #[test]
    fn removes_plos_navigation_without_dropping_real_sentences() {
        let cleaned = clean_text_for_source(
            "Finding retained\nFigures\nCitation\nReader Comments\nMedia Coverage\nPeer Review\nRelated Content\nFigures describe the workflow in detail, and citation context remains useful.",
            "Public Library of Science",
        );

        assert!(cleaned.contains("Finding retained"));
        assert!(!cleaned.lines().any(|line| line == "Figures"));
        assert!(!cleaned.lines().any(|line| line == "Citation"));
        assert!(!cleaned.lines().any(|line| line == "Reader Comments"));
        assert!(!cleaned.lines().any(|line| line == "Media Coverage"));
        assert!(!cleaned.lines().any(|line| line == "Peer Review"));
        assert!(!cleaned.lines().any(|line| line == "Related Content"));
        assert!(cleaned.contains("Figures describe the workflow"));
    }

    #[test]
    fn removes_acs_navigation_without_dropping_real_sentences() {
        let cleaned = clean_text_for_source(
            "Finding retained\nArticle Views (123)\nThis article is cited by 262 publications.\nView Author InformationOpen PDFClick to copy citationCitation copied!The catalyst remains stable.\nThe article views adsorption as a mechanistic descriptor.",
            "ACS Publications",
        );

        assert!(cleaned.contains("Finding retained"));
        assert!(!cleaned.contains("Article Views (123)"));
        assert!(!cleaned.contains("This article is cited by"));
        assert!(!cleaned.contains("View Author Information"));
        assert!(!cleaned.contains("Click to copy citation"));
        assert!(cleaned.contains("The catalyst remains stable."));
        assert!(cleaned.contains("The article views adsorption"));
    }

    #[test]
    fn removes_rsc_navigation_without_dropping_real_sentences() {
        let cleaned = clean_text_for_source(
            "Finding retained\nShow CompoundsShow Chemical TermsRationally clicked framework remains selective.\nFootnote† Electronic supplementary information available. See DOI: 10.1039/test\nThis journal is © The Royal Society of Chemistry 2021\nRoyal Society of Chemistry references remain in historical context.",
            "Royal Society of Chemistry",
        );

        assert!(cleaned.contains("Finding retained"));
        assert!(cleaned.contains("Rationally clicked framework remains selective."));
        assert!(!cleaned.contains("Show Compounds"));
        assert!(!cleaned.contains("Show Chemical Terms"));
        assert!(!cleaned.contains("Electronic supplementary information available"));
        assert!(!cleaned.contains("This journal is ©"));
        assert!(cleaned.contains("Royal Society of Chemistry references remain"));
    }

    #[test]
    fn removes_springer_nature_navigation_without_dropping_real_sentences() {
        let cleaned = clean_text_for_source(
            "Finding retained\nSupplementary information\nSupplementary Movie 1 (download MP4 )\nDescription of Additional Supplementary Files (download PDF )\nShare this article\nProvided by the Springer Nature SharedIt content-sharing initiative\nSpringer Nature remains neutral with regard to jurisdictional claims in published maps and institutional affiliations.\nNature Communications remains the source journal in this example.",
            "Springer Nature",
        );

        assert!(cleaned.contains("Finding retained"));
        assert!(
            !cleaned
                .lines()
                .any(|line| line == "Supplementary information")
        );
        assert!(!cleaned.contains("Supplementary Movie"));
        assert!(!cleaned.contains("Description of Additional Supplementary Files"));
        assert!(!cleaned.contains("Share this article"));
        assert!(!cleaned.contains("SharedIt"));
        assert!(!cleaned.contains("Springer Nature remains neutral"));
        assert!(cleaned.contains("Nature Communications remains the source journal"));
    }

    #[test]
    fn removes_frontiers_navigation_without_dropping_real_sentences() {
        let cleaned = clean_text_for_source(
            "Finding retained\nOpen Access\nOriginal Research\nDownload Article\nView article impact\nPeople also looked at\nViews 12,345\nCitations 18\nEdited by: Alice Reviewer\nReviewed by: Bob Reviewer\nPublished: 15 March 2024\nThe catalyst shows open access to reactants inside the porous channel.",
            "Frontiers",
        );

        assert!(cleaned.contains("Finding retained"));
        assert!(!cleaned.lines().any(|line| line == "Open Access"));
        assert!(!cleaned.lines().any(|line| line == "Original Research"));
        assert!(!cleaned.lines().any(|line| line == "Download Article"));
        assert!(!cleaned.contains("View article impact"));
        assert!(!cleaned.contains("People also looked at"));
        assert!(!cleaned.contains("Views 12,345"));
        assert!(!cleaned.contains("Citations 18"));
        assert!(!cleaned.contains("Edited by:"));
        assert!(!cleaned.contains("Reviewed by:"));
        assert!(!cleaned.contains("Published:"));
        assert!(cleaned.contains("open access to reactants"));
    }

    #[test]
    fn removes_taylor_francis_navigation_without_dropping_real_sentences() {
        let cleaned = clean_text_for_source(
            "Finding retained\nLog in | Register\nAdvanced search\nBrowse journals by subject\nFull Article\nFigures & data\nReprints & Permissions\nPeople also read\nRecommended articles\nArticle Metrics\nViews 1,204\nCitations 9\nTaylor dispersion was used as a real transport descriptor.",
            "Taylor & Francis Online",
        );

        assert!(cleaned.contains("Finding retained"));
        assert!(!cleaned.contains("Log in | Register"));
        assert!(!cleaned.contains("Advanced search"));
        assert!(!cleaned.contains("Browse journals"));
        assert!(!cleaned.contains("Full Article"));
        assert!(!cleaned.contains("Figures & data"));
        assert!(!cleaned.contains("Reprints & Permissions"));
        assert!(!cleaned.contains("People also read"));
        assert!(!cleaned.contains("Recommended articles"));
        assert!(!cleaned.contains("Article Metrics"));
        assert!(!cleaned.contains("Views 1,204"));
        assert!(!cleaned.contains("Citations 9"));
        assert!(cleaned.contains("Taylor dispersion"));
    }

    #[test]
    fn removes_ieee_navigation_without_dropping_real_sentences() {
        let cleaned = clean_text_for_source(
            "Finding retained\nIEEE Account\nPersonal Sign In\nInstitutional Sign In\nView All Authors\nPurchase PDF\nExport to\nAlerts\nRelated Articles\nCited By (12)\nIEEE 802.11 was used only as a real protocol reference in the experiment.",
            "IEEE Xplore",
        );

        assert!(cleaned.contains("Finding retained"));
        assert!(!cleaned.contains("IEEE Account"));
        assert!(!cleaned.contains("Personal Sign In"));
        assert!(!cleaned.contains("Institutional Sign In"));
        assert!(!cleaned.contains("View All Authors"));
        assert!(!cleaned.contains("Purchase PDF"));
        assert!(!cleaned.contains("Export to"));
        assert!(!cleaned.lines().any(|line| line == "Alerts"));
        assert!(!cleaned.contains("Related Articles"));
        assert!(!cleaned.contains("Cited By (12)"));
        assert!(cleaned.contains("IEEE 802.11 was used"));
    }

    #[test]
    fn removes_oxford_academic_navigation_without_dropping_real_sentences() {
        let cleaned = clean_text_for_source(
            "Finding retained\nOxford Academic\nArticle Navigation\nIssue Section:\nGet help with access\nEmail alerts\nSupplementary data\nCiting articles 14\nViews 3,451\nOxford nanopore data remained part of the mechanistic evidence.",
            "Oxford Academic",
        );

        assert!(cleaned.contains("Finding retained"));
        assert!(!cleaned.contains("Oxford Academic"));
        assert!(!cleaned.contains("Article Navigation"));
        assert!(!cleaned.contains("Issue Section"));
        assert!(!cleaned.contains("Get help with access"));
        assert!(!cleaned.contains("Email alerts"));
        assert!(!cleaned.contains("Supplementary data"));
        assert!(!cleaned.contains("Citing articles 14"));
        assert!(!cleaned.contains("Views 3,451"));
        assert!(cleaned.contains("Oxford nanopore data"));
    }

    #[test]
    fn removes_sage_navigation_without_dropping_real_sentences() {
        let cleaned = clean_text_for_source(
            "Finding retained\nAccess options\nView all access and purchase options\nMetrics and citations\nFigures and tables\nSupplemental material\nInformation, rights and permissions\nArticle usage 2,184\nCitations 7\nSAGE remains a real acronym in this retained methods sentence.",
            "SAGE Journals",
        );

        assert!(cleaned.contains("Finding retained"));
        assert!(!cleaned.contains("Access options"));
        assert!(!cleaned.contains("View all access and purchase options"));
        assert!(!cleaned.contains("Metrics and citations"));
        assert!(!cleaned.contains("Figures and tables"));
        assert!(!cleaned.contains("Supplemental material"));
        assert!(!cleaned.contains("Information, rights and permissions"));
        assert!(!cleaned.contains("Article usage 2,184"));
        assert!(!cleaned.contains("Citations 7"));
        assert!(cleaned.contains("SAGE remains a real acronym"));
    }

    #[test]
    fn removes_cell_press_navigation_without_dropping_real_sentences() {
        let cleaned = clean_text_for_source(
            "Finding retained\nCell Press\nHighlights\nGraphical abstract\nIn brief\nArticle info\nRecommended articles\nCited by (18)\nPublished by Cell Press\nThe cell press protocol compresses the catalyst pellet in a real method sentence.",
            "Cell Press",
        );

        assert!(cleaned.contains("Finding retained"));
        assert!(!cleaned.lines().any(|line| line == "Cell Press"));
        assert!(!cleaned.lines().any(|line| line == "Highlights"));
        assert!(!cleaned.lines().any(|line| line == "Graphical abstract"));
        assert!(!cleaned.lines().any(|line| line == "In brief"));
        assert!(!cleaned.lines().any(|line| line == "Article info"));
        assert!(!cleaned.contains("Recommended articles"));
        assert!(!cleaned.contains("Cited by (18)"));
        assert!(!cleaned.contains("Published by Cell Press"));
        assert!(cleaned.contains("The cell press protocol compresses"));
    }

    #[test]
    fn removes_pnas_navigation_without_dropping_real_sentences() {
        let cleaned = clean_text_for_source(
            "Finding retained\nPNAS\nArticle Figures & Data\nCitation\nRelated Content\nSign up for alerts\nArticle Metrics (42)\nCited by 11\nPublished under the PNAS license.\nThe PNAS dataset label is retained inside this evidence sentence.",
            "Proceedings of the National Academy of Sciences",
        );

        assert!(cleaned.contains("Finding retained"));
        assert!(!cleaned.lines().any(|line| line == "PNAS"));
        assert!(!cleaned.contains("Article Figures & Data"));
        assert!(!cleaned.lines().any(|line| line == "Citation"));
        assert!(!cleaned.contains("Related Content"));
        assert!(!cleaned.contains("Sign up for alerts"));
        assert!(!cleaned.contains("Article Metrics (42)"));
        assert!(!cleaned.contains("Cited by 11"));
        assert!(!cleaned.contains("Published under the PNAS license"));
        assert!(cleaned.contains("The PNAS dataset label is retained"));
    }

    #[test]
    fn removes_elife_navigation_without_dropping_real_sentences() {
        let cleaned = clean_text_for_source(
            "Finding retained\nDownload Article\nFigures and data\nMetrics\nShare this article\nCopy to clipboard\nArticle and author information\nViews 3,201\nCitations 14\nFor correspondence: alice@example.edu\nThe eLife review process is discussed as a real study object.",
            "eLife",
        );

        assert!(cleaned.contains("Finding retained"));
        assert!(!cleaned.contains("Download Article"));
        assert!(!cleaned.contains("Figures and data"));
        assert!(!cleaned.lines().any(|line| line == "Metrics"));
        assert!(!cleaned.contains("Share this article"));
        assert!(!cleaned.contains("Copy to clipboard"));
        assert!(!cleaned.contains("Article and author information"));
        assert!(!cleaned.contains("Views 3,201"));
        assert!(!cleaned.contains("Citations 14"));
        assert!(!cleaned.contains("For correspondence:"));
        assert!(cleaned.contains("The eLife review process is discussed"));
    }

    #[test]
    fn removes_aaas_navigation_without_dropping_real_sentences() {
        let cleaned = clean_text_for_source(
            "Finding retained\nFigures & Data\nMetrics & Citations\nArticle Tools\nRelated Content\nSign up for alerts\nCited by 23\nPublished by the American Association for the Advancement of Science\nScience denitrification remains a real mechanism phrase in this retained sentence.",
            "Science Advances",
        );

        assert!(cleaned.contains("Finding retained"));
        assert!(!cleaned.contains("Figures & Data"));
        assert!(!cleaned.contains("Metrics & Citations"));
        assert!(!cleaned.contains("Article Tools"));
        assert!(!cleaned.contains("Related Content"));
        assert!(!cleaned.contains("Sign up for alerts"));
        assert!(!cleaned.contains("Cited by 23"));
        assert!(!cleaned.contains("Published by the American Association"));
        assert!(cleaned.contains("Science denitrification remains"));
    }

    #[test]
    fn removes_iop_navigation_without_dropping_real_sentences() {
        let cleaned = clean_text_for_source(
            "Finding retained\nIOPscience\nArticle Lookup\nDownload article PDF\nExport citation\nFigures\nMetrics\nRelated content\nViews 1,234\nTotal citations 9\nPublished under licence by IOP Publishing Ltd\nThe IOP gate voltage remains part of the retained methods sentence.",
            "IOPscience",
        );

        assert!(cleaned.contains("Finding retained"));
        assert!(!cleaned.lines().any(|line| line == "IOPscience"));
        assert!(!cleaned.contains("Article Lookup"));
        assert!(!cleaned.contains("Download article PDF"));
        assert!(!cleaned.contains("Export citation"));
        assert!(!cleaned.lines().any(|line| line == "Figures"));
        assert!(!cleaned.lines().any(|line| line == "Metrics"));
        assert!(!cleaned.contains("Related content"));
        assert!(!cleaned.contains("Views 1,234"));
        assert!(!cleaned.contains("Total citations 9"));
        assert!(!cleaned.contains("Published under licence"));
        assert!(cleaned.contains("The IOP gate voltage remains"));
    }

    #[test]
    fn removes_aip_navigation_without_dropping_real_sentences() {
        let cleaned = clean_text_for_source(
            "Finding retained\nScitation\nArticle Navigation\nArticle Tools\nDownload PDF\nExport Citation\nRelated Content\nCited by (6)\nPublished by AIP Publishing\nThe AIP acronym remains part of the retained physics sentence.",
            "AIP Publishing",
        );

        assert!(cleaned.contains("Finding retained"));
        assert!(!cleaned.lines().any(|line| line == "Scitation"));
        assert!(!cleaned.contains("Article Navigation"));
        assert!(!cleaned.contains("Article Tools"));
        assert!(!cleaned.contains("Download PDF"));
        assert!(!cleaned.contains("Export Citation"));
        assert!(!cleaned.contains("Related Content"));
        assert!(!cleaned.contains("Cited by (6)"));
        assert!(!cleaned.contains("Published by AIP Publishing"));
        assert!(cleaned.contains("The AIP acronym remains"));
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
