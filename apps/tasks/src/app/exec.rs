//! The executor (T3.7): Close, End, End now, End process tree and Reopen run on an act thread, every key re-checked
//! immediately before it is touched. The types, the Executor trait and the RecordingExecutor are TF0's; the Win32
//! executor is filled by TA-T3c.
#![allow(dead_code, unused_variables)] // stub: removed by TA-T3c

use std::sync::{Arc, Mutex};

use oled_sys::admin::policy::{HelperFacts, ProcEdge};
use oled_sysmon::ProcKey;
use oled_ui::host::Proxy;
use oled_ui::model::RowKey;

use super::access::Refusal;
use super::launch::Launcher;

/// What a batch does.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ActVerb {
    Close,
    End,
    EndNow,
    EndTree,
    Reopen,
}

/// One row's part of a batch: its processes, its windows (Close) and its launch string (Reopen).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Target {
    pub row: RowKey,
    pub members: Vec<ProcKey>,
    pub windows: Vec<isize>,
    pub launch: Option<String>,
}

/// The keys no batch may terminate, built on the UI thread from the latest frame when the batch is made.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct KillGuard {
    pub helper: HelperFacts,
    /// Our parent, its parent and so on, at most 64.
    pub own_chain: Vec<ProcKey>,
    pub shell: Option<ProcKey>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Batch {
    pub verb: ActVerb,
    pub targets: Vec<Target>,
    pub guard: KillGuard,
    /// The process edges of the latest frame when the batch is made.
    pub edges: Vec<ProcEdge>,
}

/// A target's result: the worst of its members' (Failed > NeedsAdmin > Done > Gone); Refused when every key was.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Outcome {
    Posted,
    Done,
    Gone,
    NeedsAdmin,
    Refused(Refusal),
    Failed(u32),
}

/// AppMsg: one per target; refused counts the keys skipped by the guard.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ActionDone {
    pub row: RowKey,
    pub outcome: Outcome,
    pub refused: u16,
}

/// The UI's one way to act on processes.
pub trait Executor: Send + Sync {
    fn submit(&self, b: Batch);
}

/// Keeps every batch and runs nothing; every Harness test and scene.
#[derive(Default)]
pub struct RecordingExecutor {
    pub batches: Mutex<Vec<Batch>>,
}

impl Executor for RecordingExecutor {
    fn submit(&self, b: Batch) {
        self.batches.lock().unwrap_or_else(|p| p.into_inner()).push(b);
    }
}

/// The real executor: each batch on a new "tasks-act" thread, its results through the attached proxy. T3.7.
pub struct Win32Executor {
    launcher: Arc<dyn Launcher>,
    proxy: Mutex<Option<Proxy>>,
}

impl Win32Executor {
    pub fn new(l: Arc<dyn Launcher>) -> Arc<Self> {
        Arc::new(Win32Executor { launcher: l, proxy: Mutex::new(None) })
    }

    pub fn attach(&self, p: Proxy) {
        *self.proxy.lock().unwrap_or_else(|e| e.into_inner()) = Some(p);
    }
}

impl Executor for Win32Executor {
    /// T3.7. Stub: every target comes back as Failed(50) (ERROR_NOT_SUPPORTED); nothing is touched.
    fn submit(&self, b: Batch) {
        let proxy = self.proxy.lock().unwrap_or_else(|e| e.into_inner()).clone();
        if let Some(p) = proxy {
            for t in b.targets {
                let _ = p.send(Box::new(ActionDone { row: t.row, outcome: Outcome::Failed(50), refused: 0 }));
            }
        }
    }
}
