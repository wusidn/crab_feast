//! 玩家设置的持久化。
//!
//! 落盘位置 `<assets>/settings/movement.json`：`assets` 由
//! [`find_asset_root`](crate::anim::character::find_asset_root) 查找（`CRAB_ASSET_ROOT`，
//! 否则依次找工作目录 / 可执行文件旁的 `assets`）。找不到目录时只在内存里用，不让游戏失败。
//!
//! 写盘策略：
//! - 开关这类**离散**改动立即落盘（不会因为切得晚而丢），
//! - 滑杆这类**连续**改动停下 [`SAVE_DELAY`] 秒再落盘，拖动时不会每帧写盘；
//! - 退出时（`AppExit` / 离开 `Game` 状态）再补一次。
//!
//! 读盘时机：`Startup`，早于任何 `OnEnter` 状态切换，页面构建时拿到的就是磁盘上的值。

use std::path::{Path, PathBuf};

use bevy::{app::OnAppExitSystems, ecs::message::MessageReader, prelude::*};

use crate::{GameState, anim::character::find_asset_root, input::MovementSettings};

/// 设置文件所在子目录（相对 assets）
const SETTINGS_DIR: &str = "settings";
/// 移动设置文件名
const MOVEMENT_FILE: &str = "movement.json";
/// 连续改动停下多久后落盘（秒）
const SAVE_DELAY: f32 = 0.2;

/// 设置持久化插件
pub struct SettingsPlugin;

impl Plugin for SettingsPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<MovementSettings>()
            .init_resource::<PendingSave>()
            .insert_resource(MovementSettingsPath(movement_settings_path()))
            .add_systems(Startup, load_settings)
            .add_systems(Update, save_settings_debounced)
            // 退出游戏（回主菜单等）时补写
            .add_systems(OnExit(GameState::Game), flush_settings)
            // 关窗口退出时补写：AppExit 由 bevy_window 在 Last 里发出，
            // 必须排在它之后才看得到（写在 Update 里永远读不到这条消息）
            .add_systems(Last, flush_settings_on_exit.in_set(OnAppExitSystems));
    }
}

/// 移动设置文件路径；`None` 表示找不到 assets 目录，只在内存里用
#[derive(Resource, Clone, Debug)]
pub struct MovementSettingsPath(pub Option<PathBuf>);

/// 攒起来的待写设置
#[derive(Resource)]
struct PendingSave {
    timer: Timer,
    dirty: bool,
    /// 上次真正落盘的值，用来判断「这次改动是开关还是滑杆」
    last_saved: Option<MovementSettings>,
}

impl Default for PendingSave {
    fn default() -> Self {
        Self {
            timer: Timer::from_seconds(SAVE_DELAY, TimerMode::Once),
            dirty: false,
            last_saved: None,
        }
    }
}

impl PendingSave {
    /// 立刻落盘，并把这次写下的值记成基线
    fn write_now(&mut self, settings: &MovementSettings, path: &MovementSettingsPath) {
        let Some(path) = path.0.as_deref() else {
            // 没有 assets 目录：只留下基线，避免每帧重试
            self.last_saved = Some(settings.clone().sanitized());
            self.dirty = false;
            return;
        };
        match write_movement_settings(path, settings) {
            Ok(()) => {
                self.last_saved = Some(settings.clone().sanitized());
                self.dirty = false;
            }
            Err(error) => warn!("移动设置保存失败：{error}"),
        }
    }

    /// 跟磁盘上的值相比是否脏
    fn is_dirty_against(&self, settings: &MovementSettings) -> bool {
        match &self.last_saved {
            Some(last) => *last != *settings,
            None => true,
        }
    }
}

/// 设置目录
fn settings_dir() -> Option<PathBuf> {
    Some(find_asset_root()?.join(SETTINGS_DIR))
}

/// 移动设置文件路径
fn movement_settings_path() -> Option<PathBuf> {
    Some(settings_dir()?.join(MOVEMENT_FILE))
}

/// 从磁盘读移动设置；文件不存在返回 `Ok(None)`，读到的值会先夹到合法区间
fn read_movement_settings(path: &Path) -> Result<Option<MovementSettings>, String> {
    let content = match std::fs::read_to_string(path) {
        Ok(content) => content,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(format!("{} 读取失败：{error}", path.display())),
    };
    let settings = serde_json::from_str::<MovementSettings>(&content)
        .map_err(|error| format!("{} 解析失败：{error}", path.display()))?;
    Ok(Some(settings.sanitized()))
}

/// 把移动设置写到磁盘，顺带建目录；写入前再夹一次区间
fn write_movement_settings(path: &Path, settings: &MovementSettings) -> Result<(), String> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|error| format!("{} 目录创建失败：{error}", parent.display()))?;
    }
    let json = serde_json::to_string_pretty(&settings.clone().sanitized())
        .map_err(|error| format!("移动设置序列化失败：{error}"))?;
    std::fs::write(path, json).map_err(|error| format!("{} 写入失败：{error}", path.display()))
}

/// 启动时把磁盘上的设置灌进资源
fn load_settings(
    mut settings: ResMut<MovementSettings>,
    path: Res<MovementSettingsPath>,
    mut pending: ResMut<PendingSave>,
) {
    let loaded = match path.0.as_deref() {
        Some(path) => match read_movement_settings(path) {
            Ok(loaded) => loaded,
            Err(error) => {
                warn!("移动设置读取失败，使用默认值：{error}");
                None
            }
        },
        None => None,
    };
    match loaded {
        Some(loaded) => {
            info!("移动设置已从磁盘载入：{loaded:?}");
            *settings = loaded;
        }
        None => info!("没有移动设置文件，使用默认值"),
    }
    // 记为基线，免得启动那一帧又被当成改动重写一遍
    pending.last_saved = Some(settings.clone().sanitized());
}

/// 资源被改过后落盘：离散改动（开关）立刻写，连续改动（滑杆）等停手
fn save_settings_debounced(
    settings: Res<MovementSettings>,
    path: Res<MovementSettingsPath>,
    time: Res<Time>,
    mut pending: ResMut<PendingSave>,
) {
    if settings.is_changed() {
        // 开关是一步到位的改动：立刻落盘，避免「切完就退出」丢掉
        // （以后加了别的开关类字段，记得在这里一并比较）
        let discrete_changed = pending
            .last_saved
            .as_ref()
            .is_none_or(|last| last.smoothing_enabled != settings.smoothing_enabled);
        if discrete_changed {
            pending.write_now(&settings, &path);
            return;
        }
        // 滑杆还在拖：把计时器踢回去，等手停下来
        pending.dirty = true;
        pending.timer.reset();
        return;
    }

    if !pending.dirty {
        return;
    }
    pending.timer.tick(time.delta());
    if !pending.timer.is_finished() {
        return;
    }
    pending.write_now(&settings, &path);
}

/// 离开游戏状态时补写（比如回主菜单）
fn flush_settings(
    settings: Res<MovementSettings>,
    path: Res<MovementSettingsPath>,
    mut pending: ResMut<PendingSave>,
) {
    if pending.dirty || pending.is_dirty_against(&settings) {
        pending.write_now(&settings, &path);
    }
}

/// 关窗口退出前补写
fn flush_settings_on_exit(
    mut exits: MessageReader<AppExit>,
    settings: Res<MovementSettings>,
    path: Res<MovementSettingsPath>,
    mut pending: ResMut<PendingSave>,
) {
    if exits.read().next().is_none() {
        return;
    }
    if pending.dirty || pending.is_dirty_against(&settings) {
        pending.write_now(&settings, &path);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn temp_path(name: &str) -> PathBuf {
        std::env::temp_dir()
            .join(format!("crab_feast_settings_{}", std::process::id()))
            .join(name)
    }

    /// 一份「只有持久化系统」的最小 App：`App::new()` 自带 MainSchedule，但不带 TimePlugin，
    /// 这样测试里可以自己推进 `Time` 来模拟防抖
    fn settings_app(path: &Path) -> App {
        let mut app = App::new();
        app.insert_resource(Time::<()>::default())
            .insert_resource(MovementSettings::default())
            .init_resource::<PendingSave>()
            .insert_resource(MovementSettingsPath(Some(path.to_path_buf())))
            .add_systems(Startup, load_settings)
            .add_systems(Update, save_settings_debounced);
        app
    }

    fn advance(app: &mut App, seconds: f32) {
        app.world_mut()
            .resource_mut::<Time>()
            .advance_by(std::time::Duration::from_secs_f32(seconds));
    }

    // ---- 纯函数层面的读写 ----

    #[test]
    fn missing_file_is_not_an_error() {
        let path = temp_path("missing/movement.json");
        assert!(read_movement_settings(&path).unwrap().is_none());
    }

    #[test]
    fn write_then_read_round_trips() {
        let path = temp_path("round_trip/movement.json");
        let settings = MovementSettings {
            smoothing_enabled: false,
            smoothing_factor: 0.25,
        };
        write_movement_settings(&path, &settings).unwrap();

        let loaded = read_movement_settings(&path).unwrap().unwrap();
        assert!(!loaded.smoothing_enabled);
        assert!((loaded.smoothing_factor - 0.25).abs() < f32::EPSILON);

        std::fs::remove_dir_all(path.parent().unwrap()).ok();
    }

    #[test]
    fn out_of_range_factor_is_clamped_on_load() {
        let path = temp_path("clamp/movement.json");
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).unwrap();
        }
        std::fs::write(
            &path,
            r#"{ "smoothing_enabled": true, "smoothing_factor": 3.5 }"#,
        )
        .unwrap();
        let loaded = read_movement_settings(&path).unwrap().unwrap();
        assert_eq!(loaded.smoothing_factor, 1.0);

        std::fs::write(&path, r#"{ "smoothing_factor": -2.0 }"#).unwrap();
        let loaded = read_movement_settings(&path).unwrap().unwrap();
        assert_eq!(loaded.smoothing_factor, crate::input::SMOOTHING_MIN);

        std::fs::remove_dir_all(path.parent().unwrap()).ok();
    }

    #[test]
    fn partial_file_falls_back_to_defaults() {
        let path = temp_path("partial/movement.json");
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).unwrap();
        }
        std::fs::write(&path, r#"{ "smoothing_factor": 0.8 }"#).unwrap();

        let loaded = read_movement_settings(&path).unwrap().unwrap();
        assert_eq!(loaded.smoothing_factor, 0.8);
        // 缺的字段走默认值
        assert!(loaded.smoothing_enabled);

        std::fs::remove_dir_all(path.parent().unwrap()).ok();
    }

    // ---- 系统层面：开关必须立刻落盘，滑杆要防抖 ----

    /// 开关切完不等防抖就要在磁盘上能读到（这正是「切完就退出」丢配置的根因）
    #[test]
    fn toggle_is_persisted_immediately() {
        let path = temp_path("toggle/movement.json");
        let mut app = settings_app(&path);
        app.update();

        // 模拟点一下开关
        app.world_mut()
            .resource_mut::<MovementSettings>()
            .smoothing_enabled = false;
        app.update();

        let saved = read_movement_settings(&path).unwrap().unwrap();
        assert!(!saved.smoothing_enabled, "开关改动必须立刻落盘");

        std::fs::remove_dir_all(path.parent().unwrap()).ok();
    }

    /// 拖动滑杆时先不写，停手超过 SAVE_DELAY 才写
    #[test]
    fn slider_change_is_debounced() {
        let path = temp_path("slider/movement.json");
        // 先落一份基线，模拟「上一次已经保存过」
        write_movement_settings(&path, &MovementSettings::default()).unwrap();
        let mut app = settings_app(&path);
        app.update();
        let before = std::fs::read_to_string(&path).unwrap();

        app.world_mut()
            .resource_mut::<MovementSettings>()
            .smoothing_factor = 0.42;
        app.update(); // 记脏、重置计时器，但不写
        assert_eq!(std::fs::read_to_string(&path).unwrap(), before);

        advance(&mut app, SAVE_DELAY + 0.05);
        app.update(); // 计时器到点，落盘
        let saved = read_movement_settings(&path).unwrap().unwrap();
        assert!((saved.smoothing_factor - 0.42).abs() < 1e-6);

        std::fs::remove_dir_all(path.parent().unwrap()).ok();
    }

    /// 启动时文件里的值要覆盖掉默认资源
    #[test]
    fn startup_loads_file_over_defaults() {
        let path = temp_path("load_on_startup/movement.json");
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).unwrap();
        }
        std::fs::write(
            &path,
            r#"{ "smoothing_enabled": false, "smoothing_factor": 0.12 }"#,
        )
        .unwrap();

        let mut app = settings_app(&path);
        app.update();

        let settings = app.world().resource::<MovementSettings>();
        assert!(!settings.smoothing_enabled);
        assert!((settings.smoothing_factor - 0.12).abs() < 1e-6);

        std::fs::remove_dir_all(path.parent().unwrap()).ok();
    }

    /// 退出兜底：脏了就要补写（不依赖防抖计时器）
    #[test]
    fn flush_writes_pending_change() {
        let path = temp_path("flush/movement.json");
        let mut app = settings_app(&path);
        app.add_systems(Update, flush_settings);
        app.update();

        app.world_mut()
            .resource_mut::<MovementSettings>()
            .smoothing_factor = 0.33;
        app.update(); // 记脏

        // 手动模拟「离开 Game 状态」的兜底写入
        let world = app.world_mut();
        world.resource_mut::<PendingSave>().dirty = true;
        let settings = world.resource::<MovementSettings>().clone();
        let path_res = world.resource::<MovementSettingsPath>().clone();
        world
            .resource_mut::<PendingSave>()
            .write_now(&settings, &path_res);

        let saved = read_movement_settings(&path).unwrap().unwrap();
        assert!((saved.smoothing_factor - 0.33).abs() < 1e-6);

        std::fs::remove_dir_all(path.parent().unwrap()).ok();
    }
}
