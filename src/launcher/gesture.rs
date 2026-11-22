//! Run as administrator from a typed line needs proof that a person pressed Ctrl+Shift+Enter (SPEC 5.4.9). The
//! constructor from the hook's evidence and the review conditions arrive in L1a-9; here are the type the session takes
//! and the switch the owner session may turn off.

/// Proof that a person pressed Ctrl+Shift+Enter. The field is private: outside tests only the launcher's key handler
/// builds one (`from_key`, L1a-9; lint 1 keeps it there).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Gesture {
    time: u32,
}

impl Gesture {
    /// Tests of the session only (lint 1 keeps it inside test code).
    #[cfg(test)]
    pub(crate) fn for_test(time: u32) -> Gesture {
        Gesture { time }
    }
}

/// S-GESTURE (L0-2 step 2): `GetMessageTime()` equals the hook's `kb.time`. `false` turns Run as administrator off
/// for run lines and file rows (`verbs::verbs_for`'s `admin_ok`).
pub const GESTURE_TIME_MATCHES: bool = true;
