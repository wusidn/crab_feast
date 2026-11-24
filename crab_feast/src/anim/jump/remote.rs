//! 远端 / 模拟端观察式跳跃：没有输入，只按复制轨迹识别离地与触地。
//!
//! 剪辑选择与本地共用资源表和回退链，支撑脚改由复制相位推断。

use avian3d::prelude::{SpatialQuery, SpatialQueryFilter};
use bevy::animation::{AnimationClip, AnimationTargetId};
use bevy::prelude::*;

use crate::anim::character::ClipGait;
use crate::anim::core::ClipSet;
use crate::ground::GROUND_CONTACT_DIST;
use crate::input::LocomotionIntent;

#[cfg(feature = "net")]
use super::air::ABORT_BLEND_SECS;
use super::air::{AIR_GRACE_SECS, GRAVITY, REMOTE_JUMP_DETECT_VY};
use super::resources::{JumpPose, RUN_VARIANTS};
use super::selection::{pick_clip, probe_kind};
use super::state::JumpState;
use super::timeline::{Timeline, analyze_clip, sample_curve};
use super::tuning::{JumpProfile, JumpTuning, JumpTunings};

/// 远端 / 模拟端角色的一次跳跃解析结果：剪辑 + 时间轴 + 起跳髋高 + 本次档位参数。
pub struct RemoteJumpSetup {
    pub(super) clip: &'static str,
    pub(super) timeline: Timeline,
    pub(super) takeoff_hips: f32,
    pub(super) tuning: JumpTuning,
}

/// 远端 / 模拟端角色的跳跃动作层。
///
/// 远端没有输入，也没有本地那套步态相位窗口，手里只有复制过来的位置。所以不去猜
/// 「什么时候按下」，而是**观察**这具身体什么时候离地、什么时候触地，把剪辑的
/// 离地帧 / 落地帧对到这两个事件上：
///
/// - 起跳：脚底离地且竖直速度为正（走空坠落是负速度，不播起跳动作）。动作直接从
///   剪辑的**离地帧**开始，跳过前置下蹲——远端的起跳没有蓄力可看，播了反而晚；
///   已经离地 h 米才被看到时，按当前竖直速度倒推 `h / vy` 秒，让动作从真正的离地
///   时刻算起，而不是从「被看见离地」算起。
/// - 滞空：整段弧线按预测滞空（`2·vy/g`）慢放拉伸，播完钉在落地帧等触地。
/// - 落地：脚底回到接触距离以内即进入落地段，从落地帧开始放恢复段并淡出。
#[derive(Component)]
pub struct RemoteJump {
    pub(super) state: JumpState,
    pub(super) clip: &'static str,
    pub(super) timeline: Timeline,
    pub(super) takeoff_hips: f32,
    /// 本次跳跃的档位参数（起跳那一帧从表里取一次，之后不再读表）
    pub(super) tuning: JumpTuning,
    /// 已经飞了多久（含倒推量）
    pub(super) air_time: f32,
    /// 落地恢复段已播时长
    pub(super) land_time: f32,
    /// 进入动作层的时长，用于淡入
    pub(super) elapsed: f32,
    /// 动作层权重
    pub(super) weight: f32,
    /// 剪辑本地时间
    pub(super) clip_time: f32,
    /// 滞空慢放系数
    pub(super) slow: f32,
    /// 宽限计时：刚离地那一帧射线可能还贴地，先不判落地
    pub(super) grace: f32,
    /// 观察到的第几跳：远端自己数，用来轮换奔跑跳的三套剪辑。
    pub(super) variant: u8,
    /// 平滑后的竖直速度（m/s）
    pub(super) vy: f32,
    pub(super) last_y: f32,
    pub(super) primed: bool,
    /// 已经接过的最新一跳序号（`NetJump.seq`）
    #[cfg(feature = "net")]
    pub(super) net_seq: u8,
    /// 是否已经见过第一条跳跃状态
    #[cfg(feature = "net")]
    pub(super) net_primed: bool,
    /// 上一帧见过的阶段编码，用来打排障日志（`CRAB_LOG_NET_JUMP`）
    #[cfg(feature = "net")]
    pub(super) net_phase: u8,
}

impl Default for RemoteJump {
    fn default() -> Self {
        Self {
            state: JumpState::Ground,
            clip: JumpPose::Stand.clip(),
            timeline: Timeline::default(),
            takeoff_hips: 0.0,
            tuning: JumpTuning::default(),
            air_time: 0.0,
            land_time: 0.0,
            elapsed: 0.0,
            weight: 0.0,
            clip_time: 0.0,
            slow: 1.0,
            grace: 0.0,
            variant: 0,
            vy: 0.0,
            last_y: 0.0,
            primed: false,
            #[cfg(feature = "net")]
            net_seq: 0,
            #[cfg(feature = "net")]
            net_primed: false,
            #[cfg(feature = "net")]
            net_phase: 0,
        }
    }
}

impl RemoteJump {
    pub fn weight(&self) -> f32 {
        self.weight
    }

    pub fn clip(&self) -> &'static str {
        self.clip
    }

    pub fn clip_time(&self) -> f32 {
        self.clip_time
    }

    pub fn takeoff_hips(&self) -> f32 {
        self.takeoff_hips
    }

    pub fn is_airborne(&self) -> bool {
        self.state == JumpState::Air
    }

    /// 用这具身体的实际轨迹推进动作层。
    ///
    /// - `y`：根节点的世界高度（用来差分竖直速度）
    /// - `height`：脚底距地高度；射线打不中（飞得太高）传 `None`
    /// - `setup`：只在「确认离地」那一帧调用一次，参数是这一跳该用第几套剪辑
    ///   （远端自己按观察到的起跳次数轮换），给出本次该播的剪辑与时间轴
    pub fn observe(
        &mut self,
        dt: f32,
        y: f32,
        height: Option<f32>,
        setup: impl FnOnce(u8) -> Option<RemoteJumpSetup>,
    ) {
        if dt <= f32::EPSILON {
            return;
        }
        let raw = if self.primed {
            (y - self.last_y) / dt
        } else {
            0.0
        };
        self.primed = true;
        self.last_y = y;
        // 复制位置是阶跃到达的，单帧差分会有 0 和尖峰；低通一下再当竖直速度用
        self.vy = self.vy * 0.6 + raw * 0.4;

        let lifted = height.is_none_or(|h| h > GROUND_CONTACT_DIST);

        match self.state {
            JumpState::Air => {
                self.elapsed += dt;
                self.grace += dt;
                self.air_time += dt;
                if self.grace >= AIR_GRACE_SECS && !lifted {
                    // 触地即进落地段：恢复段从剪辑的落地帧开始，落地点对得上物理
                    self.state = JumpState::Land;
                    self.land_time = 0.0;
                    self.clip_time = self.timeline.land;
                } else {
                    self.clip_time = self.air_clip_time();
                }
                self.weight =
                    (self.elapsed / self.timeline.blend_secs.max(f32::EPSILON)).clamp(0.0, 1.0);
            }
            JumpState::Land => {
                self.land_time += dt;
                let recover = self.timeline.recover_secs.max(f32::EPSILON);
                let k = (self.land_time / recover).clamp(0.0, 1.0);
                self.clip_time = self.timeline.land + k * (self.timeline.end - self.timeline.land);
                self.weight = (1.0 - k).clamp(0.0, 1.0);
                if self.land_time >= recover {
                    self.state = JumpState::Ground;
                    self.weight = 0.0;
                }
            }
            _ => {
                if self.weight > 0.0 {
                    self.weight = 0.0;
                }
                // 只有「真的在往上走」才算起跳
                if !lifted || self.vy <= REMOTE_JUMP_DETECT_VY {
                    return;
                }
                // 每确认一次离地就是一跳：套别接着上一跳往下轮，
                // 远端没有上行序号可用，自己数也一样能看出「每跳不一样」。
                let variant = self.variant;
                self.variant = (self.variant + 1) % RUN_VARIANTS;
                let Some(entry) = setup(variant) else {
                    return;
                };
                self.clip = entry.clip;
                self.timeline = entry.timeline;
                self.takeoff_hips = entry.takeoff_hips;
                // 远端也按档走：预测与慢放下限来自起跳那一帧解析出的档位，
                // 不存在「跑跳的慢放被站跳拖慢」这种串味。
                self.tuning = entry.tuning;
                self.state = JumpState::Air;
                self.grace = 0.0;
                self.elapsed = 0.0;
                let launch = self.vy.max(1.0);
                // 倒推已经飞过的时间：动作从真正的离地时刻开始
                self.air_time = (height.unwrap_or(0.0) / launch).clamp(0.0, 0.2);
                let predicted = (2.0 * launch / GRAVITY * self.tuning.air_predict)
                    .clamp(self.timeline.air_secs.max(0.01), self.tuning.max_air_hold);
                self.slow = (self.timeline.air_secs / predicted).clamp(self.tuning.slow_min, 1.0);
                self.clip_time = self.air_clip_time();
            }
        }
    }

    /// 按服务端复制的跳跃状态推进（`NetJump`）。
    ///
    /// 与 [`Self::observe`] 相比，起跳与触地时刻来自服务端，不再靠「位置差 + 竖直速度」
    /// 猜：阶段里的 `ticks` 直接映射到剪辑时间轴，动作包晚到（RTT）也落在正确相位；
    /// `seq` 变化就是新的一跳，剪辑轮换也不再依赖「自己数到几次离地」。
    #[cfg(feature = "net")]
    pub fn drive_net(
        &mut self,
        dt: f32,
        net: &crab_feast_library::net::NetJump,
        setup: impl FnOnce(u8) -> Option<RemoteJumpSetup>,
    ) {
        use crab_feast_library::net::{TICK_HZ, jump_phase};

        let phase = net.phase;
        // 阶段内已过 tick → 秒；直接当剪辑时间轴的锚点用
        let ticks = net.ticks as f32 / TICK_HZ as f32;
        let fresh = phase != jump_phase::GROUND && (net.seq != self.net_seq || !self.net_primed);
        self.net_primed = true;
        if std::env::var("CRAB_LOG_NET_JUMP").is_ok() && phase != self.net_phase {
            info!(
                "remote jump: seq={} phase {}→{} ticks={}",
                net.seq, self.net_phase, phase, net.ticks
            );
        }
        self.net_phase = phase;

        if fresh {
            self.net_seq = net.seq;
            let variant = self.variant;
            self.variant = (self.variant + 1) % RUN_VARIANTS;
            if let Some(entry) = setup(variant) {
                self.clip = entry.clip;
                self.timeline = entry.timeline;
                self.takeoff_hips = entry.takeoff_hips;
                self.tuning = entry.tuning;
                self.state = match phase {
                    jump_phase::CHARGE => JumpState::Takeoff,
                    jump_phase::AIR => JumpState::Air,
                    jump_phase::LAND => JumpState::Land,
                    _ => JumpState::Ground,
                };
                self.elapsed = 0.0;
                self.weight = 0.0;
                self.air_time = ticks;
                self.land_time = ticks;
                if self.state == JumpState::Air {
                    let predicted = (2.0 * self.tuning.net_launch_vy / GRAVITY
                        * self.tuning.air_predict)
                        .clamp(self.timeline.air_secs.max(0.01), self.tuning.max_air_hold);
                    self.slow =
                        (self.timeline.air_secs / predicted).clamp(self.tuning.slow_min, 1.0);
                }
                self.clip_time = match self.state {
                    JumpState::Takeoff => {
                        self.timeline
                            .clip_time(JumpState::Takeoff, ticks, 0.0, 0.0, 1.0)
                    }
                    JumpState::Air => self.air_clip_time(),
                    JumpState::Land => self.timeline.clip_time(
                        JumpState::Land,
                        self.elapsed,
                        0.0,
                        self.land_time,
                        self.slow,
                    ),
                    _ => 0.0,
                };
            } else {
                // 剪辑没解析出来（角色/资产还没就绪）：这一跳先不演，等下一跳
                self.state = JumpState::Ground;
                self.weight = 0.0;
            }
        }

        match phase {
            jump_phase::CHARGE => {
                if self.state == JumpState::Takeoff {
                    self.elapsed += dt;
                    self.clip_time =
                        self.timeline
                            .clip_time(JumpState::Takeoff, ticks, 0.0, 0.0, 1.0);
                    self.weight = self.timeline.weight(JumpState::Takeoff, self.elapsed, 0.0);
                }
            }
            jump_phase::AIR => {
                if self.state != JumpState::Ground {
                    if self.state != JumpState::Air {
                        // 蓄力段在中途直接收到「已离地」：切到滞空，时间按服务端 tick 落位
                        self.state = JumpState::Air;
                        let predicted = (2.0 * self.tuning.net_launch_vy / GRAVITY
                            * self.tuning.air_predict)
                            .clamp(self.timeline.air_secs.max(0.01), self.tuning.max_air_hold);
                        self.slow =
                            (self.timeline.air_secs / predicted).clamp(self.tuning.slow_min, 1.0);
                    }
                    self.elapsed += dt;
                    self.air_time = ticks;
                    self.clip_time = self.air_clip_time();
                    self.weight = self.timeline.weight(JumpState::Air, self.elapsed, 0.0);
                }
            }
            jump_phase::LAND => {
                if self.state != JumpState::Ground {
                    if self.state != JumpState::Land {
                        self.state = JumpState::Land;
                    }
                    self.elapsed += dt;
                    self.land_time = ticks;
                    self.clip_time = self.timeline.clip_time(
                        JumpState::Land,
                        self.elapsed,
                        0.0,
                        self.land_time,
                        self.slow,
                    );
                    self.weight =
                        self.timeline
                            .weight(JumpState::Land, self.elapsed, self.land_time);
                    if self.land_time >= self.timeline.recover_secs {
                        self.state = JumpState::Ground;
                        self.weight = 0.0;
                    }
                }
            }
            _ => match self.state {
                // 服务端落地段比剪辑恢复段短：剩下的一段按本地时间播完，不硬切
                JumpState::Land => {
                    self.land_time += dt;
                    let recover = self.timeline.recover_secs.max(f32::EPSILON);
                    let k = (self.land_time / recover).clamp(0.0, 1.0);
                    self.clip_time =
                        self.timeline.land + k * (self.timeline.end - self.timeline.land);
                    self.weight = (1.0 - k).clamp(0.0, 1.0);
                    if self.land_time >= self.timeline.recover_secs {
                        self.state = JumpState::Ground;
                        self.weight = 0.0;
                    }
                }
                // 蓄力被服务端取消（被推离地面 / 起跳被物理吃掉）：定格淡出
                JumpState::Takeoff => {
                    self.weight = (self.weight - dt / ABORT_BLEND_SECS).max(0.0);
                    if self.weight <= 0.0 {
                        self.state = JumpState::Ground;
                    }
                }
                // 空中被直接判回地面：按落地恢复段收尾
                JumpState::Air => {
                    self.state = JumpState::Land;
                    self.land_time = 0.0;
                }
                _ => self.weight = 0.0,
            },
        }
    }

    fn air_clip_time(&self) -> f32 {
        let air = self.timeline.air_secs;
        if air <= f32::EPSILON {
            return self.timeline.land;
        }
        let k = (self.air_time * self.slow / air).clamp(0.0, 1.0);
        self.timeline.takeoff + k * (self.timeline.land - self.timeline.takeoff)
    }
}

/// 远端起跳那一帧解析：探台阶类型、挑剪辑、跑时间轴。
///
/// 参数里带着复制端的实测速度与相位，所以远端不需要知道按键，也能播对的那一条。
#[allow(clippy::too_many_arguments)]
pub fn remote_jump_setup(
    spatial: &SpatialQuery,
    filter: &SpatialQueryFilter,
    transform: &Transform,
    speed: f32,
    direction: Vec3,
    phase: Option<f32>,
    gait: Option<&ClipGait>,
    variant: u8,
    hips: AnimationTargetId,
    clips: &ClipSet,
    assets: &Assets<AnimationClip>,
    tunings: &JumpTunings,
) -> Option<RemoteJumpSetup> {
    let intent = LocomotionIntent {
        direction,
        magnitude: speed,
        // 远端挑跳跃剪辑只看方向与档位，转身降速不参与
        speed_scale: 1.0,
    };
    let kind = probe_kind(spatial, filter, transform, &intent);
    let clip = pick_clip(kind, speed, phase, gait, variant, clips);
    let asset = assets.get(clips.handle(clip)?)?;
    let (mut timeline, curve, step) = analyze_clip(clip, asset, hips);
    let tuning = tunings.get(JumpProfile::for_jump(kind, speed));
    // 与本地联网路径同一条规则：蓄力段按档位表的时长，和服务端 `charge_secs` 对齐。
    // 不覆盖的话远端按剪辑自带的蓄力长度播，服务端却按表里的时长起跳，离地帧会错开。
    if let Some(secs) = tuning.take_secs {
        timeline.set_take_secs(secs);
    }
    Some(RemoteJumpSetup {
        clip,
        timeline,
        takeoff_hips: sample_curve(&curve, step, timeline.takeoff),
        tuning,
    })
}
