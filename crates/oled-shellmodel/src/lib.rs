//! The shell model of the suite: how a running program is identified, named and drawn.
//!
//! * `identity`: the grouping key of an exe path, install trees, AppUserModelIDs (pure).
//! * `process`: the image, package, creation time and parent of a process (Windows, query-only handles).
//! * `version`: the version-resource strings of an exe (COM STA on the calling thread).
//! * `appsfolder`: the catalogue name of an AppUserModelID (COM STA on the calling thread).
//! * `names`: the name of a program and its heuristics (pure, except `SystemSources`).
//! * `icon`: the icon sources in their order (COM STA on the calling thread; the caller converts and caches).
//! * `window`: what a window says about its program (its AppUserModelID).
//! * `startmenu`: the dock's Start Menu shortcut index and the sources of a name over it.
//!
//! The crate keeps no state of its own: no `thread_local!`, no `static mut`. Every cache belongs to the caller.

pub mod appsfolder;
pub mod icon;
pub mod identity;
pub mod names;
pub mod process;
pub mod startmenu;
pub mod version;
pub mod window;
