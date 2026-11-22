#!/usr/bin/env bash
# 把工作区里「不入库」的东西镜像到资产库（vault），供 self-hosted runner 构建时同步。
#
# 用法:
#   scripts/ci/sync_vault.sh                 # 默认 ~/crab_feast-assets
#   CRAB_ASSET_VAULT=/path/to/vault scripts/ci/sync_vault.sh
#
# 什么时候跑：导出了新动画 / 换了模型 / 改了 winit 补丁 / 依赖升级后。
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
VAULT="${CRAB_ASSET_VAULT:-$HOME/crab_feast-assets}"

mkdir -p "$VAULT/assets" "$VAULT/winit"

echo "==> 资源 $(du -sh "$ROOT_DIR/assets" | cut -f1) -> $VAULT/assets"
rsync -a --delete "$ROOT_DIR/assets/" "$VAULT/assets/"

if [[ -d "$ROOT_DIR/third_party/winit" ]]; then
  echo "==> winit 补丁 -> $VAULT/winit"
  rsync -a --delete "$ROOT_DIR/third_party/winit/" "$VAULT/winit/"
else
  echo "warning: 没有 third_party/winit，iOS 27 上会因 UIScene 缺失而崩溃" >&2
fi

if [[ -f "$ROOT_DIR/Cargo.lock" ]]; then
  cp "$ROOT_DIR/Cargo.lock" "$VAULT/Cargo.lock"
fi

echo "==> vault 完成: $(du -sh "$VAULT" | cut -f1)"
