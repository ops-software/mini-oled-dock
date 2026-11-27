//! The proxy (P2-kit 2.5; K1a.11, KA2): how another thread hands a message to a window's Screen::data.

use std::collections::VecDeque;
use std::sync::{Arc, Mutex};

use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, WPARAM};

use super::{def_proc, AppMsg, WM_OLED_DATA};

/// A window's mailbox, usable from any thread.
#[derive(Clone)]
pub struct Proxy {
    /// The window's HWND as isize (0: no window).
    hwnd: isize,
    queue: Arc<Mutex<VecDeque<AppMsg>>>,
    /// The message that wakes the window (WM_OLED_DATA).
    msg: u32,
}

impl Proxy {
    /// A proxy with no window behind it: send returns false.
    pub(crate) fn detached() -> Proxy {
        Proxy { hwnd: 0, queue: Arc::new(Mutex::new(VecDeque::new())), msg: WM_OLED_DATA }
    }

    /// Queues `msg` for the window's Screen::data and wakes it; false once the window is gone. Filled by K1a.11; until
    /// then no window exists and it returns false.
    pub fn send(&self, msg: AppMsg) -> bool {
        let _ = (msg, &self.queue, self.msg, self.hwnd);
        false
    }
}

/// WM_OLED_DATA: every queued message to Screen::data, in order. Filled by K1a.11.
#[allow(dead_code)]
pub(crate) fn drain(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    def_proc(hwnd, msg, wp, lp)
}
