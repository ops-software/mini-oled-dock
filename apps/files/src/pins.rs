//! Pinned folders (P3-files-app 2.2; F4.8): our own list in files.json, imported once from File Explorer's Quick access
//! through the shell host, and only when the FS-2 record allows it. Nothing is ever written to Explorer.

use std::collections::HashMap;
use std::sync::{Mutex, PoisonError};

use oled_trust::frame::files::{ShellEvt, ShellReq};

use crate::listing::WidePath;
use crate::msg::{Fwd, Msg};
use crate::options::FilesOptions;
use crate::services::ReqOwner;
use crate::shell_client::ShellReply;
use crate::window::Shared;

/// Set true by the FS-2 record only (an orchestrator commit).
pub const PINS_IMPORT_OK: bool = false;

/// The most pins kept.
pub const MAX_PINS: usize = 64;

/// A request in flight: the folders its frames brought so far, and whether it replaces a list that exists.
struct Pending {
    list: Vec<WidePath>,
    reimport: bool,
}

/// The imports in flight, by window state (the address of its Shared) and request: the frames of a request arrive one by
/// one and are applied at its end. One import at a time per Shared.
static PENDING: Mutex<Option<HashMap<(usize, u32), Pending>>> = Mutex::new(None);

fn with_pending<R>(f: impl FnOnce(&mut HashMap<(usize, u32), Pending>) -> R) -> R {
    let mut g = PENDING.lock().unwrap_or_else(PoisonError::into_inner);
    f(g.get_or_insert_with(HashMap::new))
}

fn me(shared: &Shared) -> usize {
    std::ptr::from_ref(shared) as usize
}

/// First frame: QuickAccessPins when PINS_IMPORT_OK and not options.pins_imported.
pub fn import_if_due(shared: &Shared) {
    import_if_due_with(shared, PINS_IMPORT_OK);
}

/// import_if_due with the FS-2 answer given: true when a request went out.
pub fn import_if_due_with(shared: &Shared, import_ok: bool) -> bool {
    import_ok && !shared.opts.borrow().pins_imported && request(shared, false)
}

/// "Re-import from File Explorer": enabled only when the FS-2 record allows it.
pub fn reimport_enabled() -> bool {
    PINS_IMPORT_OK
}

/// Replaces the list with a fresh import; true when a request went out (never while the constant is false).
pub fn reimport(shared: &Shared) -> bool {
    reimport_with(shared, PINS_IMPORT_OK)
}

/// reimport with the FS-2 answer given.
pub fn reimport_with(shared: &Shared, import_ok: bool) -> bool {
    import_ok && request(shared, true)
}

fn request(shared: &Shared, reimport: bool) -> bool {
    let in_flight = shared.req_owner.borrow().values().any(|o| matches!(o, ReqOwner::Pins));
    if in_flight {
        return false;
    }
    // An entry left by a request nobody answered (or by a window state that was freed) is not in flight.
    with_pending(|p| p.retain(|k, _| k.0 != me(shared)));
    let shell = &shared.services.shell;
    let req = shell.next_req();
    shared.req_owner.borrow_mut().insert(req, ReqOwner::Pins);
    with_pending(|p| p.insert((me(shared), req.0), Pending { list: Vec::new(), reimport }));
    shell.request(ShellReq::QuickAccessPins { req });
    true
}

/// ReqOwner::Pins: the frames of the import are gathered and applied at ReqDone; true when the list changed. A failed
/// or lost request leaves the list and pins_imported as they were (the next start asks again).
pub fn on_reply(shared: &Shared, reply: &ShellReply) -> bool {
    match reply {
        ShellReply::Evt(ShellEvt::Pins { req, pins }) => {
            with_pending(|p| {
                if let Some(pending) = p.get_mut(&(me(shared), req.0)) {
                    gather(&mut pending.list, pins);
                }
            });
            false
        }
        ShellReply::Evt(ShellEvt::ReqDone { req }) => {
            let done = with_pending(|p| p.remove(&(me(shared), req.0)));
            match done {
                Some(done) => {
                    apply_import(shared, done.list, done.reimport);
                    true
                }
                None => false,
            }
        }
        ShellReply::Evt(ShellEvt::Failed { req, .. }) | ShellReply::Fallback { req, .. } => {
            with_pending(|p| p.remove(&(me(shared), req.0)));
            false
        }
        _ => false,
    }
}

/// Adds the folders of one frame to what the request gathered: the same rule as the list's (duplicates dropped, at
/// most MAX_PINS), so a host that never ends its frames cannot grow the gathering without limit.
fn gather(list: &mut Vec<WidePath>, frame: &[WidePath]) {
    for p in frame {
        if list.len() >= MAX_PINS {
            return;
        }
        let key = same_key(&p.to_string_lossy());
        if !list.iter().any(|l| same_key(&l.to_string_lossy()) == key) {
            list.push(p.clone());
        }
    }
}

/// The folders of the import become the list: order kept, duplicates dropped, at most MAX_PINS; the import is marked
/// done, the options are saved through the service.
fn apply_import(shared: &Shared, list: Vec<WidePath>, reimport: bool) {
    let _ = reimport;
    let mut texts: Vec<String> = Vec::new();
    for p in &list {
        push_unique(&mut texts, p.to_string_lossy());
    }
    change(shared, &mut |o| {
        o.pins = texts.clone();
        o.pins_imported = true;
    });
}

/// The comparison key of a folder: case-insensitive, without a closing backslash (a drive root keeps it).
pub fn same_key(path: &str) -> String {
    let t = path.trim_end_matches('\\');
    let t = if t.ends_with(':') { path.trim_end_matches('\\').to_string() + "\\" } else { t.to_string() };
    t.to_lowercase()
}

fn push_unique(texts: &mut Vec<String>, path: String) -> bool {
    let key = same_key(&path);
    if texts.len() >= MAX_PINS || texts.iter().any(|t| same_key(t) == key) {
        return false;
    }
    texts.push(path);
    true
}

/// Applies `f` through the options service, refreshes the window's copy and bumps every window (a pin is shown by
/// every sidebar and by Home).
fn change(shared: &Shared, f: &mut dyn FnMut(&mut FilesOptions)) {
    shared.services.options.modify(f);
    *shared.opts.borrow_mut() = shared.services.options.get();
    for w in shared.main_windows() {
        shared.hub.post_to(w, Msg::Fwd(Fwd::Bump));
    }
}

/// Appends a folder to our list; false when it is there already, the list is full, or it is not a folder path.
pub fn add(shared: &Shared, p: &WidePath) -> bool {
    let mut texts = shared.opts.borrow().pins.clone();
    if !push_unique(&mut texts, p.to_string_lossy()) {
        return false;
    }
    change(shared, &mut |o| o.pins = texts.clone());
    true
}

/// Removes a folder from our list; false when it was not there.
pub fn remove(shared: &Shared, p: &WidePath) -> bool {
    let key = same_key(&p.to_string_lossy());
    let before = shared.opts.borrow().pins.clone();
    let after: Vec<String> = before.iter().filter(|t| same_key(t) != key).cloned().collect();
    if after.len() == before.len() {
        return false;
    }
    change(shared, &mut |o| o.pins = after.clone());
    true
}

/// Whether the folder is in our list.
pub fn contains(shared: &Shared, p: &WidePath) -> bool {
    let key = same_key(&p.to_string_lossy());
    shared.opts.borrow().pins.iter().any(|t| same_key(t) == key)
}

/// The pins as stored in files.json (entries that are not paths are skipped).
pub fn paths(shared: &Shared) -> Vec<WidePath> {
    shared.opts.borrow().pins.iter().filter_map(|p| WidePath::parse(p).ok()).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn wp(s: &str) -> WidePath {
        WidePath::parse(s).expect("a path")
    }

    #[test]
    fn gathering_is_bounded_and_unique() {
        let mut list = Vec::new();
        let many: Vec<WidePath> = (0..MAX_PINS * 3).map(|i| wp(&format!(r"C:\made-up\p{i}"))).collect();
        for frame in many.chunks(50) {
            gather(&mut list, frame);
        }
        assert_eq!(list.len(), MAX_PINS, "a host that keeps sending cannot grow the gathering");
        assert_eq!(list[0], many[0], "order kept");
        let mut list = Vec::new();
        gather(&mut list, &[wp(r"C:\made-up\a"), wp(r"C:\made-up\A\")]);
        gather(&mut list, &[wp(r"C:\made-up\a")]);
        assert_eq!(list.len(), 1, "duplicates (case, closing backslash) are dropped as the list drops them");
    }
}
