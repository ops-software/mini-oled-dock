//! The dev-only fault DLL of the shell-host tests: an example of the files package (a cdylib), never shipped and never
//! registered outside CI's files-disposable job. Three COM classes whose calls fail on demand, so the shell host's
//! deadlines, kills, breakers and busy protocol can be proven against a real extension:
//! - FaultMenu (IShellExtInit + IContextMenu): QueryContextMenu faults at point "menu", else inserts "Fault write"
//!   (verb "modfault.write"); InvokeCommand of it appends 1 KiB per second to FILES_FAULT_OUT when asked to.
//! - FaultIcon (IExtractIconW, and IPersistFile, through whose Load the shell initializes a registered icon handler):
//!   Extract faults at point "icon", else makes a solid icon.
//! - FaultThumb (IInitializeWithItem + IThumbnailProvider): GetThumbnail faults at point "thumb", else a solid
//!   64x64 bitmap. Registered, it loads in-process only with DisableProcessIsolation set on its class (the
//!   disposable job's -Register does it).
//!
//! The host's environment chooses the fault: FILES_FAULT_POINT (menu, icon, thumb) and FILES_FAULT_MODE: "die" ends
//! the process at once without an error report (the mode plain tests use), "crash" writes to an unmapped address,
//! "hang" sleeps 30 s, "slow" sleeps 1 s and then works, "abort" fails fast, "stack" recurses without end ("crash",
//! "abort" and "stack" may raise Windows Error Reporting: disposable runner only), "ok" works, "invoke-write" works
//! and makes InvokeCommand write. FILES_FAULT_INVOKE=write makes InvokeCommand write whatever the mode, and
//! FILES_FAULT_MODE_FILE names a file whose text replaces FILES_FAULT_MODE at each call (a test switches modes between
//! two hosts). InvokeCommand writes only under %TEMP%\mod-files-test-*; FILES_FAULT_SECS seconds (5 by default).
//!
//! Exports DllGetClassObject and DllCanUnloadNow only: there is no DllRegisterServer, nothing can self-register.
#![allow(clippy::missing_safety_doc)]

use std::ffi::c_void;
use std::io::Write;
use std::path::{Component, Path, PathBuf};
use std::time::Duration;

use windows::core::{implement, IUnknown, Interface, Ref, Result, BOOL, GUID, HRESULT, PCWSTR, PSTR, PWSTR};
use windows::Win32::Foundation::{
    CLASS_E_CLASSNOTAVAILABLE, CLASS_E_NOAGGREGATION, E_ACCESSDENIED, E_FAIL, E_INVALIDARG, E_NOTIMPL, E_POINTER,
    S_FALSE,
};
use windows::Win32::Graphics::Gdi::{
    CreateBitmap, CreateDIBSection, DeleteObject, BITMAPINFO, BITMAPINFOHEADER, BI_RGB, DIB_RGB_COLORS, HBITMAP,
    HGDIOBJ,
};
use windows::Win32::System::Com::{
    IClassFactory, IClassFactory_Impl, IDataObject, IPersistFile, IPersistFile_Impl, IPersist_Impl, STGM,
};
use windows::Win32::System::Registry::HKEY;
use windows::Win32::System::Threading::{GetCurrentProcess, TerminateProcess};
use windows::Win32::UI::Shell::Common::ITEMIDLIST;
use windows::Win32::UI::Shell::{
    IContextMenu, IContextMenu_Impl, IExtractIconW, IExtractIconW_Impl, IInitializeWithItem, IInitializeWithItem_Impl,
    IShellExtInit, IShellExtInit_Impl, IShellItem, IThumbnailProvider, IThumbnailProvider_Impl, CMF_DEFAULTONLY,
    CMINVOKECOMMANDINFO, GCS_VALIDATEA, GCS_VALIDATEW, GCS_VERBA, GCS_VERBW, GIL_DONTCACHE, GIL_NOTFILENAME,
    WTSAT_ARGB, WTS_ALPHATYPE,
};
use windows::Win32::UI::WindowsAndMessaging::{
    CreateIconIndirect, InsertMenuW, HICON, HMENU, ICONINFO, MF_BYPOSITION, MF_STRING,
};

/// FaultMenu, FaultIcon and FaultThumb: generated once, pinned here, in the shell host (shell::host::fault) and in
/// tools/ci/files-disposable.ps1.
pub const CLSID_FAULT_MENU: GUID = GUID::from_u128(0x08f7748e_e7c9_4950_ac48_6ebab3ca29dc);
pub const CLSID_FAULT_ICON: GUID = GUID::from_u128(0xe3fec5ef_f489_427f_879f_ef723f66d403);
pub const CLSID_FAULT_THUMB: GUID = GUID::from_u128(0x5a4db2eb_6c01_444f_be94_ebff37f11b35);

/// The verb of the one menu item.
const VERB: &str = "modfault.write";
/// The exit code of mode "die": STATUS_ACCESS_VIOLATION, as a crash would end.
const DIE_CODE: u32 = 0xC000_0005;

fn var(k: &str) -> String {
    std::env::var(k).unwrap_or_default()
}

/// The mode in force for this call.
fn mode() -> String {
    let file = var("FILES_FAULT_MODE_FILE");
    if !file.is_empty() {
        if let Ok(m) = std::fs::read_to_string(&file) {
            return m.trim().to_string();
        }
    }
    var("FILES_FAULT_MODE")
}

/// Faults at `point` when the environment asks for it there.
fn fault(point: &str) {
    if var("FILES_FAULT_POINT") != point {
        return;
    }
    match mode().as_str() {
        "die" => {
            // SAFETY: ends this process at once, the point of the mode; the pseudo handle needs no closing.
            let _ = unsafe { TerminateProcess(GetCurrentProcess(), DIE_CODE) };
            std::process::abort();
        }
        // SAFETY: none: a write to an unmapped, aligned, non-null address is the access violation this mode wants.
        "crash" => unsafe { std::ptr::without_provenance_mut::<u32>(16).write_volatile(1) },
        "hang" => std::thread::sleep(Duration::from_secs(30)),
        "slow" => std::thread::sleep(Duration::from_secs(1)),
        "abort" => std::process::abort(),
        "stack" => {
            std::hint::black_box(recurse(1));
        }
        _ => {}
    }
}

#[allow(unconditional_recursion)]
fn recurse(n: u64) -> u64 {
    let frame = std::hint::black_box([n; 64]);
    recurse(n + 1).wrapping_add(frame[(n % 64) as usize])
}

/// FILES_FAULT_OUT when it lies under %TEMP%\mod-files-test-* (no ".." anywhere), else None.
fn out_path() -> Option<PathBuf> {
    let out = PathBuf::from(var("FILES_FAULT_OUT"));
    if out.components().any(|c| matches!(c, Component::ParentDir | Component::CurDir)) {
        return None;
    }
    let under = |base: &Path| {
        let rel = out.strip_prefix(base).ok()?;
        let first = rel.components().next()?.as_os_str().to_str()?.to_ascii_lowercase();
        (first.starts_with("mod-files-test-") && rel.components().count() >= 2).then_some(())
    };
    // %TEMP% as given, or in its long form (an 8.3 %TEMP% under C:\Users\RUNNER~1 on the CI runner, where the tests'
    // roots are long).
    let temp = std::env::temp_dir();
    let long = std::fs::canonicalize(&temp).ok().and_then(|p| p.to_str()?.strip_prefix(r"\\?\").map(PathBuf::from));
    under(&temp).or_else(|| long.as_deref().and_then(under))?;
    Some(out)
}

/// InvokeCommand's work in mode invoke-write: 1 KiB per second for FILES_FAULT_SECS seconds.
fn invoke_write() -> Result<()> {
    let Some(out) = out_path() else {
        return Err(E_ACCESSDENIED.into());
    };
    let secs: u64 = var("FILES_FAULT_SECS").parse().unwrap_or(5).min(120);
    for _ in 0..secs {
        let mut f = std::fs::OpenOptions::new().create(true).append(true).open(&out).map_err(|_| E_FAIL)?;
        f.write_all(&[b'w'; 1024]).map_err(|_| E_FAIL)?;
        drop(f);
        std::thread::sleep(Duration::from_secs(1));
    }
    Ok(())
}

fn invoke_writes() -> bool {
    mode() == "invoke-write" || var("FILES_FAULT_INVOKE") == "write"
}

#[implement(IShellExtInit, IContextMenu)]
struct FaultMenu;

impl IShellExtInit_Impl for FaultMenu_Impl {
    fn Initialize(&self, _folder: *const ITEMIDLIST, _data: Ref<IDataObject>, _progid: HKEY) -> Result<()> {
        Ok(())
    }
}

impl IContextMenu_Impl for FaultMenu_Impl {
    fn QueryContextMenu(&self, hmenu: HMENU, index: u32, first: u32, _last: u32, flags: u32) -> HRESULT {
        fault("menu");
        if flags & CMF_DEFAULTONLY != 0 {
            return HRESULT(0);
        }
        // SAFETY: `hmenu` is the caller's live menu; the text is a static NUL-terminated string.
        match unsafe {
            InsertMenuW(hmenu, index, MF_BYPOSITION | MF_STRING, first as usize, windows::core::w!("Fault write"))
        } {
            Ok(()) => HRESULT(1),
            Err(e) => e.code(),
        }
    }

    fn InvokeCommand(&self, pici: *const CMINVOKECOMMANDINFO) -> Result<()> {
        if pici.is_null() {
            return Err(E_POINTER.into());
        }
        // SAFETY: the caller passes a valid CMINVOKECOMMANDINFO (or the larger EX form, whose head it is).
        let verb = unsafe { (*pici).lpVerb };
        let ours = if (verb.0 as usize) >> 16 == 0 {
            verb.0 as usize == 0
        } else {
            // SAFETY: a verb that is not an id is a NUL-terminated ANSI string.
            unsafe { verb.to_string() }.is_ok_and(|v| v.eq_ignore_ascii_case(VERB))
        };
        if !ours {
            return Err(E_INVALIDARG.into());
        }
        if invoke_writes() {
            invoke_write()
        } else {
            Ok(())
        }
    }

    fn GetCommandString(&self, id: usize, kind: u32, _r: *const u32, name: PSTR, cch: u32) -> Result<()> {
        if id != 0 {
            return Err(E_INVALIDARG.into());
        }
        match kind {
            GCS_VERBW => {
                let w: Vec<u16> = VERB.encode_utf16().chain(Some(0)).collect();
                if name.is_null() || (cch as usize) < w.len() {
                    return Err(E_INVALIDARG.into());
                }
                // SAFETY: for GCS_VERBW the buffer holds cch UTF-16 units, at least w.len() (checked).
                unsafe { std::ptr::copy_nonoverlapping(w.as_ptr(), name.0.cast::<u16>(), w.len()) };
                Ok(())
            }
            GCS_VERBA => {
                let a: Vec<u8> = VERB.bytes().chain(Some(0)).collect();
                if name.is_null() || (cch as usize) < a.len() {
                    return Err(E_INVALIDARG.into());
                }
                // SAFETY: for GCS_VERBA the buffer holds cch bytes, at least a.len() (checked).
                unsafe { std::ptr::copy_nonoverlapping(a.as_ptr(), name.0, a.len()) };
                Ok(())
            }
            GCS_VALIDATEW | GCS_VALIDATEA => Ok(()),
            _ => Err(E_NOTIMPL.into()),
        }
    }
}

/// A solid opaque 32-bit top-down DIB section of side `n`.
fn solid_bitmap(n: i32, argb: u32) -> Result<HBITMAP> {
    let bi = BITMAPINFO {
        bmiHeader: BITMAPINFOHEADER {
            biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
            biWidth: n,
            biHeight: -n,
            biPlanes: 1,
            biBitCount: 32,
            biCompression: BI_RGB.0,
            ..Default::default()
        },
        ..Default::default()
    };
    let mut bits: *mut c_void = std::ptr::null_mut();
    // SAFETY: `bi` describes a 32-bit DIB; `bits` receives its pixel pointer.
    let bmp = unsafe { CreateDIBSection(None, &bi, DIB_RGB_COLORS, &mut bits, None, 0) }?;
    if bits.is_null() {
        // SAFETY: the bitmap was just created here.
        let _ = unsafe { DeleteObject(HGDIOBJ(bmp.0)) };
        return Err(E_FAIL.into());
    }
    let len = (n * n) as usize;
    // SAFETY: the DIB section holds n * n 32-bit pixels at `bits`, writable until the bitmap is deleted.
    unsafe { std::slice::from_raw_parts_mut(bits.cast::<u32>(), len) }.fill(argb);
    Ok(bmp)
}

/// A solid icon of side `n`.
fn solid_icon(n: i32) -> Result<HICON> {
    let color = solid_bitmap(n, 0xFF20_80C0)?;
    let mask_bits = vec![0u8; (n.max(16) as usize).div_ceil(16) * 2 * n as usize];
    // SAFETY: a monochrome bitmap of n x n from a zeroed buffer of word-aligned rows, copied by the call.
    let mask = unsafe { CreateBitmap(n, n, 1, 1, Some(mask_bits.as_ptr().cast())) };
    let info = ICONINFO { fIcon: true.into(), xHotspot: 0, yHotspot: 0, hbmMask: mask, hbmColor: color };
    // SAFETY: both bitmaps are valid; CreateIconIndirect copies them, so they are deleted after.
    let icon = unsafe { CreateIconIndirect(&info) };
    // SAFETY: created above, owned here, deleted once.
    unsafe {
        let _ = DeleteObject(HGDIOBJ(color.0));
        let _ = DeleteObject(HGDIOBJ(mask.0));
    }
    icon
}

#[implement(IExtractIconW, IPersistFile)]
struct FaultIcon;

impl IPersist_Impl for FaultIcon_Impl {
    fn GetClassID(&self) -> Result<GUID> {
        Ok(CLSID_FAULT_ICON)
    }
}

/// The shell's initialization of a registered icon handler: the file is not kept (every icon is the same).
impl IPersistFile_Impl for FaultIcon_Impl {
    fn IsDirty(&self) -> HRESULT {
        S_FALSE
    }

    fn Load(&self, _file: &PCWSTR, _mode: STGM) -> Result<()> {
        Ok(())
    }

    fn Save(&self, _file: &PCWSTR, _remember: BOOL) -> Result<()> {
        Err(E_NOTIMPL.into())
    }

    fn SaveCompleted(&self, _file: &PCWSTR) -> Result<()> {
        Err(E_NOTIMPL.into())
    }

    fn GetCurFile(&self) -> Result<PWSTR> {
        Err(E_NOTIMPL.into())
    }
}

impl IExtractIconW_Impl for FaultIcon_Impl {
    fn GetIconLocation(&self, _flags: u32, file: PWSTR, cch: u32, index: *mut i32, out_flags: *mut u32) -> Result<()> {
        let w: Vec<u16> = "files_faultext".encode_utf16().chain(Some(0)).collect();
        if file.is_null() || index.is_null() || out_flags.is_null() || (cch as usize) < w.len() {
            return Err(E_INVALIDARG.into());
        }
        // SAFETY: the caller's buffer holds cch units (checked against w); the two out pointers are non-null.
        unsafe {
            std::ptr::copy_nonoverlapping(w.as_ptr(), file.0, w.len());
            *index = 0;
            *out_flags = GIL_NOTFILENAME | GIL_DONTCACHE;
        }
        Ok(())
    }

    fn Extract(&self, _file: &PCWSTR, _index: u32, large: *mut HICON, small: *mut HICON, size: u32) -> Result<()> {
        fault("icon");
        let side = |s: u32| if s == 0 { 16 } else { s.min(256) as i32 };
        if !large.is_null() {
            // SAFETY: a non-null out pointer of the caller's.
            unsafe { *large = solid_icon(side(size & 0xFFFF))? };
        }
        if !small.is_null() {
            // SAFETY: as above.
            unsafe { *small = solid_icon(side(size >> 16))? };
        }
        Ok(())
    }
}

#[implement(IInitializeWithItem, IThumbnailProvider)]
struct FaultThumb;

impl IInitializeWithItem_Impl for FaultThumb_Impl {
    fn Initialize(&self, _item: Ref<IShellItem>, _mode: u32) -> Result<()> {
        Ok(())
    }
}

impl IThumbnailProvider_Impl for FaultThumb_Impl {
    fn GetThumbnail(&self, _cx: u32, bmp: *mut HBITMAP, alpha: *mut WTS_ALPHATYPE) -> Result<()> {
        fault("thumb");
        if bmp.is_null() || alpha.is_null() {
            return Err(E_POINTER.into());
        }
        let b = solid_bitmap(64, 0xFFC0_4020)?;
        // SAFETY: two non-null out pointers of the caller's.
        unsafe {
            *bmp = b;
            *alpha = WTSAT_ARGB;
        }
        Ok(())
    }
}

#[derive(Clone, Copy)]
enum Class {
    Menu,
    Icon,
    Thumb,
}

#[implement(IClassFactory)]
struct Factory(Class);

impl IClassFactory_Impl for Factory_Impl {
    fn CreateInstance(&self, outer: Ref<IUnknown>, riid: *const GUID, ppv: *mut *mut c_void) -> Result<()> {
        if outer.ok().is_ok() {
            return Err(CLASS_E_NOAGGREGATION.into());
        }
        if riid.is_null() || ppv.is_null() {
            return Err(E_POINTER.into());
        }
        let unknown: IUnknown = match self.0 {
            Class::Menu => FaultMenu.into(),
            Class::Icon => FaultIcon.into(),
            Class::Thumb => FaultThumb.into(),
        };
        // SAFETY: riid and ppv are the caller's valid in and out pointers (checked non-null).
        unsafe { unknown.query(riid, ppv) }.ok()
    }

    fn LockServer(&self, _lock: BOOL) -> Result<()> {
        Ok(())
    }
}

/// The class factory of one of the three classes.
#[no_mangle]
#[allow(non_snake_case)]
pub unsafe extern "system" fn DllGetClassObject(
    clsid: *const GUID,
    riid: *const GUID,
    ppv: *mut *mut c_void,
) -> HRESULT {
    if clsid.is_null() || riid.is_null() || ppv.is_null() {
        return E_POINTER;
    }
    // SAFETY: ppv is a non-null out pointer of the caller's; clsid a valid GUID.
    unsafe { *ppv = std::ptr::null_mut() };
    let class = match unsafe { *clsid } {
        CLSID_FAULT_MENU => Class::Menu,
        CLSID_FAULT_ICON => Class::Icon,
        CLSID_FAULT_THUMB => Class::Thumb,
        _ => return CLASS_E_CLASSNOTAVAILABLE,
    };
    let factory: IClassFactory = Factory(class).into();
    // SAFETY: riid and ppv are the caller's valid in and out pointers.
    unsafe { factory.query(riid, ppv) }
}

/// The DLL stays loaded for the life of the process.
#[no_mangle]
#[allow(non_snake_case)]
pub extern "system" fn DllCanUnloadNow() -> HRESULT {
    S_FALSE
}
