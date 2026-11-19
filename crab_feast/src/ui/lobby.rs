//! 大厅（服务器列表 + 本地游戏）与登录/准备遮罩。
//!
//! 启动后先停在这里：左侧/中间列出局域网里广播自己的服务器，点某一行会在该行
//! 右侧露出「加入」按钮；列表下方是「本地游戏」。两者都只是往
//! [`SessionMode`] 写目标并切到 `Login` —— 之后的建场景、建角色、开输入 UI
//! 走同一条路，不因为本地/联机分叉。

use std::net::SocketAddr;

use bevy::picking::pointer::PointerButton;
use bevy::prelude::*;

use crate::GameState;
use crate::state::{AbortSession, LobbyNotice, SessionMode, SessionReady};
use crate::ui::font::font_scope;

const BACKDROP: Color = Color::srgb(0.035, 0.045, 0.065);
const PANEL: Color = Color::srgba(0.09, 0.11, 0.16, 0.95);
const PANEL_HOVER: Color = Color::srgba(0.13, 0.16, 0.23, 0.98);
const ACCENT: Color = Color::srgb(0.22, 0.45, 0.86);
const BORDER: Color = Color::srgba(1.0, 1.0, 1.0, 0.10);
const TEXT: Color = Color::srgb(0.90, 0.93, 0.97);
const MUTED: Color = Color::srgb(0.60, 0.66, 0.76);
const WARN: Color = Color::srgb(0.98, 0.72, 0.36);

const FONT_PATH: &str = "fonts/PingFang-SC-Regular.ttf";

pub struct LobbyUiPlugin;

/// 当前被点中的服务器：只有它的「加入」按钮露出来。
#[derive(Resource, Default)]
struct LobbySelection(Option<SocketAddr>);

/// 列表里正在显示的服务器（用于刷新时先收掉旧行）。
#[derive(Resource, Default)]
struct LobbyRows(Vec<Entity>);

/// 上一次刷新的列表内容，和这次一样就不重建。
#[derive(Resource, Default)]
struct LobbySnapshot(Vec<RowData>);

#[derive(Clone, PartialEq)]
struct RowData {
    addr: SocketAddr,
    name: String,
    players: u8,
    max_players: u8,
}

#[derive(Component)]
struct LobbyRoot;

#[derive(Component)]
struct LobbyList;

#[derive(Component)]
struct LobbyHint;

#[derive(Component)]
struct LobbyNoticeText;

#[derive(Component)]
struct ServerRow(SocketAddr);

#[derive(Component)]
struct JoinButton(SocketAddr);

#[derive(Component)]
struct LocalGameButton;

#[derive(Component)]
struct LoginOverlay;

#[derive(Component)]
struct PreparingOverlay;

impl Plugin for LobbyUiPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<LobbySelection>()
            .init_resource::<LobbyRows>()
            .init_resource::<LobbySnapshot>()
            .add_systems(OnEnter(GameState::Lobby), setup_lobby)
            .add_systems(OnExit(GameState::Lobby), cleanup_lobby)
            .add_systems(OnEnter(GameState::Login), setup_login_overlay)
            .add_systems(OnExit(GameState::Login), cleanup_login_overlay)
            .add_systems(OnEnter(GameState::Game), setup_preparing_overlay)
            .add_systems(OnExit(GameState::Game), cleanup_preparing_overlay)
            .add_systems(
                Update,
                apply_selection.run_if(in_state(GameState::Lobby)),
            )
            .add_systems(
                Update,
                hide_preparing.run_if(in_state(GameState::Game)),
            );

        #[cfg(feature = "net")]
        app.add_systems(
            Update,
            sync_rows_from_discovery.run_if(in_state(GameState::Lobby)),
        );
    }
}

fn setup_lobby(
    mut commands: Commands,
    asset_server: Res<AssetServer>,
    mut notice: ResMut<LobbyNotice>,
    mut selection: ResMut<LobbySelection>,
    mut snapshot: ResMut<LobbySnapshot>,
    mut rows: ResMut<LobbyRows>,
) {
    selection.0 = None;
    snapshot.0.clear();
    rows.0.clear();

    let font = asset_server.load(FONT_PATH);
    let root = commands
        .spawn((
            LobbyRoot,
            Node {
                width: Val::Percent(100.0),
                height: Val::Percent(100.0),
                flex_direction: FlexDirection::Column,
                align_items: AlignItems::Center,
                justify_content: JustifyContent::Center,
                row_gap: Val::Px(14.0),
                padding: UiRect::all(Val::Px(28.0)),
                ..default()
            },
            BackgroundColor(BACKDROP),
            GlobalZIndex(1000),
            font_scope(&font),
        ))
        .id();

    commands.entity(root).with_children(|parent| {
        parent.spawn((
            Text::new("CRAB FEAST"),
            TextFont {
                font_size: FontSize::Px(46.0),
                ..default()
            },
            TextColor(TEXT),
        ));
        parent.spawn((
            Text::new("选择一台服务器，或直接开始本地游戏"),
            TextFont {
                font_size: FontSize::Px(14.0),
                ..default()
            },
            TextColor(MUTED),
        ));

        parent.spawn((
            LobbyNoticeText,
            Text::new(notice.0.clone().unwrap_or_default()),
            TextFont {
                font_size: FontSize::Px(13.0),
                ..default()
            },
            TextColor(WARN),
            Node {
                display: if notice.0.is_some() {
                    Display::Flex
                } else {
                    Display::None
                },
                ..default()
            },
        ));

        // 服务器列表
        parent.spawn((
            LobbyList,
            Node {
                // 默认收起：还没收到广播时只显示「正在搜索…」
                display: Display::None,
                width: Val::Percent(100.0),
                max_width: Val::Px(640.0),
                flex_direction: FlexDirection::Column,
                row_gap: Val::Px(8.0),
                padding: UiRect::all(Val::Px(10.0)),
                border: UiRect::all(Val::Px(1.0)),
                border_radius: BorderRadius::all(Val::Px(10.0)),
                ..default()
            },
            BackgroundColor(PANEL),
            BorderColor::all(BORDER),
        ));

        parent.spawn((
            LobbyHint,
            Text::new("正在搜索局域网服务器…"),
            TextFont {
                font_size: FontSize::Px(13.0),
                ..default()
            },
            TextColor(MUTED),
        ));

        // 本地游戏：和「加入」走同一条登录流程
        parent
            .spawn((
                LocalGameButton,
                Node {
                    width: Val::Percent(100.0),
                    max_width: Val::Px(640.0),
                    padding: UiRect::axes(Val::Px(16.0), Val::Px(14.0)),
                    justify_content: JustifyContent::Center,
                    align_items: AlignItems::Center,
                    border: UiRect::all(Val::Px(1.0)),
                    border_radius: BorderRadius::all(Val::Px(8.0)),
                    ..default()
                },
                BackgroundColor(PANEL),
                BorderColor::all(BORDER),
            ))
            .with_children(|button| {
                button.spawn((
                    Text::new("本地游戏（单机）"),
                    TextFont {
                        font_size: FontSize::Px(16.0),
                        ..default()
                    },
                    TextColor(TEXT),
                ));
            })
            .observe(
                |mut click: On<PointerClick>,
                 mut mode: ResMut<SessionMode>,
                 mut next: ResMut<NextState<GameState>>| {
                    if click.button != PointerButton::Primary {
                        return;
                    }
                    click.propagate(false);
                    info!("大厅：选择本地游戏");
                    *mode = SessionMode::Local;
                    next.set(GameState::Login);
                },
            );

        parent.spawn((
            Text::new("同一局域网内的服务器会自动出现在列表里"),
            TextFont {
                font_size: FontSize::Px(11.0),
                ..default()
            },
            TextColor(MUTED),
        ));
    });

    // 提示只显示这一次，下次回大厅不再翻旧账
    notice.0 = None;
}

fn cleanup_lobby(
    mut commands: Commands,
    roots: Query<Entity, With<LobbyRoot>>,
    mut rows: ResMut<LobbyRows>,
    mut selection: ResMut<LobbySelection>,
) {
    for entity in &roots {
        commands.entity(entity).try_despawn();
    }
    rows.0.clear();
    selection.0 = None;
}

/// 收到广播后刷新列表；内容没变就不动 UI（避免每帧重建、抢走点击）。
#[cfg(feature = "net")]
fn sync_rows_from_discovery(
    mut commands: Commands,
    browser: Res<crate::discovery::ServerBrowser>,
    // 两个查询都要写 Node：用 Without 交叉排除，让 Bevy 能证明它们不重叠
    mut list: Query<(Entity, &mut Node), (With<LobbyList>, Without<LobbyHint>)>,
    mut hint: Query<&mut Node, (With<LobbyHint>, Without<LobbyList>)>,
    mut snapshot: ResMut<LobbySnapshot>,
    mut rows: ResMut<LobbyRows>,
    selector: Res<LobbySelection>,
) {
    let Ok((list, mut list_node)) = list.single_mut() else {
        return;
    };
    let entries: Vec<RowData> = browser
        .snapshot()
        .into_iter()
        .map(|server| RowData {
            addr: server.addr,
            name: server.name,
            players: server.players,
            max_players: server.max_players,
        })
        .collect();

    if entries == snapshot.0 {
        return;
    }
    snapshot.0 = entries.clone();

    for entity in rows.0.drain(..) {
        commands.entity(entity).try_despawn();
    }

    for entry in &entries {
        let row = spawn_server_row(&mut commands, list, entry, selector.0 == Some(entry.addr));
        rows.0.push(row);
    }

    // 没有服务器时整块列表收起来，只留「正在搜索…」一行提示，
    // 而不是摆一个空的大方框
    let empty = entries.is_empty();
    list_node.display = if empty { Display::None } else { Display::Flex };
    if let Ok(mut node) = hint.single_mut() {
        node.display = if empty { Display::Flex } else { Display::None };
    }
}

/// 一行服务器：左边名字/地址，右边人数 + 点击后才出现的「加入」。
#[cfg(feature = "net")]
fn spawn_server_row(
    commands: &mut Commands,
    list: Entity,
    entry: &RowData,
    selected: bool,
) -> Entity {
    let addr = entry.addr;
    let row = commands
        .spawn((
            ServerRow(addr),
            Node {
                width: Val::Percent(100.0),
                padding: UiRect::axes(Val::Px(14.0), Val::Px(11.0)),
                justify_content: JustifyContent::SpaceBetween,
                align_items: AlignItems::Center,
                column_gap: Val::Px(12.0),
                border: UiRect::all(Val::Px(1.0)),
                border_radius: BorderRadius::all(Val::Px(8.0)),
                ..default()
            },
            BackgroundColor(if selected { PANEL_HOVER } else { PANEL }),
            BorderColor::all(if selected { ACCENT } else { BORDER }),
            ChildOf(list),
        ))
        .observe(
            move |mut click: On<PointerClick>,
                  mut selection: ResMut<LobbySelection>,
                  mut backgrounds: Query<(&ServerRow, &mut BackgroundColor)>| {
                if click.button != PointerButton::Primary {
                    return;
                }
                click.propagate(false);
                selection.0 = Some(addr);
                for (row, mut color) in &mut backgrounds {
                    *color = if row.0 == addr {
                        BackgroundColor(PANEL_HOVER)
                    } else {
                        BackgroundColor(PANEL)
                    };
                }
                info!("大厅：选中服务器 {addr}");
            },
        )
        .id();

    commands.entity(row).with_children(|row| {
        row.spawn(Node {
            flex_direction: FlexDirection::Column,
            row_gap: Val::Px(2.0),
            ..default()
        })
        .with_children(|left| {
            left.spawn((
                Text::new(entry.name.clone()),
                TextFont {
                    font_size: FontSize::Px(16.0),
                    ..default()
                },
                TextColor(TEXT),
            ));
            left.spawn((
                Text::new(addr.to_string()),
                TextFont {
                    font_size: FontSize::Px(11.0),
                    ..default()
                },
                TextColor(MUTED),
            ));
        });

        row.spawn(Node {
            align_items: AlignItems::Center,
            column_gap: Val::Px(10.0),
            ..default()
        })
        .with_children(|right| {
            right.spawn((
                Text::new(format!("{} / {}", entry.players, entry.max_players)),
                TextFont {
                    font_size: FontSize::Px(12.0),
                    ..default()
                },
                TextColor(if entry.players >= entry.max_players {
                    WARN
                } else {
                    MUTED
                }),
            ));

            right
                .spawn((
                    JoinButton(addr),
                    Node {
                        display: if selected {
                            Display::Flex
                        } else {
                            Display::None
                        },
                        padding: UiRect::axes(Val::Px(16.0), Val::Px(7.0)),
                        border_radius: BorderRadius::all(Val::Px(6.0)),
                        ..default()
                    },
                    BackgroundColor(ACCENT),
                ))
                .with_children(|button| {
                    button.spawn((
                        Text::new("加入"),
                        TextFont {
                            font_size: FontSize::Px(14.0),
                            ..default()
                        },
                        TextColor(Color::WHITE),
                    ));
                })
                .observe(
                    move |mut click: On<PointerClick>,
                          mut mode: ResMut<SessionMode>,
                          mut next: ResMut<NextState<GameState>>| {
                        if click.button != PointerButton::Primary {
                            return;
                        }
                        click.propagate(false);
                        info!("大厅：加入服务器 {addr}");
                        *mode = SessionMode::Online { server: addr };
                        next.set(GameState::Login);
                    },
                );
        });
    });

    row
}

/// 选择变化时露出/收起「加入」按钮（列表重建时也会调用一次）。
fn apply_selection(
    selection: Res<LobbySelection>,
    mut joins: Query<(&JoinButton, &mut Node)>,
    mut rows: Query<(&ServerRow, &mut BackgroundColor, &mut BorderColor)>,
) {
    if !selection.is_changed() {
        return;
    }
    for (button, mut node) in &mut joins {
        node.display = if selection.0 == Some(button.0) {
            Display::Flex
        } else {
            Display::None
        };
    }
    for (row, mut background, mut border) in &mut rows {
        let selected = selection.0 == Some(row.0);
        *background = BackgroundColor(if selected { PANEL_HOVER } else { PANEL });
        *border = BorderColor::all(if selected { ACCENT } else { BORDER });
    }
}

fn setup_login_overlay(mut commands: Commands, asset_server: Res<AssetServer>, mode: Res<SessionMode>) {
    let font = asset_server.load(FONT_PATH);
    let detail = match *mode {
        SessionMode::Local => "正在进入本地游戏…".to_string(),
        SessionMode::Online { server } => format!("正在连接 {server}…"),
    };

    commands
        .spawn((
            LoginOverlay,
            Node {
                width: Val::Percent(100.0),
                height: Val::Percent(100.0),
                flex_direction: FlexDirection::Column,
                align_items: AlignItems::Center,
                justify_content: JustifyContent::Center,
                row_gap: Val::Px(12.0),
                ..default()
            },
            BackgroundColor(BACKDROP),
            GlobalZIndex(1200),
            font_scope(&font),
        ))
        .with_children(|parent| {
            parent.spawn((
                Text::new("登录中"),
                TextFont {
                    font_size: FontSize::Px(30.0),
                    ..default()
                },
                TextColor(TEXT),
            ));
            parent.spawn((
                Text::new(detail),
                TextFont {
                    font_size: FontSize::Px(14.0),
                    ..default()
                },
                TextColor(MUTED),
            ));
            parent.spawn((
                Text::new("角色与场景就绪后才开放操作"),
                TextFont {
                    font_size: FontSize::Px(11.0),
                    ..default()
                },
                TextColor(MUTED),
            ));

            parent
                .spawn((
                    Node {
                        margin: UiRect::top(Val::Px(10.0)),
                        padding: UiRect::axes(Val::Px(22.0), Val::Px(8.0)),
                        border: UiRect::all(Val::Px(1.0)),
                        border_radius: BorderRadius::all(Val::Px(6.0)),
                        ..default()
                    },
                    BackgroundColor(PANEL),
                    BorderColor::all(BORDER),
                ))
                .with_children(|button| {
                    button.spawn((
                        Text::new("取消"),
                        TextFont {
                            font_size: FontSize::Px(13.0),
                            ..default()
                        },
                        TextColor(TEXT),
                    ));
                })
                .observe(
                    |mut click: On<PointerClick>,
                     mut abort: MessageWriter<AbortSession>,
                     mut next: ResMut<NextState<GameState>>| {
                        if click.button != PointerButton::Primary {
                            return;
                        }
                        click.propagate(false);
                        info!("登录：玩家取消");
                        abort.write(AbortSession);
                        next.set(GameState::Lobby);
                    },
                );
        });
}

fn cleanup_login_overlay(mut commands: Commands, roots: Query<Entity, With<LoginOverlay>>) {
    for entity in &roots {
        commands.entity(entity).try_despawn();
    }
}

fn setup_preparing_overlay(mut commands: Commands, asset_server: Res<AssetServer>) {
    let font = asset_server.load(FONT_PATH);
    commands
        .spawn((
            PreparingOverlay,
            Node {
                width: Val::Percent(100.0),
                height: Val::Percent(100.0),
                flex_direction: FlexDirection::Column,
                align_items: AlignItems::Center,
                justify_content: JustifyContent::Center,
                row_gap: Val::Px(10.0),
                ..default()
            },
            BackgroundColor(Color::srgba(0.03, 0.04, 0.06, 0.92)),
            GlobalZIndex(1100),
            font_scope(&font),
        ))
        .with_children(|parent| {
            parent.spawn((
                Text::new("正在准备场景…"),
                TextFont {
                    font_size: FontSize::Px(24.0),
                    ..default()
                },
                TextColor(TEXT),
            ));
            parent.spawn((
                Text::new("生成关卡、角色与相机"),
                TextFont {
                    font_size: FontSize::Px(12.0),
                    ..default()
                },
                TextColor(MUTED),
            ));
        });
}

/// 角色模型与动画图都挂上之后撤掉遮罩，同时输入 UI 才会被 `InputPlugin` 生成。
fn hide_preparing(mut commands: Commands, ready: Res<SessionReady>, roots: Query<Entity, With<PreparingOverlay>>) {
    if !ready.0 {
        return;
    }
    for entity in &roots {
        commands.entity(entity).try_despawn();
    }
}

fn cleanup_preparing_overlay(mut commands: Commands, roots: Query<Entity, With<PreparingOverlay>>) {
    for entity in &roots {
        commands.entity(entity).try_despawn();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn button(app: &mut App, addr: SocketAddr, display: Display) -> Entity {
        app.world_mut()
            .spawn((
                JoinButton(addr),
                Node {
                    display,
                    ..default()
                },
            ))
            .id()
    }

    #[test]
    fn selecting_a_server_reveals_only_its_join_button() {
        let mut app = App::new();
        app.init_resource::<LobbySelection>()
            .add_systems(Update, apply_selection);

        let first: SocketAddr = "127.0.0.1:5100".parse().unwrap();
        let second: SocketAddr = "127.0.0.1:5200".parse().unwrap();
        let first_button = button(&mut app, first, Display::None);
        let second_button = button(&mut app, second, Display::None);

        app.world_mut().resource_mut::<LobbySelection>().0 = Some(first);
        app.update();

        assert_eq!(
            app.world().entity(first_button).get::<Node>().unwrap().display,
            Display::Flex,
            "被选中的服务器应露出「加入」"
        );
        assert_eq!(
            app.world().entity(second_button).get::<Node>().unwrap().display,
            Display::None,
            "其它服务器保持收起"
        );

        app.world_mut().resource_mut::<LobbySelection>().0 = Some(second);
        app.update();

        assert_eq!(
            app.world().entity(first_button).get::<Node>().unwrap().display,
            Display::None
        );
        assert_eq!(
            app.world().entity(second_button).get::<Node>().unwrap().display,
            Display::Flex
        );
    }
}
