//! What a window says about its program: the AppUserModelID it publishes. COM (STA) must be initialized on the calling
//! thread.

use std::ffi::c_void;

use windows::Win32::Foundation::{HWND, PROPERTYKEY};
use windows::Win32::System::Com::CoTaskMemFree;
use windows::Win32::System::Com::StructuredStorage::{PropVariantClear, PropVariantToStringAlloc};
use windows::Win32::UI::Shell::PropertiesSystem::{IPropertyStore, SHGetPropertyStoreForWindow};

/// `PKEY_AppUserModel_ID` {9F4C2855-9F79-4B39-A8D0-E1D42DE1D5F3}, 5. Defined here to avoid depending
/// on the `Win32_Storage_EnhancedStorage` feature (which exports thousands of constants).
const PKEY_APP_USER_MODEL_ID: PROPERTYKEY =
    PROPERTYKEY { fmtid: windows::core::GUID::from_u128(0x9f4c2855_9f79_4b39_a8d0_e1d42de1d5f3), pid: 5 };

/// AUMID published by the window (`PKEY_AppUserModel_ID`) through `SHGetPropertyStoreForWindow`; None when the window
/// publishes none, does not exist, or COM is not initialized on this thread.
pub fn aumid(hwnd: isize) -> Option<String> {
    let hwnd = HWND(hwnd as *mut c_void);
    // SAFETY: arbitrary HWND allowed (fails if the window does not exist). The PROPVARIANT returned
    // by GetValue is owned by us: it must be freed with PropVariantClear (the type has no Drop).
    // The string allocated by PropVariantToStringAlloc must be freed with CoTaskMemFree.
    unsafe {
        let store: IPropertyStore = SHGetPropertyStoreForWindow(hwnd).ok()?;
        let mut pv = store.GetValue(&PKEY_APP_USER_MODEL_ID).ok()?;
        let out = match PropVariantToStringAlloc(&pv) {
            Ok(p) => {
                let s = p.to_string().ok();
                CoTaskMemFree(Some(p.0 as *const c_void));
                s
            }
            Err(_) => None,
        };
        let _ = PropVariantClear(&mut pv);
        out.filter(|s| !s.is_empty())
    }
}

#[cfg(test)]
mod tests {
    use windows::Win32::System::Com::{CoInitializeEx, CoUninitialize, COINIT_APARTMENTTHREADED};

    use super::aumid;

    /// A handle that names no window (and the null one) gives no AUMID and no panic; no window is created.
    #[test]
    fn a_missing_window_has_no_aumid() {
        // SAFETY: COM STA for this test thread, paired with CoUninitialize below.
        let hr = unsafe { CoInitializeEx(None, COINIT_APARTMENTTHREADED) };
        assert_eq!(aumid(0), None);
        assert_eq!(aumid(0x7FFF_FFF0), None);
        if hr.is_ok() {
            // SAFETY: paired with the successful CoInitializeEx above.
            unsafe { CoUninitialize() };
        }
    }
}
