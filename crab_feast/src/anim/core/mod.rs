//! 动画混合空间的通用层：文档、采样权重与姿态求值。
//!
//! 这里不出现任何本作内容（剪辑名、档位、界面文案都在 `super::spaces`），
//! 也不依赖 `crab_feast` 的其他模块——只有 bevy 和 serde。
//! 目前全工程只有本作用它，所以从库里搬了回来；真要第二个作品用时再抽成 crate。

pub mod blendspace;
pub mod clip;
pub mod mixer;
pub mod pose;
pub mod space;
pub mod spec;

// 只重导出上层真正用到的名字，其余从各自子模块里拿（如 `core::pose::Pose`）。
pub use blendspace::Sample2D;
pub use clip::{ClipMeta, ClipSet};
pub use mixer::{BlendMixer, MixerCore};
pub use space::{BlendSpaceDoc, SpaceEvalContext, evaluate_space};
pub use spec::{BlendSpaceSpec, ParamSpec};
