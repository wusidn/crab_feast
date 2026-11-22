//! 资源版本仓库的运行时部分：版本探测、增量清单、整包/增量包生成。
//!
//! 打包（snapshot）仍由 `scripts/dev/assets_store.py` 的 CLI 完成，目录布局和
//! JSON 字段保持完全兼容：
//!
//! ```text
//! <store>/versions.json            版本时间线
//! <store>/indexes/<ver>.json       版本的文件清单
//! <store>/blobs/<sha前2>/<sha>     内容寻址的文件本体
//! <store>/full/<ver>.zip           整包
//! <store>/deltas/<from>_<to>.zip   增量包
//! <store>/deltas/<from>_<to>.json  增量元信息（含删除清单）
//! ```
//!
//! 客户端协议见 `crab_feast/src/assetpack.rs`：
//! `GET /assets/version`、`/assets/state.json`、`/assets/diff?from=`。

use std::{
    fs::{self, File},
    io::{self, Read, Write},
    path::{Path, PathBuf},
    sync::Mutex,
    time::{SystemTime, UNIX_EPOCH},
};

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use zip::{CompressionMethod, ZipWriter, write::SimpleFileOptions};

/// `versions.json` 里的一条版本摘要。
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct VersionSummary {
    pub version: String,
    #[serde(default)]
    pub created: String,
    #[serde(default)]
    pub bytes: u64,
    #[serde(default)]
    pub files: usize,
}

#[derive(Debug, Deserialize)]
struct VersionsFile {
    #[serde(default)]
    versions: Vec<VersionSummary>,
}

/// 版本索引里的一个文件。
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct FileEntry {
    pub path: String,
    pub sha256: String,
    #[serde(default)]
    pub size: u64,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
struct Index {
    version: String,
    #[serde(default)]
    created: String,
    #[serde(default)]
    bytes: u64,
    #[serde(default)]
    files: Vec<FileEntry>,
}

/// 增量包元信息。字段与 `assets_store.py` 的 `deltas/<a>_<b>.json` 对齐。
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct DeltaMeta {
    pub from: String,
    pub to: String,
    pub url: String,
    pub bytes: u64,
    pub sha256: String,
    #[serde(default)]
    pub files: Vec<FileEntry>,
    #[serde(default)]
    pub deleted: Vec<String>,
    #[serde(default)]
    pub created: String,
    #[serde(default)]
    pub cached: bool,
}

/// 整包元信息（缓存文件 `full/<ver>.json`）。
#[derive(Debug, Clone, Deserialize, Serialize)]
struct FullMeta {
    sha256: String,
    bytes: u64,
}

/// OTA 站点：静态安装文件根目录 + 资源仓库。
pub struct OtaSite {
    root: PathBuf,
    assets: AssetStore,
}

impl OtaSite {
    /// `root` 是 manifest.plist / ipa / index.html 所在目录，仓库默认在 `root/assets`。
    pub fn new(root: PathBuf, store: PathBuf) -> Self {
        Self {
            root,
            assets: AssetStore::new(store),
        }
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    pub fn assets(&self) -> &AssetStore {
        &self.assets
    }
}

/// 资源仓库。`build_lock` 保证同一对版本的 zip 不会被两个请求同时写。
pub struct AssetStore {
    store: PathBuf,
    build_lock: Mutex<()>,
}

impl AssetStore {
    pub fn new(store: PathBuf) -> Self {
        Self {
            store,
            build_lock: Mutex::new(()),
        }
    }

    pub fn store(&self) -> &Path {
        &self.store
    }

    /// 最新版本号；仓库为空时返回空串（和 Python 的 `/assets/version` 一致）。
    pub fn latest_version(&self) -> Result<String, String> {
        Ok(self
            .load_versions()?
            .last()
            .map(|v| v.version.clone())
            .unwrap_or_default())
    }

    /// `GET /assets/state.json` 的 JSON。
    pub fn state(&self, client: &str) -> Result<Value, String> {
        let versions = self.load_versions()?;
        let latest = versions
            .last()
            .cloned()
            .ok_or_else(|| "仓库为空，先跑 scripts/dev/ota_pack.sh".to_string())?;
        let index = self.load_index(&latest.version)?;
        let mut recent: Vec<_> = versions.iter().rev().take(10).cloned().collect();
        recent.reverse();
        Ok(json!({
            "version": latest.version,
            "client": client,
            "versions": recent,
            "files": index.files.len(),
            "bytes": latest.bytes,
        }))
    }

    /// `GET /assets/diff?from=<ver>` 的 JSON。
    ///
    /// `from` 为空或不在版本时间线里 → 整包；等于最新 → `none`；否则按索引算增量。
    pub fn diff(&self, from: &str) -> Result<Value, String> {
        let versions = self.load_versions()?;
        let latest = versions
            .last()
            .map(|v| v.version.clone())
            .ok_or_else(|| "仓库为空，先跑 scripts/dev/ota_pack.sh".to_string())?;

        if from == latest {
            return Ok(json!({
                "version": latest,
                "client": from,
                "mode": "none",
                "message": "already up to date",
            }));
        }
        let known = versions.iter().any(|v| v.version == from);
        if from.is_empty() || !known {
            let meta = self.ensure_full(&latest)?;
            return Ok(json!({
                "version": latest,
                "client": from,
                "mode": "full",
                "url": format!("/assets/full/{latest}.zip"),
                "sha256": meta.sha256,
                "bytes": meta.bytes,
            }));
        }

        let meta = self.ensure_delta(from, &latest)?;
        Ok(json!({
            "version": latest,
            "client": from,
            "mode": "delta",
            "url": meta.url,
            "sha256": meta.sha256,
            "bytes": meta.bytes,
            "files": meta.files,
            "deleted": meta.deleted,
            "cached": meta.cached,
        }))
    }

    /// 整包文件路径（不检查是否存在）。
    pub fn full_path(&self, version: &str) -> PathBuf {
        self.store.join("full").join(format!("{version}.zip"))
    }

    /// 增量包文件路径（不检查是否存在）。
    pub fn delta_path(&self, from: &str, to: &str) -> PathBuf {
        self.store.join("deltas").join(format!("{from}_{to}.zip"))
    }

    fn load_versions(&self) -> Result<Vec<VersionSummary>, String> {
        let path = self.store.join("versions.json");
        if !path.exists() {
            return Ok(Vec::new());
        }
        let text = fs::read_to_string(&path).map_err(|e| format!("读取 {:?} 失败：{e}", path))?;
        let file: VersionsFile =
            serde_json::from_str(&text).map_err(|e| format!("{:?} 不是合法 JSON：{e}", path))?;
        Ok(file.versions)
    }

    fn load_index(&self, version: &str) -> Result<Index, String> {
        let path = self.store.join("indexes").join(format!("{version}.json"));
        let text = fs::read_to_string(&path)
            .map_err(|e| format!("没有版本 {version} 的索引（{path:?}）：{e}"))?;
        serde_json::from_str(&text).map_err(|e| format!("{path:?} 不是合法 JSON：{e}"))
    }

    fn blob_path(&self, sha: &str) -> PathBuf {
        let prefix = sha.get(..2).unwrap_or("__");
        self.store.join("blobs").join(prefix).join(sha)
    }

    fn ensure_full(&self, version: &str) -> Result<FullMeta, String> {
        let zip_path = self.full_path(version);
        let meta_path = self.store.join("full").join(format!("{version}.json"));
        let _guard = self
            .build_lock
            .lock()
            .map_err(|_| "构建锁被毒化".to_string())?;

        if !zip_path.is_file() {
            let index = self.load_index(version)?;
            build_zip(&zip_path, &index.files, |sha| self.blob_path(sha))?;
        }
        if let Ok(text) = fs::read_to_string(&meta_path)
            && let Ok(meta) = serde_json::from_str::<FullMeta>(&text)
        {
            return Ok(meta);
        }
        let meta = FullMeta {
            sha256: sha256_file(&zip_path)?,
            bytes: fs::metadata(&zip_path)
                .map_err(|e| format!("读取 {:?} 大小失败：{e}", zip_path))?
                .len(),
        };
        write_json(&meta_path, &meta)?;
        Ok(meta)
    }

    fn ensure_delta(&self, from: &str, to: &str) -> Result<DeltaMeta, String> {
        let meta_path = self.store.join("deltas").join(format!("{from}_{to}.json"));
        let pack_path = self.delta_path(from, to);
        let _guard = self
            .build_lock
            .lock()
            .map_err(|_| "构建锁被毒化".to_string())?;

        if meta_path.is_file() && pack_path.is_file() {
            let text = fs::read_to_string(&meta_path)
                .map_err(|e| format!("读取 {:?} 失败：{e}", meta_path))?;
            let mut meta: DeltaMeta = serde_json::from_str(&text)
                .map_err(|e| format!("{:?} 不是合法 JSON：{e}", meta_path))?;
            meta.cached = true;
            return Ok(meta);
        }

        let old = self.load_index(from)?;
        let new = self.load_index(to)?;
        let old_by_path: std::collections::HashMap<&str, &FileEntry> =
            old.files.iter().map(|f| (f.path.as_str(), f)).collect();
        let changed: Vec<FileEntry> = new
            .files
            .iter()
            .filter(|f| {
                old_by_path
                    .get(f.path.as_str())
                    .is_none_or(|old| old.sha256 != f.sha256)
            })
            .cloned()
            .collect();
        let deleted: Vec<String> = {
            let new_paths: std::collections::HashSet<&str> =
                new.files.iter().map(|f| f.path.as_str()).collect();
            let mut deleted: Vec<String> = old
                .files
                .iter()
                .filter(|f| !new_paths.contains(f.path.as_str()))
                .map(|f| f.path.clone())
                .collect();
            deleted.sort();
            deleted
        };

        build_zip(&pack_path, &changed, |sha| self.blob_path(sha))?;
        let meta = DeltaMeta {
            from: from.to_string(),
            to: to.to_string(),
            url: format!("/assets/deltas/{from}_{to}.zip"),
            bytes: fs::metadata(&pack_path)
                .map_err(|e| format!("读取 {:?} 大小失败：{e}", pack_path))?
                .len(),
            sha256: sha256_file(&pack_path)?,
            files: changed,
            deleted,
            created: now_secs(),
            cached: false,
        };
        write_json(&meta_path, &meta)?;
        Ok(meta)
    }
}

/// 把 `files` 里列出的 blob 打成 deflate zip；条目路径就是资源相对路径。
fn build_zip(
    out: &Path,
    files: &[FileEntry],
    blob_of: impl Fn(&str) -> PathBuf,
) -> Result<(), String> {
    if let Some(parent) = out.parent() {
        fs::create_dir_all(parent).map_err(|e| format!("建目录 {parent:?} 失败：{e}"))?;
    }
    // 先写临时文件再改名，避免下载到一半的包被当成完整缓存
    let tmp = out.with_extension("zip.tmp");
    let result = (|| -> Result<(), String> {
        let file = File::create(&tmp).map_err(|e| format!("创建 {tmp:?} 失败：{e}"))?;
        let mut zip = ZipWriter::new(file);
        let options = SimpleFileOptions::default()
            .compression_method(CompressionMethod::Deflated)
            .compression_level(Some(6));
        let mut buf = vec![0u8; 256 * 1024];
        for entry in files {
            let blob = blob_of(&entry.sha256);
            let mut source = File::open(&blob)
                .map_err(|e| format!("blob 缺失 {}（{}）：{e}", entry.sha256, entry.path))?;
            zip.start_file(entry.path.as_str(), options)
                .map_err(|e| format!("zip 写入 {} 失败：{e}", entry.path))?;
            loop {
                let read = source
                    .read(&mut buf)
                    .map_err(|e| format!("读取 {blob:?} 失败：{e}"))?;
                if read == 0 {
                    break;
                }
                zip.write_all(&buf[..read])
                    .map_err(|e| format!("写 zip 失败：{e}"))?;
            }
        }
        zip.finish().map_err(|e| format!("收尾 zip 失败：{e}"))?;
        fs::rename(&tmp, out).map_err(|e| format!("改名 {tmp:?} -> {out:?} 失败：{e}"))
    })();
    if result.is_err() {
        let _ = fs::remove_file(&tmp);
    }
    result
}

fn write_json<T: Serialize>(path: &Path, value: &T) -> Result<(), String> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|e| format!("建目录 {parent:?} 失败：{e}"))?;
    }
    let text = serde_json::to_string_pretty(value).map_err(|e| e.to_string())?;
    fs::write(path, text).map_err(|e| format!("写 {path:?} 失败：{e}"))
}

fn sha256_file(path: &Path) -> Result<String, String> {
    let mut file = File::open(path).map_err(|e| format!("打开 {path:?} 失败：{e}"))?;
    let mut hasher = Sha256::new();
    let mut buf = vec![0u8; 1 << 20];
    loop {
        let read = file
            .read(&mut buf)
            .map_err(|e| format!("读取 {path:?} 失败：{e}"))?;
        if read == 0 {
            break;
        }
        hasher.update(&buf[..read]);
    }
    Ok(format!("{:x}", hasher.finalize()))
}

fn now_secs() -> String {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs().to_string())
        .unwrap_or_else(|_| "0".to_string())
}

/// 错误统一成 `io::Error`，给不接受 `String` 错误的地方用。
pub fn io_err(message: impl Into<String>) -> io::Error {
    io::Error::other(message.into())
}
