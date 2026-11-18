//! `launcher.json`：把「服务器地址、每个玩家用哪个 client_id、窗口摆在哪」从命令行
//! 搬到配置文件里，日常启动不用再记参数。
//!
//! 查找顺序（先命中先用）：
//! 1. `--config <路径>` 显式指定
//! 2. 环境变量 `CRAB_LAUNCHER_CONFIG`
//! 3. 当前工作目录下的 `launcher.json`
//! 4. 可执行文件同目录的 `launcher.json`
//! 5. 仓库根目录的 `launcher.json`（debug 构建时的默认）
//!
//! 都找不到时用内置默认值（一个 server + player1/player2 两个玩家位），
//! 保证程序永远能起来。

use std::path::{Path, PathBuf};

use serde::Deserialize;

/// 整份启动配置。
#[derive(Deserialize, Clone, Debug)]
pub struct LauncherConfig {
    #[serde(default)]
    pub server: ServerSection,
    /// 玩家位列表：每个对应一个客户端进程。
    #[serde(default)]
    pub players: Vec<PlayerProfile>,
    /// 不传 `--profile` 时默认用哪个玩家位。
    #[serde(default)]
    pub default_player: Option<String>,
}

/// 服务端段。
#[derive(Deserialize, Clone, Debug)]
pub struct ServerSection {
    /// 监听地址；留空则用协议默认 `127.0.0.1:5000`。
    #[serde(default)]
    pub addr: Option<String>,
}

/// 一个玩家位：一个客户端进程的启动参数。
#[derive(Deserialize, Clone, Debug)]
pub struct PlayerProfile {
    /// `--profile` 用的名字。
    pub name: String,
    /// netcode client id，**必须全局唯一**；两个客户端撞 id 会互相顶掉。
    pub client_id: u64,
    /// 窗口左上角 `[x, y]`，同机多开时用来错开窗口。
    #[serde(default)]
    pub window: Option<[i32; 2]>,
    /// 想连的服务器地址；不填就用 `server.addr`。
    #[serde(default)]
    pub server: Option<String>,
    /// 跑够多少秒自动退出，冒烟测试用；正常玩不填。
    #[serde(default)]
    pub exit_after: Option<f32>,
}

impl Default for ServerSection {
    fn default() -> Self {
        Self { addr: None }
    }
}

impl Default for LauncherConfig {
    fn default() -> Self {
        Self {
            server: ServerSection::default(),
            players: vec![
                PlayerProfile {
                    name: "player1".to_string(),
                    client_id: 11,
                    window: Some([0, 60]),
                    server: None,
                    exit_after: None,
                },
                PlayerProfile {
                    name: "player2".to_string(),
                    client_id: 12,
                    window: Some([1280, 60]),
                    server: None,
                    exit_after: None,
                },
            ],
            default_player: Some("player1".to_string()),
        }
    }
}

impl LauncherConfig {
    /// 按查找顺序读配置；文件存在但解析失败时直接报错退出（不静默退回默认值，
    /// 否则改错一个字段会变成"参数怎么都不生效"）。
    pub fn load(explicit: Option<&Path>) -> Self {
        match Self::find_path(explicit) {
            Some(path) => match std::fs::read_to_string(&path) {
                Ok(text) => match serde_json::from_str::<Self>(&text) {
                    Ok(config) => {
                        log::info!("launcher: 读取配置 {}", path.display());
                        config
                    }
                    Err(error) => {
                        log::error!("launcher: {} 解析失败：{error}", path.display());
                        std::process::exit(2);
                    }
                },
                Err(error) => {
                    log::error!("launcher: {} 读取失败：{error}", path.display());
                    std::process::exit(2);
                }
            },
            None => {
                log::info!("launcher: 没找到 launcher.json，使用内置默认配置");
                Self::default()
            }
        }
    }

    /// 实际会用到的配置文件路径；找不到返回 `None`。
    pub fn resolve_path(explicit: Option<&Path>) -> Option<PathBuf> {
        Self::find_path(explicit)
    }

    fn find_path(explicit: Option<&Path>) -> Option<PathBuf> {
        if let Some(path) = explicit {
            return Some(path.to_path_buf());
        }
        if let Ok(path) = std::env::var("CRAB_LAUNCHER_CONFIG") {
            if !path.is_empty() {
                return Some(PathBuf::from(path));
            }
        }
        let mut candidates = Vec::new();
        candidates.push(PathBuf::from("launcher.json"));
        if let Ok(exe) = std::env::current_exe() {
            if let Some(dir) = exe.parent() {
                candidates.push(dir.join("launcher.json"));
            }
        }
        candidates.push(PathBuf::from(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../launcher.json"
        )));
        candidates.into_iter().find(|path| path.is_file())
    }

    /// 按名字取玩家位；名字不存在时报错并列出可选项。
    pub fn player(&self, name: &str) -> &PlayerProfile {
        match self.players.iter().find(|player| player.name == name) {
            Some(player) => player,
            None => {
                let names: Vec<&str> = self.players.iter().map(|p| p.name.as_str()).collect();
                log::error!(
                    "launcher: launcher.json 里没有名为 {name} 的玩家位，可选：{names:?}"
                );
                std::process::exit(2);
            }
        }
    }

    /// 不指定 `--profile` 时该用哪个玩家位。
    pub fn default_profile_name(&self) -> Option<&str> {
        self.default_player
            .as_deref()
            .or_else(|| self.players.first().map(|player| player.name.as_str()))
    }
}

/// 从命令行里抽出 `--config <路径>`：其他业务参数各自解析自己的，这里只负责通用项。
pub fn config_path_from_args() -> Option<PathBuf> {
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        if arg == "--config" {
            return args.next().map(PathBuf::from);
        }
    }
    None
}
