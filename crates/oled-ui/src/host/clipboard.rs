//! The system clipboard behind EventCx::clipboard (K1a.8, KA2): read only on an explicit paste, written only on copy or
//! cut; no listener. OpenClipboard is retried 3 times 10 ms apart (another process may hold it for a moment).

use windows::Win32::Foundation::{GlobalFree, HANDLE, HGLOBAL, HWND};
use windows::Win32::System::DataExchange::{
    CloseClipboard, EmptyClipboard, GetClipboardData, IsClipboardFormatAvailable, OpenClipboard, SetClipboardData,
};
use windows::Win32::System::Memory::{GlobalAlloc, GlobalLock, GlobalSize, GlobalUnlock, GMEM_MOVEABLE};
use windows::Win32::System::Ole::{CF_HDROP, CF_UNICODETEXT};

use crate::cx::Clipboard;

/// The Windows clipboard, opened for the window `hwnd`.
pub(crate) struct SysClipboard {
    pub(crate) hwnd: HWND,
}

/// The clipboard opened for one operation; closed on drop.
struct Open;

impl Open {
    fn new(hwnd: HWND) -> Option<Open> {
        for attempt in 0..3 {
            // SAFETY: our window as the owner; closed by Drop.
            if unsafe { OpenClipboard(Some(hwnd)) }.is_ok() {
                return Some(Open);
            }
            if attempt < 2 {
                std::thread::sleep(std::time::Duration::from_millis(10));
            }
        }
        None
    }
}

impl Drop for Open {
    fn drop(&mut self) {
        // SAFETY: this thread opened the clipboard in Open::new.
        let _ = unsafe { CloseClipboard() };
    }
}

/// UTF-16 units up to the first NUL (or the end).
pub(crate) fn until_nul(units: &[u16]) -> &[u16] {
    &units[..units.iter().position(|u| *u == 0).unwrap_or(units.len())]
}

/// A string as the NUL-terminated UTF-16 that CF_UNICODETEXT holds.
pub(crate) fn wide_nul(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

impl Clipboard for SysClipboard {
    fn text(&mut self) -> Option<String> {
        let _open = Open::new(self.hwnd)?;
        // SAFETY: the clipboard is open; the handle belongs to the clipboard and is only locked for the copy below.
        let h = unsafe { GetClipboardData(CF_UNICODETEXT.0.into()) }.ok()?;
        let mem = HGLOBAL(h.0);
        // SAFETY: `mem` is the clipboard's global memory; locked and unlocked here, read within its size.
        unsafe {
            let p = GlobalLock(mem) as *const u16;
            if p.is_null() {
                return None;
            }
            let units = std::slice::from_raw_parts(p, GlobalSize(mem) / 2);
            let s = String::from_utf16_lossy(until_nul(units));
            let _ = GlobalUnlock(mem);
            Some(s)
        }
    }

    fn set_text(&mut self, s: &str) -> bool {
        let wide = wide_nul(s);
        let Some(_open) = Open::new(self.hwnd) else { return false };
        // SAFETY: the clipboard is open; the memory is ours until SetClipboardData takes it, freed here otherwise.
        unsafe {
            if EmptyClipboard().is_err() {
                return false;
            }
            let Ok(mem) = GlobalAlloc(GMEM_MOVEABLE, wide.len() * 2) else { return false };
            let p = GlobalLock(mem) as *mut u16;
            if p.is_null() {
                let _ = GlobalFree(Some(mem));
                return false;
            }
            std::ptr::copy_nonoverlapping(wide.as_ptr(), p, wide.len());
            let _ = GlobalUnlock(mem);
            if SetClipboardData(CF_UNICODETEXT.0.into(), Some(HANDLE(mem.0))).is_err() {
                let _ = GlobalFree(Some(mem));
                return false;
            }
        }
        true
    }

    fn has_files(&mut self) -> bool {
        // SAFETY: no precondition; the clipboard need not be open.
        unsafe { IsClipboardFormatAvailable(CF_HDROP.0.into()) }.is_ok()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clipboard_text_round_trips_as_utf16() {
        let w = wide_nul("Hello w\u{f6}rld \u{1F600}");
        assert_eq!(w.last(), Some(&0));
        assert_eq!(String::from_utf16_lossy(until_nul(&w)), "Hello w\u{f6}rld \u{1F600}");
        assert_eq!(until_nul(&[0x61, 0, 0x62]), &[0x61]);
        assert_eq!(until_nul(&[0x61, 0x62]), &[0x61, 0x62], "no NUL: the whole buffer");
    }
}
