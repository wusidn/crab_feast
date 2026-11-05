#!/usr/bin/env bash
# 批量把 assets/source/character/rigs 下的 Mixamo FBX 导出成带控制器的 GLB。
#
#   scripts/export_rigs.sh                 导出到 assets/characters/rigs
#   scripts/export_rigs.sh -j 3 --force    3 进程并行并覆盖已有文件
#   scripts/export_rigs.sh --list          只列出待处理文件
#   scripts/export_rigs.sh --only Ch43     只处理文件名含 Ch43 的
#
# 其余参数原样透传给 scripts/export_mixamo_rigs.py（见其 --help）。
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
BLENDER="${BLENDER:-/Applications/Blender.app/Contents/MacOS/Blender}"
SCRIPT="$ROOT_DIR/scripts/export_mixamo_rigs.py"

usage() {
  cat <<'EOF'
Usage: scripts/export_rigs.sh [-j N] [--blender PATH] [-- <python 参数>]

Options:
  -j, --jobs N     并行 Blender 进程数（默认 1）
  --blender PATH   Blender 可执行文件（默认 /Applications/Blender.app/Contents/MacOS/Blender，
                   也可用环境变量 BLENDER 指定）
  -h, --help       显示本帮助

Python 侧参数（需放在 -- 之后，或直接跟在上面这些选项后面）：
  --src DIR        源目录（默认 assets/source/character/rigs）
  --out DIR        输出目录（默认 assets/characters/rigs）
  --only PATTERN   只处理匹配的文件名，可重复
  --skip PATTERN   跳过匹配的文件名，可重复
  --force          覆盖已存在的 GLB（默认跳过）
  --keep-name      保留 FBX 原始文件名（默认截断第一个下划线及其后，
                   如 Ch02_nonPBR.fbx -> Ch02.glb）
  --no-anim        创建控制器时不烘焙动画
  --fk-arms/--fk-legs  手臂/腿改用 FK
  --list           只列出待处理文件

压缩相关：
  --compress       预设：贴图转 JPEG(quality=80) + 贴图最长边 2048
  --tex-quality N  贴图 JPEG 质量 1-100（0=不压贴图）
  --tex-format M   auto|jpeg（auto=保留原格式）
  --tex-max-size N 贴图最长边上限（超过则等比缩小）
  --tex-alpha M    keep=透明贴图保留 PNG（默认）| jpeg
  --tex-normal M   keep=法线贴图保留 PNG（默认）| jpeg
  --decimate R     网格减面比例 0.05-1.0（1.0=不减）
  --draco N        Draco 等级 0-10（Bevy 0.20.1 不支持该扩展，慎用）
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
  echo "hint: 用 BLENDER=/path/to/blender scripts/export_rigs.sh 或 --blender PATH 指定" >&2
  exit 1
fi

run_one() {
  "$BLENDER" -b --python "$SCRIPT" -- "$@" 2>&1 \
    | grep -v -E '^(WARNING|[0-9]{2}:[0-9]{2}:[0-9]{2} \| WARNING)|^Info: Del' || true
  return "${PIPESTATUS[0]}"
}

if [[ "$JOBS" -le 1 ]]; then
  run_one "${PASSTHROUGH[@]+"${PASSTHROUGH[@]}"}"
  exit $?
fi

# 并行：先让 Python 侧数一下总数，再按总分片启动多个进程
COUNT="$("$BLENDER" -b --python "$SCRIPT" -- --list "${PASSTHROUGH[@]+"${PASSTHROUGH[@]}"}" 2>/dev/null \
  | grep -c -E '\.[fF][bB][xX]7?$' || true)"
if [[ "$COUNT" -eq 0 ]]; then
  echo "没有需要处理的 FBX 文件" >&2
  exit 0
fi
[[ "$JOBS" -gt "$COUNT" ]] && JOBS="$COUNT"

echo "共 $COUNT 个文件，用 $JOBS 个进程并行导出"
PIDS=()
FAILED=0
for i in $(seq 1 "$JOBS"); do
  run_one --shard "$i/$JOBS" "${PASSTHROUGH[@]+"${PASSTHROUGH[@]}"}" &
  PIDS+=("$!")
done
for pid in "${PIDS[@]}"; do
  wait "$pid" || FAILED=1
done

exit "$FAILED"
