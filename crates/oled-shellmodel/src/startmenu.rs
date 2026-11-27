//! The dock's Start Menu shortcut index, as the dock builds it (`shell_thread.rs` and `jumplist.rs` at the base of this
//! copy), so that a program has the same name here as on the dock's bar: `exe path -> shortcut name`, ambiguous
//! targets dropped, "uninstall" and "readme" links and links with arguments skipped.
//!
//! SECURITY (as the dock): a per-user shortcut can be written by any program of the user, so its bytes never reach the
//! shell link loader: it is read by the Rust parser below (`read_lnk`, through `oled_sys::datadir::read_file`: no
//! reparse point, a single link, capped) and named by its folder's `desktop.ini` (a string of a system DLL only) or by
//! its file name. The all-users folder is writable by administrators only, so its links go to the shell (target and
//! localized display name).
//!
//! KNOWN DUPLICATE: the bodies below are copies of the dock's, until the dock switches to this module. The dock's
//! helper process for per-user links (its "agent") has no counterpart here: the per-user path is the Rust parser's.
//! COM (STA) must be initialized on the thread that builds an index.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use windows::core::{Interface, GUID, HSTRING, PCWSTR, PWSTR};
use windows::Win32::Globalization::{MultiByteToWideChar, CP_ACP, MULTI_BYTE_TO_WIDE_CHAR_FLAGS};
use windows::Win32::Storage::FileSystem::WIN32_FIND_DATAW;
use windows::Win32::System::Com::{CoCreateInstance, IBindCtx, IPersistFile, CLSCTX_INPROC_SERVER, STGM_READ};
use windows::Win32::UI::Shell::{
    FOLDERID_CommonPrograms, FOLDERID_Programs, IShellItem, IShellLinkW, SHCreateItemFromParsingName,
    SHGetKnownFolderPath, ShellLink, KF_FLAG_DEFAULT, SIGDN_NORMALDISPLAY,
};

use crate::identity::norm_path;
use crate::names::{in_windows_dir_of, NameSources};
use crate::version::{take_pwstr, FileInfo};

/// Refresh a missed lookup only if the index is older than this.
pub const INDEX_MAX_AGE: Duration = Duration::from_secs(600);
/// Maximum folder depth scanned under each Start Menu root.
pub const INDEX_MAX_DEPTH: u32 = 6;
/// Most `.lnk` files indexed under each Start Menu root.
pub const INDEX_MAX_FILES: usize = 4096;

pub const MAX_LNK_FILE: u64 = 128 * 1024;
/// EnvironmentVariableDataBlock ([MS-SHLLINK] 2.5.4): its signature and its only valid size (size,
/// signature, TargetAnsi of 260 bytes, TargetUnicode of 520 bytes).
const ENV_BLOCK: u32 = 0xA000_0001;
const ENV_BLOCK_SIZE: usize = 0x314;

/// Largest `desktop.ini` read for the localized names of a Start menu folder (`localized_name_ref`).
pub const MAX_DESKTOP_INI: u64 = 16 * 1024;
/// CLSID of "This PC" {20D04FE0-3AEA-1069-A2D8-08002B30309D}, as stored in the root item of an id list.
const CLSID_MY_COMPUTER: [u8; 16] =
    [0xE0, 0x4F, 0xD0, 0x20, 0xEA, 0x3A, 0x69, 0x10, 0xA2, 0xD8, 0x08, 0x00, 0x2B, 0x30, 0x30, 0x9D];

/// CLSID_ShellLink {00021401-0000-0000-C000-000000000046}, as stored before each link.
const CLSID_LINK: [u8; 16] = [0x01, 0x14, 0x02, 0, 0, 0, 0, 0, 0xC0, 0, 0, 0, 0, 0, 0, 0x46];
/// FMTID of `PKEY_Title` {F29F85E0-4FF9-1068-AB91-08002B27B3D9} (property 2).
const FMTID_TITLE: [u8; 16] =
    [0xE0, 0x85, 0x9F, 0xF2, 0xF9, 0x4F, 0x68, 0x10, 0xAB, 0x91, 0x08, 0x00, 0x2B, 0x27, 0xB3, 0xD9];
/// FMTID of `PKEY_AppUserModel_IsDestListSeparator` {9F4C2855-9F79-4B39-A8D0-E1D42DE1D5F3} (property 6).
const FMTID_APP: [u8; 16] =
    [0x55, 0x28, 0x4C, 0x9F, 0x79, 0x9F, 0x39, 0x4B, 0xA8, 0xD0, 0xE1, 0xD4, 0x2D, 0xE1, 0xD5, 0xF3];

/// What the Start menu index and the Start import take from a `.lnk` file (`parse_lnk`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LnkFacts {
    /// A local `X:\...` path, or `%NAME%\...` from the environment block, not expanded here.
    pub target: String,
    /// The link passes arguments to its target.
    pub has_args: bool,
}

/// Why a `.lnk` file gives no target (`parse_lnk`), for the counts of the Start menu index log line.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LnkMiss {
    /// The link passes arguments, so it names something other than its exe (refused by the callers).
    Arguments,
    /// An id list that names a shell object, not a drive path: Control Panel, Run, This PC, a shell
    /// folder this parser does not walk.
    ShellItem,
    /// A Windows Installer (advertised) shortcut: only Windows Installer knows its target.
    Advertised,
    /// A network (UNC) target.
    Network,
    /// A path this parser refuses: relative, too long, with characters a file path cannot have.
    Refused,
    /// Not a shell link, cut short, malformed, or not readable (missing, too big, a reparse point).
    Damaged,
}

impl LnkMiss {
    /// Every kind, in the order of the log line.
    pub const ALL: [LnkMiss; 6] = [
        LnkMiss::Arguments,
        LnkMiss::ShellItem,
        LnkMiss::Advertised,
        LnkMiss::Network,
        LnkMiss::Refused,
        LnkMiss::Damaged,
    ];

    /// Its name in the log line.
    pub fn label(self) -> &'static str {
        match self {
            LnkMiss::Arguments => "arguments",
            LnkMiss::ShellItem => "shell item",
            LnkMiss::Advertised => "advertised",
            LnkMiss::Network => "network",
            LnkMiss::Refused => "refused path",
            LnkMiss::Damaged => "damaged",
        }
    }
}

/// The fields of one serialized shell link that this module uses (`parse_link_raw`), unchecked.
#[allow(dead_code)] // as the dock's: its jump lists read the title and the separator too
struct RawLink {
    /// LinkInfo's local path.
    info_path: Option<String>,
    /// A LinkInfo was read (`info_path` may still be none), and it has a network part.
    has_info: bool,
    info_network: bool,
    /// HasDarwinID: a Windows Installer (advertised) shortcut.
    darwin: bool,
    /// Where the items of the id list are, if there is one.
    id_list: Option<std::ops::Range<usize>>,
    /// The environment block's target (HasExpString set).
    env_path: Option<String>,
    name: String,
    args: String,
    title: Option<String>,
    separator: bool,
}

/// A `.lnk` of the Start Menu index. `per_user`: under the user's own Programs folder, which any program of the user
/// can write, so its bytes never reach the shell link loader; the all-users folder is writable by administrators only.
pub struct StartLnk {
    pub path: PathBuf,
    pub per_user: bool,
}

/// Counts of one index build (the resolver's log line): per-user links the Rust parser read, those it could not read by
/// kind (in the order of `LnkMiss::ALL`), and all-users links given to the shell link loader.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct IndexStats {
    pub rust_parsed: usize,
    pub rust_unresolved: [usize; LnkMiss::ALL.len()],
    pub com_common: usize,
}

// ------------------------------------------------------------------ the .lnk parser (jumplist.rs)

fn u16_at(b: &[u8], o: usize) -> Option<u16> {
    Some(u16::from_le_bytes(b.get(o..o.checked_add(2)?)?.try_into().ok()?))
}

fn u32_at(b: &[u8], o: usize) -> Option<u32> {
    Some(u32::from_le_bytes(b.get(o..o.checked_add(4)?)?.try_into().ok()?))
}

/// `b[o..o + n]`, or `None` when it does not fit.
fn span(b: &[u8], o: usize, n: usize) -> Option<&[u8]> {
    b.get(o..o.checked_add(n)?)
}

/// UTF-16LE text up to the first NUL (or the end).
fn utf16(b: &[u8]) -> String {
    let units: Vec<u16> = b.chunks_exact(2).map(|c| u16::from_le_bytes([c[0], c[1]])).take_while(|&u| u != 0).collect();
    String::from_utf16_lossy(&units)
}

/// Text in the system code page up to the first NUL (or the end).
fn ansi(b: &[u8]) -> String {
    let b = &b[..b.iter().position(|&c| c == 0).unwrap_or(b.len())];
    if b.is_ascii() {
        return String::from_utf8_lossy(b).into_owned();
    }
    let mut wide = vec![0u16; b.len()];
    // SAFETY: both buffers are valid for their lengths; the output is as long as the input, which
    // is enough for any single- or double-byte code page.
    let n = unsafe { MultiByteToWideChar(CP_ACP, MULTI_BYTE_TO_WIDE_CHAR_FLAGS(0), b, Some(&mut wide)) };
    String::from_utf16_lossy(&wide[..(n.max(0) as usize).min(wide.len())])
}

/// The fields of the shell link at `*pos`, which is moved past it; `None` when the data is not a
/// shell link. `honour_force_no_link_info`: a link with ForceNoLinkInfo has no LinkInfo path, as
/// the shell reads it (`.lnk` files; jump lists keep their reading).
fn parse_link_raw(b: &[u8], pos: &mut usize, honour_force_no_link_info: bool) -> Option<RawLink> {
    let mut o = *pos;
    span(b, o, 0x4C)?;
    if u32_at(b, o)? != 0x4C {
        return None;
    }
    let flags = u32_at(b, o + 20)?;
    o += 0x4C;
    // LinkTargetIDList: only where it is (`id_list_path` reads it for a `.lnk` without a LinkInfo path).
    let mut id_list = None;
    if flags & 0x01 != 0 {
        let n = usize::from(u16_at(b, o)?);
        span(b, o, 2 + n)?;
        id_list = Some(o + 2..o + 2 + n);
        o += 2 + n;
    }
    let (mut info_path, mut has_info, mut info_network) = (None, false, false);
    if flags & 0x02 != 0 {
        let n = u32_at(b, o)? as usize;
        if n < 0x1C {
            return None;
        }
        let info = span(b, o, n)?;
        if !(honour_force_no_link_info && flags & 0x100 != 0) {
            info_path = link_info_path(info);
            has_info = true;
            // LinkInfoFlags: CommonNetworkRelativeLinkAndPathSuffix.
            info_network = u32_at(info, 8)? & 0x02 != 0;
        }
        o += n;
    }
    // StringData: NAME, RELATIVE_PATH, WORKING_DIR, ARGUMENTS, ICON_LOCATION, each counted.
    let unicode = flags & 0x80 != 0;
    let (mut name, mut args) = (String::new(), String::new());
    for bit in [0x04u32, 0x08, 0x10, 0x20, 0x40] {
        if flags & bit == 0 {
            continue;
        }
        let n = usize::from(u16_at(b, o)?) * if unicode { 2 } else { 1 };
        let raw = span(b, o + 2, n)?;
        o += 2 + n;
        if bit == 0x04 || bit == 0x20 {
            let text = if unicode { utf16(raw) } else { ansi(raw) };
            if bit == 0x04 {
                name = text;
            } else {
                args = text;
            }
        }
    }
    // ExtraData: blocks until a size below 4. The property store holds the title; the environment
    // block (read only with HasExpString, as the shell does) holds the unexpanded target.
    let (mut title, mut separator, mut env_path) = (None, false, None);
    loop {
        let n = u32_at(b, o)? as usize;
        if n < 4 {
            o += 4;
            break;
        }
        let block = span(b, o, n)?;
        if n >= 8 && u32_at(block, 4)? == 0xA000_0009 {
            let _ = properties(&block[8..], &mut title, &mut separator);
        } else if n == ENV_BLOCK_SIZE && flags & 0x200 != 0 && u32_at(block, 4)? == ENV_BLOCK {
            env_path = env_block_path(block);
        }
        o += n;
    }
    *pos = o;
    let darwin = flags & 0x1000 != 0;
    Some(RawLink { info_path, has_info, info_network, darwin, id_list, env_path, name, args, title, separator })
}

/// The target of an EnvironmentVariableDataBlock: TargetUnicode, else TargetAnsi.
fn env_block_path(block: &[u8]) -> Option<String> {
    let wide = utf16(span(block, 8 + 260, 520)?);
    let text = if wide.is_empty() { ansi(span(block, 8, 260)?) } else { wide };
    (!text.is_empty()).then_some(text)
}

/// The target of a `.lnk` file, parsed in safe Rust (the structure of a jump list link, without
/// the CLSID in front), in this order: LinkInfo's local path unless ForceNoLinkInfo is set, the
/// environment block's target, left unexpanded (the caller expands it with `expand_env`
/// and checks the result with `is_local_file_path`), then the drive path of the id list
/// (`id_list_path`). An advertised shortcut is never resolved. Never panics.
pub fn parse_lnk(b: &[u8]) -> Result<LnkFacts, LnkMiss> {
    if span(b, 4, 16) != Some(&CLSID_LINK[..]) {
        return Err(LnkMiss::Damaged);
    }
    let raw = parse_link_raw(b, &mut 0, true).ok_or(LnkMiss::Damaged)?;
    if raw.darwin {
        return Err(LnkMiss::Advertised);
    }
    let has_args = !raw.args.is_empty();
    let paths = raw.has_info || raw.env_path.is_some();
    if let Some(target) = raw.info_path.filter(|t| is_local_file_path(t)) {
        return Ok(LnkFacts { target, has_args });
    }
    if let Some(target) = raw.env_path.filter(|t| is_local_file_path(t) || is_env_file_path(t)) {
        return Ok(LnkFacts { target, has_args });
    }
    let id_path = raw.id_list.and_then(|r| b.get(r)).map(id_list_path);
    match id_path {
        Some(Ok(target)) if is_local_file_path(&target) => Ok(LnkFacts { target, has_args }),
        _ if raw.info_network => Err(LnkMiss::Network),
        Some(Ok(_)) => Err(LnkMiss::Refused),
        _ if paths => Err(LnkMiss::Refused),
        Some(Err(miss)) => Err(miss),
        None => Err(LnkMiss::Refused),
    }
}

/// `parse_lnk` of the file `path`, read through `oled_sys::datadir::read_file` (no reparse point, a single
/// link, at most `MAX_LNK_FILE` bytes).
pub fn read_lnk(path: &Path) -> Result<LnkFacts, LnkMiss> {
    let bytes = oled_sys::datadir::read_file(path, MAX_LNK_FILE, true).map_err(|_| LnkMiss::Damaged)?;
    parse_lnk(&bytes)
}

/// The path an id list names, if it is a drive path as Windows writes it: the root item "This PC",
/// a volume item `X:\`, then one file entry item per folder and for the file, each with its long name
/// in a 0xBEEF0004 extension block (the layout of the items is not in [MS-SHLLINK]; it follows the
/// public descriptions of the Windows shell item format). `ShellItem` for any other list,
/// `Refused` for a name that is no single path component, `Damaged` for items that do not fit.
fn id_list_path(list: &[u8]) -> Result<String, LnkMiss> {
    let mut path = String::new();
    let (mut o, mut n) = (0usize, 0usize);
    loop {
        let size = usize::from(u16_at(list, o).ok_or(LnkMiss::Damaged)?);
        if size == 0 {
            break;
        }
        let item = span(list, o, size).filter(|_| size >= 3).ok_or(LnkMiss::Damaged)?;
        let kind = item[2];
        match n {
            0 if kind == 0x1F && span(item, 4, 16) == Some(&CLSID_MY_COMPUTER[..]) => {}
            1 if kind & 0x70 == 0x20 => {
                let drive = item.get(3..7).ok_or(LnkMiss::ShellItem)?;
                if !(drive[0].is_ascii_alphabetic() && drive[1..] == *b":\\\0") {
                    return Err(LnkMiss::ShellItem);
                }
                path.push(char::from(drive[0]));
                path.push_str(":\\");
            }
            _ if n >= 2 && kind & 0x70 == 0x30 => {
                let name = long_name(item).ok_or(LnkMiss::Refused)?;
                if name.is_empty() || name == "." || name == ".." || name.contains(['\\', '/']) {
                    return Err(LnkMiss::Refused);
                }
                if n > 2 {
                    path.push('\\');
                }
                path.push_str(&name);
                if path.len() > 1024 {
                    return Err(LnkMiss::Refused);
                }
            }
            _ => return Err(LnkMiss::ShellItem),
        }
        o += size;
        n += 1;
    }
    if n < 3 {
        return Err(LnkMiss::ShellItem);
    }
    Ok(path)
}

/// The long name of a file entry shell item: after the header (14 bytes) and the primary name (ANSI,
/// padded to an even length, or UTF-16 when the type has 0x04), the extension blocks; the 0xBEEF0004
/// one holds the name, NUL-terminated, at an offset that depends on its version (3, 7, 8 or 9).
fn long_name(item: &[u8]) -> Option<String> {
    let rest = item.get(14..)?;
    let name = if item[2] & 0x04 != 0 {
        2 * (rest.chunks_exact(2).position(|c| c == [0, 0])? + 1)
    } else {
        rest.iter().position(|&c| c == 0)? + 1
    };
    let mut o = 14 + name;
    o += o & 1;
    loop {
        let size = usize::from(u16_at(item, o)?);
        let block = span(item, o, size).filter(|_| size >= 8)?;
        if u32_at(block, 4)? == 0xBEEF_0004 {
            let at = match u16_at(block, 2)? {
                3 => 20,
                7 => 38,
                8 => 42,
                9 => 46,
                _ => return None,
            };
            // The name, then its NUL, then the u16 that ends every block.
            let units: Vec<u16> = block
                .get(at..size.checked_sub(2)?)?
                .chunks_exact(2)
                .map(|c| u16::from_le_bytes([c[0], c[1]]))
                .collect();
            let end = units.iter().position(|&u| u == 0)?;
            return String::from_utf16(&units[..end]).ok();
        }
        o += size;
    }
}

/// The localized name `desktop.ini` (`ini`, UTF-16 with its byte order mark, else the system code
/// page) gives the file `file_name` of its folder, as the shell reads it: `[LocalizedFileNames]`,
/// `<file>=@%SystemRoot%\system32\<name>.dll,-<id>` (or `%windir%`). Returns the DLL's file name,
/// lowercase, and the string id; the caller loads the DLL from `GetSystemDirectoryW`, never from the
/// environment. Any other value (another folder, a path, no id) gives `None`.
pub fn localized_name_ref(ini: &[u8], file_name: &str) -> Option<(String, u16)> {
    let text = match ini {
        [0xFF, 0xFE, rest @ ..] => utf16(rest),
        _ => ansi(ini),
    };
    let wanted = file_name.to_lowercase();
    let mut names = false;
    for line in text.lines().map(str::trim) {
        if let Some(section) = line.strip_prefix('[').and_then(|l| l.strip_suffix(']')) {
            names = section.trim().eq_ignore_ascii_case("LocalizedFileNames");
        } else if let Some((key, value)) = line.split_once('=').filter(|_| names) {
            if key.trim().to_lowercase() == wanted {
                return system_string_ref(value.trim());
            }
        }
    }
    None
}

/// `@%SystemRoot%\system32\<name>.dll,-<id>` -> (`<name>.dll` lowercase, id), for a plain file name
/// (letters, digits, `.`, `_`, `-`) and an id of 1 to 65535.
fn system_string_ref(value: &str) -> Option<(String, u16)> {
    let (module, id) = value.strip_prefix('@')?.rsplit_once(",-")?;
    if id.is_empty() || !id.bytes().all(|c| c.is_ascii_digit()) {
        return None;
    }
    let id = id.parse::<u16>().ok().filter(|&id| id != 0)?;
    let module = module.to_ascii_lowercase();
    let name = ["%systemroot%\\system32\\", "%windir%\\system32\\"].iter().find_map(|p| module.strip_prefix(p))?;
    let plain = name.len() > 4
        && name.len() <= 64
        && name.ends_with(".dll")
        && !name.starts_with('.')
        && name.bytes().all(|c| c.is_ascii_alphanumeric() || b"._-".contains(&c));
    plain.then(|| (name.to_string(), id))
}

/// The local path of a LinkInfo structure (`LocalBasePath` + `CommonPathSuffix`), if it has one.
fn link_info_path(info: &[u8]) -> Option<String> {
    let header = u32_at(info, 4)? as usize;
    if u32_at(info, 8)? & 1 == 0 {
        return None;
    }
    let text = |at: usize, wide: bool| -> Option<String> {
        let off = u32_at(info, at)? as usize;
        let raw = info.get(off..).filter(|_| off != 0)?;
        Some(if wide { utf16(raw) } else { ansi(raw) })
    };
    let (mut base, mut suffix) = (String::new(), String::new());
    if header >= 0x24 {
        base = text(28, true).unwrap_or_default();
        suffix = text(32, true).unwrap_or_default();
    }
    if base.is_empty() {
        base = text(16, false)?;
        suffix = text(24, false).unwrap_or_default();
    }
    (!base.is_empty()).then(|| base + &suffix)
}

#[allow(clippy::while_immutable_condition)]
fn properties(b: &[u8], title: &mut Option<String>, separator: &mut bool) -> Option<()> {
    let mut o = 0usize;
    loop {
        let size = u32_at(b, o)? as usize;
        if size < 24 {
            return Some(());
        }
        let storage = span(b, o, size)?;
        let fmtid = &storage[8..24];
        let mut p = 24usize;
        // Values: size, id, one reserved byte, then type (u16), padding (u16) and the data.
        while fmtid == FMTID_TITLE || fmtid == FMTID_APP {
            let n = u32_at(storage, p)? as usize;
            if n < 13 {
                break;
            }
            let value = span(storage, p, n)?;
            let (id, vt, data) = (u32_at(value, 4)?, u16_at(value, 9)?, &value[13..]);
            if fmtid == FMTID_TITLE && id == 2 && vt == 0x1F {
                let chars = u32_at(data, 0)? as usize;
                *title = Some(utf16(span(data, 4, chars.checked_mul(2)?)?));
            } else if fmtid == FMTID_APP && id == 6 && vt == 0x0B {
                *separator = u16_at(data, 0)? != 0;
            }
            p += n;
        }
        o += size;
    }
}

/// `X:\...` without characters a file path cannot have.
pub fn is_local_file_path(p: &str) -> bool {
    let b = p.as_bytes();
    b.len() > 3
        && b.len() <= 1024
        && b[0].is_ascii_alphabetic()
        && b[1] == b':'
        && b[2] == b'\\'
        && !p[2..].chars().any(|c| c.is_control() || "\"<>|?*:".contains(c))
}

/// `%NAME%\...`: an environment block target before expansion, a plain variable name, then a path
/// with the characters `is_local_file_path` accepts.
pub fn is_env_file_path(p: &str) -> bool {
    let Some((name, rest)) = p.strip_prefix('%').and_then(|r| r.split_once('%')) else { return false };
    !name.is_empty()
        && name.chars().all(|c| c.is_ascii_alphanumeric() || "_()".contains(c))
        && rest.starts_with('\\')
        && is_local_file_path(&format!("C:{rest}"))
}

// ------------------------------------------------------------------ the index (shell_thread.rs)

/// `Name (Preview)` / `Name (x64)` -> `Name`.
pub fn strip_paren_suffix(name: &str) -> String {
    let t = name.trim();
    if t.ends_with(')') {
        if let Some(i) = t.rfind(" (") {
            let head = t[..i].trim_end();
            if !head.is_empty() {
                return head.to_string();
            }
        }
    }
    t.to_string()
}

pub fn known_folder(id: &GUID) -> Option<std::path::PathBuf> {
    // SAFETY: COM initialized; `SHGetKnownFolderPath` returns a CoTaskMemAlloc'd string, which `take_pwstr` frees.
    let p = unsafe { SHGetKnownFolderPath(id, KF_FLAG_DEFAULT, None) }.ok()?;
    // SAFETY: `p` is that string and is not used after this call.
    unsafe { take_pwstr(p) }.map(std::path::PathBuf::from)
}

/// The `.lnk` files under `dir`, at most `INDEX_MAX_DEPTH` folders deep and `*left` files in all.
pub fn collect_lnk(dir: &Path, depth: u32, per_user: bool, left: &mut usize, out: &mut Vec<StartLnk>) {
    let Ok(rd) = std::fs::read_dir(dir) else { return };
    for e in rd.flatten() {
        if *left == 0 {
            return;
        }
        let p = e.path();
        let Ok(ft) = e.file_type() else { continue };
        if ft.is_dir() {
            if depth < INDEX_MAX_DEPTH {
                collect_lnk(&p, depth + 1, per_user, left, out);
            }
        } else if p.extension().is_some_and(|x| x.eq_ignore_ascii_case("lnk")) {
            *left -= 1;
            out.push(StartLnk { path: p, per_user });
        }
    }
}

/// Resolves the target of a .lnk without UI and without searching (`IPersistFile::Load` + `GetPath`).
pub fn lnk_target(link: &IShellLinkW, file: &IPersistFile, lnk: &Path) -> Option<String> {
    let wide = HSTRING::from(lnk.as_os_str());
    // SAFETY: COM STA initialized; `wide` is NUL-terminated and lives for the call.
    unsafe { file.Load(PCWSTR(wide.as_ptr()), STGM_READ) }.ok()?;
    let mut buf = [0u16; 520];
    let mut fd = WIN32_FIND_DATAW::default();
    // SAFETY: `buf` and `fd` are writable and sized as declared; flags 0 = long path.
    unsafe { link.GetPath(&mut buf, &mut fd, 0) }.ok()?;
    // Shortcuts with arguments ("explorer.exe <folder>", "java -jar ...") name something other than the exe.
    let mut args = [0u16; 8];
    // SAFETY: `args` is writable with the declared length; a longer string just fails (treated as "has arguments").
    if unsafe { link.GetArguments(&mut args) }.is_err() || args[0] != 0 {
        return None;
    }
    let n = buf.iter().position(|&c| c == 0).unwrap_or(buf.len());
    let s = String::from_utf16_lossy(&buf[..n]);
    Some(s).filter(|s| !s.is_empty())
}

/// Display name of a `.lnk` as the shell shows it: localized, without the extension.
pub fn lnk_display_name(lnk: &Path) -> Option<String> {
    let wide = HSTRING::from(lnk.as_os_str());
    // SAFETY: COM STA initialized on this thread; `wide` lives for the call.
    let item: IShellItem = unsafe { SHCreateItemFromParsingName(&wide, None::<&IBindCtx>) }.ok()?;
    // SAFETY: `item` is valid; `GetDisplayName` returns a CoTaskMemAlloc'd string, which `take_pwstr` frees.
    let name = unsafe { take_pwstr(item.GetDisplayName(SIGDN_NORMALDISPLAY).ok()?) }?;
    let name = name.trim();
    let name = if name.len() > 4 && name.to_lowercase().ends_with(".lnk") { &name[..name.len() - 4] } else { name };
    Some(name.to_string()).filter(|n| !n.is_empty())
}

/// `System32` from `GetSystemDirectoryW` (never from an environment variable).
pub fn system_dir() -> Option<std::path::PathBuf> {
    let mut buf = [0u16; 260];
    // SAFETY: `buf` is writable and its length is passed by the wrapper.
    let n = unsafe { windows::Win32::System::SystemInformation::GetSystemDirectoryW(Some(&mut buf)) } as usize;
    (n > 0 && n < buf.len()).then(|| std::path::PathBuf::from(String::from_utf16_lossy(&buf[..n])))
}

/// The name the folder's `desktop.ini` gives the per-user shortcut `lnk`, as the shell shows it
/// (Windows' own shortcuts of the per-user folder, "Command Prompt.lnk" and the accessibility tools,
/// are localized this way): read through `datadir::read_file` (capped, no reparse point), and only a
/// string of a DLL of the system directory (`jumplist::localized_name_ref`, `system_string`).
/// `ini` holds the file of the last folder asked, so one index build holds one folder's bytes at most (`MAX_DESKTOP_INI`),
/// however many folders a per-user tree has; a folder's file is read again after a subfolder's.
pub fn localized_lnk_name(lnk: &Path, ini: &mut IniSlot) -> Option<String> {
    let dir = lnk.parent()?;
    if ini.as_ref().map(|(d, _)| d.as_path()) != Some(dir) {
        *ini = None; // the last folder's bytes go before the next are read
        let bytes = oled_sys::datadir::read_file(&dir.join("desktop.ini"), MAX_DESKTOP_INI, true).ok();
        *ini = Some((dir.to_path_buf(), bytes));
    }
    let bytes = ini.as_ref()?.1.as_deref()?;
    let file_name = lnk.file_name()?.to_string_lossy();
    let (module, id) = localized_name_ref(bytes, &file_name)?;
    system_string(&module, id)
}

/// `localized_lnk_name`'s cache: a folder and its `desktop.ini` (None: none, or not readable).
pub type IniSlot = Option<(std::path::PathBuf, Option<Vec<u8>>)>;

/// String `id` of the DLL `module` (a plain file name) of the system directory (`system_dir`, never
/// the environment), in the display language. The DLL is mapped as an image resource, so none of
/// its code runs, and released at once.
fn system_string(module: &str, id: u16) -> Option<String> {
    use windows::Win32::Foundation::FreeLibrary;
    use windows::Win32::System::LibraryLoader::{
        LoadLibraryExW, LOAD_LIBRARY_AS_DATAFILE, LOAD_LIBRARY_AS_IMAGE_RESOURCE,
    };
    use windows::Win32::UI::WindowsAndMessaging::LoadStringW;
    let path = HSTRING::from(system_dir()?.join(module).as_os_str());
    // SAFETY: `path` is NUL-terminated and lives for the call; the file is mapped as data, never run.
    let lib = unsafe { LoadLibraryExW(&path, None, LOAD_LIBRARY_AS_DATAFILE | LOAD_LIBRARY_AS_IMAGE_RESOURCE) }.ok()?;
    let mut buf = [0u16; 260];
    // SAFETY: `lib` is the module mapped above; `buf` is writable for the length passed.
    let n = unsafe { LoadStringW(Some(lib.into()), u32::from(id), PWSTR(buf.as_mut_ptr()), buf.len() as i32) };
    // SAFETY: handle from LoadLibraryExW above, freed once; nothing from it is kept.
    let _ = unsafe { FreeLibrary(lib) };
    let name = String::from_utf16_lossy(&buf[..usize::try_from(n).unwrap_or(0).min(buf.len())]);
    let name = name.trim();
    (!name.is_empty() && !name.chars().any(char::is_control)).then(|| name.to_string())
}

/// `%NAME%` variables of the process environment expanded; unknown ones are kept as written.
pub fn expand_env(s: &str) -> String {
    let mut out = String::new();
    let mut parts = s.split('%');
    out.push_str(parts.next().unwrap_or(""));
    while let Some(var) = parts.next() {
        match (parts.next(), std::env::var(var)) {
            (Some(rest), Ok(v)) => {
                out.push_str(&v);
                out.push_str(rest);
            }
            // Unknown variable or unbalanced '%': keep the text as is.
            (Some(rest), Err(_)) => {
                out.push('%');
                out.push_str(var);
                out.push('%');
                out.push_str(rest);
            }
            (None, _) => {
                out.push('%');
                out.push_str(var);
            }
        }
    }
    out
}

/// The loop of `StartMenuIndex::build_from` (pure): `exe path -> name` pairs of `files`, ambiguous targets
/// dropped. A per-user link is read by `rust_target` and `rust_display` only (its `desktop.ini` name, else the
/// file's); an all-users one by `com_target` and `com_display`.
pub fn index_links(
    files: &[StartLnk],
    rust_target: &mut dyn FnMut(&StartLnk) -> Result<String, LnkMiss>,
    rust_display: &mut dyn FnMut(&StartLnk) -> Option<String>,
    com_target: &mut dyn FnMut(&StartLnk) -> Option<String>,
    com_display: &mut dyn FnMut(&StartLnk) -> Option<String>,
) -> (HashMap<String, String>, IndexStats) {
    let mut stats = IndexStats::default();
    let mut map: HashMap<String, String> = HashMap::new();
    let mut ambiguous: Vec<String> = Vec::new();
    for lnk in files {
        let target = if lnk.per_user {
            match rust_target(lnk) {
                Ok(t) => {
                    stats.rust_parsed += 1;
                    Some(t)
                }
                Err(miss) => {
                    if let Some(i) = LnkMiss::ALL.iter().position(|&k| k == miss) {
                        stats.rust_unresolved[i] += 1;
                    }
                    None
                }
            }
        } else {
            stats.com_common += 1;
            com_target(lnk)
        };
        let Some(target) = target else { continue };
        let target = if target.contains('%') { expand_env(&target) } else { target };
        if !target.to_lowercase().ends_with(".exe") {
            continue;
        }
        let Some(stem) = lnk.path.file_stem().map(|s| s.to_string_lossy().into_owned()) else { continue };
        let lower = stem.to_lowercase();
        if lower.contains("uninstall") || lower.contains("readme") {
            continue;
        }
        // The name the shell shows (Windows' own shortcuts are localized through `desktop.ini`:
        // "Remote Desktop Connection.lnk" is "Connessione Desktop remoto" in Italian, as in the
        // Start menu), else the file name.
        let shown = if lnk.per_user { rust_display(lnk) } else { com_display(lnk) };
        let name = strip_paren_suffix(&shown.unwrap_or(stem));
        if name.is_empty() {
            continue;
        }
        let key = norm_path(&target);
        match map.get(&key) {
            Some(old) if !old.eq_ignore_ascii_case(&name) => ambiguous.push(key),
            Some(_) => {}
            None => {
                map.insert(key, name);
            }
        }
    }
    for k in ambiguous {
        map.remove(&k);
    }
    (map, stats)
}

/// The index: `exe path -> name` and when it was built.
#[derive(Debug, Default)]
pub struct StartMenuIndex {
    /// normalised exe path -> shortcut name (ambiguous targets are not stored).
    map: HashMap<String, String>,
    built: Option<Instant>,
    stats: IndexStats,
}

impl StartMenuIndex {
    /// The all-users and the per-user Programs folders of this machine. COM STA on the calling thread.
    pub fn build() -> StartMenuIndex {
        let dirs: Vec<(PathBuf, bool)> = [(FOLDERID_CommonPrograms, false), (FOLDERID_Programs, true)]
            .iter()
            .filter_map(|(id, per_user)| known_folder(id).map(|d| (d, *per_user)))
            .collect();
        StartMenuIndex::build_from(&dirs)
    }

    /// The index of these folders (`per_user`: a folder any program of the user can write). COM STA on the calling thread.
    pub fn build_from(dirs: &[(PathBuf, bool)]) -> StartMenuIndex {
        let mut files = Vec::new();
        for (dir, per_user) in dirs {
            let mut left = INDEX_MAX_FILES;
            collect_lnk(dir, 0, *per_user, &mut left, &mut files);
        }
        // SAFETY: COM STA initialized on this thread; ShellLink CLSID with the requested interface.
        let link = unsafe { CoCreateInstance::<_, IShellLinkW>(&ShellLink, None, CLSCTX_INPROC_SERVER) }.ok();
        let file = link.as_ref().and_then(|l| l.cast::<IPersistFile>().ok());
        let mut rust_target = |l: &StartLnk| {
            let facts = read_lnk(&l.path)?;
            // Rejected with arguments, as `lnk_target` does.
            if facts.has_args {
                return Err(LnkMiss::Arguments);
            }
            let target = expand_env(&facts.target);
            if is_local_file_path(&target) {
                Ok(target)
            } else {
                Err(LnkMiss::Refused)
            }
        };
        // The `desktop.ini` of the last folder only: `collect_lnk` lists a folder's files mostly together.
        let mut ini: IniSlot = None;
        let mut rust_display = |l: &StartLnk| localized_lnk_name(&l.path, &mut ini);
        let mut com_target = |l: &StartLnk| {
            // An all-users link only: a per-user one never reaches the shell link loader.
            if l.per_user {
                return None;
            }
            match (&link, &file) {
                (Some(link), Some(file)) => lnk_target(link, file, &l.path),
                _ => None,
            }
        };
        let mut com_display = |l: &StartLnk| if l.per_user { None } else { lnk_display_name(&l.path) };
        let (mut map, stats) =
            index_links(&files, &mut rust_target, &mut rust_display, &mut com_target, &mut com_display);
        map.shrink_to_fit();
        StartMenuIndex { map, built: Some(Instant::now()), stats }
    }

    /// The name of the Start Menu shortcut whose target is `exe_path`.
    pub fn name(&self, exe_path: &str) -> Option<String> {
        self.map.get(&norm_path(exe_path)).cloned()
    }

    /// true when it was never built or is older than `INDEX_MAX_AGE` at `now`: a missed lookup then rebuilds it.
    pub fn is_stale(&self, now: Instant) -> bool {
        self.built.is_none_or(|t| now.saturating_duration_since(t) >= INDEX_MAX_AGE)
    }

    /// Exe targets indexed.
    pub fn len(&self) -> usize {
        self.map.len()
    }

    pub fn is_empty(&self) -> bool {
        self.map.is_empty()
    }

    /// The counts of the build, for a log line.
    pub fn stats(&self) -> &IndexStats {
        &self.stats
    }
}

/// The sources of a name as the dock has them: the catalogue, the Start Menu index, the version strings and
/// `%SystemRoot%` (`system_root`).
pub struct IndexedSources<'a> {
    pub index: &'a StartMenuIndex,
    pub system_root: &'a str,
}

impl NameSources for IndexedSources<'_> {
    fn appsfolder_name(&self, aumid: &str) -> Option<String> {
        crate::appsfolder::display_name(aumid)
    }

    fn start_menu_name(&self, exe_path: &str) -> Option<String> {
        self.index.name(exe_path)
    }

    fn file_info(&self, exe_path: &str) -> FileInfo {
        FileInfo::read(exe_path)
    }

    fn in_windows_dir(&self, exe_path: &str) -> bool {
        in_windows_dir_of(exe_path, self.system_root)
    }
}
