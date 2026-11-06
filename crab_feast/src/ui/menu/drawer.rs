//! 通用菜单组件：右上角图标 + 右侧抽屉 + 可嵌套的多级页面。
//!
//! 使用方式：
//! 1. 插入 [`MenuConfig`] 描述页面树；
//! 2. 添加 [`MenuPlugin::new(状态)`]，菜单只在该状态下存在；
//! 3. 需要自定义内容的页面，在 `MenuSet::Setup` 之后往 [`MenuPageBody`] 里挂子节点。

use bevy::{
    input::mouse::MouseScrollUnit,picking::pointer::PointerButton, prelude::*, state::state::States};

use super::widgets;

/// 页面标识。`None` 表示根页面。
pub type PageId = &'static str;

/// 抽屉宽度，同时也是页面滑动的距离
pub const DRAWER_WIDTH: f32 = 340.0;
/// 滚轮「一行」折算的像素数
const SCROLL_LINE_PX: f32 = 40.0;
/// 单次滚动事件的最大位移，避免异常增量把内容甩飞
const MAX_SCROLL_STEP: f32 = 320.0;

const HEADER_HEIGHT: f32 = 56.0;
/// 抽屉开合的临界阻尼弹簧角频率（越大越快，约 5/ω 秒到位）
const DRAWER_OMEGA: f32 = 20.0;
/// 弹簧每步的最大步长：把一帧拆成多个子步，避免大 delta 时积分发散
const DRAWER_MAX_STEP: f32 = 1.0 / 120.0;
/// 距目标小于这个距离、且速度低于下面的阈值时，直接落到目标（否则永远逼近不到 0）
const DRAWER_REST_DISTANCE: f32 = 0.002;
const DRAWER_REST_SPEED: f32 = 0.01;
/// 单个页面滑入 / 滑出的时长
const SLIDE_SECONDS: f32 = 0.26;

const PANEL_BACKGROUND: Color = Color::srgb(0.04, 0.05, 0.08);
const HEADER_BACKGROUND: Color = Color::srgb(0.07, 0.09, 0.14);
const ITEM_IDLE: Color = Color::srgba(0.16, 0.18, 0.24, 0.9);
const BORDER_SOFT: Color = Color::srgba(1.0, 1.0, 1.0, 0.12);
const TEXT_COLOR: Color = Color::srgb(0.88, 0.91, 0.96);
const ICON_BACKGROUND: Color = Color::srgba(0.08, 0.1, 0.15, 0.86);

/// 一个菜单页面。父子关系由 `parent` 决定，根页面的子项 `parent` 为 `None`。
#[derive(Clone, Debug)]
pub struct MenuPageDef {
    pub id: PageId,
    pub title: &'static str,
    pub description: &'static str,
    pub parent: Option<PageId>,
}

impl MenuPageDef {
    /// 根页面下的一级入口
    pub fn section(id: PageId, title: &'static str, description: &'static str) -> Self {
        Self {
            id,
            title,
            description,
            parent: None,
        }
    }

    /// 挂在某个页面下的子入口，可以继续嵌套
    #[allow(dead_code)] // 多级菜单入口：当前只有一级，保留给后续子页面
    pub fn item(
        id: PageId,
        title: &'static str,
        description: &'static str,
        parent: PageId,
    ) -> Self {
        Self {
            id,
            title,
            description,
            parent: Some(parent),
        }
    }
}

/// 菜单配置，必须在进入菜单状态前插入。
#[derive(Resource, Clone, Debug)]
pub struct MenuConfig {
    /// 一级菜单标题
    pub root_title: &'static str,
    /// 中文字体资源路径；不填则用 Bevy 默认字体（中文会缺字形）
    pub font_path: Option<&'static str>,
    /// 开合菜单的快捷键
    pub toggle_keys: Vec<KeyCode>,
    pub pages: Vec<MenuPageDef>,
}

impl Default for MenuConfig {
    fn default() -> Self {
        Self {
            root_title: "功能菜单",
            font_path: None,
            toggle_keys: vec![KeyCode::F2, KeyCode::Backquote],
            pages: Vec::new(),
        }
    }
}

/// 页面切换 / 开合的对外事件，应用可以据此重置自己的面板状态。
#[derive(Message, Clone, Debug, PartialEq, Eq)]
pub enum MenuEvent {
    Opened,
    Closed,
    PageChanged {
        from: Option<PageId>,
        to: Option<PageId>,
    },
}

/// 菜单系统集合。`Setup` 是 UI 生成阶段，自定义页面内容必须排在它之后。
#[derive(SystemSet, Debug, Clone, PartialEq, Eq, Hash)]
pub enum MenuSet {
    Setup,
}

/// 页面内容区。应用可以按 `page` 找到自己的页面并往里挂自定义 UI。
#[derive(Component, Clone, Debug, Default)]
pub struct MenuPageBody {
    /// `None` 表示根页面的内容区
    pub page: Option<PageId>,
}

#[derive(Resource)]
pub struct MenuState {
    pub open: bool,
    stack: Vec<Option<PageId>>,
    /// 抽屉展开进度：0 完全收起，1 完全展开
    drawer: f32,
    /// 抽屉展开速度，配合弹簧阻尼使用；切换中途反向时会自然接续
    drawer_velocity: f32,
    transition: Option<PageTransition>,
}

impl Default for MenuState {
    fn default() -> Self {
        Self {
            open: false,
            stack: vec![None],
            drawer: 0.0,
            drawer_velocity: 0.0,
            transition: None,
        }
    }
}

impl MenuState {
    /// 抽屉展开进度：0 完全收起，1 完全展开
    pub fn drawer_progress(&self) -> f32 {
        self.drawer
    }

    /// 当前停留的页面，`None` 为根页面
    pub fn current(&self) -> Option<PageId> {
        *self.stack.last().unwrap_or(&None)
    }

    #[allow(dead_code)] // 页面判断接口，供后续功能页面查询自己是否处于前台
    pub fn is_on(&self, page: PageId) -> bool {
        self.current() == Some(page)
    }

    fn toggle(&mut self) -> Option<MenuEvent> {
        if !self.open {
            self.stack.clear();
            self.stack.push(None);
        }
        self.open = !self.open;
        self.transition = None;
        Some(if self.open {
            MenuEvent::Opened
        } else {
            MenuEvent::Closed
        })
    }

    /// 进入子页面：压栈并从右侧滑入
    fn push(&mut self, page: PageId) -> Option<MenuEvent> {
        if !self.open || self.transition.is_some() || self.current() == Some(page) {
            return None;
        }
        let from = self.current();
        self.stack.push(Some(page));
        self.transition = Some(PageTransition {
            from,
            to: Some(page),
            forward: true,
            t: 0.0,
        });
        Some(MenuEvent::PageChanged {
            from,
            to: Some(page),
        })
    }

    /// 返回上一级：出栈并向右滑出；已经在根页面时关闭抽屉
    fn back(&mut self) -> Option<MenuEvent> {
        if self.transition.is_some() {
            return None;
        }
        if self.stack.len() <= 1 {
            self.open = false;
            return Some(MenuEvent::Closed);
        }
        let from = self.stack.pop().flatten();
        let to = self.current();
        self.transition = Some(PageTransition {
            from,
            to,
            forward: false,
            t: 0.0,
        });
        Some(MenuEvent::PageChanged { from, to })
    }
}

/// 一次页面切换动画
struct PageTransition {
    from: Option<PageId>,
    to: Option<PageId>,
    /// true：新页面从右侧滑入；false：当前页面向右滑出
    forward: bool,
    t: f32,
}

#[derive(Component, Clone, Default)]
struct MenuRoot;

#[derive(Component, Clone, Default)]
struct MenuPanelRoot;

#[derive(Component, Clone, Default)]
struct MenuPageRoot(Option<PageId>);

/// 右上角的菜单图标；抽屉展开时随进度缩小隐去
#[derive(Component, Clone, Default)]
struct MenuIcon;

/// 通用菜单插件。菜单只在一个状态内存在，例如 `MenuPlugin::new(GameState::Game)`。
pub struct MenuPlugin<S: States> {
    state: S,
}

impl<S: States> MenuPlugin<S> {
    pub fn new(state: S) -> Self {
        Self { state }
    }
}

impl<S: States> Plugin for MenuPlugin<S> {
    fn build(&self, app: &mut App) {
        app.init_resource::<MenuConfig>()
            .init_resource::<MenuState>()
            .add_message::<MenuEvent>()
            .add_systems(
                OnEnter(self.state.clone()),
                setup_menu.in_set(MenuSet::Setup),
            )
            .add_systems(OnExit(self.state.clone()), cleanup_menu)
            .add_systems(Update, keep_page_rows_unshrinkable)
            .add_systems(
                Update,
                (menu_hotkeys, advance_menu, sync_menu_layout)
                    .chain()
                    .run_if(in_state(self.state.clone())),
            );
    }
}

/// 页面内容行不参与 flex 压缩：否则内容会被压进可视区，永远不会溢出，也就滚不动
fn keep_page_rows_unshrinkable(
    bodies: Query<&Children, With<MenuPageBody>>,
    mut nodes: Query<&mut Node>,
) {
    for children in &bodies {
        for child in children.iter() {
            if let Ok(mut node) = nodes.get_mut(child) {
                if node.flex_shrink != 0.0 {
                    node.flex_shrink = 0.0;
                }
            }
        }
    }
}

fn setup_menu(mut commands: Commands, asset_server: Res<AssetServer>, config: Res<MenuConfig>) {
    let config = config.clone();
    let font: Option<Handle<Font>> = config
        .font_path
        .map(|path| asset_server.load::<Font>(path));

    spawn_icon(&mut commands);

    // 抽屉面板：靠 right 从 -DRAWER_WIDTH 滑到 0
    let drawer_hidden_right = Val::Px(-DRAWER_WIDTH);
    let drawer = commands
        .spawn_scene(bsn! {
            Node {
                position_type: PositionType::Absolute,
                top: Val::Px(0.0),
                bottom: Val::Px(0.0),
                right: drawer_hidden_right,
                width: Val::Px(DRAWER_WIDTH),
                flex_direction: FlexDirection::Column,
                border: UiRect::left(Val::Px(1.0)),
                display: Display::None,
            }
            BackgroundColor(PANEL_BACKGROUND)
            BorderColor::all(BORDER_SOFT)
            GlobalZIndex(100)
            MenuRoot
            MenuPanelRoot
        })
        .id();

    // 页面容器：页面在容器内横向滑动，超出部分被容器裁掉
    let container = commands
        .spawn_scene(bsn! {
            Node {
                width: Val::Percent(100.0),
                flex_grow: 1.0,
                overflow: Overflow::clip(),
            }
            ChildOf(drawer)
        })
        .id();

    spawn_page(
        &mut commands,
        container,
        None,
        config.root_title,
        &config,
        font.as_ref(),
    );
    for page in &config.pages {
        spawn_page(
            &mut commands,
            container,
            Some(page.id),
            page.title,
            &config,
            font.as_ref(),
        );
    }
}

/// 右上角的菜单图标（三条横线，纯 Node 绘制，不依赖字体字形）
fn spawn_icon(commands: &mut Commands) {
    let icon = commands
        .spawn_scene(bsn! {
            Node {
                position_type: PositionType::Absolute,
                top: Val::Px(12.0),
                right: Val::Px(12.0),
                width: Val::Px(46.0),
                height: Val::Px(38.0),
                align_items: AlignItems::Center,
                justify_content: JustifyContent::Center,
                border: UiRect::all(Val::Px(1.0)),
                border_radius: BorderRadius::all(Val::Px(9.0)),
            }
            BackgroundColor(ICON_BACKGROUND)
            BorderColor::all(BORDER_SOFT)
            GlobalZIndex(200)
            MenuRoot
            MenuIcon
        })
        .insert(UiTransform::IDENTITY)
        .observe(
            |mut click: On<PointerClick>,
             mut menu: ResMut<MenuState>,
             mut events: MessageWriter<MenuEvent>| {
                if click.button != PointerButton::Primary {
                    return;
                }
                click.propagate(false);
                if let Some(event) = menu.toggle() {
                    events.write(event);
                }
            },
        )
        .id();

    let bars = commands
        .spawn_scene(bsn! {
            Node {
                flex_direction: FlexDirection::Column,
                row_gap: Val::Px(4.0),
            }
            ChildOf(icon)
        })
        .id();

    for _ in 0..3 {
        commands.spawn_scene(bsn! {
            Node {
                width: Val::Px(20.0),
                height: Val::Px(2.0),
                border_radius: BorderRadius::all(Val::Px(1.0)),
            }
            BackgroundColor(TEXT_COLOR)
            ChildOf(bars)
        });
    }
}

/// 一个菜单页面：顶部返回栏 + 内容区（自动生成子页面入口，应用可追加自定义内容）
fn spawn_page(
    commands: &mut Commands,
    container: Entity,
    page: Option<PageId>,
    title: &'static str,
    config: &MenuConfig,
    font: Option<&Handle<Font>>,
) {
    let page_entity = commands
        .spawn_scene(bsn! {
            Node {
                position_type: PositionType::Absolute,
                top: Val::Px(0.0),
                left: Val::Px(0.0),
                width: Val::Percent(100.0),
                height: Val::Percent(100.0),
                flex_direction: FlexDirection::Column,
                display: Display::None,
            }
            BackgroundColor(PANEL_BACKGROUND)
            GlobalZIndex(110)
            MenuPageRoot(page)
            ChildOf(container)
        })
        .id();

    let header = commands
        .spawn_scene(bsn! {
            Node {
                width: Val::Percent(100.0),
                height: Val::Px(HEADER_HEIGHT),
                flex_shrink: 0.0,
                align_items: AlignItems::Center,
                column_gap: Val::Px(8.0),
                padding: UiRect::axes(Val::Px(10.0), Val::Px(0.0)),
                border: UiRect::bottom(Val::Px(1.0)),
            }
            BackgroundColor(HEADER_BACKGROUND)
            BorderColor::all(BORDER_SOFT)
            ChildOf(page_entity)
        })
        .id();

    // 返回按钮：根页面时等同于关闭抽屉
    let back = widgets::icon_button(commands, header, font, "‹".to_string(), 18.0);

    commands
        .entity(back)
        .observe(
            |mut click: On<PointerClick>,
             mut menu: ResMut<MenuState>,
             mut events: MessageWriter<MenuEvent>| {
                if click.button != PointerButton::Primary {
                    return;
                }
                click.propagate(false);
                if let Some(event) = menu.back() {
                    events.write(event);
                }
            },
        );

    widgets::text(commands, header, font, title.to_string(), 16.0, widgets::TEXT_COLOR);

    // 内容区：自动放子页面入口，应用可以继续往里挂控件；内容超出时纵向滚动
    let body = commands
        .spawn_scene(bsn! {
            Node {
                width: Val::Percent(100.0),
                flex_grow: 1.0,
                min_height: Val::Px(0.0),
                flex_direction: FlexDirection::Column,
                row_gap: Val::Px(10.0),
                padding: UiRect::all(Val::Px(14.0)),
                overflow: Overflow::scroll_y(),
            }
            MenuPageBody { page }
            ChildOf(page_entity)
        })
        .id();
    commands.entity(body).insert(ScrollPosition::DEFAULT);

    // 滚轮事件冒泡到内容区时，改 ScrollPosition；越界由布局系统自动夹紧
    commands.entity(body).observe(
        move |scroll: On<PointerScroll>,
              mut positions: Query<&mut ScrollPosition>,
              nodes: Query<&ComputedNode>| {
            let pixels = match scroll.unit {
                MouseScrollUnit::Line => scroll.y * SCROLL_LINE_PX,
                // 触控板/滚轮在 macOS 上给的是像素增量，直接当像素用
                MouseScrollUnit::Pixel => scroll.y,
            }
            .clamp(-MAX_SCROLL_STEP, MAX_SCROLL_STEP);
            let (Ok(mut position), Ok(computed)) = (positions.get_mut(body), nodes.get(body)) else {
                return;
            };
            // 可滚动距离 = 内容高度 - 可视高度（ComputedNode 是物理像素，换算回逻辑像素）
            let max = (computed.content_size().y - computed.size().y).max(0.0)
                * computed.inverse_scale_factor();
            position.0.y = (position.0.y - pixels).clamp(0.0, max);
        },
    );

    for child in config.pages.iter().filter(|def| def.parent == page) {
        spawn_menu_item(commands, body, child, font);
    }
}

/// 页面里的一行入口
fn spawn_menu_item(commands: &mut Commands, body: Entity, page: &MenuPageDef, font: Option<&Handle<Font>>) {
    let target = page.id;
    let item_title = page.title;
    let item_description = page.description;

    let row = commands
        .spawn_scene(bsn! {
            Node {
                width: Val::Percent(100.0),
                padding: UiRect::axes(Val::Px(12.0), Val::Px(10.0)),
                justify_content: JustifyContent::SpaceBetween,
                align_items: AlignItems::Center,
                border: UiRect::all(Val::Px(1.0)),
                border_radius: BorderRadius::all(Val::Px(8.0)),
            }
            BackgroundColor(ITEM_IDLE)
            BorderColor::all(BORDER_SOFT)
            ChildOf(body)
        })
        .observe(
            move |mut click: On<PointerClick>,
                  mut menu: ResMut<MenuState>,
                  mut events: MessageWriter<MenuEvent>| {
                if click.button != PointerButton::Primary {
                    return;
                }
                click.propagate(false);
                if let Some(event) = menu.push(target) {
                    events.write(event);
                }
            },
        )
        .id();

    let texts = commands
        .spawn_scene(bsn! {
            Node {
                flex_direction: FlexDirection::Column,
                row_gap: Val::Px(3.0),
            }
            ChildOf(row)
        })
        .id();

    widgets::text(commands, texts, font, item_title.to_string(), 14.0, widgets::TEXT_COLOR);

    widgets::text(
        commands,
        texts,
        font,
        item_description.to_string(),
        11.0,
        widgets::LABEL_COLOR,
    );

    widgets::text(commands, row, font, "›".to_string(), 16.0, widgets::LABEL_COLOR);
}

fn cleanup_menu(mut commands: Commands, roots: Query<Entity, With<MenuRoot>>) {
    for entity in &roots {
        commands.entity(entity).despawn();
    }
}

/// 快捷键：开合菜单，Esc 逐级返回
fn menu_hotkeys(
    keys: Res<ButtonInput<KeyCode>>,
    config: Res<MenuConfig>,
    mut menu: ResMut<MenuState>,
    mut events: MessageWriter<MenuEvent>,
) {
    if config.toggle_keys.iter().any(|key| keys.just_pressed(*key)) {
        if let Some(event) = menu.toggle() {
            events.write(event);
        }
    } else if keys.just_pressed(KeyCode::Escape) {
        if let Some(event) = menu.back() {
            events.write(event);
        }
    }
}

/// 推进抽屉与页面切换的动画进度
fn advance_menu(mut menu: ResMut<MenuState>, time: Res<Time>) {
    let delta = time.delta_secs();

    let target = if menu.open { 1.0 } else { 0.0 };
    let state = &mut *menu;
    step_drawer_spring(
        &mut state.drawer,
        &mut state.drawer_velocity,
        target,
        delta,
    );

    let finished = if let Some(transition) = menu.transition.as_mut() {
        transition.t = (transition.t + delta / SLIDE_SECONDS).min(1.0);
        transition.t >= 1.0
    } else {
        false
    };
    if finished {
        menu.transition = None;
    }
}

/// 临界阻尼弹簧：把抽屉进度拉向目标，起步渐快、临近目标自动减速，不会硬停也不会冲过头
fn step_drawer_spring(drawer: &mut f32, velocity: &mut f32, target: f32, delta: f32) {
    let mut remaining = delta.max(0.0);
    while remaining > f32::EPSILON {
        let step = remaining.min(DRAWER_MAX_STEP);
        // x'' = -2ωx' - ω²(x - target)
        let accel =
            -2.0 * DRAWER_OMEGA * *velocity - DRAWER_OMEGA * DRAWER_OMEGA * (*drawer - target);
        *velocity += accel * step;
        *drawer += *velocity * step;
        remaining -= step;
    }

    // 弹簧只会逼近目标，手动收口到精确值，便于据此切 Display
    if (*drawer - target).abs() <= DRAWER_REST_DISTANCE && velocity.abs() <= DRAWER_REST_SPEED {
        *drawer = target;
        *velocity = 0.0;
    }
}

fn ease_out_cubic(t: f32) -> f32 {
    let t = t.clamp(0.0, 1.0);
    1.0 - (1.0 - t).powi(3)
}

/// 把抽屉位置、每一页的横向偏移、图标的退场状态写回 UI
fn sync_menu_layout(
    menu: Res<MenuState>,
    mut panels: Query<&mut Node, (With<MenuPanelRoot>, Without<MenuPageRoot>, Without<MenuIcon>)>,
    mut pages: Query<
        (&MenuPageRoot, &mut Node, &mut GlobalZIndex),
        (Without<MenuPanelRoot>, Without<MenuIcon>),
    >,
    mut icons: Query<
        (&mut Node, &mut UiTransform),
        (With<MenuIcon>, Without<MenuPanelRoot>, Without<MenuPageRoot>),
    >,
) {
    let drawer = menu.drawer.clamp(0.0, 1.0);
    let drawer_shown = menu.open || drawer > 0.0;

    for mut node in &mut panels {
        let right = Val::Px(-DRAWER_WIDTH * (1.0 - drawer));
        if node.right != right {
            node.right = right;
        }
        let display = if drawer_shown {
            Display::Flex
        } else {
            Display::None
        };
        if node.display != display {
            node.display = display;
        }
    }

    // 图标与抽屉共用同一个阻尼进度：展开时一边缩小一边向右退，收起时复原
    for (mut node, mut transform) in &mut icons {
        let scale = (1.0 - drawer).clamp(0.0, 1.0);
        let next_scale = Vec2::splat(scale);
        if transform.scale != next_scale {
            transform.scale = next_scale;
        }
        transform.translation = Val2::px(12.0 * (1.0 - scale), 0.0);

        let display = if scale > 0.0 {
            Display::Flex
        } else {
            Display::None
        };
        if node.display != display {
            node.display = display;
        }
    }

    let active = menu.current();
    for (page, mut node, mut z) in &mut pages {
        let mut left = DRAWER_WIDTH;
        let mut shown = false;
        let mut z_index = 110;

        if let Some(transition) = menu.transition.as_ref() {
            let progress = ease_out_cubic(transition.t);
            if transition.forward {
                if page.0 == transition.from {
                    left = 0.0;
                    shown = true;
                    z_index = 118;
                } else if page.0 == transition.to {
                    left = DRAWER_WIDTH * (1.0 - progress);
                    shown = true;
                    z_index = 120;
                }
            } else if page.0 == transition.to {
                left = 0.0;
                shown = true;
                z_index = 120;
            } else if page.0 == transition.from {
                // 返回：当前页面向右滑到屏幕外
                left = DRAWER_WIDTH * progress;
                shown = true;
                z_index = 124;
            }
        } else if page.0 == active {
            left = 0.0;
            shown = true;
            z_index = 120;
        }

        let left_val = Val::Px(left);
        if node.left != left_val {
            node.left = left_val;
        }
        let display = if shown { Display::Flex } else { Display::None };
        if node.display != display {
            node.display = display;
        }
        if z.0 != z_index {
            z.0 = z_index;
        }
    }
}
