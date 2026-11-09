//! 混合器资源、注册表与快照缓存 —— 全项目唯一的混合系统真源。
//!
//! 一个混合空间对应一个 Resource（`LocomotionMixer` / `LookMixer` / `AimMixer`），
//! 里面同时装着「定义」（可编辑的 `BlendSpaceDoc`）和「当前参数」：
//!
//! - 游戏代码按自己的类型写参数，例如 `ResMut<LocomotionMixer>`；
//! - 预览编辑器不持有状态，只发一帧一次的 `MixerEdit` 请求；
//! - 每帧把这些资源汇总成 `MixerCache` 快照，姿态驱动和 UI 都只读这份快照。
//!
//! 新增一个混合空间：写一个新类型 + 实现 `RegisteredMixer`，再在
//! [`MixerRegistry::builtin`] 与 [`sync_cache`] 里各加一行。

use std::collections::HashMap;
use std::path::PathBuf;

use crate::impl_blend_mixer;
use bevy::ecs::component::Mutable;
use bevy::prelude::*;

use super::AnimationSet;
use super::character::{AnimCatalog, AnimSelection, find_asset_root};
use super::core::{BlendMixer, BlendSpaceDoc, BlendSpaceSpec, MixerCore};
use super::spaces::game_spaces;

/// 移动：朝向角 × 速度，覆盖 Idle / Walk / Run / Strafe / Backpedal
#[derive(Resource, Default, Clone, Debug)]
pub struct LocomotionMixer(pub MixerCore);
impl_blend_mixer!(LocomotionMixer);

/// 抬头低头：上半身独立于移动
#[derive(Resource, Default, Clone, Debug)]
pub struct LookMixer(pub MixerCore);
impl_blend_mixer!(LookMixer);

/// 瞄准：侧身上限 × 俯仰上限
#[derive(Resource, Default, Clone, Debug)]
pub struct AimMixer(pub MixerCore);
impl_blend_mixer!(AimMixer);

/// 混合器身份：类型 ↔ 空间 id。id 同时是资源 key、注册表顺序和存档文件名主干。
pub trait RegisteredMixer: BlendMixer {
    const ID: &'static str;

    fn from_core(core: MixerCore) -> Self;
}

impl RegisteredMixer for LocomotionMixer {
    const ID: &'static str = "locomotion";
    fn from_core(core: MixerCore) -> Self {
        Self(core)
    }
}

impl RegisteredMixer for LookMixer {
    const ID: &'static str = "look";
    fn from_core(core: MixerCore) -> Self {
        Self(core)
    }
}

impl RegisteredMixer for AimMixer {
    const ID: &'static str = "aim";
    fn from_core(core: MixerCore) -> Self {
        Self(core)
    }
}

/// 本作注册的空间定义：混合器初始化时按 id 取参数与采样点
#[derive(Resource)]
pub struct AnimSpaces(pub Vec<BlendSpaceSpec>);

impl Default for AnimSpaces {
    fn default() -> Self {
        Self(game_spaces())
    }
}

impl AnimSpaces {
    pub fn spec(&self, id: &str) -> Option<&BlendSpaceSpec> {
        self.0.iter().find(|spec| spec.id == id)
    }
}

/// 注册表在启动时由代码写死，不扫盘、不猜类型。
///
/// 只存 id：标题跟着空间定义走（快照里已经带了一份），多存一份就会对不上。
#[derive(Resource)]
pub struct MixerRegistry {
    /// 已注册的混合器 id，顺序即预览标签顺序与叠加顺序
    pub ids: Vec<&'static str>,
}

impl MixerRegistry {
    /// 已注册类型 + 本作空间定义 → 注册表；没登记空间定义的类型不进来。
    pub fn builtin(spaces: &[BlendSpaceSpec]) -> Self {
        let candidates: [&'static str; 3] = [LocomotionMixer::ID, LookMixer::ID, AimMixer::ID];
        Self {
            ids: candidates
                .into_iter()
                .filter(|id| spaces.iter().any(|spec| spec.id == *id))
                .collect(),
        }
    }
}

/// 混合器某一帧的只读视图：驱动与 UI 的唯一数据来源
#[derive(Clone, Debug)]
pub struct MixerSnapshot {
    pub key: String,
    pub title: String,
    pub description: String,
    /// 当前生效的空间定义（编辑结果已合并进来）
    pub space: BlendSpaceDoc,
    pub params: HashMap<String, f32>,
    pub enabled: bool,
    /// 这份混合空间属于哪个角色；不在的角色不驱动
    pub character: String,
}

impl MixerSnapshot {
    fn from_core(core: &MixerCore) -> Self {
        Self {
            key: core.key.clone(),
            title: core.title.clone(),
            description: core.description.clone(),
            space: core.space.clone(),
            params: core.params.clone(),
            enabled: core.enabled,
            character: core.character.clone(),
        }
    }
}

/// 每帧由混合器资源重建的快照表，顺序与注册表一致。只读：写请求走 [`MixerEdit`]。
#[derive(Resource, Default)]
pub struct MixerCache {
    pub mixers: Vec<MixerSnapshot>,
}

/// UI 的编辑请求：每帧把聚焦混合器的工作副本写回资源。
/// `mixer` 为 None 表示没有编辑器在改，参数归游戏代码所有。
#[derive(Resource, Default)]
pub struct MixerEdit {
    pub mixer: Option<String>,
    pub doc: BlendSpaceDoc,
    pub params: HashMap<String, f32>,
    pub enabled: bool,
}

/// 落盘位置：`assets/characters/animations/<角色>/blend_spaces/<key>.json`
pub fn space_file(character: &str, key: &str) -> Option<PathBuf> {
    let root = find_asset_root()?;
    Some(
        root.join("characters")
            .join("animations")
            .join(character)
            .join("blend_spaces")
            .join(format!("{key}.json")),
    )
}

/// 读磁盘上的混合空间；轴范围不合用时按采样点扩张，避免采样点评测时权重为 0
pub fn load_space_doc(character: &str, key: &str) -> Option<BlendSpaceDoc> {
    let path = space_file(character, key)?;
    let content = std::fs::read_to_string(&path).ok()?;
    let mut doc = match serde_json::from_str::<BlendSpaceDoc>(&content) {
        Ok(doc) => doc,
        Err(error) => {
            warn!("混合空间 {} 解析失败：{error}", path.display());
            return None;
        }
    };
    if doc.fit_ranges() {
        warn!(
            "混合空间 {} 的轴范围没包住采样点，已按采样点扩张为 {:?}",
            path.display(),
            doc.axis_ranges()
        );
    }
    Some(doc)
}

/// 把文档写回磁盘，成功时返回路径文案给 UI 显示
pub fn save_space_doc(character: &str, key: &str, doc: &BlendSpaceDoc) -> Result<String, String> {
    let path = space_file(character, key).ok_or("找不到 assets 目录")?;
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let json = serde_json::to_string_pretty(doc).map_err(|error| error.to_string())?;
    std::fs::write(&path, json).map_err(|error| error.to_string())?;
    Ok(path.display().to_string())
}

/// 内置定义 → 文档：丢掉当前角色没有剪辑的采样点
fn doc_for(spec: &BlendSpaceSpec, available: &[String]) -> BlendSpaceDoc {
    let mut doc = spec.default_doc();
    doc.samples
        .retain(|sample| available.iter().any(|clip| clip == &sample.clip));
    doc
}

/// 资源还不属于当前角色时重建：优先磁盘，其次内置定义按可用剪辑过滤
fn ensure_mixer<M>(
    spaces: Res<AnimSpaces>,
    catalog: Res<AnimCatalog>,
    selection: Res<AnimSelection>,
    mut mixer: ResMut<M>,
) where
    M: RegisteredMixer + Resource<Mutability = Mutable>,
{
    let Some(spec) = spaces.spec(M::ID) else {
        return;
    };
    let Some(def) = catalog.character(selection.character) else {
        return;
    };
    if mixer.core().key == M::ID && mixer.core().character == def.name {
        return; // 已经是当前角色，编辑结果不能被动覆盖
    }

    let space = load_space_doc(&def.name, M::ID).unwrap_or_else(|| doc_for(spec, &def.clips));
    let mut core = MixerCore::new(M::ID, spec.title.clone(), space, def.name.clone());
    core.description = spec.description.clone();
    // 只有移动混合默认开启：多个混合器同时生效时后者覆盖前者同名骨骼，
    // look / aim 一开就会把整具骨架抢走。要叠加得先在库里加姿态叠加原语。
    core.enabled = M::ID == "locomotion";
    // 参数初值取轴定义的 default，不要一律归零：speed 的 default 是走档
    for (name, value) in spec.default_params() {
        core.params.insert(name.to_string(), value);
    }
    *mixer = M::from_core(core);
}

/// 把聚焦混合器的编辑副本写回资源。编辑器没打开时什么都不做。
fn apply_edits<M>(edit: Res<MixerEdit>, mut mixer: ResMut<M>)
where
    M: RegisteredMixer + Resource<Mutability = Mutable>,
{
    if edit.mixer.as_deref() != Some(M::ID) {
        return;
    }
    let core = mixer.core_mut();
    if core.space != edit.doc {
        core.space = edit.doc.clone();
    }
    core.params = edit.params.clone();
    core.enabled = edit.enabled;
}

/// 快照表每帧重建：条目只有几条，比增量同步更难写错
pub(crate) fn sync_cache(
    registry: Res<MixerRegistry>,
    locomotion: Res<LocomotionMixer>,
    look: Res<LookMixer>,
    aim: Res<AimMixer>,
    mut cache: ResMut<MixerCache>,
) {
    let cores = [locomotion.core(), look.core(), aim.core()];
    let mut mixers = Vec::with_capacity(registry.ids.len());
    for id in &registry.ids {
        if let Some(core) = cores.iter().find(|core| core.key == *id) {
            mixers.push(MixerSnapshot::from_core(core));
        }
    }
    cache.mixers = mixers;
}

pub struct MixerPlugin;

impl Plugin for MixerPlugin {
    fn build(&self, app: &mut App) {
        let spaces = AnimSpaces::default();
        let registry = MixerRegistry::builtin(&spaces.0);
        if registry.ids.is_empty() {
            warn!("没有任何已注册的混合空间，动画预览与移动驱动都不会工作");
        }

        app.insert_resource(spaces)
            .insert_resource(registry)
            .init_resource::<MixerCache>()
            .init_resource::<MixerEdit>()
            .init_resource::<LocomotionMixer>()
            .init_resource::<LookMixer>()
            .init_resource::<AimMixer>()
            .add_systems(
                Update,
                (
                    ensure_mixer::<LocomotionMixer>,
                    ensure_mixer::<LookMixer>,
                    ensure_mixer::<AimMixer>,
                    apply_edits::<LocomotionMixer>,
                    apply_edits::<LookMixer>,
                    apply_edits::<AimMixer>,
                )
                    .in_set(AnimationSet::Drive),
            )
            .add_systems(Update, sync_cache.in_set(AnimationSet::Sync));
    }
}
