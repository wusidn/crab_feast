//! 角色动画运行时：扫描角色与剪辑清单、加载模型并挂到 `Avatar` 下、按选择播放单条剪辑、
//! 处理根运动的位移补偿。
//!
//! 这是游戏系统：进入 `GameState::Game` 后玩家身上的角色由这里生成，
//! 「动画预览」面板只是改 [`AnimSelection`] 的一个使用者。

use std::{
    collections::{BTreeSet, HashMap},
    path::{Path, PathBuf},
    time::Duration,
};

use super::core::{ClipMeta, ClipSet};
use super::driver::PoseDriver;
use crate::{GameState, scene::Avatar};
use bevy::{app::AnimationSystems, prelude::*, transform::TransformSystems};

pub struct CharacterPlugin;

/// 角色模型相对挂点（Avatar）的初始偏移
const MODEL_BASE: Vec3 = Vec3::new(0.0, -0.7, 0.0);

/// 倍速滑动条的范围
pub const SPEED_RANGE: (f32, f32) = (0.1, 3.0);

/// 扫描 `assets/characters` 得到的角色与剪辑清单
#[derive(Resource, Debug, Default)]
pub struct AnimCatalog {
    /// 角色列表（按目录名排序）
    pub characters: Vec<CharacterDef>,
    /// 所有角色出现过的剪辑名（并集，按名字排序）
    pub clips: Vec<String>,
}

/// 从 `<角色>/manifest.json` 读到的步态元数据
#[derive(Debug, Clone, Copy, Default)]
pub struct ClipGait {
    /// 一个循环的净前进距离（米）
    pub stride: f32,
    /// 左脚触地的归一化相位；没测出来时为 None
    pub contact_left: Option<f32>,
    /// 左脚离地的归一化相位
    pub lift_left: Option<f32>,
    /// 右脚触地的归一化相位
    pub contact_right: Option<f32>,
    /// 右脚离地的归一化相位
    pub lift_right: Option<f32>,
}

impl ClipGait {
    /// 事件表，顺序与 `ClipMeta::gait_events` 一致：
    /// `[左脚触地, 左脚离地, 右脚触地, 右脚离地]`
    pub fn events(&self) -> [Option<f32>; 4] {
        [
            self.contact_left,
            self.lift_left,
            self.contact_right,
            self.lift_right,
        ]
    }
}

#[derive(Debug, Clone)]
pub struct CharacterDef {
    /// 下拉框里显示的名字（取自 rig 文件名）
    pub name: String,
    /// rig 资源路径（不含 `#Scene0`）
    pub rig: String,
    /// 该角色的动画目录
    pub animations: String,
    /// 该角色自己拥有的剪辑名
    pub clips: Vec<String>,
    /// 剪辑名 → 步态元数据（来自 manifest，缺失时为空）
    pub gaits: HashMap<String, ClipGait>,
}

impl AnimCatalog {
    /// 当前角色定义；下标越界时退回第一个
    pub fn character(&self, index: usize) -> Option<&CharacterDef> {
        self.characters
            .get(index)
            .or_else(|| self.characters.first())
    }

    pub fn clip(&self, index: usize) -> Option<&str> {
        self.clips.get(index).map(|clip| clip.as_str())
    }
}

/// 播放选择：播哪个角色、哪条剪辑、倍速与根运动开关。
///
/// 这是**游戏状态**而不是面板状态，所以放在动画系统里；预览面板和混合空间编辑器
/// 都往这里写，驱动端只读它。
#[derive(Resource, Debug, Clone)]
pub struct AnimSelection {
    pub character: usize,
    pub clip: usize,
    pub speed: f32,
    pub root_motion: bool,
}

impl Default for AnimSelection {
    fn default() -> Self {
        Self {
            character: 0,
            clip: 0,
            speed: 1.0,
            root_motion: false,
        }
    }
}

/// 当前场景里的角色模型实体（`WorldAssetRoot` 持有者）
#[derive(Component)]
pub(crate) struct AnimModelRoot;

#[derive(Resource, Default)]
pub struct AnimRuntime {
    /// 当前的模型实体
    pub(crate) model: Option<Entity>,
    /// 当前角色的剪辑句柄（按剪辑名索引，供动画图编辑器采样）
    pub(crate) clips: HashMap<String, Handle<AnimationClip>>,
    /// 动画图与 AnimationPlayer 所在实体
    pub(crate) player: Option<Entity>,
    /// 当前角色在清单中的下标
    spawned_character: Option<usize>,
    /// 正在播放的剪辑下标
    playing_animation: Option<usize>,
    /// 已应用的倍速
    applied_speed: f32,
    /// 模型场景是否还在等待异步加载完成
    awaiting_scene: bool,
    /// 根骨骼（mixamorig:Hips）
    hips: Option<Entity>,
    /// 剪辑首帧的根骨骼位置，作为根运动的原点
    hips_base: Option<Vec3>,
    /// 上一帧的位移增量（Avatar 局部空间）
    last_delta: Vec3,
    /// 累计的根运动位移
    offset: Vec3,
    /// 当前角色对应的动画节点
    nodes: Vec<AnimationNodeIndex>,
    /// 预加载中的角色
    pending_character: Option<usize>,
    pending_rig: Option<Handle<WorldAsset>>,
    pending_clips: Vec<Handle<AnimationClip>>,
    /// 已加载好、等待建图的剪辑
    pending_clips_for_graph: Vec<Handle<AnimationClip>>,
}

impl AnimRuntime {
    fn reset_transient(&mut self) {
        self.hips = None;
        self.hips_base = None;
        self.last_delta = Vec3::ZERO;
        self.offset = Vec3::ZERO;
    }
}

impl Plugin for CharacterPlugin {
    fn build(&self, app: &mut App) {
        let catalog = scan_catalog();
        app.insert_resource(catalog)
            .init_resource::<AnimSelection>()
            .init_resource::<AnimRuntime>()
            .add_systems(
                Update,
                (
                    request_character,
                    apply_character_when_loaded,
                    sync_clip_set,
                    finish_model_setup,
                    locate_hips,
                    sync_clip,
                    sync_speed,
                    sync_root_motion,
                )
                    .chain()
                    .run_if(in_state(GameState::Game)),
            )
            .add_systems(
                PostUpdate,
                apply_root_motion
                    .after(AnimationSystems)
                    // 混合空间驱动姿态时骨骼已被写过，这里再锁一次会打架
                    .before(super::driver::drive_pose)
                    .before(TransformSystems::Propagate),
            );
    }
}

/// 启动时扫描 assets/characters：rigs 下的每个 glb 是一个角色，animations 下的同名目录是它的剪辑
fn scan_catalog() -> AnimCatalog {
    let Some(root) = find_asset_root() else {
        warn!("没有找到 assets 目录，角色与剪辑清单为空");
        return AnimCatalog::default();
    };

    let rigs_dir = root.join("characters/rigs");
    let animations_root = root.join("characters/animations");

    let mut rig_files = read_names(&rigs_dir, "glb");
    rig_files.sort();

    let mut characters = Vec::new();
    let mut all_clips: BTreeSet<String> = BTreeSet::new();

    for file in rig_files {
        let mut clips = read_names(&animations_root.join(&file), "glb");
        clips.sort();
        for clip in &clips {
            all_clips.insert(clip.clone());
        }

        characters.push(CharacterDef {
            rig: format!("characters/rigs/{file}.glb"),
            animations: format!("characters/animations/{file}"),
            name: file.clone(),
            gaits: read_gaits(&animations_root.join(&file)),
            clips,
        });
    }

    let catalog = AnimCatalog {
        characters,
        clips: all_clips.into_iter().collect(),
    };

    info!(
        "动画清单：{} 个角色，{} 个剪辑",
        catalog.characters.len(),
        catalog.clips.len()
    );
    catalog
}

/// 读 `<角色>/manifest.json` 里的步态元数据：净步幅、触地与离地相位。
///
/// 这些值由 `scripts/validate_animations.py` 从 GLB 里实测得出；
/// 缺失时返回空表，运行时会退回「按加权时长推进相位」的旧行为。
fn read_gaits(dir: &Path) -> HashMap<String, ClipGait> {
    let path = dir.join("manifest.json");
    let Ok(text) = std::fs::read_to_string(&path) else {
        return HashMap::new();
    };
    let Ok(json) = serde_json::from_str::<serde_json::Value>(&text) else {
        warn!("{} 解析失败，步幅/相位元数据缺失", path.display());
        return HashMap::new();
    };
    let Some(clips) = json.get("clips").and_then(|value| value.as_object()) else {
        return HashMap::new();
    };
    let mut out = HashMap::with_capacity(clips.len());
    for (name, clip) in clips {
        let stride = clip
            .pointer("/stride/net")
            .and_then(|value| value.as_f64())
            .unwrap_or(0.0);
        let event = |field: &str| {
            clip.pointer(&format!("/gait/{field}"))
                .and_then(|value| value.as_f64())
                .map(|value| value as f32)
        };
        out.insert(
            name.clone(),
            ClipGait {
                stride: stride as f32,
                contact_left: event("contact_left"),
                lift_left: event("lift_left"),
                contact_right: event("contact_right"),
                lift_right: event("lift_right"),
            },
        );
    }
    out
}

/// 读目录下的文件名（去掉扩展名），失败时返回空列表
fn read_names(dir: &Path, extension: &str) -> Vec<String> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    entries
        .flatten()
        .filter(|entry| entry.path().is_file())
        .filter(|entry| {
            entry
                .path()
                .extension()
                .is_some_and(|ext| ext.eq_ignore_ascii_case(extension))
        })
        .filter_map(|entry| {
            entry
                .path()
                .file_stem()
                .map(|stem| stem.to_string_lossy().to_string())
        })
        .collect()
}

/// 角色切换后重建按名字索引的剪辑集合，并把 manifest 里的步幅 / 步态事件写进去。
///
/// 相位对齐不是固定常数偏移：给每个剪辑存一组事件
/// `[左脚触地, 左脚离地, 右脚触地, 右脚离地]`，求值时按当前权重做环形加权平均，
/// 得到统一时间轴后再分段重映射每个剪辑的相位。
pub(crate) fn sync_clip_set(
    catalog: Res<AnimCatalog>,
    selection: Res<AnimSelection>,
    runtime: Res<AnimRuntime>,
    mut clips: ResMut<ClipSet>,
) {
    if runtime.clips.is_empty() {
        return;
    }
    // 剪辑集合与角色都没变时，不必每帧拼指纹字符串
    if clips.rig.is_some() && !runtime.is_changed() {
        return;
    }
    let character = catalog.character(selection.character);
    let name = character.map(|def| def.name.as_str()).unwrap_or("");
    let fingerprint = runtime.clips.keys().cloned().collect::<Vec<_>>().join("|");
    let key = format!("{name}|{fingerprint}");
    if clips.rig.as_deref() == Some(key.as_str()) {
        return;
    }
    clips.rebuild(key);
    for (clip, handle) in &runtime.clips {
        clips.insert(clip.clone(), handle.clone());
    }

    let Some(def) = character else { return };

    let mut with_events = 0usize;
    for clip in runtime.clips.keys() {
        let Some(gait) = def.gaits.get(clip) else {
            continue;
        };
        let events = gait.events();
        if events[0].is_some() && events[2].is_some() {
            with_events += 1;
        }
        clips.set_meta(
            clip.clone(),
            ClipMeta {
                // 0 表示由 ClipSet::meta 回落到 AnimationClip 自身时长
                duration: 0.0,
                stride: gait.stride,
                gait_events: events,
            },
        );
    }
    info!(
        "{name}: {} 个剪辑写入步态元数据，其中 {} 个有完整的触地事件",
        def.gaits.len(),
        with_events
    );
}

/// 找 assets 根目录：先看环境变量，再从工作目录和可执行文件位置向上找
pub fn find_asset_root() -> Option<PathBuf> {
    if let Ok(root) = std::env::var("CRAB_ASSET_ROOT") {
        let path = PathBuf::from(root);
        if path.join("characters/rigs").is_dir() {
            return Some(path);
        }
    }

    let mut bases: Vec<PathBuf> = Vec::new();
    if let Ok(cwd) = std::env::current_dir() {
        bases.push(cwd);
    }
    if let Ok(exe) = std::env::current_exe() {
        let mut dir = exe.parent().map(|p| p.to_path_buf());
        while let Some(path) = dir {
            bases.push(path.clone());
            dir = path.parent().map(|p| p.to_path_buf());
        }
    }

    for base in bases {
        for rel in ["assets", "../assets", "../../assets", "../../../assets"] {
            let candidate = base.join(rel);
            if candidate.join("characters/rigs").is_dir() {
                return candidate.canonicalize().ok();
            }
        }
    }
    None
}

/// 请求切换角色：先把新角色的模型和全部剪辑预加载好，避免出现空窗
fn request_character(
    asset_server: Res<AssetServer>,
    catalog: Res<AnimCatalog>,
    selection: Res<AnimSelection>,
    mut runtime: ResMut<AnimRuntime>,
) {
    let Some(def) = catalog.character(selection.character) else {
        return;
    };
    let index = catalog
        .characters
        .iter()
        .position(|candidate| candidate.name == def.name)
        .unwrap_or(0);
    let up_to_date =
        runtime.spawned_character == Some(index) && runtime.pending_character.is_none();
    let already_requested = runtime.pending_character == Some(index);
    if up_to_date || already_requested {
        return;
    }

    let rig: Handle<WorldAsset> = asset_server.load(format!("{}#Scene0", def.rig));
    // 每个角色都有自己的剪辑子集；缺失的用该角色的第一个剪辑顶替，保证节点下标与全局清单一致
    let fallback = def.clips.first().map(|clip| clip.as_str());
    let clips: Vec<Handle<AnimationClip>> = catalog
        .clips
        .iter()
        .map(|clip| {
            let clip = if def.clips.iter().any(|own| own == clip) {
                clip.as_str()
            } else {
                fallback.unwrap_or(clip.as_str())
            };
            asset_server.load(format!("{}/{}.glb#Animation0", def.animations, clip))
        })
        .collect();

    runtime.pending_character = Some(index);
    runtime.pending_rig = Some(rig);
    runtime.pending_clips = clips;

    // 混合空间按「剪辑名」采样，这里单独保存一份不做缺件顶替的句柄
    runtime.clips.clear();
    for clip in &def.clips {
        runtime.clips.insert(
            clip.clone(),
            asset_server.load(format!("{}/{}.glb#Animation0", def.animations, clip)),
        );
    }
}

/// 资源加载完成后换模型：旧角色会一直播放到换上的这一帧
fn apply_character_when_loaded(
    mut commands: Commands,
    asset_server: Res<AssetServer>,
    mut runtime: ResMut<AnimRuntime>,
    avatars: Query<Entity, With<Avatar>>,
) {
    let Some(index) = runtime.pending_character else {
        return;
    };
    let Ok(avatar) = avatars.single() else {
        return;
    };

    let rig_ready = runtime
        .pending_rig
        .as_ref()
        .map(|rig| asset_server.is_loaded_with_dependencies(rig))
        .unwrap_or(false);
    let clips_ready = runtime
        .pending_clips
        .iter()
        .all(|clip| asset_server.is_loaded_with_dependencies(clip));

    if !(rig_ready && clips_ready) {
        return;
    }

    let Some(rig) = runtime.pending_rig.take() else {
        return;
    };
    let clips = std::mem::take(&mut runtime.pending_clips);
    runtime.pending_character = None;

    if let Some(old) = runtime.model.take() {
        commands.entity(old).despawn();
    }

    let model = commands
        .spawn((
            WorldAssetRoot(rig),
            Transform::from_translation(MODEL_BASE),
            AnimModelRoot,
            ChildOf(avatar),
        ))
        .id();

    runtime.model = Some(model);
    runtime.player = None;
    runtime.spawned_character = Some(index);
    runtime.pending_clips_for_graph = clips;
    runtime.playing_animation = None;
    runtime.nodes.clear();
    runtime.awaiting_scene = true;
    runtime.reset_transient();
}

/// 场景异步载入完成后，为角色重建动画图并播放当前剪辑
fn finish_model_setup(
    mut commands: Commands,
    selection: Res<AnimSelection>,
    mut runtime: ResMut<AnimRuntime>,
    mut graphs: ResMut<Assets<AnimationGraph>>,
    children: Query<&Children>,
    mut players: Query<&mut AnimationPlayer>,
) {
    if !runtime.awaiting_scene {
        return;
    }
    let Some(model) = runtime.model else {
        return;
    };

    let Some(player_entity) = find_descendant(model, &children, |entity| players.contains(entity))
    else {
        return;
    };

    let clips = std::mem::take(&mut runtime.pending_clips_for_graph);
    let (graph, nodes) = AnimationGraph::from_clips(clips);
    if nodes.is_empty() {
        warn!("动画剪辑为空，无法为角色建立动画图");
        runtime.awaiting_scene = false;
        return;
    }
    let graph_handle = graphs.add(graph);

    let Ok(mut player) = players.get_mut(player_entity) else {
        return;
    };

    let mut transitions = AnimationTransitions::new();
    let index = nodes[selection.clip.min(nodes.len() - 1)];
    {
        transitions
            .play(&mut player, index, Duration::ZERO)
            .repeat();
    }
    if let Some(active) = player.animation_mut(index) {
        active.set_speed(selection.speed);
    }

    commands
        .entity(player_entity)
        .insert((AnimationGraphHandle(graph_handle), transitions));

    runtime.player = Some(player_entity);
    runtime.nodes = nodes;
    runtime.playing_animation = Some(selection.clip);
    runtime.applied_speed = selection.speed;
    runtime.awaiting_scene = false;
    runtime.hips_base = None;
    runtime.last_delta = Vec3::ZERO;
}

/// 骨骼层次可能比场景本体晚一点出现，缺失时每帧尝试一次
fn locate_hips(mut runtime: ResMut<AnimRuntime>, children: Query<&Children>, names: Query<&Name>) {
    if runtime.hips.is_some() {
        return;
    }
    let Some(model) = runtime.model else {
        return;
    };

    let exact = find_descendant(model, &children, |entity| {
        names
            .get(entity)
            .map(|name| name.as_str() == "mixamorig:Hips")
            .unwrap_or(false)
    });

    let fallback = exact.or_else(|| {
        find_descendant(model, &children, |entity| {
            names
                .get(entity)
                .map(|name| {
                    let name = name.as_str();
                    name.contains("Hips")
                        && !name.contains("Ctrl")
                        && !name.contains("Helper")
                        && !name.contains("Free")
                })
                .unwrap_or(false)
        })
    });

    if let Some(hips) = fallback {
        runtime.hips = Some(hips);
        runtime.hips_base = None;
        runtime.last_delta = Vec3::ZERO;
    }
}

/// 切换剪辑
fn sync_clip(
    selection: Res<AnimSelection>,
    mut runtime: ResMut<AnimRuntime>,
    mut players: Query<(&mut AnimationPlayer, &mut AnimationTransitions)>,
) {
    if runtime.playing_animation == Some(selection.clip) {
        return;
    }
    let (Some(entity), Some(index)) = (runtime.player, runtime.nodes.get(selection.clip).copied())
    else {
        return;
    };
    let Ok((mut player, mut transitions)) = players.get_mut(entity) else {
        return;
    };

    {
        transitions
            .play(&mut player, index, Duration::ZERO)
            .repeat();
    }
    if let Some(active) = player.animation_mut(index) {
        active.set_speed(selection.speed);
    }

    runtime.playing_animation = Some(selection.clip);
    runtime.applied_speed = selection.speed;
    // 换剪辑后重新取根骨骼原点，并回到没有位移的姿态
    runtime.hips_base = None;
    runtime.last_delta = Vec3::ZERO;
    runtime.offset = Vec3::ZERO;
}

/// 倍速
fn sync_speed(
    selection: Res<AnimSelection>,
    mut runtime: ResMut<AnimRuntime>,
    mut players: Query<&mut AnimationPlayer>,
) {
    if (runtime.applied_speed - selection.speed).abs() < f32::EPSILON {
        return;
    }
    let Some(entity) = runtime.player else {
        return;
    };
    let Ok(mut player) = players.get_mut(entity) else {
        return;
    };

    for (_, active) in player.playing_animations_mut() {
        active.set_speed(selection.speed);
    }
    runtime.applied_speed = selection.speed;
}

/// 根运动开关
fn sync_root_motion(
    mut runtime: ResMut<AnimRuntime>,
    mut previous: Local<Option<bool>>,
    selection: Res<AnimSelection>,
) {
    if previous.is_none() {
        *previous = Some(selection.root_motion);
    }
    if *previous == Some(selection.root_motion) {
        return;
    }
    *previous = Some(selection.root_motion);

    runtime.hips_base = None;
    runtime.last_delta = Vec3::ZERO;
    runtime.offset = Vec3::ZERO;
}

/// 把根骨骼的水平位移转到模型根节点上
///
/// 动画本身会把 Hips 沿着剪辑的位移方向推走，如果原样播放，角色会在原地漂移。
/// 这里在动画求值之后、变换传播之前，把 Hips 的水平分量锁回原点，
/// 并把这段位移按 Avatar 的朝向累加到模型根节点上：
/// - 关闭根运动：只锁回，角色原地播放；
/// - 打开根运动：累加位移，角色真的在地面上跑动（相机同步跟随保持构图）。
fn apply_root_motion(
    selection: Res<AnimSelection>,
    mut runtime: ResMut<AnimRuntime>,
    driver: Res<PoseDriver>,
    avatars: Query<Entity, With<Avatar>>,
    mut transforms: Query<&mut Transform>,
    globals: Query<&GlobalTransform>,
    parents: Query<&ChildOf>,
) {
    let (Some(model), Some(hips)) = (runtime.model, runtime.hips) else {
        return;
    };

    // 混合空间正在写骨骼：位移由 `SpaceEvalOutput::root_delta` 统一处理，
    // 这里再锁一次 hips / 模型位置就会互相覆盖
    if driver.active {
        return;
    }

    let Ok(mut hips_transform) = transforms.get_mut(hips) else {
        return;
    };
    let current = hips_transform.translation;

    let base = match runtime.hips_base {
        Some(base) => base,
        None => {
            runtime.hips_base = Some(current);
            current
        }
    };

    // 剪辑首帧到当前帧的水平位移
    let local_delta = Vec3::new(current.x - base.x, 0.0, current.z - base.z);

    // 从骨骼所在父节点的空间转到 Avatar 的局部空间
    let hip_parent_rotation = parents
        .get(hips)
        .ok()
        .and_then(|child_of| globals.get(child_of.parent()).ok())
        .map(|global| global.rotation())
        .unwrap_or(Quat::IDENTITY);
    let avatar_rotation = avatars
        .single()
        .ok()
        .and_then(|avatar| globals.get(avatar).ok())
        .map(|global| global.rotation())
        .unwrap_or(Quat::IDENTITY);
    let avatar_delta = avatar_rotation.inverse() * (hip_parent_rotation * local_delta);

    // 骨骼水平位置锁回原点，只保留竖直方向的起伏
    hips_transform.translation.x = base.x;
    hips_transform.translation.z = base.z;

    let mut offset = runtime.offset;
    if selection.root_motion {
        let last = runtime.last_delta;
        let step = avatar_delta - last;
        if step.length() < 0.5 {
            offset += step;
        } else {
            // 剪辑循环回到起点：把角色放回起始位置，避免位移无限累积
            offset = Vec3::ZERO;
        }
        runtime.last_delta = avatar_delta;
    } else {
        offset = Vec3::ZERO;
        runtime.last_delta = Vec3::ZERO;
    }
    runtime.offset = offset;

    if let Ok(mut model_transform) = transforms.get_mut(model) {
        model_transform.translation = MODEL_BASE + offset;
        // 同步 Avatar 的旋转到模型根节点，确保动画系统驱动的朝向与物理层一致
        if let Ok(avatar_global) = globals.get(avatars.single().ok().unwrap_or(model)) {
            model_transform.rotation = avatar_global.rotation();
        }
    }
}

/// 深度优先查找满足条件的后代实体
fn find_descendant<F>(root: Entity, children: &Query<&Children>, mut predicate: F) -> Option<Entity>
where
    F: FnMut(Entity) -> bool,
{
    let mut stack = vec![root];
    while let Some(entity) = stack.pop() {
        if entity != root && predicate(entity) {
            return Some(entity);
        }
        if let Ok(entity_children) = children.get(entity) {
            stack.extend(entity_children.iter());
        }
    }
    None
}
