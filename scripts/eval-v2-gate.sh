#!/usr/bin/env sh
set -eu

fixture="${1:-${CHECK_PAPER_EVAL_FIXTURE:-data/eval/ruqiang_zou_golden_questions_expanded_2026-05-21.json}}"
top_k="${CHECK_PAPER_EVAL_TOP_K:-8}"
ppc_bin="${CHECK_PAPER_PPC_BIN:-target/debug/ppc}"
report_dir="${CHECK_PAPER_EVAL_REPORT_DIR:-}"

if [ ! -f "$fixture" ]; then
  echo "eval gate fixture not found: $fixture" >&2
  echo "Pass a fixture path or set CHECK_PAPER_EVAL_FIXTURE." >&2
  exit 2
fi

if [ ! -x "$ppc_bin" ]; then
  cargo build --bin ppc
fi

if [ -n "$report_dir" ]; then
  mkdir -p "$report_dir"
  report_path="$report_dir/check-paper V1 V2 eval gate $(date +%Y-%m-%d).md"
  "$ppc_bin" eval \
    --fixture "$fixture" \
    --top-k "$top_k" \
    --compare-profile-versions \
    --baseline-markdown \
    --fail-on-hold \
    --output "$report_path"
  echo "eval gate report: $report_path"
else
  "$ppc_bin" eval \
    --fixture "$fixture" \
    --top-k "$top_k" \
    --compare-profile-versions \
    --baseline-markdown \
    --fail-on-hold
fi
