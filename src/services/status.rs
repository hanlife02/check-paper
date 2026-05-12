use anyhow::Result;

use crate::services::jobs::JobService;
use crate::storage::{AnalysisJobSummary, LibraryStatus, Storage};

pub struct StatusReport {
    pub status: LibraryStatus,
    pub failed_jobs: Vec<AnalysisJobSummary>,
}

pub struct StatusService<'a> {
    storage: &'a Storage,
}

impl<'a> StatusService<'a> {
    pub fn new(storage: &'a Storage) -> Self {
        Self { storage }
    }

    pub fn summary(&self, author: Option<&str>) -> Result<LibraryStatus> {
        self.storage.library_status(author)
    }

    pub fn report(
        &self,
        author: Option<&str>,
        include_failed_jobs: bool,
        failed_job_limit: usize,
    ) -> Result<StatusReport> {
        let status = self.summary(author)?;
        let failed_jobs = if include_failed_jobs {
            JobService::new(self.storage).list(author, Some("failed"), failed_job_limit)?
        } else {
            Vec::new()
        };
        Ok(StatusReport {
            status,
            failed_jobs,
        })
    }
}

#[cfg(test)]
mod tests {
    use tempfile::tempdir;

    use super::StatusService;
    use crate::storage::Storage;

    #[test]
    fn report_includes_failed_jobs_when_requested() {
        let dir = tempdir().unwrap();
        let storage = Storage::open(&dir.path().join("test.sqlite")).unwrap();
        storage
            .record_analysis_job("Alice/paper-a", "analyze", "failed", Some("boom"))
            .unwrap();
        let report = StatusService::new(&storage)
            .report(Some("Alice"), true, 5)
            .unwrap();

        assert_eq!(report.status.failed_jobs, 1);
        assert_eq!(report.failed_jobs.len(), 1);
    }
}
