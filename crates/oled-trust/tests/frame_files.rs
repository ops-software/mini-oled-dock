//! The files protocols' codecs (P1-files-engine FR4, FR5): every kind pinned, round trips, the distance between
//! Recycle and DeletePermanent, the field caps, the list frames that senders split by bytes, the frozen v1 bytes and
//! fuzz.

use std::collections::BTreeSet;
use std::sync::{Mutex, MutexGuard, PoisonError};

use oled_trust::frame::files::*;
use oled_trust::frame::wire::{Reader, Writer};
use oled_trust::frame::{
    decode, decode_hello, encode, encode_hello, split_by_bytes, Assembler, Codec, FrameError, Hello, Proto, WireLen,
    HEADER_LEN, SPLIT_MARGIN,
};

/// Every variant of the five message enums with its kind, as P1-files-engine 2.2 pins it. A value here never
/// changes and is never reused: a new variant takes the next free value and bumps the protocol version.
const KINDS: &[(&str, u16)] = &[
    ("InstanceCmd::NewWindow", 0x0301),
    ("InstanceCmd::OpenTab", 0x0302),
    ("InstanceCmd::Select", 0x0303),
    ("InstanceCmd::Probe", 0x0304),
    ("OpReq::Attach", 0x0101),
    ("OpReq::Stage", 0x0102),
    ("OpReq::Copy", 0x0103),
    ("OpReq::Move", 0x0104),
    ("OpReq::Recycle", 0x0105),
    ("OpReq::DeletePermanent", 0x0106),
    ("OpReq::Rename", 0x0107),
    ("OpReq::NewFolder", 0x0108),
    ("OpReq::UndoStage", 0x0109),
    ("OpReq::Undo", 0x010A),
    ("OpReq::Cancel", 0x010B),
    ("OpReq::Pause", 0x010C),
    ("OpReq::BringForward", 0x010D),
    ("OpReq::ResumeRest", 0x010E),
    ("OpReq::AckDone", 0x010F),
    ("OpReq::Bye", 0x0110),
    ("OpEvt::Accepted", 0x0181),
    ("OpEvt::Started", 0x0182),
    ("OpEvt::Progress", 0x0183),
    ("OpEvt::NeedsInput", 0x0184),
    ("OpEvt::ItemsDone", 0x0185),
    ("OpEvt::ItemFailed", 0x0186),
    ("OpEvt::UndoPart", 0x0187),
    ("OpEvt::Finished", 0x0188),
    ("OpEvt::Refused", 0x0189),
    ("OpEvt::Snapshot", 0x018A),
    ("OpEvt::Pending", 0x018B),
    ("OpEvt::DoneWhileAway", 0x018C),
    ("ShellReq::Menu", 0x0201),
    ("ShellReq::ModernItems", 0x0202),
    ("ShellReq::Exec", 0x0203),
    ("ShellReq::RunTyped", 0x0204),
    ("ShellReq::Icons", 0x0205),
    ("ShellReq::Thumbs", 0x0206),
    ("ShellReq::ClassIcons", 0x0207),
    ("ShellReq::ShellNewTypes", 0x0208),
    ("ShellReq::QuickAccessPins", 0x0209),
    ("ShellReq::CancelGen", 0x020A),
    ("ShellReq::Bye", 0x020B),
    ("ShellReq::TypeNames", 0x020C),
    ("ShellEvt::Ready", 0x0281),
    ("ShellEvt::MenuShown", 0x0282),
    ("ShellEvt::MenuDone", 0x0283),
    ("ShellEvt::Modern", 0x0284),
    ("ShellEvt::ExecDone", 0x0285),
    ("ShellEvt::Icon", 0x0286),
    ("ShellEvt::Thumb", 0x0287),
    ("ShellEvt::ClassIcon", 0x0288),
    ("ShellEvt::ShellNew", 0x0289),
    ("ShellEvt::Pins", 0x028A),
    ("ShellEvt::Failed", 0x028B),
    ("ShellEvt::Busy", 0x028C),
    ("ShellEvt::ReqDone", 0x028D),
    ("ShellEvt::TypeNames", 0x028E),
];

/// A message of any of the five enums.
#[derive(Clone, Debug, PartialEq, Eq)]
enum Msg {
    Instance(InstanceCmd),
    OpReq(OpReq),
    OpEvt(OpEvt),
    ShellReq(ShellReq),
    ShellEvt(ShellEvt),
}

type Decoder = fn(&[u8]) -> Result<(Msg, usize), FrameError>;

fn dec_instance(b: &[u8]) -> Result<(Msg, usize), FrameError> {
    decode::<InstanceCmd>(b).map(|(m, n)| (Msg::Instance(m), n))
}
fn dec_op_req(b: &[u8]) -> Result<(Msg, usize), FrameError> {
    decode::<OpReq>(b).map(|(m, n)| (Msg::OpReq(m), n))
}
fn dec_op_evt(b: &[u8]) -> Result<(Msg, usize), FrameError> {
    decode::<OpEvt>(b).map(|(m, n)| (Msg::OpEvt(m), n))
}
fn dec_shell_req(b: &[u8]) -> Result<(Msg, usize), FrameError> {
    decode::<ShellReq>(b).map(|(m, n)| (Msg::ShellReq(m), n))
}
fn dec_shell_evt(b: &[u8]) -> Result<(Msg, usize), FrameError> {
    decode::<ShellEvt>(b).map(|(m, n)| (Msg::ShellEvt(m), n))
}

/// Whether C's decoder knows a kind: anything but UnknownKind on an empty payload.
fn knows<C: Codec>(kind: u16) -> bool {
    !matches!(C::get(kind, &mut Reader::new(&[])), Err(FrameError::UnknownKind(_)))
}

/// An enum: name, kind range (2.2), decoder, and whether the decoder knows a kind.
type EnumRow = (&'static str, u16, u16, Decoder, fn(u16) -> bool);

const ENUMS: [EnumRow; 5] = [
    ("InstanceCmd", 0x0301, 0x03FF, dec_instance, knows::<InstanceCmd>),
    ("OpReq", 0x0101, 0x017F, dec_op_req, knows::<OpReq>),
    ("OpEvt", 0x0181, 0x01FF, dec_op_evt, knows::<OpEvt>),
    ("ShellReq", 0x0201, 0x027F, dec_shell_req, knows::<ShellReq>),
    ("ShellEvt", 0x0281, 0x02FF, dec_shell_evt, knows::<ShellEvt>),
];

impl Msg {
    fn enum_name(&self) -> &'static str {
        match self {
            Msg::Instance(_) => "InstanceCmd",
            Msg::OpReq(_) => "OpReq",
            Msg::OpEvt(_) => "OpEvt",
            Msg::ShellReq(_) => "ShellReq",
            Msg::ShellEvt(_) => "ShellEvt",
        }
    }

    /// The row of KINDS: the matches are exhaustive, so a new variant does not compile until it is named here.
    fn name(&self) -> &'static str {
        match self {
            Msg::Instance(m) => match m {
                InstanceCmd::NewWindow(_) => "InstanceCmd::NewWindow",
                InstanceCmd::OpenTab(_) => "InstanceCmd::OpenTab",
                InstanceCmd::Select { .. } => "InstanceCmd::Select",
                InstanceCmd::Probe { .. } => "InstanceCmd::Probe",
            },
            Msg::OpReq(m) => match m {
                OpReq::Attach { .. } => "OpReq::Attach",
                OpReq::Stage { .. } => "OpReq::Stage",
                OpReq::Copy { .. } => "OpReq::Copy",
                OpReq::Move { .. } => "OpReq::Move",
                OpReq::Recycle { .. } => "OpReq::Recycle",
                OpReq::DeletePermanent { .. } => "OpReq::DeletePermanent",
                OpReq::Rename { .. } => "OpReq::Rename",
                OpReq::NewFolder { .. } => "OpReq::NewFolder",
                OpReq::UndoStage { .. } => "OpReq::UndoStage",
                OpReq::Undo { .. } => "OpReq::Undo",
                OpReq::Cancel { .. } => "OpReq::Cancel",
                OpReq::Pause { .. } => "OpReq::Pause",
                OpReq::BringForward { .. } => "OpReq::BringForward",
                OpReq::ResumeRest { .. } => "OpReq::ResumeRest",
                OpReq::AckDone { .. } => "OpReq::AckDone",
                OpReq::Bye => "OpReq::Bye",
            },
            Msg::OpEvt(m) => match m {
                OpEvt::Accepted { .. } => "OpEvt::Accepted",
                OpEvt::Started { .. } => "OpEvt::Started",
                OpEvt::Progress { .. } => "OpEvt::Progress",
                OpEvt::NeedsInput { .. } => "OpEvt::NeedsInput",
                OpEvt::ItemsDone { .. } => "OpEvt::ItemsDone",
                OpEvt::ItemFailed { .. } => "OpEvt::ItemFailed",
                OpEvt::UndoPart { .. } => "OpEvt::UndoPart",
                OpEvt::Finished { .. } => "OpEvt::Finished",
                OpEvt::Refused { .. } => "OpEvt::Refused",
                OpEvt::Snapshot { .. } => "OpEvt::Snapshot",
                OpEvt::Pending { .. } => "OpEvt::Pending",
                OpEvt::DoneWhileAway { .. } => "OpEvt::DoneWhileAway",
            },
            Msg::ShellReq(m) => match m {
                ShellReq::Menu { .. } => "ShellReq::Menu",
                ShellReq::ModernItems { .. } => "ShellReq::ModernItems",
                ShellReq::Exec { .. } => "ShellReq::Exec",
                ShellReq::RunTyped { .. } => "ShellReq::RunTyped",
                ShellReq::Icons { .. } => "ShellReq::Icons",
                ShellReq::Thumbs { .. } => "ShellReq::Thumbs",
                ShellReq::ClassIcons { .. } => "ShellReq::ClassIcons",
                ShellReq::ShellNewTypes { .. } => "ShellReq::ShellNewTypes",
                ShellReq::QuickAccessPins { .. } => "ShellReq::QuickAccessPins",
                ShellReq::CancelGen { .. } => "ShellReq::CancelGen",
                ShellReq::Bye => "ShellReq::Bye",
                ShellReq::TypeNames { .. } => "ShellReq::TypeNames",
            },
            Msg::ShellEvt(m) => match m {
                ShellEvt::Ready { .. } => "ShellEvt::Ready",
                ShellEvt::MenuShown { .. } => "ShellEvt::MenuShown",
                ShellEvt::MenuDone { .. } => "ShellEvt::MenuDone",
                ShellEvt::Modern { .. } => "ShellEvt::Modern",
                ShellEvt::ExecDone { .. } => "ShellEvt::ExecDone",
                ShellEvt::Icon { .. } => "ShellEvt::Icon",
                ShellEvt::Thumb { .. } => "ShellEvt::Thumb",
                ShellEvt::ClassIcon { .. } => "ShellEvt::ClassIcon",
                ShellEvt::ShellNew { .. } => "ShellEvt::ShellNew",
                ShellEvt::Pins { .. } => "ShellEvt::Pins",
                ShellEvt::Failed { .. } => "ShellEvt::Failed",
                ShellEvt::Busy { .. } => "ShellEvt::Busy",
                ShellEvt::ReqDone { .. } => "ShellEvt::ReqDone",
                ShellEvt::TypeNames { .. } => "ShellEvt::TypeNames",
            },
        }
    }

    fn kind(&self) -> u16 {
        match self {
            Msg::Instance(m) => m.kind(),
            Msg::OpReq(m) => m.kind(),
            Msg::OpEvt(m) => m.kind(),
            Msg::ShellReq(m) => m.kind(),
            Msg::ShellEvt(m) => m.kind(),
        }
    }

    fn proto(&self) -> Proto {
        match self {
            Msg::Instance(_) => InstanceCmd::PROTO,
            Msg::OpReq(_) => OpReq::PROTO,
            Msg::OpEvt(_) => OpEvt::PROTO,
            Msg::ShellReq(_) => ShellReq::PROTO,
            Msg::ShellEvt(_) => ShellEvt::PROTO,
        }
    }

    fn encode(&self, out: &mut Vec<u8>) -> Result<(), FrameError> {
        match self {
            Msg::Instance(m) => encode(m, out),
            Msg::OpReq(m) => encode(m, out),
            Msg::OpEvt(m) => encode(m, out),
            Msg::ShellReq(m) => encode(m, out),
            Msg::ShellEvt(m) => encode(m, out),
        }
    }

    fn frame(&self) -> Vec<u8> {
        let mut out = Vec::new();
        self.encode(&mut out).unwrap_or_else(|e| panic!("{}: {e:?}", self.name()));
        out
    }

    /// Decodes with the codec of this message's own enum.
    fn decode_as(&self, buf: &[u8]) -> Result<(Msg, usize), FrameError> {
        let name = self.enum_name();
        (ENUMS.iter().find(|e| e.0 == name).unwrap().3)(buf)
    }

    /// Decodes with the codec whose range holds the frame's kind.
    fn decode_any(buf: &[u8]) -> Result<(Msg, usize), FrameError> {
        let kind = u16::from_le_bytes([buf[4], buf[5]]);
        let e = ENUMS.iter().find(|e| (e.1..=e.2).contains(&kind)).ok_or(FrameError::UnknownKind(kind))?;
        (e.3)(buf)
    }
}

fn p(s: &str) -> WidePath {
    WidePath::parse(s).unwrap()
}

fn w(s: &str) -> Vec<u16> {
    s.encode_utf16().collect()
}

const OP_KINDS: [OpKind; 8] = [
    OpKind::Copy,
    OpKind::Move,
    OpKind::Recycle,
    OpKind::DeletePermanent,
    OpKind::Rename,
    OpKind::NewFolder,
    OpKind::Undo,
    OpKind::Resume,
];
const UNDO_KINDS: [UndoKind; 6] = [
    UndoKind::RenameBack,
    UndoKind::MoveBack,
    UndoKind::RecycleCopies,
    UndoKind::RecycleNewFolder,
    UndoKind::RestoreRecycled,
    UndoKind::OpenRecycleBin,
];
const REFUSALS: [Refusal; 14] = [
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
const VERBS: [Verb; 8] = [
    Verb::Open,
    Verb::OpenWith,
    Verb::Properties,
    Verb::Pin,
    Verb::Unpin,
    Verb::Share,
    Verb::InvokeModern(77),
    Verb::OpenInExplorer,
];
const CANON: [CanonVerb; 6] =
    [CanonVerb::Delete, CanonVerb::Cut, CanonVerb::Copy, CanonVerb::Paste, CanonVerb::PasteLink, CanonVerb::Rename];

/// One sample or more of every variant, with every value of every nested enum somewhere among them: the variants of
/// version 1, then the ones version 2 added.
fn samples() -> Vec<Msg> {
    let mut v = samples_v1();
    v.extend(samples_v2());
    v
}

/// One sample or more of every variant of version 1. The frozen v1 bytes are these samples encoded in this order:
/// never edit or reorder them.
fn samples_v1() -> Vec<Msg> {
    let a = p(r"C:\data\report.txt");
    let dir = p(r"C:\data");
    let unc = p(r"\\server\share\folder");
    let dest = p(r"D:\backup");
    let path = ItemRef::Path(a.clone());
    let pidl = ItemRef::Pidl(vec![6, 0, 0x1F, 0x50, 0xAA, 0xBB, 0, 0].into());
    let e1 = UndoEntry {
        from: a.clone(),
        to: p(r"D:\backup\report.txt"),
        size: 1_234_567,
        mtime: 133_000_000_000_000_000,
        dir: false,
        bin: None,
    };
    let e2 = UndoEntry {
        from: unc.join(&w("sub")).unwrap(),
        to: p(r"C:\$Recycle.Bin\$RAB12CD"),
        size: 0,
        mtime: -1,
        dir: true,
        bin: Some(p(r"C:\$Recycle.Bin\$RAB12CD")),
    };
    let px = |w: u16, h: u16| Pixels { w, h, bgra: (0..u32::from(w) * u32::from(h) * 4).map(|i| i as u8).collect() };
    let mut v = vec![
        Msg::Instance(InstanceCmd::NewWindow(Location::Fs(dir.clone()))),
        Msg::Instance(InstanceCmd::NewWindow(Location::Home)),
        Msg::Instance(InstanceCmd::OpenTab(Location::Search { root: unc.clone(), query: "*.txt report".into() })),
        Msg::Instance(InstanceCmd::OpenTab(Location::Options)),
        Msg::Instance(InstanceCmd::Select { dir: dir.clone(), item: a.clone() }),
        Msg::Instance(InstanceCmd::Probe { path: unc.clone() }),
        Msg::OpReq(OpReq::Attach { ui_pid: 4_242, owner: 0x0012_03A4 }),
        Msg::OpReq(OpReq::Stage { op: OpId(1), items: vec![path.clone(), pidl.clone(), ItemRef::Path(unc.clone())] }),
        Msg::OpReq(OpReq::Stage { op: OpId(1), items: vec![] }),
    ];
    for c in [Conflicts::AskSystem, Conflicts::ReplaceAll, Conflicts::SkipAll] {
        v.push(Msg::OpReq(OpReq::Copy { op: OpId(2), dest: dest.clone(), conflicts: c }));
    }
    v.extend([
        Msg::OpReq(OpReq::Move { op: OpId(3), dest: unc.clone(), conflicts: Conflicts::AskSystem }),
        Msg::OpReq(OpReq::Recycle { op: OpId(4) }),
        Msg::OpReq(OpReq::DeletePermanent { op: OpId(5) }),
        Msg::OpReq(OpReq::Rename { op: OpId(6), item: a.clone(), to: "report final.txt".into() }),
        Msg::OpReq(OpReq::NewFolder { op: OpId(7), parent: dir.clone(), name: "New folder".into() }),
        Msg::OpReq(OpReq::UndoStage { op: OpId(8), entries: vec![e1.clone(), e2.clone()] }),
    ]);
    for k in UNDO_KINDS {
        v.push(Msg::OpReq(OpReq::Undo { op: OpId(9), of: OpId(2), kind: k, replaced: 3 }));
    }
    v.extend([
        Msg::OpReq(OpReq::Cancel { op: OpId(10) }),
        Msg::OpReq(OpReq::Pause { op: OpId(11), on: true }),
        Msg::OpReq(OpReq::Pause { op: OpId(11), on: false }),
        Msg::OpReq(OpReq::BringForward { op: OpId(12) }),
        Msg::OpReq(OpReq::ResumeRest { op: OpId(13), of: OpId(3) }),
        Msg::OpReq(OpReq::AckDone { of: OpId(13) }),
        Msg::OpReq(OpReq::Bye),
        Msg::OpEvt(OpEvt::Accepted { op: OpId(1) }),
    ]);
    for k in OP_KINDS {
        v.push(Msg::OpEvt(OpEvt::Started { op: OpId(2), kind: k, items: 3, bytes: u64::MAX }));
    }
    let to = p(r"D:\backup\report.txt");
    v.extend([
        Msg::OpEvt(OpEvt::Progress {
            op: OpId(2),
            done: 512,
            total: 4_096,
            bps: 1_000_000,
            current: "report.txt".into(),
        }),
        Msg::OpEvt(OpEvt::NeedsInput { op: OpId(2), on: true }),
        Msg::OpEvt(OpEvt::ItemsDone {
            op: OpId(2),
            changes: vec![
                Change { kind: ChangeKind::Copied, from: Some(a.clone()), to: Some(to.clone()) },
                Change { kind: ChangeKind::Moved, from: Some(a.clone()), to: Some(to.clone()) },
                Change { kind: ChangeKind::Renamed, from: Some(a.clone()), to: Some(p(r"C:\data\b.txt")) },
                Change { kind: ChangeKind::Recycled, from: Some(a.clone()), to: None },
                Change { kind: ChangeKind::Deleted, from: Some(unc.clone()), to: None },
                Change { kind: ChangeKind::Created, from: None, to: Some(dir.clone()) },
            ],
        }),
        Msg::OpEvt(OpEvt::ItemFailed { op: OpId(2), item: a.clone(), hr: 0x8007_0005_u32 as i32 }),
        Msg::OpEvt(OpEvt::UndoPart { op: OpId(2), entries: vec![e1.clone()] }),
    ]);
    for (i, r) in [OpResult::Done, OpResult::Cancelled, OpResult::Failed, OpResult::Refused].into_iter().enumerate() {
        let undo = if i % 2 == 0 { Some(UndoKind::MoveBack) } else { None };
        v.push(Msg::OpEvt(OpEvt::Finished { op: OpId(2), result: r, undo, replaced: 1, items: 10, failed: i as u32 }));
    }
    for why in REFUSALS {
        v.push(Msg::OpEvt(OpEvt::Refused { op: OpId(3), why, detail: "report.txt".into() }));
    }
    v.extend([
        Msg::OpEvt(OpEvt::Snapshot {
            running: vec![
                OpStatus { op: OpId(2), kind: OpKind::Copy, done: 1, total: 2, needs_input: false },
                OpStatus { op: OpId(3), kind: OpKind::Move, done: 0, total: u64::MAX, needs_input: true },
            ],
        }),
        Msg::OpEvt(OpEvt::Snapshot { running: vec![] }),
        Msg::OpEvt(OpEvt::Pending { of: OpId(4), kind: OpKind::Move, done: 2, total: 5, dest: Some(dest.clone()) }),
        Msg::OpEvt(OpEvt::DoneWhileAway { of: OpId(5), kind: OpKind::Copy, items: 5, failed: 1, dest: None }),
        Msg::ShellReq(ShellReq::Menu {
            req: ReqId(1),
            items: vec![path.clone(), pidl.clone()],
            at: (-1_920, 1_080),
            extended: true,
            owner: 0x0001_0000,
        }),
        Msg::ShellReq(ShellReq::ModernItems { req: ReqId(2), items: vec![path.clone()] }),
    ]);
    for verb in VERBS {
        v.push(Msg::ShellReq(ShellReq::Exec { req: ReqId(3), verb, items: vec![path.clone()] }));
    }
    v.extend([
        Msg::ShellReq(ShellReq::RunTyped { req: ReqId(4), text: "notepad report.txt".into(), dir: dir.clone() }),
        Msg::ShellReq(ShellReq::Icons {
            req: ReqId(5),
            gen: Gen(9),
            items: vec![path.clone(), ItemRef::Path(unc.clone())],
            px: 32,
        }),
        Msg::ShellReq(ShellReq::Thumbs {
            req: ReqId(6),
            gen: Gen(u64::MAX),
            items: vec![path.clone()],
            px: 256,
            cache_only: true,
        }),
        Msg::ShellReq(ShellReq::ClassIcons {
            req: ReqId(7),
            exts: vec!["txt".into(), "tar".into(), "".into()],
            px: 16,
        }),
        Msg::ShellReq(ShellReq::ShellNewTypes { req: ReqId(8) }),
        Msg::ShellReq(ShellReq::QuickAccessPins { req: ReqId(9) }),
        Msg::ShellReq(ShellReq::CancelGen { gen: Gen(8) }),
        Msg::ShellReq(ShellReq::Bye),
        Msg::ShellEvt(ShellEvt::Ready { pid: 777 }),
        Msg::ShellEvt(ShellEvt::MenuShown { req: ReqId(1) }),
        Msg::ShellEvt(ShellEvt::MenuDone { req: ReqId(1), outcome: MenuOutcome::Dismissed, items: vec![] }),
        Msg::ShellEvt(ShellEvt::MenuDone { req: ReqId(1), outcome: MenuOutcome::Invoked, items: vec![] }),
    ]);
    for c in CANON {
        let outcome = MenuOutcome::Intercepted(c);
        v.push(Msg::ShellEvt(ShellEvt::MenuDone { req: ReqId(1), outcome, items: vec![path.clone(), pidl.clone()] }));
    }
    v.extend([
        Msg::ShellEvt(ShellEvt::MenuDone {
            req: ReqId(1),
            outcome: MenuOutcome::Failed(0x8000_4005_u32 as i32),
            items: vec![],
        }),
        Msg::ShellEvt(ShellEvt::Modern {
            req: ReqId(2),
            items: vec![
                ModernItem { id: 1, label: "Share".into(), has_sub: false },
                ModernItem { id: 2, label: "Open with \u{2026}".into(), has_sub: true },
            ],
            done: true,
        }),
        Msg::ShellEvt(ShellEvt::Modern { req: ReqId(2), items: vec![], done: false }),
        Msg::ShellEvt(ShellEvt::ExecDone { req: ReqId(3), hr: 0 }),
        Msg::ShellEvt(ShellEvt::Icon { req: ReqId(5), gen: Gen(9), index: 1, px: px(2, 2) }),
        Msg::ShellEvt(ShellEvt::Thumb { req: ReqId(6), gen: Gen(9), index: 0, px: px(3, 1) }),
        Msg::ShellEvt(ShellEvt::Thumb { req: ReqId(6), gen: Gen(9), index: 2, px: px(0, 0) }),
        Msg::ShellEvt(ShellEvt::ClassIcon { req: ReqId(7), ext: "txt".into(), px: px(1, 1) }),
        Msg::ShellEvt(ShellEvt::ShellNew {
            req: ReqId(8),
            types: vec![
                ShellNewType { ext: "txt".into(), label: "Text Document".into() },
                ShellNewType { ext: "zip".into(), label: "Compressed (zipped) Folder".into() },
            ],
        }),
        Msg::ShellEvt(ShellEvt::Pins { req: ReqId(9), pins: vec![dir.clone(), unc.clone()] }),
        Msg::ShellEvt(ShellEvt::Failed { req: ReqId(4), hr: -1 }),
        Msg::ShellEvt(ShellEvt::Busy { on: true }),
        Msg::ShellEvt(ShellEvt::Busy { on: false }),
        Msg::ShellEvt(ShellEvt::ReqDone { req: ReqId(9) }),
    ]);
    v
}

/// One sample or more of every variant SHELL version 2 added (type names). The frozen v2 bytes are these samples
/// encoded in this order: never edit or reorder them.
fn samples_v2() -> Vec<Msg> {
    let accented: Box<str> = "\u{e9}t\u{e9}".into();
    vec![
        Msg::ShellReq(ShellReq::TypeNames { req: ReqId(10), exts: vec!["txt".into(), "pdf".into(), accented.clone()] }),
        Msg::ShellReq(ShellReq::TypeNames { req: ReqId(11), exts: vec![] }),
        Msg::ShellEvt(ShellEvt::TypeNames {
            req: ReqId(10),
            names: vec![
                TypeName { ext: "txt".into(), name: "Text Document".into() },
                TypeName { ext: "pdf".into(), name: "PDF File".into() },
                TypeName { ext: accented, name: "\u{c9}T\u{c9} File".into() },
            ],
        }),
        Msg::ShellEvt(ShellEvt::TypeNames {
            req: ReqId(11),
            names: vec![TypeName { ext: "".into(), name: "File".into() }],
        }),
    ]
}

/// A frame with the given kind and payload, built by hand.
fn raw(kind: u16, payload: &[u8]) -> Vec<u8> {
    let mut f = (payload.len() as u32 + 2).to_le_bytes().to_vec();
    f.extend_from_slice(&kind.to_le_bytes());
    f.extend_from_slice(payload);
    f
}

fn payload(build: impl FnOnce(&mut Writer)) -> Vec<u8> {
    let mut wr = Writer::new();
    build(&mut wr);
    wr.into_inner()
}

/// The kinds a decoder knows, found by asking it about every u16.
fn known_kinds(knows: fn(u16) -> bool) -> BTreeSet<u16> {
    (0..=u16::MAX).filter(|&k| knows(k)).collect()
}

#[test]
fn kinds_are_pinned() {
    let samples = samples();
    for (name, value) in KINDS {
        let hits: Vec<&Msg> = samples.iter().filter(|m| m.name() == *name).collect();
        assert!(!hits.is_empty(), "{name} has no sample");
        for m in hits {
            assert_eq!(m.kind(), *value, "{name}");
        }
    }
    for m in &samples {
        assert!(KINDS.iter().any(|(n, _)| *n == m.name()), "{} has no row in KINDS", m.name());
    }
    // The decoders know exactly the kinds of the table: nothing more, nothing less.
    for (name, _, _, _, knows) in ENUMS {
        let rows: BTreeSet<u16> =
            KINDS.iter().filter(|(n, _)| n.split("::").next() == Some(name)).map(|(_, k)| *k).collect();
        assert_eq!(known_kinds(knows), rows, "{name}");
    }
    let values: BTreeSet<u16> = KINDS.iter().map(|(_, k)| *k).collect();
    assert_eq!(values.len(), KINDS.len(), "no value twice");
}

#[test]
fn kinds_do_not_cross_ranges() {
    for (name, lo, hi, _, knows) in ENUMS {
        assert!(lo > 0, "kind 0 is the Hello");
        for (row, k) in KINDS.iter().filter(|(n, _)| n.split("::").next() == Some(name)) {
            assert!((lo..=hi).contains(k), "{row} {k:#06x} outside {lo:#06x}..={hi:#06x}");
        }
        assert!(known_kinds(knows).iter().all(|k| (lo..=hi).contains(k)), "{name} decodes a kind outside its range");
        for (other, olo, ohi, _, _) in ENUMS {
            assert!(other == name || hi < olo || ohi < lo, "{name} and {other} overlap");
        }
    }
    // A frame sent the wrong way is UnknownKind on the other side.
    for m in samples() {
        let f = m.frame();
        for (name, _, _, dec, _) in ENUMS {
            if name != m.enum_name() {
                assert_eq!(dec(&f), Err(FrameError::UnknownKind(m.kind())), "{} through {name}", m.name());
            }
        }
    }
}

#[test]
fn round_trip_every_variant() {
    for m in samples() {
        let f = m.frame();
        assert_eq!(f.len(), HEADER_LEN + (u32::from_le_bytes([f[0], f[1], f[2], f[3]]) as usize - 2));
        assert_eq!(u16::from_le_bytes([f[4], f[5]]), m.kind());
        assert_eq!(m.decode_as(&f), Ok((m.clone(), f.len())), "{}", m.name());
        assert_eq!(Msg::decode_any(&f), Ok((m.clone(), f.len())), "{}", m.name());
    }
}

#[test]
fn recycle_never_decodes_as_delete() {
    let recycle = OpReq::Recycle { op: OpId(7) };
    let mut f = Vec::new();
    encode(&recycle, &mut f).unwrap();
    assert_eq!(decode::<OpReq>(&f), Ok((recycle, f.len())));
    f[4..6].copy_from_slice(&0x0106_u16.to_le_bytes());
    // The kind alone decides: hence the distance the next two tests keep between DeletePermanent and every kind.
    assert_eq!(decode::<OpReq>(&f), Ok((OpReq::DeletePermanent { op: OpId(7) }, f.len())));
}

#[test]
fn no_bit_flip_makes_a_delete() {
    for m in samples() {
        let f = m.frame();
        let was_delete = matches!(m, Msg::OpReq(OpReq::DeletePermanent { .. }));
        let kind = u16::from_le_bytes([f[4], f[5]]);
        for bit in 0..16 {
            let mut g = f.clone();
            g[4..6].copy_from_slice(&(kind ^ (1 << bit)).to_le_bytes());
            let got = decode::<OpReq>(&g);
            assert!(
                was_delete || !matches!(got, Ok((OpReq::DeletePermanent { .. }, _))),
                "{} with kind bit {bit} flipped decodes as DeletePermanent",
                m.name()
            );
        }
    }
}

#[test]
fn same_shape_kinds_are_two_bits_from_delete() {
    // The OpReq kinds whose payload has DeletePermanent's shape (a bare OpId): every kind that decodes a 4-byte payload.
    let shape: Vec<u16> = (0..=u16::MAX).filter(|&k| decode::<OpReq>(&raw(k, &7_u32.to_le_bytes())).is_ok()).collect();
    assert_eq!(
        shape,
        [0x0105, 0x0106, 0x010B, 0x010D, 0x010F],
        "Recycle, DeletePermanent, Cancel, BringForward, AckDone"
    );
    for k in shape.into_iter().filter(|&k| k != 0x0106) {
        assert!((k ^ 0x0106).count_ones() >= 2, "{k:#06x} is one bit from DeletePermanent");
    }
    // Every one-bit neighbour of DeletePermanent is no OpReq, or one whose payload is longer than an OpId.
    for bit in 0..16 {
        let k = 0x0106_u16 ^ (1 << bit);
        let got = decode::<OpReq>(&raw(k, &7_u32.to_le_bytes()));
        assert!(got.is_err(), "{k:#06x} decodes a bare OpId: {got:?}");
    }
}

#[test]
fn stage_over_max_list_is_cap() {
    let items = |n: u32| (0..n).map(|i| ItemRef::Path(p(&format!(r"C:\s\{i}")))).collect::<Vec<_>>();
    let f = Msg::OpReq(OpReq::Stage { op: OpId(1), items: items(MAX_LIST + 1) }).frame();
    assert_eq!(decode::<OpReq>(&f), Err(FrameError::Cap { field: "Stage.items", len: MAX_LIST + 1, max: MAX_LIST }));
    let m = OpReq::Stage { op: OpId(1), items: items(MAX_LIST) };
    let f = Msg::OpReq(m.clone()).frame();
    assert_eq!(decode::<OpReq>(&f), Ok((m, f.len())));
}

#[test]
fn pixels_must_match_their_size() {
    let icon = |w: u16, h: u16, n: usize| {
        Msg::ShellEvt(ShellEvt::Icon {
            req: ReqId(1),
            gen: Gen(1),
            index: 0,
            px: Pixels { w, h, bgra: vec![7; n].into() },
        })
    };
    assert_eq!(decode::<ShellEvt>(&icon(2, 2, 15).frame()), Err(FrameError::Invalid { field: "Icon.px" }));
    assert_eq!(decode::<ShellEvt>(&icon(2, 2, 17).frame()), Err(FrameError::Invalid { field: "Icon.px" }));
    assert!(decode::<ShellEvt>(&icon(2, 2, 16).frame()).is_ok());
    assert_eq!(
        decode::<ShellEvt>(&icon(481, 1, 481 * 4).frame()),
        Err(FrameError::Cap { field: "Icon.px", len: 481, max: 480 })
    );
    assert_eq!(
        decode::<ShellEvt>(&icon(1, 481, 481 * 4).frame()),
        Err(FrameError::Cap { field: "Icon.px", len: 481, max: 480 })
    );
    let side = usize::from(MAX_PIXELS_SIDE);
    assert!(decode::<ShellEvt>(&icon(480, 480, side * side * 4).frame()).is_ok(), "the largest picture fits");
    let thumb = Msg::ShellEvt(ShellEvt::Thumb {
        req: ReqId(1),
        gen: Gen(1),
        index: 0,
        px: Pixels { w: 1, h: 1, bgra: vec![0; 3].into() },
    });
    assert_eq!(decode::<ShellEvt>(&thumb.frame()), Err(FrameError::Invalid { field: "Thumb.px" }));
    let class = Msg::ShellEvt(ShellEvt::ClassIcon {
        req: ReqId(1),
        ext: "txt".into(),
        px: Pixels { w: 0, h: 1, bgra: vec![0; 4].into() },
    });
    assert_eq!(decode::<ShellEvt>(&class.frame()), Err(FrameError::Invalid { field: "ClassIcon.px" }));
    // A requested side over MAX_PIXELS_SIDE could not be answered in one frame.
    let icons = Msg::ShellReq(ShellReq::Icons { req: ReqId(1), gen: Gen(1), items: vec![], px: 481 });
    assert_eq!(decode::<ShellReq>(&icons.frame()), Err(FrameError::Cap { field: "Icons.px", len: 481, max: 480 }));
}

#[test]
fn ext_is_lower_case_without_a_dot() {
    let class_icons =
        |e: &str| Msg::ShellReq(ShellReq::ClassIcons { req: ReqId(1), exts: vec!["txt".into(), e.into()], px: 16 });
    for bad in [".txt", "t.xt", "txt.", "TXT", "Txt", "\u{c9}t\u{e9}", "a\\b", "a/b", "a\u{1}"] {
        assert_eq!(
            decode::<ShellReq>(&class_icons(bad).frame()),
            Err(FrameError::Invalid { field: "ClassIcons.exts" }),
            "{bad:?}"
        );
    }
    for good in ["txt", "", "tar", "\u{e9}t\u{e9}", "7z", "a-b_c"] {
        assert!(decode::<ShellReq>(&class_icons(good).frame()).is_ok(), "{good:?}");
    }
    let long = "x".repeat(65);
    assert_eq!(
        decode::<ShellReq>(&class_icons(&long).frame()),
        Err(FrameError::Cap { field: "ClassIcons.exts", len: 65, max: 64 })
    );
    assert!(decode::<ShellReq>(&class_icons(&long[1..]).frame()).is_ok(), "64 bytes");
    let class = Msg::ShellEvt(ShellEvt::ClassIcon {
        req: ReqId(1),
        ext: "a.b".into(),
        px: Pixels { w: 0, h: 0, bgra: Box::new([]) },
    });
    assert_eq!(decode::<ShellEvt>(&class.frame()), Err(FrameError::Invalid { field: "ClassIcon.ext" }));
    let new = Msg::ShellEvt(ShellEvt::ShellNew {
        req: ReqId(1),
        types: vec![ShellNewType { ext: ".txt".into(), label: "Text".into() }],
    });
    assert_eq!(decode::<ShellEvt>(&new.frame()), Err(FrameError::Invalid { field: "ShellNewType.ext" }));
    // Type names (SHELL 2) key their extensions the same way, both ways.
    let type_names = |e: &str| Msg::ShellReq(ShellReq::TypeNames { req: ReqId(1), exts: vec!["txt".into(), e.into()] });
    let type_name = |e: &str| {
        Msg::ShellEvt(ShellEvt::TypeNames { req: ReqId(1), names: vec![TypeName { ext: e.into(), name: "x".into() }] })
    };
    for bad in [".txt", "t.xt", "TXT", "a\\b", "a/b", "a\u{1}"] {
        assert_eq!(
            decode::<ShellReq>(&type_names(bad).frame()),
            Err(FrameError::Invalid { field: "TypeNames.exts" }),
            "{bad:?}"
        );
        assert_eq!(
            decode::<ShellEvt>(&type_name(bad).frame()),
            Err(FrameError::Invalid { field: "TypeName.ext" }),
            "{bad:?}"
        );
    }
    for good in ["txt", "", "\u{e9}t\u{e9}", "7z", &long[1..]] {
        assert!(decode::<ShellReq>(&type_names(good).frame()).is_ok(), "{good:?}");
        assert!(decode::<ShellEvt>(&type_name(good).frame()).is_ok(), "{good:?}");
    }
    assert_eq!(
        decode::<ShellReq>(&type_names(&long).frame()),
        Err(FrameError::Cap { field: "TypeNames.exts", len: 65, max: 64 })
    );
}

#[test]
fn type_names_request_holds_at_most_256_exts() {
    let req = |n: usize| ShellReq::TypeNames { req: ReqId(1), exts: (0..n).map(|i| format!("e{i}").into()).collect() };
    let m = req(256);
    let f = Msg::ShellReq(m.clone()).frame();
    assert_eq!(decode::<ShellReq>(&f), Ok((m, f.len())));
    let f = Msg::ShellReq(req(257)).frame();
    assert_eq!(decode::<ShellReq>(&f), Err(FrameError::Cap { field: "TypeNames.exts", len: 257, max: 256 }));
}

#[test]
fn type_names_reply_is_never_empty() {
    let empty = payload(|wr| {
        wr.u32(4);
        wr.u32(0);
    });
    assert_eq!(decode::<ShellEvt>(&raw(0x028E, &empty)), Err(FrameError::Invalid { field: "TypeNames.names" }));
    // The same bytes under Pins' kind, one bit away, are an empty Pins, as version 1 decodes them: so an empty
    // TypeNames is never sent and never decoded, and no flip turns one kind into the other.
    assert_eq!(decode::<ShellEvt>(&raw(0x028A, &empty)), Ok((ShellEvt::Pins { req: ReqId(4), pins: vec![] }, 14)));
    assert_eq!((0x028E_u16 ^ 0x028A).count_ones(), 1);
}

/// SHELL 2's kinds are one bit from kinds of version 1 (0x020C from ShellNewTypes 0x0208 and RunTyped 0x0204; 0x028E
/// from Busy 0x028C, Pins 0x028A and Icon 0x0286), so their payloads keep them apart: every single-bit flip of a
/// TypeNames frame's kind, and every flip of any sample's kind onto a TypeNames kind, is refused by every decoder.
#[test]
fn no_bit_flip_crosses_type_names() {
    const NEW: [u16; 2] = [0x020C, 0x028E];
    let mut checked = 0;
    for m in samples() {
        let f = m.frame();
        let kind = m.kind();
        for bit in 0..16 {
            let flipped = kind ^ (1 << bit);
            if !NEW.contains(&kind) && !NEW.contains(&flipped) {
                continue;
            }
            let mut g = f.clone();
            g[4..6].copy_from_slice(&flipped.to_le_bytes());
            for (name, _, _, dec, _) in ENUMS {
                let got = dec(&g);
                assert!(got.is_err(), "{} with kind bit {bit} flipped decodes through {name}: {got:?}", m.name());
            }
            checked += 1;
        }
    }
    assert!(checked > 4 * 16, "{checked} flips checked");
}

#[test]
fn names_are_one_component() {
    let rename = |to: &str| Msg::OpReq(OpReq::Rename { op: OpId(1), item: p(r"C:\a"), to: to.into() });
    let folder = |name: &str| Msg::OpReq(OpReq::NewFolder { op: OpId(1), parent: p(r"C:\a"), name: name.into() });
    let units_256 = "a".repeat(256);
    for bad in ["a\\b", "a/b", "\\", "a\u{1}b", "\u{1f}", "a\0", ".", "..", "", units_256.as_str()] {
        assert_eq!(decode::<OpReq>(&rename(bad).frame()), Err(FrameError::Invalid { field: "Rename.to" }), "{bad:?}");
        assert_eq!(
            decode::<OpReq>(&folder(bad).frame()),
            Err(FrameError::Invalid { field: "NewFolder.name" }),
            "{bad:?}"
        );
    }
    // 255 units, whatever their UTF-8 length; 256 units are one too many.
    let emoji_255 = format!("{}a", "\u{1F600}".repeat(127));
    let emoji_256 = "\u{1F600}".repeat(128);
    let euro_255 = "\u{20AC}".repeat(255);
    let a_255 = "a".repeat(255);
    for good in ["a", "...", ".a", "a.", "report final.txt", a_255.as_str(), emoji_255.as_str(), euro_255.as_str()] {
        assert!(decode::<OpReq>(&rename(good).frame()).is_ok(), "{good:?}");
    }
    assert_eq!(decode::<OpReq>(&rename(&emoji_256).frame()), Err(FrameError::Invalid { field: "Rename.to" }));
    // Past 765 bytes a name is past 255 units too: Invalid like any name over the limit, not Cap.
    let euro_256 = "\u{20AC}".repeat(256);
    assert_eq!(decode::<OpReq>(&rename(&euro_256).frame()), Err(FrameError::Invalid { field: "Rename.to" }));
    assert_eq!(decode::<OpReq>(&folder(&euro_256).frame()), Err(FrameError::Invalid { field: "NewFolder.name" }));
}

#[test]
fn string_fields_keep_their_caps() {
    #[track_caller]
    fn cap(make: impl Fn(String) -> Msg, field: &'static str, max: usize) {
        let at = make("x".repeat(max));
        assert!(at.decode_as(&at.frame()).is_ok(), "{field} at its cap");
        let over = make("x".repeat(max + 1));
        assert_eq!(
            over.decode_as(&over.frame()),
            Err(FrameError::Cap { field, len: max as u32 + 1, max: max as u32 }),
            "{field} over its cap"
        );
    }
    let root = p(r"C:\a");
    cap(
        |s| Msg::Instance(InstanceCmd::NewWindow(Location::Search { root: root.clone(), query: s.into() })),
        "Search.query",
        1_024,
    );
    cap(
        |s| Msg::OpEvt(OpEvt::Progress { op: OpId(1), done: 0, total: 0, bps: 0, current: s.into() }),
        "Progress.current",
        520,
    );
    cap(|s| Msg::OpEvt(OpEvt::Refused { op: OpId(1), why: Refusal::BadName, detail: s.into() }), "Refused.detail", 520);
    cap(
        |s| Msg::ShellReq(ShellReq::RunTyped { req: ReqId(1), text: s.into(), dir: root.clone() }),
        "RunTyped.text",
        8_192,
    );
    let modern = |s: String| ModernItem { id: 1, label: s.into(), has_sub: false };
    cap(
        |s| Msg::ShellEvt(ShellEvt::Modern { req: ReqId(1), items: vec![modern(s)], done: true }),
        "ModernItem.label",
        512,
    );
    let new_type = |s: String| ShellNewType { ext: "txt".into(), label: s.into() };
    cap(|s| Msg::ShellEvt(ShellEvt::ShellNew { req: ReqId(1), types: vec![new_type(s)] }), "ShellNewType.label", 512);
    let type_name = |s: String| TypeName { ext: "txt".into(), name: s.into() };
    cap(|s| Msg::ShellEvt(ShellEvt::TypeNames { req: ReqId(1), names: vec![type_name(s)] }), "TypeName.name", 260);
    // A PIDL hand-off is at most 64 KiB.
    let stage = |n: usize| Msg::OpReq(OpReq::Stage { op: OpId(1), items: vec![ItemRef::Pidl(vec![0; n].into())] });
    assert!(stage(65_536).decode_as(&stage(65_536).frame()).is_ok());
    assert_eq!(
        stage(65_537).decode_as(&stage(65_537).frame()),
        Err(FrameError::Cap { field: "Stage.items", len: 65_537, max: 65_536 })
    );
}

#[test]
fn paths_on_the_wire_are_normal() {
    let probe = |units: &[u16]| raw(0x0304, &payload(|wr| wr.wide(units)));
    assert!(decode::<InstanceCmd>(&probe(&w(r"C:\a\b"))).is_ok());
    for bad in [
        r"C:/a",
        r"C:\a\",
        r"c:\a\\b",
        r"\\?\C:\a",
        r"\\?\UNC\srv\sh\x",
        r"a\b",
        r"C:a",
        r"C:\a\..\b",
        r"\\.\pipe\x",
        "",
    ] {
        assert_eq!(decode::<InstanceCmd>(&probe(&w(bad))), Err(FrameError::Invalid { field: "Probe.path" }), "{bad:?}");
    }
    // A path of MAX_PATH_UNITS is over the instance protocol's 64 KiB: the ops protocol carries it.
    let failed = |units: &[u16]| {
        raw(
            0x0186,
            &payload(|wr| {
                wr.u32(1);
                wr.wide(units);
                wr.i32(-1);
            }),
        )
    };
    let mut long = w(r"C:\");
    long.resize(usize::from(MAX_PATH_UNITS) + 1, u16::from(b'a'));
    assert_eq!(
        decode::<OpEvt>(&failed(&long)),
        Err(FrameError::Cap { field: "ItemFailed.item", len: 32_768, max: 32_767 })
    );
    long.pop();
    assert!(decode::<OpEvt>(&failed(&long)).is_ok(), "32,767 units");
}

#[test]
fn tags_out_of_range_are_bad_value() {
    let op = 1_u32.to_le_bytes();
    let path = payload(|wr| wr.wide(&w(r"C:\a")));
    #[track_caller]
    fn bad<C: Codec + PartialEq + std::fmt::Debug>(kind: u16, body: Vec<u8>, field: &'static str, value: u32) {
        assert_eq!(decode::<C>(&raw(kind, &body)).map(|(m, _)| m), Err(FrameError::BadValue { field, value }));
    }
    let cat = |parts: &[&[u8]]| parts.concat();
    bad::<OpReq>(0x0103, cat(&[&op, &path, &[3]]), "Copy.conflicts", 3);
    bad::<OpReq>(0x010A, cat(&[&op, &op, &[0], &op]), "Undo.kind", 0);
    bad::<OpReq>(0x010A, cat(&[&op, &op, &[7], &op]), "Undo.kind", 7);
    bad::<OpReq>(0x010C, cat(&[&op, &[2]]), "Pause.on", 2);
    bad::<OpReq>(0x0102, cat(&[&op, &1_u32.to_le_bytes(), &[3], &path]), "Stage.items", 3);
    bad::<OpReq>(0x0102, cat(&[&op, &1_u32.to_le_bytes(), &[0], &path]), "Stage.items", 0);
    bad::<OpEvt>(0x0182, cat(&[&op, &[0], &op, &[0; 8]]), "Started.kind", 0);
    bad::<OpEvt>(0x0182, cat(&[&op, &[9], &op, &[0; 8]]), "Started.kind", 9);
    bad::<OpEvt>(0x0188, cat(&[&op, &[4], &[0], &op, &op, &op]), "Finished.result", 4);
    bad::<OpEvt>(0x0188, cat(&[&op, &[0], &[2], &op, &op, &op]), "Finished.undo", 2);
    bad::<OpEvt>(0x0188, cat(&[&op, &[0], &[1, 7], &op, &op, &op]), "Finished.undo", 7);
    bad::<OpEvt>(0x0189, cat(&[&op, &[15], &0_u32.to_le_bytes()]), "Refused.why", 15);
    bad::<OpEvt>(0x0185, cat(&[&op, &1_u32.to_le_bytes(), &[7, 0, 0]]), "Change.kind", 7);
    bad::<OpEvt>(0x0185, cat(&[&op, &1_u32.to_le_bytes(), &[1, 2, 0]]), "Change.from", 2);
    bad::<OpEvt>(0x018A, cat(&[&1_u32.to_le_bytes(), &op, &[1], &[0; 16], &[2]]), "OpStatus.needs_input", 2);
    bad::<InstanceCmd>(0x0301, vec![5], "NewWindow.0", 5);
    bad::<InstanceCmd>(0x0302, vec![0], "OpenTab.0", 0);
    bad::<ShellReq>(0x0203, cat(&[&op, &9_u16.to_le_bytes(), &0_u32.to_le_bytes()]), "Exec.verb", 9);
    bad::<ShellReq>(0x0203, cat(&[&op, &0_u16.to_le_bytes(), &0_u32.to_le_bytes()]), "Exec.verb", 0);
    bad::<ShellReq>(0x0201, cat(&[&op, &0_u32.to_le_bytes(), &[0; 8], &[2], &[0; 8]]), "Menu.extended", 2);
    bad::<ShellEvt>(0x0283, cat(&[&op, &[5], &0_u32.to_le_bytes()]), "MenuDone.outcome", 5);
    bad::<ShellEvt>(0x0283, cat(&[&op, &[3, 7], &0_u32.to_le_bytes()]), "MenuDone.outcome", 7);
    bad::<ShellEvt>(0x028C, vec![2], "Busy.on", 2);
}

fn total<T: WireLen>(v: &[T]) -> usize {
    v.iter().map(WireLen::wire_len).sum()
}

/// A list frame's payload is fixed + 4 + its elements' wire_len, the sum split_by_bytes bounds; fixed is pinned here
/// for every frame that carries a list.
#[test]
fn list_frames_are_fixed_plus_count_plus_elements() {
    let mut seen = BTreeSet::new();
    for m in samples() {
        let (fixed, elems) = match &m {
            Msg::OpReq(OpReq::Stage { items, .. }) => (4, total(items)),
            Msg::OpReq(OpReq::UndoStage { entries, .. }) => (4, total(entries)),
            Msg::OpEvt(OpEvt::ItemsDone { changes, .. }) => (4, total(changes)),
            Msg::OpEvt(OpEvt::UndoPart { entries, .. }) => (4, total(entries)),
            Msg::ShellReq(ShellReq::Menu { items, .. }) => (21, total(items)),
            Msg::ShellReq(ShellReq::ModernItems { items, .. }) => (4, total(items)),
            Msg::ShellReq(ShellReq::Icons { items, .. }) => (14, total(items)),
            Msg::ShellReq(ShellReq::Thumbs { items, .. }) => (15, total(items)),
            Msg::ShellReq(ShellReq::ClassIcons { exts, .. }) => (6, total(exts)),
            Msg::ShellEvt(ShellEvt::Modern { items, .. }) => (5, total(items)),
            Msg::ShellEvt(ShellEvt::ShellNew { types, .. }) => (4, total(types)),
            Msg::ShellEvt(ShellEvt::Pins { pins, .. }) => (4, total(pins)),
            Msg::ShellReq(ShellReq::TypeNames { exts, .. }) => (4, total(exts)),
            Msg::ShellEvt(ShellEvt::TypeNames { names, .. }) => (4, total(names)),
            _ => continue,
        };
        assert_eq!(m.frame().len() - HEADER_LEN, fixed + 4 + elems, "{}", m.name());
        seen.insert(m.name());
    }
    assert_eq!(seen.len(), 14);
}

/// wire_len (FZ2) against the encoder, element by element: a frame with one element minus the same frame empty.
#[test]
fn wire_len_equals_the_encoded_length() {
    #[track_caller]
    fn each<T: WireLen + Clone>(items: &[T], wrap: impl Fn(Vec<T>) -> Msg) {
        let empty = wrap(Vec::new()).frame().len();
        assert!(!items.is_empty());
        for x in items {
            assert_eq!(wrap(vec![x.clone()]).frame().len() - empty, x.wire_len());
        }
    }
    let base = deep_base();
    let (d1, d2) = (deep(&base, 1), deep(&base, 2));
    let mut items = vec![ItemRef::Path(d1.clone()), ItemRef::Pidl(vec![9; 65_536].into()), ItemRef::Pidl(Box::new([]))];
    let mut entries =
        vec![UndoEntry { from: d1.clone(), to: d2.clone(), size: 1, mtime: 2, dir: true, bin: Some(d1.clone()) }];
    let mut changes = vec![Change { kind: ChangeKind::Moved, from: Some(d1.clone()), to: Some(d2.clone()) }];
    let mut modern = vec![ModernItem { id: u32::MAX, label: "x".repeat(512).into(), has_sub: true }];
    let mut types = vec![ShellNewType { ext: "x".repeat(64).into(), label: "\u{e9}".repeat(256).into() }];
    let mut pins = vec![d1.clone()];
    let mut exts: Vec<Box<str>> = vec!["x".repeat(64).into()];
    let mut type_names = vec![TypeName { ext: "x".repeat(64).into(), name: "\u{e9}".repeat(130).into() }];
    for m in samples() {
        match m {
            Msg::OpReq(OpReq::Stage { items: v, .. }) | Msg::ShellReq(ShellReq::Menu { items: v, .. }) => {
                items.extend(v)
            }
            Msg::OpReq(OpReq::UndoStage { entries: v, .. }) | Msg::OpEvt(OpEvt::UndoPart { entries: v, .. }) => {
                entries.extend(v)
            }
            Msg::OpEvt(OpEvt::ItemsDone { changes: v, .. }) => changes.extend(v),
            Msg::ShellEvt(ShellEvt::Modern { items: v, .. }) => modern.extend(v),
            Msg::ShellEvt(ShellEvt::ShellNew { types: v, .. }) => types.extend(v),
            Msg::ShellEvt(ShellEvt::Pins { pins: v, .. }) => pins.extend(v),
            Msg::ShellReq(ShellReq::ClassIcons { exts: v, .. })
            | Msg::ShellReq(ShellReq::TypeNames { exts: v, .. }) => exts.extend(v),
            Msg::ShellEvt(ShellEvt::TypeNames { names: v, .. }) => type_names.extend(v),
            _ => {}
        }
    }
    each(&items, |v| Msg::OpReq(OpReq::Stage { op: OpId(1), items: v }));
    each(&items, |v| Msg::ShellReq(ShellReq::Menu { req: ReqId(1), items: v, at: (0, 0), extended: false, owner: 0 }));
    each(&entries, |v| Msg::OpEvt(OpEvt::UndoPart { op: OpId(1), entries: v }));
    each(&entries, |v| Msg::OpReq(OpReq::UndoStage { op: OpId(1), entries: v }));
    each(&changes, |v| Msg::OpEvt(OpEvt::ItemsDone { op: OpId(1), changes: v }));
    each(&modern, |v| Msg::ShellEvt(ShellEvt::Modern { req: ReqId(1), items: v, done: false }));
    each(&types, |v| Msg::ShellEvt(ShellEvt::ShellNew { req: ReqId(1), types: v }));
    each(&pins, |v| Msg::ShellEvt(ShellEvt::Pins { req: ReqId(1), pins: v }));
    each(&exts, |v| Msg::ShellReq(ShellReq::ClassIcons { req: ReqId(1), exts: v, px: 16 }));
    each(&exts, |v| Msg::ShellReq(ShellReq::TypeNames { req: ReqId(1), exts: v }));
    each(&type_names, |v| Msg::ShellEvt(ShellEvt::TypeNames { req: ReqId(1), names: v }));
}

// ---- byte-split list frames (2.2's splitting rules)

/// The deep-path tests hold hundreds of megabytes each: one at a time.
static DEEP: Mutex<()> = Mutex::new(());

fn deep_lock() -> MutexGuard<'static, ()> {
    DEEP.lock().unwrap_or_else(PoisonError::into_inner)
}

/// A normal path of MAX_PATH_UNITS - 7 units: "C:\" then components of 199 letters.
fn deep_base() -> WidePath {
    let mut units = w(r"C:\");
    let n = usize::from(MAX_PATH_UNITS) - 7;
    while units.len() < n {
        let k = units.len();
        units.push(if k % 200 == 199 && k + 1 < n { u16::from(b'\\') } else { u16::from(b'a') + (k % 26) as u16 });
    }
    WidePath::new(&units).unwrap()
}

/// A path of exactly MAX_PATH_UNITS units, different for each i.
fn deep(base: &WidePath, i: usize) -> WidePath {
    let d = base.join(&w(&format!("{i:06}"))).unwrap();
    assert_eq!(d.units().len(), usize::from(MAX_PATH_UNITS));
    d
}

/// Splits items as a sender does, encodes each slice with wrap (told whether it is the last), and checks every
/// frame: payload within max_payload - SPLIT_MARGIN, decodes to its own slice, and in order the slices are the
/// input. Returns the number of frames.
fn split_round_trip<T: WireLen + Clone + PartialEq + std::fmt::Debug>(
    items: &[T],
    fixed: usize,
    wrap: impl Fn(Vec<T>, bool) -> Msg,
    unwrap: impl Fn(Msg, bool) -> Vec<T>,
) -> usize {
    let proto = wrap(Vec::new(), true).proto();
    let limit = (proto.max_payload - SPLIT_MARGIN) as usize;
    let parts = split_by_bytes(items, fixed, &proto, MAX_LIST);
    let mut at = 0;
    let mut out = Vec::new();
    for (i, part) in parts.iter().enumerate() {
        let last = i + 1 == parts.len();
        let m = wrap(part.to_vec(), last);
        out.clear();
        m.encode(&mut out).unwrap_or_else(|e| panic!("frame {i}: {e:?}"));
        assert!(out.len() - HEADER_LEN <= limit, "frame {i}: {} > {limit}", out.len() - HEADER_LEN);
        assert_eq!(out.len() - HEADER_LEN, fixed + 4 + part.iter().map(WireLen::wire_len).sum::<usize>());
        let (back, used) = m.decode_as(&out).unwrap_or_else(|e| panic!("frame {i}: {e:?}"));
        assert_eq!(used, out.len());
        assert!(unwrap(back, last) == items[at..at + part.len()], "frame {i} decodes to its slice");
        at += part.len();
    }
    assert_eq!(at, items.len(), "the frames carry the whole input");
    parts.len()
}

#[test]
fn items_done_2048_deep_paths_split() {
    let _one = deep_lock();
    let base = deep_base();
    let changes: Vec<Change> = (0..MAX_LIST as usize)
        .map(|i| Change { kind: ChangeKind::Moved, from: Some(deep(&base, 2 * i)), to: Some(deep(&base, 2 * i + 1)) })
        .collect();
    let frames = split_round_trip(
        &changes,
        4,
        |v, _| Msg::OpEvt(OpEvt::ItemsDone { op: OpId(9), changes: v }),
        |m, _| match m {
            Msg::OpEvt(OpEvt::ItemsDone { op: OpId(9), changes }) => changes,
            m => panic!("{m:?}"),
        },
    );
    assert!(
        frames > 1,
        "a count cap alone would have sent one frame of {} bytes",
        changes.len() * changes[0].wire_len()
    );
}

#[test]
fn undo_part_three_deep_paths_split() {
    let _one = deep_lock();
    let base = deep_base();
    let entries: Vec<UndoEntry> = (0..256)
        .map(|i| UndoEntry {
            from: deep(&base, 3 * i),
            to: deep(&base, 3 * i + 1),
            size: i as u64,
            mtime: -(i as i64),
            dir: i % 2 == 0,
            bin: Some(deep(&base, 3 * i + 2)),
        })
        .collect();
    let frames = split_round_trip(
        &entries,
        4,
        |v, _| Msg::OpEvt(OpEvt::UndoPart { op: OpId(9), entries: v }),
        |m, _| match m {
            Msg::OpEvt(OpEvt::UndoPart { entries, .. }) => entries,
            m => panic!("{m:?}"),
        },
    );
    assert!(frames > 1);
    // The client's UndoStage carries the same elements under the same rule.
    let frames = split_round_trip(
        &entries[..32],
        4,
        |v, _| Msg::OpReq(OpReq::UndoStage { op: OpId(9), entries: v }),
        |m, _| match m {
            Msg::OpReq(OpReq::UndoStage { entries, .. }) => entries,
            m => panic!("{m:?}"),
        },
    );
    assert!(frames > 1);
}

#[test]
fn stage_deep_paths_and_pidls_split() {
    let _one = deep_lock();
    let base = deep_base();
    let items: Vec<ItemRef> = (0..256)
        .map(|i| {
            if i % 2 == 0 {
                ItemRef::Path(deep(&base, i))
            } else {
                ItemRef::Pidl((0..65_536).map(|b| (b + i) as u8).collect())
            }
        })
        .collect();
    let frames = split_round_trip(
        &items,
        4,
        |v, _| Msg::OpReq(OpReq::Stage { op: OpId(9), items: v }),
        |m, _| match m {
            Msg::OpReq(OpReq::Stage { items, .. }) => items,
            m => panic!("{m:?}"),
        },
    );
    assert!(frames > 1);
}

#[test]
fn modern_split_is_done_only_on_the_last() {
    let items: Vec<ModernItem> = (0..MAX_LIST)
        .map(|i| ModernItem { id: i, label: format!("{}{i:012}", "x".repeat(500)).into(), has_sub: i % 3 == 0 })
        .collect();
    let frames = split_round_trip(
        &items,
        5,
        |v, last| Msg::ShellEvt(ShellEvt::Modern { req: ReqId(4), items: v, done: last }),
        |m, last| match m {
            Msg::ShellEvt(ShellEvt::Modern { items, done, .. }) => {
                assert_eq!(done, last, "done only on the last frame");
                items
            }
            m => panic!("{m:?}"),
        },
    );
    assert!(frames > 1, "2,048 labels of 512 bytes are over one frame");
}

#[test]
fn pins_and_shell_new_split() {
    let _one = deep_lock();
    let base = deep_base();
    let pins: Vec<WidePath> = (0..256).map(|i| deep(&base, i)).collect();
    let frames = split_round_trip(
        &pins,
        4,
        |v, _| Msg::ShellEvt(ShellEvt::Pins { req: ReqId(5), pins: v }),
        |m, _| match m {
            Msg::ShellEvt(ShellEvt::Pins { pins, .. }) => pins,
            m => panic!("{m:?}"),
        },
    );
    assert!(frames > 1);
    let types: Vec<ShellNewType> = (0..MAX_LIST)
        .map(|i| ShellNewType { ext: format!("{i:064}").into(), label: format!("{}{i:012}", "y".repeat(500)).into() })
        .collect();
    let frames = split_round_trip(
        &types,
        4,
        |v, _| Msg::ShellEvt(ShellEvt::ShellNew { req: ReqId(6), types: v }),
        |m, _| match m {
            Msg::ShellEvt(ShellEvt::ShellNew { types, .. }) => types,
            m => panic!("{m:?}"),
        },
    );
    assert!(frames > 1);
}

#[test]
fn type_names_split() {
    // 2,048 names of the longest extension and name: a frame's count cap, and still within its bytes (2,048 x 332
    // bytes), so the count splits a TypeNames list before the bytes would.
    let names: Vec<TypeName> = (0..MAX_LIST as usize)
        .map(|i| TypeName { ext: format!("{i:064}").into(), name: format!("{}{i:012}", "n".repeat(248)).into() })
        .collect();
    assert!(names.iter().all(|t| t.ext.len() == 64 && t.name.len() == 260 && t.wire_len() == 332));
    let wrap = |v: Vec<TypeName>, _: bool| Msg::ShellEvt(ShellEvt::TypeNames { req: ReqId(7), names: v });
    let unwrap = |m: Msg, _: bool| match m {
        Msg::ShellEvt(ShellEvt::TypeNames { names, .. }) => names,
        m => panic!("{m:?}"),
    };
    assert_eq!(split_round_trip(&names, 4, wrap, unwrap), 1, "2,048 names of 260 bytes fit one frame");
    let twice: Vec<TypeName> = names.iter().chain(&names).cloned().collect();
    assert_eq!(split_round_trip(&twice, 4, wrap, unwrap), 2, "the count cap splits");
    // Names of four-byte characters: 260 bytes are 65 of them.
    let wide: Vec<TypeName> =
        (0..MAX_LIST).map(|i| TypeName { ext: format!("e{i}").into(), name: "\u{1F600}".repeat(65).into() }).collect();
    assert_eq!(split_round_trip(&wide, 4, wrap, unwrap), 1);
}

#[test]
fn whole_frames_report_too_long() {
    let _one = deep_lock();
    let base = deep_base();
    let items: Vec<ItemRef> = (0..20).map(|i| ItemRef::Path(deep(&base, i))).collect(); // about 1.3 MB
    let whole = [
        Msg::ShellReq(ShellReq::Menu { req: ReqId(1), items: items.clone(), at: (0, 0), extended: false, owner: 0 }),
        Msg::ShellReq(ShellReq::ModernItems { req: ReqId(1), items: items.clone() }),
        Msg::ShellReq(ShellReq::Exec { req: ReqId(1), verb: Verb::Open, items: items.clone() }),
        Msg::ShellReq(ShellReq::Icons { req: ReqId(1), gen: Gen(1), items: items.clone(), px: 16 }),
        Msg::ShellReq(ShellReq::Thumbs { req: ReqId(1), gen: Gen(1), items: items.clone(), px: 16, cache_only: false }),
        Msg::ShellReq(ShellReq::ClassIcons { req: ReqId(1), exts: vec!["x".repeat(64).into(); 20_000], px: 16 }),
        Msg::ShellEvt(ShellEvt::MenuDone { req: ReqId(1), outcome: MenuOutcome::Intercepted(CanonVerb::Copy), items }),
        Msg::Instance(InstanceCmd::Select { dir: deep(&base, 1), item: deep(&base, 2) }),
    ];
    for m in whole {
        let mut out = vec![1, 2, 3];
        let got = m.encode(&mut out);
        assert!(
            matches!(&got, Err(FrameError::TooLong { len, max }) if len > max && *max == m.proto().max_payload),
            "{}: {got:?}",
            m.name()
        );
        assert_eq!(out, [1, 2, 3], "{}: out unchanged", m.name());
    }
}

#[test]
fn one_element_always_fits() {
    let _one = deep_lock();
    let base = deep_base();
    let limit = (OPS.max_payload - SPLIT_MARGIN) as usize;
    let big = UndoEntry {
        from: deep(&base, 1),
        to: deep(&base, 2),
        size: u64::MAX,
        mtime: i64::MIN,
        dir: true,
        bin: Some(deep(&base, 3)),
    };
    assert_eq!(big.wire_len(), 196_626, "the largest single element of the files protocols");
    let side = usize::from(MAX_PIXELS_SIDE);
    let px = || Pixels { w: MAX_PIXELS_SIDE, h: MAX_PIXELS_SIDE, bgra: vec![0xFF; side * side * 4].into() };
    let alone = [
        Msg::OpEvt(OpEvt::UndoPart { op: OpId(1), entries: vec![big.clone()] }),
        Msg::OpReq(OpReq::UndoStage { op: OpId(1), entries: vec![big] }),
        Msg::OpEvt(OpEvt::ItemsDone {
            op: OpId(1),
            changes: vec![Change { kind: ChangeKind::Copied, from: Some(deep(&base, 4)), to: Some(deep(&base, 5)) }],
        }),
        Msg::OpReq(OpReq::Stage { op: OpId(1), items: vec![ItemRef::Pidl(vec![1; 65_536].into())] }),
        Msg::ShellEvt(ShellEvt::Icon { req: ReqId(1), gen: Gen(u64::MAX), index: u32::MAX, px: px() }),
        Msg::ShellEvt(ShellEvt::Thumb { req: ReqId(1), gen: Gen(u64::MAX), index: u32::MAX, px: px() }),
        Msg::ShellEvt(ShellEvt::ClassIcon { req: ReqId(1), ext: "x".repeat(64).into(), px: px() }),
    ];
    for m in alone {
        let f = m.frame();
        assert!(f.len() - HEADER_LEN <= limit, "{}: {} > {limit}", m.name(), f.len() - HEADER_LEN);
        assert_eq!(m.decode_as(&f), Ok((m.clone(), f.len())), "{}", m.name());
    }
}

// ---- the frozen v1 bytes and fuzz (FR5)

/// The build id of the frozen Hello frames (any id decodes: it is logged, never compared).
const FROZEN_BUILD: [u8; 16] = *b"frozen-v1\0\0\0\0\0\0\0";
const FROZEN_BUILD_V2: [u8; 16] = *b"frozen-v2\0\0\0\0\0\0\0";
/// The protocols as they are now.
const PROTOS: [Proto; 3] = [OPS, SHELL, INSTANCE];
/// The protocols as version 1 froze them: SHELL was version 1 then.
const PROTOS_V1: [Proto; 3] = [OPS, Proto { version: 1, ..SHELL }, INSTANCE];

fn v1_path() -> std::path::PathBuf {
    std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/frames/files-v1.bin")
}

fn v2_path() -> std::path::PathBuf {
    std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/frames/files-v2.bin")
}

/// The version-1 Hello of each protocol, then every version-1 sample in order.
fn v1_stream() -> Vec<u8> {
    let mut out = Vec::new();
    for p in &PROTOS_V1 {
        encode_hello(p, FROZEN_BUILD, &mut out);
    }
    for m in samples_v1() {
        m.encode(&mut out).unwrap();
    }
    out
}

/// SHELL's version-2 Hello, then every sample of the kinds version 2 added, in order.
fn v2_stream() -> Vec<u8> {
    let mut out = Vec::new();
    encode_hello(&SHELL, FROZEN_BUILD_V2, &mut out);
    for m in samples_v2() {
        m.encode(&mut out).unwrap();
    }
    out
}

/// Writes files-v1.bin: once, when version 1 of the protocols is frozen, and never again. A version 2 keeps this
/// file and adds files-v2.bin; this test's v1 check then becomes "decode_hello refuses v1 (BadProto)".
#[test]
#[ignore = "writes tests/fixtures/frames/files-v1.bin: UPDATE_FRAMES=1, once"]
fn write_frames_v1() {
    if std::env::var("UPDATE_FRAMES").as_deref() != Ok("1") {
        eprintln!("UPDATE_FRAMES is not 1: nothing written");
        return;
    }
    let path = v1_path();
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(&path, v1_stream()).unwrap();
}

/// Writes files-v2.bin: once, when version 2 of the shell protocol is frozen, and never again.
#[test]
#[ignore = "writes tests/fixtures/frames/files-v2.bin: UPDATE_FRAMES=1, once"]
fn write_frames_v2() {
    if std::env::var("UPDATE_FRAMES").as_deref() != Ok("1") {
        eprintln!("UPDATE_FRAMES is not 1: nothing written");
        return;
    }
    let path = v2_path();
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(&path, v2_stream()).unwrap();
}

#[test]
fn frozen_bytes_v1() {
    let bytes = std::fs::read(v1_path()).expect("tests/fixtures/frames/files-v1.bin");
    let mut asm = Assembler::new(OPS.max_payload);
    asm.push(&bytes);
    let frames: Vec<Vec<u8>> = std::iter::from_fn(|| asm.next_frame().unwrap()).collect();
    assert_eq!(frames.concat(), bytes, "the file is whole frames and nothing else");
    let samples = samples_v1();
    assert_eq!(frames.len(), PROTOS_V1.len() + samples.len());
    for (f, (v1, now)) in frames.iter().zip(PROTOS_V1.iter().zip(&PROTOS)) {
        let hello = Hello { magic: v1.magic, proto: v1.version, build: FROZEN_BUILD };
        assert_eq!(decode_hello(v1, f), Ok((hello, f.len())));
        let mut again = Vec::new();
        encode_hello(v1, FROZEN_BUILD, &mut again);
        assert_eq!(&again, f);
        // Today: a protocol still at version 1 accepts its v1 Hello; one at a later version refuses it.
        let want = if now.version == v1.version {
            Ok((hello, f.len()))
        } else {
            Err(FrameError::BadProto { got: v1.version, want: now.version })
        };
        assert_eq!(decode_hello(now, f), want);
    }
    // SHELL is version 2: a v1 shell host or client is refused at its Hello, so nothing else on that stream is
    // decoded; OPS and INSTANCE are still version 1.
    assert_eq!(decode_hello(&SHELL, &frames[1]), Err(FrameError::BadProto { got: 1, want: 2 }));
    assert_eq!(decode_hello(&OPS, &frames[0]).map(|(h, _)| h.proto), Ok(1));
    assert_eq!(decode_hello(&INSTANCE, &frames[2]).map(|(h, _)| h.proto), Ok(1));
    // Version 2 only added kinds: every v1 frame still decodes and encodes to its frozen bytes.
    for (f, m) in frames[PROTOS_V1.len()..].iter().zip(&samples) {
        assert_eq!(Msg::decode_any(f), Ok((m.clone(), f.len())), "{}", m.name());
        assert_eq!(&m.frame(), f, "{} encodes to its frozen bytes", m.name());
    }
}

#[test]
fn frozen_bytes_v2() {
    let bytes = std::fs::read(v2_path()).expect("tests/fixtures/frames/files-v2.bin");
    let mut asm = Assembler::new(SHELL.max_payload);
    asm.push(&bytes);
    let frames: Vec<Vec<u8>> = std::iter::from_fn(|| asm.next_frame().unwrap()).collect();
    assert_eq!(frames.concat(), bytes, "the file is whole frames and nothing else");
    let samples = samples_v2();
    assert_eq!(frames.len(), 1 + samples.len());
    let hello = Hello { magic: SHELL.magic, proto: 2, build: FROZEN_BUILD_V2 };
    assert_eq!(decode_hello(&SHELL, &frames[0]), Ok((hello, frames[0].len())));
    let mut again = Vec::new();
    encode_hello(&SHELL, FROZEN_BUILD_V2, &mut again);
    assert_eq!(again, frames[0]);
    for (f, m) in frames[1..].iter().zip(&samples) {
        assert_eq!(Msg::decode_any(f), Ok((m.clone(), f.len())), "{}", m.name());
        assert_eq!(&m.frame(), f, "{} encodes to its frozen bytes", m.name());
    }
}

/// xorshift64* (Vigna), seeded: the same inputs on every run and machine.
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x >> 12;
        x ^= x << 25;
        x ^= x >> 27;
        self.0 = x;
        x.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }

    fn below(&mut self, n: usize) -> usize {
        (self.next() % n.max(1) as u64) as usize
    }

    fn bytes(&mut self, n: usize) -> Vec<u8> {
        (0..n).map(|_| self.next() as u8).collect()
    }
}

/// Random bytes, a random payload under the header of a known kind, a sample cut short, or a sample with one byte
/// changed.
fn fuzz_input(rng: &mut Rng, seeds: &[Vec<u8>], kinds: &[u16]) -> Vec<u8> {
    match rng.below(4) {
        0 => {
            let n = rng.below(64);
            rng.bytes(n)
        }
        1 => {
            let n = rng.below(96);
            let body = rng.bytes(n);
            raw(kinds[rng.below(kinds.len())], &body)
        }
        2 => {
            let s = &seeds[rng.below(seeds.len())];
            s[..rng.below(s.len())].to_vec()
        }
        _ => {
            let mut s = seeds[rng.below(seeds.len())].clone();
            let at = rng.below(s.len());
            s[at] ^= 1 + rng.below(255) as u8;
            s
        }
    }
}

/// Every decoder takes per_decoder inputs without panicking, every input it accepts encodes back to the same bytes,
/// and the stream assembler cuts the v1 stream the same way under random pushes.
fn fuzz(per_decoder: usize, seed: u64) {
    let samples = samples();
    for (name, lo, hi, dec, _) in ENUMS {
        let seeds: Vec<Vec<u8>> = samples.iter().filter(|m| m.enum_name() == name).map(Msg::frame).collect();
        let kinds: Vec<u16> =
            KINDS.iter().filter(|(n, _)| n.split("::").next() == Some(name)).map(|(_, k)| *k).collect();
        let mut rng = Rng(seed ^ (u64::from(lo) << 32) ^ u64::from(hi));
        let mut accepted = 0;
        for i in 0..per_decoder {
            let input = fuzz_input(&mut rng, &seeds, &kinds);
            if let Ok((m, used)) = dec(&input) {
                accepted += 1;
                assert_eq!(m.frame(), input[..used], "{name}, input {i}: {} does not encode back", m.name());
            }
        }
        assert!(accepted >= per_decoder / 50, "{name}: {accepted} of {per_decoder} inputs decoded");
    }
    let mut hello = Vec::new();
    encode_hello(&OPS, FROZEN_BUILD, &mut hello);
    let mut rng = Rng(seed ^ 0x0048_454C_4C4F);
    let mut accepted = 0;
    for i in 0..per_decoder {
        let input = fuzz_input(&mut rng, std::slice::from_ref(&hello), &[0]);
        if let Ok((h, used)) = decode_hello(&OPS, &input) {
            accepted += 1;
            let mut again = Vec::new();
            encode_hello(&OPS, h.build, &mut again);
            assert_eq!(again, input[..used], "Hello, input {i}");
        }
    }
    assert!(accepted >= per_decoder / 50, "Hello: {accepted} of {per_decoder} inputs decoded");
    let stream = v1_stream();
    let mut whole = Assembler::new(OPS.max_payload);
    whole.push(&stream);
    let want: Vec<Vec<u8>> = std::iter::from_fn(|| whole.next_frame().unwrap()).collect();
    for _ in 0..per_decoder / 1_000 {
        let mut asm = Assembler::new(OPS.max_payload);
        let (mut got, mut at) = (Vec::new(), 0);
        while at < stream.len() {
            let end = (at + 1 + rng.below(64)).min(stream.len());
            asm.push(&stream[at..end]);
            at = end;
            while let Some(f) = asm.next_frame().unwrap() {
                got.push(f);
            }
        }
        assert_eq!(got, want);
    }
}

#[test]
fn fuzz_smoke() {
    fuzz(100_000, 0x9E37_79B9_7F4A_7C15);
}

#[test]
#[ignore = "fuzz: nightly in the files-disposable job (trailing gate)"]
fn fuzz_1m() {
    fuzz(1_000_000, 0xD1B5_4A32_D192_ED03);
}
