//! 滞空判定：接地噪声阈值、离地确认、起跳失败与起跳初速。
//!
//! 这里的常量决定「人是真的飞起来了」还是「被物理吃回去了」。

use super::tuning::JumpTuning;

/// 空中速度补偿用的重力（Avian 默认 -9.81）
pub(super) const GRAVITY: f32 = 9.81;

/// 接地判定允许的残余上升速度（m/s）。
///
/// 贴地时 avian 的接触求解会留下 ±0.2 m/s 量级的残余速度（实测 0.192、
/// -0.135 反复出现）。拿 0.1 当「已经蹬升」的界线，会把这种噪声当成起跳
/// 初速（`v²/2g` 只有几毫米）或把接地判成悬空。界线要拉开噪声一个量级。
pub(super) const GROUND_NOISE_VY: f32 = 0.5;
/// 起跳宽限：进入滞空后这段时间内不做接地判定，避免蹬离瞬间射线仍贴地误判落地。
pub(super) const AIR_GRACE_SECS: f32 = 0.08;
/// 确认离地的最小高度（米）：脚底距地超过它才算真的离开地面。
/// 滞空段要等到「确实抬起来过」才允许判定落地，防止小速度起跳时在贴着地面
/// 的第 0.08 秒就被判落地、整段跳跃变成抖一下。
pub(super) const AIR_LIFT_DIST: f32 = 0.18;
/// 起跳失败判定窗口（单机 Dynamic，秒）：进入滞空段这么久，根没抬起来、脚底也没离地，
/// 就说明这一跳被物理吃回去了（顶棚 / 贴身的别的刚体 / 摩擦卡住）。
///
/// 不能等 [`JumpTuning::max_air_hold`] 兜底：那两秒里人明明站在地上，动画却一直锁在
/// 滞空姿态上，玩家看到的是「按了跳，人卡在半空不动」。
pub(super) const TAKEOFF_FAIL_SECS: f32 = 0.15;
/// 起跳失败判定窗口（联网 Kinematic，秒）：根位置由服务端复制、比按键晚半个 RTT 才到，
/// 窗口要留够，别把一次真起跳错判成失败。
pub(super) const NET_TAKEOFF_FAIL_SECS: f32 = 0.25;
/// 「根真的升起来了」的位移阈值（米）：与 [`AIR_LIFT_DIST`] 同口径。
///
/// 不能用「抬了一点点」当成功：顶棚只差几厘米时，离散求解器在起跳那一 tick 里
/// 仍能让人穿进去几厘米再被推回来，那不是一次起跳，是撞顶。真要算成功，
/// 根得抬到脚离地那一步——低于它，玩家看到的就是「蹲一下、跳一下、人还在原地」。
pub(super) const TAKEOFF_RISE_EPS: f32 = AIR_LIFT_DIST;
/// 起跳失败后动作层的淡出时长（秒）：这一跳没有落地冲击可演，不走完整的落地恢复段。
pub(super) const ABORT_BLEND_SECS: f32 = 0.08;
/// 远端观察起跳的最小上升速度（m/s）：走空坠落是负速度，不该播起跳动作。
pub(super) const REMOTE_JUMP_DETECT_VY: f32 = 1.0;

/// 滞空段是否该判「起跳失败」。
///
/// 三个条件必须同时成立：根没升起来（`climbed == false`）、脚底也没抬离地面过
/// （`air_lifted == false`）、而且人还贴在地上（`grounded`）。任一不成立就说明
/// 这具身体真的飞起来了，按正常滞空走完，绝不提前收尾——宁可晚退，也不能把
/// 一次真起跳的动画剪掉。
pub(super) fn takeoff_failed(
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
pub(super) fn launch_speed(measured: f32, tuning: JumpTuning) -> f32 {
    measured.max(tuning.launch_vy)
}
