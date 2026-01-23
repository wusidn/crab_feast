
use bevy::prelude::*;

mod assets;
mod scene;
mod ui;
mod fps;
pub fn build_app() -> App {
	let mut app = App::new();
	app.add_plugins(scene::ScenePlugin)
		.add_plugins((
			assets::AssetsPlugin, 
			ui::UiPlugin,
			fps::FPSPlugin
		));

	app
}