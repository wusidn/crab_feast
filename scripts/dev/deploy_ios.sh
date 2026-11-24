#!/usr/bin/env bash
# 一键构建并安装到已连接 / 已配对的 iPhone，然后直接启动看效果。
#
# 用法:
#   scripts/dev/deploy_ios.sh                 # Release 构建 + 安装 + 启动（默认，60fps 稳）
#   scripts/dev/deploy_ios.sh --debug         # Debug 构建（帧率会掉到 35fps，只在排障时用）
#   scripts/dev/deploy_ios.sh --build-only    # 只构建
#   scripts/dev/deploy_ios.sh --no-launch     # 装完不启动
#   scripts/dev/deploy_ios.sh --udid <id>     # 指定设备
#   scripts/dev/deploy_ios.sh --list          # 列出可用设备
#
# 设备 id 也可以用环境变量 IOS_UDID 传。
# 无线安装前提: 首次 USB 配对一次；之后手机与 Mac 在同一局域网即可。
#   Xcode 27 的 Device Hub 已经没有「Connect via network」勾选框（我全盘 grep 过
#   Xcode 里也没有该字符串），配对记录本身就让 CoreDevice 走网络传输：
#   手机一上 Wi-Fi，Bonjour 会广播 _remotepairing._tcp，devicectl 自动切成
#   Transport Type: localNetwork，安装/启动/抓 console 全部无线。
#   验证: xcrun devicectl device info details --device <udid> | grep Transport
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
cd "$ROOT_DIR"

IOS_DIR="launcher/mobile/ios"
SCHEME="crab_feast_mobile"
BUNDLE_ID="org.crab.feast"
TEAM_ID="${IOS_TEAM_ID:-7JRXM77YNL}"
# Debug 构建在 iPhone 上只能勉强 60fps，场景一复杂就掉到 20~33ms（实测抖得厉害），
# 所以默认走 Release；需要符号/断点时显式 --debug。
CONFIG="Release"

DO_BUILD=1
DO_LAUNCH=1
DO_LIST=0
UDID="${IOS_UDID:-}"

while [[ $# -gt 0 ]]; do
  case "$1" in
    --release) CONFIG="Release" ;;
    --debug) CONFIG="Debug" ;;
    --build-only) DO_LAUNCH=0 ;;
    --no-launch) DO_LAUNCH=0 ;;
    --list) DO_LIST=1 ;;
    --udid) UDID="${2:?--udid 需要参数}"; shift ;;
    -h|--help) sed -n '2,18p' "$0"; exit 0 ;;
    *) echo "error: 未知参数 $1" >&2; exit 1 ;;
  esac
  shift
done

list_devices() {
  local json
  json="$(mktemp)"
  xcrun devicectl list devices --json-output "$json" >/dev/null
  echo "IDENTIFIER                               REALITY   STATE      TRANSPORT  NAME"
  python3 - "$json" <<'PY'
import json, sys
devices = json.load(open(sys.argv[1]))["result"]["devices"]
for d in devices:
    hw = d.get("properties", {}).get("hardware", {})
    conn = d.get("properties", {}).get("connection", {})
    if hw.get("platform") != "iOS":
        continue
    name = d.get("deviceProperties", {}).get("name") or hw.get("marketingName")
    print(f'{d["identifier"]:40} {hw.get("reality", "?"):9} {conn.get("state", "?"):10} '
          f'{conn.get("transportType", "?"):10} {name}')
PY
  rm -f "$json"
}

# 自动挑选第一台「物理设备且已连接」的 iPhone。
resolve_udid() {
  local json
  json="$(mktemp)"
  xcrun devicectl list devices --json-output "$json" >/dev/null
  python3 - "$json" <<'PY'
import json, sys
devices = json.load(open(sys.argv[1]))["result"]["devices"]
physical = [d for d in devices
            if d.get("properties", {}).get("hardware", {}).get("reality") == "physical"
            and d.get("properties", {}).get("hardware", {}).get("platform") == "iOS"]
if not physical:
    sys.exit("error: 没有找到已配对的 iPhone，先 USB 连一次并在 Xcode 里信任")
connected = [d for d in physical
             if d.get("properties", {}).get("connection", {}).get("state") == "connected"]
if not connected:
    names = ", ".join(d.get("deviceProperties", {}).get("name", "?") for d in physical)
    sys.exit(f"error: 设备已配对但当前未连接（{names}）")
print(connected[0]["identifier"])
PY
  rm -f "$json"
}

if [[ "$DO_LIST" == 1 ]]; then
  list_devices
  exit 0
fi

if [[ -z "$UDID" ]]; then
  UDID="$(resolve_udid)" || exit 1
fi

START=$SECONDS

if [[ "$DO_BUILD" == 1 ]]; then
  echo "==> 构建 $SCHEME [$CONFIG] (device/arm64)"
  (
    cd "$IOS_DIR"
    xcodebuild \
      -scheme "$SCHEME" \
      -configuration "$CONFIG" \
      -sdk iphoneos \
      -arch arm64 \
      -derivedDataPath build-device \
      -allowProvisioningUpdates \
      CODE_SIGN_STYLE=Automatic \
      DEVELOPMENT_TEAM="$TEAM_ID" \
      -quiet \
      build
  )
  echo "==> 构建完成 $((SECONDS - START))s"
fi

APP="$IOS_DIR/build-device/Build/Products/${CONFIG}-iphoneos/${SCHEME}.app"
if [[ ! -d "$APP" ]]; then
  echo "error: 找不到构建产物 $APP" >&2
  exit 1
fi

APP_MB=$(( $(du -sk "$APP" | awk '{print $1}') / 1024 ))
echo "==> 安装到 $UDID (${APP_MB}MB)，无线安装时这一步最慢"
INSTALL_START=$SECONDS
xcrun devicectl device install app --device "$UDID" --timeout 900 "$APP"
echo "==> 安装完成 $((SECONDS - INSTALL_START))s"

if [[ "$DO_LAUNCH" == 1 ]]; then
  echo "==> 启动 $BUNDLE_ID"
  xcrun devicectl device process launch --device "$UDID" --terminate-existing "$BUNDLE_ID" >/dev/null
fi

echo "==> 全部完成，总耗时 $((SECONDS - START))s"
echo "    首次安装若提示「不受信任的开发者」: 设置 -> 通用 -> VPN 与设备管理 -> 信任 $TEAM_ID"
