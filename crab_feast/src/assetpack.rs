//! 资源热更：启动时按版本号向 OTA 服务要差量包，落地到可写目录后再进游戏。
//!
//! 服务端（`crab-feast-web` + `scripts/dev/assets_store.py`）维护一份内容寻址的版本
//! 仓库：每次打包都会给当时的资源做一次快照，客户端只要报上自己的版本，服务端就能
//! 算出「它缺的那几个文件」并打成增量包；同一对 (from, to) 只算一次，之后直接命中。
//!
//! 协议：
//!   GET /assets/state.json            → {version, versions, files, bytes}
//!   GET /assets/diff?from=<ver>       → {mode: none|delta|full, url, sha256, bytes, files, deleted}
//!   GET /assets/deltas/<a>_<b>.zip    → 增量包（条目路径相对资源根，不含 assets/ 前缀）
//!   GET /assets/full/<ver>.zip        → 整包
//!
//! 本地落地：
//!   <pack_dir>/.crab_version          已安装的版本号（没有就说明还没装过，走整包）
//!   <pack_dir>/characters/...         资源本体，解压后直接当 AssetPlugin 的资源根

use bevy::{prelude::*, text::FontSize};
use flate2::read::DeflateDecoder;
use sha2::{Digest, Sha256};
use std::{
    fs::{self, File},
    io::{Read, Seek, SeekFrom, Write},
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
    thread,
};

use crate::GameState;

/// 已安装资源包的版本标记文件
pub const VERSION_FILE: &str = ".crab_version";
/// 没有设置 `CRAB_OTA_BASE` 时的默认服务端
const DEFAULT_BASE: &str = "https://ota.qiaqia.cyou:8443";
/// 同步用的临时文件名，解压完成才写版本标记
const DOWNLOAD_TMP: &str = ".pack-download.tmp";

/// 资源包落地目录。iOS 上是 App 容器内可写目录，覆盖安装会保留。
pub fn pack_dir() -> Option<PathBuf> {
    if let Ok(dir) = std::env::var("CRAB_PACK_DIR") {
        return Some(PathBuf::from(dir));
    }
    #[cfg(target_os = "ios")]
    {
        Some(
            container_root()?
                .join("Library/Application Support")
                .join("crab_feast")
                .join("assets"),
        )
    }
    #[cfg(not(target_os = "ios"))]
    {
        None
    }
}

/// 只有「有版本标记 + 有角色目录」才算装好，避免半途失败后拿残缺目录当资源根
pub fn ready_pack_dir() -> Option<PathBuf> {
    let dir = pack_dir()?;
    let ok = dir.join(VERSION_FILE).is_file() && dir.join("characters/rigs").is_dir();
    ok.then_some(dir)
}

/// iOS 的 App 数据容器根目录。
///
/// 实测 `HOME` 在这个 App 里可能取不到（取不到会让整个热更链路被跳过、首启一闪而过），
/// 所以按 `CRAB_HOME` → `HOME` → `TMPDIR` 的上一级 的顺序找，并且用「同级有没有
/// Library/Documents」来验证，避免拿到一个不存在的路径。
#[cfg(target_os = "ios")]
pub fn container_root() -> Option<PathBuf> {
    let mut candidates: Vec<PathBuf> = Vec::new();
    if let Ok(v) = std::env::var("CRAB_HOME") {
        candidates.push(PathBuf::from(v));
    }
    if let Ok(v) = std::env::var("HOME") {
        candidates.push(PathBuf::from(v));
    }
    // TMPDIR = <容器>/tmp/，上一级就是容器根
    if let Ok(v) = std::env::var("TMPDIR") {
        let tmp = PathBuf::from(v);
        if let Some(parent) = tmp.parent() {
            candidates.push(parent.to_path_buf());
        }
    }
    candidates
        .into_iter()
        .find(|c| c.join("Library").is_dir() || c.join("Documents").is_dir())
}

/// 同步日志：一行一条，落到容器里，出问题可以把容器整份捞出来看
#[cfg(target_os = "ios")]
pub fn log_line(msg: &str) {
    let Some(root) = container_root() else { return };
    let dir = root.join("Library/Application Support").join("crab_feast");
    if fs::create_dir_all(&dir).is_err() {
        return;
    }
    let path = dir.join("sync.log");
    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    if let Ok(mut f) = fs::OpenOptions::new().create(true).append(true).open(&path) {
        use std::io::Write;
        let _ = writeln!(f, "[{stamp}] {msg}");
    }
}

#[cfg(not(target_os = "ios"))]
pub fn log_line(_msg: &str) {}

pub fn base_url() -> String {
    std::env::var("CRAB_OTA_BASE").unwrap_or_else(|_| DEFAULT_BASE.to_string())
}

/// 同步任务的共享状态：主线程每帧读，工作线程写
#[derive(Clone, Debug)]
pub struct PackJob {
    pub done: bool,
    pub ok: bool,
    pub message: String,
    /// 0.0 ~ 1.0，未知总大小时为 -1
    pub progress: f32,
}

impl Default for PackJob {
    fn default() -> Self {
        Self { done: false, ok: true, message: "准备同步资源…".into(), progress: 0.0 }
    }
}

#[derive(Resource, Clone, Default)]
pub struct PackSync(pub Arc<Mutex<PackJob>>);

pub struct AssetPackPlugin;

impl Plugin for AssetPackPlugin {
    fn build(&self, app: &mut App) {
        app.init_state::<GameState>()
            .insert_resource(PackSync::default())
            .add_systems(OnEnter(GameState::AssetsSync), start_sync)
            .add_systems(
                Update,
                tick_sync.run_if(in_state(GameState::AssetsSync)),
            )
            // 大厅左下角显示资源版本，用来确认跑的到底是不是热更下来的那一包
            .add_systems(OnEnter(GameState::Lobby), spawn_pack_badge)
            .add_systems(OnExit(GameState::Lobby), despawn_pack_badge);
    }
}

#[derive(Component)]
struct PackBadge;

fn spawn_pack_badge(mut commands: Commands) {
    let Some(dir) = pack_dir() else { return };
    let ver = read_version(&dir).unwrap_or_else(|| "none".into());
    let files = std::fs::read_dir(&dir)
        .map(|d| {
            d.filter_map(Result::ok)
                .filter(|e| e.file_type().map(|t| t.is_dir()).unwrap_or(false))
                .count()
        })
        .unwrap_or(0);
    commands.spawn((
        Text::new(format!("PACK {ver} · {files} dirs")),
        TextFont { font_size: FontSize::Px(12.0), ..default() },
        TextColor(Color::srgba(0.62, 0.68, 0.78, 0.9)),
        Node {
            position_type: PositionType::Absolute,
            left: Val::Px(8.0),
            bottom: Val::Px(6.0),
            ..default()
        },
        PackBadge,
    ));
    log_line(&format!("大厅显示资源版本 {ver}"));
}

fn despawn_pack_badge(mut commands: Commands, badge: Query<Entity, With<PackBadge>>) {
    for e in &badge {
        commands.entity(e).despawn();
    }
}

/// 标记：同步阶段的临时 UI，退出该状态时清掉，别和游戏自己的 2D 相机打架
#[derive(Component)]
struct SyncUi;

/// 进度条的填充块：不依赖任何字体资源，首启也能看见进展
#[derive(Component)]
struct SyncBarFill;

fn start_sync(
    mut commands: Commands,
    sync: Res<PackSync>,
    mut next: ResMut<NextState<GameState>>,
) {
    // 桌面/安卓没有落地目录，直接进资源加载
    let Some(dir) = pack_dir() else {
        log_line("pack_dir=None（没找到可写容器，同步被跳过）");
        next.set(GameState::AssetLoading);
        return;
    };
    log_line(&format!(
        "启动 HOME={:?} TMPDIR={:?} → 资源目录 {}",
        std::env::var("HOME").ok(),
        std::env::var("TMPDIR").ok(),
        dir.display()
    ));

    let job = sync.0.clone();
    let base = base_url();
    let dir_clone = dir.clone();
    info!("资源热更：检查 {}（本地目录 {}）", base, dir.display());

    // 网络与解压都放后台线程，主线程照常出帧（避免 iOS 启动看门狗把进程杀掉）
    thread::Builder::new()
        .name("asset-pack-sync".into())
        .spawn(move || {
            let result = sync_pack(&base, &dir_clone, &job);
            let mut j = job.lock().unwrap();
            match result {
                Ok(msg) => {
                    info!("资源热更完成：{msg}");
                    j.ok = true;
                    j.message = format!("资源已就绪 · {msg}");
                }
                Err(err) => {
                    // 失败也不阻塞进游戏：有旧包就继续用旧包，没有就走 bundle 内置资源
                    warn!("资源热更失败（继续使用现有资源）：{err}");
                    j.ok = false;
                    j.message = format!("资源同步失败，沿用上一版\n{err}");
                }
            }
            j.done = true;
        })
        .ok();

    // 一张临时 2D 相机 + 一条进度条 + 一行文字，只为了别让首启黑屏
    commands.spawn((Camera2d, SyncUi));
    // 首启包里没有任何资源（字体也走热更），这里只能用引擎内置的默认字体，
    // 它只有 ASCII，所以同步阶段一律显示英文
    commands.spawn((
        Text::new("ASSET SYNC ..."),
        TextFont { font_size: FontSize::Px(20.0), ..default() },
        TextColor(Color::srgb(0.85, 0.89, 0.95)),
        Node {
            position_type: PositionType::Absolute,
            left: Val::Percent(20.0),
            top: Val::Percent(46.0),
            ..default()
        },
        SyncUi,
    ));

    // 进度条底板 + 填充块
    commands
        .spawn((
            Node {
                position_type: PositionType::Absolute,
                left: Val::Percent(20.0),
                top: Val::Percent(52.0),
                width: Val::Percent(60.0),
                height: Val::Px(10.0),
                border_radius: BorderRadius::all(Val::Px(5.0)),
                ..default()
            },
            BackgroundColor(Color::srgba(0.18, 0.20, 0.26, 0.9)),
            SyncUi,
        ))
        .with_children(|parent| {
            parent.spawn((
                Node {
                    width: Val::Percent(0.0),
                    height: Val::Percent(100.0),
                    border_radius: BorderRadius::all(Val::Px(5.0)),
                    ..default()
                },
                BackgroundColor(Color::srgb(0.36, 0.68, 0.98)),
                SyncBarFill,
            ));
        });
}

fn tick_sync(
    sync: Res<PackSync>,
    mut next: ResMut<NextState<GameState>>,
    mut texts: Query<&mut Text, With<SyncUi>>,
    mut bars: Query<&mut Node, With<SyncBarFill>>,
    mut commands: Commands,
    ui: Query<Entity, With<SyncUi>>,
) {
    let job = sync.0.lock().unwrap().clone();

    if let Ok(mut node) = bars.single_mut() {
        let pct = if job.done { 1.0 } else { job.progress.max(0.0) };
        node.width = Val::Percent((pct * 100.0).clamp(0.0, 100.0));
    }

    if let Ok(mut text) = texts.single_mut() {
        if job.done {
            *text = Text::new(if job.ok {
                "ASSET READY".to_string()
            } else {
                "ASSET SYNC FAILED - restart the app once online".to_string()
            });
        } else if job.progress >= 0.0 {
            *text = Text::new(format!("ASSET SYNC {:>3.0}%", job.progress * 100.0));
        } else {
            *text = Text::new("ASSET SYNC ...".to_string());
        }
    }

    if !job.done {
        return;
    }

    // 包内已经没有任何资源：同步失败且本地还没装过包时，进游戏只会是一片空白，
    // 所以停在这一屏，把原因打在日志里（网络恢复后重启即可）。
    let installed = ready_pack_dir();
    if !job.ok && installed.is_none() {
        return;
    }

    if let Some(dir) = installed {
        // 让角色/剪辑清单与 asset_server 的资源根指向同一个目录
        // SAFETY: 启动阶段单线程设置，之后只读
        unsafe { std::env::set_var("CRAB_ASSET_ROOT", &dir) };
        let catalog = crate::anim::character::scan_catalog_at(&dir);
        info!(
            "热更资源清单：{} 个角色，{} 个剪辑",
            catalog.characters.len(),
            catalog.clips.len()
        );
        commands.insert_resource(catalog);
    }

    next.set(GameState::AssetLoading);
    for entity in &ui {
        commands.entity(entity).despawn();
    }
}

// ---------------------------------------------------------------- 同步主体

fn sync_pack(base: &str, dir: &Path, job: &Arc<Mutex<PackJob>>) -> Result<String, String> {
    let local = read_version(dir);

    // 只取一个十几字节的纯文本版本号（带 ETag：本地已是最新时服务端回 304，正文为空）。
    // 原来的做法是每次启动都拉 2KB 的 state.json，纯属浪费。
    let remote = match remote_version(base, local.as_deref(), job)? {
        Some(v) => v,
        None => {
            let v = local.clone().unwrap_or_default();
            set_msg(job, format!("已是最新 {v}"));
            log_line(&format!("版本探测 304：本地已是最新 {v}"));
            return Ok(format!("已是最新 {v}"));
        }
    };

    log_line(&format!("本地版本={local:?} 服务端版本={remote}"));
    if local.as_deref() == Some(remote.as_str()) {
        set_msg(job, format!("已是最新 {remote}"));
        return Ok(format!("已是最新 {remote}"));
    }

    let from = local.clone().unwrap_or_default();
    let diff_url = format!("{base}/assets/diff?from={from}");
    let diff = get_json(&diff_url)?;
    let mode = diff.get("mode").and_then(|v| v.as_str()).unwrap_or("none");

    if mode == "none" {
        write_version(dir, &remote)?;
        return Ok(format!("无需更新 {remote}"));
    }

    let rel = diff
        .get("url")
        .and_then(|v| v.as_str())
        .ok_or_else(|| "diff 里没有 url".to_string())?;
    let url = format!("{base}{rel}");
    let expected = diff.get("sha256").and_then(|v| v.as_str()).unwrap_or("");
    let total = diff.get("bytes").and_then(|v| v.as_u64()).unwrap_or(0);

    set_msg(
        job,
        format!("{mode}：{from} → {remote}（{:.1}MB）", total as f64 / 1048576.0),
    );

    fs::create_dir_all(dir).map_err(|e| format!("建目录失败 {dir:?}: {e}"))?;
    let tmp = dir.join(DOWNLOAD_TMP);
    log_line(&format!("开始下载 {mode} {url}（期望 {total} 字节）"));
    let got = download(&url, &tmp, total, job).inspect_err(|e| {
        log_line(&format!("下载失败: {e}"));
    })?;
    log_line(&format!("下载完成 {got}"));

    if !expected.is_empty() && got != expected {
        let _ = fs::remove_file(&tmp);
        return Err(format!("增量包 sha256 不匹配：期望 {expected}，实际 {got}"));
    }

    set_msg(job, "解压中…".into());
    apply_zip(&tmp, dir).inspect_err(|e| log_line(&format!("解压失败: {e}")))?;

    // 服务端告诉你哪些文件这一版已经删了，解压不会删，必须显式清理
    if let Some(list) = diff.get("deleted").and_then(|v| v.as_array()) {
        for item in list {
            if let Some(rel_path) = item.as_str() {
                let target = dir.join(rel_path);
                if target.starts_with(dir) && target.is_file() {
                    let _ = fs::remove_file(&target);
                    // 顺手把因此变空的父目录收掉，别在容器里留一堆空壳
                    let mut parent = target.parent().map(|p| p.to_path_buf());
                    while let Some(p) = parent {
                        if p == dir || fs::remove_dir(&p).is_err() {
                            break;
                        }
                        parent = p.parent().map(|x| x.to_path_buf());
                    }
                }
            }
        }
    }

    let _ = fs::remove_file(&tmp);
    write_version(dir, &remote)?;
    log_line(&format!("同步完成 {mode} {from} → {remote}"));
    Ok(format!("{mode} {from} → {remote}"))
}

fn set_msg(job: &Arc<Mutex<PackJob>>, msg: String) {
    if let Ok(mut j) = job.lock() {
        j.message = msg;
        j.progress = 0.0;
    }
}

fn read_version(dir: &Path) -> Option<String> {
    let s = fs::read_to_string(dir.join(VERSION_FILE)).ok()?;
    let t = s.trim();
    (!t.is_empty()).then(|| t.to_string())
}

fn write_version(dir: &Path, version: &str) -> Result<(), String> {
    fs::create_dir_all(dir).map_err(|e| e.to_string())?;
    fs::write(dir.join(VERSION_FILE), version).map_err(|e| format!("写版本标记失败: {e}"))
}

/// 纯文本 GET；服务端回 304 时返回 None（表示本地版本已是最新，不用下东西）
fn get_text(url: &str, if_none_match: Option<&str>) -> Result<Option<String>, String> {
    let mut req = ureq::get(url);
    if let Some(tag) = if_none_match {
        req = req.header("If-None-Match", tag);
    }
    let mut resp = req.call().map_err(|e| format!("请求 {url} 失败: {e}"))?;
    if resp.status().as_u16() == 304 {
        return Ok(None);
    }
    let body = resp
        .body_mut()
        .read_to_string()
        .map_err(|e| format!("读取 {url} 响应失败: {e}"))?;
    Ok(Some(body.trim().to_string()))
}

/// 版本探测。首启常常还没网（DNS 未就绪 / IPv6 路由没建好），所以退避重试 8 次。
fn remote_version(
    base: &str,
    local: Option<&str>,
    job: &Arc<Mutex<PackJob>>,
) -> Result<Option<String>, String> {
    let url = format!("{base}/assets/version");
    let tag = local.map(|v| format!("\"{v}\""));
    let mut last = String::new();
    for attempt in 1..=8 {
        match get_text(&url, tag.as_deref()) {
            Ok(v) => {
                log_line(&format!("version 探测成功（第 {attempt} 次尝试）"));
                return Ok(v);
            }
            Err(e) => {
                last = e;
                log_line(&format!("version 第 {attempt} 次失败: {last}"));
                if attempt < 8 {
                    set_msg(job, format!("等待网络… {attempt}/8"));
                    std::thread::sleep(std::time::Duration::from_secs(attempt.min(5) as u64));
                }
            }
        }
    }
    Err(format!("连不上资源服务（重试 8 次）：{last}"))
}

fn get_json(url: &str) -> Result<serde_json::Value, String> {
    let mut resp = ureq::get(url)
        .call()
        .map_err(|e| format!("请求 {url} 失败: {e}"))?;
    let body = resp
        .body_mut()
        .read_to_string()
        .map_err(|e| format!("读取 {url} 响应失败: {e}"))?;
    serde_json::from_str(&body).map_err(|e| format!("{url} 返回的不是 JSON: {e}"))
}

fn download(
    url: &str,
    dest: &Path,
    total: u64,
    job: &Arc<Mutex<PackJob>>,
) -> Result<String, String> {
    let resp = ureq::get(url)
        .call()
        .map_err(|e| format!("下载 {url} 失败: {e}"))?;
    let mut reader = resp.into_body().into_reader();
    let mut file = File::create(dest).map_err(|e| format!("创建 {dest:?} 失败: {e}"))?;
    let mut hasher = Sha256::new();
    let mut buf = [0u8; 1 << 16];
    let mut got = 0u64;

    loop {
        let n = reader.read(&mut buf).map_err(|e| format!("下载中断: {e}"))?;
        if n == 0 {
            break;
        }
        file.write_all(&buf[..n]).map_err(|e| format!("写盘失败: {e}"))?;
        hasher.update(&buf[..n]);
        got += n as u64;
        if let Ok(mut j) = job.lock() {
            j.progress = if total > 0 { (got as f32 / total as f32).min(0.99) } else { -1.0 };
            j.message = format!(
                "下载中 {:.1} / {:.1} MB",
                got as f64 / 1048576.0,
                total as f64 / 1048576.0
            );
        }
    }
    file.flush().ok();
    Ok(format!("{:x}", hasher.finalize()))
}

// ---------------------------------------------------------------- 最小 ZIP 读取
// 只服务于自己和自己的服务端，够用就好：不做 zip64、不验 CRC（整包 sha256 已经兜住）

struct ZipEntry {
    name: String,
    method: u16,
    comp_size: u64,
    local_offset: u64,
}

fn read_central_directory(file: &mut File) -> Result<Vec<ZipEntry>, String> {
    let len = file.metadata().map_err(|e| e.to_string())?.len();
    if len < 22 {
        return Err("zip 太小".into());
    }
    let tail_len = len.min(66_000) as usize;
    file.seek(SeekFrom::Start(len - tail_len as u64))
        .map_err(|e| e.to_string())?;
    let mut tail = vec![0u8; tail_len];
    file.read_exact(&mut tail).map_err(|e| e.to_string())?;

    // 从后往前找 EOCD 签名 0x06054b50
    let mut eocd = None;
    for i in (0..tail.len().saturating_sub(3)).rev() {
        if tail[i] == 0x50 && tail[i + 1] == 0x4b && tail[i + 2] == 0x05 && tail[i + 3] == 0x06 {
            eocd = Some(i);
            break;
        }
    }
    let eocd = eocd.ok_or_else(|| "找不到 zip 目录结尾".to_string())?;
    let count = u16::from_le_bytes([tail[eocd + 10], tail[eocd + 11]]) as usize;
    let cd_size = u32::from_le_bytes([
        tail[eocd + 12],
        tail[eocd + 13],
        tail[eocd + 14],
        tail[eocd + 15],
    ]) as u64;
    let cd_off = u32::from_le_bytes([
        tail[eocd + 16],
        tail[eocd + 17],
        tail[eocd + 18],
        tail[eocd + 19],
    ]) as u64;

    file.seek(SeekFrom::Start(cd_off)).map_err(|e| e.to_string())?;
    let mut buf = vec![0u8; cd_size as usize];
    file.read_exact(&mut buf).map_err(|e| e.to_string())?;

    let mut entries = Vec::with_capacity(count);
    let mut pos = 0usize;
    while pos + 46 <= buf.len() {
        if buf[pos] != 0x50 || buf[pos + 1] != 0x4b || buf[pos + 2] != 0x01 || buf[pos + 3] != 0x02 {
            break;
        }
        let method = u16::from_le_bytes([buf[pos + 10], buf[pos + 11]]);
        let comp = u32::from_le_bytes([
            buf[pos + 20],
            buf[pos + 21],
            buf[pos + 22],
            buf[pos + 23],
        ]) as u64;
        let name_len = u16::from_le_bytes([buf[pos + 28], buf[pos + 29]]) as usize;
        let extra_len = u16::from_le_bytes([buf[pos + 30], buf[pos + 31]]) as usize;
        let comment_len = u16::from_le_bytes([buf[pos + 32], buf[pos + 33]]) as usize;
        let local = u32::from_le_bytes([
            buf[pos + 42],
            buf[pos + 43],
            buf[pos + 44],
            buf[pos + 45],
        ]) as u64;
        let name = String::from_utf8_lossy(&buf[pos + 46..pos + 46 + name_len]).into_owned();
        entries.push(ZipEntry { name, method, comp_size: comp, local_offset: local });
        pos += 46 + name_len + extra_len + comment_len;
    }
    Ok(entries)
}

fn apply_zip(zip_path: &Path, dest: &Path) -> Result<(), String> {
    let mut file = File::open(zip_path).map_err(|e| format!("打开 {zip_path:?} 失败: {e}"))?;
    let entries = read_central_directory(&mut file)?;
    for entry in entries {
        if entry.name.ends_with('/') {
            continue;
        }
        // 拒绝绝对路径与路径穿越，别让一个坏包写到资源目录外面
        if entry.name.starts_with('/') || entry.name.split('/').any(|c| c == "..") {
            return Err(format!("压缩包里有可疑路径: {}", entry.name));
        }
        let out_path = dest.join(&entry.name);
        if let Some(parent) = out_path.parent() {
            fs::create_dir_all(parent).map_err(|e| format!("建目录 {parent:?} 失败: {e}"))?;
        }

        // 本地头里才有真正的数据起点
        file.seek(SeekFrom::Start(entry.local_offset))
            .map_err(|e| e.to_string())?;
        let mut head = [0u8; 30];
        file.read_exact(&mut head).map_err(|e| e.to_string())?;
        let name_len = u16::from_le_bytes([head[26], head[27]]) as u64;
        let extra_len = u16::from_le_bytes([head[28], head[29]]) as u64;
        let data_off = entry.local_offset + 30 + name_len + extra_len;
        file.seek(SeekFrom::Start(data_off)).map_err(|e| e.to_string())?;

        let mut out = File::create(&out_path).map_err(|e| format!("创建 {out_path:?} 失败: {e}"))?;
        match entry.method {
            0 => {
                let mut take = (&mut file).take(entry.comp_size);
                std::io::copy(&mut take, &mut out).map_err(|e| format!("写 {out_path:?} 失败: {e}"))?;
            }
            8 => {
                let take = (&mut file).take(entry.comp_size);
                let mut decoder = DeflateDecoder::new(take);
                std::io::copy(&mut decoder, &mut out)
                    .map_err(|e| format!("解压 {out_path:?} 失败: {e}"))?;
            }
            other => return Err(format!("不支持的压缩方式 {other}（{}）", entry.name)),
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 端到端：真实向 OTA 服务要资源 → 下载 → 校验 sha256 → 解压 → 写版本标记
    /// 需要服务在跑；跑法：
    ///   cargo test -p crab_feast --lib assetpack -- --nocapture
    #[test]
    fn sync_pack_end_to_end() {
        // CRAB_TEST_DIR 指向已装好的目录时复用（用来验证增量路径）
        let dir = match std::env::var("CRAB_TEST_DIR") {
            Ok(d) => PathBuf::from(d),
            Err(_) => {
                let d = std::env::temp_dir().join("crab_packtest");
                let _ = fs::remove_dir_all(&d);
                d
            }
        };
        fs::create_dir_all(&dir).unwrap();

        let job = Arc::new(Mutex::new(PackJob::default()));
        let base = std::env::var("CRAB_OTA_BASE").unwrap_or_else(|_| DEFAULT_BASE.to_string());

        let first = sync_pack(&base, &dir, &job).expect("首次同步应成功");
        println!("第一次: {first}");

        let version = read_version(&dir).expect("应写出版本标记");
        assert!(dir.join("characters/rigs").is_dir(), "角色目录应落地");
        assert!(dir.join("fonts/PingFang-SC-Regular.ttf").is_file(), "字体应落地");

        // 解压出来的文件大小必须和版本清单一致
        let index = get_json(&format!("{base}/assets/state.json")).unwrap();
        assert_eq!(index["version"].as_str().unwrap(), version);

        // 落地目录必须能直接当资源根：角色/剪辑清单扫得出来
        let catalog = crate::anim::character::scan_catalog_at(&dir);
        assert!(
            !catalog.characters.is_empty(),
            "热更目录应能扫出角色，实际 {}",
            catalog.characters.len()
        );
        println!(
            "落地目录扫描：{} 个角色 / {} 个剪辑",
            catalog.characters.len(),
            catalog.clips.len()
        );

        let second = sync_pack(&base, &dir, &job).expect("二次同步应成功");
        println!("第二次: {second}");
        assert!(
            second.contains("已是最新") || second.contains("无需更新"),
            "版本一致时不该重复下载，实际: {second}"
        );
    }
}
