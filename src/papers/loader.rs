use std::fs;
use std::path::Path;

use anyhow::{Context, Result};
use serde_json::Value;
use sha2::{Digest, Sha256};

use super::cleaner::{clean_sections, clean_text, select_primary_body};
use super::models::Paper;
use super::parser::{parse_frontmatter, parse_sections};

pub fn load_paper(paper_root: &Path, paper_dir: &Path) -> Result<Paper> {
    let article_path = paper_dir.join("article.md");
    let fetch_result_path = paper_dir.join("fetch-result.json");
    let article_text = fs::read_to_string(&article_path)
        .with_context(|| format!("failed to read {}", article_path.display()))?;
    let fetch_result = if fetch_result_path.exists() {
        read_json(&fetch_result_path)?
    } else {
        Value::Object(Default::default())
    };

    let (metadata, raw_body) = parse_frontmatter(&article_text);
    let primary_body = select_primary_body(&raw_body);
    let cleaned_body = clean_text(&primary_body);
    let sections = clean_sections(parse_sections(&cleaned_body));

    Ok(Paper {
        author: author_from_root(paper_root, paper_dir),
        paper_id: paper_dir
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or_default()
            .to_string(),
        paper_dir: paper_dir.to_path_buf(),
        article_path: article_path.clone(),
        fetch_result_path: fetch_result_path
            .exists()
            .then_some(fetch_result_path.clone()),
        source_hash: source_hash(
            &article_path,
            fetch_result_path
                .exists()
                .then_some(fetch_result_path.as_path()),
        )?,
        metadata,
        fetch_result,
        raw_body,
        clean_text: cleaned_body,
        sections,
    })
}

fn read_json(path: &Path) -> Result<Value> {
    let text =
        fs::read_to_string(path).with_context(|| format!("failed to read {}", path.display()))?;
    Ok(serde_json::from_str(&text).unwrap_or(Value::Object(Default::default())))
}

fn source_hash(article_path: &Path, fetch_result_path: Option<&Path>) -> Result<String> {
    let mut digest = Sha256::new();
    digest.update(fs::read(article_path)?);
    if let Some(path) = fetch_result_path {
        digest.update(fs::read(path)?);
    }
    Ok(format!("{:x}", digest.finalize()))
}

fn author_from_root(paper_root: &Path, paper_dir: &Path) -> String {
    paper_dir
        .strip_prefix(paper_root)
        .ok()
        .and_then(|relative| relative.components().next())
        .and_then(|component| component.as_os_str().to_str())
        .map(str::to_string)
        .or_else(|| {
            paper_dir
                .parent()
                .and_then(Path::file_name)
                .and_then(|name| name.to_str())
                .map(str::to_string)
        })
        .unwrap_or_default()
}
