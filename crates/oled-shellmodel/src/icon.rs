//! The icon sources of a program, in their order: the AppsFolder catalogue (an AUMID), `SHDefExtractIconW` and the
//! jumbo system image list (the exe), then the window's own icon. COM (STA) must be initialized on the calling
//! thread. The caller converts the result to its bitmap type (`IconDecode`) and caches it.

use std::ffi::c_void;

use windows::core::HSTRING;
use windows::Win32::Foundation::{HWND, LPARAM, SIZE, WPARAM};
use windows::Win32::Graphics::Gdi::{DeleteObject, HBITMAP, HGDIOBJ};
use windows::Win32::Storage::FileSystem::FILE_FLAGS_AND_ATTRIBUTES;
use windows::Win32::System::Com::IBindCtx;
use windows::Win32::UI::Controls::{IImageList, ILD_TRANSPARENT};
use windows::Win32::UI::Shell::{
    IShellItemImageFactory, SHCreateItemFromParsingName, SHDefExtractIconW, SHGetFileInfoW, SHGetImageList,
    SHFILEINFOW, SHGFI_SYSICONINDEX, SHIL_JUMBO, SIIGBF_ICONONLY,
};
use windows::Win32::UI::WindowsAndMessaging::{
    DestroyIcon, GetClassLongPtrW, SendMessageTimeoutW, GCLP_HICON, GCLP_HICONSM, HICON, ICON_BIG, ICON_SMALL2,
    SMTO_ABORTIFHUNG, WM_GETICON,
};

use crate::identity::is_usable_exe;

/// Side (px) requested from high-resolution icon sources.
pub const SRC_ICON_PX: i32 = 256;
/// `WM_GETICON` timeout towards other apps' windows.
pub const GETICON_TIMEOUT_MS: u32 = 50;

/// The caller's bitmap type (the dock's `IconBitmap`; Tasks' kit bitmap). Each method returns None for an empty or
/// invisible result. The crate destroys the icons and bitmaps it owns after the call; it never keeps one.
#[allow(clippy::wrong_self_convention)] // the names of the frozen interface (SM1)
pub trait IconDecode {
    type Bitmap;
    fn from_hicon(&self, icon: HICON) -> Option<Self::Bitmap>;
    fn from_hbitmap(&self, bmp: HBITMAP) -> Option<Self::Bitmap>;
}

/// What an icon is asked for: the exe, the AppUserModelID and the window of a program (0: no window).
#[derive(Clone, Copy, Debug)]
pub struct IconQuery<'a> {
    pub exe_path: &'a str,
    pub aumid: Option<&'a str>,
    pub hwnd: isize,
}

/// The sources in their order: AppsFolder (an AUMID), `SHDefExtractIconW`, the jumbo system image list (an exe that is
/// usable), then `WM_GETICON` and the class icon (a window). Gives the bitmap and whether it may be cached: the
/// window icon, low resolution and per-window, may not.
pub fn extract<D: IconDecode>(q: &IconQuery<'_>, d: &D) -> Option<(D::Bitmap, bool)> {
    order(q, |a| from_appsfolder(a, d), |e| from_def_extract(e, d), |e| from_jumbo_list(e, d), |h| from_window(h, d))
}

/// The order of `extract` over four sources, so that a test can run it without a Windows call (each closure is
/// inlined into `extract`, one copy per bitmap type).
fn order<B>(
    q: &IconQuery<'_>,
    appsfolder: impl Fn(&str) -> Option<B>,
    def_extract: impl Fn(&str) -> Option<B>,
    jumbo: impl Fn(&str) -> Option<B>,
    window: impl Fn(isize) -> Option<B>,
) -> Option<(B, bool)> {
    if let Some(a) = q.aumid.map(str::trim).filter(|a| !a.is_empty()) {
        if let Some(b) = appsfolder(a) {
            return Some((b, true));
        }
    }
    if is_usable_exe(q.exe_path) {
        if let Some(b) = def_extract(q.exe_path) {
            return Some((b, true));
        }
        // Fallback only: the jumbo system image list caches ~256 KB per index for the life of the process.
        if let Some(b) = jumbo(q.exe_path) {
            return Some((b, true));
        }
    }
    if q.hwnd != 0 {
        if let Some(b) = window(q.hwnd) {
            return Some((b, false));
        }
    }
    None
}

/// Converts an owned HICON (to be destroyed) into the caller's bitmap.
fn owned_hicon<D: IconDecode>(icon: HICON, d: &D) -> Option<D::Bitmap> {
    let b = d.from_hicon(icon);
    // SAFETY: `icon` was created for us (GetIcon/SHDefExtractIconW) and is no longer used.
    let _ = unsafe { DestroyIcon(icon) };
    b
}

/// `IShellItemImageFactory::GetImage(256, SIIGBF_ICONONLY)` on `shell:AppsFolder\<AUMID>`.
pub fn from_appsfolder<D: IconDecode>(aumid: &str, d: &D) -> Option<D::Bitmap> {
    let path = HSTRING::from(format!("shell:AppsFolder\\{aumid}"));
    // SAFETY: COM STA initialized; `path` lives for the call.
    let item: IShellItemImageFactory = unsafe { SHCreateItemFromParsingName(&path, None::<&IBindCtx>) }.ok()?;
    // SAFETY: `item` is valid; the returned HBITMAP is ours and is deleted below.
    let hbm = unsafe { item.GetImage(SIZE { cx: SRC_ICON_PX, cy: SRC_ICON_PX }, SIIGBF_ICONONLY) }.ok()?;
    let b = d.from_hbitmap(hbm);
    // SAFETY: bitmap owned by us, not selected into any DC.
    let _ = unsafe { DeleteObject(HGDIOBJ(hbm.0)) };
    b
}

/// `SHGetFileInfoW(SHGFI_SYSICONINDEX)` + `SHGetImageList(SHIL_JUMBO)` + `GetIcon(ILD_TRANSPARENT)`,
/// discarding `iIcon == 0` (generic document icon). The jumbo list keeps ~256 KB per index for the life of the process:
/// a fallback only.
pub fn from_jumbo_list<D: IconDecode>(exe_path: &str, d: &D) -> Option<D::Bitmap> {
    let path = HSTRING::from(exe_path);
    let mut sfi = SHFILEINFOW::default();
    // SAFETY: `path` NUL-terminated for the duration of the call; `sfi` writable with its size.
    let ok = unsafe {
        SHGetFileInfoW(
            &path,
            FILE_FLAGS_AND_ATTRIBUTES(0),
            Some(&mut sfi),
            std::mem::size_of::<SHFILEINFOW>() as u32,
            SHGFI_SYSICONINDEX,
        )
    };
    if ok == 0 || sfi.iIcon == 0 {
        return None;
    }
    // SAFETY: COM STA initialized; the requested interface matches the IID.
    let list: IImageList = unsafe { SHGetImageList(SHIL_JUMBO as i32) }.ok()?;
    // SAFETY: `list` is valid; index obtained from SHGetFileInfoW on the same system list.
    let icon = unsafe { list.GetIcon(sfi.iIcon, ILD_TRANSPARENT.0) }.ok()?;
    owned_hicon(icon, d)
}

/// `SHDefExtractIconW(exe, 0, 0, 256)`: extracts the resource closest to 256 px.
pub fn from_def_extract<D: IconDecode>(exe_path: &str, d: &D) -> Option<D::Bitmap> {
    let path = HSTRING::from(exe_path);
    let mut large = HICON::default();
    // SAFETY: `path` valid for the call; `large` is writable; small icon not requested.
    let hr = unsafe { SHDefExtractIconW(&path, 0, 0, Some(&mut large), None, SRC_ICON_PX as u32) };
    if hr.is_err() || large.is_invalid() {
        return None;
    }
    owned_hicon(large, d)
}

/// `WM_GETICON` (with timeout, ICON_BIG then ICON_SMALL2), then the class icon.
/// The HICONs remain owned by the window/class: they must NOT be destroyed.
pub fn from_window<D: IconDecode>(hwnd: isize, d: &D) -> Option<D::Bitmap> {
    let hwnd = HWND(hwnd as *mut c_void);
    for kind in [ICON_BIG, ICON_SMALL2] {
        let mut res = 0usize;
        // SAFETY: SendMessageTimeoutW with SMTO_ABORTIFHUNG and a short timeout: it cannot block for
        // long; `res` is writable. The shell thread is not the thread of any dock window.
        let r = unsafe {
            SendMessageTimeoutW(
                hwnd,
                WM_GETICON,
                WPARAM(kind as usize),
                LPARAM(0),
                SMTO_ABORTIFHUNG,
                GETICON_TIMEOUT_MS,
                Some(&mut res),
            )
        };
        if r.0 != 0 && res != 0 {
            if let Some(b) = d.from_hicon(HICON(res as *mut c_void)) {
                return Some(b);
            }
        }
    }
    for idx in [GCLP_HICON, GCLP_HICONSM] {
        // SAFETY: reads a class attribute; if the window is gone it returns 0.
        let v = unsafe { GetClassLongPtrW(hwnd, idx) };
        if v != 0 {
            if let Some(b) = d.from_hicon(HICON(v as *mut c_void)) {
                return Some(b);
            }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;

    /// Runs the order over sources that record their call and answer from `answers` (AppsFolder, SHDefExtractIconW,
    /// jumbo list, window).
    fn run(q: &IconQuery<'_>, answers: [bool; 4]) -> (Option<(u32, bool)>, Vec<String>) {
        let calls = RefCell::new(Vec::new());
        let ask = |name: &str, arg: String, yes: bool| {
            calls.borrow_mut().push(format!("{name}:{arg}"));
            yes.then_some(7u32)
        };
        let got = order(
            q,
            |a| ask("appsfolder", a.to_string(), answers[0]),
            |e| ask("def_extract", e.to_string(), answers[1]),
            |e| ask("jumbo", e.to_string(), answers[2]),
            |h| ask("window", h.to_string(), answers[3]),
        );
        (got, calls.into_inner())
    }

    #[test]
    fn icon_order_is_appsfolder_then_exe_then_window() {
        let exe = r"C:\Program Files\Contoso\app.exe";
        let q = IconQuery { exe_path: exe, aumid: Some("Contoso.App!App"), hwnd: 5 };
        // Every source empty: all of them are asked, in the order of the plan.
        let (got, calls) = run(&q, [false; 4]);
        assert_eq!(got, None);
        assert_eq!(
            calls,
            [
                "appsfolder:Contoso.App!App".to_string(),
                format!("def_extract:{exe}"),
                format!("jumbo:{exe}"),
                "window:5".into()
            ]
        );
        // The first that answers ends the walk; only the window's icon is not cacheable.
        assert_eq!(run(&q, [true; 4]), (Some((7, true)), vec!["appsfolder:Contoso.App!App".to_string()]));
        assert_eq!(run(&q, [false, true, true, true]).1.len(), 2);
        assert_eq!(run(&q, [false, false, true, true]).1.len(), 3);
        assert_eq!(run(&q, [false, false, true, true]).0, Some((7, true)));
        assert_eq!(run(&q, [false, false, false, true]), (Some((7, false)), run(&q, [false; 4]).1));
    }

    #[test]
    fn sources_that_do_not_apply_are_not_asked() {
        // No AUMID (or a blank one): no catalogue call. No usable exe: no exe source. No window: no window call.
        let q = IconQuery { exe_path: "", aumid: Some("  "), hwnd: 0 };
        assert_eq!(run(&q, [true; 4]), (None, vec![]));
        let host = IconQuery { exe_path: r"C:\Windows\System32\ApplicationFrameHost.exe", aumid: None, hwnd: 9 };
        assert_eq!(run(&host, [true; 4]), (Some((7, false)), vec!["window:9".to_string()]));
        let exe = IconQuery { exe_path: r"C:\x\a.exe", aumid: None, hwnd: 0 };
        assert_eq!(run(&exe, [true; 4]).1, [r"def_extract:C:\x\a.exe"]);
    }
}
