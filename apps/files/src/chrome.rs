//! The window's chrome (P3-files-app 2.2; FB-CHROME fills it, F1b.1): the tab strip, the Beta chip and, with the
//! custom caption, the caption buttons. F1a has none: the system caption is the only chrome.

use oled_ui::host::Caption;
use oled_ui::{CaptionHit, Event, EventCx, LayoutCx, PaintCx, PointP, RectP, Reply, Scale};

use crate::window::FilesScreen;

#[derive(Debug, Default)]
pub struct Chrome;

impl Chrome {
    pub fn new() -> Chrome {
        Chrome
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ChromeRects {
    pub strip: RectP,
    pub tabs: RectP,
    pub beta: Option<RectP>,
    pub buttons: Option<RectP>,
    pub below: i32,
}

/// Pure. Filled by FB-CHROME; until then every rect empty and below 0.
pub fn rects(caption: Caption, width: i32, scale: Scale) -> ChromeRects {
    let _ = (caption, width, scale);
    ChromeRects::default()
}

/// The rect below the chrome. Filled by FB-CHROME; until then `r` (zero height).
pub fn layout(s: &mut FilesScreen, cx: &mut LayoutCx, r: RectP) -> RectP {
    let _ = (s, cx);
    r
}

/// Filled by FB-CHROME; until then nothing is drawn.
pub fn paint(s: &FilesScreen, cx: &mut PaintCx) {
    let _ = (s, cx);
}

/// TabOut -> tab::on_out; PathOut -> pathline::on_out. Filled by FB-CHROME; until then not taken.
pub fn event(s: &mut FilesScreen, cx: &mut EventCx, ev: &Event) -> Option<Reply> {
    let _ = (s, cx, ev);
    None
}

/// Filled by FB-CHROME; until then Client.
pub fn caption_hit(s: &FilesScreen, p: PointP) -> CaptionHit {
    let _ = (s, p);
    CaptionHit::Client
}

/// A tab strip exists (F1b on). Filled by FB-CHROME; until then false.
pub fn has_strip(s: &FilesScreen) -> bool {
    let _ = s;
    false
}
