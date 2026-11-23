//! Design tokens of the launcher (A05 section 7). Values that exist in code are aliases, never copies.
// The whole table, on purpose: the tokens of L1b and L2 (compact rows, the width step of Settings > Launcher, the busy
// mark, danger and disabled colours) are here before their users. Constants only, so nothing else can hide behind it.
#![allow(dead_code)]

use crate::surface;

// ---- alphas (white on black)
pub const FG_PRIMARY: u32 = surface::TEXT_ALPHA;
pub const FG_SECONDARY: u32 = surface::TEXT2_ALPHA;
/// 128 in A05, 110 in code today: the X23 change moves Start and the launcher together.
pub const FG_TERTIARY: u32 = surface::TEXT3_ALPHA;
pub const FG_HOT: u32 = 255;
pub const FG_DISABLED: u32 = 90;

pub const FILL_CONTROL: u32 = 16;
pub const FILL_CONTROL_HOT: u32 = 28;
pub const RIM_CONTROL: u32 = 34;
pub const FILL_HOVER: u32 = surface::HOVER_ALPHA;
pub const FILL_PRESS: u32 = surface::PRESS_ALPHA;
pub const RIM_HOVER: u32 = surface::HOVER_RIM_ALPHA;
pub const FILL_TEXT_SELECTION: u32 = 70;
pub const RIM_FOCUS: u32 = 160;
pub const RIM_SURFACE: u32 = surface::PANEL_BORDER_ALPHA;

// ---- radii and spacing (logical px)
pub const R_L: i32 = surface::PANEL_RADIUS;
pub const S_4: i32 = surface::PANEL_PAD;
pub const R_S: i32 = surface::ROW_RADIUS;
pub const R_M: i32 = 10;
pub const R_XS: i32 = 4;
pub const S_3: i32 = 8;

pub const ICON_S: i32 = 16;
pub const ICON_M: i32 = 24;
pub const ICON_TEXT_GAP: i32 = 8;

pub const ROW_COMFORT: i32 = 44;
pub const ROW_COMPACT: i32 = 34;
/// Height of the framed search control, shared with the Start menu.
pub const SEARCH_H: i32 = crate::popup::popup_startmenu::SEARCH_H;

pub const ACCENT_ATTENTION: (u32, u32, u32) = crate::render::ATTENTION_RGB;
pub const ACCENT_DANGER: (u32, u32, u32) = (255, 69, 58);
/// Alpha of the running indicator under an icon.
pub const OPEN_INDICATOR: u32 = crate::render::OPEN_ALPHA;

pub const SCROLLBAR_W: i32 = 3;
pub const SCROLLBAR_REST: u32 = 50;
pub const SCROLLBAR_HOT: u32 = 110;

// ---- launcher-only
pub const LAUNCHER_W: i32 = 640;
pub const WIDTH_RANGE: core::ops::RangeInclusive<i32> = 560..=880;
pub const WIDTH_STEP: i32 = 40;
pub const ROWS_RANGE: core::ops::RangeInclusive<u8> = 4..=12;
pub const ROWS_DEFAULT: u8 = 8;
pub const TOP_PERCENT_DEFAULT: u8 = 24;
/// Milliseconds before the busy mark shows (files, L2).
pub const BUSY_DELAY: u32 = 300;

pub const FIELD_X: i32 = 12;
pub const FIELD_Y: i32 = 12;
/// The launcher's field (Q7, the 48 / 18 candidate): 48 px high, the query 18 px (`Font::Query`), the magnifier 16 px,
/// the size of every icon of the rows' glyph style, centred in the icon column of the rows, the caret as tall as the
/// query's line.
pub const FIELD_H: i32 = 48;
pub const FIELD_ICON: i32 = ICON_S;
pub const FIELD_CARET_H: i32 = 24;
/// Field bottom to the first row.
pub const FIELD_LIST_GAP: i32 = 8;
pub const LIST_Y: i32 = FIELD_Y + FIELD_H + FIELD_LIST_GAP;
pub const NAME_X: i32 = 56;
pub const ICON_X: i32 = 24;
pub const HINT_RIGHT_INSET: i32 = 24;
pub const SCROLLBAR_X_INSET: i32 = 8;
pub const MAX_REVIEW_LINES: usize = 3;
pub const REVIEW_LINE: i32 = 16;

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::indexing_slicing)]
mod tests {
    use super::*;

    #[test]
    fn tokens_match_a05() {
        assert_eq!((FG_PRIMARY, FG_SECONDARY), (230, 170));
        assert_eq!((FILL_CONTROL, FILL_CONTROL_HOT, RIM_CONTROL), (16, 28, 34));
        assert_eq!((FILL_HOVER, FILL_PRESS, RIM_HOVER), (20, 36, 18));
        assert_eq!((RIM_SURFACE, R_L, S_4, R_S), (22, 14, 12, 7));
        assert_eq!((ROW_COMFORT, ROW_COMPACT, SEARCH_H), (44, 34, 38));
        assert_eq!(OPEN_INDICATOR, 120);
        assert_eq!(ACCENT_ATTENTION, (255, 140, 0));
    }

    #[test]
    fn the_field_is_the_48_candidate_and_the_list_follows_it() {
        assert_eq!((FIELD_H, FIELD_CARET_H, FIELD_ICON), (48, 24, 16));
        assert_eq!(LIST_Y, FIELD_Y + FIELD_H + 8);
    }

    #[test]
    fn ranges_hold_their_defaults() {
        assert!(WIDTH_RANGE.contains(&LAUNCHER_W));
        assert_eq!(LAUNCHER_W % WIDTH_STEP, 0);
        assert!(ROWS_RANGE.contains(&ROWS_DEFAULT));
        assert!((10..=50).contains(&TOP_PERCENT_DEFAULT));
        assert_eq!((560..=880).step_by(WIDTH_STEP as usize).count(), 9);
    }
}
