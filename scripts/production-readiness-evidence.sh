#!/usr/bin/env sh
set -eu

report_dir="${CHECK_PAPER_PRODUCTION_READINESS_REPORT_DIR:-/private/tmp/check-paper-production-readiness}"
stamp="$(date -u +%Y-%m-%dT%H%M%SZ)"
report_path="$report_dir/check-paper production readiness $stamp.md"

v2_report_dir="${CHECK_PAPER_V2_READINESS_REPORT_DIR:-$report_dir/v2-default-readiness}"
regression_report_dir="${CHECK_PAPER_REGRESSION_DEPLOY_REPORT_DIR:-$report_dir/regression-deploy}"
telegram_report_dir="${CHECK_PAPER_TG_DEPLOY_REPORT_DIR:-$report_dir/telegram-deploy}"

mkdir -p "$report_dir" "$v2_report_dir" "$regression_report_dir" "$telegram_report_dir"

failed_sections=""

record_failed_section() {
  title="$1"
  failed_sections="${failed_sections}${failed_sections:+, }$title"
}

append_section_result() {
  title="$1"
  command_status="$2"
  output="$3"
  evidence_path="$4"
  evidence_result="<unknown>"
  if [ -n "$evidence_path" ] && [ -f "$evidence_path" ]; then
    evidence_result="$(sed -n 's/^- result: //p' "$evidence_path" | tail -n 1)"
  fi
  if [ -z "$evidence_result" ]; then
    evidence_result="<unknown>"
  fi

  {
    echo
    echo "## $title"
    echo
    echo "- command_exit_status: $command_status"
    echo "- evidence_path: ${evidence_path:-<unknown>}"
    echo "- evidence_result: $evidence_result"
    echo
    echo '```text'
    printf '%s\n' "$output"
    echo '```'
  } >> "$report_path"

  if [ "$command_status" -ne 0 ] || [ "$evidence_result" != "ready" ]; then
    record_failed_section "$title"
  fi
}

run_evidence_script() {
  title="$1"
  marker="$2"
  shift 2
  tmp="$report_dir/.production-readiness-$stamp.tmp"
  set +e
  "$@" > "$tmp" 2>&1
  status="$?"
  set -e
  output="$(cat "$tmp")"
  rm -f "$tmp"
  evidence_path="$(printf '%s\n' "$output" | sed -n "s/^$marker: //p" | tail -n 1)"
  append_section_result "$title" "$status" "$output" "$evidence_path"
}

{
  echo "# check-paper production readiness $stamp"
  echo
  echo "- generated_at: $(date -u +%Y-%m-%dT%H:%M:%SZ)"
  echo "- report_dir: $report_dir"
  echo "- v2_report_dir: $v2_report_dir"
  echo "- regression_report_dir: $regression_report_dir"
  echo "- telegram_report_dir: $telegram_report_dir"
  echo "- host: $(hostname 2>/dev/null || echo unknown)"
  echo "- uname: $(uname -a 2>/dev/null || echo unknown)"
  echo
  echo "This report is read-only. It runs existing evidence scripts but does not edit config, install services, bootstrap schedulers, rotate logs, or send Telegram notifications."
} > "$report_path"

run_evidence_script \
  "V2 default readiness" \
  "V2 default readiness evidence" \
  env CHECK_PAPER_V2_READINESS_REPORT_DIR="$v2_report_dir" \
  scripts/v2-default-readiness.sh "${1:-${CHECK_PAPER_V2_AUTHOR:-Ruqiang ZOU}}"

run_evidence_script \
  "Regression deploy readiness" \
  "regression deploy evidence" \
  env CHECK_PAPER_REGRESSION_DEPLOY_REPORT_DIR="$regression_report_dir" \
  scripts/regression-deploy-evidence.sh

run_evidence_script \
  "Telegram deploy readiness" \
  "telegram deploy evidence" \
  env CHECK_PAPER_TG_DEPLOY_REPORT_DIR="$telegram_report_dir" \
  scripts/telegram-deploy-evidence.sh

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
    echo "- Keep this report together with the child evidence reports as the target-machine readiness record."
	  else
	    echo "- Open each child evidence_path above and resolve its hold sections on the target machine."
	    echo "- Use scripts/production-bootstrap-plan.sh launchd|systemd|cron to generate exact target-machine bootstrap templates and apply commands."
	    echo "- Rerun scripts/production-readiness-evidence.sh after profile signoff, regression scheduler, Telegram service, health timer, and logrotate schedule are all in place."
	  fi
	} >> "$report_path"

echo "production readiness evidence: $report_path"

if [ -n "$failed_sections" ] && [ "${CHECK_PAPER_PRODUCTION_READINESS_FAIL_ON_HOLD:-0}" = "1" ]; then
  exit 1
fi
