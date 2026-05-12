use anyhow::Result;

use crate::storage::{AnalysisJobSummary, Storage};

pub struct JobService<'a> {
    storage: &'a Storage,
}

impl<'a> JobService<'a> {
    pub fn new(storage: &'a Storage) -> Self {
        Self { storage }
    }

    pub fn list(
        &self,
        author: Option<&str>,
        status: Option<&str>,
        limit: usize,
    ) -> Result<Vec<AnalysisJobSummary>> {
        self.storage.analysis_jobs(author, status, limit)
    }

    pub fn cancel(&self, job_id: i64) -> Result<()> {
        self.storage.cancel_analysis_job(job_id)
    }

    pub fn retry_failed(&self, author: Option<&str>) -> Result<usize> {
        self.storage.retry_failed_analysis_jobs(author)
    }

    pub fn error_counts(
        &self,
        author: Option<&str>,
        status: Option<&str>,
    ) -> Result<Vec<(String, i64)>> {
        self.storage.analysis_job_error_counts(author, status)
    }
}

#[cfg(test)]
mod tests {
    use tempfile::tempdir;

    use super::JobService;
    use crate::storage::Storage;

    #[test]
    fn lists_and_cancels_jobs() {
        let dir = tempdir().unwrap();
        let storage = Storage::open(&dir.path().join("test.sqlite")).unwrap();
        storage
            .record_analysis_job("Alice/paper-a", "analyze", "failed", Some("boom"))
            .unwrap();
        let service = JobService::new(&storage);

        let jobs = service.list(Some("Alice"), Some("failed"), 10).unwrap();
        assert_eq!(jobs.len(), 1);

        service.cancel(jobs[0].id).unwrap();
        let cancelled = service.list(Some("Alice"), Some("cancelled"), 10).unwrap();
        assert_eq!(cancelled.len(), 1);
    }
}
