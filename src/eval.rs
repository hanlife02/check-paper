use std::time::Instant;

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

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
}

#[derive(Debug, Serialize)]
pub struct EvalReport {
    pub total: usize,
    pub retrieval_hit_at_k: f64,
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
    pub citation_precision: f64,
    pub answer_contains_required: bool,
    pub insufficient_when_missing: bool,
    pub latency_ms: u128,
    pub missing_required_terms: Vec<String>,
    pub forbidden_terms_found: Vec<String>,
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
        let chunks = storage.search_chunks(&question.author, &question.question, k)?;
        let retrieved = chunks
            .iter()
            .map(|chunk| chunk.paper_key.clone())
            .collect::<Vec<_>>();
        let retrieval_hit = question.must_cite.is_empty()
            || question
                .must_cite
                .iter()
                .any(|paper_key| retrieved.iter().any(|item| item == paper_key));
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
        cases.push(EvalCaseReport {
            question: question.question,
            retrieved,
            retrieval_hit,
            citation_precision,
            answer_contains_required,
            insufficient_when_missing,
            latency_ms: case_started.elapsed().as_millis(),
            missing_required_terms,
            forbidden_terms_found,
        });
    }
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

#[cfg(test)]
mod tests {
    use serde_json::Value;
    use tempfile::tempdir;

    use super::run_golden_eval;
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
        assert!(report.citation_precision > 0.0);
        assert_eq!(report.answer_contains_required, 1.0);
        assert!(report.cases[0].latency_ms <= report.latency_ms);
        assert!(report.cases[0].missing_required_terms.is_empty());
        assert!(report.cases[0].forbidden_terms_found.is_empty());
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
