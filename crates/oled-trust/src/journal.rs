//! The change journal: line format, records and the undo planner (F-P5).
//!
//! WHY: every change the suite makes to Windows outside its own folders (a registry value, key or tree, a scheduled
//! task, a Start shortcut, a swapped component) is written down and flushed before it is made. An undo after a crash,
//! a failed run, `--undo-user` or an uninstall then puts back what was there, and leaves alone what another program
//! changed since (ownership: only a target still in the state the change left it in is restored).
//!
//! THE LINE: `<crc32 of the json, 8 lower hex> <json>\n`, the json written by hand with F-P5's keys in F-P5's order (the
//! text is frozen, so no serde derives) and read back through `serde_json::Value` with every cap checked. A file is all or
//! nothing: one bad line refuses the whole journal, except a last line without its `\n` (a torn append), which is
//! ignored. Whatever parses can be written again: the parser accepts a record only when the encoder would write it.
//!
//! No OS calls and no hashing here: the registry, tasks, files and SHA-256 are `JournalOs`'s, appends are `Sink`'s.

use std::collections::BTreeSet;
use std::fmt::Write as _;

use serde_json::{Map, Value};

pub const JOURNAL_SCHEMA: u16 = 1;
pub const MAX_LINE: usize = 96 * 1024; // one encoded line, crc and '\n' included
pub const MAX_RECORDS: usize = 4096;
pub const SNAPSHOT_MAX: usize = 64 * 1024; // the ENCODED size of a RegSnapshot inside its line (the "before" object's JSON text)
pub const TASK_XML_MAX: usize = 32 * 1024; // the ENCODED size of a before_xml JSON string, quotes and escapes included
pub const SNAPSHOT_DEPTH_MAX: u8 = 16;
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Hive {
    Hkcu,
    Hklm,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RegPath(String); // no leading '\', no NUL, segments 1..=255, total <= 512
impl RegPath {
    pub fn new(s: &str) -> Result<RegPath, JournalError> {
        let ok = s.encode_utf16().count() <= REG_PATH_MAX
            && !s.contains('\0')
            && s.split('\\').all(|seg| (1..=REG_SEGMENT_MAX).contains(&seg.encode_utf16().count()));
        if ok {
            Ok(RegPath(s.to_string()))
        } else {
            Err(JournalError::BadPath)
        }
    }
    pub fn as_str(&self) -> &str {
        &self.0
    }
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RegData {
    Sz(String),
    ExpandSz(String),
    MultiSz(Vec<String>),
    Dword(u32),
    Qword(u64),
    Binary(Vec<u8>),
    Other { ty: u32, bytes: Vec<u8> },
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RegSnapshot {
    pub values: Vec<(Option<String>, RegData)>,
    pub subkeys: Vec<(String, RegSnapshot)>,
}
pub type Digest = [u8; 32];
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TaskName(String); // '\' then [A-Za-z0-9 ._-]{1,64}
impl TaskName {
    pub fn new(s: &str) -> Result<TaskName, JournalError> {
        let ok = s.strip_prefix('\\').is_some_and(|rest| {
            (1..=64).contains(&rest.len())
                && rest.bytes().all(|b| b.is_ascii_alphanumeric() || matches!(b, b' ' | b'.' | b'_' | b'-'))
        });
        if ok {
            Ok(TaskName(s.to_string()))
        } else {
            Err(JournalError::BadName)
        }
    }
    pub fn as_str(&self) -> &str {
        &self.0
    }
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Change {
    RegValue { hive: Hive, key: RegPath, name: Option<String>, before: Option<RegData>, after: Option<RegData> },
    RegKeyCreated { hive: Hive, key: RegPath }, // undo: delete the key with its whole subtree if it exists (the key is ours)
    RegTree { hive: Hive, key: RegPath, before: Option<RegSnapshot>, after: Digest }, // ROADMAP X31
    Task { name: TaskName, before_xml: Option<String>, after: Digest },
    FileCreated { path: String, after: Digest },
    FileSwap { dest: String, prev: Option<String>, staged_sha256: Digest }, // update.jnl only; prev None = an addition
    ServiceStartType { name: String, before: u32, after: u32 },             // parsed; never planned in v1
    Unknown(String), // a newer kind: its JSON text kept verbatim, never undone here
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TakeoverId {
    FolderVerbs,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AppId {
    Dock,
    Tasks,
    Files,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Kind {
    Takeover(TakeoverId),
    Install,
    Update,
    UserEdit(AppId),
    Unknown(String),
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum KindClass {
    Takeover,
    Install,
    Update,
    UserEdit,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Scope {
    User,
    Machine,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct RunId(pub u64);
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Entry {
    pub seq: u64,
    pub run: RunId,
    pub ts: u64,
    pub kind: Kind,
    pub change: Change,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Record {
    Entry(Entry),
    Undone { seq: u64 },
    RunFailed { run: RunId },
    Committed { run: RunId },
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum JournalError {
    Corrupt { line: u32 },
    TooLarge,
    BadPath,
    BadName,
    Version(u16),
}
/// The records of one journal file and the next free seq and run, read under the lock (journal_os::open_locked)
/// and kept up to date by every append made through this value.
pub struct Journal {
    scope: Scope,
    records: Vec<Record>,
    next_seq: u64,
    next_run: u64,
}
impl Journal {
    pub fn empty(scope: Scope) -> Journal {
        Journal { scope, records: Vec::new(), next_seq: 1, next_run: 1 }
    }
    pub fn parse(scope: Scope, bytes: &[u8]) -> Result<Journal, JournalError> {
        let mut j = Journal::empty(scope);
        let mut seqs = BTreeSet::new();
        let mut rest = bytes;
        let mut line_no: u32 = 0;
        while let Some(end) = rest.iter().position(|&b| b == b'\n') {
            line_no = line_no.saturating_add(1);
            let line = rest.get(..end).unwrap_or_default();
            rest = rest.get(end + 1..).unwrap_or_default();
            if j.records.len() >= MAX_RECORDS {
                return Err(JournalError::TooLarge);
            }
            let corrupt = JournalError::Corrupt { line: line_no };
            if line.len() >= MAX_LINE {
                return Err(corrupt);
            }
            let rec = parse_line(line).map_err(|e| match e {
                LineError::Version(v) => JournalError::Version(v),
                LineError::Corrupt => corrupt.clone(),
            })?;
            if let Record::Entry(e) = &rec {
                if !seqs.insert(e.seq) {
                    return Err(corrupt);
                }
            }
            j.push(rec);
        }
        // What is left has no '\n': an append cut by a crash, ignored (journal_os cuts it before the next append).
        Ok(j)
    } // all-or-nothing; a last line without '\n' is torn and ignored
    pub fn encode(r: &Record) -> Result<Vec<u8>, JournalError> {
        let json = record_json(r)?;
        let line = format!("{:08x} {json}\n", crc32(json.as_bytes()));
        if line.len() > MAX_LINE {
            return Err(JournalError::TooLarge);
        }
        Ok(line.into_bytes())
    } // the line below; TooLarge above MAX_LINE or a field cap
    pub fn records(&self) -> &[Record] {
        &self.records
    }
    pub fn open_entries(&self) -> Vec<&Entry> {
        let mut undone = BTreeSet::new();
        let mut closed_runs = BTreeSet::new();
        for r in &self.records {
            match r {
                Record::Undone { seq } => {
                    undone.insert(*seq);
                }
                Record::RunFailed { run } | Record::Committed { run } => {
                    closed_runs.insert(run.0);
                }
                Record::Entry(_) => {}
            }
        }
        self.records
            .iter()
            .filter_map(|r| match r {
                Record::Entry(e) if !undone.contains(&e.seq) && !closed_runs.contains(&e.run.0) => Some(e),
                _ => None,
            })
            .collect()
    } // not undone, run not failed, run not committed
    pub fn is_settled(&self) -> bool {
        self.open_entries().is_empty()
    } // no open entry: the file may be deleted
    pub fn begin_run(&mut self) -> RunId {
        let run = RunId(self.next_run);
        self.next_run = self.next_run.saturating_add(1);
        run
    } // max(run in the file) + 1
    pub fn next_seq(&self) -> u64 {
        self.next_seq
    } // max(seq in the file) + 1, read under the lock (journal_os::open_locked)

    /// Adds a record that is on disk (parsed, or appended through a Sink) and moves the counters past it.
    fn push(&mut self, r: Record) {
        let (seq, run) = match &r {
            Record::Entry(e) => (Some(e.seq), Some(e.run.0)),
            Record::Undone { seq } => (Some(*seq), None),
            Record::RunFailed { run } | Record::Committed { run } => (None, Some(run.0)),
        };
        if let Some(s) = seq {
            self.next_seq = self.next_seq.max(s.saturating_add(1));
        }
        if let Some(r) = run {
            self.next_run = self.next_run.max(r.saturating_add(1));
        }
        self.records.push(r);
    }
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OsErr {
    pub code: u32,
    pub what: String,
}
pub trait Sink {
    fn append(&mut self, bytes: &[u8]) -> Result<(), OsErr>;
} // returns only after FlushFileBuffers
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Target {
    Reg { hive: Hive, key: String, name: Option<String> },
    Task(String),
    File(String),
}
pub trait JournalOs {
    fn reg_get(&self, hive: Hive, key: &RegPath, name: Option<&str>) -> Result<Option<RegData>, OsErr>;
    fn reg_set(&mut self, hive: Hive, key: &RegPath, name: Option<&str>, data: Option<&RegData>) -> Result<(), OsErr>; // None = delete the value
    fn reg_key_exists(&self, hive: Hive, key: &RegPath) -> Result<bool, OsErr>;
    fn reg_key_create(&mut self, hive: Hive, key: &RegPath) -> Result<(), OsErr>;
    fn reg_key_delete_if_empty(&mut self, hive: Hive, key: &RegPath) -> Result<bool, OsErr>;
    fn reg_tree_get(&self, hive: Hive, key: &RegPath) -> Result<Option<RegSnapshot>, OsErr>;
    fn reg_tree_put(&mut self, hive: Hive, key: &RegPath, snap: Option<&RegSnapshot>) -> Result<(), OsErr>; // replaces the whole subtree; None deletes it
    fn task_xml(&self, name: &TaskName) -> Result<Option<String>, OsErr>;
    fn task_put(&mut self, name: &TaskName, xml: Option<&str>) -> Result<(), OsErr>; // None = delete
    fn file_digest(&self, path: &str) -> Result<Option<Digest>, OsErr>;
    fn file_delete(&mut self, path: &str) -> Result<(), OsErr>;
    fn file_restore(&mut self, dest: &str, from: &str) -> Result<(), OsErr>; // FileSwap undo
    fn digest(&self, bytes: &[u8]) -> Result<Digest, OsErr>; // SHA-256; RealOs = oled_sys::sha256 (BCryptHash), FakeOs = a test hash
}
pub fn canonical_tree(s: &RegSnapshot) -> Vec<u8> {
    let mut out = String::new();
    put_tree(&mut out, s);
    out.into_bytes()
} // the snapshot's JSON text exactly as in a line
pub fn digest_tree(s: &RegSnapshot, os: &dyn JournalOs) -> Result<Digest, OsErr> {
    os.digest(&canonical_tree(s))
} // os.digest(&canonical_tree(s))
pub fn digest_text(s: &str, os: &dyn JournalOs) -> Result<Digest, OsErr> {
    os.digest(s.as_bytes())
} // os.digest(s.as_bytes())
/// Appends one entry (the next seq of this journal) and returns its seq. A change on the hive the journal's scope does
/// not hold, a full journal (MAX_RECORDS) and a change no line can hold are refused before the sink is called.
pub fn record(
    j: &mut Journal,
    sink: &mut dyn Sink,
    run: RunId,
    kind: Kind,
    change: Change,
    now: u64,
) -> Result<u64, OsErr> {
    if !hive_allowed(j.scope, &change) {
        return Err(os_err(ERROR_ACCESS_DENIED, "journal: the change is outside the journal's scope"));
    }
    record_entry(j, sink, run, kind, change, now).map_err(RecErr::into_os)
}
pub fn commit(j: &mut Journal, sink: &mut dyn Sink, run: RunId) -> Result<(), OsErr> {
    append(j, sink, Record::Committed { run }).map_err(RecErr::into_os)
} // appends Committed{run}
/// A list of changes to make as one run. `apply` makes RegValue, RegKeyCreated, RegTree and Task changes; it fills every
/// "before" from the OS, so the before slot of a planned RegTree carries the tree to write (in canonical order: values
/// None first then by name, subkeys by name, no two names equal ignoring case) and the before_xml slot of a planned
/// Task the XML to register, each matching its `after` digest. FileCreated and FileSwap are journaled with `record`
/// by the code that writes the file; ServiceStartType and Unknown are never planned.
pub struct Plan {
    pub kind: Kind,
    pub changes: Vec<Change>,
} // "before" fields are filled by apply from the OS
/// Makes a plan as one run: for each change, its before is read from the OS (or taken from the first open entry on the
/// same target: the first original wins), the entry is recorded and flushed, then the change is made. A change that
/// would create missing keys records a RegKeyCreated for the topmost of them first, so its undo leaves no empty key. A
/// planned RegKeyCreated on a key that already exists records and changes nothing: the key is not ours, so no undo
/// may delete it (an earlier open entry still covers a key an interrupted run of ours created).
/// Every cap is checked for the whole plan before the first change (TooLarge(index), nothing changed or recorded).
/// When a change fails, the changes of the run made so far are undone and the run is marked RunFailed (unless an
/// undo itself failed: those entries stay open for a later undo).
pub fn apply(
    j: &mut Journal,
    sink: &mut dyn Sink,
    os: &mut dyn JournalOs,
    plan: Plan,
    now: u64,
) -> Result<RunId, ApplyError> {
    // Pass 1: every change planned against the state as it is, and every cap checked, before anything changes.
    let mut entries = 0usize;
    for (i, c) in plan.changes.iter().enumerate() {
        let Some(p) = plan_one(j, &*os, c).map_err(|r| r.into_apply(i, UndoReport::default()))? else { continue };
        entries += 1 + usize::from(p.creates.is_some());
        let probe = Entry {
            seq: j.next_seq.saturating_add(entries as u64),
            run: RunId(j.next_run),
            ts: now,
            kind: plan.kind.clone(),
            change: p.change,
        };
        Journal::encode(&Record::Entry(probe))
            .map_err(|e| Refusal::from_line(e).into_apply(i, UndoReport::default()))?;
        // Room for the entries, an Undone for each if the run must be undone, and its RunFailed.
        if j.records.len() + 2 * entries + 1 > MAX_RECORDS {
            return Err(ApplyError::TooLarge(i));
        }
    }
    // Pass 2: plan again on the live state (earlier changes of this run count), record, flush, change.
    let run = j.begin_run();
    for (i, c) in plan.changes.iter().enumerate() {
        let p = match plan_one(j, &*os, c) {
            Ok(Some(p)) => p,
            Ok(None) => continue,
            Err(r) => return Err(r.into_apply(i, rollback(j, sink, os, run))),
        };
        if let (Some(created), Some(hive)) = (&p.creates, hive_of(&p.change)) {
            let rc = Change::RegKeyCreated { hive, key: created.clone() };
            if let Err(e) = record_entry(j, sink, run, plan.kind.clone(), rc, now) {
                let undo = rollback(j, sink, os, run);
                return Err(e.into_apply(i, undo));
            }
        }
        if let Err(e) = record_entry(j, sink, run, plan.kind.clone(), p.change.clone(), now) {
            let undo = rollback(j, sink, os, run);
            return Err(e.into_apply(i, undo));
        }
        if let Err(err) = perform(os, &p) {
            let undo = rollback(j, sink, os, run);
            return Err(ApplyError::Os { at: i, err, undo });
        }
    }
    Ok(run)
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ApplyError {
    Os { at: usize, err: OsErr, undo: UndoReport },
    Record(OsErr),
    TooLarge(usize),
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum UndoSet {
    Classes(Vec<KindClass>),
    Run(RunId),
    Seqs(Vec<u64>),
}
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct UndoReport {
    pub restored: u32,
    pub already: u32,
    pub changed_by_other: Vec<Target>,
    pub failed: Vec<(Target, OsErr)>,
    pub unknown: u32,
}
/// Undoes the selected open entries, newest first, with F-P5's per-change rule. A restored entry, one already in its
/// before state and one another program changed since are closed with Undone{seq} (the last is left as it is: it is
/// no longer ours); a failed one stays open for a later undo; Unknown and ServiceStartType changes are counted and
/// never touched. Kind::Unknown entries belong to no class: only Run and Seqs select them.
pub fn undo(j: &mut Journal, sink: &mut dyn Sink, os: &mut dyn JournalOs, set: &UndoSet) -> UndoReport {
    let mut chosen: Vec<Entry> = j.open_entries().into_iter().filter(|e| selected(set, e)).cloned().collect();
    chosen.sort_by(|a, b| b.seq.cmp(&a.seq));
    let mut report = UndoReport::default();
    for e in chosen {
        let close = match undo_change(os, &e.change) {
            Ok(Undone::Restored) => {
                report.restored += 1;
                true
            }
            Ok(Undone::Already) => {
                report.already += 1;
                true
            }
            Ok(Undone::ChangedByOther) => {
                report.changed_by_other.extend(target_of(&e.change));
                true
            }
            Ok(Undone::NotOurs) => {
                report.unknown += 1;
                false
            }
            Err(err) => {
                if let Some(t) = target_of(&e.change) {
                    report.failed.push((t, err));
                }
                false
            }
        };
        if close {
            // A lost Undone is harmless: the entry is then found in its before state and closed again.
            let _ = append(j, sink, Record::Undone { seq: e.seq });
        }
    }
    report
} // newest first, idempotent
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EntryView {
    pub seq: u64,
    pub kind: Kind,
    pub target: Target,
    pub ts: u64,
}
/// One view per open entry, in file order; an Unknown change (a newer version's) has no target and is not listed.
pub fn views(j: &Journal) -> Vec<EntryView> {
    j.open_entries()
        .into_iter()
        .filter_map(|e| {
            target_of(&e.change).map(|target| EntryView { seq: e.seq, kind: e.kind.clone(), target, ts: e.ts })
        })
        .collect()
}

// ------------------------------------------------------------------------------------------------ names and limits

/// A registry path: at most 512 UTF-16 units, segments of 1..=255 (the registry's key name limit).
const REG_PATH_MAX: usize = 512;
const REG_SEGMENT_MAX: usize = 255;
/// The registry's value name limit, in UTF-16 units.
const VALUE_NAME_MAX: usize = 16_383;
/// A kind string, in bytes.
const KIND_MAX: usize = 64;
/// A service name, in UTF-16 units (the service manager's limit).
const SERVICE_NAME_MAX: usize = 256;

const ERROR_ACCESS_DENIED: u32 = 5;
const ERROR_INVALID_PARAMETER: u32 = 87;
const ERROR_BUFFER_OVERFLOW: u32 = 111;
const ERROR_DISK_FULL: u32 = 112;

fn os_err(code: u32, what: &str) -> OsErr {
    OsErr { code, what: what.to_string() }
}

fn kind_name(k: &Kind) -> &str {
    match k {
        Kind::Takeover(TakeoverId::FolderVerbs) => "takeover.folder_verbs",
        Kind::Install => "install",
        Kind::Update => "update",
        Kind::UserEdit(AppId::Dock) => "user_edit.dock",
        Kind::UserEdit(AppId::Tasks) => "user_edit.tasks",
        Kind::UserEdit(AppId::Files) => "user_edit.files",
        Kind::Unknown(s) => s,
    }
}

fn kind_of(s: &str) -> Kind {
    match s {
        "takeover.folder_verbs" => Kind::Takeover(TakeoverId::FolderVerbs),
        "install" => Kind::Install,
        "update" => Kind::Update,
        "user_edit.dock" => Kind::UserEdit(AppId::Dock),
        "user_edit.tasks" => Kind::UserEdit(AppId::Tasks),
        "user_edit.files" => Kind::UserEdit(AppId::Files),
        other => Kind::Unknown(other.to_string()),
    }
}

fn class_of(k: &Kind) -> Option<KindClass> {
    match k {
        Kind::Takeover(_) => Some(KindClass::Takeover),
        Kind::Install => Some(KindClass::Install),
        Kind::Update => Some(KindClass::Update),
        Kind::UserEdit(_) => Some(KindClass::UserEdit),
        Kind::Unknown(_) => None,
    }
}

const KNOWN_CHANGES: [&str; 7] =
    ["reg_value", "reg_key_created", "reg_tree", "task", "file_created", "file_swap", "service_start_type"];

fn hive_name(h: Hive) -> &'static str {
    match h {
        Hive::Hkcu => "hkcu",
        Hive::Hklm => "hklm",
    }
}

fn hive_of(c: &Change) -> Option<Hive> {
    match c {
        Change::RegValue { hive, .. } | Change::RegKeyCreated { hive, .. } | Change::RegTree { hive, .. } => {
            Some(*hive)
        }
        Change::ServiceStartType { .. } => Some(Hive::Hklm),
        _ => None,
    }
}

/// The user journal holds HKCU changes only, a machine journal HKLM changes only (RealOs refuses the other hive too).
fn hive_allowed(scope: Scope, c: &Change) -> bool {
    match (scope, hive_of(c)) {
        (_, None) => true,
        (Scope::User, Some(h)) => h == Hive::Hkcu,
        (Scope::Machine, Some(h)) => h == Hive::Hklm,
    }
}

fn units(s: &str) -> usize {
    s.encode_utf16().count()
}

fn value_name_ok(n: &str) -> bool {
    !n.contains('\0') && units(n) <= VALUE_NAME_MAX
}

fn subkey_name_ok(n: &str) -> bool {
    !n.contains('\0') && !n.contains('\\') && (1..=REG_SEGMENT_MAX).contains(&units(n))
}

fn path_ok(p: &str) -> bool {
    !p.is_empty() && !p.contains('\0')
}

/// Depth (the root is level 1, at most SNAPSHOT_DEPTH_MAX levels) and the names of a snapshot.
fn check_tree(s: &RegSnapshot, level: u8) -> Result<(), JournalError> {
    if level > SNAPSHOT_DEPTH_MAX {
        return Err(JournalError::TooLarge);
    }
    if !s.values.iter().all(|(n, _)| n.as_deref().is_none_or(value_name_ok)) {
        return Err(JournalError::BadName);
    }
    for (n, t) in &s.subkeys {
        if !subkey_name_ok(n) {
            return Err(JournalError::BadName);
        }
        check_tree(t, level.saturating_add(1))?;
    }
    Ok(())
}

/// The order reg_tree_get gives and a planned tree must have: values None first then by name, subkeys by name, each
/// strictly increasing and no two names equal ignoring case (the registry's names are case-insensitive).
fn canonical_order(s: &RegSnapshot) -> bool {
    let values_sorted = s.values.windows(2).all(|w| matches!(w, [a, b] if a.0 < b.0));
    let subkeys_sorted = s.subkeys.windows(2).all(|w| matches!(w, [a, b] if a.0 < b.0));
    let folded: BTreeSet<String> = s.values.iter().filter_map(|(n, _)| n.as_deref()).map(str::to_lowercase).collect();
    let folded_keys: BTreeSet<String> = s.subkeys.iter().map(|(n, _)| n.to_lowercase()).collect();
    values_sorted
        && subkeys_sorted
        && folded.len() == s.values.iter().filter(|(n, _)| n.is_some()).count()
        && folded_keys.len() == s.subkeys.len()
        && s.subkeys.iter().all(|(_, t)| canonical_order(t))
}

// ------------------------------------------------------------------------------------------------ the line, written

const CRC_TABLE: [u32; 256] = {
    let mut t = [0u32; 256];
    let mut i = 0;
    while i < 256 {
        let mut c = i as u32;
        let mut k = 0;
        while k < 8 {
            c = if c & 1 != 0 { 0xEDB8_8320 ^ (c >> 1) } else { c >> 1 };
            k += 1;
        }
        t[i] = c;
        i += 1;
    }
    t
};

/// CRC-32/IEEE (reflected 0xEDB88320, init and xorout 0xFFFFFFFF).
fn crc32(data: &[u8]) -> u32 {
    let mut c = 0xFFFF_FFFFu32;
    for &b in data {
        c = CRC_TABLE[((c ^ u32::from(b)) & 0xFF) as usize] ^ (c >> 8);
    }
    c ^ 0xFFFF_FFFF
}

const HEX: &[u8; 16] = b"0123456789abcdef";

/// A JSON string with serde_json's escaping: quote, backslash and the C0 controls escaped (\b \t \n \f \r, the rest
/// as \u00xx in lower hex), everything else as UTF-8.
fn put_str(out: &mut String, s: &str) {
    out.push('"');
    for ch in s.chars() {
        match ch {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\u{8}' => out.push_str("\\b"),
            '\u{c}' => out.push_str("\\f"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if u32::from(c) < 0x20 => {
                let b = u32::from(c) as u8;
                out.push_str("\\u00");
                out.push(char::from(HEX[usize::from(b >> 4)]));
                out.push(char::from(HEX[usize::from(b & 0xF)]));
            }
            c => out.push(c),
        }
    }
    out.push('"');
}

fn put_opt_str(out: &mut String, s: Option<&str>) {
    match s {
        Some(s) => put_str(out, s),
        None => out.push_str("null"),
    }
}

fn put_hex(out: &mut String, bytes: &[u8]) {
    out.push('"');
    for &b in bytes {
        out.push(char::from(HEX[usize::from(b >> 4)]));
        out.push(char::from(HEX[usize::from(b & 0xF)]));
    }
    out.push('"');
}

fn put_num(out: &mut String, n: u64) {
    let _ = write!(out, "{n}");
}

fn put_data(out: &mut String, d: &RegData) {
    match d {
        RegData::Sz(s) => {
            out.push_str(r#"{"t":"sz","s":"#);
            put_str(out, s);
        }
        RegData::ExpandSz(s) => {
            out.push_str(r#"{"t":"expand_sz","s":"#);
            put_str(out, s);
        }
        RegData::MultiSz(l) => {
            out.push_str(r#"{"t":"multi_sz","l":["#);
            for (i, s) in l.iter().enumerate() {
                if i > 0 {
                    out.push(',');
                }
                put_str(out, s);
            }
            out.push(']');
        }
        RegData::Dword(n) => {
            out.push_str(r#"{"t":"dword","n":"#);
            put_num(out, u64::from(*n));
        }
        RegData::Qword(n) => {
            out.push_str(r#"{"t":"qword","n":"#);
            put_num(out, *n);
        }
        RegData::Binary(b) => {
            out.push_str(r#"{"t":"binary","hex":"#);
            put_hex(out, b);
        }
        RegData::Other { ty, bytes } => {
            out.push_str(r#"{"t":"other","ty":"#);
            put_num(out, u64::from(*ty));
            out.push_str(r#","hex":"#);
            put_hex(out, bytes);
        }
    }
    out.push('}');
}

fn put_opt_data(out: &mut String, d: Option<&RegData>) {
    match d {
        Some(d) => put_data(out, d),
        None => out.push_str("null"),
    }
}

fn put_tree(out: &mut String, s: &RegSnapshot) {
    out.push_str(r#"{"values":["#);
    for (i, (name, data)) in s.values.iter().enumerate() {
        if i > 0 {
            out.push(',');
        }
        out.push_str(r#"{"name":"#);
        put_opt_str(out, name.as_deref());
        out.push_str(r#","data":"#);
        put_data(out, data);
        out.push('}');
    }
    out.push_str(r#"],"subkeys":["#);
    for (i, (name, tree)) in s.subkeys.iter().enumerate() {
        if i > 0 {
            out.push(',');
        }
        out.push_str(r#"{"name":"#);
        put_str(out, name);
        out.push_str(r#","tree":"#);
        put_tree(out, tree);
        out.push('}');
    }
    out.push_str("]}");
}

fn put_key(out: &mut String, t: &str, hive: Hive, key: &RegPath) {
    out.push_str(r#"{"t":"#);
    put_str(out, t);
    out.push_str(r#","hive":"#);
    put_str(out, hive_name(hive));
    out.push_str(r#","key":"#);
    put_str(out, key.as_str());
}

/// An Unknown change is kept only as what a line can carry again: one JSON object on one line whose "t" is a string
/// this version does not know.
fn unknown_ok(text: &str) -> bool {
    !text.contains('\n')
        && text.starts_with('{')
        && text.ends_with('}')
        && serde_json::from_str::<Value>(text)
            .ok()
            .is_some_and(|v| v.get("t").and_then(Value::as_str).is_some_and(|t| !KNOWN_CHANGES.contains(&t)))
}

fn put_change(out: &mut String, c: &Change) -> Result<(), JournalError> {
    match c {
        Change::RegValue { hive, key, name, before, after } => {
            if !name.as_deref().is_none_or(value_name_ok) {
                return Err(JournalError::BadName);
            }
            put_key(out, "reg_value", *hive, key);
            out.push_str(r#","name":"#);
            put_opt_str(out, name.as_deref());
            out.push_str(r#","before":"#);
            put_opt_data(out, before.as_ref());
            out.push_str(r#","after":"#);
            put_opt_data(out, after.as_ref());
        }
        Change::RegKeyCreated { hive, key } => put_key(out, "reg_key_created", *hive, key),
        Change::RegTree { hive, key, before, after } => {
            put_key(out, "reg_tree", *hive, key);
            out.push_str(r#","before":"#);
            match before {
                Some(t) => {
                    check_tree(t, 1)?;
                    let text = canonical_tree(t);
                    if text.len() > SNAPSHOT_MAX {
                        return Err(JournalError::TooLarge);
                    }
                    out.push_str(std::str::from_utf8(&text).map_err(|_| JournalError::TooLarge)?);
                }
                None => out.push_str("null"),
            }
            out.push_str(r#","after":"#);
            put_hex(out, after);
        }
        Change::Task { name, before_xml, after } => {
            out.push_str(r#"{"t":"task","name":"#);
            put_str(out, name.as_str());
            out.push_str(r#","before_xml":"#);
            let at = out.len();
            put_opt_str(out, before_xml.as_deref());
            if before_xml.is_some() && out.len() - at > TASK_XML_MAX {
                return Err(JournalError::TooLarge);
            }
            out.push_str(r#","after":"#);
            put_hex(out, after);
        }
        Change::FileCreated { path, after } => {
            if !path_ok(path) {
                return Err(JournalError::BadPath);
            }
            out.push_str(r#"{"t":"file_created","path":"#);
            put_str(out, path);
            out.push_str(r#","after":"#);
            put_hex(out, after);
        }
        Change::FileSwap { dest, prev, staged_sha256 } => {
            if !path_ok(dest) || !prev.as_deref().is_none_or(path_ok) {
                return Err(JournalError::BadPath);
            }
            out.push_str(r#"{"t":"file_swap","dest":"#);
            put_str(out, dest);
            out.push_str(r#","prev":"#);
            put_opt_str(out, prev.as_deref());
            out.push_str(r#","staged":"#);
            put_hex(out, staged_sha256);
        }
        Change::ServiceStartType { name, before, after } => {
            if name.is_empty() || name.contains(['\0', '\\']) || units(name) > SERVICE_NAME_MAX {
                return Err(JournalError::BadName);
            }
            out.push_str(r#"{"t":"service_start_type","name":"#);
            put_str(out, name);
            out.push_str(r#","before":"#);
            put_num(out, u64::from(*before));
            out.push_str(r#","after":"#);
            put_num(out, u64::from(*after));
        }
        Change::Unknown(text) => {
            if !unknown_ok(text) {
                return Err(JournalError::Corrupt { line: 0 });
            }
            out.push_str(text);
            return Ok(());
        }
    }
    out.push('}');
    Ok(())
}

/// The JSON object of a record. Errors: TooLarge for a cap, BadName and BadPath for a name or path no OS call would
/// take, Corrupt { line: 0 } for an Unknown change or kind that no line could carry back.
fn record_json(r: &Record) -> Result<String, JournalError> {
    let mut out = String::with_capacity(256);
    match r {
        Record::Entry(e) => {
            let kind = kind_name(&e.kind);
            if let Kind::Unknown(k) = &e.kind {
                if k.len() > KIND_MAX {
                    return Err(JournalError::TooLarge);
                }
                if !matches!(kind_of(k), Kind::Unknown(_)) {
                    return Err(JournalError::Corrupt { line: 0 });
                }
            }
            out.push_str(r#"{"r":"entry","v":"#);
            put_num(&mut out, u64::from(JOURNAL_SCHEMA));
            out.push_str(r#","seq":"#);
            put_num(&mut out, e.seq);
            out.push_str(r#","run":"#);
            put_num(&mut out, e.run.0);
            out.push_str(r#","ts":"#);
            put_num(&mut out, e.ts);
            out.push_str(r#","kind":"#);
            put_str(&mut out, kind);
            out.push_str(r#","change":"#);
            put_change(&mut out, &e.change)?;
        }
        Record::Undone { seq } => {
            out.push_str(r#"{"r":"undone","seq":"#);
            put_num(&mut out, *seq);
        }
        Record::RunFailed { run } => {
            out.push_str(r#"{"r":"run_failed","run":"#);
            put_num(&mut out, run.0);
        }
        Record::Committed { run } => {
            out.push_str(r#"{"r":"committed","run":"#);
            put_num(&mut out, run.0);
        }
    }
    out.push('}');
    Ok(out)
}

// ------------------------------------------------------------------------------------------------ the line, read

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum LineError {
    Corrupt,
    Version(u16),
}

fn parse_line(line: &[u8]) -> Result<Record, LineError> {
    let c = LineError::Corrupt;
    let want = line.get(..8).and_then(parse_crc).ok_or(c)?;
    let json = line.get(8..).and_then(|r| r.strip_prefix(b" ")).ok_or(c)?;
    if crc32(json) != want {
        return Err(c);
    }
    let text = std::str::from_utf8(json).map_err(|_| c)?;
    let v: Value = serde_json::from_str(text).map_err(|_| c)?;
    let rec = parse_record(&v, text)?;
    // What parses can be written again: the encoder's caps, names and paths hold for every record read.
    Journal::encode(&rec).map_err(|_| c)?;
    Ok(rec)
}

fn hex_digit(b: u8) -> Option<u8> {
    match b {
        b'0'..=b'9' => Some(b - b'0'),
        b'a'..=b'f' => Some(b - b'a' + 10),
        _ => None,
    }
}

fn parse_crc(t: &[u8]) -> Option<u32> {
    if t.len() != 8 {
        return None;
    }
    t.iter().try_fold(0u32, |acc, &b| Some((acc << 4) | u32::from(hex_digit(b)?)))
}

/// Lower hex, even length.
fn parse_hex(s: &str) -> Option<Vec<u8>> {
    let b = s.as_bytes();
    if !b.len().is_multiple_of(2) {
        return None;
    }
    b.chunks_exact(2).map(|p| Some((hex_digit(*p.first()?)? << 4) | hex_digit(*p.get(1)?)?)).collect()
}

fn digest_of(o: &Map<String, Value>, k: &str) -> Option<Digest> {
    let s = o.get(k)?.as_str()?;
    if s.len() != 64 {
        return None;
    }
    parse_hex(s)?.try_into().ok()
}

fn str_of<'a>(o: &'a Map<String, Value>, k: &str) -> Option<&'a str> {
    o.get(k)?.as_str()
}

fn u64_of(o: &Map<String, Value>, k: &str) -> Option<u64> {
    o.get(k)?.as_u64()
}

fn u32_of(o: &Map<String, Value>, k: &str) -> Option<u32> {
    u32::try_from(u64_of(o, k)?).ok()
}

/// A key that must be present, as a string or null.
fn opt_str_of(o: &Map<String, Value>, k: &str) -> Option<Option<String>> {
    match o.get(k)? {
        Value::Null => Some(None),
        Value::String(s) => Some(Some(s.clone())),
        _ => None,
    }
}

fn hive_of_str(o: &Map<String, Value>) -> Option<Hive> {
    match str_of(o, "hive")? {
        "hkcu" => Some(Hive::Hkcu),
        "hklm" => Some(Hive::Hklm),
        _ => None,
    }
}

fn key_of(o: &Map<String, Value>) -> Option<RegPath> {
    RegPath::new(str_of(o, "key")?).ok()
}

fn parse_data(v: &Value) -> Option<RegData> {
    let o = v.as_object()?;
    Some(match str_of(o, "t")? {
        "sz" => RegData::Sz(str_of(o, "s")?.to_string()),
        "expand_sz" => RegData::ExpandSz(str_of(o, "s")?.to_string()),
        "multi_sz" => RegData::MultiSz(
            o.get("l")?.as_array()?.iter().map(|s| s.as_str().map(str::to_string)).collect::<Option<_>>()?,
        ),
        "dword" => RegData::Dword(u32_of(o, "n")?),
        "qword" => RegData::Qword(u64_of(o, "n")?),
        "binary" => RegData::Binary(parse_hex(str_of(o, "hex")?)?),
        "other" => RegData::Other { ty: u32_of(o, "ty")?, bytes: parse_hex(str_of(o, "hex")?)? },
        _ => return None,
    })
}

fn opt_data_of(o: &Map<String, Value>, k: &str) -> Option<Option<RegData>> {
    match o.get(k)? {
        Value::Null => Some(None),
        v => Some(Some(parse_data(v)?)),
    }
}

fn parse_tree(v: &Value, level: u8) -> Option<RegSnapshot> {
    if level > SNAPSHOT_DEPTH_MAX {
        return None;
    }
    let o = v.as_object()?;
    let mut values = Vec::new();
    for item in o.get("values")?.as_array()? {
        let io = item.as_object()?;
        values.push((opt_str_of(io, "name")?, parse_data(io.get("data")?)?));
    }
    let mut subkeys = Vec::new();
    for item in o.get("subkeys")?.as_array()? {
        let io = item.as_object()?;
        subkeys.push((str_of(io, "name")?.to_string(), parse_tree(io.get("tree")?, level.saturating_add(1))?));
    }
    Some(RegSnapshot { values, subkeys })
}

fn parse_change(v: &Value, text: &str) -> Option<Change> {
    let o = v.as_object()?;
    Some(match str_of(o, "t")? {
        "reg_value" => Change::RegValue {
            hive: hive_of_str(o)?,
            key: key_of(o)?,
            name: opt_str_of(o, "name")?,
            before: opt_data_of(o, "before")?,
            after: opt_data_of(o, "after")?,
        },
        "reg_key_created" => Change::RegKeyCreated { hive: hive_of_str(o)?, key: key_of(o)? },
        "reg_tree" => Change::RegTree {
            hive: hive_of_str(o)?,
            key: key_of(o)?,
            before: match o.get("before")? {
                Value::Null => None,
                t => Some(parse_tree(t, 1)?),
            },
            after: digest_of(o, "after")?,
        },
        "task" => Change::Task {
            name: TaskName::new(str_of(o, "name")?).ok()?,
            before_xml: opt_str_of(o, "before_xml")?,
            after: digest_of(o, "after")?,
        },
        "file_created" => Change::FileCreated { path: str_of(o, "path")?.to_string(), after: digest_of(o, "after")? },
        "file_swap" => Change::FileSwap {
            dest: str_of(o, "dest")?.to_string(),
            prev: opt_str_of(o, "prev")?,
            staged_sha256: digest_of(o, "staged")?,
        },
        "service_start_type" => Change::ServiceStartType {
            name: str_of(o, "name")?.to_string(),
            before: u32_of(o, "before")?,
            after: u32_of(o, "after")?,
        },
        _ => Change::Unknown(member_text(text, "change")?.to_string()),
    })
}

fn parse_record(v: &Value, text: &str) -> Result<Record, LineError> {
    let c = LineError::Corrupt;
    let o = v.as_object().ok_or(c)?;
    match str_of(o, "r").ok_or(c)? {
        "entry" => {
            let version = u64_of(o, "v").ok_or(c)?;
            if version > u64::from(JOURNAL_SCHEMA) {
                return Err(LineError::Version(u16::try_from(version).unwrap_or(u16::MAX)));
            }
            if version == 0 {
                return Err(c);
            }
            let kind = str_of(o, "kind").filter(|k| k.len() <= KIND_MAX).map(kind_of).ok_or(c)?;
            Ok(Record::Entry(Entry {
                seq: u64_of(o, "seq").ok_or(c)?,
                run: RunId(u64_of(o, "run").ok_or(c)?),
                ts: u64_of(o, "ts").ok_or(c)?,
                kind,
                change: o.get("change").and_then(|ch| parse_change(ch, text)).ok_or(c)?,
            }))
        }
        "undone" => Ok(Record::Undone { seq: u64_of(o, "seq").ok_or(c)? }),
        "run_failed" => Ok(Record::RunFailed { run: RunId(u64_of(o, "run").ok_or(c)?) }),
        "committed" => Ok(Record::Committed { run: RunId(u64_of(o, "run").ok_or(c)?) }),
        _ => Err(c),
    }
}

fn skip_ws(b: &[u8], mut i: usize) -> usize {
    while matches!(b.get(i), Some(b' ' | b'\t' | b'\n' | b'\r')) {
        i += 1;
    }
    i
}

/// The index just past the JSON string that starts at `i` (its opening quote).
fn string_end(b: &[u8], i: usize) -> Option<usize> {
    let mut j = i + 1;
    loop {
        match b.get(j)? {
            b'"' => return Some(j + 1),
            b'\\' => j += 2,
            _ => j += 1,
        }
    }
}

/// The index just past the JSON value that starts at `i`.
fn value_end(b: &[u8], i: usize) -> Option<usize> {
    match b.get(i)? {
        b'"' => string_end(b, i),
        b'{' | b'[' => {
            let mut depth = 0usize;
            let mut j = i;
            loop {
                match b.get(j)? {
                    b'"' => {
                        j = string_end(b, j)?;
                        continue;
                    }
                    b'{' | b'[' => depth += 1,
                    b'}' | b']' => {
                        depth = depth.checked_sub(1)?;
                        if depth == 0 {
                            return Some(j + 1);
                        }
                    }
                    _ => {}
                }
                j += 1;
            }
        }
        _ => {
            let mut j = i;
            while !matches!(b.get(j), None | Some(b',' | b'}' | b']' | b' ' | b'\t' | b'\n' | b'\r')) {
                j += 1;
            }
            (j > i).then_some(j)
        }
    }
}

/// The text of member `key` of the JSON object `s` (serde_json read it first, so it is well formed), the last one
/// when a key repeats, as serde_json keeps: an Unknown change keeps its text verbatim.
fn member_text<'a>(s: &'a str, key: &str) -> Option<&'a str> {
    let b = s.as_bytes();
    let mut i = skip_ws(b, 0);
    if b.get(i) != Some(&b'{') {
        return None;
    }
    i += 1;
    let mut found = None;
    loop {
        i = skip_ws(b, i);
        match b.get(i)? {
            b'}' => return found,
            b',' => {
                i += 1;
                continue;
            }
            b'"' => {}
            _ => return None,
        }
        let key_end = string_end(b, i)?;
        let name: String = serde_json::from_str(s.get(i..key_end)?).ok()?;
        i = skip_ws(b, key_end);
        if b.get(i) != Some(&b':') {
            return None;
        }
        i = skip_ws(b, i + 1);
        let end = value_end(b, i)?;
        if name == key {
            found = Some(s.get(i..end)?);
        }
        i = end;
    }
}

// ------------------------------------------------------------------------------------------------ appends

enum RecErr {
    Line(JournalError),
    Os(OsErr),
}

impl RecErr {
    fn into_os(self) -> OsErr {
        match self {
            RecErr::Line(JournalError::TooLarge) => {
                os_err(ERROR_BUFFER_OVERFLOW, "journal: the record does not fit in a line")
            }
            RecErr::Line(_) => os_err(ERROR_INVALID_PARAMETER, "journal: the record cannot be written"),
            RecErr::Os(e) => e,
        }
    }

    fn into_apply(self, at: usize, undo: UndoReport) -> ApplyError {
        match self {
            RecErr::Line(e) => Refusal::from_line(e).into_apply(at, undo),
            RecErr::Os(e) => ApplyError::Record(e),
        }
    }
}

/// Appends a record and adds it to the journal; nothing is added when the journal is full or the sink fails.
fn append(j: &mut Journal, sink: &mut dyn Sink, r: Record) -> Result<(), RecErr> {
    if j.records.len() >= MAX_RECORDS {
        return Err(RecErr::Os(os_err(ERROR_DISK_FULL, "journal: full")));
    }
    let line = Journal::encode(&r).map_err(RecErr::Line)?;
    sink.append(&line).map_err(RecErr::Os)?;
    j.push(r);
    Ok(())
}

fn record_entry(
    j: &mut Journal,
    sink: &mut dyn Sink,
    run: RunId,
    kind: Kind,
    change: Change,
    now: u64,
) -> Result<u64, RecErr> {
    let seq = j.next_seq;
    append(j, sink, Record::Entry(Entry { seq, run, ts: now, kind, change }))?;
    Ok(seq)
}

// ------------------------------------------------------------------------------------------------ apply

/// Why a planned change cannot be made.
enum Refusal {
    TooLarge,
    Invalid(&'static str),
    Os(OsErr),
}

impl Refusal {
    fn from_line(e: JournalError) -> Refusal {
        match e {
            JournalError::TooLarge => Refusal::TooLarge,
            _ => Refusal::Invalid("journal: the change cannot be written"),
        }
    }

    fn into_apply(self, at: usize, undo: UndoReport) -> ApplyError {
        match self {
            Refusal::TooLarge => ApplyError::TooLarge(at),
            Refusal::Invalid(what) => ApplyError::Os { at, err: os_err(ERROR_INVALID_PARAMETER, what), undo },
            Refusal::Os(err) => ApplyError::Os { at, err, undo },
        }
    }
}

/// What a planned change writes beside its entry.
enum Content {
    Nothing,
    Tree(RegSnapshot),
    Xml(String),
}

struct Planned {
    /// The entry's change, its before read from the OS or taken from the first original.
    change: Change,
    content: Content,
    /// The topmost missing key the change would create (journaled as RegKeyCreated before it).
    creates: Option<RegPath>,
}

/// The before of the first open entry on the same target (the first original wins).
enum FirstBefore {
    Value(Option<RegData>),
    Tree(Option<RegSnapshot>),
    Xml(Option<String>),
}

fn same_text(a: &str, b: &str) -> bool {
    a == b || a.to_lowercase() == b.to_lowercase()
}

fn first_before(j: &Journal, c: &Change) -> Option<FirstBefore> {
    j.open_entries().into_iter().find_map(|e| match (&e.change, c) {
        (
            Change::RegValue { hive: h1, key: k1, name: n1, before, .. },
            Change::RegValue { hive: h2, key: k2, name: n2, .. },
        ) if h1 == h2
            && same_text(k1.as_str(), k2.as_str())
            && match (n1, n2) {
                (None, None) => true,
                (Some(a), Some(b)) => same_text(a, b),
                _ => false,
            } =>
        {
            Some(FirstBefore::Value(before.clone()))
        }
        (Change::RegTree { hive: h1, key: k1, before, .. }, Change::RegTree { hive: h2, key: k2, .. })
            if h1 == h2 && same_text(k1.as_str(), k2.as_str()) =>
        {
            Some(FirstBefore::Tree(before.clone()))
        }
        (Change::Task { name: n1, before_xml, .. }, Change::Task { name: n2, .. })
            if same_text(n1.as_str(), n2.as_str()) =>
        {
            Some(FirstBefore::Xml(before_xml.clone()))
        }
        _ => None,
    })
}

/// The topmost missing key on the way to `key` (the key itself included when `with_key`).
fn missing_top(os: &dyn JournalOs, hive: Hive, key: &RegPath, with_key: bool) -> Result<Option<RegPath>, Refusal> {
    let segs: Vec<&str> = key.as_str().split('\\').collect();
    let mut n = if with_key { segs.len() } else { segs.len().saturating_sub(1) };
    let mut top = None;
    while n > 0 {
        let prefix = segs.get(..n).unwrap_or_default().join("\\");
        let p = RegPath::new(&prefix).map_err(|_| Refusal::Invalid("journal: a bad key path"))?;
        if os.reg_key_exists(hive, &p).map_err(Refusal::Os)? {
            break;
        }
        top = Some(p);
        n -= 1;
    }
    Ok(top)
}

/// The entry a change records and what it writes; None when there is nothing to record or change.
fn plan_one(j: &Journal, os: &dyn JournalOs, c: &Change) -> Result<Option<Planned>, Refusal> {
    if !hive_allowed(j.scope, c) {
        return Err(Refusal::Os(os_err(ERROR_ACCESS_DENIED, "journal: the change is outside the journal's scope")));
    }
    match c {
        Change::RegValue { hive, key, name, after, .. } => {
            let before = match first_before(j, c) {
                Some(FirstBefore::Value(b)) => b,
                _ => os.reg_get(*hive, key, name.as_deref()).map_err(Refusal::Os)?,
            };
            let creates = if after.is_some() { missing_top(os, *hive, key, true)? } else { None };
            let change =
                Change::RegValue { hive: *hive, key: key.clone(), name: name.clone(), before, after: after.clone() };
            Ok(Some(Planned { change, content: Content::Nothing, creates }))
        }
        Change::RegKeyCreated { hive, key } => {
            // A key that exists is not ours: its undo would delete it with a subtree the suite never made.
            if os.reg_key_exists(*hive, key).map_err(Refusal::Os)? {
                return Ok(None);
            }
            let creates = missing_top(os, *hive, key, false)?;
            Ok(Some(Planned { change: c.clone(), content: Content::Nothing, creates }))
        }
        Change::RegTree { hive, key, before: content, after } => {
            let tree = content.as_ref().ok_or(Refusal::Invalid("journal: a planned tree carries no tree to write"))?;
            check_tree(tree, 1).map_err(Refusal::from_line)?;
            if canonical_tree(tree).len() > SNAPSHOT_MAX {
                return Err(Refusal::TooLarge);
            }
            if !canonical_order(tree) {
                return Err(Refusal::Invalid("journal: a planned tree is not in canonical order"));
            }
            if carries_mark(tree) {
                return Err(Refusal::Invalid("journal: a planned tree carries the journal's write mark"));
            }
            if tree.subkeys.iter().any(|(n, _)| child_path(key, n).is_err()) {
                return Err(Refusal::Invalid("journal: a planned tree's subkey path is too long"));
            }
            if digest_tree(tree, os).map_err(Refusal::Os)? != *after {
                return Err(Refusal::Invalid("journal: a planned tree does not match its after digest"));
            }
            let before = match first_before(j, c) {
                Some(FirstBefore::Tree(b)) => b,
                // A mark with no open entry left (the journal was lost): what is there is the original, without it.
                _ => os.reg_tree_get(*hive, key).map_err(Refusal::Os)?.map(unmarked),
            };
            let creates = missing_top(os, *hive, key, false)?;
            let change = Change::RegTree { hive: *hive, key: key.clone(), before, after: *after };
            Ok(Some(Planned { change, content: Content::Tree(tree.clone()), creates }))
        }
        Change::Task { name, before_xml: content, after } => {
            let xml = content.as_ref().ok_or(Refusal::Invalid("journal: a planned task carries no XML to register"))?;
            let mut encoded = String::new();
            put_str(&mut encoded, xml);
            if encoded.len() > TASK_XML_MAX {
                return Err(Refusal::TooLarge);
            }
            if digest_text(xml, os).map_err(Refusal::Os)? != *after {
                return Err(Refusal::Invalid("journal: a planned task does not match its after digest"));
            }
            let before_xml = match first_before(j, c) {
                Some(FirstBefore::Xml(b)) => b,
                _ => os.task_xml(name).map_err(Refusal::Os)?,
            };
            let change = Change::Task { name: name.clone(), before_xml, after: *after };
            Ok(Some(Planned { change, content: Content::Xml(xml.clone()), creates: None }))
        }
        _ => Err(Refusal::Invalid("journal: apply makes registry and task changes only (record the others)")),
    }
}

fn perform(os: &mut dyn JournalOs, p: &Planned) -> Result<(), OsErr> {
    match (&p.change, &p.content) {
        (Change::RegValue { hive, key, name, after, .. }, _) => os.reg_set(*hive, key, name.as_deref(), after.as_ref()),
        (Change::RegKeyCreated { hive, key }, _) => os.reg_key_create(*hive, key),
        (Change::RegTree { hive, key, .. }, Content::Tree(t)) => put_tree_marked(os, *hive, key, Some(t)),
        (Change::Task { name, .. }, Content::Xml(x)) => os.task_put(name, Some(x)),
        _ => Err(os_err(ERROR_INVALID_PARAMETER, "journal: nothing to apply")),
    }
}

/// The value a tree write keeps on its key from its first registry call to its last (put_tree_marked). A key that
/// carries it holds a write of ours that a crash or an error cut part-way (neither the before nor the after), so its
/// undo restores the before instead of leaving the key as another program's change. No planned tree may carry it.
const WRITING_MARK: &str = "mini-oled-dock.journal.writing";

fn is_mark(name: Option<&str>) -> bool {
    name.is_some_and(|n| n.eq_ignore_ascii_case(WRITING_MARK))
}

fn carries_mark(s: &RegSnapshot) -> bool {
    s.values.iter().any(|(n, _)| is_mark(n.as_deref()))
}

fn unmarked(mut s: RegSnapshot) -> RegSnapshot {
    s.values.retain(|(n, _)| !is_mark(n.as_deref()));
    s
}

fn child_path(key: &RegPath, name: &str) -> Result<RegPath, OsErr> {
    RegPath::new(&format!("{}\\{name}", key.as_str()))
        .map_err(|_| os_err(ERROR_INVALID_PARAMETER, "journal: a subkey path is too long"))
}

/// Makes the tree at `key` equal `target` (None removes the key) through single registry calls, which a crash or an
/// error can cut anywhere: the key carries WRITING_MARK from the first call to the last, so a cut write is known as
/// ours. Its subkeys go first (each deepest first), then the values the target lacks; the target's values and subkeys
/// are written, and the mark goes last. Removing a key goes deepest first and the key last, the mark with it.
fn put_tree_marked(
    os: &mut dyn JournalOs,
    hive: Hive,
    key: &RegPath,
    target: Option<&RegSnapshot>,
) -> Result<(), OsErr> {
    let current = os.reg_tree_get(hive, key)?;
    if current.is_none() && target.is_none() {
        return Ok(());
    }
    os.reg_set(hive, key, Some(WRITING_MARK), Some(&RegData::Dword(1)))?;
    let Some(t) = target else {
        return os.reg_tree_put(hive, key, None);
    };
    if let Some(c) = &current {
        for (name, _) in &c.subkeys {
            os.reg_tree_put(hive, &child_path(key, name)?, None)?;
        }
        for (name, _) in &c.values {
            if !is_mark(name.as_deref()) && !t.values.iter().any(|(n, _)| n == name) {
                os.reg_set(hive, key, name.as_deref(), None)?;
            }
        }
    }
    for (name, data) in &t.values {
        os.reg_set(hive, key, name.as_deref(), Some(data))?;
    }
    for (name, sub) in &t.subkeys {
        os.reg_tree_put(hive, &child_path(key, name)?, Some(sub))?;
    }
    os.reg_set(hive, key, Some(WRITING_MARK), None)
}

/// Undoes what a failed run made; the run is marked failed only when nothing of it is left to undo.
fn rollback(j: &mut Journal, sink: &mut dyn Sink, os: &mut dyn JournalOs, run: RunId) -> UndoReport {
    let report = undo(j, sink, os, &UndoSet::Run(run));
    if report.failed.is_empty() {
        let _ = append(j, sink, Record::RunFailed { run });
    }
    report
}

// ------------------------------------------------------------------------------------------------ undo

enum Undone {
    Restored,
    Already,
    ChangedByOther,
    NotOurs,
}

fn selected(set: &UndoSet, e: &Entry) -> bool {
    match set {
        UndoSet::Classes(classes) => class_of(&e.kind).is_some_and(|c| classes.contains(&c)),
        UndoSet::Run(run) => e.run == *run,
        UndoSet::Seqs(seqs) => seqs.contains(&e.seq),
    }
}

fn same_tree(a: Option<&RegSnapshot>, b: Option<&RegSnapshot>) -> bool {
    match (a, b) {
        (None, None) => true,
        (Some(a), Some(b)) => canonical_tree(a) == canonical_tree(b),
        _ => false,
    }
}

fn undo_change(os: &mut dyn JournalOs, c: &Change) -> Result<Undone, OsErr> {
    Ok(match c {
        Change::RegValue { hive, key, name, before, after } => {
            let current = os.reg_get(*hive, key, name.as_deref())?;
            if current == *after {
                os.reg_set(*hive, key, name.as_deref(), before.as_ref())?;
                Undone::Restored
            } else if current == *before {
                Undone::Already
            } else {
                Undone::ChangedByOther
            }
        }
        Change::RegKeyCreated { hive, key } => {
            if os.reg_key_exists(*hive, key)? {
                os.reg_tree_put(*hive, key, None)?;
                Undone::Restored
            } else {
                Undone::Already
            }
        }
        Change::RegTree { hive, key, before, after } => {
            let current = os.reg_tree_get(*hive, key)?;
            let ours = match &current {
                // A write of ours a crash or an error cut part-way (put_tree_marked): ours, whatever else it holds.
                Some(t) if carries_mark(t) => true,
                Some(t) => digest_tree(t, &*os)? == *after,
                None => false,
            };
            if ours {
                put_tree_marked(os, *hive, key, before.as_ref())?;
                Undone::Restored
            } else if same_tree(current.as_ref(), before.as_ref()) {
                Undone::Already
            } else {
                Undone::ChangedByOther
            }
        }
        Change::Task { name, before_xml, after } => {
            let current = os.task_xml(name)?;
            let ours = match &current {
                Some(x) => digest_text(x, &*os)? == *after,
                None => false,
            };
            if ours {
                os.task_put(name, before_xml.as_deref())?;
                Undone::Restored
            } else if current == *before_xml {
                Undone::Already
            } else {
                Undone::ChangedByOther
            }
        }
        Change::FileCreated { path, after } => match os.file_digest(path)? {
            Some(d) if d == *after => {
                os.file_delete(path)?;
                Undone::Restored
            }
            None => Undone::Already,
            Some(_) => Undone::ChangedByOther,
        },
        Change::FileSwap { dest, prev, staged_sha256 } => {
            let current = os.file_digest(dest)?;
            if current == Some(*staged_sha256) {
                match prev {
                    Some(p) => os.file_restore(dest, p)?,
                    None => os.file_delete(dest)?,
                }
                Undone::Restored
            } else {
                let prev_digest = match prev {
                    Some(p) => os.file_digest(p)?,
                    None => None,
                };
                if current == prev_digest {
                    Undone::Already
                } else {
                    Undone::ChangedByOther
                }
            }
        }
        Change::ServiceStartType { .. } | Change::Unknown(_) => Undone::NotOurs,
    })
}

/// What a change touches, as the reports and the lists name it; None for an Unknown change.
fn target_of(c: &Change) -> Option<Target> {
    Some(match c {
        Change::RegValue { hive, key, name, .. } => {
            Target::Reg { hive: *hive, key: key.as_str().to_string(), name: name.clone() }
        }
        Change::RegKeyCreated { hive, key } | Change::RegTree { hive, key, .. } => {
            Target::Reg { hive: *hive, key: key.as_str().to_string(), name: None }
        }
        Change::Task { name, .. } => Target::Task(name.as_str().to_string()),
        Change::FileCreated { path, .. } => Target::File(path.clone()),
        Change::FileSwap { dest, .. } => Target::File(dest.clone()),
        Change::ServiceStartType { name, .. } => Target::Reg {
            hive: Hive::Hklm,
            key: format!("SYSTEM\\CurrentControlSet\\Services\\{name}"),
            name: Some("Start".to_string()),
        },
        Change::Unknown(_) => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::{Cell, RefCell};
    use std::collections::BTreeMap;
    use std::rc::Rc;

    // ---------------------------------------------------------------------------------------- the fake OS and disk

    /// One process's luck. Every mutating OS call and every append is one op; with a limit, the op that reaches it
    /// "crashes" the process: it fails (an append leaves half its line, as a torn write does) and so does every later
    /// op, until `revive` (the next process).
    struct Crash {
        limit: Cell<Option<usize>>,
        done: Cell<usize>,
        dead: Cell<bool>,
    }

    enum Step {
        Go,
        Killed,
        Dead,
    }

    impl Crash {
        fn new(limit: Option<usize>) -> Rc<Crash> {
            Rc::new(Crash { limit: Cell::new(limit), done: Cell::new(0), dead: Cell::new(false) })
        }

        fn step(&self) -> Step {
            if self.dead.get() {
                return Step::Dead;
            }
            if self.limit.get().is_some_and(|l| self.done.get() >= l) {
                self.dead.set(true);
                return Step::Killed;
            }
            self.done.set(self.done.get() + 1);
            Step::Go
        }

        fn revive(&self) {
            self.dead.set(false);
            self.limit.set(None);
        }
    }

    fn crashed() -> OsErr {
        OsErr { code: 1, what: "crashed".into() }
    }

    fn hk(h: Hive) -> u8 {
        match h {
            Hive::Hkcu => 0,
            Hive::Hklm => 1,
        }
    }

    fn under(key: &str, other: &str) -> bool {
        other == key || other.strip_prefix(key).is_some_and(|r| r.starts_with('\\'))
    }

    /// The registry (keys and values), the tasks and the files of the fake OS.
    #[derive(Clone, Debug, Default, PartialEq, Eq)]
    struct World {
        keys: BTreeSet<(u8, String)>,
        values: BTreeMap<(u8, String, Option<String>), RegData>,
        tasks: BTreeMap<String, String>,
        files: BTreeMap<String, Vec<u8>>,
    }

    impl World {
        /// RegCreateKeyExW's rule: the missing keys on the way are created too.
        fn create_key(&mut self, h: u8, k: &str) {
            let segs: Vec<&str> = k.split('\\').collect();
            for n in 1..=segs.len() {
                self.keys.insert((h, segs[..n].join("\\")));
            }
        }

        fn remove_tree(&mut self, h: u8, k: &str) {
            self.keys.retain(|(kh, kk)| !(*kh == h && under(k, kk)));
            self.values.retain(|(vh, vk, _), _| !(*vh == h && under(k, vk)));
        }

        fn tree(&self, h: u8, k: &str) -> RegSnapshot {
            let values = self
                .values
                .iter()
                .filter(|((vh, vk, _), _)| *vh == h && vk == k)
                .map(|((_, _, n), d)| (n.clone(), d.clone()))
                .collect();
            let prefix = format!("{k}\\");
            let subkeys = self
                .keys
                .iter()
                .filter(|(kh, _)| *kh == h)
                .filter_map(|(_, kk)| kk.strip_prefix(&prefix).filter(|rest| !rest.contains('\\')))
                .map(|n| (n.to_string(), self.tree(h, &format!("{k}\\{n}"))))
                .collect();
            RegSnapshot { values, subkeys }
        }

        fn put_tree(&mut self, h: u8, k: &str, s: &RegSnapshot) {
            self.create_key(h, k);
            for (n, d) in &s.values {
                self.values.insert((h, k.to_string(), n.clone()), d.clone());
            }
            for (n, t) in &s.subkeys {
                self.put_tree(h, &format!("{k}\\{n}"), t);
            }
        }

        fn set(&mut self, h: Hive, k: &str, n: Option<&str>, d: RegData) {
            self.create_key(hk(h), k);
            self.values.insert((hk(h), k.to_string(), n.map(str::to_string)), d);
        }
    }

    /// The test hash: four FNV-1a 64 lanes with different offsets (not cryptographic).
    fn fake_digest(bytes: &[u8]) -> Digest {
        let mut out = [0u8; 32];
        for (lane, chunk) in out.chunks_exact_mut(8).enumerate() {
            let mut h = 0xcbf2_9ce4_8422_2325u64 ^ (lane as u64 + 1).wrapping_mul(0x9e37_79b9_7f4a_7c15);
            for &b in bytes {
                h ^= u64::from(b);
                h = h.wrapping_mul(0x0000_0100_0000_01b3);
            }
            chunk.copy_from_slice(&h.to_le_bytes());
        }
        out
    }

    struct FakeOs {
        w: World,
        crash: Rc<Crash>,
        /// Mutating calls on keys under this one fail with ACCESS_DENIED (a permission error, not a crash).
        deny: Option<String>,
        /// reg_tree_put as RealOs makes it: every key deleted (deepest first), every key created and every value written
        /// is an op of its own, so a crash or a refusal can cut the tree between them. Off: one op, all or nothing.
        granular: bool,
    }

    impl FakeOs {
        fn new(w: World) -> FakeOs {
            FakeOs { w, crash: Crash::new(None), deny: None, granular: false }
        }

        fn with(w: &World, crash: &Rc<Crash>) -> FakeOs {
            FakeOs { w: w.clone(), crash: crash.clone(), deny: None, granular: false }
        }

        /// RealOs's write_tree: the key, each of its values, then each subkey the same way, one op each.
        fn write_tree(&mut self, h: u8, k: &str, s: &RegSnapshot) -> Result<(), OsErr> {
            self.mutate(Some(k))?;
            self.w.create_key(h, k);
            for (n, d) in &s.values {
                self.mutate(Some(k))?;
                self.w.values.insert((h, k.to_string(), n.clone()), d.clone());
            }
            for (n, t) in &s.subkeys {
                self.write_tree(h, &format!("{k}\\{n}"), t)?;
            }
            Ok(())
        }

        fn mutate(&self, key: Option<&str>) -> Result<(), OsErr> {
            if key.is_some_and(|k| self.deny.as_deref().is_some_and(|d| under(d, k))) {
                return Err(OsErr { code: 5, what: "denied".into() });
            }
            match self.crash.step() {
                Step::Go => Ok(()),
                Step::Killed | Step::Dead => Err(crashed()),
            }
        }
    }

    impl JournalOs for FakeOs {
        fn reg_get(&self, hive: Hive, key: &RegPath, name: Option<&str>) -> Result<Option<RegData>, OsErr> {
            Ok(self.w.values.get(&(hk(hive), key.as_str().to_string(), name.map(str::to_string))).cloned())
        }
        fn reg_set(
            &mut self,
            hive: Hive,
            key: &RegPath,
            name: Option<&str>,
            data: Option<&RegData>,
        ) -> Result<(), OsErr> {
            self.mutate(Some(key.as_str()))?;
            match data {
                Some(d) => self.w.set(hive, key.as_str(), name, d.clone()),
                None => {
                    self.w.values.remove(&(hk(hive), key.as_str().to_string(), name.map(str::to_string)));
                }
            }
            Ok(())
        }
        fn reg_key_exists(&self, hive: Hive, key: &RegPath) -> Result<bool, OsErr> {
            Ok(self.w.keys.contains(&(hk(hive), key.as_str().to_string())))
        }
        fn reg_key_create(&mut self, hive: Hive, key: &RegPath) -> Result<(), OsErr> {
            self.mutate(Some(key.as_str()))?;
            self.w.create_key(hk(hive), key.as_str());
            Ok(())
        }
        fn reg_key_delete_if_empty(&mut self, hive: Hive, key: &RegPath) -> Result<bool, OsErr> {
            self.mutate(Some(key.as_str()))?;
            let t = self.w.tree(hk(hive), key.as_str());
            let exists = self.w.keys.contains(&(hk(hive), key.as_str().to_string()));
            if exists && t.values.is_empty() && t.subkeys.is_empty() {
                self.w.remove_tree(hk(hive), key.as_str());
                return Ok(true);
            }
            Ok(false)
        }
        fn reg_tree_get(&self, hive: Hive, key: &RegPath) -> Result<Option<RegSnapshot>, OsErr> {
            let exists = self.w.keys.contains(&(hk(hive), key.as_str().to_string()));
            Ok(exists.then(|| self.w.tree(hk(hive), key.as_str())))
        }
        fn reg_tree_put(&mut self, hive: Hive, key: &RegPath, snap: Option<&RegSnapshot>) -> Result<(), OsErr> {
            let h = hk(hive);
            if !self.granular {
                self.mutate(Some(key.as_str()))?;
                self.w.remove_tree(h, key.as_str());
                if let Some(s) = snap {
                    self.w.put_tree(h, key.as_str(), s);
                }
                return Ok(());
            }
            let mut gone: Vec<String> = self
                .w
                .keys
                .iter()
                .filter(|(kh, kk)| *kh == h && under(key.as_str(), kk))
                .map(|(_, kk)| kk.clone())
                .collect();
            gone.sort_by_key(|k| std::cmp::Reverse(k.matches('\\').count()));
            for k in gone {
                self.mutate(Some(&k))?;
                self.w.remove_tree(h, &k);
            }
            match snap {
                Some(s) => self.write_tree(h, key.as_str(), s),
                None => Ok(()),
            }
        }
        fn task_xml(&self, name: &TaskName) -> Result<Option<String>, OsErr> {
            Ok(self.w.tasks.get(name.as_str()).cloned())
        }
        fn task_put(&mut self, name: &TaskName, xml: Option<&str>) -> Result<(), OsErr> {
            self.mutate(None)?;
            match xml {
                Some(x) => self.w.tasks.insert(name.as_str().to_string(), x.to_string()),
                None => self.w.tasks.remove(name.as_str()),
            };
            Ok(())
        }
        fn file_digest(&self, path: &str) -> Result<Option<Digest>, OsErr> {
            Ok(self.w.files.get(path).map(|b| fake_digest(b)))
        }
        fn file_delete(&mut self, path: &str) -> Result<(), OsErr> {
            self.mutate(None)?;
            self.w.files.remove(path);
            Ok(())
        }
        fn file_restore(&mut self, dest: &str, from: &str) -> Result<(), OsErr> {
            self.mutate(None)?;
            let bytes = self.w.files.get(from).cloned().ok_or(OsErr { code: 2, what: "no prev".into() })?;
            self.w.files.insert(dest.to_string(), bytes);
            Ok(())
        }
        fn digest(&self, bytes: &[u8]) -> Result<Digest, OsErr> {
            Ok(fake_digest(bytes))
        }
    }

    struct FakeSink {
        disk: Rc<RefCell<Vec<u8>>>,
        crash: Rc<Crash>,
    }

    impl Sink for FakeSink {
        fn append(&mut self, bytes: &[u8]) -> Result<(), OsErr> {
            match self.crash.step() {
                Step::Go => {
                    self.disk.borrow_mut().extend_from_slice(bytes);
                    Ok(())
                }
                Step::Killed => {
                    self.disk.borrow_mut().extend_from_slice(&bytes[..bytes.len() / 2]);
                    Err(crashed())
                }
                Step::Dead => Err(crashed()),
            }
        }
    }

    fn new_disk(crash: &Rc<Crash>) -> (Rc<RefCell<Vec<u8>>>, FakeSink) {
        let disk = Rc::new(RefCell::new(Vec::new()));
        (disk.clone(), FakeSink { disk, crash: crash.clone() })
    }

    /// What the next process sees: the torn tail cut (journal_os does that under the lock), the rest parsed.
    fn reopen(disk: &Rc<RefCell<Vec<u8>>>, scope: Scope, crash: &Rc<Crash>) -> (Journal, FakeSink) {
        let mut d = disk.borrow_mut();
        let cut = d.iter().rposition(|&b| b == b'\n').map_or(0, |p| p + 1);
        d.truncate(cut);
        let j = Journal::parse(scope, &d).expect("a journal written by apply and undo parses");
        drop(d);
        (j, FakeSink { disk: disk.clone(), crash: crash.clone() })
    }

    fn kp(s: &str) -> RegPath {
        RegPath::new(s).expect("a valid test key")
    }

    fn tn(s: &str) -> TaskName {
        TaskName::new(s).expect("a valid test task name")
    }

    fn sz(s: &str) -> RegData {
        RegData::Sz(s.to_string())
    }

    fn snap(values: Vec<(Option<&str>, RegData)>, subkeys: Vec<(&str, RegSnapshot)>) -> RegSnapshot {
        RegSnapshot {
            values: values.into_iter().map(|(n, d)| (n.map(str::to_string), d)).collect(),
            subkeys: subkeys.into_iter().map(|(n, t)| (n.to_string(), t)).collect(),
        }
    }

    fn value(key: &str, name: Option<&str>, after: Option<RegData>) -> Change {
        Change::RegValue { hive: Hive::Hkcu, key: kp(key), name: name.map(str::to_string), before: None, after }
    }

    /// A planned tree: the tree to write rides in the before slot, its digest in after.
    fn tree_change(key: &str, t: RegSnapshot) -> Change {
        let after = fake_digest(&canonical_tree(&t));
        Change::RegTree { hive: Hive::Hkcu, key: kp(key), before: Some(t), after }
    }

    fn task_change(name: &str, xml: &str) -> Change {
        Change::Task { name: tn(name), before_xml: Some(xml.to_string()), after: fake_digest(xml.as_bytes()) }
    }

    const TAKEOVER: Kind = Kind::Takeover(TakeoverId::FolderVerbs);
    const ALL: [KindClass; 4] = [KindClass::Takeover, KindClass::Install, KindClass::Update, KindClass::UserEdit];

    fn three_levels(tag: &str) -> RegSnapshot {
        snap(
            vec![(None, sz(tag)), (Some("v"), RegData::Dword(7))],
            vec![(
                "one",
                snap(
                    vec![(Some("w"), RegData::MultiSz(vec![tag.into(), "x".into()]))],
                    vec![("two", snap(vec![(Some("z"), RegData::Binary(tag.as_bytes().to_vec()))], vec![]))],
                ),
            )],
        )
    }

    /// The world the crash tests start from, and a plan with every kind apply makes.
    fn start_world() -> World {
        let mut w = World::default();
        w.set(Hive::Hkcu, "Software\\A", Some("a"), sz("orig"));
        w.set(Hive::Hkcu, "Software\\A", Some("b"), RegData::Dword(2));
        w.put_tree(0, "Software\\T", &three_levels("old"));
        w.tasks.insert("\\T1".into(), "<one/>".into());
        w
    }

    fn full_plan() -> Plan {
        Plan {
            kind: TAKEOVER,
            changes: vec![
                value("Software\\A", Some("a"), Some(sz("one"))),
                value("Software\\A", Some("b"), None),
                value("Software\\A", Some("c"), Some(RegData::Qword(3))),
                tree_change("Software\\T", three_levels("new")),
                Change::RegKeyCreated { hive: Hive::Hkcu, key: kp("Software\\N\\M") },
                value("Software\\N\\M", Some("x"), Some(RegData::Binary(vec![1, 2, 3]))),
                task_change("\\T1", "<two/>"),
                task_change("\\T2", "<new/>"),
                value("Software\\Q\\R", None, Some(RegData::ExpandSz("%x%".into()))),
                value("Software\\A", Some("a"), Some(sz("uno"))),
            ],
        }
    }

    /// The number of ops (mutations and appends) a whole run of `plan` takes from `w`.
    fn ops_of(w: &World, plan: &Plan, granular: bool) -> usize {
        let crash = Crash::new(None);
        let mut os = FakeOs::with(w, &crash);
        os.granular = granular;
        let (_, mut sink) = new_disk(&crash);
        let mut j = Journal::empty(Scope::User);
        let plan = Plan { kind: plan.kind.clone(), changes: plan.changes.clone() };
        assert!(apply(&mut j, &mut sink, &mut os, plan, 1).is_ok());
        crash.done.get()
    }

    // ---------------------------------------------------------------------------------------- the line

    fn frozen_records() -> Vec<Record> {
        let entry = |seq: u64, run: u64, ts: u64, kind: Kind, change: Change| {
            Record::Entry(Entry { seq, run: RunId(run), ts, kind, change })
        };
        let rv = |hive, key: &str, name: Option<&str>, before, after| Change::RegValue {
            hive,
            key: kp(key),
            name: name.map(str::to_string),
            before,
            after,
        };
        let mut d = [0u8; 32];
        for (i, b) in d.iter_mut().enumerate() {
            *b = i as u8;
        }
        vec![
            entry(
                1,
                1,
                1_700_000_000,
                TAKEOVER,
                rv(
                    Hive::Hkcu,
                    "Software\\Classes\\Directory\\shell\\x",
                    Some("MUIVerb"),
                    Some(sz("a\"b\\c")),
                    Some(RegData::ExpandSz("%SystemRoot%\\x".into())),
                ),
            ),
            entry(
                2,
                1,
                1_700_000_001,
                Kind::Install,
                rv(Hive::Hklm, "SOFTWARE\\v", None, Some(RegData::MultiSz(vec!["a".into(), "\u{e8}\n".into()])), None),
            ),
            entry(
                3,
                2,
                5,
                Kind::Update,
                rv(
                    Hive::Hkcu,
                    "Software\\v",
                    Some("d"),
                    Some(RegData::Dword(u32::MAX)),
                    Some(RegData::Qword(u64::MAX)),
                ),
            ),
            entry(
                4,
                2,
                6,
                Kind::UserEdit(AppId::Dock),
                rv(
                    Hive::Hkcu,
                    "Software\\v",
                    Some("b"),
                    Some(RegData::Binary(vec![0, 15, 16, 255])),
                    Some(RegData::Other { ty: 8, bytes: vec![1, 2] }),
                ),
            ),
            entry(
                5,
                3,
                7,
                Kind::UserEdit(AppId::Tasks),
                Change::RegKeyCreated {
                    hive: Hive::Hklm,
                    key: kp("SOFTWARE\\Microsoft\\Windows\\CurrentVersion\\Uninstall\\mini-oled-dock"),
                },
            ),
            entry(
                6,
                3,
                8,
                Kind::UserEdit(AppId::Files),
                Change::RegTree {
                    hive: Hive::Hkcu,
                    key: kp("Software\\t"),
                    before: Some(snap(
                        vec![(None, sz("d")), (Some("v"), RegData::Dword(1))],
                        vec![("s", snap(vec![], vec![]))],
                    )),
                    after: d,
                },
            ),
            entry(
                7,
                4,
                9,
                Kind::Unknown("future.kind".into()),
                Change::Task { name: tn("\\Dock"), before_xml: Some("<Task>\r\n</Task>".into()), after: [0xab; 32] },
            ),
            entry(8, 4, 10, Kind::Install, Change::Task { name: tn("\\Other task"), before_xml: None, after: [0; 32] }),
            entry(
                9,
                5,
                11,
                Kind::Install,
                Change::FileCreated { path: "C:\\Suite\\Start\\Tasks.lnk".into(), after: [0x11; 32] },
            ),
            entry(
                10,
                6,
                12,
                Kind::Update,
                Change::FileSwap {
                    dest: "C:\\Suite\\tasks.exe".into(),
                    prev: Some("C:\\Suite\\tasks.prev.exe".into()),
                    staged_sha256: [0x22; 32],
                },
            ),
            entry(
                11,
                6,
                13,
                Kind::Update,
                Change::FileSwap { dest: "C:\\Suite\\files.exe".into(), prev: None, staged_sha256: [0x33; 32] },
            ),
            entry(
                12,
                7,
                14,
                Kind::UserEdit(AppId::Tasks),
                Change::ServiceStartType { name: "svc".into(), before: 2, after: 4 },
            ),
            entry(13, 7, 15, Kind::Install, Change::Unknown(r#"{"t":"future_change","x":[1,{"y":null}]}"#.into())),
            Record::Undone { seq: 3 },
            Record::RunFailed { run: RunId(2) },
            Record::Committed { run: RunId(6) },
        ]
    }

    /// F-P5's lines, crc included, computed apart from this code.
    const FROZEN_LINES: [(&str, &str); 16] = [
        (
            "476e5d6a",
            r#"{"r":"entry","v":1,"seq":1,"run":1,"ts":1700000000,"kind":"takeover.folder_verbs","change":{"t":"reg_value","hive":"hkcu","key":"Software\\Classes\\Directory\\shell\\x","name":"MUIVerb","before":{"t":"sz","s":"a\"b\\c"},"after":{"t":"expand_sz","s":"%SystemRoot%\\x"}}}"#,
        ),
        (
            "586718a0",
            r#"{"r":"entry","v":1,"seq":2,"run":1,"ts":1700000001,"kind":"install","change":{"t":"reg_value","hive":"hklm","key":"SOFTWARE\\v","name":null,"before":{"t":"multi_sz","l":["a","è\n"]},"after":null}}"#,
        ),
        (
            "3d1778c2",
            r#"{"r":"entry","v":1,"seq":3,"run":2,"ts":5,"kind":"update","change":{"t":"reg_value","hive":"hkcu","key":"Software\\v","name":"d","before":{"t":"dword","n":4294967295},"after":{"t":"qword","n":18446744073709551615}}}"#,
        ),
        (
            "4dac2b4c",
            r#"{"r":"entry","v":1,"seq":4,"run":2,"ts":6,"kind":"user_edit.dock","change":{"t":"reg_value","hive":"hkcu","key":"Software\\v","name":"b","before":{"t":"binary","hex":"000f10ff"},"after":{"t":"other","ty":8,"hex":"0102"}}}"#,
        ),
        (
            "a56b7227",
            r#"{"r":"entry","v":1,"seq":5,"run":3,"ts":7,"kind":"user_edit.tasks","change":{"t":"reg_key_created","hive":"hklm","key":"SOFTWARE\\Microsoft\\Windows\\CurrentVersion\\Uninstall\\mini-oled-dock"}}"#,
        ),
        (
            "77c35f8a",
            r#"{"r":"entry","v":1,"seq":6,"run":3,"ts":8,"kind":"user_edit.files","change":{"t":"reg_tree","hive":"hkcu","key":"Software\\t","before":{"values":[{"name":null,"data":{"t":"sz","s":"d"}},{"name":"v","data":{"t":"dword","n":1}}],"subkeys":[{"name":"s","tree":{"values":[],"subkeys":[]}}]},"after":"000102030405060708090a0b0c0d0e0f101112131415161718191a1b1c1d1e1f"}}"#,
        ),
        (
            "bca30308",
            r#"{"r":"entry","v":1,"seq":7,"run":4,"ts":9,"kind":"future.kind","change":{"t":"task","name":"\\Dock","before_xml":"<Task>\r\n</Task>","after":"abababababababababababababababababababababababababababababababab"}}"#,
        ),
        (
            "4b011873",
            r#"{"r":"entry","v":1,"seq":8,"run":4,"ts":10,"kind":"install","change":{"t":"task","name":"\\Other task","before_xml":null,"after":"0000000000000000000000000000000000000000000000000000000000000000"}}"#,
        ),
        (
            "ef8c31a3",
            r#"{"r":"entry","v":1,"seq":9,"run":5,"ts":11,"kind":"install","change":{"t":"file_created","path":"C:\\Suite\\Start\\Tasks.lnk","after":"1111111111111111111111111111111111111111111111111111111111111111"}}"#,
        ),
        (
            "7a821cbf",
            r#"{"r":"entry","v":1,"seq":10,"run":6,"ts":12,"kind":"update","change":{"t":"file_swap","dest":"C:\\Suite\\tasks.exe","prev":"C:\\Suite\\tasks.prev.exe","staged":"2222222222222222222222222222222222222222222222222222222222222222"}}"#,
        ),
        (
            "1cb02da5",
            r#"{"r":"entry","v":1,"seq":11,"run":6,"ts":13,"kind":"update","change":{"t":"file_swap","dest":"C:\\Suite\\files.exe","prev":null,"staged":"3333333333333333333333333333333333333333333333333333333333333333"}}"#,
        ),
        (
            "870db1b8",
            r#"{"r":"entry","v":1,"seq":12,"run":7,"ts":14,"kind":"user_edit.tasks","change":{"t":"service_start_type","name":"svc","before":2,"after":4}}"#,
        ),
        (
            "340a544e",
            r#"{"r":"entry","v":1,"seq":13,"run":7,"ts":15,"kind":"install","change":{"t":"future_change","x":[1,{"y":null}]}}"#,
        ),
        ("06b6b734", r#"{"r":"undone","seq":3}"#),
        ("a9ee6fa3", r#"{"r":"run_failed","run":2}"#),
        ("77b7ec84", r#"{"r":"committed","run":6}"#),
    ];

    fn frozen_file() -> Vec<u8> {
        FROZEN_LINES.iter().flat_map(|(crc, json)| format!("{crc} {json}\n").into_bytes()).collect()
    }

    #[test]
    fn crc32_known_vector() {
        assert_eq!(crc32(b"123456789"), 0xcbf4_3926);
        assert_eq!(crc32(b""), 0);
    }

    #[test]
    fn line_format_is_frozen() {
        let records = frozen_records();
        assert_eq!(records.len(), FROZEN_LINES.len());
        for (r, (crc, json)) in records.iter().zip(FROZEN_LINES) {
            let line = Journal::encode(r).expect("a frozen record encodes");
            assert_eq!(String::from_utf8(line).expect("utf-8"), format!("{crc} {json}\n"));
        }
        let j = Journal::parse(Scope::Machine, &frozen_file()).expect("the frozen file parses");
        assert_eq!(j.records(), &records[..]);
        assert_eq!(
            (j.next_seq(), Journal::parse(Scope::Machine, &frozen_file()).map(|mut j| j.begin_run())),
            (14, Ok(RunId(8)))
        );
    }

    #[test]
    fn json_strings_are_escaped_as_serde_json_does() {
        let mut samples: Vec<String> = (0u32..0x80).filter_map(char::from_u32).map(String::from).collect();
        samples.extend(["\u{7f}\u{80}\u{e8}\u{2028}\u{1f600}", "a\"b\\c/d", ""].map(String::from));
        for s in samples {
            let mut ours = String::new();
            put_str(&mut ours, &s);
            assert_eq!(ours, serde_json::to_string(&s).expect("a string serializes"), "{s:?}");
        }
    }

    fn all_data() -> Vec<RegData> {
        vec![
            sz(""),
            sz("tab\tquote\"nul\u{0}ctl\u{1f}\u{e8}"),
            RegData::ExpandSz("%TEMP%\\x".into()),
            RegData::MultiSz(vec![]),
            RegData::MultiSz(vec!["a".into(), "".into(), "\u{1f600}".into()]),
            RegData::Dword(0),
            RegData::Dword(u32::MAX),
            RegData::Qword(u64::MAX),
            RegData::Binary(vec![]),
            RegData::Binary((0..=255).collect()),
            RegData::Other { ty: 0, bytes: vec![] },
            RegData::Other { ty: u32::MAX, bytes: vec![0xde, 0xad] },
        ]
    }

    #[test]
    fn encode_parse_round_trip() {
        let mut records = frozen_records();
        let mut seq = 100;
        for d in all_data() {
            seq += 1;
            let change = value("Software\\r", Some("n"), Some(d.clone()));
            records.push(Record::Entry(Entry { seq, run: RunId(9), ts: 0, kind: Kind::Update, change }));
            seq += 1;
            let tree = snap(vec![(None, d.clone()), (Some(""), d)], vec![("k", three_levels("t"))]);
            let change = Change::RegTree { hive: Hive::Hklm, key: kp("A\\B"), before: Some(tree), after: [7; 32] };
            records.push(Record::Entry(Entry {
                seq,
                run: RunId(9),
                ts: u64::MAX,
                kind: Kind::Unknown(String::new()),
                change,
            }));
        }
        records.push(Record::Entry(Entry {
            seq: u64::MAX,
            run: RunId(u64::MAX),
            ts: 0,
            kind: Kind::Unknown("k".repeat(KIND_MAX)),
            change: Change::Unknown("{ \"t\" : \"later\", \"n\": [ ] }".into()),
        }));
        let file: Vec<u8> = records.iter().flat_map(|r| Journal::encode(r).expect("encodes")).collect();
        let j = Journal::parse(Scope::User, &file).expect("parses");
        assert_eq!(j.records(), &records[..]);
        assert_eq!(j.next_seq(), u64::MAX, "the counters saturate");
    }

    #[test]
    fn binary_is_hex() {
        let bytes: Vec<u8> = (0..=255).collect();
        let mut out = String::new();
        put_data(&mut out, &RegData::Binary(bytes.clone()));
        let hex: String = bytes.iter().map(|b| format!("{b:02x}")).collect();
        assert_eq!(hex.len(), 512);
        assert_eq!(out, format!(r#"{{"t":"binary","hex":"{hex}"}}"#));
    }

    #[test]
    fn crc_mismatch_is_corrupt() {
        let file = frozen_file();
        let mut line_of = Vec::with_capacity(file.len());
        let mut line = 1u32;
        for &b in &file {
            line_of.push(line);
            if b == b'\n' {
                line += 1;
            }
        }
        // Every byte but the last '\n' (flipping it makes the last line a torn tail, which is ignored on purpose).
        for at in 0..file.len() - 1 {
            for bit in [at % 8, 7] {
                let mut bad = file.clone();
                bad[at] ^= 1 << bit;
                assert_eq!(
                    Journal::parse(Scope::Machine, &bad).err(),
                    Some(JournalError::Corrupt { line: line_of[at] }),
                    "byte {at} bit {bit}"
                );
            }
        }
    }

    #[test]
    fn torn_tail_is_ignored() {
        let file = frozen_file();
        let records = frozen_records();
        let last_start = file[..file.len() - 1].iter().rposition(|&b| b == b'\n').map_or(0, |p| p + 1);
        for cut in last_start..file.len() {
            let j = Journal::parse(Scope::Machine, &file[..cut]).expect("a torn tail is ignored");
            assert_eq!(j.records(), &records[..records.len() - 1], "cut at {cut}");
        }
        assert_eq!(Journal::parse(Scope::Machine, &file).map(|j| j.records().len()), Ok(records.len()));
    }

    #[test]
    fn parse_refuses_newer_versions_unknown_records_duplicates_and_floods() {
        let line = |json: &str| format!("{:08x} {json}\n", crc32(json.as_bytes())).into_bytes();
        let entry = |v: u32, seq: u32| {
            format!(
                r#"{{"r":"entry","v":{v},"seq":{seq},"run":1,"ts":0,"kind":"install","change":{{"t":"file_created","path":"p","after":"{}"}}}}"#,
                "0".repeat(64)
            )
        };
        assert_eq!(Journal::parse(Scope::User, &line(&entry(2, 1))).err(), Some(JournalError::Version(2)));
        assert_eq!(Journal::parse(Scope::User, &line(&entry(70_000, 1))).err(), Some(JournalError::Version(u16::MAX)));
        assert_eq!(Journal::parse(Scope::User, &line(&entry(0, 1))).err(), Some(JournalError::Corrupt { line: 1 }));
        let two = [line(&entry(1, 1)), line(&entry(1, 1))].concat();
        assert_eq!(Journal::parse(Scope::User, &two).err(), Some(JournalError::Corrupt { line: 2 }));
        assert_eq!(
            Journal::parse(Scope::User, &line(r#"{"r":"later","seq":1}"#)).err(),
            Some(JournalError::Corrupt { line: 1 })
        );
        // Unknown keys inside known objects are ignored; key order does not matter.
        let loose = r#"{"seq":4,"extra":[1],"r":"undone"}"#;
        assert_eq!(
            Journal::parse(Scope::User, &line(loose)).map(|j| j.records().to_vec()),
            Ok(vec![Record::Undone { seq: 4 }])
        );
        // Upper-case hex, a bad digest length, a NUL in a path, a bad key path: corrupt.
        for bad in [
            r#"{"r":"entry","v":1,"seq":1,"run":1,"ts":0,"kind":"install","change":{"t":"file_created","path":"p","after":"AB"}}"#,
            r#"{"r":"entry","v":1,"seq":1,"run":1,"ts":0,"kind":"install","change":{"t":"file_created","path":"a\u0000","after":"0000000000000000000000000000000000000000000000000000000000000000"}}"#,
            r#"{"r":"entry","v":1,"seq":1,"run":1,"ts":0,"kind":"install","change":{"t":"reg_key_created","hive":"hkcu","key":"\\lead"}}"#,
            r#"{"r":"entry","v":1,"seq":1,"run":1,"ts":0,"kind":"install","change":{"t":"reg_key_created","hive":"hkcr","key":"x"}}"#,
        ] {
            assert_eq!(Journal::parse(Scope::User, &line(bad)).err(), Some(JournalError::Corrupt { line: 1 }), "{bad}");
        }
        let flood: Vec<u8> = (0..=MAX_RECORDS).flat_map(|i| line(&format!(r#"{{"r":"undone","seq":{i}}}"#))).collect();
        assert_eq!(Journal::parse(Scope::User, &flood).err(), Some(JournalError::TooLarge));
        let long = format!(r#"{{"r":"undone","seq":1,"pad":"{}"}}"#, "x".repeat(MAX_LINE));
        assert_eq!(Journal::parse(Scope::User, &line(&long)).err(), Some(JournalError::Corrupt { line: 1 }));
    }

    #[test]
    fn names_and_paths_follow_their_rules() {
        assert!(RegPath::new("Software\\A b\\c").is_ok());
        let seg = "s".repeat(255);
        assert!(RegPath::new(&seg).is_ok());
        assert!(RegPath::new(&format!("{seg}\\{seg}")).is_ok());
        for bad in ["", "\\x", "x\\", "a\\\\b", "a\0b"] {
            assert_eq!(RegPath::new(bad), Err(JournalError::BadPath), "{bad:?}");
        }
        assert_eq!(RegPath::new(&"s".repeat(256)), Err(JournalError::BadPath));
        assert_eq!(RegPath::new(&[seg.as_str(); 3].join("\\")), Err(JournalError::BadPath), "over 512");
        assert_eq!(RegPath::new("a\\b").map(|p| p.as_str().to_string()), Ok("a\\b".to_string()));
        for ok in ["\\Dock", "\\a b.c_d-e", &format!("\\{}", "x".repeat(64))] {
            assert!(TaskName::new(ok).is_ok(), "{ok}");
        }
        for bad in ["Dock", "\\", "\\a\\b", "\\a:b", "\\\u{e8}", &format!("\\{}", "x".repeat(65))] {
            assert_eq!(TaskName::new(bad), Err(JournalError::BadName), "{bad}");
        }
        let e = |change| Record::Entry(Entry { seq: 1, run: RunId(1), ts: 0, kind: Kind::Install, change });
        assert_eq!(
            Journal::encode(&e(Change::FileCreated { path: String::new(), after: [0; 32] })),
            Err(JournalError::BadPath)
        );
        let deep = (0..SNAPSHOT_DEPTH_MAX).fold(snap(vec![], vec![]), |t, _| snap(vec![], vec![("k", t)]));
        let tree = |t| e(Change::RegTree { hive: Hive::Hkcu, key: kp("k"), before: Some(t), after: [0; 32] });
        assert_eq!(Journal::encode(&tree(deep.clone())), Err(JournalError::TooLarge), "17 levels");
        assert!(Journal::encode(&tree(deep.subkeys[0].1.clone())).is_ok(), "16 levels");
        assert_eq!(
            Journal::encode(&tree(snap(vec![], vec![("a\\b", snap(vec![], vec![]))]))),
            Err(JournalError::BadName)
        );
        let unknown = |k: &str| {
            Record::Entry(Entry {
                seq: 1,
                run: RunId(1),
                ts: 0,
                kind: Kind::Unknown(k.into()),
                change: Change::Unknown(r#"{"t":"x"}"#.into()),
            })
        };
        assert_eq!(Journal::encode(&unknown(&"k".repeat(65))), Err(JournalError::TooLarge));
        assert_eq!(Journal::encode(&unknown("install")), Err(JournalError::Corrupt { line: 0 }));
        for text in [r#"{"t":"task"}"#, "{\"t\":\"x\"\n}", r#"[1]"#, r#"{"u":"x"}"#, r#" {"t":"x"}"#] {
            let r = Record::Entry(Entry {
                seq: 1,
                run: RunId(1),
                ts: 0,
                kind: Kind::Install,
                change: Change::Unknown(text.into()),
            });
            assert_eq!(Journal::encode(&r), Err(JournalError::Corrupt { line: 0 }), "{text}");
        }
    }

    // ---------------------------------------------------------------------------------------- apply and undo

    #[test]
    fn a_maximal_tree_records() {
        let padded = |n: usize| snap(vec![(Some("v"), sz(&"x".repeat(n)))], vec![]);
        let base = canonical_tree(&padded(0)).len();
        let max = padded(SNAPSHOT_MAX - base);
        assert_eq!(canonical_tree(&max).len(), SNAPSHOT_MAX);
        for (tree, fits) in [(max.clone(), true), (padded(SNAPSHOT_MAX - base + 1), false)] {
            let mut w = World::default();
            w.put_tree(0, "Software\\Big", &tree);
            let crash = Crash::new(None);
            let mut os = FakeOs::with(&w, &crash);
            let (disk, mut sink) = new_disk(&crash);
            let mut j = Journal::empty(Scope::User);
            let plan = Plan { kind: TAKEOVER, changes: vec![tree_change("Software\\Big", three_levels("small"))] };
            let r = apply(&mut j, &mut sink, &mut os, plan, 1);
            if fits {
                assert!(r.is_ok(), "{r:?}");
                let len = disk.borrow().len();
                assert!(len > SNAPSHOT_MAX && len < MAX_LINE, "{len}");
            } else {
                assert_eq!(r, Err(ApplyError::TooLarge(0)));
                assert_eq!(crash.done.get(), 0, "nothing changed and nothing appended");
                assert!(disk.borrow().is_empty() && j.records().is_empty());
            }
        }
    }

    #[test]
    fn first_original_wins() {
        let mut w = World::default();
        w.set(Hive::Hkcu, "Software\\A", Some("a"), sz("orig"));
        let mut os = FakeOs::new(w.clone());
        let crash = os.crash.clone();
        let (_, mut sink) = new_disk(&crash);
        let mut j = Journal::empty(Scope::User);
        for v in ["one", "two"] {
            let plan = Plan { kind: TAKEOVER, changes: vec![value("Software\\A", Some("a"), Some(sz(v)))] };
            assert!(apply(&mut j, &mut sink, &mut os, plan, 1).is_ok());
        }
        let befores: Vec<_> = j
            .open_entries()
            .iter()
            .filter_map(|e| match &e.change {
                Change::RegValue { before, .. } => Some(before.clone()),
                _ => None,
            })
            .collect();
        assert_eq!(befores, vec![Some(sz("orig")), Some(sz("orig"))]);
        let report = undo(&mut j, &mut sink, &mut os, &UndoSet::Classes(vec![KindClass::Takeover]));
        assert_eq!((report.restored, report.already), (1, 1));
        assert_eq!(os.w, w);
    }

    #[test]
    fn ownership() {
        let mut w = World::default();
        w.set(Hive::Hkcu, "Software\\A", Some("a"), sz("orig"));
        let mut os = FakeOs::new(w);
        let crash = os.crash.clone();
        let (_, mut sink) = new_disk(&crash);
        let mut j = Journal::empty(Scope::User);
        let plan = Plan { kind: TAKEOVER, changes: vec![value("Software\\A", Some("a"), Some(sz("ours")))] };
        assert!(apply(&mut j, &mut sink, &mut os, plan, 1).is_ok());
        os.w.set(Hive::Hkcu, "Software\\A", Some("a"), sz("theirs"));
        let report = undo(&mut j, &mut sink, &mut os, &UndoSet::Classes(vec![KindClass::Takeover]));
        let target = Target::Reg { hive: Hive::Hkcu, key: "Software\\A".into(), name: Some("a".into()) };
        assert_eq!(report, UndoReport { changed_by_other: vec![target], ..UndoReport::default() });
        assert_eq!(os.reg_get(Hive::Hkcu, &kp("Software\\A"), Some("a")), Ok(Some(sz("theirs"))), "left alone");
        assert!(j.is_settled(), "no longer ours: closed");
    }

    #[test]
    fn reg_key_created_undo_deletes_the_subtree() {
        let mut w = World::default();
        w.create_key(0, "Software");
        let mut os = FakeOs::new(w.clone());
        let crash = os.crash.clone();
        let (_, mut sink) = new_disk(&crash);
        let mut j = Journal::empty(Scope::User);
        let plan = Plan {
            kind: Kind::Install,
            changes: vec![Change::RegKeyCreated { hive: Hive::Hkcu, key: kp("Software\\Ours") }],
        };
        assert!(apply(&mut j, &mut sink, &mut os, plan, 1).is_ok());
        os.w.put_tree(0, "Software\\Ours", &three_levels("later"));
        let report = undo(&mut j, &mut sink, &mut os, &UndoSet::Classes(vec![KindClass::Install]));
        assert_eq!((report.restored, report.already, report.changed_by_other.len()), (1, 0, 0));
        assert_eq!(os.w, w);
        assert!(j.is_settled());
    }

    #[test]
    fn reg_key_created_on_an_existing_key_records_nothing() {
        let mut w = World::default();
        w.set(Hive::Hkcu, "Software\\Theirs", Some("v"), sz("user data"));
        w.put_tree(0, "Software\\Theirs\\Sub", &three_levels("user"));
        let mut os = FakeOs::new(w.clone());
        let crash = os.crash.clone();
        let (disk, mut sink) = new_disk(&crash);
        let mut j = Journal::empty(Scope::User);
        let plan = Plan {
            kind: Kind::Install,
            changes: vec![Change::RegKeyCreated { hive: Hive::Hkcu, key: kp("Software\\Theirs") }],
        };
        assert!(apply(&mut j, &mut sink, &mut os, plan, 1).is_ok());
        assert!(j.records().is_empty() && disk.borrow().is_empty(), "nothing recorded");
        assert_eq!(crash.done.get(), 0, "nothing changed");
        let report = undo(&mut j, &mut sink, &mut os, &UndoSet::Classes(ALL.to_vec()));
        assert_eq!(report, UndoReport::default());
        assert_eq!(os.w, w, "the key, its values and its subtree are still there");
    }

    #[test]
    fn missing_keys_are_journaled_and_removed_by_undo() {
        let mut w = World::default();
        w.create_key(0, "Software");
        let mut os = FakeOs::new(w.clone());
        let crash = os.crash.clone();
        let (_, mut sink) = new_disk(&crash);
        let mut j = Journal::empty(Scope::User);
        let plan =
            Plan { kind: TAKEOVER, changes: vec![value("Software\\New\\Deep", Some("x"), Some(RegData::Dword(1)))] };
        assert!(apply(&mut j, &mut sink, &mut os, plan, 1).is_ok());
        let kinds: Vec<_> = j.open_entries().iter().map(|e| target_of(&e.change)).collect();
        assert_eq!(
            kinds,
            vec![
                Some(Target::Reg { hive: Hive::Hkcu, key: "Software\\New".into(), name: None }),
                Some(Target::Reg { hive: Hive::Hkcu, key: "Software\\New\\Deep".into(), name: Some("x".into()) }),
            ]
        );
        let report = undo(&mut j, &mut sink, &mut os, &UndoSet::Classes(vec![KindClass::Takeover]));
        assert_eq!(report.restored, 2);
        assert_eq!(os.w, w, "no empty key is left behind");
    }

    #[test]
    fn undo_is_idempotent() {
        let mut os = FakeOs::new(start_world());
        let crash = os.crash.clone();
        let (disk, mut sink) = new_disk(&crash);
        let mut j = Journal::empty(Scope::User);
        assert!(apply(&mut j, &mut sink, &mut os, full_plan(), 1).is_ok());
        let applied = disk.borrow().clone();
        let entries = j.open_entries().len() as u32;
        let first = undo(&mut j, &mut sink, &mut os, &UndoSet::Classes(ALL.to_vec()));
        // The plan sets value a twice: the newest entry restores the first original, the older one finds it there.
        assert_eq!(
            (first.restored, first.already, first.changed_by_other.len(), first.failed.len()),
            (entries - 1, 1, 0, 0)
        );
        assert_eq!(os.w, start_world());
        assert_eq!(
            undo(&mut j, &mut sink, &mut os, &UndoSet::Classes(ALL.to_vec())),
            UndoReport::default(),
            "nothing open"
        );
        // The Undone records lost (a crash right after the undo): every entry is found already in its before state.
        let mut lost = Journal::parse(Scope::User, &applied).expect("parses");
        let (_, mut sink2) = new_disk(&crash);
        let again = undo(&mut lost, &mut sink2, &mut os, &UndoSet::Classes(ALL.to_vec()));
        assert_eq!((again.restored, again.already, again.changed_by_other.len()), (0, entries, 0));
        assert_eq!(os.w, start_world());
        assert!(lost.is_settled());
    }

    #[test]
    fn committed_runs_are_not_open() {
        let crash = Crash::new(None);
        let (_, mut sink) = new_disk(&crash);
        let mut j = Journal::empty(Scope::Machine);
        let swap =
            |n: &str| Change::FileSwap { dest: format!("C:\\Suite\\{n}.exe"), prev: None, staged_sha256: [1; 32] };
        let run = j.begin_run();
        assert_eq!(record(&mut j, &mut sink, run, Kind::Update, swap("tasks"), 1), Ok(1));
        assert_eq!(j.open_entries().len(), 1);
        assert!(commit(&mut j, &mut sink, run).is_ok());
        assert!(j.open_entries().is_empty() && j.is_settled());
        let run2 = j.begin_run();
        assert_eq!(record(&mut j, &mut sink, run2, Kind::Update, swap("files"), 2), Ok(2));
        assert!(!j.is_settled(), "a run without Committed is open");
        assert!(append(&mut j, &mut sink, Record::RunFailed { run: run2 }).is_ok());
        assert!(j.is_settled(), "a failed run is not open");
        assert_eq!(j.begin_run(), RunId(3));
    }

    #[test]
    fn record_refuses_a_hive_outside_the_scope_and_a_full_journal() {
        let crash = Crash::new(None);
        let (disk, mut sink) = new_disk(&crash);
        let mut user = Journal::empty(Scope::User);
        let hklm = Change::RegKeyCreated { hive: Hive::Hklm, key: kp("SOFTWARE\\x") };
        assert_eq!(record(&mut user, &mut sink, RunId(1), Kind::Install, hklm.clone(), 0).map_err(|e| e.code), Err(5));
        let mut machine = Journal::empty(Scope::Machine);
        assert!(record(&mut machine, &mut sink, RunId(1), Kind::Install, hklm, 0).is_ok());
        let hkcu = Change::RegKeyCreated { hive: Hive::Hkcu, key: kp("Software\\x") };
        assert_eq!(record(&mut machine, &mut sink, RunId(1), Kind::Install, hkcu, 0).map_err(|e| e.code), Err(5));
        for _ in 1..MAX_RECORDS {
            assert!(commit(&mut machine, &mut sink, RunId(1)).is_ok());
        }
        assert_eq!(commit(&mut machine, &mut sink, RunId(1)).map_err(|e| e.code), Err(112));
        assert_eq!(Journal::parse(Scope::Machine, &disk.borrow()).map(|j| j.records().len()), Ok(MAX_RECORDS));
    }

    #[test]
    fn apply_refuses_what_it_cannot_make_before_any_change() {
        let file = Change::FileCreated { path: "C:\\x.lnk".into(), after: [0; 32] };
        let mut bad_digest = tree_change("Software\\T", three_levels("x"));
        if let Change::RegTree { after, .. } = &mut bad_digest {
            after[0] ^= 1;
        }
        let unsorted = {
            let t = snap(vec![(Some("b"), sz("1")), (Some("a"), sz("2"))], vec![]);
            tree_change("Software\\T", t)
        };
        let case_twins = tree_change("Software\\T", snap(vec![(Some("A"), sz("1")), (Some("a"), sz("2"))], vec![]));
        let no_xml = Change::Task { name: tn("\\T1"), before_xml: None, after: [0; 32] };
        let hklm = Change::RegKeyCreated { hive: Hive::Hklm, key: kp("SOFTWARE\\x") };
        let marked = tree_change("Software\\T", snap(vec![(Some(WRITING_MARK), RegData::Dword(1))], vec![]));
        let cases =
            [(file, 87), (bad_digest, 87), (unsorted, 87), (case_twins, 87), (no_xml, 87), (hklm, 5), (marked, 87)];
        for (bad, code) in cases {
            let mut os = FakeOs::new(start_world());
            let crash = os.crash.clone();
            let (disk, mut sink) = new_disk(&crash);
            let mut j = Journal::empty(Scope::User);
            let plan =
                Plan { kind: TAKEOVER, changes: vec![value("Software\\A", Some("a"), Some(sz("1"))), bad.clone()] };
            match apply(&mut j, &mut sink, &mut os, plan, 1) {
                Err(ApplyError::Os { at: 1, err, undo }) => {
                    assert_eq!((err.code, undo), (code, UndoReport::default()), "{bad:?}");
                }
                other => panic!("{bad:?}: {other:?}"),
            }
            assert_eq!(os.w, start_world());
            assert!(disk.borrow().is_empty() && crash.done.get() == 0);
        }
    }

    #[test]
    fn a_failed_change_rolls_back_and_marks_the_run_failed() {
        let mut os = FakeOs::new(start_world());
        os.deny = Some("Software\\T".into());
        let crash = os.crash.clone();
        let (disk, mut sink) = new_disk(&crash);
        let mut j = Journal::empty(Scope::User);
        let r = apply(&mut j, &mut sink, &mut os, full_plan(), 1);
        match r {
            Err(ApplyError::Os { at: 3, err, undo }) => {
                assert_eq!(err.code, 5);
                assert_eq!((undo.restored, undo.already, undo.failed.len()), (3, 1, 0), "{undo:?}");
            }
            other => panic!("{other:?}"),
        }
        assert_eq!(os.w, start_world());
        let again = Journal::parse(Scope::User, &disk.borrow()).expect("parses");
        assert!(again.records().contains(&Record::RunFailed { run: RunId(1) }));
        assert!(again.is_settled());
    }

    #[test]
    fn a_tree_cut_part_way_is_ours_to_restore() {
        // Subkey b of the tree cannot be deleted (its ACL): the write stops after it removed a, and so does the
        // rollback. The entry stays open with its before; once b can go, the next undo restores the whole tree.
        let mut w = World::default();
        let old = snap(vec![(None, sz("old"))], vec![("a", three_levels("a")), ("b", three_levels("b"))]);
        w.put_tree(0, "Software\\T", &old);
        let mut os = FakeOs::new(w.clone());
        os.granular = true;
        os.deny = Some("Software\\T\\b".into());
        let crash = os.crash.clone();
        let (disk, mut sink) = new_disk(&crash);
        let mut j = Journal::empty(Scope::User);
        let plan = Plan { kind: TAKEOVER, changes: vec![tree_change("Software\\T", three_levels("new"))] };
        match apply(&mut j, &mut sink, &mut os, plan, 1) {
            Err(ApplyError::Os { at: 0, err, undo }) => {
                assert_eq!(err.code, 5);
                assert_eq!((undo.restored, undo.changed_by_other.len(), undo.failed.len()), (0, 0, 1), "{undo:?}");
            }
            other => panic!("{other:?}"),
        }
        assert!(!os.w.keys.contains(&(0, "Software\\T\\a".to_string())), "a went before b refused");
        let reread = Journal::parse(Scope::User, &disk.borrow()).expect("parses");
        assert!(!reread.is_settled(), "the entry and its before are kept");
        os.deny = None;
        let report = undo(&mut j, &mut sink, &mut os, &UndoSet::Classes(vec![KindClass::Takeover]));
        assert_eq!((report.restored, report.changed_by_other.len(), report.failed.len()), (1, 0, 0), "{report:?}");
        assert_eq!(os.w, w);
        assert!(j.is_settled());
    }

    #[test]
    fn a_stale_mark_is_not_taken_as_the_before() {
        // A mark with no open entry (its journal was lost): the before is the tree without it.
        let mut w = World::default();
        w.put_tree(0, "Software\\T", &three_levels("old"));
        let clean = w.clone();
        w.set(Hive::Hkcu, "Software\\T", Some(WRITING_MARK), RegData::Dword(1));
        let mut os = FakeOs::new(w);
        let crash = os.crash.clone();
        let (_, mut sink) = new_disk(&crash);
        let mut j = Journal::empty(Scope::User);
        let plan = Plan { kind: TAKEOVER, changes: vec![tree_change("Software\\T", three_levels("new"))] };
        assert!(apply(&mut j, &mut sink, &mut os, plan, 1).is_ok());
        let befores: Vec<_> = j
            .open_entries()
            .iter()
            .filter_map(|e| match &e.change {
                Change::RegTree { before, .. } => Some(before.clone()),
                _ => None,
            })
            .collect();
        assert_eq!(befores, vec![Some(three_levels("old"))]);
        let report = undo(&mut j, &mut sink, &mut os, &UndoSet::Classes(vec![KindClass::Takeover]));
        assert_eq!(report.restored, 1);
        assert_eq!(os.w, clean);
    }

    #[test]
    fn views_name_the_open_entries() {
        let mut os = FakeOs::new(start_world());
        let crash = os.crash.clone();
        let (_, mut sink) = new_disk(&crash);
        let mut j = Journal::empty(Scope::User);
        let plan = Plan {
            kind: TAKEOVER,
            changes: vec![value("Software\\A", Some("a"), Some(sz("1"))), task_change("\\T1", "<x/>")],
        };
        assert!(apply(&mut j, &mut sink, &mut os, plan, 77).is_ok());
        assert!(
            record(&mut j, &mut sink, RunId(9), Kind::Install, Change::Unknown(r#"{"t":"later"}"#.into()), 78).is_ok()
        );
        let v = views(&j);
        assert_eq!(
            v,
            vec![
                EntryView {
                    seq: 1,
                    kind: TAKEOVER,
                    target: Target::Reg { hive: Hive::Hkcu, key: "Software\\A".into(), name: Some("a".into()) },
                    ts: 77
                },
                EntryView { seq: 2, kind: TAKEOVER, target: Target::Task("\\T1".into()), ts: 77 },
            ]
        );
        assert_eq!(j.open_entries().len(), 3, "the Unknown change is open but has no view");
    }

    #[test]
    fn unknown_kinds_are_kept_and_never_undone() {
        let mut os = FakeOs::new(start_world());
        let crash = os.crash.clone();
        let (disk, mut sink) = new_disk(&crash);
        let mut j = Journal::empty(Scope::User);
        let newer = Change::Unknown(r#"{"t":"shell_ext","clsid":"{0}","n":[1, 2]}"#.into());
        let run = j.begin_run();
        assert!(record(&mut j, &mut sink, run, Kind::Takeover(TakeoverId::FolderVerbs), newer.clone(), 1).is_ok());
        let svc = Change::ServiceStartType { name: "svc".into(), before: 3, after: 4 };
        let mut machine = Journal::empty(Scope::Machine);
        let (_, mut machine_sink) = new_disk(&crash);
        assert!(record(&mut machine, &mut machine_sink, RunId(1), Kind::UserEdit(AppId::Tasks), svc, 1).is_ok());
        let kind = Kind::Unknown("takeover.later".into());
        let plan = Plan { kind: kind.clone(), changes: vec![value("Software\\A", Some("a"), Some(sz("1")))] };
        let later_run = apply(&mut j, &mut sink, &mut os, plan, 2).expect("applies");
        let reread = Journal::parse(Scope::User, &disk.borrow()).expect("parses");
        assert!(matches!(&reread.records()[0], Record::Entry(e) if e.change == newer));
        // By class: neither the Unknown change nor the Unknown kind is selected.
        let by_class = undo(&mut j, &mut sink, &mut os, &UndoSet::Classes(ALL.to_vec()));
        assert_eq!((by_class.unknown, by_class.restored), (1, 0));
        assert_eq!(j.open_entries().len(), 2);
        // By run: the Unknown kind's change is undone.
        let by_run = undo(&mut j, &mut sink, &mut os, &UndoSet::Run(later_run));
        assert_eq!(by_run.restored, 1);
        assert_eq!(os.w, start_world());
        let all = undo(&mut machine, &mut machine_sink, &mut os, &UndoSet::Classes(ALL.to_vec()));
        assert_eq!((all.unknown, all.restored), (1, 0), "ServiceStartType is never undone in v1");
        assert!(!j.is_settled() && !machine.is_settled(), "kept open for a newer version");
    }

    #[test]
    fn crash_at_every_op_of_apply() {
        let start = start_world();
        // Granular: the fake's tree calls are cut between their key and value writes too, as RealOs's can be.
        for granular in [false, true] {
            let total = ops_of(&start, &full_plan(), granular);
            assert!(total > 20, "{total}");
            for k in 0..=total {
                let crash = Crash::new(Some(k));
                let mut os = FakeOs::with(&start, &crash);
                os.granular = granular;
                let (disk, mut sink) = new_disk(&crash);
                let mut j = Journal::empty(Scope::User);
                let r = apply(&mut j, &mut sink, &mut os, full_plan(), 1);
                assert_eq!(r.is_ok(), k == total, "granular {granular} k {k}");
                crash.revive();
                let (mut j2, mut sink2) = reopen(&disk, Scope::User, &crash);
                let report = undo(&mut j2, &mut sink2, &mut os, &UndoSet::Classes(vec![KindClass::Takeover]));
                assert!(report.failed.is_empty(), "granular {granular} k {k}: {report:?}");
                assert_eq!(os.w, start, "granular {granular} k {k}");
                assert!(j2.is_settled(), "granular {granular} k {k}");
            }
        }
    }

    #[test]
    fn crash_at_every_op_of_undo() {
        let start = start_world();
        let crash = Crash::new(None);
        let mut os = FakeOs::with(&start, &crash);
        let (disk, mut sink) = new_disk(&crash);
        let mut j = Journal::empty(Scope::User);
        assert!(apply(&mut j, &mut sink, &mut os, full_plan(), 1).is_ok());
        let applied_world = os.w.clone();
        let applied_disk = disk.borrow().clone();
        // Granular: the fake's tree calls are cut between their key and value writes too, as RealOs's can be.
        for granular in [false, true] {
            let total = {
                let c = Crash::new(None);
                let mut os = FakeOs::with(&applied_world, &c);
                os.granular = granular;
                let d = Rc::new(RefCell::new(applied_disk.clone()));
                let (mut j, mut s) = reopen(&d, Scope::User, &c);
                undo(&mut j, &mut s, &mut os, &UndoSet::Classes(ALL.to_vec()));
                c.done.get()
            };
            for k in 0..=total {
                let c = Crash::new(Some(k));
                let mut os = FakeOs::with(&applied_world, &c);
                os.granular = granular;
                let d = Rc::new(RefCell::new(applied_disk.clone()));
                let (mut j, mut s) = reopen(&d, Scope::User, &c);
                undo(&mut j, &mut s, &mut os, &UndoSet::Classes(ALL.to_vec()));
                c.revive();
                let (mut j2, mut s2) = reopen(&d, Scope::User, &c);
                let report = undo(&mut j2, &mut s2, &mut os, &UndoSet::Classes(ALL.to_vec()));
                assert!(report.failed.is_empty(), "granular {granular} k {k}: {report:?}");
                assert_eq!(os.w, start, "granular {granular} k {k}");
                assert!(j2.is_settled(), "granular {granular} k {k}");
            }
        }
    }

    // ---------------------------------------------------------------------------------------- random plans and fuzz

    /// xorshift64*: seeded, so a failure replays.
    struct Rng(u64);

    impl Rng {
        fn next(&mut self) -> u64 {
            self.0 ^= self.0 >> 12;
            self.0 ^= self.0 << 25;
            self.0 ^= self.0 >> 27;
            self.0.wrapping_mul(0x2545_f491_4f6c_dd1d)
        }

        fn below(&mut self, n: usize) -> usize {
            (self.next() % n.max(1) as u64) as usize
        }

        fn text(&mut self) -> String {
            let alphabet = ['a', 'Z', '0', ' ', '"', '\\', '\n', '\u{1}', '\u{e8}', '\u{1f600}', '{', ':'];
            (0..self.below(6)).map(|_| alphabet[self.below(alphabet.len())]).collect()
        }

        fn data(&mut self) -> RegData {
            match self.below(7) {
                0 => RegData::Sz(self.text()),
                1 => RegData::ExpandSz(self.text()),
                2 => RegData::MultiSz((0..self.below(3)).map(|_| self.text()).collect()),
                3 => RegData::Dword(self.next() as u32),
                4 => RegData::Qword(self.next()),
                5 => RegData::Binary((0..self.below(8)).map(|_| self.next() as u8).collect()),
                _ => RegData::Other {
                    ty: 100 + self.below(5) as u32,
                    bytes: (0..self.below(4)).map(|_| self.next() as u8).collect(),
                },
            }
        }

        /// A canonical tree of `levels` levels (BTreeMaps give the order).
        fn tree(&mut self, levels: u8) -> RegSnapshot {
            let names = ["a", "b", "c", "d"];
            let mut values = BTreeMap::new();
            for _ in 0..self.below(3) {
                let n = if self.below(4) == 0 { None } else { Some(names[self.below(4)].to_string()) };
                values.insert(n, self.data());
            }
            let mut subkeys = BTreeMap::new();
            if levels > 1 {
                for _ in 0..1 + self.below(2) {
                    subkeys.insert(names[self.below(4)].to_string(), self.tree(levels - 1));
                }
            }
            RegSnapshot { values: values.into_iter().collect(), subkeys: subkeys.into_iter().collect() }
        }
    }

    /// Four keys: R0 and R1 exist, R2 holds a tree (or is missing), R3 is missing; task T1 exists, T2 does not.
    fn random_case(rng: &mut Rng) -> (World, Plan) {
        let mut w = World::default();
        w.set(Hive::Hkcu, "Software\\R0", Some("a"), rng.data());
        w.set(Hive::Hkcu, "Software\\R1", None, rng.data());
        if rng.below(2) == 0 {
            w.put_tree(0, "Software\\R2", &rng.tree(3));
        }
        w.tasks.insert("\\T1".into(), format!("<t1>{}</t1>", rng.below(1000)));
        let names = [None, Some("a"), Some("b"), Some("c")];
        let changes = (0..1 + rng.below(12))
            .map(|_| match rng.below(5) {
                0 | 1 => {
                    let key = ["Software\\R0", "Software\\R1", "Software\\R3"][rng.below(3)];
                    let after = if rng.below(5) == 0 { None } else { Some(rng.data()) };
                    value(key, names[rng.below(4)], after)
                }
                2 => tree_change("Software\\R2", rng.tree(3)),
                3 => {
                    let key = ["Software\\R3", "Software\\R3\\Sub"][rng.below(2)];
                    Change::RegKeyCreated { hive: Hive::Hkcu, key: kp(key) }
                }
                _ => {
                    let xml = format!("<t>{}</t>", rng.next());
                    task_change(["\\T1", "\\T2"][rng.below(2)], &xml)
                }
            })
            .collect();
        (w, Plan { kind: TAKEOVER, changes })
    }

    #[test]
    fn random_plans_500() {
        let mut rng = Rng(0x5eed_1234_abcd_0001);
        for case in 0..500 {
            let (start, plan) = random_case(&mut rng);
            let copy = |p: &Plan| Plan { kind: p.kind.clone(), changes: p.changes.clone() };
            // Whole: apply, then undo the run.
            let mut os = FakeOs::new(start.clone());
            let crash = os.crash.clone();
            let (_, mut sink) = new_disk(&crash);
            let mut j = Journal::empty(Scope::User);
            let run =
                apply(&mut j, &mut sink, &mut os, copy(&plan), 1).unwrap_or_else(|e| panic!("case {case}: {e:?}"));
            let report = undo(&mut j, &mut sink, &mut os, &UndoSet::Run(run));
            assert!(report.failed.is_empty() && report.changed_by_other.is_empty(), "case {case}: {report:?}");
            assert_eq!(os.w, start, "case {case}");
            assert!(j.is_settled(), "case {case}");
            // Crashed at a random op, then undone by the next process.
            let total = ops_of(&start, &plan, false);
            let k = rng.below(total + 1);
            let crash = Crash::new(Some(k));
            let mut os = FakeOs::with(&start, &crash);
            let (disk, mut sink) = new_disk(&crash);
            let mut j = Journal::empty(Scope::User);
            let _ = apply(&mut j, &mut sink, &mut os, copy(&plan), 1);
            crash.revive();
            let (mut j2, mut sink2) = reopen(&disk, Scope::User, &crash);
            let report = undo(&mut j2, &mut sink2, &mut os, &UndoSet::Classes(vec![KindClass::Takeover]));
            assert!(report.failed.is_empty(), "case {case} k {k}: {report:?}");
            assert_eq!(os.w, start, "case {case} k {k}");
        }
    }

    /// Inputs for the parser: random bytes, the frozen file mutated, and frozen JSON mutated under a correct crc (so
    /// the JSON reader and the checks behind it are reached).
    fn fuzz_input(rng: &mut Rng) -> Vec<u8> {
        let frozen = frozen_file();
        match rng.below(4) {
            0 => (0..rng.below(300)).map(|_| rng.next() as u8).collect(),
            1 => {
                let mut b = frozen;
                for _ in 0..1 + rng.below(3) {
                    let at = rng.below(b.len());
                    match rng.below(4) {
                        0 => b[at] ^= 1 << rng.below(8),
                        1 => {
                            let end = (at + rng.below(40)).min(b.len());
                            b.drain(at..end);
                        }
                        2 => b.insert(at, rng.next() as u8),
                        _ => b.truncate(at),
                    }
                    if b.is_empty() {
                        break;
                    }
                }
                b
            }
            _ => {
                let mut out = Vec::new();
                for _ in 0..1 + rng.below(4) {
                    let (_, json) = FROZEN_LINES[rng.below(FROZEN_LINES.len())];
                    let mut chars: Vec<char> = json.chars().collect();
                    for _ in 0..1 + rng.below(3) {
                        let at = rng.below(chars.len());
                        let pool =
                            ['"', '{', '}', '[', ']', ',', ':', '0', '9', 'a', 'f', 'n', '\\', '-', ' ', '\u{e8}', 'A'];
                        match rng.below(3) {
                            0 => chars[at] = pool[rng.below(pool.len())],
                            1 => {
                                chars.remove(at);
                            }
                            _ => chars.insert(at, pool[rng.below(pool.len())]),
                        }
                    }
                    let json: String = chars.into_iter().collect();
                    out.extend(format!("{:08x} {json}\n", crc32(json.as_bytes())).into_bytes());
                }
                out
            }
        }
    }

    fn fuzz(n: usize, seed: u64) {
        let mut rng = Rng(seed);
        let mut parsed = 0usize;
        for i in 0..n {
            let input = fuzz_input(&mut rng);
            if let Ok(j) = Journal::parse(Scope::User, &input) {
                parsed += 1;
                // What parses is written again, and reads back the same.
                for r in j.records() {
                    let line = Journal::encode(r).unwrap_or_else(|e| panic!("input {i}: {r:?} does not encode: {e:?}"));
                    let back = Journal::parse(Scope::User, &line).map(|j| j.records().to_vec());
                    assert_eq!(back, Ok(vec![r.clone()]), "input {i}");
                }
            }
        }
        assert!(parsed > n / 50, "too few inputs parsed ({parsed}): the mutations miss the parser");
    }

    #[test]
    fn fuzz_journal_10k() {
        fuzz(10_000, 0x0123_4567_89ab_cdef);
    }

    #[test]
    #[ignore = "1,000,000 inputs: fuzz-nightly.yml and the coder once per group (cargo test --release -- --ignored fuzz_)"]
    fn fuzz_journal_1m() {
        fuzz(1_000_000, 0x0fed_cba9_8765_4321);
    }

    #[test]
    #[ignore = "bench: 100 entries undone on the fake OS within 50 ms (recorded)"]
    fn bench_undo_100() {
        let mut w = World::default();
        w.create_key(0, "Software\\B");
        let mut os = FakeOs::new(w.clone());
        let crash = os.crash.clone();
        let (_, mut sink) = new_disk(&crash);
        let mut j = Journal::empty(Scope::User);
        let changes = (0..100).map(|i| value("Software\\B", Some(&format!("v{i}")), Some(RegData::Dword(i)))).collect();
        assert!(apply(&mut j, &mut sink, &mut os, Plan { kind: TAKEOVER, changes }, 1).is_ok());
        let t = std::time::Instant::now();
        let report = undo(&mut j, &mut sink, &mut os, &UndoSet::Classes(vec![KindClass::Takeover]));
        let ms = t.elapsed().as_secs_f64() * 1000.0;
        println!("bench_undo_100: {ms:.2} ms for {} entries", report.restored);
        assert_eq!(report.restored, 100);
        assert_eq!(os.w, w);
        assert!(ms <= 50.0, "{ms} ms");
    }
}
