//! The advised IFileOperationProgressSink of every row. It records each callback (kind, the item's own name, the
//! HRESULT, the time) and acts on the n-th PreCopyItem as the row asks: cancel with
//! HRESULT_FROM_WIN32(ERROR_CANCELLED) (row e1), or block on an event for a while and then go on (rows e2, e2w).
//! Names are the parent-relative parsing names of the probe's own synthetic files; of a deleted item it also keeps
//! the newly created item (the Recycle Bin's copy: row g restores it, row c reports it).

use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::time::Instant;

use windows::core::{implement, Ref, Result, HRESULT, PCWSTR};
use windows::Win32::Foundation::{CloseHandle, ERROR_CANCELLED};
use windows::Win32::System::Com::CoTaskMemFree;
use windows::Win32::System::Threading::{CreateEventW, WaitForSingleObject};
use windows::Win32::UI::Shell::{
    IFileOperationProgressSink, IFileOperationProgressSink_Impl, IShellItem, SIGDN, SIGDN_DESKTOPABSOLUTEPARSING,
    SIGDN_PARENTRELATIVEPARSING,
};

use crate::json::J;
use crate::util::{hr_hex, ms};

/// What the sink does on the n-th (1-based) PreCopyItem.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Act {
    Nothing,
    CancelAt(u32),
    /// Blocks this many milliseconds on an event nobody sets, then returns S_OK.
    BlockAt(u32, u32),
}

/// One callback.
#[derive(Clone, Debug)]
pub struct Ev {
    pub t_ms: u64,
    pub kind: &'static str,
    pub name: String,
    pub hr: Option<HRESULT>,
}

/// What the sink saw; shared with the row (the sink runs on the row's thread, inside PerformOperations).
pub struct State {
    pub t0: Instant,
    act: Act,
    pub events: RefCell<Vec<Ev>>,
    pub pre_copy: Cell<u32>,
    pub progress_calls: Cell<u32>,
    /// When the cancel was returned, or when the block began.
    pub acted_ms: Cell<Option<u64>>,
    pub block_end_ms: Cell<Option<u64>>,
    /// The newly created items of deletes: (desktop-absolute parsing name, the item).
    pub deleted_new: RefCell<Vec<(String, IShellItem)>>,
}

impl State {
    /// Callbacks of one kind.
    pub fn count(&self, kind: &str) -> usize {
        self.events.borrow().iter().filter(|e| e.kind == kind).count()
    }

    /// The HRESULTs of the callbacks of one kind, with how often each came.
    pub fn hr_counts(&self, kind: &str) -> J {
        let mut v: Vec<(String, u32)> = Vec::new();
        for e in self.events.borrow().iter().filter(|e| e.kind == kind) {
            let k = e.hr.map(hr_hex).unwrap_or_default();
            match v.iter_mut().find(|(h, _)| *h == k) {
                Some(slot) => slot.1 += 1,
                None => v.push((k, 1)),
            }
        }
        v.into_iter().fold(J::obj(), |j, (k, n)| j.set(&k, n))
    }

    /// Every callback in order (kind, name, HRESULT, time), without UpdateProgress.
    pub fn events_json(&self) -> J {
        J::Arr(
            self.events
                .borrow()
                .iter()
                .map(|e| {
                    J::obj()
                        .set("t_ms", e.t_ms)
                        .set("kind", e.kind)
                        .set("name", e.name.as_str())
                        .set("hr", e.hr.map(hr_hex))
                })
                .collect(),
        )
    }
}

#[implement(IFileOperationProgressSink)]
pub struct Sink {
    st: Rc<State>,
}

/// A new sink and the state the row reads after PerformOperations.
pub fn new(act: Act, t0: Instant) -> (IFileOperationProgressSink, Rc<State>) {
    let st = Rc::new(State {
        t0,
        act,
        events: RefCell::new(Vec::new()),
        pre_copy: Cell::new(0),
        progress_calls: Cell::new(0),
        acted_ms: Cell::new(None),
        block_end_ms: Cell::new(None),
        deleted_new: RefCell::new(Vec::new()),
    });
    (Sink { st: st.clone() }.into(), st)
}

/// A display name of an item (CoTaskMem string, freed here).
pub fn display(item: &IShellItem, sigdn: SIGDN) -> Option<String> {
    // SAFETY: GetDisplayName returns a CoTaskMem string that is read once and freed once.
    unsafe {
        let p = item.GetDisplayName(sigdn).ok()?;
        let s = p.to_string().ok();
        CoTaskMemFree(Some(p.0 as *const core::ffi::c_void));
        s
    }
}

fn leaf(item: &Ref<IShellItem>) -> String {
    item.as_ref().and_then(|i| display(i, SIGDN_PARENTRELATIVEPARSING)).unwrap_or_default()
}

fn block(ms_to_wait: u32) {
    // SAFETY: an unnamed manual-reset event nobody sets; waited on with a timeout, then closed.
    unsafe {
        if let Ok(ev) = CreateEventW(None, true, false, PCWSTR::null()) {
            let _ = WaitForSingleObject(ev, ms_to_wait);
            let _ = CloseHandle(ev);
        }
    }
}

impl Sink_Impl {
    fn ev(&self, kind: &'static str, name: String, hr: Option<HRESULT>) {
        self.st.events.borrow_mut().push(Ev { t_ms: ms(self.st.t0), kind, name, hr });
    }
}

impl IFileOperationProgressSink_Impl for Sink_Impl {
    fn StartOperations(&self) -> Result<()> {
        self.ev("start", String::new(), None);
        Ok(())
    }
    fn FinishOperations(&self, hrresult: HRESULT) -> Result<()> {
        self.ev("finish", String::new(), Some(hrresult));
        Ok(())
    }
    fn PreRenameItem(&self, _flags: u32, item: Ref<IShellItem>, _new: &PCWSTR) -> Result<()> {
        self.ev("pre_rename", leaf(&item), None);
        Ok(())
    }
    fn PostRenameItem(
        &self,
        _f: u32,
        item: Ref<IShellItem>,
        _n: &PCWSTR,
        hr: HRESULT,
        _c: Ref<IShellItem>,
    ) -> Result<()> {
        self.ev("post_rename", leaf(&item), Some(hr));
        Ok(())
    }
    fn PreMoveItem(&self, _f: u32, item: Ref<IShellItem>, _d: Ref<IShellItem>, _n: &PCWSTR) -> Result<()> {
        self.ev("pre_move", leaf(&item), None);
        Ok(())
    }
    fn PostMoveItem(
        &self,
        _f: u32,
        item: Ref<IShellItem>,
        _d: Ref<IShellItem>,
        _n: &PCWSTR,
        hr: HRESULT,
        _c: Ref<IShellItem>,
    ) -> Result<()> {
        self.ev("post_move", leaf(&item), Some(hr));
        Ok(())
    }
    fn PreCopyItem(&self, _f: u32, item: Ref<IShellItem>, _d: Ref<IShellItem>, _n: &PCWSTR) -> Result<()> {
        let n = self.st.pre_copy.get() + 1;
        self.st.pre_copy.set(n);
        self.ev("pre_copy", leaf(&item), None);
        match self.st.act {
            Act::CancelAt(at) if at == n => {
                self.st.acted_ms.set(Some(ms(self.st.t0)));
                return Err(ERROR_CANCELLED.to_hresult().into());
            }
            Act::BlockAt(at, wait) if at == n => {
                self.st.acted_ms.set(Some(ms(self.st.t0)));
                block(wait);
                self.st.block_end_ms.set(Some(ms(self.st.t0)));
            }
            _ => {}
        }
        Ok(())
    }
    fn PostCopyItem(
        &self,
        _f: u32,
        item: Ref<IShellItem>,
        _d: Ref<IShellItem>,
        _n: &PCWSTR,
        hr: HRESULT,
        _c: Ref<IShellItem>,
    ) -> Result<()> {
        self.ev("post_copy", leaf(&item), Some(hr));
        Ok(())
    }
    fn PreDeleteItem(&self, _f: u32, item: Ref<IShellItem>) -> Result<()> {
        self.ev("pre_delete", leaf(&item), None);
        Ok(())
    }
    fn PostDeleteItem(&self, _f: u32, item: Ref<IShellItem>, hr: HRESULT, created: Ref<IShellItem>) -> Result<()> {
        self.ev("post_delete", leaf(&item), Some(hr));
        if let Some(new) = created.as_ref() {
            let name = display(new, SIGDN_DESKTOPABSOLUTEPARSING).unwrap_or_default();
            self.st.deleted_new.borrow_mut().push((name, new.clone()));
        }
        Ok(())
    }
    fn PreNewItem(&self, _f: u32, _d: Ref<IShellItem>, _n: &PCWSTR) -> Result<()> {
        self.ev("pre_new", String::new(), None);
        Ok(())
    }
    fn PostNewItem(
        &self,
        _f: u32,
        _d: Ref<IShellItem>,
        _n: &PCWSTR,
        _t: &PCWSTR,
        _a: u32,
        hr: HRESULT,
        _i: Ref<IShellItem>,
    ) -> Result<()> {
        self.ev("post_new", String::new(), Some(hr));
        Ok(())
    }
    fn UpdateProgress(&self, _total: u32, _so_far: u32) -> Result<()> {
        self.st.progress_calls.set(self.st.progress_calls.get() + 1);
        Ok(())
    }
    fn ResetTimer(&self) -> Result<()> {
        Ok(())
    }
    fn PauseTimer(&self) -> Result<()> {
        Ok(())
    }
    fn ResumeTimer(&self) -> Result<()> {
        Ok(())
    }
}
