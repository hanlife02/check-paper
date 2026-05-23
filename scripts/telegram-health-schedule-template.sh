#!/usr/bin/env sh
set -eu

usage() {
  cat <<'EOF'
Usage: scripts/telegram-health-schedule-template.sh launchd|systemd|cron

Print a target-machine schedule template for Telegram polling health alerts.

Environment overrides:
  CHECK_PAPER_PPC_BIN                     ppc binary, default: target/debug/ppc
  CHECK_PAPER_WORKDIR                     repository workdir, default: current dir
  CHECK_PAPER_TG_HEALTH_LOG              scheduler log path, default: ~/.local/state/check-paper/telegram-health.log
  CHECK_PAPER_TG_HEALTH_INTERVAL_SECONDS interval for launchd/systemd, default: 300
  CHECK_PAPER_TG_HEALTH_CRON_SCHEDULE    cron schedule, default: */5 * * * *
EOF
}

kind="${1:-}"
if [ -z "$kind" ] || [ "$kind" = "--help" ] || [ "$kind" = "-h" ]; then
  usage
  exit 0
fi

home="${HOME:-$PWD}"
ppc_bin="${CHECK_PAPER_PPC_BIN:-target/debug/ppc}"
workdir="${CHECK_PAPER_WORKDIR:-$(pwd)}"
log_path="${CHECK_PAPER_TG_HEALTH_LOG:-$home/.local/state/check-paper/telegram-health.log}"
interval_seconds="${CHECK_PAPER_TG_HEALTH_INTERVAL_SECONDS:-300}"
cron_schedule="${CHECK_PAPER_TG_HEALTH_CRON_SCHEDULE:-*/5 * * * *}"

shell_quote() {
  printf "'%s'" "$(printf '%s' "$1" | sed "s/'/'\\\\''/g")"
}

xml_escape() {
  printf '%s' "$1" | sed \
    -e 's/&/\&amp;/g' \
    -e 's/</\&lt;/g' \
    -e 's/>/\&gt;/g' \
    -e 's/"/\&quot;/g' \
    -e "s/'/\&apos;/g"
}

systemd_escape() {
  printf '%s' "$1" | sed \
    -e 's/\\/\\\\/g' \
    -e 's/"/\\"/g' \
    -e 's/%/%%/g'
}

log_dir="$(dirname "$log_path")"
health_command="mkdir -p $(shell_quote "$log_dir") && cd $(shell_quote "$workdir") && $(shell_quote "$ppc_bin") tg health --strict --notify >> $(shell_quote "$log_path") 2>&1"

case "$kind" in
  launchd)
    escaped_command="$(xml_escape "$health_command")"
    cat <<EOF
<!-- Save as ~/Library/LaunchAgents/com.check-paper.telegram-health.plist, then run:
     launchctl bootstrap gui/\$(id -u) ~/Library/LaunchAgents/com.check-paper.telegram-health.plist
     launchctl kickstart -k gui/\$(id -u)/com.check-paper.telegram-health
-->
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN"
  "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>Label</key>
  <string>com.check-paper.telegram-health</string>
  <key>ProgramArguments</key>
  <array>
    <string>/bin/sh</string>
    <string>-lc</string>
    <string>$escaped_command</string>
  </array>
  <key>StartInterval</key>
  <integer>$interval_seconds</integer>
  <key>StandardOutPath</key>
  <string>$(xml_escape "$log_path")</string>
  <key>StandardErrorPath</key>
  <string>$(xml_escape "$log_path")</string>
</dict>
</plist>
EOF
    ;;
  systemd)
    cat <<EOF
# Save the first unit as ~/.config/systemd/user/check-paper-telegram-health.service
[Unit]
Description=check-paper Telegram polling health alert

[Service]
Type=oneshot
ExecStart=/bin/sh -lc "$(systemd_escape "$health_command")"

# Save the second unit as ~/.config/systemd/user/check-paper-telegram-health.timer
[Unit]
Description=Run check-paper Telegram polling health alert

[Timer]
OnBootSec=60
OnUnitActiveSec=${interval_seconds}
Persistent=true
Unit=check-paper-telegram-health.service

[Install]
WantedBy=timers.target

# Then run:
# systemctl --user daemon-reload
# systemctl --user enable --now check-paper-telegram-health.timer
# systemctl --user list-timers check-paper-telegram-health.timer
EOF
    ;;
  cron)
    cat <<EOF
# Install with: crontab -e
# Ensure TELEGRAM_CHAT_IDS or --notify-chat-id-equivalent configuration is available to the cron environment.
$cron_schedule $health_command
EOF
    ;;
  *)
    echo "unknown schedule kind: $kind" >&2
    usage >&2
    exit 2
    ;;
esac
