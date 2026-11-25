//! 系统健康诊断：每条移动/表现链路一个采集器，把实时指标写进 [`Diagnostics`]，
//! 面板上逐行显示状态灯（正常 / 警告 / 异常 / 离线）与关键数值。
//!
//! 设计原则：
//! - **只读不改**：采集系统不碰物理、动画、网络任何写路径，只读数、写诊断资源，
//!   所以开不开诊断面板都不影响手感；
//! - **离线可降级**：`net` 特性之外、或本帧还没有本地角色时，对应链路标「离线」，
//!   不编造数值；
//! - **读数稳定**：同一行的数值不会逐帧横跳——步频按 0.5 s 固定窗口闭口、快照间隔
//!   走指数平滑；读不到数据时先在 [`DATUM_GRACE`] 宽限期内沿用上一次读数，之后只
//!   降状态灯、不把数值推回「—」。面板是叠在对局上的，闪一次就等于整页重排一次；
//! - **阈值集中**：警告/异常阈值写在本模块常量里，判定逻辑纯函数、可单测。
//!
//! 新增一条链路：在 [`DiagnosticRowId`] 加枚举、在 [`Diagnostics::default`] 注册一行，
//! 再写一个采集系统把数值填进对应行即可。

use std::collections::HashMap;

use bevy::prelude::*;

use avian3d::prelude::LinearVelocity;

use crate::anim::{FootIk, PoseDriver};
use crate::camera::CameraFollowSettings;

/// 定步长间隔（与 predict.rs 的 `FIXED_DT` 同口径），用来把帧内步数换算成步频。
const FIXED_DT: f32 = 1.0 / 60.0;

/// 本地角色是否存在的判定时间窗：超过这么久没见到本地角色，链路标离线。
const LOCAL_MISSING_TIMEOUT: f32 = 1.0;

/// 单帧没读到数据时的宽限期：这么短的空档沿用上一次读数，避免「正常 / 警告」
/// 逐帧来回切；超过它才降级成警告并写原因。
const DATUM_GRACE: f32 = 0.4;

/// 步频统计窗长度（秒）。按帧算增量会在 60Hz 物理 + 60Hz 渲染的错相位下
/// 一半帧是 0 步、一半帧是 1 步，读数在 0 Hz / 60 Hz 之间反复横跳。
#[cfg_attr(not(feature = "net"), allow(dead_code))]
const STEP_WINDOW_SECS: f32 = 0.5;

/// 面板单行副信息能放下的「半角单位」数：一个汉字算 2 个单位。
/// 11px 字体下，抽屉内容区一行大约这么宽；超出就裁掉，保证永远只占一行。
pub const SUB_MAX_UNITS: usize = 52;

/// 按显示宽度把一句话裁到 `max_units` 个半角单位以内，超出部分用「…」收尾。
///
/// 面板上的副信息／原因都要走这一道：文本一旦换行，卡片高度就会变，整页跟着重排。
pub fn clip_text(text: &str, max_units: usize) -> String {
    let mut units = 0usize;
    let mut out = String::new();
    for ch in text.chars() {
        let width = if ch.is_ascii() { 1 } else { 2 };
        if units + width > max_units {
            return format!("{}…", out.trim_end());
        }
        units += width;
        out.push(ch);
    }
    out
}

/// 物理步频偏离 60Hz 的容差（每秒步数）。
#[cfg_attr(not(feature = "net"), allow(dead_code))]
const STEP_RATE_WARN: f32 = 10.0;
#[cfg_attr(not(feature = "net"), allow(dead_code))]
const STEP_RATE_BAD: f32 = 20.0;

/// 朝向角速度异常偏大（度/秒）。超过它说明在原地打转或朝向系统振荡。
const YAW_RATE_BAD: f32 = 180.0;

/// 脚端 IK 的**额外**修正超过这个距离（米）算异常。
///
/// 口径是「修正 − 本帧根位移」：身体被快照台阶整体挪动时，腿把脚拽回世界锚点
/// 是 IK 的本职，那部分不该算异常；扣掉之后还剩这么多，才是脚在原地被拽着滑。
/// 小幅修正是正常工作状态（真实走路时 IK 一直在几厘米的量级上修）。
const IK_CORRECTION_BAD: f32 = 0.06;

/// 快照间隔阈值（毫秒）。60Hz 服务端理论 16.7 ms，实际随帧相位在 16~33 ms 摆动，
/// 所以 40 ms 以下都按正常算。
#[cfg_attr(not(feature = "net"), allow(dead_code))]
const NET_INTERVAL_WARN_MS: f32 = 40.0;
#[cfg_attr(not(feature = "net"), allow(dead_code))]
const NET_INTERVAL_BAD_MS: f32 = 80.0;

/// 链路状态。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DiagnosticStatus {
    /// 指标在正常区间。
    Ok,
    /// 数值逼近阈值，需要留意。
    Warn,
    /// 明显越界：这条链路很可能就是抖动来源。
    Bad,
    /// 当前构建/对局状态下这条链路不参与（例如非 net 构建、没有本地角色）。
    Offline,
}

impl DiagnosticStatus {
    /// 面板状态灯颜色。
    pub fn color(self) -> Color {
        match self {
            Self::Ok => Color::srgb(0.32, 0.78, 0.46),
            Self::Warn => Color::srgb(0.93, 0.74, 0.24),
            Self::Bad => Color::srgb(0.92, 0.30, 0.28),
            Self::Offline => Color::srgba(0.55, 0.58, 0.64, 0.7),
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Ok => "正常",
            Self::Warn => "警告",
            Self::Bad => "异常",
            Self::Offline => "离线",
        }
    }
}

/// 一条被监控的链路标识。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Default)]
pub enum DiagnosticRowId {
    /// 物理步频：固定步长每秒跑几步，是否和 60Hz 对齐。
    #[default]
    PhysicsStepRate,
    /// 朝向系统：角速度、目标夹角。
    Facing,
    /// 速度层：线速度大小与方向。
    LinearVelocity,
    /// 网络快照：位置同步的方式（现在只有「快照直接写 Transform」一种）。
    NetSnapshot,
    /// 动画混合空间：地速、支撑权重、是否求值。
    AnimBlend,
    /// 脚部 IK：锁脚权重、单脚修正距离。
    FootIk,
    /// 相机跟随：当前策略与参数。
    CameraFollow,
}

impl DiagnosticRowId {
    /// 面板上的中文标题。
    pub fn title(self) -> &'static str {
        match self {
            Self::PhysicsStepRate => "物理步频",
            Self::Facing => "朝向系统",
            Self::LinearVelocity => "速度层",
            Self::NetSnapshot => "网络快照",
            Self::AnimBlend => "动画混合",
            Self::FootIk => "脚部 IK",
            Self::CameraFollow => "相机跟随",
        }
    }

    /// 固定顺序：面板按这个顺序逐行渲染。
    pub const ALL: [Self; 7] = [
        Self::PhysicsStepRate,
        Self::Facing,
        Self::LinearVelocity,
        Self::NetSnapshot,
        Self::AnimBlend,
        Self::FootIk,
        Self::CameraFollow,
    ];
}

/// 一条链路的实时读数。
#[derive(Clone, Debug, PartialEq)]
pub struct DiagnosticRow {
    pub status: DiagnosticStatus,
    /// 主数值（面板第一行右侧，例如「0.012 m」）。
    pub primary: String,
    /// 副信息（面板第二行，例如「上限 0.06 m」）。
    pub detail: String,
    /// 异常/警告时的原因说明；正常/离线为空。
    pub reason: String,
    /// 距离上一次拿到有效读数过了多久（秒）：只在采集不到数据时累加。
    since_seen: f32,
    /// 是否拿到过读数。拿到过就一直保留最后一次的数值，不再退回占位符。
    ever_seen: bool,
}

impl Default for DiagnosticRow {
    fn default() -> Self {
        Self {
            status: DiagnosticStatus::Offline,
            primary: "—".to_string(),
            detail: String::new(),
            reason: String::new(),
            since_seen: 0.0,
            ever_seen: false,
        }
    }
}

impl DiagnosticRow {
    /// 本帧没读到这条链路的数据：宽限期内沿用上一次读数，之后转「警告」，
    /// 超过 [`LOCAL_MISSING_TIMEOUT`] 再转「离线」。
    ///
    /// 有历史数值时只降状态、保留最后一次的数值，画面不会在「—」和数字之间闪。
    fn mark_missing(&mut self, dt: f32, grace_reason: &str) -> bool {
        self.since_seen = (self.since_seen + dt).min(LOCAL_MISSING_TIMEOUT + 1.0);

        if !self.ever_seen {
            self.status = DiagnosticStatus::Offline;
            self.primary = "—".to_string();
            self.detail = "等待本地角色…".to_string();
            self.reason.clear();
            return true;
        }

        if self.since_seen < DATUM_GRACE {
            return false;
        }
        if self.since_seen < LOCAL_MISSING_TIMEOUT {
            if self.status == DiagnosticStatus::Ok {
                self.status = DiagnosticStatus::Warn;
                self.reason = grace_reason.to_string();
            }
            return false;
        }

        self.status = DiagnosticStatus::Offline;
        self.reason.clear();
        true
    }

    /// 直接写入一条「在线」读数。
    pub(crate) fn set(
        &mut self,
        status: DiagnosticStatus,
        primary: impl Into<String>,
        detail: impl Into<String>,
        reason: impl Into<String>,
    ) {
        self.status = status;
        self.primary = primary.into();
        self.detail = detail.into();
        self.reason = reason.into();
        self.since_seen = 0.0;
        self.ever_seen = true;
    }
}

/// 全部链路的诊断结果。采集系统每帧更新对应行，面板只读它渲染。
#[derive(Resource, Clone, Debug, PartialEq)]
pub struct Diagnostics {
    rows: HashMap<DiagnosticRowId, DiagnosticRow>,
    /// 网络快照的落地读数：快照多久变一次、每次位移多远。只有 net 构建会填。
    #[cfg_attr(not(feature = "net"), allow(dead_code))]
    net: NetMeter,
    /// 步频窗的参考采样：(采样时刻, 当时的累计步数)。窗满 [`STEP_WINDOW_SECS`] 才闭口。
    step_ref: Option<(f32, u32)>,
    /// 上一次闭口算出的步频（Hz）。首个窗口走完之前是 `None`。
    step_rate: Option<f32>,
    /// 诊断自己累计的时钟（秒），与物理 tick 计数配对。
    last_steps_time: f32,
    /// 自启动以来物理 tick 总数（仅作副信息展示，不参与 Hz 计算）。
    total_steps: u32,
}

impl Default for Diagnostics {
    fn default() -> Self {
        let rows = DiagnosticRowId::ALL
            .iter()
            .map(|id| (*id, DiagnosticRow::default()))
            .collect();
        Self {
            rows,
            net: NetMeter::default(),
            step_ref: None,
            step_rate: None,
            last_steps_time: 0.0,
            total_steps: 0,
        }
    }
}

/// 每具骨架上一帧的世界位置：脚部 IK 的口径要用「本帧根位移」把快照台阶扣掉。
#[derive(Resource, Default, Debug)]
struct IkStepCache {
    last: HashMap<Entity, Vec3>,
}

/// IK 修正里超出「本帧根位移」的那一部分（米）。
///
/// 身体整体被挪走 Δ 时，撑地那只脚必须被拽回世界锚点，修正量本来就会有 Δ 那么
/// 大——这不是滑步，是锁脚在做本职。真正该报的只有**超出**这一份的额外拉动。
fn ik_excess(correction: f32, root_step: f32) -> f32 {
    (correction - root_step).max(0.0)
}

/// 快照落地的量度：NetPos 多久变一次、每次跳多远。
///
/// 这两个数直接解释「台阶感」从哪来：间隔 16.7 ms、步长 = 速度 × 16.7 ms 时，
/// 60Hz 屏幕上角色就是一格一格往前挪；间隔变大或步长忽大忽小，观感就是抖和拉扯。
#[derive(Clone, Debug, Default, PartialEq)]
struct NetMeter {
    /// 诊断自己的时钟（秒），与 `last_change` 配对。
    clock: f32,
    /// 上一次看到的快照位置。
    last_pos: Option<[f32; 3]>,
    /// 上一次快照变化的时刻。
    last_change: Option<f32>,
    /// 两次变化间隔的指数平滑值（毫秒）。
    interval_ms: f32,
    /// 最近一次变化位移的长度（米）。
    step_m: f32,
}

impl NetMeter {
    /// 吸一帧快照位置：位置变了就记一次「间隔 + 步长」。
    #[cfg_attr(not(feature = "net"), allow(dead_code))]
    fn observe(&mut self, dt: f32, pos: Option<[f32; 3]>) {
        self.clock += dt;
        let Some(pos) = pos else { return };
        let Some(last) = self.last_pos else {
            self.last_pos = Some(pos);
            self.last_change = Some(self.clock);
            return;
        };
        if last == pos {
            return;
        }
        self.step_m = Vec3::from_array(pos).distance(Vec3::from_array(last));
        if let Some(prev) = self.last_change {
            let interval = (self.clock - prev) * 1000.0;
            // 指数平滑：单次间隔会被帧抖动带偏，面板上的数不该跟着跳
            self.interval_ms = if self.interval_ms <= 0.0 {
                interval
            } else {
                self.interval_ms * 0.85 + interval * 0.15
            };
        }
        self.last_pos = Some(pos);
        self.last_change = Some(self.clock);
    }

    /// 快照已经多久没有变化（秒）；还没见过快照时返回 `None`。
    #[cfg_attr(not(feature = "net"), allow(dead_code))]
    fn still_secs(&self) -> Option<f32> {
        self.last_change.map(|change| self.clock - change)
    }
}

impl Diagnostics {
    pub fn row(&self, id: DiagnosticRowId) -> &DiagnosticRow {
        self.rows
            .get(&id)
            .expect("DiagnosticRowId::ALL 必须与注册行一致")
    }

    pub fn row_mut(&mut self, id: DiagnosticRowId) -> &mut DiagnosticRow {
        self.rows
            .get_mut(&id)
            .expect("DiagnosticRowId::ALL 必须与注册行一致")
    }

    /// 总体状态：任一异常 = 异常；否则任一警告 = 警告；全离线 = 离线；其余正常。
    pub fn overall(&self) -> DiagnosticStatus {
        let mut any_warn = false;
        let mut any_online = false;
        for row in self.rows.values() {
            match row.status {
                DiagnosticStatus::Bad => return DiagnosticStatus::Bad,
                DiagnosticStatus::Warn => any_warn = true,
                DiagnosticStatus::Ok => any_online = true,
                DiagnosticStatus::Offline => {}
            }
        }
        if any_warn {
            return DiagnosticStatus::Warn;
        }
        if any_online {
            return DiagnosticStatus::Ok;
        }
        DiagnosticStatus::Offline
    }
}

/// 按累计步数与累计时间算步频（Hz）：两次采样间的步数增量 / 时间增量。
///
/// `prev_steps` 为 `None`（首次采样）时没有增量可比，返回 0，避免拿累计计数
/// 直接除帧间隔放大成几千 Hz。
#[cfg_attr(not(feature = "net"), allow(dead_code))]
fn step_rate_from_delta(
    now_steps: u32,
    prev_steps: Option<u32>,
    prev_time: f32,
    now_time: f32,
) -> f32 {
    match prev_steps {
        Some(prev) if now_time - prev_time > 1e-4 => {
            let delta_steps = now_steps.wrapping_sub(prev) as f32;
            let dt = now_time - prev_time;
            let rate = delta_steps / dt;
            // 抗噪：物理时钟每秒最多跑 1/FIXED_DT 个 tick。帧率暴跌/补帧时 dt 极小，
            // 相邻帧 tick 增量 ÷ dt 会算出上百甚至上千 Hz，这种尖峰是窗内相位噪声，
            // 不是真实步频，直接钳到物理时钟上限。
            let phys_cap = 1.0 / FIXED_DT;
            rate.min(phys_cap).max(0.0)
        }
        _ => 0.0,
    }
}

/// 物理步频分级：每秒固定步数应接近 60。
#[cfg_attr(not(feature = "net"), allow(dead_code))]
fn classify_step_rate(steps_per_sec: f32) -> (DiagnosticStatus, String) {    let target = 1.0 / FIXED_DT;
    let dev = (steps_per_sec - target).abs();
    if steps_per_sec <= f32::EPSILON {
        (DiagnosticStatus::Warn, "本秒没有物理步".to_string())
    } else if dev > STEP_RATE_BAD {
        (
            DiagnosticStatus::Bad,
            format!("步频 {steps_per_sec:.0} Hz，偏离 {target:.0} Hz 超 {STEP_RATE_BAD:.0}"),
        )
    } else if dev > STEP_RATE_WARN {
        (
            DiagnosticStatus::Warn,
            format!("步频 {steps_per_sec:.0} Hz，偏离 {target:.0} Hz"),
        )
    } else {
        (DiagnosticStatus::Ok, String::new())
    }
}

pub(crate) struct DiagnosticsPlugin;

impl Plugin for DiagnosticsPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<Diagnostics>()
            .init_resource::<IkStepCache>()
            // 物理步计数：挂在固定步长调度上，每个物理 tick 自增 1。步频直接数这个
            // 累加器，不再从 `DriftDiag.steps`（历史累计计数）反推，避免累计量泄漏进 Hz。
            .add_systems(bevy::app::FixedPostUpdate, count_physics_step)
            .add_systems(Update, collect_diagnostics.run_if(in_state(crate::GameState::Game)));
    }
}

/// 每个物理固定步长 +1：步频滑窗的计数来源。
fn count_physics_step(mut diag: ResMut<Diagnostics>) {
    diag.total_steps = diag.total_steps.wrapping_add(1);
}

/// 本帧采集到的纯数据快照：从 world 读出来时不持有诊断资源的借用，
/// 算完状态再一次性写回 [`Diagnostics`]，避免「可变诊断资源 + 不可变 world」交叉借用。
#[derive(Default, Clone, Debug)]
struct Sample {
    facing_rate: Option<f32>,
    facing_error: Option<f32>,
    vel_horizontal: Option<f32>,
    vel_total: Option<f32>,
    anim_speed: Option<f32>,
    anim_support: Option<f32>,
    anim_gain: Option<f32>,
    anim_issues: Vec<String>,
    anim_evaluated: bool,
    /// 扣掉本帧根位移之后仍存在的脚部修正（米）——「IK 额外干的活」。
    ik_excess: Option<f32>,
    /// 同一具骨架的原始修正（米），只作副信息对照。
    ik_corr: Option<f32>,
    /// 同一具骨架本帧的根位移（米），只作副信息对照。
    ik_step: Option<f32>,
    ik_avg_blend: Option<f32>,
    ik_count: usize,
    cam_strategy: Option<&'static str>,
    cam_snap: Option<f32>,
    /// 本地（联机）角色的最新快照位置。
    #[cfg_attr(not(feature = "net"), allow(dead_code))]
    net_pos: Option<[f32; 3]>,
}

impl Sample {
    /// 从 world 读出全部可读数据（不碰诊断资源）。
    fn collect(world: &mut World) -> Self {
        let mut sample = Sample::default();

        if let Some((facing, vel)) = world
            .query::<(&crate::input::FacingState, &LinearVelocity)>()
            .iter(world)
            .next()
        {
            sample.facing_rate = Some(facing.yaw_rate.abs());
            sample.facing_error = Some(facing.error_deg);
            sample.vel_horizontal = Some(Vec2::new(vel.0.x, vel.0.z).length());
            sample.vel_total = Some(vel.0.length());
        }

        if let Some(driver) = world.get_resource::<PoseDriver>() {
            sample.anim_evaluated = driver.evaluated;
            sample.anim_speed = Some(driver.ground_speed);
            sample.anim_support = Some(driver.contacts[0].max(driver.contacts[1]));
            sample.anim_gain = Some(driver.pose_gain);
            sample.anim_issues = driver.issues.clone();
        }

        // 脚部 IK：每具骨架单独量本帧根位移，再把修正里「超出根位移的那一份」取最坏。
        // 跨角色（本地 + 远端）混算会把别人的台阶扣到这具骨架上，所以位移必须逐实体配。
        let rows: Vec<(Entity, Vec3, f32, f32)> = world
            .query::<(Entity, &FootIk, &GlobalTransform)>()
            .iter(world)
            .map(|(entity, ik, transform)| {
                let correction = ik.correction();
                (
                    entity,
                    transform.translation(),
                    correction[0].max(correction[1]),
                    ik.blend(),
                )
            })
            .collect();
        if !rows.is_empty() {
            sample.ik_count = rows.len();
            sample.ik_avg_blend = Some(
                rows.iter().map(|(_, _, _, blend)| *blend).sum::<f32>() / rows.len() as f32,
            );
            let mut cache = world.resource_mut::<IkStepCache>();
            let mut best: Option<(f32, f32, f32)> = None;
            for (entity, pos, correction, _) in rows {
                // 第一次见到这具骨架（刚刷出来 / 重生）：没有上一帧位置，这一帧不判，
                // 否则出生时的那一次大修正会被当成异常
                let previous = cache.last.insert(entity, pos);
                let Some(previous) = previous else {
                    continue;
                };
                let step = pos.distance(previous);
                let excess = ik_excess(correction, step);
                if best.is_none_or(|(worst, _, _)| excess > worst) {
                    best = Some((excess, correction, step));
                }
            }
            if let Some((excess, correction, step)) = best {
                sample.ik_excess = Some(excess);
                sample.ik_corr = Some(correction);
                sample.ik_step = Some(step);
            }
            // 全部骨架都还是首帧：读数还没建立，交给 `mark_missing` 的宽限期
        }

        if let Some(settings) = world.get_resource::<CameraFollowSettings>() {
            sample.cam_strategy = Some(settings.strategy.label());
            sample.cam_snap = Some(settings.snap_distance);
        }

        #[cfg(feature = "net")]
        {
            let local_id = world.get_resource::<crate::net::LocalPlayer>().map(|p| p.0);
            if let Some(local_id) = local_id {
                sample.net_pos = world
                    .query::<(
                        &crab_feast_library::net::NetPlayerId,
                        &crab_feast_library::net::NetPos,
                    )>()
                    .iter(world)
                    .find(|(id, _)| id.0 == local_id)
                    .map(|(_, pos)| pos.0);
            }
        }

        sample
    }
}

/// 汇总采集入口：先从 world 读快照，再统一写进 [`Diagnostics`]。
fn collect_diagnostics(world: &mut World) {
    let dt = world
        .get_resource::<Time>()
        .map(|time| time.delta_secs())
        .unwrap_or(FIXED_DT)
        .max(0.0);

    let sample = Sample::collect(world);
    let mut diag = world.resource_mut::<Diagnostics>();

    #[cfg(feature = "net")]
    apply_net_side(&sample, &mut diag, dt);
    #[cfg(not(feature = "net"))]
    for id in [DiagnosticRowId::NetSnapshot, DiagnosticRowId::PhysicsStepRate] {
        diag.row_mut(id).set(DiagnosticStatus::Offline, "—", "非联机构建不参与", "");
    }

    apply_common_side(&sample, &mut diag, dt);
}

/// 不依赖 net 特性也能采的链路：朝向、速度、动画混合、脚部 IK、相机跟随。
fn apply_common_side(sample: &Sample, diag: &mut Diagnostics, dt: f32) {
    // 朝向系统
    let facing_row = diag.row_mut(DiagnosticRowId::Facing);
    match (sample.facing_rate, sample.facing_error) {
        (Some(rate), Some(error_deg)) => {
            let status = if rate > YAW_RATE_BAD {
                DiagnosticStatus::Bad
            } else {
                DiagnosticStatus::Ok
            };
            let reason = if status == DiagnosticStatus::Bad {
                format!("角速度 {rate:.0}°/s 偏大，可能原地打转或振荡")
            } else {
                String::new()
            };
            facing_row.set(
                status,
                format!("{rate:.1} °/s"),
                format!("目标夹角 {error_deg:.1}°"),
                reason,
            );
        }
        _ => {
            facing_row.mark_missing(dt, "本帧没有本地角色朝向");
        }
    }

    // 速度层
    let vel_row = diag.row_mut(DiagnosticRowId::LinearVelocity);
    match (sample.vel_horizontal, sample.vel_total) {
        (Some(horizontal), Some(total)) => {
            vel_row.set(
                DiagnosticStatus::Ok,
                format!("{horizontal:.2} m/s"),
                format!("合速度 {total:.2} m/s"),
                "",
            );
        }
        _ => {
            vel_row.mark_missing(dt, "本帧读不到本地角色速度");
        }
    }

    // 动画混合空间：本帧没求值（滞空、切模型）先在宽限期内沿用上一次读数，
    // 持续没有输出才降级成警告，避免一帧空白就把灯和文字来回闪。
    let anim_row = diag.row_mut(DiagnosticRowId::AnimBlend);
    match (sample.anim_speed, sample.anim_support, sample.anim_gain) {
        (Some(ground_speed), Some(support), Some(pose_gain)) if sample.anim_evaluated => {
            let status = if ground_speed.is_nan() {
                DiagnosticStatus::Bad
            } else {
                DiagnosticStatus::Ok
            };
            let reason = clip_text(&sample.anim_issues.join("；"), SUB_MAX_UNITS);
            anim_row.set(
                status,
                format!("{ground_speed:.2} m/s"),
                format!("支撑权重 {support:.2} · 档位补偿 {pose_gain:.2}"),
                reason,
            );
        }
        (Some(_), _, _) => {
            anim_row.mark_missing(dt, "混合空间本帧没有输出");
        }
        (None, _, _) => {
            anim_row.mark_missing(dt, "动画驱动本帧读不到");
        }
    }

    // 脚部 IK：主数值是「扣掉本帧根位移之后还剩多少」——小幅修正属于正常工作状态，
    // 只有额外拉动才值得报警。原始修正与台阶放副信息里对照。
    let ik_row = diag.row_mut(DiagnosticRowId::FootIk);
    match (sample.ik_excess, sample.ik_avg_blend) {
        (Some(excess), Some(avg_blend)) => {
            let status = if excess > IK_CORRECTION_BAD {
                DiagnosticStatus::Bad
            } else {
                DiagnosticStatus::Ok
            };
            let reason = if status == DiagnosticStatus::Bad {
                format!("超出台阶 {IK_CORRECTION_BAD:.2} m，脚在原地被额外拉")
            } else {
                String::new()
            };
            let correction = sample.ik_corr.unwrap_or(0.0);
            let step = sample.ik_step.unwrap_or(0.0);
            ik_row.set(
                status,
                format!("{excess:.3} m"),
                format!(
                    "修正 {correction:.3} · 台阶 {step:.3} · {} 具 · 权重 {avg_blend:.2}",
                    sample.ik_count
                ),
                reason,
            );
        }
        _ => {
            // 角色还没刷出来 / IK 目标刚换：宽限期内保留上一次读数。
            ik_row.mark_missing(dt, "本帧没有参与 IK 的角色");
        }
    }

    // 相机跟随：策略是从设置资源读的，资源在就是在线。
    let cam_row = diag.row_mut(DiagnosticRowId::CameraFollow);
    match (sample.cam_strategy, sample.cam_snap) {
        (Some(strategy_label), Some(snap)) => {
            cam_row.set(
                DiagnosticStatus::Ok,
                strategy_label,
                format!("硬切距离 {snap:.1} m"),
                "",
            );
        }
        _ => {
            cam_row.mark_missing(dt, "相机设置本帧读不到");
        }
    }
}

/// 只在 net 特性下采集的链路：步频、网络快照。
///
/// 位置同步已经回到「快照直接赋值」：没有插值光标、没有和解偏置、没有两端残差，
/// 所以这里只剩步频（判断物理时钟有没有跑偏）与一条说明当前对齐方式的读数。
#[cfg(feature = "net")]
fn apply_net_side(sample: &Sample, diag: &mut Diagnostics, dt: f32) {
    // 物理步频：按固定长度的窗口统计，读数不再逐帧横跳。
    let now = diag.last_steps_time + dt;
    diag.last_steps_time = now;
    let total = diag.total_steps;

    let reference = match diag.step_ref {
        Some(reference) => reference,
        None => {
            // 第一个窗口从「现在」起算：之前累计的步数（可能包含菜单里的物理帧）
            // 不掺进增量里，只数窗内的步数。
            diag.step_ref = Some((now, total));
            (now, total)
        }
    };
    if now - reference.0 >= STEP_WINDOW_SECS {
        diag.step_rate = Some(step_rate_from_delta(
            total,
            Some(reference.1),
            reference.0,
            now,
        ));
        diag.step_ref = Some((now, total));
    }

    let target_hz = 1.0 / FIXED_DT;
    match diag.step_rate {
        Some(rate) => {
            let (step_status, step_reason) = classify_step_rate(rate);
            diag.row_mut(DiagnosticRowId::PhysicsStepRate).set(
                step_status,
                format!("{rate:.0} Hz"),
                format!("目标 {target_hz:.0} Hz · 累计 {total} 步"),
                step_reason,
            );
        }
        None => {
            diag.row_mut(DiagnosticRowId::PhysicsStepRate).set(
                DiagnosticStatus::Ok,
                "采样中",
                format!("首次统计窗 {STEP_WINDOW_SECS:.1} s 未走完"),
                "",
            );
        }
    }

    // 网络快照：`Position` 每帧直接取快照，视觉 `Transform` 由 `PoseSmoothing`
    // 在最近两份快照之间插值（固定滞后 1.25 个间隔）。这里量的是快照本身的节奏——
    // 多久变一次、每次跳多远，有没有插值、预测都不影响这两个数。
    diag.net.observe(dt, sample.net_pos);
    let meter = diag.net.clone();
    let net_row = diag.row_mut(DiagnosticRowId::NetSnapshot);
    match meter.still_secs() {
        None => net_row.set(
            DiagnosticStatus::Ok,
            "等待快照",
            "还没收到本地角色的 NetPos",
            "",
        ),
        Some(still) if still > 0.5 => net_row.set(
            DiagnosticStatus::Ok,
            "静止",
            "最近 0.5 s 位置没有变化",
            "",
        ),
        Some(_) => {
            let interval = meter.interval_ms;
            // 阈值放宽到 40 ms（25Hz）以下都算正常：60Hz 服务端的实际间隔会在
            // 16~33 ms 之间随帧相位摆动，卡死 16.7 会让灯来回闪。
            let status = if interval >= NET_INTERVAL_BAD_MS {
                DiagnosticStatus::Bad
            } else if interval >= NET_INTERVAL_WARN_MS {
                DiagnosticStatus::Warn
            } else {
                DiagnosticStatus::Ok
            };
            let reason = match status {
                DiagnosticStatus::Bad => format!(
                    "快照 {interval:.0} ms 一次，位移会出现明显台阶"
                ),
                DiagnosticStatus::Warn => format!("快照 {interval:.0} ms 一次，偏慢"),
                _ => String::new(),
            };
            net_row.set(
                status,
                format!("{interval:.0} ms"),
                format!(
                    "步长 {:.3} m · 视觉滞后 {:.0} ms",
                    meter.step_m,
                    crate::net_pose::PoseSmoothing::lag() * 1000.0
                ),
                reason,
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classify_step_rate_bands() {
        assert_eq!(classify_step_rate(60.0).0, DiagnosticStatus::Ok);
        assert_eq!(classify_step_rate(45.0).0, DiagnosticStatus::Warn);
        assert_eq!(classify_step_rate(20.0).0, DiagnosticStatus::Bad);
        assert_eq!(classify_step_rate(0.0).0, DiagnosticStatus::Warn);
    }

    #[test]
    fn step_rate_uses_delta_not_cumulative() {
        // 首次采样没有上一帧基准：返回 0，不拿累计计数除时间。
        assert_eq!(step_rate_from_delta(120, None, 0.0, 0.016), 0.0);

        // 0.5 秒内跑了 30 步 → 60 Hz；累计步数已经很大也只算增量。
        let rate = step_rate_from_delta(150, Some(120), 0.0, 0.5);
        assert!((rate - 60.0).abs() < 1e-3, "{rate}");

        // 0.1 秒内只跑了 3 步 → 30 Hz（半速）。
        let slow = step_rate_from_delta(153, Some(150), 0.5, 0.6);
        assert!((slow - 30.0).abs() < 1e-3, "{slow}");

        // 窗内相位噪声：16ms 窗里误计成 100 步时钳到物理上限 60Hz，
        // 不能算出几千 Hz 的尖峰。
        let noisy = step_rate_from_delta(120, Some(20), 0.0, 1.0 / 60.0);
        let cap = 1.0 / FIXED_DT;
        assert!(noisy <= cap + 1e-3, "尖峰被钳到物理上限：{noisy} ≤ {cap}");
    }

    /// 一句话裁到单行宽度：汉字算 2 个单位，超出用「…」收尾。
    #[test]
    fn clip_text_keeps_one_line() {
        let short = clip_text("目标 60 Hz", SUB_MAX_UNITS);
        assert_eq!(short, "目标 60 Hz");

        let long = "这是一句明显超过面板一行宽度的副信息，必须在裁剪之后仍然保持单行显示";
        let clipped = clip_text(long, SUB_MAX_UNITS);
        assert!(clipped.ends_with('…'), "{clipped}");
        let units: usize = clipped
            .chars()
            .map(|ch| if ch.is_ascii() { 1 } else { 2 })
            .sum();
        assert!(units <= SUB_MAX_UNITS + 2, "裁完仍超宽：{units}");
    }

    /// 读不到数据时：宽限期内原样保留，之后只降状态灯，数值不回退成「—」。
    #[test]
    fn missing_data_keeps_last_reading() {
        let mut row = DiagnosticRow::default();
        row.set(DiagnosticStatus::Ok, "1.20 m/s", "合速度 1.20 m/s", "");

        // 一帧空档：状态与数值都不动
        assert!(!row.mark_missing(0.016, "本帧没有数据"));
        assert_eq!(row.status, DiagnosticStatus::Ok);
        assert_eq!(row.primary, "1.20 m/s");

        // 超过宽限期：转警告，数值保留
        row.mark_missing(0.5, "混合空间本帧没有输出");
        assert_eq!(row.status, DiagnosticStatus::Warn);
        assert_eq!(row.primary, "1.20 m/s");
        assert_eq!(row.reason, "混合空间本帧没有输出");

        // 超过离线超时：只把灯降成离线，仍然保留最后一次数值
        assert!(row.mark_missing(0.6, "x"));
        assert_eq!(row.status, DiagnosticStatus::Offline);
        assert_eq!(row.primary, "1.20 m/s");
        assert!(row.reason.is_empty());
    }

    /// 从没拿到过读数：只写一次占位符，之后一直稳定在同一句。
    #[test]
    fn unseen_row_placeholder_is_stable() {
        let mut row = DiagnosticRow::default();
        let mut texts = std::collections::HashSet::new();
        for _ in 0..120 {
            row.mark_missing(1.0 / 60.0, "x");
            texts.insert((row.status.label(), row.primary.clone(), row.detail.clone()));
        }
        assert_eq!(texts.len(), 1, "占位符不该来回变");
        assert_eq!(row.status, DiagnosticStatus::Offline);
        assert_eq!(row.primary, "—");
        assert_eq!(row.detail, "等待本地角色…");
    }

    /// 步频读数按固定窗口闭口：窗内逐帧交替 0/1 步，读数一个字都不许变。
    #[cfg(feature = "net")]
    #[test]
    fn step_rate_holds_between_windows() {
        let mut diag = Diagnostics::default();
        let frame = 1.0 / 60.0;
        apply_net_side(&Sample::default(), &mut diag, frame);
        assert_eq!(
            diag.row(DiagnosticRowId::PhysicsStepRate).primary,
            "采样中",
            "首帧还没有窗口可算"
        );

        let mut seen = std::collections::HashSet::new();
        for index in 0..28u32 {
            diag.total_steps += index % 2;
            apply_net_side(&Sample::default(), &mut diag, frame);
            seen.insert(diag.row(DiagnosticRowId::PhysicsStepRate).primary.clone());
        }
        assert_eq!(seen.len(), 1, "窗内读数横跳：{seen:?}");
        assert_eq!(seen.iter().next().unwrap(), "采样中");

        // 窗闭口之后才换成数字读数
        for index in 0..10u32 {
            diag.total_steps += index % 2;
            apply_net_side(&Sample::default(), &mut diag, frame);
        }
        assert!(
            diag.row(DiagnosticRowId::PhysicsStepRate)
                .primary
                .ends_with("Hz")
        );
    }

    /// 快照节奏：间隔按变化算、步长按位移算；不动的时候给出「静止」。
    #[test]
    fn net_meter_measures_snapshot_step() {
        let mut meter = NetMeter::default();
        let frame = 1.0 / 60.0;
        meter.observe(frame, Some([0.0, 0.0, 0.0]));
        assert!(meter.still_secs().is_some(), "首帧就记下参考时刻");

        let mut x = 0.0;
        for _ in 0..4 {
            x += 0.1;
            meter.observe(frame, Some([x, 0.0, 0.0]));
        }
        assert!((meter.step_m - 0.1).abs() < 1e-4, "步长 {}", meter.step_m);
        assert!(
            (meter.interval_ms - 1000.0 / 60.0).abs() < 1.0,
            "间隔 {}",
            meter.interval_ms
        );

        // 位置不变：间隔与步长都保持，静止时长继续走
        for _ in 0..40 {
            meter.observe(frame, Some([x, 0.0, 0.0]));
        }
        assert!(meter.still_secs().unwrap() > 0.5);
        assert!((meter.step_m - 0.1).abs() < 1e-4);
    }

    /// 脚部 IK 的口径：根节点被整体挪走的那一份不算异常，只有额外拉动才算。
    #[test]
    fn ik_excess_discounts_the_root_step() {
        // 满速 3 m/s × 40 ms 快照 = 0.12 m 台阶：修正 0.118 全是本职，读数是 0
        assert!(ik_excess(0.118, 0.120) < 1e-4);
        // 本体不动时的纯滑步：一分不减
        assert!((ik_excess(0.09, 0.0) - 0.09).abs() < 1e-4);
        // 修正大于台阶的那部分留在读数里
        assert!((ik_excess(0.15, 0.12) - 0.03).abs() < 1e-4);
        // 台阶大于修正（被带着走、IK 反而放松）不能出现负数
        assert_eq!(ik_excess(0.01, 0.12), 0.0);

        // 判定沿用同一个阈值：小幅修正（单机走路的常态）保持绿灯
        let small = ik_excess(0.05, 0.02);
        assert!(small <= IK_CORRECTION_BAD, "{small}");
        let bad = ik_excess(0.12, 0.02);
        assert!(bad > IK_CORRECTION_BAD, "{bad}");
    }

    #[test]
    fn overall_status_prefers_bad_then_warn() {
        let mut diag = Diagnostics::default();
        // 全离线 → 离线
        assert_eq!(diag.overall(), DiagnosticStatus::Offline);

        diag.row_mut(DiagnosticRowId::Facing)
            .set(DiagnosticStatus::Ok, "x", "", "");
        assert_eq!(diag.overall(), DiagnosticStatus::Ok);

        diag.row_mut(DiagnosticRowId::LinearVelocity)
            .set(DiagnosticStatus::Warn, "x", "", "");
        assert_eq!(diag.overall(), DiagnosticStatus::Warn);

        diag.row_mut(DiagnosticRowId::NetSnapshot)
            .set(DiagnosticStatus::Bad, "x", "", "");
        assert_eq!(diag.overall(), DiagnosticStatus::Bad);
    }
}
