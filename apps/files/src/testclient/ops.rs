//! The test client's ops lines ("ops_*"): drive an OpsClient from a script. Filled by F-OPS.

/// Runs one script line; 0 on success, else the exit code to end with.
pub fn run_line(line: &serde_json::Value) -> i32 {
    let _ = line;
    todo!("F-OPS")
}
