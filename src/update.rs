//! Self-updater state and check worker.
//!
//! `status()` is the snapshot the UI reads; `check_now()` runs one check on a throwaway
//! `update` thread (the UI thread never blocks on the network). Everything the worker learns is
//! kept in `STATE` and mirrored into `config.json` (`last_update_check`, `update_available`).
//! `stage()` downloads a release into the admin-only `<data>\update\<tag>\` folder and verifies it
//! (SHA-256, then `--version`); `install_now()` hands the verified release to the update guard (`guard`: a copy of
//! this exe that runs the release's installer, starts the new dock, judges it and puts this version back if it fails).
//! `init`/`sync_timer`/`on_timer` drive the daily check (`TIMER_UPDATE_CHECK` on the dock window);
//! `on_idle` installs a staged release when `update_auto_install` is on and nothing is open.

use std::os::windows::io::{AsRawHandle, OwnedHandle};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;
use std::time::Duration;

use windows::Win32::Foundation::{HANDLE, HWND, WAIT_OBJECT_0};
use windows::Win32::System::Threading::{GetExitCodeProcess, WaitForSingleObject};
use windows::Win32::UI::WindowsAndMessaging::{KillTimer, PostMessageW, SetTimer};

use crate::config::{self, Config};
use crate::datadir;
use crate::i18n::{k, t};
use crate::msg::{TIMER_UPDATE_CHECK, WM_DOCK_UPDATE};
use crate::shell_thread::{self, ShellRequest};
use crate::update_release::{
    self, host_allowed, is_newer, next_check_delay_ms, parse_release, parse_sha256_file, parse_tag, release_page_url,
    rfc3339_utc, safe_release_url, tag_api_url, version_output_matches, Asset, Release, Version, API_MAX_BYTES,
    CHECK_PERIOD_MS, EXE_ASSET, EXE_MAX_BYTES, FIRST_CHECK_MS, LATEST_API_URL, RETRY_MS, SHA_ASSET, SHA_MAX_BYTES,
};
use crate::winhttp;
use oled_sys::i18n::{subst, S};
use oled_sys::worker::bound::trusted_exe;
pub(crate) use oled_sys::worker::bound::{spawn_in_job, JobChild};

pub(crate) mod guard;
pub(crate) mod suite;

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
    /// The available release was installed, failed and was rolled back (`update\<tag>\rolled-back`
    /// exists): not installed automatically again; Update now retries it.
    RolledBack,
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
        let say = |key: S, args: &[(&str, &str)]| subst(t(key), args);
        let tag = self.available.as_ref().map(|a| a.tag.as_str()).unwrap_or("");
        let v = self.available.as_ref().map(|a| a.version.to_string()).unwrap_or_default();
        match self.phase {
            Phase::Checking => return say(k::UPDATE_CHECKING, &[]),
            Phase::Downloading => return say(k::UPDATE_DOWNLOADING, &[("tag", tag)]),
            Phase::Verifying => return say(k::UPDATE_VERIFYING, &[]),
            Phase::Installing => return say(k::UPDATE_INSTALLING, &[("tag", tag)]),
            Phase::Idle => {}
        }
        match &self.outcome {
            Outcome::None => say(k::UPDATE_NOT_CHECKED_YET, &[]),
            Outcome::UpToDate => say(k::UPDATE_UP_TO_DATE, &[]),
            Outcome::Available => say(k::UPDATE_VERSION_AVAILABLE, &[("v", &v)]),
            Outcome::CouldNotCheck(CheckFail::Http(c)) => say(k::UPDATE_CHECK_HTTP, &[("c", &c.to_string())]),
            Outcome::CouldNotCheck(CheckFail::NoConnection) => say(k::UPDATE_CHECK_NO_CONNECTION, &[]),
            Outcome::CouldNotCheck(CheckFail::Invalid) => say(k::UPDATE_CHECK_INVALID, &[]),
            Outcome::DownloadFailed => say(k::UPDATE_DOWNLOAD_FAILED, &[]),
            Outcome::VerifyFailed => say(k::UPDATE_VERIFY_FAILED, &[]),
            Outcome::Ready => say(k::UPDATE_READY, &[("v", &v)]),
            Outcome::InstallFailed(c) => say(k::UPDATE_INSTALL_FAILED, &[("c", &c.to_string())]),
            Outcome::RolledBack => say(k::UPDATE_ROLLED_BACK, &[("v", &v)]),
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

/// Folds a finished check into the status. A failed check changes only `outcome`. `rolled_back`: the
/// newer release found has the rolled-back mark; it stays available and is shown as rolled back.
pub(crate) fn apply_check(
    st: &mut UpdateStatus,
    r: &Result<Release, CheckError>,
    installed: Version,
    now: u64,
    rolled_back: bool,
) {
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
            st.outcome = match (rolled_back, st.staged) {
                (true, _) => Outcome::RolledBack,
                (false, true) => Outcome::Ready,
                (false, false) => Outcome::Available,
            };
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

/// Asks GitHub for the latest release (`fetch_release` without a tag).
fn fetch_latest(in_child: bool) -> Result<Release, CheckError> {
    fetch_release(None, in_child)
}

/// Asks GitHub for the latest release, or (`tag`, U1: the suite's own release) for the release of one canonical
/// tag, which must then be the release that comes back. `in_child` (the dock's checks): the GET runs in an
/// `--update-fetch` child, so the WinHTTP, Schannel and crypt32 state it loads (about +100 handles and +0.6 MB, kept
/// for good) ends with the child (M1 F2); a child that cannot start, times out or prints anything `decode_fetch`
/// refuses is logged and the GET runs here, as before. A network error the child reports is the answer, with no
/// second try. The host, status and release checks run here on either path, so the child's bytes are trusted
/// exactly as much as the network's. A suite asset the release parser left out is logged by name.
pub(crate) fn fetch_release(tag: Option<&str>, in_child: bool) -> Result<Release, CheckError> {
    let url = match tag {
        None => LATEST_API_URL.to_string(),
        Some(t) => tag_api_url(t).ok_or_else(|| CheckError::Invalid("not a canonical tag".into()))?,
    };
    let from_child = if in_child { fetch_in_child(tag) } else { None };
    let fetched = match from_child {
        Some(r) => r,
        None => winhttp::get(&url, &CHECK_HEADERS, API_MAX_BYTES).map_err(|e| e.to_string()),
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
    let rel = parse_release(&resp.body).map_err(CheckError::Invalid)?;
    if tag.is_some_and(|t| t != rel.tag) {
        return Err(CheckError::Invalid("the release of another tag".into()));
    }
    for name in &rel.dropped {
        crate::log!("update: release asset {name} left out (unexpected URL or size)");
    }
    Ok(rel)
}

/// The flag of the child mode that makes the check's GET (`run_fetch_child_for`); combines only with a tag.
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

/// The check's GET (or, with `tag`, that release's GET) in an `--update-fetch [<tag>]` child (`run_capture`: no
/// window, the dock's job, output capped). None = no usable answer, the reason logged once per process (house rule
/// 7 of plan L3): the caller asks in-process, every time.
fn fetch_in_child(tag: Option<&str>) -> Option<Result<winhttp::Response, String>> {
    let args: Vec<&str> = std::iter::once(FETCH_FLAG).chain(tag).collect();
    let answer = child_exe()
        .and_then(|exe| run_capture(&exe, &args, FETCH_TIMEOUT, FETCH_MAX_BYTES))
        .and_then(|(code, out)| decode_fetch(code, out));
    match answer {
        Ok(r) => Some(r),
        Err(why) => {
            static WARNED: AtomicBool = AtomicBool::new(false);
            if !WARNED.swap(true, Ordering::Relaxed) {
                crate::log!("update: check process failed ({why}), checking in-process");
            }
            None
        }
    }
}

/// `--update-fetch [<tag>]`, in the child `fetch_release(.., true)` starts: one GET, of the latest release (no
/// tag: the update check) or of the release of a canonical tag (U1b: the suite's own release), framed by
/// `encode_fetch` on stdout. It touches no data folder, logger, mutex or console, and writes nothing but stdout.
/// Exit 0 = a response, 2 = a network error or a tag that is not canonical (refused before any GET), 1 = stdout
/// could not be written.
pub fn run_fetch_child_for(tag: Option<&str>) -> i32 {
    let mut stdout = std::io::stdout().lock();
    fetch_child(tag, |url| winhttp::get(url, &CHECK_HEADERS, API_MAX_BYTES).map_err(|e| e.to_string()), &mut stdout)
}

/// `run_fetch_child_for` with its GET and its stdout as parameters (tests).
fn fetch_child(
    tag: Option<&str>,
    get: impl FnOnce(&str) -> Result<winhttp::Response, String>,
    out: &mut dyn std::io::Write,
) -> i32 {
    let answer = match tag {
        None => get(LATEST_API_URL),
        Some(t) => match tag_api_url(t) {
            Some(url) => get(&url),
            None => Err("not a canonical tag".to_string()),
        },
    };
    let (code, bytes) = encode_fetch(&answer);
    match out.write_all(&bytes).and_then(|()| out.flush()) {
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

/// True while an update operation runs on the worker (a check, a download, an install and, from U1e, the suite's
/// sync): the Safety page reads it before it offers a rollback (P3-dock-suite R2).
#[allow(dead_code)] // DS-SAFE reads it
pub(crate) fn busy() -> bool {
    BUSY.load(Ordering::Acquire)
}

/// Runs `job` on a thread named `update`. false = another update operation is already running.
fn spawn_worker(job: impl FnOnce() + Send + 'static) -> bool {
    if BUSY.compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire).is_err() {
        return false;
    }
    let spawned = std::thread::Builder::new().name("update".into()).spawn(move || {
        probe!(crate::probe::bump(crate::probe::C::Spawned));
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

/// Starts a check on the worker thread, then the suite's sync when the check answered. false = an update operation is
/// already running.
pub fn check_now() -> bool {
    check_and_sync(false)
}

/// `check_now`'s job: one check, the automatic stage, then `--suite-sync --network` (U1e: it also retries the
/// components still pending), after an answered check or, `sync_anyway` (the start's first timer fire), after any.
fn check_and_sync(sync_anyway: bool) -> bool {
    spawn_worker(move || {
        let answered = check_once();
        // With `update_auto_install` on, a newer release is downloaded and verified right away, so
        // the idle gate (`on_idle`) only has to start the installer. With it off nothing is downloaded,
        // and a rolled-back release is never staged, so `on_idle` never installs it.
        let latest = LATEST.lock().unwrap_or_else(|e| e.into_inner()).clone();
        let skip = |tag: &str| {
            let rb = rolled_back(tag);
            if rb {
                crate::log!("update: {tag} was rolled back, not installed automatically");
            }
            rb
        };
        let auto = config::get().update_auto_install;
        if let Some(rel) = auto_stage_target(latest.as_ref(), Version::current(), auto, skip) {
            let _ = stage(&rel);
        }
        if answered || sync_anyway {
            run_suite_sync(true);
        }
    })
}

/// One check, on the worker thread: asks GitHub, folds the answer into the status and the config. true = GitHub
/// answered with a release.
fn check_once() -> bool {
    with_state(|s| s.phase = Phase::Checking);
    notify();
    let installed = Version::current();
    let r = fetch_latest(true);
    let rb = matches!(&r, Ok(rel) if rel.version > installed && rolled_back(&rel.tag));
    match &r {
        Ok(rel) if rel.version > installed => crate::log!("update: {} available", rel.tag),
        Ok(rel) => crate::log!("update: up to date (latest {})", rel.tag),
        Err(e) => crate::log!("update: could not check ({e})"),
    }
    if let Ok(rel) = &r {
        *LATEST.lock().unwrap_or_else(|e| e.into_inner()) = is_newer(&rel.tag, installed).then(|| rel.clone());
    }
    let (last, tag) = with_state(|s| {
        apply_check(s, &r, installed, now_unix(), rb);
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
    r.is_ok()
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

/// Largest `--version` output read back from a staged (or installed) exe.
pub(crate) const CAPTURE_MAX_BYTES: u64 = 4096;
/// How long a staged exe may take to answer `--version`.
const VERSION_TIMEOUT: Duration = Duration::from_secs(5);

/// `exe` must hash to the value published in the release's `.sha256` asset and, when GitHub sent one,
/// to `digest`, the SHA-256 GitHub computed for the uploaded asset (served by api.github.com, not by the
/// host that serves both files). Hashes once and returns the hash.
pub(crate) fn verify_bytes(exe: &[u8], sha_file: &[u8], digest: Option<[u8; 32]>) -> Result<[u8; 32], String> {
    let want = parse_sha256_file(sha_file).ok_or("unreadable .sha256 file")?;
    let got = crate::sha256::sha256(exe)?;
    if got != want {
        return Err("SHA-256 mismatch".into());
    }
    if digest.is_some_and(|d| d != got) {
        return Err("SHA-256 differs from GitHub's digest of the asset".into());
    }
    Ok(got)
}

/// What a freshly fetched release announces: the sizes of `dock.exe` and `dock.exe.sha256`, and GitHub's
/// SHA-256 digest of `dock.exe` when it sent one.
pub(crate) struct Expected {
    pub exe: u64,
    pub sha: u64,
    pub digest: Option<[u8; 32]>,
}

/// The wake-probe build's thread counter (L-BUD's rows), called by oled-sys first thing in every capture thread.
#[cfg(feature = "wake-probe")]
fn on_thread() {
    probe!(crate::probe::bump(crate::probe::C::Spawned));
}

/// `oled_sys::worker::bound::run_capture` with the dock's thread hook registered first in the wake-probe build (the
/// first call sets it, the later ones find it set); without the feature there is nothing to count and no hook.
pub(crate) fn run_capture(exe: &Path, args: &[&str], timeout: Duration, cap: u64) -> Result<(i32, Vec<u8>), String> {
    probe!(oled_sys::worker::bound::set_on_thread(on_thread););
    oled_sys::worker::bound::run_capture(exe, args, timeout, cap)
}

/// The exe of every child the dock starts again (`--update-fetch`, `--start-apps`, `--start-settings`, `--start-icons`):
/// `std::env::current_exe()` (house rule 7 of plan L3), when it is safe to run with the dock's token. A running exe can be
/// renamed and another file put at its path, which `current_exe` still names; so an elevated dock (or one whose token
/// cannot be read) starts itself only from the installed copy or from a folder `datadir` accepts as admin-only. Err
/// (say, a dock.exe started from Downloads): the caller logs it once and does the work in-process, as when a child
/// cannot start.
pub(crate) fn child_exe() -> Result<PathBuf, String> {
    let exe = std::env::current_exe().map_err(|e| format!("no exe path: {e}"))?;
    let installed = crate::autostart::installed_exe().ok();
    let elevated = shell_thread::elevation() != Some(false);
    trusted_exe(exe, elevated, installed.as_deref(), datadir::is_admin_only_dir)
}

/// Downloads `dock.exe` and `dock.exe.sha256` of `rel` into `<data>\update\<tag>\`. Each asset must
/// come back with 200, from an allowed host, at exactly the size the release JSON announced. Unlike the
/// check, this stays in-process: it runs only when an install follows (auto-install or "Install now"),
/// and the installer replaces this dock, so the WinHTTP state it loads lives only until that restart.
fn download(rel: &Release) -> Result<(), StageError> {
    let fail = StageError::Download;
    for (asset, name, cap) in [(&rel.exe, EXE_ASSET, EXE_MAX_BYTES), (&rel.sha256, SHA_ASSET, SHA_MAX_BYTES)] {
        let body = get_asset(asset, cap).map_err(|e| fail(format!("{name}: {e}")))?;
        datadir::write_stage_file(&rel.tag, name, &body).map_err(|e| fail(format!("{name}: {e}")))?;
        // `body` is dropped here, before the next asset is fetched.
    }
    crate::log!("update: {} downloaded", rel.tag);
    Ok(())
}

/// One release asset by GET: it must come back with 200, from an allowed host, at exactly the size the release JSON
/// announced (at most `cap`). `download` and the suite's downloads (U1e) both fetch through it.
pub(crate) fn get_asset(asset: &Asset, cap: u64) -> Result<Vec<u8>, String> {
    let Asset { url, size, .. } = asset;
    let resp = winhttp::get(url, &[("Accept", "application/octet-stream")], cap).map_err(|e| e.to_string())?;
    if resp.status != 200 {
        return Err(format!("HTTP {}", resp.status));
    }
    if !winhttp::split_https_url(&resp.final_url).is_some_and(|(h, _)| host_allowed(&h)) {
        return Err("unexpected host".into());
    }
    if resp.body.len() as u64 != *size {
        return Err(format!("{} bytes, expected {size}", resp.body.len()));
    }
    Ok(resp.body)
}

/// Checks the staged `<data>\update\<tag>\dock.exe` and returns its path: hash equal to the staged
/// `.sha256` (and to GitHub's digest, when `expected` has one), then `dock.exe --version` must print exactly `mini-oled-dock <tag version>`.
///
/// Running the staged exe for `--version` is safe: its SHA-256 matched the `.sha256` of the same
/// release, both fetched over HTTPS from the allowed GitHub hosts; `datadir` has checked that the
/// folder is admin-only and not a reparse point and that the file is a single-link regular file
/// inside it; the folder was created by this elevated process, so no user can swap the file after
/// the check.
///
/// `expected` = the asset sizes and GitHub's digest of a freshly fetched release (`None` only for
/// `--update-install`, which installs what is staged): a stage of another upload of the same tag is
/// deleted and is `Download`, so the caller fetches the current assets; an exe whose hash differs from
/// the digest fails verification.
///
/// A missing file is `Download` (the caller downloads); any other failure deletes the folder and
/// is `Verify`.
pub(crate) fn verify_stage(tag: &str, expected: Option<&Expected>) -> Result<PathBuf, StageError> {
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
        verify_bytes(&exe, &sha, expected.and_then(|e| e.digest))?;
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

/// true when the staged files have the sizes `expected` announces, or no sizes are expected.
fn stage_sizes_match(exe: &[u8], sha: &[u8], expected: Option<&Expected>) -> bool {
    expected.is_none_or(|e| exe.len() as u64 == e.exe && sha.len() as u64 == e.sha)
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
    let expected = Expected { exe: rel.exe.size, sha: rel.sha256.size, digest: rel.exe.digest };
    if expected.digest.is_none() {
        crate::log!("update: GitHub sent no digest for {}/{EXE_ASSET}; checked against {SHA_ASSET} only", rel.tag);
    }
    let mut r = verify_stage(&rel.tag, Some(&expected));
    if matches!(r, Err(StageError::Download(_))) {
        set_phase(Phase::Downloading);
        r = download(rel).and_then(|()| {
            set_phase(Phase::Verifying);
            verify_stage(&rel.tag, Some(&expected))
        });
    }
    match &r {
        Ok(p) => crate::log!("update: {} staged at {}", rel.tag, p.display()),
        Err(e) => crate::log!("update: {} not staged ({e})", rel.tag),
    }
    if r.is_ok() {
        stage_suite(rel);
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

/// The release's suite.json and the components whose files differ from the installed ones, staged beside its dock.exe
/// (U1e), so the sync after the update reads the stage, not the network. A failure only costs that sync a download.
fn stage_suite(rel: &Release) {
    let Some(dir) = crate::autostart::installed_exe().ok().and_then(|e| e.parent().map(Path::to_path_buf)) else {
        return;
    };
    let mut at = suite::DataStage { tag: rel.tag.clone() };
    match suite::stage_components(rel, &dir, &mut suite::RealNet, &mut at) {
        Ok(0) => {}
        Ok(n) => crate::log!("update: {}: {n} suite files staged", rel.tag),
        Err(e) => crate::log!("update: {}: suite files not staged ({e})", rel.tag),
    }
}

/// How long the dock waits for the update guard, and the guard (or `--rollback`) for the installer it starts.
const INSTALL_WAIT: Duration = Duration::from_secs(120);

/// Why the update guard was not started (`arm_guard`).
#[derive(Debug)]
pub(crate) enum ArmError {
    /// `CreateProcessW` of the guard failed.
    Spawn(String),
    /// This process is not the installed `dock.exe`: the guard must be a copy of the installed old version.
    NotInstalledCopy,
    /// The guard's copy in the stage could not be made or checked.
    Stage(String),
}

impl ArmError {
    /// The code `Outcome::InstallFailed` shows: negative, never one of the guard's own exit codes.
    fn code(&self) -> i32 {
        match self {
            ArmError::Spawn(_) => -1,
            ArmError::NotInstalledCopy => -2,
            ArmError::Stage(_) => -3,
        }
    }
}

impl std::fmt::Display for ArmError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ArmError::Spawn(e) => write!(f, "cannot start the update guard: {e}"),
            ArmError::NotInstalledCopy => write!(f, "this is not the installed dock.exe"),
            ArmError::Stage(e) => write!(f, "cannot stage the update guard: {e}"),
        }
    }
}

/// Starts the update guard for the verified stage `tag`: a copy of this exe, which must be the installed old version,
/// as `update\<tag>\dock-guard.exe --update-guard <tag>` (`guard::run_cli`). The kernel copies the file
/// (`datadir::copy_into_stage`), so this process's heap never holds the image. Started by `spawn_breakaway`, never by
/// `spawn_in_job`: the guard must outlive this dock, which the release's installer stops.
fn arm_guard(tag: &str) -> Result<OwnedHandle, ArmError> {
    let me = std::env::current_exe().map_err(|_| ArmError::NotInstalledCopy)?;
    if !crate::autostart::installed_exe().is_ok_and(|dest| crate::autostart::same_file(&me, &dest)) {
        return Err(ArmError::NotInstalledCopy);
    }
    let name = update_release::GUARD_FILE;
    datadir::copy_into_stage(tag, name, &me).map_err(ArmError::Stage)?;
    let path = datadir::checked_stage_path(tag, name).map_err(|e| ArmError::Stage(e.to_string()))?;
    let child = crate::watchdog::spawn_breakaway(&path, &guard::guard_args(tag)).map_err(ArmError::Spawn)?;
    crate::log!("update: {tag}: update guard started");
    Ok(child)
}

/// true when an automatic install leaves the release alone: it was rolled back. Update now (`manual`) retries it. Pure.
fn skip_install(manual: bool, rolled_back: bool) -> bool {
    !manual && rolled_back
}

/// The release to install: only a freshly fetched one (`LATEST`), and only if newer than
/// `installed`. A badge restored from `config.json` alone never installs anything.
pub(crate) fn install_target(latest: Option<&Release>, installed: Version) -> Option<Release> {
    latest.filter(|r| r.version > installed).cloned()
}

/// The release a check stages for the automatic install: the freshly fetched newer one, only with
/// `update_auto_install` on, and never one `rolled_back` says was rolled back. Pure.
pub(crate) fn auto_stage_target(
    latest: Option<&Release>,
    installed: Version,
    auto_install: bool,
    rolled_back: impl Fn(&str) -> bool,
) -> Option<Release> {
    if !auto_install {
        return None;
    }
    install_target(latest, installed).filter(|r| !rolled_back(&r.tag))
}

/// true when `update\<tag>\rolled-back` exists in the admin-only stage folder (the update guard rolled
/// `<tag>` back). Reads the disk: never on the UI thread.
fn rolled_back(tag: &str) -> bool {
    datadir::checked_stage_path(tag, update_release::ROLLED_BACK_FILE).is_ok()
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

/// Update now (Settings): installs the newest release, a rolled-back one too (`start_install`). false = an update
/// operation is already running.
pub fn install_now() -> bool {
    start_install(true)
}

/// The automatic install (`on_idle`): as `install_now`, but never a release that was rolled back.
fn install_auto() -> bool {
    start_install(false)
}

/// Installs the newest release on the worker thread: check if nothing fresh is known, stage (download +
/// verify), then hand the verified release to the update guard (`arm_guard`). The guard's installer stops this
/// dock through its normal stop path (`WM_DOCK_QUIT_REQUEST`, cleanup, taskbar restored, exit 0). `manual` = Update
/// now. false = an update operation is already running.
fn start_install(manual: bool) -> bool {
    spawn_worker(move || {
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
        if skip_install(manual, rolled_back(&rel.tag)) {
            crate::log!("update: {} was rolled back, not installed automatically", rel.tag);
            return;
        }
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
        let child = match arm_guard(&rel.tag) {
            Ok(c) => c,
            Err(e) => {
                crate::log!("update: {}: {e}", rel.tag);
                return failed(e.code());
            }
        };
        match wait_exit(&child, INSTALL_WAIT) {
            Some(0) => {
                crate::log!("update: the update guard finished but the dock is still running");
                set_phase(Phase::Idle);
            }
            Some(code) => {
                crate::log!("update: the update guard exited with {code}; the stage {} is kept", rel.tag);
                failed(code);
            }
            None => {
                crate::log!("update: the update guard did not finish within {} s", INSTALL_WAIT.as_secs());
                set_phase(Phase::Idle);
            }
        }
    })
}

/// Deletes staging folders of canonical tags at or below the running version (failed installs and
/// the version now running). Run once at startup on a thread named `update-cleanup`; the installer
/// that just finished may still run from the current version's folder, so a failure is only logged
/// and the next start retries. The running version's stage stays while it holds a component not yet in the install
/// folder (U1e): the sync installs it from there and removes the stage after an "ok".
fn cleanup_stages() {
    if !datadir::writable() {
        return;
    }
    let current = Version::current();
    let stale = update_release::stale_stages(&datadir::update_stage_names(), current);
    for tag in removable_stages(stale, current, suite::own_stage_pending) {
        match datadir::remove_update_stage(&tag) {
            Ok(()) => crate::log!("update: removed the old stage {tag}"),
            Err(e) => crate::log!("update: cannot remove the old stage {tag}: {e}"),
        }
    }
}

/// The stale stages `cleanup_stages` deletes: all of them but the running version's while `pending` says it still
/// holds a component to install (asked only for that one). Pure but for `pending`.
pub(crate) fn removable_stages(stale: Vec<String>, current: Version, pending: impl Fn(&str) -> bool) -> Vec<String> {
    let own = current.tag();
    stale.into_iter().filter(|t| *t != own || !pending(t)).collect()
}

/// `--update-install <tag>`: hands an already staged release to the update guard (`arm_guard`). The only input is a
/// canonical tag, mapped to `<data>\update\<tag>\`; there is no path argument. Runs only from the installed
/// `dock.exe` (the guard is a copy of it) and exits at once: that image must not stay mapped for the install's swap.
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
    if let Err(e) = verify_stage(tag, None) {
        say(&format!("{tag}: {e}"));
        return 1;
    }
    // Waiting would keep the installed image mapped: the installer's rename over it fails and `exe_in_use_by_other`
    // counts this process as a holder. The guard runs on its own and logs its outcome.
    match arm_guard(tag) {
        Ok(child) => {
            drop(child);
            say(&format!("Update guard started for {tag}; see dock.log."));
            0
        }
        Err(ArmError::NotInstalledCopy) => {
            say(r"run the installed dock.exe (%ProgramFiles%\mini-oled-dock\dock.exe)");
            1
        }
        Err(e) => {
            say(&e.to_string());
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
/// true once this start's first suite sync was handed to the worker (U1e): at the first `TIMER_UPDATE_CHECK` fire,
/// alone or after the check. With `update_check` off the timer then ends.
static SYNC_STARTED: AtomicBool = AtomicBool::new(false);
/// true while the check that is running was started by the timer (not by the user).
static SCHEDULED: AtomicBool = AtomicBool::new(false);
/// The tag `on_idle` last tried to install: one automatic attempt per release, so a failing
/// installer is not restarted every 2 s.
static AUTO_TRIED: Mutex<String> = Mutex::new(String::new());

/// Dock start: status from the (now loaded) config, old stages cleaned, timer armed.
pub fn init(h: HWND) {
    let st = status_from_config(&config::get(), Version::current());
    with_state(|s| *s = st);
    let spawned = std::thread::Builder::new().name("update-cleanup".into()).spawn(|| {
        probe!(crate::probe::bump(crate::probe::C::Spawned));
        cleanup_stages();
        suite::remember_installed();
        // A release rolled back before this start says so again (its stage, above this version, is kept).
        let Some(tag) = status().available.map(|a| a.tag).filter(|t| rolled_back(t)) else {
            return;
        };
        let set = with_state(|s| {
            let idle = s.phase == Phase::Idle && s.available.as_ref().is_some_and(|a| a.tag == tag);
            if idle {
                s.outcome = Outcome::RolledBack;
            }
            idle
        });
        if set {
            notify();
        }
    });
    if let Err(e) = spawned {
        crate::log!("update: cannot start the cleanup thread: {e}");
    }
    sync_timer(h);
}

/// Whether `TIMER_UPDATE_CHECK` runs: for the daily check, or until this start's first suite sync was handed to the
/// worker (U1e), so with `update_check` off the timer fires once, 60 s after the start, then stops. Pure.
pub(crate) fn timer_wanted(update_check: bool, sync_started: bool) -> bool {
    update_check || !sync_started
}

/// What one `TIMER_UPDATE_CHECK` fire does: the check when `update_check` is on and it is due; the suite's sync
/// (Some(network)) after that check, or alone at the start's first fire, downloading only when `update_check` is on.
/// Pure.
pub(crate) fn timer_plan(first_fire: bool, check_due: bool, update_check: bool) -> (bool, Option<bool>) {
    let check = update_check && check_due;
    let sync = if check {
        Some(true)
    } else if first_fire {
        Some(update_check)
    } else {
        None
    };
    (check, sync)
}

/// Arms or kills the check timer according to `update_check` (and the start's first suite sync, `timer_wanted`).
pub fn sync_timer(h: HWND) {
    let on = config::get().update_check;
    let wanted = timer_wanted(on, SYNC_STARTED.load(Ordering::Relaxed));
    if wanted && !ARMED.load(Ordering::Relaxed) {
        // SAFETY: `h` is the dock window, owned by the calling (UI) thread.
        if unsafe { SetTimer(Some(h), TIMER_UPDATE_CHECK, FIRST_CHECK_MS, None) } == 0 {
            crate::log!("update: SetTimer failed");
            return;
        }
        ARMED.store(true, Ordering::Relaxed);
        if on {
            crate::log!("update: automatic check on");
        }
    } else if !wanted && ARMED.load(Ordering::Relaxed) {
        // SAFETY: our own id/window; fails harmlessly if the timer does not exist.
        let _ = unsafe { KillTimer(Some(h), TIMER_UPDATE_CHECK) };
        ARMED.store(false, Ordering::Relaxed);
        crate::log!("update: automatic check off");
    }
}

/// `TIMER_UPDATE_CHECK`: checks when 24 h have passed since the last check, else waits for the rest; at the start's
/// first fire the suite's sync runs too (`timer_plan`), after the check or alone, on the worker (under BUSY, so it
/// never overlaps a check, a download or an install). A sync the busy worker refused is tried again a minute later.
pub fn on_timer(h: HWND) {
    let c = config::get();
    let last = update_release::parse_rfc3339_utc(&c.last_update_check);
    let delay = next_check_delay_ms(now_unix(), last);
    let first = !SYNC_STARTED.load(Ordering::Relaxed);
    let (check, sync) = timer_plan(first, delay == 0, c.update_check);
    if check {
        SCHEDULED.store(true, Ordering::Relaxed);
        if check_and_sync(first) {
            SYNC_STARTED.store(true, Ordering::Relaxed);
        } else {
            SCHEDULED.store(false, Ordering::Relaxed);
        }
    } else if let Some(network) = sync {
        if spawn_worker(move || run_suite_sync(network)) {
            SYNC_STARTED.store(true, Ordering::Relaxed);
        }
    }
    let started = SYNC_STARTED.load(Ordering::Relaxed);
    if !timer_wanted(c.update_check, started) {
        // SAFETY: our own id/window; fails harmlessly if the timer does not exist.
        let _ = unsafe { KillTimer(Some(h), TIMER_UPDATE_CHECK) };
        ARMED.store(false, Ordering::Relaxed);
        return;
    }
    let next = if !started {
        FIRST_CHECK_MS
    } else if delay == 0 {
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

/// The sync child's time limit: three selftests of 15 s, the release's fetch and the downloads fit inside it.
const SYNC_TIMEOUT: Duration = Duration::from_secs(300);

/// `dock.exe --suite-sync [--network]` (U1e) in the dock's kill-on-close job, on the worker thread: its result line is
/// logged and kept for `suite::status`, the entry points follow a changed component (I1b), and the UI is told
/// (P3-dock-suite R3). A dock that exits for an update ends the child; update.jnl's replay finishes its job later.
fn run_suite_sync(network: bool) {
    let args: &[&str] = if network { &["--suite-sync", "--network"] } else { &["--suite-sync"] };
    let line = match child_exe().and_then(|exe| run_capture(&exe, args, SYNC_TIMEOUT, CAPTURE_MAX_BYTES)) {
        Ok((code, out)) => suite::result_line(code, &out),
        Err(e) => format!("{}error (the sync process: {e})", suite::RESULT_PREFIX),
    };
    crate::log!("{line}");
    if suite::remember(&line) {
        crate::entry_points::request_sync();
    }
    notify();
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
    if install_auto() {
        *AUTO_TRIED.lock().unwrap_or_else(|e| e.into_inner()) = a.tag.clone();
        crate::log!("update: auto-install {}", a.tag);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::update_release::{parse_release, Release, Version, RELEASE_FIXTURE};
    use std::time::Instant;

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
        apply_check(&mut s, &Ok(rel.clone()), cur, 1_794_227_696, false);
        assert_eq!((s.outcome.clone(), s.badge()), (Outcome::Available, 1));
        assert_eq!(s.available.as_ref().unwrap().date, "2026-11-20");
        assert_eq!(s.last_check, "2026-11-09T12:34:56Z");
        apply_check(&mut s, &Err(CheckError::Http(403)), cur, 1_794_300_000, false);
        assert_eq!(s.outcome, Outcome::CouldNotCheck(CheckFail::Http(403)));
        apply_check(&mut s, &Err(CheckError::Network("timeout".into())), cur, 1_794_300_000, false);
        assert_eq!(s.outcome, Outcome::CouldNotCheck(CheckFail::NoConnection));
        apply_check(&mut s, &Err(CheckError::Invalid("no dock.exe asset".into())), cur, 1_794_300_000, false);
        assert_eq!(s.outcome, Outcome::CouldNotCheck(CheckFail::Invalid));
        if !crate::i18n::italian() {
            assert_eq!(s.line(), "Could not check for updates (unexpected answer)");
        }
        // a failed check changes nothing else
        assert_eq!((s.badge(), s.last_check.as_str()), (1, "2026-11-09T12:34:56Z"));
        // the staged tag survives a re-check of the same tag, not a newer one
        s.staged = true;
        apply_check(&mut s, &Ok(rel.clone()), cur, 1_794_300_000, false);
        assert_eq!((s.staged, s.outcome.clone()), (true, Outcome::Ready));
        let newer = Release { tag: "v1.4.9".into(), version: Version::parse("1.4.9").unwrap(), ..rel.clone() };
        apply_check(&mut s, &Ok(newer), cur, 1_794_300_000, false);
        assert_eq!((s.staged, s.outcome.clone(), s.badge()), (false, Outcome::Available, 1));
        let same = Release { tag: "v1.4.0".into(), version: cur, ..rel };
        apply_check(&mut s, &Ok(same), cur, 1_794_300_000, false);
        assert_eq!((s.outcome.clone(), s.badge()), (Outcome::UpToDate, 0));
    }

    #[test]
    fn every_status_line_equals_the_old_text_in_both_languages() {
        let mut s = status_from_config(&Config::default(), Version::parse("1.5.0").unwrap());
        let version = Version::parse("1.5.3").unwrap();
        s.available = Some(Available { tag: version.tag(), version, date: String::new(), notes: String::new() });
        let (tag, v) = ("v1.5.3", "1.5.3");
        // (phase, outcome, English, Italian) as the format! calls built them before the keyed strings.
        let phases = [
            (Phase::Checking, "Checking for updates...".to_string(), "Ricerca aggiornamenti...".to_string()),
            (Phase::Downloading, format!("Downloading {tag}..."), format!("Download di {tag}...")),
            (Phase::Verifying, "Verifying the download...".to_string(), "Verifica del download...".to_string()),
            (Phase::Installing, format!("Installing {tag}..."), format!("Installazione di {tag}...")),
        ];
        for (phase, en, it) in phases {
            s.phase = phase;
            assert_eq!(crate::i18n::with_italian(false, || s.line()), en);
            assert_eq!(crate::i18n::with_italian(true, || s.line()), it);
        }
        s.phase = Phase::Idle;
        let outcomes = [
            (Outcome::None, "Not checked yet".to_string(), "Non ancora verificato".to_string()),
            (Outcome::UpToDate, "Up to date".to_string(), "Aggiornato".to_string()),
            (Outcome::Available, format!("Version {v} available"), format!("Disponibile la versione {v}")),
            (
                Outcome::CouldNotCheck(CheckFail::Http(403)),
                "Could not check for updates (HTTP 403)".to_string(),
                "Impossibile cercare aggiornamenti (HTTP 403)".to_string(),
            ),
            (
                Outcome::CouldNotCheck(CheckFail::NoConnection),
                "Could not check for updates (no connection)".to_string(),
                "Impossibile cercare aggiornamenti (nessuna connessione)".to_string(),
            ),
            (
                Outcome::CouldNotCheck(CheckFail::Invalid),
                "Could not check for updates (unexpected answer)".to_string(),
                "Impossibile cercare aggiornamenti (risposta non valida)".to_string(),
            ),
            (Outcome::DownloadFailed, "Download failed".to_string(), "Download non riuscito".to_string()),
            (
                Outcome::VerifyFailed,
                "Download failed verification".to_string(),
                "Il download non ha superato la verifica".to_string(),
            ),
            (
                Outcome::Ready,
                format!("Version {v} downloaded and verified"),
                format!("Versione {v} scaricata e verificata"),
            ),
            (
                Outcome::InstallFailed(5),
                "Installation failed (exit code 5)".to_string(),
                "Installazione non riuscita (codice 5)".to_string(),
            ),
            (Outcome::RolledBack, format!("Update to {v} rolled back"), format!("Aggiornamento a {v} annullato")),
        ];
        for (outcome, en, it) in outcomes {
            s.outcome = outcome;
            assert_eq!(crate::i18n::with_italian(false, || s.line()), en);
            assert_eq!(crate::i18n::with_italian(true, || s.line()), it);
        }
    }

    #[test]
    fn rolled_back_release_line() {
        let mut s = status_from_config(&Config::default(), Version::parse("1.5.0").unwrap());
        let version = Version::parse("1.5.3").unwrap();
        s.available = Some(Available { tag: version.tag(), version, date: String::new(), notes: String::new() });
        s.outcome = Outcome::RolledBack;
        if !crate::i18n::italian() {
            assert_eq!(s.line(), "Update to 1.5.3 rolled back");
        }
    }

    #[test]
    fn a_check_keeps_a_rolled_back_release_visible() {
        let cur = Version::parse("1.4.0").unwrap();
        let rel = parse_release(RELEASE_FIXTURE.as_bytes()).unwrap(); // v1.4.1
        let mut s = status_from_config(&Config::default(), cur);
        apply_check(&mut s, &Ok(rel.clone()), cur, 1_794_227_696, true);
        assert_eq!((s.outcome.clone(), s.badge(), s.staged), (Outcome::RolledBack, 1, false));
        assert_eq!(s.available.as_ref().map(|a| a.tag.as_str()), Some("v1.4.1"));
        assert_eq!(s.last_check, "2026-11-09T12:34:56Z");
        // Nothing newer: up to date, whatever the mark says.
        let same = Release { tag: "v1.4.0".into(), version: cur, ..rel };
        apply_check(&mut s, &Ok(same), cur, 1_794_300_000, true);
        assert_eq!((s.outcome.clone(), s.badge()), (Outcome::UpToDate, 0));
    }

    #[test]
    fn auto_stage_skips_a_rolled_back_release() {
        let cur = Version::parse("1.4.0").unwrap();
        let rel = parse_release(RELEASE_FIXTURE.as_bytes()).unwrap(); // v1.4.1
        assert_eq!(auto_stage_target(Some(&rel), cur, true, |t| t == "v1.4.1"), None);
        assert_eq!(auto_stage_target(Some(&rel), cur, true, |_| false), Some(rel.clone()));
        assert_eq!(auto_stage_target(Some(&rel), cur, false, |_| false), None);
        assert_eq!(auto_stage_target(None, cur, true, |_| false), None);
        assert_eq!(auto_stage_target(Some(&rel), Version::parse("1.4.1").unwrap(), true, |_| false), None);
    }

    #[test]
    fn verify_bytes_compares_the_published_hash() {
        let sha = b"ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad  dock.exe\r\n";
        assert!(verify_bytes(b"abc", sha, None).is_ok());
        assert!(verify_bytes(b"abd", sha, None).is_err());
        assert!(verify_bytes(b"abc", b"not a hash", None).is_err());
    }

    #[test]
    fn verify_bytes_checks_the_github_digest() {
        let sha = b"ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad  dock.exe\r\n";
        let abc = parse_sha256_file(sha).unwrap();
        assert_eq!(verify_bytes(b"abc", sha, Some(abc)), Ok(abc));
        let mut other = abc;
        other[31] ^= 1;
        let e = verify_bytes(b"abc", sha, Some(other)).unwrap_err();
        assert!(e.contains("digest"), "{e}");
        // No digest: the .sha256 check alone, as before.
        assert_eq!(verify_bytes(b"abc", sha, None), Ok(abc));
        assert!(verify_bytes(b"abd", sha, None).unwrap_err().contains("SHA-256 mismatch"));
        // The .sha256 asset is checked first, whatever the digest says.
        assert!(verify_bytes(b"abd", sha, Some(abc)).unwrap_err().contains("SHA-256 mismatch"));
    }

    #[test]
    fn a_stage_must_match_the_fetched_asset_sizes() {
        let sizes = |exe, sha| Expected { exe, sha, digest: None };
        assert!(stage_sizes_match(b"abc", b"0123", Some(&sizes(3, 4))));
        assert!(stage_sizes_match(b"abc", b"0123", None)); // --update-install: staged only
        assert!(!stage_sizes_match(b"abcd", b"0123", Some(&sizes(3, 4)))); // re-uploaded exe
        assert!(!stage_sizes_match(b"abc", b"012", Some(&sizes(3, 4)))); // re-uploaded .sha256
    }

    fn system32(exe: &str) -> PathBuf {
        PathBuf::from(std::env::var_os("SystemRoot").unwrap()).join("System32").join(exe)
    }

    #[test]
    fn run_capture_reads_stdout_and_times_out() {
        let echo = ["/c", "echo mini-oled-dock 1.4.1"];
        let (code, out) = run_capture(&system32("cmd.exe"), &echo, Duration::from_secs(60), CAPTURE_MAX_BYTES).unwrap();
        assert_eq!(code, 0);
        assert!(crate::update_release::version_output_matches(&out, Version::parse("1.4.1").unwrap()));
        let t = Instant::now();
        let ping = ["-n", "61", "127.0.0.1"];
        let r = run_capture(&system32("ping.exe"), &ping, Duration::from_millis(300), CAPTURE_MAX_BYTES);
        assert_eq!(r.err().as_deref(), Some("timeout"));
        // Killed, not waited for: the ping would run for a minute.
        assert!(t.elapsed() < Duration::from_secs(30), "{:?}", t.elapsed());
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

    /// `--update-fetch <tag>` GETs that tag's release through the same framing; a tag that is not canonical is refused
    /// with exit 2 before any GET; no tag is the check's latest-release GET as before.
    #[test]
    fn fetch_child_takes_a_tag() {
        let run = |tag: Option<&str>| {
            let mut urls = Vec::new();
            let mut out = Vec::new();
            let code = fetch_child(
                tag,
                |url| {
                    urls.push(url.to_string());
                    fetched(200, url, b"{}")
                },
                &mut out,
            );
            (code, urls, out)
        };
        let (code, urls, out) = run(Some("v1.8.0"));
        let want = "https://api.github.com/repos/ops-software/mini-oled-dock/releases/tags/v1.8.0";
        assert_eq!((code, urls.as_slice()), (0, [want.to_string()].as_slice()));
        let Ok(r) = decode_fetch(code, out).unwrap() else { panic!("not a response") };
        assert_eq!((r.status, r.final_url.as_str(), r.body.as_slice()), (200, want, &b"{}"[..]));
        let (code, urls, _) = run(None);
        assert_eq!((code, urls.as_slice()), (0, [LATEST_API_URL.to_string()].as_slice()));
        for bad in ["v1.8", "1.8.0", "v1.8.0/../latest", "latest"] {
            let (code, urls, out) = run(Some(bad));
            assert_eq!(code, 2, "{bad}");
            assert!(urls.is_empty(), "{bad}: no GET");
            assert!(decode_fetch(code, out).unwrap().is_err(), "{bad}: framed as an error");
        }
        // A child whose stdout fails says so with exit 1.
        struct Broken;
        impl std::io::Write for Broken {
            fn write(&mut self, _: &[u8]) -> std::io::Result<usize> {
                Err(std::io::Error::other("closed"))
            }
            fn flush(&mut self) -> std::io::Result<()> {
                Ok(())
            }
        }
        assert_eq!(fetch_child(Some("v1.8.0"), |url| fetched(200, url, b"{}"), &mut Broken), 1);
    }

    /// A made-up 1.8.0 release in GitHub's format: dock.exe, its .sha256, suite.json and three components.
    #[test]
    fn release_with_suite_parses() {
        use oled_trust::manifest::suite::ComponentId;
        const JSON: &[u8] = include_bytes!("../tests/fixtures/update/v1.8.0-release.json");
        let rel = parse_release(JSON).unwrap();
        assert_eq!((rel.tag.as_str(), rel.version), ("v1.8.0", Version::parse("1.8.0").unwrap()));
        let hex = |h: &str| crate::update_release::parse_digest(Some(format!("sha256:{h}").as_str()));
        let suite = rel.suite.as_ref().expect("suite.json");
        let suite_url = update_release::asset_url("v1.8.0", "suite.json");
        assert_eq!((suite.url.as_str(), suite.size), (suite_url.as_str(), 1024));
        assert_eq!(suite.digest, hex("2c26b46b68ffc68ff99b453c1d30413413422d706483bfa0f98a5e886266e7ae"));
        let got: Vec<(ComponentId, u64, Option<[u8; 32]>)> =
            rel.component_assets.iter().map(|(c, a)| (*c, a.size, a.digest)).collect();
        let (h_tasks, h_files, h_lang) = (
            hex("e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"),
            hex("ffeeddccbbaa99887766554433221100ffeeddccbbaa99887766554433221100"),
            hex("0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef"),
        );
        assert_eq!(
            got,
            [
                (ComponentId::Tasks, 917_504, h_tasks),
                (ComponentId::Files, 1_048_576, h_files),
                (ComponentId::LangIt, 40_960, h_lang)
            ]
        );
        for (c, a) in &rel.component_assets {
            assert_eq!(a.url, update_release::asset_url("v1.8.0", c.asset()));
        }
        assert!(rel.dropped.is_empty());
        assert_eq!(rel.exe.size, 1_966_080);
    }

    /// Every release before the suite: no suite.json, no component, nothing dropped, dock.exe as before.
    #[test]
    fn release_without_suite_is_unchanged() {
        const JSON: &[u8] = include_bytes!("../tests/fixtures/update/v1.5.0-release.json");
        for json in [JSON, RELEASE_FIXTURE.as_bytes()] {
            let rel = parse_release(json).unwrap();
            assert_eq!(rel.suite, None);
            assert!(rel.component_assets.is_empty() && rel.dropped.is_empty());
        }
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

    #[test]
    fn sync_starts_at_first_check_ms_even_when_the_check_is_not_due() {
        // (first fire, check due, update_check) -> (check, sync with --network?)
        assert_eq!(timer_plan(true, false, true), (false, Some(true)), "first fire, checked today: the sync alone");
        assert_eq!(timer_plan(true, true, true), (true, Some(true)), "first fire, check due: the sync after the check");
        assert_eq!(timer_plan(true, false, false), (false, Some(false)), "checks off: the sync without the network");
        assert_eq!(timer_plan(true, true, false), (false, Some(false)), "checks off: never a check");
        assert_eq!(timer_plan(false, true, true), (true, Some(true)), "a later daily check: the sync after it");
        assert_eq!(timer_plan(false, false, true), (false, None));
        assert_eq!(timer_plan(false, false, false), (false, None));
        assert_eq!(timer_plan(false, true, false), (false, None));
        // The first fire is FIRST_CHECK_MS after the start: past the update guard's wait for the new dock (60 s).
        assert_eq!(FIRST_CHECK_MS, 60_000);
    }

    #[test]
    fn first_fire_is_armed_with_checks_off() {
        // (update_check, the start's first sync handed to the worker) -> the timer runs
        assert!(timer_wanted(false, false), "checks off: armed once, for the start's sync");
        assert!(!timer_wanted(false, true), "checks off, the sync started: no more wake-ups");
        assert!(timer_wanted(true, false));
        assert!(timer_wanted(true, true));
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
        assert_eq!(guard::guard_args("v1.4.1"), "--update-guard v1.4.1");
    }

    #[test]
    fn arm_errors_have_distinct_codes() {
        assert_eq!(ArmError::Spawn("x".into()).code(), -1);
        assert_eq!(ArmError::NotInstalledCopy.code(), -2);
        assert_eq!(ArmError::Stage("x".into()).code(), -3);
    }

    #[test]
    fn an_automatic_install_skips_a_rolled_back_release() {
        assert!(skip_install(false, true));
        assert!(!skip_install(true, true), "Update now retries a rolled-back release");
        assert!(!skip_install(false, false));
        assert!(!skip_install(true, false));
    }
}
