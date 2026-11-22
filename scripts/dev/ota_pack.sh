#!/usr/bin/env bash
# ota_pack.sh —— 打包时给资源做版本备份，并预生成增量包
#
#   scripts/dev/ota_pack.sh                 # 快照当前资源 + 出整包 + 预生成上版到本版的增量
#   scripts/dev/ota_pack.sh --prune 3       # 顺手只保留最近 3 个版本
#   scripts/dev/ota_pack.sh --list          # 看版本时间线
#
# 产物都在 ~/.crab_feast-ota/assets/（由 crab-feast-web 直接对外提供）：
#   indexes/<ver>.json        版本清单，等于一次完整备份
#   blobs/<sha256>            内容寻址的文件库，跨版本只存一份
#   full/<ver>.zip            整包（首次安装）
#   deltas/<from>_<to>.zip    增量包（只含改动文件，缓存复用）
#
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
cd "$ROOT_DIR"
OUT="${OTA_OUT_DIR:-$HOME/.crab_feast-ota}"
STORE="${CRAB_ASSET_STORE:-$OUT/assets}"
export CRAB_ASSET_STORE="$STORE"

# shellcheck disable=SC1091
[[ -f "$OUT/ota.env" ]] && . "$OUT/ota.env"
HOST="${OTA_HOST:-}"
PORT="${OTA_HTTPS_PORT:-8443}"

if [[ "${1:-}" == "--list" ]]; then
  python3 scripts/dev/assets_store.py list
  exit 0
fi

PRUNE=""
if [[ "${1:-}" == "--prune" ]]; then
  PRUNE="${2:-3}"
fi

# 贴图降采样是手工的一次性步骤（tools/optimize_glb.py），不在构建/打包链路里。
# 一旦有人用 scripts/export_rigs.sh --force 从 FBX 重新导出，characters/rigs 会被
# 未压缩版本覆盖（288MB），然后被原样打进热更包 —— 客户端要下几十上百 MB 的差量。
# 所以这里先量一下，明显超标就停下，要求先跑优化。
RIGS_DIR="assets/characters/rigs"
if [[ -d "$RIGS_DIR" && "${CRAB_ALLOW_BIG_RIGS:-0}" != "1" ]]; then
  RIGS_BYTES=$(find "$RIGS_DIR" -name '*.glb' -type f -exec stat -f%z {} + 2>/dev/null | awk '{s+=$1} END {print s+0}')
  RIGS_MB=$(( RIGS_BYTES / 1024 / 1024 ))
  BIGGEST=$(find "$RIGS_DIR" -name '*.glb' -type f -exec stat -f%z {} + 2>/dev/null | sort -rn | head -1)
  BIGGEST_MB=$(( ${BIGGEST:-0} / 1024 / 1024 ))
  if (( RIGS_MB > 130 )) || (( BIGGEST_MB > 35 )); then
    echo "error: characters/rigs 有 ${RIGS_MB}MB（单文件最大 ${BIGGEST_MB}MB），像是未压缩的原始导出。" >&2
    echo "       这会直接进热更包，客户端要下同体积的差量。" >&2
    echo "       先压一遍再打包：" >&2
    echo "         python3 tools/optimize_glb.py --in $RIGS_DIR --out /tmp/glbq --all --max-tex 2048 --quantize" >&2
    echo "         rsync -a --delete /tmp/glbq/ $RIGS_DIR/" >&2
    echo "       确实要原样打包就设 CRAB_ALLOW_BIG_RIGS=1。" >&2
    exit 2
  fi
  echo "==> rigs 体积 ${RIGS_MB}MB（单文件最大 ${BIGGEST_MB}MB），看起来是压缩过的"
fi

echo "==> 快照当前资源（= 给这一版做备份）"
VER_FILE="$(mktemp)"
python3 scripts/dev/assets_store.py snapshot --src assets --ver-out "$VER_FILE"
VERSION="$(cat "$VER_FILE")"; rm -f "$VER_FILE"

echo "==> 生成整包"
python3 scripts/dev/assets_store.py full --version "$VERSION"

# 上一版到本版先算一次，客户端第一次来问就能直接命中缓存
PREV="$(python3 - <<'PY'
import sys, json, os
sys.path.insert(0, "scripts/dev")
import assets_store
v = assets_store.load_versions()["versions"]
print(v[-2]["version"] if len(v) >= 2 else "")
PY
)"
if [[ -n "$PREV" ]]; then
  echo "==> 预生成增量 ${PREV} → ${VERSION}"
  python3 scripts/dev/assets_store.py delta --from "$PREV" --to "$VERSION" || true
else
  echo "==> 这是第一个版本，没有增量可算"
fi

[[ -n "$PRUNE" ]] && python3 scripts/dev/assets_store.py prune --keep "$PRUNE"

echo
echo "PACK_OK $VERSION"
printf "  整包:     https://%s:%s/assets/full/%s.zip\n" "${HOST:-<OTA_HOST 未配置>}" "$PORT" "$VERSION"
printf "  版本查询: https://%s:%s/assets/state.json\n" "${HOST:-<OTA_HOST 未配置>}" "$PORT"
printf "  增量查询: https://%s:%s/assets/diff?from=<客户端版本>\n" "${HOST:-<OTA_HOST 未配置>}" "$PORT"
