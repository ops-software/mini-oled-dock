//! --preview <dir> (K1a.6, KA1): every page x the golden matrix to <dir>/<page>/<lang>-<dpi>[-<theme>].png.

use crate::cli::PreviewOpts;

/// Renders the pages offscreen. Filled by K1a.6.
pub fn run(opts: &PreviewOpts) -> i32 {
    let _ = opts;
    crate::measure::not_ready("K1a.6")
}
