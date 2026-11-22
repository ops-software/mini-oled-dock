//! The launcher popup (SPEC-launcher 5.2 and 5.5): the content the popup host shows for `PopupKind::Launcher`.
//! L1a-13 adds this stub (an empty 640 x 62 frame); L1a-14 fills the view and L1a-15 the chord flow, replacing the
//! bodies and keeping these signatures.
#![deny(clippy::unwrap_used, clippy::expect_used, clippy::indexing_slicing)]

use windows::Win32::Foundation::HWND;
use windows::Win32::UI::WindowsAndMessaging::KillTimer;

use super::{Content, PopupKind, Reply};
use crate::launcher::style;
use crate::surface::Surface;

/// Height of the empty panel, the field alone (logical px, SPEC 5.2.3).
const EMPTY_H: i32 = 62;

/// The launcher's popup content.
pub(crate) struct Launcher;

impl Content for Launcher {
    fn kind(&self) -> PopupKind {
        PopupKind::Launcher
    }

    fn size(&mut self, s: &Surface) -> (i32, i32) {
        let m = s.metrics();
        (m.px(style::LAUNCHER_W), m.px(EMPTY_H))
    }

    fn paint(&self, s: &mut Surface, _hot: Option<u32>, _pressed: Option<u32>) {
        s.panel();
    }

    fn hit(&self, _x: i32, _y: i32) -> Option<u32> {
        None
    }

    fn click(&mut self, _id: u32) -> Reply {
        Reply::None
    }
}

/// The content of a new launcher popup (`popup::make_content`).
pub(crate) fn new() -> Box<dyn Content> {
    Box::new(Launcher)
}

/// Once, at the end of `popup::init` (wired by L1a-15): nothing to set up yet.
// The chord flow of L1a-15 calls it; until then nothing does.
#[allow(dead_code)]
pub(crate) fn init() {}

/// One of the launcher's one-shot timers (`TIMER_LAUNCHER_*`) fired on the popup window: killed first.
// `popup::wnd_proc` forwards the launcher's timer ids here from L1a-13f on.
#[allow(dead_code)]
pub(crate) fn on_timer(hwnd: HWND, id: usize) {
    // SAFETY: a timer of our popup window; killing an id that is not armed fails harmlessly.
    let _ = unsafe { KillTimer(Some(hwnd), id) };
}
