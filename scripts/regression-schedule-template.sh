#!/usr/bin/env sh
set -eu

usage() {
  cat <<'EOF'
Usage: scripts/regression-schedule-template.sh launchd|systemd|cron

Print a target-machine schedule template for scripts/regression-check.sh.

Environment overrides:
  CHECK_PAPER_WORKDIR                 repository workdir, default: current dir
  CHECK_PAPER_REGRESSION_REPORT_DIR   report directory, default: /private/tmp/check-paper-eval-gate
  CHECK_PAPER_REGRESSION_LOG          scheduler log path, default: <report_dir>/check-paper-regression.log
  CHECK_PAPER_REGRESSION_WEEKDAY      weekday, default: 1 (Monday for launchd/cron)
  CHECK_PAPER_REGRESSION_HOUR         hour, default: 3
  CHECK_PAPER_REGRESSION_MINUTE       minute, default: 23
EOF
}

kind="${1:-}"
if [ -z "$kind" ] || [ "$kind" = "--help" ] || [ "$kind" = "-h" ]; then
  usage
  exit 0
fi

workdir="${CHECK_PAPER_WORKDIR:-$(pwd)}"
report_dir="${CHECK_PAPER_REGRESSION_REPORT_DIR:-/private/tmp/check-paper-eval-gate}"
log_path="${CHECK_PAPER_REGRESSION_LOG:-$report_dir/check-paper-regression.log}"
weekday="${CHECK_PAPER_REGRESSION_WEEKDAY:-1}"
hour="${CHECK_PAPER_REGRESSION_HOUR:-3}"
minute="${CHECK_PAPER_REGRESSION_MINUTE:-23}"

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

regression_command="cd $(shell_quote "$workdir") && CHECK_PAPER_REGRESSION_REPORT_DIR=$(shell_quote "$report_dir") scripts/regression-check.sh >> $(shell_quote "$log_path") 2>&1"

case "$kind" in
  launchd)
    escaped_command="$(xml_escape "$regression_command")"
    cat <<EOF
<!-- Save as ~/Library/LaunchAgents/com.check-paper.regression.plist, then run:
     launchctl bootstrap gui/\$(id -u) ~/Library/LaunchAgents/com.check-paper.regression.plist
     launchctl kickstart -k gui/\$(id -u)/com.check-paper.regression
-->
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN"
  "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>Label</key>
  <string>com.check-paper.regression</string>
  <key>ProgramArguments</key>
  <array>
    <string>/bin/sh</string>
    <string>-lc</string>
    <string>$escaped_command</string>
  </array>
  <key>StartCalendarInterval</key>
  <dict>
    <key>Weekday</key>
    <integer>$weekday</integer>
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
# Save the first unit as ~/.config/systemd/user/check-paper-regression.service
[Unit]
Description=check-paper regression gate

[Service]
Type=oneshot
WorkingDirectory=$workdir
ExecStart=/bin/sh -lc "$(systemd_escape "$regression_command")"

# Save the second unit as ~/.config/systemd/user/check-paper-regression.timer
[Unit]
Description=Run check-paper regression gate weekly

[Timer]
OnCalendar=Mon *-*-* $(printf '%02d' "$hour"):$(printf '%02d' "$minute"):00
Persistent=true
Unit=check-paper-regression.service

[Install]
WantedBy=timers.target

# Then run:
# systemctl --user daemon-reload
# systemctl --user enable --now check-paper-regression.timer
# systemctl --user list-timers check-paper-regression.timer
EOF
    ;;
  cron)
    cat <<EOF
# Install with: crontab -e
# Then verify the latest evidence Markdown in: $report_dir
$minute $hour * * $weekday $regression_command
EOF
    ;;
  *)
    echo "unknown schedule kind: $kind" >&2
    usage >&2
    exit 2
    ;;
esac
