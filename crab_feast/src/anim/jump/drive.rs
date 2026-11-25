//! 本地驱动：按键锁存、每帧状态机、`begin` 解析与根位移。
//!
//! 跑在 `AnimationSet::Drive`，晚于移动 / 朝向；插件注册在 `plugin.rs`。

use avian3d::prelude::{LinearVelocity, RigidBody, SpatialQuery, SpatialQueryFilter};
use bevy::animation::AnimationClip;
use bevy::prelude::*;

use crate::anim::character::{AnimCatalog, AnimSelection};
use crate::anim::core::ClipSet;
use crate::anim::driver::{AnimFocus, PoseDriver};
use crate::anim::mixers::{LocomotionMixer, RegisteredMixer};
use crate::ground::{GROUND_CONTACT_DIST, foot_ground_dist};
use crate::input::{JumpInput, LocomotionIntent};
use crate::scene::Avatar;

use super::air::{
    ABORT_BLEND_SECS, AIR_GRACE_SECS, AIR_LIFT_DIST, GRAVITY, GROUND_NOISE_VY,
    NET_TAKEOFF_FAIL_SECS, TAKEOFF_FAIL_SECS, TAKEOFF_RISE_EPS, launch_speed, takeoff_failed,
};
use super::gait::{STOP_SPEED, ready_to_jump, running_takeoff_ready, takeoff_allowed};
use super::landing::{follow_landing_phase, landing_alignment};
use super::resources::RULES;
use super::selection::{probe_kind, resolve_clip, support_from_contacts};
use super::state::{JumpAction, JumpPolicy, JumpState};
use super::timeline::{analyze_clip, sample_curve};
use super::tuning::{JumpKind, JumpProfile, JumpTuning, JumpTunings};

/// 按键 → 请求。边沿锁存：按住不会在落地后立刻再跳一次。
///
/// 落地恢复段（[`JumpState::Land`]）里按下也算数：那 0.2 秒里人看着已经站稳了，
/// 这一下若被丢掉，玩家只会觉得「按了没反应」。请求先挂起，回到地面就起跳。
/// 起跳失败后的 [`JumpState::Abort`] 同理：人本来就还站在地上，只差几十毫秒淡出。
pub(super) fn latch_jump_request(input: Res<JumpInput>, mut action: ResMut<JumpAction>) {
    match *input {
        JumpInput::Activated => {
            if !action.hold_latched {
                action.hold_latched = true;
                if matches!(
                    action.state,
                    JumpState::Ground | JumpState::Queued | JumpState::Land | JumpState::Abort
                ) {
                    action.queued = true;
                    action.queued_for = 0.0;
                }
            }
        }
        JumpInput::Idle => action.hold_latched = false,
    }
}

/// 本地角色查询：实体 + 移动意向。
///
/// 意向并进这条查询是为了压住系统参数上限（Bevy 最多 16 个参数）。
type LocalAvatarQuery<'w, 's> = Query<
    'w,
    's,
    (Entity, Option<&'static LocomotionIntent>),
    With<Avatar>,
>;

/// 状态机 + 时间轴 + 垂直位移。跑在 `AnimationSet::Drive`，晚于移动/朝向。
#[allow(clippy::too_many_arguments)]
pub(super) fn drive_jump(
    time: Res<Time>,
    focus: Res<AnimFocus>,
    policy: Res<JumpPolicy>,
    mut driver: ResMut<PoseDriver>,
    catalog: Res<AnimCatalog>,
    selection: Res<AnimSelection>,
    clips: Res<ClipSet>,
    clip_assets: Res<Assets<AnimationClip>>,
    tunings: Res<JumpTunings>,
    mut action: ResMut<JumpAction>,
    avatar_entities: LocalAvatarQuery,
    mut velocities: Query<&mut LinearVelocity>,
    rigid_bodies: Query<&RigidBody>,
    transforms: Query<&Transform, With<Avatar>>,
    spatial: SpatialQuery,
    // 联机模式（含预测端）：离地速度必须用服务端同一份 `net_launch_vy`，
    // 不能按本地实测抬升，否则两端跳起来的高度不同
    mode: Option<Res<crate::state::SessionMode>>,
) {
    let dt = time.delta_secs();
    if dt <= f32::EPSILON {
        return;
    }
    // 预览单条剪辑时姿态归 AnimationPlayer，动作层不参与，也不该让角色真的起飞
    if focus.preview {
        return;
    }

    // 地面 / 排队：只有这里接受新请求
    if matches!(action.state, JumpState::Ground | JumpState::Queued) {
        if !action.queued {
            action.state = JumpState::Ground;
            return;
        }
        action.queued_for += dt;

        let standing = !driver.evaluated || driver.ground_speed < STOP_SPEED;
        // 联网也照等相位窗口：窗口保证起跳那一帧是单脚支撑、重心在后的蹬地相，
        // 主控端的姿态优先。上行电平会跟着窗口走（`send_input` 排在动作层之后，
        // 等窗口时电平压住），所以服务端的蓄力与客户端的起跳帧仍然对得上。
        // 奔跑档强制单脚着地 + 重心在后的起跳窗口；站立 / 走路档维持原策略。
        let window_open = standing
            || if driver.ground_speed >= RULES.run_jump_speed {
                running_takeoff_ready(&driver, &catalog, &selection)
            } else {
                ready_to_jump(policy.timing, &driver, &catalog, &selection)
            };
        // 相位锁定只负责「推迟」：等满一个步幅周期窗口还没来，就按当前步态起跳。
        // 请求一旦锁存就必须落地执行，绝不因为窗口不开而丢掉——那才是玩家感知到的
        // 「按了跳没反应」。
        let still_waiting = !takeoff_allowed(window_open, action.queued_for);
        if still_waiting {
            action.state = JumpState::Queued;
            return;
        }
        if !window_open {
            debug!(
                "jump: 等窗口 {:.2}s 超时，按当前步态强制起跳（地速 {:.2} 接触[{:.2},{:.2}]）",
                action.queued_for, driver.ground_speed, driver.contacts[0], driver.contacts[1]
            );
        }

        let Ok((avatar, intent)) = avatar_entities.single() else {
            return;
        };
        let intent = intent.copied().unwrap_or_default();
        let filter = SpatialQueryFilter::from_excluded_entities([avatar]);
        let transform = transforms.single().ok().copied();
        let kind = transform
            .map(|t| probe_kind(&spatial, &filter, &t, &intent))
            .unwrap_or_default();
        // 大类 + 地速 → 调参档。四档各一套参数（跑 / 站 / 上 / 下），
        // 之后所有手感旋钮都只从这一次查表结果里取，不再读全局常量。
        //
        let ground_speed = driver.ground_speed;
        let profile = JumpProfile::for_jump(kind, ground_speed);
        // 起跳那一帧脚底距地：联网时这具身体由服务端抬升，这里能看出客户端的
        // 动作是不是比服务端的离地晚（值 > AIR_LIFT_DIST 就是「已经在空中了」）。
        let gd_begin = transform.and_then(|t| foot_ground_dist(&spatial, &filter, &t));
        let tuning = tunings.get(profile);
        let clip = resolve_clip(
            kind,
            ground_speed,
            support_from_contacts(driver.contacts),
            // 连跳轮换：第 n 跳用第 n 套奔跑跳（1 → 2 → 3 → 1 …），
            // 站着跳 / 上跳 / 下跳都只有一套，传什么都会退到第一套。
            action.run_variant(),
            &clips,
        );
        // 起跳基线：这一跳有没有真的把根抬起来，从起跳这一帧的根高度开始量
        let takeoff_y = transform.map_or(0.0, |t| t.translation.y);

        // 联网：这具身体的位置由服务端抬升，蓄力段按服务端的蓄力时长对齐，
        // 让动画的离地帧落在根位置真正开始上升的那一刻。
        // 时长取**这一档自己的**，不再按地速借用平地那两档。
        let kinematic = rigid_bodies.get(avatar).is_ok_and(|rb| rb.is_kinematic());
        // 联网角色是 Kinematic：身体由服务端 `NetPos` 抬升，蓄力时长要按服务端那一档
        // 对齐，动画的离地帧才会落在根位置真正开始上升的那一刻
        let net_mode = kinematic;
        let take_secs = net_mode.then_some(tuning.take_secs).flatten();
        if !begin(
            &mut action,
            &driver,
            &clips,
            &clip_assets,
            kind,
            profile,
            clip,
            tuning,
            take_secs,
            takeoff_y,
        ) {
            action.queued = false;
            action.state = JumpState::Ground;
            return;
        }
        info!(
            "跳跃：{:?}/{:?}（剪辑 {clip}，蓄力 {:.3}s，离地 {:.3}s，蹲低点 {:.3}s，地速 {:.2} m/s，接触 [{:.2},{:.2}]，相位 {:.3}，脚底距地 {:.3} m，起跳 {:?}）",
            kind,
            profile,
            action.timeline.take_secs,
            action.timeline.takeoff,
            action.crouch_low,
            ground_speed,
            driver.contacts[0],
            driver.contacts[1],
            driver
                .phases
                .get(LocomotionMixer::ID)
                .copied()
                .unwrap_or(-1.0),
            gd_begin.unwrap_or(-1.0),
            if net_mode {
                format!("{:.2} m/s（联网）", tuning.net_launch_vy)
            } else {
                format!("{:.2} m/s（本地）", tuning.launch_vy)
            }
        );
        return;
    }

    // 动作进行中
    action.elapsed += dt;
    let elapsed = action.elapsed;
    let previous = action.state;
    let timeline = action.timeline;

    // 脚下接地探测（排除自身胶囊）。Air 是否结束看物理是否落地，
    // 而不是剪辑空中段是否播完——物理滞空通常比剪辑空中段长。
    // 落地要脚底距地足够近；Dynamic 还要求残余上升速度归零，排除蹬升初期贴地误判。
    let Ok((avatar, _)) = avatar_entities.single() else {
        return;
    };
    let transform = transforms.get(avatar).ok().copied();
    let vy = velocities.get(avatar).ok().map(|v| v.0.y);
    let ground_dist = transform.map(|t| {
        let filter = SpatialQueryFilter::from_excluded_entities([avatar]);
        foot_ground_dist(&spatial, &filter, &t)
    });
    let ground_dist = ground_dist.flatten();
    let upward = vy.unwrap_or(0.0);
    // Kinematic 位置由外部（服务端同步）驱动，速度组件不被物理积分、值停留在起跳
    // 写入值而失真；这类角色接地只看脚底距离，不再用速度门槛把落地卡死。
    let kinematic = rigid_bodies.get(avatar).is_ok_and(|rb| rb.is_kinematic());
    // 贴地噪声（±0.2 m/s）不算「还在蹬升」，否则落地永远差那么一点点
    let speed_ok = kinematic || upward <= GROUND_NOISE_VY;
    let grounded = ground_dist.is_some_and(|d| d <= GROUND_CONTACT_DIST && speed_ok);
    // 确认离地：脚底离地超过 AIR_LIFT_DIST 才算真的抬起来过
    if ground_dist.is_some_and(|d| d > AIR_LIFT_DIST) {
        action.air_lifted = true;
    }
    // 确认上升：根比起身跳那一帧高了 TAKEOFF_RISE_EPS 才算物理真的把人抬起来。
    // 联网时读的是复制下来的服务端位置，是权威轨迹；单机读的是本地物理结果。
    if !action.climbed
        && let Some(t) = transform
        && t.translation.y > action.takeoff_y + TAKEOFF_RISE_EPS
    {
        action.climbed = true;
    }

    // 各段内进度累计：空中段播完后冻结在落地预备姿态，落地后才走恢复段。
    match action.state {
        JumpState::Air => {
            action.air_time += dt;
            action.air_hold += dt;
        }
        JumpState::Land => action.land_time += dt,
        _ => {}
    }

    // 状态推进：Takeoff 按蓄力时长；Air 由接地信号（或兜底超时）退出；Land 播完恢复段回地面。
    action.state = match action.state {
        JumpState::Takeoff => {
            if elapsed < timeline.take_secs {
                JumpState::Takeoff
            } else {
                JumpState::Air
            }
        }
        JumpState::Air => {
            let past_grace = action.air_hold >= AIR_GRACE_SECS;
            // 没抬起来过就不判落地：小速度起跳时人还贴着地面，射线会立刻判「接地」
            let landed = past_grace && action.air_lifted && grounded;
            let timed_out = action.air_hold >= action.tuning.max_air_hold;
            // 起跳失败：物理把这一跳顶回去了。顶棚、贴身压过来的刚体、贴墙的摩擦
            // 都会把写入的离地速度当场吃掉——根不升、脚不离地，人还站在原处。
            // 不等 `max_air_hold` 兜底（2 秒），窗口一到就进 Abort 定格淡出。
            let fail_after = if kinematic {
                NET_TAKEOFF_FAIL_SECS
            } else {
                TAKEOFF_FAIL_SECS
            };
            let failed = takeoff_failed(
                action.air_hold,
                fail_after,
                action.climbed,
                action.air_lifted,
                grounded,
            );
            if failed {
                debug!(
                    "jump: 起跳失败（根未抬升、脚未离地），{:.2}s 退出跳跃状态、定格离地帧淡出：{}",
                    action.air_hold,
                    action.clip_name()
                );
                JumpState::Abort
            } else if landed || timed_out {
                JumpState::Land
            } else {
                JumpState::Air
            }
        }
        JumpState::Land => {
            if action.land_time < timeline.recover_secs {
                JumpState::Land
            } else {
                JumpState::Ground
            }
        }
        // 起跳失败：权重淡到零就回地面。这一跳没有落地冲击，不等恢复段。
        JumpState::Abort => {
            if action.weight <= 0.0 {
                JumpState::Ground
            } else {
                JumpState::Abort
            }
        }
        _ => JumpState::Ground,
    };

    action.clip_time = timeline.clip_time(
        action.state,
        elapsed,
        action.air_time,
        action.land_time,
        action.slow,
    );
    action.weight = if action.state == JumpState::Abort {
        // 起跳被物理吃掉的那一跳：从上一帧的权重开始，在 ABORT_BLEND_SECS 内淡出
        // 交回移动混合。时间轴那套「落地恢复段随 land_time 淡出」是按真实触地冲击
        // 设计的，这里人根本没离开地面，按它走会多演一次落地缓冲。
        (action.weight - dt / ABORT_BLEND_SECS).max(0.0)
    } else {
        timeline.weight(action.state, elapsed, action.land_time)
    };
    action.root_y = root_y_at(&action, action.clip_time);
    // 现场调参用：`CRAB_LOG_JUMP=1` 逐帧打出状态机（状态 / 权重 / 剪辑时间 / 接地距离 /
    // 移动相位 / 支撑权重），用来看空中是不是还在跑、落地交接有没有跳。
    if std::env::var("CRAB_LOG_JUMP").is_ok() {
        let phase = driver
            .phases
            .get(LocomotionMixer::ID)
            .copied()
            .unwrap_or(0.0);
        info!(
            "jump {} {:?} elapsed={:.3} air={:.3} land={:.3} w={:.2} clip={:.3} gd={:.3} \
             phase={:.3} contacts={:.2}/{:.2} slow={:.2}",
            action.clip_name(),
            action.state,
            elapsed,
            action.air_time,
            action.land_time,
            action.weight,
            action.clip_time,
            ground_dist.unwrap_or(-1.0),
            phase,
            driver.contacts[0],
            driver.contacts[1],
            action.slow
        );
    }

    match action.state {
        JumpState::Air => {
            // 进入滞空帧：记录起跳上升速度，据此预测滞空时长并给空中姿态算慢放系数。
            // 物理轨迹只在这一帧写入一次初速度，之后交回重力自然抛物；姿态时间轴
            // 单独按慢放推进，不再用被拉伸的曲线差分去覆写物理速度。
            if previous != JumpState::Air {
                let measured = vy.unwrap_or(0.0);
                let kinematic = rigid_bodies.get(avatar).is_ok_and(|rb| rb.is_kinematic());
                // 预测端的刚体是 Dynamic（自己积分），但它同样属于联机：起跳速度
                // 仍要取服务端那一档，否则客户端跳得比服务端高一截
                let net_mode = kinematic
                    || mode
                        .as_deref()
                        .is_some_and(|mode| matches!(mode, crate::state::SessionMode::Online { .. }));
                // 实测速度只用来「抬高」：贴地时接触求解会留下 ±0.2 m/s 的噪声，
                // 按它起跳只能升两三厘米，表现就是「按了跳、人只抬了一下」。
                // 联网 / 预测端直接用服务端同一份离地速度，不能按实测抬（会越抬越高）。
                let launch = if net_mode {
                    action.tuning.net_launch_vy
                } else {
                    launch_speed(measured, action.tuning)
                };
                action.launch_vy = launch;
                // Kinematic 位置由服务端同步、本地速度组件失真，按**这一档的联网起跳速度**预测；
                // Dynamic 按本地实测（或兜底）速度预测。两边取值都来自同一份档位表。
                let predict_vy = if net_mode {
                    action.tuning.net_launch_vy
                } else {
                    launch
                };
                let predicted_air = (2.0 * predict_vy / GRAVITY * action.tuning.air_predict)
                    .clamp(timeline.air_secs, action.tuning.max_air_hold);
                action.slow =
                    (timeline.air_secs / predicted_air).clamp(action.tuning.slow_min, 1.0);
                // 只给本地 Dynamic 写入一次起跳初速度；Kinematic 速度组件不参与积分，不写失真值。
                // 预测端是 Dynamic，这里写的就是上面取的联网起跳速度。
                if !kinematic && let Ok(mut velocity) = velocities.get_mut(avatar) {
                    velocity.0.y = launch;
                }
                action.last_root = 0.0;
            }
            action.last_root = action.root_y;
        }
        JumpState::Land => {
            if previous == JumpState::Air {
                if let Ok(mut velocity) = velocities.get_mut(avatar) {
                    velocity.0.y = 0.0;
                }
                // 对齐基准只在接这一帧算一次：之后权重从 Running 滑到 Walking / Idle，
                // 每帧现算会让目标相位当场跳到 Idle 的 0.51，脚看起来就是猛换一下。
                let (target, advance) = landing_alignment(
                    &driver,
                    &catalog,
                    &selection,
                    action.clip_name(),
                    timeline,
                    &clips,
                    &clip_assets,
                );
                action.land_phase_target = target;
                action.land_phase_advance = advance;
                action.land_aligned = true;
            }
            // 恢复段每帧都把交接相位往前推：动作剪辑的尾巴在往前走，移动相位
            // 也得按同样的速度走，冻在原地会让着地脚被身体带着滑一路。
            if action.land_aligned {
                follow_landing_phase(
                    &mut driver,
                    action.land_phase_target,
                    action.land_phase_advance,
                    action.land_time,
                    timeline.recover_secs,
                );
            }
        }
        JumpState::Ground if previous != JumpState::Ground => {
            if let Ok(mut velocity) = velocities.get_mut(avatar) {
                velocity.0.y = 0.0;
            }
            // 不在这里清 `queued`：落地恢复段按下的那一下还挂着，下一帧回到地面就执行
            action.weight = 0.0;
        }
        _ => {}
    }
}

/// 剪辑髋高 → 起跳基线以上的根位移（米）。
///
/// 平地 / 上跳只取基线以上的部分，蹲姿和落地压缩不该把胶囊往地下拽；
/// 下跳取基线以下的有符号部分，胶囊沿曲线落向低台面。
pub(super) fn root_y_at(action: &JumpAction, clip_time: f32) -> f32 {
    let raw = sample_curve(&action.curve, action.curve_step, clip_time) - action.takeoff_hips;
    match action.kind {
        JumpKind::Down => raw.min(0.0),
        JumpKind::Flat | JumpKind::Up => raw.max(0.0),
    }
}

#[allow(clippy::too_many_arguments)]
fn begin(
    action: &mut JumpAction,
    driver: &PoseDriver,
    clips: &ClipSet,
    assets: &Assets<AnimationClip>,
    kind: JumpKind,
    profile: JumpProfile,
    clip: &'static str,
    tuning: JumpTuning,
    take_secs: Option<f32>,
    takeoff_y: f32,
) -> bool {
    let Some(skeleton) = driver.skeleton.as_ref() else {
        warn!("[jump-begin] 失败：骨架未就绪");
        return false;
    };
    let Some(hips) = skeleton.hips else {
        warn!("[jump-begin] 失败：未找到髋骨");
        return false;
    };
    let Some(handle) = clips.handle(clip) else {
        warn!("[jump-begin] 失败：剪辑 {clip} 不在剪辑表");
        return false;
    };
    let Some(asset) = assets.get(handle) else {
        warn!("[jump-begin] 失败：剪辑 {clip} 资产未加载");
        return false;
    };
    let (mut timeline, curve, step) = analyze_clip(clip, asset, hips);
    // 联网：根由服务端抬升，把蓄力段压 / 拉到服务端蓄力时长，离地帧才对得上
    if let Some(secs) = take_secs {
        timeline.set_take_secs(secs);
    }
    // 下蹲低点：离地帧之前髋高最小的那一帧（观测用，不驱动状态）
    let takeoff_idx =
        ((timeline.takeoff / step).max(0.0) as usize).min(curve.len().saturating_sub(1));
    let crouch_low = curve[..=takeoff_idx]
        .iter()
        .enumerate()
        .min_by(|a, b| a.1.partial_cmp(b.1).unwrap_or(std::cmp::Ordering::Equal))
        .map(|(index, _)| index as f32 * step)
        .unwrap_or(0.0);

    action.curve = curve;
    action.curve_step = step;
    action.kind = kind;
    // 档位与参数在起跳这一帧定型：之后即使外部改了 `JumpTunings`，
    // 这一跳的蓄力 / 慢放 / 滞空上限都按快照走，不会被别的档串味。
    action.profile = profile;
    action.tuning = tuning;
    action.clip = clip;
    action.timeline = timeline;
    action.crouch_low = crouch_low;
    action.takeoff_hips = sample_curve(&action.curve, step, timeline.takeoff);
    action.hips_freeze = action.takeoff_hips;
    action.elapsed = 0.0;
    action.clip_time = timeline.entry;
    action.weight = 0.0;
    action.root_y = 0.0;
    action.last_root = 0.0;
    action.air_time = 0.0;
    action.land_time = 0.0;
    action.air_hold = 0.0;
    action.air_lifted = false;
    // 起跳基线：这一跳到底有没有把根抬起来，就从这里开始量
    action.takeoff_y = takeoff_y;
    action.climbed = false;
    action.launch_vy = 0.0;
    action.slow = 1.0;
    action.jump_seq = action.jump_seq.wrapping_add(1);
    action.state = JumpState::Takeoff;
    action.queued = false;
    action.queued_for = 0.0;
    action.land_aligned = false;
    true
}
