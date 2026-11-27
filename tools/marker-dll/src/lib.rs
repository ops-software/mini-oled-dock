//! The marker DLL (live check L-I1 (d)): when a process loads it, it writes `loaded-<its file name>.txt` next to itself
//! and returns TRUE. It exports nothing a real system DLL exports, so a process that took it in place of one fails
//! later on a missing entry point: the file is the evidence that the loader looked beside the exe.

use std::ffi::{c_void, OsString};
use std::os::windows::ffi::OsStringExt;
use std::path::{Path, PathBuf};

/// `DLL_PROCESS_ATTACH`.
const PROCESS_ATTACH: u32 = 1;
/// The longest module path asked for, in UTF-16 units.
const PATH_UNITS: usize = 32_768;

#[link(name = "kernel32")]
extern "system" {
    fn GetModuleFileNameW(module: *mut c_void, name: *mut u16, size: u32) -> u32;
}

/// The loader's entry point: on process attach, the marker file; always TRUE.
#[no_mangle]
pub extern "system" fn DllMain(module: *mut c_void, reason: u32, _reserved: *mut c_void) -> i32 {
    if reason == PROCESS_ATTACH {
        if let Some(dll) = own_path(module) {
            mark(&dll);
        }
    }
    1
}

/// This DLL's full path, from the module handle the loader gave `DllMain`.
fn own_path(module: *mut c_void) -> Option<PathBuf> {
    let mut buf = vec![0u16; PATH_UNITS];
    // SAFETY: `buf` holds PATH_UNITS writable units, the size passed; `module` is this DLL's handle.
    let n = unsafe { GetModuleFileNameW(module, buf.as_mut_ptr(), PATH_UNITS as u32) } as usize;
    if n == 0 || n >= PATH_UNITS {
        return None;
    }
    buf.truncate(n);
    Some(PathBuf::from(OsString::from_wide(&buf)))
}

/// Writes `loaded-<file name>.txt` in the DLL's folder; errors are ignored (nothing else may fail a load).
fn mark(dll: &Path) {
    let (Some(dir), Some(name)) = (dll.parent(), dll.file_name()) else { return };
    let mut marker = OsString::from("loaded-");
    marker.push(name);
    marker.push(".txt");
    let _ = std::fs::write(dir.join(marker), b"loaded\r\n");
}
