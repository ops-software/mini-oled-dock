//! fs3-probe: spike FS-3 of the files engine (plans P1-files-engine, SP1). What IFileOperation does when the sink
//! cancels or blocks (row e), with paths past 260 units (f), with a name collision under FOF_SILENT (a), with an
//! owner window in another, hung process (b), when the progress window would appear during a blocked callback (e2w),
//! when a recycled file is moved back out of the bin (g), which verbs the classic menu's ids read back as (i), and
//! what a Recycle does on volumes without a usable bin (c). Every row works only inside --root (guard.rs) and writes
//! its record to --json. Rows e and f run without any window (FOF_SILENT, FOF_NOCONFIRMATION, FOF_NOERRORUI,
//! FOF_NOCONFIRMMKDIR); a, b, e2w, g and i are the live tester's and the disposable CI job's; c runs only there.

use std::io::{BufRead, BufReader, Write as _};
use std::os::windows::fs::MetadataExt as _;
use std::os::windows::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::rc::Rc;
use std::sync::mpsc;
use std::time::{Duration, Instant};

use fs3_spike::guard::{self, inside};
use fs3_spike::json::J;
use fs3_spike::rules;
use fs3_spike::sink::{self, Act, State};
use fs3_spike::util::{
    from_wide, hash_file, hr_hex, hr_of, ms, names_in, pcwstr, system_facts, units, verbatim, wide_os, wide_str,
    Hidden, Sta,
};
use fs3_spike::watch::{self, Seen, Watcher};

use windows::core::{w, HRESULT, PCWSTR, PSTR};
use windows::Win32::Foundation::{HWND, RECT};
use windows::Win32::Storage::FileSystem::{
    GetFileAttributesW, GetVolumePathNameW, FILE_ATTRIBUTE_REPARSE_POINT, INVALID_FILE_ATTRIBUTES,
};
use windows::Win32::System::Com::{CoCreateInstance, CoTaskMemFree, IBindCtx, CLSCTX_ALL};
use windows::Win32::System::DataExchange::{IsClipboardFormatAvailable, RegisterClipboardFormatW};
use windows::Win32::System::Threading::{GetCurrentThreadId, CREATE_NO_WINDOW};
use windows::Win32::UI::Shell::Common::ITEMIDLIST;
use windows::Win32::UI::Shell::{
    FileOperation, IContextMenu, IFileOperation, IFileOperationProgressSink, IShellFolder, IShellItem, SHBindToParent,
    SHCreateItemFromParsingName, SHParseDisplayName, SHQueryRecycleBinW, CMF_CANRENAME, CMF_EXTENDEDVERBS, CMF_NORMAL,
    FILEOPERATION_FLAGS, FOF_ALLOWUNDO, FOF_NOCONFIRMATION, FOF_NOCONFIRMMKDIR, FOF_NOERRORUI, FOF_SILENT,
    FOF_WANTNUKEWARNING, GCS_VERBW, SHQUERYRBINFO,
};
use windows::Win32::UI::WindowsAndMessaging::{
    CreatePopupMenu, CreateWindowExW, DestroyMenu, DestroyWindow, DispatchMessageW, GetMenuItemCount, GetMenuItemInfoW,
    PeekMessageW, ShowWindow, SystemParametersInfoW, TranslateMessage, HMENU, MENUITEMINFOW, MFT_SEPARATOR, MIIM_FTYPE,
    MIIM_ID, MIIM_SUBMENU, MSG, PM_REMOVE, SPI_GETWORKAREA, SW_SHOWNOACTIVATE, SYSTEM_PARAMETERS_INFO_UPDATE_FLAGS,
    WS_CAPTION, WS_EX_NOACTIVATE, WS_OVERLAPPED,
};

const USAGE: &str =
    "usage: fs3-probe <row> --root <dir> --json <file> [--close-after-ms <n>] [--keep] [--no-silent] [--raw-verbs]
  rows without any window: e (cancel, pause, granularity), f (long paths)
  rows for the live tester and the disposable CI job: a (collision under FOF_SILENT), b (owner in a hung process;
    --no-silent drops FOF_SILENT), e2w (pause without FOF_SILENT), g (restore from the Recycle Bin), i (menu verbs)
  row for the disposable CI job only: c (Recycle on a share, FAT32, a full bin, a disabled bin)
  --root: a folder under %TEMP% whose first component below it starts with mod-files- (no reparse point on the path)
  --json: the record; its folder follows the same rule
  --close-after-ms: how long a dialog stays up before the probe closes it (default 5000, 2000 on CI)
  --keep: rows e and f keep their trees (default: removed at the end)
  --raw-verbs: row i writes every verb as it is (default: verbs outside the canonical list are masked)";

const ROWS: [&str; 8] = ["e", "f", "a", "e2w", "b", "g", "i", "c"];

/// Row e: 64 files of 1 MiB; the sink acts on the 10th PreCopyItem; the block lasts 5 s.
const E_FILES: usize = 64;
const E_ITEM: u32 = 10;
const E_BLOCK_MS: u32 = 5000;
/// Row f: the file's path is at least this long (folders of 40 units).
const F_UNITS: usize = 300;
/// Row b: the owner's process stops pumping for this long after it prints its window.
const B_HANG_MS: u64 = 20_000;
/// Row g: the bytes of r.txt (24); its bin file is deleted only at this length.
const G_BYTES: &[u8] = b"restore me from the bin\n";
/// Row i: the id range handed to QueryContextMenu.
const MENU_FIRST: u32 = 1;
const MENU_LAST: u32 = 0x7FFF;
/// Row i: the flags of the classic menus. The default menu adds Rename only for a caller that passes CMF_CANRENAME;
/// the second pass adds CMF_EXTENDEDVERBS.
const MENU_FLAGS: u32 = CMF_NORMAL | CMF_CANRENAME;
/// Row i: CF_HDROP, the clipboard format of a list of files (one number; the Ole feature is not needed for it).
const CF_HDROP: u32 = 15;
/// Row i: verbs written as they are; every other verb is masked (it can name an installed application).
const CANONICAL_VERBS: [&str; 30] = [
    "open",
    "opennew",
    "opennewwindow",
    "opennewprocess",
    "opennewtab",
    "openas",
    "edit",
    "print",
    "printto",
    "find",
    "explore",
    "runas",
    "runasuser",
    "properties",
    "delete",
    "cut",
    "copy",
    "paste",
    "pastelink",
    "rename",
    "link",
    "copyaspath",
    "pintohome",
    "sendto",
    "share",
    "preview",
    "play",
    "newfolder",
    "undelete",
    "removeproperties",
];

#[derive(Debug, PartialEq, Eq)]
struct Args {
    row: String,
    root: PathBuf,
    json: PathBuf,
    close_after: Option<u64>,
    keep: bool,
    no_silent: bool,
    raw_verbs: bool,
}

fn parse(args: &[String]) -> Result<Args, String> {
    let mut it = args.iter();
    let row = it.next().ok_or("no row given")?.clone();
    if !ROWS.contains(&row.as_str()) {
        return Err(format!("unknown row {row}"));
    }
    let (mut root, mut json, mut close_after) = (None, None, None);
    let (mut keep, mut no_silent, mut raw_verbs) = (false, false, false);
    while let Some(a) = it.next() {
        match a.as_str() {
            "--root" => root = Some(PathBuf::from(it.next().ok_or("--root needs a folder")?)),
            "--json" => json = Some(PathBuf::from(it.next().ok_or("--json needs a file")?)),
            "--close-after-ms" => {
                let v = it.next().ok_or("--close-after-ms needs a number")?;
                close_after = Some(v.parse::<u64>().map_err(|_| format!("--close-after-ms: {v} is not a number"))?);
            }
            "--keep" => keep = true,
            "--no-silent" => no_silent = true,
            "--raw-verbs" => raw_verbs = true,
            other => return Err(format!("unknown option {other}")),
        }
    }
    Ok(Args {
        row,
        root: root.ok_or("--root is required")?,
        json: json.ok_or("--json is required")?,
        close_after,
        keep,
        no_silent,
        raw_verbs,
    })
}

struct Ctx {
    root: PathBuf,
    t0: Instant,
    close_after: Duration,
    keep: bool,
    no_silent: bool,
    raw_verbs: bool,
    owner: HWND,
    op_thread: u32,
}

impl Ctx {
    fn check(&self, p: &Path) -> Result<(), String> {
        if inside(&self.root, p) {
            Ok(())
        } else {
            Err(format!("{} is outside the root", p.display()))
        }
    }

    fn mkdir(&self, p: &Path) -> Result<(), String> {
        self.check(p)?;
        std::fs::create_dir_all(verbatim(p)).map_err(|e| format!("create {}: {e}", p.display()))?;
        guard::no_reparse(p).map_err(|e| e.to_string())
    }

    fn put(&self, p: &Path, bytes: &[u8]) -> Result<(), String> {
        self.check(p)?;
        std::fs::write(verbatim(p), bytes).map_err(|e| format!("write {}: {e}", p.display()))
    }

    fn remove_file(&self, p: &Path) -> Result<(), String> {
        self.check(p)?;
        match std::fs::remove_file(verbatim(p)) {
            Err(e) if e.kind() != std::io::ErrorKind::NotFound => Err(format!("remove {}: {e}", p.display())),
            _ => Ok(()),
        }
    }

    /// A fresh folder `name` in the root (one left by an earlier run is removed first).
    fn fresh(&self, name: &str) -> Result<PathBuf, String> {
        let p = self.root.join(name);
        self.remove_tree(&p)?;
        self.mkdir(&p)?;
        Ok(p)
    }

    /// Removes a tree of the root (never the root itself); Rust's remove_dir_all does not follow junctions.
    fn remove_tree(&self, p: &Path) -> Result<(), String> {
        self.check(p)?;
        if p == self.root {
            return Err("the root itself is never removed".into());
        }
        if std::fs::symlink_metadata(verbatim(p)).is_err() {
            return Ok(());
        }
        guard::no_reparse(p).map_err(|e| e.to_string())?;
        std::fs::remove_dir_all(verbatim(p)).map_err(|e| format!("remove {}: {e}", p.display()))
    }

    fn cleanup(&self, p: &Path) -> J {
        if self.keep {
            return J::from("kept");
        }
        match self.remove_tree(p) {
            Ok(()) => J::from("removed"),
            Err(e) => J::from(e),
        }
    }

    fn watch(&self, label: &'static str, watch_for: Duration, close_after: Duration, ping: bool) -> watch::Cfg {
        watch::Cfg { label, watch_for, close_after, op_thread: self.op_thread, ping, t0: self.t0 }
    }

    /// Rows e and f: any window at all is closed at once (none is expected).
    fn safety(&self) -> watch::Cfg {
        self.watch("window", Duration::from_secs(600), Duration::ZERO, false)
    }
}

fn silent() -> FILEOPERATION_FLAGS {
    FOF_SILENT | FOF_NOCONFIRMATION | FOF_NOERRORUI | FOF_NOCONFIRMMKDIR
}

fn flags_hex(f: FILEOPERATION_FLAGS) -> String {
    format!("0x{:04X}", f.0)
}

fn item(p: &Path) -> Result<IShellItem, HRESULT> {
    let w = wide_os(p.as_os_str());
    // SAFETY: w is NUL-terminated and outlives the call.
    unsafe { SHCreateItemFromParsingName(pcwstr(&w), None::<&IBindCtx>) }.map_err(|e| e.code())
}

fn new_op(flags: FILEOPERATION_FLAGS, owner: Option<HWND>) -> Result<IFileOperation, String> {
    // SAFETY: COM is initialised (STA) on this thread for the whole run.
    let op: IFileOperation =
        unsafe { CoCreateInstance(&FileOperation, None, CLSCTX_ALL) }.map_err(|e| format!("CoCreateInstance: {e}"))?;
    // SAFETY: plain setters on the new object.
    unsafe { op.SetOperationFlags(flags) }.map_err(|e| format!("SetOperationFlags: {e}"))?;
    if let Some(h) = owner {
        // SAFETY: as above; h is a window handle (this process's, or row b's child's).
        unsafe { op.SetOwnerWindow(h) }.map_err(|e| format!("SetOwnerWindow: {e}"))?;
    }
    Ok(op)
}

/// An operation with its advised sink.
struct Op {
    op: IFileOperation,
    st: Rc<State>,
    cookie: u32,
}

impl Op {
    fn new(flags: FILEOPERATION_FLAGS, owner: Option<HWND>, act: Act, t0: Instant) -> Result<Op, String> {
        let op = new_op(flags, owner)?;
        let (sink, st) = sink::new(act, t0);
        // SAFETY: Advise on this thread's operation; Unadvise in Drop.
        let cookie = unsafe { op.Advise(&sink) }.map_err(|e| format!("Advise: {e}"))?;
        Ok(Op { op, st, cookie })
    }

    fn copy(&self, it: &IShellItem, dst: &IShellItem) -> Result<(), HRESULT> {
        // SAFETY: valid items; no per-item sink.
        unsafe { self.op.CopyItem(it, dst, PCWSTR::null(), None::<&IFileOperationProgressSink>) }.map_err(|e| e.code())
    }

    fn mv(&self, it: &IShellItem, dst: &IShellItem, name: Option<&str>) -> Result<(), HRESULT> {
        let w = name.map(wide_str);
        let n = w.as_deref().map_or(PCWSTR::null(), pcwstr);
        // SAFETY: valid items; the name buffer outlives the call.
        unsafe { self.op.MoveItem(it, dst, n, None::<&IFileOperationProgressSink>) }.map_err(|e| e.code())
    }

    fn rename(&self, it: &IShellItem, name: &str) -> Result<(), HRESULT> {
        let w = wide_str(name);
        // SAFETY: as above.
        unsafe { self.op.RenameItem(it, pcwstr(&w), None::<&IFileOperationProgressSink>) }.map_err(|e| e.code())
    }

    fn delete(&self, it: &IShellItem) -> Result<(), HRESULT> {
        // SAFETY: a valid item; no per-item sink.
        unsafe { self.op.DeleteItem(it, None::<&IFileOperationProgressSink>) }.map_err(|e| e.code())
    }

    fn perform(&self, cfg: watch::Cfg, t0: Instant) -> Done {
        let w = Watcher::start(cfg);
        let start_ms = ms(t0);
        // SAFETY: the operation was built on this STA thread; PerformOperations runs the queue here.
        let r = unsafe { self.op.PerformOperations() };
        let end_ms = ms(t0);
        // SAFETY: a getter on the same object.
        let aborted = unsafe { self.op.GetAnyOperationsAborted() }.ok().map(|b| b.as_bool());
        let seen = w.finish();
        Done { hr: hr_of(&r), aborted, start_ms, end_ms, seen }
    }
}

impl Drop for Op {
    fn drop(&mut self) {
        // SAFETY: pairs the Advise of new.
        let _ = unsafe { self.op.Unadvise(self.cookie) };
    }
}

/// What PerformOperations did.
struct Done {
    hr: HRESULT,
    aborted: Option<bool>,
    start_ms: u64,
    end_ms: u64,
    seen: Vec<Seen>,
}

impl Done {
    fn ok(&self) -> bool {
        self.hr.is_ok()
    }

    fn first_window(&self) -> Option<&Seen> {
        self.seen.iter().min_by_key(|s| s.seen_ms)
    }

    fn json(&self) -> J {
        J::obj()
            .set("hr", hr_hex(self.hr))
            .set("any_aborted", self.aborted)
            .set("start_ms", self.start_ms)
            .set("end_ms", self.end_ms)
            .set("windows", self.seen.iter().map(Seen::json).collect::<Vec<_>>())
    }
}

/// A row's verdict; `value` is a bool, or None ("not measured", written as null) where a row can end without one.
fn verdict(constant: &str, value: impl Into<J>, rule: &str) -> J {
    J::obj().set("constant", constant).set("value", value).set("rule", rule)
}

fn payload(i: usize, n: usize) -> Vec<u8> {
    (0..n).map(|k| ((k.wrapping_mul(31) + i * 7) & 0xff) as u8).collect()
}

// ------------------------------------------------------------------------------------------------ row e

fn e_sources(ctx: &Ctx, base: &Path) -> Result<Vec<PathBuf>, String> {
    let src = base.join("src");
    ctx.mkdir(&src)?;
    (0..E_FILES)
        .map(|i| {
            let p = src.join(format!("f{i:02}.bin"));
            ctx.put(&p, &payload(i, 1 << 20))?;
            Ok(p)
        })
        .collect()
}

/// Copies `files` into `dst` with one CopyItem each and an advised sink acting as `act` says.
fn copy_run(
    ctx: &Ctx,
    files: &[PathBuf],
    dst: &Path,
    flags: FILEOPERATION_FLAGS,
    act: Act,
    cfg: watch::Cfg,
) -> Result<(J, Done, Rc<State>, usize), String> {
    ctx.mkdir(dst)?;
    let op = Op::new(flags, None, act, ctx.t0)?;
    let dst_item = item(dst).map_err(|h| format!("item(dst): {}", hr_hex(h)))?;
    for f in files {
        ctx.check(f)?;
        let it = item(f).map_err(|h| format!("item({}): {}", f.display(), hr_hex(h)))?;
        op.copy(&it, &dst_item).map_err(|h| format!("CopyItem: {}", hr_hex(h)))?;
    }
    let done = op.perform(cfg, ctx.t0);
    let at_dst = names_in(dst).len();
    let st = op.st.clone();
    let j = J::obj()
        .set("flags", flags_hex(flags))
        .set("items", files.len())
        .set("perform", done.json())
        .set("pre_copy", st.count("pre_copy"))
        .set("post_copy", st.count("post_copy"))
        .set("post_copy_hr", st.hr_counts("post_copy"))
        .set("update_progress_calls", st.progress_calls.get())
        .set("acted_ms", st.acted_ms.get())
        .set("block_end_ms", st.block_end_ms.get())
        .set("files_at_dst", at_dst);
    Ok((j, done, st, at_dst))
}

fn row_e(ctx: &Ctx) -> Result<J, String> {
    let base = ctx.fresh("e")?;
    let files = e_sources(ctx, &base)?;

    let (j1, d1, s1, n1) = copy_run(ctx, &files, &base.join("dst1"), silent(), Act::CancelAt(E_ITEM), ctx.safety())?;
    let cancel = rules::cancel_in_op(s1.acted_ms.get(), d1.end_ms, n1);
    let e1 = j1
        .set("act", format!("PreCopyItem {E_ITEM} returns HRESULT_FROM_WIN32(ERROR_CANCELLED)"))
        .set("cancel_to_return_ms", s1.acted_ms.get().map(|c| d1.end_ms.saturating_sub(c)))
        .set(
            "verdict",
            verdict(
                "CANCEL_IN_OP",
                cancel,
                "PerformOperations returns within 1 s of the cancel and at most 11 files reach dst",
            ),
        );

    let (j2, d2, s2, n2) =
        copy_run(ctx, &files, &base.join("dst2"), silent(), Act::BlockAt(E_ITEM, E_BLOCK_MS), ctx.safety())?;
    let block = s2.acted_ms.get().zip(s2.block_end_ms.get());
    let completed = rules::pause_completed(d2.ok(), d2.aborted, n2, E_FILES, block);
    let e2 = j2
        .set("act", format!("PreCopyItem {E_ITEM} blocks {E_BLOCK_MS} ms on an event, then returns S_OK"))
        .set("total_ms", d2.end_ms.saturating_sub(d2.start_ms))
        .set(
            "verdict",
            verdict(
                "PAUSE_OK (with LIVE-F1 e2w)",
                completed,
                "the copy completes after the block, all 64 files at dst",
            ),
        );

    // e3: one folder of 3 files and a subfolder of 2.
    let tree = base.join("tree");
    ctx.mkdir(&tree.join("sub"))?;
    for n in ["a.txt", "b.txt", "c.txt"] {
        ctx.put(&tree.join(n), n.as_bytes())?;
    }
    for n in ["d.txt", "e.txt"] {
        ctx.put(&tree.join("sub").join(n), n.as_bytes())?;
    }
    let (j3, _d3, s3, _) =
        copy_run(ctx, std::slice::from_ref(&tree), &base.join("dst3"), silent(), Act::Nothing, ctx.safety())?;
    let pre: Vec<String> = s3.events.borrow().iter().filter(|e| e.kind == "pre_copy").map(|e| e.name.clone()).collect();
    let nested = pre.iter().any(|n| n != "tree");
    let e3 = j3
        .set("pre_copy_names", pre.into_iter().map(J::from).collect::<Vec<_>>())
        .set("events", s3.events_json())
        .set("granularity", if nested { "every nested item" } else { "top-level items only" });

    Ok(J::obj().set("e1", e1).set("e2", e2).set("e3", e3).set("cleanup", ctx.cleanup(&base)))
}

// ------------------------------------------------------------------------------------------------ row f

#[derive(Clone, Copy)]
enum FOp<'a> {
    Copy(&'a Path),
    Move(&'a Path),
    Rename(&'a str),
    Delete,
}

fn exists(p: &Path) -> bool {
    std::fs::symlink_metadata(verbatim(p)).is_ok()
}

/// One operation of row f on a path past 260 units: the item from the plain or the `\\?\` form, the result.
fn f_step(ctx: &Ctx, op_kind: FOp<'_>, src: &Path, plain: bool, present: Option<&Path>, absent: Option<&Path>) -> J {
    let name = match op_kind {
        FOp::Copy(_) => "copy",
        FOp::Move(_) => "move",
        FOp::Rename(_) => "rename",
        FOp::Delete => "delete",
    };
    let mut j = J::obj().set("op", name).set("item_units", units(src));
    if let Err(e) = ctx.put(src, b"long path\n").and_then(|()| present.map_or(Ok(()), |p| ctx.remove_file(p))) {
        return j.set("error", e);
    }
    let form = |p: &Path| if plain { p.to_path_buf() } else { verbatim(p) };
    let it = match item(&form(src)) {
        Ok(i) => i,
        Err(h) => return j.set("item_hr", hr_hex(h)),
    };
    j = j.set("item_hr", "0x00000000");
    let op = match Op::new(silent(), None, Act::Nothing, ctx.t0) {
        Ok(o) => o,
        Err(e) => return j.set("error", e),
    };
    let queued = match op_kind {
        FOp::Copy(d) | FOp::Move(d) => {
            let dst = match item(&form(d)) {
                Ok(i) => i,
                Err(h) => return j.set("dst_hr", hr_hex(h)),
            };
            j = j.set("dst_hr", "0x00000000").set("dst_units", units(d));
            if matches!(op_kind, FOp::Copy(_)) {
                op.copy(&it, &dst)
            } else {
                op.mv(&it, &dst, None)
            }
        }
        FOp::Rename(n) => op.rename(&it, n),
        FOp::Delete => op.delete(&it),
    };
    j = j.set("queue_hr", hr_hex(queued.err().unwrap_or(HRESULT(0))));
    if queued.is_err() {
        return j;
    }
    let done = op.perform(ctx.safety(), ctx.t0);
    let post: Vec<J> = op
        .st
        .events
        .borrow()
        .iter()
        .filter(|e| e.kind.starts_with("post_"))
        .map(|e| J::from(e.hr.map(hr_hex)))
        .collect();
    let outcome = present.is_none_or(exists) && absent.is_none_or(|p| !exists(p));
    j.set("perform", done.json()).set("post_hr", post).set("outcome_as_expected", outcome)
}

fn row_f(ctx: &Ctx) -> Result<J, String> {
    let base = ctx.fresh("f")?;
    let mut deep = base.join("long");
    let mut i = 0;
    while units(&deep.join("src").join("file.txt")) < F_UNITS {
        deep.push(format!("{i:02}{}", "d".repeat(38)));
        i += 1;
    }
    let (src, copy, mv) = (deep.join("src"), deep.join("copy"), deep.join("move"));
    for d in [&src, &copy, &mv] {
        ctx.mkdir(d)?;
    }
    let file = src.join("file.txt");
    ctx.put(&file, b"long path\n")?;
    let w = wide_os(file.as_os_str());
    // SAFETY: w is NUL-terminated; a read-only query.
    let plain_win32 = unsafe { GetFileAttributesW(pcwstr(&w)) } != INVALID_FILE_ATTRIBUTES;
    let (copied, moved, renamed) = (copy.join("file.txt"), mv.join("file.txt"), mv.join("renamed.txt"));
    let mut forms = J::obj();
    for (form, plain) in [("plain", true), ("verbatim", false)] {
        let steps = vec![
            f_step(ctx, FOp::Copy(&copy), &file, plain, Some(&copied), None),
            f_step(ctx, FOp::Move(&mv), &copied, plain, Some(&moved), Some(&copied)),
            f_step(ctx, FOp::Rename("renamed.txt"), &moved, plain, Some(&renamed), Some(&moved)),
            f_step(ctx, FOp::Delete, &renamed, plain, None, Some(&renamed)),
        ];
        forms = forms.set(form, J::Arr(steps));
    }
    Ok(J::obj()
        .set("file_units", units(&file))
        .set("folder_units", units(&deep))
        .set("win32_plain_path_reads", plain_win32)
        .set("forms", forms)
        .set("cleanup", ctx.cleanup(&base)))
}

// ------------------------------------------------------------------------------------------------ rows a and b

/// Copies src\a.txt onto a dst\a.txt that holds other bytes: the collision the system's dialog asks about.
fn collision(
    ctx: &Ctx,
    base: &Path,
    owner: HWND,
    flags: FILEOPERATION_FLAGS,
    cfg: watch::Cfg,
) -> Result<(J, Done, bool), String> {
    let (src, dst) = (base.join("src"), base.join("dst"));
    ctx.mkdir(&src)?;
    ctx.mkdir(&dst)?;
    ctx.put(&src.join("a.txt"), b"source: must never replace the destination\n")?;
    ctx.put(&dst.join("a.txt"), b"destination: other content, kept\n")?;
    let before = hash_file(&dst.join("a.txt"));
    let op = Op::new(flags, Some(owner), Act::Nothing, ctx.t0)?;
    let it = item(&src.join("a.txt")).map_err(|h| format!("item(src): {}", hr_hex(h)))?;
    let d = item(&dst).map_err(|h| format!("item(dst): {}", hr_hex(h)))?;
    op.copy(&it, &d).map_err(|h| format!("CopyItem: {}", hr_hex(h)))?;
    let done = op.perform(cfg, ctx.t0);
    let after = hash_file(&dst.join("a.txt"));
    let equal = before.is_some() && before == after;
    let j = J::obj()
        .set("flags", flags_hex(flags))
        .set("perform", done.json())
        .set("events", op.st.events_json())
        .set("dst_hash_before", before)
        .set("dst_hash_after", after)
        .set("dst_hash_equal", equal)
        .set("dst_names", names_in(&dst).into_iter().map(J::from).collect::<Vec<_>>());
    Ok((j, done, equal))
}

fn row_a(ctx: &Ctx) -> Result<J, String> {
    let base = ctx.fresh("a")?;
    let flags = FOF_SILENT | FOF_ALLOWUNDO | FOF_NOCONFIRMMKDIR;
    let cfg = ctx.watch("dialog", Duration::from_secs(10), ctx.close_after, false);
    let (j, done, equal) = collision(ctx, &base, ctx.owner, flags, cfg)?;
    let first = done.first_window().map(|s| s.seen_ms);
    let v = rules::dialogs_under_silent(first, done.start_ms, equal);
    Ok(j.set(
        "verdict",
        verdict(
            "DIALOGS_UNDER_SILENT",
            v,
            "a dialog within 3 s of PerformOperations and the destination's hash unchanged",
        ),
    ))
}

/// Kills and waits for the child when the row ends (D8: no process of a probe outlives it).
struct ChildGuard(Child);

impl Drop for ChildGuard {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

fn row_b(ctx: &Ctx) -> Result<J, String> {
    let base = ctx.fresh("b")?;
    let exe = std::env::current_exe().map_err(|e| format!("current_exe: {e}"))?;
    let mut child = ChildGuard(
        Command::new(exe)
            .creation_flags(CREATE_NO_WINDOW.0)
            .arg("b-ui")
            .env("FILES_PROBE_B_UI", "1")
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .map_err(|e| format!("spawn b-ui: {e}"))?,
    );
    let out = child.0.stdout.take().ok_or("no stdout of b-ui")?;
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        for line in BufReader::new(out).lines().map_while(Result::ok) {
            if tx.send(line).is_err() {
                break;
            }
        }
    });
    let line = rx.recv_timeout(Duration::from_secs(5)).map_err(|_| "b-ui printed no window in 5 s")?;
    let h: isize =
        line.strip_prefix("hwnd ").and_then(|s| s.trim().parse().ok()).ok_or(format!("b-ui said {line:?}"))?;
    // The child stops pumping right after printing; count conservatively from when the line arrived.
    let hung_until = ms(ctx.t0) + B_HANG_MS - 200;
    let flags = if ctx.no_silent {
        FOF_ALLOWUNDO | FOF_NOCONFIRMMKDIR
    } else {
        FOF_SILENT | FOF_ALLOWUNDO | FOF_NOCONFIRMMKDIR
    };
    let cfg = ctx.watch("dialog", Duration::from_secs(30), ctx.close_after, true);
    let (j, done, _) = collision(ctx, &base, HWND(h as *mut core::ffi::c_void), flags, cfg)?;
    drop(child);
    let first = done.first_window();
    let answered = first.map_or(0, Seen::answered);
    let v = rules::owner_across_processes(first.map(|s| s.seen_ms), hung_until, answered);
    Ok(j.set("owner_hung_until_ms", hung_until).set("answered_of_5", answered).set(
        "verdict",
        verdict(
            "OWNER_ACROSS_PROCESSES",
            v,
            "the dialog appears while the owner's process is hung and answers 5 of 5 WM_NULL within 500 ms",
        ),
    ))
}

/// Row b's child: a visible 120x80 window titled "files probe" at the bottom-left of the work area, never
/// activated; it prints the window and stops pumping for 20 s.
fn b_ui() -> i32 {
    if std::env::var("FILES_PROBE_B_UI").as_deref() != Ok("1") {
        eprintln!("fs3-probe: b-ui runs only as row b's child");
        return 2;
    }
    let mut wa = RECT::default();
    // SAFETY: SPI_GETWORKAREA writes one RECT.
    let _ = unsafe {
        SystemParametersInfoW(
            SPI_GETWORKAREA,
            0,
            Some(&mut wa as *mut RECT as *mut _),
            SYSTEM_PARAMETERS_INFO_UPDATE_FLAGS(0),
        )
    };
    // SAFETY: a STATIC window of this thread, destroyed below.
    let h = match unsafe {
        CreateWindowExW(
            WS_EX_NOACTIVATE,
            w!("STATIC"),
            w!("files probe"),
            WS_OVERLAPPED | WS_CAPTION,
            wa.left,
            wa.bottom - 80,
            120,
            80,
            None,
            None,
            None,
            None,
        )
    } {
        Ok(h) => h,
        Err(e) => {
            eprintln!("fs3-probe: CreateWindowExW: {e}");
            return 1;
        }
    };
    // SAFETY: shows the window without activating it.
    let _ = unsafe { ShowWindow(h, SW_SHOWNOACTIVATE) };
    pump(Duration::from_millis(300));
    println!("hwnd {}", h.0 as isize);
    let _ = std::io::stdout().flush();
    std::thread::sleep(Duration::from_millis(B_HANG_MS));
    pump(Duration::from_millis(200));
    // SAFETY: the window of this thread, destroyed once.
    let _ = unsafe { DestroyWindow(h) };
    0
}

fn pump(for_how_long: Duration) {
    let end = Instant::now() + for_how_long;
    let mut msg = MSG::default();
    while Instant::now() < end {
        // SAFETY: the standard message loop of this thread.
        unsafe {
            while PeekMessageW(&mut msg, None, 0, 0, PM_REMOVE).as_bool() {
                let _ = TranslateMessage(&msg);
                DispatchMessageW(&msg);
            }
        }
        std::thread::sleep(Duration::from_millis(10));
    }
}

// ------------------------------------------------------------------------------------------------ row e2w

fn row_e2w(ctx: &Ctx) -> Result<J, String> {
    let base = ctx.fresh("e2w")?;
    let files = e_sources(ctx, &base)?;
    let flags = FOF_NOCONFIRMATION | FOF_NOERRORUI | FOF_NOCONFIRMMKDIR;
    let cfg = ctx.watch("window", Duration::from_secs(30), ctx.close_after, false);
    let (j, done, st, n) = copy_run(ctx, &files, &base.join("dst"), flags, Act::BlockAt(E_ITEM, E_BLOCK_MS), cfg)?;
    let block = st.acted_ms.get().zip(st.block_end_ms.get());
    let wins: Vec<(u64, Option<u64>)> = done.seen.iter().map(|s| (s.seen_ms, s.gone_ms)).collect();
    let quiet = rules::no_window_during(&wins, block);
    let completed = rules::pause_completed(done.ok(), done.aborted, n, E_FILES, block);
    Ok(j.set("no_window_during_block", quiet).set("completed", completed).set(
        "verdict",
        verdict(
            "PAUSE_OK",
            quiet && completed,
            "no window while the callback blocks and the copy completes (with row e's e2)",
        ),
    ))
}

// ------------------------------------------------------------------------------------------------ row g

fn volume_of(p: &Path) -> Result<String, String> {
    let w = wide_os(p.as_os_str());
    let mut buf = [0u16; 300];
    // SAFETY: w is NUL-terminated; buf is writable for its length.
    unsafe { GetVolumePathNameW(pcwstr(&w), &mut buf) }.map_err(|e| format!("GetVolumePathNameW: {e}"))?;
    Ok(from_wide(&buf))
}

fn bin_count(vol: &str) -> Option<i64> {
    let w = wide_str(vol);
    let mut info = SHQUERYRBINFO { cbSize: std::mem::size_of::<SHQUERYRBINFO>() as u32, ..Default::default() };
    // SAFETY: w is NUL-terminated; info is sized.
    unsafe { SHQueryRecycleBinW(pcwstr(&w), &mut info) }.ok().map(|()| info.i64NumItems)
}

/// A bin item's parsing name with the user's SID masked.
fn mask_bin_name(name: &str) -> String {
    name.split('\\').map(|c| if c.starts_with("S-1-") { "<sid>" } else { c }).collect::<Vec<_>>().join("\\")
}

/// Whether `name` is the bin's file of one recycled `.ext` file on `vol`:
/// `<vol>$Recycle.Bin\<one folder>\$R<rest>.<ext>`, exactly two components below $Recycle.Bin (never the bin, a
/// user's folder of it, anything deeper or an $I record).
fn is_bin_path(name: &str, vol: &str, ext: &str) -> bool {
    let (name, suffix) = (name.to_lowercase(), format!(".{}", ext.to_lowercase()));
    let Some(rest) = name.strip_prefix(&format!("{}$recycle.bin\\", vol.to_lowercase())) else {
        return false;
    };
    let plain = |c: &str| !c.is_empty() && c != "." && c != ".." && !c.contains(['/', ':']);
    let recycled = |c: &str| c.strip_prefix("$r").and_then(|s| s.strip_suffix(&suffix)).is_some_and(|s| !s.is_empty());
    let parts: Vec<&str> = rest.split('\\').collect();
    matches!(parts.as_slice(), [folder, leaf] if plain(folder) && plain(leaf) && recycled(leaf))
}

/// Whether the bin's file `name` is still there as a plain file (not a folder, not a reparse point) of `len` bytes;
/// read on the file system, not parsed through the shell (the bin's user folder is a shell junction).
fn is_bin_file(name: &str, len: u64) -> bool {
    std::fs::symlink_metadata(verbatim(Path::new(name)))
        .is_ok_and(|m| m.is_file() && m.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT.0 == 0 && m.len() == len)
}

fn row_g(ctx: &Ctx) -> Result<J, String> {
    let base = ctx.fresh("g")?;
    let r = base.join("r.txt");
    ctx.put(&r, G_BYTES)?;
    let h0 = hash_file(&r);
    let vol = volume_of(&base)?;
    let bin0 = bin_count(&vol);
    let dialog = || ctx.watch("dialog", Duration::from_secs(10), ctx.close_after, false);

    let del = Op::new(FOF_ALLOWUNDO | FOF_SILENT, Some(ctx.owner), Act::Nothing, ctx.t0)?;
    del.delete(&item(&r).map_err(|h| format!("item(r.txt): {}", hr_hex(h)))?)
        .map_err(|h| format!("DeleteItem: {}", hr_hex(h)))?;
    let d1 = del.perform(dialog(), ctx.t0);
    // A delete that never happened (a confirmation the watcher cancelled, any failure) leaves r.txt where it was.
    let recycled = !exists(&r);
    let new = del.st.deleted_new.borrow().first().cloned();
    let bin1 = bin_count(&vol);
    let in_bin = new.as_ref().is_some_and(|(n, _)| is_bin_path(n, &vol, "txt"));

    let mut j = J::obj()
        .set("volume", vol.as_str())
        .set("bin_start", bin0)
        .set("delete", d1.json().set("flags", flags_hex(FOF_ALLOWUNDO | FOF_SILENT)))
        .set("gone_after_delete", recycled)
        .set("new_item", new.as_ref().map(|(n, _)| mask_bin_name(n)))
        .set("new_item_in_bin_folder", in_bin)
        .set("bin_after_delete", bin1);
    let mut restore_ok = false;
    // Only a file that left its folder is moved back (r.txt still there would make the move a collision).
    if let (Some((_, it)), true, true) = (&new, in_bin, recycled) {
        let flags = FOF_SILENT | FOF_NOERRORUI | FOF_NOCONFIRMMKDIR;
        let mv = Op::new(flags, Some(ctx.owner), Act::Nothing, ctx.t0)?;
        let folder = item(&base).map_err(|h| format!("item(root): {}", hr_hex(h)))?;
        let q = mv.mv(it, &folder, Some("r.txt"));
        j = j.set("restore_queue_hr", hr_hex(q.err().unwrap_or(HRESULT(0))));
        if q.is_ok() {
            let done = mv.perform(dialog(), ctx.t0);
            restore_ok = done.ok();
            j = j.set("restore", done.json().set("flags", flags_hex(flags)));
        }
    }
    let restored = exists(&r) && h0.is_some() && hash_file(&r) == h0;
    let bin2 = bin_count(&vol);
    j = j.set("restore_ok", restore_ok).set("restored_hash_equal", restored).set("bin_after_restore", bin2);
    // Not moved back: delete exactly the bin item of this run (never anything else), so the bin returns to its count;
    // only while its name is one recycled file and the file system shows a plain file of r.txt's length there.
    if let (false, true, Some((name, it))) = (restore_ok && restored, in_bin, &new) {
        if is_bin_file(name, G_BYTES.len() as u64) {
            let flags = FOF_SILENT | FOF_NOCONFIRMATION | FOF_NOERRORUI;
            let op = Op::new(flags, Some(ctx.owner), Act::Nothing, ctx.t0)?;
            op.delete(it).map_err(|h| format!("DeleteItem(bin item): {}", hr_hex(h)))?;
            j = j
                .set("bin_item_removed", op.perform(dialog(), ctx.t0).json())
                .set("bin_after_cleanup", bin_count(&vol));
        } else {
            j = j.set("bin_item_removed", "no plain file of r.txt's length at the bin item: reported, not cleaned");
        }
    }
    let v = rules::restore_from_bin(recycled && in_bin, restore_ok && restored, bin0, bin2);
    Ok(j.set("cleanup", ctx.cleanup(&base)).set(
        "verdict",
        verdict(
            "RESTORE_FROM_BIN",
            v,
            "the file left its folder for the bin, the move back succeeded with an equal hash, and the bin count is back at its start value",
        ),
    ))
}

// ------------------------------------------------------------------------------------------------ row i

fn collect_ids(menu: HMENU, out: &mut Vec<u32>, depth: u32) {
    // SAFETY: a menu this row created and still owns.
    let n = unsafe { GetMenuItemCount(Some(menu)) };
    for i in 0..u32::try_from(n).unwrap_or(0) {
        let mut mii = MENUITEMINFOW {
            cbSize: std::mem::size_of::<MENUITEMINFOW>() as u32,
            fMask: MIIM_ID | MIIM_SUBMENU | MIIM_FTYPE,
            ..Default::default()
        };
        // SAFETY: mii is sized and asks for scalars only.
        if unsafe { GetMenuItemInfoW(menu, i, true, &mut mii) }.is_err() || mii.fType.0 & MFT_SEPARATOR.0 != 0 {
            continue;
        }
        if !mii.hSubMenu.is_invalid() && depth < 4 {
            collect_ids(mii.hSubMenu, out, depth + 1);
        }
        out.push(mii.wID);
    }
}

/// The classic menu of `path`, built and never shown; for every id the verb it reads back as.
fn menu_verbs(path: &Path, owner: HWND, flags: u32, raw: bool) -> (J, Vec<String>) {
    let mut verbs = Vec::new();
    let w = wide_os(path.as_os_str());
    let mut pidl: *mut ITEMIDLIST = std::ptr::null_mut();
    // SAFETY: w is NUL-terminated; pidl receives a CoTaskMem ITEMIDLIST freed below.
    if let Err(e) = unsafe { SHParseDisplayName(pcwstr(&w), None::<&IBindCtx>, &mut pidl, 0, None) } {
        return (J::obj().set("parse_hr", hr_hex(e.code())), verbs);
    }
    let mut child: *mut ITEMIDLIST = std::ptr::null_mut();
    // SAFETY: pidl is valid; child points into pidl and is not freed separately.
    let cm: windows::core::Result<IContextMenu> = unsafe { SHBindToParent::<IShellFolder>(pidl, Some(&mut child)) }
        .and_then(|folder| unsafe { folder.GetUIObjectOf(owner, &[child as *const ITEMIDLIST], None) });
    let mut j = J::obj().set("flags", format!("0x{flags:04X}"));
    match (cm, unsafe { CreatePopupMenu() }) {
        (Ok(cm), Ok(menu)) => {
            // SAFETY: a menu of this thread; the handler adds its items with ids in [FIRST, LAST].
            let hr = unsafe { cm.QueryContextMenu(menu, 0, MENU_FIRST, MENU_LAST, flags) };
            let mut ids = Vec::new();
            collect_ids(menu, &mut ids, 0);
            let mut items = Vec::new();
            for id in ids.into_iter().filter(|id| (MENU_FIRST..=MENU_LAST).contains(id)) {
                let mut buf = [0u16; 260];
                // SAFETY: GCS_VERBW writes at most buf.len() UTF-16 units into buf.
                let r = unsafe {
                    cm.GetCommandString(
                        (id - MENU_FIRST) as usize,
                        GCS_VERBW,
                        None,
                        PSTR(buf.as_mut_ptr() as *mut u8),
                        buf.len() as u32,
                    )
                };
                let e = J::obj().set("id", id);
                items.push(match r {
                    Ok(()) => {
                        let v = from_wide(&buf);
                        let known = CANONICAL_VERBS.iter().any(|c| c.eq_ignore_ascii_case(&v));
                        verbs.push(v.clone());
                        if raw || known {
                            e.set("verb", v)
                        } else {
                            e.set("verb", "<other>").set("len", v.len())
                        }
                    }
                    Err(err) => e.set("hr", hr_hex(err.code())),
                });
            }
            j = j.set("query_hr", hr_hex(hr)).set("items", items);
            // SAFETY: the menu created above, destroyed once.
            let _ = unsafe { DestroyMenu(menu) };
        }
        (Err(e), menu) => {
            j = j.set("context_menu_hr", hr_hex(e.code()));
            if let Ok(m) = menu {
                // SAFETY: as above.
                let _ = unsafe { DestroyMenu(m) };
            }
        }
        (Ok(_), Err(e)) => j = j.set("menu_error", e.to_string()),
    }
    // SAFETY: frees the ITEMIDLIST SHParseDisplayName allocated.
    unsafe { CoTaskMemFree(Some(pidl as *const core::ffi::c_void)) };
    (j, verbs)
}

/// Whether the clipboard holds files, as (CF_HDROP, "Shell IDList Array"): read with IsClipboardFormatAvailable only,
/// so the clipboard is never opened nor written.
fn clipboard_files() -> (bool, bool) {
    // SAFETY: a query of the formats on the clipboard; nothing is opened.
    let hdrop = unsafe { IsClipboardFormatAvailable(CF_HDROP) }.is_ok();
    // SAFETY: a NUL-terminated literal; the shell's own format name, which returns its existing id.
    let id = unsafe { RegisterClipboardFormatW(w!("Shell IDList Array")) };
    // SAFETY: as the first query.
    let idlist = id != 0 && unsafe { IsClipboardFormatAvailable(id) }.is_ok();
    (hdrop, idlist)
}

fn row_i(ctx: &Ctx) -> Result<J, String> {
    let base = ctx.fresh("i")?;
    let f = base.join("menu.txt");
    let d = base.join("menu-folder");
    ctx.put(&f, b"menu\n")?;
    ctx.mkdir(&d)?;
    // The folder's Paste depends on what the clipboard holds: read before the menus are built.
    let (hdrop, idlist) = clipboard_files();
    let w = Watcher::start(ctx.watch("window", Duration::from_secs(600), ctx.close_after, false));
    let (fnorm, fv) = menu_verbs(&f, ctx.owner, MENU_FLAGS, ctx.raw_verbs);
    let (dnorm, dv) = menu_verbs(&d, ctx.owner, MENU_FLAGS, ctx.raw_verbs);
    let (fext, fxv) = menu_verbs(&f, ctx.owner, MENU_FLAGS | CMF_EXTENDEDVERBS, ctx.raw_verbs);
    let (dext, dxv) = menu_verbs(&d, ctx.owner, MENU_FLAGS | CMF_EXTENDEDVERBS, ctx.raw_verbs);
    let seen = w.finish();
    let (value, gaps) = rules::menu_verdict(&fv, &dv, hdrop || idlist);
    let mut v = verdict(
        "menu interception reliable (F 8.1 i)",
        value,
        "delete, cut, copy, rename read back on the file's menu and paste on the folder's (CMF_NORMAL | CMF_CANRENAME); null (not measured) when only paste is missing and the clipboard holds no files; otherwise false",
    );
    if !gaps.is_empty() {
        v = v.set("not_measured", gaps.iter().map(|g| J::from(*g)).collect::<Vec<_>>());
    }
    Ok(J::obj()
        .set("clipboard", J::obj().set("hdrop", hdrop).set("shell_idlist", idlist))
        .set("file", J::obj().set("normal", fnorm).set("extended", fext))
        .set("folder", J::obj().set("normal", dnorm).set("extended", dext))
        .set("extended_pass", rules::menu_pass(&fxv, &dxv))
        .set("windows", seen.iter().map(Seen::json).collect::<Vec<_>>())
        .set("cleanup", ctx.cleanup(&base))
        .set("verdict", v))
}

// ------------------------------------------------------------------------------------------------ row c

fn disposable() -> Result<(), String> {
    let v = |k: &str| std::env::var(k).ok();
    if v("FILES_DISPOSABLE").as_deref() == Some("1")
        && v("GITHUB_ACTIONS").as_deref() == Some("true")
        && v("RUNNER_ENVIRONMENT").as_deref() == Some("github-hosted")
    {
        Ok(())
    } else {
        Err("row c runs only in the disposable CI job (FILES_DISPOSABLE=1 on a GitHub-hosted runner)".into())
    }
}

fn c_case(ctx: &Ctx, case: &str, var: &str, size: usize) -> J {
    let Ok(base) = std::env::var(var) else {
        return J::obj().set("skipped", format!("{var} is not set"));
    };
    // The probe's own folder on the disposable volume or share; nothing else there is touched.
    let dir = PathBuf::from(base).join(format!("mod-files-{case}"));
    let file = dir.join(format!("{case}.bin"));
    if let Err(e) = std::fs::create_dir_all(&dir).and_then(|()| std::fs::write(&file, payload(1, size))) {
        return J::obj().set("error", format!("prepare: {e}"));
    }
    let vol = volume_of(&dir).ok();
    let bin0 = vol.as_deref().and_then(bin_count);
    let flags = FOF_ALLOWUNDO | FOF_WANTNUKEWARNING | FOF_SILENT;
    let op = match Op::new(flags, Some(ctx.owner), Act::Nothing, ctx.t0) {
        Ok(o) => o,
        Err(e) => return J::obj().set("error", e),
    };
    let it = match item(&file) {
        Ok(i) => i,
        Err(h) => return J::obj().set("item_hr", hr_hex(h)),
    };
    if let Err(h) = op.delete(&it) {
        return J::obj().set("queue_hr", hr_hex(h));
    }
    let done = op.perform(ctx.watch("dialog", Duration::from_secs(10), ctx.close_after, false), ctx.t0);
    let still = file.exists();
    let new = op.st.deleted_new.borrow().first().map(|(n, _)| n.clone());
    let dialog = !done.seen.is_empty();
    let outcome = rules::delete_outcome(dialog, still, new.is_some());
    J::obj()
        .set("size", size)
        .set("flags", flags_hex(flags))
        .set("perform", done.json())
        .set("dialog", dialog)
        .set("still_at_source", still)
        .set("in_bin", new.is_some())
        .set("new_item", new.as_deref().map(mask_bin_name))
        .set("bin_before", bin0)
        .set("bin_after", vol.as_deref().and_then(bin_count))
        .set("outcome", outcome)
        .set("reproduced", rules::reproduced(case, outcome))
}

fn row_c(ctx: &Ctx) -> Result<J, String> {
    disposable()?;
    let cases = [
        ("c1", "FILES_DISP_UNC", 4096),
        ("c2", "FILES_DISP_FAT", 4096),
        ("c3", "FILES_DISP_QUOTA", 2 << 20),
        ("c4", "FILES_DISP_NUKE", 4096),
    ];
    let mut j = J::obj();
    let mut seen: Vec<(&str, Option<String>, bool)> = Vec::new();
    for (case, var, size) in cases {
        let r = c_case(ctx, case, var, size);
        // Measured only with an outcome: a skipped case, a prepare error or a refused item has none.
        let outcome = match r.get("outcome") {
            Some(J::Str(s)) => Some(s.clone()),
            _ => None,
        };
        let reproduced = r.get("reproduced").and_then(J::as_bool).unwrap_or(false);
        seen.push((case, outcome, reproduced));
        j = j.set(case, r);
    }
    let cases: Vec<(&str, Option<&str>, bool)> = seen.iter().map(|(c, o, r)| (*c, o.as_deref(), *r)).collect();
    let (value, gaps) = rules::no_data_loss(&cases);
    let mut v = verdict(
        "no data loss on Recycle",
        value,
        "false when a case ends with the file gone, not in the bin and no dialog; true only when every case has an outcome and c3 and c4 reproduced their bin value; otherwise null (not measured)",
    );
    if !gaps.is_empty() {
        v = v.set("not_measured", gaps.iter().map(|c| J::from(*c)).collect::<Vec<_>>());
    }
    Ok(j.set("verdict", v))
}

// ------------------------------------------------------------------------------------------------ main

fn run(a: &Args) -> Result<(PathBuf, J), (i32, String)> {
    let root = guard::prepare_root(&a.root).map_err(|e| (2, format!("refused --root {}: {e}", a.root.display())))?;
    let json = guard::prepare_record(&a.json).map_err(|e| (2, format!("refused --json {}: {e}", a.json.display())))?;
    if a.row == "c" {
        disposable().map_err(|e| (2, e))?;
    }
    let _sta = Sta::init().map_err(|e| (1, e))?;
    let hidden = Hidden::new().map_err(|e| (1, e))?;
    let ci = std::env::var("GITHUB_ACTIONS").as_deref() == Ok("true");
    let ctx = Ctx {
        root,
        t0: Instant::now(),
        close_after: Duration::from_millis(a.close_after.unwrap_or(if ci { 2000 } else { 5000 })),
        keep: a.keep,
        no_silent: a.no_silent,
        raw_verbs: a.raw_verbs,
        owner: hidden.0,
        // SAFETY: no arguments.
        op_thread: unsafe { GetCurrentThreadId() },
    };
    let res = match a.row.as_str() {
        "e" => row_e(&ctx),
        "f" => row_f(&ctx),
        "a" => row_a(&ctx),
        "b" => row_b(&ctx),
        "e2w" => row_e2w(&ctx),
        "g" => row_g(&ctx),
        "i" => row_i(&ctx),
        "c" => row_c(&ctx),
        _ => Err("unknown row".into()),
    };
    let rec = J::obj()
        .set("probe", "fs3-probe")
        .set("row", a.row.as_str())
        .set("system", system_facts())
        .set("ci", ci)
        .set("close_after_ms", ctx.close_after.as_millis() as u64);
    let rec = match res {
        Ok(r) => rec.set("result", r),
        Err(e) => rec.set("error", e),
    };
    Ok((json, rec))
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.first().map(String::as_str) == Some("b-ui") {
        std::process::exit(b_ui());
    }
    let a = match parse(&args) {
        Ok(a) => a,
        Err(e) => {
            eprintln!("fs3-probe: {e}\n{USAGE}");
            std::process::exit(2);
        }
    };
    match run(&a) {
        Ok((json, rec)) => {
            if let Err(e) = std::fs::write(&json, rec.render()) {
                eprintln!("fs3-probe: cannot write {}: {e}", json.display());
                std::process::exit(1);
            }
            let failed = rec.get("error").is_some();
            println!("fs3-probe {}: {}; record {}", a.row, if failed { "error" } else { "done" }, json.display());
            std::process::exit(i32::from(failed));
        }
        Err((code, e)) => {
            eprintln!("fs3-probe: {e}");
            std::process::exit(code);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn s(v: &[&str]) -> Vec<String> {
        v.iter().map(|x| x.to_string()).collect()
    }

    #[test]
    fn arguments_are_parsed_and_checked() {
        let a =
            parse(&s(&["e", "--root", r"C:\t\mod-files-x", "--json", r"C:\t\mod-files-x\e.json", "--keep"])).unwrap();
        assert_eq!(a.row, "e");
        assert!(a.keep && !a.no_silent && a.close_after.is_none());
        let b = parse(&s(&["b", "--root", "r", "--json", "j", "--no-silent", "--close-after-ms", "2000"])).unwrap();
        assert!(b.no_silent);
        assert_eq!(b.close_after, Some(2000));
        assert!(parse(&s(&["x", "--root", "r", "--json", "j"])).is_err(), "unknown row");
        assert!(parse(&s(&["e", "--json", "j"])).is_err(), "no root");
        assert!(parse(&s(&["e", "--root", "r"])).is_err(), "no record");
        assert!(parse(&s(&["e", "--root", "r", "--json", "j", "--close-after-ms", "soon"])).is_err());
        assert!(parse(&s(&["e", "--root", "r", "--json", "j", "--force"])).is_err(), "unknown option");
        assert!(parse(&s(&[])).is_err());
    }

    #[test]
    fn bin_names_are_masked_and_recognised() {
        let n = r"C:\$Recycle.Bin\S-1-5-21-1-2-3-1001\$RABC123.txt";
        assert_eq!(mask_bin_name(n), r"C:\$Recycle.Bin\<sid>\$RABC123.txt");
        assert!(is_bin_path(n, r"C:\", "txt"));
        assert!(is_bin_path(r"C:\$Recycle.Bin\S-1\$RABC123.txt", r"C:\", "txt"));
        assert!(!is_bin_path(n, r"D:\", "txt"), "another volume");
        assert!(!is_bin_path(n, r"C:\", "bin"), "another extension");
        assert!(!is_bin_path(r"C:\Temp\$Recycle.Bin.txt", r"C:\", "txt"));
    }

    #[test]
    fn only_a_recycled_file_is_a_bin_path() {
        // Never the bin, a user's folder of it, anything deeper, an $I record or a name that climbs out.
        for bad in [
            r"C:\$Recycle.Bin",
            r"C:\$Recycle.Bin\",
            r"C:\$Recycle.Bin\S-1-5-21-1",
            r"C:\$Recycle.Bin\S-1-5-21-1\",
            r"C:\$Recycle.Bin\S-1\sub\$Rx.txt",
            r"C:\$Recycle.Bin\S-1\$Ix.txt",
            r"C:\$Recycle.Bin\S-1\$R.txt",
            r"C:\$Recycle.Bin\\$Rx.txt",
            r"C:\$Recycle.Bin\..\$Rx.txt",
            r"C:\$Recycle.Bin\S-1\$Rx.txt:s.txt",
            r"C:\$Recycle.Bin\S-1/$Rx.txt",
        ] {
            assert!(!is_bin_path(bad, r"C:\", "txt"), "{bad}");
        }
    }

    #[test]
    fn only_a_plain_file_of_the_length_is_cleaned() {
        let dir = std::env::temp_dir().join(format!("mod-files-bin-check-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let (file, other, sub) = (dir.join("$Rx.txt"), dir.join("$Ry.txt"), dir.join("$Rz.txt"));
        std::fs::write(&file, G_BYTES).unwrap();
        std::fs::write(&other, b"other bytes").unwrap();
        std::fs::create_dir_all(&sub).unwrap();
        let name = |p: &Path| p.to_string_lossy().into_owned();
        let len = G_BYTES.len() as u64;
        let seen = [is_bin_file(&name(&file), len), is_bin_file(&name(&other), len), is_bin_file(&name(&sub), len)];
        let missing = is_bin_file(&name(&dir.join("$Rw.txt")), len);
        std::fs::remove_dir_all(&dir).unwrap();
        assert_eq!(seen, [true, false, false], "the file, another length, a folder");
        assert!(!missing);
        assert_eq!(len, 24);
    }

    #[test]
    fn row_c_needs_the_disposable_runner() {
        // The variables are never all set on a developer machine; on the runner the job sets them for the probe only.
        if std::env::var("FILES_DISPOSABLE").is_err() {
            assert!(disposable().is_err());
        }
    }

    #[test]
    fn menus_ask_for_rename() {
        assert_ne!(MENU_FLAGS & CMF_CANRENAME, 0, "without it the default menu has no Rename item");
        assert_eq!(MENU_FLAGS & CMF_EXTENDEDVERBS, 0, "the first pass is the normal menu");
    }

    #[test]
    fn silent_flags_never_include_undo() {
        let f = silent();
        assert_eq!(f.0 & FOF_ALLOWUNDO.0, 0, "rows e and f never recycle");
        assert_ne!(f.0 & FOF_SILENT.0, 0);
        assert_ne!(f.0 & FOF_NOERRORUI.0, 0);
    }
}
