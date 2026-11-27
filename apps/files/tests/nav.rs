//! Per-tab navigation (P3-files-app F1a.5): one scripted table per rule, times from a fake clock. Pure: no window,
//! no service, no file system.

mod common;

use std::os::windows::ffi::OsStrExt;
use std::sync::Arc;
use std::time::{Duration, Instant};

use common::temp::TempRoot;
use files_engine::hub::{Collect, Hub};
use files_engine::listing::{
    Batch, CancelToken, EntryStore, EnumError, EnumJob, EnumPool, Gen, RawEntry, TabId, WidePath,
};
use files_engine::model::Listing;
use files_engine::msg::Msg;
use files_engine::nav::{parent_of, BatchVerdict, NavEffect, NavState, NavTimer, Page};
use oled_trust::frame::files::Location;

fn wp(s: &str) -> WidePath {
    WidePath::parse(s).unwrap()
}

fn fs(s: &str) -> Location {
    Location::Fs(wp(s))
}

fn nav_at(s: &str) -> NavState {
    // A tab that showed its folder completely.
    let mut n = NavState::new(TabId(2), fs(s));
    let g = n.gen();
    let (v, _) = n.on_batch(&batch(g, 3, true), 5);
    assert_eq!(v, BatchVerdict::Done);
    assert_eq!(n.page(), &Page::Listing);
    n
}

fn batch(gen: Gen, entries: u32, done: bool) -> Batch {
    let mut b = Batch { gen, names: Vec::new(), entries: Vec::new(), done, error: None, elapsed_us: 0 };
    for i in 0..entries {
        let name = format!("item-{i}.txt");
        let name_off = b.names.len() as u32;
        b.names.extend_from_slice(name.as_bytes());
        b.entries.push(RawEntry {
            name_off,
            name_len: name.len() as u16,
            attrs: 0x20,
            reparse_tag: 0,
            size: 1,
            mtime: 0,
            ctime: 0,
        });
    }
    b
}

fn failed(gen: Gen, e: EnumError) -> Batch {
    Batch { error: Some(e), ..batch(gen, 0, true) }
}

fn enumerate(fx: &[NavEffect]) -> Option<(WidePath, Gen, CancelToken)> {
    fx.iter().find_map(|e| match e {
        NavEffect::Enumerate { dir, gen, cancel } => Some((dir.clone(), *gen, cancel.clone())),
        _ => None,
    })
}

fn arms(fx: &[NavEffect]) -> Vec<(NavTimer, u32)> {
    fx.iter().filter_map(|e| if let NavEffect::Arm(t, ms) = e { Some((*t, *ms)) } else { None }).collect()
}

fn disarms(fx: &[NavEffect]) -> Vec<NavTimer> {
    fx.iter().filter_map(|e| if let NavEffect::Disarm(t) = e { Some(*t) } else { None }).collect()
}

fn has(fx: &[NavEffect], f: impl Fn(&NavEffect) -> bool) -> bool {
    fx.iter().any(f)
}

#[test]
fn go_cancels_the_old_listing_and_bumps_gen() {
    let mut n = NavState::new(TabId(2), fs(r"C:\a"));
    let (_, g0, token0) = enumerate(&n.go(fs(r"C:\b"), None, 10)).expect("enumerates");
    assert_ne!(g0, Gen(1));
    assert_eq!(n.gen(), g0);
    assert!(!token0.is_cancelled());
    let fx = n.go(fs(r"C:\c"), None, 20);
    let cancelled = fx.iter().find_map(|e| if let NavEffect::Cancel(t) = e { Some(t.clone()) } else { None });
    cancelled.expect("the running listing is cancelled by an effect").cancel();
    assert!(token0.is_cancelled(), "the effect carries the old listing's token");
    let (dir, g1, _) = enumerate(&fx).unwrap();
    assert_eq!(dir, wp(r"C:\c"));
    assert!(g1 > g0);
    assert_eq!(n.location(), &fs(r"C:\c"));
}

#[test]
fn first_batch_within_60_shows_new() {
    let mut n = nav_at(r"C:\a");
    let fx = n.go(fs(r"C:\b"), None, 1_000);
    assert!(has(&fx, |e| matches!(e, NavEffect::KeepOld)));
    assert!(arms(&fx).contains(&(NavTimer::Blank60, 60)));
    assert_eq!(n.page(), &Page::Listing, "the old page stays");
    let (v, fx) = n.on_batch(&batch(n.gen(), 5, false), 1_030);
    assert_eq!(v, BatchVerdict::First);
    assert!(has(&fx, |e| matches!(e, NavEffect::ShowNew)));
    assert!(disarms(&fx).contains(&NavTimer::Blank60));
    assert_eq!(n.page(), &Page::Listing);
}

#[test]
fn no_batch_by_60_shows_new_empty() {
    let mut n = nav_at(r"C:\a");
    n.go(fs(r"C:\b"), None, 1_000);
    assert_eq!(n.page(), &Page::Listing, "KeepOld until the timer");
    let fx = n.on_timer(NavTimer::Blank60, 1_060);
    assert!(has(&fx, |e| matches!(e, NavEffect::ShowNew)));
    assert_eq!(n.page(), &Page::Loading { since_ms: 1_000 });
}

#[test]
fn busy_from_300_until_done() {
    let mut n = nav_at(r"C:\a");
    let fx = n.go(fs(r"C:\b"), None, 0);
    assert!(arms(&fx).contains(&(NavTimer::Busy300, 300)));
    n.on_timer(NavTimer::Blank60, 60);
    assert!(!n.busy());
    n.on_timer(NavTimer::Busy300, 300);
    assert!(n.busy());
    assert_eq!(n.page(), &Page::Busy);
    let (v, _) = n.on_batch(&batch(n.gen(), 2, true), 500);
    assert_eq!(v, BatchVerdict::Done);
    assert!(!n.busy());
    assert_eq!(n.page(), &Page::Listing);
}

#[test]
fn network_connecting_at_1000_offline_at_4000() {
    let mut n = nav_at(r"C:\a");
    let fx = n.go(fs(r"\\nas\share\x"), None, 0);
    let a = arms(&fx);
    assert!(a.contains(&(NavTimer::Slow1000, 1_000)) && a.contains(&(NavTimer::Offline4000, 4_000)));
    n.on_timer(NavTimer::Blank60, 60);
    n.on_timer(NavTimer::Busy300, 300);
    n.on_timer(NavTimer::Slow1000, 1_000);
    assert_eq!(n.page(), &Page::Connecting);
    n.on_timer(NavTimer::Offline4000, 4_000);
    assert_eq!(n.page(), &Page::Offline);
    let (v, _) = n.on_batch(&batch(n.gen(), 4, false), 5_000);
    assert_eq!(v, BatchVerdict::First);
    assert_eq!(n.page(), &Page::Listing, "a later batch still shows the listing");
    // A local folder arms neither.
    let fx = n.go(fs(r"C:\b"), None, 6_000);
    assert!(!arms(&fx).iter().any(|(t, _)| matches!(t, NavTimer::Slow1000 | NavTimer::Offline4000)));
}

#[test]
fn still_loading_at_10000_stops_the_sweep() {
    let mut n = nav_at(r"C:\a");
    let fx = n.go(fs(r"C:\b"), None, 0);
    assert!(arms(&fx).contains(&(NavTimer::Still10000, 10_000)));
    n.on_timer(NavTimer::Blank60, 60);
    n.on_timer(NavTimer::Busy300, 300);
    assert!(n.busy());
    n.on_timer(NavTimer::Still10000, 10_000);
    assert!(!n.busy());
    assert_eq!(n.page(), &Page::StillLoading);
}

#[test]
fn done_disarms_every_timer_and_watches() {
    let mut n = nav_at(r"C:\a");
    n.go(fs(r"C:\b"), None, 0);
    n.on_batch(&batch(n.gen(), 300, false), 20);
    let (_, fx) = n.on_batch(&batch(n.gen(), 10, false), 40);
    assert!(arms(&fx).contains(&(NavTimer::Merge100, 80)));
    let (v, fx) = n.on_batch(&batch(n.gen(), 10, true), 50);
    assert_eq!(v, BatchVerdict::Done);
    let off = disarms(&fx);
    for t in [NavTimer::Busy300, NavTimer::Still10000, NavTimer::Merge100] {
        assert!(off.contains(&t), "{t:?} disarmed");
    }
    assert!(has(&fx, |e| matches!(e, NavEffect::Watch(p) if *p == wp(r"C:\b"))));
    assert!(has(&fx, |e| matches!(e, NavEffect::FreeSpace(p) if *p == wp(r"C:\b"))));
    // An idle tab has no timer: a stray timer does nothing.
    assert!(n.on_timer(NavTimer::Busy300, 300).is_empty());
    assert!(!n.busy());
}

#[test]
fn error_pages() {
    for (err, page) in [
        (EnumError::AccessDenied, Page::Denied),
        (EnumError::Offline { waited_ms: 4_000 }, Page::Offline),
        (EnumError::TooLong, Page::TooLong),
        (EnumError::Unsupported, Page::Unsupported),
        (EnumError::Win32(1234), Page::Unsupported),
    ] {
        let mut n = nav_at(r"C:\a");
        n.go(fs(r"C:\b"), None, 0);
        let (v, fx) = n.on_batch(&failed(n.gen(), err), 10);
        assert_eq!(v, BatchVerdict::Failed(err));
        assert_eq!(n.page(), &page, "{err:?}");
        assert!(has(&fx, |e| matches!(e, NavEffect::ShowNew)), "the error page replaces the old listing");
        assert!(!n.busy());
    }
}

#[test]
fn not_found_stays_where_we_were() {
    let mut n = nav_at(r"C:\a");
    n.go(fs(r"C:\missing"), None, 100);
    let (v, fx) = n.on_batch(&failed(n.gen(), EnumError::NotFound), 110);
    assert_eq!(v, BatchVerdict::Failed(EnumError::NotFound), "the verdict carries the path error for the path line");
    assert_eq!(n.location(), &fs(r"C:\a"));
    assert_eq!(n.page(), &Page::Listing);
    assert!(!n.can_back(), "the history reverted");
    assert!(has(&fx, |e| matches!(e, NavEffect::KeepOld)));
    assert!(!has(&fx, |e| matches!(e, NavEffect::ShowNew)));
    assert!(enumerate(&fx).is_none(), "the old listing is still on screen: nothing to list again");
    // Late: the empty new page was already shown, so the old folder is listed again.
    n.go(fs(r"C:\missing"), None, 200);
    n.on_timer(NavTimer::Blank60, 260);
    let (_, fx) = n.on_batch(&failed(n.gen(), EnumError::NotFound), 270);
    assert_eq!(n.location(), &fs(r"C:\a"));
    let (dir, gen, _) = enumerate(&fx).expect("old folder listed again");
    assert_eq!(dir, wp(r"C:\a"));
    assert_eq!(gen, n.gen());
    // A stale history entry behaves the same.
    let mut n = nav_at(r"C:\a");
    n.go(fs(r"C:\b"), None, 0);
    n.on_batch(&batch(n.gen(), 1, true), 5);
    n.back(10);
    n.on_batch(&failed(n.gen(), EnumError::NotFound), 12);
    assert_eq!(n.location(), &fs(r"C:\b"));
    assert!(n.can_back() && !n.can_forward());
}

#[test]
fn other_gen_batches_dropped() {
    let mut n = nav_at(r"C:\a");
    let old = n.gen();
    n.go(fs(r"C:\b"), None, 0);
    let (v, fx) = n.on_batch(&batch(old, 9, true), 5);
    assert_eq!(v, BatchVerdict::Drop);
    assert!(fx.is_empty());
    // After done, a straggler of the same generation is dropped too.
    n.on_batch(&batch(n.gen(), 1, true), 6);
    assert_eq!(n.on_batch(&batch(n.gen(), 1, false), 7).0, BatchVerdict::Drop);
}

#[test]
fn history_cap_100_and_forward_truncated() {
    let mut n = nav_at(r"C:\a");
    for i in 0..150 {
        n.go(fs(&format!(r"C:\d{i}")), None, 0);
    }
    assert_eq!(n.history(true, 1_000).len(), 99, "100 entries, one of them the current");
    assert_eq!(n.history(true, 3).len(), 3);
    assert_eq!(n.history(true, 1)[0].1, fs(r"C:\d148"), "nearest first");
    n.back(0);
    n.back(0);
    assert!(n.can_forward());
    assert_eq!(n.history(false, 10).len(), 2);
    n.go(fs(r"C:\fresh"), None, 0);
    assert!(!n.can_forward(), "forward history is dropped");
    assert_eq!(n.history(true, 1)[0].1, fs(r"C:\d147"));
    let idx = n.history(true, 5)[4].0;
    n.jump(idx, 0);
    assert_eq!(n.location(), &fs(r"C:\d143"));
}

#[test]
fn up_focuses_the_folder_we_came_from() {
    let mut n = nav_at(r"C:\data\reports");
    n.up(0);
    assert_eq!(n.location(), &fs(r"C:\data"));
    assert!(n.can_back());
    let (_, fx) = n.on_batch(&batch(n.gen(), 3, true), 5);
    assert!(has(&fx, |e| matches!(e, NavEffect::Focus(Some(f)) if &**f == b"reports")));
    // Focus is given once.
    n.refresh(10);
    let (_, fx) = n.on_batch(&batch(n.gen(), 3, true), 15);
    assert!(!has(&fx, |e| matches!(e, NavEffect::Focus(_))));
}

#[test]
fn up_from_a_drive_root_goes_home() {
    let mut n = nav_at(r"C:\");
    assert!(n.can_up());
    let fx = n.up(0);
    assert_eq!(n.location(), &Location::Home);
    assert_eq!(parent_of(&fs(r"C:\")), Some(Location::Home));
    assert_eq!(n.page(), &Page::Home);
    assert!(enumerate(&fx).is_none());
    assert!(!n.can_up());
    assert!(n.up(0).is_empty(), "Home has no parent");
}

#[test]
fn home_needs_no_enumeration() {
    let mut n = nav_at(r"C:\a");
    let fx = n.go(Location::Home, None, 0);
    assert!(enumerate(&fx).is_none());
    assert!(has(&fx, |e| matches!(e, NavEffect::ShowNew)));
    assert!(arms(&fx).is_empty());
    assert_eq!(n.page(), &Page::Home);
    assert!(!n.busy());
    // Back from Home lists the folder again; Home stays on screen until the first batch.
    let fx = n.back(5);
    assert!(enumerate(&fx).is_some());
    assert_eq!(n.page(), &Page::Home);
    assert!(NavState::new(TabId(3), Location::Home).page() == &Page::Home);
}

#[test]
fn merge_every_100ms() {
    let mut n = nav_at(r"C:\a");
    n.go(fs(r"C:\b"), None, 0);
    let (v, fx) = n.on_batch(&batch(n.gen(), 256, false), 10);
    assert_eq!(v, BatchVerdict::First);
    assert!(arms(&fx).is_empty(), "the first batch is merged at once");
    let (v, fx) = n.on_batch(&batch(n.gen(), 2_048, false), 40);
    assert_eq!(v, BatchVerdict::More);
    assert_eq!(arms(&fx), vec![(NavTimer::Merge100, 70)]);
    let (_, fx) = n.on_batch(&batch(n.gen(), 2_048, false), 60);
    assert!(arms(&fx).is_empty(), "armed once");
    assert!(n.on_timer(NavTimer::Merge100, 110).is_empty());
    let (_, fx) = n.on_batch(&batch(n.gen(), 2_048, false), 130);
    assert_eq!(arms(&fx), vec![(NavTimer::Merge100, 80)], "the next merge is due 100 ms after the last");
}

#[test]
fn refresh_keeps_the_history_and_bumps_gen() {
    let mut n = nav_at(r"C:\a");
    n.go(fs(r"C:\b"), None, 0);
    n.on_batch(&batch(n.gen(), 1, true), 1);
    let g = n.gen();
    let fx = n.refresh(10);
    assert!(n.gen() > g);
    assert!(enumerate(&fx).is_some());
    assert_eq!(n.history(true, 10).len(), 1);
    assert!(!n.can_forward());
}

/// F1a.7: the real pool and the real store behind the hub, driven by the navigation state, over a folder on disk. The
/// window half of this check (the table rows and the status text) is FA-WIN's: its screen does not consume batches yet.
#[test]
fn temproot_listing_end_to_end() {
    let root = TempRoot::new("nav_end_to_end");
    for i in 0..10 {
        root.dir(&format!("folder {}", i + 1));
    }
    for i in 0..1_990 {
        root.file(&format!("file {}.txt", i + 1), b"");
    }
    let dir = WidePath::new(&root.path().as_os_str().encode_wide().collect::<Vec<u16>>()).unwrap();
    let hub = Hub::new();
    let window = Arc::new(Collect::new());
    hub.attach(1, Box::new(Arc::clone(&window)));
    let tab = TabId(2);
    hub.route(tab, 1);
    let pool = EnumPool::new(hub.sink());

    let mut nav = NavState::new(tab, Location::Fs(dir.clone()));
    let fx = nav.refresh(0);
    let (job_dir, gen, cancel) = enumerate(&fx).expect("the folder is enumerated");
    let mut listing: Box<dyn Listing> = Box::new(EntryStore::new(job_dir.clone(), gen));
    pool.submit(EnumJob { tab, gen, dir: job_dir, cancel });

    let end = Instant::now() + Duration::from_secs(30);
    let mut verdicts = Vec::new();
    while verdicts.last() != Some(&BatchVerdict::Done) {
        assert!(Instant::now() < end, "the listing finished");
        for m in window.take() {
            let Msg::Batch { tab: t, batch } = m else { continue };
            assert_eq!(t, tab);
            let (v, _) = nav.on_batch(&batch, 1);
            verdicts.push(v);
            if v != BatchVerdict::Drop {
                listing.append(batch);
                listing.merge_pending();
            }
        }
        std::thread::sleep(Duration::from_millis(2));
    }
    assert_eq!(nav.page(), &Page::Listing);
    assert!(listing.done() && listing.error().is_none());
    assert_eq!(listing.len(), 2_000);
    let names: Vec<(bool, String)> = listing
        .order()
        .iter()
        .map(|&id| (listing.is_dir(id), String::from_utf8_lossy(listing.name(id).unwrap_or_default()).into_owned()))
        .collect();
    assert_eq!(names.len(), 2_000);
    assert!(names[..10].iter().all(|(d, _)| *d), "folders first");
    assert!(names[10..].iter().all(|(d, _)| !*d));
    let want: Vec<String> =
        (1..=10).map(|i| format!("folder {i}")).chain((1..=1_990).map(|i| format!("file {i}.txt"))).collect();
    let got: Vec<String> = names.into_iter().map(|(_, n)| n).collect();
    assert_eq!(got, want, "natural order: file 2 before file 10");
}

#[test]
fn not_found_while_the_old_folder_is_still_loading_lists_it_again() {
    let mut n = NavState::new(TabId(2), fs(r"C:\big"));
    n.on_batch(&batch(n.gen(), 50, false), 5);
    assert_eq!(n.page(), &Page::Listing);
    let g_big = n.gen();
    let fx = n.go(fs(r"C:\missing"), None, 100);
    let cancelled = fx.iter().find_map(|e| if let NavEffect::Cancel(t) = e { Some(t.clone()) } else { None });
    assert!(cancelled.is_some(), "the unfinished listing was cancelled");
    let (_, fx) = n.on_batch(&failed(n.gen(), EnumError::NotFound), 110);
    assert_eq!(n.location(), &fs(r"C:\big"));
    let (dir, gen, _) = enumerate(&fx).expect("the cancelled folder is listed again");
    assert_eq!(dir, wp(r"C:\big"));
    assert!(gen > g_big && gen == n.gen());
    assert!(has(&fx, |e| matches!(e, NavEffect::KeepOld)));
    assert!(arms(&fx).contains(&(NavTimer::Busy300, 300)));
    let (v, _) = n.on_batch(&batch(gen, 3, true), 150);
    assert_eq!(v, BatchVerdict::Done);
}

#[test]
fn go_then_not_found_within_60ms_lists_the_target_of_the_first_go() {
    let mut n = nav_at(r"C:\b");
    n.go(fs(r"C:\a"), None, 1_000);
    n.go(fs(r"C:\missing"), None, 1_010);
    let (_, fx) = n.on_batch(&failed(n.gen(), EnumError::NotFound), 1_020);
    assert_eq!(n.location(), &fs(r"C:\a"));
    let (dir, gen, _) = enumerate(&fx).expect("a's enumeration was cancelled: list it again");
    assert_eq!(dir, wp(r"C:\a"));
    assert_eq!(gen, n.gen());
    assert!(has(&fx, |e| matches!(e, NavEffect::KeepOld)));
    assert!(n.can_back(), "the history keeps a and b");
}

#[test]
fn startup_tab_arms_its_timers_and_cancels_the_start_job() {
    let mut n = NavState::new(TabId(2), fs(r"\\nas\share"));
    let job = CancelToken::new();
    let fx = n.adopt_start(job.clone(), 5);
    let a = arms(&fx);
    for t in [NavTimer::Busy300, NavTimer::Slow1000, NavTimer::Offline4000, NavTimer::Still10000] {
        assert!(a.iter().any(|(x, _)| *x == t), "{t:?} armed");
    }
    n.on_timer(NavTimer::Slow1000, 1_005);
    assert_eq!(n.page(), &Page::Connecting);
    // Leaving the folder cancels the start-up job itself.
    let fx = n.go(fs(r"C:\x"), None, 2_000);
    let c = fx.iter().find_map(|e| if let NavEffect::Cancel(t) = e { Some(t.clone()) } else { None });
    c.expect("Cancel").cancel();
    assert!(job.is_cancelled());
    // Done ends the timers it armed.
    let mut n = NavState::new(TabId(2), fs(r"\\nas\share"));
    n.adopt_start(CancelToken::new(), 0);
    let (v, fx) = n.on_batch(&batch(n.gen(), 2, true), 10);
    assert_eq!(v, BatchVerdict::Done);
    let off = disarms(&fx);
    for t in [NavTimer::Busy300, NavTimer::Slow1000, NavTimer::Offline4000, NavTimer::Still10000] {
        assert!(off.contains(&t), "{t:?} disarmed");
    }
}

#[test]
fn home_to_folder_keeps_home_until_the_first_batch() {
    let mut n = NavState::new(TabId(2), Location::Home);
    let fx = n.go(fs(r"C:\a"), None, 100);
    assert!(has(&fx, |e| matches!(e, NavEffect::KeepOld)));
    assert!(arms(&fx).contains(&(NavTimer::Blank60, 60)));
    assert!(!has(&fx, |e| matches!(e, NavEffect::ShowNew)), "no blank frame");
    assert_eq!(n.page(), &Page::Home);
    let (_, fx) = n.on_batch(&batch(n.gen(), 4, false), 105);
    assert!(has(&fx, |e| matches!(e, NavEffect::ShowNew)));
    assert_eq!(n.page(), &Page::Listing);
    // An error page is kept the same way.
    let mut n = nav_at(r"C:\a");
    n.go(fs(r"C:\b"), None, 0);
    n.on_batch(&failed(n.gen(), EnumError::AccessDenied), 5);
    assert_eq!(n.page(), &Page::Denied);
    let fx = n.go(fs(r"C:\"), None, 10);
    assert!(has(&fx, |e| matches!(e, NavEffect::KeepOld)) && !has(&fx, |e| matches!(e, NavEffect::ShowNew)));
}

#[test]
fn go_to_the_current_folder_with_a_name_focuses_it() {
    let mut n = nav_at(r"C:\work");
    let fx = n.go(fs(r"C:\work"), Some(b"report.pdf".to_vec().into_boxed_slice()), 10);
    assert!(enumerate(&fx).is_some());
    let (_, fx) = n.on_batch(&batch(n.gen(), 3, true), 20);
    assert!(has(&fx, |e| matches!(e, NavEffect::Focus(Some(f)) if &**f == b"report.pdf")));
    assert!(!n.can_back(), "no history entry for the same folder");
}

#[test]
fn up_focus_name_is_wtf8() {
    let mut units: Vec<u16> = r"C:\data\".encode_utf16().collect();
    units.extend([0xD800, u16::from(b'x')]);
    let mut n = NavState::new(TabId(2), Location::Fs(WidePath::new(&units).unwrap()));
    n.up(0);
    assert_eq!(n.location(), &fs(r"C:\data"));
    let (_, fx) = n.on_batch(&batch(n.gen(), 1, true), 5);
    assert!(has(&fx, |e| matches!(e, NavEffect::Focus(Some(f)) if **f == [0xED, 0xA0, 0x80, b'x'])));
}
