# 户外用手机看效果

场景：**打包一直在这台 Mac 上跑，测试机只有自己那台 iPhone，人在外面时也想看到最新效果。**

## 0. 先回答两个问题

### 公网 OTA 需要公网 IP 吗？不需要

OTA 的本质是**手机主动发起的一次 HTTPS 下载**：

1. Safari 打开一个 `https://` 页面；
2. 页面里 `itms-services://?action=download-manifest&url=...` 指向一个 `manifest.plist`；
3. 系统安装器自己去 `GET` 那个 ipa。

全程是**客户端 → 服务器**的单向连接，没有任何「服务器反向连手机」的环节。
所以只要那个 HTTPS 地址在手机上能解析、能连上、证书受信任，就算成功——**跟你家有没有公网 IP 完全无关**。
反过来，也正因为是手机主动连，NAT 打洞才有意义：**先让手机能连到 Mac，再让 Mac 讲正常的 HTTPS**。

### Vibe2Phone 的打洞方式能用吗？可以，而且它就是这个方案

查到的 Vibe2Phone 实现就是「Tailscale 建私有隧道 + 本机起 HTTP 服务 + OTA 安装」，
没有别的魔法。也就是说**你要的东西和我上一轮给的是同一套架构**，区别只在 TLS 由谁终止：

| 环节 | Vibe2Phone | 这里 |
| --- | --- | --- |
| 通道 | Tailscale（打洞直连 / DERP 中继兜底） | 同 |
| TLS | 由 Tailscale 自动签发并终止 | `tailscale serve`，同 |
| 分发 | itms-services + manifest.plist | 同（`ota_manifest.py` 生成） |
| 触发 | 它自己的 CLI | 快捷指令 SSH 进来跑 `ota_publish.sh` |

但要注意：**裸的 P2P 打洞本身不能直接给 OTA 用**。iOS 安装器只认「DNS 名 + 受信任证书 + HTTP」，
一个打洞出来的裸 socket 既没有域名也没有公网 CA 签的证书，系统安装器根本不接。
打洞只是把 IP 层打通，真正让 OTA 成立的是 Tailscale 附带的 **MagicDNS + Let's Encrypt 证书 + 标准 HTTPS**。

所以「用打洞」的正确形态就是 Tailscale 这种：**打洞负责连通，TLS 负责被系统信任**。

### 三种对外暴露方式，选哪个

| 方式 | 命令 | 谁能装 | 需要公网 IP | 带宽 |
| --- | --- | --- | --- | --- |
| **tailnet 内（默认，推荐）** | `ota_serve.sh --start` | 只有装了 Tailscale 的设备 | 否 | 直连打洞，基本跑满你家上行 |
| 公网 Funnel | `ota_serve.sh --start --public` | 任何人，不需要装 Tailscale | 否 | **有不可调的带宽上限**（官方未公布数值） |
| 对象存储 / GitHub Release | 上一份文档的 CI 方案 | 任何人 | 否 | 不限速，但要先把包传上去 |

你只有自己一台手机 → 用第一种。手机装个 Tailscale（免费、后台常驻、耗电可忽略），
从此它在任何网络下都算「在这个私有网里」，OTA 地址不变、证书有效、不用开 Funnel。

> Funnel 适合「临时给没装 Tailscale 的朋友装一下」这种小流量场景。
> 400MB 的包走 Funnel 很可能被限速卡住，官方明说带宽限制不可配置、加钱也不能提。

## 1. Tailscale 装好、开证书

```sh
brew install --cask tailscale     # Mac
# iPhone：App Store 装 Tailscale，登录同一个账号
```

```sh
# 看自己的 tailnet 名字和地址
/Applications/Tailscale.app/Contents/MacOS/Tailscale status --json | \
  python3 -c 'import json,sys; d=json.load(sys.stdin); print(d["Self"]["DNSName"], d["Self"]["TailscaleIPs"][0])'
```

再在 **Tailscale admin → DNS → HTTPS Certificates** 打开开关（免费额度内）。
`tailscale serve` / `tailscale cert` 都依赖它，不开会直接报错。

## 2. 出包

```sh
scripts/dev/ota_export.sh --release --lean
```

- `--release`：Release 编译，二进制从 Debug 的 246MB 降下来。
- `--lean`：构建期间把 `assets/source`（345MB 的 Blender FBX，运行时用不到）临时挪走，
  结束**无条件还原**（trap 兜底，Ctrl-C 也还原）。这是省流量最有效的一刀。
- 产物落在 `~/.crab_feast-ota/`：`crab_feast-ios.ipa`、`manifest.plist`、`index.html`。

| 组合 | 构建 | 手机要下载 |
| --- | --- | --- |
| Debug | 2–4 min | ~711MB |
| Release + lean | 5–15 min | ~400MB（剩下 378MB 是角色资源，压不动） |

## 3. 起服务

```sh
scripts/dev/ota_serve.sh --start           # 仅 tailnet（推荐）
scripts/dev/ota_serve.sh --status
scripts/dev/ota_serve.sh --stop
```

内部就两件事：

```sh
# 1) 本机明文源，只绑回环（crab-feast-web 的 OTA 端口）
cargo run -p crab_feast_web -- --ota-addr 127.0.0.1:8080 --no-registry
# 2) 让 tailscaled 做 TLS 终止 + 证书自动续期，8443 是 Funnel 允许的端口之一
tailscale serve --bg --https=8443 http://127.0.0.1:8080
```

对外就是 `https://<mac>.<tailnet>.ts.net:8443/`，证书是 Let's Encrypt 的真证书，
`tailscaled` 负责续期，你不需要管 `.crt/.key`。加密链路是端到端的（Tailscale 节点之间），
本机只监听 127.0.0.1，局域网和公网都碰不到那个明文端口。

开机自启（LaunchAgent）：

```sh
cat > ~/Library/LaunchAgents/com.crab.feast.ota.plist <<'EOF'
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0"><dict>
  <key>Label</key><string>com.crab.feast.ota</string>
  <key>ProgramArguments</key><array>
    <string>/bin/bash</string>
    <string>/Users/cc/workspace/projects/crab_feast/scripts/dev/ota_serve.sh</string>
    <string>--start</string>
  </array>
  <key>RunAtLoad</key><true/>
  <key>KeepAlive</key><false/>
  <key>StandardOutPath</key><string>/Users/cc/.crab_feast-ota/launchd.log</string>
  <key>StandardErrorPath</key><string>/Users/cc/.crab_feast-ota/launchd.err</string>
</dict></plist>
EOF
launchctl load ~/Library/LaunchAgents/com.crab.feast.ota.plist
```

如果 `tailscale serve` 在你的 macOS 版本上不工作（App Store 版的沙箱偶尔会挡），
用兜底模式，自己申请证书并直接终止 TLS：

```sh
scripts/dev/ota_serve.sh --start --direct
```

## 4. 手机上安装

Safari 打开 `https://<mac>.<tailnet>.ts.net:8443/`，点「安装到 iPhone」。
Safari → 分享 → **添加到主屏幕**，以后桌面一个图标。

首次装完要在 设置 → 通用 → VPN 与设备管理 里信任开发者证书；同 bundle id 覆盖安装不用再信任。

## 5. 户外一键：快捷指令 SSH 触发

```sh
sudo systemsetup -setremotelogin on       # 或 系统设置 → 通用 → 共享 → 远程登录
ssh-keygen -t ed25519 -f ~/.ssh/crab_phone -N "" -C "phone-shortcut"
cat ~/.ssh/crab_phone.pub >> ~/.ssh/authorized_keys
```

想把这把钥匙的能力锁死到「只能发版」，在 `authorized_keys` 里改成强制命令：

```
command="/Users/cc/workspace/projects/crab_feast/scripts/dev/ota_publish.sh",no-pty,no-agent-forwarding,no-port-forwarding ssh-ed25519 AAAA... phone-shortcut
```

iPhone「快捷指令」建两个动作：

1. **通过 SSH 运行脚本**
   - 主机 `<mac>.<tailnet>.ts.net`，端口 `22`，用户 `cc`
   - 认证用 SSH 密钥，粘贴 `~/.ssh/crab_phone` 的私钥
   - 脚本填 `/Users/cc/workspace/projects/crab_feast/scripts/dev/ota_publish.sh`（已配强制命令就留空）
2. **打开 URL**：`https://<mac>.<tailnet>.ts.net:8443/`

放主屏：点一下 → Mac 编译 → 网页刷新 → 点安装。
`ota_publish.sh` 默认 Release+lean；想换 Debug 就 SSH 里跑 `OTA_PROFILE=debug scripts/dev/ota_publish.sh`。

## 6. 直连还是走中继？先量一下

打洞不成功时 Tailscale 会退回 DERP 中继，**400MB 走中继会明显慢**：

```sh
/Applications/Tailscale.app/Contents/MacOS/Tailscale ping <iphone 主机名>
# "via DERP(tok)" = 走中继；  "direct" / "via 192.168.x.x" = 打洞成功
```

- 直连：速度基本等于你家上行带宽（100Mbps 上行 → 400MB 约 35 秒）。
- 中继：几 Mbps 到十几 Mbps，400MB 可能要几分钟。

蜂窝网络大量用 CGNAT，直连不一定成功。如果长期走 DERP，两条路：
把包再压小（`--lean` + Release 已经是底线），或者把 ipa 放对象存储/GitHub Release 走 CDN。
另外注意手机流量：400MB 一次，一周一次，自己算套餐够不够。

## 7. 常见问题

| 现象 | 原因 | 处理 |
| --- | --- | --- |
| `tailscale serve` 报错 / 404 | admin 没开 HTTPS Certificates | 打开后重跑 `--start` |
| Safari 打不开，但 Mac 上 curl 正常 | 手机没连 Tailscale，或登了别的账号 | 手机打开 Tailscale，确认同一 tailnet |
| 点安装提示「无法安装，请稍后再试」 | 证书过期 / UDID 不在描述文件 | 见第 8 节；USB 连一次 Xcode 让自动签名登记设备 |
| 下载中途断了要重来 | 服务不支持 Range | 当前 `crab-feast-web` 已实现 206 续传 |
| 安装页能开但点安装没反应 | 用微信/Chrome 打开了 | 必须 Safari |
| 出门前好好的，晚上打不开 App | 描述文件 7 天到期 | 重装一次即可 |
| Mac 睡了没响应 | 系统休眠断网 | `sudo pmset -a sleep 0 disksleep 0 womp 1` |

## 8. 七天：你接受的那条

当前真机包描述文件到期 **2026-11-28**（免费 Apple ID 个人团队）。手机上已装的 App 到第 8 天
直接打不开，跟有没有重新打包无关，只能重装。每周打开安装页点一次即可，Mac 侧全自动。
哪天嫌烦，99 美元/年换付费账号，描述文件变 1 年，这套流程一行代码不用改。

## 9. 补充：只想瞄一眼画面

不用等打包，Tailscale + 屏幕共享连回 Mac 跑模拟器：

```sh
# Mac：系统设置 → 通用 → 共享 → 屏幕共享
# iPhone：Jump Desktop / Screens，或 Safari 打开 vnc://<mac>.<tailnet>.ts.net
xcrun simctl boot "iPhone 18 Pro" || true
cd launcher/mobile/ios && xcodebuild -scheme crab_feast_mobile -configuration Debug \
  -derivedDataPath build -destination "id=<模拟器 UDID>" -quiet
xcrun simctl install booted build/Build/Products/Debug-iphonesimulator/crab_feast_mobile.app
xcrun simctl launch booted org.crab.feast
```

即连即看、不签名、不受 7 天限制、不耗流量；代价是模拟器手感，不是真机。

## 10. 国内路径：IPv6 直连能不能用

Tailscale 的可用性问题是结构性的（控制面与 DERP 都在境外，高峰期建连抖动），不适合当唯一通道。
三条不依赖境外的路，按代价从低到高：

| 路径 | 代价 | 前提 | 结论 |
| --- | --- | --- | --- |
| 家宽 IPv6 直连 + DDNS | 0 元 | 运营商允许入站、路由器放行 8443 | 首选，本文档默认 |
| 花生壳 HTTPS 映射 | 58 元/年（加密壳）起 | 无需公网 IP | 入站被封时的兜底 |
| 境内轻量云 + frp | ~30 元/月起 | 有公网 IPv4 | 最稳，但要过一层中继带宽 |

### 10.1 先把「谁在拦」测清楚

本机实测（2026-11-21）：

- Mac 上有全球单播 `2409:8a3c:424:5ff0:…/64`（中国移动段，`secured` 稳定地址），v6 默认路由正常，
  `socketfilterfw` **防火墙是关的** —— 所以拦截点不在 Mac。
- 路径是双 NAT：`192.168.0.1`（TP-LINK）→ `192.168.1.1`（光猫）→ `100.86.0.1`（运营商 CGNAT）。
- IPv6 第一跳是 TP-LINK 在同 /64 内的地址 → **TP-LINK 才是这个前缀的 v6 路由器**，它的 IPv6 防火墙是第一道门；
  光猫若也在 PPPoE 上，还有第二道。

结论：**"只能下行"绝大多数不是运营商封的**，而是路由器/光猫的 IPv6 防火墙默认丢弃未经允许的入站 SYN。
TCP 80/443 在 IPv4 侧确实会被运营商封，所以直连一律用 `8443`，别用 80/443。

### 10.2 判定实验（已实测：入站可用）

需要一台**真正在外网**的机器发起连接；本机的 curl、本机内置抓取服务都是本地回路，测了不算；
本项目的云主机环境也没有 IPv6（只有 `fe80::`），同样不能当起点。唯一可靠起点是手机蜂窝。

2026-11-21 实测流程与结果：

```sh
# Mac：绑定全部地址起一个最简监听
python3 -m http.server 18080 --bind ::
```

iPhone 关 Wi-Fi、只用 5G，Safari 打开 `http://[2409:8a3c:424:5ff0:1cda:21aa:9ba1:d1cd]:18080/hello.txt`
→ **返回 `INBOUND_OK`**。

结论：**家宽 IPv6 入站没有被运营商封**，TP-LINK 与光猫（IPv6 桥模式，只做二层透传）也都不拦。
「只能下行」如果再次出现，八成是端口用了 80/443、手机没拿到 v6、或地址用了旧的临时地址。

### 10.3 落地流程：DDNS + 证书（脚本已就位）

OTA 要求 HTTPS + 受信任证书 + 域名，IPv6 裸地址不行（Let's Encrypt 不签 IP，iOS 也不认）。
家宽前缀会随重拨漂移，所以域名指向必须自动维护。

**选一种域名来源：**

| 方案 | 成本 | 需要的东西 |
| --- | --- | --- |
| DuckDNS | 免费 | **国内基本不可用**：注册要过 Google reCAPTCHA，打不开 |
| DNSPod 免费版 + 自购域名 | 域名 14 元/年起 | `DP_LOGIN_TOKEN`（`id,key`）、`OTA_DNS_ZONE` |
| 阿里云解析 + 自购域名 | 域名 35 元/年起 | `ALI_ACCESS_KEY_ID` / `ALI_ACCESS_KEY_SECRET`、`OTA_DNS_ZONE` |
| Cloudflare | 域名费 | `CF_API_TOKEN`（Zone.DNS 编辑权限），账号注册也可能卡验证码 |

为什么绕不开「买域名」：OTA 要的是**受信任证书**，Let's Encrypt 的 DNS-01 必须能改 TXT 记录。
国内免费 DDNS（花生壳免费体验域名、nat123 免费二级域名）基本只做 IPv4 地址解析，不开放 TXT 接口，
签不出证书；花生壳/贝锐的 DDNS 客户端也主要面向 IPv4 公网。所以唯一「全免费 + 可签证书」的组合不存在，
最省的是十几块的域名 + 免费解析。

### 10.3.1 DNSPod 具体走法（推荐，14 元起）

1. **买域名**：腾讯云域名注册挑个 `.top`（首年 14 元，续费 34 元/年），完成实名认证。
2. **解析**：腾讯云买的域名默认托管在 DNSPod，免费版就够（不限记录数、支持 API、支持 AAAA，TTL 下限 600s）。
3. **拿 Token**：DNSPod 控制台 → API 密钥 → 新建密钥，得到 `ID` 和 `Token`。
   注意要选 **DNSPod Token**，不是腾讯云 API 密钥（那是另一套签名，`dns_tencent` 用的）。
4. **写配置**：

```sh
# ~/.crab_feast-ota/ota.env
OTA_HOST=ota.example.top
OTA_DNS_PROVIDER=dnspod
DP_LOGIN_TOKEN=<ID>,<Token>
OTA_DNS_ZONE=example.top
```

5. **跑三条命令**（同 10.3）：`ota_ddns.py --check` → `ota_cert.sh` → `ota_serve.sh --ipv6 --start`。

两个 DNSPod 特有的坑，代码里已经处理：

- **TTL 最低 600 秒**（免费版限制），`ota_ddns.py` 会按 provider 自动抬到 600，填 60 会被 API 拒。
  代价是运营商换前缀后，手机上最多有 10 分钟还在用旧地址。
- **记录必须带线路**，修改时要回填 `record_line_id`，否则报「记录不存在」。

`ota_cert.sh` 走的是 acme.sh 的 `dns_dp`（用 ID+Token），不是 `dns_tencent`（用 SecretId/SecretKey），别混。

以 DuckDNS 为例，先把配置写进**唯一配置点**（SSH 强制命令与 cron 都不会读你交互 shell 的 export，
所以不要依赖 `export`）：

```sh
mkdir -p ~/.crab_feast-ota
cp scripts/dev/ota.env.example ~/.crab_feast-ota/ota.env
chmod 600 ~/.crab_feast-ota/ota.env
# 编辑它：OTA_HOST / OTA_DNS_PROVIDER / DUCKDNS_TOKEN
```

然后三步走完（都从 `ota.env` 读配置，不用再传环境变量）：

```sh
cd /Users/cc/workspace/projects/crab_feast
python3 scripts/dev/ota_ddns.py --check   # 期望 DDNS_DRIFT（DNS 还是旧值）或 DDNS_NOCHANGE
scripts/dev/ota_cert.sh                   # 自动装 acme.sh，DNS-01 签发，装到 ~/.crab_feast-ota/certs
scripts/dev/ota_serve.sh --ipv6 --start   # 启动前自动跑 DDNS、校验证书 SAN、回环自检
```

`ota_serve.sh --ipv6 --start` 现在会做三件事，任何一件不过就直接报错，不用等手机试：

1. 证书 SAN 必须包含 `OTA_HOST`（macOS 自带 LibreSSL 没有 `-ext`，脚本从 `text` 输出里抠）；
2. 证书 3 天内到期 → 提示续期；
3. 设置 `OTA_DNS_PROVIDER` 时自动跑一次 DDNS，把 AAAA 对齐到当前 `secured` 地址；
4. 起服务后自检 `https://[::1]:8443/`，返回非 200 就告警。

**前缀漂移的兜底 cron（每 5 分钟比一次，脚本自己会读 `ota.env`）：**

```sh
crontab -e
*/5 * * * * /usr/bin/python3 /Users/cc/workspace/projects/crab_feast/scripts/dev/ota_ddns.py >> $HOME/.crab_feast-ota/ddns.log 2>&1
```

证书 90 天，acme.sh 安装时自带续期 cron，续期后 `--reloadcmd` 会自动重启 OTA 服务，不用管。

> acme.sh 的官方安装脚本 `get.acme.sh` 会去拉 `raw.githubusercontent.com`，国内实测被 connection reset。
> `ota_cert.sh` 已改成从 **Gitee 镜像整仓 clone**（`gitee.com/neilpang/acme.sh`）后 `--install`，
> 因为单文件版不带 `dnsapi/` 插件，会报 `Cannot find DNS API hook for: dns_dp`。

**日常发布仍然是那一条：**

```sh
scripts/dev/ota_publish.sh     # 构建 + 导出 + 重启服务（配置从 ota.env 读）
```

注意：手机在只给 IPv4 的 Wi-Fi（咖啡店、公司网）打不开，只有蜂窝 5G 才行；
`OTA_IFACE` 默认 `en1`，走有线改 `en0`。

### 10.3.2 每年换一个域名（省续费钱的操作单）

首年 10 元、续费 66 元，如果对域名本身没要求，完全可以到期丢弃、重新注册一个没被占用的名字。
促销价按「新注册」算，要求是**这个域名还没被注册过**；部分促销限账号首单，以结算页为准。

换域名不影响已经装在手机上的 App——覆盖安装看的是 bundle id，不是域名。域名只决定「下次从哪里下载」。
真正要重做的有四件：

```sh
# 1. 新域名先做实名认证（不做则解析不生效，通常当天到次日）

# 2. 改 ~/.crab_feast-ota/ota.env 两行
#    OTA_HOST=ota.new-name.icu
#    OTA_DNS_ZONE=new-name.icu

# 3. 建/改 AAAA
python3 scripts/dev/ota_ddns.py --check

# 4. 重签证书（acme.sh 会新增一条证书记录，旧的可留着一年）
scripts/dev/ota_cert.sh

# 5. 重启服务（会自动加载新证书、重跑 DDNS 与 SAN 校验）
scripts/dev/ota_serve.sh --ipv6 --restart

# 6. 手机上改一处：快捷指令 / 主屏书签里的安装页 URL

# 7. 可选：清掉旧域名的证书，免得 acme.sh 续期时刷失败日志
~/.acme.sh/acme.sh --remove -d ota.old-name.icu --ecc
```

省下来的钱与代价对比：

| 做法 | 年成本 | 代价 |
| --- | --- | --- |
| 每年换新域名（`.icu`） | 10 元 | 每年重做实名 + 改两行配置 + 重签证书 + 改手机 URL，约 10 分钟 |
| `.icu` 续费 | 66 元 | 什么都不用动 |
| `.top` 续费 | 34 元 | 什么都不用动（`.top` 首年 14，换名也不亏） |

如果只是不想每年折腾，`.top` 续费 34 元/年是性价比更高的一档；真想压到 10 元，就按上面的清单走，脚本这边除 `ota.env` 外零改动。

### 10.3.3 阿里云走法（腾讯云实名没过时的替代）

DNS 换到阿里云解析，功能上完全等价：免费版支持 AAAA、TTL 下限 600 秒、开放 API。

1. **买域名**：阿里云域名注册选 `.top` / `.icu` / `.com`（**别买 `.cn`**）。阿里云买的域名默认 NS 指向阿里云云解析，
   不用改 NS。注意阿里云有「1 元注册」活动，但要搭配买 ESA 基础版，别为了 1 元多买一个服务。
2. **实名认证**：阿里云同样强制校验。腾讯云被驳回要先查原因（常见：证件照片不合格、姓名与证件号不一致、
   同一证件实名账号数超限），**同样的资料在阿里云大概率也过不了**，别盲目重复提交。
3. **拿 AK/SK**：RAM 访问控制 → 新建子账号 → 只授予 `AliyunDNSFullAccess`（最小权限，别用主账号 AK），
   记下 AccessKeyId / AccessKeySecret。
4. **写配置**：

```sh
# ~/.crab_feast-ota/ota.env
OTA_HOST=ota.example.top
OTA_DNS_PROVIDER=alidns
ALI_ACCESS_KEY_ID=LTAI...
ALI_ACCESS_KEY_SECRET=...
OTA_DNS_ZONE=example.top
```

5. **三条命令**（同 10.3）：`ota_ddns.py --check` → `ota_cert.sh` → `ota_serve.sh --ipv6 --start`。

**域名买完会卡在 `clientHold`（2026-11-21 实测 `qiaqia.cyou`）：** 这个状态是**注册商锁**，不是注册局在排队审核，
两种原因最常见，都要自己去点：

1. **域名持有者邮箱未验证**：注册时会发一封验证邮件，不点链接就一直 `clientHold`。别用公共邮箱（QQ/163）当持有者邮箱。
2. **未提交实名认证信息模板**：阿里云按境内监管要求，注册后 **5 天内**必须交证件资料；审核通常 1 天、最多 3–5 个工作日，
   通过后 **1–2 个工作日**才解锁。逾期会转成 `serverHold`。

验证方式：`whois qiaqia.cyou | grep -i status`，出现 `clientHold` 就说明解析一定不生效（`dig` 会是 NXDOMAIN），
此时跑 `ota_ddns.py` 只会得到 `DDNS_DRIFT`，跟脚本无关。

阿里云特有的两个细节，脚本里已处理：

- **错误码不走 HTTP 200**：鉴权失败时网关返回 `404` + JSON（`InvalidAccessKeyId.NotFound`，
  权限不足是 `Forbidden.RAM`）。`http_json()` 会把 `Code` / `Message` 原文透出来，不会只剩一句 `404 Not Found`。
- **偶发 connection reset**：阿里云 API 对短时间重复请求会 reset，`http_json()` 已加 3 次退避重试。
- 证书侧 acme.sh 用 `dns_ali`，需要 `Ali_Key` / `Ali_Secret`，`ota_cert.sh` 自动从 `ALI_ACCESS_KEY_ID/SECRET` 映射。

### 10.4 不通：按顺序排除

本机这条路已经实测通（10.2），TP-LINK 与光猫**都不用改**。只有换网络或换了路由器之后再不通，才按下面查：

1. **本机服务活着吗**：`scripts/dev/ota_serve.sh --status`，看 host / DNS / cert / 端口四项；
   再 `curl -k -I "https://[::1]:8443/"`，回环不通就是服务没起来，跟外网无关。
2. **地址漂了没**：`ifconfig en1 | grep secured` 与 `python3 scripts/dev/ota_ddns.py --check`。
   前缀变了而 DNS 没跟上 → `DDNS_DRIFT`，跑一次 `ota_ddns.py` 即可。
3. **手机侧**：iPhone 必须走蜂窝（Wi-Fi 多为 IPv4-only），最稳的验证是 `ping6` 或直接开安装页。
4. **用外部起点测，别用本机**：本机 curl 和平台抓取都走本地回路，必然「通」；云主机没有 IPv6。
   唯一可靠起点是手机蜂窝。
5. **换过路由器才需要看防火墙**：TP-LINK `TL-R470GP-AC` 面板里没有 IPv6 防火墙项（实测），
   光猫是 IPv6 桥模式，两台都不做过滤；如果换成别的新固件才出现「IPv6 防火墙」，先关它。
6. 服务、地址、手机都对，仍然只能下行不能入站 → 运营商侧限制，这条路径作废，走花生壳或 frp 兜底。

如果最后落到兜底：**花生壳**买「加密壳域名」58 元/年，映射本机 8443，它自带证书，manifest 里的 URL 换成它的域名即可，
`crab-feast-web` 照旧只听 127.0.0.1，前端由花生壳转发；**frp** 则在同一台境内 VPS 上跑 `frps`，Mac 跑 `frpc` 把 8443 映射出去，
再用 acme.sh 在那个 VPS 上签证书。

### 10.5 备案：这套方案不需要，也办不了

三条事实，别被「买域名必须备案」带偏：

1. **备案不在 DNS 服务商办**。工信部流程是「在互联网接入服务企业侧系统申请」——也就是托管服务器的那家云厂商。
   腾讯云自己的文档写明：可备案的前提是「网站/APP 托管在腾讯云中国大陆地区的云服务器 CVM、轻量应用服务器 Lighthouse 等云服务上」。
   DNSPod 只做 DNS，它页面上的「网站备案」是转接腾讯云备案系统，仍然要求你先有一台腾讯云境内服务器。
2. **你的服务器是家里那台 Mac**。家庭宽带不是接入服务商，没有「备案服务号」可填，
   所以这条路不是「免费不免费」的问题，而是根本进不了备案流程。个人自用、不对外公开，也不需要。
3. **备案里没有「IP 实时动态调整」这种机制**。备案登记的是主体 + 域名 + 接入商，IP 属接入信息；
   换 IP 走「变更备案」，不是自动同步。家宽 IPv6 前缀本来就不在备案体系内——DDNS 是给 DNS 用的，跟备案无关。

真正会卡住你的两件事，都跟备案无关：

| 事项 | 触发条件 | 处理 |
| --- | --- | --- |
| 域名实名认证 | 国内后缀（`.top`/`.cn`/`.com` 等）注册后限期内未实名 | 腾讯云/注册商控制台提交身份证，1–2 天通过；否则解析被暂停 |
| `.cn` 域名解析前置备案 | 买的是 `.cn` | DNSPod 文档明确 `.cn` 需先备案才能解析，家用场景走不通 → 换 `.top`/`.com`/`.icu` |
| 家庭宽带跑对外服务 | 把安装页公开给他人使用 | 家宽协议不允许经营性用途；自用不公开一般不管，公开就该挪到云服务器并备案 |

**后缀怎么挑（都不需要备案，只需实名）：**

| 后缀 | 首年 | 续费 | 说明 |
| --- | --- | --- | --- |
| `.icu` | 10 元 | 66 元/年 | 2019 年获工信部批复，境内可注册可实名，DNSPod 支持；反垃圾信誉一般，但 OTA 只走 HTTPS 不受影响 |
| `.top` | 14 元 | 34 元/年 | 续费最便宜，最稳妥 |
| `.com` | 90 元 | 99 元/年 | 通用性最好，贵 |
| `.cn` | 33 元 | 39 元/年 | **不要用**：未备案不解析 |

> 阿里云口径略有差异：`.top` 12 元首年 / 32 元续费，`.com` 85 / 95，`.cn` 38 / 42，
> `.icu` 的 1 元首年要搭配买 ESA 基础版，单独买按活动价走，以结算页为准。

APP 备案同理：仅自用一台手机、不经应用商店分发，不构成「向境内用户提供互联网信息服务」，
腾讯云 FAQ 的说法是不上架分发平台的单机/自用场景不强制备案；一旦公开分发就需要，且同样要先有接入商。

### 10.6 地址漂了怎么办：手机不用装任何东西，DNS 自己跟上

**手机侧零安装**：iOS 不需要 TestFlight、不需要描述文件 App，也不需要任何 VPN/DDNS 客户端。
需要的只有三样，都是系统自带或已经做过的：

| 项 | 是否必需 | 说明 |
| --- | --- | --- |
| Safari | 必需 | 打开安装页 → 点「安装」触发 `itms-services` |
| 信任开发者证书 | 首次一次 | 设置 → 通用 → VPN 与设备管理 → 信任；用 `devicectl` 有线装过的话已经做过 |
| 开发者模式 | 首次一次 | 设置 → 隐私与安全性 → 开发者模式；同上，有线装过就说明已开 |

可选的两样便利品：主屏书签（存安装页 URL，扫一次就行）、快捷指令（SSH 到家宽 Mac 跑 `ota_publish.sh`，见第 5 章）。

**地址为什么会变**：家宽 IPv6 前缀是 DHCPv6-PD 下发的，光猫重启、运营商租期到期、重拨都会换。
SLAAC 会让网卡自己拿到新地址，但 DNS 里的 AAAA 不会自己变——这就是 `ota_ddns.py` 存在的理由。

**自动改的三层**：

1. **判断**：只取 `en1` 上 `autoconf secured` 的地址（排除 `fe80`、`temporary`、`deprecated`），
   与 DNS 当前解析值比对，一致时不写（`DDNS_NOCHANGE`），避免无谓的 API 调用。
2. **写入**：不一致才 `UpdateDomainRecord`（alidns 走 `UpdateDomainRecord`，TTL 自动抬到 600）。
3. **定时**：LaunchAgent 每 300 秒跑一次，`RunAtLoad` 开机也跑一次。

```sh
scripts/dev/ota_ddns_agent.sh install     # 装（已装好）
scripts/dev/ota_ddns_agent.sh status      # 看是否在跑 + 最近日志
scripts/dev/ota_ddns_agent.sh run         # 手动跑一次
scripts/dev/ota_ddns_agent.sh uninstall
```

日志：`~/Library/Logs/crab-feast-ota/ddns.log`；结果码 `DDNS_OK` / `DDNS_NOCHANGE` / `DDNS_DRIFT` / `DDNS_NO_V6` / `DDNS_FAILED`。

**切换期间会怎样**：DNS 改完后，手机侧旧记录最多残留一个 TTL（600 秒）——表现为最长 10 分钟打不开安装页，
Safari 重新加载即可。所以「前缀变了 → 装着用 10 分钟」是可接受的最坏情况，比手动查地址强。

**唯一会真正断掉的是 Mac 睡眠**。服务跑在这台机器上，睡了就没人应答：

```sh
sudo pmset -a sleep 0 disksleep 0     # 桌面机接电源时别睡（Mac Studio 建议直接这么设）
caffeinate -s                         # 或临时顶一段时间
pmset -g | grep sleep                 # 确认
```

### 10.8 服务僵死：端口在听但握手被 reset

2026-11-21 遇到过：`lsof` 显示 `*:8443 LISTEN`、进程 `S` 状态活着，但本机 `curl` 也返回 000
（`Send failure: Broken pipe` / `error:35`），服务端日志里是一串 `ConnectionResetError`。
手机端表现就是「页面打不开」。**进程活着 ≠ 能服务**。

处置：`scripts/dev/ota_serve.sh --ipv6 --restart` 立刻恢复。为了不用每次手动发现，加了守护：

```sh
scripts/dev/ota_serve_agent.sh install   # 每 60 秒探 https://[::1]:8443/，挂了自动重启
scripts/dev/ota_serve_agent.sh status
```

判据永远是**回环探测**（`curl -sk https://[::1]:8443/`），不是 `ps`：

| 回环 | 公网（手机） | 结论 |
| --- | --- | --- |
| 000 | 打不开 | 服务僵死 → `--restart` |
| 200 | 打不开 | 入站/DNS/端口问题 → 下一步 |

**端口排查技巧**：先在一个「已验证能入站」的端口（当时的 18080）起个 `hello.txt`，
手机能取到就证明入站没问题、只是新端口（8443）被运营商过滤，换端口即可；
两处都取不到才回到 IPv6 入站或手机侧。

### 10.7 实测记录：从买域名到手机能装（2026-11-21 走通）

| 步骤 | 结果 |
| --- | --- |
| 注册 `qiaqia.cyou`（阿里云） | NS 自动指向 `dns29/dns30.hichina.com` |
| `clientHold`（实名 + 邮箱验证未完成） | 实名通过后**即时解除**，`dig` 从 NXDOMAIN 变 NOERROR |
| 云解析 zone | 已自动存在（SOA serial 可见），不用手动添加 |
| `ota_ddns.py` 写 AAAA | `DDNS_OK created`，三个公共 DNS（阿里/腾讯/Google）均已返回本机地址 |
| `ota_cert.sh` DNS-01 | 签发成功，SAN = `DNS:ota.qiaqia.cyou`，有效期 90 天 |
| `ota_serve.sh --ipv6 --start` | pid 50413，`probe: 200`，回环与公网解析一致 |

踩到的四个坑（都已在脚本里修掉）：

1. **`AddDomainRecord` 缺 `SignatureVersion`**：阿里云查询接口容忍省略，写入接口强制要求，报 `MissingSignatureVersion`。
   只有写 AAAA/TXT 时才会遇到，容易误判成授权问题。
2. **`Forbidden.RAM`**：AK 本身有效但子账号只授了只读策略。判据是「`--check` 通、写失败」，就是权限档位不够。
3. **Let's Encrypt 端点间歇性不可达**：`acme-v02.api.letsencrypt.org` 连续 3 分钟内 8 次超时（curl 28），
   几分钟后 5/5 全部返回 200。脚本加了 8 秒预检 + 明确报错，不再让 acme.sh 空转。
4. **ZeroSSL 备用路径的两个前提**：`acme.zerossl.com` 也时通时不通；且 ACME 账号注册要求**真实可达邮箱**
   （`admin@qiaqia.cyou` 这类无 MX 的域名邮箱会被判 `invalidContact`）。EAB 凭据 acme.sh 自己取不到
   （`Cannot resolve _eab_kid`），脚本改成自己 POST `api.zerossl.com/acme/eab-credentials-email` 再喂给 acme.sh。

现在整条链路的成品形态：

```sh
scripts/dev/ota_publish.sh        # 户外一键：构建 → 导出 → 重启服务
scripts/dev/ota_serve.sh --status # 看 host / 地址 / 证书 / 端口
```

### 10.9 包体：374MB → 150MB（2026-11-21 实测）

户外装机走蜂窝流量，包体直接决定"点一下安装等多久"。三个互不冲突的杠杆，按收益排序：

| 手段 | 效果 | 落点 |
|---|---|---|
| Release 体积配置 | 二进制 163MB → **80.2MB** | `Cargo.toml` 的 `[profile.release]` |
| 贴图降采样（保持 PNG） | `characters/rigs` 288MB → **87MB** | `tools/optimize_glb.py` |
| 未引用字体不进包 | 少 20MB | Xcode「Strip unused assets」阶段 |

合计 ipa **374MB → 150MB（-60%）**，包内未压缩 284MB。

> 2026-11-21 晚追加：资源全部移出包体后，ipa 进一步降到 **29.4MB**（二进制 81.8MB 压缩后 28MB，
> 包内 `assets` 条目数 0）。资源走 10.10 的差量下发，首装下载 121MB。

**1. 二进制：LTO + strip**

```toml
[profile.release]
lto = "thin"          # 跨 crate 内联
codegen-units = 1     # 配合 lto 再压一截
strip = "symbols"     # 48 万个符号没必要时进包
opt-level = 3
```

实测 `__text` 68MB → 58MB，符号表 48 万 → 267 条，文件 163MB → 80.2MB。
首次改这个配置会触发全量重编（约 8–10 分钟），之后增量恢复正常。

**2. 贴图：问题根本在 4K PNG**

九个角色 GLB 里，网格只有 1~2MB，**95% 是 4K PNG 贴图**：

```
Steve.glb   122.5MB → 25.9MB（Diffuse 20.8MB→0.7MB，Normal 23.2MB→6.4MB）
Pete.glb     66.6MB → 18.9MB
Megan.glb    51.4MB → 14.3MB
rigs 合计   288MB   → 87MB（-69%）
```

工具：`tools/optimize_glb.py`（纯 PNG 输出，不动引擎特性；不转 JPEG）

```sh
# 单测：先看一档的收益，不覆盖原文件
python3 tools/optimize_glb.py --in assets/characters/rigs/Steve.glb --out /tmp/glbq --quantize

# 全量（默认 2048 上限 + 256 色量化，法线不量化）
python3 tools/optimize_glb.py --in assets/characters/rigs --out /tmp/glbq --all --quantize
```

三档实测（同一批 GLB）：

| 档 | 参数 | rigs 合计 | 观感 |
|---|---|---|---|
| A | `--max-tex 2048`（不量化） | 119MB | 几乎无损 |
| B | `--max-tex 2048 --quantize` | **87MB** | Diffuse 轻微色带，选的是这档 |
| C | `--max-tex 1024` | 48MB | 近景会糊 |

做法要点（换了机器重做时别踩）：

- 只改 image 的 bufferView，其余 bufferView 按原顺序整体重排、重新算 offset；
  accessor 的 `byteOffset` 是相对 view 的，所以网格/骨骼/动画数据一个字节都不用动。
- 校验脚本要按 componentType 算元素大小（5120/5121=1、5122/5123=2、5125/5126=4），
  漏了会把 `JOINTS_0` 之类的 accessor 误判成越界。
- 小贴图转码后可能反而变大，脚本里做了「不更小就保留原字节」。
- **原图备份**：`assets/source/rigs-original/`（288MB，该目录不进包）。要回滚直接
  `rsync -a assets/source/rigs-original/ assets/characters/rigs/`。

**3. 字体：只有 Regular 被引用**

代码里三处都写死 `fonts/PingFang-SC-Regular.ttf`，`Bold`/`Light` 各 10MB 纯属陪跑。
按"构建忽略"处理，不进包但留在仓库 —— 在 Xcode 的「Strip unused assets」阶段
（`project.pbxproj` 里 UUID `CB7A55E1…`，挂在 Resources 之后、CodeSign 之前）删除：

```sh
for f in PingFang-SC-Bold.ttf PingFang-SC-Light.ttf; do
  rm -f "${APP}/assets/fonts/${f}"
done
```

**还剩什么（下一步）**

- `assets/characters/animations` **90MB**、240 个纯关键帧 GLB，没动。要压得改采样率或
  做浮点量化，会碰动画手感，留着单独决定。
- 二进制 80MB 里 `__text` 还有 58MB，来自 Bevy 特性集（`bevy_dev_tools`、
  `render_dev_tools` 这些只在调试有用），按 feature 拆能再砍一截，但要改构建矩阵。
- 真要再降一个量级就是**资源热更**：把 rigs/animations/fonts 移出包体，ipa 只剩 80MB，
  资源首启下载到 `Application Support`。接入点已确认：`launcher/shared/src/lib.rs`
  的 `asset_file_path()`（`AssetPlugin.file_path` 唯一来源）与
  `crab_feast/src/anim/character.rs::find_asset_root()`（已支持 `CRAB_ASSET_ROOT`）。
  打包侧脚本已就绪：`scripts/dev/ota_pack.sh` 生成 `packs/assets-<版本>.zip` + `latest.json`。


### 10.10 资源热更：版本仓库 + 差量下发（2026-11-21 落地）

目标：改贴图/动画不用重新打包安装，App 启动时自己把缺的文件补上。

```
打包 ──► assets_store.snapshot   给当前资源存档（版本号 = 内容摘要）
             │                    文件按 sha256 进 blobs/，跨版本只存一份
             ├──► full/<ver>.zip        整包（首装用）
             └──► deltas/<a>_<b>.zip    增量（只含新增/改动，删单列在 json 里）

App 启动 ──► GET /assets/state.json        问服务端最新版本
             GET /assets/diff?from=<本地版> 要差量清单
             GET <url>                     下载 → 校验 sha256 → 解压 → 删旧文件
             写 .crab_version               下一版之前不再请求
```

**服务端存储**（`~/.crab_feast-ota/assets/`）

```
indexes/<ver>.json          该版本 280 个文件的 path/sha256/size
blobs/ab/ab12…              文件内容，按内容寻址，跨版本去重
full/<ver>.zip              整包 141MB（原始 221MB）
deltas/<a>_<b>.zip + .json  增量包与元信息（含 deleted 列表）
versions.json               版本时间线
```

**接口**

| 路径 | 返回 |
|---|---|
| `GET /assets/state.json` | `{version, versions[], files, bytes}` |
| `GET /assets/diff?from=<ver>` | `mode = none` / `delta` / `full`，含 `url / sha256 / bytes / files / deleted / cached` |
| `GET /assets/deltas/<a>_<b>.zip` | 增量包（条目无 `assets/` 前缀，解压即资源根） |
| `GET /assets/full/<ver>.zip` | 整包 |

版本号是**内容摘要**（排序后的 `path\0sha256` 再 sha256 取前 12 位），所以同样的资源永远同一个版本，改一个字节就一定换版本。客户端版本在服务端不认识（首装/清过缓存）时回整包。

**缓存**：`deltas/<a>_<b>.zip` 生成后落盘，同一条 `(a→b)` 再问直接命中并在 JSON 里返 `cached: true`；`ota_pack.sh` 在打包时就把「上一版 → 本版」先算一遍，第一个客户端来问时也不会等。

**客户端（`crab_feast/src/assetpack.rs`）**

- 状态机前面多了一档 `GameState::AssetsSync`（默认状态）：桌面/安卓没有落地目录，直接跳到 `AssetLoading`。
- iOS 上 `asset_file_path()` **无条件**返回热更目录（包里已经没有 `assets`），所以首启同步完成后**同一次会话**就能用新资源，不需要重启两次。
- 同步阶段界面用一张临时客观相机 + 进度条 + 一行 ASCII 文字；文字依赖 bevy 的 `default_font` 特性（已在 workspace 打开），进度条本身不依赖任何字体资源。
- 落地目录 iOS = `$HOME/Library/Application Support/crab_feast/assets`，覆盖安装会保留；标记文件 `.crab_version`。
- 同步在独立线程跑，主线程照常出帧（避免 iOS 启动看门狗杀进程），期间显示一行进度文字。
- 失败**不阻塞进游戏**：有旧包继续用旧包，没有就用 bundle 内置资源，日志里 `warn` 出原因。
- 资源根有两处必须一致：`launcher/shared/src/lib.rs::asset_file_path()`（`AssetPlugin.file_path`）与 `crab_feast/src/lib.rs` 里给 `CRAB_ASSET_ROOT` 赋值（供 `scan_catalog()` 扫目录）；只有两处都指向同一个已装好的目录才切换，否则回退 `assets`。

**命令行**

```sh
scripts/dev/ota_pack.sh              # 快照 + 整包 + 预生成增量（ota_export.sh 已自动调用）
scripts/dev/ota_pack.sh --list       # 版本时间线
scripts/dev/ota_pack.sh --prune 3    # 只留最近 3 版（连同它们的增量包）
curl -k "https://ota.qiaqia.cyou:8443/assets/diff?from=<ver>"
```

**实测（2026-11-21）**

| 场景 | 结果 |
|---|---|
| 同版本请求 | `mode: none` |
| 未知版本 | `mode: full`，141MB，Range 206 |
| 改动 1 个文件后请求 | `mode: delta`，1 个文件 156 字节 |
| 同一 `(from,to)` 第二次请求 | `cached: true`，直接复用 |
| 删文件后再请求 | `deleted` 里带路径，客户端解压后显式删除 |
| 全量快照耗时 | 3.7 秒（221MB / 280 文件） |

**打包/首装体积现状（2026-11-21）**

| 项 | 体积 |
|---|---|
| ipa（纯二进制，无 assets） | 29.4MB |
| 热更整包（首次启动下载） | 121MB（rigs 83MB / animations 31.6MB / Regular 字体 6.8MB） |
| 之后改资源 | 只下差量，通常几 MB |

**客户端自测（不走 UI，直接跑同步逻辑）**

```sh
# 首次：整包下载 → 校验 → 解压 → 写版本
cargo test -p crab_feast --lib assetpack -- --nocapture

# 复用已装好的目录，验证增量：先改一个资源并 snapshot，再跑同一命令
CRAB_TEST_DIR=/path/to/installed/assets \
  cargo test -p crab_feast --lib assetpack -- --nocapture
```

实测输出（2026-11-21，服务真在跑）：

```
第一次: full  → dbbfc88e83f9          # 首装走整包
第二次: 已是最新 dbbfc88e83f9          # 版本一致不再请求
第一次: delta dbbfc88e83f9 → 6f9ff61e742a   # 只加了一个 21 字节文件
第一次: delta 6f9ff61e742a → 10239a8a46b5   # 删一个 + 加一个
```

删除验证：增量包里没被包含的旧文件，客户端按 `deleted` 列表显式删掉了。

**边界（明确没做的）**

- 不做 gzip 传输（iPhone 侧省 CPU），大文件靠 zip 内 deflate。
- 不支持 zip64（单包 > 4GB 会失败，当前 141MB 余量充足）。
- 增量包不校验 CRC（整包 sha256 已兜住完整性）。
- 没有断点续传：下到一半失败就整包重下（增量包通常只有几 MB）。
- 首装**同步失败且本地没有资源**时会停在 `ASSET SYNC FAILED`（不进退），因为包里已经没有任何可展示的资源；网络恢复后重启即可。
- 资源已全部移出包体：ipa 29.4MB（纯二进制），首装下载整包 121MB；后续改资源只下差量。
- 整包里排除了 `fonts/PingFang-SC-Bold.ttf` 与 `-Light.ttf`（代码只用 Regular），少 13.7MB。

## 附：脚本

```
scripts/dev/ota_export.sh    出 ipa + 生成 manifest/index（--release --lean）
scripts/dev/ota_serve.sh     起停服务；默认 tailscale serve，--public 走 Funnel，--ipv6 直连家宽 IPv6
web/ (crab-feast-web)        本机静态源（双栈绑定、TLS、Range 续传）+ 注册中心，--ipv6 时直接终止 TLS
scripts/dev/ota_ddns.py      AAAA 同步：cloudflare / dnspod / alidns / duckdns
scripts/dev/ota_cert.sh      DNS-01 签发（自动装 acme.sh）+ 装证书 + 续期后自动重载
scripts/dev/ota_publish.sh   户外一键：构建 + 导出 + 重启服务（SSH 强制命令用）
scripts/dev/ota_pack.sh      把资源打成本地热更包（packs/assets-<版本>.zip + latest.json）
tools/optimize_glb.py        GLB 贴图降采样/重编码（纯 PNG，网格与动画数据不动）
scripts/dev/assets_store.py  资源版本仓库：快照(备份)、整包、按版本对算增量、缓存、prune
scripts/dev/ota_pack.sh      打包时调用上面这个：快照 + 整包 + 预生成增量
```

### 10.11 版本号、首启重试与「怎么验证真的在走热更」

**版本号为什么一直是固定值**

安装页上的 `build 82` 和 `5650019-dirty` 分别来自 `git rev-list --count HEAD` 与
`git rev-parse --short HEAD`。这个仓库从未 commit、也从未 push，所以两个值恒定不变，
看页面无法判断包到底有没有更新。另外 `Info.plist` 里的 `CFBundleVersion` 是写死的 `1`，
iOS 侧同样分不出新旧。

现在的做法（`scripts/dev/ota_export.sh`）：

| 字段 | 来源 | 用途 |
|---|---|---|
| `BUILD` | `~/.crab_feast-ota/.build_number` 持久化自增 | 同时写进 ipa 的 `CFBundleVersion` 和清单的 `bundle-version`（两者必须一致，否则 iOS 不认这个更新） |
| `BUILD_ID` | 出包时间戳 `YYYYMMDD-HHMMSS` | 安装页的「构建标识」 |
| `MARKETING_VERSION` | 默认 `1.0` | `CFBundleShortVersionString` |
| `ASSETS_VER` | `assets/versions.json` 里最新快照 | 安装页的「资源版本」，用来分辨是「换了包」还是「换了资源」 |

`Info.plist` 里的两个版本字段已改成 `$(MARKETING_VERSION)` / `$(CURRENT_PROJECT_VERSION)`，
不再写死。`--no-build` 复用旧 ipa 时不会推高构建号，以免清单和包对不上。

安装页现在显示五行：Bundle ID / Version / Build / 构建标识 / 资源版本。

**首启「一闪而过」的真正原因**

不是路径问题。设备上的 `HOME` 是正常的，日志给出的是网络：

```
[..] 启动 HOME=Some("/private/var/mobile/Containers/Data/Application/…")
[..] state.json 第 1 次失败: failed to lookup address information
[..] state.json 第 2 次失败: No route to host (os error 65)
[..] state.json OK（第 3 次尝试）
```

刚装完就打开时 DNS 还没就绪、IPv6 路由还没建立，第一次请求必失败。原来的实现只打一枪，
失败就把状态切到 `AssetLoading`，于是进度条一闪、资源也没下。现在 `sync_pack` 在拉
`state.json` 这一步做 8 次退避重试（1s→5s 递增，总上限约 30s），期间屏幕上显示 `等待网络… n/8`。

**三条验证「资源真的来自热更」的方法（由快到彻底）**

1. **看大厅左下角**：`PACK <版本> · <n> dirs`，版本应等于安装页上的「资源版本」。
2. **看服务端日志**（谁在什么时候请求了什么）：
   ```sh
   grep "assets/" ~/.crab_feast-ota/server.log | tail
   ```
   首启应看到 `state.json` → `diff?from=` → `full/<ver>.zip`（或 `deltas/…zip`）；
   再启动只应看到一条 `state.json`（本地版本已等于服务端版本）。
3. **看设备容器里的同步日志**（最彻底，能看到每一步耗时与结果）：
   ```sh
   xcrun devicectl device copy from --device <UDID> \
     --domain-type appDataContainer --domain-identifier org.crab.feast \
     --source . --destination /tmp/c3
   cat "/tmp/c3/Library/Application Support/crab_feast/sync.log"
   ls  "/tmp/c3/Library/Application Support/crab_feast/assets"
   ```
   `sync.log` 会记录：解析到的资源目录、每次 state.json 尝试、本地/服务端版本、
   下载模式与字节数、校验值、同步完成的结果。

顺带确认包体本身确实没有资源：`unzip -Z1 ~/.crab_feast-ota/crab_feast-ios.ipa | grep -c "app/assets/"` 应为 `0`。

**关于「懒加载」**：资源不是懒加载。首启是一次性把整包/差量同步到本地容器（带进度条），
之后每帧都从本地目录读，不会在运行时再去拉网络。所以「闪一下就进游戏」只在
「本地版本已等于服务端版本」时才成立——那时确实没有任何下载。

**版本探测：13 字节，而不是 2KB JSON**

启动时只需要回答一个问题：本地版本和服务端版本是否一致？原来的实现每次启动都拉
`/assets/state.json`（约 2KB，含 versions/files/bytes），对这个问题是浪费。现在：

| 端点 | 返回 | 体积 |
|---|---|---|
| `GET /assets/version` | 纯文本版本号 + `ETag` | 13 字节 |
| 带 `If-None-Match: "<本地版本>"` | `304`，正文为空 | 0 字节 |
| `GET /assets/diff?from=<ver>` | 仅在版本不一致时才请求 | 差量描述 + 包 |

客户端逻辑：`sync_pack` 先用本地 `.crab_version` 作 `If-None-Match` 去探测 version，
拿到 304 就直接进游戏（一次 RTT、零正文）；不一致才去拉 `diff`，用 `diff` 返回的
`version` 作为目标版本写标记。`state.json` 仍保留在服务端（页面/自测用），但客户端不再请求它。

**资源包内容**：`assets/android-res`（安卓专用的启动图标，16K/1 文件）已排除，不再进 iOS 热更包；
需要恢复时 `CRAB_PACK_KEEP_ANDROID_RES=1 scripts/dev/ota_pack.sh`。删文件后客户端会顺手清理
因此变空的父目录，不在容器里留空壳。

### 10.12 GLB 贴图压缩在链路里的位置（以及覆盖风险）

**压缩发生在哪一步？**

贴图降采样**不在**构建/打包/OTA 的任何自动流程里，它是数据准备阶段的手工步骤：

```
assets/source/character/rigs/*.fbx
        │  scripts/export_rigs.sh          （Blender 导出；默认不压缩）
        ▼
assets/characters/rigs/*.glb             ← 直接写这里（--out 默认就是这个目录）
        │  tools/optimize_glb.py           （手工，一次；2048 + 256 色量化，纯 PNG）
        ▼
assets/characters/rigs/*.glb             ← 原地覆盖成压缩版
        │  scripts/dev/ota_pack.sh         （只做 snapshot + zip，不碰图像）
        ▼
~/.crab_feast-ota/assets/{blobs,full,deltas}
```

- `export_rigs.sh --compress`：Blender 内降采样（最长边 2048、格式 PNG、`--tex-quality 80`
  只在 JPEG 路径生效）。**默认关闭**，不加就是原始分辨率。
- `tools/optimize_glb.py`：离线降采样 + 256 色量化，法线贴图保持全彩不量化；
  输出不比原文件小就保留原字节，所以**重复跑是安全的**（不会越压越糊）。
- `ota_pack.sh` / `assets_store.py` 只做内容寻址与打包，**完全不做图像处理**。

**会不会覆盖已压缩的资源？**

- 不加 `--force`：已存在的 GLB 会被跳过，不会覆盖。
- 加 `--force`（脚本示例里就有 `scripts/export_rigs.sh -j 3 --force`）：会用未压缩的导出结果
  覆盖压缩版。原始的 288MB 备份在 `assets/source/rigs-original/`，但**压缩版本身没有中间备份**，
  覆盖后只能重跑 `optimize_glb.py` 复原。
- 新增角色同理：新导出的 GLB 天然是原始分辨率。

**一段式流程（`--optimize`，默认开启）**

`scripts/export_rigs.sh` 现在默认就把三件事串起来做，产物直接落在最终资源目录：

```
1/3 导出到临时区（原始贴图分辨率）
2/3 降采样（tools/optimize_glb.py，纯 PNG，最长边 2048 + 256 色量化）
3/3 按 --force 语义回写到 assets/characters/rigs
```

```sh
scripts/export_rigs.sh                    # 默认：导出 + 压缩 + 回写（推荐）
scripts/export_rigs.sh -j 3 --force       # 并行 + 覆盖
scripts/export_rigs.sh --no-optimize      # 只要原始分辨率导出（体积会暴涨）
scripts/export_rigs.sh --list             # 只看待处理文件
CRAB_TEX_MAX=1024 scripts/export_rigs.sh  # 换成更小一档
CRAB_TEX_QUANTIZE=0 scripts/export_rigs.sh # 只缩放、不做 256 色量化
```

回写阶段尊重 `--force`：目标目录已有同名 GLB 且没加 `--force` 时跳过，不会悄悄覆盖。
中间文件都在 `mktemp -d` 的临时目录里，中断不会在资源目录留下半成品。

**守卫（已加）**

- `scripts/dev/ota_pack.sh`：`characters/rigs` 总量 >130MB 或单文件 >35MB 时直接 `exit 2`，
  并打印修复命令；确认要原样打包可设 `CRAB_ALLOW_BIG_RIGS=1`。
- `scripts/export_rigs.sh`：导出完成后若总量 >130MB，打印降采样 + 回写命令。

这条守卫是为了避免「重新导出 → 静默把 288MB 打进热更包 → 客户端下一版要下几百 MB 差量」。
