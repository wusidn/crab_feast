//! 跳跃大类与四档手感参数：`JumpKind` / `JumpProfile` / `JumpTuning(s)`。
//!
//! 档位只决定取哪一行资源参数，具体数值仍在 `resources.rs` 的表里。

use bevy::prelude::Resource;

use super::resources::{JumpPose, RULES, default_tuning};

/// 跳跃大类：平地跳 / 跳上高台 / 跳下高台。
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum JumpKind {
    #[default]
    Flat,
    Up,
    Down,
}

impl JumpKind {
    /// 上行协议里的编码：客户端在**真正起跳那一帧**把自己判出来的大类发给服务端。
    /// 服务端不自己再探一遍——它的 `Transform` 朝向是恒等的，站定起跳时探不出前方。
    pub fn code(self) -> u8 {
        match self {
            JumpKind::Flat => 0,
            JumpKind::Up => 1,
            JumpKind::Down => 2,
        }
    }

    pub fn from_code(code: u8) -> Self {
        match code {
            1 => JumpKind::Up,
            2 => JumpKind::Down,
            _ => JumpKind::Flat,
        }
    }
}

/// 跳跃调参档位：**跑跳 / 站立跳 / 跳上 / 跳下** 各一套，互不覆盖。
///
/// 之前这些旋钮是所有分支共用的常量：把「跑跳要更高」改一下，站立跳、上跳、
/// 下跳会一起被改掉，而且空中姿态的慢放会被一个全局下限拖着走。现在统一走
/// [`JumpTunings`]，一档一个 [`JumpTuning`]；起跳那一帧还会把整份参数快照进
/// [`JumpAction`](super::state::JumpAction)，空中改表不会影响已经在飞的这一跳。
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum JumpProfile {
    /// 平地站立 / 慢走起跳（`Jumping`）
    #[default]
    Stand,
    /// 奔跑起跳（`RunningJump1/2/3` 三套 + 各自的左 / 右镜像派生）
    Run,
    /// 跳上高台（`JumpingUp`）
    Up,
    /// 跳下高台（`JumpingDown`）
    Down,
}

impl JumpProfile {
    /// 档位数：`JumpTunings` 用定长数组存，换档只查表不分配。
    pub const COUNT: usize = 4;

    pub fn index(self) -> usize {
        match self {
            JumpProfile::Stand => 0,
            JumpProfile::Run => 1,
            JumpProfile::Up => 2,
            JumpProfile::Down => 3,
        }
    }

    /// 从「台阶类型 + 地速」定档：高差优先，平地再按速度分站 / 跑。
    pub fn for_jump(kind: JumpKind, ground_speed: f32) -> Self {
        match kind {
            JumpKind::Up => JumpProfile::Up,
            JumpKind::Down => JumpProfile::Down,
            JumpKind::Flat if ground_speed >= RULES.run_jump_speed => JumpProfile::Run,
            JumpKind::Flat => JumpProfile::Stand,
        }
    }
}

/// 单档跳跃的全部手感旋钮。值都是「这一档自己的」，改哪一档都不会顺手改别的档。
///
/// 默认值不在逻辑层写死：每档的初值来自资源参数表（[`JumpPose`] 的行，
/// 见 [`default_tuning`]）。
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct JumpTuning {
    /// 单机 Dynamic 角色的起跳初速度下限（m/s）。实测更高时才采用。
    pub launch_vy: f32,
    /// 联网起跳速度（m/s）：服务端写入的就是它，客户端 Kinematic 预测也用它。
    /// 一个旋钮同时管两端，不会再出现「本地 2.6 / 服务端 5.0」两套高度。
    pub net_launch_vy: f32,
    /// 联网时覆盖剪辑蓄力段（秒）。`None` = 按剪辑自己的节奏播。
    pub take_secs: Option<f32>,
    /// 服务端贴地蓄力时长（秒）。与 `take_secs` 取同一个数，离地帧才对得上。
    pub charge_secs: f32,
    /// 滞空预测缓冲：`2·vy/g` 再乘它，覆盖落地容差与顶点延迟。
    pub air_predict: f32,
    /// 空中姿态慢放下限：再低就成慢镜头。
    pub slow_min: f32,
    /// 滞空兜底上限（秒）：射线失效时强制结束滞空段。
    pub max_air_hold: f32,
}

impl Default for JumpTuning {
    /// 默认取原地跳那一档（资源表里 [`JumpPose::Stand`] 的行）。
    fn default() -> Self {
        JumpPose::Stand.tuning()
    }
}

/// 四档调参表。运行时换这个资源就能单独调某一档，不影响另外三档。
#[derive(Resource, Clone, Copy, Debug)]
pub struct JumpTunings([JumpTuning; JumpProfile::COUNT]);

impl JumpTunings {
    pub fn get(&self, profile: JumpProfile) -> JumpTuning {
        self.0[profile.index()]
    }

    /// 单独改一档：`tunings.tune(JumpProfile::Run).net_launch_vy = 5.6;`
    /// 其余三档保持原值。
    pub fn tune(&mut self, profile: JumpProfile) -> &mut JumpTuning {
        &mut self.0[profile.index()]
    }

    pub fn set(&mut self, profile: JumpProfile, tuning: JumpTuning) {
        self.0[profile.index()] = tuning;
    }
}

impl Default for JumpTunings {
    fn default() -> Self {
        let mut tunings = [JumpTuning::default(); JumpProfile::COUNT];
        for profile in [
            JumpProfile::Stand,
            JumpProfile::Run,
            JumpProfile::Up,
            JumpProfile::Down,
        ] {
            tunings[profile.index()] = default_tuning(profile);
        }
        Self(tunings)
    }
}
