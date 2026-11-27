//! files.exe --selftest --signal <nonce> (P3-files-app 2.2; FA-ST fills it, F1a.18, P2-plumbing F-P7).

/// Filled by FA-ST; until then 1 (it never signals).
pub fn run(nonce: &str) -> i32 {
    let _ = nonce;
    1
}
