// prelude.rs
// 显式导出用户最常使用的类型
pub use crate::joystick::{
    JoystickPlugin,
    Joystick, 
    JoystickState, 
    JoystickThumb, 
    JoystickDisabled, 
    JoystickEvent, 
    JoystickInteraction
};
pub use crate::marionette::{
    JoystickMarionettePlugin,
    JoystickMarionette,
};
