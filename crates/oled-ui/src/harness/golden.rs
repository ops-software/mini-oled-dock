//! The golden comparator (SPEC-platform 5.7.1, ported: open point 12). Filled by K1a.5.

/// Compares `img` with the golden `<root>/<set>/<name>.png`; a mismatch writes a diff PNG under `diff` and panics with its
/// path. root = apps/gallery/tests/golden; the set is GOLDEN_SET (default "base", the CI runner's set); name =
/// "<page>/<cell>"; UPDATE_GOLDEN=1 writes the set (refused when CI is set unless GOLDEN_CI_REGEN); a page missing from
/// the base set fails on CI and only warns elsewhere. Until K1a.5: no comparison.
pub fn check(root: &std::path::Path, diff: &std::path::Path, name: &str, img: &super::Image) {
    let _ = (root, diff, name, img.w);
}
