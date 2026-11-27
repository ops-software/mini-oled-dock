//! Undo (OP4): check a record against the disk, then plan its inverse as ordinary chunks for the runner. The host
//! runs check again right before plan (the UI's copy of the record is not trusted for freshness). Entries are
//! top-level items (OP3), so an undo acts on whole top-level folders. No plan ever holds DeletePermanent: copies and
//! new folders go to the Recycle Bin, moves and renames go back, a recycle is restored or the bin opened. A restore
//! moves the item out of the bin, which leaves the bin's $I record of it behind: the runner then removes that record,
//! and only that one (binrec).

use std::collections::HashMap;

use oled_trust::frame::files::{Conflicts, OpId, OpKind, Refusal, UndoEntry, UndoKind, WidePath};
use windows::core::PCWSTR;
use windows::Win32::Storage::FileSystem::{
    FindClose, FindExInfoBasic, FindExSearchNameMatch, FindFirstFileExW, FindNextFileW, FIND_FIRST_EX_FLAGS,
    WIN32_FIND_DATAW,
};

use super::checks::folded;
use super::consts::{CHUNK_ITEMS, RESTORE_FROM_BIN};
use super::runner::Prepared;
use crate::ops_client::UndoRecord;

/// What check reads from the disk.
pub trait UndoFs {
    /// Size, last-write time and folder flag of `p`; None when it does not exist.
    fn stat(&self, p: &WidePath) -> Option<(u64, i64, bool)>;
    /// `p` is a folder with nothing in it.
    fn is_empty_dir(&self, p: &WidePath) -> bool;
}

/// The real disk (reads only).
pub struct OsFs;

impl UndoFs for OsFs {
    fn stat(&self, p: &WidePath) -> Option<(u64, i64, bool)> {
        super::sink::attrs(p)
    }

    fn is_empty_dir(&self, p: &WidePath) -> bool {
        let mut pattern = p.raw();
        pattern.pop();
        if pattern.last() != Some(&u16::from(b'\\')) {
            pattern.push(u16::from(b'\\'));
        }
        pattern.extend([u16::from(b'*'), 0]);
        let mut fd = WIN32_FIND_DATAW::default();
        // SAFETY: `pattern` is NUL-terminated; `fd` is the record FindExInfoBasic fills.
        let found = unsafe {
            FindFirstFileExW(
                PCWSTR(pattern.as_ptr()),
                FindExInfoBasic,
                std::ptr::from_mut(&mut fd).cast(),
                FindExSearchNameMatch,
                None,
                FIND_FIRST_EX_FLAGS(0),
            )
        };
        let Ok(h) = found else { return false };
        let mut empty = true;
        loop {
            let name = fd.cFileName;
            let n = name.iter().position(|&u| u == 0).unwrap_or(name.len());
            let dots = matches!(name.get(..n), Some([0x2E]) | Some([0x2E, 0x2E]));
            if !dots {
                empty = false;
                break;
            }
            // SAFETY: the find handle opened above; `fd` is writable.
            if unsafe { FindNextFileW(h, &mut fd) }.is_err() {
                break;
            }
        }
        // SAFETY: the find handle opened above, closed once.
        let _ = unsafe { FindClose(h) };
        empty
    }
}

/// A refusal with its detail (a name or a count, never a path list).
pub type Refused = (Refusal, Box<str>);

fn name_of(p: &WidePath) -> Box<str> {
    p.file_name().map(String::from_utf16_lossy).unwrap_or_else(|| p.to_string_lossy()).into()
}

/// Whether `rec` can be undone now, and as what: replaced older files refuse it (Replaced, detail = their count);
/// every entry's `to` must exist with its recorded size and mtime (folders: exist as folders), else Changed (its
/// name); a new folder must still be empty (NotEmpty). A RestoreRecycled record with RESTORE_FROM_BIN false comes
/// back as OpenRecycleBin (no operation).
pub fn check(rec: &UndoRecord, fs: &dyn UndoFs) -> Result<UndoKind, Refused> {
    match rec.kind {
        UndoKind::OpenRecycleBin => return Ok(UndoKind::OpenRecycleBin),
        UndoKind::RestoreRecycled if !RESTORE_FROM_BIN => return Ok(UndoKind::OpenRecycleBin),
        _ => {}
    }
    if rec.replaced > 0 {
        return Err((Refusal::Replaced, rec.replaced.to_string().into()));
    }
    if rec.entries.is_empty() {
        return Err((Refusal::NoItems, "".into()));
    }
    for e in &rec.entries {
        let at = if rec.kind == UndoKind::RestoreRecycled { e.bin.as_ref().unwrap_or(&e.to) } else { &e.to };
        let same = match fs.stat(at) {
            None => false,
            Some((_, _, dir)) if e.dir => dir,
            Some((size, mtime, dir)) => !dir && size == e.size && mtime == e.mtime,
        };
        if !same {
            return Err((Refusal::Changed, name_of(&e.to)));
        }
        if rec.kind == UndoKind::RecycleNewFolder && !fs.is_empty_dir(&e.to) {
            return Err((Refusal::NotEmpty, name_of(&e.to)));
        }
    }
    Ok(rec.kind)
}

/// The inverse operation of `rec` as chunks of at most CHUNK_ITEMS items, under the new operation `op`.
pub fn plan(op: OpId, rec: &UndoRecord) -> Vec<Prepared> {
    let chunk = |kind: OpKind, items: Vec<WidePath>, dest: Option<WidePath>, names: Vec<Box<str>>| Prepared {
        op,
        kind,
        items,
        dest,
        name: None,
        conflicts: Conflicts::AskSystem,
        pre: Vec::new(),
        names,
    };
    let from_name = |e: &UndoEntry| -> Box<str> { name_of(&e.from) };
    let mut out = Vec::new();
    match rec.kind {
        UndoKind::RenameBack => {
            for part in rec.entries.chunks(CHUNK_ITEMS) {
                let items = part.iter().map(|e| e.to.clone()).collect();
                out.push(chunk(OpKind::Rename, items, None, part.iter().map(from_name).collect()));
            }
        }
        UndoKind::MoveBack | UndoKind::RestoreRecycled => {
            let source = |e: &UndoEntry| -> WidePath {
                if rec.kind == UndoKind::RestoreRecycled {
                    e.bin.clone().unwrap_or_else(|| e.to.clone())
                } else {
                    e.to.clone()
                }
            };
            for (parent, group) in by_parent(&rec.entries) {
                for part in group.chunks(CHUNK_ITEMS) {
                    let items = part.iter().map(|e| source(e)).collect();
                    let names = part.iter().map(|e| from_name(e)).collect();
                    out.push(chunk(OpKind::Move, items, Some(parent.clone()), names));
                }
            }
        }
        UndoKind::RecycleCopies | UndoKind::RecycleNewFolder => {
            for part in rec.entries.chunks(CHUNK_ITEMS) {
                out.push(chunk(OpKind::Recycle, part.iter().map(|e| e.to.clone()).collect(), None, Vec::new()));
            }
        }
        UndoKind::OpenRecycleBin => {}
    }
    out
}

/// The entries grouped by the parent folder of their `from`, in the order the parents first appear.
fn by_parent(entries: &[UndoEntry]) -> Vec<(WidePath, Vec<&UndoEntry>)> {
    let mut order: Vec<(WidePath, Vec<&UndoEntry>)> = Vec::new();
    let mut at: HashMap<Vec<u16>, usize> = HashMap::new();
    for e in entries {
        let Some(parent) = e.from.parent() else { continue };
        let key = folded(parent.units());
        match at.get(&key) {
            Some(&i) => {
                if let Some((_, g)) = order.get_mut(i) {
                    g.push(e);
                }
            }
            None => {
                at.insert(key, order.len());
                order.push((parent, vec![e]));
            }
        }
    }
    order
}
