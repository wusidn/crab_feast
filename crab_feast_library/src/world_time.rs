//! 世界时间速度（`Time<Virtual>` 的 relative speed）的取值区间。
//!
//! 客户端调试面板的滑杆、网络协议的夹取、服务端落值都用这一份数字：
//! 滑杆能拖到的范围如果比服务端允许的宽，玩家每次拖到头都会看到"滑杆跳回来"；
//! 反过来比服务端窄，网络版就永远调不到快捷键时代能调到的慢放档。

/// 最慢：0.02（50 倍慢放）。
///
/// 这是原 `[` 键最慢一档。速度直接乘在虚拟时间上，0.02 时固定步进只剩 1.2Hz，
/// 一个姿态要等将近一秒才走一帧——再往下不是"更慢"，而是"卡住"。
pub const MIN_SCALE: f32 = 0.02;

/// 最快：2.0（2 倍快放），与原 `]` 键顶格一致。
pub const MAX_SCALE: f32 = 2.0;

/// 把任意输入夹到合法区间。
///
/// 客户端来的浮点不可信：NaN 会顺着 `set_relative_speed` 把整套时间系统污染成 NaN
/// （虚拟时间永不再前进，表现是"画面冻住"），0 和负数会让所有 `delta` 归零。
pub fn clamp_scale(value: f32) -> f32 {
    if value.is_finite() {
        value.clamp(MIN_SCALE, MAX_SCALE)
    } else {
        1.0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clamps_out_of_range_and_nan() {
        assert_eq!(clamp_scale(0.0), MIN_SCALE);
        assert_eq!(clamp_scale(-3.0), MIN_SCALE);
        assert_eq!(clamp_scale(10.0), MAX_SCALE);
        assert_eq!(clamp_scale(f32::NAN), 1.0);
        assert_eq!(clamp_scale(f32::INFINITY), 1.0);
        assert!((clamp_scale(0.25) - 0.25).abs() < 1e-6);
    }
}
