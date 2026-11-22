# crab_feast
bevy playground

## Build

Single entry point:

```sh
scripts/build.sh <platform> [--release]
```

Platforms: `pc`, `wasm`, `android`, `ios`, `all` (pc + wasm). See `scripts/build.sh --help` for details.

The web build is served from the repository root:

```sh
scripts/build.sh wasm
python3 -m http.server 8080
# open http://localhost:8080/launcher/wasm/
```

## 联机：服务器注册中心 + 资源更新（同一个 Web 进程）

`crab-feast-web` 一个进程开两个端口：**8090** 注册 API（明文 HTTP），**8443**
资源更新/OTA + iOS 安装页（可选 TLS）。资源版本与增量逻辑全部在 Rust 侧实现；
Python 只保留 `assets_store.py` 的打包 CLI。

```sh
# 1) Web 服务：注册 + OTA（8443 没证书就跑明文，给 tailscale funnel 做上游）
cargo run -p crab_feast_web -- --addr 0.0.0.0:8090 --ota-addr 0.0.0.0:8443

# 需要 iOS 直连安装时给 8443 配证书：
# cargo run -p crab_feast_web -- --cert ~/.crab_feast-ota/certs/fullchain.pem \
#     --key ~/.crab_feast-ota/certs/privkey.pem

# 只跑注册中心：
cargo run -p crab_feast_web -- --no-ota

# 2) 游戏服务端：默认向 http://127.0.0.1:8090 报到，每 5 秒心跳
#    绑定本机稳定公网 IPv6；public_host 用域名 game.qiaqia.cyou（AAAA 指向这个 IPv6），
#    广播和注册中心都会报域名，大厅里同一台服只出现一行。
cargo run -p crab_feast_server -- --addr '[2409:8a3c:424:5ff0:1cda:21aa:9ba1:d1cd]:5099' \
    --public-host game.qiaqia.cyou --name 客厅服

# 3) 客户端：大厅直接看到并加入
cargo run -p crab_feast_pc
```

iOS 包没有 `launcher.json`，所以移动端在 `launcher/mobile/src/lib.rs` 里显式注入
注册地址，默认是 `https://ota.qiaqia.cyou:8443`（和 OTA 同一个域名/端口）；
出包时可用 `CRAB_MOBILE_REGISTRY_URL` 覆盖，运行时可临时用 `CRAB_REGISTRY_URL` 覆盖。
移动端 Cargo 已打开 `crab_feast` 的 `net` feature，否则 iOS 包里根本不会编译
局域网发现和注册中心拉取。但 feature 只负责把代码编进去，launcher 还必须显式
`add_plugins(NetPlugin)`（PC 在 `launcher/pc/src/main.rs`，手机在
`launcher/mobile/src/lib.rs`）；漏掉这一步大厅照样能显示服务器，但点「加入」后
没有 `connect` 系统、没有 UDP socket，只会停在「正在连接」，服务端也收不到包。
客户端连接时会按服务器地址族选择本地 UDP 绑定：
IPv6 服务器用 `[::]:0`，IPv4 服务器用 `0.0.0.0:0`；固定绑 IPv4 会导致 IPv6 握手
一直停在「正在连接」。

服务端启动时上报 `host` / `port` / 名称 / 人数，退出时注销；客户端每 5 秒拉一次
`GET /api/servers`，与局域网广播合并去重后显示来源。`--name` 同时用于 UDP 广播和
注册中心，同一台服务器不会因为两条来源名字不同而在大厅里跳名。

资源仓库仍是 `scripts/dev/assets_store.py snapshot` 生成；部署脚本
`scripts/dev/ota_serve.sh` 已改为启动 `crab-feast-web`（`--start --ipv6` 会同时起
TLS 8443 与注册 API）。接口与部署说明见
[docs/net/server-registry.md](docs/net/server-registry.md)。
