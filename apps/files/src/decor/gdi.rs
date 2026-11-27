//! HICON and HBITMAP to premultiplied BGRA (GD1): a port of the dock's reader, not a move (the dock keeps its own).
// Decodes pixel data the system hands back: no unwrap, no expect, no indexing.
#![deny(clippy::unwrap_used, clippy::expect_used, clippy::indexing_slicing)]

use std::ffi::c_void;
use std::mem::size_of;

use oled_sys::log;
use oled_ui::gfx::image::premultiply_px;
use windows::Win32::Graphics::Gdi::{
    DeleteObject, GetDC, GetDIBits, GetObjectW, ReleaseDC, BITMAP, BITMAPINFO, BITMAPINFOHEADER, BI_RGB,
    DIB_RGB_COLORS, HBITMAP, HGDIOBJ,
};
use windows::Win32::UI::WindowsAndMessaging::{GetIconInfoExW, HICON, ICONINFOEXW};

/// The largest source side read (as the dock's reader).
pub const MAX_SRC_SIDE: u32 = 2048;

/// Premultiplied 0xAARRGGBB pixels, row by row (the layout of oled_ui's IconBitmap pixels).
pub struct Bgra {
    pub w: u32,
    pub h: u32,
    pub px: Vec<u32>,
}

/// Size of a bitmap as (width, height) in pixels, None when it is invalid or larger than MAX_SRC_SIDE.
fn bitmap_size(hbm: HBITMAP) -> Option<(i32, i32)> {
    if hbm.0.is_null() {
        return None;
    }
    let mut bm = BITMAP::default();
    // SAFETY: `hbm` is a valid HBITMAP owned by the caller; `bm` is writable and as large as declared in the
    // `c` parameter.
    let n =
        unsafe { GetObjectW(HGDIOBJ(hbm.0), size_of::<BITMAP>() as i32, Some(&mut bm as *mut BITMAP as *mut c_void)) };
    if n == 0 || bm.bmWidth <= 0 || bm.bmHeight == 0 {
        return None;
    }
    let h = bm.bmHeight.abs();
    let side = MAX_SRC_SIDE as i32;
    if bm.bmWidth > side || h > side {
        return None;
    }
    Some((bm.bmWidth, h))
}

/// Reads an HBITMAP as a top-down 32-bit DIB (each u32 = B | G<<8 | R<<16 | A<<24, i.e. 0xAARRGGBB). `h` is the
/// number of rows to read. The HBITMAP must not be selected into a DC.
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
    // SAFETY: GetDC(None) returns the screen DC, released right below with ReleaseDC. `buf` holds exactly w*h u32 =
    // w*h*4 bytes, as much as GetDIBits writes for `h` 32-bit rows; `bmi` is valid.
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

/// Does not destroy `icon`. Native size; alpha all 0 in the color bitmap -> the AND mask gives the alpha.
pub fn hicon_to_bgra(icon: HICON) -> Option<Bgra> {
    if icon.is_invalid() {
        return None;
    }
    let mut info = ICONINFOEXW { cbSize: size_of::<ICONINFOEXW>() as u32, ..Default::default() };
    // SAFETY: `icon` is a valid HICON (the caller owns it); `info` has cbSize set and lives for the whole call.
    if !unsafe { GetIconInfoExW(icon, &mut info) }.as_bool() {
        log!("files gdi: GetIconInfoExW failed");
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
fn color_icon(color: HBITMAP, mask: HBITMAP) -> Option<Bgra> {
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
    Some(Bgra { w: w as u32, h: h as u32, px })
}

/// Monochrome icon (no color bitmap): the mask is twice as tall (AND on top, XOR below).
fn monochrome_icon(mask: HBITMAP) -> Option<Bgra> {
    let (w, h2) = bitmap_size(mask)?;
    let h = h2 / 2;
    if h <= 0 {
        return None;
    }
    let m = read_dib32(mask, w, h2)?;
    let n = (w as usize) * (h as usize);
    let (and_rows, xor_rows) = m.split_at_checked(n)?;
    let px: Vec<u32> = and_rows
        .iter()
        .zip(xor_rows.iter())
        .map(|(a, x)| match (a & 0x00FF_FFFF != 0, x & 0x00FF_FFFF != 0) {
            (false, false) => 0xFF00_0000,
            (false, true) => 0xFFFF_FFFF,
            (true, true) => 0xFFFF_FFFF, // "invert": approximated with white
            (true, false) => 0,
        })
        .collect();
    Some(Bgra { w: w as u32, h: h as u32, px })
}

/// Does not destroy `bmp`; a bitmap without real alpha is opaque.
pub fn hbitmap_to_bgra(bmp: HBITMAP) -> Option<Bgra> {
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
    Some(Bgra { w: w as u32, h: h as u32, px })
}

/// Little-endian BGRA bytes, for Pixels and IconRef.
pub fn to_bytes(b: &Bgra) -> Box<[u8]> {
    b.px.iter().flat_map(|p| p.to_le_bytes()).collect()
}
