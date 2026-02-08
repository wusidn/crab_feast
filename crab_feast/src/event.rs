use bevy::{ecs::{event::Event, resource::Resource}, math::Vec2};


#[derive(Resource, Default)]
pub struct MoveInputState {
    pub direction: Vec2,
    pub force: f32,
    pub active: bool,
}


#[derive(Event)]
pub struct RotateInput(pub Vec2);
