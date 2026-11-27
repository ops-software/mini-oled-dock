//! Process and file identity.

use std::os::windows::ffi::OsStrExt;
use std::os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle};
use std::path::Path;

use windows::core::PCWSTR;
use windows::Win32::Foundation::HANDLE;
use windows::Win32::Storage::FileSystem::{
    CreateFileW, FileIdInfo, GetFileInformationByHandleEx, FILE_FLAG_OPEN_REPARSE_POINT, FILE_ID_INFO,
    FILE_READ_ATTRIBUTES, FILE_SHARE_DELETE, FILE_SHARE_READ, FILE_SHARE_WRITE, OPEN_EXISTING,
};

pub fn same_file(a: &Path, b: &Path) -> bool {
    match (std::fs::canonicalize(a), std::fs::canonicalize(b)) {
        (Ok(a), Ok(b)) => a.to_string_lossy().eq_ignore_ascii_case(&b.to_string_lossy()),
        _ => false,
    }
}

/// The peer rule of every pipe and worker check (F-P3): both paths name the same file by volume serial number and
/// file id, so two spellings of one file (case, 8.3, a \\?\ prefix, a hard link) are equal and a copy is not. Any open
/// or query error is "not the same"; a reparse point is the link itself, not its target.
pub fn same_image(a: &Path, b: &Path) -> bool {
    match (file_id(a), file_id(b)) {
        (Some(a), Some(b)) => a == b,
        _ => false,
    }
}

/// The volume serial number and the 128-bit file id of `path` (FileIdInfo), opened for attributes only, sharing
/// everything (a running image opens too), without following a reparse point.
fn file_id(path: &Path) -> Option<(u64, [u8; 16])> {
    let wide: Vec<u16> = path.as_os_str().encode_wide().chain(std::iter::once(0)).collect();
    if wide.iter().rev().skip(1).any(|&c| c == 0) {
        return None;
    }
    // SAFETY: `wide` is NUL-terminated and lives for the call; no security attributes or template.
    let h = unsafe {
        CreateFileW(
            PCWSTR(wide.as_ptr()),
            FILE_READ_ATTRIBUTES.0,
            FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE,
            None,
            OPEN_EXISTING,
            FILE_FLAG_OPEN_REPARSE_POINT,
            None,
        )
    }
    .ok()?;
    // SAFETY: a fresh handle, owned by nothing else.
    let h = unsafe { OwnedHandle::from_raw_handle(h.0) };
    let mut info = FILE_ID_INFO::default();
    // SAFETY: the handle is valid while `h` lives; `info` is writable and as large as the size given.
    unsafe {
        GetFileInformationByHandleEx(
            HANDLE(h.as_raw_handle()),
            FileIdInfo,
            std::ptr::from_mut(&mut info).cast(),
            std::mem::size_of::<FILE_ID_INFO>() as u32,
        )
    }
    .ok()?;
    Some((info.VolumeSerialNumber, info.FileId.Identifier))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn same_file_ignores_case_and_refuses_missing_paths() {
        let exe = std::env::current_exe().unwrap();
        assert!(same_file(&exe, &exe));
        assert!(same_file(&exe, Path::new(&exe.to_string_lossy().to_uppercase())));
        assert!(!same_file(&exe, &exe.with_extension("missing")));
        assert!(!same_file(&exe.with_extension("missing"), &exe.with_extension("missing")));
    }

    #[test]
    fn a_path_with_a_nul_is_no_file() {
        let exe = std::env::current_exe().unwrap();
        let bad = std::path::PathBuf::from(format!("{}\0x", exe.display()));
        assert!(!same_image(&exe, &bad));
    }
}
