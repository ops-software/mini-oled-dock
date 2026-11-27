//! The suite's UI toolkit.
// ---- L-B (this plan): moved drawing code and the tokens
pub mod gfx; // scale, image, wic, glyphs, canvas, pixel, panel
pub mod scroll; // the dock popups' ScrollModel (LEGACY until K1b's ScrollArea)
pub mod text; // gdi (GdiText)
pub mod tokens;
// ---- L-K (kit): every other module (host, theme, anim, widgets, a11y, test harness) below this line

pub(crate) mod a11y;
pub(crate) mod anim;
pub mod command;
pub mod cx;
pub(crate) mod dispatch;
pub mod event;
pub mod geom;
pub mod glyph;
pub mod harness;
pub mod host;
pub mod model;
pub mod node;
pub mod present;
pub mod screen;
pub mod strings;
pub mod theme;
pub mod verbs;
pub(crate) mod w3;
pub mod widgets;

// What `use oled_ui::*;` gives an app.
pub use command::*;
pub use cx::*;
pub use event::*;
pub use geom::*;
pub use glyph::Glyph;
pub use host::Show;
pub use node::*;
pub use screen::*;
pub use strings::{Lang, UiLang};
pub use theme::*;
pub use w3::{Canvas, Scale};
