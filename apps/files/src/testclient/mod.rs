//! The test client (debug builds only): files.exe --role=test-client <script> reads a script of JSON lines, one
//! operation per line ({"op": "...", ...}), and routes each line: "ops_*" to testclient::ops, "shell_*" and
//! "probe-handle" to testclient::shell, "sleep_ms" here ({"op": "sleep_ms", "ms": n}, at most 600,000). An unknown
//! operation, an unreadable script or a line that is not JSON ends it with the usage code; the first line that
//! returns non-zero ends it with that code.

pub mod ops;
pub mod shell;

use std::path::Path;

use serde_json::Value;

use crate::exit;

/// Where a script line goes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Route {
    Ops,
    Shell,
    Sleep,
    Unknown,
}

pub fn route(op: &str) -> Route {
    if op.starts_with("ops_") {
        Route::Ops
    } else if op.starts_with("shell_") || op == "probe-handle" {
        Route::Shell
    } else if op == "sleep_ms" {
        Route::Sleep
    } else {
        Route::Unknown
    }
}

/// Runs the script at `script`; returns the process exit code.
pub fn run(script: &Path) -> i32 {
    let Ok(text) = std::fs::read_to_string(script) else {
        return exit::USAGE;
    };
    run_lines(&text, &mut |l| match route(op_of(l)) {
        Route::Ops => ops::run_line(l),
        Route::Shell => shell::run_line(l),
        Route::Sleep | Route::Unknown => exit::USAGE,
    })
}

/// The loop of `run`, with the ops and shell lines handed to `dispatch` (tests pass a fake).
fn run_lines(text: &str, dispatch: &mut dyn FnMut(&Value) -> i32) -> i32 {
    for line in text.lines().filter(|l| !l.trim().is_empty()) {
        let Ok(v) = serde_json::from_str::<Value>(line) else {
            return exit::USAGE;
        };
        let code = match route(op_of(&v)) {
            Route::Sleep => {
                let ms = v.get("ms").and_then(Value::as_u64).unwrap_or(0).min(600_000);
                std::thread::sleep(std::time::Duration::from_millis(ms));
                exit::OK
            }
            Route::Unknown => return exit::USAGE,
            Route::Ops | Route::Shell => dispatch(&v),
        };
        if code != exit::OK {
            return code;
        }
    }
    exit::OK
}

fn op_of(v: &Value) -> &str {
    v.get("op").and_then(Value::as_str).unwrap_or("")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testkit::TempRoot;

    #[test]
    fn route_table() {
        for (op, want) in [
            ("ops_copy", Route::Ops),
            ("ops_", Route::Ops),
            ("shell_menu", Route::Shell),
            ("probe-handle", Route::Shell),
            ("sleep_ms", Route::Sleep),
            ("sleep", Route::Unknown),
            ("probe-handles", Route::Unknown),
            ("OPS_copy", Route::Unknown),
            ("", Route::Unknown),
        ] {
            assert_eq!(route(op), want, "{op}");
        }
    }

    #[test]
    fn script_lines_are_dispatched_in_order() {
        let mut seen = Vec::new();
        let script = "{\"op\":\"ops_a\"}\n\n{\"op\":\"sleep_ms\",\"ms\":1}\n{\"op\":\"shell_b\"}\n";
        let mut fake = |v: &Value| {
            seen.push(op_of(v).to_string());
            exit::OK
        };
        assert_eq!(run_lines(script, &mut fake), exit::OK);
        assert_eq!(seen, ["ops_a", "shell_b"]);
        let mut fail = |_: &Value| 7;
        assert_eq!(run_lines("{\"op\":\"ops_a\"}\n{\"op\":\"ops_b\"}", &mut fail), 7, "the first failing line ends it");
        let mut never = |_: &Value| -> i32 { panic!("not reached") };
        assert_eq!(run_lines("{\"op\":\"format_c\"}", &mut never), exit::USAGE, "an unknown op");
        assert_eq!(run_lines("not json", &mut never), exit::USAGE);
        assert_eq!(run_lines("{\"ms\":1}", &mut never), exit::USAGE, "a line without op");
    }

    #[test]
    fn run_reads_the_script_file() {
        let root = TempRoot::new("testclient");
        let script = root.file("s.jsonl", b"{\"op\":\"sleep_ms\",\"ms\":1}\n");
        assert_eq!(run(&script), exit::OK);
        assert_eq!(run(&root.at("missing.jsonl")), exit::USAGE);
        let bad = root.file("bad.jsonl", b"{\"op\":\"unknown\"}\n");
        assert_eq!(run(&bad), exit::USAGE);
    }
}
