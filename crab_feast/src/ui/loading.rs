use bevy::prelude::*;
use iyes_progress::ProgressTracker;

use crate::GameState;

pub struct LoadingUiPlugin;

#[derive(Resource)]
struct LoadingUI {
    root: Entity,
}

#[derive(Component)]
struct ProgressBarMarker;

impl Plugin for LoadingUiPlugin {
    fn build(&self, app: &mut App) {
        app.add_systems(OnEnter(GameState::AssetLoading), setup_loading_ui)
            .add_systems(OnExit(GameState::AssetLoading), cleanup_loading_ui)
            .add_systems(
                Update,
                update_progress_bar.run_if(in_state(GameState::AssetLoading)),
            );
    }
}

fn setup_loading_ui(mut commands: Commands) {
    let progress_bar = commands
        .spawn((
            Node {
                width: Val::Percent(0.0),
                height: Val::Percent(100.),
                ..default()
            },
            BackgroundColor(Color::srgb(0.2, 0.6, 1.0)),
            ProgressBarMarker,
        ))
        .id();

    let root = commands
        .spawn((
            Node {
                width: Val::Percent(100.0),
                height: Val::Percent(100.0),
                align_items: AlignItems::Center,
                justify_content: JustifyContent::Center,
                flex_direction: FlexDirection::Column,
                row_gap: Val::Px(20.0),
                ..default()
            },
            BackgroundColor(Color::srgba(0.0, 0.0, 0.0, 0.9)),
            GlobalZIndex(i32::MAX),
        ))
        .with_children(|parent| {
            parent.spawn((
                Text::new("Loading..."),
                TextFont {
                    font_size: 40.0,
                    ..default()
                },
                TextColor(Color::WHITE),
            ));
            parent
                .spawn((
                    Node {
                        width: Val::Px(300.0),
                        height: Val::Px(20.0),
                        border: UiRect::all(Val::Px(2.0)),
                        ..default()
                    },
                    BackgroundColor(Color::srgba(0.0, 0.0, 0.0, 0.0)),
                    BorderColor::all(Color::WHITE),
                ))
                .add_child(progress_bar);
        })
        .id();

    commands.insert_resource(LoadingUI { root });
}

fn cleanup_loading_ui(mut commands: Commands, loading_ui: Res<LoadingUI>) {
    commands.entity(loading_ui.root).despawn();
    commands.remove_resource::<LoadingUI>();
}

fn update_progress_bar(
    progress_tracker: Option<Res<ProgressTracker<GameState>>>,
    mut query: Query<&mut Node, With<ProgressBarMarker>>,
) {
    if let Some(tracker) = progress_tracker {
        let progress = tracker.get_global_progress();
        if progress.total > 0 {
            let percentage = (progress.done as f32 / progress.total as f32) * 100.0;
            for mut node in query.iter_mut() {
                node.width = Val::Percent(percentage);
            }
        }
    }
}