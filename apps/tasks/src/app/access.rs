//! Access facts and refusals (T3.6): the policy's check_target plus the app's own rules, one pure function used by
//! the menu and again by the executor on every key it is about to terminate. The Refusal type is TF0's; the facts
//! and the rules are filled by TA-T3c.
#![allow(dead_code, unused_variables)] // stub: removed by TA-T3c

use oled_sys::admin::policy::TargetFacts;
use oled_trust::frame::admin::DenyReason;

use super::exec::KillGuard;

/// Why a key is not terminated.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Refusal {
    /// The policy's rule (LT-POLICY's check_target).
    Deny(DenyReason),
    /// The shell process (why.shell).
    Shell,
    /// Elevated, another user's or protected: needs administrator rights (why.admin).
    Limited,
}

/// check_target with g.helper, then the app's rules (the shell process, our ancestor chain). T3.6. Stub: None.
pub fn refusal(t: &TargetFacts, g: &KillGuard) -> Option<Refusal> {
    None
}
