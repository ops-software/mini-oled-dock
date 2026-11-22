//! The owner-probe build (L0 and the live checks, SPEC-launcher Labels): switches and timings compiled in only with the
//! `owner-probe` cargo feature (lint 4), never in a release the owner installs. This part holds what the keyboard hook
//! reads (L1a-10 step 7): the menu-mask key, the callback timer and the UI-freeze switch; the chord flow's probe lines
//! join it in L1a-15e. Each switch is read once.
#![deny(clippy::unwrap_used, clippy::expect_used, clippy::indexing_slicing)]

use std::sync::atomic::{AtomicBool, AtomicU32, Ordering::Relaxed};
use std::sync::OnceLock;
use std::time::Duration;

use crate::log;

/// `DOCK_LAUNCHER_MASK`: `e8` -> 0xE8, `ctrl` -> 0xA2 (left Ctrl), `none` -> 0; unset or anything else -> 0xE8.
pub fn parse_mask(v: &str) -> u32 {
    match v {
        "ctrl" => 0xA2,
        "none" => 0,
        _ => 0xE8,
    }
}

/// The menu-mask key the hook injects under Alt when it swallows the chord (S-ALTSPACE, L0-2 step 4).
pub fn mask_key() -> u32 {
    static MASK: OnceLock<u32> = OnceLock::new();
    *MASK.get_or_init(|| parse_mask(std::env::var("DOCK_LAUNCHER_MASK").as_deref().unwrap_or("")))
}

/// The keyboard hook callback's own duration: one bucket per microsecond, the last for 63 us and more.
pub static HOOK_US: [AtomicU32; 64] = [const { AtomicU32::new(0) }; 64];

/// Counts one callback that took `d`.
pub fn record_hook_us(d: Duration) {
    let us = usize::try_from(d.as_micros()).unwrap_or(usize::MAX).min(63);
    if let Some(bucket) = HOOK_US.get(us) {
        bucket.fetch_add(1, Relaxed);
    }
}

/// `DOCK_TEST_FREEZE_UI_MS`: the first launcher acknowledgement stalls the UI thread this long (the frozen-UI case of
/// live check 7), the launcher's own copy of WS-C 17.1 step 6, whose helper exists only in debug builds.
#[allow(dead_code)] // called by hook::ack_launcher, which the chord flow (L1a-15) calls
pub fn freeze_ui_once() {
    static FREEZE_MS: OnceLock<Option<u64>> = OnceLock::new();
    static FROZE: AtomicBool = AtomicBool::new(false);
    let ms =
        *FREEZE_MS.get_or_init(|| std::env::var("DOCK_TEST_FREEZE_UI_MS").ok().and_then(|v| v.trim().parse().ok()));
    if let Some(ms) = ms {
        if !FROZE.swap(true, Relaxed) {
            log!("TEST: freezing the UI thread for {ms} ms");
            std::thread::sleep(Duration::from_millis(ms));
        }
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::indexing_slicing)]
mod tests {
    use super::*;

    #[test]
    fn mask_key_values() {
        assert_eq!(parse_mask("e8"), 0xE8);
        assert_eq!(parse_mask("ctrl"), 0xA2);
        assert_eq!(parse_mask("none"), 0);
        assert_eq!(parse_mask(""), 0xE8);
        assert_eq!(parse_mask("x"), 0xE8);
    }

    #[test]
    fn hook_time_buckets() {
        let before = HOOK_US[63].load(Relaxed);
        record_hook_us(Duration::from_secs(1));
        assert_eq!(HOOK_US[63].load(Relaxed), before + 1, "63 us and more");
        let b = HOOK_US[5].load(Relaxed);
        record_hook_us(Duration::from_nanos(5_900));
        assert_eq!(HOOK_US[5].load(Relaxed), b + 1);
    }
}
