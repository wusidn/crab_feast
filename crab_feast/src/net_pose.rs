//! 复制状态的落地：服务端快照 → 本地 `Transform` / 动画参数。
//!
//! 位置只有一条权威曲线——服务端快照；客户端不模拟、不比对、不产生偏置速度：
//!
//! 1. `NetPos` / `NetYaw` 每帧直接写进 `Position` / `Rotation`（物理与判定用这一份）；
//! 2. **只加一层渲染插值**（[`PoseSmoothing`]）：视觉 `Transform` 的时间轴是服务端
//!    `NetTick`，客户端只维护一个按真实时长自由奔跑的时钟（见
//!    [`PoseSmoothing::sample`]）。它不回写 `Position`，只是把「60 Hz 采样落在显示器
//!    帧上」的台阶铺平。**不用收包时刻当时间轴**：那会被渲染帧量化，75 Hz 屏上相邻
//!    两份快照差 13.3 / 26.7 ms 交替，插值必然出锯齿（这正是"加了插值还抖"的原因）；
//! 3. 本地玩家的动画参数（[`LocomotionIntent`] / [`FacingState`]）由复制下来的
//!    `NetMotion` / `NetYaw` 翻译过来：联网模式下没有本地物理去写它们，漏了这步
//!    角色会以 Idle 姿态在地面上滑行。
//!
//! 本地角色和别的玩家走同一条路径（同为 `Kinematic`，位置由快照写）。代价是
//! 输入到画面的延迟 = 一个单程 RTT；换来的是不存在「两套仿真互相拉扯」——
//! 之前的位置修正 / 速度偏置都是在两套仿真之间追残差，快照 60Hz 连着来时
//! 就表现成肉眼可见的抖动和拖拽感。

use avian3d::prelude::{Position, Rotation};
use bevy::prelude::*;
use crab_feast_library::net::{
    MOVE_SPEED_MPS, NetMotion, NetPlayerId, NetPos, NetTick, NetYaw, TICK_HZ,
};

use crate::input::{FacingState, LocomotionIntent, PlanarMotion, wrap_pi};
use crate::net::LocalPlayer;

/// 视觉落后最新快照多少个 tick。
///
/// 这是唯一的"缓冲"：留一个多 tick 的余量，保证视觉始终落在两份快照之间。
/// 65 mm@3 m/s ≈ 21 ms 的额外延迟，换掉的是所有台阶。
pub(crate) const LAG_TICKS: f32 = 1.25;

/// 视觉时钟相对最新 tick 允许落后/超前的上限（tick）。
///
/// 时间是**自由奔跑**的：每帧按 `dt * TICK_HZ` 前进，只在越界时被夹回来。
/// 帧率怎么抖都不影响时间轴本身——这就是它比"按渲染帧对齐"稳的地方。
const MAX_LAG_TICKS: f32 = 2.0;

/// 两份快照的位移超过这个距离（米）就当传送：丢掉历史直接落位。
const TELEPORT_SNAP_M: f32 = 1.5;

/// 相对 tick 的上限（≈4.6 小时 @60 Hz）。超过它说明时间轴断了。
const MAX_TICK_SPAN: f32 = 1_000_000.0;

/// 历史上限：插值只需要 3 份，但本地预测端要能查回「服务端 RTT 之前的那个 tick」，
/// 30 份 ≈ 0.5 s，够覆盖一个高延迟往返。
const HISTORY: usize = 32;

/// 一份快照：相对 tick + 位置 + 偏航角。
///
/// `rel` 是相对「第一份快照的 tick」的偏移，**不是收包时刻**。服务端的 tick 是精确
/// 的整数时间轴，间距恒为 1；收包时刻会被渲染帧量化（75 Hz 屏上相邻两份差
/// 13.3 / 26.7 ms 交替），拿它当时间轴，插值必然出锯齿。
#[derive(Clone, Copy, Debug, Default)]
struct Snapshot {
    rel: f32,
    pos: Vec3,
    yaw: f32,
}

/// 渲染插值状态：最近几份快照 + 一个在 tick 轴上自由奔跑的视觉时钟。
///
/// 挂在所有由快照驱动的玩家身上（本地与远端同一套）。只写 `Transform`，
/// `Position` 仍由 [`apply_authoritative_pose`] 权威赋值。
#[derive(Component, Clone, Copy, Debug)]
pub struct PoseSmoothing {
    hist: [Snapshot; HISTORY],
    len: usize,
    /// tick 基准（第一份快照的 tick），`u32::MAX` = 还没收到过。
    base: u32,
    /// 视觉时钟，单位是「相对基准的 tick」。`None` = 还没起步。
    cursor: Option<f32>,
}

impl Default for PoseSmoothing {
    fn default() -> Self {
        Self {
            hist: [Snapshot::default(); HISTORY],
            len: 0,
            base: u32::MAX,
            cursor: None,
        }
    }
}

impl PoseSmoothing {
    /// 收一份快照（带服务端 tick）。
    ///
    /// 同一个 tick 的重复不计（复制层每 tick 发一次，渲染帧比它密时会重复喂）。
    /// tick 倒退（重启 / 乱序）或位移超过传送阈值时，清空历史重新起步。
    pub(crate) fn push(&mut self, tick: u32, pos: Vec3, yaw: f32) {
        if self.base == u32::MAX {
            self.base = tick;
        }
        let mut rel = tick.wrapping_sub(self.base) as f32;
        // 服务端重启 / 重连会换一条时间轴（tick 从头来），相对偏移会变成天文数字。
        // 这种情况重新锚定，否则后面每一份都会被当成"倒退"永久丢掉。
        if rel > MAX_TICK_SPAN {
            self.base = tick;
            self.len = 0;
            self.cursor = None;
            rel = 0.0;
        }
        if let Some(newest) = self.newest() {
            if rel < newest.rel {
                return;
            }
            if rel == newest.rel && newest.pos == pos && newest.yaw == yaw {
                return;
            }
            if rel > newest.rel && pos.distance(newest.pos) > TELEPORT_SNAP_M {
                self.len = 0;
                self.cursor = None;
            }
        }
        // 覆盖同一个 tick（值变了），否则追加
        if let Some(newest) = self.newest() {
            if newest.rel == rel {
                let last = self.len - 1;
                self.hist[last] = Snapshot { rel, pos, yaw };
                return;
            }
        }
        if self.len == HISTORY {
            self.hist.rotate_left(1);
            self.len -= 1;
        }
        self.hist[self.len] = Snapshot { rel, pos, yaw };
        self.len += 1;
        // 时钟第一次起步：落后 LAG 个 tick
        if self.cursor.is_none() {
            self.cursor = Some(rel - LAG_TICKS);
        }
    }

    fn newest(&self) -> Option<&Snapshot> {
        self.len.checked_sub(1).map(|i| &self.hist[i])
    }

    /// 取指定 tick 上我在那个时刻的位置（用于和解）。
    ///
    /// 找不到（历史被清 / 时间轴还没锚定 / tick 太远）就返回 `None`，调用方跳过
    /// 这一次校正——宁可不修，也不要拿错帧去拉人。
    pub(crate) fn sample_at(&self, tick: u32) -> Option<(Vec3, f32)> {
        if self.base == u32::MAX || self.len == 0 {
            return None;
        }
        let rel = tick.wrapping_sub(self.base) as f32;
        if rel > MAX_TICK_SPAN || rel < self.hist[0].rel - 0.5 || rel > self.newest()?.rel + 0.5 {
            return None;
        }
        let older = (0..self.len).rev().find(|&i| self.hist[i].rel <= rel)?;
        let newer = (0..self.len).find(|&i| self.hist[i].rel >= rel)?;
        let a = self.hist[older];
        let b = self.hist[newer];
        let span = b.rel - a.rel;
        let alpha = if span <= 1e-4 {
            1.0
        } else {
            ((rel - a.rel) / span).clamp(0.0, 1.0)
        };
        Some((a.pos.lerp(b.pos, alpha), lerp_yaw(a.yaw, b.yaw, alpha)))
    }

    /// 把整段历史整体平移（和解用）。
    ///
    /// 校正过的位置必须同时作用于历史，否则下一帧插值还在用旧轨迹，
    /// 视觉会第二次跳。
    pub(crate) fn shift(&mut self, delta: Vec3, delta_yaw: f32) {
        for sample in &mut self.hist[..self.len] {
            sample.pos += delta;
            sample.yaw = wrap_pi(sample.yaw + delta_yaw);
        }
    }

    /// 视觉相对最新快照的落后量（秒）。
    pub(crate) fn lag() -> f32 {
        LAG_TICKS / TICK_HZ as f32
    }

    /// 本帧视觉该停在哪：时钟按真实时长前进，在 tick 轴上夹逼，再找夹住它的两份快照。
    ///
    /// 没有"相位"、没有"到达时间"——时间轴是服务端 tick，精确整数，间距恒定。
    /// 帧率变化只是采样密度变化：一帧走得远一点，落点仍然是同一条曲线上的点。
    pub(crate) fn sample(&mut self, dt: f32) -> Option<(Vec3, f32)> {
        let newest = *self.newest()?;
        let latest = newest.rel;
        let cursor = self.cursor.get_or_insert(latest - LAG_TICKS);
        *cursor += dt.clamp(0.0, 0.1) * TICK_HZ as f32;
        // 落后太多（断档后恢复）就夹到一个多 tick，超前太多就夹到最新
        *cursor = cursor.clamp(latest - MAX_LAG_TICKS, latest);

        let at = *cursor;
        // 夹住 at 的相邻两份：前一份是「tick ≤ at 里最新的」，后一份是「tick ≥ at 里最旧的」
        let older = (0..self.len)
            .rev()
            .find(|&i| self.hist[i].rel <= at)
            .unwrap_or(0);
        let newer = (0..self.len)
            .find(|&i| self.hist[i].rel >= at)
            .unwrap_or(self.len - 1);
        let a = self.hist[older];
        let b = self.hist[newer];
        let span = b.rel - a.rel;
        let alpha = if span <= 1e-4 {
            1.0
        } else {
            ((at - a.rel) / span).clamp(0.0, 1.0)
        };
        Some((
            a.pos.lerp(b.pos, alpha),
            lerp_yaw(a.yaw, b.yaw, alpha),
        ))
    }
}

/// 角度插值走最短一边，避免 ±π 处绕远路。
fn lerp_yaw(from: f32, to: f32, alpha: f32) -> f32 {
    from + wrap_pi(to - from) * alpha
}

/// 所有玩家实体：位置与朝向直接取服务端快照。
///
/// 物理 `Position` / `Rotation` 也要一起写：Avian 的 `position_to_transform` 只要看到
/// 这两个组件变了，就会把 `Transform` 的**平移和旋转**一起覆盖回来。只写 `Transform`、
/// 让 `Position` 停在出生点，碰撞体会留在原地；只写 `Position` 不写 `Rotation`，
/// 朝向又会在下一个物理步被 `Rotation`（单位四元数）冲掉。所以三者同源同值。
///
/// 挂了 [`PoseSmoothing`] 的实体会在 [`smooth_avatar_pose`] 里被覆盖成插值后的
/// `Transform`；`Position` 始终是这份权威值。
///
/// （`transform_to_position` 在 `ScenePlugin` 里是关掉的，不存在反向回灌。）
pub(crate) fn apply_authoritative_pose(
    mut avatars: Query<(
        &NetPos,
        &NetYaw,
        &mut Transform,
        Option<&mut Position>,
        Option<&mut Rotation>,
        Option<&mut PoseSmoothing>,
        Option<&NetTick>,
        Has<crate::predict::Prediction>,
    )>,
) {
    for (net_pos, net_yaw, mut transform, position, rotation, smoothing, tick, predicted) in
        &mut avatars
    {
        // 预测端自己写 `Position`/`Rotation`/`Transform`，这里一个字都不能碰
        if predicted {
            continue;
        }
        let translation = Vec3::from_array(net_pos.0);
        let yaw = Quat::from_rotation_y(net_yaw.0);
        transform.translation = translation;
        transform.rotation = yaw;
        if let Some(mut position) = position {
            position.0 = translation;
        }
        if let Some(mut rotation) = rotation {
            rotation.0 = yaw;
        }
        // 时间轴取服务端 tick：没有 tick 就不插值（拿帧时钟当轴正是锯齿的来源）
        if let (Some(mut smoothing), Some(tick)) = (smoothing, tick) {
            smoothing.push(tick.0, translation, net_yaw.0);
        }
    }
}

/// 渲染插值：本帧把 `Transform` 写成两份快照之间的位置。
///
/// 必须排在 [`apply_authoritative_pose`] 之后、相机之前——相机与动画都读这一帧的
/// 角色 `Transform`，晚一步就是一帧的抖动。
pub(crate) fn smooth_avatar_pose(
    time: Res<Time>,
    mut avatars: Query<(&mut PoseSmoothing, &mut Transform), Without<crate::predict::Prediction>>,
) {
    let dt = time.delta_secs();
    for (mut smoothing, mut transform) in &mut avatars {
        let Some((translation, yaw)) = smoothing.sample(dt) else {
            continue;
        };
        transform.translation = translation;
        transform.rotation = Quat::from_rotation_y(yaw);
    }
}

/// 本地玩家的动画参数：实测速度 → 档位，意图方向相对朝向 → 混合空间 `yaw`。
///
/// 朝向直接取服务端 `NetYaw`（本体旋转由 [`apply_authoritative_pose`] 写），
/// 所以这里只算「位移方向比脸向偏了多少」，给混合空间的侧移 / 倒退 / 转向档用。
pub(crate) fn drive_net_animation(
    local: Option<Res<LocalPlayer>>,
    mut avatars: Query<(
        &NetPlayerId,
        &NetMotion,
        &NetYaw,
        &mut LocomotionIntent,
        &mut FacingState,
        &mut PlanarMotion,
        Has<crate::predict::Prediction>,
    )>,
) {
    let Some(local) = local else {
        return;
    };
    for (id, motion, net_yaw, mut intent, mut state, mut planar_motion, predicted) in
        avatars.iter_mut()
    {
        // 预测端自己写动画参数（本地实测速度 + 本地预测朝向），这里跳过
        if id.0 != local.0 || predicted {
            continue;
        }
        let planar = Vec3::new(motion.dir[0], 0.0, motion.dir[1]);
        // 主动度是「这一帧有没有自己的输入」：被人推着滑、被平台带走时它是 0，
        // 站定必须清零回到 Idle。只看实测速度的话，人会被搬走的同时原地跑步
        if motion.intent > 1e-3 && planar.length_squared() > 1e-6 {
            let dir = planar.normalize();
            intent.direction = dir;
            intent.magnitude = (motion.speed / MOVE_SPEED_MPS).clamp(0.0, 1.0);
            let target = dir.x.atan2(dir.z);
            state.error_deg = wrap_pi(target - net_yaw.0).to_degrees();
            // 实测位移喂给姿态融合：混合空间的 `yaw` 轴靠它算「位移方向相对脸向」，
            // 没有它转身时腿会继续朝正前方迈（见 [`PlanarMotion`]）
            planar_motion.0 = Vec2::new(dir.x, dir.z) * motion.speed;
        } else {
            intent.direction = Vec3::ZERO;
            intent.magnitude = 0.0;
            state.error_deg = 0.0;
            planar_motion.0 = Vec2::ZERO;
        }
        // 朝向的权威值就是服务端快照；视觉上的过渡由 [`PoseSmoothing`] 负责，
        // 这里保留原值给混合空间的误差计算
        state.yaw = net_yaw.0;
        state.yaw_rate = 0.0;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 以恒定速度前进的服务端，按 `fps` 渲染输出视觉位置序列。
    ///
    /// 时间轴用 f64 推进，避免测试自己的累加误差被当成插值误差。
    fn run(fps: f64, frames: usize) -> Vec<f32> {
        let snap_dt = 1.0 / TICK_HZ as f64;
        let frame_dt = 1.0 / fps;
        let step = MOVE_SPEED_MPS * snap_dt as f32;
        let mut smoothing = PoseSmoothing::default();
        let mut wall = 0.0_f64;
        // 从 wall=0 起就有一份快照，保证每帧都有可插值的基点
        let mut next_snap = 0.0;
        let mut tick = 0u32;
        let mut shown = Vec::with_capacity(frames);
        for _ in 0..frames {
            while next_snap <= wall + 1e-9 {
                tick += 1;
                smoothing.push(tick, Vec3::new(step * tick as f32, 0.0, 0.0), 0.0);
                next_snap += snap_dt;
            }
            shown.push(smoothing.sample(frame_dt as f32).unwrap().0.x);
            wall += frame_dt;
        }
        shown
    }

    fn spread(shown: &[f32], skip: usize) -> (f32, f32) {
        let deltas: Vec<f32> = shown[skip..].windows(2).map(|w| w[1] - w[0]).collect();
        (
            deltas.iter().cloned().fold(f32::INFINITY, f32::min),
            deltas.iter().cloned().fold(f32::NEG_INFINITY, f32::max),
        )
    }

    /// 核心承诺：帧率怎么变，单帧位移都是同一条直线。
    ///
    /// 覆盖整数倍（60/120）、非整数倍（45/50/55/75/90/144）以及用户实测的 75 Hz。
    /// 拿收包时刻当时间轴的实现会在非整数倍上出锯齿（75 Hz 上 5 cm / 2.5 cm 交替），
    /// 因为快照间隔被渲染帧量化成 13.3 / 26.7 ms；tick 轴不会。
    #[test]
    fn every_frame_rate_moves_evenly() {
        for fps in [45.0, 50.0, 55.0, 60.0, 75.0, 90.0, 120.0, 144.0] {
            let shown = run(fps, 400);
            let (min, max) = spread(&shown, 20);
            let ideal = MOVE_SPEED_MPS as f32 / fps as f32;
            assert!(min > 0.0, "{fps} fps 有帧完全没动：{:?}", &shown[..8]);
            assert!(
                (max - min) < ideal * 0.02,
                "{fps} fps 单帧位移 {min}..{max}（理想 {ideal}）摆动过大"
            );
        }
    }

    /// 一帧跨两个 tick（卡了一下）：位移均匀放大，不许瞬移两格。
    #[test]
    fn a_slow_frame_advances_smoothly() {
        let snap_dt = 1.0 / TICK_HZ as f64;
        let step = MOVE_SPEED_MPS * snap_dt as f32;
        let mut smoothing = PoseSmoothing::default();
        let mut wall = 0.0_f64;
        // 从 wall=0 起就有一份快照，保证每帧都有可插值的基点
        let mut next_snap = 0.0;
        let mut tick = 0u32;
        let mut shown = Vec::new();
        for frame in 0..60 {
            while next_snap <= wall + 1e-9 {
                tick += 1;
                smoothing.push(tick, Vec3::new(step * tick as f32, 0.0, 0.0), 0.0);
                next_snap += snap_dt;
            }
            // 第 30 帧卡了 33 ms，之后恢复
            let dt = if frame == 30 { 2.0 / 60.0 } else { 1.0 / 60.0 };
            shown.push(smoothing.sample(dt).unwrap().0.x);
            wall += dt as f64;
        }
        let jump = shown[31] - shown[30];
        assert!(jump < 0.12, "卡顿帧位移 {jump}，等于瞬移两格：{shown:?}");
        // 卡完之后几帧内回到正常步长（时钟只被夹到最新，不欠账）
        let after = shown[35] - shown[34];
        assert!((after - 0.05).abs() < 0.01, "恢复后步长 {after} 未回到 5 cm");
    }

    /// 同一份快照被渲染帧重复喂进来（120 Hz 屏 / 60 Hz 快照）：不许打断插值。
    #[test]
    fn duplicate_snapshots_are_ignored() {
        let dt = 1.0 / 120.0;
        let mut smoothing = PoseSmoothing::default();
        let mut shown = Vec::new();
        for frame in 0..120u32 {
            if frame % 2 == 0 {
                let tick = frame / 2;
                smoothing.push(tick, Vec3::new(0.05 * tick as f32, 0.0, 0.0), 0.0);
                // 复制层同一 tick 可能重复下发，值不变
                smoothing.push(tick, Vec3::new(0.05 * tick as f32, 0.0, 0.0), 0.0);
            }
            shown.push(smoothing.sample(dt).unwrap().0.x);
        }
        let (min, max) = spread(&shown, 10);
        assert!(min > 0.0, "有帧没动：{:?}", &shown[..10]);
        assert!((max - min) < 0.002, "单帧位移 {min}..{max} 不均匀");
    }

    /// 停步：服务端不再发新快照，视觉必须停在最后一份上，不许外推出去。
    #[test]
    fn stop_holds_on_the_last_snapshot() {
        let frame_dt = 1.0 / 75.0;
        let snap_dt = 1.0 / 60.0;
        let mut smoothing = PoseSmoothing::default();
        let mut wall = 0.0_f32;
        // 从 wall=0 起就有一份快照，保证每帧都有可插值的基点
        let mut next_snap = 0.0;
        let mut tick = 0u32;
        let mut last = 0.0;
        let mut pos = Vec3::ZERO;
        for _ in 0..200 {
            while next_snap <= wall + 1e-6 && tick < 12 {
                tick += 1;
                last = 0.05 * tick as f32;
                smoothing.push(tick, Vec3::new(last, 0.0, 0.0), 0.0);
                next_snap += snap_dt;
            }
            pos = smoothing.sample(frame_dt).unwrap().0;
            wall += frame_dt;
        }
        // 200 帧 ≈ 2.7 s，远超一个快照间隔
        assert!(
            (pos.x - last).abs() < 2e-3,
            "停步后多站出 {} m（应在 {last}）",
            pos.x - last
        );
    }

    /// 传送 / 断档超过阈值直接落位，不走慢悠悠的追赶。
    #[test]
    fn teleport_snaps_immediately() {
        let dt = 1.0 / 60.0;
        let mut smoothing = PoseSmoothing::default();
        smoothing.push(0, Vec3::ZERO, 0.0);
        let _ = smoothing.sample(dt);
        // 5 米开外的一次跳变 = 传送
        smoothing.push(1, Vec3::new(5.0, 0.0, 0.0), 0.0);
        let (pos, _) = smoothing.sample(dt).unwrap();
        assert!((pos.x - 5.0).abs() < 1e-3, "传送后还在路上：{pos:?}");
    }

    /// 和解要能按 tick 查到「我当时在哪」：查到的是那一条 tick 的位置，
    /// 没有这个能力，服务端报的位置就没东西可比。
    #[test]
    fn sample_at_returns_the_recorded_tick() {
        let mut smoothing = PoseSmoothing::default();
        for tick in 0..5u32 {
            smoothing.push(tick, Vec3::new(0.05 * tick as f32, 0.0, 0.0), 0.0);
        }
        for tick in 0..5u32 {
            let (pos, _) = smoothing.sample_at(tick).expect("有历史就该查得到");
            assert!((pos.x - 0.05 * tick as f32).abs() < 1e-5, "tick {tick} 查到 {pos:?}");
        }
        // 超出历史范围：宁可不修，也不许拿错帧
        assert!(smoothing.sample_at(900).is_none());
    }

    /// 校正平移必须同时作用于历史：否则下一帧插值还在用旧轨迹，画面会二次跳。
    #[test]
    fn shift_moves_the_whole_history() {
        let mut smoothing = PoseSmoothing::default();
        smoothing.push(0, Vec3::ZERO, 0.0);
        smoothing.push(1, Vec3::new(0.05, 0.0, 0.0), 0.1);
        smoothing.shift(Vec3::new(0.2, 0.0, -0.1), 0.3);
        let (p0, y0) = smoothing.sample_at(0).unwrap();
        let (p1, y1) = smoothing.sample_at(1).unwrap();
        assert!((p0 - Vec3::new(0.2, 0.0, -0.1)).length() < 1e-5);
        assert!((p1 - Vec3::new(0.25, 0.0, -0.1)).length() < 1e-5);
        assert!((y0 - 0.3).abs() < 1e-5 && (y1 - 0.4).abs() < 1e-5, "{y0} {y1}");
    }

    /// 朝向插值走最短一边：跨 ±180° 不许绕远路。
    #[test]
    fn yaw_interpolation_takes_the_short_way() {
        let from = 3.0_f32; // 约 172°
        let to = -3.0_f32; // 约 -172°
        let middle = lerp_yaw(from, to, 0.5);
        assert!(
            middle.abs() > 3.1 && middle.abs() <= std::f32::consts::PI,
            "{middle}"
        );
    }
}
