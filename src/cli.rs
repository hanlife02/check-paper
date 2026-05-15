use anyhow::{Result, anyhow};
use clap::{Args, Parser, Subcommand};
use indicatif::{ProgressBar, ProgressStyle};
use reqwest::Proxy;
use reqwest::blocking::ClientBuilder;
use serde::Deserialize;
use serde_json::json;
use std::collections::BTreeMap;
use std::io::{self, Write};
use std::process;
use std::thread;
use std::time::Duration;

use crate::bots::handlers::BotHandlers;
use crate::bots::telegram_bot::TelegramBot;
use crate::config::{Settings, config_path, load_config, redacted_config, save_config};
use crate::papers::loader::load_paper;
use crate::papers::scanner::scan_paper_dirs;
use crate::retrieval::chunker::chunk_paper;
use crate::retrieval::embedding::{EmbeddingConfig, OpenAiCompatibleEmbeddingClient};
use crate::schemas::paper_profile::PAPER_PROFILE_SCHEMA_VERSION;
use crate::services::analysis::{AnalysisQueueOptions, AnalysisService};
use crate::services::embedding::EmbeddingService;
use crate::services::eval::EvalService;
use crate::services::jobs::JobService;
use crate::services::profile::{AuthorProfileLookup, AuthorProfileRebuild, ProfileService};
use crate::services::qa::QaService;
use crate::services::status::StatusService;
use crate::storage::{AuthorSummary, PaperProfileMetadata, Storage};
use crate::understanding::llm::{LlmConfig, OpenAiCompatibleClient};
use crate::understanding::paper_analyzer::{analyze_paper, extract_section_facts};
use crate::understanding::prompts::PAPER_PROFILE_PROMPT_VERSION;

const TELEGRAM_STATUS_TIMEOUT_SECS: u64 = 20;

#[derive(Parser)]
#[command(version, about = "Analyze local paper archives and answer questions.")]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    #[command(about = "Configure database path, default author, and proxy")]
    Config(ConfigArgs),
    #[command(about = "Configure or check the OpenAI-compatible LLM")]
    Llm {
        #[command(subcommand)]
        command: LlmCommand,
    },
    #[command(about = "Configure Telegram bot settings")]
    Tg {
        #[command(subcommand)]
        command: TgCommand,
    },
    #[command(about = "List authors already ingested into the database")]
    Authors,
    #[command(about = "List local paper directories")]
    Scan(AuthorArgs),
    #[command(about = "Parse papers, chunk text, and update the database")]
    Ingest(AuthorArgs),
    #[command(about = "Generate structured paper profiles with the LLM")]
    Analyze(AnalyzeArgs),
    #[command(about = "Run ingest and analyze in one step")]
    Sync(AnalyzeArgs),
    #[command(about = "Create or refresh chunk embeddings")]
    Embed(EmbedArgs),
    #[command(about = "Ask a question about an author's papers")]
    Ask(AskArgs),
    #[command(about = "Run a golden-question retrieval/answer evaluation")]
    Eval(EvalArgs),
    #[command(about = "Inspect, retry, or cancel analysis jobs")]
    Jobs(JobsArgs),
    #[command(about = "Show QA or job logs")]
    Logs {
        #[command(subcommand)]
        command: LogsCommand,
    },
    #[command(about = "Show library and job status")]
    Status(AuthorArgs),
    #[command(about = "Show or rebuild an author profile")]
    Profile(ProfileArgs),
    #[command(about = "Start the Telegram bot polling loop")]
    ServeTelegram,
}

#[derive(Args)]
struct ConfigArgs {
    #[arg(long, help = "Print saved config values without prompting")]
    show: bool,
}

#[derive(Subcommand)]
enum LlmCommand {
    #[command(about = "Configure LLM endpoint, key, model, timeout, and costs")]
    Config(LlmConfigArgs),
    #[command(about = "Send a small test request to the configured LLM")]
    Check,
}

#[derive(Args)]
struct LlmConfigArgs {
    #[arg(long, help = "Print saved LLM config values without prompting")]
    show: bool,
}

#[derive(Subcommand)]
enum TgCommand {
    #[command(about = "Configure Telegram bot token and allowed chat IDs")]
    Config(TgConfigArgs),
    #[command(about = "Check Telegram bot configuration and API connectivity")]
    Status,
}

#[derive(Args)]
struct TgConfigArgs {
    #[arg(long, help = "Print saved Telegram config values without prompting")]
    show: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct TelegramBotStatus {
    id: i64,
    username: Option<String>,
    first_name: String,
}

#[derive(Deserialize)]
struct TelegramApiResponse<T> {
    ok: bool,
    result: Option<T>,
    description: Option<String>,
}

#[derive(Deserialize)]
struct TelegramApiUser {
    id: i64,
    first_name: String,
    username: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct PaperRootAuthorSummary {
    author: String,
    paper_count: usize,
}

#[derive(Args)]
struct AuthorArgs {
    #[arg(long, help = "Author directory under the paper root")]
    author: Option<String>,
}

#[derive(Args, Clone)]
struct AnalyzeArgs {
    #[arg(long, help = "Author directory under the paper root")]
    author: Option<String>,
    #[arg(long, help = "Maximum number of papers to queue/process")]
    limit: Option<usize>,
    #[arg(long, help = "Re-analyze papers even if their profiles are current")]
    force: bool,
    #[arg(long, help = "Only retry papers with failed analysis jobs")]
    failed_only: bool,
    #[arg(long, help = "Only analyze papers whose stored profile is stale")]
    stale_only: bool,
    #[arg(
        long,
        default_value_t = 3,
        help = "Maximum stored job attempts before giving up"
    )]
    max_attempts: i64,
    #[arg(
        long,
        help = "Skip rebuilding the author-level profile after paper analysis"
    )]
    skip_author_profile: bool,
}

#[derive(Args, Clone)]
struct EmbedArgs {
    #[arg(long, help = "Author directory under the paper root")]
    author: Option<String>,
    #[arg(long, help = "Maximum number of chunks to embed")]
    limit: Option<usize>,
    #[arg(
        long,
        help = "Refresh embeddings even when the stored model/version matches"
    )]
    force: bool,
}

#[derive(Args)]
struct AskArgs {
    #[arg(long, help = "Author directory under the paper root")]
    author: Option<String>,
    #[arg(help = "Question text")]
    question: Vec<String>,
}

#[derive(Args)]
struct EvalArgs {
    #[arg(long, help = "Path to a JSON fixture of golden questions")]
    fixture: std::path::PathBuf,
    #[arg(
        long,
        default_value_t = 8,
        help = "Number of retrieved chunks to evaluate"
    )]
    top_k: usize,
    #[arg(long, help = "Include per-question trace details in the JSON report")]
    trace: bool,
}

#[derive(Args)]
struct JobsArgs {
    #[arg(long, help = "Filter jobs by author")]
    author: Option<String>,
    #[arg(
        long,
        help = "Filter by job status: queued, running, failed, succeeded, cancelled, retry_waiting"
    )]
    status: Option<String>,
    #[arg(long, default_value_t = 20, help = "Maximum jobs to print")]
    limit: usize,
    #[arg(long, help = "Move failed jobs back to the queued state")]
    retry_failed: bool,
    #[arg(long, help = "Cancel a job by numeric ID")]
    cancel: Option<i64>,
}

#[derive(Subcommand)]
enum LogsCommand {
    #[command(about = "Show recent QA logs or QA error counts")]
    Qa(LogQaArgs),
    #[command(about = "Show recent analysis jobs or job error counts")]
    Jobs(LogJobsArgs),
}

#[derive(Args)]
struct LogQaArgs {
    #[arg(long, help = "Filter QA logs by author")]
    author: Option<String>,
    #[arg(long, default_value_t = 10, help = "Maximum QA logs to print")]
    last: usize,
    #[arg(long, help = "Show grouped QA error counts instead of log rows")]
    errors: bool,
}

#[derive(Args)]
struct LogJobsArgs {
    #[arg(long, help = "Filter job logs by author")]
    author: Option<String>,
    #[arg(long, default_value_t = 10, help = "Maximum jobs to print")]
    last: usize,
    #[arg(long, help = "Only show failed jobs")]
    failed: bool,
    #[arg(long, help = "Show grouped job error counts instead of job rows")]
    errors: bool,
}

#[derive(Args)]
struct ProfileArgs {
    #[arg(long, help = "Author directory under the paper root")]
    author: Option<String>,
    #[arg(
        long,
        help = "Force rebuilding the author-level profile from paper profiles"
    )]
    rebuild: bool,
}

pub fn run() -> Result<()> {
    install_ctrlc_handler()?;
    let cli = Cli::parse();
    match cli.command {
        Command::Config(args) => cmd_config(args),
        Command::Llm { command } => match command {
            LlmCommand::Config(args) => cmd_llm_config(args),
            LlmCommand::Check => {
                let settings = Settings::from_sources();
                cmd_llm_check(&settings)
            }
        },
        Command::Tg { command } => match command {
            TgCommand::Config(args) => cmd_tg_config(args),
            TgCommand::Status => {
                let settings = Settings::from_sources();
                cmd_tg_status(&settings)
            }
        },
        command => {
            let settings = Settings::from_sources();
            settings.ensure_dirs()?;
            match command {
                Command::Scan(args) => cmd_scan(args, &settings),
                Command::Authors => cmd_authors(&settings),
                Command::Ingest(args) => cmd_ingest(args, &settings),
                Command::Analyze(args) => cmd_analyze(args, &settings),
                Command::Sync(args) => {
                    cmd_ingest(
                        AuthorArgs {
                            author: args.author.clone(),
                        },
                        &settings,
                    )?;
                    cmd_analyze(args, &settings)
                }
                Command::Embed(args) => cmd_embed(args, &settings),
                Command::Ask(args) => cmd_ask(args, &settings),
                Command::Eval(args) => cmd_eval(args, &settings),
                Command::Jobs(args) => cmd_jobs(args, &settings),
                Command::Logs { command } => cmd_logs(command, &settings),
                Command::Status(args) => cmd_status(args, &settings),
                Command::Profile(args) => cmd_profile(args, &settings),
                Command::ServeTelegram => cmd_serve_telegram(&settings),
                Command::Config(_) | Command::Llm { .. } | Command::Tg { .. } => unreachable!(),
            }
        }
    }
}

fn install_ctrlc_handler() -> Result<()> {
    ctrlc::set_handler(|| {
        eprintln!("received Ctrl-C, exiting");
        process::exit(130);
    })?;
    Ok(())
}

fn cmd_config(args: ConfigArgs) -> Result<()> {
    if args.show {
        print_config(&[
            "CHECK_PAPER_DB_PATH",
            "CHECK_PAPER_DEFAULT_AUTHOR",
            "CHECK_PAPER_PROXY",
        ])?;
        return Ok(());
    }

    let current = load_config(None).unwrap_or_default();
    let mut updates = BTreeMap::new();
    updates.insert(
        "CHECK_PAPER_DB_PATH".to_string(),
        prompt_value(
            "db-path",
            current
                .get("CHECK_PAPER_DB_PATH")
                .map(String::as_str)
                .unwrap_or("data/check_paper.sqlite"),
            false,
        )?,
    );
    updates.insert(
        "CHECK_PAPER_DEFAULT_AUTHOR".to_string(),
        prompt_value(
            "default-author",
            current
                .get("CHECK_PAPER_DEFAULT_AUTHOR")
                .map(String::as_str)
                .unwrap_or(""),
            false,
        )?,
    );
    updates.insert(
        "CHECK_PAPER_PROXY".to_string(),
        prompt_value(
            "proxy",
            current
                .get("CHECK_PAPER_PROXY")
                .map(String::as_str)
                .unwrap_or(""),
            false,
        )?,
    );

    let path = save_config(&updates, None)?;
    println!("saved config to {}", path.display());
    Ok(())
}

fn cmd_llm_config(args: LlmConfigArgs) -> Result<()> {
    if args.show {
        print_config(&[
            "CHECK_PAPER_LLM_BASE_URL",
            "CHECK_PAPER_LLM_API_KEY",
            "CHECK_PAPER_LLM_MODEL",
            "CHECK_PAPER_LLM_TIMEOUT_SECS",
            "CHECK_PAPER_LLM_TLS_BACKEND",
            "CHECK_PAPER_LLM_PROMPT_COST_PER_1K",
            "CHECK_PAPER_LLM_COMPLETION_COST_PER_1K",
        ])?;
        return Ok(());
    }

    let current = load_config(None).unwrap_or_default();
    let mut updates = BTreeMap::new();
    updates.insert(
        "CHECK_PAPER_LLM_BASE_URL".to_string(),
        prompt_value(
            "base-url",
            current
                .get("CHECK_PAPER_LLM_BASE_URL")
                .map(String::as_str)
                .unwrap_or("https://api.openai.com/v1"),
            false,
        )?,
    );
    updates.insert(
        "CHECK_PAPER_LLM_API_KEY".to_string(),
        prompt_value(
            "api-key",
            current
                .get("CHECK_PAPER_LLM_API_KEY")
                .map(String::as_str)
                .unwrap_or(""),
            true,
        )?,
    );
    updates.insert(
        "CHECK_PAPER_LLM_MODEL".to_string(),
        prompt_value(
            "model",
            current
                .get("CHECK_PAPER_LLM_MODEL")
                .map(String::as_str)
                .unwrap_or(""),
            false,
        )?,
    );
    updates.insert(
        "CHECK_PAPER_LLM_TIMEOUT_SECS".to_string(),
        prompt_value(
            "timeout-secs",
            current
                .get("CHECK_PAPER_LLM_TIMEOUT_SECS")
                .map(String::as_str)
                .unwrap_or("180"),
            false,
        )?,
    );
    updates.insert(
        "CHECK_PAPER_LLM_TLS_BACKEND".to_string(),
        prompt_value(
            "tls-backend (rustls/native)",
            current
                .get("CHECK_PAPER_LLM_TLS_BACKEND")
                .map(String::as_str)
                .unwrap_or("rustls"),
            false,
        )?,
    );
    updates.insert(
        "CHECK_PAPER_LLM_PROMPT_COST_PER_1K".to_string(),
        prompt_value(
            "prompt-cost-per-1k",
            current
                .get("CHECK_PAPER_LLM_PROMPT_COST_PER_1K")
                .map(String::as_str)
                .unwrap_or(""),
            false,
        )?,
    );
    updates.insert(
        "CHECK_PAPER_LLM_COMPLETION_COST_PER_1K".to_string(),
        prompt_value(
            "completion-cost-per-1k",
            current
                .get("CHECK_PAPER_LLM_COMPLETION_COST_PER_1K")
                .map(String::as_str)
                .unwrap_or(""),
            false,
        )?,
    );

    let path = save_config(&updates, None)?;
    println!("saved LLM config to {}", path.display());
    Ok(())
}

fn cmd_llm_check(settings: &Settings) -> Result<()> {
    require_llm(settings)?;
    let llm = make_llm(settings)?;
    let reply = llm.chat(
        vec![crate::understanding::llm::ChatMessage {
            role: "user".to_string(),
            content: "Reply with ok.".to_string(),
        }],
        0.0,
        8,
    )?;
    println!("LLM check succeeded: {reply}");
    Ok(())
}

fn cmd_tg_config(args: TgConfigArgs) -> Result<()> {
    if args.show {
        print_config(&["TELEGRAM_BOT_TOKEN", "TELEGRAM_CHAT_IDS"])?;
        return Ok(());
    }

    let current = load_config(None).unwrap_or_default();
    let mut updates = BTreeMap::new();
    updates.insert(
        "TELEGRAM_BOT_TOKEN".to_string(),
        prompt_value(
            "bot-token",
            current
                .get("TELEGRAM_BOT_TOKEN")
                .map(String::as_str)
                .unwrap_or(""),
            true,
        )?,
    );
    updates.insert(
        "TELEGRAM_CHAT_IDS".to_string(),
        prompt_value(
            "chat-ids",
            current
                .get("TELEGRAM_CHAT_IDS")
                .map(String::as_str)
                .unwrap_or(""),
            false,
        )?,
    );

    let path = save_config(&updates, None)?;
    println!("saved Telegram config to {}", path.display());
    Ok(())
}

fn cmd_tg_status(settings: &Settings) -> Result<()> {
    let token = settings
        .telegram_bot_token
        .as_deref()
        .ok_or_else(|| anyhow!("missing TELEGRAM_BOT_TOKEN; run `ppc tg config`"))?;
    let bot = telegram_get_me(token, settings.proxy.as_deref())?;
    println!("{}", format_tg_status(settings, &bot));
    Ok(())
}

fn telegram_get_me(token: &str, proxy: Option<&str>) -> Result<TelegramBotStatus> {
    let mut builder =
        ClientBuilder::new().timeout(Duration::from_secs(TELEGRAM_STATUS_TIMEOUT_SECS));
    if let Some(proxy) = proxy {
        builder = builder.proxy(Proxy::all(proxy)?);
    }
    let endpoint = format!("https://api.telegram.org/bot{token}/getMe");
    let response = builder.build()?.get(&endpoint).send().map_err(|error| {
        anyhow!(
            "Telegram getMe request failed: {}",
            redact_secret(&error.to_string(), token)
        )
    })?;
    let status = response.status();
    let body = response
        .text()
        .map_err(|error| anyhow!("Telegram getMe response read failed: {error}"))?;
    if !status.is_success() {
        return Err(anyhow!(
            "Telegram getMe returned HTTP {status}: {}",
            redact_secret(&body, token)
        ));
    }
    let response: TelegramApiResponse<TelegramApiUser> =
        serde_json::from_str(&body).map_err(|error| {
            anyhow!(
                "Telegram getMe response JSON parse failed: {error}; body: {}",
                redact_secret(&body, token)
            )
        })?;
    if !response.ok {
        return Err(anyhow!(
            "Telegram getMe failed: {}",
            response
                .description
                .unwrap_or_else(|| "unknown error".to_string())
        ));
    }
    let user = response
        .result
        .ok_or_else(|| anyhow!("Telegram getMe response missing result"))?;
    Ok(TelegramBotStatus {
        id: user.id,
        username: user.username,
        first_name: user.first_name,
    })
}

fn format_tg_status(settings: &Settings, bot: &TelegramBotStatus) -> String {
    let username = bot
        .username
        .as_deref()
        .map(|username| format!("@{username}"))
        .unwrap_or_else(|| "<no username>".to_string());
    [
        "Telegram status: ok".to_string(),
        format!(
            "bot: {} id={} first_name={}",
            username, bot.id, bot.first_name
        ),
        format!(
            "allowed_chats: {}",
            format_cli_chat_ids(&settings.telegram_chat_ids)
        ),
        format!(
            "proxy: {}",
            settings.proxy.as_deref().unwrap_or("<not configured>")
        ),
        "api_check: getMe succeeded".to_string(),
        "serve_command: ppc serve-telegram".to_string(),
    ]
    .join("\n")
}

fn format_cli_chat_ids(chat_ids: &[i64]) -> String {
    if chat_ids.is_empty() {
        "all".to_string()
    } else {
        chat_ids
            .iter()
            .map(i64::to_string)
            .collect::<Vec<_>>()
            .join(",")
    }
}

fn redact_secret(text: &str, secret: &str) -> String {
    if secret.is_empty() {
        text.to_string()
    } else {
        text.replace(secret, "<redacted>")
    }
}

fn prompt_value(name: &str, current: &str, secret: bool) -> Result<String> {
    if secret {
        let suffix = if current.is_empty() {
            ""
        } else {
            " [leave blank to keep current]"
        };
        let value = rpassword::prompt_password(format!("{name}{suffix}: "))
            .or_else(|_| prompt_line(name, suffix))?;
        return Ok(if value.trim().is_empty() {
            current.to_string()
        } else {
            value.trim().to_string()
        });
    }

    let suffix = if current.is_empty() {
        String::new()
    } else {
        format!(" [{current}]")
    };
    let value = prompt_line(name, &suffix)?;
    let value = value.trim();
    Ok(if value.is_empty() {
        current.to_string()
    } else {
        value.to_string()
    })
}

fn prompt_line(name: &str, suffix: &str) -> Result<String> {
    print!("{name}{suffix}: ");
    io::stdout().flush()?;
    let mut value = String::new();
    io::stdin().read_line(&mut value)?;
    Ok(value)
}

fn print_config(keys: &[&str]) -> Result<()> {
    let config = redacted_config(None)?;
    let values = keys
        .iter()
        .map(|key| {
            (
                (*key).to_string(),
                config.get(*key).cloned().unwrap_or_default(),
            )
        })
        .collect::<BTreeMap<_, _>>();
    println!(
        "{}",
        serde_json::to_string_pretty(&json!({
            "config_file": config_path(),
            "values": values,
        }))?
    );
    Ok(())
}

fn progress_bar(len: u64, prefix: &'static str) -> ProgressBar {
    let progress = ProgressBar::new(len);
    if let Ok(style) = ProgressStyle::with_template(
        "{prefix:.bold} [{elapsed_precise}] [{wide_bar:.cyan/blue}] {pos}/{len} {msg}",
    ) {
        progress.set_style(style.progress_chars("=>-"));
    }
    progress.set_prefix(prefix);
    progress
}

fn paper_progress(message: String) -> ProgressBar {
    let progress = ProgressBar::new_spinner();
    if let Ok(style) = ProgressStyle::with_template("{spinner:.cyan} [{elapsed_precise}] {msg}") {
        progress.set_style(style.tick_chars("|/-\\"));
    }
    progress.enable_steady_tick(Duration::from_millis(120));
    progress.set_message(message);
    progress
}

fn display_name(path: &std::path::Path) -> String {
    path.file_name()
        .map(|name| name.to_string_lossy().to_string())
        .unwrap_or_else(|| path.display().to_string())
}

fn cmd_authors(settings: &Settings) -> Result<()> {
    let authors = database_authors(settings)?;
    let paper_root_authors = paper_root_authors(&settings.paper_root)?;
    println!(
        "{}",
        format_author_inventory(
            &authors,
            &paper_root_authors,
            settings.default_author.as_deref()
        )
    );
    Ok(())
}

fn database_authors(settings: &Settings) -> Result<Vec<AuthorSummary>> {
    if !settings.db_path.exists() {
        return Ok(Vec::new());
    }
    Storage::open(&settings.db_path)?.authors()
}

fn paper_root_authors(paper_root: &std::path::Path) -> Result<Vec<PaperRootAuthorSummary>> {
    let paper_dirs = scan_paper_dirs(paper_root, None)?;
    let mut counts = BTreeMap::<String, usize>::new();
    for paper_dir in paper_dirs {
        let Some(author) = paper_dir
            .parent()
            .and_then(|path| path.file_name())
            .map(|name| name.to_string_lossy().to_string())
        else {
            continue;
        };
        *counts.entry(author).or_default() += 1;
    }
    Ok(counts
        .into_iter()
        .map(|(author, paper_count)| PaperRootAuthorSummary {
            author,
            paper_count,
        })
        .collect())
}

fn format_author_inventory(
    authors: &[AuthorSummary],
    paper_root_authors: &[PaperRootAuthorSummary],
    default_author: Option<&str>,
) -> String {
    if authors.is_empty() {
        if paper_root_authors.is_empty() {
            return "no authors found; run `ppc scan`, then `ppc ingest --author AUTHOR` or `ppc sync --author AUTHOR` first".to_string();
        }
        let mut lines = vec![format!("authors: {}", paper_root_authors.len())];
        for (index, author) in paper_root_authors.iter().enumerate() {
            let default_marker = if Some(author.author.as_str()) == default_author {
                "; default"
            } else {
                ""
            };
            lines.push(format!(
                "{}. {} (paper/: {} papers; not ingested{})",
                index + 1,
                author.author,
                author.paper_count,
                default_marker
            ));
        }
        lines.push(
            "Import one with `ppc ingest --author \"NAME\"` or `ppc sync --author \"NAME\"`."
                .to_string(),
        );
        return lines.join("\n");
    }

    let paper_counts = paper_root_authors
        .iter()
        .map(|author| (author.author.as_str(), author.paper_count))
        .collect::<BTreeMap<_, _>>();
    let mut lines = vec![format!("authors: {}", authors.len())];
    for (index, author) in authors.iter().enumerate() {
        let default_marker = if Some(author.author.as_str()) == default_author {
            "; default"
        } else {
            ""
        };
        let paper_root_text = paper_counts
            .get(author.author.as_str())
            .map(|count| format!("; paper/: {count} papers"))
            .unwrap_or_else(|| "; paper/: not found".to_string());
        lines.push(format!(
            "{}. {} (db: {} papers{}{})",
            index + 1,
            author.author,
            author.paper_count,
            paper_root_text,
            default_marker
        ));
    }
    lines.push(
        "Use one with `--author \"NAME\"`, or set it as default with `ppc config`.".to_string(),
    );
    lines.join("\n")
}

fn cmd_scan(args: AuthorArgs, settings: &Settings) -> Result<()> {
    let paper_dirs = scan_paper_dirs(&settings.paper_root, args.author.as_deref())?;
    println!("found {} paper directories", paper_dirs.len());
    for path in paper_dirs.iter().take(20) {
        println!("{}", path.display());
    }
    if paper_dirs.len() > 20 {
        println!("... {} more", paper_dirs.len() - 20);
    }
    Ok(())
}

fn cmd_ingest(args: AuthorArgs, settings: &Settings) -> Result<()> {
    let mut storage = Storage::open(&settings.db_path)?;
    let paper_dirs = scan_paper_dirs(&settings.paper_root, args.author.as_deref())?;
    let mut changed_count = 0usize;
    let progress = progress_bar(paper_dirs.len() as u64, "ingesting");
    for paper_dir in &paper_dirs {
        progress.set_message(display_name(paper_dir));
        let paper = load_paper(&settings.paper_root, paper_dir)?;
        let chunks = chunk_paper(&paper, settings.chunk_max_chars, settings.chunk_overlap);
        if storage.upsert_paper_with_chunker(
            &paper,
            &chunks,
            &settings.chunker_version,
            settings.chunk_max_chars,
            settings.chunk_overlap,
        )? {
            changed_count += 1;
        }
        progress.inc(1);
    }
    progress.finish_with_message(format!(
        "ingested {} papers; changed {}",
        paper_dirs.len(),
        changed_count
    ));
    Ok(())
}

fn cmd_analyze(args: AnalyzeArgs, settings: &Settings) -> Result<()> {
    let author = resolve_author(args.author.as_deref(), settings)?;
    require_llm(settings)?;
    let storage = Storage::open(&settings.db_path)?;
    let llm = make_llm(settings)?;
    let plan = AnalysisService::new(&storage).enqueue_author(
        &author,
        AnalysisQueueOptions {
            failed_only: args.failed_only,
            stale_only: args.stale_only,
            force: args.force,
            limit: args.limit,
            max_attempts: args.max_attempts,
            model_id: llm.model_name(),
            chunker_version: &settings.chunker_version,
        },
    )?;
    println!(
        "papers needing analysis: {}; newly queued: {queued}",
        plan.candidates.len(),
        queued = plan.queued
    );
    let mut failures = Vec::new();
    let mut processed_count = 0usize;
    let mut success_count = 0usize;
    let worker_id = format!("ppc-{}", process::id());
    let process_limit = plan.candidates.len();
    for index in 0..process_limit {
        let Some(task) =
            storage.claim_next_analysis_job(&author, "analyze", &worker_id, 30 * 60)?
        else {
            break;
        };
        processed_count += 1;
        let row = task.candidate;
        let paper_dir = settings.paper_root.join(&row.author).join(&row.paper_id);
        let paper = load_paper(&settings.paper_root, &paper_dir)?;
        let message = format!(
            "[{}/{}] job #{} attempt {}/{} {} {}",
            index + 1,
            process_limit,
            task.id,
            task.attempt_count + 1,
            task.max_attempts,
            paper.year(),
            paper.title()
        );
        let progress = paper_progress(message.clone());
        match analyze_paper_with_retries(
            &paper,
            &llm,
            &progress,
            &message,
            settings.chunk_max_chars,
            settings.chunk_overlap,
        ) {
            Ok(profile) => {
                storage.save_paper_profile_with_metadata(
                    &paper.key(),
                    &profile,
                    PaperProfileMetadata {
                        source_hash: &paper.source_hash,
                        schema_version: PAPER_PROFILE_SCHEMA_VERSION,
                        prompt_version: PAPER_PROFILE_PROMPT_VERSION,
                        model_id: llm.model_name(),
                        chunker_version: &settings.chunker_version,
                    },
                )?;
                storage.save_paper_facts(
                    &paper.key(),
                    &extract_section_facts(
                        &paper,
                        settings.chunk_max_chars,
                        settings.chunk_overlap,
                    ),
                )?;
                storage.complete_analysis_job(task.id, &paper.key())?;
                progress.finish_with_message(format!("{message} done"));
                success_count += 1;
            }
            Err(err) => {
                progress.finish_with_message(format!("{message} failed"));
                let error_code = classify_analysis_error(&err);
                let status = storage.fail_analysis_job(
                    task.id,
                    &paper.key(),
                    error_code,
                    &err.to_string(),
                )?;
                progress.println(format!("job #{} marked {status}", task.id));
                failures.push(AnalysisFailure {
                    paper_key: paper.key(),
                    error_code,
                    error: err.to_string(),
                });
            }
        }
    }
    println!(
        "processed {}; succeeded {}; failed {}",
        processed_count,
        success_count,
        failures.len()
    );

    if should_rebuild_author_profile(args.skip_author_profile, success_count) {
        match ProfileService::new(&storage).rebuild_author_profile(&author, &llm, false)? {
            AuthorProfileRebuild::NoPaperProfiles => {}
            AuthorProfileRebuild::Current { .. } => {
                println!("author profile already up to date");
            }
            AuthorProfileRebuild::Rebuilt { profile_count } => {
                println!("updated author profile with {profile_count} paper profiles");
            }
        }
    } else if !args.skip_author_profile {
        println!("author profile rebuild skipped; no paper profiles changed in this run");
    }
    if !failures.is_empty() {
        for line in analysis_failure_summary_lines(&author, &failures) {
            println!("{line}");
        }
    }
    Ok(())
}

#[derive(Debug, Clone)]
struct AnalysisFailure {
    paper_key: String,
    error_code: &'static str,
    error: String,
}

fn classify_analysis_error(error: &anyhow::Error) -> &'static str {
    let message = error
        .chain()
        .map(ToString::to_string)
        .collect::<Vec<_>>()
        .join("；");
    let lower = message.to_lowercase();
    if lower.contains("missing check_paper_llm")
        || lower.contains("invalid check_paper_llm_tls_backend")
    {
        "llm_config_error"
    } else if message.contains("请求超时")
        || lower.contains("timed out")
        || lower.contains("timeout")
    {
        "llm_timeout"
    } else if message.contains("无法连接")
        || lower.contains("connection refused")
        || lower.contains("dns")
        || lower.contains("network")
        || lower.contains("connect error")
    {
        "network_error"
    } else if message.contains("LLM API 返回 HTTP") {
        "llm_http_error"
    } else if message.contains("LLM API 响应 JSON 解析失败") {
        "llm_response_error"
    } else if message.contains("PaperProfileV1")
        || lower.contains("evidence_chunks")
        || lower.contains("missing field")
        || lower.contains("invalid type")
    {
        "schema_error"
    } else {
        "analyze_error"
    }
}

fn analysis_failure_summary_lines(author: &str, failures: &[AnalysisFailure]) -> Vec<String> {
    let mut lines = vec![format!(
        "analysis completed with {} failed attempts",
        failures.len()
    )];
    let mut counts = BTreeMap::new();
    for failure in failures {
        *counts.entry(failure.error_code).or_insert(0usize) += 1;
    }
    lines.push("failure summary:".to_string());
    for (error_code, count) in counts {
        lines.push(format!("- {error_code}: {count}"));
    }
    let author_arg = quote_cli_arg(author);
    lines.push("next steps:".to_string());
    lines.push(format!(
        "- Inspect failed jobs: ppc jobs --author {author_arg} --status failed"
    ));
    lines.push(format!(
        "- Inspect retry-waiting jobs: ppc jobs --author {author_arg} --status retry_waiting"
    ));
    lines.push(format!(
        "- Retry failed jobs: ppc analyze --author {author_arg} --failed-only"
    ));
    lines.push("failed attempts:".to_string());
    for failure in failures.iter().take(20) {
        lines.push(format!(
            "- {} [{}]: {}",
            failure.paper_key, failure.error_code, failure.error
        ));
    }
    if failures.len() > 20 {
        lines.push(format!("- ... {} more", failures.len() - 20));
    }
    lines
}

fn quote_cli_arg(value: &str) -> String {
    format!("\"{}\"", value.replace('"', "\\\""))
}

fn should_rebuild_author_profile(skip_author_profile: bool, success_count: usize) -> bool {
    !skip_author_profile && success_count > 0
}

fn cmd_embed(args: EmbedArgs, settings: &Settings) -> Result<()> {
    let author = resolve_author(args.author.as_deref(), settings)?;
    require_embedding(settings)?;
    let storage = Storage::open(&settings.db_path)?;
    let client = make_embedding(settings)?;
    let embeddings = EmbeddingService::new(&storage);
    let pending = embeddings.pending_chunks(
        &author,
        args.limit,
        client.model_name(),
        client.model_version(),
        args.force,
    )?;
    println!("chunks needing embedding: {}", pending.len());
    let progress = progress_bar(pending.len() as u64, "embedding");
    let batch_size = client.batch_size();
    for batch in pending.chunks(batch_size) {
        let input = batch
            .iter()
            .map(|item| {
                format!(
                    "{}\n{}\n{}\n{}",
                    item.chunk.title, item.chunk.doi, item.chunk.section, item.chunk.text
                )
            })
            .collect::<Vec<_>>();
        let vectors = match client.embed(&input) {
            Ok(vectors) => vectors,
            Err(err) => {
                for item in batch {
                    embeddings.record_failure(
                        item,
                        client.model_name(),
                        client.model_version(),
                        &err.to_string(),
                    )?;
                }
                return Err(err);
            }
        };
        if vectors.len() != batch.len() {
            return Err(anyhow!(
                "embedding API returned {} vectors for {} inputs",
                vectors.len(),
                batch.len()
            ));
        }
        for (item, vector) in batch.iter().zip(vectors.iter()) {
            embeddings.save_success(item, client.model_name(), client.model_version(), vector)?;
            progress.inc(1);
        }
    }
    progress.finish_with_message("embedded chunks");
    Ok(())
}

fn analyze_paper_with_retries(
    paper: &crate::papers::models::Paper,
    llm: &OpenAiCompatibleClient,
    progress: &ProgressBar,
    message: &str,
    chunk_max_chars: usize,
    chunk_overlap: usize,
) -> Result<serde_json::Value> {
    let mut last_error = None;
    for attempt in 1..=3 {
        progress.set_message(format!("{message} attempt {attempt}/3"));
        match analyze_paper(paper, llm, 22000, chunk_max_chars, chunk_overlap) {
            Ok(profile) => return Ok(profile),
            Err(err) => {
                progress.println(format!("{message} attempt {attempt}/3 failed: {err}"));
                last_error = Some(err);
                if attempt < 3 {
                    thread::sleep(Duration::from_secs(2 * attempt));
                }
            }
        }
    }
    Err(last_error.unwrap_or_else(|| anyhow!("analysis failed")))
}

fn cmd_ask(args: AskArgs, settings: &Settings) -> Result<()> {
    let author = resolve_author(args.author.as_deref(), settings)?;
    let question = args.question.join(" ");
    if question.trim().is_empty() {
        return Err(anyhow!("missing question; pass text after `ppc ask`"));
    }
    require_llm(settings)?;
    let storage = Storage::open(&settings.db_path)?;
    let qa = QaService::new(
        &storage,
        make_llm(settings)?,
        make_optional_embedding(settings)?,
    );
    println!("{}", qa.answer(&author, &question)?);
    Ok(())
}

fn cmd_eval(args: EvalArgs, settings: &Settings) -> Result<()> {
    let storage = Storage::open(&settings.db_path)?;
    let eval = EvalService::new(&storage);
    let report = eval.run_golden(&args.fixture, args.top_k)?;
    println!(
        "{}",
        serde_json::to_string_pretty(&eval.report_json(&report, args.trace)?)?
    );
    Ok(())
}

fn cmd_jobs(args: JobsArgs, settings: &Settings) -> Result<()> {
    let storage = Storage::open(&settings.db_path)?;
    let jobs = JobService::new(&storage);
    if let Some(job_id) = args.cancel {
        jobs.cancel(job_id)?;
        println!("cancelled job #{job_id}");
        return Ok(());
    }
    if args.retry_failed {
        let count = jobs.retry_failed(args.author.as_deref())?;
        println!("queued {count} failed jobs for retry");
        return Ok(());
    }
    let rows = jobs.list(args.author.as_deref(), args.status.as_deref(), args.limit)?;
    if rows.is_empty() {
        println!("no jobs");
        return Ok(());
    }
    for job in rows {
        println!(
            "#{} [{}] {} {} model={} updated={}",
            job.id,
            job.status,
            job.job_type,
            job.paper_key.as_deref().unwrap_or("-"),
            job.model_id.as_deref().unwrap_or("-"),
            job.updated_at
        );
        if let Some(error_code) = job.error_code.as_deref() {
            println!("  error_code={error_code}");
        }
        if let Some(error) = job.error.as_deref() {
            if !error.trim().is_empty() {
                println!("  error={}", error.trim());
            }
        }
    }
    Ok(())
}

fn cmd_logs(command: LogsCommand, settings: &Settings) -> Result<()> {
    let storage = Storage::open(&settings.db_path)?;
    match command {
        LogsCommand::Qa(args) => {
            if args.errors {
                let counts = storage.qa_error_counts(args.author.as_deref())?;
                if counts.is_empty() {
                    println!("no qa errors");
                } else {
                    for (error_code, count) in counts {
                        println!("{error_code}: {count}");
                    }
                }
                return Ok(());
            }
            let logs = storage.qa_logs(args.author.as_deref(), args.last)?;
            if logs.is_empty() {
                println!("no qa logs");
                return Ok(());
            }
            for log in logs {
                println!(
                    "#{} [{}] author={} model={} latency_ms={} prompt_tokens={} completion_tokens={} total_tokens={} cost_usd={} question={}",
                    log.id,
                    log.created_at,
                    log.author,
                    log.model.as_deref().unwrap_or("-"),
                    log.latency_ms
                        .map(|value| value.to_string())
                        .unwrap_or_else(|| "-".to_string()),
                    log.prompt_tokens
                        .map(|value| value.to_string())
                        .unwrap_or_else(|| "-".to_string()),
                    log.completion_tokens
                        .map(|value| value.to_string())
                        .unwrap_or_else(|| "-".to_string()),
                    log.total_tokens
                        .map(|value| value.to_string())
                        .unwrap_or_else(|| "-".to_string()),
                    log.cost_usd
                        .map(|value| format!("{value:.6}"))
                        .unwrap_or_else(|| "-".to_string()),
                    log.question
                );
                if let Some(error_code) = log.error_code.as_deref() {
                    println!("  error_code={error_code}");
                }
            }
        }
        LogsCommand::Jobs(args) => {
            let jobs = JobService::new(&storage);
            let status = args.failed.then_some("failed");
            if args.errors {
                let counts = jobs.error_counts(args.author.as_deref(), status)?;
                if counts.is_empty() {
                    println!("no job errors");
                } else {
                    for (error_code, count) in counts {
                        println!("{error_code}: {count}");
                    }
                }
                return Ok(());
            }
            let rows = jobs.list(args.author.as_deref(), status, args.last)?;
            if rows.is_empty() {
                println!("no jobs");
                return Ok(());
            }
            for job in rows {
                println!(
                    "#{} [{}] {} {} model={} updated={}",
                    job.id,
                    job.status,
                    job.job_type,
                    job.paper_key.as_deref().unwrap_or("-"),
                    job.model_id.as_deref().unwrap_or("-"),
                    job.updated_at
                );
                if let Some(error_code) = job.error_code.as_deref() {
                    println!("  error_code={error_code}");
                }
                if let Some(error) = job.error.as_deref() {
                    if !error.trim().is_empty() {
                        println!("  error={}", error.trim());
                    }
                }
            }
        }
    }
    Ok(())
}

fn cmd_status(args: AuthorArgs, settings: &Settings) -> Result<()> {
    let storage = Storage::open(&settings.db_path)?;
    let status = StatusService::new(&storage).summary(args.author.as_deref())?;
    println!("papers: {}", status.papers);
    println!("analyzed: {}", status.analyzed);
    println!("stale_papers: {}", status.stale_papers);
    println!("queued_jobs: {}", status.queued_jobs);
    println!("running_jobs: {}", status.running_jobs);
    println!("retry_waiting_jobs: {}", status.retry_waiting_jobs);
    println!("failed_jobs: {}", status.failed_jobs);
    println!("cancelled_jobs: {}", status.cancelled_jobs);
    println!("qa_logs: {}", status.qa_logs);
    if let Some(latency) = status.avg_qa_latency_ms {
        println!("avg_qa_latency_ms: {latency:.0}");
    }
    if let Some(tokens) = status.total_qa_tokens {
        println!("total_qa_tokens: {tokens}");
    }
    if let Some(cost) = status.total_qa_cost_usd {
        println!("total_qa_cost_usd: {cost:.6}");
    }
    Ok(())
}

fn cmd_profile(args: ProfileArgs, settings: &Settings) -> Result<()> {
    let author = resolve_author(args.author.as_deref(), settings)?;
    let storage = Storage::open(&settings.db_path)?;
    if args.rebuild {
        require_llm(settings)?;
        let llm = make_llm(settings)?;
        match ProfileService::new(&storage).rebuild_author_profile(&author, &llm, true)? {
            AuthorProfileRebuild::NoPaperProfiles => {
                println!("no paper profiles for {author}");
            }
            AuthorProfileRebuild::Current { profile_count }
            | AuthorProfileRebuild::Rebuilt { profile_count } => {
                println!("rebuilt author profile with {profile_count} paper profiles");
            }
        }
        return Ok(());
    }
    match ProfileService::new(&storage).author_profile(&author)? {
        AuthorProfileLookup::Found(profile) => {
            println!("{}", serde_json::to_string_pretty(&profile)?);
        }
        AuthorProfileLookup::Missing { .. } => {
            println!("no author profile for {author}");
        }
    }
    Ok(())
}

fn cmd_serve_telegram(settings: &Settings) -> Result<()> {
    require_llm(settings)?;
    let token = settings
        .telegram_bot_token
        .clone()
        .ok_or_else(|| anyhow!("missing TELEGRAM_BOT_TOKEN; run `ppc tg config`"))?;
    let handlers = BotHandlers::new(
        settings.db_path.clone(),
        make_llm(settings)?,
        make_optional_embedding(settings)?,
        settings.default_author.clone(),
    );
    TelegramBot::new(
        token,
        settings.telegram_chat_ids.clone(),
        settings.proxy.clone(),
        handlers,
    )?
    .run_polling()
}

fn resolve_author(author: Option<&str>, settings: &Settings) -> Result<String> {
    author
        .map(str::to_string)
        .or_else(|| settings.default_author.clone())
        .ok_or_else(|| anyhow!(missing_author_message(settings)))
}

fn missing_author_message(settings: &Settings) -> String {
    let mut lines =
        vec!["missing author; pass --author or set a default with `ppc config`.".to_string()];
    lines.push(available_author_hint(settings));
    lines.join("\n")
}

fn available_author_hint(settings: &Settings) -> String {
    let paper_root_authors = paper_root_authors(&settings.paper_root).unwrap_or_default();
    match database_authors(settings) {
        Ok(authors) => format_author_inventory(
            &authors,
            &paper_root_authors,
            settings.default_author.as_deref(),
        ),
        Err(err) => format!(
            "Could not read authors from {}: {err}",
            settings.db_path.display()
        ),
    }
}

fn require_llm(settings: &Settings) -> Result<()> {
    if settings.llm_api_key.is_none() {
        return Err(anyhow!(
            "missing CHECK_PAPER_LLM_API_KEY; run `ppc llm config`"
        ));
    }
    if settings.llm_model.trim().is_empty() {
        return Err(anyhow!(
            "missing CHECK_PAPER_LLM_MODEL; run `ppc llm config`"
        ));
    }
    Ok(())
}

fn make_llm(settings: &Settings) -> Result<OpenAiCompatibleClient> {
    OpenAiCompatibleClient::new(LlmConfig {
        base_url: settings.llm_base_url.clone(),
        api_key: settings.llm_api_key.clone(),
        model: settings.llm_model.clone(),
        proxy: settings.proxy.clone(),
        timeout_secs: settings.llm_timeout_secs,
        tls_backend: settings.llm_tls_backend.clone(),
        prompt_cost_per_1k: settings.llm_prompt_cost_per_1k,
        completion_cost_per_1k: settings.llm_completion_cost_per_1k,
    })
}

fn require_embedding(settings: &Settings) -> Result<()> {
    if settings.embedding_provider.trim() != "openai-compatible" {
        return Err(anyhow!(
            "missing or unsupported CHECK_PAPER_EMBEDDING_PROVIDER; set it to `openai-compatible`"
        ));
    }
    if settings.embedding_api_key.is_none() {
        return Err(anyhow!("missing CHECK_PAPER_EMBEDDING_API_KEY"));
    }
    if settings.embedding_model.trim().is_empty() {
        return Err(anyhow!("missing CHECK_PAPER_EMBEDDING_MODEL"));
    }
    Ok(())
}

fn make_embedding(settings: &Settings) -> Result<OpenAiCompatibleEmbeddingClient> {
    OpenAiCompatibleEmbeddingClient::new(EmbeddingConfig {
        provider: settings.embedding_provider.clone(),
        base_url: settings.embedding_base_url.clone(),
        api_key: settings.embedding_api_key.clone(),
        model: settings.embedding_model.clone(),
        model_version: settings.embedding_model_version.clone(),
        proxy: settings.proxy.clone(),
        timeout_secs: settings.embedding_timeout_secs,
        tls_backend: settings.embedding_tls_backend.clone(),
        batch_size: settings.embedding_batch_size,
    })
}

fn make_optional_embedding(settings: &Settings) -> Result<Option<OpenAiCompatibleEmbeddingClient>> {
    if settings.embedding_provider.trim() != "openai-compatible" {
        return Ok(None);
    }
    if settings.embedding_api_key.is_none() || settings.embedding_model.trim().is_empty() {
        return Ok(None);
    }
    Ok(Some(make_embedding(settings)?))
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;
    use std::fs;
    use std::path::PathBuf;

    use super::{
        AnalysisFailure, AskArgs, PaperRootAuthorSummary, TelegramBotStatus,
        analysis_failure_summary_lines, classify_analysis_error, cmd_ask, format_author_inventory,
        format_cli_chat_ids, format_tg_status, missing_author_message, paper_root_authors,
        redact_secret, resolve_author, should_rebuild_author_profile,
    };
    use crate::config::Settings;
    use crate::papers::models::Paper;
    use crate::storage::AuthorSummary;
    use crate::storage::Storage;
    use serde_json::json;
    use tempfile::tempdir;

    fn settings(default_author: Option<&str>) -> Settings {
        Settings {
            paper_root: PathBuf::from("paper"),
            db_path: PathBuf::from("data/test.sqlite"),
            default_author: default_author.map(str::to_string),
            proxy: None,
            llm_base_url: "https://api.openai.com/v1".to_string(),
            llm_api_key: None,
            llm_model: String::new(),
            llm_timeout_secs: 180,
            llm_tls_backend: "rustls".to_string(),
            llm_prompt_cost_per_1k: None,
            llm_completion_cost_per_1k: None,
            embedding_provider: "disabled".to_string(),
            embedding_base_url: "https://api.openai.com/v1".to_string(),
            embedding_api_key: None,
            embedding_model: String::new(),
            embedding_model_version: None,
            embedding_timeout_secs: 180,
            embedding_tls_backend: "rustls".to_string(),
            embedding_batch_size: 64,
            chunk_max_chars: 3200,
            chunk_overlap: 350,
            chunker_version: "section-char-v1".to_string(),
            telegram_bot_token: None,
            telegram_chat_ids: Vec::new(),
        }
    }

    #[test]
    fn resolve_author_requires_explicit_or_configured_author() {
        let without_default = settings(None);
        let err = resolve_author(None, &without_default)
            .unwrap_err()
            .to_string();
        assert!(err.contains("missing author"));
        assert!(err.contains("ppc ingest"));
        assert_eq!(
            resolve_author(Some("Alice"), &without_default).unwrap(),
            "Alice"
        );

        let with_default = settings(Some("Bob"));
        assert_eq!(resolve_author(None, &with_default).unwrap(), "Bob");
    }

    #[test]
    fn missing_author_message_lists_available_authors() {
        let dir = tempdir().unwrap();
        let db_path = dir.path().join("test.sqlite");
        let mut storage = Storage::open(&db_path).unwrap();
        storage
            .upsert_paper(&test_paper(dir.path(), "Alice", "paper-a"), &[])
            .unwrap();
        drop(storage);
        let mut settings = settings(None);
        settings.db_path = db_path;
        settings.paper_root = dir.path().join("paper");

        let message = missing_author_message(&settings);

        assert!(message.contains("missing author"));
        assert!(message.contains("authors: 1"));
        assert!(message.contains("1. Alice (db: 1 papers; paper/: not found)"));
        assert!(message.contains("--author \"NAME\""));
    }

    #[test]
    fn ask_rejects_empty_question_before_requiring_llm() {
        let err = cmd_ask(
            AskArgs {
                author: Some("Alice".to_string()),
                question: Vec::new(),
            },
            &settings(None),
        )
        .unwrap_err()
        .to_string();

        assert!(err.contains("missing question"));
    }

    #[test]
    fn classifies_analysis_errors_for_retry_visibility() {
        assert_eq!(
            classify_analysis_error(&anyhow::anyhow!("LLM API 请求超时：https://example.test")),
            "llm_timeout"
        );
        assert_eq!(
            classify_analysis_error(&anyhow::anyhow!("PaperProfileV1 missing non-empty title")),
            "schema_error"
        );
        assert_eq!(
            classify_analysis_error(&anyhow::anyhow!("LLM API 返回 HTTP 429：rate limited")),
            "llm_http_error"
        );
    }

    #[test]
    fn formats_analysis_failure_summary_with_next_steps() {
        let lines = analysis_failure_summary_lines(
            "Alice",
            &[
                AnalysisFailure {
                    paper_key: "Alice/paper-a".to_string(),
                    error_code: "schema_error",
                    error: "PaperProfileV1 missing non-empty title".to_string(),
                },
                AnalysisFailure {
                    paper_key: "Alice/paper-b".to_string(),
                    error_code: "llm_timeout",
                    error: "LLM API 请求超时".to_string(),
                },
            ],
        );
        let text = lines.join("\n");

        assert!(text.contains("analysis completed with 2 failed attempts"));
        assert!(text.contains("- schema_error: 1"));
        assert!(text.contains("- llm_timeout: 1"));
        assert!(text.contains("ppc jobs --author \"Alice\" --status failed"));
        assert!(text.contains("ppc analyze --author \"Alice\" --failed-only"));
        assert!(text.contains("Alice/paper-a [schema_error]"));
    }

    #[test]
    fn author_profile_rebuild_only_runs_after_successful_paper_profiles() {
        assert!(should_rebuild_author_profile(false, 1));
        assert!(!should_rebuild_author_profile(false, 0));
        assert!(!should_rebuild_author_profile(true, 1));
    }

    #[test]
    fn formats_authors_with_default_marker() {
        let text = format_author_inventory(
            &[
                AuthorSummary {
                    author: "Alice".to_string(),
                    paper_count: 5,
                },
                AuthorSummary {
                    author: "Bob".to_string(),
                    paper_count: 2,
                },
            ],
            &[],
            Some("Bob"),
        );

        assert!(text.contains("authors: 2"));
        assert!(text.contains("1. Alice (db: 5 papers; paper/: not found)"));
        assert!(text.contains("2. Bob (db: 2 papers; paper/: not found; default)"));
        assert!(text.contains("--author \"NAME\""));
    }

    #[test]
    fn formats_empty_authors_with_next_step() {
        let text = format_author_inventory(&[], &[], None);

        assert!(text.contains("no authors found"));
        assert!(text.contains("ppc ingest --author AUTHOR"));
    }

    #[test]
    fn formats_paper_root_authors_before_ingest() {
        let text = format_author_inventory(
            &[],
            &[PaperRootAuthorSummary {
                author: "Alice".to_string(),
                paper_count: 2,
            }],
            None,
        );

        assert!(text.contains("authors: 1"));
        assert!(text.contains("1. Alice (paper/: 2 papers; not ingested)"));
        assert!(text.contains("ppc ingest --author \"NAME\""));
    }

    #[test]
    fn counts_paper_root_authors_from_article_dirs() {
        let dir = tempdir().unwrap();
        let paper_a = dir.path().join("paper").join("Alice").join("paper-a");
        let paper_b = dir.path().join("paper").join("Alice").join("paper-b");
        let ignored = dir.path().join("paper").join("Bob").join("paper-x");
        fs::create_dir_all(&paper_a).unwrap();
        fs::create_dir_all(&paper_b).unwrap();
        fs::create_dir_all(&ignored).unwrap();
        fs::write(paper_a.join("article.md"), "# A").unwrap();
        fs::write(paper_b.join("article.md"), "# B").unwrap();

        let authors = paper_root_authors(&dir.path().join("paper")).unwrap();

        assert_eq!(
            authors,
            vec![PaperRootAuthorSummary {
                author: "Alice".to_string(),
                paper_count: 2,
            }]
        );
    }

    #[test]
    fn formats_tg_status_without_secrets() {
        let mut settings = settings(None);
        settings.telegram_chat_ids = vec![-1003854490002];
        settings.proxy = Some("socks5://127.0.0.1:7890".to_string());
        let text = format_tg_status(
            &settings,
            &TelegramBotStatus {
                id: 42,
                username: Some("ppc_ethan_bot".to_string()),
                first_name: "ppc".to_string(),
            },
        );

        assert!(text.contains("Telegram status: ok"));
        assert!(text.contains("bot: @ppc_ethan_bot id=42 first_name=ppc"));
        assert!(text.contains("allowed_chats: -1003854490002"));
        assert!(text.contains("proxy: socks5://127.0.0.1:7890"));
        assert!(text.contains("serve_command: ppc serve-telegram"));
    }

    #[test]
    fn formats_tg_status_chat_ids_and_redacts_token() {
        assert_eq!(format_cli_chat_ids(&[]), "all");
        assert_eq!(format_cli_chat_ids(&[1, -2]), "1,-2");
        assert_eq!(
            redact_secret("https://api.telegram.org/bot123:secret/getMe", "123:secret"),
            "https://api.telegram.org/bot<redacted>/getMe"
        );
    }

    fn test_paper(root: &std::path::Path, author: &str, paper_id: &str) -> Paper {
        Paper {
            author: author.to_string(),
            paper_id: paper_id.to_string(),
            paper_dir: root.join(author).join(paper_id),
            article_path: root.join(author).join(paper_id).join("article.md"),
            fetch_result_path: None,
            source_hash: format!("{author}-{paper_id}-hash"),
            metadata: BTreeMap::from([
                ("title".to_string(), format!("{author} {paper_id}")),
                ("year".to_string(), "2024".to_string()),
            ]),
            fetch_result: json!({}),
            raw_body: String::new(),
            clean_text: String::new(),
            sections: vec![],
        }
    }
}
