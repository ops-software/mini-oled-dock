//! CI-only smoke test of the loop requests (kit request K-R0): #[ignore], never run on a developer's machine.

use std::borrow::Cow;
use std::cell::Cell;
use std::rc::Rc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use oled_ui::host::{AppId, Caption, EventLoop, LoopCx, WindowKind, WindowSpec};
use oled_ui::*;
use windows::Win32::Foundation::{CloseHandle, RECT};
use windows::Win32::System::Threading::{CreateEventW, SetEvent};
use windows::Win32::UI::WindowsAndMessaging::{GetWindowRect, IsWindowVisible};

/// A window with nothing in it.
struct Empty(CommandTable);

impl Screen for Empty {
    fn layout(&mut self, _cx: &mut LayoutCx, avail: SizeP) -> SizeP {
        avail
    }
    fn paint(&self, _cx: &mut PaintCx) {}
    fn event(&mut self, _cx: &mut EventCx, _ev: &Event) -> Reply {
        Reply::None
    }
    fn commands(&self) -> &CommandTable {
        &self.0
    }
}

#[test]
#[ignore = "smoke: CI only"]
fn smoke_loopcx_show_and_move() {
    let mut lp = EventLoop::new(AppId("gallery"), Lang::En, ThemeSpec::oled()).expect("loop");
    let spec = WindowSpec {
        kind: WindowKind::Opaque { min: SizeL { w: 200, h: 150 }, caption: Caption::System },
        class: "MiniOledSmokeLoop",
        title: Cow::Borrowed("Smoke"),
        size: SizeL { w: 320, h: 200 },
        owner: None,
        esc_closes: false,
        placement: None,
        icon_resource: None,
        opened_by_chord: false,
    };
    let w = lp.open(spec, Empty(CommandTable::new())).expect("window");
    // SAFETY: an unnamed auto-reset event, closed after the loop returns.
    let ev = unsafe { CreateEventW(None, false, false, None) }.expect("event");
    let step = Rc::new(Cell::new(0u32));
    let seen = Rc::clone(&step);
    lp.add_handle(
        ev,
        Box::new(move |cx: &mut LoopCx| {
            let hwnd = cx.hwnd(w).expect("the window exists");
            // SAFETY: reads of a live window of this thread.
            let visible = || unsafe { IsWindowVisible(hwnd) }.as_bool();
            let n = seen.get();
            seen.set(n + 1);
            match n {
                0 => {
                    assert!(cx.show(w, Show::Hide));
                    assert!(!visible(), "hidden");
                    // Step 1 runs on the next loop pass, after the hide's messages are pumped.
                    // SAFETY: the event stays open until the loop has returned.
                    unsafe { SetEvent(ev) }.expect("signal");
                }
                1 => {
                    assert!(cx.show(w, Show::Front));
                    assert!(visible(), "shown again");
                    assert!(cx.move_to(w, PointP { x: 120, y: 90 }));
                    let mut r = RECT::default();
                    // SAFETY: `r` outlives the call.
                    unsafe { GetWindowRect(hwnd, &mut r) }.expect("rect");
                    assert!(r.left == 120 && r.top == 90, "moved to 120,90: {r:?}");
                    assert!(cx.proxy(w).is_some());
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
        eprintln!("smoke_loop: the loop did not finish within 30 s");
        std::process::exit(101);
    });
    // SAFETY: the event is open.
    unsafe { SetEvent(ev) }.expect("signal");
    assert_eq!(lp.run(), 0);
    done.store(true, Ordering::Relaxed);
    dog.join().unwrap();
    // SAFETY: the loop has returned; nothing waits on the event any more.
    let _ = unsafe { CloseHandle(ev) };
    assert_eq!(step.get(), 2);
}
