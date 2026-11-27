//! The shell client's breakers (SH7a, pure; times are ms of one monotonic clock). A crash or a deadline kill is
//! attributed to (call kind, extension) and recorded here:
//! - rule A: two of the same pair within 60 s disable that pair for the session (its requests get Fallback Disabled);
//!   the calls of one classic menu (QueryContextMenu, HandleMenuMsg, VerbName, InvokeCommand) are one pair, the one a
//!   Menu request is checked against;
//! - rule B: three of any kind within 10 minutes turn the menus off (Menu and ModernItems get Fallback Disabled;
//!   images go on under rule A);
//! - rule C: more than 5 spawns within 60 s stop spawning for 60 s (Fallback Unavailable).

use std::collections::{HashMap, HashSet, VecDeque};

use oled_trust::frame::files::CallKind;

pub const PAIR_WINDOW_MS: u64 = 60_000;
pub const PAIR_LIMIT: usize = 2;
pub const ANY_WINDOW_MS: u64 = 600_000;
pub const ANY_LIMIT: usize = 3;
pub const SPAWN_WINDOW_MS: u64 = 60_000;
pub const SPAWN_LIMIT: usize = 5;
pub const SPAWN_PAUSE_MS: u64 = 60_000;

/// The call kinds of a menu: off when rule B trips.
pub fn is_menu_kind(kind: CallKind) -> bool {
    matches!(
        kind,
        CallKind::QueryContextMenu
            | CallKind::HandleMenuMsg
            | CallKind::InvokeCommand
            | CallKind::ModernItems
            | CallKind::VerbName
    )
}

/// The kind a pair is kept under: the calls of one classic menu are one handler's, under QueryContextMenu.
fn pair_kind(kind: CallKind) -> CallKind {
    match kind {
        CallKind::HandleMenuMsg | CallKind::VerbName | CallKind::InvokeCommand => CallKind::QueryContextMenu,
        k => k,
    }
}

/// Drops the times older than the window from the front.
fn prune(q: &mut VecDeque<u64>, now: u64, window: u64) {
    while q.front().is_some_and(|&t| now.saturating_sub(t) >= window) {
        q.pop_front();
    }
}

#[derive(Debug, Default)]
pub struct Breakers {
    pairs: HashMap<(CallKind, String), VecDeque<u64>>,
    disabled: HashSet<(CallKind, String)>,
    any: VecDeque<u64>,
    menu_off: bool,
    spawns: VecDeque<u64>,
    no_spawn_until: Option<u64>,
}

impl Breakers {
    /// A crash or a deadline kill attributed to (kind, ext) at now.
    pub fn record(&mut self, kind: CallKind, ext: &str, now: u64) {
        let key = (pair_kind(kind), ext.to_string());
        let q = self.pairs.entry(key.clone()).or_default();
        prune(q, now, PAIR_WINDOW_MS);
        q.push_back(now);
        if q.len() >= PAIR_LIMIT {
            self.disabled.insert(key);
        }
        prune(&mut self.any, now, ANY_WINDOW_MS);
        self.any.push_back(now);
        if self.any.len() >= ANY_LIMIT {
            self.menu_off = true;
        }
    }

    /// Rule A for (kind, ext), and rule B for the menu kinds.
    pub fn disabled(&self, kind: CallKind, ext: &str) -> bool {
        (self.menu_off && is_menu_kind(kind)) || self.disabled.contains(&(pair_kind(kind), ext.to_string()))
    }

    pub fn menu_off(&self) -> bool {
        self.menu_off
    }

    /// Rule C: true (and the spawn counted) when a host may be started now.
    pub fn spawn(&mut self, now: u64) -> bool {
        if let Some(until) = self.no_spawn_until {
            if now < until {
                return false;
            }
            self.no_spawn_until = None;
        }
        prune(&mut self.spawns, now, SPAWN_WINDOW_MS);
        if self.spawns.len() >= SPAWN_LIMIT {
            self.no_spawn_until = Some(now + SPAWN_PAUSE_MS);
            return false;
        }
        self.spawns.push_back(now);
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rule_a_two_of_a_pair_within_a_minute() {
        let mut b = Breakers::default();
        b.record(CallKind::Icon, "png", 0);
        assert!(!b.disabled(CallKind::Icon, "png"), "one is not enough");
        b.record(CallKind::Icon, "png", 70_000);
        assert!(!b.disabled(CallKind::Icon, "png"), "the first left the window");
        b.record(CallKind::Icon, "png", 100_000);
        assert!(b.disabled(CallKind::Icon, "png"), "two within 60 s");
        assert!(!b.disabled(CallKind::Icon, "jpg"), "another extension");
        assert!(!b.disabled(CallKind::Thumb, "png"), "another kind");
        assert!(b.disabled(CallKind::Icon, "png"), "for the session");
        assert!(b.menu_off(), "three of any kind within 10 minutes also turn the menus off");
        let mut m = Breakers::default();
        m.record(CallKind::QueryContextMenu, "txt", 0);
        m.record(CallKind::QueryContextMenu, "txt", 1_000);
        assert!(m.disabled(CallKind::QueryContextMenu, "txt") && !m.disabled(CallKind::QueryContextMenu, "log"));
        assert!(!m.menu_off());
    }

    #[test]
    fn a_classic_menu_is_one_pair() {
        let mut b = Breakers::default();
        b.record(CallKind::HandleMenuMsg, "txt", 0);
        b.record(CallKind::InvokeCommand, "txt", 1_000);
        assert!(b.disabled(CallKind::QueryContextMenu, "txt"), "a Menu request for .txt is answered Disabled");
        assert!(b.disabled(CallKind::VerbName, "txt"), "every call of the same menu");
        assert!(!b.disabled(CallKind::QueryContextMenu, "log"), "another extension");
        assert!(!b.disabled(CallKind::ModernItems, "txt"), "the modern items are a pair of their own");
        let mut v = Breakers::default();
        v.record(CallKind::VerbName, "doc", 0);
        v.record(CallKind::QueryContextMenu, "doc", 1);
        assert!(v.disabled(CallKind::QueryContextMenu, "doc"), "two calls of one menu, two kinds");
    }

    #[test]
    fn rule_b_three_of_any_kind_within_ten_minutes() {
        let mut b = Breakers::default();
        b.record(CallKind::Icon, "png", 0);
        b.record(CallKind::Thumb, "jpg", 300_000);
        assert!(!b.menu_off());
        b.record(CallKind::QueryContextMenu, "txt", 610_000);
        assert!(!b.menu_off(), "the first left the 10-minute window");
        b.record(CallKind::HandleMenuMsg, "doc", 620_000);
        assert!(b.menu_off());
        assert!(b.disabled(CallKind::QueryContextMenu, "anything") && b.disabled(CallKind::ModernItems, ""));
        assert!(!b.disabled(CallKind::Icon, "bmp"), "images go on under rule A");
    }

    #[test]
    fn rule_c_more_than_five_spawns_a_minute() {
        let mut b = Breakers::default();
        for t in 0..5 {
            assert!(b.spawn(t * 1_000), "spawn {t}");
        }
        assert!(!b.spawn(10_000), "the sixth within 60 s");
        assert!(!b.spawn(69_000), "paused for 60 s");
        assert!(b.spawn(70_000), "the pause is over and the old spawns left the window");
    }
}
