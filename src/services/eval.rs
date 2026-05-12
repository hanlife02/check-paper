use std::path::Path;

use anyhow::Result;
use serde_json::Value;

use crate::eval::{EvalReport, eval_report_json, run_golden_eval};
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

    pub fn report_json(&self, report: &EvalReport, include_trace: bool) -> Result<Value> {
        eval_report_json(report, include_trace)
    }
}
