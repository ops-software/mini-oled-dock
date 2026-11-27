//! The shell client's supervisor (SH7a: the pure deadline rules; SH7c: the thread). Request deadlines: a Menu has
//! 1,500 ms until MenuShown, a ModernItems 1,500 ms until its last Modern frame. Crumb deadlines: a call of
//! QueryContextMenu or ModernItems longer than 1,500 ms, or of HandleMenuMsg, Icon, Thumb, ClassIcon, ShellNew, Pins
//! or VerbName longer than 2,000 ms is due for a kill; InvokeCommand and Exec have none. The supervisor waits without
//! a timeout when nothing is outstanding and no menu is displayed, and checks the crumbs every 100 ms while image work
//! is outstanding or a menu is displayed.

use oled_trust::frame::files::{CallKind, ReqId};

use crate::shell::crumbs::SlotView;

pub const MENU_SHOWN_MS: u64 = 1_500;
pub const MODERN_MS: u64 = 1_500;
pub const CRUMB_POLL_MS: u64 = 100;
/// A host that has not said Ready this long after its spawn is killed.
pub const START_MS: u64 = 10_000;

/// What an outstanding request waits for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Wait {
    /// A Menu before MenuShown (its own deadline).
    MenuShown,
    /// A Menu on screen, until MenuDone (crumb checks).
    MenuOpen,
    /// A ModernItems before its last frame (its own deadline).
    ModernDone,
    /// Image work, ShellNewTypes, QuickAccessPins (crumb checks).
    Work,
    /// Exec, RunTyped and an invoke in progress: no deadline.
    Busy,
}

/// One request the host has not answered yet.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Outstanding {
    pub req: ReqId,
    pub wait: Wait,
    /// When it was sent (ms).
    pub since: u64,
    /// The kind and extension a host death is attributed to when no crumb is in a call.
    pub kind: CallKind,
    pub ext: String,
}

/// The crumb deadline of a call kind; None for InvokeCommand and Exec.
pub fn crumb_limit(kind: CallKind) -> Option<u64> {
    match kind {
        CallKind::QueryContextMenu | CallKind::ModernItems => Some(1_500),
        CallKind::HandleMenuMsg
        | CallKind::Icon
        | CallKind::Thumb
        | CallKind::ClassIcon
        | CallKind::ShellNew
        | CallKind::Pins
        | CallKind::VerbName => Some(2_000),
        CallKind::InvokeCommand | CallKind::Exec => None,
    }
}

/// true when the slot is in a call past its kind's deadline at now.
pub fn crumb_overdue(slot: &SlotView, now: u64) -> bool {
    slot.in_call && slot.kind.and_then(crumb_limit).is_some_and(|limit| now.saturating_sub(slot.started_ms) > limit)
}

/// The request's own deadline (ms of the clock), if it has one.
fn deadline(o: &Outstanding) -> Option<u64> {
    match o.wait {
        Wait::MenuShown => Some(o.since.saturating_add(MENU_SHOWN_MS)),
        Wait::ModernDone => Some(o.since.saturating_add(MODERN_MS)),
        Wait::MenuOpen | Wait::Work | Wait::Busy => None,
    }
}

/// The requests whose own deadline has passed at now.
pub fn overdue(out: &[Outstanding], now: u64) -> Vec<ReqId> {
    out.iter().filter(|o| deadline(o).is_some_and(|d| now >= d)).map(|o| o.req).collect()
}

/// How long the supervisor may sleep (ms); None: without a timeout (nothing outstanding, no menu displayed).
pub fn next_wake(out: &[Outstanding], now: u64) -> Option<u64> {
    out.iter()
        .filter_map(|o| match o.wait {
            Wait::MenuShown | Wait::ModernDone => deadline(o).map(|d| d.saturating_sub(now)),
            Wait::MenuOpen | Wait::Work => Some(CRUMB_POLL_MS),
            Wait::Busy => None,
        })
        .min()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn o(req: u32, wait: Wait, since: u64) -> Outstanding {
        Outstanding { req: ReqId(req), wait, since, kind: CallKind::QueryContextMenu, ext: String::new() }
    }

    #[test]
    fn deadline_math() {
        assert_eq!(next_wake(&[], 5), None, "idle: INFINITE");
        assert_eq!(next_wake(&[o(1, Wait::Busy, 0)], 5), None, "an invoke or Exec has no deadline");
        assert_eq!(next_wake(&[o(1, Wait::MenuShown, 1_000)], 1_200), Some(1_300), "the menu's 1,500 ms");
        assert_eq!(next_wake(&[o(1, Wait::MenuShown, 1_000), o(2, Wait::Work, 1_100)], 1_200), Some(100), "crumbs");
        assert_eq!(next_wake(&[o(1, Wait::ModernDone, 0), o(2, Wait::MenuShown, 500)], 1_000), Some(500));
        assert_eq!(next_wake(&[o(1, Wait::MenuOpen, 0)], 99_999), Some(CRUMB_POLL_MS), "a menu displayed");
        assert_eq!(next_wake(&[o(1, Wait::MenuShown, 0)], 5_000), Some(0), "overdue: at once");
        let out = [o(1, Wait::MenuShown, 0), o(2, Wait::ModernDone, 600), o(3, Wait::Work, 0), o(4, Wait::Busy, 0)];
        assert_eq!(overdue(&out, 1_499), Vec::<ReqId>::new());
        assert_eq!(overdue(&out, 1_500), [ReqId(1)]);
        assert_eq!(overdue(&out, 2_100), [ReqId(1), ReqId(2)]);
    }

    #[test]
    fn crumb_deadlines() {
        let slot = |kind: CallKind, started_ms: u64, in_call: bool| SlotView {
            seq: u32::from(in_call),
            in_call,
            kind: Some(kind),
            flags: 0,
            started_ms,
            ext: String::new(),
            calls: 0,
        };
        assert!(!crumb_overdue(&slot(CallKind::QueryContextMenu, 0, true), 1_500));
        assert!(crumb_overdue(&slot(CallKind::QueryContextMenu, 0, true), 1_501));
        assert!(!crumb_overdue(&slot(CallKind::Icon, 0, true), 2_000));
        assert!(crumb_overdue(&slot(CallKind::VerbName, 0, true), 2_001));
        assert!(!crumb_overdue(&slot(CallKind::InvokeCommand, 0, true), 3_600_000), "an invoke has no deadline");
        assert!(!crumb_overdue(&slot(CallKind::Exec, 0, true), 3_600_000));
        assert!(!crumb_overdue(&slot(CallKind::Icon, 0, false), 9_000), "not in a call");
        for k in [CallKind::HandleMenuMsg, CallKind::Thumb, CallKind::ClassIcon, CallKind::ShellNew, CallKind::Pins] {
            assert_eq!(crumb_limit(k), Some(2_000), "{k:?}");
        }
        assert_eq!(crumb_limit(CallKind::ModernItems), Some(1_500));
    }
}
