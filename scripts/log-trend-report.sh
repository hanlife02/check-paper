#!/usr/bin/env sh
set -eu

ppc_bin="${CHECK_PAPER_PPC_BIN:-target/debug/ppc}"
report_dir="${CHECK_PAPER_TREND_REPORT_DIR:-${CHECK_PAPER_REGRESSION_REPORT_DIR:-/private/tmp/check-paper-log-trends}}"
days="${CHECK_PAPER_TREND_DAYS:-14}"
author="${CHECK_PAPER_TREND_AUTHOR:-}"
chat_id="${CHECK_PAPER_TREND_CHAT_ID:-}"

if [ ! -x "$ppc_bin" ]; then
  cargo build --bin ppc
fi

mkdir -p "$report_dir"
report_path="$report_dir/check-paper QA Telegram trend $(date +%Y-%m-%d).md"

{
  echo "# check-paper QA / Telegram trend $(date +%Y-%m-%d)"
  echo
  echo "- generated_at: $(date -u +%Y-%m-%dT%H:%M:%SZ)"
  echo "- days: $days"
  if [ -n "$author" ]; then
    echo "- author: $author"
  else
    echo "- author: all"
  fi
  if [ -n "$chat_id" ]; then
    echo "- telegram_chat_id: $chat_id"
  else
    echo "- telegram_chat_id: all"
  fi
  echo
  echo "## QA Trend"
  echo
  echo '```text'
  if [ -n "$author" ]; then
    "$ppc_bin" logs qa --trend --days "$days" --author "$author"
  else
    "$ppc_bin" logs qa --trend --days "$days"
  fi
  echo '```'
  echo
  echo "## Telegram Trend"
  echo
  echo '```text'
  if [ -n "$chat_id" ]; then
    "$ppc_bin" logs telegram --trend --days "$days" --chat-id "$chat_id"
  else
    "$ppc_bin" logs telegram --trend --days "$days"
  fi
  echo '```'
  echo
  echo "## Telegram Summary"
  echo
  echo '```text'
  if [ -n "$chat_id" ]; then
    "$ppc_bin" logs telegram --summary --chat-id "$chat_id"
  else
    "$ppc_bin" logs telegram --summary
  fi
  echo '```'
} > "$report_path"

echo "trend report: $report_path"
