//! Pinned folders served by the shell host (P3-files-app F4.8): the folders File Explorer pins in Quick access, read
//! through the shell's own folder (a bind and an enumeration, never a write), on the exec STA inside a crumbs guard,
//! sent as Pins frames split by bytes; the caller sends ReqDone after the last. The pin verb is not used (open point 4):
//! pin() answers E_NOTIMPL, so nothing is ever written to Explorer's state.

use oled_trust::frame::files::{CallKind, ItemRef, ReqId, ShellEvt, WidePath, MAX_LIST, SHELL};
use oled_trust::frame::split_by_bytes;
use windows::core::{Interface, GUID, PCWSTR};
use windows::Win32::Foundation::{E_NOTIMPL, PROPERTYKEY};
use windows::Win32::System::Com::{CoTaskMemFree, IBindCtx};
use windows::Win32::UI::Shell::{
    BHID_EnumItems, IEnumShellItems, IShellItem, IShellItem2, SHCreateItemFromParsingName, SIGDN_FILESYSPATH,
};

use crate::shell::crumbs;
use crate::shell::host::test_var;

/// The Quick access folder, as a parsing name.
const QUICK_ACCESS: &str = "shell:::{679f85cb-0220-4080-b29b-5540cc05aab6}";

/// System.Home.IsPinned: the pinned state of an item of Quick access.
const PKEY_HOME_IS_PINNED: PROPERTYKEY =
    PROPERTYKEY { fmtid: GUID::from_u128(0x30C8EEF4_A832_41E2_AB32_E3C3CA28FD29), pid: 4 };

/// Debug builds only: a list of folders separated by '|' that serve_pins answers instead of reading Quick access.
pub const FAKE_VAR: &str = "FILES_TEST_PINS";

/// The pinned folders of Quick access that have a filesystem path, each handed to out, in Explorer's order; the
/// count. It runs inside a COM apartment of the caller and writes nothing.
pub fn enumerate_pins(out: &mut dyn FnMut(WidePath)) -> Result<u32, i32> {
    let hr = |e: windows::core::Error| e.code().0;
    let name: Vec<u16> = QUICK_ACCESS.encode_utf16().chain(Some(0)).collect();
    // SAFETY: name is NUL-terminated and outlives the call.
    let folder: IShellItem =
        unsafe { SHCreateItemFromParsingName(PCWSTR(name.as_ptr()), None::<&IBindCtx>) }.map_err(hr)?;
    // SAFETY: BHID_EnumItems is a valid handler id and the result type matches it.
    let items: IEnumShellItems = unsafe { folder.BindToHandler(None::<&IBindCtx>, &BHID_EnumItems) }.map_err(hr)?;
    let mut n = 0u32;
    loop {
        let mut one: [Option<IShellItem>; 1] = [None];
        let mut got = 0u32;
        // SAFETY: the slice and the counter are writable for the call.
        unsafe { items.Next(&mut one, Some(&mut got)) }.map_err(hr)?;
        let Some(item) = one[0].take() else { break };
        if got == 0 {
            break;
        }
        let pinned = item
            .cast::<IShellItem2>()
            .ok()
            // SAFETY: the key is a constant and the item is live.
            .and_then(|i| unsafe { i.GetBool(&PKEY_HOME_IS_PINNED) }.ok())
            .is_some_and(|b| b.as_bool());
        if !pinned {
            continue;
        }
        // SAFETY: SIGDN_FILESYSPATH returns a CoTaskMem string (or an error for an item without a path).
        let path = unsafe { item.GetDisplayName(SIGDN_FILESYSPATH) }.ok().and_then(|p| {
            // SAFETY: p is a valid NUL-terminated string until freed just below.
            let units = unsafe { p.as_wide() }.to_vec();
            // SAFETY: allocated by the shell with CoTaskMemAlloc, freed once.
            unsafe { CoTaskMemFree(Some(p.0 as *const _)) };
            WidePath::new(&units).ok()
        });
        if let Some(p) = path {
            n += 1;
            out(p);
        }
    }
    Ok(n)
}

/// One Pins frame per slice of the folders found (none when there are none); the caller sends ReqDone.
pub fn serve_pins(req: ReqId, out: &mut dyn FnMut(ShellEvt)) -> Result<(), i32> {
    let mut found = Vec::new();
    {
        let _g = crumbs::enter(CallKind::Pins, "");
        match test_var(FAKE_VAR) {
            Some(list) => found.extend(list.split('|').filter_map(|p| WidePath::parse(p).ok())),
            None => {
                enumerate_pins(&mut |p| found.push(p))?;
            }
        }
    }
    send(req, &found, out);
    Ok(())
}

/// The frames of a list of folders: split by bytes, never an empty one.
fn send(req: ReqId, found: &[WidePath], out: &mut dyn FnMut(ShellEvt)) {
    // The payload of a Pins frame is the request id and the list.
    for part in split_by_bytes(found, 4, &SHELL, MAX_LIST) {
        if !part.is_empty() {
            out(ShellEvt::Pins { req, pins: part.to_vec() });
        }
    }
}

/// Verb::Pin and Verb::Unpin: not used (open point 4); the host answers E_NOTIMPL and writes nothing to Explorer. A
/// pin that wrote would run inside crumbs::busy_enter, like every verb of exec.rs; this one does nothing to protect.
pub fn pin(items: &[ItemRef], on: bool) -> i32 {
    let _ = (items, on);
    E_NOTIMPL.0
}

#[cfg(test)]
mod tests {
    use super::*;

    fn wp(s: &str) -> WidePath {
        WidePath::parse(s).expect("a made-up path parses")
    }

    #[test]
    fn frames_split_by_count_and_never_empty() {
        let mut evts = Vec::new();
        send(ReqId(4), &[], &mut |e| evts.push(e));
        assert!(evts.is_empty(), "nothing found: no frame, only ReqDone");
        let found: Vec<WidePath> = (0..MAX_LIST as usize + 3).map(|i| wp(&format!(r"C:\made-up\p{i}"))).collect();
        send(ReqId(4), &found, &mut |e| evts.push(e));
        assert_eq!(evts.len(), 2);
        let total: usize = evts
            .iter()
            .map(|e| match e {
                ShellEvt::Pins { req: ReqId(4), pins } => pins.len(),
                e => panic!("{e:?}"),
            })
            .sum();
        assert_eq!(total, found.len());
    }

    #[test]
    fn the_pin_verb_is_not_implemented() {
        assert_eq!(pin(&[], true), E_NOTIMPL.0);
        assert_eq!(pin(&[], false), E_NOTIMPL.0);
    }
}
