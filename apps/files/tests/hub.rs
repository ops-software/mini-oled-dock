//! The hub and the background lanes (P3-files-app F1a.6): routing by tab and window, queued messages, the wake
//! coalescing, the start-up slot and the lanes' ordering, abandonment and idleness. No window, no file system.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use files_engine::bg::{Bg, Lane};
use files_engine::hub::{Collect, Hub, Poster};
use files_engine::listing::{Batch, Gen, TabId, WidePath};
use files_engine::msg::{JobDone, Msg};
use files_engine::window::STARTUP_TAB;
use oled_trust::frame::files::{InstanceCmd, Location};
use oled_ui::host::AppMsg;

fn batch(n: u32) -> Batch {
    Batch { gen: Gen(u64::from(n)), names: Vec::new(), entries: Vec::new(), done: true, error: None, elapsed_us: 0 }
}

fn space(tag: u64) -> JobDone {
    JobDone::FreeSpace { root: WidePath::parse(r"C:\").unwrap(), free: Some(tag), total: None }
}

fn tag_of(m: &Msg) -> Option<u64> {
    match m {
        Msg::Job { done: JobDone::FreeSpace { free, .. }, .. } => *free,
        _ => None,
    }
}

fn window(hub: &Arc<Hub>, win: u32) -> Arc<Collect> {
    let c = Arc::new(Collect::new());
    hub.attach(win, Box::new(Arc::clone(&c)));
    c
}

fn wait_until(what: &str, limit: Duration, mut f: impl FnMut() -> bool) {
    let end = Instant::now() + limit;
    while !f() {
        assert!(Instant::now() < end, "timed out waiting for {what}");
        std::thread::sleep(Duration::from_millis(5));
    }
}

#[test]
fn post_without_a_window_queues_then_flushes_on_attach() {
    let hub = Hub::new();
    hub.route(TabId(5), 1);
    hub.post(Msg::Batch { tab: TabId(5), batch: batch(1) });
    hub.post(Msg::Batch { tab: TabId(5), batch: batch(2) });
    hub.post(Msg::Job { win: 1, done: space(9) });
    let w1 = window(&hub, 1);
    let got = w1.take();
    assert_eq!(got.len(), 3);
    assert!(matches!(&got[0], Msg::Batch { tab: TabId(5), batch } if batch.gen == Gen(1)), "in order");
    assert!(matches!(&got[1], Msg::Batch { batch, .. } if batch.gen == Gen(2)));
    assert_eq!(tag_of(&got[2]), Some(9));
    hub.post(Msg::Batch { tab: TabId(5), batch: batch(3) });
    assert_eq!(w1.take().len(), 1, "after the attach, messages go straight through");
}

#[test]
fn batch_goes_to_its_tabs_window() {
    let hub = Hub::new();
    let (w1, w2) = (window(&hub, 1), window(&hub, 2));
    hub.route(TabId(5), 2);
    hub.route(TabId(4), 1);
    hub.post(Msg::Batch { tab: TabId(5), batch: batch(1) });
    assert!(w1.take().is_empty());
    assert!(matches!(w2.take().as_slice(), [Msg::Batch { tab: TabId(5), .. }]));
    hub.post(Msg::Batch { tab: TabId(4), batch: batch(2) });
    assert_eq!(w1.take().len(), 1);
    assert!(w2.take().is_empty());
    // A tab moved to another window: later batches follow it.
    hub.route(TabId(5), 1);
    hub.post(Msg::Batch { tab: TabId(5), batch: batch(3) });
    assert_eq!(w1.take().len(), 1);
}

#[test]
fn unrouted_batch_dropped() {
    let hub = Hub::new();
    let w1 = window(&hub, 1);
    hub.post(Msg::Batch { tab: TabId(9), batch: batch(1) });
    hub.route(TabId(5), 1);
    hub.unroute(TabId(5));
    hub.post(Msg::Batch { tab: TabId(5), batch: batch(2) });
    assert!(w1.take().is_empty());
    let late = window(&hub, 2);
    assert!(late.take().is_empty(), "nothing was queued for them either");
}

#[test]
fn job_goes_to_its_window() {
    let hub = Hub::new();
    let (w1, w2) = (window(&hub, 1), window(&hub, 2));
    hub.post(Msg::Job { win: 2, done: space(2) });
    assert_eq!(w2.take().iter().filter_map(tag_of).collect::<Vec<_>>(), [2]);
    assert!(w1.take().is_empty());
    hub.post(Msg::Job { win: 0, done: space(7) });
    assert_eq!(w1.take().iter().filter_map(tag_of).collect::<Vec<_>>(), [7], "win 0 is the primary window");
    assert!(w2.take().is_empty());
    assert!(!hub.post_to(3, Msg::Lang), "post_to a window not attached says so");
    assert!(hub.post_to(2, Msg::FirstFrame));
    assert!(matches!(w2.take().as_slice(), [Msg::FirstFrame]));
}

#[test]
fn instance_goes_to_the_active_window() {
    let hub = Hub::new();
    let (w1, w2) = (window(&hub, 1), window(&hub, 2));
    hub.post(Msg::Instance(InstanceCmd::OpenTab(Location::Home)));
    assert_eq!(w1.take().len(), 1, "no active window yet: the primary");
    hub.set_active(2);
    hub.post(Msg::Instance(InstanceCmd::NewWindow(Location::Home)));
    assert!(w1.take().is_empty());
    assert_eq!(w2.take().len(), 1);
    hub.detach(2);
    hub.post(Msg::Instance(InstanceCmd::OpenTab(Location::Home)));
    assert_eq!(w1.take().len(), 1, "the active window closed: back to the primary");
}

#[test]
fn updated_and_lang_reach_every_window() {
    let hub = Hub::new();
    let (w1, w2) = (window(&hub, 1), window(&hub, 2));
    hub.post(Msg::Updated { version: 7 });
    hub.post(Msg::Lang);
    for w in [&w1, &w2] {
        let got = w.take();
        assert!(matches!(got.as_slice(), [Msg::Updated { version: 7 }, Msg::Lang]));
    }
}

#[test]
fn detach_drops_routes_and_pending() {
    let hub = Hub::new();
    let w1 = window(&hub, 1);
    hub.route(TabId(7), 2);
    hub.post(Msg::Batch { tab: TabId(7), batch: batch(1) });
    hub.post(Msg::Job { win: 2, done: space(1) });
    hub.detach(2);
    hub.post(Msg::Batch { tab: TabId(7), batch: batch(2) });
    let w2 = window(&hub, 2);
    assert!(w2.take().is_empty(), "the queue and the route went with the window");
    assert!(w1.take().is_empty());
    assert_eq!(hub.primary(), Some(1));
}

struct Dead(Arc<AtomicBool>);
impl Poster for Dead {
    fn post(&self, _m: AppMsg) -> bool {
        self.0.store(true, Ordering::SeqCst);
        false
    }
}

#[test]
fn dead_poster_detached() {
    let hub = Hub::new();
    let tried = Arc::new(AtomicBool::new(false));
    hub.attach(1, Box::new(Dead(Arc::clone(&tried))));
    let w2 = window(&hub, 2);
    assert_eq!(hub.primary(), Some(1));
    hub.post(Msg::Job { win: 1, done: space(1) });
    assert!(tried.load(Ordering::SeqCst));
    assert_eq!(hub.primary(), Some(2), "the window whose post failed is gone");
    hub.post(Msg::Job { win: 1, done: space(2) });
    assert!(w2.take().is_empty(), "no further attempt, nothing re-routed");
    // A wake reaches the next primary.
    hub.wake();
    assert!(matches!(w2.take().as_slice(), [Msg::EnginesWake]));
}

#[test]
fn wake_coalesces() {
    let hub = Hub::new();
    let (w1, w2) = (window(&hub, 1), window(&hub, 2));
    let threads: Vec<_> = (0..4)
        .map(|_| {
            let h = Arc::clone(&hub);
            std::thread::spawn(move || {
                for _ in 0..250 {
                    h.wake();
                }
            })
        })
        .collect();
    for t in threads {
        t.join().unwrap();
    }
    assert_eq!(w1.take().len(), 1, "one EnginesWake, to the primary");
    assert!(w2.take().is_empty());
    hub.wake();
    assert!(w1.take().is_empty(), "none until wake_done");
    hub.wake_done();
    (hub.notifier())();
    assert_eq!(w1.take().len(), 1);
}

#[test]
fn first_batch_slot() {
    let hub = Hub::new();
    let sink = hub.sink();
    let t = std::thread::spawn(move || {
        std::thread::sleep(Duration::from_millis(30));
        sink(STARTUP_TAB, batch(1));
    });
    let got = hub.wait_first_batch(Instant::now() + Duration::from_secs(10));
    t.join().unwrap();
    assert!(matches!(got, Some(b) if b.gen == Gen(1)));
    // The slot is closed: later batches travel as messages.
    hub.route(STARTUP_TAB, 1);
    (hub.sink())(STARTUP_TAB, batch(2));
    let w1 = window(&hub, 1);
    assert!(matches!(w1.take().as_slice(), [Msg::Batch { batch, .. }] if batch.gen == Gen(2)));
    // No batch by the deadline.
    let hub = Hub::new();
    let t0 = Instant::now();
    assert!(hub.wait_first_batch(Instant::now() + Duration::from_millis(60)).is_none());
    assert!(t0.elapsed() >= Duration::from_millis(50));
}

#[test]
fn bg_runs_in_order_per_lane() {
    let hub = Hub::new();
    let w1 = window(&hub, 1);
    let bg = Bg::new(Arc::clone(&hub));
    let order = Arc::new(Mutex::new(Vec::new()));
    for lane in [Lane::Local, Lane::Network] {
        for i in 0..20u64 {
            let order = Arc::clone(&order);
            let tag = i + if lane == Lane::Network { 100 } else { 0 };
            bg.run(
                1,
                lane,
                Box::new(move || {
                    order.lock().unwrap().push(tag);
                    Some(space(tag))
                }),
            );
        }
    }
    wait_until("40 results", Duration::from_secs(10), || w1.len() >= 40);
    let got: Vec<u64> = w1.take().iter().filter_map(tag_of).collect();
    for (lo, hi) in [(0, 20), (100, 120)] {
        let lane: Vec<u64> = got.iter().copied().filter(|t| (lo..hi).contains(t)).collect();
        assert_eq!(lane, (lo..hi).collect::<Vec<_>>(), "in order within a lane");
    }
    assert_eq!(bg.stats().queued, 0);
    assert_eq!(bg.stats().threads, 2, "one thread per lane");
}

#[test]
fn bg_network_stall_does_not_block_local() {
    let hub = Hub::new();
    let w1 = window(&hub, 1);
    let bg = Bg::new(Arc::clone(&hub));
    let t0 = Instant::now();
    bg.run(
        1,
        Lane::Network,
        Box::new(|| {
            std::thread::sleep(Duration::from_secs(6));
            Some(space(1))
        }),
    );
    bg.run(1, Lane::Network, Box::new(|| Some(space(2))));
    for i in 0..3 {
        bg.run(1, Lane::Local, Box::new(move || Some(space(10 + i))));
    }
    wait_until("the local jobs", Duration::from_secs(2), || w1.len() >= 3);
    assert!(t0.elapsed() < Duration::from_secs(3), "local jobs did not wait for the stalled share");
    assert_eq!(bg.stats().abandoned, 0);
    assert_eq!(bg.stats().queued, 1, "the next network job waits");
    // After 4 s the stuck thread is abandoned and a fresh one takes the next job.
    wait_until("the second network job", Duration::from_secs(5), || w1.take().iter().any(|m| tag_of(m) == Some(2)));
    let at = t0.elapsed();
    assert!(at >= Duration::from_millis(3_900) && at < Duration::from_millis(5_900), "after about 4 s, not {at:?}");
    assert_eq!(bg.stats().abandoned, 1);
    wait_until("the stalled job returns", Duration::from_secs(5), || bg.stats().abandoned == 0);
    assert!(w1.take().iter().any(|m| tag_of(m) == Some(1)), "its result is still delivered");
}

#[test]
fn bg_idle_threads_do_not_wake() {
    let hub = Hub::new();
    let w1 = window(&hub, 1);
    let bg = Bg::new(Arc::clone(&hub));
    for lane in [Lane::Local, Lane::Network] {
        bg.run(1, lane, Box::new(|| Some(space(1))));
    }
    wait_until("both results", Duration::from_secs(5), || w1.len() >= 2);
    std::thread::sleep(Duration::from_millis(50));
    let before = bg.turns();
    std::thread::sleep(Duration::from_millis(300));
    assert_eq!(bg.turns(), before, "an empty queue costs no wake-up");
}

/// A poster that posts to another window from inside post(), as a window queue drain forwarding an engine reply does.
struct Reenter {
    hub: std::sync::Weak<Hub>,
    to: u32,
    seen: Arc<AtomicBool>,
}
impl Poster for Reenter {
    fn post(&self, _m: AppMsg) -> bool {
        if let Some(h) = self.hub.upgrade() {
            self.seen.store(h.post_to(self.to, Msg::FirstFrame), Ordering::SeqCst);
            h.post(Msg::Job { win: self.to, done: space(5) });
        }
        true
    }
}

#[test]
fn a_poster_may_call_back_into_the_hub() {
    let hub = Hub::new();
    let w2 = window(&hub, 2);
    let seen = Arc::new(AtomicBool::new(false));
    hub.attach(1, Box::new(Reenter { hub: Arc::downgrade(&hub), to: 2, seen: Arc::clone(&seen) }));
    let (tx, rx) = std::sync::mpsc::channel();
    let h = Arc::clone(&hub);
    std::thread::spawn(move || {
        h.post_to(1, Msg::Lang);
        let _ = tx.send(());
    });
    rx.recv_timeout(Duration::from_secs(10)).expect("post() ran without the route lock");
    assert!(seen.load(Ordering::SeqCst));
    let got = w2.take();
    assert!(matches!(got.as_slice(), [Msg::FirstFrame, Msg::Job { .. }]), "in order, {} messages", got.len());
}
