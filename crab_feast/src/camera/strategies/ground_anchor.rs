//! 地面锚定：焦点钉在**脚下的地面**上，而不是角色身上。
//!
//! 这是「跳跃时相机不许上下窜」的正解。参考系从「角色胶囊中心」换成
//! 「角色脚下的地面高度」：跳起、落地、蹲起都不改变锚点，画面纹丝不动；
//! 只有脚真正踩到另一个高度（爬台阶、跳上箱子、走下悬崖）锚点才变。
//!
//! 三个细节决定它好不好用：
//!
//! 1. **竖直用弹簧、水平瞬时**。水平必须刚性——移动输入的方向是按
//!    `CameraFacing::yaw` 换算的，这里一旦滞后就会「按前走却侧移」。
//! 2. **死区锁在「地面高度」上，不锁在「误差」上**。锚点是一段一段跳的，
//!    弹簧追的是个不会动的东西，所以永远收敛、不会在缓坡上来回弹。
//!    死区的作用是忽略小于 `dead_zone` 的台阶与地面网格噪声。
//! 3. **探测不到地面时不归零**。跳过坑时保持上一个地面高度，
//!    跌落深坑时才跟着脚底下降。

use avian3d::prelude::{SpatialQuery, SpatialQueryFilter};
use bevy::prelude::*;
use serde::{Deserialize, Serialize};

use crate::camera::{
    CameraFollowSettings, CameraFollowState, CameraFollowStrategy, CameraSet, FollowInput,
    strategy_is,
};
use crate::ground::{CAPSULE_HALF_HEIGHT, PROBE_RISE, ground_height};

pub struct GroundAnchorPlugin;

impl Plugin for GroundAnchorPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<AnchorMemory>().add_systems(
            Update,
            ground_anchor_follow
                .in_set(CameraSet::Follow)
                .run_if(strategy_is(CameraFollowStrategy::GroundAnchor)),
        );
    }
}

/// 地面锚定的参数
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct AnchorParams {
    /// 竖直方向的特征时间（秒）。水平永远瞬时，这个只影响上下坡 / 台阶。
    pub smooth_time: f32,
    /// 死区（米）：地面高度变化小于它就不动画面。台阶与缓坡全靠这个「不反应」。
    pub dead_zone: f32,
    /// 竖直方向最大速度（米/秒）。`0` = 不限。
    pub max_speed: f32,
}

impl Default for AnchorParams {
    fn default() -> Self {
        Self {
            // 0.35s：上台阶是「镜头缓缓抬起来」，不是硬切
            smooth_time: 0.35,
            // 0.15m：小于它的地面起伏（缓坡、网格噪声）完全不反应
            dead_zone: 0.15,
            max_speed: 20.0,
        }
    }
}

/// 地面锚定参数的合法区间（UI 滑杆与读盘夹取共用）
pub const ANCHOR_TIME_RANGE: (f32, f32) = (0.03, 1.0);
pub const DEAD_ZONE_RANGE: (f32, f32) = (0.0, 1.0);
pub const ANCHOR_SPEED_RANGE: (f32, f32) = (0.0, 40.0);

/// 锚定策略自己的记忆：地面探测的历史，以及当前锁存的锚点高度。
///
/// 这个资源是策略私有的，不进 [`crate::camera::CameraFollowState`]：
/// 别的策略用不到「上一块地面」，留在共享状态里只会变成看不懂的字段。
#[derive(Resource, Debug, Default)]
struct AnchorMemory {
    /// 上一次成功探到的地面高度；探测 miss 时用它兜底
    last_ground: Option<f32>,
    /// 锁存的锚点高度。地面高度变化超过死区才会被重新赋值。
    anchor: Option<f32>,
}

fn ground_anchor_follow(
    input: Res<FollowInput>,
    settings: Res<CameraFollowSettings>,
    spatial: SpatialQuery,
    mut follow: ResMut<CameraFollowState>,
    mut memory: ResMut<AnchorMemory>,
    time: Res<Time>,
) {
    if !input.valid {
        return;
    }

    let avatar = input.translation;
    let foot_y = avatar.y - CAPSULE_HALF_HEIGHT;

    // 从胶囊中段往下打一条射线；排除角色自己的碰撞体，否则永远打到自己
    let filter = SpatialQueryFilter::from_excluded_entities([input.avatar]);
    let hit = ground_height(&spatial, &filter, avatar + Vec3::Y * PROBE_RISE);
    let ground = anchor_ground(hit, memory.last_ground, foot_y);
    memory.last_ground = Some(ground);

    // 死区：地面高度变化不超过它就仍用旧锚点，台阶与缓坡因此不会推动画面
    let anchor = match memory.anchor {
        Some(anchor) if (ground - anchor).abs() <= settings.anchor.dead_zone => anchor,
        _ => ground,
    };
    memory.anchor = Some(anchor);

    // 目标 = 角色所在的水平位置 + 锚点地面抬高到焦点高度
    let target = Vec3::new(avatar.x, anchor + input.focus_height, avatar.z);

    // 第一帧 / 传送：直接钉住，别让弹簧从地图另一头拽过来
    if follow.snap_if_needed(target, settings.snap_distance) {
        return;
    }

    follow.target = target;

    // 水平瞬时：输入方向按相机 yaw 换算，滞后会让角色「按前走却侧移」
    follow.focus.x = target.x;
    follow.focus.z = target.z;
    follow.velocity.x = 0.0;
    follow.velocity.z = 0.0;

    // 竖直交给弹簧；dt = 0（暂停）时不推进，避免算爆
    let dt = time.delta_secs();
    if dt > 0.0 {
        let mut velocity_y = follow.velocity.y;
        follow.focus.y = super::spring::smooth_damp_axis(
            follow.focus.y,
            target.y,
            &mut velocity_y,
            settings.anchor.smooth_time,
            settings.anchor.max_speed,
            dt,
        );
        follow.velocity.y = velocity_y;
    }
}

/// 决定用哪个高度当「地面」（纯函数，方便单测）。
///
/// - 打到地面：就用打到的地面高度（走上箱子、跳上平台都靠这条）；
/// - 没打到，也没历史：退回脚底高度，至少别把焦点留在半空；
/// - 没打到但有历史：`min(上次地面, 脚底)` —— 跳过坑时脚底高于上次地面，
///   取上次地面（保持高度不掉）；跌落深坑时脚底低于上次地面，取脚底（跟着下降）。
fn anchor_ground(hit: Option<f32>, last: Option<f32>, foot_y: f32) -> f32 {
    match hit {
        Some(ground) => ground,
        None => last.map_or(foot_y, |last| last.min(foot_y)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 打到地面：以探到的高度为准，脚底偏移不参与
    #[test]
    fn raycast_hit_wins() {
        assert_eq!(anchor_ground(Some(2.0), Some(0.0), 1.4), 2.0);
    }

    /// 什么都没探到、也没有历史：退回脚底，不能把焦点留在原地
    #[test]
    fn miss_without_history_falls_back_to_foot() {
        assert_eq!(anchor_ground(None, None, 1.4), 1.4);
    }

    /// 跳过坑：脚底高于上次地面 → 保持上次地面高度，画面不往下掉
    #[test]
    fn jumping_over_a_pit_keeps_the_last_ground() {
        assert_eq!(anchor_ground(None, Some(0.0), 3.0), 0.0);
    }

    /// 跌落深坑：脚底低于上次地面 → 跟着脚底下降
    #[test]
    fn falling_into_a_pit_follows_the_feet() {
        assert_eq!(anchor_ground(None, Some(0.0), -4.0), -4.0);
    }

    /// 默认值都在合法区间内（否则 UI 滑杆与读盘会互相打架）
    #[test]
    fn default_params_are_inside_their_ranges() {
        let params = AnchorParams::default();
        assert!(
            params.smooth_time >= ANCHOR_TIME_RANGE.0 && params.smooth_time <= ANCHOR_TIME_RANGE.1
        );
        assert!(params.dead_zone >= DEAD_ZONE_RANGE.0 && params.dead_zone <= DEAD_ZONE_RANGE.1);
        assert!(
            params.max_speed >= ANCHOR_SPEED_RANGE.0 && params.max_speed <= ANCHOR_SPEED_RANGE.1
        );
    }

    fn anchor_settings() -> CameraFollowSettings {
        CameraFollowSettings {
            strategy: CameraFollowStrategy::GroundAnchor,
            anchor: AnchorParams {
                smooth_time: 0.3,
                dead_zone: 0.15,
                max_speed: 20.0,
            },
            ..default()
        }
        .sanitized()
    }

    /// 测试装置：不依赖物理世界，直接喂一个「地面高度」序列
    struct Rig {
        state: CameraFollowState,
        memory: AnchorMemory,
        settings: CameraFollowSettings,
        /// 角色水平位置
        at: Vec3,
        dt: f32,
    }

    impl Rig {
        fn new() -> Self {
            Self {
                state: CameraFollowState::default(),
                memory: AnchorMemory::default(),
                settings: anchor_settings(),
                at: Vec3::new(0.0, 0.7, 0.0),
                dt: 1.0 / 60.0,
            }
        }

        /// 跑一帧：`ground` 是这一帧探到的地面高度（模拟射线命中）
        fn step(&mut self, ground: Option<f32>) {
            let foot_y = self.at.y - CAPSULE_HALF_HEIGHT;
            let ground = anchor_ground(ground, self.memory.last_ground, foot_y);
            self.memory.last_ground = Some(ground);
            let anchor = match self.memory.anchor {
                Some(anchor) if (ground - anchor).abs() <= self.settings.anchor.dead_zone => anchor,
                _ => ground,
            };
            self.memory.anchor = Some(anchor);

            let target = Vec3::new(self.at.x, anchor + 1.3, self.at.z);
            if self
                .state
                .snap_if_needed(target, self.settings.snap_distance)
            {
                return;
            }
            self.state.target = target;
            self.state.focus.x = target.x;
            self.state.focus.z = target.z;

            let mut velocity_y = self.state.velocity.y;
            self.state.focus.y = super::super::spring::smooth_damp_axis(
                self.state.focus.y,
                target.y,
                &mut velocity_y,
                self.settings.anchor.smooth_time,
                self.settings.anchor.max_speed,
                self.dt,
            );
            self.state.velocity.y = velocity_y;
        }
    }

    /// 站着不动：焦点正好在「地面 + 焦点高度」
    #[test]
    fn standing_still_puts_the_focus_on_the_ground() {
        let mut rig = Rig::new();
        rig.at = Vec3::new(2.0, 0.7, 3.0);
        rig.step(Some(0.0));

        let focus = rig.state.focus();
        assert!((focus.y - 1.3).abs() < 1e-6, "焦点高度 {}", focus.y);
        assert!((focus.x - 2.0).abs() < 1e-6);
        assert!((focus.z - 3.0).abs() < 1e-6);
    }

    /// 核心诉求：角色跳起 1.2m，画面竖直方向完全不动
    #[test]
    fn jumping_does_not_move_the_view() {
        let mut rig = Rig::new();
        rig.step(Some(0.0));
        let before = rig.state.focus();

        // 起跳：角色抬高，射线仍打在下面同一块地上
        rig.at.y += 1.2;
        for _ in 0..30 {
            rig.step(Some(0.0));
            assert!(
                (rig.state.focus().y - before.y).abs() < 1e-6,
                "跳跃不该带动机位：{} vs {}",
                rig.state.focus().y,
                before.y
            );
        }
        assert!((rig.state.focus().x - before.x).abs() < 1e-6);
    }

    /// 小于死区的台阶被忽略：画面完全不动
    #[test]
    fn steps_below_the_dead_zone_are_ignored() {
        let mut rig = Rig::new();
        rig.step(Some(0.0));
        // 地面抬高 0.1m（< 0.15 死区）
        for _ in 0..30 {
            rig.step(Some(0.1));
        }
        assert!(
            (rig.state.focus().y - 1.3).abs() < 1e-6,
            "不该被 10cm 台阶推动"
        );
    }

    /// 大于死区的台阶：平滑跟上去，不硬切、不过冲
    #[test]
    fn a_real_step_is_followed_smoothly() {
        let mut rig = Rig::new();
        rig.step(Some(0.0));

        rig.step(Some(1.0)); // 跳上 1m 高的平台
        let first = rig.state.focus().y;
        assert!(first > 1.3 && first < 2.3, "一帧不该走完 1m：{first}");

        let mut previous = first;
        for _ in 0..120 {
            rig.step(Some(1.0));
            let y = rig.state.focus().y;
            assert!(y <= 2.3 + 1e-3, "过冲：{y}");
            assert!(y >= previous - 1e-5, "回弹：{previous} -> {y}");
            previous = y;
        }
        assert!(
            (previous - 2.3).abs() < 1e-2,
            "两秒后该站在平台上：{previous}"
        );
    }

    /// 爬缓坡：焦点必须单调上升，不能一停一走地抽动
    #[test]
    fn a_slope_climb_is_monotonic() {
        let mut rig = Rig::new();
        rig.step(Some(0.0));

        let mut previous = rig.state.focus().y;
        let mut moving_frames = 0;
        for frame in 1..=120 {
            rig.step(Some(frame as f32 * 0.01));
            let y = rig.state.focus().y;
            assert!(y >= previous - 1e-5, "上坡不该往下走：{previous} -> {y}");
            if y > previous + 1e-6 {
                moving_frames += 1;
            }
            previous = y;
        }
        assert!(
            moving_frames > 60,
            "缓坡上大部分帧都该在动：{moving_frames}/120"
        );
        assert!(previous > 1.3, "爬完坡焦点该抬高：{previous}");
    }

    /// 走下悬崖：探测 miss 时按脚底下降，不会停在半空
    #[test]
    fn falling_off_a_ledge_descends() {
        let mut rig = Rig::new();
        rig.step(Some(0.0));

        // 走到悬崖外，脚下没有地面
        rig.step(None);
        // 脚底已经被重力带下去 3m
        rig.at.y -= 3.0;
        let mut previous = rig.state.focus().y;
        for _ in 0..120 {
            rig.step(None);
            let y = rig.state.focus().y;
            assert!(y <= previous + 1e-5, "下降不该反弹：{previous} -> {y}");
            previous = y;
        }
        assert!(previous < 1.3, "落到低处焦点该跟着降：{previous}");
    }

    /// 水平方向永远瞬时跟随：跑动时焦点不留残影
    #[test]
    fn horizontal_never_lags() {
        let mut rig = Rig::new();
        rig.step(Some(0.0));
        for frame in 1..=30 {
            rig.at.x = frame as f32 * 0.5;
            rig.step(Some(0.0));
            assert!((rig.state.focus().x - rig.at.x).abs() < 1e-6);
        }
    }
}
