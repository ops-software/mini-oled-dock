//! Self-updater state and check worker.
//!
//! `status()` is the snapshot the UI reads; `check_now()` runs one check on a throwaway
//! `update` thread (the UI thread never blocks on the network). Everything the worker learns is
//! kept in `STATE` and mirrored into `config.json` (`last_update_check`, `update_available`).
//! The download, install handoff and timer are added by later tasks.

// Wired into the dock by later tasks; until then some items are unused.
#![allow(dead_code)]

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;

use windows::Win32::UI::WindowsAndMessaging::PostMessageW;

use crate::config::{self, Config};
use crate::msg::WM_DOCK_UPDATE;
use crate::shell_thread::{self, ShellRequest};
use crate::update_release::{
    self, host_allowed, is_newer, parse_release, parse_tag, release_page_url, rfc3339_utc, Release, Version,
    API_MAX_BYTES, LATEST_API_URL,
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
            st.available = Some(Available {
                tag: rel.tag.clone(),
                version: rel.version,
                date: rel.published.get(..10).unwrap_or("").to_string(),
                notes: rel.notes.clone(),
            });
            st.outcome = Outcome::Available;
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
    })
}

/// Opens the release page (or the latest-release page) de-elevated through the shell thread.
pub fn open_release_page() {
    let url = release_page_url(status().available.as_ref().map(|a| a.tag.as_str()));
    shell_thread::request(ShellRequest::Launch { target: url, args: None });
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
        let same = Release { tag: "v1.4.0".into(), version: cur, ..rel };
        apply_check(&mut s, &Ok(same), cur, 1_794_300_000);
        assert_eq!((s.outcome.clone(), s.badge()), (Outcome::UpToDate, 0));
    }
}
