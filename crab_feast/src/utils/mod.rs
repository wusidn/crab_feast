

pub fn is_mobile() -> bool {
    cfg!(target_os = "android") || cfg!(target_os = "ios")
}

pub fn is_non_mobile() -> bool {
    !is_mobile()
}