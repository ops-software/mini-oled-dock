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
    let mut o = *pos;
    span(b, o, 0x4C)?;
    if u32_at(b, o)? != 0x4C {
        return None;
    }
    let flags = u32_at(b, o + 20)?;
    o += 0x4C;
    // LinkTargetIDList: skipped (the target is taken from LinkInfo).
    if flags & 0x01 != 0 {
        let n = usize::from(u16_at(b, o)?);
        span(b, o, 2 + n)?;
        o += 2 + n;
    }
    let mut target = None;
    if flags & 0x02 != 0 {
        let n = u32_at(b, o)? as usize;
        if n < 0x1C {
            return None;
        }
        target = link_info_path(span(b, o, n)?);
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
    // ExtraData: blocks until a size below 4. The property store holds the title.
    let (mut title, mut separator) = (None, false);
    loop {
        let n = u32_at(b, o)? as usize;
        if n < 4 {
            o += 4;
            break;
        }
        let block = span(b, o, n)?;
        if n >= 8 && u32_at(block, 4)? == 0xA000_0009 {
            let _ = properties(&block[8..], &mut title, &mut separator);
        }
        o += n;
    }
    *pos = o;
    if separator {
        return Some(Some(Entry::Separator));
    }
    let entry = (|| {
        let title = clean(title.as_deref().filter(|t| !t.trim().is_empty()).unwrap_or(&name), MAX_TITLE)?;
        let target = target.filter(|t| is_local_file_path(t))?;
        let args = args.trim().to_string();
        (args.len() <= MAX_ARGS && !args.chars().any(char::is_control)).then_some(Entry::Link { title, target, args })
    })();
    Some(entry)
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
fn is_local_file_path(p: &str) -> bool {
    let b = p.as_bytes();
    b.len() > 3
        && b.len() <= 1024
        && b[0].is_ascii_alphabetic()
        && b[1] == b':'
        && b[2] == b'\\'
        && !p[2..].chars().any(|c| c.is_control() || "\"<>|?*:".contains(c))
}

#[cfg(test)]
mod tests {
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
