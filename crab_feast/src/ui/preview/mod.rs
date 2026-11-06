//! 动画预览：角色切换、播放倍速、根运动开关，右侧面板由 F2 / ` 呼出。

use std::{
    collections::{BTreeSet, HashMap},
    path::{Path, PathBuf},
    time::Duration,
};

use bevy::{app::AnimationSystems, prelude::*, transform::TransformSystems};
use crate::ui::menu::{MenuConfig, MenuEvent, MenuPageBody, MenuPageDef, MenuSet};

use crate::{GameState, scene::Avatar};

pub(crate) mod ui;

/// 动画预览页面在菜单里的 id
pub const ANIMATION_PREVIEW_PAGE: &str = "animation_preview";

pub struct AnimationPreviewPlugin;

/// 角色模型相对挂点（Avatar）的初始偏移
const MODEL_BASE: Vec3 = Vec3::new(0.0, -0.7, 0.0);

/// 扫描 `assets/characters` 得到的角色与剪辑清单
#[derive(Resource, Debug, Default)]
pub struct PreviewCatalog {
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
    /// 固有地速（米/秒），等于 stride / duration；留给后续的播放倍率匹配
    #[allow(dead_code)]
    pub speed: f32,
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

impl PreviewCatalog {
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

/// 启动时扫描 assets/characters：rigs 下的每个 glb 是一个角色，animations 下的同名目录是它的剪辑
fn scan_catalog() -> PreviewCatalog {
    let Some(root) = find_asset_root() else {
        warn!("没有找到 assets 目录，预览面板的角色/剪辑清单为空");
        return PreviewCatalog::default();
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

    let catalog = PreviewCatalog {
        characters,
        clips: all_clips.into_iter().collect(),
    };

    info!(
        "预览清单：{} 个角色，{} 个剪辑",
        catalog.characters.len(),
        catalog.clips.len()
    );
    catalog
}

/// 读 `<角色>/manifest.json` 里的步态元数据：步幅、固有地速、左脚触地相位。
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
        let speed = clip
            .pointer("/gait/speed")
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
                speed: speed as f32,
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

/// 找 assets 根目录：先看环境变量，再从工作目录和可执行文件位置向上找
pub(crate) fn find_asset_root() -> Option<PathBuf> {
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

/// 倍速滑动条的范围
pub const SPEED_RANGE: (f32, f32) = (0.1, 3.0);

/// 面板上两个下拉框
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub(crate) enum DropdownKind {
    #[default]
    Character,
    Animation,
}

#[derive(Resource, Debug)]
pub struct PreviewState {
    /// 当前展开的下拉框（同一时间只展开一个）
    pub dropdown_open: Option<DropdownKind>,
    pub character: usize,
    pub animation: usize,
    pub speed: f32,
    pub root_motion: bool,
}

impl Default for PreviewState {
    fn default() -> Self {
        Self {
            dropdown_open: None,
            character: 0,
            animation: 0,
            speed: 1.0,
            root_motion: false,
        }
    }
}

/// 面板按钮携带的动作
#[derive(Clone, Copy, PartialEq, Debug, Default)]
pub(crate) enum PreviewAction {
    #[default]
    ToggleRootMotion,
}

#[derive(Component, Clone, Default)]
pub(crate) struct PreviewButton(pub(crate) PreviewAction);

#[derive(Component, Clone, Default)]
pub(crate) struct PreviewDropdownTrigger;

#[derive(Component, Clone, Default)]
pub(crate) struct PreviewDropdownPopup(pub(crate) DropdownKind);

/// 下拉框里的选项
#[derive(Component, Clone, Default)]
pub(crate) struct PreviewDropdownItem {
    pub(crate) kind: DropdownKind,
    pub(crate) index: usize,
}

#[derive(Component, Clone, Default)]
pub(crate) struct PreviewDropdownLabel(pub(crate) DropdownKind);

#[derive(Component, Clone, Default)]
pub(crate) struct PreviewSpeedSlider;

#[derive(Component, Clone, Default)]
pub(crate) struct PreviewSpeedThumb;

/// 滑动条已播放到的部分
#[derive(Component, Clone, Default)]
pub(crate) struct PreviewSpeedFill;

#[derive(Component, Clone, Default)]
pub(crate) struct PreviewSpeedLabel;

#[derive(Component, Clone, Default)]
pub(crate) struct PreviewRootMotionLabel;

/// 当前场景里的角色模型实体（WorldAssetRoot 持有者）
#[derive(Component)]
struct PreviewModelRoot;

#[derive(Resource, Default)]
pub(crate) struct PreviewRuntime {
    /// 当前的模型实体
    pub(crate) model: Option<Entity>,
    /// 当前角色的剪辑句柄（按剪辑名索引，供动画图编辑器采样）
    pub(crate) clips: std::collections::HashMap<String, Handle<AnimationClip>>,
    /// 动画图与 AnimationPlayer 所在实体
    player: Option<Entity>,
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

impl PreviewRuntime {
    fn reset_transient(&mut self) {
        self.hips = None;
        self.hips_base = None;
        self.last_delta = Vec3::ZERO;
        self.offset = Vec3::ZERO;
    }
}

impl Plugin for AnimationPreviewPlugin {
    fn build(&self, app: &mut App) {
        let catalog = scan_catalog();
        app.insert_resource(catalog)
            .init_resource::<PreviewState>()
            .init_resource::<PreviewRuntime>()
            // 先把页面挂进菜单树，再在菜单 UI 生成后往页面内容区填控件
            .add_systems(
                OnEnter(GameState::Game),
                register_menu_page.before(MenuSet::Setup),
            )
            .add_systems(
                OnEnter(GameState::Game),
                fill_menu_page.after(MenuSet::Setup),
            )
            // 菜单开合或换页时收起展开的下拉框
            .add_systems(
                Update,
                reset_dropdown_on_menu_event.run_if(in_state(GameState::Game)),
            )
            .add_systems(
                Update,
                (
                    request_character,
                    apply_character_when_loaded,
                    finish_model_setup,
                    locate_hips,
                    sync_animation,
                    sync_speed,
                    sync_root_motion,
                )
                    .chain()
                    .run_if(in_state(GameState::Game)),
            )
            .add_systems(
                Update,
                (
                    ui::update_button_styles,
                    ui::update_dropdown,
                    ui::update_speed_thumb,
                    ui::update_labels,
                )
                    .run_if(in_state(GameState::Game)),
            )
            .add_systems(
                PostUpdate,
                apply_root_motion
                    .after(AnimationSystems)
                    .before(TransformSystems::Propagate),
            );
    }
}

/// 把「动画预览」注册成菜单里的一级入口
fn register_menu_page(mut config: ResMut<MenuConfig>) {
    if config
        .pages
        .iter()
        .any(|page| page.id == ANIMATION_PREVIEW_PAGE)
    {
        return;
    }
    config.pages.push(MenuPageDef::section(
        ANIMATION_PREVIEW_PAGE,
        "动画预览",
        "切换角色、动画剪辑、播放倍速与根运动",
    ));
}

/// 把预览控件挂进菜单的「动画预览」页面内容区
fn fill_menu_page(
    mut commands: Commands,
    asset_server: Res<AssetServer>,
    catalog: Res<PreviewCatalog>,
    state: Res<PreviewState>,
    bodies: Query<(Entity, &MenuPageBody)>,
) {
    let Some((entity, _)) = bodies
        .iter()
        .find(|(_, body)| body.page == Some(ANIMATION_PREVIEW_PAGE))
    else {
        warn!("菜单里没有找到动画预览页面，跳过内容填充");
        return;
    };

    ui::build_page(&mut commands, entity, &asset_server, &catalog, &state);
}

fn reset_dropdown_on_menu_event(
    mut events: MessageReader<MenuEvent>,
    mut preview: ResMut<PreviewState>,
) {
    if events.read().count() > 0 {
        preview.dropdown_open = None;
    }
}

/// 请求切换角色：先把新角色的模型和全部剪辑预加载好，避免出现空窗
fn request_character(
    asset_server: Res<AssetServer>,
    catalog: Res<PreviewCatalog>,
    state: Res<PreviewState>,
    mut runtime: ResMut<PreviewRuntime>,
) {
    let Some(def) = catalog.character(state.character) else {
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

    // 编辑器按「剪辑名」采样，这里单独保存一份不做缺件顶替的句柄
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
    mut runtime: ResMut<PreviewRuntime>,
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
            PreviewModelRoot,
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
    state: Res<PreviewState>,
    mut runtime: ResMut<PreviewRuntime>,
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
    let index = nodes[state.animation.min(nodes.len() - 1)];
    {
        transitions
            .play(&mut player, index, Duration::ZERO)
            .repeat();
    }
    if let Some(active) = player.animation_mut(index) {
        active.set_speed(state.speed);
    }

    commands
        .entity(player_entity)
        .insert((AnimationGraphHandle(graph_handle), transitions));

    runtime.player = Some(player_entity);
    runtime.nodes = nodes;
    runtime.playing_animation = Some(state.animation);
    runtime.applied_speed = state.speed;
    runtime.awaiting_scene = false;
    runtime.hips_base = None;
    runtime.last_delta = Vec3::ZERO;
}

/// 骨骼层次可能比场景本体晚一点出现，缺失时每帧尝试一次
fn locate_hips(
    mut runtime: ResMut<PreviewRuntime>,
    children: Query<&Children>,
    names: Query<&Name>,
) {
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
fn sync_animation(
    state: Res<PreviewState>,
    mut runtime: ResMut<PreviewRuntime>,
    mut players: Query<(&mut AnimationPlayer, &mut AnimationTransitions)>,
) {
    if runtime.playing_animation == Some(state.animation) {
        return;
    }
    let (Some(entity), Some(index)) = (runtime.player, runtime.nodes.get(state.animation).copied())
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
        active.set_speed(state.speed);
    }

    runtime.playing_animation = Some(state.animation);
    runtime.applied_speed = state.speed;
    // 换剪辑后重新取根骨骼原点，并回到没有位移的姿态
    runtime.hips_base = None;
    runtime.last_delta = Vec3::ZERO;
    runtime.offset = Vec3::ZERO;
}

/// 倍速
fn sync_speed(
    state: Res<PreviewState>,
    mut runtime: ResMut<PreviewRuntime>,
    mut players: Query<&mut AnimationPlayer>,
) {
    if (runtime.applied_speed - state.speed).abs() < f32::EPSILON {
        return;
    }
    let Some(entity) = runtime.player else {
        return;
    };
    let Ok(mut player) = players.get_mut(entity) else {
        return;
    };

    for (_, active) in player.playing_animations_mut() {
        active.set_speed(state.speed);
    }
    runtime.applied_speed = state.speed;
}

/// 根运动开关
fn sync_root_motion(
    mut runtime: ResMut<PreviewRuntime>,
    mut previous: Local<Option<bool>>,
    state: Res<PreviewState>,
) {
    if previous.is_none() {
        *previous = Some(state.root_motion);
    }
    if *previous == Some(state.root_motion) {
        return;
    }
    *previous = Some(state.root_motion);

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
    state: Res<PreviewState>,
    mut runtime: ResMut<PreviewRuntime>,
    avatars: Query<Entity, With<Avatar>>,
    mut transforms: Query<&mut Transform>,
    globals: Query<&GlobalTransform>,
    parents: Query<&ChildOf>,
) {
    let (Some(model), Some(hips)) = (runtime.model, runtime.hips) else {
        return;
    };

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
    if state.root_motion {
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
