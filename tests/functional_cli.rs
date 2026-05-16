use std::fs;
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::Path;
use std::process::{Command, Output};
use std::sync::mpsc::{self, Receiver, Sender};
use std::thread::{self, JoinHandle};
use std::time::Duration;

use serde_json::{Value, json};
use tempfile::tempdir;

#[test]
fn cli_flow_grounds_broad_answer_in_article_body_not_metadata() {
    let dir = tempdir().unwrap();
    let paper_root = dir.path().join("paper");
    let author = "Functional Author";
    let paper_dir = paper_root.join(author).join("paper-a");
    fs::create_dir_all(&paper_dir).unwrap();
    fs::write(
        paper_dir.join("article.md"),
        r#"---
title: "Metadata Title Paper"
doi: "10.999/test"
year: "2026"
---
# Metadata Title Paper

- DOI: `10.999/test`
- Year: `2026`
- Journal: `Functional Test`

## Article Text

Abstract

Developing scalable methods to synthesize catalysts is the central topic.

Results

The best condition reports 91% conversion under mild conditions.
"#,
    )
    .unwrap();

    let (base_url, request_rx, server) = start_mock_llm();
    let config_path = dir.path().join("config.json");
    fs::write(
        &config_path,
        json!({
            "CHECK_PAPER_PAPER_ROOT": paper_root,
            "CHECK_PAPER_DB_PATH": dir.path().join("test.sqlite"),
            "CHECK_PAPER_LLM_BASE_URL": base_url,
            "CHECK_PAPER_LLM_API_KEY": "test-key",
            "CHECK_PAPER_LLM_MODEL": "mock-model",
            "CHECK_PAPER_LLM_TIMEOUT_SECS": "5"
        })
        .to_string(),
    )
    .unwrap();

    assert_success(run_ppc(&config_path, &["ingest", "--author", author]));
    assert_success(run_ppc(
        &config_path,
        &["analyze", "--author", author, "--limit", "1"],
    ));
    let ask = run_ppc(
        &config_path,
        &[
            "ask",
            "--author",
            author,
            "请用一句话概括目前已分析论文的主要方向",
        ],
    );
    assert_success(ask);

    let requests = collect_requests(&request_rx, 3);
    let qa_prompt = requests
        .iter()
        .filter_map(|request| prompt_payload(request))
        .find(|payload| payload.get("source_chunks").is_some())
        .expect("mock LLM should receive a QA prompt");
    let source_chunks = qa_prompt["source_chunks"].as_array().unwrap();

    assert_eq!(source_chunks.len(), 1);
    assert_eq!(source_chunks[0]["section"], "Article Text");
    assert_eq!(source_chunks[0]["chunk_index"], 1);
    assert!(
        source_chunks[0]["text"]
            .as_str()
            .unwrap()
            .contains("Developing scalable methods")
    );
    assert!(
        !source_chunks[0]["text"]
            .as_str()
            .unwrap()
            .contains("Journal: `Functional Test`")
    );

    server.join().unwrap();
}

fn run_ppc(config_path: &Path, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_ppc"))
        .env("PAPER_CHECK_CONFIG", config_path)
        .args(args)
        .output()
        .unwrap()
}

fn assert_success(output: Output) {
    if !output.status.success() {
        panic!(
            "command failed\nstatus: {}\nstdout:\n{}\nstderr:\n{}",
            output.status,
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
    }
}

fn start_mock_llm() -> (String, Receiver<String>, JoinHandle<()>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    let (tx, rx) = mpsc::channel();
    let handle = thread::spawn(move || {
        for _ in 0..3 {
            let Ok((mut stream, _)) = listener.accept() else {
                break;
            };
            handle_request(&mut stream, &tx);
        }
    });
    (format!("http://{address}/v1"), rx, handle)
}

fn handle_request(stream: &mut TcpStream, tx: &Sender<String>) {
    let request = read_http_request(stream);
    tx.send(request.clone()).unwrap();
    let content = mock_response_content(&request);
    let body = json!({
        "choices": [{"message": {"content": content}}],
        "usage": {"prompt_tokens": 10, "completion_tokens": 5, "total_tokens": 15}
    })
    .to_string();
    let response = format!(
        "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{}",
        body.len(),
        body
    );
    stream.write_all(response.as_bytes()).unwrap();
}

fn mock_response_content(request: &str) -> String {
    let Some(payload) = prompt_payload(request) else {
        return "{}".to_string();
    };
    if payload.get("paper_chunks").is_some() {
        return json!({
            "paper_key": "Functional Author/paper-a",
            "title": "Metadata Title Paper",
            "doi": "10.999/test",
            "year": "2026",
            "one_sentence_summary": "This paper studies scalable catalyst synthesis.",
            "research_question": "How to synthesize catalysts at scale?",
            "core_contributions": ["Uses article-body abstract and results."],
            "methods": [{"method": "scalable synthesis", "evidence_chunks": [1], "section": "Article Text"}],
            "key_results": [{"claim": "Reports 91% conversion.", "evidence_chunks": [1], "section": "Article Text", "confidence": "high"}],
            "limitations": [],
            "topic_keywords": ["catalysts", "scalable synthesis"],
            "reliable_answer_scope": ["scalable catalyst synthesis"],
            "evidence_notes": ["Functional test profile."]
        })
        .to_string();
    }
    if payload.get("paper_profiles").is_some() && payload.get("source_chunks").is_none() {
        return json!({
            "author": "Functional Author",
            "research_areas": ["scalable catalyst synthesis"],
            "research_evolution": [],
            "representative_works": [{
                "year": "2026",
                "title": "Metadata Title Paper",
                "doi": "10.999/test",
                "reason": "Functional test profile."
            }],
            "methodological_strengths": ["article-body grounded synthesis analysis"],
            "answer_scope": ["scalable catalyst synthesis"]
        })
        .to_string();
    }
    if let Some(chunk) = payload["source_chunks"].as_array().and_then(|chunks| {
        chunks
            .iter()
            .find(|chunk| chunk["section"] == "Article Text")
    }) {
        return json!({
            "answer": "已分析论文主要围绕可规模化催化剂合成。",
            "claims": [{
                "claim": "已分析论文主要围绕可规模化催化剂合成。",
                "evidence_indices": [0],
                "support": "strong"
            }],
            "evidence": [{
                "paper_key": chunk["paper_key"],
                "title": chunk["title"],
                "doi": chunk["doi"],
                "year": chunk["year"],
                "chunk_id": chunk["chunk_id"],
                "section": chunk["section"],
                "quote_or_summary": "Developing scalable methods"
            }],
            "uncertainty": "",
            "followup_queries": []
        })
        .to_string();
    }
    json!({
        "answer": "证据不足。",
        "claims": [],
        "evidence": [],
        "uncertainty": "source_chunks 缺少 Article Text。",
        "followup_queries": []
    })
    .to_string()
}

fn prompt_payload(request: &str) -> Option<Value> {
    let body = request.split("\r\n\r\n").nth(1)?;
    let request_json: Value = serde_json::from_str(body).ok()?;
    let content = request_json["messages"]
        .as_array()?
        .iter()
        .rev()
        .find_map(|message| message["content"].as_str())?;
    serde_json::from_str(content).ok()
}

fn collect_requests(rx: &Receiver<String>, count: usize) -> Vec<String> {
    (0..count)
        .map(|_| {
            rx.recv_timeout(Duration::from_secs(2))
                .expect("mock LLM request")
        })
        .collect()
}

fn read_http_request(stream: &mut TcpStream) -> String {
    stream
        .set_read_timeout(Some(Duration::from_secs(2)))
        .unwrap();
    let mut request = Vec::new();
    let mut buffer = [0u8; 4096];
    loop {
        let count = stream.read(&mut buffer).unwrap();
        if count == 0 {
            break;
        }
        request.extend_from_slice(&buffer[..count]);
        if request_body_complete(&request) {
            break;
        }
    }
    String::from_utf8_lossy(&request).to_string()
}

fn request_body_complete(request: &[u8]) -> bool {
    let Some(header_end) = request.windows(4).position(|window| window == b"\r\n\r\n") else {
        return false;
    };
    let headers = String::from_utf8_lossy(&request[..header_end]);
    let content_length = headers
        .lines()
        .find_map(|line| {
            line.to_ascii_lowercase()
                .strip_prefix("content-length:")
                .and_then(|value| value.trim().parse::<usize>().ok())
        })
        .unwrap_or(0);
    request.len() >= header_end + 4 + content_length
}
