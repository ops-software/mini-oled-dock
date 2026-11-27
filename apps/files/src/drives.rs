//! Drives (P3-files-app 2.2; F4.9 fills the scan): the process-wide cache of drive facts the lanes and the sidebar read.

use crate::bg::Lane;
use crate::window::Shared;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DriveInfo {
    /// 'A'..='Z'.
    pub letter: u8,
    pub label: Option<String>,
    pub kind: DriveKind,
    pub free: Option<u64>,
    pub total: Option<u64>,
    pub ready: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DriveKind {
    Fixed,
    Removable,
    Network,
    Optical,
    Ram,
    Unknown,
}

/// The drives seen so far, by letter.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct DriveCache {
    drives: Vec<DriveInfo>,
}

impl DriveCache {
    pub fn new() -> DriveCache {
        DriveCache { drives: Vec::new() }
    }

    /// The lane of a letter (any case): Network for a network drive, Local for any other known drive.
    pub fn lane(&self, letter: u8) -> Option<Lane> {
        let l = letter.to_ascii_uppercase();
        self.drives.iter().find(|d| d.letter == l).map(|d| {
            if d.kind == DriveKind::Network {
                Lane::Network
            } else {
                Lane::Local
            }
        })
    }

    /// Adds or replaces the drive of d.letter; the list stays in letter order.
    pub fn set(&mut self, d: DriveInfo) {
        let d = DriveInfo { letter: d.letter.to_ascii_uppercase(), ..d };
        match self.drives.binary_search_by_key(&d.letter, |x| x.letter) {
            Ok(i) => {
                if let Some(slot) = self.drives.get_mut(i) {
                    *slot = d;
                }
            }
            Err(i) => self.drives.insert(i, d),
        }
    }

    pub fn all(&self) -> &[DriveInfo] {
        &self.drives
    }
}

/// GetLogicalDrives, then a Bg job per letter (win 0). Filled by FD-SIDE (F4.9); until then nothing.
pub fn scan(shared: &Shared) {
    let _ = shared;
}
