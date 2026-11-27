//! Spawning with an explicit inherited-handle list, CREATE_NO_WINDOW and no job (SH2). The shell host is not in a
//! kill-on-close job (open point 3: a busy host must be able to outlive the UI, and what it starts must not die with
//! the UI), so this is the job-free form of the dock's spawn: the child is created suspended, inherits exactly the
//! handles listed (PROC_THREAD_ATTRIBUTE_HANDLE_LIST) and nothing else, and gets no window and no busy pointer.
#![deny(clippy::unwrap_used, clippy::expect_used, clippy::indexing_slicing)]

use std::ffi::OsString;
use std::io;
use std::os::windows::ffi::OsStrExt;
use std::os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle};
use std::path::Path;

use windows::core::{PCWSTR, PWSTR};
use windows::Win32::Foundation::{HANDLE, WAIT_OBJECT_0, WAIT_TIMEOUT};
use windows::Win32::System::Threading::{
    CreateProcessW, DeleteProcThreadAttributeList, GetExitCodeProcess, InitializeProcThreadAttributeList, ResumeThread,
    TerminateProcess, UpdateProcThreadAttribute, WaitForSingleObject, CREATE_NO_WINDOW, CREATE_SUSPENDED,
    CREATE_UNICODE_ENVIRONMENT, EXTENDED_STARTUPINFO_PRESENT, LPPROC_THREAD_ATTRIBUTE_LIST, PROCESS_CREATION_FLAGS,
    PROCESS_INFORMATION, PROC_THREAD_ATTRIBUTE_HANDLE_LIST, STARTF_FORCEOFFFEEDBACK, STARTF_USESTDHANDLES,
    STARTUPINFOEXW, STARTUPINFOW,
};

/// A started child: its process handle (SYNCHRONIZE, TERMINATE, QUERY) and id. Dropping it closes the handle only.
pub struct Child {
    pub process: OwnedHandle,
    pub pid: u32,
}

impl Child {
    pub fn handle(&self) -> HANDLE {
        HANDLE(self.process.as_raw_handle())
    }

    /// The exit code once the child has ended within `ms`, else None.
    pub fn wait_ms(&self, ms: u32) -> Option<u32> {
        // SAFETY: the process handle is valid while `self` lives; waiting has no other effect.
        if unsafe { WaitForSingleObject(self.handle(), ms) } != WAIT_OBJECT_0 {
            return None;
        }
        let mut code = 0u32;
        // SAFETY: as above; `code` is writable.
        unsafe { GetExitCodeProcess(self.handle(), &mut code) }.ok()?;
        Some(code)
    }

    /// true while the child runs.
    pub fn running(&self) -> bool {
        // SAFETY: as in wait_ms.
        let waited = unsafe { WaitForSingleObject(self.handle(), 0) };
        waited == WAIT_TIMEOUT
    }
}

/// The NUL-terminated command line of `exe args` as the C runtime reads it back (the suite's one writer of it).
pub fn command_line(exe: &Path, args: &[&str]) -> io::Result<Vec<u16>> {
    oled_sys::worker::bound::command_line(exe, args)
}

/// The environment block of a child: this process's variables with `env` set on top (names compared without case),
/// sorted by name as Windows keeps it, each "name=value" NUL-terminated, then one more NUL.
pub fn env_block(env: &[(&str, &str)]) -> Vec<u16> {
    let mut vars: Vec<(OsString, OsString)> = std::env::vars_os()
        .filter(|(k, _)| !env.iter().any(|(n, _)| k.to_string_lossy().eq_ignore_ascii_case(n)))
        .collect();
    vars.extend(env.iter().map(|(k, v)| (OsString::from(k), OsString::from(v))));
    vars.sort_by_key(|(k, _)| k.to_string_lossy().to_uppercase());
    let mut block = Vec::new();
    for (k, v) in &vars {
        block.extend(k.encode_wide());
        block.push(u16::from(b'='));
        block.extend(v.encode_wide());
        block.push(0);
    }
    block.push(0);
    block
}

/// Starts `"exe" args` with CREATE_NO_WINDOW, no job, `env` set on the child only, and exactly `handles`
/// inherited (each must be inheritable at the call: anon::inheritable). No standard handles.
pub fn spawn_inheriting(exe: &Path, args: &[&str], env: &[(&str, &str)], handles: &[HANDLE]) -> io::Result<Child> {
    let mut line = command_line(exe, args)?;
    let app: Vec<u16> = exe.as_os_str().encode_wide().chain(std::iter::once(0)).collect();
    let block = (!env.is_empty()).then(|| env_block(env));
    let mut attrs = if handles.is_empty() { None } else { Some(HandleList::new(handles)?) };
    let si = STARTUPINFOEXW {
        StartupInfo: STARTUPINFOW {
            cb: std::mem::size_of::<STARTUPINFOEXW>() as u32,
            dwFlags: STARTF_USESTDHANDLES | STARTF_FORCEOFFFEEDBACK,
            ..Default::default()
        },
        lpAttributeList: attrs.as_mut().map_or(LPPROC_THREAD_ATTRIBUTE_LIST::default(), HandleList::list),
    };
    let mut pi = PROCESS_INFORMATION::default();
    let extended = if attrs.is_some() { EXTENDED_STARTUPINFO_PRESENT } else { PROCESS_CREATION_FLAGS(0) };
    let flags = CREATE_NO_WINDOW | CREATE_SUSPENDED | CREATE_UNICODE_ENVIRONMENT | extended;
    // SAFETY: `app`, `line` (mutable, as CreateProcessW wants) and `block` are NUL-terminated and live for the
    // call; `si` and its attribute list, which points at `handles`, live until after it; `pi` is writable.
    let created = unsafe {
        CreateProcessW(
            PCWSTR(app.as_ptr()),
            Some(PWSTR(line.as_mut_ptr())),
            None,
            None,
            !handles.is_empty(),
            flags,
            block.as_ref().map(|b| b.as_ptr().cast()),
            PCWSTR::null(),
            std::ptr::from_ref(&si).cast(),
            &mut pi,
        )
    };
    drop(attrs);
    created?;
    // SAFETY: both handles were just returned by CreateProcessW and are owned here, each once.
    let (process, thread) =
        unsafe { (OwnedHandle::from_raw_handle(pi.hProcess.0), OwnedHandle::from_raw_handle(pi.hThread.0)) };
    // SAFETY: the handle of the child's only thread, suspended since its creation.
    if unsafe { ResumeThread(HANDLE(thread.as_raw_handle())) } == u32::MAX {
        let e = io::Error::last_os_error();
        // The child never ran a line of its own (no request, no crumbs, no busy call): ending it needs no kill
        // protocol. SAFETY: the process handle was just returned by CreateProcessW.
        let _ = unsafe { TerminateProcess(HANDLE(process.as_raw_handle()), 1) };
        return Err(e);
    }
    Ok(Child { process, pid: pi.dwProcessId })
}

/// The attribute list of one spawn: the handles its child may inherit, and nothing else. It borrows `handles`.
struct HandleList<'a> {
    buf: Vec<usize>,
    _handles: std::marker::PhantomData<&'a [HANDLE]>,
}

impl<'a> HandleList<'a> {
    fn new(handles: &'a [HANDLE]) -> io::Result<Self> {
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

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::indexing_slicing)]
    use super::*;
    use windows::Win32::Foundation::{LocalFree, HLOCAL};
    use windows::Win32::UI::Shell::CommandLineToArgvW;

    /// What the C runtime reads back from a command line (CommandLineToArgvW follows the same rules).
    fn argv(line: &[u16]) -> Vec<String> {
        let mut n = 0;
        // SAFETY: `line` is NUL-terminated; the returned block is freed once with LocalFree.
        let list = unsafe { CommandLineToArgvW(PCWSTR(line.as_ptr()), &mut n) };
        assert!(!list.is_null());
        // SAFETY: the block holds `n` valid NUL-terminated strings.
        let out = (0..n as usize).map(|i| unsafe { (*list.add(i)).to_string() }.unwrap_or_default()).collect();
        // SAFETY: allocated by CommandLineToArgvW, freed once.
        unsafe { LocalFree(Some(HLOCAL(list.cast()))) };
        out
    }

    #[test]
    fn command_line_round_trip() {
        let exe = Path::new(r"C:\Program Files\x\files.exe");
        let args = [
            "--role=shell",
            "",
            "a b",
            "tab\there",
            r#"quote"inside"#,
            r"trailing\",
            r"two\\",
            r#"back\"slash"#,
            r"C:\a b\",
            "plain",
        ];
        let line = command_line(exe, &args).expect("a valid line");
        let back = argv(&line);
        assert_eq!(back.first().map(String::as_str), Some(r"C:\Program Files\x\files.exe"));
        assert_eq!(back.get(1..).unwrap_or_default(), args);
        assert!(command_line(exe, &["nul\0"]).is_err(), "a NUL is refused");
        assert!(command_line(Path::new(r#"C:\a"b.exe"#), &[]).is_err(), "a quote in the program is refused");
    }

    #[test]
    fn env_block_sets_and_sorts() {
        let block = env_block(&[("FILES_ZZ_TEST", "1"), ("files_aa_test", "x=y")]);
        assert_eq!(block.last(), Some(&0));
        let text = String::from_utf16_lossy(&block);
        let vars: Vec<&str> = text.split('\0').filter(|s| !s.is_empty()).collect();
        assert!(vars.contains(&"FILES_ZZ_TEST=1") && vars.contains(&"files_aa_test=x=y"), "{vars:?}");
        let keys: Vec<String> = vars.iter().map(|v| v.split('=').next().unwrap_or("").to_uppercase()).collect();
        let mut sorted = keys.clone();
        sorted.sort();
        assert_eq!(keys, sorted, "sorted by name");
    }
}
