use anyhow::{Result, anyhow};
use chrono::Utc;
use clap::{Args, Parser, Subcommand, ValueEnum};
use indicatif::{ProgressBar, ProgressStyle};
use reqwest::Proxy;
use reqwest::blocking::ClientBuilder;
use serde::Deserialize;
use serde_json::json;
use std::collections::BTreeMap;
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::process;
use std::thread;
use std::time::Duration;

use crate::bots::handlers::{BotHandlers, BotRuntimeSettings, TELEGRAM_HEARTBEAT_NAME};
use crate::bots::telegram_bot::TelegramBot;
use crate::config::{Settings, config_path, load_config, redacted_config, save_config};
use crate::papers::loader::load_paper;
use crate::papers::scanner::scan_paper_dirs;
use crate::qa::answerer::QaProfileVersion;
use crate::retrieval::chunker::chunk_paper;
use crate::retrieval::embedding::{
    EmbeddingConfig, EmbeddingProvider, OpenAiCompatibleEmbeddingClient,
};
use crate::schemas::paper_profile::PAPER_PROFILE_SCHEMA_VERSION;
use crate::services::analysis::{AnalysisQueueOptions, AnalysisService};
use crate::services::classification::{
    ClassificationOptions, ClassificationReport, ClassificationService,
};
use crate::services::comprehension::{
    ComprehensionService, ProfileDiffReport, ProfileGateReport, S3ComprehensionOptions,
    S3ComprehensionReport, S4AuthorComprehensionOptions, S4AuthorComprehensionReport,
};
use crate::services::embedding::EmbeddingService;
use crate::services::eval::EvalService;
use crate::services::extraction::{ExtractionService, V2ExtractionOptions, V2ExtractionReport};
use crate::services::jobs::JobService;
use crate::services::profile::{AuthorProfileLookup, AuthorProfileRebuild, ProfileService};
use crate::services::qa::{QaProfileVersionPreference, QaService};
use crate::services::status::StatusService;
use crate::storage::{AuthorSummary, PaperProfileMetadata, RuntimeHeartbeat, Storage};
use crate::understanding::llm::{LlmConfig, OpenAiCompatibleClient};
use crate::understanding::paper_analyzer::{analyze_paper, extract_section_facts};
use crate::understanding::prompts::PAPER_PROFILE_PROMPT_VERSION;

const TELEGRAM_STATUS_TIMEOUT_SECS: u64 = 20;
const TELEGRAM_HEARTBEAT_STALE_SECS: i64 = 90;
const TELEGRAM_HEALTH_CHECK_INTERVAL_SECS: i64 = 300;

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
    #[command(about = "Classify chunks for the V2 comprehension pipeline")]
    Classify(ClassifyArgs),
    #[command(about = "Extract V2 chunk facts from classified chunks")]
    Extract(ExtractArgs),
    #[command(about = "Build V2 paper or author profiles")]
    Comprehend(ComprehendArgs),
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
    #[command(about = "Show, rebuild, or diff profiles")]
    Profile(ProfileArgs),
    #[command(about = "Create a SQLite database backup before full production runs")]
    Backup(BackupArgs),
    #[command(about = "Print a production preflight checklist without running analysis")]
    Preflight(PreflightArgs),
    #[command(about = "Start the Telegram bot polling loop")]
    ServeTelegram,
}

#[derive(Args)]
struct ConfigArgs {
    #[arg(long, help = "Print saved config values without prompting")]
    show: bool,
}

#[derive(Args)]
struct BackupArgs {
    #[arg(
        long,
        help = "Backup output path; defaults beside the configured database"
    )]
    output: Option<PathBuf>,
}

#[derive(Args)]
struct PreflightArgs {
    #[arg(long, help = "Author directory under the paper root")]
    author: Option<String>,
    #[arg(long, help = "Maximum number of papers planned for the next sync")]
    limit: Option<usize>,
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
    #[command(about = "Check whether the local Telegram polling heartbeat is fresh")]
    Health(TgHealthArgs),
    #[command(about = "Print a launchd, systemd, or logrotate template for Telegram polling")]
    ServiceTemplate(TgServiceTemplateArgs),
    #[command(about = "Write a launchd, systemd, or logrotate template to a user-level path")]
    ServiceInstall(TgServiceInstallArgs),
    #[command(about = "Check whether the installed Telegram service template matches")]
    ServiceCheck(TgServiceCheckArgs),
}

#[derive(Args)]
struct TgConfigArgs {
    #[arg(long, help = "Print saved Telegram config values without prompting")]
    show: bool,
}

#[derive(Args)]
struct TgHealthArgs {
    #[arg(
        long,
        help = "Exit with a non-zero status when the local polling heartbeat is missing or stale"
    )]
    strict: bool,
    #[arg(
        long,
        help = "Send a Telegram alert when the local polling heartbeat is missing or stale"
    )]
    notify: bool,
    #[arg(
        long = "notify-chat-id",
        help = "Chat ID to notify on failed health checks; defaults to TELEGRAM_CHAT_IDS"
    )]
    notify_chat_ids: Vec<i64>,
}

#[derive(Args)]
struct TgServiceTemplateArgs {
    #[arg(long, value_enum, default_value = "launchd")]
    kind: TgServiceTemplateKind,
    #[arg(long, help = "Path to the ppc binary used in the generated template")]
    bin: Option<PathBuf>,
    #[arg(long, help = "Working directory used by ppc serve-telegram")]
    workdir: Option<PathBuf>,
    #[arg(long, help = "Log file path used by service and logrotate templates")]
    log: Option<PathBuf>,
}

#[derive(Args)]
struct TgServiceInstallArgs {
    #[arg(long, value_enum, default_value = "launchd")]
    kind: TgServiceTemplateKind,
    #[arg(long, help = "Path to the ppc binary used in the installed template")]
    bin: Option<PathBuf>,
    #[arg(long, help = "Working directory used by ppc serve-telegram")]
    workdir: Option<PathBuf>,
    #[arg(long, help = "Log file path used by service and logrotate templates")]
    log: Option<PathBuf>,
    #[arg(
        long,
        help = "Template output path; defaults to a user-level service path"
    )]
    output: Option<PathBuf>,
    #[arg(long, help = "Print the install plan without writing files")]
    dry_run: bool,
    #[arg(long, help = "Overwrite an existing installed template")]
    force: bool,
}

#[derive(Args)]
struct TgServiceCheckArgs {
    #[arg(long, value_enum, default_value = "launchd")]
    kind: TgServiceTemplateKind,
    #[arg(
        long,
        help = "Path to the ppc binary expected in the installed template"
    )]
    bin: Option<PathBuf>,
    #[arg(long, help = "Working directory expected in the installed template")]
    workdir: Option<PathBuf>,
    #[arg(
        long,
        help = "Log file path expected in the service or logrotate template"
    )]
    log: Option<PathBuf>,
    #[arg(
        long,
        help = "Installed template path; defaults to a user-level service path"
    )]
    output: Option<PathBuf>,
}

#[derive(Copy, Clone, Debug, PartialEq, Eq, ValueEnum)]
enum TgServiceTemplateKind {
    Launchd,
    LaunchdHealth,
    Systemd,
    Logrotate,
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
    #[arg(
        long,
        default_value_t = 3,
        help = "Maximum embedding API attempts per batch"
    )]
    max_attempts: usize,
}

#[derive(Args, Clone)]
struct ClassifyArgs {
    #[arg(long, help = "Author directory under the paper root")]
    author: Option<String>,
    #[arg(long, help = "Maximum number of chunks to classify")]
    limit: Option<usize>,
    #[arg(
        long,
        help = "Reclassify chunks even when current classification exists"
    )]
    force: bool,
    #[arg(
        long,
        help = "Print the classification plan without writing the database"
    )]
    dry_run: bool,
}

#[derive(Args, Clone)]
struct ExtractArgs {
    #[arg(long, help = "Author directory under the paper root")]
    author: Option<String>,
    #[arg(long, help = "Run the V2 chunk fact extraction pipeline")]
    v2: bool,
    #[arg(long, help = "Maximum number of chunks to scan")]
    limit: Option<usize>,
    #[arg(long, help = "Re-extract chunks even when current chunk facts exist")]
    force: bool,
    #[arg(
        long,
        help = "Only retry chunks recorded as failed in a previous V2 extraction"
    )]
    failed_only: bool,
    #[arg(long, help = "Print the extraction plan without writing the database")]
    dry_run: bool,
}

#[derive(Args, Clone)]
struct ComprehendArgs {
    #[arg(long, help = "Author directory under the paper root")]
    author: Option<String>,
    #[arg(long, help = "Run the V2 paper comprehension pipeline")]
    v2: bool,
    #[arg(
        long,
        help = "Build the V2 author profile from saved V2 paper profiles"
    )]
    author_profile: bool,
    #[arg(long, help = "Maximum number of papers to scan")]
    limit: Option<usize>,
    #[arg(long, help = "Rebuild profiles even when current V2 profiles exist")]
    force: bool,
    #[arg(
        long,
        help = "Build V2 paper profiles only for papers with V1 profiles"
    )]
    profiled_only: bool,
    #[arg(long, help = "Skip LLM synthesis and build deterministic V2 profiles")]
    deterministic: bool,
    #[arg(long, help = "Build profiles without writing the database")]
    dry_run: bool,
}

#[derive(Args)]
struct AskArgs {
    #[arg(long, help = "Author directory under the paper root")]
    author: Option<String>,
    #[arg(
        long,
        value_enum,
        help = "Profile source used for QA context; defaults to CHECK_PAPER_QA_PROFILE_VERSION"
    )]
    profile_version: Option<AskProfileVersionArg>,
    #[arg(help = "Question text")]
    question: Vec<String>,
}

#[derive(Copy, Clone, Debug, PartialEq, Eq, ValueEnum)]
enum AskProfileVersionArg {
    V1,
    V2,
    Auto,
}

impl From<AskProfileVersionArg> for QaProfileVersionPreference {
    fn from(value: AskProfileVersionArg) -> Self {
        match value {
            AskProfileVersionArg::V1 => QaProfileVersionPreference::V1,
            AskProfileVersionArg::V2 => QaProfileVersionPreference::V2,
            AskProfileVersionArg::Auto => QaProfileVersionPreference::Auto,
        }
    }
}

#[derive(Copy, Clone, Debug, PartialEq, Eq, ValueEnum)]
enum QaProfileVersionArg {
    V1,
    V2,
}

impl From<QaProfileVersionArg> for QaProfileVersion {
    fn from(value: QaProfileVersionArg) -> Self {
        match value {
            QaProfileVersionArg::V1 => QaProfileVersion::V1,
            QaProfileVersionArg::V2 => QaProfileVersion::V2,
        }
    }
}

#[derive(Args)]
struct EvalArgs {
    #[arg(long, help = "Path to a JSON fixture of golden questions")]
    fixture: PathBuf,
    #[arg(
        long,
        default_value_t = 8,
        help = "Number of retrieved chunks to evaluate"
    )]
    top_k: usize,
    #[arg(
        long,
        value_enum,
        default_value = "v1",
        help = "Profile source used when planning QA routes"
    )]
    profile_version: QaProfileVersionArg,
    #[arg(
        long,
        help = "Run both V1 and V2 eval and compare default-switch metric thresholds"
    )]
    compare_profile_versions: bool,
    #[arg(
        long,
        help = "Return a non-zero exit code when --compare-profile-versions recommends hold"
    )]
    fail_on_hold: bool,
    #[arg(
        long,
        default_value_t = 0.0,
        help = "Maximum allowed V2 retrieval hit@k drop when comparing profile versions"
    )]
    max_retrieval_hit_drop: f64,
    #[arg(
        long,
        default_value_t = 0.02,
        help = "Maximum allowed V2 citation precision drop when comparing profile versions"
    )]
    max_citation_precision_drop: f64,
    #[arg(
        long,
        default_value_t = 0.0,
        help = "Maximum allowed V2 required-answer coverage drop when comparing profile versions"
    )]
    max_answer_contains_required_drop: f64,
    #[arg(
        long,
        default_value_t = 1.0,
        help = "Minimum required V2 retrieval hit@k when comparing profile versions"
    )]
    min_candidate_retrieval_hit_at_k: f64,
    #[arg(
        long,
        default_value_t = 0.4,
        help = "Minimum required V2 citation precision when comparing profile versions"
    )]
    min_candidate_citation_precision: f64,
    #[arg(
        long,
        default_value_t = 1.0,
        help = "Minimum required V2 required-answer coverage when comparing profile versions"
    )]
    min_candidate_answer_contains_required: f64,
    #[arg(long, help = "Include per-question trace details in the JSON report")]
    trace: bool,
    #[arg(long, help = "Render a Markdown baseline report instead of JSON")]
    baseline_markdown: bool,
    #[arg(long, help = "Write the eval report to a path instead of stdout")]
    output: Option<PathBuf>,
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
    #[command(about = "Show recent Telegram delivery logs")]
    Telegram(LogTelegramArgs),
}

#[derive(Args)]
struct LogQaArgs {
    #[arg(long, help = "Filter QA logs by author")]
    author: Option<String>,
    #[arg(long, default_value_t = 10, help = "Maximum QA logs to print")]
    last: usize,
    #[arg(long, help = "Show grouped QA error counts instead of log rows")]
    errors: bool,
    #[arg(long, help = "Show daily QA trend rows instead of log rows")]
    trend: bool,
    #[arg(
        long,
        default_value_t = 14,
        help = "Maximum active days to print for --trend"
    )]
    days: usize,
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
struct LogTelegramArgs {
    #[arg(long, help = "Filter Telegram delivery logs by chat id")]
    chat_id: Option<i64>,
    #[arg(long, help = "Show grouped Telegram delivery counts instead of rows")]
    summary: bool,
    #[arg(long, help = "Show daily Telegram delivery trend rows instead of rows")]
    trend: bool,
    #[arg(long, help = "Include matching QA log fields by Telegram chat/job id")]
    with_qa: bool,
    #[arg(
        long,
        default_value_t = 14,
        help = "Maximum active days to print for --trend"
    )]
    days: usize,
    #[arg(
        long,
        default_value_t = 10,
        help = "Maximum Telegram delivery logs to print"
    )]
    last: usize,
}

#[derive(Args)]
struct ProfileArgs {
    #[command(subcommand)]
    command: Option<ProfileCommand>,
    #[arg(long, help = "Author directory under the paper root")]
    author: Option<String>,
    #[arg(long, help = "Show the V2 author profile")]
    v2: bool,
    #[arg(
        long,
        help = "Force rebuilding the author-level profile from paper profiles"
    )]
    rebuild: bool,
}

#[derive(Subcommand)]
enum ProfileCommand {
    #[command(about = "Compare V1 paper profiles with V2 paper profiles")]
    Diff(ProfileDiffArgs),
    #[command(about = "Check a profile diff review Markdown for human signoff")]
    Signoff(ProfileSignoffArgs),
    #[command(about = "Check whether V2 profiles are ready to gate into default QA")]
    Gate(ProfileGateArgs),
}

#[derive(Args)]
struct ProfileDiffArgs {
    #[arg(long, help = "Author directory under the paper root")]
    author: Option<String>,
    #[arg(long, help = "Render a Markdown profile diff review instead of text")]
    markdown: bool,
    #[arg(
        long,
        help = "Write the profile diff report to a path instead of stdout"
    )]
    output: Option<PathBuf>,
}

#[derive(Args)]
struct ProfileSignoffArgs {
    #[arg(long, help = "Path to a profile diff review Markdown file")]
    input: PathBuf,
    #[arg(long, help = "Return a non-zero exit code when signoff is not ready")]
    fail_on_hold: bool,
}

#[derive(Args)]
struct ProfileGateArgs {
    #[arg(long, help = "Author directory under the paper root")]
    author: Option<String>,
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
            TgCommand::Health(args) => {
                let settings = Settings::from_sources();
                cmd_tg_health(args, &settings)
            }
            TgCommand::ServiceTemplate(args) => {
                let settings = Settings::from_sources();
                cmd_tg_service_template(args, &settings)
            }
            TgCommand::ServiceInstall(args) => {
                let settings = Settings::from_sources();
                cmd_tg_service_install(args, &settings)
            }
            TgCommand::ServiceCheck(args) => {
                let settings = Settings::from_sources();
                cmd_tg_service_check(args, &settings)
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
                Command::Classify(args) => cmd_classify(args, &settings),
                Command::Extract(args) => cmd_extract(args, &settings),
                Command::Comprehend(args) => cmd_comprehend(args, &settings),
                Command::Embed(args) => cmd_embed(args, &settings),
                Command::Ask(args) => cmd_ask(args, &settings),
                Command::Eval(args) => cmd_eval(args, &settings),
                Command::Jobs(args) => cmd_jobs(args, &settings),
                Command::Logs { command } => cmd_logs(command, &settings),
                Command::Status(args) => cmd_status(args, &settings),
                Command::Profile(args) => cmd_profile(args, &settings),
                Command::Backup(args) => cmd_backup(args, &settings),
                Command::Preflight(args) => cmd_preflight(args, &settings),
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
            "CHECK_PAPER_QA_PROFILE_VERSION",
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
        "CHECK_PAPER_QA_PROFILE_VERSION".to_string(),
        prompt_value(
            "qa-profile-version (v1/v2/auto)",
            current
                .get("CHECK_PAPER_QA_PROFILE_VERSION")
                .map(String::as_str)
                .unwrap_or("v1"),
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
        print_config(&[
            "TELEGRAM_BOT_TOKEN",
            "TELEGRAM_CHAT_IDS",
            "TELEGRAM_ADMIN_USER_IDS",
        ])?;
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
    updates.insert(
        "TELEGRAM_ADMIN_USER_IDS".to_string(),
        prompt_value(
            "admin-user-ids",
            current
                .get("TELEGRAM_ADMIN_USER_IDS")
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

fn cmd_tg_health(args: TgHealthArgs, settings: &Settings) -> Result<()> {
    settings.ensure_dirs()?;
    let storage = Storage::open(&settings.db_path)?;
    let heartbeat = storage.runtime_heartbeat(TELEGRAM_HEARTBEAT_NAME)?;
    let status = tg_health_status(heartbeat.as_ref());
    println!("{}", format_tg_health(settings, heartbeat.as_ref()));
    let mut notify_error = None;
    if args.notify && status != "ok" {
        if let Err(error) =
            send_tg_health_alert(settings, &args.notify_chat_ids, heartbeat.as_ref())
        {
            notify_error = Some(error);
        }
    }
    if args.strict && status != "ok" {
        if let Some(error) = notify_error {
            return Err(anyhow!(
                "Telegram health check failed: {status}; alert failed: {error}"
            ));
        }
        return Err(anyhow!("Telegram health check failed: {status}"));
    }
    if let Some(error) = notify_error {
        return Err(error);
    }
    Ok(())
}

fn cmd_tg_service_template(args: TgServiceTemplateArgs, settings: &Settings) -> Result<()> {
    let bin = args.bin.unwrap_or_else(default_tg_service_bin_path);
    let workdir = args.workdir.unwrap_or_else(default_tg_service_workdir);
    let log = args
        .log
        .unwrap_or_else(|| default_tg_service_log_path(settings, &workdir));
    println!(
        "{}",
        format_tg_service_template(args.kind, &bin, &workdir, &log)
    );
    Ok(())
}

fn cmd_tg_service_install(args: TgServiceInstallArgs, settings: &Settings) -> Result<()> {
    let bin = args.bin.unwrap_or_else(default_tg_service_bin_path);
    let workdir = args.workdir.unwrap_or_else(default_tg_service_workdir);
    let log = args
        .log
        .unwrap_or_else(|| default_tg_service_log_path(settings, &workdir));
    let output = match args.output {
        Some(path) => path,
        None => default_tg_service_install_path(args.kind)?,
    };
    let template = format_tg_service_template(args.kind, &bin, &workdir, &log);
    let written = install_tg_service_template(&output, &template, args.force, args.dry_run)?;
    println!(
        "{}",
        format_tg_service_install_report(args.kind, &output, written, args.dry_run)
    );
    Ok(())
}

fn cmd_tg_service_check(args: TgServiceCheckArgs, settings: &Settings) -> Result<()> {
    let bin = args.bin.unwrap_or_else(default_tg_service_bin_path);
    let workdir = args.workdir.unwrap_or_else(default_tg_service_workdir);
    let log = args
        .log
        .unwrap_or_else(|| default_tg_service_log_path(settings, &workdir));
    let output = match args.output {
        Some(path) => path,
        None => default_tg_service_install_path(args.kind)?,
    };
    let expected = format_tg_service_template(args.kind, &bin, &workdir, &log);
    let check = check_tg_service_template(&output, &expected)?;
    println!(
        "{}",
        format_tg_service_check_report(args.kind, &output, &check)
    );
    Ok(())
}

fn default_tg_service_bin_path() -> PathBuf {
    std::env::current_exe().unwrap_or_else(|_| PathBuf::from("ppc"))
}

fn default_tg_service_workdir() -> PathBuf {
    std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."))
}

fn default_tg_service_log_path(settings: &Settings, workdir: &Path) -> PathBuf {
    let log_path = settings
        .db_path
        .parent()
        .filter(|path| !path.as_os_str().is_empty())
        .map(|path| path.join("ppc-telegram.log"))
        .unwrap_or_else(|| PathBuf::from("ppc-telegram.log"));
    if log_path.is_absolute() {
        log_path
    } else {
        workdir.join(log_path)
    }
}

fn default_tg_service_install_path(kind: TgServiceTemplateKind) -> Result<PathBuf> {
    let home = home_dir()?;
    Ok(match kind {
        TgServiceTemplateKind::Launchd => home
            .join("Library")
            .join("LaunchAgents")
            .join("com.check-paper.telegram.plist"),
        TgServiceTemplateKind::LaunchdHealth => home
            .join("Library")
            .join("LaunchAgents")
            .join("com.check-paper.telegram-health.plist"),
        TgServiceTemplateKind::Systemd => home
            .join(".config")
            .join("systemd")
            .join("user")
            .join("check-paper-telegram.service"),
        TgServiceTemplateKind::Logrotate => home
            .join(".config")
            .join("logrotate.d")
            .join("check-paper-telegram"),
    })
}

fn home_dir() -> Result<PathBuf> {
    std::env::var_os("HOME")
        .map(PathBuf::from)
        .filter(|path| !path.as_os_str().is_empty())
        .ok_or_else(|| anyhow!("missing HOME; pass --output for the install path"))
}

fn install_tg_service_template(
    output: &Path,
    template: &str,
    force: bool,
    dry_run: bool,
) -> Result<bool> {
    if dry_run {
        return Ok(false);
    }
    if output.exists() && !force {
        return Err(anyhow!(
            "{} already exists; pass --force to overwrite",
            output.display()
        ));
    }
    if let Some(parent) = output.parent().filter(|path| !path.as_os_str().is_empty()) {
        std::fs::create_dir_all(parent)
            .map_err(|error| anyhow!("failed to create {}: {error}", parent.display()))?;
    }
    std::fs::write(output, template)
        .map_err(|error| anyhow!("failed to write {}: {error}", output.display()))?;
    Ok(true)
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct TgServiceCheck {
    installed: bool,
    matches_expected: Option<bool>,
    actual_bytes: Option<usize>,
    expected_bytes: usize,
}

fn check_tg_service_template(output: &Path, expected: &str) -> Result<TgServiceCheck> {
    let expected_bytes = expected.len();
    if !output.exists() {
        return Ok(TgServiceCheck {
            installed: false,
            matches_expected: None,
            actual_bytes: None,
            expected_bytes,
        });
    }
    let actual = std::fs::read(output)
        .map_err(|error| anyhow!("failed to read {}: {error}", output.display()))?;
    Ok(TgServiceCheck {
        installed: true,
        matches_expected: Some(actual == expected.as_bytes()),
        actual_bytes: Some(actual.len()),
        expected_bytes,
    })
}

fn telegram_get_me(token: &str, proxy: Option<&str>) -> Result<TelegramBotStatus> {
    let mut builder = ClientBuilder::new()
        .use_rustls_tls()
        .timeout(Duration::from_secs(TELEGRAM_STATUS_TIMEOUT_SECS));
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

fn format_tg_service_template(
    kind: TgServiceTemplateKind,
    bin: &Path,
    workdir: &Path,
    log: &Path,
) -> String {
    match kind {
        TgServiceTemplateKind::Launchd => format_launchd_template(bin, workdir, log),
        TgServiceTemplateKind::LaunchdHealth => format_launchd_health_template(bin, workdir, log),
        TgServiceTemplateKind::Systemd => format_systemd_template(bin, workdir, log),
        TgServiceTemplateKind::Logrotate => format_logrotate_template(log),
    }
}

fn format_launchd_template(bin: &Path, workdir: &Path, log: &Path) -> String {
    let bin = xml_escape(&bin.display().to_string());
    let workdir = xml_escape(&workdir.display().to_string());
    let log = xml_escape(&log.display().to_string());
    [
        r#"<?xml version="1.0" encoding="UTF-8"?>"#.to_string(),
        r#"<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">"#.to_string(),
        r#"<plist version="1.0">"#.to_string(),
        "<dict>".to_string(),
        "  <key>Label</key>".to_string(),
        "  <string>com.check-paper.telegram</string>".to_string(),
        "  <key>ProgramArguments</key>".to_string(),
        "  <array>".to_string(),
        format!("    <string>{bin}</string>"),
        "    <string>serve-telegram</string>".to_string(),
        "  </array>".to_string(),
        "  <key>WorkingDirectory</key>".to_string(),
        format!("  <string>{workdir}</string>"),
        "  <key>RunAtLoad</key>".to_string(),
        "  <true/>".to_string(),
        "  <key>KeepAlive</key>".to_string(),
        "  <true/>".to_string(),
        "  <key>StandardOutPath</key>".to_string(),
        format!("  <string>{log}</string>"),
        "  <key>StandardErrorPath</key>".to_string(),
        format!("  <string>{log}</string>"),
        "</dict>".to_string(),
        "</plist>".to_string(),
    ]
    .join("\n")
}

fn format_launchd_health_template(bin: &Path, workdir: &Path, log: &Path) -> String {
    let bin = xml_escape(&bin.display().to_string());
    let workdir = xml_escape(&workdir.display().to_string());
    let log = xml_escape(&log.display().to_string());
    [
        r#"<?xml version="1.0" encoding="UTF-8"?>"#.to_string(),
        r#"<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">"#.to_string(),
        r#"<plist version="1.0">"#.to_string(),
        "<dict>".to_string(),
        "  <key>Label</key>".to_string(),
        "  <string>com.check-paper.telegram-health</string>".to_string(),
        "  <key>ProgramArguments</key>".to_string(),
        "  <array>".to_string(),
        format!("    <string>{bin}</string>"),
        "    <string>tg</string>".to_string(),
        "    <string>health</string>".to_string(),
        "    <string>--strict</string>".to_string(),
        "    <string>--notify</string>".to_string(),
        "  </array>".to_string(),
        "  <key>WorkingDirectory</key>".to_string(),
        format!("  <string>{workdir}</string>"),
        "  <key>RunAtLoad</key>".to_string(),
        "  <true/>".to_string(),
        "  <key>StartInterval</key>".to_string(),
        format!("  <integer>{TELEGRAM_HEALTH_CHECK_INTERVAL_SECS}</integer>"),
        "  <key>StandardOutPath</key>".to_string(),
        format!("  <string>{log}</string>"),
        "  <key>StandardErrorPath</key>".to_string(),
        format!("  <string>{log}</string>"),
        "</dict>".to_string(),
        "</plist>".to_string(),
    ]
    .join("\n")
}

fn format_systemd_template(bin: &Path, workdir: &Path, log: &Path) -> String {
    let bin = systemd_exec_arg(&bin.display().to_string());
    let workdir = workdir.display().to_string();
    let log = log.display().to_string();
    [
        "[Unit]".to_string(),
        "Description=check-paper Telegram bot polling service".to_string(),
        "After=network-online.target".to_string(),
        "Wants=network-online.target".to_string(),
        String::new(),
        "[Service]".to_string(),
        "Type=simple".to_string(),
        format!("WorkingDirectory={workdir}"),
        format!("ExecStart={bin} serve-telegram"),
        "Restart=always".to_string(),
        "RestartSec=5".to_string(),
        format!("StandardOutput=append:{log}"),
        format!("StandardError=append:{log}"),
        String::new(),
        "[Install]".to_string(),
        "WantedBy=default.target".to_string(),
    ]
    .join("\n")
}

fn format_logrotate_template(log: &Path) -> String {
    let log = log.display().to_string();
    [
        format!("{log} {{"),
        "    daily".to_string(),
        "    rotate 14".to_string(),
        "    compress".to_string(),
        "    missingok".to_string(),
        "    notifempty".to_string(),
        "    copytruncate".to_string(),
        "}".to_string(),
    ]
    .join("\n")
}

fn format_tg_service_install_report(
    kind: TgServiceTemplateKind,
    output: &Path,
    written: bool,
    dry_run: bool,
) -> String {
    let action = if dry_run {
        "dry_run"
    } else if written {
        "written"
    } else {
        "skipped"
    };
    let mut lines = vec![
        "Telegram service install".to_string(),
        format!("kind: {}", tg_service_kind_name(kind)),
        format!("path: {}", output.display()),
        format!("status: {action}"),
        "next_steps:".to_string(),
    ];
    lines.extend(
        tg_service_install_next_steps(kind, output)
            .into_iter()
            .map(|step| format!("- {step}")),
    );
    lines.join("\n")
}

fn format_tg_service_check_report(
    kind: TgServiceTemplateKind,
    output: &Path,
    check: &TgServiceCheck,
) -> String {
    let matches_expected = match check.matches_expected {
        Some(true) => "yes",
        Some(false) => "no",
        None => "no_file",
    };
    let actual_bytes = check
        .actual_bytes
        .map(|bytes| bytes.to_string())
        .unwrap_or_else(|| "<missing>".to_string());
    let mut lines = vec![
        "Telegram service check".to_string(),
        format!("kind: {}", tg_service_kind_name(kind)),
        format!("path: {}", output.display()),
        format!("installed: {}", if check.installed { "yes" } else { "no" }),
        format!("matches_expected_template: {matches_expected}"),
        format!("expected_bytes: {}", check.expected_bytes),
        format!("actual_bytes: {actual_bytes}"),
        "next_steps:".to_string(),
    ];
    lines.extend(
        tg_service_check_next_steps(kind, output, check)
            .into_iter()
            .map(|step| format!("- {step}")),
    );
    lines.join("\n")
}

fn tg_service_install_next_steps(kind: TgServiceTemplateKind, output: &Path) -> Vec<String> {
    let path = output.display();
    match kind {
        TgServiceTemplateKind::Launchd => vec![
            format!("launchctl bootstrap gui/$(id -u) {path}"),
            "launchctl kickstart -k gui/$(id -u)/com.check-paper.telegram".to_string(),
            "ppc tg health --strict --notify".to_string(),
        ],
        TgServiceTemplateKind::LaunchdHealth => vec![
            format!("launchctl bootstrap gui/$(id -u) {path}"),
            "launchctl kickstart -k gui/$(id -u)/com.check-paper.telegram-health".to_string(),
            "confirm TELEGRAM_CHAT_IDS or --notify-chat-id is configured before relying on alerts"
                .to_string(),
        ],
        TgServiceTemplateKind::Systemd => vec![
            "systemctl --user daemon-reload".to_string(),
            "systemctl --user enable --now check-paper-telegram.service".to_string(),
            "ppc tg health --strict --notify".to_string(),
        ],
        TgServiceTemplateKind::Logrotate => vec![
            format!("logrotate -s ~/.local/state/check-paper/logrotate.status {path}"),
            "schedule that command with launchd, systemd timer, or cron".to_string(),
        ],
    }
}

fn tg_service_check_next_steps(
    kind: TgServiceTemplateKind,
    output: &Path,
    check: &TgServiceCheck,
) -> Vec<String> {
    let path = output.display();
    if !check.installed {
        let mut steps = vec![format!(
            "ppc tg service-install --kind {} --output {path}",
            tg_service_kind_name(kind)
        )];
        steps.extend(tg_service_install_next_steps(kind, output));
        return steps;
    }
    if check.matches_expected == Some(false) {
        let mut steps = vec![format!(
            "ppc tg service-install --kind {} --output {path} --force",
            tg_service_kind_name(kind)
        )];
        steps.extend(tg_service_install_next_steps(kind, output));
        return steps;
    }
    match kind {
        TgServiceTemplateKind::Launchd => vec![
            "launchctl print gui/$(id -u)/com.check-paper.telegram".to_string(),
            "ppc tg health --strict --notify".to_string(),
        ],
        TgServiceTemplateKind::LaunchdHealth => vec![
            "launchctl print gui/$(id -u)/com.check-paper.telegram-health".to_string(),
            "launchctl kickstart -k gui/$(id -u)/com.check-paper.telegram-health".to_string(),
            "confirm TELEGRAM_CHAT_IDS or --notify-chat-id is configured before relying on alerts"
                .to_string(),
        ],
        TgServiceTemplateKind::Systemd => vec![
            "systemctl --user status check-paper-telegram.service".to_string(),
            "ppc tg health --strict --notify".to_string(),
        ],
        TgServiceTemplateKind::Logrotate => vec![
            format!("logrotate -d -s ~/.local/state/check-paper/logrotate.status {path}"),
            "schedule that command with launchd, systemd timer, or cron".to_string(),
        ],
    }
}

fn tg_service_kind_name(kind: TgServiceTemplateKind) -> &'static str {
    match kind {
        TgServiceTemplateKind::Launchd => "launchd",
        TgServiceTemplateKind::LaunchdHealth => "launchd-health",
        TgServiceTemplateKind::Systemd => "systemd",
        TgServiceTemplateKind::Logrotate => "logrotate",
    }
}

fn xml_escape(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&apos;")
}

fn systemd_exec_arg(value: &str) -> String {
    if !value
        .chars()
        .any(|character| character.is_whitespace() || matches!(character, '"' | '\\'))
    {
        return value.to_string();
    }
    format!("\"{}\"", value.replace('\\', "\\\\").replace('"', "\\\""))
}

fn tg_health_status(heartbeat: Option<&RuntimeHeartbeat>) -> &'static str {
    match heartbeat {
        None => "missing",
        Some(heartbeat)
            if heartbeat
                .age_seconds
                .is_some_and(|age| age <= TELEGRAM_HEARTBEAT_STALE_SECS) =>
        {
            "ok"
        }
        Some(_) => "stale",
    }
}

fn format_tg_health(settings: &Settings, heartbeat: Option<&RuntimeHeartbeat>) -> String {
    let Some(heartbeat) = heartbeat else {
        return [
            "Telegram health: missing".to_string(),
            "heartbeat: missing".to_string(),
            format!("db_path: {}", settings.db_path.display()),
            "serve_command: ppc serve-telegram".to_string(),
        ]
        .join("\n");
    };
    let age = heartbeat
        .age_seconds
        .map(|value| value.to_string())
        .unwrap_or_else(|| "-".to_string());
    [
        format!("Telegram health: {}", tg_health_status(Some(heartbeat))),
        format!(
            "heartbeat: {} status={} updated={} age_seconds={}",
            heartbeat.name, heartbeat.status, heartbeat.updated_at, age
        ),
        format!("stale_after_seconds: {TELEGRAM_HEARTBEAT_STALE_SECS}"),
        format!("db_path: {}", settings.db_path.display()),
        "serve_command: ppc serve-telegram".to_string(),
    ]
    .join("\n")
}

fn send_tg_health_alert(
    settings: &Settings,
    notify_chat_ids: &[i64],
    heartbeat: Option<&RuntimeHeartbeat>,
) -> Result<()> {
    let token = settings.telegram_bot_token.as_deref().ok_or_else(|| {
        anyhow!("missing TELEGRAM_BOT_TOKEN; run `ppc tg config` or omit --notify")
    })?;
    let chat_ids = tg_health_notify_chat_ids(settings, notify_chat_ids)?;
    let message = format_tg_health_alert(settings, heartbeat);
    for chat_id in &chat_ids {
        telegram_send_message(token, settings.proxy.as_deref(), *chat_id, &message)?;
    }
    println!(
        "Telegram health alert sent: chats={}",
        format_cli_chat_ids(&chat_ids)
    );
    Ok(())
}

fn tg_health_notify_chat_ids(settings: &Settings, notify_chat_ids: &[i64]) -> Result<Vec<i64>> {
    let chat_ids = if notify_chat_ids.is_empty() {
        settings.telegram_chat_ids.clone()
    } else {
        notify_chat_ids.to_vec()
    };
    if chat_ids.is_empty() {
        return Err(anyhow!(
            "missing notify chat IDs; set TELEGRAM_CHAT_IDS or pass --notify-chat-id"
        ));
    }
    Ok(chat_ids)
}

fn format_tg_health_alert(settings: &Settings, heartbeat: Option<&RuntimeHeartbeat>) -> String {
    let status = tg_health_status(heartbeat);
    let mut lines = vec![
        "check-paper Telegram polling health failed".to_string(),
        format!("status: {status}"),
    ];
    match heartbeat {
        None => lines.push("heartbeat: missing".to_string()),
        Some(heartbeat) => {
            let age = heartbeat
                .age_seconds
                .map(|value| value.to_string())
                .unwrap_or_else(|| "-".to_string());
            lines.push(format!(
                "heartbeat: {} status={} updated={} age_seconds={}",
                heartbeat.name, heartbeat.status, heartbeat.updated_at, age
            ));
            lines.push(format!(
                "stale_after_seconds: {TELEGRAM_HEARTBEAT_STALE_SECS}"
            ));
        }
    }
    lines.extend([
        format!("db_path: {}", settings.db_path.display()),
        "check: ppc tg health --strict".to_string(),
        "restart: ppc serve-telegram".to_string(),
    ]);
    lines.join("\n")
}

fn telegram_send_message(token: &str, proxy: Option<&str>, chat_id: i64, text: &str) -> Result<()> {
    let mut builder = ClientBuilder::new()
        .use_rustls_tls()
        .timeout(Duration::from_secs(TELEGRAM_STATUS_TIMEOUT_SECS));
    if let Some(proxy) = proxy {
        builder = builder.proxy(Proxy::all(proxy)?);
    }
    let endpoint = format!("https://api.telegram.org/bot{token}/sendMessage");
    let response = builder
        .build()?
        .post(&endpoint)
        .json(&json!({
            "chat_id": chat_id,
            "text": text,
            "disable_web_page_preview": true,
        }))
        .send()
        .map_err(|error| {
            anyhow!(
                "Telegram sendMessage request failed: {}",
                redact_secret(&error.to_string(), token)
            )
        })?;
    let status = response.status();
    let body = response
        .text()
        .map_err(|error| anyhow!("Telegram sendMessage response read failed: {error}"))?;
    if !status.is_success() {
        return Err(anyhow!(
            "Telegram sendMessage returned HTTP {status}: {}",
            redact_secret(&body, token)
        ));
    }
    let response: TelegramApiResponse<serde_json::Value> =
        serde_json::from_str(&body).map_err(|error| {
            anyhow!(
                "Telegram sendMessage response JSON parse failed: {error}; body: {}",
                redact_secret(&body, token)
            )
        })?;
    if !response.ok {
        return Err(anyhow!(
            "Telegram sendMessage failed: {}",
            response
                .description
                .unwrap_or_else(|| "unknown error".to_string())
        ));
    }
    Ok(())
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
            "admin_user_ids: {}",
            format_cli_chat_ids(&settings.telegram_admin_user_ids)
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

fn cmd_classify(args: ClassifyArgs, settings: &Settings) -> Result<()> {
    let author = resolve_author(args.author.as_deref(), settings)?;
    let storage = Storage::open(&settings.db_path)?;
    let report = ClassificationService::new(&storage).classify_author(
        &author,
        ClassificationOptions {
            limit: args.limit,
            force: args.force,
            dry_run: args.dry_run,
        },
    )?;
    println!("{}", format_classification_report(&report));
    Ok(())
}

fn format_classification_report(report: &ClassificationReport) -> String {
    let mut lines = Vec::new();
    let mode = if report.dry_run {
        "chunk classification dry run:"
    } else {
        "chunk classification:"
    };
    lines.push(mode.to_string());
    lines.push(format!("chunks_scanned: {}", report.chunks_scanned));
    lines.push(format!("classified: {}", report.classified));
    lines.push(format!("changed: {}", report.changed));
    lines.push(format!("skipped_current: {}", report.skipped_current));
    lines.push("by kind:".to_string());
    if report.by_kind.is_empty() {
        lines.push("- <none>: 0".to_string());
    } else {
        for (kind, count) in &report.by_kind {
            lines.push(format!("- {kind}: {count}"));
        }
    }
    lines.push("skip reasons:".to_string());
    if report.skip_reasons.is_empty() {
        lines.push("- <none>: 0".to_string());
    } else {
        for (reason, count) in &report.skip_reasons {
            lines.push(format!("- {reason}: {count}"));
        }
    }
    lines.join("\n")
}

fn cmd_extract(args: ExtractArgs, settings: &Settings) -> Result<()> {
    if !args.v2 {
        return Err(anyhow!("extract currently requires --v2"));
    }
    let author = resolve_author(args.author.as_deref(), settings)?;
    let storage = Storage::open(&settings.db_path)?;
    let report = ExtractionService::new(&storage).extract_author_v2(
        &author,
        V2ExtractionOptions {
            limit: args.limit,
            force: args.force,
            dry_run: args.dry_run,
            failed_only: args.failed_only,
        },
    )?;
    println!("{}", format_v2_extraction_report(&report));
    Ok(())
}

fn cmd_comprehend(args: ComprehendArgs, settings: &Settings) -> Result<()> {
    if !args.v2 {
        return Err(anyhow!("comprehend currently requires --v2"));
    }
    let author = resolve_author(args.author.as_deref(), settings)?;
    let storage = Storage::open(&settings.db_path)?;
    let llm = if args.deterministic {
        None
    } else {
        make_optional_llm(settings)?
    };
    if args.author_profile {
        if args.profiled_only {
            return Err(anyhow!(
                "--profiled-only only applies to V2 paper comprehension"
            ));
        }
        let report = ComprehensionService::new(&storage).comprehend_author_profile_v2(
            &author,
            S4AuthorComprehensionOptions {
                limit: args.limit,
                force: args.force,
                dry_run: args.dry_run,
            },
            llm.as_ref(),
        )?;
        println!("{}", format_s4_author_comprehension_report(&report));
        return Ok(());
    }
    let report = ComprehensionService::new(&storage).comprehend_author_v2(
        &author,
        S3ComprehensionOptions {
            limit: args.limit,
            force: args.force,
            dry_run: args.dry_run,
            profiled_only: args.profiled_only,
        },
        llm.as_ref(),
    )?;
    println!("{}", format_s3_comprehension_report(&report));
    Ok(())
}

fn format_s4_author_comprehension_report(report: &S4AuthorComprehensionReport) -> String {
    let mut lines = Vec::new();
    let mode = if report.dry_run {
        "v2 author comprehension dry run:"
    } else {
        "v2 author comprehension:"
    };
    lines.push(mode.to_string());
    lines.push(format!("model_id: {}", report.model_id));
    lines.push(format!(
        "paper_profiles_scanned: {}",
        report.paper_profiles_scanned
    ));
    lines.push(format!("built: {}", report.built));
    lines.push(format!("changed: {}", report.changed));
    lines.push(format!("skipped_current: {}", report.skipped_current));
    lines.push(format!(
        "missing_paper_profiles: {}",
        report.missing_paper_profiles
    ));
    lines.push(format!("research_themes: {}", report.research_themes));
    lines.join("\n")
}

fn format_s3_comprehension_report(report: &S3ComprehensionReport) -> String {
    let mut lines = Vec::new();
    let mode = if report.dry_run {
        "v2 paper comprehension dry run:"
    } else {
        "v2 paper comprehension:"
    };
    lines.push(mode.to_string());
    lines.push(format!("model_id: {}", report.model_id));
    lines.push(format!("papers_scanned: {}", report.papers_scanned));
    lines.push(format!("built: {}", report.built));
    lines.push(format!("changed: {}", report.changed));
    lines.push(format!("skipped_current: {}", report.skipped_current));
    lines.push(format!(
        "missing_chunk_facts: {}",
        report.missing_chunk_facts
    ));
    lines.push(format!("failed: {}", report.failed));
    lines.push("by fact type:".to_string());
    if report.by_fact_type.is_empty() {
        lines.push("- <none>: 0".to_string());
    } else {
        for (fact_type, count) in &report.by_fact_type {
            lines.push(format!("- {fact_type}: {count}"));
        }
    }
    lines.join("\n")
}

fn format_v2_extraction_report(report: &V2ExtractionReport) -> String {
    let mut lines = Vec::new();
    let mode = if report.dry_run {
        "v2 chunk fact extraction dry run:"
    } else {
        "v2 chunk fact extraction:"
    };
    lines.push(mode.to_string());
    lines.push(format!("failed_only: {}", report.failed_only));
    lines.push(format!("chunks_scanned: {}", report.chunks_scanned));
    lines.push(format!("extracted: {}", report.extracted));
    lines.push(format!("changed: {}", report.changed));
    lines.push(format!("skipped_current: {}", report.skipped_current));
    lines.push(format!(
        "skipped_by_classification: {}",
        report.skipped_by_classification
    ));
    lines.push(format!(
        "missing_current_classification: {}",
        report.missing_current_classification
    ));
    lines.push(format!("failed: {}", report.failed));
    lines.push("by fact type:".to_string());
    if report.by_fact_type.is_empty() {
        lines.push("- <none>: 0".to_string());
    } else {
        for (fact_type, count) in &report.by_fact_type {
            lines.push(format!("- {fact_type}: {count}"));
        }
    }
    lines.join("\n")
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
        let vectors = match embed_batch_with_retries(
            &client,
            &input,
            &progress,
            args.max_attempts,
            thread::sleep,
        ) {
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

fn embed_batch_with_retries<C, F>(
    client: &C,
    input: &[String],
    progress: &ProgressBar,
    max_attempts: usize,
    mut sleep: F,
) -> Result<Vec<Vec<f32>>>
where
    C: EmbeddingProvider,
    F: FnMut(Duration),
{
    let max_attempts = max_attempts.max(1);
    let mut last_error = None;
    for attempt in 1..=max_attempts {
        match client.embed(input) {
            Ok(vectors) => return Ok(vectors),
            Err(err) => {
                progress.println(format!(
                    "embedding batch attempt {attempt}/{max_attempts} failed: {err}"
                ));
                last_error = Some(err);
                if attempt < max_attempts {
                    sleep(Duration::from_secs(2 * attempt as u64));
                }
            }
        }
    }
    Err(last_error.unwrap_or_else(|| anyhow!("embedding failed")))
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
    let qa = QaService::new_with_profile_preference(
        &storage,
        make_llm(settings)?,
        make_optional_embedding(settings)?,
        &author,
        ask_profile_version_preference(args.profile_version, settings)?,
    )?;
    println!("{}", qa.answer(&author, &question)?);
    Ok(())
}

fn ask_profile_version_preference(
    arg: Option<AskProfileVersionArg>,
    settings: &Settings,
) -> Result<QaProfileVersionPreference> {
    arg.map(Into::into)
        .map(Ok)
        .unwrap_or_else(|| QaProfileVersionPreference::parse(&settings.qa_profile_version))
}

fn cmd_eval(args: EvalArgs, settings: &Settings) -> Result<()> {
    if args.fail_on_hold && !args.compare_profile_versions {
        return Err(anyhow!(
            "--fail-on-hold requires --compare-profile-versions"
        ));
    }
    let storage = Storage::open(&settings.db_path)?;
    let eval = EvalService::new(&storage);
    if args.compare_profile_versions {
        let v1_report =
            eval.run_golden_with_profile_version(&args.fixture, args.top_k, QaProfileVersion::V1)?;
        let v2_report =
            eval.run_golden_with_profile_version(&args.fixture, args.top_k, QaProfileVersion::V2)?;
        let thresholds = crate::eval::EvalComparisonThresholds {
            max_retrieval_hit_drop: args.max_retrieval_hit_drop,
            max_citation_precision_drop: args.max_citation_precision_drop,
            max_answer_contains_required_drop: args.max_answer_contains_required_drop,
            min_candidate_retrieval_hit_at_k: args.min_candidate_retrieval_hit_at_k,
            min_candidate_citation_precision: args.min_candidate_citation_precision,
            min_candidate_answer_contains_required: args.min_candidate_answer_contains_required,
        };
        let comparison = crate::eval::compare_eval_reports(&v1_report, &v2_report, thresholds);
        let output = if args.baseline_markdown {
            format_eval_profile_comparison_markdown(
                settings,
                &args.fixture,
                args.top_k,
                &v1_report,
                &v2_report,
                &comparison,
            )
        } else {
            serde_json::to_string_pretty(&json!({
                "comparison": comparison,
                "v1": eval.report_json(&v1_report, args.trace)?,
                "v2": eval.report_json(&v2_report, args.trace)?,
            }))?
        };
        if let Some(path) = args.output {
            std::fs::write(&path, output)
                .map_err(|error| anyhow!("failed to write {}: {error}", path.display()))?;
            println!("eval comparison report written: {}", path.display());
        } else {
            println!("{output}");
        }
        if args.fail_on_hold {
            if let Some(error) = eval_comparison_gate_error(&comparison) {
                return Err(anyhow!(error));
            }
        }
        return Ok(());
    }
    let report = eval.run_golden_with_profile_version(
        &args.fixture,
        args.top_k,
        args.profile_version.into(),
    )?;
    let output = if args.baseline_markdown {
        let author_statuses = eval_author_statuses(&storage, &report)?;
        format_eval_baseline_markdown(
            settings,
            &args.fixture,
            args.top_k,
            &report,
            &author_statuses,
        )
    } else {
        serde_json::to_string_pretty(&eval.report_json(&report, args.trace)?)?
    };
    if let Some(path) = args.output {
        std::fs::write(&path, output)
            .map_err(|error| anyhow!("failed to write {}: {error}", path.display()))?;
        println!("eval report written: {}", path.display());
    } else {
        println!("{output}");
    }
    Ok(())
}

fn eval_comparison_gate_error(comparison: &crate::eval::EvalComparisonReport) -> Option<String> {
    if comparison.metric_gate_pass {
        return None;
    }
    if comparison.blockers.is_empty() {
        return Some(format!(
            "eval comparison gate failed: {}",
            comparison.default_switch_recommendation
        ));
    }
    Some(format!(
        "eval comparison gate failed: {}",
        comparison.blockers.join("; ")
    ))
}

fn eval_author_statuses(
    storage: &Storage,
    report: &crate::eval::EvalReport,
) -> Result<Vec<(String, crate::storage::LibraryStatus)>> {
    let mut authors = report
        .cases
        .iter()
        .map(|case| case.author.clone())
        .collect::<Vec<_>>();
    authors.sort();
    authors.dedup();
    let service = StatusService::new(storage);
    authors
        .into_iter()
        .map(|author| {
            let status = service.summary(Some(&author))?;
            Ok((author, status))
        })
        .collect()
}

fn format_eval_baseline_markdown(
    settings: &Settings,
    fixture: &Path,
    top_k: usize,
    report: &crate::eval::EvalReport,
    author_statuses: &[(String, crate::storage::LibraryStatus)],
) -> String {
    let generated_at = Utc::now().format("%Y-%m-%d %H:%M:%S UTC");
    let mut lines = vec![
        format!(
            "# check-paper 真实作者评测 baseline {}",
            Utc::now().format("%Y-%m-%d")
        ),
        String::new(),
        format!("- generated_at: {generated_at}"),
        format!("- fixture: {}", fixture.display()),
        format!("- top_k: {top_k}"),
        format!("- qa_profile_version: {}", report.qa_profile_version),
        format!("- db_path: {}", settings.db_path.display()),
        format!("- paper_root: {}", settings.paper_root.display()),
        String::new(),
        "## Library Status".to_string(),
        String::new(),
        "| author | papers | analyzed | stale | queued | running | retry_waiting | failed | qa_logs | avg_qa_latency_ms | total_qa_tokens | total_qa_cost_usd |".to_string(),
        "| --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: |".to_string(),
    ];
    if author_statuses.is_empty() {
        lines.push("| <none> | 0 | 0 | 0 | 0 | 0 | 0 | 0 | 0 | - | - | - |".to_string());
    } else {
        for (author, status) in author_statuses {
            lines.push(format!(
                "| {} | {} | {} | {} | {} | {} | {} | {} | {} | {} | {} | {} |",
                markdown_cell(author),
                status.papers,
                status.analyzed,
                status.stale_papers,
                status.queued_jobs,
                status.running_jobs,
                status.retry_waiting_jobs,
                status.failed_jobs,
                status.qa_logs,
                optional_f64(status.avg_qa_latency_ms, 0),
                optional_i64(status.total_qa_tokens),
                optional_f64(status.total_qa_cost_usd, 6)
            ));
        }
    }

    lines.extend([
        String::new(),
        "## Eval Summary".to_string(),
        String::new(),
        format!("- total_questions: {}", report.total),
        format!("- retrieval_hit_at_k: {:.3}", report.retrieval_hit_at_k),
        format!("- citation_precision: {:.3}", report.citation_precision),
        format!(
            "- answer_contains_required: {:.3}",
            report.answer_contains_required
        ),
        format!(
            "- insufficient_when_missing: {:.3}",
            report.insufficient_when_missing
        ),
        format!("- latency_ms: {}", report.latency_ms),
        String::new(),
        "## QA Mode Summary".to_string(),
        String::new(),
        "| qa_mode | total | retrieval_hit_at_k | citation_precision | answer_contains_required | route_reasons |".to_string(),
        "| --- | ---: | ---: | ---: | ---: | --- |".to_string(),
    ]);
    if report.qa_mode_summary.is_empty() {
        lines.push("| <none> | 0 | 0.000 | 0.000 | 0.000 | <none> |".to_string());
    } else {
        for (mode, summary) in &report.qa_mode_summary {
            lines.push(format!(
                "| {} | {} | {:.3} | {:.3} | {:.3} | {} |",
                markdown_cell(mode),
                summary.total,
                summary.retrieval_hit_at_k,
                summary.citation_precision,
                summary.answer_contains_required,
                markdown_cell(&format_count_map(&summary.route_reasons))
            ));
        }
    }

    lines.extend([
        String::new(),
        "## Route Metrics".to_string(),
        String::new(),
        "| route | hit_at_k | avg_candidates |".to_string(),
        "| --- | ---: | ---: |".to_string(),
    ]);
    if report.route_hit_at_k.is_empty() && report.route_candidate_count_avg.is_empty() {
        lines.push("| <none> | 0.000 | 0.000 |".to_string());
    } else {
        let mut routes = report
            .route_hit_at_k
            .keys()
            .chain(report.route_candidate_count_avg.keys())
            .cloned()
            .collect::<Vec<_>>();
        routes.sort();
        routes.dedup();
        for route in routes {
            lines.push(format!(
                "| {} | {:.3} | {:.1} |",
                markdown_cell(&route),
                report.route_hit_at_k.get(&route).copied().unwrap_or(0.0),
                report
                    .route_candidate_count_avg
                    .get(&route)
                    .copied()
                    .unwrap_or(0.0)
            ));
        }
    }

    lines.extend([String::new(), "## Review Queue".to_string(), String::new()]);
    let review_cases = report
        .cases
        .iter()
        .filter(|case| {
            !case.retrieval_hit
                || !case.answer_contains_required
                || !case.answer_evidence_valid
                || !case.forbidden_terms_found.is_empty()
        })
        .collect::<Vec<_>>();
    if review_cases.is_empty() {
        lines.push("- <none>".to_string());
    } else {
        for case in review_cases.iter().take(20) {
            lines.push(format!(
                "- [{}] {} | mode={} reason={} retrieval_hit={} evidence_valid={}",
                case.author,
                case.question,
                case.qa_mode,
                case.route_reason,
                case.retrieval_hit,
                case.answer_evidence_valid
            ));
            if !case.missing_required_terms.is_empty() {
                lines.push(format!(
                    "  - missing_required_terms: {}",
                    case.missing_required_terms.join(", ")
                ));
            }
            if !case.forbidden_terms_found.is_empty() {
                lines.push(format!(
                    "  - forbidden_terms_found: {}",
                    case.forbidden_terms_found.join(", ")
                ));
            }
            if !case.answer_validation_error.is_empty() {
                lines.push(format!(
                    "  - answer_validation_error: {}",
                    case.answer_validation_error
                ));
            }
        }
    }

    lines.extend([
        String::new(),
        "## Decision Notes".to_string(),
        String::new(),
        "- Baseline is a measurement artifact, not an approval to switch default QA behavior.".to_string(),
        "- Review low retrieval hit, low citation precision, and invalid evidence cases before comparing V1/V2 QA behavior.".to_string(),
    ]);
    lines.join("\n")
}

fn format_eval_profile_comparison_markdown(
    settings: &Settings,
    fixture: &Path,
    top_k: usize,
    v1_report: &crate::eval::EvalReport,
    v2_report: &crate::eval::EvalReport,
    comparison: &crate::eval::EvalComparisonReport,
) -> String {
    let generated_at = Utc::now().format("%Y-%m-%d %H:%M:%S UTC");
    let mut lines = vec![
        format!(
            "# check-paper V1 V2 Eval Comparison {}",
            Utc::now().format("%Y-%m-%d")
        ),
        String::new(),
        format!("- generated_at: {generated_at}"),
        format!("- fixture: {}", fixture.display()),
        format!("- top_k: {top_k}"),
        format!("- db_path: {}", settings.db_path.display()),
        format!("- paper_root: {}", settings.paper_root.display()),
        format!(
            "- baseline_profile_version: {}",
            comparison.baseline_profile_version
        ),
        format!(
            "- candidate_profile_version: {}",
            comparison.candidate_profile_version
        ),
        format!(
            "- default_switch_recommendation: {}",
            comparison.default_switch_recommendation
        ),
        format!(
            "- metric_gate_pass: {}",
            yes_no(comparison.metric_gate_pass)
        ),
        String::new(),
        "## Metric Gate".to_string(),
        String::new(),
        "| metric | V1 | V2 | delta | max_allowed_drop | min_required_v2 | status |".to_string(),
        "| --- | ---: | ---: | ---: | ---: | ---: | --- |".to_string(),
    ];
    for metric in &comparison.metrics {
        lines.push(format!(
            "| {} | {:.3} | {:.3} | {:.3} | {:.3} | {:.3} | {} |",
            metric.metric,
            metric.baseline,
            metric.candidate,
            metric.delta,
            metric.max_allowed_drop,
            metric.min_required_candidate,
            if metric.passed { "pass" } else { "fail" }
        ));
    }
    lines.extend([String::new(), "## Blockers".to_string(), String::new()]);
    if comparison.blockers.is_empty() {
        lines.push("- <none>".to_string());
    } else {
        for blocker in &comparison.blockers {
            lines.push(format!("- {blocker}"));
        }
    }
    lines.extend([
        String::new(),
        "## QA Mode Totals".to_string(),
        String::new(),
        "| profile_version | total_questions | qa_modes |".to_string(),
        "| --- | ---: | --- |".to_string(),
        format!(
            "| {} | {} | {} |",
            v1_report.qa_profile_version,
            v1_report.total,
            markdown_cell(&format_qa_mode_totals(&v1_report.qa_mode_summary))
        ),
        format!(
            "| {} | {} | {} |",
            v2_report.qa_profile_version,
            v2_report.total,
            markdown_cell(&format_qa_mode_totals(&v2_report.qa_mode_summary))
        ),
        String::new(),
        "## Decision Notes".to_string(),
        String::new(),
        "- Passing this metric gate does not switch default QA behavior by itself.".to_string(),
        "- Keep default V1 until profile gate is ready, profile diff review is signed off, and expanded fixtures do not regress.".to_string(),
    ]);
    lines.join("\n")
}

fn format_qa_mode_totals(map: &BTreeMap<String, crate::eval::EvalQaModeSummary>) -> String {
    if map.is_empty() {
        return "<none>".to_string();
    }
    map.iter()
        .map(|(mode, summary)| format!("{mode}={}", summary.total))
        .collect::<Vec<_>>()
        .join(", ")
}

fn optional_i64(value: Option<i64>) -> String {
    value
        .map(|value| value.to_string())
        .unwrap_or_else(|| "-".to_string())
}

fn optional_f64(value: Option<f64>, decimals: usize) -> String {
    value
        .map(|value| format!("{value:.decimals$}"))
        .unwrap_or_else(|| "-".to_string())
}

fn format_count_map(map: &BTreeMap<String, usize>) -> String {
    if map.is_empty() {
        return "<none>".to_string();
    }
    map.iter()
        .map(|(key, value)| format!("{key}={value}"))
        .collect::<Vec<_>>()
        .join(", ")
}

fn markdown_cell(value: &str) -> String {
    value.replace('|', "\\|").replace('\n', " ")
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
            if args.trend {
                let trend = storage.qa_log_trend(args.author.as_deref(), args.days)?;
                if trend.is_empty() {
                    println!("no qa logs");
                    return Ok(());
                }
                for row in trend {
                    println!(
                        "day={} total={} errors={} avg_latency_ms={} total_tokens={} cost_usd={} streaming={} streaming_finalized={} telegram={}",
                        row.day,
                        row.total,
                        row.errors,
                        row.avg_latency_ms
                            .map(|value| format!("{value:.0}"))
                            .unwrap_or_else(|| "-".to_string()),
                        row.total_tokens
                            .map(|value| value.to_string())
                            .unwrap_or_else(|| "-".to_string()),
                        row.total_cost_usd
                            .map(|value| format!("{value:.6}"))
                            .unwrap_or_else(|| "-".to_string()),
                        row.streaming,
                        row.streaming_finalized,
                        row.telegram,
                    );
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
                if let Some(profile_version) = log.qa_profile_version.as_deref() {
                    println!("  qa_profile_version={profile_version}");
                }
                if let Some(qa_mode) = log.qa_mode.as_deref() {
                    println!("  qa_mode={qa_mode}");
                }
                if let Some(route_reason) = log.route_reason.as_deref() {
                    println!("  route_reason={route_reason}");
                }
                if let Some(delivery_mode) = log.delivery_mode.as_deref() {
                    println!("  delivery_mode={delivery_mode}");
                }
                if let Some(streaming_finalized) = log.streaming_finalized {
                    println!("  streaming_finalized={streaming_finalized}");
                }
                if let Some(delta_count) = log.stream_delta_count {
                    println!("  stream_delta_count={delta_count}");
                }
                if let Some(chars) = log.streamed_chars {
                    println!("  streamed_chars={chars}");
                }
                if let Some(first_delta_ms) = log.stream_first_delta_ms {
                    println!("  stream_first_delta_ms={first_delta_ms}");
                }
                if let Some(duration_ms) = log.stream_duration_ms {
                    println!("  stream_duration_ms={duration_ms}");
                }
                if let (Some(chat_id), Some(job_id)) = (log.telegram_chat_id, log.telegram_job_id) {
                    println!("  telegram_chat_id={chat_id} telegram_job_id={job_id}");
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
        LogsCommand::Telegram(args) => {
            if args.summary {
                let summary = storage.telegram_delivery_summary(args.chat_id)?;
                if summary.is_empty() {
                    println!("no telegram delivery logs");
                    return Ok(());
                }
                for row in summary {
                    println!(
                        "final_delivery={} error_code={} total={} cancelled={} preview_edit_attempts={} preview_edit_successes={} preview_edit_failures={} reply_chars={}",
                        row.final_delivery,
                        row.error_code.as_deref().unwrap_or("-"),
                        row.total,
                        row.cancelled,
                        row.preview_edit_attempts,
                        row.preview_edit_successes,
                        row.preview_edit_failures,
                        row.reply_chars,
                    );
                }
                return Ok(());
            }
            if args.trend {
                let trend = storage.telegram_delivery_trend(args.chat_id, args.days)?;
                if trend.is_empty() {
                    println!("no telegram delivery logs");
                    return Ok(());
                }
                for row in trend {
                    println!(
                        "day={} total={} cancelled={} failed={} edited_placeholder={} sent_fallback={} matched_qa={} preview_edit_attempts={} preview_edit_failures={} reply_chars={}",
                        row.day,
                        row.total,
                        row.cancelled,
                        row.failed,
                        row.edited_placeholder,
                        row.sent_fallback,
                        row.matched_qa,
                        row.preview_edit_attempts,
                        row.preview_edit_failures,
                        row.reply_chars,
                    );
                }
                return Ok(());
            }
            if args.with_qa {
                let logs = storage.telegram_delivery_logs_with_qa(args.chat_id, args.last)?;
                if logs.is_empty() {
                    println!("no telegram delivery logs");
                    return Ok(());
                }
                for log in logs {
                    println!(
                        "#{} [{}] chat_id={} job_id={} final_delivery={} cancelled={} error_code={} qa_log_id={} qa_author={} qa_mode={} streaming_finalized={}",
                        log.id,
                        log.created_at,
                        log.chat_id,
                        log.job_id,
                        log.final_delivery,
                        log.cancelled,
                        log.error_code.as_deref().unwrap_or("-"),
                        log.qa_log_id
                            .map(|value| value.to_string())
                            .unwrap_or_else(|| "-".to_string()),
                        log.qa_author.as_deref().unwrap_or("-"),
                        log.qa_mode.as_deref().unwrap_or("-"),
                        log.streaming_finalized
                            .map(|value| value.to_string())
                            .unwrap_or_else(|| "-".to_string()),
                    );
                    if let Some(route_reason) = log.route_reason.as_deref() {
                        println!("  route_reason={route_reason}");
                    }
                    if let Some(qa_error_code) = log.qa_error_code.as_deref() {
                        println!("  qa_error_code={qa_error_code}");
                    }
                    if let Some(question) = log.qa_question.as_deref() {
                        println!("  qa_question={question}");
                    }
                }
                return Ok(());
            }
            let logs = storage.telegram_delivery_logs(args.chat_id, args.last)?;
            if logs.is_empty() {
                println!("no telegram delivery logs");
                return Ok(());
            }
            for log in logs {
                println!(
                    "#{} [{}] chat_id={} job_id={} final_delivery={} cancelled={} reply_chars={} preview_edit_attempts={} preview_edit_successes={} preview_edit_failures={} preview_last_chars={}",
                    log.id,
                    log.created_at,
                    log.chat_id,
                    log.job_id,
                    log.final_delivery,
                    log.cancelled,
                    log.reply_chars,
                    log.preview_edit_attempts,
                    log.preview_edit_successes,
                    log.preview_edit_failures,
                    log.preview_last_chars,
                );
                if let Some(error_code) = log.error_code.as_deref() {
                    println!("  error_code={error_code}");
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
    if let Some(command) = args.command {
        return match command {
            ProfileCommand::Diff(diff_args) => cmd_profile_diff(diff_args, settings),
            ProfileCommand::Signoff(signoff_args) => cmd_profile_signoff(signoff_args),
            ProfileCommand::Gate(gate_args) => cmd_profile_gate(gate_args, settings),
        };
    }
    let author = resolve_author(args.author.as_deref(), settings)?;
    let storage = Storage::open(&settings.db_path)?;
    if args.v2 {
        if args.rebuild {
            return Err(anyhow!(
                "use `ppc comprehend --author {} --v2 --author-profile` to build AuthorProfileV2",
                quote_cli_arg(&author)
            ));
        }
        match storage.author_profile_v2(&author)? {
            Some(record) => {
                println!("{}", serde_json::to_string_pretty(&record.profile_json)?);
            }
            None => {
                println!("no v2 author profile for {author}");
            }
        }
        return Ok(());
    }
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

fn cmd_profile_diff(args: ProfileDiffArgs, settings: &Settings) -> Result<()> {
    let author = resolve_author(args.author.as_deref(), settings)?;
    let storage = Storage::open(&settings.db_path)?;
    let report = ComprehensionService::new(&storage).profile_diff(&author)?;
    let output = if args.markdown {
        format_profile_diff_markdown(&author, &report)
    } else {
        format_profile_diff_report(&report)
    };
    if let Some(path) = args.output {
        std::fs::write(&path, output)
            .map_err(|error| anyhow!("failed to write {}: {error}", path.display()))?;
        println!("profile diff report written: {}", path.display());
    } else {
        println!("{output}");
    }
    Ok(())
}

fn cmd_profile_signoff(args: ProfileSignoffArgs) -> Result<()> {
    let input = std::fs::read_to_string(&args.input)
        .map_err(|error| anyhow!("failed to read {}: {error}", args.input.display()))?;
    let report = check_profile_diff_signoff(&input);
    println!(
        "{}",
        format_profile_diff_signoff_report(&args.input, &report)
    );
    if args.fail_on_hold {
        if let Some(error) = profile_diff_signoff_gate_error(&report) {
            return Err(anyhow!(error));
        }
    }
    Ok(())
}

fn cmd_profile_gate(args: ProfileGateArgs, settings: &Settings) -> Result<()> {
    let author = resolve_author(args.author.as_deref(), settings)?;
    let storage = Storage::open(&settings.db_path)?;
    let report = ComprehensionService::new(&storage).profile_gate(&author)?;
    println!("{}", format_profile_gate_report(&report));
    Ok(())
}

fn format_profile_diff_report(report: &ProfileDiffReport) -> String {
    let mut lines = Vec::new();
    lines.push("profile diff:".to_string());
    lines.push(format!("papers_with_v1: {}", report.papers_with_v1));
    lines.push(format!("papers_with_v2: {}", report.papers_with_v2));
    lines.push(format!("missing_v2: {}", report.missing_v2.len()));
    for paper_key in report.missing_v2.iter().take(10) {
        lines.push(format!("- missing_v2: {paper_key}"));
    }
    lines.push(format!("missing_v1: {}", report.missing_v1.len()));
    for paper_key in report.missing_v1.iter().take(10) {
        lines.push(format!("- missing_v1: {paper_key}"));
    }
    lines.push(format!(
        "changed_summaries: {}",
        report.changed_summaries.len()
    ));
    for diff in report.changed_summaries.iter().take(10) {
        lines.push(format!("- {}", diff.paper_key));
        lines.push(format!("  title: {}", diff.title));
        lines.push(format!("  v1: {}", diff.v1_summary));
        lines.push(format!("  v2: {}", diff.v2_summary));
    }
    lines.join("\n")
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
struct ProfileDiffSignoffReport {
    ready: bool,
    changed_summaries_declared: Option<usize>,
    review_status_total: usize,
    accepted: usize,
    accepted_with_note: usize,
    pending: usize,
    rejected: usize,
    unresolved_impacts: usize,
    other_review_statuses: BTreeMap<String, usize>,
    human_status: Option<String>,
    reviewer: Option<String>,
    signed_off_at: Option<String>,
    blockers: Vec<String>,
    warnings: Vec<String>,
}

fn check_profile_diff_signoff(markdown: &str) -> ProfileDiffSignoffReport {
    let mut report = ProfileDiffSignoffReport::default();
    let mut section = String::new();
    let mut saw_human_signoff = false;

    for line in markdown.lines() {
        let trimmed = line.trim();
        if let Some(heading) = trimmed.strip_prefix("## ") {
            section = heading.trim().to_string();
            if section == "Human Signoff" {
                saw_human_signoff = true;
            }
            continue;
        }
        let Some((key, value)) = markdown_bullet_field(trimmed) else {
            continue;
        };
        match key {
            "changed_summaries" => {
                if let Ok(count) = value.parse::<usize>() {
                    report.changed_summaries_declared = Some(count);
                }
            }
            "review_status" => {
                report.review_status_total += 1;
                match value {
                    "accepted" => report.accepted += 1,
                    "accepted_with_note" => report.accepted_with_note += 1,
                    "pending" => report.pending += 1,
                    "rejected" => report.rejected += 1,
                    other => {
                        *report
                            .other_review_statuses
                            .entry(other.to_string())
                            .or_insert(0) += 1;
                    }
                }
            }
            "default_switch_impact" => {
                if value == "unresolved" {
                    report.unresolved_impacts += 1;
                } else if value == "high" {
                    report.warnings.push(
                        "one or more summaries are marked high default_switch_impact".to_string(),
                    );
                }
            }
            "status" if section == "Human Signoff" => {
                report.human_status = Some(value.to_string());
            }
            "reviewer" if section == "Human Signoff" => {
                report.reviewer = Some(value.to_string());
            }
            "signed_off_at" if section == "Human Signoff" => {
                report.signed_off_at = Some(value.to_string());
            }
            _ => {}
        }
    }

    if !saw_human_signoff {
        report
            .blockers
            .push("missing ## Human Signoff section".to_string());
    }
    match report.human_status.as_deref() {
        Some("signed_off" | "signed_off_with_note") => {}
        Some(status) => report.blockers.push(format!(
            "human signoff status is {status}, expected signed_off"
        )),
        None => report
            .blockers
            .push("missing human signoff status".to_string()),
    }
    if !is_filled_signoff_field(report.reviewer.as_deref()) {
        report
            .blockers
            .push("missing human signoff reviewer".to_string());
    }
    if !is_filled_signoff_field(report.signed_off_at.as_deref()) {
        report
            .blockers
            .push("missing human signed_off_at".to_string());
    }

    let changed_summaries = report
        .changed_summaries_declared
        .unwrap_or(report.review_status_total);
    if changed_summaries > 0 && report.review_status_total == 0 {
        report
            .blockers
            .push("changed summaries have no review_status entries".to_string());
    }
    if report.review_status_total < changed_summaries {
        report.blockers.push(format!(
            "only {} review_status entries for {changed_summaries} changed summaries",
            report.review_status_total
        ));
    }
    if report.pending > 0 {
        report.blockers.push(format!(
            "{} changed summaries are still pending",
            report.pending
        ));
    }
    if report.rejected > 0 {
        report.blockers.push(format!(
            "{} changed summaries are rejected",
            report.rejected
        ));
    }
    if report.unresolved_impacts > 0 {
        report.blockers.push(format!(
            "{} changed summaries still have unresolved default_switch_impact",
            report.unresolved_impacts
        ));
    }
    for (status, count) in &report.other_review_statuses {
        report.blockers.push(format!(
            "{count} changed summaries use unsupported review_status {status}"
        ));
    }

    report.ready = report.blockers.is_empty();
    report
}

fn markdown_bullet_field(line: &str) -> Option<(&str, &str)> {
    let line = line.strip_prefix("- ")?;
    let (key, value) = line.split_once(':')?;
    Some((key.trim(), value.trim()))
}

fn is_filled_signoff_field(value: Option<&str>) -> bool {
    let Some(value) = value.map(str::trim) else {
        return false;
    };
    !value.is_empty() && value != "<pending>" && value != "pending"
}

fn profile_diff_signoff_gate_error(report: &ProfileDiffSignoffReport) -> Option<String> {
    if report.ready {
        return None;
    }
    if report.blockers.is_empty() {
        return Some("profile diff signoff gate failed: hold".to_string());
    }
    Some(format!(
        "profile diff signoff gate failed: {}",
        report.blockers.join("; ")
    ))
}

fn format_profile_diff_signoff_report(input: &Path, report: &ProfileDiffSignoffReport) -> String {
    let mut lines = vec![
        format!(
            "profile diff signoff: {}",
            if report.ready { "ready" } else { "hold" }
        ),
        format!("input: {}", input.display()),
        format!(
            "changed_summaries: {}",
            report
                .changed_summaries_declared
                .map(|count| count.to_string())
                .unwrap_or_else(|| "<unknown>".to_string())
        ),
        format!(
            "review_statuses: accepted={} accepted_with_note={} pending={} rejected={} other={}",
            report.accepted,
            report.accepted_with_note,
            report.pending,
            report.rejected,
            report.other_review_statuses.values().sum::<usize>()
        ),
        format!(
            "human_status: {}",
            report.human_status.as_deref().unwrap_or("<missing>")
        ),
        format!(
            "reviewer: {}",
            report.reviewer.as_deref().unwrap_or("<missing>")
        ),
        format!(
            "signed_off_at: {}",
            report.signed_off_at.as_deref().unwrap_or("<missing>")
        ),
    ];
    if !report.blockers.is_empty() {
        lines.push("blockers:".to_string());
        for blocker in &report.blockers {
            lines.push(format!("- {blocker}"));
        }
    }
    if !report.warnings.is_empty() {
        lines.push("warnings:".to_string());
        for warning in &report.warnings {
            lines.push(format!("- {warning}"));
        }
    }
    lines.push("next_steps:".to_string());
    if report.ready {
        lines.push(
            "- keep this report with the profile diff review, eval gate, and trend records"
                .to_string(),
        );
        lines.push(
            "- continue holding default V1 until sustained A/B evidence is present".to_string(),
        );
    } else {
        lines.push(
            "- fill ## Human Signoff with status signed_off, reviewer, and signed_off_at"
                .to_string(),
        );
        lines.push(
            "- resolve pending/rejected summary review entries before default promotion"
                .to_string(),
        );
        lines.push("- rerun ppc profile signoff --input <review.md> --fail-on-hold".to_string());
    }
    lines.join("\n")
}

fn format_profile_diff_markdown(author: &str, report: &ProfileDiffReport) -> String {
    let generated_at = Utc::now().format("%Y-%m-%d %H:%M:%S UTC");
    let mut lines = vec![
        format!("# check-paper V1 V2 Profile Diff Review {author}"),
        String::new(),
        format!("- generated_at: {generated_at}"),
        format!("- author: {author}"),
        format!("- papers_with_v1: {}", report.papers_with_v1),
        format!("- papers_with_v2: {}", report.papers_with_v2),
        format!("- missing_v2: {}", report.missing_v2.len()),
        format!("- missing_v1: {}", report.missing_v1.len()),
        format!("- changed_summaries: {}", report.changed_summaries.len()),
        String::new(),
        "## Review Decision".to_string(),
        String::new(),
        "- status: needs_human_review".to_string(),
        "- default_switch: hold".to_string(),
        "- reason: profile diff is a quality gate; review semantic changes before promoting V2 to default.".to_string(),
        String::new(),
        "## Human Signoff".to_string(),
        String::new(),
        "- status: pending".to_string(),
        "- reviewer: <pending>".to_string(),
        "- signed_off_at: <pending>".to_string(),
        "- signoff_note: <pending>".to_string(),
        String::new(),
        "## Missing Coverage".to_string(),
        String::new(),
    ];
    if report.missing_v2.is_empty() && report.missing_v1.is_empty() {
        lines.push("- <none>".to_string());
    } else {
        for paper_key in &report.missing_v2 {
            lines.push(format!("- missing_v2: {paper_key}"));
        }
        for paper_key in &report.missing_v1 {
            lines.push(format!("- missing_v1: {paper_key}"));
        }
    }
    lines.extend([
        String::new(),
        "## Changed Summaries".to_string(),
        String::new(),
    ]);
    if report.changed_summaries.is_empty() {
        lines.push("- <none>".to_string());
    } else {
        for diff in &report.changed_summaries {
            lines.push(format!("### {}", diff.paper_key));
            lines.push(String::new());
            lines.push(format!("- title: {}", diff.title));
            lines.push("- review_status: pending".to_string());
            lines.push("- default_switch_impact: unresolved".to_string());
            lines.push(String::new());
            lines.push("V1 summary:".to_string());
            lines.push(String::new());
            lines.push(format!("> {}", diff.v1_summary.replace('\n', " ")));
            lines.push(String::new());
            lines.push("V2 summary:".to_string());
            lines.push(String::new());
            lines.push(format!("> {}", diff.v2_summary.replace('\n', " ")));
            lines.push(String::new());
        }
    }
    lines.extend([
        "## Next Actions".to_string(),
        String::new(),
        "- Review each changed summary against source chunks or paper text.".to_string(),
        "- Mark review_status as accepted, accepted_with_note, or rejected.".to_string(),
        "- Fill ## Human Signoff after a human reviewer accepts the changed summaries.".to_string(),
        "- Validate signoff with `ppc profile signoff --input path/to/profile-diff-review.md --fail-on-hold`.".to_string(),
        "- Keep default V1 until changed summaries and expanded A/B fixtures are reviewed."
            .to_string(),
    ]);
    lines.join("\n")
}

fn format_profile_gate_report(report: &ProfileGateReport) -> String {
    let mut lines = Vec::new();
    lines.push(format!(
        "profile gate: {}",
        if report.ready { "ready" } else { "blocked" }
    ));
    lines.push(format!("papers_with_v1: {}", report.papers_with_v1));
    lines.push(format!("papers_with_v2: {}", report.papers_with_v2));
    lines.push(format!("missing_v2: {}", report.missing_v2.len()));
    for paper_key in report.missing_v2.iter().take(10) {
        lines.push(format!("- missing_v2: {paper_key}"));
    }
    lines.push(format!("missing_v1: {}", report.missing_v1.len()));
    for paper_key in report.missing_v1.iter().take(10) {
        lines.push(format!("- missing_v1: {paper_key}"));
    }
    lines.push(format!(
        "invalid_v2_profiles: {}",
        report.invalid_v2_profiles.len()
    ));
    for issue in report.invalid_v2_profiles.iter().take(10) {
        lines.push(format!(
            "- invalid_v2: {} [{}]",
            issue.paper_key, issue.error
        ));
    }
    lines.push(format!(
        "author_profile_v2: {}",
        if report.author_profile_v2_valid {
            "valid"
        } else if report.author_profile_v2_present {
            "invalid"
        } else {
            "missing"
        }
    ));
    if let Some(error) = &report.author_profile_v2_error {
        lines.push(format!("author_profile_v2_error: {error}"));
    }
    lines.push(format!("factual_objects: {}", report.factual_objects));
    lines.push(format!(
        "claims_with_support_refs: {}",
        report.claims_with_support_refs
    ));
    lines.push(format!("support_refs: {}", report.support_refs));
    lines.push("blockers:".to_string());
    if report.blockers.is_empty() {
        lines.push("- <none>".to_string());
    } else {
        for blocker in &report.blockers {
            lines.push(format!("- {blocker}"));
        }
    }
    lines.push("warnings:".to_string());
    if report.warnings.is_empty() {
        lines.push("- <none>".to_string());
    } else {
        for warning in &report.warnings {
            lines.push(format!("- {warning}"));
        }
    }
    lines.join("\n")
}

fn cmd_backup(args: BackupArgs, settings: &Settings) -> Result<()> {
    let backup_path = backup_database(&settings.db_path, args.output.as_deref())?;
    println!("backup written: {}", backup_path.display());
    Ok(())
}

fn cmd_preflight(args: PreflightArgs, settings: &Settings) -> Result<()> {
    let author = resolve_author(args.author.as_deref(), settings)?;
    let storage = Storage::open(&settings.db_path)?;
    let status = StatusService::new(&storage).summary(Some(&author))?;
    println!(
        "{}",
        format_preflight_report(settings, &author, args.limit, &status)
    );
    Ok(())
}

fn format_preflight_report(
    settings: &Settings,
    author: &str,
    limit: Option<usize>,
    status: &crate::storage::LibraryStatus,
) -> String {
    let planned_papers = limit
        .map(|limit| limit.min(status.stale_papers.max(0) as usize))
        .unwrap_or(status.stale_papers.max(0) as usize);
    let limit_arg = limit
        .map(|limit| format!(" --limit {limit}"))
        .unwrap_or_default();
    let author_arg = quote_cli_arg(author);
    [
        "production preflight:".to_string(),
        format!("db_path: {}", settings.db_path.display()),
        format!(
            "backup_path: {}",
            default_backup_path(&settings.db_path).display()
        ),
        format!("paper_root: {}", settings.paper_root.display()),
        format!("author: {author}"),
        format!(
            "llm_configured: {}",
            yes_no(settings.llm_api_key.is_some() && !settings.llm_model.trim().is_empty())
        ),
        format!(
            "embedding_configured: {}",
            yes_no(
                settings.embedding_provider == "openai-compatible"
                    && settings.embedding_api_key.is_some()
                    && !settings.embedding_model.trim().is_empty()
            )
        ),
        format!(
            "telegram_configured: {}",
            yes_no(settings.telegram_bot_token.is_some())
        ),
        format!(
            "telegram_allowed_chats: {}",
            settings.telegram_chat_ids.len()
        ),
        format!(
            "telegram_admin_users: {}",
            settings.telegram_admin_user_ids.len()
        ),
        format!("papers: {}", status.papers),
        format!("analyzed: {}", status.analyzed),
        format!("stale_papers: {}", status.stale_papers),
        format!("queued_jobs: {}", status.queued_jobs),
        format!("running_jobs: {}", status.running_jobs),
        format!("retry_waiting_jobs: {}", status.retry_waiting_jobs),
        format!("failed_jobs: {}", status.failed_jobs),
        format!("planned_sync_papers: {planned_papers}"),
        "next commands:".to_string(),
        "ppc backup".to_string(),
        format!("ppc sync --author {author_arg}{limit_arg}"),
        format!("ppc jobs --author {author_arg} --status failed"),
        format!("ppc analyze --author {author_arg} --failed-only"),
    ]
    .join("\n")
}

fn yes_no(value: bool) -> &'static str {
    if value { "yes" } else { "no" }
}

fn backup_database(db_path: &Path, output: Option<&Path>) -> Result<PathBuf> {
    if !db_path.exists() {
        return Err(anyhow!(
            "database not found: {}; run `ppc ingest` or `ppc sync` first",
            db_path.display()
        ));
    }
    let backup_path = output
        .map(Path::to_path_buf)
        .unwrap_or_else(|| default_backup_path(db_path));
    if backup_path.exists() {
        return Err(anyhow!(
            "backup target already exists: {}",
            backup_path.display()
        ));
    }
    if let Some(parent) = backup_path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let conn = rusqlite::Connection::open(db_path)?;
    conn.execute(
        "VACUUM main INTO ?",
        rusqlite::params![backup_path.to_string_lossy().as_ref()],
    )?;
    Ok(backup_path)
}

fn default_backup_path(db_path: &Path) -> PathBuf {
    let timestamp = Utc::now().format("%Y%m%d-%H%M%S");
    let stem = db_path
        .file_stem()
        .and_then(|value| value.to_str())
        .unwrap_or("check_paper");
    let file_name = format!("{stem}-backup-{timestamp}.sqlite");
    db_path
        .parent()
        .map(|parent| parent.join(&file_name))
        .unwrap_or_else(|| PathBuf::from(file_name))
}

fn cmd_serve_telegram(settings: &Settings) -> Result<()> {
    require_llm(settings)?;
    let token = settings
        .telegram_bot_token
        .clone()
        .ok_or_else(|| anyhow!("missing TELEGRAM_BOT_TOKEN; run `ppc tg config`"))?;
    let handlers = BotHandlers::new_with_runtime_settings(
        settings.db_path.clone(),
        make_llm(settings)?,
        make_optional_embedding(settings)?,
        settings.default_author.clone(),
        BotRuntimeSettings {
            paper_root: Some(settings.paper_root.clone()),
            chunker_version: settings.chunker_version.clone(),
            chunk_max_chars: settings.chunk_max_chars,
            chunk_overlap: settings.chunk_overlap,
            qa_profile_version: QaProfileVersionPreference::parse(&settings.qa_profile_version)?,
        },
    );
    TelegramBot::new(
        token,
        settings.telegram_chat_ids.clone(),
        settings.telegram_admin_user_ids.clone(),
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

fn make_optional_llm(settings: &Settings) -> Result<Option<OpenAiCompatibleClient>> {
    if settings.llm_api_key.is_some() && !settings.llm_model.trim().is_empty() {
        return make_llm(settings).map(Some);
    }
    Ok(None)
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
    use std::cell::Cell;
    use std::collections::BTreeMap;
    use std::fs;
    use std::path::PathBuf;
    use std::time::Duration;

    use super::{
        AnalysisFailure, AskArgs, AskProfileVersionArg, ComprehendArgs, PaperRootAuthorSummary,
        TelegramBotStatus, TgServiceTemplateKind, analysis_failure_summary_lines,
        ask_profile_version_preference, backup_database, check_profile_diff_signoff,
        check_tg_service_template, classify_analysis_error, cmd_ask, cmd_comprehend,
        default_tg_service_log_path, embed_batch_with_retries, eval_comparison_gate_error,
        format_author_inventory, format_classification_report, format_cli_chat_ids,
        format_eval_baseline_markdown, format_eval_profile_comparison_markdown,
        format_preflight_report, format_profile_diff_markdown, format_profile_diff_report,
        format_profile_diff_signoff_report, format_profile_gate_report,
        format_s3_comprehension_report, format_s4_author_comprehension_report, format_tg_health,
        format_tg_health_alert, format_tg_service_check_report, format_tg_service_install_report,
        format_tg_service_template, format_tg_status, format_v2_extraction_report,
        install_tg_service_template, missing_author_message, paper_root_authors,
        profile_diff_signoff_gate_error, redact_secret, resolve_author,
        should_rebuild_author_profile, tg_health_notify_chat_ids, tg_health_status,
    };
    use crate::config::Settings;
    use crate::eval::{EvalCaseReport, EvalComparisonThresholds, EvalQaModeSummary, EvalReport};
    use crate::papers::models::Paper;
    use crate::retrieval::embedding::EmbeddingProvider;
    use crate::services::classification::ClassificationReport;
    use crate::services::comprehension::{
        ProfileDiffReport, ProfileGateIssue, ProfileGateReport, ProfileSummaryDiff,
        S3ComprehensionReport, S4AuthorComprehensionReport,
    };
    use crate::services::extraction::V2ExtractionReport;
    use crate::storage::RuntimeHeartbeat;
    use crate::storage::Storage;
    use crate::storage::{AuthorSummary, LibraryStatus};
    use indicatif::ProgressBar;
    use serde_json::json;
    use tempfile::tempdir;

    fn settings(default_author: Option<&str>) -> Settings {
        Settings {
            paper_root: PathBuf::from("paper"),
            db_path: PathBuf::from("data/test.sqlite"),
            default_author: default_author.map(str::to_string),
            qa_profile_version: "v1".to_string(),
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
            telegram_admin_user_ids: Vec::new(),
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
                profile_version: Some(AskProfileVersionArg::V1),
                question: Vec::new(),
            },
            &settings(None),
        )
        .unwrap_err()
        .to_string();

        assert!(err.contains("missing question"));
    }

    #[test]
    fn ask_profile_version_uses_config_default_and_allows_override() {
        let mut settings = settings(None);
        settings.qa_profile_version = "auto".to_string();

        assert_eq!(
            ask_profile_version_preference(None, &settings).unwrap(),
            crate::services::qa::QaProfileVersionPreference::Auto
        );
        assert_eq!(
            ask_profile_version_preference(Some(AskProfileVersionArg::V2), &settings).unwrap(),
            crate::services::qa::QaProfileVersionPreference::V2
        );

        settings.qa_profile_version = "bad".to_string();
        assert!(ask_profile_version_preference(None, &settings).is_err());
    }

    #[test]
    fn comprehend_v2_can_run_without_llm_config() {
        let dir = tempdir().unwrap();
        let mut settings = settings(Some("Alice"));
        settings.db_path = dir.path().join("test.sqlite");

        cmd_comprehend(
            ComprehendArgs {
                author: None,
                v2: true,
                author_profile: false,
                limit: None,
                force: false,
                profiled_only: false,
                deterministic: false,
                dry_run: true,
            },
            &settings,
        )
        .unwrap();
    }

    #[test]
    fn embedding_batch_retry_succeeds_after_transient_failure() {
        struct FlakyEmbedding {
            calls: Cell<usize>,
        }

        impl EmbeddingProvider for FlakyEmbedding {
            fn model_name(&self) -> &str {
                "embed-model"
            }

            fn model_version(&self) -> Option<&str> {
                Some("v1")
            }

            fn batch_size(&self) -> usize {
                1
            }

            fn embed(&self, _input: &[String]) -> anyhow::Result<Vec<Vec<f32>>> {
                let calls = self.calls.get() + 1;
                self.calls.set(calls);
                if calls == 1 {
                    return Err(anyhow::anyhow!("temporary failure"));
                }
                Ok(vec![vec![0.1, 0.2]])
            }
        }

        let client = FlakyEmbedding {
            calls: Cell::new(0),
        };
        let mut sleeps = Vec::new();
        let vectors = embed_batch_with_retries(
            &client,
            &["paper text".to_string()],
            &ProgressBar::hidden(),
            3,
            |duration| sleeps.push(duration),
        )
        .unwrap();

        assert_eq!(vectors, vec![vec![0.1, 0.2]]);
        assert_eq!(client.calls.get(), 2);
        assert_eq!(sleeps, vec![Duration::from_secs(2)]);
    }

    #[test]
    fn backup_database_writes_sqlite_copy() {
        let dir = tempdir().unwrap();
        let db_path = dir.path().join("source.sqlite");
        {
            let conn = rusqlite::Connection::open(&db_path).unwrap();
            conn.execute("CREATE TABLE items (name TEXT NOT NULL)", [])
                .unwrap();
            conn.execute("INSERT INTO items (name) VALUES ('paper-a')", [])
                .unwrap();
        }
        let backup_path = dir.path().join("backups").join("source.backup.sqlite");

        let written = backup_database(&db_path, Some(&backup_path)).unwrap();

        assert_eq!(written, backup_path);
        let conn = rusqlite::Connection::open(&written).unwrap();
        let value: String = conn
            .query_row("SELECT name FROM items", [], |row| row.get(0))
            .unwrap();
        assert_eq!(value, "paper-a");
    }

    #[test]
    fn preflight_report_summarizes_backup_and_next_commands() {
        let mut settings = settings(Some("Alice"));
        settings.db_path = PathBuf::from("data/check_paper.sqlite");
        settings.llm_api_key = Some("secret".to_string());
        settings.llm_model = "model-a".to_string();
        settings.telegram_bot_token = Some("token".to_string());
        settings.telegram_chat_ids = vec![7, 8];
        let report = format_preflight_report(
            &settings,
            "Alice",
            Some(5),
            &LibraryStatus {
                papers: 10,
                analyzed: 4,
                stale_papers: 6,
                failed_jobs: 1,
                queued_jobs: 2,
                running_jobs: 0,
                retry_waiting_jobs: 1,
                cancelled_jobs: 0,
                qa_logs: 3,
                avg_qa_latency_ms: None,
                total_qa_tokens: None,
                total_qa_cost_usd: None,
            },
        );

        assert!(report.contains("backup_path: data/check_paper-backup-"));
        assert!(report.contains("llm_configured: yes"));
        assert!(report.contains("telegram_allowed_chats: 2"));
        assert!(report.contains("planned_sync_papers: 5"));
        assert!(report.contains("ppc backup"));
        assert!(report.contains("ppc sync --author \"Alice\" --limit 5"));
    }

    #[test]
    fn formats_classification_report_with_kind_and_skip_counts() {
        let report = ClassificationReport {
            chunks_scanned: 4,
            classified: 3,
            changed: 2,
            skipped_current: 1,
            by_kind: BTreeMap::from([("methods".to_string(), 1), ("results".to_string(), 2)]),
            skip_reasons: BTreeMap::from([("reference_section".to_string(), 1)]),
            dry_run: true,
        };

        let text = format_classification_report(&report);

        assert!(text.contains("chunk classification dry run"));
        assert!(text.contains("chunks_scanned: 4"));
        assert!(text.contains("classified: 3"));
        assert!(text.contains("- methods: 1"));
        assert!(text.contains("- reference_section: 1"));
    }

    #[test]
    fn formats_v2_extraction_report_with_fact_counts() {
        let report = V2ExtractionReport {
            chunks_scanned: 5,
            extracted: 3,
            changed: 2,
            skipped_current: 1,
            skipped_by_classification: 1,
            missing_current_classification: 0,
            failed: 0,
            by_fact_type: BTreeMap::from([("method".to_string(), 1), ("result".to_string(), 2)]),
            dry_run: true,
            failed_only: true,
        };

        let text = format_v2_extraction_report(&report);

        assert!(text.contains("v2 chunk fact extraction dry run"));
        assert!(text.contains("failed_only: true"));
        assert!(text.contains("chunks_scanned: 5"));
        assert!(text.contains("extracted: 3"));
        assert!(text.contains("skipped_by_classification: 1"));
        assert!(text.contains("- result: 2"));
    }

    #[test]
    fn formats_s3_comprehension_report_with_fact_counts() {
        let report = S3ComprehensionReport {
            papers_scanned: 3,
            built: 2,
            changed: 1,
            skipped_current: 1,
            missing_chunk_facts: 0,
            failed: 0,
            by_fact_type: BTreeMap::from([("result".to_string(), 2)]),
            dry_run: true,
            model_id: "test-model".to_string(),
        };

        let text = format_s3_comprehension_report(&report);

        assert!(text.contains("v2 paper comprehension dry run"));
        assert!(text.contains("model_id: test-model"));
        assert!(text.contains("papers_scanned: 3"));
        assert!(text.contains("built: 2"));
        assert!(text.contains("- result: 2"));
    }

    #[test]
    fn formats_s4_author_comprehension_report() {
        let report = S4AuthorComprehensionReport {
            paper_profiles_scanned: 5,
            built: 1,
            changed: 1,
            skipped_current: 0,
            missing_paper_profiles: 0,
            research_themes: 3,
            dry_run: true,
            model_id: "test-model".to_string(),
        };

        let text = format_s4_author_comprehension_report(&report);

        assert!(text.contains("v2 author comprehension dry run"));
        assert!(text.contains("paper_profiles_scanned: 5"));
        assert!(text.contains("research_themes: 3"));
    }

    #[test]
    fn formats_profile_diff_report() {
        let report = ProfileDiffReport {
            papers_with_v1: 2,
            papers_with_v2: 1,
            missing_v2: vec!["Alice/paper-b".to_string()],
            missing_v1: vec![],
            changed_summaries: vec![ProfileSummaryDiff {
                paper_key: "Alice/paper-a".to_string(),
                title: "A Paper".to_string(),
                v1_summary: "old".to_string(),
                v2_summary: "new".to_string(),
            }],
        };

        let text = format_profile_diff_report(&report);

        assert!(text.contains("papers_with_v1: 2"));
        assert!(text.contains("- missing_v2: Alice/paper-b"));
        assert!(text.contains("changed_summaries: 1"));
        assert!(text.contains("v2: new"));
    }

    #[test]
    fn formats_profile_diff_markdown_for_review() {
        let report = ProfileDiffReport {
            papers_with_v1: 2,
            papers_with_v2: 1,
            missing_v2: vec!["Alice/paper-b".to_string()],
            missing_v1: vec!["Alice/paper-c".to_string()],
            changed_summaries: vec![ProfileSummaryDiff {
                paper_key: "Alice/paper-a".to_string(),
                title: "A Paper".to_string(),
                v1_summary: "old summary".to_string(),
                v2_summary: "new summary".to_string(),
            }],
        };

        let text = format_profile_diff_markdown("Alice", &report);

        assert!(text.contains("# check-paper V1 V2 Profile Diff Review Alice"));
        assert!(text.contains("- default_switch: hold"));
        assert!(text.contains("- missing_v2: Alice/paper-b"));
        assert!(text.contains("- missing_v1: Alice/paper-c"));
        assert!(text.contains("### Alice/paper-a"));
        assert!(text.contains("- review_status: pending"));
        assert!(text.contains("## Human Signoff"));
        assert!(text.contains("- status: pending"));
        assert!(text.contains("ppc profile signoff --input"));
        assert!(text.contains("> old summary"));
        assert!(text.contains("> new summary"));
    }

    #[test]
    fn checks_profile_diff_signoff_blocks_until_human_fields_are_filled() {
        let markdown = r#"# check-paper V1 V2 Profile Diff Review Alice

- changed_summaries: 2

## Human Signoff

- status: pending
- reviewer: <pending>
- signed_off_at: <pending>

## Changed Summaries

### Alice/paper-a

- review_status: accepted_with_note
- default_switch_impact: low

### Alice/paper-b

- review_status: pending
- default_switch_impact: unresolved
"#;

        let report = check_profile_diff_signoff(markdown);
        let text = format_profile_diff_signoff_report(&PathBuf::from("review.md"), &report);
        let error = profile_diff_signoff_gate_error(&report).unwrap();

        assert!(!report.ready);
        assert!(text.contains("profile diff signoff: hold"));
        assert!(text.contains("human_status: pending"));
        assert!(text.contains("accepted_with_note=1"));
        assert!(error.contains("human signoff status is pending"));
        assert!(error.contains("changed summaries are still pending"));
        assert!(error.contains("unresolved default_switch_impact"));
    }

    #[test]
    fn checks_profile_diff_signoff_ready_after_human_signoff() {
        let markdown = r#"# check-paper V1 V2 Profile Diff Review Alice

- changed_summaries: 2

## Human Signoff

- status: signed_off_with_note
- reviewer: Alice Reviewer
- signed_off_at: 2026-05-22

## Changed Summaries

### Alice/paper-a

- review_status: accepted
- default_switch_impact: low

### Alice/paper-b

- review_status: accepted_with_note
- default_switch_impact: low
"#;

        let report = check_profile_diff_signoff(markdown);
        let text = format_profile_diff_signoff_report(&PathBuf::from("review.md"), &report);

        assert!(report.ready);
        assert!(profile_diff_signoff_gate_error(&report).is_none());
        assert!(text.contains("profile diff signoff: ready"));
        assert!(text.contains("reviewer: Alice Reviewer"));
        assert!(text.contains("accepted=1 accepted_with_note=1"));
    }

    #[test]
    fn formats_profile_gate_report_with_blockers_and_warnings() {
        let report = ProfileGateReport {
            ready: false,
            papers_with_v1: 2,
            papers_with_v2: 1,
            missing_v2: vec!["Alice/paper-b".to_string()],
            missing_v1: vec![],
            invalid_v2_profiles: vec![ProfileGateIssue {
                paper_key: "Alice/paper-c".to_string(),
                error: "missing evidence".to_string(),
            }],
            author_profile_v2_present: true,
            author_profile_v2_valid: false,
            author_profile_v2_error: Some("missing research_themes".to_string()),
            factual_objects: 4,
            claims_with_support_refs: 2,
            support_refs: 6,
            blockers: vec!["AuthorProfileV2 is missing or invalid".to_string()],
            warnings: vec!["1 paper summaries differ and need review".to_string()],
        };

        let text = format_profile_gate_report(&report);

        assert!(text.contains("profile gate: blocked"));
        assert!(text.contains("papers_with_v1: 2"));
        assert!(text.contains("- missing_v2: Alice/paper-b"));
        assert!(text.contains("- invalid_v2: Alice/paper-c [missing evidence]"));
        assert!(text.contains("author_profile_v2: invalid"));
        assert!(text.contains("support_refs: 6"));
        assert!(text.contains("- AuthorProfileV2 is missing or invalid"));
        assert!(text.contains("- 1 paper summaries differ and need review"));
    }

    #[test]
    fn formats_eval_baseline_markdown_with_status_modes_and_review_queue() {
        let settings = settings(Some("Alice"));
        let report = EvalReport {
            qa_profile_version: "v2".to_string(),
            total: 2,
            retrieval_hit_at_k: 0.5,
            qa_mode_summary: BTreeMap::from([(
                "profile_first".to_string(),
                EvalQaModeSummary {
                    total: 2,
                    retrieval_hit_at_k: 0.5,
                    citation_precision: 0.75,
                    answer_contains_required: 0.5,
                    route_reasons: BTreeMap::from([("broad_profile_context".to_string(), 2)]),
                },
            )]),
            route_hit_at_k: BTreeMap::from([("fts".to_string(), 0.5)]),
            route_candidate_count_avg: BTreeMap::from([("fts".to_string(), 3.0)]),
            citation_precision: 0.75,
            answer_contains_required: 0.5,
            insufficient_when_missing: 1.0,
            latency_ms: 42,
            cases: vec![
                EvalCaseReport {
                    author: "Alice".to_string(),
                    question: "What is the main contribution?".to_string(),
                    qa_profile_version: "v2".to_string(),
                    qa_mode: "profile_first".to_string(),
                    route_reason: "broad_profile_context".to_string(),
                    retrieved: vec!["Alice/paper-a".to_string()],
                    retrieval_hit: true,
                    route_hits: BTreeMap::new(),
                    citation_precision: 1.0,
                    answer_contains_required: true,
                    insufficient_when_missing: true,
                    latency_ms: 10,
                    missing_required_terms: vec![],
                    forbidden_terms_found: vec![],
                    retrieval_trace: json!({}),
                    answer_checked: false,
                    answer_contains_expected_terms: true,
                    answer_missing_expected_terms: vec![],
                    answer_evidence_valid: true,
                    answer_evidence_citation_precision: 1.0,
                    answer_validation_error: String::new(),
                },
                EvalCaseReport {
                    author: "Alice".to_string(),
                    question: "Which condition failed?".to_string(),
                    qa_profile_version: "v2".to_string(),
                    qa_mode: "profile_first".to_string(),
                    route_reason: "broad_profile_context".to_string(),
                    retrieved: vec!["Alice/paper-b".to_string()],
                    retrieval_hit: false,
                    route_hits: BTreeMap::new(),
                    citation_precision: 0.5,
                    answer_contains_required: false,
                    insufficient_when_missing: true,
                    latency_ms: 20,
                    missing_required_terms: vec!["82%".to_string()],
                    forbidden_terms_found: vec![],
                    retrieval_trace: json!({}),
                    answer_checked: true,
                    answer_contains_expected_terms: false,
                    answer_missing_expected_terms: vec!["82%".to_string()],
                    answer_evidence_valid: false,
                    answer_evidence_citation_precision: 0.0,
                    answer_validation_error: "missing evidence".to_string(),
                },
            ],
        };

        let text = format_eval_baseline_markdown(
            &settings,
            &PathBuf::from("tests/fixtures/golden_questions.json"),
            8,
            &report,
            &[(
                "Alice".to_string(),
                LibraryStatus {
                    papers: 3,
                    analyzed: 2,
                    stale_papers: 1,
                    failed_jobs: 1,
                    queued_jobs: 0,
                    running_jobs: 0,
                    retry_waiting_jobs: 0,
                    cancelled_jobs: 0,
                    qa_logs: 4,
                    avg_qa_latency_ms: Some(123.4),
                    total_qa_tokens: Some(1000),
                    total_qa_cost_usd: Some(0.0123),
                },
            )],
        );

        assert!(text.contains("# check-paper 真实作者评测 baseline"));
        assert!(text.contains("- fixture: tests/fixtures/golden_questions.json"));
        assert!(text.contains("- qa_profile_version: v2"));
        assert!(text.contains("| Alice | 3 | 2 | 1 | 0 | 0 | 0 | 1 | 4 | 123 | 1000 | 0.012300 |"));
        assert!(
            text.contains(
                "| profile_first | 2 | 0.500 | 0.750 | 0.500 | broad_profile_context=2 |"
            )
        );
        assert!(text.contains("| fts | 0.500 | 3.0 |"));
        assert!(text.contains("- [Alice] Which condition failed?"));
        assert!(text.contains("missing_required_terms: 82%"));
        assert!(text.contains("answer_validation_error: missing evidence"));
    }

    #[test]
    fn formats_eval_profile_comparison_markdown_with_threshold_decision() {
        fn report(version: &str, retrieval: f64, citation: f64) -> EvalReport {
            EvalReport {
                qa_profile_version: version.to_string(),
                total: 9,
                retrieval_hit_at_k: retrieval,
                qa_mode_summary: BTreeMap::from([(
                    "profile_first".to_string(),
                    EvalQaModeSummary {
                        total: 9,
                        retrieval_hit_at_k: retrieval,
                        citation_precision: citation,
                        answer_contains_required: 1.0,
                        route_reasons: BTreeMap::from([("broad_profile_context".to_string(), 9)]),
                    },
                )]),
                route_hit_at_k: BTreeMap::new(),
                route_candidate_count_avg: BTreeMap::new(),
                citation_precision: citation,
                answer_contains_required: 1.0,
                insufficient_when_missing: 1.0,
                latency_ms: 42,
                cases: Vec::new(),
            }
        }

        let settings = settings(Some("Alice"));
        let v1 = report("v1", 1.0, 0.80);
        let v2 = report("v2", 1.0, 0.79);
        let comparison =
            crate::eval::compare_eval_reports(&v1, &v2, EvalComparisonThresholds::default());

        let text = format_eval_profile_comparison_markdown(
            &settings,
            &PathBuf::from("tests/fixtures/golden_questions.json"),
            8,
            &v1,
            &v2,
            &comparison,
        );

        assert!(text.contains("# check-paper V1 V2 Eval Comparison"));
        assert!(text.contains("- default_switch_recommendation: eligible_for_manual_review"));
        assert!(
            text.contains("| citation_precision | 0.800 | 0.790 | -0.010 | 0.020 | 0.400 | pass |")
        );
        assert!(text.contains("| v1 | 9 | profile_first=9 |"));
        assert!(text.contains("| v2 | 9 | profile_first=9 |"));
    }

    #[test]
    fn eval_comparison_gate_error_reports_hold_blockers() {
        fn report(version: &str, retrieval: f64) -> EvalReport {
            EvalReport {
                qa_profile_version: version.to_string(),
                total: 9,
                retrieval_hit_at_k: retrieval,
                qa_mode_summary: BTreeMap::new(),
                route_hit_at_k: BTreeMap::new(),
                route_candidate_count_avg: BTreeMap::new(),
                citation_precision: 0.80,
                answer_contains_required: 1.0,
                insufficient_when_missing: 1.0,
                latency_ms: 0,
                cases: Vec::new(),
            }
        }
        let v1 = report("v1", 1.0);
        let v2 = report("v2", 0.99);
        let comparison =
            crate::eval::compare_eval_reports(&v1, &v2, EvalComparisonThresholds::default());

        let error = eval_comparison_gate_error(&comparison).unwrap();

        assert!(error.contains("eval comparison gate failed"));
        assert!(error.contains("retrieval_hit_at_k dropped"));
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
        settings.telegram_admin_user_ids = vec![123456789];
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
        assert!(text.contains("admin_user_ids: 123456789"));
        assert!(text.contains("proxy: socks5://127.0.0.1:7890"));
        assert!(text.contains("serve_command: ppc serve-telegram"));
    }

    #[test]
    fn formats_tg_health_missing_and_stale_states() {
        let settings = settings(None);

        let missing = format_tg_health(&settings, None);
        assert!(missing.contains("Telegram health: missing"));
        assert!(missing.contains("heartbeat: missing"));
        assert!(missing.contains("serve_command: ppc serve-telegram"));

        let stale = format_tg_health(
            &settings,
            Some(&RuntimeHeartbeat {
                name: "telegram_polling".to_string(),
                status: "polling".to_string(),
                updated_at: "2026-05-20 10:00:00".to_string(),
                age_seconds: Some(120),
            }),
        );
        assert!(stale.contains("Telegram health: stale"));
        assert!(stale.contains("status=polling"));
        assert!(stale.contains("age_seconds=120"));
    }

    #[test]
    fn formats_tg_health_fresh_heartbeat() {
        let settings = settings(None);
        let text = format_tg_health(
            &settings,
            Some(&RuntimeHeartbeat {
                name: "telegram_polling".to_string(),
                status: "polling".to_string(),
                updated_at: "2026-05-20 10:00:00".to_string(),
                age_seconds: Some(3),
            }),
        );

        assert!(text.contains("Telegram health: ok"));
        assert!(text.contains("heartbeat: telegram_polling status=polling"));
        assert!(text.contains("stale_after_seconds: 90"));
    }

    #[test]
    fn formats_tg_health_alert_message() {
        let mut settings = settings(None);
        settings.db_path = PathBuf::from("data/check_paper.sqlite");

        let missing = format_tg_health_alert(&settings, None);
        assert!(missing.contains("check-paper Telegram polling health failed"));
        assert!(missing.contains("status: missing"));
        assert!(missing.contains("heartbeat: missing"));
        assert!(missing.contains("check: ppc tg health --strict"));

        let stale = format_tg_health_alert(
            &settings,
            Some(&RuntimeHeartbeat {
                name: "telegram_polling".to_string(),
                status: "polling".to_string(),
                updated_at: "2026-05-20 10:00:00".to_string(),
                age_seconds: Some(120),
            }),
        );
        assert!(stale.contains("status: stale"));
        assert!(stale.contains("heartbeat: telegram_polling status=polling"));
        assert!(stale.contains("age_seconds=120"));
        assert!(stale.contains("stale_after_seconds: 90"));
    }

    #[test]
    fn resolves_tg_health_notify_chat_ids() {
        let mut settings = settings(None);
        settings.telegram_chat_ids = vec![-100, 42];

        assert_eq!(
            tg_health_notify_chat_ids(&settings, &[]).unwrap(),
            vec![-100, 42]
        );
        assert_eq!(tg_health_notify_chat_ids(&settings, &[7]).unwrap(), vec![7]);

        settings.telegram_chat_ids.clear();
        let error = tg_health_notify_chat_ids(&settings, &[])
            .unwrap_err()
            .to_string();
        assert!(error.contains("missing notify chat IDs"));
    }

    #[test]
    fn classifies_tg_health_for_strict_checks() {
        assert_eq!(tg_health_status(None), "missing");
        assert_eq!(
            tg_health_status(Some(&RuntimeHeartbeat {
                name: "telegram_polling".to_string(),
                status: "polling".to_string(),
                updated_at: "2026-05-20 10:00:00".to_string(),
                age_seconds: Some(90),
            })),
            "ok"
        );
        assert_eq!(
            tg_health_status(Some(&RuntimeHeartbeat {
                name: "telegram_polling".to_string(),
                status: "polling".to_string(),
                updated_at: "2026-05-20 10:00:00".to_string(),
                age_seconds: Some(91),
            })),
            "stale"
        );
        assert_eq!(
            tg_health_status(Some(&RuntimeHeartbeat {
                name: "telegram_polling".to_string(),
                status: "polling".to_string(),
                updated_at: "2026-05-20 10:00:00".to_string(),
                age_seconds: None,
            })),
            "stale"
        );
    }

    #[test]
    fn formats_tg_service_templates() {
        let bin = PathBuf::from("/opt/check-paper/ppc");
        let workdir = PathBuf::from("/opt/check-paper");
        let log = PathBuf::from("/opt/check-paper/data/ppc-telegram.log");

        let launchd =
            format_tg_service_template(TgServiceTemplateKind::Launchd, &bin, &workdir, &log);
        assert!(launchd.contains("<string>com.check-paper.telegram</string>"));
        assert!(launchd.contains("<string>/opt/check-paper/ppc</string>"));
        assert!(launchd.contains("<string>serve-telegram</string>"));
        assert!(launchd.contains("<key>KeepAlive</key>"));
        assert!(launchd.contains("<string>/opt/check-paper/data/ppc-telegram.log</string>"));

        let launchd_health =
            format_tg_service_template(TgServiceTemplateKind::LaunchdHealth, &bin, &workdir, &log);
        assert!(launchd_health.contains("<string>com.check-paper.telegram-health</string>"));
        assert!(launchd_health.contains("<string>tg</string>"));
        assert!(launchd_health.contains("<string>health</string>"));
        assert!(launchd_health.contains("<string>--strict</string>"));
        assert!(launchd_health.contains("<string>--notify</string>"));
        assert!(launchd_health.contains("<integer>300</integer>"));

        let systemd =
            format_tg_service_template(TgServiceTemplateKind::Systemd, &bin, &workdir, &log);
        assert!(systemd.contains("Description=check-paper Telegram bot polling service"));
        assert!(systemd.contains("WorkingDirectory=/opt/check-paper"));
        assert!(systemd.contains("ExecStart=/opt/check-paper/ppc serve-telegram"));
        assert!(systemd.contains("Restart=always"));
        assert!(systemd.contains("StandardOutput=append:/opt/check-paper/data/ppc-telegram.log"));

        let logrotate =
            format_tg_service_template(TgServiceTemplateKind::Logrotate, &bin, &workdir, &log);
        assert!(logrotate.contains("/opt/check-paper/data/ppc-telegram.log {"));
        assert!(logrotate.contains("    rotate 14"));
        assert!(logrotate.contains("    copytruncate"));
    }

    #[test]
    fn installs_tg_service_template_and_refuses_unforced_overwrite() {
        let dir = tempdir().unwrap();
        let output = dir.path().join("com.check-paper.telegram.plist");

        let written = install_tg_service_template(&output, "template-v1", false, false).unwrap();
        assert!(written);
        assert_eq!(fs::read_to_string(&output).unwrap(), "template-v1");

        let error = install_tg_service_template(&output, "template-v2", false, false)
            .unwrap_err()
            .to_string();
        assert!(error.contains("already exists"));
        assert_eq!(fs::read_to_string(&output).unwrap(), "template-v1");

        install_tg_service_template(&output, "template-v2", true, false).unwrap();
        assert_eq!(fs::read_to_string(&output).unwrap(), "template-v2");
    }

    #[test]
    fn dry_run_tg_service_install_does_not_write() {
        let dir = tempdir().unwrap();
        let output = dir.path().join("check-paper-telegram.service");

        let written = install_tg_service_template(&output, "template", false, true).unwrap();

        assert!(!written);
        assert!(!output.exists());
    }

    #[test]
    fn formats_tg_service_install_report_with_next_steps() {
        let launchd = format_tg_service_install_report(
            TgServiceTemplateKind::Launchd,
            &PathBuf::from("/Users/alice/Library/LaunchAgents/com.check-paper.telegram.plist"),
            true,
            false,
        );
        assert!(launchd.contains("Telegram service install"));
        assert!(launchd.contains("kind: launchd"));
        assert!(launchd.contains("status: written"));
        assert!(launchd.contains("launchctl bootstrap gui/$(id -u)"));
        assert!(launchd.contains("ppc tg health --strict --notify"));

        let launchd_health = format_tg_service_install_report(
            TgServiceTemplateKind::LaunchdHealth,
            &PathBuf::from(
                "/Users/alice/Library/LaunchAgents/com.check-paper.telegram-health.plist",
            ),
            true,
            false,
        );
        assert!(launchd_health.contains("kind: launchd-health"));
        assert!(launchd_health.contains("com.check-paper.telegram-health"));
        assert!(launchd_health.contains("TELEGRAM_CHAT_IDS"));

        let systemd = format_tg_service_install_report(
            TgServiceTemplateKind::Systemd,
            &PathBuf::from("/home/alice/.config/systemd/user/check-paper-telegram.service"),
            false,
            true,
        );
        assert!(systemd.contains("kind: systemd"));
        assert!(systemd.contains("status: dry_run"));
        assert!(systemd.contains("systemctl --user enable --now check-paper-telegram.service"));
    }

    #[test]
    fn checks_tg_service_template_installation_state() {
        let dir = tempdir().unwrap();
        let output = dir.path().join("com.check-paper.telegram.plist");
        let expected = "template-v1";

        let missing = check_tg_service_template(&output, expected).unwrap();
        let missing_report =
            format_tg_service_check_report(TgServiceTemplateKind::Launchd, &output, &missing);
        assert!(missing_report.contains("Telegram service check"));
        assert!(missing_report.contains("installed: no"));
        assert!(missing_report.contains("matches_expected_template: no_file"));
        assert!(missing_report.contains("ppc tg service-install --kind launchd"));

        fs::write(&output, "template-v0").unwrap();
        let mismatch = check_tg_service_template(&output, expected).unwrap();
        let mismatch_report =
            format_tg_service_check_report(TgServiceTemplateKind::Launchd, &output, &mismatch);
        assert!(mismatch_report.contains("installed: yes"));
        assert!(mismatch_report.contains("matches_expected_template: no"));
        assert!(mismatch_report.contains("--force"));

        fs::write(&output, expected).unwrap();
        let matched = check_tg_service_template(&output, expected).unwrap();
        let matched_report =
            format_tg_service_check_report(TgServiceTemplateKind::Launchd, &output, &matched);
        assert!(matched_report.contains("matches_expected_template: yes"));
        assert!(matched_report.contains("launchctl print gui/$(id -u)/com.check-paper.telegram"));
        assert!(matched_report.contains("ppc tg health --strict --notify"));
    }

    #[test]
    fn defaults_tg_service_log_path_beside_database_under_workdir() {
        let mut settings = settings(None);
        settings.db_path = PathBuf::from("data/check_paper.sqlite");
        let workdir = PathBuf::from("/opt/check-paper");

        assert_eq!(
            default_tg_service_log_path(&settings, &workdir),
            PathBuf::from("/opt/check-paper/data/ppc-telegram.log")
        );
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
