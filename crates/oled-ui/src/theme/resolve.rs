//! Token resolution (P2-kit 2.2, 2.7): the OLED table written out and the resolver from a ThemeSpec. The resolver's rules
//! and the F-P2 mapping are filled by K1a.1.

use super::{ColorTokens, Ink, MotionTokens, RadiusTokens, SizeTokens, SpaceTokens, SysOverlay, ThemeSpec, Tokens};
use crate::tokens as t;

impl Tokens {
    /// The OLED tokens: A05 section 7 and ROADMAP 2.3 (table 2.7), written out. Equals resolve(ThemeSpec::oled(),
    /// SysOverlay::none()).
    pub fn oled() -> Tokens {
        let w = Ink::white;
        Tokens {
            color: ColorTokens {
                bg: Ink { rgb: t::BG, a: 255 },
                fg_primary: w(t::FG_PRIMARY),
                fg_hot: w(t::FG_HOT),
                fg_secondary: w(t::FG_SECONDARY),
                fg_secondary_hot: w(t::FG_SECONDARY_HOT),
                fg_tertiary: w(t::FG_TERTIARY),
                fg_disabled: w(t::FG_DISABLED),
                fg_on_selected: Ink { rgb: t::FG_ON_SELECTED, a: 255 },
                fill_hover: w(t::FILL_HOVER),
                fill_press: w(t::FILL_PRESS),
                fill_selected: w(t::FILL_SELECTED),
                fill_selected_hot: w(255),
                fill_selected_press: w(t::FILL_SELECTED_PRESS),
                fill_control: w(t::FILL_CONTROL),
                fill_control_hot: w(t::FILL_CONTROL_HOT),
                fill_control_off: w(t::FILL_CONTROL_OFF),
                fill_row_selected: w(t::FILL_ROW_SELECTED),
                fill_row_selected_hot: w(t::FILL_ROW_SELECTED_HOT),
                fill_row_selected_inactive: w(t::FILL_ROW_SELECTED_INACTIVE),
                fill_text_selection: w(t::FILL_TEXT_SELECTION),
                rim_surface: w(t::RIM_SURFACE),
                rim_control: w(t::RIM_CONTROL),
                rim_hover: w(t::RIM_HOVER),
                rim_focus: w(t::RIM_FOCUS),
                separator: w(t::SEPARATOR),
                scrim: Ink { rgb: t::BG, a: t::SCRIM },
                scrollbar: w(t::SCROLLBAR_REST),
                scrollbar_hot: w(t::SCROLLBAR_HOT),
                heat: [w(t::HEAT_1), w(t::HEAT_2), w(t::HEAT_3), w(t::HEAT_4)],
                chart_fill: w(t::CHART_FILL),
                chart_grid: w(t::CHART_GRID),
                accent_attention: Ink { rgb: t::ACCENT_ATTENTION, a: 255 },
                accent_danger: Ink { rgb: t::ACCENT_DANGER, a: 255 },
                accent: None,
                toggle_off_track: w(40),
                toggle_off_rim: w(70),
                toggle_off_knob: w(160),
                slider_rail: w(60),
                seg_track: w(14),
                seg_rim: w(40),
                seg_hot: w(26),
                disabled_veil: Ink { rgb: t::BG, a: 150 },
                nav_bar: w(255),
            },
            size: SizeTokens {
                control_h: t::CONTROL_H,
                row_compact: t::ROW_COMPACT,
                row_detail: t::ROW_DETAIL,
                row_comfort: t::ROW_COMFORT,
                row_dense: t::ROW_DENSE,
                row_table: 28,
                header_h: t::HEADER_H,
                toolbar_h: t::TOOLBAR_H,
                status_h: t::STATUS_H,
                titlebar_h: t::TITLEBAR_H,
                caption_btn_w: t::CAPTION_BTN_W,
                tab_h: t::TAB_H,
                tab_w_min: t::TAB_W_MIN,
                tab_w_max: t::TAB_W_MAX,
                tab_row_h: t::TAB_ROW_H,
                nav_w: t::NAV_W,
                col_min_w: t::COL_MIN_W,
                col_resize_hit: t::COL_RESIZE_HIT,
                splitter_hit: t::SPLITTER_HIT,
                scrollbar_w: t::SCROLLBAR_W,
                scrollbar_hot_w: t::SCROLLBAR_HOT_W,
                scrollbar_hit: t::SCROLLBAR_HIT,
                scroll_thumb_min: t::SCROLL_THUMB_MIN,
                menu_row: t::MENU_ROW,
                menu_text_col: t::MENU_TEXT_COL,
                menu_w_min: t::MENU_W_MIN,
                menu_w_max: t::MENU_W_MAX,
                chip_h: t::CHIP_H,
                toast_h: t::TOAST_H,
                toast_w_max: t::TOAST_W_MAX,
                dialog_w: t::DIALOG_W,
                dialog_w_wide: t::DIALOG_W_WIDE,
                progress_h: t::PROGRESS_H,
                icon_s: t::ICON_S,
                icon_m: t::ICON_M,
                icon_l: t::ICON_L,
                text_col: t::TEXT_COL,
                text_col_icon: t::TEXT_COL_ICON,
                icon_text_gap: t::ICON_TEXT_GAP,
                focus_gap: t::FOCUS_GAP,
                drag_threshold: t::DRAG_THRESHOLD,
                dot_attention: t::DOT_ATTENTION,
                toggle_w: t::TOGGLE_W,
                toggle_h: t::TOGGLE_H,
                toggle_knob: t::TOGGLE_KNOB,
                toggle_knob_pressed: t::TOGGLE_KNOB_PRESSED,
                slider_track: t::SLIDER_TRACK,
                slider_knob_r: t::SLIDER_KNOB_R,
                slider_knob_r_hot: t::SLIDER_KNOB_R_HOT,
            },
            space: SpaceTokens {
                s1: t::S_1,
                s2: t::S_2,
                s3: t::S_3,
                s4: t::S_4,
                s5: t::S_5,
                s6: t::S_6,
                s7: t::S_7,
                s8: t::S_8,
            },
            radius: RadiusTokens { xs: t::R_XS, s: t::R_S, m: t::R_M, l: t::R_L, bar: t::R_BAR },
            motion: MotionTokens {
                dur_fast: t::DUR_FAST,
                dur_normal: t::DUR_NORMAL,
                dur_slow: t::DUR_SLOW,
                dur_exit: t::DUR_EXIT,
                dur_progress: 250,
                hover_delay: t::HOVER_DELAY,
                tooltip_delay: t::TOOLTIP_DELAY,
                tooltip_warm: t::TOOLTIP_WARM,
                submenu_delay: t::SUBMENU_DELAY,
                busy_delay: t::BUSY_DELAY,
                sweep_period: t::SWEEP_PERIOD,
                toast_hold: t::TOAST_HOLD,
                typeahead_reset: t::TYPEAHEAD_RESET,
                caret_blink: t::CARET_BLINK,
                caret_blink_for: t::CARET_BLINK_FOR,
                spring_delay: 600,
                slide_px: t::SLIDE_PX_POPUP,
                reduced: false,
            },
            text_scale: 1.0,
            generation: 0,
            high_contrast: false,
        }
    }

    /// The tokens of `spec` under `sys`: the dock's derivation rules, the F-P2 mapping and the overrides, then
    /// sys.with_theme(&spec.a11y) last. Filled by K1a.1; until then it returns Tokens::oled().
    pub fn resolve(spec: &ThemeSpec, sys: &SysOverlay) -> Tokens {
        let _ = (spec, sys);
        Tokens::oled()
    }

    /// The overrides resolve ignores (an unknown name or a value out of range), one line each. Filled by K1a.1; until
    /// then it reports nothing.
    pub fn resolve_report(spec: &ThemeSpec) -> Vec<String> {
        let _ = spec;
        Vec::new()
    }
}
