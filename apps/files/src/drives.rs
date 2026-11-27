//! Drives (P3-files-app 2.2, F4.9): the process-wide cache of drive facts the lanes, the sidebar and Home read, and the
//! scan that fills it: the letters at once (the type of each is a cheap local call), then one Bg job per letter for
//! its label and free space, each inside a budget so a stuck volume shows its letter and nothing else.

use std::sync::mpsc;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use windows::core::PCWSTR;
use windows::Win32::Storage::FileSystem::{GetDiskFreeSpaceExW, GetDriveTypeW, GetVolumeInformationW};
use windows::Win32::System::Diagnostics::Debug::{SetThreadErrorMode, SEM_FAILCRITICALERRORS};

use crate::bg::Lane;
use crate::msg::{Fwd, JobDone, Msg};
use crate::watch::drives::snapshot;
use crate::window::Shared;

/// A drive that has not answered with its size within this long shows its letter only (SPEC-files 5.5.7).
pub const FACTS_BUDGET_MS: u64 = 1_500;
/// A network drive's label is given up after this long: the letter alone.
pub const NET_LABEL_BUDGET_MS: u64 = 200;

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

/// What the scan asks of the system, one seam so a test can answer for made-up drives. Every method may block, except
/// present and kind, which are local and cheap; the facts are asked from Bg lanes only.
pub trait DriveApi: Send + Sync {
    /// True for an answer that never waits: the scan then fills the cache at once and runs no job.
    fn instant(&self) -> bool {
        false
    }
    /// GetLogicalDrives: bit n is the letter A + n.
    fn present(&self) -> u32;
    fn kind(&self, letter: u8) -> DriveKind;
    /// The volume's label (empty: none); None when the volume does not answer (no media, not ready).
    fn label(&self, letter: u8) -> Option<String>;
    /// (free, total) bytes for the user, None when unknown.
    fn space(&self, letter: u8) -> Option<(u64, u64)>;
}

/// The real answers.
pub struct SystemApi;

fn root_of(letter: u8) -> [u16; 4] {
    [u16::from(letter), u16::from(b':'), u16::from(b'\\'), 0]
}

impl DriveApi for SystemApi {
    fn present(&self) -> u32 {
        snapshot().present
    }

    fn kind(&self, letter: u8) -> DriveKind {
        let root = root_of(letter);
        // SAFETY: a NUL-terminated root path that lives for the call.
        match unsafe { GetDriveTypeW(PCWSTR(root.as_ptr())) } {
            2 => DriveKind::Removable,
            3 => DriveKind::Fixed,
            4 => DriveKind::Network,
            5 => DriveKind::Optical,
            6 => DriveKind::Ram,
            _ => DriveKind::Unknown,
        }
    }

    fn label(&self, letter: u8) -> Option<String> {
        let root = root_of(letter);
        let mut name = [0u16; 261];
        // SAFETY: this thread's error mode only; it keeps a missing medium from raising a system box.
        let _ = unsafe { SetThreadErrorMode(SEM_FAILCRITICALERRORS, None) };
        // SAFETY: a NUL-terminated root and a writable buffer, both alive for the call.
        unsafe { GetVolumeInformationW(PCWSTR(root.as_ptr()), Some(&mut name), None, None, None, None) }.ok()?;
        let n = name.iter().position(|&u| u == 0).unwrap_or(name.len());
        Some(String::from_utf16_lossy(name.get(..n).unwrap_or_default()))
    }

    fn space(&self, letter: u8) -> Option<(u64, u64)> {
        let root = root_of(letter);
        let (mut free, mut total) = (0u64, 0u64);
        // SAFETY: a NUL-terminated root and writable counters, alive for the call.
        unsafe { GetDiskFreeSpaceExW(PCWSTR(root.as_ptr()), Some(&mut free), Some(&mut total), None) }.ok()?;
        Some((free, total))
    }
}

/// The two budgets of facts().
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Budget {
    pub local_ms: u64,
    pub net_label_ms: u64,
}

impl Default for Budget {
    fn default() -> Budget {
        Budget { local_ms: FACTS_BUDGET_MS, net_label_ms: NET_LABEL_BUDGET_MS }
    }
}

/// f on a thread of its own, waited for at most `ms`: None when it did not answer (the thread is left to finish by
/// itself; a stuck volume holds nothing of ours).
fn within<T: Send + 'static>(ms: u64, f: impl FnOnce() -> T + Send + 'static) -> Option<T> {
    let (tx, rx) = mpsc::channel();
    std::thread::Builder::new()
        .name("files-drive".into())
        .spawn(move || {
            let _ = tx.send(f());
        })
        .ok()?;
    rx.recv_timeout(Duration::from_millis(ms)).ok()
}

/// What one drive tells, inside the budget. A network drive: its label (200 ms) and no size at all. Any other: the
/// label and the size (1.5 s); a medium that does not answer is not ready; no answer in time is the letter alone.
pub fn facts(api: &Arc<dyn DriveApi>, letter: u8, kind: DriveKind, budget: Budget) -> DriveInfo {
    let letter = letter.to_ascii_uppercase();
    let bare = DriveInfo { letter, label: None, kind, free: None, total: None, ready: true };
    let labelled = |label: String| Some(label).filter(|l| !l.is_empty());
    if kind == DriveKind::Network {
        let a = Arc::clone(api);
        return match within(budget.net_label_ms, move || a.label(letter)) {
            Some(Some(label)) => DriveInfo { label: labelled(label), ..bare },
            _ => bare,
        };
    }
    let a = Arc::clone(api);
    let answered = within(budget.local_ms, move || a.label(letter).map(|label| (label, a.space(letter))));
    match answered {
        Some(Some((label, space))) => {
            DriveInfo { label: labelled(label), free: space.map(|s| s.0), total: space.map(|s| s.1), ..bare }
        }
        Some(None) => DriveInfo { ready: false, ..bare },
        None => bare,
    }
}

/// Fwd::Bump to every main window.
fn bump_all(shared: &Shared) {
    for w in shared.main_windows() {
        shared.hub.post_to(w, Msg::Fwd(Fwd::Bump));
    }
}

/// The system the scan asks: None is the real one. The fakes install MadeUpApi, so every test and scene sees the same
/// drives whatever machine it runs on; the application never sets it.
static API: Mutex<Option<Arc<dyn DriveApi>>> = Mutex::new(None);

/// Replaces (or, with None, restores) the system the scan asks.
#[doc(hidden)]
pub fn set_api(api: Option<Arc<dyn DriveApi>>) {
    *API.lock().unwrap_or_else(PoisonError::into_inner) = api;
}

/// Three made-up drives: System (C:), Data (D:) and a mapped Share (Z:). Instant answers, no file system.
pub struct MadeUpApi;

impl DriveApi for MadeUpApi {
    fn instant(&self) -> bool {
        true
    }

    fn present(&self) -> u32 {
        [b'C', b'D', b'Z'].iter().fold(0, |m, l| m | (1 << (l - b'A')))
    }

    fn kind(&self, letter: u8) -> DriveKind {
        if letter == b'Z' {
            DriveKind::Network
        } else {
            DriveKind::Fixed
        }
    }

    fn label(&self, letter: u8) -> Option<String> {
        Some(
            match letter {
                b'C' => "System",
                b'D' => "Data",
                _ => "Share",
            }
            .to_string(),
        )
    }

    fn space(&self, letter: u8) -> Option<(u64, u64)> {
        match letter {
            b'C' => Some((412_000_000_000, 1_990_000_000_000)),
            b'D' => Some((70_000_000_000, 1_000_000_000_000)),
            _ => None,
        }
    }
}

/// GetLogicalDrives, then a Bg job per letter (win 0): the letters are in the cache at once, without a size, and each
/// job's DriveFacts fills its own.
pub fn scan(shared: &Shared) {
    let api = API.lock().unwrap_or_else(PoisonError::into_inner).clone().unwrap_or_else(|| Arc::new(SystemApi));
    scan_with(shared, api, Budget::default());
}

/// scan over another system (tests).
pub fn scan_with(shared: &Shared, api: Arc<dyn DriveApi>, budget: Budget) {
    let present = api.present() & 0x03FF_FFFF;
    let old: Vec<DriveInfo> = shared.drives.borrow().all().to_vec();
    let mut cache = DriveCache::new();
    let mut asked = Vec::new();
    for n in 0..26u8 {
        if present & (1 << n) == 0 {
            continue;
        }
        let (letter, kind) = (b'A' + n, api.kind(b'A' + n));
        if api.instant() {
            cache.set(facts(&api, letter, kind, budget));
            continue;
        }
        let known = old.iter().find(|d| d.letter == letter && d.kind == kind).cloned();
        cache.set(known.unwrap_or(DriveInfo { letter, label: None, kind, free: None, total: None, ready: true }));
        asked.push((letter, kind));
    }
    let changed = *shared.drives.borrow() != cache;
    *shared.drives.borrow_mut() = cache;
    for (letter, kind) in asked {
        let lane = if kind == DriveKind::Network { Lane::Network } else { Lane::Local };
        let a = Arc::clone(&api);
        shared.services.bg.run(
            0,
            lane,
            Box::new(move || {
                let info = facts(&a, letter, kind, budget);
                // A drive pulled while its facts were read is not put back.
                (a.present() & (1 << (letter - b'A')) != 0).then_some(JobDone::DriveFacts(info))
            }),
        );
    }
    if changed {
        bump_all(shared);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Slow;

    impl DriveApi for Slow {
        fn present(&self) -> u32 {
            0
        }
        fn kind(&self, _: u8) -> DriveKind {
            DriveKind::Fixed
        }
        fn label(&self, letter: u8) -> Option<String> {
            match letter {
                b'D' => None,
                b'N' => {
                    std::thread::sleep(Duration::from_millis(400));
                    Some("late".into())
                }
                _ => Some("Data".into()),
            }
        }
        fn space(&self, letter: u8) -> Option<(u64, u64)> {
            if letter == b'S' {
                std::thread::sleep(Duration::from_millis(400));
            }
            Some((10, 100))
        }
    }

    const TIGHT: Budget = Budget { local_ms: 100, net_label_ms: 50 };

    #[test]
    fn facts_inside_and_outside_the_budget() {
        let api: Arc<dyn DriveApi> = Arc::new(Slow);
        let ok = facts(&api, b'c', DriveKind::Fixed, TIGHT);
        assert_eq!(
            (ok.letter, ok.label.as_deref(), ok.free, ok.total, ok.ready),
            (b'C', Some("Data"), Some(10), Some(100), true)
        );
        let no_media = facts(&api, b'D', DriveKind::Optical, TIGHT);
        assert_eq!((no_media.ready, no_media.free), (false, None), "a volume that does not answer is not ready");
        let slow = facts(&api, b'S', DriveKind::Fixed, TIGHT);
        assert_eq!((slow.ready, slow.label, slow.free, slow.total), (true, None, None, None), "the letter alone");
        let net_fast = facts(&api, b'Z', DriveKind::Network, TIGHT);
        assert_eq!(
            (net_fast.label.as_deref(), net_fast.free, net_fast.total),
            (Some("Data"), None, None),
            "never a size"
        );
        let net_slow = facts(&api, b'N', DriveKind::Network, TIGHT);
        assert_eq!((net_slow.label, net_slow.ready), (None, true), "the label is given up");
    }
}
