//! Changes to user files as the window asks for them (P3-files-app 2.2, F3). request() is the only caller of
//! OpsService's mutating methods: every change passes the real-data gate first. Around it: the operations model fed by
//! the engine's events, Del with the recycle classification (U3), the chip with its progress line in the status line,
//! the flyout, and the reports of operations an earlier session left (F3.6).

use std::collections::HashMap;
use std::rc::Rc;
use std::sync::Arc;

use oled_trust::frame::files::{Change, ChangeKind, ItemRef, OpEvt, OpId, OpKind, OpResult, Refusal, UndoKind};
use oled_ui::model::{Align, ListModel, ScrollTo, Selection};
use oled_ui::node::{Actions, Focusable, Node, NodeFlags, NodeId, Role, Text, WidgetId};
use oled_ui::text::Trim;
use oled_ui::theme::TypeRole;
use oled_ui::widgets::{ButtonOut, IconButton, Progress};
use oled_ui::{
    Event, EventCx, Glyph, Ink, Key, LayoutCx, MenuItem, MouseButton, PaintCx, RectP, Reply, UiLang, Widget,
};
use windows::core::{PCWSTR, PWSTR};
use windows::Win32::Storage::FileSystem::GetDriveTypeW;
use windows::Win32::System::Diagnostics::Debug::{
    FormatMessageW, FORMAT_MESSAGE_FROM_SYSTEM, FORMAT_MESSAGE_IGNORE_INSERTS,
};
use windows::Win32::UI::WindowsAndMessaging::AllowSetForegroundWindow;

use crate::format::Fmt;
use crate::gate;
use crate::keys::{self, Cmd};
use crate::listing::{TabId, WidePath};
use crate::model::key_of;
use crate::msg::{Fwd, JobDone};
use crate::ops::classify::{classify_facts, gather_facts, BinFacts, Recyclability};
use crate::ops::consts::PAUSE_OK;
use crate::ops_client::{OpsError, OpsEvent, UndoRecord};
use crate::status::{self, Notice, NoticeAct};
use crate::tr::{k, t, tf};
use crate::undo::{self, UndoEntry};
use crate::window::{self, FilesScreen, Shared, WinKind};
use crate::{bar, bg, names};

/// A change the window asks for.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum MutReq {
    Recycle(Vec<WidePath>),
    DeletePermanent(Vec<WidePath>),
    Rename { item: WidePath, to: String },
    NewFolder { parent: WidePath, name: String },
    Undo(UndoRecord),
    ResumeRest(OpId),
}

/// The paths a request touches: its items and destination (Undo: every entry's from, to and bin; ResumeRest: the items
/// OpsModel recorded, none for an operation of an earlier process). None when the destination cannot be formed (a
/// Rename or NewFolder name that is not one valid component, or an item without a parent): the gate never approves a
/// destination it did not see.
fn paths_of(shared: &Shared, r: &MutReq) -> Option<Vec<WidePath>> {
    let joined = |dir: &WidePath, name: &str| dir.join(&name.encode_utf16().collect::<Vec<u16>>()).ok();
    Some(match r {
        MutReq::Recycle(items) | MutReq::DeletePermanent(items) => items.clone(),
        MutReq::Rename { item, to } => vec![item.clone(), joined(&item.parent()?, to)?],
        MutReq::NewFolder { parent, name } => vec![parent.clone(), joined(parent, name)?],
        MutReq::Undo(rec) => rec
            .entries
            .iter()
            .flat_map(|e| [Some(e.from.clone()), Some(e.to.clone()), e.bin.clone()])
            .flatten()
            .collect(),
        MutReq::ResumeRest(of) => shared.ops.borrow().items_of(*of),
    })
}

fn item_refs(items: &[WidePath]) -> Vec<ItemRef> {
    items.iter().cloned().map(ItemRef::Path).collect()
}

/// The gate, then the OpsService call. A destination that cannot be formed: the forbidden-name notice, nothing sent.
/// Refused by the gate: the preview_gate notice, nothing sent. The host unavailable: host_lost with Retry. Refused by
/// the host: its reason. Accepted: on_accepted, Some(op).
pub fn request(s: &mut FilesScreen, cx: &mut EventCx, r: MutReq) -> Option<OpId> {
    let lang = cx.lang();
    let Some(paths) = paths_of(&s.shared, &r) else {
        status::notice(s, cx, Notice::new(t(k::FILES_RENAME_FORBIDDEN, lang)));
        return None;
    };
    let refs: Vec<&WidePath> = paths.iter().collect();
    if !gate::may_mutate(&refs) {
        status::notice(s, cx, Notice::new(t(k::FILES_OPS_PREVIEW_GATE, lang)));
        return None;
    }
    let ops = std::rc::Rc::clone(&s.shared.services.ops);
    let res = match &r {
        MutReq::Recycle(items) => ops.recycle(&item_refs(items)),
        MutReq::DeletePermanent(items) => ops.delete_permanent(&item_refs(items)),
        MutReq::Rename { item, to } => ops.rename(item, to),
        MutReq::NewFolder { parent, name } => ops.new_folder(parent, name),
        MutReq::Undo(rec) => ops.undo(rec),
        MutReq::ResumeRest(of) => ops.resume_rest(*of),
    };
    match res {
        Ok(op) => {
            on_accepted(s, cx, op, &r);
            Some(op)
        }
        Err(OpsError::Unavailable(why)) => {
            oled_sys::log!("files: operations helper unavailable: {why}");
            let retry = (t(k::FILES_ACTION_RETRY, lang), NoticeAct::Retry);
            status::notice(s, cx, Notice { action: Some(retry), ..Notice::new(t(k::FILES_OPS_HOST_LOST, lang)) });
            None
        }
        Err(OpsError::Refused(why)) => {
            status::notice(s, cx, Notice::new(refusal_text(&why, lang)));
            None
        }
    }
}

// ---- the operations model ---------------------------------------------------------------------------------------

/// Classifies items (path, size from the store) for the Recycle Bin, off the UI thread. The system's by default
/// (system_facts); tests and scenes install a scripted one.
pub type FactsFn = Arc<dyn Fn(&[(WidePath, u64)]) -> Vec<(WidePath, Recyclability)> + Send + Sync>;

/// How long a finished operation stays in the chip and as a finished row (ms).
pub const FINISHED_HOLD_MS: u64 = 6_000;
/// Failed items kept per operation (their names and Windows' messages).
const FAILURES_KEPT: usize = 64;
/// Stopped operations whose items are kept after their rows went.
const KEPT_STOPPED: usize = 16;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum OpState {
    /// Asked for; the helper has not said it started.
    Queued,
    Running,
    Done,
    Cancelled,
    Failed,
    /// The helper went away while it ran.
    Stopped,
}

impl OpState {
    pub fn running(self) -> bool {
        matches!(self, OpState::Queued | OpState::Running)
    }
}

/// One operation of this process (or of an earlier window of it, from a Snapshot).
#[derive(Clone, Debug)]
pub struct OpRow {
    pub op: OpId,
    /// The kind the texts use: a Resume shows the kind it resumes.
    pub kind: OpKind,
    /// The window that asked; 0 for an operation this process did not ask for (a Snapshot).
    pub win: u32,
    pub items: Vec<WidePath>,
    /// The folder of the items (delete, recycle) or the folder changed (rename, new folder).
    pub folder: Option<WidePath>,
    /// A rename's new name, a new folder's name.
    pub name: Option<String>,
    pub count: u64,
    pub done: u64,
    pub total: u64,
    pub bps: u64,
    pub state: OpState,
    pub needs_input: bool,
    /// The failed items with Windows' message for each (at most FAILURES_KEPT).
    pub failures: Vec<(WidePath, String)>,
    pub failed: u32,
    /// When the first window saw it finish (its EventCx clock); None while it runs or until a window saw it.
    pub finished_at: Option<u64>,
    pub undoable: bool,
    /// Its failures were opened (Details): the row may go.
    pub opened: bool,
    /// An undo: the record it undoes (the refusal texts read replaced; a success pushes the redo).
    pub undo_of: Option<UndoRecord>,
    /// Asked for by a redo: it does not clear the redo stack.
    pub redo: bool,
    /// The row to focus once this delete's removals arrive (SPEC-files 5.4.6), and whether they did.
    pub focus: Option<Box<[u8]>>,
    pub focus_due: bool,
}

impl OpRow {
    /// A row as on_accepted makes it, before any event (tests and scenes fill the rest).
    pub fn new(op: OpId, kind: OpKind, win: u32) -> OpRow {
        OpRow {
            op,
            kind,
            win,
            items: Vec::new(),
            folder: None,
            name: None,
            count: 0,
            done: 0,
            total: 0,
            bps: 0,
            state: OpState::Queued,
            needs_input: false,
            failures: Vec::new(),
            failed: 0,
            finished_at: None,
            undoable: false,
            opened: false,
            undo_of: None,
            redo: false,
            focus: None,
            focus_due: false,
        }
    }

    fn finished(&self) -> bool {
        !self.state.running()
    }
}

/// A notice an event left for a window, rendered in that window's language when it takes it.
#[derive(Clone, Debug, PartialEq, Eq)]
enum Pend {
    Refused { why: Refusal, detail: String, replaced: u32 },
    HostLost,
    Pending { of: OpId, kind: OpKind, done: u32, total: u32 },
    Away { kind: OpKind, items: u32, failed: u32 },
}

/// The operations this process started, the notices their events left, and the classifier of Del.
pub struct OpsModel {
    rows: Vec<OpRow>,
    /// (window, notice); window 0: the first main window that takes notices.
    notices: Vec<(u32, Pend)>,
    /// AckDone to send once the window that took the DoneWhileAway notice showed it.
    acks: Vec<(u32, OpId)>,
    /// The kinds of the Pending operations reported this session (a resume shows them).
    pending_kinds: HashMap<OpId, OpKind>,
    /// The items of stopped operations whose rows went (a later Pending offers their rest; at most KEPT_STOPPED).
    kept: Vec<(OpId, Vec<WidePath>)>,
    facts: FactsFn,
    /// The next accepted request comes from a redo (it keeps the redo stack).
    redo_next: bool,
    /// The main window that waits for the operations window before it closes (F3.5).
    close_after: Option<u32>,
    /// The operations window it waits for was built (its open may still have failed).
    ops_screen_made: bool,
    /// The language take_notices renders in (a window passes its own to take_notices_in).
    lang: UiLang,
}

impl std::fmt::Debug for OpsModel {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("OpsModel").field("rows", &self.rows.len()).field("notices", &self.notices.len()).finish()
    }
}

impl Default for OpsModel {
    fn default() -> OpsModel {
        OpsModel::new()
    }
}

impl OpsModel {
    pub fn new() -> OpsModel {
        OpsModel {
            rows: Vec::new(),
            notices: Vec::new(),
            acks: Vec::new(),
            pending_kinds: HashMap::new(),
            kept: Vec::new(),
            facts: Arc::new(system_facts),
            redo_next: false,
            close_after: None,
            ops_screen_made: false,
            lang: UiLang::EN,
        }
    }

    /// Operations asked for or running.
    pub fn running(&self) -> usize {
        self.rows.iter().filter(|r| r.state.running()).count()
    }

    /// The items an operation was asked to change (none for an operation of an earlier process).
    pub fn items_of(&self, op: OpId) -> Vec<WidePath> {
        match self.row(op) {
            Some(r) => r.items.clone(),
            None => self.kept.iter().find(|(o, _)| *o == op).map(|(_, v)| v.clone()).unwrap_or_default(),
        }
    }

    /// The notices left for the window (and those for any window), in the model's language.
    pub fn take_notices(&mut self, win: u32) -> Vec<Notice> {
        let lang = self.lang;
        self.take_notices_in(win, lang, &Fmt::fixed_en())
    }

    /// The notices left for the window (and those for any window), rendered in its language with its numbers.
    pub fn take_notices_in(&mut self, win: u32, lang: UiLang, fmt: &Fmt) -> Vec<Notice> {
        let (mine, rest): (Vec<_>, Vec<_>) =
            std::mem::take(&mut self.notices).into_iter().partition(|(w, _)| *w == win || *w == 0);
        self.notices = rest;
        mine.into_iter().map(|(_, p)| render(&p, lang, fmt)).collect()
    }

    /// The AckDone calls due once the window showed its notices.
    pub fn take_acks(&mut self, win: u32) -> Vec<OpId> {
        let (mine, rest): (Vec<_>, Vec<_>) =
            std::mem::take(&mut self.acks).into_iter().partition(|(w, _)| *w == win || *w == 0);
        self.acks = rest;
        mine.into_iter().map(|(_, op)| op).collect()
    }

    /// The classifier Del runs (tests and scenes script it).
    pub fn set_facts(&mut self, f: FactsFn) {
        self.facts = f;
    }

    pub fn facts(&self) -> FactsFn {
        Arc::clone(&self.facts)
    }

    pub fn rows(&self) -> &[OpRow] {
        &self.rows
    }

    /// A row as a scene or a test builds it (replaces the row of the same operation).
    pub fn insert(&mut self, row: OpRow) {
        self.rows.retain(|r| r.op != row.op);
        self.rows.push(row);
    }

    pub fn row(&self, op: OpId) -> Option<&OpRow> {
        self.rows.iter().find(|r| r.op == op)
    }

    fn row_mut(&mut self, op: OpId) -> Option<&mut OpRow> {
        self.rows.iter_mut().find(|r| r.op == op)
    }

    /// The row of the operation, made for one this process did not ask for.
    fn row_or_new(&mut self, op: OpId, kind: OpKind) -> &mut OpRow {
        if !self.rows.iter().any(|r| r.op == op) {
            self.rows.push(OpRow::new(op, kind, 0));
        }
        let i = self.rows.iter().position(|r| r.op == op).unwrap_or(self.rows.len() - 1);
        &mut self.rows[i]
    }

    /// Marks the next accepted request as a redo's (it keeps the redo stack).
    pub fn mark_redo_next(&mut self) {
        self.redo_next = true;
    }

    pub(crate) fn take_redo_next(&mut self) -> bool {
        std::mem::take(&mut self.redo_next)
    }

    /// Stamps every finish no window has seen yet with now. True when one was stamped.
    pub fn stamp(&mut self, now: u64) -> bool {
        let mut any = false;
        for r in self.rows.iter_mut().filter(|r| r.finished() && r.finished_at.is_none()) {
            r.finished_at = Some(now);
            any = true;
        }
        any
    }

    /// Finished rows older than the hold go, except those with failures nobody opened; a stopped row leaves its items
    /// behind for a later Pending.
    pub fn prune(&mut self, now: u64) {
        let mut gone = Vec::new();
        self.rows.retain(|r| {
            let old = r.finished_at.is_some_and(|at| now >= at + FINISHED_HOLD_MS);
            let go = old && (r.failed == 0 || r.opened);
            if go && r.state == OpState::Stopped {
                gone.push((r.op, r.items.clone()));
            }
            !go
        });
        for g in gone {
            if self.kept.len() >= KEPT_STOPPED {
                self.kept.remove(0);
            }
            self.kept.push(g);
        }
    }

    /// The next moment a finished row's hold ends (the chip's, and the row's unless it keeps unopened failures),
    /// after now.
    pub fn next_due(&self, now: u64) -> Option<u64> {
        self.rows.iter().filter_map(|r| r.finished_at.map(|at| at + FINISHED_HOLD_MS)).filter(|&due| due > now).min()
    }

    /// Finished rows the hold still keeps at now (or that no window saw yet).
    fn recent(&self, now: u64) -> impl Iterator<Item = &OpRow> + '_ {
        self.rows.iter().filter(move |r| {
            r.finished() && r.state != OpState::Stopped && r.finished_at.is_none_or(|at| now < at + FINISHED_HOLD_MS)
        })
    }

    /// The share of the running operations' work that is done, when any of them knows its total.
    pub fn aggregate(&self) -> Option<f32> {
        let (done, total) = self
            .rows
            .iter()
            .filter(|r| r.state.running() && r.total > 0)
            .fold((0u64, 0u64), |(d, t), r| (d + r.done.min(r.total), t + r.total));
        (total > 0).then(|| (done as f64 / total as f64) as f32)
    }

    /// The main window that waits for the operations window before it closes.
    pub fn close_after(&self) -> Option<u32> {
        self.close_after
    }

    pub fn set_close_after(&mut self, win: Option<u32>) {
        self.close_after = win;
        self.ops_screen_made = false;
    }

    /// The operations window was built for the window that waits (F3.5).
    pub fn ops_screen_made(&mut self) {
        self.ops_screen_made = true;
    }

    fn notice(&mut self, win: u32, p: Pend) {
        self.notices.push((win, p));
    }
}

/// The kind an undo record undoes.
fn forward_kind(k: UndoKind) -> OpKind {
    match k {
        UndoKind::RenameBack => OpKind::Rename,
        UndoKind::RecycleNewFolder => OpKind::NewFolder,
        UndoKind::MoveBack => OpKind::Move,
        UndoKind::RecycleCopies => OpKind::Copy,
        UndoKind::RestoreRecycled | UndoKind::OpenRecycleBin => OpKind::Recycle,
    }
}

pub(crate) fn name_of(p: &WidePath) -> String {
    p.file_name().map(String::from_utf16_lossy).unwrap_or_else(|| p.to_string_lossy())
}

/// The op enters OpsModel with its kind, items, folder and window; a new operation (not an undo, not a redo's) clears
/// the redo stack; a delete from this window remembers the row to focus once its items are gone.
pub fn on_accepted(s: &mut FilesScreen, cx: &mut EventCx, op: OpId, r: &MutReq) {
    let _ = cx;
    let shared = Rc::clone(&s.shared);
    let (kind, items, folder, name) = match r {
        MutReq::Recycle(v) => (OpKind::Recycle, v.clone(), v.first().and_then(WidePath::parent), None),
        MutReq::DeletePermanent(v) => (OpKind::DeletePermanent, v.clone(), v.first().and_then(WidePath::parent), None),
        MutReq::Rename { item, to } => (OpKind::Rename, vec![item.clone()], item.parent(), Some(to.clone())),
        MutReq::NewFolder { parent, name } => {
            let made = parent.join(&name.encode_utf16().collect::<Vec<u16>>()).ok();
            (OpKind::NewFolder, made.into_iter().collect(), Some(parent.clone()), Some(name.clone()))
        }
        MutReq::Undo(rec) => {
            let items: Vec<WidePath> = rec.entries.iter().map(|e| e.from.clone()).collect();
            let folder = items.first().and_then(WidePath::parent);
            (OpKind::Undo, items, folder, rec.entries.first().map(|e| name_of(&e.from)))
        }
        MutReq::ResumeRest(of) => {
            let m = shared.ops.borrow();
            let kind = m.pending_kinds.get(of).copied().unwrap_or(OpKind::DeletePermanent);
            let items = m.items_of(*of);
            let folder = items.first().and_then(WidePath::parent);
            (kind, items, folder, None)
        }
    };
    let focus = match r {
        MutReq::Recycle(v) | MutReq::DeletePermanent(v) => focus_after(s, v),
        _ => None,
    };
    let redo = {
        let mut m = shared.ops.borrow_mut();
        let redo = m.take_redo_next();
        let mut row = OpRow::new(op, kind, s.win);
        row.count = items.len() as u64;
        row.items = items;
        row.folder = folder;
        row.name = name;
        row.redo = redo;
        row.focus = focus;
        if let MutReq::Undo(rec) = r {
            row.undo_of = Some(rec.clone());
        }
        m.rows.retain(|x| x.op != op);
        m.rows.push(row);
        redo
    };
    if !redo && !matches!(r, MutReq::Undo(_)) {
        shared.undo.borrow_mut().clear_redo();
    }
}

/// The row that takes the focus once the items are gone: the next row after the last of them, or the one before the
/// first at the end of the list (SPEC-files 5.4.6). Only for items of the folder the active tab shows.
fn focus_after(s: &FilesScreen, items: &[WidePath]) -> Option<Box<[u8]>> {
    let m = s.tabs.active().table.model();
    let l = m.listing();
    let mut rows = Vec::new();
    for p in items {
        if p.parent().as_ref() != Some(l.dir()) {
            continue;
        }
        let Some(file) = p.file_name() else { continue };
        let mut name = Vec::new();
        crate::listing::collate::utf16_to_wtf8(file, &mut name);
        if let Some(row) = l.find(&name).and_then(|id| ListModel::row_of(m, key_of(id))) {
            rows.push(row);
        }
    }
    let (first, last) = (*rows.iter().min()?, *rows.iter().max()?);
    let gone = |row: usize| rows.contains(&row);
    let len = ListModel::len(m);
    let pick = (last + 1..len).find(|&r| !gone(r)).or_else(|| (0..first).rev().find(|&r| !gone(r)))?;
    let id = crate::model::id_of(ListModel::key(m, pick));
    l.name(id).map(Box::from)
}

/// The text of a refusal, for a request the helper refused at once (no detail).
pub fn refusal_text(r: &Refusal, lang: UiLang) -> Text {
    refusal_text_with(r, "", 0, lang, &Fmt::fixed_en())
}

/// The text of a refusal with its detail (the name an undo found changed) and, for Replaced, the count the undo
/// record carries. Undo refusals are shown as they are (F3.3); the rest map to the nearest text the table has.
pub fn refusal_text_with(r: &Refusal, detail: &str, replaced: u32, lang: UiLang, fmt: &Fmt) -> Text {
    match r {
        Refusal::Replaced => tf(k::FILES_OPS_UNDO_REPLACED, lang, &[("n", &fmt.count(u64::from(replaced)))]),
        Refusal::Changed => tf(k::FILES_OPS_UNDO_CHANGED, lang, &[("name", detail)]),
        Refusal::NotEmpty => tf(k::FILES_OPS_UNDO_NOT_EMPTY, lang, &[("name", detail)]),
        Refusal::OutsideTestRoot => t(k::FILES_OPS_PREVIEW_GATE, lang),
        Refusal::BadName => t(k::FILES_RENAME_FORBIDDEN, lang),
        Refusal::Root
        | Refusal::BadPath
        | Refusal::MixedParents
        | Refusal::TooMany
        | Refusal::Elevated
        | Refusal::PauseUnsupported => t(k::FILES_MENU_NOT_IN_BETA, lang),
        Refusal::NoItems | Refusal::UnknownOp | Refusal::OpInUse => t(k::FILES_OPS_HOST_LOST, lang),
    }
}

fn render(p: &Pend, lang: UiLang, fmt: &Fmt) -> Notice {
    match p {
        Pend::Refused { why, detail, replaced } => Notice::new(refusal_text_with(why, detail, *replaced, lang, fmt)),
        Pend::HostLost => Notice::new(t(k::FILES_OPS_HOST_LOST, lang)),
        Pend::Pending { of, kind, done, total } => {
            let (done, total) = (fmt.count(u64::from(*done)), fmt.count(u64::from(*total)));
            let text = tf(k::FILES_OPS_STOPPED, lang, &[("done", &done), ("total", &total)]);
            // Only a delete offers its rest; any other kind (an undo, a host that died mid-rename) shows no button,
            // and its journal is kept for the next start.
            let label = match kind {
                OpKind::Recycle => Some(k::FILES_OPS_REST_RECYCLE),
                OpKind::DeletePermanent => Some(k::FILES_OPS_REST_DELETE),
                _ => None,
            };
            Notice { action: label.map(|l| (t(l, lang), NoticeAct::Rest(*of))), ..Notice::new(text) }
        }
        Pend::Away { kind, items, failed } => {
            let app = lang.kit(oled_ui::strings::k::KIT_APP_FILES);
            let key = if *kind == OpKind::Recycle { k::FILES_OPS_AWAY_RECYCLED } else { k::FILES_OPS_AWAY_DELETED };
            let mut text = tf(key, lang, &[("n", &fmt.count(u64::from(*items))), ("name", &app)]).into_owned();
            if *failed > 0 {
                text.push_str(SEP_DOT);
                text.push_str(&failed_text(u64::from(*failed), lang, fmt));
            }
            Notice::new(Text::Owned(text))
        }
    }
}

fn failed_text(n: u64, lang: UiLang, fmt: &Fmt) -> Text {
    if n == 1 {
        t(k::FILES_OPS_FAILED_ONE, lang)
    } else {
        tf(k::FILES_OPS_FAILED_OTHER, lang, &[("n", &fmt.count(n))])
    }
}

/// Windows' message for an HRESULT (FormatMessageW), or its hex form.
fn win_message(hr: i32) -> String {
    let mut buf = [0u16; 512];
    // SAFETY: the buffer is writable for its whole length; FORMAT_MESSAGE_IGNORE_INSERTS reads no arguments.
    let n = unsafe {
        FormatMessageW(
            FORMAT_MESSAGE_FROM_SYSTEM | FORMAT_MESSAGE_IGNORE_INSERTS,
            None,
            hr as u32,
            0,
            PWSTR(buf.as_mut_ptr()),
            buf.len() as u32,
            None,
        )
    } as usize;
    let text = buf.get(..n).map(String::from_utf16_lossy).unwrap_or_default();
    let text = text.trim();
    if text.is_empty() {
        format!("0x{:08X}", hr as u32)
    } else {
        text.to_string()
    }
}

/// Every ops event into OpsModel and the undo stack. ItemsDone goes to every main window as Fwd::Own (each applies
/// what its tabs show, F3.10); when the model changed, every window then hears Fwd::Ops (its chip, flyout, notices).
pub fn drain(shared: &Shared) -> Vec<(u32, Fwd)> {
    let events = shared.services.ops.drain();
    if events.is_empty() {
        return Vec::new();
    }
    let mut out = Vec::new();
    let active = shared.active_win.get();
    let mut changed = false;
    for e in events {
        changed |= match e {
            OpsEvent::Evt(OpEvt::ItemsDone { op, changes }) => {
                out.extend(shared.main_windows().into_iter().map(|w| (w, Fwd::Own { op, changes: changes.clone() })));
                items_done(shared, op, &changes)
            }
            OpsEvent::Evt(e) => on_evt(shared, e, active),
            OpsEvent::Undo(rec) => on_undo_record(shared, rec),
            OpsEvent::HostLost => host_lost(shared),
            OpsEvent::HostUpdated => false,
        };
    }
    if !changed {
        return out;
    }
    shared.ops.borrow_mut().lang = shared.lang.get();
    let mut wins: Vec<u32> = shared.windows.borrow().iter().map(|w| w.win).collect();
    if wins.is_empty() {
        // A window that is not recorded yet (the Harness, a scene): the primary hears it.
        wins.extend(shared.hub.primary());
    }
    out.extend(wins.into_iter().map(|w| (w, Fwd::Ops)));
    out
}

/// The helper went away: running operations become stopped, and each window that asked for one hears it once.
fn host_lost(shared: &Shared) -> bool {
    let mut m = shared.ops.borrow_mut();
    let mut wins = Vec::new();
    for r in m.rows.iter_mut().filter(|r| r.state.running()) {
        r.state = OpState::Stopped;
        if !wins.contains(&r.win) {
            wins.push(r.win);
        }
    }
    let any = !wins.is_empty();
    for w in wins {
        m.notice(w, Pend::HostLost);
    }
    any
}

/// A delete's removals arrived: the row it chose takes the focus on the next Fwd::Ops. True when one is due.
fn items_done(shared: &Shared, op: OpId, changes: &[Change]) -> bool {
    let mut m = shared.ops.borrow_mut();
    let Some(r) = m.row_mut(op) else { return false };
    if r.focus.is_some() && changes.iter().any(|c| matches!(c.kind, ChangeKind::Recycled | ChangeKind::Deleted)) {
        r.focus_due = true;
    }
    r.focus_due
}

/// One event into the model; true when what a window shows changed.
fn on_evt(shared: &Shared, e: OpEvt, active: u32) -> bool {
    let mut redo = None;
    let changed = {
        let mut m = shared.ops.borrow_mut();
        match e {
            OpEvt::Accepted { op } => m.row_mut(op).map(|r| r.state = OpState::Running).is_some(),
            OpEvt::Started { op, kind, items, .. } => {
                let r = m.row_or_new(op, kind);
                r.state = OpState::Running;
                r.count = u64::from(items).max(r.count);
                true
            }
            OpEvt::Progress { op, done, total, bps, .. } => {
                let Some(r) = m.row_mut(op) else { return false };
                (r.done, r.total, r.bps) = (done, total, bps);
                if r.state == OpState::Queued {
                    r.state = OpState::Running;
                }
                true
            }
            OpEvt::NeedsInput { op, on } => m.row_mut(op).map(|r| r.needs_input = on).is_some(),
            OpEvt::ItemFailed { op, item, hr } => {
                let Some(r) = m.row_mut(op) else { return false };
                r.failed = r.failed.saturating_add(1);
                if r.failures.len() < FAILURES_KEPT {
                    r.failures.push((item, win_message(hr)));
                }
                true
            }
            OpEvt::Finished { op, result, undo, items, failed, .. } => {
                let Some(r) = m.row_mut(op) else { return false };
                r.state = match result {
                    OpResult::Done => OpState::Done,
                    OpResult::Cancelled => OpState::Cancelled,
                    OpResult::Failed | OpResult::Refused => OpState::Failed,
                };
                r.needs_input = false;
                r.failed = r.failed.max(failed);
                r.undoable = r.undoable || (undo.is_some() && r.kind != OpKind::Undo);
                if items > 0 {
                    r.count = u64::from(items);
                }
                r.done = r.total;
                if result == OpResult::Done {
                    redo = r.undo_of.clone().map(|rec| (op, rec));
                }
                true
            }
            OpEvt::Refused { op, why, detail } => {
                let (win, replaced) = match m.row(op) {
                    Some(r) => (r.win, r.undo_of.as_ref().map_or(0, |u| u.replaced)),
                    None => (active, 0),
                };
                m.rows.retain(|r| r.op != op);
                m.notice(win, Pend::Refused { why, detail: detail.to_string(), replaced });
                true
            }
            OpEvt::Snapshot { running } => {
                let any = !running.is_empty();
                for st in running {
                    let r = m.row_or_new(st.op, st.kind);
                    r.state = OpState::Running;
                    (r.done, r.total, r.needs_input) = (st.done, st.total, st.needs_input);
                }
                any
            }
            OpEvt::Pending { of, kind, done, total, .. } => {
                m.pending_kinds.insert(of, kind);
                m.notice(0, Pend::Pending { of, kind, done, total });
                true
            }
            OpEvt::DoneWhileAway { of, kind, items, failed, .. } => {
                if matches!(kind, OpKind::Recycle | OpKind::DeletePermanent) {
                    m.notice(0, Pend::Away { kind, items, failed });
                } else {
                    oled_sys::log!("files: an operation of kind {kind:?} finished while closed");
                }
                m.acks.push((0, of));
                true
            }
            OpEvt::ItemsDone { .. } | OpEvt::UndoPart { .. } => false,
        }
    };
    if let Some((op, rec)) = redo {
        shared.undo.borrow_mut().push_redo(UndoEntry { op, kind: forward_kind(rec.kind), record: rec });
    }
    changed
}

/// An undo record for an operation this process asked for goes on the undo stack (never for an undo itself).
fn on_undo_record(shared: &Shared, rec: UndoRecord) -> bool {
    let kind = {
        let mut m = shared.ops.borrow_mut();
        let Some(r) = m.row_mut(rec.of) else { return false };
        if r.win == 0 || r.kind == OpKind::Undo {
            return false;
        }
        r.undoable = true;
        r.kind
    };
    shared.undo.borrow_mut().push(UndoEntry { op: rec.of, kind, record: rec });
    true
}

/// Fwd::Ops: finishes are stamped, the chip and the line follow, this window's notices show, a delete's next row
/// takes the focus, and a window waiting for the operations window closes once that window is there.
pub fn on_fwd_ops(s: &mut FilesScreen, cx: &mut EventCx) {
    let now = cx.now_ms();
    let shared = Rc::clone(&s.shared);
    let fmt = shared.fmt.borrow().clone();
    let (notices, acks, focus) = {
        let mut m = shared.ops.borrow_mut();
        m.stamp(now);
        let notices = m.take_notices_in(s.win, cx.lang(), &fmt);
        let acks = m.take_acks(s.win);
        let mut focus = Vec::new();
        for r in m.rows.iter_mut().filter(|r| r.win == s.win && r.focus_due) {
            r.focus_due = false;
            let name = r.focus.take();
            let folder = r.folder.clone();
            if let Some(name) = name {
                focus.push((folder, name));
            }
        }
        (notices, acks, focus)
    };
    for n in notices {
        status::notice(s, cx, n);
    }
    for of in acks {
        shared.services.ops.ack_done(of);
    }
    for (folder, name) in focus {
        focus_row(s, cx, folder.as_ref(), &name);
    }
    sync_line(s, cx);
    arm_chip_timer(s, cx, now);
    cx.relayout();
    let (waits, made) = {
        let m = shared.ops.borrow();
        (m.close_after() == Some(s.win), m.ops_screen_made)
    };
    if waits && has_ops_window(&shared) {
        shared.ops.borrow_mut().set_close_after(None);
        // The operations window is open: this window closes as if asked again (opswin lets it now).
        if <FilesScreen as oled_ui::Screen>::event(s, cx, &Event::CloseRequested) == Reply::Close {
            cx.close_window();
        }
    } else if waits && made {
        // The operations window could not open: this window stays, and a later close asks again.
        shared.ops.borrow_mut().set_close_after(None);
    }
}

pub(crate) fn has_ops_window(shared: &Shared) -> bool {
    shared.windows.borrow().iter().any(|w| w.kind == WinKind::Ops)
}

/// The row of that name in the active tab, when it still shows the folder, takes the focus and the selection.
fn focus_row(s: &mut FilesScreen, cx: &mut EventCx, folder: Option<&WidePath>, name: &[u8]) {
    let t = s.tabs.active_mut();
    if folder.is_some_and(|f| f != t.table.model().listing().dir()) {
        return;
    }
    let Some(id) = t.table.model().listing().find(name) else { return };
    let key = key_of(id);
    t.table.set_selection(cx, Selection::Single(key));
    t.table.set_focused(cx, key, ScrollTo::Nearest);
}

fn chip_timer() -> oled_ui::TimerId {
    window::timer(names::TIMER_CHIP, TabId(0))
}

/// Timer kind 6 is armed only while a finished row is held: for the moment the next hold ends.
fn arm_chip_timer(s: &mut FilesScreen, cx: &mut EventCx, now: u64) {
    let due = s.shared.ops.borrow().next_due(now);
    match due {
        Some(at) => cx.set_timer(chip_timer(), u32::try_from(at - now).unwrap_or(u32::MAX)),
        None => cx.cancel_timer(chip_timer()),
    }
}

/// Timer kind 6: the held finishes that are over go; the timer is armed again only for the next one.
pub fn on_chip_timer(s: &mut FilesScreen, cx: &mut EventCx) {
    let now = cx.now_ms();
    s.shared.ops.borrow_mut().prune(now);
    if s.flyout.open && s.shared.ops.borrow().rows().is_empty() {
        close_flyout(s, cx);
    }
    arm_chip_timer(s, cx, now);
    cx.relayout();
}

/// The 2 px line follows the running operations: their aggregate, or a sweep while none knows its total.
fn sync_line(s: &mut FilesScreen, cx: &mut EventCx) {
    let (running, value) = {
        let m = s.shared.ops.borrow();
        (m.running() > 0, m.aggregate())
    };
    if running || s.flyout.line_on {
        s.flyout.progress.set(cx, value, running);
    }
    s.flyout.line_on = running;
}

// ---- Del --------------------------------------------------------------------------------------------------------

/// The system's classifier: per folder, the facts once (a share, a network or removable drive reads nothing more than
/// its drive type), then classify_facts per item with the item's size from the store. Runs on a Bg lane.
pub fn system_facts(items: &[(WidePath, u64)]) -> Vec<(WidePath, Recyclability)> {
    const DRIVE_REMOVABLE: u32 = 2;
    const DRIVE_REMOTE: u32 = 4;
    const DRIVE_CDROM: u32 = 5;
    let mut by_folder: Vec<(Option<WidePath>, BinFacts)> = Vec::new();
    let mut out = Vec::with_capacity(items.len());
    for (p, size) in items {
        let folder = p.parent();
        if !by_folder.iter().any(|(f, _)| *f == folder) {
            let facts = if p.is_unc() {
                BinFacts { drive_type: DRIVE_REMOTE, unc: true, size: 0, bin_free: None, bin_disabled: None }
            } else {
                let dt = drive_type(p);
                if matches!(dt, DRIVE_REMOTE | DRIVE_REMOVABLE | DRIVE_CDROM) {
                    BinFacts { drive_type: dt, unc: false, size: 0, bin_free: None, bin_disabled: None }
                } else {
                    gather_facts(p)
                }
            };
            by_folder.push((folder.clone(), facts));
        }
        let Some((_, f)) = by_folder.iter().find(|(x, _)| *x == folder) else { continue };
        let mine = BinFacts {
            drive_type: f.drive_type,
            unc: f.unc,
            size: *size,
            bin_free: f.bin_free,
            bin_disabled: f.bin_disabled,
        };
        out.push((p.clone(), classify_facts(&mine)));
    }
    out
}

/// GetDriveTypeW of the item's drive root ("X:\"); 0 (unknown) for anything else.
fn drive_type(p: &WidePath) -> u32 {
    let u = p.units();
    let (Some(&d), Some(&c)) = (u.first(), u.get(1)) else { return 0 };
    if c != u16::from(b':') || !u8::try_from(d).is_ok_and(|b| b.is_ascii_alphabetic()) {
        return 0;
    }
    let root = [d, u16::from(b':'), u16::from(b'\\'), 0];
    // SAFETY: root is NUL-terminated and lives for the call; the call reads no file.
    unsafe { GetDriveTypeW(PCWSTR(root.as_ptr())) }
}

/// The selected items of the active tab, in model order.
fn selected(s: &FilesScreen) -> Vec<WidePath> {
    let t = s.tabs.active();
    t.table.model().selected_paths(t.table.selection())
}

/// Del, Ctrl+D, the menu's Delete, an intercepted Delete and the redo of a restore (U3, U4): the items are classified
/// on their lane first; all recyclable: one Recycle; any other answer: the bar, which alone sends DeletePermanent.
pub fn delete_flow(s: &mut FilesScreen, cx: &mut EventCx, items: Vec<WidePath>) -> Reply {
    let _ = cx;
    // A redo's mark is taken here whatever follows, so it never reaches a later, unrelated request.
    let redo = s.shared.ops.borrow_mut().take_redo_next();
    if items.is_empty() {
        return Reply::None;
    }
    classify(s, items, redo);
    Reply::Redraw
}

/// Starts the classification job of the items for this window (F3.2, F3.3).
fn classify(s: &mut FilesScreen, items: Vec<WidePath>, redo: bool) {
    let Some(first) = items.first().cloned() else { return };
    let shared = Rc::clone(&s.shared);
    let input: Vec<(WidePath, u64)> = {
        let m = s.tabs.active().table.model();
        let l = m.listing();
        items
            .iter()
            .map(|p| {
                let mut name = Vec::new();
                if let Some(n) = p.file_name() {
                    crate::listing::collate::utf16_to_wtf8(n, &mut name);
                }
                let in_dir = p.parent().as_ref() == Some(l.dir());
                let size = l
                    .find(&name)
                    .filter(|&id| in_dir && !l.is_dir(id))
                    .and_then(|id| l.entry(id))
                    .map_or(0, |e| e.size);
                (p.clone(), size)
            })
            .collect()
    };
    let req = shared.next_req.get();
    shared.next_req.set(req + 1);
    let t = s.tabs.active();
    s.bar.pending = Some(bar::Pending { req, tab: t.id, at: t.nav.location().clone(), redo });
    let lane = bg::lane_of(&first, &shared.drives.borrow());
    let facts = shared.ops.borrow().facts();
    shared.services.bg.run(s.win, lane, Box::new(move || Some(JobDone::Facts { req, items: facts(&input) })));
}

/// JobDone::Facts of this window's pending Del: every item Yes, the Recycle; any No (Unknown included), the bar. A
/// navigation or a tab switch since the Del drops it. False when the job is not ours.
pub fn on_facts(s: &mut FilesScreen, cx: &mut EventCx, req: u64, items: Vec<(WidePath, Recyclability)>) -> bool {
    if s.bar.pending.as_ref().is_none_or(|p| p.req != req) {
        return false;
    }
    let Some(p) = s.bar.pending.take() else { return false };
    let t = s.tabs.active();
    if t.id != p.tab || *t.nav.location() != p.at {
        return true;
    }
    let (yes, no): (Vec<_>, Vec<_>) = items.into_iter().partition(|(_, r)| *r == Recyclability::Yes);
    let yes: Vec<WidePath> = yes.into_iter().map(|(p, _)| p).collect();
    if no.is_empty() {
        if !yes.is_empty() {
            if p.redo {
                s.shared.ops.borrow_mut().mark_redo_next();
            }
            request(s, cx, MutReq::Recycle(yes));
            // A request the gate refused leaves the mark: it must not reach a later, unrelated request.
            s.shared.ops.borrow_mut().take_redo_next();
        }
        return true;
    }
    let no: Vec<WidePath> = no.into_iter().map(|(p, _)| p).collect();
    bar::open(s, cx, yes, no, p.redo);
    true
}

/// Rename, Delete, Delete permanently for the context menu (FB-MENU places them): the window's commands, so a pick
/// runs the same flow as the key. Delete permanently is the danger item with the note "Can't be undone".
pub fn f3_items(s: &FilesScreen, sel: &[WidePath]) -> Vec<MenuItem> {
    if sel.is_empty() {
        return Vec::new();
    }
    let id = |c: Cmd| oled_ui::CommandId(c as u32);
    let mut out = Vec::new();
    if sel.len() == 1 {
        out.push(MenuItem::Command(id(Cmd::Rename)));
    }
    out.push(MenuItem::Command(id(Cmd::Recycle)));
    let lang = s.shared.lang.get();
    let permanent = <FilesScreen as oled_ui::Screen>::commands(s).get(id(Cmd::DeletePermanent));
    out.push(MenuItem::Custom {
        id: id(Cmd::DeletePermanent),
        label: permanent
            .map(|c| c.label.clone())
            .unwrap_or_else(|| lang.kit(oled_ui::strings::k::KIT_VERB_DELETE_PERMANENTLY)),
        glyph: permanent.and_then(|c| c.glyph),
        chord: permanent.and_then(|c| c.chord).map(|c| Text::Owned(c.label(lang))),
        enabled: true,
        checked: None,
        note: Some(t(k::FILES_MENU_CANT_UNDO, lang)),
        danger: true,
    });
    out
}

/// Recycle, DeletePermanent, Undo, Redo, OpsFlyout.
pub fn cmd(s: &mut FilesScreen, cx: &mut EventCx, c: Cmd) -> Reply {
    match c {
        Cmd::Recycle => {
            let items = selected(s);
            delete_flow(s, cx, items)
        }
        Cmd::DeletePermanent => {
            let items = selected(s);
            if items.is_empty() {
                return Reply::None;
            }
            request(s, cx, MutReq::DeletePermanent(items));
            Reply::Redraw
        }
        Cmd::Undo => undo::undo(s, cx),
        Cmd::Redo => undo::redo(s, cx),
        Cmd::OpsFlyout => {
            if s.flyout.open {
                close_flyout(s, cx);
                Reply::Relayout
            } else {
                flyout_open(s, cx, None)
            }
        }
        _ => Reply::None,
    }
}

/// [Recycle the rest] / [Delete the rest]: the rest of an interrupted operation, through the gate like every request
/// (an operation of an earlier process has no recorded items, so the gate refuses it until real data is on).
pub fn on_rest(s: &mut FilesScreen, cx: &mut EventCx, of: OpId) -> Reply {
    request(s, cx, MutReq::ResumeRest(of));
    Reply::Redraw
}

// ---- texts ------------------------------------------------------------------------------------------------------

fn counted(n: u64, one: oled_sys::i18n::S, other: oled_sys::i18n::S, lang: UiLang, fmt: &Fmt) -> Text {
    if n == 1 {
        t(one, lang)
    } else {
        tf(other, lang, &[("n", &fmt.count(n))])
    }
}

/// What a running operation does: "Deleting 14 items", "Renaming notes.txt".
pub fn what_text(r: &OpRow, lang: UiLang, fmt: &Fmt) -> Text {
    let n = r.count.max(r.items.len() as u64);
    let item_name = r.items.first().map(name_of).unwrap_or_default();
    match r.kind {
        OpKind::Recycle => counted(n, k::FILES_OPS_RECYCLING_ONE, k::FILES_OPS_RECYCLING_OTHER, lang, fmt),
        OpKind::Rename => tf(k::FILES_OPS_RENAMING, lang, &[("name", &item_name)]),
        OpKind::NewFolder => tf(k::FILES_OPS_CREATING, lang, &[("name", r.name.as_deref().unwrap_or_default())]),
        OpKind::Undo => match r.undo_of.as_ref().map(|u| u.kind) {
            Some(UndoKind::RenameBack) => tf(k::FILES_OPS_RENAMING, lang, &[("name", &undo_to_name(r))]),
            Some(UndoKind::RecycleNewFolder) => {
                counted(n, k::FILES_OPS_RECYCLING_ONE, k::FILES_OPS_RECYCLING_OTHER, lang, fmt)
            }
            _ => counted(n, k::FILES_OPS_RESTORING_ONE, k::FILES_OPS_RESTORING_OTHER, lang, fmt),
        },
        OpKind::DeletePermanent | OpKind::Copy | OpKind::Move | OpKind::Resume => {
            counted(n, k::FILES_OPS_DELETING_ONE, k::FILES_OPS_DELETING_OTHER, lang, fmt)
        }
    }
}

/// The name an undo of a rename starts from: the item's name after the rename.
fn undo_to_name(r: &OpRow) -> String {
    r.undo_of.as_ref().and_then(|u| u.entries.first()).map(|e| name_of(&e.to)).unwrap_or_default()
}

/// What a finished operation did: "Deleted 14 items", "Renamed to b.txt".
pub fn done_text(r: &OpRow, lang: UiLang, fmt: &Fmt) -> Text {
    let n = r.count.max(r.items.len() as u64);
    match r.kind {
        OpKind::Recycle => counted(n, k::FILES_OPS_DONE_RECYCLED_ONE, k::FILES_OPS_DONE_RECYCLED_OTHER, lang, fmt),
        OpKind::Rename => tf(k::FILES_OPS_DONE_RENAMED, lang, &[("name", r.name.as_deref().unwrap_or_default())]),
        OpKind::NewFolder => tf(k::FILES_OPS_DONE_CREATED, lang, &[("name", r.name.as_deref().unwrap_or_default())]),
        OpKind::Undo => match r.undo_of.as_ref().map(|u| u.kind) {
            Some(UndoKind::RenameBack) => {
                let back = r.undo_of.as_ref().and_then(|u| u.entries.first()).map(|e| name_of(&e.from));
                tf(k::FILES_OPS_DONE_RENAMED, lang, &[("name", &back.unwrap_or_default())])
            }
            Some(UndoKind::RecycleNewFolder) => {
                counted(n, k::FILES_OPS_DONE_RECYCLED_ONE, k::FILES_OPS_DONE_RECYCLED_OTHER, lang, fmt)
            }
            _ => tf(k::FILES_OPS_DONE_RESTORED_OTHER, lang, &[("n", &fmt.count(n))]),
        },
        OpKind::DeletePermanent | OpKind::Copy | OpKind::Move | OpKind::Resume => {
            counted(n, k::FILES_OPS_DONE_DELETED_ONE, k::FILES_OPS_DONE_DELETED_OTHER, lang, fmt)
        }
    }
}

/// "62 %"; with a rate: "62 % · 48.2 MB/s · about 1 min left" (under a minute in seconds, else in minutes).
pub fn meta_text(r: &OpRow, lang: UiLang, fmt: &Fmt) -> Text {
    let p = percent(r.done, r.total);
    if r.bps == 0 || r.total <= r.done {
        return Text::Owned(p.to_string() + " %");
    }
    let secs = (r.total - r.done) / r.bps.max(1);
    let eta = if secs < 60 {
        tf(k::FILES_OPS_ETA_S, lang, &[("n", &secs.max(1).to_string())])
    } else {
        tf(k::FILES_OPS_ETA_MIN, lang, &[("n", &((secs + 30) / 60).to_string())])
    };
    tf(k::FILES_OPS_RATE, lang, &[("p", &p.to_string()), ("rate", &fmt.size(r.bps, false)), ("eta", &eta)])
}

fn percent(done: u64, total: u64) -> u32 {
    if total == 0 {
        return 0;
    }
    ((done.min(total) as f64 / total as f64) * 100.0).floor() as u32
}

/// What the chip shows now.
#[derive(Clone, Debug, PartialEq)]
pub struct ChipText {
    pub text: Text,
    /// "2 items couldn't be changed", in ACCENT_DANGER after a dot.
    pub failed: Option<Text>,
    /// "Undo" after a dot: a second click target (the operation the stack's top undoes).
    pub undo: Option<Text>,
    /// The operation waiting for an answer in a Windows dialog.
    pub attention: Option<OpId>,
    /// The failure count as shown inside `failed`: the only part in ACCENT_DANGER.
    pub count: Option<String>,
}

/// The chip: while something runs, "{what} · {p} %" (one) or "{n} operations · {p} %", NeedsInput first; for the hold
/// after the last finish, what it did (failures in red, Undo when the stack's top undoes it); else nothing.
pub fn chip_text(m: &OpsModel, undo_top: Option<OpId>, now: u64, lang: UiLang, fmt: &Fmt) -> Option<ChipText> {
    let running: Vec<&OpRow> = m.rows().iter().filter(|r| r.state.running()).collect();
    if let Some(r) = running.iter().find(|r| r.needs_input) {
        return Some(ChipText {
            text: t(k::FILES_OPS_NEEDS_INPUT, lang),
            failed: None,
            undo: None,
            attention: Some(r.op),
            count: None,
        });
    }
    let p = (m.aggregate().unwrap_or(0.0) * 100.0).floor() as u32;
    match running.as_slice() {
        [] => {}
        [one] => {
            let what = what_text(one, lang, fmt);
            let text = tf(k::FILES_OPS_CHIP, lang, &[("what", &what), ("p", &p.to_string())]);
            return Some(ChipText { text, failed: None, undo: None, attention: None, count: None });
        }
        many => {
            let n = fmt.count(many.len() as u64);
            let text = tf(k::FILES_OPS_CHIP_MANY, lang, &[("n", &n), ("p", &p.to_string())]);
            return Some(ChipText { text, failed: None, undo: None, attention: None, count: None });
        }
    }
    let last = m.recent(now).last()?;
    let failed = (last.failed > 0).then(|| failed_text(u64::from(last.failed), lang, fmt));
    let count = (last.failed > 0).then(|| fmt.count(u64::from(last.failed)));
    let undo = (last.undoable && undo_top == Some(last.op)).then(|| t(k::FILES_MORE_UNDO_PLAIN, lang));
    Some(ChipText { text: done_text(last, lang, fmt), failed, undo, attention: None, count })
}

// ---- the chip and the line (status line) -------------------------------------------------------------------------

const ID_CHIP: WidgetId = WidgetId(300);
const ID_LINE: WidgetId = WidgetId(301);
const ID_FLY: WidgetId = WidgetId(302);
/// The rows' IconButtons: cancel at 310 + 2i, pause at 311 + 2i, for the first ROW_BUTTONS running rows.
const ID_ROW_BUTTONS: u32 = 310;
const ROW_BUTTONS: usize = 16;
/// The slot bit of the rows' own nodes (the links use the low slots).
const ROW_NODE: u32 = 0x1000_0000;

const SEP_DOT: &str = " \u{b7} ";

/// Logical px.
const CHIP_PAD: i32 = 10;
const CHIP_BACK: i32 = 8;
const FLY_PAD: i32 = 12;
const FLY_GAP: i32 = 4;
const ROW_TITLE: i32 = 34;
const ROW_LINE: i32 = 18;
const RAIL_H: i32 = 4;
const SEP_BLOCK: i32 = 13;
const SEP_GAP: i32 = 6;
const BTN: i32 = 28;
const LINK_PAD: i32 = 10;
const DETAILS_SHOWN: usize = 8;

/// The chip as the last layout placed it.
#[derive(Clone, Debug, Default)]
struct ChipLayout {
    rect: RectP,
    text: Option<ChipText>,
    text_r: RectP,
    failed_r: RectP,
    undo_r: RectP,
    /// The backdrop that hides the status text under the chip.
    back: RectP,
}

/// The chip, the line and the flyout of one main window.
pub struct Flyout {
    pub(crate) open: bool,
    chip: ChipLayout,
    pub(crate) progress: Progress,
    pub(crate) line_on: bool,
    /// The status rect of the last layout (the flyout anchors on it).
    status: RectP,
    panel: RectP,
    rows: Rows,
}

impl std::fmt::Debug for Flyout {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Flyout").field("open", &self.open).finish()
    }
}

impl Default for Flyout {
    fn default() -> Flyout {
        Flyout::new()
    }
}

impl Flyout {
    pub fn new() -> Flyout {
        Flyout {
            open: false,
            chip: ChipLayout::default(),
            progress: Progress::new(ID_LINE),
            line_on: false,
            status: RectP::EMPTY,
            panel: RectP::EMPTY,
            rows: Rows::new(),
        }
    }

    pub fn is_open(&self) -> bool {
        self.open
    }

    /// The chip's rect and text as laid out.
    pub fn chip(&self) -> Option<(RectP, String)> {
        let c = self.chip.text.as_ref()?;
        let mut s = c.text.to_string();
        if let Some(f) = &c.failed {
            s.push_str(SEP_DOT);
            s.push_str(f);
        }
        if let Some(u) = &c.undo {
            s.push_str(SEP_DOT);
            s.push_str(u);
        }
        Some((self.chip.rect, s))
    }

    /// The rect of the chip's Undo part, when it has one.
    pub fn chip_undo(&self) -> Option<RectP> {
        self.chip.text.as_ref()?.undo.as_ref()?;
        Some(self.chip.undo_r)
    }

    /// Whether the 2 px line runs.
    pub fn line_on(&self) -> bool {
        self.line_on
    }

    /// The panel's rect while open.
    pub fn panel(&self) -> Option<RectP> {
        self.open.then_some(self.panel)
    }

    /// The rows the panel shows, as text lines.
    pub fn row_texts(&self) -> Vec<String> {
        self.rows.texts()
    }

    /// The rect of the cancel button of the running row at that index, as laid out.
    pub fn cancel_rect(&self, row: usize) -> Option<RectP> {
        self.rows.cancel_rect(row)
    }

    /// The rect of the link (Undo, Details, Show) of the row at that index.
    pub fn link_rect(&self, row: usize) -> Option<RectP> {
        self.rows.link_rect(row)
    }
}

/// The window's chip, line and flyout (what tests read).
pub fn flyout_of(s: &FilesScreen) -> &Flyout {
    &s.flyout
}

fn width(cx: &mut LayoutCx, s: &str, role: TypeRole) -> i32 {
    let font = cx.font(role);
    cx.text().width(s, font)
}

/// The chip in the centre of the status line, over a backdrop, and the 2 px line along its top edge; the whole rect
/// stays the status's (the chip covers what runs under it).
pub fn chip_layout(s: &mut FilesScreen, cx: &mut LayoutCx, r: RectP) -> RectP {
    let fmt = s.shared.fmt.borrow().clone();
    let undo_top = s.shared.undo.borrow().top_op();
    // The bar takes the status line while open: no chip over its buttons.
    let text =
        if s.bar.is_open() { None } else { chip_text(&s.shared.ops.borrow(), undo_top, cx.now_ms(), cx.lang(), &fmt) };
    let fl = &mut s.flyout;
    fl.status = r;
    if fl.line_on {
        fl.progress.layout(cx, r);
    }
    let Some(text) = text else {
        fl.chip = ChipLayout::default();
        return r;
    };
    let role = TypeRole::Caption;
    let pad = cx.px(CHIP_PAD);
    let main_w = width(cx, &text.text, role);
    let fail_w = text.failed.as_ref().map_or(0, |f| width(cx, &format!("{SEP_DOT}{f}"), role));
    let undo_w = text.undo.as_ref().map_or(0, |u| width(cx, &format!("{SEP_DOT}{u}"), role));
    let h = cx.px(cx.tokens().size.chip_h).min(r.h());
    let w = (pad * 2 + main_w + fail_w + undo_w).min(r.w());
    let x = r.l + (r.w() - w) / 2;
    let y = r.t + (r.h() - h) / 2;
    let rect = RectP::wh(x, y, w, h);
    let text_r = RectP { l: x + pad, r: (x + pad + main_w).min(rect.r), ..rect };
    let failed_r = RectP { l: text_r.r, r: (text_r.r + fail_w).min(rect.r), ..rect };
    let undo_r = RectP { l: failed_r.r, r: (failed_r.r + undo_w).min(rect.r), ..rect };
    let line_h = cx.px(cx.tokens().size.progress_h);
    let back = RectP { l: rect.l - cx.px(CHIP_BACK), r: rect.r + cx.px(CHIP_BACK), t: r.t + line_h, b: r.b };
    let mut label = text.text.to_string();
    if let Some(f) = &text.failed {
        label.push_str(SEP_DOT);
        label.push_str(f);
    }
    let node = Node::new(NodeId::new(ID_CHIP, 0), Role::Button, rect)
        .flags(NodeFlags::HOVER_FADE)
        .focus(Focusable::Tab)
        .label(Text::Owned(label))
        .actions(Actions::CLICK | Actions::FOCUS);
    cx.push(node);
    if let Some(u) = &text.undo {
        let node = Node::new(NodeId::new(ID_CHIP, 1), Role::Button, undo_r)
            .parent(NodeId::new(ID_CHIP, 0))
            .focus(Focusable::Tab)
            .label(u.clone())
            .actions(Actions::CLICK | Actions::FOCUS);
        cx.push(node);
    }
    fl.chip = ChipLayout { rect, text: Some(text), text_r, failed_r, undo_r, back };
    r
}

/// A line of Caption text centred in the rect's height.
fn caption_in(cx: &mut PaintCx, s: &str, r: RectP, ink: oled_ui::Ink) {
    let line = cx.px(TypeRole::Caption.spec().line);
    let y = r.t + (r.h() - line) / 2;
    cx.text_in(s, TypeRole::Caption, RectP { t: y, b: y + line, ..r }, Align::Start, Trim::End, ink);
}

/// One line of text from the rect's left with its failure count in `danger` and the rest in `ink` (red is only for
/// failure counts); returns the width drawn.
fn text_counted(cx: &mut PaintCx, s: &str, count: &str, role: TypeRole, r: RectP, ink: Ink, danger: Ink) -> i32 {
    let (before, mid, after) = match s.find(count) {
        Some(i) if !count.is_empty() => (&s[..i], &s[i..i + count.len()], &s[i + count.len()..]),
        _ => (s, "", ""),
    };
    let mut x = r.l;
    for (part, color) in [(before, ink), (mid, danger), (after, ink)] {
        if !part.is_empty() && x < r.r {
            x += cx.text_in(part, role, RectP { l: x, ..r }, Align::Start, Trim::End, color);
        }
    }
    x - r.l
}

pub fn chip_paint(s: &FilesScreen, cx: &mut PaintCx) {
    let fl = &s.flyout;
    if fl.line_on {
        fl.progress.paint(cx);
    }
    let Some(text) = &fl.chip.text else { return };
    let c = cx.tokens().color.clone();
    let ch = &fl.chip;
    cx.fill(ch.back, 0, c.bg);
    let id = NodeId::new(ID_CHIP, 0);
    let radius = ch.rect.h() / 2;
    let (fill, ink) = if text.attention.is_some() {
        (c.accent_attention, c.bg)
    } else if cx.pressed(id) {
        (c.fill_press, c.fg_primary)
    } else if cx.hot(id) {
        (c.fill_control_hot, c.fg_primary)
    } else {
        (c.fill_control, c.fg_primary)
    };
    cx.fill(ch.rect, radius, fill);
    caption_in(cx, &text.text, ch.text_r, ink);
    if let Some(f) = &text.failed {
        let line = cx.px(TypeRole::Caption.spec().line);
        let y = ch.failed_r.t + (ch.failed_r.h() - line) / 2;
        let r = RectP { t: y, b: y + line, ..ch.failed_r };
        let sep = cx.text_in(SEP_DOT, TypeRole::Caption, r, Align::Start, Trim::End, ink);
        let count = text.count.as_deref().unwrap_or_default();
        text_counted(cx, f, count, TypeRole::Caption, RectP { l: r.l + sep, ..r }, ink, c.accent_danger);
    }
    if let Some(u) = &text.undo {
        let hot = cx.hot(NodeId::new(ID_CHIP, 1));
        caption_in(cx, &format!("{SEP_DOT}{u}"), ch.undo_r, if hot { c.fg_hot } else { ink });
        if cx.focused(NodeId::new(ID_CHIP, 1)) {
            cx.focus_ring(ch.undo_r, cx.px(cx.tokens().radius.xs), false);
        }
    }
    if cx.focused(id) {
        cx.focus_ring(ch.rect, radius, false);
    }
}

/// A click (or Enter, Space) on the chip: NeedsInput brings the Windows dialog forward; else the flyout opens. On its
/// Undo part: Ctrl+Z.
fn chip_event(s: &mut FilesScreen, cx: &mut EventCx, ev: &Event) -> Option<Reply> {
    let attention = s.flyout.chip.text.as_ref()?.attention;
    let target = cx.target()?;
    let (main, undo_part) = (NodeId::new(ID_CHIP, 0), NodeId::new(ID_CHIP, 1));
    if target != main && target != undo_part {
        return None;
    }
    match ev {
        Event::Click { button: MouseButton::Left, .. } => {}
        Event::Key { key: Key::ENTER | Key::SPACE, down: true, .. } => cx.consume(),
        _ => return None,
    }
    if target == undo_part {
        return Some(undo::undo(s, cx));
    }
    if let Some(op) = attention {
        rows_out(&s.shared, RowsOut::Show(op));
        return Some(Reply::Redraw);
    }
    Some(flyout_open(s, cx, None))
}

// ---- the rows of the flyout and of the operations window -----------------------------------------------------------

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Link {
    Undo = 0,
    Details = 1,
    Show = 2,
}

/// What a click in the rows asks for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum RowsOut {
    Cancel(OpId),
    Pause(OpId, bool),
    Undo(OpId),
    Details(OpId),
    Show(OpId),
}

struct RowLayout {
    op: OpId,
    title: Text,
    title_r: RectP,
    strong: bool,
    /// A failure row's count: the only part of its title in ACCENT_DANGER.
    count: Option<String>,
    sub: Option<(Text, RectP)>,
    rail: Option<(f32, RectP)>,
    meta: Option<(Text, RectP)>,
    link: Option<(Link, Text, RectP)>,
    details: Vec<(Text, RectP)>,
    /// Index into the IconButtons, when this row has them, and the cancel button's rect.
    buttons: Option<usize>,
    cancel_r: Option<RectP>,
    paused: bool,
    /// The separator under the group: its y and its right end.
    sep: Option<(i32, i32)>,
}

/// The rows of the operations: one group per operation, in the order they started.
pub(crate) struct Rows {
    rows: Vec<RowLayout>,
    cancel: Vec<IconButton>,
    pause: Vec<IconButton>,
    paused: HashMap<OpId, bool>,
}

fn link_id(row: usize, link: Link) -> NodeId {
    NodeId::new(ID_FLY, 1 + row as u32 * 4 + link as u32)
}

impl Rows {
    pub(crate) fn new() -> Rows {
        Rows { rows: Vec::new(), cancel: Vec::new(), pause: Vec::new(), paused: HashMap::new() }
    }

    pub(crate) fn texts(&self) -> Vec<String> {
        let mut out = Vec::new();
        for r in &self.rows {
            let mut line = r.title.to_string();
            if let Some((_, t, _)) = &r.link {
                line.push_str(SEP_DOT);
                line.push_str(t);
            }
            out.push(line);
            out.extend(r.sub.iter().map(|(t, _)| t.to_string()));
            out.extend(r.meta.iter().map(|(t, _)| t.to_string()));
            out.extend(r.details.iter().map(|(t, _)| t.to_string()));
        }
        out
    }

    fn cancel_rect(&self, row: usize) -> Option<RectP> {
        self.rows.get(row)?.cancel_r
    }

    fn link_rect(&self, row: usize) -> Option<RectP> {
        self.rows.get(row)?.link.as_ref().map(|(_, _, r)| *r)
    }

    /// The height the rows of the model take (logical px), without the padding.
    pub(crate) fn height_of(m: &OpsModel) -> i32 {
        let mut h = 0;
        for (i, r) in m.rows().iter().enumerate() {
            if i > 0 {
                h += SEP_BLOCK;
            }
            h += ROW_TITLE;
            if r.state.running() {
                h += ROW_LINE * 3;
            } else if r.state == OpState::Stopped {
                h += ROW_LINE;
            } else if r.failed > 0 && r.opened {
                h += ROW_LINE * r.failures.len().min(DETAILS_SHOWN) as i32;
            }
        }
        h
    }

    /// Lays the rows of the model out from the top of the rect; returns the bottom.
    pub(crate) fn layout(
        &mut self,
        cx: &mut LayoutCx,
        m: &OpsModel,
        undo_top: Option<OpId>,
        fmt: &Fmt,
        r: RectP,
    ) -> i32 {
        let lang = cx.lang();
        let (line, title_h) = (cx.px(ROW_LINE), cx.px(ROW_TITLE));
        self.rows.clear();
        let mut y = r.t;
        let mut buttons = 0usize;
        let count = m.rows().len();
        for (i, op) in m.rows().iter().enumerate() {
            let title_r = RectP { t: y, b: y + title_h, ..r };
            let mut row = RowLayout {
                op: op.op,
                title: Text::Borrowed(""),
                title_r,
                strong: true,
                count: None,
                sub: None,
                rail: None,
                meta: None,
                link: None,
                details: Vec::new(),
                buttons: None,
                cancel_r: None,
                paused: self.paused.get(&op.op).copied().unwrap_or(false),
                sep: None,
            };
            // The row's own node first: its buttons and link, pushed after it, lie on top of it.
            let lines = if op.state.running() {
                3
            } else if op.state == OpState::Stopped {
                1
            } else if op.failed > 0 && op.opened {
                op.failures.len().min(DETAILS_SHOWN) as i32
            } else {
                0
            };
            let group = RectP { t: title_r.t, b: title_r.b + line * lines, ..r };
            let label = if op.state.running() || op.state == OpState::Stopped {
                what_text(op, lang, fmt)
            } else if op.failed > 0 {
                failed_text(u64::from(op.failed), lang, fmt)
            } else {
                done_text(op, lang, fmt)
            };
            cx.push(Node::new(NodeId::new(ID_FLY, ROW_NODE | i as u32), Role::ListItem, group).label(label));
            y = title_r.b;
            if op.state.running() {
                row.title = what_text(op, lang, fmt);
                let sub = if op.needs_input {
                    row.link = Some((Link::Show, t(k::FILES_OPS_SHOW, lang), RectP::EMPTY));
                    t(k::FILES_OPS_WAITING, lang)
                } else {
                    Text::Owned(op.folder.as_ref().map(name_of).unwrap_or_default())
                };
                row.sub = Some((sub, RectP { t: y, b: y + line, ..r }));
                y += line;
                let rail_t = y + (line - cx.px(RAIL_H)) / 2;
                let v = if op.total > 0 { op.done.min(op.total) as f32 / op.total as f32 } else { 0.0 };
                row.rail = Some((v, RectP { t: rail_t, b: rail_t + cx.px(RAIL_H), ..r }));
                y += line;
                row.meta = Some((meta_text(op, lang, fmt), RectP { t: y, b: y + line, ..r }));
                y += line;
                if buttons < ROW_BUTTONS {
                    row.buttons = Some(buttons);
                    buttons += 1;
                }
            } else if op.state == OpState::Stopped {
                row.title = what_text(op, lang, fmt);
                row.sub = Some((t(k::FILES_OPS_HOST_LOST, lang), RectP { t: y, b: y + line, ..r }));
                y += line;
            } else if op.failed > 0 {
                row.title = failed_text(u64::from(op.failed), lang, fmt);
                row.strong = false;
                row.count = Some(fmt.count(u64::from(op.failed)));
                if op.opened {
                    for (p, msg) in op.failures.iter().take(DETAILS_SHOWN) {
                        let text = Text::Owned(format!("{}{SEP_DOT}{msg}", name_of(p)));
                        row.details.push((text, RectP { t: y, b: y + line, ..r }));
                        y += line;
                    }
                } else {
                    row.link = Some((Link::Details, t(k::FILES_OPS_DETAILS, lang), RectP::EMPTY));
                }
            } else {
                row.title = done_text(op, lang, fmt);
                row.strong = false;
                if op.undoable && undo_top == Some(op.op) {
                    row.link = Some((Link::Undo, t(k::FILES_MORE_UNDO_PLAIN, lang), RectP::EMPTY));
                }
            }
            // The title row: the text, then the link, then the buttons at the right end.
            let mut right = r.r;
            let size = cx.px(BTN);
            let by = title_r.t + (title_r.h() - size) / 2;
            if let Some(b) = row.buttons {
                self.ensure(b, lang);
                let cancel_r = RectP::wh(right - size, by, size, size);
                if let Some(btn) = self.cancel.get_mut(b) {
                    btn.layout(cx, cancel_r);
                }
                row.cancel_r = Some(cancel_r);
                right = cancel_r.l - cx.px(4);
                if PAUSE_OK {
                    let pause_r = RectP::wh(right - size, by, size, size);
                    if let Some(btn) = self.pause.get_mut(b) {
                        btn.layout(cx, pause_r);
                    }
                    right = pause_r.l - cx.px(4);
                }
            }
            if let Some((link, text, rect)) = row.link.as_mut() {
                let w = width(cx, text, TypeRole::BodyStrong) + cx.px(LINK_PAD) * 2;
                *rect = RectP::wh(right - w, by, w, size);
                right = rect.l - cx.px(4);
                let node = Node::new(link_id(i, *link), Role::Button, *rect)
                    .flags(NodeFlags::HOVER_FADE)
                    .focus(Focusable::Tab)
                    .label(text.clone())
                    .actions(Actions::CLICK | Actions::FOCUS);
                cx.push(node);
            }
            row.title_r = RectP { r: right, ..title_r };
            if i + 1 < count {
                row.sep = Some((y + cx.px(SEP_GAP), r.r));
                y += cx.px(SEP_BLOCK);
            }
            self.rows.push(row);
        }
        y
    }

    /// The IconButtons of row slot b exist.
    fn ensure(&mut self, b: usize, lang: UiLang) {
        while self.cancel.len() <= b {
            let i = self.cancel.len() as u32;
            let cancel = IconButton::new(WidgetId(ID_ROW_BUTTONS + 2 * i), Glyph::Close, t(k::FILES_CMD_ESCAPE, lang));
            let pause =
                IconButton::new(WidgetId(ID_ROW_BUTTONS + 2 * i + 1), Glyph::Pause, t(k::FILES_OPS_PAUSE, lang));
            self.cancel.push(cancel);
            self.pause.push(pause);
        }
    }

    pub(crate) fn paint(&self, cx: &mut PaintCx) {
        let c = cx.tokens().color.clone();
        for (i, r) in self.rows.iter().enumerate() {
            let role = if r.strong { TypeRole::BodyStrong } else { TypeRole::Body };
            let body = cx.px(role.spec().line);
            let tr = RectP {
                t: r.title_r.t + (r.title_r.h() - body) / 2,
                b: r.title_r.t + (r.title_r.h() + body) / 2,
                ..r.title_r
            };
            match &r.count {
                Some(n) => text_counted(cx, &r.title, n, role, tr, c.fg_primary, c.accent_danger),
                None => cx.text_in(&r.title, role, tr, Align::Start, Trim::End, c.fg_primary),
            };
            if let Some((t, rect)) = &r.sub {
                caption_in(cx, t, *rect, c.fg_secondary);
            }
            if let Some((v, rect)) = &r.rail {
                let radius = rect.h() / 2;
                cx.fill(*rect, radius, c.slider_rail);
                let w = (rect.w() as f32 * v.clamp(0.0, 1.0)).round() as i32;
                if w > 0 {
                    cx.fill(RectP { r: rect.l + w, ..*rect }, radius, c.fg_primary);
                }
            }
            if let Some((t, rect)) = &r.meta {
                caption_in(cx, t, *rect, c.fg_secondary);
            }
            for (t, rect) in &r.details {
                cx.text_in(t, TypeRole::Caption, *rect, Align::Start, Trim::Middle { tail: 10 }, c.fg_secondary);
            }
            if let Some((link, text, rect)) = &r.link {
                let id = link_id(i, *link);
                let radius = cx.px(cx.tokens().radius.s);
                if cx.pressed(id) {
                    cx.fill(*rect, radius, c.fill_press);
                } else if cx.hot(id) {
                    cx.fill(*rect, radius, c.fill_hover);
                }
                let strong = cx.px(TypeRole::BodyStrong.spec().line);
                let y = rect.t + (rect.h() - strong) / 2;
                let inner = RectP { t: y, b: y + strong, ..rect.inset(cx.px(LINK_PAD), 0) };
                cx.text_in(text, TypeRole::BodyStrong, inner, Align::Start, Trim::End, c.fg_primary);
                if cx.focused(id) {
                    cx.focus_ring(*rect, radius, false);
                }
            }
            if let Some(b) = r.buttons {
                if let Some(btn) = self.cancel.get(b) {
                    btn.paint(cx);
                }
                if PAUSE_OK {
                    if let Some(btn) = self.pause.get(b) {
                        btn.paint(cx);
                    }
                }
            }
            if let Some((y, right)) = r.sep {
                cx.hline(r.title_r.l, right, y, c.separator);
            }
        }
    }

    /// The buttons and links of the rows; None when the event is not theirs.
    pub(crate) fn event(&mut self, cx: &mut EventCx, ev: &Event) -> Option<RowsOut> {
        for r in &self.rows {
            let Some(b) = r.buttons else { continue };
            if let Some(btn) = self.cancel.get_mut(b) {
                if let Some(ButtonOut::Clicked) = btn.event(cx, ev) {
                    return Some(RowsOut::Cancel(r.op));
                }
            }
            if PAUSE_OK {
                if let Some(btn) = self.pause.get_mut(b) {
                    if let Some(ButtonOut::Clicked) = btn.event(cx, ev) {
                        let on = !r.paused;
                        self.paused.insert(r.op, on);
                        return Some(RowsOut::Pause(r.op, on));
                    }
                }
            }
        }
        let target = cx.target()?;
        match ev {
            Event::Click { button: MouseButton::Left, .. }
            | Event::Key { key: Key::ENTER | Key::SPACE, down: true, .. } => {}
            _ => return None,
        }
        for (i, r) in self.rows.iter().enumerate() {
            let Some((link, _, _)) = &r.link else { continue };
            if target == link_id(i, *link) {
                if matches!(ev, Event::Key { .. }) {
                    cx.consume();
                }
                return Some(match link {
                    Link::Undo => RowsOut::Undo(r.op),
                    Link::Details => RowsOut::Details(r.op),
                    Link::Show => RowsOut::Show(r.op),
                });
            }
        }
        None
    }
}

/// What a click in the rows does, in whichever window shows them (Undo is the window's: Ctrl+Z).
pub(crate) fn rows_out(shared: &Shared, out: RowsOut) {
    let ops = Rc::clone(&shared.services.ops);
    match out {
        RowsOut::Cancel(op) => ops.cancel(op),
        RowsOut::Pause(op, on) => ops.pause(op, on),
        RowsOut::Details(op) => {
            if let Some(r) = shared.ops.borrow_mut().row_mut(op) {
                r.opened = true;
            }
        }
        RowsOut::Show(op) => {
            if let Some(pid) = ops.host_pid() {
                // SAFETY: a plain call; it only lets that process take the foreground once.
                let _ = unsafe { AllowSetForegroundWindow(pid) };
            }
            ops.bring_forward(op);
        }
        RowsOut::Undo(_) => {}
    }
}

// ---- the flyout --------------------------------------------------------------------------------------------------

/// Ctrl+J or a click on the chip: the panel opens above the chip (op: the row whose failures it opens). Nothing to
/// show: nothing opens.
pub fn flyout_open(s: &mut FilesScreen, cx: &mut EventCx, op: Option<OpId>) -> Reply {
    if let Some(op) = op {
        rows_out(&s.shared, RowsOut::Details(op));
    }
    if s.shared.ops.borrow().rows().is_empty() {
        return Reply::None;
    }
    s.flyout.open = true;
    cx.relayout();
    Reply::Relayout
}

fn close_flyout(s: &mut FilesScreen, cx: &mut EventCx) {
    s.flyout.open = false;
    // Opened failure rows may go once the panel is closed.
    s.shared.ops.borrow_mut().prune(cx.now_ms());
    let ours = |w: WidgetId| w == ID_FLY || (ID_ROW_BUTTONS..ID_ROW_BUTTONS + 2 * ROW_BUTTONS as u32).contains(&w.0);
    if cx.focused().is_some_and(|f| ours(f.owner())) {
        cx.set_focus(NodeId::new(crate::tab::TABLE_ID, 0));
    }
    cx.relayout();
}

/// The panel: 360 wide, R_L, RIM_SURFACE, pad 12, above the chip (or the status line's centre), inside the client.
pub fn flyout_layout(s: &mut FilesScreen, cx: &mut LayoutCx, r: RectP) {
    if !s.flyout.open {
        s.flyout.panel = RectP::EMPTY;
        return;
    }
    let fmt = s.shared.fmt.borrow().clone();
    let undo_top = s.shared.undo.borrow().top_op();
    let status = s.flyout.status;
    let anchor = if s.flyout.chip.text.is_some() { s.flyout.chip.rect } else { status };
    let w = cx.px(oled_ui::tokens::POPUP_W_L).min(r.w());
    let pad = cx.px(FLY_PAD);
    let content = cx.px(Rows::height_of(&s.shared.ops.borrow()));
    let bottom = status.t - cx.px(FLY_GAP);
    let h = (content + pad * 2).min(bottom - r.t).max(0);
    let mid = anchor.l + anchor.w() / 2;
    let l = (mid - w / 2).clamp(r.l, (r.r - w).max(r.l));
    let panel = RectP::wh(l, bottom - h, w, h);
    s.flyout.panel = panel;
    cx.push(Node::new(NodeId::new(ID_FLY, 0), Role::Group, panel).label(t(k::FILES_CMD_OPS, cx.lang())));
    let m = s.shared.ops.borrow();
    s.flyout.rows.layout(cx, &m, undo_top, &fmt, panel.inset(pad, pad));
}

pub fn flyout_paint(s: &FilesScreen, cx: &mut PaintCx) {
    if !s.flyout.open {
        return;
    }
    let p = s.flyout.panel;
    let c = cx.tokens().color.clone();
    let radius = cx.px(cx.tokens().radius.l);
    cx.fill(p, radius, c.bg);
    cx.stroke(p, radius, 1, c.rim_surface);
    let inner = p.inset(cx.px(FLY_PAD), cx.px(FLY_PAD));
    cx.with_clip(inner, &mut |cx| s.flyout.rows.paint(cx));
}

/// While open the flyout takes events first: Esc or a press outside closes it, its buttons and links act, other keys
/// stop here (Tab and Ctrl+J go on to the window). While closed only the chip's clicks are taken.
pub fn flyout_event(s: &mut FilesScreen, cx: &mut EventCx, ev: &Event) -> Option<Reply> {
    if !s.flyout.open {
        return chip_event(s, cx, ev);
    }
    match ev {
        Event::Key { key: Key::ESC, down: true, .. } => {
            cx.consume();
            close_flyout(s, cx);
            return Some(Reply::Relayout);
        }
        Event::PointerDown { pos, .. } if !s.flyout.panel.contains(*pos) && !s.flyout.chip.rect.contains(*pos) => {
            close_flyout(s, cx);
            return Some(Reply::Relayout);
        }
        _ => {}
    }
    if let Some(out) = s.flyout.rows.event(cx, ev) {
        if let RowsOut::Undo(_) = out {
            return Some(undo::undo(s, cx));
        }
        rows_out(&s.shared, out);
        cx.relayout();
        return Some(Reply::Relayout);
    }
    if let Some(r) = chip_event(s, cx, ev) {
        return Some(r);
    }
    match ev {
        Event::Key { key: Key::TAB, .. } => None,
        Event::Key { key, mods, down, .. } => {
            if *key == Key(u16::from(b'J')) && mods.ctrl() {
                return None;
            }
            if *down {
                cx.consume();
            }
            Some(Reply::None)
        }
        Event::Text(_) => Some(Reply::None),
        Event::Command(id) if keys::cmd_of(*id) == Some(Cmd::OpsFlyout) => None,
        _ => None,
    }
}
