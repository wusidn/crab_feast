//! 跳跃动作层：一次性动作叠在移动混合空间之上，不参与参数场。
//!
//! 移动混合空间只装循环步态（Idle / Walk / Run / 侧移 / 倒退），它的采样
//! 权重同时决定地速和支撑权重，所以一次性动作不能放进去。这里单独一层：
//! 自己的播放头、自己的时间轴、自己的垂直位移，播完把权重交回移动混合。
//!
//! # 起跳分流（起跳瞬间判定一次，整段动作不再切换）
//!
//! 1. **高差优先**：从脚下与前方落点各打一条向下的地面射线。落点比当前地面
//!    高过 [`STEP_HEIGHT`] → [`JumpingUp`](UP_CLIP)；低过阈值 → [`JumpingDown`](DOWN_CLIP)；
//!    其余按平地跳处理。
//! 2. **平地跳再按速度分档**：地速低于 [`RUN_JUMP_SPEED`] 用原地双脚跳
//!    [`Jumping`](STATIC_CLIP)；处于奔跑档用 [`RunningJump`](RUN_CLIP)，
//!    并按起跳支撑脚选左 / 右派生版本（[`RUN_CLIP_LEFT`] / [`RUN_CLIP_RIGHT`]，
//!    由 `scripts/make_runjump_clips.py` 程序化生成）。
//!
//! # 时间轴锚点（`scripts/probe_jump_clips.py` 实测，Amy 标定）
//!
//! Mixamo 逐帧重定向，跨角色时长与归一化相位一致。锚点是剪辑本地秒，
//! 运行时 `begin` 会再用髋高曲线跑一遍基线事件检测校正，检测不到才回退表内默认值：
//!
//! | 剪辑 | 时长 | 离地 | 落地 | 蓄力段 | 说明 |
//! |---|---|---|---|---|---|
//! | `Jumping` | 1.900s | 0.543 | 1.116 | 1:1 保留 | 先下蹲（低点 ~0.30s）再蹬伸，落地后恢复 |
//! | `RunningJump` | 0.900s | 0.029 | 0.657 | 1:1 保留 | 起手即蹬伸，几乎无蓄力 |
//! | `JumpingUp` | 0.833s | 0.516 | 0.833 | 1:1 保留 | 蹲姿后登上高台，落地即结束 |
//! | `JumpingDown` | 3.067s | 0.876 | 1.168 | 压缩 | 高台准备段后下落，长尾恢复 |
//!
//! 地面深蹲类剪辑的蓄力段（下蹲→蹬伸）按剪辑原速播放，不再压缩。
//! 滞空段是否结束看物理接地（脚下射线 + 残余上升速度归零），不按剪辑固定时长提前退出：
//! 整段腾空弧线按预测滞空慢放拉伸，播完钉在落地帧等接地，接地后才播放压缩的落地恢复段并淡出。
//!
//! # 起跳失败（表现层兜底）
//!
//! 写进物理的离地速度不等于人真的离地：顶棚、贴过来的其他刚体、贴墙的摩擦都会在
//! 接触求解里把它当场吃掉。这种情况下人还站在原处，动作层不能继续锁在滞空姿态上
//! 等 [`MAX_AIR_HOLD_SECS`] 的兜底超时——那两秒玩家看到的是「按了跳，人卡在半空」。
//! 判定条件见 [`takeoff_failed`]：根没抬起来、脚底也没离地、人仍然贴地，窗口一到就进
//! [`JumpState::Abort`]：姿态定格在剪辑的离地帧（不跳到落地恢复段），在
//! [`ABORT_BLEND_SECS`] 内把权重淡到零交回移动混合，不演一次假的落地缓冲。
//!
//! 联网时位置由服务端复制，客户端只用复制下来的权威轨迹做同一个判定——服务端自己
//! 也会在写速后的验证窗口里退出跳跃状态（见 `launcher/server`），客户端这里负责的是
//! 表现层：动画必须跟着服务端的结果一起收，不能各演各的。
//!
//! # 垂直位移
//!
//! 进入滞空那一帧写入一次起跳初速度，之后垂直轨迹交回重力自然抛物，不再用被慢放
//! 拉伸的髋曲线差分去覆写物理速度；动画只负责姿态，同时空中把 hips 骨骼的竖直平移
//! 钉在起跳高度。跳上 / 跳下平台的净高差由曲线相对起跳帧的有符号偏移承担：
//! 平地 / 上跳偏移不低于 0，下跳偏移不高于 0。

use avian3d::prelude::{LinearVelocity, RigidBody, SpatialQuery, SpatialQueryFilter};
use bevy::animation::{AnimationClip, AnimationTargetId};
use bevy::prelude::*;
use bevy_animation::animated_field;

use super::AnimationSet;
use super::character::{AnimCatalog, AnimSelection, ClipGait};
use super::core::ClipSet;
use super::core::pose::Pose;
use super::driver::PoseDriver;
use super::mixers::{LocomotionMixer, RegisteredMixer};
use crate::ground::{GROUND_CONTACT_DIST, PROBE_RISE, foot_ground_dist, ground_height};
use crate::input::{JumpInput, LocomotionIntent};
use crate::scene::Avatar;

/// 原地双脚跳（静态 / 慢速）
const STATIC_CLIP: &str = "Jumping";
/// 奔跑跳基础剪辑（原片：右腿支撑）
const RUN_CLIP: &str = "RunningJump";
/// 奔跑跳派生：左腿支撑
const RUN_CLIP_LEFT: &str = "RunningJumpLeft";
/// 奔跑跳派生：右腿支撑
const RUN_CLIP_RIGHT: &str = "RunningJumpRight";
/// 跳上高台
const UP_CLIP: &str = "JumpingUp";
/// 跳下高台
const DOWN_CLIP: &str = "JumpingDown";

/// 判定「哪只脚在支撑」的容差：两只脚的支撑权重差不到这个值就当作双脚起跳
const CONTACT_EPSILON: f32 = 0.05;
/// 平地跳 / 奔跑跳的地速分界（m/s）。低于它按原地静态跳处理；
/// 走路档（~1.15）落在静态侧，奔跑档（~3.0）落在奔跑跳侧。
const RUN_JUMP_SPEED: f32 = 1.5;
/// 台阶高度阈值（米）：落点与当前地面高差绝对值超过它才判上 / 下平台。
/// 普通小碎步高差在几厘米内，1m 高的关卡台阶远超此值。
const STEP_HEIGHT: f32 = 0.25;
/// 落点向前探测距离（米）：沿当前移动 / 朝向方向前瞻一段再打地面射线。
const LAND_PROBE_DIST: f32 = 0.0; // TEMP

/// 进入动作层的剪辑本地时间：与 Running 的最低髋点对齐，避免「先挺直再蹲」
const STOP_SPEED: f32 = 0.6;
/// 排队最长等待（秒）。相位窗口只是把起跳**推迟**到步态合适的那一帧，
/// 不是把请求吞掉：等满一个步幅周期还没等到窗口，就照常起跳，宁可不完美
/// 也不让玩家觉得按键失灵。
const QUEUE_MAX_WAIT: f32 = 0.55;

/// 进入时从移动混合融进来的时长。
///
/// 起跳前腿要从「跑步跨步的一条腿在前一条在后」收到「双脚并拢下蹲」，
/// 0.08s 只有 5 帧，腿的角速度接近 3 rad/s，看着就是绊一下。0.14s 才是可读的"收腿"。
///
/// 但这只是上限：淡入**必须在离地帧之前跑完**（见 [`blend_for`]）。否则物理已经把
/// 人抬起来了，姿态还停在跑步循环里——跑跳的蓄力段只有 0.05s，慢 0.09s 就是
/// 「人已经飞起来了还在跑」。
const BLEND_IN_SECS: f32 = 0.14;

/// 淡入最短时长（一帧半）：压得再狠也不能在一帧里把姿态硬切过去。
const MIN_BLEND_SECS: f32 = 0.02;

/// 起手淡入时长：不超过本档的蓄力段（`take_secs`），保证离地那一帧姿态已经换完。
fn blend_for(take_secs: f32) -> f32 {
    BLEND_IN_SECS.min(take_secs.max(MIN_BLEND_SECS))
}

/// 髋曲线的采样频率（只采一个通道，一次性开销）
const CURVE_HZ: f32 = 120.0;

/// 起跳窗口：起跳腿进入支撑后的这段时间。
const CONTACT_WINDOW: f32 = 0.14;

/// 跑步跳单脚着地判定：踩实脚支撑权重 ≥ 此值，另一脚 ≤ 此值。
/// 双脚同时踩实、双脚腾空、或权重模糊（过渡相）都不放行。
const SUPPORT_ON: f32 = 0.50;
const SUPPORT_OFF: f32 = 0.35;
/// 起跳窗口在支撑相里的进度区间（0=触地，1=离地）。
///
/// 早于此值的「触地初期」不放行：跑跳剪辑的入场帧是「支撑腿已蹬直在后、
/// 摆动腿已前摆到大步」，而跑步在触地初期摆动腿还在体后（实测前后错位差
/// 0.24m），在那一刻切剪辑，摆动腿会"抢半步"——看上去就是在空中走了一步。
/// 拖到支撑相后段（蹬离前）再起跳，两段的前后错位差降到 0.04m，
/// 摆动腿高度差 9cm（0.05s 淡入可以吃下）。
///
/// 晚于此值不放行：脚已经离地，没有再蹬的支撑点。
const TAKEOFF_EARLY: f32 = 0.55;
const TAKEOFF_LATE: f32 = 0.95;

/// 空中速度补偿用的重力（Avian 默认 -9.81）
const GRAVITY: f32 = 9.81;

/// 接地判定允许的残余上升速度（m/s）。
///
/// 贴地时 avian 的接触求解会留下 ±0.2 m/s 量级的残余速度（实测 0.192、
/// -0.135 反复出现）。拿 0.1 当「已经蹬升」的界线，会把这种噪声当成起跳
/// 初速（`v²/2g` 只有几毫米）或把接地判成悬空。界线要拉开噪声一个量级。
const GROUND_NOISE_VY: f32 = 0.5;
/// 起跳宽限：进入滞空后这段时间内不做接地判定，避免蹬离瞬间射线仍贴地误判落地。
const AIR_GRACE_SECS: f32 = 0.08;
/// 确认离地的最小高度（米）：脚底距地超过它才算真的离开地面。
/// 滞空段要等到「确实抬起来过」才允许判定落地，防止小速度起跳时在贴着地面
/// 的第 0.08 秒就被判落地、整段跳跃变成抖一下。
const AIR_LIFT_DIST: f32 = 0.18;
/// 空中姿态保持的兜底上限：射线失效 / 落在高台边缘探测不到地面时，超时强制结束滞空段。
const MAX_AIR_HOLD_SECS: f32 = 2.0;
/// 起跳失败判定窗口（单机 Dynamic，秒）：进入滞空段这么久，根没抬起来、脚底也没离地，
/// 就说明这一跳被物理吃回去了（顶棚 / 贴身的别的刚体 / 摩擦卡住）。
///
/// 不能等 [`MAX_AIR_HOLD_SECS`] 兜底：那两秒里人明明站在地上，动画却一直锁在
/// 滞空姿态上，玩家看到的是「按了跳，人卡在半空不动」。
const TAKEOFF_FAIL_SECS: f32 = 0.15;
/// 起跳失败判定窗口（联网 Kinematic，秒）：根位置由服务端复制、比按键晚半个 RTT 才到，
/// 窗口要留够，别把一次真起跳错判成失败。
const NET_TAKEOFF_FAIL_SECS: f32 = 0.25;
/// 「根真的升起来了」的位移阈值（米）：与 [`AIR_LIFT_DIST`] 同口径。
///
/// 不能用「抬了一点点」当成功：顶棚只差几厘米时，离散求解器在起跳那一 tick 里
/// 仍能让人穿进去几厘米再被推回来，那不是一次起跳，是撞顶。真要算成功，
/// 根得抬到脚离地那一步——低于它，玩家看到的就是「蹲一下、跳一下、人还在原地」。
const TAKEOFF_RISE_EPS: f32 = AIR_LIFT_DIST;
/// 起跳失败后动作层的淡出时长（秒）：这一跳没有落地冲击可演，不走完整的落地恢复段。
const ABORT_BLEND_SECS: f32 = 0.08;
/// 本地 Dynamic 角色的起跳初速度（m/s）。
///
/// 这是**下限**而不是兜底：实测速度只有在明显高于它（真的被别的力弹起来）
/// 时才采用，噪声级的小正值一律按这个速度起跳。低于它的起跳会变成几厘米的
/// 抽搐——「按了跳，人只抬了一下」。
const LOCAL_LAUNCH_VY: f32 = 5.0;
/// 远端 Kinematic 复制体无速度组件，按服务端高跳速度预测滞空（m/s）。
const REMOTE_LAUNCH_VY: f32 = 5.0;
/// 滞空预测系数：对称抛物线 `2·vy/g` 再乘这个缓冲。
///
/// 实测联网跑跳（起跳 5.0 m/s、阻尼 0.3）真正滞空 0.95s，而 `2·5/9.81`
/// = 1.02s；乘 1.15 会预测成 1.17s，剪辑整段腾空被压成 0.54× 慢放，
/// **落地那一刻剪辑才播到 82%**：脚还在"往前伸着没踩实"的姿态上，收尾
/// 0.12s 被丢掉——画面就是人已经落地了，腿还在空中多迈一步。
/// 取 0.95 让剪辑的落地帧与真实触地落在同一时刻，剩下的误差由
/// 「钉在落地帧等接地」兜住。
const AIR_PREDICT_FACTOR: f32 = 0.95;
/// 远端观察起跳的最小上升速度（m/s）：走空坠落是负速度，不该播起跳动作。
const REMOTE_JUMP_DETECT_VY: f32 = 1.0;
/// 联网时本地角色的蓄力段时长（秒），与服务端 `drive_avatars` 的两档蓄力一致。
/// 根位置由服务端抬升，动画的离地帧必须落在同一时刻，否则「人已经飞起来还在蹲」。
const NET_STAND_TAKE_SECS: f32 = 0.45;
const NET_RUN_TAKE_SECS: f32 = 0.05;
/// 空中姿态慢放系数下限：再低就成慢镜头了。
const SLOW_MIN: f32 = 0.35;

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
/// [`JumpAction`]，空中改表不会影响已经在飞的这一跳。
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum JumpProfile {
    /// 平地站立 / 慢走起跳（`Jumping`）
    #[default]
    Stand,
    /// 奔跑起跳（`RunningJump` / 左 / 右派生）
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
            JumpKind::Flat if ground_speed >= RUN_JUMP_SPEED => JumpProfile::Run,
            JumpKind::Flat => JumpProfile::Stand,
        }
    }
}

/// 单档跳跃的全部手感旋钮。值都是「这一档自己的」，改哪一档都不会顺手改别的档。
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

impl JumpTuning {
    /// 站立 / 慢走跳：剪辑 `Jumping` 实测离地 0.517s，联网按 0.45s 蓄力对齐。
    pub const STAND: Self = JumpTuning {
        launch_vy: LOCAL_LAUNCH_VY,
        net_launch_vy: REMOTE_LAUNCH_VY,
        take_secs: Some(NET_STAND_TAKE_SECS),
        charge_secs: NET_STAND_TAKE_SECS,
        air_predict: AIR_PREDICT_FACTOR,
        slow_min: SLOW_MIN,
        max_air_hold: MAX_AIR_HOLD_SECS,
    };

    /// 奔跑跳：剪辑起手即蹬伸，蓄力只留 0.05s。
    pub const RUN: Self = JumpTuning {
        launch_vy: LOCAL_LAUNCH_VY,
        net_launch_vy: REMOTE_LAUNCH_VY,
        take_secs: Some(NET_RUN_TAKE_SECS),
        charge_secs: NET_RUN_TAKE_SECS,
        air_predict: AIR_PREDICT_FACTOR,
        slow_min: SLOW_MIN,
        max_air_hold: MAX_AIR_HOLD_SECS,
    };

    /// 跳上高台：`JumpingUp` 蹲姿到离地 0.516s 是**这一档自己的节奏**，
    /// 不跟平地的 0.45s 走（那会让「人已经飞起来还在蹲」）。
    pub const UP: Self = JumpTuning {
        launch_vy: LOCAL_LAUNCH_VY,
        net_launch_vy: REMOTE_LAUNCH_VY,
        take_secs: Some(0.516),
        charge_secs: 0.516,
        air_predict: AIR_PREDICT_FACTOR,
        slow_min: SLOW_MIN,
        max_air_hold: MAX_AIR_HOLD_SECS,
    };

    /// 跳下高台：剪辑的高台准备段本来就压缩到 0.16s，别再拉回平地的 0.45s。
    pub const DOWN: Self = JumpTuning {
        launch_vy: LOCAL_LAUNCH_VY,
        net_launch_vy: REMOTE_LAUNCH_VY,
        take_secs: Some(COMPRESSED_TAKE_SECS),
        charge_secs: COMPRESSED_TAKE_SECS,
        air_predict: AIR_PREDICT_FACTOR,
        slow_min: SLOW_MIN,
        max_air_hold: MAX_AIR_HOLD_SECS,
    };
}

impl Default for JumpTuning {
    fn default() -> Self {
        JumpTuning::STAND
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
        Self([
            JumpTuning::STAND,
            JumpTuning::RUN,
            JumpTuning::UP,
            JumpTuning::DOWN,
        ])
    }
}

/// 蓄力段被压缩时的目标时长（秒）：只用于不需要展示深蹲的剪辑（下跳高台准备）。
const COMPRESSED_TAKE_SECS: f32 = 0.16;

/// 髋高基线事件检测阈值（米）：髋高抬离首帧基线超过这个值判为离地。
const EVENT_BASELINE: f32 = 0.03;

/// 单条跳跃剪辑的静态策略：默认事件锚点 + 各段压缩策略。
///
/// 锚点是剪辑本地秒，取自探针离线实测；运行时 `begin` 会用髋高曲线
/// 再校一遍（事件检测），检测不到才回退这里的默认值。
#[derive(Clone, Copy, Debug)]
struct JumpSpec {
    /// 剪辑本地时间：动作起点（站立 / 预备帧）
    entry: f32,
    /// 默认离地帧（双脚蹬离地面）
    takeoff: f32,
    /// 默认落地帧（双脚回到地面）
    land: f32,
    /// 剪辑结束帧
    end: f32,
    /// 蓄力段是否 1:1 保留剪辑时长：地面深蹲要按原速看到下蹲→蹬伸；
    /// 高台准备段（下跳）不展示深蹲，可以压缩。
    preserve_anticipation: bool,
    /// 落地恢复段压缩后的目标时长（秒）
    recover_secs: f32,
}

/// `Jumping` 1.900s：下蹲低点 ~0.30s，离地 0.543，顶点 0.754，落地 1.116。
static SPEC_STATIC: JumpSpec = JumpSpec {
    entry: 0.0,
    takeoff: 0.543,
    land: 1.116,
    end: 1.900,
    preserve_anticipation: true,
    recover_secs: 0.22,
};

/// `RunningJump` 0.900s：起手即蹬伸，离地 0.029，顶点 0.329，落地 0.657。
static SPEC_RUN: JumpSpec = JumpSpec {
    entry: 0.0,
    takeoff: 0.029,
    land: 0.657,
    end: 0.900,
    preserve_anticipation: true,
    // 恢复段 = 落地缓冲段的长度（end − land = 0.243s），让这段按 1× 播完。
    // 原来拿 0.15s 播 0.243s 的缓冲，相当于 1.62× 加速：左脚踩实到右脚踩实
    // 只有 0.13s 被压到 0.08s，且着地脚在淡出里被身体拖着滑 3 m/s。
    recover_secs: 0.24,
};

/// `JumpingUp` 0.833s：蹲姿后蹬上高台，离地 0.516，落地即剪辑结尾。轨迹固定。
static SPEC_UP: JumpSpec = JumpSpec {
    entry: 0.0,
    takeoff: 0.516,
    land: 0.833,
    end: 0.833,
    preserve_anticipation: true,
    recover_secs: 0.05,
};

/// `JumpingDown` 3.067s：高台蓄力到 0.876 离地，1.168 落地，长尾恢复到 3.067。轨迹固定。
static SPEC_DOWN: JumpSpec = JumpSpec {
    entry: 0.0,
    takeoff: 0.876,
    land: 1.168,
    end: 3.067,
    preserve_anticipation: false,
    recover_secs: 0.30,
};

/// 一次跳跃解析后的时间轴：默认锚点经曲线事件检测校正，段时长按策略折算。
#[derive(Clone, Copy, Debug, Default)]
struct Timeline {
    entry: f32,
    /// 离地事件帧（剪辑本地秒）
    takeoff: f32,
    /// 落地事件帧（剪辑本地秒）
    land: f32,
    end: f32,
    /// 蓄力段动作时长（秒）：下蹲→蹬伸
    take_secs: f32,
    /// 滞空段动作时长（秒）：1:1 保留剪辑
    air_secs: f32,
    /// 落地恢复段动作时长（秒）：压缩后
    recover_secs: f32,
    /// 起手淡入时长（秒）：永远不超过 `take_secs`，离地帧姿态已就位
    blend_secs: f32,
}

impl Timeline {
    /// 用静态策略 + 实测髋高曲线解析出本次动作的时间轴。
    fn build(spec: &JumpSpec, curve: &[f32], step: f32, duration: f32) -> Self {
        let (takeoff, land) = if spec.preserve_anticipation {
            detect_events(spec, curve, step, duration)
        } else {
            (spec.takeoff, spec.land)
        };
        // 事件顺序兜底：落地不能早于离地
        let land = land.max(takeoff);
        let take_secs = if spec.preserve_anticipation {
            (takeoff - spec.entry).max(0.0)
        } else {
            COMPRESSED_TAKE_SECS
        };
        let air_secs = (land - takeoff).max(0.0);
        Self {
            entry: spec.entry,
            takeoff,
            land,
            end: spec.end,
            take_secs,
            air_secs,
            recover_secs: spec.recover_secs,
            blend_secs: blend_for(take_secs),
        }
    }

    /// 把蓄力段对齐到外部时长（联网跟服务端蓄力走）。
    ///
    /// 淡入要同步压进新的蓄力段：服务端按 0.05s 就把人蹬起来了，
    /// 姿态却还要 0.14s 才换完，那 0.09s 就是「脚离地了还在跑」。
    fn set_take_secs(&mut self, secs: f32) {
        self.take_secs = secs.max(0.0);
        self.blend_secs = blend_for(self.take_secs);
    }

    /// 剪辑时间轴上的理论总时长（秒）。实际滞空段由物理接地信号决定，
    /// 不再按这个时长提前退出；仅测试锚点时参照。
    #[allow(dead_code)]
    fn total_secs(self) -> f32 {
        self.take_secs + self.air_secs + self.recover_secs
    }

    /// 按状态与段进度求剪辑本地时间。
    ///
    /// - `Takeoff`：蓄力段 `entry → takeoff`；
    /// - `Air`：整段腾空弧线 `takeoff → land` 按 `slow` 慢放拉伸；滞空比剪辑
    ///   长时钉在落地帧，等接地信号，不回绕、不循环；
    /// - `Land`：接地后恢复段 `land → end`；
    /// - `Abort`：起跳失败，定格在离地帧 `takeoff`（**不**取 `land`），权重由动作层
    ///   在 [`ABORT_BLEND_SECS`] 内淡出，姿态不回弹、也不假装落地。
    fn clip_time(
        self,
        state: JumpState,
        elapsed: f32,
        air_time: f32,
        land_time: f32,
        slow: f32,
    ) -> f32 {
        match state {
            JumpState::Takeoff => {
                if self.take_secs > f32::EPSILON {
                    let k = (elapsed / self.take_secs).clamp(0.0, 1.0);
                    self.entry + k * (self.takeoff - self.entry)
                } else {
                    self.takeoff
                }
            }
            JumpState::Air => {
                if self.air_secs > f32::EPSILON {
                    let k = (air_time * slow) / self.air_secs;
                    self.takeoff + k.clamp(0.0, 1.0) * (self.land - self.takeoff)
                } else {
                    self.land
                }
            }
            JumpState::Land | JumpState::Ground => {
                if self.recover_secs > f32::EPSILON {
                    let k = (land_time / self.recover_secs).clamp(0.0, 1.0);
                    self.land + k * (self.end - self.land)
                } else {
                    self.end
                }
            }
            // 起跳失败：定格在离地那一帧的姿态。取 `land` 会让人在半空摆出踩地
            // 的恢复姿势，再在这上面淡出——玩家看到的是一次凭空的「落地回弹」。
            JumpState::Abort => self.takeoff,
            JumpState::Queued => self.entry,
        }
    }

    /// 按状态求动作层权重：蓄力 / 滞空保持淡入后的权重，落地恢复段淡出交回移动混合。
    ///
    /// `Abort` 的权重不在这里算：失败那一帧要从**上一帧**的权重开始往下淡，这里返回
    /// 0 会把淡出起点直接抹掉，所以由动作层用 [`ABORT_BLEND_SECS`] 逐帧递减。
    fn weight(self, state: JumpState, elapsed: f32, land_time: f32) -> f32 {
        let fade_in = (elapsed / self.blend_secs.max(f32::EPSILON)).clamp(0.0, 1.0);
        match state {
            JumpState::Takeoff | JumpState::Air => fade_in,
            JumpState::Land => {
                let k = (land_time / self.recover_secs.max(f32::EPSILON)).clamp(0.0, 1.0);
                (fade_in * (1.0 - k)).clamp(0.0, 1.0)
            }
            JumpState::Abort | JumpState::Ground | JumpState::Queued => 0.0,
        }
    }
}

/// 髋高基线法检测离地 / 落地事件（剪辑本地秒）；检测不到回退 `spec` 默认锚点。
///
/// 离地：髋高第一次抬离首帧基线超过 [`EVENT_BASELINE`]；落地：顶点之后
/// 髋高回到基线附近。登上高台这类末帧仍高于基线的剪辑找不到落地帧，
/// 由调用方回退默认值。
fn detect_events(spec: &JumpSpec, curve: &[f32], step: f32, duration: f32) -> (f32, f32) {
    if curve.len() < 4 {
        return (spec.takeoff, spec.land);
    }
    let base = curve[0];
    let takeoff = curve
        .iter()
        .position(|&y| y > base + EVENT_BASELINE)
        .map(|i| (i as f32 * step).clamp(spec.entry, duration))
        .unwrap_or(spec.takeoff);

    let apex = curve
        .iter()
        .enumerate()
        .max_by(|a, b| a.1.partial_cmp(b.1).unwrap_or(std::cmp::Ordering::Equal))
        .map(|(i, _)| i)
        .unwrap_or(0);
    let land = (apex + 1..curve.len())
        .find(|&i| curve[i] <= base + EVENT_BASELINE)
        .map(|i| (i as f32 * step).clamp(takeoff, duration))
        .unwrap_or(spec.land);

    (takeoff, land)
}

/// 按剪辑名取静态策略。
fn spec_for(clip: &str) -> &'static JumpSpec {
    match clip {
        UP_CLIP => &SPEC_UP,
        DOWN_CLIP => &SPEC_DOWN,
        RUN_CLIP | RUN_CLIP_LEFT | RUN_CLIP_RIGHT => &SPEC_RUN,
        _ => &SPEC_STATIC,
    }
}

/// 起跳时机策略。等窗口换来的是"动作精确"，付出的是跟手。
// 三档由 `JumpPolicy` 资源在运行时切换，另外两档暂时没有调用方
#[allow(dead_code)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum JumpTiming {
    /// 按下就起跳，不等任何相位。脚可能从摆动相起跳，靠融合时长盖过去。
    #[default]
    Immediate,
    /// 等任意一只脚进入支撑：跑最坏 0.30s，走最坏 0.18s。
    AnySupport,
    /// 等起跳腿进入支撑的前 0.14s：跑最坏 0.70s。
    PushOffFoot,
}

/// 跳跃的策略开关。默认立即起跳；换这个资源就能在三种策略间切换，
/// 不用改 `window_open` 的实现。
#[derive(Resource, Clone, Copy, Debug, Default)]
pub struct JumpPolicy {
    pub timing: JumpTiming,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum JumpState {
    /// 脚在地面上，移动混合完全接管
    #[default]
    Ground,
    /// 已收到请求，等起跳窗口（`Immediate` 下最多停留一帧）
    Queued,
    /// 蓄力 + 蹬伸：根仍在地面，姿态从移动融进蹲姿
    Takeoff,
    /// 滞空：垂直由髋曲线驱动，水平保持起跳速度
    Air,
    /// 落地冲击 + 融回移动
    Land,
    /// 起跳被物理吃掉：姿态冻在离地帧，快速淡出交回移动混合。
    ///
    /// 这一跳没有落地冲击可演，所以**不走** [`JumpState::Land`] 的恢复段——那一段是
    /// 按真实触地设计的，硬播一次就是凭空多出来的一个落地缓冲。
    Abort,
}

/// 跳跃动作层的全部状态。资源是单实例的：本作的动画栈本来就只有一个本地角色。
#[derive(Resource)]
pub struct JumpAction {
    pub state: JumpState,
    /// 动作层对最终姿态的权重（0 = 纯移动混合）
    pub weight: f32,
    /// 当前在剪辑本地时间轴上的位置
    pub clip_time: f32,
    /// 空中 hips 骨骼要钉住的竖直平移（= 起跳时的髋高）
    pub hips_freeze: f32,
    /// 相对起跳高度已经升了多少（米）；下跳为负值
    pub root_y: f32,
    /// 本次跳跃的大类（平地 / 上 / 下）。
    pub kind: JumpKind,
    /// 本次跳跃走的调参档（站 / 跑 / 上 / 下）。
    pub profile: JumpProfile,
    /// 起跳那一帧快照下来的整档参数：空中改表不会影响这一跳。
    pub tuning: JumpTuning,

    /// 动作层已播放的总时间（用于淡入；滞空段时长由物理接地决定，不封顶剪辑时长）
    elapsed: f32,
    /// 请求已锁存，等窗口
    queued: bool,
    queued_for: f32,
    /// 这一次按键已经被消费，必须先松开才能再请求
    hold_latched: bool,

    /// 剪辑髋高曲线（米，按 CURVE_HZ 采样）
    curve: Vec<f32>,
    curve_step: f32,
    /// 这一次用的是哪条剪辑
    clip: &'static str,
    /// 本次动作解析后的时间轴（事件锚点 + 段时长）
    timeline: Timeline,
    /// 下蹲低点帧（剪辑本地秒），离地前髋高极小值；仅用于观测
    crouch_low: f32,
    /// 起跳时的髋高，作为根位移的基线
    takeoff_hips: f32,
    /// 上一帧的根位移，用来求竖直速度
    last_root: f32,

    /// 滞空段累计时长：空中姿态按慢放系数推进到接地。
    air_time: f32,
    /// 落地恢复段进度（0..=recover_secs）：接地后才开始播缓冲、淡出交回移动混合。
    land_time: f32,
    /// 滞空段累计时长（含姿态保持），用于宽限判定和兜底超时。
    air_hold: f32,
    /// 起跳那一帧的根高度（世界 Y）：判断这一跳有没有真的把根抬起来。
    takeoff_y: f32,
    /// 根是否已经明显升离 [`JumpAction::takeoff_y`]：起跳成功的直接证据。
    climbed: bool,
    /// 滞空期间是否已经确认离开地面（脚底距地 > [`AIR_LIFT_DIST`]）。
    /// 没抬起来过就不判落地：小速度起跳时人还贴着地面，射线会立刻判「接地」。
    air_lifted: bool,
    /// 进入滞空帧记录的起跳上升速度（m/s）；用于预测滞空时长。
    launch_vy: f32,
    /// 空中姿态慢放系数（≤1）：预测滞空比剪辑空中段越长，放得越慢。
    slow: f32,
    /// 起跳请求序号：每 `begin` 一次自增，随上行快照发给服务端。
    ///
    /// 线上传的是状态快照，只靠 `jump` 电平推上升沿会丢（两帧之间的回落可能被
    /// 合包压平，落地恢复段里的第二跳还会跟上一跳共用一个电平）。把序号放进每条
    /// 快照，服务端比较 `prev_seq != seq` 就认得出新的一跳。
    jump_seq: u8,
    /// 落地恢复段的相位对齐基准，**进入 Land 那一帧算一次**，之后整段不变。
    ///
    /// 每帧按「当前权重最大的移动剪辑」现算会让目标相位跟着权重跳：跑着落地
    /// 时人一停，混合从 Running 滑到 Walking / Idle，目标相位从跑姿的触点（0.066）
    /// 跳到 Idle 的 0.51 —— 恢复段中间那一帧相位会瞬移半圈，脚当场换一只。
    land_phase_target: f32,
    land_phase_advance: f32,
    /// 对齐基准算过没有（`begin` 时清掉，进 Land 时置真）。
    land_aligned: bool,
}

impl Default for JumpAction {
    fn default() -> Self {
        Self {
            state: JumpState::default(),
            weight: 0.0,
            clip_time: 0.0,
            hips_freeze: 0.0,
            root_y: 0.0,
            kind: JumpKind::default(),
            profile: JumpProfile::default(),
            tuning: JumpTuning::default(),
            elapsed: 0.0,
            queued: false,
            queued_for: 0.0,
            hold_latched: false,
            curve: Vec::new(),
            curve_step: 1.0 / CURVE_HZ,
            clip: STATIC_CLIP,
            timeline: Timeline::build(&SPEC_STATIC, &[], 1.0 / CURVE_HZ, SPEC_STATIC.end),
            crouch_low: 0.0,
            takeoff_hips: 0.0,
            last_root: 0.0,
            air_time: 0.0,
            land_time: 0.0,
            air_hold: 0.0,
            takeoff_y: 0.0,
            climbed: false,
            air_lifted: false,
            launch_vy: 0.0,
            slow: 1.0,
            jump_seq: 0,
            land_phase_target: 0.0,
            land_phase_advance: 0.0,
            land_aligned: false,
        }
    }
}

impl JumpAction {
    /// 双脚是否离地。空中不接管水平输入、也不让脚 IK 钉脚。
    pub fn is_airborne(&self) -> bool {
        self.state == JumpState::Air
    }

    /// 动作层是否在接管姿态（蓄力 / 滞空 / 落地都算）。
    ///
    /// 这三个阶段脚 IK 都不能按移动循环的支撑事件去钉：剪辑的双脚此时有自己的
    /// 轨迹，钉住一只、放开另一只，正好把「两脚并拢起跳」变成「一脚踩地一脚抬」。
    ///
    /// 排队态 [`JumpState::Queued`] 不算接管：角色还在地面正常跑，接触权重和脚 IK
    /// 必须按移动循环继续算；否则排队期间接触权重被清零，跑步起跳窗口读到全零
    /// 接触就一直判「双腾空」拒绝，形成卡死循环。
    pub fn is_active(&self) -> bool {
        !matches!(self.state, JumpState::Ground | JumpState::Queued)
    }

    /// 当前跳跃用的剪辑名。没走过 `begin` 时（比如单测里直接构造）退回原地跳。
    pub fn clip_name(&self) -> &'static str {
        if self.clip.is_empty() {
            STATIC_CLIP
        } else {
            self.clip
        }
    }

    /// 起跳请求序号：客户端每起跳一次自增，服务端用它认「新的一跳」。
    #[cfg_attr(not(feature = "net"), allow(dead_code))]
    pub fn jump_seq(&self) -> u8 {
        self.jump_seq
    }

    /// 请求是否还挂着（已锁存、尚未起跳）。联网上行用它决定是否先让出电平。
    ///
    /// 必须看锁存位，不能只看状态：落地恢复段（[`JumpState::Land`]）里按下时状态仍是
    /// `Land`，上行电平本来就是 true；等回到 `Ground` 窗口已经开着，状态一路
    /// `Ground → Takeoff` 都不回落——服务端看不到上升沿，这一跳只有客户端在演。
    #[cfg_attr(not(feature = "net"), allow(dead_code))]
    pub fn is_queued(&self) -> bool {
        self.queued || self.state == JumpState::Queued
    }
}

pub struct JumpPlugin;

impl Plugin for JumpPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<JumpAction>()
            .init_resource::<JumpPolicy>()
            .init_resource::<JumpTunings>()
            // 锁存按键要排在动作层之前：同一帧按下就在这一帧参与窗口判定
            .add_systems(
                Update,
                latch_jump_request
                    .in_set(AnimationSet::Publish)
                    .run_if(in_state(crate::GameState::Game)),
            )
            .add_systems(
                Update,
                drive_jump
                    .in_set(AnimationSet::Drive)
                    .run_if(in_state(crate::GameState::Game)),
            );
    }
}

/// 按键 → 请求。边沿锁存：按住不会在落地后立刻再跳一次。
///
/// 落地恢复段（[`JumpState::Land`]）里按下也算数：那 0.2 秒里人看着已经站稳了，
/// 这一下若被丢掉，玩家只会觉得「按了没反应」。请求先挂起，回到地面就起跳。
/// 起跳失败后的 [`JumpState::Abort`] 同理：人本来就还站在地上，只差几十毫秒淡出。
fn latch_jump_request(input: Res<JumpInput>, mut action: ResMut<JumpAction>) {
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

/// 状态机 + 时间轴 + 垂直位移。跑在 `AnimationSet::Drive`，晚于移动/朝向。
#[allow(clippy::too_many_arguments)]
fn drive_jump(
    time: Res<Time>,
    focus: Res<super::driver::AnimFocus>,
    policy: Res<JumpPolicy>,
    mut driver: ResMut<PoseDriver>,
    catalog: Res<AnimCatalog>,
    selection: Res<AnimSelection>,
    clips: Res<ClipSet>,
    clip_assets: Res<Assets<AnimationClip>>,
    tunings: Res<JumpTunings>,
    mut action: ResMut<JumpAction>,
    avatar_entities: Query<Entity, With<Avatar>>,
    mut velocities: Query<&mut LinearVelocity>,
    rigid_bodies: Query<&RigidBody>,
    transforms: Query<&Transform, With<Avatar>>,
    intents: Query<&LocomotionIntent, With<Avatar>>,
    spatial: SpatialQuery,
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
            || if driver.ground_speed >= RUN_JUMP_SPEED {
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

        let Ok(avatar) = avatar_entities.single() else {
            return;
        };
        let filter = SpatialQueryFilter::from_excluded_entities([avatar]);
        let transform = transforms.single().ok().copied();
        let intent = intents.single().copied().unwrap_or_default();
        let kind = transform
            .map(|t| probe_kind(&spatial, &filter, &t, &intent))
            .unwrap_or_default();
        // 大类 + 地速 → 调参档。四档各一套参数（跑 / 站 / 上 / 下），
        // 之后所有手感旋钮都只从这一次查表结果里取，不再读全局常量。
        let profile = JumpProfile::for_jump(kind, driver.ground_speed);
        // 起跳那一帧脚底距地：联网时这具身体由服务端抬升，这里能看出客户端的
        // 动作是不是比服务端的离地晚（值 > AIR_LIFT_DIST 就是「已经在空中了」）。
        let gd_begin = transform
            .map(|t| foot_ground_dist(&spatial, &filter, &t))
            .flatten();
        let tuning = tunings.get(profile);
        let clip = resolve_clip(kind, &driver, &clips);
        // 起跳基线：这一跳有没有真的把根抬起来，从起跳这一帧的根高度开始量
        let takeoff_y = transform.map_or(0.0, |t| t.translation.y);

        // 联网：这具身体的位置由服务端抬升，蓄力段按服务端的蓄力时长对齐，
        // 让动画的离地帧落在根位置真正开始上升的那一刻。
        // 时长取**这一档自己的**，不再按地速借用平地那两档。
        let kinematic = rigid_bodies.get(avatar).is_ok_and(|rb| rb.is_kinematic());
        let take_secs = kinematic.then_some(tuning.take_secs).flatten();
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
            driver.ground_speed,
            driver.contacts[0],
            driver.contacts[1],
            driver
                .phases
                .get(LocomotionMixer::ID)
                .copied()
                .unwrap_or(-1.0),
            gd_begin.unwrap_or(-1.0),
            if kinematic {
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
    let Some(avatar) = avatar_entities.single().ok() else {
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
                // 实测速度只用来「抬高」：贴地时接触求解会留下 ±0.2 m/s 的噪声，
                // 按它起跳只能升两三厘米，表现就是「按了跳、人只抬了一下」。
                let launch = launch_speed(measured, action.tuning);
                action.launch_vy = launch;
                // Kinematic 位置由服务端同步、本地速度组件失真，按**这一档的联网起跳速度**预测；
                // Dynamic 按本地实测（或兜底）速度预测。两边取值都来自同一份档位表。
                let predict_vy = if kinematic {
                    action.tuning.net_launch_vy
                } else {
                    launch
                };
                let predicted_air = (2.0 * predict_vy / GRAVITY * action.tuning.air_predict)
                    .clamp(timeline.air_secs, action.tuning.max_air_hold);
                action.slow =
                    (timeline.air_secs / predicted_air).clamp(action.tuning.slow_min, 1.0);
                // 只给本地 Dynamic 写入一次起跳初速度；Kinematic 速度组件不参与积分，不写失真值。
                if !kinematic {
                    if let Ok(mut velocity) = velocities.get_mut(avatar) {
                        velocity.0.y = launch;
                    }
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

/// 窗口没开时是否也该放行：等满一个步幅周期就强制起跳。
fn takeoff_allowed(window_open: bool, queued_for: f32) -> bool {
    window_open || queued_for >= QUEUE_MAX_WAIT
}

/// 滞空段是否该判「起跳失败」。
///
/// 三个条件必须同时成立：根没升起来（`climbed == false`）、脚底也没抬离地面过
/// （`air_lifted == false`）、而且人还贴在地上（`grounded`）。任一不成立就说明
/// 这具身体真的飞起来了，按正常滞空走完，绝不提前收尾——宁可晚退，也不能把
/// 一次真起跳的动画剪掉。
fn takeoff_failed(
    air_hold: f32,
    fail_after: f32,
    climbed: bool,
    air_lifted: bool,
    grounded: bool,
) -> bool {
    air_hold >= fail_after && !climbed && !air_lifted && grounded
}

/// 起跳初速度：取实测与**本档**标定值的较大者。
///
/// 贴地时 avian 的接触求解会留下 ±0.2 m/s 的残余竖直速度；若直接把它当起跳
/// 初速（`v²/2g` 只有几毫米），每次跳跃就变成「抬一下」。因此实测只有明显
/// 高于标定值（真的被别的力弹起来）时才采用。标定值来自档位表，跑跳 / 站跳 /
/// 上跳 / 下跳可以各自定高度，互不牵连。
fn launch_speed(measured: f32, tuning: JumpTuning) -> f32 {
    measured.max(tuning.launch_vy)
}

/// 起跳窗口。按策略决定此刻能不能起跳。
fn ready_to_jump(
    timing: JumpTiming,
    driver: &PoseDriver,
    catalog: &AnimCatalog,
    selection: &AnimSelection,
) -> bool {
    if timing == JumpTiming::Immediate {
        return true;
    }
    let Some(phase) = driver.phases.get(LocomotionMixer::ID).copied() else {
        return true;
    };
    let Some(clip) = driver.weights.first().map(|(name, _)| name.as_str()) else {
        return true;
    };
    let Some(gait) = catalog
        .character(selection.character)
        .and_then(|def| def.gaits.get(clip))
    else {
        return true;
    };

    match timing {
        JumpTiming::Immediate => true,
        // 任意一只脚在地上：跑 61% 的周期都在这个区间里，最坏 0.30s 必到
        JumpTiming::AnySupport => {
            phase_in_span(phase, gait.contact_left, gait.lift_left)
                || phase_in_span(phase, gait.contact_right, gait.lift_right)
        }
        // 起跳腿要在支撑期的**前段**：0.16s 的蓄力得塞进剩下的支撑时间里
        JumpTiming::PushOffFoot => match gait.contact_right {
            Some(contact) => (phase - contact).rem_euclid(1.0) < CONTACT_WINDOW,
            None => true,
        },
    }
}

/// 奔跑档起跳窗口：必须单脚着地，且踩实脚处于支撑相前段（重心尚在支撑脚后方）。
///
/// - 双脚同时踩实、双脚腾空、支撑权重模糊 → 不放行；
/// - 踩实脚已进入支撑后段（髋追过支撑脚）→ 不放行，避免「跨出去才蹬」；
/// - 步态相位元数据缺失时，退回单脚着地这一条硬条件，别把起跳彻底卡死。
fn running_takeoff_ready(
    driver: &PoseDriver,
    catalog: &AnimCatalog,
    selection: &AnimSelection,
) -> bool {
    let phase = driver.phases.get(LocomotionMixer::ID).copied();
    let clip = driver.weights.first().map(|(name, _)| name.as_str());
    let gait = match clip {
        Some(clip) => catalog
            .character(selection.character)
            .and_then(|def| def.gaits.get(clip)),
        None => None,
    };
    running_window(driver.contacts, phase, gait)
}

/// 奔跑起跳窗口的纯判定：单脚着地且踩实脚走到支撑相后段（蹬离前）。
///
/// - 双脚同时踩实、双脚腾空、支撑权重模糊 → 不放行；
/// - 踩实脚还在支撑相前段（[`TAKEOFF_EARLY`] 之前）→ 不放行，此时摆动腿仍在
///   体后，切到跑跳剪辑会让摆动腿"抢半步"；
/// - 相位 / 步态元数据缺失时，退回单脚着地这一条硬条件，别把起跳彻底卡死。
fn running_window(contacts: [f32; 2], phase: Option<f32>, gait: Option<&ClipGait>) -> bool {
    let [left, right] = contacts;
    let support = if left >= SUPPORT_ON && right <= SUPPORT_OFF {
        0usize
    } else if right >= SUPPORT_ON && left <= SUPPORT_OFF {
        1
    } else {
        return false;
    };

    let (Some(phase), Some(gait)) = (phase, gait) else {
        return true;
    };
    let (contact, lift) = if support == 0 {
        (gait.contact_left, gait.lift_left)
    } else {
        (gait.contact_right, gait.lift_right)
    };
    let (Some(contact), Some(lift)) = (contact, lift) else {
        return true;
    };
    if !phase_in_span(phase, Some(contact), Some(lift)) {
        return false;
    }
    let span = (lift - contact).rem_euclid(1.0);
    if span <= f32::EPSILON {
        return true;
    }
    let progress = (phase - contact).rem_euclid(1.0) / span;
    (TAKEOFF_EARLY..=TAKEOFF_LATE).contains(&progress)
}

/// 联网上行里 `jump` 电平该发什么。
///
/// 上行是「空口上的按键电平 + 服务端查上升沿」，所以只推迟边沿、不吞请求：
///
/// - 动作执行中（Takeoff / Air / Land）：**永远发 `true`**，按键抬起也不改。
///   电平跟的是动作状态、不是按键：一次跳跃一旦触发就要演完，提前回落会让
///   服务端以为这一跳已经结束；
/// - 已锁存但还没起跳（Queued / 地面等待）：发 `false`，让出电平，起跳那一帧
///   再发上升沿（相位锁定只推迟边沿、不吞请求）；
/// - 回到地面：跟随按键，但**挂着的下一次请求**要先让出电平——否则连跳时
///   两跳共用同一个上升沿，服务端第二跳不会被触发。
///
/// 等窗口超时后动作层会强制起跳，这里自然就把边沿发出去了——**相位锁定
/// 延后跳跃，不禁止跳跃**。
#[cfg_attr(not(feature = "net"), allow(dead_code))]
pub fn uplink_jump_level(state: JumpState, queued: bool, jump_pressed: bool) -> bool {
    match state {
        // 跳跃执行中：锁电平，直到 Land / Abort 演完回到地面才允许变化。
        // Abort 也锁住：这一跳的收尾还挂在客户端，提前回落会让服务端把
        // 「上一跳的余波」和「下一次按键」混成同一个电平。
        JumpState::Takeoff | JumpState::Air | JumpState::Land | JumpState::Abort => true,
        // 还没起跳：让出电平，边沿留到真正起跳那一帧
        JumpState::Queued => false,
        // 地面：跟随按键；锁存着的请求要先让出电平，下一跳才有干净的上升沿
        JumpState::Ground => jump_pressed && !queued,
    }
}

/// 相位是否落在 `[contact, lift]` 这段支撑里（跨 0 也算）
fn phase_in_span(phase: f32, contact: Option<f32>, lift: Option<f32>) -> bool {
    let (Some(contact), Some(lift)) = (contact, lift) else {
        return false;
    };
    let span = (lift - contact).rem_euclid(1.0);
    (phase - contact).rem_euclid(1.0) <= span
}

/// 沿移动 / 朝向方向前瞻探测落点，比较脚下与落点的地面高差。
fn probe_kind(
    spatial: &SpatialQuery,
    filter: &SpatialQueryFilter,
    transform: &Transform,
    intent: &LocomotionIntent,
) -> JumpKind {
    let pos = transform.translation;
    let Some(current) = ground_height(spatial, filter, pos + Vec3::Y * PROBE_RISE) else {
        return JumpKind::Flat;
    };
    let forward = if intent.magnitude > 1e-3 {
        intent.direction.normalize_or_zero()
    } else {
        transform.rotation * Vec3::Z
    };
    let Some(landing) = ground_height(
        spatial,
        filter,
        pos + forward * LAND_PROBE_DIST + Vec3::Y * PROBE_RISE,
    ) else {
        return JumpKind::Flat;
    };

    let delta = landing - current;
    if delta > STEP_HEIGHT {
        JumpKind::Up
    } else if delta < -STEP_HEIGHT {
        JumpKind::Down
    } else {
        JumpKind::Flat
    }
}

/// 按大类 / 速度 / 支撑脚解析本次跳跃用的剪辑名；缺件逐级回退到原地跳。
fn resolve_clip(kind: JumpKind, driver: &PoseDriver, clips: &ClipSet) -> &'static str {
    match kind {
        JumpKind::Up => {
            if clips.handle(UP_CLIP).is_some() {
                UP_CLIP
            } else {
                STATIC_CLIP
            }
        }
        JumpKind::Down => {
            if clips.handle(DOWN_CLIP).is_some() {
                DOWN_CLIP
            } else {
                STATIC_CLIP
            }
        }
        JumpKind::Flat => {
            let candidate = if driver.ground_speed >= RUN_JUMP_SPEED {
                run_clip_for_contacts(driver.contacts)
            } else {
                STATIC_CLIP
            };
            if clips.handle(candidate).is_some() {
                candidate
            } else if clips.handle(RUN_CLIP).is_some() {
                RUN_CLIP
            } else {
                STATIC_CLIP
            }
        }
    }
}

/// 奔跑跳按支撑脚挑派生版本：左脚踩实用左腿镜像版，右脚踩实用右腿源片版，
/// 两脚支撑相当（或都不在支撑）用基础奔跑跳。
fn run_clip_for_contacts(contacts: [f32; 2]) -> &'static str {
    let [left, right] = contacts;
    if left > right + CONTACT_EPSILON {
        RUN_CLIP_LEFT
    } else if right > left + CONTACT_EPSILON {
        RUN_CLIP_RIGHT
    } else {
        RUN_CLIP
    }
}

/// 剪辑髋高 → 起跳基线以上的根位移（米）。
///
/// 平地 / 上跳只取基线以上的部分，蹲姿和落地压缩不该把胶囊往地下拽；
/// 下跳取基线以下的有符号部分，胶囊沿曲线落向低台面。
fn root_y_at(action: &JumpAction, clip_time: f32) -> f32 {
    let raw = sample_curve(&action.curve, action.curve_step, clip_time) - action.takeoff_hips;
    match action.kind {
        JumpKind::Down => raw.min(0.0),
        JumpKind::Flat | JumpKind::Up => raw.max(0.0),
    }
}

/// 一次跳跃开始时把髋曲线采下来，省得每帧再解一条剪辑通道
#[allow(clippy::too_many_arguments)]
/// 解析剪辑的髋高曲线与时间轴（离地 / 落地帧）。本地起跳与远端观察共用同一份
/// 事件锚点，否则动画的离地帧对不上物理的离地时刻。
fn analyze_clip(
    clip: &'static str,
    asset: &AnimationClip,
    hips: AnimationTargetId,
) -> (Timeline, Vec<f32>, f32) {
    let spec = spec_for(clip);
    let duration = asset.duration().max(spec.end);
    let step = 1.0 / CURVE_HZ;
    let count = (duration / step).ceil() as usize + 1;
    let mut curve = Vec::with_capacity(count);
    for index in 0..count {
        let time = (index as f32 * step).min(duration);
        let y = asset
            .sample_clamped(animated_field!(Transform::translation), hips, time)
            .map(|translation| translation.y)
            .unwrap_or(0.0);
        curve.push(y);
    }
    let timeline = Timeline::build(spec, &curve, step, duration);
    (timeline, curve, step)
}

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

fn sample_curve(curve: &[f32], step: f32, time: f32) -> f32 {
    if curve.is_empty() || step <= f32::EPSILON {
        return 0.0;
    }
    let x = (time / step).max(0.0);
    let index = x.floor() as usize;
    let frac = x - index as f32;
    let a = curve[index.min(curve.len() - 1)];
    let b = curve[(index + 1).min(curve.len() - 1)];
    a + (b - a) * frac
}

/// 落地恢复段的交接相位：每帧把移动相位推到「动作剪辑末帧那只着地脚的支撑位置」。
///
/// 不写这一步，融回移动的第一帧可能落在「半支撑」姿态上，脚会在地上滑半步。
///
/// **着地脚不能按剪辑名猜**：跑跳是「一条腿蹬地、另一条腿先着地」，蹬地腿和
/// 最后踩实的腿不是同一只。实测 `RunningJump`（右腿蹬地的原片）左踝 0.700 就
/// 触地，右踝到 0.875 才踩实、一直撑到末帧；`RunningJumpLeft` 正好镜像。
/// 名字里带不带 `Right` 只在派生片上成立，原片 `RunningJump` 也落在右脚——
/// 按名字取脚会整整差半个周期：交接那一刻「另一只脚」正处在支撑相里往后蹭，
/// 看着就是落地后步频少了一拍。
///
/// 所以按**剪辑末帧还踩在地上的那只脚**判（它的支撑窗口跨过相位 0/1），
/// 现场没有步态数据时才退回名字约定。
///
/// **只对到触地点还不够。** 动作剪辑末帧的着地脚早不在触地位置，而是已经撑到
/// 支撑相后段；`Running` 的触地点却是「脚掌刚落下、还在体前」。实测（Amy，
/// 踝相对髋的前后位置）：`RunningJumpRight` 末帧右脚 z −0.234（体后），
/// `Running` 相位 0.927 右脚 z +0.075（体前）。差 0.31 m，全摊在淡出的 0.15s 上
/// 就是 2.1 m/s —— 脚在地上被硬拽回体前，正常速度下看着就是落地后又「换了一次脚」
/// 加一层残影（慢放拆得开，所以慢放看着正常）。
///
/// 补法：动作剪辑末帧这只脚在支撑相里已经走完的比例（`stance_progress`，跑跳实测
/// 0.67），乘到移动剪辑这只脚的支撑相长度上，得到 `target`。
///
/// **相位也不能冻着。** 恢复段里移动相位按它自己的原生速度推进
/// `recover_secs / 剪辑时长` 个周期（跑跳 = 0.24/0.70 = 0.347），所以起点要退到
/// `target − 0.343`，正好在落地那一帧开始走、交回那一刻走到 `target`。
/// 这样淡出期间着地脚相对身体是往后走的（和真正跑步一样），不会被身体带着
/// 一路往前滑。起点 0.723 落在移动剪辑这只脚的摆动相（0.135 → 0.927）——那一刻
/// 动作剪辑权重还是 1.0，摆动相看不见；等权重降下来，相位已经走到 0.923 → 0.066
/// 的支撑段，交回时着地脚正好踩实。
/// 落地恢复段的相位基准：`(交接目标相位, 恢复段该走的周期数)`。
///
/// 只在**进入 Land 那一帧**算一次（`drive_jump` 的 `previous == Air` 分支），之后整段
/// 复用。每帧现算会让基准跟着权重漂——跑着落地时人一停，混合从 Running 滑向
/// Walking / Idle，`target` 从 0.066 跳到 0.51、`advance` 从 0.343 掉到 0.03，
/// 写出来的相位当场瞬移半圈，脚就是「落地后猛换一下」。
#[allow(clippy::too_many_arguments)]
fn landing_alignment(
    driver: &PoseDriver,
    catalog: &AnimCatalog,
    selection: &AnimSelection,
    action_clip: &str,
    timeline: Timeline,
    clips: &ClipSet,
    clip_assets: &Assets<AnimationClip>,
) -> (f32, f32) {
    let def = catalog.character(selection.character);
    let right = def
        .and_then(|def| def.gaits.get(action_clip))
        .and_then(landing_foot_right)
        .unwrap_or(action_clip != RUN_CLIP_LEFT);
    // 混合里第一个不一定是最主要的剪辑：取权重最大的那条拿它的触地 / 离地事件
    let dominant = driver
        .weights
        .iter()
        .filter(|(_, weight)| *weight > f32::EPSILON)
        .max_by(|a, b| a.1.total_cmp(&b.1))
        .map(|(clip, _)| clip.as_str());
    let locomotion = dominant.and_then(|clip| def.and_then(|def| def.gaits.get(clip)));
    let target = landing_phase(
        def.and_then(|def| def.gaits.get(action_clip)),
        locomotion,
        right,
    );
    // 恢复段该推进多少周期：按移动剪辑的原生播放速度（相位的推进速度就是 1/时长）
    let advance = dominant
        .and_then(|clip| clips.duration(clip_assets, clip))
        .filter(|duration| *duration > f32::EPSILON)
        .map(|duration| timeline.recover_secs / duration)
        .unwrap_or(0.0);
    (target, advance)
}

/// 恢复段每帧把移动相位写到「基准相位 + 按原生速度走出来的那一段」。
///
/// 现场调参用：`CRAB_LOG_LAND=1` 启动客户端，就能看到恢复段每帧写了多少相位。
fn follow_landing_phase(
    driver: &mut PoseDriver,
    target: f32,
    advance: f32,
    land_time: f32,
    recover_secs: f32,
) {
    let progress = (land_time / recover_secs.max(f32::EPSILON)).clamp(0.0, 1.0);
    let phase = landing_phase_at(target, advance, progress);
    if std::env::var("CRAB_LOG_LAND").is_ok() {
        info!(
            "land t={:.3} prog={:.3} phase={:.3} target={:.3} adv={:.3}",
            land_time, progress, phase, target, advance
        );
    }
    driver.phases.insert(LocomotionMixer::ID.to_string(), phase);
}

/// 恢复段进度 `progress` 时该写的移动相位：从 `target − advance` 线性推到 `target`。
fn landing_phase_at(target: f32, advance: f32, progress: f32) -> f32 {
    (target - (1.0 - progress.clamp(0.0, 1.0)) * advance).rem_euclid(1.0)
}

/// 交接相位：移动剪辑着地脚的触地点 + 动作剪辑末帧已经走完的那段支撑相。
fn landing_phase(action: Option<&ClipGait>, locomotion: Option<&ClipGait>, right: bool) -> f32 {
    let (action_contact, action_lift) = action.map_or((None, None), |gait| foot_span(gait, right));
    let (contact, lift) = locomotion.map_or((None, None), |gait| foot_span(gait, right));
    // 触地点缺数据时退回实测默认（跑跳：右脚 0.927 / 左脚 0.406）
    let contact = contact.unwrap_or(if right { 0.927 } else { 0.406 });
    let lag = match (
        stance_progress(1.0, action_contact, action_lift),
        stance_fraction(contact, lift),
    ) {
        (Some(progress), Some(span)) => progress * span,
        _ => 0.0,
    };
    (contact + lag).rem_euclid(1.0)
}

/// 某只脚的 `[触地, 离地)` 事件对。
fn foot_span(gait: &ClipGait, right: bool) -> (Option<f32>, Option<f32>) {
    if right {
        (gait.contact_right, gait.lift_right)
    } else {
        (gait.contact_left, gait.lift_left)
    }
}

/// 支撑窗口 `[触地, 离地)` 的长度（周比），跨相位 0/1 时按回绕算。
fn stance_fraction(contact: f32, lift: Option<f32>) -> Option<f32> {
    let span = (lift? - contact).rem_euclid(1.0);
    (span > f32::EPSILON).then_some(span)
}

/// 支撑窗口里 `phase` 已经走完的比例（0 = 刚触地，1 = 正要抬脚）。
fn stance_progress(phase: f32, contact: Option<f32>, lift: Option<f32>) -> Option<f32> {
    let contact = contact?;
    let span = stance_fraction(contact, lift)?;
    let progress = (phase - contact).rem_euclid(1.0) / span;
    (progress <= 1.0 + 1e-4).then_some(progress.clamp(0.0, 1.0))
}

/// 剪辑末帧还踩在地上的是不是右脚。
///
/// 判据是支撑窗口 `[触地, 离地)` 跨过相位 0/1——跑跳收尾正是「一只脚撑住、
/// 另一只脚已经抬起来前摆」，所以这一只就是交接后该继续钉地的那只。
/// 两只脚都在窗口里或都不在（原地双脚跳那种）时分不出来，返回 `None`。
fn landing_foot_right(gait: &ClipGait) -> Option<bool> {
    let supports_at_wrap = |contact: Option<f32>, lift: Option<f32>| {
        let (Some(contact), Some(lift)) = (contact, lift) else {
            return false;
        };
        let span = (lift - contact).rem_euclid(1.0);
        span > f32::EPSILON && (0.0 - contact).rem_euclid(1.0) < span
    };
    match (
        supports_at_wrap(gait.contact_left, gait.lift_left),
        supports_at_wrap(gait.contact_right, gait.lift_right),
    ) {
        (true, false) => Some(false),
        (false, true) => Some(true),
        _ => None,
    }
}

/// 动作层求值：给驱动端调用，按权重混进移动姿态
/// 远端 / 模拟端角色的一次跳跃解析结果：剪辑 + 时间轴 + 起跳髋高 + 本次档位参数。
pub struct RemoteJumpSetup {
    clip: &'static str,
    timeline: Timeline,
    takeoff_hips: f32,
    tuning: JumpTuning,
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
    state: JumpState,
    clip: &'static str,
    timeline: Timeline,
    takeoff_hips: f32,
    /// 本次跳跃的档位参数（起跳那一帧从表里取一次，之后不再读表）
    tuning: JumpTuning,
    /// 已经飞了多久（含倒推量）
    air_time: f32,
    /// 落地恢复段已播时长
    land_time: f32,
    /// 进入动作层的时长，用于淡入
    elapsed: f32,
    /// 动作层权重
    weight: f32,
    /// 剪辑本地时间
    clip_time: f32,
    /// 滞空慢放系数
    slow: f32,
    /// 宽限计时：刚离地那一帧射线可能还贴地，先不判落地
    grace: f32,
    /// 平滑后的竖直速度（m/s）
    vy: f32,
    last_y: f32,
    primed: bool,
}

impl Default for RemoteJump {
    fn default() -> Self {
        Self {
            state: JumpState::Ground,
            clip: STATIC_CLIP,
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
            vy: 0.0,
            last_y: 0.0,
            primed: false,
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
    /// - `setup`：只在「确认离地」那一帧调用一次，给出本次该播的剪辑与时间轴
    pub fn observe(
        &mut self,
        dt: f32,
        y: f32,
        height: Option<f32>,
        setup: impl FnOnce() -> Option<RemoteJumpSetup>,
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
                let Some(entry) = setup() else {
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

    fn air_clip_time(&self) -> f32 {
        let air = self.timeline.air_secs;
        if air <= f32::EPSILON {
            return self.timeline.land;
        }
        let k = (self.air_time * self.slow / air).clamp(0.0, 1.0);
        self.timeline.takeoff + k * (self.timeline.land - self.timeline.takeoff)
    }
}

/// 远端跳跃的剪辑选择：台阶类型 / 地速 / 相位，规则与本地一致。
pub fn pick_clip(
    kind: JumpKind,
    speed: f32,
    phase: Option<f32>,
    gait: Option<&ClipGait>,
    clips: &ClipSet,
) -> &'static str {
    match kind {
        JumpKind::Up => {
            if clips.handle(UP_CLIP).is_some() {
                UP_CLIP
            } else {
                STATIC_CLIP
            }
        }
        JumpKind::Down => {
            if clips.handle(DOWN_CLIP).is_some() {
                DOWN_CLIP
            } else {
                STATIC_CLIP
            }
        }
        JumpKind::Flat => {
            let candidate = if speed >= RUN_JUMP_SPEED {
                match (phase, gait) {
                    (Some(phase), Some(gait)) => run_clip_for_phase(phase, gait),
                    _ => RUN_CLIP,
                }
            } else {
                STATIC_CLIP
            };
            if clips.handle(candidate).is_some() {
                candidate
            } else if clips.handle(RUN_CLIP).is_some() {
                RUN_CLIP
            } else {
                STATIC_CLIP
            }
        }
    }
}

/// 远端用相位推支撑脚：这一步落在哪只脚的支撑窗里就用哪边的心跳剪辑。
fn run_clip_for_phase(phase: f32, gait: &ClipGait) -> &'static str {
    let left = phase_in_span(phase, gait.contact_left, gait.lift_left);
    let right = phase_in_span(phase, gait.contact_right, gait.lift_right);
    match (left, right) {
        (true, false) => RUN_CLIP_LEFT,
        (false, true) => RUN_CLIP_RIGHT,
        _ => RUN_CLIP,
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
    hips: AnimationTargetId,
    clips: &ClipSet,
    assets: &Assets<AnimationClip>,
    tunings: &JumpTunings,
) -> Option<RemoteJumpSetup> {
    let intent = LocomotionIntent {
        direction,
        magnitude: speed,
    };
    let kind = probe_kind(spatial, filter, transform, &intent);
    let clip = pick_clip(kind, speed, phase, gait, clips);
    let asset = assets.get(clips.handle(clip)?)?;
    let (timeline, curve, step) = analyze_clip(clip, asset, hips);
    let tuning = tunings.get(JumpProfile::for_jump(kind, speed));
    Some(RemoteJumpSetup {
        clip,
        timeline,
        takeoff_hips: sample_curve(&curve, step, timeline.takeoff),
        tuning,
    })
}

pub(crate) fn apply_action(
    action: &JumpAction,
    clips: &ClipSet,
    assets: &Assets<AnimationClip>,
    targets: &[AnimationTargetId],
    rest: &std::collections::HashMap<AnimationTargetId, super::core::pose::BonePose>,
    pose: &mut Pose,
) {
    blend_clip_pose(
        action.clip_name(),
        action.clip_time,
        action.weight,
        clips,
        assets,
        targets,
        rest,
        pose,
    );
}

/// 把某条剪辑在 `clip_time` 的姿态按 `weight` 混进 `pose`。本地动作层与远端
/// 观察层共用：两边采样的必须是同一套骨骼映射与静息姿态。
#[allow(clippy::too_many_arguments)]
pub(crate) fn blend_clip_pose(
    clip: &str,
    clip_time: f32,
    weight: f32,
    clips: &ClipSet,
    assets: &Assets<AnimationClip>,
    targets: &[AnimationTargetId],
    rest: &std::collections::HashMap<AnimationTargetId, super::core::pose::BonePose>,
    pose: &mut Pose,
) {
    if weight <= 0.0 {
        return;
    }
    let Some(handle) = clips.handle(clip) else {
        return;
    };
    let Some(asset) = assets.get(handle) else {
        return;
    };
    let action_pose = Pose::sample(asset, targets, rest, clip_time);
    pose.blend_in_place(&action_pose, weight);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn build_timeline(spec: &JumpSpec) -> Timeline {
        Timeline::build(spec, &[], 1.0 / CURVE_HZ, spec.end)
    }

    /// 各档锚点必须精确落在实测时间上
    #[test]
    fn timeline_hits_measured_anchors() {
        for spec in [SPEC_STATIC, SPEC_RUN, SPEC_UP, SPEC_DOWN] {
            let tl = build_timeline(&spec);
            assert!((tl.clip_time(JumpState::Takeoff, 0.0, 0.0, 0.0, 1.0) - tl.entry).abs() < 1e-4);
            assert!(
                (tl.clip_time(JumpState::Takeoff, tl.take_secs, 0.0, 0.0, 1.0) - tl.takeoff).abs()
                    < 1e-4
            );
            assert!((tl.clip_time(JumpState::Air, 0.0, 0.0, 0.0, 1.0) - tl.takeoff).abs() < 1e-4);
            assert!(
                (tl.clip_time(JumpState::Land, tl.total_secs(), 0.0, tl.recover_secs, 1.0)
                    - tl.end)
                    .abs()
                    < 1e-3
            );
        }
    }

    /// 蓄力段与落地恢复段剪辑时间单调，否则姿态会倒着走
    #[test]
    fn takeoff_and_land_segments_monotonic() {
        for spec in [SPEC_STATIC, SPEC_RUN, SPEC_UP, SPEC_DOWN] {
            let tl = build_timeline(&spec);
            let mut last = tl.clip_time(JumpState::Takeoff, 0.0, 0.0, 0.0, 1.0);
            for i in 1..=50 {
                let e = tl.take_secs * i as f32 / 50.0;
                let now = tl.clip_time(JumpState::Takeoff, e, 0.0, 0.0, 1.0);
                assert!(now >= last - 1e-5, "takeoff {e}: {now} < {last}");
                last = now;
            }
            for i in 1..=50 {
                let l = tl.recover_secs * i as f32 / 50.0;
                let now = tl.clip_time(JumpState::Land, tl.total_secs(), 0.0, l, 1.0);
                assert!(now >= last - 1e-5, "land {l}: {now} < {last}");
                last = now;
            }
        }
    }

    /// 空中：整段 takeoff→land 单调推进、不回绕；慢放系数越小推进越慢；
    /// 滞空超过剪辑空中段后钉在落地帧，等接地信号。
    #[test]
    fn air_plays_full_clip_slowed_without_wrap() {
        for spec in [SPEC_STATIC, SPEC_RUN, SPEC_UP, SPEC_DOWN] {
            let tl = build_timeline(&spec);
            let mut last = tl.clip_time(JumpState::Air, 0.0, 0.0, 0.0, 1.0);
            for i in 1..=50 {
                let a = tl.air_secs * i as f32 / 50.0;
                let now = tl.clip_time(JumpState::Air, 0.0, a, 0.0, 1.0);
                assert!(now >= last - 1e-5, "air {a}: {now} < {last}");
                assert!(now <= tl.land + 1e-4, "air {a}: {now} > land");
                last = now;
            }
            // 超长滞空钉在落地帧，不回绕
            let parked = tl.clip_time(JumpState::Air, 0.0, 5.0, 0.0, 1.0);
            assert!((parked - tl.land).abs() < 1e-4, "parked: {parked}");
            // 慢放 0.5：同样 air_time 只走一半剪辑进度
            let half = tl.clip_time(JumpState::Air, 0.0, tl.air_secs, 0.0, 0.5);
            let mid = tl.takeoff + 0.5 * (tl.land - tl.takeoff);
            assert!((half - mid).abs() < 1e-4, "slow half: {half} vs {mid}");
        }
    }

    /// 权重：起跳淡入、滞空保持满权重、落地恢复段淡出归零
    #[test]
    fn weight_fades_in_and_out() {
        for spec in [SPEC_STATIC, SPEC_RUN, SPEC_UP, SPEC_DOWN] {
            let tl = build_timeline(&spec);
            assert_eq!(tl.weight(JumpState::Ground, 0.0, 0.0), 0.0);
            assert_eq!(tl.weight(JumpState::Takeoff, 0.0, 0.0), 0.0);
            assert!((tl.weight(JumpState::Takeoff, BLEND_IN_SECS, 0.0) - 1.0).abs() < 1e-5);
            assert!((tl.weight(JumpState::Air, BLEND_IN_SECS, 0.0) - 1.0).abs() < 1e-5);
            assert!(tl.weight(JumpState::Land, tl.total_secs(), tl.recover_secs) < 1e-5);
        }
    }

    /// 淡入必须在离地帧之前完成：否则物理已经把人抬起来，姿态还在跑
    #[test]
    fn blend_completes_before_takeoff() {
        for spec in [SPEC_STATIC, SPEC_RUN, SPEC_UP, SPEC_DOWN] {
            let tl = build_timeline(&spec);
            assert!(
                tl.blend_secs <= tl.take_secs.max(MIN_BLEND_SECS) + 1e-6,
                "淡入 {:.3}s 超过蓄力段 {:.3}s",
                tl.blend_secs,
                tl.take_secs
            );
            // 离地那一帧就是满权重：起飞瞬间姿态已经换到跳跃剪辑
            assert!(
                (tl.weight(JumpState::Air, tl.take_secs, 0.0) - 1.0).abs() < 1e-5,
                "离地帧权重不足 1（take {:.3}s, blend {:.3}s）",
                tl.take_secs,
                tl.blend_secs
            );
        }
        // 跑跳蓄力段 0.05s：淡入跟着压到 0.05s，不再吃满 0.14s
        let mut run = build_timeline(&SPEC_RUN);
        run.set_take_secs(NET_RUN_TAKE_SECS);
        assert!((run.blend_secs - NET_RUN_TAKE_SECS).abs() < 1e-6);
    }

    /// 蓄力段：地面深蹲 1:1 保留剪辑时长；高台准备段压缩到固定时长
    #[test]
    fn anticipation_preserved_for_jumps_not_drop() {
        let flat = build_timeline(&SPEC_STATIC);
        assert!((flat.take_secs - (SPEC_STATIC.takeoff - SPEC_STATIC.entry)).abs() < 1e-4);
        let drop = build_timeline(&SPEC_DOWN);
        assert!((drop.take_secs - COMPRESSED_TAKE_SECS).abs() < 1e-4);
    }

    /// 事件检测：先蹲后升再落的曲线能检出离地 / 落地；末帧仍高于基线（登上高台）回退默认落地帧
    #[test]
    fn detect_events_finds_takeoff_and_land() {
        let step = 1.0 / CURVE_HZ;
        // 0.70 基线：先蹲到 0.55，抬到 0.95 顶点，再落回 0.70
        let curve = vec![
            0.70, 0.66, 0.60, 0.55, 0.58, 0.64, 0.74, 0.85, 0.95, 0.88, 0.78, 0.71, 0.70,
        ];
        let (takeoff, land) = detect_events(&SPEC_STATIC, &curve, step, curve.len() as f32 * step);
        // 越过基线+0.03 的第一帧是索引 6（0.74）；落地是索引 11（0.71）
        assert!((takeoff - 6.0 * step).abs() < 1e-4);
        assert!((land - 11.0 * step).abs() < 1e-4);

        // 登上高台：末帧 1.05 仍高于基线，找不到回落帧 → 回退默认 land
        let up = vec![0.70, 0.66, 0.60, 0.62, 0.75, 0.90, 1.02, 1.05];
        let (_, land_up) = detect_events(&SPEC_UP, &up, step, up.len() as f32 * step);
        assert!((land_up - SPEC_UP.land).abs() < 1e-4);
    }

    /// 平地 / 上跳根位移只取起跳基线以上的部分
    #[test]
    fn flat_root_offset_is_non_negative() {
        let curve = vec![0.71, 0.54, 0.63, 0.91, 0.61, 0.72];
        let action = JumpAction {
            curve,
            curve_step: 0.1,
            takeoff_hips: 0.6339,
            kind: JumpKind::Flat,
            ..Default::default()
        };
        for step in 0..40 {
            let y = root_y_at(&action, step as f32 * 0.02);
            assert!(y >= 0.0, "t={step}: {y}");
        }
        // 0.3s 处正好是曲线里的 0.914（最高点）
        assert!(root_y_at(&action, 0.3) > 0.27);
    }

    /// 下跳根位移取基线以下的有符号部分
    #[test]
    fn down_root_offset_is_non_positive() {
        let curve = vec![1.43, 1.40, 1.00, 0.66, 0.66];
        let action = JumpAction {
            curve,
            curve_step: 0.1,
            takeoff_hips: 1.43,
            kind: JumpKind::Down,
            ..Default::default()
        };
        for step in 0..40 {
            let y = root_y_at(&action, step as f32 * 0.02);
            assert!(y <= 0.0, "t={step}: {y}");
        }
    }

    /// 支撑区间判定：右脚 0.927→1.135 是跨 0 的，也要算对
    #[test]
    fn stance_span_handles_wrap() {
        // Running 右脚：0.927 触地，0.135 离地
        assert!(phase_in_span(0.95, Some(0.927), Some(0.135)));
        assert!(phase_in_span(0.02, Some(0.927), Some(0.135)));
        assert!(!phase_in_span(0.2, Some(0.927), Some(0.135)));

        // Running 左脚：0.406 → 0.625
        assert!(phase_in_span(0.5, Some(0.406), Some(0.625)));
        assert!(!phase_in_span(0.7, Some(0.406), Some(0.625)));

        // 没测出来就当不在支撑
        assert!(!phase_in_span(0.5, None, None));
    }

    /// 奔跑跳剪辑跟着支撑脚走；差距没超过容差用基础奔跑跳
    #[test]
    fn run_clip_follows_support_foot() {
        assert_eq!(run_clip_for_contacts([1.0, 0.0]), RUN_CLIP_LEFT);
        assert_eq!(run_clip_for_contacts([0.0, 1.0]), RUN_CLIP_RIGHT);
        assert_eq!(run_clip_for_contacts([1.0, 1.0]), RUN_CLIP);
        assert_eq!(run_clip_for_contacts([0.0, 0.0]), RUN_CLIP);
        assert_eq!(run_clip_for_contacts([0.6, 0.58]), RUN_CLIP);
        // 没走过 begin 的默认状态退回原地跳
        assert_eq!(JumpAction::default().clip_name(), STATIC_CLIP);
    }

    /// 落地脚按剪辑末帧的支撑判，不按剪辑名猜。
    ///
    /// 实测 `RunningJump`（右腿蹬地）左踝 0.700 就触地、0.781 抬走；
    /// 右踝 0.875 才踩实（离地 0.062，撑到剪辑末尾）。所以交接后该继续
    /// 钉地的是**右脚**——名字里没有 `Right` 也一样。
    #[test]
    fn landing_foot_follows_the_clip_not_its_name() {
        let base = ClipGait {
            contact_left: Some(0.708),
            lift_left: Some(0.781),
            contact_right: Some(0.875),
            lift_right: Some(0.062),
            ..Default::default()
        };
        assert_eq!(landing_foot_right(&base), Some(true));
        assert_ne!(RUN_CLIP, RUN_CLIP_LEFT);

        // 镜像派生片：左手撑到末帧
        let mirrored = ClipGait {
            contact_left: Some(0.875),
            lift_left: Some(0.052),
            contact_right: Some(0.708),
            lift_right: Some(0.781),
            ..Default::default()
        };
        assert_eq!(landing_foot_right(&mirrored), Some(false));

        // 原地双脚跳：两只脚都在末帧前就「抬」过（检测的是髋弹跳），分不出来
        let static_jump = ClipGait {
            contact_left: Some(0.167),
            lift_left: Some(0.26),
            contact_right: Some(0.146),
            lift_right: Some(0.281),
            ..Default::default()
        };
        assert_eq!(landing_foot_right(&static_jump), None);
        // 缺步态数据也分不出来，交给名字兜底
        assert_eq!(landing_foot_right(&ClipGait::default()), None);
    }

    /// 交接相位补到支撑相后段：动作末帧的着地脚已经撑到哪，移动剪辑就走到哪。
    ///
    /// Amy 实测：`RunningJumpRight` 右脚 0.875→0.062（末帧相位 1.0 已走完
    /// (1.0-0.875)/0.187 = 0.67 的支撑相）；`Running` 右脚 0.927→0.135（支撑相
    /// 长 0.208）。补 0.67×0.208 = 0.139 → 相位 1.066 ≈ 0.066。
    /// 直接对到触地点（0.927）时，着地脚在交接瞬间会从体后 −0.23m 被拽到体前
    /// +0.08m（0.31m / 0.15s），这就是「落地后又换了一次脚」。
    #[test]
    fn landing_phase_reaches_past_the_contact_frame() {
        let action = ClipGait {
            contact_left: Some(0.708),
            lift_left: Some(0.781),
            contact_right: Some(0.875),
            lift_right: Some(0.062),
            ..Default::default()
        };
        let run = ClipGait {
            contact_left: Some(0.406),
            lift_left: Some(0.625),
            contact_right: Some(0.927),
            lift_right: Some(0.135),
            ..Default::default()
        };

        let phase = landing_phase(Some(&action), Some(&run), true);
        assert!((phase - 0.066).abs() < 0.002, "phase = {phase}");
        // 交接后支撑脚只剩 0.135 - 0.066 = 0.07 个周期就要抬脚，
        // 正好接上动作末帧「右脚已经在起跟」的姿态，不再多钉一段
        assert!((0.135 - phase).abs() < 0.07);

        // 镜像：着地脚换成左脚，同样要补
        let mirrored = ClipGait {
            contact_left: Some(0.875),
            lift_left: Some(0.052),
            contact_right: Some(0.708),
            lift_right: Some(0.781),
            ..Default::default()
        };
        let phase = landing_phase(Some(&mirrored), Some(&run), false);
        // 左脚支撑相 0.406→0.625 长 0.219，末帧走完 0.125/0.177 = 0.71
        let expected = 0.406 + 0.71 * 0.219;
        assert!((phase - expected).abs() < 0.01, "phase = {phase}");

        // 缺离地帧（原地跳 / 站立落地）补不了，退回触地点本身
        let no_lift = ClipGait {
            contact_left: Some(0.969),
            contact_right: Some(0.51),
            ..Default::default()
        };
        assert!((landing_phase(None, Some(&no_lift), true) - 0.51).abs() < 1e-6);
        // 全缺数据：跑跳实测默认
        assert!((landing_phase(None, None, true) - 0.927).abs() < 1e-6);
        assert!((landing_phase(None, None, false) - 0.406).abs() < 1e-6);
    }

    /// 恢复段里相位按移动剪辑的原生速度往前走：起点退回一个恢复段，终点正好到 target。
    ///
    /// 跑跳：recover 0.24s / Running 0.70s = 0.343 个周期。所以落地那一帧写
    /// `1.066 − 0.343 = 0.723`，之后每帧往前推，恢复段播完正好 1.066（= 0.066 相位），
    /// 交回移动时两套姿态对齐。冻在 0.927 那版是「一步也不走」，着地脚 0.24s 里
    /// 被身体带着滑 0.7m。
    #[test]
    fn landing_phase_walks_back_one_recover_segment() {
        let target = 1.066;
        let advance: f32 = 0.24 / 0.70;
        assert!((advance - 0.343).abs() < 5e-4);

        let start = landing_phase_at(target, advance, 0.0);
        assert!((start - 0.723).abs() < 5e-3, "start = {start}");
        let end = landing_phase_at(target, advance, 1.0);
        assert!((end - 0.066).abs() < 5e-3, "end = {end}");
        let mid = landing_phase_at(target, advance, 0.5);
        assert!((mid - 0.895).abs() < 5e-3, "mid = {mid}");

        // 交回那一刻右脚只剩 0.135 − 0.066 = 0.07 个周期就要抬脚，正好接上动作
        // 末帧「右脚已经在起跟」的姿态，不会再多钉一段也不会立刻被换掉
        assert!(0.135 - 0.066 > 0.05);
    }

    /// 支撑相进度 / 长度：跨相位 0/1 也要算对
    #[test]
    fn stance_window_helpers_wrap() {
        // 右脚 0.927→0.135：跨 0，长 0.208
        assert!((stance_fraction(0.927, Some(0.135)).unwrap() - 0.208).abs() < 1e-5);
        // 支撑窗口外的相位（右脚摆动相）→ None
        assert_eq!(stance_progress(0.6, Some(0.927), Some(0.135)), None);
        // 刚触地 / 刚要抬脚
        assert!(stance_progress(0.927, Some(0.927), Some(0.135)).unwrap() < 1e-4);
        assert!(stance_progress(0.135, Some(0.927), Some(0.135)).unwrap() > 0.999);
        // 窗口长度为零（原地跳那类没测出步态）→ None
        assert_eq!(stance_fraction(0.5, Some(0.5)), None);
    }

    /// 奔跑跳窗口：单脚着地且踩实脚在支撑前段（重心在后）才放行
    #[test]
    fn running_window_requires_single_foot_rear_support() {
        // Running 实测相位：右脚 0.927→0.135（跨 0），左脚 0.406→0.625
        let gait = ClipGait {
            contact_left: Some(0.406),
            lift_left: Some(0.625),
            contact_right: Some(0.927),
            lift_right: Some(0.135),
            ..Default::default()
        };

        // 右脚着地、支撑相后段（进度 ~0.83，蹬离前）→ 放行
        assert!(running_window([0.05, 0.95], Some(0.10), Some(&gait)));
        // 左脚着地、支撑相后段（进度 ~0.79）→ 放行
        assert!(running_window([0.95, 0.05], Some(0.58), Some(&gait)));

        // 双脚同时踩实 → 拒
        assert!(!running_window([0.9, 0.9], Some(0.10), Some(&gait)));
        // 双脚腾空 → 拒
        assert!(!running_window([0.05, 0.05], Some(0.10), Some(&gait)));
        // 右脚着地但还在触地初期（进度 ~0.11，摆动腿仍在体后）→ 拒
        assert!(!running_window([0.05, 0.95], Some(0.95), Some(&gait)));
        // 右脚着地、支撑进度 ~0.45（门槛 0.55 之前）→ 拒
        assert!(!running_window([0.05, 0.95], Some(0.02), Some(&gait)));
        // 右脚着地但相位在摆动相（不在右脚支撑窗）→ 拒
        assert!(!running_window([0.05, 0.95], Some(0.50), Some(&gait)));

        // 相位 / 步态元数据缺失：退回单脚着地硬条件，不卡死
        assert!(running_window([0.05, 0.95], None, Some(&gait)));
        assert!(running_window([0.05, 0.95], Some(0.95), None));
    }

    /// 剪辑腾空段要跟真实滞空同步：落地那一刻剪辑的落地帧已经播到
    ///
    /// 实测联网跑跳（5.0 m/s + LinearDamping 0.3）真正滞空 0.95s，
    /// RunningJump 的腾空段（takeoff 0.029 → land 0.658）只有 0.63s，
    /// 所以必须慢放；慢放系数由 `AIR_PREDICT_FACTOR` 定，预测得比真实长
    /// 就会在落地时只播到 82%，画面变成"人落地了腿还在空中迈一步"。
    #[test]
    fn air_segment_ends_with_the_real_touchdown() {
        let measured_flight = 0.95_f32;
        let air_secs = 0.658_f32 - 0.029_f32;
        let predicted =
            (2.0 * REMOTE_LAUNCH_VY / GRAVITY * AIR_PREDICT_FACTOR).clamp(air_secs, 60.0);
        let slow = (air_secs / predicted).clamp(SLOW_MIN, 1.0);
        let k = (measured_flight * slow / air_secs).clamp(0.0, 1.0);
        assert!(k >= 0.95, "触地时剪辑腾空段应已播到 95% 以上，实际 {k}");
    }

    /// 相位锁定只推迟不吞请求：等满一个步幅周期窗口不开也照跳
    #[test]
    fn queue_never_swallows_the_request() {
        assert!(takeoff_allowed(true, 0.0));
        assert!(!takeoff_allowed(false, 0.0));
        assert!(!takeoff_allowed(false, QUEUE_MAX_WAIT - 0.01));
        assert!(takeoff_allowed(false, QUEUE_MAX_WAIT));
        assert!(takeoff_allowed(false, QUEUE_MAX_WAIT + 1.0));
    }

    /// 起跳初速不会被贴地接触噪声拉低（噪声 v²/2g 只有毫米级）
    #[test]
    fn launch_speed_ignores_contact_noise() {
        let stand = JumpTuning::STAND;
        for noise in [-1.0, -0.135, 0.0, 0.028, 0.192, 0.45] {
            assert!(
                (launch_speed(noise, stand) - LOCAL_LAUNCH_VY).abs() < 1e-5,
                "噪声 {noise} 不该决定起跳初速"
            );
        }
        // 真的被别的力弹起来（明显高于标定值）才采用实测
        assert!((launch_speed(3.2, stand) - 3.2).abs() < 1e-5);
    }

    /// 台阶类型 + 地速 → 档位：高差优先，平地再按速度分站 / 跑
    #[test]
    fn profile_resolves_from_kind_and_speed() {
        assert_eq!(
            JumpProfile::for_jump(JumpKind::Flat, 0.0),
            JumpProfile::Stand
        );
        assert_eq!(
            JumpProfile::for_jump(JumpKind::Flat, STOP_SPEED),
            JumpProfile::Stand
        );
        assert_eq!(
            JumpProfile::for_jump(JumpKind::Flat, RUN_JUMP_SPEED),
            JumpProfile::Run
        );
        // 高差压过速度：跑着跳上高台仍然是「上跳」档，不会串成跑跳
        assert_eq!(JumpProfile::for_jump(JumpKind::Up, 3.0), JumpProfile::Up);
        assert_eq!(
            JumpProfile::for_jump(JumpKind::Down, 3.0),
            JumpProfile::Down
        );
    }

    /// 四档调参互不影响：改一档，另外三档原样
    #[test]
    fn tuning_profiles_do_not_leak_into_each_other() {
        let mut tunings = JumpTunings::default();
        let before = [
            tunings.get(JumpProfile::Stand),
            tunings.get(JumpProfile::Run),
            tunings.get(JumpProfile::Up),
            tunings.get(JumpProfile::Down),
        ];

        // 只调跑跳：抬高一点、慢放下限抬到 0.8
        tunings.tune(JumpProfile::Run).launch_vy = 4.2;
        tunings.tune(JumpProfile::Run).net_launch_vy = 4.8;
        tunings.tune(JumpProfile::Run).slow_min = 0.8;

        assert_eq!(tunings.get(JumpProfile::Run).launch_vy, 4.2);
        assert_eq!(tunings.get(JumpProfile::Run).net_launch_vy, 4.8);
        assert_eq!(tunings.get(JumpProfile::Run).slow_min, 0.8);

        assert_eq!(tunings.get(JumpProfile::Stand), before[0]);
        assert_eq!(tunings.get(JumpProfile::Up), before[2]);
        assert_eq!(tunings.get(JumpProfile::Down), before[3]);

        // 站 / 跑之外的档位不借用平地的蓄力时长
        assert_eq!(before[2].take_secs, Some(0.516), "上跳应按剪辑自己的离地帧");
        assert_eq!(
            before[3].take_secs,
            Some(COMPRESSED_TAKE_SECS),
            "下跳的准备段本来就是压缩段"
        );
        assert_eq!(before[0].take_secs, Some(NET_STAND_TAKE_SECS));
        assert_eq!(before[1].take_secs, Some(NET_RUN_TAKE_SECS));
    }

    /// 起跳初速取的是**这一档**的标定值：同一份实测速度在不同档里得到不同高度
    #[test]
    fn launch_speed_uses_the_profile_tuning() {
        let mut run = JumpTuning::RUN;
        run.launch_vy = 4.4;
        assert!((launch_speed(0.19, run) - 4.4).abs() < 1e-5);
        assert!((launch_speed(0.19, JumpTuning::STAND) - LOCAL_LAUNCH_VY).abs() < 1e-5);
    }

    /// 起跳那一帧把参数快照进动作层：之后改表不影响这一跳
    #[test]
    fn action_keeps_its_tuning_snapshot() {
        let mut action = JumpAction::default();
        let mut tunings = JumpTunings::default();
        action.profile = JumpProfile::Up;
        action.tuning = tunings.get(JumpProfile::Up);

        tunings.tune(JumpProfile::Up).slow_min = 0.9;
        tunings.tune(JumpProfile::Up).max_air_hold = 0.6;

        assert_eq!(action.tuning.slow_min, JumpTuning::UP.slow_min);
        assert_eq!(action.tuning.max_air_hold, JumpTuning::UP.max_air_hold);
        assert_eq!(tunings.get(JumpProfile::Up).slow_min, 0.9);
    }

    /// 上行电平：等窗口压住、起跳与执行期间保持、落地请求先让出电平
    #[test]
    fn uplink_level_defers_but_never_swallows() {
        assert!(!uplink_jump_level(JumpState::Queued, true, true));
        // 窗口打开后玩家早已松手，上升沿也要发出去
        assert!(uplink_jump_level(JumpState::Takeoff, false, false));
        assert!(uplink_jump_level(JumpState::Air, false, false));
        assert!(uplink_jump_level(JumpState::Land, false, false));
        assert!(uplink_jump_level(JumpState::Abort, false, false));
        // 地面跟随按键
        assert!(uplink_jump_level(JumpState::Ground, false, true));
        assert!(!uplink_jump_level(JumpState::Ground, false, false));
        // 跳跃执行中锁电平：落地恢复段里按下的下一跳不能把电平提前拉下来，
        // 否则服务端会认为这一跳已经结束
        assert!(uplink_jump_level(JumpState::Land, true, true));
        // 但回到地面时，锁存中的请求必须先让出电平，下一跳才有干净的上升沿
        assert!(!uplink_jump_level(JumpState::Ground, true, true));
    }

    /// 动作层接管判定：地面与排队都不算接管，避免排队期间接触权重被清零卡死窗口
    #[test]
    fn active_excludes_ground_and_queued() {
        let mut action = JumpAction::default();
        action.state = JumpState::Ground;
        assert!(!action.is_active());
        action.state = JumpState::Queued;
        assert!(!action.is_active());
        for state in [JumpState::Takeoff, JumpState::Air, JumpState::Land] {
            action.state = state;
            assert!(action.is_active(), "{:?} 应算接管", state);
        }
        // 起跳失败的淡出段也算接管：姿态还挂在动作层上，这时放开脚 IK 会把
        // 半蹬出去的腿当场钉回地面，看着就是一顿抽。
        action.state = JumpState::Abort;
        assert!(action.is_active());
    }

    /// 起跳失败：姿态定格在离地帧，而不是跳到落地恢复段。
    ///
    /// 失败时人还站在原处，没有落地冲击可演。若按 `Land` 取剪辑时间，动作层会在
    /// 权重还是 1.0 的那一帧摆出「落地踩实」的恢复姿势，再在这上面淡出——玩家看到
    /// 的是一次凭空的落地回弹。定格在 `takeoff` 则只是把蹬伸姿态收回移动混合。
    #[test]
    fn failed_jump_freezes_the_takeoff_frame() {
        for spec in [SPEC_STATIC, SPEC_RUN, SPEC_UP, SPEC_DOWN] {
            let tl = build_timeline(&spec);
            assert!((tl.clip_time(JumpState::Abort, 0.0, 0.0, 0.0, 1.0) - tl.takeoff).abs() < 1e-4);
            // 滞空段无论已经飞了多久，失败后都定格在离地帧，不再往前推
            for air in [0.0, 0.05, 0.3, tl.air_secs] {
                let frozen = tl.clip_time(JumpState::Abort, 0.0, air, 0.0, 0.5);
                assert!((frozen - tl.takeoff).abs() < 1e-4, "{air}s: {frozen}");
            }
            // 落地恢复段的起点是另一帧，绝不能被拿来冒充格
            assert!((tl.takeoff - tl.land).abs() > 1e-3);
        }
    }

    /// 剪辑名 → 时间轴规格
    #[test]
    fn spec_matches_clip_family() {
        assert!(std::ptr::eq(spec_for(UP_CLIP), &SPEC_UP));
        assert!(std::ptr::eq(spec_for(DOWN_CLIP), &SPEC_DOWN));
        assert!(std::ptr::eq(spec_for(RUN_CLIP_LEFT), &SPEC_RUN));
        assert!(std::ptr::eq(spec_for(STATIC_CLIP), &SPEC_STATIC));
    }

    /// 远端挑剪辑：慢速原地跳、快速按支撑脚选左右派生、缺剪辑时逐级回退
    #[test]
    fn pick_clip_matches_local_rules() {
        let mut clips = ClipSet::default();
        clips.insert(STATIC_CLIP, Handle::<AnimationClip>::default());
        // 只有原地跳：快速也回退到 Jumping
        assert_eq!(
            pick_clip(JumpKind::Flat, 3.0, None, None, &clips),
            STATIC_CLIP
        );

        clips.insert(RUN_CLIP, Handle::<AnimationClip>::default());
        // 无相位可用 → 基础奔跑跳
        assert_eq!(pick_clip(JumpKind::Flat, 3.0, None, None, &clips), RUN_CLIP);
        // 慢速 → 原地跳
        assert_eq!(
            pick_clip(JumpKind::Flat, 0.4, Some(0.6), None, &clips),
            STATIC_CLIP
        );

        clips.insert(RUN_CLIP_LEFT, Handle::<AnimationClip>::default());
        clips.insert(RUN_CLIP_RIGHT, Handle::<AnimationClip>::default());
        let gait = ClipGait {
            stride: 1.6,
            contact_left: Some(0.50),
            lift_left: Some(0.85),
            contact_right: Some(0.00),
            lift_right: Some(0.35),
        };
        // 相位落在右脚支撑窗里 → 右脚心跳剪辑
        assert_eq!(
            pick_clip(JumpKind::Flat, 3.0, Some(0.2), Some(&gait), &clips),
            RUN_CLIP_RIGHT
        );
        // 左脚支撑窗
        assert_eq!(
            pick_clip(JumpKind::Flat, 3.0, Some(0.6), Some(&gait), &clips),
            RUN_CLIP_LEFT
        );
        // 台阶类型优先于速度分档
        assert_eq!(
            pick_clip(JumpKind::Up, 3.0, Some(0.6), Some(&gait), &clips),
            STATIC_CLIP
        );
    }

    fn test_remote_jump(timeline: Timeline) -> RemoteJump {
        RemoteJump {
            timeline,
            ..RemoteJump::default()
        }
    }

    #[test]
    fn failed_takeoff_only_when_blocked() {
        // 窗口到、根没升、脚没离地、人还贴地 → 这一跳被物理吃掉了
        assert!(takeoff_failed(
            TAKEOFF_FAIL_SECS,
            TAKEOFF_FAIL_SECS,
            false,
            false,
            true
        ));
        // 窗口没到：先别认输，可能只是物理还没把它抬起来
        assert!(!takeoff_failed(0.02, TAKEOFF_FAIL_SECS, false, false, true));
        // 根升起来了 → 真起跳，绝不能提前收尾
        assert!(!takeoff_failed(
            MAX_AIR_HOLD_SECS,
            TAKEOFF_FAIL_SECS,
            true,
            false,
            true
        ));
        // 脚底抬离过地面 → 真起跳
        assert!(!takeoff_failed(
            MAX_AIR_HOLD_SECS,
            TAKEOFF_FAIL_SECS,
            false,
            true,
            true
        ));
        // 人已经不在接地状态（掉下高台 / 还在上升）→ 不算失败
        assert!(!takeoff_failed(
            MAX_AIR_HOLD_SECS,
            TAKEOFF_FAIL_SECS,
            false,
            false,
            false
        ));
    }

    /// 远端观察：起跳剪辑从离地帧起播、滞空钉在落地帧、触地即进落地段
    #[test]
    fn remote_jump_aligns_to_observed_trajectory() {
        let timeline = Timeline {
            entry: 0.0,
            takeoff: 0.30,
            land: 0.90,
            end: 1.20,
            take_secs: 0.30,
            air_secs: 0.60,
            recover_secs: 0.30,
            blend_secs: BLEND_IN_SECS,
        };
        let mut jump = test_remote_jump(timeline);
        let setup = || {
            Some(RemoteJumpSetup {
                clip: STATIC_CLIP,
                timeline,
                takeoff_hips: 1.05,
                tuning: JumpTuning::STAND,
            })
        };

        const GROUND_Y: f32 = 0.70;
        const LAUNCH: f32 = 5.0;
        let dt = 1.0 / 60.0;
        // 先站 0.5s：平地上不该被当成起跳
        let mut t = 0.0;
        for _ in 0..30 {
            jump.observe(dt, GROUND_Y, Some(0.0), setup);
            t += dt;
            assert!(!jump.is_airborne(), "平地被误判起跳");
            assert_eq!(jump.weight(), 0.0);
        }

        // 起跳：y 走抛物线，脚底高度跟着 y 走（时间从离地那一刻重新起算）
        let t0 = t;
        let mut first_air = None;
        let mut last_clip = jump.clip_time();
        let mut landed = None;
        let mut monotone = true;
        for _ in 0..120 {
            t += dt;
            // 抛物线从「离地那一刻」起算
            let air = t - t0;
            let y = GROUND_Y + LAUNCH * air - 0.5 * GRAVITY * air * air;
            let height = Some((y - GROUND_Y).max(0.0));
            jump.observe(dt, y, height, setup);
            if jump.is_airborne() {
                assert_eq!(jump.clip(), STATIC_CLIP);
                let ct = jump.clip_time();
                if ct + 1e-4 < last_clip {
                    monotone = false;
                }
                last_clip = ct;
                assert!(ct <= timeline.land + 1e-4, "空中越过落地帧：{ct}");
                first_air.get_or_insert((t - t0, ct));
            } else if first_air.is_some() && landed.is_none() {
                // 触地那一帧必须已经切到落地帧
                assert!(
                    (jump.clip_time() - timeline.land).abs() < 1e-3,
                    "落地点没对齐：{}",
                    jump.clip_time()
                );
                landed = Some(t - t0);
                break;
            }
            if y < GROUND_Y - 0.05 {
                break;
            }
        }
        let (takeoff_t, takeoff_clip) = first_air.expect("应该识别到起跳");
        assert!(monotone, "空中剪辑时间回绕");
        // 动作从离地帧起播（倒推量不超过 0.2s），不能落到蓄力段里
        assert!(
            takeoff_clip >= timeline.takeoff - 1e-4 && takeoff_clip <= timeline.takeoff + 0.2,
            "起跳帧对齐失败：{takeoff_clip}"
        );
        // 识别起跳只比真正离地晚两帧以内（60Hz 下 ≤ 0.05s）
        assert!(takeoff_t <= 0.06, "起跳识别太晚：{takeoff_t}");
        // 落地时刻（相对真正离地）应当就是物理滞空 2v/g ≈ 1.02s，误差 ≲ 两帧
        let landed = landed.expect("应该落回地面");
        let physical_air = 2.0 * LAUNCH / GRAVITY;
        assert!(
            (landed - physical_air).abs() < 0.05,
            "落地识别偏差：{landed} vs {physical_air}"
        );
        // 识别起跳那一刻动作已经倒推了 `h/vy`，起跳帧仍然对得上
        let _ = takeoff_t;

        // 落地恢复段：权重单调淡出到 0
        let mut weight = jump.weight();
        for _ in 0..40 {
            jump.observe(dt, GROUND_Y, Some(0.0), setup);
            let now = jump.weight();
            assert!(now <= weight + 1e-5, "落地权重回升：{weight}→{now}");
            weight = now;
        }
        assert!(weight < 1e-5, "落地恢复没有淡出：{weight}");
    }

    /// 下坠（竖直速度为负、脚下没有地面）不能播成起跳
    #[test]
    fn remote_jump_ignores_falling() {
        let mut jump = RemoteJump::default();
        let mut y = 3.0f32;
        for _ in 0..40 {
            let y0 = y;
            y -= 0.08;
            jump.observe(1.0 / 60.0, y, None, || {
                unreachable!("下坠不该请求起跳剪辑");
            });
            let _ = y0;
            assert!(!jump.is_airborne());
            assert_eq!(jump.weight(), 0.0);
        }
    }
}
