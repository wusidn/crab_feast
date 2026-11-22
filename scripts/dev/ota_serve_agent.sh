#!/usr/bin/env bash
# ota_serve_agent.sh —— OTA 服务的自愈守护：每 60 秒探一次回环，挂了就重启
#
#   scripts/dev/ota_serve_agent.sh install    安装并启动 LaunchAgent
#   scripts/dev/ota_serve_agent.sh status     守护状态 + 最近日志
#   scripts/dev/ota_serve_agent.sh check      立刻探一次（挂了会重启）
#   scripts/dev/ota_serve_agent.sh uninstall  卸载
#
# 为什么需要：2026-11-21 实测过一次「进程还在、端口在 LISTEN，但所有握手被 reset」的僵死态
# （手机端表现为页面打不开、本机 curl 也是 000）。进程活着不代表能服务，只能靠主动探测。
set -euo pipefail

LABEL="org.crab.feast.ota-serve"
PLIST="$HOME/Library/LaunchAgents/$LABEL.plist"
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
SERVE="$ROOT/scripts/dev/ota_serve.sh"
PORT="${OTA_HTTPS_PORT:-8443}"
LOG_DIR="$HOME/Library/Logs/crab-feast-ota"
LOG="$LOG_DIR/serve.log"

mkdir -p "$LOG_DIR"

alive() {
  curl -sk --max-time 3 -o /dev/null "https://[::1]:${PORT}/" 2>/dev/null
}

do_check() {
  if alive; then
    return 0
  fi
  echo "$(date '+%F %T') 回环探测失败，重启服务" >>"$LOG"
  "$SERVE" --ipv6 --restart >>"$LOG" 2>&1 || echo "$(date '+%F %T') 重启失败" >>"$LOG"
}

write_plist() {
  cat > "$PLIST" <<PLIST_EOF
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>Label</key><string>$LABEL</string>
  <key>ProgramArguments</key>
  <array>
    <string>/bin/bash</string>
    <string>-lc</string>
    <string>$0 check</string>
  </array>
  <key>WorkingDirectory</key><string>$ROOT</string>
  <key>RunAtLoad</key><true/>
  <key>StartInterval</key><integer>60</integer>
  <key>StandardOutPath</key><string>$LOG</string>
  <key>StandardErrorPath</key><string>$LOG</string>
  <key>EnvironmentVariables</key>
  <dict>
    <key>HOME</key><string>$HOME</string>
    <key>PATH</key><string>/usr/bin:/bin:/usr/sbin:/sbin:/usr/local/bin:/opt/homebrew/bin</string>
  </dict>
</dict>
</plist>
PLIST_EOF
}

bootout() { launchctl bootout "gui/$UID/$LABEL" 2>/dev/null || true; }

case "${1:-}" in
  install)
    write_plist
    plutil -lint "$PLIST" >/dev/null || { echo "error: plist 不合法" >&2; exit 1; }
    bootout
    launchctl bootstrap "gui/$UID" "$PLIST" 2>/dev/null || launchctl load -w "$PLIST"
    echo "AGENT_OK ${LABEL}（每 60 秒探测 https://[::1]:${PORT}/）"
    ;;
  uninstall) bootout; rm -f "$PLIST"; echo "AGENT_REMOVED ${LABEL}" ;;
  check)     do_check ;;
  status)
    if launchctl print "gui/$UID/$LABEL" >/dev/null 2>&1; then
      echo "守护: 已加载"; launchctl print "gui/$UID/$LABEL" | grep -E "state =|runs =" | sed 's/^/  /'
    else
      echo "守护: 未加载"
    fi
    printf "服务: "; alive && echo "活着 (200)" || echo "死了 / 无响应"
    echo "--- 最近日志 ---"; tail -6 "$LOG" 2>/dev/null || echo "(无)"
    ;;
  *) sed -n '2,10p' "$0"; exit 1 ;;
esac
