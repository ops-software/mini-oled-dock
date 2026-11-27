//! Class icons through icons.bin and a source (IC3, IC4): a counting fake extractor on the decor thread, a fake clock.
mod common;

use std::sync::atomic::{AtomicI64, AtomicU32, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use common::temp::TempRoot;
use files_engine::decor::cache::{due, WEEK};
use files_engine::decor::iconsbin::{build, Item};
use files_engine::decor::{ClassIcons, IconRef, IconsBin};

const T0: i64 = 133_000_000_000_000_000;

struct Fake {
    calls: AtomicU32,
    fill: u8,
    refuse: &'static str,
}

impl Fake {
    fn new(fill: u8) -> Arc<Fake> {
        Arc::new(Fake { calls: AtomicU32::new(0), fill, refuse: "" })
    }
    fn calls(&self) -> u32 {
        self.calls.load(Ordering::SeqCst)
    }
}

struct Rig {
    icons: ClassIcons,
    fake: Arc<Fake>,
    clock: Arc<AtomicI64>,
    woke: Arc<AtomicU32>,
}

fn rig(root: &TempRoot, fake: Arc<Fake>) -> Rig {
    let clock = Arc::new(AtomicI64::new(T0));
    let woke = Arc::new(AtomicU32::new(0));
    let extract = {
        let fake = Arc::clone(&fake);
        Arc::new(move |ext: &str, px: u16| {
            fake.calls.fetch_add(1, Ordering::SeqCst);
            (ext != fake.refuse).then(|| vec![fake.fill; usize::from(px) * usize::from(px) * 4])
        })
    };
    let now = {
        let clock = Arc::clone(&clock);
        Arc::new(move || clock.load(Ordering::SeqCst))
    };
    let notify = {
        let woke = Arc::clone(&woke);
        Arc::new(move || {
            woke.fetch_add(1, Ordering::SeqCst);
        })
    };
    let icons = ClassIcons::with_extractor(root.path().to_path_buf(), extract, notify, now);
    Rig { icons, fake, clock, woke }
}

fn until(what: &str, mut f: impl FnMut() -> bool) {
    let end = Instant::now() + Duration::from_secs(20);
    while !f() {
        assert!(Instant::now() < end, "timed out waiting for {what}");
        std::thread::sleep(Duration::from_millis(5));
    }
}

fn ready(r: IconRef) -> Option<Vec<u8>> {
    match r {
        IconRef::Ready(a) => Some(a.to_vec()),
        _ => None,
    }
}

fn names(root: &TempRoot) -> Vec<String> {
    let mut v: Vec<String> = std::fs::read_dir(root.path())
        .expect("read_dir")
        .filter_map(|e| e.ok())
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .filter(|n| n.starts_with("icons-"))
        .collect();
    v.sort();
    v
}

fn bin_with(root: &TempRoot, built: i64, items: &[(&str, u16, u8, u32)]) {
    let items = items
        .iter()
        .map(|&(e, px, fill, hits)| Item {
            ext: e.into(),
            px,
            bgra: vec![fill; usize::from(px) * usize::from(px) * 4],
            hits,
        })
        .collect();
    root.file("icons-1.bin", &build(items, built));
}

#[test]
fn cache_hits_icons_bin_first() {
    let root = TempRoot::new("ci-bin-first");
    bin_with(&root, T0, &[("txt", 16, 7, 3)]);
    let r = rig(&root, Fake::new(9));
    assert_eq!(ready(r.icons.get(b"txt", 16)), Some(vec![7; 16 * 16 * 4]), "answered from the file");
    assert_eq!(ready(r.icons.get(b"TXT", 16)), Some(vec![7; 16 * 16 * 4]), "case blind");
    assert_eq!(r.fake.calls(), 0, "no source call for an extension the file holds");
    assert!(matches!(r.icons.get(b"zip", 16), IconRef::Pending), "an extension the file lacks is asked for");
    until("zip", || ready(r.icons.get(b"zip", 16)).is_some());
    assert_eq!(r.fake.calls(), 1);
    assert!(matches!(r.icons.get(b"txt", 24), IconRef::Pending), "a size the file lacks too");
}

#[test]
fn pending_then_ready() {
    let root = TempRoot::new("ci-pending");
    let mut fake = Fake::new(5);
    Arc::get_mut(&mut fake).expect("sole owner").refuse = "bad";
    let r = rig(&root, fake);
    assert!(matches!(r.icons.get(b"txt", 16), IconRef::Pending));
    until("txt", || ready(r.icons.get(b"txt", 16)).is_some());
    assert!(r.woke.load(Ordering::SeqCst) >= 1, "the UI was woken");
    assert_eq!(ready(r.icons.get(b"txt", 16)), Some(vec![5; 16 * 16 * 4]));
    for _ in 0..5 {
        let _ = r.icons.get(b"txt", 16);
    }
    assert_eq!(r.fake.calls(), 1, "asked once");
    assert!(matches!(r.icons.get(b"bad", 16), IconRef::Pending));
    until("bad", || matches!(r.icons.get(b"bad", 16), IconRef::Generic));
    assert!(matches!(r.icons.get(b"bad", 16), IconRef::Generic), "a failure stays generic");
    assert_eq!(r.fake.calls(), 2);
    for (e, px) in [(&b"a.b"[..], 16), (b"x/y", 16), (b"txt", 0), (b"txt", 300), (&[0xff][..], 16)] {
        assert!(matches!(r.icons.get(e, px), IconRef::Generic), "{e:?} {px}");
    }
    assert_eq!(r.fake.calls(), 2, "refused without a call");
}

#[test]
fn weekly_due() {
    assert!(due(0, 5), "no file: due");
    assert!(!due(T0, T0 + WEEK - 1));
    assert!(due(T0, T0 + WEEK));
    let root = TempRoot::new("ci-weekly");
    bin_with(&root, T0, &[("txt", 16, 7, 3)]);
    let r = rig(&root, Fake::new(9));
    r.clock.store(T0 + WEEK - 1, Ordering::SeqCst);
    r.icons.rebuild_if_due();
    std::thread::sleep(Duration::from_millis(200));
    assert_eq!((r.fake.calls(), names(&root)), (0, vec!["icons-1.bin".to_string()]), "six days and a bit: not due");
    r.clock.store(T0 + WEEK + 1, Ordering::SeqCst);
    r.icons.rebuild_if_due();
    until("icons-2.bin", || names(&root).contains(&"icons-2.bin".to_string()));
    assert_eq!(r.fake.calls(), 1, "every extension of the file answered again, at the file's size");
    r.icons.rebuild_if_due();
    std::thread::sleep(Duration::from_millis(200));
    assert_eq!(r.fake.calls(), 1, "not again in the same run");
    let next = IconsBin::open(root.path()).expect("the new file at the next start");
    assert_eq!((next.counter(), next.built()), (2, T0 + WEEK + 1));
    assert_eq!(next.get(b"txt", 16), Some(&vec![9u8; 16 * 16 * 4][..]));
    assert_eq!(names(&root), vec!["icons-2.bin".to_string()], "the old file went at that start");
}

#[test]
fn nothing_to_rebuild_writes_nothing() {
    let root = TempRoot::new("ci-empty");
    let r = rig(&root, Fake::new(9));
    r.icons.rebuild_if_due();
    std::thread::sleep(Duration::from_millis(200));
    assert_eq!((r.fake.calls(), names(&root).len()), (0, 0));
}

#[test]
fn assoc_changed_invalidates() {
    let root = TempRoot::new("ci-assoc");
    bin_with(&root, T0, &[("txt", 16, 1, 3)]);
    let r = rig(&root, Fake::new(2));
    assert_eq!(ready(r.icons.get(b"txt", 16)), Some(vec![1; 16 * 16 * 4]));
    let woke = r.woke.load(Ordering::SeqCst);
    r.icons.assoc_changed();
    assert!(r.woke.load(Ordering::SeqCst) > woke, "the windows are told");
    assert!(matches!(r.icons.get(b"txt", 16), IconRef::Pending), "memory and the old file are not trusted");
    until("the new icon", || ready(r.icons.get(b"txt", 16)).is_some());
    assert_eq!(ready(r.icons.get(b"txt", 16)), Some(vec![2; 16 * 16 * 4]), "the source's answer, not the file's");
    until("a rebuilt file", || names(&root).contains(&"icons-2.bin".to_string()));
    let next = IconsBin::open(root.path()).expect("the new file");
    assert_eq!(next.get(b"txt", 16), Some(&vec![2u8; 16 * 16 * 4][..]));
}

#[test]
fn hits_decide_the_top_256() {
    let root = TempRoot::new("ci-hits");
    let r = rig(&root, Fake::new(4));
    for n in 0..300u32 {
        let e = format!("e{n:03}");
        for _ in 0..=n {
            r.icons.note(e.as_bytes());
        }
    }
    let _ = r.icons.get(b"e299", 16);
    until("e299", || ready(r.icons.get(b"e299", 16)).is_some());
    r.icons.rebuild_if_due();
    until("icons-1.bin", || names(&root).contains(&"icons-1.bin".to_string()));
    let bin = IconsBin::open(root.path()).expect("the file");
    assert_eq!(bin.entries(), 256);
    assert!(bin.get(b"e299", 16).is_some() && bin.get(b"e044", 16).is_some(), "the 256 most noted");
    assert!(bin.get(b"e043", 16).is_none() && bin.get(b"e000", 16).is_none(), "the 44 least noted are out");
}
