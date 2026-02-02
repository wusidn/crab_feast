use bevy::{ecs::resource::Resource, math::Vec2};


#[derive(Resource)]
pub struct InputState {
    pub move_direction: Option<(Vec2, f32)>,
    pub rotate_direction: Option<Vec2>,
}

