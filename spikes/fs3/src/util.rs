//! Small helpers shared by the probes: wide strings, HRESULT text, a content hash, the COM apartment, the hidden
//! owner window and the system facts every record starts with.

use std::ffi::OsStr;
use std::os::windows::ffi::OsStrExt;
use std::path::{Path, PathBuf};
use std::time::Instant;

use windows::core::{w, HRESULT, PCWSTR};
use windows::Win32::Foundation::HWND;
use windows::Win32::Globalization::{GetUserDefaultLocaleName, GetUserDefaultUILanguage};
use windows::Win32::System::Com::{CoInitializeEx, CoUninitialize, COINIT_APARTMENTTHREADED, COINIT_DISABLE_OLE1DDE};
use windows::Win32::System::SystemInformation::{GetVersionExW, OSVERSIONINFOW};
use windows::Win32::UI::WindowsAndMessaging::{CreateWindowExW, DestroyWindow, WINDOW_EX_STYLE, WS_POPUP};

use crate::json::J;

/// `s` as a NUL-terminated UTF-16 string.
pub fn wide_str(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(Some(0)).collect()
}

/// `s` as a NUL-terminated UTF-16 string.
pub fn wide_os(s: &OsStr) -> Vec<u16> {
    s.encode_wide().chain(Some(0)).collect()
}

/// A PCWSTR over a NUL-terminated buffer; the buffer must outlive every use of the pointer.
pub fn pcwstr(v: &[u16]) -> PCWSTR {
    PCWSTR(v.as_ptr())
}

/// The text of a UTF-16 buffer up to its first NUL.
pub fn from_wide(buf: &[u16]) -> String {
    let n = buf.iter().position(|&c| c == 0).unwrap_or(buf.len());
    String::from_utf16_lossy(&buf[..n])
}

/// An HRESULT as the record writes it.
pub fn hr_hex(hr: HRESULT) -> String {
    format!("0x{:08X}", hr.0 as u32)
}

/// The HRESULT of a call's result (S_OK for success).
pub fn hr_of<T>(r: &windows::core::Result<T>) -> HRESULT {
    match r {
        Ok(_) => HRESULT(0),
        Err(e) => e.code(),
    }
}

/// FNV-1a, 64 bits: enough to tell whether a file's bytes changed.
pub fn fnv64(bytes: &[u8]) -> u64 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for &b in bytes {
        h ^= u64::from(b);
        h = h.wrapping_mul(0x0000_0100_0000_01b3);
    }
    h
}

/// The hash of a file's bytes, or None when it cannot be read.
pub fn hash_file(p: &Path) -> Option<String> {
    std::fs::read(verbatim(p)).ok().map(|b| format!("{:016x}", fnv64(&b)))
}

/// Milliseconds since `t0`.
pub fn ms(t0: Instant) -> u64 {
    u64::try_from(t0.elapsed().as_millis()).unwrap_or(u64::MAX)
}

/// The `\\?\` form of an absolute path (`\\?\UNC\` for a UNC path); a verbatim path is returned as it is.
pub fn verbatim(p: &Path) -> PathBuf {
    let s = p.as_os_str().to_string_lossy();
    if s.starts_with(r"\\?\") {
        p.to_path_buf()
    } else if let Some(rest) = s.strip_prefix(r"\\") {
        PathBuf::from(format!(r"\\?\UNC\{rest}"))
    } else {
        PathBuf::from(format!(r"\\?\{s}"))
    }
}

/// The length of a path in UTF-16 units.
pub fn units(p: &Path) -> usize {
    p.as_os_str().encode_wide().count()
}

/// The names of a folder's entries, sorted (the probe's own synthetic names).
pub fn names_in(dir: &Path) -> Vec<String> {
    let mut v: Vec<String> = std::fs::read_dir(verbatim(dir))
        .map(|it| it.flatten().map(|e| e.file_name().to_string_lossy().into_owned()).collect())
        .unwrap_or_default();
    v.sort();
    v
}

/// A single-threaded COM apartment for the thread that holds it (IFileOperation needs one).
pub struct Sta(());

impl Sta {
    pub fn init() -> Result<Sta, String> {
        // SAFETY: initialises COM on this thread; balanced by CoUninitialize in Drop.
        let hr = unsafe { CoInitializeEx(None, COINIT_APARTMENTTHREADED | COINIT_DISABLE_OLE1DDE) };
        if hr.is_err() {
            return Err(format!("CoInitializeEx: {}", hr_hex(hr)));
        }
        Ok(Sta(()))
    }
}

impl Drop for Sta {
    fn drop(&mut self) {
        // SAFETY: pairs the successful CoInitializeEx of init on the same thread.
        unsafe { CoUninitialize() };
    }
}

/// The probe's hidden top-level window: WS_POPUP, never shown; it owns the operations' dialogs.
pub struct Hidden(pub HWND);

impl Hidden {
    pub fn new() -> Result<Hidden, String> {
        // SAFETY: a STATIC window of this thread with no WS_VISIBLE; it is never shown and is destroyed in Drop.
        let h = unsafe {
            CreateWindowExW(
                WINDOW_EX_STYLE(0),
                w!("STATIC"),
                w!("files probe owner"),
                WS_POPUP,
                0,
                0,
                1,
                1,
                None,
                None,
                None,
                None,
            )
        }
        .map_err(|e| format!("CreateWindowExW: {e}"))?;
        Ok(Hidden(h))
    }
}

impl Drop for Hidden {
    fn drop(&mut self) {
        // SAFETY: the window was created by this thread in new and is destroyed once.
        let _ = unsafe { DestroyWindow(self.0) };
    }
}

/// The facts every record starts with: Windows version and build, the user's locale and UI language.
pub fn system_facts() -> J {
    let mut v =
        OSVERSIONINFOW { dwOSVersionInfoSize: std::mem::size_of::<OSVERSIONINFOW>() as u32, ..Default::default() };
    // SAFETY: v is a properly sized OSVERSIONINFOW; the manifest declares Windows 10 so the real version is returned.
    let ver = unsafe { GetVersionExW(&mut v) }
        .ok()
        .map(|()| format!("{}.{}.{}", v.dwMajorVersion, v.dwMinorVersion, v.dwBuildNumber));
    let mut loc = [0u16; 85];
    // SAFETY: the buffer holds LOCALE_NAME_MAX_LENGTH units.
    let n = unsafe { GetUserDefaultLocaleName(&mut loc) };
    // SAFETY: no arguments.
    let ui = unsafe { GetUserDefaultUILanguage() };
    J::obj()
        .set("windows", ver)
        .set("locale", if n > 0 { Some(from_wide(&loc)) } else { None })
        .set("ui_language", format!("0x{ui:04X}"))
        .set("arch", std::env::consts::ARCH)
        .set("debug_build", cfg!(debug_assertions))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn verbatim_forms() {
        assert_eq!(verbatim(Path::new(r"C:\a\b")), PathBuf::from(r"\\?\C:\a\b"));
        assert_eq!(verbatim(Path::new(r"\\host\share\a")), PathBuf::from(r"\\?\UNC\host\share\a"));
        assert_eq!(verbatim(Path::new(r"\\?\C:\a")), PathBuf::from(r"\\?\C:\a"));
    }

    #[test]
    fn fnv64_matches_the_reference_vectors() {
        assert_eq!(fnv64(b""), 0xcbf2_9ce4_8422_2325);
        assert_eq!(fnv64(b"a"), 0xaf63_dc4c_8601_ec8c);
        assert_eq!(fnv64(b"foobar"), 0x8594_4171_f739_67e8);
    }

    #[test]
    fn wide_round_trip() {
        let w = wide_str("abc");
        assert_eq!(w, [97, 98, 99, 0]);
        assert_eq!(from_wide(&w), "abc");
        assert_eq!(units(Path::new(r"C:\ab")), 5);
        assert_eq!(hr_hex(HRESULT(0x8007_04C7_u32 as i32)), "0x800704C7");
    }
}
