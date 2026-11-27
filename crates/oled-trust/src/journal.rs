//! The change journal: line format, records and the undo planner (F-P5).
// PF1 freeze: the signatures below are frozen; bodies arrive with their tasks (J1).
#![allow(dead_code, unused_variables)]

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
        unimplemented!("PF1 stub")
    }
    pub fn as_str(&self) -> &str {
        unimplemented!("PF1 stub")
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
        unimplemented!("PF1 stub")
    }
    pub fn as_str(&self) -> &str {
        unimplemented!("PF1 stub")
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
pub struct Journal {
    // scope, records, next seq, next run
    _private: (),
}
impl Journal {
    pub fn empty(scope: Scope) -> Journal {
        unimplemented!("PF1 stub")
    }
    pub fn parse(scope: Scope, bytes: &[u8]) -> Result<Journal, JournalError> {
        unimplemented!("PF1 stub")
    } // all-or-nothing; a last line without '\n' is torn and ignored
    pub fn encode(r: &Record) -> Result<Vec<u8>, JournalError> {
        unimplemented!("PF1 stub")
    } // the line below; TooLarge above MAX_LINE or a field cap
    pub fn records(&self) -> &[Record] {
        unimplemented!("PF1 stub")
    }
    pub fn open_entries(&self) -> Vec<&Entry> {
        unimplemented!("PF1 stub")
    } // not undone, run not failed, run not committed
    pub fn is_settled(&self) -> bool {
        unimplemented!("PF1 stub")
    } // no open entry: the file may be deleted
    pub fn begin_run(&mut self) -> RunId {
        unimplemented!("PF1 stub")
    } // max(run in the file) + 1
    pub fn next_seq(&self) -> u64 {
        unimplemented!("PF1 stub")
    } // max(seq in the file) + 1, read under the lock (journal_os::open_locked)
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
    unimplemented!("PF1 stub")
} // the snapshot's JSON text exactly as in a line
pub fn digest_tree(s: &RegSnapshot, os: &dyn JournalOs) -> Result<Digest, OsErr> {
    unimplemented!("PF1 stub")
} // os.digest(&canonical_tree(s))
pub fn digest_text(s: &str, os: &dyn JournalOs) -> Result<Digest, OsErr> {
    unimplemented!("PF1 stub")
} // os.digest(s.as_bytes())
pub fn record(
    j: &mut Journal,
    sink: &mut dyn Sink,
    run: RunId,
    kind: Kind,
    change: Change,
    now: u64,
) -> Result<u64, OsErr> {
    unimplemented!("PF1 stub")
}
pub fn commit(j: &mut Journal, sink: &mut dyn Sink, run: RunId) -> Result<(), OsErr> {
    unimplemented!("PF1 stub")
} // appends Committed{run}
pub struct Plan {
    pub kind: Kind,
    pub changes: Vec<Change>,
} // "before" fields are filled by apply from the OS
pub fn apply(
    j: &mut Journal,
    sink: &mut dyn Sink,
    os: &mut dyn JournalOs,
    plan: Plan,
    now: u64,
) -> Result<RunId, ApplyError> {
    unimplemented!("PF1 stub")
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
pub fn undo(j: &mut Journal, sink: &mut dyn Sink, os: &mut dyn JournalOs, set: &UndoSet) -> UndoReport {
    unimplemented!("PF1 stub")
} // newest first, idempotent
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EntryView {
    pub seq: u64,
    pub kind: Kind,
    pub target: Target,
    pub ts: u64,
}
pub fn views(j: &Journal) -> Vec<EntryView> {
    unimplemented!("PF1 stub")
}
