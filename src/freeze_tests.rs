//! PF1: one line per frozen dock-side item of the plumbing plan (F-P8, F-P9, F-P10) with its full signature, so a
//! change fails to compile; and the "not available" behaviour of the stubs shipped code can reach.
// Function-pointer types spelled out in full are the point of this file.
#![allow(clippy::type_complexity)]

use std::path::Path;
use std::time::Duration;

use crate::agent_proto::*;
use crate::recover::AutostartAction;
use crate::update::suite::{ComponentStatus, SuiteStatus};

#[test]
fn entry_points_keep_their_signatures() {
    let _a: fn(&str) -> i32 = crate::agent::run;
    let _b: fn(Option<&str>) -> i32 = crate::journal_ui::run_undo_user;
    let _c: fn(bool) -> i32 = crate::recover::run;
    let _d: fn(bool) -> i32 = crate::update::suite::run_sync;
    let _e: fn(Option<&str>) -> i32 = crate::update::run_fetch_child_for;
    let _f: fn(bool, &Path) -> i32 = crate::autostart::run_install_from_cli;
    let _g: fn() -> bool = crate::recover::safe_mode;
    let _h: fn() -> std::io::Result<()> = crate::recover::clear_safe_mode;
    let _i: fn() = crate::recover::leave_safe_mode_on_manual_start;
    let _j: fn(bool) -> AutostartAction = crate::recover::autostart_action;
    let _k: fn(&crate::config::Config) = crate::theme_sync::on_config;
    let _l: fn(&crate::config::Config) = crate::entry_points::on_config;
    let _m: fn(&crate::config::Config, &Path) -> Result<u32, String> = crate::entry_points::sync;
    let _n: fn() = crate::entry_points::request_sync;
    let _o: fn() -> Option<oled_config::theme::ThemeFile> = crate::theme_sync::last_written;
    let _p: fn() -> Vec<oled_trust::journal::EntryView> = crate::journal_ui::machine_views;
    let _q: fn(Duration) -> Result<Vec<JournalRec>, String> = crate::journal_ui::user_views;
    let _r: fn() -> Result<(oled_trust::journal::UndoReport, bool), String> = crate::journal_ui::undo_all_takeovers;
    let _s: fn() -> SuiteStatus = crate::update::suite::status;
    let _ = ComponentStatus {
        id: oled_trust::manifest::suite::ComponentId::Dock,
        installed: false,
        version: None,
        stage: None,
        enabled: false,
    };
    let _ = crate::shell_thread::ShellRequest::SyncEntryPoints;
}

#[test]
fn agent_records_keep_their_signatures() {
    let _: (u16, usize, usize, usize, usize, usize, [u16; 4]) =
        (AGENT_PROTO, REQ_MAX, REC_MAX, CHUNK_MAX, ICONS_IDS_MAX, ICON_ID_MAX_BYTES, EXE_ICON_PX);
    let _a: fn(&Request) -> Result<Vec<u8>, FrameError> = encode_request;
    let _b: fn(&[u8]) -> Result<Request, FrameError> = decode_request;
    let _c: fn(&Reply) -> Result<Vec<u8>, FrameError> = encode_reply;
    let _d: fn(&[u8]) -> Result<Reply, FrameError> = decode_reply;
    let _ = Request::Hello { proto: AGENT_PROTO, pid: 0 };
    let _ = Request::Icons { id: 0, px: 16, ids: Vec::new() };
    let _ = Request::ExeInfo { id: 0, path: String::new(), px: 16 };
    let _ = (
        Request::ListApps { id: 0 },
        Request::StartLinks { id: 0 },
        Request::UserJournal { id: 0 },
        Request::Cancel { id: 0 },
        Request::Bye,
    );
    let _ = Reply::Chunk { id: 0, last: true, bytes: Vec::new() };
    let _ = Reply::End { id: 0, status: 0 };
    let _ = Reply::Link(LinkRec { id: 0, rel: String::new(), target: String::new(), name: String::new() });
    let _ = Reply::ExeInfo(ExeInfoRec { id: 0, description: None, product: None, company: None, icon: None });
    let _ = Reply::Journal(JournalRec { id: 0, seq: 0, kind: String::new(), target: String::new(), ts: 0 });
    let _ = (
        FrameError::Empty,
        FrameError::Tag(0),
        FrameError::Reserved(0),
        FrameError::Short,
        FrameError::Bool(0),
        FrameError::Cap(""),
        FrameError::Utf8,
        FrameError::Trailing,
    );
}

#[test]
fn autostart_action_table() {
    assert_eq!(crate::recover::autostart_action(false), AutostartAction::Proceed);
    assert_eq!(crate::recover::autostart_action(true), AutostartAction::LeaveSafe);
}

#[test]
fn stubs_say_not_available() {
    assert_eq!(crate::agent::run("0123456789abcdef0123456789abcdef"), 2);
    // U1e implemented --suite-sync, which acts on the installed suite: it is tested over fakes (update::suite's
    // sync_outcomes and the tests beside it), never run here.
    // U1b implemented --update-fetch <tag>, so a canonical tag (or none) would reach the network: only the refusal of
    // a tag that is not canonical, before any GET, is left to check here.
    assert_eq!(crate::update::run_fetch_child_for(Some("v9.9")), 2);
    assert!(!crate::recover::safe_mode());
    assert!(crate::recover::clear_safe_mode().is_ok());
    // J1c and J1d implemented --undo-user, --recover and the journal's undo helpers. They act on the machine
    // (--undo-user's deadline ends its process; the helpers start processes through Explorer), so only their reads
    // without a data folder are left to check here.
    assert!(crate::journal_ui::machine_views().is_empty());
    assert!(crate::theme_sync::last_written().is_none());
    assert!(crate::update::suite::status().components.is_empty());
}
