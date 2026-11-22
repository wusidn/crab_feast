//! 启动时向中央 Web 服务（`crab-feast-web`）注册自己，并定时心跳。
//!
//! 为什么要独立线程而不是塞进 Bevy 的 Update：
//! - 启动阶段网络可能还没就绪（DNS 没起来 / 注册中心刚启动），注册请求要能重试；
//! - `ureq` 是阻塞 IO，放在帧循环里会让服务器在注册中心不可达时卡住整帧；
//! - 心跳周期（5 秒）和 tick 频率（60Hz）不是一个量级，没必要每帧检查。
//!
//! 失败策略：
//! - 注册失败 → 每 2 秒重试，日志限流（同一条错误最多 30 秒报一次）；
//! - 心跳失败（含 410 过期）→ 丢掉 id、下一轮重新注册；
//! - 进程正常退出 → `Drop` 里停线程并尽量发一次 `unregister`，不让列表里留死条目
//!   （被 kill 的情况靠注册中心的 TTL 兜底）。
//!
//! 对外地址（host）的确定顺序：
//! 1. `--public-host` / `launcher.json` 的 `server.public_host` / `CRAB_PUBLIC_HOST`
//!    —— 域名或公网 IP 都行；
//! 2. 监听地址本身不是 `0.0.0.0` 时，用它（例如绑了具体网卡）；
//! 3. 绑的是 `0.0.0.0` 时探测本机默认出口网卡的 IP（UDP connect 不发包）；
//! 4. 都拿不到就回退 `127.0.0.1` 并警告——这时外网客户端连不上，必须显式配置。

use std::{
    net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr, ToSocketAddrs, UdpSocket},
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicU8, Ordering},
    },
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};

use bevy::log::{debug, info, warn};
use bevy::prelude::Resource;
use crab_feast_library::net::TICK_HZ;
use serde_json::{Value, json};

use crate::beacon;

/// 心跳间隔。注册中心的默认 TTL 是 15 秒（三倍），丢两条心跳也不会掉线。
const HEARTBEAT: Duration = Duration::from_secs(5);
/// 注册失败后的重试间隔。
const RETRY: Duration = Duration::from_secs(2);
/// 单次 HTTP 请求的总超时。注册中心是本机/局域网服务，2 秒足够。
const HTTP_TIMEOUT: Duration = Duration::from_secs(2);
/// 退出时把 5 秒的睡眠切成小片，保证 `Drop` 能很快看到停止标志。
const STOP_SLICE: Duration = Duration::from_millis(100);
/// 同一条错误日志的最短间隔，避免注册中心没起时每 2 秒刷一行。
const WARN_EVERY: Duration = Duration::from_secs(30);

/// 把 `--public-host` 的域名/IP 解析成客户端可直连的地址。
///
/// IPv6 字面量必须补方括号，否则 `2409:...:5099` 不是一个合法的 `SocketAddr`。
/// 广播和注册中心都走这个结果，保证两条来源报的是同一个地址、不会在大厅里裂成两行。
pub fn advertised_addr(host: &str, port: u16) -> Option<SocketAddr> {
    let host = host.trim();
    if host.is_empty() {
        return None;
    }
    let host = host
        .strip_prefix('[')
        .and_then(|host| host.strip_suffix(']'))
        .unwrap_or(host);
    let candidate = if host.contains(':') {
        format!("[{host}]:{port}")
    } else {
        format!("{host}:{port}")
    };
    candidate.to_socket_addrs().ok()?.next()
}

/// 日志里的 `host:port`；IPv6 加方括号，否则看起来像端口被拆开。
fn host_port(host: &str, port: u16) -> String {
    if host.contains(':') && !host.starts_with('[') {
        format!("[{host}]:{port}")
    } else {
        format!("{host}:{port}")
    }
}

/// 注册配置：地址、对外主机名、展示名。
#[derive(Clone, Debug)]
pub struct Config {
    /// 注册中心基地址，例如 `http://127.0.0.1:8090`
    pub url: String,
    /// 对外上报的 host（域名 / 公网 IP / 局域网 IP）；`None` 时自动判断
    pub public_host: Option<String>,
    /// 展示名；`None` 时用「机器名 · 端口」
    pub name: Option<String>,
}

/// 已启动的注册线程。作为 Bevy 资源插入，`Drop` 时自动注销。
#[derive(Resource)]
pub struct Registration {
    stop: Arc<AtomicBool>,
    players: Arc<AtomicU8>,
    join: Option<JoinHandle<()>>,
}

impl Registration {
    /// 起后台线程。端口为 0（系统随机分配）时无法在注册前知道真实端口，直接跳过。
    pub fn start(bind: SocketAddr, config: Config, max_players: u8) -> Option<Self> {
        if bind.port() == 0 {
            warn!("监听端口是 0（系统随机分配），无法注册；请显式指定端口");
            return None;
        }
        let host = config
            .public_host
            .as_deref()
            .map(str::trim)
            .filter(|host| !host.is_empty())
            .map(str::to_string)
            .unwrap_or_else(|| resolve_host(bind));
        let name = config
            .name
            .as_deref()
            .map(str::trim)
            .filter(|name| !name.is_empty())
            .map(str::to_string)
            .unwrap_or_else(|| beacon::server_name(bind));
        let base = config.url.trim_end_matches('/').to_string();
        // 稳定实例 id：同一进程的多次注册都覆盖同一条记录；进程重启后换新 id，
        // 旧记录由 TTL 清掉（不会因为端口相同而互相覆盖）。
        let instance_id = instance_id(&host);

        let stop = Arc::new(AtomicBool::new(false));
        let players = Arc::new(AtomicU8::new(0));
        let stop_flag = stop.clone();
        let players_flag = players.clone();
        info!(
            "向注册中心报到：{base}（对外 {}，名字 {name}）",
            host_port(&host, bind.port())
        );

        let join = match thread::Builder::new()
            .name("server-registration".to_string())
            .spawn(move || {
                run(
                    base,
                    host,
                    bind.port(),
                    name,
                    instance_id,
                    max_players,
                    players_flag,
                    stop_flag,
                )
            }) {
            Ok(join) => join,
            Err(error) => {
                warn!("注册线程启动失败：{error}（大厅里不会出现这台服务器）");
                return None;
            }
        };

        Some(Self {
            stop,
            players,
            join: Some(join),
        })
    }

    /// 每个逻辑帧把房间人数写进共享原子，下一次心跳就会带出去。
    pub fn set_players(&self, players: u8) {
        self.players.store(players, Ordering::Relaxed);
    }
}

impl Drop for Registration {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(join) = self.join.take() {
            // 线程最多在一个 100ms 睡眠片或一次 2 秒 HTTP 请求后退出；
            // 正常关服时这点等待值得，能换掉列表里 15 秒的死条目。
            let _ = join.join();
        }
    }
}

/// 注册线程主循环。
fn run(
    base: String,
    host: String,
    port: u16,
    name: String,
    instance_id: String,
    max_players: u8,
    players: Arc<AtomicU8>,
    stop: Arc<AtomicBool>,
) {
    let register_url = format!("{base}/api/servers/register");
    let mut current_id: Option<String> = None;
    let mut last_problem = String::new();
    let mut last_warn = Instant::now()
        .checked_sub(WARN_EVERY)
        .unwrap_or_else(Instant::now);

    loop {
        if stop.load(Ordering::Relaxed) {
            if let Some(current) = current_id.as_deref() {
                // 退出前尽力注销；失败也无所谓，TTL 会兜底
                let url = format!("{base}/api/servers/{current}/unregister");
                match post_json(&url, &json!({})) {
                    Ok((status, _)) if (200..300).contains(&status) => {
                        info!("已从注册中心注销 {current}");
                    }
                    Ok((status, _)) => warn!("注销返回 HTTP {status}"),
                    Err(error) => warn!("注销失败：{error}"),
                }
            }
            return;
        }

        match current_id.as_deref() {
            None => {
                let body = json!({
                    // 固定实例 id：注册中心按它覆盖，重复注册不会产生多条记录
                    "id": instance_id,
                    "name": name,
                    "host": host,
                    "port": port,
                    "players": players.load(Ordering::Relaxed),
                    "max_players": max_players,
                    "tick_hz": TICK_HZ,
                });
                match post_json(&register_url, &body) {
                    Ok((status, value)) if (200..300).contains(&status) => {
                        match value.get("id").and_then(Value::as_str) {
                            Some(assigned) => {
                                info!(
                                    "注册成功：id={assigned}，客户端已能在列表里看到 {}",
                                    host_port(&host, port)
                                );
                                current_id = Some(assigned.to_string());
                                last_problem.clear();
                            }
                            None => note_problem(
                                &mut last_problem,
                                &mut last_warn,
                                "注册响应缺少 id".to_string(),
                            ),
                        }
                    }
                    Ok((status, value)) => note_problem(
                        &mut last_problem,
                        &mut last_warn,
                        format!(
                            "注册被拒绝（HTTP {status}）：{}",
                            value.get("error").and_then(Value::as_str).unwrap_or("")
                        ),
                    ),
                    Err(error) => note_problem(
                        &mut last_problem,
                        &mut last_warn,
                        format!("注册中心不可达：{error}"),
                    ),
                }
                sleep_or_stop(&stop, RETRY);
            }
            Some(current) => {
                let url = format!("{base}/api/servers/{current}/heartbeat");
                let body = json!({
                    "players": players.load(Ordering::Relaxed),
                    "max_players": max_players,
                });
                match post_json(&url, &body) {
                    Ok((status, _)) if (200..300).contains(&status) => {
                        debug!("心跳正常：{current}");
                        last_problem.clear();
                    }
                    Ok((status, _)) => {
                        // 410 = 条目过期或注册中心重启：丢掉 id，下一轮重新注册
                        warn!("心跳 HTTP {status}，重新注册");
                        current_id = None;
                        sleep_or_stop(&stop, RETRY);
                        continue;
                    }
                    Err(error) => {
                        // 网络抖动：保留 id 再试一次，注册中心 TTL 还没到就还在列表里
                        note_problem(
                            &mut last_problem,
                            &mut last_warn,
                            format!("心跳失败：{error}"),
                        );
                    }
                }
                sleep_or_stop(&stop, HEARTBEAT);
            }
        }
    }
}

/// 同一条错误日志限流：错误内容变了立刻报，否则最多 30 秒一次。
fn note_problem(last: &mut String, at: &mut Instant, message: String) {
    if last != &message || at.elapsed() >= WARN_EVERY {
        warn!("{message}（会继续重试）");
        *last = message;
        *at = Instant::now();
    }
}

fn sleep_or_stop(stop: &AtomicBool, total: Duration) {
    let mut left = total;
    while left > Duration::ZERO && !stop.load(Ordering::Relaxed) {
        let step = left.min(STOP_SLICE);
        thread::sleep(step);
        left = left.saturating_sub(step);
    }
}

/// 发一条 JSON POST，返回状态码与解析后的响应体（解析失败当空值）。
///
/// `http_status_as_error(false)` 是必须的：默认 ureq 会把 4xx/5xx 变成错误，
/// 我们就读不到注册中心返回的 `{"error": "..."}`，排查时会只剩一句「HTTP status」。
fn post_json(url: &str, body: &Value) -> Result<(u16, Value), String> {
    let text = serde_json::to_string(body).map_err(|error| error.to_string())?;
    let mut response = ureq::post(url)
        .config()
        .http_status_as_error(false)
        .timeout_global(Some(HTTP_TIMEOUT))
        .build()
        .header("Content-Type", "application/json")
        .send(text)
        .map_err(|error| error.to_string())?;
    let status = response.status().as_u16();
    let text = response
        .body_mut()
        .read_to_string()
        .map_err(|error| error.to_string())?;
    let value = if text.is_empty() {
        Value::Null
    } else {
        serde_json::from_str(&text).unwrap_or(Value::Null)
    };
    Ok((status, value))
}

/// 决定对外上报的 host。
fn resolve_host(bind: SocketAddr) -> String {
    match bind.ip() {
        IpAddr::V4(ip) if ip.is_unspecified() => match detect_outbound_v4() {
            Some(ip) => ip.to_string(),
            None => {
                warn!(
                    "服务器绑在 0.0.0.0，但探测不到本机网卡地址；注册上报 127.0.0.1。\
                     外网客户端连不上，请用 --public-host 指定域名/公网 IP"
                );
                Ipv4Addr::LOCALHOST.to_string()
            }
        },
        IpAddr::V6(ip) if ip.is_unspecified() => match detect_outbound_v6() {
            Some(ip) => ip.to_string(),
            None => {
                warn!(
                    "服务器绑在 [::]，但探测不到本机网卡地址；注册上报 ::1。\
                     外网客户端连不上，请用 --public-host 指定域名/公网 IP"
                );
                Ipv6Addr::LOCALHOST.to_string()
            }
        },
        ip => ip.to_string(),
    }
}

/// 让内核按默认路由选一个源地址。`connect` 一个不可达目标也不会真的发包，
/// 只是借路由表拿到「本机出口网卡」的地址，比遍历网卡简单且跨平台。
fn detect_outbound_v4() -> Option<IpAddr> {
    let socket = UdpSocket::bind((Ipv4Addr::UNSPECIFIED, 0)).ok()?;
    socket.connect((Ipv4Addr::new(8, 8, 8, 8), 80)).ok()?;
    let ip = socket.local_addr().ok()?.ip();
    (!ip.is_unspecified() && !ip.is_loopback()).then_some(ip)
}

fn detect_outbound_v6() -> Option<IpAddr> {
    let socket = UdpSocket::bind((Ipv6Addr::UNSPECIFIED, 0)).ok()?;
    let target = SocketAddr::new(
        IpAddr::V6(Ipv6Addr::new(0x2001, 0x4860, 0x4860, 0, 0, 0, 0, 0x8888)),
        80,
    );
    socket.connect(target).ok()?;
    let ip = socket.local_addr().ok()?.ip();
    (!ip.is_unspecified() && !ip.is_loopback()).then_some(ip)
}

/// 进程级稳定 id。字符集必须落在注册中心的校验范围内（ASCII 字母数字 `-_.`）。
fn instance_id(host: &str) -> String {
    let machine = std::env::var("HOSTNAME")
        .or_else(|_| std::env::var("COMPUTERNAME"))
        .unwrap_or_else(|_| "host".to_string());
    let machine: String = machine
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
        .take(24)
        .collect();
    let pid = std::process::id();
    let short_host: String = host
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
        .take(16)
        .collect();
    format!("crab-{machine}-{short_host}-{pid}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn instance_id_is_registry_safe() {
        let id = instance_id("192.168.1.5");
        assert!(id.len() >= 6 && id.len() <= 80, "{id}");
        assert!(
            id.chars()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.')),
            "{id}"
        );
    }

    #[test]
    fn bind_ip_is_used_when_not_unspecified() {
        assert_eq!(
            resolve_host("192.168.1.7:5000".parse().unwrap()),
            "192.168.1.7"
        );
        assert_eq!(resolve_host("127.0.0.1:5000".parse().unwrap()), "127.0.0.1");
    }

    #[test]
    fn public_ipv6_is_bracketed_for_clients() {
        let host = "2409:8a3c:424:5ff0:1cda:21aa:9ba1:d1cd";
        let addr = advertised_addr(host, 5099).expect("IPv6 字面量应能解析");
        assert!(addr.is_ipv6());
        assert_eq!(addr.port(), 5099);
        assert_eq!(host_port(host, 5099), format!("[{host}]:5099"));
    }
}
