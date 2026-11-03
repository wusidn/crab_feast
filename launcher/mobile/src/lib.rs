use bevy::prelude::*;
use crab_feast_launcher::{LaunchConfig, build_app};

// the `bevy_main` proc_macro generates the required boilerplate for iOS and Android
#[bevy_main]
pub fn main() {
    let mut app = build_app(LaunchConfig::default(), crab_feast::build_app);
    app.run();
}
