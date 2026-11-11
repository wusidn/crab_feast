//! 转身剪辑层：大幅转向时用 `LeftTurn* / RightTurn*` 接管腿和身体旋转。
//!
//! 为什么需要单独一层：移动混合空间只描述「相对朝向 × 速度」，没有「原地转身」
//! 这一档；转身期硬用反放的走/跑，脚会滑、腿会交叉。这里把转身剪辑按权重叠到
//! 移动姿态上，**身体朝向也改由剪辑里的 hips 偏航推出**——否则 hips 烘进去的
//! 90°/176° 会和 [`crate::input`] 里的限速积分叠加成双倍旋转。
//!
//! Amy 实测：`LeftTurn90` 0.933s / 净 +90°，`LeftTurn` 1.633s / 净 +176°。
//! 所以 90° 以内用 90° 版，更大的角用 180° 版并提前收手（按剩余误差退出）。
//!
//! 权重是叠在移动姿态上的：`weight=1` 时腿部完全归转身剪辑，地速也压到 0
//! （原地转身），淡出后交还移动混合。

use bevy::prelude::*;

use super::AnimationSet;
use super::core::ClipSet;
use super::driver::AnimFocus;
use crate::input::{FacingState, LocomotionIntent};

/// 误差超过它才让剪辑接管（低于这个角用侧移剪辑已经够自然）
const ENTER_DEG: f32 = 50.0;
/// 误差小于它就开始交还，交还后剩下的零头由移动混合收尾
const EXIT_DEG: f32 = 12.0;
/// 超过这个角用 180° 剪辑，否则用 90° 剪辑
const BIG_TURN_DEG: f32 = 95.0;
/// 权重淡入淡出时长（秒）
const FADE_SECS: f32 = 0.12;
/// 剪辑的播放倍速。1.0 = 原速（180° 要 1.63s，偏慢）；1.6 时 180° 约 1.0s。
const PLAYBACK: f32 = 1.6;

/// 转身剪辑层的跨帧状态。
#[derive(Resource, Default)]
pub struct TurnLayer {
    clip: Option<String>,
    /// 转身方向：+1 左转（剪辑名 Left*），-1 右转
    dir: f32,
    /// 剪辑播放进度 0..=1
    phase: f32,
    /// 叠加到移动姿态上的权重
    weight: f32,
    /// 起手那一帧的身体 yaw（弧度）
    start_yaw: f32,
    /// 剪辑第 0 帧 hips 的「绕 Y 扭转分量」（四元数），第一次采样时记住。
    /// 存四元数而不是欧拉角：转身恰好把偏航推到 ±90°/180°，
    /// 那正是欧拉分解的奇异点，用角度做会被万向节锁搅成一团。
    base_twist: Option<Quat>,
    /// 上一帧的转角（弧度，连续不回绕），用来给本帧转角解缠绕
    last_angle: Option<f32>,
}

impl TurnLayer {
    pub fn clip(&self) -> Option<&str> {
        self.clip.as_deref()
    }

    pub fn weight(&self) -> f32 {
        self.weight
    }

    pub fn sample_time(&self, duration: f32) -> f32 {
        self.phase * duration
    }

    pub fn start_yaw(&self) -> f32 {
        self.start_yaw
    }

    pub fn base_twist(&self) -> Option<Quat> {
        self.base_twist
    }

    pub fn set_base_twist(&mut self, twist: Quat) {
        self.base_twist = Some(twist);
    }

    /// 把本帧的转角接到上一帧上：转角超过 ±180° 时它会回绕，
    /// 180° 版剪辑（净 176°，叠加基准偏移后过 180）正好踩在这个坑上。
    pub fn unwrap_angle(&mut self, raw: f32) -> f32 {
        let angle = match self.last_angle {
            None => raw,
            Some(last) => {
                let mut angle = raw;
                while angle - last > std::f32::consts::PI {
                    angle -= std::f32::consts::TAU;
                }
                while angle - last < -std::f32::consts::PI {
                    angle += std::f32::consts::TAU;
                }
                angle
            }
        };
        self.last_angle = Some(angle);
        angle
    }

    fn start(&mut self, clip: &str, dir: f32, yaw: f32) {
        self.clip = Some(clip.to_string());
        self.dir = dir;
        self.phase = 0.0;
        self.weight = 0.0;
        self.start_yaw = yaw;
        self.base_twist = None;
        self.last_angle = None;
    }

    fn reset(&mut self) {
        *self = Self::default();
    }
}

pub struct TurnPlugin;

impl Plugin for TurnPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<TurnLayer>().add_systems(
            Update,
            update_turn
                .in_set(AnimationSet::Drive)
                .run_if(in_state(crate::GameState::Game)),
        );
    }
}

/// 请求 / 推进 / 收尾：只维护状态，不碰骨骼（采样和叠加在 [`super::driver`]）。
fn update_turn(
    time: Res<Time>,
    focus: Res<AnimFocus>,
    clips: Res<ClipSet>,
    clip_assets: Res<Assets<AnimationClip>>,
    mut layer: ResMut<TurnLayer>,
    mut avatars: Query<(&mut FacingState, &LocomotionIntent)>,
) {
    let dt = time.delta_secs();
    let Some((mut state, intent)) = avatars.iter_mut().next() else {
        layer.reset();
        return;
    };

    // 编辑器/单条预览时姿态归别人，这一层完全让位
    if focus.mixer.is_some() || focus.preview {
        layer.reset();
        state.turn_locked = false;
        return;
    }

    let error = state.error_deg;
    let moving = intent.magnitude > 0.05;

    if layer.clip.is_some() {
        if let Some(clip) = layer.clip.clone() {
            let duration = clips.meta(&clip_assets, &clip).duration;
            if duration > 0.0 {
                layer.phase = (layer.phase + dt * PLAYBACK / duration).min(1.0);
            }
        }
        // 方向反了、停手了、转够了、剪辑播完了，都开始淡出
        let reversed = error.signum() * layer.dir < 0.0;
        let done = layer.phase >= 1.0 || !moving || error.abs() <= EXIT_DEG || reversed;
        let target = if done { 0.0 } else { 1.0 };
        layer.weight = approach(layer.weight, target, dt / FADE_SECS);
        if layer.weight <= 0.0 {
            layer.reset();
        }
    } else if moving && error.abs() >= ENTER_DEG {
        if let Some(clip) = choose_clip(error, &clips, &clip_assets) {
            layer.start(clip, error.signum(), state.yaw);
            layer.weight = 0.0;
        }
    }

    // 一有剪辑就锁朝向，不等权重起来：否则起手第一帧输入系统还会再转一点，
    // 下一帧被剪辑的起点顶回去，看起来像回抽一下
    state.turn_locked = layer.clip.is_some();
}

/// 按误差挑剪辑：方向看符号，幅度看大小；当前角色没有对应剪辑时退回另一档。
fn choose_clip<'a>(
    error_deg: f32,
    clips: &'a ClipSet,
    assets: &Assets<AnimationClip>,
) -> Option<&'a str> {
    let left = error_deg > 0.0;
    let big = error_deg.abs() >= BIG_TURN_DEG;
    let (primary, fallback) = match (left, big) {
        (true, true) => ("LeftTurn", "LeftTurn90"),
        (true, false) => ("LeftTurn90", "LeftTurn"),
        (false, true) => ("RightTurn", "RightTurn90"),
        (false, false) => ("RightTurn90", "RightTurn"),
    };
    if clips.handle(primary).and_then(|h| assets.get(h)).is_some() {
        Some(primary)
    } else if clips.handle(fallback).and_then(|h| assets.get(h)).is_some() {
        Some(fallback)
    } else {
        None
    }
}

fn approach(current: f32, target: f32, max_delta: f32) -> f32 {
    let delta = target - current;
    if delta.abs() <= max_delta {
        target
    } else {
        current + delta.signum() * max_delta
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn approach_never_overshoots() {
        assert_eq!(approach(0.0, 1.0, 0.3), 0.3);
        assert_eq!(approach(0.9, 1.0, 0.3), 1.0);
        assert_eq!(approach(0.0, 0.0, 0.3), 0.0);
    }

    #[test]
    fn big_errors_use_the_180_clip() {
        // 直接对着阈值验证选择规则本身（不碰剪辑资源）
        assert!(BIG_TURN_DEG > ENTER_DEG && BIG_TURN_DEG < 180.0);
        assert!(EXIT_DEG < ENTER_DEG);
    }

    #[test]
    fn angle_stays_continuous_past_180() {
        // 180° 版剪辑：踝 3°/帧地推进，到 182° 时 atan2 会回绕成 -178，必须解回连续
        let mut layer = TurnLayer::default();
        let mut last = 0.0;
        for deg in (0..=182).step_by(3) {
            let raw = ((deg as f32 + 180.0) % 360.0 - 180.0).to_radians();
            last = layer.unwrap_angle(raw);
            assert!((last.to_degrees() - deg as f32).abs() < 0.5, "deg={deg} got {last}");
        }
    }
}
