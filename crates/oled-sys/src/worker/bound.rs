//! Bound workers: a kill-on-close job child with frames on its stdio (F-P4).
//!
//! The dock's child spawner (moved from the dock's update.rs by WK1): `spawn_in_job` starts a child with no window,
//! suspended until it is in the process's kill-on-close job, inheriting only its pipe ends; `run_capture` runs one to
//! its end and keeps its stdout; `command_line` quotes its arguments; `trusted_exe` decides whether an elevated
//! process may start an exe again, and `held_start_path` gives the path that starts a file held open. `set_on_thread`
//! lets the host count the threads this module starts.

use std::ffi::OsString;
use std::fs::File;
use std::io::{PipeReader, PipeWriter, Read};
use std::os::windows::ffi::{OsStrExt, OsStringExt};
use std::os::windows::fs::OpenOptionsExt;
use std::os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle, RawHandle};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, RecvTimeoutError};
use std::sync::{Mutex, OnceLock, PoisonError};
use std::time::{Duration, Instant};

use windows::core::{PCWSTR, PWSTR};
use windows::Win32::Foundation::{SetHandleInformation, HANDLE, HANDLE_FLAG_INHERIT, WAIT_OBJECT_0, WAIT_TIMEOUT};
use windows::Win32::Storage::FileSystem::{
    GetFinalPathNameByHandleW, FILE_NAME_NORMALIZED, FILE_SHARE_READ, GETFINALPATHNAMEBYHANDLE_FLAGS, VOLUME_NAME_DOS,
    VOLUME_NAME_GUID,
};
use windows::Win32::System::JobObjects::{
    AssignProcessToJobObject, CreateJobObjectW, JobObjectExtendedLimitInformation, SetInformationJobObject,
    JOBOBJECT_EXTENDED_LIMIT_INFORMATION, JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
};
use windows::Win32::System::Threading::{
    CreateProcessW, DeleteProcThreadAttributeList, GetExitCodeProcess, InitializeProcThreadAttributeList, ResumeThread,
    TerminateProcess, UpdateProcThreadAttribute, WaitForSingleObject, CREATE_NO_WINDOW, CREATE_SUSPENDED,
    EXTENDED_STARTUPINFO_PRESENT, INFINITE, LPPROC_THREAD_ATTRIBUTE_LIST, PROCESS_INFORMATION,
    PROC_THREAD_ATTRIBUTE_HANDLE_LIST, STARTF_FORCEOFFFEEDBACK, STARTF_USESTDHANDLES, STARTUPINFOEXW, STARTUPINFOW,
};

/// The host's hook for the threads this module starts (`set_on_thread`); unset, nothing is called.
static ON_THREAD: OnceLock<fn()> = OnceLock::new();

/// Once per process: called first thing in every thread bound starts (the dock's wake-probe counter). False when a
/// hook was already set (the first one stays).
pub fn set_on_thread(f: fn()) -> bool {
    ON_THREAD.set(f).is_ok()
}

/// The host's hook, first thing in a thread this crate starts for a worker (`worker-rx`).
pub(crate) fn thread_started() {
    if let Some(on_thread) = ON_THREAD.get() {
        on_thread();
    }
}

/// How often `run_capture` looks at its child.
const CAPTURE_POLL: Duration = Duration::from_millis(50);
/// How long `run_capture` waits for the end of a child's output once the child has exited.
const READER_GRACE: Duration = Duration::from_secs(2);

/// Runs `exe args` without a window, through `spawn_in_job`, and returns its exit code and stdout. The
/// output is read on a short-lived `capture` thread while this one waits, so a child that writes more
/// than a pipe buffer never blocks; at most `cap` bytes are kept. Every failure is an `Err`, with the
/// child killed and reaped when it still runs: output over `cap` ("output larger than <cap> bytes"), no
/// exit within `timeout` ("timeout"), an output still open 2 s after the exit. stdin and stderr are
/// null: nothing is ever written to a child, so it can never block this thread on its input.
#[deny(clippy::unwrap_used, clippy::expect_used, clippy::indexing_slicing)]
pub fn run_capture(exe: &Path, args: &[&str], timeout: Duration, cap: u64) -> Result<(i32, Vec<u8>), String> {
    let mut child = spawn_in_job(exe, args, false).map_err(|e| format!("cannot start {}: {e}", exe.display()))?;
    let Some(stdout) = child.stdout.take() else {
        return Err(stop(&mut child, "no output pipe".into()));
    };
    let (tx, rx) = mpsc::channel();
    let reader = std::thread::Builder::new().name("capture".into()).stack_size(64 * 1024).spawn(move || {
        if let Some(on_thread) = ON_THREAD.get() {
            on_thread();
        }
        let mut out = Vec::new();
        // One byte over the cap tells "too large" from "exactly the cap".
        let read = stdout.take(cap.saturating_add(1)).read_to_end(&mut out).map(|_| out);
        let _ = tx.send(read);
    });
    if let Err(e) = reader {
        return Err(stop(&mut child, format!("cannot start the reader thread: {e}")));
    }
    let started = Instant::now();
    let mut output: Option<std::io::Result<Vec<u8>>> = None;
    let code = loop {
        match &output {
            Some(Ok(out)) if out.len() as u64 > cap => {
                return Err(stop(&mut child, format!("output larger than {cap} bytes")))
            }
            Some(Err(e)) => return Err(stop(&mut child, format!("reading the output of {}: {e}", exe.display()))),
            _ => {}
        }
        match child.try_wait() {
            Ok(Some(code)) => break code,
            Ok(None) if started.elapsed() > timeout => return Err(stop(&mut child, "timeout".into())),
            Ok(None) => {}
            Err(e) => return Err(stop(&mut child, format!("waiting for {} failed: {e}", exe.display()))),
        }
        if output.is_some() {
            std::thread::sleep(CAPTURE_POLL);
            continue;
        }
        match rx.recv_timeout(CAPTURE_POLL) {
            Ok(read) => output = Some(read),
            Err(RecvTimeoutError::Timeout) => {}
            Err(RecvTimeoutError::Disconnected) => output = Some(Err(std::io::Error::other("the reader thread ended"))),
        }
    };
    let output = match output {
        Some(read) => read,
        // Only something the child started can still hold the pipe; the reader is left to it.
        None => rx.recv_timeout(READER_GRACE).map_err(|_| {
            format!("the output of {} was still open {} s after it exited", exe.display(), READER_GRACE.as_secs())
        })?,
    };
    let out = output.map_err(|e| format!("reading the output of {}: {e}", exe.display()))?;
    if out.len() as u64 > cap {
        return Err(format!("output larger than {cap} bytes"));
    }
    Ok((code, out))
}

/// Kills and reaps `child` (harmless when it has already exited) and hands `why` back.
fn stop(child: &mut JobChild, why: String) -> String {
    let _ = child.kill();
    let _ = child.wait();
    why
}

/// The dock's job object (house rule 7 of plan L3): every child `spawn_in_job` starts joins it, so none
/// outlives the dock, however it ends (a crash, the watchdog's restart, the installer's stop), and none
/// holds the exe while an update replaces it. Created at the first spawn with
/// `JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE` and no security attributes, so its handle is not inherited and
/// only the dock holds it; kept until the process ends, when Windows closes it and kills what is left.
/// `None` = it could not be created (logged once): children then run outside it, as before.
static JOB: OnceLock<Option<OwnedHandle>> = OnceLock::new();

fn create_job() -> Option<OwnedHandle> {
    // SAFETY: no attributes and no name; the handle returned is ours to own.
    let job = match unsafe { CreateJobObjectW(None, PCWSTR::null()) } {
        // SAFETY: a fresh handle, owned by nothing else.
        Ok(h) => unsafe { OwnedHandle::from_raw_handle(h.0) },
        Err(e) => {
            crate::log!("children: cannot create the dock's job ({e}); child processes run outside it");
            return None;
        }
    };
    let mut info = JOBOBJECT_EXTENDED_LIMIT_INFORMATION::default();
    info.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
    let size = std::mem::size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32;
    // SAFETY: `job` is a valid job handle; `info` is readable and `size` bytes long.
    let set = unsafe {
        SetInformationJobObject(
            HANDLE(job.as_raw_handle()),
            JobObjectExtendedLimitInformation,
            std::ptr::from_ref(&info).cast(),
            size,
        )
    };
    if let Err(e) = set {
        crate::log!("children: cannot set the dock's job to kill on close ({e}); child processes run outside it");
        return None;
    }
    Some(job)
}

/// The dock's job, once a child has been started through `spawn_in_job` and the job could be made.
#[cfg(test)]
fn dock_job() -> Option<HANDLE> {
    JOB.get().and_then(Option::as_ref).map(|j| HANDLE(j.as_raw_handle()))
}

/// A child of `spawn_in_job`: its process and the dock's ends of its pipes (stdout always, stdin when asked for),
/// with the three calls the dock makes on it. Dropping it closes the handles and leaves the child running, as with
/// `std::process::Child`; the dock's job still ends it with the dock.
pub struct JobChild {
    process: OwnedHandle,
    pub stdin: Option<PipeWriter>,
    pub stdout: Option<PipeReader>,
}

impl JobChild {
    /// The exit code once the child has ended, None while it runs.
    pub fn try_wait(&mut self) -> std::io::Result<Option<i32>> {
        self.wait_ms(0)
    }

    /// Waits for the end of the child: its exit code.
    pub fn wait(&mut self) -> std::io::Result<i32> {
        self.wait_ms(INFINITE)?.ok_or_else(|| std::io::Error::other("the process did not end"))
    }

    /// Ends the child with exit code 1, as `std::process::Child::kill` does; Ok when it had already ended.
    pub fn kill(&mut self) -> std::io::Result<()> {
        // SAFETY: the process handle is valid while `self` lives.
        match unsafe { TerminateProcess(self.handle(), 1) } {
            Ok(()) => Ok(()),
            // An ended process refuses TerminateProcess (access denied).
            Err(_) if matches!(self.wait_ms(0), Ok(Some(_))) => Ok(()),
            Err(e) => Err(e.into()),
        }
    }

    fn handle(&self) -> HANDLE {
        HANDLE(self.process.as_raw_handle())
    }

    fn wait_ms(&self, ms: u32) -> std::io::Result<Option<i32>> {
        // SAFETY: the process handle is valid while `self` lives; waiting on it has no other effect.
        let waited = unsafe { WaitForSingleObject(self.handle(), ms) };
        if waited == WAIT_TIMEOUT {
            return Ok(None);
        }
        if waited != WAIT_OBJECT_0 {
            return Err(std::io::Error::last_os_error());
        }
        let mut code = 0u32;
        // SAFETY: as above; `code` is writable.
        unsafe { GetExitCodeProcess(self.handle(), &mut code) }?;
        // The code as `std::process::ExitStatus::code` reads it.
        Ok(Some(code as i32))
    }
}

impl AsRawHandle for JobChild {
    fn as_raw_handle(&self) -> RawHandle {
        self.process.as_raw_handle()
    }
}

/// `child_exe`'s decision on `exe`, and the path the caller starts: unelevated, `exe`, any file (a child gains nothing
/// the user lacks); elevated, only the installed copy (`installed`, the same file), started by `installed` itself, or an
/// exe on a local drive whose resolved folder `admin_dir` accepts, started by its resolved path (`admin_exe`). Never
/// `exe` as given when elevated: a junction its user owns in that path could point at the checked file for the check
/// and at another for the start. The resolved path is the final path of the file held open (`pin`) from before its
/// folder is asked until the process ends: a user who can change a folder above an admin-only one could otherwise
/// rename it between the check and the start and put another tree in its place.
pub fn trusted_exe(
    exe: PathBuf,
    elevated: bool,
    installed: Option<&Path>,
    admin_dir: impl FnOnce(&Path) -> bool,
) -> Result<PathBuf, String> {
    if !elevated {
        return Ok(exe);
    }
    if let Some(i) = installed.filter(|i| crate::ident::same_file(&exe, i)) {
        return Ok(i.to_path_buf());
    }
    let pin = pin(&exe);
    let started = admin_exe(pin.as_ref().and_then(held_start_path), admin_dir)?;
    let mut held = PINNED.lock().unwrap_or_else(PoisonError::into_inner);
    if let Some(f) = pin.filter(|_| !held.iter().any(|(p, _)| *p == started)) {
        held.push((started.clone(), f));
    }
    Ok(started)
}

/// The exes `trusted_exe` accepted from an admin-only folder, by the path it gave, each held open (`pin`) until the
/// process ends: one handle per path, however often it is asked.
static PINNED: Mutex<Vec<(PathBuf, File)>> = Mutex::new(Vec::new());

/// `exe` opened for reading with every link followed, shared for reading only: while the handle is open nobody can
/// write, rename or delete the file, nor rename a folder above it on its volume, so its final path keeps naming it. A
/// running image pins nothing: the handle it was started by is closed once its process runs.
fn pin(exe: &Path) -> Option<File> {
    std::fs::OpenOptions::new().read(true).share_mode(FILE_SHARE_READ.0).open(exe).ok()
}

/// The path that starts the very file `f` holds while it is held: `start_path` of its final paths, by drive and by
/// volume; None when it has none.
pub fn held_start_path(f: &File) -> Option<PathBuf> {
    start_path(&final_path(f, VOLUME_NAME_DOS)?, final_path(f, VOLUME_NAME_GUID).as_deref())
}

/// The final path of the file `f` holds, normalized (`GetFinalPathNameByHandleW`), with `volume`'s prefix: `\\?\` and a
/// drive or `UNC\` (`VOLUME_NAME_DOS`, as `std::fs::canonicalize` gives it) or `\\?\Volume{...}` (`VOLUME_NAME_GUID`);
/// None when it cannot be had.
fn final_path(f: &File, volume: GETFINALPATHNAMEBYHANDLE_FLAGS) -> Option<String> {
    let flags = GETFINALPATHNAMEBYHANDLE_FLAGS(FILE_NAME_NORMALIZED.0 | volume.0);
    let mut buf = vec![0u16; 512];
    for _ in 0..2 {
        // SAFETY: the handle is valid while `f` lives; `buf` is writable for its whole length.
        let n = unsafe { GetFinalPathNameByHandleW(HANDLE(f.as_raw_handle()), &mut buf, flags) } as usize;
        if n == 0 {
            return None;
        }
        if n < buf.len() {
            return buf.get(..n).and_then(|p| OsString::from_wide(p).into_string().ok());
        }
        // Too small: `n` is the length it needs, its NUL included.
        buf.resize(n, 0);
    }
    None
}

/// Lets a test delete the folders of an exe `trusted_exe` accepted.
#[cfg(test)]
fn unpin(started: &Path) {
    PINNED.lock().unwrap_or_else(PoisonError::into_inner).retain(|(p, _)| p != started);
}

/// The path an elevated child starts by when its exe is not the installed copy: `start`, the path that starts the exe
/// held open (`held_start_path`, every junction and link in it followed), when `admin_dir` accepts its folder. None is
/// refused before `admin_dir` reads a descriptor, as an exe on a share is: its owner and DACL are what its server
/// reports, and the Administrators and SYSTEM they name are the server's. Whoever the server lets write there (this
/// user's unelevated processes too, when the account administers the server) could swap the exe between the check and
/// the start.
fn admin_exe(start: Option<PathBuf>, admin_dir: impl FnOnce(&Path) -> bool) -> Result<PathBuf, String> {
    match start {
        Some(p) if p.parent().is_some_and(admin_dir) => Ok(p),
        _ => Err("the exe is not in an admin-only folder".into()),
    }
}

/// The path a start takes to the file whose final paths are `dos` (`\\?\X:\...`) and `guid` (`\\?\Volume{...}\...`,
/// None when it has none): `dos` without its `\\?\` (the shell's launch does not take one), when it names that file
/// and nothing else while the file is held. A held file pins the folders on its own volume only, and the DOS path of a
/// volume with no drive letter goes through its mount folder on another (`\\?\C:\mnt\...`), which whoever can change
/// it (a folder an admin makes at the root of a drive inherits Modify for every account) could rename and fill with
/// another tree: the tail after the drive must be the one after the volume. A path the Win32 parser would change
/// (`parsed_as_is`) names another file without its prefix. A share (`\\?\UNC\`, a mapped drive's too) has no volume
/// path, and its folders are what its server reports. Any other form is None too. Pure.
fn start_path(dos: &str, guid: Option<&str>) -> Option<PathBuf> {
    let drive = dos.strip_prefix(r"\\?\").filter(|d| d.as_bytes().get(1) == Some(&b':'))?;
    let tail = guid?.strip_prefix(r"\\?\Volume{")?.split_once('}')?.1;
    (drive.get(2..) == Some(tail) && parsed_as_is(drive)).then(|| PathBuf::from(drive))
}

/// True when no name in `path` ends in '.' or ' '. The Win32 parser (`GetFullPathNameW`), which every start and every
/// open of a path without `\\?\` goes through, drops a '.' at the end of a folder's name and every '.' and ' ' at the
/// end of the file's: such a path, a final path without its prefix, names another file. Pure.
fn parsed_as_is(path: &str) -> bool {
    path.split('\\').all(|name| !name.ends_with(['.', ' ']))
}

/// Starts `"<exe>" args` with the dock's own token and no window, and puts it into the dock's job (created at the
/// first call): stdout piped, stdin piped only when `stdin` (else null, as stderr). For short-lived children whose
/// work belongs to the dock (`run_capture`'s, the icon child); never for the watchdog, an update's installer or
/// anything the user launches, which must outlive the dock. The child is created suspended and runs only once it is
/// in the job; a failed assignment is logged once and the child runs outside the job. `STARTF_FORCEOFFFEEDBACK`: a
/// GUI program (dock.exe) started by a GUI program gets Windows' "working in background" pointer until it reads its
/// first message, which no child does, so without it the pointer would spin for as long as a child runs (up to
/// 7 s). The child inherits its pipe ends and nothing else (`PROC_THREAD_ATTRIBUTE_HANDLE_LIST`).
#[deny(clippy::unwrap_used, clippy::expect_used, clippy::indexing_slicing)]
pub fn spawn_in_job(exe: &Path, args: &[&str], stdin: bool) -> std::io::Result<JobChild> {
    let mut line = command_line(exe, args)?;
    let app: Vec<u16> = exe.as_os_str().encode_wide().chain(std::iter::once(0)).collect();
    // `std::io::pipe` makes both ends non-inheritable; only the child's end is made inheritable, for this spawn.
    let (out_read, out_write) = std::io::pipe()?;
    let input = if stdin { Some(std::io::pipe()?) } else { None };
    let child_out = HANDLE(out_write.as_raw_handle());
    let child_in = input.as_ref().map_or(HANDLE::default(), |(read, _)| HANDLE(read.as_raw_handle()));
    let ends = [child_out, child_in];
    let inherited = ends.get(..if stdin { 2 } else { 1 }).unwrap_or_default();
    for &end in inherited {
        // SAFETY: a pipe handle of this function's, open until its end.
        unsafe { SetHandleInformation(end, HANDLE_FLAG_INHERIT.0, HANDLE_FLAG_INHERIT) }?;
    }
    let mut attrs = HandleList::new(inherited)?;
    let si = STARTUPINFOEXW {
        StartupInfo: STARTUPINFOW {
            cb: std::mem::size_of::<STARTUPINFOEXW>() as u32,
            dwFlags: STARTF_USESTDHANDLES | STARTF_FORCEOFFFEEDBACK,
            hStdInput: child_in,
            hStdOutput: child_out,
            ..Default::default()
        },
        lpAttributeList: attrs.list(),
    };
    let mut pi = PROCESS_INFORMATION::default();
    // SAFETY: `app` and `line` are NUL-terminated and live for the call (`line` mutable, as CreateProcessW wants);
    // `si` and its attribute list, which points at `inherited`, live until after it; `pi` is writable.
    let created = unsafe {
        CreateProcessW(
            PCWSTR(app.as_ptr()),
            Some(PWSTR(line.as_mut_ptr())),
            None,
            None,
            true,
            CREATE_NO_WINDOW | CREATE_SUSPENDED | EXTENDED_STARTUPINFO_PRESENT,
            None,
            PCWSTR::null(),
            std::ptr::from_ref(&si).cast(),
            &mut pi,
        )
    };
    drop(attrs);
    // The child holds its own copies of its ends: the dock's close here, so the output ends when the child exits.
    drop(out_write);
    let input = input.map(|(_, write)| write);
    created?;
    // SAFETY: both handles were just returned by CreateProcessW and are owned here, each once.
    let (process, thread) =
        unsafe { (OwnedHandle::from_raw_handle(pi.hProcess.0), OwnedHandle::from_raw_handle(pi.hThread.0)) };
    let mut child = JobChild { process, stdin: input, stdout: Some(out_read) };
    if let Some(job) = JOB.get_or_init(create_job) {
        // SAFETY: the job's handle is valid until the process ends, the child's while `child` lives.
        let joined = unsafe { AssignProcessToJobObject(HANDLE(job.as_raw_handle()), child.handle()) };
        if let Err(e) = joined {
            static WARNED: AtomicBool = AtomicBool::new(false);
            if !WARNED.swap(true, Ordering::Relaxed) {
                crate::log!("children: a child process could not join the dock's job ({e}); it runs outside it");
            }
        }
    }
    // SAFETY: the handle of the child's only thread, suspended since its creation.
    if unsafe { ResumeThread(HANDLE(thread.as_raw_handle())) } == u32::MAX {
        let e = std::io::Error::last_os_error();
        let _ = child.kill();
        let _ = child.wait();
        return Err(e);
    }
    Ok(child)
}

/// The attribute list of one `spawn_in_job`: the handles its child may inherit, and nothing else
/// (`PROC_THREAD_ATTRIBUTE_HANDLE_LIST`). The list points at `handles`, so it borrows them; deleted at drop.
struct HandleList<'a> {
    buf: Vec<usize>,
    _handles: std::marker::PhantomData<&'a [HANDLE]>,
}

impl<'a> HandleList<'a> {
    fn new(handles: &'a [HANDLE]) -> std::io::Result<Self> {
        let mut size = 0usize;
        // SAFETY: a size query (no list): it fails with ERROR_INSUFFICIENT_BUFFER and writes the size it needs.
        let _ = unsafe { InitializeProcThreadAttributeList(None, 1, None, &mut size) };
        let mut buf = vec![0usize; size.div_ceil(std::mem::size_of::<usize>())];
        let list = LPPROC_THREAD_ATTRIBUTE_LIST(buf.as_mut_ptr().cast());
        // SAFETY: `buf` holds at least `size` bytes, aligned for the list; its heap block moves with it, unchanged.
        unsafe { InitializeProcThreadAttributeList(Some(list), 1, None, &mut size) }?;
        let this = HandleList { buf, _handles: std::marker::PhantomData };
        // SAFETY: the list was just initialized; `handles` outlives it (the borrow), as the attribute requires.
        unsafe {
            UpdateProcThreadAttribute(
                list,
                0,
                PROC_THREAD_ATTRIBUTE_HANDLE_LIST as usize,
                Some(handles.as_ptr().cast()),
                std::mem::size_of_val(handles),
                None,
                None,
            )
        }?;
        Ok(this)
    }

    fn list(&mut self) -> LPPROC_THREAD_ATTRIBUTE_LIST {
        LPPROC_THREAD_ATTRIBUTE_LIST(self.buf.as_mut_ptr().cast())
    }
}

impl Drop for HandleList<'_> {
    fn drop(&mut self) {
        // SAFETY: initialized in `new`, deleted once, here.
        unsafe { DeleteProcThreadAttributeList(self.list()) }
    }
}

/// The NUL-terminated command line `spawn_in_job` gives `exe`, written as `std::process::Command` writes it: the
/// program in quotes, then each argument as the C runtime reads it back (in quotes when empty or holding a space or
/// a tab; a quote escaped; backslashes doubled before a quote and before a closing quote). A NUL anywhere, or a quote
/// in the program's path, is refused.
pub fn command_line(exe: &Path, args: &[&str]) -> std::io::Result<Vec<u16>> {
    const QUOTE: u16 = b'"' as u16;
    const BACKSLASH: u16 = b'\\' as u16;
    let mut line = vec![QUOTE];
    line.extend(exe.as_os_str().encode_wide());
    let bad_path = line.contains(&0) || line.get(1..).is_some_and(|path| path.contains(&QUOTE));
    line.push(QUOTE);
    for arg in args {
        line.push(u16::from(b' '));
        let quoted = arg.is_empty() || arg.contains([' ', '\t']);
        if quoted {
            line.push(QUOTE);
        }
        let mut backslashes = 0;
        for c in arg.encode_utf16() {
            if c == BACKSLASH {
                backslashes += 1;
            } else {
                if c == QUOTE {
                    line.extend(std::iter::repeat_n(BACKSLASH, backslashes + 1));
                }
                backslashes = 0;
            }
            line.push(c);
        }
        if quoted {
            line.extend(std::iter::repeat_n(BACKSLASH, backslashes));
            line.push(QUOTE);
        }
    }
    if bad_path || line.contains(&0) {
        return Err(std::io::Error::new(std::io::ErrorKind::InvalidInput, "a NUL or a quote in the command line"));
    }
    line.push(0);
    Ok(line)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn system32(exe: &str) -> PathBuf {
        PathBuf::from(std::env::var_os("SystemRoot").unwrap()).join("System32").join(exe)
    }

    /// L3 review: an elevated dock starts itself again only from a file no non-admin can swap (the running exe can be
    /// renamed and another put at its path, which `current_exe` still names): the installed copy, or an exe in a folder
    /// `datadir` accepts as admin-only. A temporary folder is the user's: refused when elevated, fine when not. SEC-4
    /// review: elevated, the path to start is never the one given but the installed path or the exe resolved, so a
    /// junction the user owns in the given path cannot point at the checked file for the check and elsewhere for the
    /// start.
    #[test]
    fn children_run_only_an_exe_no_user_can_swap() {
        use std::os::windows::process::CommandExt;
        // A temporary folder with no link and no short name in its path: the resolved spelling of its files.
        let tmp = std::fs::canonicalize(std::env::temp_dir()).unwrap();
        let base = PathBuf::from(tmp.to_str().unwrap().trim_start_matches(r"\\?\"))
            .join(format!("dock_child_exe_test_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        let dir = base.join("real");
        std::fs::create_dir_all(&dir).unwrap();
        let exe = dir.join("dock.exe");
        std::fs::write(&exe, b"MZ").unwrap();
        let link = base.join("link");
        let made = std::process::Command::new(system32("cmd.exe"))
            .creation_flags(CREATE_NO_WINDOW.0)
            .args(["/C", "mklink", "/J"])
            .arg(&link)
            .arg(&dir)
            .output()
            .is_ok_and(|o| o.status.success());
        let (by_link, spelled) = (link.join("dock.exe"), PathBuf::from(exe.to_string_lossy().to_uppercase()));
        let refused = Err("the exe is not in an admin-only folder".to_string());
        let user_dir = trusted_exe(exe.clone(), true, None, crate::datadir::is_admin_only_dir);
        let unelevated = trusted_exe(exe.clone(), false, None, |_| false);
        let installed = trusted_exe(exe.clone(), true, Some(&exe), |_| false);
        let installed_by_link = trusted_exe(by_link.clone(), true, Some(&exe), |_| false);
        let installed_spelled = trusted_exe(spelled, true, Some(&exe), |_| false);
        let other = trusted_exe(exe.clone(), true, Some(&dir.join("other.exe")), |_| false);
        let admin_dir = trusted_exe(exe.clone(), true, None, |d| d == dir);
        let admin_dir_by_link = trusted_exe(by_link, true, None, |d| d == dir);
        unpin(&exe);
        let _ = std::fs::remove_dir(&link);
        let _ = std::fs::remove_dir_all(&base);
        assert!(made, "the junction could be made");
        assert_eq!(user_dir, refused, "elevated, in a folder the user can write");
        assert_eq!(unelevated, Ok(exe.clone()), "unelevated: a child gains nothing the user lacks");
        assert_eq!(installed, Ok(exe.clone()), "the installed copy");
        assert_eq!(installed_by_link, Ok(exe.clone()), "the installed copy by its own path, not through the junction");
        assert_eq!(installed_spelled, Ok(exe.clone()), "the installed copy by its own path, not another spelling");
        assert_eq!(other, refused, "another file than the installed copy");
        assert_eq!(admin_dir, Ok(exe.clone()), "a folder datadir accepts");
        assert_eq!(admin_dir_by_link, Ok(exe), "the folder checked and the path started are the junction's target");
    }

    /// A share's owner and DACL are what its server reports, and the Administrators and SYSTEM they name are the
    /// server's: an elevated dock never starts a child from one (`\\?\UNC\`, where a mapped drive resolves too), and
    /// the folder's descriptor is not even read. A local drive path is asked as before and started without `\\?\`.
    #[test]
    fn an_exe_on_a_share_is_refused_before_its_folder_is_read() {
        let refused = Err("the exe is not in an admin-only folder".to_string());
        let mut asked = Vec::new();
        let share = admin_exe(start_path(r"\\?\UNC\server\share\dock\dock.exe", None), |d| {
            asked.push(d.to_path_buf());
            true
        });
        assert_eq!(share, refused, "an exe on a share");
        assert!(asked.is_empty(), "the descriptor of a share's folder is never read: {asked:?}");
        let local = admin_exe(start_path(r"\\?\C:\dock\dock.exe", Some(r"\\?\Volume{v}\dock\dock.exe")), |d| {
            asked.push(d.to_path_buf());
            true
        });
        assert_eq!(local, Ok(PathBuf::from(r"C:\dock\dock.exe")), "a local drive path, started without its prefix");
        assert_eq!(asked, [PathBuf::from(r"C:\dock")], "its folder is asked");
        assert_eq!(admin_exe(None, |_| true), refused, "an exe that cannot be resolved");
    }

    /// SEC review: a start (`CreateProcessW`) and `admin_dir`'s open read a path without `\\?\` through the Win32
    /// parser (`GetFullPathNameW`), which drops a '.' at the end of a folder's name and every '.' and ' ' at the end of
    /// the file's. `C:\Tools.\dock\dock.exe`, the final path of a file held in the folder `Tools.`, would be read as
    /// `C:\Tools\dock\dock.exe`, through a junction `Tools` its user can point at the held file for the check and at
    /// their own tree for the start. A drive path with a name that ends in '.' or ' ' is refused before its folder is
    /// read.
    #[test]
    fn a_path_the_parser_changes_is_refused_before_its_folder_is_read() {
        let refused = Err("the exe is not in an admin-only folder".to_string());
        let mut asked = Vec::new();
        for real in [r"\\?\C:\Tools.\dock\dock.exe", r"\\?\C:\dock\dock.exe.", r"\\?\C:\dock\dock.exe "] {
            let drive = real.strip_prefix(r"\\?\").unwrap();
            assert_ne!(std::path::absolute(drive).unwrap(), Path::new(drive), "the parser changes {drive}");
            let got = admin_exe(start_path(real, Some(&by_volume(real))), |d| {
                asked.push(d.to_path_buf());
                true
            });
            assert_eq!(got, refused, "{real}");
        }
        assert!(asked.is_empty(), "the folder of a path the parser changes is never read: {asked:?}");
        let plain = r"C:\Tools\dock\dock.exe";
        assert_eq!(std::path::absolute(plain).unwrap(), Path::new(plain), "the parser keeps {plain}");
        let dos = format!(r"\\?\{plain}");
        assert_eq!(admin_exe(start_path(&dos, Some(&by_volume(&dos))), |_| true), Ok(PathBuf::from(plain)));
    }

    /// The final path by volume of a file on a drive's own volume whose final path by drive is `dos` (`\\?\X:\...`).
    fn by_volume(dos: &str) -> String {
        format!(r"\\?\Volume{{v}}{}", dos.get(6..).unwrap())
    }

    /// SEC review: a held exe pins the folders on its own volume only. The DOS path of a volume with no drive letter
    /// goes through its mount folder on another (`\\?\C:\mnt\dock\dock.exe`): whoever can change that folder (one an
    /// admin makes at the root of a drive inherits Modify for every account) could rename it while the exe is held and
    /// put another tree with another exe at its path, which the start would run. The DOS path is started only when its
    /// tail after the drive is the one after the volume (`VOLUME_NAME_GUID`); otherwise, and with no path by volume (a
    /// share), the exe is refused before its folder is read. The tests above hold real files: their final paths agree.
    #[test]
    fn an_exe_through_a_mount_folder_is_refused_before_its_folder_is_read() {
        let mut asked = Vec::new();
        let mut ask = |start: Option<PathBuf>| {
            admin_exe(start, |d| {
                asked.push(d.to_path_buf());
                true
            })
        };
        let refused = Err("the exe is not in an admin-only folder".to_string());
        let mount = start_path(r"\\?\C:\mnt\dock\dock.exe", Some(r"\\?\Volume{v}\dock\dock.exe"));
        assert_eq!(ask(mount), refused, "a volume reached through its mount folder");
        assert_eq!(ask(start_path(r"\\?\C:\dock\dock.exe", None)), refused, "no path by volume");
        let by_volume_only = start_path(r"\\?\Volume{v}\dock\dock.exe", Some(r"\\?\Volume{v}\dock\dock.exe"));
        assert_eq!(ask(by_volume_only), refused, "a volume with no drive letter and no mount folder");
        let own = start_path(r"\\?\C:\Tools\dock\dock.exe", Some(r"\\?\Volume{v}\Tools\dock\dock.exe"));
        assert_eq!(ask(own), Ok(PathBuf::from(r"C:\Tools\dock\dock.exe")), "a drive's own volume");
        assert_eq!(asked, [PathBuf::from(r"C:\Tools\dock")], "only the folder on the drive's own volume is asked");
    }

    /// SEC review: `held_start_path` asks for both final paths of the very file held; the tests above give `start_path`
    /// strings. A real file in a folder whose name ends in '.' (made through `\\?\`: the Win32 parser reads that path
    /// as the folder without the '.', where a junction could be) has no path to start it by; one in a plain sibling
    /// folder starts by its canonical path, its drive and volume paths read from the file system.
    #[test]
    fn a_held_file_the_parser_would_read_as_another_has_no_start_path() {
        let tmp = std::fs::canonicalize(std::env::temp_dir()).unwrap();
        let base = tmp.join(format!("dock_child_parsed_test_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        let (dotted, plain) = (base.join("dock.").join("dock.exe"), base.join("dock").join("dock.exe"));
        for exe in [&dotted, &plain] {
            std::fs::create_dir_all(exe.parent().unwrap()).unwrap();
            std::fs::write(exe, b"MZ").unwrap();
        }
        let held = [dotted.as_path(), plain.as_path()].map(pin);
        let both_held = held.iter().all(Option::is_some);
        let [dotted_start, plain_start] = held.each_ref().map(|f| f.as_ref().and_then(held_start_path));
        drop(held);
        let canonical = std::fs::canonicalize(&plain).unwrap();
        let _ = std::fs::remove_dir_all(&base);
        assert!(both_held, "both files are held");
        assert_eq!(dotted_start, None, "a folder whose name ends in '.'");
        let plain_path = PathBuf::from(canonical.to_str().unwrap().trim_start_matches(r"\\?\"));
        assert_eq!(plain_start, Some(plain_path), "a plain folder");
    }

    /// SEC review: elevated, the folder asked is the one that holds the file started. A running exe pins nothing (the
    /// folder of a running image, and every folder above it, can be renamed while it runs), so a user who can change a
    /// folder above an admin-only one could rename it between the check and the start and put another tree in its
    /// place. The exe accepted is held open, shared for reading only, from before its folder is asked until the process
    /// ends: no folder above it can be renamed, nor the file renamed or written. A refused exe is not held.
    #[test]
    fn the_exe_checked_is_the_exe_started() {
        let tmp = std::fs::canonicalize(std::env::temp_dir()).unwrap();
        let base = PathBuf::from(tmp.to_str().unwrap().trim_start_matches(r"\\?\"))
            .join(format!("dock_child_pin_test_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        // `admin` stands for an admin-only folder, `user` and `other` for folders above one that their user can change.
        let (dir, refused_dir) = (base.join("user").join("admin"), base.join("other").join("admin"));
        let (exe, refused_exe) = (dir.join("dock.exe"), refused_dir.join("dock.exe"));
        for d in [&dir, &refused_dir] {
            std::fs::create_dir_all(d).unwrap();
            std::fs::write(d.join("dock.exe"), b"MZ").unwrap();
        }
        // Renames `top` away and back: true when the rename went through.
        let swap = |top: &str| {
            let (from, to) = (base.join(top), base.join(format!("{top}.moved")));
            std::fs::rename(&from, &to).is_ok_and(|()| std::fs::rename(&to, &from).is_ok())
        };
        let mut swapped_in_check = None;
        let started = trusted_exe(exe.clone(), true, None, |d| {
            swapped_in_check = Some(swap("user"));
            d == dir
        });
        let swapped_after = swap("user");
        let refused = trusted_exe(refused_exe.clone(), true, None, |_| false);
        let refused_swapped = swap("other");
        // The exe a dock starts again is its own running image, which opens shared for reading only too.
        let own = trusted_exe(std::env::current_exe().unwrap(), true, None, |_| true);
        if let Ok(own) = &own {
            unpin(own);
        }
        unpin(&exe);
        let _ = std::fs::remove_dir_all(&base);
        assert_eq!(started, Ok(exe), "an exe in a folder admin_dir accepts");
        assert_eq!(swapped_in_check, Some(false), "a folder above was renamed while the exe's folder was asked");
        assert!(!swapped_after, "a folder above was renamed between the check and the start");
        assert_eq!(refused, Err("the exe is not in an admin-only folder".to_string()));
        assert!(refused_swapped, "a refused exe is not held");
        assert!(own.is_ok(), "the running image: {own:?}");
    }

    /// A child that writes more than a pipe buffer neither blocks nor loses a byte; one that writes more
    /// than the cap is stopped at the cap, not at the timeout (the error names the cap).
    #[test]
    fn run_capture_reads_more_than_a_pipe_buffer() {
        let dir = std::env::temp_dir().join(format!("dock_capture_test_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("big.txt");
        let line: &[u8] = b"0123456789 abcdefghijklmnopqrstuvwxyz ABCDEFGHIJKLMNOPQRSTUVWXYZ\r\n";
        let data: Vec<u8> = line.iter().copied().cycle().take(300_000).collect();
        std::fs::write(&path, &data).unwrap();
        let args = ["/c", "type", path.to_str().unwrap()];
        let whole = run_capture(&system32("cmd.exe"), &args, Duration::from_secs(60), 1024 * 1024);
        let capped = run_capture(&system32("cmd.exe"), &args, Duration::from_secs(60), 100_000);
        let _ = std::fs::remove_dir_all(&dir);
        let (code, out) = whole.unwrap();
        assert_eq!(code, 0);
        assert_eq!(out.len(), data.len());
        assert!(out == data, "the output differs from the file");
        assert_eq!(capped.err().as_deref(), Some("output larger than 100000 bytes"));
    }

    /// A child inherits its pipe ends and nothing else: an inheritable handle of the dock (here the write end of a
    /// pipe) stays out of it, so that pipe ends when the dock closes its end, not when the child exits.
    #[test]
    fn children_inherit_only_their_pipes() {
        let (mut read, write) = std::io::pipe().unwrap();
        // SAFETY: a handle of this test's, open until `write` is dropped.
        unsafe { SetHandleInformation(HANDLE(write.as_raw_handle()), HANDLE_FLAG_INHERIT.0, HANDLE_FLAG_INHERIT) }
            .unwrap();
        // About 5 s of ping: a write end it inherited would hold the pipe open that long.
        let mut child = spawn_in_job(&system32("ping.exe"), &["-n", "6", "127.0.0.1"], false).unwrap();
        drop(write);
        let t = Instant::now();
        let mut rest = Vec::new();
        read.read_to_end(&mut rest).unwrap();
        let open = t.elapsed();
        let _ = child.kill();
        let _ = child.wait();
        assert!(open < Duration::from_secs(2), "the pipe stayed open {open:?}: the child inherited its write end");
    }

    /// What `spawn_in_job` writes on the command line, the child reads back as the same arguments (Windows' parser).
    #[test]
    fn command_line_round_trip() {
        use windows::Win32::Foundation::{LocalFree, HLOCAL};
        use windows::Win32::UI::Shell::CommandLineToArgvW;
        let exe = Path::new(r"C:\Program Files\mini dock\dock.exe");
        let args = [
            "--start-icons",
            "32",
            "",
            "a b",
            "tab\there",
            "q\"uote",
            "\"",
            "end\\",
            "end space\\",
            "two\\\\\"q",
            r"\\srv\a b\",
        ];
        let line = command_line(exe, &args).unwrap();
        assert_eq!(line.last(), Some(&0));
        let mut n = 0;
        // SAFETY: `line` is NUL-terminated; the array returned holds `n` strings, read once, then freed once.
        let parsed: Vec<String> = unsafe {
            let argv = CommandLineToArgvW(PCWSTR(line.as_ptr()), &mut n);
            assert!(!argv.is_null());
            let parsed = (0..n as usize).map(|i| (*argv.add(i)).to_string().unwrap()).collect();
            let _ = LocalFree(Some(HLOCAL(argv.cast())));
            parsed
        };
        let want: Vec<String> = std::iter::once(exe.to_str().unwrap()).chain(args).map(String::from).collect();
        assert_eq!(parsed, want);
        assert!(command_line(exe, &["a\0b"]).is_err());
        assert!(command_line(Path::new("C:\\a\"b.exe"), &[]).is_err());
    }

    /// Every child of `spawn_in_job` is in the dock's job, which kills what is left when the dock ends.
    #[test]
    fn children_join_the_job() {
        use windows::core::BOOL;
        use windows::Win32::System::JobObjects::{
            IsProcessInJob, JobObjectExtendedLimitInformation, QueryInformationJobObject,
            JOBOBJECT_EXTENDED_LIMIT_INFORMATION, JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
        };
        let mut child = spawn_in_job(&system32("ping.exe"), &["-n", "3", "127.0.0.1"], false).unwrap();
        let job = dock_job().expect("the job exists after a spawn");
        let mut inside = BOOL(0);
        // SAFETY: the child's handle is valid until `child` is dropped, the job's until the process ends.
        let asked = unsafe { IsProcessInJob(HANDLE(child.as_raw_handle()), Some(job), &mut inside) };
        let mut info = JOBOBJECT_EXTENDED_LIMIT_INFORMATION::default();
        let size = std::mem::size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32;
        // SAFETY: `info` is writable and `size` bytes long.
        let queried = unsafe {
            QueryInformationJobObject(
                Some(job),
                JobObjectExtendedLimitInformation,
                std::ptr::from_mut(&mut info).cast(),
                size,
                None,
            )
        };
        let _ = child.kill();
        let _ = child.wait();
        asked.unwrap();
        queried.unwrap();
        assert!(inside.as_bool());
        assert!(info.BasicLimitInformation.LimitFlags.contains(JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE));
    }
}
