use std::path::Path;

use anyhow::Result;

use crate::papers::loader::load_paper;
use crate::papers::scanner::scan_paper_dirs;
use crate::retrieval::chunker::chunk_paper;
use crate::services::analysis::{AnalysisQueueOptions, AnalysisQueuePlan, AnalysisService};
use crate::storage::Storage;

#[derive(Debug, Clone)]
pub struct SyncRunOptions<'a> {
    pub paper_root: &'a Path,
    pub author: &'a str,
    pub limit: Option<usize>,
    pub chunk_max_chars: usize,
    pub chunk_overlap: usize,
    pub analysis: AnalysisQueueOptions<'a>,
}

#[derive(Debug, Clone)]
pub struct SyncRunReport {
    pub paper_dirs: usize,
    pub ingested: usize,
    pub changed: usize,
    pub analysis: AnalysisQueuePlan,
}

pub struct SyncService<'a> {
    storage: &'a mut Storage,
}

impl<'a> SyncService<'a> {
    pub fn new(storage: &'a mut Storage) -> Self {
        Self { storage }
    }

    pub fn sync_author(&mut self, options: SyncRunOptions<'_>) -> Result<SyncRunReport> {
        let mut paper_dirs = scan_paper_dirs(options.paper_root, Some(options.author))?;
        let paper_dirs_count = paper_dirs.len();
        if let Some(limit) = options.limit {
            paper_dirs.truncate(limit);
        }
        let mut changed = 0usize;
        for paper_dir in &paper_dirs {
            let paper = load_paper(options.paper_root, paper_dir)?;
            let chunks = chunk_paper(&paper, options.chunk_max_chars, options.chunk_overlap);
            if self.storage.upsert_paper_with_chunker(
                &paper,
                &chunks,
                options.analysis.chunker_version,
                options.chunk_max_chars,
                options.chunk_overlap,
            )? {
                changed += 1;
            }
        }
        let analysis =
            AnalysisService::new(self.storage).enqueue_author(options.author, options.analysis)?;
        Ok(SyncRunReport {
            paper_dirs: paper_dirs_count,
            ingested: paper_dirs.len(),
            changed,
            analysis,
        })
    }
}

#[cfg(test)]
mod tests {
    use tempfile::tempdir;

    use super::{SyncRunOptions, SyncService};
    use crate::services::analysis::AnalysisQueueOptions;
    use crate::storage::Storage;

    #[test]
    fn sync_author_ingests_papers_and_queues_analysis() {
        let dir = tempdir().unwrap();
        let paper_root = dir.path().join("paper");
        write_article(&paper_root, "Alice", "paper-a");
        let mut storage = Storage::open(&dir.path().join("test.sqlite")).unwrap();

        let report = SyncService::new(&mut storage)
            .sync_author(options(&paper_root, "Alice", Some(1)))
            .unwrap();

        assert_eq!(report.paper_dirs, 1);
        assert_eq!(report.ingested, 1);
        assert_eq!(report.changed, 1);
        assert_eq!(report.analysis.candidates.len(), 1);
        assert_eq!(report.analysis.queued, 1);
        assert_eq!(
            storage
                .analysis_jobs(Some("Alice"), Some("queued"), 10)
                .unwrap()
                .len(),
            1
        );
    }

    fn options<'a>(
        paper_root: &'a std::path::Path,
        author: &'a str,
        limit: Option<usize>,
    ) -> SyncRunOptions<'a> {
        SyncRunOptions {
            paper_root,
            author,
            limit,
            chunk_max_chars: 3200,
            chunk_overlap: 350,
            analysis: AnalysisQueueOptions {
                failed_only: false,
                stale_only: false,
                force: false,
                limit,
                max_attempts: 3,
                model_id: "test-model",
                chunker_version: "section-char-v1",
            },
        }
    }

    fn write_article(paper_root: &std::path::Path, author: &str, paper_id: &str) {
        let paper_dir = paper_root.join(author).join(paper_id);
        std::fs::create_dir_all(&paper_dir).unwrap();
        std::fs::write(
            paper_dir.join("article.md"),
            r#"---
title: "A Paper"
year: "2024"
---
# Abstract
This paper studies MOF catalysis.

## Methods
The method uses solvent screening.
"#,
        )
        .unwrap();
    }
}
