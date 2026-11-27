//! Windows entry points (Start menu shortcuts, Open with) of the suite apps. Lands with I1b; until then it does nothing.
#![allow(dead_code, unused_variables)]

/// config's on_set hook: I1b.
pub fn on_config(c: &crate::config::Config) {}

/// Brings the entry points in line with `c`; elevated only: I1b.
pub fn sync(c: &crate::config::Config, install_dir: &std::path::Path) -> Result<u32, String> {
    Err("not available in this build".into())
}

/// Asks the shell thread to run `sync`: I1b.
pub fn request_sync() {}
