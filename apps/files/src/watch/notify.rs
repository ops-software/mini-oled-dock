//! The FILE_NOTIFY_INFORMATION record parser (pure): a ReadDirectoryChangesW buffer in, one (action, name) pair per
//! record out. The buffer comes from the kernel but is checked as if it did not: every offset is validated before it
//! is followed, so a damaged buffer is an error, never a panic or a read outside the slice.

use super::ChangeAction;

/// Why a buffer was refused.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NotifyError {
    /// A record header (12 bytes) does not fit in the buffer.
    Truncated,
    /// A record offset is not a multiple of 4.
    Misaligned,
    /// NextEntryOffset does not move forward past the record, or points outside the buffer.
    BadNext,
    /// FileNameLength is odd, or the name runs past the end of the buffer.
    BadName,
    /// A name holds a backslash: a recursive watch nobody asked for.
    Backslash,
}

const HEADER: usize = 12;
const BACKSLASH: u16 = b'\\' as u16;

fn u32_at(buf: &[u8], at: usize) -> Option<u32> {
    let b = buf.get(at..at.checked_add(4)?)?;
    Some(u32::from_le_bytes([b[0], b[1], b[2], b[3]]))
}

fn action_of(code: u32) -> Option<ChangeAction> {
    match code {
        1 => Some(ChangeAction::Added),
        2 => Some(ChangeAction::Removed),
        3 => Some(ChangeAction::Modified),
        4 => Some(ChangeAction::RenamedFrom),
        5 => Some(ChangeAction::RenamedTo),
        _ => None,
    }
}

/// The records of `buf` in order. An empty buffer holds no record. A record with an unknown action is
/// skipped (its framing is still checked).
pub fn parse(buf: &[u8]) -> Result<Vec<(ChangeAction, Vec<u16>)>, NotifyError> {
    let mut out = Vec::new();
    if buf.is_empty() {
        return Ok(out);
    }
    let mut at = 0usize;
    loop {
        if !at.is_multiple_of(4) {
            return Err(NotifyError::Misaligned);
        }
        let (Some(next), Some(action), Some(name_len)) = (u32_at(buf, at), u32_at(buf, at + 4), u32_at(buf, at + 8))
        else {
            return Err(NotifyError::Truncated);
        };
        let name_len = name_len as usize;
        if !name_len.is_multiple_of(2) {
            return Err(NotifyError::BadName);
        }
        let name_start = at + HEADER;
        let name_end = name_start.checked_add(name_len).filter(|&e| e <= buf.len()).ok_or(NotifyError::BadName)?;
        let name: Vec<u16> =
            buf[name_start..name_end].chunks_exact(2).map(|c| u16::from_le_bytes([c[0], c[1]])).collect();
        if name.contains(&BACKSLASH) {
            return Err(NotifyError::Backslash);
        }
        if let Some(a) = action_of(action) {
            out.push((a, name));
        }
        if next == 0 {
            return Ok(out);
        }
        let next = next as usize;
        if !next.is_multiple_of(4) {
            return Err(NotifyError::Misaligned);
        }
        // Forward past this record's own bytes, and the next header starts inside the buffer.
        if next < HEADER + name_len {
            return Err(NotifyError::BadNext);
        }
        at = at.checked_add(next).filter(|&n| n < buf.len()).ok_or(NotifyError::BadNext)?;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// One record; `next` 0 = last, else the aligned size of the record.
    fn rec(action: u32, name: &str, last: bool) -> Vec<u8> {
        let units: Vec<u16> = name.encode_utf16().collect();
        let len = units.len() * 2;
        let size = (HEADER + len + 3) & !3;
        let mut v = Vec::new();
        v.extend_from_slice(&(if last { 0u32 } else { size as u32 }).to_le_bytes());
        v.extend_from_slice(&action.to_le_bytes());
        v.extend_from_slice(&(len as u32).to_le_bytes());
        for u in units {
            v.extend_from_slice(&u.to_le_bytes());
        }
        v.resize(size, 0);
        v
    }

    fn wide(s: &str) -> Vec<u16> {
        s.encode_utf16().collect()
    }

    #[test]
    fn empty_buffer_has_no_records() {
        assert_eq!(parse(&[]), Ok(vec![]));
    }

    #[test]
    fn parses_a_chain() {
        let mut b = rec(1, "a.txt", false);
        b.extend(rec(4, "old", false));
        b.extend(rec(5, "néw", true));
        let got = parse(&b).unwrap();
        assert_eq!(
            got,
            vec![
                (ChangeAction::Added, wide("a.txt")),
                (ChangeAction::RenamedFrom, wide("old")),
                (ChangeAction::RenamedTo, wide("néw")),
            ]
        );
    }

    #[test]
    fn all_actions_map() {
        for (code, want) in [
            (1, ChangeAction::Added),
            (2, ChangeAction::Removed),
            (3, ChangeAction::Modified),
            (4, ChangeAction::RenamedFrom),
            (5, ChangeAction::RenamedTo),
        ] {
            assert_eq!(parse(&rec(code, "x", true)).unwrap()[0].0, want);
        }
    }

    #[test]
    fn unknown_action_is_skipped() {
        let mut b = rec(1, "a", false);
        b.extend(rec(9, "weird", false));
        b.extend(rec(0, "zero", false));
        b.extend(rec(2, "b", true));
        let got = parse(&b).unwrap();
        assert_eq!(got, vec![(ChangeAction::Added, wide("a")), (ChangeAction::Removed, wide("b"))]);
    }

    #[test]
    fn lone_surrogate_name_is_kept() {
        let mut b = rec(1, "ab", true);
        b[12] = 0x00;
        b[13] = 0xD8; // a lone high surrogate
        assert_eq!(parse(&b).unwrap()[0].1, vec![0xD800, u16::from(b'b')]);
    }

    #[test]
    fn backslash_is_refused() {
        assert_eq!(parse(&rec(1, "sub\\file", true)), Err(NotifyError::Backslash));
    }

    #[test]
    fn bad_framing_is_refused() {
        let good = {
            let mut b = rec(1, "abc", false);
            b.extend(rec(1, "def", true));
            b
        };
        assert!(parse(&good).is_ok());
        // Truncated header.
        assert_eq!(parse(&good[..8]), Err(NotifyError::Truncated));
        // Name runs past the end.
        assert_eq!(parse(&good[..14]), Err(NotifyError::BadName));
        // Odd FileNameLength.
        let mut odd = good.clone();
        odd[8] = 5;
        assert_eq!(parse(&odd), Err(NotifyError::BadName));
        // Misaligned next offset.
        let mut mis = good.clone();
        mis[0] = 18;
        assert_eq!(parse(&mis), Err(NotifyError::Misaligned));
        // Next offset past the end of the buffer.
        let mut far = good.clone();
        far[0] = 0x80;
        assert_eq!(parse(&far), Err(NotifyError::BadNext));
        // Next offset that does not move forward past the record.
        let mut back = good.clone();
        back[0] = 4;
        assert_eq!(parse(&back), Err(NotifyError::BadNext));
        // A next offset that points exactly at the end: no room for a header.
        let mut end = good.clone();
        end[0] = good.len() as u8;
        assert_eq!(parse(&end), Err(NotifyError::BadNext));
        // FileNameLength of u32::MAX does not overflow.
        let mut huge = good.clone();
        huge[8..12].copy_from_slice(&u32::MAX.to_le_bytes());
        assert!(parse(&huge).is_err());
    }

    /// 100,000 seeded inputs, mutated records and raw noise: no panic, and whatever parses is well formed.
    #[test]
    fn fuzz_notify() {
        let mut s: u64 = 0x9E37_79B9_7F4A_7C15;
        let mut next = move || {
            s ^= s << 13;
            s ^= s >> 7;
            s ^= s << 17;
            s
        };
        let mut ok = 0u32;
        for i in 0..100_000u32 {
            let mut b = Vec::new();
            for r in 0..(next() % 4) {
                let name = ["a", "file.txt", "x y", "é"][(next() % 4) as usize];
                b.extend(rec((next() % 7) as u32, name, r == 3 || next() % 3 == 0));
            }
            if i % 4 == 0 {
                b = (0..next() % 64).map(|_| next() as u8).collect();
            }
            for _ in 0..(next() % 4) {
                if !b.is_empty() {
                    let at = (next() as usize) % b.len();
                    b[at] = next() as u8;
                }
            }
            if let Ok(v) = parse(&b) {
                ok += 1;
                assert!(v.iter().all(|(_, n)| !n.contains(&BACKSLASH)));
            }
        }
        assert!(ok > 1_000, "the fuzz must reach the Ok path too ({ok})");
    }
}
