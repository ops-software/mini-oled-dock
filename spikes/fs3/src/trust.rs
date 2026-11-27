//! FS-6's rule for "Microsoft's module": only a valid signature that chains to a Microsoft root counts, never the
//! folder a DLL sits in (vendor DLLs in System32 are common). The embedded signature is tried first (WinVerifyTrust,
//! WINTRUST_ACTION_GENERIC_VERIFY_V2), then the catalog signature (CryptCATAdmin* to find the catalog, then
//! WinVerifyTrust with WTD_CHOICE_CATALOG); the signer's chain must pass
//! CertVerifyCertificateChainPolicy(CERT_CHAIN_POLICY_MICROSOFT_ROOT); the version resource's CompanyName is a
//! cross-check, and a valid Microsoft chain with another CompanyName counts as not Microsoft. No revocation check and
//! cache-only URL retrieval: nothing goes to the network.

use std::os::windows::ffi::OsStrExt;
use std::path::Path;

use windows::core::{w, GUID, PCWSTR};
use windows::Win32::Foundation::{CloseHandle, GENERIC_READ, HANDLE, HMODULE, HWND, TRUST_E_NOSIGNATURE};
use windows::Win32::Security::Cryptography::Catalog::{
    CryptCATAdminAcquireContext2, CryptCATAdminCalcHashFromFileHandle2, CryptCATAdminEnumCatalogFromHash,
    CryptCATAdminReleaseCatalogContext, CryptCATAdminReleaseContext, CryptCATCatalogInfoFromContext, CATALOG_INFO,
};
use windows::Win32::Security::Cryptography::{
    CertVerifyCertificateChainPolicy, CERT_CHAIN_POLICY_FLAGS, CERT_CHAIN_POLICY_MICROSOFT_ROOT,
    CERT_CHAIN_POLICY_PARA, CERT_CHAIN_POLICY_STATUS,
};
use windows::Win32::Security::WinTrust::{
    WTHelperGetProvSignerFromChain, WTHelperProvDataFromStateData, WinVerifyTrust, DRIVER_ACTION_VERIFY,
    WINTRUST_ACTION_GENERIC_VERIFY_V2, WINTRUST_CATALOG_INFO, WINTRUST_DATA, WINTRUST_DATA_0, WINTRUST_FILE_INFO,
    WTD_CACHE_ONLY_URL_RETRIEVAL, WTD_CHOICE_CATALOG, WTD_CHOICE_FILE, WTD_REVOKE_NONE, WTD_STATEACTION_CLOSE,
    WTD_STATEACTION_VERIFY, WTD_UI_NONE,
};
use windows::Win32::Storage::FileSystem::{
    CreateFileW, GetFileVersionInfoSizeW, GetFileVersionInfoW, VerQueryValueW, FILE_ATTRIBUTE_NORMAL,
    FILE_SHARE_DELETE, FILE_SHARE_READ, OPEN_EXISTING,
};
use windows::Win32::System::Diagnostics::ToolHelp::{
    CreateToolhelp32Snapshot, Module32FirstW, Module32NextW, MODULEENTRY32W, TH32CS_SNAPMODULE,
};
use windows::Win32::System::LibraryLoader::GetModuleFileNameW;

use crate::util::{from_wide, pcwstr, wide_os, wide_str};

/// Where the signature that was checked lives.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    Embedded,
    Catalog,
    Unsigned,
}

/// What the checks found for one file.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Verdict {
    pub kind: Kind,
    /// WinVerifyTrust's result for the signature checked (0: valid).
    pub trust: i32,
    /// The signer's chain ends in a Microsoft root.
    pub microsoft_root: bool,
    pub company: Option<String>,
}

impl Verdict {
    pub fn company_is_microsoft(&self) -> bool {
        self.company.as_deref() == Some("Microsoft Corporation")
    }

    fn valid_microsoft_chain(&self) -> bool {
        self.kind != Kind::Unsigned && self.trust == 0 && self.microsoft_root
    }

    /// Microsoft's: a valid embedded or catalog signature, a Microsoft root, and CompanyName "Microsoft Corporation".
    pub fn microsoft(&self) -> bool {
        self.valid_microsoft_chain() && self.company_is_microsoft()
    }

    /// A valid Microsoft chain with another CompanyName: reported, and counted as not Microsoft.
    pub fn mismatch(&self) -> bool {
        self.valid_microsoft_chain() && !self.company_is_microsoft()
    }
}

/// INVALID_HANDLE_VALUE as the window: no interactive user, no UI.
fn no_ui() -> HWND {
    HWND(-1isize as *mut core::ffi::c_void)
}

/// The signer's chain of a verified state ends in a Microsoft root.
fn chain_is_microsoft(state: HANDLE) -> bool {
    // SAFETY: state comes from a successful WTD_STATEACTION_VERIFY and is closed only after this returns; every
    // pointer is checked before it is read.
    unsafe {
        let prov = WTHelperProvDataFromStateData(state);
        if prov.is_null() {
            return false;
        }
        let sgnr = WTHelperGetProvSignerFromChain(prov, 0, false, 0);
        if sgnr.is_null() || (*sgnr).pChainContext.is_null() {
            return false;
        }
        let para = CERT_CHAIN_POLICY_PARA {
            cbSize: std::mem::size_of::<CERT_CHAIN_POLICY_PARA>() as u32,
            dwFlags: CERT_CHAIN_POLICY_FLAGS(0),
            pvExtraPolicyPara: std::ptr::null_mut(),
        };
        let mut status = CERT_CHAIN_POLICY_STATUS {
            cbSize: std::mem::size_of::<CERT_CHAIN_POLICY_STATUS>() as u32,
            ..Default::default()
        };
        CertVerifyCertificateChainPolicy(CERT_CHAIN_POLICY_MICROSOFT_ROOT, (*sgnr).pChainContext, &para, &mut status)
            .as_bool()
            && status.dwError == 0
    }
}

/// WinVerifyTrust on prepared data: (result, chain ends in a Microsoft root); the state is always closed.
fn verify(data: &mut WINTRUST_DATA) -> (i32, bool) {
    let mut action: GUID = WINTRUST_ACTION_GENERIC_VERIFY_V2;
    data.dwStateAction = WTD_STATEACTION_VERIFY;
    // SAFETY: data and the structure its union points to live across both calls.
    let r = unsafe { WinVerifyTrust(no_ui(), &mut action, data as *mut WINTRUST_DATA as *mut core::ffi::c_void) };
    let root = r == 0 && chain_is_microsoft(data.hWVTStateData);
    data.dwStateAction = WTD_STATEACTION_CLOSE;
    // SAFETY: closes the state of the call above.
    let _ = unsafe { WinVerifyTrust(no_ui(), &mut action, data as *mut WINTRUST_DATA as *mut core::ffi::c_void) };
    (r, root)
}

fn base_data(
    choice: windows::Win32::Security::WinTrust::WINTRUST_DATA_UNION_CHOICE,
    u: WINTRUST_DATA_0,
) -> WINTRUST_DATA {
    WINTRUST_DATA {
        cbStruct: std::mem::size_of::<WINTRUST_DATA>() as u32,
        dwUIChoice: WTD_UI_NONE,
        fdwRevocationChecks: WTD_REVOKE_NONE,
        dwUnionChoice: choice,
        Anonymous: u,
        dwProvFlags: WTD_CACHE_ONLY_URL_RETRIEVAL,
        ..Default::default()
    }
}

fn embedded(w: &[u16]) -> (i32, bool) {
    let mut fi = WINTRUST_FILE_INFO {
        cbStruct: std::mem::size_of::<WINTRUST_FILE_INFO>() as u32,
        pcwszFilePath: pcwstr(w),
        hFile: HANDLE::default(),
        pgKnownSubject: std::ptr::null_mut(),
    };
    let mut data = base_data(WTD_CHOICE_FILE, WINTRUST_DATA_0 { pFile: &mut fi });
    verify(&mut data)
}

/// The catalog signature of the file, when a catalog of the system lists its hash (SHA-256 catalogs, then SHA-1).
fn catalog(w: &[u16]) -> Option<(i32, bool)> {
    // SAFETY: a read-only handle on the file, closed at the end.
    let file = unsafe {
        CreateFileW(
            pcwstr(w),
            GENERIC_READ.0,
            FILE_SHARE_READ | FILE_SHARE_DELETE,
            None,
            OPEN_EXISTING,
            FILE_ATTRIBUTE_NORMAL,
            None,
        )
    }
    .ok()?;
    let mut out = None;
    for alg in [Some("SHA256"), None] {
        let algw = alg.map(wide_str);
        let mut admin = 0isize;
        // SAFETY: admin receives a context released below; the algorithm name outlives the call.
        let ok = unsafe {
            CryptCATAdminAcquireContext2(
                &mut admin,
                Some(&DRIVER_ACTION_VERIFY),
                algw.as_deref().map_or(PCWSTR::null(), pcwstr),
                None,
                None,
            )
        };
        if ok.is_err() {
            continue;
        }
        let mut n = 0u32;
        // SAFETY: the first call only reports the hash size.
        let _ = unsafe { CryptCATAdminCalcHashFromFileHandle2(admin, file, &mut n, None, None) };
        let mut hash = vec![0u8; n as usize];
        // SAFETY: hash holds n bytes.
        let hashed = n > 0
            && unsafe { CryptCATAdminCalcHashFromFileHandle2(admin, file, &mut n, Some(hash.as_mut_ptr()), None) }
                .is_ok();
        // SAFETY: a catalog context of admin, released below.
        let cat = if hashed { unsafe { CryptCATAdminEnumCatalogFromHash(admin, &hash, None, None) } } else { 0 };
        if cat != 0 {
            let mut info = CATALOG_INFO { cbStruct: std::mem::size_of::<CATALOG_INFO>() as u32, ..Default::default() };
            // SAFETY: info is sized; the catalog context is valid.
            if unsafe { CryptCATCatalogInfoFromContext(cat, &mut info, 0) }.is_ok() {
                let tag = wide_str(&hash.iter().map(|b| format!("{b:02X}")).collect::<String>());
                let mut ci = WINTRUST_CATALOG_INFO {
                    cbStruct: std::mem::size_of::<WINTRUST_CATALOG_INFO>() as u32,
                    dwCatalogVersion: 0,
                    pcwszCatalogFilePath: PCWSTR(info.wszCatalogFile.as_ptr()),
                    pcwszMemberTag: pcwstr(&tag),
                    pcwszMemberFilePath: pcwstr(w),
                    hMemberFile: file,
                    pbCalculatedFileHash: hash.as_mut_ptr(),
                    cbCalculatedFileHash: n,
                    pcCatalogContext: std::ptr::null_mut(),
                    hCatAdmin: admin,
                };
                let mut data = base_data(WTD_CHOICE_CATALOG, WINTRUST_DATA_0 { pCatalog: &mut ci });
                out = Some(verify(&mut data));
            }
            // SAFETY: releases the catalog context of admin.
            let _ = unsafe { CryptCATAdminReleaseCatalogContext(admin, cat, 0) };
        }
        // SAFETY: releases the context acquired above.
        let _ = unsafe { CryptCATAdminReleaseContext(admin, 0) };
        if out.is_some() {
            break;
        }
    }
    // SAFETY: the handle opened above.
    let _ = unsafe { CloseHandle(file) };
    out
}

/// The version resource's CompanyName (the first translation that has one, then US English).
pub fn company(path: &Path) -> Option<String> {
    let w = wide_os(path.as_os_str());
    // SAFETY: w is NUL-terminated.
    let size = unsafe { GetFileVersionInfoSizeW(pcwstr(&w), None) };
    if size == 0 {
        return None;
    }
    let mut buf = vec![0u8; size as usize];
    // SAFETY: buf holds size bytes.
    unsafe { GetFileVersionInfoW(pcwstr(&w), None, size, buf.as_mut_ptr() as *mut core::ffi::c_void) }.ok()?;
    let mut keys: Vec<(u16, u16)> = Vec::new();
    let mut p: *mut core::ffi::c_void = std::ptr::null_mut();
    let mut len = 0u32;
    // SAFETY: VerQueryValueW points p into buf, which outlives every read below; len is in bytes here.
    if unsafe {
        VerQueryValueW(buf.as_ptr() as *const core::ffi::c_void, w!("\\VarFileInfo\\Translation"), &mut p, &mut len)
    }
    .as_bool()
    {
        // SAFETY: len bytes of (language, code page) pairs at p, inside buf.
        let pairs = unsafe { std::slice::from_raw_parts(p as *const u16, len as usize / 2) };
        keys.extend(pairs.chunks_exact(2).map(|c| (c[0], c[1])));
    }
    keys.extend([(0x0409, 0x04B0), (0x0409, 0x04E4), (0x0409, 0x0000)]);
    for (lang, cp) in keys {
        let key = wide_str(&format!("\\StringFileInfo\\{lang:04x}{cp:04x}\\CompanyName"));
        let mut v: *mut core::ffi::c_void = std::ptr::null_mut();
        let mut n = 0u32;
        // SAFETY: as above; for strings n counts UTF-16 units, the NUL included.
        if unsafe { VerQueryValueW(buf.as_ptr() as *const core::ffi::c_void, pcwstr(&key), &mut v, &mut n) }.as_bool()
            && n > 0
        {
            // SAFETY: n units at v, inside buf.
            let s = unsafe { std::slice::from_raw_parts(v as *const u16, n as usize) };
            return Some(from_wide(s).trim().to_string());
        }
    }
    None
}

/// The signature verdict of one file.
pub fn classify(path: &Path) -> Verdict {
    let w: Vec<u16> = path.as_os_str().encode_wide().chain(Some(0)).collect();
    let company = company(path);
    let (r, root) = embedded(&w);
    if r == 0 {
        return Verdict { kind: Kind::Embedded, trust: r, microsoft_root: root, company };
    }
    match catalog(&w) {
        Some((cr, croot)) => Verdict { kind: Kind::Catalog, trust: cr, microsoft_root: croot, company },
        // A signature that is there but does not verify stays a failed embedded one (no signature, an unknown
        // subject form or an unknown provider mean there is none).
        None if ![TRUST_E_NOSIGNATURE.0, 0x800B_0003_u32 as i32, 0x800B_0001_u32 as i32].contains(&r) => {
            Verdict { kind: Kind::Embedded, trust: r, microsoft_root: false, company }
        }
        None => Verdict { kind: Kind::Unsigned, trust: r, microsoft_root: false, company },
    }
}

fn module_path(h: HMODULE) -> Option<String> {
    let mut buf = vec![0u16; 32_768];
    // SAFETY: a module of this process; buf is writable for its length.
    let n = unsafe { GetModuleFileNameW(Some(h), &mut buf) } as usize;
    (n > 0 && n < buf.len()).then(|| String::from_utf16_lossy(&buf[..n]))
}

/// The full paths (lower case) of the modules loaded in this process (a Toolhelp module snapshot). None when five
/// attempts 50 ms apart all failed (ERROR_BAD_LENGTH while modules load) or listed nothing, so a failed snapshot never
/// reads as an empty one.
pub fn modules() -> Option<Vec<String>> {
    for attempt in 0..5 {
        if attempt > 0 {
            std::thread::sleep(std::time::Duration::from_millis(50));
        }
        // SAFETY: a snapshot of this process (0), closed below.
        let Ok(snap) = (unsafe { CreateToolhelp32Snapshot(TH32CS_SNAPMODULE, 0) }) else {
            continue; // ERROR_BAD_LENGTH while modules load: try again
        };
        let mut out = Vec::new();
        let mut me = MODULEENTRY32W { dwSize: std::mem::size_of::<MODULEENTRY32W>() as u32, ..Default::default() };
        // SAFETY: me is sized; the snapshot is valid until CloseHandle.
        let mut ok = unsafe { Module32FirstW(snap, &mut me) }.is_ok();
        while ok {
            let p = module_path(me.hModule).unwrap_or_else(|| from_wide(&me.szExePath));
            out.push(p.to_lowercase());
            // SAFETY: as above.
            ok = unsafe { Module32NextW(snap, &mut me) }.is_ok();
        }
        // SAFETY: the snapshot handle of this iteration.
        let _ = unsafe { CloseHandle(snap) };
        if out.is_empty() {
            continue; // a process always holds its exe and ntdll: an empty list is a failed walk
        }
        out.sort();
        out.dedup();
        return Some(out);
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use windows::Win32::System::SystemInformation::GetSystemDirectoryW;

    fn system_dll(name: &str) -> std::path::PathBuf {
        let mut buf = [0u16; 260];
        // SAFETY: buf is writable for its length.
        let n = unsafe { GetSystemDirectoryW(Some(&mut buf)) } as usize;
        std::path::PathBuf::from(String::from_utf16_lossy(&buf[..n])).join(name)
    }

    #[test]
    fn a_system_dll_is_microsofts() {
        let v = classify(&system_dll("kernel32.dll"));
        assert_ne!(v.kind, Kind::Unsigned, "{v:?}");
        assert!(v.microsoft(), "{v:?}");
        assert!(!v.mismatch());
    }

    #[test]
    fn an_unsigned_exe_is_not() {
        let me = std::env::current_exe().unwrap();
        let v = classify(&me);
        assert_eq!(v.kind, Kind::Unsigned, "{v:?}");
        assert!(!v.microsoft() && !v.mismatch());
    }

    #[test]
    fn company_name_and_the_mismatch_rule() {
        assert_eq!(company(&system_dll("kernel32.dll")).as_deref(), Some("Microsoft Corporation"));
        let v = Verdict { kind: Kind::Catalog, trust: 0, microsoft_root: true, company: Some("Other".into()) };
        assert!(v.mismatch() && !v.microsoft());
        let bad = Verdict {
            kind: Kind::Embedded,
            trust: 1,
            microsoft_root: false,
            company: Some("Microsoft Corporation".into()),
        };
        assert!(!bad.microsoft() && !bad.mismatch(), "a failed signature never counts");
    }

    #[test]
    fn the_module_snapshot_sees_this_process() {
        let m = modules().expect("a module snapshot of this process");
        let me = std::env::current_exe().unwrap().to_string_lossy().to_lowercase();
        assert!(m.contains(&me), "{me} not in {m:?}");
        assert!(m.iter().any(|p| p.ends_with("\\ntdll.dll")));
    }
}
