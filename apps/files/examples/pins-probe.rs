//! pins-probe: spike FS-2, the read-only look at File Explorer's Quick access folder. A release-built tool, never
//! shipped.
//!
//!   pins-probe list --json <out>
//!
//! In its own STA it enumerates the Quick access folder and counts the pinned items that have a filesystem path. It
//! changes nothing in Explorer: it only binds, enumerates and reads properties. <out> is a file inside an existing
//! mod-files-live-* folder under %TEMP% (LiveRoot::at), the one file the probe writes. The report holds counts only,
//! never a path: the tester keeps what Explorer shows in scratch. The same count is printed as one "probe: ..." line.
//! A failure prints "probe: error 0x<HRESULT>" and exits 1; bad arguments exit 2.

#[path = "common/live.rs"]
mod live;

use std::path::PathBuf;

use oled_trust::frame::files::WidePath;
use windows::core::{Interface, GUID, PCWSTR};
use windows::Win32::Foundation::PROPERTYKEY;
use windows::Win32::System::Com::{
    CoInitializeEx, CoTaskMemFree, CoUninitialize, IBindCtx, COINIT_APARTMENTTHREADED, COINIT_DISABLE_OLE1DDE,
};
use windows::Win32::UI::Shell::{
    BHID_EnumItems, IEnumShellItems, IShellItem, IShellItem2, SHCreateItemFromParsingName, SIGDN_FILESYSPATH,
};

/// The Quick access folder, as a parsing name (unverified until the live session).
const QUICK_ACCESS: &str = "shell:::{679f85cb-0220-4080-b29b-5540cc05aab6}";

/// System.Home.IsPinned (from the public property header): the pinned state of an item of Quick access.
const PKEY_HOME_IS_PINNED: PROPERTYKEY =
    PROPERTYKEY { fmtid: GUID::from_u128(0x30C8EEF4_A832_41E2_AB32_E3C3CA28FD29), pid: 4 };

/// The arguments of the probe: the report file.
#[derive(Debug, PartialEq, Eq)]
pub struct Args {
    pub out: PathBuf,
}

/// list --json <out> and nothing else.
pub fn parse_args(args: &[String]) -> Result<Args, String> {
    match args {
        [cmd, flag, out] if cmd == "list" && flag == "--json" && !out.is_empty() => {
            Ok(Args { out: PathBuf::from(out) })
        }
        _ => Err("usage: pins-probe list --json <out>".into()),
    }
}

/// What one item of the Quick access folder tells.
struct Found {
    /// The pinned-state property; None when the item does not answer it.
    pinned: Option<bool>,
    path: Option<WidePath>,
}

/// Every item of the folder, in order. Err is the HRESULT of the bind or the enumeration.
fn enumerate_all(out: &mut dyn FnMut(Found)) -> Result<u32, i32> {
    let hr = |e: windows::core::Error| e.code().0;
    let name: Vec<u16> = QUICK_ACCESS.encode_utf16().chain(Some(0)).collect();
    // SAFETY: name is NUL-terminated and outlives the call.
    let folder: IShellItem =
        unsafe { SHCreateItemFromParsingName(PCWSTR(name.as_ptr()), None::<&IBindCtx>) }.map_err(hr)?;
    // SAFETY: BHID_EnumItems is a valid handler id and the result type matches it.
    let items: IEnumShellItems = unsafe { folder.BindToHandler(None::<&IBindCtx>, &BHID_EnumItems) }.map_err(hr)?;
    let mut total = 0u32;
    loop {
        let mut one: [Option<IShellItem>; 1] = [None];
        let mut got = 0u32;
        // SAFETY: the slice and the counter are writable for the call.
        unsafe { items.Next(&mut one, Some(&mut got)) }.map_err(hr)?;
        let Some(item) = one[0].take() else { break };
        if got == 0 {
            break;
        }
        total += 1;
        let pinned = item
            .cast::<IShellItem2>()
            .ok()
            // SAFETY: the key is a constant and the item is live.
            .and_then(|i| unsafe { i.GetBool(&PKEY_HOME_IS_PINNED) }.ok())
            .map(|b| b.as_bool());
        // SAFETY: SIGDN_FILESYSPATH returns a CoTaskMem string (or an error for an item without a path).
        let path = unsafe { item.GetDisplayName(SIGDN_FILESYSPATH) }.ok().and_then(|p| {
            // SAFETY: p is a valid NUL-terminated string until freed just below.
            let units = unsafe { p.as_wide() }.to_vec();
            // SAFETY: allocated by the shell with CoTaskMemAlloc, freed once.
            unsafe { CoTaskMemFree(Some(p.0 as *const _)) };
            WidePath::new(&units).ok()
        });
        out(Found { pinned, path });
    }
    Ok(total)
}

/// The pinned folders of Quick access that have a filesystem path, each handed to out; the count. One function so
/// that the library can take it over unchanged; it runs inside a COM apartment of the caller.
fn enumerate_pins(out: &mut dyn FnMut(WidePath)) -> Result<u32, i32> {
    let mut n = 0u32;
    enumerate_all(&mut |f| {
        if let (Some(true), Some(p)) = (f.pinned, f.path) {
            n += 1;
            out(p);
        }
    })?;
    Ok(n)
}

/// The counts of one run.
struct Counts {
    items: u32,
    pinned: u32,
    pinned_with_path: u32,
    state_unknown: u32,
}

fn run() -> Result<Counts, i32> {
    let mut c = Counts { items: 0, pinned: 0, pinned_with_path: 0, state_unknown: 0 };
    c.items = enumerate_all(&mut |f| match f.pinned {
        Some(true) => {
            c.pinned += 1;
            if f.path.is_some() {
                c.pinned_with_path += 1;
            }
        }
        Some(false) => {}
        None => c.state_unknown += 1,
    })?;
    // The count the library function gives must agree with the counted one.
    let n = enumerate_pins(&mut |_| {})?;
    debug_assert_eq!(n, c.pinned_with_path);
    Ok(c)
}

/// Runs the enumeration in a thread of its own that owns an STA.
fn in_sta() -> Result<Counts, i32> {
    std::thread::spawn(|| {
        // SAFETY: once on this thread, undone below.
        let inited = unsafe { CoInitializeEx(None, COINIT_APARTMENTTHREADED | COINIT_DISABLE_OLE1DDE) }.is_ok();
        let r = run();
        if inited {
            // SAFETY: balances the successful CoInitializeEx above.
            unsafe { CoUninitialize() };
        }
        r
    })
    .join()
    .unwrap_or(Err(-1))
}

fn main() {
    let argv: Vec<String> = std::env::args().skip(1).collect();
    let args = match parse_args(&argv) {
        Ok(a) => a,
        Err(e) => {
            eprintln!("pins-probe: {e}");
            std::process::exit(2);
        }
    };
    let (Some(dir), Some(name)) = (args.out.parent(), args.out.file_name().and_then(|n| n.to_str())) else {
        eprintln!("pins-probe: <out> must be a file path");
        std::process::exit(2);
    };
    let root = match live::LiveRoot::at(dir) {
        Ok(r) => r,
        Err(e) => {
            eprintln!("pins-probe: {e}");
            std::process::exit(2);
        }
    };
    match in_sta() {
        Ok(c) => {
            let text = serde_json::to_string_pretty(&serde_json::json!({
                "import": "read",
                "items": c.items,
                "pinned": c.pinned,
                "pinned_with_path": c.pinned_with_path,
                "state_unknown": c.state_unknown,
            }))
            .unwrap_or_default();
            root.file(name, text.as_bytes());
            println!(
                "probe: items {} pinned {} pinned_with_path {} state_unknown {}",
                c.items, c.pinned, c.pinned_with_path, c.state_unknown
            );
        }
        Err(code) => {
            println!("probe: error 0x{:08X}", code as u32);
            std::process::exit(1);
        }
    }
}
