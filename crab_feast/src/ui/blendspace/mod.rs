//! Blend Space 2D 编辑器：两个参数轴 + 网格上的动画采样点。
//!
//! 没有节点图：文档只有一个混合空间，编辑对象就是采样点本身。
//! 布局上分两块：
//! - 屏幕下方停靠的只有 2D 网格本体（[`EditorRoot`]，F3 显示 / 隐藏）；
//! - 混合系统、参数、采样点管理、存取都挂在菜单的「混合空间」页面里
//!   （[`fill_menu_page`]，页面 id 为 [`BLEND_SPACE_PAGE`]）。

mod grid;
mod inspector;

use std::collections::HashMap;

use bevy::app::Propagate;
use bevy::{picking::pointer::PointerButton, prelude::*, ui_widgets::SliderValue};

use crate::{
    GameState,
    anim::core::BlendSpaceDoc,
    anim::{AnimCatalog, AnimFocus, AnimSelection, AnimationSet, MixerCache, MixerEdit},
    ui::font::{FontFamily, font_scope},
    ui::menu::{
        DRAWER_WIDTH, MenuConfig, MenuEvent, MenuPageBody, MenuPageDef, MenuState, widgets,
    },
};

/// 网格尺寸（像素）：横轴铺开、纵轴压扁的长方形，贴在屏幕下方
pub const GRID_W: f32 = 640.0;
pub const GRID_H: f32 = 210.0;
/// 网格在内容区里的上边距
pub const GRID_TOP: f32 = 24.0;
/// 整个编辑器面板的高度（贴底停靠）
pub const PANEL_HEIGHT: f32 = 300.0;
/// 采样点半径
pub const DOT_RADIUS: f32 = 7.0;
/// 采样点吸附步长：0.05 即吸附到 1/20 刻度；改成 0.5 就只落在网格刻度点上
pub const SNAP_STEP: f32 = 0.05;

pub const GRID_BACKGROUND: Color = Color::srgba(0.04, 0.05, 0.08, 0.68);
pub const GRID_LINE: Color = Color::srgba(1.0, 1.0, 1.0, 0.08);
pub const GRID_AXIS: Color = Color::srgba(1.0, 1.0, 1.0, 0.22);
pub const MARKER_COLOR: Color = Color::srgb(1.0, 0.84, 0.35);
/// 网格外框：与菜单里选中的蓝色一致
pub const FRAME_BORDER: Color = Color::srgba(0.28, 0.52, 0.92, 0.9);
pub const TEXT_COLOR: Color = Color::srgb(0.86, 0.89, 0.95);
pub const MUTED_TEXT: Color = Color::srgba(0.72, 0.76, 0.84, 0.85);
pub const HINT_COLOR: Color = Color::srgb(0.62, 0.80, 1.0);
pub const WARN_COLOR: Color = Color::srgb(1.0, 0.58, 0.45);

/// 采样点配色，按索引循环，便于区分
pub const DOT_COLORS: [Color; 6] = [
    Color::srgb(0.36, 0.62, 0.95),
    Color::srgb(0.42, 0.80, 0.62),
    Color::srgb(0.95, 0.68, 0.38),
    Color::srgb(0.72, 0.55, 0.95),
    Color::srgb(0.95, 0.52, 0.60),
    Color::srgb(0.45, 0.82, 0.88),
];

/// 采样点坐标吸附到「每轴范围的一定比例」；`free` 为真时（按住 Shift）不吸附。
/// 步长按各轴自身的跨度取比例，yaw（度）与 speed（0~1）量纲不同也能各自吸附。
pub fn snap_point(at: [f32; 2], free: bool, ranges: [[f32; 2]; 2]) -> [f32; 2] {
    let snap_axis = |value: f32, range: [f32; 2]| {
        let step = (range[1] - range[0]).max(1e-6) * SNAP_STEP;
        let snapped = if free {
            value
        } else {
            (value / step).round() * step
        };
        snapped.clamp(range[0], range[1])
    };
    [snap_axis(at[0], ranges[0]), snap_axis(at[1], ranges[1])]
}

#[derive(Resource)]
pub struct SpaceEditor {
    pub open: bool,
    /// 当前预览/编辑的混合系统（对应 `MixerRegistry` 的顺序）
    pub mixer: usize,
    /// 当前文档属于哪个角色，保存与载入按它拼路径
    pub character: String,
    /// 当前文档来自哪个混合器，用于切换时重新载入
    pub loaded_key: Option<String>,
    /// 当前混合系统是否在游戏中启用
    pub enabled: bool,
    pub doc: BlendSpaceDoc,
    /// 文档结构变化时递增，触发网格与检查器重建
    pub revision: u64,
    pub selected: Option<usize>,
    pub params: HashMap<String, f32>,
    pub status: String,
    /// 正在拖动的采样点：索引 + 拖动起点坐标
    pub drag: Option<SampleDrag>,
    /// 本次手势累计移动的像素距离（用来区分“点击删除”和“拖动结束”）
    pub gesture_travel: f32,
    /// 正在拖动的当前值标记：拖动起点坐标（参数空间）
    pub marker_drag: Option<[f32; 2]>,
}

/// 混合空间页面里的两个下拉框标识（展开状态由 `widgets::OpenDropdown` 统一持有）
pub const MIXER_DROPDOWN: &str = "blend-space/mixer";
pub const CLIP_DROPDOWN: &str = "blend-space/clip";

#[derive(Clone)]
pub struct SampleDrag {
    pub index: usize,
    pub start_at: [f32; 2],
}

impl Default for SpaceEditor {
    fn default() -> Self {
        Self {
            open: false,
            mixer: 0,
            character: String::new(),
            loaded_key: None,
            enabled: true,
            doc: BlendSpaceDoc::default(),
            revision: 0,
            selected: None,
            params: HashMap::from([("yaw".to_string(), 0.0), ("speed".to_string(), 0.5)]),
            status: String::new(),
            drag: None,
            gesture_travel: 0.0,
            marker_drag: None,
        }
    }
}

#[derive(Component, Clone, Default)]
pub struct EditorRoot;
/// 菜单「混合空间」页面里的内容根节点
#[derive(Component, Clone, Default)]
struct SpacePageRoot;
#[derive(Component, Clone, Default)]
pub struct GridLayer;
#[derive(Component, Clone, Default)]
pub struct SampleLayer;
#[derive(Component, Clone, Default)]
pub struct SampleDot {
    pub index: usize,
}
#[derive(Component, Clone, Default)]
pub struct ParamMarker;
#[derive(Component, Clone, Default)]
pub struct InspectorBody;
#[derive(Component, Clone, Default)]
pub struct StatusText;
#[derive(Component, Clone, Default)]
pub struct RootMotionLabel;
/// 需要每帧刷新数值文本的滑杆标签
#[derive(Component, Clone, Default)]
pub struct WeightText;

#[derive(Component, Clone, Default)]
pub struct EnabledLabel;

#[derive(Component, Clone, Default)]
pub struct ParamValue {
    /// 0/1 = 两个轴，2 = 播放速度
    pub slot: usize,
}

/// 检查器里的参数滑杆（根节点），slot 语义同 [`ParamValue`]
#[derive(Component, Clone, Default)]
pub struct ParamSlider {
    pub slot: usize,
}
#[derive(Resource, Default)]
pub struct ForceRebuild(pub bool);

/// 参数坐标 → 网格内部像素坐标（网格自身左上角为原点）
pub fn to_px(at: [f32; 2], ranges: [[f32; 2]; 2]) -> Vec2 {
    let norm = |value: f32, range: [f32; 2]| {
        let span = (range[1] - range[0]).max(1e-6);
        ((value - range[0]) / span).clamp(0.0, 1.0)
    };
    Vec2::new(
        norm(at[0], ranges[0]) * GRID_W,
        (1.0 - norm(at[1], ranges[1])) * GRID_H,
    )
}

/// 像素位移 → 参数位移（拖动采样点时换算）
pub fn px_to_param(delta: Vec2, ranges: [[f32; 2]; 2]) -> [f32; 2] {
    [
        delta.x / GRID_W * (ranges[0][1] - ranges[0][0]),
        -delta.y / GRID_H * (ranges[1][1] - ranges[1][0]),
    ]
}

pub struct BlendSpaceEditorPlugin;

impl Plugin for BlendSpaceEditorPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<SpaceEditor>()
            .init_resource::<ForceRebuild>()
            .add_systems(Startup, register_menu_page)
            .add_systems(
                Update,
                (
                    toggle_editor,
                    open_grid_on_page,
                    build_editor_ui,
                    attach_editor_font_scope,
                    sync_dock_inset,
                    fill_menu_page,
                )
                    .chain()
                    .run_if(in_state(GameState::Game)),
            )
            .add_systems(
                Update,
                (
                    grid::rebuild_grid,
                    grid::apply_sample_positions,
                    grid::update_sample_selection,
                    grid::update_marker,
                    inspector::rebuild_inspector,
                    sync_status,
                    sync_root_motion_label,
                    sync_param_labels,
                    sync_param_sliders,
                    sync_weight_text,
                    sync_enabled_label,
                    switch_mixer_keys,
                    apply_dropdown_pick,
                    load_editor_state,
                    publish_edit,
                    widgets::update_slider_styling,
                )
                    .chain()
                    // 本帧的编辑要赶在动画系统应用参数之前交出去，否则拖滑杆会慢一帧
                    .in_set(AnimationSet::Publish)
                    .run_if(in_state(GameState::Game)),
            );
    }
}

fn toggle_editor(
    mut commands: Commands,
    keys: Res<ButtonInput<KeyCode>>,
    mut editor: ResMut<SpaceEditor>,
) {
    if !keys.just_pressed(KeyCode::F3) {
        return;
    }
    editor.open = !editor.open;
    if editor.open {
        commands.insert_resource(ForceRebuild(true));
    }
}

/// 菜单里的「混合空间」页面 id
pub const BLEND_SPACE_PAGE: &str = "blend_space";

/// 把「混合空间」注册成菜单一级入口：网格停靠屏幕下方，控制项都在这个页面里
fn register_menu_page(mut config: ResMut<MenuConfig>) {
    if config.pages.iter().any(|page| page.id == BLEND_SPACE_PAGE) {
        return;
    }
    config.pages.push(MenuPageDef::section(
        BLEND_SPACE_PAGE,
        "混合空间",
        "网格停靠屏幕下方，混合系统、参数、采样点在这里调",
    ));
}

/// 进入混合空间页面时点亮底部的 2D 网格
fn open_grid_on_page(
    mut commands: Commands,
    mut events: MessageReader<MenuEvent>,
    mut editor: ResMut<SpaceEditor>,
) {
    for event in events.read() {
        let MenuEvent::PageChanged { to: Some(page), .. } = event else {
            continue;
        };
        if *page == BLEND_SPACE_PAGE && !editor.open {
            editor.open = true;
            commands.insert_resource(ForceRebuild(true));
        }
    }
}

fn build_editor_ui(
    mut commands: Commands,
    editor: Res<SpaceEditor>,
    mut force: ResMut<ForceRebuild>,
    roots: Query<Entity, With<EditorRoot>>,
    mut last_mixer: Local<Option<usize>>,
) {
    if !editor.open {
        *last_mixer = None;
        for root in &roots {
            commands.entity(root).despawn();
        }
        return;
    }
    if *last_mixer != Some(editor.mixer) {
        // 切换混合系统：网格重建一次，轴名与采样点跟着新文档走
        *last_mixer = Some(editor.mixer);
        for root in &roots {
            commands.entity(root).despawn();
        }
    }
    if !roots.is_empty() {
        return;
    }

    // 新面板里的采样点层是空的，强制下一帧重画
    force.0 = true;
    commands.spawn_scene(editor_scene(&editor));
}

/// 给网格面板定字体作用域：编辑器是独立悬浮层，不在菜单页面里，字体族要自己挂一次。
///
/// 轴名、剪辑名都是文档里的参数名（可能是中文），必须走项目自带的字体，不能用引擎默认字体。
fn attach_editor_font_scope(
    mut commands: Commands,
    asset_server: Res<AssetServer>,
    roots: Query<Entity, (With<EditorRoot>, Without<Propagate<FontFamily>>)>,
) {
    let Some(root) = roots.iter().next() else {
        return;
    };
    let font: Handle<Font> = asset_server.load("fonts/PingFang-SC-Regular.ttf");
    commands.entity(root).insert(font_scope(&font));
}

/// 网格编辑器：停靠在屏幕下方的面板 → 网格区 → 网格（外框 / 网格线 / 采样点 / 参数标记）
fn editor_scene(editor: &SpaceEditor) -> impl Scene + use<> {
    bsn! {
        Node {
            position_type: PositionType::Absolute,
            left: Val::Px(0.0),
            bottom: Val::Px(0.0),
            width: Val::Percent(100.0),
            height: Val::Px(PANEL_HEIGHT),
            flex_direction: FlexDirection::Row,
            justify_content: JustifyContent::Center,
            align_items: AlignItems::FlexStart,
            padding: UiRect::top(Val::Px(GRID_TOP)),
            overflow: Overflow::clip(),
        }
        GlobalZIndex(60)
        EditorRoot
        Children [
            // 网格区：横向长方形居中
            Node {
                flex_grow: 1.0,
                min_width: Val::Px(0.0),
                flex_direction: FlexDirection::Row,
                justify_content: JustifyContent::Center,
                align_items: AlignItems::FlexStart,
            }
            Children [
                @{grid::grid_scene(editor)}
            ]
        ]
    }
}

/// 抽屉展开多少，网格就在剩下的左侧空白区域居中
fn sync_dock_inset(menu: Res<MenuState>, mut roots: Query<&mut Node, With<EditorRoot>>) {
    let pad = Val::Px(DRAWER_WIDTH * menu.drawer_progress());
    for mut node in &mut roots {
        if node.padding.right != pad {
            node.padding.right = pad;
            node.padding.top = Val::Px(GRID_TOP);
        }
    }
}

/// 往菜单的「混合空间」页面里填内容：混合系统、参数、采样点管理、工具
fn fill_menu_page(
    mut commands: Commands,
    editor: Res<SpaceEditor>,
    cache: Res<MixerCache>,
    bodies: Query<(Entity, &MenuPageBody)>,
    roots: Query<Entity, With<SpacePageRoot>>,
    mut last: Local<(usize, usize)>,
) {
    // 混合系统切换、或混合器注册完毕（数量变化）时整页重建
    let key = (editor.mixer, cache.mixers.len());
    if !roots.is_empty() {
        if *last != key {
            *last = key;
            for root in &roots {
                commands.entity(root).despawn();
            }
        }
        return;
    }
    if cache.mixers.is_empty() {
        return;
    }
    *last = key;
    let Some((body, _)) = bodies
        .iter()
        .find(|(_, body)| body.page == Some(BLEND_SPACE_PAGE))
    else {
        return;
    };
    commands.spawn_scene(page_scene(body, &editor, &cache));
}

/// 整页结构：混合系统 → 参数 → 采样点 → 工具行 → 操作提示
fn page_scene(body: Entity, editor: &SpaceEditor, cache: &MixerCache) -> impl Scene + use<> {
    let titles = cache
        .mixers
        .iter()
        .map(|mixer| mixer.title.clone())
        .collect::<Vec<_>>();
    let selected = editor.mixer.min(titles.len().saturating_sub(1));
    let status = editor.status.clone();

    bsn! {
        Node {
            width: Val::Percent(100.0),
            flex_grow: 1.0,
            min_height: Val::Px(0.0),
            flex_direction: FlexDirection::Column,
            row_gap: Val::Px(8.0),
        }
        SpacePageRoot
        ChildOf(body)
        // 点页面空白处收起下拉框（触发器和选项都 propagate(false)，不会误触发）
        on(|click: On<PointerClick>, mut open: ResMut<widgets::OpenDropdown>| {
            if click.button == PointerButton::Primary {
                open.0 = None;
            }
        })
        Children [
            @{widgets::text("混合系统", 12.0, MUTED_TEXT)}
            --
            @{widgets::dropdown(MIXER_DROPDOWN, &titles, selected)}
            --
            Text::new(status)
            TextFont { font_size: FontSize::Px(11.0) }
            TextColor(MUTED_TEXT)
            StatusText
            --
            @{widgets::text("参数", 12.0, MUTED_TEXT)}
            --
            @{inspector::params_scene(editor, cache)}
            --
            @{widgets::text("采样点", 12.0, MUTED_TEXT)}
            --
            // 采样点列表由 inspector 反复重建，这里只留一个空容器
            Node {
                width: Val::Percent(100.0),
                flex_direction: FlexDirection::Column,
                row_gap: Val::Px(4.0),
            }
            InspectorBody
            --
            @{tools_row()}
            --
            @{widgets::text(
                "拖采样点会吸附到网格刻度点，按住 Shift 自由放置 · 白色标记是当前参数值 · \
                 F3 显示 / 隐藏下方网格",
                10.0,
                MUTED_TEXT,
            )}
        ]
    }
}

/// 工具行：保存、载入、根运动开关、隐藏网格
fn tools_row() -> impl Scene + use<> {
    bsn! {
        Node {
            width: Val::Percent(100.0),
            flex_direction: FlexDirection::Row,
            flex_wrap: FlexWrap::Wrap,
            align_items: AlignItems::Center,
            column_gap: Val::Px(6.0),
            row_gap: Val::Px(6.0),
        }
        Children [
            @{widgets::button("保存")}
            on(|mut click: On<PointerClick>, mut editor: ResMut<SpaceEditor>| {
                if click.button != PointerButton::Primary {
                    return;
                }
                click.propagate(false);
                inspector::save_space(&mut editor);
                editor.revision = editor.revision.wrapping_add(1);
            })
            --
            @{widgets::button("载入")}
            on(|mut click: On<PointerClick>, mut editor: ResMut<SpaceEditor>| {
                if click.button != PointerButton::Primary {
                    return;
                }
                click.propagate(false);
                inspector::load_space(&mut editor);
                editor.revision = editor.revision.wrapping_add(1);
            })
            --
            Node {
                padding: UiRect::axes(Val::Px(10.0), Val::Px(4.0)),
                border: UiRect::all(Val::Px(1.0)),
                border_radius: BorderRadius::all(Val::Px(5.0)),
            }
            BackgroundColor(Color::srgb(0.14, 0.16, 0.22))
            BorderColor::all(Color::srgba(1.0, 1.0, 1.0, 0.14))
            on(|click: On<PointerClick>, mut selection: ResMut<AnimSelection>| {
                if click.button != PointerButton::Primary {
                    return;
                }
                selection.root_motion = !selection.root_motion;
            })
            Children [
                Text::new("根运动：关")
                TextFont { font_size: FontSize::Px(11.0) }
                TextColor(TEXT_COLOR)
                RootMotionLabel
            ]
            --
            @{widgets::button("隐藏网格")}
            on(|click: On<PointerClick>, mut editor: ResMut<SpaceEditor>| {
                if click.button != PointerButton::Primary {
                    return;
                }
                editor.open = false;
            })
        ]
    }
}

/// 下拉框选中结果：混合系统切换 / 采样点剪辑替换
fn apply_dropdown_pick(
    mut picked: MessageReader<widgets::DropdownPicked>,
    cache: Res<MixerCache>,
    catalog: Res<AnimCatalog>,
    selection: Res<AnimSelection>,
    mut editor: ResMut<SpaceEditor>,
) {
    for pick in picked.read() {
        match pick.id {
            MIXER_DROPDOWN => {
                let Some(mixer) = cache.mixers.get(pick.index) else {
                    continue;
                };
                let key = mixer.key.clone();
                if editor.loaded_key.as_deref() != Some(key.as_str()) {
                    editor.mixer = pick.index;
                    editor.loaded_key = None;
                    editor.status = format!("切换到混合系统：{key}");
                    editor.revision = editor.revision.wrapping_add(1);
                }
            }
            CLIP_DROPDOWN => {
                let Some(index) = editor.selected else {
                    continue;
                };
                let clips = catalog
                    .character(selection.character)
                    .map(|def| def.clips.clone())
                    .unwrap_or_default();
                let Some(clip) = clips.get(pick.index).cloned() else {
                    continue;
                };
                if let Some(sample) = editor.doc.samples.get_mut(index) {
                    sample.clip = clip.clone();
                }
                editor.revision = editor.revision.wrapping_add(1);
                editor.status = format!("采样点 #{index} → {clip}");
            }
            _ => {}
        }
    }
}

/// 把当前选中的混合器载入编辑器：换系统或换角色时重新取文档与参数。
/// 只读快照，写回走 [`publish_edit`]。
fn load_editor_state(
    cache: Res<MixerCache>,
    mut editor: ResMut<SpaceEditor>,
    mut rebuild: ResMut<ForceRebuild>,
) {
    let Some(snapshot) = cache.mixers.get(editor.mixer) else {
        return;
    };
    if editor.loaded_key.as_deref() == Some(snapshot.key.as_str())
        && editor.character == snapshot.character
    {
        return;
    }
    editor.loaded_key = Some(snapshot.key.clone());
    editor.character = snapshot.character.clone();
    editor.doc = snapshot.space.clone();
    editor.params = snapshot.params.clone();
    editor.enabled = snapshot.enabled;
    editor.selected = None;
    editor.revision = editor.revision.wrapping_add(1);
    rebuild.0 = true;
}

/// 把编辑器的工作副本交给动画系统；面板关掉后参数所有权交还游戏逻辑。
fn publish_edit(
    mut edit: ResMut<MixerEdit>,
    mut focus: ResMut<AnimFocus>,
    editor: Res<SpaceEditor>,
) {
    let key = editor.open.then(|| editor.loaded_key.clone()).flatten();
    match &key {
        Some(id) => focus.focus(id),
        None => focus.clear(),
    }
    if edit.mixer != key {
        edit.mixer = key;
    }
    let Some(_) = edit.mixer else {
        return;
    };
    if edit.doc != editor.doc {
        edit.doc = editor.doc.clone();
    }
    if edit.params != editor.params {
        edit.params = editor.params.clone();
    }
    edit.enabled = editor.enabled;
}

/// 面板打开时用数字键 1..9 切换混合系统
fn switch_mixer_keys(
    keys: Res<ButtonInput<KeyCode>>,
    cache: Res<MixerCache>,
    mut editor: ResMut<SpaceEditor>,
) {
    // 只在网格打开时接管数字键，避免影响游戏本身的按键
    if !editor.open {
        return;
    }
    const DIGITS: [KeyCode; 9] = [
        KeyCode::Digit1,
        KeyCode::Digit2,
        KeyCode::Digit3,
        KeyCode::Digit4,
        KeyCode::Digit5,
        KeyCode::Digit6,
        KeyCode::Digit7,
        KeyCode::Digit8,
        KeyCode::Digit9,
    ];
    for (index, key) in DIGITS.iter().enumerate() {
        if !keys.just_pressed(*key) || index >= cache.mixers.len() {
            continue;
        }
        if editor.mixer == index && editor.loaded_key.as_deref().is_some() {
            continue;
        }
        let title = cache.mixers[index].title.clone();
        editor.mixer = index;
        editor.loaded_key = None;
        editor.status = format!("切换到混合系统：{title}（{}）", index + 1);
        editor.revision = editor.revision.wrapping_add(1);
    }
}

fn sync_enabled_label(editor: Res<SpaceEditor>, mut texts: Query<&mut Text, With<EnabledLabel>>) {
    if !editor.is_changed() {
        return;
    }
    let desired = if editor.enabled { "开" } else { "关" };
    for mut text in &mut texts {
        if text.0 != desired {
            text.0 = desired.to_string();
        }
    }
}

fn sync_status(editor: Res<SpaceEditor>, mut texts: Query<&mut Text, With<StatusText>>) {
    if !editor.is_changed() {
        return;
    }
    for mut text in &mut texts {
        if text.0 != editor.status {
            text.0 = editor.status.clone();
        }
    }
}

fn sync_weight_text(editor: Res<SpaceEditor>, mut texts: Query<&mut Text, With<WeightText>>) {
    if !editor.is_changed() {
        return;
    }
    let point = [
        editor
            .params
            .get(editor.doc.axis(0))
            .copied()
            .unwrap_or(0.0),
        editor
            .params
            .get(editor.doc.axis(1))
            .copied()
            .unwrap_or(0.0),
    ];
    let weights = editor.doc.weights(point);
    let hint = if weights.is_empty() {
        "无可用采样点".to_string()
    } else {
        weights
            .iter()
            .map(|(clip, weight)| format!("{clip} {:.0}%", weight * 100.0))
            .collect::<Vec<_>>()
            .join(" · ")
    };
    let desired = format!("当前权重：{hint}");
    for mut text in &mut texts {
        if text.0 != desired {
            text.0 = desired.clone();
        }
    }
}

fn sync_param_labels(editor: Res<SpaceEditor>, mut labels: Query<(&ParamValue, &mut Text)>) {
    if !editor.is_changed() {
        return;
    }
    for (value, mut text) in &mut labels {
        let desired = match value.slot {
            0 | 1 => {
                let name = editor.doc.axis(value.slot).to_string();
                let number = editor.params.get(&name).copied().unwrap_or(0.0);
                format!("{name} = {number:.2}")
            }
            _ => format!("播放速度 {:.2}x", editor.doc.speed),
        };
        if text.0 != desired {
            text.0 = desired;
        }
    }
}

/// 让参数滑杆的滑块跟着参数值走（网格拖当前值或别处改参数时保持同步）
fn sync_param_sliders(
    editor: Res<SpaceEditor>,
    mut commands: Commands,
    sliders: Query<(Entity, &ParamSlider, &SliderValue)>,
) {
    if !editor.is_changed() {
        return;
    }
    for (entity, slider, current) in &sliders {
        let desired = match slider.slot {
            0 | 1 => editor
                .params
                .get(editor.doc.axis(slider.slot))
                .copied()
                .unwrap_or(0.0),
            _ => editor.doc.speed,
        };
        if (current.0 - desired).abs() > 1e-4 {
            commands.entity(entity).insert(SliderValue(desired));
        }
    }
}

fn sync_root_motion_label(
    selection: Res<AnimSelection>,
    mut labels: Query<&mut Text, With<RootMotionLabel>>,
) {
    if !selection.is_changed() {
        return;
    }
    let desired = if selection.root_motion {
        "根运动：开"
    } else {
        "根运动：关"
    };
    for mut text in &mut labels {
        if text.0 != desired {
            text.0 = desired.to_string();
        }
    }
}
