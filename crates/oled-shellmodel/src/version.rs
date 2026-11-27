//! Version-resource strings of an executable, read through `IShellItem2::GetString`. Needs COM (STA) initialized
//! on the calling thread.

use std::ffi::c_void;

use windows::core::{GUID, HSTRING, PWSTR};
use windows::Win32::Foundation::PROPERTYKEY;
use windows::Win32::System::Com::{CoTaskMemFree, IBindCtx};
use windows::Win32::UI::Shell::{IShellItem2, SHCreateItemFromParsingName};

/// `PKEY_FileDescription` ({0CEF7D53-FA64-11D1-A203-0000F81FEDEE}, 3). Defined here to avoid
/// enabling the `Win32_Storage_EnhancedStorage` feature just for a constant.
const PKEY_FILE_DESCRIPTION: PROPERTYKEY =
    PROPERTYKEY { fmtid: GUID::from_u128(0x0cef7d53_fa64_11d1_a203_0000f81fedee), pid: 3 };

/// `PKEY_Software_ProductName` (same format id as FileDescription, 7).
const PKEY_PRODUCT_NAME: PROPERTYKEY =
    PROPERTYKEY { fmtid: GUID::from_u128(0x0cef7d53_fa64_11d1_a203_0000f81fedee), pid: 7 };
/// `PKEY_Company` ({D5CDD502-2E9C-101B-9397-08002B2CF9AE}, 15).
const PKEY_COMPANY: PROPERTYKEY =
    PROPERTYKEY { fmtid: GUID::from_u128(0xd5cdd502_2e9c_101b_9397_08002b2cf9ae), pid: 15 };

/// Version-resource strings of an executable: what the shell shows as its description, product and company.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct FileInfo {
    pub description: Option<String>,
    pub product: Option<String>,
    pub company: Option<String>,
}

impl FileInfo {
    /// The strings of the file at `exe_path`; all `None` when it cannot be opened or has none.
    #[inline]
    pub fn read(exe_path: &str) -> FileInfo {
        let path = HSTRING::from(exe_path);
        // SAFETY: COM STA initialized; `path` lives for the call.
        let Ok(item) = (unsafe { SHCreateItemFromParsingName::<_, _, IShellItem2>(&path, None::<&IBindCtx>) }) else {
            return FileInfo::default();
        };
        let get = |key: &PROPERTYKEY| {
            // SAFETY: `item` is valid, key is valid; `IShellItem2::GetString` returns a CoTaskMemAlloc'd string,
            // which `take_pwstr` frees.
            unsafe { item.GetString(key).ok().and_then(|p| take_pwstr(p)) }
        };
        FileInfo {
            description: get(&PKEY_FILE_DESCRIPTION),
            product: get(&PKEY_PRODUCT_NAME),
            company: get(&PKEY_COMPANY),
        }
    }
}

/// Frees a `PWSTR` allocated by COM (`CoTaskMemAlloc`) and extracts its string.
///
/// # Safety
/// `p` is null, or a NUL-terminated wide string allocated by `CoTaskMemAlloc`; it is not used after the call.
#[inline]
pub unsafe fn take_pwstr(p: PWSTR) -> Option<String> {
    if p.is_null() {
        return None;
    }
    // SAFETY: `p` is a NUL-terminated wide string returned by a shell API; it is read
    // once and then freed with CoTaskMemFree, as required by the GetDisplayName/GetString contract.
    let s = unsafe { p.to_string() }.ok();
    // SAFETY: same pointer, allocated by COM, not used after this line.
    unsafe { CoTaskMemFree(Some(p.0 as *const c_void)) };
    s.map(|s| s.trim().to_string()).filter(|s| !s.is_empty())
}
