//! The loop requests of the files app (kit request K-R0): what an add_handle or follow_theme_file callback may do to
//! a window beyond send, set_theme, open and quit. Compile-only: no test creates a window (P2-kit 0.4 rule 1); the
//! live read-back is apps/gallery/tests/smoke_loop.rs.

use oled_ui::host::{LoopCx, Proxy, WindowRef};
use oled_ui::{Lang, PointP, Show};
use windows::Win32::Foundation::HWND;

/// What an app's loop callback does with its windows: one use of each method.
#[allow(dead_code)]
fn on_signal(cx: &mut LoopCx, w: WindowRef) {
    let _p: Option<Proxy> = cx.proxy(w);
    let _h: Option<HWND> = cx.hwnd(w);
    cx.set_lang(Lang::It);
    let _shown: bool = cx.show(w, Show::Front);
    let _hidden: bool = cx.show(w, Show::Hide);
    let _moved: bool = cx.move_to(w, PointP { x: 10, y: 20 });
}

#[test]
fn loop_requests_compile() {
    // As add_handle takes it.
    let _cb: Box<dyn FnMut(&mut LoopCx)> = Box::new(|cx: &mut LoopCx| on_signal(cx, WindowRef(1)));
    // Show is Copy and comparable, at the crate root and in host.
    let how: oled_ui::host::Show = Show::Hide;
    let again = how;
    assert_eq!(how, again);
    assert_ne!(Show::Hide, Show::Front);
}
