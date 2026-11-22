use std::net::{SocketAddr, ToSocketAddrs};

use bevy::prelude::*;
use bevy::render::view::screenshot::{Screenshot, save_to_disk};
use bevy_inspector_egui::{bevy_egui::EguiPlugin, quick::WorldInspectorPlugin};
use crab_feast::{AutoJoin, AutoJump, AutoWalk, NetConfig, NetPlugin, RegistryConfig, SessionMode};
use crab_feast_launcher::{
    LaunchConfig, build_app,
    config::{LauncherConfig, config_path_from_args},
};

fn main() {
    // `--launch-all`：按 launcher.json 一键拉起服务器 + 所有玩家窗口
    if std::env::args().any(|arg| arg == "--launch-all") {
        launch_all();
        return;
    }

    let args = parse_args();
    println!(
        "启动玩家位 {}：client_id={}，服务器 {}，窗口 {:?}",
        args.profile, args.client_id, args.server, args.window
    );

    // 同机多开时把窗口错开，方便同时看到两个客户端
    let launch = LaunchConfig {
        window_position: args
            .window
            .map(|(x, y)| IVec2::new(x, y)),
        ..LaunchConfig::default()
    };

    let mut app = build_app(launch, move |app| {
        app.add_plugins(EguiPlugin::default())
            .add_plugins(WorldInspectorPlugin::new())
            // 联网时窗口失焦不能让主循环停下来：lightyear 的 tick、收包、复制
            // 都跑在 Update 里，一旦进入低频模式，别人看到的就是你"卡住不动"
            .insert_resource(bevy::winit::WinitSettings {
                focused_mode: bevy::winit::UpdateMode::Continuous,
                unfocused_mode: bevy::winit::UpdateMode::Continuous,
            })
            .insert_resource(NetConfig {
                server: args.server,
                client_id: args.client_id,
            })
            // 大厅的服务器列表：局域网广播 + 注册中心 HTTP。
            // 必须在 `crab_feast::build_app` 之前插入——DiscoveryPlugin 在 build
            // 阶段只读一次这个资源，晚插入就退回环境变量/默认地址了。
            .insert_resource(RegistryConfig::from_option(args.registry.clone()))
            // `--server` / `--local` 带了明确目标就跳过大厅直接登录；否则停在大厅
            .insert_resource(AutoJoin(args.auto_join))
            .add_plugins(NetPlugin);

        if let Some(secs) = args.auto_walk {
            app.insert_resource(AutoWalk(Timer::from_seconds(secs, TimerMode::Once)));
        }

        if let Some(period) = args.auto_jump {
            app.insert_resource(AutoJump::new(period));
        }

        if let Some(path) = args.screenshot.clone() {
            // TEMP: CRAB_SHOT_EVERY=0.0335 时从 8s 起连拍，用于看跑跳过程
            let every = std::env::var("CRAB_SHOT_EVERY")
                .ok()
                .and_then(|v| v.parse::<f32>().ok());
            app.insert_resource(ScreenshotPlan {
                path,
                timer: Timer::from_seconds(8.0, TimerMode::Once),
                every,
                next: std::env::var("CRAB_SHOT_AT")
                    .ok()
                    .and_then(|v| v.parse::<f32>().ok())
                    .unwrap_or(8.0),
                index: 0,
            })
            .add_systems(Update, take_screenshot);
        }

        if let Some(secs) = args.exit_after {
            app.insert_resource(ExitAfter(Timer::from_seconds(secs, TimerMode::Once)))
                .add_systems(Update, exit_when_elapsed);
        }

        crab_feast::build_app(app);
    });

    app.run();
}

/// 冒烟测试用：跑够 8 秒把主窗口画面存成 png
#[derive(Resource)]
struct ScreenshotPlan {
    path: std::path::PathBuf,
    timer: Timer,
    every: Option<f32>,
    next: f32,
    index: u32,
}

fn take_screenshot(
    time: Res<Time>,
    mut plan: ResMut<ScreenshotPlan>,
    mut commands: Commands,
) {
    let now = time.elapsed_secs();
    let Some(every) = plan.every else {
        if !plan.timer.tick(time.delta()).just_finished() {
            return;
        }
        let path = plan.path.clone();
        info!("screenshot: 保存到 {:?}", path);
        commands
            .spawn(Screenshot::primary_window())
            .observe(save_to_disk(path));
        return;
    };
    if now < plan.next {
        return;
    }
    plan.next += every;
    let stem = plan
        .path
        .file_stem()
        .map(|s| s.to_string_lossy().to_string())
        .unwrap_or_else(|| "shot".into());
    let path = plan
        .path
        .with_file_name(format!("{stem}-{:03}.png", plan.index));
    plan.index += 1;
    commands
        .spawn(Screenshot::primary_window())
        .observe(save_to_disk(path));
}

/// `--server` 既接受 `IP:端口`，也接受域名（`game.qiaqia.cyou:5099`）。
///
/// 域名可能有 A/AAAA 两条记录，而当前游戏服只监听 IPv6，优先取 AAAA；
/// 纯 IPv4 的地址仍按原样使用。
fn resolve_server_addr(value: &str) -> Result<SocketAddr, String> {
    let trimmed = value.trim();
    let mut addrs = trimmed
        .to_socket_addrs()
        .map_err(|error| format!("{error}"))?;
    let mut first = None;
    for addr in addrs.by_ref() {
        if addr.is_ipv6() {
            return Ok(addr);
        }
        first.get_or_insert(addr);
    }
    first.ok_or_else(|| "没有解析到任何地址".to_string())
}

struct Args {
    /// 用的是 launcher.json 里的哪个玩家位（没配就是 auto）
    profile: String,
    server: SocketAddr,
    client_id: u64,
    /// 命令行明确指定了「直接进哪一局」：连服务器或本地游戏
    auto_join: Option<SessionMode>,
    /// 注册中心地址；`None` = 用默认/环境变量，`Some("off")` = 关闭
    registry: Option<String>,
    /// 窗口左上角（同机多开时错开用）
    window: Option<(i32, i32)>,
    /// 跑够多少秒自动退出（冒烟测试用，正常玩不要传）
    exit_after: Option<f32>,
    /// 自动前进多少秒（冒烟测试用，模拟一直推摇杆向前）
    auto_walk: Option<f32>,
    /// 自动跳的周期秒数（冒烟测试用，模拟点按跳跃）
    auto_jump: Option<f32>,
    /// 8 秒时把窗口画面存成 png（冒烟测试用）
    screenshot: Option<std::path::PathBuf>,
}

fn parse_args() -> Args {
    let config = LauncherConfig::load(config_path_from_args().as_deref());

    // 支持 `--key=value` 写法：先归一化成 `--key value`，后面只按空格形式解析
    let mut normalized: Vec<String> = Vec::new();
    for arg in std::env::args().skip(1) {
        if let Some(rest) = arg.strip_prefix("--")
            && let Some((key, value)) = rest.split_once('=')
        {
            normalized.push(format!("--{key}"));
            normalized.push(value.to_string());
            continue;
        }
        normalized.push(arg);
    }
    let mut args = normalized.into_iter();
    let mut server: Option<String> = None;
    let mut profile: Option<String> = None;
    // netcode 的 client_id 必须全局唯一，两台客户端都用默认值会互相顶掉。
    // 优先用 launcher.json 里给每个玩家位配好的 id，都没有才按时间戳生成。
    let mut client_id: Option<u64> = None;
    let mut exit_after = None;
    let mut auto_walk = None;
    let mut auto_jump = None;
    let mut window = None;
    let mut screenshot: Option<std::path::PathBuf> = None;
    let mut explicit_server = false;
    let mut local_game = false;
    let mut registry: Option<String> = None;
    let mut no_registry = false;
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--profile" => {
                if let Some(value) = args.next() {
                    profile = Some(value);
                }
            }
            "--config" => {
                // 已经提前读过（config_path_from_args），这里只是把值跳过
                let _ = args.next();
            }
            "--server" => {
                if let Some(value) = args.next() {
                    explicit_server = true;
                    server = Some(value);
                }
            }
            "--local" => {
                local_game = true;
            }
            "--registry" => {
                if let Some(value) = args.next() {
                    registry = Some(value);
                }
            }
            "--no-registry" => no_registry = true,
            "--client-id" => {
                if let Some(value) = args.next() {
                    client_id = value.parse().ok();
                }
            }
            "--screenshot" => {
                if let Some(value) = args.next() {
                    screenshot = Some(value.into());
                }
            }
            "--window" => {
                if let Some(value) = args.next() {
                    let mut parts = value.split(',');
                    let x = parts.next().and_then(|v| v.parse().ok());
                    let y = parts.next().and_then(|v| v.parse().ok());
                    if let (Some(x), Some(y)) = (x, y) {
                        window = Some((x, y));
                    }
                }
            }
            "--exit-after" => {
                if let Some(value) = args.next() {
                    exit_after = value.parse().ok();
                }
            }
            "--auto-walk" => {
                if let Some(value) = args.next() {
                    auto_walk = value.parse().ok();
                }
            }
            "--auto-jump" => {
                if let Some(value) = args.next() {
                    auto_jump = value.parse().ok();
                }
            }
            "--help" | "-h" => {
                println!(
                    "crab_feast_pc --launch-all   # 按 launcher.json 起服务器 + 所有玩家\n\
                     crab_feast_pc [--profile player1] [--server game.qiaqia.cyou:5099 | --local] \
                     [--client-id <n>] [--registry http://127.0.0.1:8090 | --no-registry] \
                     [--window x,y] [--config launcher.json] \
                     [--screenshot <png>] [--exit-after <秒>] [--auto-walk <秒>]\n\
                     不传 --profile 时沿用 launcher.json 的 default_player 作为窗口/地址兜底，\n\
                     但 client_id 会按时间戳自动生成，保证同机多开不会撞号；\n\
                     停在大厅的服务器列表；--server 自动加入该服务器，--local 直接本地游戏。\n\
                     默认同时展示局域网广播与注册中心里的服务器，--no-registry 可关闭后者。"
                );
                std::process::exit(0);
            }
            other => eprintln!("未知参数 {other}，已忽略"),
        }
    }
    // 玩家位名字：命令行 > 环境变量 > launcher.json 的 default_player。
    // 前两者算「明确指定」；default_player 只是单开时的兜底——多开时每个进程
    // 都会落到 player1，client_id 撞车会被 netcode 服务端静默丢弃（表现为连接超时）。
    let env_profile = std::env::var("CRAB_PROFILE").ok();
    let explicit_profile = profile.is_some() || env_profile.is_some();
    let profile_name = profile
        .or(env_profile)
        .or_else(|| config.default_profile_name().map(str::to_string));
    let profile = profile_name.as_deref().map(|name| config.player(name).clone());

    let server = server
        .or_else(|| profile.as_ref().and_then(|p| p.server.clone()))
        .or_else(|| config.server.addr.clone())
        .unwrap_or_else(|| crab_feast_library::net::DEFAULT_ADDR.to_string());
    let server = resolve_server_addr(&server).unwrap_or_else(|error| {
        eprintln!("服务器地址 {server} 无法解析：{error}");
        std::process::exit(2);
    });
    // 只有明确点了玩家位才沿用配置里的 id；否则一律生成唯一 id，
    // 避免第二个实例悄悄复用 player1 的 11 把第一个顶掉。
    let client_id = client_id
        .or_else(|| {
            if explicit_profile {
                profile.as_ref().map(|p| p.client_id)
            } else {
                None
            }
        })
        .unwrap_or_else(unique_client_id);
    // 没显式指定玩家位时窗口按进程号错开，避免两次默认启动完全叠在一起
    let window = window
        .or_else(|| profile.as_ref().and_then(|p| p.window.map(|[x, y]| (x, y))))
        .map(|(x, y)| {
            if explicit_profile {
                (x, y)
            } else {
                let step = (std::process::id() % 6) as i32 * 48;
                (x + step, y + step)
            }
        });
    let exit_after = exit_after.or_else(|| profile.as_ref().and_then(|p| p.exit_after));
    // 明确给了 --server 就自动加入；否则 --local 直接本地游戏；
    // 都没给就停在大厅，等玩家自己点
    let auto_join = if explicit_server {
        Some(SessionMode::Online { server })
    } else if local_game {
        Some(SessionMode::Local)
    } else {
        None
    };

    // 注册中心：命令行 > launcher.json 顶层 registry；`--no-registry` 用 "off" 表达。
    // 两者都没给时传 None，`RegistryConfig` 会退回 CRAB_REGISTRY_URL 或内置默认。
    let registry = if no_registry {
        Some("off".to_string())
    } else {
        registry.or_else(|| config.registry.clone())
    };

    Args {
        profile: profile_name.unwrap_or_else(|| "auto".to_string()),
        server,
        client_id,
        auto_join,
        registry,
        window,
        exit_after,
        auto_walk,
        auto_jump,
        screenshot,
    }
}

/// 一键拉起：服务器进程 + 每个玩家位一个客户端进程。
/// 父进程只负责起进程，起完就退出；关窗口即结束对应客户端。
fn launch_all() {
    let config_path = LauncherConfig::resolve_path(config_path_from_args().as_deref());
    let config = LauncherConfig::load(config_path.as_deref());
    let exe = std::env::current_exe().expect("拿不到当前可执行文件路径");
    let dir = exe.parent().expect("可执行文件没有所在目录").to_path_buf();
    let config_arg: Vec<String> = config_path
        .as_ref()
        .and_then(|path| path.canonicalize().ok())
        .map(|path| vec!["--config".to_string(), path.display().to_string()])
        .unwrap_or_default();

    let server_bin = dir.join(format!("crab-feast-server{}", std::env::consts::EXE_SUFFIX));
    let mut server = std::process::Command::new(&server_bin);
    server.args(&config_arg);
    match server.spawn() {
        Ok(child) => println!("已启动服务器（pid {}）：{}", child.id(), server_bin.display()),
        Err(error) => {
            eprintln!("服务器 {} 启动失败：{error}", server_bin.display());
            std::process::exit(2);
        }
    }

    for player in &config.players {
        let mut client = std::process::Command::new(&exe);
        client.args(["--profile", &player.name]).args(&config_arg);
        match client.spawn() {
            Ok(child) => println!(
                "已启动玩家位 {}（client_id {}，pid {}）",
                player.name,
                player.client_id,
                child.id()
            ),
            Err(error) => eprintln!("玩家位 {} 启动失败：{error}", player.name),
        }
    }
}

/// 没传 `--client-id` 时用纳秒时间戳派生一个编号。
fn unique_client_id() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos() as u64)
        .unwrap_or(1)
}

#[derive(Resource)]
struct ExitAfter(Timer);

fn exit_when_elapsed(
    time: Res<Time>,
    mut timer: ResMut<ExitAfter>,
    mut exit: MessageWriter<AppExit>,
) {
    if timer.0.tick(time.delta()).just_finished() {
        info!("--exit-after 到点，客户端退出");
        exit.write(AppExit::Success);
    }
}
