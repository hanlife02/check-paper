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

#[cfg(test)]
mod tests {
    use tempfile::tempdir;

    use super::scan_paper_dirs;

    #[test]
    fn scans_only_article_directories_and_respects_author_filter() {
        let dir = tempdir().unwrap();
        let root = dir.path().join("paper");
        std::fs::create_dir_all(root.join("Alice").join("paper-b")).unwrap();
        std::fs::create_dir_all(root.join("Alice").join("paper-a")).unwrap();
        std::fs::create_dir_all(root.join("Alice").join("notes")).unwrap();
        std::fs::create_dir_all(root.join("Bob").join("paper-c")).unwrap();
        std::fs::write(root.join("Alice").join("paper-b").join("article.md"), "B").unwrap();
        std::fs::write(root.join("Alice").join("paper-a").join("article.md"), "A").unwrap();
        std::fs::write(root.join("Bob").join("paper-c").join("article.md"), "C").unwrap();

        let all = scan_paper_dirs(&root, None).unwrap();
        let alice = scan_paper_dirs(&root, Some("Alice")).unwrap();

        assert_eq!(all.len(), 3);
        assert_eq!(alice.len(), 2);
        assert!(alice[0].ends_with("paper-a"));
        assert!(alice[1].ends_with("paper-b"));
        assert!(!alice.iter().any(|path| path.ends_with("notes")));
    }

    #[test]
    fn missing_root_scans_as_empty_library() {
        let dir = tempdir().unwrap();
        let rows = scan_paper_dirs(&dir.path().join("missing"), None).unwrap();

        assert!(rows.is_empty());
    }
}
