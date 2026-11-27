//! Icons for the file view (SPEC-files 5.5.5): icons.bin, class icon extraction, the HICON/HBITMAP reader and the
//! per-(ext, px) cache. F-GDI fills gdi.rs, F-ICONS the others; this file only declares and re-exports them.

pub mod cache;
pub mod extract;
pub mod gdi;
pub mod iconsbin;

pub use cache::{ClassIconSource, ClassIcons, IconRef, CLASS_ICONS_IN_UI};
pub use iconsbin::IconsBin;
