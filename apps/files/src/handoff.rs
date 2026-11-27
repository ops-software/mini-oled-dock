//! Hand-offs to File Explorer (P3-files-app 2.2, F4.13; SPEC-files 5.5.7): the locations this window does not list
//! (the Recycle Bin, Network, This PC, other shell: names the user typed) and plain folders open in File Explorer
//! through the shell host. A persisted loop guard (files-guard.json through Services.own, so it spans processes)
//! turns the same hand-off within 3 s into a notice with [Open ↗] instead of a second window.
#![deny(clippy::unwrap_used, clippy::expect_used, clippy::indexing_slicing)]

use std::rc::Rc;

use oled_trust::frame::files::{ItemRef, Location, ShellReq, Verb};
use oled_ui::{EventCx, Reply};
use serde::{Deserialize, Serialize};

use crate::listing::WidePath;
use crate::names;
use crate::open;
use crate::services::ReqOwner;
use crate::status::{self, Notice, NoticeAct};
use crate::tr::{self, k};
use crate::window::FilesScreen;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Target {
    RecycleBin,
    Network,
    ThisPc,
    Other(String),
    Folder(WidePath),
}

/// The shell: names File Explorer opens for the three virtual places. Not in the research: LIVE-FD step 9 checks
/// that each opens the right view.
pub const RECYCLE_BIN: &str = "shell:RecycleBinFolder";
pub const NETWORK: &str = "shell:NetworkPlacesFolder";
pub const THIS_PC: &str = "shell:MyComputerFolder";

/// The same target again within this many ms is held back.
pub const GUARD_MS: u64 = 3_000;
/// Entries the guard file keeps (the newest).
pub const GUARD_MAX: usize = 64;
/// The guard file is read up to this many bytes; a larger one counts as empty.
pub const GUARD_CAP: usize = 64 * 1024;
/// Characters of a target the guard keeps (a typed text can be long).
const KEY_MAX: usize = 512;

/// What a hand-off asks of the shell host.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Plan {
    /// ShellReq::RunTyped of this text, in the user's profile folder.
    Run(String),
    /// ShellReq::Exec(Verb::OpenInExplorer) of this folder.
    Explorer(WidePath),
}

/// The hand-off the guard held back, for its notice's [Open ↗] (a field of FilesScreen: the owner of the frozen
/// struct has no other place for it).
#[derive(Debug, Default)]
pub struct Held(Option<Plan>);

/// What a target asks of the host. Text runs only when the user typed it (U5): Other from anywhere else is None.
pub fn plan_of(t: &Target, typed: bool) -> Option<Plan> {
    match t {
        Target::RecycleBin => Some(Plan::Run(RECYCLE_BIN.to_string())),
        Target::Network => Some(Plan::Run(NETWORK.to_string())),
        Target::ThisPc => Some(Plan::Run(THIS_PC.to_string())),
        Target::Other(text) if typed && !text.trim().is_empty() => Some(Plan::Run(text.clone())),
        Target::Other(_) => None,
        Target::Folder(p) => Some(Plan::Explorer(p.clone())),
    }
}

/// The name a hand-off is kept under in the guard: the text it runs or the folder's path, at most KEY_MAX chars.
pub fn guard_key(p: &Plan) -> String {
    let full = match p {
        Plan::Run(text) => text.clone(),
        Plan::Explorer(path) => path.to_string_lossy(),
    };
    full.chars().take(KEY_MAX).collect()
}

/// One hand-off as the guard file keeps it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
struct Entry {
    target: String,
    /// Unix time, ms.
    ms: u64,
}

/// The guard's answer.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Guard {
    /// Hand it off and write these bytes back (the entry appended, at most GUARD_MAX kept).
    Allow(Vec<u8>),
    /// The same target was handed off less than GUARD_MS ago (in any process).
    Block,
}

/// The file's entries: a missing, oversized or malformed file counts as empty (it is rewritten whole).
fn entries(bytes: Option<&[u8]>) -> Vec<Entry> {
    let Some(b) = bytes else { return Vec::new() };
    match serde_json::from_slice::<Vec<Entry>>(b) {
        Ok(v) => v,
        Err(e) => {
            oled_sys::log!("files: {} is not usable ({e}): started again", names::GUARD_FILE);
            Vec::new()
        }
    }
}

/// The guard before a hand-off of `key` at `now_ms` (unix ms) over the file's bytes. Pure.
pub fn guard_check(bytes: Option<&[u8]>, key: &str, now_ms: u64) -> Guard {
    let list = entries(bytes);
    if list.iter().any(|e| e.target == key && e.ms.abs_diff(now_ms) < GUARD_MS) {
        return Guard::Block;
    }
    Guard::Allow(appended(list, key, now_ms))
}

/// The file's bytes with one more entry for `key`, the oldest dropped past GUARD_MAX. Pure.
pub fn guard_record(bytes: Option<&[u8]>, key: &str, now_ms: u64) -> Vec<u8> {
    appended(entries(bytes), key, now_ms)
}

fn appended(mut list: Vec<Entry>, key: &str, now_ms: u64) -> Vec<u8> {
    list.push(Entry { target: key.to_string(), ms: now_ms });
    let extra = list.len().saturating_sub(GUARD_MAX);
    list.drain(..extra);
    serde_json::to_vec(&list).unwrap_or_default()
}

/// Wall-clock ms: the guard spans processes, so it cannot use a process's own clock.
fn unix_ms() -> u64 {
    let since = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap_or_default();
    u64::try_from(since.as_millis()).unwrap_or(u64::MAX)
}

/// Hands the target to File Explorer, unless the guard holds it back: then the notice "This location opens in File
/// Explorer" with [Open ↗]. `typed`: the user's own Enter in the path field (only then does Other text run).
pub fn hand_off(s: &mut FilesScreen, cx: &mut EventCx, t: Target, typed: bool) -> Reply {
    let Some(plan) = plan_of(&t, typed) else {
        oled_sys::log!("files: a hand-off of text the user did not type is refused");
        return not_in_beta(s, cx);
    };
    let own = Rc::clone(&s.shared.services.own);
    let key = guard_key(&plan);
    match guard_check(own.read(names::GUARD_FILE, GUARD_CAP).as_deref(), &key, unix_ms()) {
        Guard::Block => {
            oled_sys::log!("files: the same hand-off within {GUARD_MS} ms: the notice instead");
            s.held.0 = Some(plan);
            let lang = cx.lang();
            let mut n = Notice::new(tr::t(k::FILES_STATE_HANDOFF, lang));
            n.action = Some((tr::t(k::FILES_ACTION_OPEN_EXPLORER_ARROW, lang), NoticeAct::HandOff));
            status::notice(s, cx, n);
            Reply::Redraw
        }
        Guard::Allow(bytes) => {
            if !own.write_atomic(names::GUARD_FILE, &bytes) {
                oled_sys::log!("files: {} was not written", names::GUARD_FILE);
            }
            send(s, cx, plan)
        }
    }
}

/// The guard notice's [Open ↗]: the hand-off it held back, asked for again by the user's own click (recorded, not
/// checked).
pub fn open_held(s: &mut FilesScreen, cx: &mut EventCx) -> Reply {
    let Some(plan) = s.held.0.take() else { return Reply::None };
    let own = Rc::clone(&s.shared.services.own);
    let bytes = guard_record(own.read(names::GUARD_FILE, GUARD_CAP).as_deref(), &guard_key(&plan), unix_ms());
    if !own.write_atomic(names::GUARD_FILE, &bytes) {
        oled_sys::log!("files: {} was not written", names::GUARD_FILE);
    }
    send(s, cx, plan)
}

/// The request, owned by this window as an Exec (a failure shows Windows' message; the earlier open's Retry is not
/// offered for it).
fn send(s: &mut FilesScreen, cx: &mut EventCx, plan: Plan) -> Reply {
    let shell = Rc::clone(&s.shared.services.shell);
    let req = shell.next_req();
    let r = match plan {
        Plan::Run(text) => match run_dir(s) {
            Some(dir) => ShellReq::RunTyped { req, text: text.into_boxed_str(), dir },
            None => {
                oled_sys::log!("files: no folder to run the hand-off in");
                return not_in_beta(s, cx);
            }
        },
        Plan::Explorer(p) => ShellReq::Exec { req, verb: Verb::OpenInExplorer, items: vec![ItemRef::Path(p)] },
    };
    open::forget_retry(s);
    s.shared.req_owner.borrow_mut().insert(req, ReqOwner::Exec { win: s.win });
    shell.request(r);
    Reply::None
}

/// The user's profile folder (the known folders the start read), else the folder the tab shows.
fn run_dir(s: &FilesScreen) -> Option<WidePath> {
    let known = s.shared.session.borrow().known.get("profile").and_then(|p| WidePath::parse(p).ok());
    known.or_else(|| match s.tabs.active().nav.location() {
        Location::Fs(p) => Some(p.clone()),
        _ => None,
    })
}

fn not_in_beta(s: &mut FilesScreen, cx: &mut EventCx) -> Reply {
    let text = tr::t(k::FILES_MENU_NOT_IN_BETA, cx.lang());
    status::notice(s, cx, Notice::new(text));
    Reply::Redraw
}
