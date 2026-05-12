use std::collections::{BTreeMap, BTreeSet};
use std::time::Instant;

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::schemas::qa_answer::QaAnswerV1;
use crate::storage::Storage;

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
    pub total: usize,
    pub retrieval_hit_at_k: f64,
    pub route_hit_at_k: BTreeMap<String, f64>,
    pub route_candidate_count_avg: BTreeMap<String, f64>,
    pub citation_precision: f64,
    pub answer_contains_required: f64,
    pub insufficient_when_missing: f64,
    pub latency_ms: u128,
    pub cases: Vec<EvalCaseReport>,
}

#[derive(Debug, Serialize)]
pub struct EvalCaseReport {
    pub question: String,
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

pub fn run_golden_eval(
    storage: &Storage,
    fixture_path: &std::path::Path,
    k: usize,
) -> Result<EvalReport> {
    let text = std::fs::read_to_string(fixture_path)
        .with_context(|| format!("failed to read {}", fixture_path.display()))?;
    let questions: Vec<GoldenQuestion> = serde_json::from_str(&text)
        .with_context(|| format!("invalid golden fixture {}", fixture_path.display()))?;
    let started = Instant::now();
    let mut cases = Vec::new();
    for question in questions {
        let case_started = Instant::now();
        let (chunks, retrieval_trace) =
            storage.search_chunks_with_trace(&question.author, &question.question, k)?;
        let retrieved = chunks
            .iter()
            .map(|chunk| chunk.paper_key.clone())
            .collect::<Vec<_>>();
        let retrieval_hit = question.must_cite.is_empty()
            || question
                .must_cite
                .iter()
                .any(|paper_key| retrieved.iter().any(|item| item == paper_key));
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
            question: question.question,
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
        total,
        retrieval_hit_at_k: if total == 0 {
            0.0
        } else {
            hits as f64 / total as f64
        },
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
    use serde_json::Value;
    use tempfile::tempdir;

    use super::{eval_report_json, run_golden_eval};
    use crate::papers::loader::load_paper;
    use crate::papers::models::Paper;
    use crate::retrieval::chunker::chunk_paper;
    use crate::storage::Storage;

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

        assert_eq!(report.total, 20);
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
