use bevy::{ecs::{event::Event, resource::Resource}, math::Vec2};


#[derive(Resource)]
pub struct MoveInputState(pub Vec2, pub f32);


#[derive(Event)]
pub struct RotateInput(pub Vec2);
