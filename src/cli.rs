use anyhow::{Result, anyhow};
use clap::{Args, Parser, Subcommand};
use indicatif::{ProgressBar, ProgressStyle};
use serde_json::json;
use std::collections::BTreeMap;
use std::io::{self, Write};
use std::thread;
use std::time::Duration;

use crate::bots::handlers::BotHandlers;
use crate::bots::telegram_bot::TelegramBot;
use crate::config::{Settings, config_path, load_config, redacted_config, save_config};
use crate::papers::loader::load_paper;
use crate::papers::scanner::scan_paper_dirs;
use crate::qa::answerer::Answerer;
use crate::retrieval::chunker::chunk_paper;
use crate::storage::Storage;
use crate::understanding::author_analyzer::build_author_profile;
use crate::understanding::llm::{LlmConfig, OpenAiCompatibleClient};
use crate::understanding::paper_analyzer::analyze_paper;

#[derive(Parser)]
#[command(version, about = "Analyze local paper archives and answer questions.")]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    Config(ConfigArgs),
    Llm {
        #[command(subcommand)]
        command: LlmCommand,
    },
    Tg {
        #[command(subcommand)]
        command: TgCommand,
    },
    Scan(AuthorArgs),
    Ingest(AuthorArgs),
    Analyze(AnalyzeArgs),
    Sync(AnalyzeArgs),
    Ask(AskArgs),
    Profile(AuthorArgs),
    ServeTelegram,
}

#[derive(Args)]
struct ConfigArgs {
    #[arg(long)]
    show: bool,
}

#[derive(Subcommand)]
enum LlmCommand {
    Config(LlmConfigArgs),
}

#[derive(Args)]
struct LlmConfigArgs {
    #[arg(long)]
    show: bool,
}

#[derive(Subcommand)]
enum TgCommand {
    Config(TgConfigArgs),
}

#[derive(Args)]
struct TgConfigArgs {
    #[arg(long)]
    show: bool,
}

#[derive(Args)]
struct AuthorArgs {
    #[arg(long)]
    author: Option<String>,
}

#[derive(Args, Clone)]
struct AnalyzeArgs {
    #[arg(long)]
    author: Option<String>,
    #[arg(long)]
    limit: Option<usize>,
    #[arg(long)]
    force: bool,
    #[arg(long)]
    skip_author_profile: bool,
}

#[derive(Args)]
struct AskArgs {
    #[arg(long)]
    author: Option<String>,
    question: Vec<String>,
}

pub fn run() -> Result<()> {
    let cli = Cli::parse();
    match cli.command {
        Command::Config(args) => cmd_config(args),
        Command::Llm { command } => match command {
            LlmCommand::Config(args) => cmd_llm_config(args),
        },
        Command::Tg { command } => match command {
            TgCommand::Config(args) => cmd_tg_config(args),
        },
        command => {
            let settings = Settings::from_sources();
            settings.ensure_dirs()?;
            match command {
                Command::Scan(args) => cmd_scan(args, &settings),
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
                Command::Ask(args) => cmd_ask(args, &settings),
                Command::Profile(args) => cmd_profile(args, &settings),
                Command::ServeTelegram => cmd_serve_telegram(&settings),
                Command::Config(_) | Command::Llm { .. } | Command::Tg { .. } => unreachable!(),
            }
        }
    }
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
                .unwrap_or("root"),
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

    let path = save_config(&updates, None)?;
    println!("saved LLM config to {}", path.display());
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
    progress.set_style(
        ProgressStyle::with_template(
            "{prefix:.bold} [{elapsed_precise}] [{wide_bar:.cyan/blue}] {pos}/{len} {msg}",
        )
        .unwrap()
        .progress_chars("=>-"),
    );
    progress.set_prefix(prefix);
    progress
}

fn paper_progress(message: String) -> ProgressBar {
    let progress = ProgressBar::new_spinner();
    progress.set_style(
        ProgressStyle::with_template("{spinner:.cyan} [{elapsed_precise}] {msg}")
            .unwrap()
            .tick_chars("|/-\\"),
    );
    progress.enable_steady_tick(Duration::from_millis(120));
    progress.set_message(message);
    progress
}

fn display_name(path: &std::path::Path) -> String {
    path.file_name()
        .map(|name| name.to_string_lossy().to_string())
        .unwrap_or_else(|| path.display().to_string())
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
        let chunks = chunk_paper(&paper, 3200, 350);
        if storage.upsert_paper(&paper, &chunks)? {
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
    let mut rows = storage.papers_needing_analysis(&author, args.force)?;
    if let Some(limit) = args.limit {
        rows.truncate(limit);
    }
    println!("papers needing analysis: {}", rows.len());
    let mut failures = Vec::new();
    for (index, row) in rows.iter().enumerate() {
        let paper_dir = settings.paper_root.join(&row.author).join(&row.paper_id);
        let paper = load_paper(&settings.paper_root, &paper_dir)?;
        let message = format!(
            "[{}/{}] {} {}",
            index + 1,
            rows.len(),
            paper.year(),
            paper.title()
        );
        let progress = paper_progress(message.clone());
        match analyze_paper_with_retries(&paper, &llm, &progress, &message) {
            Ok(profile) => {
                storage.save_paper_profile(&paper.key(), &paper.source_hash, &profile)?;
                progress.finish_with_message(format!("{message} done"));
            }
            Err(err) => {
                progress.finish_with_message(format!("{message} failed"));
                failures.push((paper.key(), err.to_string()));
            }
        }
    }
    println!(
        "analyzed {}; failed {}",
        rows.len().saturating_sub(failures.len()),
        failures.len()
    );

    if !args.skip_author_profile {
        let profiles = storage.paper_profiles(&author, None)?;
        if !profiles.is_empty() {
            let author_profile = build_author_profile(&author, &profiles, Some(&llm))?;
            storage.save_author_profile(&author, &author_profile)?;
            println!(
                "updated author profile with {} paper profiles",
                profiles.len()
            );
        }
    }
    if !failures.is_empty() {
        println!(
            "analysis completed with {} failed papers; rerun later to retry them",
            failures.len()
        );
        for (paper_key, err) in failures.iter().take(20) {
            println!("- {paper_key}: {err}");
        }
        if failures.len() > 20 {
            println!("- ... {} more", failures.len() - 20);
        }
    }
    Ok(())
}

fn analyze_paper_with_retries(
    paper: &crate::papers::models::Paper,
    llm: &OpenAiCompatibleClient,
    progress: &ProgressBar,
    message: &str,
) -> Result<serde_json::Value> {
    let mut last_error = None;
    for attempt in 1..=3 {
        progress.set_message(format!("{message} attempt {attempt}/3"));
        match analyze_paper(paper, llm, 22000) {
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
    require_llm(settings)?;
    let question = args.question.join(" ");
    let storage = Storage::open(&settings.db_path)?;
    let answerer = Answerer::new(&storage, make_llm(settings)?);
    println!("{}", answerer.answer(&author, &question)?);
    Ok(())
}

fn cmd_profile(args: AuthorArgs, settings: &Settings) -> Result<()> {
    let author = resolve_author(args.author.as_deref(), settings)?;
    let storage = Storage::open(&settings.db_path)?;
    if let Some(profile) = storage.get_author_profile(&author)? {
        println!("{}", serde_json::to_string_pretty(&profile)?);
        Ok(())
    } else {
        println!("no author profile for {author}");
        Ok(())
    }
}

fn cmd_serve_telegram(settings: &Settings) -> Result<()> {
    require_llm(settings)?;
    let token = settings
        .telegram_bot_token
        .clone()
        .ok_or_else(|| anyhow!("missing TELEGRAM_BOT_TOKEN; run `ppc tg config`"))?;
    let storage = Storage::open(&settings.db_path)?;
    let answerer = Answerer::new(&storage, make_llm(settings)?);
    let handlers = BotHandlers::new(&storage, answerer, settings.default_author.clone());
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
        .ok_or_else(|| anyhow!("missing author; pass --author or run `ppc config`"))
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
    })
}
