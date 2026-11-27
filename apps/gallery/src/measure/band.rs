//! S-BAND: the full-window table scrolled at 2000 px/s (K2a.11).

use crate::cli::MeasureOpts;

/// Filled by K2a.11.
pub fn run(opts: &MeasureOpts) -> i32 {
    let _ = opts;
    super::not_ready("K2a.11")
}
