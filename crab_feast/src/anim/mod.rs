//! 游戏侧动画系统：角色模型与剪辑、混合空间求值、把真实移动喂给混合参数。
//!
//! 分层（依赖只允许从 UI 指向这里，反过来不行）：
//! - [`character`]：角色/剪辑清单、模型加载与挂点、单条剪辑播放与根运动位移；
//! - [`mixers`]：各个混合系统资源（移动 / 看向 / 瞄准）、注册表与快照缓存、磁盘读写；
//! - [`driver`]：把启用的混合系统求值成姿态并写到骨骼上；
//! - [`locomotion`]：玩家输入与真实地速 ↔ 「移动混合」的参数、角色的实际移动速度；
//! - [`spaces`]：本作有哪些混合空间（参数轴 + 默认采样点），属于游戏内容。
//!
//! `ui::preview`（动画预览面板）和 `ui::blendspace`（混合空间编辑器）都只是这套系统的
//! 使用者：它们写选择、写编辑请求，不拥有任何动画状态。
//!
//! 注：`turn.rs`（转身剪辑接管腿部与身体旋转）实测观感不如「倒退着走 + 身体逐渐
//! 转向」，已停用、未声明为模块，文件留在原地备查。

pub mod character;
/// 与具体作品无关的混合空间层（文档 / 采样权重 / 姿态求值），只给本作用。
pub mod core;
pub mod locomotion;
pub mod mixers;

mod driver;
mod spaces;

use bevy::prelude::*;

pub use character::{AnimCatalog, AnimSelection, SPEED_RANGE};
pub use driver::{AnimFocus, PoseDriver};
pub use mixers::{MixerCache, MixerEdit, load_space_doc, save_space_doc};

use self::core::ClipSet;

/// 动画系统内部的执行顺序。
///
/// `Edit` 里谁都可以改混合器参数（编辑器的滑杆、真实移动的系统），
/// 但快照只能在所有改动之后重建一次，否则驱动端会读到半新半旧的参数。
#[derive(SystemSet, Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum AnimationSet {
    /// 编辑器把「载入的文档 + 滑杆值」写进 [`MixerEdit`]，本帧稍后才落到混合器上
    Publish,
    /// 对混合器参数的写入（应用编辑副本、真实移动）
    Drive,
    /// 重建快照缓存，供 `PostUpdate` 的姿态驱动读取
    Sync,
}

pub struct AnimPlugin;

impl Plugin for AnimPlugin {
    fn build(&self, app: &mut App) {
        app.add_plugins((
            character::CharacterPlugin,
            mixers::MixerPlugin,
            locomotion::LocomotionPlugin,
        ))
        .init_resource::<PoseDriver>()
        // 剪辑集合与步态元数据由动画系统持有，编辑器只负责往里改文档
        .init_resource::<ClipSet>()
        .init_resource::<AnimFocus>()
        .configure_sets(
            Update,
            (
                AnimationSet::Publish,
                AnimationSet::Drive,
                AnimationSet::Sync,
            )
                .chain(),
        )
        .add_systems(Update, mixers::sync_cache.in_set(AnimationSet::Sync))
        .add_systems(
            PostUpdate,
            driver::drive_pose
                .after(bevy::app::AnimationSystems)
                .before(bevy::transform::TransformSystems::Propagate)
                .run_if(in_state(crate::GameState::Game)),
        );
    }
}
