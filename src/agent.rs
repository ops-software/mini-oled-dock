//! The medium agent (--agent <nonce>). Lands with AG2; until then the flag says so.
#![allow(dead_code, unused_variables)]

/// `--agent <nonce>`: AG2.
pub fn run(nonce: &str) -> i32 {
    crate::logger::say("--agent: not available in this build");
    2
}
