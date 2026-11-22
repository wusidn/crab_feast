//! 极简 HTTP/1.1 服务：注册中心 JSON API + OTA 静态/资源接口。
//!
//! 为什么不用 axum/tokio：这里最多几十个并发小请求 + 偶尔几个 GB 级下载，用
//! `std::net::TcpListener` + 每连接一个线程足够，且不引入异步运行时。大文件走
//! `Body::File` 流式写出，不会先把整个 zip 读进内存。
//!
//! 约定：
//! - 每个连接只处理一个请求，响应完就关（`Connection: close`），不实现 keep-alive；
//! - 请求体上限 [`MAX_BODY_BYTES`]，请求头上限 [`MAX_HEADER_BYTES`]；
//! - API 连接读写 10 秒超时；文件下载写超时 5 分钟，避免慢网被 2 秒掐断；
//! - 支持 `HEAD` 与单段 `Range`（iOS 下载大 ipa 断线续传靠它）；
//! - 所有响应带 CORS 头；OTA 只暴露安装页/manifest/ipa 与资源包，不暴露证书和仓库。

use std::{
    collections::HashMap,
    fs::File,
    io::{BufRead, BufReader, Read, Seek, SeekFrom, Write},
    net::{IpAddr, SocketAddr, TcpListener},
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    thread::{self, JoinHandle},
    time::Duration,
};

use base64::Engine;
use rustls::{
    ServerConfig, ServerConnection, StreamOwned,
    pki_types::{
        CertificateDer, PrivateKeyDer, PrivatePkcs1KeyDer, PrivatePkcs8KeyDer, PrivateSec1KeyDer,
    },
};
use serde_json::{Value, json};

use crate::{
    assets::OtaSite,
    registry::{RegisterRequest, Registry},
};

/// API/小请求的读写超时。
const IO_TIMEOUT: Duration = Duration::from_secs(10);
/// 大文件下载的写超时。700MB 的 ipa 在慢网下也不能因为 2 秒没写出就断开。
const FILE_WRITE_TIMEOUT: Duration = Duration::from_secs(300);
/// 请求体上限：注册请求只有几百字节，给 16KB 已经非常宽松。
const MAX_BODY_BYTES: usize = 16 * 1024;
/// 请求头上限，防止有人用超长 header 撑内存。
const MAX_HEADER_BYTES: usize = 8 * 1024;

/// 路由共享状态。`HttpServer` 的两个监听（8090 API / 8443 OTA）拿的是同一份。
#[derive(Clone)]
pub struct WebState {
    registry: Registry,
    ota: Option<Arc<OtaSite>>,
}

impl WebState {
    pub fn new(registry: Registry) -> Self {
        Self {
            registry,
            ota: None,
        }
    }

    pub fn with_ota(registry: Registry, ota: Arc<OtaSite>) -> Self {
        Self {
            registry,
            ota: Some(ota),
        }
    }
}

/// 已启动的 HTTP 服务。`Drop` 会通知 accept 线程退出并 join。
pub struct HttpServer {
    local_addr: SocketAddr,
    stop: Arc<AtomicBool>,
    join: Option<JoinHandle<()>>,
}

impl HttpServer {
    /// 只提供注册 API（测试与旧调用方用）。
    pub fn start(addr: SocketAddr, registry: Registry) -> std::io::Result<Self> {
        Self::start_with(addr, WebState::new(registry))
    }

    /// 绑定 `addr` 并起明文 HTTP 接受线程。
    ///
    /// 传 `127.0.0.1:0` 可以让系统分配端口，测试里用 `local_addr()` 取实际地址。
    pub fn start_with(addr: SocketAddr, state: WebState) -> std::io::Result<Self> {
        start_listener(addr, state, None, "web-http")
    }

    /// 绑定 `addr` 并起 TLS HTTP 接受线程（`cert`/`key` 都是 PEM）。
    pub fn start_tls(
        addr: SocketAddr,
        state: WebState,
        cert: &Path,
        key: &Path,
    ) -> std::io::Result<Self> {
        let config = load_tls_config(cert, key)?;
        start_listener(addr, state, Some(config), "web-https")
    }

    pub fn local_addr(&self) -> SocketAddr {
        self.local_addr
    }
}

impl Drop for HttpServer {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(join) = self.join.take() {
            let _ = join.join();
        }
    }
}

fn start_listener(
    addr: SocketAddr,
    state: WebState,
    tls: Option<Arc<ServerConfig>>,
    name: &str,
) -> std::io::Result<HttpServer> {
    let listener = TcpListener::bind(addr)?;
    listener.set_nonblocking(true)?;
    let local_addr = listener.local_addr()?;
    let stop = Arc::new(AtomicBool::new(false));
    let stop_flag = stop.clone();
    let join = thread::Builder::new()
        .name(name.to_string())
        .spawn(move || accept_loop(listener, state, tls, stop_flag))?;
    Ok(HttpServer {
        local_addr,
        stop,
        join: Some(join),
    })
}

fn accept_loop(
    listener: TcpListener,
    state: WebState,
    tls: Option<Arc<ServerConfig>>,
    stop: Arc<AtomicBool>,
) {
    while !stop.load(Ordering::Relaxed) {
        match listener.accept() {
            Ok((stream, peer)) => {
                let state = state.clone();
                let tls = tls.clone();
                let _ = thread::Builder::new()
                    .name("web-conn".to_string())
                    .spawn(move || {
                        // macOS/BSD 上 accept 出来的 socket 会继承 listener 的非阻塞标志；
                        // 不显式改回阻塞，大文件写满发送缓冲区就会 WouldBlock 中断。
                        let _ = stream.set_nonblocking(false);
                        let _ = stream.set_read_timeout(Some(IO_TIMEOUT));
                        let _ = stream.set_write_timeout(Some(FILE_WRITE_TIMEOUT));
                        let source = peer.ip();
                        match tls {
                            Some(config) => match ServerConnection::new(config) {
                                Ok(conn) => {
                                    let mut tls = StreamOwned::new(conn, stream);
                                    handle_connection(&mut tls, &state, source);
                                }
                                Err(error) => {
                                    let _ = error;
                                }
                            },
                            None => {
                                let mut stream = stream;
                                handle_connection(&mut stream, &state, source);
                            }
                        }
                    });
            }
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                thread::sleep(Duration::from_millis(15));
            }
            Err(_) => {
                // 文件描述符耗尽之类的瞬时错误：退让一下再继续，不要把 accept 线程打死
                thread::sleep(Duration::from_millis(50));
            }
        }
    }
}

struct Request {
    method: String,
    path: String,
    query: String,
    headers: HashMap<String, String>,
    body: Vec<u8>,
}

impl Request {
    fn header(&self, name: &str) -> Option<&str> {
        self.headers.get(name).map(String::as_str)
    }
}

fn handle_connection<S: Read + Write>(stream: &mut S, state: &WebState, source: IpAddr) {
    let request = match read_request(stream) {
        Ok(Some(request)) => request,
        Ok(None) => return,
        Err(message) => {
            let response = Response::json(400, json!({ "ok": false, "error": message }));
            let _ = write_response(stream, &response, false);
            return;
        }
    };
    let head_only = request.method == "HEAD";
    let response = route(state, &request, source);
    let _ = write_response(stream, &response, head_only);
}

fn read_request<S: Read>(stream: &mut S) -> Result<Option<Request>, String> {
    let mut reader = BufReader::new(stream);

    let mut request_line = String::new();
    let read = reader
        .read_line(&mut request_line)
        .map_err(|e| format!("读取请求行失败：{e}"))?;
    if read == 0 {
        // 对端连上就关（端口扫描/健康检查），正常结束
        return Ok(None);
    }
    let line = request_line.trim_end();
    let mut parts = line.split_whitespace();
    let method = parts.next().ok_or("请求行缺少 method")?.to_string();
    let target = parts.next().ok_or("请求行缺少路径")?.to_string();
    // HTTP/1.1 的版本号不参与路由，但要确认不是怪东西
    let version = parts.next().unwrap_or("");
    if !version.starts_with("HTTP/") {
        return Err("不是 HTTP 请求".to_string());
    }
    let (path, query) = match target.split_once('?') {
        Some((path, query)) => (path.to_string(), query.to_string()),
        None => (target, String::new()),
    };

    let mut content_length = 0usize;
    let mut header_bytes = 0usize;
    let mut headers = HashMap::new();
    loop {
        let mut header = String::new();
        let read = reader
            .read_line(&mut header)
            .map_err(|e| format!("读取请求头失败：{e}"))?;
        if read == 0 {
            return Err("请求头意外结束".to_string());
        }
        if header == "\r\n" || header == "\n" {
            break;
        }
        header_bytes += header.len();
        if header_bytes > MAX_HEADER_BYTES {
            return Err("请求头过大".to_string());
        }
        if let Some((name, value)) = header.split_once(':') {
            let name = name.trim().to_ascii_lowercase();
            let value = value.trim().to_string();
            if name == "content-length" {
                content_length = value
                    .parse::<usize>()
                    .map_err(|_| "Content-Length 不是数字".to_string())?;
            }
            headers.insert(name, value);
        }
    }
    if content_length > MAX_BODY_BYTES {
        return Err(format!(
            "请求体过大（{content_length} 字节，上限 {MAX_BODY_BYTES}）"
        ));
    }
    let mut body = vec![0u8; content_length];
    if content_length > 0 {
        reader
            .read_exact(&mut body)
            .map_err(|e| format!("读取请求体失败：{e}"))?;
    }
    Ok(Some(Request {
        method,
        path,
        query,
        headers,
        body,
    }))
}

enum Body {
    Empty,
    Bytes(Vec<u8>),
    File { path: PathBuf, start: u64, len: u64 },
}

struct Response {
    status: u16,
    content_type: Option<String>,
    headers: Vec<(String, String)>,
    body: Body,
}

impl Response {
    fn json(status: u16, value: Value) -> Self {
        Self::bytes(status, "application/json; charset=utf-8", value.to_string())
    }

    fn error(status: u16, message: impl Into<String>) -> Self {
        Self::json(status, json!({ "ok": false, "error": message.into() }))
    }

    fn bytes(status: u16, content_type: &str, body: impl Into<Vec<u8>>) -> Self {
        Self {
            status,
            content_type: Some(content_type.to_string()),
            headers: Vec::new(),
            body: Body::Bytes(body.into()),
        }
    }

    fn empty(status: u16) -> Self {
        Self {
            status,
            content_type: None,
            headers: Vec::new(),
            body: Body::Empty,
        }
    }

    fn file(path: PathBuf, start: u64, len: u64) -> Self {
        Self {
            status: 200,
            content_type: Some(content_type_for(&path).to_string()),
            headers: vec![("Accept-Ranges".to_string(), "bytes".to_string())],
            body: Body::File { path, start, len },
        }
    }

    fn header(mut self, name: &str, value: impl Into<String>) -> Self {
        self.headers.push((name.to_string(), value.into()));
        self
    }
}

fn content_type_for(path: &Path) -> &'static str {
    match path
        .extension()
        .and_then(|ext| ext.to_str())
        .unwrap_or("")
        .to_ascii_lowercase()
        .as_str()
    {
        "html" | "htm" => "text/html; charset=utf-8",
        "plist" => "application/xml; charset=utf-8",
        "ipa" => "application/octet-stream",
        "zip" => "application/zip",
        "json" => "application/json; charset=utf-8",
        "txt" => "text/plain; charset=utf-8",
        "ico" => "image/x-icon",
        "png" => "image/png",
        "css" => "text/css; charset=utf-8",
        "js" => "text/javascript; charset=utf-8",
        _ => "application/octet-stream",
    }
}

fn route(state: &WebState, request: &Request, source: IpAddr) -> Response {
    let segments: Vec<&str> = request
        .path
        .split('/')
        .filter(|segment| !segment.is_empty())
        .collect();

    if request.method == "OPTIONS" {
        return Response::empty(204);
    }

    if request.path.starts_with("/api/") || request.path == "/api" {
        return route_api(state, request, source);
    }

    if request.path == "/" {
        return serve_site_root(state, request);
    }

    if segments.first() == Some(&"assets") {
        return route_assets(state, request, &segments);
    }

    if segments.len() == 1
        && (segments[0] == "index.html"
            || segments[0] == "manifest.plist"
            || segments[0].ends_with(".ipa"))
        && safe_name(segments[0])
    {
        return serve_site_file(state, request, segments[0]);
    }

    Response::error(404, "没有这个接口")
}

fn route_api(state: &WebState, request: &Request, source: IpAddr) -> Response {
    let segments: Vec<&str> = request
        .path
        .split('/')
        .filter(|segment| !segment.is_empty())
        .collect();
    let registry = &state.registry;

    match (request.method.as_str(), segments.as_slice()) {
        ("GET" | "HEAD", ["api"]) => registry_root(registry),
        ("GET" | "HEAD", ["api", "health"]) => Response::json(
            200,
            json!({
                "ok": true,
                "servers": registry.len(),
                "ttl_secs": registry.ttl().as_secs_f32(),
                "ota": state.ota.is_some(),
            }),
        )
        .header("Cache-Control", "no-store"),
        ("GET" | "HEAD", ["api", "servers"]) => {
            let servers = registry.list();
            let views: Vec<Value> = servers
                .iter()
                .map(|server| {
                    json!({
                        "id": server.id,
                        "name": server.name,
                        "host": server.host,
                        "port": server.port,
                        "addr": server.addr,
                        "players": server.players,
                        "max_players": server.max_players,
                        "tick_hz": server.tick_hz,
                        "age_secs": (server.age_secs() * 10.0).round() / 10.0,
                    })
                })
                .collect();
            Response::json(
                200,
                json!({
                    "ok": true,
                    "count": views.len(),
                    "servers": views,
                }),
            )
            .header("Cache-Control", "no-store")
        }
        ("POST", ["api", "servers", "register"]) => register(registry, request, source),
        ("POST", ["api", "servers", id, "heartbeat"]) => heartbeat(registry, request, id),
        ("POST", ["api", "servers", id, "unregister"]) => {
            let removed = registry.unregister(id);
            Response::json(200, json!({ "ok": true, "removed": removed }))
        }
        (_, ["api", "servers"]) => Response::error(405, "只支持 GET"),
        (_, ["api", "servers", "register"]) => Response::error(405, "只支持 POST"),
        (_, ["api", "servers", _, "heartbeat" | "unregister"]) => {
            Response::error(405, "只支持 POST")
        }
        _ => Response::error(404, "没有这个接口"),
    }
}

fn registry_root(registry: &Registry) -> Response {
    Response::json(
        200,
        json!({
            "ok": true,
            "service": "crab-feast web（服务器注册中心 + 资源更新）",
            "endpoints": [
                "GET /api/health",
                "GET /api/servers",
                "POST /api/servers/register",
                "POST /api/servers/{id}/heartbeat",
                "POST /api/servers/{id}/unregister",
                "GET /assets/version",
                "GET /assets/state.json",
                "GET /assets/diff?from=<ver>",
                "GET /assets/full/<ver>.zip",
                "GET /assets/deltas/<from>_<to>.zip",
                "GET /manifest.plist",
                "GET /<name>.ipa",
            ],
            "servers": registry.len(),
        }),
    )
}

fn route_assets(state: &WebState, request: &Request, segments: &[&str]) -> Response {
    let Some(site) = state.ota.as_deref() else {
        return Response::json(503, json!({ "error": "OTA 未启用（启动时加了 --no-ota）" }));
    };
    let assets = site.assets();
    match (request.method.as_str(), segments) {
        ("GET" | "HEAD", ["assets", "version"]) => {
            let latest = match assets.latest_version() {
                Ok(v) => v,
                Err(message) => return Response::json(503, json!({ "error": message })),
            };
            let etag = format!("\"{latest}\"");
            if !latest.is_empty() && request.header("if-none-match") == Some(etag.as_str()) {
                return Response::empty(304)
                    .header("ETag", etag)
                    .header("Cache-Control", "no-store, max-age=0");
            }
            let body = format!("{latest}\n");
            let mut response = Response::bytes(200, "text/plain; charset=utf-8", body)
                .header("ETag", etag)
                .header("Cache-Control", "no-store, max-age=0");
            response.content_type = Some("text/plain; charset=utf-8".to_string());
            response
        }
        ("GET" | "HEAD", ["assets", "state.json"]) => {
            let client = query_param(&request.query, "from")
                .unwrap_or_default()
                .trim()
                .to_string();
            match assets.state(&client) {
                Ok(value) => {
                    Response::json(200, value).header("Cache-Control", "no-store, max-age=0")
                }
                Err(message) => Response::json(503, json!({ "error": message })),
            }
        }
        ("GET" | "HEAD", ["assets", "diff"]) => {
            let from = query_param(&request.query, "from")
                .unwrap_or_default()
                .trim()
                .to_string();
            match assets.diff(&from) {
                Ok(value) => {
                    Response::json(200, value).header("Cache-Control", "no-store, max-age=0")
                }
                Err(message) => Response::json(503, json!({ "error": message })),
            }
        }
        ("GET" | "HEAD", ["assets", "full", name]) if valid_zip_name(name) => {
            let version = name.strip_suffix(".zip").unwrap_or(name);
            serve_file(&assets.full_path(version), request)
        }
        ("GET" | "HEAD", ["assets", "deltas", name]) if valid_zip_name(name) => {
            let Some((from, to)) = parse_delta_name(name) else {
                return Response::error(404, "增量包名不合法");
            };
            serve_file(&assets.delta_path(from, to), request)
        }
        _ => Response::error(404, "没有这个资源"),
    }
}

fn serve_site_root(state: &WebState, request: &Request) -> Response {
    let Some(site) = state.ota.as_deref() else {
        return Response::json(
            200,
            json!({
                "ok": true,
                "service": "crab-feast server registry",
                "message": "OTA 未启用，仅注册 API 可用",
            }),
        );
    };
    serve_file(&site.root().join("index.html"), request)
}

fn serve_site_file(state: &WebState, request: &Request, name: &str) -> Response {
    let Some(site) = state.ota.as_deref() else {
        return Response::error(404, "OTA 未启用");
    };
    serve_file(&site.root().join(name), request)
}

fn serve_file(path: &Path, request: &Request) -> Response {
    let metadata = match std::fs::metadata(path) {
        Ok(metadata) if metadata.is_file() => metadata,
        Ok(_) => return Response::error(404, "不是文件"),
        Err(_) => return Response::error(404, "文件不存在"),
    };
    let size = metadata.len();
    let range = match request.header("range") {
        Some(header) => match parse_range(header, size) {
            RangeDecision::Ignore => None,
            RangeDecision::Unsatisfiable => {
                return Response::empty(416)
                    .header("Content-Range", format!("bytes */{size}"))
                    .header("Cache-Control", "no-store");
            }
            RangeDecision::Satisfiable(start, end) => Some((start, end)),
        },
        None => None,
    };
    let (start, len) = match range {
        Some((start, end)) => (start, end - start + 1),
        None => (0, size),
    };
    let cache = match path
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
        .to_ascii_lowercase()
        .as_str()
    {
        // 版本化 zip 的 URL 不可变，可以放心缓存
        "zip" => "public, max-age=86400",
        // index.html 里写着 ipa 地址，ipa 文件名固定但内容会变，都必须每次现取
        _ => "no-store, max-age=0",
    };
    let mut response =
        Response::file(path.to_path_buf(), start, len).header("Cache-Control", cache);
    if let Some((start, end)) = range {
        response.status = 206;
        response.headers.push((
            "Content-Range".to_string(),
            format!("bytes {start}-{end}/{size}"),
        ));
    }
    response
}

enum RangeDecision {
    Ignore,
    Unsatisfiable,
    Satisfiable(u64, u64),
}

/// 只支持单段 Range；格式不认识时按 Python 版的策略忽略，返回 200 整文件。
fn parse_range(header: &str, size: u64) -> RangeDecision {
    if size == 0 {
        return RangeDecision::Ignore;
    }
    let Some(spec) = header.trim().strip_prefix("bytes=") else {
        return RangeDecision::Ignore;
    };
    let Some((start_s, end_s)) = spec.split_once('-') else {
        return RangeDecision::Ignore;
    };
    if start_s.contains(',') || end_s.contains(',') {
        return RangeDecision::Ignore;
    }
    let (start, end) = if start_s.is_empty() {
        // bytes=-N：最后 N 字节
        let Ok(n) = end_s.parse::<u64>() else {
            return RangeDecision::Ignore;
        };
        if n == 0 {
            return RangeDecision::Unsatisfiable;
        }
        (size.saturating_sub(n), size - 1)
    } else {
        let Ok(start) = start_s.parse::<u64>() else {
            return RangeDecision::Ignore;
        };
        let end = if end_s.is_empty() {
            size - 1
        } else {
            match end_s.parse::<u64>() {
                Ok(end) => end.min(size - 1),
                Err(_) => return RangeDecision::Ignore,
            }
        };
        (start, end)
    };
    if start >= size || start > end {
        return RangeDecision::Unsatisfiable;
    }
    RangeDecision::Satisfiable(start, end)
}

fn valid_zip_name(name: &str) -> bool {
    name.ends_with(".zip")
        && safe_name(name)
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | '.'))
}

fn parse_delta_name(name: &str) -> Option<(&str, &str)> {
    let stem = name.strip_suffix(".zip")?;
    let (from, to) = stem.split_once('_')?;
    if from.is_empty() || to.is_empty() {
        return None;
    }
    Some((from, to))
}

fn safe_name(name: &str) -> bool {
    !name.is_empty()
        && name != "."
        && name != ".."
        && !name.contains("..")
        && !name.contains('/')
        && !name.contains('\\')
        && !name.chars().any(char::is_control)
}

fn query_param(query: &str, key: &str) -> Option<String> {
    query.split('&').find_map(|pair| {
        let (name, value) = pair.split_once('=')?;
        (name == key).then(|| percent_decode(value))
    })
}

fn percent_decode(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    let bytes = value.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'%' if i + 2 < bytes.len() => {
                if let Ok(byte) = u8::from_str_radix(&value[i + 1..i + 3], 16) {
                    out.push(byte as char);
                    i += 3;
                    continue;
                }
                out.push('%');
                i += 1;
            }
            b'+' => {
                out.push(' ');
                i += 1;
            }
            byte => {
                out.push(byte as char);
                i += 1;
            }
        }
    }
    out
}

fn write_response<S: Write>(
    stream: &mut S,
    response: &Response,
    head_only: bool,
) -> std::io::Result<()> {
    let status = response.status;
    let reason = match status {
        200 => "OK",
        204 => "No Content",
        206 => "Partial Content",
        304 => "Not Modified",
        400 => "Bad Request",
        404 => "Not Found",
        405 => "Method Not Allowed",
        410 => "Gone",
        413 => "Payload Too Large",
        416 => "Range Not Satisfiable",
        500 => "Internal Server Error",
        503 => "Service Unavailable",
        _ => "Unknown",
    };
    let content_length = match &response.body {
        Body::Empty => 0,
        Body::Bytes(body) => body.len() as u64,
        Body::File { len, .. } => *len,
    };
    let cache = response
        .headers
        .iter()
        .find(|(name, _)| name.eq_ignore_ascii_case("Cache-Control"))
        .map(|(_, value)| value.as_str())
        .unwrap_or("no-store");
    let mut headers = format!(
        "HTTP/1.1 {status} {reason}\r\n\
         Content-Length: {content_length}\r\n\
         Cache-Control: {cache}\r\n\
         Access-Control-Allow-Origin: *\r\n\
         Access-Control-Allow-Methods: GET, POST, HEAD, OPTIONS\r\n\
         Access-Control-Allow-Headers: Content-Type, Range, If-None-Match\r\n\
         Access-Control-Max-Age: 600\r\n\
         X-Content-Type-Options: nosniff\r\n\
         Connection: close\r\n"
    );
    if let Some(content_type) = &response.content_type {
        headers.push_str(&format!("Content-Type: {content_type}\r\n"));
    }
    for (name, value) in &response.headers {
        if name.eq_ignore_ascii_case("Cache-Control") {
            continue;
        }
        headers.push_str(&format!("{name}: {value}\r\n"));
    }
    headers.push_str("\r\n");
    stream.write_all(headers.as_bytes())?;
    if head_only || status == 304 || status == 204 {
        return stream.flush();
    }
    match &response.body {
        Body::Empty => {}
        Body::Bytes(body) => stream.write_all(body)?,
        Body::File { path, start, len } => {
            let mut file = File::open(path)?;
            file.seek(SeekFrom::Start(*start))?;
            let mut remaining = *len;
            let mut buf = vec![0u8; 256 * 1024];
            while remaining > 0 {
                let want = remaining.min(buf.len() as u64) as usize;
                let read = file.read(&mut buf[..want])?;
                if read == 0 {
                    break;
                }
                stream.write_all(&buf[..read])?;
                remaining -= read as u64;
            }
        }
    }
    stream.flush()
}

fn load_tls_config(cert: &Path, key: &Path) -> std::io::Result<Arc<ServerConfig>> {
    // rustls 0.23 需要显式安装 provider；ring 后端与 ureq 客户端用的是同一套
    let _ = rustls::crypto::ring::default_provider().install_default();
    let cert_text = std::fs::read_to_string(cert)
        .map_err(|e| std::io::Error::other(format!("读取证书 {cert:?} 失败：{e}")))?;
    let key_text = std::fs::read_to_string(key)
        .map_err(|e| std::io::Error::other(format!("读取私钥 {key:?} 失败：{e}")))?;
    let mut certs = Vec::new();
    for (label, bytes) in pem_blocks(&cert_text) {
        if label == "CERTIFICATE" {
            certs.push(CertificateDer::from(bytes));
        }
    }
    if certs.is_empty() {
        return Err(std::io::Error::other(format!(
            "{cert:?} 里没有 CERTIFICATE 块"
        )));
    }
    let mut key_der: Option<PrivateKeyDer<'static>> = None;
    for (label, bytes) in pem_blocks(&key_text) {
        key_der = match label.as_str() {
            "PRIVATE KEY" => Some(PrivateKeyDer::Pkcs8(PrivatePkcs8KeyDer::from(bytes))),
            "RSA PRIVATE KEY" => Some(PrivateKeyDer::Pkcs1(PrivatePkcs1KeyDer::from(bytes))),
            "EC PRIVATE KEY" => Some(PrivateKeyDer::Sec1(PrivateSec1KeyDer::from(bytes))),
            _ => continue,
        };
        break;
    }
    let key_der = key_der.ok_or_else(|| {
        std::io::Error::other(format!("{key:?} 里没有可用的私钥块（PKCS#8/RSA/EC）"))
    })?;
    let config = ServerConfig::builder()
        .with_no_client_auth()
        .with_single_cert(certs, key_der)
        .map_err(|e| std::io::Error::other(format!("TLS 配置失败：{e}")))?;
    Ok(Arc::new(config))
}

fn pem_blocks(text: &str) -> Vec<(String, Vec<u8>)> {
    let mut out = Vec::new();
    let mut label: Option<String> = None;
    let mut payload = String::new();
    for line in text.lines() {
        let line = line.trim();
        if let Some(rest) = line.strip_prefix("-----BEGIN ") {
            label = rest.strip_suffix("-----").map(|s| s.trim().to_string());
            payload.clear();
        } else if line.starts_with("-----END ") {
            if let Some(name) = label.take()
                && let Ok(bytes) =
                    base64::engine::general_purpose::STANDARD.decode(payload.as_str())
            {
                out.push((name, bytes));
            }
            payload.clear();
        } else if label.is_some() {
            payload.push_str(line);
        }
    }
    out
}

/// 注册请求体。`host` 可省略——注册中心会用 TCP 源 IP 兜底。
#[derive(serde::Deserialize)]
struct RegisterBody {
    #[serde(default)]
    id: Option<String>,
    #[serde(default)]
    name: String,
    #[serde(default)]
    host: Option<String>,
    port: u16,
    #[serde(default)]
    players: u8,
    #[serde(default = "default_max_players")]
    max_players: u8,
    #[serde(default = "default_tick_hz")]
    tick_hz: f32,
}

fn default_max_players() -> u8 {
    100
}

fn default_tick_hz() -> f32 {
    60.0
}

fn register(registry: &Registry, request: &Request, source: IpAddr) -> Response {
    let body: RegisterBody = match serde_json::from_slice(&request.body) {
        Ok(body) => body,
        Err(error) => return Response::error(400, format!("请求体不是合法 JSON：{error}")),
    };
    let register = RegisterRequest {
        id: body.id,
        name: body.name,
        host: body.host,
        port: body.port,
        players: body.players,
        max_players: body.max_players,
        tick_hz: body.tick_hz,
    };
    match registry.register(register, source) {
        Ok(server) => Response::json(
            200,
            json!({
                "ok": true,
                "id": server.id,
                "addr": server.addr,
                "ttl_secs": registry.ttl().as_secs_f32(),
            }),
        ),
        Err(message) => Response::error(400, message),
    }
}

#[derive(serde::Deserialize, Default)]
struct HeartbeatBody {
    #[serde(default)]
    players: u8,
    #[serde(default)]
    max_players: Option<u8>,
}

fn heartbeat(registry: &Registry, request: &Request, id: &str) -> Response {
    let body: HeartbeatBody = if request.body.is_empty() {
        HeartbeatBody::default()
    } else {
        match serde_json::from_slice(&request.body) {
            Ok(body) => body,
            Err(error) => return Response::error(400, format!("请求体不是合法 JSON：{error}")),
        }
    };
    match registry.heartbeat(id, body.players, body.max_players) {
        Ok(()) => Response::json(200, json!({ "ok": true })),
        // 410：条目不存在或已过期。服务端看到它就知道要重新注册，而不是继续白心跳
        Err(message) => Response::error(410, message),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::assets::OtaSite;
    use std::{
        path::PathBuf,
        sync::atomic::{AtomicU64, Ordering},
        time::Instant,
    };

    fn start() -> HttpServer {
        HttpServer::start(
            "127.0.0.1:0".parse().unwrap(),
            Registry::new(Duration::from_secs(30)),
        )
        .expect("测试服务应能绑定本机端口")
    }

    fn post(base: &str, path: &str, body: Value) -> (u16, Value) {
        let mut response = ureq::post(&format!("{base}{path}"))
            .config()
            .http_status_as_error(false)
            .build()
            .header("Content-Type", "application/json")
            .send(serde_json::to_string(&body).unwrap())
            .expect("HTTP 请求应能发出");
        let status = response.status().as_u16();
        let text = response.body_mut().read_to_string().expect("响应体应可读");
        let json = if text.is_empty() {
            Value::Null
        } else {
            serde_json::from_str(&text).expect("响应应是 JSON")
        };
        (status, json)
    }

    fn get(base: &str, path: &str) -> (u16, Value) {
        let mut response = ureq::get(&format!("{base}{path}"))
            .config()
            .http_status_as_error(false)
            .build()
            .call()
            .expect("HTTP 请求应能发出");
        let status = response.status().as_u16();
        let text = response.body_mut().read_to_string().expect("响应体应可读");
        (status, serde_json::from_str(&text).expect("响应应是 JSON"))
    }

    #[test]
    fn register_list_heartbeat_unregister() {
        let server = start();
        let base = format!("http://{}", server.local_addr());
        let (status, body) = post(
            &base,
            "/api/servers/register",
            json!({
                "name": "测试服",
                "host": "127.0.0.1",
                "port": 5099,
                "players": 3,
                "max_players": 100,
                "tick_hz": 60.0,
            }),
        );
        assert_eq!(status, 200);
        let id = body["id"].as_str().unwrap().to_string();
        assert_eq!(body["addr"], "127.0.0.1:5099");

        let (status, list) = get(&base, "/api/servers");
        assert_eq!(status, 200);
        assert_eq!(list["count"], 1);
        assert_eq!(list["servers"][0]["id"], id);
        assert_eq!(list["servers"][0]["addr"], "127.0.0.1:5099");

        let (status, heart) = post(
            &base,
            &format!("/api/servers/{id}/heartbeat"),
            json!({ "players": 7, "max_players": 100 }),
        );
        assert_eq!(status, 200);
        assert_eq!(heart["ok"], true);

        let (_, list) = get(&base, "/api/servers");
        assert_eq!(list["servers"][0]["players"], 7);

        let (status, removed) = post(&base, &format!("/api/servers/{id}/unregister"), json!({}));
        assert_eq!(status, 200);
        assert_eq!(removed["removed"], true);

        let (_, list) = get(&base, "/api/servers");
        assert_eq!(list["count"], 0);
    }

    #[test]
    fn bad_body_and_unknown_id_are_reported() {
        let server = start();
        let base = format!("http://{}", server.local_addr());
        let (status, error) = post(&base, "/api/servers/register", json!("not json object"));
        assert_eq!(status, 400);
        assert_eq!(error["ok"], false);

        let (status, error) = post(&base, "/api/servers/ghost/heartbeat", json!({}));
        assert_eq!(status, 410);
        assert_eq!(error["ok"], false);

        let (status, _) = get(&base, "/status");
        assert_eq!(status, 404);
    }

    static NEXT: AtomicU64 = AtomicU64::new(0);

    struct TestDir(PathBuf);

    impl TestDir {
        fn new(name: &str) -> Self {
            let stamp = Instant::now().elapsed().as_nanos();
            let path = std::env::temp_dir().join(format!(
                "crab-web-{name}-{}-{stamp}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ));
            std::fs::create_dir_all(&path).unwrap();
            Self(path)
        }
    }

    impl Drop for TestDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn write(path: &Path, text: &str) {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).unwrap();
        }
        std::fs::write(path, text).unwrap();
    }

    fn sha(text: &str) -> String {
        use sha2::{Digest, Sha256};
        let mut hasher = Sha256::new();
        hasher.update(text.as_bytes());
        format!("{:x}", hasher.finalize())
    }

    /// 造一个两版本的仓库：v2 改了 a.txt、加了 b.txt、删了 old.txt。
    fn make_store(root: &Path) -> PathBuf {
        let store = root.join("assets");
        let v1 = "111111111111";
        let v2 = "222222222222";
        write(
            &store.join("versions.json"),
            &serde_json::to_string_pretty(&json!({
                "versions": [
                    {"version": v1, "created": "t1", "bytes": 20, "files": 2},
                    {"version": v2, "created": "t2", "bytes": 21, "files": 2},
                ]
            }))
            .unwrap(),
        );
        let a1 = sha("a1");
        let old = sha("old");
        let a2 = sha("a2");
        let b2 = sha("b2");
        for (text, hash) in [("a1", &a1), ("old", &old), ("a2", &a2), ("b2", &b2)] {
            let path = store.join("blobs").join(&hash[..2]).join(hash);
            write(&path, text);
        }
        write(
            &store.join("indexes").join(format!("{v1}.json")),
            &serde_json::to_string_pretty(&json!({
                "version": v1,
                "created": "t1",
                "bytes": 20,
                "files": [
                    {"path": "a.txt", "sha256": a1, "size": 2},
                    {"path": "old.txt", "sha256": old, "size": 3},
                ]
            }))
            .unwrap(),
        );
        write(
            &store.join("indexes").join(format!("{v2}.json")),
            &serde_json::to_string_pretty(&json!({
                "version": v2,
                "created": "t2",
                "bytes": 21,
                "files": [
                    {"path": "a.txt", "sha256": a2, "size": 2},
                    {"path": "b.txt", "sha256": b2, "size": 2},
                ]
            }))
            .unwrap(),
        );
        store
    }

    fn site(root: &Path) -> Arc<OtaSite> {
        Arc::new(OtaSite::new(root.to_path_buf(), root.join("assets")))
    }

    fn start_site(root: &Path) -> (HttpServer, String) {
        let server = HttpServer::start_with(
            "127.0.0.1:0".parse().unwrap(),
            WebState::with_ota(Registry::new(Duration::from_secs(30)), site(root)),
        )
        .unwrap();
        let base = format!("http://{}", server.local_addr());
        (server, base)
    }

    #[test]
    fn asset_version_state_and_delta() {
        let dir = TestDir::new("assets");
        make_store(&dir.0);
        write(&dir.0.join("index.html"), "<html>ok</html>");
        let (_server, base) = start_site(&dir.0);

        let (status, text) = {
            let mut response = ureq::get(&format!("{base}/assets/version"))
                .config()
                .http_status_as_error(false)
                .build()
                .call()
                .unwrap();
            (
                response.status().as_u16(),
                response.body_mut().read_to_string().unwrap(),
            )
        };
        assert_eq!(status, 200);
        assert_eq!(text.trim(), "222222222222");

        let (status, _) = {
            let response = ureq::get(&format!("{base}/assets/version"))
                .header("If-None-Match", "\"222222222222\"")
                .config()
                .http_status_as_error(false)
                .build()
                .call()
                .unwrap();
            (response.status().as_u16(), ())
        };
        assert_eq!(status, 304);

        let (status, state) = get(&base, "/assets/state.json");
        assert_eq!(status, 200);
        assert_eq!(state["version"], "222222222222");
        assert_eq!(state["files"], 2);

        // 未知版本 → 整包
        let (status, diff) = get(&base, "/assets/diff?from=");
        assert_eq!(status, 200);
        assert_eq!(diff["mode"], "full");
        assert_eq!(diff["url"], "/assets/full/222222222222.zip");
        let full = dir.0.join("assets/full/222222222222.zip");
        assert!(full.is_file());
        assert_eq!(
            diff["bytes"].as_u64().unwrap(),
            std::fs::metadata(&full).unwrap().len()
        );
        let mut response = ureq::get(&format!("{base}/assets/full/222222222222.zip"))
            .config()
            .http_status_as_error(false)
            .build()
            .call()
            .unwrap();
        assert_eq!(response.status().as_u16(), 200);
        let body = response.body_mut().read_to_vec().unwrap();
        assert_eq!(body.len() as u64, std::fs::metadata(&full).unwrap().len());

        // v1 → v2 → 只含改动的 a.txt 与新增的 b.txt，old.txt 在删除清单里
        let (status, diff) = get(&base, "/assets/diff?from=111111111111");
        assert_eq!(status, 200);
        assert_eq!(diff["mode"], "delta");
        assert_eq!(diff["files"].as_array().unwrap().len(), 2);
        assert_eq!(diff["deleted"].as_array().unwrap().len(), 1);
        let delta = dir.0.join("assets/deltas/111111111111_222222222222.zip");
        assert!(delta.is_file());
        let mut response = ureq::get(&format!(
            "{base}/assets/deltas/111111111111_222222222222.zip"
        ))
        .config()
        .http_status_as_error(false)
        .build()
        .call()
        .unwrap();
        assert_eq!(response.status().as_u16(), 200);
        let body = response.body_mut().read_to_vec().unwrap();
        assert_eq!(body.len() as u64, std::fs::metadata(&delta).unwrap().len());

        // 同一个版本 → none
        let (_, diff) = get(&base, "/assets/diff?from=222222222222");
        assert_eq!(diff["mode"], "none");
    }

    #[test]
    fn static_files_support_head_and_range_and_hide_secrets() {
        let dir = TestDir::new("static");
        make_store(&dir.0);
        write(&dir.0.join("index.html"), "0123456789abcdef");
        write(&dir.0.join("manifest.plist"), "<plist/>");
        write(&dir.0.join("secrets/cert.pem"), "SECRET");
        let (_server, base) = start_site(&dir.0);

        let (status, body) = {
            let mut response = ureq::get(&format!("{base}/"))
                .config()
                .http_status_as_error(false)
                .build()
                .call()
                .unwrap();
            (
                response.status().as_u16(),
                response.body_mut().read_to_string().unwrap(),
            )
        };
        assert_eq!(status, 200);
        assert_eq!(body, "0123456789abcdef");

        let mut response = ureq::get(&format!("{base}/"))
            .header("Range", "bytes=0-3")
            .config()
            .http_status_as_error(false)
            .build()
            .call()
            .unwrap();
        assert_eq!(response.status().as_u16(), 206);
        assert_eq!(
            response.headers().get("content-range").unwrap(),
            "bytes 0-3/16"
        );
        assert_eq!(response.body_mut().read_to_vec().unwrap(), b"0123");

        let response = ureq::head(&format!("{base}/manifest.plist"))
            .config()
            .http_status_as_error(false)
            .build()
            .call()
            .unwrap();
        assert_eq!(response.status().as_u16(), 200);
        assert_eq!(response.headers().get("content-length").unwrap(), "8");

        let response = ureq::get(&format!("{base}/secrets/cert.pem"))
            .config()
            .http_status_as_error(false)
            .build()
            .call()
            .unwrap();
        assert_eq!(response.status().as_u16(), 404);

        // 大于 socket 发送缓冲区：验证 accept 出来的连接是阻塞写，不会写一半断开
        let big = "x".repeat(8 * 1024 * 1024);
        write(&dir.0.join("big.ipa"), &big);
        let mut response = ureq::get(&format!("{base}/big.ipa"))
            .config()
            .http_status_as_error(false)
            .build()
            .call()
            .unwrap();
        assert_eq!(response.status().as_u16(), 200);
        assert_eq!(
            response.body_mut().read_to_vec().unwrap().len(),
            8 * 1024 * 1024
        );
    }
}
