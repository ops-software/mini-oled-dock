//! The files helper child (plan L2-F2): `dock.exe --launcher-files` with stdin closed writes its header and exactly one
//! state frame, then ends with exit code 0. The child mode is dispatched before the data folder, the logger, the mutex
//! and the watchdog, so this starts no dock; with stdin closed the helper runs no query. A hosted runner has no shell
//! window, so its state is 4 there.

use std::io::Read;
use std::os::windows::process::CommandExt;
use std::process::{Command, Stdio};

/// `CREATE_NO_WINDOW` (winbase.h): every process a test starts carries it (lint `every_spawned_process_has_no_window`).
const CREATE_NO_WINDOW: u32 = 0x0800_0000;
/// Appendix A's header, checked byte by byte.
const HEADER: &[u8] = b"mini-oled-dock launcher-files 1\n";

#[test]
fn header_and_one_state_frame_then_exit_0() {
    let mut child = Command::new(env!("CARGO_BIN_EXE_dock"))
        .creation_flags(CREATE_NO_WINDOW)
        .arg("--launcher-files")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .unwrap_or_else(|e| match e.raw_os_error() {
            Some(740) => panic!("dock.exe needs elevation: run the tests with DOCK_NO_MANIFEST=1 (see build.rs)"),
            _ => panic!("spawn: {e}"),
        });
    let mut out = Vec::new();
    child.stdout.take().unwrap().read_to_end(&mut out).unwrap();
    let code = child.wait().unwrap().code();
    assert_eq!(out.get(..HEADER.len()), Some(HEADER), "{out:?}");
    let frame = &out[HEADER.len()..];
    assert_eq!(frame.len(), 8, "exactly one state frame, then the end: {frame:?}");
    assert_eq!(u32::from_le_bytes(frame[..4].try_into().unwrap()), 1, "the state frame's tag");
    let state = u32::from_le_bytes(frame[4..].try_into().unwrap());
    assert!(state <= 4, "state {state}");
    assert_eq!(code, Some(0));
}
