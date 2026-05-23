#!/usr/bin/env sh
set -eu

ppc_bin="${CHECK_PAPER_PPC_BIN:-target/debug/ppc}"
report_dir="${CHECK_PAPER_TG_DEPLOY_REPORT_DIR:-/private/tmp/check-paper-telegram-deploy}"
days="${CHECK_PAPER_TG_DEPLOY_TREND_DAYS:-14}"
service_kinds="${CHECK_PAPER_TG_SERVICE_KINDS:-launchd launchd-health logrotate}"
home="${HOME:-$PWD}"
logrotate_config="${CHECK_PAPER_TG_LOGROTATE_CONFIG:-$home/.config/logrotate.d/check-paper-telegram}"
logrotate_status="${CHECK_PAPER_TG_LOGROTATE_STATUS:-$home/.local/state/check-paper/logrotate.status}"
stamp="$(date -u +%Y-%m-%dT%H%M%SZ)"
report_path="$report_dir/check-paper Telegram deploy evidence $stamp.md"

mkdir -p "$report_dir"

if [ ! -x "$ppc_bin" ]; then
  cargo build --bin ppc
fi

failed_sections=""
logrotate_schedule_signals=""
health_schedule_signals=""

record_failed_section() {
  title="$1"
  failed_sections="${failed_sections}${failed_sections:+, }$title"
}

record_logrotate_schedule_signal() {
  title="$1"
  logrotate_schedule_signals="${logrotate_schedule_signals}${logrotate_schedule_signals:+, }$title"
}

record_health_schedule_signal() {
  title="$1"
  health_schedule_signals="${health_schedule_signals}${health_schedule_signals:+, }$title"
}

append_command() {
  title="$1"
  shift
  tmp="$report_dir/.telegram-evidence-$stamp.tmp"
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

append_probe_command() {
  title="$1"
  shift
  tmp="$report_dir/.telegram-evidence-$stamp.tmp"
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
  echo "# check-paper Telegram deploy evidence $stamp"
  echo
  echo "- generated_at: $(date -u +%Y-%m-%dT%H:%M:%SZ)"
  echo "- ppc_bin: $ppc_bin"
  echo "- report_dir: $report_dir"
  echo "- service_kinds: $service_kinds"
  echo "- trend_days: $days"
  echo "- logrotate_config: $logrotate_config"
  echo "- logrotate_status: $logrotate_status"
  echo "- host: $(hostname 2>/dev/null || echo unknown)"
  echo "- uname: $(uname -a 2>/dev/null || echo unknown)"
  echo
  echo "This report is read-only. It does not bootstrap launchd, enable systemd, perform log rotation, or send Telegram notifications."
} > "$report_path"

append_command "Telegram config/API status" "$ppc_bin" tg status

for kind in $service_kinds; do
  append_command "Telegram service check: $kind" "$ppc_bin" tg service-check --kind "$kind"
done

append_command "Telegram polling health" "$ppc_bin" tg health --strict
append_command "Telegram delivery summary" "$ppc_bin" logs telegram --summary
append_command "Telegram delivery trend" "$ppc_bin" logs telegram --trend --days "$days"

if command -v logrotate >/dev/null 2>&1 && [ -f "$logrotate_config" ]; then
  append_command "Telegram logrotate dry run" logrotate -d -s "$logrotate_status" "$logrotate_config"
else
  append_hold_section \
    "Telegram logrotate dry run" \
    "missing logrotate command or config file; install logrotate and run ppc tg service-install --kind logrotate before relying on rotation"
fi

if command -v launchctl >/dev/null 2>&1; then
  append_command "launchd service state" launchctl print "gui/$(id -u)/com.check-paper.telegram"
  if append_probe_command "launchd health timer state" launchctl print "gui/$(id -u)/com.check-paper.telegram-health"; then
    record_health_schedule_signal "launchd health timer"
  else
    record_failed_section "launchd health timer state"
  fi
  if append_probe_command "launchd logrotate timer state" launchctl print "gui/$(id -u)/com.check-paper.telegram-logrotate"; then
    record_logrotate_schedule_signal "launchd logrotate timer"
  fi
fi

if command -v systemctl >/dev/null 2>&1; then
  append_command "systemd user service state" systemctl --user status check-paper-telegram.service
  if append_probe_command "systemd health timer state" systemctl --user status check-paper-telegram-health.timer; then
    record_health_schedule_signal "systemd health timer"
  fi
  if append_probe_command "systemd logrotate timer state" systemctl --user status check-paper-telegram-logrotate.timer; then
    record_logrotate_schedule_signal "systemd logrotate timer"
  fi
  append_command "systemd user timers" systemctl --user list-timers --all
fi

if command -v crontab >/dev/null 2>&1; then
  tmp="$report_dir/.telegram-evidence-crontab-$stamp.tmp"
  set +e
  crontab -l > "$tmp" 2>&1
  status="$?"
  set -e
  {
    echo
    echo "## cron schedule state"
    echo
    echo "- exit_status: $status"
    echo
    echo '```text'
    cat "$tmp"
    echo '```'
  } >> "$report_path"
  if [ "$status" -eq 0 ] && grep -Eq 'check-paper\.telegram-health|telegram-health|tg health --strict --notify' "$tmp"; then
    record_health_schedule_signal "cron health schedule"
  fi
  if [ "$status" -eq 0 ] && grep -Eq 'check-paper\.telegram-logrotate|telegram-logrotate|logrotate .*check-paper-telegram' "$tmp"; then
    record_logrotate_schedule_signal "cron logrotate schedule"
  fi
  rm -f "$tmp"
fi

{
  echo
  echo "## Telegram health schedule result"
  echo
  if [ -z "$health_schedule_signals" ]; then
    echo "- schedule_signals: <none>"
  else
    echo "- schedule_signals: $health_schedule_signals"
  fi
  echo
  echo "## Telegram logrotate schedule result"
  echo
  if [ -z "$logrotate_schedule_signals" ]; then
    echo "- schedule_signals: <none>"
  else
    echo "- schedule_signals: $logrotate_schedule_signals"
  fi
} >> "$report_path"

if [ -z "$health_schedule_signals" ]; then
  record_failed_section "Telegram health schedule"
fi

if [ -z "$logrotate_schedule_signals" ]; then
  record_failed_section "Telegram logrotate schedule"
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
    echo "- Keep this report with the service install, health timer, and logrotate deployment notes."
  else
    echo "- Resolve failed sections, then rerun scripts/telegram-deploy-evidence.sh on the target machine."
    echo "- Use ppc tg service-check next_steps for exact bootstrap/status/logrotate commands."
    echo "- Use scripts/telegram-health-schedule-template.sh launchd|systemd|cron to schedule Telegram health alerts."
    echo "- Use scripts/telegram-logrotate-schedule-template.sh launchd|systemd|cron to schedule Telegram log rotation."
  fi
} >> "$report_path"

echo "telegram deploy evidence: $report_path"

if [ -n "$failed_sections" ] && [ "${CHECK_PAPER_TG_DEPLOY_FAIL_ON_HOLD:-0}" = "1" ]; then
  exit 1
fi
