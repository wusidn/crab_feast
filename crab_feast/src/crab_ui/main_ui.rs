use bevy::prelude::*;
use crab_feast_ui_joysticks::{Joystick, JoystickPlugin};

pub struct MainUI;

impl Plugin for MainUI {
    fn build(&self, app: &mut App) {
        app.add_plugins(JoystickPlugin)
        .add_systems(Startup, Self::setup);
    }
}

impl MainUI {
    fn setup(
        mut commands: Commands,
        asset_server: Res<AssetServer>,
    ) {

        let font = asset_server.load("fonts/PingFang-SC-Light.ttf");

        commands.spawn((
            Node{
                width: Val::Vw(100.0),
                height: Val::Vh(100.0),
                justify_content: JustifyContent::Center,
                align_items: AlignItems::Center,
                ..Default::default()
            },
            children![
                (
                    Node{
                        width: Val::Vw(60.0),
                        height: Val::Px(200.0),
                        ..Default::default()
                    },
                    BackgroundColor(Color::hsl(160.0, 0.6, 0.8)),
                    Text("你好，Bevy!".to_string()),
                    TextFont{
                        font,
                        font_size: 32.0,
                        ..Default::default()
                    },
                    TextColor(Color::WHITE),
                    TextLayout {
                        justify: JustifyText::Center,
                        linebreak: LineBreak::AnyCharacter,
                    },
                    Joystick
                )
            ]
        ));
    }
}