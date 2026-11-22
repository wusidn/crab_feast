# Web 服务：服务器注册中心 + 资源更新

同一个 `crab-feast-web` 进程开两个端口：

- **8090**：注册中心 JSON API（明文 HTTP）；
- **8443**：资源版本/增量包 + iOS 安装页（有证书时 TLS，没问题时明文，给 tailscale 做上游）。

解决「不在同一个局域网，怎么在大厅里看到并直连服务器」，同时也承载 OTA 运行时；
`assets_store.py` 只负责打包快照。

```
┌────────────────┐   POST /api/servers/register   ┌──────────────────────┐
│ crab-feast-    │ ─────────────────────────────► │ crab-feast-web       │
│ server         │   每 5 秒 heartbeat            │ :8090 注册 API       │
│ （游戏服务端）  │ ─────────────────────────────► │ :8443 OTA/安装页     │
└────────────────┘                                │ 内存表 + TTL 15s     │
                                                  └──────────────────────┘
                                                          ▲
┌────────────────┐   GET /api/servers（每 5 秒）           │
│ crab_feast_pc  │ ────────────────────────────────────────┘
│ （大厅）        │   合并局域网 UDP 广播 → 一行一个服务器 → 点「加入」直连
└────────────────┘

┌────────────────┐   GET /assets/version / diff  ┌──────────────────────┐
│ crab_feast_pc  │ ─────────────────────────────► │ :8443 OTA            │
│ / iOS 安装页    │   整包/增量 zip、manifest/ipa  │ Rust 版资源运行时    │
└────────────────┘                                └──────────────────────┘
```

局域网广播（UDP 5098）和注册中心（HTTP 8090）**是两条独立来源**，客户端会合并去重。
同机/同局域网时两条都能看到，会显示成「局域网 + 注册中心」；只有注册中心能看到时
显示「注册中心」。这样原来不经过公网的局域网玩法完全不受影响。

## 1. 启动顺序

```sh
# 1) 先起 Web 服务：注册 API + OTA 在同一个进程
cargo run -p crab_feast_web -- --addr 0.0.0.0:8090 --ota-addr 0.0.0.0:8443

# 2) 起游戏服务端：默认就会向 http://127.0.0.1:8090 报到
#    绑定本机稳定公网 IPv6；--public-host 用域名 game.qiaqia.cyou（AAAA 已指向这个 IPv6）。
#    广播与注册中心都会报域名，客户端大厅只出现一行；连接时客户端解析域名再走 IPv6 UDP。
cargo run -p crab_feast_server -- --addr '[2409:8a3c:424:5ff0:1cda:21aa:9ba1:d1cd]:5099' \
    --public-host game.qiaqia.cyou \
    --name 客厅服

# 3) 起客户端：大厅会同时列出局域网广播与注册中心里的服务器
cargo run -p crab_feast_pc
```

一键拉起（沿用 `launcher.json` 的 `server.registry`）：

```sh
cargo run -p crab_feast_pc -- --launch-all
```

> 注册中心默认端口是 **8090**，不是 8080——README 里的 wasm 预览服务已经占用 8080，
> 两个服务同时开不会撞端口。OTA 默认 **8443**；只跑注册时加 `--no-ota`。

## 2. HTTP API

所有响应都是 JSON；错误统一为 `{"ok": false, "error": "..."}`。

### `GET /api/health`

```json
{"ok": true, "servers": 2, "ttl_secs": 15.0, "ota": true}
```

### `GET /api/servers`

```json
{
  "ok": true,
  "count": 1,
  "servers": [
    {
      "id": "crab-mac-studio-192-168-1-5-12345",
      "name": "客厅服",
      "host": "game.example.com",
      "port": 5099,
      "addr": "game.example.com:5099",
      "players": 3,
      "max_players": 100,
      "tick_hz": 60.0,
      "age_secs": 2.1
    }
  ]
}
```

- `addr` 是拼好的 `host:port`，IPv6 会带方括号；客户端直接拿它做 DNS 解析与连接。
- `age_secs` 是最后一次心跳距今的秒数，用来判断服务器是否还活着。
- 超过 TTL（默认 15 秒）没心跳的条目不会出现在列表里。

### `POST /api/servers/register`

```json
{
  "id": "可选：稳定实例 id，重复注册会覆盖同一条记录",
  "name": "客厅服",
  "host": "game.example.com",
  "port": 5099,
  "players": 0,
  "max_players": 100,
  "tick_hz": 60
}
```

```json
{"ok": true, "id": "crab-...", "addr": "game.example.com:5099", "ttl_secs": 15.0}
```

- `host` 可省略：注册中心会用 TCP 源 IP 兜底（适合「客户端和服务端在同一台机器」）。
- `port` 必填且不能为 0。
- `id` 留空时由注册中心生成；服务端自己带稳定 id，进程重启后不会留下旧记录。

### `POST /api/servers/{id}/heartbeat`

```json
{"players": 3, "max_players": 100}
```

```json
{"ok": true}
```

条目不存在或已过期时返回 **410 Gone**，服务端看到后会自动重新注册。

### `POST /api/servers/{id}/unregister`

```json
{"ok": true, "removed": true}
```

正常关服时调用；进程被 kill 时靠 TTL 清理。

### 手动验证

```sh
# 注册
curl -s -X POST http://127.0.0.1:8090/api/servers/register \
  -H 'Content-Type: application/json' \
  -d '{"name":"手测服","host":"127.0.0.1","port":5099,"players":0,"max_players":100,"tick_hz":60}'

# 列表
curl -s http://127.0.0.1:8090/api/servers

# 心跳（把 <id> 换成注册返回的 id）
curl -s -X POST http://127.0.0.1:8090/api/servers/<id>/heartbeat \
  -H 'Content-Type: application/json' -d '{"players":2,"max_players":100}'

# 注销
curl -s -X POST http://127.0.0.1:8090/api/servers/<id>/unregister
```

## 3. 服务端行为

- **注册时机**：Bevy `Startup` 阶段起一个独立线程，不阻塞服务器启动。
- **心跳**：每 5 秒一次，带上当前房间人数（由 `Room.avatar_of_link` 每帧同步进原子变量）。
- **失败重试**：注册失败每 2 秒重试；心跳失败保留 id 再试，410 则重新注册。
- **退出注销**：`Registration` 资源 `Drop` 时停线程并尽力 `unregister`。
- **对外地址（host）的确定顺序**：
  1. `--public-host` / `launcher.json` 的 `server.public_host` / `CRAB_PUBLIC_HOST`；
  2. 监听地址不是 `0.0.0.0` 时直接用监听 IP；
  3. 绑 `0.0.0.0` 时用 UDP connect 探测默认出口网卡的 IP（不发包）；
  4. 都拿不到就回退 `127.0.0.1` 并警告——这时外网连不上，必须显式配置。

> 绑 `0.0.0.0` + 不配 `public_host` 时，探测到的是**局域网 IP**。
> 要让公网客户端连上，必须用 `--public-host` 指定公网 IP 或域名，
> 并做端口映射（游戏端口是 **UDP**）。

## 4. 客户端行为

- 大厅阶段后台线程每 5 秒 `GET /api/servers`；
- 12 秒没出现在列表里就移除该来源（留两次拉取的容错）；拉取失败时保留已有列表；
- 与 UDP 广播按 `SocketAddr` 合并去重，来源标签显示「局域网 / 注册中心 / 局域网 + 注册中心」；
  合并时展示串优先用注册中心上报的原文（如 `game.qiaqia.cyou:5099`），连接仍用解析后的 `SocketAddr`。
- 点「加入」后走原来的 `SessionMode::Online { server }` 流程，连接方式不变（UDP + netcode）。
- **IPv6 本地 socket**：客户端按服务器地址族选择本地绑定，服务器是 IPv6 时绑 `[::]:0`，
  是 IPv4 时绑 `0.0.0.0:0`。如果固定用 IPv4 socket 去 `send_to` IPv6 服务器，
  会报地址族不支持，界面表现就是一直卡在「正在连接」。
- **iOS 包没有 `launcher.json`**：移动端入口显式注入 `RegistryConfig`，默认指向
  `https://ota.qiaqia.cyou:8443`（与 OTA 同一个域名/端口）；出包时可设
  `CRAB_MOBILE_REGISTRY_URL`，运行时可设 `CRAB_REGISTRY_URL` 覆盖。
  移动端 Cargo 必须开 `crab_feast` 的 `net` feature，否则 iOS 包里不会编译发现逻辑。
- iOS 的 `Info.plist` 需要 `NSLocalNetworkUsageDescription`（局域网发现/直连）与
  `NSAppTransportSecurity → NSAllowsLocalNetworking`（允许局域网明文 HTTP）。

## 5. 配置

`launcher.json`：

```json
{
  "server": {
    "addr": "[2409:8a3c:424:5ff0:1cda:21aa:9ba1:d1cd]:5099",
    "registry": "http://127.0.0.1:8090",
    "public_host": "game.qiaqia.cyou",
    "name": "客厅服"
  },
  "registry": "http://127.0.0.1:8090"
}
```

- `server.addr`：本机监听地址。绑定具体公网 IPv6 能固定回包源、避开 macOS 临时地址，
  但只接受 IPv6 客户端；要同时接局域网 IPv4 客户端，需要另行双 socket/端口映射。
  只绑 `0.0.0.0` 时无法接 IPv6，不能用于公网 IPv6 直连。
- `server.registry`：服务端报到用的注册中心；`"off"` 关闭。
- `registry`（顶层）：客户端拉列表用的注册中心；`"off"` 关闭（只保留局域网）。
- `server.public_host`：对外上报的域名/公网 IP（IPv6 字面量不用写方括号）。
  域名必须有可用的 AAAA 记录；广播和注册中心共用它解析出的地址，避免同一台服在大厅里出现两行。
- `server.name`：大厅显示名；同时用于 UDP 广播和注册中心，不填用「机器名 · 端口」。
  客户端合并同一地址的两条来源时优先显示注册中心的名字，注册中心过期后回落到广播里报的名字。

命令行：

```sh
# 服务端
--registry <url>    --no-registry    --public-host <域名/IP>    --name <显示名>
# 客户端
--registry <url>    --no-registry
# 注册中心 + OTA
--addr <监听地址>    --ota-addr <监听地址>    --no-ota    --no-registry
--ota-root <目录>    --asset-store <目录>    --cert <pem>    --key <pem>
```

环境变量：

| 变量 | 作用 |
| --- | --- |
| `CRAB_REGISTRY_URL` | 注册中心地址（客户端/服务端共用；`off` 关闭） |
| `CRAB_REGISTRY_ADDR` | 注册中心监听地址，默认 `0.0.0.0:8090` |
| `CRAB_REGISTRY_TTL_SECS` | 心跳超时秒数，默认 15 |
| `CRAB_OTA_ADDR` | OTA 监听地址，默认 `0.0.0.0:8443` |
| `CRAB_OTA_ROOT` | 安装页/manifest/ipa/证书目录，默认 `~/.crab_feast-ota` |
| `CRAB_ASSET_STORE` | 资源仓库，默认 `<ota-root>/assets` |
| `CRAB_OTA_CERT` / `CRAB_OTA_KEY` | 8443 的 TLS 证书/私钥；都不给则明文 |
| `CRAB_WEB_NO_OTA` / `CRAB_WEB_NO_REGISTRY` | 置 1 时只跑另一半 |
| `CRAB_PUBLIC_HOST` | 服务端上报的对外主机名 |
| `CRAB_SERVER_NAME` | 服务端在大厅里的显示名 |

优先级：命令行 > `launcher.json` > 环境变量 > 内置默认。

### 域名与 DDNS

当前使用两个二级域名，都只有 AAAA：

| 域名 | 用途 |
| --- | --- |
| `ota.qiaqia.cyou` | OTA 安装页 + TLS 注册 API（8443） |
| `game.qiaqia.cyou` | 游戏服对外地址（UDP 5099） |

新增或更新游戏服 AAAA：

```sh
python3 scripts/dev/ota_ddns.py --host game.qiaqia.cyou \
    --provider alidns --zone qiaqia.cyou
# 输出 DDNS_OK created / DDNS_OK updated / DDNS_NOCHANGE
```

检查解析：

```sh
dig +short AAAA game.qiaqia.cyou @223.5.5.5
```

家宽 IPv6 前缀变化后，`ota.qiaqia.cyou` 与 `game.qiaqia.cyou` 都要重新同步；
阿里云免费版 TTL 为 600 秒，更新后要等解析缓存过期。

## 6. 部署与安全

- 注册中心是**无状态服务**（内存表），重启后表清空，各服务端会在下一次心跳收到 410 并自动重新注册；
- 暴露到公网时建议放在 Caddy/Nginx 后面终止 TLS，客户端填 `https://...` 即可（`ureq` 自带 rustls）；
- 需要放行的端口：注册中心 **TCP 8090**、OTA/安装页 **TCP 8443**、游戏 **UDP 5099**；
- OTA 静态路由是**白名单**：只暴露 `/`、`/index.html`、`/manifest.plist`、`/*.ipa`
  和 `/assets/*`，`certs/`、`ota.env`、`server.log` 都不会暴露；
- 当前没有鉴权：谁都能注册/心跳/注销。仅适合开发/自建小规模使用；公网部署前应加 token
  （在 `register`/`heartbeat` 处校验 `Authorization`，并只允许自己的服务端持有）。

## 7. 资源更新 / OTA（8443）

运行时已经由 `web/src/assets.rs` + `web/src/http.rs` 接管，目录布局和 JSON 字段与
`scripts/dev/assets_store.py` 完全兼容；打包（snapshot）仍用 Python CLI：

```sh
python3 scripts/dev/assets_store.py snapshot --src assets --ver-out /tmp/ver
```

接口：

| 请求 | 响应 |
| --- | --- |
| `GET /assets/version` | 纯文本最新版本号 + `ETag`；`If-None-Match` 命中回 **304** |
| `GET /assets/state.json?from=<ver>` | `{version, client, versions(最近10), files, bytes}` |
| `GET /assets/diff?from=<ver>` | `mode=none/full/delta`、`url`、`sha256`、`bytes`、`files`、`deleted` |
| `GET /assets/full/<ver>.zip` | 整包，条目路径不带 `assets/` 前缀 |
| `GET /assets/deltas/<from>_<to>.zip` | 增量包，只含新增/改动；删除清单在 `diff.deleted` |
| `GET /` | iOS 安装页 `index.html` |
| `GET /manifest.plist` | `application/xml`，`Cache-Control: no-store` |
| `GET /<name>.ipa` | `application/octet-stream`，支持 `Range` 断点续传 |

行为约定：

- 仓库为空返回 503 `{"error": ...}`；未知/空 `from` 回整包；同版本回 `none`；
- 整包/增量包首次请求时按索引现场生成，之后命中缓存；同一对版本只生成一次；
- 增量包按 `path + sha256` 找新增/改动，`deleted` 单独列出；
- `manifest.plist`、`index.html`、`.ipa` 都 `no-store`，避免 iOS 拿旧安装地址；
- 版本化 zip 可缓存一天；大文件流式写出，不受 2 秒短超时限制。

直接 TLS（保留原 `--ipv6` / `--direct` 模式）：

```sh
cargo run -p crab_feast_web -- \
  --addr 0.0.0.0:8090 --ota-addr 0.0.0.0:8443 \
  --cert ~/.crab_feast-ota/certs/fullchain.pem \
  --key ~/.crab_feast-ota/certs/privkey.pem
```

`--cert/--key` 都不给时 8443 跑明文，配合 `tailscale funnel --https=8443
http://127.0.0.1:8080` 的上游；`scripts/dev/ota_serve.sh` 已改为启动本进程，
`--start --ipv6` 会同时占用 8443（TLS）和 8090（注册 API）。

## 8. 排查

| 现象 | 检查 |
| --- | --- |
| 服务端日志「注册中心不可达」 | 注册中心是否启动、`--registry` 地址是否正确、TCP 8090 是否放行 |
| 列表里能看到但连不上 | 域名 AAAA 是否指向服务端公网 IPv6；UDP 5099 是否放行；客户端是否用了修复前的旧包 |
| 一直卡在「正在连接」 | 客户端本地 socket 是否按服务器地址族绑定（IPv6 服务器必须用 `[::]:0`）；服务端日志有没有 `New connection` / `join` |
| `game.qiaqia.cyou` 本机解析失败 | `dig +short AAAA game.qiaqia.cyou` 是否有值；刚新增记录时 macOS 可能缓存旧 NXDOMAIN，等负缓存 TTL 过后再试或刷新 DNS |
| 服务端显示「已从注册中心注销」但列表还有 | TTL 还没到（最多 15 秒），或注销请求失败 |
| 客户端一直「正在搜索」 | 注册中心是否可访问；`CRAB_REGISTRY_URL` 是否被设成 `off` |
| 列表出现两台相同服务器 | 来源地址不同（如域名解析到的 IP 与广播 IP 不同），属预期；按 `SocketAddr` 去重 |
| `/assets/version` 返回 503 | 仓库 `versions.json` 不存在/损坏；先 `assets_store.py snapshot`，并核对 `--ota-root`/`--asset-store` |
| iOS 点安装没反应 | 8443 是否是受信任 HTTPS、证书 SAN 是否匹配域名；`curl -vk` 看 manifest.plist |
| 8443 启动报 Address already in use | 另一个 `crab-feast-web` 还在跑：`scripts/dev/ota_serve.sh --stop`（会按端口清理残留进程） |
