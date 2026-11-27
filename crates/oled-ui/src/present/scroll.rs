//! Retained scroll (K2a.10, KC2): the viewport's pixels move by dy and only the exposed strip is painted.

use windows::Win32::Foundation::RECT;
use windows::Win32::Graphics::Gdi::InvalidateRect;

use crate::geom::RectP;
use crate::harness::Image;
use crate::host::window::WinHost;

/// Effect::Scroll in a window. Until K2a.10: the whole viewport is invalidated.
#[allow(dead_code)] // Reached once the host creates windows (K1a.7).
pub(crate) fn apply(host: &mut WinHost, viewport: RectP, dy: i32) {
    let _ = dy;
    let r = RECT::from(viewport);
    // SAFETY: host.hwnd is the window this state belongs to; the rect lives for the call.
    let _ = unsafe { InvalidateRect(Some(host.hwnd), Some(&r), false) };
}

/// Effect::Scroll in the Harness: moves the image's viewport pixels; false when it did not (the Harness then repaints
/// the viewport). Until K2a.10: false.
pub(crate) fn emulate(img: &mut Image, viewport: RectP, dy: i32) -> bool {
    let _ = (img, viewport, dy);
    false
}
