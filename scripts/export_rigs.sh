#!/usr/bin/env bash
# 批量把 assets/source/character/rigs 下的 Mixamo FBX 导出成带控制器的 GLB，
# 并在同一流程里做贴图降采样（纯 PNG，不动网格/骨骼/动画），直接产出最终资源。
#
#   scripts/export_rigs.sh                    导出 + 降采样 + 回写（默认）
#   scripts/export_rigs.sh --no-optimize      只导出，保留原始贴图分辨率
#   scripts/export_rigs.sh -j 3 --force       3 进程并行，覆盖已有 GLB
#   scripts/export_rigs.sh --list             只列出待处理文件（不导出）
#   scripts/export_rigs.sh --only Ch43        只处理文件名含 Ch43 的
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
BLENDER="${BLENDER:-/Applications/Blender.app/Contents/MacOS/Blender}"
SCRIPT="$ROOT_DIR/scripts/export_mixamo_rigs.py"
OPTIMIZER="$ROOT_DIR/tools/optimize_glb.py"

usage() {
  cat <<'EOF'
Usage: scripts/export_rigs.sh [-j N] [--blender PATH] [--optimize|--no-optimize] [-- <python 参数>]

Options:
  -j, --jobs N      并行 Blender 进程数（默认 1）
  --blender PATH    Blender 可执行文件（默认 /Applications/Blender.app/Contents/MacOS/Blender，
                    也可用环境变量 BLENDER 指定）
  --optimize        导出后降采样贴图再回写（默认开启）
  --no-optimize     只导出，不降采样（贴图保持原始分辨率，单文件可能上百 MB）
  -h, --help        显示本帮助

降采样档位（环境变量）：
  CRAB_TEX_MAX      贴图最长边上限，默认 2048（0=不缩放）
  CRAB_TEX_QUANTIZE 1=256 色量化（默认）｜0=只缩放不量化

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

导出时（Blender 内）的贴图选项（一般不需要，交给后置降采样即可）：
  --compress       预设：贴图最长边压到 2048（格式仍为 PNG，bevy_gltf 不认 JPEG）
  --tex-quality N  贴图 JPEG 质量 1-100（0=不压贴图）
  --tex-format M   auto|png|jpeg（auto=保留原格式）
  --tex-max-size N 贴图最长边上限
  --tex-alpha M    keep=透明贴图保留 PNG（默认）| jpeg
  --tex-normal M   keep=法线贴图保留 PNG（默认）| jpeg
  --decimate R     网格减面比例 0.05-1.0（1.0=不减）
  --draco N        Draco 等级 0-10（Bevy 0.20.1 不支持该扩展，慎用）
EOF
}

JOBS=1
OPTIMIZE=1
FORCE=0
LIST=0
TARGET_OUT="assets/characters/rigs"
TEX_MAX="${CRAB_TEX_MAX:-2048}"
TEX_QUANTIZE="${CRAB_TEX_QUANTIZE:-1}"
PASSTHROUGH=()

while [[ $# -gt 0 ]]; do
  case "$1" in
    -j|--jobs)
      [[ $# -ge 2 ]] || { echo "error: $1 需要参数" >&2; exit 1; }
      JOBS="$2"; shift 2 ;;
    --blender)
      [[ $# -ge 2 ]] || { echo "error: $1 需要参数" >&2; exit 1; }
      BLENDER="$2"; shift 2 ;;
    --optimize) OPTIMIZE=1; shift ;;
    --no-optimize) OPTIMIZE=0; shift ;;
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

# 从透传参数里摘出需要自己处理的：--out / --force / --list
if [[ ${#PASSTHROUGH[@]} -gt 0 ]]; then
  ARGS=()
  set -- "${PASSTHROUGH[@]}"
  while [[ $# -gt 0 ]]; do
    case "$1" in
      --out)
        [[ $# -ge 2 ]] || { echo "error: --out 需要参数" >&2; exit 1; }
        TARGET_OUT="$2"; shift 2 ;;
      --out=*)
        TARGET_OUT="${1#--out=}"; shift ;;
      --force)
        FORCE=1; ARGS+=("$1"); shift ;;
      --list)
        LIST=1; shift ;;
      *)
        ARGS+=("$1"); shift ;;
    esac
  done
  # bash 3.2 + set -u：空数组直接展开会报 unbound
  PASSTHROUGH=("${ARGS[@]+"${ARGS[@]}"}")
fi

if [[ "$LIST" == 1 ]]; then
  if [[ ! -x "$BLENDER" ]]; then
    echo "error: 找不到 Blender：$BLENDER" >&2
    exit 1
  fi
  "$BLENDER" -b --python "$SCRIPT" -- --list --out "$TARGET_OUT" \
    "${PASSTHROUGH[@]+"${PASSTHROUGH[@]}"}" 2>/dev/null || true
  exit 0
fi

if [[ ! -x "$BLENDER" ]]; then
  echo "error: 找不到 Blender：$BLENDER" >&2
  echo "hint: 用 BLENDER=/path/to/blender scripts/export_rigs.sh 或 --blender PATH 指定" >&2
  exit 1
fi
if [[ "$OPTIMIZE" == 1 && ! -f "$OPTIMIZER" ]]; then
  echo "error: 找不到降采样工具 $OPTIMIZER（可用 --no-optimize 跳过）" >&2
  exit 1
fi

run_one() {
  "$BLENDER" -b --python "$SCRIPT" -- "$@" 2>&1 \
    | grep -v -E '^(WARNING|[0-9]{2}:[0-9]{2}:[0-9]{2} \| WARNING)|^Info: Del' || true
  return "${PIPESTATUS[0]}"
}

# 导出到指定目录；并行分片时每个进程写同一目录，文件名互不重叠
export_batch() {
  local out_dir="$1"; shift
  if [[ "$JOBS" -le 1 ]]; then
    run_one --out "$out_dir" "${PASSTHROUGH[@]+"${PASSTHROUGH[@]}"}"
    return $?
  fi

  local count
  count="$("$BLENDER" -b --python "$SCRIPT" -- --list --out "$out_dir" \
    "${PASSTHROUGH[@]+"${PASSTHROUGH[@]}"}" 2>/dev/null \
    | grep -c -E '\.[fF][bB][xX]7?$' || true)"
  if [[ "$count" -eq 0 ]]; then
    echo "没有需要处理的 FBX 文件" >&2
    return 0
  fi
  local jobs="$JOBS"
  [[ "$jobs" -gt "$count" ]] && jobs="$count"

  echo "共 $count 个文件，用 $jobs 个进程并行导出"
  local -a pids=()
  local failed=0
  local i
  for i in $(seq 1 "$jobs"); do
    run_one --shard "$i/$jobs" --out "$out_dir" \
      "${PASSTHROUGH[@]+"${PASSTHROUGH[@]}"}" &
    pids+=("$!")
  done
  local pid
  for pid in "${pids[@]}"; do
    wait "$pid" || failed=1
  done
  return "$failed"
}

dir_mb() {
  find "$1" -name '*.glb' -type f -exec stat -f%z {} + 2>/dev/null \
    | awk '{s+=$1} END {printf "%d", s/1024/1024}'
}

# ---------- 只导出（原始分辨率） ----------
if [[ "$OPTIMIZE" == 0 ]]; then
  export_batch "$TARGET_OUT"
  FAILED=$?
  OUT_MB="$(dir_mb "$TARGET_OUT")"
  echo
  echo "==> $TARGET_OUT 现在 ${OUT_MB}MB（原始贴图分辨率，未降采样）"
  if [[ "${OUT_MB:-0}" -gt 130 ]]; then
    echo "注意: 这个体积会被打包进热更包。要降采样就重跑（去掉 --no-optimize），或手动："
    echo "        python3 tools/optimize_glb.py --in $TARGET_OUT --out /tmp/glbq --all --max-tex 2048 --quantize"
    echo "        rsync -a --delete /tmp/glbq/ $TARGET_OUT/"
  fi
  exit "$FAILED"
fi

# ---------- 导出 → 降采样 → 回写 ----------
WORK="$(mktemp -d "${TMPDIR:-/tmp}/crab_rigs.XXXXXX")"
STAGE="$WORK/raw"
OPT_DIR="$WORK/opt"
mkdir -p "$STAGE" "$OPT_DIR"
trap 'rm -rf "$WORK"' EXIT

echo "==> 1/3 导出到临时区（原始贴图分辨率）"
export_batch "$STAGE" || { echo "error: 导出失败" >&2; exit 1; }

shopt -s nullglob
RAW_GLB=("$STAGE"/*.glb)
if [[ ${#RAW_GLB[@]} -eq 0 ]]; then
  echo "没有导出任何 GLB（源目录可能为空，或 --only/--skip 都过滤掉了）" >&2
  exit 0
fi
RAW_MB="$(dir_mb "$STAGE")"
echo "    导出 ${#RAW_GLB[@]} 个文件，共 ${RAW_MB}MB"

echo "==> 2/3 降采样贴图（最长边 ${TEX_MAX}，量化 ${TEX_QUANTIZE}，格式 PNG）"
OPT_ARGS=(--in "$STAGE" --out "$OPT_DIR" --all --format png --max-tex "$TEX_MAX")
[[ "$TEX_QUANTIZE" == 1 ]] && OPT_ARGS+=(--quantize)
python3 "$OPTIMIZER" "${OPT_ARGS[@]}"
OPT_MB="$(dir_mb "$OPT_DIR")"
echo "    压缩后 ${OPT_MB}MB（省 $(( RAW_MB - OPT_MB ))MB）"

echo "==> 3/3 回写到 $TARGET_OUT"
mkdir -p "$TARGET_OUT"
COPIED=0
SKIPPED=0
for f in "$OPT_DIR"/*.glb; do
  name="$(basename "$f")"
  if [[ -e "$TARGET_OUT/$name" && "$FORCE" == 0 ]]; then
    SKIPPED=$((SKIPPED + 1))
    continue
  fi
  cp "$f" "$TARGET_OUT/$name"
  COPIED=$((COPIED + 1))
done
if [[ "$SKIPPED" -gt 0 ]]; then
  echo "    跳过 $SKIPPED 个已存在文件（要覆盖加 --force）"
fi
echo "    写入 $COPIED 个文件"
echo
echo "完成: $TARGET_OUT 共 $(dir_mb "$TARGET_OUT")MB，$(find "$TARGET_OUT" -name '*.glb' -type f | wc -l | tr -d ' ') 个 GLB"
echo "下一步: scripts/dev/ota_pack.sh"
