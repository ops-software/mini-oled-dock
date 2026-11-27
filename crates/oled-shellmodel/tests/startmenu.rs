//! The copied tests of the Start Menu shortcut parser and index (the dock's `jumplist.rs` and `shell_thread.rs` tests, with
//! their vectors unchanged), and the index over a temp folder. Shortcut files are written in a temp folder only: no
//! window, no process, no Start menu of the machine.

use std::path::{Path, PathBuf};

use oled_shellmodel::names::{natural_name, NameQuery, NameStep};
use oled_shellmodel::startmenu::{
    expand_env, index_links, is_env_file_path, is_local_file_path, localized_lnk_name, localized_name_ref, parse_lnk,
    read_lnk, strip_paren_suffix, IndexStats, IndexedSources, IniSlot, LnkFacts, LnkMiss, StartLnk, StartMenuIndex,
    MAX_DESKTOP_INI, MAX_LNK_FILE,
};
use windows::core::{Interface, HSTRING};
use windows::Win32::System::Com::{
    CoCreateInstance, CoInitializeEx, CoUninitialize, IPersistFile, CLSCTX_INPROC_SERVER, COINIT_APARTMENTTHREADED,
    STGM_READ,
};
use windows::Win32::UI::Shell::{IShellLinkW, ShellLink};

/// CLSID_ShellLink {00021401-0000-0000-C000-000000000046}, as stored in a shell link's header.
const CLSID_LINK: [u8; 16] = [0x01, 0x14, 0x02, 0, 0, 0, 0, 0, 0xC0, 0, 0, 0, 0, 0, 0, 0x46];
/// EnvironmentVariableDataBlock ([MS-SHLLINK] 2.5.4): its signature and its only valid size.
const ENV_BLOCK: u32 = 0xA000_0001;
const ENV_BLOCK_SIZE: usize = 0x314;
/// CLSID of "This PC" {20D04FE0-3AEA-1069-A2D8-08002B30309D}, as stored in the root item of an id list.
const CLSID_MY_COMPUTER: [u8; 16] =
    [0xE0, 0x4F, 0xD0, 0x20, 0xEA, 0x3A, 0x69, 0x10, 0xA2, 0xD8, 0x08, 0x00, 0x2B, 0x30, 0x30, 0x9D];

fn wide(s: &str) -> Vec<u8> {
    s.encode_utf16().flat_map(u16::to_le_bytes).collect()
}

mod parser {
    use super::*;

    /// FMTID of `PKEY_Title` {F29F85E0-4FF9-1068-AB91-08002B27B3D9} (property 2).
    const FMTID_TITLE: [u8; 16] =
        [0xE0, 0x85, 0x9F, 0xF2, 0xF9, 0x4F, 0x68, 0x10, 0xAB, 0x91, 0x08, 0x00, 0x2B, 0x27, 0xB3, 0xD9];
    /// FMTID of `PKEY_AppUserModel_IsDestListSeparator` {9F4C2855-9F79-4B39-A8D0-E1D42DE1D5F3} (property 6).
    const FMTID_APP: [u8; 16] =
        [0x55, 0x28, 0x4C, 0x9F, 0x79, 0x9F, 0x39, 0x4B, 0xA8, 0xD0, 0xE1, 0xD4, 0x2D, 0xE1, 0xD5, 0xF3];
    const STEAM: &str = r"C:\Program Files (x86)\Steam\steam.exe";

    /// A serialized property store with the given title and/or the separator flag.
    fn props(title: Option<&str>, separator: bool) -> Vec<u8> {
        let storage = |fmtid: &[u8; 16], id: u32, vt: u16, data: &[u8]| {
            let mut value = Vec::new();
            value.extend(((13 + data.len()) as u32).to_le_bytes());
            value.extend(id.to_le_bytes());
            value.push(0);
            value.extend(vt.to_le_bytes());
            value.extend([0, 0]);
            value.extend(data);
            let mut s = Vec::new();
            s.extend(((24 + value.len() + 4) as u32).to_le_bytes());
            s.extend(b"1SPS");
            s.extend(fmtid);
            s.extend(value);
            s.extend(0u32.to_le_bytes());
            s
        };
        let mut out = Vec::new();
        if let Some(t) = title {
            let mut data = ((t.encode_utf16().count() + 1) as u32).to_le_bytes().to_vec();
            data.extend(wide(t));
            data.extend([0, 0]);
            out.extend(storage(&FMTID_TITLE, 2, 0x1F, &data));
        }
        if separator {
            out.extend(storage(&FMTID_APP, 6, 0x0B, &[0xFF, 0xFF, 0, 0]));
        }
        out.extend(0u32.to_le_bytes());
        out
    }

    /// A serialized shell link as Windows stores a jump list item.
    fn link(
        target: Option<&str>,
        name: Option<&str>,
        args: Option<&str>,
        title: Option<&str>,
        separator: bool,
    ) -> Vec<u8> {
        let mut flags = 0x80u32;
        let mut body = Vec::new();
        if let Some(t) = target {
            flags |= 0x03;
            // An id list (opaque here), then LinkInfo with an ANSI local base path.
            body.extend(6u16.to_le_bytes());
            body.extend([4, 0, 0x1F, 0x50, 0, 0]);
            let mut info = Vec::new();
            info.extend(((0x1C + t.len() + 2) as u32).to_le_bytes());
            info.extend(0x1Cu32.to_le_bytes());
            info.extend(1u32.to_le_bytes());
            info.extend(0u32.to_le_bytes());
            info.extend(0x1Cu32.to_le_bytes());
            info.extend(0u32.to_le_bytes());
            info.extend(((0x1C + t.len() + 1) as u32).to_le_bytes());
            info.extend(t.as_bytes());
            info.extend([0, 0]);
            body.extend(info);
        }
        for (bit, text) in [(0x04u32, name), (0x20, args)] {
            if let Some(t) = text {
                flags |= bit;
                body.extend((t.encode_utf16().count() as u16).to_le_bytes());
                body.extend(wide(t));
            }
        }
        let p = props(title, separator);
        body.extend(((8 + p.len()) as u32).to_le_bytes());
        body.extend(0xA000_0009u32.to_le_bytes());
        body.extend(p);
        body.extend(0u32.to_le_bytes());
        let mut out = vec![0u8; 0x4C];
        out[..4].copy_from_slice(&0x4Cu32.to_le_bytes());
        out[4..20].copy_from_slice(&CLSID_LINK);
        out[20..24].copy_from_slice(&flags.to_le_bytes());
        out.extend(body);
        out
    }

    /// A `.lnk` file: header, an id list that names no file, LinkInfo (header 0x24) with `info` as its
    /// ANSI and Unicode local path, the arguments, and an environment block with `env` (HasExpString).
    /// `flags` are added to the header's.
    pub(crate) fn lnk(info: Option<&str>, env: Option<&str>, args: Option<&str>, flags: u32) -> Vec<u8> {
        lnk_with_ids(&[4, 0, 0x1F, 0x50, 0, 0], info, env, args, flags)
    }

    /// `lnk` with the id list `ids` (its items and the terminal 0, `id_list`).
    fn lnk_with_ids(ids: &[u8], info: Option<&str>, env: Option<&str>, args: Option<&str>, flags: u32) -> Vec<u8> {
        let mut flags = 0x81 | flags;
        let mut body = Vec::new();
        body.extend((ids.len() as u16).to_le_bytes());
        body.extend(ids);
        if let Some(t) = info {
            flags |= 0x02;
            let narrow: Vec<u8> = t.chars().map(|c| if c.is_ascii() { c as u8 } else { b'?' }).collect();
            let (ansi_at, wide_at) = (0x24usize, 0x24 + narrow.len() + 2);
            let mut data = narrow;
            data.extend([0, 0]);
            data.extend(wide(t));
            data.extend([0, 0, 0, 0]);
            let mut info = Vec::new();
            info.extend(((0x24 + data.len()) as u32).to_le_bytes());
            info.extend(0x24u32.to_le_bytes());
            info.extend(1u32.to_le_bytes());
            info.extend(0u32.to_le_bytes());
            info.extend((ansi_at as u32).to_le_bytes());
            info.extend(0u32.to_le_bytes());
            info.extend(((wide_at - 1) as u32).to_le_bytes());
            info.extend((wide_at as u32).to_le_bytes());
            info.extend(((wide_at + 2 * t.encode_utf16().count()) as u32).to_le_bytes());
            info.extend(data);
            body.extend(info);
        }
        if let Some(a) = args {
            flags |= 0x20;
            body.extend((a.encode_utf16().count() as u16).to_le_bytes());
            body.extend(wide(a));
        }
        if let Some(e) = env {
            flags |= 0x200;
            body.extend((ENV_BLOCK_SIZE as u32).to_le_bytes());
            body.extend(ENV_BLOCK.to_le_bytes());
            let (mut narrow, mut w) = (vec![0u8; 260], vec![0u8; 520]);
            narrow[..e.len()].copy_from_slice(e.as_bytes());
            w[..2 * e.encode_utf16().count()].copy_from_slice(&wide(e));
            body.extend(narrow);
            body.extend(w);
        }
        body.extend(0u32.to_le_bytes());
        let mut out = vec![0u8; 0x4C];
        out[..4].copy_from_slice(&0x4Cu32.to_le_bytes());
        out[4..20].copy_from_slice(&CLSID_LINK);
        out[20..24].copy_from_slice(&flags.to_le_bytes());
        out.extend(body);
        out
    }

    /// A `.lnk` file with neither an id list nor a LinkInfo: the header and the Unicode RELATIVE_PATH
    /// and WORKING_DIR strings. `flags` are added to the header's.
    fn lnk_relative(relative: &str, dir: &str, flags: u32) -> Vec<u8> {
        let mut out = vec![0u8; 0x4C];
        out[..4].copy_from_slice(&0x4Cu32.to_le_bytes());
        out[4..20].copy_from_slice(&CLSID_LINK);
        out[20..24].copy_from_slice(&(0x98 | flags).to_le_bytes());
        for s in [relative, dir] {
            out.extend((s.encode_utf16().count() as u16).to_le_bytes());
            out.extend(wide(s));
        }
        out.extend(0u32.to_le_bytes());
        out
    }

    /// Writes `lnk(Some(target), ..)` as `<temp>\dock_lnk_<tag>_<pid>\Programs\Sub\Shortcut name.lnk`;
    /// returns the folder to remove, the `Programs` folder and the file.
    pub(crate) fn lnk_file(tag: &str, target: &str) -> (PathBuf, PathBuf, PathBuf) {
        let base = std::env::temp_dir().join(format!("dock_lnk_{tag}_{}", std::process::id()));
        let root = base.join("Programs");
        let path = root.join("Sub").join("Shortcut name.lnk");
        std::fs::create_dir_all(root.join("Sub")).expect("test folder");
        std::fs::write(&path, lnk(Some(target), None, None, 0)).expect("test link");
        (base, root, path)
    }

    fn facts(target: &str, has_args: bool) -> Result<LnkFacts, LnkMiss> {
        Ok(LnkFacts { target: target.into(), has_args })
    }

    /// The root shell item of an id list: "This PC", or another shell folder of `clsid`.
    fn root_item(clsid: &[u8; 16]) -> Vec<u8> {
        let mut item = 20u16.to_le_bytes().to_vec();
        item.extend([0x1F, 0x50]);
        item.extend(clsid);
        item
    }

    /// The volume shell item of a drive (`C:\`), as Windows writes it (type 0x2F, 25 bytes).
    fn drive_item(drive: &str) -> Vec<u8> {
        let mut item = 25u16.to_le_bytes().to_vec();
        item.push(0x2F);
        item.extend(drive.as_bytes());
        item.resize(25, 0);
        item
    }

    /// A file entry shell item (`kind` 0x31 folder, 0x32 file): a short name (the letters and digits of
    /// `name`, at most 8, padded to an even length), then the long `name` in a 0xBEEF0004 block of
    /// `version` (3, 7, 8 or 9: where the name sits moves).
    fn file_item(kind: u8, name: &str, version: u16) -> Vec<u8> {
        let mut item = vec![0, 0, kind, 0];
        item.extend([0u8; 10]);
        item.extend(name.bytes().filter(u8::is_ascii_alphanumeric).map(|c| c.to_ascii_uppercase()).take(8));
        item.push(0);
        if item.len() % 2 == 1 {
            item.push(0);
        }
        let at = match version {
            3 => 20,
            7 => 38,
            8 => 42,
            _ => 46,
        };
        let mut ext = vec![0u8; at];
        ext[2..4].copy_from_slice(&version.to_le_bytes());
        ext[4..8].copy_from_slice(&0xBEEF_0004u32.to_le_bytes());
        ext[16..18].copy_from_slice(&(at as u16).to_le_bytes());
        ext.extend(wide(name));
        ext.extend([0, 0]);
        ext.extend((item.len() as u16).to_le_bytes());
        let n = ext.len() as u16;
        ext[0..2].copy_from_slice(&n.to_le_bytes());
        item.extend(ext);
        let n = item.len() as u16;
        item[0..2].copy_from_slice(&n.to_le_bytes());
        item
    }

    /// An id list of `items` with its terminal 0.
    fn id_list(items: &[Vec<u8>]) -> Vec<u8> {
        let mut out: Vec<u8> = items.concat();
        out.extend([0, 0]);
        out
    }

    /// The id list of `C:\Vendor Dir\app.exe` as Windows writes it, its long names in blocks of `version`.
    fn drive_path(version: u16) -> Vec<u8> {
        id_list(&[
            root_item(&CLSID_MY_COMPUTER),
            drive_item("C:\\"),
            file_item(0x31, "Vendor Dir", version),
            file_item(0x32, "app.exe", version),
        ])
    }

    const ID_APP: &str = r"C:\Vendor Dir\app.exe";
    /// The CLSID of a shell folder that is not a file (Control Panel's, as Windows stores it).
    const CONTROL_PANEL: [u8; 16] =
        [0x20, 0x20, 0xEC, 0x21, 0xEA, 0x3A, 0x10, 0x69, 0xA2, 0xDD, 0x08, 0x00, 0x2B, 0x30, 0x30, 0x9D];

    const APP: &str = r"C:\Program Files\Vendor\app.exe";
    const ENV_APP: &str = r"%ProgramFiles%\Vendor\app.exe";

    #[test]
    fn lnk_file_target_and_args() {
        assert_eq!(parse_lnk(&lnk(Some(APP), None, None, 0)), facts(APP, false));
        assert_eq!(parse_lnk(&lnk(Some(APP), None, Some("--x"), 0)), facts(APP, true));
        assert_eq!(parse_lnk(&lnk(Some(APP), None, Some(""), 0)), facts(APP, false), "empty arguments are none");
        let unicode = "C:\\Programmi\\Citt\u{e0} \u{65e5}\\app.exe";
        assert_eq!(parse_lnk(&lnk(Some(unicode), None, None, 0)), facts(unicode, false), "the Unicode path wins");
        // A jump list link is a .lnk too (ANSI LinkInfo, a property store).
        let jump = link(Some(STEAM), None, Some("-n"), Some("Title"), false);
        assert_eq!(parse_lnk(&jump), facts(STEAM, true));
        assert_eq!(parse_lnk(&lnk(None, None, None, 0)), Err(LnkMiss::ShellItem), "an id list that names no file");
        let mut clsid = lnk(Some(APP), None, None, 0);
        clsid[4] ^= 1;
        assert_eq!(parse_lnk(&clsid), Err(LnkMiss::Damaged), "not a shell link");
        assert_eq!(parse_lnk(&[]), Err(LnkMiss::Damaged));
    }

    #[test]
    fn lnk_env_block_target() {
        assert_eq!(parse_lnk(&lnk(None, Some(ENV_APP), None, 0)), facts(ENV_APP, false), "left unexpanded");
        let sys = parse_lnk(&lnk(None, Some(r"%SystemRoot%\System32\notepad.exe"), None, 0)).expect("env target");
        let expanded = expand_env(&sys.target);
        assert!(is_local_file_path(&expanded), "{expanded}");
        assert!(expanded.to_lowercase().ends_with(r"\system32\notepad.exe"), "{expanded}");
        assert_eq!(parse_lnk(&lnk(Some(APP), Some(ENV_APP), None, 0)), facts(APP, false), "LinkInfo first");
        // TargetUnicode empty: TargetAnsi.
        let mut narrow = lnk(None, Some(ENV_APP), None, 0);
        let at = narrow.len() - 4 - 520;
        narrow[at..at + 520].fill(0);
        assert_eq!(parse_lnk(&narrow), facts(ENV_APP, false));
        // Without HasExpString the block is not read, as by the shell.
        let mut no_flag = lnk(None, Some(ENV_APP), None, 0);
        no_flag[21] &= !0x02;
        assert_eq!(parse_lnk(&no_flag), Err(LnkMiss::ShellItem));
        assert_eq!(parse_lnk(&lnk(None, Some("C:\\Tools\\t.exe"), None, 0)), facts("C:\\Tools\\t.exe", false));
    }

    #[test]
    fn lnk_rejects_non_local_targets() {
        let long = format!("C:\\{}", "a".repeat(1_097));
        let hostile = [
            r"\\server\share\a.exe",
            r"\\?\C:\a.exe",
            r"\\.\pipe\a",
            long.as_str(),
            r"C:\a.exe:stream",
            "C:\\a\u{7}b.exe",
            "C:\\a\nb.exe",
            "a.exe",
            r"C:a.exe",
            r"%SystemRoot%",
            r"%%\a.exe",
            r"%A B%\a.exe",
            r"%SystemRoot%\a|b.exe",
        ];
        for p in hostile {
            assert_eq!(parse_lnk(&lnk(Some(p), None, None, 0)), Err(LnkMiss::Refused), "LinkInfo {p}");
            if p.len() < 260 && p.is_ascii() {
                assert_eq!(parse_lnk(&lnk(None, Some(p), None, 0)), Err(LnkMiss::Refused), "environment block {p}");
            }
        }
        // A refused LinkInfo path leaves the environment block's.
        assert_eq!(parse_lnk(&lnk(Some(r"\\server\a.exe"), Some(ENV_APP), None, 0)), facts(ENV_APP, false));
    }

    #[test]
    fn lnk_relative_path_only_is_refused() {
        // No id list, no LinkInfo, no environment block: only a relative path and a working directory,
        // which this parser does not resolve.
        assert_eq!(parse_lnk(&lnk_relative(r"..\app.exe", r"C:\Vendor", 0)), Err(LnkMiss::Refused));
    }

    /// Four samples: LinkInfo with arguments, the environment block with ForceNoLinkInfo, a jump list
    /// link, an id list only.
    fn lnk_samples() -> [Vec<u8>; 4] {
        [
            lnk(Some(APP), None, Some("--flag"), 0),
            lnk(Some(r"C:\Old\old.exe"), Some(ENV_APP), None, 0x100),
            link(Some(STEAM), Some("Steam"), None, Some("Steam"), false),
            lnk_with_ids(&drive_path(9), None, None, None, 0),
        ]
    }

    #[test]
    fn lnk_truncations_never_panic() {
        for s in lnk_samples() {
            assert!(parse_lnk(&s).is_ok());
            for n in 0..s.len() {
                assert_eq!(parse_lnk(&s[..n]), Err(LnkMiss::Damaged), "cut at {n}");
            }
        }
    }

    #[test]
    fn lnk_mutations_never_panic() {
        let samples = lnk_samples();
        let mut x = 0x9E37_79B9_7F4A_7C15u64;
        let mut next = move || {
            x ^= x << 13;
            x ^= x >> 7;
            x ^= x << 17;
            x
        };
        let started = std::time::Instant::now();
        let mut found = 0usize;
        for _ in 0..100_000 {
            let mut b = samples[(next() % 4) as usize].clone();
            for _ in 0..1 + next() % 3 {
                let len = b.len().max(1) as u64;
                let at = (next() % len) as usize;
                match next() % 6 {
                    0 => {
                        if let Some(v) = b.get_mut(at) {
                            *v ^= 1 << (next() % 8);
                        }
                    }
                    k @ 1..=3 => {
                        let v = [0, u32::MAX, b.len() as u32][(k - 1) as usize];
                        if let Some(w) = b.get_mut(at..at + 4) {
                            w.copy_from_slice(&v.to_le_bytes());
                        }
                    }
                    4 => b.truncate(at),
                    _ => {
                        let end = (at + (next() % 64) as usize).min(b.len());
                        let piece = b[at.min(end)..end].to_vec();
                        let to = (next() % len) as usize;
                        b.splice(to.min(b.len())..to.min(b.len()), piece);
                    }
                }
            }
            if let Ok(f) = parse_lnk(&b) {
                found += 1;
                assert!(is_local_file_path(&f.target) || is_env_file_path(&f.target), "{:?}", f.target);
            }
        }
        assert!(found > 0, "some mutations keep a valid link");
        assert!(started.elapsed() < std::time::Duration::from_secs(10), "{:?}", started.elapsed());
    }

    #[test]
    fn lnk_force_no_link_info_flag() {
        let both = lnk(Some(r"C:\Old\old.exe"), Some(ENV_APP), None, 0x100);
        assert_eq!(parse_lnk(&both), facts(ENV_APP, false), "the LinkInfo is ignored");
        assert_eq!(parse_lnk(&lnk(Some(APP), None, None, 0x100)), Err(LnkMiss::ShellItem));
    }

    #[test]
    fn lnk_linkinfo_without_local_base_path() {
        // LinkInfo at 0x4C + 8 (after the id list): flags at +8, LocalBasePathOffset(Unicode) at +16 / +28.
        let info = 0x4C + 8;
        let mut network = lnk(Some(APP), None, None, 0);
        network[info + 8] = 0x02;
        assert_eq!(parse_lnk(&network), Err(LnkMiss::Network), "no VolumeIDAndLocalBasePath, a network part");
        let mut zero = lnk(Some(APP), None, None, 0);
        zero[info + 16..info + 20].fill(0);
        zero[info + 28..info + 32].fill(0);
        assert_eq!(parse_lnk(&zero), Err(LnkMiss::Refused), "offsets 0");
        let mut outside = lnk(Some(APP), Some(ENV_APP), None, 0);
        outside[info + 16..info + 20].copy_from_slice(&0xFFFFu32.to_le_bytes());
        outside[info + 28..info + 32].copy_from_slice(&0xFFFF_FFFFu32.to_le_bytes());
        assert_eq!(parse_lnk(&outside), facts(ENV_APP, false), "offsets outside the structure");
    }

    #[test]
    fn read_lnk_reads_a_capped_single_file() {
        let (base, _, path) = lnk_file("read", APP);
        let got = read_lnk(&path);
        let big = base.join("big.lnk");
        let mut bytes = lnk(Some(APP), None, None, 0);
        bytes.resize(MAX_LNK_FILE as usize + 1, 0);
        std::fs::write(&big, bytes).expect("big link");
        let refused = read_lnk(&big);
        let missing = read_lnk(&base.join("missing.lnk"));
        let _ = std::fs::remove_dir_all(&base);
        assert_eq!(got, facts(APP, false));
        assert_eq!(
            (refused, missing),
            (Err(LnkMiss::Damaged), Err(LnkMiss::Damaged)),
            "above MAX_LNK_FILE, or missing"
        );
    }

    #[test]
    fn lnk_id_list_target() {
        // An id list only (no LinkInfo), as some installers write it: This PC, the drive, the folders, the file.
        let only = |ids: &[u8]| parse_lnk(&lnk_with_ids(ids, None, None, None, 0));
        for version in [3, 7, 8, 9] {
            assert_eq!(only(&drive_path(version)), facts(ID_APP, false), "block version {version}");
        }
        assert_eq!(parse_lnk(&lnk_with_ids(&drive_path(9), None, None, Some("-x"), 0)), facts(ID_APP, true));
        // Another drive, a short name of even length (no padding), Unicode long names.
        let unicode = "Citt\u{e0} \u{65e5}.exe";
        let ids = id_list(&[
            root_item(&CLSID_MY_COMPUTER),
            drive_item("d:\\"),
            file_item(0x31, "Sub", 9),
            file_item(0x32, unicode, 9),
        ]);
        assert_eq!(only(&ids), facts(&format!("d:\\Sub\\{unicode}"), false));
        // A Unicode primary name (type flag 0x04) instead of the short one.
        let mut wide_item = file_item(0x36, "app.exe", 9);
        let short = 14 + "APPEXE".len() + 1 + 1;
        let mut primary = wide("APP.EXE");
        primary.extend([0, 0]);
        wide_item.splice(14..short, primary);
        let n = wide_item.len() as u16;
        wide_item[0..2].copy_from_slice(&n.to_le_bytes());
        let ids =
            id_list(&[root_item(&CLSID_MY_COMPUTER), drive_item("C:\\"), file_item(0x31, "Vendor Dir", 9), wide_item]);
        assert_eq!(only(&ids), facts(ID_APP, false));
        // The id list comes last: LinkInfo, then the environment block; it stands in for a LinkInfo
        // that ForceNoLinkInfo hides or whose path is refused.
        assert_eq!(parse_lnk(&lnk_with_ids(&drive_path(9), Some(APP), None, None, 0)), facts(APP, false));
        assert_eq!(parse_lnk(&lnk_with_ids(&drive_path(9), None, Some(ENV_APP), None, 0)), facts(ENV_APP, false));
        let hidden = lnk_with_ids(&drive_path(9), Some(r"C:\Old\old.exe"), None, None, 0x100);
        assert_eq!(parse_lnk(&hidden), facts(ID_APP, false));
        let refused = lnk_with_ids(&drive_path(9), Some(r"\\server\a.exe"), None, None, 0);
        assert_eq!(parse_lnk(&refused), facts(ID_APP, false));
    }

    #[test]
    fn lnk_id_list_refusals() {
        let only = |items: &[Vec<u8>]| parse_lnk(&lnk_with_ids(&id_list(items), None, None, None, 0));
        let root = || root_item(&CLSID_MY_COMPUTER);
        let c = || drive_item("C:\\");
        let dir = || file_item(0x31, "Vendor Dir", 9);
        let exe = || file_item(0x32, "app.exe", 9);
        // Shell objects that are no file: a shell folder, This PC alone, a drive alone, an empty list.
        assert_eq!(only(&[root_item(&CONTROL_PANEL), exe()]), Err(LnkMiss::ShellItem));
        assert_eq!(only(&[root()]), Err(LnkMiss::ShellItem));
        assert_eq!(only(&[root(), c()]), Err(LnkMiss::ShellItem));
        assert_eq!(only(&[]), Err(LnkMiss::ShellItem));
        // Under This PC something other than a drive letter: a shell folder item, odd volume names.
        let mut folder = c();
        folder[2] = 0x2E;
        folder[3..19].copy_from_slice(&CONTROL_PANEL);
        assert_eq!(only(&[root(), folder, dir(), exe()]), Err(LnkMiss::ShellItem));
        for d in ["C:", "CC:\\", "1:\\", "C:\\x", "C:/", ""] {
            assert_eq!(only(&[root(), drive_item(d), exe()]), Err(LnkMiss::ShellItem), "{d:?}");
        }
        // After the drive, an item that is no file entry.
        assert_eq!(only(&[root(), c(), root(), exe()]), Err(LnkMiss::ShellItem));
        assert_eq!(only(&[root(), c(), dir(), drive_item("C:\\")]), Err(LnkMiss::ShellItem));
        // Long names that are no single path component, or that make a path the other targets refuse.
        for name in ["..", ".", "a\\b", "a/b", "", "a:b", "a\u{7}b", "a|b"] {
            assert_eq!(only(&[root(), c(), file_item(0x31, name, 9), exe()]), Err(LnkMiss::Refused), "{name:?}");
        }
        let long = file_item(0x31, &"a".repeat(255), 9);
        let deep = [root(), c(), long.clone(), long.clone(), long.clone(), long, exe()];
        assert_eq!(only(&deep), Err(LnkMiss::Refused), "longer than 1,024 bytes");
        // No long name: an unknown block version, another block, no block, a name without its NUL.
        assert_eq!(only(&[root(), c(), file_item(0x31, "Vendor Dir", 10), exe()]), Err(LnkMiss::Refused));
        let mut other = dir();
        let sig = other.windows(4).position(|w| w == 0xBEEF_0004u32.to_le_bytes()).expect("signature");
        other[sig] = 0x05;
        assert_eq!(only(&[root(), c(), other, exe()]), Err(LnkMiss::Refused));
        let mut bare = exe();
        bare.truncate(22);
        bare[0..2].copy_from_slice(&22u16.to_le_bytes());
        assert_eq!(only(&[root(), c(), dir(), bare]), Err(LnkMiss::Refused));
        let mut unended = exe();
        let end = unended.len() - 4;
        unended[end..end + 2].copy_from_slice(&wide("x"));
        assert_eq!(only(&[root(), c(), dir(), unended]), Err(LnkMiss::Refused));
        // Damaged lists: an item running past the list, a size below 3, no terminal 0.
        let mut past = id_list(&[root(), c(), dir(), exe()]);
        let at = past.len() - 2 - exe().len();
        past[at..at + 2].copy_from_slice(&0x7FFFu16.to_le_bytes());
        assert_eq!(parse_lnk(&lnk_with_ids(&past, None, None, None, 0)), Err(LnkMiss::Damaged));
        let tiny = [root(), c(), vec![2, 0]].concat();
        assert_eq!(
            parse_lnk(&lnk_with_ids(&[tiny.as_slice(), &[0, 0]].concat(), None, None, None, 0)),
            Err(LnkMiss::Damaged)
        );
        let open = [root(), c(), dir(), exe()].concat();
        assert_eq!(parse_lnk(&lnk_with_ids(&open, None, None, None, 0)), Err(LnkMiss::Damaged), "no terminal");
    }

    #[test]
    fn lnk_advertised_shortcuts_are_not_resolved() {
        // HasDarwinID: Windows Installer resolves the target; the paths in the file name its cache.
        let advertised = [
            lnk(Some(APP), None, None, 0x1000),
            lnk_with_ids(&drive_path(9), None, Some(ENV_APP), None, 0x1000),
            lnk_relative(r"..\app.exe", r"C:\Vendor", 0x1000),
        ];
        for link in advertised {
            assert_eq!(parse_lnk(&link), Err(LnkMiss::Advertised));
        }
    }

    /// A `desktop.ini` as Windows writes it in the Start menu folders: UTF-16 with a byte order mark.
    fn ini_utf16(text: &str) -> Vec<u8> {
        let mut b = vec![0xFF, 0xFE];
        b.extend(wide(text));
        b
    }

    const INI: &str = "\r\n[.ShellClassInfo]\r\nLocalizedResourceName=@%SystemRoot%\\system32\\shell32.dll,-21761\r\n\
        [LocalizedFileNames]\r\nTool A.lnk=@%SystemRoot%\\system32\\shell32.dll,-22041\r\n \
        Tool B.lnk = @%windir%\\System32\\Vendor-Res_2.dll,-7 \r\n[Other]\r\nTool C.lnk=@%SystemRoot%\\system32\\shell32.dll,-1\r\n";

    #[test]
    fn desktop_ini_localized_names() {
        let ini = ini_utf16(INI);
        let shell32 = Some(("shell32.dll".to_string(), 22041));
        assert_eq!(localized_name_ref(&ini, "Tool A.lnk"), shell32);
        assert_eq!(localized_name_ref(&ini, "TOOL a.LNK"), shell32, "file names compare case-insensitively");
        assert_eq!(localized_name_ref(INI.as_bytes(), "Tool A.lnk"), shell32, "ANSI");
        assert_eq!(localized_name_ref(&ini, "Tool B.lnk"), Some(("vendor-res_2.dll".to_string(), 7)));
        assert_eq!(localized_name_ref(&ini, "Tool C.lnk"), None, "only [LocalizedFileNames] names files");
        assert_eq!(localized_name_ref(&ini, "LocalizedResourceName"), None);
        assert_eq!(localized_name_ref(&ini, "Other.lnk"), None);
        assert_eq!(localized_name_ref(&[], "Tool A.lnk"), None);
    }

    #[test]
    fn desktop_ini_names_only_strings_of_system_dlls() {
        for value in [
            r"@C:\Users\Someone\name.dll,-1",
            r"@%APPDATA%\name.dll,-1",
            r"@%SystemRoot%\name.dll,-1",
            r"@%SystemRoot%\system32\..\..\Users\name.dll,-1",
            r"@%SystemRoot%\system32\sub\name.dll,-1",
            r"@%SystemRoot%\system32\name.exe,-1",
            r"@%SystemRoot%\system32\.dll,-1",
            r"@%SystemRoot%\system32\na me.dll,-1",
            r"@%SystemRoot%\system32\shell32.dll,1",
            r"@%SystemRoot%\system32\shell32.dll,-0",
            r"@%SystemRoot%\system32\shell32.dll,-65536",
            r"@%SystemRoot%\system32\shell32.dll,-+5",
            r"@%SystemRoot%\system32\shell32.dll,-5,-6",
            r"@%SystemRoot%\system32\shell32.dll",
            r"%SystemRoot%\system32\shell32.dll,-5",
            "A plain name",
            "",
        ] {
            let ini = format!("[LocalizedFileNames]\r\nTool.lnk={value}\r\n");
            assert_eq!(localized_name_ref(ini.as_bytes(), "Tool.lnk"), None, "{value}");
        }
    }

    #[test]
    fn desktop_ini_damage_never_panics() {
        let good = ini_utf16(INI);
        for n in 0..good.len() {
            let _ = localized_name_ref(&good[..n], "Tool A.lnk");
        }
        for i in 0..good.len() {
            for v in [0x00, 0x0A, 0x2C, 0x3D, 0x40, 0x5B, 0xFF] {
                let mut bad = good.clone();
                bad[i] = v;
                if let Some((module, id)) = localized_name_ref(&bad, "Tool A.lnk") {
                    assert!(module.ends_with(".dll") && !module.contains('\\') && id > 0, "{module} {id}");
                }
            }
        }
    }
}

/// %TEMP% with its 8.3 components expanded (C:\Users\RUNNER~1 on the CI runner): the shell reports a link's target by
/// its long path, so the targets the test asks about are long too. %TEMP% itself when it cannot be read.
fn temp_long() -> PathBuf {
    let base = std::env::temp_dir();
    let long = std::fs::canonicalize(&base).ok().and_then(|p| {
        let s = p.to_str()?.to_string();
        s.strip_prefix(r"\\?\").filter(|rest| !rest.starts_with("UNC\\")).map(PathBuf::from)
    });
    long.unwrap_or(base)
}

/// A scratch folder of this test, removed at the end of it.
struct Scratch(PathBuf);

impl Scratch {
    fn new(tag: &str) -> Scratch {
        let dir = temp_long().join(format!("shellmodel_sm_{tag}_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("test folder");
        Scratch(dir)
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// COM STA on the test thread for the length of a test.
struct Com(bool);

impl Com {
    fn new() -> Com {
        // SAFETY: COM STA for this test thread, paired with CoUninitialize in Drop.
        Com(unsafe { CoInitializeEx(None, COINIT_APARTMENTTHREADED) }.is_ok())
    }
}

impl Drop for Com {
    fn drop(&mut self) {
        if self.0 {
            // SAFETY: paired with the successful CoInitializeEx in new.
            unsafe { CoUninitialize() };
        }
    }
}

/// An (empty) file that stands for a program: it is never run.
fn fake_exe(dir: &Path, name: &str) -> String {
    let p = dir.join(name);
    std::fs::write(&p, b"MZ").expect("fake exe");
    p.to_string_lossy().into_owned()
}

/// Writes the shortcut `lnk` to `target` through IShellLinkW::SetPath and IPersistFile::Save (with `args` when given).
fn write_link(lnk: &Path, target: &str, args: Option<&str>) {
    // SAFETY: COM STA initialized by the caller; ShellLink CLSID with the requested interface.
    let link: IShellLinkW = unsafe { CoCreateInstance(&ShellLink, None, CLSCTX_INPROC_SERVER) }.expect("ShellLink");
    let file = link.cast::<IPersistFile>().expect("IPersistFile");
    let target = HSTRING::from(target);
    // SAFETY: the strings are NUL-terminated and live for the calls.
    unsafe {
        link.SetPath(&target).expect("SetPath");
        if let Some(a) = args {
            link.SetArguments(&HSTRING::from(a)).expect("SetArguments");
        }
        file.Save(&HSTRING::from(lnk.as_os_str()), true).expect("Save");
    }
    let _ = STGM_READ;
}

#[test]
fn index_over_a_temp_folder() {
    let _com = Com::new();
    let s = Scratch::new("index");
    let (all, user, apps) = (s.0.join("all"), s.0.join("user"), s.0.join("apps"));
    for d in [&all, &user, &apps] {
        std::fs::create_dir_all(d).expect("folder");
    }
    let (a, b, c, d, e) = (
        fake_exe(&apps, "alpha.exe"),
        fake_exe(&apps, "beta.exe"),
        fake_exe(&apps, "gamma.exe"),
        fake_exe(&apps, "delta.exe"),
        fake_exe(&apps, "epsilon.exe"),
    );
    // All-users, named by the shell's display name, "(x64)" stripped.
    write_link(&all.join("Alpha Studio (x64).lnk"), &a, None);
    // Per-user, named by its file.
    write_link(&user.join("Beta Notes.lnk"), &b, None);
    // Two links to one target with different names: dropped.
    write_link(&user.join("Gamma One.lnk"), &c, None);
    write_link(&all.join("Gamma Two.lnk"), &c, None);
    // Skipped: an uninstaller and a link with arguments.
    write_link(&user.join("Uninstall Delta.lnk"), &d, None);
    write_link(&user.join("Epsilon Args.lnk"), &e, Some("--flag"));
    let index = StartMenuIndex::build_from(&[(all.clone(), false), (user.clone(), true)]);
    assert_eq!(index.name(&a).as_deref(), Some("Alpha Studio"), "{:?}", index.stats());
    assert_eq!(index.name(&b.to_uppercase()).as_deref(), Some("Beta Notes"), "a per-user link, by its file; any case");
    assert_eq!(index.name(&c), None, "two names for one target");
    assert_eq!(index.name(&d), None, "an uninstaller");
    assert_eq!(index.name(&e), None, "a link with arguments");
    assert_eq!(index.len(), 2);
    assert!(!index.is_stale(std::time::Instant::now()));
    assert!(StartMenuIndex::default().is_stale(std::time::Instant::now()));
}

#[test]
fn names_with_a_real_start_menu() {
    let _com = Com::new();
    let s = Scratch::new("names");
    let (all, user, apps) = (s.0.join("all"), s.0.join("user"), s.0.join("apps"));
    for d in [&all, &user, &apps] {
        std::fs::create_dir_all(d).expect("folder");
    }
    let (a, b, c) = (fake_exe(&apps, "orbit.exe"), fake_exe(&apps, "quill.exe"), fake_exe(&apps, "plain.exe"));
    write_link(&all.join("Orbit Suite.lnk"), &a, None);
    write_link(&user.join("Quill Writer.lnk"), &b, None);
    write_link(&user.join("Plain Args.lnk"), &c, Some("--x"));
    let index = StartMenuIndex::build_from(&[(all, false), (user, true)]);
    let src = IndexedSources { index: &index, system_root: r"C:\Windows" };
    let name = |exe: &str| {
        let key = oled_shellmodel::identity::key_from_exe_path(exe);
        natural_name(&NameQuery { key: &key, exe_path: exe, aumid: None }, &src)
    };
    assert_eq!(name(&a), ("Orbit Suite".to_string(), NameStep::Shortcut));
    assert_eq!(name(&b), ("Quill Writer".to_string(), NameStep::Shortcut));
    let (_, step) = name(&c);
    assert_ne!(step, NameStep::Shortcut, "a link with arguments names nothing");
}

/// Every link of the all-users folder goes to the shell and every per-user one to the Rust parser.
#[test]
fn per_user_links_never_reach_the_shell_loader() {
    let l = |p: &str, per_user: bool| StartLnk { path: p.into(), per_user };
    let files = [
        l(r"C:\All\Tool.lnk", false),
        l(r"C:\Me\Mine.lnk", true),
        l(r"C:\All\Other (x64).lnk", false),
        l(r"C:\Me\Broken.lnk", true),
        l(r"C:\All\Uninstall Tool.lnk", false),
        l(r"C:\Me\Magnify.lnk", true),
        l(r"C:\Me\Args.lnk", true),
    ];
    let mut rust_calls = Vec::new();
    let mut rust = |s: &StartLnk| {
        rust_calls.push(s.path.clone());
        // A Windows program of the per-user folder, renamed there by its `desktop.ini`.
        if s.path.ends_with("Magnify.lnk") {
            return Ok(r"%SystemRoot%\System32\x.exe".to_string());
        }
        if s.path.ends_with("Broken.lnk") {
            return Err(LnkMiss::ShellItem);
        }
        if s.path.ends_with("Args.lnk") {
            return Err(LnkMiss::Arguments);
        }
        Ok(r"C:\Me\mine.exe".to_string())
    };
    let mut rust_names = Vec::new();
    let mut rust_display = |s: &StartLnk| {
        rust_names.push(s.path.clone());
        s.path.ends_with("Magnify.lnk").then(|| "Lente".to_string())
    };
    let mut com_target = |s: &StartLnk| {
        assert!(!s.per_user, "{:?} reached the shell link loader", s.path);
        Some(format!(r"C:\All\{}.exe", s.path.file_stem()?.to_string_lossy()))
    };
    let mut com_display = |s: &StartLnk| {
        assert!(!s.per_user, "{:?} reached the shell's display name", s.path);
        s.path.ends_with("Tool.lnk").then(|| "Strumento".to_string())
    };
    let (map, stats) = index_links(&files, &mut rust, &mut rust_display, &mut com_target, &mut com_display);
    let me = |p: &str| PathBuf::from(p);
    let per_user = [me(r"C:\Me\Mine.lnk"), me(r"C:\Me\Broken.lnk"), me(r"C:\Me\Magnify.lnk"), me(r"C:\Me\Args.lnk")];
    assert_eq!(rust_calls, per_user);
    assert_eq!(rust_names, [me(r"C:\Me\Mine.lnk"), me(r"C:\Me\Magnify.lnk")], "named only when indexed");
    assert_eq!(stats, IndexStats { rust_parsed: 2, rust_unresolved: [1, 1, 0, 0, 0, 0], com_common: 3 });
    let windows_exe = oled_shellmodel::identity::norm_path(&expand_env(r"%SystemRoot%\System32\x.exe"));
    assert_eq!(map.get(&windows_exe).map(String::as_str), Some("Lente"), "its desktop.ini name: {map:?}");
    assert_eq!(map.get(r"c:\me\mine.exe").map(String::as_str), Some("Mine"), "per-user: else the file name");
    assert_eq!(map.get(r"c:\all\tool.exe").map(String::as_str), Some("Strumento"), "the shell's name");
    assert_eq!(map.get(r"c:\all\other (x64).exe").map(String::as_str), Some("Other"));
    assert_eq!(map.len(), 4, "the uninstaller is left out: {map:?}");
}

/// The `desktop.ini` name of a per-user shortcut: a string of a system DLL, read once per folder.
#[test]
fn localized_names_come_from_system_dlls_only() {
    let s = Scratch::new("ini");
    let dir = s.0.clone();
    let text = "[LocalizedFileNames]\r\nA.lnk=@%SystemRoot%\\system32\\shell32.dll,-22022\r\n\
                B.lnk=@C:\\Somewhere\\shell32.dll,-22022\r\nC.lnk=@%SystemRoot%\\system32\\no-such-dll.dll,-1\r\n";
    let mut ini = vec![0xFF, 0xFE];
    ini.extend(text.encode_utf16().flat_map(u16::to_le_bytes));
    std::fs::write(dir.join("desktop.ini"), ini).expect("desktop.ini");
    let mut ini: IniSlot = None;
    let name = |f: &str, ini: &mut IniSlot| localized_lnk_name(&dir.join(f), ini);
    let a = name("A.lnk", &mut ini);
    let others = [name("B.lnk", &mut ini), name("C.lnk", &mut ini), name("D.lnk", &mut ini)];
    assert!(a.as_deref().is_some_and(|n| !n.is_empty() && !n.contains('@')), "{a:?}");
    assert_eq!(others, [None, None, None], "another folder, a missing DLL, no entry");
    assert_eq!(ini.as_ref().map(|(d, bytes)| (d.clone(), bytes.is_some())), Some((dir.clone(), true)));
}

/// The `desktop.ini` cache of an index build holds one folder's file at most.
#[test]
fn the_desktop_ini_cache_holds_one_folder() {
    let s = Scratch::new("inislot");
    let (a, b) = (s.0.join("A"), s.0.join("B"));
    let text = "[LocalizedFileNames]\r\nX.lnk=@%SystemRoot%\\system32\\shell32.dll,-22022\r\n";
    let mut bytes = vec![0xFF, 0xFE];
    bytes.extend(text.encode_utf16().flat_map(u16::to_le_bytes));
    for d in [&a, &b] {
        std::fs::create_dir_all(d).expect("test folder");
        std::fs::write(d.join("desktop.ini"), &bytes).expect("desktop.ini");
    }
    let mut ini: IniSlot = None;
    let held = |ini: &IniSlot| ini.as_ref().map(|(d, f)| (d.clone(), f.as_ref().map_or(0, Vec::len)));
    let first = localized_lnk_name(&a.join("X.lnk"), &mut ini);
    let after_a = held(&ini);
    let second = localized_lnk_name(&b.join("X.lnk"), &mut ini);
    let after_b = held(&ini);
    let again = localized_lnk_name(&a.join("X.lnk"), &mut ini);
    let after_again = held(&ini);
    assert!(first.as_deref().is_some_and(|n| !n.is_empty()), "{first:?}");
    assert_eq!([&second, &again], [&first, &first], "the same name from either folder, and after another folder");
    assert_eq!(after_a, Some((a.clone(), bytes.len())));
    assert_eq!(after_b, Some((b.clone(), bytes.len())), "the folder before is dropped");
    assert_eq!(after_again, Some((a.clone(), bytes.len())), "read again after another folder");
    assert!(bytes.len() as u64 <= MAX_DESKTOP_INI);
}

#[test]
fn stems_and_suffixes() {
    assert_eq!(strip_paren_suffix("PowerToys (Preview)"), "PowerToys");
    assert_eq!(strip_paren_suffix("Steam"), "Steam");
    assert_eq!(expand_env("%NOPE_X%\\a"), "%NOPE_X%\\a");
    assert!(is_local_file_path(r"C:\a\b.exe") && !is_local_file_path(r"\\server\share\b.exe"));
    let _ =
        (parse_lnk(&[]), LnkFacts { target: String::new(), has_args: false }, MAX_LNK_FILE, read_lnk(Path::new("")));
    let _ = localized_name_ref(&[], "x");
}
