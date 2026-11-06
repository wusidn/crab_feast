//! 动画资源描述层：剪辑的稳定语义标识与元数据。
//!
//! 混合器节点只认 [`ClipId`]，不认文件路径、更不认数组下标；
//! 换角色时由上层重新填充 [`ClipSet`]，图本身保持不变。

use std::collections::HashMap;

use bevy::{animation::AnimationClip, prelude::*};

/// 剪辑的语义标识，例如 `loco/walk_fwd`。
pub type ClipId = String;

/// 剪辑元数据：混合与同步需要的静态信息。
#[derive(Clone, Debug)]
pub struct ClipMeta {
    /// 时长（秒），用于相位同步与播放倍率；为 0 时回落到资源本身的时长
    pub duration: f32,
    /// 是否循环（一次性动作走 clamp）
    pub looped: bool,
    /// 一个循环的净前进距离（米），用于步幅匹配；未知时为 0
    pub stride: f32,
    /// 步态事件在**本地相位**上的位置，顺序固定为
    /// `[左脚触地, 左脚离地, 右脚触地, 右脚离地]`。
    /// 混合时用它做分段相位重映射，让支撑期两端都对齐。
    pub gait_events: [Option<f32>; 4],
}

impl Default for ClipMeta {
    fn default() -> Self {
        Self {
            duration: 0.0,
            looped: true,
            stride: 0.0,
            gait_events: [None; 4],
        }
    }
}

/// 当前角色可用的剪辑集合。
#[derive(Resource, Default)]
pub struct ClipSet {
    /// 当前 rig 的标识（角色名）
    pub rig: Option<String>,
    handles: HashMap<ClipId, Handle<AnimationClip>>,
    meta: HashMap<ClipId, ClipMeta>,
}

impl ClipSet {
    /// 换角色时整体替换，图里的 ClipId 不变
    pub fn rebuild(&mut self, rig: impl Into<String>) {
        self.rig = Some(rig.into());
        self.handles.clear();
        self.meta.clear();
    }

    pub fn insert(&mut self, id: impl Into<ClipId>, handle: Handle<AnimationClip>) {
        self.handles.insert(id.into(), handle);
    }

    pub fn set_meta(&mut self, id: impl Into<ClipId>, meta: ClipMeta) {
        self.meta.insert(id.into(), meta);
    }

    pub fn handle(&self, id: &str) -> Option<&Handle<AnimationClip>> {
        self.handles.get(id)
    }

    pub fn ids(&self) -> impl Iterator<Item = &str> {
        self.handles.keys().map(|key| key.as_str())
    }

    pub fn len(&self) -> usize {
        self.handles.len()
    }

    pub fn is_empty(&self) -> bool {
        self.handles.is_empty()
    }

    /// 已加载剪辑的时长；资源未就绪时返回 `None`
    pub fn duration(&self, assets: &Assets<AnimationClip>, id: &str) -> Option<f32> {
        let handle = self.handles.get(id)?;
        assets.get(handle).map(|clip| clip.duration())
    }

    /// 剪辑的锚定元数据（字段缺省时补齐：时长从资源取，其余用零值）
    pub fn meta(&self, assets: &Assets<AnimationClip>, id: &str) -> ClipMeta {
        let mut meta = self.meta.get(id).cloned().unwrap_or_default();
        if meta.duration <= f32::EPSILON {
            meta.duration = self.duration(assets, id).unwrap_or(0.0);
        }
        meta
    }
}
