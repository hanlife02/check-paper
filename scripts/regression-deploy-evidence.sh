#!/usr/bin/env sh
set -eu

regression_report_dir="${CHECK_PAPER_REGRESSION_REPORT_DIR:-/private/tmp/check-paper-eval-gate}"
report_dir="${CHECK_PAPER_REGRESSION_DEPLOY_REPORT_DIR:-/private/tmp/check-paper-regression-deploy}"
min_pass_count="${CHECK_PAPER_REGRESSION_MIN_PASS_COUNT:-2}"
max_evidence_age_days="${CHECK_PAPER_REGRESSION_MAX_EVIDENCE_AGE_DAYS:-14}"
stamp="$(date -u +%Y-%m-%dT%H%M%SZ)"
report_path="$report_dir/check-paper regression deploy evidence $stamp.md"

mkdir -p "$report_dir"

failed_sections=""
schedule_signals=""

record_failed_section() {
  title="$1"
  failed_sections="${failed_sections}${failed_sections:+, }$title"
}

record_schedule_signal() {
  title="$1"
  schedule_signals="${schedule_signals}${schedule_signals:+, }$title"
}

append_command() {
  title="$1"
  shift
  tmp="$report_dir/.regression-deploy-$stamp.tmp"
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

append_skipped() {
  title="$1"
  reason="$2"
  {
    echo
    echo "## $title"
    echo
    echo "- exit_status: skipped"
    echo
    echo '```text'
    echo "$reason"
    echo '```'
  } >> "$report_path"
}

append_evidence_artifacts() {
  artifact_list="$report_dir/.regression-artifacts-$stamp.tmp"
  : > "$artifact_list"
  if [ -d "$regression_report_dir" ]; then
    find "$regression_report_dir" -maxdepth 1 -type f -name 'check-paper regression evidence *.md' -print 2>/dev/null | sort > "$artifact_list"
  fi

  artifact_count=0
  pass_count=0
  recent_pass_count=0
  latest_evidence="<none>"
  while IFS= read -r path; do
    [ -n "$path" ] || continue
    artifact_count=$((artifact_count + 1))
    latest_evidence="$path"
    if grep -q '^- result: pass$' "$path"; then
      pass_count=$((pass_count + 1))
      if find "$path" -maxdepth 0 -mtime "-$max_evidence_age_days" -print 2>/dev/null | grep -q .; then
        recent_pass_count=$((recent_pass_count + 1))
      fi
    fi
  done < "$artifact_list"

  {
    echo
    echo "## Regression Evidence Artifacts"
    echo
    echo "- regression_report_dir: $regression_report_dir"
    echo "- artifact_count: $artifact_count"
    echo "- pass_count: $pass_count"
    echo "- recent_pass_count: $recent_pass_count"
    echo "- min_pass_count: $min_pass_count"
    echo "- max_evidence_age_days: $max_evidence_age_days"
    echo "- latest_evidence: $latest_evidence"
    echo
    echo '```text'
    if [ "$artifact_count" -eq 0 ]; then
      echo "<none>"
    else
      tail -n 10 "$artifact_list" | while IFS= read -r path; do
        result="$(sed -n 's/^- result: //p' "$path" | head -n 1)"
        exit_status="$(sed -n 's/^- exit_status: //p' "$path" | head -n 1)"
        finished_at="$(sed -n 's/^- finished_at: //p' "$path" | head -n 1)"
        failed_step="$(sed -n 's/^- failed_step: //p' "$path" | head -n 1)"
        printf '%s | result=%s exit_status=%s finished_at=%s failed_step=%s\n' \
          "$path" \
          "${result:-unknown}" \
          "${exit_status:-unknown}" \
          "${finished_at:-unknown}" \
          "${failed_step:-unknown}"
      done
    fi
    echo '```'
  } >> "$report_path"

  rm -f "$artifact_list"
  if [ "$recent_pass_count" -lt "$min_pass_count" ]; then
    record_failed_section "Regression evidence artifacts"
  fi
}

append_scheduler_state() {
  if command -v launchctl >/dev/null 2>&1; then
    if append_command "launchd regression schedule state" launchctl print "gui/$(id -u)/com.check-paper.regression"; then
      record_schedule_signal "launchd regression schedule"
    fi
  else
    append_skipped "launchd regression schedule state" "launchctl not found on this host"
  fi

  if command -v systemctl >/dev/null 2>&1; then
    if append_command "systemd regression timer state" systemctl --user status check-paper-regression.timer; then
      record_schedule_signal "systemd regression timer"
    fi
    append_command "systemd user timers" systemctl --user list-timers --all || true
  else
    append_skipped "systemd regression timer state" "systemctl not found on this host"
  fi

  if command -v crontab >/dev/null 2>&1; then
    tmp="$report_dir/.regression-crontab-$stamp.tmp"
    set +e
    crontab -l > "$tmp" 2>&1
    status="$?"
    set -e
    {
      echo
      echo "## cron regression schedule state"
      echo
      echo "- exit_status: $status"
      echo
      echo '```text'
      cat "$tmp"
      echo '```'
    } >> "$report_path"
    if [ "$status" -eq 0 ] && grep -Eq 'scripts/regression-check\.sh|check-paper-regression' "$tmp"; then
      record_schedule_signal "cron regression schedule"
    fi
    rm -f "$tmp"
  else
    append_skipped "cron regression schedule state" "crontab not found on this host"
  fi

  {
    echo
    echo "## Scheduler Result"
    echo
    if [ -z "$schedule_signals" ]; then
      echo "- schedule_signals: <none>"
    else
      echo "- schedule_signals: $schedule_signals"
    fi
  } >> "$report_path"

  if [ -z "$schedule_signals" ]; then
    record_failed_section "Regression scheduler state"
  fi
}

{
  echo "# check-paper regression deploy evidence $stamp"
  echo
  echo "- generated_at: $(date -u +%Y-%m-%dT%H:%M:%SZ)"
  echo "- regression_report_dir: $regression_report_dir"
  echo "- report_dir: $report_dir"
  echo "- min_pass_count: $min_pass_count"
  echo "- max_evidence_age_days: $max_evidence_age_days"
  echo "- host: $(hostname 2>/dev/null || echo unknown)"
  echo "- uname: $(uname -a 2>/dev/null || echo unknown)"
  echo
  echo "This report is read-only. It does not install launchd/systemd/cron entries and does not run scripts/regression-check.sh."
} > "$report_path"

append_evidence_artifacts
append_scheduler_state

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
    echo "- Keep this report with the scheduled regression run records and latest regression evidence manifests."
  else
    echo "- Install or enable one regression scheduler with scripts/regression-schedule-template.sh launchd|systemd|cron."
    echo "- Let scripts/regression-check.sh run until at least $min_pass_count recent pass evidence files exist, then rerun this report on the target machine."
  fi
} >> "$report_path"

echo "regression deploy evidence: $report_path"

if [ -n "$failed_sections" ] && [ "${CHECK_PAPER_REGRESSION_DEPLOY_FAIL_ON_HOLD:-0}" = "1" ]; then
  exit 1
fi
