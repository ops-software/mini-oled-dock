//! The identity of a program: the grouping key of an exe path, the install tree of a helper, the
//! AppUserModelID of a launch target. Pure rules over strings; `updated_exe_path` only reads the parent
//! directory of a path.

use std::path::Path;

/// Lowercase directory segments of an exe path (`c:`, `program files (x86)`, `steam`...), without the file name.
#[inline]
fn dir_segments(exe: &str) -> Vec<String> {
    let lower = exe.replace('/', "\\").to_lowercase();
    let mut segs: Vec<String> = lower.split('\\').filter(|s| !s.is_empty()).map(str::to_string).collect();
    segs.pop();
    segs
}

/// A directory too generic to be an install root (drive root, Windows, Program Files, user profile...).
#[inline]
fn is_generic_dir(segs: &[String]) -> bool {
    if segs.len() < 2 {
        return true;
    }
    match segs[1].as_str() {
        "windows" => true,
        "program files" | "program files (x86)" | "programdata" => segs.len() == 2,
        // `c:\users`, `c:\users\<name>`, `...\appdata`, `...\appdata\<local|roaming>`, `...\appdata\local\programs`.
        "users" => {
            let seg = |i: usize| segs.get(i).map(String::as_str);
            segs.len() <= 3
                || (seg(3) == Some("appdata")
                    && (segs.len() <= 5 || (segs.len() == 6 && seg(4) == Some("local") && seg(5) == Some("programs"))))
        }
        _ => false,
    }
}

/// Directory names of game libraries kept inside a launcher's install tree (Steam `steamapps\common`,
/// GOG Galaxy and Ubisoft Connect `games`...): what lives below them is a separate app, not a helper.
const LIBRARY_DIRS: [&str; 4] = ["steamapps", "steamlibrary", "games", "common"];

/// Normalized (lowercase, backslashes) form of an exe path, for comparisons.
#[inline]
pub fn norm_path(exe: &str) -> String {
    exe.replace('/', "\\").to_lowercase()
}

/// True when `parent_exe` is the root of an install tree containing `child_exe` as a helper: either the
/// same exe (multi-process apps), or an exe in a strict subdirectory of the parent's one, which must not
/// be a generic location and must not pass through a game library. A different exe in the same directory
/// is a sibling app (Word started by Outlook), not a helper.
#[inline]
pub fn same_install_tree(parent_exe: &str, child_exe: &str) -> bool {
    let p = dir_segments(parent_exe);
    let c = dir_segments(child_exe);
    if is_generic_dir(&p) || c.len() < p.len() || c[..p.len()] != p[..] {
        return false;
    }
    if c.len() == p.len() {
        return norm_path(parent_exe) == norm_path(child_exe);
    }
    !c[p.len()..].iter().any(|s| LIBRARY_DIRS.contains(&s.as_str()))
}

#[inline]
pub fn is_application_frame_host(exe_path: &str) -> bool {
    exe_path.rsplit(['\\', '/']).next().is_some_and(|f| f.eq_ignore_ascii_case("applicationframehost.exe"))
}

/// Key from exe path: lowercase, `\` separators. (`C:/Foo/Bar.EXE` -> `c:\foo\bar.exe`).
/// Squirrel-style versioned folders (`app-1.0.9265`) become `app-*`, so the key
/// (and therefore a pin) survives app updates (Discord, Slack, etc.).
#[inline]
pub fn key_from_exe_path(path: &str) -> String {
    let lower = path.replace('/', "\\").to_lowercase();
    lower
        .split('\\')
        .map(|seg| match seg.strip_prefix("app-") {
            Some(v) if !v.is_empty() && v.chars().all(|c| c.is_ascii_digit() || c == '.') => "app-*",
            _ => seg,
        })
        .collect::<Vec<_>>()
        .join("\\")
}

/// True for a Squirrel-style versioned folder name (`app-1.0.9265`).
#[inline]
pub fn is_version_dir(seg: &str) -> bool {
    seg.get(..4).is_some_and(|p| p.eq_ignore_ascii_case("app-"))
        && seg.len() > 4
        && seg[4..].chars().all(|c| c.is_ascii_digit() || c == '.')
}

/// True when `a` and `b` are the same exe in two versioned folders of one app
/// (`...\app-1.0.9265\Discord.exe` and `...\app-1.0.9267\Discord.exe`).
#[inline]
pub fn same_exe_other_version(a: &str, b: &str) -> bool {
    !a.is_empty() && !b.is_empty() && norm_path(a) != norm_path(b) && key_from_exe_path(a) == key_from_exe_path(b)
}

/// Where the exe `path` lives now, when it is gone because the app updated itself into a new
/// versioned folder (Discord, Slack...: `app-1.0.9265` is deleted, `app-1.0.9267` appears): the same
/// file inside the highest `app-<version>` folder next to the old one. `None` when `path` still
/// exists, has no versioned folder, or no other version holds the file.
#[inline]
pub fn updated_exe_path(path: &str) -> Option<String> {
    use std::path::Path;
    let p = Path::new(path);
    if !p.is_absolute() || p.exists() {
        return None;
    }
    let segs: Vec<&str> = path.split(['\\', '/']).collect();
    let i = segs.iter().rposition(|s| is_version_dir(s))?;
    let (base, rest) = (segs[..i].join("\\"), segs[i + 1..].join("\\"));
    if rest.is_empty() {
        return None;
    }
    let version = |name: &str| name[4..].split('.').map(|n| n.parse::<u64>().unwrap_or(0)).collect::<Vec<_>>();
    std::fs::read_dir(format!("{base}\\"))
        .ok()?
        .flatten()
        .filter_map(|e| e.file_name().into_string().ok())
        .filter(|name| is_version_dir(name))
        .map(|name| (version(&name), format!("{base}\\{name}\\{rest}")))
        .filter(|(_, cand)| Path::new(cand).is_file())
        .max_by(|a, b| a.0.cmp(&b.0))
        .map(|(_, cand)| cand)
}

/// Extracts the AUMID from a launch target `shell:AppsFolder\<AUMID>`; `None` if it does not have that form.
#[inline]
pub fn aumid_from_launch(launch: &str) -> Option<String> {
    const PREFIX: &str = "shell:appsfolder\\";
    if launch.len() > PREFIX.len()
        && launch.is_char_boundary(PREFIX.len())
        && launch[..PREFIX.len()].eq_ignore_ascii_case(PREFIX)
    {
        Some(launch[PREFIX.len()..].to_string())
    } else {
        None
    }
}

#[inline]
pub fn exe_file_name(exe_path: &str) -> Option<String> {
    Path::new(exe_path).file_name().map(|s| s.to_string_lossy().into_owned()).filter(|s| !s.is_empty())
}

/// True if `exe_path` points to an executable usable for name/icon (not empty and not the
/// UWP window container, which would give the wrong identity).
#[inline]
pub fn is_usable_exe(exe_path: &str) -> bool {
    match exe_file_name(exe_path) {
        Some(n) => !n.eq_ignore_ascii_case("applicationframehost.exe"),
        None => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn key_normalization() {
        assert_eq!(key_from_exe_path("C:/Foo/Bar.EXE"), "c:\\foo\\bar.exe");
        assert_eq!(
            key_from_exe_path("C:\\Users\\x\\AppData\\Local\\Discord\\app-1.0.9265\\Discord.exe"),
            "c:\\users\\x\\appdata\\local\\discord\\app-*\\discord.exe"
        );
        assert_eq!(key_from_exe_path("C:\\app-store\\a.exe"), "c:\\app-store\\a.exe");
    }

    #[test]
    fn versioned_folders() {
        assert!(is_version_dir("app-1.0.9265") && is_version_dir("APP-2"));
        assert!(!is_version_dir("app-") && !is_version_dir("app-store"));
        let (a, b) = ("C:\\x\\Discord\\app-1.0.9265\\Discord.exe", "c:/x/discord/app-1.0.9267/discord.exe");
        assert!(same_exe_other_version(a, b));
        assert!(!same_exe_other_version(a, a) && !same_exe_other_version(a, "C:\\x\\Discord\\Update.exe"));
        assert!(!same_exe_other_version(a, "") && !same_exe_other_version("shell:AppsFolder\\A!B", a));
    }

    #[test]
    fn updated_exe_path_follows_the_newest_version() {
        let root = std::env::temp_dir().join(format!("dock-test-versions-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        for v in ["app-1.0.9", "app-1.0.10", "app-1.0.11"] {
            std::fs::create_dir_all(root.join(v)).unwrap();
        }
        // 1.0.11 does not hold the file (an update still being unpacked): 1.0.10 wins, not "1.0.9".
        for v in ["app-1.0.9", "app-1.0.10"] {
            std::fs::write(root.join(v).join("App.exe"), b"x").unwrap();
        }
        let r = root.to_string_lossy().into_owned();
        assert_eq!(updated_exe_path(&format!("{r}\\app-1.0.3\\App.exe")), Some(format!("{r}\\app-1.0.10\\App.exe")));
        assert_eq!(updated_exe_path(&format!("{r}\\app-1.0.9\\App.exe")), None, "still there: untouched");
        assert_eq!(updated_exe_path(&format!("{r}\\app-1.0.3\\Other.exe")), None);
        assert_eq!(updated_exe_path(&format!("{r}\\gone\\App.exe")), None, "no versioned folder");
        assert_eq!(updated_exe_path("app-1.0.3\\App.exe"), None, "relative");
        assert_eq!(updated_exe_path("shell:AppsFolder\\A!B"), None);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn aumid_prefix() {
        assert_eq!(aumid_from_launch("shell:AppsFolder\\A.B_x!App").as_deref(), Some("A.B_x!App"));
        assert_eq!(aumid_from_launch("C:\\x.exe"), None);
        assert_eq!(aumid_from_launch("shell:AppsFolder\\"), None);
    }

    #[test]
    fn install_tree() {
        let steam = "C:\\Program Files (x86)\\Steam\\steam.exe";
        let web = "C:\\Program Files (x86)\\Steam\\bin\\cef\\cef.win64\\steamwebhelper.exe";
        assert!(same_install_tree(steam, web));
        assert!(same_install_tree(web, web));
        assert!(!same_install_tree("C:\\Windows\\explorer.exe", web));
        assert!(!same_install_tree("C:\\Windows\\System32\\cmd.exe", "C:\\Windows\\System32\\x\\a.exe"));
        assert!(!same_install_tree("C:\\launcher.exe", "C:\\Games\\a\\a.exe"));
        assert!(!same_install_tree("C:\\Program Files\\x.exe", "C:\\Program Files\\Foo\\y.exe"));
        assert!(!same_install_tree("C:\\Users\\me\\t.exe", "C:\\Users\\me\\Foo\\y.exe"));
        assert!(!same_install_tree("D:\\Other\\steam.exe", web));
    }

    #[test]
    fn install_tree_rejects_games_and_siblings() {
        let steam = "C:\\Program Files (x86)\\Steam\\steam.exe";
        let portal = "C:\\Program Files (x86)\\Steam\\steamapps\\common\\Portal 2\\portal2.exe";
        assert!(!same_install_tree(steam, portal));
        let gog = "C:\\Program Files (x86)\\GOG Galaxy\\GalaxyClient.exe";
        assert!(!same_install_tree(gog, "C:\\Program Files (x86)\\GOG Galaxy\\Games\\Witcher\\bin\\w.exe"));
        let upc = "C:\\Program Files (x86)\\Ubisoft\\Ubisoft Game Launcher\\upc.exe";
        assert!(!same_install_tree(upc, "C:\\Program Files (x86)\\Ubisoft\\Ubisoft Game Launcher\\games\\X\\x.exe"));
        let office = "C:\\Program Files\\Microsoft Office\\root\\Office16\\";
        assert!(!same_install_tree(&format!("{office}OUTLOOK.EXE"), &format!("{office}WINWORD.EXE")));
        assert!(same_install_tree(&format!("{office}OUTLOOK.EXE"), &format!("{office}outlook.exe")));
    }

    #[test]
    fn local_programs_is_generic() {
        let progs = "C:\\Users\\me\\AppData\\Local\\Programs";
        assert!(!same_install_tree(&format!("{progs}\\stub.exe"), &format!("{progs}\\Foo\\foo.exe")));
        assert!(same_install_tree(&format!("{progs}\\Foo\\foo.exe"), &format!("{progs}\\Foo\\bin\\h.exe")));
        assert!(!same_install_tree(
            "C:\\Users\\me\\AppData\\Roaming\\s.exe",
            "C:\\Users\\me\\AppData\\Roaming\\A\\a.exe"
        ));
    }

    #[test]
    fn afh_detection() {
        assert!(is_application_frame_host("C:\\Windows\\System32\\ApplicationFrameHost.exe"));
        assert!(!is_application_frame_host("C:\\x\\notepad.exe"));
    }
}
