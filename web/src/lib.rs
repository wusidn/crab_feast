//! crab_feast Web 服务：服务器注册中心 + 资源更新/OTA（同一进程、双端口）。
//!
//! 注册中心解决的是「不在同一个局域网，也能在大厅里看到服务器」：
//! - 游戏服务端进程启动时 `POST /api/servers/register` 上报自己的公网/域名 + 端口，
//!   之后每 5 秒心跳一次；
//! - 客户端启动后 `GET /api/servers` 拉列表，把返回的服务器并进大厅；
//! - 心跳停了超过 TTL，条目自动过期，列表里不会留死服务器。
//!
//! OTA 端口在同一个进程里提供 `/assets/version|state.json|diff`、整包/增量 zip、
//! iOS 安装页与 `.ipa` 下载。
//!
//! API：
//! ```text
//! GET  /api/health                        → {ok, servers, ttl_secs, ota}
//! GET  /api/servers                       → {ok, count, servers:[...]}
//! POST /api/servers/register              → {ok, id, addr, ttl_secs}
//! POST /api/servers/{id}/heartbeat        → {ok}
//! POST /api/servers/{id}/unregister       → {ok, removed}
//! ```
//!
//! 注册请求体：
//! ```json
//! {"id":"可选稳定实例 id","name":"客厅服","host":"1.2.3.4 或 game.example.com",
//!  "port":5099,"players":2,"max_players":100,"tick_hz":60}
//! ```
//! `host` 留空时用 TCP 源 IP 兜底；`addr` 由注册中心拼成可直接解析的
//! `host:port`（IPv6 会加方括号）。

pub mod assets;
pub mod http;
pub mod registry;

pub use assets::OtaSite;
pub use http::{HttpServer, WebState};
pub use registry::{RegisterRequest, RegisteredServer, Registry};
