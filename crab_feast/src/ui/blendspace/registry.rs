//! 混合系统注册表：多个混合器各自持有一个 Resource，预览通过类型擦除的访问器读取/写回。
//!
//! 新增一个混合系统：在库里声明 newtype（`struct AimMixer(pub MixerCore)`）+ `impl_blend_mixer!`，
//! 再在 `init_mixers` 里 `commands.insert_resource(...)` 并 `registry.register::<T>()`。

use std::collections::HashMap;

use bevy::prelude::*;
use bevy::ecs::component::Mutable;
use crab_feast_library::anim::{
    builtin_spaces, BlendMixer, BlendSpaceDoc, BlendSpaceSpec, MixerCore, Sample2D,
};
use crab_feast_library::impl_blend_mixer;

use crate::ui::preview::{find_asset_root, PreviewCatalog, PreviewState};

use super::{ForceRebuild, SpaceEditor};

#[derive(Resource)]
pub struct LocomotionMixer(pub MixerCore);
#[derive(Resource)]
pub struct LookMixer(pub MixerCore);
#[derive(Resource)]
pub struct AimMixer(pub MixerCore);

impl_blend_mixer!(LocomotionMixer);
impl_blend_mixer!(LookMixer);
impl_blend_mixer!(AimMixer);

/// 某一帧的混合器快照（预览与驱动只读这份）
#[derive(Clone, Debug)]
pub struct MixerSnapshot {
    pub key: String,
    pub title: String,
    pub description: String,
    pub space: BlendSpaceDoc,
    pub params: HashMap<String, f32>,
    pub enabled: bool,
    pub character: String,
}

#[derive(Clone, Debug)]
pub enum MixerEdit {
    Param { axis: String, value: f32 },
    Space(BlendSpaceDoc),
    Enabled(bool),
}

#[derive(Clone, Copy)]
pub struct MixerEntry {
    snapshot: fn(&World) -> MixerSnapshot,
    edit: fn(&mut World, MixerEdit),
}

#[derive(Resource, Default)]
pub struct MixerRegistry {
    pub entries: Vec<MixerEntry>,
}

impl MixerRegistry {
    pub fn register<T: BlendMixer + Component<Mutability = Mutable> + 'static>(&mut self) {
        self.entries.push(MixerEntry {
            snapshot: snapshot_of::<T>,
            edit: edit_of::<T>,
        });
    }
}

/// 当前帧所有混合器的快照，供预览与姿态驱动读取
#[derive(Resource, Default)]
pub struct MixerCache {
    pub mixers: Vec<MixerSnapshot>,
}

fn snapshot_of<T: BlendMixer + 'static>(world: &World) -> MixerSnapshot {
    let core = world.resource::<T>().core();
    MixerSnapshot {
        key: core.key.clone(),
        title: core.title.clone(),
        description: core.description.clone(),
        space: core.space.clone(),
        params: core.params.clone(),
        enabled: core.enabled,
        character: core.character.clone(),
    }
}

fn edit_of<T: BlendMixer + Component<Mutability = Mutable> + 'static>(world: &mut World, edit: MixerEdit) {
    let Some(mut mixer) = world.get_resource_mut::<T>() else {
        return;
    };
    let core = mixer.core_mut();
    match edit {
        MixerEdit::Param { axis, value } => {
            core.params.insert(axis, value);
        }
        MixerEdit::Space(space) => core.space = space,
        MixerEdit::Enabled(enabled) => core.enabled = enabled,
    }
}

/// 游戏进入时创建三个混合系统：移动 / 看向 / 瞄准
pub(super) fn init_mixers(
    mut commands: Commands,
    catalog: Res<PreviewCatalog>,
    preview: Res<PreviewState>,
) {
    let character = catalog
        .character(preview.character)
        .map(|def| def.name.clone())
        .unwrap_or_default();

    let specs = builtin_spaces();
    let mut registry = MixerRegistry::default();

    for spec in &specs {
        let doc = load_or_default(spec, &catalog, preview.character);
        let mut core = MixerCore::new(spec.id.clone(), spec.title.clone(), doc, character.clone());
        core.description = spec.description.clone();
        // 默认只让移动系统在游戏里生效，看向/瞄准按需手动启用
        core.enabled = spec.id == "locomotion";
        for (axis, value) in spec.default_params() {
            core.params.insert(axis.to_string(), value);
        }
        match spec.id.as_str() {
            "locomotion" => {
                commands.insert_resource(LocomotionMixer(core));
                registry.register::<LocomotionMixer>();
            }
            "look" => {
                commands.insert_resource(LookMixer(core));
                registry.register::<LookMixer>();
            }
            "aim" => {
                commands.insert_resource(AimMixer(core));
                registry.register::<AimMixer>();
            }
            _ => {}
        }
    }

    info!(
        "已注册 {} 个混合系统：{}",
        registry.entries.len(),
        specs
            .iter()
            .map(|spec| spec.title.clone())
            .collect::<Vec<_>>()
            .join(" / ")
    );
    commands.insert_resource(registry);
}

fn load_or_default(
    spec: &BlendSpaceSpec,
    catalog: &PreviewCatalog,
    character: usize,
) -> BlendSpaceDoc {
    if let Some(path) = space_file(&spec.id) {
        if let Ok(text) = std::fs::read_to_string(&path) {
            match serde_json::from_str::<BlendSpaceDoc>(&text) {
                Ok(doc) => return doc,
                Err(error) => warn!("混合空间 {} 解析失败：{error}", path.display()),
            }
        }
    }

    let mut doc = spec.default_doc();
    if let Some(def) = catalog.character(character) {
        doc.samples.retain(|sample| def.clips.contains(&sample.clip));
        if doc.samples.is_empty() {
            if let Some(clip) = def.clips.first() {
                doc.samples.push(Sample2D {
                    at: [0.0, 0.0],
                    clip: clip.clone(),
                    reverse: false,
                });
            }
        }
    }
    doc
}

pub fn space_file(key: &str) -> Option<std::path::PathBuf> {
    find_asset_root().map(|root| root.join(format!("animgraph/{key}.json")))
}

/// 每帧：刷新快照缓存；把预览里的编辑写回对应混合器，或切换混合器时载入它的文档。
pub(super) fn sync_mixers(world: &mut World) {
    let entries = world.resource::<MixerRegistry>().entries.clone();
    let snapshots: Vec<MixerSnapshot> = entries.iter().map(|entry| (entry.snapshot)(&*world)).collect();

    if let Some(mut cache) = world.get_resource_mut::<MixerCache>() {
        cache.mixers = snapshots.clone();
    } else {
        world.insert_resource(MixerCache {
            mixers: snapshots.clone(),
        });
    }

    let (open, index, loaded, doc, params, enabled) = {
        let editor = world.resource::<SpaceEditor>();
        (
            editor.open,
            editor.mixer,
            editor.loaded_key.clone(),
            editor.doc.clone(),
            editor.params.clone(),
            editor.enabled,
        )
    };
    if !open {
        return;
    }
    let Some(snapshot) = snapshots.get(index) else {
        return;
    };

    if loaded.as_deref() != Some(snapshot.key.as_str()) {
        // 切换了混合系统：把它的文档载入编辑器
        let mut editor = world.resource_mut::<SpaceEditor>();
        editor.loaded_key = Some(snapshot.key.clone());
        editor.doc = snapshot.space.clone();
        editor.params = snapshot.params.clone();
        editor.enabled = snapshot.enabled;
        editor.selected = None;
        editor.revision = editor.revision.wrapping_add(1);
        world.resource_mut::<ForceRebuild>().0 = true;
        return;
    }

    let entry = entries[index];
    if doc != snapshot.space {
        (entry.edit)(&mut *world, MixerEdit::Space(doc));
    }
    if enabled != snapshot.enabled {
        (entry.edit)(&mut *world, MixerEdit::Enabled(enabled));
    }
    for (axis, value) in params {
        if snapshot.params.get(&axis).copied().unwrap_or(f32::NAN) != value {
            (entry.edit)(
                &mut *world,
                MixerEdit::Param {
                    axis,
                    value,
                },
            );
        }
    }
}
