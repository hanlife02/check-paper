#!/usr/bin/env sh
set -eu

usage() {
  cat <<'EOF'
Usage: scripts/github-actions-evidence.sh

Generate a read-only GitHub Actions regression evidence report.

This script checks the local regression workflow definition and, when the GitHub
CLI is available and authenticated, summarizes recent remote workflow runs. It
does not trigger workflows, download artifacts, change repository settings, or
modify GitHub state.

Environment overrides:
  CHECK_PAPER_GITHUB_ACTIONS_REPORT_DIR output dir, default: /private/tmp/check-paper-github-actions
  CHECK_PAPER_GITHUB_WORKFLOW_PATH      workflow file, default: .github/workflows/regression.yml
  CHECK_PAPER_GITHUB_WORKFLOW_SELECTOR  gh workflow selector, default: regression.yml
  CHECK_PAPER_GITHUB_REPO               optional gh --repo owner/name override
  CHECK_PAPER_GITHUB_RECENT_LIMIT       recent remote runs, default: 10
  CHECK_PAPER_GITHUB_MIN_SUCCESS_COUNT  required recent success count, default: 2
  CHECK_PAPER_GITHUB_FAIL_ON_HOLD       return non-zero on hold when set to 1
EOF
}

if [ "${1:-}" = "--help" ] || [ "${1:-}" = "-h" ]; then
  usage
  exit 0
fi

report_dir="${CHECK_PAPER_GITHUB_ACTIONS_REPORT_DIR:-/private/tmp/check-paper-github-actions}"
workflow_path="${CHECK_PAPER_GITHUB_WORKFLOW_PATH:-.github/workflows/regression.yml}"
workflow_selector="${CHECK_PAPER_GITHUB_WORKFLOW_SELECTOR:-regression.yml}"
repo="${CHECK_PAPER_GITHUB_REPO:-}"
recent_limit="${CHECK_PAPER_GITHUB_RECENT_LIMIT:-10}"
min_success_count="${CHECK_PAPER_GITHUB_MIN_SUCCESS_COUNT:-2}"
stamp="$(date -u +%Y-%m-%dT%H%M%SZ)"
report_path="$report_dir/check-paper GitHub Actions evidence $stamp.md"

mkdir -p "$report_dir"

failed_sections=""

record_failed_section() {
  title="$1"
  failed_sections="${failed_sections}${failed_sections:+, }$title"
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

append_probe_command() {
  title="$1"
  shift
  tmp="$report_dir/.github-actions-evidence-$stamp-$$.tmp"
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

check_workflow_signal() {
  label="$1"
  pattern="$2"
  if grep -Eq "$pattern" "$workflow_path"; then
    echo "- $label: yes"
  else
    echo "- $label: no"
    workflow_missing="${workflow_missing}${workflow_missing:+, }$label"
  fi
}

run_gh_run_list() {
  output_path="$1"
  if [ -n "$repo" ]; then
    gh run list \
      --repo "$repo" \
      --workflow "$workflow_selector" \
      --limit "$recent_limit" \
      --json databaseId,status,conclusion,event,headBranch,createdAt,updatedAt,displayTitle,url \
      > "$output_path"
  else
    gh run list \
      --workflow "$workflow_selector" \
      --limit "$recent_limit" \
      --json databaseId,status,conclusion,event,headBranch,createdAt,updatedAt,displayTitle,url \
      > "$output_path"
  fi
}

{
  echo "# check-paper GitHub Actions evidence $stamp"
  echo
  echo "- generated_at: $(date -u +%Y-%m-%dT%H:%M:%SZ)"
  echo "- report_dir: $report_dir"
  echo "- workflow_path: $workflow_path"
  echo "- workflow_selector: $workflow_selector"
  echo "- repo: ${repo:-<current repository>}"
  echo "- recent_limit: $recent_limit"
  echo "- min_success_count: $min_success_count"
  echo "- host: $(hostname 2>/dev/null || echo unknown)"
  echo "- uname: $(uname -a 2>/dev/null || echo unknown)"
  echo
  echo "This report is read-only. It verifies workflow shape and summarizes existing remote runs when GitHub CLI access is available."
} > "$report_path"

remote_url="<none>"
if git remote get-url origin >/dev/null 2>&1; then
  remote_url="$(git remote get-url origin 2>/dev/null || echo '<unknown>')"
else
  record_failed_section "Git remote origin"
fi

{
  echo
  echo "## Git Remote"
  echo
  echo "- origin: $remote_url"
} >> "$report_path"

workflow_missing=""
if [ -f "$workflow_path" ]; then
  {
    echo
    echo "## Local Workflow Definition"
    echo
    echo "- exit_status: 0"
    check_workflow_signal "push_trigger" '^[[:space:]]{2}push:'
    check_workflow_signal "pull_request_trigger" '^[[:space:]]{2}pull_request:'
    check_workflow_signal "weekly_schedule" '^[[:space:]]{2}schedule:'
    check_workflow_signal "manual_dispatch" '^[[:space:]]{2}workflow_dispatch:'
    check_workflow_signal "regression_gate" 'scripts/regression-check\.sh'
    check_workflow_signal "artifact_upload" 'actions/upload-artifact'
    echo
    echo '```yaml'
    sed -n '1,180p' "$workflow_path"
    echo '```'
  } >> "$report_path"
  if [ -n "$workflow_missing" ]; then
    record_failed_section "Local workflow definition"
  fi
else
  append_hold_section "Local Workflow Definition" "workflow file not found: $workflow_path"
fi

gh_status="unavailable"
if command -v gh >/dev/null 2>&1; then
  if append_probe_command "GitHub CLI auth status" gh auth status; then
    gh_status="authenticated"
  else
    gh_status="auth_failed"
    record_failed_section "GitHub CLI auth status"
  fi
else
  append_hold_section "GitHub CLI availability" "gh command not found; install GitHub CLI or set CHECK_PAPER_GITHUB_REPO and rerun after authentication."
fi

remote_success_count=0
remote_completed_count=0
remote_run_count=0
remote_runs_path="$report_dir/.github-actions-runs-$stamp-$$.json"
if [ "$gh_status" = "authenticated" ]; then
  set +e
  run_gh_run_list "$remote_runs_path" 2> "$report_dir/.github-actions-runs-$stamp-$$.err"
  gh_runs_status="$?"
  set -e
  {
    echo
    echo "## Remote Workflow Runs"
    echo
    echo "- exit_status: $gh_runs_status"
    echo "- runs_json: $remote_runs_path"
    echo
    echo '```json'
    if [ -f "$remote_runs_path" ]; then
      cat "$remote_runs_path"
    fi
    echo '```'
    if [ -s "$report_dir/.github-actions-runs-$stamp-$$.err" ]; then
      echo
      echo '```text'
      cat "$report_dir/.github-actions-runs-$stamp-$$.err"
      echo '```'
    fi
  } >> "$report_path"
  if [ "$gh_runs_status" -eq 0 ] && [ -f "$remote_runs_path" ]; then
    remote_success_count="$(grep -Eo '"conclusion"[[:space:]]*:[[:space:]]*"success"' "$remote_runs_path" | wc -l | tr -d ' ')"
    remote_completed_count="$(grep -Eo '"status"[[:space:]]*:[[:space:]]*"completed"' "$remote_runs_path" | wc -l | tr -d ' ')"
    remote_run_count="$(grep -Eo '"databaseId"[[:space:]]*:' "$remote_runs_path" | wc -l | tr -d ' ')"
  else
    record_failed_section "Remote workflow runs"
  fi
else
  append_hold_section "Remote Workflow Runs" "remote GitHub Actions run history was not checked because gh is not authenticated or unavailable."
fi
rm -f "$report_dir/.github-actions-runs-$stamp-$$.err"

{
  echo
  echo "## Remote Run Summary"
  echo
  echo "- gh_status: $gh_status"
  echo "- remote_run_count: $remote_run_count"
  echo "- remote_completed_count: $remote_completed_count"
  echo "- remote_success_count: $remote_success_count"
  echo "- min_success_count: $min_success_count"
} >> "$report_path"

if [ "$remote_success_count" -lt "$min_success_count" ]; then
  record_failed_section "Remote success count"
fi

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
    echo "- Keep this report with regression evidence and CI artifacts as the remote cadence record."
  else
    echo "- Enable or authenticate GitHub Actions observation, then rerun scripts/github-actions-evidence.sh."
    echo "- Confirm at least $min_success_count recent successful runs of $workflow_selector."
    echo "- Keep uploaded regression-reports artifacts with the evidence ledger."
  fi
} >> "$report_path"

echo "github actions evidence: $report_path"

if [ -n "$failed_sections" ] && [ "${CHECK_PAPER_GITHUB_FAIL_ON_HOLD:-0}" = "1" ]; then
  exit 1
fi
