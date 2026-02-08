use bevy::{ecs::{event::Event, resource::Resource}, math::Vec2, reflect::Reflect};


#[derive(Resource, Debug, Clone, Copy, Reflect, Default)]
pub enum MoveInputState {
    #[default]
    Idle,
    Activated {
        direction: Vec2,
        force: f32,
    }
}


#[derive(Event)]
pub struct RotateInput(pub Vec2);
