//! 跳跃动作层：一次性动作叠在移动混合空间之上，不参与参数场。
//!
//! 移动混合空间只装循环步态（Idle / Walk / Run / 侧移 / 倒退），它的采样
//! 权重同时决定地速和支撑权重，所以一次性动作不能放进去。这里单独一层：
//! 自己的播放头、自己的时间轴、自己的垂直位移，播完把权重交回移动混合。
//!
//! # 模块划分
//!
//! | 文件 | 职责 |
//! |---|---|
//! | [`resources`] | 资源参数表：剪辑名、锚点、档位、支撑 / 落地脚、回退链 |
//! | `tuning.rs` | 跳跃大类、四档调参（站 / 跑 / 上 / 下） |
//! | `state.rs` | 运行时状态：`JumpState` / `JumpAction` / 策略资源 |
//! | `timeline.rs` | 剪辑时间轴、髋高曲线采样、离地 / 落地事件检测 |
//! | `selection.rs` | 台阶探测、支撑脚判定、剪辑选择与缺件回退 |
//! | `gait.rs` | 起跳窗口：步态相位、单脚支撑、排队超时、上行电平 |
//! | `air.rs` | 滞空物理判定：接地噪声、起跳失败、起跳初速 |
//! | `landing.rs` | 落地恢复段的相位交接 |
//! | `remote.rs` | 远端 / 模拟端观察式跳跃 |
//! | `drive.rs` | 本地状态机与 `begin` |
//! | `blend.rs` | 跳跃姿态混入移动姿态 |
//! | `plugin.rs` | 系统注册 |
//!
//! # 资源参数表
//!
//! 剪辑名、时间轴锚点、蓄力策略、支撑脚 / 落地脚约定、缺件回退与默认手感参数，
//! 全部集中在 [`resources`] 的 [`POSES`](resources::POSES) 表里，与
//! `assets/characters/animations/<角色>/*.glb` 一一对应。状态机只按 [`JumpPose`]
//! 查表，不写死任何剪辑名或剪辑专属数字；新增一种跳跃姿态 = 资源目录放 glb +
//! 表里加一行。剪辑实测锚点见 [`resources`] 的模块文档。
//!
//! # 起跳分流（起跳瞬间判定一次，整段动作不再切换）
//!
//! 选择规则参数见 [`RULES`]：
//!
//! 1. **高差优先**：从脚下与前方落点各打一条向下的地面射线。落点比当前地面高过
//!    `step_height` → [`JumpPose::Up`]；低过阈值 → [`JumpPose::Down`]；其余按平地跳处理。
//! 2. **平地跳再按速度分档**：地速低于 [`JumpRules::run_jump_speed`] 用原地双脚跳
//!    [`JumpPose::Stand`]；处于奔跑档用 [`JumpPose::Run1`] / [`JumpPose::Run2`] /
//!    [`JumpPose::Run3`] 三套里的第 `n` 套（`n` = 起跳序号，连续起跳 1 → 2 → 3 → 1
//!    轮换，见 [`JumpAction::run_variant`]），再按起跳支撑脚选源片或
//!    [`JumpPose::Run1Left`] / [`JumpPose::Run2Right`] / [`JumpPose::Run3Left`] 镜像片
//!    （由 `scripts/make_runjump_mirror.py` 程序化生成）。
//!
//! 滞空段是否结束看物理接地（脚下射线 + 残余上升速度归零），不按剪辑固定时长提前退出：
//! 整段腾空弧线按预测滞空慢放拉伸，播完钉在落地帧等接地，接地后才播放压缩的落地恢复段并淡出。
//!
//! # 起跳失败（表现层兜底）
//!
//! 写进物理的离地速度不等于人真的离地：顶棚、贴过来的其他刚体、贴墙的摩擦都会在
//! 接触求解里把它当场吃掉。这种情况下人还站在原处，动作层不能继续锁在滞空姿态上
//! 等 [`JumpTuning::max_air_hold`] 的兜底超时——那两秒玩家看到的是「按了跳，人卡在半空」。
//! 判定条件见 `air::takeoff_failed`：根没抬起来、脚底也没离地、人仍然贴地，窗口一到就进
//! [`JumpState::Abort`]：姿态定格在剪辑的离地帧（不跳到落地恢复段），在
//! `air::ABORT_BLEND_SECS` 内把权重淡到零交回移动混合，不演一次假的落地缓冲。
//! 滞空姿态的兜底上限来自这一档的 [`JumpTuning::max_air_hold`]。
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

mod air;
mod blend;
mod drive;
mod gait;
mod landing;
mod plugin;
mod remote;
pub mod resources;
mod selection;
mod settings;
mod state;
mod timeline;
mod tuning;

pub use gait::uplink_jump_level;
pub use plugin::JumpPlugin;
pub use remote::{RemoteJump, RemoteJumpSetup, remote_jump_setup};
pub use resources::{
    Anticipation, JumpClipSpec, JumpPose, JumpPoseDef, JumpRules, RULES, SupportFoot,
};
pub use selection::pick_clip;
pub use settings::JumpSettings;
pub use state::{JumpAction, JumpPolicy, JumpState, JumpTiming};
pub use tuning::{JumpKind, JumpProfile, JumpTuning, JumpTunings};

pub(crate) use blend::apply_action;
#[cfg(feature = "net")]
pub(crate) use blend::blend_clip_pose;

#[cfg(test)]
mod tests;
