//! Low-level keyboard hook (WH_KEYBOARD_LL) for the Win key pressed "alone", and the launcher's chord.
//!
//! Win key rules (see README for the limits):
//!  * Win down  -> stores `win_down`, `other=false`, SWALLOWS the event.
//!  * first other key down while `win_down` -> SWALLOWS that one too and injects
//!    with a SINGLE SendInput the pair [WIN down, key down] (extra-info = INJECT_TAG
//!    on both): the order is deterministic. Win+E, Win+D, Win+R... work.
//!    Repeats and the release of that key pass through normally.
//!  * Win up -> if `other==false` it swallows the event and posts WM_DOCK_TOGGLE
//!    (the real work is done by the wnd_proc), otherwise it lets the release through.
//!  * Double tap: a lone Win press starting within DOUBLE_TAP_MS of the previous lone release
//!    posts WM_DOCK_START_MENU instead of the second WM_DOCK_TOGGLE (the first tap already toggled).
//!  * Win auto-repeat: ignored (swallowed while the press is "pending").
//!  * Events injected by OTHER apps (LLKHF_INJECTED without our tag): ignored,
//!    the dock only responds to the physical Win key. Only an owner-probe build started with the accept-injected
//!    switch of `launcher_probe` treats them as physical (`accepted`), so a test agent can drive the L0 session.
//!  * Left + right Win together: the second Win is treated as an "other key".
//!  * Stuck state (release lost on the secure desktop): if more than REPEAT_WINDOW_MS
//!    have passed since the last Win event, the first other key does NOT inject anything.
//!
//! Dock chord rules (SPEC-dock 5.2.2), only while the UI has set owned rows (`set_dock_chords`); without any row the
//! Win rules above are unchanged:
//!  * A chord key (a top-row digit, T or B) pressed while Win is held with exactly its row's modifiers (none, Shift or
//!    Alt; `resolve_chord`) is swallowed and WM_DOCK_CHORD posted with the next sequence and kb.time. Its auto-repeat
//!    and its release are swallowed too, Win held or not, until its own up (or 1.5 s without an event of it), so a
//!    digit held after Win's release never types. Not while the dock window does not exist, and not when the UI left
//!    a post unacknowledged for CHORD_ACK_TIMEOUT_MS (failed open: Windows gets the chord as in 1.5.2, and a Sync post
//!    lets a live UI acknowledge again).
//!  * A modifier pressed while Win is held passes alone and Win stays pending; the menu-mask key follows a Shift or
//!    Alt chord's post and precedes an Alt up Windows would read as a lone tap.
//!  * The Win up, in order: after an injected pair it passes (Windows saw the injected Win down); during Peek, or for
//!    a press that owned a chord or passed a modifier, it is swallowed and toggles nothing (with Alt still down after
//!    a passed modifier, the menu-mask key follows, so the Alt release opens no menu bar); otherwise the tap as above.
//!    A Win up during Peek also posts WM_DOCK_PEEK(0).
//!  * A key-down that went by the hook unseen (a reinstall, a time-out) leaves Windows holding a Win the hook believes
//!    it swallowed. At that press's up, with no pair injected, the callback reads the key's async state (the state from
//!    before this event); the watchdog's tick notes it too (`note_leak`, one line per press). Either way the up passes
//!    behind the menu-mask key (no Start menu) and the press ends with no tap; a Peek on ends. A press that owned a chord or passed a modifier has no
//!    auto-repeat once another key went down: the keys it keeps (the chord key and its auto-repeat, the modifier and
//!    its auto-repeat) renew its time while each comes within REPEAT_WINDOW_MS of it, since the hook saw no gap. Past
//!    REPEAT_WINDOW_MS of silence only a chord key it owns or a modifier it passes keeps it (up to LONE_STUCK_MS); any
//!    other key takes the stale rule, so no pair is injected after a release lost on the secure desktop.
//!  * Peek (PEEK and DIGITS owned, no fullscreen window in front): a fresh Win down arms a thread timer of this
//!    thread; PEEK_MS later, with Win still held alone (no pair, chord or modifier), WM_DOCK_PEEK(1) is posted. The Win
//!    up, an injected pair or the repair ends it (WM_DOCK_PEEK(0)); every Win up kills the timer.
//!  * The modifiers are the hook's own (`ModTrack`, below), tracked while the launcher chord or any row is set.
//!  * A cut-off (`cut_off`: a desktop switch, a lock or unlock, suspend or resume, a remote client leaving the front, a
//!    window above the dock such as the UAC prompt coming to the front or leaving it, a reinstall that finds the hook
//!    removed) forgets the Win press and the tracked modifiers while a row is owned, so a release that went by unseen
//!    turns no digit, T or B typed next into a chord and no stale Shift or Alt picks a row.
//!  * A stall is a cut-off too, noticed while a row is owned: the callback notes a return more than STALL_MS after its
//!    entry or a key it admits reaching it more than STALL_MS after its time stamp (`stall_check`, with atomics and a
//!    thread message, never acting on it); while a press that owned a chord or passed a modifier is held, a heartbeat
//!    timer of the hook thread notes a tick more than STALL_MS late (`heartbeat`). The hook thread then runs the
//!    cut-off and reinstalls the hook at once (`stalled`): the keys that waited, a release among them, went by unseen.
//!
//! Launcher chord rules (SPEC-launcher 5.5.4), only while the UI has set a chord (`set_launcher_chord`); without
//! one the callback loads one atomic and behaves as above:
//!  * Modifiers come from the hook's own events (Alt, Ctrl, Shift per side; Win from the Win state above). One with
//!    no event for MOD_STALE_MS is re-read from the async key state, and `reset_modifiers` (lock, suspend, an admin
//!    launch) forgets them all: a lost release can only make the chord pass, never match a bare Space.
//!  * The chord key down with exactly the chord's modifiers (AltGr is Ctrl+Alt: never Alt+Space) and no Win:
//!    passed in fullscreen (with WM_DOCK_LAUNCHER_PASSED, so the UI checks the flag again) and remote sessions;
//!    passed with WM_DOCK_LAUNCHER_YIELDED while Command Palette answers;
//!    failed open (passed, one log line per sequence, and WM_DOCK_LAUNCHER_PASSED with the newest sequence, which a
//!    live UI acknowledges) when the UI thread left a post unacknowledged for ACK_TIMEOUT_MS; otherwise swallowed, the menu-mask key injected under Alt (so the Alt release opens no menu
//!    bar) and WM_DOCK_LAUNCHER posted with the next sequence number and kb.time. Its repeats and its release go
//!    the way its down went. The UI opens nothing for a stale, superseded or failed-open post (`post_verdict`).
//!  * Type-ahead: a chord that will open the launcher opens a window of TYPEAHEAD_MS in which every other key down
//!    (modifiers, Win, the system's keys of `never_kept` and chords of `system_chord`, keys pressed with Win and the
//!    repeats of a key Windows already holds excepted) is swallowed
//!    into a ring of RING_CAP keys tagged with the post's sequence. The UI takes the ring of the sequence it opened and replays it, or ends it when nothing
//!    opens; the hook expires it at the first key after the deadline. An up is swallowed only when its down was.
//!  * The ring's control word (sequence, OPEN, ENDED, length) is changed by both threads only by compare-exchange,
//!    except the hook's open of a new window: a buffered key is in the UI's copy or passed to Windows, never both.
//!  * Gesture evidence: the whole kb.time of a physical Ctrl+Shift+Enter is kept for the launcher's run as
//!    administrator; the UI consumes it once (`take_gesture_time`). It never swallows.
//!
//! Takeover rules (P3-dock-suite's TakeoverChord), after the launcher's and before the Win branch, only for an Esc event
//! while the UI owns a takeover bit (`DockChords::takeovers`) or an Esc the takeover swallowed is held; any other key
//! costs one compare:
//!  * An Esc down Windows does not hold yet, with exactly Ctrl and Shift (no Alt, no Win, either side), Ctrl+Shift+Esc
//!    owned, the UI's gate saying Tasks is ready and not in front, the breaker closed or expired, the shell thread
//!    waiting in GetMessageW, the shell up and no post left unacknowledged for TAKEOVER_ACK_TIMEOUT_MS (failed open:
//!    Windows gets the chord) is posted to the shell thread (WM_SHELL_TAKEOVER, the next sequence, kb.time) and
//!    swallowed, the menu-mask key injected while Ctrl and Shift are down; a failed post passes it. Its auto-repeat and
//!    its up are swallowed too, until its up or REPEAT_WINDOW_MS without an Esc event; the watchdog's repair of a stale
//!    Win press forgets it.
//!  * The Win branch sees the dock's rows only (`DockChords::rows`): a takeover owned alone leaves it release 1.5.2's.
//!
//! The callback does NO I/O nor heavy work (LL hook timeout ~300 ms):
//! it only posts messages to the window, which logs and acts.
//!
//! The hook lives in a dedicated thread (`hook`) that only runs a message loop: the callback
//! is served by that thread, independently of rendering, timers and logging of the UI thread.
//! A thread timer of that thread reinstalls the hook (`reinstall`) every 2 s while keys reach the callback or the UI
//! notes activity (`note_activity`), and stops after a quiet period: with no input nothing can remove the hook.

use std::cell::Cell;
use std::sync::atomic::Ordering::{AcqRel, Acquire, Relaxed, Release};
use std::sync::atomic::{AtomicBool, AtomicIsize, AtomicU16, AtomicU32, AtomicU64, AtomicU8};
use std::sync::mpsc::{self, Receiver, Sender};
use std::thread::JoinHandle;
use std::time::Duration;

use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, WPARAM};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::System::SystemInformation::GetTickCount;
use windows::Win32::System::Threading::GetCurrentThreadId;
use windows::Win32::System::Threading::{GetCurrentThread, SetThreadPriority, THREAD_PRIORITY_TIME_CRITICAL};
use windows::Win32::UI::Accessibility::{SetWinEventHook, UnhookWinEvent, HWINEVENTHOOK};
use windows::Win32::UI::Input::KeyboardAndMouse::{
    GetAsyncKeyState, SendInput, INPUT, INPUT_0, INPUT_KEYBOARD, KEYBDINPUT, KEYBD_EVENT_FLAGS, KEYEVENTF_EXTENDEDKEY,
    KEYEVENTF_KEYUP, VIRTUAL_KEY, VK_LWIN, VK_RWIN,
};
use windows::Win32::UI::WindowsAndMessaging::{
    CallNextHookEx, DispatchMessageW, GetForegroundWindow, GetMessageW, GetShellWindow, KillTimer, PeekMessageW,
    PostMessageW, PostThreadMessageW, SetCoalescableTimer, SetTimer, SetWindowsHookExW, UnhookWindowsHookEx,
    EVENT_SYSTEM_DESKTOPSWITCH, HC_ACTION, HHOOK, KBDLLHOOKSTRUCT, LLKHF_EXTENDED, LLKHF_INJECTED, MSG, PM_NOREMOVE,
    PM_QS_SENDMESSAGE, TIMERV_NO_COALESCING, WH_KEYBOARD_LL, WINEVENT_OUTOFCONTEXT, WM_APP, WM_KEYDOWN, WM_KEYUP,
    WM_QUIT, WM_SYSKEYDOWN, WM_SYSKEYUP, WM_TIMER,
};

use crate::msg::{
    WM_DOCK_CHORD, WM_DOCK_DIAG, WM_DOCK_LAUNCHER, WM_DOCK_LAUNCHER_PASSED, WM_DOCK_LAUNCHER_YIELDED, WM_DOCK_PEEK,
    WM_DOCK_START_MENU, WM_DOCK_TOGGLE, WM_DOCK_WINKEY, WM_SHELL_TAKEOVER,
};
use crate::{hwnd, log};

/// Events for WM_DOCK_WINKEY (log only).
pub const EV_PRESSED: usize = 0;
pub const EV_ALONE: usize = 1;
pub const EV_COMBO: usize = 2;
pub const EV_SENDFAIL: usize = 3;
pub const EV_DOUBLE: usize = 4;
/// The release of a tracked Win press that Windows held down (a key-down of it went by the hook unseen): it passed
/// behind the menu-mask key and toggled nothing (`win_up_plan`).
pub const EV_LEAK: usize = 5;

/// Marker in dwExtraInfo of the events we inject ("DOCK").
pub const INJECT_TAG: usize = 0x4B43_4F44;
/// Beyond this interval (ms) from the last Win event the "Win held" state is considered
/// stuck (e.g. Win+L: the release goes to the secure desktop). Auto-repeat keeps renewing it.
const REPEAT_WINDOW_MS: u32 = 1500;
/// A lone Win press (no other key) is cleared by the watchdog only beyond this age (ms): it may be
/// held with no auto-repeat, but a release lost on the secure desktop must still be repaired.
const LONE_STUCK_MS: u32 = 10_000;
/// Double tap: a lone Win press that starts within this interval (ms) of the previous lone Win
/// release posts WM_DOCK_START_MENU instead of WM_DOCK_TOGGLE.
pub const DOUBLE_TAP_MS: u32 = 300;

/// Period (ms) of the hook thread's reinstall tick (`reinstall`): armed by activity, stopped after a quiet period.
const REINSTALL_MS: u32 = 2000;
/// `WM_HOOK_NOTE` from any thread (`note_activity`): activity the UI thread saw, which arms the reinstall tick.
const NOTE_ACTIVITY: usize = 4;
/// Windows' low-level hook time-out (ms) when LowLevelHooksTimeout is not set (Windows 11 passes each key of a stalled
/// callback on after about 0.3 s): a key the callback has not answered by then goes on without the hook.
const HOOK_TIMEOUT_MS: u32 = 300;
/// One step (ms) of the tick count: GetTickCount moves with the system timer (15.6 ms by default), so a gap measured
/// on it can read up to one step short of the time that passed.
const TICK_STEP_MS: u32 = 16;
/// A stall (ms): the hook thread did not serve the keyboard in time (`stall_check`, `heartbeat`). 50 ms (three steps
/// of the tick count) below HOOK_TIMEOUT_MS, so every key whose answer Windows gave up on shows as a stall, and
/// thousands of times a callback's usual cost (tens of microseconds).
const STALL_MS: u32 = 250;
/// Period (ms) of the heartbeat (`heartbeat`). With STALL_MS and one step of the tick count it stays below
/// HOOK_TIMEOUT_MS, so a pause of the hook thread long enough for Windows to give up on a key, even when the tick
/// count reads it one step short, makes the next tick more than STALL_MS late whatever its phase; about 32 wake-ups a
/// second, only while a press that owned a chord or passed a modifier is held.
const HEARTBEAT_MS: u32 = 30;
const _: () = assert!(HEARTBEAT_MS + STALL_MS + TICK_STEP_MS < HOOK_TIMEOUT_MS);

/// Thread id of the running hook thread (0 = none): `note_activity` posts to it.
static HOOK_TID: AtomicU32 = AtomicU32::new(0);
/// A failed SetTimer of the reinstall tick is logged once per process.
static TICK_FAIL_LOGGED: AtomicBool = AtomicBool::new(false);
/// A stall note of the callback (`note_stall`) waits in the hook thread's queue.
static STALL_NOTED: AtomicBool = AtomicBool::new(false);

thread_local! {
    // Hook thread only: the LL callback runs on it, inside its GetMessageW, so nothing else reads or writes these.
    static TICK: Cell<Tick> = const { Cell::new(Tick { armed: false, active: false }) };
    /// Id of the armed reinstall tick (a thread timer of the hook thread), 0 = none.
    static TICK_ID: Cell<usize> = const { Cell::new(0) };
    /// The armed Peek timer (a thread timer of the hook thread) and the press it was armed for (`WinState::press`);
    /// id 0 = none. It exists only between a Win down and its release.
    static PEEK_TIMER: Cell<(usize, u32)> = const { Cell::new((0, 0)) };
    /// The heartbeat (a thread timer of the hook thread, `heartbeat`) and the tick count of its arming or of its last
    /// tick; id 0 = none. It exists only while `heartbeat_due`.
    static HEARTBEAT: Cell<(usize, u32)> = const { Cell::new((0, 0)) };
}

/// The reinstall tick: armed by the first activity (a key reaching the hook, or a note from the UI), it ticks every
/// `REINSTALL_MS` while activity comes within each period (or a Win press is tracked) and stops after a quiet one.
/// Windows removes a low-level hook only when its callback overran LowLevelHooksTimeout, which needs input: with no
/// activity there is nothing to repair.
#[derive(Clone, Copy, Default)]
struct Tick {
    armed: bool,
    active: bool,
}

impl Tick {
    /// Activity seen. true when the tick was not armed: the caller arms the timer (and clears `armed` again if
    /// SetTimer fails, so the next activity tries again).
    fn on_activity(&mut self) -> bool {
        self.active = true;
        !std::mem::replace(&mut self.armed, true)
    }

    /// A tick ran: true (keep ticking) when activity came in its period or a Win press is tracked, so `repair_plan` still
    /// sees a press whose release was lost.
    fn on_tick(&mut self, win_down: bool) -> bool {
        let keep = self.active || win_down;
        self.active = false;
        self.armed = keep;
        keep
    }
}

static WIN_DOWN: AtomicBool = AtomicBool::new(false);
static WIN_OTHER: AtomicBool = AtomicBool::new(false);
static WIN_VK: AtomicU32 = AtomicU32::new(0);
static WIN_LAST_TIME: AtomicU32 = AtomicU32::new(0);
static HOOK_RAW: AtomicIsize = AtomicIsize::new(0);
/// Time (`kb.time`) of the last lone Win release that toggled the dock; 0 = none (or already
/// consumed by a double tap, so a third tap toggles again).
static LAST_ALONE_UP: AtomicU32 = AtomicU32::new(0);
/// true if the current Win press started within `DOUBLE_TAP_MS` of `LAST_ALONE_UP`.
static WIN_DOUBLE: AtomicBool = AtomicBool::new(false);
/// Experiment: if true a lone Win is swallowed but does NOT toggle the dock.
pub static NO_TOGGLE: AtomicBool = AtomicBool::new(false);
/// Opt-in diagnostics (`--diag`): if false `diag()` returns immediately.
pub static DIAG: AtomicBool = AtomicBool::new(false);

fn set_hook() -> Result<isize, String> {
    // SAFETY: no preconditions.
    let module = unsafe { GetModuleHandleW(None) }.map_err(|e| format!("GetModuleHandleW: {e}"))?;
    // SAFETY: the callback is `extern "system"` and lives for the whole process; thread id 0 = global.
    // Invariant: it must be called from the `hook` thread, which is also the one serving the callback.
    let h = unsafe { SetWindowsHookExW(WH_KEYBOARD_LL, Some(ll_keyboard_proc), Some(module.into()), 0) }
        .map_err(|e| format!("SetWindowsHookExW: {e}"))?;
    Ok(h.0 as isize)
}

/// Hook thread started: `tid` is used to post messages to it, `done` signals the end.
pub struct HookThread {
    tid: u32,
    done: Receiver<()>,
    join: JoinHandle<()>,
}

/// Starts the `hook` thread and waits for the installation result. Must be called AFTER
/// creating the dock window (the callback posts messages to `hwnd()`).
pub fn start() -> Result<HookThread, String> {
    #[cfg(feature = "owner-probe")]
    MASK_KEY.store(crate::launcher_probe::mask_key(), Relaxed);
    #[cfg(feature = "owner-probe")]
    ACCEPT_INJECTED.store(crate::launcher_probe::accept_injected(), Relaxed);
    // The stall switch of live check K-3, read here so the callback never reads the environment.
    #[cfg(feature = "owner-probe")]
    let _ = crate::launcher_probe::hook_stall_ms();
    let (ready_tx, ready_rx) = mpsc::channel::<Result<u32, String>>();
    let (done_tx, done_rx) = mpsc::channel::<()>();
    let join = std::thread::Builder::new()
        .name("hook".into())
        .spawn(move || thread_main(ready_tx, done_tx))
        .map_err(|e| format!("starting hook thread: {e}"))?;
    match ready_rx.recv() {
        Ok(Ok(tid)) => Ok(HookThread { tid, done: done_rx, join }),
        Ok(Err(e)) => Err(e),
        Err(_) => Err("the hook thread ended before installing the hook".into()),
    }
}

/// Body of the `hook` thread: installs the hook and runs ONLY the message loop.
fn thread_main(ready: Sender<Result<u32, String>>, done: Sender<()>) {
    let mut msg = MSG::default();
    // SAFETY: `msg` is valid and writable. PM_NOREMOVE on an empty queue only serves to create
    // the thread message queue, so PostThreadMessageW from the UI thread cannot fail.
    let _ = unsafe { PeekMessageW(&mut msg, None, 0, 0, PM_NOREMOVE) };
    // Maximum priority for the hook thread: the LL callback must respond within
    // LowLevelHooksTimeout even with a game saturating the CPU.
    // SAFETY: GetCurrentThread returns an always-valid pseudo-handle.
    if let Err(e) = unsafe { SetThreadPriority(GetCurrentThread(), THREAD_PRIORITY_TIME_CRITICAL) } {
        let _ = e; // not critical
    }
    match set_hook() {
        Ok(raw) => {
            HOOK_RAW.store(raw, Relaxed);
            log!("low-level keyboard hook installed (dedicated thread)");
            // SAFETY: no preconditions.
            let tid = unsafe { GetCurrentThreadId() };
            HOOK_TID.store(tid, Relaxed);
            let _ = ready.send(Ok(tid));
        }
        Err(e) => {
            let _ = ready.send(Err(e));
            let _ = done.send(());
            return;
        }
    }
    // A desktop switch (a UAC prompt, Ctrl+Alt+Del, the lock screen) takes the keyboard from this hook: the event's
    // callback runs on this thread's loop, out of context, and forgets what the keys of the other desktop may have ended.
    // SAFETY: `desktop_switched` is a 'static WINEVENTPROC; out of context no module is needed; this thread pumps
    // (GetMessageW below), as out-of-context delivery requires.
    let desk = unsafe {
        SetWinEventHook(
            EVENT_SYSTEM_DESKTOPSWITCH,
            EVENT_SYSTEM_DESKTOPSWITCH,
            None,
            Some(desktop_switched),
            0,
            0,
            WINEVENT_OUTOFCONTEXT,
        )
    };
    if desk.is_invalid() {
        log!("hook: desktop switches not watched: {}", windows::core::Error::from_thread());
    }
    loop {
        // SAFETY: `msg` is valid and writable; filter None = all messages of the thread.
        let r = unsafe { GetMessageW(&mut msg, None, 0, 0) };
        if r.0 <= 0 {
            break; // WM_QUIT (0) or error (-1)
        }
        let peek = PEEK_TIMER.with(Cell::get);
        let peek_tick = msg.message == WM_TIMER && msg.hwnd.0.is_null() && peek.0 != 0 && msg.wParam.0 == peek.0;
        let beat = HEARTBEAT.with(Cell::get);
        let beat_tick = msg.message == WM_TIMER && msg.hwnd.0.is_null() && beat.0 != 0 && msg.wParam.0 == beat.0;
        probe!({
            // The Peek timer's and the heartbeat's ids change at each arming: each is counted under one fixed id
            // (`is_peek_timer`, `is_heartbeat_timer`).
            let mut m = msg;
            if peek_tick {
                m.wParam = WPARAM(PEEK_PROBE_ID);
            }
            if beat_tick {
                m.wParam = WPARAM(HEARTBEAT_PROBE_ID);
            }
            crate::probe::thread_msg(crate::probe::Th::Hook, &m)
        });
        if peek_tick {
            kill_peek_timer();
            peek_fired(peek.1);
            continue;
        }
        if beat_tick {
            // A late tick: the thread did not run for more than STALL_MS; the heartbeat then ends unless still due.
            // SAFETY: GetTickCount has no preconditions.
            let now = unsafe { GetTickCount() };
            HEARTBEAT.with(|b| b.set((beat.0, now)));
            if beat_late(beat.1, now) {
                stalled(CutOff::LateHeartbeat);
            }
            heartbeat(heartbeat_due(&load_win_state()));
            continue;
        }
        let tick_id = TICK_ID.with(Cell::get);
        if msg.message == WM_TIMER && msg.hwnd.0.is_null() && tick_id != 0 && msg.wParam.0 == tick_id {
            reinstall();
            let mut tick = TICK.with(Cell::get);
            if !tick.on_tick(WIN_DOWN.load(Relaxed)) {
                // SAFETY: the thread timer `activity` armed on this thread; fails harmlessly if it is already gone.
                let _ = unsafe { KillTimer(None, tick_id) };
                TICK_ID.with(|t| t.set(0));
            }
            TICK.with(|t| t.set(tick));
            continue;
        }
        if msg.message == WM_HOOK_NOTE {
            if msg.wParam.0 == NOTE_ACTIVITY {
                activity();
            } else if msg.wParam.0 == NOTE_LAUNCHER_FAIL_OPEN {
                let seq = msg.lParam.0 as u32;
                log!(
                    "hook: launcher chord failed open (seq {seq}): the UI thread did not acknowledge for {ACK_TIMEOUT_MS} ms"
                );
            } else if msg.wParam.0 == NOTE_PEEK_TIMER_FAIL {
                log!("hook: Peek timer not armed (SetTimer failed): Peek is skipped");
            } else if msg.wParam.0 == NOTE_CUT_OFF {
                if let Some(why) = CutOff::from_code(msg.lParam.0) {
                    cut_off_now(why);
                }
            } else if msg.wParam.0 == NOTE_STALL {
                STALL_NOTED.store(false, Relaxed);
                if let Some(why) = CutOff::from_code(msg.lParam.0) {
                    stalled(why);
                }
            }
            continue;
        }
        // SAFETY: `msg` just filled by GetMessageW (the thread has no windows: in practice
        // only thread messages arrive here, but forwarding is harmless).
        unsafe { DispatchMessageW(&msg) };
    }
    HOOK_TID.store(0, Relaxed);
    if !desk.is_invalid() {
        // SAFETY: the handle SetWinEventHook returned on this thread, removed once.
        let _ = unsafe { UnhookWinEvent(desk) };
    }
    uninstall();
    let _ = done.send(());
}

/// Activity the UI thread sees (a foreground change, the bar shown or hidden, resume from sleep): arms the hook thread's
/// reinstall tick, in case the callback itself no longer runs. Any thread; a no-op while no hook thread runs, and a
/// failed post is harmless.
pub fn note_activity() {
    let tid = HOOK_TID.load(Relaxed);
    if tid == 0 {
        return;
    }
    // SAFETY: PostThreadMessageW is thread-safe; if the thread has already ended it merely fails.
    let _ = unsafe { PostThreadMessageW(tid, WM_HOOK_NOTE, WPARAM(NOTE_ACTIVITY), LPARAM(0)) };
}

/// Positive evidence that keys went by the hook unseen, a release among them (`cut_off`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CutOff {
    /// Another desktop had the keyboard: a UAC prompt, Ctrl+Alt+Del, the lock screen (EVENT_SYSTEM_DESKTOPSWITCH).
    DesktopSwitch,
    /// The session was locked, unlocked, disconnected or connected (WM_WTSSESSION_CHANGE).
    Session,
    /// Suspend or resume (WM_POWERBROADCAST).
    Power,
    /// A remote-desktop or VM client left the front: while it was there its own hook could take keys before ours.
    RemoteClient,
    /// The reinstall found the hook already removed by Windows (a callback overran LowLevelHooksTimeout).
    HookRemoved,
    /// A window above the dock came to the front or left it: the UAC prompt shown without the secure desktop, or a
    /// process the dock may not look at. The keys typed while it is in front never reach this hook.
    WindowAbove,
    /// A callback returned more than STALL_MS after its entry (`stall_check`): Windows passed on the keys that waited.
    SlowCallback,
    /// A key reached the callback more than STALL_MS after its time stamp (`stall_check`): the hook thread was not
    /// serving the keyboard, and the keys before it may have gone on without the hook.
    LateKey,
    /// The heartbeat of a held press ticked more than STALL_MS late (`heartbeat`): the hook thread was not running.
    LateHeartbeat,
}

impl CutOff {
    const ALL: [CutOff; 9] = [
        CutOff::DesktopSwitch,
        CutOff::Session,
        CutOff::Power,
        CutOff::RemoteClient,
        CutOff::HookRemoved,
        CutOff::WindowAbove,
        CutOff::SlowCallback,
        CutOff::LateKey,
        CutOff::LateHeartbeat,
    ];

    /// The lParam of `NOTE_CUT_OFF` back to its `CutOff` (the index in `ALL`).
    fn from_code(code: isize) -> Option<CutOff> {
        usize::try_from(code).ok().and_then(|i| CutOff::ALL.get(i)).copied()
    }
}

/// Any thread, on positive evidence that keys went by the hook unseen (`CutOff`): the hook thread forgets its Win press
/// and the tracked modifiers (`cut_off_now`). A no-op while no hook thread runs; a failed post is harmless.
pub fn cut_off(why: CutOff) {
    let tid = HOOK_TID.load(Relaxed);
    if tid == 0 {
        return;
    }
    // SAFETY: PostThreadMessageW is thread-safe; if the thread has already ended it merely fails.
    let _ = unsafe { PostThreadMessageW(tid, WM_HOOK_NOTE, WPARAM(NOTE_CUT_OFF), LPARAM(why as isize)) };
}

/// Hook thread: a cut-off (`CutOff`) on the Win statics. While a row is owned, or the press is one of the dock's, the
/// press is forgotten (`forget_after_cut_off`, its Peek ended) and so are the tracked modifiers (`reset_modifiers`):
/// a lost Win release must not turn the digits, T and B typed next into chords, nor a lost Shift or Alt pick the wrong
/// row. Without rows the 1.5.2 Win branch keeps its own stale rule and nothing changes.
fn cut_off_now(why: CutOff) {
    let rows = DockChords(DOCK_CHORDS.load(Acquire));
    let mut s = load_win_state();
    let held = s.down;
    let Some(peek_end) = forget_after_cut_off(&mut s, rows) else { return };
    store_win_state(&s);
    kill_peek_timer();
    kill_heartbeat();
    if peek_end {
        // SAFETY: GetTickCount has no preconditions. Same clock as `kb.time`.
        post_peek(false, unsafe { GetTickCount() });
    }
    reset_modifiers();
    if held {
        log!("hook: input cut off ({why:?}): the Win press and the modifiers are forgotten");
    }
}

/// EVENT_SYSTEM_DESKTOPSWITCH, on the hook thread's loop (out of context): the keys of the other desktop never reached
/// this hook.
unsafe extern "system" fn desktop_switched(
    _hook: HWINEVENTHOOK,
    _event: u32,
    _hwnd: HWND,
    _id_object: i32,
    _id_child: i32,
    _thread: u32,
    _time: u32,
) {
    cut_off_now(CutOff::DesktopSwitch);
}

/// Hook thread, on stall evidence (the callback's `note_stall`, a late tick of the heartbeat): the cut-off, then the
/// hook reinstalled at once rather than at the next tick (Windows may have removed it for the overrun), so a fresh Win
/// press is seen at once; one line when it is back.
fn stalled(why: CutOff) {
    cut_off_now(why);
    let old = HOOK_RAW.load(Relaxed);
    reinstall();
    let new = HOOK_RAW.load(Relaxed);
    if new != 0 && new != old {
        log!("hook: reinstalled after a stall ({why:?})");
    }
}

/// Activity on the hook thread (a key reaching the callback, or `NOTE_ACTIVITY`): marks the current tick period active
/// and arms the tick when it is not armed (one SetTimer per activity burst, nothing per key once armed).
fn activity() {
    let mut tick = TICK.with(Cell::get);
    if tick.on_activity() {
        // SAFETY: a thread timer of the calling (hook) thread, no callback: its WM_TIMER reaches `thread_main`'s loop.
        let id = unsafe { SetTimer(None, 0, REINSTALL_MS, None) };
        if id == 0 {
            tick.armed = false;
            if !TICK_FAIL_LOGGED.swap(true, Relaxed) {
                log!("hook: reinstall tick not armed: {}", windows::core::Error::from_thread());
            }
        } else {
            TICK_ID.with(|t| t.set(id));
        }
    }
    TICK.with(|t| t.set(tick));
}

/// Closes the hook thread: WM_QUIT, then waits (max 2 s) for it to remove the hook.
pub fn stop(t: HookThread) {
    // SAFETY: as above.
    let _ = unsafe { PostThreadMessageW(t.tid, WM_QUIT, WPARAM(0), LPARAM(0)) };
    match t.done.recv_timeout(Duration::from_secs(2)) {
        Ok(()) => {
            let _ = t.join.join();
        }
        Err(_) => log!("hook thread: no response within 2 s, proceeding without join"),
    }
}

#[derive(Debug, PartialEq, Eq)]
enum Repair {
    Go,
    ClearThenGo,
    Skip,
}

/// What the watchdog does given the tracked `WIN_DOWN`, the async state of the Win keys, `WIN_OTHER` and
/// the age (ms) of the last Win event (auto-repeat included, or a key the press kept, `renew`).
/// A Win key the async state reads down is held for Windows (an injected pair): the hook is not touched in the middle
/// of that combination. A press only the hook tracks (its Win-down swallowed) keeps its state and the hook is
/// reinstalled: a callback stall during it (Windows removed the hook, so its release never came) has the hook back at
/// the next tick, not `LONE_STUCK_MS` later.
/// The async state alone is not trusted: a Win-down our hook swallowed never reaches it, so a Win held
/// alone reads as "up". A press unseen by the async state and older than `REPEAT_WINDOW_MS` is cleared
/// only if a chord was injected (Win+L) or it is older than `LONE_STUCK_MS`: a lone Win can be held
/// with no auto-repeat reaching the hook, and its release must still toggle.
fn repair_plan(win_down: bool, lwin_pressed: bool, rwin_pressed: bool, win_other: bool, age_ms: u32) -> Repair {
    if !win_down {
        return Repair::Go;
    }
    if lwin_pressed || rwin_pressed {
        return Repair::Skip;
    }
    let over = age_ms > REPEAT_WINDOW_MS && (win_other || age_ms > LONE_STUCK_MS);
    if over {
        Repair::ClearThenGo
    } else {
        Repair::Go
    }
}

/// Watchdog (run on the hook thread): Windows silently removes an LL hook that exceeds
/// LowLevelHooksTimeout several times. We ALWAYS reinstall (first the new one, then remove the
/// old one: no instant without a hook), and the old one answers the keys already sent to it before it goes
/// (`answer_sent_keys`). The Win state is in a static, so it survives; between the two calls only that answer pumps
/// on this thread. It is skipped while the async state reads a tracked Win key down (Windows holds the
/// injected Win down), so as not to touch the hook in the middle of that combination; a press only the hook tracks
/// is kept across the reinstall. If `WIN_DOWN` is set, no Win key is down and the press
/// is over (Win+L: the release went to the secure desktop; see `repair_plan`), the state is cleared first. A Win key
/// Windows holds for a press the hook swallowed is noted on the press first (`note_leak`).
fn reinstall() {
    if HOOK_RAW.load(Relaxed) == 0 {
        return;
    }
    // SAFETY: GetAsyncKeyState has no preconditions; the high bit (negative) means "down".
    let (l, r) = unsafe { (GetAsyncKeyState(VK_LWIN.0 as i32) < 0, GetAsyncKeyState(VK_RWIN.0 as i32) < 0) };
    // SAFETY: GetTickCount has no preconditions. Same clock as `kb.time`.
    let age = unsafe { GetTickCount() }.wrapping_sub(WIN_LAST_TIME.load(Relaxed));
    // Windows holds the Win key of a press the hook swallowed (`note_leak`): the press notes it, so its release passes
    // (`win_up_plan`), with one line per press. Load, note and store with no pump in between: the callback, which runs
    // only inside this thread's pumps, cannot interleave.
    let mut press = load_win_state();
    if note_leak(&mut press, l, r) {
        store_win_state(&press);
        log!(
            "hook watchdog: Windows holds Win (vk {:#x}) of a press the hook swallowed, with no pair injected: a key-down went by unseen",
            press.vk
        );
    }
    match repair_plan(WIN_DOWN.load(Relaxed), l, r, WIN_OTHER.load(Relaxed), age) {
        Repair::Skip => return,
        Repair::ClearThenGo => {
            let mut s = load_win_state();
            let peek_end = clear_for_repair(&mut s);
            store_win_state(&s);
            // An Esc press the takeover swallowed is forgotten with it: its up may have gone by unseen too.
            let mut take = load_take_state();
            clear_take_for_repair(&mut take);
            store_take_state(&take);
            kill_peek_timer();
            kill_heartbeat();
            if peek_end {
                // SAFETY: GetTickCount has no preconditions. Same clock as `kb.time`.
                post_peek(false, unsafe { GetTickCount() });
            }
            log!("hook watchdog: stale Win state cleared");
        }
        Repair::Go => {}
    }
    match set_hook() {
        Ok(new) => {
            let old = HOOK_RAW.swap(new, Relaxed);
            answer_sent_keys();
            // SAFETY: `old` is an HHOOK of ours; if Windows had already removed it, this fails
            // with no effect and the error is intended (it is the case the watchdog covers).
            if unsafe { UnhookWindowsHookEx(HHOOK(old as *mut _)) }.is_err() {
                // Removed by Windows: keys went by unseen until now, a Win or modifier release among them.
                cut_off_now(CutOff::HookRemoved);
            }
            // Owner-probe builds: one line per reinstall, so a live check matches a key to a reinstall.
            #[cfg(feature = "owner-probe")]
            reinstalled_line();
        }
        Err(e) => log!("hook watchdog: reinstall failed: {e}"),
    }
}

/// `reinstall`, between the new hook's install and the old hook's removal: a key Windows handed to the old hook before
/// the install waits in this thread's queue as a sent call, which the old hook answers here while it is installed.
/// Removed first, it would never answer, and Windows would pass the key on unseen (a Win auto-repeat of a press the
/// hook swallows: Windows then holds a Win whose release the hook swallows too). Every later key goes to the new hook;
/// one that comes during this call and passes reaches the old hook too (CallNextHookEx), whose second look passes it
/// again and changes nothing (`a_passed_key_seen_twice_changes_nothing_more`). Only the calls sent to this thread run
/// here: no posted message and no timer (this thread's timers have no callback, the loop alone handles them), so no
/// note, tick, heartbeat or Peek timer runs inside it. A key a foreign hook above ours hands on to the old hook after
/// this call is still lost; the release rule (`win_up_plan`) covers that and a time-out alike.
fn answer_sent_keys() {
    let mut msg = MSG::default();
    // SAFETY: `msg` is valid and writable. PM_QS_SENDMESSAGE handles the sent calls only, and PM_NOREMOVE leaves any
    // posted message for the loop.
    let _ = unsafe { PeekMessageW(&mut msg, None, 0, 0, PM_NOREMOVE | PM_QS_SENDMESSAGE) };
}

/// Owner-probe builds, after each reinstall: one line with the tick count (kb.time's clock), so a live check matches a
/// key to a reinstall. Release builds have none: the reinstall runs every 2 s while keys come.
#[cfg(feature = "owner-probe")]
fn reinstalled_line() {
    // SAFETY: GetTickCount has no preconditions.
    let tick = unsafe { GetTickCount() };
    log!("hook: reinstalled at tick {tick}");
}

fn uninstall() {
    let raw = HOOK_RAW.swap(0, Relaxed);
    if raw == 0 {
        return;
    }
    // SAFETY: `raw` is the HHOOK returned by SetWindowsHookExW, removed only once (swap),
    // by the same thread that installed it.
    match unsafe { UnhookWindowsHookEx(HHOOK(raw as *mut _)) } {
        Ok(()) => log!("keyboard hook removed"),
        Err(e) => log!("UnhookWindowsHookEx failed: {e}"),
    }
}

unsafe extern "system" fn ll_keyboard_proc(code: i32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    #[cfg(feature = "owner-probe")]
    let started = std::time::Instant::now();
    if code == HC_ACTION as i32 {
        // The stall evidence's entry time, only while a row is owned (`stall_watch`).
        let entered = stall_watch();
        // First, before `handle_key` (which returns at once for injected keys): any key keeps the reinstall tick going.
        activity();
        // SAFETY: for HC_ACTION in a WH_KEYBOARD_LL hook, lparam points to a
        // KBDLLHOOKSTRUCT valid for the whole duration of the callback.
        let kb = unsafe { &*(lp.0 as *const KBDLLHOOKSTRUCT) };
        #[cfg(feature = "owner-probe")]
        crate::launcher_probe::key_seen(wp.0 as u32, kb.time);
        let swallowed = handle_key(wp.0 as u32, kb);
        probe!(crate::probe::kb(wp.0 as u32, kb.flags.0, swallowed));
        diag(wp.0 as u32, kb, swallowed);
        #[cfg(feature = "owner-probe")]
        crate::launcher_probe::record_hook_us(started.elapsed());
        if let Some(entered) = entered {
            stall_check(entered, kb);
        }
        if swallowed {
            return LRESULT(1);
        }
    }
    // SAFETY: unchanged forwarding to the hook chain.
    unsafe { CallNextHookEx(None, code, wp, lp) }
}

/// Returns true if the event must be swallowed. In order: the launcher chord, the takeover (an Esc only), the Win branch.
#[deny(clippy::unwrap_used, clippy::expect_used, clippy::indexing_slicing)]
fn handle_key(msg: u32, kb: &KBDLLHOOKSTRUCT) -> bool {
    if !accepted(kb.flags.0, kb.dwExtraInfo) {
        return false; // our own re-injected event, or injected by another app: not a physical key
    }
    let down = msg == WM_KEYDOWN || msg == WM_SYSKEYDOWN;
    let up = msg == WM_KEYUP || msg == WM_SYSKEYUP;
    if !down && !up {
        return false;
    }
    let vk = kb.vkCode;
    let launcher = LAUNCHER_CHORD.load(Acquire) & CHORD_SET != 0;
    let chords = DockChords(DOCK_CHORDS.load(Acquire));
    let mods = if tracks(launcher, chords) { mods_step(vk, down, kb.time) } else { Mods::NONE };
    if launcher {
        note_gesture(&GESTURE_TIME, kb.flags.0, kb.dwExtraInfo, vk, down, mods, kb.time);
        if let Some(swallow) = launcher_step(kb, down, mods) {
            return swallow;
        }
    }
    // The takeover (Ctrl+Shift+Esc to Tasks): an Esc event while a takeover is owned or an Esc it swallowed is held.
    if vk == VK_ESCAPE_ && (chords.takeovers() != DockChords::NONE || TAKE_ESC_BIT.load(Relaxed)) {
        let mut t = load_take_state();
        match take_decide(&mut t, take_in(kb, down, mods, chords)) {
            TakeDecision::NotOurs => store_take_state(&t),
            TakeDecision::SwallowSilent => {
                store_take_state(&t);
                return true;
            }
            TakeDecision::Candidate(seq) => {
                if !post_takeover(seq, kb.time) {
                    // A failed post passes the key: Windows opens its Task Manager, as without the takeover.
                    store_take_state(&t);
                    return false;
                }
                take_commit(&mut t, seq, kb.time);
                store_take_state(&t);
                // While Ctrl and Shift are still down: their release is then no lone Ctrl+Shift (no layout switch).
                send_mask();
                return true;
            }
        }
    }
    win_key(kb, down, mods, chords)
}

/// Callback: GetTickCount at its entry while a row is owned, for `stall_check`. None with no row: the callback is
/// release 1.5.2's, and nothing is measured.
#[deny(clippy::unwrap_used, clippy::expect_used, clippy::indexing_slicing)]
fn stall_watch() -> Option<u32> {
    if DOCK_CHORDS.load(Acquire) == DockChords::NONE.0 {
        return None;
    }
    // SAFETY: GetTickCount has no preconditions. Same clock as `kb.time`.
    Some(unsafe { GetTickCount() })
}

/// Callback, while a row was owned at its entry (`entered`): the stall evidence of this event, noted for the hook
/// thread's loop (`note_stall`) and never acted on here, so no answer changes: a late return, the callback returning
/// more than STALL_MS after its entry; a late key, one the hook admits reaching it more than STALL_MS after its time
/// stamp (an injected key the hook ignores is no evidence, whatever its stamp).
#[deny(clippy::unwrap_used, clippy::expect_used, clippy::indexing_slicing)]
fn stall_check(entered: u32, kb: &KBDLLHOOKSTRUCT) {
    // SAFETY: GetTickCount has no preconditions.
    let left = unsafe { GetTickCount() };
    if overran(entered, left) {
        note_stall(CutOff::SlowCallback);
    } else if accepted(kb.flags.0, kb.dwExtraInfo) && came_late(kb.time, entered) {
        note_stall(CutOff::LateKey);
    }
}

/// Callback: stall evidence for the hook thread's loop, which runs the cut-off once the callback has returned
/// (`stalled`). One note waits at a time (STALL_NOTED, cleared by the loop, or at once when the post fails).
#[deny(clippy::unwrap_used, clippy::expect_used, clippy::indexing_slicing)]
fn note_stall(why: CutOff) {
    if STALL_NOTED.swap(true, Relaxed) {
        return;
    }
    // SAFETY: PostThreadMessageW is thread-safe; this thread has a message queue (thread_main's PeekMessageW).
    let posted =
        unsafe { PostThreadMessageW(GetCurrentThreadId(), WM_HOOK_NOTE, WPARAM(NOTE_STALL), LPARAM(why as isize)) };
    if posted.is_err() {
        STALL_NOTED.store(false, Relaxed);
    }
}

/// `now` is more than `limit` ms after `then` on the tick count, which wraps; a `then` ahead of `now` is not.
#[deny(clippy::unwrap_used, clippy::expect_used, clippy::indexing_slicing)]
const fn later_than(then: u32, now: u32, limit: u32) -> bool {
    let d = now.wrapping_sub(then);
    d > limit && d <= i32::MAX as u32
}

/// A late return: a callback entered at `entered` and returning at `left` overran STALL_MS.
#[deny(clippy::unwrap_used, clippy::expect_used, clippy::indexing_slicing)]
const fn overran(entered: u32, left: u32) -> bool {
    later_than(entered, left, STALL_MS)
}

/// A late key: one time-stamped `stamp` (`kb.time`) reached the callback at `entered` more than STALL_MS later. A
/// stamp ahead of the tick count (an injection with a made-up time) is not late.
#[deny(clippy::unwrap_used, clippy::expect_used, clippy::indexing_slicing)]
const fn came_late(stamp: u32, entered: u32) -> bool {
    later_than(stamp, entered, STALL_MS)
}

/// A late tick: the heartbeat ticked at `now` more than HEARTBEAT_MS + STALL_MS after its last tick (or its arming)
/// at `last`.
#[deny(clippy::unwrap_used, clippy::expect_used, clippy::indexing_slicing)]
const fn beat_late(last: u32, now: u32) -> bool {
    later_than(last, now, HEARTBEAT_MS + STALL_MS)
}

/// Hook thread, from the callback or its loop: the heartbeat follows `on` (`heartbeat_due`), armed when it is not (one
/// timer, whose time is the first tick's baseline) and killed when it is no longer due. Windows is asked not to
/// coalesce it, so only a thread that did not run makes a tick late. A failed arming leaves the callback's own
/// evidence, and the next key tries again.
#[deny(clippy::unwrap_used, clippy::expect_used, clippy::indexing_slicing)]
fn heartbeat(on: bool) {
    let (id, _) = HEARTBEAT.with(Cell::get);
    if on && id == 0 {
        // SAFETY: a thread timer of the calling (hook) thread, no callback: its WM_TIMER reaches `thread_main`'s loop.
        let id = unsafe { SetCoalescableTimer(None, 0, HEARTBEAT_MS, None, TIMERV_NO_COALESCING) };
        if id != 0 {
            // SAFETY: GetTickCount has no preconditions.
            let now = unsafe { GetTickCount() };
            HEARTBEAT.with(|b| b.set((id, now)));
        }
    } else if !on && id != 0 {
        kill_heartbeat();
    }
}

/// Hook thread: kills the heartbeat, if any.
#[deny(clippy::unwrap_used, clippy::expect_used, clippy::indexing_slicing)]
fn kill_heartbeat() {
    let (id, _) = HEARTBEAT.with(|b| b.replace((0, 0)));
    if id != 0 {
        // SAFETY: the thread timer `heartbeat` armed on this thread; fails harmlessly if it is already gone.
        let _ = unsafe { KillTimer(None, id) };
    }
}

/// The Win branch on its statics: loads `WinState`, runs `win_step`, stores the state back, then does what the step
/// asks. The state is stored before the SendInput, whose tagged events re-enter the callback and pass. `mods` are the
/// tracked modifiers (Win is the Win state's own), `chords` the owned chords, of which the Win branch takes the rows.
#[deny(clippy::unwrap_used, clippy::expect_used, clippy::indexing_slicing)]
fn win_key(kb: &KBDLLHOOKSTRUCT, down: bool, mods: Mods, chords: DockChords) -> bool {
    // The dock's rows only (bits 0-7): the takeover bits are the takeover block's, never the Win branch's.
    let chords = chords.rows();
    // `post` drops a post while the dock window does not exist: a chord swallowed then would be lost.
    let ui_ready = !hwnd().0.is_null();
    let ev = win_in_of(kb, down, mods, chords, DOCK_FS.load(Acquire), ui_ready, || {
        chord_fail_open(CHORD_SEQ.load(Relaxed), CHORD_ACKED.load(Acquire), CHORD_OLDEST.load(Relaxed), kb.time)
    });
    let mut s = load_win_state();
    // SAFETY: GetAsyncKeyState has no preconditions. Inside the callback it reads the state from before this event
    // (LowLevelKeyboardProc's documentation): down means Windows holds the key this up releases.
    note_release(&mut s, ev, |vk| unsafe { GetAsyncKeyState(vk as i32) } < 0);
    let out = win_step(&mut s, ev);
    store_win_state(&s);
    let mut swallow = out.swallow;
    let mut event = out.note;
    if out.act == WinAct::Inject && !inject_win_and_key(s.vk, kb) {
        // SendInput failed: better to let at least the original key through.
        swallow = false;
        event = Some(EV_SENDFAIL);
    }
    if let Some(ev) = event {
        note(ev);
    }
    match out.act {
        WinAct::Toggle => post_toggle(),
        WinAct::StartMenu => post_start_menu(),
        WinAct::Chord(a) => post_chord(a, kb.time),
        WinAct::None | WinAct::Inject => {}
    }
    if out.sync {
        post_chord(ChordAction::Sync, kb.time);
    }
    if out.mask {
        // After the post, while the modifier is still down: its release is then no lone tap (no menu bar, no IME
        // switch). Before a passed Alt up, the same.
        send_mask();
    }
    if out.peek_end {
        post_peek(false, kb.time);
    }
    peek_timer(out.peek);
    // The heartbeat follows the press while a row is owned (with none, its next tick ends one that is left).
    if chords != DockChords::NONE {
        heartbeat(heartbeat_due(&s));
    }
    // Owner-probe builds, live check K-3: the first owned chord stalls the callback here, after its post and the
    // heartbeat (Windows times out the keys that wait; the late return is a cut-off that puts the hook back).
    #[cfg(feature = "owner-probe")]
    let stall = matches!(out.act, WinAct::Chord(_)).then(crate::launcher_probe::hook_stall_once).flatten();
    #[cfg(feature = "owner-probe")]
    if let Some(ms) = stall {
        std::thread::sleep(Duration::from_millis(ms.into()));
    }
    swallow
}

/// The Win branch's event (`WinIn`) from the callback's reads: Win taken out of `mods` (the Win state has its own) and
/// only the rows of `chords` (`DockChords::rows`), so a takeover bit never reaches `win_step` and a takeover owned alone
/// leaves release 1.5.2's Win branch. `overdue` (`chord_fail_open` on the statics) is read only while a row is owned.
#[deny(clippy::unwrap_used, clippy::expect_used, clippy::indexing_slicing)]
fn win_in_of(
    kb: &KBDLLHOOKSTRUCT,
    down: bool,
    mods: Mods,
    chords: DockChords,
    fullscreen: bool,
    ui_ready: bool,
    overdue: impl FnOnce() -> bool,
) -> WinIn {
    let rows = chords.rows();
    WinIn {
        vk: kb.vkCode,
        down,
        time: kb.time,
        mods: Mods(mods.0 & !Mods::WIN.0),
        chords: rows,
        fullscreen,
        fail_open: rows != DockChords::NONE && overdue(),
        ui_ready,
    }
}

/// Hook thread: the Peek timer command of a Win event. Arm: one SetTimer (a timer still armed for an earlier press,
/// whose release was lost, is killed first); a failed SetTimer skips Peek and notes it once to the loop, which logs it.
/// Kill: one KillTimer when a timer is armed.
#[deny(clippy::unwrap_used, clippy::expect_used, clippy::indexing_slicing)]
fn peek_timer(cmd: PeekCmd) {
    match cmd {
        PeekCmd::Keep => {}
        PeekCmd::Kill => kill_peek_timer(),
        PeekCmd::Arm(press) => {
            kill_peek_timer();
            // SAFETY: a thread timer of the calling (hook) thread, no callback: its WM_TIMER reaches `thread_main`'s loop.
            let id = unsafe { SetTimer(None, 0, PEEK_MS, None) };
            if id != 0 {
                PEEK_TIMER.with(|t| t.set((id, press)));
            } else if !PEEK_FAIL_NOTED.swap(true, Relaxed) {
                // SAFETY: PostThreadMessageW is thread-safe; this thread has a message queue (thread_main's PeekMessageW).
                let _ = unsafe {
                    PostThreadMessageW(GetCurrentThreadId(), WM_HOOK_NOTE, WPARAM(NOTE_PEEK_TIMER_FAIL), LPARAM(0))
                };
            }
        }
    }
}

/// Hook thread: kills the armed Peek timer, if any.
#[deny(clippy::unwrap_used, clippy::expect_used, clippy::indexing_slicing)]
fn kill_peek_timer() {
    let (id, _) = PEEK_TIMER.with(|t| t.replace((0, 0)));
    if id != 0 {
        // SAFETY: the thread timer `peek_timer` armed on this thread; fails harmlessly if it is already gone.
        let _ = unsafe { KillTimer(None, id) };
    }
}

/// Hook thread's loop, the Peek timer of `press` fired (and was killed): Peek begins when the press is still Win alone
/// (`peek_due`), PEEK and DIGITS are still owned and the dock window exists.
#[deny(clippy::unwrap_used, clippy::expect_used, clippy::indexing_slicing)]
fn peek_fired(press: u32) {
    let chords = DockChords(DOCK_CHORDS.load(Acquire));
    let mut s = load_win_state();
    let owned = chords.has(DockChords::PEEK) && chords.has(DockChords::DIGITS);
    if owned && !hwnd().0.is_null() && peek_due(&s, press, DOCK_FS.load(Acquire)) {
        s.peeking = true;
        store_win_state(&s);
        // SAFETY: GetTickCount has no preconditions. Same clock as `kb.time`.
        post_peek(true, unsafe { GetTickCount() });
    }
}

/// Wake-probe builds: the fixed id the hook thread's loop counts the Peek timer's WM_TIMER under (`probe::timer_name`).
#[cfg(feature = "wake-probe")]
const PEEK_PROBE_ID: usize = 0xFF_FFFF;

/// Wake-probe builds: the hook thread's timer `id` (as the probe keys it) is the Peek timer's, not the reinstall tick's.
#[cfg(feature = "wake-probe")]
pub(crate) fn is_peek_timer(id: u32) -> bool {
    id as usize == PEEK_PROBE_ID
}

/// Wake-probe builds: the fixed id the hook thread's loop counts the heartbeat's WM_TIMER under (`probe::timer_name`).
#[cfg(feature = "wake-probe")]
const HEARTBEAT_PROBE_ID: usize = 0xFF_FFFE;

/// Wake-probe builds: the hook thread's timer `id` (as the probe keys it) is the heartbeat's.
#[cfg(feature = "wake-probe")]
pub(crate) fn is_heartbeat_timer(id: u32) -> bool {
    id as usize == HEARTBEAT_PROBE_ID
}

/// Posts WM_DOCK_PEEK: Peek began (`on`) or ended; lParam = the time of the event that decided it.
#[deny(clippy::unwrap_used, clippy::expect_used, clippy::indexing_slicing)]
fn post_peek(on: bool, time: u32) {
    probe!(crate::probe::post_by(crate::probe::Th::Hook, crate::probe::BY_KB_HOOK, WM_DOCK_PEEK));
    post_launcher(WM_DOCK_PEEK, usize::from(on), time);
}

/// Posts WM_DOCK_CHORD with the hook's next sequence (`ack_chord` acknowledges it). The first post the UI has not
/// acknowledged starts the fail-open window (`chord_fail_open`).
#[deny(clippy::unwrap_used, clippy::expect_used, clippy::indexing_slicing)]
fn post_chord(a: ChordAction, time: u32) {
    if CHORD_SEQ.load(Relaxed) == CHORD_ACKED.load(Acquire) {
        CHORD_OLDEST.store(time, Relaxed);
    }
    let seq = CHORD_SEQ.fetch_add(1, Relaxed).wrapping_add(1);
    probe!(crate::probe::post_by(crate::probe::Th::Hook, crate::probe::BY_KB_HOOK, WM_DOCK_CHORD));
    post_launcher(WM_DOCK_CHORD, encode_chord(a, seq), time);
}

fn post(msg: u32, ev: usize) {
    let h = hwnd();
    if h.0.is_null() {
        return; // without this check PostMessageW(NULL) would post to the hook thread
    }
    probe!(crate::probe::post_by(crate::probe::Th::Hook, crate::probe::BY_KB_HOOK, msg));
    // SAFETY: PostMessageW is thread-safe; the error is ignored (no I/O in the callback).
    let _ = unsafe { PostMessageW(Some(h), msg, WPARAM(ev), LPARAM(0)) };
}

fn note(ev: usize) {
    post(WM_DOCK_WINKEY, ev);
}

fn post_toggle() {
    if NO_TOGGLE.load(Relaxed) {
        return;
    }
    post(WM_DOCK_TOGGLE, 0);
}

fn post_start_menu() {
    if NO_TOGGLE.load(Relaxed) {
        return;
    }
    post(WM_DOCK_START_MENU, 0);
}

fn key_down_input(vk: u32, scan: u32, extended: bool) -> INPUT {
    INPUT {
        r#type: INPUT_KEYBOARD,
        Anonymous: INPUT_0 {
            ki: KEYBDINPUT {
                wVk: VIRTUAL_KEY(vk as u16),
                wScan: scan as u16,
                dwFlags: if extended { KEYEVENTF_EXTENDEDKEY } else { KEYBD_EVENT_FLAGS(0) },
                time: 0,
                dwExtraInfo: INJECT_TAG,
            },
        },
    }
}

/// A key up tagged INJECT_TAG (the launcher's menu-mask key).
#[deny(clippy::unwrap_used, clippy::expect_used, clippy::indexing_slicing)]
fn key_up_input(vk: u32) -> INPUT {
    INPUT {
        r#type: INPUT_KEYBOARD,
        Anonymous: INPUT_0 {
            ki: KEYBDINPUT {
                wVk: VIRTUAL_KEY(vk as u16),
                wScan: 0,
                dwFlags: KEYEVENTF_KEYUP,
                time: 0,
                dwExtraInfo: INJECT_TAG,
            },
        },
    }
}

/// Injects [WIN down, key down] with ONE call: the two events enter the queue
/// contiguously and in order, regardless of when the callback returns.
/// Returns true if both were accepted.
fn inject_win_and_key(win_vk: u32, kb: &KBDLLHOOKSTRUCT) -> bool {
    let inputs = [
        key_down_input(win_vk, 0, true), // LWIN/RWIN are "extended" keys
        key_down_input(kb.vkCode, kb.scanCode, kb.flags.0 & LLKHF_EXTENDED.0 != 0),
    ];
    // SAFETY: slice of 2 initialized INPUTs; cbSize = size_of::<INPUT>() as required.
    // The injected events re-enter our callback with INJECT_TAG and pass through.
    let sent = unsafe { SendInput(&inputs, std::mem::size_of::<INPUT>() as i32) };
    sent == 2
}

/// DIAGNOSTICS (only with `--diag`): for every Win event (including those let through) posts WM_DOCK_DIAG.
/// wparam: bit0=down, bit1=up, bit2=injected, bit3=lower_il_injected, bit4=swallowed,
/// bit5=our tag, bit6=extended, bit7=rwin, bit8..=scancode. lparam: dwExtraInfo.
fn diag(msg: u32, kb: &KBDLLHOOKSTRUCT, swallowed: bool) {
    if !DIAG.load(Relaxed) {
        return;
    }
    let vk = kb.vkCode;
    if vk != u32::from(VK_LWIN.0) && vk != u32::from(VK_RWIN.0) {
        return;
    }
    let down = msg == WM_KEYDOWN || msg == WM_SYSKEYDOWN;
    let up = msg == WM_KEYUP || msg == WM_SYSKEYUP;
    let mut w: usize = 0;
    if down {
        w |= 1
    }
    if up {
        w |= 2
    }
    if kb.flags.0 & LLKHF_INJECTED.0 != 0 {
        w |= 4
    }
    if kb.flags.0 & 0x02 != 0 {
        w |= 8
    } // LLKHF_LOWER_IL_INJECTED
    if swallowed {
        w |= 16
    }
    if kb.dwExtraInfo == INJECT_TAG {
        w |= 32
    }
    if kb.flags.0 & LLKHF_EXTENDED.0 != 0 {
        w |= 64
    }
    if vk == u32::from(VK_RWIN.0) {
        w |= 128
    }
    w |= (kb.scanCode as usize & 0xFFFF) << 8;
    if hwnd().0.is_null() {
        return;
    }
    // Latency: difference between now and the event timestamp (both GetTickCount, ms).
    // SAFETY: GetTickCount has no preconditions.
    let delta_ms = unsafe { GetTickCount() }.wrapping_sub(kb.time) & 0xFFFF;
    w |= (delta_ms as usize & 0xFFFF) << 24;
    // SAFETY: GetForegroundWindow has no preconditions; PostMessageW is thread-safe.
    let fg = unsafe { GetForegroundWindow() };
    let _ = unsafe { PostMessageW(Some(hwnd()), WM_DOCK_DIAG, WPARAM(w), LPARAM(fg.0 as isize)) };
}

// ------------------------------------------------------------------ the launcher chord (SPEC-launcher 5.5.4)
//
// Everything below runs only while the UI has set a chord (`set_launcher_chord`): without one `handle_key` loads one
// atomic and goes on exactly as before. Every function of this block denies unwrap, expect and indexing: a panic in
// the callback (`extern "system"`) aborts the dock.

pub(crate) use crate::launcher::model::{ChordSpec, Mods};

/// Thread message to the hook thread. wParam = `NOTE_*`: from the callback, a note to log outside it (lParam = a
/// sequence); from any thread, `NOTE_ACTIVITY` (`note_activity`). wParam values 1-15 belong to WS-C, 16-31 to the
/// launcher.
const WM_HOOK_NOTE: u32 = WM_APP + 19;
/// `WM_HOOK_NOTE`: the launcher chord failed open; lParam = the unacknowledged sequence.
const NOTE_LAUNCHER_FAIL_OPEN: usize = 16;
/// `WM_HOOK_NOTE` (values 32-47 belong to the dock chords): the Peek timer could not be armed, noted once per process.
const NOTE_PEEK_TIMER_FAIL: usize = 32;
/// `WM_HOOK_NOTE` from any thread (`cut_off`): keys went by unseen; lParam = the `CutOff`'s index.
const NOTE_CUT_OFF: usize = 33;
/// `WM_HOOK_NOTE` from the callback (`note_stall`): stall evidence; lParam = the `CutOff`'s index.
const NOTE_STALL: usize = 34;

/// A post may stay unacknowledged this long before the chord fails open (the name and value of WS-C 17.1).
pub const ACK_TIMEOUT_MS: u32 = 1500;
/// The UI opens nothing for a post older than this (`post_verdict`).
pub const STALE_POST_MS: u32 = 300;
/// The type-ahead window after the post of a chord that will open the launcher.
pub const TYPEAHEAD_MS: u32 = 500;
/// A tracked modifier with no event for this long is re-read from the async key state; a chord key held by the
/// launcher with no event for this long lost its release.
pub const MOD_STALE_MS: u32 = 1500;
/// Keys a type-ahead window keeps; later ones are swallowed and dropped (spec rule 6).
pub const RING_CAP: usize = 32;
/// The menu-mask key injected under Alt when the chord is swallowed, so the Alt release opens no menu bar: an
/// unassigned VK. S-ALTSPACE may choose left Ctrl (0xA2) instead (L1a-10 step 8).
const MASK_KEY_DEFAULT: u32 = 0xE8;

const VK_RETURN_: u32 = 0x0D;
const VK_SPACE_: u16 = 0x20;
const VK_MENU_: u16 = 0x12;
const CTRL_SHIFT: Mods = Mods(Mods::CTRL.0 | Mods::SHIFT.0);
/// `LAUNCHER_CHORD`: bits 0-7 the vk, 8-11 the modifiers, bit 12 set.
const CHORD_SET: u32 = 1 << 12;

/// Why the chord passes instead of opening the launcher (set by the UI on every foreground change).
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub struct PassFlags(pub u8);

#[deny(clippy::unwrap_used, clippy::expect_used, clippy::indexing_slicing)]
impl PassFlags {
    pub const FULLSCREEN: PassFlags = PassFlags(1);
    pub const REMOTE: PassFlags = PassFlags(2);
    pub const YIELD: PassFlags = PassFlags(4);

    pub const fn has(self, f: PassFlags) -> bool {
        self.0 & f.0 != 0
    }

    pub const fn with(self, f: PassFlags) -> PassFlags {
        PassFlags(self.0 | f.0)
    }
}

/// Hook-thread view of the posts to the UI thread (WS-C 17.1's struct, verbatim). Pure: the statics are loaded into
/// it and stored back. The launcher has its own instance and its own sequence space.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct AckWatch {
    sent: u32,
    sent_at: u32,
    ack_seen: u32,
}

#[deny(clippy::unwrap_used, clippy::expect_used, clippy::indexing_slicing)]
impl AckWatch {
    /// On every key event: true = a post has been unacknowledged for ACK_TIMEOUT_MS.
    fn check(&mut self, acked: u32, now: u32) -> bool {
        if acked != self.ack_seen {
            self.ack_seen = acked;
            if acked != self.sent {
                self.sent_at = now; // progress: the rest gets a fresh window
            }
        }
        self.sent != acked && now.wrapping_sub(self.sent_at) >= ACK_TIMEOUT_MS
    }

    /// A post is about to be sent: its sequence number.
    fn on_post(&mut self, acked: u32, now: u32) -> u32 {
        if self.sent == acked {
            self.sent_at = now; // first outstanding post starts the window
        }
        self.sent = self.sent.wrapping_add(1);
        self.sent
    }
}

/// Alt, Ctrl and Shift as the hook saw them, per side (the bits of `SIDES`); `last` is the time of the last modifier
/// event, `reset_seen` the last `RESET_MODS` value applied.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct ModTrack {
    down: u8,
    last: u32,
    reset_seen: u32,
}

/// (bit in `ModTrack::down`, vk, modifier): left and right Alt, Ctrl and Shift.
const SIDES: [(u8, u32, Mods); 6] = [
    (1, 0xA4, Mods::ALT),
    (2, 0xA5, Mods::ALT),
    (4, 0xA2, Mods::CTRL),
    (8, 0xA3, Mods::CTRL),
    (16, 0xA0, Mods::SHIFT),
    (32, 0xA1, Mods::SHIFT),
];
const ALT_BITS: u8 = 1 | 2;

#[deny(clippy::unwrap_used, clippy::expect_used, clippy::indexing_slicing)]
impl ModTrack {
    fn on_key(&mut self, vk: u32, down: bool, time: u32) {
        let Some(&(bit, _, _)) = SIDES.iter().find(|s| s.1 == vk) else { return };
        if down {
            self.down |= bit;
        } else {
            self.down &= !bit;
        }
        self.last = time;
    }

    fn mods(&self, win: bool) -> Mods {
        let held = SIDES.iter().filter(|s| self.down & s.0 != 0).fold(Mods::NONE, |m, s| m.with(s.2));
        if win {
            held.with(Mods::WIN)
        } else {
            held
        }
    }

    fn alt_down(&self) -> bool {
        self.down & ALT_BITS != 0
    }

    /// A tracked modifier with no event for MOD_STALE_MS is kept only if the async key state still reads it down (a
    /// release lost on the secure desktop); the next check is MOD_STALE_MS later.
    fn repair(&mut self, now: u32, async_down: impl Fn(u32) -> bool) {
        if self.down == 0 || now.wrapping_sub(self.last) <= MOD_STALE_MS {
            return;
        }
        for &(bit, vk, _) in &SIDES {
            if self.down & bit != 0 && !async_down(vk) {
                self.down &= !bit;
            }
        }
        self.last = now;
    }

    /// `reset_modifiers` was called since the last event (lock, suspend, an admin launch): forget every modifier.
    fn reset_if(&mut self, reset: u32) {
        if reset != self.reset_seen {
            self.reset_seen = reset;
            self.down = 0;
        }
    }
}

/// One event of the modifier tracking, in the order of spec rule 1: the reset first, then the event, then the repair.
#[deny(clippy::unwrap_used, clippy::expect_used, clippy::indexing_slicing)]
fn track_step(t: &mut ModTrack, reset: u32, vk: u32, down: bool, time: u32, async_down: impl Fn(u32) -> bool) {
    t.reset_if(reset);
    t.on_key(vk, down, time);
    t.repair(time, async_down);
}

/// The chord key's press as the hook handles it.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
enum SpaceState {
    #[default]
    Idle,
    /// Swallowed (posted): its repeats and its release are swallowed too.
    Swallowed,
    /// Passed to Windows: its repeats and its release pass too.
    Passed,
}

#[deny(clippy::unwrap_used, clippy::expect_used, clippy::indexing_slicing)]
impl SpaceState {
    fn from_u8(v: u8) -> SpaceState {
        match v {
            1 => SpaceState::Swallowed,
            2 => SpaceState::Passed,
            _ => SpaceState::Idle,
        }
    }

    fn to_u8(self) -> u8 {
        match self {
            SpaceState::Idle => 0,
            SpaceState::Swallowed => 1,
            SpaceState::Passed => 2,
        }
    }
}

/// One physical key event as the chord rules see it.
#[derive(Clone, Copy, Debug)]
pub(crate) struct KeyEv {
    pub vk: u32,
    pub scan: u32,
    pub ext: bool,
    pub down: bool,
    pub time: u32,
    /// A down of a key Windows already holds (its async state before this event reads down): a repeat of a press
    /// whose down reached Windows. Such a press stays Windows' whole, its repeats and its release included.
    pub held: bool,
}

/// The ring's control word: bits 32-63 the sequence of the post that opened the window, bit 9 OPEN, bit 8 ENDED,
/// bits 0-5 the length.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub(crate) struct RingCtl(pub u64);

#[deny(clippy::unwrap_used, clippy::expect_used, clippy::indexing_slicing)]
impl RingCtl {
    const OPEN: u64 = 1 << 9;
    const ENDED: u64 = 1 << 8;
    const LEN: u64 = 0x3F;

    /// A new window for this sequence, empty.
    fn opened(seq: u32) -> RingCtl {
        RingCtl((u64::from(seq) << 32) | Self::OPEN)
    }

    fn seq(self) -> u32 {
        (self.0 >> 32) as u32
    }

    fn is_open(self) -> bool {
        self.0 & Self::OPEN != 0
    }

    fn is_ended(self) -> bool {
        self.0 & Self::ENDED != 0
    }

    fn len(self) -> usize {
        (self.0 & Self::LEN) as usize
    }

    fn ended(self) -> RingCtl {
        RingCtl(self.0 | Self::ENDED)
    }

    /// One more entry; the caller has checked that the length is below RING_CAP.
    fn pushed(self) -> RingCtl {
        RingCtl(self.0.wrapping_add(1))
    }
}

/// A key the type-ahead ring kept: a down, with the modifiers tracked when it was pressed.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub struct KeyRec {
    pub vk: u16,
    pub scan: u16,
    pub ext: bool,
    pub mods: Mods,
    pub time: u32,
}

#[deny(clippy::unwrap_used, clippy::expect_used, clippy::indexing_slicing)]
impl KeyRec {
    /// time:32 | scan:12 | vk:8 | mods:4 | ext:1.
    fn pack(self) -> u64 {
        u64::from(self.time)
            | (u64::from(self.scan & 0xFFF) << 32)
            | (u64::from(self.vk & 0xFF) << 44)
            | (u64::from(self.mods.0 & 0xF) << 52)
            | (u64::from(self.ext) << 56)
    }

    fn unpack(v: u64) -> KeyRec {
        KeyRec {
            time: v as u32,
            scan: ((v >> 32) & 0xFFF) as u16,
            vk: ((v >> 44) & 0xFF) as u16,
            mods: Mods(((v >> 52) & 0xF) as u8),
            ext: (v >> 56) & 1 != 0,
        }
    }
}

/// What `Ring::try_push` did with a key.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum Push {
    /// In the ring: the UI replays it, or drops it with the window.
    Stored,
    /// The window holds RING_CAP keys already: swallowed and dropped.
    Full,
    /// No open window for this sequence (the UI ended it): the key passes to Windows.
    Closed,
}

/// The type-ahead ring. Both threads change `ctl` only by compare-exchange, except the hook's `open` (only the hook
/// issues sequence numbers, so a UI compare-exchange for an older sequence always fails): a buffered key is either in
/// the copy the UI takes or reported `Closed` to the hook, which passes it; never both, never neither.
pub(crate) struct Ring {
    ctl: AtomicU64,
    slots: [AtomicU64; RING_CAP],
}

#[deny(clippy::unwrap_used, clippy::expect_used, clippy::indexing_slicing)]
impl Ring {
    pub(crate) const fn new() -> Ring {
        Ring { ctl: AtomicU64::new(0), slots: [const { AtomicU64::new(0) }; RING_CAP] }
    }

    fn ctl(&self) -> RingCtl {
        RingCtl(self.ctl.load(Acquire))
    }

    /// Hook: a new window for this sequence, length 0.
    fn open(&self, seq: u32) {
        self.ctl.store(RingCtl::opened(seq).0, Release);
    }

    /// Hook: keeps one key in the open window of `seq`.
    fn try_push(&self, seq: u32, entry: u64) -> Push {
        let c = self.ctl();
        if !c.is_open() || c.is_ended() || c.seq() != seq {
            return Push::Closed;
        }
        let Some(slot) = self.slots.get(c.len()) else { return Push::Full };
        slot.store(entry, Release);
        match self.ctl.compare_exchange(c.0, c.pushed().0, Release, Relaxed) {
            Ok(_) => Push::Stored,
            Err(_) => Push::Closed, // the UI ended the window in between
        }
    }

    /// Hook: the window of `seq` is past its deadline (one compare-exchange; the UI may have ended it already).
    fn expire(&self, seq: u32) {
        let c = self.ctl();
        if c.is_open() && !c.is_ended() && c.seq() == seq {
            let _ = self.ctl.compare_exchange(c.0, c.ended().0, AcqRel, Relaxed);
        }
    }

    /// UI: ends the open window of `seq`; its length, or None when there is no such window (or it already ended).
    pub(crate) fn end(&self, seq: u32) -> Option<usize> {
        let mut cur = self.ctl.load(Acquire);
        loop {
            let c = RingCtl(cur);
            if !c.is_open() || c.is_ended() || c.seq() != seq {
                return None;
            }
            match self.ctl.compare_exchange_weak(cur, c.ended().0, AcqRel, Acquire) {
                Ok(_) => return Some(c.len()),
                Err(now) => cur = now,
            }
        }
    }

    /// UI: ends the window of `seq` and copies its keys, once. 0 when another window opened during the copy.
    pub(crate) fn take(&self, seq: u32, out: &mut [KeyRec; RING_CAP]) -> usize {
        let Some(len) = self.end(seq) else { return 0 };
        let mut n = 0;
        for (slot, rec) in self.slots.iter().zip(out.iter_mut()).take(len) {
            *rec = KeyRec::unpack(slot.load(Acquire));
            n += 1;
        }
        if self.ctl().seq() != seq {
            return 0;
        }
        n
    }
}

/// The chord rules' inputs and the hook's own state, loaded from the statics on each event.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub(crate) struct ChordState {
    // UI-owned: loaded on each event, never stored back by the hook.
    chord: Option<ChordSpec>,
    flags: PassFlags,
    acked: u32,
    shown: bool,
    // Hook-owned: loaded and stored back by the hook thread only (`mods` and `win_down` are computed per event).
    mods: Mods,
    win_down: bool,
    ack: AckWatch,
    space: SpaceState,
    /// The time of the chord key's last event while it was swallowed or passed.
    space_time: u32,
    post_time: u32,
    /// A snapshot of the ring's control word.
    ring: RingCtl,
    /// Vks whose down was swallowed into the ring (their up is swallowed too).
    buffered_down: [u64; 4],
    last_failopen: u32,
}

/// What the hook does with one key event of the launcher's.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Verdict {
    Pass,
    /// Pass, and post WM_DOCK_LAUNCHER_YIELDED (the UI runs the PowerToys answer check).
    PassNotify,
    /// Pass under FULLSCREEN, and post WM_DOCK_LAUNCHER_PASSED: the UI checks the flag again (a window can leave
    /// fullscreen without a foreground change), so a stale flag loses one press.
    PassFullscreen,
    /// Swallow and post WM_DOCK_LAUNCHER with this sequence.
    SwallowPost(u32),
    SwallowSilent,
    /// Swallow into the type-ahead ring (a pass when the ring is closed).
    Buffer,
    /// Pass: the UI has not acknowledged a post for ACK_TIMEOUT_MS.
    FailOpen,
}

/// The injected filter of `handle_key` as a function: false for LLKHF_INJECTED (from any integrity level) or our tag.
#[deny(clippy::unwrap_used, clippy::expect_used, clippy::indexing_slicing)]
pub(crate) fn physical(flags: u32, extra: usize) -> bool {
    extra != INJECT_TAG && flags & LLKHF_INJECTED.0 == 0
}

/// The filter `handle_key` and `note_gesture` apply: `physical`.
#[cfg(not(feature = "owner-probe"))]
#[deny(clippy::unwrap_used, clippy::expect_used, clippy::indexing_slicing)]
fn accepted(flags: u32, extra: usize) -> bool {
    physical(flags, extra)
}

/// The filter `handle_key` and `note_gesture` apply in an owner-probe build: `admitted` under the accept-injected
/// switch, which `start` read once into `ACCEPT_INJECTED` (the callback only loads it). With the switch off: `physical`.
#[cfg(feature = "owner-probe")]
#[deny(clippy::unwrap_used, clippy::expect_used, clippy::indexing_slicing)]
fn accepted(flags: u32, extra: usize) -> bool {
    admitted(flags, extra, ACCEPT_INJECTED.load(Relaxed))
}

/// The owner-probe decision as a pure function. `accept_injected` off: `physical`. On (a test agent drives the L0
/// session with SendInput): every event without our INJECT_TAG, injected by another app or not. Our tagged events still
/// pass through untouched, or the hook would act on its own re-injections and loop.
#[cfg(any(test, feature = "owner-probe"))]
#[deny(clippy::unwrap_used, clippy::expect_used, clippy::indexing_slicing)]
fn admitted(flags: u32, extra: usize, accept_injected: bool) -> bool {
    if accept_injected {
        extra != INJECT_TAG
    } else {
        physical(flags, extra)
    }
}

/// The keys a type-ahead window never keeps, so the system's key state stays true: the modifiers (Shift, Ctrl, Alt:
/// 0x10-0x12, 0xA0-0xA5), Win (0x5B, 0x5C) and the system's keys, which the launcher does not own and a replay would
/// drop (they type nothing): CapsLock, the IME keys (Esc, 0x1B, excepted), Print Screen, Sleep, NumLock, ScrollLock
/// and the browser, volume, media and launch keys (0xA6-0xB7).
#[deny(clippy::unwrap_used, clippy::expect_used, clippy::indexing_slicing)]
const fn never_kept(vk: u32) -> bool {
    matches!(vk, 0x10..=0x12 | 0x14..=0x1A | 0x1C..=0x1F | 0x2C | 0x5B | 0x5C | 0x5F | 0x90 | 0x91 | 0xA0..=0xB7)
}

/// The system's chords on keys a type-ahead window keeps, which it passes too: Alt+Tab (with Shift or Ctrl as well),
/// Alt+Esc, Ctrl+Esc and Ctrl+Shift+Esc (the task switcher, the window cycle, Start, Task Manager). The launcher acts
/// on a plain Tab or Esc only, so a replay would drop them.
#[deny(clippy::unwrap_used, clippy::expect_used, clippy::indexing_slicing)]
const fn system_chord(mods: Mods, vk: u32) -> bool {
    match vk {
        0x09 => mods.has(Mods::ALT),
        0x1B => mods.has(Mods::ALT) || mods.has(Mods::CTRL),
        _ => false,
    }
}

#[deny(clippy::unwrap_used, clippy::expect_used, clippy::indexing_slicing)]
impl ChordState {
    /// The vk's down was swallowed into the ring.
    fn buffered(&self, vk: u32) -> bool {
        let i = (vk / 64) as usize;
        self.buffered_down.get(i).is_some_and(|w| w & (1u64 << (vk % 64)) != 0)
    }

    fn set_buffered(&mut self, vk: u32, on: bool) {
        let i = (vk / 64) as usize;
        if let Some(w) = self.buffered_down.get_mut(i) {
            if on {
                *w |= 1u64 << (vk % 64);
            } else {
                *w &= !(1u64 << (vk % 64));
            }
        }
    }

    /// The type-ahead window is open, not ended and within TYPEAHEAD_MS of its post.
    fn live(&self, time: u32) -> bool {
        self.ring.is_open() && !self.ring.is_ended() && time.wrapping_sub(self.post_time) <= TYPEAHEAD_MS
    }

    /// The chord key's state for an event at `time`: a swallowed or passed press with no event for MOD_STALE_MS lost
    /// its release (the secure desktop), so it is over (auto-repeat renews it far more often than that).
    fn space_now(&self, time: u32) -> SpaceState {
        if self.space != SpaceState::Idle && time.wrapping_sub(self.space_time) > MOD_STALE_MS {
            SpaceState::Idle
        } else {
            self.space
        }
    }

    /// The hook-owned fields after the verdict `v` (`resolve_launcher`); `pushed` is the ring's answer to a Buffer.
    pub(crate) fn after(&mut self, v: Option<&Verdict>, ev: KeyEv, pushed: Option<Push>) {
        let Some(chord) = self.chord else { return };
        let chord_key = ev.vk == u32::from(chord.vk);
        if chord_key {
            self.space = self.space_now(ev.time);
        }
        if self.ring.is_open() && !self.ring.is_ended() && !self.live(ev.time) {
            self.ring = self.ring.ended(); // past its deadline: the caller expires the ring
        }
        match v {
            Some(Verdict::SwallowPost(seq)) => {
                let _ = self.ack.check(self.acked, ev.time);
                let _ = self.ack.on_post(self.acked, ev.time);
                self.space = SpaceState::Swallowed;
                if !self.shown {
                    // This chord opens the launcher: its type-ahead window. A closing chord opens none.
                    self.post_time = ev.time;
                    self.ring = RingCtl::opened(*seq);
                }
            }
            Some(Verdict::FailOpen) => {
                let _ = self.ack.check(self.acked, ev.time);
                self.last_failopen = self.ack.sent;
                self.space = SpaceState::Passed;
            }
            Some(Verdict::Pass | Verdict::PassNotify | Verdict::PassFullscreen) if ev.down => {
                self.space = SpaceState::Passed
            }
            // A key the ring refused (Closed) passed to Windows: no bit, or its up would be swallowed.
            Some(Verdict::Buffer) => self.set_buffered(ev.vk, matches!(pushed, Some(Push::Stored | Push::Full))),
            Some(Verdict::SwallowSilent) if !ev.down => self.set_buffered(ev.vk, false),
            None if ev.down => self.set_buffered(ev.vk, false),
            _ => {}
        }
        if chord_key {
            if !ev.down {
                self.space = SpaceState::Idle; // any release ends the press
            } else if self.space != SpaceState::Idle {
                self.space_time = ev.time;
            }
        }
    }
}

/// The chord rules (SPEC 5.5.4 rules 1-6), in order. None: not the launcher's key (the Win branch decides).
#[deny(clippy::unwrap_used, clippy::expect_used, clippy::indexing_slicing)]
pub(crate) fn resolve_launcher(s: &ChordState, ev: KeyEv) -> Option<Verdict> {
    // 1. No chord: nothing changes for users without one.
    let chord = s.chord?;
    // 2. The chord key.
    if ev.vk == u32::from(chord.vk) {
        match (s.space_now(ev.time), ev.down) {
            (SpaceState::Swallowed, _) => return Some(Verdict::SwallowSilent),
            (SpaceState::Passed, _) => return Some(Verdict::Pass),
            (SpaceState::Idle, true) if s.mods == chord.mods && !s.win_down && !ev.held => {
                // FULLSCREEN can be stale (a window left fullscreen without a foreground change): the note asks the UI
                // to check it again.
                if s.flags.has(PassFlags::FULLSCREEN) {
                    return Some(Verdict::PassFullscreen);
                }
                if s.flags.has(PassFlags::REMOTE) {
                    return Some(Verdict::Pass);
                }
                if s.flags.has(PassFlags::YIELD) {
                    return Some(Verdict::PassNotify);
                }
                let mut ack = s.ack;
                if ack.check(s.acked, ev.time) {
                    return Some(Verdict::FailOpen);
                }
                return Some(Verdict::SwallowPost(s.ack.sent.wrapping_add(1)));
            }
            (SpaceState::Idle, _) => {}
        }
    }
    // 3. Type-ahead: inside a live window every key but modifiers, Win, the system's keys and chords, Win chords and the
    //    repeats of a key Windows already holds goes to the ring; an up is swallowed only when its down was.
    if s.live(ev.time) {
        if !ev.down {
            return s.buffered(ev.vk).then_some(Verdict::SwallowSilent);
        }
        if never_kept(ev.vk) || system_chord(s.mods, ev.vk) || s.win_down || ev.held {
            return None;
        }
        return Some(Verdict::Buffer);
    }
    // 4. Outside a window: the up of a swallowed down is swallowed; a down with its bit set is a stale bit (its up was
    //    lost), which `after` clears.
    if s.buffered(ev.vk) && !ev.down {
        return Some(Verdict::SwallowSilent);
    }
    // 5. Everything else.
    None
}

/// What the callback does besides swallowing or passing.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Act {
    Nothing,
    /// Mask the Alt release, then post WM_DOCK_LAUNCHER with this sequence.
    Post(u32),
    /// Post WM_DOCK_LAUNCHER_YIELDED.
    Yielded,
    /// Post WM_DOCK_LAUNCHER_PASSED with the newest sequence issued (the UI acknowledges it, `resync_launcher`).
    Passed(u32),
    /// Failed open: post WM_DOCK_LAUNCHER_PASSED with the newest sequence issued, so a live UI acknowledges it and the
    /// next chord is posted again (a lost post is never acknowledged otherwise); `note`: this sequence failed open for
    /// the first time, noted to the hook thread (logged outside the callback).
    FailOpen {
        seq: u32,
        note: bool,
    },
}

/// Swallowed (Some(true)), passed by the launcher (Some(false)) or not the launcher's (None).
#[deny(clippy::unwrap_used, clippy::expect_used, clippy::indexing_slicing)]
fn swallow_of(v: Option<Verdict>, pushed: Option<Push>) -> Option<bool> {
    match v? {
        Verdict::Pass | Verdict::PassNotify | Verdict::PassFullscreen | Verdict::FailOpen => Some(false),
        Verdict::SwallowPost(_) | Verdict::SwallowSilent => Some(true),
        Verdict::Buffer => Some(!matches!(pushed, Some(Push::Closed))),
    }
}

/// One event through the chord rules on `ring`: the verdict, the ring's push, open or expiry, and the state after.
#[deny(clippy::unwrap_used, clippy::expect_used, clippy::indexing_slicing)]
fn chord_step(s: &mut ChordState, ring: &Ring, ev: KeyEv) -> (Option<bool>, Act) {
    s.ring = ring.ctl();
    let before = s.ring;
    let failopen_before = s.last_failopen;
    let v = resolve_launcher(s, ev);
    let pushed = (v == Some(Verdict::Buffer)).then(|| {
        let rec = KeyRec {
            vk: (ev.vk & 0xFF) as u16,
            scan: (ev.scan & 0xFFF) as u16,
            ext: ev.ext,
            mods: s.mods,
            time: ev.time,
        };
        ring.try_push(before.seq(), rec.pack())
    });
    s.after(v.as_ref(), ev, pushed);
    if s.ring != before {
        if s.ring.is_ended() {
            ring.expire(before.seq());
        } else {
            ring.open(s.ring.seq());
        }
    }
    let act = match v {
        Some(Verdict::SwallowPost(seq)) => Act::Post(seq),
        Some(Verdict::PassNotify) => Act::Yielded,
        Some(Verdict::PassFullscreen) => Act::Passed(s.ack.sent),
        Some(Verdict::FailOpen) => Act::FailOpen { seq: s.ack.sent, note: s.last_failopen != failopen_before },
        _ => Act::Nothing,
    };
    (swallow_of(v, pushed), act)
}

/// What the UI does with a WM_DOCK_LAUNCHER post (spec rule 8).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum PostAction {
    Open,
    Close,
    Drop,
}

/// Rule 8: a stale (over STALE_POST_MS), superseded (below the newest sequence) or failed-open post opens nothing; any
/// post closes an open launcher (closing is always safe). Sequences and times compare with wrapping.
#[deny(clippy::unwrap_used, clippy::expect_used, clippy::indexing_slicing)]
pub(crate) fn post_verdict(
    seq: u32,
    sent_time: u32,
    now: u32,
    newest: u32,
    last_failopen: u32,
    open: bool,
) -> PostAction {
    let stale = now.wrapping_sub(sent_time) > STALE_POST_MS;
    let superseded = (newest.wrapping_sub(seq) as i32) > 0;
    let failed_open = (last_failopen.wrapping_sub(seq) as i32) >= 0;
    match (open, stale || superseded || failed_open) {
        (true, _) => PostAction::Close,
        (false, true) => PostAction::Drop,
        (false, false) => PostAction::Open,
    }
}

/// The keys the UI injects when the launcher cannot open, so the window menu opens as Windows would have: (vk, up),
/// all tagged INJECT_TAG.
pub struct InjectPlan {
    pub keys: [(u16, bool); 4],
    pub len: u8,
}

#[deny(clippy::unwrap_used, clippy::expect_used, clippy::indexing_slicing)]
impl InjectPlan {
    pub fn keys(&self) -> &[(u16, bool)] {
        self.keys.get(..usize::from(self.len)).unwrap_or(&[])
    }
}

/// With Alt still down: Space down, Space up. With Alt already up: Alt down, Space down, Space up, Alt up. The real
/// Space up stays swallowed (the plan has its own), so no key is left held.
#[deny(clippy::unwrap_used, clippy::expect_used, clippy::indexing_slicing)]
pub fn reinject_plan(alt_tracked_down: bool) -> InjectPlan {
    let sp = VK_SPACE_;
    if alt_tracked_down {
        InjectPlan { keys: [(sp, false), (sp, true), (0, false), (0, false)], len: 2 }
    } else {
        InjectPlan { keys: [(VK_MENU_, false), (sp, false), (sp, true), (VK_MENU_, true)], len: 4 }
    }
}

/// The hook's gesture evidence: the whole kb.time and a separate valid bit (a "| 1" on the time would change every
/// even time and fail the launcher's time equality on half of all presses).
#[deny(clippy::unwrap_used, clippy::expect_used, clippy::indexing_slicing)]
pub(crate) fn pack_gesture_time(t: u32) -> u64 {
    (1 << 32) | u64::from(t)
}

#[deny(clippy::unwrap_used, clippy::expect_used, clippy::indexing_slicing)]
pub(crate) fn unpack_gesture_time(v: u64) -> Option<u32> {
    if v >> 32 == 1 {
        Some(v as u32)
    } else {
        None
    }
}

/// Spec rule 7: a physical Enter down with tracked Ctrl and Shift and no Alt or Win stores its kb.time (never swallows).
#[deny(clippy::unwrap_used, clippy::expect_used, clippy::indexing_slicing)]
fn note_gesture(slot: &AtomicU64, flags: u32, extra: usize, vk: u32, down: bool, mods: Mods, time: u32) {
    if down && vk == VK_RETURN_ && mods == CTRL_SHIFT && accepted(flags, extra) {
        slot.store(pack_gesture_time(time), Relaxed);
    }
}

// UI-written (Relaxed: one writer each; `LAUNCHER_CHORD` is stored with Release after `RESET_MODS`, `swap_chord`).
static LAUNCHER_CHORD: AtomicU32 = AtomicU32::new(0);
static PASS_FLAGS: AtomicU8 = AtomicU8::new(0);
static LAUNCHER_ACKED: AtomicU32 = AtomicU32::new(0);
static LAUNCHER_SHOWN: AtomicBool = AtomicBool::new(false);
static RESET_MODS: AtomicU32 = AtomicU32::new(0);
// Hook-written (Relaxed: one writer each).
/// The newest sequence the hook issued (`AckWatch::sent`).
pub static LAUNCHER_SEQ: AtomicU32 = AtomicU32::new(0);
static LAUNCHER_ACK_SENT_AT: AtomicU32 = AtomicU32::new(0);
static LAUNCHER_ACK_SEEN: AtomicU32 = AtomicU32::new(0);
/// The newest sequence the hook failed open on.
pub static LAST_FAILOPEN_SEQ: AtomicU32 = AtomicU32::new(0);
static SPACE_STATE: AtomicU8 = AtomicU8::new(0);
static SPACE_TIME: AtomicU32 = AtomicU32::new(0);
static TA_POST_TIME: AtomicU32 = AtomicU32::new(0);
static BUFFERED: [AtomicU64; 4] = [const { AtomicU64::new(0) }; 4];
/// `ModTrack::down` (bits 0-7) and `reset_seen` (bits 32-63).
static MODS: AtomicU64 = AtomicU64::new(0);
static MODS_LAST: AtomicU32 = AtomicU32::new(0);
/// The menu-mask key: 0xE8 by default; owner-probe builds read their mask switch at start (0xA2 left Ctrl, 0 none).
static MASK_KEY: AtomicU32 = AtomicU32::new(MASK_KEY_DEFAULT);
/// Owner-probe builds: the accept-injected switch, stored by `start` before the hook is installed (`accepted`).
#[cfg(feature = "owner-probe")]
static ACCEPT_INJECTED: AtomicBool = AtomicBool::new(false);
// Shared by protocol.
/// The type-ahead ring (its compare-exchange protocol: `Ring`).
static RING: Ring = Ring::new();
/// `pack_gesture_time(kb.time)`, stored by the hook, consumed by the UI with one swap.
static GESTURE_TIME: AtomicU64 = AtomicU64::new(0);

#[deny(clippy::unwrap_used, clippy::expect_used, clippy::indexing_slicing)]
fn pack_chord(chord: Option<ChordSpec>) -> u32 {
    chord.map_or(0, |c| u32::from(c.vk & 0xFF) | (u32::from(c.mods.0 & 0xF) << 8) | CHORD_SET)
}

#[deny(clippy::unwrap_used, clippy::expect_used, clippy::indexing_slicing)]
fn unpack_chord(v: u32) -> Option<ChordSpec> {
    (v & CHORD_SET != 0).then_some(ChordSpec { vk: (v & 0xFF) as u16, mods: Mods(((v >> 8) & 0xF) as u8) })
}

/// UI: the launcher's chord (None: off, and the hook leaves every key to the Win branch). A change of chord also
/// forgets the tracked modifiers: they are not tracked while the chord is off, so a release missed then must not make
/// a bare Space match when it comes back (spec rule 1). Setting the same chord again forgets nothing.
#[deny(clippy::unwrap_used, clippy::expect_used, clippy::indexing_slicing)]
pub fn set_launcher_chord(chord: Option<ChordSpec>) {
    swap_chord(&LAUNCHER_CHORD, &RESET_MODS, chord);
}

/// `set_launcher_chord` on its two statics. The UI thread is their only writer, so the load and the store cannot race;
/// the reset is published before the chord (Release, paired with `handle_key`'s Acquire load), so an event that sees
/// the new chord also sees the reset.
#[deny(clippy::unwrap_used, clippy::expect_used, clippy::indexing_slicing)]
fn swap_chord(slot: &AtomicU32, reset: &AtomicU32, chord: Option<ChordSpec>) {
    let new = pack_chord(chord);
    if slot.load(Relaxed) != new {
        reset.fetch_add(1, Relaxed);
        slot.store(new, Release);
    }
}

/// UI: why the chord passes (fullscreen, remote, yield), on every foreground change.
#[deny(clippy::unwrap_used, clippy::expect_used, clippy::indexing_slicing)]
pub fn set_pass_flags(flags: PassFlags) {
    PASS_FLAGS.store(flags.0, Relaxed);
}

/// UI, first thing on WM_DOCK_LAUNCHER: the post of `seq` arrived.
#[deny(clippy::unwrap_used, clippy::expect_used, clippy::indexing_slicing)]
pub fn ack_launcher(seq: u32) {
    #[cfg(feature = "owner-probe")]
    crate::launcher_probe::freeze_ui_once();
    LAUNCHER_ACKED.store(seq, Relaxed);
}

/// UI, first thing on WM_DOCK_LAUNCHER_PASSED (wParam the newest sequence the hook issued): every post up to `seq` was
/// handled (posted messages keep their order), a lost one included, so the hook stops failing open. The owner probe's
/// freeze stays on the chord's own acknowledgement (`ack_launcher`).
#[deny(clippy::unwrap_used, clippy::expect_used, clippy::indexing_slicing)]
pub fn resync_launcher(seq: u32) {
    LAUNCHER_ACKED.store(seq, Relaxed);
}

/// UI: true right before the launcher's ShowWindow, false at its close (a chord while shown closes the launcher and
/// opens no type-ahead window).
#[deny(clippy::unwrap_used, clippy::expect_used, clippy::indexing_slicing)]
pub fn set_launcher_shown(on: bool) {
    LAUNCHER_SHOWN.store(on, Relaxed);
}

/// UI: the post of `seq` opens nothing (dropped, failed open, fullscreen, a closing chord): its keys are discarded. A
/// no-op for another sequence.
#[deny(clippy::unwrap_used, clippy::expect_used, clippy::indexing_slicing)]
pub fn end_typeahead(seq: u32) {
    let _ = RING.end(seq);
}

/// UI, once, at the launcher's activation: the keys typed after the chord of `seq`, in order.
#[deny(clippy::unwrap_used, clippy::expect_used, clippy::indexing_slicing)]
pub fn take_typeahead(seq: u32, out: &mut [KeyRec; RING_CAP]) -> usize {
    RING.take(seq, out)
}

/// UI: forget the tracked modifiers (after lock, suspend and every admin launch).
#[deny(clippy::unwrap_used, clippy::expect_used, clippy::indexing_slicing)]
pub fn reset_modifiers() {
    RESET_MODS.fetch_add(1, Relaxed);
}

/// UI: the kb.time of the last physical Ctrl+Shift+Enter, consumed (one swap: the whole value or nothing).
#[deny(clippy::unwrap_used, clippy::expect_used, clippy::indexing_slicing)]
pub fn take_gesture_time() -> Option<u32> {
    unpack_gesture_time(GESTURE_TIME.swap(0, Relaxed))
}

/// UI: Alt is down as the hook tracks it (for `reinject_plan`).
#[deny(clippy::unwrap_used, clippy::expect_used, clippy::indexing_slicing)]
pub fn alt_tracked_down() -> bool {
    load_mods().alt_down()
}

#[deny(clippy::unwrap_used, clippy::expect_used, clippy::indexing_slicing)]
fn load_mods() -> ModTrack {
    let v = MODS.load(Relaxed);
    ModTrack { down: (v & 0xFF) as u8, last: MODS_LAST.load(Relaxed), reset_seen: (v >> 32) as u32 }
}

/// Spec rule 1 on the statics: the tracked modifiers after this event, Win from the Win branch's state.
#[deny(clippy::unwrap_used, clippy::expect_used, clippy::indexing_slicing)]
fn mods_step(vk: u32, down: bool, time: u32) -> Mods {
    let mut t = load_mods();
    // SAFETY: GetAsyncKeyState has no preconditions; the high bit (negative) means "down".
    let async_down = |vk: u32| unsafe { GetAsyncKeyState(vk as i32) } < 0;
    track_step(&mut t, RESET_MODS.load(Relaxed), vk, down, time, async_down);
    MODS.store(u64::from(t.down) | (u64::from(t.reset_seen) << 32), Relaxed);
    MODS_LAST.store(t.last, Relaxed);
    t.mods(WIN_DOWN.load(Relaxed))
}

#[deny(clippy::unwrap_used, clippy::expect_used, clippy::indexing_slicing)]
fn load_chord_state(mods: Mods) -> ChordState {
    let mut buffered_down = [0u64; 4];
    for (d, b) in buffered_down.iter_mut().zip(BUFFERED.iter()) {
        *d = b.load(Relaxed);
    }
    ChordState {
        chord: unpack_chord(LAUNCHER_CHORD.load(Relaxed)),
        flags: PassFlags(PASS_FLAGS.load(Relaxed)),
        acked: LAUNCHER_ACKED.load(Relaxed),
        shown: LAUNCHER_SHOWN.load(Relaxed),
        mods,
        win_down: WIN_DOWN.load(Relaxed),
        ack: AckWatch {
            sent: LAUNCHER_SEQ.load(Relaxed),
            sent_at: LAUNCHER_ACK_SENT_AT.load(Relaxed),
            ack_seen: LAUNCHER_ACK_SEEN.load(Relaxed),
        },
        space: SpaceState::from_u8(SPACE_STATE.load(Relaxed)),
        space_time: SPACE_TIME.load(Relaxed),
        post_time: TA_POST_TIME.load(Relaxed),
        ring: RingCtl::default(),
        buffered_down,
        last_failopen: LAST_FAILOPEN_SEQ.load(Relaxed),
    }
}

/// Stores the hook-owned fields back (the ring changes through its own protocol).
#[deny(clippy::unwrap_used, clippy::expect_used, clippy::indexing_slicing)]
fn store_chord_state(s: &ChordState) {
    LAUNCHER_SEQ.store(s.ack.sent, Relaxed);
    LAUNCHER_ACK_SENT_AT.store(s.ack.sent_at, Relaxed);
    LAUNCHER_ACK_SEEN.store(s.ack.ack_seen, Relaxed);
    LAST_FAILOPEN_SEQ.store(s.last_failopen, Relaxed);
    SPACE_STATE.store(s.space.to_u8(), Relaxed);
    SPACE_TIME.store(s.space_time, Relaxed);
    TA_POST_TIME.store(s.post_time, Relaxed);
    for (b, d) in BUFFERED.iter().zip(s.buffered_down.iter()) {
        b.store(*d, Relaxed);
    }
}

/// The launcher's part of the callback: Some(swallow) when the event was the launcher's, None for the Win branch.
/// O(1), no allocation, no logging: at most one SendInput and one PostMessage (and a fail-open's note to this thread).
#[deny(clippy::unwrap_used, clippy::expect_used, clippy::indexing_slicing)]
fn launcher_step(kb: &KBDLLHOOKSTRUCT, down: bool, mods: Mods) -> Option<bool> {
    // SAFETY: GetAsyncKeyState has no preconditions. Inside the callback it reads the state from before this event
    // (LowLevelKeyboardProc's documentation), so a down it reads as down is a repeat of a press Windows already has.
    let held = down && unsafe { GetAsyncKeyState(kb.vkCode as i32) } < 0;
    let ev =
        KeyEv { vk: kb.vkCode, scan: kb.scanCode, ext: kb.flags.0 & LLKHF_EXTENDED.0 != 0, down, time: kb.time, held };
    let mut s = load_chord_state(mods);
    let (swallow, act) = chord_step(&mut s, &RING, ev);
    store_chord_state(&s);
    match act {
        Act::Nothing => {}
        Act::Post(seq) => {
            // A failed SendInput still posts (the mask is cosmetic); a failed post stays unacknowledged, which fails
            // open on the next chord (whose note lets a live UI acknowledge it again), and its window expires at the
            // deadline.
            send_mask();
            post_launcher(WM_DOCK_LAUNCHER, seq as usize, kb.time);
        }
        Act::Yielded => post_launcher(WM_DOCK_LAUNCHER_YIELDED, 0, kb.time),
        Act::Passed(seq) => post_launcher(WM_DOCK_LAUNCHER_PASSED, seq as usize, kb.time),
        Act::FailOpen { seq, note } => {
            post_launcher(WM_DOCK_LAUNCHER_PASSED, seq as usize, kb.time);
            if note {
                // SAFETY: PostThreadMessageW is thread-safe; this thread has a message queue (thread_main's PeekMessageW).
                let _ = unsafe {
                    PostThreadMessageW(
                        GetCurrentThreadId(),
                        WM_HOOK_NOTE,
                        WPARAM(NOTE_LAUNCHER_FAIL_OPEN),
                        LPARAM(seq as isize),
                    )
                };
            }
        }
    }
    swallow
}

#[deny(clippy::unwrap_used, clippy::expect_used, clippy::indexing_slicing)]
fn post_launcher(msg: u32, wparam: usize, time: u32) {
    let h = hwnd();
    if h.0.is_null() {
        return; // without this check PostMessageW(NULL) would post to the hook thread
    }
    // SAFETY: PostMessageW is thread-safe; the error is ignored (no I/O in the callback).
    let _ = unsafe { PostMessageW(Some(h), msg, WPARAM(wparam), LPARAM(time as isize)) };
}

/// [MASK_KEY down, MASK_KEY up], tagged, while Alt is still down: the Alt release is then no lone Alt tap.
#[deny(clippy::unwrap_used, clippy::expect_used, clippy::indexing_slicing)]
fn send_mask() {
    let vk = MASK_KEY.load(Relaxed);
    if vk == 0 {
        return;
    }
    let inputs = [key_down_input(vk, 0, false), key_up_input(vk)];
    // SAFETY: slice of 2 initialized INPUTs; cbSize = size_of::<INPUT>() as required. The injected events re-enter
    // the callback with INJECT_TAG and pass through.
    let _ = unsafe { SendInput(&inputs, std::mem::size_of::<INPUT>() as i32) };
}

// ------------------------------------------------------------------ the dock's owned chords (SPEC-dock 5.2.2)
//
// The chord table and its API (dock-keys Z2). The hook's use of it (the Win branch) lands in DK-HOOK; the UI's in
// DK-KEYS. Every function denies unwrap, expect and indexing: a panic in the callback aborts the dock.

/// The dock's owned chords (SPEC-dock 5.2.2), one bit per row of the chord table. NONE: the Win branch behaves as in
/// release 1.5.2. Bits 8-15 are reserved for TakeoverChord (P3-dock-suite).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct DockChords(pub u16);

#[deny(clippy::unwrap_used, clippy::expect_used, clippy::indexing_slicing)]
impl DockChords {
    pub const NONE: DockChords = DockChords(0);
    pub const DIGITS: DockChords = DockChords(1 << 0); // Win+1..9, Win+0
    pub const SHIFT_DIGITS: DockChords = DockChords(1 << 1); // Win+Shift+n
    pub const ALT_DIGITS: DockChords = DockChords(1 << 2); // Win+Alt+n
    pub const WIN_T: DockChords = DockChords(1 << 3); // Win+T
    pub const WIN_SHIFT_T: DockChords = DockChords(1 << 4); // Win+Shift+T
    pub const WIN_B: DockChords = DockChords(1 << 5); // Win+B
    pub const PEEK: DockChords = DockChords(1 << 6); // Win held PEEK_MS alone

    pub const fn has(self, f: DockChords) -> bool {
        f.0 != 0 && self.0 & f.0 == f.0
    }

    pub const fn with(self, f: DockChords) -> DockChords {
        DockChords(self.0 | f.0)
    }
}

/// What an owned chord asks the UI to do; `slot` 0..=9 is the n-th app item in bar order (Win+1 = 0, Win+0 = 9).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ChordAction {
    App(u8),
    NewInstance(u8),
    AppMenu(u8),
    FocusFirst,
    FocusLast,
    FocusTray,
    Sync,
}

/// Win held this long with no other key: Peek begins.
pub const PEEK_MS: u32 = 600;
/// A WM_DOCK_CHORD older than this (kb.time to the UI's GetTickCount) acts on nothing (acknowledged only).
pub const CHORD_STALE_MS: u32 = 1000;
/// Owned chords pass to Windows while a post stays unacknowledged this long (the UI thread is stuck).
pub const CHORD_ACK_TIMEOUT_MS: u32 = 1500;

/// The owned chord rows, `DockChords` bits.
static DOCK_CHORDS: AtomicU16 = AtomicU16::new(0);
/// A fullscreen window is in front: Peek is suppressed.
static DOCK_FS: AtomicBool = AtomicBool::new(false);
/// The sequence the UI last acknowledged (`ack_chord`).
static CHORD_ACKED: AtomicU16 = AtomicU16::new(0);

/// The slot of a top-row digit key: 0x31-0x39 are slots 0-8, 0x30 is slot 9.
#[deny(clippy::unwrap_used, clippy::expect_used, clippy::indexing_slicing)]
const fn digit_slot(vk: u32) -> Option<u8> {
    match vk {
        0x31..=0x39 => Some((vk - 0x31) as u8),
        0x30 => Some(9),
        _ => None,
    }
}

/// Pure: the action of `vk` pressed while a Win key is held with exactly `mods` (Shift/Ctrl/Alt; Win excluded), if its
/// row is owned. Top-row digits 0x30-0x39 only; Ctrl (and AltGr = Ctrl+Alt) never matches.
#[deny(clippy::unwrap_used, clippy::expect_used, clippy::indexing_slicing)]
pub fn resolve_chord(owned: DockChords, vk: u32, mods: Mods) -> Option<ChordAction> {
    if mods == Mods::NONE {
        if let Some(n) = digit_slot(vk) {
            return owned.has(DockChords::DIGITS).then_some(ChordAction::App(n));
        }
        return match vk {
            0x54 if owned.has(DockChords::WIN_T) => Some(ChordAction::FocusFirst),
            0x42 if owned.has(DockChords::WIN_B) => Some(ChordAction::FocusTray),
            _ => None,
        };
    }
    if mods == Mods::SHIFT {
        if let Some(n) = digit_slot(vk) {
            return owned.has(DockChords::SHIFT_DIGITS).then_some(ChordAction::NewInstance(n));
        }
        return (vk == 0x54 && owned.has(DockChords::WIN_SHIFT_T)).then_some(ChordAction::FocusLast);
    }
    if mods == Mods::ALT {
        if let Some(n) = digit_slot(vk) {
            return owned.has(DockChords::ALT_DIGITS).then_some(ChordAction::AppMenu(n));
        }
    }
    None
}

/// wParam of WM_DOCK_CHORD: bits 0-7 kind (1 App, 2 NewInstance, 3 AppMenu, 4 FocusFirst, 5 FocusLast, 6 FocusTray,
/// 7 Sync), bits 8-15 slot, bits 16-31 the hook's sequence. decode(encode(a, s)) == Some((a, s)); kind 0 or > 7 is None.
#[deny(clippy::unwrap_used, clippy::expect_used, clippy::indexing_slicing)]
pub fn encode_chord(a: ChordAction, seq: u16) -> usize {
    let (kind, slot) = match a {
        ChordAction::App(n) => (1usize, n),
        ChordAction::NewInstance(n) => (2, n),
        ChordAction::AppMenu(n) => (3, n),
        ChordAction::FocusFirst => (4, 0),
        ChordAction::FocusLast => (5, 0),
        ChordAction::FocusTray => (6, 0),
        ChordAction::Sync => (7, 0),
    };
    kind | ((slot as usize) << 8) | ((seq as usize) << 16)
}

/// The inverse of `encode_chord`; a kind outside 1..=7 is None, so is a slot above 9 on a slot kind.
#[deny(clippy::unwrap_used, clippy::expect_used, clippy::indexing_slicing)]
pub fn decode_chord(wparam: usize) -> Option<(ChordAction, u16)> {
    let kind = wparam & 0xFF;
    let slot = ((wparam >> 8) & 0xFF) as u8;
    let seq = ((wparam >> 16) & 0xFFFF) as u16;
    let a = match kind {
        1..=3 if slot > 9 => return None,
        1 => ChordAction::App(slot),
        2 => ChordAction::NewInstance(slot),
        3 => ChordAction::AppMenu(slot),
        4 => ChordAction::FocusFirst,
        5 => ChordAction::FocusLast,
        6 => ChordAction::FocusTray,
        7 => ChordAction::Sync,
        _ => return None,
    };
    Some((a, seq))
}

/// UI thread: the owned chords (NONE when hide_native_taskbar is off, or while a remote-desktop or VM client or a
/// window above the dock is in front). One atomic store, read by the callback.
#[deny(clippy::unwrap_used, clippy::expect_used, clippy::indexing_slicing)]
pub fn set_dock_chords(t: DockChords) {
    DOCK_CHORDS.store(t.0, Release);
}

/// UI thread, on each foreground change while PEEK is owned: a fullscreen window is in front (Peek suppressed).
#[deny(clippy::unwrap_used, clippy::expect_used, clippy::indexing_slicing)]
pub fn set_dock_fullscreen(on: bool) {
    DOCK_FS.store(on, Release);
}

/// UI thread, first thing on WM_DOCK_CHORD: the post of `seq` arrived.
#[deny(clippy::unwrap_used, clippy::expect_used, clippy::indexing_slicing)]
pub fn ack_chord(seq: u16) {
    CHORD_ACKED.store(seq, Release);
}

// ------------------------------------------------------------------ the takeover items (dock-suite DS-0A)
//
// The swallow of Ctrl+Shift+Esc (and, reserved, Win+E): the items the UI, the hook and the shell thread share. The
// behaviour lands in DS-HOOK; the policy that fills the gate in DS-GATE. Every function denies unwrap, expect and
// indexing: a panic in the callback aborts the dock.

#[deny(clippy::unwrap_used, clippy::expect_used, clippy::indexing_slicing)]
impl DockChords {
    /// Ctrl+Shift+Esc to Tasks through TakeoverChord (bits 8-15 are the takeovers).
    pub const CTRL_SHIFT_ESC: DockChords = DockChords(1 << 8);
    /// Win+E to Folio: reserved for a later release; never set in 1.8.0.
    #[allow(dead_code)] // read by the Files takeover (reserved)
    pub const WIN_E: DockChords = DockChords(1 << 9);

    /// The dock's own rows only (bits 0-7): what `win_step` sees.
    pub const fn rows(self) -> DockChords {
        DockChords(self.0 & 0x00FF)
    }

    /// The takeover bits only (8-15).
    pub const fn takeovers(self) -> DockChords {
        DockChords(self.0 & 0xFF00)
    }
}

/// Which app a swallowed takeover chord opens.
#[repr(u8)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TakeoverTarget {
    Tasks = 1,
    /// Reserved for the Files takeover.
    Files = 2,
}

/// What the UI publishes for the hook's swallow decision (the cached conditions of the takeover).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct TakeoverGate(pub u32);

#[deny(clippy::unwrap_used, clippy::expect_used, clippy::indexing_slicing)]
#[allow(dead_code)] // NONE and with: read by DS-GATE G2
impl TakeoverGate {
    pub const NONE: TakeoverGate = TakeoverGate(0);
    /// `takeover::availability` is Ok and the setting is on (on, installed, enabled, stable or test, build, fallback).
    pub const TASKS_READY: u32 = 1 << 0;
    /// The foreground window is a Tasks window: the press passes to Windows.
    pub const TASKS_IN_FRONT: u32 = 1 << 1;
    // bits 8-15 reserved for Files

    pub const fn has(self, bit: u32) -> bool {
        self.0 & bit != 0
    }

    pub const fn with(self, bit: u32, on: bool) -> TakeoverGate {
        if on {
            TakeoverGate(self.0 | bit)
        } else {
            TakeoverGate(self.0 & !bit)
        }
    }
}

/// Unacknowledged takeover posts this old fail open (the same bound as the dock chords').
pub const TAKEOVER_ACK_TIMEOUT_MS: u32 = CHORD_ACK_TIMEOUT_MS;

/// The UI's takeover gate (`TakeoverGate` bits).
static TAKEOVER_GATE: AtomicU32 = AtomicU32::new(0);
/// 0 = closed, else the takeover passes to Windows until `GetTickCount` reaches it.
static TAKEOVER_BREAKER: AtomicU32 = AtomicU32::new(0);
/// 0 exactly while the shell thread waits in GetMessageW, else `GetTickCount() | 1`.
static SHELL_BUSY: AtomicU32 = AtomicU32::new(0);
/// The sequence of the newest takeover post, and the newest the shell thread acknowledged.
static TAKEOVER_SEQ: AtomicU16 = AtomicU16::new(0);
static TAKEOVER_ACKED: AtomicU16 = AtomicU16::new(0);
/// Tick of the oldest unacknowledged post (0 = none), and `kb.time` of the newest post.
static TAKEOVER_OLDEST: AtomicU32 = AtomicU32::new(0);
static TAKEOVER_SENT_TIME: AtomicU32 = AtomicU32::new(0);
/// The hook's swallow state of the Esc key (press swallowed, its repeats and release follow it) and its last time.
static TAKE_ESC_BIT: AtomicBool = AtomicBool::new(false);
static TAKE_ESC_LAST: AtomicU32 = AtomicU32::new(0);

/// UI thread: the takeover gate. One Release store, read by the callback.
#[deny(clippy::unwrap_used, clippy::expect_used, clippy::indexing_slicing)]
#[allow(dead_code)] // read by DS-GATE G2
pub fn set_takeover_gate(g: TakeoverGate) {
    TAKEOVER_GATE.store(g.0, Release);
}

/// Any thread: the last gate the UI published.
#[deny(clippy::unwrap_used, clippy::expect_used, clippy::indexing_slicing)]
pub fn takeover_gate() -> TakeoverGate {
    TakeoverGate(TAKEOVER_GATE.load(Acquire))
}

/// Any thread: the last `set_dock_chords` value.
#[deny(clippy::unwrap_used, clippy::expect_used, clippy::indexing_slicing)]
#[allow(dead_code)] // read by DS-GATE G2
pub fn dock_chords() -> DockChords {
    DockChords(DOCK_CHORDS.load(Acquire))
}

/// 0 = closed, else pass-through until `GetTickCount` reaches `until_tick`. Written by the shell thread on a trip
/// and by the UI thread with 0 when the takeover is switched off.
#[deny(clippy::unwrap_used, clippy::expect_used, clippy::indexing_slicing)]
#[allow(dead_code)] // read by DS-DELIVER D3 (set) and DS-GATE G2 (clear)
pub fn set_takeover_breaker(until_tick: u32) {
    TAKEOVER_BREAKER.store(until_tick, Release);
}

#[deny(clippy::unwrap_used, clippy::expect_used, clippy::indexing_slicing)]
pub fn takeover_breaker() -> u32 {
    TAKEOVER_BREAKER.load(Acquire)
}

/// Shell thread: 0 exactly while it waits in GetMessageW, else `GetTickCount() | 1`.
#[deny(clippy::unwrap_used, clippy::expect_used, clippy::indexing_slicing)]
pub fn set_shell_busy(since_tick: u32) {
    SHELL_BUSY.store(since_tick, Release);
}

/// The shell thread's busy stamp as the hook reads it.
#[deny(clippy::unwrap_used, clippy::expect_used, clippy::indexing_slicing)]
pub fn shell_busy() -> u32 {
    SHELL_BUSY.load(Acquire)
}

/// Shell thread, first statement of the WM_SHELL_TAKEOVER handling: the post of `seq` arrived.
#[deny(clippy::unwrap_used, clippy::expect_used, clippy::indexing_slicing)]
pub fn ack_takeover(seq: u16) {
    TAKEOVER_ACKED.store(seq, Release);
}

/// `(seq, kb.time)` of the newest post: the lost-message check.
#[deny(clippy::unwrap_used, clippy::expect_used, clippy::indexing_slicing)]
#[allow(dead_code)] // read by DS-DELIVER D3
pub fn takeover_last_sent() -> (u16, u32) {
    (TAKEOVER_SEQ.load(Acquire), TAKEOVER_SENT_TIME.load(Acquire))
}

/// wParam of WM_SHELL_TAKEOVER: bits 0-7 target, bits 16-31 seq; lParam = kb.time. `decode(encode(t, s)) == Some((t, s))`;
/// target 0 or above 2 decodes to None.
#[deny(clippy::unwrap_used, clippy::expect_used, clippy::indexing_slicing)]
pub fn encode_takeover(t: TakeoverTarget, seq: u16) -> usize {
    (t as usize) | ((seq as usize) << 16)
}

/// The inverse of `encode_takeover`.
#[deny(clippy::unwrap_used, clippy::expect_used, clippy::indexing_slicing)]
#[allow(dead_code)] // read by DS-DELIVER D1
pub fn decode_takeover(wparam: usize) -> Option<(TakeoverTarget, u16)> {
    let t = match wparam & 0xFF {
        1 => TakeoverTarget::Tasks,
        2 => TakeoverTarget::Files,
        _ => return None,
    };
    Some((t, ((wparam >> 16) & 0xFFFF) as u16))
}

/// [Ctrl down, Shift down, Esc down, Esc up, Shift up, Ctrl up], each tagged INJECT_TAG, in one SendInput; true if 6
/// were accepted. The fallback when Windows opens its Task Manager for a re-injected chord. Shell thread, never the
/// callback: the tagged keys re-enter the callback and pass (`accepted`), so Windows alone answers them.
#[deny(clippy::unwrap_used, clippy::expect_used, clippy::indexing_slicing)]
#[allow(dead_code)] // read by DS-DELIVER D2
pub fn inject_ctrl_shift_esc() -> bool {
    let inputs = ctrl_shift_esc_inputs();
    // SAFETY: slice of 6 initialized INPUTs; cbSize = size_of::<INPUT>() as required.
    let sent = unsafe { SendInput(&inputs, std::mem::size_of::<INPUT>() as i32) };
    sent == 6
}

/// The six inputs of `inject_ctrl_shift_esc`: Ctrl, Shift and Esc down, then their ups in reverse, each tagged.
#[deny(clippy::unwrap_used, clippy::expect_used, clippy::indexing_slicing)]
fn ctrl_shift_esc_inputs() -> [INPUT; 6] {
    [
        key_down_input(VK_CONTROL_, 0, false),
        key_down_input(VK_SHIFT_, 0, false),
        key_down_input(VK_ESCAPE_, 0, false),
        key_up_input(VK_ESCAPE_),
        key_up_input(VK_SHIFT_),
        key_up_input(VK_CONTROL_),
    ]
}

// ------------------------------------------------------------------ the takeover step (dock-suite H1)
//
// One pure state machine over the takeover statics, as the Win branch's: the callback loads `TakeState`, runs
// `take_decide` on an Esc event, posts a candidate to the shell thread and commits it (`take_commit`), then stores the
// state back. Every function denies unwrap, expect and indexing: a panic in the callback aborts the dock.

/// Esc, the takeover's key, and the modifiers of its re-injection (`inject_ctrl_shift_esc`).
const VK_ESCAPE_: u32 = 0x1B;
const VK_CONTROL_: u32 = 0x11;
const VK_SHIFT_: u32 = 0x10;

/// The takeover's swallow state, loaded from the takeover statics on each Esc event and stored back (hook thread).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct TakeState {
    /// The Esc down of a takeover was swallowed: its auto-repeat and its up are swallowed too.
    esc_bit: bool,
    /// `kb.time` of the last Esc event the bit swallowed.
    esc_last: u32,
    /// The newest sequence posted on WM_SHELL_TAKEOVER, and the newest the shell thread acknowledged (`ack_takeover`).
    sent: u16,
    acked: u16,
    /// `kb.time` of the oldest post not yet acknowledged: the start of the fail-open window (`takeover_fail_open`).
    oldest: u32,
}

/// One key event as the takeover step sees it, with the conditions the UI (`set_dock_chords`, `set_takeover_gate`)
/// and the shell thread (`set_shell_busy`, `set_takeover_breaker`) publish.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct TakeIn {
    vk: u32,
    down: bool,
    time: u32,
    /// Shift, Ctrl and Alt as the hook tracks them (`mods_step`), and Win while the Win branch tracks a press.
    mods: Mods,
    /// The owned chords, the takeover bits included.
    chords: DockChords,
    gate: TakeoverGate,
    /// `takeover_breaker()`: 0 = closed, else the takeover passes to Windows until the tick count reaches it.
    breaker_until: u32,
    /// `shell_busy()`: 0 exactly while the shell thread waits in GetMessageW.
    shell_busy: u32,
    /// The shell thread runs and GetShellWindow is not null; read by the caller only for a press that can be the chord.
    shell_ok: bool,
    /// Windows already holds Esc down (its async state, read like `shell_ok`): this down is the auto-repeat of a press
    /// that reached Windows, whose up must reach it too.
    held: bool,
}

/// What the callback does with a key event (`take_decide`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum TakeDecision {
    /// Not the takeover's: the event goes on to the Win branch.
    NotOurs,
    /// The auto-repeat or the up of an Esc whose down was swallowed: swallowed, nothing posted.
    SwallowSilent,
    /// Ctrl+Shift+Esc under the cached conditions: posted to the shell thread with this sequence, then swallowed and
    /// committed (`take_commit`); a failed post passes the key to Windows.
    Candidate(u16),
}

/// The takeover step, pure: `s` is the state before the event and becomes the state after it. In order: (1) an Esc
/// whose down was swallowed has its auto-repeat (a down within REPEAT_WINDOW_MS of the last one) and its up swallowed;
/// a down past that window clears the bit and goes on as a fresh press; (2) an Esc down Windows does not hold yet, with
/// exactly Ctrl and Shift (no Alt, no Win, either side), the Ctrl+Shift+Esc takeover owned, Tasks ready and not in
/// front, the breaker closed or expired, the shell thread waiting in GetMessageW, the shell up and no post
/// unacknowledged for TAKEOVER_ACK_TIMEOUT_MS is a candidate with the next sequence; (3) everything else is not the
/// takeover's.
#[deny(clippy::unwrap_used, clippy::expect_used, clippy::indexing_slicing)]
pub(crate) fn take_decide(s: &mut TakeState, ev: TakeIn) -> TakeDecision {
    if ev.vk != VK_ESCAPE_ {
        return TakeDecision::NotOurs;
    }
    if s.esc_bit {
        if !ev.down {
            s.esc_bit = false;
            return TakeDecision::SwallowSilent;
        }
        if ev.time.wrapping_sub(s.esc_last) <= REPEAT_WINDOW_MS {
            s.esc_last = ev.time;
            return TakeDecision::SwallowSilent;
        }
        s.esc_bit = false;
    }
    let paused = ev.breaker_until != 0 && (ev.time.wrapping_sub(ev.breaker_until) as i32) < 0;
    let ours = ev.down
        && !ev.held
        && ev.mods == CTRL_SHIFT
        && ev.chords.has(DockChords::CTRL_SHIFT_ESC)
        && ev.gate.has(TakeoverGate::TASKS_READY)
        && !ev.gate.has(TakeoverGate::TASKS_IN_FRONT)
        && !paused
        && ev.shell_busy == 0
        && ev.shell_ok
        && !takeover_fail_open(s.sent, s.acked, s.oldest, ev.time);
    if ours {
        TakeDecision::Candidate(s.sent.wrapping_add(1))
    } else {
        TakeDecision::NotOurs
    }
}

/// A candidate's post succeeded: `seq` is the newest post, the Esc bit is set (its repeats and its up follow it) and,
/// when no post was pending before it, the fail-open window starts at `time`.
#[deny(clippy::unwrap_used, clippy::expect_used, clippy::indexing_slicing)]
pub(crate) fn take_commit(s: &mut TakeState, seq: u16, time: u32) {
    if s.sent == s.acked {
        s.oldest = time;
    }
    s.sent = seq;
    s.esc_bit = true;
    s.esc_last = time;
}

/// A takeover post has stayed unacknowledged for more than TAKEOVER_ACK_TIMEOUT_MS since the oldest pending one
/// (`oldest`): the shell thread is stuck, so Ctrl+Shift+Esc passes to Windows. Sequences and times compare with wrapping.
#[deny(clippy::unwrap_used, clippy::expect_used, clippy::indexing_slicing)]
pub(crate) fn takeover_fail_open(sent: u16, acked: u16, oldest: u32, now: u32) -> bool {
    (sent.wrapping_sub(acked) as i16) > 0 && now.wrapping_sub(oldest) > TAKEOVER_ACK_TIMEOUT_MS
}

/// The watchdog's repair of a stale Win press (`Repair::ClearThenGo`) forgets an Esc press the takeover swallowed: its
/// up may have gone by unseen too. The sequences stay, so a post in flight keeps its fail-open window.
#[deny(clippy::unwrap_used, clippy::expect_used, clippy::indexing_slicing)]
fn clear_take_for_repair(s: &mut TakeState) {
    s.esc_bit = false;
}

#[deny(clippy::unwrap_used, clippy::expect_used, clippy::indexing_slicing)]
fn load_take_state() -> TakeState {
    TakeState {
        esc_bit: TAKE_ESC_BIT.load(Relaxed),
        esc_last: TAKE_ESC_LAST.load(Relaxed),
        sent: TAKEOVER_SEQ.load(Relaxed),
        acked: TAKEOVER_ACKED.load(Acquire),
        oldest: TAKEOVER_OLDEST.load(Relaxed),
    }
}

/// Stores the hook-owned fields back (`acked` is the shell thread's). The sequence goes last, with Release: a shell
/// thread that reads it (`takeover_last_sent`) reads the post's time too, stored before it by `post_takeover`.
#[deny(clippy::unwrap_used, clippy::expect_used, clippy::indexing_slicing)]
fn store_take_state(s: &TakeState) {
    TAKE_ESC_BIT.store(s.esc_bit, Relaxed);
    TAKE_ESC_LAST.store(s.esc_last, Relaxed);
    TAKEOVER_OLDEST.store(s.oldest, Relaxed);
    TAKEOVER_SEQ.store(s.sent, Release);
}

/// The takeover step's input on the statics. The shell (`shell_ready`: GetShellWindow) and Esc's async state
/// (`esc_held`) are read only for a press that can be the chord: an Esc down with Ctrl and Shift tracked down and
/// Ctrl+Shift+Esc owned.
#[deny(clippy::unwrap_used, clippy::expect_used, clippy::indexing_slicing)]
fn take_in(kb: &KBDLLHOOKSTRUCT, down: bool, mods: Mods, chords: DockChords) -> TakeIn {
    let candidate = down && mods == CTRL_SHIFT && chords.has(DockChords::CTRL_SHIFT_ESC);
    TakeIn {
        vk: kb.vkCode,
        down,
        time: kb.time,
        mods,
        chords,
        gate: takeover_gate(),
        breaker_until: takeover_breaker(),
        shell_busy: shell_busy(),
        shell_ok: candidate && shell_ready(),
        held: candidate && esc_held(),
    }
}

/// Windows already holds Esc down: an Esc down now is the auto-repeat of a press that reached Windows.
#[deny(clippy::unwrap_used, clippy::expect_used, clippy::indexing_slicing)]
fn esc_held() -> bool {
    // SAFETY: GetAsyncKeyState has no preconditions. Inside the callback it reads the state from before this event
    // (LowLevelKeyboardProc's documentation), so a down it reads as down is a repeat of a press Windows already has.
    let state = unsafe { GetAsyncKeyState(VK_ESCAPE_ as i32) };
    state < 0
}

/// The shell thread runs and the shell's window exists (Explorer is up): a delivery can start.
#[deny(clippy::unwrap_used, clippy::expect_used, clippy::indexing_slicing)]
fn shell_ready() -> bool {
    // SAFETY: GetShellWindow has no preconditions; it returns null while no shell runs.
    crate::shell_thread::thread_id() != 0 && !unsafe { GetShellWindow() }.0.is_null()
}

/// Posts WM_SHELL_TAKEOVER for Tasks with `seq` (lParam = kb.time) to the shell thread; true when it was queued. On
/// success the post's time is published for the lost-message check (`takeover_last_sent`), before its sequence.
#[deny(clippy::unwrap_used, clippy::expect_used, clippy::indexing_slicing)]
fn post_takeover(seq: u16, time: u32) -> bool {
    let tid = crate::shell_thread::thread_id();
    if tid == 0 {
        return false;
    }
    let wparam = WPARAM(encode_takeover(TakeoverTarget::Tasks, seq));
    // SAFETY: PostThreadMessageW is thread-safe; a thread that ended or a full queue makes it fail, and the key passes.
    let posted = unsafe { PostThreadMessageW(tid, WM_SHELL_TAKEOVER, wparam, LPARAM(time as isize)) }.is_ok();
    if posted {
        TAKEOVER_SENT_TIME.store(time, Release);
    }
    posted
}

// ------------------------------------------------------------------ the Win branch (dock-keys H1-H3)
//
// One pure state machine over the Win statics: `win_key` loads `WinState`, runs `win_step` and stores it back. With
// no dock chord owned it is release 1.5.2's Win branch, rule for rule (the module doc). With owned rows (`DockChords`),
// a chord key pressed while Win is held is swallowed and posted (WM_DOCK_CHORD) instead of reaching Windows with Win,
// and its auto-repeat and its release are swallowed after it; a modifier pressed while Win is held passes alone and
// leaves Win pending (Win+Shift+n and Win+Alt+n are chords too), and the menu-mask key keeps its release from being a
// lone tap. The modifiers are the hook's own (`ModTrack`), which runs whenever the launcher chord or a dock row is set.

/// The Win branch's state, loaded from the Win statics on each event and stored back (hook thread only).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct WinState {
    /// A Win press is tracked; `vk` is its key.
    down: bool,
    /// The pair [Win down, key down] was injected for this press: Windows holds Win, later keys and the Win up pass.
    other: bool,
    vk: u32,
    /// `kb.time` of the last Win event, auto-repeat included, or of a key the press kept with no gap (`renew`).
    last_time: u32,
    /// `kb.time | 1` of the last lone Win release that toggled; 0 = none, or consumed by a double tap.
    last_alone_up: u32,
    /// This press started within DOUBLE_TAP_MS of `last_alone_up`.
    double: bool,
    /// An owned chord was posted during this press: Windows never saw this Win down.
    owned: bool,
    /// A modifier went down while Win was held and passed alone: Win stays pending, Windows saw the modifier only.
    modded: bool,
    /// Peek is on: WM_DOCK_PEEK(1) was posted for this press.
    peeking: bool,
    /// `kb.time` of this press's Win down: the press the Peek timer was armed for.
    press: u32,
    /// The keys whose down an owned chord swallowed (`chord_bit`): their auto-repeat and their up are swallowed too.
    chord_keys: u16,
    /// `kb.time` of the last event of a key whose `chord_keys` bit is set (only the newest key auto-repeats).
    chord_last: u32,
    /// Windows holds this press's Win key down though the hook injected no pair for it: a key-down of it went by the
    /// hook unseen (a reinstall, a time-out). Noted by the watchdog's tick (`note_leak`) or read at the release
    /// (`note_release`).
    leaked: bool,
}

/// One physical key event as the Win branch sees it, with the UI's settings.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct WinIn {
    vk: u32,
    down: bool,
    time: u32,
    /// Shift, Ctrl and Alt as the hook tracks them (`mods_step`), Win removed.
    mods: Mods,
    /// The owned rows (`set_dock_chords`).
    chords: DockChords,
    /// A fullscreen window is in front (`set_dock_fullscreen`): Peek is not armed.
    fullscreen: bool,
    /// The UI left a chord post unacknowledged for CHORD_ACK_TIMEOUT_MS (`chord_fail_open`): Windows gets the chords.
    fail_open: bool,
    /// The dock window exists, so a post reaches it.
    ui_ready: bool,
}

/// What the callback does besides swallowing or passing.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum WinAct {
    None,
    /// Post WM_DOCK_TOGGLE.
    Toggle,
    /// Post WM_DOCK_START_MENU.
    StartMenu,
    /// Inject [Win down, key down]; a failed SendInput passes the key instead and notes EV_SENDFAIL.
    Inject,
    /// Post WM_DOCK_CHORD with this action.
    Chord(ChordAction),
}

/// The Peek timer (a thread timer of the hook thread, `peek_timer`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum PeekCmd {
    Keep,
    /// Arm it for the press of this time: PEEK_MS later `peek_fired` checks `peek_due`.
    Arm(u32),
    Kill,
}

/// `win_step`'s answer for one event.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct WinOut {
    swallow: bool,
    act: WinAct,
    /// Post `ChordAction::Sync` as well: an owned chord failed open, and a live UI acknowledges the newest sequence.
    sync: bool,
    /// Inject the menu-mask key (`send_mask`) after the posts.
    mask: bool,
    /// Post WM_DOCK_PEEK(0): Peek ended.
    peek_end: bool,
    /// The Peek timer.
    peek: PeekCmd,
    /// WM_DOCK_WINKEY's event (log only).
    note: Option<usize>,
}

impl WinOut {
    const PASS: WinOut = WinOut {
        swallow: false,
        act: WinAct::None,
        sync: false,
        mask: false,
        peek_end: false,
        peek: PeekCmd::Keep,
        note: None,
    };
    const SWALLOW: WinOut = WinOut { swallow: true, ..WinOut::PASS };
}

/// Hook-written Win state besides the 1.5.2 statics above (Relaxed: the hook thread is the only reader and writer).
static WIN_OWNED: AtomicBool = AtomicBool::new(false);
static WIN_MODDED: AtomicBool = AtomicBool::new(false);
static WIN_PEEKING: AtomicBool = AtomicBool::new(false);
static WIN_PRESS: AtomicU32 = AtomicU32::new(0);
/// A failed SetTimer of the Peek timer is noted (and logged) once per process.
static PEEK_FAIL_NOTED: AtomicBool = AtomicBool::new(false);
static CHORD_KEYS: AtomicU16 = AtomicU16::new(0);
static CHORD_LAST: AtomicU32 = AtomicU32::new(0);
static WIN_LEAKED: AtomicBool = AtomicBool::new(false);
/// The newest sequence the hook posted on WM_DOCK_CHORD, and `kb.time` of the oldest post not yet acknowledged.
static CHORD_SEQ: AtomicU16 = AtomicU16::new(0);
static CHORD_OLDEST: AtomicU32 = AtomicU32::new(0);

#[deny(clippy::unwrap_used, clippy::expect_used, clippy::indexing_slicing)]
fn load_win_state() -> WinState {
    WinState {
        down: WIN_DOWN.load(Relaxed),
        other: WIN_OTHER.load(Relaxed),
        vk: WIN_VK.load(Relaxed),
        last_time: WIN_LAST_TIME.load(Relaxed),
        last_alone_up: LAST_ALONE_UP.load(Relaxed),
        double: WIN_DOUBLE.load(Relaxed),
        owned: WIN_OWNED.load(Relaxed),
        modded: WIN_MODDED.load(Relaxed),
        peeking: WIN_PEEKING.load(Relaxed),
        press: WIN_PRESS.load(Relaxed),
        chord_keys: CHORD_KEYS.load(Relaxed),
        chord_last: CHORD_LAST.load(Relaxed),
        leaked: WIN_LEAKED.load(Relaxed),
    }
}

#[deny(clippy::unwrap_used, clippy::expect_used, clippy::indexing_slicing)]
fn store_win_state(s: &WinState) {
    WIN_DOWN.store(s.down, Relaxed);
    WIN_OTHER.store(s.other, Relaxed);
    WIN_VK.store(s.vk, Relaxed);
    WIN_LAST_TIME.store(s.last_time, Relaxed);
    LAST_ALONE_UP.store(s.last_alone_up, Relaxed);
    WIN_DOUBLE.store(s.double, Relaxed);
    WIN_OWNED.store(s.owned, Relaxed);
    WIN_MODDED.store(s.modded, Relaxed);
    WIN_PEEKING.store(s.peeking, Relaxed);
    WIN_PRESS.store(s.press, Relaxed);
    CHORD_KEYS.store(s.chord_keys, Relaxed);
    CHORD_LAST.store(s.chord_last, Relaxed);
    WIN_LEAKED.store(s.leaked, Relaxed);
}

/// Left or right Win.
#[deny(clippy::unwrap_used, clippy::expect_used, clippy::indexing_slicing)]
const fn is_win(vk: u32) -> bool {
    vk == VK_LWIN.0 as u32 || vk == VK_RWIN.0 as u32
}

/// Shift, Ctrl or Alt, generic or of either side.
#[deny(clippy::unwrap_used, clippy::expect_used, clippy::indexing_slicing)]
const fn is_modifier(vk: u32) -> bool {
    matches!(vk, 0x10..=0x12 | 0xA0..=0xA5)
}

/// Alt, generic or of either side.
#[deny(clippy::unwrap_used, clippy::expect_used, clippy::indexing_slicing)]
const fn is_alt(vk: u32) -> bool {
    matches!(vk, 0x12 | 0xA4 | 0xA5)
}

/// The callback runs the modifier tracker (`mods_step`) while the launcher chord or any dock row is set.
#[deny(clippy::unwrap_used, clippy::expect_used, clippy::indexing_slicing)]
fn tracks(launcher_set: bool, dock: DockChords) -> bool {
    launcher_set || dock != DockChords::NONE
}

/// The `chord_keys` bit of a key an owned chord can swallow: the top-row digits 0x30-0x39 bits 0-9, T bit 10, B bit 11.
#[deny(clippy::unwrap_used, clippy::expect_used, clippy::indexing_slicing)]
const fn chord_bit(vk: u32) -> Option<u16> {
    match vk {
        0x30..=0x39 => Some(1 << (vk - 0x30)),
        0x54 => Some(1 << 10),
        0x42 => Some(1 << 11),
        _ => None,
    }
}

/// A chord post has stayed unacknowledged for more than CHORD_ACK_TIMEOUT_MS since the oldest pending one (`oldest`):
/// the UI thread is stuck, so owned chords pass to Windows. Sequences and times compare with wrapping.
#[deny(clippy::unwrap_used, clippy::expect_used, clippy::indexing_slicing)]
fn chord_fail_open(sent: u16, acked: u16, oldest: u32, now: u32) -> bool {
    (sent.wrapping_sub(acked) as i16) > 0 && now.wrapping_sub(oldest) > CHORD_ACK_TIMEOUT_MS
}

/// The Win branch, pure: `s` is the state before the event and becomes the state after it.
#[deny(clippy::unwrap_used, clippy::expect_used, clippy::indexing_slicing)]
pub(crate) fn win_step(s: &mut WinState, ev: WinIn) -> WinOut {
    match (is_win(ev.vk), ev.down) {
        (true, true) => win_down(s, ev),
        (true, false) => win_up(s, ev),
        (false, true) => key_down(s, ev),
        (false, false) => key_up(s, ev),
    }
}

/// A Win key down: a fresh press, its auto-repeat, or the other Win key while one is held (an "other key").
#[deny(clippy::unwrap_used, clippy::expect_used, clippy::indexing_slicing)]
fn win_down(s: &mut WinState, ev: WinIn) -> WinOut {
    let fresh = ev.time.wrapping_sub(s.last_time) > REPEAT_WINDOW_MS;
    s.last_time = ev.time;
    if s.down && !fresh {
        if ev.vk == s.vk {
            // Auto-repeat: still "pending" -> swallow; already re-injected -> let through.
            return WinOut { swallow: !s.other, ..WinOut::PASS };
        }
        // The other Win key while the first is held: it is an "other key".
        return key_down(s, ev);
    }
    // A fresh press; Peek of a press whose release was lost ends here.
    let peek_end = std::mem::take(&mut s.peeking);
    s.down = true;
    s.other = false;
    s.owned = false;
    s.modded = false;
    s.leaked = false;
    s.vk = ev.vk;
    s.press = ev.time;
    s.double = s.last_alone_up != 0 && ev.time.wrapping_sub(s.last_alone_up) <= DOUBLE_TAP_MS;
    let peek = if ev.chords.has(DockChords::PEEK) && ev.chords.has(DockChords::DIGITS) && !ev.fullscreen {
        PeekCmd::Arm(ev.time)
    } else {
        PeekCmd::Kill
    };
    WinOut { note: Some(EV_PRESSED), peek_end, peek, ..WinOut::SWALLOW }
}

/// A Win key up, the table of the module doc: after an injected pair it passes; with a row owned, when Windows holds
/// this Win down (`leaked`) it passes behind the menu-mask key and toggles nothing; during Peek, or for a press that
/// owned a chord or passed a modifier, it is swallowed and toggles nothing; otherwise the tap toggles the dock (a double
/// tap opens Start). The release of anything but the tracked press passes.
#[deny(clippy::unwrap_used, clippy::expect_used, clippy::indexing_slicing)]
fn win_up(s: &mut WinState, ev: WinIn) -> WinOut {
    // Every Win up kills the Peek timer: it lives only between a Win down and its release.
    let out = win_up_plan(s, ev);
    WinOut { peek: PeekCmd::Kill, ..out }
}

#[deny(clippy::unwrap_used, clippy::expect_used, clippy::indexing_slicing)]
fn win_up_plan(s: &mut WinState, ev: WinIn) -> WinOut {
    if !s.down || ev.vk != s.vk {
        return WinOut::PASS;
    }
    s.down = false;
    let peeking = std::mem::take(&mut s.peeking);
    let owned = std::mem::take(&mut s.owned);
    let modded = std::mem::take(&mut s.modded);
    let leaked = std::mem::take(&mut s.leaked);
    if s.other {
        // Win used in a combination: it does not count as the first tap of a double tap.
        s.last_alone_up = 0;
        s.double = false;
        return WinOut { peek_end: peeking, ..WinOut::PASS };
    }
    if leaked && ev.chords != DockChords::NONE {
        // Windows holds this Win down though the hook passed none of its downs (`note_release`, `note_leak`): the
        // release reaches Windows behind the menu-mask key, so it opens no Start menu, and the press is no tap.
        s.last_alone_up = 0;
        s.double = false;
        return WinOut { mask: true, peek_end: peeking, note: Some(EV_LEAK), ..WinOut::PASS };
    }
    if peeking || owned || modded {
        // Windows never saw this Win down: its release is swallowed too, and it is no tap. A modifier Windows saw alone
        // that is still down (Alt) gets the menu-mask key, so its later release opens no menu bar.
        s.last_alone_up = 0;
        s.double = false;
        let mask = modded && ev.mods.has(Mods::ALT);
        return WinOut { mask, peek_end: peeking, ..WinOut::SWALLOW };
    }
    if std::mem::take(&mut s.double) {
        // Second quick tap: Start menu; the next tap starts a fresh sequence.
        s.last_alone_up = 0;
        return WinOut { act: WinAct::StartMenu, note: Some(EV_DOUBLE), ..WinOut::SWALLOW };
    }
    // `| 1`: a real timestamp of 0 must not read as "no previous tap".
    s.last_alone_up = ev.time | 1;
    WinOut { act: WinAct::Toggle, note: Some(EV_ALONE), ..WinOut::SWALLOW }
}

/// A key (other than the held Win) pressed, in order: the stale rule, the auto-repeat of a key an owned chord swallowed,
/// an owned chord, a modifier passed alone, the pair [Win down, key down] injected (the key swallowed) as in 1.5.2.
#[deny(clippy::unwrap_used, clippy::expect_used, clippy::indexing_slicing)]
fn key_down(s: &mut WinState, ev: WinIn) -> WinOut {
    // 1. Win release lost (secure desktop): Win is no longer held, and a Peek of it ends. No key injects the pair
    //    after REPEAT_WINDOW_MS of silence (Win's events, or the keys the press kept, `renew`). After such a pause, a
    //    press that owned a chord or passed a modifier is still kept up to LONE_STUCK_MS (as the watchdog judges a lone
    //    press, `repair_plan`) for a key that step 3 owns or step 4 passes, neither of which injects; any other key,
    //    and every key beyond, takes this rule.
    let age = ev.time.wrapping_sub(s.last_time);
    let kept = (s.owned || s.modded) && age <= LONE_STUCK_MS && injects_nothing(ev);
    if s.down && !s.other && age > REPEAT_WINDOW_MS && !kept {
        s.down = false;
        s.owned = false;
        s.modded = false;
        s.leaked = false;
        if std::mem::take(&mut s.peeking) {
            return WinOut { peek_end: true, peek: PeekCmd::Kill, ..key_down_rules(s, ev) };
        }
    }
    key_down_rules(s, ev)
}

/// The stale rule's test for a press that owned a chord or passed a modifier: `ev` is a key that step 3 owns as a
/// chord or step 4 passes as a modifier, so keeping the press injects no pair.
#[deny(clippy::unwrap_used, clippy::expect_used, clippy::indexing_slicing)]
fn injects_nothing(ev: WinIn) -> bool {
    let chord = ev.ui_ready
        && !ev.fail_open
        && chord_bit(ev.vk).is_some()
        && resolve_chord(ev.chords, ev.vk, ev.mods).is_some();
    chord || (ev.chords != DockChords::NONE && is_modifier(ev.vk))
}

/// `key_down` after the stale rule: steps 2 to 5. Every step that makes the press unable to Peek (an owned chord, a
/// modifier, the pair) kills the Peek timer; the pair also ends a running Peek (Windows now holds Win).
#[deny(clippy::unwrap_used, clippy::expect_used, clippy::indexing_slicing)]
fn key_down_rules(s: &mut WinState, ev: WinIn) -> WinOut {
    // 2. A key an owned chord swallowed: its auto-repeat is swallowed, Win held or not, so a digit held after Win's
    //    release never types. A bit with no event for REPEAT_WINDOW_MS lost its release: cleared, a fresh press.
    let bit = chord_bit(ev.vk).unwrap_or(0);
    if s.chord_keys & bit != 0 {
        if ev.time.wrapping_sub(s.chord_last) > REPEAT_WINDOW_MS {
            s.chord_keys &= !bit;
        } else {
            s.chord_last = ev.time;
            renew(s, ev.time);
            return WinOut::SWALLOW;
        }
    }
    if !s.down {
        return WinOut::PASS;
    }
    // 3. An owned chord: swallowed and posted, unless the UI thread is stuck (failed open: Windows gets it as today, and
    //    a Sync post lets a live UI acknowledge again) or the dock window does not exist yet.
    let mut sync = false;
    if let Some(a) = resolve_chord(ev.chords, ev.vk, ev.mods).filter(|_| ev.ui_ready && bit != 0) {
        if !ev.fail_open {
            s.owned = true;
            s.chord_keys |= bit;
            s.chord_last = ev.time;
            renew(s, ev.time);
            // With Shift or Alt the menu-mask key follows the post: the modifier's release is no lone tap.
            let mask = ev.mods.has(Mods::SHIFT) || ev.mods.has(Mods::ALT);
            return WinOut { act: WinAct::Chord(a), mask, peek: PeekCmd::Kill, ..WinOut::SWALLOW };
        }
        sync = true;
    }
    if s.other {
        return WinOut { sync, ..WinOut::PASS };
    }
    // 4. A modifier while rows are owned: it passes alone and Win stays pending, so Win+Shift+n and Win+Alt+n can be
    //    chords (without rows it injects the pair, as in 1.5.2).
    if ev.chords != DockChords::NONE && is_modifier(ev.vk) {
        s.modded = true;
        renew(s, ev.time);
        return WinOut { peek: PeekCmd::Kill, ..WinOut::PASS };
    }
    // 5. Today's injection, once per press; it ends a running Peek.
    s.other = true;
    let peek_end = std::mem::take(&mut s.peeking);
    WinOut { act: WinAct::Inject, sync, peek_end, peek: PeekCmd::Kill, note: Some(EV_COMBO), ..WinOut::SWALLOW }
}

/// A key the held press keeps (an owned chord key, its swallowed auto-repeat, a modifier passed alone) is the newest
/// key: it auto-repeats and Win no longer does. Coming within REPEAT_WINDOW_MS of `last_time`, it shows the hook was
/// never cut off (the secure desktop, a removed hook), so Win's release was not lost: `last_time` moves to it, for the
/// stale rule and the watchdog's age alike. After a gap it stays, so a lost release still takes the stale rule.
#[deny(clippy::unwrap_used, clippy::expect_used, clippy::indexing_slicing)]
fn renew(s: &mut WinState, time: u32) {
    if s.down && time.wrapping_sub(s.last_time) <= REPEAT_WINDOW_MS {
        s.last_time = time;
    }
}

/// Callback, at a Win up before `win_step`: at the release of the tracked press, with a row owned and no pair injected,
/// `held` (the async key state, which inside the callback is the state from before this event) tells whether Windows
/// holds that Win key down; if so the press notes it (`leaked`), and its release passes (`win_up_plan`). `held` is not
/// called otherwise: with no row owned, nothing is read.
#[deny(clippy::unwrap_used, clippy::expect_used, clippy::indexing_slicing)]
fn note_release(s: &mut WinState, ev: WinIn, held: impl FnOnce(u32) -> bool) {
    let tracked = is_win(ev.vk) && !ev.down && s.down && ev.vk == s.vk && !s.other;
    if ev.chords != DockChords::NONE && tracked && held(ev.vk) {
        s.leaked = true;
    }
}

/// The watchdog's tick (decision of the hook thread's loop, never the callback): Windows holds the Win key of the
/// tracked press (`lwin`, `rwin`: the async state of each) though the hook injected no pair for it, so a key-down of
/// it went by the hook unseen. The press notes it (`leaked`): true the first time for the press, for its one line.
#[deny(clippy::unwrap_used, clippy::expect_used, clippy::indexing_slicing)]
fn note_leak(s: &mut WinState, lwin: bool, rwin: bool) -> bool {
    let held = (s.vk == VK_LWIN.0 as u32 && lwin) || (s.vk == VK_RWIN.0 as u32 && rwin);
    s.down && !s.other && held && !std::mem::replace(&mut s.leaked, true)
}

/// The Peek timer of `armed_press` fired: Peek begins when that press is still held alone (no pair injected, no chord
/// owned, no modifier passed) and no fullscreen window is in front.
#[deny(clippy::unwrap_used, clippy::expect_used, clippy::indexing_slicing)]
fn peek_due(s: &WinState, armed_press: u32, fullscreen: bool) -> bool {
    s.down && !s.other && !s.owned && !s.modded && s.press == armed_press && !fullscreen
}

/// The heartbeat is due while a Win press that owned a chord or passed a modifier is held: the only press whose lost
/// release costs anything, its digits, T and B staying chords for up to LONE_STUCK_MS.
#[deny(clippy::unwrap_used, clippy::expect_used, clippy::indexing_slicing)]
fn heartbeat_due(s: &WinState) -> bool {
    s.down && (s.owned || s.modded)
}

/// The watchdog's repair of a press whose release was lost (`Repair::ClearThenGo`): the press, its double-tap memory
/// and the chord state are forgotten (a chord key's bit included). true when Peek was on: the caller posts its end.
#[deny(clippy::unwrap_used, clippy::expect_used, clippy::indexing_slicing)]
fn clear_for_repair(s: &mut WinState) -> bool {
    s.down = false;
    s.other = false;
    s.double = false;
    s.last_alone_up = 0;
    s.owned = false;
    s.modded = false;
    s.leaked = false;
    s.chord_keys = 0;
    std::mem::take(&mut s.peeking)
}

/// A cut-off (`CutOff`): keys went by the hook unseen, perhaps Win's release. None while no row is owned and the press
/// is none of the dock's (no chord owned, no modifier passed, no Peek): release 1.5.2's Win branch keeps its own stale
/// rule. Otherwise the press is forgotten with its double-tap memory, and Some(true) when Peek was on (the caller posts
/// its end). The keys an owned chord swallowed keep their bits: an up that still comes is swallowed, a lost one expires
/// at the key's next down (`key_down_rules` step 2).
#[deny(clippy::unwrap_used, clippy::expect_used, clippy::indexing_slicing)]
fn forget_after_cut_off(s: &mut WinState, rows: DockChords) -> Option<bool> {
    if rows == DockChords::NONE && !s.owned && !s.modded && !s.peeking {
        return None;
    }
    s.down = false;
    s.other = false;
    s.owned = false;
    s.modded = false;
    s.leaked = false;
    s.double = false;
    s.last_alone_up = 0;
    Some(std::mem::take(&mut s.peeking))
}

/// A key (other than Win) released: the up of a key an owned chord swallowed is swallowed (its down never reached
/// Windows), Win held or not; an Alt up that Windows would read as a lone Alt tap (it saw that Alt down alone, Win
/// held and pending) passes after the menu-mask key; every other release passes.
#[deny(clippy::unwrap_used, clippy::expect_used, clippy::indexing_slicing)]
fn key_up(s: &mut WinState, ev: WinIn) -> WinOut {
    let bit = chord_bit(ev.vk).unwrap_or(0);
    if s.chord_keys & bit != 0 {
        s.chord_keys &= !bit;
        return WinOut::SWALLOW;
    }
    WinOut { mask: is_alt(ev.vk) && s.down && s.modded && !s.other, ..WinOut::PASS }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn watchdog_repairs_a_stale_win_state() {
        use Repair::*;
        const STALE: u32 = REPEAT_WINDOW_MS + 1;
        const LONG: u32 = LONE_STUCK_MS + 1;
        assert_eq!(repair_plan(false, false, false, false, 0), Go);
        assert_eq!(repair_plan(false, true, false, true, LONG), Go, "not tracked as held: nothing to protect");
        assert_eq!(repair_plan(true, true, false, true, STALE), Skip, "Win really held: do not touch the hook mid-combination");
        assert_eq!(repair_plan(true, false, true, true, STALE), Skip);
        assert_eq!(repair_plan(true, true, false, false, LONG), Skip, "async state says held: never clear");
        assert_eq!(repair_plan(true, false, false, false, 0), Go, "swallowed Win-down never reaches async state: kept");
        assert_eq!(repair_plan(true, false, false, true, REPEAT_WINDOW_MS), Go, "chord still within the repeat window");
        assert_eq!(repair_plan(true, false, false, true, STALE), ClearThenGo, "Win+L left WIN_DOWN stuck");
        assert_eq!(repair_plan(true, false, false, false, STALE), Go, "lone Win held with no auto-repeat: kept");
        assert_eq!(repair_plan(true, false, false, false, LONE_STUCK_MS), Go, "lone Win at the limit: still held");
        assert_eq!(repair_plan(true, false, false, false, LONG), ClearThenGo, "lone release lost on the secure desktop");
    }

    /// wsC Q6: the reinstall tick is armed by the first activity of a burst, not by every key.
    #[test]
    fn first_activity_arms_once() {
        let mut t = Tick::default();
        assert!(t.on_activity(), "the first activity arms the tick");
        assert!(!t.on_activity(), "an armed tick is not armed again");
    }

    #[test]
    fn ticks_stop_after_one_quiet_period() {
        let mut t = Tick::default();
        assert!(t.on_activity());
        assert!(t.on_tick(false), "activity in the period: keep ticking");
        assert!(!t.on_tick(false), "a quiet period: stop");
        assert!(t.on_activity(), "the next activity arms it again");
    }

    #[test]
    fn steady_typing_keeps_ticking() {
        let mut t = Tick::default();
        assert!(t.on_activity());
        for _ in 0..10 {
            assert!(!t.on_activity(), "still armed: no second timer");
            assert!(t.on_tick(false));
        }
    }

    #[test]
    fn a_tracked_win_press_keeps_ticking_until_repaired() {
        let mut t = Tick::default();
        assert!(t.on_activity());
        assert!(t.on_tick(false), "the activity's own period");
        assert!(t.on_tick(true), "no activity, a Win press tracked: keep, so `repair_plan` sees it end");
        assert!(t.on_tick(true));
        assert!(!t.on_tick(false), "repaired (or released): stop");
    }

    /// wsC Q6: injected keys (the rig's F24 taps among them) make `handle_key` return at once, so the callback arms the
    /// tick before it calls `handle_key`.
    #[test]
    fn activity_comes_before_handle_key() {
        let src = include_str!("hook.rs");
        let body = src.find("fn ll_keyboard_proc(").map(|at| &src[at..]).unwrap();
        let act = body.find("activity();").unwrap();
        let key = body.find("handle_key(").unwrap();
        assert!(act < key, "activity() must precede handle_key in ll_keyboard_proc");
    }

    /// L2-C1: the Clipboard history command sends Win+V through `ui::send_win_key`. Its four events, as this hook sees
    /// them (injected, Win extended, every one tagged), pass untouched: none is swallowed, the Win-tap state that toggles
    /// the dock or opens its Start stays unarmed, and the tracked modifiers do not change.
    #[test]
    fn tagged_win_v_passes_and_arms_nothing() {
        use windows::Win32::UI::WindowsAndMessaging::{KBDLLHOOKSTRUCT_FLAGS, LLKHF_UP};
        // The filter is the callback's first step, so a tagged event changes no state; checked before `handle_key`
        // runs, so a broken filter fails here and never reaches the Win branch's SendInput.
        let src = include_str!("hook.rs");
        let sig = "fn handle_key(msg: u32, kb: &KBDLLHOOKSTRUCT) -> bool {";
        let body = src.find(sig).map(|at| &src[at + sig.len()..]).unwrap();
        let first: Vec<&str> = body.split_whitespace().take(6).collect();
        assert_eq!(first, ["if", "!accepted(kb.flags.0,", "kb.dwExtraInfo)", "{", "return", "false;"]);
        let state = || {
            (
                (WIN_DOWN.load(Relaxed), WIN_OTHER.load(Relaxed), WIN_VK.load(Relaxed), WIN_LAST_TIME.load(Relaxed)),
                (LAST_ALONE_UP.load(Relaxed), WIN_DOUBLE.load(Relaxed)),
                (MODS.load(Relaxed), MODS_LAST.load(Relaxed)),
            )
        };
        let before = state();
        let mut seen = Vec::new();
        for (i, input) in crate::ui::win_key_inputs(VIRTUAL_KEY(0x56)).iter().enumerate() {
            assert_eq!(input.r#type, INPUT_KEYBOARD);
            // SAFETY: a keyboard input (checked above): `ki` is the union's live field.
            let ki = unsafe { input.Anonymous.ki };
            let up = ki.dwFlags.0 & KEYEVENTF_KEYUP.0 != 0;
            let ext = ki.dwFlags.0 & KEYEVENTF_EXTENDEDKEY.0 != 0;
            let mut flags = LLKHF_INJECTED.0;
            if ext {
                flags |= LLKHF_EXTENDED.0;
            }
            if up {
                flags |= LLKHF_UP.0;
            }
            let kb = KBDLLHOOKSTRUCT {
                vkCode: u32::from(ki.wVk.0),
                scanCode: u32::from(ki.wScan),
                flags: KBDLLHOOKSTRUCT_FLAGS(flags),
                time: 10_000 + i as u32,
                dwExtraInfo: ki.dwExtraInfo,
            };
            assert_eq!(kb.dwExtraInfo, INJECT_TAG, "event {i} is tagged");
            assert!(!accepted(kb.flags.0, kb.dwExtraInfo), "event {i}: not a key of the user's");
            assert!(!admitted(kb.flags.0, kb.dwExtraInfo, true), "event {i}: not even under the probe's switch");
            assert!(!handle_key(if up { WM_KEYUP } else { WM_KEYDOWN }, &kb), "event {i} passes");
            seen.push((kb.vkCode, up, ext));
        }
        assert_eq!(seen, [(0x5B, false, true), (0x56, false, false), (0x56, true, false), (0x5B, true, true)]);
        assert_eq!(state(), before, "nothing armed, nothing tracked");
    }

    // ------------------------------------------------------------------ the launcher chord

    use crate::launcher::field::KeyEvent;
    use crate::launcher::gesture::Gesture;
    use std::sync::Barrier;

    const SPACE: u32 = 0x20;
    const ENTER: u32 = 0x0D;
    const LALT: u32 = 0xA4;
    const RALT: u32 = 0xA5;
    const LCTRL: u32 = 0xA2;
    const LSHIFT: u32 = 0xA0;
    const LWIN: u32 = 0x5B;
    const KEY_A: u32 = 0x41;
    const KEY_C: u32 = 0x43;
    const KEY_E: u32 = 0x45;
    const TAB: u32 = 0x09;
    const ESC: u32 = 0x1B;
    const ALT_SPACE: ChordSpec = ChordSpec { vk: 0x20, mods: Mods::ALT };
    const ALT_SHIFT: Mods = Mods(Mods::ALT.0 | Mods::SHIFT.0);
    const CTRL_SHIFT_: Mods = Mods(Mods::CTRL.0 | Mods::SHIFT.0);

    /// Chord Alt+Space, nothing pressed, acked = sent = 0.
    fn st() -> ChordState {
        ChordState { chord: Some(ALT_SPACE), ..ChordState::default() }
    }

    fn ev(vk: u32, down: bool, t: u32) -> KeyEv {
        KeyEv { vk, scan: vk & 0x7F, ext: false, down, time: t, held: false }
    }

    /// The hook's side of a keyboard: its tracked modifiers, its chord state and a local ring; `seen` is what Windows
    /// got (the passed events), `posts` the sequences posted to the UI, `notes` the fail-open notes.
    struct Kbd {
        s: ChordState,
        ring: Ring,
        track: ModTrack,
        seen: Vec<(u32, bool)>,
        posts: Vec<u32>,
        notes: Vec<u32>,
        /// The WM_DOCK_LAUNCHER_PASSED notes of a chord passed under FULLSCREEN.
        passed: u32,
        /// The wParam of every WM_DOCK_LAUNCHER_PASSED note (FULLSCREEN passes and fail-opens): the sequence the UI
        /// acknowledges.
        passed_seqs: Vec<u32>,
    }

    impl Kbd {
        fn new() -> Kbd {
            Kbd {
                s: st(),
                ring: Ring::new(),
                track: ModTrack::default(),
                seen: Vec::new(),
                posts: Vec::new(),
                notes: Vec::new(),
                passed: 0,
                passed_seqs: Vec::new(),
            }
        }

        /// One physical event; Some(true) swallowed, Some(false) passed by the launcher, None not the launcher's.
        fn key(&mut self, vk: u32, down: bool, t: u32) -> Option<bool> {
            track_step(&mut self.track, 0, vk, down, t, |_| true);
            self.s.mods = self.track.mods(self.s.win_down);
            // Windows' async state: the last event of this key it got was a down.
            let held = down && self.seen.iter().rev().find(|e| e.0 == vk).is_some_and(|e| e.1);
            let (swallow, act) = chord_step(&mut self.s, &self.ring, KeyEv { held, ..ev(vk, down, t) });
            match act {
                Act::Post(seq) => self.posts.push(seq),
                Act::FailOpen { seq, note } => {
                    if note {
                        self.notes.push(seq);
                    }
                    self.passed_seqs.push(seq);
                }
                Act::Passed(seq) => {
                    self.passed += 1;
                    self.passed_seqs.push(seq);
                }
                Act::Yielded | Act::Nothing => {}
            }
            if swallow != Some(true) {
                self.seen.push((vk, down));
            }
            swallow
        }

        /// Alt down, Space down, Space up, Alt up from `t`: the swallow results of the four.
        fn chord(&mut self, t: u32) -> [Option<bool>; 4] {
            [
                self.key(LALT, true, t - 10),
                self.key(SPACE, true, t),
                self.key(SPACE, false, t + 20),
                self.key(LALT, false, t + 30),
            ]
        }

        fn tap(&mut self, vk: u32, t: u32) -> (Option<bool>, Option<bool>) {
            (self.key(vk, true, t), self.key(vk, false, t + 5))
        }

        fn take(&self, seq: u32) -> Vec<u32> {
            let mut out = [KeyRec::default(); RING_CAP];
            let n = self.ring.take(seq, &mut out);
            out[..n].iter().map(|r| u32::from(r.vk)).collect()
        }
    }

    /// Every key Windows saw go down came back up, and no up came without its down (repeats are downs of a held key).
    fn balanced(seen: &[(u32, bool)]) -> bool {
        let mut held: Vec<u32> = Vec::new();
        for &(vk, down) in seen {
            match (down, held.iter().position(|&h| h == vk)) {
                (true, None) => held.push(vk),
                (true, Some(_)) => {}
                (false, Some(i)) => {
                    held.remove(i);
                }
                (false, None) => return false,
            }
        }
        held.is_empty()
    }

    #[test]
    fn resolve_launcher_table() {
        let down = |mods: Mods| {
            let mut s = st();
            s.mods = mods;
            resolve_launcher(&s, ev(SPACE, true, 100))
        };
        assert_eq!(down(Mods::ALT), Some(Verdict::SwallowPost(1)), "Alt+Space");
        assert_eq!(down(Mods(Mods::CTRL.0 | Mods::ALT.0)), None, "AltGr is Ctrl+Alt");
        assert_eq!(down(Mods::SHIFT), None, "Shift+Space");
        assert_eq!(down(Mods::NONE), None, "Space");
        assert_eq!(down(ALT_SHIFT), None, "Alt+Shift+Space is not Alt+Space");
        let mut s = st();
        s.chord = Some(ChordSpec { vk: 0x20, mods: ALT_SHIFT });
        s.mods = ALT_SHIFT;
        assert_eq!(resolve_launcher(&s, ev(SPACE, true, 100)), Some(Verdict::SwallowPost(1)), "Alt+Shift+Space");
        s.mods = Mods::ALT;
        assert_eq!(resolve_launcher(&s, ev(SPACE, true, 100)), None);
        // Win held.
        let mut s = st();
        s.mods = Mods::ALT.with(Mods::WIN);
        s.win_down = true;
        assert_eq!(resolve_launcher(&s, ev(SPACE, true, 100)), None, "Win held");
        // The next sequence number.
        let mut s = st();
        s.mods = Mods::ALT;
        s.ack = AckWatch { sent: 7, sent_at: 50, ack_seen: 7 };
        s.acked = 7;
        assert_eq!(resolve_launcher(&s, ev(SPACE, true, 100)), Some(Verdict::SwallowPost(8)));
        // Each pass flag.
        for (f, v) in [
            (PassFlags::FULLSCREEN, Verdict::PassFullscreen),
            (PassFlags::REMOTE, Verdict::Pass),
            (PassFlags::YIELD, Verdict::PassNotify),
            (PassFlags::FULLSCREEN.with(PassFlags::YIELD), Verdict::PassFullscreen),
            (PassFlags::FULLSCREEN.with(PassFlags::REMOTE), Verdict::PassFullscreen),
        ] {
            let mut s = st();
            s.mods = Mods::ALT;
            s.flags = f;
            assert_eq!(resolve_launcher(&s, ev(SPACE, true, 100)), Some(v), "{f:?}");
        }
        // Repeats and the release of a swallowed Space are swallowed; a passed Space passes, its release too.
        let mut s = st();
        s.mods = Mods::ALT;
        s.space = SpaceState::Swallowed;
        s.space_time = 90;
        assert_eq!(resolve_launcher(&s, ev(SPACE, true, 100)), Some(Verdict::SwallowSilent), "repeat");
        assert_eq!(resolve_launcher(&s, ev(SPACE, false, 120)), Some(Verdict::SwallowSilent), "release");
        s.mods = Mods::NONE;
        assert_eq!(resolve_launcher(&s, ev(SPACE, false, 120)), Some(Verdict::SwallowSilent), "Alt let go first");
        s.space = SpaceState::Passed;
        assert_eq!(resolve_launcher(&s, ev(SPACE, true, 100)), Some(Verdict::Pass));
        assert_eq!(resolve_launcher(&s, ev(SPACE, false, 120)), Some(Verdict::Pass));
        // A swallowed press whose release was lost (secure desktop) does not eat the next press.
        let mut s = st();
        s.space = SpaceState::Swallowed;
        s.space_time = 100;
        assert_eq!(resolve_launcher(&s, ev(SPACE, true, 100 + MOD_STALE_MS + 1)), None);
        // No chord: nothing is the launcher's, and nothing changes.
        let mut k = Kbd::new();
        k.s.chord = None;
        for (vk, down, t) in
            [(LALT, true, 0), (SPACE, true, 10), (KEY_A, true, 20), (KEY_A, false, 30), (SPACE, false, 40)]
        {
            let before = k.s;
            assert_eq!(k.key(vk, down, t), None);
            assert_eq!(k.s, ChordState { mods: k.s.mods, ..before });
        }
        assert!(k.posts.is_empty() && !k.ring.ctl().is_open());
        // A chord passed under FULLSCREEN posts one note per press (not for its repeat or release), so the UI checks
        // the flag again: a window can leave fullscreen without a foreground change.
        let mut k = Kbd::new();
        k.s.flags = PassFlags::FULLSCREEN;
        assert_eq!(k.chord(1_000), [None, Some(false), Some(false), None]);
        assert_eq!((k.passed, k.posts.len()), (1, 0));
        k.key(LALT, true, 1_990);
        let press = [k.key(SPACE, true, 2_000), k.key(SPACE, true, 2_030)];
        assert_eq!(press, [Some(false); 2], "the press and its repeat pass");
        assert_eq!(k.passed, 2, "one note for the press, none for its repeat");
    }

    #[test]
    fn physical_filter() {
        assert!(!physical(LLKHF_INJECTED.0, 0), "injected");
        assert!(!physical(LLKHF_INJECTED.0 | 0x02, 0), "injected from a lower integrity level");
        assert!(!physical(0, INJECT_TAG), "our own tag");
        assert!(physical(LLKHF_EXTENDED.0, 0));
        assert!(physical(0, 0));
    }

    /// The rows of `physical_filter`, and an injection of another app that carries some other extra info.
    const FILTER_ROWS: [(u32, usize); 7] = [
        (0, 0),
        (LLKHF_EXTENDED.0, 0),
        (LLKHF_INJECTED.0, 0),
        (LLKHF_INJECTED.0 | 0x02, 0),
        (LLKHF_INJECTED.0, 0x1234),
        (0, INJECT_TAG),
        (LLKHF_INJECTED.0, INJECT_TAG),
    ];

    #[test]
    fn probe_switch_off_admits_only_physical_keys() {
        for (flags, extra) in FILTER_ROWS {
            assert_eq!(admitted(flags, extra, false), physical(flags, extra), "{flags:#x} {extra:#x}");
            // The hook's own filter: no build sets the switch in tests (`start` is never called), so it is `physical`.
            assert_eq!(accepted(flags, extra), physical(flags, extra), "{flags:#x} {extra:#x}");
        }
        assert!(!admitted(LLKHF_INJECTED.0 | 0x02, 0, false), "a test agent's key stays ignored");
    }

    #[test]
    fn probe_switch_on_admits_other_apps_injections_never_ours() {
        assert!(admitted(LLKHF_INJECTED.0, 0, true), "another app's injection counts as physical");
        assert!(admitted(LLKHF_INJECTED.0 | 0x02, 0, true), "also from a lower integrity level (a non-elevated agent)");
        assert!(admitted(LLKHF_INJECTED.0, 0x1234, true), "any extra info but ours");
        assert!(admitted(0, 0, true) && admitted(LLKHF_EXTENDED.0, 0, true), "physical keys still count");
        assert!(!admitted(0, INJECT_TAG, true), "our own tag passes through: the hook would loop on it");
        assert!(!admitted(LLKHF_INJECTED.0, INJECT_TAG, true), "our own re-injection passes through");
        for (flags, extra) in FILTER_ROWS {
            assert_eq!(admitted(flags, extra, true), extra != INJECT_TAG, "{flags:#x} {extra:#x}");
        }
    }

    #[test]
    fn unacked_1500_fails_open() {
        // Post at 10,000; the next chord at 11,499 is still swallowed.
        let mut k = Kbd::new();
        assert_eq!(k.chord(10_000), [None, Some(true), Some(true), None]);
        assert_eq!(k.posts, [1]);
        assert_eq!(k.chord(11_499)[1], Some(true));
        assert_eq!(k.posts, [1, 2]);
        // At 11,500 it fails open: passed, with one note, and LAST_FAILOPEN_SEQ is the first sequence.
        let mut k = Kbd::new();
        k.chord(10_000);
        assert_eq!(k.chord(11_500), [None, Some(false), Some(false), None], "the press and its release pass");
        assert_eq!((k.s.last_failopen, k.notes.as_slice()), (1, &[1][..]));
        assert_eq!(k.chord(11_600)[1], Some(false), "still failing open");
        assert_eq!(k.notes, [1], "one note per sequence");
        assert!(balanced(&k.seen));
        // The UI acknowledges: the next chord is swallowed and posted again.
        k.s.acked = 1;
        assert_eq!(k.chord(12_000)[1], Some(true));
        assert_eq!(k.posts, [1, 2]);
        // Progress (an acknowledged post) gives the rest a fresh window.
        let mut k = Kbd::new();
        k.chord(10_000);
        k.chord(11_000);
        k.s.acked = 1;
        assert_eq!(k.chord(11_600)[1], Some(true), "#1 acknowledged at 11,600: #2 gets 1500 ms from now");
        assert_eq!(k.chord(13_099)[1], Some(true));
        assert_eq!(k.chord(13_100)[1], Some(false));
        assert_eq!(k.notes, [4]);
    }

    #[test]
    fn a_lost_post_fails_open_until_the_ui_reads_the_note() {
        // Post 1 is lost (PostMessageW failed): the UI never acknowledges it.
        let mut k = Kbd::new();
        k.chord(10_000);
        assert_eq!(k.posts, [1]);
        // The next chord, 1,500 ms later, fails open and notes the newest sequence to the dock window.
        assert_eq!(k.chord(11_500), [None, Some(false), Some(false), None]);
        assert_eq!((k.notes.as_slice(), k.passed_seqs.as_slice()), (&[1][..], &[1][..]));
        // A live UI acknowledges the noted sequence (`resync_launcher`): the next chord is swallowed, posted and opens.
        k.s.acked = k.passed_seqs[0];
        assert_eq!(k.chord(11_600)[1], Some(true));
        assert_eq!(k.posts, [1, 2]);
        assert_eq!(post_verdict(2, 11_600, 11_610, 2, k.s.last_failopen, false), PostAction::Open);
        assert!(balanced(&k.seen));
        // While the UI does not answer, every chord fails open and notes again (the log note stays one per sequence).
        let mut k = Kbd::new();
        k.chord(10_000);
        k.chord(11_500);
        assert_eq!(k.chord(11_700)[1], Some(false));
        assert_eq!((k.notes.as_slice(), k.passed_seqs.as_slice()), (&[1][..], &[1, 1][..]));
        // A chord passed under FULLSCREEN carries the newest sequence too.
        let mut k = Kbd::new();
        k.chord(10_000);
        k.s.acked = 1;
        k.s.flags = PassFlags::FULLSCREEN;
        assert_eq!(k.chord(10_500)[1], Some(false));
        assert_eq!((k.passed, k.passed_seqs.as_slice()), (1, &[1][..]));
    }

    #[test]
    fn typeahead_window() {
        let mut k = Kbd::new();
        k.chord(10_000);
        assert_eq!(k.tap(KEY_A, 10_100), (Some(true), Some(true)), "typed after the chord: kept for the launcher");
        assert_eq!(k.key(SPACE, true, 10_200), Some(true), "a plain Space too");
        assert_eq!(k.key(SPACE, false, 10_210), Some(true));
        assert_eq!(k.key(KEY_C, true, 10_500), Some(true), "500 ms after the post");
        assert_eq!(k.key(KEY_C, false, 10_501), Some(true), "its down was kept, so its up is too");
        assert_eq!(k.tap(KEY_E, 10_501), (None, None), "501 ms: passed");
        assert!(balanced(&k.seen));
        // Taken before the deadline, the ring holds the downs in order.
        let mut k = Kbd::new();
        k.chord(10_000);
        k.tap(KEY_A, 10_100);
        k.key(KEY_C, true, 10_200);
        assert_eq!(k.take(1), [KEY_A, KEY_C]);
        let mut out = [KeyRec::default(); RING_CAP];
        assert_eq!(k.ring.take(1, &mut out), 0);
        assert_eq!(k.key(KEY_C, false, 10_250), Some(true), "the up of a taken key stays swallowed");
        assert_eq!(k.tap(KEY_E, 10_260), (None, None), "the window ended at the take");
    }

    #[test]
    fn ring_records_what_the_ui_replays() {
        let mut k = Kbd::new();
        k.key(LALT, true, 9_990);
        k.key(SPACE, true, 10_000);
        k.key(SPACE, false, 10_020);
        k.key(0x73, true, 10_030);
        k.key(LALT, false, 10_040);
        k.key(LSHIFT, true, 10_050);
        k.key(KEY_A, true, 10_060);
        let mut out = [KeyRec::default(); RING_CAP];
        assert_eq!(k.ring.take(1, &mut out), 2);
        assert_eq!(out[0], KeyRec { vk: 0x73, scan: 0x73 & 0x7F, ext: false, mods: Mods::ALT, time: 10_030 }, "Alt+F4");
        assert_eq!(out[1], KeyRec { vk: 0x41, scan: 0x41, ext: false, mods: Mods::SHIFT, time: 10_060 });
        let r = KeyRec { vk: 0xFF, scan: 0xFFF, ext: true, mods: Mods(0xF), time: u32::MAX };
        assert_eq!(KeyRec::unpack(r.pack()), r);
    }

    #[test]
    fn ring_discarded_after_deadline() {
        let mut k = Kbd::new();
        k.chord(10_000);
        k.tap(KEY_A, 10_100);
        assert_eq!(k.key(KEY_E, true, 10_501), None, "the first key after the deadline passes");
        assert!(k.ring.ctl().is_ended(), "and expires the ring");
        assert!(k.take(1).is_empty());
        assert_eq!(k.key(KEY_E, false, 10_510), None);
    }

    #[test]
    fn ring_overflow_drops_beyond_32() {
        let mut k = Kbd::new();
        k.chord(10_000);
        let vks: Vec<u32> = (0x30..=0x39).chain(0x41..=0x57).collect();
        assert_eq!(vks.len(), 33);
        for (i, &vk) in vks.iter().enumerate() {
            assert_eq!(k.key(vk, true, 10_100 + i as u32 * 10), Some(true), "{vk:#x}");
        }
        assert_eq!(k.key(0x57, false, 10_450), Some(true), "the 33rd: swallowed, not stored, its up swallowed");
        let taken = k.take(1);
        assert_eq!(taken.len(), 32);
        assert!(!taken.contains(&0x57));
        assert_eq!(taken, &vks[..32]);
    }

    #[test]
    fn modifiers_never_buffered() {
        let mut k = Kbd::new();
        k.chord(10_000);
        for (i, vk) in [LSHIFT, 0xA1, LCTRL, 0xA3, LALT, RALT, 0x10, 0x11, 0x12, LWIN, 0x5C].into_iter().enumerate() {
            let t = 10_100 + i as u32 * 20;
            assert_eq!(k.tap(vk, t), (None, None), "{vk:#x}");
        }
        assert!(k.take(1).is_empty());
        assert!(balanced(&k.seen));
    }

    /// The system's keys the launcher does not own pass inside a type-ahead window, down and up: Windows toggles the
    /// locks, takes the screenshot, changes the volume and switches the IME (a replay would drop them: they type
    /// nothing), and so do their chords on Tab and Esc (`system_chord`). A plain Tab or Esc, the menu key and the keys
    /// around them are still kept.
    #[test]
    fn system_keys_never_buffered() {
        let system = [0x14, 0x15, 0x16, 0x17, 0x18, 0x19, 0x1A, 0x1C, 0x1D, 0x1E, 0x1F, 0x2C, 0x5F, 0x90, 0x91];
        let mut k = Kbd::new();
        k.chord(10_000);
        k.tap(KEY_A, 10_050);
        for (i, vk) in system.into_iter().chain(0xA6..=0xB7).enumerate() {
            let t = 10_100 + i as u32 * 10;
            assert_eq!(k.tap(vk, t), (None, None), "{vk:#x}");
        }
        k.tap(KEY_C, 10_450);
        assert_eq!(k.take(1), [KEY_A, KEY_C], "the text around them is kept, in order");
        assert!(balanced(&k.seen));
        // The system's chords on Tab and Esc reach the task switcher, Start and Task Manager; a plain Tab or Esc is
        // the launcher's.
        let mut c = Kbd::new();
        c.chord(10_000);
        c.key(LALT, true, 10_040);
        assert_eq!(c.tap(TAB, 10_050), (None, None), "Alt+Tab");
        c.key(LSHIFT, true, 10_060);
        assert_eq!(c.tap(TAB, 10_070), (None, None), "Alt+Shift+Tab");
        c.key(LSHIFT, false, 10_080);
        assert_eq!(c.tap(ESC, 10_090), (None, None), "Alt+Esc");
        c.key(LALT, false, 10_100);
        c.key(LCTRL, true, 10_110);
        assert_eq!(c.tap(ESC, 10_120), (None, None), "Ctrl+Esc");
        c.key(LSHIFT, true, 10_130);
        assert_eq!(c.tap(ESC, 10_140), (None, None), "Ctrl+Shift+Esc");
        c.key(LSHIFT, false, 10_150);
        c.key(LCTRL, false, 10_160);
        assert_eq!(c.tap(TAB, 10_170), (Some(true), Some(true)), "a plain Tab is kept");
        assert_eq!(c.tap(ESC, 10_180), (Some(true), Some(true)), "a plain Esc is kept");
        assert_eq!(c.take(1), [TAB, ESC]);
        assert!(balanced(&c.seen));
        for vk in [TAB, 0x13, ESC, 0x2D, 0x5D, 0x5E, 0x8F, 0x92, 0xB8, 0xBA] {
            let mut s = st();
            s.ring = RingCtl::opened(1);
            s.post_time = 10_000;
            assert_eq!(resolve_launcher(&s, ev(vk, true, 10_100)), Some(Verdict::Buffer), "{vk:#x}");
        }
    }

    #[test]
    fn win_chord_during_typeahead_is_not_buffered() {
        let mut k = Kbd::new();
        k.chord(10_000);
        assert_eq!(k.key(LWIN, true, 10_090), None, "the Win branch takes it");
        k.s.win_down = true; // what the Win branch records
                             // E reaches the Win branch, whose other_key_down injects Win+E (not run here: it calls SendInput).
        assert_eq!(k.key(KEY_E, true, 10_100), None);
        assert_eq!(k.key(KEY_E, false, 10_150), None);
        assert_eq!(k.key(LWIN, false, 10_160), None);
        k.s.win_down = false;
        assert!(k.take(1).is_empty(), "never an \"e\" in the launcher");
    }

    #[test]
    fn up_of_an_unbuffered_down_passes() {
        let mut k = Kbd::new();
        assert_eq!(k.key(KEY_A, true, 9_900), None, "pressed before the chord");
        k.chord(10_000);
        assert_eq!(k.key(KEY_A, false, 10_050), None, "released inside the window: Windows saw its down");
        assert!(balanced(&k.seen));
    }

    #[test]
    fn repeats_of_a_held_key_stay_with_windows() {
        // An arrow held before the chord: its repeats inside the window pass, and so does its release.
        let mut k = Kbd::new();
        assert_eq!(k.key(0x28, true, 9_900), None);
        k.chord(10_000);
        for t in [10_050, 10_083, 10_116] {
            assert_eq!(k.key(0x28, true, t), None, "a repeat of a press Windows has");
        }
        assert_eq!(k.key(0x28, false, 10_130), None);
        assert!(k.take(1).is_empty());
        assert!(balanced(&k.seen), "{:?}", k.seen);
        // Space held, then Alt: Space's repeats are no chord.
        let mut k = Kbd::new();
        assert_eq!(k.key(SPACE, true, 10_000), None);
        k.key(LALT, true, 10_100);
        assert_eq!(k.key(SPACE, true, 10_133), None);
        assert_eq!(k.key(SPACE, false, 10_150), None);
        k.key(LALT, false, 10_160);
        assert!(k.posts.is_empty());
        assert!(balanced(&k.seen), "{:?}", k.seen);
        // A buffered key's own repeats stay buffered (Windows never got its down).
        let mut k = Kbd::new();
        k.chord(10_000);
        assert_eq!(k.key(KEY_A, true, 10_100), Some(true));
        assert_eq!(k.key(KEY_A, true, 10_133), Some(true));
        assert_eq!(k.key(KEY_A, false, 10_140), Some(true));
        assert_eq!(k.take(1), [KEY_A, KEY_A]);
        assert!(balanced(&k.seen));
    }

    #[test]
    fn stale_buffered_bit_cleared_by_a_passed_down() {
        let mut k = Kbd::new();
        k.s.buffered_down = [0, 1 << (KEY_A - 64), 0, 0];
        assert!(k.s.buffered(KEY_A));
        assert_eq!(k.key(KEY_A, true, 20_000), None, "no live window: its down passes");
        assert!(!k.s.buffered(KEY_A), "and clears the stale bit");
        assert_eq!(k.key(KEY_A, false, 20_050), None, "so its up passes too");
        assert!(balanced(&k.seen));
    }

    #[test]
    fn closed_push_of_a_buffered_key_clears_its_bit() {
        // 'a' is kept for the launcher; its repeat races the UI's end of the window: the hook saw the window live, its
        // push is refused (Closed) and the repeat passes, so Windows now holds 'a' down and must get its up.
        let mut k = Kbd::new();
        k.chord(10_000);
        assert_eq!(k.key(KEY_A, true, 10_100), Some(true), "stored");
        assert!(k.s.buffered(KEY_A));
        // The repeat, step by step: the hook's snapshot of the open window, the UI's end, the refused push.
        let repeat = ev(KEY_A, true, 10_133);
        k.s.ring = k.ring.ctl();
        let v = resolve_launcher(&k.s, repeat);
        assert_eq!(v, Some(Verdict::Buffer));
        assert_eq!(k.ring.end(1), Some(1), "the UI ends the window in between");
        let pushed = k.ring.try_push(k.s.ring.seq(), 0);
        assert_eq!(pushed, Push::Closed);
        assert_eq!(swallow_of(v, Some(pushed)), Some(false), "the repeat passes");
        k.s.after(v.as_ref(), repeat, Some(pushed));
        k.seen.push((KEY_A, true));
        assert!(!k.s.buffered(KEY_A), "Windows holds this down: no bit is left");
        assert_eq!(k.key(KEY_A, false, 10_150), None, "so its up passes");
        assert!(balanced(&k.seen), "{:?}", k.seen);
    }

    #[test]
    fn dropped_post_ends_the_window() {
        let mut k = Kbd::new();
        k.chord(10_000);
        assert_eq!(k.ring.ctl(), RingCtl::opened(1));
        assert_eq!(k.ring.end(1), Some(0), "the UI drops the post at 10,005 and ends the window");
        k.s.acked = 1;
        assert_eq!(k.tap(KEY_A, 10_010), (None, None), "passed, not in the ring");
        assert!(k.take(1).is_empty());
        // The hook's view raced the end: its push is refused and the key passes.
        let ring = Ring::new();
        ring.open(5);
        assert_eq!(ring.try_push(5, 1), Push::Stored);
        assert_eq!(ring.end(5), Some(1));
        assert_eq!(ring.try_push(5, 2), Push::Closed);
        assert_eq!(swallow_of(Some(Verdict::Buffer), Some(Push::Closed)), Some(false));
        assert_eq!(swallow_of(Some(Verdict::Buffer), Some(Push::Full)), Some(true));
        let mut s = st();
        s.after(Some(&Verdict::Buffer), ev(KEY_A, true, 10), Some(Push::Closed));
        assert!(!s.buffered(KEY_A), "a passed key leaves no bit");
        assert_eq!(ring.try_push(4, 3), Push::Closed, "another sequence");
    }

    #[test]
    fn ring_of_an_older_post_never_replayed() {
        let mut k = Kbd::new();
        k.chord(10_000);
        for (i, vk) in [0x53, 0x48, 0x55, 0x54, ENTER].into_iter().enumerate() {
            assert_eq!(k.tap(vk, 10_100 + i as u32 * 20), (Some(true), Some(true)));
        }
        assert_eq!(k.ring.end(1), Some(5), "post 1 dropped: the UI ends its window");
        k.s.acked = 1;
        assert_eq!(k.chord(10_300)[1], Some(true));
        assert_eq!(k.posts, [1, 2]);
        assert_eq!(k.tap(KEY_C, 10_350), (Some(true), Some(true)));
        assert_eq!(k.take(1), Vec::<u32>::new(), "\"shut\" and Enter are gone");
        assert_eq!(k.take(2), [KEY_C]);
    }

    #[test]
    fn closing_chord_opens_no_window() {
        let mut k = Kbd::new();
        k.s.shown = true;
        assert_eq!(k.chord(10_000)[1], Some(true));
        assert_eq!(k.posts, [1], "posted: the UI closes the launcher");
        assert_eq!(k.tap(KEY_A, 10_010), (None, None), "typed for the window behind");
        assert!(!k.ring.ctl().is_open());
    }

    #[test]
    fn take_after_end_is_empty() {
        let ring = Ring::new();
        ring.open(3);
        assert_eq!(ring.try_push(3, KeyRec { vk: 0x41, ..KeyRec::default() }.pack()), Push::Stored);
        assert_eq!(ring.end(3), Some(1));
        assert_eq!(ring.end(3), None);
        let mut out = [KeyRec::default(); RING_CAP];
        assert_eq!(ring.take(3, &mut out), 0);
        assert_eq!(Ring::new().take(0, &mut out), 0, "never opened");
    }

    #[test]
    fn take_is_once() {
        let ring = Ring::new();
        ring.open(9);
        for vk in [0x41u16, 0x42] {
            assert_eq!(ring.try_push(9, KeyRec { vk, ..KeyRec::default() }.pack()), Push::Stored);
        }
        let mut out = [KeyRec::default(); RING_CAP];
        assert_eq!(ring.take(8, &mut out), 0, "another sequence");
        assert_eq!(ring.take(9, &mut out), 2);
        assert_eq!((out[0].vk, out[1].vk), (0x41, 0x42));
        assert_eq!(ring.take(9, &mut out), 0);
        // Sequences wrap freely: OPEN, not a non-zero sequence, marks a window.
        ring.open(0);
        assert_eq!(ring.try_push(0, 7), Push::Stored);
        assert_eq!(ring.take(0, &mut out), 1);
    }

    #[test]
    fn ring_take_races_push_without_loss() {
        const ROUNDS: u32 = 10_000;
        const PUSHES: u32 = 40;
        let ring = Ring::new();
        let start = Barrier::new(2);
        let done = Barrier::new(2);
        let xorshift = |x: &mut u64| {
            *x ^= *x << 13;
            *x ^= *x >> 7;
            *x ^= *x << 17;
            *x
        };
        let (pushed, taken) = std::thread::scope(|sc| {
            let pusher = sc.spawn(|| {
                let mut rng = 0x5EED_u64;
                let mut all = Vec::new();
                for round in 0..ROUNDS {
                    ring.open(round);
                    start.wait();
                    let mut res = Vec::new();
                    for i in 1..=PUSHES {
                        for _ in 0..xorshift(&mut rng) % 4 {
                            std::hint::spin_loop();
                        }
                        res.push(ring.try_push(round, KeyRec { time: i, ..KeyRec::default() }.pack()));
                    }
                    all.push(res);
                    done.wait();
                }
                all
            });
            let taker = sc.spawn(|| {
                let mut rng = 0xC0FFEE_u64;
                let mut all = Vec::new();
                for round in 0..ROUNDS {
                    start.wait();
                    for _ in 0..xorshift(&mut rng) % 256 {
                        std::hint::spin_loop();
                    }
                    let mut out = [KeyRec::default(); RING_CAP];
                    let n = ring.take(round, &mut out);
                    all.push(out[..n].iter().map(|r| r.time).collect::<Vec<u32>>());
                    done.wait();
                }
                all
            });
            (pusher.join().unwrap(), taker.join().unwrap())
        });
        for (round, (res, got)) in pushed.iter().zip(&taken).enumerate() {
            let stored: Vec<u32> = (1..=PUSHES).zip(res).filter(|(_, p)| **p == Push::Stored).map(|(i, _)| i).collect();
            assert_eq!(*got, stored, "round {round}: every stored key is in the copy, no other");
            for (i, p) in (1..=PUSHES).zip(res) {
                match p {
                    Push::Stored => assert!(got.contains(&i)),
                    Push::Closed => assert!(!got.contains(&i), "round {round}: {i} passed and taken"),
                    Push::Full => assert!(i > RING_CAP as u32 && !got.contains(&i)),
                }
            }
        }
    }

    #[test]
    fn reset_mods_clears_tracked_alt() {
        let mut t = ModTrack::default();
        track_step(&mut t, 0, LALT, true, 100, |_| true);
        assert_eq!(t.mods(false), Mods::ALT);
        track_step(&mut t, 1, KEY_A, true, 200, |_| true);
        assert_eq!(t.mods(false), Mods::NONE, "RESET_MODS moved: tracked Alt cleared first");
        track_step(&mut t, 1, LALT, false, 300, |_| true);
        track_step(&mut t, 1, LSHIFT, true, 400, |_| true);
        assert_eq!(t.mods(false), Mods::SHIFT, "the same reset counts once");
        assert_eq!(t.mods(true), Mods::SHIFT.with(Mods::WIN));
        // With the chord: a lost Shift release cleared by the reset can only make Alt+Shift+Space pass.
        let mut k = Kbd::new();
        k.s.chord = Some(ChordSpec { vk: 0x20, mods: ALT_SHIFT });
        k.key(LSHIFT, true, 100);
        track_step(&mut k.track, 1, LALT, true, 200, |_| true);
        k.s.mods = k.track.mods(false);
        assert_eq!(resolve_launcher(&k.s, ev(SPACE, true, 210)), None);
    }

    #[test]
    fn chord_change_forgets_tracked_modifiers() {
        let (slot, reset) = (AtomicU32::new(0), AtomicU32::new(0));
        let mut t = ModTrack::default();
        swap_chord(&slot, &reset, Some(ALT_SPACE));
        track_step(&mut t, reset.load(Relaxed), LALT, true, 100, |_| true);
        assert_eq!(t.mods(false), Mods::ALT);
        // The chord goes off: Alt's release at 200 is not tracked. It comes back within MOD_STALE_MS.
        swap_chord(&slot, &reset, None);
        swap_chord(&slot, &reset, Some(ALT_SPACE));
        track_step(&mut t, reset.load(Relaxed), SPACE, true, 300, |_| true);
        let s = ChordState { mods: t.mods(false), ..st() };
        assert_eq!(resolve_launcher(&s, ev(SPACE, true, 300)), None, "a bare Space never matches");
        track_step(&mut t, reset.load(Relaxed), SPACE, false, 310, |_| true);
        // The same chord again forgets nothing: Alt really held still matches.
        track_step(&mut t, reset.load(Relaxed), LALT, true, 400, |_| true);
        let before = reset.load(Relaxed);
        swap_chord(&slot, &reset, Some(ALT_SPACE));
        assert_eq!(reset.load(Relaxed), before);
        assert_eq!(unpack_chord(slot.load(Relaxed)), Some(ALT_SPACE));
        track_step(&mut t, reset.load(Relaxed), SPACE, true, 410, |_| true);
        let s = ChordState { mods: t.mods(false), ..st() };
        assert_eq!(resolve_launcher(&s, ev(SPACE, true, 410)), Some(Verdict::SwallowPost(1)));
    }

    #[test]
    fn stale_modifier_repaired_after_1500_ms() {
        let mut t = ModTrack::default();
        t.on_key(LALT, true, 1_000);
        t.on_key(LSHIFT, true, 1_000);
        t.repair(2_500, |_| false);
        assert_eq!(t.mods(false), ALT_SHIFT, "1500 ms: not stale yet");
        t.repair(2_501, |_| true);
        assert_eq!(t.mods(false), ALT_SHIFT, "stale but still down");
        t.repair(4_002, |vk| vk == LSHIFT);
        assert_eq!(t.mods(false), Mods::SHIFT, "Alt's release was lost: cleared; Shift really held");
        assert!(!t.alt_down());
        // Right-side keys are tracked on their own.
        let mut t = ModTrack::default();
        t.on_key(RALT, true, 0);
        t.on_key(LALT, true, 0);
        t.on_key(LALT, false, 5);
        assert!(t.alt_down(), "right Alt still down");
    }

    #[test]
    fn gesture_time_only_for_physical_ctrl_shift_enter() {
        let slot = AtomicU64::new(0);
        let cs = CTRL_SHIFT_;
        note_gesture(&slot, 0, 0, ENTER, true, cs, 5);
        assert_eq!(slot.swap(0, Relaxed), pack_gesture_time(5));
        let none = [
            (0, 0, ENTER, true, cs.with(Mods::ALT), "an Alt-held Enter"),
            (0, 0, ENTER, true, Mods::CTRL, "a lone Ctrl+Enter"),
            (0, 0, ENTER, true, cs.with(Mods::WIN), "Win held"),
            (LLKHF_INJECTED.0, 0, ENTER, true, cs, "an injected Ctrl+Shift+Enter"),
            (0, INJECT_TAG, ENTER, true, cs, "our own injection"),
            (0, 0, ENTER, false, cs, "the release"),
            (0, 0, KEY_E, true, cs, "Ctrl+Shift+E"),
        ];
        for (flags, extra, vk, down, mods, what) in none {
            note_gesture(&slot, flags, extra, vk, down, mods, 7);
            assert_eq!(slot.load(Relaxed), 0, "{what}");
        }
    }

    #[test]
    fn gesture_time_round_trips_into_from_key() {
        let cse = KeyEvent { vk: 0x0D, mods: CTRL_SHIFT_, repeat: false, sys: false };
        for t in [0, 1, 2, 1_000_000, 1_000_001, u32::MAX - 1, u32::MAX] {
            let hook = unpack_gesture_time(pack_gesture_time(t));
            assert_eq!(hook, Some(t));
            assert!(Gesture::from_key(&cse, hook, t, 0, INJECT_TAG, t.wrapping_add(5)).is_some(), "{t}");
            assert!(
                Gesture::from_key(&cse, hook, t.wrapping_add(1), 0, INJECT_TAG, t.wrapping_add(5)).is_none(),
                "{t}"
            );
        }
        assert_eq!(unpack_gesture_time(0), None, "nothing stored");
        // Once through the static, as handle_key stores it (no other test touches GESTURE_TIME).
        let t = 1_000_001;
        note_gesture(&GESTURE_TIME, 0, 0, ENTER, true, CTRL_SHIFT_, t);
        let hook = take_gesture_time();
        assert_eq!(hook, Some(t));
        assert!(Gesture::from_key(&cse, hook, t, 0, INJECT_TAG, t + 5).is_some());
        assert_eq!(take_gesture_time(), None, "consumed");
    }

    #[test]
    fn stale_chord_never_opens() {
        use PostAction::*;
        // seq, sent at, now, newest, last failed open, open.
        assert_eq!(post_verdict(1, 1_000, 1_300, 1, 0, false), Open);
        assert_eq!(post_verdict(1, 1_000, 1_301, 1, 0, false), Drop, "301 ms late");
        assert_eq!(post_verdict(1, 1_000, 1_301, 1, 0, true), Close, "closing is always safe");
        assert_eq!(post_verdict(1, 1_000, 1_010, 2, 0, false), Drop, "superseded");
        assert_eq!(post_verdict(1, 1_000, 1_010, 2, 0, true), Close);
        assert_eq!(post_verdict(3, 1_000, 1_010, 3, 3, false), Drop, "failed open");
        assert_eq!(post_verdict(2, 1_000, 1_010, 3, 3, false), Drop);
        assert_eq!(post_verdict(4, 1_000, 1_010, 4, 3, false), Open);
        assert_eq!(post_verdict(4, 1_000, 1_010, 4, 3, true), Close);
        // Wrapping clocks and sequences.
        assert_eq!(post_verdict(0, u32::MAX - 100, 50, 0, u32::MAX, false), Open);
        assert_eq!(post_verdict(u32::MAX, 0, 10, 0, 0, false), Drop, "u32::MAX is older than 0");
        // Three queued posts open at most once.
        let mut open = false;
        let mut opened = 0;
        for seq in 1..=3 {
            match post_verdict(seq, 1_000, 1_100, 3, 0, open) {
                Open => {
                    open = true;
                    opened += 1;
                }
                Close => open = false,
                Drop => {}
            }
        }
        assert_eq!((opened, open), (1, true));
        // All stale: none opens.
        assert!((1..=3).all(|seq| post_verdict(seq, 1_000, 2_000, 3, 0, false) == Drop));
    }

    #[test]
    fn reinject_plan_pairs() {
        let p = reinject_plan(true);
        assert_eq!(p.keys(), [(0x20, false), (0x20, true)], "Alt still down: Space down, Space up");
        let p = reinject_plan(false);
        assert_eq!(p.keys(), [(0x12, false), (0x20, false), (0x20, true), (0x12, true)]);
    }

    #[test]
    fn swallowed_down_ui_says_pass_then_up() {
        // The popup cannot open: the UI re-injects the plan (tagged, so the hook passes it), then the real up arrives.
        for alt_released_first in [false, true] {
            let mut k = Kbd::new();
            k.key(LALT, true, 9_990);
            assert_eq!(k.key(SPACE, true, 10_000), Some(true));
            if alt_released_first {
                k.key(LALT, false, 10_005);
            }
            assert_eq!(post_verdict(1, 10_000, 10_010, 1, 0, false), PostAction::Open);
            let plan = reinject_plan(k.track.alt_down());
            k.seen.extend(plan.keys().iter().map(|&(vk, up)| (u32::from(vk), !up))); // the SendInput recorder
            k.ring.end(1);
            assert_eq!(k.key(SPACE, false, 10_050), Some(true), "the real up stays swallowed");
            if !alt_released_first {
                k.key(LALT, false, 10_060);
            }
            assert!(balanced(&k.seen), "{:?}", k.seen);
            assert_eq!(k.seen.iter().filter(|e| e.0 == SPACE).count(), 2, "one Space down, one Space up");
        }
    }
    // ---- the dock's owned chords (dock-keys Z2)

    const ALL_ROWS: DockChords = DockChords(0x7F);
    const CA: Mods = Mods(Mods::CTRL.0 | Mods::ALT.0);
    const SA: Mods = Mods(Mods::SHIFT.0 | Mods::ALT.0);

    #[test]
    fn chord_table_rows() {
        let r = |vk, m| resolve_chord(ALL_ROWS, vk, m);
        assert_eq!(r(0x31, Mods::NONE), Some(ChordAction::App(0)));
        assert_eq!(r(0x39, Mods::NONE), Some(ChordAction::App(8)));
        assert_eq!(r(0x30, Mods::NONE), Some(ChordAction::App(9)));
        assert_eq!(r(0x33, Mods::SHIFT), Some(ChordAction::NewInstance(2)));
        assert_eq!(r(0x33, Mods::ALT), Some(ChordAction::AppMenu(2)));
        assert_eq!(r(0x54, Mods::NONE), Some(ChordAction::FocusFirst));
        assert_eq!(r(0x54, Mods::SHIFT), Some(ChordAction::FocusLast));
        assert_eq!(r(0x42, Mods::NONE), Some(ChordAction::FocusTray));
        assert_eq!(r(0x33, Mods::CTRL), None);
        assert_eq!(r(0x33, CA), None, "AltGr is Ctrl+Alt");
        assert_eq!(r(0x33, SA), None);
        assert_eq!(r(0x61, Mods::NONE), None, "numpad 1");
        assert_eq!(r(0x45, Mods::NONE), None);
        assert_eq!(r(0x42, Mods::SHIFT), None);
        assert_eq!(r(0x54, Mods::ALT), None);
        assert_eq!(r(0x2F, Mods::NONE), None, "just below the digits");
        assert_eq!(r(0x3A, Mods::NONE), None, "just above the digits");
    }

    #[test]
    fn chord_rows_follow_ownership() {
        let no_digits = DockChords(ALL_ROWS.0 & !DockChords::DIGITS.0);
        assert_eq!(resolve_chord(no_digits, 0x31, Mods::NONE), None);
        assert_eq!(resolve_chord(no_digits, 0x31, Mods::SHIFT), Some(ChordAction::NewInstance(0)));
        let rows = [
            (DockChords::DIGITS, 0x31, Mods::NONE),
            (DockChords::SHIFT_DIGITS, 0x31, Mods::SHIFT),
            (DockChords::ALT_DIGITS, 0x31, Mods::ALT),
            (DockChords::WIN_T, 0x54, Mods::NONE),
            (DockChords::WIN_SHIFT_T, 0x54, Mods::SHIFT),
            (DockChords::WIN_B, 0x42, Mods::NONE),
        ];
        for (row, vk, m) in rows {
            assert!(resolve_chord(row, vk, m).is_some(), "row {row:?} alone owns its chord");
            assert_eq!(resolve_chord(DockChords(ALL_ROWS.0 & !row.0), vk, m), None, "row {row:?} off");
            assert_eq!(resolve_chord(DockChords::NONE, vk, m), None);
        }
        assert!(!DockChords::NONE.has(DockChords::NONE), "an empty row is never owned");
        assert!(ALL_ROWS.has(DockChords::PEEK));
        assert_eq!(DockChords::NONE.with(DockChords::WIN_B).with(DockChords::PEEK).0, 0x60);
    }

    #[test]
    fn chord_wparam_round_trips() {
        let mut actions =
            vec![ChordAction::FocusFirst, ChordAction::FocusLast, ChordAction::FocusTray, ChordAction::Sync];
        for n in 0..=9u8 {
            actions.extend([ChordAction::App(n), ChordAction::NewInstance(n), ChordAction::AppMenu(n)]);
        }
        for a in actions {
            for seq in [0u16, 1, 0x7FFF, 0xFFFF] {
                assert_eq!(decode_chord(encode_chord(a, seq)), Some((a, seq)), "{a:?} seq {seq}");
            }
        }
        assert_eq!(decode_chord(0), None, "kind 0");
        assert_eq!(decode_chord(8), None, "kind 8");
        assert_eq!(decode_chord(0xFF | 0x1234_0000), None);
        assert_eq!(decode_chord(1 | (10 << 8)), None, "slot above 9");
    }

    #[test]
    fn takeover_wparam_round_trips() {
        for t in [TakeoverTarget::Tasks, TakeoverTarget::Files] {
            for seq in [0u16, 1, 0x7FFF, 0xFFFF] {
                assert_eq!(decode_takeover(encode_takeover(t, seq)), Some((t, seq)), "{t:?} seq {seq}");
            }
        }
        assert_eq!(decode_takeover(0), None, "target 0");
        assert_eq!(decode_takeover(3), None, "target 3");
        assert_eq!(decode_takeover(0xFF | 0x1234_0000), None);
    }

    #[test]
    fn dock_chords_split() {
        assert_eq!(DockChords(0x01FF).rows(), DockChords(0x00FF));
        assert_eq!(DockChords(0x01FF).takeovers(), DockChords(0x0100));
        assert_eq!(DockChords::CTRL_SHIFT_ESC.0, 1 << 8);
        assert_eq!(DockChords::WIN_E.0, 1 << 9);
        assert_eq!(DockChords::CTRL_SHIFT_ESC.0 & ALL_ROWS.0, 0, "no DK-0 row shares the bit");
        assert_eq!(DockChords::WIN_E.0 & ALL_ROWS.0, 0, "no DK-0 row shares the bit");
        assert_eq!(DockChords::CTRL_SHIFT_ESC.rows(), DockChords::NONE);
        assert_eq!(DockChords::CTRL_SHIFT_ESC.with(DockChords::WIN_E).takeovers().0, 0x0300);
    }

    #[test]
    fn gate_bits() {
        let g = TakeoverGate::NONE;
        assert!(!g.has(TakeoverGate::TASKS_READY) && !g.has(TakeoverGate::TASKS_IN_FRONT));
        let g = g.with(TakeoverGate::TASKS_READY, true);
        assert!(g.has(TakeoverGate::TASKS_READY) && !g.has(TakeoverGate::TASKS_IN_FRONT));
        let g = g.with(TakeoverGate::TASKS_IN_FRONT, true);
        assert!(g.has(TakeoverGate::TASKS_READY) && g.has(TakeoverGate::TASKS_IN_FRONT));
        let g = g.with(TakeoverGate::TASKS_READY, false);
        assert!(!g.has(TakeoverGate::TASKS_READY) && g.has(TakeoverGate::TASKS_IN_FRONT));
        assert_eq!(g.with(TakeoverGate::TASKS_IN_FRONT, false), TakeoverGate::NONE);
    }

    #[test]
    fn shell_busy_round_trip() {
        set_shell_busy(7);
        assert_eq!(shell_busy(), 7);
        set_shell_busy(0);
        assert_eq!(shell_busy(), 0);
    }

    // ---- the takeover step (dock-suite H1)

    const READY: TakeoverGate = TakeoverGate(TakeoverGate::TASKS_READY);
    use TakeDecision::{Candidate, NotOurs, SwallowSilent};

    /// A Ctrl+Shift+Esc down at `time` with every condition of the takeover met.
    fn cse(time: u32) -> TakeIn {
        TakeIn {
            vk: ESC,
            down: true,
            time,
            mods: CTRL_SHIFT_,
            chords: DockChords::CTRL_SHIFT_ESC,
            gate: READY,
            breaker_until: 0,
            shell_busy: 0,
            shell_ok: true,
            held: false,
        }
    }

    /// An Esc event at `time` with no modifier, nothing owned and no gate: what the Esc bit alone decides.
    fn esc(down: bool, time: u32) -> TakeIn {
        TakeIn { down, mods: Mods::NONE, chords: DockChords::NONE, gate: TakeoverGate::NONE, ..cse(time) }
    }

    /// The state after the Ctrl+Shift+Esc of `time` was posted with sequence 1 and committed.
    fn taken(time: u32) -> TakeState {
        let mut s = TakeState::default();
        assert_eq!(take_decide(&mut s, cse(time)), Candidate(1));
        take_commit(&mut s, 1, time);
        s
    }

    /// Rule 2: every condition false on its own gives NotOurs and changes nothing; all true give the next sequence.
    #[test]
    fn take_table() {
        let t = 10_000;
        let mut s = TakeState::default();
        assert_eq!(take_decide(&mut s, cse(t)), Candidate(1), "every condition met");
        assert_eq!(s, TakeState::default(), "the decision alone commits nothing");
        let none = TakeState::default();
        let stuck = TakeState { sent: 1, acked: 0, oldest: t - 2_000, ..none };
        let rows: [(&str, TakeIn, TakeState); 15] = [
            ("not Esc", TakeIn { vk: KEY_A, ..cse(t) }, none),
            ("an Esc up", TakeIn { down: false, ..cse(t) }, none),
            ("Ctrl alone", TakeIn { mods: Mods::CTRL, ..cse(t) }, none),
            ("Shift alone", TakeIn { mods: Mods::SHIFT, ..cse(t) }, none),
            ("no modifier", TakeIn { mods: Mods::NONE, ..cse(t) }, none),
            ("the takeover not owned", TakeIn { chords: ALL_ROWS, ..cse(t) }, none),
            ("only Win+E owned", TakeIn { chords: DockChords::WIN_E, ..cse(t) }, none),
            ("Tasks not ready", TakeIn { gate: TakeoverGate::NONE, ..cse(t) }, none),
            ("Tasks in front", TakeIn { gate: READY.with(TakeoverGate::TASKS_IN_FRONT, true), ..cse(t) }, none),
            ("in front, not ready", TakeIn { gate: TakeoverGate(TakeoverGate::TASKS_IN_FRONT), ..cse(t) }, none),
            ("paused", TakeIn { breaker_until: (t + 600_000) | 1, ..cse(t) }, none),
            ("the shell thread busy", TakeIn { shell_busy: t | 1, ..cse(t) }, none),
            ("no shell", TakeIn { shell_ok: false, ..cse(t) }, none),
            ("a repeat of an Esc that reached Windows", TakeIn { held: true, ..cse(t) }, none),
            ("a post unacknowledged for 2 s", cse(t), stuck),
        ];
        for (what, ev, before) in rows {
            let mut s = before;
            assert_eq!(take_decide(&mut s, ev), NotOurs, "{what}");
            assert_eq!(s, before, "{what}: no state change");
        }
        // The sequence follows the last post, wrapping.
        let mut s = TakeState { sent: 0xFFFF, acked: 0xFFFF, ..none };
        assert_eq!(take_decide(&mut s, cse(t)), Candidate(0));
    }

    /// Rule 1: after the swallowed press, its auto-repeat (Ctrl and Shift still held or not) and its up are swallowed
    /// with nothing posted; the up clears the bit, and the next Esc is evaluated fresh.
    #[test]
    fn repeats_and_up_are_swallowed() {
        let mut s = taken(10_000);
        assert!(s.esc_bit && s.esc_last == 10_000);
        for t in [10_033, 10_066, 10_099] {
            assert_eq!(take_decide(&mut s, cse(t)), SwallowSilent, "the repeat at {t}");
            assert_eq!(s.esc_last, t);
        }
        assert_eq!(take_decide(&mut s, esc(true, 10_132)), SwallowSilent, "a repeat after Ctrl and Shift went up");
        assert_eq!(take_decide(&mut s, TakeIn { vk: LCTRL, ..esc(false, 10_140) }), NotOurs, "another key");
        assert!(s.esc_bit, "another key leaves the bit");
        assert_eq!(take_decide(&mut s, esc(false, 10_150)), SwallowSilent, "the up");
        assert!(!s.esc_bit, "the up clears the bit");
        assert_eq!(take_decide(&mut s, esc(false, 10_160)), NotOurs, "a second up passes");
        assert_eq!(take_decide(&mut s, esc(true, 10_200)), NotOurs, "a plain Esc next passes");
        assert_eq!(take_decide(&mut s, esc(false, 10_250)), NotOurs, "and so does its up");
        assert_eq!(take_decide(&mut s, cse(10_300)), Candidate(2), "a new Ctrl+Shift+Esc is a candidate");
        assert_eq!((s.sent, s.acked, s.oldest), (1, 0, 10_000), "only a commit moves the sequence");
    }

    /// The Esc bit ages out: a down more than REPEAT_WINDOW_MS after the last Esc event is a fresh press (its up lost).
    #[test]
    fn bit_expires() {
        let mut s = taken(10_000);
        assert_eq!(take_decide(&mut s, esc(true, 10_000 + REPEAT_WINDOW_MS)), SwallowSilent, "1500 ms: a repeat");
        let mut s = taken(10_000);
        assert_eq!(take_decide(&mut s, esc(true, 11_501)), NotOurs, "1501 ms: a fresh plain Esc passes");
        assert!(!s.esc_bit);
        assert_eq!(take_decide(&mut s, esc(false, 11_550)), NotOurs, "its up passes as its down did");
        let mut s = TakeState { acked: 1, ..taken(10_000) };
        assert_eq!(take_decide(&mut s, cse(11_501)), Candidate(2), "1501 ms: a fresh Ctrl+Shift+Esc (answered)");
        assert!(!s.esc_bit, "cleared until the commit");
        let mut s = taken(u32::MAX - 500);
        assert_eq!(take_decide(&mut s, esc(true, 999)), SwallowSilent, "a repeat across the tick wrap");
        assert_eq!(take_decide(&mut s, esc(true, 2_500)), NotOurs, "expired across the tick wrap");
    }

    /// Alt or Win with the chord, or one modifier short: Windows' own chords, never the takeover's.
    #[test]
    fn alt_or_win_is_not_ours() {
        let cs = CTRL_SHIFT_;
        for (what, mods) in [
            ("Ctrl+Shift+Alt+Esc", cs.with(Mods::ALT)),
            ("Ctrl+Shift+Esc with Win held", cs.with(Mods::WIN)),
            ("Win+Esc", Mods::WIN),
            ("Ctrl+Esc", Mods::CTRL),
            ("Shift+Esc", Mods::SHIFT),
            ("Alt+Esc", Mods::ALT),
            ("Ctrl+Alt+Esc", Mods::CTRL.with(Mods::ALT)),
        ] {
            let mut s = TakeState::default();
            assert_eq!(take_decide(&mut s, TakeIn { mods, ..cse(10_000) }), NotOurs, "{what}");
        }
        // Either side: the tracker folds right Ctrl and left Shift into Ctrl+Shift; Win comes from the Win branch.
        let mut t = ModTrack::default();
        track_step(&mut t, 0, 0xA3, true, 10_000, |_| false);
        track_step(&mut t, 0, LSHIFT, true, 10_010, |_| false);
        let mut s = TakeState::default();
        assert_eq!(take_decide(&mut s, TakeIn { mods: t.mods(true), ..cse(10_020) }), NotOurs, "Win held");
        assert_eq!(take_decide(&mut s, TakeIn { mods: t.mods(false), ..cse(10_020) }), Candidate(1), "right Ctrl");
    }

    /// The breaker: a pause 10 minutes ahead passes every press until the tick count reaches it, across a wrap too.
    #[test]
    fn breaker_and_wrap() {
        let pause = |at: u32| at.wrapping_add(600_000) | 1;
        let until = pause(10_000);
        for t in [10_000, 300_000, until - 1] {
            let mut s = TakeState::default();
            assert_eq!(take_decide(&mut s, TakeIn { breaker_until: until, ..cse(t) }), NotOurs, "paused at {t}");
        }
        for t in [until, until + 1, until + 3_600_000] {
            let mut s = TakeState::default();
            assert_eq!(take_decide(&mut s, TakeIn { breaker_until: until, ..cse(t) }), Candidate(1), "over at {t}");
        }
        let trip = u32::MAX - 100;
        let until = pause(trip);
        assert!(until < trip, "the pause ends after the wrap");
        for t in [trip, u32::MAX, 0, until - 1] {
            let mut s = TakeState::default();
            assert_eq!(take_decide(&mut s, TakeIn { breaker_until: until, ..cse(t) }), NotOurs, "paused at {t}");
        }
        for t in [until, until + 1_000] {
            let mut s = TakeState::default();
            assert_eq!(take_decide(&mut s, TakeIn { breaker_until: until, ..cse(t) }), Candidate(1), "over at {t}");
        }
    }

    /// Fail-open: a post unacknowledged for more than 1500 ms since the oldest pending one passes the chord to
    /// Windows; an acknowledgement closes it. The window starts at the oldest pending post.
    #[test]
    fn takeover_fail_open_window() {
        let t = 10_000;
        assert!(!takeover_fail_open(1, 0, t, t + 1_499), "pending 1499 ms");
        assert!(!takeover_fail_open(1, 0, t, t + TAKEOVER_ACK_TIMEOUT_MS), "pending 1500 ms");
        assert!(takeover_fail_open(1, 0, t, t + 1_501), "pending 1501 ms");
        assert!(!takeover_fail_open(1, 1, t, t + 60_000), "acknowledged");
        assert!(takeover_fail_open(0, 0xFFFF, t, t + 1_501), "the sequence wrapped");
        assert!(!takeover_fail_open(0xFFFF, 0, t, t + 1_501), "an acknowledgement ahead is no pending post");
        assert!(takeover_fail_open(1, 0, u32::MAX - 500, 1_001), "across the tick wrap");
        assert!(!takeover_fail_open(1, 0, u32::MAX - 500, 999), "inside the window across the wrap");
        let mut s = taken(t);
        assert_eq!(take_decide(&mut s, esc(false, t + 50)), SwallowSilent);
        assert_eq!(take_decide(&mut s, cse(t + 1_000)), Candidate(2), "pending 1000 ms");
        take_commit(&mut s, 2, t + 1_000);
        assert_eq!(s.oldest, t, "a second post keeps the first one's window");
        assert_eq!(take_decide(&mut s, esc(false, t + 1_050)), SwallowSilent);
        assert_eq!(take_decide(&mut s, cse(t + 1_501)), NotOurs, "failed open");
        s.acked = 2;
        assert_eq!(take_decide(&mut s, cse(t + 1_600)), Candidate(3), "the shell thread answered");
        take_commit(&mut s, 3, t + 1_600);
        assert_eq!(s.oldest, t + 1_600, "the next window starts at the next post");
    }

    /// The shell thread is not waiting in GetMessageW (any busy stamp): the press passes to Windows.
    #[test]
    fn busy_shell_is_not_ours() {
        for busy in [1, 7, 10_000 | 1, 0x8000_0001, u32::MAX] {
            let mut s = TakeState::default();
            assert_eq!(take_decide(&mut s, TakeIn { shell_busy: busy, ..cse(10_000) }), NotOurs, "busy {busy:#x}");
        }
    }

    // ---- the takeover wired into the callback (dock-suite H2)

    /// A key event as the callback receives it, for `win_in_of`.
    fn kb_at(vk: u32, time: u32) -> KBDLLHOOKSTRUCT {
        KBDLLHOOKSTRUCT { vkCode: vk, time, ..Default::default() }
    }

    /// The Win branch takes the rows of the owned chords only. With Ctrl+Shift+Esc owned alone, the event `win_key`
    /// builds (`win_in_of`) is release 1.5.2's and every legacy sequence gives today's answers; with rows too, the rows
    /// reach `win_step` and the takeover bit does not. The tracker runs for a takeover alone (it reads Ctrl and Shift).
    #[test]
    fn win_branch_sees_only_the_rows() {
        let alone = |vk: u32, down: bool, time: u32| {
            win_in_of(&kb_at(vk, time), down, Mods::NONE, DockChords::CTRL_SHIFT_ESC, false, true, || {
                panic!("no row owned: the fail-open is never read")
            })
        };
        for (what, steps) in &legacy_table() {
            for &(vk, down, t, _) in steps {
                assert_eq!(alone(vk, down, t), legacy(vk, down, t), "{what}: the event");
            }
            run_steps(what, steps, alone);
        }
        let both = DockChords::CTRL_SHIFT_ESC.with(ALL_ROWS);
        let ev = win_in_of(&kb_at(KEY_3, 10_100), true, CTRL_SHIFT_.with(Mods::WIN), both, true, true, || true);
        assert_eq!(ev, WinIn { mods: CTRL_SHIFT_, fullscreen: true, fail_open: true, ..owned(KEY_3, true, 10_100) });
        let ev = win_in_of(&kb_at(KEY_3, 10_100), true, Mods::NONE, both, false, false, || false);
        assert_eq!(ev, WinIn { ui_ready: false, ..owned(KEY_3, true, 10_100) });
        assert!(tracks(false, DockChords::CTRL_SHIFT_ESC), "the takeover reads the tracked Ctrl and Shift");
        let src = include_str!("hook.rs");
        let order = |b: &str, x: &str, y: &str| matches!((b.find(x), b.find(y)), (Some(i), Some(j)) if i < j);
        let key = source_block(src, "fn win_key(");
        let wires = [
            ("win_key: the rows taken first", order(&key, "let chords = chords.rows();", "let ev = win_in_of(")),
            (
                "win_key: the event of the rows",
                key.contains("let ev = win_in_of(kb, down, mods, chords, DOCK_FS.load(Acquire),"),
            ),
            ("win_key: the step on that event", order(&key, "let ev = win_in_of(", "let out = win_step(&mut s, ev);")),
        ];
        let missing: Vec<&str> = wires.iter().filter(|(_, ok)| !ok).map(|(what, _)| *what).collect();
        assert!(missing.is_empty(), "unwired: {missing:?}");
    }

    /// An Esc the takeover step does not take goes on to the Win branch as today: Win+Esc with the takeover owned and
    /// ready is not the takeover's, and the Win branch answers it event for event as release 1.5.2's; a plain Esc passes.
    #[test]
    fn not_ours_falls_through() {
        let chords = DockChords::CTRL_SHIFT_ESC;
        let steps: [(u32, bool, u32, Mods); 4] = [
            (LWIN, true, 10_000, Mods::NONE),
            (ESC, true, 10_100, Mods::WIN),
            (ESC, false, 10_150, Mods::WIN),
            (LWIN, false, 10_200, Mods::WIN),
        ];
        let (mut t, mut s, mut old) = (TakeState::default(), WinState::default(), WinState::default());
        for (vk, down, time, mods) in steps {
            if vk == ESC {
                let ev = TakeIn { vk, down, time, mods: mods.with(CTRL_SHIFT_), ..cse(time) };
                assert_eq!(take_decide(&mut t, ev), NotOurs, "Ctrl+Shift+Esc with Win held at {time}");
                assert_eq!(take_decide(&mut t, TakeIn { mods, ..ev }), NotOurs, "Win+Esc at {time}");
            }
            let out = win_step(&mut s, win_in_of(&kb_at(vk, time), down, mods, chords, false, true, || false));
            let want = win_step(&mut old, legacy(vk, down, time));
            assert_eq!(out, want, "vk {vk:#x} down {down}: as release 1.5.2");
            if vk == ESC && down {
                assert_eq!((out.swallow, out.act), (true, WinAct::Inject), "Win+Esc reaches Windows as the pair");
            }
        }
        assert_eq!(t, TakeState::default(), "the takeover step kept nothing");
        let mut s = WinState::default();
        assert_eq!(take_decide(&mut t, TakeIn { mods: Mods::NONE, ..cse(20_000) }), NotOurs, "a plain Esc");
        let out = win_step(&mut s, win_in_of(&kb_at(ESC, 20_000), true, Mods::NONE, chords, false, true, || false));
        assert_eq!(out, WinOut::PASS, "and the Win branch passes it");
    }

    /// The callback's takeover block, pinned on the source (a callback cannot run here): after the launcher's block and
    /// before the Win branch; a NotOurs Esc goes on to the Win branch; a candidate is posted first, a failed post passes
    /// the key, a posted one is committed, then the menu-mask key goes out under Ctrl and Shift and the key is
    /// swallowed; the post publishes its time before the state publishes its sequence; the shell is read only for a
    /// press that can be the chord; the state's load and store cover the hook's fields and leave the shell thread's.
    #[test]
    fn mask_after_swallow() {
        let src = include_str!("hook.rs");
        let order = |b: &str, x: &str, y: &str| matches!((b.find(x), b.find(y)), (Some(i), Some(j)) if i < j);
        let key = source_block(src, "fn handle_key(");
        let block = source_block(
            &key,
            "if vk == VK_ESCAPE_ && (chords.takeovers() != DockChords::NONE || TAKE_ESC_BIT.load(Relaxed))",
        );
        let arm = source_block(&block, "TakeDecision::Candidate(seq) =>");
        let failed = "if !post_takeover(seq, kb.time) { store_take_state(&t); return false; }";
        let commit = "take_commit(&mut t, seq, kb.time); store_take_state(&t);";
        let post = source_block(src, "fn post_takeover(seq: u16, time: u32)");
        let take_in = source_block(src, "fn take_in(");
        let store = source_block(src, "fn store_take_state(");
        let load = source_block(src, "fn load_take_state()");
        let wires = [
            (
                "handle_key: the launcher's block first",
                order(&key, "launcher_step(kb, down, mods)", "if vk == VK_ESCAPE_ &&"),
            ),
            (
                "handle_key: the takeover before the Win branch",
                order(&key, "take_decide(", "win_key(kb, down, mods, chords)"),
            ),
            (
                "handle_key: the step on the statics",
                block.contains(
                    "let mut t = load_take_state(); match take_decide(&mut t, take_in(kb, down, mods, chords)) {",
                ),
            ),
            ("handle_key: a NotOurs Esc goes on", block.contains("TakeDecision::NotOurs => store_take_state(&t),")),
            (
                "handle_key: a repeat or an up swallowed",
                block.contains("TakeDecision::SwallowSilent => { store_take_state(&t); return true; }"),
            ),
            ("handle_key: a failed post passes the key", arm.starts_with(&format!("{{ {failed}"))),
            ("handle_key: the commit after the post", order(&arm, failed, commit)),
            ("handle_key: the mask after the commit", order(&arm, commit, "send_mask(); return true; }")),
            (
                "post_takeover: to the shell thread",
                post.contains("PostThreadMessageW(tid, WM_SHELL_TAKEOVER, wparam, LPARAM(time as isize))"),
            ),
            (
                "post_takeover: Tasks and the sequence",
                post.contains("WPARAM(encode_takeover(TakeoverTarget::Tasks, seq))"),
            ),
            (
                "post_takeover: the time on success",
                post.contains("if posted { TAKEOVER_SENT_TIME.store(time, Release); }"),
            ),
            (
                "store_take_state: the sequence last",
                order(&store, "TAKEOVER_OLDEST.store(s.oldest, Relaxed);", "TAKEOVER_SEQ.store(s.sent, Release);"),
            ),
            ("store_take_state: the acknowledgement left alone", !store.contains("TAKEOVER_ACKED")),
            ("load_take_state: the acknowledgement read", load.contains("acked: TAKEOVER_ACKED.load(Acquire),")),
            ("take_in: the shell only for a possible chord", take_in.contains("shell_ok: candidate && shell_ready(),")),
            ("take_in: the key state only for a possible chord", take_in.contains("held: candidate && esc_held(),")),
            (
                "take_in: the chord's press",
                take_in
                    .contains("let candidate = down && mods == CTRL_SHIFT && chords.has(DockChords::CTRL_SHIFT_ESC);"),
            ),
        ];
        let missing: Vec<&str> = wires.iter().filter(|(_, ok)| !ok).map(|(what, _)| *what).collect();
        assert!(missing.is_empty(), "unwired: {missing:?}");
    }

    /// The watchdog's repair of a stale Win press (`Repair::ClearThenGo`) forgets an Esc the takeover swallowed, and
    /// keeps the sequences: an up that still comes passes, the next Esc is a fresh press, a post in flight keeps its
    /// fail-open window.
    #[test]
    fn clear_for_repair_clears_the_esc_bit() {
        let mut s = taken(10_000);
        clear_take_for_repair(&mut s);
        assert!(!s.esc_bit, "the swallowed press is forgotten");
        assert_eq!((s.sent, s.acked, s.oldest), (1, 0, 10_000), "the post in flight keeps its window");
        assert_eq!(take_decide(&mut s, esc(false, 10_100)), NotOurs, "an up that still comes passes");
        assert_eq!(take_decide(&mut s, esc(true, 10_200)), NotOurs, "a plain Esc passes at once");
        let src = include_str!("hook.rs");
        let order = |b: &str, x: &str, y: &str| matches!((b.find(x), b.find(y)), (Some(i), Some(j)) if i < j);
        let repair = source_block(src, "Repair::ClearThenGo =>");
        let cleared = "let mut take = load_take_state(); clear_take_for_repair(&mut take); store_take_state(&take);";
        assert!(order(&repair, "store_win_state(&s);", cleared), "reinstall's repair clears the Esc bit");
    }

    /// The re-injected chord (`inject_ctrl_shift_esc`, the fallback when Windows opens its Task Manager for it): six
    /// tagged keyboard inputs in one SendInput, Ctrl, Shift and Esc down, then the ups in reverse; the hook passes each.
    #[test]
    fn reinjected_chord_is_tagged_and_paired() {
        let keys: Vec<(u16, bool, usize)> = ctrl_shift_esc_inputs()
            .iter()
            .map(|i| {
                assert_eq!(i.r#type, INPUT_KEYBOARD);
                // SAFETY: a keyboard input (checked above): `ki` is the union's live field.
                let ki = unsafe { i.Anonymous.ki };
                (ki.wVk.0, ki.dwFlags.0 & KEYEVENTF_KEYUP.0 != 0, ki.dwExtraInfo)
            })
            .collect();
        let tag = INJECT_TAG;
        let want = [
            (0x11, false, tag),
            (0x10, false, tag),
            (0x1B, false, tag),
            (0x1B, true, tag),
            (0x10, true, tag),
            (0x11, true, tag),
        ];
        assert_eq!(keys, want);
        assert!(!accepted(LLKHF_INJECTED.0, tag), "the hook passes its own keys");
        let src = include_str!("hook.rs");
        let inject = source_block(src, "pub fn inject_ctrl_shift_esc()");
        let one = "let inputs = ctrl_shift_esc_inputs(); let sent = unsafe { SendInput(&inputs, std::mem::size_of::<INPUT>() as i32) }; sent == 6";
        assert!(inject.contains(one), "one SendInput of the six");
    }

    // ---- the Win branch (dock-keys H1)

    const RWIN: u32 = 0x5C;
    const KEY_L: u32 = 0x4C;
    const KEY_S: u32 = 0x53;
    const PRESSED: Option<usize> = Some(EV_PRESSED);
    const ALONE: Option<usize> = Some(EV_ALONE);
    const DOUBLE: Option<usize> = Some(EV_DOUBLE);
    const COMBO: Option<usize> = Some(EV_COMBO);

    /// One event of a sequence, (vk, down, kb.time), and `win_step`'s answer to it: (swallow, act, note).
    type Step = (u32, bool, u32, (bool, WinAct, Option<usize>));

    /// An event with no dock chord owned: release 1.5.2's Win branch.
    fn legacy(vk: u32, down: bool, time: u32) -> WinIn {
        WinIn {
            vk,
            down,
            time,
            mods: Mods::NONE,
            chords: DockChords::NONE,
            fullscreen: false,
            fail_open: false,
            ui_ready: true,
        }
    }

    /// Runs `steps` from a fresh state through the events `make` builds, asserting every answer.
    fn run_steps(what: &str, steps: &[Step], make: impl Fn(u32, bool, u32) -> WinIn) -> WinState {
        let mut s = WinState::default();
        for (i, &(vk, down, t, want)) in steps.iter().enumerate() {
            let out = win_step(&mut s, make(vk, down, t));
            assert_eq!((out.swallow, out.act, out.note), want, "{what}: event {i} (vk {vk:#x}, down {down}, t {t})");
        }
        s
    }

    /// Runs `steps` from a fresh state, asserting every answer.
    fn run_legacy(what: &str, steps: &[Step]) -> WinState {
        run_steps(what, steps, legacy)
    }

    /// The sequences of `legacy_sequences_are_unchanged`: today's swallow, posts and injections, written from the code
    /// of release 1.5.2 (`handle_key` and `other_key_down`).
    fn legacy_table() -> Vec<(&'static str, Vec<Step>)> {
        use WinAct::{Inject, StartMenu, Toggle};
        let none = WinAct::None;
        let pass = (false, none, None);
        let held = (true, none, None);
        let press = (true, none, PRESSED);
        let toggle = (true, Toggle, ALONE);
        let combo = (true, Inject, COMBO);
        vec![
            ("Win tap", vec![(LWIN, true, 10_000, press), (LWIN, false, 10_080, toggle)]),
            (
                "double tap within 300 ms",
                vec![
                    (LWIN, true, 10_000, press),
                    (LWIN, false, 10_080, toggle),
                    (LWIN, true, 10_300, press),
                    (LWIN, false, 10_350, (true, StartMenu, DOUBLE)),
                ],
            ),
            (
                "a third tap toggles",
                vec![
                    (LWIN, true, 10_000, press),
                    (LWIN, false, 10_080, toggle),
                    (LWIN, true, 10_300, press),
                    (LWIN, false, 10_350, (true, StartMenu, DOUBLE)),
                    (LWIN, true, 10_500, press),
                    (LWIN, false, 10_550, toggle),
                ],
            ),
            (
                "a second tap 301 ms after the first release toggles",
                vec![
                    (LWIN, true, 10_000, press),
                    (LWIN, false, 10_080, toggle),
                    (LWIN, true, 10_382, press),
                    (LWIN, false, 10_400, toggle),
                ],
            ),
            (
                "Win+E: Win swallowed, E injected with it, E up and Win up pass",
                vec![
                    (LWIN, true, 10_000, press),
                    (KEY_E, true, 10_100, combo),
                    (KEY_E, false, 10_150, pass),
                    (LWIN, false, 10_200, pass),
                ],
            ),
            (
                "Win held with auto-repeat, then released alone",
                vec![
                    (LWIN, true, 10_000, press),
                    (LWIN, true, 10_500, held),
                    (LWIN, true, 10_533, held),
                    (LWIN, true, 10_566, held),
                    (LWIN, false, 10_600, toggle),
                ],
            ),
            (
                "Win+L 2 s later (the release was lost): the key passes, the Win state is cleared",
                vec![
                    (LWIN, true, 10_000, press),
                    (KEY_L, true, 12_000, pass),
                    (KEY_L, false, 12_050, pass),
                    (LWIN, false, 12_100, pass),
                    (KEY_E, true, 12_200, pass),
                ],
            ),
            (
                "a key 1500 ms after the Win press is still a combination",
                vec![(LWIN, true, 10_000, press), (KEY_E, true, 11_500, combo), (KEY_E, false, 11_550, pass)],
            ),
            (
                "LWin+RWin: the second Win is an other key",
                vec![
                    (LWIN, true, 10_000, press),
                    (RWIN, true, 10_100, combo),
                    (RWIN, false, 10_150, pass),
                    (KEY_E, true, 10_200, pass),
                    (LWIN, false, 10_250, pass),
                ],
            ),
            (
                "Win+Shift+S: Shift injects the pair, S and the releases pass",
                vec![
                    (LWIN, true, 10_000, press),
                    (LSHIFT, true, 10_100, combo),
                    (KEY_S, true, 10_150, pass),
                    (KEY_S, false, 10_200, pass),
                    (LSHIFT, false, 10_250, pass),
                    (LWIN, false, 10_300, pass),
                ],
            ),
            (
                "the Win up after Win+E is no first tap",
                vec![
                    (LWIN, true, 10_000, press),
                    (KEY_E, true, 10_050, combo),
                    (KEY_E, false, 10_080, pass),
                    (LWIN, false, 10_100, pass),
                    (LWIN, true, 10_200, press),
                    (LWIN, false, 10_250, toggle),
                ],
            ),
            (
                "Win auto-repeat after the pair passes",
                vec![
                    (LWIN, true, 10_000, press),
                    (KEY_E, true, 10_100, combo),
                    (KEY_E, false, 10_150, pass),
                    (LWIN, true, 10_600, pass),
                    (LWIN, false, 10_700, pass),
                ],
            ),
            (
                "keys without Win pass",
                vec![(KEY_E, true, 10_000, pass), (KEY_E, false, 10_050, pass), (LSHIFT, true, 10_100, pass)],
            ),
            ("RWin tap", vec![(RWIN, true, 10_000, press), (RWIN, false, 10_080, toggle)]),
            (
                "a press whose release was lost restarts after 1.5 s",
                vec![(LWIN, true, 10_000, press), (LWIN, true, 11_501, press), (LWIN, false, 11_550, toggle)],
            ),
            ("a Win up with no tracked press passes", vec![(LWIN, false, 10_000, pass), (RWIN, false, 10_010, pass)]),
            (
                "after Win+L the injected press is never stale-cleared; the next press starts fresh",
                vec![
                    (LWIN, true, 10_000, press),
                    (KEY_L, true, 10_100, combo),
                    (KEY_E, true, 20_000, pass),
                    (LWIN, true, 20_100, press),
                    (LWIN, false, 20_150, toggle),
                ],
            ),
            (
                "the right Win up does not release a left Win press",
                vec![(LWIN, true, 10_000, press), (RWIN, false, 10_050, pass), (LWIN, false, 10_100, toggle)],
            ),
        ]
    }

    /// H1: every sequence gives today's swallow, posts and injections, written from the code of release 1.5.2
    /// (`handle_key` and `other_key_down`).
    #[test]
    fn legacy_sequences_are_unchanged() {
        let table = legacy_table();
        assert!(table.len() >= 14);
        for (what, steps) in &table {
            run_legacy(what, steps);
        }
        let s = run_legacy("Win+E state", &table[4].1);
        assert_eq!((s.down, s.other, s.last_alone_up, s.double), (false, true, 0, false));
        // Injected keys never reach `win_step`: `handle_key` returns first for our tag and for other apps' injections
        // (`tagged_win_v_passes_and_arms_nothing` pins that its first statement is this filter).
        assert!(!accepted(0, INJECT_TAG));
        assert!(!accepted(LLKHF_INJECTED.0, 0));
        assert!(accepted(0, 0));
    }

    /// Release 1.5.2's Win branch (`handle_key` and `other_key_down` of 18f2762) on a local state, kept as the reference
    /// `win_step` must equal while no dock chord is owned.
    #[derive(Default)]
    struct Old {
        down: bool,
        other: bool,
        vk: u32,
        last: u32,
        alone: u32,
        double: bool,
    }

    fn old_step(o: &mut Old, vk: u32, down: bool, t: u32) -> (bool, WinAct, Option<usize>) {
        let pass = (false, WinAct::None, None);
        let other_key_down = |o: &mut Old| {
            if !o.down || o.other {
                return pass;
            }
            if t.wrapping_sub(o.last) > REPEAT_WINDOW_MS {
                o.down = false;
                return pass;
            }
            o.other = true;
            (true, WinAct::Inject, COMBO)
        };
        if vk == LWIN || vk == RWIN {
            if down {
                let fresh = t.wrapping_sub(o.last) > REPEAT_WINDOW_MS;
                o.last = t;
                if o.down && !fresh {
                    if vk == o.vk {
                        return (!o.other, WinAct::None, None);
                    }
                    return other_key_down(o);
                }
                o.down = true;
                o.other = false;
                o.vk = vk;
                o.double = o.alone != 0 && t.wrapping_sub(o.alone) <= DOUBLE_TAP_MS;
                return (true, WinAct::None, PRESSED);
            }
            if o.down && vk == o.vk {
                o.down = false;
                if !o.other {
                    if std::mem::replace(&mut o.double, false) {
                        o.alone = 0;
                        return (true, WinAct::StartMenu, DOUBLE);
                    }
                    o.alone = t | 1;
                    return (true, WinAct::Toggle, ALONE);
                }
                o.alone = 0;
                o.double = false;
            }
            return pass;
        }
        if down {
            other_key_down(o)
        } else {
            pass
        }
    }

    /// A small deterministic generator (xorshift32) for the random sequences.
    struct Rng(u32);

    impl Rng {
        fn next(&mut self) -> u32 {
            self.0 ^= self.0 << 13;
            self.0 ^= self.0 >> 17;
            self.0 ^= self.0 << 5;
            self.0
        }

        fn pick<T: Copy>(&mut self, of: &[T]) -> T {
            of[self.next() as usize % of.len()]
        }
    }

    /// The keys the random sequences press: both Win keys, the chord keys, the modifiers and two plain keys.
    const MIXED_KEYS: [u32; 12] = [LWIN, RWIN, LWIN, 0x31, 0x33, 0x30, 0x54, 0x42, LSHIFT, LALT, LCTRL, KEY_E];
    /// Gaps between events (ms): auto-repeat, typing, the double-tap edge, the stale edge and beyond.
    const GAPS: [u32; 9] = [0, 33, 80, 299, 300, 301, 1500, 1501, 4000];

    /// H1: with no dock chord owned `win_step` answers exactly as release 1.5.2's branch on 20,000 random sequences.
    #[test]
    fn legacy_model_matches_on_random_sequences() {
        let mut rng = Rng(0x2545_F491);
        for seq in 0..20_000 {
            let (mut s, mut o) = (WinState::default(), Old::default());
            let mut t = 10_000u32;
            for i in 0..12 {
                let vk = rng.pick(&MIXED_KEYS);
                let down = !rng.next().is_multiple_of(3);
                t = t.wrapping_add(rng.pick(&GAPS));
                let out = win_step(&mut s, legacy(vk, down, t));
                let want = old_step(&mut o, vk, down, t);
                assert_eq!((out.swallow, out.act, out.note), want, "sequence {seq}, event {i}: vk {vk:#x} down {down}");
                assert!(
                    !out.sync && !out.mask && !out.peek_end && !matches!(out.peek, PeekCmd::Arm(_)),
                    "sequence {seq}, event {i}: no chord post, no mask, no Peek without a chord table"
                );
                assert_eq!((s.owned, s.modded, s.peeking, s.chord_keys), (false, false, false, 0), "{seq}, {i}");
                assert_eq!(
                    (s.down, s.other, s.vk, s.last_time, s.last_alone_up, s.double),
                    (o.down, o.other, o.vk, o.last, o.alone, o.double),
                    "sequence {seq}, event {i}"
                );
            }
        }
    }

    // ---- owned chords (dock-keys H2a)

    const KEY_3: u32 = 0x33;
    const KEY_4: u32 = 0x34;

    /// An event with every chord row owned, a live UI and no modifier.
    fn owned(vk: u32, down: bool, time: u32) -> WinIn {
        WinIn { chords: ALL_ROWS, ..legacy(vk, down, time) }
    }

    /// (swallow, act, sync, note) of an answer.
    fn ans(o: WinOut) -> (bool, WinAct, bool, Option<usize>) {
        (o.swallow, o.act, o.sync, o.note)
    }

    const SILENT: (bool, WinAct, bool, Option<usize>) = (true, WinAct::None, false, None);
    const PASSED: (bool, WinAct, bool, Option<usize>) = (false, WinAct::None, false, None);
    const PRESS: (bool, WinAct, bool, Option<usize>) = (true, WinAct::None, false, PRESSED);

    /// Win down at 10,000 and 3 down at 10,100 with every row owned: the state after the owned chord.
    fn after_win_3() -> WinState {
        let mut s = WinState::default();
        assert_eq!(ans(win_step(&mut s, owned(LWIN, true, 10_000))), PRESS);
        let out = win_step(&mut s, owned(KEY_3, true, 10_100));
        assert_eq!(ans(out), (true, WinAct::Chord(ChordAction::App(2)), false, None));
        s
    }

    #[test]
    fn owned_digit_is_swallowed_and_posted() {
        let mut s = after_win_3();
        assert_eq!(ans(win_step(&mut s, owned(KEY_3, false, 10_150))), SILENT, "its up never reaches Windows");
        assert_eq!(ans(win_step(&mut s, owned(LWIN, false, 10_200))), SILENT, "Win up swallowed, no toggle");
        assert_eq!((s.down, s.owned, s.chord_keys, s.last_alone_up), (false, false, 0, 0));
        // The next lone tap toggles (the owned press was no first tap of a double tap).
        assert_eq!(ans(win_step(&mut s, owned(LWIN, true, 10_300))), PRESS);
        assert_eq!(ans(win_step(&mut s, owned(LWIN, false, 10_350))), (true, WinAct::Toggle, false, ALONE));
    }

    #[test]
    fn owned_then_unowned_injects_the_pair() {
        let mut s = after_win_3();
        assert_eq!(ans(win_step(&mut s, owned(KEY_3, false, 10_150))), SILENT);
        assert_eq!(ans(win_step(&mut s, owned(KEY_E, true, 10_200))), (true, WinAct::Inject, false, COMBO));
        assert_eq!(ans(win_step(&mut s, owned(KEY_E, false, 10_250))), PASSED);
        assert_eq!(ans(win_step(&mut s, owned(LWIN, false, 10_300))), PASSED, "Windows saw the injected Win down");
        // After the pair an owned chord is still the dock's (Windows holds Win, sees nothing of the digit).
        let mut s = WinState::default();
        win_step(&mut s, owned(LWIN, true, 20_000));
        assert_eq!(ans(win_step(&mut s, owned(KEY_E, true, 20_100))).1, WinAct::Inject);
        assert_eq!(ans(win_step(&mut s, owned(KEY_4, true, 20_200))).1, WinAct::Chord(ChordAction::App(3)));
        assert_eq!(ans(win_step(&mut s, owned(KEY_4, false, 20_250))), SILENT);
        assert_eq!(ans(win_step(&mut s, owned(LWIN, false, 20_300))), PASSED);
    }

    #[test]
    fn repeat_of_a_chord_key_is_silent() {
        let mut s = after_win_3();
        for t in (10_600..12_000).step_by(33) {
            assert_eq!(ans(win_step(&mut s, owned(KEY_3, true, t))), SILENT, "repeat at {t}");
        }
        assert_eq!(ans(win_step(&mut s, owned(KEY_3, false, 12_000))), SILENT);
        assert_eq!(s.chord_keys, 0);
        // Win still held: the next digit is a new chord.
        assert_eq!(ans(win_step(&mut s, owned(KEY_4, true, 12_100))).1, WinAct::Chord(ChordAction::App(3)));
    }

    #[test]
    fn chord_key_up_after_win_up_is_swallowed() {
        let mut s = after_win_3();
        assert_eq!(ans(win_step(&mut s, owned(LWIN, false, 10_150))), SILENT);
        assert_eq!(ans(win_step(&mut s, owned(KEY_3, false, 10_200))), SILENT);
        assert_eq!(s.chord_keys, 0);
        assert_eq!(ans(win_step(&mut s, owned(KEY_3, false, 10_250))), PASSED, "a second up has no bit");
    }

    #[test]
    fn held_digit_after_win_up_is_not_typed() {
        let mut s = after_win_3();
        assert_eq!(ans(win_step(&mut s, owned(LWIN, false, 10_150))), SILENT);
        for i in 0..5 {
            let t = 10_600 + i * 33;
            assert_eq!(ans(win_step(&mut s, owned(KEY_3, true, t))), SILENT, "repeat {i}: never typed");
        }
        assert_eq!(ans(win_step(&mut s, owned(KEY_3, false, 10_800))), SILENT);
        assert_eq!(s.chord_keys, 0, "the bit is clear");
        assert_eq!(ans(win_step(&mut s, owned(KEY_3, true, 10_900))), PASSED, "a new 3 types");
    }

    #[test]
    fn bit_expires_after_the_stale_window() {
        let base = {
            let mut s = after_win_3();
            win_step(&mut s, owned(LWIN, false, 10_150));
            assert_eq!((s.down, s.chord_keys, s.chord_last), (false, 1 << 3, 10_100));
            s
        };
        let mut s = base;
        assert_eq!(ans(win_step(&mut s, owned(KEY_3, true, 10_100 + 1_499))), SILENT, "within the window");
        assert_eq!(s.chord_keys, 1 << 3);
        let mut s = base;
        assert_eq!(ans(win_step(&mut s, owned(KEY_3, true, 10_100 + 1_501))), PASSED, "its release was lost");
        assert_eq!(s.chord_keys, 0);
        assert_eq!(ans(win_step(&mut s, owned(KEY_3, false, 11_700))), PASSED, "the up of that new press passes");
        // Win held again: an aged bit is a fresh press, so the chord is the dock's again.
        let mut s = base;
        win_step(&mut s, owned(LWIN, true, 13_000));
        assert_eq!(ans(win_step(&mut s, owned(KEY_3, true, 13_050))).1, WinAct::Chord(ChordAction::App(2)));
    }

    #[test]
    fn owned_chord_is_not_swallowed_when_hwnd_is_null() {
        let no_ui = |vk, down, t| WinIn { ui_ready: false, ..owned(vk, down, t) };
        let mut s = WinState::default();
        assert_eq!(ans(win_step(&mut s, no_ui(LWIN, true, 10_000))), PRESS);
        assert_eq!(ans(win_step(&mut s, no_ui(KEY_3, true, 10_100))), (true, WinAct::Inject, false, COMBO));
        assert_eq!(ans(win_step(&mut s, no_ui(KEY_3, false, 10_150))), PASSED);
        assert_eq!(ans(win_step(&mut s, no_ui(LWIN, false, 10_200))), PASSED);
        assert_eq!((s.owned, s.chord_keys), (false, 0));
    }

    #[test]
    fn fail_open_injects_and_syncs() {
        let stuck = |vk, down, t| WinIn { fail_open: true, ..owned(vk, down, t) };
        let mut s = WinState::default();
        assert_eq!(ans(win_step(&mut s, stuck(LWIN, true, 10_000))), PRESS);
        assert_eq!(ans(win_step(&mut s, stuck(KEY_3, true, 10_100))), (true, WinAct::Inject, true, COMBO));
        assert_eq!(ans(win_step(&mut s, stuck(KEY_3, false, 10_150))), PASSED, "no bit: Windows has the key");
        assert_eq!(ans(win_step(&mut s, stuck(KEY_4, true, 10_200))), (false, WinAct::None, true, None));
        assert_eq!(ans(win_step(&mut s, stuck(KEY_E, true, 10_250))), PASSED, "not a chord: no sync");
        assert_eq!(ans(win_step(&mut s, stuck(LWIN, false, 10_300))), PASSED);
        assert_eq!((s.owned, s.chord_keys), (false, 0));
    }

    #[test]
    fn fail_open_window() {
        assert!(!chord_fail_open(1, 1, 0, 10_000), "nothing pending");
        assert!(!chord_fail_open(1, 0, 10_000, 11_499));
        assert!(!chord_fail_open(1, 0, 10_000, 11_500), "1500 ms: not yet");
        assert!(chord_fail_open(1, 0, 10_000, 11_501));
        assert!(!chord_fail_open(1, 1, 10_000, 11_501), "acknowledged");
        assert!(chord_fail_open(0, u16::MAX, 10_000, 11_501), "the sequence wraps");
        assert!(!chord_fail_open(u16::MAX, 0, 10_000, 11_501), "acked ahead (a resync): nothing pending");
        assert!(chord_fail_open(3, 1, u32::MAX - 500, 1_001), "the clock wraps");
        assert!(!chord_fail_open(3, 1, u32::MAX - 500, 999));
    }

    #[test]
    fn chord_bits_cover_the_owned_keys() {
        let mut seen = 0u16;
        for vk in (0x30..=0x39).chain([0x54, 0x42]) {
            let bit = chord_bit(vk).unwrap();
            assert_eq!(bit.count_ones(), 1, "{vk:#x}");
            assert_eq!(seen & bit, 0, "{vk:#x} has its own bit");
            seen |= bit;
        }
        assert_eq!(seen, 0x0FFF);
        assert_eq!(chord_bit(0x30), Some(1), "0 is bit 0");
        assert_eq!(chord_bit(0x39), Some(1 << 9));
        for vk in [0x2F, 0x3A, 0x41, 0x60, 0x61, LWIN, LSHIFT, KEY_E] {
            assert_eq!(chord_bit(vk), None, "{vk:#x}");
        }
        // Every key an owned row resolves has a bit.
        for vk in 0..=0xFF {
            for m in [Mods::NONE, Mods::SHIFT, Mods::ALT] {
                if resolve_chord(ALL_ROWS, vk, m).is_some() {
                    assert!(chord_bit(vk).is_some(), "{vk:#x}");
                }
            }
        }
    }

    /// A press that owned a chord has no auto-repeat once the digit went down: a pause while Win is held is no lost
    /// release, so the next digit is still a chord (up to LONE_STUCK_MS for a chord key or a modifier, as `repair_plan`
    /// waits for a lone press).
    #[test]
    fn a_pause_after_an_owned_chord_keeps_win_held() {
        let mut s = after_win_3();
        win_step(&mut s, owned(KEY_3, false, 10_150));
        assert_eq!(ans(win_step(&mut s, owned(KEY_4, true, 13_000))).1, WinAct::Chord(ChordAction::App(3)));
        win_step(&mut s, owned(KEY_4, false, 13_050));
        // Beyond LONE_STUCK_MS the release was lost (the secure desktop): the key passes, nothing injected.
        assert_eq!(ans(win_step(&mut s, owned(KEY_E, true, 13_000 + LONE_STUCK_MS + 1))), PASSED);
        assert_eq!((s.down, s.owned), (false, false));
        assert_eq!(ans(win_step(&mut s, owned(LWIN, false, 23_200))), PASSED, "no tracked press");
    }

    /// A Win release lost on the secure desktop after an owned chord or a passed modifier (a UAC prompt raised by
    /// Win+Shift+n): past REPEAT_WINDOW_MS after the last key the press kept (`renew`: here the chord's or the
    /// modifier's down) a key that is neither a chord key nor a modifier takes the stale rule and passes, so Windows
    /// never gets an injected Win down it would not see released.
    #[test]
    fn a_plain_key_after_a_pause_injects_nothing() {
        let mut s = after_win_3();
        win_step(&mut s, owned(KEY_3, false, 10_150));
        let out = win_step(&mut s, owned(KEY_E, true, 10_100 + REPEAT_WINDOW_MS + 1));
        assert_eq!(ans(out), PASSED);
        assert_eq!((s.down, s.owned, s.modded, s.other), (false, false, false, false));
        assert_eq!(ans(win_step(&mut s, owned(KEY_E, false, 11_650))), PASSED);
        assert_eq!(ans(win_step(&mut s, owned(LWIN, false, 11_700))), PASSED, "no tracked press");
        // Within REPEAT_WINDOW_MS the pair is injected as before.
        let mut s = after_win_3();
        win_step(&mut s, owned(KEY_3, false, 10_150));
        let out = win_step(&mut s, owned(KEY_E, true, 10_100 + REPEAT_WINDOW_MS));
        assert_eq!(ans(out), (true, WinAct::Inject, false, COMBO));
        // After a passed modifier: the same.
        let mut s = WinState::default();
        win_step(&mut s, owned(LWIN, true, 10_000));
        win_step(&mut s, with_mods(LSHIFT, true, 10_100, Mods::SHIFT));
        win_step(&mut s, owned(LSHIFT, false, 10_150));
        assert!(s.modded);
        assert_eq!(ans(win_step(&mut s, owned(KEY_E, true, 10_100 + REPEAT_WINDOW_MS + 1))), PASSED);
        assert!(!s.down);
        // A chord key under fail-open would inject the pair: past REPEAT_WINDOW_MS it takes the stale rule too.
        let mut s = after_win_3();
        win_step(&mut s, owned(KEY_3, false, 10_150));
        let out = win_step(&mut s, WinIn { fail_open: true, ..owned(KEY_4, true, 10_100 + REPEAT_WINDOW_MS + 1) });
        assert_eq!(ans(out), PASSED);
        assert!(!s.down);
    }

    /// A modifier passed while Win is held, or an owned chord key, is the newest key: it auto-repeats and Win no longer
    /// does. Its events, with no gap over REPEAT_WINDOW_MS, show the hook was never cut off, so Win's release was not
    /// lost: the next key injects the pair as in 1.5.2, however long the modifier or the digit was held.
    #[test]
    fn a_held_modifier_or_chord_key_keeps_win_held() {
        const F4: u32 = 0x73;
        // Win+Ctrl held 2 s (Ctrl auto-repeating), then F4: Win+Ctrl+F4, never Ctrl+F4 alone.
        let mut s = WinState::default();
        win_step(&mut s, owned(LWIN, true, 10_000));
        let mut t = 10_050;
        while t < 12_050 {
            assert_eq!(ans(win_step(&mut s, with_mods(LCTRL, true, t, Mods::CTRL))), PASSED, "Ctrl at {t}: passed");
            t += 33;
        }
        assert!(s.down && s.modded && !s.other);
        assert_eq!(s.last_time, t - 33, "the stamp the watchdog reads is renewed too");
        assert_eq!(ans(win_step(&mut s, with_mods(F4, true, t, Mods::CTRL))), (true, WinAct::Inject, false, COMBO));
        // Win+Shift held 2 s, then S: Win+Shift+S.
        let mut s = WinState::default();
        win_step(&mut s, owned(LWIN, true, 10_000));
        let mut t = 10_050;
        while t < 12_050 {
            assert_eq!(ans(win_step(&mut s, with_mods(LSHIFT, true, t, Mods::SHIFT))), PASSED);
            t += 33;
        }
        assert_eq!(ans(win_step(&mut s, with_mods(KEY_S, true, t, Mods::SHIFT))), (true, WinAct::Inject, false, COMBO));
        // Win+3 held 2 s (3 auto-repeating, swallowed), then E: Win+E.
        let mut s = after_win_3();
        let mut t = 10_133;
        while t < 12_100 {
            assert_eq!(ans(win_step(&mut s, owned(KEY_3, true, t))), SILENT, "3's repeat at {t} is swallowed");
            t += 33;
        }
        assert_eq!(ans(win_step(&mut s, owned(KEY_E, true, t))), (true, WinAct::Inject, false, COMBO));
        // The chord's own down renews too: with the longest typing delay (1 s) 3's first repeat comes 1.6 s after Win.
        let mut s = WinState::default();
        win_step(&mut s, owned(LWIN, true, 10_000));
        assert_eq!(ans(win_step(&mut s, owned(KEY_3, true, 10_600))).1, WinAct::Chord(ChordAction::App(2)));
        let mut t = 11_600;
        while t < 12_600 {
            assert_eq!(ans(win_step(&mut s, owned(KEY_3, true, t))), SILENT);
            t += 33;
        }
        assert_eq!(ans(win_step(&mut s, owned(KEY_E, true, t))), (true, WinAct::Inject, false, COMBO));
    }

    /// A gap over REPEAT_WINDOW_MS in the hook's events still takes the stale rule: after a pause, or when a modifier
    /// auto-repeats again after the hook was cut off (the secure desktop), a key that is neither a chord key nor a
    /// modifier passes and no pair is injected.
    #[test]
    fn a_gap_in_the_events_still_takes_the_stale_rule() {
        // Win, Shift down and up, a 2 s pause, then S.
        let mut s = WinState::default();
        win_step(&mut s, owned(LWIN, true, 10_000));
        win_step(&mut s, with_mods(LSHIFT, true, 10_050, Mods::SHIFT));
        win_step(&mut s, owned(LSHIFT, false, 10_100));
        assert_eq!(ans(win_step(&mut s, owned(KEY_S, true, 12_100))), PASSED);
        assert_eq!((s.down, s.modded, s.other), (false, false, false));
        // Shift auto-repeating, a 2 s gap, Shift auto-repeating again: the repeats after the gap renew nothing.
        let mut s = WinState::default();
        win_step(&mut s, owned(LWIN, true, 10_000));
        for t in (10_050..11_000).step_by(33) {
            win_step(&mut s, with_mods(LSHIFT, true, t, Mods::SHIFT));
        }
        let renewed = s.last_time;
        for t in (13_000..14_000).step_by(33) {
            assert_eq!(ans(win_step(&mut s, with_mods(LSHIFT, true, t, Mods::SHIFT))), PASSED);
        }
        assert_eq!(s.last_time, renewed, "no renewal across the gap");
        assert_eq!(ans(win_step(&mut s, with_mods(KEY_S, true, 14_000, Mods::SHIFT))), PASSED);
        assert!(!s.down);
        // Win+3 with 3 auto-repeating, a 2 s pause, then E.
        let mut s = after_win_3();
        for t in (10_133..11_000).step_by(33) {
            win_step(&mut s, owned(KEY_3, true, t));
        }
        assert_eq!(ans(win_step(&mut s, owned(KEY_E, true, 13_000))), PASSED);
        assert!(!s.down);
    }

    /// A callback stall during an owned chord (live check K-3): Windows removed the hook, so the digit's and Win's
    /// releases never came, and the async state never saw the swallowed Win down. Every tick reinstalls the hook with
    /// the state kept (the clear still waits for LONE_STUCK_MS), and the next Win press is fresh and owns its chord.
    #[test]
    fn watchdog_reinstalls_after_a_stall_in_an_owned_chord() {
        let mut s = after_win_3();
        let mut age = 0;
        while age <= LONE_STUCK_MS {
            assert_eq!(repair_plan(s.down, false, false, s.other, age), Repair::Go, "age {age}: the hook is back");
            assert_eq!(repair_plan(s.down, true, false, s.other, age), Repair::Skip, "Windows holds a Win down");
            age += REINSTALL_MS;
        }
        assert_eq!(repair_plan(s.down, false, false, s.other, LONE_STUCK_MS + 1), Repair::ClearThenGo);
        // The reinstalled hook sees the next Win+3: a fresh press, the stale bit of 3 expired, the chord owned.
        assert_eq!(ans(win_step(&mut s, owned(LWIN, true, 12_500))), PRESS);
        let out = win_step(&mut s, owned(KEY_3, true, 12_600));
        assert_eq!(ans(out), (true, WinAct::Chord(ChordAction::App(2)), false, None));
    }

    // ---- Shift and Alt (dock-keys H2b)

    /// An owned-rows event with the tracked modifiers `mods`.
    fn with_mods(vk: u32, down: bool, time: u32, mods: Mods) -> WinIn {
        WinIn { mods, ..owned(vk, down, time) }
    }

    /// (swallow, act, mask) of an answer.
    fn sam(o: WinOut) -> (bool, WinAct, bool) {
        (o.swallow, o.act, o.mask)
    }

    #[test]
    fn shift_and_alt_chords_mask_the_modifier() {
        let (sh, alt) = (Mods::SHIFT, Mods::ALT);
        let mut s = WinState::default();
        assert_eq!(ans(win_step(&mut s, owned(LWIN, true, 10_000))), PRESS);
        assert_eq!(sam(win_step(&mut s, with_mods(LSHIFT, true, 10_050, sh))), (false, WinAct::None, false));
        assert!(s.modded && !s.other, "Shift passed alone, Win pending");
        let out = win_step(&mut s, with_mods(KEY_3, true, 10_100, sh));
        assert_eq!(sam(out), (true, WinAct::Chord(ChordAction::NewInstance(2)), true), "the mask follows the post");
        assert_eq!(sam(win_step(&mut s, with_mods(KEY_3, false, 10_150, sh))), (true, WinAct::None, false));
        assert_eq!(sam(win_step(&mut s, owned(LSHIFT, false, 10_200))), (false, WinAct::None, false));
        assert_eq!(sam(win_step(&mut s, owned(LWIN, false, 10_250))), (true, WinAct::None, false), "no toggle");
        // Win+Alt+3.
        let mut s = WinState::default();
        win_step(&mut s, owned(LWIN, true, 20_000));
        assert_eq!(sam(win_step(&mut s, with_mods(LALT, true, 20_050, alt))), (false, WinAct::None, false));
        let out = win_step(&mut s, with_mods(KEY_3, true, 20_100, alt));
        assert_eq!(sam(out), (true, WinAct::Chord(ChordAction::AppMenu(2)), true));
        assert_eq!(sam(win_step(&mut s, with_mods(KEY_3, false, 20_150, alt))), (true, WinAct::None, false));
        // Win is still held: the Alt up gets the mask again (harmless; Windows saw the Alt down alone).
        assert_eq!(sam(win_step(&mut s, owned(LALT, false, 20_200))), (false, WinAct::None, true));
        assert_eq!(sam(win_step(&mut s, owned(LWIN, false, 20_250))), (true, WinAct::None, false));
        // A chord without a modifier injects no mask.
        let mut s = WinState::default();
        win_step(&mut s, owned(LWIN, true, 30_000));
        assert_eq!(
            sam(win_step(&mut s, owned(KEY_3, true, 30_100))),
            (true, WinAct::Chord(ChordAction::App(2)), false)
        );
    }

    #[test]
    fn modifier_alone_with_win_is_no_tap() {
        let mut s = WinState::default();
        assert_eq!(ans(win_step(&mut s, owned(LWIN, true, 10_000))), PRESS);
        assert_eq!(ans(win_step(&mut s, with_mods(LSHIFT, true, 10_050, Mods::SHIFT))), PASSED, "nothing injected");
        assert_eq!(sam(win_step(&mut s, owned(LSHIFT, false, 10_100))), (false, WinAct::None, false));
        assert_eq!(ans(win_step(&mut s, owned(LWIN, false, 10_150))), SILENT, "Win up swallowed, nothing toggled");
        assert_eq!((s.down, s.modded, s.last_alone_up), (false, false, 0));
        // Ctrl alike; Win+Ctrl+3 is no chord (Ctrl never matches), so Windows gets it with the injected Win.
        let mut s = WinState::default();
        win_step(&mut s, owned(LWIN, true, 20_000));
        assert_eq!(ans(win_step(&mut s, with_mods(LCTRL, true, 20_050, Mods::CTRL))), PASSED);
        assert_eq!(
            ans(win_step(&mut s, with_mods(KEY_3, true, 20_100, Mods::CTRL))),
            (true, WinAct::Inject, false, COMBO)
        );
    }

    #[test]
    fn win_shift_s_still_reaches_windows() {
        let sh = Mods::SHIFT;
        let mut s = WinState::default();
        win_step(&mut s, owned(LWIN, true, 10_000));
        assert_eq!(ans(win_step(&mut s, with_mods(LSHIFT, true, 10_050, sh))), PASSED, "Shift was passed");
        assert_eq!(ans(win_step(&mut s, with_mods(KEY_S, true, 10_100, sh))), (true, WinAct::Inject, false, COMBO));
        assert_eq!(ans(win_step(&mut s, with_mods(KEY_S, false, 10_150, sh))), PASSED);
        assert_eq!(ans(win_step(&mut s, owned(LSHIFT, false, 10_200))), PASSED);
        assert_eq!(ans(win_step(&mut s, owned(LWIN, false, 10_250))), PASSED, "Windows saw the injected Win down");
    }

    #[test]
    fn win_alt_alt_up_first_is_no_menu() {
        let mut s = WinState::default();
        win_step(&mut s, owned(LWIN, true, 10_000));
        assert_eq!(sam(win_step(&mut s, with_mods(LALT, true, 10_050, Mods::ALT))), (false, WinAct::None, false));
        assert_eq!(sam(win_step(&mut s, owned(LALT, false, 10_100))), (false, WinAct::None, true), "mask, then the up");
        assert_eq!(sam(win_step(&mut s, owned(LWIN, false, 10_150))), (true, WinAct::None, false));
        // Without Win held an Alt up is plain.
        assert_eq!(sam(win_step(&mut s, owned(LALT, false, 10_200))), (false, WinAct::None, false));
        // After the pair Windows holds Win: no mask (Win+Alt+R reached Windows whole).
        let mut s = WinState::default();
        win_step(&mut s, owned(LWIN, true, 20_000));
        win_step(&mut s, with_mods(LALT, true, 20_050, Mods::ALT));
        assert_eq!(sam(win_step(&mut s, with_mods(0x52, true, 20_100, Mods::ALT))), (true, WinAct::Inject, false));
        assert_eq!(sam(win_step(&mut s, owned(LALT, false, 20_150))), (false, WinAct::None, false));
    }

    /// An Alt pressed after an owned chord is seen by Windows alone too: its release gets the mask.
    #[test]
    fn alt_after_an_owned_chord_is_masked() {
        let mut s = after_win_3();
        win_step(&mut s, owned(KEY_3, false, 10_150));
        assert_eq!(sam(win_step(&mut s, with_mods(LALT, true, 10_200, Mods::ALT))), (false, WinAct::None, false));
        assert_eq!(sam(win_step(&mut s, owned(LALT, false, 10_250))), (false, WinAct::None, true));
    }

    #[test]
    fn fast_shift_digit_reads_the_tracker() {
        // The async key state is stale inside the callback (it reads every key up here): the tracker alone decides.
        let stale = |_: u32| false;
        let mut t = ModTrack::default();
        let mut s = WinState::default();
        let step = |t: &mut ModTrack, s: &mut WinState, vk: u32, down: bool, time: u32| {
            track_step(t, 0, vk, down, time, stale);
            win_step(s, with_mods(vk, down, time, Mods(t.mods(false).0 & !Mods::WIN.0)))
        };
        step(&mut t, &mut s, LWIN, true, 10_000);
        step(&mut t, &mut s, LSHIFT, true, 10_100);
        let out = step(&mut t, &mut s, KEY_3, true, 10_105);
        assert_eq!(sam(out), (true, WinAct::Chord(ChordAction::NewInstance(2)), true));
        // Past MOD_STALE_MS with no modifier event the async state repairs it: a Shift really released reads up.
        step(&mut t, &mut s, KEY_3, false, 10_150);
        let out = step(&mut t, &mut s, KEY_4, true, 10_100 + MOD_STALE_MS + 1);
        assert_eq!(sam(out), (true, WinAct::Chord(ChordAction::App(3)), false));
    }

    #[test]
    fn tracker_runs_for_the_dock_alone() {
        assert!(!tracks(false, DockChords::NONE), "nothing set: the callback tracks nothing, as in 1.5.2");
        assert!(tracks(true, DockChords::NONE));
        assert!(tracks(false, DockChords::DIGITS));
        assert!(tracks(false, DockChords::PEEK));
        assert!(tracks(true, ALL_ROWS));
    }

    /// Win+Shift held: Shift auto-repeats, Win does not; a pause before the digit is no lost release.
    #[test]
    fn a_pause_with_a_modifier_keeps_win_held() {
        let sh = Mods::SHIFT;
        let mut s = WinState::default();
        win_step(&mut s, owned(LWIN, true, 10_000));
        win_step(&mut s, with_mods(LSHIFT, true, 10_100, sh));
        for t in (10_600..12_500).step_by(33) {
            assert_eq!(ans(win_step(&mut s, with_mods(LSHIFT, true, t, sh))), PASSED, "Shift's repeat at {t}");
        }
        let out = win_step(&mut s, with_mods(KEY_3, true, 12_500, sh));
        assert_eq!(sam(out), (true, WinAct::Chord(ChordAction::NewInstance(2)), true));
    }

    // ---- the Win release (dock-keys H2c)

    /// H2c: the Win up of a tracked press over every (other, peeking, owned, modded), with Alt up and with Alt still
    /// down: (swallow, act, mask, Peek end) pinned per row.
    #[test]
    fn win_up_plan_table() {
        use WinAct::{None as N, Toggle as T};
        /// (other, peeking, owned, modded) and (swallow, act, mask with Alt down, Peek end).
        type Row = ((bool, bool, bool, bool), (bool, WinAct, bool, bool));
        #[rustfmt::skip]
        let table: [Row; 16] = [
            // (other, peeking, owned, modded) -> (swallow, act, mask with Alt down, Peek end)
            ((false, false, false, false), (true, T, false, false)),
            ((false, false, false, true), (true, N, true, false)),
            ((false, false, true, false), (true, N, false, false)),
            ((false, false, true, true), (true, N, true, false)),
            ((false, true, false, false), (true, N, false, true)),
            ((false, true, false, true), (true, N, true, true)),
            ((false, true, true, false), (true, N, false, true)),
            ((false, true, true, true), (true, N, true, true)),
            ((true, false, false, false), (false, N, false, false)),
            ((true, false, false, true), (false, N, false, false)),
            ((true, false, true, false), (false, N, false, false)),
            ((true, false, true, true), (false, N, false, false)),
            ((true, true, false, false), (false, N, false, true)),
            ((true, true, false, true), (false, N, false, true)),
            ((true, true, true, false), (false, N, false, true)),
            ((true, true, true, true), (false, N, false, true)),
        ];
        for ((other, peeking, owned, modded), (swallow, act, mask, peek_end)) in table {
            for alt in [false, true] {
                let mut s = WinState {
                    down: true,
                    vk: LWIN,
                    last_time: 10_000,
                    other,
                    peeking,
                    owned,
                    modded,
                    ..WinState::default()
                };
                let mods = if alt { Mods::ALT } else { Mods::NONE };
                let out = win_step(&mut s, with_mods(LWIN, false, 10_100, mods));
                let row = (other, peeking, owned, modded, alt);
                assert_eq!(
                    (out.swallow, out.act, out.mask, out.peek_end),
                    (swallow, act, mask && alt, peek_end),
                    "{row:?}"
                );
                assert!(!s.down && !s.peeking && !s.owned && !s.modded, "{row:?}: the press is over");
                let tap = act == T;
                assert_eq!(s.last_alone_up, if tap { 10_100 | 1 } else { 0 }, "{row:?}: only a tap is a first tap");
            }
        }
    }

    #[test]
    fn win_alt_then_release_is_no_menu() {
        let mut s = WinState::default();
        win_step(&mut s, owned(LWIN, true, 10_000));
        assert_eq!(sam(win_step(&mut s, with_mods(LALT, true, 10_050, Mods::ALT))), (false, WinAct::None, false));
        let out = win_step(&mut s, with_mods(LWIN, false, 10_100, Mods::ALT));
        assert_eq!((sam(out), out.peek_end), ((true, WinAct::None, true), false), "swallowed with the mask");
        assert_eq!(sam(win_step(&mut s, owned(LALT, false, 10_150))), (false, WinAct::None, false), "Alt up passes");
    }

    // ---- Peek (dock-keys H3)

    /// The state after a Win down at 10,000 with every row owned and the Peek timer's fire (`peek_due` held).
    fn peeking() -> WinState {
        let mut s = WinState::default();
        let out = win_step(&mut s, owned(LWIN, true, 10_000));
        assert_eq!(out.peek, PeekCmd::Arm(10_000));
        assert!(peek_due(&s, 10_000, false));
        s.peeking = true;
        s
    }

    #[test]
    fn peek_commands_on_down_and_up() {
        let peek_digits = DockChords::PEEK.with(DockChords::DIGITS);
        let arm = |chords: DockChords, fullscreen: bool| {
            let mut s = WinState::default();
            win_step(&mut s, WinIn { chords, fullscreen, ..legacy(LWIN, true, 10_000) }).peek
        };
        assert_eq!(arm(ALL_ROWS, false), PeekCmd::Arm(10_000));
        assert_eq!(arm(peek_digits, false), PeekCmd::Arm(10_000));
        assert_eq!(arm(ALL_ROWS, true), PeekCmd::Kill, "a fullscreen window is in front");
        assert_eq!(arm(DockChords::PEEK, false), PeekCmd::Kill, "no digits: no chips to show");
        assert_eq!(arm(DockChords(ALL_ROWS.0 & !DockChords::PEEK.0), false), PeekCmd::Kill);
        assert_eq!(arm(DockChords::NONE, false), PeekCmd::Kill);
        // Only a fresh down arms; auto-repeat keeps the timer.
        let mut s = WinState::default();
        assert_eq!(win_step(&mut s, owned(LWIN, true, 10_000)).peek, PeekCmd::Arm(10_000));
        assert_eq!(win_step(&mut s, owned(LWIN, true, 10_500)).peek, PeekCmd::Keep);
        assert_eq!(win_step(&mut s, owned(LWIN, true, 10_533)).peek, PeekCmd::Keep);
        // Kill on every Win up: the tracked press, the other Win key's, one with no press, with or without rows.
        assert_eq!(win_step(&mut s, owned(RWIN, false, 10_550)).peek, PeekCmd::Kill);
        assert_eq!(win_step(&mut s, owned(LWIN, false, 10_600)).peek, PeekCmd::Kill);
        assert_eq!(win_step(&mut s, owned(LWIN, false, 10_700)).peek, PeekCmd::Kill);
        assert_eq!(win_step(&mut s, legacy(RWIN, false, 10_800)).peek, PeekCmd::Kill);
        // A press that can no longer Peek kills its timer at once: a chord, a modifier, the pair.
        let mut s = WinState::default();
        win_step(&mut s, owned(LWIN, true, 20_000));
        assert_eq!(win_step(&mut s, owned(KEY_3, true, 20_100)).peek, PeekCmd::Kill);
        let mut s = WinState::default();
        win_step(&mut s, owned(LWIN, true, 30_000));
        assert_eq!(win_step(&mut s, with_mods(LSHIFT, true, 30_100, Mods::SHIFT)).peek, PeekCmd::Kill);
        let mut s = WinState::default();
        win_step(&mut s, owned(LWIN, true, 40_000));
        assert_eq!(win_step(&mut s, owned(KEY_E, true, 40_100)).peek, PeekCmd::Kill);
        // Other keys leave it alone.
        assert_eq!(win_step(&mut WinState::default(), owned(KEY_E, true, 50_000)).peek, PeekCmd::Keep);
    }

    #[test]
    fn peek_due_table() {
        let mut held = WinState::default();
        win_step(&mut held, owned(LWIN, true, 10_000));
        win_step(&mut held, owned(LWIN, true, 10_533));
        assert!(peek_due(&held, 10_000, false), "Win held alone, auto-repeat included");
        assert!(!peek_due(&held, 10_000, true), "fullscreen");
        assert!(!peek_due(&held, 9_000, false), "a timer of an earlier press");
        let cases = [
            ("released", WinState { down: false, ..held }),
            ("pair injected", WinState { other: true, ..held }),
            ("chord owned", WinState { owned: true, ..held }),
            ("modifier passed", WinState { modded: true, ..held }),
        ];
        for (what, s) in cases {
            assert!(!peek_due(&s, 10_000, false), "{what}");
        }
        // Released and pressed again: the new press has its own timer.
        let mut s = held;
        win_step(&mut s, owned(LWIN, false, 10_590));
        win_step(&mut s, owned(LWIN, true, 10_595));
        assert!(!peek_due(&s, 10_000, false) && peek_due(&s, 10_595, false));
    }

    #[test]
    fn peek_ends_on_every_win_up() {
        // Alone: swallowed, no toggle, Peek ends.
        let mut s = peeking();
        let out = win_step(&mut s, owned(LWIN, false, 10_900));
        assert_eq!((ans(out), out.peek_end, out.peek), (SILENT, true, PeekCmd::Kill));
        assert_eq!(s.last_alone_up, 0, "no first tap");
        // After a chord during Peek (Win+3 while the chips show).
        let mut s = peeking();
        assert_eq!(win_step(&mut s, owned(KEY_3, true, 10_700)).act, WinAct::Chord(ChordAction::App(2)));
        assert!(s.peeking, "the chips stay while Win is held");
        win_step(&mut s, owned(KEY_3, false, 10_750));
        let out = win_step(&mut s, owned(LWIN, false, 10_900));
        assert_eq!((ans(out), out.peek_end), (SILENT, true));
        // After a modifier during Peek, Alt still down: the mask too.
        let mut s = peeking();
        win_step(&mut s, with_mods(LALT, true, 10_700, Mods::ALT));
        let out = win_step(&mut s, with_mods(LWIN, false, 10_900, Mods::ALT));
        assert_eq!((sam(out), out.peek_end), ((true, WinAct::None, true), true));
        // A fresh down after a lost release ends the old Peek and arms the new one.
        let mut s = peeking();
        let out = win_step(&mut s, owned(LWIN, true, 12_000));
        assert_eq!((out.peek_end, out.peek, s.peeking), (true, PeekCmd::Arm(12_000), false));
        // The stale rule (a lost release, then a key): the key passes and Peek ends.
        let mut s = peeking();
        let out = win_step(&mut s, owned(KEY_E, true, 10_000 + REPEAT_WINDOW_MS + 1));
        assert_eq!((ans(out), out.peek_end, s.peeking), (PASSED, true, false));
    }

    #[test]
    fn injection_during_peek_ends_it_and_the_win_up_passes() {
        let mut s = peeking();
        let out = win_step(&mut s, owned(KEY_E, true, 10_700));
        assert_eq!((ans(out), out.peek_end, out.peek), ((true, WinAct::Inject, false, COMBO), true, PeekCmd::Kill));
        assert!(!s.peeking);
        assert_eq!(ans(win_step(&mut s, owned(KEY_E, false, 10_750))), PASSED);
        let out = win_step(&mut s, owned(LWIN, false, 10_800));
        assert_eq!((ans(out), out.peek_end), (PASSED, false), "Windows saw the injected Win down; Peek already ended");
    }

    #[test]
    fn stale_repair_clears_the_chord_state() {
        let mut s = peeking();
        win_step(&mut s, owned(KEY_3, true, 10_700));
        win_step(&mut s, with_mods(LSHIFT, true, 10_750, Mods::SHIFT));
        assert!(s.down && s.owned && s.modded && s.peeking && s.chord_keys != 0);
        let kept = (s.vk, s.last_time, s.press);
        assert!(clear_for_repair(&mut s), "Peek was on: its end is posted");
        assert_eq!(
            (s.down, s.other, s.owned, s.modded, s.peeking, s.chord_keys),
            (false, false, false, false, false, 0)
        );
        assert_eq!((s.double, s.last_alone_up), (false, 0));
        assert_eq!((s.vk, s.last_time, s.press), kept);
        assert!(!clear_for_repair(&mut s), "nothing to end the second time");
        // After the repair the held digit's up passes (its bit is gone) and the next press starts fresh.
        assert_eq!(ans(win_step(&mut s, owned(KEY_3, false, 30_000))), PASSED);
        assert_eq!(ans(win_step(&mut s, owned(LWIN, true, 30_100))), PRESS);
    }

    // ---- cut-offs: the secure desktop, a lock, sleep, a removed hook

    const KEY_2: u32 = 0x32;
    const KEY_T: u32 = 0x54;
    const KEY_B: u32 = 0x42;

    /// Win+Shift+3 starts an app that needs elevation: the UAC prompt's desktop takes Win's and Shift's releases. The
    /// desktop switch is a cut-off: the press is forgotten, so 2, t and b typed in the elevated app right after are plain
    /// keys, never Win+2 (switch to app 2), Win+T or Win+B (the bar takes the keyboard). The 3 keeps its bit: its up, if
    /// it still comes, is swallowed like its down was.
    #[test]
    fn a_cut_off_forgets_the_win_press() {
        let mut s = WinState::default();
        win_step(&mut s, owned(LWIN, true, 10_000));
        win_step(&mut s, with_mods(LSHIFT, true, 10_050, Mods::SHIFT));
        let out = win_step(&mut s, with_mods(KEY_3, true, 10_100, Mods::SHIFT));
        assert_eq!(out.act, WinAct::Chord(ChordAction::NewInstance(2)));
        assert_eq!(forget_after_cut_off(&mut s, ALL_ROWS), Some(false));
        assert_eq!((s.down, s.other, s.owned, s.modded, s.last_alone_up), (false, false, false, false, 0));
        let mut t = 12_000;
        for vk in [KEY_2, KEY_T, KEY_B, KEY_4] {
            assert_eq!(ans(win_step(&mut s, owned(vk, true, t))), PASSED, "{vk:#x} typed after the prompt");
            assert_eq!(ans(win_step(&mut s, owned(vk, false, t + 50))), PASSED);
            t += 100;
        }
        assert_eq!(ans(win_step(&mut s, owned(KEY_3, false, t))), SILENT, "the chord key's up is still swallowed");
        assert_eq!(ans(win_step(&mut s, owned(LWIN, false, t + 50))), PASSED, "no tracked press");
        // Peek on at the cut-off: it ends.
        let mut s = peeking();
        assert_eq!(forget_after_cut_off(&mut s, ALL_ROWS), Some(true));
        assert!(!s.down && !s.peeking);
        // A plain press after the pair: Windows holds the injected Win, the physical Win up still passes.
        let mut s = WinState::default();
        win_step(&mut s, owned(LWIN, true, 20_000));
        win_step(&mut s, owned(KEY_E, true, 20_100));
        assert_eq!(forget_after_cut_off(&mut s, ALL_ROWS), Some(false));
        assert_eq!(ans(win_step(&mut s, owned(LWIN, false, 20_200))), PASSED);
    }

    /// The same cut-off forgets the tracked modifiers (the hook moves RESET_MODS when `forget_after_cut_off` answers,
    /// a wire pinned by `the_cut_off_is_wired_on_the_hook_thread`): a Shift released on the UAC prompt's desktop, then
    /// Alt+Tab, which renews the tracker's time, then Win+2 within MOD_STALE_MS. Without the reset the lost Shift picks
    /// Win+Shift+2 (a new instance) instead of Win+2. With rows owned the answer is Some even when no Win press is
    /// tracked.
    #[test]
    fn a_cut_off_forgets_the_tracked_modifiers() {
        let mut s = WinState::default();
        assert_eq!(forget_after_cut_off(&mut s, ALL_ROWS), Some(false), "rows owned: the modifiers are forgotten");
        let tracked = |reset: u32| {
            let mut t = ModTrack::default();
            track_step(&mut t, 0, LSHIFT, true, 10_000, |_| false);
            // The prompt; Shift's release went to its desktop.
            track_step(&mut t, reset, LALT, true, 13_000, |_| false);
            track_step(&mut t, reset, TAB, true, 13_050, |_| false);
            track_step(&mut t, reset, LALT, false, 13_100, |_| false);
            track_step(&mut t, reset, LWIN, true, 13_500, |_| false);
            t.mods(false)
        };
        let row = |mods: Mods| {
            let mut s = WinState::default();
            win_step(&mut s, with_mods(LWIN, true, 13_500, mods));
            win_step(&mut s, with_mods(KEY_2, true, 13_600, mods)).act
        };
        assert_eq!(row(tracked(0)), WinAct::Chord(ChordAction::NewInstance(1)), "no reset: the stale Shift's row");
        assert_eq!(row(tracked(1)), WinAct::Chord(ChordAction::App(1)), "the cut-off's reset: Win+2");
    }

    /// With no row owned and a press that is not the dock's, a cut-off changes nothing: release 1.5.2's Win branch keeps
    /// its own stale rule. A press the dock owned before its rows went (a remote client came to the front) is forgotten.
    #[test]
    fn a_cut_off_without_rows_leaves_the_1_5_2_branch_alone() {
        let mut s = WinState::default();
        win_step(&mut s, legacy(LWIN, true, 10_000));
        win_step(&mut s, legacy(KEY_E, true, 10_100));
        let before = s;
        assert_eq!(forget_after_cut_off(&mut s, DockChords::NONE), None);
        assert_eq!(s, before);
        let mut s = WinState::default();
        win_step(&mut s, legacy(LWIN, true, 10_000));
        assert_eq!(forget_after_cut_off(&mut s, DockChords::NONE), None, "a lone press");
        let mut s = after_win_3();
        assert_eq!(forget_after_cut_off(&mut s, DockChords::NONE), Some(false), "an owned press");
        assert!(!s.down);
        let mut s = WinState::default();
        assert_eq!(forget_after_cut_off(&mut s, DockChords::NONE), None, "nothing at all");
    }

    /// A reinstall whose UnhookWindowsHookEx fails found the hook already removed by Windows (a callback overran
    /// LowLevelHooksTimeout): keys went by unseen, so it is a cut-off. Pinned on the source: `reinstall` cannot run here.
    #[test]
    fn a_reinstall_that_finds_the_hook_removed_is_a_cut_off() {
        let src = include_str!("hook.rs");
        let body = src.find("fn reinstall() {").map(|at| &src[at..]).unwrap();
        let body = &body[..body.find("\nfn uninstall()").unwrap()];
        let unhook = body.find("UnhookWindowsHookEx(HHOOK(old as *mut _)) }.is_err()").unwrap();
        let cut = body.find("cut_off_now(CutOff::HookRemoved)").unwrap();
        assert!(unhook < cut, "the failed unhook of the old hook calls the cut-off");
        // The note's code round-trips for every cut-off; a variant left out of ALL would lose its note.
        for why in CutOff::ALL {
            assert_eq!(CutOff::from_code(why as isize), Some(why));
        }
        assert_eq!(CutOff::from_code(CutOff::WindowAbove as isize), Some(CutOff::WindowAbove));
        assert_eq!(CutOff::from_code(-1), None);
        assert_eq!(CutOff::from_code(CutOff::ALL.len() as isize), None);
    }

    /// A reinstall swaps the hook while keys come: a key Windows handed to the old hook just before the new one went
    /// in waits in this thread's queue, and a hook removed before the thread pumps never answers it, so Windows passes
    /// the key on unseen. A Win auto-repeat of a press the hook swallows then gives Windows a Win down whose release
    /// the hook swallows too: Windows holds Win after it is released. The old hook answers the calls already sent to it
    /// before it goes; every key after the install reaches the new one. Pinned on the source: no hook installs here.
    #[test]
    fn a_reinstall_answers_the_keys_sent_to_the_old_hook_before_removing_it() {
        let src = include_str!("hook.rs");
        let order = |b: &str, x: &str, y: &str| matches!((b.find(x), b.find(y)), (Some(i), Some(j)) if i < j);
        let reinstall = source_block(src, "fn reinstall()");
        let answer = "answer_sent_keys();";
        let unhook = "UnhookWindowsHookEx(HHOOK(old as *mut _))";
        let drained = src.contains("fn answer_sent_keys()").then(|| source_block(src, "fn answer_sent_keys()"));
        let peek = "PeekMessageW(&mut msg, None, 0, 0, PM_NOREMOVE | PM_QS_SENDMESSAGE)";
        let wires = [
            ("reinstall: the new hook in before the old hook answers", order(&reinstall, "match set_hook() {", answer)),
            ("reinstall: the old hook answers before it is removed", order(&reinstall, answer, unhook)),
            ("answer_sent_keys: the sent calls only, nothing posted taken", drained.is_some_and(|b| b.contains(peek))),
        ];
        let missing: Vec<&str> = wires.iter().filter(|(_, ok)| !ok).map(|(what, _)| *what).collect();
        assert!(missing.is_empty(), "unwired: {missing:?}");
    }

    // ---- a Win key-down that went by the hook unseen (a reinstall, a time-out)

    /// One event as `win_key` runs it: `note_release` (Windows holding the key: `held`, None when the key state must not
    /// be read, which panics if it is), then `win_step`.
    fn step_seen(s: &mut WinState, ev: WinIn, held: Option<bool>) -> WinOut {
        note_release(s, ev, |_| held.expect("the key state is read only at the release of a tracked press"));
        win_step(s, ev)
    }

    /// The state after a lone Win down at 10,000 with every row owned.
    fn lone_press() -> WinState {
        let mut s = WinState::default();
        assert_eq!(ans(step_seen(&mut s, owned(LWIN, true, 10_000), None)), PRESS);
        s
    }

    /// The live failure: a key-down of a tracked press went by the hook unseen (a reinstall swapped the hook, or Windows
    /// timed it out), so Windows holds Win while the hook believes it swallowed the press. At its release, with a row
    /// owned and no pair injected, the release reaches Windows behind the menu-mask key (no Start menu) and the press
    /// ends with no tap; a Peek on ends. Without it the release is swallowed and Windows keeps Win down: every later key
    /// is Win+key. Read at the release (a release between two ticks) or noted by the tick: either is enough.
    #[test]
    fn a_release_windows_holds_passes_behind_the_mask() {
        let cases = [
            ("lone press", lone_press(), false),
            ("peeking", peeking(), true),
            ("chord owned", after_win_3(), false),
            ("modifier passed", WinState { modded: true, ..lone_press() }, false),
            ("second press of a double tap", WinState { double: true, ..lone_press() }, false),
        ];
        for (what, before, peek_end) in cases {
            for mods in [Mods::NONE, Mods::ALT] {
                let mut s = before;
                let out = step_seen(&mut s, with_mods(LWIN, false, 12_000, mods), Some(true));
                assert_eq!(
                    (out.swallow, out.act, out.mask, out.peek_end, out.note),
                    (false, WinAct::None, true, peek_end, Some(EV_LEAK)),
                    "{what}, mods {}",
                    mods.0
                );
                assert_eq!(out.peek, PeekCmd::Kill, "{what}");
                assert!(!s.down && !s.leaked && !s.peeking && !s.owned && !s.modded, "{what}: the press is over");
                assert_eq!((s.last_alone_up, s.double), (0, false), "{what}: no tap, so no first tap of a double");
            }
        }
        // The watchdog's note alone: a release read as up still passes, masked.
        let mut s = WinState { leaked: true, ..lone_press() };
        let out = step_seen(&mut s, owned(LWIN, false, 12_000), Some(false));
        assert_eq!((out.swallow, out.act, out.mask, out.note), (false, WinAct::None, true, Some(EV_LEAK)));
        // Windows does not hold it and nothing was noted: the answers of before.
        let mut s = lone_press();
        let out = step_seen(&mut s, owned(LWIN, false, 12_000), Some(false));
        assert_eq!((out.swallow, out.act, out.mask), (true, WinAct::Toggle, false));
        let mut s = after_win_3();
        assert_eq!(ans(step_seen(&mut s, owned(LWIN, false, 12_000), Some(false))), SILENT);
    }

    /// Rule I runs only at the release of a tracked press with no pair and a row owned: with no row the key state is
    /// never read and every answer is release 1.5.2's, a note included; with rows, a down, the other Win's up and a
    /// pair's release read nothing and answer as before.
    #[test]
    fn a_release_reads_the_key_state_only_with_a_row_and_no_pair() {
        let mut s = WinState::default();
        step_seen(&mut s, legacy(LWIN, true, 10_000), None);
        let out = step_seen(&mut s, legacy(LWIN, false, 10_100), None);
        assert_eq!((out.swallow, out.act, out.mask), (true, WinAct::Toggle, false), "no row");
        let mut s = WinState::default();
        step_seen(&mut s, legacy(LWIN, true, 20_000), None);
        s.leaked = true;
        let out = step_seen(&mut s, legacy(LWIN, false, 20_100), None);
        assert_eq!((out.swallow, out.act, out.mask), (true, WinAct::Toggle, false), "no row, noted");
        let mut s = lone_press();
        assert_eq!(ans(step_seen(&mut s, owned(RWIN, false, 10_050), None)), PASSED, "the other Win's up");
        assert!(s.down, "the press goes on");
        assert_eq!(step_seen(&mut s, owned(KEY_L, true, 10_100), None).act, WinAct::Inject);
        step_seen(&mut s, owned(KEY_L, false, 10_150), None);
        let out = step_seen(&mut s, owned(LWIN, false, 10_200), None);
        assert_eq!((out.swallow, out.mask, out.note), (false, false, None), "a pair's release passes as before");
    }

    /// Rule I's wires, pinned on the source (the callback cannot run here): `win_key` reads the key state through
    /// `note_release` before the step, and sends the menu-mask key the step asks for, before the up passes; the UI logs
    /// the release. Every missing wire is named.
    #[test]
    fn the_release_rule_is_wired() {
        let src = include_str!("hook.rs");
        let order = |b: &str, x: &str, y: &str| matches!((b.find(x), b.find(y)), (Some(i), Some(j)) if i < j);
        let key = source_block(src, "fn win_key(");
        let read = "note_release(&mut s, ev, |vk| unsafe { GetAsyncKeyState(vk as i32) } < 0);";
        let wires = [
            ("win_key: the key state read before the step", order(&key, read, "let out = win_step(&mut s, ev);")),
            ("win_key: the step's mask sent", key.contains("if out.mask { send_mask(); }")),
            ("main: the release's line", include_str!("main.rs").contains("hook::EV_LEAK => log!(")),
        ];
        let missing: Vec<&str> = wires.iter().filter(|(_, ok)| !ok).map(|(what, _)| *what).collect();
        assert!(missing.is_empty(), "unwired: {missing:?}");
    }

    /// The watchdog's tick: Windows holds the Win key of a tracked press with no pair injected. The press
    /// notes it (its release then passes, `win_up_plan`), with one line per press; the other Win key, a pair's press or
    /// no press is no leak. Every path that ends the press drops the note, so no later press inherits it.
    #[test]
    fn the_watchdog_notes_a_held_win_once_per_press() {
        let held = WinState { down: true, vk: LWIN, last_time: 10_000, press: 10_000, ..WinState::default() };
        let table = [
            ("Windows holds the press's key", held, true, false, true),
            ("Windows holds both Win keys", held, true, true, true),
            ("Windows holds the other Win key only", held, false, true, false),
            ("Windows holds no Win key", held, false, false, false),
            ("a pair injected", WinState { other: true, ..held }, true, false, false),
            ("no press", WinState { down: false, ..held }, true, true, false),
            ("a right Win press", WinState { vk: RWIN, ..held }, false, true, true),
        ];
        for (what, before, lwin, rwin, noted) in table {
            let mut s = before;
            assert_eq!(note_leak(&mut s, lwin, rwin), noted, "{what}: the line");
            assert_eq!(s, WinState { leaked: noted, ..before }, "{what}: the note, and nothing else");
            assert!(!note_leak(&mut s, lwin, rwin), "{what}: one line per press");
        }
        let noted = WinState { leaked: true, ..held };
        let mut s = noted;
        win_step(&mut s, owned(LWIN, true, 10_000 + REPEAT_WINDOW_MS + 1));
        assert!(s.down && !s.leaked, "a new press");
        let mut s = noted;
        win_step(&mut s, legacy(LWIN, false, 10_100));
        assert!(!s.down && !s.leaked, "the release, no row owned");
        let mut s = noted;
        win_step(&mut s, owned(KEY_L, true, 10_000 + REPEAT_WINDOW_MS + 1));
        assert!(!s.down && !s.leaked, "the stale rule");
        let mut s = noted;
        clear_for_repair(&mut s);
        assert!(!s.down && !s.leaked, "the watchdog's repair");
        let mut s = noted;
        assert_eq!(forget_after_cut_off(&mut s, ALL_ROWS), Some(false));
        assert!(!s.down && !s.leaked, "a cut-off");
    }

    /// The tick's wires, pinned on the source (`reinstall` cannot run here): the tick notes the press and writes its
    /// one line before the skip that leaves the hook alone while Windows holds a Win key, storing the note on the hook
    /// thread with no pump in between. The note reaches the release only through the statics: `store_win_state` keeps
    /// it and `load_win_state` reads it back for the callback's step. Every missing wire is named.
    #[test]
    fn the_watchdog_note_is_wired() {
        let src = include_str!("hook.rs");
        let order = |b: &str, x: &str, y: &str| matches!((b.find(x), b.find(y)), (Some(i), Some(j)) if i < j);
        let reinstall = source_block(src, "fn reinstall()");
        let noted = "let mut press = load_win_state(); if note_leak(&mut press, l, r) { store_win_state(&press); log!(";
        let load = source_block(src, "fn load_win_state()");
        let store = source_block(src, "fn store_win_state(");
        let wires = [
            ("reinstall: the note and its line", reinstall.contains(noted)),
            ("reinstall: the note before the skip", order(&reinstall, noted, "Repair::Skip => return,")),
            ("load_win_state: the note read back", load.contains("leaked: WIN_LEAKED.load(Relaxed),")),
            ("store_win_state: the note kept", store.contains("WIN_LEAKED.store(s.leaked, Relaxed);")),
        ];
        let missing: Vec<&str> = wires.iter().filter(|(_, ok)| !ok).map(|(what, _)| *what).collect();
        assert!(missing.is_empty(), "unwired: {missing:?}");
    }

    /// A key seen twice: between a reinstall's install and its unhook (`answer_sent_keys`) a key the new hook passes reaches the
    /// old hook too (CallNextHookEx), so the callback sees it twice. Every key the Win branch passes, from every kind of
    /// press, with no row, with rows and failed open, in time and after the stale window: the second look passes it,
    /// changes no state and adds nothing the first did not do (the menu-mask key of an Alt up and the Sync post of a
    /// failed-open chord repeat, both harmless). A swallowed key never reaches the old hook, and the dock's own tagged
    /// injections are no user key (`accepted`). The launcher's rules too: a key they pass posts nothing more.
    #[test]
    fn a_passed_key_seen_twice_changes_nothing_more() {
        let lone = lone_press();
        let starts = [
            ("idle", WinState::default()),
            ("lone press", lone),
            ("pair injected", WinState { other: true, ..lone }),
            ("chord owned", after_win_3()),
            ("modifier passed", WinState { modded: true, ..lone }),
            ("peeking", peeking()),
            ("noted", WinState { leaked: true, ..lone }),
        ];
        let stuck = |vk, down, t| WinIn { fail_open: true, ..owned(vk, down, t) };
        let ins: [fn(u32, bool, u32) -> WinIn; 3] = [legacy, owned, stuck];
        let mut passed = 0;
        for (what, start) in starts {
            for vk in [LWIN, RWIN, KEY_3, KEY_L, LALT, LSHIFT] {
                for (down, t, mods, i) in [true, false]
                    .into_iter()
                    .flat_map(|d| [10_100, 10_000 + REPEAT_WINDOW_MS + 1].map(|t| (d, t)))
                    .flat_map(|(d, t)| [Mods::NONE, Mods::ALT].map(|m| (d, t, m)))
                    .flat_map(|(d, t, m)| [0, 1, 2].map(|i| (d, t, m, i)))
                {
                    let ev = WinIn { mods, ..ins[i](vk, down, t) };
                    let mut once = start;
                    let first = win_step(&mut once, ev);
                    if first.swallow {
                        continue;
                    }
                    let mut twice = once;
                    let second = win_step(&mut twice, ev);
                    let row = format!("{what}: vk {vk:#x} down {down} t {t} mods {} in {i}", mods.0);
                    assert_eq!(twice, once, "{row}: the state changed again");
                    assert!(!second.swallow, "{row}: swallowed the second time");
                    assert_eq!((second.act, second.note, second.peek_end), (WinAct::None, None, false), "{row}");
                    assert!(!matches!(second.peek, PeekCmd::Arm(_)), "{row}: Peek armed again");
                    assert!(!second.mask || first.mask, "{row}: a mask the first look did not send");
                    assert!(!second.sync || first.sync, "{row}: a Sync the first look did not post");
                    passed += 1;
                }
            }
        }
        assert!(passed > 100, "{passed} passed keys looked at");
        let alt = |flags: u8| ChordState { flags: PassFlags(flags), mods: Mods::ALT, ..st() };
        let cases = [
            ("fullscreen", alt(PassFlags::FULLSCREEN.0), SPACE, true),
            ("remote", alt(PassFlags::REMOTE.0), SPACE, true),
            ("yielding", alt(PassFlags::YIELD.0), SPACE, true),
            ("passed press", ChordState { space: SpaceState::Passed, space_time: 10_000, ..alt(0) }, SPACE, false),
            ("plain key", st(), KEY_A, true),
            ("modifier", st(), LSHIFT, true),
        ];
        for (what, start, vk, down) in cases {
            let ring = Ring::new();
            let mut once = start;
            let (swallow, _) = chord_step(&mut once, &ring, ev(vk, down, 10_100));
            assert_ne!(swallow, Some(true), "{what}: passed");
            let mut twice = once;
            let (again, act) = chord_step(&mut twice, &ring, ev(vk, down, 10_100));
            assert_ne!(again, Some(true), "{what}: swallowed the second time");
            assert_eq!(act, Act::Nothing, "{what}: posted again");
            assert_eq!(twice, once, "{what}: the state changed again");
        }
    }

    /// Pinned on the source (`reinstall` cannot run here): owner-probe builds write one line per successful
    /// reinstall, with its tick count on kb.time's clock, so a live check matches a key to a reinstall; release builds
    /// have no such line (the reinstall runs every 2 s while keys come). Every missing wire is named.
    #[test]
    fn a_probe_build_logs_each_reinstall() {
        let src = include_str!("hook.rs");
        let reinstall = source_block(src, "fn reinstall()");
        let ok = source_block(&reinstall, "Ok(new) =>");
        let err = reinstall.find("Err(e) => log!(\"hook watchdog: reinstall failed: {e}\")").map(|at| &reinstall[at..]);
        let call = "#[cfg(feature = \"owner-probe\")] reinstalled_line(); }";
        let line = flat("#[cfg(feature = \"owner-probe\")] fn reinstalled_line() {");
        let body = src.contains("fn reinstalled_line()").then(|| source_block(src, "fn reinstalled_line()"));
        let tick = "log!(\"hook: reinstalled at tick {tick}\")";
        let wires = [
            ("reinstall: the line after the unhook", ok.ends_with(call)),
            ("reinstall: no line for a failed install", err.is_some_and(|e| !e.contains("reinstalled_line"))),
            ("reinstalled_line: probe builds only", flat(src).contains(&line)),
            ("reinstalled_line: the tick count", body.is_some_and(|b| b.contains(tick))),
        ];
        let missing: Vec<&str> = wires.iter().filter(|(_, ok)| !ok).map(|(what, _)| *what).collect();
        assert!(missing.is_empty(), "unwired: {missing:?}");
    }

    /// `s` with its whitespace folded to single spaces.
    fn flat(s: &str) -> String {
        s.split_whitespace().collect::<Vec<_>>().join(" ")
    }

    /// The block that follows `head` in `src` (its first `{` to the matching `}`), comment lines dropped and the
    /// whitespace folded (`flat`): the source pins of wires a test cannot run. A comment satisfies no pin.
    fn source_block(src: &str, head: &str) -> String {
        let code = src.lines().filter(|l| !l.trim_start().starts_with("//")).collect::<Vec<_>>().join("\n");
        let at = code.find(head).unwrap_or_else(|| panic!("{head}: not in the source")) + head.len();
        let open = at + code[at..].find('{').unwrap();
        let mut depth = 0;
        for (i, c) in code[open..].char_indices() {
            depth += match c {
                '{' => 1,
                '}' => -1,
                _ => 0,
            };
            if depth == 0 {
                return flat(&code[open..=open + i]);
            }
        }
        panic!("{head}: the block never ends");
    }

    /// The hook thread's cut-off wires, pinned on the source (a desktop switch, a thread note and `cut_off_now` cannot
    /// run here): once `forget_after_cut_off` answers, `cut_off_now` stores the forgotten press, posts the end of a
    /// Peek that was on (without the post the digit chips stay up after a lock or a UAC prompt: no later Win event of
    /// that press finds Peek on) and resets the tracked modifiers (without the reset a Shift released on the UAC
    /// prompt's desktop turns Win+2 into Win+Shift+2); the thread registers EVENT_SYSTEM_DESKTOPSWITCH out of context
    /// before it pumps, and that event's callback is the desktop switch's cut-off; `cut_off` posts the note and the
    /// loop hands it to `cut_off_now`. Every missing wire is named.
    #[test]
    fn the_cut_off_is_wired_on_the_hook_thread() {
        let src = include_str!("hook.rs");
        let now = source_block(src, "fn cut_off_now(why: CutOff)");
        let forget = now.find("let Some(peek_end) = forget_after_cut_off(&mut s, rows) else { return };");
        let after = |what: &str| matches!((forget, now.find(what)), (Some(a), Some(b)) if a < b);
        let main = source_block(src, "fn thread_main(");
        let desk = flat(
            "SetWinEventHook( EVENT_SYSTEM_DESKTOPSWITCH, EVENT_SYSTEM_DESKTOPSWITCH, None, Some(desktop_switched),
             0, 0, WINEVENT_OUTOFCONTEXT, )",
        );
        let pumped = matches!((main.find(&desk), main.find("GetMessageW(")), (Some(a), Some(b)) if a < b);
        let note = source_block(&main, "else if msg.wParam.0 == NOTE_CUT_OFF");
        let switched = source_block(src, "fn desktop_switched(");
        let post = "PostThreadMessageW(tid, WM_HOOK_NOTE, WPARAM(NOTE_CUT_OFF), LPARAM(why as isize))";
        let wires = [
            ("cut_off_now: the press stored after forget_after_cut_off", after("store_win_state(&s);")),
            ("cut_off_now: a Peek on is ended", after("if peek_end { post_peek(false, unsafe { GetTickCount() }); }")),
            ("cut_off_now: reset_modifiers after forget_after_cut_off", after("reset_modifiers();")),
            ("thread_main: the desktop-switch WinEvent hook before the pump", pumped),
            (
                "desktop_switched: the desktop switch's cut-off",
                switched.contains("cut_off_now(CutOff::DesktopSwitch);"),
            ),
            ("cut_off: posts NOTE_CUT_OFF", source_block(src, "pub fn cut_off(why: CutOff)").contains(post)),
            (
                "thread_main: NOTE_CUT_OFF reaches cut_off_now",
                note.contains("CutOff::from_code(msg.lParam.0) { cut_off_now(why); }"),
            ),
        ];
        let missing: Vec<&str> = wires.iter().filter(|(_, ok)| !ok).map(|(what, _)| *what).collect();
        assert!(missing.is_empty(), "unwired: {missing:?}");
    }

    /// The UI's wires of the cut-offs and of the rows for the window in front, pinned on ui.rs and launcher_sys.rs
    /// (window messages and the window in front cannot run here): the power and session arms of `handle_message` hand
    /// `cut_off_of`'s answer to `cut_off`; the foreground arm runs `chords_follow_foreground`, which reads the window
    /// in front (`front_of` of its process's image, an open denied being `Refused`), hands the hook the rows for it and
    /// hands the hook the cut-off of the change from the last look (`front_cut_off`); `chords_sync` (the start and each
    /// config reload) stores the config's rows, then goes through `chords_follow_foreground` too and never hands the
    /// hook the rows itself (they would reach a remote client in front); owning a row watches the session events. Every
    /// missing wire is named.
    #[test]
    fn the_ui_wires_the_cut_offs_and_the_remote_rows() {
        let src = include_str!("ui.rs");
        let sys = include_str!("launcher_sys.rs");
        let cut = "if let Some(why) = cut_off_of(msg, wp.0) { crate::hook::cut_off(why); }";
        let foreground = source_block(src, "if wp.0 as u32 == EVENT_SYSTEM_FOREGROUND");
        let follow = source_block(src, "fn chords_follow_foreground()");
        let front = "let front = if rows == hook::DockChords::NONE { Front::Local } else { \
                     crate::launcher_sys::with_foreground_image(front_of) };";
        let was = "let was = FRONT.with(|f| f.replace(front));";
        let changed = "if let Some(why) = front_cut_off(was, front) { hook::cut_off(why); }";
        let sync = source_block(src, "fn chords_sync()");
        let set = sync.find("CHORDS.set(chords);");
        let follows = matches!((set, sync.find("chords_follow_foreground();")), (Some(a), Some(b)) if a < b);
        let owned = source_block(&sync, "if chords != hook::DockChords::NONE");
        let image = source_block(sys, "fn with_image<R>(");
        let pid = source_block(sys, "pub(crate) fn with_foreground_image<R>(");
        let wires = [
            ("WM_POWERBROADCAST: hook::cut_off", source_block(src, "WM_POWERBROADCAST =>").contains(cut)),
            ("WM_WTSSESSION_CHANGE: hook::cut_off", source_block(src, "WM_WTSSESSION_CHANGE =>").contains(cut)),
            ("EVENT_SYSTEM_FOREGROUND: chords_follow_foreground", foreground.contains("chords_follow_foreground();")),
            ("chords_follow_foreground: the window in front", follow.contains(&flat(front))),
            ("chords_follow_foreground: the last look", follow.contains(was)),
            (
                "chords_follow_foreground: the rows",
                follow
                    .contains("hook::set_dock_chords(effective_chords(rows, front).with(takeover::chord_bits_now()));"),
            ),
            ("chords_follow_foreground: the cut-off of the change", follow.contains(changed)),
            ("with_foreground_image: the image of its process", pid.contains("with_image(pid, f)")),
            (
                "with_image: an open denied is Refused",
                image.contains("Err(e) if refused(e.code()) => return f(ProcessImage::Refused),"),
            ),
            ("chords_sync: a row watches the sessions", owned.contains("crate::launcher_sys::watch_sessions();")),
            ("chords_sync: the rows follow the window in front", follows && !sync.contains("hook::set_dock_chords(")),
        ];
        let missing: Vec<&str> = wires.iter().filter(|(_, ok)| !ok).map(|(what, _)| *what).collect();
        assert!(missing.is_empty(), "unwired: {missing:?}");
    }

    // ---- stall evidence: a hook thread that did not serve the keyboard in time is a cut-off

    const KEY_0: u32 = 0x30;
    const KEY_7: u32 = 0x37;
    const KEY_9: u32 = 0x39;

    /// A late return: a callback that returns more than STALL_MS after its entry kept the keyboard waiting, and Windows
    /// passed on the keys that came meanwhile (in a live session each key of a 5 s stall timed out after about 0.3 s
    /// and never reached the hook). The tick count wraps after 49.7 days.
    #[test]
    fn a_callback_that_returns_late_is_a_stall() {
        assert!(!overran(10_000, 10_000), "the usual callback, under one step of the tick count");
        assert!(!overran(10_000, 10_000 + STALL_MS));
        assert!(overran(10_000, 10_000 + STALL_MS + 1));
        assert!(overran(10_000, 10_000 + HOOK_TIMEOUT_MS), "Windows gave up on the key");
        assert!(overran(10_000, 15_000), "the owner-probe stall of 5 s");
        assert!(overran(u32::MAX - 100, STALL_MS), "across the wrap");
        assert!(!overran(u32::MAX - 100, 100), "across the wrap, in time");
    }

    /// The live session's stall with its cut-off: Win, then T (Win+T, owned), whose callback returns 5 s later; T's
    /// auto-repeat, T's up and Win's up time out meanwhile and never reach the hook. The late return is a cut-off, so
    /// the 9, 0 and 7 typed a second later are plain keys, where they were Win+9, Win+0 and Win+7 (App(8), App(9) and
    /// App(6) in the session's log). A fresh Win press right after works at once and owns its chords, Win+T included:
    /// T's bit, kept for an up that never came, has expired.
    #[test]
    fn a_stall_in_an_owned_chord_is_a_cut_off_and_the_next_press_works() {
        let mut s = WinState::default();
        assert_eq!(ans(win_step(&mut s, owned(LWIN, true, 10_000))), PRESS);
        let out = win_step(&mut s, owned(KEY_T, true, 10_047));
        assert_eq!(ans(out), (true, WinAct::Chord(ChordAction::FocusFirst), false, None));
        assert!(overran(10_047, 15_047), "the callback of T returns 5 s after its entry");
        let mut kept = s;
        let out = win_step(&mut kept, owned(KEY_9, true, 16_360));
        assert_eq!(out.act, WinAct::Chord(ChordAction::App(8)), "no cut-off: the 9 is Win+9");
        assert_eq!(forget_after_cut_off(&mut s, ALL_ROWS), Some(false));
        let mut t = 16_360;
        for vk in [KEY_9, KEY_0, KEY_7] {
            assert_eq!(ans(win_step(&mut s, owned(vk, true, t))), PASSED, "{vk:#x} typed after the stall");
            assert_eq!(ans(win_step(&mut s, owned(vk, false, t + 50))), PASSED);
            t += 160;
        }
        assert_eq!(ans(win_step(&mut s, owned(LWIN, true, 17_000))), PRESS, "a fresh press");
        let out = win_step(&mut s, owned(KEY_T, true, 17_100));
        assert_eq!(ans(out), (true, WinAct::Chord(ChordAction::FocusFirst), false, None));
        assert_eq!(ans(win_step(&mut s, owned(KEY_T, false, 17_150))), SILENT);
        assert_eq!(ans(win_step(&mut s, owned(LWIN, false, 17_200))), SILENT, "an owned press toggles nothing");
    }

    /// A late key: one that reaches the callback more than STALL_MS after its time stamp shows the hook thread was not
    /// serving the keyboard. A stamp ahead of the tick count (an injection with a made-up time, which the hook ignores
    /// anyway unless the owner-probe switch admits it) is not late. The tick count wraps.
    #[test]
    fn a_key_that_reaches_the_callback_late_is_a_stall() {
        assert!(!came_late(10_000, 10_000), "the usual key, at once");
        assert!(!came_late(10_000, 10_000 + STALL_MS));
        assert!(came_late(10_000, 10_000 + STALL_MS + 1));
        assert!(came_late(10_000, 10_297), "Windows gives up on such a key (about 0.3 s)");
        assert!(!came_late(10_001, 10_000), "a stamp 1 ms ahead");
        assert!(!came_late(20_000, 10_000), "a stamp 10 s ahead");
        assert!(came_late(u32::MAX - 100, STALL_MS), "across the wrap");
        assert!(!came_late(u32::MAX - 100, 100), "across the wrap, in time");
        // Only a key the hook admits counts: an injection of another app (no tag) and the dock's own (tagged) do not.
        assert!(!accepted(LLKHF_INJECTED.0, 0));
        assert!(!accepted(LLKHF_INJECTED.0, INJECT_TAG));
        assert!(accepted(0, 0));
    }

    /// A late tick: a heartbeat tick more than HEARTBEAT_MS + STALL_MS after the last one (or the arming) shows the
    /// hook thread did not run. A pause of the thread long enough for Windows to give up on a key (HOOK_TIMEOUT_MS,
    /// which the tick count can read one step short) makes the next tick that late even when the pause begins right
    /// after a tick; the usual tick, rounded up to the system timer's 15.6 ms steps, is far inside. The tick count
    /// wraps.
    #[test]
    fn a_late_heartbeat_is_a_stall() {
        assert!(!beat_late(10_000, 10_000 + HEARTBEAT_MS), "on time");
        assert!(!beat_late(10_000, 10_063), "rounded up to the system timer");
        assert!(!beat_late(10_000, 10_000 + HEARTBEAT_MS + STALL_MS));
        assert!(beat_late(10_000, 10_000 + HEARTBEAT_MS + STALL_MS + 1));
        assert!(beat_late(10_000, 10_000 + HOOK_TIMEOUT_MS + 1), "a pause just past the time-out, right after a tick");
        assert!(beat_late(10_000, 10_297), "the shortest time-out a live session measured");
        assert!(beat_late(10_000, 10_000 + HOOK_TIMEOUT_MS - TICK_STEP_MS), "a lost key, read one step short");
        assert!(beat_late(10_000, 15_000), "the owner-probe stall of 5 s");
        assert!(beat_late(u32::MAX - 100, HEARTBEAT_MS + STALL_MS), "across the wrap");
        assert!(!beat_late(u32::MAX - 10, HEARTBEAT_MS), "across the wrap, on time");
    }

    /// The heartbeat exists only while a Win press that owned a chord or passed a modifier is held, the only press
    /// whose lost release costs anything (its digits, T and B stay chords for up to LONE_STUCK_MS): it ends with the
    /// press, with a cut-off, with the watchdog's 10 s rule and with the stale rule. A lone press, the pair and every
    /// press without a row (release 1.5.2's branch) have none, so an idle dock gets no new wake-up.
    #[test]
    fn the_heartbeat_runs_only_while_a_dock_press_is_held() {
        let mut s = WinState::default();
        assert!(!heartbeat_due(&s), "nothing held");
        win_step(&mut s, owned(LWIN, true, 10_000));
        assert!(!heartbeat_due(&s), "a lone press");
        win_step(&mut s, owned(KEY_3, true, 10_100));
        assert!(heartbeat_due(&s), "Win+3 owned");
        win_step(&mut s, owned(KEY_3, false, 10_150));
        assert!(heartbeat_due(&s), "Win still held after the 3");
        win_step(&mut s, owned(LWIN, false, 10_200));
        assert!(!heartbeat_due(&s), "Win released");
        let mut s = WinState::default();
        win_step(&mut s, owned(LWIN, true, 20_000));
        win_step(&mut s, with_mods(LSHIFT, true, 20_050, Mods::SHIFT));
        assert!(heartbeat_due(&s), "Shift passed alone, Win pending");
        let mut s = WinState::default();
        win_step(&mut s, owned(LWIN, true, 30_000));
        win_step(&mut s, owned(KEY_E, true, 30_100));
        assert!(!heartbeat_due(&s), "Win+E, the pair");
        win_step(&mut s, owned(KEY_3, true, 30_200));
        assert!(heartbeat_due(&s), "then Win+3 owned");
        let mut cut = after_win_3();
        forget_after_cut_off(&mut cut, ALL_ROWS);
        assert!(!heartbeat_due(&cut), "a cut-off");
        let mut repaired = after_win_3();
        clear_for_repair(&mut repaired);
        assert!(!heartbeat_due(&repaired), "the watchdog's 10 s rule");
        let mut stale = after_win_3();
        win_step(&mut stale, owned(KEY_E, true, 10_100 + LONE_STUCK_MS + 1));
        assert!(!heartbeat_due(&stale), "the stale rule");
        let mut s = WinState::default();
        for (vk, t) in [(LWIN, 40_000), (KEY_3, 40_100), (LSHIFT, 40_200)] {
            win_step(&mut s, legacy(vk, true, t));
            assert!(!heartbeat_due(&s), "{vk:#x} with no row");
        }
    }

    /// The stall evidence's wires, pinned on the source (the callback, its thread message and the hook thread's loop
    /// cannot run here). The callback takes its entry time on the tick count before `handle_key` and judges it after,
    /// against the tick count at its return, before a swallowed key's answer, only while a row is owned
    /// (`stall_watch`), and notes a late return or a late key the hook admits; one note waits at a time, and a failed
    /// post leaves none pending; the loop hands it to `stalled`, which runs the cut-off, then reinstalls the hook at
    /// once and logs it back. The callback has the heartbeat follow the press while a row is owned, before the
    /// owner-probe stall can hold it. The heartbeat is armed once per press, its arming being the first tick's
    /// baseline, and killed once no longer due; killing it forgets it, so that the next press arms it again, and kills
    /// its timer. The loop knows a tick by the heartbeat's id, takes it as the next tick's baseline, judges it, hands a
    /// late one to `stalled` and ends a heartbeat no longer due; a cut-off and the watchdog's repair end it too. Every
    /// missing wire is named.
    #[test]
    fn the_stall_evidence_is_wired_on_the_hook_thread() {
        let src = include_str!("hook.rs");
        let order = |b: &str, x: &str, y: &str| matches!((b.find(x), b.find(y)), (Some(i), Some(j)) if i < j);
        let callback = source_block(src, "fn ll_keyboard_proc(");
        let judged = "if let Some(entered) = entered { stall_check(entered, kb); }";
        let answered = "if swallowed { return LRESULT(1); }";
        let watch = source_block(src, "fn stall_watch()");
        let no_row = "if DOCK_CHORDS.load(Acquire) == DockChords::NONE.0 { return None; }";
        let check = source_block(src, "fn stall_check(");
        let slow = "if overran(entered, left) { note_stall(CutOff::SlowCallback); }";
        let late_key = "else if accepted(kb.flags.0, kb.dwExtraInfo) && came_late(kb.time, entered) {";
        let note = source_block(src, "fn note_stall(why: CutOff)");
        let post = "PostThreadMessageW(GetCurrentThreadId(), WM_HOOK_NOTE, WPARAM(NOTE_STALL), LPARAM(why as isize))";
        let failed = "if posted.is_err() { STALL_NOTED.store(false, Relaxed); }";
        let main = source_block(src, "fn thread_main(");
        let arm = source_block(&main, "else if msg.wParam.0 == NOTE_STALL");
        let handed = "if let Some(why) = CutOff::from_code(msg.lParam.0) { stalled(why); }";
        let stalled = source_block(src, "fn stalled(why: CutOff)");
        let back = r#"log!("hook: reinstalled after a stall ({why:?})");"#;
        let key = source_block(src, "fn win_key(");
        let follows = "if chords != DockChords::NONE { heartbeat(heartbeat_due(&s)); }";
        let timer = "SetCoalescableTimer(None, 0, HEARTBEAT_MS, None, TIMERV_NO_COALESCING)";
        let heart = source_block(src, "fn heartbeat(on: bool)");
        let killed = "} else if !on && id != 0 { kill_heartbeat(); }";
        let kill = source_block(src, "fn kill_heartbeat()");
        let forgets = "let (id, _) = HEARTBEAT.with(|b| b.replace((0, 0)));";
        let kills = "if id != 0 { let _ = unsafe { KillTimer(None, id) }; }";
        let baseline = "let now = unsafe { GetTickCount() }; HEARTBEAT.with(|b| b.set((id, now)));";
        let beat_tick = flat(
            "let beat = HEARTBEAT.with(Cell::get); let beat_tick = msg.message == WM_TIMER && msg.hwnd.0.is_null()
             && beat.0 != 0 && msg.wParam.0 == beat.0;",
        );
        let tick = "if beat_tick { let now = unsafe { GetTickCount() }; HEARTBEAT.with(|b| b.set((beat.0, now)));";
        let late = "if beat_late(beat.1, now) { stalled(CutOff::LateHeartbeat); }";
        let ended = "heartbeat(heartbeat_due(&load_win_state()));";
        let cut = source_block(src, "fn cut_off_now(why: CutOff)");
        let forgot = "let Some(peek_end) = forget_after_cut_off(&mut s, rows) else { return };";
        let repair = source_block(src, "Repair::ClearThenGo =>");
        let wires = [
            ("ll_keyboard_proc: the entry time before handle_key", order(&callback, "= stall_watch();", "handle_key(")),
            ("ll_keyboard_proc: the check after handle_key", order(&callback, "handle_key(", judged)),
            ("ll_keyboard_proc: the check before a swallowed key's answer", order(&callback, judged, answered)),
            ("stall_watch: nothing without a row", watch.contains(no_row)),
            ("stall_watch: the entry on the tick count", watch.contains("Some(unsafe { GetTickCount() })")),
            ("stall_check: the return on the tick count", check.contains("let left = unsafe { GetTickCount() };")),
            ("stall_check: a late return", check.contains(slow)),
            ("stall_check: a late key the hook admits", order(&check, late_key, "note_stall(CutOff::LateKey);")),
            ("note_stall: one note at a time", note.contains("if STALL_NOTED.swap(true, Relaxed) { return; }")),
            ("note_stall: posts NOTE_STALL", note.contains(post)),
            ("note_stall: a failed post leaves none pending", note.contains(failed)),
            ("thread_main: NOTE_STALL reaches stalled", order(&arm, "STALL_NOTED.store(false, Relaxed);", handed)),
            ("stalled: the cut-off, then the reinstall", order(&stalled, "cut_off_now(why);", "reinstall();")),
            ("stalled: one line when the hook is back", order(&stalled, "reinstall();", back)),
            ("win_key: the heartbeat follows the press", order(&key, follows, "hook_stall_once")),
            ("heartbeat: a thread timer of HEARTBEAT_MS, not coalesced", heart.contains(timer)),
            ("heartbeat: the arming is the first tick's baseline", order(&heart, timer, baseline)),
            ("heartbeat: armed once per press", heart.contains("if on && id == 0 {")),
            ("heartbeat: killed once no longer due", heart.contains(killed)),
            ("kill_heartbeat: forgets the heartbeat", kill.contains(forgets)),
            ("kill_heartbeat: kills its timer", kill.contains(kills)),
            ("thread_main: a heartbeat tick is its timer's", main.contains(&beat_tick)),
            ("thread_main: a heartbeat tick is judged before the reinstall tick", order(&main, tick, "let tick_id =")),
            ("thread_main: each tick is the next one's baseline", order(&main, tick, late)),
            ("thread_main: a late tick reaches stalled", main.contains(late)),
            ("thread_main: a heartbeat no longer due ends", order(&main, late, ended)),
            ("cut_off_now: the heartbeat ends", order(&cut, forgot, "kill_heartbeat();")),
            ("reinstall: the 10 s rule ends the heartbeat", repair.contains("kill_heartbeat();")),
        ];
        let missing: Vec<&str> = wires.iter().filter(|(_, ok)| !ok).map(|(what, _)| *what).collect();
        assert!(missing.is_empty(), "unwired: {missing:?}");
    }
}
