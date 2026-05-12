use std::fs;
use std::path::Path;

use anyhow::{Context, Result, anyhow};
use serde_json::Value;
use sha2::{Digest, Sha256};

use super::cleaner::{
    CLEANER_VERSION, clean_sections_for_source, clean_text_for_source, select_primary_body,
};
use super::models::Paper;
use super::parser::{PARSER_VERSION, parse_frontmatter, parse_sections};

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
    let source = source_from_metadata(&metadata, &fetch_result);
    let primary_body = select_primary_body(&raw_body);
    let cleaned_body = clean_text_for_source(&primary_body, &source);
    let sections = clean_sections_for_source(parse_sections(&cleaned_body), &source);

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
    let value: Value = serde_json::from_str(&text)
        .with_context(|| format!("failed to parse JSON from {}", path.display()))?;
    validate_fetch_result(&value).with_context(|| format!("invalid {}", path.display()))?;
    Ok(value)
}

fn validate_fetch_result(value: &Value) -> Result<()> {
    let Some(object) = value.as_object() else {
        return Err(anyhow!("fetch-result.json must be a JSON object"));
    };
    for key in ["title", "doi", "year", "source"] {
        if let Some(value) = object.get(key) {
            if !(value.is_string() || value.is_number() || value.is_null()) {
                return Err(anyhow!("field `{key}` must be a scalar"));
            }
        }
    }
    Ok(())
}

fn source_from_metadata(
    metadata: &std::collections::BTreeMap<String, String>,
    fetch_result: &Value,
) -> String {
    metadata
        .get("source")
        .cloned()
        .or_else(|| {
            fetch_result
                .get("source")
                .and_then(Value::as_str)
                .map(str::to_string)
        })
        .unwrap_or_default()
}

fn source_hash(article_path: &Path, fetch_result_path: Option<&Path>) -> Result<String> {
    source_hash_with_versions(
        article_path,
        fetch_result_path,
        PARSER_VERSION,
        CLEANER_VERSION,
    )
}

fn source_hash_with_versions(
    article_path: &Path,
    fetch_result_path: Option<&Path>,
    parser_version: &str,
    cleaner_version: &str,
) -> Result<String> {
    let mut digest = Sha256::new();
    digest.update(format!("parser:{parser_version}\ncleaner:{cleaner_version}\n").as_bytes());
    digest.update(fs::read(article_path)?);
    if let Some(path) = fetch_result_path {
        digest.update(fs::read(path)?);
    }
    Ok(format!("{:x}", digest.finalize()))
}

#[cfg(test)]
mod tests {
    use serde_json::json;
    use tempfile::tempdir;

    use super::{source_hash_with_versions, validate_fetch_result};

    #[test]
    fn validates_fetch_result_shape() {
        validate_fetch_result(&json!({"title": "A Paper", "year": 2024})).unwrap();
        assert!(validate_fetch_result(&json!([])).is_err());
        assert!(validate_fetch_result(&json!({"doi": ["10.1/test"]})).is_err());
    }

    #[test]
    fn source_hash_includes_parser_and_cleaner_versions() {
        let dir = tempdir().unwrap();
        let article_path = dir.path().join("article.md");
        std::fs::write(&article_path, "# Article\nBody").unwrap();

        let base = source_hash_with_versions(&article_path, None, "parser-a", "cleaner-a").unwrap();
        let parser_changed =
            source_hash_with_versions(&article_path, None, "parser-b", "cleaner-a").unwrap();
        let cleaner_changed =
            source_hash_with_versions(&article_path, None, "parser-a", "cleaner-b").unwrap();

        assert_ne!(base, parser_changed);
        assert_ne!(base, cleaner_changed);
    }
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
