# 手机端自动打包 / 装机流水线

目标：**改完代码 → 自动编译 → 手机自己更新 → 直接看效果**，不用每次手动 Xcode、不用连数据线。
这份文档给的是三档递进的方案，先跑最快的，再补 CI。

```
本地改动 ──┐
          ├─ L1 scripts/dev/deploy_ios.sh ── xcodebuild ── devicectl 装机 ── 手机屏幕
          └─ L2 scripts/dev/watch_ios.sh  ── 监听改动，自动跑 L1

git push ── GitHub Actions(self-hosted Mac Studio)
              └─ xcodebuild archive ── .ipa
                 ├─ Rolling Release ── https 直链
                 └─ gh-pages: manifest.plist + 安装页 ── Safari 一点就装
```

## 0. 结论与方案取舍

先说为什么现成工具不顶用，再说为什么这么搭：

| 方案 | 迭代延迟 | 成本 | 对你这个项目的适配度 |
| --- | --- | --- | --- |
| **本机 devicectl 直装（L1）** | 增量 1–3 min | 0 | ★★★★★ 已经在用，只是缺个一键脚本 |
| **本机监听自动装（L2）** | 同上，零操作 | 0 | ★★★★★ 日常开发主力 |
| **自托管 Runner + OTA（L3）** | 5–15 min | 0（自己的机器跑） | ★★★★ 走到哪都能装，页面一点就行 |
| TestFlight | 20–40 min | $99/年 | ★★ 700MB 上传慢，还要等处理 |
| Xcode Cloud | 10–20 min | $12/月起 | ★★ 25 小时/月不够烧，且资源不入库 |
| Firebase App Distribution | 2–5 min | 免费 | ★★★ 只覆盖 Android |
| Vibe2Phone / Expo 一类 | — | — | ✗ 只面向 JS/H5 工程，Rust + xcodebuild 原生壳用不上 |
| AltStore / SideStore | 1–2 min | 0 | ✗ 免费证书 7 天过期，签名要重做，不比直装省事 |

关键差异点：你这个仓库有 **754MB 资源（`assets/characters` 378MB、`assets/source` 345MB）没有入库**，
`third_party/winit` 的 UIScene 补丁和 `Cargo.lock` 也没入库。任何「云端拉 git 仓库再编译」的 SaaS，
编译出来的包一定是缺模型、iOS 27 上直接 SIGTRAP 的。所以 **构建必须发生在能拿到这些本地文件的机器上**——
也就是这台 Mac Studio。它既是最快的构建机，又正好是签名证书所在的地方，自托管 Runner 是唯一不折腾的选择。

> 如果实际场景只是「本机打包 + 人在户外用自己手机看效果」，**不需要 GitHub 这一套**：
> 见 [`remote-preview.md`](./remote-preview.md)，用 Tailscale + 本机 HTTPS OTA，离家也能装。

---

## 1. 前置条件确认

```sh
xcodebuild -version                      # 27.2 已验证可用
rustup target list --installed | grep ios # 需要 aarch64-apple-ios
which cargo-ndk                          # 只有 Android 需要
xcrun devicectl list devices             # 能看到 iPhone 15 Pro 的 physical 设备
```

Xcode 里必须已登录同一个 Apple ID（Team `7JRXM77YNL`），否则 `-allowProvisioningUpdates` 无法自动生成描述文件。

---

## 2. L1：一条命令装到手机

新增脚本：`scripts/dev/deploy_ios.sh`

```sh
scripts/dev/deploy_ios.sh --list           # 看设备
scripts/dev/deploy_ios.sh                  # Debug 构建 + 安装 + 启动
scripts/dev/deploy_ios.sh --release        # Release：包更小，装机明显更快
scripts/dev/deploy_ios.sh --build-only     # 只编译，不动手机
```

它做的事：`xcodebuild -sdk iphoneos -arch arm64 -derivedDataPath build-device`（增量编译，
和之前手动跑 Archive 用的是同一份 DerivedData）→ `xcrun devicectl device install app` →
`devicectl device process launch --terminate-existing`。

> 构建这一段已经在本机实测通过：产物
> `launcher/mobile/ios/build-device/Build/Products/Debug-iphoneos/crab_feast_mobile.app`，
> 988MB，`codesign` 通过（Team `7JRXM77YNL`），`embedded.mobileprovision` 里包含
> 这台 iPhone 15 Pro 的 UDID `00008130-000A55103E2B803A`。

**无线安装**（拔掉数据线也能装）：
Xcode → Window → Devices and Simulators → 选中 iPhone → 勾上 **Connect via network**。
首次勾选要 USB 连一次。之后只要手机和 Mac 在同一 Wi-Fi，脚本自动走无线隧道。
无线装 988MB 的 Debug 包大概 1–3 分钟，USB 约 30–60 秒。

---

## 3. L2：改代码就自动装机

新增脚本：`scripts/dev/watch_ios.sh`

```sh
brew install fswatch          # 可选，装了延迟从 2s 降到 1.5s 内
scripts/dev/watch_ios.sh      # 常驻，Ctrl-C 退出
```

监听 `crab_feast/ crab_feast_library/ crab_feast_ui/ launcher/ assets/ Cargo.toml`，
忽略 `target/ build*/ .git/ dist/ imported_assets/`（否则构建产物会自己触发自己）。
一次保存 → 构建 → 覆盖安装 → 重启 App，全自动。

这是日常开发真正该用的档位：**和 Vibe2Phone 的体验差在「有没有云端」，
而不在「改完多久能在手机上看到」**。

---

## 4. L3：把 Mac Studio 变成 GitHub Actions Runner

### 4.1 注册 runner

GitHub 仓库 → Settings → Actions → Runners → **New self-hosted runner** → macOS。
页面会给出带一次性 token 的命令，形如：

```sh
mkdir -p ~/actions-runner && cd ~/actions-runner
curl -O -L https://github.com/actions/runner/releases/download/<最新版本>/actions-runner-osx-arm64-<版本>.tar.gz
tar xzf actions-runner-osx-arm64-*.tar.gz
./config.sh \
  --url https://github.com/wusidn/crab_feast \
  --token <页面上的 token> \
  --name mac-studio \
  --labels self-hosted,macOS,crab-feast \
  --work _work
```

`crab-feast` 这个自定义 label 对应工作流里的 `runs-on: [self-hosted, macOS, crab-feast]`。
token 一小时内有效，过期回页面重新生成。

### 4.2 常驻

```sh
cd ~/actions-runner
./svc.sh install      # 装成 LaunchAgent（当前登录用户）
./svc.sh start
./svc.sh status
```

**必须用 LaunchAgent（默认就是），不要改成 LaunchDaemon**：
签名证书和私钥在登录用户的 keychain 里，守护进程模式读不到，`xcodebuild` 会报 `No signing certificate`。

### 4.3 别让它睡

系统设置 → 能源 → 打开「防止电脑自动进入睡眠」；或者：

```sh
sudo pmset -a sleep 0 disksleep 0 womp 1
```

Mac 睡了 runner 就掉线；`womp 1` 允许网络唤醒。

---

## 5. 资产库（vault）——这一层不做，CI 一定是坏的

因为下面这些都不在 git 里：

- `assets/characters/`、`assets/source/character/rigs/`（378MB + 345MB）
- `third_party/winit/`（iOS 27 的 UIScene 补丁，缺失 = 启动 SIGTRAP）
- `Cargo.lock`（不入库，每次拉取会重新解析依赖版本）
- `.cargo/config.toml`（里面写的是绝对路径的 winit patch）

做法：把这堆东西镜像到工作区之外的固定目录，CI 每次 checkout 后同步过去。
新增 `scripts/ci/sync_vault.sh`：

```sh
scripts/ci/sync_vault.sh                       # 默认 ~/crab_feast-assets
CRAB_ASSET_VAULT=/Users/cc/crab_feast-assets scripts/ci/sync_vault.sh
```

只需要在**导出了新动画、换了模型、改了 winit 补丁、升级了依赖**之后跑一次。

然后在 GitHub 上设置仓库变量（Settings → Secrets and variables → Actions → **Variables** 标签页）：

| Name | Value |
| --- | --- |
| `CRAB_ASSET_VAULT` | `/Users/cc/crab_feast-assets` |

注意是 Variables 不是 Secrets——它只是本机路径，不需要加密，而且日志里能看见便于排错。

工作流里的同步逻辑（`.github/workflows/mobile-ota.yml` 第 2 步）：

```sh
rsync -a "$CRAB_ASSET_VAULT/assets/" assets/
rsync -a "$CRAB_ASSET_VAULT/winit/" third_party/winit/
cp "$CRAB_ASSET_VAULT/Cargo.lock" Cargo.lock
```

`rsync -a` 按大小+时间戳增量，754MB 在同机 SSD 上通常 2–5 秒，不用每次全量拷。

---

## 6. 流水线做的事

`.github/workflows/mobile-ota.yml`，触发条件：`main` 分支上改到代码/原生壳/资源清单，或手动
`Run workflow`（可选 `debug`/`release`，可选是否顺带出 Android APK）。

| 步骤 | 说明 |
| --- | --- |
| checkout | `clean: false`——否则 `git clean` 会删掉 `target/` 和 `build-device/`，每次都冷编译 |
| vault 同步 | 见上一节 |
| 版本号 | `CFBundleVersion = github.run_number`，`CFBundleShortVersionString = 1.0.0` |
| iOS | `xcodebuild archive` + `exportArchive`（`launcher/mobile/ios/ExportOptions.plist`，`method=development`，自动签名） |
| Android | `cargo ndk -t arm64-v8a` + `./gradlew assembleDebug/Release -PcrabVersionCode=<run_number>` |
| OTA 产物 | `scripts/ci/ota_manifest.py` 生成 `manifest.plist` + `index.html` |
| 分发 | 覆盖写 `mobile-latest` Release 里的固定文件名；页面推到 `gh-pages` |

固定文件名是为了让安装链接永远不变：

- `https://github.com/wusidn/crab_feast/releases/download/mobile-latest/crab_feast-ios.ipa`
- `https://github.com/wusidn/crab_feast/releases/download/mobile-latest/crab_feast-android.apk`
- 安装页 `https://wusidn.github.io/crab_feast/`
- 清单 `https://wusidn.github.io/crab_feast/manifest.plist`

每次构建在链接后面自动加 `?v=<short_sha>` 规避 GitHub CDN 缓存读到旧包。

Android 版本号在 `launcher/mobile/android/app/build.gradle` 里改成可注入：

```gradle
versionCode ((project.findProperty("crabVersionCode") ?: "1") as String).toInteger()
versionName ((project.findProperty("crabVersionName") ?: "1.0") as String)
```

本地不带参数时行为和原来完全一致。

---

## 7. 打开 GitHub Pages

仓库 Settings → Pages → **Deploy from a branch** → Branch `gh-pages` / `/ (root)` → Save。
分支要等第一次工作流跑完才会由 `peaceiris/actions-gh-pages` 创建出来。
仓库是公开的，Pages 免费用；私有仓库要 Pro 才能开 Pages。

---

## 8. 手机上安装

1. iPhone 用 **Safari**（微信/Chrome 内置浏览器不行）打开 `https://wusidn.github.io/crab_feast/`。
2. 点「安装到 iPhone」→ 系统弹窗确认 → 回到桌面看图标转圈。
3. 首次装完：设置 → 通用 → VPN 与设备管理 → 开发者 App → 信任 `7JRXM77YNL`。
4. 之后再装就是直接覆盖，不需要重复信任。

**装不上先查这三条**：

- 设备的 UDID 必须在描述文件里。最省事的办法是**用数据线连一次 Xcode**，自动签名会把该设备登记进去；
  也可以去 developer.apple.com → Certificates, Identifiers & Profiles → Devices 手动加。
  取 UDID：`xcrun devicectl list devices --json-output -` 里 `properties.hardware.udid`，或 Finder 点设备名下方那行。
- 描述文件过期（见下一节，这是目前最大的坑）。
- manifest 必须是 **HTTPS + 受信任证书**，这也是为什么走 GitHub Pages 而不是本机起个 http 服务。

### 8.1 证书现实：当前是 7 天描述文件

用刚构建出来的真机包查一下：

```sh
security cms -D -i launcher/mobile/ios/build-device/Build/Products/Debug-iphoneos/crab_feast_mobile.app/embedded.mobileprovision \
  | plutil -extract ExpirationDate raw -o - -
# 2026-11-28T06:10:02Z
security cms -D -i launcher/mobile/ios/build-device/Build/Products/Debug-iphoneos/crab_feast_mobile.app/embedded.mobileprovision \
  | plutil -extract ProvisionedDevices json -o - -
# ["00008130-000A55103E2B803A"]
```

`ExpirationDate` 只有 7 天 = 现在这个 Team 用的是**免费 Apple ID 的个人团队**。
免费团队的限制会直接决定这份流水线能「自动」到什么程度：

| 限制 | 影响 | 对策 |
| --- | --- | --- |
| 描述文件 7 天过期 | 手机上的 App 到第 8 天就打不开，跟 CI 跑没跑没关系 | 每周重装一次；或升级付费账号 |
| 同一台设备最多 3 个自签 App | 装多了要删旧的 | 只留 crab_feast 一个 |
| 无法用 TestFlight / 推送 / Ad Hoc | 分发只能走 development 描述文件 | 要给别人测就必须付费 |

两种走法：

1. **先按免费跑通全链路**（推荐，零成本验证方案是否可用）：手机每周重装一次。可以给工作流加一条
   定时触发，让它每 5 天自动出一个新包，你点一下安装页就行，省得手动跑：
   ```yaml
   on:
     schedule:
       - cron: "0 2 */5 * *"   # 每 5 天 02:00 UTC
   ```
   注意定时构建只解决「包是新的」，**手机上那次点击仍然要你自己做**。
2. **升级到 99 美元/年的付费账号**：描述文件变 1 年，装一次管一年，同一套流水线不用改任何代码；
   顺带解锁 TestFlight（给别人测、90 天有效期、不用管 UDID）。

这是整条链路里唯一花钱的地方，其他环节都是零成本。

---

## 9. Android（可选）

本机目前没装 `adb`。要用的话：

```sh
brew install --cask android-platform-tools
adb pair <手机IP>:<配对端口>          # 开发者选项 → 无线调试 → 使用配对码配对
adb connect <手机IP>:<调试端口>
adb install -r launcher/mobile/android/app/build/outputs/apk/debug/app-debug.apk
```

想要「手机上自动发现新版本、点一下就升级」：装 **Obtainium**，添加仓库
`https://github.com/wusidn/crab_feast`，只匹配 `crab_feast-android.apk` 这个资产。
配合上面的 `versionCode` 自增，每次流水线跑完手机就会提示更新。

---

## 10. 故障对照表

| 现象 | 原因 | 处理 |
| --- | --- | --- |
| `devicectl` 报 `device is locked` | 手机锁屏 | 解锁保持亮屏，重跑 |
| `No signing certificate "Apple Development"` | runner 以 LaunchDaemon 运行 / Xcode 没登录 | `./svc.sh uninstall` 后用 LaunchAgent 重装；Xcode 里重新登录 Apple ID |
| `xcodebuild: error: Unable to find a destination` | 传了 `devicectl` 的 UUID 给 `-destination` | 构建阶段不要传 `-destination`，用 `-sdk iphoneos -arch arm64` |
| 真机启动即闪退（iOS 27） | 没有 `third_party/winit` 的 UIScene 补丁 | 确认 vault 同步过，`git status` 里 `third_party/winit` 有内容 |
| 游戏里模型/动画全丢 | vault 里没有 `assets/characters` | 重跑 `scripts/ci/sync_vault.sh` |
| 手机点安装提示「无法安装，请稍后再试」 | 非 HTTPS / 设备 UDID 不在描述文件 / 证书过期 | 逐条对照上一节 |
| 流水线每次都是冷编译，20 分钟起步 | checkout 的 `clean: true` 清掉了缓存 | 保持 `clean: false` |
| Pages 打开是 404 | 还没跑过工作流 / Pages 分支没设成 `gh-pages` | 先跑一次，再检查 Pages 设置 |

---

## 11. 体积与速度

当前 Debug 包：ipa 711MB、解压后 app 988MB，其中 `assets/` 占 754MB。
装一次无线要 1–3 分钟，这部分是纯带宽，编译优化救不了，只能减体积：

1. **切 Release**：`Run workflow` 时选 `release`，Bevy 的 debug 符号和未优化代码去掉后体积明显下降。
2. **别把 `assets/source` 打进包**：这 345MB 是 Blender 导出用的 FBX 源文件，运行时用不到。
   在 CI 的 vault 同步后加一行 `rm -rf assets/source`（只作用于 runner 的临时代码目录），
   包体直接砍掉三分之一。本地开发想保留就别动本地目录。
3. **符号表分离**：需要崩溃栈时再单独留 dSYM，不要塞进 app。

编译速度方面，自托管的好处是 `target/` 和 `build-device/` 一直留着：
首次冷编译 15–20 分钟，之后增量通常 2–5 分钟。云端 runner 每次都是冷编译，这也是不用云的原因之一。

---

## 12. 两个安全提醒

- `launcher/mobile/game_mobile.jks` 和 `android_key.txt` 是**连私钥带明文密码提交进公开仓库**的。
  任何人都能用你的签名打 Android 包、冒充你的应用升级。建议轮换 keystore，密码改走
  `-PstorePassword` 或环境变量，仓库里只留占位。
- iOS 侧不需要任何 GitHub Secrets：签名完全依赖这台 Mac 的 keychain 和 Xcode 登录态，
  这是自托管方案的额外好处，但也意味着**能在这台机器上执行代码的人就能签你的包**。

---

## 附：新增文件清单

```
.github/workflows/mobile-ota.yml        流水线
scripts/dev/deploy_ios.sh               一键构建+装机
scripts/dev/watch_ios.sh                监听自动装机
scripts/ci/sync_vault.sh                本地资源 -> 资产库
scripts/ci/ota_manifest.py              生成 manifest.plist / 安装页
launcher/mobile/ios/ExportOptions.plist 导出 ipa 的签名参数
docs/ci/mobile-cicd.md                  本文档
```
