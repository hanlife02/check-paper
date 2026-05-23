#!/usr/bin/env sh
set -eu

author="${1:-${CHECK_PAPER_V2_AUTHOR:-Ruqiang ZOU}}"
ppc_bin="${CHECK_PAPER_PPC_BIN:-target/debug/ppc}"
report_dir="${CHECK_PAPER_V2_READINESS_REPORT_DIR:-/private/tmp/check-paper-v2-readiness}"
fixture="${CHECK_PAPER_EVAL_FIXTURE:-data/eval/ruqiang_zou_golden_questions_expanded_2026-05-21.json}"
top_k="${CHECK_PAPER_EVAL_TOP_K:-8}"
target_profile_version="${CHECK_PAPER_V2_TARGET_PROFILE_VERSION:-auto}"
review_doc="${CHECK_PAPER_PROFILE_DIFF_REVIEW:-}"
stamp="$(date -u +%Y-%m-%dT%H%M%SZ)"
report_path="$report_dir/check-paper V2 default readiness $stamp.md"

mkdir -p "$report_dir"

if [ ! -x "$ppc_bin" ]; then
  cargo build --bin ppc
fi

failed_sections=""

record_failed_section() {
  title="$1"
  failed_sections="${failed_sections}${failed_sections:+, }$title"
}

append_command() {
  title="$1"
  shift
  tmp="$report_dir/.v2-readiness-$stamp.tmp"
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
  if [ "$status" -ne 0 ]; then
    record_failed_section "$title"
  fi
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

append_default_profile_check() {
  title="Default profile target check"
  tmp="$report_dir/.v2-readiness-config-$stamp.tmp"
  set +e
  "$ppc_bin" config --show > "$tmp" 2>&1
  command_status="$?"
  set -e

  effective_profile_version="<unknown>"
  effective_source="config"
  if [ -n "${CHECK_PAPER_QA_PROFILE_VERSION:-}" ]; then
    effective_profile_version="$CHECK_PAPER_QA_PROFILE_VERSION"
    effective_source="environment CHECK_PAPER_QA_PROFILE_VERSION"
  elif [ "$command_status" -eq 0 ]; then
    parsed_profile_version="$(sed -n 's/.*"CHECK_PAPER_QA_PROFILE_VERSION"[[:space:]]*:[[:space:]]*"\([^"]*\)".*/\1/p' "$tmp" | head -n 1)"
    if [ -n "$parsed_profile_version" ]; then
      effective_profile_version="$parsed_profile_version"
    else
      effective_profile_version="v1"
      effective_source="default fallback"
    fi
  fi

  status=1
  if [ "$command_status" -eq 0 ] && [ "$effective_profile_version" = "$target_profile_version" ]; then
    status=0
  fi

  {
    echo
    echo "## $title"
    echo
    echo "- exit_status: $status"
    echo "- command_exit_status: $command_status"
    echo "- expected_profile_version: $target_profile_version"
    echo "- effective_profile_version: $effective_profile_version"
    echo "- effective_source: $effective_source"
    echo
    echo '```text'
    cat "$tmp"
    echo '```'
  } >> "$report_path"
  rm -f "$tmp"

  if [ "$status" -ne 0 ]; then
    record_failed_section "$title"
  fi
}

append_signoff_check() {
  if [ -z "$review_doc" ]; then
    append_hold_section \
      "Profile diff human signoff" \
      "missing CHECK_PAPER_PROFILE_DIFF_REVIEW; set it to the profile diff review Markdown before default switch."
    return
  fi

  if [ ! -f "$review_doc" ]; then
    append_hold_section \
      "Profile diff human signoff" \
      "profile diff review file not found: $review_doc"
    return
  fi

  append_command "Profile diff human signoff" "$ppc_bin" profile signoff --input "$review_doc" --fail-on-hold
}

{
  echo "# check-paper V2 default readiness $stamp"
  echo
  echo "- generated_at: $(date -u +%Y-%m-%dT%H:%M:%SZ)"
  echo "- author: $author"
  echo "- ppc_bin: $ppc_bin"
  echo "- report_dir: $report_dir"
  echo "- fixture: $fixture"
  echo "- top_k: $top_k"
  echo "- target_profile_version: $target_profile_version"
  if [ -n "$review_doc" ]; then
    echo "- profile_diff_review: $review_doc"
  else
    echo "- profile_diff_review: <not configured>"
  fi
  echo "- host: $(hostname 2>/dev/null || echo unknown)"
  echo "- uname: $(uname -a 2>/dev/null || echo unknown)"
  echo
  echo "This report does not change CHECK_PAPER_QA_PROFILE_VERSION, edit config files, or deploy services."
} > "$report_path"

append_default_profile_check
append_command "V2 profile gate" "$ppc_bin" profile gate --author "$author"
append_signoff_check
append_command "V1/V2 eval gate" env \
  CHECK_PAPER_EVAL_REPORT_DIR="$report_dir" \
  CHECK_PAPER_EVAL_TOP_K="$top_k" \
  CHECK_PAPER_PPC_BIN="$ppc_bin" \
  CHECK_PAPER_EVAL_FIXTURE="$fixture" \
  scripts/eval-v2-gate.sh "$fixture"

{
  echo
  echo "## Evidence Result"
  echo
  if [ -z "$failed_sections" ]; then
    echo "- result: ready"
    echo "- failed_sections: <none>"
  else
    echo "- result: hold"
    echo "- failed_sections: $failed_sections"
  fi
  echo
  echo "## Next Steps"
  echo
  if [ -z "$failed_sections" ]; then
    echo "- Keep this report with the default profile switch/change record and subsequent regression evidence."
  else
    echo "- Resolve failed sections, then rerun scripts/v2-default-readiness.sh on the target machine."
    echo "- Do not switch the default profile source until signoff and eval gate evidence are both ready."
  fi
} >> "$report_path"

echo "V2 default readiness evidence: $report_path"

if [ -n "$failed_sections" ] && [ "${CHECK_PAPER_V2_READINESS_FAIL_ON_HOLD:-0}" = "1" ]; then
  exit 1
fi
