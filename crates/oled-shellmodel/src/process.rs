//! A process, looked at through a query-only handle: its image, package, creation time and parent, and the walk
//! from a helper up to the root of its install tree. Windows only; nothing here needs more than
//! PROCESS_QUERY_LIMITED_INFORMATION.

use std::collections::HashMap;
use std::ffi::c_void;

use windows::core::PWSTR;
use windows::Win32::Foundation::{CloseHandle, FILETIME, HANDLE};
use windows::Win32::Storage::Packaging::Appx::GetApplicationUserModelId;
use windows::Win32::System::Threading::{
    GetProcessTimes, OpenProcess, QueryFullProcessImageNameW, PROCESS_NAME_WIN32, PROCESS_QUERY_LIMITED_INFORMATION,
};

use crate::identity::same_install_tree;

#[link(name = "ntdll")]
extern "system" {
    fn NtQueryInformationProcess(process: HANDLE, class: u32, info: *mut c_void, len: u32, ret_len: *mut u32) -> i32;
}

/// `PROCESS_BASIC_INFORMATION` (`ProcessBasicInformation` = class 0).
#[repr(C)]
struct ProcessBasicInformation {
    exit_status: i32,
    peb_base: *mut c_void,
    affinity_mask: usize,
    base_priority: i32,
    pid: usize,
    parent_pid: usize,
}

/// Maximum number of ancestors walked when looking for the root of an install tree.
const MAX_ROOT_DEPTH: usize = 8;
/// Bound on the per-pid root cache (cleared when exceeded: entries are cheap to recompute).
const ROOT_CACHE_MAX: usize = 256;

/// Automatically closed process handle, opened with PROCESS_QUERY_LIMITED_INFORMATION.
pub struct OwnedProcess(HANDLE);

impl OwnedProcess {
    #[inline]
    pub fn open(pid: u32) -> Option<OwnedProcess> {
        // SAFETY: OpenProcess has no preconditions; the obtained handle is owned by `OwnedProcess`.
        unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid) }.ok().map(OwnedProcess)
    }

    /// The raw handle, valid while `self` lives.
    #[inline]
    pub fn handle(&self) -> HANDLE {
        self.0
    }
}

impl Drop for OwnedProcess {
    fn drop(&mut self) {
        // SAFETY: valid handle obtained from OpenProcess, closed exactly once here.
        let _ = unsafe { CloseHandle(self.0) };
    }
}

/// Exe path of process `pid` (`None` if it cannot be opened).
#[inline]
pub fn exe_path_of_pid(pid: u32) -> Option<String> {
    let proc = OwnedProcess::open(pid)?;
    exe_path(&proc)
}

/// Full Win32 image path of the process.
#[inline]
pub fn exe_path(p: &OwnedProcess) -> Option<String> {
    exe_path_of_handle(p.0)
}

/// Full Win32 image path of `proc`, a handle with PROCESS_QUERY_LIMITED_INFORMATION.
#[inline]
pub fn exe_path_of_handle(proc: HANDLE) -> Option<String> {
    // Common case: stack buffer, no heap allocation.
    let mut small = [0u16; 512];
    let mut len = small.len() as u32;
    // SAFETY: `small` has `len` writable elements; `len` is in/out (in: capacity, out: length written
    // without NUL). `proc` is a handle with PROCESS_QUERY_LIMITED_INFORMATION.
    let r = unsafe { QueryFullProcessImageNameW(proc, PROCESS_NAME_WIN32, PWSTR(small.as_mut_ptr()), &mut len) };
    if r.is_ok() {
        let s = String::from_utf16_lossy(&small[..(len as usize).min(small.len())]);
        return if s.is_empty() { None } else { Some(s) };
    }
    // Long paths: retry once with the maximum size.
    let mut buf = vec![0u16; 32768];
    let mut len = buf.len() as u32;
    // SAFETY: same contract as above, with `buf` as the output buffer.
    let r = unsafe { QueryFullProcessImageNameW(proc, PROCESS_NAME_WIN32, PWSTR(buf.as_mut_ptr()), &mut len) };
    if r.is_err() {
        return None;
    }
    let s = String::from_utf16_lossy(&buf[..(len as usize).min(buf.len())]);
    if s.is_empty() {
        None
    } else {
        Some(s)
    }
}

/// Package AUMID of the process. `None` if the process has no package identity
/// (`APPMODEL_ERROR_NO_APPLICATION`).
/// Package AUMID of the process. `None` if the process has no package identity
/// (`APPMODEL_ERROR_NO_APPLICATION`).
#[inline]
pub fn package_aumid(p: &OwnedProcess) -> Option<String> {
    let proc = p.0;
    let mut buf = [0u16; 256]; // an AUMID is at most 129 characters
    let mut len = buf.len() as u32;
    // SAFETY: `buf` has `len` writable WCHARs; `len` is in/out; `proc` is a valid handle.
    let rc = unsafe { GetApplicationUserModelId(proc, &mut len, Some(PWSTR(buf.as_mut_ptr()))) };
    if rc.0 != 0 {
        return None;
    }
    // `len` includes the terminating NUL.
    let n = (len as usize).saturating_sub(1).min(buf.len());
    let s = String::from_utf16_lossy(&buf[..n]);
    if s.is_empty() {
        None
    } else {
        Some(s)
    }
}

/// Process creation time (100 ns ticks since 1601).
#[inline]
pub fn creation_time(p: &OwnedProcess) -> Option<u64> {
    creation_time_of_handle(p.0)
}

/// `creation_time` for a handle the caller owns (any access that includes PROCESS_QUERY_LIMITED_INFORMATION).
#[inline]
pub fn creation_time_of_handle(proc: HANDLE) -> Option<u64> {
    let (mut c, mut e, mut k, mut u) =
        (FILETIME::default(), FILETIME::default(), FILETIME::default(), FILETIME::default());
    // SAFETY: all four out-parameters are valid local FILETIMEs; `proc` is a valid process handle.
    unsafe { GetProcessTimes(proc, &mut c, &mut e, &mut k, &mut u) }.ok()?;
    Some((u64::from(c.dwHighDateTime) << 32) | u64::from(c.dwLowDateTime))
}

/// Parent process id of the process (`NtQueryInformationProcess` class 0); None for 0 or on failure.
#[inline]
pub fn parent_pid(p: &OwnedProcess) -> Option<u32> {
    parent_pid_of_handle(p.0)
}

/// `parent_pid` for a handle the caller owns.
#[inline]
pub fn parent_pid_of_handle(proc: HANDLE) -> Option<u32> {
    let mut pbi = ProcessBasicInformation {
        exit_status: 0,
        peb_base: std::ptr::null_mut(),
        affinity_mask: 0,
        base_priority: 0,
        pid: 0,
        parent_pid: 0,
    };
    let mut ret = 0u32;
    // SAFETY: `pbi` is a writable struct matching PROCESS_BASIC_INFORMATION with the declared size;
    // `proc` is a handle with PROCESS_QUERY_LIMITED_INFORMATION, enough for class 0.
    let st = unsafe {
        NtQueryInformationProcess(
            proc,
            0,
            (&mut pbi as *mut ProcessBasicInformation).cast::<c_void>(),
            std::mem::size_of::<ProcessBasicInformation>() as u32,
            &mut ret,
        )
    };
    if st < 0 {
        return None;
    }
    u32::try_from(pbi.parent_pid).ok().filter(|&p| p != 0)
}

/// pid -> (creation time of the pid, root exe), at most 256 entries (cleared when full). The creation time guards
/// against pid reuse. Owned by the caller, which decides the thread and the borrow rule.
#[derive(Default)]
pub struct RootCache {
    map: HashMap<u32, (u64, String)>,
}

impl RootCache {
    /// Walks up the parent chain while the parent lives in the same install tree; returns the topmost
    /// such exe (or `exe_path` itself). Cached per pid.
    #[inline]
    pub fn root_exe_of(&mut self, pid: u32, p: &OwnedProcess, exe_path: &str) -> String {
        let Some(t0) = creation_time(p) else {
            return exe_path.to_string();
        };
        if let Some(r) = self.map.get(&pid).filter(|(t, _)| *t == t0).map(|(_, r)| r.clone()) {
            return r;
        }
        let mut cur_exe = exe_path.to_string();
        let mut cur_time = t0;
        let mut cur_pid = pid;
        let mut cur_handle = p.0;
        let mut held: Option<OwnedProcess> = None;
        for _ in 0..MAX_ROOT_DEPTH {
            let Some(ppid) = parent_pid_of_handle(cur_handle).filter(|&p| p != cur_pid) else { break };
            let Some(parent) = OwnedProcess::open(ppid) else { break };
            // The parent must be older than the child, otherwise the pid was reused.
            let Some(pt) = creation_time(&parent).filter(|&pt| pt < cur_time) else { break };
            let Some(pexe) = exe_path_of_handle(parent.0) else { break };
            if !same_install_tree(&pexe, &cur_exe) {
                break;
            }
            cur_exe = pexe;
            cur_time = pt;
            cur_pid = ppid;
            cur_handle = parent.0;
            held = Some(parent); // keeps the handle alive for the next step; the previous one is closed
        }
        drop(held);
        if self.map.len() >= ROOT_CACHE_MAX {
            self.map.clear();
        }
        self.map.insert(pid, (t0, cur_exe.clone()));
        cur_exe
    }

    /// Test door: a cached answer.
    #[cfg(test)]
    fn insert(&mut self, pid: u32, created: u64, root: &str) {
        self.map.insert(pid, (created, root.to_string()));
    }

    #[cfg(test)]
    fn len(&self) -> usize {
        self.map.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn own() -> (u32, OwnedProcess) {
        let pid = std::process::id();
        (pid, OwnedProcess::open(pid).expect("the test process opens itself"))
    }

    #[test]
    fn root_cache_clears_when_full() {
        let (pid, p) = own();
        let mut c = RootCache::default();
        for fake in 0..ROOT_CACHE_MAX as u32 {
            c.insert(fake + 1_000_000, 1, "x");
        }
        assert_eq!(c.len(), ROOT_CACHE_MAX);
        // A miss on a full cache clears it, then stores the new entry.
        let root = c.root_exe_of(pid, &p, r"C:\Program Files\Contoso\app.exe");
        assert_eq!(root, r"C:\Program Files\Contoso\app.exe");
        assert_eq!(c.len(), 1);
    }

    #[test]
    fn root_cache_respects_creation_time() {
        let (pid, p) = own();
        let born = creation_time(&p).expect("creation time of the test process");
        let mut c = RootCache::default();
        // The same pid with another creation time is a reused pid: the stale answer is not used.
        c.insert(pid, born + 1, "stale");
        assert_eq!(c.root_exe_of(pid, &p, "fresh"), "fresh");
        // The entry of the live process is.
        c.insert(pid, born, "cached");
        assert_eq!(c.root_exe_of(pid, &p, "fresh"), "cached");
    }

    #[test]
    fn the_test_process_has_an_image_and_a_parent() {
        let (pid, p) = own();
        assert!(exe_path(&p).is_some_and(|e| e.to_ascii_lowercase().ends_with(".exe")));
        assert_eq!(exe_path_of_pid(pid), exe_path(&p));
        assert!(parent_pid(&p).is_some());
        assert_eq!(package_aumid(&p), None, "a plain exe has no package identity");
    }
}
