//! Live changes (P3-files-app 2.2; FC-LIVE fills it, F3.9, F3.10): one watch per window's active tab, diffs
//! resolved off the UI thread, our own operations' changes applied at once.

use oled_trust::frame::files::{Change, OpId};
use oled_ui::EventCx;

use crate::drives;
use crate::icons;
use crate::listing::{Diff, Gen, TabId};
use crate::msg::Fwd;
use crate::watch::WatchEvent;
use crate::window::{FilesScreen, Shared};

/// Registers the active tab's watch in Shared.watch_owner. Filled by FC-LIVE; until then nothing.
pub fn watch_active(s: &mut FilesScreen, cx: &mut EventCx) {
    let _ = (s, cx);
}

/// Removes this window's watch. Filled by FC-LIVE; until then nothing.
pub fn unwatch(s: &mut FilesScreen) {
    let _ = s;
}

/// WindowActive(true): a tab whose watch is Limited refreshes on every activation of its window (F3.9). Filled by
/// FC-LIVE; until then nothing.
pub fn on_activate(s: &mut FilesScreen, cx: &mut EventCx) {
    let _ = (s, cx);
}

/// Every watcher event: Changes, Rescan, Limited and Gone go to the window that owns the watch; Drives rescans the
/// drives and AssocChanged refreshes class icons and type names, both then bump every main window. FC-LIVE keeps this
/// routing.
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
                drives::scan(shared);
                bump = true;
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

/// Fwd::Watch. Filled by FC-LIVE; until then nothing.
pub fn on_watch(s: &mut FilesScreen, cx: &mut EventCx, ev: WatchEvent) {
    let _ = (s, cx, ev);
}

/// Filled by FC-LIVE; until then nothing.
pub fn on_resolved(s: &mut FilesScreen, cx: &mut EventCx, tab: TabId, gen: Gen, diff: Diff) {
    let _ = (s, cx, tab, gen, diff);
}

/// Fwd::Own; calls rename::on_created for Created. Filled by FC-LIVE; until then nothing.
pub fn own_changes(s: &mut FilesScreen, cx: &mut EventCx, op: OpId, changes: &[Change]) {
    let _ = (s, cx, op, changes);
}
