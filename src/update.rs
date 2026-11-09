//! Self-updater state and check worker.
//!
//! `status()` is the snapshot the UI reads; `check_now()` runs one check on a throwaway
//! `update` thread (the UI thread never blocks on the network). Everything the worker learns is
//! kept in `STATE` and mirrored into `config.json` (`last_update_check`, `update_available`).
//! `stage()` downloads a release into the admin-only `<data>\update\<tag>\` folder and verifies it
//! (SHA-256, then `--version`); `install_now()` hands the verified exe to `--install --quiet`.
//! `init`/`sync_timer`/`on_timer` drive the daily check (`TIMER_UPDATE_CHECK` on the dock window);
//! `on_idle` installs a staged release when `update_auto_install` is on and nothing is open.

// Wired into the dock by later tasks; until then some items are unused.
#![allow(dead_code)]

use std::io::Read;
use std::os::windows::io::{AsRawHandle, OwnedHandle};
use std::os::windows::process::CommandExt;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use windows::Win32::Foundation::{HANDLE, HWND, WAIT_OBJECT_0};
use windows::Win32::System::Threading::{GetExitCodeProcess, WaitForSingleObject, CREATE_NO_WINDOW};
use windows::Win32::UI::WindowsAndMessaging::{KillTimer, PostMessageW, SetTimer};

use crate::config::{self, Config};
use crate::datadir;
use crate::msg::{TIMER_UPDATE_CHECK, WM_DOCK_UPDATE};
use crate::shell_thread::{self, ShellRequest};
use crate::update_release::{
    self, host_allowed, is_newer, next_check_delay_ms, parse_release, parse_sha256_file, parse_tag, release_page_url,
    rfc3339_utc, version_output_matches, Asset, Release, Version, API_MAX_BYTES, CHECK_PERIOD_MS, EXE_ASSET,
    EXE_MAX_BYTES, FIRST_CHECK_MS, LATEST_API_URL, RETRY_MS, SHA_ASSET, SHA_MAX_BYTES,
};
use crate::winhttp;

/// What the update worker is doing right now.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum Phase {
    #[default]
    Idle,
    Checking,
    Downloading,
    Verifying,
    Installing,
}

/// Result of the last finished operation.
#[derive(Clone, Debug, PartialEq, Eq, Default)]
pub enum Outcome {
    #[default]
    None,
    UpToDate,
    Available,
    /// `Some(code)` = GitHub answered with that HTTP status; `None` = no answer or an unusable one.
    CouldNotCheck(Option<u16>),
    DownloadFailed,
    VerifyFailed,
    Ready,
    InstallFailed(i32),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Available {
    pub tag: String,
    pub version: Version,
    /// `YYYY-MM-DD` or "".
    pub date: String,
    pub notes: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct UpdateStatus {
    pub installed: Version,
    pub phase: Phase,
    pub available: Option<Available>,
    /// UTC time of the last successful check (RFC 3339) or "".
    pub last_check: String,
    pub outcome: Outcome,
    /// true = `available`'s tag is downloaded and verified. Set only by `stage()`; cleared by
    /// `stage()` on failure and by `apply_check` when nothing newer exists.
    pub staged: bool,
}

impl UpdateStatus {
    /// 1 when a newer release is known, else 0.
    pub fn badge(&self) -> u32 {
        u32::from(self.available.is_some())
    }

    /// Short localized status line. A running phase wins over the last outcome.
    pub fn line(&self) -> String {
        let it = crate::i18n::italian();
        let pick = |en: String, ita: String| if it { ita } else { en };
        let tag = self.available.as_ref().map(|a| a.tag.as_str()).unwrap_or("");
        let v = self.available.as_ref().map(|a| a.version.to_string()).unwrap_or_default();
        match self.phase {
            Phase::Checking => return pick("Checking for updates...".into(), "Ricerca aggiornamenti...".into()),
            Phase::Downloading => return pick(format!("Downloading {tag}..."), format!("Download di {tag}...")),
            Phase::Verifying => return pick("Verifying the download...".into(), "Verifica del download...".into()),
            Phase::Installing => return pick(format!("Installing {tag}..."), format!("Installazione di {tag}...")),
            Phase::Idle => {}
        }
        match &self.outcome {
            Outcome::None => pick("Not checked yet".into(), "Non ancora verificato".into()),
            Outcome::UpToDate => pick("Up to date".into(), "Aggiornato".into()),
            Outcome::Available => pick(format!("Version {v} available"), format!("Disponibile la versione {v}")),
            Outcome::CouldNotCheck(Some(c)) => pick(
                format!("Could not check for updates (HTTP {c})"),
                format!("Impossibile cercare aggiornamenti (HTTP {c})"),
            ),
            Outcome::CouldNotCheck(None) => pick(
                "Could not check for updates (no connection)".into(),
                "Impossibile cercare aggiornamenti (nessuna connessione)".into(),
            ),
            Outcome::DownloadFailed => pick("Download failed".into(), "Download non riuscito".into()),
            Outcome::VerifyFailed => {
                pick("Download failed verification".into(), "Il download non ha superato la verifica".into())
            }
            Outcome::Ready => pick(
                format!("Version {v} downloaded and verified"),
                format!("Versione {v} scaricata e verificata"),
            ),
            Outcome::InstallFailed(c) => pick(
                format!("Installation failed (exit code {c})"),
                format!("Installazione non riuscita (codice {c})"),
            ),
        }
    }
}

#[derive(Debug)]
pub(crate) enum CheckError {
    /// GitHub answered, but not with 200.
    Http(u16),
    /// No answer (DNS, TLS, timeout, oversized body).
    Network(String),
    /// A 200 that is not a usable release (captive portal HTML, bad tag, foreign host).
    Invalid(String),
}

impl std::fmt::Display for CheckError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            CheckError::Http(c) => write!(f, "HTTP {c}"),
            CheckError::Network(e) => write!(f, "{e}"),
            CheckError::Invalid(e) => write!(f, "unusable answer: {e}"),
        }
    }
}

static STATE: Mutex<Option<UpdateStatus>> = Mutex::new(None);
/// The last fetched newer release, in memory only (the download uses its asset URLs).
static LATEST: Mutex<Option<Release>> = Mutex::new(None);
/// Serializes all worker operations.
static BUSY: AtomicBool = AtomicBool::new(false);

/// The status a fresh start shows: only a canonical tag newer than `installed` is trusted, because
/// `config.json` is user-writable.
pub(crate) fn status_from_config(c: &Config, installed: Version) -> UpdateStatus {
    let available = parse_tag(&c.update_available).filter(|v| *v > installed).map(|version| Available {
        tag: c.update_available.clone(),
        version,
        date: String::new(),
        notes: String::new(),
    });
    let last_check = if update_release::parse_rfc3339_utc(&c.last_update_check).is_some() {
        c.last_update_check.clone()
    } else {
        String::new()
    };
    let outcome = if available.is_some() { Outcome::Available } else { Outcome::None };
    UpdateStatus { installed, phase: Phase::Idle, available, last_check, outcome, staged: false }
}

/// Folds a finished check into the status. A failed check changes only `outcome`.
pub(crate) fn apply_check(st: &mut UpdateStatus, r: &Result<Release, CheckError>, installed: Version, now: u64) {
    match r {
        Ok(rel) if rel.version > installed => {
            // `staged` only holds for the tag that was verified: a newer tag is not downloaded yet.
            st.staged &= st.available.as_ref().is_some_and(|a| a.tag == rel.tag);
            st.available = Some(Available {
                tag: rel.tag.clone(),
                version: rel.version,
                date: rel.published.get(..10).unwrap_or("").to_string(),
                notes: rel.notes.clone(),
            });
            st.outcome = if st.staged { Outcome::Ready } else { Outcome::Available };
            st.last_check = rfc3339_utc(now);
        }
        Ok(_) => {
            st.available = None;
            st.staged = false;
            st.outcome = Outcome::UpToDate;
            st.last_check = rfc3339_utc(now);
        }
        Err(CheckError::Http(c)) => st.outcome = Outcome::CouldNotCheck(Some(*c)),
        Err(_) => st.outcome = Outcome::CouldNotCheck(None),
    }
}

fn fetch_latest() -> Result<Release, CheckError> {
    let headers = [("Accept", "application/vnd.github+json"), ("X-GitHub-Api-Version", "2022-11-28")];
    let resp = winhttp::get(LATEST_API_URL, &headers, API_MAX_BYTES).map_err(|e| CheckError::Network(e.to_string()))?;
    let host_ok = winhttp::split_https_url(&resp.final_url).is_some_and(|(h, _)| host_allowed(&h));
    if !host_ok {
        return Err(CheckError::Invalid("unexpected host".into()));
    }
    if resp.status != 200 {
        crate::log!("update: check failed, HTTP {}", resp.status);
        return Err(CheckError::Http(resp.status));
    }
    parse_release(&resp.body).map_err(CheckError::Invalid)
}

/// Runs `f` on the shared status, filling an empty one from the config on first use.
fn with_state<R>(f: impl FnOnce(&mut UpdateStatus) -> R) -> R {
    let mut g = STATE.lock().unwrap_or_else(|e| e.into_inner());
    let st = g.get_or_insert_with(|| status_from_config(&config::get(), Version::current()));
    f(st)
}

/// Tells the UI thread the status changed (harmless without a window, e.g. in CLI modes).
fn notify() {
    let h = crate::hwnd();
    if h.0.is_null() {
        return;
    }
    // SAFETY: PostMessageW is thread-safe; if the window no longer exists it just fails.
    let _ = unsafe { PostMessageW(Some(h), WM_DOCK_UPDATE, Default::default(), Default::default()) };
}

pub fn status() -> UpdateStatus {
    with_state(|s| s.clone())
}

/// Runs `job` on a thread named `update`. false = another update operation is already running.
fn spawn_worker(job: impl FnOnce() + Send + 'static) -> bool {
    if BUSY.compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire).is_err() {
        return false;
    }
    let spawned = std::thread::Builder::new().name("update".into()).spawn(move || {
        job();
        BUSY.store(false, Ordering::Release);
    });
    if let Err(e) = spawned {
        BUSY.store(false, Ordering::Release);
        crate::log!("update: cannot start the worker thread: {e}");
        return false;
    }
    true
}

fn now_unix() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// Starts a check on the worker thread. false = an update operation is already running.
pub fn check_now() -> bool {
    spawn_worker(|| {
        check_once();
        // With `update_auto_install` on, a newer release is downloaded and verified right away, so
        // the idle gate (`on_idle`) only has to start the installer. With it off nothing is downloaded.
        if config::get().update_auto_install {
            if let Some(rel) = latest_target(Version::current()) {
                let _ = stage(&rel);
            }
        }
    })
}

/// One check, on the worker thread: asks GitHub, folds the answer into the status and the config.
fn check_once() {
    with_state(|s| s.phase = Phase::Checking);
    notify();
    let installed = Version::current();
    let r = fetch_latest();
    match &r {
        Ok(rel) if rel.version > installed => crate::log!("update: {} available", rel.tag),
        Ok(rel) => crate::log!("update: up to date (latest {})", rel.tag),
        Err(e) => crate::log!("update: could not check ({e})"),
    }
    if let Ok(rel) = &r {
        *LATEST.lock().unwrap_or_else(|e| e.into_inner()) = is_newer(&rel.tag, installed).then(|| rel.clone());
    }
    let (last, tag) = with_state(|s| {
        apply_check(s, &r, installed, now_unix());
        s.phase = Phase::Idle;
        (s.last_check.clone(), s.available.as_ref().map(|a| a.tag.clone()).unwrap_or_default())
    });
    let c = config::get();
    if c.last_update_check != last || c.update_available != tag {
        if let Err(e) = config::modify(|c| {
            c.last_update_check = last;
            c.update_available = tag;
        }) {
            crate::log!("update: cannot save the check result: {e}");
        }
    }
    notify();
}

/// Opens the release page (or the latest-release page) de-elevated through the shell thread.
pub fn open_release_page() {
    let url = release_page_url(status().available.as_ref().map(|a| a.tag.as_str()));
    shell_thread::request(ShellRequest::Launch { target: url, args: None });
}

#[derive(Debug)]
pub(crate) enum StageError {
    /// Nothing (or only part) is staged, or the download failed: the folder is kept for a retry.
    Download(String),
    /// The staged files are complete but wrong: the folder has been deleted.
    Verify(String),
}

impl std::fmt::Display for StageError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            StageError::Download(e) => write!(f, "download failed: {e}"),
            StageError::Verify(e) => write!(f, "failed verification: {e}"),
        }
    }
}

/// Largest `--version` output read back from a staged exe.
const CAPTURE_MAX_BYTES: u64 = 4096;
/// How long a staged exe may take to answer `--version`.
const VERSION_TIMEOUT: Duration = Duration::from_secs(5);

/// `exe` must hash to the value published in the release's `.sha256` asset.
pub(crate) fn verify_bytes(exe: &[u8], sha_file: &[u8]) -> Result<(), String> {
    let want = parse_sha256_file(sha_file).ok_or("unreadable .sha256 file")?;
    let got = crate::sha256::sha256(exe)?;
    if got == want {
        Ok(())
    } else {
        Err("SHA-256 mismatch".into())
    }
}

/// Runs `exe args` without a window and returns its exit code and stdout (at most 4 KB). A child
/// that does not exit within `timeout` is killed and reported as `Err("timeout")`.
pub(crate) fn run_capture(exe: &Path, args: &[&str], timeout: Duration) -> Result<(i32, Vec<u8>), String> {
    let mut child = std::process::Command::new(exe)
        .args(args)
        .creation_flags(CREATE_NO_WINDOW.0)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .spawn()
        .map_err(|e| format!("cannot start {}: {e}", exe.display()))?;
    let t = Instant::now();
    let code = loop {
        match child.try_wait() {
            Ok(Some(st)) => break st.code().unwrap_or(-1),
            Ok(None) => {
                if t.elapsed() > timeout {
                    let _ = child.kill();
                    let _ = child.wait();
                    return Err("timeout".into());
                }
                std::thread::sleep(Duration::from_millis(50));
            }
            Err(e) => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(format!("waiting for {} failed: {e}", exe.display()));
            }
        }
    };
    let mut out = Vec::new();
    if let Some(stdout) = child.stdout.take() {
        stdout
            .take(CAPTURE_MAX_BYTES)
            .read_to_end(&mut out)
            .map_err(|e| format!("reading the output of {}: {e}", exe.display()))?;
    }
    Ok((code, out))
}

/// Downloads `dock.exe` and `dock.exe.sha256` of `rel` into `<data>\update\<tag>\`. Each asset must
/// come back with 200, from an allowed host, at exactly the size the release JSON announced.
fn download(rel: &Release) -> Result<(), StageError> {
    let fail = StageError::Download;
    for (asset, name, cap) in [(&rel.exe, EXE_ASSET, EXE_MAX_BYTES), (&rel.sha256, SHA_ASSET, SHA_MAX_BYTES)] {
        let Asset { url, size } = asset;
        let resp = winhttp::get(url, &[("Accept", "application/octet-stream")], cap)
            .map_err(|e| fail(format!("{name}: {e}")))?;
        if resp.status != 200 {
            return Err(fail(format!("{name}: HTTP {}", resp.status)));
        }
        if !winhttp::split_https_url(&resp.final_url).is_some_and(|(h, _)| host_allowed(&h)) {
            return Err(fail(format!("{name}: unexpected host")));
        }
        if resp.body.len() as u64 != *size {
            return Err(fail(format!("{name}: {} bytes, expected {size}", resp.body.len())));
        }
        datadir::update_stage_dir(&rel.tag).map_err(fail)?;
        datadir::write_stage_file(&rel.tag, name, &resp.body).map_err(|e| fail(format!("{name}: {e}")))?;
        // `resp` (and its body) is dropped here, before the next asset is fetched.
    }
    crate::log!("update: {} downloaded", rel.tag);
    Ok(())
}

/// Checks the staged `<data>\update\<tag>\dock.exe` and returns its path: hash equal to the staged
/// `.sha256`, then `dock.exe --version` must print exactly `mini-oled-dock <tag version>`.
///
/// Running the staged exe for `--version` is safe: its SHA-256 matched the `.sha256` of the same
/// release, both fetched over HTTPS from the allowed GitHub hosts; `datadir` has checked that the
/// folder is admin-only and not a reparse point and that the file is a single-link regular file
/// inside it; the folder was created by this elevated process, so no user can swap the file after
/// the check.
///
/// A missing file is `Download` (the caller downloads); any other failure deletes the folder and
/// is `Verify`.
pub(crate) fn verify_stage(tag: &str) -> Result<PathBuf, StageError> {
    let Some(version) = parse_tag(tag) else {
        return Err(StageError::Verify(format!("{tag:?} is not a canonical tag")));
    };
    let read = |name: &str, cap: u64| datadir::read_stage_file(tag, name, cap);
    let files = match (read(EXE_ASSET, EXE_MAX_BYTES), read(SHA_ASSET, SHA_MAX_BYTES)) {
        (Ok(exe), Ok(sha)) => Ok((exe, sha)),
        (Err(e), _) | (_, Err(e)) if e.kind() == std::io::ErrorKind::NotFound => {
            return Err(StageError::Download(format!("not staged: {e}")));
        }
        (Err(e), _) | (_, Err(e)) => Err(format!("cannot read the staged files: {e}")),
    };
    let checked = files.and_then(|(exe, sha)| {
        verify_bytes(&exe, &sha)?;
        drop(exe);
        let path = datadir::checked_stage_path(tag, EXE_ASSET).map_err(|e| format!("staged exe: {e}"))?;
        let (code, out) = run_capture(&path, &["--version"], VERSION_TIMEOUT)?;
        if code != 0 {
            return Err(format!("--version exited with {code}"));
        }
        if !version_output_matches(&out, version) {
            return Err(format!("--version printed {:?}", String::from_utf8_lossy(&out).trim()));
        }
        Ok(path)
    });
    checked.map_err(|why| {
        crate::log!("update: {tag} failed verification: {why}");
        if let Err(e) = datadir::remove_update_stage(tag) {
            crate::log!("update: cannot remove the stage {tag}: {e}");
        }
        StageError::Verify(why)
    })
}

fn set_phase(phase: Phase) {
    with_state(|s| s.phase = phase);
    notify();
}

/// Reuses a stage of `rel` that verifies, else downloads and verifies it. Returns the path of the
/// verified staged `dock.exe`. The only place that sets `UpdateStatus.staged = true` and the
/// `Ready` / `DownloadFailed` / `VerifyFailed` outcomes.
fn stage(rel: &Release) -> Result<PathBuf, StageError> {
    set_phase(Phase::Verifying);
    let mut r = verify_stage(&rel.tag);
    if matches!(r, Err(StageError::Download(_))) {
        set_phase(Phase::Downloading);
        r = download(rel).and_then(|()| {
            set_phase(Phase::Verifying);
            verify_stage(&rel.tag)
        });
    }
    match &r {
        Ok(p) => crate::log!("update: {} staged at {}", rel.tag, p.display()),
        Err(e) => crate::log!("update: {} not staged ({e})", rel.tag),
    }
    with_state(|s| {
        s.phase = Phase::Idle;
        s.staged = r.is_ok();
        s.outcome = match &r {
            Ok(_) => Outcome::Ready,
            Err(StageError::Download(_)) => Outcome::DownloadFailed,
            Err(StageError::Verify(_)) => Outcome::VerifyFailed,
        };
    });
    notify();
    r
}

/// Arguments the staged exe is started with: it stops this dock through `Local\DockStop`, replaces
/// the installed exe, re-registers the task, writes the ARP key and relaunches.
pub(crate) const INSTALL_ARGS: &str = "--install --quiet";
/// How long the dock (or `--update-install`) waits for the installer child.
const INSTALL_WAIT: Duration = Duration::from_secs(120);

/// The release to install: only a freshly fetched one (`LATEST`), and only if newer than
/// `installed`. A badge restored from `config.json` alone never installs anything.
pub(crate) fn install_target(latest: Option<&Release>, installed: Version) -> Option<Release> {
    latest.filter(|r| r.version > installed).cloned()
}

fn latest_target(installed: Version) -> Option<Release> {
    install_target(LATEST.lock().unwrap_or_else(|e| e.into_inner()).as_ref(), installed)
}

/// Waits up to `timeout` for `process` to exit: `Some(exit code)`, or `None` on timeout or error.
fn wait_exit(process: &OwnedHandle, timeout: Duration) -> Option<i32> {
    let h = HANDLE(process.as_raw_handle());
    let ms = u32::try_from(timeout.as_millis()).unwrap_or(u32::MAX - 1);
    // SAFETY: `h` is a valid process handle owned by `process` for the whole call.
    if unsafe { WaitForSingleObject(h, ms) } != WAIT_OBJECT_0 {
        return None;
    }
    let mut code = 0u32;
    // SAFETY: valid process handle and output pointer.
    unsafe { GetExitCodeProcess(h, &mut code) }.ok()?;
    Some(code as i32)
}

/// Installs the newest release on the worker thread: check if nothing fresh is known, stage (download +
/// verify), then hand the verified exe to `--install --quiet`. The child stops this dock through its
/// normal stop path (`WM_DOCK_QUIT_REQUEST`, cleanup, taskbar restored, exit 0). false = an update
/// operation is already running.
pub fn install_now() -> bool {
    spawn_worker(|| {
        let installed = Version::current();
        let rel = match latest_target(installed) {
            Some(r) => r,
            None => {
                // `check_once` sets the outcome itself: UpToDate on an answer, CouldNotCheck otherwise.
                check_once();
                match latest_target(installed) {
                    Some(r) => r,
                    None => return,
                }
            }
        };
        let Ok(path) = stage(&rel) else { return };
        set_phase(Phase::Installing);
        crate::log!("update: installing {} from {}", rel.tag, path.display());
        let failed = |code: i32| {
            with_state(|s| {
                s.phase = Phase::Idle;
                s.outcome = Outcome::InstallFailed(code);
            });
            notify();
        };
        let child = match crate::watchdog::spawn_breakaway(&path, INSTALL_ARGS) {
            Ok(c) => c,
            Err(e) => {
                crate::log!("update: cannot start the installer: {e}");
                return failed(-1);
            }
        };
        match wait_exit(&child, INSTALL_WAIT) {
            Some(0) => {
                crate::log!("update: the installer finished but the dock is still running");
                set_phase(Phase::Idle);
            }
            Some(code) => {
                crate::log!("update: the installer exited with {code}; the stage {} is kept", rel.tag);
                failed(code);
            }
            None => {
                crate::log!("update: the installer did not finish within {} s", INSTALL_WAIT.as_secs());
                set_phase(Phase::Idle);
            }
        }
    })
}

/// Deletes staging folders of canonical tags at or below the running version (failed installs and
/// the version now running). Run once at startup on a thread named `update-cleanup`; the installer
/// that just finished may still run from the current version's folder, so a failure is only logged
/// and the next start retries.
fn cleanup_stages() {
    if !datadir::writable() {
        return;
    }
    for tag in update_release::stale_stages(&datadir::update_stage_names(), Version::current()) {
        match datadir::remove_update_stage(&tag) {
            Ok(()) => crate::log!("update: removed the old stage {tag}"),
            Err(e) => crate::log!("update: cannot remove the old stage {tag}: {e}"),
        }
    }
}

/// `--update-install <tag>`: installs an already staged release. The only input is a canonical tag,
/// mapped to `<data>\update\<tag>\`; there is no path argument. Returns the installer's exit code.
pub fn run_install_cli(tag: &str) -> i32 {
    let say = crate::logger::say;
    if parse_tag(tag).is_none() {
        say(&format!("invalid tag {tag:?} (expected vX.Y.Z)"));
        return crate::autostart::EXIT_BAD_ARGS;
    }
    if !datadir::writable() {
        say("the data folder is not writable (run elevated)");
        return 1;
    }
    let path = match verify_stage(tag) {
        Ok(p) => p,
        Err(e) => {
            say(&format!("{tag}: {e}"));
            return 1;
        }
    };
    crate::log!("update: installing {tag} from {}", path.display());
    let child = match crate::watchdog::spawn_breakaway(&path, INSTALL_ARGS) {
        Ok(c) => c,
        Err(e) => {
            say(&format!("cannot start the installer: {e}"));
            return 1;
        }
    };
    match wait_exit(&child, INSTALL_WAIT) {
        Some(0) => {
            say(&format!("Installed {tag}."));
            0
        }
        Some(code) => {
            say(&format!("Installer exited with {code}."));
            code
        }
        None => {
            say(&format!("The installer did not finish within {} s.", INSTALL_WAIT.as_secs()));
            1
        }
    }
}

/// `--update-check`: one synchronous check, no config write. Exit 0 = answered, 1 = could not check.
pub fn run_check_cli() -> i32 {
    let cur = Version::current();
    match fetch_latest() {
        Ok(rel) if rel.version > cur => {
            let date = rel.published.get(..10).unwrap_or("");
            crate::logger::say(&format!(
                "mini-oled-dock {cur}: {} available ({date}) {}",
                rel.tag,
                release_page_url(Some(&rel.tag))
            ));
            0
        }
        Ok(rel) => {
            crate::logger::say(&format!("mini-oled-dock {cur}: up to date (latest {})", rel.tag));
            0
        }
        Err(e) => {
            crate::logger::say(&format!("mini-oled-dock {cur}: could not check ({e})"));
            1
        }
    }
}

// ------------------------------------------------------------ timer and auto-install

/// true while `TIMER_UPDATE_CHECK` is armed.
static ARMED: AtomicBool = AtomicBool::new(false);
/// true while the check that is running was started by the timer (not by the user).
static SCHEDULED: AtomicBool = AtomicBool::new(false);
/// The tag `on_idle` last tried to install: one automatic attempt per release, so a failing
/// installer is not restarted every 2 s.
static AUTO_TRIED: Mutex<String> = Mutex::new(String::new());

/// Dock start: status from the (now loaded) config, old stages cleaned, timer armed.
pub fn init(h: HWND) {
    let st = status_from_config(&config::get(), Version::current());
    with_state(|s| *s = st);
    let spawned = std::thread::Builder::new().name("update-cleanup".into()).spawn(cleanup_stages);
    if let Err(e) = spawned {
        crate::log!("update: cannot start the cleanup thread: {e}");
    }
    sync_timer(h);
}

/// Arms or kills the check timer according to `update_check`.
pub fn sync_timer(h: HWND) {
    let on = config::get().update_check;
    if on && !ARMED.load(Ordering::Relaxed) {
        // SAFETY: `h` is the dock window, owned by the calling (UI) thread.
        if unsafe { SetTimer(Some(h), TIMER_UPDATE_CHECK, FIRST_CHECK_MS, None) } == 0 {
            crate::log!("update: SetTimer failed");
            return;
        }
        ARMED.store(true, Ordering::Relaxed);
        crate::log!("update: automatic check on");
    } else if !on && ARMED.load(Ordering::Relaxed) {
        // SAFETY: our own id/window; fails harmlessly if the timer does not exist.
        let _ = unsafe { KillTimer(Some(h), TIMER_UPDATE_CHECK) };
        ARMED.store(false, Ordering::Relaxed);
        crate::log!("update: automatic check off");
    }
}

/// `TIMER_UPDATE_CHECK`: checks when 24 h have passed since the last check, else waits for the rest.
pub fn on_timer(h: HWND) {
    let last = update_release::parse_rfc3339_utc(&config::get().last_update_check);
    let delay = next_check_delay_ms(now_unix(), last);
    let next = if delay == 0 {
        SCHEDULED.store(true, Ordering::Relaxed);
        if !check_now() {
            SCHEDULED.store(false, Ordering::Relaxed);
        }
        CHECK_PERIOD_MS
    } else {
        delay
    };
    // Re-arming replaces the interval: still one timer.
    // SAFETY: `h` is the dock window, owned by the calling (UI) thread.
    if unsafe { SetTimer(Some(h), TIMER_UPDATE_CHECK, next, None) } == 0 {
        crate::log!("update: SetTimer failed");
    }
}

/// `WM_DOCK_UPDATE` on the UI thread.
pub fn on_changed(h: HWND) {
    let st = status();
    if SCHEDULED.load(Ordering::Relaxed) && st.phase != Phase::Checking {
        SCHEDULED.store(false, Ordering::Relaxed);
        if matches!(st.outcome, Outcome::CouldNotCheck(_)) && ARMED.load(Ordering::Relaxed) {
            // SAFETY: `h` is the dock window, owned by the calling (UI) thread.
            if unsafe { SetTimer(Some(h), TIMER_UPDATE_CHECK, RETRY_MS, None) } == 0 {
                crate::log!("update: SetTimer failed");
            }
        }
    }
    // An open power popup redraws its badge.
    crate::popup::tick_soon(10);
}

/// A verified release waits for a quiet moment and the user asked for automatic installs.
pub fn auto_install_pending() -> bool {
    let st = status();
    let tried = AUTO_TRIED.lock().unwrap_or_else(|e| e.into_inner());
    let fresh = st.available.as_ref().is_some_and(|a| *tried != a.tag);
    config::get().update_auto_install && st.staged && st.phase == Phase::Idle && fresh
}

/// Pure gate: install only when the option is on, a verified release is staged, the worker is idle
/// and nothing (popup, fullscreen app) would be interrupted.
pub(crate) fn auto_install_due(auto_install: bool, staged: bool, phase: Phase, busy: bool) -> bool {
    auto_install && staged && phase == Phase::Idle && !busy
}

/// From `TIMER_HOUSEKEEPING` when `auto_install_pending()`; `busy` = a popup is open or a fullscreen
/// app has the foreground.
pub fn on_idle(busy: bool) {
    let st = status();
    if !auto_install_due(config::get().update_auto_install, st.staged, st.phase, busy) {
        return;
    }
    let Some(a) = st.available else { return };
    *AUTO_TRIED.lock().unwrap_or_else(|e| e.into_inner()) = a.tag.clone();
    crate::log!("update: auto-install {}", a.tag);
    install_now();
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::update_release::{parse_release, Release, Version, RELEASE_FIXTURE};

    #[test]
    fn status_from_config_trusts_only_newer_tags() {
        let cur = Version::parse("1.4.0").unwrap();
        let with = |t: &str| Config {
            update_available: t.into(),
            last_update_check: "2026-11-09T12:34:56Z".into(),
            ..Config::default()
        };
        let s = status_from_config(&with("v1.4.1"), cur);
        assert_eq!(s.available.as_ref().map(|a| a.tag.as_str()), Some("v1.4.1"));
        assert_eq!((s.badge(), s.staged, s.last_check.as_str()), (1, false, "2026-11-09T12:34:56Z"));
        for t in ["", "v1.4.0", "v1.3.9", "garbage", "../v9.9.9", "v99.0.0-rc1"] {
            assert_eq!(status_from_config(&with(t), cur).badge(), 0, "{t}");
        }
    }

    #[test]
    fn check_results_update_the_status() {
        let cur = Version::parse("1.4.0").unwrap();
        let rel = parse_release(RELEASE_FIXTURE.as_bytes()).unwrap();
        let mut s = status_from_config(&Config::default(), cur);
        apply_check(&mut s, &Ok(rel.clone()), cur, 1_794_227_696);
        assert_eq!((s.outcome.clone(), s.badge()), (Outcome::Available, 1));
        assert_eq!(s.available.as_ref().unwrap().date, "2026-11-20");
        assert_eq!(s.last_check, "2026-11-09T12:34:56Z");
        apply_check(&mut s, &Err(CheckError::Http(403)), cur, 1_794_300_000);
        assert_eq!(s.outcome, Outcome::CouldNotCheck(Some(403)));
        // a failed check changes nothing else
        assert_eq!((s.badge(), s.last_check.as_str()), (1, "2026-11-09T12:34:56Z"));
        // the staged tag survives a re-check of the same tag, not a newer one
        s.staged = true;
        apply_check(&mut s, &Ok(rel.clone()), cur, 1_794_300_000);
        assert_eq!((s.staged, s.outcome.clone()), (true, Outcome::Ready));
        let newer = Release { tag: "v1.4.9".into(), version: Version::parse("1.4.9").unwrap(), ..rel.clone() };
        apply_check(&mut s, &Ok(newer), cur, 1_794_300_000);
        assert_eq!((s.staged, s.outcome.clone(), s.badge()), (false, Outcome::Available, 1));
        let same = Release { tag: "v1.4.0".into(), version: cur, ..rel };
        apply_check(&mut s, &Ok(same), cur, 1_794_300_000);
        assert_eq!((s.outcome.clone(), s.badge()), (Outcome::UpToDate, 0));
    }

    #[test]
    fn verify_bytes_compares_the_published_hash() {
        let sha = b"ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad  dock.exe\r\n";
        assert!(verify_bytes(b"abc", sha).is_ok());
        assert!(verify_bytes(b"abd", sha).is_err());
        assert!(verify_bytes(b"abc", b"not a hash").is_err());
    }

    fn system32(exe: &str) -> PathBuf {
        PathBuf::from(std::env::var_os("SystemRoot").unwrap()).join("System32").join(exe)
    }

    #[test]
    fn run_capture_reads_stdout_and_times_out() {
        let (code, out) =
            run_capture(&system32("cmd.exe"), &["/c", "echo mini-oled-dock 1.4.1"], Duration::from_secs(5)).unwrap();
        assert_eq!(code, 0);
        assert!(crate::update_release::version_output_matches(&out, Version::parse("1.4.1").unwrap()));
        let t = Instant::now();
        assert!(run_capture(&system32("ping.exe"), &["-n", "6", "127.0.0.1"], Duration::from_millis(300)).is_err());
        assert!(t.elapsed() < Duration::from_secs(3)); // killed, not waited for
    }

    #[test]
    fn auto_install_gate() {
        assert!(auto_install_due(true, true, Phase::Idle, false));
        assert!(!auto_install_due(false, true, Phase::Idle, false)); // option off
        assert!(!auto_install_due(true, false, Phase::Idle, false)); // nothing verified yet
        assert!(!auto_install_due(true, true, Phase::Downloading, false));
        assert!(!auto_install_due(true, true, Phase::Idle, true)); // popup / fullscreen app
    }

    #[test]
    fn install_target_needs_a_fresh_newer_release() {
        let cur = Version::parse("1.4.0").unwrap();
        let rel = parse_release(RELEASE_FIXTURE.as_bytes()).unwrap();
        assert_eq!(install_target(None, cur), None); // a badge from config.json alone never installs
        assert_eq!(install_target(Some(&rel), cur), Some(rel.clone()));
        assert_eq!(install_target(Some(&rel), Version::parse("1.4.1").unwrap()), None);
        assert_eq!(INSTALL_ARGS, "--install --quiet");
    }
}
