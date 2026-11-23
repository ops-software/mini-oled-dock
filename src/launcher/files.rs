//! Files from the Windows Search index, the pure half (plan L2 D1-D7, Appendix A). The helper child
//! (`dock.exe --launcher-files`, files_child.rs) runs the query; here are the scope clause and the words it sends to
//! Windows Search, the request line the dock writes to it, the frames it writes back and the strict reader the dock
//! decodes them with (`FrameReader`), the re-sort and the rows, when a query asks for files (`wanted`), the state rows
//! and the pump's policy (`Pump`: when to spawn, write, kill and post) over the caller's clock. Nothing here calls
//! Windows: the pump thread (launcher_files.rs) runs the policy's acts.

use super::aliases::is_hidden;
use super::model::{
    Batch, CommandId, FileClass, GlyphId, IconRef, Item, ItemKey, Kind, Mode, Payload, ProviderId, ProviderMask, Query,
    Score, Verb, VerbSet,
};
use super::providers::{self, KIND_FILE, KIND_FOLDER};
use super::rank;
use super::strings;
use core::cmp::Reverse;
use std::io::Read;

// ---------------------------------------------------------------- limits (Appendix A)

/// The typed text of a request: at most this many characters.
pub const MAX_TEXT_CHARS: usize = 256;
/// A request line over this many bytes (its newline included) ends the helper with exit code 2.
pub const MAX_LINE_BYTES: usize = 1_100;
/// The words sent to Windows Search: at most 8, each cut to 64 characters.
const MAX_WORDS: usize = 8;
const MAX_WORD_CHARS: usize = 64;
/// The first bytes the helper writes: protocol version 2 (a row's flags carry bit 1, `FLAG_DANGEROUS`). A dock of
/// another version refuses the header, so a helper the dock does not understand never names a row.
pub const HEADER: &[u8] = b"mini-oled-dock launcher-files 2\n";
/// Rows of one frame, the skipped ones included (the helper asks Windows for 50).
pub const MAX_ROWS: u32 = 50;
/// A frame's bytes, its tag included: never more by construction (50 rows of capped columns, `frame_cap_holds`).
#[cfg(test)]
const MAX_FRAME: usize = 256 * 1024;
/// Column caps: bytes of UTF-8, and UTF-16 units (SPEC 5.4.6: name 260, path 1,024).
const PATH_BYTES: (usize, usize) = (3, 3_072);
pub const PATH_UNITS: usize = 1_024;
const NAME_BYTES: (usize, usize) = (1, 780);
pub const NAME_UNITS: usize = 260;
const FOLDER_BYTES: (usize, usize) = (0, 780);
pub const FOLDER_UNITS: usize = 260;

const TAG_STATE: u32 = 1;
const TAG_ROWS: u32 = 2;
/// A row's flags: bit 0 a folder, bit 1 Windows names the type of its name dangerous; any other bit is refused.
const FLAG_FOLDER: u8 = 1;
const FLAG_DANGEROUS: u8 = 2;
/// The helper's states (the state frame).
pub const STATE_READY: u32 = 0;
pub const STATE_SEARCH_OFF: u32 = 1;
pub const STATE_SID_DIFFERS: u32 = 2;
pub const STATE_NO_INDEX: u32 = 3;
pub const STATE_NO_ROOTS: u32 = 4;
/// A rows frame's status: the query ran, or it failed (no rows).
pub const STATUS_OK: u32 = 0;
pub const STATUS_FAILED: u32 = 1;

// ---------------------------------------------------------------- the scope and the words

/// Why the roots could not make a scope clause.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Refusal {
    NoRoots,
    Empty,
    Control,
    /// Longer than a listed path may be: nothing under it could be shown.
    TooLong,
}

/// `SCOPE='file:<root>'` for each root, joined by OR: every `'` doubled (SQL string literal), a root with a control
/// character refused (plan L2-F1, from the probe's `scope_clause`).
pub fn scope_clause(roots: &[&str]) -> Result<String, Refusal> {
    if roots.is_empty() {
        return Err(Refusal::NoRoots);
    }
    let mut out = String::new();
    for (i, r) in roots.iter().enumerate() {
        if r.is_empty() {
            return Err(Refusal::Empty);
        }
        if r.chars().any(char::is_control) {
            return Err(Refusal::Control);
        }
        if r.encode_utf16().count() > PATH_UNITS {
            return Err(Refusal::TooLong);
        }
        if i > 0 {
            out.push_str(" OR ");
        }
        out.push_str("SCOPE='file:");
        for c in r.chars() {
            if c == '\'' {
                out.push('\'');
            }
            out.push(c);
        }
        out.push('\'');
    }
    Ok(out)
}

/// The words of a typed text as Windows Search gets them: at most 8, each cut to its first 64 characters; the rest
/// dropped.
pub fn words_of(text: &str) -> Vec<&str> {
    text.split_whitespace()
        .take(MAX_WORDS)
        .map(|w| match w.char_indices().nth(MAX_WORD_CHARS) {
            Some((at, _)) => w.get(..at).unwrap_or(w),
            None => w,
        })
        .collect()
}

// ---------------------------------------------------------------- the roots (SPEC 5.4.6)

/// `launcher.file_folders`: at most this many folders of the user's own join the roots; later entries are ignored.
pub const MAX_EXTRA_ROOTS: usize = 8;
/// A root's length in UTF-16 units: a folder deeper than MAX_PATH is no root.
pub const ROOT_UNITS: usize = 260;

/// The folders no root may be, lie in or hold, read from known folders whose paths only an administrator can change
/// (HKLM): the Windows folder, the Program Files folders and the profiles folder; and the user's own profile, the one
/// place inside the profiles folder a root may be.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Fences {
    pub windows: String,
    pub program_files: Vec<String>,
    pub profiles: String,
    pub profile: String,
}

/// `path` lies under `root`: the root (ASCII case-insensitive, a trailing separator ignored), a separator, then more.
pub fn under(path: &str, root: &str) -> bool {
    let root = root.strip_suffix('\\').unwrap_or(root).as_bytes();
    let p = path.as_bytes();
    !root.is_empty()
        && p.len() > root.len() + 1
        && p.get(..root.len()).is_some_and(|head| head.eq_ignore_ascii_case(root))
        && p.get(root.len()) == Some(&b'\\')
}

fn trim_sep(s: &str) -> &str {
    s.strip_suffix('\\').unwrap_or(s)
}

/// `a` is the folder `b` (ASCII case-insensitive, a trailing separator of either ignored) or lies under it.
fn within(a: &str, b: &str) -> bool {
    (!trim_sep(b).is_empty() && trim_sep(a).eq_ignore_ascii_case(trim_sep(b))) || under(a, b)
}

/// `path` as a root of the files helper, in the form roots are compared and quoted in (`\` for each `/`, no trailing
/// `\` but a drive root's), or None when it is no safe root (SPEC 5.4.6). A safe root is a drive path (`D:\...`) or a
/// UNC path (`\\server\share\...`) of at most 260 UTF-16 units in the form the files reader accepts (no device or
/// long-path prefix, no colon past the drive's, no segment of dots and spaces alone), with no control, bidi or format
/// character, no empty segment, no segment ending in a dot or a space (Windows drops them, so the text would not name
/// the folder Windows reads), none of `<>"|?*` and no administrative share (a share name ending in `$`); a drive root
/// (`D:\`) or a share root (`\\server\share`) only when `drive_root` (the user's own list); neither the Windows folder
/// nor a Program Files folder, nor inside or holding one; neither the profiles folder nor holding it nor holding the
/// user's profile, and inside the profiles folder only inside the user's profile, so no root reaches another account's
/// profile. A fence that is empty refuses every path.
pub fn root_of(path: &str, f: &Fences, drive_root: bool) -> Option<String> {
    if path.len() > 3 * ROOT_UNITS {
        return None;
    }
    let mut p = path.replace('/', "\\");
    if p.len() > 3 && p.ends_with('\\') {
        p.pop();
    }
    let unc = p.starts_with("\\\\");
    let rest = p.get(if unc { 2 } else { 3 }..).unwrap_or_default();
    let segs: Vec<&str> = rest.split('\\').filter(|_| !rest.is_empty()).collect();
    let seg_ok = |s: &&str| {
        !s.is_empty() && !s.ends_with('.') && !s.ends_with(' ') && !s.bytes().any(|c| b"<>\"|?*".contains(&c))
    };
    let form = p.encode_utf16().count() <= ROOT_UNITS
        && !p.chars().any(is_hidden)
        && path_form_ok(&p)
        && segs.iter().all(seg_ok)
        && (!unc || segs.get(1).is_some_and(|share| !share.ends_with('$')));
    let top = segs.len() == if unc { 2 } else { 0 };
    let apart = |fence: &str| !fence.is_empty() && !within(&p, fence) && !under(fence, &p);
    let fenced = f.program_files.iter().chain([&f.windows]).all(|d| apart(d))
        && !f.profile.is_empty()
        && !f.profiles.is_empty()
        && !within(&f.profiles, &p)
        && !under(&f.profile, &p)
        && (!under(&p, &f.profiles) || within(&p, &f.profile));
    (form && (drive_root || !top) && fenced).then_some(p)
}

/// Adds each of `folders` that `root_of` accepts to `roots`, unless a root already holds it; the roots it holds leave.
/// Returns how many it refused.
pub fn add_roots<'a>(
    roots: &mut Vec<String>,
    folders: impl IntoIterator<Item = &'a str>,
    f: &Fences,
    drive_root: bool,
) -> usize {
    let mut refused = 0;
    for folder in folders {
        match root_of(folder, f, drive_root) {
            None => refused += 1,
            Some(r) if roots.iter().any(|a| within(&r, a)) => {}
            Some(r) => {
                roots.retain(|a| !under(a, &r));
                roots.push(r);
            }
        }
    }
    refused
}

/// The folders of `launcher.file_folders` the dock hands the helper (which checks them again): the first 8 entries,
/// each as `root_of` accepts it with a drive root allowed, none twice nor inside another; and how many entries are
/// ignored (refused, or past the 8th). With no fences every entry is ignored.
pub fn extra_roots(list: &[String], f: Option<&Fences>) -> (Vec<String>, usize) {
    let mut roots = Vec::new();
    let Some(f) = f else { return (roots, list.len()) };
    let refused = add_roots(&mut roots, list.iter().take(MAX_EXTRA_ROOTS).map(String::as_str), f, true);
    (roots, refused + list.len().saturating_sub(MAX_EXTRA_ROOTS))
}

// ---------------------------------------------------------------- requests

/// One files request of the session (`Effect::RequestFiles`): the query's generation, its text as typed (without the
/// prefix, trimmed), how long a quiet keyboard waits before it is due and how many rows it may show.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Req {
    pub gen: u64,
    pub text: Box<str>,
    pub quiet_ms: u32,
    pub cap: u8,
}

/// A text the helper may get: 1-256 characters, none of them a control, bidi or format character, not blank. Allocates
/// nothing (the session asks it at every requery that wants files).
pub fn sendable(text: &str) -> bool {
    text_ok(text) && !text.trim().is_empty()
}

/// 1-256 characters, none of them a control, bidi or format character.
fn text_ok(text: &str) -> bool {
    (1..=MAX_TEXT_CHARS).contains(&text.chars().count()) && !text.chars().any(is_hidden)
}

/// The line the dock writes for `r`: `<gen> <text>\n`. None for a text the helper may not get (empty or blank, over
/// 256 characters, or holding a control, bidi or format character).
pub fn request_line(r: &Req) -> Option<String> {
    sendable(&r.text).then(|| format!("{} {}\n", r.gen, r.text))
}

/// Why the helper refused a request line (it exits with code 2).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ReqError {
    TooLong,
    NoNewline,
    NotUtf8,
    NoSpace,
    Gen,
    Text,
}

/// The helper's reading of one line, its newline included: the generation and the text.
pub fn parse_request_line(line: &[u8]) -> Result<(u64, &str), ReqError> {
    if line.len() > MAX_LINE_BYTES {
        return Err(ReqError::TooLong);
    }
    let body = line.strip_suffix(b"\n").ok_or(ReqError::NoNewline)?;
    let s = core::str::from_utf8(body).map_err(|_| ReqError::NotUtf8)?;
    let (gen, text) = s.split_once(' ').ok_or(ReqError::NoSpace)?;
    if gen.is_empty() || gen.len() > 20 || !gen.bytes().all(|b| b.is_ascii_digit()) {
        return Err(ReqError::Gen);
    }
    let gen = gen.parse::<u64>().map_err(|_| ReqError::Gen)?;
    if !text_ok(text) {
        return Err(ReqError::Text);
    }
    Ok((gen, text))
}

// ---------------------------------------------------------------- frames

/// One listed item as the index names it: never opened, stat-ed or parsed beyond the reader's checks (D6).
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct FileHit {
    pub path: Box<str>,
    pub name: Box<str>,
    /// The parent folder's display name (may be empty).
    pub folder: Box<str>,
    pub dir: bool,
    /// Windows names the type of the name's extension dangerous (its own check of a type, asked by the helper, which
    /// runs the shell's association code in its own process, files_child.rs): the row is an executable file whatever
    /// `file_class` says. The helper can only add to the list, never take a type off it.
    pub dangerous: bool,
}

#[derive(Clone, PartialEq, Eq, Debug)]
pub enum Frame {
    State(u32),
    Rows { gen: u64, status: u32, skipped: u32, hits: Vec<FileHit> },
}

/// Why the dock refused the helper's output: each check of Appendix A its own error. The first one kills the helper and
/// ends files for the open.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum FrameError {
    /// The header bytes differ, or the output ended inside them.
    Header,
    /// The output ended inside a frame, or before the state frame.
    Truncated,
    /// The pipe could not be read.
    Io,
    Tag,
    StateTwice,
    StateValue,
    RowsBeforeState,
    /// Anything after a state other than 0.
    AfterState,
    /// A rows frame of another generation than the request in flight (the pump's check).
    WrongGen,
    Status,
    TooManyRows,
    /// Status 1 with rows or skipped rows.
    FailedWithRows,
    Flags,
    PathLen,
    NameLen,
    FolderLen,
    Utf8,
    /// A control, bidi or format character.
    Hidden,
    /// Not a drive path or a UNC path, or a path with a device or long-path prefix.
    PathForm,
    /// The name is not the path's last component.
    NameMismatch,
    /// A frame the dock did not ask for: more frames than the state frame and one per request line written (the
    /// reader's count, so a flood is never queued).
    Unasked,
}

/// The state frame.
pub fn encode_state(state: u32, out: &mut Vec<u8>) {
    out.extend_from_slice(&TAG_STATE.to_le_bytes());
    out.extend_from_slice(&state.to_le_bytes());
}

/// A rows frame: status 1 carries no row; otherwise the hits `check_hit` accepts, while `skipped + n` stays within
/// 50, and the refused ones counted in `skipped` (never named).
pub fn encode_rows(gen: u64, status: u32, skipped: u32, hits: &[FileHit], out: &mut Vec<u8>) {
    let ok = |h: &&FileHit| check_hit(&h.path, &h.name, &h.folder).is_ok();
    let (skipped, n) = if status == STATUS_OK {
        let refused = u32::try_from(hits.iter().filter(|h| !ok(h)).count()).unwrap_or(MAX_ROWS);
        let skipped = skipped.saturating_add(refused).min(MAX_ROWS);
        let n = u32::try_from(hits.iter().filter(ok).count()).unwrap_or(MAX_ROWS).min(MAX_ROWS - skipped);
        (skipped, n)
    } else {
        (0, 0)
    };
    out.extend_from_slice(&TAG_ROWS.to_le_bytes());
    out.extend_from_slice(&gen.to_le_bytes());
    out.extend_from_slice(&status.to_le_bytes());
    out.extend_from_slice(&skipped.to_le_bytes());
    out.extend_from_slice(&n.to_le_bytes());
    for h in hits.iter().filter(ok).take(n as usize) {
        out.push(if h.dir { FLAG_FOLDER } else { 0 } | if h.dangerous { FLAG_DANGEROUS } else { 0 });
        for s in [&h.path, &h.name, &h.folder] {
            // Within the caps `check_hit` holds them to, far below u16::MAX.
            out.extend_from_slice(&u16::try_from(s.len()).unwrap_or(0).to_le_bytes());
            out.extend_from_slice(s.as_bytes());
        }
    }
}

/// The checks of Appendix A on one row's strings (the reader's, and the helper's before it writes one).
pub fn check_hit(path: &str, name: &str, folder: &str) -> Result<(), FrameError> {
    let fits = |s: &str, (lo, hi): (usize, usize), units: usize| {
        (lo..=hi).contains(&s.len()) && s.encode_utf16().count() <= units
    };
    if !fits(path, PATH_BYTES, PATH_UNITS) {
        return Err(FrameError::PathLen);
    }
    if !fits(name, NAME_BYTES, NAME_UNITS) {
        return Err(FrameError::NameLen);
    }
    if !fits(folder, FOLDER_BYTES, FOLDER_UNITS) {
        return Err(FrameError::FolderLen);
    }
    if [path, name, folder].iter().any(|s| s.chars().any(is_hidden)) {
        return Err(FrameError::Hidden);
    }
    if !path_form_ok(path) {
        return Err(FrameError::PathForm);
    }
    let p = path.strip_suffix('\\').unwrap_or(path);
    if !p.rsplit('\\').next().is_some_and(|last| last.eq_ignore_ascii_case(name)) {
        return Err(FrameError::NameMismatch);
    }
    Ok(())
}

/// A drive path (`C:\...`) or a UNC path (`\\server\...`); never one with a device or long-path prefix, nor a colon
/// past a drive's: no NTFS or SMB name holds one, and `C:\u\tool.exe::$DATA` (a stream) would hide the extension.
/// Nor a segment of dots and spaces alone (`.`, `..`, `...`, `. `): Windows Search lists none, and Windows' path
/// normalization would resolve it, so the name, the helper's roots fence and type check and the class would no longer
/// read the path that Open acts on.
fn path_form_ok(p: &str) -> bool {
    let b = p.as_bytes();
    let drive = b.first().is_some_and(u8::is_ascii_alphabetic) && b.get(1) == Some(&b':') && b.get(2) == Some(&b'\\');
    let unc = b.starts_with(b"\\\\") && b.get(2).is_some_and(|c| !matches!(c, b'\\' | b'/' | b'?' | b'.'));
    let dots = p.split(['\\', '/']).any(|s| !s.is_empty() && s.bytes().all(|c| c == b'.' || c == b' '));
    (drive || unc) && !dots && !b.get(if drive { 2 } else { 0 }..).unwrap_or_default().contains(&b':')
}

/// How far a read filled its buffer.
enum Fill {
    Empty,
    Partial,
    Full,
}

fn fill(r: &mut impl Read, buf: &mut [u8]) -> Result<Fill, FrameError> {
    let mut got = 0;
    while got < buf.len() {
        let Some(rest) = buf.get_mut(got..) else { break };
        match r.read(rest) {
            Ok(0) => break,
            Ok(k) => got += k,
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => {}
            Err(_) => return Err(FrameError::Io),
        }
    }
    Ok(match got {
        0 => Fill::Empty,
        n if n < buf.len() => Fill::Partial,
        _ => Fill::Full,
    })
}

/// The dock's decoder of the helper's output (Appendix A): the header once, then one frame per call. It holds the
/// checks across frames (the state first and once, rows only after state 0, nothing after another state); the pump
/// checks the generation. No input panics it, and nothing it allocates is sized by a field it has not checked.
#[derive(Default, Debug)]
pub struct FrameReader {
    header: bool,
    state: Option<u32>,
}

impl FrameReader {
    /// The next frame; Ok(None) when the output ended at a frame boundary after the state frame.
    pub fn next(&mut self, r: &mut impl Read) -> Result<Option<Frame>, FrameError> {
        if !self.header {
            let mut h = [0u8; HEADER.len()];
            if !matches!(fill(r, &mut h)?, Fill::Full) || h != HEADER {
                return Err(FrameError::Header);
            }
            self.header = true;
        }
        let mut tag = [0u8; 4];
        match fill(r, &mut tag)? {
            Fill::Empty if self.state.is_some() => return Ok(None),
            Fill::Empty | Fill::Partial => return Err(FrameError::Truncated),
            Fill::Full => {}
        }
        match (u32::from_le_bytes(tag), self.state) {
            (_, Some(s)) if s != STATE_READY => Err(FrameError::AfterState),
            (TAG_STATE, Some(_)) => Err(FrameError::StateTwice),
            (TAG_STATE, None) => {
                let s = read_u32(r)?;
                if s > STATE_NO_ROOTS {
                    return Err(FrameError::StateValue);
                }
                self.state = Some(s);
                Ok(Some(Frame::State(s)))
            }
            (TAG_ROWS, None) => Err(FrameError::RowsBeforeState),
            (TAG_ROWS, Some(_)) => read_rows(r).map(Some),
            _ => Err(FrameError::Tag),
        }
    }
}

/// Exactly `buf.len()` bytes of the frame.
fn take(r: &mut impl Read, buf: &mut [u8]) -> Result<(), FrameError> {
    match fill(r, buf)? {
        Fill::Full => Ok(()),
        Fill::Empty | Fill::Partial => Err(FrameError::Truncated),
    }
}

fn read_u32(r: &mut impl Read) -> Result<u32, FrameError> {
    let mut b = [0u8; 4];
    take(r, &mut b)?;
    Ok(u32::from_le_bytes(b))
}

/// One string column: its byte length checked against `(lo, hi)` before anything is read or allocated.
fn read_str(r: &mut impl Read, (lo, hi): (usize, usize), err: FrameError) -> Result<Box<str>, FrameError> {
    let mut l = [0u8; 2];
    take(r, &mut l)?;
    let len = usize::from(u16::from_le_bytes(l));
    if !(lo..=hi).contains(&len) {
        return Err(err);
    }
    let mut buf = vec![0u8; len];
    take(r, &mut buf)?;
    String::from_utf8(buf).map(String::into_boxed_str).map_err(|_| FrameError::Utf8)
}

fn read_rows(r: &mut impl Read) -> Result<Frame, FrameError> {
    let mut g = [0u8; 8];
    take(r, &mut g)?;
    let gen = u64::from_le_bytes(g);
    let status = read_u32(r)?;
    let skipped = read_u32(r)?;
    let n = read_u32(r)?;
    if status > STATUS_FAILED {
        return Err(FrameError::Status);
    }
    if u64::from(skipped) + u64::from(n) > u64::from(MAX_ROWS) {
        return Err(FrameError::TooManyRows);
    }
    if status == STATUS_FAILED && (skipped != 0 || n != 0) {
        return Err(FrameError::FailedWithRows);
    }
    let mut hits = Vec::with_capacity(n as usize);
    for _ in 0..n {
        let mut f = [0u8; 1];
        take(r, &mut f)?;
        let [flags] = f;
        if flags & !(FLAG_FOLDER | FLAG_DANGEROUS) != 0 {
            return Err(FrameError::Flags);
        }
        let path = read_str(r, PATH_BYTES, FrameError::PathLen)?;
        let name = read_str(r, NAME_BYTES, FrameError::NameLen)?;
        let folder = read_str(r, FOLDER_BYTES, FrameError::FolderLen)?;
        check_hit(&path, &name, &folder)?;
        let (dir, dangerous) = (flags & FLAG_FOLDER != 0, flags & FLAG_DANGEROUS != 0);
        hits.push(FileHit { path, name, folder, dir, dangerous });
    }
    Ok(Frame::Rows { gen, status, skipped, hits })
}

// ---------------------------------------------------------------- rows

/// How a name matches the first folded word: 0 it starts with it, 1 one of its words does, 2 Windows' own match.
fn match_of(folded_name: &str, first: &str) -> u8 {
    if first.is_empty() || folded_name.starts_with(first) {
        return 0;
    }
    let word_start = folded_name.char_indices().any(|(i, c)| {
        !c.is_alphanumeric() && folded_name.get(i + c.len_utf8()..).is_some_and(|rest| rest.starts_with(first))
    });
    if word_start {
        1
    } else {
        2
    }
}

fn folded(s: &str) -> String {
    let mut f = String::with_capacity(s.len());
    rank::fold(s, &mut f);
    f
}

/// The helper's rows in the launcher's order: names that start with the first word, then names with a word that
/// does, then the rest, each group in the helper's order (its rank); a path listed twice is kept once.
pub fn resort(hits: &mut Vec<FileHit>, folded_words: &[&str]) {
    let first = folded_words.first().copied().unwrap_or("");
    let mut keyed: Vec<(u8, FileHit)> = Vec::with_capacity(hits.len());
    for h in hits.drain(..) {
        if !keyed.iter().any(|(_, k)| k.path.eq_ignore_ascii_case(&h.path)) {
            keyed.push((match_of(&folded(&h.name), first), h));
        }
    }
    // At most 50 rows: the small stable sort (`tinysort`) is enough, and the standard one would add its code.
    crate::tinysort::sort_by_key(&mut keyed, |k| k.0);
    hits.extend(keyed.into_iter().map(|(_, h)| h));
}

/// The glyph of a listed item by its class (D6: a file row never asks for its own icon).
fn glyph_of(dir: bool, class: FileClass) -> GlyphId {
    match (dir, class) {
        (true, _) => GlyphId::Folder,
        (false, FileClass::Executable) => GlyphId::Executable,
        (false, FileClass::Image) => GlyphId::Image,
        (false, FileClass::Archive) => GlyphId::Archive,
        (false, FileClass::Document | FileClass::Other) => GlyphId::Document,
    }
}

/// The rows of a re-sorted batch, the first `cap`: a file or a folder by its path, its name and its folder, a glyph by
/// class, the file kinds' verbs (an executable has no Open while the Mark of the Web check is open), never destructive.
/// The class comes from the path's last name as Windows opens it (`check_hit` holds the name to that component and
/// refuses dot segments, so the name, the helper's type check and the class read one name; a trailing separator hides
/// no extension; `file_class` drops the trailing dots and spaces Windows drops) or, when the helper says Windows names
/// that type dangerous (`FileHit::dangerous`), is executable whatever the list says: the list is the floor and the
/// helper only adds. The helper's folder flag is not taken for an executable's name: that row is an executable file, so
/// nothing in the helper's output can give Open to what SPEC 5.4.6 holds back (a real folder so named keeps Open file
/// location and Copy).
pub fn items(hits: Vec<FileHit>, folded_words: &[&str], cap: u8) -> Vec<Item> {
    let first = folded_words.first().copied().unwrap_or("");
    let mut out = Vec::with_capacity(hits.len().min(usize::from(cap)));
    for (i, h) in hits.into_iter().take(usize::from(cap)).enumerate() {
        let tier = match match_of(&folded(&h.name), first) {
            0 => rank::T_PREFIX,
            1 => rank::T_WORD,
            _ => rank::T_SUBSTR,
        };
        // By the path, which Open acts on (check_hit refused its dot segments: its last name is the row's name).
        let class = if h.dangerous { FileClass::Executable } else { providers::file_class(&h.path) };
        let dir = h.dir && class != FileClass::Executable;
        let class = if dir { FileClass::Other } else { class };
        let (kind, rank_kind, verbs) = if dir {
            (Kind::Folder, KIND_FOLDER, providers::verbs(Kind::Folder, false, Some(class)))
        } else {
            (Kind::File, KIND_FILE, providers::file_verbs(&h.path, class))
        };
        let score = Score {
            tier,
            learned: Reverse(0),
            idle: true,
            kind: rank_kind,
            rank: u32::try_from(i).unwrap_or(u32::MAX),
            len: 0,
        };
        out.push(Item {
            key: ItemKey::of(ProviderId::Files, &h.path),
            from: ProviderId::Files,
            kind,
            title: h.name,
            detail: (!h.folder.is_empty()).then_some(h.folder),
            icon: IconRef::Glyph(glyph_of(dir, class)),
            score,
            verbs,
            payload: Payload::Path { path: h.path, dir, class },
            destructive: false,
        });
    }
    out
}

// ---------------------------------------------------------------- when files are asked (D4)

/// Mixed mode: from 3 characters, after 150 ms without a key, at most 3 rows.
pub const QUIET_MIXED_MS: u32 = 150;
const MIXED_CHARS: usize = 3;
const CAP_MIXED: u8 = 3;
/// "file " mode: from 1 character, after 50 ms, at most 20 rows.
pub const QUIET_FILE_MS: u32 = 50;
const CAP_FILE: u8 = 20;

/// Whether this query asks files, and if so its quiet time and row cap. Never while the provider is off, in another
/// provider's mode, or in Mixed mode when the synchronous rows hold an answer, a run line, a path or an address.
pub fn wanted(q: &Query, mask: ProviderMask, has_answer_or_implicit: bool) -> Option<(u32, u8)> {
    if !mask.has(ProviderId::Files) {
        return None;
    }
    let chars = q.folded.trim().chars().count();
    match q.mode {
        Mode::Mixed if !has_answer_or_implicit && chars >= MIXED_CHARS => Some((QUIET_MIXED_MS, CAP_MIXED)),
        Mode::Only(ProviderId::Files) if chars >= 1 => Some((QUIET_FILE_MS, CAP_FILE)),
        _ => None,
    }
}

// ---------------------------------------------------------------- the open's state

/// Why files are not answering in this open.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Why {
    /// The Windows Search service is not running.
    SearchOff,
    /// The dock's user is not the shell's (Administrator protection, SPEC 5.4.6).
    SidDiffers,
    /// No index, no roots, or no helper.
    Unavailable,
    /// The helper did not answer in time.
    Stalled,
    /// The helper's output failed the reader.
    Broken,
}

/// What the pump tells the launcher (`ShellEvent::LauncherFiles`).
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum FilesEvent {
    /// The rows of the newest request.
    Batch(Batch),
    /// The request of this generation is taking long: the busy mark.
    Slow(u64),
    /// The request of this generation was given up.
    Stalled(u64),
    /// Files are off for the rest of the open.
    Off(Why),
}

/// The key of the state row.
pub fn state_key() -> ItemKey {
    ItemKey::of(ProviderId::Files, "\u{0}state")
}

/// The honest row of "file " mode when files do not answer (never in Mixed mode): Windows Search off, with Indexing
/// Options on Enter; otherwise a row without an action.
pub fn state_row(why: Why, file_mode: bool) -> Option<Item> {
    if !file_mode {
        return None;
    }
    let (title, open) = match why {
        Why::SearchOff => (strings::files_search_off(), true),
        Why::Stalled => (strings::files_not_answering(), false),
        Why::SidDiffers => (strings::files_admin_protection(), false),
        Why::Unavailable | Why::Broken => (strings::files_unavailable(), false),
    };
    Some(Item {
        key: state_key(),
        from: ProviderId::Files,
        kind: if open { Kind::Command } else { Kind::Notice },
        title: title.into(),
        detail: open.then(|| strings::files_open_indexing().into()),
        icon: IconRef::Glyph(GlyphId::Info),
        score: Score { tier: 0, learned: Reverse(0), idle: false, kind: 0, rank: 0, len: 0 },
        verbs: if open { VerbSet::empty().with(Verb::Open) } else { VerbSet::empty() },
        payload: if open { Payload::Command(CommandId::IndexingOptions) } else { Payload::None },
        destructive: false,
    })
}

// ---------------------------------------------------------------- the pump's policy (D2, D3)

/// The pump's clocks in ms: `LIMITS` in the dock (the plan's values); the thread's own test runs them faster.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Limits {
    /// The busy mark of the newest request still in flight.
    pub slow_ms: u64,
    /// A request in flight this long is killed when a newer one waits (not counted).
    pub stale_kill_ms: u64,
    /// A request in flight this long is killed and counted.
    pub deadline_ms: u64,
    /// A helper whose state frame has not come this long after its spawn is killed and counted.
    pub spawn_deadline_ms: u64,
}

pub const LIMITS: Limits = Limits { slow_ms: 300, stale_kill_ms: 300, deadline_ms: 1_000, spawn_deadline_ms: 2_000 };
/// Counted kills in one open that turn files off for the rest of it.
pub const MAX_KILLS: u8 = 3;
/// What a helper whose stdin was closed gets to end by itself before it is killed.
pub const CLOSE_GRACE_MS: u64 = 200;

/// Why the pump kills the live helper.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum KillWhy {
    /// A newer request waits behind a slow one: the policy working, not counted.
    Stale,
    /// The request in flight passed its deadline (counted).
    Deadline,
    /// The state frame did not come (counted).
    SpawnDeadline,
    /// Its output failed the reader: files are off for the open.
    Broken(FrameError),
}

/// What the pump thread does, in order.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum Act {
    /// Start helper number n (it is live from now on; `Pump::spawned` says how the start went).
    Spawn(u32),
    /// The request's line to the live helper's stdin.
    Write(Req),
    /// Kill the live helper now and reap it.
    Kill(KillWhy),
    /// The live helper is done (its state was not 0, or its output ended with nothing in flight): close its stdin, give
    /// it `CLOSE_GRACE_MS`, kill it if it is still there and reap it.
    Retire,
    /// The live helper's output ended with a request in flight (it died on it): retired as with `Retire`, its exit
    /// code logged once per process.
    Died,
    /// To the launcher.
    Post(FilesEvent),
    /// The open is over: the live helper is retired and the thread ends.
    End,
}

#[derive(Debug)]
struct Flight {
    req: Req,
    written_at: u64,
    slow: bool,
}

#[derive(Debug)]
struct Helper {
    spawn: u32,
    spawned_at: u64,
    /// Its state frame said 0.
    ready: bool,
    flight: Option<Flight>,
}

/// What the live helper's clocks call for.
enum Due {
    Nothing,
    SpawnLate,
    Deadline(u64),
    Stale,
    Slow(u64),
}

fn earliest(at: Option<u64>, t: u64) -> Option<u64> {
    Some(at.map_or(t, |a| a.min(t)))
}

/// The policy of D2 and D3 for one open, over the caller's clock (ms): one helper at a time, spawned only when a
/// request is due; one request in flight, written only after state 0; the newest request waiting its quiet time; the
/// kills; files off after the third counted kill or death, a failed start or a broken output. Anything from a helper
/// it killed or retired is ignored. Pure: it returns acts and never waits. A list's posts come before its kill or
/// retire, whose wait on the pump thread can last 2 s: the launcher hears of a stall at once.
#[derive(Debug)]
pub struct Pump {
    limits: Limits,
    /// The newest request not yet written, and since when it waits.
    waiting: Option<(Req, u64)>,
    /// The newest request's generation: rows of any other are dropped.
    newest: Option<u64>,
    helper: Option<Helper>,
    spawns: u32,
    kills: u8,
    off: bool,
    closed: bool,
}

impl Pump {
    pub fn new(limits: Limits) -> Pump {
        Pump { limits, waiting: None, newest: None, helper: None, spawns: 0, kills: 0, off: false, closed: false }
    }

    /// A requery's request: Some replaces the waiting one; None withdraws it (a request already written runs on, and its
    /// rows are dropped).
    pub fn request(&mut self, r: Option<Req>, now: u64) -> Vec<Act> {
        if self.off || self.closed {
            return Vec::new();
        }
        self.newest = r.as_ref().map(|r| r.gen);
        self.waiting = r.map(|r| (r, now));
        self.tick(now)
    }

    /// How the start of helper `spawn` went: one that could not start leaves files off for the open (D5 b).
    pub fn spawned(&mut self, spawn: u32, ok: bool, now: u64) -> Vec<Act> {
        match self.helper.as_mut().filter(|h| h.spawn == spawn) {
            Some(h) if ok => {
                h.spawned_at = now;
                Vec::new()
            }
            Some(_) => {
                self.helper = None;
                self.turn_off(Why::Unavailable, Vec::new())
            }
            None => Vec::new(),
        }
    }

    /// What the reader of helper `spawn` read.
    pub fn frame(&mut self, spawn: u32, f: Result<Option<Frame>, FrameError>, now: u64) -> Vec<Act> {
        if self.helper.as_ref().is_none_or(|h| h.spawn != spawn) {
            return Vec::new();
        }
        match f {
            Ok(None) => self.exited(spawn),
            Err(e) => self.broken(e),
            Ok(Some(Frame::State(s))) => self.state(s, now),
            Ok(Some(Frame::Rows { gen, status, hits, .. })) => self.rows(gen, status, hits, now),
        }
    }

    /// The live helper's output ended at a frame boundary: it exited, and the next due request starts another. A
    /// healthy helper ends only when the dock closes its stdin, which the pump never waits for, so an end with a
    /// request in flight is a death (a crash, a line it refused): that request is given up and the end counts as a
    /// deadline kill does. An end with nothing in flight is not counted.
    pub fn exited(&mut self, spawn: u32) -> Vec<Act> {
        let Some(h) = self.helper.take_if(|h| h.spawn == spawn) else { return Vec::new() };
        match h.flight {
            Some(f) => {
                let gen = Some(f.req.gen).filter(|g| Some(*g) == self.newest);
                self.counted(gen, Act::Died, Vec::new())
            }
            None => vec![Act::Retire],
        }
    }

    /// The open is over.
    pub fn close(&mut self) -> Vec<Act> {
        self.closed = true;
        self.waiting = None;
        self.newest = None;
        self.helper = None;
        vec![Act::End]
    }

    /// Files are off for the rest of the open.
    #[cfg(test)]
    pub fn is_off(&self) -> bool {
        self.off
    }

    /// The clocks: the live helper's kills and busy mark, then a due request is written or a helper spawned for it.
    pub fn tick(&mut self, now: u64) -> Vec<Act> {
        let mut acts = Vec::new();
        if self.off || self.closed {
            return acts;
        }
        let lim = self.limits;
        let due = match &self.helper {
            Some(h) if !h.ready && now.saturating_sub(h.spawned_at) >= lim.spawn_deadline_ms => Due::SpawnLate,
            Some(Helper { ready: true, flight: Some(f), .. }) => {
                let age = now.saturating_sub(f.written_at);
                if age >= lim.deadline_ms {
                    Due::Deadline(f.req.gen)
                } else if age >= lim.stale_kill_ms && self.waiting.is_some() {
                    Due::Stale
                } else if age >= lim.slow_ms && !f.slow && self.newest == Some(f.req.gen) {
                    Due::Slow(f.req.gen)
                } else {
                    Due::Nothing
                }
            }
            _ => Due::Nothing,
        };
        match due {
            Due::Nothing => {}
            Due::Slow(gen) => {
                if let Some(f) = self.helper.as_mut().and_then(|h| h.flight.as_mut()) {
                    f.slow = true;
                }
                acts.push(Act::Post(FilesEvent::Slow(gen)));
            }
            Due::Stale => {
                self.helper = None;
                acts.push(Act::Kill(KillWhy::Stale));
            }
            Due::SpawnLate | Due::Deadline(_) => {
                self.helper = None;
                let (why, gen) = match due {
                    Due::Deadline(gen) => (KillWhy::Deadline, Some(gen).filter(|g| Some(*g) == self.newest)),
                    _ => (KillWhy::SpawnDeadline, self.newest),
                };
                acts = self.counted(gen, Act::Kill(why), acts);
            }
        }
        let is_due =
            self.waiting.as_ref().is_some_and(|(r, since)| now.saturating_sub(*since) >= u64::from(r.quiet_ms));
        if is_due {
            match self.helper.as_mut() {
                None => {
                    self.spawns = self.spawns.saturating_add(1);
                    self.helper = Some(Helper { spawn: self.spawns, spawned_at: now, ready: false, flight: None });
                    acts.push(Act::Spawn(self.spawns));
                }
                Some(h) if h.ready && h.flight.is_none() => {
                    if let Some((req, _)) = self.waiting.take() {
                        h.flight = Some(Flight { req: req.clone(), written_at: now, slow: false });
                        acts.push(Act::Write(req));
                    }
                }
                Some(_) => {}
            }
        }
        acts
    }

    /// How long until `tick` has something to do; None: nothing until the next input.
    pub fn next_wake(&self, now: u64) -> Option<u64> {
        if self.off || self.closed {
            return None;
        }
        let lim = self.limits;
        let mut at = None;
        let free = match &self.helper {
            None => true,
            Some(h) if !h.ready => {
                at = earliest(at, h.spawned_at.saturating_add(lim.spawn_deadline_ms));
                false
            }
            Some(Helper { flight: Some(f), .. }) => {
                at = earliest(at, f.written_at.saturating_add(lim.deadline_ms));
                if self.waiting.is_some() {
                    at = earliest(at, f.written_at.saturating_add(lim.stale_kill_ms));
                }
                if !f.slow && self.newest == Some(f.req.gen) {
                    at = earliest(at, f.written_at.saturating_add(lim.slow_ms));
                }
                false
            }
            Some(_) => true,
        };
        if let Some((r, since)) = self.waiting.as_ref().filter(|_| free) {
            at = earliest(at, since.saturating_add(u64::from(r.quiet_ms)));
        }
        at.map(|t| t.saturating_sub(now))
    }

    /// A counted end of the live helper (a deadline or spawn-deadline kill, a death): the stall of `gen` posted, files
    /// off at the `MAX_KILLS`th, then `end`.
    fn counted(&mut self, gen: Option<u64>, end: Act, mut acts: Vec<Act>) -> Vec<Act> {
        if let Some(gen) = gen {
            acts.push(Act::Post(FilesEvent::Stalled(gen)));
        }
        self.kills = self.kills.saturating_add(1);
        if self.kills >= MAX_KILLS {
            acts = self.turn_off(Why::Stalled, acts);
        }
        acts.push(end);
        acts
    }

    fn turn_off(&mut self, why: Why, mut acts: Vec<Act>) -> Vec<Act> {
        self.off = true;
        self.waiting = None;
        self.newest = None;
        acts.push(Act::Post(FilesEvent::Off(why)));
        acts
    }

    fn broken(&mut self, e: FrameError) -> Vec<Act> {
        self.helper = None;
        let mut acts = self.turn_off(Why::Broken, Vec::new());
        acts.push(Act::Kill(KillWhy::Broken(e)));
        acts
    }

    fn state(&mut self, s: u32, now: u64) -> Vec<Act> {
        if s == STATE_READY {
            if let Some(h) = self.helper.as_mut() {
                h.ready = true;
            }
            return self.tick(now);
        }
        self.helper = None;
        let why = match s {
            STATE_SEARCH_OFF => Why::SearchOff,
            STATE_SID_DIFFERS => Why::SidDiffers,
            _ => Why::Unavailable,
        };
        let mut acts = self.turn_off(why, Vec::new());
        acts.push(Act::Retire);
        acts
    }

    /// The rows of the request in flight: posted only while it is the newest request, built here (re-sorted, capped),
    /// never on the UI thread; a failed query posts an empty batch, so the busy mark goes.
    fn rows(&mut self, gen: u64, status: u32, hits: Vec<FileHit>, now: u64) -> Vec<Act> {
        let flight = self.helper.as_mut().and_then(|h| h.flight.take());
        let Some(flight) = flight.filter(|f| f.req.gen == gen) else { return self.broken(FrameError::WrongGen) };
        let mut acts = Vec::new();
        if self.newest == Some(gen) {
            let text = folded(&flight.req.text);
            let words = words_of(&text);
            let mut hits = if status == STATUS_OK { hits } else { Vec::new() };
            resort(&mut hits, &words);
            let items = items(hits, &words, flight.req.cap);
            acts.push(Act::Post(FilesEvent::Batch(Batch { gen, from: ProviderId::Files, items, done: true })));
        }
        acts.extend(self.tick(now));
        acts
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::indexing_slicing)]
mod tests {
    use super::*;
    use crate::launcher::model::NumberStyle;
    use crate::launcher::query::{parse_query, PrefixTable};
    use crate::launcher::testkit::XorShift;

    const NUM: NumberStyle = NumberStyle { decimal: ',', group: '.' };

    fn hit(path: &str, dir: bool) -> FileHit {
        let p = path.strip_suffix('\\').unwrap_or(path);
        let name = p.rsplit('\\').next().unwrap();
        let folder = p.rsplit('\\').nth(1).unwrap_or("");
        FileHit { path: path.into(), name: name.into(), folder: folder.into(), dir, dangerous: false }
    }

    /// A hit whose type Windows names dangerous (the helper's flag bit 1).
    fn dangerous(path: &str, dir: bool) -> FileHit {
        FileHit { dangerous: true, ..hit(path, dir) }
    }

    fn stream(frames: &[Vec<u8>]) -> Vec<u8> {
        let mut out = HEADER.to_vec();
        for f in frames {
            out.extend_from_slice(f);
        }
        out
    }

    fn state(s: u32) -> Vec<u8> {
        let mut v = Vec::new();
        encode_state(s, &mut v);
        v
    }

    fn rows(gen: u64, hits: &[FileHit]) -> Vec<u8> {
        let mut v = Vec::new();
        encode_rows(gen, STATUS_OK, 0, hits, &mut v);
        v
    }

    /// One row's flags, path, name and folder as raw bytes, so every field can be wrong.
    type RawRow<'a> = (u8, &'a [u8], &'a [u8], &'a [u8]);

    /// A rows frame built by hand.
    fn raw_rows(gen: u64, status: u32, skipped: u32, n: u32, rows: &[RawRow]) -> Vec<u8> {
        let mut v = Vec::new();
        v.extend_from_slice(&TAG_ROWS.to_le_bytes());
        v.extend_from_slice(&gen.to_le_bytes());
        for x in [status, skipped, n] {
            v.extend_from_slice(&x.to_le_bytes());
        }
        for (flags, path, name, folder) in rows {
            v.push(*flags);
            for s in [path, name, folder] {
                v.extend_from_slice(&(s.len() as u16).to_le_bytes());
                v.extend_from_slice(s);
            }
        }
        v
    }

    fn read_all(bytes: &[u8]) -> (Vec<Frame>, Result<(), FrameError>) {
        let mut r = FrameReader::default();
        let mut cur = std::io::Cursor::new(bytes);
        let mut out = Vec::new();
        for _ in 0..200 {
            match r.next(&mut cur) {
                Ok(Some(f)) => out.push(f),
                Ok(None) => return (out, Ok(())),
                Err(e) => return (out, Err(e)),
            }
        }
        panic!("the reader never ended");
    }

    fn first_error(bytes: &[u8]) -> Option<FrameError> {
        read_all(bytes).1.err()
    }

    #[test]
    fn scope_clause_quoting() {
        assert_eq!(scope_clause(&["C:\\Users\\a"]).unwrap(), "SCOPE='file:C:\\Users\\a'");
        assert_eq!(scope_clause(&["C:\\O'Neil"]).unwrap(), "SCOPE='file:C:\\O''Neil'");
        assert_eq!(scope_clause(&["C:\\a", "C:\\b"]).unwrap(), "SCOPE='file:C:\\a' OR SCOPE='file:C:\\b'");
        assert_eq!(scope_clause(&["x' OR 1=1 --"]).unwrap(), "SCOPE='file:x'' OR 1=1 --'", "the literal holds");
        assert_eq!(scope_clause(&["C:\\100%--"]).unwrap(), "SCOPE='file:C:\\100%--'");
        assert_eq!(scope_clause(&[]), Err(Refusal::NoRoots));
        assert_eq!(scope_clause(&["C:\\ok", ""]), Err(Refusal::Empty));
        for bad in ["C:\\a\nb", "C:\\a\u{0}", "C:\\a\u{7f}", "C:\\a\u{85}"] {
            assert_eq!(scope_clause(&["C:\\ok", bad]), Err(Refusal::Control), "{bad:?}");
        }
        // A long root is quoted whole; one no listed path could fit under is refused.
        let long = format!("C:\\{}'", "d".repeat(1_000));
        let c = scope_clause(&[&long]).unwrap();
        assert_eq!(c.len(), "SCOPE='file:'".len() + long.len() + 1);
        assert!(c.ends_with("d'''"));
        assert_eq!(scope_clause(&[&"e".repeat(1_025)]), Err(Refusal::TooLong));
    }

    fn fences() -> Fences {
        Fences {
            windows: "C:\\Windows".into(),
            program_files: vec!["C:\\Program Files".into(), "C:\\Program Files (x86)".into()],
            profiles: "C:\\Users".into(),
            profile: "C:\\Users\\a".into(),
        }
    }

    #[test]
    fn a_root_is_accepted_only_when_safe() {
        let f = fences();
        // Known folders moved to other drives or to a share are roots, in one form; so is a folder of the profile.
        for (path, root) in [
            ("D:\\Docs", "D:\\Docs"),
            ("E:\\Media\\Pictures\\", "E:\\Media\\Pictures"),
            ("d:/docs/2026", "d:\\docs\\2026"),
            ("\\\\nas\\share\\docs", "\\\\nas\\share\\docs"),
            ("//nas/share/docs/", "\\\\nas\\share\\docs"),
            ("C:\\Users\\a\\Documents", "C:\\Users\\a\\Documents"),
            ("c:\\users\\A", "c:\\users\\A"),
            ("C:\\Data", "C:\\Data"),
            ("C:\\Windowsx\\a", "C:\\Windowsx\\a"),
            ("C:\\Program Files Data", "C:\\Program Files Data"),
            ("D:\\O'Brien", "D:\\O'Brien"),
        ] {
            assert_eq!(root_of(path, &f, false).as_deref(), Some(root), "{path:?}");
            assert_eq!(root_of(path, &f, true).as_deref(), Some(root), "{path:?}");
        }
        // A drive root or a share root: only in the user's own list.
        for (top, root) in [
            ("D:\\", "D:\\"),
            ("e:/", "e:\\"),
            ("\\\\nas\\share", "\\\\nas\\share"),
            ("\\\\nas\\share\\", "\\\\nas\\share"),
        ] {
            assert_eq!(root_of(top, &f, false), None, "{top:?}");
            assert_eq!(root_of(top, &f, true).as_deref(), Some(root), "{top:?}");
        }
        // Never: the drive that holds Windows and the profiles, the system trees, the profiles folder, another
        // account's profile, the public one, relative and odd paths, a device or long-path prefix, an administrative
        // share, a hidden character, an over-long path.
        let long = format!("D:\\{}", "d".repeat(258));
        for bad in [
            "C:\\",
            "c:/",
            "C:\\Windows",
            "c:\\windows\\Temp",
            "C:\\Program Files",
            "C:\\Program Files (x86)\\Tool",
            "C:\\Users",
            "C:\\Users\\",
            "c:\\users\\b",
            "C:\\Users\\b\\Documents",
            "C:\\Users\\ab",
            "C:\\Users\\Public\\Documents",
            "",
            "Docs",
            "Docs\\a",
            "\\Docs",
            "C:",
            "C:Docs",
            "D:\\a\\..\\b",
            "D:\\a\\.",
            "D:\\a.",
            "D:\\a ",
            "D:\\a\\\\b",
            "D:\\a\\\\",
            "D:\\a:b",
            "D:\\a*",
            "D:\\a?",
            "D:\\a|b",
            "D:\\a\"b",
            "D:\\a<b",
            "\\\\?\\D:\\Docs",
            "\\\\.\\D:",
            "\\\\nas",
            "\\\\nas\\c$\\Users",
            "\\\\localhost\\C$",
            "\\\\nas\\share$",
            "\\\\\\nas\\share",
            "D:\\a\u{7}",
            "D:\\a\u{202e}b",
            "D:\\a\nb",
            &long,
        ] {
            assert_eq!(root_of(bad, &f, false), None, "{bad:?}");
            assert_eq!(root_of(bad, &f, true), None, "{bad:?}");
        }
        let edge = format!("D:\\{}", "d".repeat(257));
        assert_eq!(root_of(&edge, &f, false).as_deref(), Some(&*edge), "260 units");
        // A profile outside the profiles folder: no root holds it, so none holds its neighbours.
        let moved = Fences { profile: "D:\\Profiles\\a".into(), ..fences() };
        for bad in ["D:\\Profiles", "D:\\"] {
            assert_eq!(root_of(bad, &moved, true), None, "{bad:?}");
        }
        assert!(root_of("D:\\Profiles\\a\\Docs", &moved, false).is_some());
        // A fence missing refuses everything.
        for missing in [
            Fences { windows: String::new(), ..fences() },
            Fences { profiles: String::new(), ..fences() },
            Fences { profile: String::new(), ..fences() },
            Fences { program_files: vec![String::new()], ..fences() },
            Fences::default(),
        ] {
            assert_eq!(root_of("D:\\Docs", &missing, true), None, "{missing:?}");
        }
    }

    #[test]
    fn a_root_is_added_once() {
        let f = fences();
        let common = "C:\\ProgramData\\Microsoft\\Windows\\Start Menu\\Programs";
        let mut roots = vec!["C:\\Users\\a".to_string(), common.into()];
        let folders =
            ["C:\\Users\\a\\Desktop", "D:\\Docs", "d:\\docs\\", "D:\\Docs\\Work", "C:\\Users\\b", "E:\\Pictures"];
        assert_eq!(add_roots(&mut roots, folders, &f, false), 1);
        assert_eq!(roots, ["C:\\Users\\a", common, "D:\\Docs", "E:\\Pictures"]);
        // A wider root takes the place of those it holds.
        assert_eq!(add_roots(&mut roots, ["E:\\", "D:\\"], &f, true), 0);
        assert_eq!(roots, ["C:\\Users\\a", common, "E:\\", "D:\\"]);
    }

    #[test]
    fn file_folders_are_capped_and_counted() {
        let f = fences();
        let list: Vec<String> = [
            "D:\\",
            "C:\\Windows",
            "relative",
            "E:\\Archive",
            "e:\\archive\\2025",
            "\\\\nas\\share",
            "C:\\Users\\b",
            "F:\\A",
            "G:\\B",
            "H:\\C",
        ]
        .map(String::from)
        .to_vec();
        let (roots, ignored) = extra_roots(&list, Some(&f));
        assert_eq!(roots, ["D:\\", "E:\\Archive", "\\\\nas\\share", "F:\\A"]);
        assert_eq!(ignored, 3 + 2, "three refused, two past the 8th; a folder inside another is no error");
        assert_eq!(extra_roots(&list, None), (Vec::new(), 10));
        assert_eq!(extra_roots(&[], Some(&f)), (Vec::new(), 0));
    }

    #[test]
    fn words_are_capped() {
        assert_eq!(words_of("  quokka   plan "), ["quokka", "plan"]);
        assert_eq!(words_of("a b c d e f g h i j"), ["a", "b", "c", "d", "e", "f", "g", "h"]);
        let w = "é".repeat(70);
        assert_eq!(words_of(&w), [&*"é".repeat(64)]);
        assert!(words_of("   ").is_empty());
        assert_eq!(words_of("ext:pdf report"), ["ext:pdf", "report"], "AQS filters pass through");
    }

    #[test]
    fn request_line_rules() {
        let req = |text: &str| Req { gen: 42, text: text.into(), quiet_ms: 50, cap: 20 };
        assert_eq!(request_line(&req("quokka plan")).as_deref(), Some("42 quokka plan\n"));
        assert_eq!(request_line(&req(&"x".repeat(256))).map(|l| l.len()), Some(3 + 256 + 1));
        for bad in ["", "   ", "a\nb", "a\tb", "a\u{202E}b", "a\u{200B}b", "a\u{ad}b", "a\u{feff}"] {
            assert_eq!(request_line(&req(bad)), None, "{bad:?}");
        }
        assert_eq!(request_line(&req(&"x".repeat(257))), None);
        // The helper reads back exactly what the dock wrote.
        for text in ["quokka", "C:\\a b", "perché 2026", &"é".repeat(256)] {
            let line = request_line(&Req { gen: u64::MAX, ..req(text) }).unwrap();
            assert!(line.len() <= MAX_LINE_BYTES);
            assert_eq!(parse_request_line(line.as_bytes()), Ok((u64::MAX, text)));
        }
        let p = parse_request_line;
        assert_eq!(p(b"1 x"), Err(ReqError::NoNewline));
        assert_eq!(p(b"1 x\n\n"), Err(ReqError::Text));
        assert_eq!(p(b"1x\n"), Err(ReqError::NoSpace));
        assert_eq!(p(b" x\n"), Err(ReqError::Gen));
        assert_eq!(p(b"-1 x\n"), Err(ReqError::Gen));
        assert_eq!(p(b"+1 x\n"), Err(ReqError::Gen));
        assert_eq!(p(b"18446744073709551616 x\n"), Err(ReqError::Gen), "past u64");
        assert_eq!(p(b"1 \n"), Err(ReqError::Text));
        assert_eq!(p(b"1 a\rb\n"), Err(ReqError::Text));
        assert_eq!(p(b"1 \xff\n"), Err(ReqError::NotUtf8));
        assert_eq!(p(&[b'x'; 1_101]), Err(ReqError::TooLong));
        let mut long = b"1 ".to_vec();
        long.extend(std::iter::repeat_n(b'x', 257));
        long.push(b'\n');
        assert_eq!(p(&long), Err(ReqError::Text), "257 characters");
    }

    #[test]
    fn frames_round_trip() {
        let hits = vec![
            hit("C:\\Users\\a\\quokka plan.md", false),
            hit("C:\\Users\\a\\quokka-dir\\", true),
            hit("\\\\server\\share\\Perché.txt", false),
            dangerous("C:\\Users\\a\\quokka.qqq", false),
            dangerous("C:\\Users\\a\\quokka.qqd\\", true),
        ];
        let mut failed = Vec::new();
        encode_rows(8, STATUS_FAILED, 0, &[], &mut failed);
        let bytes = stream(&[state(0), rows(7, &hits), failed]);
        let (frames, end) = read_all(&bytes);
        assert_eq!(end, Ok(()));
        assert_eq!(frames[0], Frame::State(0));
        assert_eq!(frames[1], Frame::Rows { gen: 7, status: 0, skipped: 0, hits: hits.clone() });
        assert_eq!(frames[2], Frame::Rows { gen: 8, status: 1, skipped: 0, hits: Vec::new() });
        // The encoder counts what it refuses and keeps skipped + n within 50.
        let mut many: Vec<FileHit> = (0..60).map(|i| hit(&format!("C:\\u\\f{i}.txt"), false)).collect();
        many[0].name = "other.txt".into();
        let mut v = Vec::new();
        encode_rows(9, STATUS_OK, 2, &many, &mut v);
        let (frames, _) = read_all(&stream(&[state(0), v]));
        let Frame::Rows { skipped, hits, .. } = &frames[1] else { panic!() };
        assert_eq!((*skipped, hits.len()), (3, 47));
        // A state other than 0 is the last frame.
        let (frames, end) = read_all(&stream(&[state(STATE_SEARCH_OFF)]));
        assert_eq!((frames, end), (vec![Frame::State(1)], Ok(())));
    }

    #[test]
    fn frame_cap_holds() {
        // The largest frame the caps allow: the tag, gen, status, skipped, n and 50 rows of the longest columns.
        let row = 1 + (2 + PATH_BYTES.1) + (2 + NAME_BYTES.1) + (2 + FOLDER_BYTES.1);
        assert!(4 + 8 + 4 + 4 + 4 + MAX_ROWS as usize * row <= MAX_FRAME);
    }

    #[test]
    fn frame_rejections() {
        let ok = |n: &[u8]| -> Vec<u8> { [b"C:\\u\\".as_slice(), n].concat() };
        let row = |path: &[u8], name: &[u8]| raw_rows(1, 0, 0, 1, &[(0, path, name, b"u")]);
        let after_state = |f: Vec<u8>| stream(&[state(0), f]);
        let cases: Vec<(&str, Vec<u8>, FrameError)> = vec![
            ("wrong header", b"mini-oled-dock launcher-files 3\n".to_vec(), FrameError::Header),
            ("version 1", b"mini-oled-dock launcher-files 1\n".to_vec(), FrameError::Header),
            ("short header", HEADER[..10].to_vec(), FrameError::Header),
            ("no state", HEADER.to_vec(), FrameError::Truncated),
            ("cut tag", [stream(&[state(0)]), vec![2, 0]].concat(), FrameError::Truncated),
            ("cut row", after_state(raw_rows(1, 0, 0, 2, &[(0, &ok(b"a"), b"a", b"u")])), FrameError::Truncated),
            ("tag", after_state(vec![3, 0, 0, 0]), FrameError::Tag),
            ("state twice", stream(&[state(0), state(0)]), FrameError::StateTwice),
            ("state value", stream(&[state(5)]), FrameError::StateValue),
            ("rows first", stream(&[rows(1, &[])]), FrameError::RowsBeforeState),
            ("after state 1", stream(&[state(1), rows(1, &[])]), FrameError::AfterState),
            ("status", after_state(raw_rows(1, 2, 0, 0, &[])), FrameError::Status),
            ("too many", after_state(raw_rows(1, 0, 40, 11, &[])), FrameError::TooManyRows),
            ("overflow", after_state(raw_rows(1, 0, u32::MAX, 2, &[])), FrameError::TooManyRows),
            ("failed with rows", after_state(raw_rows(1, 1, 1, 0, &[])), FrameError::FailedWithRows),
            ("flags", after_state(raw_rows(1, 0, 0, 1, &[(4, &ok(b"a"), b"a", b"u")])), FrameError::Flags),
            ("flags 7", after_state(raw_rows(1, 0, 0, 1, &[(7, &ok(b"a"), b"a", b"u")])), FrameError::Flags),
            ("flags 128", after_state(raw_rows(1, 0, 0, 1, &[(128, &ok(b"a"), b"a", b"u")])), FrameError::Flags),
            ("path short", after_state(row(b"C:", b"C:")), FrameError::PathLen),
            ("path bytes", after_state(row(&ok(&[b'a'; 3_068]), &[b'a'; 200])), FrameError::PathLen),
            ("path units", after_state(row(&ok(&[b'a'; 1_020]), &[b'a'; 200])), FrameError::PathLen),
            ("name empty", after_state(row(b"C:\\u\\", b"")), FrameError::NameLen),
            ("name units", after_state(row(&ok(&[b'n'; 261]), &[b'n'; 261])), FrameError::NameLen),
            ("folder", after_state(raw_rows(1, 0, 0, 1, &[(0, &ok(b"a"), b"a", &[b'f'; 781])])), FrameError::FolderLen),
            ("utf-8", after_state(row(&ok(b"\xff"), b"\xff")), FrameError::Utf8),
            ("bidi", after_state(row(&ok("a\u{202E}b".as_bytes()), "a\u{202E}b".as_bytes())), FrameError::Hidden),
            ("control", after_state(row(&ok(b"a\x07"), b"a\x07")), FrameError::Hidden),
            ("relative", after_state(row(b"u\\a.txt", b"a.txt")), FrameError::PathForm),
            ("long prefix", after_state(row(b"\\\\?\\C:\\a.txt", b"a.txt")), FrameError::PathForm),
            ("device", after_state(row(b"\\\\.\\pipe\\a", b"a")), FrameError::PathForm),
            ("slashes", after_state(row(b"C:/u/a.txt", b"C:/u/a.txt")), FrameError::PathForm),
            ("stream", after_state(row(&ok(b"evil.exe::$DATA"), b"evil.exe::$DATA")), FrameError::PathForm),
            ("named stream", after_state(row(&ok(b"a.txt:evil.exe"), b"a.txt:evil.exe")), FrameError::PathForm),
            ("unc colon", after_state(row(b"\\\\srv\\s\\a:b.exe", b"a:b.exe")), FrameError::PathForm),
            ("drive twice", after_state(row(b"C:\\C:\\a.txt", b"a.txt")), FrameError::PathForm),
            ("dot segment", after_state(row(b"C:\\u\\..\\v\\a.txt", b"a.txt")), FrameError::PathForm),
            ("not the last part", after_state(row(&ok(b"a.txt"), b"b.txt")), FrameError::NameMismatch),
        ];
        let mut seen = Vec::new();
        for (what, bytes, want) in &cases {
            assert_eq!(first_error(bytes), Some(*want), "{what}");
            if !seen.contains(want) {
                seen.push(*want);
            }
        }
        // Every check but the pump's generation (`pump_checks_the_generation`) and the pipe's own failure is covered.
        assert_eq!(seen.len(), 18);
        // Bit 0 (folder) and bit 1 (Windows names the type dangerous) are the only flags.
        for flags in 0..=3u8 {
            let (frames, end) = read_all(&after_state(raw_rows(1, 0, 0, 1, &[(flags, &ok(b"a.q"), b"a.q", b"u")])));
            let Frame::Rows { hits, .. } = &frames[1] else { panic!("{flags}") };
            assert_eq!((end, hits[0].dir, hits[0].dangerous), (Ok(()), flags & 1 != 0, flags & 2 != 0), "{flags}");
        }
        // The name is compared ASCII case-insensitively, with one trailing separator ignored.
        assert_eq!(first_error(&after_state(row(&ok(b"Quokka.TXT"), b"quokka.txt"))), None);
        assert_eq!(first_error(&after_state(row(b"C:\\u\\dir\\", b"DIR"))), None);
    }

    #[test]
    fn frame_reader_never_panics() {
        let hits = [hit("C:\\Users\\a\\quokka plan.md", false), hit("C:\\Users\\a\\q-dir\\", true)];
        let valid = stream(&[state(0), rows(1, &hits), rows(2, &hits[..1])]);
        let mut rng = XorShift::new(0x000F_11E5);
        for i in 0..30_000u32 {
            let mut bytes = match i % 3 {
                0 => {
                    let len = (rng.next_u64() % 600) as usize;
                    let mut b: Vec<u8> = (0..len).map(|_| rng.next_u64() as u8).collect();
                    if rng.next_u64().is_multiple_of(2) {
                        b.splice(0..0, HEADER.iter().copied());
                    }
                    b
                }
                1 => valid[..(rng.next_u64() % valid.len() as u64) as usize].to_vec(),
                _ => {
                    let mut b = valid.clone();
                    let at = (rng.next_u64() % b.len() as u64) as usize;
                    b[at] ^= 1 << (rng.next_u64() % 8);
                    b
                }
            };
            bytes.truncate(4_096);
            let _ = read_all(&bytes);
        }
    }

    #[test]
    fn resort_name_prefix_first() {
        let mut hits = vec![
            hit("C:\\u\\my quokka.txt", false),
            hit("C:\\u\\aquokka.txt", false),
            hit("C:\\u\\Quokka plan.md", false),
            hit("C:\\u\\b\\my quokka.txt", false),
            hit("C:\\u\\quokka.zip", false),
            hit("C:\\U\\QUOKKA PLAN.MD", false),
        ];
        resort(&mut hits, &["quok"]);
        let paths: Vec<&str> = hits.iter().map(|h| &*h.path).collect();
        assert_eq!(
            paths,
            [
                "C:\\u\\Quokka plan.md",
                "C:\\u\\quokka.zip",
                "C:\\u\\my quokka.txt",
                "C:\\u\\b\\my quokka.txt",
                "C:\\u\\aquokka.txt"
            ],
            "name prefix, word prefix, the rest; the helper's order inside each; a path listed twice once"
        );
        let mut none = Vec::new();
        resort(&mut none, &[]);
        assert!(none.is_empty());
    }

    #[test]
    fn file_items_ask_no_icons() {
        let hits = vec![
            hit("C:\\u\\docs\\quokka.txt", false),
            hit("C:\\u\\quokka-dir\\", true),
            hit("C:\\u\\quokka.png", false),
            hit("C:\\u\\quokka.zip", false),
            hit("C:\\u\\quokka-tool.bat", false),
            hit("C:\\u\\the quokka.xyz", false),
            hit("C:\\u\\bquokka.pdf", false),
        ];
        let rows = items(hits.clone(), &["quokka"], 20);
        assert!(rows.iter().all(|r| matches!(r.icon, IconRef::Glyph(_))), "no row asks for an icon");
        let glyphs: Vec<IconRef> = rows.iter().map(|r| r.icon.clone()).collect();
        use GlyphId::*;
        let want = [Document, Folder, Image, Archive, Executable, Document, Document].map(IconRef::Glyph);
        assert_eq!(glyphs, want);
        assert_eq!((rows[0].kind, rows[1].kind), (Kind::File, Kind::Folder));
        assert_eq!(rows[0].detail.as_deref(), Some("docs"));
        assert_eq!(rows[0].key, ItemKey::of(ProviderId::Files, "C:\\u\\docs\\quokka.txt"));
        assert_eq!(
            rows[1].payload,
            Payload::Path { path: "C:\\u\\quokka-dir\\".into(), dir: true, class: FileClass::Other }
        );
        assert!(!rows[4].verbs.has(Verb::Open), "an executable is not opened while the Mark of the Web check is open");
        assert!(rows[4].verbs.has(Verb::Reveal) && rows[0].verbs.has(Verb::Open) && rows[1].verbs.has(Verb::Open));
        let tiers: Vec<u8> = rows.iter().map(|r| r.score.tier).collect();
        assert_eq!(tiers, [1, 1, 1, 1, 1, 2, 5]);
        assert_eq!(rows.iter().map(|r| r.score.rank).collect::<Vec<_>>(), (0..7).collect::<Vec<u32>>());
        assert!(rows.iter().all(|r| !r.destructive && r.from == ProviderId::Files));
        assert_eq!(items(hits, &["quokka"], 3).len(), 3);
        // The helper's folder flag on an executable's name, a trailing separator, the trailing dots and spaces
        // Windows drops and a class id for the extension (a folder so named too): executable files, no Open, never the
        // Mixed default.
        let odd = vec![
            hit("C:\\u\\Downloads\\setup.exe", true),
            hit("C:\\u\\quokka.cmd\\", false),
            hit("C:\\u\\quokka.exe. .", false),
            hit("C:\\u\\quokka.pdf.{3050F4D8-98B5-11CF-BB82-00AA00BDCE0B}", false),
            hit("C:\\u\\quokka.{20D04FE0-3AEA-1069-A2D8-08002B30309D}\\", true),
        ];
        for r in items(odd, &["quokka"], 20) {
            assert_eq!((r.kind, &r.icon), (Kind::File, &IconRef::Glyph(Executable)), "{}", r.title);
            assert!(matches!(r.payload, Payload::Path { dir: false, class: FileClass::Executable, .. }));
            assert!(!r.verbs.has(Verb::Open) && r.verbs.has(Verb::Reveal), "{}", r.title);
        }
    }

    /// Windows Search never lists a segment of dots and spaces alone; resolved, one would move the path away from the
    /// name the helper asks Windows about, from its roots fence and from the class.
    #[test]
    fn a_dot_segment_is_refused() {
        let refused = [
            ("C:\\u\\Downloads\\setup.exe\\.", "."),
            ("C:\\u\\Downloads\\setup.exe\\x\\..", ".."),
            ("C:\\Users\\me\\..\\..\\other\\report.pdf\\x\\..", ".."),
            ("C:\\Users\\me\\..\\other\\report.pdf", "report.pdf"),
            ("C:\\Users\\me\\x/../..\\other\\report.pdf", "report.pdf"),
            ("\\\\srv\\share\\.\\a.txt", "a.txt"),
            ("C:\\u\\...\\a.txt", "a.txt"),
            ("C:\\u\\. \\a.txt", "a.txt"),
        ];
        for (path, name) in refused {
            assert_eq!(check_hit(path, name, "x"), Err(FrameError::PathForm), "{path}");
        }
        let listed = [("C:\\u\\.config\\a.txt", "a.txt"), ("C:\\u\\a..b.txt", "a..b.txt"), ("C:\\u\\..x", "..x")];
        for (path, name) in listed {
            assert_eq!(check_hit(path, name, "x"), Ok(()), "{path}");
        }
    }

    #[test]
    fn a_type_windows_names_dangerous_is_an_executable_row() {
        let hits = vec![
            dangerous("C:\\u\\quokka.qqq", false),
            dangerous("C:\\u\\quokka.qqd\\", true),
            hit("C:\\u\\quokka.exe", false),
            hit("C:\\u\\quokka.qqq", false),
            dangerous("C:\\u\\quokka.txt", false),
        ];
        let rows = items(hits, &["quokka"], 20);
        // The helper's bit makes a file, whatever its extension's class and its folder flag, an executable file.
        for r in [&rows[0], &rows[1], &rows[4]] {
            assert_eq!((r.kind, &r.icon), (Kind::File, &IconRef::Glyph(GlyphId::Executable)), "{:?}", r.key);
            assert!(matches!(r.payload, Payload::Path { dir: false, class: FileClass::Executable, .. }));
            assert!(!r.verbs.has(Verb::Open) && r.verbs.has(Verb::Reveal) && r.verbs.has(Verb::Copy));
        }
        // The list is the floor: without the bit an executable stays one, and another type keeps Open.
        assert!(matches!(rows[2].payload, Payload::Path { class: FileClass::Executable, .. }));
        assert!(!rows[2].verbs.has(Verb::Open));
        assert!(matches!(rows[3].payload, Payload::Path { dir: false, class: FileClass::Other, .. }));
        assert!(rows[3].verbs.has(Verb::Open));
    }

    #[test]
    fn wanted_rules() {
        let t = PrefixTable::defaults();
        let all = ProviderMask::all();
        let w = |raw: &str, mask, answered| wanted(&parse_query(raw, &t, NUM, 1), mask, answered);
        assert_eq!(w("qu", all, false), None, "Mixed with 2 characters");
        assert_eq!(w("quo", all, false), Some((150, 3)));
        assert_eq!(w("calc", all, true), None, "an answer or run row");
        assert_eq!(w("file x", all, false), Some((50, 20)));
        assert_eq!(w("file x", all, true), Some((50, 20)), "\"file \" mode lists files whatever else answers");
        assert_eq!(w("file ", all, false), None);
        assert_eq!(w("quokka", all.with(ProviderId::Files, false), false), None, "provider off");
        assert_eq!(w("file x", all.with(ProviderId::Files, false), false), None);
        assert_eq!(w("< x", all, false), None);
        assert_eq!(w("=quokka", all, false), None);
    }

    #[test]
    fn state_rows_only_in_file_mode() {
        for why in [Why::SearchOff, Why::SidDiffers, Why::Unavailable, Why::Stalled, Why::Broken] {
            assert_eq!(state_row(why, false), None);
            let row = state_row(why, true).unwrap();
            assert_eq!(row.key, state_key());
            assert_eq!(row.verbs.is_empty(), why != Why::SearchOff, "{why:?}");
        }
        let off = state_row(Why::SearchOff, true).unwrap();
        assert_eq!((&*off.title, off.detail.as_deref()), ("Windows Search is off", Some("Open indexing options")));
        assert_eq!(off.payload, Payload::Command(CommandId::IndexingOptions));
        assert_eq!(&*state_row(Why::Stalled, true).unwrap().title, "File search isn't answering");
        let admin = state_row(Why::SidDiffers, true).unwrap();
        assert_eq!(&*admin.title, "File search is unavailable while Administrator protection is on");
        assert_eq!(&*state_row(Why::Broken, true).unwrap().title, "File search is unavailable");
    }

    // ---- the pump

    fn req(gen: u64, quiet_ms: u32) -> Req {
        Req { gen, text: format!("quokka {gen}").into(), quiet_ms, cap: 3 }
    }

    fn ready(p: &mut Pump, spawn: u32, now: u64) -> Vec<Act> {
        p.spawned(spawn, true, now);
        p.frame(spawn, Ok(Some(Frame::State(0))), now)
    }

    fn rows_of(gen: u64, names: &[&str]) -> Result<Option<Frame>, FrameError> {
        let hits = names.iter().map(|n| hit(&format!("C:\\u\\{n}"), false)).collect();
        Ok(Some(Frame::Rows { gen, status: 0, skipped: 0, hits }))
    }

    fn posts(acts: &[Act]) -> Vec<&FilesEvent> {
        acts.iter().filter_map(|a| if let Act::Post(e) = a { Some(e) } else { None }).collect()
    }

    /// A pump with helper 1 ready and `gen` written at `now`.
    fn written(gen: u64, now: u64) -> Pump {
        let mut p = Pump::new(LIMITS);
        assert_eq!(p.request(Some(req(gen, 0)), now), [Act::Spawn(1)]);
        assert_eq!(ready(&mut p, 1, now), [Act::Write(req(gen, 0))]);
        p
    }

    #[test]
    fn debounce_by_mode() {
        for quiet in [QUIET_MIXED_MS, QUIET_FILE_MS] {
            let mut p = Pump::new(LIMITS);
            assert!(p.request(Some(req(1, quiet)), 1_000).is_empty());
            assert_eq!(p.next_wake(1_000), Some(u64::from(quiet)));
            assert!(p.tick(1_000 + u64::from(quiet) - 1).is_empty());
            assert_eq!(p.tick(1_000 + u64::from(quiet)), [Act::Spawn(1)]);
        }
    }

    #[test]
    fn no_spawn_before_the_quiet_time() {
        let mut p = Pump::new(LIMITS);
        for (t, gen) in [(0, 1), (100, 2), (200, 3)] {
            assert!(p.request(Some(req(gen, 150)), t).is_empty(), "typing at 100 ms per key");
        }
        assert!(p.tick(349).is_empty());
        assert_eq!(p.tick(350), [Act::Spawn(1)]);
    }

    #[test]
    fn none_drops_the_waiting_request() {
        let mut p = Pump::new(LIMITS);
        assert!(p.request(Some(req(1, 150)), 0).is_empty(), "\"cal\" waits");
        assert!(p.request(None, 40).is_empty(), "\"calc\" has a run row");
        assert_eq!(p.next_wake(40), None);
        assert!(p.tick(10_000).is_empty(), "no Spawn, no Write");
    }

    #[test]
    fn writes_wait_for_state_zero() {
        let mut p = Pump::new(LIMITS);
        assert_eq!(p.request(Some(req(1, 50)), 0), []);
        assert_eq!(p.tick(50), [Act::Spawn(1)]);
        assert!(p.spawned(1, true, 75).is_empty());
        assert!(p.tick(500).is_empty(), "no write before the state frame");
        assert!(p.request(Some(req(2, 50)), 600).is_empty(), "a newer request replaces the waiting one");
        assert_eq!(p.next_wake(600), Some(1_475), "only the spawn deadline runs");
        assert_eq!(p.frame(1, Ok(Some(Frame::State(0))), 700), [Act::Write(req(2, 50))]);
    }

    #[test]
    fn one_request_in_flight() {
        let mut p = written(1, 0);
        assert!(p.request(Some(req(2, 0)), 10).is_empty(), "no second write while one is in flight");
        let acts = p.frame(1, rows_of(1, &["quokka.txt"]), 20);
        assert_eq!(acts, [Act::Write(req(2, 0))], "the rows of an older generation are dropped");
        let acts = p.frame(1, rows_of(2, &["quokka.txt", "quokka 2.txt"]), 30);
        let [FilesEvent::Batch(b)] = posts(&acts)[..] else { panic!("{acts:?}") };
        assert_eq!((b.gen, b.from, b.items.len()), (2, ProviderId::Files, 2));
        assert!(p.tick(5_000).is_empty(), "nothing in flight, nothing waiting");
    }

    #[test]
    fn newer_request_replaces_the_waiting_one() {
        let mut p = written(1, 0);
        p.request(Some(req(2, 0)), 10);
        p.request(Some(req(3, 0)), 20);
        assert_eq!(p.frame(1, rows_of(1, &[]), 30), [Act::Write(req(3, 0))]);
    }

    #[test]
    fn old_generation_rows_dropped() {
        let mut p = written(1, 0);
        p.request(Some(req(2, 150)), 10);
        assert!(p.frame(1, rows_of(1, &["a.txt"]), 20).is_empty(), "dropped; 2 is not due yet");
        assert_eq!(p.tick(160), [Act::Write(req(2, 150))]);
        // A withdrawn request's rows are dropped too.
        p.request(None, 170);
        assert!(posts(&p.frame(1, rows_of(2, &["a.txt"]), 180)).is_empty());
    }

    #[test]
    fn rows_are_capped_and_sorted_on_the_pump() {
        let mut p = written(1, 0);
        let acts =
            p.frame(1, rows_of(1, &["my quokka 1.txt", "a.txt", "quokka b.txt", "quokka c.txt", "quokka d.txt"]), 5);
        let [FilesEvent::Batch(b)] = posts(&acts)[..] else { panic!() };
        let titles: Vec<&str> = b.items.iter().map(|i| &*i.title).collect();
        assert_eq!(titles, ["quokka b.txt", "quokka c.txt", "quokka d.txt"], "Mixed mode shows 3");
        // A failed query posts an empty batch: the busy mark goes.
        p.request(Some(req(2, 0)), 10);
        let acts = p.frame(1, Ok(Some(Frame::Rows { gen: 2, status: 1, skipped: 0, hits: Vec::new() })), 20);
        assert!(matches!(posts(&acts)[..], [FilesEvent::Batch(Batch { gen: 2, items, .. })] if items.is_empty()));
    }

    #[test]
    fn slow_mark_once_at_300_ms() {
        let mut p = written(1, 1_000);
        assert_eq!(p.next_wake(1_000), Some(300));
        assert!(p.tick(1_299).is_empty());
        assert_eq!(p.tick(1_300), [Act::Post(FilesEvent::Slow(1))]);
        assert!(p.tick(1_400).is_empty(), "once");
        assert_eq!(p.next_wake(1_400), Some(600), "the deadline is next");
    }

    #[test]
    fn stale_slow_request_killed_when_a_newer_waits() {
        let mut p = written(1, 0);
        p.request(Some(req(2, 50)), 100);
        assert_eq!(p.next_wake(100), Some(200));
        assert!(p.tick(299).is_empty());
        assert_eq!(p.tick(300), [Act::Kill(KillWhy::Stale), Act::Spawn(2)], "the newer query goes to a new helper");
        assert_eq!(ready(&mut p, 2, 330), [Act::Write(req(2, 50))]);
    }

    #[test]
    fn stale_kill_does_not_count() {
        let mut p = written(1, 0);
        let mut t = 0;
        for spawn in 1..=4u32 {
            p.request(Some(req(u64::from(spawn) + 1, 0)), t + 10);
            let acts = p.tick(t + 300);
            assert_eq!(acts, [Act::Kill(KillWhy::Stale), Act::Spawn(spawn + 1)], "stale kill {spawn}");
            t += 400;
            ready(&mut p, spawn + 1, t);
        }
        assert!(!p.is_off(), "four stale kills: files stay on");
    }

    #[test]
    fn deadline_kills_and_counts() {
        let mut p = written(1, 0);
        assert_eq!(p.tick(300), [Act::Post(FilesEvent::Slow(1))]);
        assert_eq!(p.next_wake(300), Some(700));
        assert_eq!(p.tick(1_000), [Act::Post(FilesEvent::Stalled(1)), Act::Kill(KillWhy::Deadline)]);
        assert!(!p.is_off());
        assert!(p.tick(5_000).is_empty(), "the stalled request is not sent again");
        assert_eq!(p.request(Some(req(2, 0)), 6_000), [Act::Spawn(2)], "respawned at the next request");
    }

    #[test]
    fn three_kills_turn_files_off_for_the_open() {
        let mut p = written(1, 0);
        assert_eq!(p.tick(1_000), [Act::Post(FilesEvent::Stalled(1)), Act::Kill(KillWhy::Deadline)]);
        // A spawn deadline counts as well.
        assert_eq!(p.request(Some(req(2, 0)), 2_000), [Act::Spawn(2)]);
        p.spawned(2, true, 2_000);
        assert_eq!(
            p.tick(4_000),
            [Act::Post(FilesEvent::Stalled(2)), Act::Kill(KillWhy::SpawnDeadline), Act::Spawn(3)]
        );
        ready(&mut p, 3, 4_100);
        let acts = p.tick(5_100);
        assert_eq!(
            acts,
            [Act::Post(FilesEvent::Stalled(2)), Act::Post(FilesEvent::Off(Why::Stalled)), Act::Kill(KillWhy::Deadline)]
        );
        assert!(p.is_off());
        assert!(p.request(Some(req(3, 0)), 6_000).is_empty(), "off for the rest of the open");
        assert_eq!(p.next_wake(6_000), None);
        // The next open starts fresh.
        assert_eq!(Pump::new(LIMITS).request(Some(req(1, 0)), 0), [Act::Spawn(1)]);
    }

    #[test]
    fn spawn_deadline_kills() {
        let mut p = Pump::new(LIMITS);
        p.request(Some(req(1, 0)), 0);
        p.spawned(1, true, 25);
        assert_eq!(p.next_wake(25), Some(2_000));
        assert!(p.tick(2_024).is_empty());
        assert_eq!(
            p.tick(2_025),
            [Act::Post(FilesEvent::Stalled(1)), Act::Kill(KillWhy::SpawnDeadline), Act::Spawn(2)]
        );
        // A helper that cannot start leaves files off.
        let mut p = Pump::new(LIMITS);
        p.request(Some(req(1, 0)), 0);
        assert_eq!(p.spawned(1, false, 5), [Act::Post(FilesEvent::Off(Why::Unavailable))]);
        assert!(p.is_off());
    }

    #[test]
    fn helper_states_turn_files_off() {
        for (s, why) in [(1, Why::SearchOff), (2, Why::SidDiffers), (3, Why::Unavailable), (4, Why::Unavailable)] {
            let mut p = Pump::new(LIMITS);
            p.request(Some(req(1, 0)), 0);
            p.spawned(1, true, 0);
            assert_eq!(p.frame(1, Ok(Some(Frame::State(s))), 5), [Act::Post(FilesEvent::Off(why)), Act::Retire]);
            assert!(p.frame(1, Ok(None), 6).is_empty(), "its exit that follows is ignored");
        }
    }

    #[test]
    fn broken_frame_ends_files() {
        let mut p = written(1, 0);
        let acts = p.frame(1, Err(FrameError::Utf8), 10);
        assert_eq!(acts, [Act::Post(FilesEvent::Off(Why::Broken)), Act::Kill(KillWhy::Broken(FrameError::Utf8))]);
        assert!(p.request(Some(req(2, 0)), 20).is_empty());
    }

    #[test]
    fn pump_checks_the_generation() {
        let mut p = written(5, 0);
        let acts = p.frame(1, rows_of(6, &["a.txt"]), 10);
        assert_eq!(acts, [Act::Post(FilesEvent::Off(Why::Broken)), Act::Kill(KillWhy::Broken(FrameError::WrongGen))]);
        let mut p = Pump::new(LIMITS);
        p.request(Some(req(1, 0)), 0);
        ready(&mut p, 1, 0);
        p.frame(1, rows_of(1, &[]), 1);
        assert_eq!(
            p.frame(1, rows_of(1, &[]), 2)[1],
            Act::Kill(KillWhy::Broken(FrameError::WrongGen)),
            "no request in flight"
        );
    }

    #[test]
    fn eof_after_our_kill_is_not_broken() {
        let mut p = written(1, 0);
        p.request(Some(req(2, 0)), 10);
        assert_eq!(p.tick(300), [Act::Kill(KillWhy::Stale), Act::Spawn(2)]);
        assert!(p.frame(1, Err(FrameError::Truncated), 301).is_empty(), "a truncated frame of the killed helper");
        assert!(p.frame(1, Ok(None), 302).is_empty());
        assert!(p.frame(1, rows_of(1, &["a.txt"]), 303).is_empty());
        assert!(!p.is_off());
        // The live helper's EOF at a frame boundary with nothing in flight is its exit: reaped, respawned at the next
        // request, never Broken and never counted.
        assert_eq!(ready(&mut p, 2, 310), [Act::Write(req(2, 0))]);
        assert_eq!(posts(&p.frame(2, rows_of(2, &[]), 311)).len(), 1);
        for spawn in 2..=5 {
            assert_eq!(p.frame(spawn, Ok(None), 320), [Act::Retire], "exit {spawn}");
            let gen = u64::from(spawn) + 1;
            assert_eq!(p.request(Some(req(gen, 0)), 400), [Act::Spawn(spawn + 1)]);
            assert_eq!(ready(&mut p, spawn + 1, 400), [Act::Write(req(gen, 0))]);
            assert_eq!(posts(&p.frame(spawn + 1, rows_of(gen, &[]), 401)).len(), 1);
        }
        assert!(!p.is_off(), "four exits with nothing in flight: files stay on");
    }

    #[test]
    fn exits_mid_request_count_as_kills() {
        // A helper ends only when the dock closes its stdin: an end with a request in flight is a death (a crash, a
        // line it refused), counted as a deadline kill is, so a helper that dies on every query is not respawned for
        // ever.
        let mut p = written(1, 0);
        assert_eq!(p.frame(1, Ok(None), 10), [Act::Post(FilesEvent::Stalled(1)), Act::Died]);
        assert!(!p.is_off());
        assert!(p.tick(20).is_empty(), "the request it died on is not sent again");
        assert_eq!(p.request(Some(req(2, 0)), 30), [Act::Spawn(2)]);
        assert_eq!(ready(&mut p, 2, 31), [Act::Write(req(2, 0))]);
        // A newer request waits: the death gives up the one in flight without a stall row, and the newer one goes to
        // a new helper.
        assert!(p.request(Some(req(3, 0)), 40).is_empty());
        assert_eq!(p.frame(2, Ok(None), 41), [Act::Died]);
        assert_eq!(p.tick(41), [Act::Spawn(3)]);
        assert_eq!(ready(&mut p, 3, 42), [Act::Write(req(3, 0))]);
        assert_eq!(
            p.frame(3, Ok(None), 50),
            [Act::Post(FilesEvent::Stalled(3)), Act::Post(FilesEvent::Off(Why::Stalled)), Act::Died]
        );
        assert!(p.is_off());
        assert!(p.request(Some(req(4, 0)), 60).is_empty(), "off for the rest of the open");
        assert_eq!(p.next_wake(60), None);
    }

    #[test]
    fn close_ends_with_grace_then_kill() {
        let mut p = written(1, 0);
        p.request(Some(req(2, 0)), 10);
        assert_eq!(p.close(), [Act::End], "the thread retires the helper: stdin closed, the grace, then a kill");
        assert_eq!(CLOSE_GRACE_MS, 200);
        assert!(p.frame(1, rows_of(1, &["a.txt"]), 20).is_empty(), "nothing after the close");
        assert!(p.request(Some(req(3, 0)), 30).is_empty());
        assert!(p.tick(5_000).is_empty());
        assert_eq!(p.next_wake(5_000), None);
    }
}
