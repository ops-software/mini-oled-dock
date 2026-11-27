//! Icons and thumbnails in the shell host (SH9): per item, IShellItemImageFactory::GetImage of the asked size inside a
//! crumbs guard, the bitmap read to premultiplied BGRA (decor::gdi), one Icon or Thumb frame per item that worked,
//! then ReqDone. Work of a generation that CancelGen dropped stops between items. A bitmap larger than
//! MAX_PIXELS_SIDE is skipped (GetImage returns the asked size; a larger one is a broken handler). In a debug build
//! with FILES_FAULT_DLL and the fault point "icon" or "thumb", the fault DLL's FaultIcon::Extract or
//! FaultThumb::GetThumbnail runs in the same guard instead.

use oled_trust::frame::files::{CallKind, Gen, ItemRef, Pixels, ReqId, ShellEvt, WidePath, MAX_PIXELS_SIDE};
use windows::core::{Interface, HSTRING};
use windows::Win32::Foundation::SIZE;
use windows::Win32::Graphics::Gdi::{DeleteObject, HBITMAP, HGDIOBJ};
use windows::Win32::System::Com::{IBindCtx, STGM_READ};
use windows::Win32::UI::Shell::{
    IExtractIconW, IInitializeWithItem, IShellItem, IShellItemImageFactory, IThumbnailProvider,
    SHCreateItemFromParsingName, SIIGBF, SIIGBF_ICONONLY, SIIGBF_INCACHEONLY, SIIGBF_THUMBNAILONLY, WTS_ALPHATYPE,
};
use windows::Win32::UI::WindowsAndMessaging::{DestroyIcon, HICON};

use crate::decor::gdi::{self, Bgra};
use crate::shell::crumbs;
use crate::shell::host::{self, fault, HostCfg};
use crate::shell::writer::Writer;

/// Icons (thumbs None) or thumbnails (Some(cache_only)) of the items, then ReqDone.
pub fn run(req: ReqId, gen: Gen, items: &[ItemRef], px: u16, thumbs: Option<bool>, w: &Writer, cfg: &HostCfg) {
    let kind = if thumbs.is_some() { CallKind::Thumb } else { CallKind::Icon };
    for (index, item) in items.iter().enumerate() {
        if host::cancelled(gen) {
            break;
        }
        let ItemRef::Path(p) = item else { continue };
        let ext = crumbs::ext_of(p);
        let img = {
            let _g = crumbs::enter(kind, &ext);
            image_of(p, px, thumbs, cfg)
        };
        let Some(b) = img else { continue };
        let (Ok(bw), Ok(bh)) = (u16::try_from(b.w), u16::try_from(b.h)) else { continue };
        if bw == 0 || bh == 0 || bw > MAX_PIXELS_SIDE || bh > MAX_PIXELS_SIDE {
            continue;
        }
        let pixels = Pixels { w: bw, h: bh, bgra: gdi::to_bytes(&b) };
        let index = index as u32;
        w.send(&match thumbs {
            Some(_) => ShellEvt::Thumb { req, gen, index, px: pixels },
            None => ShellEvt::Icon { req, gen, index, px: pixels },
        });
    }
    w.send(&ShellEvt::ReqDone { req });
}

/// The image of one item, or None (no image, a failure).
fn image_of(p: &WidePath, px: u16, thumbs: Option<bool>, cfg: &HostCfg) -> Option<Bgra> {
    let point = if thumbs.is_some() { "thumb" } else { "icon" };
    if let (Some(dll), Some(true)) = (&cfg.fault_dll, cfg.fault_point.as_deref().map(|f| f == point)) {
        return if thumbs.is_some() { fault_thumb(dll, p, px) } else { fault_icon(dll, px) };
    }
    let name = HSTRING::from(p.to_string_lossy());
    // SAFETY: a NUL-terminated path; no bind context.
    let factory: IShellItemImageFactory = unsafe { SHCreateItemFromParsingName(&name, None::<&IBindCtx>) }.ok()?;
    let flags = match thumbs {
        None => SIIGBF_ICONONLY,
        Some(false) => SIIGBF_THUMBNAILONLY,
        Some(true) => SIIGBF(SIIGBF_THUMBNAILONLY.0 | SIIGBF_INCACHEONLY.0),
    };
    let side = i32::from(px);
    // SAFETY: a valid factory; the bitmap returned is owned here and deleted below.
    let bmp = unsafe { factory.GetImage(SIZE { cx: side, cy: side }, flags) }.ok()?;
    bitmap(bmp)
}

/// Reads and deletes a bitmap.
fn bitmap(bmp: HBITMAP) -> Option<Bgra> {
    let b = gdi::hbitmap_to_bgra(bmp);
    // SAFETY: a bitmap this module owns, deleted once.
    let _ = unsafe { DeleteObject(HGDIOBJ(bmp.0)) };
    b
}

/// FaultIcon::GetIconLocation and Extract (debug builds, registration-free).
fn fault_icon(dll: &std::path::Path, px: u16) -> Option<Bgra> {
    let x: IExtractIconW = fault::create(dll, &fault::CLSID_FAULT_ICON).ok()?;
    let mut file = [0u16; 260];
    let (mut index, mut flags) = (0i32, 0u32);
    // SAFETY: a writable buffer of 260 units and two writable out values.
    unsafe { x.GetIconLocation(0, &mut file, &mut index, &mut flags) }.ok()?;
    let mut large = HICON::default();
    // SAFETY: the location the handler just gave; one out icon, owned and destroyed below.
    unsafe { x.Extract(windows::core::PCWSTR(file.as_ptr()), index as u32, Some(&mut large), None, u32::from(px)) }
        .ok()?;
    let b = gdi::hicon_to_bgra(large);
    // SAFETY: the icon Extract made for this call, destroyed once.
    let _ = unsafe { DestroyIcon(large) };
    b
}

/// FaultThumb: IInitializeWithItem then IThumbnailProvider::GetThumbnail (debug builds, registration-free).
fn fault_thumb(dll: &std::path::Path, p: &WidePath, px: u16) -> Option<Bgra> {
    let name = HSTRING::from(p.to_string_lossy());
    // SAFETY: a NUL-terminated path; no bind context.
    let item: IShellItem = unsafe { SHCreateItemFromParsingName(&name, None::<&IBindCtx>) }.ok()?;
    let init: IInitializeWithItem = fault::create(dll, &fault::CLSID_FAULT_THUMB).ok()?;
    // SAFETY: a valid item, read access.
    unsafe { init.Initialize(&item, STGM_READ.0) }.ok()?;
    let thumb: IThumbnailProvider = init.cast().ok()?;
    let (mut bmp, mut alpha) = (HBITMAP::default(), WTS_ALPHATYPE::default());
    // SAFETY: two writable out values; the bitmap is owned and deleted by bitmap().
    unsafe { thumb.GetThumbnail(u32::from(px), &mut bmp, &mut alpha) }.ok()?;
    bitmap(bmp)
}
