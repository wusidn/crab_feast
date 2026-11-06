//! Blend Space 2D 编辑器：两个参数轴 + 网格上的动画采样点。
//!
//! 没有节点图：文档只有一个混合空间，编辑对象就是采样点本身。
//! 布局上分两块：
//! - 屏幕下方停靠的只有 2D 网格本体（[`EditorRoot`]，F3 显示 / 隐藏）；
//! - 混合系统、参数、采样点管理、存取都挂在菜单的「混合空间」页面里
//!   （[`fill_menu_page`]，页面 id 为 [`BLEND_SPACE_PAGE`]）。

pub(crate) mod driver;
mod grid;
mod inspector;
mod registry;

use std::collections::HashMap;

use bevy::{
    input::mouse::MouseScrollUnit,
    picking::pointer::PointerButton,
    prelude::*,
    ui::{OverflowAxis, UiGlobalTransform},
    ui_widgets::SliderValue,
};

use crab_feast_library::anim::{BlendSpaceDoc, ClipSet};

use crate::{
    ui::menu::{widgets, MenuConfig, MenuEvent, MenuPageBody, MenuPageDef, MenuState, DRAWER_WIDTH},
    GameState,
};

use driver::PoseDriver;

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
    /// 当前展开的下拉框
    pub open_dropdown: Option<BlendDropdown>,
}

/// 混合空间页面里的下拉框种类（同一时间只展开一个）
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BlendDropdown {
    /// 混合系统切换
    Mixer,
    /// 选中采样点的动画剪辑
    Clip,
}

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
            loaded_key: None,
            enabled: true,
            doc: BlendSpaceDoc::default(),
            revision: 0,
            selected: None,
            params: HashMap::from([
                ("yaw".to_string(), 0.0),
                ("speed".to_string(), 0.5),
            ]),
            status: String::new(),
            drag: None,
            gesture_travel: 0.0,
            open_dropdown: None,
            marker_drag: None,
        }
    }
}

#[derive(Component)]
pub struct EditorRoot;
/// 下拉框触发器（种类只用于调试识别，交互由观察者处理）
#[derive(Component)]
pub struct BlendDropdownTrigger;
/// 下拉框浮层
#[derive(Component)]
pub struct BlendDropdownPopup(pub BlendDropdown);
/// 浮层锚点：记住触发器，用来算可展开空间
#[derive(Component)]
pub struct DropdownAnchor {
    pub trigger: Entity,
}
/// 菜单「混合空间」页面里的内容根节点
#[derive(Component)]
struct SpacePageRoot;
#[derive(Component)]
pub struct GridLayer;
#[derive(Component)]
pub struct SampleLayer;
#[derive(Component)]
pub struct SampleDot {
    pub index: usize,
}
#[derive(Component)]
pub struct ParamMarker;
#[derive(Component)]
pub struct InspectorBody;
#[derive(Component)]
pub struct StatusText;
#[derive(Component)]
pub struct RootMotionLabel;
/// 需要每帧刷新数值文本的滑杆标签
#[derive(Component)]
pub struct WeightText;

#[derive(Component)]
pub struct EnabledLabel;

#[derive(Component)]
pub struct ParamValue {
    /// 0/1 = 两个轴，2 = 播放速度
    pub slot: usize,
}

/// 检查器里的参数滑杆（根节点），slot 语义同 [`ParamValue`]
#[derive(Component)]
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

pub fn label(
    commands: &mut Commands,
    parent: Entity,
    font: &Handle<Font>,
    text: impl Into<String>,
    size: f32,
    color: Color,
) -> Entity {
    commands
        .spawn((
            Text::new(text),
            TextFont {
                font: FontSource::from(font.clone()),
                font_size: FontSize::Px(size),
                ..default()
            },
            TextColor(color),
            ChildOf(parent),
        ))
        .id()
}

pub struct BlendSpaceEditorPlugin;

impl Plugin for BlendSpaceEditorPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<SpaceEditor>()
            .init_resource::<ClipSet>()
            .init_resource::<PoseDriver>()
            .init_resource::<ForceRebuild>()
            .init_resource::<registry::MixerRegistry>()
            .init_resource::<registry::MixerCache>()
            .add_systems(Startup, register_menu_page)
            .add_systems(OnEnter(GameState::Game), registry::init_mixers)
            .add_systems(
                Update,
                (
                    toggle_editor,
                    open_grid_on_page,
                    build_editor_ui,
                    sync_dock_inset,
                    fill_menu_page,
                )
                    .chain()
                    .run_if(in_state(GameState::Game)),
            )
            .add_systems(
                Update,
                (
                    driver::sync_clip_set,
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
                    sync_dropdown,
                    registry::sync_mixers,
                    widgets::update_slider_styling,
                )
                    .chain()
                    .run_if(in_state(GameState::Game)),
            )
            .add_systems(
                PostUpdate,
                driver::drive_pose
                    .after(bevy::app::AnimationSystems)
                    .before(bevy::transform::TransformSystems::Propagate)
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
    if config
        .pages
        .iter()
        .any(|page| page.id == BLEND_SPACE_PAGE)
    {
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
    asset_server: Res<AssetServer>,
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

    let font: Handle<Font> = asset_server.load("fonts/PingFang-SC-Regular.ttf");
    // 新面板里的采样点层是空的，强制下一帧重画
    force.0 = true;
    // 停靠在屏幕下方：只有 2D 网格本体，其他控件都在菜单页面里
    let root = commands
        .spawn((
            Node {
                position_type: PositionType::Absolute,
                left: Val::Px(0.0),
                bottom: Val::Px(0.0),
                width: Val::Percent(100.0),
                height: Val::Px(PANEL_HEIGHT),
                flex_direction: FlexDirection::Row,
                justify_content: JustifyContent::Center,
                align_items: AlignItems::FlexStart,
                padding: UiRect {
                    top: Val::Px(GRID_TOP),
                    right: Val::Px(0.0),
                    ..default()
                },
                overflow: Overflow::clip(),
                ..default()
            },
            GlobalZIndex(60),
            EditorRoot,
        ))
        .id();

    let area = build_grid_area(&mut commands, root);
    grid::build_grid(&mut commands, area, &font, &editor);
}

/// 网格区：横向长方形居中
fn build_grid_area(commands: &mut Commands, parent: Entity) -> Entity {
    commands
        .spawn((
            Node {
                flex_grow: 1.0,
                min_width: Val::Px(0.0),
                flex_direction: FlexDirection::Row,
                justify_content: JustifyContent::Center,
                align_items: AlignItems::FlexStart,
                ..default()
            },
            ChildOf(parent),
        ))
        .id()
}

/// 抽屉展开多少，网格就在剩下的左侧空白区域居中
fn sync_dock_inset(
    menu: Res<MenuState>,
    mut roots: Query<&mut Node, With<EditorRoot>>,
) {
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
    asset_server: Res<AssetServer>,
    editor: Res<SpaceEditor>,
    cache: Res<registry::MixerCache>,
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
    let font: Handle<Font> = asset_server.load("fonts/PingFang-SC-Regular.ttf");
    let root = commands
        .spawn((
            Node {
                width: Val::Percent(100.0),
                flex_grow: 1.0,
                min_height: Val::Px(0.0),
                flex_direction: FlexDirection::Column,
                row_gap: Val::Px(8.0),
                ..default()
            },
            SpacePageRoot,
            ChildOf(body),
        ))
        .id();
    // 点页面空白处收起下拉框（触发器和选项都 propagate(false)，不会误触发）
    commands.entity(root).observe(
        |click: On<PointerClick>, mut editor: ResMut<SpaceEditor>| {
            if click.button == PointerButton::Primary && editor.open_dropdown.is_some() {
                editor.open_dropdown = None;
            }
        },
    );
    build_page_content(&mut commands, root, &font, &editor, &cache);
}

fn build_page_content(
    commands: &mut Commands,
    root: Entity,
    font: &Handle<Font>,
    editor: &SpaceEditor,
    cache: &registry::MixerCache,
) {
    // 混合系统切换（下拉框）
    label(commands, root, font, "混合系统", 12.0, MUTED_TEXT);
    let titles = cache
        .mixers
        .iter()
        .map(|mixer| mixer.title.clone())
        .collect::<Vec<_>>();
    let selected = editor.mixer.min(titles.len().saturating_sub(1));
    let parts = spawn_dropdown(
        commands,
        root,
        font,
        BlendDropdown::Mixer,
        &titles,
        selected,
    );
    for (index, item) in parts.items.into_iter().enumerate() {
        let key = cache.mixers.get(index).map(|mixer| mixer.key.clone()).unwrap_or_default();
        commands.entity(item).observe(
            move |mut click: On<PointerClick>, mut editor: ResMut<SpaceEditor>| {
                if click.button != PointerButton::Primary {
                    return;
                }
                click.propagate(false);
                editor.open_dropdown = None;
                if editor.loaded_key.as_deref() != Some(key.as_str()) {
                    editor.mixer = index;
                    editor.loaded_key = None;
                    editor.status = format!("切换到混合系统：{}", key);
                    editor.revision = editor.revision.wrapping_add(1);
                }
            },
        );
    }

    let status = label(commands, root, font, editor.status.clone(), 11.0, MUTED_TEXT);
    commands.entity(status).insert(StatusText);

    // 参数
    label(commands, root, font, "参数", 12.0, MUTED_TEXT);
    let params = commands
        .spawn((
            Node {
                width: Val::Percent(100.0),
                flex_direction: FlexDirection::Column,
                row_gap: Val::Px(4.0),
                ..default()
            },
            ChildOf(root),
        ))
        .id();
    inspector::build_params(commands, params, font, editor, cache);

    // 采样点：列表 + 当前位置添加 + 当前权重
    label(commands, root, font, "采样点", 12.0, MUTED_TEXT);
    commands.spawn((
        Node {
            width: Val::Percent(100.0),
            flex_direction: FlexDirection::Column,
            row_gap: Val::Px(4.0),
            ..default()
        },
        InspectorBody,
        ChildOf(root),
    ));

    // 工具行
    let tools = commands
        .spawn((
            Node {
                width: Val::Percent(100.0),
                flex_direction: FlexDirection::Row,
                flex_wrap: FlexWrap::Wrap,
                align_items: AlignItems::Center,
                column_gap: Val::Px(6.0),
                row_gap: Val::Px(6.0),
                ..default()
            },
            ChildOf(root),
        ))
        .id();

    toolbar_button(commands, tools, font, "保存", |editor| {
        inspector::save_space(editor);
    });
    toolbar_button(commands, tools, font, "载入", |editor| {
        inspector::load_space(editor);
    });

    let root_button = commands
        .spawn((
            Node {
                padding: UiRect::axes(Val::Px(10.0), Val::Px(4.0)),
                border: UiRect::all(Val::Px(1.0)),
                border_radius: BorderRadius::all(Val::Px(5.0)),
                ..default()
            },
            BackgroundColor(Color::srgb(0.14, 0.16, 0.22)),
            BorderColor::all(Color::srgba(1.0, 1.0, 1.0, 0.14)),
            ChildOf(tools),
        ))
        .id();
    let root_text = label(commands, root_button, font, "根运动：关", 11.0, TEXT_COLOR);
    commands.entity(root_text).insert(RootMotionLabel);
    commands.entity(root_button).observe(
        move |click: On<PointerClick>, mut driver: ResMut<PoseDriver>| {
            if click.button != PointerButton::Primary {
                return;
            }
            driver.root_motion = !driver.root_motion;
        },
    );

    toolbar_button(commands, tools, font, "隐藏网格", |editor| {
        editor.open = false;
    });

    label(
        commands,
        root,
        font,
        "拖采样点会吸附到网格刻度点，按住 Shift 自由放置 · 白色标记是当前参数值 · F3 显示 / 隐藏下方网格",
        10.0,
        MUTED_TEXT,
    );
}

fn toolbar_button(
    commands: &mut Commands,
    parent: Entity,
    font: &Handle<Font>,
    text: &str,
    action: impl Fn(&mut SpaceEditor) + Send + Sync + 'static,
) {
    let button = widgets::button(commands, parent, Some(font), text.to_string());
    commands.entity(button).observe(
        move |mut click: On<PointerClick>, mut editor: ResMut<SpaceEditor>| {
            if click.button != PointerButton::Primary {
                return;
            }
            click.propagate(false);
            action(&mut editor);
            editor.revision = editor.revision.wrapping_add(1);
        },
    );
}

/// 生成下拉框：通用外观交给 menu::widgets，这里挂上展开/收起所需的标记
fn spawn_dropdown(
    commands: &mut Commands,
    parent: Entity,
    font: &Handle<Font>,
    kind: BlendDropdown,
    labels: &[String],
    selected: usize,
) -> widgets::DropdownParts {
    let parts = widgets::dropdown(commands, parent, Some(font), labels, selected);
    // 浮层改成自己可滚：内容长的时候在浮层里滚，而不是被外层面板吞掉滚轮
    commands.entity(parts.popup).insert((
        Node {
            position_type: PositionType::Absolute,
            top: Val::Percent(100.0),
            left: Val::Px(0.0),
            right: Val::Px(0.0),
            margin: UiRect::top(Val::Px(4.0)),
            flex_direction: FlexDirection::Column,
            row_gap: Val::Px(2.0),
            padding: UiRect::all(Val::Px(4.0)),
            border: UiRect::all(Val::Px(1.0)),
            border_radius: BorderRadius::all(Val::Px(8.0)),
            max_height: Val::Px(280.0),
            overflow: Overflow::scroll_y(),
            display: Display::None,
            ..default()
        },
        ScrollPosition::DEFAULT,
        DropdownAnchor {
            trigger: parts.trigger,
        },
    ));

    // 浮层自己的滚轮：拦在浮层上，避免继续冒泡去滚面板
    let popup = parts.popup;
    commands.entity(popup).observe(
        move |mut scroll: On<PointerScroll>,
              mut positions: Query<&mut ScrollPosition>,
              nodes: Query<&ComputedNode>| {
            scroll.propagate(false);
            let pixels = match scroll.unit {
                MouseScrollUnit::Line => scroll.y * 40.0,
                MouseScrollUnit::Pixel => scroll.y,
            }
            .clamp(-320.0, 320.0);
            let (Ok(mut position), Ok(computed)) = (positions.get_mut(popup), nodes.get(popup))
            else {
                return;
            };
            let max = (computed.content_size().y - computed.size().y).max(0.0)
                * computed.inverse_scale_factor();
            position.0.y = (position.0.y - pixels).clamp(0.0, max);
        },
    );

    commands
        .entity(parts.trigger)
        .insert(BlendDropdownTrigger)
        .observe(
            move |mut click: On<PointerClick>, mut editor: ResMut<SpaceEditor>| {
                if click.button != PointerButton::Primary {
                    return;
                }
                // 阻断冒泡，否则页面根节点的“点空白收起”会立刻把它关掉
                click.propagate(false);
                editor.open_dropdown = if editor.open_dropdown == Some(kind) {
                    None
                } else {
                    Some(kind)
                };
            },
        );
    commands.entity(parts.popup).insert(BlendDropdownPopup(kind));
    parts
}

/// 按 `open_dropdown` 显隐浮层
fn sync_dropdown(
    editor: Res<SpaceEditor>,
    mut popups: Query<(&BlendDropdownPopup, &DropdownAnchor, &mut Node, &mut ScrollPosition)>,
    mut ui: ParamSet<(
        Query<(&ComputedNode, &UiGlobalTransform)>,
        Query<(&Node, Option<&ChildOf>), Without<BlendDropdownPopup>>,
    )>,
) {
    // 收起状态只保证浮层隐藏，不做几何测量（每帧走父链 + 取 rect 是白花）
    let Some(open_kind) = editor.open_dropdown else {
        for (_popup, _anchor, mut node, mut scroll) in &mut popups {
            if node.display != Display::None {
                node.display = Display::None;
                scroll.0 = Vec2::ZERO;
            }
        }
        return;
    };

    for (popup, anchor, mut node, mut scroll) in &mut popups {
        if popup.0 != open_kind {
            if node.display != Display::None {
                node.display = Display::None;
            }
            continue;
        }
        if node.display != Display::Flex {
            scroll.0 = Vec2::ZERO;
            node.display = Display::Flex;
        }
        // 触发器的上下边界（UI 逻辑像素，原点左上）
        let trigger_rect = {
            let nodes = ui.p0();
            nodes.get(anchor.trigger).ok().map(|(computed, transform)| {
                let height = computed.size().y * computed.inverse_scale_factor();
                let center = transform.translation.y;
                (center - height * 0.5, center + height * 0.5)
            })
        };
        let Some((trigger_top, trigger_bottom)) = trigger_rect else {
            continue;
        };

        // 沿父链找第一个会裁剪后代的祖先（面板 body 的滚动区 / clip 容器）
        let host = {
            let nodes = ui.p1();
            let mut current = anchor.trigger;
            let mut found = None;
            while let Ok((_style, child_of)) = nodes.get(current) {
                let Some(parent) = child_of.map(|child_of| child_of.parent()) else {
                    break;
                };
                current = parent;
                if let Ok((parent_style, _)) = nodes.get(parent) {
                    if parent_style.overflow.y != OverflowAxis::Visible {
                        found = Some(parent);
                        break;
                    }
                }
            }
            found
        };
        let host_rect = {
            let nodes = ui.p0();
            host.and_then(|host| {
                nodes.get(host).ok().map(|(computed, transform)| {
                    let height = computed.size().y * computed.inverse_scale_factor();
                    let center = transform.translation.y;
                    (center - height * 0.5, center + height * 0.5)
                })
            })
        };
        let Some((host_top, host_bottom)) = host_rect else {
            continue;
        };

        let space_up = (trigger_top - host_top - 6.0).max(0.0);
        let space_down = (host_bottom - trigger_bottom - 6.0).max(0.0);
        let upward = space_up > space_down;
        let available = if upward { space_up } else { space_down };
        // 高度按 8px 取整，避免每帧微调布局
        let max_height = Val::Px(((available.min(300.0)) / 8.0).floor().max(2.0) * 8.0);

        let want_top = if upward { Val::Auto } else { Val::Percent(100.0) };
        let want_bottom = if upward { Val::Percent(100.0) } else { Val::Auto };
        if node.top != want_top || node.bottom != want_bottom {
            node.top = want_top;
            node.bottom = want_bottom;
            node.margin = if upward {
                UiRect::bottom(Val::Px(4.0))
            } else {
                UiRect::top(Val::Px(4.0))
            };
        }
        if node.max_height != max_height {
            node.max_height = max_height;
        }
    }
}

/// 面板打开时用数字键 1..9 切换混合系统
fn switch_mixer_keys(
    keys: Res<ButtonInput<KeyCode>>,
    cache: Res<registry::MixerCache>,
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

fn sync_weight_text(
    editor: Res<SpaceEditor>,
    mut texts: Query<&mut Text, With<WeightText>>,
) {
    if !editor.is_changed() {
        return;
    }
    let point = [
        editor.params.get(editor.doc.axis(0)).copied().unwrap_or(0.0),
        editor.params.get(editor.doc.axis(1)).copied().unwrap_or(0.0),
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

fn sync_param_labels(
    editor: Res<SpaceEditor>,
    mut labels: Query<(&ParamValue, &mut Text)>,
) {
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
    driver: Res<PoseDriver>,
    mut labels: Query<&mut Text, With<RootMotionLabel>>,
) {
    if !driver.is_changed() {
        return;
    }
    let desired = if driver.root_motion { "根运动：开" } else { "根运动：关" };
    for mut text in &mut labels {
        if text.0 != desired {
            text.0 = desired.to_string();
        }
    }
}

