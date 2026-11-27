//! --list-monitors: index, device, resolution, DPI and refresh rate per monitor (K1a.10).

use crate::cli::MeasureOpts;

/// Filled by K1a.10.
pub fn run(opts: &MeasureOpts) -> i32 {
    let _ = opts;
    super::not_ready("K1a.10")
}
