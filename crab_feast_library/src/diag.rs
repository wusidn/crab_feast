//! 诊断日志闸门：编译期 + 运行期两层开关。
//!
//! `CRAB_LOG_*` 这组环境变量原本是在系统里直接 `std::env::var(...)` 判的 —— 一次
//! 分配一个 `String`、再做一次系统级的字符串比较，**每个渲染帧**都来一遍。开了
//! 几个开关就是每帧几次分配，而它换来的只是「要不要打日志」这一个布尔。
//!
//! 这里换成两层：
//! - 编译期：不带 feature `diag-log` 的构建里，[`log_enabled`] 是 `#[inline]`
//!   直接返回 `false` 的空函数 —— 没有 `env::var`、没有分配，release 里连分支都不剩；
//! - 运行期：带 feature 时，每个名字**只在首帧读一次**环境变量（`OnceLock`），
//!   之后是一次哈希查表。
//!
//! 名字必须登记在 [`NAMES`] 里：表里没有的一律 `false`，宁可少打日志也不会
//! 因为拼错一个常量就常开。新增一路日志记得同步加进去。

/// 所有会被查询的诊断开关名（关掉 feature 时只给测试用，故允许 dead_code）。
///
/// 只放**每帧级**的开关（`CRAB_LOG_*`）。那些只在启动时读一次的
/// （`CRAB_AUTO_TURN` / `CRAB_AUTO_WALK` / `CRAB_FRAME_JITTER` / `CRAB_SHOT_*`
/// / `CRAB_PROFILE`）留在各自的初始化处，不需要这张表。
#[cfg_attr(not(feature = "diag-log"), allow(dead_code))]
const NAMES: &[&str] = &[
    "CRAB_LOG_SPEED",
    "CRAB_LOG_YAW",
    "CRAB_LOG_TURN",
    "CRAB_LOG_JUMP",
    "CRAB_LOG_LAND",
    "CRAB_LOG_NET_JUMP",
    "CRAB_LOG_INPUT",
    "CRAB_LOG_SRV",
];

/// 这一路诊断日志要不要打。
///
/// 关掉 feature 时恒 `false`（编译器直接折掉调用点的整个分支）。
#[cfg(feature = "diag-log")]
pub fn log_enabled(name: &str) -> bool {
    use std::collections::HashMap;
    use std::sync::OnceLock;

    static FLAGS: OnceLock<HashMap<&'static str, bool>> = OnceLock::new();
    let flags = FLAGS.get_or_init(|| {
        NAMES
            .iter()
            .map(|name| (*name, std::env::var(name).is_ok()))
            .collect()
    });
    // 没登记的名字一律 `false`：拼错一个常量只该「日志少打一路」，
    // 不该在排障途中把游戏 panic 掉（所以这里也不用 `debug_assert`）
    flags.get(name).copied().unwrap_or(false)
}

/// 这一路诊断日志要不要打（未启用 `diag-log` 的构建）。
#[cfg(not(feature = "diag-log"))]
#[inline]
pub fn log_enabled(_name: &str) -> bool {
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 没登记的开关名不许返回 true（拼错常量时宁可少打日志）
    #[test]
    fn unknown_names_are_off() {
        assert!(!log_enabled("CRAB_LOG_TYPO"));
        assert!(!log_enabled(""));
    }

    #[test]
    fn names_are_well_formed() {
        for name in NAMES {
            assert!(name.starts_with("CRAB_LOG_"), "{name}");
        }
    }
}
