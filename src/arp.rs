//! Add/Remove Programs entry (`HKLM\...\Uninstall\mini-oled-dock`).
//!
//! The key is written only by the elevated process, from `autostart::enable()` (the `--install` and
//! power-popup autostart paths), and deleted by `--uninstall`. It lives in the 64-bit view of HKLM
//! (admin-only). Besides the usual values it carries `QuietUninstallString`, so winget and Intune get
//! a silent uninstall.

use std::path::Path;

use windows::core::HSTRING;
use windows::Win32::Foundation::ERROR_SUCCESS;
use windows::Win32::System::Registry::{
    RegCloseKey, RegCreateKeyExW, RegSetValueExW, HKEY, HKEY_LOCAL_MACHINE, KEY_SET_VALUE, KEY_WOW64_64KEY, REG_DWORD,
    REG_OPTION_NON_VOLATILE, REG_SZ,
};

/// The Uninstall key under `HKEY_LOCAL_MACHINE`.
pub const ARP_KEY: &str = r"SOFTWARE\Microsoft\Windows\CurrentVersion\Uninstall\mini-oled-dock";

/// One registry value of the entry.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RegValue {
    Sz(String),
    Dword(u32),
}

/// The values of the entry for an install in `install_dir`. Pure. `exe_bytes` = size of `dock.exe`,
/// reported in KB rounded up (`EstimatedSize`).
pub fn arp_values(install_dir: &Path, version: &str, exe_bytes: u64) -> Vec<(&'static str, RegValue)> {
    let dir = install_dir.to_string_lossy().into_owned();
    let exe = install_dir.join("dock.exe").to_string_lossy().into_owned();
    let sz = |s: String| RegValue::Sz(s);
    let kb = u32::try_from(exe_bytes.div_ceil(1024)).unwrap_or(u32::MAX);
    vec![
        ("DisplayName", sz("mini-oled-dock".into())),
        ("DisplayVersion", sz(version.into())),
        ("Publisher", sz("Ops Software".into())),
        ("InstallLocation", sz(dir)),
        ("DisplayIcon", sz(format!("{exe},0"))),
        ("UninstallString", sz(format!("\"{exe}\" --uninstall --remove"))),
        ("QuietUninstallString", sz(format!("\"{exe}\" --uninstall --remove --quiet"))),
        ("NoModify", RegValue::Dword(1)),
        ("NoRepair", RegValue::Dword(1)),
        ("EstimatedSize", RegValue::Dword(kb)),
    ]
}

/// Writes the entry for the `dock.exe` in `install_dir` (creating or updating the key). Elevated only.
pub fn write(install_dir: &Path) -> Result<(), String> {
    let exe = install_dir.join("dock.exe");
    let bytes = std::fs::metadata(&exe).map_err(|e| format!("{}: {e}", exe.display()))?.len();
    let values = arp_values(install_dir, env!("CARGO_PKG_VERSION"), bytes);
    let mut key = HKEY::default();
    // SAFETY: `ARP_KEY` becomes a NUL-terminated HSTRING for the call; `key` is a writable HKEY.
    let r = unsafe {
        RegCreateKeyExW(
            HKEY_LOCAL_MACHINE,
            &HSTRING::from(ARP_KEY),
            None,
            None,
            REG_OPTION_NON_VOLATILE,
            KEY_SET_VALUE | KEY_WOW64_64KEY,
            None,
            &mut key,
            None,
        )
    };
    if r != ERROR_SUCCESS {
        return Err(format!("creating HKLM\\{ARP_KEY}: {}", windows::core::Error::from(r.to_hresult())));
    }
    let result = values.iter().try_for_each(|(name, value)| set_value(key, name, value));
    // SAFETY: `key` was opened above and is closed once, on every path.
    unsafe {
        let _ = RegCloseKey(key);
    }
    result
}

fn set_value(key: HKEY, name: &str, value: &RegValue) -> Result<(), String> {
    let (kind, data) = match value {
        // REG_SZ: UTF-16 LE including the terminating NUL; the length is in bytes.
        RegValue::Sz(s) => (REG_SZ, s.encode_utf16().chain(std::iter::once(0)).flat_map(u16::to_le_bytes).collect()),
        RegValue::Dword(d) => (REG_DWORD, d.to_le_bytes().to_vec()),
    };
    // SAFETY: `key` is an open key with KEY_SET_VALUE; the name is a NUL-terminated HSTRING and
    // `data` lives for the call.
    let r = unsafe { RegSetValueExW(key, &HSTRING::from(name), None, kind, Some(data.as_slice())) };
    if r == ERROR_SUCCESS {
        Ok(())
    } else {
        Err(format!("writing {name}: {}", windows::core::Error::from(r.to_hresult())))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn arp_values_for_install_dir() {
        let v = arp_values(Path::new(r"C:\Program Files\mini-oled-dock"), "1.4.0", 700_001);
        let get = |k: &str| v.iter().find(|(n, _)| *n == k).map(|(_, x)| x.clone());
        let sz = |s: &str| Some(RegValue::Sz(s.to_string()));
        assert_eq!(get("DisplayName"), sz("mini-oled-dock"));
        assert_eq!(get("DisplayVersion"), sz("1.4.0"));
        assert_eq!(get("Publisher"), sz("Ops Software"));
        assert_eq!(get("InstallLocation"), sz(r"C:\Program Files\mini-oled-dock"));
        assert_eq!(get("DisplayIcon"), sz(r"C:\Program Files\mini-oled-dock\dock.exe,0"));
        assert_eq!(get("UninstallString"), sz(r#""C:\Program Files\mini-oled-dock\dock.exe" --uninstall --remove"#));
        assert_eq!(
            get("QuietUninstallString"),
            sz(r#""C:\Program Files\mini-oled-dock\dock.exe" --uninstall --remove --quiet"#)
        );
        assert_eq!(get("NoModify"), Some(RegValue::Dword(1)));
        assert_eq!(get("NoRepair"), Some(RegValue::Dword(1)));
        assert_eq!(get("EstimatedSize"), Some(RegValue::Dword(684))); // KB, rounded up
        assert_eq!(v.len(), 10);
    }
}
