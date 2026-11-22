#!/usr/bin/env bash
# 户外一键出包：构建 → 导出 ipa → 刷新清单 → 确保 OTA 服务在跑。
#
# 这个脚本同时是 SSH 强制命令（authorized_keys 里 command="..."），
# 手机上的「快捷指令」只需要连 SSH 跑它，不用传参数。
#
#   OTA_PROFILE=debug scripts/dev/ota_publish.sh   # 想快：Debug，包大
#   scripts/dev/ota_publish.sh                     # 默认 Release + 剔除 source
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
cd "$ROOT_DIR"

OUT="${OTA_OUT_DIR:-$HOME/.crab_feast-ota}"
mkdir -p "$OUT"
LOG="$OUT/publish.log"

# 统一配置：SSH 强制命令不会带上你交互 shell 里的 export，写在这里最省事
# shellcheck disable=SC1091
[[ -f "$OUT/ota.env" ]] && . "$OUT/ota.env"

PROFILE="${OTA_PROFILE:-release}"
FLAGS=(--lean)
if [[ "$PROFILE" == "release" ]]; then
  FLAGS+=(--release)
fi

# 有 OTA_HOST 就走家宽 IPv6 直连（已验证入站可用），否则回落到 tailscale serve
MODE="${OTA_MODE:-}"
if [[ -z "$MODE" ]]; then
  if [[ -n "${OTA_HOST:-}" ]]; then MODE=ipv6; else MODE=serve; fi
fi
SERVE_ARGS=""
case "$MODE" in
  ipv6)   SERVE_ARGS="--ipv6" ;;
  funnel) SERVE_ARGS="--public" ;;
  direct) SERVE_ARGS="--direct" ;;
  serve|tailscale) SERVE_ARGS="" ;;
  *) echo "error: 未知 OTA_MODE=$MODE" >&2; exit 1 ;;
esac

{
  echo "=== $(date '+%F %T') publish start (profile=$PROFILE) ==="
  if ! scripts/dev/ota_export.sh "${FLAGS[@]}"; then
    echo "=== $(date '+%F %T') export FAILED ==="
    echo "PUBLISH_FAILED"
    exit 1
  fi
  scripts/dev/ota_serve.sh --start $SERVE_ARGS
  echo "=== $(date '+%F %T') publish OK ==="
  echo "PUBLISH_OK"
} 2>&1 | tee -a "$LOG"
