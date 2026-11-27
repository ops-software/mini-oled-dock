//! The files app's engine, without windows: its own data folder, the entry store, the enumerator, the change
//! watcher, icons, the ops host that changes user files only through IFileOperation and the shell host that keeps
//! third-party shell code out of the UI. The interfaces are frozen; most bodies are filled by the groups named in
//! their todo!() markers. Identifiers, files and logs say "files": the app's display name lives in one string
//! table only.

pub mod decor;
pub mod host;
pub mod listing;
pub mod ops;
pub mod ops_client;
pub mod own_files;
pub mod role_ops;
pub mod role_shell;
pub mod shell;
pub mod shell_client;
pub mod strings;
#[cfg(debug_assertions)]
pub mod testclient;
pub mod watch;

// ---- P3-files-app (window)
pub mod about;
pub mod app;
pub mod bar;
pub mod bg;
pub mod chrome;
pub mod cli;
pub mod complete;
pub mod drives;
#[doc(hidden)]
pub mod fakes;
pub mod format;
pub mod gate;
pub mod handoff;
pub mod home;
pub mod hub;
pub mod icons;
pub mod instance;
pub mod keys;
pub mod langfile;
pub mod linger;
pub mod live;
pub mod menu;
pub mod model;
pub mod msg;
pub mod names;
pub mod nav;
pub mod navcache;
pub mod notices;
pub mod open;
pub mod ops_ui;
pub mod opswin;
pub mod options;
pub mod parse;
pub mod pathline;
pub mod pins;
pub mod preview;
pub mod rename;
pub mod selftest;
pub mod services;
pub mod session;
pub mod sidebar;
pub mod status;
pub mod tab;
pub mod toolbar;
pub mod toolmenus;
pub mod tr;
pub mod types;
pub mod undo;
pub mod view;
pub mod window;

#[cfg(test)]
#[path = "../tests/common/temp.rs"]
mod testkit;

use std::path::PathBuf;

use oled_sys::logger::LogSink;

/// Exit codes of files.exe (pinned by exit_codes_are_pinned).
pub mod exit {
    /// Normal end.
    pub const OK: i32 = 0;
    /// Bad arguments, or a role this build does not have.
    pub const USAGE: i32 = 2;
    /// The peer speaks another protocol version.
    pub const PROTO_MISMATCH: i32 = 3;
    /// The pipe name is taken (another host, or a squatter).
    pub const PIPE_TAKEN: i32 = 4;
    /// Started with a UAC-elevated token: the helpers run de-elevated only.
    pub const ELEVATED: i32 = 5;
    /// The Hello exchange failed.
    pub const HELLO_FAILED: i32 = 6;
}

/// The build id sent in Hello frames and logged, never compared: the package version, ASCII, NUL-padded to 16 bytes.
pub const BUILD_ID: [u8; 16] = build_id(env!("CARGO_PKG_VERSION"));

const fn build_id(version: &str) -> [u8; 16] {
    let b = version.as_bytes();
    assert!(b.len() <= 16, "the package version must fit the 16-byte build id");
    let mut out = [0u8; 16];
    let mut i = 0;
    while i < b.len() {
        out[i] = b[i];
        i += 1;
    }
    out
}

/// Eight lower-case hex digits of FNV-1a 32 over BUILD_ID: the build part of the ops pipe name.
pub fn build_tag() -> String {
    let mut h: u32 = 0x811c_9dc5;
    for b in BUILD_ID {
        h ^= u32::from(b);
        h = h.wrapping_mul(0x0100_0193);
    }
    format!("{h:08x}")
}

/// The UI's log, files.log in the data folder.
pub static LOG_UI: LogSink = sink(ui_log);
/// The ops host's log, files-ops.log (one file per role: two processes never share one log).
pub static LOG_OPS: LogSink = sink(ops_log);
/// The shell host's log, files-shell.log.
pub static LOG_SHELL: LogSink = sink(shell_log);

const fn sink(path: fn() -> Option<PathBuf>) -> LogSink {
    LogSink { path, open_append: own_files::open_log, debug_out, version: env!("CARGO_PKG_VERSION"), on_line: no_line }
}

fn log_in_data_dir(name: &str) -> Option<PathBuf> {
    own_files::OwnDir::default_dir().map(|d| d.path().join(name))
}

fn ui_log() -> Option<PathBuf> {
    log_in_data_dir("files.log")
}

fn ops_log() -> Option<PathBuf> {
    log_in_data_dir("files-ops.log")
}

fn shell_log() -> Option<PathBuf> {
    log_in_data_dir("files-shell.log")
}

fn debug_out(line: &str) {
    let w = windows::core::HSTRING::from(line);
    // SAFETY: `w` is a NUL-terminated string that lives for the call.
    unsafe { windows::Win32::System::Diagnostics::Debug::OutputDebugStringW(&w) };
}

fn no_line() {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exit_codes_are_pinned() {
        assert_eq!(
            [exit::OK, exit::USAGE, exit::PROTO_MISMATCH, exit::PIPE_TAKEN, exit::ELEVATED, exit::HELLO_FAILED],
            [0, 2, 3, 4, 5, 6]
        );
    }

    #[test]
    fn build_id_is_the_padded_version() {
        let v = env!("CARGO_PKG_VERSION").as_bytes();
        assert_eq!(&BUILD_ID[..v.len()], v);
        assert!(BUILD_ID[v.len()..].iter().all(|&b| b == 0));
        assert_eq!(build_id("1.2.3"), *b"1.2.3\0\0\0\0\0\0\0\0\0\0\0");
    }

    #[test]
    fn build_tag_is_eight_hex_digits() {
        let t = build_tag();
        assert_eq!(t.len(), 8);
        assert!(t.bytes().all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b)), "{t}");
        assert_eq!(t, build_tag(), "stable");
    }

    #[test]
    fn log_sinks_name_one_file_per_role() {
        let names: Vec<_> = [&LOG_UI, &LOG_OPS, &LOG_SHELL]
            .iter()
            .map(|s| (s.path)().and_then(|p| p.file_name().map(|n| n.to_string_lossy().into_owned())))
            .collect();
        if names.iter().all(Option::is_some) {
            let names: Vec<_> = names.into_iter().flatten().collect();
            assert_eq!(names, ["files.log", "files-ops.log", "files-shell.log"]);
        }
        assert_eq!(LOG_OPS.version, env!("CARGO_PKG_VERSION"));
    }
}
