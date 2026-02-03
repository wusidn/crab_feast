use bevy::{ecs::{message::Message, resource::Resource}, math::Vec2};


#[derive(Resource)]
pub struct MoveInputState(pub Vec2, pub f32);


#[derive(Message)]
pub struct RotateInput(pub Vec2);
