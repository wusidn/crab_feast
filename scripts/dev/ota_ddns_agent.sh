#!/usr/bin/env bash
# ota_ddns_agent.sh —— 让 DDNS 每 5 分钟自检一次，运营商换了 IPv6 前缀也能自己跟上
#
#   scripts/dev/ota_ddns_agent.sh install    安装并启动 LaunchAgent
#   scripts/dev/ota_ddns_agent.sh status     查看是否在跑 + 最近日志
#   scripts/dev/ota_ddns_agent.sh run        立刻跑一次（手动排障）
#   scripts/dev/ota_ddns_agent.sh uninstall  卸载
#
# 为什么需要它：家宽 IPv6 前缀由 DHCPv6-PD 下发，光猫重启、运营商租期到期、
# 重拨都会换前缀；SLAAC 会给网卡新地址，但 DNS 里的 AAAA 不会自己变。
# 阈值 300 秒 + 免费版 TTL 下限 600 秒（10 分钟）是这条链路的最短闭环。
set -euo pipefail

LABEL="org.crab.feast.ota-ddns"
PLIST="$HOME/Library/LaunchAgents/$LABEL.plist"
LOG_DIR="$HOME/Library/Logs/crab-feast-ota"
LOG="$LOG_DIR/ddns.log"
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
DDNS="$ROOT/scripts/dev/ota_ddns.py"
PY="/usr/bin/python3"

write_plist() {
  mkdir -p "$LOG_DIR" "$(dirname "$PLIST")"
  cat > "$PLIST" <<PLIST_EOF
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>Label</key><string>$LABEL</string>
  <key>ProgramArguments</key>
  <array>
    <string>$PY</string>
    <string>$DDNS</string>
  </array>
  <key>WorkingDirectory</key><string>$ROOT</string>
  <key>RunAtLoad</key><true/>
  <key>StartInterval</key><integer>300</integer>
  <key>StandardOutPath</key><string>$LOG</string>
  <key>StandardErrorPath</key><string>$LOG</string>
  <key>EnvironmentVariables</key>
  <dict>
    <key>HOME</key><string>$HOME</string>
    <key>PATH</key><string>/usr/bin:/bin:/usr/sbin:/sbin:/usr/local/bin:/opt/homebrew/bin</string>
  </dict>
  <key>ProcessType</key><string>Background</string>
  <key>LowPriorityIO</key><true/>
</dict>
</plist>
PLIST_EOF
}

bootout() {
  launchctl bootout "gui/$UID/$LABEL" 2>/dev/null || launchctl unload "$PLIST" 2>/dev/null || true
}

case "${1:-}" in
  install)
    [[ -x "$DDNS" ]] || { echo "error: 找不到 $DDNS" >&2; exit 1; }
    write_plist
    plutil -lint "$PLIST" >/dev/null || { echo "error: plist 不合法" >&2; exit 1; }
    bootout
    launchctl bootstrap "gui/$UID" "$PLIST" 2>/dev/null || launchctl load -w "$PLIST"
    echo "AGENT_OK ${LABEL}（每 300 秒检查一次，日志 ${LOG}）"
    ;;
  uninstall)
    bootout
    rm -f "$PLIST"
    echo "AGENT_REMOVED $LABEL"
    ;;
  run)
    "$PY" "$DDNS" ;;
  status)
    if launchctl print "gui/$UID/$LABEL" >/dev/null 2>&1; then
      echo "状态: 已加载"
      launchctl print "gui/$UID/$LABEL" | grep -E "state =|last exit code|runs =" | sed 's/^/  /'
    else
      echo "状态: 未加载"
    fi
    echo "--- 最近日志 ---"
    tail -8 "$LOG" 2>/dev/null || echo "(还没有日志)"
    ;;
  *)
    sed -n '2,12p' "$0"; exit 1 ;;
esac
