//! `crab-feast-web`：服务器注册中心 + 资源更新/OTA，同一个进程、两个端口。
//!
//! 启动：
//! ```sh
//! cargo run -p crab_feast_web -- --addr 0.0.0.0:8090 --ota-addr 0.0.0.0:8443
//! # 只跑注册中心
//! cargo run -p crab_feast_web -- --no-ota
//! # 8443 直连 TLS（保留原来 ipv6/direct 模式的自签证书）
//! cargo run -p crab_feast_web -- --cert certs/fullchain.pem --key certs/privkey.pem
//! ```
//!
//! 端口分工：
//! - `--addr`（默认 `0.0.0.0:8090`）：注册 API，明文 HTTP，给游戏服务端/客户端用；
//! - `--ota-addr`（默认 `0.0.0.0:8443`）：资源更新 + iOS 安装页；有 `--cert/--key` 时
//!   直接 TLS，没有时跑明文（给 tailscale funnel / 反代做上游）。
//!
//! 环境变量与参数等价：`CRAB_REGISTRY_ADDR`、`CRAB_REGISTRY_TTL_SECS`、`CRAB_OTA_ADDR`、
//! `CRAB_OTA_ROOT`、`CRAB_ASSET_STORE`、`CRAB_OTA_CERT`、`CRAB_OTA_KEY`。

use std::{net::SocketAddr, path::PathBuf, sync::Arc, thread, time::Duration};

use crab_feast_web::{HttpServer, OtaSite, Registry, WebState};

/// 默认监听所有网卡：游戏服务器可能在别的机器上，只在 127.0.0.1 上监听就收不到注册。
const DEFAULT_ADDR: &str = "0.0.0.0:8090";
const DEFAULT_OTA_ADDR: &str = "0.0.0.0:8443";
/// 心跳间隔的 3 倍。服务端 5 秒一条心跳，允许丢两条再判下线。
const DEFAULT_TTL_SECS: u64 = 15;

struct Options {
    addr: SocketAddr,
    ota_addr: SocketAddr,
    ota_root: PathBuf,
    asset_store: PathBuf,
    cert: Option<PathBuf>,
    key: Option<PathBuf>,
    no_ota: bool,
    no_registry: bool,
}

fn main() {
    let options = parse_options();
    let ttl_secs = parse_ttl();
    let registry = Registry::new(Duration::from_secs(ttl_secs));

    let ota = if options.no_ota {
        None
    } else {
        if !options.ota_root.join("assets/versions.json").is_file() {
            eprintln!(
                "警告：{} 下没有 assets/versions.json，OTA 接口会返回 503。\
                 先跑 scripts/dev/assets_store.py snapshot，或检查 --ota-root/--asset-store。",
                options.ota_root.display()
            );
        }
        Some(Arc::new(OtaSite::new(
            options.ota_root.clone(),
            options.asset_store.clone(),
        )))
    };
    let state = match &ota {
        Some(site) => WebState::with_ota(registry.clone(), site.clone()),
        None => WebState::new(registry.clone()),
    };

    let api = if options.no_registry {
        None
    } else {
        match HttpServer::start_with(options.addr, state.clone()) {
            Ok(server) => Some(server),
            Err(error) => exit_bind("注册 API", options.addr, error),
        }
    };
    if options.no_registry && options.no_ota {
        eprintln!("--no-registry 与 --no-ota 不能同时给，否则进程什么都不做");
        std::process::exit(2);
    }

    let ota_server = match &ota {
        Some(_) => {
            let result = match (&options.cert, &options.key) {
                (Some(cert), Some(key)) => {
                    HttpServer::start_tls(options.ota_addr, state.clone(), cert, key)
                }
                (None, None) => HttpServer::start_with(options.ota_addr, state.clone()),
                _ => {
                    eprintln!("--cert 与 --key 必须同时给，或者都不给（明文）");
                    std::process::exit(2);
                }
            };
            match result {
                Ok(server) => Some(server),
                Err(error) => exit_bind("OTA", options.ota_addr, error),
            }
        }
        None => None,
    };

    println!("crab-feast web 已启动（心跳超时 {} 秒）", ttl_secs);
    if let Some(api) = &api {
        println!(
            "  注册 API：http://{}{}",
            api.local_addr(),
            if api.local_addr().ip().is_unspecified() {
                "（所有网卡）"
            } else {
                ""
            }
        );
        println!("    GET  /api/health");
        println!("    GET  /api/servers");
        println!("    POST /api/servers/register");
        println!("    POST /api/servers/{{id}}/heartbeat");
        println!("    POST /api/servers/{{id}}/unregister");
    } else {
        println!("  注册 API 已禁用（--no-registry）");
    }
    if let Some(server) = &ota_server {
        let scheme = if options.cert.is_some() {
            "https"
        } else {
            "http"
        };
        println!("  资源更新：{scheme}://{}", server.local_addr());
        println!("    GET  /assets/version");
        println!("    GET  /assets/state.json");
        println!("    GET  /assets/diff?from=<版本>");
        println!("    GET  /assets/full/<版本>.zip");
        println!("    GET  /assets/deltas/<from>_<to>.zip");
        println!("    GET  /             iOS 安装页");
        println!("    GET  /manifest.plist");
        println!("    GET  /<name>.ipa");
        println!("  资源根目录：{}", options.ota_root.display());
    } else {
        println!("  OTA 已禁用（--no-ota）");
    }
    println!("Ctrl-C 退出。");

    // 接受连接在后台线程；主线程只负责把进程挂住。
    loop {
        thread::park();
    }
}

fn exit_bind(what: &str, addr: SocketAddr, error: std::io::Error) -> ! {
    eprintln!("{what} 监听 {addr} 失败：{error}");
    if error.kind() == std::io::ErrorKind::AddrInUse {
        eprintln!("端口已被占用：先停掉另一个 crab-feast-web 进程。");
    }
    std::process::exit(2);
}

fn parse_options() -> Options {
    let mut addr = env("CRAB_REGISTRY_ADDR");
    let mut ota_addr = env("CRAB_OTA_ADDR");
    let mut ota_root = env("CRAB_OTA_ROOT").map(PathBuf::from);
    let mut asset_store = env("CRAB_ASSET_STORE").map(PathBuf::from);
    let mut cert = env("CRAB_OTA_CERT").map(PathBuf::from);
    let mut key = env("CRAB_OTA_KEY").map(PathBuf::from);
    let mut no_ota = env("CRAB_WEB_NO_OTA")
        .map(|value| !value.is_empty() && value != "0" && value != "false")
        .unwrap_or(false);
    let mut no_registry = env("CRAB_WEB_NO_REGISTRY")
        .map(|value| !value.is_empty() && value != "0" && value != "false")
        .unwrap_or(false);

    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--addr" => addr = args.next(),
            "--ota-addr" => ota_addr = args.next(),
            "--ota-root" => ota_root = args.next().map(PathBuf::from),
            "--asset-store" => asset_store = args.next().map(PathBuf::from),
            "--cert" => cert = args.next().map(PathBuf::from),
            "--key" => key = args.next().map(PathBuf::from),
            "--no-ota" => no_ota = true,
            "--no-registry" => no_registry = true,
            "--help" | "-h" => {
                print_help();
                std::process::exit(0);
            }
            other => eprintln!("未知参数 {other}，已忽略"),
        }
    }

    let ota_root = ota_root.unwrap_or_else(default_ota_root);
    let asset_store = asset_store.unwrap_or_else(|| ota_root.join("assets"));

    Options {
        addr: parse_socket_addr(addr.unwrap_or_else(|| DEFAULT_ADDR.to_string()), "注册 API"),
        ota_addr: parse_socket_addr(
            ota_addr.unwrap_or_else(|| DEFAULT_OTA_ADDR.to_string()),
            "OTA",
        ),
        ota_root,
        asset_store,
        cert,
        key,
        no_ota,
        no_registry,
    }
}

fn parse_socket_addr(raw: String, what: &str) -> SocketAddr {
    raw.parse().unwrap_or_else(|error| {
        eprintln!("{what} 监听地址 {raw} 无法解析：{error}");
        std::process::exit(2);
    })
}

fn print_help() {
    println!(
        "crab-feast-web [选项]\n\
         \n\
         --addr <ip:port>        注册 API 监听地址（默认 {DEFAULT_ADDR}）\n\
         --ota-addr <ip:port>    OTA 监听地址（默认 {DEFAULT_OTA_ADDR}）\n\
         --ota-root <dir>        OTA 根目录（安装页/ipa/证书，默认 ~/.crab_feast-ota）\n\
         --asset-store <dir>     资源仓库（默认 <ota-root>/assets）\n\
         --cert <pem>            OTA TLS 证书；与 --key 同时给才启用 HTTPS\n\
         --key <pem>             OTA TLS 私钥\n\
         --no-ota                只跑注册 API，不监听 {DEFAULT_OTA_ADDR}\n\
         --no-registry           只跑 OTA，不监听 {DEFAULT_ADDR}\n\
         \n\
         环境变量：CRAB_REGISTRY_ADDR、CRAB_REGISTRY_TTL_SECS、CRAB_OTA_ADDR、\n\
         CRAB_OTA_ROOT、CRAB_ASSET_STORE、CRAB_OTA_CERT、CRAB_OTA_KEY、\n\
         CRAB_WEB_NO_OTA、CRAB_WEB_NO_REGISTRY"
    );
}

fn env(name: &str) -> Option<String> {
    std::env::var(name)
        .ok()
        .filter(|value| !value.trim().is_empty())
}

fn default_ota_root() -> PathBuf {
    if let Some(home) = std::env::var_os("HOME") {
        return PathBuf::from(home).join(".crab_feast-ota");
    }
    PathBuf::from(".crab_feast-ota")
}

fn parse_ttl() -> u64 {
    env("CRAB_REGISTRY_TTL_SECS")
        .and_then(|value| value.parse().ok())
        .filter(|secs| *secs > 0)
        .unwrap_or(DEFAULT_TTL_SECS)
}
