//! The proxy (P2-kit 2.5; K1a.11, KA2): how another thread hands a message to a window's Screen::data. The window's
//! mailbox is closed at WM_NCDESTROY under its lock, so a send never posts to a destroyed window's recycled HWND.

use std::collections::VecDeque;
use std::sync::{Arc, Mutex, MutexGuard};

use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, WPARAM};
use windows::Win32::UI::WindowsAndMessaging::{IsWindow, PostMessageW};

use super::window::{self, Outcome};
use super::{def_proc, AppMsg, WM_OLED_DATA};
use crate::dispatch::Input;

/// A window's queue of messages for Screen::data.
#[derive(Default)]
pub(crate) struct Mailbox {
    queue: VecDeque<AppMsg>,
    /// WM_NCDESTROY: nothing is accepted any more.
    closed: bool,
    /// A WM_OLED_DATA is posted and not yet drained.
    posted: bool,
}

/// The mailbox, its poison recovered (a panic elsewhere leaves the queue usable, as the dock's tray host does).
pub(crate) fn lock(m: &Mutex<Mailbox>) -> MutexGuard<'_, Mailbox> {
    m.lock().unwrap_or_else(|p| p.into_inner())
}

impl Mailbox {
    /// Closes it: the messages still queued are dropped.
    pub(crate) fn close(&mut self) {
        self.closed = true;
        self.queue.clear();
    }

    /// Every queued message, oldest first; the next send posts again.
    pub(crate) fn take(&mut self) -> Vec<AppMsg> {
        self.posted = false;
        self.queue.drain(..).collect()
    }
}

/// A window's mailbox, usable from any thread.
#[derive(Clone)]
pub struct Proxy {
    /// The window's HWND as isize (0: no window).
    hwnd: isize,
    queue: Arc<Mutex<Mailbox>>,
    /// The message that wakes the window (WM_OLED_DATA).
    msg: u32,
}

impl Proxy {
    /// A proxy with no window behind it: send returns false.
    pub(crate) fn detached() -> Proxy {
        Proxy { hwnd: 0, queue: Arc::new(Mutex::new(Mailbox::default())), msg: WM_OLED_DATA }
    }

    /// The proxy of a window and its mailbox.
    pub(crate) fn new(hwnd: HWND, queue: Arc<Mutex<Mailbox>>) -> Proxy {
        Proxy { hwnd: hwnd.0 as isize, queue, msg: WM_OLED_DATA }
    }

    /// Queues `msg` for the window's Screen::data and wakes it; false once the window is gone (or the post fails).
    pub fn send(&self, msg: AppMsg) -> bool {
        if self.hwnd == 0 {
            return false;
        }
        let hwnd = HWND(self.hwnd as *mut std::ffi::c_void);
        let mut mb = lock(&self.queue);
        // SAFETY: IsWindow only reads; the mailbox is open, so the window has not reached WM_NCDESTROY and this HWND is
        // still the window's (it closes the mailbox under this lock before its HWND can be reused).
        if mb.closed || !unsafe { IsWindow(Some(hwnd)) }.as_bool() {
            return false;
        }
        mb.queue.push_back(msg);
        if !mb.posted {
            // SAFETY: posting to a live window; the message carries no pointer.
            if unsafe { PostMessageW(Some(hwnd), self.msg, WPARAM(0), LPARAM(0)) }.is_err() {
                mb.queue.pop_back();
                return false;
            }
            mb.posted = true;
        }
        true
    }
}

/// WM_OLED_DATA: every queued message to Screen::data, in order.
pub(crate) fn drain(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    let r = window::handle(hwnd, msg, wp, lp, |h| {
        let msgs = lock(&h.mailbox).take();
        for m in msgs {
            h.input(Input::Data(m));
        }
    });
    match r {
        Outcome::NoState => def_proc(hwnd, msg, wp, lp),
        Outcome::Done(()) | Outcome::Queued => LRESULT(0),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn proxy_reports_a_gone_window() {
        assert!(!Proxy::detached().send(Box::new(1u32)), "no window");
        let mb = Arc::new(Mutex::new(Mailbox::default()));
        lock(&mb).close();
        assert!(
            !Proxy::new(HWND(std::ptr::dangling_mut::<std::ffi::c_void>()), Arc::clone(&mb)).send(Box::new(2u32)),
            "closed"
        );
        let open = Arc::new(Mutex::new(Mailbox::default()));
        assert!(
            !Proxy::new(HWND(std::ptr::dangling_mut::<std::ffi::c_void>()), Arc::clone(&open)).send(Box::new(3u32)),
            "not a window"
        );
        assert!(lock(&open).take().is_empty(), "a refused message is not kept");
    }

    #[test]
    fn mailbox_keeps_order_and_closes() {
        let mut mb = Mailbox::default();
        for i in 0..3u32 {
            mb.queue.push_back(Box::new(i));
        }
        mb.posted = true;
        let got: Vec<u32> = mb.take().into_iter().map(|m| *m.downcast::<u32>().unwrap()).collect();
        assert_eq!(got, [0, 1, 2]);
        assert!(!mb.posted, "the next send posts again");
        mb.queue.push_back(Box::new(9u32));
        mb.close();
        assert!(mb.closed && mb.take().is_empty());
    }

    #[test]
    fn proxy_is_send_and_sync() {
        fn check<T: Send + Sync>() {}
        check::<Proxy>();
    }
}
