//! The files protocols: ops (named pipe), shell host (inherited anonymous pipes) and instance (WM_COPYDATA).
//! Discriminant ranges: InstanceCmd 0x0301-0x03FF, OpReq 0x0101-0x017F, OpEvt 0x0181-0x01FF, ShellReq 0x0201-0x027F,
//! ShellEvt 0x0281-0x02FF, so a frame sent the wrong way is UnknownKind. Every value is written on its variant and is
//! never reordered or reused; a new variant takes the next free value and bumps the protocol version.
//!
//! WidePath, the encoded sizes (WireLen) and the codecs of the five message enums; the framing is in mod.rs.

use super::wire::{Reader, Writer};
use super::{Codec, FrameError, Proto, WireLen};

pub const OPS: Proto = Proto { magic: u32::from_le_bytes(*b"FOPS"), version: 1, max_payload: 1 << 20 };
/// Version 2 added the type names (ShellReq::TypeNames, ShellEvt::TypeNames); a version-1 peer is refused at the Hello.
pub const SHELL: Proto = Proto { magic: u32::from_le_bytes(*b"FSHL"), version: 2, max_payload: 1 << 20 };
pub const INSTANCE: Proto = Proto { magic: u32::from_le_bytes(*b"FINS"), version: 1, max_payload: 64 << 10 };
pub const MAX_PATH_UNITS: u16 = 32_767;
pub const MAX_NAME_UNITS: u16 = 255;
pub const MAX_LIST: u32 = 2_048; // elements per frame, and never more bytes than split_by_bytes allows (splitting rules below)
pub const UNDO_MAX_ENTRIES: u32 = 16_384; // top-level entries of one undo record; above it the op is not undoable (OP3)
pub const MAX_PIXELS_SIDE: u16 = 480;

/// A normalised absolute Win32 path: "X:\..." or "\\server\share\...", no \\?\ prefix, no NUL, no control
/// character, no "." or ".." component, no trailing separator except on a root.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct WidePath(Box<[u16]>);
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PathError {
    Empty,
    TooLong,
    Control,
    NotAbsolute,
    Device,
    DotComponent,
}

const BS: u16 = b'\\' as u16;
const SLASH: u16 = b'/' as u16;
const COLON: u16 = b':' as u16;
const DOT: u16 = b'.' as u16;
const QUESTION: u16 = b'?' as u16;

/// `units` starts with the ASCII `prefix`, compared case-insensitively.
fn starts_ascii_ci(units: &[u16], prefix: &[u8]) -> bool {
    units.len() >= prefix.len()
        && units.iter().zip(prefix).all(|(&u, &b)| u < 0x80 && (u as u8).eq_ignore_ascii_case(&b))
}

fn is_dot_name(c: &[u16]) -> bool {
    c == [DOT] || c == [DOT, DOT]
}

impl WidePath {
    /// Strips "\\?\" and "\\?\UNC\", maps '/' to '\', folds repeated separators and drops a trailing '\'; refuses
    /// "\\.\", "\\?\GLOBALROOT", "\\?\Volume{", a relative or drive-relative path, a "." or ".." component, NUL and
    /// control characters, and more than MAX_PATH_UNITS units once normalised. The display form keeps the case.
    pub fn new(units: &[u16]) -> Result<Self, PathError> {
        if units.is_empty() {
            return Err(PathError::Empty);
        }
        // The longest prefix stripped below is 8 units: anything longer cannot normalise under the cap.
        if units.len() > usize::from(MAX_PATH_UNITS) + 8 {
            return Err(PathError::TooLong);
        }
        if units.iter().any(|&u| u < 0x20) {
            return Err(PathError::Control);
        }
        let mapped: Vec<u16> = units.iter().map(|&u| if u == SLASH { BS } else { u }).collect();
        let mut s: &[u16] = &mapped;
        let mut unc = false;
        if starts_ascii_ci(s, br"\\?\UNC\") {
            s = s.get(8..).unwrap_or_default();
            unc = true;
        } else if starts_ascii_ci(s, br"\\?\") {
            let rest = s.get(4..).unwrap_or_default();
            if starts_ascii_ci(rest, b"GLOBALROOT") || starts_ascii_ci(rest, b"Volume{") {
                return Err(PathError::Device);
            }
            s = rest;
        } else if starts_ascii_ci(s, br"\\.\") || s == [BS, BS, DOT] || s == [BS, BS, QUESTION] {
            return Err(PathError::Device);
        }
        let mut out: Vec<u16> = Vec::with_capacity(s.len());
        let rest: &[u16];
        if unc || starts_ascii_ci(s, br"\\") {
            let body = if unc { s } else { s.get(2..).unwrap_or_default() };
            let mut parts = body.splitn(3, |&u| u == BS);
            let server = parts.next().unwrap_or_default();
            let share = parts.next().unwrap_or_default();
            if server == [DOT] || server == [QUESTION] {
                return Err(PathError::Device);
            }
            if server.is_empty() || share.is_empty() {
                return Err(PathError::NotAbsolute);
            }
            if is_dot_name(server) || is_dot_name(share) {
                return Err(PathError::DotComponent);
            }
            out.extend_from_slice(&[BS, BS]);
            out.extend_from_slice(server);
            out.push(BS);
            out.extend_from_slice(share);
            rest = parts.next().unwrap_or_default();
        } else {
            match s {
                [d, c, b, tail @ ..] if *c == COLON && *b == BS && *d < 0x80 && (*d as u8).is_ascii_alphabetic() => {
                    out.extend_from_slice(&[*d, *c, *b]);
                    rest = tail;
                }
                _ => return Err(PathError::NotAbsolute),
            }
        }
        for c in rest.split(|&u| u == BS).filter(|c| !c.is_empty()) {
            if is_dot_name(c) {
                return Err(PathError::DotComponent);
            }
            if out.last() != Some(&BS) {
                out.push(BS);
            }
            out.extend_from_slice(c);
        }
        if out.len() > usize::from(MAX_PATH_UNITS) {
            return Err(PathError::TooLong);
        }
        Ok(WidePath(out.into_boxed_slice()))
    }

    pub fn parse(s: &str) -> Result<Self, PathError> {
        Self::new(&s.encode_utf16().collect::<Vec<u16>>())
    }

    pub fn units(&self) -> &[u16] {
        &self.0
    }

    /// "\\?\X:\..." or "\\?\UNC\server\share\...", NUL-terminated, for Win32 calls only.
    pub fn raw(&self) -> Vec<u16> {
        let mut v = Vec::with_capacity(self.0.len() + 9);
        if self.is_unc() {
            v.extend(r"\\?\UNC\".encode_utf16());
            v.extend_from_slice(self.0.get(2..).unwrap_or_default());
        } else {
            v.extend(r"\\?\".encode_utf16());
            v.extend_from_slice(&self.0);
        }
        v.push(0);
        v
    }

    pub fn is_unc(&self) -> bool {
        self.0.starts_with(&[BS, BS])
    }

    pub fn is_root(&self) -> bool {
        self.root_len() == self.0.len()
    }

    /// Units of the root: "X:\" (3) or "\\server\share".
    fn root_len(&self) -> usize {
        if !self.is_unc() {
            return 3;
        }
        // The second separator after the leading two ends the share, else the path is the root.
        let body = self.0.get(2..).unwrap_or_default();
        body.iter().enumerate().filter(|(_, &u)| u == BS).nth(1).map_or(self.0.len(), |(i, _)| i + 2)
    }

    /// The position of the separator before the last component (None on a root).
    fn last_sep(&self) -> Option<usize> {
        if self.is_root() {
            return None;
        }
        self.0.iter().rposition(|&u| u == BS)
    }

    pub fn parent(&self) -> Option<WidePath> {
        let cut = self.last_sep()?;
        // "C:\a" -> "C:\" keeps the root's separator; "\\srv\sh\x" -> "\\srv\sh".
        let end = if self.is_unc() { cut } else { cut.max(2) + usize::from(cut == 2) };
        self.0.get(..end).map(|p| WidePath(p.into()))
    }

    pub fn file_name(&self) -> Option<&[u16]> {
        let cut = self.last_sep()?;
        self.0.get(cut + 1..)
    }

    /// This path with one more component; refuses an empty name, a separator, "." or "..", a control character and
    /// a result over MAX_PATH_UNITS.
    pub fn join(&self, name: &[u16]) -> Result<WidePath, PathError> {
        if name.is_empty() {
            return Err(PathError::Empty);
        }
        if name.iter().any(|&u| u < 0x20) {
            return Err(PathError::Control);
        }
        if name.iter().any(|&u| u == BS || u == SLASH) {
            return Err(PathError::NotAbsolute);
        }
        if is_dot_name(name) {
            return Err(PathError::DotComponent);
        }
        let mut v = Vec::with_capacity(self.0.len() + 1 + name.len());
        v.extend_from_slice(&self.0);
        if v.last() != Some(&BS) {
            v.push(BS);
        }
        v.extend_from_slice(name);
        if v.len() > usize::from(MAX_PATH_UNITS) {
            return Err(PathError::TooLong);
        }
        Ok(WidePath(v.into_boxed_slice()))
    }

    pub fn to_string_lossy(&self) -> String {
        String::from_utf16_lossy(&self.0)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct Gen(pub u64);
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct OpId(pub u32);
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct ReqId(pub u32);
#[repr(u8)]
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ItemRef {
    Path(WidePath) = 1,
    Pidl(Box<[u8]>) = 2,
} // Pidl <= 64 KiB, hand-offs only
#[repr(u8)]
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Location {
    Fs(WidePath) = 1,
    Home = 2,
    Search { root: WidePath, query: Box<str> } = 3,
    Options = 4,
} // query <= 1 KiB

// ---- instance: WM_COPYDATA to the MiniOledFilesIpc window, dwData = INSTANCE.magic, lpData = one frame
#[repr(u16)]
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum InstanceCmd {
    NewWindow(Location) = 0x0301,
    OpenTab(Location) = 0x0302,
    Select { dir: WidePath, item: WidePath } = 0x0303,
    Probe { path: WidePath } = 0x0304,
}

// ---- ops pipe \\.\pipe\mini-oled-dock.files.ops.<session>-<proto>-<build8>[-<test tag>]  (F-P3 PipeName form: app "files", purpose "ops", one nonce)
#[repr(u8)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Conflicts {
    AskSystem = 0,
    ReplaceAll = 1,
    SkipAll = 2,
}
#[repr(u8)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum OpKind {
    Copy = 1,
    Move = 2,
    Recycle = 3,
    DeletePermanent = 4,
    Rename = 5,
    NewFolder = 6,
    Undo = 7,
    Resume = 8,
}
#[repr(u8)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum UndoKind {
    RenameBack = 1,
    MoveBack = 2,
    RecycleCopies = 3,
    RecycleNewFolder = 4,
    RestoreRecycled = 5,
    OpenRecycleBin = 6,
}
/// One item of an undo record: where it was before the operation, where it is after, and size and mtime of 'to'
/// read right after the operation (dirs: size 0, mtime of the folder). bin = the recycled item's parsing name.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct UndoEntry {
    pub from: WidePath,
    pub to: WidePath,
    pub size: u64,
    pub mtime: i64,
    pub dir: bool,
    pub bin: Option<WidePath>,
}
#[repr(u8)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ChangeKind {
    Copied = 1,
    Moved = 2,
    Renamed = 3,
    Recycled = 4,
    Deleted = 5,
    Created = 6,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Change {
    pub kind: ChangeKind,
    pub from: Option<WidePath>,
    pub to: Option<WidePath>,
}
#[repr(u8)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum OpResult {
    Done = 0,
    Cancelled = 1,
    Failed = 2,
    Refused = 3,
}
#[repr(u8)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Refusal {
    BadPath = 1,
    Root = 2,
    OutsideTestRoot = 3,
    NoItems = 4,
    UnknownOp = 5,
    OpInUse = 6,
    Changed = 7,
    Replaced = 8,
    NotEmpty = 9,
    PauseUnsupported = 10,
    BadName = 11,
    Elevated = 12,
    MixedParents = 13,
    TooMany = 14,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OpStatus {
    pub op: OpId,
    pub kind: OpKind,
    pub done: u64,
    pub total: u64,
    pub needs_input: bool,
}

#[repr(u16)]
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum OpReq {
    Attach { ui_pid: u32, owner: u64 } = 0x0101, // owner = the UI window for dialogs (FS-3 b)
    Stage { op: OpId, items: Vec<ItemRef> } = 0x0102, // appends; 1..=MAX_LIST per frame
    Copy { op: OpId, dest: WidePath, conflicts: Conflicts } = 0x0103,
    Move { op: OpId, dest: WidePath, conflicts: Conflicts } = 0x0104,
    Recycle { op: OpId } = 0x0105,
    DeletePermanent { op: OpId } = 0x0106,
    // The decoder refuses (Invalid) a `to` that is empty, "." or "..", over MAX_NAME_UNITS units, or holds a
    // separator or a control character; encode does not check it.
    Rename { op: OpId, item: WidePath, to: Box<str> } = 0x0107, // to: one name, <= MAX_NAME_UNITS
    // name: as Rename's `to`.
    NewFolder { op: OpId, parent: WidePath, name: Box<str> } = 0x0108,
    UndoStage { op: OpId, entries: Vec<UndoEntry> } = 0x0109,
    Undo { op: OpId, of: OpId, kind: UndoKind, replaced: u32 } = 0x010A,
    Cancel { op: OpId } = 0x010B,
    Pause { op: OpId, on: bool } = 0x010C,
    BringForward { op: OpId } = 0x010D,
    ResumeRest { op: OpId, of: OpId } = 0x010E,
    AckDone { of: OpId } = 0x010F,
    Bye = 0x0110,
}
#[repr(u16)]
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum OpEvt {
    Accepted { op: OpId } = 0x0181,
    Started { op: OpId, kind: OpKind, items: u32, bytes: u64 } = 0x0182,
    Progress { op: OpId, done: u64, total: u64, bps: u64, current: Box<str> } = 0x0183, // <= 10 per second per op; current <= 520 bytes
    NeedsInput { op: OpId, on: bool } = 0x0184,
    ItemsDone { op: OpId, changes: Vec<Change> } = 0x0185, // coalesced with Progress, <= MAX_LIST per frame
    ItemFailed { op: OpId, item: WidePath, hr: i32 } = 0x0186,
    UndoPart { op: OpId, entries: Vec<UndoEntry> } = 0x0187, // before Finished
    Finished { op: OpId, result: OpResult, undo: Option<UndoKind>, replaced: u32, items: u32, failed: u32 } = 0x0188,
    Refused { op: OpId, why: Refusal, detail: Box<str> } = 0x0189, // detail <= 520 bytes (a name, never a whole path list)
    Snapshot { running: Vec<OpStatus> } = 0x018A,                  // reply to Attach
    Pending { of: OpId, kind: OpKind, done: u32, total: u32, dest: Option<WidePath> } = 0x018B, // an interrupted op that can resume
    DoneWhileAway { of: OpId, kind: OpKind, items: u32, failed: u32, dest: Option<WidePath> } = 0x018C,
}

// ---- shell host: inherited anonymous pipes
#[repr(u16)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Verb {
    Open = 1,
    OpenWith = 2,
    Properties = 3,
    Pin = 4,
    Unpin = 5,
    Share = 6,
    InvokeModern(u32) = 7,
    OpenInExplorer = 8,
}
#[repr(u8)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CanonVerb {
    Delete = 1,
    Cut = 2,
    Copy = 3,
    Paste = 4,
    PasteLink = 5,
    Rename = 6,
}
#[repr(u16)]
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum CallKind {
    QueryContextMenu = 1,
    HandleMenuMsg = 2,
    InvokeCommand = 3,
    ModernItems = 4,
    Exec = 5,
    Icon = 6,
    Thumb = 7,
    ClassIcon = 8,
    ShellNew = 9,
    Pins = 10,
    VerbName = 11,
}
#[repr(u8)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MenuOutcome {
    Dismissed = 1,
    Invoked = 2,
    Intercepted(CanonVerb) = 3,
    Failed(i32) = 4,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ModernItem {
    pub id: u32,
    pub label: Box<str>,
    pub has_sub: bool,
} // label <= 512 bytes
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Pixels {
    pub w: u16,
    pub h: u16,
    pub bgra: Box<[u8]>,
} // premultiplied, w*h*4 bytes, sides <= MAX_PIXELS_SIDE
/// One type of the "New" menu, as the decoder checks it: encode does not, and one bad type refuses the whole frame.
///
/// ext as in ClassIcons: at most 64 bytes, lower case, no dot, separator or control character ("" = no extension);
/// the sender strips the leading dot, lower-cases, and leaves out a type whose ext still breaks the rule.
/// label: at most 512 bytes, as ModernItem's.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ShellNewType {
    pub ext: Box<str>,
    pub label: Box<str>,
}
/// One file type's name as the shell names it (SHELL version 2), as the decoder checks it: ext as in ClassIcons (at
/// most 64 bytes, lower case, no dot, separator or control character), name at most 260 bytes.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TypeName {
    pub ext: Box<str>,  // <= 64 bytes, lower case, no dot
    pub name: Box<str>, // <= 260 bytes
}
#[repr(u16)]
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ShellReq {
    Menu { req: ReqId, items: Vec<ItemRef>, at: (i32, i32), extended: bool, owner: u64 } = 0x0201, // owner: logged only, never attached to
    ModernItems { req: ReqId, items: Vec<ItemRef> } = 0x0202,
    Exec { req: ReqId, verb: Verb, items: Vec<ItemRef> } = 0x0203,
    RunTyped { req: ReqId, text: Box<str>, dir: WidePath } = 0x0204, // text <= 8 KiB
    // px at most MAX_PIXELS_SIDE, or the decoder refuses the frame (Cap; encode does not check): the sender clamps it.
    Icons { req: ReqId, gen: Gen, items: Vec<ItemRef>, px: u16 } = 0x0205,
    // px: as Icons.
    Thumbs { req: ReqId, gen: Gen, items: Vec<ItemRef>, px: u16, cache_only: bool } = 0x0206,
    // px: as Icons.
    ClassIcons { req: ReqId, exts: Vec<Box<str>>, px: u16 } = 0x0207, // ext <= 64 bytes, lower case, no dot
    ShellNewTypes { req: ReqId } = 0x0208,
    QuickAccessPins { req: ReqId } = 0x0209,
    CancelGen { gen: Gen } = 0x020A, // drops queued image work of older gens
    Bye = 0x020B,
    TypeNames { req: ReqId, exts: Vec<Box<str>> } = 0x020C, // whole frame; at most 256 exts per request
}
#[repr(u16)]
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ShellEvt {
    Ready { pid: u32 } = 0x0281,
    MenuShown { req: ReqId } = 0x0282,
    MenuDone { req: ReqId, outcome: MenuOutcome, items: Vec<ItemRef> } = 0x0283, // items only for Intercepted
    Modern { req: ReqId, items: Vec<ModernItem>, done: bool } = 0x0284,
    ExecDone { req: ReqId, hr: i32 } = 0x0285,
    Icon { req: ReqId, gen: Gen, index: u32, px: Pixels } = 0x0286,
    Thumb { req: ReqId, gen: Gen, index: u32, px: Pixels } = 0x0287,
    ClassIcon { req: ReqId, ext: Box<str>, px: Pixels } = 0x0288,
    ShellNew { req: ReqId, types: Vec<ShellNewType> } = 0x0289, // split by bytes; ReqDone after the last
    Pins { req: ReqId, pins: Vec<WidePath> } = 0x028A,          // split by bytes; ReqDone after the last
    Failed { req: ReqId, hr: i32 } = 0x028B,
    Busy { on: bool } = 0x028C,
    // The last frame of Icons, Thumbs, ClassIcons, ShellNewTypes, QuickAccessPins and TypeNames.
    ReqDone { req: ReqId } = 0x028D,
    // Split by bytes (split_by_bytes); ReqDone after the last. Never empty: the decoder refuses an empty list.
    TypeNames { req: ReqId, names: Vec<TypeName> } = 0x028E,
}

// ---- encoded sizes (2.2's field encodings): a WidePath is 2 + 2 x units bytes, a Box<str> 4 + its UTF-8 bytes,
// an Option 1 + its value, a byte list 4 + its length, an enum tag 1 or 2 bytes as declared.

fn opt_len<T: WireLen>(v: &Option<T>) -> usize {
    1 + v.as_ref().map_or(0, WireLen::wire_len)
}

impl WireLen for WidePath {
    fn wire_len(&self) -> usize {
        2 + 2 * self.0.len()
    }
}

impl WireLen for Box<str> {
    fn wire_len(&self) -> usize {
        4 + self.len()
    }
}

impl WireLen for ItemRef {
    fn wire_len(&self) -> usize {
        1 + match self {
            ItemRef::Path(p) => p.wire_len(),
            ItemRef::Pidl(b) => 4 + b.len(),
        }
    }
}

impl WireLen for UndoEntry {
    fn wire_len(&self) -> usize {
        self.from.wire_len() + self.to.wire_len() + 8 + 8 + 1 + opt_len(&self.bin)
    }
}

impl WireLen for Change {
    fn wire_len(&self) -> usize {
        1 + opt_len(&self.from) + opt_len(&self.to)
    }
}

impl WireLen for ModernItem {
    fn wire_len(&self) -> usize {
        4 + (4 + self.label.len()) + 1
    }
}

impl WireLen for ShellNewType {
    fn wire_len(&self) -> usize {
        (4 + self.ext.len()) + (4 + self.label.len())
    }
}

impl WireLen for TypeName {
    fn wire_len(&self) -> usize {
        (4 + self.ext.len()) + (4 + self.name.len())
    }
}

// ---- codecs (2.2's field encodings): ids and Gen as their integer; a WidePath as wire::wide (at most
// MAX_PATH_UNITS) and only in its normal form, so that decoding then encoding gives back the same bytes; a Box<str>
// as wire::str under the cap written on its field; an Option as a u8 tag 0/1 then the value; a Vec as wire::count
// (at most MAX_LIST) then the items; a nested enum as its u8/u16 tag then its fields; (i32, i32) as two i32.

const PIDL_MAX: u32 = 64 << 10;
const QUERY_MAX: u32 = 1 << 10;
/// A name of MAX_NAME_UNITS units takes at most three UTF-8 bytes per unit; the unit count is checked after.
const NAME_MAX: u32 = 3 * MAX_NAME_UNITS as u32;
const CURRENT_MAX: u32 = 520;
const DETAIL_MAX: u32 = 520;
const LABEL_MAX: u32 = 512;
const TEXT_MAX: u32 = 8 << 10;
const EXT_MAX: u32 = 64;
/// A type name: SHGetFileInfoW's szTypeName (80 UTF-16 units) as UTF-8 fits.
const TYPE_NAME_MAX: u32 = 260;
/// The extensions of one TypeNames request.
const TYPE_EXTS_MAX: u32 = 256;
const BGRA_MAX: u32 = MAX_PIXELS_SIDE as u32 * MAX_PIXELS_SIDE as u32 * 4;

fn op_id(r: &mut Reader<'_>) -> Result<OpId, FrameError> {
    r.u32().map(OpId)
}

fn req_id(r: &mut Reader<'_>) -> Result<ReqId, FrameError> {
    r.u32().map(ReqId)
}

fn gen(r: &mut Reader<'_>) -> Result<Gen, FrameError> {
    r.u64().map(Gen)
}

fn put_path(w: &mut Writer, p: &WidePath) {
    w.wide(p.units());
}

/// A path in WidePath's normal form; one that WidePath::new refuses or would rewrite is Invalid (a sender's WidePath
/// is always normal, so the bytes of a path have one meaning).
fn get_path(r: &mut Reader<'_>, field: &'static str) -> Result<WidePath, FrameError> {
    let units = r.wide(field, MAX_PATH_UNITS)?;
    match WidePath::new(&units) {
        Ok(p) if *p.units() == *units => Ok(p),
        _ => Err(FrameError::Invalid { field }),
    }
}

fn put_opt<T>(w: &mut Writer, v: &Option<T>, put: impl FnOnce(&mut Writer, &T)) {
    match v {
        None => w.u8(0),
        Some(x) => {
            w.u8(1);
            put(w, x);
        }
    }
}

fn get_opt<'a, T>(
    r: &mut Reader<'a>,
    field: &'static str,
    get: impl FnOnce(&mut Reader<'a>) -> Result<T, FrameError>,
) -> Result<Option<T>, FrameError> {
    match r.u8()? {
        0 => Ok(None),
        1 => get(r).map(Some),
        v => Err(FrameError::BadValue { field, value: u32::from(v) }),
    }
}

fn put_list<T>(w: &mut Writer, items: &[T], mut put: impl FnMut(&mut Writer, &T)) {
    // Past u32::MAX items the payload is past max_payload too, and encode refuses it.
    w.u32(u32::try_from(items.len()).unwrap_or(u32::MAX));
    for x in items {
        put(w, x);
    }
}

fn get_list<'a, T>(
    r: &mut Reader<'a>,
    field: &'static str,
    get: impl FnMut(&mut Reader<'a>) -> Result<T, FrameError>,
) -> Result<Vec<T>, FrameError> {
    get_list_upto(r, field, MAX_LIST, get)
}

/// get_list with a lower count cap than MAX_LIST.
fn get_list_upto<'a, T>(
    r: &mut Reader<'a>,
    field: &'static str,
    max: u32,
    mut get: impl FnMut(&mut Reader<'a>) -> Result<T, FrameError>,
) -> Result<Vec<T>, FrameError> {
    // count() bounds n by max and by the bytes left before anything is allocated.
    let n = r.count(field, max)?;
    let mut v = Vec::with_capacity(n as usize);
    for _ in 0..n {
        v.push(get(r)?);
    }
    Ok(v)
}

fn put_str(w: &mut Writer, s: &str) {
    w.str(s);
}

fn get_str(r: &mut Reader<'_>, field: &'static str, max_bytes: u32) -> Result<Box<str>, FrameError> {
    r.str(field, max_bytes).map(Box::from)
}

/// One file or folder name: 1..=MAX_NAME_UNITS units, no separator, no control character, not "." or "..";
/// Invalid otherwise, also past NAME_MAX bytes (no name within MAX_NAME_UNITS units is longer).
fn get_name(r: &mut Reader<'_>, field: &'static str) -> Result<Box<str>, FrameError> {
    let s = r.str(field, NAME_MAX).map_err(|e| match e {
        FrameError::Cap { .. } => FrameError::Invalid { field },
        e => e,
    })?;
    let ok = !s.is_empty()
        && s != "."
        && s != ".."
        && s.encode_utf16().count() <= usize::from(MAX_NAME_UNITS)
        && !s.chars().any(|c| c == '\\' || c == '/' || c < ' ');
    if ok {
        Ok(s.into())
    } else {
        Err(FrameError::Invalid { field })
    }
}

/// An extension as class icons are keyed: at most EXT_MAX bytes, lower case, no dot, no separator and no control
/// character ("" is the class of names without one).
fn get_ext(r: &mut Reader<'_>, field: &'static str) -> Result<Box<str>, FrameError> {
    let s = r.str(field, EXT_MAX)?;
    let lower = if s.is_ascii() { !s.bytes().any(|b| b.is_ascii_uppercase()) } else { s.to_lowercase() == s };
    if lower && !s.chars().any(|c| c == '.' || c == '\\' || c == '/' || c < ' ') {
        Ok(s.into())
    } else {
        Err(FrameError::Invalid { field })
    }
}

/// A requested icon or thumbnail side: at most MAX_PIXELS_SIDE, so the answer fits in one frame.
fn get_px(r: &mut Reader<'_>, field: &'static str) -> Result<u16, FrameError> {
    let px = r.u16()?;
    if px > MAX_PIXELS_SIDE {
        return Err(FrameError::Cap { field, len: u32::from(px), max: u32::from(MAX_PIXELS_SIDE) });
    }
    Ok(px)
}

fn put_pixels(w: &mut Writer, px: &Pixels) {
    w.u16(px.w);
    w.u16(px.h);
    w.bytes(&px.bgra);
}

/// Sides at most MAX_PIXELS_SIDE (Cap), then exactly w * h * 4 bytes (Invalid otherwise).
fn get_pixels(r: &mut Reader<'_>, field: &'static str) -> Result<Pixels, FrameError> {
    let (w, h) = (r.u16()?, r.u16()?);
    for side in [w, h] {
        if side > MAX_PIXELS_SIDE {
            return Err(FrameError::Cap { field, len: u32::from(side), max: u32::from(MAX_PIXELS_SIDE) });
        }
    }
    let bgra = r.bytes(field, BGRA_MAX)?;
    if bgra.len() != usize::from(w) * usize::from(h) * 4 {
        return Err(FrameError::Invalid { field });
    }
    Ok(Pixels { w, h, bgra: bgra.into() })
}

/// Decoders of the fieldless u8 enums: the tag is the value written on the variant, as the encoders write it
/// (`as u8`). The inner match lists every variant, so a new one does not compile until it is listed here.
macro_rules! u8_tags {
    ($($get:ident: $t:ident [$($v:ident),+ $(,)?];)+) => {$(
        fn $get(r: &mut Reader<'_>, field: &'static str) -> Result<$t, FrameError> {
            fn listed(k: $t) {
                match k {
                    $($t::$v)|+ => {}
                }
            }
            let _ = listed;
            let v = r.u8()?;
            [$($t::$v),+]
                .into_iter()
                .find(|k| *k as u8 == v)
                .ok_or(FrameError::BadValue { field, value: u32::from(v) })
        }
    )+};
}

u8_tags! {
    get_conflicts: Conflicts [AskSystem, ReplaceAll, SkipAll];
    get_op_kind: OpKind [Copy, Move, Recycle, DeletePermanent, Rename, NewFolder, Undo, Resume];
    get_undo_kind: UndoKind [RenameBack, MoveBack, RecycleCopies, RecycleNewFolder, RestoreRecycled, OpenRecycleBin];
    get_change_kind: ChangeKind [Copied, Moved, Renamed, Recycled, Deleted, Created];
    get_op_result: OpResult [Done, Cancelled, Failed, Refused];
    get_refusal: Refusal [BadPath, Root, OutsideTestRoot, NoItems, UnknownOp, OpInUse, Changed, Replaced, NotEmpty,
                          PauseUnsupported, BadName, Elevated, MixedParents, TooMany];
    get_canon: CanonVerb [Delete, Cut, Copy, Paste, PasteLink, Rename];
}

fn put_item(w: &mut Writer, it: &ItemRef) {
    match it {
        ItemRef::Path(p) => {
            w.u8(1);
            put_path(w, p);
        }
        ItemRef::Pidl(b) => {
            w.u8(2);
            w.bytes(b);
        }
    }
}

fn get_item(r: &mut Reader<'_>, field: &'static str) -> Result<ItemRef, FrameError> {
    match r.u8()? {
        1 => get_path(r, field).map(ItemRef::Path),
        2 => r.bytes(field, PIDL_MAX).map(|b| ItemRef::Pidl(b.into())),
        v => Err(FrameError::BadValue { field, value: u32::from(v) }),
    }
}

fn put_location(w: &mut Writer, at: &Location) {
    match at {
        Location::Fs(p) => {
            w.u8(1);
            put_path(w, p);
        }
        Location::Home => w.u8(2),
        Location::Search { root, query } => {
            w.u8(3);
            put_path(w, root);
            w.str(query);
        }
        Location::Options => w.u8(4),
    }
}

fn get_location(r: &mut Reader<'_>, field: &'static str) -> Result<Location, FrameError> {
    Ok(match r.u8()? {
        1 => Location::Fs(get_path(r, "Location::Fs")?),
        2 => Location::Home,
        3 => Location::Search { root: get_path(r, "Search.root")?, query: get_str(r, "Search.query", QUERY_MAX)? },
        4 => Location::Options,
        v => return Err(FrameError::BadValue { field, value: u32::from(v) }),
    })
}

fn put_undo(w: &mut Writer, e: &UndoEntry) {
    put_path(w, &e.from);
    put_path(w, &e.to);
    w.u64(e.size);
    w.i64(e.mtime);
    w.bool(e.dir);
    put_opt(w, &e.bin, put_path);
}

fn get_undo(r: &mut Reader<'_>) -> Result<UndoEntry, FrameError> {
    Ok(UndoEntry {
        from: get_path(r, "UndoEntry.from")?,
        to: get_path(r, "UndoEntry.to")?,
        size: r.u64()?,
        mtime: r.i64()?,
        dir: r.bool("UndoEntry.dir")?,
        bin: get_opt(r, "UndoEntry.bin", |r| get_path(r, "UndoEntry.bin"))?,
    })
}

fn put_change(w: &mut Writer, c: &Change) {
    w.u8(c.kind as u8);
    put_opt(w, &c.from, put_path);
    put_opt(w, &c.to, put_path);
}

fn get_change(r: &mut Reader<'_>) -> Result<Change, FrameError> {
    Ok(Change {
        kind: get_change_kind(r, "Change.kind")?,
        from: get_opt(r, "Change.from", |r| get_path(r, "Change.from"))?,
        to: get_opt(r, "Change.to", |r| get_path(r, "Change.to"))?,
    })
}

fn put_status(w: &mut Writer, s: &OpStatus) {
    w.u32(s.op.0);
    w.u8(s.kind as u8);
    w.u64(s.done);
    w.u64(s.total);
    w.bool(s.needs_input);
}

fn get_status(r: &mut Reader<'_>) -> Result<OpStatus, FrameError> {
    Ok(OpStatus {
        op: op_id(r)?,
        kind: get_op_kind(r, "OpStatus.kind")?,
        done: r.u64()?,
        total: r.u64()?,
        needs_input: r.bool("OpStatus.needs_input")?,
    })
}

fn put_verb(w: &mut Writer, v: Verb) {
    let (tag, id) = match v {
        Verb::Open => (1, None),
        Verb::OpenWith => (2, None),
        Verb::Properties => (3, None),
        Verb::Pin => (4, None),
        Verb::Unpin => (5, None),
        Verb::Share => (6, None),
        Verb::InvokeModern(id) => (7, Some(id)),
        Verb::OpenInExplorer => (8, None),
    };
    w.u16(tag);
    if let Some(id) = id {
        w.u32(id);
    }
}

fn get_verb(r: &mut Reader<'_>, field: &'static str) -> Result<Verb, FrameError> {
    Ok(match r.u16()? {
        1 => Verb::Open,
        2 => Verb::OpenWith,
        3 => Verb::Properties,
        4 => Verb::Pin,
        5 => Verb::Unpin,
        6 => Verb::Share,
        7 => Verb::InvokeModern(r.u32()?),
        8 => Verb::OpenInExplorer,
        v => return Err(FrameError::BadValue { field, value: u32::from(v) }),
    })
}

fn put_outcome(w: &mut Writer, o: MenuOutcome) {
    match o {
        MenuOutcome::Dismissed => w.u8(1),
        MenuOutcome::Invoked => w.u8(2),
        MenuOutcome::Intercepted(c) => {
            w.u8(3);
            w.u8(c as u8);
        }
        MenuOutcome::Failed(hr) => {
            w.u8(4);
            w.i32(hr);
        }
    }
}

fn get_outcome(r: &mut Reader<'_>, field: &'static str) -> Result<MenuOutcome, FrameError> {
    Ok(match r.u8()? {
        1 => MenuOutcome::Dismissed,
        2 => MenuOutcome::Invoked,
        3 => MenuOutcome::Intercepted(get_canon(r, field)?),
        4 => MenuOutcome::Failed(r.i32()?),
        v => return Err(FrameError::BadValue { field, value: u32::from(v) }),
    })
}

fn put_modern(w: &mut Writer, m: &ModernItem) {
    w.u32(m.id);
    w.str(&m.label);
    w.bool(m.has_sub);
}

fn get_modern(r: &mut Reader<'_>) -> Result<ModernItem, FrameError> {
    Ok(ModernItem {
        id: r.u32()?,
        label: get_str(r, "ModernItem.label", LABEL_MAX)?,
        has_sub: r.bool("ModernItem.has_sub")?,
    })
}

fn put_new_type(w: &mut Writer, t: &ShellNewType) {
    w.str(&t.ext);
    w.str(&t.label);
}

fn get_new_type(r: &mut Reader<'_>) -> Result<ShellNewType, FrameError> {
    Ok(ShellNewType { ext: get_ext(r, "ShellNewType.ext")?, label: get_str(r, "ShellNewType.label", LABEL_MAX)? })
}

fn put_type_name(w: &mut Writer, t: &TypeName) {
    w.str(&t.ext);
    w.str(&t.name);
}

fn get_type_name(r: &mut Reader<'_>) -> Result<TypeName, FrameError> {
    Ok(TypeName { ext: get_ext(r, "TypeName.ext")?, name: get_str(r, "TypeName.name", TYPE_NAME_MAX)? })
}

impl Codec for InstanceCmd {
    const PROTO: Proto = INSTANCE;
    fn kind(&self) -> u16 {
        match self {
            InstanceCmd::NewWindow(_) => 0x0301,
            InstanceCmd::OpenTab(_) => 0x0302,
            InstanceCmd::Select { .. } => 0x0303,
            InstanceCmd::Probe { .. } => 0x0304,
        }
    }
    fn put(&self, w: &mut Writer) {
        match self {
            InstanceCmd::NewWindow(at) | InstanceCmd::OpenTab(at) => put_location(w, at),
            InstanceCmd::Select { dir, item } => {
                put_path(w, dir);
                put_path(w, item);
            }
            InstanceCmd::Probe { path } => put_path(w, path),
        }
    }
    fn get(kind: u16, r: &mut Reader<'_>) -> Result<Self, FrameError> {
        Ok(match kind {
            0x0301 => InstanceCmd::NewWindow(get_location(r, "NewWindow.0")?),
            0x0302 => InstanceCmd::OpenTab(get_location(r, "OpenTab.0")?),
            0x0303 => InstanceCmd::Select { dir: get_path(r, "Select.dir")?, item: get_path(r, "Select.item")? },
            0x0304 => InstanceCmd::Probe { path: get_path(r, "Probe.path")? },
            k => return Err(FrameError::UnknownKind(k)),
        })
    }
}

impl Codec for OpReq {
    const PROTO: Proto = OPS;
    fn kind(&self) -> u16 {
        match self {
            OpReq::Attach { .. } => 0x0101,
            OpReq::Stage { .. } => 0x0102,
            OpReq::Copy { .. } => 0x0103,
            OpReq::Move { .. } => 0x0104,
            OpReq::Recycle { .. } => 0x0105,
            OpReq::DeletePermanent { .. } => 0x0106,
            OpReq::Rename { .. } => 0x0107,
            OpReq::NewFolder { .. } => 0x0108,
            OpReq::UndoStage { .. } => 0x0109,
            OpReq::Undo { .. } => 0x010A,
            OpReq::Cancel { .. } => 0x010B,
            OpReq::Pause { .. } => 0x010C,
            OpReq::BringForward { .. } => 0x010D,
            OpReq::ResumeRest { .. } => 0x010E,
            OpReq::AckDone { .. } => 0x010F,
            OpReq::Bye => 0x0110,
        }
    }
    fn put(&self, w: &mut Writer) {
        match self {
            OpReq::Attach { ui_pid, owner } => {
                w.u32(*ui_pid);
                w.u64(*owner);
            }
            OpReq::Stage { op, items } => {
                w.u32(op.0);
                put_list(w, items, put_item);
            }
            OpReq::Copy { op, dest, conflicts } | OpReq::Move { op, dest, conflicts } => {
                w.u32(op.0);
                put_path(w, dest);
                w.u8(*conflicts as u8);
            }
            OpReq::Recycle { op }
            | OpReq::DeletePermanent { op }
            | OpReq::Cancel { op }
            | OpReq::BringForward { op }
            | OpReq::AckDone { of: op } => w.u32(op.0),
            OpReq::Rename { op, item, to } => {
                w.u32(op.0);
                put_path(w, item);
                put_str(w, to);
            }
            OpReq::NewFolder { op, parent, name } => {
                w.u32(op.0);
                put_path(w, parent);
                put_str(w, name);
            }
            OpReq::UndoStage { op, entries } => {
                w.u32(op.0);
                put_list(w, entries, put_undo);
            }
            OpReq::Undo { op, of, kind, replaced } => {
                w.u32(op.0);
                w.u32(of.0);
                w.u8(*kind as u8);
                w.u32(*replaced);
            }
            OpReq::Pause { op, on } => {
                w.u32(op.0);
                w.bool(*on);
            }
            OpReq::ResumeRest { op, of } => {
                w.u32(op.0);
                w.u32(of.0);
            }
            OpReq::Bye => {}
        }
    }
    fn get(kind: u16, r: &mut Reader<'_>) -> Result<Self, FrameError> {
        Ok(match kind {
            0x0101 => OpReq::Attach { ui_pid: r.u32()?, owner: r.u64()? },
            0x0102 => {
                OpReq::Stage { op: op_id(r)?, items: get_list(r, "Stage.items", |r| get_item(r, "Stage.items"))? }
            }
            0x0103 => OpReq::Copy {
                op: op_id(r)?,
                dest: get_path(r, "Copy.dest")?,
                conflicts: get_conflicts(r, "Copy.conflicts")?,
            },
            0x0104 => OpReq::Move {
                op: op_id(r)?,
                dest: get_path(r, "Move.dest")?,
                conflicts: get_conflicts(r, "Move.conflicts")?,
            },
            0x0105 => OpReq::Recycle { op: op_id(r)? },
            0x0106 => OpReq::DeletePermanent { op: op_id(r)? },
            0x0107 => OpReq::Rename { op: op_id(r)?, item: get_path(r, "Rename.item")?, to: get_name(r, "Rename.to")? },
            0x0108 => OpReq::NewFolder {
                op: op_id(r)?,
                parent: get_path(r, "NewFolder.parent")?,
                name: get_name(r, "NewFolder.name")?,
            },
            0x0109 => OpReq::UndoStage { op: op_id(r)?, entries: get_list(r, "UndoStage.entries", get_undo)? },
            0x010A => {
                OpReq::Undo { op: op_id(r)?, of: op_id(r)?, kind: get_undo_kind(r, "Undo.kind")?, replaced: r.u32()? }
            }
            0x010B => OpReq::Cancel { op: op_id(r)? },
            0x010C => OpReq::Pause { op: op_id(r)?, on: r.bool("Pause.on")? },
            0x010D => OpReq::BringForward { op: op_id(r)? },
            0x010E => OpReq::ResumeRest { op: op_id(r)?, of: op_id(r)? },
            0x010F => OpReq::AckDone { of: op_id(r)? },
            0x0110 => OpReq::Bye,
            k => return Err(FrameError::UnknownKind(k)),
        })
    }
}

impl Codec for OpEvt {
    const PROTO: Proto = OPS;
    fn kind(&self) -> u16 {
        match self {
            OpEvt::Accepted { .. } => 0x0181,
            OpEvt::Started { .. } => 0x0182,
            OpEvt::Progress { .. } => 0x0183,
            OpEvt::NeedsInput { .. } => 0x0184,
            OpEvt::ItemsDone { .. } => 0x0185,
            OpEvt::ItemFailed { .. } => 0x0186,
            OpEvt::UndoPart { .. } => 0x0187,
            OpEvt::Finished { .. } => 0x0188,
            OpEvt::Refused { .. } => 0x0189,
            OpEvt::Snapshot { .. } => 0x018A,
            OpEvt::Pending { .. } => 0x018B,
            OpEvt::DoneWhileAway { .. } => 0x018C,
        }
    }
    fn put(&self, w: &mut Writer) {
        match self {
            OpEvt::Accepted { op } => w.u32(op.0),
            OpEvt::Started { op, kind, items, bytes } => {
                w.u32(op.0);
                w.u8(*kind as u8);
                w.u32(*items);
                w.u64(*bytes);
            }
            OpEvt::Progress { op, done, total, bps, current } => {
                w.u32(op.0);
                w.u64(*done);
                w.u64(*total);
                w.u64(*bps);
                put_str(w, current);
            }
            OpEvt::NeedsInput { op, on } => {
                w.u32(op.0);
                w.bool(*on);
            }
            OpEvt::ItemsDone { op, changes } => {
                w.u32(op.0);
                put_list(w, changes, put_change);
            }
            OpEvt::ItemFailed { op, item, hr } => {
                w.u32(op.0);
                put_path(w, item);
                w.i32(*hr);
            }
            OpEvt::UndoPart { op, entries } => {
                w.u32(op.0);
                put_list(w, entries, put_undo);
            }
            OpEvt::Finished { op, result, undo, replaced, items, failed } => {
                w.u32(op.0);
                w.u8(*result as u8);
                put_opt(w, undo, |w, k| w.u8(*k as u8));
                w.u32(*replaced);
                w.u32(*items);
                w.u32(*failed);
            }
            OpEvt::Refused { op, why, detail } => {
                w.u32(op.0);
                w.u8(*why as u8);
                put_str(w, detail);
            }
            OpEvt::Snapshot { running } => put_list(w, running, put_status),
            OpEvt::Pending { of, kind, done: a, total: b, dest }
            | OpEvt::DoneWhileAway { of, kind, items: a, failed: b, dest } => {
                w.u32(of.0);
                w.u8(*kind as u8);
                w.u32(*a);
                w.u32(*b);
                put_opt(w, dest, put_path);
            }
        }
    }
    fn get(kind: u16, r: &mut Reader<'_>) -> Result<Self, FrameError> {
        Ok(match kind {
            0x0181 => OpEvt::Accepted { op: op_id(r)? },
            0x0182 => OpEvt::Started {
                op: op_id(r)?,
                kind: get_op_kind(r, "Started.kind")?,
                items: r.u32()?,
                bytes: r.u64()?,
            },
            0x0183 => OpEvt::Progress {
                op: op_id(r)?,
                done: r.u64()?,
                total: r.u64()?,
                bps: r.u64()?,
                current: get_str(r, "Progress.current", CURRENT_MAX)?,
            },
            0x0184 => OpEvt::NeedsInput { op: op_id(r)?, on: r.bool("NeedsInput.on")? },
            0x0185 => OpEvt::ItemsDone { op: op_id(r)?, changes: get_list(r, "ItemsDone.changes", get_change)? },
            0x0186 => OpEvt::ItemFailed { op: op_id(r)?, item: get_path(r, "ItemFailed.item")?, hr: r.i32()? },
            0x0187 => OpEvt::UndoPart { op: op_id(r)?, entries: get_list(r, "UndoPart.entries", get_undo)? },
            0x0188 => OpEvt::Finished {
                op: op_id(r)?,
                result: get_op_result(r, "Finished.result")?,
                undo: get_opt(r, "Finished.undo", |r| get_undo_kind(r, "Finished.undo"))?,
                replaced: r.u32()?,
                items: r.u32()?,
                failed: r.u32()?,
            },
            0x0189 => OpEvt::Refused {
                op: op_id(r)?,
                why: get_refusal(r, "Refused.why")?,
                detail: get_str(r, "Refused.detail", DETAIL_MAX)?,
            },
            0x018A => OpEvt::Snapshot { running: get_list(r, "Snapshot.running", get_status)? },
            0x018B => OpEvt::Pending {
                of: op_id(r)?,
                kind: get_op_kind(r, "Pending.kind")?,
                done: r.u32()?,
                total: r.u32()?,
                dest: get_opt(r, "Pending.dest", |r| get_path(r, "Pending.dest"))?,
            },
            0x018C => OpEvt::DoneWhileAway {
                of: op_id(r)?,
                kind: get_op_kind(r, "DoneWhileAway.kind")?,
                items: r.u32()?,
                failed: r.u32()?,
                dest: get_opt(r, "DoneWhileAway.dest", |r| get_path(r, "DoneWhileAway.dest"))?,
            },
            k => return Err(FrameError::UnknownKind(k)),
        })
    }
}

impl Codec for ShellReq {
    const PROTO: Proto = SHELL;
    fn kind(&self) -> u16 {
        match self {
            ShellReq::Menu { .. } => 0x0201,
            ShellReq::ModernItems { .. } => 0x0202,
            ShellReq::Exec { .. } => 0x0203,
            ShellReq::RunTyped { .. } => 0x0204,
            ShellReq::Icons { .. } => 0x0205,
            ShellReq::Thumbs { .. } => 0x0206,
            ShellReq::ClassIcons { .. } => 0x0207,
            ShellReq::ShellNewTypes { .. } => 0x0208,
            ShellReq::QuickAccessPins { .. } => 0x0209,
            ShellReq::CancelGen { .. } => 0x020A,
            ShellReq::Bye => 0x020B,
            ShellReq::TypeNames { .. } => 0x020C,
        }
    }
    fn put(&self, w: &mut Writer) {
        match self {
            ShellReq::Menu { req, items, at, extended, owner } => {
                w.u32(req.0);
                put_list(w, items, put_item);
                w.i32(at.0);
                w.i32(at.1);
                w.bool(*extended);
                w.u64(*owner);
            }
            ShellReq::ModernItems { req, items } => {
                w.u32(req.0);
                put_list(w, items, put_item);
            }
            ShellReq::Exec { req, verb, items } => {
                w.u32(req.0);
                put_verb(w, *verb);
                put_list(w, items, put_item);
            }
            ShellReq::RunTyped { req, text, dir } => {
                w.u32(req.0);
                put_str(w, text);
                put_path(w, dir);
            }
            ShellReq::Icons { req, gen, items, px } => {
                w.u32(req.0);
                w.u64(gen.0);
                put_list(w, items, put_item);
                w.u16(*px);
            }
            ShellReq::Thumbs { req, gen, items, px, cache_only } => {
                w.u32(req.0);
                w.u64(gen.0);
                put_list(w, items, put_item);
                w.u16(*px);
                w.bool(*cache_only);
            }
            ShellReq::ClassIcons { req, exts, px } => {
                w.u32(req.0);
                put_list(w, exts, |w, e| put_str(w, e));
                w.u16(*px);
            }
            ShellReq::ShellNewTypes { req } | ShellReq::QuickAccessPins { req } => w.u32(req.0),
            ShellReq::CancelGen { gen } => w.u64(gen.0),
            ShellReq::Bye => {}
            ShellReq::TypeNames { req, exts } => {
                w.u32(req.0);
                put_list(w, exts, |w, e| put_str(w, e));
            }
        }
    }
    fn get(kind: u16, r: &mut Reader<'_>) -> Result<Self, FrameError> {
        Ok(match kind {
            0x0201 => ShellReq::Menu {
                req: req_id(r)?,
                items: get_list(r, "Menu.items", |r| get_item(r, "Menu.items"))?,
                at: (r.i32()?, r.i32()?),
                extended: r.bool("Menu.extended")?,
                owner: r.u64()?,
            },
            0x0202 => ShellReq::ModernItems {
                req: req_id(r)?,
                items: get_list(r, "ModernItems.items", |r| get_item(r, "ModernItems.items"))?,
            },
            0x0203 => ShellReq::Exec {
                req: req_id(r)?,
                verb: get_verb(r, "Exec.verb")?,
                items: get_list(r, "Exec.items", |r| get_item(r, "Exec.items"))?,
            },
            0x0204 => ShellReq::RunTyped {
                req: req_id(r)?,
                text: get_str(r, "RunTyped.text", TEXT_MAX)?,
                dir: get_path(r, "RunTyped.dir")?,
            },
            0x0205 => ShellReq::Icons {
                req: req_id(r)?,
                gen: gen(r)?,
                items: get_list(r, "Icons.items", |r| get_item(r, "Icons.items"))?,
                px: get_px(r, "Icons.px")?,
            },
            0x0206 => ShellReq::Thumbs {
                req: req_id(r)?,
                gen: gen(r)?,
                items: get_list(r, "Thumbs.items", |r| get_item(r, "Thumbs.items"))?,
                px: get_px(r, "Thumbs.px")?,
                cache_only: r.bool("Thumbs.cache_only")?,
            },
            0x0207 => ShellReq::ClassIcons {
                req: req_id(r)?,
                exts: get_list(r, "ClassIcons.exts", |r| get_ext(r, "ClassIcons.exts"))?,
                px: get_px(r, "ClassIcons.px")?,
            },
            0x0208 => ShellReq::ShellNewTypes { req: req_id(r)? },
            0x0209 => ShellReq::QuickAccessPins { req: req_id(r)? },
            0x020A => ShellReq::CancelGen { gen: gen(r)? },
            0x020B => ShellReq::Bye,
            0x020C => ShellReq::TypeNames {
                req: req_id(r)?,
                exts: get_list_upto(r, "TypeNames.exts", TYPE_EXTS_MAX, |r| get_ext(r, "TypeNames.exts"))?,
            },
            k => return Err(FrameError::UnknownKind(k)),
        })
    }
}

impl Codec for ShellEvt {
    const PROTO: Proto = SHELL;
    fn kind(&self) -> u16 {
        match self {
            ShellEvt::Ready { .. } => 0x0281,
            ShellEvt::MenuShown { .. } => 0x0282,
            ShellEvt::MenuDone { .. } => 0x0283,
            ShellEvt::Modern { .. } => 0x0284,
            ShellEvt::ExecDone { .. } => 0x0285,
            ShellEvt::Icon { .. } => 0x0286,
            ShellEvt::Thumb { .. } => 0x0287,
            ShellEvt::ClassIcon { .. } => 0x0288,
            ShellEvt::ShellNew { .. } => 0x0289,
            ShellEvt::Pins { .. } => 0x028A,
            ShellEvt::Failed { .. } => 0x028B,
            ShellEvt::Busy { .. } => 0x028C,
            ShellEvt::ReqDone { .. } => 0x028D,
            ShellEvt::TypeNames { .. } => 0x028E,
        }
    }
    fn put(&self, w: &mut Writer) {
        match self {
            ShellEvt::Ready { pid } => w.u32(*pid),
            ShellEvt::MenuShown { req } | ShellEvt::ReqDone { req } => w.u32(req.0),
            ShellEvt::MenuDone { req, outcome, items } => {
                w.u32(req.0);
                put_outcome(w, *outcome);
                put_list(w, items, put_item);
            }
            ShellEvt::Modern { req, items, done } => {
                w.u32(req.0);
                put_list(w, items, put_modern);
                w.bool(*done);
            }
            ShellEvt::ExecDone { req, hr } | ShellEvt::Failed { req, hr } => {
                w.u32(req.0);
                w.i32(*hr);
            }
            ShellEvt::Icon { req, gen, index, px } | ShellEvt::Thumb { req, gen, index, px } => {
                w.u32(req.0);
                w.u64(gen.0);
                w.u32(*index);
                put_pixels(w, px);
            }
            ShellEvt::ClassIcon { req, ext, px } => {
                w.u32(req.0);
                put_str(w, ext);
                put_pixels(w, px);
            }
            ShellEvt::ShellNew { req, types } => {
                w.u32(req.0);
                put_list(w, types, put_new_type);
            }
            ShellEvt::Pins { req, pins } => {
                w.u32(req.0);
                put_list(w, pins, put_path);
            }
            ShellEvt::Busy { on } => w.bool(*on),
            ShellEvt::TypeNames { req, names } => {
                w.u32(req.0);
                put_list(w, names, put_type_name);
            }
        }
    }
    fn get(kind: u16, r: &mut Reader<'_>) -> Result<Self, FrameError> {
        Ok(match kind {
            0x0281 => ShellEvt::Ready { pid: r.u32()? },
            0x0282 => ShellEvt::MenuShown { req: req_id(r)? },
            0x0283 => ShellEvt::MenuDone {
                req: req_id(r)?,
                outcome: get_outcome(r, "MenuDone.outcome")?,
                items: get_list(r, "MenuDone.items", |r| get_item(r, "MenuDone.items"))?,
            },
            0x0284 => ShellEvt::Modern {
                req: req_id(r)?,
                items: get_list(r, "Modern.items", get_modern)?,
                done: r.bool("Modern.done")?,
            },
            0x0285 => ShellEvt::ExecDone { req: req_id(r)?, hr: r.i32()? },
            0x0286 => ShellEvt::Icon { req: req_id(r)?, gen: gen(r)?, index: r.u32()?, px: get_pixels(r, "Icon.px")? },
            0x0287 => {
                ShellEvt::Thumb { req: req_id(r)?, gen: gen(r)?, index: r.u32()?, px: get_pixels(r, "Thumb.px")? }
            }
            0x0288 => ShellEvt::ClassIcon {
                req: req_id(r)?,
                ext: get_ext(r, "ClassIcon.ext")?,
                px: get_pixels(r, "ClassIcon.px")?,
            },
            0x0289 => ShellEvt::ShellNew { req: req_id(r)?, types: get_list(r, "ShellNew.types", get_new_type)? },
            0x028A => ShellEvt::Pins { req: req_id(r)?, pins: get_list(r, "Pins.pins", |r| get_path(r, "Pins.pins"))? },
            0x028B => ShellEvt::Failed { req: req_id(r)?, hr: r.i32()? },
            0x028C => ShellEvt::Busy { on: r.bool("Busy.on")? },
            0x028D => ShellEvt::ReqDone { req: req_id(r)? },
            0x028E => {
                let req = req_id(r)?;
                let names = get_list(r, "TypeNames.names", get_type_name)?;
                // Never empty: an empty list would have Pins' bytes, one kind bit away (0x028A).
                if names.is_empty() {
                    return Err(FrameError::Invalid { field: "TypeNames.names" });
                }
                ShellEvt::TypeNames { req, names }
            }
            k => return Err(FrameError::UnknownKind(k)),
        })
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::indexing_slicing)]

    use super::*;

    fn w(s: &str) -> Vec<u16> {
        s.encode_utf16().collect()
    }

    fn p(s: &str) -> WidePath {
        WidePath::parse(s).unwrap()
    }

    #[track_caller]
    fn ok(input: &str, want: &str) {
        assert_eq!(WidePath::parse(input).map(|p| p.to_string_lossy()), Ok(want.to_string()), "{input}");
    }

    #[track_caller]
    fn err(input: &str, e: PathError) {
        assert_eq!(WidePath::parse(input), Err(e), "{input}");
    }

    #[test]
    fn widepath_table() {
        ok(r"C:\", r"C:\");
        ok(r"c:\a\b\", r"c:\a\b");
        ok(r"\\?\C:\x", r"C:\x");
        ok(r"\\?\c:\", r"c:\");
        ok(r"\\?\UNC\srv\sh\x", r"\\srv\sh\x");
        ok("C:/a/b", r"C:\a\b");
        ok(r"C:\a\\b\\\", r"C:\a\b");
        ok(r"\\srv\sh", r"\\srv\sh");
        ok(r"\\srv\sh\", r"\\srv\sh");
        ok(r"C:\Users\Mixed Case\File.TXT", r"C:\Users\Mixed Case\File.TXT");
        err("", PathError::Empty);
        err(r"a\b", PathError::NotAbsolute);
        err("C:a", PathError::NotAbsolute);
        err("C:", PathError::NotAbsolute);
        err(r"\a", PathError::NotAbsolute);
        err(r"\\srv", PathError::NotAbsolute);
        err(r"\\srv\", PathError::NotAbsolute);
        err(r"\\?\pipe\x", PathError::NotAbsolute);
        err(r"1:\x", PathError::NotAbsolute);
        err(r"\\.\pipe\x", PathError::Device);
        err("//./pipe/x", PathError::Device);
        err(r"\\.", PathError::Device);
        err(r"\\?\GLOBALROOT\Device", PathError::Device);
        err(r"\\?\globalroot\Device\x", PathError::Device);
        err(r"\\?\Volume{01234567-89ab-cdef-0123-456789abcdef}\x", PathError::Device);
        err(r"\\?\UNC\.\x", PathError::Device);
        err(r"C:\a\..\b", PathError::DotComponent);
        err(r"C:\a\.\b", PathError::DotComponent);
        err(r"C:\a\..", PathError::DotComponent);
        err(r"\\srv\..\x", PathError::DotComponent);
        err("C:\\a\0b", PathError::Control);
        err("C:\\a\u{1}b", PathError::Control);
        err("C:\\a\u{1f}b", PathError::Control);
        let mut long = w(r"C:\");
        long.extend(std::iter::repeat_n(u16::from(b'a'), 32_764));
        assert_eq!(long.len(), 32_767);
        assert_eq!(WidePath::new(&long).map(|p| p.units().len()), Ok(32_767), "exactly 32,767 units");
        long.push(u16::from(b'a'));
        assert_eq!(WidePath::new(&long), Err(PathError::TooLong), "32,768 units");
        let mut lone = w(r"C:\x");
        lone.push(0xD800);
        assert_eq!(WidePath::new(&lone).unwrap().units(), &lone[..], "a lone surrogate is kept");
    }

    #[test]
    fn widepath_parts() {
        assert_eq!(p(r"\\srv\sh\x").raw(), w("\\\\?\\UNC\\srv\\sh\\x\0"));
        assert_eq!(p(r"\\srv\sh").raw(), w("\\\\?\\UNC\\srv\\sh\0"));
        assert_eq!(p(r"C:\x").raw(), w("\\\\?\\C:\\x\0"));
        assert_eq!(p(r"C:\").raw(), w("\\\\?\\C:\\\0"));
        assert!(p(r"\\srv\sh").is_unc() && p(r"\\srv\sh").is_root());
        assert!(!p(r"\\srv\sh\x").is_root() && p(r"C:\").is_root() && !p(r"C:\a").is_root());
        assert!(!p(r"C:\a").is_unc());
        assert_eq!(p(r"C:\a").parent(), Some(p(r"C:\")));
        assert_eq!(p(r"C:\a\b").parent(), Some(p(r"C:\a")));
        assert_eq!(p(r"C:\").parent(), None);
        assert_eq!(p(r"\\srv\sh").parent(), None);
        assert_eq!(p(r"\\srv\sh\x").parent(), Some(p(r"\\srv\sh")));
        assert_eq!(p(r"\\srv\sh\x\y").parent(), Some(p(r"\\srv\sh\x")));
        assert_eq!(p(r"C:\a\b.txt").file_name(), Some(&w("b.txt")[..]));
        assert_eq!(p(r"\\srv\sh\x").file_name(), Some(&w("x")[..]));
        assert_eq!(p(r"C:\").file_name(), None);
        assert_eq!(p(r"\\srv\sh").file_name(), None);
        assert_eq!(p(r"C:\").join(&w("a")), Ok(p(r"C:\a")));
        assert_eq!(p(r"C:\a").join(&w("b.txt")), Ok(p(r"C:\a\b.txt")));
        assert_eq!(p(r"\\srv\sh").join(&w("x")), Ok(p(r"\\srv\sh\x")));
        assert_eq!(p(r"C:\a").join(&w(r"b\c")), Err(PathError::NotAbsolute));
        assert_eq!(p(r"C:\a").join(&w("b/c")), Err(PathError::NotAbsolute));
        assert_eq!(p(r"C:\a").join(&w("..")), Err(PathError::DotComponent));
        assert_eq!(p(r"C:\a").join(&w(".")), Err(PathError::DotComponent));
        assert_eq!(p(r"C:\a").join(&[]), Err(PathError::Empty));
        assert_eq!(p(r"C:\a").join(&w("b\0")), Err(PathError::Control));
        let mut long = w(r"C:\");
        long.extend(std::iter::repeat_n(u16::from(b'a'), 32_764));
        assert_eq!(WidePath::new(&long).unwrap().join(&w("b")), Err(PathError::TooLong));
        assert_eq!(WidePath::parse(r"C:\x"), WidePath::new(&w(r"C:\x")));
    }

    #[test]
    fn wire_len_by_hand() {
        let a = p(r"C:\a"); // 4 units: 2 + 8
        assert_eq!(a.wire_len(), 10);
        assert_eq!(Box::<str>::from("abc").wire_len(), 7);
        assert_eq!(ItemRef::Path(a.clone()).wire_len(), 11);
        assert_eq!(ItemRef::Pidl(vec![1u8, 2, 3].into()).wire_len(), 1 + 4 + 3);
        let e = UndoEntry { from: a.clone(), to: p(r"C:\b"), size: 1, mtime: 2, dir: false, bin: None };
        assert_eq!(e.wire_len(), 10 + 10 + 8 + 8 + 1 + 1);
        let e = UndoEntry { bin: Some(p(r"C:\c")), ..e };
        assert_eq!(e.wire_len(), 38 + 10);
        let c = Change { kind: ChangeKind::Copied, from: Some(a.clone()), to: None };
        assert_eq!(c.wire_len(), 1 + 11 + 1);
        assert_eq!(ModernItem { id: 7, label: "Open".into(), has_sub: false }.wire_len(), 4 + 8 + 1);
        assert_eq!(ShellNewType { ext: "txt".into(), label: "Text Document".into() }.wire_len(), 7 + 17);
        assert_eq!(TypeName { ext: "txt".into(), name: "Text Document".into() }.wire_len(), 7 + 17);
        // The largest single element of the files protocols, and it fits alone.
        let mut deep = w(r"C:\");
        deep.extend(std::iter::repeat_n(u16::from(b'a'), 32_764));
        let deep = WidePath::new(&deep).unwrap();
        let big = UndoEntry { from: deep.clone(), to: deep.clone(), size: 0, mtime: 0, dir: true, bin: Some(deep) };
        assert_eq!(big.wire_len(), 196_626);
        let parts = super::super::split_by_bytes(std::slice::from_ref(&big), 64, &OPS, MAX_LIST);
        assert_eq!(parts.len(), 1);
        assert!(64 + 4 + big.wire_len() <= (OPS.max_payload - super::super::SPLIT_MARGIN) as usize);
    }

    /// Each message enum's variants, matched once: a variant added or removed without this table fails to compile.
    /// The values are the ones written on the variants; FR4 pins them through kind().
    const fn instance_kind(m: &InstanceCmd) -> u16 {
        match m {
            InstanceCmd::NewWindow(_) => 0x0301,
            InstanceCmd::OpenTab(_) => 0x0302,
            InstanceCmd::Select { .. } => 0x0303,
            InstanceCmd::Probe { .. } => 0x0304,
        }
    }

    const fn op_req_kind(m: &OpReq) -> u16 {
        match m {
            OpReq::Attach { .. } => 0x0101,
            OpReq::Stage { .. } => 0x0102,
            OpReq::Copy { .. } => 0x0103,
            OpReq::Move { .. } => 0x0104,
            OpReq::Recycle { .. } => 0x0105,
            OpReq::DeletePermanent { .. } => 0x0106,
            OpReq::Rename { .. } => 0x0107,
            OpReq::NewFolder { .. } => 0x0108,
            OpReq::UndoStage { .. } => 0x0109,
            OpReq::Undo { .. } => 0x010A,
            OpReq::Cancel { .. } => 0x010B,
            OpReq::Pause { .. } => 0x010C,
            OpReq::BringForward { .. } => 0x010D,
            OpReq::ResumeRest { .. } => 0x010E,
            OpReq::AckDone { .. } => 0x010F,
            OpReq::Bye => 0x0110,
        }
    }

    const fn op_evt_kind(m: &OpEvt) -> u16 {
        match m {
            OpEvt::Accepted { .. } => 0x0181,
            OpEvt::Started { .. } => 0x0182,
            OpEvt::Progress { .. } => 0x0183,
            OpEvt::NeedsInput { .. } => 0x0184,
            OpEvt::ItemsDone { .. } => 0x0185,
            OpEvt::ItemFailed { .. } => 0x0186,
            OpEvt::UndoPart { .. } => 0x0187,
            OpEvt::Finished { .. } => 0x0188,
            OpEvt::Refused { .. } => 0x0189,
            OpEvt::Snapshot { .. } => 0x018A,
            OpEvt::Pending { .. } => 0x018B,
            OpEvt::DoneWhileAway { .. } => 0x018C,
        }
    }

    const fn shell_req_kind(m: &ShellReq) -> u16 {
        match m {
            ShellReq::Menu { .. } => 0x0201,
            ShellReq::ModernItems { .. } => 0x0202,
            ShellReq::Exec { .. } => 0x0203,
            ShellReq::RunTyped { .. } => 0x0204,
            ShellReq::Icons { .. } => 0x0205,
            ShellReq::Thumbs { .. } => 0x0206,
            ShellReq::ClassIcons { .. } => 0x0207,
            ShellReq::ShellNewTypes { .. } => 0x0208,
            ShellReq::QuickAccessPins { .. } => 0x0209,
            ShellReq::CancelGen { .. } => 0x020A,
            ShellReq::Bye => 0x020B,
            ShellReq::TypeNames { .. } => 0x020C,
        }
    }

    const fn shell_evt_kind(m: &ShellEvt) -> u16 {
        match m {
            ShellEvt::Ready { .. } => 0x0281,
            ShellEvt::MenuShown { .. } => 0x0282,
            ShellEvt::MenuDone { .. } => 0x0283,
            ShellEvt::Modern { .. } => 0x0284,
            ShellEvt::ExecDone { .. } => 0x0285,
            ShellEvt::Icon { .. } => 0x0286,
            ShellEvt::Thumb { .. } => 0x0287,
            ShellEvt::ClassIcon { .. } => 0x0288,
            ShellEvt::ShellNew { .. } => 0x0289,
            ShellEvt::Pins { .. } => 0x028A,
            ShellEvt::Failed { .. } => 0x028B,
            ShellEvt::Busy { .. } => 0x028C,
            ShellEvt::ReqDone { .. } => 0x028D,
            ShellEvt::TypeNames { .. } => 0x028E,
        }
    }

    #[test]
    fn frame_types_have_their_layout() {
        assert_eq!(std::mem::size_of::<Gen>(), 8);
        assert_eq!(std::mem::size_of::<OpId>(), 4);
        assert_eq!(std::mem::size_of::<ReqId>(), 4);
        assert_eq!(OPS.magic.to_le_bytes(), *b"FOPS");
        assert_eq!(SHELL.magic.to_le_bytes(), *b"FSHL");
        assert_eq!(INSTANCE.magic.to_le_bytes(), *b"FINS");
        assert_eq!((OPS.version, OPS.max_payload, INSTANCE.max_payload), (1, 1 << 20, 64 << 10));
        assert_eq!((SHELL.version, INSTANCE.version, SHELL.max_payload), (2, 1, 1 << 20), "type names made SHELL 2");
        assert_eq!(
            (MAX_PATH_UNITS, MAX_NAME_UNITS, MAX_LIST, UNDO_MAX_ENTRIES, MAX_PIXELS_SIDE),
            (32_767, 255, 2_048, 16_384, 480)
        );
        // Message enums: one sample per enum through the const table, inside each enum's range.
        let op = OpId(1);
        assert_eq!(instance_kind(&InstanceCmd::NewWindow(Location::Home)), 0x0301);
        assert_eq!(op_req_kind(&OpReq::DeletePermanent { op }), 0x0106);
        assert_eq!(op_req_kind(&OpReq::Bye), 0x0110);
        assert_eq!(op_evt_kind(&OpEvt::Accepted { op }), 0x0181);
        assert_eq!(shell_req_kind(&ShellReq::Bye), 0x020B);
        assert_eq!(shell_evt_kind(&ShellEvt::Busy { on: true }), 0x028C);
        assert_eq!(shell_req_kind(&ShellReq::TypeNames { req: ReqId(1), exts: vec![] }), 0x020C);
        assert_eq!(shell_evt_kind(&ShellEvt::TypeNames { req: ReqId(1), names: vec![] }), 0x028E);
        // The fieldless enums carry their values directly.
        assert_eq!([Conflicts::AskSystem as u8, Conflicts::ReplaceAll as u8, Conflicts::SkipAll as u8], [0, 1, 2]);
        let kinds = [
            OpKind::Copy,
            OpKind::Move,
            OpKind::Recycle,
            OpKind::DeletePermanent,
            OpKind::Rename,
            OpKind::NewFolder,
            OpKind::Undo,
            OpKind::Resume,
        ];
        assert_eq!(kinds.map(|k| k as u8), [1, 2, 3, 4, 5, 6, 7, 8]);
        let undo = [
            UndoKind::RenameBack,
            UndoKind::MoveBack,
            UndoKind::RecycleCopies,
            UndoKind::RecycleNewFolder,
            UndoKind::RestoreRecycled,
            UndoKind::OpenRecycleBin,
        ];
        assert_eq!(undo.map(|k| k as u8), [1, 2, 3, 4, 5, 6]);
        let changes = [
            ChangeKind::Copied,
            ChangeKind::Moved,
            ChangeKind::Renamed,
            ChangeKind::Recycled,
            ChangeKind::Deleted,
            ChangeKind::Created,
        ];
        assert_eq!(changes.map(|k| k as u8), [1, 2, 3, 4, 5, 6]);
        assert_eq!(
            [OpResult::Done, OpResult::Cancelled, OpResult::Failed, OpResult::Refused].map(|k| k as u8),
            [0, 1, 2, 3]
        );
        let refusals = [
            Refusal::BadPath,
            Refusal::Root,
            Refusal::OutsideTestRoot,
            Refusal::NoItems,
            Refusal::UnknownOp,
            Refusal::OpInUse,
            Refusal::Changed,
            Refusal::Replaced,
            Refusal::NotEmpty,
            Refusal::PauseUnsupported,
            Refusal::BadName,
            Refusal::Elevated,
            Refusal::MixedParents,
            Refusal::TooMany,
        ];
        assert_eq!(refusals.map(|k| k as u8), [1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14]);
        let canon = [
            CanonVerb::Delete,
            CanonVerb::Cut,
            CanonVerb::Copy,
            CanonVerb::Paste,
            CanonVerb::PasteLink,
            CanonVerb::Rename,
        ];
        assert_eq!(canon.map(|k| k as u8), [1, 2, 3, 4, 5, 6]);
        let calls = [
            CallKind::QueryContextMenu,
            CallKind::HandleMenuMsg,
            CallKind::InvokeCommand,
            CallKind::ModernItems,
            CallKind::Exec,
            CallKind::Icon,
            CallKind::Thumb,
            CallKind::ClassIcon,
            CallKind::ShellNew,
            CallKind::Pins,
            CallKind::VerbName,
        ];
        assert_eq!(calls.map(|k| k as u16), [1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11]);
        // The kinds DeletePermanent shares its payload shape (a bare OpId) with stay two bits or more away (D6).
        let same_shape = [0x0105u16, 0x010B, 0x010D, 0x010F];
        assert!(same_shape.iter().all(|k| (k ^ 0x0106).count_ones() >= 2));
    }
}
