//! Self-updater state and check worker.
//!
//! `status()` is the snapshot the UI reads; `check_now()` runs one check on a throwaway
//! `update` thread (the UI thread never blocks on the network). Everything the worker learns is
//! kept in `STATE` and mirrored into `config.json` (`last_update_check`, `update_available`).
//! `stage()` downloads a release into the admin-only `<data>\update\<tag>\` folder and verifies it
//! (SHA-256, then `--version`); `install_now()` hands the verified exe to `--install --quiet`.
//! `init`/`sync_timer`/`on_timer` drive the daily check (`TIMER_UPDATE_CHECK` on the dock window);
//! `on_idle` installs a staged release when `update_auto_install` is on and nothing is open.

use std::io::Read;
use std::os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle};
use std::os::windows::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, RecvTimeoutError};
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

use windows::core::PCWSTR;
use windows::Win32::Foundation::{HANDLE, HWND, WAIT_OBJECT_0};
use windows::Win32::System::JobObjects::{
    AssignProcessToJobObject, CreateJobObjectW, JobObjectExtendedLimitInformation, SetInformationJobObject,
    JOBOBJECT_EXTENDED_LIMIT_INFORMATION, JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
};
use windows::Win32::System::Threading::{GetExitCodeProcess, WaitForSingleObject, CREATE_NO_WINDOW};
use windows::Win32::UI::WindowsAndMessaging::{KillTimer, PostMessageW, SetTimer};

use crate::config::{self, Config};
use crate::datadir;
use crate::msg::{TIMER_UPDATE_CHECK, WM_DOCK_UPDATE};
use crate::shell_thread::{self, ShellRequest};
use crate::update_release::{
    self, host_allowed, is_newer, next_check_delay_ms, parse_release, parse_sha256_file, parse_tag, release_page_url,
    rfc3339_utc, safe_release_url, version_output_matches, Asset, Release, Version, API_MAX_BYTES, CHECK_PERIOD_MS, EXE_ASSET,
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
    CouldNotCheck(CheckFail),
    DownloadFailed,
    VerifyFailed,
    Ready,
    InstallFailed(i32),
}

/// Why a check gave no answer about the latest release.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CheckFail {
    /// GitHub answered with this HTTP status.
    Http(u16),
    /// No answer (DNS, TLS, timeout, oversized body).
    NoConnection,
    /// A 200 that is not a usable release (foreign host, captive portal, assets not uploaded yet).
    Invalid,
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
            Outcome::CouldNotCheck(CheckFail::Http(c)) => pick(
                format!("Could not check for updates (HTTP {c})"),
                format!("Impossibile cercare aggiornamenti (HTTP {c})"),
            ),
            Outcome::CouldNotCheck(CheckFail::NoConnection) => pick(
                "Could not check for updates (no connection)".into(),
                "Impossibile cercare aggiornamenti (nessuna connessione)".into(),
            ),
            Outcome::CouldNotCheck(CheckFail::Invalid) => pick(
                "Could not check for updates (unexpected answer)".into(),
                "Impossibile cercare aggiornamenti (risposta non valida)".into(),
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
        Err(CheckError::Http(c)) => st.outcome = Outcome::CouldNotCheck(CheckFail::Http(*c)),
        Err(CheckError::Network(_)) => st.outcome = Outcome::CouldNotCheck(CheckFail::NoConnection),
        Err(CheckError::Invalid(_)) => st.outcome = Outcome::CouldNotCheck(CheckFail::Invalid),
    }
}

/// Headers of the latest-release request, the same in the dock and in the `--update-fetch` child.
const CHECK_HEADERS: [(&str, &str); 2] =
    [("Accept", "application/vnd.github+json"), ("X-GitHub-Api-Version", "2022-11-28")];

/// Asks GitHub for the latest release. `in_child` (the dock's checks): the GET runs in an `--update-fetch`
/// child, so the WinHTTP, Schannel and crypt32 state it loads (about +100 handles and +0.6 MB, kept for
/// good) ends with the child (M1 F2); a child that cannot start, times out or prints anything
/// `decode_fetch` refuses is logged and the GET runs here, as before. A network error the child reports
/// is the answer, with no second try. The host, status and release checks run here on either path, so
/// the child's bytes are trusted exactly as much as the network's.
fn fetch_latest(in_child: bool) -> Result<Release, CheckError> {
    let from_child = if in_child { fetch_in_child() } else { None };
    let fetched = match from_child {
        Some(r) => r,
        None => winhttp::get(LATEST_API_URL, &CHECK_HEADERS, API_MAX_BYTES).map_err(|e| e.to_string()),
    };
    let resp = fetched.map_err(CheckError::Network)?;
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

/// The flag of the child mode that makes the check's GET (`run_fetch_child`); not combinable.
pub(crate) const FETCH_FLAG: &str = "--update-fetch";
/// First line of the child's output; the 1 is the format's version.
const FETCH_HEADER: &[u8] = b"mini-oled-dock update-fetch 1\n";
/// Second line of the child's output when the GET failed.
const FETCH_ERROR_LINE: &[u8] = b"error";
/// Longest final URL the dock accepts from the child.
const FETCH_URL_MAX: usize = 2048;
/// Longest error text the child writes and the dock accepts.
const FETCH_ERROR_MAX: usize = 512;
/// The most the dock reads back from the child: the body's cap plus room for the three lines before it.
const FETCH_MAX_BYTES: u64 = API_MAX_BYTES + 4096;
/// The child waits on WinHTTP's own timeouts (10 + 10 + 30 + 30 s); 100 s cover them.
const FETCH_TIMEOUT: Duration = Duration::from_secs(100);
/// Exit code of `--update-fetch` with a response.
const FETCH_EXIT_RESPONSE: i32 = 0;
/// Exit code of `--update-fetch` with a network error.
const FETCH_EXIT_ERROR: i32 = 2;

/// The check's GET in an `--update-fetch` child (`run_capture`: no window, the dock's job, output
/// capped). None = no usable answer, the reason logged: the caller asks in-process.
fn fetch_in_child() -> Option<Result<winhttp::Response, String>> {
    let answer = std::env::current_exe()
        .map_err(|e| format!("no exe path: {e}"))
        .and_then(|exe| run_capture(&exe, &[FETCH_FLAG], FETCH_TIMEOUT, FETCH_MAX_BYTES))
        .and_then(|(code, out)| decode_fetch(code, out));
    match answer {
        Ok(r) => Some(r),
        Err(why) => {
            crate::log!("update: check process failed ({why}), checking in-process");
            None
        }
    }
}

/// `--update-fetch`, in the child `fetch_latest(true)` starts: the check's one GET, framed by
/// `encode_fetch` on stdout. It touches no data folder, logger, mutex or console, and writes nothing
/// but stdout. Exit 0 = a response, 2 = a network error, 1 = stdout could not be written.
pub fn run_fetch_child() -> i32 {
    use std::io::Write;
    let (code, out) =
        encode_fetch(&winhttp::get(LATEST_API_URL, &CHECK_HEADERS, API_MAX_BYTES).map_err(|e| e.to_string()));
    let mut stdout = std::io::stdout().lock();
    match stdout.write_all(&out).and_then(|()| stdout.flush()) {
        Ok(()) => code,
        Err(_) => 1,
    }
}

/// The child's output for `decode_fetch`, as (exit code, stdout). A response is `FETCH_HEADER`, the
/// status and the final URL, each on its own line, then the body's bytes (exit 0); a network error is
/// `FETCH_HEADER`, `error` and the error text on one line, control characters as spaces and at most
/// 512 bytes (exit 2).
pub(crate) fn encode_fetch(r: &Result<winhttp::Response, String>) -> (i32, Vec<u8>) {
    let mut out = FETCH_HEADER.to_vec();
    match r {
        Ok(resp) => {
            let lines = format!("{}\n{}\n", resp.status, resp.final_url);
            out.reserve(lines.len() + resp.body.len());
            out.extend_from_slice(lines.as_bytes());
            out.extend_from_slice(&resp.body);
            (FETCH_EXIT_RESPONSE, out)
        }
        Err(e) => {
            let mut text = String::with_capacity(e.len().min(FETCH_ERROR_MAX));
            for c in e.chars().map(|c| if c.is_control() { ' ' } else { c }) {
                if text.len() + c.len_utf8() > FETCH_ERROR_MAX {
                    break;
                }
                text.push(c);
            }
            out.extend_from_slice(FETCH_ERROR_LINE);
            out.push(b'\n');
            out.extend_from_slice(text.as_bytes());
            out.push(b'\n');
            (FETCH_EXIT_ERROR, out)
        }
    }
}

/// The line before the first LF and everything after it; None without an LF.
fn split_line(b: &[u8]) -> Option<(&[u8], &[u8])> {
    let lf = b.iter().position(|&c| c == b'\n')?;
    Some((b.get(..lf)?, b.get(lf + 1..)?))
}

/// Reads what an `--update-fetch` child printed, strictly (the elevated dock trusts none of it until
/// this agrees): `Ok(Ok(response))`, `Ok(Err(the child's network error))`, or `Err(why the output is
/// malformed)`, after which the caller checks in-process. The rules: the header exactly; a status of 3
/// digits from 100 to 599; a final URL of 1 to 2048 bytes of visible ASCII; a body of at most
/// `API_MAX_BYTES`; an error text of at most 512 bytes of UTF-8 without control characters, one line
/// and nothing after it. The exit code must agree with the frame (0 a response, 2 an error), so a child
/// that died while writing never passes a cut body off as a whole one. The output's buffer becomes the
/// body, with no second copy.
#[deny(clippy::unwrap_used, clippy::expect_used, clippy::indexing_slicing)]
pub(crate) fn decode_fetch(code: i32, mut out: Vec<u8>) -> Result<Result<winhttp::Response, String>, String> {
    let rest = out.strip_prefix(FETCH_HEADER).ok_or("no update-fetch header")?;
    let (first, after) = split_line(rest).ok_or("the second line has no end")?;
    if first == FETCH_ERROR_LINE {
        if code != FETCH_EXIT_ERROR {
            return Err(format!("an error with exit code {code}"));
        }
        let (text, tail) = split_line(after).ok_or("the error line has no end")?;
        if !tail.is_empty() {
            return Err("bytes after the error line".into());
        }
        if text.len() > FETCH_ERROR_MAX {
            return Err("an error text over 512 bytes".into());
        }
        let text = std::str::from_utf8(text).map_err(|_| "an error text that is not UTF-8")?;
        if text.chars().any(char::is_control) {
            return Err("a control character in the error text".into());
        }
        return Ok(Err(text.to_owned()));
    }
    if code != FETCH_EXIT_RESPONSE {
        return Err(format!("a response with exit code {code}"));
    }
    let status = match first {
        [a, b, c] if first.iter().all(u8::is_ascii_digit) => {
            u16::from(a - b'0') * 100 + u16::from(b - b'0') * 10 + u16::from(c - b'0')
        }
        _ => return Err("a status that is not 3 digits".into()),
    };
    if !(100..=599).contains(&status) {
        return Err(format!("status {status}"));
    }
    let (url, body) = split_line(after).ok_or("the URL line has no end")?;
    if url.is_empty() || url.len() > FETCH_URL_MAX || !url.iter().all(u8::is_ascii_graphic) {
        return Err("a final URL that is empty, over 2048 bytes or not visible ASCII".into());
    }
    if body.len() as u64 > API_MAX_BYTES {
        return Err(format!("a body over {API_MAX_BYTES} bytes"));
    }
    let final_url = String::from_utf8_lossy(url).into_owned();
    let body_start = out.len() - body.len();
    out.drain(..body_start);
    Ok(Ok(winhttp::Response { status, body: out, final_url }))
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
        // A check or a download frees buffers the heap keeps committed: the next hidden period trims again.
        crate::mem::rearm();
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
    let r = fetch_latest(true);
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

/// Opens the release page (or the latest-release page) de-elevated through the shell thread; any
/// URL that is not a release page of this project is refused.
pub fn open_release_page() {
    let url = release_page_url(status().available.as_ref().map(|a| a.tag.as_str()));
    if safe_release_url(&url) {
        shell_thread::request(ShellRequest::Launch { target: url, args: None });
    } else {
        crate::log!("update: release page URL refused: {url}");
    }
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

/// How often `run_capture` looks at its child.
const CAPTURE_POLL: Duration = Duration::from_millis(50);
/// How long `run_capture` waits for the end of a child's output once the child has exited.
const READER_GRACE: Duration = Duration::from_secs(2);

/// Runs `exe args` without a window, through `spawn_in_job`, and returns its exit code and stdout. The
/// output is read on a short-lived `capture` thread while this one waits, so a child that writes more
/// than a pipe buffer never blocks; at most `cap` bytes are kept. Every failure is an `Err`, with the
/// child killed and reaped when it still runs: output over `cap` ("output larger than <cap> bytes"), no
/// exit within `timeout` ("timeout"), an output still open 2 s after the exit. stdin and stderr are
/// null: nothing is ever written to a child, so it can never block this thread on its input.
#[deny(clippy::unwrap_used, clippy::expect_used, clippy::indexing_slicing)]
pub(crate) fn run_capture(exe: &Path, args: &[&str], timeout: Duration, cap: u64) -> Result<(i32, Vec<u8>), String> {
    let mut cmd = Command::new(exe);
    cmd.args(args).creation_flags(CREATE_NO_WINDOW.0).stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::null());
    let mut child = spawn_in_job(&mut cmd).map_err(|e| format!("cannot start {}: {e}", exe.display()))?;
    let Some(stdout) = child.stdout.take() else {
        return Err(stop(&mut child, "no output pipe".into()));
    };
    let (tx, rx) = mpsc::channel();
    let reader = std::thread::Builder::new().name("capture".into()).stack_size(64 * 1024).spawn(move || {
        let mut out = Vec::new();
        // One byte over the cap tells "too large" from "exactly the cap".
        let read = stdout.take(cap.saturating_add(1)).read_to_end(&mut out).map(|_| out);
        let _ = tx.send(read);
    });
    if let Err(e) = reader {
        return Err(stop(&mut child, format!("cannot start the reader thread: {e}")));
    }
    let started = Instant::now();
    let mut output: Option<std::io::Result<Vec<u8>>> = None;
    let code = loop {
        match &output {
            Some(Ok(out)) if out.len() as u64 > cap => {
                return Err(stop(&mut child, format!("output larger than {cap} bytes")))
            }
            Some(Err(e)) => return Err(stop(&mut child, format!("reading the output of {}: {e}", exe.display()))),
            _ => {}
        }
        match child.try_wait() {
            Ok(Some(st)) => break st.code().unwrap_or(-1),
            Ok(None) if started.elapsed() > timeout => return Err(stop(&mut child, "timeout".into())),
            Ok(None) => {}
            Err(e) => return Err(stop(&mut child, format!("waiting for {} failed: {e}", exe.display()))),
        }
        if output.is_some() {
            std::thread::sleep(CAPTURE_POLL);
            continue;
        }
        match rx.recv_timeout(CAPTURE_POLL) {
            Ok(read) => output = Some(read),
            Err(RecvTimeoutError::Timeout) => {}
            Err(RecvTimeoutError::Disconnected) => output = Some(Err(std::io::Error::other("the reader thread ended"))),
        }
    };
    let output = match output {
        Some(read) => read,
        // Only something the child started can still hold the pipe; the reader is left to it.
        None => rx.recv_timeout(READER_GRACE).map_err(|_| {
            format!("the output of {} was still open {} s after it exited", exe.display(), READER_GRACE.as_secs())
        })?,
    };
    let out = output.map_err(|e| format!("reading the output of {}: {e}", exe.display()))?;
    if out.len() as u64 > cap {
        return Err(format!("output larger than {cap} bytes"));
    }
    Ok((code, out))
}

/// Kills and reaps `child` (harmless when it has already exited) and hands `why` back.
fn stop(child: &mut Child, why: String) -> String {
    let _ = child.kill();
    let _ = child.wait();
    why
}

/// The dock's job object (house rule 7 of plan L3): every child `spawn_in_job` starts joins it, so none
/// outlives the dock, however it ends (a crash, the watchdog's restart, the installer's stop), and none
/// holds the exe while an update replaces it. Created at the first spawn with
/// `JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE` and no security attributes, so its handle is not inherited and
/// only the dock holds it; kept until the process ends, when Windows closes it and kills what is left.
/// `None` = it could not be created (logged once): children then run outside it, as before.
static JOB: OnceLock<Option<OwnedHandle>> = OnceLock::new();

fn create_job() -> Option<OwnedHandle> {
    // SAFETY: no attributes and no name; the handle returned is ours to own.
    let job = match unsafe { CreateJobObjectW(None, PCWSTR::null()) } {
        // SAFETY: a fresh handle, owned by nothing else.
        Ok(h) => unsafe { OwnedHandle::from_raw_handle(h.0) },
        Err(e) => {
            crate::log!("children: cannot create the dock's job ({e}); child processes run outside it");
            return None;
        }
    };
    let mut info = JOBOBJECT_EXTENDED_LIMIT_INFORMATION::default();
    info.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
    let size = std::mem::size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32;
    // SAFETY: `job` is a valid job handle; `info` is readable and `size` bytes long.
    let set = unsafe {
        SetInformationJobObject(
            HANDLE(job.as_raw_handle()),
            JobObjectExtendedLimitInformation,
            std::ptr::from_ref(&info).cast(),
            size,
        )
    };
    if let Err(e) = set {
        crate::log!("children: cannot set the dock's job to kill on close ({e}); child processes run outside it");
        return None;
    }
    Some(job)
}

/// The dock's job, once a child has been started through `spawn_in_job` and the job could be made.
#[cfg(test)]
fn dock_job() -> Option<HANDLE> {
    JOB.get().and_then(Option::as_ref).map(|j| HANDLE(j.as_raw_handle()))
}

/// Spawns `cmd` and puts the child into the dock's job (created at the first call). For short-lived
/// children whose work belongs to the dock (`run_capture`'s, the icon child); never for the watchdog,
/// an update's installer or anything the user launches, which must outlive the dock. A failed
/// assignment is logged once and the child runs outside the job.
pub(crate) fn spawn_in_job(cmd: &mut Command) -> std::io::Result<Child> {
    let child = cmd.spawn()?;
    if let Some(job) = JOB.get_or_init(create_job) {
        // SAFETY: the job's handle is valid until the process ends, the child's until `child` is dropped.
        let joined = unsafe { AssignProcessToJobObject(HANDLE(job.as_raw_handle()), HANDLE(child.as_raw_handle())) };
        if let Err(e) = joined {
            static WARNED: AtomicBool = AtomicBool::new(false);
            if !WARNED.swap(true, Ordering::Relaxed) {
                crate::log!("children: a child process could not join the dock's job ({e}); it runs outside it");
            }
        }
    }
    Ok(child)
}

/// Downloads `dock.exe` and `dock.exe.sha256` of `rel` into `<data>\update\<tag>\`. Each asset must
/// come back with 200, from an allowed host, at exactly the size the release JSON announced. Unlike the
/// check, this stays in-process: it runs only when an install follows (auto-install or "Install now"),
/// and the installer replaces this dock, so the WinHTTP state it loads lives only until that restart.
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
/// `expected` = the asset sizes of a freshly fetched release (`None` only for `--update-install`,
/// which installs what is staged): a stage of another upload of the same tag is deleted and is
/// `Download`, so the caller fetches the current assets.
///
/// A missing file is `Download` (the caller downloads); any other failure deletes the folder and
/// is `Verify`.
pub(crate) fn verify_stage(tag: &str, expected: Option<(u64, u64)>) -> Result<PathBuf, StageError> {
    let Some(version) = parse_tag(tag) else {
        return Err(StageError::Verify(format!("{tag:?} is not a canonical tag")));
    };
    let read = |name: &str, cap: u64| datadir::read_stage_file(tag, name, cap);
    let files = match (read(EXE_ASSET, EXE_MAX_BYTES), read(SHA_ASSET, SHA_MAX_BYTES)) {
        (Ok(exe), Ok(sha)) if !stage_sizes_match(&exe, &sha, expected) => {
            crate::log!("update: the stage of {tag} does not match the release assets, downloading again");
            if let Err(e) = datadir::remove_update_stage(tag) {
                crate::log!("update: cannot remove the stage {tag}: {e}");
            }
            return Err(StageError::Download("stale stage".into()));
        }
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
        let (code, out) = run_capture(&path, &["--version"], VERSION_TIMEOUT, CAPTURE_MAX_BYTES)?;
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

/// true when the staged files have the sizes `expected` (exe, sha256) announces, or no sizes are
/// expected.
fn stage_sizes_match(exe: &[u8], sha: &[u8], expected: Option<(u64, u64)>) -> bool {
    expected.is_none_or(|(e, s)| exe.len() as u64 == e && sha.len() as u64 == s)
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
    let sizes = Some((rel.exe.size, rel.sha256.size));
    let mut r = verify_stage(&rel.tag, sizes);
    if matches!(r, Err(StageError::Download(_))) {
        set_phase(Phase::Downloading);
        r = download(rel).and_then(|()| {
            set_phase(Phase::Verifying);
            verify_stage(&rel.tag, sizes)
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
    let path = match verify_stage(tag, None) {
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
    // Run from the installed dock.exe, waiting would keep that image mapped: the installer's rename
    // over it fails and `exe_in_use_by_other` counts this process as a holder, so the install could
    // never succeed. Let it run on its own; it logs its outcome.
    let from_installed = match (std::env::current_exe(), crate::autostart::installed_exe()) {
        (Ok(me), Ok(dest)) => crate::autostart::same_file(&me, &dest),
        _ => false,
    };
    if from_installed {
        drop(child);
        say("Installer started; see dock.log.");
        return 0;
    }
    match wait_exit(&child, INSTALL_WAIT) {
        Some(0) => {
            say(&format!("Installed {tag}."));
            0
        }
        Some(code) => {
            say(&format!("Installer exited with {code}."));
            cli_code(code)
        }
        None => {
            say(&format!("The installer did not finish within {} s.", INSTALL_WAIT.as_secs()));
            1
        }
    }
}

/// Process exit code for the installer's `code`: 0..=255 is kept, anything else (an NTSTATUS from a
/// crash such as 0xC0000005, or a value above 255) becomes 1, so it is never clamped to 0. Pure.
fn cli_code(code: i32) -> i32 {
    if (0..=255).contains(&code) {
        code
    } else {
        1
    }
}

/// `--update-check`: one synchronous check, no config write. Exit 0 = answered, 1 = could not check.
pub fn run_check_cli() -> i32 {
    let cur = Version::current();
    // In-process: this is already a short-lived process, a child would only double the work.
    match fetch_latest(false) {
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
    if crate::popup::is_open() {
        crate::popup::tick_soon(10);
    }
}

/// A verified release waits for a quiet moment and the user asked for automatic installs.
pub fn auto_install_pending() -> bool {
    if !config::get().update_auto_install {
        return false;
    }
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

/// From `TIMER_HOUSEKEEPING` when `auto_install_pending()`; `busy` = a popup or the Settings window is
/// open, or a fullscreen app has the foreground.
pub fn on_idle(busy: bool) {
    let st = status();
    if !auto_install_due(config::get().update_auto_install, st.staged, st.phase, busy) {
        return;
    }
    let Some(a) = st.available else { return };
    if install_now() {
        *AUTO_TRIED.lock().unwrap_or_else(|e| e.into_inner()) = a.tag.clone();
        crate::log!("update: auto-install {}", a.tag);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::update_release::{parse_release, Release, Version, RELEASE_FIXTURE};

    #[test]
    fn cli_code_never_turns_a_failure_into_success() {
        assert_eq!(cli_code(0), 0);
        assert_eq!(cli_code(3), 3);
        assert_eq!(cli_code(255), 255);
        assert_eq!(cli_code(256), 1);
        assert_eq!(cli_code(-1073741819), 1); // 0xC0000005
    }

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
        assert_eq!(s.outcome, Outcome::CouldNotCheck(CheckFail::Http(403)));
        apply_check(&mut s, &Err(CheckError::Network("timeout".into())), cur, 1_794_300_000);
        assert_eq!(s.outcome, Outcome::CouldNotCheck(CheckFail::NoConnection));
        apply_check(&mut s, &Err(CheckError::Invalid("no dock.exe asset".into())), cur, 1_794_300_000);
        assert_eq!(s.outcome, Outcome::CouldNotCheck(CheckFail::Invalid));
        if !crate::i18n::italian() {
            assert_eq!(s.line(), "Could not check for updates (unexpected answer)");
        }
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

    #[test]
    fn a_stage_must_match_the_fetched_asset_sizes() {
        assert!(stage_sizes_match(b"abc", b"0123", Some((3, 4))));
        assert!(stage_sizes_match(b"abc", b"0123", None)); // --update-install: staged only
        assert!(!stage_sizes_match(b"abcd", b"0123", Some((3, 4)))); // re-uploaded exe
        assert!(!stage_sizes_match(b"abc", b"012", Some((3, 4)))); // re-uploaded .sha256
    }

    fn system32(exe: &str) -> PathBuf {
        PathBuf::from(std::env::var_os("SystemRoot").unwrap()).join("System32").join(exe)
    }

    #[test]
    fn run_capture_reads_stdout_and_times_out() {
        let echo = ["/c", "echo mini-oled-dock 1.4.1"];
        let (code, out) = run_capture(&system32("cmd.exe"), &echo, Duration::from_secs(5), CAPTURE_MAX_BYTES).unwrap();
        assert_eq!(code, 0);
        assert!(crate::update_release::version_output_matches(&out, Version::parse("1.4.1").unwrap()));
        let t = Instant::now();
        let ping = ["-n", "6", "127.0.0.1"];
        let r = run_capture(&system32("ping.exe"), &ping, Duration::from_millis(300), CAPTURE_MAX_BYTES);
        assert_eq!(r.err().as_deref(), Some("timeout"));
        assert!(t.elapsed() < Duration::from_secs(3)); // killed, not waited for
    }

    fn fetched(status: u16, url: &str, body: &[u8]) -> Result<winhttp::Response, String> {
        Ok(winhttp::Response { status, body: body.to_vec(), final_url: url.into() })
    }

    /// What an `--update-fetch` child frames, the dock reads back unchanged.
    #[test]
    fn fetch_output_round_trip() {
        let url = "https://api.github.com/repos/ops-software/mini-oled-dock/releases/latest";
        let binary: &[u8] = b"line\nnext\r\n\0\xff";
        for (status, body) in [(200, RELEASE_FIXTURE.as_bytes()), (403, &b""[..]), (200, binary)] {
            let (code, out) = encode_fetch(&fetched(status, url, body));
            assert_eq!(code, 0);
            let Ok(r) = decode_fetch(code, out).unwrap() else { panic!("not a response") };
            assert_eq!((r.status, r.final_url.as_str(), r.body.as_slice()), (status, url, body));
        }
        let why = "WinHttpSendRequest: the server name or address could not be resolved";
        let (code, out) = encode_fetch(&Err(why.into()));
        assert_eq!(code, 2);
        assert_eq!(decode_fetch(code, out).unwrap().err().as_deref(), Some(why));
        // An error is one line of at most 512 bytes: control characters become spaces, the cut keeps whole characters.
        let (code, out) = encode_fetch(&Err(format!("a\r\nb\t{}", "\u{e9}".repeat(400))));
        let text = decode_fetch(code, out).unwrap().err().expect("an error");
        assert!(text.starts_with("a  b \u{e9}"), "{text:?}");
        assert!((509..=512).contains(&text.len()), "{}", text.len());
    }

    /// Anything but the exact frame is refused, so the dock checks in-process instead of trusting it.
    #[test]
    fn fetch_output_rejected() {
        let url = "https://api.github.com/x";
        let frame = |status: &str, url: &str, body: &[u8]| {
            let mut v = format!("mini-oled-dock update-fetch 1\n{status}\n{url}\n").into_bytes();
            v.extend_from_slice(body);
            v
        };
        let error = |text: &[u8]| [&b"mini-oled-dock update-fetch 1\nerror\n"[..], text].concat();
        let cap = API_MAX_BYTES as usize;
        let longest_url = format!("https://api.github.com/{}", "a".repeat(2048 - 23));
        // The limits themselves pass.
        assert!(decode_fetch(0, frame("100", url, b"")).is_ok());
        assert!(decode_fetch(0, frame("599", &longest_url, &vec![b' '; cap])).is_ok());
        assert!(decode_fetch(2, error(format!("{}\n", "a".repeat(512)).as_bytes())).is_ok());
        let bad: [(i32, Vec<u8>, &str); 26] = [
            (0, Vec::new(), "no output"),
            (0, b"200\nhttps://api.github.com/x\n{}".to_vec(), "no header"),
            (0, b"mini-oled-dock update-fetch 2\n200\nhttps://api.github.com/x\n{}".to_vec(), "another version"),
            (0, b"mini-oled-dock update-fetch 1".to_vec(), "a header without its LF"),
            (0, b"mini-oled-dock update-fetch 1\n200".to_vec(), "a status without its LF"),
            (0, frame("99", url, b""), "status 99"),
            (0, frame("600", url, b""), "status 600"),
            (0, frame("2O0", url, b""), "a status that is not a number"),
            (0, frame("+20", url, b""), "a signed status"),
            (0, frame("0200", url, b""), "a status of 4 digits"),
            (0, frame("200\r", url, b""), "a status with a CR"),
            (0, frame("200", "https://api.github.com/\u{1}x", b""), "a URL with a control character"),
            (0, frame("200", "https://api.github.com/a b", b""), "a URL with a space"),
            (0, frame("200", "https://api.github.com/\u{e9}", b""), "a URL that is not ASCII"),
            (0, frame("200", "", b""), "an empty URL"),
            (0, frame("200", &format!("{longest_url}a"), b""), "a URL over 2048 bytes"),
            (0, b"mini-oled-dock update-fetch 1\n200\nhttps://api.github.com/x".to_vec(), "a URL without its LF"),
            (0, frame("200", url, &vec![b' '; cap + 1]), "a body over the cap"),
            (2, frame("200", url, b"{}"), "a response with the error exit code"),
            (1, frame("200", url, b"{}"), "a response from a child that failed while writing"),
            (0, error(b"no route\n"), "an error with exit code 0"),
            (2, error(b"no route"), "an error without its LF"),
            (2, error(b"no route\nmore"), "bytes after the error line"),
            (2, error(b"no\troute\n"), "an error with a control character"),
            (2, error(format!("{}\n", "a".repeat(513)).as_bytes()), "an error over 512 bytes"),
            (2, error(b"\xff\n"), "an error that is not UTF-8"),
        ];
        for (code, out, case) in bad {
            assert!(decode_fetch(code, out).is_err(), "{case}");
        }
    }

    /// A child that writes more than a pipe buffer neither blocks nor loses a byte; one that writes more
    /// than the cap is stopped at the cap, not at the timeout (the error names the cap).
    #[test]
    fn run_capture_reads_more_than_a_pipe_buffer() {
        let dir = std::env::temp_dir().join(format!("dock_capture_test_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("big.txt");
        let line: &[u8] = b"0123456789 abcdefghijklmnopqrstuvwxyz ABCDEFGHIJKLMNOPQRSTUVWXYZ\r\n";
        let data: Vec<u8> = line.iter().copied().cycle().take(300_000).collect();
        std::fs::write(&path, &data).unwrap();
        let args = ["/c", "type", path.to_str().unwrap()];
        let whole = run_capture(&system32("cmd.exe"), &args, Duration::from_secs(60), 1024 * 1024);
        let capped = run_capture(&system32("cmd.exe"), &args, Duration::from_secs(60), 100_000);
        let _ = std::fs::remove_dir_all(&dir);
        let (code, out) = whole.unwrap();
        assert_eq!(code, 0);
        assert_eq!(out.len(), data.len());
        assert!(out == data, "the output differs from the file");
        assert_eq!(capped.err().as_deref(), Some("output larger than 100000 bytes"));
    }

    /// Every child of `spawn_in_job` is in the dock's job, which kills what is left when the dock ends.
    #[test]
    fn children_join_the_job() {
        use windows::core::BOOL;
        use windows::Win32::System::JobObjects::{
            IsProcessInJob, JobObjectExtendedLimitInformation, QueryInformationJobObject,
            JOBOBJECT_EXTENDED_LIMIT_INFORMATION, JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
        };
        let mut cmd = std::process::Command::new(system32("ping.exe"));
        cmd.args(["-n", "3", "127.0.0.1"])
            .creation_flags(CREATE_NO_WINDOW.0)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null());
        let mut child = spawn_in_job(&mut cmd).unwrap();
        let job = dock_job().expect("the job exists after a spawn");
        let mut inside = BOOL(0);
        // SAFETY: the child's handle is valid until `child` is dropped, the job's until the process ends.
        let asked = unsafe { IsProcessInJob(HANDLE(child.as_raw_handle()), Some(job), &mut inside) };
        let mut info = JOBOBJECT_EXTENDED_LIMIT_INFORMATION::default();
        let size = std::mem::size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32;
        // SAFETY: `info` is writable and `size` bytes long.
        let queried = unsafe {
            QueryInformationJobObject(
                Some(job),
                JobObjectExtendedLimitInformation,
                std::ptr::from_mut(&mut info).cast(),
                size,
                None,
            )
        };
        let _ = child.kill();
        let _ = child.wait();
        asked.unwrap();
        queried.unwrap();
        assert!(inside.as_bool());
        assert!(info.BasicLimitInformation.LimitFlags.contains(JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE));
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
