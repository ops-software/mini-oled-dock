//! Changes to user files as the window asks for them (P3-files-app 2.2, F3.1). request() is the only caller of
//! OpsService's mutating methods: every change passes the real-data gate first. FC-OPS fills the rest (the operations
//! model, the chip and flyout, Del with classification, undo).

use oled_trust::frame::files::{ItemRef, OpEvt, OpId, Refusal};
use oled_ui::node::Text;
use oled_ui::{Event, EventCx, LayoutCx, MenuItem, PaintCx, RectP, Reply, UiLang};

use crate::gate;
use crate::keys::Cmd;
use crate::listing::WidePath;
use crate::msg::Fwd;
use crate::ops::classify::Recyclability;
use crate::ops_client::{OpsError, OpsEvent, UndoRecord};
use crate::status::{self, Notice, NoticeAct};
use crate::tr::{k, t};
use crate::window::{FilesScreen, Shared};

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

/// The op enters OpsModel with its kind, items, folder and window. Filled by FC-OPS; until then nothing.
pub fn on_accepted(s: &mut FilesScreen, cx: &mut EventCx, op: OpId, r: &MutReq) {
    let _ = (s, cx, op, r);
}

/// Filled by FC-OPS; until then the host_lost text.
pub fn refusal_text(r: &Refusal, lang: UiLang) -> Text {
    let _ = r;
    t(k::FILES_OPS_HOST_LOST, lang)
}

/// Del, Ctrl+D, the menu's Delete, an intercepted Delete (U3, U4). Filled by FC-OPS; until then nothing is sent.
pub fn delete_flow(s: &mut FilesScreen, cx: &mut EventCx, items: Vec<WidePath>) -> Reply {
    let _ = (s, cx, items);
    Reply::None
}

/// Rename, Delete, Delete permanently for the context menu. Filled by FC-OPS; until then none.
pub fn f3_items(s: &FilesScreen, sel: &[WidePath]) -> Vec<MenuItem> {
    let _ = (s, sel);
    Vec::new()
}

/// Every ops event: ItemsDone goes to every main window as Fwd::Own (each applies what its tabs show, F3.10). FC-OPS
/// keeps this routing and adds the operations model, Fwd::Ops and the notices.
pub fn drain(shared: &Shared) -> Vec<(u32, Fwd)> {
    let mut out = Vec::new();
    for e in shared.services.ops.drain() {
        if let OpsEvent::Evt(OpEvt::ItemsDone { op, changes }) = e {
            out.extend(shared.main_windows().into_iter().map(|w| (w, Fwd::Own { op, changes: changes.clone() })));
        }
    }
    out
}

/// Fwd::Ops. Filled by FC-OPS; until then nothing.
pub fn on_fwd_ops(s: &mut FilesScreen, cx: &mut EventCx) {
    let _ = (s, cx);
}

/// Filled by FC-OPS; until then false (not ours).
pub fn on_facts(s: &mut FilesScreen, cx: &mut EventCx, req: u64, items: Vec<(WidePath, Recyclability)>) -> bool {
    let _ = (s, cx, req, items);
    false
}

/// Filled by FC-OPS; until then nothing.
pub fn on_rest(s: &mut FilesScreen, cx: &mut EventCx, of: OpId) -> Reply {
    let _ = (s, cx, of);
    Reply::None
}

/// Filled by FC-OPS; until then nothing.
pub fn flyout_open(s: &mut FilesScreen, cx: &mut EventCx, op: Option<OpId>) -> Reply {
    let _ = (s, cx, op);
    Reply::None
}

/// The rest of the status rect beside the chip. Filled by FC-OPS; until then `r`.
pub fn chip_layout(s: &mut FilesScreen, cx: &mut LayoutCx, r: RectP) -> RectP {
    let _ = (s, cx);
    r
}

/// Filled by FC-OPS; until then nothing is drawn.
pub fn chip_paint(s: &FilesScreen, cx: &mut PaintCx) {
    let _ = (s, cx);
}

/// Filled by FC-OPS; until then nothing.
pub fn flyout_layout(s: &mut FilesScreen, cx: &mut LayoutCx, r: RectP) {
    let _ = (s, cx, r);
}

/// Filled by FC-OPS; until then nothing is drawn.
pub fn flyout_paint(s: &FilesScreen, cx: &mut PaintCx) {
    let _ = (s, cx);
}

/// Takes every event while open. Filled by FC-OPS; until then not taken.
pub fn flyout_event(s: &mut FilesScreen, cx: &mut EventCx, ev: &Event) -> Option<Reply> {
    let _ = (s, cx, ev);
    None
}

/// Timer kind 6. Filled by FC-OPS; until then nothing.
pub fn on_chip_timer(s: &mut FilesScreen, cx: &mut EventCx) {
    let _ = (s, cx);
}

/// Recycle, DeletePermanent, Undo, Redo, OpsFlyout. Filled by FC-OPS; until then nothing.
pub fn cmd(s: &mut FilesScreen, cx: &mut EventCx, c: Cmd) -> Reply {
    let _ = (s, cx, c);
    Reply::None
}

/// The operations this process started. Filled by FC-OPS.
#[derive(Debug, Default)]
pub struct OpsModel;

impl OpsModel {
    pub fn new() -> OpsModel {
        OpsModel
    }

    /// Filled by FC-OPS; until then 0.
    pub fn running(&self) -> usize {
        0
    }

    /// The items an operation was asked to change. Filled by FC-OPS; until then none.
    pub fn items_of(&self, op: OpId) -> Vec<WidePath> {
        let _ = op;
        Vec::new()
    }

    /// Filled by FC-OPS; until then none.
    pub fn take_notices(&mut self, win: u32) -> Vec<Notice> {
        let _ = win;
        Vec::new()
    }
}

/// The operations flyout. Filled by FC-OPS.
#[derive(Debug, Default)]
pub struct Flyout;

impl Flyout {
    pub fn new() -> Flyout {
        Flyout
    }
}
