//! 跟随策略的「契约层」：策略枚举、设置、共享焦点状态、激活条件。
//!
//! 这里**不含任何一种策略的算法**——算法在 `strategies/` 各自的文件里。
//! 本文件只回答三个问题：
//!
//! 1. 现在选的是哪个策略（[`CameraFollowSettings::strategy`]）
//! 2. 策略之间传什么（[`CameraFollowState`]：一帧的焦点与速度）
//! 3. 怎么只让选中的那个跑（[`strategy_is`] + [`CameraSet::Follow`]）

use bevy::prelude::*;
use serde::{Deserialize, Serialize};

use super::strategies::{AnchorParams, SpringParams};

/// 相机一帧内的三段系统集。
///
/// 三段串行、整体挂在 `ControlSet::Camera` 上：先采输入、再算焦点、最后摆机位。
/// 策略只允许往 [`CameraSet::Follow`] 挂系统——它拿得到焦点结论，拿不到机位，
/// 于是任何策略都改不了轨道几何。
#[derive(SystemSet, Debug, Clone, PartialEq, Eq, Hash)]
pub enum CameraSet {
    /// 采集角色位置、缩放
    Gather,
    /// 当前策略算焦点
    Follow,
    /// 焦点 + 轨道偏移 = 机位
    Place,
}

/// 本帧的跟随输入：角色在哪、焦点该抬多高。
///
/// 由 `gather_follow_input` 在 [`CameraSet::Gather`] 里写一次，所有策略只读。
#[derive(Resource, Debug)]
pub struct FollowInput {
    /// 本地角色实体（地面锚定要用它排除自身碰撞体）
    pub avatar: Entity,
    /// 角色原点（胶囊中心）的世界坐标
    pub translation: Vec3,
    /// 视线落点相对锚点的高度
    pub focus_height: f32,
    /// 本帧有没有角色：没有的话所有策略与机位计算都跳过
    pub valid: bool,
}

impl Default for FollowInput {
    fn default() -> Self {
        Self {
            avatar: Entity::PLACEHOLDER,
            translation: Vec3::ZERO,
            focus_height: 1.3,
            valid: false,
        }
    }
}

impl FollowInput {
    /// 以角色本体为锚的理想焦点（刚性 / 弹簧用）
    pub fn ideal_focus(&self) -> Vec3 {
        self.translation + Vec3::Y * self.focus_height
    }
}

/// 相机跟随策略：决定「焦点」怎么追上角色。
///
/// 焦点是视线的落点，机位永远是「焦点 + 轨道偏移」，所以策略只需要管焦点这一件事。
/// 加策略 = 往这个枚举加变体 + 在 `strategies/` 加一个文件 + 注册一行。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CameraFollowStrategy {
    /// 刚性绑定：焦点每帧钉在角色身上（最初的行为，零延迟，
    /// 跳跃 / 落地冲击会整帧带进画面）
    Rigid,
    /// 临界阻尼弹簧：焦点带速度状态追角色，不过冲、速度自适应
    #[default]
    SmoothDamp,
    /// 地面锚定：焦点钉在**脚下的地面**上，跳跃时相机竖直方向不动
    GroundAnchor,
}

impl CameraFollowStrategy {
    /// 设置面板里的可选顺序（也是下拉框的下标来源）
    pub const ALL: [Self; 3] = [Self::Rigid, Self::SmoothDamp, Self::GroundAnchor];

    /// 下拉框与卡片标题用的名字
    pub fn label(self) -> &'static str {
        match self {
            Self::Rigid => "刚性绑定",
            Self::SmoothDamp => "临界阻尼弹簧",
            Self::GroundAnchor => "地面锚定",
        }
    }

    /// 设置面板里的一句说明：说清这个策略的取舍，而不是只报名字
    pub fn note(self) -> &'static str {
        match self {
            Self::Rigid => {
                "焦点每帧钉在角色身上：零延迟，但跳跃、落地冲击、物理接触噪声会整帧带进画面。"
            }
            Self::SmoothDamp => {
                "焦点带速度状态追角色：到点不过冲、速度自适应；起步与急停都不会甩镜头，代价是几十毫秒的跟随滞后。"
            }
            Self::GroundAnchor => {
                "焦点钉在脚下地面：跳跃、落地、蹲起时竖直方向完全不动，只有脚踩到的高度变了才平滑跟上；水平方向仍然瞬时跟随，转向不会发虚。"
            }
        }
    }

    /// 在 [`Self::ALL`] 里的下标（下拉框选项序号）
    pub fn index(self) -> usize {
        Self::ALL.iter().position(|s| *s == self).unwrap_or(0)
    }

    /// 从下拉框选项序号还原；越界序号退回默认策略，免得脏数据把相机搞成未知状态
    pub fn from_index(index: usize) -> Self {
        Self::ALL.get(index).copied().unwrap_or_default()
    }
}

/// 只要当前策略是 `strategy`，这个条件就为真。
///
/// 每个策略系统的 `run_if` 都用它：面板里选哪个策略，就只有那个策略的系统在跑，
/// 其余策略系统连查询都不会建立。
pub fn strategy_is(
    strategy: CameraFollowStrategy,
) -> impl Fn(Res<CameraFollowSettings>) -> bool + Clone + Send + Sync + 'static {
    move |settings: Res<CameraFollowSettings>| settings.strategy == strategy
}

/// 相机设置的合法区间（UI 滑杆与读盘夹取共用，只写一处）
pub const SMOOTH_TIME_RANGE: (f32, f32) = (0.02, 1.0);
pub const MAX_SPEED_RANGE: (f32, f32) = (0.0, 30.0);
pub const SNAP_DISTANCE_RANGE: (f32, f32) = (1.0, 50.0);

/// 相机跟随设置：策略 + 各策略自己的参数 + 所有策略共用的安全阈值。
///
/// 落在 `<assets>/settings/camera.json`，由 `settings` 模块负责读写与防抖。
/// 每个策略的参数各占一个字段：切走再切回来，手感不丢。
#[derive(Resource, Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct CameraFollowSettings {
    pub strategy: CameraFollowStrategy,
    /// 弹簧参数：只在 [`CameraFollowStrategy::SmoothDamp`] 生效时被读取
    pub spring: SpringParams,
    /// 地面锚定参数：只在 [`CameraFollowStrategy::GroundAnchor`] 生效时被读取
    pub anchor: AnchorParams,
    /// 单帧位移超过这个距离直接硬切（重生 / 切关 / 瞬移），单位米。
    /// 不然弹簧会把机位从地图一头拉到另一头。
    pub snap_distance: f32,
}

impl Default for CameraFollowSettings {
    fn default() -> Self {
        Self {
            strategy: CameraFollowStrategy::default(),
            spring: SpringParams::default(),
            anchor: AnchorParams::default(),
            snap_distance: 8.0,
        }
    }
}

impl CameraFollowSettings {
    /// 夹回合法区间：读盘、滑杆、落盘前都过一遍
    pub fn sanitized(mut self) -> Self {
        self.spring.smooth_time = self
            .spring
            .smooth_time
            .clamp(SMOOTH_TIME_RANGE.0, SMOOTH_TIME_RANGE.1);
        self.spring.max_speed = self
            .spring
            .max_speed
            .clamp(MAX_SPEED_RANGE.0, MAX_SPEED_RANGE.1);
        self.anchor.smooth_time = self
            .anchor
            .smooth_time
            .clamp(super::ANCHOR_TIME_RANGE.0, super::ANCHOR_TIME_RANGE.1);
        self.anchor.dead_zone = self
            .anchor
            .dead_zone
            .clamp(super::DEAD_ZONE_RANGE.0, super::DEAD_ZONE_RANGE.1);
        self.anchor.max_speed = self
            .anchor
            .max_speed
            .clamp(super::ANCHOR_SPEED_RANGE.0, super::ANCHOR_SPEED_RANGE.1);
        self.snap_distance = self
            .snap_distance
            .clamp(SNAP_DISTANCE_RANGE.0, SNAP_DISTANCE_RANGE.1);
        self
    }

    /// 离散改动（下拉框换策略）——这类改动用不着防抖，立刻落盘
    pub fn discrete_changed(&self, other: &Self) -> bool {
        self.strategy != other.strategy
    }
}

/// 跟随的共享状态：焦点、它的速度，以及这一帧的理想目标（面板读数要用）。
///
/// 这是策略与机位之间唯一的契约。各策略只写 `focus/target/velocity`，
/// `orbit_camera` 只读 `focus()`。切换策略不清状态：焦点已经在角色附近，
/// 换过去只是换了一条追赶曲线。
#[derive(Resource, Debug, Default)]
pub struct CameraFollowState {
    /// 这一帧的焦点（视线落点，机位绕它转）
    pub(crate) focus: Vec3,
    /// 这一帧的理想锚点，用来算滞后 / 读数
    pub(crate) target: Vec3,
    /// 焦点的移动速度；哪个轴有意义由策略自己解释
    pub(crate) velocity: Vec3,
    /// 还没有历史（第一帧 / 传送后）：直接落在目标上
    pub(crate) initialized: bool,
}

impl CameraFollowState {
    /// 焦点是否已经建立：没建立时机位不摆，免得第一帧从世界原点扫过去
    pub fn is_ready(&self) -> bool {
        self.initialized
    }

    /// 这一帧的焦点（机位绕它转）
    pub fn focus(&self) -> Vec3 {
        self.focus
    }

    /// 焦点与理想点还差多远（米）：面板上的「滞后」
    pub fn lag(&self) -> f32 {
        (self.target - self.focus).length()
    }

    /// 焦点这一刻的移动速度（米/秒）：面板上的「焦点速度」
    pub fn speed(&self) -> f32 {
        self.velocity.length()
    }

    /// 第一帧或传送：直接落在目标上。返回 `true` 表示这一帧已经处理完，策略不用再追。
    pub(crate) fn snap_if_needed(&mut self, target: Vec3, snap_distance: f32) -> bool {
        let snap_distance = snap_distance.max(1e-3);
        let has_history = self.initialized;
        let far = (target - self.focus).length() > snap_distance;
        if !has_history || far {
            self.snap_to(target);
            return true;
        }
        false
    }

    /// 把焦点直接钉在目标上并清空速度
    pub(crate) fn snap_to(&mut self, target: Vec3) {
        self.target = target;
        self.focus = target;
        self.velocity = Vec3::ZERO;
        self.initialized = true;
    }

    /// 清掉跟随历史：下一帧直接落在目标上
    pub fn reset(&mut self) {
        self.velocity = Vec3::ZERO;
        self.initialized = false;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 下拉框下标与策略一一对应，越界退回默认
    #[test]
    fn strategy_index_round_trips() {
        for (index, strategy) in CameraFollowStrategy::ALL.iter().enumerate() {
            assert_eq!(strategy.index(), index);
            assert_eq!(CameraFollowStrategy::from_index(index), *strategy);
        }
        assert_eq!(
            CameraFollowStrategy::from_index(99),
            CameraFollowStrategy::default()
        );
    }

    /// 第一帧没有历史：直接落在目标上，不能从原点滑过去
    #[test]
    fn first_frame_snaps() {
        let mut state = CameraFollowState::default();
        let target = Vec3::new(5.0, 1.0, 0.0);
        assert!(state.snap_if_needed(target, 8.0));
        assert_eq!(state.focus(), target);
        assert!(state.is_ready());
        assert_eq!(state.speed(), 0.0);
    }

    /// 超过瞬移阈值（重生、切关）必须硬切，否则会把机位拽过整张地图
    #[test]
    fn teleport_beyond_snap_distance_snaps() {
        let mut state = CameraFollowState::default();
        state.snap_if_needed(Vec3::ZERO, 8.0);
        state.snap_if_needed(Vec3::new(0.0, 1.0, 0.0), 8.0);

        let jumped = Vec3::new(20.0, 1.0, 0.0);
        assert!(state.snap_if_needed(jumped, 8.0));
        assert_eq!(state.focus(), jumped);
        assert_eq!(state.speed(), 0.0);
    }

    /// 近距离的正常行走不该触发硬切
    #[test]
    fn nearby_target_is_not_a_teleport() {
        let mut state = CameraFollowState::default();
        state.snap_if_needed(Vec3::ZERO, 8.0);
        assert!(!state.snap_if_needed(Vec3::new(0.0, 0.0, 1.0), 8.0));
        assert_ne!(state.focus(), Vec3::new(0.0, 0.0, 1.0));
    }

    /// 滞后与速度是面板读数：目标不动时滞后应等于焦点到目标的距离
    #[test]
    fn lag_and_speed_reflect_the_gap() {
        let mut state = CameraFollowState::default();
        state.snap_if_needed(Vec3::ZERO, 8.0);
        state.target = Vec3::new(0.0, 1.0, 0.0);
        state.focus = Vec3::new(0.0, 0.5, 0.0);
        state.velocity = Vec3::new(0.0, 2.0, 0.0);
        assert!((state.lag() - 0.5).abs() < 1e-6);
        assert!((state.speed() - 2.0).abs() < 1e-6);
    }

    /// 重置之后第一帧又要重新硬切
    #[test]
    fn reset_forgets_history() {
        let mut state = CameraFollowState::default();
        state.snap_if_needed(Vec3::ZERO, 8.0);
        state.reset();
        assert!(!state.is_ready());
        let target = Vec3::new(0.0, 3.0, 0.0);
        assert!(state.snap_if_needed(target, 8.0));
        assert_eq!(state.focus(), target);
    }

    /// 打开文件里缺字段时按默认值补齐（策略默认 / 各自参数默认）
    #[test]
    fn settings_fill_missing_fields_from_defaults() {
        let loaded: CameraFollowSettings =
            serde_json::from_str(r#"{"strategy":"ground_anchor"}"#).unwrap();
        assert_eq!(loaded.strategy, CameraFollowStrategy::GroundAnchor);
        assert_eq!(loaded.spring, SpringParams::default());
        assert_eq!(loaded.anchor, AnchorParams::default());
        assert_eq!(
            loaded.snap_distance,
            CameraFollowSettings::default().snap_distance
        );
    }
}
