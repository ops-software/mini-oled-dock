//! CI-only smoke test of the TK1 window items (P3-tasks-app): #[ignore], never run on a developer's machine. A real
//! window takes a WM_COPYDATA sent from another thread, then bring_to_front restores it from minimised.

use std::borrow::Cow;
use std::cell::RefCell;
use std::rc::Rc;
use std::sync::atomic::{AtomicBool, AtomicIsize, Ordering};
use std::sync::Arc;

use oled_ui::host::{AppId, Caption, EventLoop, LoopCx, WindowKind, WindowSpec};
use oled_ui::*;
use windows::Win32::Foundation::{CloseHandle, HANDLE, HWND, LPARAM, WPARAM};
use windows::Win32::System::DataExchange::COPYDATASTRUCT;
use windows::Win32::System::Threading::{CreateEventW, SetEvent};
use windows::Win32::UI::WindowsAndMessaging::{
    IsIconic, SendMessageTimeoutW, ShowWindow, SMTO_ABORTIFHUNG, SW_MINIMIZE, WM_COPYDATA,
};

/// What the window's screen saw.
#[derive(Default)]
struct Seen {
    copies: Vec<(usize, Vec<u8>)>,
    front: Option<FrontResult>,
}

/// A window with nothing in it; Screen::data asks for the front (any message) or reads its outcome (a u8).
struct Probe {
    cmds: CommandTable,
    seen: Rc<RefCell<Seen>>,
}

impl Screen for Probe {
    fn layout(&mut self, _cx: &mut LayoutCx, avail: SizeP) -> SizeP {
        avail
    }
    fn paint(&self, _cx: &mut PaintCx) {}
    fn event(&mut self, _cx: &mut EventCx, _ev: &Event) -> Reply {
        Reply::None
    }
    fn data(&mut self, cx: &mut EventCx, msg: host::AppMsg) -> Reply {
        if msg.downcast::<u8>().is_ok() {
            self.seen.borrow_mut().front = cx.front_result();
        } else {
            cx.bring_to_front();
        }
        Reply::None
    }
    fn copy_data(&mut self, _cx: &mut EventCx, tag: usize, bytes: &[u8]) -> bool {
        self.seen.borrow_mut().copies.push((tag, bytes.to_vec()));
        true
    }
    fn commands(&self) -> &CommandTable {
        &self.cmds
    }
}

/// The loop's event as a value another thread can signal.
struct Signal(isize);

impl Signal {
    fn set(&self) {
        // SAFETY: the event stays open until the loop has returned and the sender thread is joined.
        unsafe { SetEvent(HANDLE(self.0 as *mut std::ffi::c_void)) }.expect("signal");
    }
}

#[test]
#[ignore = "smoke: CI only"]
fn smoke_kit1_copy_data_and_front() {
    let mut lp = EventLoop::new(AppId("gallery"), Lang::En, ThemeSpec::oled()).expect("loop");
    let spec = WindowSpec {
        kind: WindowKind::Opaque { min: SizeL { w: 200, h: 150 }, caption: Caption::System },
        class: "MiniOledSmokeKit1",
        title: Cow::Borrowed("Smoke"),
        size: SizeL { w: 320, h: 200 },
        owner: None,
        esc_closes: false,
        placement: None,
        icon_resource: None,
        opened_by_chord: false,
    };
    let seen = Rc::new(RefCell::new(Seen::default()));
    let w = lp.open(spec, Probe { cmds: CommandTable::new(), seen: Rc::clone(&seen) }).expect("window");
    let hwnd = lp.hwnd(w).expect("the window exists");
    // SAFETY: an unnamed auto-reset event, closed after the loop returns.
    let ev = unsafe { CreateEventW(None, false, false, None) }.expect("event");
    // The sender: one WM_COPYDATA from another thread while the loop runs, then step 0 on the UI thread.
    let answer = Arc::new(AtomicIsize::new(-1));
    let (target, signal, got) = (hwnd.0 as isize, Signal(ev.0 as isize), Arc::clone(&answer));
    let sender = std::thread::spawn(move || {
        let mut payload = *b"kit1";
        let cds = COPYDATASTRUCT { dwData: 0x4B31, cbData: 4, lpData: payload.as_mut_ptr().cast() };
        let mut result = 0usize;
        // SAFETY: `cds` and its payload outlive the call; the result is writable.
        let sent = unsafe {
            SendMessageTimeoutW(
                HWND(target as *mut std::ffi::c_void),
                WM_COPYDATA,
                WPARAM(0),
                LPARAM(&cds as *const COPYDATASTRUCT as isize),
                SMTO_ABORTIFHUNG,
                5000,
                Some(&mut result),
            )
        };
        got.store(if sent.0 == 0 { -2 } else { result as isize }, Ordering::SeqCst);
        signal.set();
    });
    let step = Rc::new(RefCell::new(0u32));
    let (count, seen_cb) = (Rc::clone(&step), Rc::clone(&seen));
    let again = Signal(ev.0 as isize);
    lp.add_handle(
        ev,
        Box::new(move |cx: &mut LoopCx| {
            let n = *count.borrow();
            *count.borrow_mut() = n + 1;
            let proxy = cx.proxy(w).expect("the window exists");
            match n {
                0 => {
                    assert_eq!(answer.load(Ordering::SeqCst), 1, "SendMessageTimeoutW(WM_COPYDATA) returned TRUE");
                    assert_eq!(seen_cb.borrow().copies, [(0x4B31, b"kit1".to_vec())], "the screen saw it");
                    // SAFETY: a live window of this thread; no window borrow is held in a loop callback.
                    unsafe {
                        let _ = ShowWindow(hwnd, SW_MINIMIZE);
                        assert!(IsIconic(hwnd).as_bool(), "minimised");
                    }
                    assert!(proxy.send(Box::new(())), "bring_to_front from Screen::data");
                    again.set();
                }
                1 => {
                    // SAFETY: a read of a live window of this thread.
                    assert!(!unsafe { IsIconic(hwnd) }.as_bool(), "bring_to_front restored the window");
                    assert!(proxy.send(Box::new(0u8)), "read front_result");
                    again.set();
                }
                2 => {
                    assert!(seen_cb.borrow().front.is_some(), "the outcome is stored: {:?}", seen_cb.borrow().front);
                    cx.quit(0);
                }
                _ => {}
            }
        }),
    )
    .expect("handle");
    // Watchdog: a stuck loop fails the job in seconds instead of hanging it.
    let done = Arc::new(AtomicBool::new(false));
    let flag = Arc::clone(&done);
    let dog = std::thread::spawn(move || {
        for _ in 0..300 {
            std::thread::sleep(std::time::Duration::from_millis(100));
            if flag.load(Ordering::Relaxed) {
                return;
            }
        }
        eprintln!("smoke_kit1: the loop did not finish within 30 s");
        std::process::exit(101);
    });
    assert_eq!(lp.run(), 0);
    done.store(true, Ordering::Relaxed);
    dog.join().unwrap();
    sender.join().unwrap();
    // SAFETY: the loop has returned and the sender is joined; nothing waits on or signals the event any more.
    let _ = unsafe { CloseHandle(ev) };
    assert_eq!(*step.borrow(), 3);
}
