//! Preview scenes of FC-OPS (F3.2, F3.4): the chip while a delete runs, the chip waiting for a Windows dialog, the
//! flyout with a running, a finished and a failed operation, and the recycle bar. Made-up names; system caption.

use std::sync::Arc;

use oled_trust::frame::files::{OpEvt, OpId, OpKind, UndoKind};
use oled_ui::harness::Harness;
use oled_ui::host::Caption;
use oled_ui::SizeL;

use super::fixtures::{self, settle, ROOT};
use super::{Cells, Scene};
use crate::fakes::Fakes;
use crate::format::Fmt;
use crate::listing::WidePath;
use crate::ops::classify::{NoBin, Recyclability};
use crate::ops_client::{OpsEvent, UndoRecord};
use crate::ops_ui::{FactsFn, OpRow, OpState};
use crate::undo::UndoEntry;
use crate::window::FilesScreen;

const WINDOW_SIZE: SizeL = SizeL { w: 800, h: 500 };
/// The progress line's tween (DUR_PROGRESS) is over.
const SETTLED_MS: u32 = 300;

fn documents(fakes: &Fakes, fmt: Fmt) -> FilesScreen {
    fakes.enumr.queue(&fixtures::wp(ROOT), fixtures::documents());
    fixtures::open(fakes, fmt, ROOT)
}

fn delete_running(fakes: &Fakes) {
    fakes.ops.emit(OpsEvent::Evt(OpEvt::Started {
        op: OpId(7),
        kind: OpKind::DeletePermanent,
        items: 2_000,
        bytes: 0,
    }));
    fakes.ops.emit(OpsEvent::Evt(OpEvt::Progress {
        op: OpId(7),
        done: 1_240,
        total: 2_000,
        bps: 0,
        current: "".into(),
    }));
}

fn running(h: &mut Harness<FilesScreen>, fakes: &Fakes) {
    settle(h, fakes);
    delete_running(fakes);
    fakes.pump(h, 1);
    h.advance(SETTLED_MS);
    h.render();
}

fn needs_input(h: &mut Harness<FilesScreen>, fakes: &Fakes) {
    settle(h, fakes);
    delete_running(fakes);
    fakes.ops.emit(OpsEvent::Evt(OpEvt::NeedsInput { op: OpId(7), on: true }));
    fakes.pump(h, 1);
    h.advance(SETTLED_MS);
    h.render();
}

/// A delete running in the folder, a recycle done (the stack's top: Undo), a delete with two failures.
fn three_ops(fakes: &Fakes, fmt: Fmt) -> FilesScreen {
    let s = documents(fakes, fmt);
    let shared = s.shared();
    let folder = fixtures::wp(ROOT);
    let item = |name: &str| folder.join(&name.encode_utf16().collect::<Vec<u16>>()).ok();
    let mut a = OpRow::new(OpId(11), OpKind::DeletePermanent, 1);
    (a.count, a.done, a.total, a.state, a.folder) = (2_000, 1_240, 2_000, OpState::Running, Some(folder.clone()));
    let mut b = OpRow::new(OpId(12), OpKind::Recycle, 1);
    (b.count, b.state, b.undoable, b.folder) = (3, OpState::Done, true, Some(folder.clone()));
    let mut c = OpRow::new(OpId(13), OpKind::DeletePermanent, 1);
    (c.count, c.state, c.failed, c.folder) = (4, OpState::Failed, 2, Some(folder.clone()));
    c.failures =
        ["report-final-v2.pdf", "notes.txt"].iter().filter_map(|n| item(n)).map(|p| (p, String::new())).collect();
    let mut m = shared.ops.borrow_mut();
    m.insert(a);
    m.insert(b);
    m.insert(c);
    drop(m);
    let record = UndoRecord { of: OpId(12), kind: UndoKind::OpenRecycleBin, replaced: 0, entries: Vec::new() };
    shared.undo.borrow_mut().push(UndoEntry { op: OpId(12), kind: OpKind::Recycle, record });
    s
}

fn flyout(h: &mut Harness<FilesScreen>, fakes: &Fakes) {
    settle(h, fakes);
    fakes.ops.emit(OpsEvent::Evt(OpEvt::Progress {
        op: OpId(11),
        done: 1_240,
        total: 2_000,
        bps: 0,
        current: "".into(),
    }));
    fakes.pump(h, 1);
    h.key("Ctrl+J");
    h.advance(SETTLED_MS);
    h.render();
}

/// The folder with a classifier that sends the PDFs to no Recycle Bin (a network folder in the story).
fn with_bar(fakes: &Fakes, fmt: Fmt) -> FilesScreen {
    let s = documents(fakes, fmt);
    let facts: FactsFn = Arc::new(|items: &[(WidePath, u64)]| {
        items
            .iter()
            .map(|(p, _)| {
                let pdf = p.to_string_lossy().ends_with(".pdf");
                (p.clone(), if pdf { Recyclability::No(NoBin::Network) } else { Recyclability::Yes })
            })
            .collect()
    });
    s.shared().ops.borrow_mut().set_facts(facts);
    s
}

fn bar(h: &mut Harness<FilesScreen>, fakes: &Fakes) {
    settle(h, fakes);
    // The last three rows: two PDFs and the installer.
    h.key("End").key("Shift+Up").key("Shift+Up");
    h.key("Del");
    fakes.pump(h, 1);
    h.render();
}

fn scene(
    name: &'static str,
    cells: Cells,
    build: fn(&Fakes, Fmt) -> FilesScreen,
    script: fn(&mut Harness<FilesScreen>, &Fakes),
) -> Scene {
    Scene { name, size: WINDOW_SIZE, cells, caption: Caption::System, build, script }
}

/// This file's scenes.
pub fn scenes() -> Vec<Scene> {
    vec![
        scene("ops_chip_running", Cells::Full12, documents, running),
        scene("ops_chip_needs_input", Cells::Full12, documents, needs_input),
        scene("ops_flyout", Cells::Full12, three_ops, flyout),
        scene("ops_bar", Cells::Four, with_bar, bar),
    ]
}
