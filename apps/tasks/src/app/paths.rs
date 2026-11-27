//! Our folders (F-T2): %APPDATA%\mini-oled-dock (tasks.json) and %LOCALAPPDATA%\mini-oled-dock\tasks (state.json,
//! names.bin, the log).

use std::path::{Path, PathBuf};

use windows::core::GUID;
use windows::Win32::UI::Shell::{FOLDERID_LocalAppData, FOLDERID_RoamingAppData};

/// The suite's folder name under both roots.
const SUITE_DIR: &str = "mini-oled-dock";
/// The app's folder under the local root.
const APP_DIR: &str = "tasks";

/// None: no such folder (nothing is read or written there).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Paths {
    pub roaming: Option<PathBuf>,
    pub local: Option<PathBuf>,
}

/// The folders of this user, created when missing (they inherit the user profile's DACL). A folder that cannot be made
/// is None: everything that needs it runs from defaults and writes nothing.
pub fn init() -> Paths {
    from_folders(oled_sys::datadir::known_folder, |p| std::fs::create_dir_all(p).is_ok())
}

/// Pure over a known-folder lookup and a folder maker.
pub fn from_folders(folder: impl Fn(&GUID) -> Option<PathBuf>, make: impl Fn(&Path) -> bool) -> Paths {
    let roaming = folder(&FOLDERID_RoamingAppData).map(|r| r.join(SUITE_DIR)).filter(|p| make(p));
    let local = folder(&FOLDERID_LocalAppData).map(|l| l.join(SUITE_DIR).join(APP_DIR)).filter(|p| make(p));
    Paths { roaming, local }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;

    #[test]
    fn paths_from_known_folders() {
        let made = RefCell::new(Vec::new());
        let p = from_folders(
            |id| {
                if *id == FOLDERID_RoamingAppData {
                    Some(PathBuf::from("R:\\roam"))
                } else if *id == FOLDERID_LocalAppData {
                    Some(PathBuf::from("L:\\loc"))
                } else {
                    None
                }
            },
            |d| {
                made.borrow_mut().push(d.to_path_buf());
                true
            },
        );
        assert_eq!(p.roaming, Some(PathBuf::from("R:\\roam\\mini-oled-dock")));
        assert_eq!(p.local, Some(PathBuf::from("L:\\loc\\mini-oled-dock\\tasks")));
        assert_eq!(made.borrow().len(), 2, "both folders were made");
    }

    #[test]
    fn a_missing_or_unmakeable_folder_is_none() {
        let none = from_folders(|_| None, |_| true);
        assert_eq!(none, Paths::default());
        let half = from_folders(
            |_| Some(PathBuf::from("X:\\any")),
            |d| !d.ends_with(APP_DIR), // the local folder cannot be made
        );
        assert!(half.roaming.is_some() && half.local.is_none());
    }
}
