//! The gallery on screen (K1a.11, KA2): the index and the selected page, or one page with --page.

use crate::cli::WindowOpts;

/// Opens the gallery window and runs its loop. Filled by K1a.11.
pub fn run(opts: &WindowOpts) -> i32 {
    let _ = opts;
    crate::measure::not_ready("K1a.11")
}
