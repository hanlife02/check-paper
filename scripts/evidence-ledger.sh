#!/usr/bin/env sh
set -eu

usage() {
  cat <<'EOF'
Usage: scripts/evidence-ledger.sh

Generate a read-only Markdown ledger for existing check-paper evidence reports.

This script scans report directories and summarizes existing Markdown evidence.
It does not run regression, eval, readiness checks, deploy checks, schedulers, or
Telegram commands.

Environment overrides:
  CHECK_PAPER_EVIDENCE_LEDGER_REPORT_DIR  output dir, default: /private/tmp/check-paper-evidence-ledger
  CHECK_PAPER_EVIDENCE_LEDGER_SCAN_DIRS   colon-separated scan dirs; overrides defaults
  CHECK_PAPER_EVIDENCE_LEDGER_RECENT      recent files per section, default: 10

Default scan dirs:
  CHECK_PAPER_REGRESSION_REPORT_DIR       default: /private/tmp/check-paper-eval-gate
  CHECK_PAPER_V2_READINESS_REPORT_DIR     default: /private/tmp/check-paper-v2-readiness
  CHECK_PAPER_REGRESSION_DEPLOY_REPORT_DIR default: /private/tmp/check-paper-regression-deploy
  CHECK_PAPER_TG_DEPLOY_REPORT_DIR        default: /private/tmp/check-paper-telegram-deploy
  CHECK_PAPER_PRODUCTION_READINESS_REPORT_DIR default: /private/tmp/check-paper-production-readiness
  CHECK_PAPER_PRODUCTION_BOOTSTRAP_REPORT_DIR default: /private/tmp/check-paper-production-bootstrap
  CHECK_PAPER_V2_SWITCH_PLAN_REPORT_DIR default: /private/tmp/check-paper-v2-switch-plan
  CHECK_PAPER_GITHUB_ACTIONS_REPORT_DIR default: /private/tmp/check-paper-github-actions
EOF
}

if [ "${1:-}" = "--help" ] || [ "${1:-}" = "-h" ]; then
  usage
  exit 0
fi

report_dir="${CHECK_PAPER_EVIDENCE_LEDGER_REPORT_DIR:-/private/tmp/check-paper-evidence-ledger}"
recent_limit="${CHECK_PAPER_EVIDENCE_LEDGER_RECENT:-10}"
stamp="$(date -u +%Y-%m-%dT%H%M%SZ)"
report_path="$report_dir/check-paper evidence ledger $stamp.md"

regression_report_dir="${CHECK_PAPER_REGRESSION_REPORT_DIR:-/private/tmp/check-paper-eval-gate}"
v2_readiness_report_dir="${CHECK_PAPER_V2_READINESS_REPORT_DIR:-/private/tmp/check-paper-v2-readiness}"
regression_deploy_report_dir="${CHECK_PAPER_REGRESSION_DEPLOY_REPORT_DIR:-/private/tmp/check-paper-regression-deploy}"
telegram_deploy_report_dir="${CHECK_PAPER_TG_DEPLOY_REPORT_DIR:-/private/tmp/check-paper-telegram-deploy}"
production_readiness_report_dir="${CHECK_PAPER_PRODUCTION_READINESS_REPORT_DIR:-/private/tmp/check-paper-production-readiness}"
production_bootstrap_report_dir="${CHECK_PAPER_PRODUCTION_BOOTSTRAP_REPORT_DIR:-/private/tmp/check-paper-production-bootstrap}"
v2_switch_plan_report_dir="${CHECK_PAPER_V2_SWITCH_PLAN_REPORT_DIR:-/private/tmp/check-paper-v2-switch-plan}"
github_actions_report_dir="${CHECK_PAPER_GITHUB_ACTIONS_REPORT_DIR:-/private/tmp/check-paper-github-actions}"

mkdir -p "$report_dir"

if [ -n "${CHECK_PAPER_EVIDENCE_LEDGER_SCAN_DIRS:-}" ]; then
  scan_dirs="$(printf '%s' "$CHECK_PAPER_EVIDENCE_LEDGER_SCAN_DIRS" | tr ':' '\n')"
else
  scan_dirs="$(cat <<EOF
$regression_report_dir
$v2_readiness_report_dir
$regression_deploy_report_dir
$telegram_deploy_report_dir
$production_readiness_report_dir
$production_bootstrap_report_dir
$v2_switch_plan_report_dir
$github_actions_report_dir
EOF
)"
fi

artifact_result() {
  path="$1"
  result="$(sed -n 's/^- result: //p' "$path" | tail -n 1)"
  if [ -z "$result" ]; then
    result="$(sed -n 's/^- default_switch_recommendation: //p' "$path" | tail -n 1)"
  fi
  if [ -z "$result" ]; then
    metric_gate="$(sed -n 's/^- metric_gate_pass: //p' "$path" | tail -n 1)"
    if [ "$metric_gate" = "yes" ]; then
      result="metric_gate_pass"
    elif [ "$metric_gate" = "no" ]; then
      result="metric_gate_hold"
    fi
  fi
  if [ -z "$result" ] && grep -q '^- generated_at: ' "$path"; then
    result="generated"
  fi
  if [ -z "$result" ]; then
    result="<unknown>"
  fi
  printf '%s' "$result"
}

artifact_mtime() {
  path="$1"
  if stat -f '%m' "$path" 2>/dev/null; then
    return
  fi
  if stat -c '%Y' "$path" 2>/dev/null; then
    return
  fi
  echo 0
}

artifact_time() {
  path="$1"
  value="$(sed -n 's/^- generated_at: //p' "$path" | head -n 1)"
  if [ -z "$value" ]; then
    value="$(sed -n 's/^- finished_at: //p' "$path" | head -n 1)"
  fi
  if [ -z "$value" ]; then
    value="$(sed -n 's/^- started_at: //p' "$path" | head -n 1)"
  fi
  if [ -z "$value" ]; then
    value="<unknown>"
  fi
  printf '%s' "$value"
}

artifact_failed_sections() {
  path="$1"
  value="$(sed -n 's/^- failed_sections: //p' "$path" | tail -n 1)"
  if [ -z "$value" ]; then
    failed_step="$(sed -n 's/^- failed_step: //p' "$path" | tail -n 1)"
    if [ "$failed_step" = "<none>" ]; then
      value="<none>"
    elif [ -n "$failed_step" ]; then
      value="failed_step=$failed_step"
    fi
  fi
  if [ -z "$value" ]; then
    value="<unknown>"
  fi
  printf '%s' "$value"
}

artifact_metric_gate() {
  path="$1"
  value="$(sed -n 's/^- metric_gate_pass: //p' "$path" | tail -n 1)"
  if [ -z "$value" ]; then
    value="-"
  fi
  printf '%s' "$value"
}

append_category() {
  title="$1"
  pattern="$2"
  file_list="$report_dir/.evidence-ledger-$stamp-$$.tmp"
  mtime_list="$report_dir/.evidence-ledger-mtime-$stamp-$$.tmp"
  : > "$file_list"
  : > "$mtime_list"

  printf '%s\n' "$scan_dirs" | while IFS= read -r dir; do
    [ -n "$dir" ] || continue
    if [ -d "$dir" ]; then
      find "$dir" -maxdepth 3 -type f -name "$pattern" -print 2>/dev/null
    fi
  done | sort -u > "$file_list"

  while IFS= read -r path; do
    [ -n "$path" ] || continue
    printf '%s\t%s\n' "$(artifact_mtime "$path")" "$path"
  done < "$file_list" | sort -n | cut -f 2- > "$mtime_list"
  mv "$mtime_list" "$file_list"

  count="$(wc -l < "$file_list" | tr -d ' ')"
  pass_count=0
  ready_count=0
  eligible_count=0
  hold_count=0
  generated_count=0
  failed_count=0
  unknown_count=0
  latest="<none>"
  latest_result="<none>"
  latest_time="<none>"
  latest_failed_sections="<none>"

  while IFS= read -r path; do
    [ -n "$path" ] || continue
    result="$(artifact_result "$path")"
    latest="$path"
    latest_result="$result"
    latest_time="$(artifact_time "$path")"
    latest_failed_sections="$(artifact_failed_sections "$path")"
    case "$result" in
      pass | metric_gate_pass)
        pass_count=$((pass_count + 1))
        ;;
      ready)
        ready_count=$((ready_count + 1))
        ;;
      eligible_for_manual_review)
        eligible_count=$((eligible_count + 1))
        ;;
      hold | metric_gate_hold)
        hold_count=$((hold_count + 1))
        ;;
      generated)
        generated_count=$((generated_count + 1))
        ;;
      fail | failed)
        failed_count=$((failed_count + 1))
        ;;
      *)
        unknown_count=$((unknown_count + 1))
        ;;
    esac
  done < "$file_list"

  {
    echo "| $title | $count | $pass_count | $ready_count | $eligible_count | $hold_count | $generated_count | $failed_count | $unknown_count | \`$latest_result\` | \`$latest\` |"
  } >> "$summary_tmp"

  {
    echo
    echo "## $title"
    echo
    echo "- pattern: \`$pattern\`"
    echo "- artifact_count: $count"
    echo "- latest_artifact: \`$latest\`"
    echo "- latest_result: \`$latest_result\`"
    echo "- latest_time: $latest_time"
    echo "- latest_failed_sections: $latest_failed_sections"
    echo
    echo "| artifact | result | time | failed_sections | metric_gate_pass |"
    echo "| --- | --- | --- | --- | --- |"
    if [ "$count" -eq 0 ]; then
      echo "| <none> | - | - | - | - |"
    else
      tail -n "$recent_limit" "$file_list" | while IFS= read -r path; do
        [ -n "$path" ] || continue
        result="$(artifact_result "$path")"
        time_value="$(artifact_time "$path")"
        failed_sections="$(artifact_failed_sections "$path")"
        metric_gate="$(artifact_metric_gate "$path")"
        echo "| \`$path\` | \`$result\` | $time_value | $failed_sections | $metric_gate |"
      done
    fi
  } >> "$detail_tmp"

  rm -f "$file_list" "$mtime_list"
}

summary_tmp="$report_dir/.evidence-ledger-summary-$stamp-$$.tmp"
detail_tmp="$report_dir/.evidence-ledger-detail-$stamp-$$.tmp"
: > "$summary_tmp"
: > "$detail_tmp"

{
  echo "# check-paper evidence ledger $stamp"
  echo
  echo "- generated_at: $(date -u +%Y-%m-%dT%H:%M:%SZ)"
  echo "- report_dir: $report_dir"
  echo "- recent_limit: $recent_limit"
  echo "- host: $(hostname 2>/dev/null || echo unknown)"
  echo "- uname: $(uname -a 2>/dev/null || echo unknown)"
  echo
  echo "This ledger is read-only. It indexes existing Markdown reports and does not prove production readiness by itself."
  echo
  echo "## Scan Directories"
  echo
  printf '%s\n' "$scan_dirs" | while IFS= read -r dir; do
    [ -n "$dir" ] || continue
    if [ -d "$dir" ]; then
      echo "- \`$dir\`"
    else
      echo "- \`$dir\` (missing)"
    fi
  done
  echo
  echo "## Summary"
  echo
  echo "| category | artifacts | pass | ready | eligible | hold | generated | failed | unknown | latest_result | latest_artifact |"
  echo "| --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: | --- | --- |"
} > "$report_path"

append_category "Regression Evidence" "check-paper regression evidence *.md"
append_category "V1/V2 Eval Gate" "check-paper V1 V2 eval gate *.md"
append_category "QA Telegram Trend" "check-paper QA Telegram trend *.md"
append_category "GitHub Actions Evidence" "check-paper GitHub Actions evidence *.md"
append_category "V2 Default Readiness" "check-paper V2 default readiness *.md"
append_category "V2 Default Switch Plan" "check-paper V2 default switch plan *.md"
append_category "Regression Deploy Evidence" "check-paper regression deploy evidence *.md"
append_category "Telegram Deploy Evidence" "check-paper Telegram deploy evidence *.md"
append_category "Production Bootstrap Plan" "check-paper production bootstrap plan *.md"
append_category "Production Readiness" "check-paper production readiness *.md"

cat "$summary_tmp" >> "$report_path"
cat "$detail_tmp" >> "$report_path"

{
  echo
  echo "## Interpretation"
  echo
  echo "- For default V2 switch, the relevant chain is V1/V2 eval gate pass or eligible, V2 default readiness ready, and human profile diff signoff."
  echo "- For production deployment, the relevant chain is production bootstrap applied on the target machine, regression deploy ready, Telegram deploy ready, and production readiness ready."
  echo "- Hold entries remain actionable blockers, not failures of this ledger."
} >> "$report_path"

rm -f "$summary_tmp" "$detail_tmp"

echo "evidence ledger: $report_path"
