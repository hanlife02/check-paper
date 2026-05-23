use std::path::Path;

use anyhow::Result;
use serde_json::Value;

use crate::eval::{
    EvalReport, eval_report_json, run_golden_eval, run_golden_eval_with_profile_version,
};
use crate::qa::answerer::QaProfileVersion;
use crate::storage::Storage;

pub struct EvalService<'a> {
    storage: &'a Storage,
}

impl<'a> EvalService<'a> {
    pub fn new(storage: &'a Storage) -> Self {
        Self { storage }
    }

    pub fn run_golden(&self, fixture: &Path, top_k: usize) -> Result<EvalReport> {
        run_golden_eval(self.storage, fixture, top_k)
    }

    pub fn run_golden_with_profile_version(
        &self,
        fixture: &Path,
        top_k: usize,
        profile_version: QaProfileVersion,
    ) -> Result<EvalReport> {
        run_golden_eval_with_profile_version(self.storage, fixture, top_k, profile_version)
    }

    pub fn report_json(&self, report: &EvalReport, include_trace: bool) -> Result<Value> {
        eval_report_json(report, include_trace)
    }
}
