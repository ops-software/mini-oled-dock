//! Open, open with, properties, "open in Explorer" and typed commands in the shell host (SH6). Each runs inside
//! busy_enter, with no deadline (a verb may take as long as it needs; a kill decided before it starts refuses it with
//! E_ABORT), and its ShellExecuteExW inside a crumbs guard. The shell reports a failed open with its own UI, as
//! Explorer does; in test mode SEE_MASK_FLAG_NO_UI keeps every error box away. Pin, Unpin, Share and the modern
//! verbs answer E_NOTIMPL until the files app adds them (shell/pins.rs, shell/modern.rs).

use std::os::windows::ffi::OsStrExt;

use oled_trust::frame::files::{CallKind, ItemRef, ReqId, ShellEvt, Verb, WidePath};
use windows::core::{HSTRING, PCWSTR};
use windows::Win32::Foundation::{E_ABORT, E_INVALIDARG, E_NOTIMPL};
use windows::Win32::UI::Shell::{
    ShellExecuteExW, SEE_MASK_FLAG_NO_UI, SEE_MASK_INVOKEIDLIST, SEE_MASK_NOASYNC, SHELLEXECUTEINFOW,
};
use windows::Win32::UI::WindowsAndMessaging::SW_SHOWNORMAL;

use crate::shell::crumbs;
use crate::shell::host::HostCfg;
use crate::shell::writer::Writer;

/// How a verb runs.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ExecPlan {
    /// ShellExecuteExW of each item with this verb (None: the default verb); idlist adds SEE_MASK_INVOKEIDLIST.
    Shell { verb: Option<&'static str>, idlist: bool },
    /// %WINDIR%\explorer.exe with the quoted path.
    Explorer,
}

/// The plan of a verb, or the HRESULT it fails with.
pub fn plan(v: Verb) -> Result<ExecPlan, i32> {
    match v {
        Verb::Open => Ok(ExecPlan::Shell { verb: None, idlist: false }),
        Verb::OpenWith => Ok(ExecPlan::Shell { verb: Some("openas"), idlist: false }),
        Verb::Properties => Ok(ExecPlan::Shell { verb: Some("properties"), idlist: true }),
        Verb::OpenInExplorer => Ok(ExecPlan::Explorer),
        Verb::Pin | Verb::Unpin | Verb::Share | Verb::InvokeModern(_) => Err(E_NOTIMPL.0),
    }
}

fn wide(units: &[u16]) -> Vec<u16> {
    units.iter().copied().chain(Some(0)).collect()
}

/// One ShellExecuteExW, inside a crumbs guard of kind Exec; its HRESULT.
fn execute(verb: Option<&str>, file: &[u16], params: Option<&[u16]>, dir: Option<&[u16]>, mask: u32, ext: &str) -> i32 {
    let verb = verb.map(HSTRING::from);
    let file = wide(file);
    let params = params.map(wide);
    let dir = dir.map(wide);
    let pc = |v: &Option<Vec<u16>>| v.as_ref().map_or(PCWSTR::null(), |v| PCWSTR(v.as_ptr()));
    let mut info = SHELLEXECUTEINFOW {
        cbSize: std::mem::size_of::<SHELLEXECUTEINFOW>() as u32,
        fMask: mask,
        lpVerb: verb.as_ref().map_or(PCWSTR::null(), |v| PCWSTR(v.as_ptr())),
        lpFile: PCWSTR(file.as_ptr()),
        lpParameters: pc(&params),
        lpDirectory: pc(&dir),
        nShow: SW_SHOWNORMAL.0,
        ..Default::default()
    };
    let _g = crumbs::enter(CallKind::Exec, ext);
    // SAFETY: every string is NUL-terminated and lives for the call; the structure is sized in cbSize.
    match unsafe { ShellExecuteExW(&mut info) } {
        Ok(()) => 0,
        Err(e) => e.code().0,
    }
}

/// Runs the work under the busy protocol and answers ExecDone with its HRESULT (E_ABORT once a kill was decided).
fn busy_exec(req: ReqId, w: &Writer, work: impl FnOnce() -> i32) {
    let Ok(busy) = crumbs::busy_enter() else {
        w.send(&ShellEvt::ExecDone { req, hr: E_ABORT.0 });
        return;
    };
    w.send(&ShellEvt::Busy { on: true });
    let hr = work();
    drop(busy);
    w.send(&ShellEvt::Busy { on: false });
    w.send(&ShellEvt::ExecDone { req, hr });
}

/// Serves ShellReq::Exec on the exec STA.
pub fn run(req: ReqId, verb: Verb, items: &[ItemRef], w: &Writer, cfg: &HostCfg) {
    let plan = match plan(verb) {
        Ok(p) => p,
        Err(hr) => return w.send(&ShellEvt::Failed { req, hr }),
    };
    let mut paths: Vec<&WidePath> = Vec::with_capacity(items.len());
    for it in items {
        match it {
            ItemRef::Path(p) => paths.push(p),
            ItemRef::Pidl(_) => return w.send(&ShellEvt::Failed { req, hr: E_INVALIDARG.0 }),
        }
    }
    if paths.is_empty() {
        return w.send(&ShellEvt::Failed { req, hr: E_INVALIDARG.0 });
    }
    let no_ui = if cfg.test_no_ui { SEE_MASK_FLAG_NO_UI } else { 0 };
    busy_exec(req, w, || {
        let mut first = 0;
        for p in paths {
            let ext = crumbs::ext_of(p);
            let hr = match plan {
                ExecPlan::Shell { verb, idlist } => {
                    let mask = SEE_MASK_NOASYNC | no_ui | if idlist { SEE_MASK_INVOKEIDLIST } else { 0 };
                    execute(verb, p.units(), None, None, mask, &ext)
                }
                ExecPlan::Explorer => {
                    let windir = std::env::var_os("WINDIR").unwrap_or_else(|| "C:\\Windows".into());
                    let exe: Vec<u16> =
                        std::path::Path::new(&windir).join("explorer.exe").as_os_str().encode_wide().collect();
                    let quote = Some(u16::from(b'"'));
                    let quoted: Vec<u16> = quote.into_iter().chain(p.units().iter().copied()).chain(quote).collect();
                    execute(None, &exe, Some(&quoted), None, SEE_MASK_NOASYNC | no_ui, &ext)
                }
            };
            if first == 0 && hr < 0 {
                first = hr;
            }
        }
        first
    });
}

/// Serves ShellReq::RunTyped on the exec STA: the text as the Run box takes it, in the given folder.
pub fn run_typed(req: ReqId, text: &str, dir: &WidePath, w: &Writer, cfg: &HostCfg) {
    let no_ui = if cfg.test_no_ui { SEE_MASK_FLAG_NO_UI } else { 0 };
    let text: Vec<u16> = text.encode_utf16().collect();
    busy_exec(req, w, || execute(None, &text, None, Some(dir.units()), SEE_MASK_NOASYNC | no_ui, ""));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exec_unknown_verbs_fail_notimpl() {
        assert_eq!(plan(Verb::Open), Ok(ExecPlan::Shell { verb: None, idlist: false }));
        assert_eq!(plan(Verb::OpenWith), Ok(ExecPlan::Shell { verb: Some("openas"), idlist: false }));
        assert_eq!(plan(Verb::Properties), Ok(ExecPlan::Shell { verb: Some("properties"), idlist: true }));
        assert_eq!(plan(Verb::OpenInExplorer), Ok(ExecPlan::Explorer));
        for v in [Verb::Pin, Verb::Unpin, Verb::Share, Verb::InvokeModern(3)] {
            assert_eq!(plan(v), Err(E_NOTIMPL.0), "{v:?}");
        }
    }
}
