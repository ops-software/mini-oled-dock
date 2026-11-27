//! Drive letters: a snapshot (GetLogicalDrives plus the type of each letter) and the pure diff of two snapshots. The
//! shell's drive notifications and the UI's WM_DEVICECHANGE both only say "something changed": the diff of what is
//! there now against what was there before says what.

use windows::core::PCWSTR;
use windows::Win32::Storage::FileSystem::{GetDriveTypeW, GetLogicalDrives};

use super::DriveDiff;

/// The drives present (bit n = letter A + n) and the type of each (GetDriveTypeW's value, 0 when absent).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Snapshot {
    pub present: u32,
    pub types: [u8; 26],
}

impl Snapshot {
    /// From a letter mask and a function giving the type of a present letter (the pure part of snapshot()).
    pub fn from_parts(present: u32, type_of: impl Fn(u8) -> u8) -> Self {
        let present = present & 0x03FF_FFFF;
        let mut types = [0u8; 26];
        for (n, t) in types.iter_mut().enumerate() {
            if present & (1 << n) != 0 {
                *t = type_of(b'A' + n as u8);
            }
        }
        Snapshot { present, types }
    }
}

/// The drives now.
pub fn snapshot() -> Snapshot {
    // SAFETY: no arguments.
    let mask = unsafe { GetLogicalDrives() };
    Snapshot::from_parts(mask, |letter| {
        let root: [u16; 4] = [u16::from(letter), u16::from(b':'), u16::from(b'\\'), 0];
        // SAFETY: a NUL-terminated root path that lives for the call.
        unsafe { GetDriveTypeW(PCWSTR(root.as_ptr())) as u8 }
    })
}

/// What changed from `old` to `new`: a letter that appeared is added, one that vanished is removed, one whose type
/// changed (a stick replaced by a network share on the same letter) is both.
pub fn diff(old: &Snapshot, new: &Snapshot) -> DriveDiff {
    let mut changed = 0u32;
    for n in 0..26 {
        if old.present & new.present & (1 << n) != 0 && old.types[n] != new.types[n] {
            changed |= 1 << n;
        }
    }
    DriveDiff { added: (new.present & !old.present) | changed, removed: (old.present & !new.present) | changed }
}

#[cfg(test)]
mod tests {
    use super::*;

    const fn bit(letter: u8) -> u32 {
        1 << (letter - b'A')
    }

    fn snap(letters: &[(u8, u8)]) -> Snapshot {
        let mask = letters.iter().fold(0, |m, &(l, _)| m | bit(l));
        Snapshot::from_parts(mask, |l| letters.iter().find(|&&(x, _)| x == l).map_or(0, |&(_, t)| t))
    }

    #[test]
    fn snapshot_diff() {
        let none = snap(&[]);
        let c = snap(&[(b'C', 3)]);
        let cde = snap(&[(b'C', 3), (b'D', 5), (b'E', 2)]);
        assert_eq!(diff(&c, &c), DriveDiff::default());
        assert_eq!(diff(&none, &none), DriveDiff::default());
        assert_eq!(diff(&c, &cde), DriveDiff { added: bit(b'D') | bit(b'E'), removed: 0 });
        assert_eq!(diff(&cde, &c), DriveDiff { added: 0, removed: bit(b'D') | bit(b'E') });
        // A letter reused by another kind of drive is both.
        let c_as_net = snap(&[(b'C', 4)]);
        assert_eq!(diff(&c, &c_as_net), DriveDiff { added: bit(b'C'), removed: bit(b'C') });
        // Mixed.
        let d = snap(&[(b'D', 5)]);
        assert_eq!(diff(&c, &d), DriveDiff { added: bit(b'D'), removed: bit(b'C') });
        // Z is the last letter.
        let z = snap(&[(b'Z', 4)]);
        assert_eq!(diff(&none, &z).added, 1 << 25);
    }

    #[test]
    fn the_real_snapshot_has_a_system_drive() {
        let s = snapshot();
        assert_ne!(s.present, 0);
        assert_eq!(diff(&s, &s), DriveDiff::default());
        assert!((0..26).all(|n| s.present & (1 << n) != 0 || s.types[n] == 0), "an absent letter has no type");
    }
}
