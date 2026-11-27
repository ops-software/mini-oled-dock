//! Undo and redo (P3-files-app 2.2, F3.3): one stack per process in Shared, shared by every window. An entry is pushed
//! when the helper sends the undo record of an operation this process asked for; Ctrl+Z sends it back through the
//! gate (or hands the Recycle Bin to File Explorer); a successful undo goes on the redo stack, and a new operation
//! clears it.

use std::collections::VecDeque;

use oled_trust::frame::files::{OpId, OpKind, UndoKind};
use oled_ui::node::Text;
use oled_ui::{EventCx, Reply, UiLang};

use crate::handoff::{self, Target};
use crate::ops_client::UndoRecord;
use crate::ops_ui::{self, name_of, MutReq};
use crate::status::{self, Notice};
use crate::tr::{k, t, tf};
use crate::window::FilesScreen;

/// At most this many undo entries; the oldest goes first.
pub const UNDO_CAP: usize = 50;

/// One operation that can be undone (or, on the redo stack, done again).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct UndoEntry {
    pub op: OpId,
    /// The operation's kind (Rename, NewFolder, Recycle): the label's verb.
    pub kind: OpKind,
    pub record: UndoRecord,
}

#[derive(Debug, Default)]
pub struct UndoStack {
    undo: VecDeque<UndoEntry>,
    redo: Vec<UndoEntry>,
}

impl UndoStack {
    pub fn new() -> UndoStack {
        UndoStack::default()
    }

    /// "Undo Rename", "Undo New folder", "Undo Delete" for the newest entry.
    pub fn label(&self, lang: UiLang) -> Option<Text> {
        let e = self.undo.back()?;
        Some(tf(k::FILES_MORE_UNDO, lang, &[("op", &verb(e.kind, lang))]))
    }

    /// "Redo" while something can be done again.
    pub fn redo_label(&self, lang: UiLang) -> Option<Text> {
        (!self.redo.is_empty()).then(|| t(k::FILES_MORE_REDO, lang))
    }

    /// A new entry; past the cap the oldest goes.
    pub fn push(&mut self, e: UndoEntry) {
        self.undo.retain(|x| x.op != e.op);
        self.undo.push_back(e);
        while self.undo.len() > UNDO_CAP {
            self.undo.pop_front();
        }
    }

    pub fn pop(&mut self) -> Option<UndoEntry> {
        self.undo.pop_back()
    }

    /// The operation the newest entry undoes.
    pub fn top_op(&self) -> Option<OpId> {
        self.undo.back().map(|e| e.op)
    }

    pub fn len(&self) -> usize {
        self.undo.len()
    }

    pub fn is_empty(&self) -> bool {
        self.undo.is_empty()
    }

    pub fn redo_len(&self) -> usize {
        self.redo.len()
    }

    pub fn push_redo(&mut self, e: UndoEntry) {
        self.redo.push(e);
        if self.redo.len() > UNDO_CAP {
            self.redo.remove(0);
        }
    }

    pub fn pop_redo(&mut self) -> Option<UndoEntry> {
        self.redo.pop()
    }

    pub fn clear_redo(&mut self) {
        self.redo.clear();
    }
}

/// The label's verb: Rename, New folder, Delete (the kit's verb).
pub fn verb(kind: OpKind, lang: UiLang) -> Text {
    match kind {
        OpKind::Rename => t(k::FILES_MENU_RENAME, lang),
        OpKind::NewFolder => t(k::FILES_MENU_NEW_FOLDER, lang),
        _ => lang.kit(oled_ui::strings::k::KIT_VERB_DELETE),
    }
}

/// Ctrl+Z: the newest entry. OpenRecycleBin hands the bin to File Explorer (the entry is consumed); any other goes to
/// the helper through the gate (a refusal comes back as its own text); a request that could not be made keeps the
/// entry. Nothing to undo: the notice says so.
pub fn undo(s: &mut FilesScreen, cx: &mut EventCx) -> Reply {
    let entry = s.shared.undo.borrow_mut().pop();
    let Some(e) = entry else {
        status::notice(s, cx, Notice::new(t(k::FILES_OPS_NOTHING_TO_UNDO, cx.lang())));
        return Reply::Relayout;
    };
    if e.record.kind == UndoKind::OpenRecycleBin {
        return handoff::hand_off(s, cx, Target::RecycleBin, false);
    }
    if ops_ui::request(s, cx, MutReq::Undo(e.record.clone())).is_none() {
        s.shared.undo.borrow_mut().push(e);
    }
    Reply::Redraw
}

/// Ctrl+Y and Ctrl+Shift+Z: the forward operation again. RenameBack: the rename; RecycleNewFolder: the new folder;
/// RestoreRecycled: the Del flow over the restored items, classified like every recycle (U3), so any No opens the bar
/// and nothing goes permanently without its Enter. Copies and moves are not in this beta. Nothing to redo: nothing.
pub fn redo(s: &mut FilesScreen, cx: &mut EventCx) -> Reply {
    let entry = s.shared.undo.borrow_mut().pop_redo();
    let Some(e) = entry else { return Reply::None };
    let first = e.record.entries.first();
    let req = match e.record.kind {
        UndoKind::RenameBack => first.map(|x| MutReq::Rename { item: x.from.clone(), to: name_of(&x.to) }),
        UndoKind::RecycleNewFolder => {
            first.and_then(|x| Some(MutReq::NewFolder { parent: x.to.parent()?, name: name_of(&x.to) }))
        }
        UndoKind::RestoreRecycled => {
            let items = e.record.entries.iter().map(|x| x.from.clone()).collect();
            s.shared.ops.borrow_mut().mark_redo_next();
            return ops_ui::delete_flow(s, cx, items);
        }
        UndoKind::MoveBack | UndoKind::RecycleCopies | UndoKind::OpenRecycleBin => None,
    };
    let Some(r) = req else { return Reply::None };
    s.shared.ops.borrow_mut().mark_redo_next();
    if ops_ui::request(s, cx, r).is_none() {
        s.shared.ops.borrow_mut().take_redo_next();
        s.shared.undo.borrow_mut().push_redo(e);
    }
    Reply::Redraw
}
