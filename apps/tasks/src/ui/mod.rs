//! The window: the screen (screen.rs routes every hook to its owner's file), the command table, the view and its
//! parts. Every part is filled by the group its file names.

pub mod about;
pub mod actions;
pub mod chrome;
pub mod commands;
pub mod context_menu;
pub mod grouped;
pub mod header;
pub mod history;
pub mod keys;
pub mod keysheet;
pub mod list;
pub mod more_menu;
pub mod notice;
pub mod order;
#[cfg(feature = "preview")]
pub mod preview;
pub mod rows;
pub mod scene;
pub mod screen;
pub mod search;
pub mod selection;
pub mod show;
pub mod tree;
pub mod view;
