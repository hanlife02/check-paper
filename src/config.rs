use std::collections::BTreeMap;
use std::env;
use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};

const CONFIG_FILE_ENV: &str = "PAPER_CHECK_CONFIG";
const DEFAULT_CONFIG_PATH: &str = ".paper-check.json";

#[derive(Debug, Clone)]
pub struct Settings {
    pub paper_root: PathBuf,
    pub db_path: PathBuf,
    pub default_author: Option<String>,
    pub proxy: Option<String>,
    pub llm_base_url: String,
    pub llm_api_key: Option<String>,
    pub llm_model: String,
    pub llm_timeout_secs: u64,
    pub llm_tls_backend: String,
    pub llm_prompt_cost_per_1k: Option<f64>,
    pub llm_completion_cost_per_1k: Option<f64>,
    pub embedding_provider: String,
    pub embedding_base_url: String,
    pub embedding_api_key: Option<String>,
    pub embedding_model: String,
    pub embedding_model_version: Option<String>,
    pub embedding_timeout_secs: u64,
    pub embedding_tls_backend: String,
    pub embedding_batch_size: usize,
    pub chunk_max_chars: usize,
    pub chunk_overlap: usize,
    pub chunker_version: String,
    pub telegram_bot_token: Option<String>,
    pub telegram_chat_ids: Vec<i64>,
}

impl Settings {
    pub fn from_sources() -> Self {
        let config = load_config(None).unwrap_or_default();
        Self {
            paper_root: PathBuf::from(setting(&config, "CHECK_PAPER_PAPER_ROOT", "paper")),
            db_path: PathBuf::from(setting(
                &config,
                "CHECK_PAPER_DB_PATH",
                "data/check_paper.sqlite",
            )),
            default_author: empty_to_none(setting(&config, "CHECK_PAPER_DEFAULT_AUTHOR", "")),
            proxy: empty_to_none(setting(&config, "CHECK_PAPER_PROXY", "")),
            llm_base_url: setting(
                &config,
                "CHECK_PAPER_LLM_BASE_URL",
                "https://api.openai.com/v1",
            )
            .trim_end_matches('/')
            .to_string(),
            llm_api_key: empty_to_none(setting(&config, "CHECK_PAPER_LLM_API_KEY", "")),
            llm_model: setting(&config, "CHECK_PAPER_LLM_MODEL", ""),
            llm_timeout_secs: parse_u64(&setting(&config, "CHECK_PAPER_LLM_TIMEOUT_SECS", "180"))
                .unwrap_or(180),
            llm_tls_backend: setting(&config, "CHECK_PAPER_LLM_TLS_BACKEND", "rustls"),
            llm_prompt_cost_per_1k: parse_f64(&setting(
                &config,
                "CHECK_PAPER_LLM_PROMPT_COST_PER_1K",
                "",
            )),
            llm_completion_cost_per_1k: parse_f64(&setting(
                &config,
                "CHECK_PAPER_LLM_COMPLETION_COST_PER_1K",
                "",
            )),
            embedding_provider: setting(&config, "CHECK_PAPER_EMBEDDING_PROVIDER", "disabled"),
            embedding_base_url: setting(
                &config,
                "CHECK_PAPER_EMBEDDING_BASE_URL",
                "https://api.openai.com/v1",
            )
            .trim_end_matches('/')
            .to_string(),
            embedding_api_key: empty_to_none(setting(&config, "CHECK_PAPER_EMBEDDING_API_KEY", "")),
            embedding_model: setting(&config, "CHECK_PAPER_EMBEDDING_MODEL", ""),
            embedding_model_version: empty_to_none(setting(
                &config,
                "CHECK_PAPER_EMBEDDING_MODEL_VERSION",
                "",
            )),
            embedding_timeout_secs: parse_u64(&setting(
                &config,
                "CHECK_PAPER_EMBEDDING_TIMEOUT_SECS",
                "180",
            ))
            .unwrap_or(180),
            embedding_tls_backend: setting(&config, "CHECK_PAPER_EMBEDDING_TLS_BACKEND", "rustls"),
            embedding_batch_size: parse_usize(&setting(
                &config,
                "CHECK_PAPER_EMBEDDING_BATCH_SIZE",
                "64",
            ))
            .filter(|value| *value > 0)
            .unwrap_or(64),
            chunk_max_chars: parse_usize(&setting(&config, "CHECK_PAPER_CHUNK_MAX_CHARS", "3200"))
                .filter(|value| *value > 0)
                .unwrap_or(3200),
            chunk_overlap: parse_usize(&setting(&config, "CHECK_PAPER_CHUNK_OVERLAP", "350"))
                .unwrap_or(350),
            chunker_version: setting(&config, "CHECK_PAPER_CHUNKER_VERSION", "section-char-v1"),
            telegram_bot_token: empty_to_none(setting(&config, "TELEGRAM_BOT_TOKEN", "")),
            telegram_chat_ids: parse_i64_list(&setting(&config, "TELEGRAM_CHAT_IDS", "")),
        }
    }

    pub fn ensure_dirs(&self) -> Result<()> {
        if let Some(parent) = self.db_path.parent() {
            fs::create_dir_all(parent)
                .with_context(|| format!("failed to create {}", parent.display()))?;
        }
        Ok(())
    }
}

pub fn config_path() -> PathBuf {
    env::var(CONFIG_FILE_ENV)
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from(DEFAULT_CONFIG_PATH))
}

pub fn load_config(path: Option<&Path>) -> Result<BTreeMap<String, String>> {
    let path = path.map(PathBuf::from).unwrap_or_else(config_path);
    if !path.exists() {
        return Ok(BTreeMap::new());
    }
    let text =
        fs::read_to_string(&path).with_context(|| format!("failed to read {}", path.display()))?;
    let raw: serde_json::Value = serde_json::from_str(&text)
        .with_context(|| format!("invalid JSON in {}", path.display()))?;
    let mut config = BTreeMap::new();
    if let Some(object) = raw.as_object() {
        for (key, value) in object {
            if !value.is_null() {
                config.insert(
                    key.to_string(),
                    value.as_str().unwrap_or(&value.to_string()).to_string(),
                );
            }
        }
    }
    Ok(config)
}

pub fn save_config(updates: &BTreeMap<String, String>, path: Option<&Path>) -> Result<PathBuf> {
    let path = path.map(PathBuf::from).unwrap_or_else(config_path);
    let mut config = load_config(Some(&path)).unwrap_or_default();
    for (key, value) in updates {
        config.insert(key.clone(), value.clone());
    }
    if let Some(parent) = path.parent() {
        if parent != Path::new("") {
            fs::create_dir_all(parent)
                .with_context(|| format!("failed to create {}", parent.display()))?;
        }
    }
    let text = serde_json::to_string_pretty(&config)? + "\n";
    fs::write(&path, text).with_context(|| format!("failed to write {}", path.display()))?;
    Ok(path)
}

pub fn redacted_config(path: Option<&Path>) -> Result<BTreeMap<String, String>> {
    let mut config = load_config(path)?;
    for key in ["CHECK_PAPER_LLM_API_KEY", "TELEGRAM_BOT_TOKEN"] {
        if let Some(value) = config.get_mut(key) {
            *value = redact(value);
        }
    }
    Ok(config)
}

fn setting(config: &BTreeMap<String, String>, key: &str, default: &str) -> String {
    env::var(key)
        .ok()
        .or_else(|| config.get(key).cloned())
        .unwrap_or_else(|| default.to_string())
}

fn empty_to_none(value: String) -> Option<String> {
    if value.trim().is_empty() {
        None
    } else {
        Some(value)
    }
}

fn redact(value: &str) -> String {
    let chars: Vec<char> = value.chars().collect();
    if chars.len() <= 8 {
        "********".to_string()
    } else {
        let start: String = chars.iter().take(4).collect();
        let end: String = chars
            .iter()
            .rev()
            .take(4)
            .collect::<Vec<_>>()
            .into_iter()
            .rev()
            .collect();
        format!("{start}...{end}")
    }
}

fn parse_i64_list(value: &str) -> Vec<i64> {
    value
        .split(',')
        .filter_map(|item| item.trim().parse::<i64>().ok())
        .collect()
}

fn parse_u64(value: &str) -> Option<u64> {
    value.trim().parse::<u64>().ok()
}

fn parse_usize(value: &str) -> Option<usize> {
    value.trim().parse::<usize>().ok()
}

fn parse_f64(value: &str) -> Option<f64> {
    value.trim().parse::<f64>().ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn save_load_and_redact_config() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("config.json");
        let updates = BTreeMap::from([
            (
                "CHECK_PAPER_DB_PATH".to_string(),
                "data/test.sqlite".to_string(),
            ),
            (
                "CHECK_PAPER_LLM_API_KEY".to_string(),
                "sk-test-secret".to_string(),
            ),
            (
                "TELEGRAM_BOT_TOKEN".to_string(),
                "123456789:telegram-secret".to_string(),
            ),
        ]);
        save_config(&updates, Some(&path)).unwrap();
        let config = load_config(Some(&path)).unwrap();
        assert_eq!(config["CHECK_PAPER_DB_PATH"], "data/test.sqlite");
        let redacted = redacted_config(Some(&path)).unwrap();
        assert_eq!(redacted["CHECK_PAPER_LLM_API_KEY"], "sk-t...cret");
        assert_eq!(redacted["TELEGRAM_BOT_TOKEN"], "1234...cret");
    }
}
