//! The window's remembered state, %LOCALAPPDATA%\mini-oled-dock\tasks\state.json: the view mode, the collapsed
//! sections and the placements (our own small format, version 1, at most 64 KiB, unknown fields ignored, a bad file
//! ignored with a log line). The write is debounced without an EventCx: PlacementStore::save posts one StateDirty per
//! burst, the screen arms TIMER_STATE and the timer flushes.

use std::path::PathBuf;
use std::sync::{Arc, Mutex, MutexGuard};

use oled_sys::datadir::FileAcl;
use oled_ui::host::{Placement, PlacementKey, PlacementStore};
use oled_ui::{EventCx, RectP};
use serde_json::{json, Value};
use windows::core::BOOL;
use windows::Win32::Foundation::{LPARAM, RECT};
use windows::Win32::Graphics::Gdi::{EnumDisplayMonitors, GetMonitorInfoW, HDC, HMONITOR, MONITORINFOEXW};

use super::paths::Paths;
use super::MAIN_PLACEMENT;
use crate::model::view::{Section, ViewMode};
use crate::ui::screen::TIMER_STATE;

/// Sends StateDirty to the window; false once the window is gone.
type Poster = Box<dyn Fn() -> bool + Send + Sync>;

/// The write follows the first change of a burst by this long.
const WRITE_DELAY_MS: u32 = 2000;
/// The file's size cap.
const FILE_CAP: u64 = 64 * 1024;
/// Placements of other monitor layouts kept in the file, the current one included; the oldest is dropped.
pub const MAX_LAYOUTS: usize = 8;

/// The main window's placement under one monitor layout, as the file holds it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Entry {
    pub id: String,
    /// Higher is newer.
    pub seq: u64,
    pub p: Placement,
}

#[derive(Default)]
struct Inner {
    view: Option<ViewMode>,
    /// Apps, Background, Part of Windows.
    collapsed: [Option<bool>; 3],
    /// Under the current monitor layout.
    placements: Vec<(&'static str, Placement)>,
    /// The main window's placements under the other layouts, as read from the file.
    others: Vec<Entry>,
    /// The file; None: nothing is read or written.
    path: Option<PathBuf>,
    /// The current monitor layout's id (as of the last load or save).
    layout: String,
    /// Reads the layout id now; None: the id never changes (tests).
    layout_fn: Option<fn() -> String>,
    /// Tree mode's collapsed nodes (row keys), for the session only: never written.
    tree_collapsed: std::collections::HashSet<u64>,
    /// Changed since the last flush.
    dirty: bool,
    post: Option<Poster>,
}

/// Cheap to clone; every clone is the same state.
#[derive(Clone, Default)]
pub struct StateHandle {
    inner: Arc<Mutex<Inner>>,
}

impl StateHandle {
    fn lock(&self) -> MutexGuard<'_, Inner> {
        self.inner.lock().unwrap_or_else(|p| p.into_inner())
    }

    /// app::main: `move || proxy.send(Box::new(StateDirty))`.
    pub fn attach(&self, post: Box<dyn Fn() -> bool + Send + Sync>) {
        self.lock().post = Some(post);
    }

    /// Marks the state dirty; true only for the first change of a burst (until the next flush).
    pub fn touch(&self) -> bool {
        !std::mem::replace(&mut self.lock().dirty, true)
    }

    pub fn view(&self) -> Option<ViewMode> {
        self.lock().view
    }

    pub fn set_view(&self, m: ViewMode) {
        self.lock().view = Some(m);
    }

    pub fn collapsed(&self, s: Section) -> Option<bool> {
        self.lock().collapsed.get(section_index(s)).copied().flatten()
    }

    pub fn set_collapsed(&self, s: Section, on: bool) {
        if let Some(c) = self.lock().collapsed.get_mut(section_index(s)) {
            *c = Some(on);
        }
    }

    /// Whether the Tree node of this row key is collapsed (session only).
    pub fn tree_collapsed(&self, key: u64) -> bool {
        self.lock().tree_collapsed.contains(&key)
    }

    pub fn set_tree_collapsed(&self, key: u64, on: bool) {
        let mut g = self.lock();
        if on {
            g.tree_collapsed.insert(key);
        } else {
            g.tree_collapsed.remove(&key);
        }
    }

    /// Writes now when dirty (TIMER_STATE, close); nothing when nothing changed. With no file the burst just ends.
    pub fn flush(&self) {
        let (bytes, path) = {
            let mut g = self.lock();
            if !std::mem::replace(&mut g.dirty, false) {
                return;
            }
            let Some(path) = g.path.clone() else { return };
            (render(&g), path)
        };
        if oled_sys::datadir::write_atomic(&path, &bytes, FileAcl::Inherit).is_err() {
            oled_sys::log!("tasks: state not written");
        }
    }
}

fn section_index(s: Section) -> usize {
    match s {
        Section::Apps => 0,
        Section::Background => 1,
        Section::Windows => 2,
    }
}

impl PlacementStore for StateHandle {
    fn load(&self, key: PlacementKey) -> Option<Placement> {
        self.lock().placements.iter().find(|(k, _)| *k == key.0).map(|(_, p)| p.clone())
    }

    /// Stores it; the first change of a burst posts one StateDirty (the write follows TIMER_STATE, open point 22).
    fn save(&mut self, key: PlacementKey, p: &Placement) {
        {
            let mut inner = self.lock();
            if key.0 == MAIN_PLACEMENT.0 {
                inner.follow_layout();
            }
            match inner.placements.iter_mut().find(|(k, _)| *k == key.0) {
                Some((_, old)) => *old = p.clone(),
                None => inner.placements.push((key.0, p.clone())),
            }
        }
        if self.touch() {
            let post = self.lock().post.take();
            if let Some(post) = post {
                let _ = post();
                self.lock().post = Some(post);
            }
        }
    }
}

impl Inner {
    /// When the monitors changed since the last load or save, the main placement held so far belongs to the old
    /// layout: it moves to `others` under the old id, and the layout becomes the current one.
    fn follow_layout(&mut self) {
        let Some(f) = self.layout_fn else { return };
        let now = f();
        if now == self.layout {
            return;
        }
        let next = self.others.iter().map(|e| e.seq).max().map_or(1, |m| m.saturating_add(1));
        if let Some(i) = self.placements.iter().position(|(k, _)| *k == MAIN_PLACEMENT.0) {
            let (_, p) = self.placements.remove(i);
            let id = std::mem::take(&mut self.layout);
            self.others.retain(|e| e.id != id);
            self.others.push(Entry { id, seq: next, p });
        }
        self.others.retain(|e| e.id != now);
        self.layout = now;
    }
}

/// AppMsg: a placement changed (posted by PlacementStore::save).
pub struct StateDirty;

/// Reads state.json under the current monitor layout. A missing or bad file leaves the defaults (a bad one with one
/// log line).
pub fn load(p: &Paths) -> StateHandle {
    let h = load_with(p, &current_layout());
    h.lock().layout_fn = Some(current_layout);
    h
}

fn current_layout() -> String {
    layout_id(&monitors())
}

/// load over a given layout id.
pub fn load_with(p: &Paths, layout: &str) -> StateHandle {
    let h = StateHandle::default();
    {
        let mut g = h.lock();
        g.layout = layout.to_string();
        g.path = p.local.as_ref().map(|d| d.join("state.json"));
        if let Some(path) = g.path.clone() {
            match oled_sys::datadir::read_file(&path, FILE_CAP, false) {
                Ok(bytes) => {
                    if !apply_file(&mut g, &bytes) {
                        oled_sys::log!("tasks: state file ignored");
                    }
                }
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                Err(_) => oled_sys::log!("tasks: state file unreadable"),
            }
        }
    }
    h
}

/// Reads the file's bytes into `g`; false when they are not our format (nothing is taken then).
fn apply_file(g: &mut Inner, bytes: &[u8]) -> bool {
    let Ok(v) = serde_json::from_slice::<Value>(bytes) else { return false };
    if v.get("version").and_then(Value::as_u64) != Some(1) {
        return false;
    }
    g.view = v.get("view").and_then(Value::as_str).and_then(|s| match s {
        "grouped" => Some(ViewMode::Grouped),
        "tree" => Some(ViewMode::Tree),
        "list" => Some(ViewMode::List),
        _ => None,
    });
    for (i, name) in COLLAPSED_KEYS.iter().enumerate() {
        g.collapsed[i] = v.get("collapsed").and_then(|c| c.get(name)).and_then(Value::as_bool);
    }
    let mut entries = Vec::new();
    if let Some(map) = v.get("placements").and_then(Value::as_object) {
        for (id, e) in map {
            if let Some(entry) = entry_of(id, e) {
                entries.push(entry);
            }
        }
    }
    prune(&mut entries);
    for e in entries {
        if e.id == g.layout {
            g.placements.push((MAIN_PLACEMENT.0, e.p));
        } else {
            g.others.push(e);
        }
    }
    true
}

const COLLAPSED_KEYS: [&str; 3] = ["apps", "background", "windows"];

/// One placement of the file; None when a field is missing or the wrong type.
fn entry_of(id: &str, v: &Value) -> Option<Entry> {
    let r = v.get("rect")?.as_array()?;
    let n = |i: usize| r.get(i).and_then(Value::as_i64).and_then(|x| i32::try_from(x).ok());
    let rect = RectP { l: n(0)?, t: n(1)?, r: n(2)?, b: n(3)? };
    Some(Entry {
        id: id.to_string(),
        seq: v.get("seq").and_then(Value::as_u64).unwrap_or(0),
        p: Placement {
            monitor: v.get("monitor")?.as_str()?.to_string(),
            rect,
            dpi: u32::try_from(v.get("dpi")?.as_u64()?).ok()?,
            maximized: v.get("maximized")?.as_bool()?,
        },
    })
}

/// Keeps the MAX_LAYOUTS newest entries.
pub fn prune(entries: &mut Vec<Entry>) {
    entries.sort_by_key(|e| std::cmp::Reverse(e.seq));
    entries.truncate(MAX_LAYOUTS);
}

/// The file's bytes: the other layouts, the current one as the newest, the view and the collapsed sections.
fn render(g: &Inner) -> Vec<u8> {
    let mut entries = g.others.clone();
    let next = entries.iter().map(|e| e.seq).max().map_or(1, |m| m.saturating_add(1));
    if let Some((_, p)) = g.placements.iter().find(|(k, _)| *k == MAIN_PLACEMENT.0) {
        entries.retain(|e| e.id != g.layout);
        entries.push(Entry { id: g.layout.clone(), seq: next, p: p.clone() });
    }
    prune(&mut entries);
    let mut placements = serde_json::Map::new();
    for e in entries {
        let p = e.p;
        placements.insert(
            e.id,
            json!({ "monitor": p.monitor, "rect": [p.rect.l, p.rect.t, p.rect.r, p.rect.b], "dpi": p.dpi,
                    "maximized": p.maximized, "seq": e.seq }),
        );
    }
    let mut collapsed = serde_json::Map::new();
    for (i, name) in COLLAPSED_KEYS.iter().enumerate() {
        if let Some(b) = g.collapsed[i] {
            collapsed.insert((*name).to_string(), Value::Bool(b));
        }
    }
    let mut root = serde_json::Map::new();
    root.insert("version".into(), json!(1));
    root.insert("placements".into(), Value::Object(placements));
    root.insert("collapsed".into(), Value::Object(collapsed));
    if let Some(m) = g.view {
        let name = match m {
            ViewMode::Grouped => "grouped",
            ViewMode::Tree => "tree",
            ViewMode::List => "list",
        };
        root.insert("view".into(), json!(name));
    }
    serde_json::to_vec(&Value::Object(root)).unwrap_or_default()
}

/// 16 hex digits of FNV-1a 64 over the sorted (device name, monitor rect) pairs: the same set of monitors in any order
/// gives the same id.
pub fn layout_id(monitors: &[(String, [i32; 4])]) -> String {
    let mut sorted: Vec<&(String, [i32; 4])> = monitors.iter().collect();
    sorted.sort();
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    let mut eat = |b: u8| {
        h ^= u64::from(b);
        h = h.wrapping_mul(0x0000_0100_0000_01b3);
    };
    for (name, rect) in sorted {
        name.bytes().for_each(&mut eat);
        eat(0);
        for v in rect {
            v.to_le_bytes().into_iter().for_each(&mut eat);
        }
    }
    format!("{h:016x}")
}

/// The monitors now: device name and full rectangle.
fn monitors() -> Vec<(String, [i32; 4])> {
    unsafe extern "system" fn each(m: HMONITOR, _dc: HDC, _r: *mut RECT, data: LPARAM) -> BOOL {
        // SAFETY: data is the address of the Vec of the call below, alive for the whole enumeration.
        let out = unsafe { &mut *(data.0 as *mut Vec<(String, [i32; 4])>) };
        let mut info = MONITORINFOEXW::default();
        info.monitorInfo.cbSize = std::mem::size_of::<MONITORINFOEXW>() as u32;
        // SAFETY: info is writable and its cbSize says it is the EX form.
        if unsafe { GetMonitorInfoW(m, (&mut info as *mut MONITORINFOEXW).cast()) }.as_bool() {
            let len = info.szDevice.iter().position(|&c| c == 0).unwrap_or(info.szDevice.len());
            let name = String::from_utf16_lossy(&info.szDevice[..len]);
            let r = info.monitorInfo.rcMonitor;
            out.push((name, [r.left, r.top, r.right, r.bottom]));
        }
        BOOL(1)
    }
    let mut out: Vec<(String, [i32; 4])> = Vec::new();
    // SAFETY: the callback only touches the Vec behind the pointer, which outlives the call.
    let _ = unsafe { EnumDisplayMonitors(None, None, Some(each), LPARAM((&mut out as *mut Vec<_>) as isize)) };
    out
}

/// After a change made on the UI thread: the first change of a burst arms the write.
pub fn note_change(cx: &mut EventCx, s: &StateHandle) {
    if s.touch() {
        cx.set_timer(TIMER_STATE, WRITE_DELAY_MS);
    }
}

/// StateDirty arrived (a placement changed): arms the write.
pub fn on_dirty(cx: &mut EventCx, s: &StateHandle) {
    let _ = s;
    cx.set_timer(TIMER_STATE, WRITE_DELAY_MS);
}

/// TIMER_STATE fired.
pub fn on_timer(s: &StateHandle) {
    s.flush();
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU32, Ordering};

    fn temp(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("tasks-state-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    fn placement(dpi: u32) -> Placement {
        Placement { monitor: "M1".into(), rect: RectP::wh(10, 20, 640, 560), dpi, maximized: false }
    }

    #[test]
    fn state_roundtrip() {
        let d = temp("rt");
        let paths = Paths { roaming: None, local: Some(d.clone()) };
        let s = load_with(&paths, "aaaa");
        s.set_view(ViewMode::Tree);
        s.set_collapsed(Section::Windows, true);
        s.set_collapsed(Section::Apps, false);
        let mut store = s.clone();
        store.save(MAIN_PLACEMENT, &Placement { maximized: true, ..placement(144) });
        s.flush();
        let back = load_with(&paths, "aaaa");
        assert_eq!(back.view(), Some(ViewMode::Tree));
        assert_eq!(back.collapsed(Section::Windows), Some(true));
        assert_eq!(back.collapsed(Section::Apps), Some(false));
        assert_eq!(back.collapsed(Section::Background), None);
        assert_eq!(back.load(MAIN_PLACEMENT), Some(Placement { maximized: true, ..placement(144) }));
        // Another monitor layout finds none and keeps the first one's placement across its own write.
        let other = load_with(&paths, "bbbb");
        assert_eq!(other.load(MAIN_PLACEMENT), None);
        let mut os = other.clone();
        os.save(MAIN_PLACEMENT, &placement(96));
        other.flush();
        assert_eq!(load_with(&paths, "aaaa").load(MAIN_PLACEMENT).map(|p| p.dpi), Some(144));
        assert_eq!(load_with(&paths, "bbbb").load(MAIN_PLACEMENT).map(|p| p.dpi), Some(96));
        // Nothing changed since: no write.
        let before = std::fs::metadata(d.join("state.json")).unwrap().modified().unwrap();
        back.flush();
        assert_eq!(std::fs::metadata(d.join("state.json")).unwrap().modified().unwrap(), before);
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn layout_change_files_under_the_new_id() {
        let d = temp("layout");
        let paths = Paths { roaming: None, local: Some(d.clone()) };
        let s = load_with(&paths, "aaaa");
        let mut store = s.clone();
        store.save(MAIN_PLACEMENT, &placement(144));
        s.flush();
        // The monitors change while the window is open.
        s.lock().layout_fn = Some(|| "bbbb".to_string());
        store.save(MAIN_PLACEMENT, &placement(96));
        s.flush();
        assert_eq!(
            load_with(&paths, "aaaa").load(MAIN_PLACEMENT).map(|p| p.dpi),
            Some(144),
            "the old entry is unchanged"
        );
        assert_eq!(load_with(&paths, "bbbb").load(MAIN_PLACEMENT).map(|p| p.dpi), Some(96));
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn state_bad_file_ignored() {
        let d = temp("bad");
        let paths = Paths { roaming: None, local: Some(d.clone()) };
        for bad in [&b"not json"[..], b"[1]", b"{\"version\": 2}", b"{\"version\": 1, \"placements\": 3}", b""] {
            std::fs::write(d.join("state.json"), bad).unwrap();
            let s = load_with(&paths, "aaaa");
            assert_eq!((s.view(), s.load(MAIN_PLACEMENT)), (None, None));
        }
        // Unknown fields are ignored; a good part of a partly wrong file is kept.
        std::fs::write(
            d.join("state.json"),
            br#"{"version":1,"x":[1],"view":"list","collapsed":{"apps":"no","windows":true}}"#,
        )
        .unwrap();
        let s = load_with(&paths, "aaaa");
        assert_eq!(
            (s.view(), s.collapsed(Section::Apps), s.collapsed(Section::Windows)),
            (Some(ViewMode::List), None, Some(true))
        );
        // Too large: refused by the cap.
        std::fs::write(d.join("state.json"), vec![b' '; 70 * 1024]).unwrap();
        assert_eq!(load_with(&paths, "aaaa").view(), None);
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn layout_id_stable_under_order() {
        let a = ("\\\\.\\DISPLAY1".to_string(), [0, 0, 1920, 1080]);
        let b = ("\\\\.\\DISPLAY2".to_string(), [1920, 0, 3840, 1080]);
        let id = layout_id(&[a.clone(), b.clone()]);
        assert_eq!(id.len(), 16);
        assert!(id.bytes().all(|c| c.is_ascii_hexdigit()));
        assert_eq!(id, layout_id(&[b.clone(), a.clone()]));
        assert_ne!(id, layout_id(std::slice::from_ref(&a)));
        let moved = ("\\\\.\\DISPLAY2".to_string(), [-1920, 0, 0, 1080]);
        assert_ne!(id, layout_id(&[a, moved]));
    }

    #[test]
    fn at_most_8_layouts() {
        let mut v: Vec<Entry> = (0..12).map(|i| Entry { id: format!("{i:016x}"), seq: i, p: placement(96) }).collect();
        prune(&mut v);
        assert_eq!(v.len(), MAX_LAYOUTS);
        assert_eq!(v.iter().map(|e| e.seq).min(), Some(4), "the four oldest went");
        // Through the file: a ninth layout written drops the oldest.
        let d = temp("eight");
        let paths = Paths { roaming: None, local: Some(d.clone()) };
        for i in 0..10 {
            let s = load_with(&paths, &format!("layout{i}"));
            let mut st = s.clone();
            st.save(MAIN_PLACEMENT, &placement(96 + i));
            s.flush();
        }
        let raw: Value = serde_json::from_slice(&std::fs::read(d.join("state.json")).unwrap()).unwrap();
        let map = raw["placements"].as_object().unwrap();
        assert_eq!(map.len(), MAX_LAYOUTS);
        assert!(map.contains_key("layout9") && !map.contains_key("layout0") && !map.contains_key("layout1"));
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn a_burst_posts_once() {
        let s = StateHandle::default();
        let posts = Arc::new(AtomicU32::new(0));
        let n = Arc::clone(&posts);
        s.attach(Box::new(move || {
            n.fetch_add(1, Ordering::Relaxed);
            true
        }));
        let p = Placement { monitor: "M1".into(), rect: RectP::wh(10, 10, 640, 560), dpi: 96, maximized: false };
        let mut store = s.clone();
        store.save(PlacementKey("main"), &p);
        store.save(PlacementKey("main"), &Placement { dpi: 144, ..p.clone() });
        assert_eq!(posts.load(Ordering::Relaxed), 1, "one StateDirty per burst");
        assert_eq!(s.load(PlacementKey("main")).map(|p| p.dpi), Some(144));
        s.flush();
        store.save(PlacementKey("main"), &p);
        assert_eq!(posts.load(Ordering::Relaxed), 2, "a new burst after the flush");
        s.set_view(ViewMode::Tree);
        s.set_collapsed(Section::Windows, true);
        assert_eq!(
            (s.view(), s.collapsed(Section::Windows), s.collapsed(Section::Apps)),
            (Some(ViewMode::Tree), Some(true), None)
        );
    }
}
