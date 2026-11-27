//! Design tokens (FZ-TOKENS): sizes in logical px, alpha out of 255 for fills and text, durations in ms.

// Colour: alpha out of 255.
/// Primary text alpha.
pub const FG_PRIMARY: u32 = 230;
/// Text alpha under the pointer.
pub const FG_HOT: u32 = 255;
/// Secondary text alpha.
pub const FG_SECONDARY: u32 = 170;
/// Secondary text alpha under the pointer.
pub const FG_SECONDARY_HOT: u32 = 215;
/// Tertiary text alpha.
pub const FG_TERTIARY: u32 = 128;
/// Disabled text alpha.
pub const FG_DISABLED: u32 = 90;
/// Alpha: fill hover.
pub const FILL_HOVER: u32 = 20;
/// Alpha: fill press.
pub const FILL_PRESS: u32 = 36;
/// Alpha: fill selected.
pub const FILL_SELECTED: u32 = 235;
/// Alpha: fill selected press.
pub const FILL_SELECTED_PRESS: u32 = 200;
/// Alpha: fill control.
pub const FILL_CONTROL: u32 = 16;
/// Alpha: fill control hot.
pub const FILL_CONTROL_HOT: u32 = 28;
/// Alpha: fill control off.
pub const FILL_CONTROL_OFF: u32 = 10;
/// Alpha: fill row selected.
pub const FILL_ROW_SELECTED: u32 = 48;
/// Alpha: fill row selected hot.
pub const FILL_ROW_SELECTED_HOT: u32 = 56;
/// Alpha: fill row selected inactive.
pub const FILL_ROW_SELECTED_INACTIVE: u32 = 28;
/// Alpha: fill text selection.
pub const FILL_TEXT_SELECTION: u32 = 70;
/// Alpha: rim surface.
pub const RIM_SURFACE: u32 = 22;
/// Alpha: rim control.
pub const RIM_CONTROL: u32 = 34;
/// Alpha: rim hover.
pub const RIM_HOVER: u32 = 18;
/// Alpha: rim focus.
pub const RIM_FOCUS: u32 = 160;
/// Alpha: separator.
pub const SEPARATOR: u32 = 30;
/// Surface opacity.
pub const SURFACE_OPACITY: u32 = 255;
/// Scrim alpha behind a modal surface.
pub const SCRIM: u32 = 153;
/// Alpha: chart fill.
pub const CHART_FILL: u32 = 24;
/// Alpha: chart grid.
pub const CHART_GRID: u32 = 14;
/// Alpha: heat 1.
pub const HEAT_1: u32 = 12;
/// Alpha: heat 2.
pub const HEAT_2: u32 = 20;
/// Alpha: heat 3.
pub const HEAT_3: u32 = 30;
/// Alpha: heat 4.
pub const HEAT_4: u32 = 44;
/// Scrollbar thumb alpha at rest.
pub const SCROLLBAR_REST: u32 = 50;
/// Scrollbar thumb alpha when hot or dragged.
pub const SCROLLBAR_HOT: u32 = 110;

// Colour: RGB.
/// Background.
pub const BG: (u32, u32, u32) = (0, 0, 0);
/// Text on a selected (light) fill.
pub const FG_ON_SELECTED: (u32, u32, u32) = (0, 0, 0);
/// Attention accent.
pub const ACCENT_ATTENTION: (u32, u32, u32) = (255, 140, 0);
/// Danger accent.
pub const ACCENT_DANGER: (u32, u32, u32) = (255, 69, 58);

// Type.
/// One text style: pixel size, line height, weight and whether it uses the display face.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TypeToken {
    pub px: i32,
    pub line: i32,
    pub weight: i32,
    pub display: bool,
}

/// Captions and small labels.
pub const CAPTION: TypeToken = TypeToken { px: 12, line: 16, weight: 400, display: false };
/// Body text.
pub const BODY: TypeToken = TypeToken { px: 13, line: 18, weight: 400, display: false };
/// Emphasised body text.
pub const BODY_STRONG: TypeToken = TypeToken { px: 13, line: 18, weight: 600, display: false };
/// Section headings.
pub const SECTION: TypeToken = TypeToken { px: 12, line: 16, weight: 600, display: false };
/// Window and page titles.
pub const TITLE: TypeToken = TypeToken { px: 20, line: 28, weight: 600, display: true };
/// Large figures.
pub const DISPLAY: TypeToken = TypeToken { px: 32, line: 40, weight: 300, display: true };

/// Faces for text, first choice first.
pub const FACE_TEXT: [&str; 2] = ["Segoe UI Variable Text", "Segoe UI"];
/// Faces for titles.
pub const FACE_TITLE: [&str; 2] = ["Segoe UI Variable Display", "Segoe UI"];
/// Faces for display figures.
pub const FACE_DISPLAY: [&str; 2] = ["Segoe UI Variable Display", "Segoe UI Light"];

// Space: logical px.
/// Space: s 1.
pub const S_1: i32 = 2;
/// Space: s 2.
pub const S_2: i32 = 4;
/// Space: s 3.
pub const S_3: i32 = 8;
/// Space: s 4.
pub const S_4: i32 = 12;
/// Space: s 5.
pub const S_5: i32 = 16;
/// Space: s 6.
pub const S_6: i32 = 20;
/// Space: s 7.
pub const S_7: i32 = 24;
/// Space: s 8.
pub const S_8: i32 = 32;
/// Space: text col.
pub const TEXT_COL: i32 = 20;
/// Space: text col icon.
pub const TEXT_COL_ICON: i32 = 48;
/// Space: icon text gap.
pub const ICON_TEXT_GAP: i32 = 8;

// Size: logical px.
/// Size: icon s.
pub const ICON_S: i32 = 16;
/// Size: icon m.
pub const ICON_M: i32 = 24;
/// Size: icon l.
pub const ICON_L: i32 = 32;
/// Size: row compact.
pub const ROW_COMPACT: i32 = 34;
/// Size: row detail.
pub const ROW_DETAIL: i32 = 28;
/// Size: row comfort.
pub const ROW_COMFORT: i32 = 44;
/// Size: row dense.
pub const ROW_DENSE: i32 = 24;
/// Size: control h.
pub const CONTROL_H: i32 = 28;
/// Size: toggle w.
pub const TOGGLE_W: i32 = 32;
/// Size: toggle h.
pub const TOGGLE_H: i32 = 18;
/// Size: toggle knob.
pub const TOGGLE_KNOB: i32 = 12;
/// Size: toggle knob pressed.
pub const TOGGLE_KNOB_PRESSED: i32 = 14;
/// Size: slider track.
pub const SLIDER_TRACK: i32 = 4;
/// Size: slider knob r.
pub const SLIDER_KNOB_R: i32 = 7;
/// Size: slider knob r hot.
pub const SLIDER_KNOB_R_HOT: i32 = 8;
/// Scrollbar thumb width at rest, in logical px.
pub const SCROLLBAR_W: i32 = 3;
/// Scrollbar thumb width when hot, in logical px.
pub const SCROLLBAR_HOT_W: i32 = 6;
/// Size: scrollbar hit.
pub const SCROLLBAR_HIT: i32 = 12;
/// Size: scroll thumb min.
pub const SCROLL_THUMB_MIN: i32 = 24;
/// Size: popup w s.
pub const POPUP_W_S: i32 = 280;
/// Size: popup w m.
pub const POPUP_W_M: i32 = 320;
/// Size: popup w l.
pub const POPUP_W_L: i32 = 360;
/// Size: focus gap.
pub const FOCUS_GAP: i32 = 2;
/// Size: titlebar h.
pub const TITLEBAR_H: i32 = 40;
/// Size: caption btn w.
pub const CAPTION_BTN_W: i32 = 46;
/// Size: tab h.
pub const TAB_H: i32 = 32;
/// Size: tab w min.
pub const TAB_W_MIN: i32 = 96;
/// Size: tab w max.
pub const TAB_W_MAX: i32 = 220;
/// Size: tab row h.
pub const TAB_ROW_H: i32 = 36;
/// Size: toolbar h.
pub const TOOLBAR_H: i32 = 44;
/// Size: status h.
pub const STATUS_H: i32 = 28;
/// Size: header h.
pub const HEADER_H: i32 = 28;
/// Size: nav w.
pub const NAV_W: i32 = 200;
/// Size: nav w min.
pub const NAV_W_MIN: i32 = 160;
/// Size: nav w max.
pub const NAV_W_MAX: i32 = 400;
/// Size: col min w.
pub const COL_MIN_W: i32 = 48;
/// Size: col resize hit.
pub const COL_RESIZE_HIT: i32 = 8;
/// Size: splitter hit.
pub const SPLITTER_HIT: i32 = 6;
/// Size: menu row.
pub const MENU_ROW: i32 = 28;
/// Size: menu text col.
pub const MENU_TEXT_COL: i32 = 36;
/// Size: menu w min.
pub const MENU_W_MIN: i32 = 200;
/// Size: menu w max.
pub const MENU_W_MAX: i32 = 360;
/// Size: chip h.
pub const CHIP_H: i32 = 24;
/// Size: toast h.
pub const TOAST_H: i32 = 40;
/// Size: toast w max.
pub const TOAST_W_MAX: i32 = 480;
/// Size: dialog w.
pub const DIALOG_W: i32 = 400;
/// Size: dialog w wide.
pub const DIALOG_W_WIDE: i32 = 480;
/// Size: progress h.
pub const PROGRESS_H: i32 = 2;
/// Size: dot attention.
pub const DOT_ATTENTION: i32 = 6;
/// Size: drag threshold.
pub const DRAG_THRESHOLD: i32 = 6;
/// Slide distance of a popup entering, in logical px.
pub const SLIDE_PX_POPUP: i32 = 4;
/// Slide distance of the bar entering, in logical px.
pub const SLIDE_PX_BAR: i32 = 10;
/// Nominal stroke width, in logical px.
pub const STROKE_NOMINAL: f32 = 1.3;

// Radius: logical px.
/// Corner radius: r xs.
pub const R_XS: i32 = 4;
/// Corner radius: r s.
pub const R_S: i32 = 7;
/// Corner radius: r m.
pub const R_M: i32 = 10;
/// Corner radius: r l.
pub const R_L: i32 = 14;
/// Corner radius: r bar.
pub const R_BAR: i32 = 11;

/// Corner radius of a pill of height `h`.
pub const fn r_pill(h: i32) -> i32 {
    h / 2
}

// Motion: milliseconds.
/// Duration: dur fast.
pub const DUR_FAST: u32 = 90;
/// Duration: dur normal.
pub const DUR_NORMAL: u32 = 140;
/// Duration: dur slow.
pub const DUR_SLOW: u32 = 160;
/// Duration: dur exit.
pub const DUR_EXIT: u32 = 83;
/// Duration: hover delay.
pub const HOVER_DELAY: u32 = 400;
/// Duration: tooltip delay.
pub const TOOLTIP_DELAY: u32 = 500;
/// Duration: tooltip warm.
pub const TOOLTIP_WARM: u32 = 1000;
/// Duration: submenu delay.
pub const SUBMENU_DELAY: u32 = 250;
/// Duration: toast hold.
pub const TOAST_HOLD: u32 = 4000;
/// Duration: busy delay.
pub const BUSY_DELAY: u32 = 300;
/// Duration: sweep period.
pub const SWEEP_PERIOD: u32 = 1200;
/// Duration: typeahead reset.
pub const TYPEAHEAD_RESET: u32 = 1000;
/// Duration: caret blink.
pub const CARET_BLINK: u32 = 500;
/// Duration: caret blink for.
pub const CARET_BLINK_FOR: u32 = 5000;

/// Focus ring width in device px: 1.5 logical px rounded half up.
pub const fn rim_focus_px(dpi: u32) -> i32 {
    (15 * dpi as i32 + 480) / 960
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tokens_match_the_reconciled_table() {
        assert_eq!(FG_PRIMARY, 230);
        assert_eq!(FG_HOT, 255);
        assert_eq!(FG_SECONDARY, 170);
        assert_eq!(FG_SECONDARY_HOT, 215);
        assert_eq!(FG_TERTIARY, 128);
        assert_eq!(FG_DISABLED, 90);
        assert_eq!(FILL_HOVER, 20);
        assert_eq!(FILL_PRESS, 36);
        assert_eq!(FILL_SELECTED, 235);
        assert_eq!(FILL_SELECTED_PRESS, 200);
        assert_eq!(FILL_CONTROL, 16);
        assert_eq!(FILL_CONTROL_HOT, 28);
        assert_eq!(FILL_CONTROL_OFF, 10);
        assert_eq!(FILL_ROW_SELECTED, 48);
        assert_eq!(FILL_ROW_SELECTED_HOT, 56);
        assert_eq!(FILL_ROW_SELECTED_INACTIVE, 28);
        assert_eq!(FILL_TEXT_SELECTION, 70);
        assert_eq!(RIM_SURFACE, 22);
        assert_eq!(RIM_CONTROL, 34);
        assert_eq!(RIM_HOVER, 18);
        assert_eq!(RIM_FOCUS, 160);
        assert_eq!(SEPARATOR, 30);
        assert_eq!(SURFACE_OPACITY, 255);
        assert_eq!(SCRIM, 153);
        assert_eq!(CHART_FILL, 24);
        assert_eq!(CHART_GRID, 14);
        assert_eq!(HEAT_1, 12);
        assert_eq!(HEAT_2, 20);
        assert_eq!(HEAT_3, 30);
        assert_eq!(HEAT_4, 44);
        assert_eq!(SCROLLBAR_REST, 50);
        assert_eq!(SCROLLBAR_HOT, 110);
        assert_eq!(BG, (0, 0, 0));
        assert_eq!(FG_ON_SELECTED, (0, 0, 0));
        assert_eq!(ACCENT_ATTENTION, (255, 140, 0));
        assert_eq!(ACCENT_DANGER, (255, 69, 58));
        assert_eq!(CAPTION, TypeToken { px: 12, line: 16, weight: 400, display: false });
        assert_eq!(BODY, TypeToken { px: 13, line: 18, weight: 400, display: false });
        assert_eq!(BODY_STRONG, TypeToken { px: 13, line: 18, weight: 600, display: false });
        assert_eq!(SECTION, TypeToken { px: 12, line: 16, weight: 600, display: false });
        assert_eq!(TITLE, TypeToken { px: 20, line: 28, weight: 600, display: true });
        assert_eq!(DISPLAY, TypeToken { px: 32, line: 40, weight: 300, display: true });
        assert_eq!(FACE_TEXT, ["Segoe UI Variable Text", "Segoe UI"]);
        assert_eq!(FACE_TITLE, ["Segoe UI Variable Display", "Segoe UI"]);
        assert_eq!(FACE_DISPLAY, ["Segoe UI Variable Display", "Segoe UI Light"]);
        assert_eq!(S_1, 2);
        assert_eq!(S_2, 4);
        assert_eq!(S_3, 8);
        assert_eq!(S_4, 12);
        assert_eq!(S_5, 16);
        assert_eq!(S_6, 20);
        assert_eq!(S_7, 24);
        assert_eq!(S_8, 32);
        assert_eq!(TEXT_COL, 20);
        assert_eq!(TEXT_COL_ICON, 48);
        assert_eq!(ICON_TEXT_GAP, 8);
        assert_eq!(ICON_S, 16);
        assert_eq!(ICON_M, 24);
        assert_eq!(ICON_L, 32);
        assert_eq!(ROW_COMPACT, 34);
        assert_eq!(ROW_DETAIL, 28);
        assert_eq!(ROW_COMFORT, 44);
        assert_eq!(ROW_DENSE, 24);
        assert_eq!(CONTROL_H, 28);
        assert_eq!(TOGGLE_W, 32);
        assert_eq!(TOGGLE_H, 18);
        assert_eq!(TOGGLE_KNOB, 12);
        assert_eq!(TOGGLE_KNOB_PRESSED, 14);
        assert_eq!(SLIDER_TRACK, 4);
        assert_eq!(SLIDER_KNOB_R, 7);
        assert_eq!(SLIDER_KNOB_R_HOT, 8);
        assert_eq!(SCROLLBAR_W, 3);
        assert_eq!(SCROLLBAR_HOT_W, 6);
        assert_eq!(SCROLLBAR_HIT, 12);
        assert_eq!(SCROLL_THUMB_MIN, 24);
        assert_eq!(POPUP_W_S, 280);
        assert_eq!(POPUP_W_M, 320);
        assert_eq!(POPUP_W_L, 360);
        assert_eq!(FOCUS_GAP, 2);
        assert_eq!(TITLEBAR_H, 40);
        assert_eq!(CAPTION_BTN_W, 46);
        assert_eq!(TAB_H, 32);
        assert_eq!(TAB_W_MIN, 96);
        assert_eq!(TAB_W_MAX, 220);
        assert_eq!(TAB_ROW_H, 36);
        assert_eq!(TOOLBAR_H, 44);
        assert_eq!(STATUS_H, 28);
        assert_eq!(HEADER_H, 28);
        assert_eq!(NAV_W, 200);
        assert_eq!(NAV_W_MIN, 160);
        assert_eq!(NAV_W_MAX, 400);
        assert_eq!(COL_MIN_W, 48);
        assert_eq!(COL_RESIZE_HIT, 8);
        assert_eq!(SPLITTER_HIT, 6);
        assert_eq!(MENU_ROW, 28);
        assert_eq!(MENU_TEXT_COL, 36);
        assert_eq!(MENU_W_MIN, 200);
        assert_eq!(MENU_W_MAX, 360);
        assert_eq!(CHIP_H, 24);
        assert_eq!(TOAST_H, 40);
        assert_eq!(TOAST_W_MAX, 480);
        assert_eq!(DIALOG_W, 400);
        assert_eq!(DIALOG_W_WIDE, 480);
        assert_eq!(PROGRESS_H, 2);
        assert_eq!(DOT_ATTENTION, 6);
        assert_eq!(DRAG_THRESHOLD, 6);
        assert_eq!(SLIDE_PX_POPUP, 4);
        assert_eq!(SLIDE_PX_BAR, 10);
        assert_eq!(R_XS, 4);
        assert_eq!(R_S, 7);
        assert_eq!(R_M, 10);
        assert_eq!(R_L, 14);
        assert_eq!(R_BAR, 11);
        assert_eq!(DUR_FAST, 90);
        assert_eq!(DUR_NORMAL, 140);
        assert_eq!(DUR_SLOW, 160);
        assert_eq!(DUR_EXIT, 83);
        assert_eq!(HOVER_DELAY, 400);
        assert_eq!(TOOLTIP_DELAY, 500);
        assert_eq!(TOOLTIP_WARM, 1000);
        assert_eq!(SUBMENU_DELAY, 250);
        assert_eq!(TOAST_HOLD, 4000);
        assert_eq!(BUSY_DELAY, 300);
        assert_eq!(SWEEP_PERIOD, 1200);
        assert_eq!(TYPEAHEAD_RESET, 1000);
        assert_eq!(CARET_BLINK, 500);
        assert_eq!(CARET_BLINK_FOR, 5000);
        assert_eq!(STROKE_NOMINAL, 1.3_f32);
    }

    #[test]
    fn focus_ring_rounds_half_up() {
        assert_eq!(rim_focus_px(96), 2);
        assert_eq!(rim_focus_px(120), 2);
        assert_eq!(rim_focus_px(144), 2);
        assert_eq!(rim_focus_px(192), 3);
    }

    #[test]
    fn pill_is_half_the_height() {
        assert_eq!(r_pill(28), 14);
    }
}
