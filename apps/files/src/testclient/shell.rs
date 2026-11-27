//! The test client's shell lines ("shell_*", "probe-handle"): drive a ShellClient or probe an inherited handle from a
//! script. Filled by F-SH.

/// Runs one script line; 0 on success, else the exit code to end with.
pub fn run_line(line: &serde_json::Value) -> i32 {
    let _ = line;
    todo!("F-SH")
}
