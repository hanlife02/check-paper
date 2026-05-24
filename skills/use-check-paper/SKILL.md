---
name: use-check-paper
description: "Use this check-paper repository as a local Evidence QA service. Trigger when the user asks how to configure, run, test, operate, query, monitor, or troubleshoot this project's CLI or Telegram bot."
---

# Use check-paper

## Scope

Operate the local `check-paper` service from the repository root. Prefer `target/debug/ppc` for commands unless the user installed `ppc` globally.

## Preflight

1. Confirm the working directory is the project root containing `Cargo.toml`, `README.md`, and `src/`.
2. Check configuration without exposing secrets:

```bash
target/debug/ppc config --show
target/debug/ppc llm config --show
target/debug/ppc tg config --show
```

3. Check connectivity:

```bash
target/debug/ppc llm check
target/debug/ppc tg status
target/debug/ppc authors
```

If Telegram or LLM cannot connect and Surge is running locally, inspect local proxy ports and set `CHECK_PAPER_PROXY`, for example `http://127.0.0.1:6152`.

## Local Service

Start Telegram polling:

```bash
mkdir -p data
nohup target/debug/ppc serve-telegram > data/ppc-telegram.log 2>&1 &
```

Verify:

```bash
target/debug/ppc tg health
tail -n 80 data/ppc-telegram.log
```

`tg health: missing` means the polling loop has not written heartbeat to the configured database. `tg health: stale` means the service likely stopped or is stuck.

## CLI Workflows

List authors and ingest local paper directories:

```bash
target/debug/ppc authors
target/debug/ppc scan --author "AUTHOR"
target/debug/ppc ingest --author "AUTHOR"
```

Ask:

```bash
target/debug/ppc ask --author "AUTHOR" "QUESTION"
```

Analyze and build profiles:

```bash
target/debug/ppc analyze --author "AUTHOR" --limit 5
target/debug/ppc classify --author "AUTHOR"
target/debug/ppc extract --author "AUTHOR" --v2
target/debug/ppc comprehend --author "AUTHOR" --v2
target/debug/ppc comprehend --author "AUTHOR" --v2 --author-profile
```

Use `sync` when the user wants ingest plus analysis queueing:

```bash
target/debug/ppc sync --author "AUTHOR"
```

## Telegram Use

In allowed groups, the user must mention the bot:

```text
@你的Bot用户名 /status
@你的Bot用户名 /authors
@你的Bot用户名 QUESTION
@你的Bot用户名 /sources
```

Plain QA does not require `TELEGRAM_ADMIN_USER_IDS`. Admin-only commands such as `/sync`, `/analyze`, `/embed`, and `/comprehend` require the sender's Telegram user id in `TELEGRAM_ADMIN_USER_IDS`.

## Logs And Evidence

Use these for debugging and regression evidence:

```bash
target/debug/ppc logs qa --last 20
target/debug/ppc logs telegram --summary
scripts/regression-check.sh
scripts/evidence-ledger.sh
```

Do not expose API keys or Telegram bot tokens in user-facing output. The project config display already redacts secrets.
