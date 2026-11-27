//! Text drawing (W3).
pub mod gdi;

// ---- L-K (kit)
pub mod cache;
pub mod digits;
pub mod engine;
pub mod hit;
pub mod trim;

pub use engine::{FontId, FontMetrics, GdiTextEngine, LayoutOpts, TextEngine, TextHit, TextLayout};
pub use trim::Trim;
