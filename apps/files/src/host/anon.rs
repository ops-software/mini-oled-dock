//! Anonymous pipe pairs and the other handles a child inherits (SH2). std::io::pipe makes both ends non-inheritable;
//! the child's end is made inheritable only for the spawn that lists it, and the parent closes its copy right after.
#![deny(clippy::unwrap_used, clippy::expect_used, clippy::indexing_slicing)]

use std::io;
use std::os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle};

use windows::Win32::Foundation::{
    DuplicateHandle, SetHandleInformation, DUPLICATE_HANDLE_OPTIONS, HANDLE, HANDLE_FLAGS, HANDLE_FLAG_INHERIT,
};
use windows::Win32::System::Threading::{GetCurrentProcess, PROCESS_SYNCHRONIZE};

/// A pipe pair: (read end, write end), neither inheritable.
pub fn pipe_pair() -> io::Result<(io::PipeReader, io::PipeWriter)> {
    io::pipe()
}

/// Makes `h` inheritable, for the handle list of the next spawn, and returns its value.
pub fn inheritable(h: &impl AsRawHandle) -> io::Result<HANDLE> {
    let handle = HANDLE(h.as_raw_handle());
    // SAFETY: a handle owned by the caller, open for the call.
    unsafe { SetHandleInformation(handle, HANDLE_FLAG_INHERIT.0, HANDLE_FLAG_INHERIT) }?;
    Ok(handle)
}

/// Makes `h` non-inheritable again.
pub fn not_inheritable(h: &impl AsRawHandle) -> io::Result<()> {
    // SAFETY: a handle owned by the caller, open for the call.
    unsafe { SetHandleInformation(HANDLE(h.as_raw_handle()), HANDLE_FLAG_INHERIT.0, HANDLE_FLAGS(0)) }?;
    Ok(())
}

/// A SYNCHRONIZE-only handle to this process, not inheritable: what a child's parent watch waits on.
pub fn this_process_for_watch() -> io::Result<OwnedHandle> {
    let mut dup = HANDLE::default();
    // SAFETY: duplicates this process's pseudo handle into this process with SYNCHRONIZE access only; `dup` is
    // writable and owned below.
    unsafe {
        let me = GetCurrentProcess();
        DuplicateHandle(me, me, me, &mut dup, PROCESS_SYNCHRONIZE.0, false, DUPLICATE_HANDLE_OPTIONS(0))
    }?;
    // SAFETY: just returned by DuplicateHandle, owned here once.
    Ok(unsafe { OwnedHandle::from_raw_handle(dup.0) })
}

/// The handle value a child received on its command line (decimal), as a HANDLE.
pub fn handle_arg(v: &str) -> Option<HANDLE> {
    let n: usize = v.parse().ok()?;
    (n != 0).then_some(HANDLE(n as *mut core::ffi::c_void))
}

/// A handle's value as the child's command line carries it.
pub fn arg_of(h: HANDLE) -> String {
    (h.0 as usize).to_string()
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::indexing_slicing)]
    use super::*;
    use windows::Win32::Foundation::GetHandleInformation;

    fn flags(h: &impl AsRawHandle) -> u32 {
        let mut f = 0u32;
        // SAFETY: an open handle of the test's; `f` is writable.
        unsafe { GetHandleInformation(HANDLE(h.as_raw_handle()), &mut f) }.expect("valid");
        f
    }

    #[test]
    fn inheritable_is_set_and_cleared() {
        let (r, w) = pipe_pair().expect("pipe");
        assert_eq!(flags(&w) & HANDLE_FLAG_INHERIT.0, 0, "std's ends start non-inheritable");
        inheritable(&w).expect("set");
        assert_ne!(flags(&w) & HANDLE_FLAG_INHERIT.0, 0);
        not_inheritable(&w).expect("cleared");
        assert_eq!(flags(&w) & HANDLE_FLAG_INHERIT.0, 0);
        drop((r, w));
        let me = this_process_for_watch().expect("a handle to this process");
        assert_eq!(flags(&me) & HANDLE_FLAG_INHERIT.0, 0);
    }

    #[test]
    fn handle_args_round_trip() {
        let h = HANDLE(0x1234 as *mut core::ffi::c_void);
        assert_eq!(handle_arg(&arg_of(h)), Some(h));
        assert_eq!(handle_arg("0"), None);
        assert_eq!(handle_arg("x"), None);
        assert_eq!(handle_arg("-4"), None);
    }
}
