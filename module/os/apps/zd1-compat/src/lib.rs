//! Linux 2.6.24 compatibility for Rust programs on the module (src/compat.c). The module's binary (the multicall,
//! module/os/apps/zd1) links with `--wrap` for the calls the shims replace (its build.rs) and calls [`init`], which
//! keeps the shims in the link.

unsafe extern "C" {
    fn zd1_compat_linked() -> i32;
}

/// Keeps the shims in the link; call once at start.
pub fn init() {
    unsafe { zd1_compat_linked() };
}
