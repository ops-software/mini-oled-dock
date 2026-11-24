//! JUMP LIST of an app, for its context menu in the bar (`popup_app`): the tasks and the custom
//! categories the app itself registered with Windows ("New window", Steam's "Store" / "Library",
//! the recent folders of VS Code...), as the native taskbar shows them on a right click.
//!
//! SOURCE: `<Recent>\CustomDestinations\<hash>.customDestinations-ms`, written by Windows when an
//! app calls `ICustomDestinationList`. `<hash>` is a CRC-64 of the app's AppUserModelID
//! (`app_id_hash`). An app whose id is not known (a pin that is not running, a window that
//! publishes none) is found by its exe: the one file whose links all start that exe (`scan`).
//!
//! SECURITY: the dock runs elevated and these files are writable by any program of the user, so
//! they are untrusted input. They are read through `datadir::read_file` (no reparse points, capped)
//! and parsed HERE, in safe Rust with every offset checked (`parse`): the shell link loader of
//! Windows is never given these bytes, no icon is loaded from them and no `@dll,-id` title is
//! resolved. What comes out is text for the menu and a `(target, arguments)` pair that is started
//! only on a click, through `ShellRequest::Launch` (the desktop shell, at the user's own rights):
//! exactly what the program that wrote the file could already do by itself.
//!
//! START MENU SHORTCUTS: the same parser reads the `.lnk` files of the user's own Start menu folder
//! for the Start menu index and the Start import (`read_lnk`, `parse_lnk`), which any program of the
//! user can write too: the target path only (from LinkInfo, the environment block or a drive path id
//! list), never handed to the shell link loader; and the localized name the folder's `desktop.ini`
//! gives a shortcut, as a reference to a string of a system DLL only (`localized_name_ref`).
//!
//! NOT READ: the automatic lists (Recent / Frequent, `AutomaticDestinations`), items that are not
//! shell links, and links without a local file target.
//!
//! MEMORY: nothing is kept. The file is read when the menu opens and dropped with it.

use std::path::{Path, PathBuf};

use windows::Win32::Globalization::{MultiByteToWideChar, CP_ACP, MULTI_BYTE_TO_WIDE_CHAR_FLAGS};
use windows::Win32::System::Com::CoTaskMemFree;
use windows::Win32::UI::Shell::{FOLDERID_Recent, SHGetKnownFolderPath, KF_FLAG_DEFAULT};

/// Largest jump list file read for a known app id.
const MAX_FILE: u64 = 1 << 20;
/// Largest file looked at, most files looked at and most bytes read in all, when searching by
/// exe (`scan`): the search runs when the menu opens, so it is kept short whatever is in the folder.
const MAX_SCAN_FILE: u64 = 64 * 1024;
const MAX_SCAN: usize = 256;
const MAX_SCAN_BYTES: usize = 8 << 20;
/// Most categories and links per category parsed from one file.
const MAX_CATEGORIES: usize = 16;
const MAX_LINKS: usize = 64;
/// Most rows shown: tasks, and entries of all the other categories together.
pub(crate) const MAX_TASKS: usize = 9;
pub(crate) const MAX_OTHER: usize = 5;
/// Longest title (characters) and arguments (bytes) accepted.
const MAX_TITLE: usize = 80;
const MAX_ARGS: usize = 2048;
/// Largest `.lnk` file read (`read_lnk`).
const MAX_LNK_FILE: u64 = 128 * 1024;
/// EnvironmentVariableDataBlock ([MS-SHLLINK] 2.5.4): its signature and its only valid size (size,
/// signature, TargetAnsi of 260 bytes, TargetUnicode of 520 bytes).
const ENV_BLOCK: u32 = 0xA000_0001;
const ENV_BLOCK_SIZE: usize = 0x314;

/// Largest `desktop.ini` read for the localized names of a Start menu folder (`localized_name_ref`).
pub(crate) const MAX_DESKTOP_INI: u64 = 16 * 1024;
/// CLSID of "This PC" {20D04FE0-3AEA-1069-A2D8-08002B30309D}, as stored in the root item of an id list.
const CLSID_MY_COMPUTER: [u8; 16] =
    [0xE0, 0x4F, 0xD0, 0x20, 0xEA, 0x3A, 0x69, 0x10, 0xA2, 0xD8, 0x08, 0x00, 0x2B, 0x30, 0x30, 0x9D];

/// CLSID_ShellLink {00021401-0000-0000-C000-000000000046}, as stored before each link.
const CLSID_LINK: [u8; 16] = [0x01, 0x14, 0x02, 0, 0, 0, 0, 0, 0xC0, 0, 0, 0, 0, 0, 0, 0x46];
/// End of a category.
const FOOTER: [u8; 4] = [0xAB, 0xFB, 0xBF, 0xBA];
/// FMTID of `PKEY_Title` {F29F85E0-4FF9-1068-AB91-08002B27B3D9} (property 2).
const FMTID_TITLE: [u8; 16] = [0xE0, 0x85, 0x9F, 0xF2, 0xF9, 0x4F, 0x68, 0x10, 0xAB, 0x91, 0x08, 0x00, 0x2B, 0x27, 0xB3, 0xD9];
/// FMTID of `PKEY_AppUserModel_IsDestListSeparator` {9F4C2855-9F79-4B39-A8D0-E1D42DE1D5F3} (property 6).
const FMTID_APP: [u8; 16] = [0x55, 0x28, 0x4C, 0x9F, 0x79, 0x9F, 0x39, 0x4B, 0xA8, 0xD0, 0xE1, 0xD4, 0x2D, 0xE1, 0xD5, 0xF3];

/// One row of a jump list.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Entry {
    /// `title` as the app wrote it; `target` is an absolute local path, started with `args`.
    Link { title: String, target: String, args: String },
    Separator,
}

/// A category of a jump list. `name` is empty for the app's tasks.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Group {
    pub name: String,
    pub entries: Vec<Entry>,
}

/// What the Start menu index and the Start import take from a `.lnk` file (`parse_lnk`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct LnkFacts {
    /// A local `X:\...` path, or `%NAME%\...` from the environment block, not expanded here.
    pub(crate) target: String,
    /// The link passes arguments to its target.
    pub(crate) has_args: bool,
}

/// Why a `.lnk` file gives no target (`parse_lnk`), for the counts of the Start menu index log line.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum LnkMiss {
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
    pub(crate) const ALL: [LnkMiss; 6] = [
        LnkMiss::Arguments,
        LnkMiss::ShellItem,
        LnkMiss::Advertised,
        LnkMiss::Network,
        LnkMiss::Refused,
        LnkMiss::Damaged,
    ];

    /// Its name in the log line.
    pub(crate) fn label(self) -> &'static str {
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

// ------------------------------------------------------------------ app id -> file

/// The number Windows names the jump list files of `app_id` with: CRC-64 (reflected polynomial
/// 0x92C64265D32139A4, start value all ones) of the uppercase id as UTF-16LE.
pub(crate) fn app_id_hash(app_id: &str) -> u64 {
    let mut crc = u64::MAX;
    for unit in app_id.to_uppercase().encode_utf16() {
        for byte in unit.to_le_bytes() {
            crc ^= u64::from(byte);
            for _ in 0..8 {
                crc = if crc & 1 != 0 { (crc >> 1) ^ 0x92C6_4265_D321_39A4 } else { crc >> 1 };
            }
        }
    }
    crc
}

/// `<Recent>\CustomDestinations` of the dock's user.
fn dir() -> Option<PathBuf> {
    // SAFETY: valid known-folder id; the returned buffer is freed below.
    let p = unsafe { SHGetKnownFolderPath(&FOLDERID_Recent, KF_FLAG_DEFAULT, None) }.ok()?;
    // SAFETY: `p` is a NUL-terminated string owned by us until CoTaskMemFree.
    let recent = unsafe { p.to_string() }.unwrap_or_default();
    // SAFETY: `p` came from SHGetKnownFolderPath and is not used afterwards.
    unsafe { CoTaskMemFree(Some(p.0 as *const _)) };
    (!recent.is_empty()).then(|| Path::new(&recent).join("CustomDestinations"))
}

/// The jump list of the app with AppUserModelID `app_id` (when known) and exe `exe`, ready for the
/// menu: other categories first, tasks last, within `MAX_OTHER` / `MAX_TASKS`. Empty when the app
/// has none.
pub fn load(app_id: Option<&str>, exe: &str) -> Vec<Group> {
    let Some(dir) = dir() else { return Vec::new() };
    if let Some(id) = app_id.map(str::trim).filter(|s| !s.is_empty()) {
        let file = dir.join(format!("{:x}.customDestinations-ms", app_id_hash(id)));
        // The id is known: its file, or the app registered no jump list (no search by exe).
        return crate::datadir::read_file(&file, MAX_FILE, true).map(|b| tidy(parse(&b))).unwrap_or_default();
    }
    tidy(scan(&dir, exe))
}

/// The jump list in `dir` whose links all start `exe` (compared as `apps::key_from_exe_path`, so
/// an app that updates itself into versioned folders still matches).
fn scan(dir: &Path, exe: &str) -> Vec<Group> {
    if exe.is_empty() || exe.to_ascii_lowercase().starts_with("shell:") {
        return Vec::new();
    }
    let key = crate::apps::key_from_exe_path(exe);
    let files = std::fs::read_dir(dir).into_iter().flatten().flatten().filter(|e| {
        e.file_name().to_string_lossy().to_ascii_lowercase().ends_with(".customdestinations-ms")
            && e.metadata().is_ok_and(|m| m.is_file() && m.len() <= MAX_SCAN_FILE)
    });
    let mut read = 0usize;
    for e in files.take(MAX_SCAN) {
        let Ok(bytes) = crate::datadir::read_file(&e.path(), MAX_SCAN_FILE, true) else { continue };
        read += bytes.len();
        if read > MAX_SCAN_BYTES {
            break;
        }
        let groups = parse(&bytes);
        let mut targets = groups.iter().flat_map(|g| &g.entries).filter_map(|e| match e {
            Entry::Link { target, .. } => Some(target),
            Entry::Separator => None,
        });
        let Some(first) = targets.next() else { continue };
        if crate::apps::key_from_exe_path(first) == key && targets.all(|t| crate::apps::key_from_exe_path(t) == key) {
            return groups;
        }
    }
    Vec::new()
}

/// Order and limits of the menu: categories before the tasks (one group, whatever the file says),
/// no separator at an edge or doubled, no empty category.
// clippy: kept as is, no runtime change in 1.4.3 (ROADMAP 3.6); rewrite is a follow-up of the stream that owns this file
#[allow(clippy::single_element_loop)]
fn tidy(groups: Vec<Group>) -> Vec<Group> {
    let (tasks, mut others): (Vec<Group>, Vec<Group>) = groups.into_iter().partition(|g| g.name.is_empty());
    let mut left = MAX_OTHER;
    for g in &mut others {
        g.entries.retain(|e| matches!(e, Entry::Link { .. }));
        g.entries.truncate(left);
        left -= g.entries.len();
    }
    let all: Vec<Entry> = tasks.into_iter().flat_map(|g| g.entries).collect();
    for mut g in [Group { name: String::new(), entries: all }] {
        g.entries.dedup_by(|a, b| *a == Entry::Separator && *b == Entry::Separator);
        if g.entries.first() == Some(&Entry::Separator) {
            g.entries.remove(0);
        }
        g.entries.truncate(MAX_TASKS);
        if g.entries.last() == Some(&Entry::Separator) {
            g.entries.pop();
        }
        others.push(g);
    }
    others.retain(|g| !g.entries.is_empty());
    others
}

// ------------------------------------------------------------------ parser (pure)

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
    let units: Vec<u16> =
        b.chunks_exact(2).map(|c| u16::from_le_bytes([c[0], c[1]])).take_while(|&u| u != 0).collect();
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

/// The categories of a `.customDestinations-ms` file with the links that can be shown and started.
/// Never panics: a file that is cut short or malformed gives what was read before the damage.
pub(crate) fn parse(b: &[u8]) -> Vec<Group> {
    let mut out = Vec::new();
    let Some(count) = u32_at(b, 4) else { return out };
    let mut o = 12usize;
    for _ in 0..(count as usize).min(MAX_CATEGORIES) {
        match parse_category(b, &mut o) {
            Some(Some(g)) => out.push(g),
            Some(None) => {}
            None => break,
        }
    }
    out
}

/// One category at `*o`. `Some(None)` for one with nothing of ours (a known category: Recent or
/// Frequent, kept by Windows elsewhere); `None` when the data is not what is expected.
fn parse_category(b: &[u8], o: &mut usize) -> Option<Option<Group>> {
    let kind = u32_at(b, *o)?;
    *o += 4;
    let (name, count) = match kind {
        0 => {
            let n = usize::from(u16_at(b, *o)?);
            let name = utf16(span(b, *o + 2, 2 * n)?);
            *o += 2 + 2 * n;
            let count = u32_at(b, *o)?;
            *o += 4;
            (clean(&name, 40).unwrap_or_else(|| "...".to_string()), count)
        }
        1 => {
            u32_at(b, *o)?;
            *o += 4;
            (String::new(), 0)
        }
        2 => {
            let count = u32_at(b, *o)?;
            *o += 4;
            (String::new(), count)
        }
        _ => return None,
    };
    let mut entries = Vec::new();
    // `count` is untrusted, but every turn consumes at least a whole link header or stops.
    for _ in 0..count {
        if span(b, *o, 16)? != CLSID_LINK {
            return None;
        }
        *o += 16;
        if let Some(e) = parse_link(b, o)? {
            if entries.len() < MAX_LINKS {
                entries.push(e);
            }
        }
    }
    if span(b, *o, 4)? != FOOTER {
        return None;
    }
    *o += 4;
    Some((kind != 1).then_some(Group { name, entries }))
}

/// One serialized shell link ([MS-SHLLINK]) at `*o`, which is moved past it. `Some(None)` for a
/// link that is well formed but not shown (no title, no local file target, odd characters);
/// `None` when the data is not a shell link.
fn parse_link(b: &[u8], pos: &mut usize) -> Option<Option<Entry>> {
    let RawLink { info_path, name, args, title, separator, .. } = parse_link_raw(b, pos, false)?;
    if separator {
        return Some(Some(Entry::Separator));
    }
    let entry = (|| {
        let title = clean(title.as_deref().filter(|t| !t.trim().is_empty()).unwrap_or(&name), MAX_TITLE)?;
        let target = info_path.filter(|t| is_local_file_path(t))?;
        let args = args.trim().to_string();
        (args.len() <= MAX_ARGS && !args.chars().any(char::is_control)).then_some(Entry::Link { title, target, args })
    })();
    Some(entry)
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
/// environment block's target, left unexpanded (the caller expands it with `shell_thread::expand_env`
/// and checks the result with `is_local_file_path`), then the drive path of the id list
/// (`id_list_path`). An advertised shortcut is never resolved. Never panics.
pub(crate) fn parse_lnk(b: &[u8]) -> Result<LnkFacts, LnkMiss> {
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

/// `parse_lnk` of the file `path`, read through `datadir::read_file` (no reparse point, a single
/// link, at most `MAX_LNK_FILE` bytes).
pub(crate) fn read_lnk(path: &Path) -> Result<LnkFacts, LnkMiss> {
    let bytes = crate::datadir::read_file(path, MAX_LNK_FILE, true).map_err(|_| LnkMiss::Damaged)?;
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
pub(crate) fn localized_name_ref(ini: &[u8], file_name: &str) -> Option<(String, u16)> {
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

/// Title and separator flag from a serialized property store ([MS-PROPSTORE]).
// clippy: kept as is, no runtime change in 1.4.3 (ROADMAP 3.6); rewrite is a follow-up of the stream that owns this file
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

/// Text fit for a menu row: trimmed, control characters as spaces, at most `max` characters.
/// `None` when empty or a resource reference (`@dll,-id`), which is never resolved here.
fn clean(s: &str, max: usize) -> Option<String> {
    let s: String = s.trim().chars().take(max).map(|c| if c.is_control() { ' ' } else { c }).collect();
    let s = s.trim();
    (!s.is_empty() && !s.starts_with('@')).then(|| s.to_string())
}

/// `X:\...` without characters a file path cannot have.
pub(crate) fn is_local_file_path(p: &str) -> bool {
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
fn is_env_file_path(p: &str) -> bool {
    let Some((name, rest)) = p.strip_prefix('%').and_then(|r| r.split_once('%')) else { return false };
    !name.is_empty()
        && name.chars().all(|c| c.is_ascii_alphanumeric() || "_()".contains(c))
        && rest.starts_with('\\')
        && is_local_file_path(&format!("C:{rest}"))
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    fn wide(s: &str) -> Vec<u8> {
        s.encode_utf16().flat_map(u16::to_le_bytes).collect()
    }

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
    fn link(target: Option<&str>, name: Option<&str>, args: Option<&str>, title: Option<&str>, separator: bool) -> Vec<u8> {
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

    fn category(kind: u32, name: &str, links: &[Vec<u8>]) -> Vec<u8> {
        let mut out = kind.to_le_bytes().to_vec();
        match kind {
            0 => {
                out.extend((name.encode_utf16().count() as u16).to_le_bytes());
                out.extend(wide(name));
                out.extend((links.len() as u32).to_le_bytes());
            }
            1 => out.extend(2u32.to_le_bytes()),
            _ => out.extend((links.len() as u32).to_le_bytes()),
        }
        for l in links {
            out.extend(CLSID_LINK);
            out.extend(l);
        }
        out.extend(FOOTER);
        out
    }

    fn file(categories: &[Vec<u8>]) -> Vec<u8> {
        let mut out = 2u32.to_le_bytes().to_vec();
        out.extend((categories.len() as u32).to_le_bytes());
        out.extend(0u32.to_le_bytes());
        for c in categories {
            out.extend(c);
        }
        out
    }

    fn item(title: &str, target: &str, args: &str) -> Entry {
        Entry::Link { title: title.into(), target: target.into(), args: args.into() }
    }

    const STEAM: &str = r"C:\Program Files (x86)\Steam\steam.exe";

    fn sample() -> Vec<u8> {
        file(&[
            category(0, "Recenti", &[link(Some(STEAM), None, Some("steam://launch/1/Dialog"), Some("Deadlock"), false)]),
            category(1, "", &[]),
            category(
                2,
                "",
                &[
                    link(Some(STEAM), None, Some("steam://store"), Some("Negozio"), false),
                    link(None, None, None, None, true),
                    link(Some(STEAM), Some("Opens a window"), Some("-n"), None, false),
                ],
            ),
        ])
    }

    #[test]
    fn file_names_are_the_crc_of_the_app_id() {
        // Names of real files in `CustomDestinations` / `AutomaticDestinations`.
        assert_eq!(format!("{:x}", app_id_hash("Microsoft.Windows.Explorer")), "f01b4d95cf55d32a");
        assert_eq!(format!("{:x}", app_id_hash("Chrome")), "5d696d521de238c3");
        assert_eq!(format!("{:x}", app_id_hash("Valve.Steam.Client")), "d356105fac5527ef");
        assert_eq!(app_id_hash("chrome"), app_id_hash("CHROME"), "the id is hashed uppercase");
    }

    #[test]
    fn categories_links_and_separators() {
        let groups = parse(&sample());
        assert_eq!(groups.len(), 2, "the known category has nothing of ours");
        assert_eq!(groups[0], Group { name: "Recenti".into(), entries: vec![item("Deadlock", STEAM, "steam://launch/1/Dialog")] });
        assert_eq!(
            groups[1],
            Group {
                name: String::new(),
                entries: vec![item("Negozio", STEAM, "steam://store"), Entry::Separator, item("Opens a window", STEAM, "-n")],
            },
            "tasks: the title, or the description when there is none"
        );
    }

    #[test]
    fn links_that_cannot_be_shown_are_skipped() {
        let odd = [
            link(None, None, None, Some("No target"), false),
            link(Some(STEAM), None, None, None, false),
            link(Some(STEAM), None, None, Some("@shell32.dll,-21769"), false),
            link(Some(r"\\server\share\a.exe"), None, None, Some("UNC"), false),
            link(Some("steam.exe"), None, None, Some("Relative"), false),
            link(Some("C:\\a\"b.exe"), None, None, Some("Quote"), false),
            link(Some(STEAM), None, Some("a\nb"), Some("Line break in the arguments"), false),
            link(Some(STEAM), None, Some(&"x".repeat(MAX_ARGS + 1)), Some("Long arguments"), false),
            link(Some(STEAM), None, None, Some("  Kept \u{7} one  "), false),
        ];
        let groups = parse(&file(&[category(2, "", &odd)]));
        assert_eq!(groups, vec![Group { name: String::new(), entries: vec![item("Kept   one", STEAM, "")] }]);
    }

    #[test]
    fn damaged_files_never_panic() {
        let good = sample();
        // Cut anywhere: what was complete before the cut, never a panic.
        for n in 0..good.len() {
            assert!(parse(&good[..n]).len() <= 2);
        }
        // Any single byte changed (sizes, counts, offsets): no panic, no endless loop.
        for i in 0..good.len() {
            for v in [0x00, 0x7F, 0xFF] {
                let mut bad = good.clone();
                bad[i] = v;
                let _ = parse(&bad);
            }
        }
        // A count far above what the file holds.
        let mut huge = file(&[category(2, "", &[])]);
        huge[16..20].copy_from_slice(&u32::MAX.to_le_bytes());
        assert!(parse(&huge).is_empty());
        assert!(parse(&[]).is_empty() && parse(&[0xFF; 64]).is_empty());
    }

    #[test]
    fn menu_order_and_limits() {
        let many = |n: usize| (0..n).map(|i| item(&format!("t{i}"), STEAM, "")).collect::<Vec<_>>();
        let mut tasks = vec![Entry::Separator];
        tasks.extend(many(3));
        tasks.extend([Entry::Separator, Entry::Separator]);
        tasks.extend(many(20));
        let groups = tidy(vec![
            Group { name: String::new(), entries: tasks },
            Group { name: "A".into(), entries: many(4) },
            Group { name: "B".into(), entries: many(4) },
            Group { name: "C".into(), entries: many(4) },
        ]);
        assert_eq!(groups.iter().map(|g| g.name.as_str()).collect::<Vec<_>>(), ["A", "B", ""], "tasks last, empty ones dropped");
        assert_eq!((groups[0].entries.len(), groups[1].entries.len()), (4, 1), "MAX_OTHER in all");
        let t = &groups[2].entries;
        assert_eq!(t.len(), MAX_TASKS);
        assert_eq!(t.iter().filter(|e| **e == Entry::Separator).count(), 1, "no doubled or leading separator");
        assert!(tidy(vec![Group { name: String::new(), entries: vec![Entry::Separator] }]).is_empty());
        // Several task categories in one file are one group under the same limit.
        let split = tidy((0..16).map(|_| Group { name: String::new(), entries: many(9) }).collect());
        assert_eq!((split.len(), split[0].entries.len()), (1, MAX_TASKS));
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
        let expanded = crate::shell_thread::expand_env(&sys.target);
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
        // Jump lists keep their reading: the LinkInfo path, whatever the flag.
        let mut jump = link(Some(STEAM), None, None, Some("Store"), false);
        jump[21] |= 0x01;
        let groups = parse(&file(&[category(2, "", &[jump])]));
        assert_eq!(groups, vec![Group { name: String::new(), entries: vec![item("Store", STEAM, "")] }]);
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

    /// Reads the REAL jump lists of this machine (run by hand:
    /// `cargo test -- --ignored --nocapture real_jump_lists`).
    #[test]
    #[ignore]
    fn real_jump_lists() {
        let dir = dir().expect("Recent folder");
        let t = std::time::Instant::now();
        let none = scan(&dir, r"C:\no\such\app.exe");
        println!("scan of {} without a match: {} ms", dir.display(), t.elapsed().as_millis());
        assert!(none.is_empty());
        for (id, exe) in [(Some("Valve.Steam.Client"), ""), (None, STEAM), (Some("Microsoft.VisualStudioCode"), "")] {
            let t = std::time::Instant::now();
            let groups = load(id, exe);
            println!("{id:?} {exe:?}: {} ms", t.elapsed().as_millis());
            for g in &groups {
                println!("  [{}]", g.name);
                for e in &g.entries {
                    println!("    {e:?}");
                }
            }
        }
    }
}
