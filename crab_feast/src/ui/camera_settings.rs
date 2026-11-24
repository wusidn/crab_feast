//! 「相机设置」菜单页：选择相机跟随策略，并调整该策略的参数。
//!
//! 页面结构（字体族由菜单内容区继承，这里只写字号）：
//!
//! ```text
//! root
//! ├─ 卡片 跟随策略   [下拉框：刚性绑定 / 临界阻尼弹簧 / 地面锚定]
//! │      说明（随策略变，讲清这个策略的取舍）
//! ├─ 组   弹簧参数（仅弹簧策略显示）
//! │  ├─ 卡片 特征时间     0.20 s
//! │  │      轨道 ─────────●──────────
//! │  └─ 卡片 最大速度     12 m/s
//! ├─ 组   锚定参数（仅地面锚定显示）
//! │  ├─ 卡片 竖直特征时间   0.35 s
//! │  ├─ 卡片 死区           0.15 m
//! │  └─ 卡片 最大速度       20 m/s
//! ├─ 卡片 瞬移阈值       8 m
//! └─ 卡片 实时           滞后 0.00 m · 焦点速度 0.00 m/s
//! ```
//!
//! 交互是闭环的：控件改动写回 [`CameraFollowSettings`]，同步系统再把资源状态刷回界面。
//! 只显示当前策略的参数组——切到「刚性绑定」时弹簧与锚定两组一起隐藏，不留空壳。

use bevy::prelude::*;
use bevy::ui_widgets::{Slider, SliderValue, ValueChange, slider_self_update};

use crate::GameState;
use crate::camera::{
    ANCHOR_SPEED_RANGE, ANCHOR_TIME_RANGE, CameraFollowSettings, CameraFollowState,
    CameraFollowStrategy, DEAD_ZONE_RANGE, MAX_SPEED_RANGE, SMOOTH_TIME_RANGE, SNAP_DISTANCE_RANGE,
};
use crate::ui::menu::{MenuConfig, MenuPageBody, MenuPageDef, MenuSet, widgets};

/// 菜单页面 id
pub const CAMERA_SETTINGS_PAGE: &str = "camera_settings";

/// 跟随策略下拉框的标识（展开状态由 [`widgets::OpenDropdown`] 统一持有）
pub const CAMERA_STRATEGY_DROPDOWN: &str = "camera_settings/strategy";

/// 卡片底色（与「移动设置」页保持一致）
const CARD_BACKGROUND: Color = Color::srgba(0.16, 0.18, 0.24, 0.55);

/// 页面上的一个可调参数：既是滑杆的标记，也是读写资源时的字段选择
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
enum CameraParam {
    #[default]
    SpringTime,
    SpringSpeed,
    AnchorTime,
    AnchorDeadZone,
    AnchorSpeed,
    SnapDistance,
}

/// 只在这个策略生效时显示的节点（每个策略一组参数卡）
#[derive(Component, Clone, Copy, Default)]
struct StrategyCard {
    strategy: CameraFollowStrategy,
}

/// 页面上的滑杆根节点
#[derive(Component, Clone, Copy, Default)]
struct ParamSlider {
    param: CameraParam,
}

/// 滑杆右侧的实时数值
#[derive(Component, Clone, Copy, Default)]
struct ParamValueLabel {
    param: CameraParam,
}

/// 策略说明文字
#[derive(Component, Clone, Default)]
struct StrategyNote;

/// 实时读数（滞后 / 焦点速度）
#[derive(Component, Clone, Default)]
struct FollowStatus;

/// 相机设置插件
pub struct CameraSettingsPlugin;

impl Plugin for CameraSettingsPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<CameraFollowSettings>()
            .init_resource::<CameraFollowState>()
            .add_systems(
                OnEnter(GameState::Game),
                register_menu_page.before(MenuSet::Setup),
            )
            .add_systems(
                OnEnter(GameState::Game),
                fill_menu_page.after(MenuSet::Setup),
            )
            .add_systems(
                Update,
                (
                    apply_dropdown_pick,
                    sync_dropdown,
                    sync_strategy_visibility,
                    sync_sliders,
                    sync_value_labels,
                    update_status,
                )
                    .chain()
                    .run_if(in_state(GameState::Game)),
            )
            // 滑块与进度条的定位（核心 Slider 只维护值，不摆位置）
            .add_systems(
                Update,
                widgets::update_slider_styling.run_if(in_state(GameState::Game)),
            );
    }
}

/// 把「相机设置」注册成菜单里的一级入口
fn register_menu_page(mut config: ResMut<MenuConfig>) {
    if config
        .pages
        .iter()
        .any(|page| page.id == CAMERA_SETTINGS_PAGE)
    {
        return;
    }
    config.pages.push(MenuPageDef::section(
        CAMERA_SETTINGS_PAGE,
        "相机设置",
        "选择跟随策略并调整参数",
    ));
}

/// 把相机设置控件挂进菜单的「相机设置」页面内容区
fn fill_menu_page(
    mut commands: Commands,
    settings: Res<CameraFollowSettings>,
    bodies: Query<(Entity, &MenuPageBody)>,
) {
    let Some((body, _)) = bodies
        .iter()
        .find(|(_, body)| body.page == Some(CAMERA_SETTINGS_PAGE))
    else {
        warn!("菜单里没有找到相机设置页面，跳过内容填充");
        return;
    };

    commands.spawn_scene(page_scene(body, &settings));
}

/// 整页结构：策略卡 + 弹簧组 + 锚定组 + 瞬移阈值卡 + 实时读数卡
fn page_scene(body: Entity, settings: &CameraFollowSettings) -> impl Scene + use<> {
    bsn! {
        Node {
            width: Val::Percent(100.0),
            flex_direction: FlexDirection::Column,
            row_gap: Val::Px(10.0),
        }
        ChildOf(body)
        Children [
            @{strategy_card(settings)}
            --
            @{spring_group(settings)}
            --
            @{anchor_group(settings)}
            --
            @{snap_card(settings)}
            --
            @{status_card()}
        ]
    }
}

/// 策略卡片：标题 + 下拉框 + 一句取舍说明
fn strategy_card(settings: &CameraFollowSettings) -> impl Scene + use<> {
    let labels: Vec<String> = CameraFollowStrategy::ALL
        .iter()
        .map(|strategy| strategy.label().to_string())
        .collect();
    let selected = settings.strategy.index();
    let note = settings.strategy.note().to_string();

    bsn! {
        Node {
            width: Val::Percent(100.0),
            flex_direction: FlexDirection::Column,
            row_gap: Val::Px(8.0),
            padding: UiRect::axes(Val::Px(12.0), Val::Px(12.0)),
            border: UiRect::all(Val::Px(1.0)),
            border_radius: BorderRadius::all(Val::Px(8.0)),
        }
        BackgroundColor(CARD_BACKGROUND)
        BorderColor::all(widgets::BORDER_SOFT)
        Children [
            @{widgets::text("跟随策略", 14.0, widgets::TEXT_COLOR)}
            --
            @{widgets::dropdown(CAMERA_STRATEGY_DROPDOWN, &labels, selected)}
            --
            Text::new(note)
            TextFont { font_size: FontSize::Px(11.0) }
            TextColor(widgets::LABEL_COLOR)
            StrategyNote
        ]
    }
}

/// 弹簧参数组：整组只在「临界阻尼弹簧」生效时显示
fn spring_group(settings: &CameraFollowSettings) -> impl Scene + use<> {
    bsn! {
        Node {
            width: Val::Percent(100.0),
            flex_direction: FlexDirection::Column,
            row_gap: Val::Px(10.0),
        }
        StrategyCard { strategy: CameraFollowStrategy::SmoothDamp }
        Children [
            @{param_card(
                "特征时间",
                "追上目标所需的特征时间（秒）。0.05 跟手，0.5 明显飘。",
                CameraParam::SpringTime,
                settings,
                SMOOTH_TIME_RANGE.0,
                SMOOTH_TIME_RANGE.1,
                0.01,
            )}
            --
            @{param_card(
                "最大速度",
                "焦点最高移动速度（米/秒），0 = 不限。它是安全阀，不是常规手感旋钮。",
                CameraParam::SpringSpeed,
                settings,
                MAX_SPEED_RANGE.0,
                MAX_SPEED_RANGE.1,
                0.5,
            )}
        ]
    }
}

/// 地面锚定参数组：整组只在「地面锚定」生效时显示
fn anchor_group(settings: &CameraFollowSettings) -> impl Scene + use<> {
    bsn! {
        Node {
            width: Val::Percent(100.0),
            flex_direction: FlexDirection::Column,
            row_gap: Val::Px(10.0),
        }
        StrategyCard { strategy: CameraFollowStrategy::GroundAnchor }
        Children [
            @{param_card(
                "竖直特征时间",
                "上下坡、台阶的镜头抬升时间（秒）。水平方向永远瞬时，只调竖直。",
                CameraParam::AnchorTime,
                settings,
                ANCHOR_TIME_RANGE.0,
                ANCHOR_TIME_RANGE.1,
                0.01,
            )}
            --
            @{param_card(
                "死区",
                "地面高度变化小于它就不动画面（米），用来吃掉台阶与地面网格噪声。",
                CameraParam::AnchorDeadZone,
                settings,
                DEAD_ZONE_RANGE.0,
                DEAD_ZONE_RANGE.1,
                0.01,
            )}
            --
            @{param_card(
                "最大速度",
                "竖直方向最高移动速度（米/秒），0 = 不限。防止大落差时镜头猛甩。",
                CameraParam::AnchorSpeed,
                settings,
                ANCHOR_SPEED_RANGE.0,
                ANCHOR_SPEED_RANGE.1,
                0.5,
            )}
        ]
    }
}

/// 瞬移阈值卡片：所有策略共用，所以不挂 [`StrategyCard`]
fn snap_card(settings: &CameraFollowSettings) -> impl Scene + use<> {
    param_card(
        "瞬移阈值",
        "与焦点的单帧距离超过它就直接硬切（重生 / 切关 / 瞬移），单位米。",
        CameraParam::SnapDistance,
        settings,
        SNAP_DISTANCE_RANGE.0,
        SNAP_DISTANCE_RANGE.1,
        1.0,
    )
}

/// 一张参数卡：标题行（名称 + 当前值）+ 滑杆 + 一行说明
fn param_card(
    title: &str,
    hint: &str,
    param: CameraParam,
    settings: &CameraFollowSettings,
    min: f32,
    max: f32,
    step: f32,
) -> impl Scene + use<> {
    let value = param_value(settings, param);
    let value_text = param_text(settings, param);

    bsn! {
        Node {
            width: Val::Percent(100.0),
            flex_direction: FlexDirection::Column,
            align_items: AlignItems::Stretch,
            row_gap: Val::Px(8.0),
            padding: UiRect::axes(Val::Px(12.0), Val::Px(12.0)),
            border: UiRect::all(Val::Px(1.0)),
            border_radius: BorderRadius::all(Val::Px(8.0)),
        }
        BackgroundColor(CARD_BACKGROUND)
        BorderColor::all(widgets::BORDER_SOFT)
        Children [
            Node {
                width: Val::Percent(100.0),
                justify_content: JustifyContent::SpaceBetween,
                align_items: AlignItems::Center,
            }
            Children [
                @{widgets::text(title, 12.0, widgets::LABEL_COLOR)}
                --
                Text::new(value_text)
                TextFont { font_size: FontSize::Px(12.0) }
                TextColor(widgets::TEXT_COLOR)
                ParamValueLabel { param: param }
            ]
            --
            @{widgets::slider(value, min, max, step)}
            ParamSlider { param: param }
            on(slider_self_update)
            // 拖动写回资源：夹区间在 sanitized 里做，滑杆范围只是第一道闸
            on(move |change: On<ValueChange<f32>>, mut settings: ResMut<CameraFollowSettings>| {
                let mut next = *settings;
                set_param(&mut next, param, change.value);
                *settings = next.sanitized();
            })
            --
            @{widgets::text(hint, 11.0, widgets::LABEL_COLOR)}
        ]
    }
}

/// 实时读数卡片：滞后距离与焦点速度，调参时看这两个数就够了
fn status_card() -> impl Scene + use<> {
    bsn! {
        Node {
            width: Val::Percent(100.0),
            flex_direction: FlexDirection::Column,
            row_gap: Val::Px(4.0),
            padding: UiRect::axes(Val::Px(12.0), Val::Px(12.0)),
            border: UiRect::all(Val::Px(1.0)),
            border_radius: BorderRadius::all(Val::Px(8.0)),
        }
        BackgroundColor(CARD_BACKGROUND)
        BorderColor::all(widgets::BORDER_SOFT)
        Children [
            @{widgets::text("实时", 14.0, widgets::TEXT_COLOR)}
            --
            Text::new("")
            TextFont { font_size: FontSize::Px(11.0) }
            TextColor(Color::srgba(0.86, 0.86, 0.86, 0.72))
            FollowStatus
        ]
    }
}

/// 读一个参数当前值
fn param_value(settings: &CameraFollowSettings, param: CameraParam) -> f32 {
    match param {
        CameraParam::SpringTime => settings.spring.smooth_time,
        CameraParam::SpringSpeed => settings.spring.max_speed,
        CameraParam::AnchorTime => settings.anchor.smooth_time,
        CameraParam::AnchorDeadZone => settings.anchor.dead_zone,
        CameraParam::AnchorSpeed => settings.anchor.max_speed,
        CameraParam::SnapDistance => settings.snap_distance,
    }
}

/// 写一个参数
fn set_param(settings: &mut CameraFollowSettings, param: CameraParam, value: f32) {
    match param {
        CameraParam::SpringTime => settings.spring.smooth_time = value,
        CameraParam::SpringSpeed => settings.spring.max_speed = value,
        CameraParam::AnchorTime => settings.anchor.smooth_time = value,
        CameraParam::AnchorDeadZone => settings.anchor.dead_zone = value,
        CameraParam::AnchorSpeed => settings.anchor.max_speed = value,
        CameraParam::SnapDistance => settings.snap_distance = value,
    }
}

/// 参数值的显示文本（单位不统一，所以按参数分别格式化）
fn param_text(settings: &CameraFollowSettings, param: CameraParam) -> String {
    match param {
        CameraParam::SpringTime => format!("{:.2} s", settings.spring.smooth_time),
        CameraParam::SpringSpeed => speed_text(settings.spring.max_speed),
        CameraParam::AnchorTime => format!("{:.2} s", settings.anchor.smooth_time),
        CameraParam::AnchorDeadZone => format!("{:.2} m", settings.anchor.dead_zone),
        CameraParam::AnchorSpeed => speed_text(settings.anchor.max_speed),
        CameraParam::SnapDistance => format!("{:.0} m", settings.snap_distance),
    }
}

/// 速度类参数的统一显示：`0` 是「不限」而不是「零速」
fn speed_text(max_speed: f32) -> String {
    if max_speed <= 0.0 {
        "不限".to_string()
    } else {
        format!("{:.1} m/s", max_speed)
    }
}

/// 下拉框选中：换策略。资源里每个策略的参数各自保留，切回来手感不丢。
fn apply_dropdown_pick(
    mut picked: MessageReader<widgets::DropdownPicked>,
    mut settings: ResMut<CameraFollowSettings>,
) {
    for pick in picked.read() {
        if pick.id != CAMERA_STRATEGY_DROPDOWN {
            continue;
        }
        let next = CameraFollowStrategy::from_index(pick.index);
        if settings.strategy != next {
            settings.strategy = next;
        }
    }
}

/// 触发器里显示的当前策略、浮层选中项高亮
fn sync_dropdown(
    settings: Res<CameraFollowSettings>,
    mut labels: Query<(&widgets::DropdownLabel, &mut Text)>,
    mut items: Query<(&widgets::DropdownItem, &mut BackgroundColor)>,
) {
    if !settings.is_changed() {
        return;
    }

    let current = settings.strategy.label();
    for (label, mut text) in &mut labels {
        if label.0 != CAMERA_STRATEGY_DROPDOWN {
            continue;
        }
        if text.0 != current {
            text.0 = current.to_string();
        }
    }

    for (item, mut background) in &mut items {
        if item.id != CAMERA_STRATEGY_DROPDOWN {
            continue;
        }
        let color = if item.index == settings.strategy.index() {
            widgets::BUTTON_SELECTED
        } else {
            Color::NONE
        };
        if background.0 != color {
            background.0 = color;
        }
    }
}

/// 只显示当前策略的参数组：不用的那一组直接收起，不留空白卡
fn sync_strategy_visibility(
    settings: Res<CameraFollowSettings>,
    mut groups: Query<(&StrategyCard, &mut Node)>,
) {
    for (card, mut node) in &mut groups {
        let display = if card.strategy == settings.strategy {
            Display::Flex
        } else {
            Display::None
        };
        if node.display != display {
            node.display = display;
        }
    }
}

/// 资源被别处改过（读盘、换策略）时把滑杆拨回去，避免界面与资源不一致
fn sync_sliders(
    settings: Res<CameraFollowSettings>,
    sliders: Query<(Entity, &ParamSlider, &SliderValue), With<Slider>>,
    mut commands: Commands,
) {
    if !settings.is_changed() {
        return;
    }
    for (entity, marker, current) in &sliders {
        let desired = param_value(&settings, marker.param);
        if (current.0 - desired).abs() > 0.001 {
            commands.entity(entity).insert(SliderValue(desired));
        }
    }
}

/// 数值文本跟随资源
fn sync_value_labels(
    settings: Res<CameraFollowSettings>,
    mut labels: Query<(&ParamValueLabel, &mut Text)>,
) {
    if !settings.is_changed() {
        return;
    }
    for (marker, mut text) in &mut labels {
        let value = param_text(&settings, marker.param);
        if text.0 != value {
            text.0 = value;
        }
    }
}

/// 每帧刷新实时读数：滞后会一直变，所以这个系统不能只在 changed 时跑
fn update_status(
    settings: Res<CameraFollowSettings>,
    state: Res<CameraFollowState>,
    mut labels: Query<&mut Text, With<FollowStatus>>,
) {
    let line = match settings.strategy {
        CameraFollowStrategy::Rigid => {
            "刚性绑定：焦点零滞后，跳跃的竖直位移会整帧带进画面".to_string()
        }
        CameraFollowStrategy::SmoothDamp => format!(
            "滞后 {:.2} m · 焦点速度 {:.2} m/s · 特征时间 {:.2} s",
            state.lag(),
            state.speed(),
            settings.spring.smooth_time
        ),
        CameraFollowStrategy::GroundAnchor => format!(
            "竖直滞后 {:.2} m · 焦点速度 {:.2} m/s · 地面死区 {:.2} m（跳跃时应恒为 0）",
            state.lag(),
            state.speed(),
            settings.anchor.dead_zone
        ),
    };

    for mut text in &mut labels {
        if text.0 != line {
            text.0 = line.clone();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use bevy::asset::AssetPlugin;
    use bevy::ecs::system::RunSystemOnce;
    use bevy::scene::ScenePlugin;
    use bevy::ui_widgets::{SliderRange, SliderThumb};

    fn build_test_page() -> (App, Entity) {
        let mut app = App::new();
        app.add_plugins((
            MinimalPlugins,
            AssetPlugin::default(),
            ScenePlugin::default(),
        ));
        app.insert_resource(CameraFollowSettings::default());
        app.insert_resource(CameraFollowState::default());

        let body = app
            .world_mut()
            .spawn(MenuPageBody {
                page: Some(CAMERA_SETTINGS_PAGE),
            })
            .id();

        app.world_mut()
            .run_system_once(
                move |mut commands: Commands, settings: Res<CameraFollowSettings>| {
                    commands.spawn_scene(page_scene(body, &settings));
                },
            )
            .expect("页面构建系统跑不起来");
        app.update();
        (app, body)
    }

    fn children_of(world: &World, entity: Entity) -> Vec<Entity> {
        world
            .get::<Children>(entity)
            .map(|children| children.iter().collect())
            .unwrap_or_default()
    }

    fn descendants_of(world: &World, root: Entity) -> Vec<Entity> {
        let mut out = Vec::new();
        let mut stack = vec![root];
        while let Some(entity) = stack.pop() {
            for child in children_of(world, entity) {
                out.push(child);
                stack.push(child);
            }
        }
        out
    }

    /// 页面是「根 → 策略卡 / 弹簧组 / 锚定组 / 瞬移卡 / 实时卡」，参数组里再套参数卡
    #[test]
    fn page_is_four_cards_plus_two_strategy_groups() {
        let (app, body) = build_test_page();
        let world = app.world();

        let roots = children_of(world, body);
        assert_eq!(roots.len(), 1, "页面根应该只有一个");
        let root = roots[0];
        let children = children_of(world, root);
        assert_eq!(
            children.len(),
            5,
            "应该是策略卡 + 弹簧组 + 锚定组 + 瞬移卡 + 实时卡"
        );

        // 策略卡：标题、下拉框、说明
        let strategy_card = children[0];
        let parts = children_of(world, strategy_card);
        assert_eq!(parts.len(), 3, "策略卡 = 标题 + 下拉框 + 说明");
        assert!(world.get::<StrategyNote>(parts[2]).is_some());

        // 两组策略参数：各挂自己的策略标记，组里再套参数卡
        let expectations = [
            (
                children[1],
                CameraFollowStrategy::SmoothDamp,
                2,
                "弹簧参数组",
            ),
            (
                children[2],
                CameraFollowStrategy::GroundAnchor,
                3,
                "锚定参数组",
            ),
        ];
        for (group, strategy, card_count, name) in expectations {
            let marker = world
                .get::<StrategyCard>(group)
                .expect("参数组要有策略标记");
            assert_eq!(marker.strategy, strategy);
            let cards = children_of(world, group);
            assert_eq!(cards.len(), card_count, "{name} 的卡片数不对");
            for card in cards {
                let parts = children_of(world, card);
                assert_eq!(parts.len(), 3, "参数卡 = 标题行 + 滑杆 + 说明");
                assert!(
                    world.get::<Slider>(parts[1]).is_some(),
                    "参数卡中间必须是滑杆根节点"
                );
                assert!(world.get::<SliderRange>(parts[1]).is_some());
                assert!(world.get::<SliderValue>(parts[1]).is_some());
                assert!(
                    world.get::<ParamSlider>(parts[1]).is_some(),
                    "滑杆要有参数标记"
                );
                // 滑块与进度条必须在滑杆子树里，`update_slider_styling` 只遍历 root 的后代
                let slider_descendants = descendants_of(world, parts[1]);
                assert!(
                    slider_descendants
                        .iter()
                        .any(|entity| world.get::<SliderThumb>(*entity).is_some()),
                    "滑块必须是滑杆的后代"
                );
                assert!(
                    slider_descendants
                        .iter()
                        .any(|entity| world.get::<widgets::SliderFill>(*entity).is_some()),
                    "进度条必须是滑杆的后代"
                );
            }
        }

        // 瞬移卡挂在通用区，不跟策略走
        let snap = children[3];
        assert_eq!(children_of(world, snap).len(), 3);
        assert!(world.get::<StrategyCard>(snap).is_none());

        // 实时卡：标题 + 读数
        let status = children[4];
        let status_children = children_of(world, status);
        assert_eq!(status_children.len(), 2, "实时卡 = 标题 + 读数");
        assert!(world.get::<FollowStatus>(status_children[1]).is_some());
    }

    /// 每次只展开当前策略那一组：刚性绑定下两组都收起，锚定策略下只开锚定组
    #[test]
    fn strategy_groups_are_mutually_exclusive() {
        let (mut app, _body) = build_test_page();

        let expected: [(CameraFollowStrategy, [(CameraFollowStrategy, Display); 2]); 3] = [
            (
                CameraFollowStrategy::Rigid,
                [
                    (CameraFollowStrategy::SmoothDamp, Display::None),
                    (CameraFollowStrategy::GroundAnchor, Display::None),
                ],
            ),
            (
                CameraFollowStrategy::SmoothDamp,
                [
                    (CameraFollowStrategy::SmoothDamp, Display::Flex),
                    (CameraFollowStrategy::GroundAnchor, Display::None),
                ],
            ),
            (
                CameraFollowStrategy::GroundAnchor,
                [
                    (CameraFollowStrategy::SmoothDamp, Display::None),
                    (CameraFollowStrategy::GroundAnchor, Display::Flex),
                ],
            ),
        ];

        for (strategy, groups) in expected {
            app.world_mut()
                .resource_mut::<CameraFollowSettings>()
                .strategy = strategy;
            app.world_mut()
                .run_system_once(sync_strategy_visibility)
                .expect("可见性系统跑不起来");

            let world = app.world_mut();
            let mut cards = world.query::<(&StrategyCard, &Node)>();
            let mut found = Vec::new();
            for (card, node) in cards.iter(world) {
                found.push((card.strategy, node.display));
            }
            // 顺序按实体创建顺序：弹簧组在前，锚定组在后
            assert_eq!(
                found,
                groups.to_vec(),
                "{} 下的参数组可见性不对",
                strategy.label()
            );
        }
    }

    /// 拖滑杆写回资源要夹区间：滑杆上下限是第一道闸，`sanitized` 是第二道
    #[test]
    fn slider_write_clamps_to_range() {
        let mut settings = CameraFollowSettings::default();
        set_param(&mut settings, CameraParam::SpringTime, 5.0);
        let sanitized = settings.sanitized();
        assert_eq!(sanitized.spring.smooth_time, SMOOTH_TIME_RANGE.1);

        set_param(&mut settings, CameraParam::SpringSpeed, -3.0);
        let sanitized = settings.sanitized();
        assert_eq!(sanitized.spring.max_speed, MAX_SPEED_RANGE.0);

        // 锚定参数同样夹区间
        let mut anchor = CameraFollowSettings::default();
        set_param(&mut anchor, CameraParam::AnchorTime, 9.0);
        assert_eq!(anchor.sanitized().anchor.smooth_time, ANCHOR_TIME_RANGE.1);
        set_param(&mut anchor, CameraParam::AnchorDeadZone, -1.0);
        assert_eq!(anchor.sanitized().anchor.dead_zone, DEAD_ZONE_RANGE.0);
        set_param(&mut anchor, CameraParam::AnchorSpeed, 99.0);
        assert_eq!(anchor.sanitized().anchor.max_speed, ANCHOR_SPEED_RANGE.1);
    }
}
