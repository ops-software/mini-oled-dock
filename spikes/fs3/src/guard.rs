//! Where the probes may write: a folder under the user's temporary folder whose first component below it starts with
//! "mod-files-" (the TempRoot and LiveRoot rule of the files plan, D2), with no reparse point anywhere on its path
//! (a junction, a symbolic link, a mount point or a cloud placeholder would lead a delete somewhere else). Every row
//! refuses to start otherwise, and nothing is created before the checks pass.

use std::ffi::OsString;
use std::os::windows::ffi::OsStringExt;
use std::os::windows::fs::MetadataExt;
use std::path::{Component, Path, PathBuf, Prefix};

use windows::Win32::Storage::FileSystem::GetLongPathNameW;

use crate::util::{pcwstr, wide_os};

/// The first component below the temporary folder starts with this.
pub const PREFIX: &str = "mod-files-";

const FILE_ATTRIBUTE_REPARSE_POINT: u32 = 0x400;

/// Why a path was refused.
#[derive(Debug, PartialEq, Eq)]
pub enum Refusal {
    NotAbsolute,
    NotUnicode,
    BadComponent(String),
    NotUnderTemp,
    NoPrefix,
    ReparsePoint(PathBuf),
    Io(String),
}

impl std::fmt::Display for Refusal {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Refusal::NotAbsolute => write!(f, "not an absolute drive path"),
            Refusal::NotUnicode => write!(f, "the path is not valid Unicode"),
            Refusal::BadComponent(c) => write!(f, "component {c:?} is not allowed"),
            Refusal::NotUnderTemp => write!(f, "not under the temporary folder"),
            Refusal::NoPrefix => write!(f, "the first folder below the temporary folder must start with {PREFIX}"),
            Refusal::ReparsePoint(p) => write!(f, "{} is a reparse point", p.display()),
            Refusal::Io(e) => write!(f, "{e}"),
        }
    }
}

/// The drive letter (upper case) and the normal components of an absolute drive path, verbatim or not. Refuses
/// "." and ".." (a verbatim path keeps them), a colon (an alternate data stream) and the characters Windows
/// never allows in a name.
fn parts(p: &Path) -> Result<(u8, Vec<String>), Refusal> {
    let mut comps = p.components();
    let drive = match comps.next() {
        Some(Component::Prefix(pre)) => match pre.kind() {
            Prefix::Disk(d) | Prefix::VerbatimDisk(d) => d.to_ascii_uppercase(),
            _ => return Err(Refusal::NotUnderTemp),
        },
        _ => return Err(Refusal::NotAbsolute),
    };
    if comps.next() != Some(Component::RootDir) {
        return Err(Refusal::NotAbsolute);
    }
    let mut out = Vec::new();
    for c in comps {
        match c {
            Component::Normal(s) => {
                let s = s.to_str().ok_or(Refusal::NotUnicode)?;
                if s == "." || s == ".." || s.contains([':', '<', '>', '"', '|', '?', '*']) {
                    return Err(Refusal::BadComponent(s.to_string()));
                }
                out.push(s.to_string());
            }
            Component::CurDir | Component::ParentDir => {
                return Err(Refusal::BadComponent(c.as_os_str().to_string_lossy().into()))
            }
            Component::Prefix(_) | Component::RootDir => return Err(Refusal::NotAbsolute),
        }
    }
    Ok((drive, out))
}

fn same(a: &str, b: &str) -> bool {
    a.to_lowercase() == b.to_lowercase()
}

/// Pure: `root` lies strictly below `temp` (compared component by component, case-insensitive) and its first
/// component below `temp` starts with "mod-files-" and goes on after it.
pub fn check_lexical(root: &Path, temp: &Path) -> Result<(), Refusal> {
    let (rd, rc) = parts(root)?;
    let (td, tc) = parts(temp).map_err(|_| Refusal::NotUnderTemp)?;
    if rd != td || rc.len() <= tc.len() || !rc.iter().zip(&tc).all(|(a, b)| same(a, b)) {
        return Err(Refusal::NotUnderTemp);
    }
    let first = &rc[tc.len()];
    let prefixed = first.get(..PREFIX.len()).is_some_and(|p| p.eq_ignore_ascii_case(PREFIX));
    if !prefixed || first.len() == PREFIX.len() {
        return Err(Refusal::NoPrefix);
    }
    Ok(())
}

/// True when `p` is `root` or lies inside it, by components. Every path a row hands to the shell passes it first.
pub fn inside(root: &Path, p: &Path) -> bool {
    match (parts(root), parts(p)) {
        (Ok((rd, rc)), Ok((pd, pc))) => rd == pd && pc.len() >= rc.len() && rc.iter().zip(&pc).all(|(a, b)| same(a, b)),
        _ => false,
    }
}

/// `p` with its deepest existing ancestor expanded to long names (GetLongPathNameW: the temporary folder can come
/// in 8.3 form, as on CI runners) and the rest appended as given.
pub fn long_name(p: &Path) -> Result<PathBuf, Refusal> {
    let mut existing = p.to_path_buf();
    let mut rest: Vec<OsString> = Vec::new();
    while std::fs::symlink_metadata(&existing).is_err() {
        match (existing.file_name(), existing.parent()) {
            (Some(n), Some(parent)) => {
                rest.push(n.to_owned());
                existing = parent.to_path_buf();
            }
            _ => return Err(Refusal::Io(format!("no existing ancestor of {}", p.display()))),
        }
    }
    let w = wide_os(existing.as_os_str());
    // SAFETY: w is NUL-terminated and outlives both calls; the second call writes at most buf.len() units.
    let need = unsafe { GetLongPathNameW(pcwstr(&w), None) };
    if need == 0 {
        return Err(Refusal::Io(format!("GetLongPathNameW failed on {}", existing.display())));
    }
    let mut buf = vec![0u16; need as usize];
    // SAFETY: as above.
    let got = unsafe { GetLongPathNameW(pcwstr(&w), Some(&mut buf)) } as usize;
    if got == 0 || got >= buf.len() {
        return Err(Refusal::Io(format!("GetLongPathNameW failed on {}", existing.display())));
    }
    let mut out = PathBuf::from(OsString::from_wide(&buf[..got]));
    for c in rest.iter().rev() {
        out.push(c);
    }
    Ok(out)
}

/// Refuses when any existing component of `p`, from the drive root down, is a reparse point.
pub fn no_reparse(p: &Path) -> Result<(), Refusal> {
    let mut cur = PathBuf::new();
    for c in p.components() {
        cur.push(c.as_os_str());
        if matches!(c, Component::Prefix(_) | Component::RootDir) {
            continue;
        }
        match std::fs::symlink_metadata(&cur) {
            Ok(m) if m.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0 => return Err(Refusal::ReparsePoint(cur)),
            Ok(_) => {}
            Err(_) => break, // the rest does not exist yet
        }
    }
    Ok(())
}

/// The temporary folder in long-name form.
pub fn temp_dir() -> Result<PathBuf, Refusal> {
    long_name(&std::env::temp_dir())
}

/// Checks `root` and creates it: absolute, under the temporary folder, first component "mod-files-*", no reparse
/// point before or after the folders are made. Returns the long-name form every row works with.
pub fn prepare_root(root: &Path) -> Result<PathBuf, Refusal> {
    parts(root)?;
    let temp = temp_dir()?;
    let root = long_name(root)?;
    check_lexical(&root, &temp)?;
    no_reparse(&temp)?;
    no_reparse(&root)?;
    std::fs::create_dir_all(&root).map_err(|e| Refusal::Io(format!("create {}: {e}", root.display())))?;
    no_reparse(&root)?;
    Ok(root)
}

/// The record file: its folder passes `prepare_root` and the file, when it exists, is a plain file.
pub fn prepare_record(json: &Path) -> Result<PathBuf, Refusal> {
    let parent = json.parent().ok_or(Refusal::NotAbsolute)?;
    let name = json.file_name().ok_or(Refusal::NotAbsolute)?;
    let dir = prepare_root(parent)?;
    let out = dir.join(name);
    no_reparse(&out)?;
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::windows::process::CommandExt;

    const T: &str = r"C:\Users\someone\AppData\Local\Temp";

    fn lexical(root: &str) -> Result<(), Refusal> {
        check_lexical(Path::new(root), Path::new(T))
    }

    #[test]
    fn mod_files_folders_under_temp_are_accepted() {
        for ok in [
            r"C:\Users\someone\AppData\Local\Temp\mod-files-live-20261124",
            r"C:\Users\someone\AppData\Local\Temp\mod-files-live-1\a",
            r"C:\Users\someone\AppData\Local\Temp\mod-files-live-1\a\b",
            r"c:\users\SOMEONE\appdata\local\temp\MOD-FILES-x",
            r"\\?\C:\Users\someone\AppData\Local\Temp\mod-files-x\e",
            r"C:/Users/someone/AppData/Local/Temp/mod-files-x",
        ] {
            assert_eq!(lexical(ok), Ok(()), "{ok}");
        }
    }

    #[test]
    fn everything_else_is_refused() {
        let cases: [(&str, Refusal); 12] = [
            (T, Refusal::NotUnderTemp),
            (r"C:\Users\someone\AppData\Local\Temp\other", Refusal::NoPrefix),
            (r"C:\Users\someone\AppData\Local\Temp\other\mod-files-x", Refusal::NoPrefix),
            (r"C:\Users\someone\AppData\Local\Temp\mod-files-", Refusal::NoPrefix),
            (r"C:\Users\someone\AppData\Local\Temp\mod-filesx", Refusal::NoPrefix),
            (r"C:\Users\someone\AppData\Local\Tempest\mod-files-x", Refusal::NotUnderTemp),
            (r"D:\Users\someone\AppData\Local\Temp\mod-files-x", Refusal::NotUnderTemp),
            (r"mod-files-x", Refusal::NotAbsolute),
            (r"C:\Users\someone\AppData\Local\Temp\mod-files-x\..\..\x", Refusal::BadComponent("..".into())),
            (r"\\?\C:\Users\someone\AppData\Local\Temp\mod-files-x\.", Refusal::BadComponent(".".into())),
            (r"\\server\share\Temp\mod-files-x", Refusal::NotUnderTemp),
            (
                r"C:\Users\someone\AppData\Local\Temp\mod-files-x:stream",
                Refusal::BadComponent("mod-files-x:stream".into()),
            ),
        ];
        for (path, want) in cases {
            assert_eq!(lexical(path), Err(want), "{path}");
        }
    }

    #[test]
    fn inside_compares_whole_components() {
        let root = Path::new(r"C:\T\mod-files-x");
        assert!(inside(root, Path::new(r"C:\T\mod-files-x")));
        assert!(inside(root, Path::new(r"\\?\C:\T\MOD-FILES-X\a\b.txt")));
        assert!(!inside(root, Path::new(r"C:\T\mod-files-xy\a")));
        assert!(!inside(root, Path::new(r"C:\T")));
        assert!(!inside(root, Path::new(r"C:\T\mod-files-x\..\other")));
        assert!(!inside(root, Path::new(r"D:\T\mod-files-x\a")));
    }

    /// A folder of the test's own, removed at the end (Rust's remove_dir_all never follows a junction).
    struct Scratch(PathBuf);
    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir(self.0.join("link"));
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn prepare_root_creates_the_folder_and_refuses_a_junction_on_the_path() {
        let base = std::env::temp_dir().join(format!("mod-files-spk-test-{}", std::process::id()));
        let scratch = Scratch(prepare_root(&base).expect("a mod-files folder under the temporary folder"));
        assert!(scratch.0.is_dir());
        let target = scratch.0.join("target");
        std::fs::create_dir(&target).unwrap();
        // CREATE_NO_WINDOW: the test's child never opens a console window.
        let st = std::process::Command::new("cmd")
            .creation_flags(0x0800_0000)
            .arg("/c")
            .arg("mklink")
            .arg("/J")
            .arg(scratch.0.join("link"))
            .arg(&target)
            .output()
            .expect("run mklink");
        assert!(st.status.success(), "mklink /J failed: {}", String::from_utf8_lossy(&st.stdout));
        let through = scratch.0.join("link").join("sub");
        assert!(matches!(prepare_root(&through), Err(Refusal::ReparsePoint(_))));
        assert!(!target.join("sub").exists(), "nothing was created through the junction");
        let record = scratch.0.join("link").join("r.json");
        assert!(matches!(prepare_record(&record), Err(Refusal::ReparsePoint(_))));
        assert!(matches!(prepare_root(&std::env::temp_dir().join("other-spk")), Err(Refusal::NoPrefix)));
    }
}
