//! Run as administrator from a typed line needs proof that a person pressed Ctrl+Shift+Enter (SPEC 5.4.9). The
//! gesture itself and the review conditions arrive in L1a-9; here is the switch the owner session may turn off.

/// S-GESTURE (L0-2 step 2): `GetMessageTime()` equals the hook's `kb.time`. `false` turns Run as administrator off
/// for run lines and file rows (`verbs::verbs_for`'s `admin_ok`).
pub const GESTURE_TIME_MATCHES: bool = true;
