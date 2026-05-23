use std::collections::{BTreeMap, BTreeSet};
use std::time::Instant;

use anyhow::{Context, Result, anyhow};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::qa::answerer::QaProfileVersion;
use crate::qa::planner::{QaRoutePlan, plan_qa_route};
use crate::retrieval::hybrid;
use crate::retrieval::profile_route::{
    profile_grounding_chunks, profile_grounding_chunks_matching_terms, rank_profiles,
};
use crate::retrieval::query::query_terms;
use crate::schemas::qa_answer::QaAnswerV1;
use crate::storage::{SourceChunk, Storage};

#[derive(Debug, Deserialize)]
pub struct GoldenQuestion {
    pub author: String,
    pub question: String,
    #[serde(default)]
    pub must_cite: Vec<String>,
    #[serde(default)]
    pub must_include: Vec<String>,
    #[serde(default)]
    pub must_not_include: Vec<String>,
    #[serde(default)]
    pub must_answer_include: Vec<String>,
    #[serde(default)]
    pub must_evidence_cite: Vec<String>,
    #[serde(default)]
    pub answer_json: Option<Value>,
}

#[derive(Debug, Serialize)]
pub struct EvalReport {
    pub qa_profile_version: String,
    pub total: usize,
    pub retrieval_hit_at_k: f64,
    pub qa_mode_summary: BTreeMap<String, EvalQaModeSummary>,
    pub route_hit_at_k: BTreeMap<String, f64>,
    pub route_candidate_count_avg: BTreeMap<String, f64>,
    pub citation_precision: f64,
    pub answer_contains_required: f64,
    pub insufficient_when_missing: f64,
    pub latency_ms: u128,
    pub cases: Vec<EvalCaseReport>,
}

#[derive(Debug, Serialize)]
pub struct EvalQaModeSummary {
    pub total: usize,
    pub retrieval_hit_at_k: f64,
    pub citation_precision: f64,
    pub answer_contains_required: f64,
    pub route_reasons: BTreeMap<String, usize>,
}

#[derive(Debug, Serialize)]
pub struct EvalCaseReport {
    pub author: String,
    pub question: String,
    pub qa_profile_version: String,
    pub qa_mode: String,
    pub route_reason: String,
    pub retrieved: Vec<String>,
    pub retrieval_hit: bool,
    pub route_hits: BTreeMap<String, bool>,
    pub citation_precision: f64,
    pub answer_contains_required: bool,
    pub insufficient_when_missing: bool,
    pub latency_ms: u128,
    pub missing_required_terms: Vec<String>,
    pub forbidden_terms_found: Vec<String>,
    pub retrieval_trace: Value,
    pub answer_checked: bool,
    pub answer_contains_expected_terms: bool,
    pub answer_missing_expected_terms: Vec<String>,
    pub answer_evidence_valid: bool,
    pub answer_evidence_citation_precision: f64,
    pub answer_validation_error: String,
}

#[derive(Debug, Clone, Copy, Serialize)]
pub struct EvalComparisonThresholds {
    pub max_retrieval_hit_drop: f64,
    pub max_citation_precision_drop: f64,
    pub max_answer_contains_required_drop: f64,
    pub min_candidate_retrieval_hit_at_k: f64,
    pub min_candidate_citation_precision: f64,
    pub min_candidate_answer_contains_required: f64,
}

impl Default for EvalComparisonThresholds {
    fn default() -> Self {
        Self {
            max_retrieval_hit_drop: 0.0,
            max_citation_precision_drop: 0.02,
            max_answer_contains_required_drop: 0.0,
            min_candidate_retrieval_hit_at_k: 1.0,
            min_candidate_citation_precision: 0.4,
            min_candidate_answer_contains_required: 1.0,
        }
    }
}

#[derive(Debug, Serialize)]
pub struct EvalMetricComparison {
    pub metric: String,
    pub baseline: f64,
    pub candidate: f64,
    pub delta: f64,
    pub max_allowed_drop: f64,
    pub min_required_candidate: f64,
    pub passed: bool,
}

#[derive(Debug, Serialize)]
pub struct EvalComparisonReport {
    pub baseline_profile_version: String,
    pub candidate_profile_version: String,
    pub baseline_total: usize,
    pub candidate_total: usize,
    pub thresholds: EvalComparisonThresholds,
    pub metrics: Vec<EvalMetricComparison>,
    pub metric_gate_pass: bool,
    pub default_switch_recommendation: String,
    pub blockers: Vec<String>,
}

pub fn run_golden_eval(
    storage: &Storage,
    fixture_path: &std::path::Path,
    k: usize,
) -> Result<EvalReport> {
    run_golden_eval_with_profile_version(storage, fixture_path, k, QaProfileVersion::V1)
}

pub fn run_golden_eval_with_profile_version(
    storage: &Storage,
    fixture_path: &std::path::Path,
    k: usize,
    profile_version: QaProfileVersion,
) -> Result<EvalReport> {
    let text = std::fs::read_to_string(fixture_path)
        .with_context(|| format!("failed to read {}", fixture_path.display()))?;
    let questions: Vec<GoldenQuestion> = serde_json::from_str(&text)
        .with_context(|| format!("invalid golden fixture {}", fixture_path.display()))?;
    let started = Instant::now();
    let mut cases = Vec::new();
    for question in questions {
        let case_started = Instant::now();
        let profiles = eval_profile_context(
            storage,
            &question.author,
            &question.question,
            profile_version,
        )?;
        let route_plan = plan_qa_route(&question.question, profiles.len());
        let eval_context = eval_context_chunks(
            storage,
            &question.author,
            &question.question,
            k,
            &profiles,
            route_plan,
        )?;
        let chunks = eval_context.chunks;
        let mut retrieval_trace = eval_context.retrieval_trace;
        annotate_eval_profile_version(&mut retrieval_trace, profile_version);
        let retrieved = chunks
            .iter()
            .map(|chunk| chunk.paper_key.clone())
            .collect::<Vec<_>>();
        let retrieval_hit = question.must_cite.is_empty()
            || question
                .must_cite
                .iter()
                .all(|paper_key| retrieved.iter().any(|item| item == paper_key));
        let route_hits = route_hits(&retrieval_trace, &question.must_cite);
        let citation_precision = if retrieved.is_empty() || question.must_cite.is_empty() {
            if retrieved.is_empty() { 0.0 } else { 1.0 }
        } else {
            let cited = retrieved
                .iter()
                .filter(|paper_key| question.must_cite.iter().any(|must| must == *paper_key))
                .count();
            cited as f64 / retrieved.len() as f64
        };
        let evidence_text = chunks
            .iter()
            .filter(|chunk| {
                question.must_cite.is_empty()
                    || question
                        .must_cite
                        .iter()
                        .any(|paper_key| paper_key == &chunk.paper_key)
            })
            .map(|chunk| chunk.text.as_str())
            .collect::<Vec<_>>()
            .join("\n")
            .to_lowercase();
        let missing_required_terms = question
            .must_include
            .iter()
            .filter(|term| !evidence_text.contains(&term.to_lowercase()))
            .cloned()
            .collect::<Vec<_>>();
        let forbidden_terms_found = question
            .must_not_include
            .iter()
            .filter(|term| evidence_text.contains(&term.to_lowercase()))
            .cloned()
            .collect::<Vec<_>>();
        let answer_contains_required = missing_required_terms.is_empty();
        let insufficient_when_missing = !retrieval_hit || answer_contains_required;
        let answer_eval = evaluate_answer(&question, &chunks);
        cases.push(EvalCaseReport {
            author: question.author,
            question: question.question,
            qa_profile_version: profile_version.as_str().to_string(),
            qa_mode: eval_context.qa_mode,
            route_reason: eval_context.route_reason,
            retrieved,
            retrieval_hit,
            route_hits,
            citation_precision,
            answer_contains_required,
            insufficient_when_missing,
            latency_ms: case_started.elapsed().as_millis(),
            missing_required_terms,
            forbidden_terms_found,
            retrieval_trace,
            answer_checked: answer_eval.checked,
            answer_contains_expected_terms: answer_eval.contains_expected_terms,
            answer_missing_expected_terms: answer_eval.missing_expected_terms,
            answer_evidence_valid: answer_eval.evidence_valid,
            answer_evidence_citation_precision: answer_eval.evidence_citation_precision,
            answer_validation_error: answer_eval.validation_error,
        });
    }
    let total = cases.len();
    let hits = cases.iter().filter(|case| case.retrieval_hit).count();
    let qa_mode_summary = qa_mode_summary(&cases);
    let route_hit_at_k = route_hit_rates(&cases);
    let route_candidate_count_avg = route_candidate_count_averages(&cases);
    let citation_precision_sum = cases
        .iter()
        .map(|case| case.citation_precision)
        .sum::<f64>();
    let contains_required = cases
        .iter()
        .filter(|case| case.answer_contains_required)
        .count();
    let insufficient = cases
        .iter()
        .filter(|case| case.insufficient_when_missing)
        .count();
    Ok(EvalReport {
        qa_profile_version: profile_version.as_str().to_string(),
        total,
        retrieval_hit_at_k: if total == 0 {
            0.0
        } else {
            hits as f64 / total as f64
        },
        qa_mode_summary,
        route_hit_at_k,
        route_candidate_count_avg,
        citation_precision: if total == 0 {
            0.0
        } else {
            citation_precision_sum / total as f64
        },
        answer_contains_required: if total == 0 {
            0.0
        } else {
            contains_required as f64 / total as f64
        },
        insufficient_when_missing: if total == 0 {
            0.0
        } else {
            insufficient as f64 / total as f64
        },
        latency_ms: started.elapsed().as_millis(),
        cases,
    })
}

const EVAL_PROFILE_CONTEXT_LIMIT: usize = 8;
const EVAL_SOURCE_PROFILE_GROUNDING_RESERVE: usize = 2;

struct EvalContextChunks {
    chunks: Vec<SourceChunk>,
    retrieval_trace: Value,
    qa_mode: String,
    route_reason: String,
}

fn eval_profile_context(
    storage: &Storage,
    author: &str,
    question: &str,
    profile_version: QaProfileVersion,
) -> Result<Vec<Value>> {
    let profiles = match profile_version {
        QaProfileVersion::V1 => storage.paper_profiles(author, None)?,
        QaProfileVersion::V2 => {
            let records = storage.paper_profiles_v2_for_author(author, None)?;
            if records.is_empty() {
                return Err(anyhow!(
                    "no V2 paper profiles for {author}; run `ppc comprehend --author {} --v2` first",
                    quote_profile_author(author)
                ));
            }
            records
                .into_iter()
                .map(|record| record.profile_json)
                .collect()
        }
    };
    Ok(rank_profiles(
        profiles,
        &query_terms(question),
        EVAL_PROFILE_CONTEXT_LIMIT,
    ))
}

fn eval_context_chunks(
    storage: &Storage,
    author: &str,
    question: &str,
    k: usize,
    profiles: &[Value],
    route_plan: QaRoutePlan,
) -> Result<EvalContextChunks> {
    if !route_plan.use_source_chunks {
        let chunks = profile_grounding_chunks(storage, profiles, k)?;
        if !chunks.is_empty() {
            return Ok(EvalContextChunks {
                retrieval_trace: profile_grounding_trace(&chunks),
                chunks,
                qa_mode: route_plan.qa_mode.to_string(),
                route_reason: route_plan.route_reason.to_string(),
            });
        }
    }

    let (mut chunks, mut retrieval_trace) =
        hybrid::search_chunks_with_trace(storage, author, question, k)?;
    let (qa_mode, route_reason) = if route_plan.use_source_chunks {
        (route_plan.qa_mode, route_plan.route_reason)
    } else {
        ("source_evidence", "profile_grounding_empty")
    };
    let terms = query_terms(question);
    let excluded_chunk_ids = chunks.iter().map(|chunk| chunk.id).collect::<Vec<_>>();
    let profile_chunks =
        profile_grounding_chunks_matching_terms(storage, profiles, &terms, &excluded_chunk_ids, k)?;
    let appended_profile_chunks = append_unique_chunks_with_tail_reserve(
        &mut chunks,
        profile_chunks,
        k,
        EVAL_SOURCE_PROFILE_GROUNDING_RESERVE,
    );
    if !appended_profile_chunks.is_empty() {
        append_profile_grounding_route(&mut retrieval_trace, &appended_profile_chunks);
    }
    annotate_eval_route(&mut retrieval_trace, qa_mode, route_reason);
    Ok(EvalContextChunks {
        chunks,
        retrieval_trace,
        qa_mode: qa_mode.to_string(),
        route_reason: route_reason.to_string(),
    })
}

fn append_unique_chunks_with_tail_reserve(
    target: &mut Vec<SourceChunk>,
    candidates: Vec<SourceChunk>,
    limit: usize,
    reserve: usize,
) -> Vec<SourceChunk> {
    let reserve = reserve.min(limit);
    let mut additions = Vec::new();
    for chunk in candidates {
        if additions.len() >= reserve {
            break;
        }
        if target.iter().any(|existing| existing.id == chunk.id)
            || additions
                .iter()
                .any(|existing: &SourceChunk| existing.id == chunk.id)
        {
            continue;
        }
        additions.push(chunk);
    }
    if additions.is_empty() {
        return Vec::new();
    }
    target.truncate(limit.saturating_sub(additions.len()));
    target.extend(additions.iter().cloned());
    additions
}

fn append_profile_grounding_route(trace: &mut Value, chunks: &[SourceChunk]) {
    let route = chunks
        .iter()
        .enumerate()
        .map(|(rank, chunk)| {
            json!({
                "rank": rank + 1,
                "chunk_id": chunk.id,
                "paper_key": chunk.paper_key,
                "chunk_index": chunk.chunk_index,
                "section": chunk.section,
                "section_kind": chunk.section_kind,
                "caption_label": chunk.caption_label,
                "caption_object_type": chunk.caption_object_type,
                "caption_object_label": chunk.caption_object_label,
                "caption_panel_labels": chunk.caption_panel_labels_value(),
                "caption_target_labels": chunk.caption_target_labels_value(),
                "caption_panel_details": chunk.caption_panel_details_value(),
                "caption_measurements": chunk.caption_measurements_value(),
                "caption_conditions": chunk.caption_conditions_value(),
                "caption_values": chunk.caption_values_value(),
            })
        })
        .collect::<Vec<_>>();
    if let Some(routes) = trace.get_mut("routes").and_then(Value::as_object_mut) {
        routes.insert("profile_grounding".to_string(), Value::Array(route));
    }
}

fn profile_grounding_trace(chunks: &[SourceChunk]) -> Value {
    json!({
        "routes": {
            "profile_grounding": chunks.iter().enumerate().map(|(rank, chunk)| json!({
                "rank": rank + 1,
                "chunk_id": chunk.id,
                "paper_key": chunk.paper_key,
                "chunk_index": chunk.chunk_index,
                "section": chunk.section,
                "section_kind": chunk.section_kind,
                "caption_label": chunk.caption_label,
                "caption_object_type": chunk.caption_object_type,
                "caption_object_label": chunk.caption_object_label,
                "caption_panel_labels": chunk.caption_panel_labels_value(),
                "caption_target_labels": chunk.caption_target_labels_value(),
                "caption_panel_details": chunk.caption_panel_details_value(),
                "caption_measurements": chunk.caption_measurements_value(),
                "caption_conditions": chunk.caption_conditions_value(),
                "caption_values": chunk.caption_values_value(),
            })).collect::<Vec<_>>()
        },
        "fusion": []
    })
}

fn annotate_eval_route(trace: &mut Value, qa_mode: &str, route_reason: &str) {
    if let Some(object) = trace.as_object_mut() {
        object.insert("qa_mode".to_string(), qa_mode.into());
        object.insert("route_reason".to_string(), route_reason.into());
    }
}

fn annotate_eval_profile_version(trace: &mut Value, profile_version: QaProfileVersion) {
    if let Some(object) = trace.as_object_mut() {
        object.insert(
            "qa_profile_version".to_string(),
            profile_version.as_str().into(),
        );
    }
}

fn quote_profile_author(author: &str) -> String {
    format!("\"{}\"", author.replace('"', "\\\""))
}

pub fn compare_eval_reports(
    baseline: &EvalReport,
    candidate: &EvalReport,
    thresholds: EvalComparisonThresholds,
) -> EvalComparisonReport {
    let metrics = vec![
        compare_metric(
            "retrieval_hit_at_k",
            baseline.retrieval_hit_at_k,
            candidate.retrieval_hit_at_k,
            thresholds.max_retrieval_hit_drop,
            thresholds.min_candidate_retrieval_hit_at_k,
        ),
        compare_metric(
            "citation_precision",
            baseline.citation_precision,
            candidate.citation_precision,
            thresholds.max_citation_precision_drop,
            thresholds.min_candidate_citation_precision,
        ),
        compare_metric(
            "answer_contains_required",
            baseline.answer_contains_required,
            candidate.answer_contains_required,
            thresholds.max_answer_contains_required_drop,
            thresholds.min_candidate_answer_contains_required,
        ),
    ];
    let mut blockers = Vec::new();
    if baseline.total != candidate.total {
        blockers.push(format!(
            "question count mismatch: baseline={} candidate={}",
            baseline.total, candidate.total
        ));
    }
    for metric in &metrics {
        if metric.delta + metric.max_allowed_drop < 0.0 {
            blockers.push(format!(
                "{} dropped by {:.3}, exceeding allowed drop {:.3}",
                metric.metric, -metric.delta, metric.max_allowed_drop
            ));
        }
        if metric.candidate < metric.min_required_candidate {
            blockers.push(format!(
                "{} candidate value {:.3} is below required minimum {:.3}",
                metric.metric, metric.candidate, metric.min_required_candidate
            ));
        }
    }
    let metric_gate_pass = blockers.is_empty();
    EvalComparisonReport {
        baseline_profile_version: baseline.qa_profile_version.clone(),
        candidate_profile_version: candidate.qa_profile_version.clone(),
        baseline_total: baseline.total,
        candidate_total: candidate.total,
        thresholds,
        metrics,
        metric_gate_pass,
        default_switch_recommendation: if metric_gate_pass {
            "eligible_for_manual_review".to_string()
        } else {
            "hold".to_string()
        },
        blockers,
    }
}

fn compare_metric(
    metric: &str,
    baseline: f64,
    candidate: f64,
    max_allowed_drop: f64,
    min_required_candidate: f64,
) -> EvalMetricComparison {
    let delta = candidate - baseline;
    EvalMetricComparison {
        metric: metric.to_string(),
        baseline,
        candidate,
        delta,
        max_allowed_drop,
        min_required_candidate,
        passed: delta + max_allowed_drop >= 0.0 && candidate >= min_required_candidate,
    }
}

pub fn eval_report_json(report: &EvalReport, include_trace: bool) -> Result<Value> {
    let mut value = serde_json::to_value(report)?;
    if include_trace {
        return Ok(value);
    }
    if let Some(cases) = value.get_mut("cases").and_then(Value::as_array_mut) {
        for case in cases {
            if let Some(object) = case.as_object_mut() {
                object.remove("retrieval_trace");
            }
        }
    }
    Ok(value)
}

fn route_hit_rates(cases: &[EvalCaseReport]) -> BTreeMap<String, f64> {
    let mut routes = BTreeSet::new();
    for case in cases {
        routes.extend(case.route_hits.keys().cloned());
    }
    routes
        .into_iter()
        .map(|route| {
            let hits = cases
                .iter()
                .filter(|case| case.route_hits.get(&route).copied().unwrap_or(false))
                .count();
            let rate = if cases.is_empty() {
                0.0
            } else {
                hits as f64 / cases.len() as f64
            };
            (route, rate)
        })
        .collect()
}

fn qa_mode_summary(cases: &[EvalCaseReport]) -> BTreeMap<String, EvalQaModeSummary> {
    let mut buckets: BTreeMap<String, Vec<&EvalCaseReport>> = BTreeMap::new();
    for case in cases {
        buckets.entry(case.qa_mode.clone()).or_default().push(case);
    }
    buckets
        .into_iter()
        .map(|(mode, cases)| {
            let total = cases.len();
            let hits = cases.iter().filter(|case| case.retrieval_hit).count();
            let citation_precision_sum = cases
                .iter()
                .map(|case| case.citation_precision)
                .sum::<f64>();
            let contains_required = cases
                .iter()
                .filter(|case| case.answer_contains_required)
                .count();
            let mut route_reasons = BTreeMap::new();
            for case in &cases {
                *route_reasons.entry(case.route_reason.clone()).or_default() += 1;
            }
            (
                mode,
                EvalQaModeSummary {
                    total,
                    retrieval_hit_at_k: rate(hits, total),
                    citation_precision: if total == 0 {
                        0.0
                    } else {
                        citation_precision_sum / total as f64
                    },
                    answer_contains_required: rate(contains_required, total),
                    route_reasons,
                },
            )
        })
        .collect()
}

fn rate(count: usize, total: usize) -> f64 {
    if total == 0 {
        0.0
    } else {
        count as f64 / total as f64
    }
}

fn route_candidate_count_averages(cases: &[EvalCaseReport]) -> BTreeMap<String, f64> {
    let mut totals: BTreeMap<String, usize> = BTreeMap::new();
    for case in cases {
        let Some(routes) = case
            .retrieval_trace
            .get("routes")
            .and_then(Value::as_object)
        else {
            continue;
        };
        for (route, candidates) in routes {
            let count = candidates.as_array().map(Vec::len).unwrap_or_default();
            *totals.entry(route.clone()).or_default() += count;
        }
    }
    totals
        .into_iter()
        .map(|(route, total)| {
            let average = if cases.is_empty() {
                0.0
            } else {
                total as f64 / cases.len() as f64
            };
            (route, average)
        })
        .collect()
}

fn route_hits(trace: &Value, must_cite: &[String]) -> BTreeMap<String, bool> {
    let Some(routes) = trace.get("routes").and_then(Value::as_object) else {
        return BTreeMap::new();
    };
    routes
        .iter()
        .map(|(route, candidates)| {
            let hit = must_cite.is_empty()
                || candidates
                    .as_array()
                    .into_iter()
                    .flatten()
                    .any(|candidate| {
                        candidate
                            .get("paper_key")
                            .and_then(Value::as_str)
                            .is_some_and(|paper_key| must_cite.iter().any(|item| item == paper_key))
                    });
            (route.clone(), hit)
        })
        .collect()
}

struct AnswerEval {
    checked: bool,
    contains_expected_terms: bool,
    missing_expected_terms: Vec<String>,
    evidence_valid: bool,
    evidence_citation_precision: f64,
    validation_error: String,
}

fn evaluate_answer(
    question: &GoldenQuestion,
    chunks: &[crate::storage::SourceChunk],
) -> AnswerEval {
    let Some(answer_json) = question.answer_json.clone() else {
        return AnswerEval {
            checked: false,
            contains_expected_terms: true,
            missing_expected_terms: Vec::new(),
            evidence_valid: true,
            evidence_citation_precision: 1.0,
            validation_error: String::new(),
        };
    };
    let answer_text = answer_json
        .get("answer")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_lowercase();
    let missing_expected_terms = question
        .must_answer_include
        .iter()
        .filter(|term| !answer_text.contains(&term.to_lowercase()))
        .cloned()
        .collect::<Vec<_>>();
    let contains_expected_terms = missing_expected_terms.is_empty();
    let parsed = serde_json::from_value::<QaAnswerV1>(answer_json);
    let (evidence_valid, validation_error, evidence_citation_precision) = match parsed {
        Ok(answer) => {
            let validation = answer.validate(chunks);
            let cited = if question.must_evidence_cite.is_empty() {
                answer.evidence.len()
            } else {
                answer
                    .evidence
                    .iter()
                    .filter(|item| {
                        question
                            .must_evidence_cite
                            .iter()
                            .any(|paper_key| paper_key == &item.paper_key)
                    })
                    .count()
            };
            let precision = if answer.evidence.is_empty() {
                0.0
            } else {
                cited as f64 / answer.evidence.len() as f64
            };
            match validation {
                Ok(()) => (true, String::new(), precision),
                Err(error) => (false, error.to_string(), precision),
            }
        }
        Err(error) => (false, error.to_string(), 0.0),
    };
    AnswerEval {
        checked: true,
        contains_expected_terms,
        missing_expected_terms,
        evidence_valid,
        evidence_citation_precision,
        validation_error,
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use serde_json::Value;
    use tempfile::tempdir;

    use super::{
        EvalComparisonThresholds, EvalReport, compare_eval_reports, eval_report_json,
        run_golden_eval, run_golden_eval_with_profile_version,
    };
    use crate::papers::loader::load_paper;
    use crate::papers::models::Paper;
    use crate::qa::answerer::QaProfileVersion;
    use crate::retrieval::chunker::chunk_paper;
    use crate::schemas::paper_profile::{
        PAPER_PROFILE_SCHEMA_VERSION, PAPER_PROFILE_V2_SCHEMA_VERSION,
    };
    use crate::storage::{NewPaperProfileV2, PaperProfileMetadata, Storage};
    use crate::understanding::prompts::{
        PAPER_PROFILE_PROMPT_VERSION, PAPER_PROFILE_V2_PROMPT_VERSION,
    };

    fn eval_report(
        profile_version: &str,
        total: usize,
        retrieval_hit_at_k: f64,
        citation_precision: f64,
        answer_contains_required: f64,
    ) -> EvalReport {
        EvalReport {
            qa_profile_version: profile_version.to_string(),
            total,
            retrieval_hit_at_k,
            qa_mode_summary: BTreeMap::new(),
            route_hit_at_k: BTreeMap::new(),
            route_candidate_count_avg: BTreeMap::new(),
            citation_precision,
            answer_contains_required,
            insufficient_when_missing: 1.0,
            latency_ms: 0,
            cases: Vec::new(),
        }
    }

    #[test]
    fn compares_eval_reports_with_default_switch_thresholds() {
        let baseline = eval_report("v1", 9, 1.0, 0.80, 1.0);
        let passing_candidate = eval_report("v2", 9, 1.0, 0.79, 1.0);

        let passing = compare_eval_reports(
            &baseline,
            &passing_candidate,
            EvalComparisonThresholds::default(),
        );

        assert!(passing.metric_gate_pass);
        assert_eq!(
            passing.default_switch_recommendation,
            "eligible_for_manual_review"
        );
        assert!(passing.blockers.is_empty());

        let failing_candidate = eval_report("v2", 9, 0.99, 0.79, 1.0);
        let failing = compare_eval_reports(
            &baseline,
            &failing_candidate,
            EvalComparisonThresholds::default(),
        );

        assert!(!failing.metric_gate_pass);
        assert_eq!(failing.default_switch_recommendation, "hold");
        assert!(
            failing
                .blockers
                .iter()
                .any(|blocker| blocker.contains("retrieval_hit_at_k dropped"))
        );

        let below_absolute_candidate = eval_report("v2", 9, 1.0, 0.79, 0.80);
        let below_absolute = compare_eval_reports(
            &baseline,
            &below_absolute_candidate,
            EvalComparisonThresholds::default(),
        );

        assert!(!below_absolute.metric_gate_pass);
        assert_eq!(below_absolute.default_switch_recommendation, "hold");
        assert!(
            below_absolute.blockers.iter().any(|blocker| {
                blocker.contains("answer_contains_required candidate value 0.800")
            })
        );
    }

    #[test]
    fn computes_retrieval_hit_for_fixture_questions() {
        let dir = tempdir().unwrap();
        let mut storage = Storage::open(&dir.path().join("test.sqlite")).unwrap();
        let paper = Paper {
            author: "Alice".to_string(),
            paper_id: "paper-a".to_string(),
            paper_dir: dir.path().join("paper-a"),
            article_path: dir.path().join("paper-a/article.md"),
            fetch_result_path: None,
            source_hash: "hash".to_string(),
            metadata: std::collections::BTreeMap::from([
                ("title".to_string(), "MOF catalyst paper".to_string()),
                ("doi".to_string(), "10.1/test".to_string()),
                ("year".to_string(), "2024".to_string()),
            ]),
            fetch_result: serde_json::json!({}),
            raw_body: String::new(),
            clean_text: "The MOF catalyst improves conversion under mild conditions.".to_string(),
            sections: vec![],
        };
        let chunks = chunk_paper(&paper, 3200, 350);
        storage.upsert_paper(&paper, &chunks).unwrap();
        let fixture = dir.path().join("golden.json");
        std::fs::write(
            &fixture,
            r#"[{
                "author": "Alice",
                "question": "MOF catalyst",
                "must_cite": ["Alice/paper-a"],
                "must_include": ["conversion"],
                "must_not_include": ["photovoltaic"]
            }]"#,
        )
        .unwrap();

        let report = run_golden_eval(&storage, &fixture, 5).unwrap();
        assert_eq!(report.total, 1);
        assert_eq!(report.retrieval_hit_at_k, 1.0);
        assert_eq!(report.qa_mode_summary["source_evidence"].total, 1);
        assert_eq!(
            report.qa_mode_summary["source_evidence"].route_reasons["profile_missing"],
            1
        );
        assert!(report.route_hit_at_k.values().any(|rate| *rate > 0.0));
        assert!(
            report
                .route_candidate_count_avg
                .values()
                .any(|count| *count > 0.0)
        );
        assert!(report.citation_precision > 0.0);
        assert_eq!(report.answer_contains_required, 1.0);
        assert!(report.cases[0].latency_ms <= report.latency_ms);
        assert!(report.cases[0].missing_required_terms.is_empty());
        assert!(report.cases[0].forbidden_terms_found.is_empty());
        assert!(report.cases[0].retrieval_trace.get("routes").is_some());
        let compact = eval_report_json(&report, false).unwrap();
        assert!(
            compact["cases"][0]
                .as_object()
                .is_some_and(|case| !case.contains_key("retrieval_trace"))
        );
        let traced = eval_report_json(&report, true).unwrap();
        assert!(traced["cases"][0].get("retrieval_trace").is_some());
    }

    #[test]
    fn groups_eval_metrics_by_qa_mode_and_route_reason() {
        let dir = tempdir().unwrap();
        let mut storage = Storage::open(&dir.path().join("test.sqlite")).unwrap();
        let paper = Paper {
            author: "Alice".to_string(),
            paper_id: "paper-a".to_string(),
            paper_dir: dir.path().join("paper-a"),
            article_path: dir.path().join("paper-a/article.md"),
            fetch_result_path: None,
            source_hash: "hash".to_string(),
            metadata: std::collections::BTreeMap::from([
                ("title".to_string(), "MOF catalyst paper".to_string()),
                ("doi".to_string(), "10.1/test".to_string()),
                ("year".to_string(), "2024".to_string()),
            ]),
            fetch_result: serde_json::json!({}),
            raw_body: String::new(),
            clean_text: "The MOF catalyst improves conversion under mild experimental conditions."
                .to_string(),
            sections: vec![],
        };
        let chunks = chunk_paper(&paper, 3200, 350);
        storage.upsert_paper(&paper, &chunks).unwrap();
        storage
            .save_paper_profile_with_metadata(
                &paper.key(),
                &serde_json::json!({
                    "paper_key": paper.key(),
                    "title": "MOF catalyst paper",
                    "doi": "10.1/test",
                    "year": "2024",
                    "one_sentence_summary": "This paper studies MOF catalyst conversion.",
                    "methods": [{"method": "experimental conditions", "evidence_chunks": [0]}],
                    "topic_keywords": ["MOF catalyst"]
                }),
                PaperProfileMetadata {
                    source_hash: &paper.source_hash,
                    schema_version: PAPER_PROFILE_SCHEMA_VERSION,
                    prompt_version: PAPER_PROFILE_PROMPT_VERSION,
                    model_id: "test-model",
                    chunker_version: "section-char-v1",
                },
            )
            .unwrap();
        let fixture = dir.path().join("golden-modes.json");
        std::fs::write(
            &fixture,
            r#"[{
                "author": "Alice",
                "question": "这些论文主要讲什么？",
                "must_cite": ["Alice/paper-a"],
                "must_include": ["conversion"]
            }, {
                "author": "Alice",
                "question": "paper-a 的实验条件是什么？",
                "must_cite": ["Alice/paper-a"],
                "must_include": ["experimental conditions"]
            }]"#,
        )
        .unwrap();

        let report = run_golden_eval(&storage, &fixture, 5).unwrap();

        assert_eq!(report.qa_mode_summary["profile_first"].total, 1);
        assert_eq!(
            report.qa_mode_summary["profile_first"].route_reasons["broad_profile_context"],
            1
        );
        assert_eq!(report.qa_mode_summary["source_evidence"].total, 1);
        assert_eq!(
            report.qa_mode_summary["source_evidence"].route_reasons["detail_keyword"],
            1
        );
        assert_eq!(report.cases[0].qa_mode, "profile_first");
        assert_eq!(report.cases[1].qa_mode, "source_evidence");
    }

    #[test]
    fn v2_eval_uses_v2_profile_count_and_marks_report() {
        let dir = tempdir().unwrap();
        let mut storage = Storage::open(&dir.path().join("test.sqlite")).unwrap();
        let paper = Paper {
            author: "Alice".to_string(),
            paper_id: "paper-a".to_string(),
            paper_dir: dir.path().join("paper-a"),
            article_path: dir.path().join("paper-a/article.md"),
            fetch_result_path: None,
            source_hash: "hash".to_string(),
            metadata: std::collections::BTreeMap::from([
                ("title".to_string(), "MOF catalyst paper".to_string()),
                ("doi".to_string(), "10.1/test".to_string()),
                ("year".to_string(), "2024".to_string()),
            ]),
            fetch_result: serde_json::json!({}),
            raw_body: String::new(),
            clean_text: "The MOF catalyst improves conversion under mild experimental conditions."
                .to_string(),
            sections: vec![],
        };
        let chunks = chunk_paper(&paper, 3200, 350);
        storage.upsert_paper(&paper, &chunks).unwrap();
        let fixture = dir.path().join("golden-v2.json");
        std::fs::write(
            &fixture,
            r#"[{
                "author": "Alice",
                "question": "这些论文主要讲什么？",
                "must_cite": ["Alice/paper-a"],
                "must_include": ["conversion"]
            }]"#,
        )
        .unwrap();

        let error =
            run_golden_eval_with_profile_version(&storage, &fixture, 5, QaProfileVersion::V2)
                .unwrap_err();
        assert!(error.to_string().contains("no V2 paper profiles for Alice"));

        let paper_key = paper.key();
        let profile_json = serde_json::json!({
            "paper_key": paper_key.clone(),
            "title": "MOF catalyst paper"
        });
        storage
            .save_paper_profile_v2(NewPaperProfileV2 {
                paper_key: &paper_key,
                profile_json: &profile_json,
                profile_schema_version: PAPER_PROFILE_V2_SCHEMA_VERSION,
                builder_version: PAPER_PROFILE_V2_PROMPT_VERSION,
                model_id: "test-model",
                source_fact_hash: "facts-hash",
            })
            .unwrap();

        let report =
            run_golden_eval_with_profile_version(&storage, &fixture, 5, QaProfileVersion::V2)
                .unwrap();

        assert_eq!(report.qa_profile_version, "v2");
        assert_eq!(report.cases[0].qa_profile_version, "v2");
        assert_eq!(report.cases[0].qa_mode, "profile_first");
        assert_eq!(report.cases[0].retrieval_trace["qa_profile_version"], "v2");
    }

    #[test]
    fn evaluates_structured_answer_when_fixture_provides_answer_json() {
        let dir = tempdir().unwrap();
        let mut storage = Storage::open(&dir.path().join("test.sqlite")).unwrap();
        let paper = Paper {
            author: "Alice".to_string(),
            paper_id: "paper-a".to_string(),
            paper_dir: dir.path().join("paper-a"),
            article_path: dir.path().join("paper-a/article.md"),
            fetch_result_path: None,
            source_hash: "hash".to_string(),
            metadata: std::collections::BTreeMap::from([
                ("title".to_string(), "MOF catalyst paper".to_string()),
                ("doi".to_string(), "10.1/test".to_string()),
                ("year".to_string(), "2024".to_string()),
            ]),
            fetch_result: serde_json::json!({}),
            raw_body: String::new(),
            clean_text: "The MOF catalyst improves conversion under mild conditions.".to_string(),
            sections: vec![],
        };
        let chunks = chunk_paper(&paper, 3200, 350);
        storage.upsert_paper(&paper, &chunks).unwrap();
        let fixture = dir.path().join("golden-answer.json");
        std::fs::write(
            &fixture,
            r#"[{
                "author": "Alice",
                "question": "MOF catalyst conversion",
                "must_cite": ["Alice/paper-a"],
                "must_answer_include": ["conversion"],
                "must_evidence_cite": ["Alice/paper-a"],
                "answer_json": {
                    "answer": "The catalyst improves conversion.",
                    "claims": [{"claim": "The catalyst improves conversion.", "evidence_indices": [0], "support": "strong"}],
                    "evidence": [{
                        "paper_key": "Alice/paper-a",
                        "title": "MOF catalyst paper",
                        "doi": "10.1/test",
                        "year": "2024",
                        "chunk_id": 1,
                        "section": "Body",
                        "quote_or_summary": "improves conversion"
                    }],
                    "uncertainty": "",
                    "followup_queries": []
                }
            }]"#,
        )
        .unwrap();

        let report = run_golden_eval(&storage, &fixture, 5).unwrap();

        assert!(report.cases[0].answer_checked);
        assert!(report.cases[0].answer_contains_expected_terms);
        assert!(report.cases[0].answer_evidence_valid);
        assert_eq!(report.cases[0].answer_evidence_citation_precision, 1.0);
    }

    #[test]
    fn bundled_golden_fixture_has_required_baseline_size() {
        let fixture = include_str!("../tests/fixtures/golden_questions.json");
        let questions: Vec<Value> = serde_json::from_str(fixture).unwrap();
        let paper_keys = questions
            .iter()
            .flat_map(|question| {
                question
                    .get("must_cite")
                    .and_then(Value::as_array)
                    .into_iter()
                    .flatten()
                    .filter_map(Value::as_str)
            })
            .collect::<std::collections::BTreeSet<_>>();

        assert!(questions.len() >= 20);
        assert!(paper_keys.len() >= 5);
    }

    #[test]
    fn bundled_paper_fixture_has_five_article_files() {
        let root = std::path::Path::new("tests/fixtures/paper/Alice");
        let mut articles = std::fs::read_dir(root)
            .unwrap()
            .filter_map(Result::ok)
            .map(|entry| entry.path().join("article.md"))
            .filter(|path| path.exists())
            .collect::<Vec<_>>();
        articles.sort();

        assert_eq!(articles.len(), 5);
    }

    #[test]
    fn bundled_golden_fixture_runs_against_bundled_papers() {
        let dir = tempdir().unwrap();
        let mut storage = Storage::open(&dir.path().join("test.sqlite")).unwrap();
        let paper_root = std::path::Path::new("tests/fixtures/paper");
        let author_root = paper_root.join("Alice");
        for entry in std::fs::read_dir(&author_root).unwrap() {
            let paper_dir = entry.unwrap().path();
            let paper = load_paper(paper_root, &paper_dir).unwrap();
            let chunks = chunk_paper(&paper, 3200, 350);
            storage.upsert_paper(&paper, &chunks).unwrap();
        }

        let report = run_golden_eval(
            &storage,
            std::path::Path::new("tests/fixtures/golden_questions.json"),
            8,
        )
        .unwrap();

        assert_eq!(report.total, 30);
        assert_eq!(report.retrieval_hit_at_k, 1.0);
        assert!(report.citation_precision > 0.0);
        assert_eq!(report.answer_contains_required, 1.0);
        assert!(
            report
                .cases
                .iter()
                .all(|case| case.latency_ms <= report.latency_ms)
        );
        assert!(
            report
                .cases
                .iter()
                .all(|case| case.forbidden_terms_found.is_empty())
        );
    }
}
