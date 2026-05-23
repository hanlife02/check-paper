#!/usr/bin/env sh
set -eu

usage() {
  cat <<'EOF'
Usage: scripts/v2-default-switch-plan.sh [AUTHOR]

Generate a read-only V2 default switch plan.

This script runs current read-only gates, writes a Markdown plan, and prints the
exact preflight/apply/rollback commands for a human-controlled default profile
switch. It does not edit .paper-check.json, change CHECK_PAPER_QA_PROFILE_VERSION,
modify profile diff review docs, or deploy services.

Environment overrides:
  CHECK_PAPER_PPC_BIN                   ppc binary, default: target/debug/ppc
  CHECK_PAPER_V2_SWITCH_PLAN_REPORT_DIR output dir, default: /private/tmp/check-paper-v2-switch-plan
  CHECK_PAPER_PROFILE_DIFF_REVIEW       profile diff review Markdown
  CHECK_PAPER_V2_TARGET_PROFILE_VERSION target default, default: auto
EOF
}

if [ "${1:-}" = "--help" ] || [ "${1:-}" = "-h" ]; then
  usage
  exit 0
fi

author="${1:-${CHECK_PAPER_V2_AUTHOR:-Ruqiang ZOU}}"
ppc_bin="${CHECK_PAPER_PPC_BIN:-target/debug/ppc}"
report_dir="${CHECK_PAPER_V2_SWITCH_PLAN_REPORT_DIR:-/private/tmp/check-paper-v2-switch-plan}"
target_profile_version="${CHECK_PAPER_V2_TARGET_PROFILE_VERSION:-auto}"
review_doc="${CHECK_PAPER_PROFILE_DIFF_REVIEW:-}"
stamp="$(date -u +%Y-%m-%dT%H%M%SZ)"
report_path="$report_dir/check-paper V2 default switch plan $stamp.md"
readiness_report_dir="$report_dir/v2-default-readiness"
ledger_report_dir="$report_dir/evidence-ledger"

mkdir -p "$report_dir" "$readiness_report_dir" "$ledger_report_dir"

if [ ! -x "$ppc_bin" ]; then
  cargo build --bin ppc
fi

failed_sections=""
readiness_path="<unknown>"
readiness_result="<unknown>"
signoff_status="<not_configured>"

record_failed_section() {
  title="$1"
  failed_sections="${failed_sections}${failed_sections:+, }$title"
}

shell_quote() {
  printf "'%s'" "$(printf '%s' "$1" | sed "s/'/'\\\\''/g")"
}

append_command() {
  title="$1"
  shift
  tmp="$report_dir/.v2-switch-plan-$stamp-$$.tmp"
  set +e
  "$@" > "$tmp" 2>&1
  status="$?"
  set -e
  {
    echo
    echo "## $title"
    echo
    echo "- exit_status: $status"
    echo
    echo '```text'
    cat "$tmp"
    echo '```'
  } >> "$report_path"
  if [ "$status" -ne 0 ]; then
    record_failed_section "$title"
  fi
  rm -f "$tmp"
  return "$status"
}

append_probe_command() {
  title="$1"
  shift
  tmp="$report_dir/.v2-switch-plan-$stamp-$$.tmp"
  set +e
  "$@" > "$tmp" 2>&1
  status="$?"
  set -e
  {
    echo
    echo "## $title"
    echo
    echo "- exit_status: $status"
    echo
    echo '```text'
    cat "$tmp"
    echo '```'
  } >> "$report_path"
  rm -f "$tmp"
  return "$status"
}

append_hold_section() {
  title="$1"
  message="$2"
  {
    echo
    echo "## $title"
    echo
    echo "- exit_status: 1"
    echo
    echo '```text'
    echo "$message"
    echo '```'
  } >> "$report_path"
  record_failed_section "$title"
}

{
  echo "# check-paper V2 default switch plan $stamp"
  echo
  echo "- generated_at: $(date -u +%Y-%m-%dT%H:%M:%SZ)"
  echo "- author: $author"
  echo "- target_profile_version: $target_profile_version"
  echo "- ppc_bin: $ppc_bin"
  echo "- report_dir: $report_dir"
  if [ -n "$review_doc" ]; then
    echo "- profile_diff_review: $review_doc"
  else
    echo "- profile_diff_review: <not configured>"
  fi
  echo "- host: $(hostname 2>/dev/null || echo unknown)"
  echo "- uname: $(uname -a 2>/dev/null || echo unknown)"
  echo
  echo "This plan is read-only. It gathers current switch evidence and prints commands for a human-controlled default switch."
} > "$report_path"

append_command "Current config" "$ppc_bin" config --show
append_command "V2 profile gate" "$ppc_bin" profile gate --author "$author"

if [ -z "$review_doc" ]; then
  append_hold_section \
    "Profile diff human signoff" \
    "missing CHECK_PAPER_PROFILE_DIFF_REVIEW; set it to the signed profile diff review Markdown before switching defaults."
else
  if append_probe_command "Profile diff human signoff" "$ppc_bin" profile signoff --input "$review_doc" --fail-on-hold; then
    signoff_status="ready"
  else
    signoff_status="hold"
    record_failed_section "Profile diff human signoff"
  fi
fi

tmp_readiness="$report_dir/.v2-switch-readiness-$stamp-$$.tmp"
set +e
env \
  CHECK_PAPER_QA_PROFILE_VERSION="$target_profile_version" \
  CHECK_PAPER_PROFILE_DIFF_REVIEW="$review_doc" \
  CHECK_PAPER_V2_READINESS_REPORT_DIR="$readiness_report_dir" \
  CHECK_PAPER_PPC_BIN="$ppc_bin" \
  scripts/v2-default-readiness.sh "$author" > "$tmp_readiness" 2>&1
readiness_status="$?"
set -e
readiness_path="$(sed -n 's/^V2 default readiness evidence: //p' "$tmp_readiness" | tail -n 1)"
if [ -n "$readiness_path" ] && [ -f "$readiness_path" ]; then
  readiness_result="$(sed -n 's/^- result: //p' "$readiness_path" | tail -n 1)"
fi
if [ -z "$readiness_result" ]; then
  readiness_result="<unknown>"
fi
{
  echo
  echo "## Target-profile readiness dry run"
  echo
  echo "- exit_status: $readiness_status"
  echo "- readiness_path: ${readiness_path:-<unknown>}"
  echo "- readiness_result: $readiness_result"
  echo
  echo '```text'
  cat "$tmp_readiness"
  echo '```'
} >> "$report_path"
rm -f "$tmp_readiness"

if [ "$readiness_status" -ne 0 ] || [ "$readiness_result" != "ready" ]; then
  record_failed_section "Target-profile readiness dry run"
fi

append_probe_command "Evidence ledger snapshot" env \
  CHECK_PAPER_EVIDENCE_LEDGER_REPORT_DIR="$ledger_report_dir" \
  scripts/evidence-ledger.sh || true

{
  echo
  echo "## Switch Preconditions"
  echo
  echo "- profile_gate_ready: see V2 profile gate section"
  echo "- signoff_status: $signoff_status"
  echo "- target_readiness_result: $readiness_result"
  echo "- target_readiness_path: ${readiness_path:-<unknown>}"
  echo
  echo "## Human Apply Checklist"
  echo
  echo "Only apply these commands after a human reviewer has filled the profile diff review signoff fields and the target-profile readiness dry run reports \`ready\`."
  echo
  echo '```sh'
  if [ -n "$review_doc" ]; then
    echo "$ppc_bin profile signoff --input $(shell_quote "$review_doc") --fail-on-hold"
    echo "CHECK_PAPER_QA_PROFILE_VERSION=$(shell_quote "$target_profile_version") CHECK_PAPER_PROFILE_DIFF_REVIEW=$(shell_quote "$review_doc") scripts/v2-default-readiness.sh $(shell_quote "$author")"
  else
    echo "export CHECK_PAPER_PROFILE_DIFF_REVIEW=/path/to/profile-diff-review.md"
    echo "$ppc_bin profile signoff --input \"\$CHECK_PAPER_PROFILE_DIFF_REVIEW\" --fail-on-hold"
    echo "CHECK_PAPER_QA_PROFILE_VERSION=$(shell_quote "$target_profile_version") scripts/v2-default-readiness.sh $(shell_quote "$author")"
  fi
  echo "# Persistent config path: run ppc config and enter qa-profile-version=$(shell_quote "$target_profile_version"), or set CHECK_PAPER_QA_PROFILE_VERSION=$(shell_quote "$target_profile_version") in the target process environment."
  echo "$ppc_bin config"
  echo "$ppc_bin config --show"
  if [ -n "$review_doc" ]; then
    echo "CHECK_PAPER_PROFILE_DIFF_REVIEW=$(shell_quote "$review_doc") scripts/v2-default-readiness.sh $(shell_quote "$author")"
  else
    echo "CHECK_PAPER_PROFILE_DIFF_REVIEW=\"\$CHECK_PAPER_PROFILE_DIFF_REVIEW\" scripts/v2-default-readiness.sh $(shell_quote "$author")"
  fi
  echo "scripts/regression-check.sh"
  echo "scripts/evidence-ledger.sh"
  echo '```'
  echo
  echo "## Rollback Checklist"
  echo
  echo '```sh'
  echo "# Persistent rollback path: run ppc config and enter qa-profile-version='v1', or set CHECK_PAPER_QA_PROFILE_VERSION='v1' in the target process environment."
  echo "$ppc_bin config"
  echo "$ppc_bin config --show"
  echo "CHECK_PAPER_QA_PROFILE_VERSION='v1' scripts/regression-check.sh"
  echo '```'
  echo
  echo "## Plan Result"
  echo
  if [ -z "$failed_sections" ]; then
    echo "- result: ready_to_apply_after_human_confirmation"
    echo "- failed_sections: <none>"
  else
    echo "- result: hold"
    echo "- failed_sections: $failed_sections"
  fi
} >> "$report_path"

echo "V2 default switch plan: $report_path"
