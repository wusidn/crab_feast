#!/usr/bin/env bash
# 为 IPv6 直连签发/续期证书，装到 $OUT/certs，并让 ota_serve 自动重载。
#
#   OTA_HOST=ota.example.com OTA_DNS_PROVIDER=cloudflare scripts/dev/ota_cert.sh
#
# 为什么用 DNS-01 而不是 HTTP-01：家用 80 端口在 IPv4 侧被封、IPv6 侧又依赖入站，
# 用 DNS 验证不受这些影响，也顺带证明域名控制权在你手里。
#
# 环境变量（按 provider 取所需）：
#   cloudflare -> CF_API_TOKEN
#   dnspod     -> DP_LOGIN_TOKEN（id,key）
#   alidns     -> ALI_ACCESS_KEY_ID / ALI_ACCESS_KEY_SECRET
# 输出：CERT_OK / CERT_FAILED
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
OUT="${OTA_OUT_DIR:-$HOME/.crab_feast-ota}"
CERT_DIR="$OUT/certs"

# 与 ota_serve.sh 共用同一份配置，避免换域名时漏改一处
# shellcheck disable=SC1091
[[ -f "$OUT/ota.env" ]] && . "$OUT/ota.env"

HOST="${OTA_HOST:-}"
PROVIDER="${OTA_DNS_PROVIDER:-cloudflare}"

[[ -n "$HOST" ]] || { echo "CERT_FAILED 缺 OTA_HOST"; exit 1; }

# CA 选择：国内直连 acme-v02.api.letsencrypt.org 实测超时（IPv4/IPv6 都 12s 无响应），
# 而 ZeroSSL 的 ACME 端点可达，所以默认 LE，失败自动回落 ZeroSSL（浏览器同样信任）。
CA="${OTA_CA:-letsencrypt}"
CA_FALLBACK="${OTA_CA_FALLBACK:-1}"
ACME_EMAIL="${OTA_ACME_EMAIL:-}"
[[ -n "$ACME_EMAIL" ]] || ACME_EMAIL="admin@${OTA_DNS_ZONE:-example.com}"

# 预检：端点连不上就别让 acme.sh 空转 8 轮重试
probe_ca() {
  case "$1" in
    letsencrypt) u="https://acme-v02.api.letsencrypt.org/directory" ;;
    zerossl)     u="https://acme.zerossl.com/v2/DV90" ;;
    *)           u="" ;;
  esac
  [[ -n "$u" ]] || return 0
  curl -s -o /dev/null --max-time 8 "$u" >/dev/null 2>&1
}
if [[ "$CA" == "letsencrypt" ]] && ! probe_ca letsencrypt; then
  if [[ "$CA_FALLBACK" == "1" ]] && probe_ca zerossl; then
    echo "==> Let's Encrypt 端点不可达，自动改用 ZeroSSL（同 ACME、同信任链）"
    CA="zerossl"
  else
    echo "CERT_FAILED 连不上 ACME 端点（LE 与 ZeroSSL 都不通），检查网络或代理" >&2
    exit 1
  fi
fi

case "$PROVIDER" in
  cloudflare) export CF_Token="${CF_API_TOKEN:-}"; ACME_DNS="dns_cf"
              [[ -n "$CF_Token" ]] || { echo "CERT_FAILED 缺 CF_API_TOKEN"; exit 1; } ;;
  dnspod)     IFS=, read -r DPID DPKEY <<<"${DP_LOGIN_TOKEN:-}"
              export DP_Id="$DPID" DP_Key="$DPKEY"; ACME_DNS="dns_dp"
              [[ -n "${DPKEY:-}" ]] || { echo "CERT_FAILED 缺 DP_LOGIN_TOKEN（id,key）"; exit 1; } ;;
  alidns)     export Ali_Key="${ALI_ACCESS_KEY_ID:-}" Ali_Secret="${ALI_ACCESS_KEY_SECRET:-}"
              ACME_DNS="dns_ali"
              [[ -n "$Ali_Key" && -n "$Ali_Secret" ]] || { echo "CERT_FAILED 缺阿里云 AK/SK"; exit 1; } ;;
  *)          echo "CERT_FAILED 不支持的 OTA_DNS_PROVIDER=$PROVIDER"; exit 1 ;;
esac

# 1) 找 acme.sh；缺 acme.sh 或缺 dnsapi 插件都重装
#    官方 get.acme.sh 走 raw.githubusercontent.com，国内基本被重置；改从 Gitee 整仓 clone
ACME=""
for p in "$(command -v acme.sh 2>/dev/null)" "$HOME/.acme.sh/acme.sh"; do
  [[ -n "$p" && -x "$p" ]] && { ACME="$p"; break; }
done
if [[ -z "$ACME" || ! -d "$HOME/.acme.sh/dnsapi" ]]; then
  echo "==> 安装 acme.sh（含 dnsapi 插件）"
  src="$HOME/.acme.sh/.src"
  rm -rf "$src"
  ok=0
  git clone --depth 1 -q https://gitee.com/neilpang/acme.sh "$src" 2>/dev/null && ok=1
  if [[ "$ok" != 1 ]]; then
    echo "    Gitee 不可用，改用 ghproxy"
    git clone --depth 1 -q https://ghproxy.net/https://github.com/acmesh-official/acme.sh.git "$src" 2>/dev/null && ok=1
  fi
  if [[ "$ok" != 1 ]]; then
    echo "CERT_FAILED 拉不到 acme.sh 仓库（Gitee 与 ghproxy 都失败）"; exit 1
  fi
  ( cd "$src" && ./acme.sh --install -m "crab-feast@localhost" >/dev/null 2>&1 ) || true
  rm -rf "$src"
  ACME="$HOME/.acme.sh/acme.sh"
  [[ -x "$ACME" && -d "$HOME/.acme.sh/dnsapi" ]] || {
    echo "CERT_FAILED acme.sh 安装不完整（缺 dnsapi）"; exit 1; }
  echo "    已装: $("$ACME" --version 2>/dev/null || echo '版本未知')"
fi

# 2) 签发
mkdir -p "$CERT_DIR"
echo "==> DNS-01 签发 ${HOST}（${PROVIDER} / ${CA}）"
EAB_ARGS=""
if [[ "$CA" == "zerossl" ]]; then
  # ZeroSSL 走 EAB（External Account Binding）。acme.sh 自带的自动取 EAB 会报
  # "Cannot resolve _eab_kid"（它拿不到 / 解析不了响应），所以这里自己取再喂给它。
  EAB_KID="${OTA_EAB_KID:-}"
  EAB_HMAC="${OTA_EAB_HMAC:-}"
  if [[ -z "$EAB_KID" ]]; then
    EAB_JSON="$(curl -s --max-time 15 -X POST https://api.zerossl.com/acme/eab-credentials-email \
      --data-urlencode "email=$ACME_EMAIL" 2>/dev/null || true)"
    EAB_KID="$(printf '%s' "$EAB_JSON" | python3 -c 'import json,sys; d=json.load(sys.stdin); print(d.get("eab_kid",""))' 2>/dev/null || true)"
    EAB_HMAC="$(printf '%s' "$EAB_JSON" | python3 -c 'import json,sys; d=json.load(sys.stdin); print(d.get("eab_hmac_key",""))' 2>/dev/null || true)"
  fi
  [[ -n "$EAB_KID" && -n "$EAB_HMAC" ]] || { echo "CERT_FAILED ZeroSSL 取 EAB 凭据失败"; exit 1; }
  EAB_ARGS="--eab-kid $EAB_KID --eab-hmac-key $EAB_HMAC"
  # 账号注册（已注册过会报错，忽略即可）
  # shellcheck disable=SC2086
  "$ACME" --register-account -m "$ACME_EMAIL" --server zerossl $EAB_ARGS >/dev/null 2>&1 || true
fi
if ! "$ACME" --issue -d "$HOST" --dns "$ACME_DNS" --keylength ec-256 --server "$CA"; then
  echo "CERT_FAILED 签发失败，先手动验证 DNS 凭据：" >&2
  echo "  $ACME --issue -d $HOST --dns $ACME_DNS --keylength ec-256 --server $CA --debug 2" >&2
  exit 1
fi

# 3) 安装 + 续期后自动重载服务
RELOAD="if [[ -f $OUT/server.pid ]] && kill -0 \$(cat $OUT/server.pid) 2>/dev/null; then OTA_HOST=$HOST $ROOT_DIR/scripts/dev/ota_serve.sh --ipv6 --restart >/dev/null; fi"
"$ACME" --install-cert -d "$HOST" --ecc \
  --fullchain-file "$CERT_DIR/fullchain.pem" \
  --key-file "$CERT_DIR/privkey.pem" \
  --reloadcmd "$RELOAD" >/dev/null

chmod 600 "$CERT_DIR/privkey.pem" 2>/dev/null || true
EXP="$("$ACME" --list 2>/dev/null | grep -c "$HOST" || true)"
echo "CERT_OK $HOST -> ${CERT_DIR}（acme.sh 已自带 cron 自动续期，剩余 90 天）"
echo "  校验: openssl x509 -noout -dates -ext subjectAltName -in $CERT_DIR/fullchain.pem"
echo "  记录数: $EXP"
