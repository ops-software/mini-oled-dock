//! LiveRoot: the probes' only place to create, change and delete files (plan rule D2), under
//! %TEMP%\mod-files-live-*. It reuses TempRoot's fence (tests/common/temp.rs), so both share one implementation.
//! A probe includes this file with #[path = "common/live.rs"] mod live.
//!
//! `LiveRoot::new` makes a fresh %TEMP%\mod-files-live-<stamp> and removes it when dropped. `LiveRoot::at` adopts a
//! folder the live tester made (a mod-files-live-* folder directly under %TEMP%, or a folder inside one, with no
//! reparse point in its chain) and never removes it: the tester's own records live next to it.
#![allow(dead_code)]

#[path = "../../tests/common/temp.rs"]
pub mod temp;

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU32, Ordering};

use temp::fence;

/// The folder-name prefix of every LiveRoot.
pub const LIVE_PREFIX: &str = "mod-files-live-";

pub struct LiveRoot {
    path: PathBuf,
    owned: bool,
}

impl LiveRoot {
    /// A new, empty %TEMP%\mod-files-live-<seconds>-<pid>-<n>, removed when dropped.
    pub fn new(tag: &str) -> LiveRoot {
        static NEXT: AtomicU32 = AtomicU32::new(0);
        let base = fence::temp_base();
        let secs = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0);
        loop {
            let n = NEXT.fetch_add(1, Ordering::Relaxed);
            let path = base.join(format!("{LIVE_PREFIX}{secs}-{}-{n}", std::process::id()));
            match std::fs::create_dir(&path) {
                Ok(()) => return LiveRoot { path, owned: true },
                Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists && n < 1_000_000 => continue,
                Err(e) => panic!("LiveRoot {tag}: cannot create {}: {e}", path.display()),
            }
        }
    }

    /// Adopts an existing folder made by the live tester; refuses anything outside a mod-files-live-* root.
    pub fn at(path: &Path) -> Result<LiveRoot, String> {
        if !path.is_absolute() || !path.is_dir() {
            return Err(format!("{} is not an existing absolute folder", path.display()));
        }
        if !fence::within_fenced(path, LIVE_PREFIX) {
            return Err(format!("{} is not inside a {LIVE_PREFIX} folder directly under %TEMP%", path.display()));
        }
        Ok(LiveRoot { path: path.to_path_buf(), owned: false })
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn at_rel(&self, rel: &str) -> PathBuf {
        fence::resolve(&self.path, rel)
    }

    pub fn file(&self, rel: &str, bytes: &[u8]) -> PathBuf {
        fence::file(&self.path, rel, bytes)
    }

    pub fn file_wide(&self, name_units: &[u16], bytes: &[u8]) -> PathBuf {
        fence::file_wide(&self.path, name_units, bytes)
    }

    pub fn dir(&self, rel: &str) -> PathBuf {
        fence::dir(&self.path, rel)
    }

    pub fn write(&self, rel: &str, bytes: &[u8]) {
        fence::write(&self.path, rel, bytes)
    }

    pub fn remove(&self, rel: &str) {
        fence::remove(&self.path, rel)
    }

    pub fn remove_dir(&self, rel: &str) {
        fence::remove_dir(&self.path, rel)
    }

    pub fn rename(&self, from: &str, to: &str) {
        fence::rename(&self.path, from, to)
    }

    pub fn touch(&self, rel: &str, mtime: i64) {
        fence::touch(&self.path, rel, mtime)
    }

    pub fn set_readonly(&self, rel: &str, on: bool) {
        fence::set_readonly(&self.path, rel, on)
    }

    pub fn ads(&self, rel: &str, stream: &str, bytes: &[u8]) {
        fence::ads(&self.path, rel, stream, bytes)
    }

    /// Makes `rel` a junction to `target`, which must be a folder inside a LiveRoot.
    pub fn junction(&self, rel: &str, target: &Path) {
        fence::junction(&self.path, rel, target, LIVE_PREFIX)
    }

    pub fn hash(&self, rel: &str) -> u64 {
        fence::hash(&self.path, rel)
    }

    pub fn hash_tree(&self, rel: &str) -> BTreeMap<String, String> {
        fence::hash_tree(&self.path, rel)
    }

    pub fn deny_listing(&self, rel: &str) {
        fence::deny_listing_at(&fence::resolve(&self.path, rel))
    }

    pub fn allow_listing(&self, rel: &str) {
        fence::allow_listing_at(&fence::resolve(&self.path, rel))
    }

    /// Creates `n` files of `size` bytes (zeros) in the folder `rel`, named f000001.bin, f000002.bin, ...
    pub fn make_n(&self, rel: &str, n: u32, size: u64) -> Vec<PathBuf> {
        let bytes = vec![0u8; usize::try_from(size).expect("a size that fits in memory")];
        let name = |i: u32| if rel.is_empty() { format!("f{i:06}.bin") } else { format!("{rel}/f{i:06}.bin") };
        (1..=n).map(|i| fence::file(&self.path, &name(i), &bytes)).collect()
    }
}

impl Drop for LiveRoot {
    fn drop(&mut self) {
        if !self.owned {
            return;
        }
        if !fence::is_fenced(&self.path, LIVE_PREFIX) {
            let msg = format!(
                "LiveRoot: refusing to delete {}: not a {LIVE_PREFIX} folder under %TEMP%",
                self.path.display()
            );
            if std::thread::panicking() {
                eprintln!("{msg}");
                return;
            }
            panic!("{msg}");
        }
        let errors = fence::remove_tree(&self.path);
        if !errors.is_empty() {
            eprintln!("LiveRoot: left behind: {errors:?}");
        }
    }
}
