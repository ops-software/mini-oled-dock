//! File type names (P3-files-app 2.2, F2.3): asked of the shell host for the extensions of the visible rows (FS-6
//! keeps shell code out of the UI), kept in memory and in types.json, with File Explorer's own fallback form ("PDF
//! File") until a name arrives.
// Decodes types.json from disk: no unwrap, no expect, no indexing.
#![deny(clippy::unwrap_used, clippy::expect_used, clippy::indexing_slicing)]

use std::cell::{Cell, RefCell};
use std::collections::{HashMap, HashSet, VecDeque};
use std::rc::Rc;

use oled_trust::frame::files::{ReqId, ShellEvt, ShellReq};
use oled_ui::model::ListModel;
use oled_ui::EventCx;
use oled_ui::UiLang;

use crate::names;
use crate::services::{OwnFiles, ReqOwner};
use crate::shell_client::ShellReply;
use crate::tr::{k, t, tf};
use crate::window::{FilesScreen, Shared};

/// The most extensions one request asks for, and the most names kept, in memory and in types.json.
pub const MAX_NAMES: usize = 256;
/// types.json's size cap.
pub const FILE_CAP: usize = 16 << 10;
/// types.json is written at most this often.
pub const SAVE_EVERY_MS: u64 = 60_000;
/// An extension and a name as the shell protocol carries them (TypeName).
const EXT_MAX: usize = 64;
const NAME_MAX: usize = 260;

/// The names by extension (lower case, no dot), the language they are in, whether they changed since the last save.
#[derive(Debug, Default)]
pub struct TypeNames {
    names: RefCell<HashMap<Box<[u8]>, Rc<str>>>,
    /// The keys of names, oldest first: past MAX_NAMES the oldest goes; the newest are the ones saved.
    order: RefCell<VecDeque<Box<[u8]>>>,
    /// Extensions asked for that came back without a name (or whose request fell back): not asked again until clear.
    unnamed: RefCell<HashSet<Box<[u8]>>>,
    /// The one request in flight.
    flight: RefCell<Option<Flight>>,
    lang: u16,
    dirty: Cell<bool>,
    /// When types.json was last written (or tried); None until the first save, which is due at once.
    last_save: Cell<Option<u64>>,
}

/// A request sent and not ended yet: its id, its extensions, and whether clear() came since (its names are then
/// dropped, for they may be the old ones).
#[derive(Debug)]
struct Flight {
    req: ReqId,
    exts: Vec<Box<[u8]>>,
    stale: bool,
}

impl TypeNames {
    pub fn empty() -> TypeNames {
        TypeNames::default()
    }

    pub fn get(&self, ext: &[u8]) -> Option<Rc<str>> {
        self.names.borrow().get(ext).cloned()
    }

    /// "{EXT} File" ("File {EXT}" in Italian); "File" for no extension.
    pub fn fallback(ext: &[u8], lang: UiLang) -> String {
        if ext.is_empty() {
            return t(k::FILES_TYPE_FILE, lang).into_owned();
        }
        let upper = String::from_utf8_lossy(ext).to_uppercase();
        tf(k::FILES_TYPE_EXT, lang, &[("ext", &upper)]).into_owned()
    }

    /// Associations changed (SHCNE_ASSOCCHANGED): every name may be stale. The request in flight runs to its end, but
    /// its names are dropped; the visible rows ask again on the next layout.
    pub fn clear(&self) {
        self.names.borrow_mut().clear();
        self.order.borrow_mut().clear();
        self.unnamed.borrow_mut().clear();
        if let Some(f) = self.flight.borrow_mut().as_mut() {
            f.stale = true;
        }
        self.dirty.set(true);
    }

    /// types.json (at most FILE_CAP bytes) when it is in `lang`, the user's default UI language id; otherwise, or when
    /// it is absent or malformed, no names.
    pub fn load(own: &dyn OwnFiles, lang: u16) -> TypeNames {
        let types = TypeNames { lang, ..TypeNames::default() };
        if let Some(bytes) = own.read(names::TYPES_FILE, FILE_CAP) {
            match parse(&bytes, lang) {
                Some(list) => {
                    for (ext, name) in list {
                        types.insert(ext, name);
                    }
                    // The names are the file's own: nothing to save until one changes.
                    types.dirty.set(false);
                }
                None => oled_sys::log!("files: types.json ignored (malformed, or in another language)"),
            }
        }
        types
    }

    /// Writes types.json when the names changed since the last save and that save is SAVE_EVERY_MS old, or at once
    /// when this process has not saved yet (an idle window keeps the first names it gets).
    pub fn save_if_due(&self, own: &dyn OwnFiles, now_ms: u64) {
        let due = self.last_save.get().is_none_or(|last| now_ms >= last.saturating_add(SAVE_EVERY_MS));
        if !self.dirty.get() || !due {
            return;
        }
        // A failed write waits for the next turn too: at most one write a minute.
        self.last_save.set(Some(now_ms));
        let text = self.to_json();
        if text.len() <= FILE_CAP && own.write_atomic(names::TYPES_FILE, text.as_bytes()) {
            self.dirty.set(false);
        } else {
            oled_sys::log!("files: types.json not written ({} bytes)", text.len());
        }
    }

    /// The language id the names are in.
    pub fn lang(&self) -> u16 {
        self.lang
    }

    pub fn len(&self) -> usize {
        self.names.borrow().len()
    }

    pub fn is_empty(&self) -> bool {
        self.names.borrow().is_empty()
    }

    /// The request in flight.
    pub fn in_flight(&self) -> Option<ReqId> {
        self.flight.borrow().as_ref().map(|f| f.req)
    }

    /// Of `exts`, the ones to ask the host for now, each once, in order, at most MAX_NAMES: neither named nor asked in
    /// vain before, and keyed as the shell protocol keys them (others keep their fallback). None while a request is in
    /// flight.
    pub fn missing<'a>(&self, exts: impl IntoIterator<Item = &'a [u8]>) -> Vec<Box<str>> {
        let mut out: Vec<Box<str>> = Vec::new();
        if self.flight.borrow().is_some() {
            return out;
        }
        let (names, unnamed) = (self.names.borrow(), self.unnamed.borrow());
        for ext in exts {
            if out.len() >= MAX_NAMES {
                break;
            }
            if names.contains_key(ext) || unnamed.contains(ext) || out.iter().any(|e| e.as_bytes() == ext) {
                continue;
            }
            if let Some(s) = wire_ext(ext) {
                out.push(s.into());
            }
        }
        out
    }

    /// Takes one shell reply: the names of the request in flight, or its end. True when a name was added or changed.
    pub fn on_shell_reply(&self, reply: &ShellReply) -> bool {
        let req = match reply {
            ShellReply::Evt(
                ShellEvt::TypeNames { req, .. } | ShellEvt::ReqDone { req } | ShellEvt::Failed { req, .. },
            )
            | ShellReply::Fallback { req, .. } => *req,
            _ => return false,
        };
        let mut flight = self.flight.borrow_mut();
        let Some(f) = flight.as_ref().filter(|f| f.req == req) else { return false };
        if let ShellReply::Evt(ShellEvt::TypeNames { names, .. }) = reply {
            if f.stale {
                return false;
            }
            let mut changed = false;
            for n in names {
                let ext = n.ext.as_bytes();
                let name = n.name.trim();
                // Only what was asked for, under the protocol's caps.
                if f.exts.iter().any(|e| &**e == ext) && !name.is_empty() && name.len() <= NAME_MAX {
                    changed |= self.insert(ext.into(), name.into());
                }
            }
            return changed;
        }
        // The end (ReqDone, Failed or a Fallback): what came back without a name is not asked for again.
        if let Some(f) = flight.take().filter(|f| !f.stale) {
            let names = self.names.borrow();
            let mut unnamed = self.unnamed.borrow_mut();
            if unnamed.len() + f.exts.len() > 4 * MAX_NAMES {
                unnamed.clear();
            }
            unnamed.extend(f.exts.into_iter().filter(|e| !names.contains_key(e)));
        }
        false
    }

    /// Records the request sent for `exts`.
    fn begin(&self, req: ReqId, exts: &[Box<str>]) {
        let exts = exts.iter().map(|e| Box::from(e.as_bytes())).collect();
        *self.flight.borrow_mut() = Some(Flight { req, exts, stale: false });
    }

    /// Adds or replaces one name (past MAX_NAMES the oldest goes); true when it changed something.
    fn insert(&self, ext: Box<[u8]>, name: Rc<str>) -> bool {
        let mut names = self.names.borrow_mut();
        if let Some(old) = names.get_mut(&ext) {
            if **old == *name {
                return false;
            }
            *old = name;
        } else {
            let mut order = self.order.borrow_mut();
            while names.len() >= MAX_NAMES {
                let Some(oldest) = order.pop_front() else { break };
                names.remove(&oldest);
            }
            order.push_back(ext.clone());
            names.insert(ext, name);
        }
        self.dirty.set(true);
        true
    }

    /// types.json: {"lang": <id>, "names": {"<ext>": "<name>", ...}} with the newest names that fit FILE_CAP.
    fn to_json(&self) -> String {
        let (names, order) = (self.names.borrow(), self.order.borrow());
        let mut kept = serde_json::Map::new();
        // {"lang":65535,"names":{}} is 25 bytes; each entry adds its quoted key and value, a colon and a comma.
        let mut size = 25;
        for key in order.iter().rev() {
            let (Ok(ext), Some(name)) = (std::str::from_utf8(key), names.get(key)) else { continue };
            let (ext_json, name_json) =
                (serde_json::Value::from(ext).to_string(), serde_json::Value::from(&**name).to_string());
            size += ext_json.len() + name_json.len() + 2;
            if size > FILE_CAP {
                break;
            }
            kept.insert(ext.to_string(), serde_json::Value::from(&**name));
        }
        serde_json::json!({ "lang": self.lang, "names": kept }).to_string()
    }
}

/// One name: its extension's bytes and the name.
type Entry = (Box<[u8]>, Rc<str>);

/// The names of a types.json in `lang`: None when it is not JSON, has no lang or names, or is in another language.
/// Entries that break the protocol's rules are left out; at most MAX_NAMES are kept.
fn parse(bytes: &[u8], lang: u16) -> Option<Vec<Entry>> {
    let v: serde_json::Value = serde_json::from_slice(bytes).ok()?;
    if v.get("lang")?.as_u64()? != u64::from(lang) {
        return None;
    }
    let names = v.get("names")?.as_object()?;
    let ok = |(ext, name): (&String, &serde_json::Value)| {
        let name = name.as_str()?.trim();
        let ok = wire_ext(ext.as_bytes()).is_some() && !name.is_empty() && name.len() <= NAME_MAX;
        ok.then(|| (Box::from(ext.as_bytes()), Rc::from(name)))
    };
    Some(names.iter().filter_map(ok).take(MAX_NAMES).collect())
}

/// `ext` as the shell protocol keys it (TypeName.ext): UTF-8, 1 to 64 bytes, lower case, no dot, separator or control
/// character.
fn wire_ext(ext: &[u8]) -> Option<&str> {
    let s = std::str::from_utf8(ext).ok()?;
    let lower = if s.is_ascii() { !s.bytes().any(|b| b.is_ascii_uppercase()) } else { s.to_lowercase() == s };
    let clean = !s.chars().any(|c| c == '.' || c == '\\' || c == '/' || c < ' ');
    (!s.is_empty() && s.len() <= EXT_MAX && lower && clean).then_some(s)
}

/// After layout: asks for the names of the visible rows' extensions that have none; saves types.json when due. Before
/// the first frame no host starts (F1a.10): the rows keep their cached or fallback names and ask on the next event.
pub fn request_missing(s: &mut FilesScreen, cx: &mut EventCx) {
    let _ = cx;
    let shared = Rc::clone(&s.shared);
    if !shared.first_frame_done.get() {
        s.visible_due = true;
        return;
    }
    save_if_due(&shared);
    let table = &s.tabs.active().table;
    let (model, rows) = (table.model(), table.visible_rows());
    let listing = model.listing();
    let exts = rows.filter_map(|row| {
        let id = crate::model::id_of(model.key(row));
        // A folder's type is its own ("File folder"); a name without an extension is a "File".
        (!listing.is_dir(id)).then(|| listing.ext(id)).flatten().filter(|e| !e.is_empty())
    });
    request_for(&shared, exts);
}

/// Asks the shell host for the names of `exts` it has not named yet (TypeNames::missing); true when a request went
/// out. Its replies come back through drain_shell to on_reply (ReqOwner::TypeNames). Nothing before the first frame.
pub fn request_for<'a>(shared: &Shared, exts: impl IntoIterator<Item = &'a [u8]>) -> bool {
    if !shared.first_frame_done.get() {
        return false;
    }
    let want = shared.types.missing(exts);
    if want.is_empty() {
        return false;
    }
    let shell = &shared.services.shell;
    let req = shell.next_req();
    shared.req_owner.borrow_mut().insert(req, ReqOwner::TypeNames);
    shared.types.begin(req, &want);
    shell.request(ShellReq::TypeNames { req, exts: want });
    true
}

/// ReqOwner::TypeNames: fills the names; true when one changed (drain_shell then bumps every main window's tables).
pub fn on_reply(shared: &Shared, reply: &ShellReply) -> bool {
    let changed = shared.types.on_shell_reply(reply);
    save_if_due(shared);
    changed
}

/// types.json is saved only after the first frame (Msg::FirstFrame), on an event once it is due: no timer.
fn save_if_due(shared: &Shared) {
    if shared.first_frame_done.get() {
        shared.types.save_if_due(&*shared.services.own, shared.services.clock.now_ms());
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::indexing_slicing)]

    use oled_trust::frame::files::{Location, WidePath};
    use oled_ui::harness::Harness;
    use oled_ui::{SizeL, Tokens};

    use super::*;
    use crate::fakes::Fakes;
    use crate::window::STARTUP_TAB;

    #[test]
    fn no_request_before_the_first_frame() {
        let f = Fakes::new();
        let shared = f.shared(UiLang::EN);
        let dir = WidePath::parse(r"C:\mod-files-made-up\reports").unwrap();
        let s = FilesScreen::new(Rc::clone(&shared), 1, Location::Fs(dir), None, STARTUP_TAB);
        let mut h = Harness::new(s, SizeL { w: 800, h: 500 }, 96, Tokens::oled());
        h.render();
        h.key("Down");
        assert!(h.screen().visible_due, "before the first frame the rows ask again on the next event");
        assert!(!request_for(&shared, [&b"pdf"[..]]), "no host before the first frame");
        shared.first_frame_done.set(true);
        h.key("Down");
        assert!(!h.screen().visible_due, "asked");
        assert!(f.shell.requests().is_empty(), "no visible row has an extension in this table");
        assert!(request_for(&shared, [&b"pdf"[..]]));
        assert_eq!(f.shell.requests().len(), 1);
    }
}
