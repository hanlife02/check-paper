#!/usr/bin/env sh
set -eu

eval_report_dir="${CHECK_PAPER_EVAL_REPORT_DIR:-${CHECK_PAPER_REGRESSION_REPORT_DIR:-/private/tmp/check-paper-eval-gate}}"
fixture="${1:-${CHECK_PAPER_EVAL_FIXTURE:-data/eval/ruqiang_zou_golden_questions_expanded_2026-05-21.json}}"
top_k="${CHECK_PAPER_EVAL_TOP_K:-8}"
ppc_bin="${CHECK_PAPER_PPC_BIN:-target/debug/ppc}"
trend_days="${CHECK_PAPER_TREND_DAYS:-14}"
started_at="$(date -u +%Y-%m-%dT%H:%M:%SZ)"
manifest_stamp="$(date -u +%Y-%m-%dT%H%M%SZ)"
manifest_path="$eval_report_dir/check-paper regression evidence $manifest_stamp.md"
manifest_marker="$eval_report_dir/.regression-start-$manifest_stamp"
current_step="not_started"
export CHECK_PAPER_EVAL_REPORT_DIR="$eval_report_dir"

mkdir -p "$eval_report_dir"
: > "$manifest_marker"

write_manifest() {
  status="$?"
  set +e
  finished_at="$(date -u +%Y-%m-%dT%H:%M:%SZ)"
  git_commit="$(git rev-parse --short HEAD 2>/dev/null || echo unknown)"
  git_status="$(git status --short 2>/dev/null || echo unavailable)"
  if [ "$status" -eq 0 ]; then
    result="pass"
    failed_step="<none>"
  else
    result="fail"
    failed_step="$current_step"
  fi
  {
    echo "# check-paper regression evidence $manifest_stamp"
    echo
    echo "- result: $result"
    echo "- exit_status: $status"
    echo "- started_at: $started_at"
    echo "- finished_at: $finished_at"
    echo "- git_commit: $git_commit"
    echo "- report_dir: $eval_report_dir"
    echo "- fixture: $fixture"
    echo "- top_k: $top_k"
    echo "- ppc_bin: $ppc_bin"
    echo "- trend_days: $trend_days"
    echo "- failed_step: $failed_step"
    echo
    echo "## Commands"
    echo
    echo "- cargo fmt --all -- --check"
    echo "- cargo clippy --all-targets -- -D warnings"
    echo "- cargo test"
    echo "- scripts/eval-v2-gate.sh $fixture"
    echo "- scripts/log-trend-report.sh"
    echo
    echo "## Report Artifacts"
    echo
    find "$eval_report_dir" -maxdepth 1 -type f -name '*.md' -newer "$manifest_marker" -print | sort | while IFS= read -r path; do
      echo "- $path"
    done
    echo
    echo "## Git Status"
    echo
    echo '```text'
    if [ -n "$git_status" ]; then
      echo "$git_status"
    else
      echo "<clean>"
    fi
    echo '```'
  } > "$manifest_path"
  rm -f "$manifest_marker"
  echo "regression evidence: $manifest_path"
  exit "$status"
}

trap write_manifest EXIT

run() {
  current_step="$*"
  printf '\n==> %s\n' "$*"
  "$@"
}

run cargo fmt --all -- --check
run cargo clippy --all-targets -- -D warnings
run cargo test

printf '\n==> scripts/eval-v2-gate.sh\n'
current_step="scripts/eval-v2-gate.sh $fixture"
scripts/eval-v2-gate.sh "$@"

printf '\n==> scripts/log-trend-report.sh\n'
current_step="scripts/log-trend-report.sh"
CHECK_PAPER_TREND_REPORT_DIR="$eval_report_dir" scripts/log-trend-report.sh
current_step="completed"
