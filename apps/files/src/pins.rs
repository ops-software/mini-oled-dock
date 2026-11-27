//! Pinned folders (P3-files-app 2.2; FD-SIDE fills it, F4.8): kept in files.json, imported once from File
//! Explorer's Quick access only when the FS-2 record allows it.

use crate::listing::WidePath;
use crate::shell_client::ShellReply;
use crate::window::Shared;

/// Set true by the FS-2 record only (an orchestrator commit).
pub const PINS_IMPORT_OK: bool = false;

/// First frame: QuickAccessPins when PINS_IMPORT_OK and not options.pins_imported. Filled by FD-SIDE; until then
/// nothing.
pub fn import_if_due(shared: &Shared) {
    let _ = shared;
}

/// ReqOwner::Pins. Filled by FD-SIDE; until then false.
pub fn on_reply(shared: &Shared, reply: &ShellReply) -> bool {
    let _ = (shared, reply);
    false
}

/// Filled by FD-SIDE; until then false.
pub fn add(shared: &Shared, p: &WidePath) -> bool {
    let _ = (shared, p);
    false
}

/// Filled by FD-SIDE; until then false.
pub fn remove(shared: &Shared, p: &WidePath) -> bool {
    let _ = (shared, p);
    false
}

/// The pins as stored in files.json (entries that are not paths are skipped).
pub fn paths(shared: &Shared) -> Vec<WidePath> {
    shared.opts.borrow().pins.iter().filter_map(|p| WidePath::parse(p).ok()).collect()
}
