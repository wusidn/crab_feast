//! 会话流程：大厅 → 登录 → 建世界 → 就绪。
//!
//! 这里只管**状态**，不碰网络细节（连接、握手超时在 `net` 模块里）：
//! - 进 `Lobby` 时消费命令行带来的自动登录；
//! - 本地登录：等一个很短的过渡后进 `Game`；
//! - 进 `Game` 后等「角色 + 模型 + 动画图」到齐，把 [`SessionReady`] 置真，
//!   输入 UI 与键盘绑定以此为开关；
//! - 退出 `Game` 时把世界里的临时实体与动画缓存清掉，允许回到大厅再来一局。

use bevy::prelude::*;

use crate::GameState;
use crate::anim::{AnimRuntime, PoseDriver};
use crate::input::MovementInput;
use crate::scene::Avatar;
use crate::state::{AbortSession, AutoJoin, LobbyNotice, SessionMode, SessionReady};

/// 本地登录的过渡时长：让「正在登录」界面至少露一次脸，不至于闪一下就进场景。
const LOCAL_LOGIN_SECS: f32 = 0.4;

/// 世界就绪的兜底时间：角色资产缺失/加载失败时不能让玩家永远卡在遮罩后面。
const READY_TIMEOUT_SECS: f32 = 8.0;

pub struct SessionPlugin;

#[derive(Resource, Default)]
struct LoginStage {
    timer: Option<Timer>,
}

/// 进 `Game` 后的计时，用于就绪兜底。
#[derive(Resource, Default)]
struct WorldClock(f32);

impl Plugin for SessionPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<SessionMode>()
            .init_resource::<SessionReady>()
            .init_resource::<AutoJoin>()
            .init_resource::<LobbyNotice>()
            .init_resource::<LoginStage>()
            .init_resource::<WorldClock>()
            .add_message::<AbortSession>()
            .add_systems(OnEnter(GameState::Lobby), consume_auto_join)
            .add_systems(OnEnter(GameState::Login), begin_login)
            .add_systems(
                Update,
                (tick_local_login, connect_timeout_notice)
                    .run_if(in_state(GameState::Login)),
            )
            .add_systems(OnEnter(GameState::Game), reset_world_clock)
            .add_systems(
                Update,
                (mark_world_ready, note_ready_timeout)
                    .run_if(in_state(GameState::Game)),
            )
            .add_systems(OnExit(GameState::Game), cleanup_world);
    }
}

/// 大厅：命令行给了目标就直接开始登录，否则停着等玩家点。
fn consume_auto_join(
    mut auto: ResMut<AutoJoin>,
    mut mode: ResMut<SessionMode>,
    mut next: ResMut<NextState<GameState>>,
) {
    if let Some(target) = auto.0.take() {
        info!("auto-join：直接登录 {target:?}");
        *mode = target;
        next.set(GameState::Login);
    }
}

fn begin_login(mode: Res<SessionMode>, mut stage: ResMut<LoginStage>) {
    let secs = match *mode {
        SessionMode::Local => LOCAL_LOGIN_SECS,
        // 联机由 `net` 的握手超时负责，这里只放一个宽松的兜底
        SessionMode::Online { .. } => 10.0,
    };
    stage.timer = Some(Timer::from_seconds(secs, TimerMode::Once));
}

fn tick_local_login(
    time: Res<Time>,
    mode: Res<SessionMode>,
    mut stage: ResMut<LoginStage>,
    mut next: ResMut<NextState<GameState>>,
) {
    if *mode != SessionMode::Local {
        return;
    }
    let Some(timer) = stage.timer.as_mut() else {
        return;
    };
    if timer.tick(time.delta()).just_finished() {
        info!("本地登录完成 → 进入场景");
        next.set(GameState::Game);
    }
}

/// 联机登录超时还没进 `Game`：给一句提示回大厅。
///
/// 连接实体本身由 `net` 在退出 `Login` 时回收，这里只负责状态与提示。
fn connect_timeout_notice(
    time: Res<Time>,
    mode: Res<SessionMode>,
    mut stage: ResMut<LoginStage>,
    mut notice: ResMut<LobbyNotice>,
    mut abort: MessageWriter<AbortSession>,
    mut next: ResMut<NextState<GameState>>,
) {
    if matches!(*mode, SessionMode::Local) {
        return;
    }
    let Some(timer) = stage.timer.as_mut() else {
        return;
    };
    if timer.tick(time.delta()).just_finished() {
        notice.0 = Some("连接服务器超时，请重试".to_string());
        abort.write(AbortSession);
        next.set(GameState::Lobby);
    }
}

fn reset_world_clock(mut clock: ResMut<WorldClock>, mut ready: ResMut<SessionReady>) {
    clock.0 = 0.0;
    ready.0 = false;
}

fn note_ready_timeout(time: Res<Time>, mut clock: ResMut<WorldClock>, ready: Res<SessionReady>) {
    if !ready.0 {
        clock.0 += time.delta_secs();
    }
}

/// 世界就绪：本地角色已在场、模型与动画图都建好。
///
/// 兜底：超过 [`READY_TIMEOUT_SECS`] 还没齐（例如角色资产缺失），也让玩家能操作，
/// 否则大厅进得来、游戏进不去，比缺模型更难排查。
fn mark_world_ready(
    runtime: Res<AnimRuntime>,
    avatars: Query<Entity, With<Avatar>>,
    clock: Res<WorldClock>,
    mut ready: ResMut<SessionReady>,
) {
    if ready.0 {
        return;
    }
    let has_avatar = avatars.iter().next().is_some();
    if (has_avatar && runtime.is_ready()) || clock.0 >= READY_TIMEOUT_SECS {
        info!(
            ready = true,
            ?has_avatar,
            model_ready = runtime.is_ready(),
            elapsed = clock.0,
            "世界就绪，开启输入 UI"
        );
        ready.0 = true;
    }
}

/// 退出 `Game`：清理本局临时实体与动画缓存。
///
/// 只清「本局生成的东西」：关卡、灯光、本地角色、复制下来的玩家。
/// 相机与输入资源留着，回大厅后重新进场景时由各自插件重建。
fn cleanup_world(
    mut commands: Commands,
    level: Query<Entity, With<crate::level::LevelBlock>>,
    avatars: Query<Entity, With<Avatar>>,
    decor: Query<Entity, With<crate::scene::SceneDecor>>,
    mut runtime: ResMut<AnimRuntime>,
    mut driver: ResMut<PoseDriver>,
    mut movement: ResMut<MovementInput>,
    mut ready: ResMut<SessionReady>,
) {
    for entity in level.iter().chain(avatars.iter()).chain(decor.iter()) {
        // 联机角色可能已经被复制系统收走了：同一帧两个系统都排了 despawn，
        // 用 try_despawn 静默跳过，别把「已经没了」当成错误
        commands.entity(entity).try_despawn();
    }
    runtime.reset_for_new_session();
    driver.skeleton = None;
    *movement = MovementInput::Idle;
    ready.0 = false;
}
