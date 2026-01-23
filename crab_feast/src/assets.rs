use bevy::prelude::*;

#[derive(Resource)]
pub struct DefaultAssets {
    pub font: Handle<Font>,
}

pub struct AssetsPlugin;


impl Plugin for AssetsPlugin {
    fn build(&self, app: &mut App) {
        app.add_systems(PreStartup, load_assets);
    }
}

fn load_assets(
    mut commands: Commands,
    asset_server: Res<AssetServer>,
) { 
    commands.insert_resource(DefaultAssets {
        font: asset_server.load("fonts/PingFang-SC-Light.ttf"),
    });
}