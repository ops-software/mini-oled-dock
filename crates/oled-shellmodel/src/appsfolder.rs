//! The catalogue name of an AppUserModelID. Needs COM (STA) initialized on the calling thread.

use windows::core::HSTRING;
use windows::Win32::System::Com::IBindCtx;
use windows::Win32::UI::Shell::{IShellItem, SHCreateItemFromParsingName, SIGDN_NORMALDISPLAY};

use crate::version::take_pwstr;

/// Name from the `shell:AppsFolder\<AUMID>` catalog (`SIGDN_NORMALDISPLAY`); None when the catalogue does not know the
/// AUMID (the AUMID itself or a '!' comes back).
#[inline]
pub fn display_name(aumid: &str) -> Option<String> {
    let path = HSTRING::from(format!("shell:AppsFolder\\{aumid}"));
    // SAFETY: COM STA initialized on the thread; `path` lives for the call.
    let item: IShellItem = unsafe { SHCreateItemFromParsingName(&path, None::<&IBindCtx>) }.ok()?;
    // SAFETY: `item` is valid; `GetDisplayName` returns a CoTaskMemAlloc'd string, which `take_pwstr` frees.
    let name = unsafe { take_pwstr(item.GetDisplayName(SIGDN_NORMALDISPLAY).ok()?) }?;
    // If the catalog does not know the AUMID some paths return the AUMID itself: discarded.
    if name.eq_ignore_ascii_case(aumid) || name.contains('!') {
        return None;
    }
    Some(name)
}
