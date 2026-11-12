//! 菜单里的「调试」页面：物理可视化、脚部 IK 等开发期工具。
//!
//! 当前两个控件：物理调试开关（Avian 的 debug gizmos）与脚部 IK 开关。
//! 物理调试默认关闭；脚部 IK 默认开启，关掉后可以直接对比转身时脚有没有滑。
//!
//! ```text
//! root
//! ├─ 开关卡片    物理调试                        [关闭]
//! └─ 开关卡片    脚部 IK                         [开启]
//! ```

use avian3d::prelude::*;
use bevy::picking::pointer::PointerButton;
use bevy::prelude::*;

use crate::anim::{FootIk, FootIkSettings};
use crate::ui::menu::{widgets, MenuConfig, MenuPageBody, MenuPageDef, MenuSet};
use crate::GameState;

/// 菜单页面 id
pub const DEBUG_PAGE: &str = "debug";

/// 物理调试开关按钮
#[derive(Component, Clone, Default)]
pub struct PhysicsDebugToggle;

/// 脚部 IK 开关按钮
#[derive(Component, Clone, Default)]
pub struct FootIkToggle;

/// 脚部 IK 实时读数（权重 / 修正量）
#[derive(Component, Clone, Default)]
pub struct FootIkStatus;

/// 调试用的时间速度（慢放看单帧）。只改 `Time<Virtual>` 的 relative speed，
/// 动画、物理、脚部 IK 的 dt 一起缩，看到的就是同一段动作放慢
#[derive(Resource, Debug)]
pub struct DebugTimeScale {
    pub speed: f32,
}

impl Default for DebugTimeScale {
    fn default() -> Self {
        Self { speed: 1.0 }
    }
}

/// 慢放的档位：`[` 退一档、`]` 进一档、`\` 回到 1.0
const TIME_STEPS: [f32; 9] = [0.02, 0.05, 0.1, 0.2, 0.3, 0.5, 0.75, 1.0, 2.0];

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
            .init_resource::<DebugTimeScale>()
            .add_systems(Update, debug_time_speed)
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
                    sync_physics_debug,
                    update_physics_debug_label,
                    update_foot_ik_label,
                    update_foot_ik_status,
                )
                    .run_if(in_state(GameState::Game)),
            );
    }
}

/// 慢放/快放：`[` 退一档、`]` 进一档、`\` 复位到 1.0。
///
/// 走 `Time<Virtual>` 而不是只放大脚部 IK 的 dt：这样动画采样、物理和 IK 用的是
/// 同一个时间比例，慢放看到的仍是同一段动作，不会因为「动画没慢、IK 慢了」错位。
fn debug_time_speed(
    keys: Res<ButtonInput<KeyCode>>,
    mut time: ResMut<Time<Virtual>>,
    mut scale: ResMut<DebugTimeScale>,
) {
    let current = TIME_STEPS
        .iter()
        .position(|step| (*step - scale.speed).abs() < 1e-4)
        .unwrap_or(7);
    let next = if keys.just_pressed(KeyCode::BracketLeft) {
        current.saturating_sub(1)
    } else if keys.just_pressed(KeyCode::BracketRight) {
        (current + 1).min(TIME_STEPS.len() - 1)
    } else if keys.just_pressed(KeyCode::Backslash) {
        7
    } else {
        return;
    };
    scale.speed = TIME_STEPS[next];
    time.set_relative_speed(scale.speed);
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
    foot_ik: Res<FootIkSettings>,
    bodies: Query<(Entity, &MenuPageBody)>,
) {
    let Some((body, _)) = bodies
        .iter()
        .find(|(_, body)| body.page == Some(DEBUG_PAGE))
    else {
        warn!("菜单里没有找到调试页面，跳过内容填充");
        return;
    };

    commands.spawn_scene(page_scene(body, &settings, &foot_ik));
}

/// 整页结构：根 → 物理调试卡片 + 脚部 IK 卡片
fn page_scene(
    body: Entity,
    settings: &PhysicsDebugSettings,
    foot_ik: &FootIkSettings,
) -> impl Scene + use<> {
    let debug_enabled = settings.enabled;
    let foot_ik_enabled = foot_ik.enabled;
    bsn! {
        Node {
            width: Val::Percent(100.0),
            flex_direction: FlexDirection::Column,
            row_gap: Val::Px(10.0),
        }
        ChildOf(body)
        Children [
            @{toggle_card(debug_enabled)}
            --
            @{foot_ik_card(foot_ik_enabled)}
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

/// 脚部 IK 卡片：点一下切换全局开关。运行时只是改资源里的布尔值，
/// 淡出由 `FootIkSettings::blend_secs` 负责，所以可以反复快速切换。
/// 下面一行是实时读数：混合权重 + 两只脚这一帧被拉动的距离，用来判断锁脚有没有在干活。
fn foot_ik_card(enabled: bool) -> impl Scene + use<> {
    let toggle_color = if enabled { TOGGLE_ON } else { TOGGLE_IDLE };
    let toggle_text = if enabled { "开启" } else { "关闭" };

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
            Node {
                width: Val::Percent(100.0),
                flex_direction: FlexDirection::Row,
                justify_content: JustifyContent::SpaceBetween,
                align_items: AlignItems::Center,
            }
            Children [
                @{widgets::text("脚部 IK（锁脚防滑步）", 14.0, widgets::TEXT_COLOR)}
                --
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
                FootIkToggle
                on(|mut click: On<PointerClick>, mut settings: ResMut<FootIkSettings>| {
                    if click.button != PointerButton::Primary {
                        return;
                    }
                    click.propagate(false);
                    settings.enabled = !settings.enabled;
                })
            ]
            --
            Text::new("")
            TextFont { font_size: FontSize::Px(11.0) }
            TextColor(Color::srgba(0.86, 0.86, 0.86, 0.72))
            FootIkStatus
        ]
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

/// 更新脚部 IK 实时读数：权重、支撑、两只脚这一帧的修正距离
fn update_foot_ik_status(
    settings: Res<FootIkSettings>,
    scale: Res<DebugTimeScale>,
    ik: Query<&FootIk>,
    mut labels: Query<&mut Text, With<FootIkStatus>>,
) {
    let line = if !settings.enabled {
        "已关闭".to_string()
    } else if let Some(ik) = ik.iter().next() {
        if !ik.is_ready() {
            "等待角色模型…".to_string()
        } else {
            let [left, right] = ik.correction();
            let [contact_left, contact_right] = ik.contacts;
            let [planted_left, planted_right] = ik.planted();
            let [roll_left, roll_right] = ik.roll();
            let [heel_left, heel_right] = ik.heel_off();
            let [ankle_left, ankle_right] = ik.ankle_deg();
            // 前掌骨折角：抬跟时脚趾要留在地上，这个数应该跟着抬跟一起长
            let [toe_left, toe_right] = ik.toe_bend_deg();
            let [strength_left, strength_right] = ik.strength();
            // 承重点：踩实后期承重点滑到前掌（「掌」），前期钉踝（「踝」）。
            // 踝角 = 脚掌与小腿的夹角，90 = 站立、蹬地时该到 130~160
            let pivot = |planted: bool, roll: f32| {
                if !planted {
                    "-"
                } else if roll > 0.5 {
                    "掌"
                } else {
                    "踝"
                }
            };
            // 射线量到的地面：没打中显示「-」，打中显示高度和坡度
            // （坡度 = 命中法线偏离竖直的角度，爬楼梯/斜坡时看它有没有跳）
            let ground = |index: usize| match ik.ground(index) {
                Some(hit) => format!(
                    "{:.2}/{:.0}°",
                    hit.height,
                    hit.normal.dot(Vec3::Y).clamp(-1.0, 1.0).acos().to_degrees()
                ),
                None => "-".to_string(),
            };
            format!(
                "权重 {:.2} · 支撑 {:.2}/{:.2} · 强度 {:.2}/{:.2} · 承重 {}/{} · 抬跟 {:.0}%/{:.0}% · 踝角 {:.0}°/{:.0}° · 趾 {:.0}°/{:.0}° · 修正 {:.3}/{:.3} m · 地面 {}/{} · 时间 ×{:.2}",
                ik.blend(),
                contact_left,
                contact_right,
                strength_left,
                strength_right,
                pivot(planted_left, roll_left),
                pivot(planted_right, roll_right),
                heel_left * 100.0,
                heel_right * 100.0,
                ankle_left,
                ankle_right,
                toe_left,
                toe_right,
                left,
                right,
                ground(0),
                ground(1),
                scale.speed
            )
        }
    } else {
        "当前没有角色".to_string()
    };

    for mut text in &mut labels {
        if text.0 != line {
            text.0 = line.clone();
        }
    }
}

/// 更新脚部 IK 开关标签显示
fn update_foot_ik_label(
    settings: Res<FootIkSettings>,
    mut toggle_labels: Query<(&mut Text, &mut BackgroundColor), With<FootIkToggle>>,
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
        app.insert_resource(FootIkSettings::default());
        // 页面结构由菜单插件生成，这里只挂一个 `MenuPageBody` 模拟填充入口
        let body = app
            .world_mut()
            .spawn(MenuPageBody {
                page: Some(DEBUG_PAGE),
            })
            .id();

        app.world_mut()
            .run_system_once(
                move |mut commands: Commands,
                      settings: Res<PhysicsDebugSettings>,
                      foot_ik: Res<FootIkSettings>| {
                    commands.spawn_scene(page_scene(body, &settings, &foot_ik));
                },
            )
            .expect("页面构建系统跑不起来");

        // 场景是排队生成的，需要走一帧让 SceneSpawner 落地
        app.update();
        (app, body)
    }

    /// 页面必须是「根 → 开关卡片 → 卡片内的标签 + 按钮」的嵌套，两张卡片各带一个开关
    #[test]
    fn page_is_card_with_toggle() {
        let (app, body) = build_test_page();
        let world = app.world();

        let roots = children_of(world, body);
        assert_eq!(roots.len(), 1, "页面根应该只有一个");
        let root = roots[0];
        let cards = children_of(world, root);
        assert_eq!(cards.len(), 2, "应该有两张开关卡片");

        let card = cards[0];
        let card_children = children_of(world, card);
        assert_eq!(card_children.len(), 2, "开关卡片 = 标签 + 按钮");

        // 按钮本身：文字节点上挂着 `PhysicsDebugToggle`、Text 与 BackgroundColor
        let toggle = card_children[1];
        assert!(world.get::<PhysicsDebugToggle>(toggle).is_some());
        assert!(world.get::<Text>(toggle).is_some(), "开关按钮本身要有文字");
        assert!(world.get::<BackgroundColor>(toggle).is_some());

        // 第二张卡片是脚部 IK：第一行同样是「标签 + 按钮」，下面一行是实时读数
        let ik_card = cards[1];
        let ik_children = children_of(world, ik_card);
        assert_eq!(ik_children.len(), 2, "脚部 IK 卡片 = 开关行 + 读数行");
        let ik_row = ik_children[0];
        let ik_row_children = children_of(world, ik_row);
        assert_eq!(ik_row_children.len(), 2, "开关行 = 标签 + 按钮");
        assert!(world.get::<FootIkToggle>(ik_row_children[1]).is_some());
        assert!(world.get::<FootIkStatus>(ik_children[1]).is_some());

        // 默认状态：物理调试关、脚部 IK 开
        let settings = app.world().resource::<PhysicsDebugSettings>();
        assert!(!settings.enabled, "物理调试默认应该是关闭");
        assert!(
            app.world().resource::<FootIkSettings>().enabled,
            "脚部 IK 默认应该是开启"
        );
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
