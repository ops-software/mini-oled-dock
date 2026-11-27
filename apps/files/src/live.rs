//! Live changes (P3-files-app 2.2; FC-LIVE fills it, F3.9, F3.10): one watch per window's active tab, diffs
//! resolved off the UI thread, our own operations' changes applied at once.

use std::collections::{HashMap, HashSet};

use oled_trust::frame::files::{Change, ChangeKind, Location, OpId};
use oled_ui::model::{KeySet, ListModel, ScrollTo, Selection};
use oled_ui::EventCx;

use crate::icons;
use crate::listing::{stat_one, Anchor, Diff, Gen, TabId, WidePath};
use crate::model::{id_of, key_of};
use crate::msg::{Fwd, JobDone};
use crate::status::{self, Notice};
use crate::tr::{k, t, tf};
use crate::watch::{resolve, ChangeAction, RawChange, WatchEvent, WatchId};
use crate::window::{self, FilesScreen, Shared};

/// How long the names of our own changes are remembered, so the watcher's copies of them are dropped.
const RECENT_MS: u64 = 2_000;

/// Generations of the diffs our own changes produce start here (a watch's generation is a navigation count, far below).
const OWN_BASE: u64 = 1 << 62;

/// FILE_ATTRIBUTE_DIRECTORY.
const ATTR_DIR: u32 = 0x10;

/// The watch of the window's active tab.
struct Watched {
    tab: TabId,
    id: WatchId,
    gen: Gen,
    dir: WidePath,
}

/// What a diff of our own change brings back to the window that asked for it.
struct OwnJob {
    tab: TabId,
    dir: WidePath,
    /// The names that become the selection (only in the window the change was made in).
    select: Vec<Box<[u8]>>,
    /// A new folder of this change: its rename starts when its row is there.
    created: Option<(OpId, WidePath)>,
}

/// The live state of one window.
#[derive(Default)]
pub struct LiveState {
    watch: Option<Watched>,
    /// Tabs whose watch fell back to shell notifications (or failed): they refresh on every activation.
    limited: HashSet<TabId>,
    /// (name, is a removal, when): our own changes of the last 2 s.
    recent: Vec<(Box<[u8]>, bool, u64)>,
    own: HashMap<u64, OwnJob>,
    next_own: u64,
    /// The walk up from a folder that is gone: (request, tab, the folder).
    gone: Option<(u64, TabId, WidePath)>,
}

/// Registers the active tab's watch in Shared.watch_owner (the same folder keeps its watch). A tab switch or a
/// navigation to another folder unwatches the old one first; a page that is no folder holds no watch.
pub fn watch_active(s: &mut FilesScreen, cx: &mut EventCx) {
    let t = s.tabs.active();
    let Location::Fs(dir) = t.nav.location().clone() else {
        unwatch(s);
        return;
    };
    let (tab, gen) = (t.id, t.nav.gen());
    if s.live.watch.as_ref().is_some_and(|w| w.tab == tab && w.dir == dir) {
        return;
    }
    unwatch(s);
    // A failed watch of an earlier folder of this tab must not leave its refresh-on-activation behind.
    s.live.limited.remove(&tab);
    match s.shared.services.watch.watch(&dir, gen) {
        Ok(id) => {
            s.shared.watch_owner.borrow_mut().insert(id, s.win);
            if let Some(t) = s.tabs.by_id_mut(tab) {
                t.watch = Some(id);
            }
            s.live.watch = Some(Watched { tab, id, gen, dir });
        }
        Err(e) => {
            oled_sys::log!("files: no live updates for a folder: {e:?}");
            note_limited(s, cx, tab);
        }
    }
}

/// Removes this window's watch.
pub fn unwatch(s: &mut FilesScreen) {
    if let Some(w) = s.live.watch.take() {
        s.shared.services.watch.unwatch(w.id);
        s.shared.watch_owner.borrow_mut().remove(&w.id);
        s.live.limited.remove(&w.tab);
        if let Some(t) = s.tabs.by_id_mut(w.tab) {
            t.watch = None;
        }
    }
}

/// WindowActive(true): a tab whose watch is Limited refreshes on every activation of its window (F3.9).
pub fn on_activate(s: &mut FilesScreen, cx: &mut EventCx) {
    let id = s.tabs.active().id;
    if s.live.limited.contains(&id) && !s.fresh.contains_key(&id) {
        window::revalidate(s, cx, id);
    }
}

/// Every watcher event: Changes, Rescan, Limited and Gone go to the window that owns the watch; Drives rescans the
/// drives (their facts bump the windows when they arrive) and AssocChanged refreshes class icons and type names, then
/// bumps every main window.
pub fn drain(shared: &Shared) -> Vec<(u32, Fwd)> {
    let mut out = Vec::new();
    let mut bump = false;
    for ev in shared.services.watch.drain() {
        let id = match &ev {
            WatchEvent::Changes { id, .. }
            | WatchEvent::Rescan { id, .. }
            | WatchEvent::Limited { id, .. }
            | WatchEvent::Gone { id, .. } => Some(*id),
            WatchEvent::Drives(_) => {
                crate::drives::scan(shared);
                None
            }
            WatchEvent::AssocChanged => {
                icons::on_assoc_changed(shared);
                bump = true;
                None
            }
        };
        if let Some(win) = id.and_then(|id| shared.watch_owner.borrow().get(&id).copied()) {
            out.push((win, Fwd::Watch(ev)));
        }
    }
    if bump {
        out.extend(shared.main_windows().into_iter().map(|w| (w, Fwd::Bump)));
    }
    out
}

fn note_limited(s: &mut FilesScreen, cx: &mut EventCx, tab: TabId) {
    s.live.limited.insert(tab);
    let lang = cx.lang();
    status::notice(s, cx, Notice::new(t(k::FILES_STATUS_LIVE_LIMITED, lang)));
}

fn is_removal(a: ChangeAction) -> bool {
    matches!(a, ChangeAction::Removed | ChangeAction::RenamedFrom)
}

fn forget_old(s: &mut FilesScreen) {
    let now = s.shared.services.clock.now_ms();
    s.live.recent.retain(|r| now.saturating_sub(r.2) < RECENT_MS);
}

fn remember(s: &mut FilesScreen, name: &[u8], removal: bool) {
    let now = s.shared.services.clock.now_ms();
    s.live.recent.push((Box::from(name), removal, now));
}

/// Fwd::Watch.
pub fn on_watch(s: &mut FilesScreen, cx: &mut EventCx, ev: WatchEvent) {
    let Some((tab, wid, wgen, dir)) = s.live.watch.as_ref().map(|w| (w.tab, w.id, w.gen, w.dir.clone())) else {
        return;
    };
    match ev {
        WatchEvent::Changes { id, gen, items } if id == wid && gen == wgen => {
            forget_old(s);
            // Our own changes were applied at once: the same name with the same action is not looked at again.
            let items: Vec<RawChange> = items
                .into_iter()
                .filter(|c| {
                    let removal = is_removal(c.action);
                    !s.live.recent.iter().any(|(n, r, _)| *r == removal && **n == *c.name)
                })
                .collect();
            if items.is_empty() {
                return;
            }
            let lane = crate::bg::lane_of(&dir, &s.shared.drives.borrow());
            let job: crate::bg::JobFn = Box::new(move || {
                let diff = resolve(&dir, gen, &items);
                Some(JobDone::Resolved { tab, gen, diff })
            });
            s.shared.services.bg.run(s.win, lane, job);
        }
        WatchEvent::Rescan { id, gen } if id == wid && gen == wgen => {
            if !s.fresh.contains_key(&tab) {
                window::revalidate(s, cx, tab);
            }
        }
        WatchEvent::Limited { id, gen } if id == wid && gen == wgen => note_limited(s, cx, tab),
        WatchEvent::Gone { id, gen } if id == wid && gen == wgen => start_gone(s, tab, dir),
        _ => {}
    }
}

const GONE_REQ_BIT: u64 = 1 << 63;

/// The watched folder is gone: the nearest folder above it that still exists is found on the Bg lane.
fn start_gone(s: &mut FilesScreen, tab: TabId, dir: WidePath) {
    if s.live.gone.is_some() {
        return;
    }
    s.live.next_own += 1;
    // The high bit keeps this id apart from the small counters of the other Stat requests (the path line).
    let req = GONE_REQ_BIT | s.live.next_own;
    s.live.gone = Some((req, tab, dir.clone()));
    let lane = crate::bg::lane_of(&dir, &s.shared.drives.borrow());
    let job: crate::bg::JobFn = Box::new(move || {
        let found = nearest_existing(&dir);
        let ok = found.is_some();
        Some(JobDone::Stat { req, path: found.unwrap_or(dir), found: ok.then_some(true) })
    });
    s.shared.services.bg.run(s.win, lane, job);
}

fn is_folder(p: &WidePath) -> bool {
    use std::os::windows::ffi::OsStringExt;
    if let (Some(parent), Some(name)) = (p.parent(), p.file_name()) {
        if parent != *p {
            let mut wtf8 = Vec::new();
            crate::listing::collate::utf16_to_wtf8(name, &mut wtf8);
            return matches!(stat_one(&parent, &wtf8), Ok(Some(f)) if f.e.attrs & ATTR_DIR != 0);
        }
    }
    // A drive root has no name to look up.
    std::fs::metadata(std::ffi::OsString::from_wide(p.units())).is_ok_and(|m| m.is_dir())
}

fn nearest_existing(dir: &WidePath) -> Option<WidePath> {
    let mut p = dir.clone();
    loop {
        let up = p.parent()?;
        if up == p {
            return None;
        }
        if is_folder(&up) {
            return Some(up);
        }
        p = up;
    }
}

/// JobDone::Stat of the walk up from a folder that is gone: the tab goes to the nearest folder that exists (Home when
/// none does) with the notice state.gone. False when the reply is not ours.
pub fn on_stat(s: &mut FilesScreen, cx: &mut EventCx, req: u64, path: &WidePath, found: Option<bool>) -> bool {
    let Some((r, tab, dir)) = s.live.gone.clone() else { return false };
    if r != req {
        return false;
    }
    s.live.gone = None;
    let here = s.tabs.active();
    if here.id != tab || *here.nav.location() != Location::Fs(dir.clone()) {
        return true;
    }
    let name = dir
        .file_name()
        .map(String::from_utf16_lossy)
        .filter(|n| !n.is_empty())
        .unwrap_or_else(|| dir.to_string_lossy());
    let to = if found == Some(true) { Location::Fs(path.clone()) } else { Location::Home };
    window::navigate(s, cx, to, None);
    let lang = cx.lang();
    status::notice(s, cx, Notice::new(tf(k::FILES_STATE_GONE, lang, &[("name", &name)])));
    true
}

/// The first visible row with its offset: what a change keeps in place.
fn anchor_of(t: &crate::tab::Tab) -> Option<Anchor> {
    let m = t.table.model();
    let first = t.table.visible_rows().start;
    (first < ListModel::len(m)).then(|| Anchor {
        id: id_of(ListModel::key(m, first)),
        dy: t.table.row_rect(first).map_or(0, |r| r.t - t.table.viewport().t),
    })
}

/// Applies a diff to the tab's shown listing, keeping the row the eye is on.
fn apply_diff(s: &mut FilesScreen, cx: &mut EventCx, tab: TabId, diff: &Diff) {
    let Some(t) = s.tabs.by_id_mut(tab) else { return };
    let anchor = anchor_of(t);
    let (stats, _) = t.table.model_mut().listing_mut().apply(diff, anchor);
    if stats.added + stats.removed + stats.changed == 0 {
        return;
    }
    t.table.model_changed(cx, true);
    s.visible_due = true;
}

/// JobDone::Resolved: a watcher diff (checked against the watch's generation) or a diff of our own change (checked
/// against the folder it was made for), applied with the anchor; the selection keeps what is still there.
pub fn on_resolved(s: &mut FilesScreen, cx: &mut EventCx, tab: TabId, gen: Gen, diff: Diff) {
    let own = (gen.0 >= OWN_BASE).then(|| s.live.own.remove(&(gen.0 - OWN_BASE))).flatten();
    let dir = match (&own, &s.live.watch) {
        (Some(o), _) if o.tab == tab => o.dir.clone(),
        (None, Some(w)) if gen.0 < OWN_BASE && w.tab == tab && w.gen == gen => w.dir.clone(),
        _ => return,
    };
    let Some(t) = s.tabs.by_id(tab) else { return };
    let l = t.table.model().listing();
    if *l.dir() != dir {
        return;
    }
    if !l.done() || diff.rescan {
        if !s.fresh.contains_key(&tab) && !s.pending.contains_key(&tab) {
            window::revalidate(s, cx, tab);
        }
        return;
    }
    apply_diff(s, cx, tab, &diff);
    let Some(o) = own else { return };
    if let Some((op, p)) = &o.created {
        if crate::rename::on_created(s, cx, *op, p) {
            return;
        }
    }
    select_names(s, cx, tab, &o.select);
}

/// The names that are there become the selection, the first one the focus, scrolled into view.
fn select_names(s: &mut FilesScreen, cx: &mut EventCx, tab: TabId, names: &[Box<[u8]>]) {
    let Some(t) = s.tabs.by_id_mut(tab) else { return };
    let keys: Vec<_> = names.iter().filter_map(|n| t.table.model().listing().find(n)).map(key_of).collect();
    let Some(&first) = keys.first() else { return };
    let sel = if keys.len() == 1 {
        Selection::Single(first)
    } else {
        let mut set = KeySet::new();
        for k in &keys {
            set.insert(*k);
        }
        Selection::Keys(set)
    };
    t.table.set_selection(cx, sel);
    t.table.set_focused(cx, first, ScrollTo::Nearest);
}

fn wtf8_name(p: &WidePath) -> Option<Box<[u8]>> {
    let mut out = Vec::new();
    crate::listing::collate::utf16_to_wtf8(p.file_name()?, &mut out);
    (!out.is_empty()).then(|| out.into_boxed_slice())
}

/// Fwd::Own: what an operation of ours changed, in every tab of this window that shows the folder it happened in.
/// Removals apply at once; what was created or came in under a new name is read on the Bg lane and applied when it
/// is back. The names are remembered for 2 s so the watcher's copies of them are dropped.
pub fn own_changes(s: &mut FilesScreen, cx: &mut EventCx, op: OpId, changes: &[Change]) {
    forget_old(s);
    // The operation was asked for in the window that was active (OpsModel names no window yet).
    let active = s.shared.active_win.get();
    let mine = active == 0 || active == s.win;
    let shown: Vec<(TabId, WidePath)> = s
        .tabs
        .iter()
        .filter(|t| t.table.model().listing().done())
        .map(|t| (t.id, t.table.model().listing().dir().clone()))
        .collect();
    for (tab, dir) in shown {
        let mut removed = Diff::new(Gen(0));
        let mut upserts: Vec<Box<[u8]>> = Vec::new();
        let mut created = None;
        for c in changes {
            let (gone, came) = match c.kind {
                ChangeKind::Renamed | ChangeKind::Moved => (c.from.as_ref(), c.to.as_ref()),
                ChangeKind::Recycled | ChangeKind::Deleted => (c.from.as_ref(), None),
                ChangeKind::Created | ChangeKind::Copied => (None, c.to.as_ref().or(c.from.as_ref())),
            };
            let in_dir = |p: &&WidePath| p.parent().as_ref() == Some(&dir);
            if let Some(name) = gone.filter(in_dir).and_then(wtf8_name) {
                removed.remove(&name);
                remember(s, &name, true);
            }
            if let Some(p) = came.filter(in_dir) {
                if let Some(name) = wtf8_name(p) {
                    remember(s, &name, false);
                    upserts.push(name);
                    if c.kind == ChangeKind::Created {
                        created = Some((op, p.clone()));
                    }
                }
            }
        }
        if !removed.is_empty() {
            apply_diff(s, cx, tab, &removed);
        }
        if upserts.is_empty() {
            continue;
        }
        s.live.next_own += 1;
        let id = s.live.next_own;
        let gen = Gen(OWN_BASE + id);
        let select = if mine { upserts.clone() } else { Vec::new() };
        s.live.own.insert(id, OwnJob { tab, dir: dir.clone(), select, created });
        let lane = crate::bg::lane_of(&dir, &s.shared.drives.borrow());
        let job: crate::bg::JobFn = Box::new(move || {
            let mut diff = Diff::new(gen);
            for name in &upserts {
                match stat_one(&dir, name) {
                    Ok(Some(f)) => diff.upsert(&f.name, f.e),
                    Ok(None) => diff.remove(name),
                    Err(_) => {}
                }
            }
            Some(JobDone::Resolved { tab, gen, diff })
        });
        s.shared.services.bg.run(s.win, lane, job);
    }
}
