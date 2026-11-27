//! Class icon extraction at the exact size (IC2): the icon of a file type, not of a file. Runs on a thread with COM
//! (the decor thread or the shell host's image STA), never on the UI thread.
// Decodes pixel data the system hands back: no unwrap, no expect, no indexing.
#![deny(clippy::unwrap_used, clippy::expect_used, clippy::indexing_slicing)]

use oled_ui::gfx::image::{resize_area, IconBitmap};
use windows::core::HSTRING;
use windows::Win32::Storage::FileSystem::{FILE_ATTRIBUTE_DIRECTORY, FILE_ATTRIBUTE_NORMAL, FILE_FLAGS_AND_ATTRIBUTES};
use windows::Win32::UI::Controls::{IImageList, ILD_TRANSPARENT};
use windows::Win32::UI::Shell::{
    SHDefExtractIconW, SHGetFileInfoW, SHGetImageList, SHFILEINFOW, SHGFI_ICONLOCATION, SHGFI_SYSICONINDEX,
    SHGFI_USEFILEATTRIBUTES,
};
use windows::Win32::UI::WindowsAndMessaging::{DestroyIcon, HICON};

use crate::decor::gdi::{self, Bgra};
use crate::decor::iconsbin::MAX_PX;

/// The system image lists, smallest first: (SHIL id, side). The sides are the classic ones; the real side of an
/// icon is read back from the bitmap, so a list that differs on this machine only costs a resize.
const LISTS: [(i32, u16); 4] = [(1, 16), (0, 32), (2, 48), (4, 256)];

/// The class icon of ext ("" is a folder) at px, premultiplied BGRA (px * px * 4 bytes), or None.
pub fn class_icon(ext: &str, px: u16) -> Option<Vec<u8>> {
    if px == 0 || px > MAX_PX || ext.contains(['.', '/', '\\', ':']) || ext.chars().any(char::is_control) {
        return None;
    }
    let (name, attrs) = if ext.is_empty() {
        ("folder".to_string(), FILE_ATTRIBUTE_DIRECTORY)
    } else {
        (format!(".{ext}"), FILE_ATTRIBUTE_NORMAL)
    };
    let name = HSTRING::from(name);
    // The shell can refuse a lookup for a moment when other threads or processes hit it at once: a few tries
    // with a short pause, so a type that has an icon never shows none for that reason.
    let mut found = None;
    for attempt in 0..LOOKUP_TRIES {
        if attempt > 0 {
            std::thread::sleep(std::time::Duration::from_millis(15 * u64::from(attempt)));
        }
        found = from_location(&name, attrs, px).or_else(|| from_image_list(&name, attrs, px));
        if found.is_some() {
            break;
        }
    }
    fit(found?, px)
}

/// How many times the class lookup is tried before it gives up.
const LOOKUP_TRIES: u32 = 6;

/// (1) The icon location of the class, extracted at exactly px.
fn from_location(name: &HSTRING, attrs: FILE_FLAGS_AND_ATTRIBUTES, px: u16) -> Option<Bgra> {
    let mut sfi = SHFILEINFOW::default();
    // SAFETY: a NUL-terminated name; sfi is writable and its size is passed.
    let r = unsafe {
        SHGetFileInfoW(
            name,
            attrs,
            Some(&mut sfi),
            std::mem::size_of::<SHFILEINFOW>() as u32,
            SHGFI_USEFILEATTRIBUTES | SHGFI_ICONLOCATION,
        )
    };
    if r == 0 {
        return None;
    }
    let n = sfi.szDisplayName.iter().position(|&u| u == 0).unwrap_or(sfi.szDisplayName.len());
    if n == 0 {
        return None;
    }
    let file = HSTRING::from_wide(sfi.szDisplayName.get(..n)?);
    let mut large = HICON::default();
    // SAFETY: a NUL-terminated location the shell just gave; one out icon, owned and destroyed in take_icon; the
    // small icon is not asked for.
    let hr = unsafe { SHDefExtractIconW(&file, sfi.iIcon, 0, Some(&mut large), None, u32::from(px)) };
    if hr.is_err() {
        return None;
    }
    take_icon(large)
}

/// (2) The system image list of exactly px if there is one, else the next larger list.
fn from_image_list(name: &HSTRING, attrs: FILE_FLAGS_AND_ATTRIBUTES, px: u16) -> Option<Bgra> {
    let mut sfi = SHFILEINFOW::default();
    // SAFETY: as in from_location.
    let r = unsafe {
        SHGetFileInfoW(
            name,
            attrs,
            Some(&mut sfi),
            std::mem::size_of::<SHFILEINFOW>() as u32,
            SHGFI_USEFILEATTRIBUTES | SHGFI_SYSICONINDEX,
        )
    };
    if r == 0 {
        return None;
    }
    let from = LISTS.iter().position(|&(_, side)| side >= px).unwrap_or(LISTS.len() - 1);
    for &(id, _) in LISTS.get(from..)? {
        // SAFETY: the requested interface matches the IID; COM is initialized on this thread.
        let Ok(list) = (unsafe { SHGetImageList::<IImageList>(id) }) else { continue };
        // SAFETY: a valid list and an index the shell gave for the system list.
        let Ok(icon) = (unsafe { list.GetIcon(sfi.iIcon, ILD_TRANSPARENT.0) }) else { continue };
        if let Some(b) = take_icon(icon) {
            return Some(b);
        }
    }
    None
}

/// Reads an icon this module owns and destroys it.
fn take_icon(icon: HICON) -> Option<Bgra> {
    if icon.is_invalid() {
        return None;
    }
    let b = gdi::hicon_to_bgra(icon);
    // SAFETY: an icon the shell made for this call, destroyed once.
    let _ = unsafe { DestroyIcon(icon) };
    b
}

/// The pixels as px x px bytes, reduced or enlarged with the dock's area resampler when the side differs.
fn fit(b: Bgra, px: u16) -> Option<Vec<u8>> {
    let side = u32::from(px);
    if b.w == side && b.h == side {
        return Some(gdi::to_bytes(&b).into_vec());
    }
    let src = IconBitmap { width: b.w, height: b.h, pixels: b.px };
    let out = resize_area(&src, side);
    if out.width != side || out.height != side || out.pixels.is_empty() {
        return None;
    }
    Some(gdi::to_bytes(&Bgra { w: out.width, h: out.height, px: out.pixels }).into_vec())
}
