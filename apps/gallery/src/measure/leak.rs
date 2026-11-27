//! Open and close every page N times: private, GDI and USER deltas (K1a.11).

use crate::cli::MeasureOpts;

/// Filled by K1a.11.
pub fn run(opts: &MeasureOpts) -> i32 {
    let _ = opts;
    super::not_ready("K1a.11")
}
