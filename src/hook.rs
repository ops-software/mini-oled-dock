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
//! The callback does NO I/O nor heavy work (LL hook timeout ~300 ms):
//! it only posts messages to the window, which logs and acts.
//!
//! The hook lives in a dedicated thread (`hook`) that only runs a message loop: the callback
//! is served by that thread, independently of rendering, timers and logging of the UI thread.

use std::sync::atomic::Ordering::{AcqRel, Acquire, Relaxed, Release};
use std::sync::atomic::{AtomicBool, AtomicIsize, AtomicU32, AtomicU64, AtomicU8};
use std::sync::mpsc::{self, Receiver, Sender};
use std::thread::JoinHandle;
use std::time::Duration;

use windows::Win32::Foundation::{LPARAM, LRESULT, WPARAM};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::System::SystemInformation::GetTickCount;
use windows::Win32::System::Threading::GetCurrentThreadId;
use windows::Win32::System::Threading::{GetCurrentThread, SetThreadPriority, THREAD_PRIORITY_TIME_CRITICAL};
use windows::Win32::UI::Input::KeyboardAndMouse::{
    GetAsyncKeyState, SendInput, INPUT, INPUT_0, INPUT_KEYBOARD, KEYBDINPUT, KEYBD_EVENT_FLAGS, KEYEVENTF_EXTENDEDKEY,
    KEYEVENTF_KEYUP, VIRTUAL_KEY, VK_LWIN, VK_RWIN,
};
use windows::Win32::UI::WindowsAndMessaging::{
    CallNextHookEx, DispatchMessageW, GetForegroundWindow, GetMessageW, PeekMessageW, PostMessageW, PostThreadMessageW,
    SetWindowsHookExW, UnhookWindowsHookEx, HC_ACTION, HHOOK, KBDLLHOOKSTRUCT, LLKHF_EXTENDED, LLKHF_INJECTED, MSG,
    PM_NOREMOVE, WH_KEYBOARD_LL, WM_APP, WM_KEYDOWN, WM_KEYUP, WM_QUIT, WM_SYSKEYDOWN, WM_SYSKEYUP,
};

use crate::msg::{
    WM_DOCK_DIAG, WM_DOCK_LAUNCHER, WM_DOCK_LAUNCHER_PASSED, WM_DOCK_LAUNCHER_YIELDED, WM_DOCK_START_MENU,
    WM_DOCK_TOGGLE, WM_DOCK_WINKEY,
};
use crate::{hwnd, log};

/// Events for WM_DOCK_WINKEY (log only).
pub const EV_PRESSED: usize = 0;
pub const EV_ALONE: usize = 1;
pub const EV_COMBO: usize = 2;
pub const EV_SENDFAIL: usize = 3;
pub const EV_DOUBLE: usize = 4;

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

/// Thread message (UI -> hook): run the `reinstall` watchdog.
const WM_HOOK_REINSTALL: u32 = WM_APP + 10;

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
            let _ = ready.send(Ok(tid));
        }
        Err(e) => {
            let _ = ready.send(Err(e));
            let _ = done.send(());
            return;
        }
    }
    loop {
        // SAFETY: `msg` is valid and writable; filter None = all messages of the thread.
        let r = unsafe { GetMessageW(&mut msg, None, 0, 0) };
        if r.0 <= 0 {
            break; // WM_QUIT (0) or error (-1)
        }
        if msg.message == WM_HOOK_REINSTALL {
            reinstall();
            continue;
        }
        if msg.message == WM_HOOK_NOTE {
            if msg.wParam.0 == NOTE_LAUNCHER_FAIL_OPEN {
                let seq = msg.lParam.0 as u32;
                log!(
                    "hook: launcher chord failed open (seq {seq}): the UI thread did not acknowledge for {ACK_TIMEOUT_MS} ms"
                );
            }
            continue;
        }
        // SAFETY: `msg` just filled by GetMessageW (the thread has no windows: in practice
        // only thread messages arrive here, but forwarding is harmless).
        unsafe { DispatchMessageW(&msg) };
    }
    uninstall();
    let _ = done.send(());
}

/// Asks the hook thread to run the watchdog (called from the UI thread 2 s timer).
pub fn request_reinstall(t: &HookThread) {
    // SAFETY: PostThreadMessageW is thread-safe; if the thread has already ended it merely fails.
    let _ = unsafe { PostThreadMessageW(t.tid, WM_HOOK_REINSTALL, WPARAM(0), LPARAM(0)) };
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
/// the age (ms) of the last Win event (auto-repeat included).
/// The async state alone is not trusted: a Win-down our hook swallowed never reaches it, so a Win held
/// alone reads as "up". A press unseen by the async state and older than `REPEAT_WINDOW_MS` is cleared
/// only if a chord was injected (Win+L) or it is older than `LONE_STUCK_MS`: a lone Win can be held
/// with no auto-repeat reaching the hook, and its release must still toggle.
fn repair_plan(win_down: bool, lwin_pressed: bool, rwin_pressed: bool, win_other: bool, age_ms: u32) -> Repair {
    if !win_down {
        return Repair::Go;
    }
    let over = age_ms > REPEAT_WINDOW_MS && (win_other || age_ms > LONE_STUCK_MS);
    if lwin_pressed || rwin_pressed || !over {
        Repair::Skip
    } else {
        Repair::ClearThenGo
    }
}

/// Watchdog (run on the hook thread): Windows silently removes an LL hook that exceeds
/// LowLevelHooksTimeout several times. We ALWAYS reinstall (first the new one, then remove the
/// old one: no instant without a hook). The Win state is in a static, so it survives. It is
/// skipped while Win is held, so as not to touch the hook in the middle of a combination. "Held" is
/// judged from the async key state AND the age of the last Win event, because a Win-down our hook
/// swallowed never reaches the async state. If `WIN_DOWN` is set, no Win key is down and the press
/// is over (Win+L: the release went to the secure desktop; see `repair_plan`), the state is cleared first.
fn reinstall() {
    if HOOK_RAW.load(Relaxed) == 0 {
        return;
    }
    // SAFETY: GetAsyncKeyState has no preconditions; the high bit (negative) means "down".
    let (l, r) = unsafe { (GetAsyncKeyState(VK_LWIN.0 as i32) < 0, GetAsyncKeyState(VK_RWIN.0 as i32) < 0) };
    // SAFETY: GetTickCount has no preconditions. Same clock as `kb.time`.
    let age = unsafe { GetTickCount() }.wrapping_sub(WIN_LAST_TIME.load(Relaxed));
    match repair_plan(WIN_DOWN.load(Relaxed), l, r, WIN_OTHER.load(Relaxed), age) {
        Repair::Skip => return,
        Repair::ClearThenGo => {
            WIN_DOWN.store(false, Relaxed);
            WIN_OTHER.store(false, Relaxed);
            WIN_DOUBLE.store(false, Relaxed);
            LAST_ALONE_UP.store(0, Relaxed);
            log!("hook watchdog: stale Win state cleared");
        }
        Repair::Go => {}
    }
    match set_hook() {
        Ok(new) => {
            let old = HOOK_RAW.swap(new, Relaxed);
            // SAFETY: `old` is an HHOOK of ours; if Windows had already removed it, this fails
            // with no effect and the error is intended (it is the case the watchdog covers).
            let _ = unsafe { UnhookWindowsHookEx(HHOOK(old as *mut _)) };
        }
        Err(e) => log!("hook watchdog: reinstall failed: {e}"),
    }
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
        // SAFETY: for HC_ACTION in a WH_KEYBOARD_LL hook, lparam points to a
        // KBDLLHOOKSTRUCT valid for the whole duration of the callback.
        let kb = unsafe { &*(lp.0 as *const KBDLLHOOKSTRUCT) };
        #[cfg(feature = "owner-probe")]
        crate::launcher_probe::key_seen(wp.0 as u32, kb.time);
        let swallowed = handle_key(wp.0 as u32, kb);
        diag(wp.0 as u32, kb, swallowed);
        #[cfg(feature = "owner-probe")]
        crate::launcher_probe::record_hook_us(started.elapsed());
        if swallowed {
            return LRESULT(1);
        }
    }
    // SAFETY: unchanged forwarding to the hook chain.
    unsafe { CallNextHookEx(None, code, wp, lp) }
}

/// Returns true if the event must be swallowed.
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
    if LAUNCHER_CHORD.load(Acquire) & CHORD_SET != 0 {
        let mods = mods_step(vk, down, kb.time);
        note_gesture(&GESTURE_TIME, kb.flags.0, kb.dwExtraInfo, vk, down, mods, kb.time);
        if let Some(swallow) = launcher_step(kb, down, mods) {
            return swallow;
        }
    }
    let is_win = vk == u32::from(VK_LWIN.0) || vk == u32::from(VK_RWIN.0);

    if is_win {
        if down {
            let fresh = kb.time.wrapping_sub(WIN_LAST_TIME.load(Relaxed)) > REPEAT_WINDOW_MS;
            WIN_LAST_TIME.store(kb.time, Relaxed);
            if WIN_DOWN.load(Relaxed) && !fresh {
                if vk == WIN_VK.load(Relaxed) {
                    // auto-repeat: still "pending" -> swallow; already re-injected -> let through
                    return !WIN_OTHER.load(Relaxed);
                }
                // the other Win key while the first is held: it is an "other key"
                return other_key_down(kb);
            }
            WIN_DOWN.store(true, Relaxed);
            WIN_OTHER.store(false, Relaxed);
            WIN_VK.store(vk, Relaxed);
            let last = LAST_ALONE_UP.load(Relaxed);
            WIN_DOUBLE.store(last != 0 && kb.time.wrapping_sub(last) <= DOUBLE_TAP_MS, Relaxed);
            note(EV_PRESSED);
            return true;
        }
        if WIN_DOWN.load(Relaxed) && vk == WIN_VK.load(Relaxed) {
            WIN_DOWN.store(false, Relaxed);
            if !WIN_OTHER.load(Relaxed) {
                if WIN_DOUBLE.swap(false, Relaxed) {
                    // Second quick tap: Start menu; the next tap starts a fresh sequence.
                    LAST_ALONE_UP.store(0, Relaxed);
                    note(EV_DOUBLE);
                    post_start_menu();
                } else {
                    // `| 1`: a real timestamp of 0 must not read as "no previous tap".
                    LAST_ALONE_UP.store(kb.time | 1, Relaxed);
                    note(EV_ALONE);
                    post_toggle();
                }
                return true;
            }
            // Win used in a combination: it does not count as the first tap of a double tap.
            LAST_ALONE_UP.store(0, Relaxed);
            WIN_DOUBLE.store(false, Relaxed);
        }
        return false;
    }

    down && other_key_down(kb)
}

/// Key (other than the held Win) pressed. Returns true if it must be swallowed: in that case
/// it was re-injected inside the [WIN down, key down] pair.
fn other_key_down(kb: &KBDLLHOOKSTRUCT) -> bool {
    if !WIN_DOWN.load(Relaxed) || WIN_OTHER.load(Relaxed) {
        return false;
    }
    if kb.time.wrapping_sub(WIN_LAST_TIME.load(Relaxed)) > REPEAT_WINDOW_MS {
        // Win release lost (secure desktop): Win is no longer held, do not inject anything.
        WIN_DOWN.store(false, Relaxed);
        return false;
    }
    WIN_OTHER.store(true, Relaxed);
    if inject_win_and_key(WIN_VK.load(Relaxed), kb) {
        note(EV_COMBO);
        true
    } else {
        // SendInput failed: better to let at least the original key through.
        note(EV_SENDFAIL);
        false
    }
}

fn post(msg: u32, ev: usize) {
    let h = hwnd();
    if h.0.is_null() {
        return; // without this check PostMessageW(NULL) would post to the hook thread
    }
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

/// Thread message (hook thread -> itself): a note to log outside the callback. wParam = `NOTE_*`, lParam = a sequence.
/// WS-C's name and meaning (P0-wsC 17.1 and 17.3): wParam values 1-15 belong to WS-C, 16-31 to the launcher.
const WM_HOOK_NOTE: u32 = WM_APP + 19;
/// `WM_HOOK_NOTE`: the launcher chord failed open; lParam = the unacknowledged sequence.
const NOTE_LAUNCHER_FAIL_OPEN: usize = 16;

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
        assert_eq!(repair_plan(true, false, false, false, 0), Skip, "swallowed Win-down never reaches async state");
        assert_eq!(repair_plan(true, false, false, true, REPEAT_WINDOW_MS), Skip, "chord still within the repeat window");
        assert_eq!(repair_plan(true, false, false, true, STALE), ClearThenGo, "Win+L left WIN_DOWN stuck");
        assert_eq!(repair_plan(true, false, false, false, STALE), Skip, "lone Win held with no auto-repeat");
        assert_eq!(repair_plan(true, false, false, false, LONE_STUCK_MS), Skip, "lone Win at the limit: still held");
        assert_eq!(repair_plan(true, false, false, false, LONG), ClearThenGo, "lone release lost on the secure desktop");
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
}
