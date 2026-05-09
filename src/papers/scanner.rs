use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};

pub fn scan_paper_dirs(paper_root: &Path, author: Option<&str>) -> Result<Vec<PathBuf>> {
    if !paper_root.exists() {
        return Ok(Vec::new());
    }

    let author_dirs = if let Some(author) = author {
        vec![paper_root.join(author)]
    } else {
        sorted_dirs(paper_root)?
    };

    let mut paper_dirs = Vec::new();
    for author_dir in author_dirs {
        if !author_dir.is_dir() {
            continue;
        }
        for candidate in sorted_dirs(&author_dir)? {
            if candidate.join("article.md").exists() {
                paper_dirs.push(candidate);
            }
        }
    }
    Ok(paper_dirs)
}

fn sorted_dirs(path: &Path) -> Result<Vec<PathBuf>> {
    let mut dirs = Vec::new();
    for entry in fs::read_dir(path).with_context(|| format!("failed to read {}", path.display()))? {
        let entry = entry?;
        let path = entry.path();
        if path.is_dir() {
            dirs.push(path);
        }
    }
    dirs.sort();
    Ok(dirs)
}
