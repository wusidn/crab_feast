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

use crate::{
    GameState,
    anim::{character::find_asset_root, jump::JumpSettings},
    camera::CameraFollowSettings,
    input::MovementSettings,
    lighting::LightingSettings,
};

/// 设置文件所在子目录（相对 assets）
const SETTINGS_DIR: &str = "settings";
/// 移动设置文件名
const MOVEMENT_FILE: &str = "movement.json";
/// 相机设置文件名
const CAMERA_FILE: &str = "camera.json";
/// 全局光照设置文件名
const LIGHTING_FILE: &str = "lighting.json";
/// 跳跃设置文件名
const JUMP_FILE: &str = "jump.json";
/// 连续改动停下多久后落盘（秒）
const SAVE_DELAY: f32 = 0.2;

/// 设置持久化插件
pub struct SettingsPlugin;

impl Plugin for SettingsPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<MovementSettings>()
            .init_resource::<CameraFollowSettings>()
            .init_resource::<LightingSettings>()
            .init_resource::<JumpSettings>()
            .init_resource::<PendingSave>()
            .init_resource::<PendingCameraSave>()
            .init_resource::<PendingLightingSave>()
            .init_resource::<PendingJumpSave>()
            .insert_resource(MovementSettingsPath(movement_settings_path()))
            .insert_resource(CameraSettingsPath(camera_settings_path()))
            .insert_resource(LightingSettingsPath(lighting_settings_path()))
            .insert_resource(JumpSettingsPath(jump_settings_path()))
            .add_systems(
                Startup,
                (
                    load_settings,
                    load_camera_settings,
                    load_lighting_settings,
                    load_jump_settings,
                ),
            )
            .add_systems(
                Update,
                (
                    save_settings_debounced,
                    save_camera_settings_debounced,
                    save_lighting_settings_debounced,
                    save_jump_settings_debounced,
                ),
            )
            // 退出游戏（回主菜单等）时补写
            .add_systems(
                OnExit(GameState::Game),
                (
                    flush_settings,
                    flush_camera_settings,
                    flush_lighting_settings,
                    flush_jump_settings,
                ),
            )
            // 关窗口退出时补写：AppExit 由 bevy_window 在 Last 里发出，
            // 必须排在它之后才看得到（写在 Update 里永远读不到这条消息）
            .add_systems(
                Last,
                (
                    flush_settings_on_exit,
                    flush_camera_settings_on_exit,
                    flush_lighting_settings_on_exit,
                    flush_jump_settings_on_exit,
                )
                    .in_set(OnAppExitSystems),
            );
    }
}

/// 移动设置文件路径；`None` 表示找不到 assets 目录，只在内存里用
#[derive(Resource, Clone, Debug)]
pub struct MovementSettingsPath(pub Option<PathBuf>);

/// 相机设置文件路径；语义同上
#[derive(Resource, Clone, Debug)]
pub struct CameraSettingsPath(pub Option<PathBuf>);

/// 光照设置文件路径；语义同上
#[derive(Resource, Clone, Debug)]
pub struct LightingSettingsPath(pub Option<PathBuf>);

/// 跳跃设置文件路径；语义同上
#[derive(Resource, Clone, Debug)]
pub struct JumpSettingsPath(pub Option<PathBuf>);

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

/// 攒起来的待写相机设置
#[derive(Resource)]
struct PendingCameraSave {
    timer: Timer,
    dirty: bool,
    /// 上次真正落盘的值：用来判断「这次改动是换策略还是拖滑杆」
    last_saved: Option<CameraFollowSettings>,
}

impl Default for PendingCameraSave {
    fn default() -> Self {
        Self {
            timer: Timer::from_seconds(SAVE_DELAY, TimerMode::Once),
            dirty: false,
            last_saved: None,
        }
    }
}

impl PendingCameraSave {
    /// 立刻落盘，并把这次写下的值记成基线
    fn write_now(&mut self, settings: &CameraFollowSettings, path: &CameraSettingsPath) {
        let Some(path) = path.0.as_deref() else {
            // 没有 assets 目录：只留下基线，避免每帧重试
            self.last_saved = Some(settings.sanitized());
            self.dirty = false;
            return;
        };
        match write_camera_settings(path, settings) {
            Ok(()) => {
                self.last_saved = Some(settings.sanitized());
                self.dirty = false;
            }
            Err(error) => warn!("相机设置保存失败：{error}"),
        }
    }

    /// 跟磁盘上的值相比是否脏
    fn is_dirty_against(&self, settings: &CameraFollowSettings) -> bool {
        match &self.last_saved {
            Some(last) => *last != *settings,
            None => true,
        }
    }
}

/// 攒起来的待写光照设置
#[derive(Resource)]
struct PendingLightingSave {
    timer: Timer,
    dirty: bool,
    /// 上次真正落盘的值：光照只有滑杆一项，全靠它判断脏不脏
    last_saved: Option<LightingSettings>,
}

impl Default for PendingLightingSave {
    fn default() -> Self {
        Self {
            timer: Timer::from_seconds(SAVE_DELAY, TimerMode::Once),
            dirty: false,
            last_saved: None,
        }
    }
}

impl PendingLightingSave {
    /// 立刻落盘，并把这次写下的值记成基线
    fn write_now(&mut self, settings: &LightingSettings, path: &LightingSettingsPath) {
        let Some(path) = path.0.as_deref() else {
            // 没有 assets 目录：只留下基线，避免每帧重试
            self.last_saved = Some(settings.sanitized());
            self.dirty = false;
            return;
        };
        match write_lighting_settings(path, settings) {
            Ok(()) => {
                self.last_saved = Some(settings.sanitized());
                self.dirty = false;
            }
            Err(error) => warn!("光照设置保存失败：{error}"),
        }
    }

    /// 跟磁盘上的值相比是否脏
    fn is_dirty_against(&self, settings: &LightingSettings) -> bool {
        match &self.last_saved {
            Some(last) => *last != *settings,
            None => true,
        }
    }
}

/// 攒起来的待写跳跃设置
#[derive(Resource)]
struct PendingJumpSave {
    timer: Timer,
    dirty: bool,
    /// 上次真正落盘的值：跳跃三项全是滑杆，只靠它判断脏不脏
    last_saved: Option<JumpSettings>,
}

impl Default for PendingJumpSave {
    fn default() -> Self {
        Self {
            timer: Timer::from_seconds(SAVE_DELAY, TimerMode::Once),
            dirty: false,
            last_saved: None,
        }
    }
}

impl PendingJumpSave {
    /// 立刻落盘，并把这次写下的值记成基线
    fn write_now(&mut self, settings: &JumpSettings, path: &JumpSettingsPath) {
        let Some(path) = path.0.as_deref() else {
            // 没有 assets 目录：只留下基线，避免每帧重试
            self.last_saved = Some(settings.sanitized());
            self.dirty = false;
            return;
        };
        match write_jump_settings(path, settings) {
            Ok(()) => {
                self.last_saved = Some(settings.sanitized());
                self.dirty = false;
            }
            Err(error) => warn!("跳跃设置保存失败：{error}"),
        }
    }

    /// 跟磁盘上的值相比是否脏
    fn is_dirty_against(&self, settings: &JumpSettings) -> bool {
        match &self.last_saved {
            Some(last) => *last != settings.sanitized(),
            None => true,
        }
    }
}

/// 移动设置文件路径
fn movement_settings_path() -> Option<PathBuf> {
    Some(settings_dir()?.join(MOVEMENT_FILE))
}

/// 相机设置文件路径
fn camera_settings_path() -> Option<PathBuf> {
    Some(settings_dir()?.join(CAMERA_FILE))
}

/// 光照设置文件路径
fn lighting_settings_path() -> Option<PathBuf> {
    Some(settings_dir()?.join(LIGHTING_FILE))
}

/// 跳跃设置文件路径
fn jump_settings_path() -> Option<PathBuf> {
    Some(settings_dir()?.join(JUMP_FILE))
}

/// 读一份 JSON 设置；文件不存在返回 `Ok(None)`
fn read_settings_file<T>(path: &Path) -> Result<Option<T>, String>
where
    T: serde::de::DeserializeOwned,
{
    let content = match std::fs::read_to_string(path) {
        Ok(content) => content,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(format!("{} 读取失败：{error}", path.display())),
    };
    serde_json::from_str::<T>(&content)
        .map(Some)
        .map_err(|error| format!("{} 解析失败：{error}", path.display()))
}

/// 写一份 JSON 设置，顺带建目录
fn write_settings_file<T>(path: &Path, value: &T, what: &str) -> Result<(), String>
where
    T: serde::Serialize,
{
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|error| format!("{} 目录创建失败：{error}", parent.display()))?;
    }
    let json = serde_json::to_string_pretty(value)
        .map_err(|error| format!("{what}序列化失败：{error}"))?;
    std::fs::write(path, json).map_err(|error| format!("{} 写入失败：{error}", path.display()))
}

/// 从磁盘读移动设置；读到的值会先夹到合法区间
fn read_movement_settings(path: &Path) -> Result<Option<MovementSettings>, String> {
    Ok(read_settings_file::<MovementSettings>(path)?.map(MovementSettings::sanitized))
}

/// 把移动设置写到磁盘；写入前再夹一次区间
fn write_movement_settings(path: &Path, settings: &MovementSettings) -> Result<(), String> {
    write_settings_file(path, &settings.clone().sanitized(), "移动设置")
}

/// 从磁盘读相机设置；读到的值会先夹到合法区间
fn read_camera_settings(path: &Path) -> Result<Option<CameraFollowSettings>, String> {
    Ok(read_settings_file::<CameraFollowSettings>(path)?.map(CameraFollowSettings::sanitized))
}

/// 把相机设置写到磁盘；写入前再夹一次区间
fn write_camera_settings(path: &Path, settings: &CameraFollowSettings) -> Result<(), String> {
    write_settings_file(path, &settings.sanitized(), "相机设置")
}

/// 从磁盘读光照设置；读到的值会先夹到合法区间
fn read_lighting_settings(path: &Path) -> Result<Option<LightingSettings>, String> {
    Ok(read_settings_file::<LightingSettings>(path)?.map(LightingSettings::sanitized))
}

/// 把光照设置写到磁盘；写入前再夹一次区间
fn write_lighting_settings(path: &Path, settings: &LightingSettings) -> Result<(), String> {
    write_settings_file(path, &settings.sanitized(), "光照设置")
}

/// 从磁盘读跳跃设置；读到的值会先夹到合法区间
fn read_jump_settings(path: &Path) -> Result<Option<JumpSettings>, String> {
    Ok(read_settings_file::<JumpSettings>(path)?.map(JumpSettings::sanitized))
}

/// 把跳跃设置写到磁盘；写入前再夹一次区间
fn write_jump_settings(path: &Path, settings: &JumpSettings) -> Result<(), String> {
    write_settings_file(path, &settings.sanitized(), "跳跃设置")
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
            // 磁盘上的旋钮会**盖掉**代码里的默认值：这层改过很多次默认
            // （90→124、1.0→0.4），旧档里存的是老数，于是「改了代码没生效」。
            // 每次启动把实际口径和路径打出来，省得再猜
            let default = MovementSettings::default();
            if (loaded.turn_brake_start_deg - default.turn_brake_start_deg).abs() > 1e-3
                || (loaded.turn_brake_min_scale - default.turn_brake_min_scale).abs() > 1e-3
            {
                let path = path
                    .0
                    .as_deref()
                    .map_or("<无>".into(), |p| p.display().to_string());
                info!(
                    "移动设置用的是存档值：转身降速 起点 {:.0}° / 下限 {:.2}×（{path}），\
                     代码默认 {:.0}° / {:.2}×",
                    loaded.turn_brake_start_deg,
                    loaded.turn_brake_min_scale,
                    default.turn_brake_start_deg,
                    default.turn_brake_min_scale
                );
            }
            *settings = loaded;
        }
        None => info!("没有移动设置文件，使用默认值"),
    }
    // 记为基线，免得启动那一帧又被当成改动重写一遍
    pending.last_saved = Some(settings.clone().sanitized());
}

/// 启动时把磁盘上的相机设置灌进资源
fn load_camera_settings(
    mut settings: ResMut<CameraFollowSettings>,
    path: Res<CameraSettingsPath>,
    mut pending: ResMut<PendingCameraSave>,
) {
    let loaded = match path.0.as_deref() {
        Some(path) => match read_camera_settings(path) {
            Ok(loaded) => loaded,
            Err(error) => {
                warn!("相机设置读取失败，使用默认值：{error}");
                None
            }
        },
        None => None,
    };
    match loaded {
        Some(loaded) => {
            info!("相机设置已从磁盘载入：{loaded:?}");
            *settings = loaded;
        }
        None => info!("没有相机设置文件，使用默认值"),
    }
    pending.last_saved = Some(settings.sanitized());
}

/// 启动时把磁盘上的光照设置灌进资源
fn load_lighting_settings(
    mut settings: ResMut<LightingSettings>,
    path: Res<LightingSettingsPath>,
    mut pending: ResMut<PendingLightingSave>,
) {
    let loaded = match path.0.as_deref() {
        Some(path) => match read_lighting_settings(path) {
            Ok(loaded) => loaded,
            Err(error) => {
                warn!("光照设置读取失败，使用默认值：{error}");
                None
            }
        },
        None => None,
    };
    match loaded {
        Some(loaded) => {
            info!("光照设置已从磁盘载入：{loaded:?}");
            *settings = loaded;
        }
        None => info!("没有光照设置文件，使用默认值"),
    }
    pending.last_saved = Some(settings.sanitized());
}

/// 启动时把磁盘上的跳跃设置灌进资源
fn load_jump_settings(
    mut settings: ResMut<JumpSettings>,
    path: Res<JumpSettingsPath>,
    mut pending: ResMut<PendingJumpSave>,
) {
    let loaded = match path.0.as_deref() {
        Some(path) => match read_jump_settings(path) {
            Ok(loaded) => loaded,
            Err(error) => {
                warn!("跳跃设置读取失败，使用默认值：{error}");
                None
            }
        },
        None => None,
    };
    match loaded {
        Some(loaded) => {
            info!("跳跃设置已从磁盘载入：{loaded:?}");
            *settings = loaded;
        }
        None => info!("没有跳跃设置文件，使用默认值"),
    }
    pending.last_saved = Some(settings.sanitized());
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

/// 相机设置落盘：换策略立刻写，拖滑杆等停手
fn save_camera_settings_debounced(
    settings: Res<CameraFollowSettings>,
    path: Res<CameraSettingsPath>,
    time: Res<Time>,
    mut pending: ResMut<PendingCameraSave>,
) {
    if settings.is_changed() {
        let discrete_changed = pending
            .last_saved
            .as_ref()
            .is_none_or(|last| last.discrete_changed(&settings));
        if discrete_changed {
            pending.write_now(&settings, &path);
            return;
        }
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

/// 光照设置落盘：只有滑杆一项，改动一律走防抖，停手 SAVE_DELAY 秒才写
fn save_lighting_settings_debounced(
    settings: Res<LightingSettings>,
    path: Res<LightingSettingsPath>,
    time: Res<Time>,
    mut pending: ResMut<PendingLightingSave>,
) {
    if settings.is_changed() {
        // 启动读盘写进来的值等于基线，不该再触发一次落盘
        if pending.is_dirty_against(&settings) {
            pending.dirty = true;
            pending.timer.reset();
        }
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

/// 跳跃设置落盘：三项全是滑杆，改动一律走防抖，停手 SAVE_DELAY 秒才写
fn save_jump_settings_debounced(
    settings: Res<JumpSettings>,
    path: Res<JumpSettingsPath>,
    time: Res<Time>,
    mut pending: ResMut<PendingJumpSave>,
) {
    if settings.is_changed() {
        // 启动读盘写进来的值等于基线，不该再触发一次落盘
        if pending.is_dirty_against(&settings) {
            pending.dirty = true;
            pending.timer.reset();
        }
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

/// 离开游戏状态时补写相机设置
fn flush_camera_settings(
    settings: Res<CameraFollowSettings>,
    path: Res<CameraSettingsPath>,
    mut pending: ResMut<PendingCameraSave>,
) {
    if pending.dirty || pending.is_dirty_against(&settings) {
        pending.write_now(&settings, &path);
    }
}

/// 关窗口退出前补写相机设置
fn flush_camera_settings_on_exit(
    mut exits: MessageReader<AppExit>,
    settings: Res<CameraFollowSettings>,
    path: Res<CameraSettingsPath>,
    mut pending: ResMut<PendingCameraSave>,
) {
    if exits.read().next().is_none() {
        return;
    }
    if pending.dirty || pending.is_dirty_against(&settings) {
        pending.write_now(&settings, &path);
    }
}

/// 离开游戏状态时补写光照设置
fn flush_lighting_settings(
    settings: Res<LightingSettings>,
    path: Res<LightingSettingsPath>,
    mut pending: ResMut<PendingLightingSave>,
) {
    if pending.dirty || pending.is_dirty_against(&settings) {
        pending.write_now(&settings, &path);
    }
}

/// 关窗口退出前补写光照设置
fn flush_lighting_settings_on_exit(
    mut exits: MessageReader<AppExit>,
    settings: Res<LightingSettings>,
    path: Res<LightingSettingsPath>,
    mut pending: ResMut<PendingLightingSave>,
) {
    if exits.read().next().is_none() {
        return;
    }
    if pending.dirty || pending.is_dirty_against(&settings) {
        pending.write_now(&settings, &path);
    }
}

/// 离开游戏状态时补写跳跃设置
fn flush_jump_settings(
    settings: Res<JumpSettings>,
    path: Res<JumpSettingsPath>,
    mut pending: ResMut<PendingJumpSave>,
) {
    if pending.dirty || pending.is_dirty_against(&settings) {
        pending.write_now(&settings, &path);
    }
}

/// 关窗口退出前补写跳跃设置
fn flush_jump_settings_on_exit(
    mut exits: MessageReader<AppExit>,
    settings: Res<JumpSettings>,
    path: Res<JumpSettingsPath>,
    mut pending: ResMut<PendingJumpSave>,
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
            ..MovementSettings::default()
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
        // 缺的字段走默认值（默认开关会随游戏调参变，所以对着 Default 断言）
        assert_eq!(
            loaded.smoothing_enabled,
            MovementSettings::default().smoothing_enabled
        );

        std::fs::remove_dir_all(path.parent().unwrap()).ok();
    }

    // ---- 系统层面：开关必须立刻落盘，滑杆要防抖 ----

    /// 开关切完不等防抖就要在磁盘上能读到（这正是「切完就退出」丢配置的根因）
    #[test]
    fn toggle_is_persisted_immediately() {
        let path = temp_path("toggle/movement.json");
        let mut app = settings_app(&path);
        app.update();

        // 模拟点一下开关：拨到与默认相反的那一侧，否则「切了但值没变」什么都不会落盘
        let toggled = !MovementSettings::default().smoothing_enabled;
        app.world_mut()
            .resource_mut::<MovementSettings>()
            .smoothing_enabled = toggled;
        app.update();

        let saved = read_movement_settings(&path).unwrap().unwrap();
        assert_eq!(saved.smoothing_enabled, toggled, "开关改动必须立刻落盘");

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

    // ---- 相机设置：同一套落盘策略，只是文件与资源不同 ----

    /// 一份「只持久化相机设置」的最小 App
    fn camera_settings_app(path: &Path) -> App {
        let mut app = App::new();
        app.insert_resource(Time::<()>::default())
            .insert_resource(CameraFollowSettings::default())
            .init_resource::<PendingCameraSave>()
            .insert_resource(CameraSettingsPath(Some(path.to_path_buf())))
            .add_systems(Startup, load_camera_settings)
            .add_systems(Update, save_camera_settings_debounced);
        app
    }

    /// 换策略是离散改动：不等防抖就该在磁盘上能读到，顺带把同帧的滑杆值也写下
    #[test]
    fn camera_strategy_change_is_persisted_immediately() {
        use crate::camera::CameraFollowStrategy;

        let path = temp_path("camera/strategy.json");
        let mut app = camera_settings_app(&path);
        app.update();

        let mut settings = *app.world().resource::<CameraFollowSettings>();
        settings.strategy = CameraFollowStrategy::Rigid;
        settings.spring.smooth_time = 0.42;
        *app.world_mut().resource_mut::<CameraFollowSettings>() = settings;
        app.update();

        let saved = read_camera_settings(&path).unwrap().unwrap();
        assert_eq!(
            saved.strategy,
            CameraFollowStrategy::Rigid,
            "策略改动必须立刻落盘"
        );
        assert!((saved.spring.smooth_time - 0.42).abs() < 1e-6);

        std::fs::remove_dir_all(path.parent().unwrap()).ok();
    }

    /// 相机设置读盘也要夹区间：手改坏的 JSON 不能把弹簧参数带成负数
    #[test]
    fn camera_settings_are_clamped_on_load() {
        use crate::camera::{
            CameraFollowStrategy, MAX_SPEED_RANGE, SMOOTH_TIME_RANGE, SNAP_DISTANCE_RANGE,
        };

        let path = temp_path("camera/clamp/camera.json");
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).unwrap();
        }
        std::fs::write(
            &path,
            r#"{ "strategy": "rigid", "spring": { "smooth_time": 3.5, "max_speed": -1.0 }, "snap_distance": 0.0 }"#,
        )
        .unwrap();

        let loaded = read_camera_settings(&path).unwrap().unwrap();
        assert_eq!(loaded.strategy, CameraFollowStrategy::Rigid);
        assert_eq!(loaded.spring.smooth_time, SMOOTH_TIME_RANGE.1);
        assert_eq!(loaded.spring.max_speed, MAX_SPEED_RANGE.0);
        assert_eq!(loaded.snap_distance, SNAP_DISTANCE_RANGE.0);

        std::fs::remove_dir_all(path.parent().unwrap()).unwrap();
    }

    // ---- 光照设置：只有滑杆，全部走防抖 ----

    /// 一份「只持久化光照设置」的最小 App
    fn lighting_settings_app(path: &Path) -> App {
        let mut app = App::new();
        app.insert_resource(Time::<()>::default())
            .insert_resource(LightingSettings::default())
            .init_resource::<PendingLightingSave>()
            .insert_resource(LightingSettingsPath(Some(path.to_path_buf())))
            .add_systems(Startup, load_lighting_settings)
            .add_systems(Update, save_lighting_settings_debounced);
        app
    }

    /// 写进去再读出来还是同一个值
    #[test]
    fn lighting_round_trips() {
        let path = temp_path("lighting/round_trip/lighting.json");
        let settings = LightingSettings {
            global_illuminance: 640.0,
        };
        write_lighting_settings(&path, &settings).unwrap();

        let loaded = read_lighting_settings(&path).unwrap().unwrap();
        assert!((loaded.global_illuminance - 640.0).abs() < f32::EPSILON);

        std::fs::remove_dir_all(path.parent().unwrap()).ok();
    }

    /// 手改坏的 JSON 不能把亮度带出区间
    #[test]
    fn lighting_is_clamped_on_load() {
        let path = temp_path("lighting/clamp/lighting.json");
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).unwrap();
        }
        std::fs::write(&path, r#"{ "global_illuminance": -40.0 }"#).unwrap();
        assert_eq!(
            read_lighting_settings(&path)
                .unwrap()
                .unwrap()
                .global_illuminance,
            crate::lighting::GLOBAL_ILLUMINANCE_RANGE.0
        );

        std::fs::write(&path, r#"{ "global_illuminance": 9000.0 }"#).unwrap();
        assert_eq!(
            read_lighting_settings(&path)
                .unwrap()
                .unwrap()
                .global_illuminance,
            crate::lighting::GLOBAL_ILLUMINANCE_RANGE.1
        );

        std::fs::remove_dir_all(path.parent().unwrap()).ok();
    }

    /// 拖滑杆时先不写，停手超过 SAVE_DELAY 才写
    #[test]
    fn lighting_change_is_debounced() {
        let path = temp_path("lighting/debounce/lighting.json");
        write_lighting_settings(&path, &LightingSettings::default()).unwrap();
        let mut app = lighting_settings_app(&path);
        app.update();
        let before = std::fs::read_to_string(&path).unwrap();

        app.world_mut()
            .resource_mut::<LightingSettings>()
            .global_illuminance = 880.0;
        app.update();
        assert_eq!(std::fs::read_to_string(&path).unwrap(), before);

        advance(&mut app, SAVE_DELAY + 0.05);
        app.update();
        let saved = read_lighting_settings(&path).unwrap().unwrap();
        assert!((saved.global_illuminance - 880.0).abs() < 1e-6);

        std::fs::remove_dir_all(path.parent().unwrap()).ok();
    }

    /// 退出兜底：脏了就要补写，不依赖防抖计时器
    #[test]
    fn lighting_flush_writes_pending_change() {
        let path = temp_path("lighting/flush/lighting.json");
        let mut app = lighting_settings_app(&path);
        app.add_systems(Update, flush_lighting_settings);
        app.update();

        app.world_mut()
            .resource_mut::<LightingSettings>()
            .global_illuminance = 1250.0;
        app.update();

        let world = app.world_mut();
        world.resource_mut::<PendingLightingSave>().dirty = true;
        let settings = *world.resource::<LightingSettings>();
        let path_res = world.resource::<LightingSettingsPath>().clone();
        world
            .resource_mut::<PendingLightingSave>()
            .write_now(&settings, &path_res);

        let saved = read_lighting_settings(&path).unwrap().unwrap();
        assert!((saved.global_illuminance - 1250.0).abs() < 1e-6);

        std::fs::remove_dir_all(path.parent().unwrap()).ok();
    }
}
