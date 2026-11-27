//! The sidebar (P3-files-app 2.2; FD-SIDE, F4.9): the pinned folders, This PC with its drives, the network places and
//! the folder tree, with the SplitView that sizes it. Everything it shows is read from Shared (pins, drives, session) and
//! from what its own jobs bring back (the tree's folders, whether a pinned folder still exists); nothing blocks.

use std::collections::{HashMap, HashSet, VecDeque};
use std::ffi::OsString;
use std::os::windows::ffi::OsStringExt;
use std::os::windows::fs::MetadataExt;

use crate::drives::{DriveInfo, DriveKind};
use crate::format::Fmt;
use crate::handoff::{self, Target};
use crate::keys::Cmd;
use crate::listing::natural::natural_cmp;
use crate::listing::{CancelToken, WidePath};
use crate::msg::JobDone;
use crate::pins;
use crate::status::{self, Notice};
use crate::tr::{k, t, tf};
use crate::window::{self, FilesScreen, Shared};
use oled_trust::frame::files::Location;
use oled_ui::model::FgRole;
use oled_ui::node::Text;
use oled_ui::widgets::{SidebarItem, SidebarList, SidebarOut, SidebarStyle, SplitOut, SplitView};
use oled_ui::{
    Anchor, CommandId, Event, EventCx, Glyph, Key, LayoutCx, MenuItem, MouseButton, PaintCx, PointP, RectP, Reply,
    WheelDelta, Widget, WidgetId,
};
use windows::Win32::System::Diagnostics::Debug::{SetThreadErrorMode, SEM_FAILCRITICALERRORS};

/// The SplitView's range and default, logical px.
pub const WIDTH_MIN: i32 = 160;
pub const WIDTH_MAX: i32 = 400;
pub const WIDTH_DEFAULT: i32 = 200;
/// Below this window width (logical px) the sidebar folds away.
pub const FOLD_BELOW: i32 = 760;
/// A tree level lists at most this many folders.
pub const MAX_CHILDREN: usize = 2_048;
/// Recent network places are shown for this many days.
pub const NETWORK_DAYS: i64 = 30;
/// Recent network places kept.
const NETWORK_KEPT: usize = 16;
/// Rows scrolled by one wheel notch.
const WHEEL_ROWS: f32 = 3.0;

const ID_LIST: WidgetId = WidgetId(60);
const ID_SPLIT: WidgetId = WidgetId(61);
const ROW_H: i32 = 28;
const INSET: i32 = 8;

/// The ids of the sidebar's own menu (picked through menu::on_pick, which hands them to on_pick below).
const CMD_OPEN: u32 = 0x5001;
const CMD_OPEN_TAB: u32 = 0x5002;
const CMD_PIN: u32 = 0x5003;
const CMD_UNPIN: u32 = 0x5004;
const CMD_REIMPORT: u32 = 0x5005;

/// What a row is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RowKind {
    Section,
    Pin,
    Drive(u8),
    /// A UNC root of the recent places.
    Net,
    /// "Network ↗".
    NetHand,
    /// "Recycle Bin ↗".
    Recycle,
    /// A folder of the tree.
    Dir,
}

/// One row of the list as the model knows it (the SidebarItem is its drawing).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Row {
    pub key: u64,
    pub kind: RowKind,
    pub depth: u16,
    pub path: Option<WidePath>,
    pub disabled: bool,
}

/// Everything build() reads.
pub struct Inputs<'a> {
    pub lang: oled_ui::UiLang,
    pub fmt: &'a Fmt,
    pub like_explorer: bool,
    pub pins: &'a [WidePath],
    /// Pins known not to exist (same_key).
    pub missing: &'a HashSet<String>,
    pub drives: &'a [DriveInfo],
    pub network: &'a [WidePath],
    pub expanded: &'a HashSet<u64>,
    pub children: &'a HashMap<u64, Vec<Box<str>>>,
    /// The row whose folders are being listed.
    pub busy: Option<u64>,
}

pub struct Built {
    pub items: Vec<SidebarItem>,
    pub rows: Vec<Row>,
}

/// A stable key: FNV-1a of the parent, the kind and the lower-cased text.
pub fn key_of(parent: u64, tag: &str, text: &str) -> u64 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for b in parent.to_le_bytes().iter().chain(tag.as_bytes()).chain(&[0]).chain(text.to_lowercase().as_bytes()) {
        h ^= u64::from(*b);
        h = h.wrapping_mul(0x0100_0000_01b3);
    }
    h
}

/// "<label> (<letter>:)", the letter alone when the drive has no label.
pub fn drive_title(d: &DriveInfo, lang: oled_ui::UiLang) -> String {
    let letter = (d.letter as char).to_string();
    match d.label.as_deref() {
        Some(label) => tf(k::FILES_SIDE_DRIVE, lang, &[("label", label), ("letter", &letter)]).into_owned(),
        None => format!("{letter}:"),
    }
}

/// The drive's root.
pub fn drive_root(letter: u8) -> Option<WidePath> {
    WidePath::parse(&format!("{}:\\", letter.to_ascii_uppercase() as char)).ok()
}

/// A folder's name for a row: its last component, else the whole path (a root).
pub fn place_name(p: &WidePath) -> String {
    p.file_name().map(String::from_utf16_lossy).unwrap_or_else(|| p.to_string_lossy())
}

/// True under 10 % free.
pub fn low_space(d: &DriveInfo) -> bool {
    matches!((d.free, d.total), (Some(f), Some(t)) if t > 0 && f.saturating_mul(10) < t)
}

fn item(key: u64, label: String, glyph: Option<Glyph>, depth: u16) -> SidebarItem {
    SidebarItem {
        key,
        label: Text::Owned(label),
        glyph,
        icon: None,
        depth,
        expandable: false,
        expanded: false,
        section: false,
        trailing: None,
        disabled: false,
        busy: false,
    }
}

fn section(tag: &str, label: Text) -> (SidebarItem, Row) {
    let key = key_of(0, "sec", tag);
    let mut it = item(key, label.into_owned(), None, 0);
    it.section = true;
    (it, Row { key, kind: RowKind::Section, depth: 0, path: None, disabled: false })
}

/// The rows of the sidebar: sections, pins, drives, network places, the two hand-offs and the open tree levels. Pure.
pub fn build(inp: &Inputs) -> Built {
    let mut out = Built { items: Vec::new(), rows: Vec::new() };
    let push = |o: &mut Built, (i, r): (SidebarItem, Row)| {
        o.items.push(i);
        o.rows.push(r);
    };
    if !inp.pins.is_empty() {
        push(&mut out, section("pinned", t(k::FILES_SIDE_PINNED, inp.lang)));
        for p in inp.pins {
            let key = key_of(0, "pin", &p.to_string_lossy());
            let gone = inp.missing.contains(&pins::same_key(&p.to_string_lossy()));
            let mut it = item(key, place_name(p), Some(Glyph::Folder), 0);
            (it.disabled, it.expandable) = (gone, !gone);
            tree_state(&mut it, inp, key);
            push(&mut out, (it, Row { key, kind: RowKind::Pin, depth: 0, path: Some(p.clone()), disabled: gone }));
            if !gone {
                children(&mut out, inp, key, p, 1);
            }
        }
    }
    push(&mut out, section("this_pc", t(k::FILES_SIDE_THIS_PC, inp.lang)));
    let drive_rows = |o: &mut Built, d: &DriveInfo| {
        let key = key_of(0, "drive", &(d.letter as char).to_string());
        let mut it = item(
            key,
            drive_title(d, inp.lang),
            Some(if d.kind == DriveKind::Network { Glyph::Network } else { Glyph::Disk }),
            0,
        );
        it.disabled = !d.ready;
        it.expandable = d.ready;
        if let (Some(free), true) = (d.free, d.kind != DriveKind::Network && d.ready) {
            let role = if low_space(d) { FgRole::Attention } else { FgRole::Tertiary };
            it.trailing = Some((Text::Owned(inp.fmt.size(free, inp.like_explorer)), role));
        }
        tree_state(&mut it, inp, key);
        let path = drive_root(d.letter);
        let row = Row { key, kind: RowKind::Drive(d.letter), depth: 0, path: path.clone(), disabled: !d.ready };
        o.items.push(it);
        o.rows.push(row);
        if let (true, Some(p)) = (d.ready, path) {
            children(o, inp, key, &p, 1);
        }
    };
    for d in inp.drives.iter().filter(|d| d.kind != DriveKind::Network) {
        drive_rows(&mut out, d);
    }
    push(&mut out, section("network", t(k::FILES_SIDE_NETWORK, inp.lang)));
    for d in inp.drives.iter().filter(|d| d.kind == DriveKind::Network) {
        drive_rows(&mut out, d);
    }
    for p in inp.network {
        let key = key_of(0, "net", &p.to_string_lossy());
        let mut it = item(key, p.to_string_lossy(), Some(Glyph::Network), 0);
        it.expandable = true;
        tree_state(&mut it, inp, key);
        push(&mut out, (it, Row { key, kind: RowKind::Net, depth: 0, path: Some(p.clone()), disabled: false }));
        children(&mut out, inp, key, p, 1);
    }
    let hand = |tag: &str, label: Text, glyph: Glyph, kind: RowKind| {
        let key = key_of(0, tag, "");
        (item(key, label.into_owned(), Some(glyph), 0), Row { key, kind, depth: 0, path: None, disabled: false })
    };
    push(&mut out, hand("nethand", t(k::FILES_SIDE_NETWORK_ARROW, inp.lang), Glyph::Globe, RowKind::NetHand));
    push(&mut out, hand("recycle", t(k::FILES_SIDE_RECYCLE_BIN, inp.lang), Glyph::Delete, RowKind::Recycle));
    out
}

fn tree_state(it: &mut SidebarItem, inp: &Inputs, key: u64) {
    it.expanded = it.expandable && inp.expanded.contains(&key);
    it.busy = inp.busy == Some(key);
}

/// The loaded folders of an expanded row, then theirs.
fn children(out: &mut Built, inp: &Inputs, parent: u64, path: &WidePath, depth: u16) {
    if !inp.expanded.contains(&parent) {
        return;
    }
    let Some(names) = inp.children.get(&parent) else { return };
    for n in names {
        let Ok(p) = path.join(&n.encode_utf16().collect::<Vec<u16>>()) else { continue };
        let key = key_of(parent, "dir", n);
        let mut it = item(key, n.to_string(), Some(Glyph::Folder), depth);
        it.expandable = true;
        tree_state(&mut it, inp, key);
        out.items.push(it);
        out.rows.push(Row { key, kind: RowKind::Dir, depth, path: Some(p.clone()), disabled: false });
        children(out, inp, key, &p, depth + 1);
    }
}

/// F6 into the sidebar: the focus goes to the current row, else to the first row that takes it.
pub fn focus(s: &mut FilesScreen, cx: &mut EventCx) {
    let sb = &s.side;
    let loc = s.tabs.active().nav.location();
    let at = current_key(&sb.rows, loc)
        .and_then(|k| sb.rows.iter().position(|r| r.key == k))
        .filter(|i| !sb.rows[*i].disabled)
        .or_else(|| sb.rows.iter().position(|r| r.kind != RowKind::Section && !r.disabled));
    if let Some(slot) = at.and_then(|i| i.checked_sub(sb.first)).filter(|i| *i < sb.fit) {
        cx.set_focus(oled_ui::NodeId::new(ID_LIST, slot as u32 + 1));
    }
}

/// The key of the deepest row whose folder holds the location.
pub fn current_key(rows: &[Row], at: &Location) -> Option<u64> {
    let Location::Fs(p) = at else { return None };
    let loc = pins::same_key(&p.to_string_lossy());
    let mut best: Option<(usize, u64)> = None;
    for r in rows {
        let Some(rp) = &r.path else { continue };
        let key = pins::same_key(&rp.to_string_lossy());
        let prefix = if key.ends_with('\\') { key.clone() } else { format!("{key}\\") };
        if (loc == key || loc.starts_with(&prefix)) && best.is_none_or(|(n, _)| key.len() > n) {
            best = Some((key.len(), r.key));
        }
    }
    best.map(|(_, k)| k)
}

/// The tree level's request in flight.
struct Job {
    req: u64,
    key: u64,
    cancel: CancelToken,
}

/// The sidebar state of a window (tests and the other regions read it).
pub fn of(s: &FilesScreen) -> &Sidebar {
    &s.side
}

pub struct Sidebar {
    split: SplitView,
    list: SidebarList,
    rows: Vec<Row>,
    /// The first row shown, and how many fit (the list has no scroll of its own).
    first: usize,
    fit: usize,
    pane: RectP,
    shown: bool,
    expanded: HashSet<u64>,
    children: HashMap<u64, Vec<Box<str>>>,
    job: Option<Job>,
    queue: VecDeque<(u64, WidePath)>,
    /// Pins: same_key -> exists; the requests of the checks in flight.
    checked: HashMap<String, bool>,
    checks: HashMap<u64, String>,
    /// A width dragged and not saved yet.
    pending_width: Option<i32>,
    menu_key: Option<u64>,
    walk: Option<WidePath>,
    scrolled_to: Option<u64>,
    /// The drive letters present and the pins (same keys) at the last bump: a missing pin is asked again only when
    /// either changed.
    seen: Option<(u32, Vec<String>)>,
}

impl std::fmt::Debug for Sidebar {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Sidebar")
    }
}

impl Default for Sidebar {
    fn default() -> Sidebar {
        Sidebar::new()
    }
}

fn new_list() -> SidebarList {
    SidebarList::new(ID_LIST, SidebarStyle { row_h: ROW_H, inset: INSET })
}

impl Sidebar {
    pub fn new() -> Sidebar {
        Sidebar {
            split: SplitView::new(ID_SPLIT, WIDTH_MIN, WIDTH_MAX, WIDTH_DEFAULT),
            list: new_list(),
            rows: Vec::new(),
            first: 0,
            fit: usize::MAX,
            pane: RectP::EMPTY,
            shown: false,
            expanded: HashSet::new(),
            children: HashMap::new(),
            job: None,
            queue: VecDeque::new(),
            checked: HashMap::new(),
            checks: HashMap::new(),
            pending_width: None,
            menu_key: None,
            walk: None,
            scrolled_to: None,
            seen: None,
        }
    }

    /// Scenes only: marks a pinned folder as found (or not) without asking the file system.
    pub(crate) fn seed_exists(&mut self, p: &WidePath, there: bool) {
        self.checked.insert(pins::same_key(&p.to_string_lossy()), there);
    }

    /// Scenes only: a row expanded with the folders it shows, without listing anything.
    pub(crate) fn seed_tree(&mut self, key: u64, names: &[&str]) {
        self.expanded.insert(key);
        self.children.insert(key, names.iter().map(|n| Box::from(*n)).collect());
    }

    /// The rows as the last layout built them.
    pub fn rows(&self) -> &[Row] {
        &self.rows
    }

    /// Whether the sidebar takes width now.
    pub fn is_shown(&self) -> bool {
        self.shown
    }

    /// The sidebar's rect, physical px (empty when folded).
    pub fn pane(&self) -> RectP {
        self.pane
    }

    pub fn is_expanded(&self, key: u64) -> bool {
        self.expanded.contains(&key)
    }

    /// The rows from 'first' that fit, for tests and the scroll keys.
    pub fn first_row(&self) -> usize {
        self.first
    }

    /// Whether a pinned folder is known not to exist (Home reads it too).
    pub fn is_missing(&self, p: &WidePath) -> bool {
        self.checked.get(&pins::same_key(&p.to_string_lossy())) == Some(&false)
    }

    fn row(&self, key: u64) -> Option<(usize, &Row)> {
        self.rows.iter().enumerate().find(|(_, r)| r.key == key)
    }
}

/// Days since 1970-01-01 of the date of the clock's local time.
fn today(shared: &Shared) -> i64 {
    let t = shared.services.clock.local_now();
    days_from_civil(i64::from(t.year), i64::from(t.month), i64::from(t.day))
}

/// Howard Hinnant's days_from_civil.
pub fn days_from_civil(y: i64, m: i64, d: i64) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let doy = (153 * (if m > 2 { m - 3 } else { m + 9 }) + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

/// The UNC roots visited in the last 30 days, newest first.
pub fn network_places(shared: &Shared) -> Vec<WidePath> {
    let now = today(shared);
    shared
        .session
        .borrow()
        .network_recent
        .iter()
        .filter(|(_, day)| now.checked_sub(*day).is_some_and(|d| d <= NETWORK_DAYS) && *day <= now.saturating_add(1))
        .filter_map(|(p, _)| WidePath::parse(p).ok())
        .collect()
}

/// The inputs from the window's state.
fn inputs_run<R>(s: &FilesScreen, f: impl FnOnce(&Inputs) -> R) -> R {
    let shared = &s.shared;
    let fmt = shared.fmt.borrow().clone();
    let (pins, network) = (pins::paths(shared), network_places(shared));
    let drives = shared.drives.borrow();
    let missing: HashSet<String> = s.side.checked.iter().filter(|(_, ok)| !**ok).map(|(k, _)| k.clone()).collect();
    let inp = Inputs {
        lang: shared.lang.get(),
        fmt: &fmt,
        like_explorer: shared.opts.borrow().sizes_like_explorer,
        pins: &pins,
        missing: &missing,
        drives: drives.all(),
        network: &network,
        expanded: &s.side.expanded,
        children: &s.side.children,
        busy: s.side.job.as_ref().map(|j| j.key),
    };
    f(&inp)
}

fn next_req(shared: &Shared) -> u64 {
    let r = shared.next_req.get();
    shared.next_req.set(r + 1);
    r
}

/// Asks whether each pinned folder exists (a Bg job on its lane; the answer is a Children with one name when it does).
fn check_pins(s: &mut FilesScreen) {
    let shared = std::rc::Rc::clone(&s.shared);
    for p in pins::paths(&shared) {
        let key = pins::same_key(&p.to_string_lossy());
        if s.side.checked.contains_key(&key) || s.side.checks.values().any(|v| *v == key) {
            continue;
        }
        let req = next_req(&shared);
        s.side.checks.insert(req, key);
        let lane = crate::bg::lane_of(&p, &shared.drives.borrow());
        shared.services.bg.run(
            s.win,
            lane,
            Box::new(move || {
                // SAFETY: this thread's error mode only; it keeps a missing medium from raising a system box.
                let _ = unsafe { SetThreadErrorMode(SEM_FAILCRITICALERRORS, None) };
                let there = std::fs::metadata(wide_path(&p)).is_ok_and(|m| m.is_dir());
                let dirs: Vec<Box<str>> = if there { vec![".".into()] } else { Vec::new() };
                Some(JobDone::Children { req, parent: p, dirs, done: true })
            }),
        );
    }
}

fn wide_path(p: &WidePath) -> std::path::PathBuf {
    std::path::PathBuf::from(OsString::from_wide(p.units()))
}

/// The folders of a directory: at most MAX_CHILDREN, natural order; hidden and system ones only when asked. None when
/// the job was cancelled.
pub fn list_dirs(path: &WidePath, hidden: bool, cancel: &CancelToken) -> Option<Vec<Box<str>>> {
    let mut names: Vec<String> = Vec::new();
    if let Ok(rd) = std::fs::read_dir(wide_path(path)) {
        for e in rd.flatten() {
            if cancel.is_cancelled() {
                return None;
            }
            let Ok(m) = e.metadata() else { continue };
            let a = m.file_attributes();
            if a & 0x10 == 0 || (!hidden && a & 0x6 != 0) {
                continue;
            }
            names.push(e.file_name().to_string_lossy().into_owned());
            if names.len() >= MAX_CHILDREN {
                break;
            }
        }
    }
    names.sort_by(|a, b| natural_cmp(a.as_bytes(), b.as_bytes()));
    (!cancel.is_cancelled()).then(|| names.into_iter().map(String::into_boxed_str).collect())
}

/// Lists the folders of row 'key' (one job at a time: another waits its turn).
fn want_children(s: &mut FilesScreen, key: u64, path: WidePath) {
    if s.side.job.is_some() {
        if !s.side.queue.iter().any(|(q, _)| *q == key) {
            s.side.queue.push_back((key, path));
        }
        return;
    }
    let shared = std::rc::Rc::clone(&s.shared);
    let req = next_req(&shared);
    let cancel = CancelToken::new();
    let (c, p, hidden) = (cancel.clone(), path.clone(), shared.opts.borrow().hidden_items);
    let lane = crate::bg::lane_of(&path, &shared.drives.borrow());
    shared.services.bg.run(
        s.win,
        lane,
        Box::new(move || {
            // SAFETY: this thread's error mode only; it keeps a missing medium from raising a system box.
            let _ = unsafe { SetThreadErrorMode(SEM_FAILCRITICALERRORS, None) };
            let dirs = list_dirs(&p, hidden, &c)?;
            Some(JobDone::Children { req, parent: p, dirs, done: true })
        }),
    );
    s.side.job = Some(Job { req, key, cancel });
}

/// A tree level arrived (or a pin's existence answer). True when the request was the sidebar's.
pub fn on_children(
    s: &mut FilesScreen,
    cx: &mut EventCx,
    req: u64,
    parent: &WidePath,
    dirs: &[Box<str>],
    done: bool,
) -> bool {
    let _ = (parent, done);
    if let Some(key) = s.side.checks.remove(&req) {
        s.side.checked.insert(key, !dirs.is_empty());
        cx.relayout();
        return true;
    }
    if s.side.job.as_ref().is_none_or(|j| j.req != req) {
        return false;
    }
    if let Some(job) = s.side.job.take() {
        if s.side.expanded.contains(&job.key) {
            s.side.children.insert(job.key, dirs.to_vec());
        }
    }
    pump_queue(s);
    continue_walk(s, cx);
    cx.relayout();
    true
}

/// Starts the next queued level, when no job runs.
fn pump_queue(s: &mut FilesScreen) {
    while s.side.job.is_none() {
        let Some((key, path)) = s.side.queue.pop_front() else { break };
        if s.side.expanded.contains(&key) {
            want_children(s, key, path);
        }
    }
}

/// Marks a row expanded and lists its folders when they are not known yet.
fn open_level(s: &mut FilesScreen, key: u64, path: WidePath) {
    s.side.expanded.insert(key);
    if !s.side.children.contains_key(&key) {
        want_children(s, key, path);
    }
}

/// Expands or collapses a row. Collapsing cancels a pending job of the row or of anything under it and drops the
/// folders it showed.
fn expand(s: &mut FilesScreen, cx: &mut EventCx, key: u64, on: bool) {
    let Some((idx, row)) = s.side.row(key).map(|(i, r)| (i, r.clone())) else { return };
    if on {
        if let Some(p) = row.path {
            open_level(s, key, p);
        }
    } else {
        let mut gone = vec![key];
        gone.extend(s.side.rows.iter().skip(idx + 1).take_while(|r| r.depth > row.depth).map(|r| r.key));
        for g in &gone {
            s.side.expanded.remove(g);
            s.side.children.remove(g);
        }
        if let Some(j) = s.side.job.as_ref().filter(|j| gone.contains(&j.key)) {
            j.cancel.cancel();
            s.side.job = None;
        }
        s.side.queue.retain(|(q, _)| !gone.contains(q));
        pump_queue(s);
    }
    cx.relayout();
}

/// Ctrl+Shift+E: expands from the drive down to the current folder, one level per job.
fn continue_walk(s: &mut FilesScreen, cx: &mut EventCx) {
    let Some(target) = s.side.walk.clone() else { return };
    let mut chain = vec![target.clone()];
    while let Some(p) = chain.last().and_then(|p| p.parent()) {
        chain.push(p);
    }
    chain.reverse();
    let root_key = s.side.rows.iter().find_map(|r| {
        let rp = r.path.as_ref()?;
        (matches!(r.kind, RowKind::Drive(_) | RowKind::Net)
            && pins::same_key(&rp.to_string_lossy()) == pins::same_key(&chain.first()?.to_string_lossy()))
        .then_some(r.key)
    });
    let Some(mut cur) = root_key else {
        s.side.walk = None;
        return;
    };
    for pair in chain.windows(2) {
        let name = pair.get(1).and_then(|p| p.file_name()).map(String::from_utf16_lossy).unwrap_or_default();
        if !s.side.expanded.contains(&cur) {
            if let Some(p) = pair.first() {
                open_level(s, cur, p.clone());
            }
        }
        let Some(list) = s.side.children.get(&cur) else {
            if s.side.job.is_none() && s.side.queue.is_empty() {
                s.side.walk = None;
            }
            return;
        };
        let Some(found) = list.iter().find(|n| n.to_lowercase() == name.to_lowercase()) else {
            s.side.walk = None;
            return;
        };
        cur = key_of(cur, "dir", found);
    }
    s.side.walk = None;
    s.side.scrolled_to = None;
    cx.relayout();
    let _ = cur;
}

/// The body rect beside the sidebar (folded below 760 px or when hidden).
pub fn layout(s: &mut FilesScreen, cx: &mut LayoutCx, r: RectP) -> RectP {
    check_pins(s);
    let visible = s.shared.opts.borrow().sidebar_visible;
    if !visible || cx.scale().logical(r.w()) < FOLD_BELOW {
        s.side.shown = false;
        s.side.pane = RectP::EMPTY;
        return r;
    }
    s.side.shown = true;
    if s.side.pending_width.is_none() {
        let w = i32::from(s.shared.opts.borrow().sidebar_width);
        s.side.split.set_first(w);
    }
    let w = cx.px(s.side.split.first()).clamp(0, r.w());
    let pane = RectP { r: r.l + w, ..r };
    let body = RectP { l: (r.l + w + 1).min(r.r), ..r };
    let built = inputs_run(s, build);
    let loc = s.tabs.active().nav.location().clone();
    let current = current_key(&built.rows, &loc);
    let rh = cx.px(ROW_H).max(1);
    let fit = ((pane.h() / rh).max(1)) as usize;
    let sb = &mut s.side;
    sb.fit = fit;
    sb.first = sb.first.min(built.rows.len().saturating_sub(fit));
    if sb.scrolled_to != current {
        sb.scrolled_to = current;
        if let Some(i) = built.rows.iter().position(|r| Some(r.key) == current) {
            if i < sb.first || i >= sb.first + fit {
                sb.first = i.saturating_sub(fit / 2).min(built.rows.len().saturating_sub(fit));
            }
        }
    }
    let end = (sb.first + fit).min(built.rows.len());
    let shown: Vec<SidebarItem> = built.items.into_iter().skip(sb.first).take(end - sb.first).collect();
    sb.rows = built.rows;
    sb.list = new_list().with_items(shown);
    sb.list.set_current(current);
    sb.pane = pane;
    sb.list.layout(cx, pane);
    sb.split.layout(cx, r);
    body
}

pub fn paint(s: &FilesScreen, cx: &mut PaintCx) {
    if !s.side.shown {
        return;
    }
    cx.with_clip(s.side.pane, &mut |cx| s.side.list.paint(cx));
    s.side.split.paint(cx);
}

/// SidebarOut and the splitter.
pub fn event(s: &mut FilesScreen, cx: &mut EventCx, ev: &Event) -> Option<Reply> {
    if matches!(ev, Event::PointerUp { .. }) {
        flush_width(s);
    }
    if !s.side.shown {
        return None;
    }
    if let Some(out) = s.side.split.event(cx, ev) {
        let dragging = matches!(ev, Event::DragStart { .. } | Event::DragMove { .. });
        let w = match out {
            SplitOut::Moved(w) => w,
            SplitOut::Reset => WIDTH_DEFAULT,
        };
        s.side.pending_width = Some(w);
        if !dragging {
            flush_width(s);
        }
        return Some(Reply::Relayout);
    }
    if let Some(r) = scroll_keys(s, cx, ev) {
        return Some(r);
    }
    if let Some(out) = s.side.list.event(cx, ev) {
        let key_at = |s: &FilesScreen, k: u64| s.side.row(k).map(|(_, r)| r.clone());
        return Some(match out {
            SidebarOut::Activated(k) => match key_at(s, k) {
                Some(r) => activate(s, cx, &r),
                None => Reply::None,
            },
            SidebarOut::Expand(k, on) => {
                expand(s, cx, k, on);
                Reply::Relayout
            }
            SidebarOut::ContextMenu(k, at) => open_menu(s, cx, Some(k), at),
        });
    }
    match ev {
        Event::Wheel { pos, delta, .. } if s.side.pane.contains(*pos) => {
            let rows = match delta {
                WheelDelta::Lines(l) => -(l * WHEEL_ROWS).round() as isize,
                WheelDelta::Pixels(p) => -((p / ROW_H as f32).round() as isize),
            };
            Some(scroll_by(s, rows))
        }
        Event::Click { pos, button: MouseButton::Right, .. } if s.side.pane.contains(*pos) => {
            // The click landed where the list takes none: a missing folder's row, a section header or the empty rest.
            let rh = cx.px(ROW_H).max(1);
            let i = s.side.first + ((pos.y - s.side.pane.t) / rh).max(0) as usize;
            let key = s.side.rows.get(i).filter(|r| r.kind == RowKind::Pin).map(|r| r.key);
            Some(open_menu(s, cx, key, *pos))
        }
        _ => None,
    }
}

fn flush_width(s: &mut FilesScreen) {
    if let Some(w) = s.side.pending_width.take() {
        let w = w.clamp(WIDTH_MIN, WIDTH_MAX) as u16;
        s.shared.services.options.modify(&mut |o| o.sidebar_width = w);
        *s.shared.opts.borrow_mut() = s.shared.services.options.get();
    }
}

fn scroll_by(s: &mut FilesScreen, rows: isize) -> Reply {
    let max = s.side.rows.len().saturating_sub(s.side.fit);
    let to = (s.side.first as isize + rows).clamp(0, max as isize) as usize;
    if to == s.side.first {
        return Reply::None;
    }
    s.side.first = to;
    Reply::Relayout
}

/// Up and Down at the edge of the rows that are shown move the window of rows, and the focus with it.
fn scroll_keys(s: &mut FilesScreen, cx: &mut EventCx, ev: &Event) -> Option<Reply> {
    let Event::Key { key, down: true, .. } = ev else { return None };
    let target = cx.target().filter(|n| n.owner() == ID_LIST && n.slot() >= 1)?;
    let slot = target.slot() as usize;
    let shown = s.side.fit.min(s.side.rows.len() - s.side.first.min(s.side.rows.len()));
    let (edge, step): (bool, isize) = match *key {
        Key::DOWN => (slot == shown && s.side.first + shown < s.side.rows.len(), 1),
        Key::UP => (slot == 1 && s.side.first > 0, -1),
        _ => return None,
    };
    if !edge {
        return None;
    }
    cx.consume();
    let r = scroll_by(s, step);
    cx.set_focus(target);
    Some(r)
}

fn activate(s: &mut FilesScreen, cx: &mut EventCx, r: &Row) -> Reply {
    match r.kind {
        RowKind::Recycle => handoff::hand_off(s, cx, Target::RecycleBin, false),
        RowKind::NetHand => handoff::hand_off(s, cx, Target::Network, false),
        RowKind::Section => Reply::None,
        _ => match &r.path {
            Some(p) if !r.disabled => {
                window::navigate(s, cx, Location::Fs(p.clone()), None);
                Reply::Redraw
            }
            _ => Reply::None,
        },
    }
}

fn open_menu(s: &mut FilesScreen, cx: &mut EventCx, key: Option<u64>, at: PointP) -> Reply {
    let lang = s.shared.lang.get();
    let row = key.and_then(|k| s.side.row(k)).map(|(_, r)| r.clone());
    s.side.menu_key = row.as_ref().map(|r| r.key);
    let custom = |id: u32, label: Text, enabled: bool, note: Option<Text>| MenuItem::Custom {
        id: CommandId(id),
        label,
        glyph: None,
        chord: None,
        enabled,
        checked: None,
        note,
        danger: false,
    };
    let mut items = Vec::new();
    if let Some(Row { path: Some(p), kind, disabled, .. }) = &row {
        items.push(custom(CMD_OPEN, t(k::FILES_MENU_OPEN, lang), !disabled, None));
        items.push(custom(CMD_OPEN_TAB, t(k::FILES_MENU_OPEN_NEW_TAB, lang), !disabled, None));
        items.push(MenuItem::Separator);
        if pins::contains(&s.shared, p) || *kind == RowKind::Pin {
            items.push(custom(CMD_UNPIN, t(k::FILES_MENU_UNPIN, lang), true, None));
        } else {
            items.push(custom(CMD_PIN, t(k::FILES_MENU_PIN, lang), true, None));
        }
    }
    if row.as_ref().is_none_or(|r| r.kind == RowKind::Pin) {
        if !items.is_empty() {
            items.push(MenuItem::Separator);
        }
        let on = pins::reimport_enabled();
        let note = (!on).then(|| t(k::FILES_MENU_NOT_IN_BETA, lang));
        items.push(custom(CMD_REIMPORT, t(k::FILES_SIDE_REIMPORT, lang), on, note));
    }
    cx.open_menu(items, Anchor::Point(at));
    Reply::Redraw
}

/// A pick of the sidebar's menu; None for any other id.
pub fn on_pick(s: &mut FilesScreen, cx: &mut EventCx, id: CommandId) -> Option<Reply> {
    if !(CMD_OPEN..=CMD_REIMPORT).contains(&id.0) {
        return None;
    }
    let row = s.side.menu_key.and_then(|k| s.side.row(k)).map(|(_, r)| r.clone());
    let path = row.as_ref().and_then(|r| r.path.clone());
    Some(match (id.0, path) {
        (CMD_OPEN, Some(p)) => {
            window::navigate(s, cx, Location::Fs(p), None);
            Reply::Redraw
        }
        (CMD_OPEN_TAB, Some(p)) => {
            if !crate::tab::open_background(s, cx, Location::Fs(p)) {
                let text = t(k::FILES_MENU_NOT_IN_BETA, s.shared.lang.get());
                status::notice(s, cx, Notice::new(text));
            }
            Reply::Redraw
        }
        (CMD_PIN, Some(p)) => {
            pins::add(&s.shared, &p);
            Reply::Relayout
        }
        (CMD_UNPIN, Some(p)) => {
            pins::remove(&s.shared, &p);
            Reply::Relayout
        }
        (CMD_REIMPORT, _) => {
            pins::reimport(&s.shared);
            Reply::None
        }
        _ => Reply::None,
    })
}

/// The root of a UNC path, "\\server\share".
fn unc_root(p: &WidePath) -> Option<WidePath> {
    let mut cur = p.clone();
    while !cur.is_root() {
        cur = cur.parent()?;
    }
    cur.is_unc().then_some(cur)
}

/// network_recent for a UNC path (today's date, newest first, 16 kept); the session is written only when it changed.
pub fn on_navigated(shared: &Shared, to: &Location) {
    let Location::Fs(p) = to else { return };
    let Some(root) = unc_root(p) else { return };
    let (text, day) = (root.to_string_lossy(), today(shared));
    let mut session = shared.session.borrow_mut();
    let key = pins::same_key(&text);
    if session.network_recent.first().is_some_and(|(t, d)| pins::same_key(t) == key && *d == day) {
        return;
    }
    session.network_recent.retain(|(t, _)| pins::same_key(t) != key);
    session.network_recent.insert(0, (text, day));
    session.network_recent.truncate(NETWORK_KEPT);
    if !session.save(&*shared.services.own) {
        oled_sys::log!("files: session.json was not written");
    }
}

/// Drives or pins changed: a pin that was missing is asked again.
pub fn on_bump(s: &mut FilesScreen, cx: &mut EventCx) {
    let mask = s
        .shared
        .drives
        .borrow()
        .all()
        .iter()
        .filter_map(|d| d.letter.to_ascii_uppercase().checked_sub(b'A').filter(|n| *n < 26))
        .fold(0u32, |m, n| m | 1 << n);
    let pins: Vec<String> = pins::paths(&s.shared).iter().map(|p| pins::same_key(&p.to_string_lossy())).collect();
    let now = Some((mask, pins));
    if s.side.seen != now {
        // The last answers stay until then: a missing pin is not drawn enabled while a check is in flight.
        s.side.checked.retain(|_, there| *there);
        s.side.seen = now;
    }
    cx.relayout();
}

/// ExpandTree (Ctrl+Shift+E) and Sidebar (Ctrl+B).
pub fn cmd(s: &mut FilesScreen, cx: &mut EventCx, c: Cmd) -> Reply {
    match c {
        Cmd::Sidebar => {
            let on = !s.shared.opts.borrow().sidebar_visible;
            s.shared.services.options.modify(&mut |o| o.sidebar_visible = on);
            *s.shared.opts.borrow_mut() = s.shared.services.options.get();
            for w in s.shared.main_windows() {
                s.shared.hub.post_to(w, crate::msg::Msg::Fwd(crate::msg::Fwd::Bump));
            }
            Reply::Relayout
        }
        Cmd::ExpandTree => {
            if let Location::Fs(p) = s.tabs.active().nav.location().clone() {
                s.side.walk = Some(p);
                continue_walk(s, cx);
            }
            Reply::Relayout
        }
        _ => Reply::None,
    }
}
