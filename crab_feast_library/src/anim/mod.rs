//! 动画混合空间：文档、采样权重与姿态求值。

pub mod blendspace;
pub mod mixer;
pub mod clip;
pub mod pose;
pub mod space;
pub mod spec;

pub use blendspace::{weights_1d, weights_2d, Sample1D, Sample2D};
pub use clip::{ClipId, ClipMeta, ClipSet};
pub use pose::{BonePose, Pose};
pub use spec::{builtin_spaces, BlendSpaceSpec, ParamSpec};
pub use mixer::{BlendMixer, MixerCore};
pub use space::{
    evaluate_space, BlendSpaceDoc, SpaceEvalContext, SpaceEvalOutput,
};
