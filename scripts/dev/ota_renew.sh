#!/usr/bin/env bash
# ota_renew.sh —— 免费 Apple ID 的 7 天描述文件续期
#
# 问题：`-allowProvisioningUpdates` 只在「没有可用 profile」时才向开发者门户申请新的。
# 只要本地还有一份没到期的缓存，xcodebuild 就会原样复用它，导出包里嵌的还是旧 UUID。
# 于是「每周重新出包」并不会延长手机上的 App 寿命 —— 第 8 天照样打不开。
#
# 对策：在到期前把本地缓存的那份 profile 挪走（不是删掉，留档可回滚），
# 逼 xcodebuild 重新申请一份新的 7 天 profile，再走原有导出流程。
#
# 用法:
#   scripts/dev/ota_renew.sh --check     # 只看剩余时间，不动作（退出码 0=还够用，10=该续了）
#   scripts/dev/ota_renew.sh             # 剩余 < OTA_RENEW_LEAD_HOURS（默认 72h）就续
#   scripts/dev/ota_renew.sh --force     # 立刻续，不看剩余
#   scripts/dev/ota_renew.sh --dry-run   # 只打印将要做什么
#
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
cd "$ROOT_DIR"

OUT="${OTA_OUT_DIR:-$HOME/.crab_feast-ota}"
IPA="${OTA_IPA_PATH:-$OUT/crab_feast-ios.ipa}"
LEAD_HOURS="${OTA_RENEW_LEAD_HOURS:-72}"
PROFILE_DIR="$HOME/Library/Developer/Xcode/UserData/Provisioning Profiles"
BACKUP_DIR="$OUT/.profile-backup"
LOG="$OUT/renew.log"

MODE=auto
for a in "$@"; do
  case "$a" in
    --check)   MODE=check ;;
    --force)   MODE=force ;;
    --dry-run) MODE=dry ;;
    -h|--help) sed -n '2,16p' "$0"; exit 0 ;;
    *) echo "error: 未知参数 $a" >&2; exit 1 ;;
  esac
done

mkdir -p "$OUT"
log() { printf '%s %s\n' "$(date '+%F %T')" "$*" | tee -a "$LOG" >&2; }

# ipa 内嵌 profile -> "UUID|ExpirationDate(ISO)|剩余小时"
read_ipa_profile() {
  python3 - "$1" <<'PY'
import plistlib, subprocess, sys, zipfile, tempfile, os
from datetime import datetime, timezone
ipa = sys.argv[1]
if not os.path.exists(ipa):
    print("MISSING||-"); raise SystemExit(0)
with zipfile.ZipFile(ipa) as z:
    names = [n for n in z.namelist() if n.endswith("embedded.mobileprovision")]
    if not names:
        print("NOPROFILE||-"); raise SystemExit(0)
    raw = z.read(names[0])
with tempfile.NamedTemporaryFile(suffix=".mobileprovision", delete=False) as f:
    f.write(raw); path = f.name
dec = subprocess.run(["security", "cms", "-D", "-i", path], capture_output=True).stdout
os.unlink(path)
d = plistlib.loads(dec)
uid = d.get("UUID", "?")
exp = d.get("ExpirationDate")
if not exp:
    print(f"{uid}||-"); raise SystemExit(0)
# plistlib 会把 profile 里的 "...Z" 读成 naive datetime，直接 astimezone() 会当成
# 本地时间，白白少算 8 小时。这里显式声明它是 UTC 再转。
if exp.tzinfo is None:
    exp = exp.replace(tzinfo=timezone.utc)
exp_utc = exp.astimezone(timezone.utc)
left = (exp_utc - datetime.now(timezone.utc)).total_seconds() / 3600
print(f"{uid}|{exp.astimezone():%Y-%m-%dT%H:%M:%S%z}|{left:.1f}")
PY
}

IFS='|' read -r CUR_UUID CUR_EXP CUR_LEFT <<<"$(read_ipa_profile "$IPA")"

if [[ "$CUR_EXP" == "-" ]]; then
  log "当前 ipa 没有可用 profile（$IPA 不存在或未签名），按需要续期处理"
  LEFT_NUM=-1
else
  LEFT_NUM="${CUR_LEFT%.*}"
fi

log "当前包 profile UUID=$CUR_UUID 到期=$CUR_EXP 剩余=${LEFT_NUM}h（阈值 ${LEAD_HOURS}h）"

case "$MODE" in
  check)
    if [[ "$LEFT_NUM" == "-1" ]] || (( LEFT_NUM < LEAD_HOURS )); then
      echo "RENEW_NEEDED"
      exit 10
    fi
    echo "RENEW_NOT_NEEDED"
    exit 0
    ;;
  auto)
    if [[ "$LEFT_NUM" != "-1" ]] && (( LEFT_NUM >= LEAD_HOURS )); then
      echo "RENEW_NOT_NEEDED"
      exit 0
    fi
    ;;
  dry)
    # 只报告，不动作
    echo "DRY_RUN would: 备份 profile $CUR_UUID -> $BACKUP_DIR, 再跑 ota_publish.sh"
    exit 0
    ;;
  force) ;;
esac

# ---- 0. 设备在场检查 ----
# 免费个人团队重新签发 profile 时，Apple 会回 "Your team has no devices from which
# to generate a provisioning profile"。实测：仅靠无线配对（devicectl 显示 available）
# 也不够，必须用数据线连着，Xcode 才能把 UDID 重新登记进团队。
usb_device_present() {
  system_profiler SPUSBDataType 2>/dev/null | grep -q "iPhone"
}
if ! usb_device_present; then
  log "跳过续期：iPhone 没有用 USB 连接。免费团队重新签发 profile 必须有设备在场（无线连接不算）。"
  # 提醒节流：同一个 12 小时窗口内只弹一次，否则 6 小时巡检会反复骚扰
  STAMP="$OUT/.renew-notified"
  NOW_TS="$(date +%s)"
  LAST_TS="$(cat "$STAMP" 2>/dev/null || echo 0)"
  if (( NOW_TS - LAST_TS > 43200 )); then
    osascript -e 'display notification "插上 iPhone 数据线后跑 scripts/dev/ota_renew.sh --force" with title "Crab Feast OTA 需要插线"' 2>/dev/null || true
    echo "$NOW_TS" > "$STAMP"
  fi
  echo "RENEW_BLOCKED_NO_DEVICE"
  exit 11
fi

# ---- 1. 把缓存里那份 profile 挪走，让 xcodebuild 重新申请 ----
mkdir -p "$BACKUP_DIR"
MOVED=0
MOVED_FILES=()
if [[ -n "${CUR_UUID:-}" && "$CUR_UUID" != "?" && "$CUR_UUID" != "NOPROFILE" && -d "$PROFILE_DIR" ]]; then
  for f in "$PROFILE_DIR"/*.mobileprovision; do
    [[ -e "$f" ]] || continue
    got="$(security cms -D -i "$f" 2>/dev/null | plutil -extract UUID raw -o - - 2>/dev/null || true)"
    if [[ "$got" == "$CUR_UUID" ]]; then
      dest="$BACKUP_DIR/$(basename "$f").$(date +%Y%m%d-%H%M%S)"
      mv "$f" "$dest"
      log "已将缓存 profile 挪到 ${dest}（xcodebuild 将重新申请）"
      MOVED_FILES+=("$dest")
      MOVED=1
    fi
  done
fi
if [[ "$MOVED" == 0 ]]; then
  log "警告：没找到 UUID=$CUR_UUID 的缓存 profile，直接重出包"
fi

# ---- 2. 重出包（archive + exportArchive + 资源快照 + 刷新安装页）----
restore_profiles() {
  local b orig
  for b in ${MOVED_FILES[@]+"${MOVED_FILES[@]}"}; do
    [[ -e "$b" ]] || continue
    orig="$PROFILE_DIR/$(basename "$b" | sed -E 's/\.20[0-9]{6}-[0-9]{6}$//')"
    cp -p "$b" "$orig"
    log "已还原 profile $(basename "$orig")（保证日常构建不受影响）"
  done
}

log "开始重新出包（Release）…"
if ! OTA_PROFILE=release OTA_RENEW_LEAD_HOURS="$LEAD_HOURS" scripts/dev/ota_publish.sh >>"$OUT/renew-build.log" 2>&1; then
  log "续期失败，详情见 $OUT/renew-build.log（服务本身不受影响，旧包仍在线）"
  restore_profiles
  if grep -q "has no devices" "$OUT/renew-build.log"; then
    log "原因：门户侧没有可用设备。确认数据线真的连上（无线不算），并在 Xcode"
    log "      Window → Devices and Simulators 里看到这台 iPhone 处于 Connected。"
  fi
  exit 1
fi

# ---- 3. 校验新包真的换了 profile ----
IFS='|' read -r NEW_UUID NEW_EXP NEW_LEFT <<<"$(read_ipa_profile "$IPA")"
if [[ "$NEW_UUID" == "$CUR_UUID" ]]; then
  log "失败：新包 profile UUID 仍是 ${CUR_UUID}，门户没有签发新的描述文件。"
  log "      排查：Xcode → Settings → Accounts 里这个 Apple ID 是否仍登录；"
  log "      免费团队可能对「7 天内重复申请」有限流，隔几小时再试。"
  restore_profiles
  exit 1
fi

log "续期成功：UUID ${CUR_UUID} → ${NEW_UUID}，到期 ${NEW_EXP}（剩余 ${NEW_LEFT}h）"

# ---- 4. 通知（launchd 下 osascript 能弹系统通知）----
MSG="Crab Feast 已续期，描述文件有效期到 ${NEW_EXP}。手机上点一次安装页即可覆盖安装。"
osascript -e "display notification \"$MSG\" with title \"Crab Feast OTA\"" 2>/dev/null || true

echo "RENEW_OK $NEW_UUID $NEW_EXP"
