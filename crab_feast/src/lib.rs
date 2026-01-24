
use bevy::prelude::*;

mod scene;
mod ui;
pub fn build_app() -> App {
	let mut app = App::new();
	app.add_plugins(scene::ScenePlugin)
		.add_plugins((
			ui::UiPlugin,
			crab_feast_ui_fps::FPSPlugin,
		));

	app
}