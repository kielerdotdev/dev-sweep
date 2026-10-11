#!/usr/bin/env bash
set -euo pipefail

schedule=true
mode=scan
uninstall=false
for arg in "$@"; do
  case "$arg" in
    --no-schedule) schedule=false ;;
    --report-only) mode=scan ;;
    --automatic-cleanup) mode=clean ;;
    --uninstall) uninstall=true ;;
    --help)
      printf '%s\n' 'Usage: ./install.sh [--no-schedule | --report-only | --automatic-cleanup | --uninstall]' \
        'Default: install locally and enable daily reports at 03:00. Cleanup requires approval.'
      exit 0 ;;
    *) printf 'Unknown option: %s\n' "$arg" >&2; exit 1 ;;
  esac
done

dev_sweep_source=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)
dev_sweep_bin="$HOME/.local/bin/dev-sweep"
dev_sweep_config="${XDG_CONFIG_HOME:-$HOME/.config}/dev-sweep/config"
dev_sweep_unit_dir="${XDG_CONFIG_HOME:-$HOME/.config}/systemd/user"
dev_sweep_plist="$HOME/Library/LaunchAgents/local.dev-sweep.plist"
dev_sweep_os=$(uname -s)

if $uninstall; then
  case "$dev_sweep_os" in
    Linux)
      systemctl --user disable --now dev-sweep.timer 2>/dev/null || true
      systemctl --user stop dev-sweep.service 2>/dev/null || true
      rm -f -- "$dev_sweep_unit_dir/dev-sweep.timer" "$dev_sweep_unit_dir/dev-sweep.service"
      systemctl --user daemon-reload 2>/dev/null || true ;;
    Darwin)
      launchctl bootout "gui/$(id -u)/local.dev-sweep" 2>/dev/null || true
      rm -f -- "$dev_sweep_plist" ;;
  esac
  rm -f -- "$dev_sweep_bin"
  printf 'Uninstalled. Kept your config at %s\n' "$dev_sweep_config"
  exit 0
fi

case "$dev_sweep_os" in
  Linux|Darwin) ;;
  *) printf 'Supported platforms: Linux and macOS\n' >&2; exit 1 ;;
esac
command -v cargo >/dev/null || { printf 'Install Rust first: https://rustup.rs\n' >&2; exit 1; }
cargo build --manifest-path "$dev_sweep_source/Cargo.toml" --target-dir "$dev_sweep_source/target" --release --locked
mkdir -p -- "$(dirname -- "$dev_sweep_bin")" "$(dirname -- "$dev_sweep_config")"
install -m 755 -- "$dev_sweep_source/target/release/dev-sweep" "$dev_sweep_bin.new"
mv -f -- "$dev_sweep_bin.new" "$dev_sweep_bin"
if [[ ! -f "$dev_sweep_config" ]]; then
  "$dev_sweep_bin" init --config "$dev_sweep_config"
fi
printf 'Installed %s\nConfig: %s\n' "$dev_sweep_bin" "$dev_sweep_config"
if ! $schedule; then exit 0; fi

# Escape paths for the scheduler's format, without running them through a shell.
systemd_escape() { printf '%s' "$1" | sed 's/\\/\\\\/g; s/"/\\"/g; s/%/%%/g; s/\$/$$/g'; }
xml_escape() { printf '%s' "$1" | sed 's/\&/\&amp;/g; s/</\&lt;/g; s/>/\&gt;/g; s/"/\&quot;/g'; }
case "$dev_sweep_os" in
  Linux)
    if ! systemctl --user show-environment >/dev/null 2>&1; then
      printf 'Installed, but a systemd user session is unavailable. Run ./install.sh in your login session to enable the timer.\n' >&2
      exit 1
    fi
    mkdir -p -- "$dev_sweep_unit_dir"
    dev_sweep_apply=''
    if [[ "$mode" == clean ]]; then dev_sweep_apply=' --apply'; fi
    cat > "$dev_sweep_unit_dir/dev-sweep.service" <<EOF
[Unit]
Description=Report development storage cleanup candidates

[Service]
Type=oneshot
ExecStart="$(systemd_escape "$dev_sweep_bin")" $mode$dev_sweep_apply --config "$(systemd_escape "$dev_sweep_config")"
WorkingDirectory=%h
Environment="PATH=$(printf '%s' "$PATH" | sed 's/\\/\\\\/g; s/"/\\"/g; s/%/%%/g')"
Nice=10
IOSchedulingClass=idle
TimeoutStartSec=2h
UMask=0077
EOF
    cat > "$dev_sweep_unit_dir/dev-sweep.timer" <<'EOF'
[Unit]
Description=Daily development storage report

[Timer]
OnCalendar=*-*-* 03:00:00
RandomizedDelaySec=10m
Persistent=true

[Install]
WantedBy=timers.target
EOF
    systemctl --user daemon-reload
    systemctl --user enable --now dev-sweep.timer
    printf 'Daily %s enabled. Logs: journalctl --user -u dev-sweep.service\n' "$mode"
    systemctl --user list-timers dev-sweep.timer --no-pager ;;
  Darwin)
    mkdir -p -- "$(dirname -- "$dev_sweep_plist")" "$HOME/Library/Logs/dev-sweep"
    dev_sweep_apply=''
    if [[ "$mode" == clean ]]; then dev_sweep_apply='<string>--apply</string>'; fi
    cat > "$dev_sweep_plist" <<EOF
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0"><dict>
<key>Label</key><string>local.dev-sweep</string>
<key>ProgramArguments</key><array>
<string>$(xml_escape "$dev_sweep_bin")</string><string>$mode</string>$dev_sweep_apply
<string>--config</string><string>$(xml_escape "$dev_sweep_config")</string>
</array>
<key>WorkingDirectory</key><string>$(xml_escape "$HOME")</string>
<key>EnvironmentVariables</key><dict><key>PATH</key><string>$(xml_escape "$PATH")</string></dict>
<key>StartCalendarInterval</key><dict><key>Hour</key><integer>3</integer><key>Minute</key><integer>0</integer></dict>
<key>LowPriorityIO</key><true/>
<key>StandardOutPath</key><string>$(xml_escape "$HOME/Library/Logs/dev-sweep/output.log")</string>
<key>StandardErrorPath</key><string>$(xml_escape "$HOME/Library/Logs/dev-sweep/error.log")</string>
</dict></plist>
EOF
    plutil -lint "$dev_sweep_plist"
    launchctl bootout "gui/$(id -u)/local.dev-sweep" 2>/dev/null || true
    launchctl bootstrap "gui/$(id -u)" "$dev_sweep_plist"
    printf 'Daily %s enabled. Logs: ~/Library/Logs/dev-sweep/\n' "$mode" ;;
esac
