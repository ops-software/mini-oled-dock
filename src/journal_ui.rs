//! The dock's side of the change journal: the user part through the agent, the machine part in process.
//! Lands with J1c; until then every entry point says it is not available.
#![allow(dead_code, unused_variables)]

/// `--undo-user [--signal <nonce>]`: J1c.
pub fn run_undo_user(signal: Option<&str>) -> i32 {
    crate::logger::say("--undo-user: not available in this build");
    2
}

/// Machine journal entries for the Safety page: J1c.
pub fn machine_views() -> Vec<oled_trust::journal::EntryView> {
    Vec::new()
}

/// User journal entries, through the agent: J1c.
pub fn user_views(timeout: std::time::Duration) -> Result<Vec<crate::agent_proto::JournalRec>, String> {
    Err("not available in this build".into())
}

/// Undo of every takeover (machine part, then the user part): J1c. The flag says whether the user part is done.
pub fn undo_all_takeovers() -> Result<(oled_trust::journal::UndoReport, bool), String> {
    Err("not available in this build".into())
}
