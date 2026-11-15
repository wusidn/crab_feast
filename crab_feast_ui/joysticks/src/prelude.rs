// prelude.rs
// 显式导出用户最常使用的类型
pub use crate::joystick::{
    Joystick, JoystickDisabled, JoystickEvent, JoystickInteraction, JoystickPlugin, JoystickState,
    JoystickThumb,
};
pub use crate::marionette::{JoystickMarionette, JoystickMarionettePlugin};
