//! Icon bitmaps: HICON/HBITMAP -> premultiplied pixel conversion, hand-written area (box)
//! filter resizing, PNG on disk via WIC (decode/encode) and cache.
//!
//! THREADING: ALL functions in this module that touch GDI/COM/WIC must be called from the
//! shell thread (COM STA already initialized). `IconBitmap`, `sanitize_key`, `resize_area`,
//! `cache_path`, `is_plain_icon_name` are pure/thread-safe and usable anywhere.
//!
//! FORMAT: `IconBitmap::pixels` is a row-major, top-down `Vec<u32>`, each pixel 0xAARRGGBB
//! PREMULTIPLIED (the same layout as the 32-bit DIB of `render`, so blending is a direct
//! read). On disk the PNGs are NOT premultiplied (converted on read/write).
//!
//! FILE I/O: every file is read into memory through `datadir` (no reparse point followed, size cap)
//! and decoded from memory; PNGs are encoded into memory and written with `datadir::write_atomic`.
//! WIC never opens a path itself.
//!
//! CUSTOM ICONS: `%ProgramData%\mini-oled-dock\icons\<name>`; the user can create files there, so
//! `datadir::read_icon` checks the handle (regular file, one link, final path inside the folder).
//!
//! DISK CACHE: `%ProgramData%\mini-oled-dock\cache\<sanitized-key>.png` (admin-only), always
//! `MASTER_PX` x `MASTER_PX` (64). The UI then asks for the final size
//! (24 logical * dpi/96) and `resize_area` does the downscale. Invalidated if the exe mtime is
//! newer than the cached PNG; if the exe mtime is unknown (UWP apps) the cache
//! expires after `CACHE_MAX_AGE_NO_MTIME` (7 days).

use std::ffi::c_void;
use std::mem::size_of;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use windows::core::HSTRING;
use windows::Win32::Foundation::SIZE;
use windows::Win32::Graphics::Gdi::{
    DeleteObject, GetDC, GetDIBits, GetObjectW, ReleaseDC, BITMAP, BITMAPINFO, BITMAPINFOHEADER, BI_RGB,
    DIB_RGB_COLORS, HBITMAP, HGDIOBJ,
};
use windows::Win32::System::Com::IBindCtx;
use windows::Win32::UI::Shell::{IShellItemImageFactory, SHCreateItemFromParsingName, SIIGBF_ICONONLY};
use windows::Win32::UI::WindowsAndMessaging::{GetIconInfoExW, HICON, ICONINFOEXW};

use crate::log;

use oled_ui::gfx::image::premultiply_px;
pub use oled_ui::gfx::image::{resize_area, resize_is_identity, IconBitmap};
pub use oled_ui::gfx::wic::decode_image_bytes;
use oled_ui::gfx::wic::{encodable, encode_png};

/// Side (px) of the master PNG in the disk cache.
pub const MASTER_PX: u32 = 64;

/// Cache expiry when the exe mtime is unknown (UWP packages).
const CACHE_MAX_AGE_NO_MTIME: Duration = Duration::from_secs(7 * 24 * 3600);
/// Maximum accepted side for a source HICON/HBITMAP.
const MAX_SRC_SIDE: i32 = 2048;

/// Turns a grouping key into a valid file name: characters outside
/// `[A-Za-z0-9._-]` replaced by `_`, lowercase, max 120 characters.
/// E.g. `"C:\\Program Files\\x.exe"` -> `"c__program_files_x.exe"`.
pub fn sanitize_key(key: &str) -> String {
    let s: String = key
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() || c == '.' || c == '-' { c.to_ascii_lowercase() } else { '_' })
        .take(120)
        .collect();
    if s.is_empty() {
        "_".to_string()
    } else {
        s
    }
}

/// Path of the cache PNG for `key` (`<cache>/<sanitize_key(key)>.png`); `None` without a cache folder.
pub fn cache_path(key: &str) -> Option<PathBuf> {
    crate::datadir::cache_dir().map(|d| d.join(format!("{}.png", sanitize_key(key))))
}

/// The user's custom icon for `key`, decoded: if `configured` (the pin's `icon` field) is a valid
/// plain file name (`is_plain_icon_name`), `icons\<configured>`; otherwise (or if that is missing)
/// `icons\<sanitize_key(key)>.png`. Shell thread (WIC).
///
/// SECURITY: the config is untrusted input (the user edits it; it used to be writable by any
/// medium process), so `configured` must be a plain name: absolute, UNC, `..` or ADS paths are
/// rejected. The file itself is read by `datadir::read_icon`, which refuses reparse points, hard
/// links and anything whose final path is not exactly inside the icons folder, and caps the size;
/// the bytes are then decoded from memory.
pub fn load_custom_icon(key: &str, configured: Option<&str>) -> Option<IconBitmap> {
    if let Some(c) = configured.map(str::trim).filter(|s| !s.is_empty()) {
        if !is_plain_icon_name(c) {
            crate::log!("icons: custom icon '{c}' rejected: only a plain file name inside the icons folder is allowed");
        } else if let Some(b) = read_custom(c) {
            return Some(b);
        }
    }
    let by_key = format!("{}.png", sanitize_key(key));
    if is_plain_icon_name(&by_key) {
        read_custom(&by_key)
    } else {
        None
    }
}

/// Reads and decodes `icons\<name>`; a missing file is silent, anything else is logged.
fn read_custom(name: &str) -> Option<IconBitmap> {
    match crate::datadir::read_icon(name) {
        Ok(bytes) => {
            let b = decode_image_bytes(&bytes);
            if b.is_none() {
                log!("icons: custom icon '{name}' could not be decoded, using the extracted one");
            }
            b
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
        Err(e) => {
            log!("icons: custom icon '{name}' refused: {e}");
            None
        }
    }
}

/// Windows reserved device names (with or without extension they never name a regular file).
const RESERVED_NAMES: [&str; 22] = [
    "CON", "PRN", "AUX", "NUL", "COM1", "COM2", "COM3", "COM4", "COM5", "COM6", "COM7", "COM8", "COM9", "LPT1", "LPT2",
    "LPT3", "LPT4", "LPT5", "LPT6", "LPT7", "LPT8", "LPT9",
];

/// true if `name` is a single, plain file name: one `Normal` path component, no separators
/// (`\` or `/`), no `.`/`..`, no drive/UNC/verbatim prefix, no `:` (drive or alternate data
/// stream), no wildcard/control characters, no trailing dot or space (Win32 strips them),
/// not a reserved device name, at most 255 characters. Pure.
pub fn is_plain_icon_name(name: &str) -> bool {
    use std::path::Component;
    if name.is_empty() || name.len() > 255 || name.ends_with(['.', ' ']) {
        return false;
    }
    if name.chars().any(|c| c.is_control() || matches!(c, '\\' | '/' | ':' | '*' | '?' | '"' | '<' | '>' | '|')) {
        return false;
    }
    let mut comps = Path::new(name).components();
    if !matches!((comps.next(), comps.next()), (Some(Component::Normal(_)), None)) {
        return false;
    }
    let stem = name.split('.').next().unwrap_or(name).trim_end();
    !RESERVED_NAMES.iter().any(|r| r.eq_ignore_ascii_case(stem))
}

// ------------------------------------------------------------------ GDI -> IconBitmap

/// Size (width, height) of an HBITMAP, `None` if invalid or too large.
fn bitmap_size(hbm: HBITMAP) -> Option<(i32, i32)> {
    if hbm.0.is_null() {
        return None;
    }
    let mut bm = BITMAP::default();
    // SAFETY: `hbm` is a valid HBITMAP owned by the caller; `bm` is writable and as large
    // as declared in the `c` parameter.
    let n =
        unsafe { GetObjectW(HGDIOBJ(hbm.0), size_of::<BITMAP>() as i32, Some(&mut bm as *mut BITMAP as *mut c_void)) };
    if n == 0 || bm.bmWidth <= 0 || bm.bmHeight == 0 {
        return None;
    }
    let h = bm.bmHeight.abs();
    if bm.bmWidth > MAX_SRC_SIDE || h > MAX_SRC_SIDE {
        return None;
    }
    Some((bm.bmWidth, h))
}

/// Reads an HBITMAP as a top-down 32-bit DIB (each u32 = B | G<<8 | R<<16 | A<<24, i.e.
/// 0xAARRGGBB). `h` is the number of rows to read. The HBITMAP must not be selected into a DC.
fn read_dib32(hbm: HBITMAP, w: i32, h: i32) -> Option<Vec<u32>> {
    if w <= 0 || h <= 0 {
        return None;
    }
    let mut buf = vec![0u32; (w as usize) * (h as usize)];
    let mut bmi = BITMAPINFO {
        bmiHeader: BITMAPINFOHEADER {
            biSize: size_of::<BITMAPINFOHEADER>() as u32,
            biWidth: w,
            biHeight: -h, // negative = top-down
            biPlanes: 1,
            biBitCount: 32,
            biCompression: BI_RGB.0,
            ..Default::default()
        },
        ..Default::default()
    };
    // SAFETY: GetDC(None) returns the screen DC, released right below with ReleaseDC. `buf`
    // holds exactly w*h u32 = w*h*4 bytes, as much as GetDIBits writes for `h` 32-bit rows; `bmi` is valid.
    let lines = unsafe {
        let hdc = GetDC(None);
        if hdc.is_invalid() {
            return None;
        }
        let n = GetDIBits(hdc, hbm, 0, h as u32, Some(buf.as_mut_ptr() as *mut c_void), &mut bmi, DIB_RGB_COLORS);
        ReleaseDC(None, hdc);
        n
    };
    if lines == 0 {
        None
    } else {
        Some(buf)
    }
}

fn delete_bitmap(hbm: HBITMAP) {
    if !hbm.0.is_null() {
        // SAFETY: bitmap created by GetIconInfoExW and owned by us; it is not selected into any DC.
        let _ = unsafe { DeleteObject(HGDIOBJ(hbm.0)) };
    }
}

/// HICON -> premultiplied bitmap at the icon's native size (`GetIconInfoExW` +
/// `GetDIBits`; if the alpha channel is all 0 it uses the AND mask, otherwise it normalizes).
/// Shell thread. Does not destroy `icon` (ownership stays with the caller).
pub fn hicon_to_bitmap(icon: HICON) -> Option<IconBitmap> {
    if icon.is_invalid() {
        return None;
    }
    let mut info = ICONINFOEXW { cbSize: size_of::<ICONINFOEXW>() as u32, ..Default::default() };
    // SAFETY: `icon` is a valid HICON (the caller owns it or read it from a
    // window); `info` has cbSize set and lives for the whole call.
    if !unsafe { GetIconInfoExW(icon, &mut info) }.as_bool() {
        log!("icons::hicon_to_bitmap: GetIconInfoExW failed");
        return None;
    }
    let (color, mask) = (info.hbmColor, info.hbmMask);
    let out = if color.0.is_null() { monochrome_icon(mask) } else { color_icon(color, mask) };
    // GetIconInfoExW creates two new bitmaps: we must delete them ourselves.
    delete_bitmap(color);
    delete_bitmap(mask);
    out
}

/// Color icon: pixels from the color bitmap; if it has no real alpha, alpha comes from the AND mask.
fn color_icon(color: HBITMAP, mask: HBITMAP) -> Option<IconBitmap> {
    let (w, h) = bitmap_size(color)?;
    let mut px = read_dib32(color, w, h)?;
    if px.iter().any(|p| p >> 24 != 0) {
        // Real alpha (not premultiplied in GDI icons): premultiply.
        for p in px.iter_mut() {
            *p = premultiply_px(*p);
        }
    } else {
        // No alpha: AND mask (bit 1 = transparent). GetDIBits expands it to 0x000000/0xFFFFFF.
        let m = match bitmap_size(mask) {
            Some((mw, mh)) if mw == w && mh >= h => read_dib32(mask, mw, mh),
            _ => None,
        };
        match m {
            Some(m) => {
                for (p, mk) in px.iter_mut().zip(m.iter()) {
                    *p = if mk & 0x00FF_FFFF == 0 { 0xFF00_0000 | (*p & 0x00FF_FFFF) } else { 0 };
                }
            }
            None => {
                for p in px.iter_mut() {
                    *p |= 0xFF00_0000;
                }
            }
        }
    }
    Some(IconBitmap { width: w as u32, height: h as u32, pixels: px })
}

/// Monochrome icon (no color bitmap): the mask is twice as tall (AND on top, XOR below).
fn monochrome_icon(mask: HBITMAP) -> Option<IconBitmap> {
    let (w, h2) = bitmap_size(mask)?;
    let h = h2 / 2;
    if h <= 0 {
        return None;
    }
    let m = read_dib32(mask, w, h2)?;
    let n = (w as usize) * (h as usize);
    let mut px = vec![0u32; n];
    for i in 0..n {
        let and = m[i] & 0x00FF_FFFF != 0;
        let xor = m[n + i] & 0x00FF_FFFF != 0;
        px[i] = match (and, xor) {
            (false, false) => 0xFF00_0000,
            (false, true) => 0xFFFF_FFFF,
            (true, true) => 0xFFFF_FFFF, // "invert": approximated with white
            (true, false) => 0,
        };
    }
    Some(IconBitmap { width: w as u32, height: h as u32, pixels: px })
}

/// 32-bit HBITMAP (e.g. from `IShellItemImageFactory::GetImage`) -> premultiplied bitmap.
/// If the HBITMAP has no real alpha (all alpha pixels 0) it is assumed opaque. Does not destroy `bmp`.
pub fn hbitmap_to_bitmap(bmp: HBITMAP) -> Option<IconBitmap> {
    let (w, h) = bitmap_size(bmp)?;
    let mut px = read_dib32(bmp, w, h)?;
    if px.iter().any(|p| p >> 24 != 0) {
        for p in px.iter_mut() {
            *p = premultiply_px(*p);
        }
    } else {
        for p in px.iter_mut() {
            *p |= 0xFF00_0000;
        }
    }
    Some(IconBitmap { width: w as u32, height: h as u32, pixels: px })
}

// ------------------------------------------------------------------ WIC: PNG

/// Writes `bmp` as a PNG file (encoded in memory, then `datadir::write_atomic`: new temp file +
/// rename). Creates missing folders. Used by `--preview` (a folder chosen by the user). Shell thread.
pub fn save_png(path: &Path, bmp: &IconBitmap) -> bool {
    if !encodable(bmp) {
        return false;
    }
    if let Some(dir) = path.parent() {
        if let Err(e) = std::fs::create_dir_all(dir) {
            log!("icons::save_png: create_dir_all({}): {e}", dir.display());
            return false;
        }
    }
    let res =
        encode_png(bmp).and_then(|bytes| crate::datadir::write_atomic(path, &bytes, crate::datadir::FileAcl::Inherit));
    if let Err(e) = &res {
        log!("icons::save_png({}): {e}", path.display());
    }
    res.is_ok()
}

// ------------------------------------------------------------------ disk cache

/// Looks up `key` in the disk cache. Valid only if `exe_mtime` (mtime of the exe/package, if
/// known) is NOT newer than the cached PNG; if unknown, only if the PNG is under 7 days old.
/// Returns the already-loaded master PNG (`MASTER_PX`). Shell thread.
pub fn cache_load(key: &str, exe_mtime: Option<SystemTime>) -> Option<IconBitmap> {
    let path = cache_path(key)?;
    let png_mtime = file_mtime(&path)?;
    match exe_mtime {
        Some(m) if m > png_mtime => return None,
        Some(_) => {}
        None => {
            if SystemTime::now().duration_since(png_mtime).map(|d| d > CACHE_MAX_AGE_NO_MTIME).unwrap_or(false) {
                return None;
            }
        }
    }
    let bytes = crate::datadir::read_file(&path, crate::datadir::CACHE_MAX_BYTES, true)
        .map_err(|e| {
            if e.kind() != std::io::ErrorKind::NotFound {
                log!("icons::cache_load({}): {e}", path.display());
            }
        })
        .ok()?;
    let bmp = decode_image_bytes(&bytes)?;
    if bmp.width != MASTER_PX || bmp.height != MASTER_PX {
        log!("icons::cache_load: {} is not {MASTER_PX}x{MASTER_PX}, discarded", path.display());
        return None;
    }
    Some(bmp)
}

/// Saves the master `bmp` (see `MASTER_PX`) in the cache for `key`. Errors are only logged.
/// Shell thread.
pub fn cache_store(key: &str, bmp: &IconBitmap) {
    if !crate::datadir::writable() || !encodable(bmp) {
        return;
    }
    let Some(path) = cache_path(key) else { return };
    // The cache folder is admin-only (created and verified by `datadir`): the PNG inherits its DACL.
    let res =
        encode_png(bmp).and_then(|bytes| crate::datadir::write_atomic(&path, &bytes, crate::datadir::FileAcl::Inherit));
    if let Err(e) = res {
        log!("icons::cache_store: saving '{key}' failed: {e}");
    }
}

/// mtime of a file (None if missing/unreadable). Useful for `cache_load`.
pub fn file_mtime(path: &Path) -> Option<SystemTime> {
    std::fs::metadata(path).ok()?.modified().ok()
}

#[cfg(test)]
#[allow(clippy::items_after_test_module)] // clippy: kept as is; moving the items below the tests is churn in a file other streams own
mod tests {
    use super::*;

    #[test]
    fn plain_icon_names() {
        for ok in ["discord.png", "my icon.png", "a.b.c.png", "x", "ÿ-ünicode.png"] {
            assert!(is_plain_icon_name(ok), "{ok}");
        }
        for bad in [
            "",
            ".",
            "..",
            "..\\x.png",
            "../x.png",
            "sub\\x.png",
            "sub/x.png",
            "C:\\Windows\\x.png",
            "C:x.png",
            "\\\\server\\share\\x.png",
            "\\\\?\\C:\\x.png",
            "\\x.png",
            "x.png:stream",
            "x.png::$DATA",
            "x.png.",
            "x.png ",
            "*.png",
            "a\u{0}b.png",
            "CON",
            "nul.png",
            "com1.txt",
        ] {
            assert!(!is_plain_icon_name(bad), "{bad:?}");
        }
        assert!(!is_plain_icon_name(&"a".repeat(256)));
    }

    #[test]
    fn sanitize() {
        assert_eq!(sanitize_key("C:\\Program Files\\x.exe"), "c__program_files_x.exe");
    }

    #[test]
    fn png_roundtrip_via_wic() {
        use windows::Win32::System::Com::{CoInitializeEx, CoUninitialize, COINIT_APARTMENTTHREADED};
        // SAFETY: COM for the test thread only.
        let hr = unsafe { CoInitializeEx(None, COINIT_APARTMENTTHREADED) };
        let mut s = IconBitmap::new(64, 64);
        for y in 0..64usize {
            for x in 0..64usize {
                s.pixels[y * 64 + x] =
                    premultiply_px(0x8000_0000 | ((x as u32 * 4) << 16) | ((y as u32 * 4) << 8) | 0x40);
            }
        }
        let path = std::env::temp_dir().join("dock_icons_test_roundtrip.png");
        assert!(save_png(&path, &s));
        let bytes = crate::datadir::read_file(&path, crate::datadir::CACHE_MAX_BYTES, true).expect("read");
        let back = decode_image_bytes(&bytes).expect("decode");
        let _ = std::fs::remove_file(&path);
        assert_eq!((back.width, back.height), (64, 64));
        for (a, b) in s.pixels.iter().zip(back.pixels.iter()) {
            assert_eq!(a >> 24, b >> 24);
            for sh in [0, 8, 16] {
                assert!(((a >> sh) & 255).abs_diff((b >> sh) & 255) <= 2);
            }
        }
        if hr.is_ok() {
            // SAFETY: paired with the CoInitializeEx above.
            unsafe { CoUninitialize() };
        }
    }
}

// ------------------------------------------------------------------ notification icons

/// Icon of an app from the `shell:AppsFolder\<AUMID>` catalog (`IShellItemImageFactory`, `px` x `px`).
/// Requires COM initialized on the calling thread. `None` if the AUMID does not exist or the icon is empty.
pub fn appsfolder_icon(aumid: &str, px: i32) -> Option<IconBitmap> {
    let path = HSTRING::from(format!("shell:AppsFolder\\{aumid}"));
    // SAFETY: COM initialized by the caller; `path` lives for the call.
    let item: IShellItemImageFactory = unsafe { SHCreateItemFromParsingName(&path, None::<&IBindCtx>) }.ok()?;
    // SAFETY: valid `item`; the returned HBITMAP is ours and is deleted below.
    let hbm = unsafe { item.GetImage(SIZE { cx: px, cy: px }, SIIGBF_ICONONLY) }.ok()?;
    let b = hbitmap_to_bitmap(hbm).filter(IconBitmap::has_visible_pixels);
    // SAFETY: bitmap owned by us, not selected into any DC.
    let _ = unsafe { DeleteObject(HGDIOBJ(hbm.0)) };
    b
}
