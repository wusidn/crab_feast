#!/usr/bin/env bash
# 把 assets/source/character/animations/Default 的基础移动动画重投影到
# assets/source/character/rigs 下的每个角色，输出到 assets/characters/animations/<角色>/。
#
#   scripts/export_animations.sh                 全部角色 × 全部动画（串行）
#   scripts/export_animations.sh -j 4            4 个 Blender 进程并行（按角色分片）
#   scripts/export_animations.sh --only Amy      只处理名字含 Amy 的角色
#   scripts/export_animations.sh --clips walking,idle
#   scripts/export_animations.sh --force         覆盖已存在的 GLB
#   scripts/export_animations.sh --rebuild-rig   重建控制器 rig 缓存
#
# 其余参数原样透传给 scripts/export_mixamo_animations.py（见其 --help）。
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
BLENDER="${BLENDER:-/Applications/Blender.app/Contents/MacOS/Blender}"
SCRIPT="$ROOT_DIR/scripts/export_mixamo_animations.py"

usage() {
  cat <<'EOF'
Usage: scripts/export_animations.sh [-j N] [--blender PATH] [-- <python 参数>]

Options:
  -j, --jobs N     并行 Blender 进程数（默认 1，按角色分片）
  --blender PATH   Blender 可执行文件（默认 /Applications/Blender.app/Contents/MacOS/Blender，
                   也可用环境变量 BLENDER 指定）
  -h, --help       显示本帮助

Python 侧参数（可直接跟在上面这些选项后面，也可放在 -- 之后）：
  --rigs DIR       角色 FBX 目录（默认 assets/source/character/rigs）
  --anims DIR      动画 FBX 目录（默认 assets/source/character/animations/Default）
  --out DIR        输出根目录（默认 assets/characters/animations）
  --cache-dir DIR  控制器 rig 缓存目录
  --only PATTERN   只处理匹配的角色名，可重复
  --skip PATTERN   跳过匹配的角色名，可重复
  --clips A,B      只处理指定动画（文件名或 Pascal 名）
  --force          覆盖已存在的 GLB（默认跳过）
  --rebuild-rig    强制重建控制器 rig 缓存
  --ik             重定向使用 IK（默认 FK，体型差异大时 IK 会让角色悬空）
  --ratio-mode M   位移重投影基准：leg（默认）| height | none
  --no-ground      关闭贴地校正
  --frame-step N   贴地测量采样步长（默认 2 帧）
  --list           只列出待处理组合
EOF
}

JOBS=1
PASSTHROUGH=()

while [[ $# -gt 0 ]]; do
  case "$1" in
    -j|--jobs)
      [[ $# -ge 2 ]] || { echo "error: $1 需要参数" >&2; exit 1; }
      JOBS="$2"; shift 2 ;;
    --blender)
      [[ $# -ge 2 ]] || { echo "error: $1 需要参数" >&2; exit 1; }
      BLENDER="$2"; shift 2 ;;
    -h|--help)
      usage; exit 0 ;;
    --)
      shift
      PASSTHROUGH+=("$@")
      break ;;
    *)
      PASSTHROUGH+=("$1"); shift ;;
  esac
done

if [[ ! -x "$BLENDER" ]]; then
  echo "error: 找不到 Blender：$BLENDER" >&2
  echo "hint: 用 BLENDER=/path/to/blender scripts/export_animations.sh 或 --blender PATH 指定" >&2
  exit 1
fi

run_one() {
  "$BLENDER" -b --python "$SCRIPT" -- "$@" 2>&1 \
    | grep -v -E '^(WARNING|[0-9]{2}:[0-9]{2}:[0-9]{2} \| WARNING)|^Info: Del|^Read blend' || true
  return "${PIPESTATUS[0]}"
}

if [[ "$JOBS" -le 1 ]]; then
  run_one --manifest "${PASSTHROUGH[@]+"${PASSTHROUGH[@]}"}"
  STATUS=$?
else
  COUNT="$("$BLENDER" -b --python "$SCRIPT" -- --list "${PASSTHROUGH[@]+"${PASSTHROUGH[@]}"}" 2>/dev/null \
    | grep -c -E '^/.*\.fbx$' || true)"
  if [[ "$COUNT" -eq 0 ]]; then
    echo "没有需要处理的角色" >&2
    exit 0
  fi
  [[ "$JOBS" -gt "$COUNT" ]] && JOBS="$COUNT"
  echo "共 $COUNT 个角色，用 $JOBS 个进程并行导出"
  PIDS=()
  FAILED=0
  for i in $(seq 1 "$JOBS"); do
    run_one --shard "$i/$JOBS" --manifest "${PASSTHROUGH[@]+"${PASSTHROUGH[@]}"}" &
    PIDS+=("$!")
  done
  for pid in "${PIDS[@]}"; do
    wait "$pid" || FAILED=1
  done
  STATUS="$FAILED"
fi

# 校验产物并重建 manifest.json（普通 python，不需要 Blender）
python3 "$ROOT_DIR/scripts/validate_animations.py" --quiet || STATUS=$?

if [[ "$STATUS" -eq 0 ]]; then
  rm -f "$ROOT_DIR"/assets/characters/animations/manifest.part-*.json
fi

exit "$STATUS"
