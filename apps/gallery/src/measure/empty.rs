//! S-EMPTY: the empty kit window's private bytes and first present (K1a.7).

use crate::cli::MeasureOpts;

/// Filled by K1a.7.
pub fn run(opts: &MeasureOpts) -> i32 {
    let _ = opts;
    super::not_ready("K1a.7")
}
