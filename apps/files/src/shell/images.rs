//! Icons and thumbnails in the shell host (SH9): per item, IShellItemImageFactory::GetImage of the asked size inside a
//! crumbs guard, the bitmap read to premultiplied BGRA (decor::gdi), one Icon or Thumb frame per item that worked,
//! then ReqDone. Work of a generation that CancelGen dropped stops between items. A bitmap larger than
//! MAX_PIXELS_SIDE is skipped (GetImage returns the asked size; a larger one is a broken handler). In a debug build
//! with FILES_FAULT_DLL and the fault point "icon" or "thumb", the fault DLL's FaultIcon::Extract or
//! FaultThumb::GetThumbnail runs in the same guard instead.

use std::cell::RefCell;
use std::collections::HashMap;

use crate::drives::DriveKind;
use oled_trust::frame::files::{CallKind, Gen, ItemRef, Pixels, ReqId, ShellEvt, WidePath, MAX_PIXELS_SIDE};
use windows::core::{w, Interface, HSTRING, PCWSTR};
use windows::Win32::Foundation::SIZE;
use windows::Win32::Graphics::Gdi::{DeleteObject, HBITMAP, HGDIOBJ};
use windows::Win32::Storage::FileSystem::{GetDriveTypeW, GetFileAttributesW, INVALID_FILE_ATTRIBUTES};
use windows::Win32::System::Com::{IBindCtx, STGM_READ};
use windows::Win32::System::WindowsProgramming::GetPrivateProfileStringW;
use windows::Win32::UI::Shell::{
    IExtractIconW, IInitializeWithItem, IShellItem, IShellItemImageFactory, IThumbnailProvider,
    SHCreateItemFromParsingName, SIIGBF, SIIGBF_ICONONLY, SIIGBF_INCACHEONLY, SIIGBF_THUMBNAILONLY, WTS_ALPHATYPE,
};
use windows::Win32::UI::WindowsAndMessaging::{DestroyIcon, HICON};

use crate::decor::gdi::{self, Bgra};
use crate::gate;
use crate::shell::crumbs;
use crate::shell::host::{self, fault, HostCfg};
use crate::shell::writer::Writer;

/// Whether an image may be loaded from the item at `path` (UTF-16, no NUL): only `X:\...` or `\\?\X:\...` on a drive
/// of kind Fixed. UNC, `\\?\UNC\`, device paths (`\\.\`, `\\?\GLOBALROOT`), WebDAV shares, any `scheme:`, relative
/// paths, a stream name and an unexpanded `%` are refused, so no image load can reach a remote server or a device.
/// So is a component ending in a dot or a space (Win32 parsing drops them: `a.lnk.` would load `a.lnk`, a folder
/// `x.` is read as `x`), and a component whose last extension is a class id: the shell binds `photos.{CLSID}` to that
/// class although the file system lists a plain folder (a folder shortcut sends every path through it to its target).
pub fn may_load_icon_from(path: &[u16], drive_kind: impl Fn(u16) -> DriveKind) -> bool {
    const BS: u16 = b'\\' as u16;
    const LBRACE: u16 = b'{' as u16;
    let long = [BS, BS, b'?' as u16, BS];
    let rest = if path.starts_with(&long) { &path[4..] } else { path };
    let [letter, colon, sep, tail @ ..] = rest else { return false };
    let ascii_letter = u8::try_from(*letter).is_ok_and(|b| b.is_ascii_alphabetic());
    if !ascii_letter || *colon != u16::from(b':') || *sep != BS || tail.is_empty() {
        return false;
    }
    // A second colon names a stream; a percent sign is an unexpanded variable; a slash is not a path this host takes.
    if tail.iter().any(|&u| u == u16::from(b':') || u == u16::from(b'%') || u == u16::from(b'/') || u == 0) {
        return false;
    }
    for c in tail.split(|&u| u == BS) {
        let class_id = c.iter().rposition(|&u| u == u16::from(b'.')).is_some_and(|d| c.get(d + 1) == Some(&LBRACE));
        if class_id || c.last().is_some_and(|&u| u == u16::from(b'.') || u == u16::from(b' ')) {
            return false;
        }
    }
    drive_kind(*letter) == DriveKind::Fixed
}

/// Whether the attributes of an item allow its image to be loaded: no customised folder (the shell reads desktop.ini
/// only for a folder with the read-only or system attribute), no reparse point (a link can point at a remote file),
/// nothing offline or dehydrated (loading would download it).
pub fn attrs_allow_icon(attrs: u32) -> bool {
    const READONLY_OR_SYSTEM: u32 = 0x1 | 0x4;
    const DIRECTORY: u32 = 0x10;
    const REPARSE_POINT: u32 = 0x400;
    const OFFLINE: u32 = 0x1000;
    const RECALL_ON_OPEN: u32 = 0x4_0000;
    const RECALL_ON_DATA_ACCESS: u32 = 0x40_0000;
    let customised = attrs & DIRECTORY != 0 && attrs & READONLY_OR_SYSTEM != 0;
    !customised && attrs & (REPARSE_POINT | OFFLINE | RECALL_ON_OPEN | RECALL_ON_DATA_ACCESS) == 0
}

/// The only kinds whose per-item icon the host loads: files whose icon is inside the file itself, so the icon location
/// is the item and the checks below judge it. Every other item keeps its class icon. The UI asks only for these
/// (icons::wants_item_icon reads this list).
pub const OWN_ICON_EXTS: [&str; 5] = ["exe", "ico", "scr", "cur", "ani"];

/// Thumbnails (no caller yet): extensions whose image the shell builds from a location named inside the file (a
/// shortcut's target, a console's icon element, a command file's IconFile, a connector's iconReference, a theme's
/// wallpaper). A class id (`name.{CLSID}`) is refused by may_load_icon_from in every component. The first caller of
/// Thumbs replaces this list with a reviewed allowlist.
fn names_other_location(ext: &str) -> bool {
    const KINDS: [&str; 14] = [
        "lnk",
        "url",
        "appref-ms",
        "msc",
        "library-ms",
        "search-ms",
        "website",
        "cpl",
        "pif",
        "scf",
        "searchconnector-ms",
        "theme",
        "themepack",
        "deskthemepack",
    ];
    KINDS.contains(&ext)
}

/// The extension of a path's last component, lower case and whole (crumbs::ext_of cuts it for its slot).
fn ext_whole(p: &WidePath) -> String {
    let name = p.file_name().map(String::from_utf16_lossy).unwrap_or_default();
    name.rfind('.').and_then(|dot| name.get(dot + 1..)).map(str::to_lowercase).unwrap_or_default()
}

/// GetDriveTypeW of a letter, as a DriveKind.
fn drive_kind(letter: u16) -> DriveKind {
    let root = [letter, u16::from(b':'), u16::from(b'\\'), 0];
    // SAFETY: a NUL-terminated drive root.
    match unsafe { GetDriveTypeW(windows::core::PCWSTR(root.as_ptr())) } {
        2 => DriveKind::Removable,
        3 => DriveKind::Fixed,
        4 => DriveKind::Network,
        5 => DriveKind::Optical,
        6 => DriveKind::Ram,
        _ => DriveKind::Unknown,
    }
}

/// The check that runs before any load, in this order: the kind, the pure location rule, then every folder above the
/// item from the drive root down through `is_junction` (folder_is_junction: a link, offline or dehydrated folder, or
/// one the shell parser would leave the file system at), so that each folder is read only through folders already
/// judged plain and nothing is read through a link (reading the attributes of a UNC or WebDAV path opens a session by
/// itself), and only then the item's own attributes through `attrs_of` and the location rule again on the long form
/// of the item's folder through `long_of` (the shell parser reads the long name of every folder, so an 8.3 alias such
/// as PHOTOS would hide `photos.{CLSID}`; no long form refuses; the item's own name is the listing's long name). Only a
/// file, never a folder: a folder's thumbnail is made from the items inside it, which this check never saw, and
/// `name.{CLSID}` is a junction whose image comes from that class. Icons: a kind in OWN_ICON_EXTS. Thumbnails: no kind
/// that names another location.
pub fn allowed_with(
    p: &WidePath,
    is_thumb: bool,
    drive_kind: impl Fn(u16) -> DriveKind,
    attrs_of: impl Fn(&WidePath) -> Option<u32>,
    is_junction: impl Fn(&WidePath) -> bool,
    long_of: impl Fn(&WidePath) -> Option<WidePath>,
) -> bool {
    const DIRECTORY: u32 = 0x10;
    let ext = ext_whole(p);
    let kind_ok = if is_thumb { !names_other_location(&ext) } else { OWN_ICON_EXTS.contains(&ext.as_str()) };
    if !kind_ok || !may_load_icon_from(p.units(), &drive_kind) {
        return false;
    }
    let mut folders: Vec<WidePath> = std::iter::successors(p.parent(), WidePath::parent).collect();
    folders.reverse();
    if folders.iter().any(&is_junction) {
        return false;
    }
    if !attrs_of(p).is_some_and(|a| attrs_allow_icon(a) && a & DIRECTORY == 0) {
        return false;
    }
    let long = p.parent().and_then(|d| long_of(&d)).zip(p.file_name()).and_then(|(d, name)| d.join(name).ok());
    long.is_some_and(|q| may_load_icon_from(q.units(), &drive_kind))
}

/// Whether the shell parser binds the folder `dir` to a class instead of reading it from the file system, although the
/// file system lists a plain folder: a read-only or system folder (the only folders whose desktop.ini the shell reads)
/// whose desktop.ini names a CLSID or CLSID2 in [.ShellClassInfo]. A folder shortcut of that kind sends every path
/// through it to its target, a remote share too, and so does a folder that is a link (a symbolic link can name a
/// share). The facts are read in junction_from's order: the folder's attributes (GetFileAttributesW does not follow a
/// link), those of its desktop.ini, and only for a plain file its two keys.
pub fn folder_is_junction(dir: &WidePath) -> bool {
    let raw = dir.raw();
    let mut ini = raw.clone();
    ini.pop();
    if ini.last() != Some(&u16::from(b'\\')) {
        ini.push(u16::from(b'\\'));
    }
    ini.extend("desktop.ini\0".encode_utf16());
    let file = PCWSTR(ini.as_ptr());
    let names_class = || {
        [w!("CLSID"), w!("CLSID2")].into_iter().any(|key| {
            let mut value = [0u16; 4];
            // SAFETY: NUL-terminated names and path, alive for the call; a writable buffer whose length is passed.
            let n = unsafe { GetPrivateProfileStringW(w!(".ShellClassInfo"), key, w!(""), Some(&mut value), file) };
            n > 0
        })
    };
    junction_from(attrs_at(&raw), || attrs_at(&ini), names_class)
}

/// The rule of folder_is_junction with its facts injected. A folder whose attributes cannot be read (None), a file, and
/// a folder that is a reparse point (a junction or a symbolic link), offline or dehydrated count as a junction, and
/// nothing inside them is looked at; a folder that is neither read-only nor system does not, and its desktop.ini is not
/// looked at. Else no desktop.ini (None) is no junction; a desktop.ini that is a link, a folder, offline or dehydrated
/// counts as one and is never read (opening it would follow the link to a remote file or download it); only a plain
/// file is read, through `names_class`.
pub fn junction_from(
    dir_attrs: Option<u32>,
    ini_attrs: impl FnOnce() -> Option<u32>,
    names_class: impl FnOnce() -> bool,
) -> bool {
    const READONLY_OR_SYSTEM: u32 = 0x1 | 0x4;
    const DIRECTORY: u32 = 0x10;
    const LINK_OR_REMOTE: u32 = 0x400 | 0x1000 | 0x4_0000 | 0x40_0000;
    let Some(dir) = dir_attrs.filter(|a| a & DIRECTORY != 0 && a & LINK_OR_REMOTE == 0) else { return true };
    if dir & READONLY_OR_SYSTEM == 0 {
        return false;
    }
    match ini_attrs() {
        None => false,
        Some(a) if !attrs_allow_icon(a) || a & DIRECTORY != 0 => true,
        Some(_) => names_class(),
    }
}

/// GetFileAttributesW of a NUL-terminated path (it does not follow the last component), None when it fails.
fn attrs_at(raw: &[u16]) -> Option<u32> {
    // SAFETY: a NUL-terminated path, alive for the call.
    let attrs = unsafe { GetFileAttributesW(PCWSTR(raw.as_ptr())) };
    (attrs != INVALID_FILE_ATTRIBUTES).then_some(attrs)
}

/// GetFileAttributesW of the item itself (it does not follow a link), None when it fails.
fn file_attrs(p: &WidePath) -> Option<u32> {
    let name = HSTRING::from(p.to_string_lossy());
    // SAFETY: a NUL-terminated path.
    let attrs = unsafe { GetFileAttributesW(&name) };
    (attrs != INVALID_FILE_ATTRIBUTES).then_some(attrs)
}

/// Icons (thumbs None) or thumbnails (Some(cache_only)) of the items, then ReqDone.
pub fn run(req: ReqId, gen: Gen, items: &[ItemRef], px: u16, thumbs: Option<bool>, w: &Writer, cfg: &HostCfg) {
    let is_thumb = thumbs.is_some();
    let kind = if is_thumb { CallKind::Thumb } else { CallKind::Icon };
    // The items of a request share their folders: each folder is judged, and its long form read, once.
    let junctions = RefCell::new(HashMap::<WidePath, bool>::new());
    let is_junction = |d: &WidePath| *junctions.borrow_mut().entry(d.clone()).or_insert_with(|| folder_is_junction(d));
    let longs = RefCell::new(HashMap::<WidePath, Option<WidePath>>::new());
    let long_of = |d: &WidePath| longs.borrow_mut().entry(d.clone()).or_insert_with(|| gate::long_path(d)).clone();
    let check = |p: &WidePath| allowed_with(p, is_thumb, drive_kind, file_attrs, is_junction, long_of);
    run_with(req, gen, items, is_thumb, &|e| w.send(e), &check, &mut |p, ext| {
        let _g = crumbs::enter(kind, ext);
        image_of(p, px, thumbs, cfg)
    });
}

/// The loop of `run` with its three seams: where events go, the check, and the load. The check runs for every item
/// before the load; an item it refuses is never loaded, and ReqDone is sent in any case.
pub fn run_with(
    req: ReqId,
    gen: Gen,
    items: &[ItemRef],
    is_thumb: bool,
    send: &dyn Fn(&ShellEvt),
    check: &dyn Fn(&WidePath) -> bool,
    load: &mut dyn FnMut(&WidePath, &str) -> Option<Bgra>,
) {
    for (index, item) in items.iter().enumerate() {
        if host::cancelled(gen) {
            break;
        }
        let ItemRef::Path(p) = item else { continue };
        if !check(p) {
            continue;
        }
        let ext = crumbs::ext_of(p);
        let img = load(p, &ext);
        let Some(b) = img else { continue };
        let (Ok(bw), Ok(bh)) = (u16::try_from(b.w), u16::try_from(b.h)) else { continue };
        if bw == 0 || bh == 0 || bw > MAX_PIXELS_SIDE || bh > MAX_PIXELS_SIDE {
            continue;
        }
        let pixels = Pixels { w: bw, h: bh, bgra: gdi::to_bytes(&b) };
        let index = index as u32;
        send(&if is_thumb {
            ShellEvt::Thumb { req, gen, index, px: pixels }
        } else {
            ShellEvt::Icon { req, gen, index, px: pixels }
        });
    }
    send(&ShellEvt::ReqDone { req });
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
