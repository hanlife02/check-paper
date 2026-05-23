#!/usr/bin/env sh
set -eu

usage() {
  cat <<'EOF'
Usage: scripts/telegram-logrotate-schedule-template.sh launchd|systemd|cron

Print a target-machine schedule template for Telegram log rotation.

Environment overrides:
  CHECK_PAPER_TG_LOGROTATE_CONFIG   logrotate config path, default: ~/.config/logrotate.d/check-paper-telegram
  CHECK_PAPER_TG_LOGROTATE_STATUS   logrotate state path, default: ~/.local/state/check-paper/logrotate.status
  CHECK_PAPER_TG_LOGROTATE_LOG      scheduler log path, default: ~/.local/state/check-paper/telegram-logrotate.log
  CHECK_PAPER_TG_LOGROTATE_HOUR     hour, default: 3
  CHECK_PAPER_TG_LOGROTATE_MINUTE   minute, default: 41
EOF
}

kind="${1:-}"
if [ -z "$kind" ] || [ "$kind" = "--help" ] || [ "$kind" = "-h" ]; then
  usage
  exit 0
fi

home="${HOME:-$PWD}"
config_path="${CHECK_PAPER_TG_LOGROTATE_CONFIG:-$home/.config/logrotate.d/check-paper-telegram}"
status_path="${CHECK_PAPER_TG_LOGROTATE_STATUS:-$home/.local/state/check-paper/logrotate.status}"
log_path="${CHECK_PAPER_TG_LOGROTATE_LOG:-$home/.local/state/check-paper/telegram-logrotate.log}"
hour="${CHECK_PAPER_TG_LOGROTATE_HOUR:-3}"
minute="${CHECK_PAPER_TG_LOGROTATE_MINUTE:-41}"

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

state_dir="$(dirname "$status_path")"
log_dir="$(dirname "$log_path")"
rotate_command="mkdir -p $(shell_quote "$state_dir") $(shell_quote "$log_dir") && logrotate -s $(shell_quote "$status_path") $(shell_quote "$config_path") >> $(shell_quote "$log_path") 2>&1"

case "$kind" in
  launchd)
    escaped_command="$(xml_escape "$rotate_command")"
    cat <<EOF
<!-- Save as ~/Library/LaunchAgents/com.check-paper.telegram-logrotate.plist, then run:
     launchctl bootstrap gui/\$(id -u) ~/Library/LaunchAgents/com.check-paper.telegram-logrotate.plist
     launchctl kickstart -k gui/\$(id -u)/com.check-paper.telegram-logrotate
-->
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN"
  "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>Label</key>
  <string>com.check-paper.telegram-logrotate</string>
  <key>ProgramArguments</key>
  <array>
    <string>/bin/sh</string>
    <string>-lc</string>
    <string>$escaped_command</string>
  </array>
  <key>StartCalendarInterval</key>
  <dict>
    <key>Hour</key>
    <integer>$hour</integer>
    <key>Minute</key>
    <integer>$minute</integer>
  </dict>
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
# Save the first unit as ~/.config/systemd/user/check-paper-telegram-logrotate.service
[Unit]
Description=check-paper Telegram log rotation

[Service]
Type=oneshot
ExecStart=/bin/sh -lc "$(systemd_escape "$rotate_command")"

# Save the second unit as ~/.config/systemd/user/check-paper-telegram-logrotate.timer
[Unit]
Description=Run check-paper Telegram log rotation daily

[Timer]
OnCalendar=*-*-* $(printf '%02d' "$hour"):$(printf '%02d' "$minute"):00
Persistent=true
Unit=check-paper-telegram-logrotate.service

[Install]
WantedBy=timers.target

# Then run:
# systemctl --user daemon-reload
# systemctl --user enable --now check-paper-telegram-logrotate.timer
# systemctl --user list-timers check-paper-telegram-logrotate.timer
EOF
    ;;
  cron)
    cat <<EOF
# Install with: crontab -e
# Then verify with:
# logrotate -d -s $(shell_quote "$status_path") $(shell_quote "$config_path")
$minute $hour * * * $rotate_command
EOF
    ;;
  *)
    echo "unknown schedule kind: $kind" >&2
    usage >&2
    exit 2
    ;;
esac
