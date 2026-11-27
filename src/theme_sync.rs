//! The dock writes the suite theme: <root>\suite\theme.json (F-P2, TH2).
//!
//! WHY: the suite's apps draw with the dock's look and read it from one admin-only file that only the elevated dock
//! writes. The look is a projection of the dock's config (`project`). `on_config` runs on whatever thread changed
//! the config (UI or update worker) and only computes and compares; the write itself (an fsync and a write-through
//! rename) happens on a short-lived "theme-write" thread, one at a time, with the newest look winning.
//!
//! This is the only module that writes theme.json. Without a writable data root (tests, --preview, a non-elevated
//! process) nothing is written and no thread outlives its work.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, OnceLock};

use oled_config::theme::{self, Lang, Preset, Rgb, ThemeFile};

use crate::config::{Config, Language};

/// The `writer` field of the file.
fn writer_name() -> String {
    format!("mini-oled-dock {}", env!("CARGO_PKG_VERSION"))
}

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

/// The file the dock's config implies, with the given generation. Pure.
pub fn project(c: &Config, generation: u64) -> ThemeFile {
    let language = match c.language {
        Language::Auto => Lang::Auto,
        Language::En => Lang::En,
        Language::It => Lang::It,
    };
    let mut t = ThemeFile { generation, writer: writer_name(), language, ..ThemeFile::default() };
    let (r, g, b) = c.background.rgb();
    if (r, g, b) != (0, 0, 0) {
        t.look.preset = Preset::Custom;
        t.look.seed.bg = Rgb(r as u8, g as u8, b as u8);
    }
    // Popups follow the bar opacity.
    t.look.knobs.bar_opacity = c.bar_opacity;
    t.look.knobs.surface_opacity = c.bar_opacity;
    t.look.knobs.sizes.icon = c.app_icon_size;
    t.look.knobs.sizes.text = c.app_text_size;
    t.look.knobs.sizes.tray = c.tray_icon_size;
    t.look.knobs.sizes.module_pct = c.module_size;
    t
}

fn same_look(a: &ThemeFile, b: &ThemeFile) -> bool {
    let mut a = a.clone();
    a.generation = b.generation;
    a == *b
}

/// Where the file lives and how it is written; faked in tests.
trait Sink: Send + Sync + 'static {
    /// The file as it is on disk, if it is there and valid.
    fn existing(&self) -> Option<ThemeFile>;
    /// Writes it. Ok(false) = there is nowhere to write (nothing happened).
    fn write(&self, t: &ThemeFile) -> Result<bool, String>;
}

struct DiskSink;

impl Sink for DiskSink {
    fn existing(&self) -> Option<ThemeFile> {
        if !oled_sys::datadir::writable() {
            return None;
        }
        let p = theme::theme_path()?;
        let bytes = oled_sys::datadir::read_file(&p, theme::THEME_MAX_BYTES, true).ok()?;
        theme::parse(&bytes).ok().map(|(t, _)| t)
    }

    fn write(&self, t: &ThemeFile) -> Result<bool, String> {
        if !oled_sys::datadir::writable() {
            return Ok(false);
        }
        let Some(dir) = oled_sys::datadir::suite_dir() else { return Ok(false) };
        let path = dir.join(theme::THEME_FILE);
        oled_sys::datadir::write_atomic(&path, theme::to_json(t).as_bytes(), oled_sys::datadir::FileAcl::Inherit)?;
        Ok(true)
    }
}

#[derive(Default)]
struct State {
    /// The newest projection nobody has taken yet.
    pending: Option<ThemeFile>,
    /// The projection last seen by on_config, generation zeroed.
    seen: Option<ThemeFile>,
    running: bool,
}

struct Syncer {
    sink: Box<dyn Sink>,
    state: Mutex<State>,
    /// Held while a write happens: two writers never overlap.
    write_lock: Mutex<()>,
    last: Mutex<Option<ThemeFile>>,
    /// Highest generation known (written, or found on disk).
    floor: AtomicU64,
    seeded: Mutex<bool>,
}

impl Syncer {
    fn new(sink: Box<dyn Sink>) -> Arc<Syncer> {
        Arc::new(Syncer {
            sink,
            state: Mutex::new(State::default()),
            write_lock: Mutex::new(()),
            last: Mutex::new(None),
            floor: AtomicU64::new(0),
            seeded: Mutex::new(false),
        })
    }

    /// The caller's part: project, compare, hand over. Never touches the disk.
    fn on_config(self: &Arc<Self>, c: &Config) {
        let p = project(c, 0);
        let mut st = lock(&self.state);
        if st.seen.as_ref() == Some(&p) {
            return;
        }
        st.seen = Some(p.clone());
        st.pending = Some(p);
        if st.running {
            return;
        }
        st.running = true;
        let me = Arc::clone(self);
        let spawned =
            std::thread::Builder::new().name("theme-write".into()).stack_size(128 * 1024).spawn(move || me.run());
        if spawned.is_err() {
            st.running = false;
            st.seen = None; // retried by the next change
        }
    }

    fn next_generation(&self) -> u64 {
        let ms = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_millis() as u64);
        ms.max(self.floor.load(Ordering::Relaxed).saturating_add(1))
    }

    fn run(self: Arc<Self>) {
        loop {
            let want = {
                let mut st = lock(&self.state);
                match st.pending.take() {
                    Some(p) => p,
                    None => {
                        st.running = false;
                        return;
                    }
                }
            };
            let _w = lock(&self.write_lock);
            let mut seeded = lock(&self.seeded);
            if !*seeded {
                *seeded = true;
                if let Some(d) = self.sink.existing() {
                    self.floor.fetch_max(d.generation, Ordering::Relaxed);
                    if same_look(&d, &want) {
                        // The file already says it (a restart): remember it, write nothing.
                        *lock(&self.last) = Some(d);
                        continue;
                    }
                }
            }
            drop(seeded);
            let mut t = want.clone();
            t.generation = self.next_generation();
            match self.sink.write(&t) {
                Ok(true) => {
                    self.floor.store(t.generation, Ordering::Relaxed);
                    *lock(&self.last) = Some(t);
                }
                Ok(false) => {}
                Err(e) => {
                    crate::log!("theme: not written: {e}");
                    // Let the next change (or the same look again) try once more.
                    let mut st = lock(&self.state);
                    if st.pending.is_none() {
                        st.seen = None;
                    }
                }
            }
        }
    }

    #[cfg(test)]
    fn idle(&self) -> bool {
        !lock(&self.state).running
    }
}

static SYNC: OnceLock<Arc<Syncer>> = OnceLock::new();

fn global() -> &'static Arc<Syncer> {
    SYNC.get_or_init(|| Syncer::new(Box::new(DiskSink)))
}

/// config's on_set hook. On the caller's thread: a projection and a compare, never the disk.
pub fn on_config(c: &Config) {
    global().on_config(c);
}

/// The last theme this process wrote (or found on disk equal to its look). Read by the suite's dock-side pages
/// (P3-dock-suite); until they land only the freeze test calls it.
#[allow(dead_code)]
pub fn last_written() -> Option<ThemeFile> {
    SYNC.get().and_then(|s| lock(&s.last).clone())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicBool, AtomicUsize};
    use std::time::{Duration, Instant};

    struct Fake {
        writes: Mutex<Vec<ThemeFile>>,
        threads: Mutex<Vec<std::thread::ThreadId>>,
        inside: AtomicUsize,
        max_inside: AtomicUsize,
        blocked: AtomicBool,
        disk: Mutex<Option<ThemeFile>>,
    }

    impl Fake {
        fn new() -> Arc<Fake> {
            Arc::new(Fake {
                writes: Mutex::new(Vec::new()),
                threads: Mutex::new(Vec::new()),
                inside: AtomicUsize::new(0),
                max_inside: AtomicUsize::new(0),
                blocked: AtomicBool::new(false),
                disk: Mutex::new(None),
            })
        }
    }

    struct FakeSink(Arc<Fake>);

    impl Sink for FakeSink {
        fn existing(&self) -> Option<ThemeFile> {
            lock(&self.0.disk).clone()
        }
        fn write(&self, t: &ThemeFile) -> Result<bool, String> {
            let f = &self.0;
            let n = f.inside.fetch_add(1, Ordering::SeqCst) + 1;
            f.max_inside.fetch_max(n, Ordering::SeqCst);
            lock(&f.threads).push(std::thread::current().id());
            while f.blocked.load(Ordering::SeqCst) {
                std::thread::sleep(Duration::from_millis(1));
            }
            lock(&f.writes).push(t.clone());
            f.inside.fetch_sub(1, Ordering::SeqCst);
            Ok(true)
        }
    }

    fn rig() -> (Arc<Syncer>, Arc<Fake>) {
        let f = Fake::new();
        (Syncer::new(Box::new(FakeSink(f.clone()))), f)
    }

    fn settle(s: &Arc<Syncer>) {
        let t0 = Instant::now();
        while !s.idle() {
            assert!(t0.elapsed() < Duration::from_secs(20), "writer did not finish");
            std::thread::sleep(Duration::from_millis(1));
        }
    }

    #[test]
    fn project_today() {
        let want = ThemeFile { generation: 7, writer: writer_name(), ..ThemeFile::default() };
        assert_eq!(project(&Config::default(), 7), want);
        assert!(want.writer.starts_with("mini-oled-dock "));
    }

    #[test]
    fn project_follows_the_config() {
        let c = Config { background: crate::config::Background::DARK, ..Config::default() };
        let t = project(&c, 1);
        assert_eq!(t.look.preset, Preset::Custom);
        assert_eq!(t.look.seed.bg, Rgb(0x1C, 0x1C, 0x1E));
        let c = Config { bar_opacity: 80, ..Config::default() };
        let t = project(&c, 1);
        assert_eq!((t.look.knobs.bar_opacity, t.look.knobs.surface_opacity), (80, 80));
        let c =
            Config { app_icon_size: 20, app_text_size: 15, tray_icon_size: 18, module_size: 120, ..Config::default() };
        let s = project(&c, 1).look.knobs.sizes;
        assert_eq!((s.icon, s.text, s.tray, s.module_pct), (20, 15, 18, 120));
        for (l, w) in [(Language::Auto, Lang::Auto), (Language::En, Lang::En), (Language::It, Lang::It)] {
            let c = Config { language: l, ..Config::default() };
            assert_eq!(project(&c, 1).language, w);
        }
    }

    #[test]
    fn unchanged_projection_writes_nothing() {
        let (s, f) = rig();
        let c = Config::default();
        s.on_config(&c);
        settle(&s);
        s.on_config(&c);
        settle(&s);
        assert_eq!(lock(&f.writes).len(), 1);
    }

    #[test]
    fn a_file_that_already_says_it_is_not_rewritten() {
        let (s, f) = rig();
        let c = Config::default();
        *lock(&f.disk) = Some(project(&c, 5));
        s.on_config(&c);
        settle(&s);
        assert!(lock(&f.writes).is_empty());
        let mut c2 = c.clone();
        c2.bar_opacity = 50;
        s.on_config(&c2);
        settle(&s);
        let w = lock(&f.writes);
        assert_eq!(w.len(), 1);
        assert!(w[0].generation > 5);
    }

    #[test]
    fn writes_are_coalesced_and_serialized() {
        let (s, f) = rig();
        f.blocked.store(true, Ordering::SeqCst);
        let mut c = Config::default();
        for i in 0..50u8 {
            c.bar_opacity = i;
            s.on_config(&c);
        }
        std::thread::sleep(Duration::from_millis(20));
        f.blocked.store(false, Ordering::SeqCst);
        settle(&s);
        let w = lock(&f.writes);
        assert!(!w.is_empty() && w.len() <= 2, "{} writes", w.len());
        assert_eq!(w.last().unwrap().look.knobs.bar_opacity, 49);
        assert_eq!(f.max_inside.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn generation_rises() {
        let (s, f) = rig();
        let mut c = Config::default();
        let mut before = Vec::new();
        for i in 0..5u8 {
            c.bar_opacity = 10 + i;
            before.push(unix_ms());
            s.on_config(&c);
            settle(&s);
        }
        let w = lock(&f.writes);
        assert_eq!(w.len(), 5);
        assert!(w.iter().zip(&before).all(|(t, b)| t.generation >= *b), "a generation is below the unix ms");
        assert!(w.windows(2).all(|p| p[1].generation > p[0].generation));
        assert_eq!(lock(&s.last).as_ref().map(|t| t.generation), Some(w[4].generation));
    }

    fn unix_ms() -> u64 {
        std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_millis() as u64)
    }

    #[test]
    fn a_generation_from_the_future_is_followed_without_overflow() {
        let (s, f) = rig();
        let c = Config::default();
        let mut old = project(&c, u64::MAX - 1);
        old.look.knobs.bar_opacity = old.look.knobs.bar_opacity.wrapping_add(7);
        *lock(&f.disk) = Some(old);
        s.on_config(&c);
        settle(&s);
        let w = lock(&f.writes);
        assert_eq!(w.len(), 1);
        assert_eq!(w[0].generation, u64::MAX);
    }

    #[test]
    fn on_config_never_writes_on_the_caller() {
        let (s, f) = rig();
        let mut c = Config::default();
        for i in 0..5u8 {
            c.bar_opacity = 20 + i;
            s.on_config(&c);
            settle(&s);
        }
        let me = std::thread::current().id();
        let th = lock(&f.threads);
        assert_eq!(th.len(), 5);
        assert!(th.iter().all(|t| *t != me));
    }

    #[test]
    fn the_real_sink_writes_nothing_without_a_data_root() {
        assert_eq!(DiskSink.write(&ThemeFile::default()), Ok(false));
        assert!(DiskSink.existing().is_none());
    }

    #[test]
    #[ignore = "bench: recorded in the group record"]
    fn bench_project() {
        let s = Syncer::new(Box::new(FakeSink(Fake::new())));
        let c = Config::default();
        s.on_config(&c);
        settle(&s);
        let t0 = Instant::now();
        for _ in 0..10_000 {
            s.on_config(&c);
        }
        let mean = t0.elapsed().as_secs_f64() * 1000.0 / 10_000.0;
        eprintln!("bench_project: mean {mean:.5} ms");
        assert!(mean <= 0.1);
    }

    #[test]
    #[ignore = "bench: recorded in the group record"]
    fn bench_theme_write() {
        let salt = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_nanos());
        let dir = std::env::temp_dir().join(format!("dock_theme_bench_{}_{salt:x}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        let p = dir.join("theme.json");
        let mut t = ThemeFile::default();
        for n in 0..64 {
            t.overrides.insert(format!("bench.padding.entry_{n:02}"), n);
        }
        let json = theme::to_json(&t);
        assert!(json.len() >= 2048, "{} bytes", json.len());
        let bytes = json.as_bytes();
        let mut ms: Vec<f64> = (0..100)
            .map(|_| {
                let t0 = Instant::now();
                oled_sys::datadir::write_atomic(&p, bytes, oled_sys::datadir::FileAcl::Inherit).unwrap();
                t0.elapsed().as_secs_f64() * 1000.0
            })
            .collect();
        ms.sort_by(|a, b| a.total_cmp(b));
        eprintln!("bench_theme_write: {} bytes, p95 {:.3} ms, max {:.3} ms", bytes.len(), ms[94], ms[99]);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
