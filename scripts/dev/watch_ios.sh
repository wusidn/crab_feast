#!/usr/bin/env bash
# 监听源码 / 资源变化，自动「构建 -> 装机 -> 启动」，把改完到手机上看到效果压到最短。
#
# 用法:
#   scripts/dev/watch_ios.sh                 # Debug
#   scripts/dev/watch_ios.sh --release       # Release（参数原样透传给 deploy_ios.sh）
#
# 依赖: 有 fswatch 就用 fswatch（brew install fswatch），没有则退化为 2s 轮询。
# 建议单独开一个终端常驻，Ctrl-C 结束。
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
cd "$ROOT_DIR"

WATCH_PATHS=(Cargo.toml launcher crab_feast crab_feast_library crab_feast_ui assets)
DEPLOY="$ROOT_DIR/scripts/dev/deploy_ios.sh"

MARKER="$(mktemp)"
trap 'rm -f "$MARKER"' EXIT
touch "$MARKER"

log() { printf '[%s] %s\n' "$(date +%H:%M:%S)" "$*"; }

deploy_once() {
  log "检测到改动，开始构建部署"
  # 先推进 marker，避免构建产物自身触发下一轮
  touch "$MARKER"
  if "$DEPLOY" "$@"; then
    touch "$MARKER"
    log "已更新到手机，继续监听"
  else
    log "构建/安装失败，等你下次改动再重试"
  fi
}

log "监听: ${WATCH_PATHS[*]}"

if command -v fswatch >/dev/null 2>&1; then
  fswatch -o --latency 1.5 \
    -e '.*/target/.*' -e '.*/build[^/]*/.*' -e '.*/\.git/.*' \
    -e '.*/dist/.*' -e '.*/imported_assets/.*' -e '.*/\.DS_Store$' \
    "${WATCH_PATHS[@]}" |
    while read -r _; do
      deploy_once "$@"
    done
else
  log "未安装 fswatch，使用轮询模式（brew install fswatch 可降低延迟）"
  while true; do
    sleep 2
    CHANGED="$(find "${WATCH_PATHS[@]}" -type f -newer "$MARKER" \
      -not -path "*/target/*" -not -path "*/.git/*" \
      -not -path "*/build*/*" -not -path "*/dist/*" \
      -not -path "*/imported_assets/*" -not -name ".DS_Store" \
      -print -quit 2>/dev/null || true)"
    if [[ -n "$CHANGED" ]]; then
      deploy_once "$@"
    fi
  done
fi
