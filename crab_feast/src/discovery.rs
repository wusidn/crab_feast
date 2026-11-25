//! 服务器发现：局域网 UDP 广播 + 中央注册中心（HTTP），合并成大厅里的一份列表。
//!
//! 两条来源各自独立，最后按 `SocketAddr` 合并：
//!
//! 1. **局域网广播**：服务端每两秒往 [`DISCOVERY_PORT`] 发一条 [`ServerAnnounce`]。
//!    用一个后台线程收包（阻塞 socket 不能塞进 Bevy 的帧循环），4 秒没再收到就认为下线。
//! 2. **注册中心**：客户端每 5 秒 `GET /api/servers` 拉一次，把跨网段/公网的服务器
//!    也列出来；12 秒没在列表里出现就从「注册中心来源」里移除（留出两次拉取的余量）。
//!
//! 同一台服务器可能同时被两条来源看到（同机/同局域网 + 又注册了）。合并成一行，
//! 展示时标成「局域网 + 注册中心」，不会出现两行重复的服务器。
//!
//! 同机多开客户端时，所有进程都绑同一个发现端口，所以 unix 上必须开 `SO_REUSEPORT`；
//! 只开 `SO_REUSEADDR` 的话第二个客户端会 bind 失败，只能看到一个空列表。

use std::{
    collections::{HashMap, HashSet},
    net::{IpAddr, Ipv4Addr, SocketAddr, ToSocketAddrs, UdpSocket},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};

use bevy::log::{debug, info, warn};
use bevy::prelude::*;
use crab_feast_library::net::{
    DEFAULT_REGISTRY_URL, DISCOVERY_MAGIC, DISCOVERY_PORT, ServerAnnounce,
};
use serde::Deserialize;
use socket2::{Domain, Protocol, Socket, Type};

/// 一条广播多久没再收到就从「局域网来源」里消失。
///
/// 广播间隔 2 秒，留两倍余量：丢一两个包不至于让服务器在列表里闪烁。
///
/// 广播是 2 秒一条，留够 3 条的空间。之前是 4 秒（正好 2 条），漏掉一条就过期，
/// 于是列表里那台服务器每 4.8 秒掉一次又回来，大厅跟着闪
const STALE_AFTER: Duration = Duration::from_secs(10);

/// 注册中心轮询间隔。5 秒足够让大厅「点开就有」，也不会把注册中心打疼。
const REGISTRY_POLL: Duration = Duration::from_secs(5);

/// 注册中心来源的存活时间。比轮询间隔多一倍多：偶发一次拉取失败，
/// 列表不会立刻空掉；但服务端真的下线时，最多 12 秒就会消失。
const REGISTRY_TTL: Duration = Duration::from_secs(12);

/// 单次 HTTP 拉取超时。注册中心不可达时不能把轮询线程卡太久。
const REGISTRY_TIMEOUT: Duration = Duration::from_secs(2);

/// 停止/退出时把长睡眠切成小片，`Drop` 里 join 才不会等满 5 秒。
const STOP_SLICE: Duration = Duration::from_millis(100);

/// 客户端连接注册中心的配置。
///
/// 作为资源插入 `App` 后，[`DiscoveryPlugin`] 在 build 阶段读取一次。
/// 不插也能工作：`Default` 会读 `CRAB_REGISTRY_URL`，没有就用内置默认地址。
#[derive(Resource, Clone, Debug)]
pub struct RegistryConfig {
    /// `None` = 关闭注册中心来源（只保留局域网广播）
    pub url: Option<String>,
}

impl RegistryConfig {
    /// `None` 表示「没有显式配置」→ 用环境变量或默认地址；
    /// `Some("off"/"")` 表示显式关闭。
    pub fn from_option(url: Option<String>) -> Self {
        let url = match url {
            Some(url) => {
                let trimmed = url.trim();
                if is_off(trimmed) {
                    None
                } else {
                    Some(trimmed.to_string())
                }
            }
            None => default_url(),
        };
        Self { url }
    }
}

impl Default for RegistryConfig {
    fn default() -> Self {
        Self::from_option(None)
    }
}

fn default_url() -> Option<String> {
    match std::env::var("CRAB_REGISTRY_URL") {
        Ok(value) => {
            let trimmed = value.trim();
            if is_off(trimmed) {
                None
            } else {
                Some(trimmed.to_string())
            }
        }
        Err(_) => Some(DEFAULT_REGISTRY_URL.to_string()),
    }
}

fn is_off(value: &str) -> bool {
    value.is_empty()
        || value.eq_ignore_ascii_case("off")
        || value.eq_ignore_ascii_case("none")
        || value.eq_ignore_ascii_case("disabled")
}

/// 大厅列表里的一台服务器。
///
/// 两条来源各记一个到期时间，而不是合成一个 `last_seen`：
/// 同一地址可能一边还在广播、另一边已经从注册中心列表里消失，
/// 合并展示时要能知道「另一个来源还活着」。
#[derive(Clone, Debug)]
pub struct DiscoveredServer {
    pub addr: SocketAddr,
    /// 大厅里展示的原始地址：注册中心可能是 `game.qiaqia.cyou:5099`，
    /// 局域网广播可能是 `[v6]:5099`。连接仍用解析后的 `addr`。
    pub display: String,
    pub name: String,
    pub players: u8,
    pub max_players: u8,
    /// 局域网广播的下一次过期时间
    pub lan_until: Option<Instant>,
    /// 注册中心列表的下一次过期时间
    pub registry_until: Option<Instant>,
    /// 局域网广播报的名字。注册中心的名字优先；注册中心过期后用它恢复显示。
    lan_name: Option<String>,
    /// 局域网广播报的地址串；注册中心过期后用它恢复展示。
    lan_display: Option<String>,
}

impl DiscoveredServer {
    fn placeholder(addr: SocketAddr) -> Self {
        Self {
            addr,
            display: addr.to_string(),
            name: addr.to_string(),
            players: 0,
            max_players: 0,
            lan_until: None,
            registry_until: None,
            lan_name: None,
            lan_display: None,
        }
    }

    fn lan_alive(&self, now: Instant) -> bool {
        self.lan_until.is_some_and(|until| until > now)
    }

    fn registry_alive(&self, now: Instant) -> bool {
        self.registry_until.is_some_and(|until| until > now)
    }

    fn alive(&self, now: Instant) -> bool {
        self.lan_alive(now) || self.registry_alive(now)
    }

    /// 大厅里展示的来源标签。
    pub fn source_label(&self) -> &'static str {
        let now = Instant::now();
        match (self.lan_alive(now), self.registry_alive(now)) {
            (true, true) => "局域网 + 注册中心",
            (true, false) => "局域网",
            (false, true) => "注册中心",
            (false, false) => "已离线",
        }
    }
}

/// 服务器列表的共享存储：后台线程写，UI 帧里读。
#[derive(Resource)]
pub struct ServerBrowser {
    inner: Arc<Mutex<HashMap<SocketAddr, DiscoveredServer>>>,
    stop: Arc<AtomicBool>,
    threads: Vec<JoinHandle<()>>,
}

impl ServerBrowser {
    /// 启动两个后台线程：局域网广播监听（总是启动）与注册中心轮询（配了地址才启动）。
    ///
    /// 端口绑不上（被别的程序占了且不支持复用）时只报一次警告，
    /// 大厅会显示「正在搜索」，本地游戏仍然可用。
    pub fn start(registry_url: Option<String>) -> Self {
        let inner = Arc::new(Mutex::new(HashMap::<SocketAddr, DiscoveredServer>::new()));
        let stop = Arc::new(AtomicBool::new(false));
        let mut threads = Vec::new();

        {
            let store = inner.clone();
            let flag = stop.clone();
            match thread::Builder::new()
                .name("server-discovery".to_string())
                .spawn(move || listen_lan(store, flag))
            {
                Ok(join) => threads.push(join),
                Err(error) => warn!("服务器发现线程启动失败：{error}"),
            }
        }

        if let Some(url) = registry_url {
            let store = inner.clone();
            let flag = stop.clone();
            match thread::Builder::new()
                .name("server-registry-poll".to_string())
                .spawn(move || poll_registry(store, flag, url))
            {
                Ok(join) => threads.push(join),
                Err(error) => warn!("注册中心轮询线程启动失败：{error}"),
            }
        }

        Self {
            inner,
            stop,
            threads,
        }
    }

    /// 当前在线的服务器，按名字排序（名字相同按地址）。
    pub fn snapshot(&self) -> Vec<DiscoveredServer> {
        let Ok(mut guard) = self.inner.lock() else {
            return Vec::new();
        };
        let now = Instant::now();
        // 注册中心那一份过期后，把名字还给局域网广播报的那个，
        // 不然会一直挂着注册中心最后一次的名字直到整行消失。
        for server in guard.values_mut() {
            if !server.registry_alive(now) {
                if let Some(lan_name) = server.lan_name.as_ref() {
                    if server.name != *lan_name {
                        server.name = lan_name.clone();
                    }
                }
                if let Some(lan_display) = server.lan_display.as_ref() {
                    if server.display != *lan_display {
                        server.display = lan_display.clone();
                    }
                }
            }
        }
        guard.retain(|_, server| server.alive(now));
        let mut servers: Vec<_> = guard.values().cloned().collect();
        servers.sort_by(|a, b| a.name.cmp(&b.name).then_with(|| a.addr.cmp(&b.addr)));
        servers
    }
}

impl Drop for ServerBrowser {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        for join in self.threads.drain(..) {
            let _ = join.join();
        }
    }
}

pub struct DiscoveryPlugin;

impl Plugin for DiscoveryPlugin {
    fn build(&self, app: &mut App) {
        // launcher 可能已经插了带 `--registry` 的配置；没有就用环境变量/默认
        let config = app
            .world()
            .get_resource::<RegistryConfig>()
            .cloned()
            .unwrap_or_default();
        app.insert_resource(config.clone());
        app.insert_resource(ServerBrowser::start(config.url));
    }
}

/// 局域网广播监听线程。
fn listen_lan(store: Arc<Mutex<HashMap<SocketAddr, DiscoveredServer>>>, stop: Arc<AtomicBool>) {
    let socket = match bind() {
        Ok(socket) => socket,
        Err(error) => {
            warn!("服务器发现端口 {DISCOVERY_PORT} 绑定失败：{error}（列表会是空的）");
            return;
        }
    };
    info!("正在监听服务器广播 udp:{DISCOVERY_PORT}");
    let mut buffer = [0u8; 2048];
    while !stop.load(Ordering::Relaxed) {
        let (len, source) = match socket.recv_from(&mut buffer) {
            Ok(packet) => packet,
            // 读超时：回到循环顶部检查停止标志
            Err(error)
                if matches!(
                    error.kind(),
                    std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
                ) =>
            {
                continue;
            }
            Err(error) => {
                warn!("服务器广播接收失败：{error}");
                thread::sleep(Duration::from_millis(300));
                continue;
            }
        };
        let Ok(text) = std::str::from_utf8(&buffer[..len]) else {
            continue;
        };
        let Ok(announce) = serde_json::from_str::<ServerAnnounce>(text) else {
            continue;
        };
        if announce.magic != DISCOVERY_MAGIC {
            continue;
        }
        let addr = announce_addr(&announce, source);
        let display = announce_display(&announce, addr);
        let Ok(mut guard) = store.lock() else {
            continue;
        };
        let name = announce.name.clone();
        let now = Instant::now();
        let entry = guard
            .entry(addr)
            .or_insert_with(|| DiscoveredServer::placeholder(addr));
        let was_alive = entry.lan_alive(now);
        entry.lan_name = Some(name.clone());
        entry.lan_display = Some(display.clone());
        // 注册中心报的名字/地址是人配的，优先保留；否则同一台服务器两条来源
        // 各报一个名字或地址，合并成一行后会每两秒跳一次。
        if !entry.registry_alive(now) {
            entry.name = name.clone();
            entry.display = display;
        }
        entry.players = announce.players;
        entry.max_players = announce.max_players;
        entry.lan_until = Some(now + STALE_AFTER);
        if !was_alive {
            info!("发现服务器 {name}：{addr}（局域网）");
        }
    }
}

/// 注册中心轮询线程。
fn poll_registry(
    store: Arc<Mutex<HashMap<SocketAddr, DiscoveredServer>>>,
    stop: Arc<AtomicBool>,
    base: String,
) {
    let base = base.trim_end_matches('/').to_string();
    let mut announced = false;
    let mut warned = false;

    loop {
        if stop.load(Ordering::Relaxed) {
            return;
        }
        match fetch_registry_servers(&base) {
            Ok(servers) => {
                if !announced {
                    info!("已连接服务器注册中心 {base}");
                    announced = true;
                }
                warned = false;
                let now = Instant::now();
                let mut seen = HashSet::new();
                let Ok(mut guard) = store.lock() else {
                    sleep_or_stop(&stop, REGISTRY_POLL);
                    continue;
                };
                for server in servers {
                    let Some(addr) = resolve_addr(&server.addr) else {
                        debug!("注册中心里的地址无法解析：{}", server.addr);
                        continue;
                    };
                    seen.insert(addr);
                    let entry = guard
                        .entry(addr)
                        .or_insert_with(|| DiscoveredServer::placeholder(addr));
                    let was_registered = entry.registry_alive(now);
                    if !server.name.is_empty() {
                        entry.name = server.name;
                    }
                    let display = server.addr.trim();
                    if !display.is_empty() {
                        entry.display = display.to_string();
                    }
                    entry.players = server.players;
                    entry.max_players = server.max_players;
                    entry.registry_until = Some(now + REGISTRY_TTL);
                    if !was_registered {
                        info!("发现服务器 {}：{}（注册中心）", entry.name, entry.display);
                    }
                }
                // 这一轮没返回的立刻从「注册中心来源」里撤掉；
                // 如果它还在局域网广播，lan_until 会让这一行继续留着（标签变成「局域网」）。
                for (addr, entry) in guard.iter_mut() {
                    if !seen.contains(addr) {
                        entry.registry_until = None;
                    }
                }
                guard.retain(|_, entry| entry.alive(now));
            }
            Err(error) => {
                // 注册中心挂了/重启中：不立刻清空列表，让已有条目自然过期；
                // 日志只报一次，恢复后再失败才重新报。
                if !warned {
                    warn!("注册中心 {base} 拉取失败：{error}（先保留已有列表）");
                    warned = true;
                }
            }
        }
        sleep_or_stop(&stop, REGISTRY_POLL);
    }
}

#[derive(Deserialize)]
struct ServerListResponse {
    #[serde(default)]
    servers: Vec<RegistryServerEntry>,
}

#[derive(Deserialize)]
struct RegistryServerEntry {
    #[serde(default)]
    name: String,
    #[serde(default)]
    addr: String,
    #[serde(default)]
    players: u8,
    #[serde(default)]
    max_players: u8,
}

/// 向注册中心拉当前服务器列表。
fn fetch_registry_servers(base: &str) -> Result<Vec<RegistryServerEntry>, String> {
    let url = format!("{base}/api/servers");
    let mut response = ureq::get(&url)
        .config()
        // 默认 ureq 会把 4xx/5xx 变成错误，这里要读注册中心返回的错误体
        .http_status_as_error(false)
        .timeout_global(Some(REGISTRY_TIMEOUT))
        .build()
        .call()
        .map_err(|error| error.to_string())?;
    let status = response.status().as_u16();
    let text = response
        .body_mut()
        .read_to_string()
        .map_err(|error| error.to_string())?;
    if !(200..300).contains(&status) {
        return Err(format!(
            "GET {url} 返回 HTTP {status}：{}",
            text.chars().take(200).collect::<String>()
        ));
    }
    let parsed: ServerListResponse =
        serde_json::from_str(&text).map_err(|error| format!("响应不是合法 JSON：{error}"))?;
    Ok(parsed.servers)
}

/// `host:port` → `SocketAddr`。
///
/// `addr` 可能是域名（服务端用 `--public-host` 报的），所以这里走 `ToSocketAddrs`
/// 做一次系统 DNS 解析，而不是只支持 IP 字面量。解析放在轮询线程里，允许阻塞。
fn resolve_addr(addr: &str) -> Option<SocketAddr> {
    let trimmed = addr.trim();
    if trimmed.is_empty() {
        return None;
    }
    trimmed.to_socket_addrs().ok()?.next()
}

fn sleep_or_stop(stop: &AtomicBool, total: Duration) {
    let mut left = total;
    while left > Duration::ZERO && !stop.load(Ordering::Relaxed) {
        let step = left.min(STOP_SLICE);
        thread::sleep(step);
        left = left.saturating_sub(step);
    }
}

/// 广播里写的地址能直接用就用；写的是 `0.0.0.0`（服务端绑了所有网卡）
/// 就退回「包的源 IP + 广播里的端口」。
fn announce_addr(announce: &ServerAnnounce, source: SocketAddr) -> SocketAddr {
    match announce.addr.parse::<SocketAddr>() {
        Ok(addr) if !addr.ip().is_unspecified() => addr,
        _ => SocketAddr::new(source.ip(), announce.port),
    }
}

/// 广播用于展示的地址串：正常情况用广播里写的（`[v6]:5099`），
/// 只有广播写的是 `0.0.0.0`/`[::]` 时才退回解析后的地址。
fn announce_display(announce: &ServerAnnounce, addr: SocketAddr) -> String {
    match announce.addr.parse::<SocketAddr>() {
        Ok(parsed) if !parsed.ip().is_unspecified() => announce.addr.trim().to_string(),
        _ => addr.to_string(),
    }
}

fn bind() -> std::io::Result<UdpSocket> {
    let socket = Socket::new(Domain::IPV4, Type::DGRAM, Some(Protocol::UDP))?;
    socket.set_reuse_address(true)?;
    #[cfg(unix)]
    socket.set_reuse_port(true)?;
    let address = SocketAddr::new(IpAddr::V4(Ipv4Addr::UNSPECIFIED), DISCOVERY_PORT);
    socket.bind(&address.into())?;
    socket.set_read_timeout(Some(Duration::from_secs(1)))?;
    Ok(socket.into())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// 端到端：起一个真实注册中心 → HTTP 注册 → 客户端拉列表。
    /// 覆盖「服务端 POST + 客户端 GET」两边的 JSON 字段是否对得上。
    #[test]
    fn fetch_real_registry_list() {
        let server = crab_feast_web::HttpServer::start(
            "127.0.0.1:0".parse().unwrap(),
            crab_feast_web::Registry::new(Duration::from_secs(30)),
        )
        .expect("测试注册中心应能绑定本机端口");
        let base = format!("http://{}", server.local_addr());
        let response = ureq::post(&format!("{base}/api/servers/register"))
            .header("Content-Type", "application/json")
            .send(
                json!({
                    "name": "端到端服",
                    "host": "127.0.0.1",
                    "port": 5099,
                    "players": 2,
                    "max_players": 100,
                    "tick_hz": 60,
                })
                .to_string(),
            )
            .expect("注册应返回 200");
        assert_eq!(response.status().as_u16(), 200);

        let entries = fetch_registry_servers(&base).expect("拉取应成功");
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].name, "端到端服");
        assert_eq!(entries[0].addr, "127.0.0.1:5099");
        assert_eq!(entries[0].players, 2);
        assert_eq!(entries[0].max_players, 100);
    }

    #[test]
    fn parses_registry_list() {
        let text = r#"{
            "ok": true,
            "count": 2,
            "servers": [
                {"id":"a","name":"客厅服","host":"192.168.1.5","port":5099,
                 "addr":"192.168.1.5:5099","players":3,"max_players":100,"tick_hz":60.0},
                {"id":"b","name":"机房服","host":"game.example.com","port":5000,
                 "addr":"game.example.com:5000","players":0,"max_players":100,"tick_hz":60.0}
            ]
        }"#;
        let parsed: ServerListResponse = serde_json::from_str(text).unwrap();
        assert_eq!(parsed.servers.len(), 2);
        assert_eq!(parsed.servers[0].name, "客厅服");
        assert_eq!(parsed.servers[1].addr, "game.example.com:5000");
    }

    #[test]
    fn registry_config_off_means_none() {
        assert_eq!(RegistryConfig::from_option(Some("off".into())).url, None);
        assert_eq!(RegistryConfig::from_option(Some("".into())).url, None);
        assert_eq!(
            RegistryConfig::from_option(Some("http://example.com:8090".into())).url,
            Some("http://example.com:8090".to_string())
        );
    }

    #[test]
    fn resolve_ip_literal_without_dns() {
        assert_eq!(
            resolve_addr("127.0.0.1:5099"),
            Some("127.0.0.1:5099".parse().unwrap())
        );
        assert_eq!(resolve_addr(""), None);
    }

    #[test]
    fn announce_display_prefers_public_address() {
        let mut announce = ServerAnnounce {
            magic: "crab".into(),
            name: "Mac 测试服".into(),
            addr: "[2409:8a3c:424:5ff0:1cda:21aa:9ba1:d1cd]:5099".into(),
            port: 5099,
            players: 0,
            max_players: 100,
            tick_hz: 60.0,
        };
        let source: SocketAddr = "127.0.0.1:5099".parse().unwrap();
        assert_eq!(
            announce_display(&announce, source),
            "[2409:8a3c:424:5ff0:1cda:21aa:9ba1:d1cd]:5099"
        );
        announce.addr = "0.0.0.0:5099".into();
        assert_eq!(announce_display(&announce, source), "127.0.0.1:5099");
    }

    #[test]
    fn merged_source_label() {
        let mut server = DiscoveredServer::placeholder("127.0.0.1:5099".parse().unwrap());
        let now = Instant::now();
        server.lan_until = Some(now + Duration::from_secs(4));
        server.registry_until = Some(now + Duration::from_secs(12));
        assert_eq!(server.source_label(), "局域网 + 注册中心");
        server.registry_until = None;
        assert_eq!(server.source_label(), "局域网");
    }
}
