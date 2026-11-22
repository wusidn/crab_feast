#!/usr/bin/env bash
# 本机出 ipa 并直接生成 OTA 安装页（不经过 GitHub，全部落在本机）。
#
# 用法:
#   scripts/dev/ota_export.sh                 # Debug 构建 + 导出 + 刷新清单
#   scripts/dev/ota_export.sh --release       # Release（包小、装得快）
#   scripts/dev/ota_export.sh --no-build      # 只按现有 ipa 重刷清单
#   scripts/dev/ota_export.sh --host my-mac.tailxxx.ts.net --port 8443
#
# 前提：Tailscale 已登录，且 admin 控制台里给 tailnet 开了 HTTPS 证书。
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
cd "$ROOT_DIR"

IOS_DIR="launcher/mobile/ios"
SCHEME="crab_feast_mobile"
BUNDLE_ID="org.crab.feast"
TEAM_ID="${IOS_TEAM_ID:-7JRXM77YNL}"
MARKETING_VERSION="${MARKETING_VERSION:-1.0}"

OUT="${OTA_OUT_DIR:-$HOME/.crab_feast-ota}"
PORT="${OTA_PORT:-8443}"
# 和 ota_serve.sh / ota_cert.sh 共用同一份配置：换域名时只改一处
# shellcheck disable=SC1091
[[ -f "$OUT/ota.env" ]] && . "$OUT/ota.env"
PORT="${OTA_HTTPS_PORT:-$PORT}"
HOST="${OTA_HOST:-}"
CONFIG="Debug"
DO_BUILD=1
DO_PACK=1
LEAN=0

# 包体瘦身由 Xcode 的 "Strip unused assets" 构建阶段完成（拷完资源、签名之前删掉
# assets/source），源码目录全程不动 —— 不需要也不应该临时搬移。

# Tailscale 的 CLI 位置随安装方式变化（App Store 包在 .app 里面），
# 非交互 SSH 的 PATH 又很窄，所以这里显式找一遍。
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


while [[ $# -gt 0 ]]; do
  case "$1" in
    --release) CONFIG="Release" ;;
    --no-build) DO_BUILD=0 ;;
    --no-pack) DO_PACK=0 ;;
    --lean) LEAN=1 ;;   # 兼容旧用法：瘦身已由构建阶段完成，此项不再搬移文件
    --host) HOST="${2:?--host 需要参数}"; shift ;;
    --port) PORT="${2:?--port 需要参数}"; shift ;;
    --out) OUT="${2:?--out 需要参数}"; shift ;;
    -h|--help) sed -n '2,15p' "$0"; exit 0 ;;
    *) echo "error: 未知参数 $1" >&2; exit 1 ;;
  esac
  shift
done

if [[ -z "$HOST" ]]; then
  TS="$(find_tailscale)" || {
    echo "error: 没装 tailscale，ota.env 里也没 OTA_HOST" >&2
    echo "hint: 在 $OUT/ota.env 写 OTA_HOST=ota.example.com（IPv6 直连方案）" >&2
    exit 1
  }
  HOST="$("$TS" status --json 2>/dev/null |
    python3 -c 'import json,sys; print(json.load(sys.stdin)["Self"]["DNSName"].rstrip("."))')"
fi

mkdir -p "$OUT"

SHA="$(git rev-parse --short HEAD)"
[ -n "$(git status --porcelain)" ] && SHA="${SHA}-dirty"
# 这个仓库从没 commit 过，`git rev-list --count HEAD` 恒为 0、SHA 也恒定，
# 页面上的 build 号和版本号因此永远不变。改成持久化自增 + 出包时间戳：
#   BUILD  → 写进 ipa 的 CFBundleVersion 与清单的 bundle-version（必须一致）
#   BUILD_ID → 页面上给人看的出包标识
BUILD_NO_FILE="$OUT/.build_number"
mkdir -p "$OUT"
if [[ "$DO_BUILD" == 1 ]]; then
  BUILD=$(( $(cat "$BUILD_NO_FILE" 2>/dev/null || echo 0) + 1 ))
  echo "$BUILD" > "$BUILD_NO_FILE"
  BUILD_ID="$(date +%Y%m%d-%H%M%S)"
else
  # --no-build 复用现有 ipa：构建号必须照抄包里的，否则清单和包对不上
  BUILD="$(unzip -p "$OUT/crab_feast-ios.ipa" Payload/crab_feast_mobile.app/Info.plist 2>/dev/null \
           | plutil -extract CFBundleVersion raw - 2>/dev/null || true)"
  BUILD="${BUILD:-0}"
  BUILD_ID="$(date -r "$OUT/crab_feast-ios.ipa" +%Y%m%d-%H%M%S 2>/dev/null || echo '-')"
fi

if [[ "$DO_BUILD" == 1 ]]; then
  echo "==> archive [$CONFIG] $SCHEME (build $BUILD, $SHA)"
  echo "==> 包体瘦身：构建阶段会从 .app 里删掉 assets/source（$(du -sh assets/source 2>/dev/null | cut -f1) Blender 源文件，运行时不用）"
  (
    cd "$IOS_DIR"
    rm -rf "$OUT/.work/crab.xcarchive" "$OUT/.work/export"
    xcodebuild \
      -scheme "$SCHEME" \
      -configuration "$CONFIG" \
      -sdk iphoneos -arch arm64 \
      -derivedDataPath build-device \
      -allowProvisioningUpdates \
      CODE_SIGN_STYLE=Automatic \
      DEVELOPMENT_TEAM="$TEAM_ID" \
      CURRENT_PROJECT_VERSION="$BUILD" \
      MARKETING_VERSION="$MARKETING_VERSION" \
      -archivePath "$OUT/.work/crab.xcarchive" \
      -quiet \
      archive
    xcodebuild -exportArchive \
      -archivePath "$OUT/.work/crab.xcarchive" \
      -exportPath "$OUT/.work/export" \
      -exportOptionsPlist ExportOptions.plist \
      -allowProvisioningUpdates \
      -quiet
  )
  IPA="$(ls "$OUT"/.work/export/*.ipa | head -1)"
  cp "$IPA" "$OUT/crab_feast-ios.ipa"
fi

if [[ ! -f "$OUT/crab_feast-ios.ipa" ]]; then
  echo "error: $OUT/crab_feast-ios.ipa 不存在，去掉 --no-build 重跑" >&2
  exit 1
fi

# 每次打包都给当前资源做一次版本备份，并把「上一版 → 本版」的增量先算好
if [[ "$DO_PACK" == 1 ]]; then
  echo "==> 资源版本快照 + 预生成增量"
  scripts/dev/ota_pack.sh || echo "（资源打包失败，不影响 ipa，手动跑 scripts/dev/ota_pack.sh 补）"
fi

# 资源版本：页面上印出来，方便分辨「装了新包」还是「资源也更新了」
ASSETS_VER="$(python3 - <<'PY'
import json, pathlib
p = pathlib.Path.home()/".crab_feast-ota/assets/versions.json"
try:
    d = json.loads(p.read_text())
    vs = d.get("versions") or []
    print(vs[-1]["version"] if vs else "-")
except Exception:
    print("-")
PY
)"

IPA_MB=$(( $(stat -f%z "$OUT/crab_feast-ios.ipa") / 1024 / 1024 ))
# 缓存击穿：v= 只用 git SHA 的话，同一 SHA 下重出包 URL 不变，iOS/Safari 可能拿旧包。
# 用「SHA + 内容摘要」作版本号，任何一次出包 URL 都会变。
TAG="${SHA}-$(shasum -a 256 "$OUT/crab_feast-ios.ipa" | cut -c1-8)"
echo "==> 生成清单（${IPA_MB}MB，v=${TAG}）"
python3 scripts/ci/ota_manifest.py \
  --ipa-url "https://${HOST}:${PORT}/crab_feast-ios.ipa?v=${TAG}" \
  --manifest-url "https://${HOST}:${PORT}/manifest.plist?v=${TAG}" \
  --bundle-id "$BUNDLE_ID" \
  --version "$MARKETING_VERSION" \
  --build "$BUILD" \
  --bundle-version "$BUILD" \
  --build-id "$BUILD_ID" \
  --assets-version "$ASSETS_VER" \
  --title "Crab Feast" \
  --subtitle "build ${BUILD} · ${BUILD_ID} · ${SHA} · ${CONFIG}" \
  --out-dir "$OUT"

echo
echo "安装页:  https://${HOST}:${PORT}/"
echo "清单:    https://${HOST}:${PORT}/manifest.plist"
echo
echo "服务没起的话: scripts/dev/ota_serve.sh --start"
echo "手机装完记得去 设置 -> 通用 -> VPN 与设备管理 信任开发者证书"
