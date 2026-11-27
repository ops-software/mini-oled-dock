//! Helpers shared by the ops tests (ops_undo, ops_safety, ops_client, ops_kill): paths, a runner context fenced to
//! the test's roots, and the events of a run.
#![allow(dead_code)]

use std::path::Path;
use std::sync::mpsc::{channel, Receiver};

use files_engine::ops::journal::StagedItems;
use files_engine::ops::runner::{run, OpJob, Prepared, RunCtx, RunEnd, Work};
use files_engine::ops_client::UndoRecord;
use oled_trust::frame::files::{
    Change, ChangeKind, Conflicts, OpEvt, OpId, OpKind, OpResult, UndoEntry, UndoKind, WidePath,
};

use super::common::temp::TempRoot;

pub fn wp(p: &Path) -> WidePath {
    WidePath::parse(&p.display().to_string()).unwrap()
}

/// A runner context fenced to `roots`, with no system UI.
pub fn ctx(roots: &[&TempRoot]) -> (RunCtx, Receiver<OpEvt>) {
    let (tx, rx) = channel();
    let mut c = RunCtx::new(tx);
    c.roots = Some(roots.iter().map(|r| wp(r.path())).collect());
    c.test_no_ui = true;
    (c, rx)
}

pub fn job(kind: OpKind, items: Vec<WidePath>, dest: Option<WidePath>, conflicts: Conflicts) -> Work {
    Work::Staged(OpJob { kind, dest, name: None, conflicts, staged: StagedItems::from_vec(items) })
}

pub fn rename_job(item: WidePath, to: &str) -> Work {
    Work::Staged(OpJob {
        kind: OpKind::Rename,
        dest: None,
        name: Some(to.into()),
        conflicts: Conflicts::AskSystem,
        staged: StagedItems::from_vec(vec![item]),
    })
}

/// Runs `work` in a context fenced to `roots`: its end and every event it sent.
pub fn run_in(op: u32, work: Work, roots: &[&TempRoot]) -> (RunEnd, Vec<OpEvt>) {
    let (c, rx) = ctx(roots);
    let end = run(OpId(op), work, &c);
    (end, rx.try_iter().collect())
}

pub fn changes(evs: &[OpEvt]) -> Vec<Change> {
    evs.iter()
        .filter_map(|e| match e {
            OpEvt::ItemsDone { changes, .. } => Some(changes.clone()),
            _ => None,
        })
        .flatten()
        .collect()
}

pub fn changed(evs: &[OpEvt], kind: ChangeKind) -> usize {
    changes(evs).iter().filter(|c| c.kind == kind).count()
}

pub fn undo_entries(evs: &[OpEvt]) -> Vec<UndoEntry> {
    evs.iter()
        .filter_map(|e| match e {
            OpEvt::UndoPart { entries, .. } => Some(entries.clone()),
            _ => None,
        })
        .flatten()
        .collect()
}

/// The undo record the UI would assemble from a run's events.
pub fn record(op: u32, evs: &[OpEvt]) -> UndoRecord {
    match evs.last() {
        Some(OpEvt::Finished { undo: Some(kind), replaced, .. }) => {
            UndoRecord { of: OpId(op), kind: *kind, replaced: *replaced, entries: undo_entries(evs) }
        }
        other => panic!("no undo offered: {other:?}"),
    }
}

pub fn result(evs: &[OpEvt]) -> OpResult {
    match evs.last() {
        Some(OpEvt::Finished { result, .. }) => *result,
        other => panic!("the last event is not Finished: {other:?}"),
    }
}

pub fn finished_undo(evs: &[OpEvt]) -> Option<UndoKind> {
    match evs.last() {
        Some(OpEvt::Finished { undo, .. }) => *undo,
        other => panic!("the last event is not Finished: {other:?}"),
    }
}

/// Runs an undo plan as the host would (Work::Planned of kind Undo).
pub fn run_plan(op: u32, plan: Vec<Prepared>, roots: &[&TempRoot]) -> (RunEnd, Vec<OpEvt>) {
    run_in(op, Work::Planned(OpKind::Undo, plan), roots)
}
