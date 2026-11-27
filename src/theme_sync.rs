//! Writes theme.json from the dock's config. Lands with TH2; until then it does nothing.
#![allow(dead_code, unused_variables)]

/// config's on_set hook: TH2.
pub fn on_config(c: &crate::config::Config) {}

/// The last theme this process wrote: TH2.
pub fn last_written() -> Option<oled_config::theme::ThemeFile> {
    None
}
