//! 菜单里的「调试」页面：物理可视化等开发期工具。
//!
//! 当前只有一个控件：物理调试开关，控制 Avian 的 debug gizmos（碰撞体线框、轴、AABB）是否绘制。
//! 默认关闭：不调用 `PhysicsGizmos` 里的绘制，游戏里看不到任何调试线；打开后才会画出。
//!
//! ```text
//! root
//! └─ 开关卡片    物理调试                        [关闭]
//! ```

use avian3d::prelude::*;
use bevy::picking::pointer::PointerButton;
use bevy::prelude::*;

use crate::ui::menu::{widgets, MenuConfig, MenuPageBody, MenuPageDef, MenuSet};
use crate::GameState;

/// 菜单页面 id
pub const DEBUG_PAGE: &str = "debug";

/// 物理调试开关按钮
#[derive(Component, Clone, Default)]
pub struct PhysicsDebugToggle;

/// 资源里存开关状态，控件只负责读它；同步系统把状态写回 gizmo 配置
#[derive(Resource, Debug, Default)]
pub struct PhysicsDebugSettings {
    /// `true` 时绘制物理调试信息（碰撞体线框 / 轴 / AABB），默认关闭
    pub enabled: bool,
}

/// 开关「关」时的底色
const TOGGLE_IDLE: Color = Color::srgba(0.30, 0.30, 0.30, 0.55);
/// 开关「开」时的底色
const TOGGLE_ON: Color = widgets::BUTTON_SELECTED;
/// 卡片底色（比面板稍亮一点，用来分隔区块）
const CARD_BACKGROUND: Color = Color::srgba(0.16, 0.18, 0.24, 0.55);

/// 调试页插件：注册菜单页面、填充控件、同步物理 gizmo 配置
pub struct DebugSettingsPlugin;

impl Plugin for DebugSettingsPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<PhysicsDebugSettings>()
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
                (sync_physics_debug, update_physics_debug_label).run_if(in_state(GameState::Game)),
            );
    }
}

/// 把「调试」注册成菜单里的一级入口
fn register_menu_page(mut config: ResMut<MenuConfig>) {
    if config.pages.iter().any(|page| page.id == DEBUG_PAGE) {
        return;
    }
    config.pages.push(MenuPageDef::section(
        DEBUG_PAGE,
        "调试",
        "物理可视化等开发期工具",
    ));
}

/// 把调试控件挂进菜单的「调试」页面内容区
fn fill_menu_page(
    mut commands: Commands,
    settings: Res<PhysicsDebugSettings>,
    bodies: Query<(Entity, &MenuPageBody)>,
) {
    let Some((body, _)) = bodies
        .iter()
        .find(|(_, body)| body.page == Some(DEBUG_PAGE))
    else {
        warn!("菜单里没有找到调试页面，跳过内容填充");
        return;
    };

    commands.spawn_scene(page_scene(body, &settings));
}

/// 整页结构：根 → 开关卡片
fn page_scene(body: Entity, settings: &PhysicsDebugSettings) -> impl Scene + use<> {
    let enabled = settings.enabled;
    bsn! {
        Node {
            width: Val::Percent(100.0),
            flex_direction: FlexDirection::Column,
            row_gap: Val::Px(10.0),
        }
        ChildOf(body)
        Children [
            @{toggle_card(enabled)}
        ]
    }
}

/// 开关卡片：左标签、右胶囊按钮，点一下切换「物理调试」
fn toggle_card(enabled: bool) -> impl Scene + use<> {
    let toggle_color = if enabled { TOGGLE_ON } else { TOGGLE_IDLE };
    let toggle_text = if enabled { "开启" } else { "关闭" };

    bsn! {
        Node {
            width: Val::Percent(100.0),
            flex_direction: FlexDirection::Row,
            justify_content: JustifyContent::SpaceBetween,
            align_items: AlignItems::Center,
            padding: UiRect::axes(Val::Px(12.0), Val::Px(12.0)),
            border: UiRect::all(Val::Px(1.0)),
            border_radius: BorderRadius::all(Val::Px(8.0)),
        }
        BackgroundColor(CARD_BACKGROUND)
        BorderColor::all(widgets::BORDER_SOFT)
        Children [
            @{widgets::text("物理调试", 14.0, widgets::TEXT_COLOR)}
            --
            // 按钮本身就是文字节点：`update_physics_debug_label` 直接改它的 Text 与底色
            Node {
                width: Val::Px(52.0),
                height: Val::Px(26.0),
                justify_content: JustifyContent::Center,
                align_items: AlignItems::Center,
                border_radius: BorderRadius::all(Val::Px(13.0)),
            }
            Text::new(toggle_text)
            TextFont { font_size: FontSize::Px(12.0) }
            TextColor(Color::WHITE)
            BackgroundColor(toggle_color)
            BorderColor::all(Color::srgba(1.0, 1.0, 1.0, 0.22))
            PhysicsDebugToggle
            // 点击即取反，状态存进资源，刷新交给同步系统
            on(|mut click: On<PointerClick>, mut settings: ResMut<PhysicsDebugSettings>| {
                if click.button != PointerButton::Primary {
                    return;
                }
                click.propagate(false);
                settings.enabled = !settings.enabled;
            })
        ]
    }
}

/// 把资源里的状态写回 gizmo 配置：`PhysicsGizmos` 的 `enabled` 控制所有物理调试信息是否绘制
fn sync_physics_debug(settings: Res<PhysicsDebugSettings>, mut store: ResMut<GizmoConfigStore>) {
    let (config, _) = store.config_mut::<PhysicsGizmos>();
    if config.enabled != settings.enabled {
        config.enabled = settings.enabled;
    }
}

/// 更新物理调试开关标签显示
fn update_physics_debug_label(
    settings: Res<PhysicsDebugSettings>,
    mut toggle_labels: Query<(&mut Text, &mut BackgroundColor), With<PhysicsDebugToggle>>,
) {
    let (new_text, color) = if settings.enabled {
        ("开启", widgets::BUTTON_SELECTED)
    } else {
        ("关闭", TOGGLE_IDLE)
    };
    for (mut text, mut bg_color) in &mut toggle_labels {
        if text.0 != new_text {
            text.0 = new_text.to_string();
        }
        if bg_color.0 != color {
            bg_color.0 = color;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use bevy::asset::AssetPlugin;
    use bevy::ecs::system::RunSystemOnce;
    use bevy::scene::ScenePlugin;

    fn build_test_page() -> (App, Entity) {
        let mut app = App::new();
        app.add_plugins((
            MinimalPlugins,
            AssetPlugin::default(),
            ScenePlugin::default(),
        ));
        app.insert_resource(PhysicsDebugSettings::default());
        // 页面结构由菜单插件生成，这里只挂一个 `MenuPageBody` 模拟填充入口
        let body = app
            .world_mut()
            .spawn(MenuPageBody {
                page: Some(DEBUG_PAGE),
            })
            .id();

        app.world_mut()
            .run_system_once(
                move |mut commands: Commands, settings: Res<PhysicsDebugSettings>| {
                    commands.spawn_scene(page_scene(body, &settings));
                },
            )
            .expect("页面构建系统跑不起来");

        // 场景是排队生成的，需要走一帧让 SceneSpawner 落地
        app.update();
        (app, body)
    }

    /// 页面必须是「根 → 开关卡片 → 卡片内的标签 + 按钮」的嵌套
    #[test]
    fn page_is_card_with_toggle() {
        let (app, body) = build_test_page();
        let world = app.world();

        let roots = children_of(world, body);
        assert_eq!(roots.len(), 1, "页面根应该只有一个");
        let root = roots[0];
        let cards = children_of(world, root);
        assert_eq!(cards.len(), 1, "应该是开关卡片");

        let card = cards[0];
        let card_children = children_of(world, card);
        assert_eq!(card_children.len(), 2, "开关卡片 = 标签 + 按钮");

        // 按钮本身：文字节点上挂着 `PhysicsDebugToggle`、Text 与 BackgroundColor
        let toggle = card_children[1];
        assert!(world.get::<PhysicsDebugToggle>(toggle).is_some());
        assert!(world.get::<Text>(toggle).is_some(), "开关按钮本身要有文字");
        assert!(world.get::<BackgroundColor>(toggle).is_some());

        // 默认状态是关闭：资源里 `enabled` 为 false， gizmo 配置也应该是关的
        let settings = app.world().resource::<PhysicsDebugSettings>();
        assert!(!settings.enabled, "物理调试默认应该是关闭");
    }

    /// 开关的状态必须真正写进 gizmo 配置，否则物理调试线不会消失/出现
    #[test]
    fn sync_physics_debug_writes_gizmo_config() {
        let mut app = App::new();
        app.add_plugins((MinimalPlugins, AssetPlugin::default()));
        // `PhysicsDebugPlugin` 注册了 `PhysicsGizmos` 配置组，和场景插件里一样
        app.add_plugins(PhysicsDebugPlugin);
        app.insert_resource(PhysicsDebugSettings { enabled: false });

        let store = &mut *app.world_mut().resource_mut::<GizmoConfigStore>();
        // 插件默认是开的：开关关掉时，同步系统会把它写回关
        assert!(
            store.config::<PhysicsGizmos>().0.enabled,
            "gizmo 配置默认是开的"
        );

        // 开关默认是关的：同步一次，让 gizmo 配置跟资源一致（关掉）
        app.world_mut()
            .run_system_once(sync_physics_debug)
            .expect("同步系统跑不起来");

        let store = &*app.world().resource::<GizmoConfigStore>();
        assert!(
            !store.config::<PhysicsGizmos>().0.enabled,
            "gizmo 配置应该是关的"
        );

        // 开关打开：资源状态写回 gizmo 配置
        {
            let mut settings = app.world_mut().resource_mut::<PhysicsDebugSettings>();
            settings.enabled = true;
        }
        app.world_mut()
            .run_system_once(sync_physics_debug)
            .expect("同步系统跑不起来");

        let store = &*app.world().resource::<GizmoConfigStore>();
        assert!(
            store.config::<PhysicsGizmos>().0.enabled,
            "gizmo 配置应该是开的"
        );

        // 开关关闭：再次写回，关掉 gizmo 配置
        {
            let mut settings = app.world_mut().resource_mut::<PhysicsDebugSettings>();
            settings.enabled = false;
        }
        app.world_mut()
            .run_system_once(sync_physics_debug)
            .expect("同步系统跑不起来");

        let store = &*app.world().resource::<GizmoConfigStore>();
        assert!(
            !store.config::<PhysicsGizmos>().0.enabled,
            "gizmo 配置应该是关的"
        );
    }

    fn children_of(world: &World, entity: Entity) -> Vec<Entity> {
        world
            .get::<Children>(entity)
            .map(|children| children.iter().collect())
            .unwrap_or_default()
    }
}
