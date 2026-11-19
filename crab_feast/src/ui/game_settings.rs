//! 「游戏设置」菜单页：退出当前对局，回到大厅。
//!
//! ```text
//! root
//! └─ 卡片  退出游戏
//!          联机时会断开服务器连接，本地游戏会清空当前场景
//!          [ 退出游戏（返回大厅） ]
//! ```
//!
//! 退出走的是状态机：`Game → Lobby`。场景/角色/输入 UI 由各插件的
//! `OnExit(Game)` 清理，联机连接由 `net::leave_game` 主动断开，
//! 因此这里只负责切状态，不直接碰实体。

use bevy::picking::pointer::PointerButton;
use bevy::prelude::*;

use crate::state::{GameState, LobbyNotice};
use crate::ui::menu::{MenuConfig, MenuPageBody, MenuPageDef, MenuSet, widgets};

/// 菜单页面 id
pub const GAME_SETTINGS_PAGE: &str = "game_settings";

/// 卡片底色（与「移动设置」页保持一致）
const CARD_BACKGROUND: Color = Color::srgba(0.16, 0.18, 0.24, 0.55);
/// 退出按钮底色：偏红，和普通设置项区分开
const DANGER_BACKGROUND: Color = Color::srgba(0.52, 0.16, 0.18, 0.92);

/// 请求退出当前对局（由按钮点击发出，`handle_exit` 统一处理）
#[derive(Message, Clone, Copy, Default)]
pub struct ExitToLobby;

/// 游戏设置插件
pub struct GameSettingsPlugin;

impl Plugin for GameSettingsPlugin {
    fn build(&self, app: &mut App) {
        app.add_systems(
            OnEnter(GameState::Game),
            register_menu_page.before(MenuSet::Setup),
        )
        .add_systems(
            OnEnter(GameState::Game),
            fill_menu_page.after(MenuSet::Setup),
        )
        .add_message::<ExitToLobby>()
        .add_systems(
            Update,
            handle_exit.run_if(in_state(GameState::Game)),
        );
    }
}

/// 把「游戏设置」注册成菜单里的一级入口
fn register_menu_page(mut config: ResMut<MenuConfig>) {
    if config
        .pages
        .iter()
        .any(|page| page.id == GAME_SETTINGS_PAGE)
    {
        return;
    }
    config.pages.push(MenuPageDef::section(
        GAME_SETTINGS_PAGE,
        "游戏设置",
        "退出当前对局",
    ));
}

/// 退出当前对局：切回大厅，清理与断连交给各自的 `OnExit(Game)`
fn handle_exit(
    mut requests: MessageReader<ExitToLobby>,
    mut next_state: ResMut<NextState<GameState>>,
    mut notice: ResMut<LobbyNotice>,
) {
    if requests.is_empty() {
        return;
    }
    requests.clear();
    notice.0 = Some("已退出当前对局".to_string());
    info!("游戏设置：退出当前对局，回到大厅");
    next_state.set(GameState::Lobby);
}

/// 把退出卡片挂进「游戏设置」页面的内容区
fn fill_menu_page(mut commands: Commands, bodies: Query<(Entity, &MenuPageBody)>) {
    let Some((body, _)) = bodies
        .iter()
        .find(|(_, body)| body.page == Some(GAME_SETTINGS_PAGE))
    else {
        warn!("菜单里没有找到游戏设置页面，跳过内容填充");
        return;
    };

    commands.spawn_scene(page_scene(body));
}

/// 整页结构：一张卡片，说明 + 退出按钮
fn page_scene(body: Entity) -> impl Scene + use<> {
    bsn! {
        Node {
            width: Val::Percent(100.0),
            flex_direction: FlexDirection::Column,
            row_gap: Val::Px(10.0),
        }
        ChildOf(body)
        Children [
            @{exit_card()}
        ]
    }
}

fn exit_card() -> impl Scene + use<> {
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
            @{widgets::text("退出游戏", 14.0, widgets::TEXT_COLOR)}
            --
            @{widgets::text(
                "联机时会断开与服务器的连接，本地游戏会清空当前场景。",
                12.0,
                widgets::LABEL_COLOR
            )}
            --
            // 整行按钮：点击后切回大厅，清理与断连交给各自插件的 OnExit(Game)
            Node {
                width: Val::Percent(100.0),
                height: Val::Px(36.0),
                justify_content: JustifyContent::Center,
                align_items: AlignItems::Center,
                border: UiRect::all(Val::Px(1.0)),
                border_radius: BorderRadius::all(Val::Px(6.0)),
            }
            BackgroundColor(DANGER_BACKGROUND)
            BorderColor::all(widgets::BORDER_SOFT)
            Children [
                @{widgets::text("退出游戏（返回大厅）", 13.0, Color::WHITE)}
            ]
            on(|mut click: On<PointerClick>, mut exit: MessageWriter<ExitToLobby>| {
                if click.button != PointerButton::Primary {
                    return;
                }
                click.propagate(false);
                exit.write(ExitToLobby);
            })
        ]
    }
}


#[cfg(test)]
mod tests {
    use super::*;
    use bevy::asset::AssetPlugin;
    use bevy::ecs::system::RunSystemOnce;
    use bevy::scene::ScenePlugin;

    #[test]
    fn registers_the_page_once() {
        let mut app = App::new();
        app.add_plugins(MinimalPlugins);
        app.init_resource::<MenuConfig>();
        app.world_mut()
            .run_system_once(register_menu_page)
            .expect("注册系统跑不起来");
        let config = app.world().resource::<MenuConfig>();
        assert!(
            config.pages.iter().any(|page| page.id == GAME_SETTINGS_PAGE),
            "游戏设置页应该出现在菜单里"
        );

        // 重进一次游戏不应该堆出第二个入口
        app.world_mut()
            .run_system_once(register_menu_page)
            .expect("注册系统跑不起来");
        let config = app.world().resource::<MenuConfig>();
        assert_eq!(
            config
                .pages
                .iter()
                .filter(|page| page.id == GAME_SETTINGS_PAGE)
                .count(),
            1
        );
    }

    #[test]
    fn exit_request_returns_to_lobby() {
        let mut app = App::new();
        app.add_plugins(MinimalPlugins);
        app.add_message::<ExitToLobby>();
        app.insert_resource(NextState::<GameState>::Unchanged);
        app.insert_resource(LobbyNotice(None));
        app.add_systems(Update, handle_exit);

        app.world_mut().write_message(ExitToLobby);
        app.update();

        assert!(
            matches!(
                app.world().resource::<NextState<GameState>>(),
                NextState::Pending(GameState::Lobby)
            ),
            "退出请求应该把状态切到大厅"
        );
        assert_eq!(
            app.world().resource::<LobbyNotice>().0.as_deref(),
            Some("已退出当前对局")
        );
    }

    #[test]
    fn page_contains_the_exit_button() {
        let mut app = App::new();
        app.add_plugins((
            MinimalPlugins,
            AssetPlugin::default(),
            ScenePlugin::default(),
        ));
        let body = app
            .world_mut()
            .spawn(MenuPageBody {
                page: Some(GAME_SETTINGS_PAGE),
            })
            .id();
        app.world_mut()
            .run_system_once(move |mut commands: Commands| {
                commands.spawn_scene(page_scene(body));
            })
            .expect("页面构建系统跑不起来");
        app.update();

        let world = app.world_mut();
        let mut texts = world.query::<&Text>();
        let labels: Vec<String> = texts.iter(world).map(|text| text.0.clone()).collect();
        assert!(
            labels.iter().any(|label| label == "退出游戏"),
            "卡片要有标题：{labels:?}"
        );
        assert!(
            labels.iter().any(|label| label == "退出游戏（返回大厅）"),
            "卡片要有退出按钮：{labels:?}"
        );
    }
}


