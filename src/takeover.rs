//! The takeover policy (dock-suite 2.3): when the Ctrl+Shift+Esc takeover can be on, and what the UI publishes to the
//! hook: availability in its written order, the Administrator protection mode and the Windows build read once, and
//! the gate and chord bits the UI publishes to the keyboard hook.
//!
//! Declared as a child module of `ui` (`#[path]`).
#![allow(dead_code)] // `view`, `Block` and `toggle_enabled` are read by DS-SUITE

use std::cell::Cell;
use std::sync::atomic::{AtomicU8, Ordering::Relaxed};

use windows::Win32::Foundation::HWND;
use windows::Win32::System::SystemInformation::GetTickCount;
use windows::Win32::UI::WindowsAndMessaging::{GetClassNameW, GetShellWindow, GetWindowThreadProcessId};

use crate::hook::{self, DockChords, TakeoverGate};
use crate::log;
use crate::ui::suite_links::{self, App};

/// A suite app's release stage as the dock sees it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum StageView {
    Beta,
    Stable,
    Unknown,
}

/// The Administrator protection mode of the machine.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ApMode {
    Off,
    On,
    Unknown,
}

/// Why the Ctrl+Shift+Esc takeover cannot be on now, in the order `availability` checks them (the Settings reason).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Block {
    TasksMissing,
    TasksOff,
    Beta,
    Build,
    NoFallback,
    Paused,
}

/// Everything `availability` reads.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct PolicyIn {
    pub installed: bool,
    pub enabled: bool,
    pub stage: StageView,
    pub build: Option<u32>,
    pub ap: ApMode,
    pub reinject_ok: bool,
    pub paused: bool,
    pub test_switch: bool,
}

/// What Settings shows: the stored setting and whether it can be on now.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct TakeoverView {
    pub on: bool,
    pub avail: Result<(), Block>,
}

/// Builds where a hook-initiated activation was proven; empty until the live sessions record one.
pub(crate) const FOREGROUND_VERIFIED_BUILDS: &[u32] = &[];
/// Windows opens its Task Manager for a Ctrl+Shift+Esc re-injected with the dock's tag; false until recorded.
pub(crate) const REINJECT_OPENS_TASKMGR: bool = false;

/// Why the takeover cannot be on, in the order of the plan: the first failing condition. The test switch lifts only the
/// stage and the build conditions.
pub(crate) fn availability(p: &PolicyIn) -> Result<(), Block> {
    if !p.installed {
        return Err(Block::TasksMissing);
    }
    if !p.enabled {
        return Err(Block::TasksOff);
    }
    if p.stage != StageView::Stable && !p.test_switch {
        return Err(Block::Beta);
    }
    let verified = p.build.is_some_and(|b| FOREGROUND_VERIFIED_BUILDS.contains(&b));
    if !verified && !p.test_switch {
        return Err(Block::Build);
    }
    if !fallback_ok(p.ap, p.reinject_ok) {
        return Err(Block::NoFallback);
    }
    if p.paused {
        return Err(Block::Paused);
    }
    Ok(())
}

/// Whether a failed delivery can always end in a task manager: with Administrator protection off the dock's own
/// launch works; otherwise only a re-injected chord does.
pub(crate) fn fallback_ok(ap: ApMode, reinject_ok: bool) -> bool {
    match ap {
        ApMode::Off => true,
        ApMode::On | ApMode::Unknown => reinject_ok,
    }
}

/// Administrator protection from the two token users: the same account at both ends is Off, two accounts is On, a
/// missing side is Unknown.
pub(crate) fn ap_mode_of(own: Option<&str>, shell: Option<&str>) -> ApMode {
    match (own, shell) {
        (Some(a), Some(b)) if a.eq_ignore_ascii_case(b) => ApMode::Off,
        (Some(_), Some(_)) => ApMode::On,
        _ => ApMode::Unknown,
    }
}

const AP_UNKNOWN: u8 = 0;
const AP_OFF: u8 = 1;
const AP_ON: u8 = 2;
/// The mode read at the first refresh that could read both SIDs (an Unknown result is not kept).
static AP_CACHE: AtomicU8 = AtomicU8::new(AP_UNKNOWN);

/// The cached Administrator protection mode; reads the two token users while nothing is cached (UI thread).
pub(crate) fn ap_cached() -> ApMode {
    match AP_CACHE.load(Relaxed) {
        AP_OFF => return ApMode::Off,
        AP_ON => return ApMode::On,
        _ => {}
    }
    let own = oled_sys::datadir::token_user_sid();
    let shell = shell_user_sid();
    let mode = ap_mode_of(own.as_deref(), shell.as_deref());
    match mode {
        ApMode::Off => AP_CACHE.store(AP_OFF, Relaxed),
        ApMode::On => AP_CACHE.store(AP_ON, Relaxed),
        ApMode::Unknown => {}
    }
    mode
}

/// The token user of the process that owns the shell window (the account whose desktop this is).
fn shell_user_sid() -> Option<String> {
    // SAFETY: plain query of the desktop's shell window; a null handle gives a zero pid below.
    let shell = unsafe { GetShellWindow() };
    let mut pid = 0u32;
    // SAFETY: `pid` is a writable local; `shell` may be null, which only yields a zero pid.
    unsafe { GetWindowThreadProcessId(shell, Some(&mut pid)) };
    if pid == 0 {
        return None;
    }
    oled_sys::pipe::user_sid_of_pid(pid).ok()
}

/// The part of a Windows build string ("26300.1742") before the dot, as a number.
fn build_of(s: &str) -> Option<u32> {
    s.split('.').next()?.parse().ok()
}

pub(crate) fn build_number() -> Option<u32> {
    build_of(&crate::start_search::windows_build())
}

/// The breaker is open while its tick is in the future (wrap-safe); 0 is closed.
fn breaker_open(until: u32, now: u32) -> bool {
    until != 0 && (until.wrapping_sub(now) as i32) > 0
}

/// Turning the takeover off ends a pause: true when the setting is off and the breaker is not closed.
fn rearm(on: bool, breaker: u32) -> bool {
    !on && breaker != 0
}

/// The hook's gate: the pause is not part of it (the hook checks the breaker itself, and the gate is not published
/// again when the breaker expires).
fn ready_of(on: bool, p: &PolicyIn) -> bool {
    on && availability(&PolicyIn { paused: false, ..*p }).is_ok()
}

/// The gate for `ready`, the other bits kept.
fn gate_of(prev: TakeoverGate, ready: bool) -> TakeoverGate {
    prev.with(TakeoverGate::TASKS_READY, ready)
}

/// The rows of `prev` kept, the takeover bits replaced.
fn chords_of(prev: DockChords, ready: bool) -> DockChords {
    prev.rows().with(if ready { DockChords::CTRL_SHIFT_ESC } else { DockChords::NONE })
}

/// A window is "Tasks in front" when its class is exactly the Tasks class and its image is the installed one; the
/// image is read only for that class.
fn in_front(class: &str, image_ok: impl FnOnce() -> bool) -> bool {
    class == oled_trust::frame::tasks::WINDOW_CLASS && image_ok()
}

thread_local! {
    /// What Settings shows (UI thread).
    static VIEW: Cell<TakeoverView> = const { Cell::new(TakeoverView { on: false, avail: Err(Block::Beta) }) };
    /// The last gate verdict (UI thread).
    static READY: Cell<bool> = const { Cell::new(false) };
    /// The last logged state: (ready, setting off, reason).
    static LOGGED: Cell<Option<(bool, bool, Option<Block>)>> = const { Cell::new(None) };
}

/// The bits the hook gets: Ctrl+Shift+Esc exactly while the gate is ready, so a refresh during a pause keeps the bit
/// and the takeover resumes by itself when the breaker expires.
pub(crate) fn chord_bits_now() -> DockChords {
    chords_of(DockChords::NONE, READY.with(Cell::get))
}

/// UI thread: re-reads the conditions and publishes the gate, the chord bits and the Settings view.
pub(crate) fn refresh_gate() {
    let c = crate::config::get();
    let on = c.takeover_ctrl_shift_esc;
    if rearm(on, hook::takeover_breaker()) {
        hook::set_takeover_breaker(0);
        log!("takeover: pause ended (switched off)");
    }
    let s = suite_links::state(App::Tasks);
    // SAFETY: GetTickCount has no preconditions.
    let now = unsafe { GetTickCount() };
    let p = PolicyIn {
        installed: s.installed,
        enabled: s.enabled,
        stage: s.stage,
        build: build_number(),
        ap: ap_cached(),
        reinject_ok: REINJECT_OPENS_TASKMGR,
        paused: breaker_open(hook::takeover_breaker(), now),
        test_switch: test_switch(),
    };
    let avail = availability(&p);
    VIEW.with(|v| v.set(TakeoverView { on, avail }));
    let ready = ready_of(on, &p);
    READY.with(|r| r.set(ready));
    hook::set_takeover_gate(gate_of(hook::takeover_gate(), ready));
    hook::set_dock_chords(chords_of(hook::dock_chords(), ready));
    let reason = availability(&PolicyIn { paused: false, ..p }).err();
    let state = (ready, !on, reason);
    if LOGGED.with(|l| l.replace(Some(state))) != Some(state) {
        match (ready, on, reason) {
            (true, _, _) => log!("takeover: gate ready"),
            (false, false, _) => log!("takeover: gate off (switched off)"),
            (false, true, Some(b)) => log!("takeover: gate off ({b:?})"),
            (false, true, None) => log!("takeover: gate off"),
        }
    }
}

/// UI thread, the FOREGROUND branch: the "Tasks in front" bit follows the window now in front.
pub(crate) fn on_foreground(fg: HWND) {
    let mut cls = [0u16; 32];
    // SAFETY: `cls` is a writable buffer of the declared length.
    let n = unsafe { GetClassNameW(fg, &mut cls) }.max(0) as usize;
    let class = String::from_utf16_lossy(cls.get(..n).unwrap_or(&[]));
    let front = in_front(&class, || {
        let mut pid = 0u32;
        // SAFETY: `pid` is a writable local.
        unsafe { GetWindowThreadProcessId(fg, Some(&mut pid)) };
        let Some(exe) = suite_links::exe_path(App::Tasks) else { return false };
        pid != 0 && oled_sys::pipe::peer_of_pid(pid).is_ok_and(|peer| oled_sys::ident::same_image(&peer.image, &exe))
    });
    let prev = hook::takeover_gate();
    let next = prev.with(TakeoverGate::TASKS_IN_FRONT, front);
    if next != prev {
        hook::set_takeover_gate(next);
    }
}

/// What Settings shows (UI thread).
pub(crate) fn view() -> TakeoverView {
    let v = VIEW.with(Cell::get);
    // SAFETY: GetTickCount has no preconditions.
    let paused = breaker_open(hook::takeover_breaker(), unsafe { GetTickCount() });
    TakeoverView { avail: with_pause(v.avail, paused), ..v }
}

/// The stored availability with the pause as it is now: the breaker trips and expires without a refresh, and Paused
/// is the last check of the availability. Pure.
fn with_pause(a: Result<(), Block>, paused: bool) -> Result<(), Block> {
    match a {
        Ok(()) if paused => Err(Block::Paused),
        Err(Block::Paused) if !paused => Ok(()),
        other => other,
    }
}

/// The owner-probe switch that turns the takeover on for a live session; false in every other build.
pub(crate) fn test_switch() -> bool {
    #[cfg(feature = "owner-probe")]
    return crate::launcher_probe::takeover_test();
    #[cfg(not(feature = "owner-probe"))]
    false
}

/// The Settings toggle of a takeover is enabled when it may be turned on, and always while it is on (it can always be
/// turned off). Pure.
pub(crate) fn toggle_enabled(v: TakeoverView) -> bool {
    v.avail.is_ok() || v.on
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn turning_off_is_always_possible() {
        let blocks =
            [Block::TasksMissing, Block::TasksOff, Block::Beta, Block::Build, Block::NoFallback, Block::Paused];
        for b in blocks {
            assert!(toggle_enabled(TakeoverView { on: true, avail: Err(b) }), "{b:?}: on can always be turned off");
            assert!(!toggle_enabled(TakeoverView { on: false, avail: Err(b) }), "{b:?}: off stays off while blocked");
        }
        assert!(toggle_enabled(TakeoverView { on: false, avail: Ok(()) }));
        assert!(toggle_enabled(TakeoverView { on: true, avail: Ok(()) }));
    }

    /// Every condition met through the test switch (the verified-build list is empty until the live sessions fill it).
    fn all_good() -> PolicyIn {
        PolicyIn {
            installed: true,
            enabled: true,
            stage: StageView::Stable,
            build: Some(0),
            ap: ApMode::Off,
            reinject_ok: false,
            paused: false,
            test_switch: true,
        }
    }

    /// One row per block, each with every earlier condition satisfied and every later one failing as well.
    fn rows() -> Vec<(PolicyIn, Result<(), Block>)> {
        let g = all_good();
        let worst = PolicyIn { stage: StageView::Beta, ap: ApMode::On, paused: true, test_switch: false, ..g };
        vec![
            (PolicyIn { installed: false, enabled: false, ..worst }, Err(Block::TasksMissing)),
            (PolicyIn { enabled: false, ..worst }, Err(Block::TasksOff)),
            (worst, Err(Block::Beta)),
            (PolicyIn { stage: StageView::Unknown, ..worst }, Err(Block::Beta)),
            (PolicyIn { stage: StageView::Stable, ..worst }, Err(Block::Build)),
            (PolicyIn { test_switch: true, ..worst }, Err(Block::NoFallback)),
            (PolicyIn { test_switch: true, ap: ApMode::Unknown, ..worst }, Err(Block::NoFallback)),
            (PolicyIn { test_switch: true, ap: ApMode::Off, ..worst }, Err(Block::Paused)),
            (PolicyIn { test_switch: true, ap: ApMode::On, reinject_ok: true, ..worst }, Err(Block::Paused)),
            (g, Ok(())),
        ]
    }

    #[test]
    fn availability_order() {
        for (p, want) in rows() {
            assert_eq!(availability(&p), want, "{p:?}");
        }
        // The test switch lifts only Beta and Build.
        let g = all_good();
        assert_eq!(availability(&PolicyIn { installed: false, ..g }), Err(Block::TasksMissing));
        assert_eq!(availability(&PolicyIn { enabled: false, ..g }), Err(Block::TasksOff));
        assert_eq!(availability(&PolicyIn { stage: StageView::Beta, build: None, ..g }), Ok(()));
        assert_eq!(availability(&PolicyIn { ap: ApMode::Unknown, ..g }), Err(Block::NoFallback));
        assert_eq!(availability(&PolicyIn { paused: true, ..g }), Err(Block::Paused));
        // A verified build lifts Build without the switch (when the list holds one).
        if let Some(&b) = FOREGROUND_VERIFIED_BUILDS.first() {
            let p = PolicyIn { build: Some(b), test_switch: false, ..g };
            assert_eq!(availability(&p), Ok(()));
        }
    }

    #[test]
    fn view_follows_the_breaker() {
        assert_eq!(with_pause(Ok(()), true), Err(Block::Paused));
        assert_eq!(with_pause(Ok(()), false), Ok(()));
        assert_eq!(with_pause(Err(Block::Paused), false), Ok(()));
        assert_eq!(with_pause(Err(Block::Paused), true), Err(Block::Paused));
        assert_eq!(with_pause(Err(Block::Beta), true), Err(Block::Beta));
        assert_eq!(with_pause(Err(Block::Beta), false), Err(Block::Beta));
    }

    #[test]
    fn view_keeps_turning_off_possible() {
        for (p, _) in rows() {
            let v = TakeoverView { on: true, avail: availability(&p) };
            assert!(toggle_enabled(v), "{p:?}: a takeover that is on can always be turned off");
        }
    }

    #[test]
    fn ap_mode_table() {
        assert_eq!(ap_mode_of(Some("S-1-5-21-1"), Some("S-1-5-21-1")), ApMode::Off);
        assert_eq!(ap_mode_of(Some("S-1-5-21-1"), Some("s-1-5-21-1")), ApMode::Off);
        assert_eq!(ap_mode_of(Some("S-1-5-21-1"), Some("S-1-5-21-2")), ApMode::On);
        assert_eq!(ap_mode_of(None, Some("S-1-5-21-1")), ApMode::Unknown);
        assert_eq!(ap_mode_of(Some("S-1-5-21-1"), None), ApMode::Unknown);
        assert_eq!(ap_mode_of(None, None), ApMode::Unknown);
    }

    #[test]
    fn fallback_table() {
        for re in [false, true] {
            assert!(fallback_ok(ApMode::Off, re));
        }
        for ap in [ApMode::On, ApMode::Unknown] {
            assert!(!fallback_ok(ap, false));
            assert!(fallback_ok(ap, true));
        }
    }

    #[test]
    fn build_number_parses() {
        assert_eq!(build_of("26300.1742"), Some(26300));
        assert_eq!(build_of("26300"), Some(26300));
        assert_eq!(build_of(""), None);
        assert_eq!(build_of("x.1"), None);
    }

    /// The probe switch's name stays in the owner-probe file, and the call to it sits under the owner-probe cfg line.
    #[test]
    fn test_switch_is_owner_probe_only() {
        let needle = format!("DOCK_{}_TAKEOVER", "TEST");
        let mut stack = vec![std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("src")];
        while let Some(dir) = stack.pop() {
            for e in std::fs::read_dir(&dir).expect("src is readable").flatten() {
                let p = e.path();
                if p.is_dir() {
                    stack.push(p);
                } else if p.extension().is_some_and(|x| x == "rs") {
                    let text = std::fs::read_to_string(&p).expect("a source file reads");
                    let owner = p.file_name().is_some_and(|n| n == "launcher_probe.rs");
                    assert_eq!(text.contains(&needle), owner, "{}", p.display());
                }
            }
        }
        let src = include_str!("takeover.rs");
        let lines: Vec<&str> = src.lines().collect();
        let call = format!("crate::{}::{}", "launcher_probe", "takeover_test();");
        let i = lines.iter().position(|l| l.contains("return ") && l.contains(&call)).expect("the call exists");
        assert_eq!(lines[i - 1].trim(), format!("#[cfg(feature = \"{}\")]", "owner-probe"));
        assert_eq!(lines[i + 1].trim(), format!("#[cfg(not(feature = \"{}\"))]", "owner-probe"));
        assert_eq!(lines[i + 2].trim(), "false");
    }

    #[test]
    fn gate_follows_ready() {
        let none = TakeoverGate::NONE;
        let front = none.with(TakeoverGate::TASKS_IN_FRONT, true);
        assert!(gate_of(none, true).has(TakeoverGate::TASKS_READY));
        assert!(!gate_of(gate_of(none, true), false).has(TakeoverGate::TASKS_READY));
        assert!(gate_of(front, true).has(TakeoverGate::TASKS_IN_FRONT), "in front is kept");
        assert!(gate_of(front, false).has(TakeoverGate::TASKS_IN_FRONT));
        assert_eq!(gate_of(front, false), front);
        assert_eq!(gate_of(none, false), none);
    }

    #[test]
    fn gate_ignores_the_pause() {
        let p = PolicyIn { paused: true, ..all_good() };
        assert_eq!(availability(&p), Err(Block::Paused), "Settings shows the pause");
        assert!(ready_of(true, &p), "the gate does not hold the pause");
        assert!(!ready_of(false, &p));
        assert_eq!(chords_of(DockChords::NONE, ready_of(true, &p)), DockChords::CTRL_SHIFT_ESC);
        assert!(!breaker_open(0, 100));
        assert!(breaker_open(200, 100));
        assert!(!breaker_open(100, 100) && !breaker_open(50, 100));
        assert!(breaker_open(10, u32::MAX - 5), "wrap-safe");
    }

    #[test]
    fn off_ends_the_pause() {
        assert!(rearm(false, 0x1234));
        assert!(!rearm(true, 0x1234));
        assert!(!rearm(false, 0));
    }

    #[test]
    fn chords_keep_the_rows() {
        const ROW: DockChords = DockChords(1); // a dock row (bits 0-7)
        let prev = ROW.with(DockChords::CTRL_SHIFT_ESC);
        assert_eq!(chords_of(prev, false), ROW);
        assert_eq!(chords_of(ROW, true), ROW.with(DockChords::CTRL_SHIFT_ESC));
        assert_eq!(chords_of(DockChords::NONE, true), DockChords::CTRL_SHIFT_ESC);
    }

    #[test]
    fn in_front_needs_class_and_image() {
        let class = oled_trust::frame::tasks::WINDOW_CLASS;
        assert!(in_front(class, || true));
        assert!(!in_front(class, || false));
        let mut called = false;
        assert!(!in_front("Notepad", || {
            called = true;
            true
        }));
        assert!(!called, "the image is read only for the Tasks class");
        assert!(!in_front(&format!("{class}X"), || true));
        assert!(!in_front(&class.to_ascii_lowercase(), || true));
    }

    #[test]
    fn nothing_published_until_a_refresh() {
        assert_eq!(chord_bits_now(), DockChords::NONE);
        assert_eq!(view(), TakeoverView { on: false, avail: Err(Block::Beta) });
        assert!(!test_switch());
    }
}
