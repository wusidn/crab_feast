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
#[derive(Resource, Debug)]
pub struct MixerCache {
    pub mixers: Vec<MixerSnapshot>,
    /// 混合器资源改过、缓存需要重建。
    ///
    /// 这里必须自己维护脏标记：独占系统里的 `World::is_resource_changed`
    /// 是以「上一帧 `clear_trackers`」为基准的，读不到上一帧 Update 里写进去的改动，
    /// 靠它判断会让缓存永远停在首次构建的状态（改了参数但动画不动）。
    pub dirty: bool,
}

impl Default for MixerCache {
    fn default() -> Self {
        Self {
            mixers: Vec::new(),
            dirty: true,
        }
    }
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
    {
        let core = mixer.core_mut();
        match edit {
            MixerEdit::Param { axis, value } => {
                core.params.insert(axis, value);
            }
            MixerEdit::Space(space) => core.space = space,
            MixerEdit::Enabled(enabled) => core.enabled = enabled,
        }
    }
    // 混合器内容变了：快照缓存必须重建，否则驱动端一直读到改之前的参数
    if let Some(mut cache) = world.get_resource_mut::<MixerCache>() {
        cache.dirty = true;
    }
}

/// 重建所有混合器快照并清掉脏标记
fn refresh_snapshots(world: &mut World) {
    let snapshots: Vec<MixerSnapshot> = {
        let registry = world.resource::<MixerRegistry>();
        registry
            .entries
            .iter()
            .map(|entry| (entry.snapshot)(&*world))
            .collect()
    };
    let mut cache = world.get_resource_or_insert_with(MixerCache::default);
    cache.mixers = snapshots;
    cache.dirty = false;
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
                Ok(mut doc) => {
                    // 老文档把 speed 写成 0..1 之类的窄区间时，负向参数会被滑杆截掉
                    if doc.fit_ranges() {
                        warn!(
                            "混合空间 {} 的轴范围没包住采样点，已按采样点扩张为 {:?}",
                            path.display(),
                            doc.axis_ranges()
                        );
                    }
                    return doc;
                }
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

/// 每帧：按需刷新快照缓存；把预览里的编辑写回对应混合器，或切换混合器时载入它的文档。
///
/// 快照里带着整份文档和参数表，**只在真有过改动时**才重建——
/// 无条件每帧克隆会白做几十次分配；重建凭 [`MixerCache::dirty`]，不靠 World 的变更检测。
pub(super) fn sync_mixers(world: &mut World) {
    let dirty = world
        .get_resource::<MixerCache>()
        .map_or(true, |cache| cache.dirty || cache.mixers.is_empty());
    if dirty {
        refresh_snapshots(world);
    }

    let (open, index) = {
        let editor = world.resource::<SpaceEditor>();
        (editor.open, editor.mixer)
    };
    if !open {
        return;
    }

    let needs_load = {
        let cache = world.resource::<MixerCache>();
        let Some(snapshot) = cache.mixers.get(index) else {
            return;
        };
        let editor = world.resource::<SpaceEditor>();
        editor.loaded_key.as_deref() != Some(snapshot.key.as_str())
    };

    if needs_load {
        // 切换了混合系统：把它的文档载入编辑器
        let (key, space, params, enabled) = {
            let cache = world.resource::<MixerCache>();
            let snapshot = &cache.mixers[index];
            (
                snapshot.key.clone(),
                snapshot.space.clone(),
                snapshot.params.clone(),
                snapshot.enabled,
            )
        };
        let mut editor = world.resource_mut::<SpaceEditor>();
        editor.loaded_key = Some(key);
        editor.doc = space;
        editor.params = params;
        editor.enabled = enabled;
        editor.selected = None;
        editor.revision = editor.revision.wrapping_add(1);
        world.resource_mut::<ForceRebuild>().0 = true;
        return;
    }

    // 逐项比对差异后再写回：没有差异时连一次克隆都不做
    let (edits, edit_fn) = {
        let cache = world.resource::<MixerCache>();
        let snapshot = &cache.mixers[index];
        let editor = world.resource::<SpaceEditor>();
        let mut edits = Vec::new();
        if editor.doc != snapshot.space {
            edits.push(MixerEdit::Space(editor.doc.clone()));
        }
        if editor.enabled != snapshot.enabled {
            edits.push(MixerEdit::Enabled(editor.enabled));
        }
        for (axis, value) in &editor.params {
            if snapshot.params.get(axis).copied().unwrap_or(f32::NAN) != *value {
                edits.push(MixerEdit::Param {
                    axis: axis.clone(),
                    value: *value,
                });
            }
        }
        let registry = world.resource::<MixerRegistry>();
        let edit_fn = registry.entries.get(index).map(|entry| entry.edit);
        (edits, edit_fn)
    };

    let Some(edit_fn) = edit_fn else {
        return;
    };
    if edits.is_empty() {
        return;
    }
    for edit in edits {
        edit_fn(&mut *world, edit);
    }
    // 写完立刻刷新：否则本帧 PostUpdate 的驱动还会按旧参数求值，拖拽会慢一帧
    refresh_snapshots(world);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn build_world() -> World {
        let mut world = World::new();
        let mut doc = BlendSpaceDoc::default();
        doc.samples.push(Sample2D {
            at: [0.0, 0.5],
            clip: "Walking".to_string(),
            reverse: false,
        });
        let mut core = MixerCore::new(
            "locomotion".to_string(),
            "移动混合".to_string(),
            doc,
            "Y Bot".to_string(),
        );
        core.params.insert("yaw".to_string(), 0.0);
        core.params.insert("speed".to_string(), 0.5);
        world.insert_resource(LocomotionMixer(core));

        let mut registry = MixerRegistry::default();
        registry.register::<LocomotionMixer>();
        world.insert_resource(registry);
        world.insert_resource(MixerCache::default());
        world.insert_resource(SpaceEditor {
            open: true,
            mixer: 0,
            ..SpaceEditor::default()
        });
        world.insert_resource(ForceRebuild(false));
        world
    }

    /// 跑一帧的尾巴：游戏里每帧结束都会 `clear_trackers`，
    /// 而它会把 `World::last_change_tick` 推到当前 tick——这正是独占系统里
    /// `is_resource_changed` 读不到上一帧改动的原因。
    fn end_frame(world: &mut World) {
        world.clear_trackers();
    }

    /// 改了参数后快照必须跟着变：驱动端只读快照，缓存不重建就等于参数没改
    #[test]
    fn param_edit_reaches_snapshot() {
        let mut world = build_world();
        sync_mixers(&mut world);
        end_frame(&mut world);

        world
            .resource_mut::<SpaceEditor>()
            .params
            .insert("speed".to_string(), 1.0);
        sync_mixers(&mut world);

        let cached = world.resource::<MixerCache>().mixers[0].params["speed"];
        assert!((cached - 1.0).abs() < 1e-6, "缓存里的 speed 应为 1.0，实际 {cached}");
        let live = world.resource::<LocomotionMixer>().0.params["speed"];
        assert!((live - 1.0).abs() < 1e-6, "混合器本体也应被写回，实际 {live}");

        end_frame(&mut world);
        // 帧界一过，World 的变更检测就看不见这一帧的改动了——所以脏标记必须自己记
        assert!(
            !world.is_resource_changed::<LocomotionMixer>(),
            "前提变了：World 的变更检测居然还能看见上一帧的改动"
        );

        sync_mixers(&mut world);
        assert!(!world.resource::<MixerCache>().dirty, "无改动时不应保持脏标记");
    }

    /// 文档改动同样要立刻进入快照（采样点/范围编辑走这条路径）
    #[test]
    fn space_edit_reaches_snapshot() {
        let mut world = build_world();
        sync_mixers(&mut world);
        end_frame(&mut world);

        world.resource_mut::<SpaceEditor>().doc.speed = 2.0;
        sync_mixers(&mut world);

        let cached = world.resource::<MixerCache>().mixers[0].space.speed;
        assert!((cached - 2.0).abs() < 1e-6, "缓存的文档 speed 应为 2.0，实际 {cached}");
    }
}
