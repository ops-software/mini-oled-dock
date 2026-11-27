//! The Safety page of Settings (dock-suite 2.6): the list of changes the suite made to Windows, "Undo all" and the
//! update rollback.
//!
//! Declared as a child module of `settings_pages` (`#[path]`).

use std::sync::atomic::{AtomicBool, AtomicIsize, Ordering};
use std::sync::Mutex;
use std::time::Duration;

use oled_sys::i18n::subst;
use windows::core::{w, PCWSTR};
use windows::Win32::Foundation::{HWND, SYSTEMTIME};
use windows::Win32::Globalization::{GetDateFormatEx, ENUM_DATE_FORMATS_FLAGS};
use windows::Win32::System::Time::SystemTimeToTzSpecificLocalTime;
use windows::Win32::UI::WindowsAndMessaging::PostMessageW;

use crate::config::Config;
use crate::glyphs;
use crate::hook::TakeoverTarget;
use crate::i18n::{k, t};
use crate::log;
use crate::msg::WM_SETTINGS_SUITE_DATA;
use crate::settings_pages::{buttons, header, row, Action, Button, Control, Ctx, Glyph, Row};
use crate::ui::incidents::{self, Incident, IncidentKind, INCIDENTS_MAX};
use crate::ui::suite_links::{self, App};
use crate::update::Phase;
use crate::update_release::{parse_tag, Version, ROLLED_BACK_FILE};
use oled_trust::journal::{EntryView, Hive, Kind, TakeoverId, Target, UndoReport};

/// A part of the Safety page that opens and closes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Disclose {
    UndoAll,
    Rollback,
}

/// Who made a change listed on the Safety page.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ChangeSource {
    Chord(TakeoverTarget),
    Machine(u64),
    User(u64),
}

/// One change the suite made to Windows.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct SystemChange {
    pub source: ChangeSource,
    pub label: String,
    pub detail: String,
}

/// The list of changes, as the worker reads it.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) enum ChangesState {
    #[default]
    Loading,
    Loaded(Vec<SystemChange>),
    Failed,
}

/// What the last "Undo all" did.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct UndoLine {
    pub restored: u32,
    pub other: u32,
    pub failed: u32,
    pub user_done: bool,
}

/// Everything the Safety page shows besides the config; the window keeps one for its whole life.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct SafetyView {
    pub journal: ChangesState,
    pub open: Option<Disclose>,
    pub report: Option<UndoLine>,
    pub busy: bool,
    pub prev_kept: bool,
    pub update_idle: bool,
    pub incidents: Vec<Incident>,
    pub sync_failed: bool,
    /// `ConfigError::log_line` of `config::errors()`.
    pub config_errors: Vec<String>,
    /// Rolled-back marks, from the worker.
    pub update_incidents: Vec<Incident>,
}

/// What a worker hands to the window.
pub(crate) struct SafetyData {
    pub journal: ChangesState,
    pub report: Option<UndoLine>,
    pub rolled_back: Vec<Incident>,
}

/// How many config problems the banner lists before "+n more".
const BANNER_LINES: usize = 5;
/// Longest change detail, in characters.
const DETAIL_MAX: usize = 260;
/// Stack of the two workers.
const WORKER_STACK: usize = 256 * 1024;
/// How long the agent has to answer for the user's journal.
const USER_WAIT: Duration = Duration::from_secs(3);

// ------------------------------------------------------------------ pure helpers

/// True when no update operation runs and the phase is idle (a rollback in the middle of one would break it).
pub(crate) fn update_idle(busy: bool, phase: Phase) -> bool {
    !busy && phase == Phase::Idle
}

/// The one rule of a rollback: a kept previous exe and an idle updater.
pub(crate) fn may_roll_back(prev_kept: bool, idle: bool) -> bool {
    prev_kept && idle
}

/// The changes the page lists: the chord (when its setting is on) then the journal's.
fn all_changes(chord_on: bool, journal: &ChangesState) -> Vec<SystemChange> {
    let mut v = Vec::new();
    if chord_on {
        v.push(SystemChange {
            source: ChangeSource::Chord(TakeoverTarget::Tasks),
            label: t(k::SAFETY_CHANGE_CHORD).to_string(),
            detail: subst(t(k::SUITE_CSE_LABEL), &[("app", suite_links::name(App::Tasks))]),
        });
    }
    if let ChangesState::Loaded(list) = journal {
        v.extend(list.iter().cloned());
    }
    v
}

/// "2 restored, 1 changed by another program, left as is": the non-zero parts, then the Explorer note.
fn report_line(r: &UndoLine) -> String {
    let n = |x: u32| x.to_string();
    let mut parts = Vec::new();
    if r.restored > 0 {
        parts.push(subst(t(k::SAFETY_REPORT_RESTORED), &[("n", &n(r.restored))]));
    }
    if r.other > 0 {
        parts.push(subst(t(k::SAFETY_REPORT_OTHER), &[("n", &n(r.other))]));
    }
    if r.failed > 0 {
        parts.push(subst(t(k::SAFETY_REPORT_FAILED), &[("n", &n(r.failed))]));
    }
    if parts.is_empty() && r.user_done {
        parts.push(subst(t(k::SAFETY_REPORT_RESTORED), &[("n", "0")]));
    }
    let mut line = parts.join(", ");
    if !r.user_done {
        if !line.is_empty() {
            line.push_str(". ");
        }
        line.push_str(t(k::SAFETY_REPORT_USER));
    }
    line
}

/// Both lists of incidents, newest first, at most `INCIDENTS_MAX`.
fn merged_incidents(memory: &[Incident], marks: &[Incident]) -> Vec<Incident> {
    let mut all: Vec<Incident> = memory.iter().chain(marks).copied().collect();
    all.sort_by(|a, b| b.at_unix.cmp(&a.at_unix));
    all.truncate(INCIDENTS_MAX);
    all
}

/// (year, month 1..=12, day, hour, minute) of a Unix time, in UTC (days to civil date: Hinnant's algorithm).
fn utc_parts(unix: u64) -> (u16, u16, u16, u16, u16) {
    let days = (unix / 86_400) as i64;
    let rem = unix % 86_400;
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = (doy - (153 * mp + 2) / 5 + 1) as u16;
    let month = if mp < 10 { mp + 3 } else { mp - 9 } as u16;
    let year = (yoe + era * 400 + i64::from(month <= 2)) as u16;
    (year, month, day, (rem / 3600) as u16, (rem % 3600 / 60) as u16)
}

/// The parts of a Unix time as shown: local time, or UTC while a preview renders the matrix and in unit tests.
fn shown_parts(unix: u64) -> (u16, u16, u16, u16, u16) {
    let (year, month, day, hour, minute) = utc_parts(unix);
    if cfg!(test) || crate::preview::matrix() {
        return (year, month, day, hour, minute);
    }
    let utc = SYSTEMTIME { wYear: year, wMonth: month, wDay: day, wHour: hour, wMinute: minute, ..Default::default() };
    let mut local = SYSTEMTIME::default();
    // SAFETY: null time zone = the current one; `utc` and `local` are valid for the call.
    if unsafe { SystemTimeToTzSpecificLocalTime(None, &utc, &mut local) }.is_err() {
        return (year, month, day, hour, minute);
    }
    (local.wYear, local.wMonth, local.wDay, local.wHour, local.wMinute)
}

/// "22 Nov": day and abbreviated month in the dock language (numeric when the system call fails).
fn date_text(unix: u64) -> String {
    let (year, month, day, ..) = shown_parts(unix);
    let date = SYSTEMTIME { wYear: year, wMonth: month, wDay: day, ..Default::default() };
    let mut buf = [0u16; 32];
    // SAFETY: `date` and `buf` are valid for the call; the picture is a static NUL-terminated string.
    let n = unsafe {
        GetDateFormatEx(
            crate::i18n::locale(),
            ENUM_DATE_FORMATS_FLAGS(0),
            Some(&date),
            w!("dd MMM"),
            Some(&mut buf),
            PCWSTR::null(),
        )
    };
    if n > 1 {
        String::from_utf16_lossy(&buf[..(n as usize - 1).min(buf.len())])
    } else {
        format!("{day:02}/{month:02}")
    }
}

/// "22 Nov 10:14".
fn stamp(unix: u64) -> String {
    let (.., h, min) = shown_parts(unix);
    format!("{} {h:02}:{min:02}", date_text(unix))
}

/// The line of one incident: "dd Mon HH:MM · text".
fn incident_line(i: &Incident) -> String {
    let text = match i.kind {
        IncidentKind::TakeoverFellBack => subst(t(k::INCIDENT_FELL_BACK), &[("app", suite_links::name(App::Tasks))]),
        IncidentKind::TakeoverPaused => t(k::INCIDENT_PAUSED).to_string(),
        IncidentKind::UpdateRolledBack(v) => subst(t(k::INCIDENT_ROLLED_BACK), &[("v", &v.to_string())]),
    };
    format!("{} \u{b7} {text}", stamp(i.at_unix))
}

/// The label of a journal kind (the kind string as the journal writes it).
fn label_of(kind: &str) -> String {
    match kind {
        "takeover.folder_verbs" => subst(t(k::SAFETY_CHANGE_FOLDERS), &[("app", suite_links::name(App::Files))]),
        _ => t(k::SAFETY_CHANGE_OTHER).to_string(),
    }
}

/// A recorded target as one line.
fn target_text(t: &Target) -> String {
    match t {
        Target::Reg { hive, key, name } => {
            let hive = match hive {
                Hive::Hkcu => "HKCU",
                Hive::Hklm => "HKLM",
            };
            match name {
                Some(n) => format!("{hive}\\{key} ({n})"),
                None => format!("{hive}\\{key}"),
            }
        }
        Target::Task(n) => n.clone(),
        Target::File(p) => p.clone(),
    }
}

fn change(source: ChangeSource, kind: &str, target: &str, ts: u64) -> SystemChange {
    let target: String = target.chars().take(DETAIL_MAX).collect();
    SystemChange { source, label: label_of(kind), detail: format!("{target} \u{b7} {}", date_text(ts)) }
}

/// The rolled-back marks as incidents: a name `parse_tag` refuses gives nothing.
fn marks_to_incidents(marks: &[(String, u64)]) -> Vec<Incident> {
    marks
        .iter()
        .filter_map(|(name, at)| {
            parse_tag(name).map(|v| Incident { at_unix: *at, kind: IncidentKind::UpdateRolledBack(v) })
        })
        .collect()
}

/// The undo's result as the line the page shows.
fn undo_line(result: Result<(UndoReport, bool), String>) -> UndoLine {
    match result {
        Ok((r, user_done)) => UndoLine {
            restored: r.restored + r.already,
            other: r.changed_by_other.len() as u32,
            failed: r.failed.len() as u32,
            user_done,
        },
        Err(why) => {
            log!("settings: undo all failed: {why}");
            UndoLine { failed: 1, user_done: false, ..Default::default() }
        }
    }
}

// ------------------------------------------------------------------ the sources of the worker

/// What the workers read and do; `Real` on the machine, a fake in the tests.
trait Reader {
    fn machine(&self) -> Vec<EntryView>;
    fn user(&self) -> Result<Vec<crate::agent_proto::JournalRec>, String>;
    fn undo(&mut self) -> Result<(UndoReport, bool), String>;
    /// (stage folder name, modified time in seconds since 1970) of each update that holds a rolled-back mark.
    fn rolled_back(&self) -> Vec<(String, u64)>;
}

struct Real;

impl Reader for Real {
    fn machine(&self) -> Vec<EntryView> {
        crate::journal_ui::machine_views()
    }
    fn user(&self) -> Result<Vec<crate::agent_proto::JournalRec>, String> {
        crate::journal_ui::user_views(USER_WAIT)
    }
    fn undo(&mut self) -> Result<(UndoReport, bool), String> {
        crate::journal_ui::undo_all_takeovers()
    }
    fn rolled_back(&self) -> Vec<(String, u64)> {
        crate::datadir::update_stage_names()
            .into_iter()
            .filter(|n| parse_tag(n).is_some())
            .filter_map(|n| {
                let mark = crate::datadir::checked_stage_path(&n, ROLLED_BACK_FILE).ok()?;
                let at = std::fs::metadata(mark)
                    .and_then(|m| m.modified())
                    .ok()
                    .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                    .map_or(0, |d| d.as_secs());
                Some((n, at))
            })
            .collect()
    }
}

/// Reads the list of changes and the rolled-back marks.
fn read_all(src: &dyn Reader) -> (ChangesState, Vec<Incident>) {
    let mut list = Vec::new();
    let machine = src.machine();
    let machine_empty = machine.is_empty();
    for e in machine {
        if let Kind::Takeover(TakeoverId::FolderVerbs) = e.kind {
            list.push(change(ChangeSource::Machine(e.seq), "takeover.folder_verbs", &target_text(&e.target), e.ts));
        }
    }
    let user_failed = match src.user() {
        Ok(recs) => {
            for r in recs.iter().filter(|r| r.kind.starts_with("takeover.")) {
                list.push(change(ChangeSource::User(r.seq), &r.kind, &r.target, r.ts));
            }
            false
        }
        Err(why) => {
            log!("settings: the user's journal was not read: {why}");
            true
        }
    };
    let journal = if user_failed && machine_empty { ChangesState::Failed } else { ChangesState::Loaded(list) };
    (journal, marks_to_incidents(&src.rolled_back()))
}

// ------------------------------------------------------------------ the workers

/// The data of the last finished worker, until the window takes it.
static DATA: Mutex<Option<SafetyData>> = Mutex::new(None);
/// True while a worker runs: one at a time.
static WORKER: AtomicBool = AtomicBool::new(false);

/// The right to run a worker; released when dropped.
struct Claim<'a>(&'a AtomicBool);

impl<'a> Claim<'a> {
    fn take(flag: &'a AtomicBool) -> Option<Claim<'a>> {
        flag.compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire).is_ok().then_some(Claim(flag))
    }
}

impl Drop for Claim<'_> {
    fn drop(&mut self) {
        self.0.store(false, Ordering::Release);
    }
}

/// The window the next data goes to: the last one that asked (Settings may be closed and reopened under a worker).
static TARGET: AtomicIsize = AtomicIsize::new(0);

fn publish(data: SafetyData) {
    *DATA.lock().unwrap_or_else(|p| p.into_inner()) = Some(data);
    // SAFETY: PostMessageW is thread-safe; if the window no longer exists it just fails.
    let target = HWND(TARGET.load(Ordering::Acquire) as *mut _);
    let _ = unsafe { PostMessageW(Some(target), WM_SETTINGS_SUITE_DATA, Default::default(), Default::default()) };
}

/// Runs `job` on a named thread holding the worker claim; false when one runs already or none could start.
fn spawn_worker(name: &str, job: impl FnOnce() + Send + 'static) -> bool {
    let Some(claim) = Claim::take(&WORKER) else { return false };
    let started = std::thread::Builder::new().name(name.into()).stack_size(WORKER_STACK).spawn(move || {
        let _claim = claim;
        job();
    });
    if let Err(e) = started {
        log!("settings: the {name} worker did not start: {e}");
        return false;
    }
    true
}

/// The worker that reads the journal.
pub(crate) fn start_load(settings: HWND) {
    TARGET.store(settings.0 as isize, Ordering::Release);
    let _ = spawn_worker("suite-changes", move || {
        let (journal, rolled_back) = read_all(&Real);
        publish(SafetyData { journal, report: None, rolled_back });
    });
}

/// Hands the worker's data to the window.
pub(crate) fn take_data() -> Option<SafetyData> {
    DATA.lock().unwrap_or_else(|p| p.into_inner()).take()
}

/// Opens or closes a part of the page; opening one closes the other.
pub(crate) fn toggle(v: &mut SafetyView, d: Disclose) {
    v.open = if v.open == Some(d) { None } else { Some(d) };
}

/// `UndoAll` and `Rollback`, called inside `with_win`.
pub(crate) fn run(a: Action, v: &mut SafetyView, settings: HWND) {
    match a {
        Action::UndoAll => {
            TARGET.store(settings.0 as isize, Ordering::Release);
            let started = spawn_worker("suite-undo", move || {
                let mut real = Real;
                let report = undo_line(real.undo());
                let (journal, rolled_back) = read_all(&real);
                publish(SafetyData { journal, report: Some(report), rolled_back });
            });
            if started {
                v.busy = true;
            } else {
                log!("settings: undo all not started (a worker is running)");
            }
        }
        Action::Rollback => {
            // Both are read again at the click: a first-start suite sync runs under the busy flag with the phase
            // idle, and a rollback in the middle of a component swap would leave a journal an older dock cannot replay.
            let kept = prev_kept_now();
            let idle = update_idle(crate::update::busy(), crate::update::status().phase);
            if !may_roll_back(kept, idle) {
                log!("settings: rollback refused (kept {kept}, updater idle {idle})");
                return;
            }
            let exe = match crate::autostart::installed_exe() {
                Ok(p) => p,
                Err(e) => {
                    log!("settings: rollback not started: {e}");
                    return;
                }
            };
            match crate::watchdog::spawn_breakaway(&exe, "--rollback") {
                Ok(handle) => {
                    drop(handle);
                    log!("settings: rollback started");
                }
                Err(e) => log!("settings: rollback not started: {e}"),
            }
        }
        _ => {}
    }
}

// ------------------------------------------------------------------ the live fields and the views

/// A previous exe is kept next to the installed one (one stat).
fn prev_kept_now() -> bool {
    crate::autostart::installed_exe().is_ok_and(|e| crate::autostart::prev_path(&e).exists())
}

/// The live fields of `refresh` over `SafetyView::default()`.
pub(crate) fn view_now() -> SafetyView {
    let mut v = SafetyView::default();
    refresh(&mut v);
    v
}

/// The one-second tick: updates `prev_kept`, `update_idle`, `incidents`, `sync_failed` and `config_errors` in place;
/// `journal`, `open`, `report`, `busy` and `update_incidents` are the worker's and the user's and stay untouched.
pub(crate) fn refresh(v: &mut SafetyView) {
    v.prev_kept = prev_kept_now();
    v.update_idle = update_idle(crate::update::busy(), crate::update::status().phase);
    v.incidents = incidents::recent();
    v.sync_failed = crate::update::suite::status().last_error.is_some();
    v.config_errors = crate::config::errors().iter().map(oled_config::ConfigError::log_line).collect();
}

/// The deterministic view of the goldens.
pub(crate) fn preview_view() -> SafetyView {
    SafetyView {
        journal: ChangesState::Loaded(vec![SystemChange {
            source: ChangeSource::Machine(1),
            label: label_of("takeover.folder_verbs"),
            detail: "HKCU\\Software\\Classes\\Directory \u{b7} 22 Nov".to_string(),
        }]),
        open: None,
        report: None,
        busy: false,
        prev_kept: true,
        update_idle: true,
        incidents: Vec::new(),
        sync_failed: false,
        config_errors: Vec::new(),
        update_incidents: Vec::new(),
    }
}

/// `preview_view()` with two made-up config problems and a rolled-back update.
pub(crate) fn preview_problems() -> SafetyView {
    SafetyView {
        config_errors: vec![
            "line 12, bar_opacity: 140 is above the maximum 100 (kept 95)".to_string(),
            "line 30, launcher.rows: \"many\" is not a number (kept 8)".to_string(),
        ],
        // 2026-11-22 10:14 UTC.
        update_incidents: vec![Incident {
            at_unix: 1_795_342_440,
            kind: IncidentKind::UpdateRolledBack(Version { major: 1, minor: 7, patch: 1 }),
        }],
        ..preview_view()
    }
}

fn note(icon: Glyph, label: &str, desc: Option<&str>) -> Row {
    row(icon, label, desc, Control::Note)
}

fn plain(label: String) -> Row {
    Row { icon: None, label, desc: None, control: Control::Note }
}

fn status_row(icon: Glyph, label: &str, desc: Option<&str>, text: &str) -> Row {
    row(icon, label, desc, Control::Status(text.to_string()))
}

/// The rows of the page.
pub(crate) fn rows(c: &Config, ctx: &Ctx<'_>) -> Vec<Row> {
    let v = ctx.safety;
    let mut out: Vec<Row> = Vec::new();

    // The config problems come first: the banner and the way to the file.
    if !v.config_errors.is_empty() {
        let n = v.config_errors.len();
        let head = if n == 1 {
            t(k::SAFETY_CONFIG_PROBLEM).to_string()
        } else {
            subst(t(k::SAFETY_CONFIG_PROBLEMS), &[("n", &n.to_string())])
        };
        let mut lines: Vec<String> = v.config_errors.iter().take(BANNER_LINES).cloned().collect();
        if n > BANNER_LINES {
            lines.push(subst(t(k::SAFETY_CONFIG_MORE), &[("n", &(n - BANNER_LINES).to_string())]));
        }
        out.push(Row { icon: None, label: head, desc: None, control: Control::Banner { lines } });
        out.push(buttons(
            glyphs::log_shape,
            "config.json",
            None,
            vec![Button { action: Action::EditConfig, text: t(k::SAFETY_CONFIG_EDIT).into(), enabled: true }],
        ));
    }

    out.push(header(t(k::SAFETY_RECOVERY), None));
    out.push(buttons(
        glyphs::power_shape,
        t(k::SETTINGS_PAGES_RESTORE_THE_WINDOWS_TASKBAR),
        Some(t(k::SAFETY_RESTORE_HELP)),
        vec![Button { action: Action::ExitDock, text: t(k::COMMON_EXIT_DOCK).into(), enabled: true }],
    ));
    out.push(note(glyphs::info_shape, t(k::SAFETY_WITHOUT), Some(t(k::SAFETY_WITHOUT_HELP))));
    out.push(note(glyphs::log_shape, t(k::SAFETY_RECOVER), Some(t(k::SAFETY_RECOVER_HELP))));

    out.push(header(t(k::SAFETY_CHANGES_HEAD), None));
    changes_rows(&mut out, c, v);

    out.push(header(t(k::SAFETY_UPDATES), None));
    out.push(status_row(glyphs::info_shape, t(k::SAFETY_INSTALLED), None, crate::preview::version_text()));
    let kept = if v.prev_kept { k::SAFETY_PREVIOUS_KEPT } else { k::SAFETY_PREVIOUS_NONE };
    out.push(row(
        glyphs::refresh_shape,
        t(k::SAFETY_PREVIOUS),
        Some(t(kept)),
        Control::Disclosure {
            what: Disclose::Rollback,
            text: t(k::SAFETY_ROLLBACK).into(),
            open: v.open == Some(Disclose::Rollback),
            enabled: may_roll_back(v.prev_kept, v.update_idle),
        },
    ));
    if v.open == Some(Disclose::Rollback) {
        out.push(Row {
            icon: None,
            label: String::new(),
            desc: None,
            control: Control::Buttons(vec![Button {
                action: Action::Rollback,
                text: t(k::SAFETY_ROLLBACK_GO).into(),
                enabled: may_roll_back(v.prev_kept, v.update_idle),
            }]),
        });
    }
    if v.sync_failed {
        out.push(note(glyphs::info_shape, t(k::SAFETY_SYNC_FAILED), None));
    }

    out.push(header(t(k::SAFETY_INCIDENTS), None));
    let list = merged_incidents(&v.incidents, &v.update_incidents);
    if list.is_empty() {
        out.push(status_row(glyphs::info_shape, t(k::SAFETY_INCIDENTS_NONE), None, t(k::SAFETY_NONE)));
    }
    out.extend(list.iter().map(|i| plain(incident_line(i))));
    out
}

/// The "System changes" row, its list and its Undo all button.
fn changes_rows(out: &mut Vec<Row>, c: &Config, v: &SafetyView) {
    let shield: Glyph = shield_glyph;
    let help = Some(t(k::SAFETY_CHANGES_HELP));
    match &v.journal {
        ChangesState::Loading => {
            out.push(status_row(shield, t(k::SAFETY_CHANGES_HEAD), help, t(k::SAFETY_LOADING)));
            return;
        }
        ChangesState::Failed => {
            out.push(status_row(shield, t(k::SAFETY_CHANGES_HEAD), help, t(k::SAFETY_READ_FAILED)));
            return;
        }
        ChangesState::Loaded(_) => {}
    }
    let can_undo = !v.busy && !WORKER.load(Ordering::Acquire);
    let list = all_changes(c.takeover_ctrl_shift_esc, &v.journal);
    let n = list.len();
    let label = subst(t(k::SAFETY_CHANGES), &[("n", &n.to_string())]);
    if n == 0 {
        out.push(status_row(shield, t(k::SAFETY_CHANGES_HEAD), help, t(k::SAFETY_NONE)));
    } else {
        out.push(row(
            shield,
            &label,
            help,
            Control::Disclosure {
                what: Disclose::UndoAll,
                text: t(k::SAFETY_UNDO_ALL).into(),
                open: v.open == Some(Disclose::UndoAll),
                enabled: n > 0 && can_undo,
            },
        ));
    }
    if v.open == Some(Disclose::UndoAll) {
        for ch in &list {
            out.push(note(shield, &ch.label, Some(&ch.detail)));
        }
        let text = if n == 1 {
            t(k::SAFETY_UNDO_ONE).to_string()
        } else {
            subst(t(k::SAFETY_UNDO_N), &[("n", &n.to_string())])
        };
        out.push(Row {
            icon: None,
            label: String::new(),
            desc: None,
            control: Control::Buttons(vec![Button { action: Action::UndoAll, text: text.into(), enabled: can_undo }]),
        });
    }
    if let Some(r) = &v.report {
        out.push(note(shield, &report_line(r), None));
    }
}

/// A shield outline, in the 16 x 16 box of the page icons.
pub(crate) fn shield_glyph(u: f32, v: f32) -> bool {
    const HALF: f32 = 0.75;
    const RING: [(f32, f32); 7] =
        [(8.0, 1.5), (13.0, 3.5), (13.0, 8.0), (8.0, 14.5), (3.0, 8.0), (3.0, 3.5), (8.0, 1.5)];
    RING.windows(2).any(|w| crate::glyphs::dist_seg(u, v, w[0].0, w[0].1, w[1].0, w[1].1) <= HALF)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::start_import;
    use crate::update::{Outcome, UpdateStatus};
    use oled_trust::journal::AppId;

    fn with_rows<R>(v: &SafetyView, c: &Config, f: impl FnOnce(&[Row]) -> R) -> R {
        let st = UpdateStatus {
            installed: Version { major: 1, minor: 4, patch: 0 },
            phase: Phase::Idle,
            available: None,
            last_check: String::new(),
            outcome: Outcome::None,
            staged: false,
        };
        let suite = crate::settings_pages::suite::SuiteView::default();
        let ctx = Ctx {
            screens: &[],
            picker_open: false,
            chooser_open: false,
            tray: &[],
            import: start_import::Status::Idle,
            update: &st,
            memory: Default::default(),
            data_dir: None,
            log_path: None,
            trust_elevated: false,
            powertoys: false,
            chord_held: None,
            battery_note: false,
            keys_note: false,
            suite: &suite,
            safety: v,
        };
        f(&rows(c, &ctx))
    }

    fn disclosure(r: &[Row], what: Disclose) -> Option<(bool, bool)> {
        r.iter().find_map(|r| match &r.control {
            Control::Disclosure { what: w, open, enabled, .. } if *w == what => Some((*open, *enabled)),
            _ => None,
        })
    }

    fn has_status(r: &[Row], text: &str) -> bool {
        r.iter().any(|r| r.control == Control::Status(text.to_string()))
    }

    fn change(label: &str) -> SystemChange {
        SystemChange { source: ChangeSource::Machine(1), label: label.into(), detail: "d".into() }
    }

    #[test]
    fn safety_rows_preview() {
        for italian in [false, true] {
            crate::i18n::with_italian(italian, || {
                with_rows(&preview_view(), &Config::default(), |r| {
                    assert!(matches!(r[0].control, Control::Header { .. }), "no banner without a problem");
                    assert!(r.iter().any(|r| matches!(&r.control,
                        Control::Buttons(b) if b[0].action == Action::ExitDock && b[0].enabled)));
                    assert_eq!(disclosure(r, Disclose::Rollback), Some((false, true)));
                    assert_eq!(disclosure(r, Disclose::UndoAll), Some((false, true)));
                    assert!(has_status(r, &Version::current().to_string()));
                    assert!(r.iter().any(|r| r.label == t(k::SAFETY_INCIDENTS_NONE)));
                });
            });
        }
    }

    #[test]
    fn count_adds_chord_and_journal() {
        let loaded = ChangesState::Loaded(vec![change("a"), change("b")]);
        assert_eq!(all_changes(false, &loaded).len(), 2);
        assert_eq!(all_changes(true, &loaded).len(), 3);
        assert_eq!(all_changes(true, &ChangesState::Loading).len(), 1);
        let c = Config { takeover_ctrl_shift_esc: true, ..Config::default() };
        let v = SafetyView { journal: loaded, ..preview_view() };
        with_rows(&v, &c, |r| {
            let head = subst(t(k::SAFETY_CHANGES), &[("n", "3")]);
            assert!(r.iter().any(|r| r.label == head), "{head}");
        });
    }

    #[test]
    fn loading_and_failed_rows() {
        let c = Config::default();
        for (state, text) in [(ChangesState::Loading, k::SAFETY_LOADING), (ChangesState::Failed, k::SAFETY_READ_FAILED)]
        {
            let v = SafetyView { journal: state, ..preview_view() };
            with_rows(&v, &c, |r| {
                assert!(has_status(r, t(text)));
                assert_eq!(disclosure(r, Disclose::UndoAll), None);
            });
        }
        let empty = SafetyView { journal: ChangesState::Loaded(vec![]), ..preview_view() };
        with_rows(&empty, &c, |r| assert!(has_status(r, t(k::SAFETY_NONE))));
    }

    #[test]
    fn open_list_and_report_rows() {
        let v = SafetyView {
            open: Some(Disclose::UndoAll),
            report: Some(UndoLine { restored: 1, other: 0, failed: 0, user_done: false }),
            ..preview_view()
        };
        with_rows(&v, &Config::default(), |r| {
            assert!(r.iter().any(|r| r.desc.as_deref() == Some("d")
                || r.label == t(k::SAFETY_CHANGE_FOLDERS).replace("{app}", suite_links::name(App::Files))));
            assert!(r.iter().any(|r| matches!(&r.control,
                Control::Buttons(b) if b[0].action == Action::UndoAll && b[0].text == t(k::SAFETY_UNDO_ONE))));
            assert!(r.iter().any(|r| r.label.contains(t(k::SAFETY_REPORT_USER))));
        });
        let busy = SafetyView { busy: true, ..v };
        with_rows(&busy, &Config::default(), |r| {
            assert_eq!(disclosure(r, Disclose::UndoAll), Some((true, false)));
        });
    }

    #[test]
    fn report_line_parts() {
        let line = UndoLine { restored: 2, other: 1, failed: 0, user_done: true };
        assert_eq!(report_line(&line), "2 restored, 1 changed by another program, left as is");
        let none = UndoLine { user_done: false, ..UndoLine::default() };
        assert_eq!(report_line(&none), t(k::SAFETY_REPORT_USER));
        let failed = UndoLine { restored: 0, other: 0, failed: 1, user_done: false };
        assert_eq!(report_line(&failed), format!("1 could not be undone. {}", t(k::SAFETY_REPORT_USER)));
    }

    #[test]
    fn rollback_disabled_while_updating() {
        assert!(!update_idle(true, Phase::Idle));
        assert!(!update_idle(false, Phase::Checking));
        assert!(update_idle(false, Phase::Idle));
        let c = Config::default();
        let busy = SafetyView { update_idle: update_idle(true, Phase::Idle), ..preview_view() };
        with_rows(&busy, &c, |r| assert_eq!(disclosure(r, Disclose::Rollback), Some((false, false))));
        let idle = SafetyView { update_idle: update_idle(false, Phase::Idle), ..preview_view() };
        with_rows(&idle, &c, |r| assert_eq!(disclosure(r, Disclose::Rollback), Some((false, true))));
        let none = SafetyView { prev_kept: false, ..preview_view() };
        with_rows(&none, &c, |r| {
            assert_eq!(disclosure(r, Disclose::Rollback), Some((false, false)));
            assert!(r.iter().any(|r| r.desc.as_deref() == Some(t(k::SAFETY_PREVIOUS_NONE))));
        });
        let open = SafetyView { open: Some(Disclose::Rollback), ..preview_view() };
        with_rows(&open, &c, |r| {
            assert!(r.iter().any(|r| matches!(&r.control,
                Control::Buttons(b) if b[0].action == Action::Rollback && b[0].enabled)));
        });
    }

    #[test]
    fn safety_banner_lists_errors() {
        let c = Config::default();
        let with = |n: usize| SafetyView {
            config_errors: (1..=n).map(|i| format!("problem {i}")).collect(),
            ..preview_view()
        };
        with_rows(&preview_view(), &c, |r| assert!(!r.iter().any(|r| matches!(r.control, Control::Banner { .. }))));
        with_rows(&with(1), &c, |r| {
            assert!(matches!(&r[0].control, Control::Banner { lines } if lines == &["problem 1"]));
            assert_eq!(r[0].label, t(k::SAFETY_CONFIG_PROBLEM));
            assert!(matches!(&r[1].control, Control::Buttons(b) if b[0].action == Action::EditConfig));
        });
        with_rows(&with(7), &c, |r| {
            let Control::Banner { lines } = &r[0].control else { panic!("the banner is the first row") };
            assert_eq!(r[0].label, subst(t(k::SAFETY_CONFIG_PROBLEMS), &[("n", "7")]));
            assert_eq!(lines.len(), 6);
            assert_eq!(lines[4], "problem 5");
            assert_eq!(lines[5], "+2 more");
            assert!(matches!(&r[1].control, Control::Buttons(b) if b[0].action == Action::EditConfig));
        });
    }

    #[test]
    fn tick_keeps_loaded_and_open() {
        let mut v = SafetyView {
            journal: ChangesState::Loaded(vec![change("a")]),
            open: Some(Disclose::UndoAll),
            report: Some(UndoLine { restored: 1, ..UndoLine::default() }),
            busy: true,
            incidents: vec![Incident { at_unix: 1, kind: IncidentKind::TakeoverPaused }],
            config_errors: vec!["stale".into()],
            update_incidents: vec![Incident { at_unix: 2, kind: IncidentKind::TakeoverPaused }],
            ..SafetyView::default()
        };
        let before = v.clone();
        refresh(&mut v);
        assert_eq!(v.journal, before.journal);
        assert_eq!(v.open, before.open);
        assert_eq!(v.report, before.report);
        assert_eq!(v.busy, before.busy);
        assert_eq!(v.update_incidents, before.update_incidents);
        assert_eq!(v.incidents, incidents::recent(), "the in-memory incidents are re-read");
        assert!(!v.config_errors.contains(&"stale".to_string()), "the config errors are re-read");
    }

    #[test]
    fn incidents_merge() {
        let at = |t: u64| Incident { at_unix: t, kind: IncidentKind::TakeoverPaused };
        let mark =
            Incident { at_unix: 200, kind: IncidentKind::UpdateRolledBack(Version { major: 1, minor: 7, patch: 1 }) };
        let merged = merged_incidents(&[at(100), at(300)], &[mark]);
        assert_eq!(merged.iter().map(|i| i.at_unix).collect::<Vec<_>>(), [300, 200, 100]);
        let many: Vec<Incident> = (1..=12).map(at).collect();
        let merged = merged_incidents(&many, &[]);
        assert_eq!(merged.len(), 10);
        assert_eq!(merged[0].at_unix, 12);
        let c = Config::default();
        let v = SafetyView { incidents: many, ..preview_view() };
        with_rows(&v, &c, |r| assert_eq!(r.iter().filter(|r| r.label.contains(t(k::INCIDENT_PAUSED))).count(), 10));
    }

    #[test]
    fn incident_lines_read_well() {
        // 2026-11-22 10:14 UTC.
        let rolled = Incident {
            at_unix: 1795342440,
            kind: IncidentKind::UpdateRolledBack(Version { major: 1, minor: 7, patch: 1 }),
        };
        crate::i18n::with_italian(false, || {
            let line = incident_line(&rolled);
            assert!(line.starts_with("22 "), "{line}");
            assert!(line.ends_with("10:14 \u{b7} Version 1.7.1 was rolled back: the previous version runs"), "{line}");
            assert_eq!(utc_parts(1795342440), (2026, 11, 22, 10, 14));
        });
    }

    // ------------------------------------------------------------------ F3

    struct Fake {
        machine: Vec<EntryView>,
        user: Result<Vec<crate::agent_proto::JournalRec>, String>,
        marks: Vec<(String, u64)>,
    }

    impl Reader for Fake {
        fn machine(&self) -> Vec<EntryView> {
            self.machine.clone()
        }
        fn user(&self) -> Result<Vec<crate::agent_proto::JournalRec>, String> {
            self.user.clone()
        }
        fn undo(&mut self) -> Result<(UndoReport, bool), String> {
            Ok((UndoReport::default(), true))
        }
        fn rolled_back(&self) -> Vec<(String, u64)> {
            self.marks.clone()
        }
    }

    fn entry(seq: u64, kind: Kind) -> EntryView {
        let target = Target::Reg { hive: Hive::Hkcu, key: "Software\\Classes\\Directory".into(), name: None };
        EntryView { seq, kind, target, ts: 1795342440 }
    }

    fn rec(seq: u64, kind: &str) -> crate::agent_proto::JournalRec {
        crate::agent_proto::JournalRec { id: 0, seq, kind: kind.into(), target: "T".into(), ts: 1795342440 }
    }

    fn fake() -> Fake {
        Fake { machine: vec![], user: Ok(vec![]), marks: vec![] }
    }

    #[test]
    fn rolled_back_marks_become_incidents() {
        let f = Fake { marks: vec![("v1.7.1".into(), 500), ("junk".into(), 7)], ..fake() };
        let (_, inc) = read_all(&f);
        assert_eq!(
            inc,
            vec![Incident {
                at_unix: 500,
                kind: IncidentKind::UpdateRolledBack(Version { major: 1, minor: 7, patch: 1 })
            }]
        );
        // A stage without a mark is never reported by the source.
        assert!(read_all(&fake()).1.is_empty());
    }

    #[test]
    fn labels_by_kind() {
        crate::i18n::with_italian(false, || {
            assert_eq!(label_of("takeover.folder_verbs"), format!("Folders open in {}", suite_links::name(App::Files)));
            assert_eq!(label_of("takeover.future"), t(k::SAFETY_CHANGE_OTHER));
        });
    }

    #[test]
    fn only_takeovers_are_listed() {
        let f = Fake {
            machine: vec![
                entry(1, Kind::Install),
                entry(2, Kind::Takeover(TakeoverId::FolderVerbs)),
                entry(3, Kind::Update),
                entry(4, Kind::UserEdit(AppId::Dock)),
            ],
            user: Ok(vec![rec(9, "install"), rec(10, "takeover.folder_verbs"), rec(11, "user_edit.dock")]),
            ..fake()
        };
        let (state, _) = read_all(&f);
        let ChangesState::Loaded(list) = state else { panic!("loaded") };
        let sources: Vec<ChangeSource> = list.iter().map(|c| c.source).collect();
        assert_eq!(sources, [ChangeSource::Machine(2), ChangeSource::User(10)]);
        assert!(list[0].detail.starts_with("HKCU\\Software\\Classes\\Directory \u{b7} 22 "), "{}", list[0].detail);
    }

    #[test]
    fn user_failure_keeps_machine() {
        let f = Fake {
            machine: vec![entry(2, Kind::Takeover(TakeoverId::FolderVerbs))],
            user: Err("no agent".into()),
            ..fake()
        };
        assert!(matches!(read_all(&f).0, ChangesState::Loaded(l) if l.len() == 1));
        let both = Fake { user: Err("no agent".into()), ..fake() };
        assert_eq!(read_all(&both).0, ChangesState::Failed);
    }

    #[test]
    fn undo_line_from_report() {
        let report = UndoReport {
            restored: 2,
            already: 1,
            changed_by_other: vec![Target::Task("x".into())],
            ..Default::default()
        };
        assert_eq!(undo_line(Ok((report, true))), UndoLine { restored: 3, other: 1, failed: 0, user_done: true });
        assert_eq!(undo_line(Err("busy".into())), UndoLine { restored: 0, other: 0, failed: 1, user_done: false });
    }

    #[test]
    fn undo_arm_saves_first() {
        let window = include_str!("settings_window.rs");
        let arm = &window[window.find("Action::UndoAll =>").expect("the UndoAll arm")..];
        let (save, undo) = (arm.find("w.save(").expect("save"), arm.find("safety::run(Action::UndoAll").expect("run"));
        assert!(save < undo, "the chord is saved before the worker starts");
    }

    #[test]
    fn one_worker_at_a_time() {
        let flag = AtomicBool::new(false);
        let first = Claim::take(&flag);
        assert!(first.is_some());
        assert!(Claim::take(&flag).is_none());
        drop(first);
        assert!(Claim::take(&flag).is_some());
    }

    // ------------------------------------------------------------------ F4

    fn code() -> &'static str {
        let src = include_str!("settings_safety.rs");
        &src[..src.find("#[cfg(test)]").expect("tests module")]
    }

    #[test]
    fn rollback_gate() {
        let idles = [
            update_idle(false, Phase::Idle),
            update_idle(true, Phase::Idle),
            update_idle(false, Phase::Checking),
            update_idle(false, Phase::Installing),
        ];
        assert_eq!(idles, [true, false, false, false]);
        for kept in [false, true] {
            for (i, idle) in idles.iter().enumerate() {
                assert_eq!(may_roll_back(kept, *idle), kept && i == 0);
            }
        }
    }

    #[test]
    fn rollback_rereads_busy() {
        let src = code();
        let arm = &src[src.find("Action::Rollback =>").expect("the arm")..];
        let (busy, status, spawn) = (
            arm.find("update::busy()").expect("busy"),
            arm.find("update::status()").expect("status"),
            arm.find("spawn_breakaway(").expect("spawn"),
        );
        assert!(busy < spawn && status < spawn);
        assert!(arm.find("prev_kept_now()").expect("kept") < spawn);
    }

    #[test]
    fn rollback_uses_the_cli_role() {
        let src = code();
        assert_eq!(src.matches("spawn_breakaway(").count(), 1);
        let at = src.find("spawn_breakaway(").unwrap();
        assert!(src[at..].starts_with("spawn_breakaway(&exe, \"--rollback\")"));
        assert!(src[..at].contains("crate::autostart::installed_exe()"));
    }
}
