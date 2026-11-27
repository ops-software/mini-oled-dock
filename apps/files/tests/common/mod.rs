//! Shared support of the files package's integration tests: TempRoot (temp.rs), the natural-sort corpus (corpus.rs),
//! the gate of the disposable tests, the no-window spawn guard, per-test tags and the paths of the built artifacts.
//! Every test binary includes this module and uses a part of it.
#![allow(dead_code)]

pub mod corpus;
pub mod temp;

use std::collections::hash_map::RandomState;
use std::hash::{BuildHasher, Hasher};
use std::os::windows::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdout, Command, ExitStatus, Stdio};
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::{Duration, Instant};

/// `CREATE_NO_WINDOW` (winbase.h): every process a test starts carries it (lint no_window_spawns).
pub const CREATE_NO_WINDOW: u32 = 0x0800_0000;

/// The first line of every disposable_* test: they run only in CI's files-disposable job on a GitHub-hosted runner,
/// where they may recycle, answer system dialogs and register the fault DLL.
pub fn disposable() {
    let var = |k: &str| std::env::var(k).unwrap_or_default();
    let ok = var("FILES_DISPOSABLE") == "1"
        && var("GITHUB_ACTIONS") == "true"
        && var("RUNNER_ENVIRONMENT") == "github-hosted";
    assert!(ok, "disposable tests run only in the files-disposable CI job on a GitHub-hosted runner");
}

/// A child process that is killed and waited for when the guard is dropped.
pub struct ChildGuard {
    child: Child,
}

impl ChildGuard {
    pub fn pid(&self) -> u32 {
        self.child.id()
    }

    pub fn child(&mut self) -> &mut Child {
        &mut self.child
    }

    /// The child's stdout (piped), once.
    pub fn take_stdout(&mut self) -> Option<ChildStdout> {
        self.child.stdout.take()
    }

    /// The exit status within `timeout`, polling every 10 ms; None while it still runs.
    pub fn wait_timeout(&mut self, timeout: Duration) -> Option<ExitStatus> {
        let start = Instant::now();
        loop {
            if let Ok(Some(s)) = self.child.try_wait() {
                return Some(s);
            }
            if start.elapsed() >= timeout {
                return None;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
    }
}

impl Drop for ChildGuard {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// Starts `exe` with CREATE_NO_WINDOW, stdin and stderr null, stdout piped, and `env` set on the child only.
pub fn spawn_no_window(exe: &Path, args: &[&str], env: &[(&str, &str)]) -> ChildGuard {
    let mut c = Command::new(exe);
    c.creation_flags(CREATE_NO_WINDOW).args(args).stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::null());
    for (k, v) in env {
        c.env(k, v);
    }
    let child = c.spawn().unwrap_or_else(|e| panic!("spawn {}: {e}", exe.display()));
    ChildGuard { child }
}

/// Nine characters [a-z0-9], unique per test and per run: the process id in base 36 (5), a per-process counter (2)
/// and two characters of a random hash.
pub fn test_tag() -> Box<str> {
    static NEXT: AtomicU32 = AtomicU32::new(0);
    let n = NEXT.fetch_add(1, Ordering::Relaxed);
    let mut h = RandomState::new().build_hasher();
    h.write_u32(n);
    let r = (h.finish() % (36 * 36)) as u32;
    format!("{}{}{}", base36(std::process::id(), 5), base36(n, 2), base36(r, 2)).into()
}

/// The low `width` base-36 digits of `v`, zero-padded.
pub fn base36(mut v: u32, width: usize) -> String {
    const DIGITS: &[u8; 36] = b"0123456789abcdefghijklmnopqrstuvwxyz";
    let mut out = vec![b'0'; width];
    for slot in out.iter_mut().rev() {
        *slot = DIGITS[(v % 36) as usize];
        v /= 36;
    }
    String::from_utf8(out).expect("ASCII digits")
}

/// The test hooks of one test: its own pipe tag, its TempRoot as the only root a host may touch, its own data folder
/// under the root, no system UI, and this process as the parent the host follows.
pub fn hooks(root: &temp::TempRoot) -> files_engine::host::TestHooks {
    let s = |p: &Path| -> Box<str> { p.display().to_string().into() };
    files_engine::host::TestHooks {
        pipe_tag: Some(test_tag()),
        build_tag: None,
        env: vec![
            ("FILES_TEST_ROOT".into(), s(root.path())),
            ("FILES_DATA_DIR".into(), s(&root.path().join("data"))),
            ("FILES_TEST_NO_UI".into(), "1".into()),
            ("FILES_TEST_PARENT".into(), std::process::id().to_string().into()),
        ],
    }
}

/// The files.exe this test run built.
pub fn files_exe() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_files"))
}

/// The fault DLL (the files_faultext example) of this build profile: <target>\<profile>\examples\files_faultext.dll.
/// Panics with the build command when it is missing or older than one of its sources.
pub fn fault_dll() -> PathBuf {
    let exe = std::env::current_exe().expect("the test executable's path");
    let profile = exe.parent().and_then(Path::parent).expect("<target>\\<profile>\\deps\\<exe>");
    let dll = profile.join("examples").join("files_faultext.dll");
    let built = std::fs::metadata(&dll).and_then(|m| m.modified()).ok();
    let src = Path::new(env!("CARGO_MANIFEST_DIR")).join("examples").join("files_faultext");
    let newest = newest_mtime(&src);
    match (built, newest) {
        (Some(b), Some(n)) if b >= n => dll,
        (Some(_), None) => dll,
        _ => panic!("{} is missing or stale: build it: cargo build -p files --examples --locked", dll.display()),
    }
}

fn newest_mtime(dir: &Path) -> Option<std::time::SystemTime> {
    let mut newest = None;
    for e in std::fs::read_dir(dir).ok()?.flatten() {
        let p = e.path();
        let t = if p.is_dir() { newest_mtime(&p) } else { e.metadata().and_then(|m| m.modified()).ok() };
        newest = newest.max(t);
    }
    newest
}
