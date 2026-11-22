//! Run as administrator from a typed line needs proof that a person pressed Ctrl+Shift+Enter (SPEC 5.4.9) and a text
//! that is shown whole with nothing hidden in it. The launcher's key handler builds the proof from the hook's evidence
//! (`Gesture::from_key`, the only constructor outside tests; lint 1 keeps it there); the session checks the review
//! conditions (`review_ok`) on the typed line or the file path before it asks for the elevated launch.

use super::field::KeyEvent;
use super::model::{Mods, RunLine};
use super::runline;
use super::strings;

const VK_RETURN: u16 = 0x0D;
const CTRL_SHIFT: Mods = Mods(Mods::CTRL.0 | Mods::SHIFT.0);

/// The key message must be handled within this many ms of the key press (`kb.time` and `GetMessageTime`).
pub const GESTURE_MAX_AGE_MS: u32 = 300;
/// The review detail shows at most this many CAPTION lines (5.3); a longer text is not reviewed.
pub const REVIEW_MAX_LINES: u8 = 3;

/// Proof that a person pressed Ctrl+Shift+Enter. The field is private: outside tests only `from_key` builds one.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Gesture {
    time: u32,
}

impl Gesture {
    /// Ctrl+Shift+Enter only. `hook_time` = `hook::take_gesture_time()` (the exact `kb.time` of the last physical
    /// Ctrl+Shift+Enter, consumed by the call), `msg_time` = `GetMessageTime()`, `extra` = `GetMessageExtraInfo()`,
    /// `inject_tag` = `hook::INJECT_TAG`, `now` = `GetTickCount()`. A held key's repeat is no new press.
    pub fn from_key(
        ev: &KeyEvent,
        hook_time: Option<u32>,
        msg_time: u32,
        extra: usize,
        inject_tag: usize,
        now: u32,
    ) -> Option<Gesture> {
        let ok = ev.vk == VK_RETURN
            && ev.mods == CTRL_SHIFT
            && !ev.repeat
            && hook_time == Some(msg_time)
            && extra != inject_tag
            && now.wrapping_sub(msg_time) <= GESTURE_MAX_AGE_MS;
        ok.then_some(Gesture { time: msg_time })
    }

    /// Tests of the session only (lint 1 keeps it inside test code).
    #[cfg(test)]
    pub(crate) fn for_test(time: u32) -> Gesture {
        Gesture { time }
    }
}

/// S-GESTURE (L0-2 step 2): `GetMessageTime()` equals the hook's `kb.time`. `false` turns Run as administrator off
/// for run lines and file rows (`verbs::verbs_for`'s `admin_ok`).
pub const GESTURE_TIME_MATCHES: bool = true;

/// Why a typed line or a file path may not be run as administrator.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Refusal {
    /// It needs more than `REVIEW_MAX_LINES` lines of the review detail (or the view has not measured it).
    TooLong,
    /// A control or bidi character: what is shown is not what runs.
    Markers,
    /// A network path (\\server or //server).
    Unc,
}

impl Refusal {
    /// The detail line the view shows in place of the review. Markers are drawn in the text itself and a network path
    /// is shown whole, so only a text that cannot be shown whole has one.
    pub fn detail(self) -> Option<&'static str> {
        match self {
            Refusal::TooLong => Some(strings::line_too_long()),
            Refusal::Markers | Refusal::Unc => None,
        }
    }
}

/// A character the view draws as a marker (5.3): C0 and C1 controls, DEL, and the bidi marks and overrides.
pub fn is_marker(c: char) -> bool {
    c.is_control()
        || matches!(c, '\u{061C}' | '\u{200E}' | '\u{200F}' | '\u{202A}'..='\u{202E}' | '\u{2066}'..='\u{2069}')
}

/// The review conditions (5.4.9, conditions 3 and 4, and the UNC rule of file rows). `lines_needed` is what the view
/// measured with the real CAPTION font over the review width; 0 means not measured.
pub fn review_ok(text: &str, lines_needed: u8) -> Result<(), Refusal> {
    if text.chars().any(is_marker) {
        return Err(Refusal::Markers);
    }
    let start = text.trim_start().trim_start_matches('"');
    if start.starts_with("\\\\") || start.starts_with("//") {
        return Err(Refusal::Unc);
    }
    if lines_needed == 0 || lines_needed > REVIEW_MAX_LINES {
        return Err(Refusal::TooLong);
    }
    Ok(())
}

/// The line that runs a file as administrator: its path quoted, the file part inside the quotes. Only exe, msi, bat
/// and cmd files (5.2.8, 5.4.9 file rows), never a path with a quote in it. The one filter of file rows: a row offers
/// Run as administrator (`verbs::verbs_for_file`), shows its review and runs elevated only where this is Some.
pub fn elevated_file_line(path: &str) -> Option<RunLine> {
    const ELEVATABLE: [&str; 4] = ["exe", "msi", "bat", "cmd"];
    let name = path.rsplit(['\\', '/']).next().unwrap_or(path);
    let (_, ext) = name.rsplit_once('.')?;
    if !ELEVATABLE.iter().any(|e| e.eq_ignore_ascii_case(ext)) || path.contains('"') {
        return None;
    }
    let mut quoted = String::with_capacity(path.len() + 2);
    quoted.push('"');
    quoted.push_str(path);
    quoted.push('"');
    runline::classify(&quoted).filter(|r| r.line.get(r.file.clone()) == Some(path))
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::indexing_slicing)]
mod tests {
    use super::*;

    const TAG: usize = 0x4B43_4F44;

    fn key(vk: u16, mods: Mods) -> KeyEvent {
        KeyEvent { vk, mods, repeat: false, sys: mods.has(Mods::ALT) }
    }

    #[test]
    fn only_a_matching_hook_time() {
        let cse = key(VK_RETURN, CTRL_SHIFT);
        let t = 1_000_000;
        assert_eq!(Gesture::from_key(&cse, Some(t), t, 0, TAG, t + 5), Some(Gesture { time: t }));
        // Even and odd times each match their own hook time (no bit of the time is used as a flag).
        assert!(Gesture::from_key(&cse, Some(1_000_001), 1_000_001, 0, TAG, 1_000_010).is_some());
        assert!(Gesture::from_key(&cse, Some(1_000_000), 1_000_001, 0, TAG, 1_000_010).is_none());
        assert!(Gesture::from_key(&cse, Some(1_000_001), 1_000_000, 0, TAG, 1_000_010).is_none());
        // No hook time, another time, an injected message, too late.
        assert!(Gesture::from_key(&cse, None, t, 0, TAG, t).is_none());
        assert!(Gesture::from_key(&cse, Some(t - 16), t, 0, TAG, t).is_none());
        assert!(Gesture::from_key(&cse, Some(t), t, TAG, TAG, t).is_none());
        assert!(Gesture::from_key(&cse, Some(t), t, 0, TAG, t + 300).is_some(), "300 ms is still in time");
        assert!(Gesture::from_key(&cse, Some(t), t, 0, TAG, t + 301).is_none(), "301 ms late");
        // Across the tick wrap.
        assert!(Gesture::from_key(&cse, Some(u32::MAX - 2), u32::MAX - 2, 0, TAG, 3).is_some());
        // Other chords.
        let ctrl_enter = key(VK_RETURN, Mods::CTRL);
        assert!(Gesture::from_key(&ctrl_enter, Some(t), t, 0, TAG, t).is_none());
        let cas_enter = key(VK_RETURN, Mods(Mods::CTRL.0 | Mods::ALT.0 | Mods::SHIFT.0));
        assert!(Gesture::from_key(&cas_enter, Some(t), t, 0, TAG, t).is_none());
        let win = key(VK_RETURN, CTRL_SHIFT.with(Mods::WIN));
        assert!(Gesture::from_key(&win, Some(t), t, 0, TAG, t).is_none());
        assert!(Gesture::from_key(&key(0x45, CTRL_SHIFT), Some(t), t, 0, TAG, t).is_none(), "Ctrl+Shift+E");
        let held = KeyEvent { repeat: true, ..cse };
        assert!(Gesture::from_key(&held, Some(t), t, 0, TAG, t).is_none(), "a held key's repeat");
    }

    #[test]
    fn one_press_one_gesture() {
        let cse = key(VK_RETURN, CTRL_SHIFT);
        // The hook's evidence is one atomic, taken by the first call: the second gets None.
        let mut slot = Some(42);
        assert!(Gesture::from_key(&cse, slot.take(), 42, 0, TAG, 50).is_some());
        assert!(Gesture::from_key(&cse, slot.take(), 42, 0, TAG, 50).is_none());
    }

    #[test]
    fn review_conditions() {
        assert_eq!(review_ok("notepad c:\\temp\\a.txt", 1), Ok(()));
        assert_eq!(review_ok("x", 3), Ok(()));
        assert_eq!(review_ok("x", 4), Err(Refusal::TooLong));
        assert_eq!(review_ok("x", 0), Err(Refusal::TooLong), "not measured");
        for bad in ["a\u{202E}b", "a\u{0}b", "a\tb", "\u{7F}", "\u{85}", "\u{200E}", "\u{2066}x", "\u{061C}"] {
            assert_eq!(review_ok(bad, 1), Err(Refusal::Markers), "{bad:?}");
        }
        for unc in
            ["\\\\server\\share\\setup.exe", "//server/share/setup.exe", "\"\\\\server\\x.exe\" -y", " \\\\s\\x.exe"]
        {
            assert_eq!(review_ok(unc, 1), Err(Refusal::Unc), "{unc:?}");
        }
        assert_eq!(review_ok("c:\\a\\\\b.exe", 1), Ok(()), "a doubled separator inside is no network path");
        assert_eq!(Refusal::TooLong.detail(), Some("Line too long to review"));
        assert_eq!(Refusal::Markers.detail(), None);
        assert_eq!(Refusal::Unc.detail(), None);
    }
}
