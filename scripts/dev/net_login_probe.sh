#!/usr/bin/env bash
# 登录链路分段体检：DNS → 本机地址 → 监听 → 防火墙 → 入站 UDP 实时采样。
#
# 用法:
#   scripts/dev/net_login_probe.sh                # 只看静态四段
#   scripts/dev/net_login_probe.sh -w 20          # 再采样 20 秒入站 UDP（期间用手机点「加入」）
#   scripts/dev/net_login_probe.sh game.qiaqia.cyou 5099 -w 20
#
# 判读:
#   采样期间 udp_in 增长 + 日志出现 "Received UDP packet from new address" → 包到了服务端
#   两者都没动                                                   → 包根本没到这台 Mac
set -u

HOST="game.qiaqia.cyou"
PORT="5099"
WATCH=0

while [ $# -gt 0 ]; do
  case "$1" in
    -w|--watch) WATCH="$2"; shift 2 ;;
    -h|--help) sed -n '2,12p' "$0"; exit 0 ;;
    *) if [ "$HOST" = "game.qiaqia.cyou" ]; then HOST="$1"; else PORT="$1"; fi; shift ;;
  esac
done

say() { printf '%s\n' "$*"; }
ok()  { printf '  \033[32m✓\033[0m %s\n' "$*"; }
bad() { printf '  \033[31m✗\033[0m %s\n' "$*"; }
warn(){ printf '  \033[33m!\033[0m %s\n' "$*"; }

say "== 1. DNS =="
AAAA=$(dig +short AAAA "$HOST" | head -1)
A=$(dig +short A "$HOST" | head -1)
say "  AAAA : ${AAAA:-<none>}"
say "  A    : ${A:-<none>}"
[ -n "$AAAA" ] && ok "有 IPv6 记录" || bad "没有 AAAA —— 纯 IPv6 服务不可达"

say "== 2. 本机地址是否包含该 AAAA =="
if [ -n "$AAAA" ]; then
  if ifconfig | grep -qw -- "${AAAA}"; then
    ok "该地址确实绑在本机"
  else
    bad "该地址不在本机任何网卡上（DDNS 过期 / 前缀变了）"
    ifconfig | awk '/inet6/ && $0 !~ /fe80/ && $0 !~ /deprecated/ {print "    本机: "$2}'
  fi
fi

say "== 3. 监听 =="
PID=$(lsof -nP -iUDP 2>/dev/null | grep -E ":${PORT}\$" | awk 'NR==1 {print $2}')
if [ -z "$PID" ]; then
  bad "没有任何进程在监听 UDP ${PORT}"
else
  ok "pid ${PID} 在监听 ${PORT}：$(lsof -nP -p "$PID" -a -iUDP | grep -E ":${PORT}\$" | head -1)"
fi

say "== 4. 防火墙 =="
FW=$(/usr/libexec/ApplicationFirewall/socketfilterfw --getglobalstate 2>/dev/null)
say "  应用防火墙: ${FW:-未知}"
case "$FW" in *disabled*|"") ok "未拦截" ;; *) warn "已开启，确认 crab-feast-server 在允许列表里" ;; esac

if [ "${WATCH:-0}" -gt 0 ]; then
  say "== 5. 采样 ${WATCH}s（现在用手机点「加入」）=="
  [ -z "${PID:-}" ] && { bad "没有 pid，跳过采样"; exit 1; }
  LOG=$(lsof -p "$PID" -a -d 1 2>/dev/null | awk 'NR>1 {print $NF}' | head -1)
  [ "${LOG:0:1}" = "/" ] || LOG="/tmp/crab-feast-server-v6.log"
  B0=$(nettop -P -L 1 -m udp -J bytes_in -p "$PID" 2>/dev/null | tail -1 | awk -F, '{print $2}')
  L0=$(wc -c <"$LOG" 2>/dev/null | tr -d ' ')
  for i in $(seq 1 "$WATCH"); do
    sleep 1
    B1=$(nettop -P -L 1 -m udp -J bytes_in -p "$PID" 2>/dev/null | tail -1 | awk -F, '{print $2}')
    printf '  t=%02ds  udp_in=%s (+%s)  日志=%sB\n' "$i" "${B1:-?}" "$(( ${B1:-0} - ${B0:-0} ))" "$(wc -c <"$LOG" 2>/dev/null | tr -d ' ')"
  done
  say "== 采样期新增日志 =="
  NEW=$(tail -c +"$(( ${L0:-0} + 1 ))" "$LOG" 2>/dev/null | grep -E "Received UDP packet|Netcode error|connection challenge|New connection|confirmed connection" | tail -20)
  if [ -n "$NEW" ]; then say "$NEW"; else say "  （无新增：包没到服务端）"; fi
fi
