use bevy::prelude::*;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PlatformKind {
    Desktop,
    Web,
    Mobile,
}

#[derive(Resource, Clone, Copy, Debug)]
pub struct PlatformInfo {
    pub kind: PlatformKind,
}

impl PlatformInfo {
    pub const fn current() -> Self {
        Self {
            kind: if cfg!(target_arch = "wasm32") {
                PlatformKind::Web
            } else if cfg!(any(target_os = "android", target_os = "ios")) {
                PlatformKind::Mobile
            } else {
                PlatformKind::Desktop
            },
        }
    }

    pub const fn is_mobile(self) -> bool {
        matches!(self.kind, PlatformKind::Mobile)
    }

    pub const fn is_desktop(self) -> bool {
        matches!(self.kind, PlatformKind::Desktop)
    }

    pub const fn is_web(self) -> bool {
        matches!(self.kind, PlatformKind::Web)
    }
}
