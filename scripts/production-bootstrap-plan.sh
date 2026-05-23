#!/usr/bin/env sh
set -eu

usage() {
  cat <<'EOF'
Usage: scripts/production-bootstrap-plan.sh launchd|systemd|cron [AUTHOR]

Generate a target-machine production bootstrap plan and template bundle.

This script is read-only: it writes plan/template artifacts under the report
directory, but it does not install services, edit crontab, bootstrap launchd,
enable systemd timers, rotate logs, send Telegram notifications, or change
CHECK_PAPER_QA_PROFILE_VERSION.

Environment overrides:
  CHECK_PAPER_PPC_BIN                         ppc binary, default: target/debug/ppc
  CHECK_PAPER_WORKDIR                         repository workdir, default: current dir
  CHECK_PAPER_PRODUCTION_BOOTSTRAP_REPORT_DIR plan directory, default: /private/tmp/check-paper-production-bootstrap
EOF
}

kind="${1:-}"
if [ -z "$kind" ] || [ "$kind" = "--help" ] || [ "$kind" = "-h" ]; then
  usage
  exit 0
fi

case "$kind" in
  launchd | systemd | cron) ;;
  *)
    echo "unknown bootstrap kind: $kind" >&2
    usage >&2
    exit 2
    ;;
esac

author="${2:-${CHECK_PAPER_V2_AUTHOR:-Ruqiang ZOU}}"
ppc_bin="${CHECK_PAPER_PPC_BIN:-target/debug/ppc}"
workdir="${CHECK_PAPER_WORKDIR:-$(pwd)}"
report_dir="${CHECK_PAPER_PRODUCTION_BOOTSTRAP_REPORT_DIR:-/private/tmp/check-paper-production-bootstrap}"
stamp="$(date -u +%Y-%m-%dT%H%M%SZ)"
template_dir="$report_dir/templates-$kind-$stamp"
plan_path="$report_dir/check-paper production bootstrap plan $kind $stamp.md"
home="${HOME:-$PWD}"

mkdir -p "$report_dir" "$template_dir"

if [ ! -x "$ppc_bin" ]; then
  cargo build --bin ppc
fi

failed_sections=""

record_failed_section() {
  title="$1"
  failed_sections="${failed_sections}${failed_sections:+, }$title"
}

shell_quote() {
  printf "'%s'" "$(printf '%s' "$1" | sed "s/'/'\\\\''/g")"
}

append_template() {
  title="$1"
  output_path="$2"
  shift 2
  tmp="$report_dir/.production-bootstrap-$kind-$stamp-$$.tmp"
  set +e
  "$@" > "$output_path" 2> "$tmp"
  status="$?"
  set -e
  {
    echo
    echo "## $title"
    echo
    echo "- exit_status: $status"
    echo "- output_path: $output_path"
    echo
    echo '```text'
    cat "$tmp"
    echo '```'
  } >> "$plan_path"
  rm -f "$tmp"
  if [ "$status" -ne 0 ]; then
    record_failed_section "$title"
  fi
}

{
  echo "# check-paper production bootstrap plan $kind $stamp"
  echo
  echo "- generated_at: $(date -u +%Y-%m-%dT%H:%M:%SZ)"
  echo "- author: $author"
  echo "- kind: $kind"
  echo "- ppc_bin: $ppc_bin"
  echo "- workdir: $workdir"
  echo "- report_dir: $report_dir"
  echo "- template_dir: $template_dir"
  echo "- host: $(hostname 2>/dev/null || echo unknown)"
  echo "- uname: $(uname -a 2>/dev/null || echo unknown)"
  echo
  echo "This plan is read-only. Review the generated templates, apply them on the target machine, then run production readiness evidence."
} > "$plan_path"

case "$kind" in
  launchd)
    append_template "Telegram launchd service template" "$template_dir/com.check-paper.telegram.plist" \
      "$ppc_bin" tg service-template --kind launchd
    append_template "Telegram launchd health timer template" "$template_dir/com.check-paper.telegram-health.plist" \
      "$ppc_bin" tg service-template --kind launchd-health
    append_template "Telegram logrotate config template" "$template_dir/check-paper-telegram.logrotate" \
      "$ppc_bin" tg service-template --kind logrotate
    append_template "Regression launchd schedule template" "$template_dir/com.check-paper.regression.plist" \
      env CHECK_PAPER_WORKDIR="$workdir" scripts/regression-schedule-template.sh launchd
    append_template "Telegram logrotate launchd schedule template" "$template_dir/com.check-paper.telegram-logrotate.plist" \
      scripts/telegram-logrotate-schedule-template.sh launchd
    ;;
  systemd)
    append_template "Telegram systemd service template" "$template_dir/check-paper-telegram.service" \
      "$ppc_bin" tg service-template --kind systemd
    append_template "Telegram logrotate config template" "$template_dir/check-paper-telegram.logrotate" \
      "$ppc_bin" tg service-template --kind logrotate
    append_template "Regression systemd schedule template" "$template_dir/check-paper-regression.systemd.txt" \
      env CHECK_PAPER_WORKDIR="$workdir" scripts/regression-schedule-template.sh systemd
    append_template "Telegram logrotate systemd schedule template" "$template_dir/check-paper-telegram-logrotate.systemd.txt" \
      scripts/telegram-logrotate-schedule-template.sh systemd
    append_template "Telegram health systemd schedule template" "$template_dir/check-paper-telegram-health.systemd.txt" \
      env CHECK_PAPER_WORKDIR="$workdir" CHECK_PAPER_PPC_BIN="$ppc_bin" scripts/telegram-health-schedule-template.sh systemd
    ;;
  cron)
    append_template "Telegram logrotate config template" "$template_dir/check-paper-telegram.logrotate" \
      "$ppc_bin" tg service-template --kind logrotate
    append_template "Regression cron schedule template" "$template_dir/check-paper-regression.cron" \
      env CHECK_PAPER_WORKDIR="$workdir" scripts/regression-schedule-template.sh cron
    append_template "Telegram logrotate cron schedule template" "$template_dir/check-paper-telegram-logrotate.cron" \
      scripts/telegram-logrotate-schedule-template.sh cron
    append_template "Telegram health cron schedule template" "$template_dir/check-paper-telegram-health.cron" \
      env CHECK_PAPER_WORKDIR="$workdir" CHECK_PAPER_PPC_BIN="$ppc_bin" scripts/telegram-health-schedule-template.sh cron
    ;;
esac

{
  echo
  echo "## Apply Checklist"
  echo
  echo "Run these commands on the target machine after reviewing the generated templates."
  echo
  echo '```sh'
  echo "cd $(shell_quote "$workdir")"
  case "$kind" in
    launchd)
      launch_agents="$home/Library/LaunchAgents"
      logrotate_dir="$home/.config/logrotate.d"
      echo "mkdir -p $(shell_quote "$launch_agents") $(shell_quote "$logrotate_dir")"
      echo "$ppc_bin tg service-install --kind launchd --force"
      echo "$ppc_bin tg service-install --kind launchd-health --force"
      echo "$ppc_bin tg service-install --kind logrotate --force"
      echo "cp $(shell_quote "$template_dir/com.check-paper.regression.plist") $(shell_quote "$launch_agents/com.check-paper.regression.plist")"
      echo "cp $(shell_quote "$template_dir/com.check-paper.telegram-logrotate.plist") $(shell_quote "$launch_agents/com.check-paper.telegram-logrotate.plist")"
      echo "launchctl bootstrap gui/\$(id -u) $(shell_quote "$launch_agents/com.check-paper.telegram.plist")"
      echo "launchctl bootstrap gui/\$(id -u) $(shell_quote "$launch_agents/com.check-paper.telegram-health.plist")"
      echo "launchctl bootstrap gui/\$(id -u) $(shell_quote "$launch_agents/com.check-paper.regression.plist")"
      echo "launchctl bootstrap gui/\$(id -u) $(shell_quote "$launch_agents/com.check-paper.telegram-logrotate.plist")"
      echo "launchctl kickstart -k gui/\$(id -u)/com.check-paper.telegram"
      echo "launchctl kickstart -k gui/\$(id -u)/com.check-paper.telegram-health"
      echo "launchctl kickstart -k gui/\$(id -u)/com.check-paper.regression"
      echo "launchctl kickstart -k gui/\$(id -u)/com.check-paper.telegram-logrotate"
      ;;
    systemd)
      echo "mkdir -p \"\$HOME/.config/systemd/user\" \"\$HOME/.config/logrotate.d\""
      echo "$ppc_bin tg service-install --kind systemd --force"
      echo "$ppc_bin tg service-install --kind logrotate --force"
      echo "# Split the generated *.systemd.txt templates into the service/timer files named in their comments."
      echo "scripts/regression-schedule-template.sh systemd"
      echo "scripts/telegram-logrotate-schedule-template.sh systemd"
      echo "scripts/telegram-health-schedule-template.sh systemd"
      echo "systemctl --user daemon-reload"
      echo "systemctl --user enable --now check-paper-telegram.service"
      echo "systemctl --user enable --now check-paper-regression.timer"
      echo "systemctl --user enable --now check-paper-telegram-logrotate.timer"
      echo "systemctl --user enable --now check-paper-telegram-health.timer"
      ;;
    cron)
      echo "$ppc_bin tg service-install --kind logrotate --force"
      echo "# Keep Telegram polling under an external process manager; cron mode covers regression, health alerts, and log rotation schedules."
      echo "scripts/regression-schedule-template.sh cron"
      echo "scripts/telegram-logrotate-schedule-template.sh cron"
      echo "scripts/telegram-health-schedule-template.sh cron"
      echo "crontab -e"
      ;;
  esac
  case "$kind" in
    launchd)
      echo "scripts/production-readiness-evidence.sh $(shell_quote "$author")"
      ;;
    systemd)
      echo "CHECK_PAPER_TG_SERVICE_KINDS='systemd logrotate' scripts/production-readiness-evidence.sh $(shell_quote "$author")"
      ;;
    cron)
      echo "CHECK_PAPER_TG_SERVICE_KINDS='logrotate' scripts/production-readiness-evidence.sh $(shell_quote "$author")"
      ;;
  esac
  echo '```'
  echo
  echo "## Verification Commands"
  echo
  echo '```sh'
  case "$kind" in
    launchd)
      echo "$ppc_bin tg service-check --kind launchd || true"
      echo "$ppc_bin tg service-check --kind launchd-health || true"
      echo "$ppc_bin tg service-check --kind logrotate || true"
      echo "scripts/telegram-deploy-evidence.sh"
      echo "scripts/production-readiness-evidence.sh $(shell_quote "$author")"
      ;;
    systemd)
      echo "$ppc_bin tg service-check --kind systemd || true"
      echo "$ppc_bin tg service-check --kind logrotate || true"
      echo "systemctl --user status check-paper-telegram-health.timer || true"
      echo "CHECK_PAPER_TG_SERVICE_KINDS='systemd logrotate' scripts/telegram-deploy-evidence.sh"
      echo "CHECK_PAPER_TG_SERVICE_KINDS='systemd logrotate' scripts/production-readiness-evidence.sh $(shell_quote "$author")"
      ;;
    cron)
      echo "$ppc_bin tg service-check --kind logrotate || true"
      echo "crontab -l | grep -E 'check-paper\\.telegram-health|telegram-health|tg health --strict --notify' || true"
      echo "CHECK_PAPER_TG_SERVICE_KINDS='logrotate' scripts/telegram-deploy-evidence.sh"
      echo "CHECK_PAPER_TG_SERVICE_KINDS='logrotate' scripts/production-readiness-evidence.sh $(shell_quote "$author")"
      ;;
  esac
  echo "scripts/regression-deploy-evidence.sh"
  echo '```'
  echo
  echo "## Plan Result"
  echo
  if [ -z "$failed_sections" ]; then
    echo "- result: generated"
    echo "- failed_sections: <none>"
  else
    echo "- result: failed"
    echo "- failed_sections: $failed_sections"
  fi
} >> "$plan_path"

echo "production bootstrap plan: $plan_path"

if [ -n "$failed_sections" ]; then
  exit 1
fi
