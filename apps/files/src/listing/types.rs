//! The listing's plain types (complete): ids, the 40-byte entry, batches from the enumerator, its errors and the
//! cancel token shared with a running listing.

use std::sync::atomic::{AtomicBool, Ordering};

use super::Gen;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct EntryId(pub u32); // chunk * 4,096 + slot (ST1)
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct TabId(pub u32);

/// 40 bytes (const assert). Names live in the store's WTF-8 arena; ext = interned lower-case extension id, 0 = none.
#[repr(C)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Entry {
    pub name_off: u32,
    pub name_len: u16,
    pub ext: u16,
    pub attrs: u32,
    pub reparse_tag: u32,
    pub size: u64,
    pub mtime: i64,
    pub ctime: i64,
}

const _: () = assert!(std::mem::size_of::<Entry>() == 40, "Entry is 40 bytes (the store's arithmetic)");

/// One entry of a batch; its name is Batch::names[name_off .. name_off + name_len] (WTF-8).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RawEntry {
    pub name_off: u32,
    pub name_len: u16,
    pub attrs: u32,
    pub reparse_tag: u32,
    pub size: u64,
    pub mtime: i64,
    pub ctime: i64,
}

pub struct Batch {
    pub gen: Gen,
    pub names: Vec<u8>,
    pub entries: Vec<RawEntry>,
    pub done: bool,
    pub error: Option<EnumError>,
    pub elapsed_us: u32,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EnumError {
    AccessDenied,
    NotFound,
    Offline { waited_ms: u32 },
    TooLong,
    Unsupported,
    Win32(u32),
}

/// Shared between the UI and a listing: cancel() is seen by every clone (Release on set, Acquire on read).
#[derive(Clone, Default, Debug)]
pub struct CancelToken(std::sync::Arc<std::sync::atomic::AtomicBool>);

impl CancelToken {
    pub fn new() -> Self {
        CancelToken(std::sync::Arc::new(AtomicBool::new(false)))
    }

    pub fn cancel(&self) {
        self.0.store(true, Ordering::Release);
    }

    pub fn is_cancelled(&self) -> bool {
        self.0.load(Ordering::Acquire)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cancel_token_is_shared() {
        let a = CancelToken::new();
        let b = a.clone();
        assert!(!a.is_cancelled() && !b.is_cancelled());
        let t = std::thread::spawn(move || {
            b.cancel();
            b
        });
        let b = t.join().unwrap();
        assert!(a.is_cancelled() && b.is_cancelled(), "every clone sees the cancel");
        assert!(!CancelToken::default().is_cancelled(), "a new token is not cancelled");
    }

    #[test]
    fn entry_is_40_bytes() {
        assert_eq!(std::mem::size_of::<Entry>(), 40);
    }
}
