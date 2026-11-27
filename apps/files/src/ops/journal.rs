//! The operation journal (OP1): one JSONL file per operation, <data>\ops\<op>-<host pid>.jsonl, written through
//! OwnDir::append. Line 1 is the header; then one record per top-level staged item at each step ("queued" for every
//! item held in memory before the first PerformOperations, "started" at its Pre callback, "done", "failed" or
//! "skipped" at its Post callback); the last line is the end. Nested items are never journaled: resume works per
//! top-level item. Paths are JSON strings, or {"w": "<hex of UTF-16LE>"} when they hold a lone surrogate.
//!
//! Flush rule (open point 5): FlushFileBuffers after the header, then when 250 ms or 64 records have passed since the
//! last flush (checked at each write, no timer), at the end and at WM_ENDSESSION (flush_all). A record lost in a crash
//! only turns an automatic Replace into the system's own question at resume, never into data loss.
//!
//! Staged items above the in-memory cap spill to <data>\ops\<op>-<host pid>.stage: a u16 unit count and the UTF-16LE
//! units per item, appended by Stage, read back in order chunk by chunk and removed with the journal.
#![deny(clippy::unwrap_used, clippy::expect_used, clippy::indexing_slicing)]

use std::fs::File;
use std::io::{self, BufRead, BufReader, BufWriter, Read, Write};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use oled_trust::frame::files::{Conflicts, OpId, OpKind, WidePath, MAX_PATH_UNITS};
use serde::{Deserialize, Serialize};

use super::consts::STAGE_MAX;
use crate::own_files::OwnDir;

/// The journal format's version (header "v").
pub const VERSION: u32 = 1;
/// Flush when this much time has passed since the last flush ...
pub const FLUSH_EVERY: Duration = Duration::from_millis(250);
/// ... or this many records.
pub const FLUSH_RECORDS: u32 = 64;

/// The journal of operation `op` run by the host `pid`, relative to the data folder.
pub fn journal_rel(op: OpId, pid: u32) -> String {
    format!("ops\\{}-{pid}.jsonl", op.0)
}

/// The stage spill of operation `op` staged in the host `pid`, relative to the data folder.
pub fn stage_rel(op: OpId, pid: u32) -> String {
    format!("ops\\{}-{pid}.stage", op.0)
}

/// (op, host pid) of a journal or stage file name ("12-3456.jsonl"), None for any other name.
pub fn parse_name(name: &str, ext: &str) -> Option<(OpId, u32)> {
    let stem = name.strip_suffix(ext)?.strip_suffix('.')?;
    let (op, pid) = stem.split_once('-')?;
    let digits = |s: &str| !s.is_empty() && s.len() <= 10 && s.bytes().all(|b| b.is_ascii_digit());
    if !digits(op) || !digits(pid) {
        return None;
    }
    Some((OpId(op.parse().ok()?), pid.parse().ok()?))
}

/// A path as the journal writes it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum PathRepr {
    Text(String),
    Wide { w: String },
}

impl PathRepr {
    pub fn of(p: &WidePath) -> PathRepr {
        match String::from_utf16(p.units()) {
            Ok(s) => PathRepr::Text(s),
            Err(_) => PathRepr::Wide {
                w: p.units().iter().flat_map(|u| u.to_le_bytes()).map(|b| format!("{b:02x}")).collect(),
            },
        }
    }

    /// The path, or None when it is not a valid WidePath (a hand-edited or damaged journal).
    pub fn to_path(&self) -> Option<WidePath> {
        match self {
            PathRepr::Text(s) => WidePath::parse(s).ok(),
            PathRepr::Wide { w } => {
                let bytes = w.as_bytes();
                if bytes.len() % 4 != 0 || bytes.len() / 4 > usize::from(MAX_PATH_UNITS) {
                    return None;
                }
                let mut units = Vec::with_capacity(bytes.len() / 4);
                for q in bytes.chunks_exact(4) {
                    let s = std::str::from_utf8(q).ok()?;
                    let lo = u8::from_str_radix(s.get(..2)?, 16).ok()?;
                    let hi = u8::from_str_radix(s.get(2..)?, 16).ok()?;
                    units.push(u16::from_le_bytes([lo, hi]));
                }
                WidePath::new(&units).ok()
            }
        }
    }
}

/// The operation kind as the journal names it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum JKind {
    Copy,
    Move,
    Recycle,
    DeletePermanent,
    Rename,
    NewFolder,
    Undo,
    Resume,
}

impl From<OpKind> for JKind {
    fn from(k: OpKind) -> JKind {
        match k {
            OpKind::Copy => JKind::Copy,
            OpKind::Move => JKind::Move,
            OpKind::Recycle => JKind::Recycle,
            OpKind::DeletePermanent => JKind::DeletePermanent,
            OpKind::Rename => JKind::Rename,
            OpKind::NewFolder => JKind::NewFolder,
            OpKind::Undo => JKind::Undo,
            OpKind::Resume => JKind::Resume,
        }
    }
}

impl From<JKind> for OpKind {
    fn from(k: JKind) -> OpKind {
        match k {
            JKind::Copy => OpKind::Copy,
            JKind::Move => OpKind::Move,
            JKind::Recycle => OpKind::Recycle,
            JKind::DeletePermanent => OpKind::DeletePermanent,
            JKind::Rename => OpKind::Rename,
            JKind::NewFolder => OpKind::NewFolder,
            JKind::Undo => OpKind::Undo,
            JKind::Resume => OpKind::Resume,
        }
    }
}

/// Conflicts as the journal names them.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum JConflicts {
    Ask,
    Replace,
    Skip,
}

impl From<Conflicts> for JConflicts {
    fn from(c: Conflicts) -> JConflicts {
        match c {
            Conflicts::AskSystem => JConflicts::Ask,
            Conflicts::ReplaceAll => JConflicts::Replace,
            Conflicts::SkipAll => JConflicts::Skip,
        }
    }
}

impl From<JConflicts> for Conflicts {
    fn from(c: JConflicts) -> Conflicts {
        match c {
            JConflicts::Ask => Conflicts::AskSystem,
            JConflicts::Replace => Conflicts::ReplaceAll,
            JConflicts::Skip => Conflicts::SkipAll,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Header {
    pub v: u32,
    pub op: u32,
    pub kind: JKind,
    pub host_pid: u32,
    /// The host process's creation time (FILETIME): a journal belongs to a live host only if both match.
    pub host_created: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dest: Option<PathRepr>,
    pub conflicts: JConflicts,
    /// Top-level items of the operation.
    pub items: u64,
    /// FILETIME of the start.
    pub started: u64,
}

/// What the destination of a top-level item was before the operation (pre-flight, OP3).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Pre {
    Absent,
    File,
    Dir,
    Reparse,
}

/// The step a record reports.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum St {
    Queued,
    Started,
    Done,
    Failed,
    Skipped,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Record {
    /// The item's index among the operation's top-level items.
    pub i: u32,
    pub src: PathRepr,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dst: Option<PathRepr>,
    pub pre: Pre,
    pub st: St,
    #[serde(default)]
    pub size: u64,
    #[serde(default)]
    pub mtime: i64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EndKind {
    Done,
    Cancelled,
    Failed,
}

#[derive(Serialize, Deserialize)]
struct EndLine {
    end: EndKind,
}

/// A parsed journal: the header, every record in file order and the end when it was written.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Journal {
    pub header: Header,
    pub records: Vec<Record>,
    pub end: Option<EndKind>,
}

impl Journal {
    /// The last record of each item index, in index order.
    pub fn latest(&self) -> Vec<&Record> {
        let mut by: std::collections::BTreeMap<u32, &Record> = std::collections::BTreeMap::new();
        for r in &self.records {
            by.insert(r.i, r);
        }
        by.into_values().collect()
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum JournalError {
    /// No complete header line.
    NoHeader,
    /// A complete line (not the last) that is not a record.
    BadLine(usize),
    /// A header of another version.
    Version(u32),
}

/// Parses a journal. A last line that does not parse (a write cut by a crash) is ignored; any other bad line is an
/// error.
pub fn parse(bytes: &[u8]) -> Result<Journal, JournalError> {
    let text = String::from_utf8_lossy(bytes);
    let mut lines: Vec<&str> = text.split('\n').collect();
    // The text after the last newline is either empty or a torn line.
    let tail = lines.pop().unwrap_or_default();
    let mut header: Option<Header> = None;
    let mut records = Vec::new();
    let mut end = None;
    let all = lines.into_iter().map(|l| (l, false)).chain(std::iter::once((tail, true)));
    for (n, (line, torn_ok)) in all.enumerate() {
        let line = line.trim_end_matches('\r');
        if line.trim().is_empty() {
            continue;
        }
        let parsed = parse_line(line, header.is_none());
        match parsed {
            Some(Line::Header(h)) => {
                if h.v != VERSION {
                    return Err(JournalError::Version(h.v));
                }
                header = Some(h);
            }
            Some(Line::Record(r)) if header.is_some() => records.push(r),
            Some(Line::End(e)) if header.is_some() => end = Some(e),
            _ if torn_ok => {}
            _ if header.is_none() => return Err(JournalError::NoHeader),
            _ => return Err(JournalError::BadLine(n + 1)),
        }
    }
    let header = header.ok_or(JournalError::NoHeader)?;
    Ok(Journal { header, records, end })
}

enum Line {
    Header(Header),
    Record(Record),
    End(EndKind),
}

fn parse_line(line: &str, first: bool) -> Option<Line> {
    if first {
        return serde_json::from_str::<Header>(line).ok().map(Line::Header);
    }
    let v: serde_json::Value = serde_json::from_str(line).ok()?;
    if v.get("end").is_some() {
        return serde_json::from_value::<EndLine>(v).ok().map(|e| Line::End(e.end));
    }
    serde_json::from_value::<Record>(v).ok().map(Line::Record)
}

/// What Pending needs of one journal: its header, whether it ended, and how many items are done or skipped at their
/// latest record.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Summary {
    pub header: Header,
    pub ended: bool,
    pub done: u32,
}

/// Reads a journal line by line, holding one line and one bit per item (an interrupted copy of a million items leaves
/// hundreds of MB): its lines count as for parse (a torn last line is ignored, any other bad line refuses the
/// journal). `wanted` sees the header first, and false stops the read there. None when the journal does not parse or
/// is not wanted.
pub fn summary(r: &mut impl BufRead, wanted: impl FnOnce(&Header) -> bool) -> Option<Summary> {
    let mut wanted = Some(wanted);
    let mut header: Option<Header> = None;
    let mut ended = false;
    // Bit i: item i's latest record is done or skipped. A runner's indexes stay below the header's count, which is at
    // most STAGE_MAX.
    let mut bits: Vec<u64> = Vec::new();
    let mut cap = 0u64;
    let mut line = Vec::new();
    loop {
        line.clear();
        if r.read_until(b'\n', &mut line).ok()? == 0 {
            break;
        }
        // Only the text after the last newline lacks one: a write cut by a crash.
        let torn_ok = line.last() != Some(&b'\n');
        let text = String::from_utf8_lossy(&line);
        let text = text.strip_suffix('\n').unwrap_or(&text).trim_end_matches('\r');
        if text.trim().is_empty() {
            continue;
        }
        match parse_line(text, header.is_none()) {
            Some(Line::Header(h)) => {
                if h.v != VERSION || !wanted.take().is_some_and(|f| f(&h)) {
                    return None;
                }
                cap = h.items.min(STAGE_MAX);
                bits = vec![0; usize::try_from(cap.div_ceil(64)).unwrap_or(0)];
                header = Some(h);
            }
            Some(Line::Record(rec)) if header.is_some() => {
                let bit = 1u64 << (rec.i % 64);
                match bits.get_mut((rec.i / 64) as usize) {
                    Some(w) if u64::from(rec.i) < cap && matches!(rec.st, St::Done | St::Skipped) => *w |= bit,
                    Some(w) => *w &= !bit,
                    None => {}
                }
            }
            Some(Line::End(_)) if header.is_some() => ended = true,
            _ if torn_ok => {}
            _ => return None,
        }
    }
    let done = bits.iter().map(|w| w.count_ones()).sum();
    Some(Summary { header: header?, ended, done })
}

/// A file of the data folder opened to be read line by line (journal::summary), refused when it is a link or not a
/// plain file, as OwnDir::read refuses them. `rel` is a name the folder's own listing gave.
pub fn reader(dir: &OwnDir, rel: &str) -> io::Result<BufReader<File>> {
    Ok(BufReader::new(dir.open_read(rel)?))
}

/// When the next flush is due: 250 ms or 64 records after the last one (checked at each write).
#[derive(Clone, Copy, Debug)]
pub struct FlushRule {
    last: Instant,
    pending: u32,
}

impl FlushRule {
    pub fn new(now: Instant) -> FlushRule {
        FlushRule { last: now, pending: 0 }
    }

    /// One record was written at `now`: true when it must be flushed now (the rule restarts from it).
    pub fn wrote(&mut self, now: Instant) -> bool {
        self.pending = self.pending.saturating_add(1);
        if self.pending >= FLUSH_RECORDS || now.saturating_duration_since(self.last) >= FLUSH_EVERY {
            self.flushed(now);
            true
        } else {
            false
        }
    }

    pub fn flushed(&mut self, now: Instant) {
        self.last = now;
        self.pending = 0;
    }
}

/// Clones of the open journals' handles, so WM_ENDSESSION can flush every journal of the process.
static OPEN: Mutex<Vec<(u64, File)>> = Mutex::new(Vec::new());
static NEXT_ID: AtomicU64 = AtomicU64::new(1);

/// Flushes every open journal of this process (WM_ENDSESSION).
pub fn flush_all() {
    let open = OPEN.lock().unwrap_or_else(|e| e.into_inner());
    for (_, f) in open.iter() {
        let _ = f.sync_all();
    }
}

/// The writer of one operation's journal.
pub struct JournalWriter {
    file: File,
    id: u64,
    rule: FlushRule,
    flushes: u32,
    pub rel: String,
}

impl JournalWriter {
    /// Creates the journal of `op` by the host `pid` and writes and flushes its header.
    pub fn create(dir: &OwnDir, op: OpId, pid: u32, header: &Header, now: Instant) -> io::Result<JournalWriter> {
        let rel = journal_rel(op, pid);
        let file = dir.append(&rel)?;
        let id = NEXT_ID.fetch_add(1, Ordering::Relaxed);
        if let Ok(clone) = file.try_clone() {
            OPEN.lock().unwrap_or_else(|e| e.into_inner()).push((id, clone));
        }
        let mut w = JournalWriter { file, id, rule: FlushRule::new(now), flushes: 0, rel };
        w.line(&serde_json::to_string(header).map_err(io::Error::other)?)?;
        w.flush(now)?;
        Ok(w)
    }

    /// Writes one record; flushes when the rule says so.
    pub fn record(&mut self, r: &Record, now: Instant) -> io::Result<()> {
        self.line(&serde_json::to_string(r).map_err(io::Error::other)?)?;
        if self.rule.wrote(now) {
            self.sync()?;
        }
        Ok(())
    }

    /// Writes the end line and flushes.
    pub fn end(&mut self, e: EndKind, now: Instant) -> io::Result<()> {
        self.line(&serde_json::to_string(&EndLine { end: e }).map_err(io::Error::other)?)?;
        self.flush(now)
    }

    /// Flushes now (FinishOperations).
    pub fn flush(&mut self, now: Instant) -> io::Result<()> {
        self.rule.flushed(now);
        self.sync()
    }

    /// How many times the journal was flushed (tests).
    pub fn flushes(&self) -> u32 {
        self.flushes
    }

    fn sync(&mut self) -> io::Result<()> {
        self.flushes += 1;
        self.file.sync_all()
    }

    fn line(&mut self, s: &str) -> io::Result<()> {
        let mut bytes = Vec::with_capacity(s.len() + 1);
        bytes.extend_from_slice(s.as_bytes());
        bytes.push(b'\n');
        self.file.write_all(&bytes)
    }
}

impl Drop for JournalWriter {
    fn drop(&mut self) {
        let mut open = OPEN.lock().unwrap_or_else(|e| e.into_inner());
        open.retain(|(id, _)| *id != self.id);
    }
}

/// The current time as a FILETIME value.
pub fn now_filetime() -> u64 {
    // FILETIME counts 100 ns ticks from 1601-01-01; the Unix epoch is 11,644,473,600 s later.
    const EPOCH_DIFF: u64 = 116_444_736_000_000_000;
    let since = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap_or_default();
    EPOCH_DIFF.saturating_add(u64::try_from(since.as_nanos() / 100).unwrap_or(u64::MAX))
}

// ---- the stage spill --------------------------------------------------------------------------------------------

/// The staged items of one operation: the first `cap` in memory, the rest appended to the stage spill.
pub struct StagedItems {
    mem: Vec<WidePath>,
    cap: usize,
    spilled: u64,
    spill: Option<Spill>,
}

struct Spill {
    rel: String,
    dir: OwnDir,
    out: Option<BufWriter<File>>,
}

impl StagedItems {
    /// Items of `op` staged in the host `pid`: at most `cap` in memory; with `dir` None nothing spills (every item
    /// stays in memory: tests and plans that are already bounded).
    pub fn new(dir: Option<&OwnDir>, op: OpId, pid: u32, cap: usize) -> StagedItems {
        let spill = dir.map(|d| {
            let rel = stage_rel(op, pid);
            Spill { rel, dir: d.clone(), out: None }
        });
        StagedItems { mem: Vec::new(), cap, spilled: 0, spill }
    }

    /// Items held in memory only.
    pub fn from_vec(items: Vec<WidePath>) -> StagedItems {
        StagedItems { cap: items.len(), mem: items, spilled: 0, spill: None }
    }

    pub fn push(&mut self, p: WidePath) -> io::Result<()> {
        let Some(spill) = self.spill.as_mut().filter(|_| self.mem.len() >= self.cap) else {
            self.mem.push(p);
            return Ok(());
        };
        if spill.out.is_none() {
            spill.out = Some(BufWriter::new(spill.dir.append(&spill.rel)?));
        }
        if let Some(out) = spill.out.as_mut() {
            let units = p.units();
            let n = u16::try_from(units.len()).map_err(|_| io::Error::other("path too long"))?;
            out.write_all(&n.to_le_bytes())?;
            let bytes: Vec<u8> = units.iter().flat_map(|u| u.to_le_bytes()).collect();
            out.write_all(&bytes)?;
        }
        self.spilled += 1;
        Ok(())
    }

    pub fn len(&self) -> u64 {
        self.mem.len() as u64 + self.spilled
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Items held in memory now.
    pub fn in_memory(&self) -> usize {
        self.mem.len()
    }

    /// The items held in memory (the first ones, in order).
    pub fn memory(&self) -> &[WidePath] {
        &self.mem
    }

    /// The spill file's path relative to the data folder, when there is one.
    pub fn spill_rel(&self) -> Option<&str> {
        self.spill.as_ref().filter(|_| self.spilled > 0).map(|s| s.rel.as_str())
    }

    /// Every item in order, `n` at a time: the memory part, then the spill read back from disk. The spill is
    /// written out first.
    pub fn chunks(&mut self, n: usize) -> io::Result<Chunks<'_>> {
        let mut reader = None;
        if let Some(spill) = self.spill.as_mut().filter(|_| self.spilled > 0) {
            if let Some(mut out) = spill.out.take() {
                out.flush()?;
            }
            reader = Some(SpillReader::open(&spill.dir, &spill.rel)?);
        }
        Ok(Chunks { mem: &self.mem, at: 0, n: n.max(1), spill: reader, left: self.spilled })
    }

    /// Removes the spill file (with the journal, or when the staged list is dropped).
    pub fn remove_spill(&mut self) {
        if let Some(spill) = self.spill.as_mut() {
            spill.out = None;
            if self.spilled > 0 {
                let _ = spill.dir.remove(&spill.rel);
            }
        }
        self.spilled = 0;
    }
}

/// The chunks of `StagedItems::chunks`.
pub struct Chunks<'a> {
    mem: &'a [WidePath],
    at: usize,
    n: usize,
    spill: Option<SpillReader>,
    left: u64,
}

impl Iterator for Chunks<'_> {
    type Item = io::Result<Vec<WidePath>>;

    fn next(&mut self) -> Option<Self::Item> {
        let mut out = Vec::new();
        while out.len() < self.n && self.at < self.mem.len() {
            out.extend(self.mem.get(self.at).cloned());
            self.at += 1;
        }
        while out.len() < self.n && self.left > 0 {
            let Some(r) = self.spill.as_mut() else { break };
            match r.next_path() {
                Ok(Some(p)) => out.push(p),
                Ok(None) => {
                    self.left = 0;
                    break;
                }
                Err(e) => {
                    self.left = 0;
                    return Some(Err(e));
                }
            }
            self.left -= 1;
        }
        if out.is_empty() {
            None
        } else {
            Some(Ok(out))
        }
    }
}

/// Reads a stage spill back, one path at a time.
pub struct SpillReader {
    r: BufReader<File>,
}

impl SpillReader {
    /// Opens the spill `rel` of the data folder with the checks the writer applies (OwnDir::open_read).
    pub fn open(dir: &OwnDir, rel: &str) -> io::Result<SpillReader> {
        Ok(SpillReader { r: BufReader::new(dir.open_read(rel)?) })
    }

    /// Like `open`, for a spill that must hold at most `count` paths: refused (InvalidData) when the opened file is
    /// longer than `count` paths of the longest size (a length prefix and MAX_PATH_UNITS units each), checked on
    /// the handle that is then read.
    pub fn open_expecting(dir: &OwnDir, rel: &str, count: u64) -> io::Result<SpillReader> {
        let f = dir.open_read(rel)?;
        let most = count.saturating_mul(2 + 2 * u64::from(MAX_PATH_UNITS));
        if f.metadata()?.len() > most {
            return Err(io::Error::new(io::ErrorKind::InvalidData, "stage spill: longer than its operation allows"));
        }
        Ok(SpillReader { r: BufReader::new(f) })
    }

    /// The next path; None at the end of the file; an error on a cut or invalid entry.
    pub fn next_path(&mut self) -> io::Result<Option<WidePath>> {
        let mut n = [0u8; 2];
        match self.r.read_exact(&mut n) {
            Ok(()) => {}
            Err(e) if e.kind() == io::ErrorKind::UnexpectedEof => return Ok(None),
            Err(e) => return Err(e),
        }
        let n = usize::from(u16::from_le_bytes(n));
        let mut bytes = vec![0u8; n * 2];
        self.r.read_exact(&mut bytes)?;
        let units: Vec<u16> =
            bytes.chunks_exact(2).map(|c| u16::from_le_bytes(<[u8; 2]>::try_from(c).unwrap_or_default())).collect();
        WidePath::new(&units).map(Some).map_err(|e| io::Error::other(format!("stage spill: {e:?}")))
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::indexing_slicing)]

    use super::*;
    use crate::testkit::TempRoot;

    fn header(op: u32) -> Header {
        Header {
            v: VERSION,
            op,
            kind: JKind::Copy,
            host_pid: 42,
            host_created: 133_000_000_000_000_000,
            dest: Some(PathRepr::Text("C:\\dst".into())),
            conflicts: JConflicts::Ask,
            items: 2,
            started: 133_000_000_000_000_001,
        }
    }

    fn rec(i: u32, src: &WidePath, st: St) -> Record {
        Record { i, src: PathRepr::of(src), dst: None, pre: Pre::Absent, st, size: 7, mtime: -3 }
    }

    #[test]
    fn round_trip_with_lone_surrogate() {
        let root = TempRoot::new("journal_rt");
        let dir = OwnDir::open(&root.dir("data")).unwrap();
        let plain = WidePath::parse("C:\\a\\b.txt").unwrap();
        let mut units: Vec<u16> = "C:\\a\\x".encode_utf16().collect();
        units.push(0xD800); // a lone high surrogate: not valid UTF-16, a valid NTFS name
        units.extend("y.txt".encode_utf16());
        let lone = WidePath::new(&units).unwrap();
        assert!(matches!(PathRepr::of(&lone), PathRepr::Wide { .. }));
        assert_eq!(PathRepr::of(&plain), PathRepr::Text("C:\\a\\b.txt".into()));
        let now = Instant::now();
        let mut w = JournalWriter::create(&dir, OpId(5), 42, &header(5), now).unwrap();
        w.record(&rec(0, &plain, St::Started), now).unwrap();
        w.record(&Record { dst: Some(PathRepr::of(&lone)), ..rec(1, &lone, St::Done) }, now).unwrap();
        w.end(EndKind::Done, now).unwrap();
        drop(w);
        let j = parse(&dir.read(&journal_rel(OpId(5), 42)).unwrap()).unwrap();
        assert_eq!(j.header, header(5));
        assert_eq!(j.records.len(), 2);
        assert_eq!(j.records[0].src.to_path().unwrap(), plain);
        assert_eq!(j.records[1].src.to_path().unwrap(), lone);
        assert_eq!(j.records[1].dst.as_ref().unwrap().to_path().unwrap(), lone);
        assert_eq!(j.records[1].mtime, -3);
        assert_eq!(j.end, Some(EndKind::Done));
        let text = String::from_utf8(dir.read(&journal_rel(OpId(5), 42)).unwrap()).unwrap();
        assert!(text.starts_with("{\"v\":1,\"op\":5,\"kind\":\"copy\""), "{text}");
        assert!(text.contains("{\"w\":\""), "{text}");
    }

    #[test]
    fn torn_last_line_ignored() {
        let p = WidePath::parse("C:\\a").unwrap();
        let mut s = serde_json::to_string(&header(1)).unwrap();
        s.push('\n');
        s.push_str(&serde_json::to_string(&rec(0, &p, St::Started)).unwrap());
        s.push('\n');
        let full = serde_json::to_string(&rec(0, &p, St::Done)).unwrap();
        let torn = format!("{s}{}", &full[..full.len() / 2]);
        let j = parse(torn.as_bytes()).unwrap();
        assert_eq!(j.records.len(), 1, "the torn record is ignored");
        assert_eq!(j.records[0].st, St::Started);
        assert_eq!(j.end, None);
        let whole_unterminated = format!("{s}{full}");
        assert_eq!(parse(whole_unterminated.as_bytes()).unwrap().records.len(), 2, "a whole last line counts");
        let bad_middle = format!("{s}{{not json\n{full}\n");
        assert_eq!(parse(bad_middle.as_bytes()), Err(JournalError::BadLine(3)));
        assert_eq!(parse(b""), Err(JournalError::NoHeader));
        assert_eq!(parse(b"{\"v\":1,\"op\""), Err(JournalError::NoHeader), "a torn header");
        let other = serde_json::to_string(&Header { v: 2, ..header(1) }).unwrap();
        assert_eq!(parse(other.as_bytes()), Err(JournalError::Version(2)));
        let latest = parse(format!("{s}{full}\n").as_bytes()).unwrap();
        assert_eq!(latest.latest().iter().map(|r| r.st).collect::<Vec<_>>(), [St::Done]);
    }

    #[test]
    fn summary_reads_as_parse() {
        let p = WidePath::parse("C:\\a").unwrap();
        let h = format!("{}\n", serde_json::to_string(&header(1)).unwrap());
        let r = |i: u32, st: St| format!("{}\n", serde_json::to_string(&rec(i, &p, st)).unwrap());
        let end = format!("{}\n", serde_json::to_string(&EndLine { end: EndKind::Done }).unwrap());
        let torn = r(1, St::Done);
        let cases: Vec<String> = vec![
            format!("{h}{}{}{}{}", r(0, St::Queued), r(1, St::Queued), r(0, St::Started), r(0, St::Done)),
            format!("{h}{}{}{}", r(0, St::Done), r(1, St::Skipped), r(0, St::Started)),
            format!("{h}{}{end}", r(1, St::Done)),
            format!("{h}{}{}", r(0, St::Done), &torn[..torn.len() / 2]),
            format!("{h}{{not json\n{}", r(0, St::Done)),
            String::new(),
            format!("{h}\r\n\n{}", r(0, St::Done).replace('\n', "\r\n")),
        ];
        for text in &cases {
            let want = parse(text.as_bytes()).ok().map(|j| Summary {
                done: j.latest().iter().filter(|r| matches!(r.st, St::Done | St::Skipped)).count() as u32,
                ended: j.end.is_some(),
                header: j.header,
            });
            assert_eq!(summary(&mut text.as_bytes(), |_| true), want, "{text:?}");
        }
        let first = summary(&mut cases[0].as_bytes(), |_| true).unwrap();
        assert_eq!((first.done, first.ended), (1, false));
        assert_eq!(summary(&mut cases[1].as_bytes(), |_| true).unwrap().done, 1, "the latest record counts");
        assert!(summary(&mut cases[2].as_bytes(), |_| true).unwrap().ended);
        assert_eq!(summary(&mut cases[4].as_bytes(), |_| true), None, "a bad line that is not the last");
        assert_eq!(summary(&mut cases[0].as_bytes(), |h| h.host_pid != 42), None, "not wanted: no further read");
        let other = format!("{}\n", serde_json::to_string(&Header { v: 2, ..header(1) }).unwrap());
        assert_eq!(summary(&mut other.as_bytes(), |_| true), None, "another version");
        let root = TempRoot::new("journal_summary");
        let dir = OwnDir::open(&root.dir("data")).unwrap();
        let now = Instant::now();
        let mut w = JournalWriter::create(&dir, OpId(3), 42, &header(3), now).unwrap();
        w.record(&rec(1, &p, St::Done), now).unwrap();
        drop(w);
        let s = summary(&mut reader(&dir, &journal_rel(OpId(3), 42)).unwrap(), |_| true).unwrap();
        assert_eq!((s.header, s.ended, s.done), (header(3), false, 1), "read from the data folder");
        assert!(reader(&dir, "ops").is_err(), "a folder is not read");
    }

    #[test]
    fn flush_rule() {
        let t0 = Instant::now();
        let mut rule = FlushRule::new(t0);
        for i in 1..FLUSH_RECORDS {
            assert!(!rule.wrote(t0 + Duration::from_millis(u64::from(i))), "record {i}: no flush yet");
        }
        assert!(rule.wrote(t0 + Duration::from_millis(64)), "the 64th record flushes");
        let t1 = t0 + Duration::from_millis(64);
        assert!(!rule.wrote(t1 + Duration::from_millis(249)));
        assert!(rule.wrote(t1 + Duration::from_millis(250)), "250 ms after the last flush");
        // The writer follows the rule: the header flush, then one per 64 records, then the end.
        let root = TempRoot::new("journal_flush");
        let dir = OwnDir::open(&root.dir("data")).unwrap();
        let p = WidePath::parse("C:\\a").unwrap();
        let mut w = JournalWriter::create(&dir, OpId(1), 7, &header(1), t0).unwrap();
        assert_eq!(w.flushes(), 1, "after the header");
        for _ in 0..63 {
            w.record(&rec(0, &p, St::Started), t0).unwrap();
        }
        assert_eq!(w.flushes(), 1, "63 records in the same instant: no flush");
        w.record(&rec(0, &p, St::Started), t0).unwrap();
        assert_eq!(w.flushes(), 2, "the 64th flushes");
        w.record(&rec(0, &p, St::Started), t0 + Duration::from_millis(300)).unwrap();
        assert_eq!(w.flushes(), 3, "250 ms flushes");
        flush_all();
        w.end(EndKind::Cancelled, t0).unwrap();
        assert_eq!(w.flushes(), 4);
        assert!(OPEN.lock().unwrap().iter().any(|(id, _)| *id == w.id));
        let id = w.id;
        drop(w);
        assert!(!OPEN.lock().unwrap().iter().any(|(i, _)| *i == id), "a closed journal leaves the flush list");
    }

    #[test]
    fn stage_spill_round_trip() {
        let root = TempRoot::new("journal_stage");
        let dir = OwnDir::open(&root.dir("data")).unwrap();
        let mut s = StagedItems::new(Some(&dir), OpId(9), 77, 4_096);
        let path = |i: u32| WidePath::parse(&format!("C:\\src\\f{i:05}.bin")).unwrap();
        for i in 0..10_000 {
            s.push(path(i)).unwrap();
            assert!(s.in_memory() <= 4_096);
        }
        assert_eq!(s.len(), 10_000);
        assert_eq!(s.in_memory(), 4_096);
        assert_eq!(s.spill_rel(), Some("ops\\9-77.stage"));
        let mut seen = 0u32;
        for chunk in s.chunks(64).unwrap() {
            let chunk = chunk.unwrap();
            assert!(chunk.len() <= 64);
            for p in chunk {
                assert_eq!(p, path(seen));
                seen += 1;
            }
        }
        assert_eq!(seen, 10_000, "all read back in order");
        s.remove_spill();
        assert!(dir.list("ops").unwrap().is_empty(), "the spill is removed");
        let mut small = StagedItems::new(Some(&dir), OpId(10), 77, 4_096);
        small.push(path(1)).unwrap();
        assert_eq!(small.spill_rel(), None, "nothing spills under the cap");
        assert_eq!(small.chunks(64).unwrap().map(|c| c.unwrap().len()).sum::<usize>(), 1);
    }

    #[test]
    fn spill_reader_refuses_a_planted_link() {
        let root = TempRoot::new("journal_spill_link");
        let real = TempRoot::new("journal_spill_target");
        let dir = OwnDir::open(&root.dir("data")).unwrap();
        let other = OwnDir::open(&real.dir("data")).unwrap();
        let mut s = StagedItems::new(Some(&other), OpId(9), 77, 0);
        s.push(WidePath::parse("C:\\src\\planted.bin").unwrap()).unwrap();
        s.chunks(8).unwrap().for_each(drop);
        let rel = stage_rel(OpId(9), 77);
        assert!(SpillReader::open(&other, &rel).is_ok(), "the real spill reads");
        // The data folder's ops folder is a junction to the folder holding another spill.
        root.junction("data\\ops", &real.path().join("data\\ops"));
        assert!(SpillReader::open(&dir, &rel).is_err(), "a spill behind a link is not read");
    }

    #[test]
    fn spill_reader_refuses_a_file_longer_than_its_count() {
        let root = TempRoot::new("journal_spill_long");
        let dir = OwnDir::open(&root.dir("data")).unwrap();
        let mut s = StagedItems::new(Some(&dir), OpId(9), 78, 0);
        for k in 0..3 {
            s.push(WidePath::parse(&format!("C:\\src\\{k}.bin")).unwrap()).unwrap();
        }
        s.chunks(8).unwrap().for_each(drop);
        let rel = stage_rel(OpId(9), 78);
        assert!(SpillReader::open_expecting(&dir, &rel, 3).is_ok());
        // One path of the longest size fits 1 + 2 * MAX_PATH_UNITS bytes; three short ones are far below it.
        assert!(SpillReader::open_expecting(&dir, &rel, 1).is_ok(), "short entries are under the byte bound");
        let big = vec![0u8; usize::from(MAX_PATH_UNITS) * 2 + 3];
        dir.write_atomic("ops\\big.stage", &big).unwrap();
        assert!(SpillReader::open_expecting(&dir, "ops\\big.stage", 1).is_err(), "more bytes than one path can take");
        assert!(SpillReader::open_expecting(&dir, "ops\\big.stage", 2).is_ok());
        assert!(SpillReader::open_expecting(&dir, "ops\\big.stage", 0).is_err(), "no path expected, a file is there");
    }

    #[test]
    fn names_parse() {
        assert_eq!(parse_name("12-3456.jsonl", "jsonl"), Some((OpId(12), 3456)));
        assert_eq!(parse_name("12-3456.stage", "stage"), Some((OpId(12), 3456)));
        for bad in ["12-3456.json", "x-1.jsonl", "12.jsonl", "-1.jsonl", "1-.jsonl", "1-2-3.jsonl", "ops-done.json"] {
            assert_eq!(parse_name(bad, "jsonl"), None, "{bad}");
        }
        assert_eq!(journal_rel(OpId(3), 9), "ops\\3-9.jsonl");
    }

    #[test]
    fn kinds_round_trip() {
        for k in [
            OpKind::Copy,
            OpKind::Move,
            OpKind::Recycle,
            OpKind::DeletePermanent,
            OpKind::Rename,
            OpKind::NewFolder,
            OpKind::Undo,
            OpKind::Resume,
        ] {
            assert_eq!(OpKind::from(JKind::from(k)), k);
        }
        for c in [Conflicts::AskSystem, Conflicts::ReplaceAll, Conflicts::SkipAll] {
            assert_eq!(Conflicts::from(JConflicts::from(c)), c);
        }
        assert_eq!(serde_json::to_string(&JKind::DeletePermanent).unwrap(), "\"delete_permanent\"");
        assert!(now_filetime() > 133_000_000_000_000_000, "after 2022");
    }
}
