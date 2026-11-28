//! Session end and lifetime of the ops host (OP5c). WM_QUERYENDSESSION while an operation runs blocks the sign-out
//! with a reason Windows shows (the user may still sign out); WM_ENDSESSION flushes every journal and the host keeps
//! running until it is ended. The lifetime machine decides when an idle host leaves: 30 s (5 s in test mode) after
//! its queue emptied with no UI attached, at once when the test parent has gone and nothing runs. Pure but for the
//! two window calls.

use std::time::{Duration, Instant};

use oled_trust::frame::files::OpKind;
use windows::core::HSTRING;
use windows::Win32::Foundation::HWND;
use windows::Win32::System::Shutdown::{ShutdownBlockReasonCreate, ShutdownBlockReasonDestroy};

use crate::strings::{block_text, Lang};

/// How long an idle host waits for a UI.
pub const IDLE_EXIT: Duration = Duration::from_secs(30);
/// The same in test mode.
pub const IDLE_EXIT_TEST: Duration = Duration::from_secs(5);

/// The reason to block a sign-out with, or None to allow it: the first running operation's kind and the items of all.
pub fn block_reason(running: &[(OpKind, u64)], lang: Lang) -> Option<String> {
    let (kind, _) = running.first()?;
    let n: u64 = running.iter().map(|(_, n)| n).sum();
    Some(block_text(lang, *kind, n))
}

/// WM_QUERYENDSESSION: true allows the session to end.
pub fn query_end_session(hwnd: HWND, running: &[(OpKind, u64)], lang: Lang) -> bool {
    match block_reason(running, lang) {
        Some(text) => {
            let t = HSTRING::from(text);
            // SAFETY: our own top-level window; `t` is NUL-terminated and lives for the call.
            let _ = unsafe { ShutdownBlockReasonCreate(hwnd, &t) };
            false
        }
        None => {
            // SAFETY: our own window; destroying a reason that does not exist is harmless.
            let _ = unsafe { ShutdownBlockReasonDestroy(hwnd) };
            true
        }
    }
}

/// WM_ENDSESSION: when the session ends, every journal is flushed (resume needs them at the next start).
pub fn end_session(ending: bool) {
    if ending {
        crate::ops::journal::flush_all();
    }
}

/// When an idle host leaves.
#[derive(Clone, Copy, Debug)]
pub struct Lifetime {
    idle: Duration,
    attached: bool,
    busy: bool,
    parent_gone: bool,
    /// When the host last became idle (no UI, nothing queued).
    since: Instant,
}

impl Lifetime {
    /// A new host: no UI yet, nothing queued.
    pub fn new(idle: Duration, now: Instant) -> Lifetime {
        Lifetime { idle, attached: false, busy: false, parent_gone: false, since: now }
    }

    /// The host's state changed (an Attach, a Bye, a request, the queue emptying).
    pub fn set(&mut self, attached: bool, busy: bool, now: Instant) {
        let was_idle = !self.attached && !self.busy;
        self.attached = attached;
        self.busy = busy;
        if !attached && !busy && !was_idle {
            self.since = now;
        }
    }

    /// The test parent has exited.
    pub fn parent_gone(&mut self) {
        self.parent_gone = true;
    }

    /// When the host should leave; None while a UI is attached or anything runs.
    pub fn deadline(&self) -> Option<Instant> {
        if self.busy {
            return None;
        }
        if self.parent_gone {
            return Some(self.since);
        }
        if self.attached {
            return None;
        }
        Some(self.since + self.idle)
    }

    pub fn should_exit(&self, now: Instant) -> bool {
        self.deadline().is_some_and(|d| now >= d)
    }

    /// What to do when the idle timer woke the host at `now` (or the parent handle did). The timer and the Instant
    /// clock are different clocks: the timer may fire a little before the deadline, and it is one-shot, so a wake that
    /// finds the deadline still ahead must arm it again for the rest, or the host would never leave.
    pub fn on_wake(&self, now: Instant, busy: bool) -> Wake {
        match self.deadline() {
            Some(d) if now >= d && !busy => Wake::Exit,
            Some(d) if now < d => Wake::Rearm(d - now),
            _ => Wake::Wait,
        }
    }
}

/// The outcome of an idle-timer wake.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Wake {
    /// Leave now.
    Exit,
    /// The deadline is still ahead: arm the timer for this long.
    Rearm(Duration),
    /// Nothing to do: a state change arms the timer again when it matters.
    Wait,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lifetime_state_machine() {
        let t0 = Instant::now();
        let s = |n: u64| t0 + Duration::from_secs(n);
        let mut l = Lifetime::new(IDLE_EXIT_TEST, t0);
        assert_eq!(l.deadline(), Some(s(5)), "a host nobody attaches to leaves");
        assert!(!l.should_exit(s(4)) && l.should_exit(s(5)));
        l.set(true, false, s(1));
        assert_eq!(l.deadline(), None, "an Attach cancels the timer");
        l.set(true, true, s(2));
        l.set(false, true, s(3));
        assert_eq!(l.deadline(), None, "detached while an operation runs: it finishes first");
        l.set(false, false, s(10));
        assert_eq!(l.deadline(), Some(s(15)), "the queue empties with no UI: the timer starts");
        l.set(false, false, s(12));
        assert_eq!(l.deadline(), Some(s(15)), "nothing changed: the same deadline");
        l.set(true, false, s(13));
        assert_eq!(l.deadline(), None, "a re-attach cancels it");
        l.set(false, false, s(20));
        assert_eq!(l.deadline(), Some(s(25)));
        l.set(false, true, s(21));
        l.parent_gone();
        assert_eq!(l.deadline(), None, "the parent is gone but an operation runs");
        l.set(false, false, s(30));
        assert!(l.should_exit(s(30)), "then it leaves at once");
        let mut a = Lifetime::new(IDLE_EXIT, t0);
        a.set(true, false, s(1));
        a.parent_gone();
        assert!(a.should_exit(s(1)), "the test parent (the UI) is gone");
    }

    /// The timer is not the clock the deadline is read on: a wake a few milliseconds early must arm it again.
    #[test]
    fn early_timer_wake_rearms() {
        let t0 = Instant::now();
        let ms = |n: u64| t0 + Duration::from_millis(n);
        let l = Lifetime::new(IDLE_EXIT_TEST, t0);
        assert_eq!(l.on_wake(ms(4_997), false), Wake::Rearm(Duration::from_millis(3)), "early: not forgotten");
        assert_eq!(l.on_wake(ms(5_000), false), Wake::Exit, "on the deadline");
        assert_eq!(l.on_wake(ms(5_400), false), Wake::Exit, "late");
        assert_eq!(l.on_wake(ms(5_400), true), Wake::Wait, "due but an operation runs: its end arms the timer");
        let mut a = Lifetime::new(IDLE_EXIT_TEST, t0);
        a.set(true, false, ms(1));
        assert_eq!(a.on_wake(ms(5_000), false), Wake::Wait, "a UI is attached");
    }

    #[test]
    fn block_reasons() {
        assert_eq!(block_reason(&[], Lang::En), None);
        let r = [(OpKind::Copy, 3), (OpKind::Move, 4)];
        assert_eq!(block_reason(&r, Lang::En).as_deref(), Some("Copying 7 items"));
    }
}
