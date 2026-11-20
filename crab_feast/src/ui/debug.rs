//! 菜单里的「调试」页面：物理可视化等开发期工具。
//!
//! 当前控件：物理调试开关（Avian 的 debug gizmos，默认关闭）、世界时间速度滑杆。
//! 脚部 IK 开关已移到「移动设置」页。
//!
//! ```text
//! root
//! ├─ 开关卡片    物理调试                        [关闭]
//! └─ 滑杆卡片    世界时间速度                    ×1.00
//!                [────●────────────]
//! ```
//!
//! 世界时间速度以前是 `[` / `]` / `\` 三个快捷键，只改本机。现在改成滑杆，并且
//! **联网时由服务端权威决定**：拖动只是发一条请求，服务端改完把 `NetTimeScale`
//! 复制给所有客户端，所以同一局里任意一端拖动，所有端看到的世界速度一致
//! （见 `crate::net`，单机模式下没有上行、滑杆直接生效）。

use avian3d::prelude::*;
use bevy::picking::pointer::PointerButton;
use bevy::prelude::*;
use bevy::ui_widgets::{Slider, SliderValue, ValueChange, slider_self_update};
use crab_feast_library::world_time;

use crate::GameState;
use crate::ui::menu::{MenuConfig, MenuPageBody, MenuPageDef, MenuSet, widgets};

/// 菜单页面 id
pub const DEBUG_PAGE: &str = "debug";

/// 物理调试开关按钮
#[derive(Component, Clone, Default)]
pub struct PhysicsDebugToggle;

/// 世界时间速度滑杆（轨道上存的是 log2 倍数，见 [`slider_from_scale`]）
#[derive(Component, Clone, Default)]
pub struct TimeScaleSlider;

/// 世界时间速度的当前倍数文字（`×0.25`）
#[derive(Component, Clone, Default)]
pub struct TimeScaleLabel;

/// 世界时间速度：滑杆改它、[`apply_time_scale`] 把它写进 `Time<Virtual>`。
///
/// 只改 `Time<Virtual>` 的 relative speed：动画、物理、脚部 IK、复制下来的运动
/// 一起按同一比例缩，看到的才是"同一段动作放慢"，而不是"动画慢了但人还在飞"。
///
/// 联网时它只是**本地镜像**：拖动会先在这里生效（滑杆不能等网络往返才跟手），
/// 同时上行一条请求，服务端改完 `NetTimeScale` 再复制回来把这里校正成权威值。
#[derive(Resource, Debug)]
pub struct DebugTimeScale {
    pub speed: f32,
}

impl Default for DebugTimeScale {
    fn default() -> Self {
        Self { speed: 1.0 }
    }
}

/// 用户拖动了滑杆（只由滑杆写；服务器复制回来的值**不**发这条，否则会无限回声）。
///
/// 联网时 `crate::net` 读它往服务器发 [`crab_feast_library::net::TimeScaleRequest`]；
/// 单机时没有读者，消息直接过期丢弃。
#[derive(Message, Clone, Copy, Debug)]
pub struct TimeScaleChanged {
    /// 只被 `crate::net`（`net` feature）读；单机构建里没人消费，别报死代码
    #[cfg_attr(not(feature = "net"), allow(dead_code))]
    pub speed: f32,
}

/// 滑杆轨道下端：`log2(0.02)`。
///
/// 轨道走对数刻度而不是线性：慢放区间（0.02~0.2）在整根轨道上一共有 50 倍跨度，
/// 线性刻度下 0.02~0.1 只占 4% 的长度，想停到 ×0.05 根本停不住。
/// 取 -5.65 是为了让 0.0（正好 ×1.00）落在刻度上：`-5.65 + 113 * 0.05 = 0`。
const SLIDER_MIN: f32 = -5.65;
/// 滑杆轨道上端：`log2(2.0)`
const SLIDER_MAX: f32 = 1.0;
/// 滑杆刻度间隔（log2 单位，约等于每格 3.5% 的倍数变化）
const SLIDER_STEP: f32 = 0.05;

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
            .add_message::<TimeScaleChanged>()
            // 时间比例每帧对齐一次，而不是「只在拖动时写」：滑杆、服务器复制下来的值、
            // 单机与联网两条来源最后都汇进 `DebugTimeScale`，这里只认资源
            .add_systems(Update, apply_time_scale)
            // 退出对局把时间还回 1.0：否则从慢放 0.05 的房间退回大厅，菜单和下一局
            // 会继续按 1/20 的速度爬
            .add_systems(OnExit(GameState::Game), reset_time_scale)
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
                    update_time_scale_label,
                    sync_time_scale_slider,
                )
                    .run_if(in_state(GameState::Game)),
            );
    }
}

/// 把资源里的倍数写进 `Time<Virtual>`。
///
/// 注意：联网时这一条**不是**最终生效值。lightyear 的客户端时钟同步
/// （`LocalTimelineSyncPlugin::update_virtual_time`）每帧在 `Last` 里绝对改写
/// `Time<Virtual>` 的速度，所以真正的生效值由 `crate::net::apply_effective_time_scale`
/// 在 `First` 里按「同步基准 × 这里的倍数」重算（见那里的注释）。这里保留是为了
/// 单机构建（没有 `net` feature）也能直接生效。
///
/// 为什么用「跟当前值比一下」而不是 `Changed<DebugTimeScale>`：`set_relative_speed`
/// 自己不脏，但读完就写的系统会被 `Changed` 每帧触发一次；直接比对当前值既便宜，
/// 又顺带守住了「别的系统绕过资源直接改时间」的情况。
pub(crate) fn apply_time_scale(mut time: ResMut<Time<Virtual>>, mut scale: ResMut<DebugTimeScale>) {
    let speed = world_time::clamp_scale(scale.speed);
    // 越界的值顺手写回资源：NaN 永远 `!=` 自己，所以这一步也兜住了「脏值卡在资源里」
    if speed != scale.speed {
        scale.speed = speed;
    }
    if (time.relative_speed() - speed).abs() > 1e-4 {
        time.set_relative_speed(speed);
    }
}

/// 退出对局：时间速度复位到 1.0，下次进游戏滑杆也从 1.0 开始
fn reset_time_scale(mut scale: ResMut<DebugTimeScale>, mut time: ResMut<Time<Virtual>>) {
    if (scale.speed - 1.0).abs() > 1e-4 {
        scale.speed = 1.0;
        time.set_relative_speed(1.0);
    }
}

/// 滑杆 → 倍数：轨道上存 log2，换算成真实倍数
fn scale_from_slider(value: f32) -> f32 {
    world_time::clamp_scale(2.0f32.powf(value))
}

/// 倍数 → 滑杆位置：`1.0` 落在 0（轨道正中偏右），0.02 落在最左端
fn slider_from_scale(speed: f32) -> f32 {
    speed
        .max(f32::MIN_POSITIVE)
        .log2()
        .clamp(SLIDER_MIN, SLIDER_MAX)
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
    scale: Res<DebugTimeScale>,
    bodies: Query<(Entity, &MenuPageBody)>,
) {
    let Some((body, _)) = bodies
        .iter()
        .find(|(_, body)| body.page == Some(DEBUG_PAGE))
    else {
        warn!("菜单里没有找到调试页面，跳过内容填充");
        return;
    };

    commands.spawn_scene(page_scene(body, &settings, &scale));
}

/// 整页结构：根 → 物理调试卡片 → 世界时间速度卡片
fn page_scene(
    body: Entity,
    settings: &PhysicsDebugSettings,
    scale: &DebugTimeScale,
) -> impl Scene + use<> {
    let debug_enabled = settings.enabled;
    let speed = scale.speed;
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
            @{time_scale_card(speed)}
        ]
    }
}

/// 世界时间速度卡片：标题行（名称 + 当前倍数）+ 滑杆 + 一行说明
fn time_scale_card(speed: f32) -> impl Scene + use<> {
    let value_text = format!("×{speed:.2}");
    bsn! {
        Node {
            width: Val::Percent(100.0),
            flex_direction: FlexDirection::Column,
            align_items: AlignItems::Stretch,
            row_gap: Val::Px(10.0),
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
                @{widgets::text("世界时间速度", 14.0, widgets::TEXT_COLOR)}
                --
                Text::new(value_text)
                TextFont { font_size: FontSize::Px(12.0) }
                TextColor(widgets::TEXT_COLOR)
                TimeScaleLabel
            ]
            --
            // 滑杆：轨道存 log2 倍数，滑块与进度条的定位由 widgets::update_slider_styling 统一处理
            @{widgets::slider(
                slider_from_scale(speed),
                SLIDER_MIN,
                SLIDER_MAX,
                SLIDER_STEP,
            )}
            TimeScaleSlider
            on(slider_self_update)
            // 拖动只写资源 + 发一条「用户改了」：联网时由 `crate::net` 上行成请求，
            // 单机时没有读者，直接就是最终值
            on(|change: On<ValueChange<f32>>,
                 mut scale: ResMut<DebugTimeScale>,
                 mut changed: MessageWriter<TimeScaleChanged>| {
                let speed = scale_from_slider(change.value);
                if (scale.speed - speed).abs() < 1e-3 {
                    return;
                }
                scale.speed = speed;
                changed.write(TimeScaleChanged { speed });
            })
            --
            @{widgets::text("拖动后同一房间的所有端一起变（速度由服务端决定）", 11.0, widgets::LABEL_COLOR)}
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

/// 更新世界时间速度的倍数显示（`×0.25`）
fn update_time_scale_label(
    scale: Res<DebugTimeScale>,
    mut labels: Query<&mut Text, With<TimeScaleLabel>>,
) {
    let value = format!("×{:.2}", scale.speed);
    for mut text in &mut labels {
        if text.0 != value {
            text.0 = value.clone();
        }
    }
}

/// 把资源里的倍数写回滑杆位置。
///
/// 只在差异明显时才写：拖动时 `ValueChange` 已经把资源改成和滑杆一致，这里不会打架；
/// 真正需要它的是「服务端复制下来的值」和「刚进游戏时资源不是 1.0」两种情况——
/// 没有这个回写，滑杆会停在旧位置而世界已经不是那个速度了。
///
/// 比较的是「轨道值换算回来的倍数」而不是轨道值本身：轨道两端（-5.65 / 0.02）换算会
/// 被夹一下，直接比轨道值会让滑块在端点每帧回弹一次。
fn sync_time_scale_slider(
    scale: Res<DebugTimeScale>,
    sliders: Query<(Entity, &SliderValue), (With<TimeScaleSlider>, With<Slider>)>,
    mut commands: Commands,
) {
    for (entity, current) in &sliders {
        if (scale_from_slider(current.0) - scale.speed).abs() > 1e-3 {
            commands
                .entity(entity)
                .insert(SliderValue(slider_from_scale(scale.speed)));
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
        app.insert_resource(DebugTimeScale::default());
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
                      scale: Res<DebugTimeScale>| {
                    commands.spawn_scene(page_scene(body, &settings, &scale));
                },
            )
            .expect("页面构建系统跑不起来");

        // 场景是排队生成的，需要走一帧让 SceneSpawner 落地
        app.update();
        (app, body)
    }

    /// 页面是「根 → 开关卡片 + 时间速度卡片」，两张卡片各自保持自己的内部结构
    #[test]
    fn page_is_card_with_toggle() {
        let (app, body) = build_test_page();
        let world = app.world();

        let roots = children_of(world, body);
        assert_eq!(roots.len(), 1, "页面根应该只有一个");
        let root = roots[0];
        let cards = children_of(world, root);
        assert_eq!(cards.len(), 2, "页面应该有物理调试 + 世界时间速度两张卡片");

        let card = cards[0];
        let card_children = children_of(world, card);
        assert_eq!(card_children.len(), 2, "开关卡片 = 标签 + 按钮");

        // 按钮本身：文字节点上挂着 `PhysicsDebugToggle`、Text 与 BackgroundColor
        let toggle = card_children[1];
        assert!(world.get::<PhysicsDebugToggle>(toggle).is_some());
        assert!(world.get::<Text>(toggle).is_some(), "开关按钮本身要有文字");
        assert!(world.get::<BackgroundColor>(toggle).is_some());

        // 默认状态：物理调试关
        let settings = app.world().resource::<PhysicsDebugSettings>();
        assert!(!settings.enabled, "物理调试默认应该是关闭");
    }

    /// 时间速度卡片：标题行 + 滑杆 + 说明；滑杆上必须带业务标记，回写系统才找得到它
    #[test]
    fn time_scale_card_has_slider() {
        let (app, body) = build_test_page();
        let world = app.world();
        let root = children_of(world, body)[0];
        let card = children_of(world, root)[1];
        let kids = children_of(world, card);
        assert_eq!(kids.len(), 3, "时间卡片 = 标题行 + 滑杆 + 说明");

        let slider = kids[1];
        assert!(world.get::<TimeScaleSlider>(slider).is_some());
        assert!(world.get::<Slider>(slider).is_some(), "滑杆根要有 Slider");
        assert!(world.get::<SliderValue>(slider).is_some());

        // 默认 ×1.00：轨道值是 log2，正好落在 0
        let value = world.get::<SliderValue>(slider).unwrap().0;
        assert!(value.abs() < 1e-4, "×1.00 应对应轨道值 0，实际 {value}");
    }

    /// 轨道值 ↔ 倍数必须互为反函数，两端（0.02 / 2.0）都要落得进去
    #[test]
    fn slider_scale_round_trip() {
        for speed in [0.02, 0.05, 0.1, 0.25, 0.5, 1.0, 2.0] {
            let track = slider_from_scale(speed);
            let back = scale_from_slider(track);
            assert!(
                (back - speed).abs() < 1e-3,
                "×{speed} 过一遍滑杆变成了 ×{back}"
            );
        }
        // 拖到底 / 拖到顶不会跑出协议允许的区间
        assert!((scale_from_slider(SLIDER_MIN) - world_time::MIN_SCALE).abs() < 1e-3);
        assert!((scale_from_slider(SLIDER_MAX) - world_time::MAX_SCALE).abs() < 1e-3);
    }

    /// 资源里的倍数必须真的写进 `Time<Virtual>`：动画/物理/IK 都读它
    #[test]
    fn apply_time_scale_writes_virtual_time() {
        let mut app = App::new();
        app.add_plugins(MinimalPlugins);
        app.insert_resource(DebugTimeScale { speed: 0.25 });
        app.world_mut()
            .run_system_once(apply_time_scale)
            .expect("时间比例系统跑不起来");
        let speed = app.world().resource::<Time<Virtual>>().relative_speed();
        assert!(
            (speed - 0.25).abs() < 1e-4,
            "虚拟时间应被写成 0.25，实际 {speed}"
        );

        // 越界的值也要被夹回来，而不是把时间系统写成 0/NaN
        app.world_mut()
            .insert_resource(DebugTimeScale { speed: f32::NAN });
        app.world_mut()
            .run_system_once(apply_time_scale)
            .expect("时间比例系统跑不起来");
        let speed = app.world().resource::<Time<Virtual>>().relative_speed();
        assert!((speed - 1.0).abs() < 1e-4, "NaN 应回落到 1.0，实际 {speed}");
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
