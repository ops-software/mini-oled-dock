//! --selftest --signal <nonce> (P2-plumbing F-P7): before any mutex, no window, no forward, within 10 s. Filled by
//! TA-SUITE (TS1).
#![allow(dead_code, unused_variables)] // stub: removed by TA-SUITE

use std::process::ExitCode;

/// TS1. Stub: exit 1 (failed).
pub fn run(nonce: &str) -> ExitCode {
    ExitCode::from(1)
}
