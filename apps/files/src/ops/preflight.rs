//! The pre-flight of a chunk (OP3): what each top-level item's destination holds before the operation, read with
//! listing::stat_one (read only, never following a reparse point). It gives the journal its "pre" values, the
//! collision count op_flags takes and the items SkipAll removes; an item whose parent is the destination (a copy into
//! its own folder, which the shell names "name - Copy") is Absent.

use oled_trust::frame::files::{OpKind, WidePath};
use windows::Win32::Storage::FileSystem::{FILE_ATTRIBUTE_DIRECTORY, FILE_ATTRIBUTE_REPARSE_POINT};

use super::checks::same_path;
use super::journal::Pre;
use crate::listing::stat_one;

/// The pre-flight of one chunk.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Preflight {
    pub pre: Vec<Pre>,
    /// Items whose destination exists (File, Dir or Reparse).
    pub collisions: u32,
}

/// The name item `i` gets at its destination: its own, or `names[i]` when the plan renames it.
pub fn target_name(item: &WidePath, names: &[Box<str>], i: usize) -> Option<Vec<u16>> {
    match names.get(i) {
        Some(n) => Some(n.encode_utf16().collect()),
        None => item.file_name().map(<[u16]>::to_vec),
    }
}

/// What the destination of each item holds now. Kinds other than Copy and Move have no destination to compare:
/// every item is Absent. A destination that cannot be read counts as present (it is never replaced silently).
pub fn preflight(kind: OpKind, items: &[WidePath], names: &[Box<str>], dest: Option<&WidePath>) -> Preflight {
    let mut pre = Vec::with_capacity(items.len());
    let Some(dest) = dest.filter(|_| matches!(kind, OpKind::Copy | OpKind::Move)) else {
        return Preflight { pre: vec![Pre::Absent; items.len()], collisions: 0 };
    };
    for (i, item) in items.iter().enumerate() {
        let into_own_folder = names.is_empty() && item.parent().is_some_and(|p| same_path(&p, dest));
        let state = match target_name(item, names, i) {
            _ if into_own_folder => Pre::Absent,
            None => Pre::File,
            Some(name) => match stat_one(dest, &wtf8(&name)) {
                Ok(None) => Pre::Absent,
                Ok(Some(e)) if e.e.attrs & FILE_ATTRIBUTE_REPARSE_POINT.0 != 0 => Pre::Reparse,
                Ok(Some(e)) if e.e.attrs & FILE_ATTRIBUTE_DIRECTORY.0 != 0 => Pre::Dir,
                Ok(Some(_)) | Err(_) => Pre::File,
            },
        };
        pre.push(state);
    }
    let collisions = pre.iter().filter(|p| **p != Pre::Absent).count() as u32;
    Preflight { pre, collisions }
}

/// The indices SkipAll keeps: the items whose destination is absent.
pub fn skip_present(pre: &[Pre]) -> Vec<usize> {
    pre.iter().enumerate().filter(|(_, p)| **p == Pre::Absent).map(|(i, _)| i).collect()
}

/// UTF-16 units as WTF-8 (a lone surrogate as its three-byte form), the listing's name encoding.
pub fn wtf8(units: &[u16]) -> Vec<u8> {
    let mut out = Vec::with_capacity(units.len());
    for r in char::decode_utf16(units.iter().copied()) {
        match r {
            Ok(c) => {
                let mut b = [0u8; 4];
                out.extend_from_slice(c.encode_utf8(&mut b).as_bytes());
            }
            Err(e) => {
                let u = e.unpaired_surrogate();
                out.extend_from_slice(&[
                    0xE0 | (u >> 12) as u8,
                    0x80 | ((u >> 6) & 0x3F) as u8,
                    0x80 | (u & 0x3F) as u8,
                ]);
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wtf8_matches_utf8_and_keeps_lone_surrogates() {
        let s = "aé€😀";
        assert_eq!(wtf8(&s.encode_utf16().collect::<Vec<_>>()), s.as_bytes());
        assert_eq!(wtf8(&[0x61, 0xD800, 0x62]), [0x61, 0xED, 0xA0, 0x80, 0x62]);
    }

    #[test]
    fn other_kinds_have_no_destination() {
        let items = [WidePath::parse("C:\\a\\x").unwrap()];
        let d = WidePath::parse("C:\\b").unwrap();
        for kind in [OpKind::Recycle, OpKind::DeletePermanent, OpKind::Rename, OpKind::NewFolder] {
            assert_eq!(preflight(kind, &items, &[], Some(&d)), Preflight { pre: vec![Pre::Absent], collisions: 0 });
        }
        assert_eq!(preflight(OpKind::Copy, &items, &[], None).collisions, 0);
        assert_eq!(skip_present(&[Pre::Absent, Pre::File, Pre::Absent, Pre::Reparse, Pre::Dir]), [0, 2]);
        let renamed: [Box<str>; 1] = ["y".into()];
        assert_eq!(target_name(&items[0], &renamed, 0), Some(vec![u16::from(b'y')]));
        assert_eq!(target_name(&items[0], &[], 0), Some(vec![u16::from(b'x')]));
    }
}
