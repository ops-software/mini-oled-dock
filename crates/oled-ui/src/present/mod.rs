//! Presenting a window's pixels (P2-kit 2.5): the pure part apps may read; the presenters are internal (K1a.9, K2a.10).

pub(crate) mod banded;
pub(crate) mod layered;
pub(crate) mod scroll;

/// Rows per paint band at a width in physical px: clamp(1 MiB / (4 * width), 32, 256), so a band DIB never exceeds
/// 1 MiB at any width.
pub fn band_height(width_px: i32) -> i32 {
    if width_px <= 0 {
        return 256;
    }
    ((1 << 20) / (4 * i64::from(width_px))).clamp(32, 256) as i32
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bands_stay_under_one_mib() {
        assert_eq!((band_height(1920), band_height(3840), band_height(100), band_height(20000)), (136, 68, 256, 32));
        for w in [1, 640, 1366, 2560, 4096, 7680] {
            assert!(i64::from(band_height(w)) * i64::from(w) * 4 <= 1 << 20 || band_height(w) == 32);
        }
        assert_eq!(band_height(0), 256);
    }
}
