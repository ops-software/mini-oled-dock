//! files.exe: the engine roles (--role=ops, --role=shell), in a debug build the test client and the instance tests'
//! IPC role, and otherwise the window's command line (cli::parse, then app::run).
#![windows_subsystem = "windows"]

use files_engine::exit;

fn main() {
    // Every role can own a system-drawn menu (the shell host shows the classic one): dark, as the suite always is.
    oled_sys::darkmenu::init();
    let args: Vec<String> = std::env::args().skip(1).collect();
    std::process::exit(run(&args));
}

/// What the command line asks for.
#[derive(Debug, PartialEq, Eq)]
enum Role<'a> {
    Ops,
    Shell,
    /// Debug builds only: files.exe --role=test-client <script>.
    TestClient(&'a str),
    /// Debug builds only: files.exe --role=test-ipc <args> (the instance tests' message-only window).
    TestIpc,
    Usage,
    Window,
}

fn role(args: &[String]) -> Role<'_> {
    match args.first().map(String::as_str) {
        Some("--role=ops") => Role::Ops,
        Some("--role=shell") => Role::Shell,
        Some("--role=test-client") if cfg!(debug_assertions) => match args.get(1) {
            Some(script) => Role::TestClient(script),
            None => Role::Usage,
        },
        Some("--role=test-ipc") if cfg!(debug_assertions) => Role::TestIpc,
        _ => Role::Window,
    }
}

fn run(args: &[String]) -> i32 {
    let rest = args.get(1..).unwrap_or_default();
    match role(args) {
        Role::Ops => files_engine::role_ops::run(rest),
        Role::Shell => files_engine::role_shell::run(rest),
        #[cfg(debug_assertions)]
        Role::TestClient(script) => files_engine::testclient::run(std::path::Path::new(script)),
        #[cfg(not(debug_assertions))]
        Role::TestClient(_) => exit::USAGE,
        #[cfg(debug_assertions)]
        Role::TestIpc => files_engine::instance::test_ipc_role(rest),
        #[cfg(not(debug_assertions))]
        Role::TestIpc => exit::USAGE,
        Role::Usage => exit::USAGE,
        // The window takes every argument (a folder is the first one).
        Role::Window => match files_engine::cli::parse(args) {
            Ok(c) => files_engine::app::run(c),
            Err(e) => {
                let log = oled_sys::logger::start("", &files_engine::LOG_UI);
                oled_sys::log!("{e}");
                oled_sys::logger::stop(log);
                exit::USAGE
            }
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(a: &[&str]) -> Vec<String> {
        a.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn role_table() {
        assert_eq!(role(&args(&["--role=ops", "--proto=1"])), Role::Ops);
        assert_eq!(role(&args(&["--role=shell"])), Role::Shell);
        assert_eq!(role(&args(&[])), Role::Window);
        assert_eq!(role(&args(&["C:\\x"])), Role::Window);
        assert_eq!(role(&args(&["--role=OPS"])), Role::Window);
        let ipc_line = args(&["--role=test-ipc", "x"]);
        let ipc = role(&ipc_line);
        assert_eq!(ipc, if cfg!(debug_assertions) { Role::TestIpc } else { Role::Window }, "debug builds only");
        let line = args(&["--role=test-client", "s.jsonl"]);
        let client = role(&line);
        if cfg!(debug_assertions) {
            assert_eq!(client, Role::TestClient("s.jsonl"));
            assert_eq!(role(&args(&["--role=test-client"])), Role::Usage);
        } else {
            assert_eq!(client, Role::Window, "a release build has no test client");
        }
    }
}
