//! Support for tests/golden.rs: the child-process runner of the preview, image decoding and the comparator.

pub mod compare;
pub mod wic;

use std::ffi::OsString;
use std::os::windows::process::CommandExt;
use std::path::Path;

/// `CREATE_NO_WINDOW` (winbase.h): every process a test starts carries it (lint `every_spawned_process_has_no_window`).
const CREATE_NO_WINDOW: u32 = 0x0800_0000;

/// The whole argument vector the golden tests give to the dock exe. A test must never be able to start
/// the real dock: `--preview <dir>` is handled by main.rs before any window, hook or taskbar code.
pub fn preview_args(out: &Path) -> Vec<OsString> {
    vec![OsString::from("--preview"), out.as_os_str().to_owned()]
}

/// Runs one cell of the preview matrix into `out` (emptied first) and panics with a readable message
/// when the child fails.
pub fn run_preview(out: &Path, lang: &str, dpi: u32, no_text: bool) {
    let _ = std::fs::remove_dir_all(out);
    std::fs::create_dir_all(out).unwrap();
    let mut c = std::process::Command::new(env!("CARGO_BIN_EXE_dock"));
    // CREATE_NO_WINDOW: no test child may ever open a console or a window on the desktop.
    c.creation_flags(CREATE_NO_WINDOW)
        .args(preview_args(out))
        .env("DOCK_PREVIEW_DPI", dpi.to_string())
        .env("DOCK_PREVIEW_LANG", lang)
        .env_remove("DOCK_PREVIEW_NOTEXT");
    if no_text {
        c.env("DOCK_PREVIEW_NOTEXT", "1");
    }
    let st = c.stderr(std::process::Stdio::piped()).output().unwrap_or_else(|e| {
        if e.raw_os_error() == Some(740) {
            panic!("dock.exe needs elevation: run the tests with DOCK_NO_MANIFEST=1 (see build.rs)")
        } else {
            panic!("spawn: {e}")
        }
    });
    let code = st.status.code();
    let why = match code {
        Some(0) => return,
        Some(2) => "bad DOCK_PREVIEW_* variables (tests/golden.rs passes only valid ones: a bug in the test)",
        Some(1) => "an image failed to render",
        _ => "crashed or was killed",
    };
    // May be empty: a windows-subsystem exe does not always reach the pipe.
    let err = String::from_utf8_lossy(&st.stderr);
    panic!(
        "dock --preview exited with {code:?}: {why}. Run the cell by hand: DOCK_PREVIEW_DPI={dpi} \
         DOCK_PREVIEW_LANG={lang} DOCK_NO_MANIFEST=1 dock.exe --preview <dir>. stderr: {err}"
    );
}
