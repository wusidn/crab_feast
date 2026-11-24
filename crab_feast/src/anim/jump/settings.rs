//! 跳跃手感设置：起跳速度、空中操控力度、空中接管时长。
//!
//! 这三个值来自「跳跃设置」菜单页，落在 `<assets>/settings/jump.json`。
//! 它们同时是**联网协议的一部分**：每条上行快照都把当前值带上（见
//! `crab_feast_library::net::MoveInput`），服务端按同一个区间夹取后照着推人。
//! 所以改这里的默认值必须同步改 `crab_feast_library` 里的 `JUMP_*_DEFAULT`，
//! 否则「本机默认」和「服务端缺省」会差一截，联机第一跳就被和解拽回来。
//!
//! 区间与默认值全部来自共享库，客户端 / 服务端只有一份，不在本文件另写数字。

use bevy::prelude::*;
use serde::{Deserialize, Serialize};

use crab_feast_library::air::{
    AIR_ACCEL_DEFAULT, AIR_ACCEL_RANGE, AIR_STEER_SECS_DEFAULT, AIR_STEER_SECS_RANGE,
    JUMP_LAUNCH_VY_DEFAULT, JUMP_LAUNCH_VY_RANGE,
};

use super::tuning::JumpTuning;

/// 跳跃手感设置。
///
/// 全部是连续值（滑杆），改动一律走防抖落盘，没有「开关类」需要立刻写。
#[derive(Resource, Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct JumpSettings {
    /// 起跳竖直初速度（米/秒）：决定跳多高、滞空多久。
    ///
    /// 这一项对四档（站 / 跑 / 上 / 下）统一生效：玩家感知的是「我这一跳跳多高」，
    /// 不是「站着跳和跑着跳要分别调」。档位表里的 `launch_vy` 只在没设置过时兜底。
    pub launch_vy: f32,
    /// 空中操控加速度（米/秒²）：0 = 完全锁死落点，越大空中越「跟手」。
    pub air_accel: f32,
    /// 起跳后多久停止接管水平速度（秒）：这一段之后落点定死，推杆只转朝向。
    pub air_steer_secs: f32,
}

impl Default for JumpSettings {
    fn default() -> Self {
        Self {
            launch_vy: JUMP_LAUNCH_VY_DEFAULT,
            air_accel: AIR_ACCEL_DEFAULT,
            air_steer_secs: AIR_STEER_SECS_DEFAULT,
        }
    }
}

impl JumpSettings {
    /// 把三项都夹回合法区间。读盘 / 收到远端值 / 拖滑杆越界都走这里。
    pub fn sanitized(mut self) -> Self {
        self.launch_vy = self
            .launch_vy
            .clamp(JUMP_LAUNCH_VY_RANGE.0, JUMP_LAUNCH_VY_RANGE.1);
        self.air_accel = self.air_accel.clamp(AIR_ACCEL_RANGE.0, AIR_ACCEL_RANGE.1);
        self.air_steer_secs = self
            .air_steer_secs
            .clamp(AIR_STEER_SECS_RANGE.0, AIR_STEER_SECS_RANGE.1);
        self
    }

    /// 把设置叠到一份档位参数上：起跳速度统一覆盖本地与联网两个字段。
    ///
    /// 只覆盖 `launch_vy` / `net_launch_vy`，其余（蓄力时长、滞空预测系数、
    /// 兜底超时）仍归档位表：那些是「这套剪辑怎么播」，不是玩家该调的东西。
    pub fn apply_to(self, mut tuning: JumpTuning) -> JumpTuning {
        let launch = self
            .launch_vy
            .clamp(JUMP_LAUNCH_VY_RANGE.0, JUMP_LAUNCH_VY_RANGE.1);
        tuning.launch_vy = launch;
        tuning.net_launch_vy = launch;
        tuning
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 读盘读到一个越界值：夹回来，不 panic
    #[test]
    fn sanitize_clamps_every_slider() {
        let wild = JumpSettings {
            launch_vy: 99.0,
            air_accel: -3.0,
            air_steer_secs: 10.0,
        }
        .sanitized();
        assert_eq!(wild.launch_vy, JUMP_LAUNCH_VY_RANGE.1);
        assert_eq!(wild.air_accel, AIR_ACCEL_RANGE.0);
        assert_eq!(wild.air_steer_secs, AIR_STEER_SECS_RANGE.1);
    }

    /// 设置必须同时覆盖本地 / 联网两个起跳速度，否则联机时两边跳不一样高
    #[test]
    fn apply_to_overrides_both_launch_fields() {
        let settings = JumpSettings {
            launch_vy: 6.5,
            ..JumpSettings::default()
        };
        let tuning = settings.apply_to(JumpTuning::default());
        assert!((tuning.launch_vy - 6.5).abs() < 1e-6);
        assert!((tuning.net_launch_vy - 6.5).abs() < 1e-6);
    }

    /// 缺字段的旧文件按默认值补齐（serde(default) 生效）
    #[test]
    fn missing_fields_fall_back_to_defaults() {
        let parsed: JumpSettings = serde_json::from_str("{}").unwrap();
        assert_eq!(parsed, JumpSettings::default());
    }
}
