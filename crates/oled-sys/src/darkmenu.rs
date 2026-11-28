//! Dark system menus for a process whose popup menus Windows draws (TrackPopupMenuEx, the shell's context menu).
//! Windows paints them dark only in a process that opted in through uxtheme.dll, by ordinal (undocumented): 135 is
//! SetPreferredAppMode(mode) from build 18362 (Default 0, AllowDark 1, ForceDark 2) and AllowDarkModeForApp(bool) on
//! build 17763; 136 is FlushMenuThemes(). The suite is always dark, so the process forces dark and does not follow the
//! system theme. Anything missing leaves the menus light; nothing here fails or panics.

/// First build where ordinal 135 is SetPreferredAppMode.
const PREFERRED_MODE_BUILD: u32 = 18362;
/// The one older build where ordinal 135 is AllowDarkModeForApp(bool).
const ALLOW_APP_BUILD: u32 = 17763;
/// PreferredAppMode::ForceDark.
const FORCE_DARK: i32 = 2;

/// What to call for a Windows build.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Plan {
    /// Ordinal 135 does not have a known meaning here: call nothing.
    Nothing,
    /// Ordinal 135 as AllowDarkModeForApp(true).
    AllowApp,
    /// Ordinal 135 as SetPreferredAppMode(mode).
    PreferredMode(i32),
}

/// The call that fits `build` (the Windows build number; 0 when unknown).
pub fn plan(build: u32) -> Plan {
    if build >= PREFERRED_MODE_BUILD {
        Plan::PreferredMode(FORCE_DARK)
    } else if (ALLOW_APP_BUILD..PREFERRED_MODE_BUILD).contains(&build) {
        Plan::AllowApp
    } else {
        Plan::Nothing
    }
}

/// Opts this process in to dark system menus. Call once, early, before any menu is shown. Safe to call on any build and
/// when uxtheme lacks the ordinals: the menus then stay light.
#[cfg(windows)]
pub fn init() {
    use windows::core::{w, PCSTR};
    use windows::Win32::System::LibraryLoader::{GetProcAddress, LoadLibraryExW, LOAD_LIBRARY_SEARCH_SYSTEM32};

    let p = plan(build_number());
    if p == Plan::Nothing {
        return;
    }
    // SAFETY: a fixed name, loaded from the system directory only; the handle is kept for the life of the process.
    let Ok(ux) = (unsafe { LoadLibraryExW(w!("uxtheme.dll"), None, LOAD_LIBRARY_SEARCH_SYSTEM32) }) else {
        return;
    };
    let ordinal = |n: usize| PCSTR(n as *const u8);
    // SAFETY: `ux` is a loaded module; a missing ordinal gives None. The transmutes give each export the signature it
    // has on the builds chosen by plan() (extern "system", one argument, and no argument for the flush).
    unsafe {
        let Some(set) = GetProcAddress(ux, ordinal(135)) else { return };
        match p {
            Plan::PreferredMode(mode) => {
                let f: extern "system" fn(i32) -> i32 = std::mem::transmute(set);
                f(mode);
            }
            Plan::AllowApp => {
                let f: extern "system" fn(i32) -> i32 = std::mem::transmute(set);
                f(1);
            }
            Plan::Nothing => return,
        }
        if let Some(flush) = GetProcAddress(ux, ordinal(136)) {
            let f: extern "system" fn() = std::mem::transmute(flush);
            f();
        }
    }
}

#[cfg(not(windows))]
pub fn init() {}

#[cfg(windows)]
fn build_number() -> u32 {
    use windows::Wdk::System::SystemServices::RtlGetVersion;
    use windows::Win32::System::SystemInformation::OSVERSIONINFOW;
    let mut v =
        OSVERSIONINFOW { dwOSVersionInfoSize: std::mem::size_of::<OSVERSIONINFOW>() as u32, ..Default::default() };
    // SAFETY: `v` is a valid local OSVERSIONINFOW with its size set.
    if unsafe { RtlGetVersion(&mut v) }.is_err() || v.dwMajorVersion != 10 {
        return 0;
    }
    v.dwBuildNumber
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plan_by_build() {
        assert_eq!(plan(0), Plan::Nothing);
        assert_eq!(plan(17134), Plan::Nothing);
        assert_eq!(plan(17762), Plan::Nothing);
        assert_eq!(plan(17763), Plan::AllowApp);
        assert_eq!(plan(18361), Plan::AllowApp);
        assert_eq!(plan(18362), Plan::PreferredMode(2));
        assert_eq!(plan(22631), Plan::PreferredMode(2));
        assert_eq!(plan(26300), Plan::PreferredMode(2));
    }

    #[test]
    fn init_never_panics() {
        init();
    }
}
