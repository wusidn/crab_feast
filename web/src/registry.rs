//! 注册表：内存里维护「当前在线的游戏服务器」。
//!
//! 为什么要有 TTL：服务端进程可能被 kill / 断电 / 崩溃，没有优雅注销的机会。
//! 只靠 `unregister` 的话，列表里会永远挂着一台连不上的服务器。这里用
//! 「注册 + 定时心跳 + 超时剔除」：只要心跳停了，条目在 [`Registry::ttl`]
//! 之后自动消失，客户端看到的就是真实可连的服务器。
//!
//! 所有写操作都过同一把锁。注册中心是开发/自建规模的量级（几十台服务器），
//! 一次 list 就是一次哈希表遍历，不值得为它引入数据库或分片。

use std::{
    collections::HashMap,
    net::IpAddr,
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};

/// 注册中心里的一条服务器记录。
///
/// `host` / `port` 是服务端上报的对外地址，`addr` 是拼好的 `host:port`
/// （IPv6 自动加方括号），客户端直接拿它做 DNS 解析与连接。
#[derive(Clone, Debug)]
pub struct RegisteredServer {
    pub id: String,
    pub name: String,
    pub host: String,
    pub port: u16,
    /// 展示/连接用的完整地址，如 `192.168.1.5:5099` 或 `[::1]:5099`
    pub addr: String,
    pub players: u8,
    pub max_players: u8,
    pub tick_hz: f32,
    /// 最后一次注册/心跳的时间
    pub updated: Instant,
}

impl RegisteredServer {
    /// 距离最后一次心跳过去了多少秒（给客户端展示「刚刚更新」用）。
    pub fn age_secs(&self) -> f32 {
        self.updated.elapsed().as_secs_f32()
    }
}

/// 一次注册请求。字段都是服务端进程启动时实际持有的信息。
#[derive(Clone, Debug, Default)]
pub struct RegisterRequest {
    /// 稳定实例 id。同一台服务器重启后如果还带同一个 id，就直接覆盖旧条目，
    /// 不会在列表里留一条要等 TTL 才消失的幽灵记录。留空则由注册中心生成。
    pub id: Option<String>,
    pub name: String,
    /// 对外主机名或 IP。留空时退回请求来源 IP（同一台机器直连的场景够用）。
    pub host: Option<String>,
    pub port: u16,
    pub players: u8,
    pub max_players: u8,
    pub tick_hz: f32,
}

#[derive(Clone)]
pub struct Registry {
    inner: Arc<Mutex<Inner>>,
    ttl: Duration,
}

struct Inner {
    servers: HashMap<String, RegisteredServer>,
    /// 自增序号，用来给自动生成的 id 去重
    seq: u64,
}

impl Registry {
    /// `ttl` 是「多久没心跳就认为下线」。
    ///
    /// 服务端心跳 5 秒一次，取 15 秒（三倍）让「偶尔丢一两个请求」不至于
    /// 让服务器在列表里闪烁。
    pub fn new(ttl: Duration) -> Self {
        Self {
            inner: Arc::new(Mutex::new(Inner {
                servers: HashMap::new(),
                seq: 0,
            })),
            ttl,
        }
    }

    pub fn ttl(&self) -> Duration {
        self.ttl
    }

    /// 注册/覆盖一台服务器。
    ///
    /// `source_ip` 来自 TCP 连接的对端地址：上报里没写 host 时用它兜底，
    /// 这样最小配置（只报端口）也能在局域网里被连上。
    pub fn register(
        &self,
        request: RegisterRequest,
        source_ip: IpAddr,
    ) -> Result<RegisteredServer, String> {
        let name = clean_text(&request.name, "name", 64)?;
        let host = match request.host.as_deref().map(str::trim) {
            Some(host) if !host.is_empty() => clean_host(host)?,
            _ => source_ip.to_string(),
        };
        if request.port == 0 {
            return Err("port 不能为 0".to_string());
        }
        let max_players = request.max_players.max(1);
        let players = request.players.min(max_players);
        let tick_hz = if request.tick_hz.is_finite() && request.tick_hz > 0.0 {
            request.tick_hz
        } else {
            60.0
        };
        let addr = format_addr(&host, request.port);

        let mut inner = self.lock()?;
        let now = Instant::now();
        self.prune_locked(&mut inner, now);

        let id = match request.id.as_deref().and_then(valid_id) {
            Some(id) => id,
            None => {
                inner.seq += 1;
                let nanos = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map(|d| d.as_nanos())
                    .unwrap_or(0);
                format!("srv-{nanos:x}-{:x}", inner.seq)
            }
        };

        let server = RegisteredServer {
            id: id.clone(),
            name,
            host,
            port: request.port,
            addr,
            players,
            max_players,
            tick_hz,
            updated: now,
        };
        inner.servers.insert(id, server.clone());
        Ok(server)
    }

    /// 心跳：只允许还活着的 id 续命。
    ///
    /// 返回 `Err` 时服务端应当重新走一次 register（注册中心重启 / 条目过期）。
    pub fn heartbeat(&self, id: &str, players: u8, max_players: Option<u8>) -> Result<(), String> {
        let mut inner = self.lock()?;
        let now = Instant::now();
        self.prune_locked(&mut inner, now);
        let Some(server) = inner.servers.get_mut(id) else {
            return Err("未知或已过期的服务器 id".to_string());
        };
        if let Some(max) = max_players {
            server.max_players = max.max(1);
        }
        server.players = players.min(server.max_players);
        server.updated = now;
        Ok(())
    }

    /// 优雅注销（进程正常退出时调）。重复注销不算错误。
    pub fn unregister(&self, id: &str) -> bool {
        match self.inner.lock() {
            Ok(mut inner) => inner.servers.remove(id).is_some(),
            Err(_) => false,
        }
    }

    /// 当前在线服务器，按名字、地址排序，保证同一批数据每次返回顺序一致。
    pub fn list(&self) -> Vec<RegisteredServer> {
        let Ok(mut inner) = self.inner.lock() else {
            return Vec::new();
        };
        let now = Instant::now();
        self.prune_locked(&mut inner, now);
        let mut servers: Vec<_> = inner.servers.values().cloned().collect();
        servers.sort_by(|a, b| {
            a.name
                .cmp(&b.name)
                .then_with(|| a.addr.cmp(&b.addr))
                .then_with(|| a.id.cmp(&b.id))
        });
        servers
    }

    pub fn len(&self) -> usize {
        let Ok(mut inner) = self.inner.lock() else {
            return 0;
        };
        let now = Instant::now();
        self.prune_locked(&mut inner, now);
        inner.servers.len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    fn lock(&self) -> Result<std::sync::MutexGuard<'_, Inner>, String> {
        self.inner.lock().map_err(|_| "注册表锁已中毒".to_string())
    }

    fn prune_locked(&self, inner: &mut Inner, now: Instant) {
        let ttl = self.ttl;
        inner
            .servers
            .retain(|_, server| now.duration_since(server.updated) <= ttl);
    }
}

/// 文本字段：去首尾空白、限长、拒绝控制字符。
fn clean_text(value: &str, field: &str, max_chars: usize) -> Result<String, String> {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        return Err(format!("{field} 不能为空"));
    }
    if trimmed.chars().count() > max_chars {
        return Err(format!("{field} 超过 {max_chars} 个字符"));
    }
    if trimmed.chars().any(|c| c.is_control()) {
        return Err(format!("{field} 包含控制字符"));
    }
    Ok(trimmed.to_string())
}

/// 主机名/域名/IP：不允许空白和路径分隔符（它会被拼进 `addr`，别让客户端解析到怪东西）。
fn clean_host(host: &str) -> Result<String, String> {
    let trimmed = host.trim();
    if trimmed.chars().count() > 128 {
        return Err("host 超过 128 个字符".to_string());
    }
    if trimmed
        .chars()
        .any(|c| c.is_whitespace() || c.is_control() || c == '/' || c == '\\' || c == '@')
    {
        return Err("host 含非法字符".to_string());
    }
    Ok(trimmed.to_string())
}

/// id 只用于内存键与 URL 路径：限制成安全字符，避免路径里的奇怪内容被当成路由。
fn valid_id(raw: &str) -> Option<String> {
    let trimmed = raw.trim();
    let ok = (6..=80).contains(&trimmed.len())
        && trimmed
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'));
    ok.then(|| trimmed.to_string())
}

/// `host:port` 拼接。IPv6 字面量必须加方括号，否则 `SocketAddr` 解析不了。
pub fn format_addr(host: &str, port: u16) -> String {
    if host.contains(':') && !host.starts_with('[') {
        format!("[{host}]:{port}")
    } else {
        format!("{host}:{port}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn request(name: &str, port: u16) -> RegisterRequest {
        RegisterRequest {
            id: None,
            name: name.to_string(),
            host: Some("192.168.1.5".to_string()),
            port,
            players: 2,
            max_players: 100,
            tick_hz: 60.0,
        }
    }

    #[test]
    fn register_and_list() {
        let registry = Registry::new(Duration::from_secs(30));
        let server = registry
            .register(request("客厅服", 5099), "127.0.0.1".parse().unwrap())
            .unwrap();
        assert_eq!(server.addr, "192.168.1.5:5099");
        assert_eq!(registry.list().len(), 1);
    }

    #[test]
    fn heartbeat_extends_and_unknown_id_fails() {
        let registry = Registry::new(Duration::from_secs(30));
        let server = registry
            .register(request("客厅服", 5099), "127.0.0.1".parse().unwrap())
            .unwrap();
        assert!(registry.heartbeat(&server.id, 7, Some(100)).is_ok());
        assert_eq!(registry.list()[0].players, 7);
        assert!(registry.heartbeat("nope", 0, None).is_err());
    }

    #[test]
    fn ttl_expires_without_heartbeat() {
        let registry = Registry::new(Duration::from_millis(40));
        registry
            .register(request("临时服", 5000), "127.0.0.1".parse().unwrap())
            .unwrap();
        assert_eq!(registry.len(), 1);
        std::thread::sleep(Duration::from_millis(80));
        assert!(registry.list().is_empty());
        assert!(registry.is_empty());
    }

    #[test]
    fn host_falls_back_to_source_ip_and_ipv6_is_bracketed() {
        let mut req = request("host", 5000);
        req.host = None;
        let server = registry_register(req, "10.0.0.9".parse().unwrap());
        assert_eq!(server.addr, "10.0.0.9:5000");

        let mut req = request("v6", 5000);
        req.host = Some("::1".to_string());
        let server = registry_register(req, "127.0.0.1".parse().unwrap());
        assert_eq!(server.addr, "[::1]:5000");
    }

    #[test]
    fn re_register_with_same_id_replaces_entry() {
        let registry = Registry::new(Duration::from_secs(30));
        let mut req = request("旧名字", 5000);
        req.id = Some("stable-01".to_string());
        registry
            .register(req.clone(), "127.0.0.1".parse().unwrap())
            .unwrap();
        req.name = "新名字".to_string();
        registry
            .register(req, "127.0.0.1".parse().unwrap())
            .unwrap();
        let list = registry.list();
        assert_eq!(list.len(), 1, "同 id 应覆盖而不是新增");
        assert_eq!(list[0].name, "新名字");
    }

    #[test]
    fn invalid_input_is_rejected() {
        let registry = Registry::new(Duration::from_secs(30));
        let mut req = request("", 5000);
        assert!(
            registry
                .register(req.clone(), "127.0.0.1".parse().unwrap())
                .is_err()
        );
        req = request("ok", 0);
        assert!(
            registry
                .register(req, "127.0.0.1".parse().unwrap())
                .is_err()
        );
    }

    fn registry_register(req: RegisterRequest, source: IpAddr) -> RegisteredServer {
        Registry::new(Duration::from_secs(30))
            .register(req, source)
            .unwrap()
    }
}
