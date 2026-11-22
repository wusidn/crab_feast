#!/usr/bin/env bash
# 起停「本机 OTA 服务」。实际启动的是 crab-feast-web：同一个进程同时提供
# 8443 资源更新/安装页与 8090 服务器注册 API（可用 CRAB_WEB_NO_REGISTRY=1 关掉后者）。
#
#   scripts/dev/ota_serve.sh --start             # tailscale serve，仅 tailnet 内可访问
#   scripts/dev/ota_serve.sh --start --public    # tailscale funnel，公网可访问（有带宽上限）
#   scripts/dev/ota_serve.sh --start --direct    # 自管证书 + tailnet 地址终止 TLS（兜底）
#   scripts/dev/ota_serve.sh --start --ipv6      # 家宽公网 IPv6 直接对外，不依赖 Tailscale
#   scripts/dev/ota_serve.sh --status | --stop
#
# --ipv6 需要：
#   OTA_HOST=xxx.duckdns.org   （手机要能解析到的域名）
#   OTA_CERT / OTA_KEY         （该域名的证书，默认读 $OUT/certs/{fullchain.pem,privkey.pem}）
#
# 其他：CRAB_WEB_BIN 指向编译好的二进制（默认 target/debug/crab-feast-web，缺了会自动构建）；
#       CRAB_REGISTRY_ADDR 改注册 API 监听地址（默认 0.0.0.0:8090，别的机器才能注册）。
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
OUT="${OTA_OUT_DIR:-$HOME/.crab_feast-ota}"
HTTPS_PORT="${OTA_HTTPS_PORT:-8443}"
LOCAL_PORT="${OTA_LOCAL_PORT:-8080}"
CERT_DIR="$OUT/certs"
PID_FILE="$OUT/server.pid"
LOG_FILE="$OUT/server.log"
# 注册中心 / OTA 已经合并成一个 Rust 进程；默认用 debug 产物，生产可指向 release
WEB_BIN="${CRAB_WEB_BIN:-$ROOT_DIR/target/debug/crab-feast-web}"
REGISTRY_ADDR="${CRAB_REGISTRY_ADDR:-0.0.0.0:8090}"

# 统一配置：SSH 强制命令 / cron 拿不到交互 shell 的 export
# shellcheck disable=SC1091
[[ -f "$OUT/ota.env" ]] && . "$OUT/ota.env"

ACTION=""
PUBLIC=0
DIRECT=0
IPV6=0
for a in "$@"; do
  case "$a" in
    --start|--stop|--status|--restart) ACTION="$a" ;;
    --public) PUBLIC=1 ;;
    --direct) DIRECT=1 ;;
    --ipv6) IPV6=1 ;;
    -h|--help) sed -n '2,16p' "$0"; exit 0 ;;
    *) echo "error: 未知参数 $a" >&2; exit 1 ;;
  esac
done
ACTION="${ACTION:---status}"

find_tailscale() {
  if [[ -n "${TAILSCALE_BIN:-}" ]]; then echo "$TAILSCALE_BIN"; return 0; fi
  local p
  for p in "$(command -v tailscale 2>/dev/null)" \
           /Applications/Tailscale.app/Contents/MacOS/Tailscale \
           /opt/homebrew/bin/tailscale \
           /usr/local/bin/tailscale; do
    [[ -n "$p" && -x "$p" ]] && { echo "$p"; return 0; }
  done
  return 1
}

TS=""
need_tailscale() {
  [[ -n "$TS" ]] && return 0
  TS="$(find_tailscale)" || {
    echo "error: 这个模式需要 tailscale；没有的话用 --ipv6" >&2
    exit 1
  }
}

tailnet_name() {
  need_tailscale
  "$TS" status --json 2>/dev/null |
    python3 -c 'import json,sys; print(json.load(sys.stdin)["Self"]["DNSName"].rstrip("."))' || {
      echo "error: tailscale 未登录，先 tailscale up" >&2
      exit 1
    }
}

is_running() {
  [[ -f "$PID_FILE" ]] || return 1
  local pid; pid="$(cat "$PID_FILE" 2>/dev/null || true)"
  [[ -n "$pid" ]] && kill -0 "$pid" 2>/dev/null &&
    ps -p "$pid" -o command= 2>/dev/null | grep -q "crab-feast-web"
}

stop_local() {
  local pid="" port_pid=""
  if [[ -f "$PID_FILE" ]]; then
    pid="$(cat "$PID_FILE" 2>/dev/null || true)"
  fi
  if [[ -n "$pid" ]] && kill -0 "$pid" 2>/dev/null; then
    kill "$pid" 2>/dev/null || true
    local i
    for i in 1 2 3 4 5 6 7 8 9 10; do
      kill -0 "$pid" 2>/dev/null || break
      sleep 0.3
    done
    if kill -0 "$pid" 2>/dev/null; then
      kill -9 "$pid" 2>/dev/null || true
      sleep 0.3
    fi
  fi
  rm -f "$PID_FILE" "$OUT/server.mode"

  # pid 文件丢了（比如清理过目录）时按端口兜底
  local port i
  for port in "$@"; do
    for i in 1 2 3; do
      port_pid="$(lsof -nP -iTCP:"$port" -sTCP:LISTEN -t 2>/dev/null | head -1 || true)"
      [[ -n "$port_pid" ]] || break
      local cmd
      cmd="$(ps -p "$port_pid" -o command= 2>/dev/null || true)"
      if [[ "$cmd" != *"crab-feast-web"* ]]; then
        break
      fi
      echo "==> 端口 $port 上残留 OTA 进程（pid ${port_pid}），清理" >&2
      kill -9 "$port_pid" 2>/dev/null || true
      sleep 0.3
    done
  done
}

stop_tailscale_serve() {
  need_tailscale
  "$TS" serve --https="$HTTPS_PORT" off >/dev/null 2>&1 || true
  "$TS" funnel --https="$HTTPS_PORT" off >/dev/null 2>&1 || true
}

ensure_web_bin() {
  if [[ -x "$WEB_BIN" ]]; then
    return 0
  fi
  echo "==> 首次运行，构建 crab-feast-web（$WEB_BIN）"
  (cd "$ROOT_DIR" && cargo build -p crab_feast_web) || {
    echo "error: 构建失败。也可以先 cargo build -p crab_feast_web --release 并用 CRAB_WEB_BIN 指向产物" >&2
    exit 1
  }
  WEB_BIN="$ROOT_DIR/target/debug/crab-feast-web"
  [[ -x "$WEB_BIN" ]] || {
    echo "error: 构建后仍找不到 $WEB_BIN" >&2
    exit 1
  }
}

start_local() {
  local bind="$1" port="$2" cert="${3:-}" key="${4:-}"
  ensure_web_bin
  mkdir -p "$OUT"
  local ota_addr
  if [[ "$bind" == *:* ]]; then
    ota_addr="[$bind]:$port"
  else
    ota_addr="$bind:$port"
  fi
  local args=(--ota-root "$OUT" --ota-addr "$ota_addr" --addr "$REGISTRY_ADDR")
  if [[ -n "$cert" ]]; then
    args+=(--cert "$cert" --key "$key")
  fi
  if [[ "${CRAB_WEB_NO_REGISTRY:-0}" == 1 ]]; then
    args+=(--no-registry)
  fi
  nohup "$WEB_BIN" "${args[@]}" >> "$LOG_FILE" 2>&1 &
  echo $! > "$PID_FILE"
  sleep 1
  is_running || {
    echo "error: 本机服务启动失败，看 $LOG_FILE" >&2
    if grep -q "Address already in use" "$LOG_FILE" 2>/dev/null; then
      echo "hint: 端口 $port 或 $REGISTRY_ADDR 被占用 -> lsof -nP -iTCP:$port -sTCP:LISTEN（换端口用 OTA_HTTPS_PORT / CRAB_REGISTRY_ADDR）" >&2
    elif grep -q "TLS 配置失败" "$LOG_FILE" 2>/dev/null; then
      echo "hint: 证书/私钥不匹配，检查 OTA_CERT / OTA_KEY" >&2
    else
      tail -5 "$LOG_FILE" >&2 || true
    fi
    exit 1
  }
}

# macOS 自带的是 LibreSSL，没有 -ext / -checkhost，只能从 text 输出里抠 SAN
cert_sans() {
  openssl x509 -noout -text -in "$1" 2>/dev/null |
    awk '/Subject Alternative Name/{f=1} f' |
    grep -oE 'DNS:[^,[:space:]]+' | sed 's/^DNS://'
}

start_ipv6() {
  local host="${OTA_HOST:-}"
  local cert="${OTA_CERT:-$CERT_DIR/fullchain.pem}"
  local key="${OTA_KEY:-$CERT_DIR/privkey.pem}"
  local iface="${OTA_IFACE:-en1}"
  [[ -n "$host" ]] || {
    echo "error: --ipv6 需要 OTA_HOST（手机能解析到的域名，如 ota.example.com）" >&2
    exit 1
  }
  [[ -f "$cert" && -f "$key" ]] || {
    echo "error: 找不到证书 $cert / $key" >&2
    echo "hint: OTA_HOST=$host scripts/dev/ota_cert.sh（DNS-01 签发）" >&2
    exit 1
  }

  # 证书必须是给这个域名的，否则 iOS 静默拒绝，表现为「安装失败」而不是网络错误
  local sans
  sans="$(cert_sans "$cert" | tr '\n' ' ')"
  if ! cert_sans "$cert" | grep -Fxq "$host"; then
    echo "error: 证书 SAN 里没有 ${host}（当前: ${sans:-空}）" >&2
    exit 1
  fi
  if ! openssl x509 -checkend 259200 -noout -in "$cert" >/dev/null 2>&1; then
    echo "warning: 证书 3 天内到期，先跑 scripts/dev/ota_cert.sh 续期" >&2
  fi

  # 前缀漂移：先比一次本机地址与 AAAA，不一致就写回 DNS（只比对可加 OTA_DDNS_CHECK=1）
  local local_v6
  local_v6="$(python3 "$ROOT_DIR/scripts/dev/ota_ddns.py" --iface "$iface" --print-local || true)"
  if [[ -n "${OTA_DNS_PROVIDER:-}" ]]; then
    ddns_out="$(OTA_IFACE="$iface" python3 "$ROOT_DIR/scripts/dev/ota_ddns.py" --host "$host" 2>&1 | tail -1 || true)"
    case "$ddns_out" in
      DDNS_OK*)       echo "==> AAAA 已更新到 $local_v6" ;;
      DDNS_NOCHANGE*) ;;                                  # 已经一致，静默
      *)              [[ "${OTA_DDNS_CHECK:-0}" == 1 ]] || \
                        echo "warning: DDNS 未更新（$ddns_out），手机可能解析到旧地址" >&2 ;;
    esac
  else
    echo "note: 未设置 OTA_DNS_PROVIDER，跳过 DDNS；前缀变了要手动改 AAAA" >&2
  fi

  # 听所有地址，手机走 IPv6 进来；局域网 IPv4 也能访问，方便自测
  start_local "::" "$HTTPS_PORT" "$cert" "$key"
  echo "ipv6" > "$OUT/server.mode"

  # 本机回环自检：证书/绑定错的话这里就会失败，不用等手机
  local code
  code="$(curl -sk --max-time 4 -o /dev/null -w '%{http_code}' "https://[::1]:$HTTPS_PORT/" 2>/dev/null || true)"
  if [[ "$code" != "200" ]]; then
    echo "warning: 本机 https://[::1]:$HTTPS_PORT/ 返回 '$code'，看 $LOG_FILE" >&2
  fi

  echo "==> 安装页: https://$host:$HTTPS_PORT/"
  echo "    本机 pid $(cat "$PID_FILE")，对外只依赖家宽 IPv6 入站（已验证可用）"
  echo "    自检: curl -sk https://$host:$HTTPS_PORT/manifest.plist | head -3"
}

start_direct() {
  echo "==> direct 模式：申请证书"
  mkdir -p "$CERT_DIR"
  local fqdn cert key bind
  fqdn="$(tailnet_name)"
  cert="$CERT_DIR/$fqdn.crt"
  key="$CERT_DIR/$fqdn.key"
  if [[ ! -f "$cert" ]] || ! openssl x509 -checkend 86400 -noout -in "$cert" >/dev/null 2>&1; then
    "$TS" cert --cert-file "$cert" --key-file "$key" "$fqdn" || {
      echo "error: 证书签发失败。Tailscale admin → DNS → 打开 HTTPS Certificates" >&2
      exit 1
    }
  fi
  bind="${OTA_BIND:-$("$TS" ip -4 | head -1)}"
  start_local "$bind" "$HTTPS_PORT" "$cert" "$key"
  echo "==> 监听 https://$fqdn:$HTTPS_PORT/"
}

case "$ACTION" in
  --start|--restart)
    stop_local "$HTTPS_PORT" "$LOCAL_PORT"
    mkdir -p "$OUT"
    [[ -f "$OUT/manifest.plist" ]] || echo "warning: 还没有 manifest.plist，先跑 scripts/dev/ota_export.sh" >&2

    if [[ "$IPV6" == 1 ]]; then
      start_ipv6
      exit 0
    fi

    FQDN="$(tailnet_name)"

    if [[ "$DIRECT" == 1 ]]; then
      start_direct
      echo "==> 安装页: https://$FQDN:$HTTPS_PORT/"
      exit 0
    fi

    start_local 127.0.0.1 "$LOCAL_PORT"

    # 同一个端口不能同时被 serve 和 funnel 占用，先关掉另一种
    subcmd=serve
    if [[ "$PUBLIC" == 1 ]]; then
      subcmd=funnel
      "$TS" serve --https="$HTTPS_PORT" off >/dev/null 2>&1 || true
    else
      "$TS" funnel --https="$HTTPS_PORT" off >/dev/null 2>&1 || true
    fi
    if ! "$TS" "$subcmd" --bg --https="$HTTPS_PORT" "http://127.0.0.1:$LOCAL_PORT" >> "$LOG_FILE" 2>&1; then
      echo "error: tailscale $subcmd 失败，日志:" >&2
      tail -8 "$LOG_FILE" >&2 || true
      echo "hint: 1) admin 里打开 HTTPS Certificates  2) 或改用 --direct / --ipv6" >&2
      stop_local "$HTTPS_PORT" "$LOCAL_PORT"
      exit 1
    fi

    if [[ "$PUBLIC" == 1 ]]; then
      echo "==> 公网安装页: https://$FQDN:$HTTPS_PORT/"
      echo "    首次开启 Funnel 需要约 10 分钟让公网 DNS 生效；且 Funnel 有不可调的带宽上限"
    else
      echo "==> 安装页: https://$FQDN:$HTTPS_PORT/（仅 tailnet 内设备可访问）"
    fi
    echo "    本机源: http://127.0.0.1:$LOCAL_PORT  pid $(cat "$PID_FILE")"
    ;;
  --stop)
    stop_local "$HTTPS_PORT" "$LOCAL_PORT"
    if find_tailscale >/dev/null 2>&1; then
      TS="$(find_tailscale)"
      stop_tailscale_serve
    fi
    echo "==> 已停止"
    ;;
  --status)
    if is_running; then
      echo "local: running pid $(cat "$PID_FILE")  mode=$(cat "$OUT/server.mode" 2>/dev/null || echo unknown)"
    else
      echo "local: stopped"
    fi
    if [[ "$(cat "$OUT/server.mode" 2>/dev/null || true)" == "ipv6" ]]; then
      h="${OTA_HOST:-}"
      c="${OTA_CERT:-$CERT_DIR/fullchain.pem}"
      if [[ -n "$h" ]]; then
        echo "host: $h:$HTTPS_PORT"
        OTA_HOST="$h" OTA_IFACE="${OTA_IFACE:-en1}" python3 "$ROOT_DIR/scripts/dev/ota_ddns.py" --check 2>/dev/null |
          grep -E '^(本机|DNS)' | sed 's/^/  /' || true
      fi
      [[ -f "$c" ]] && echo "cert: $(openssl x509 -noout -enddate -in "$c" 2>/dev/null | sed 's/notAfter=//')"
      echo "probe: $(curl -sk --max-time 4 -o /dev/null -w '%{http_code}' "https://[::1]:$HTTPS_PORT/" 2>/dev/null || echo fail)"
    fi
    if find_tailscale >/dev/null 2>&1; then
      "$(find_tailscale)" serve status 2>/dev/null || true
    fi
    ;;
  *)
    echo "error: 未知参数 $ACTION" >&2
    exit 1
    ;;
esac
