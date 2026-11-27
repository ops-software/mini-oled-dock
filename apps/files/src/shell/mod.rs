//! The shell host (files.exe --role=shell): third-party shell code (context menus, icons, thumbnails, verbs) runs
//! here, never in the UI, and its crashes and hangs end in a Fallback. F-SH fills every file but class_icons.rs (IC3).

pub mod breaker;
pub mod class_icons;
pub mod crumbs;
pub mod exec;
pub mod host;
pub mod images;
pub mod menu;
pub mod sta;
pub mod supervisor;
pub mod writer;
