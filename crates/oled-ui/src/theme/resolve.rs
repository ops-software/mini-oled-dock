//! Token resolution (P2-kit 2.2, 2.7): the OLED table written out and the resolver from a ThemeSpec. The resolver's rules
//! and the F-P2 mapping are filled by K1a.1.

use super::{
    ColorTokens, Corners, HcColors, Ink, Motion, MotionTokens, RadiusTokens, SizeTokens, SpaceTokens, SysOverlay,
    ThemeSpec, Tokens,
};
use crate::tokens as t;

impl Tokens {
    /// The OLED tokens: A05 section 7 and ROADMAP 2.3 (table 2.7), written out. Equals resolve(ThemeSpec::oled(),
    /// SysOverlay::none()), generation included.
    pub fn oled() -> Tokens {
        let mut tk = Tokens::table();
        tk.generation = fnv1a64(format!("{tk:?}").as_bytes());
        tk
    }

    /// The written-out table with generation 0 (the base of resolve, which stamps the generation itself).
    fn table() -> Tokens {
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
    /// sys.with_theme(&spec.a11y) last (it overrides the Look and any preset). Every override is clamped to the range of
    /// its token, whatever the file says.
    pub fn resolve(spec: &ThemeSpec, sys: &SysOverlay) -> Tokens {
        let mut tk = Tokens::table();
        let seed = &spec.look.seed;
        let knobs = &spec.look.knobs;
        let bg = (u32::from(seed.bg.0), u32::from(seed.bg.1), u32::from(seed.bg.2));
        let fg = (u32::from(seed.fg.0), u32::from(seed.fg.1), u32::from(seed.fg.2));

        // Colours: seed.bg, seed.fg as the rgb of every white-at-alpha token.
        {
            let c = &mut tk.color;
            c.bg.rgb = bg;
            c.fg_on_selected.rgb = bg;
            c.scrim.rgb = bg;
            c.disabled_veil.rgb = bg;
            for ink in white_inks(c) {
                ink.rgb = fg;
            }
            c.accent_attention.rgb = rgb3(seed.attention);
            c.accent_danger.rgb = rgb3(seed.danger);
            // The accent is dropped below 4.5:1 against the background.
            c.accent = seed.accent.and_then(|a| {
                let rgb = rgb3(a);
                (contrast(rgb, bg) >= 4.5).then_some(Ink::solid(rgb.0, rgb.1, rgb.2))
            });
            // White level: the text and the selected fill scale, the text keeps its floors.
            let pct = u32::from(seed.white_level).clamp(70, 100);
            let w = (255 * pct + 50) / 100;
            let scale = |ink: &mut Ink| ink.a = (ink.a * w + 127) / 255;
            scale(&mut c.fg_primary);
            scale(&mut c.fg_secondary);
            scale(&mut c.fg_secondary_hot);
            scale(&mut c.fg_hot);
            scale(&mut c.fill_selected);
            for ink in
                [&mut c.fg_primary, &mut c.fg_hot, &mut c.fg_secondary, &mut c.fg_secondary_hot, &mut c.fg_tertiary]
            {
                ink.a = ink.a.max(128);
            }
        }

        // Corners and density.
        let k = match knobs.corners {
            Corners::Square => 0.0f32,
            Corners::Half => 0.5,
            Corners::Normal => 1.0,
            Corners::Round => 1.3,
        };
        let r = |v: i32| (v as f32 * k).round() as i32;
        let rad = tk.radius;
        tk.radius = RadiusTokens { xs: r(rad.xs), s: r(rad.s), m: r(rad.m), l: r(rad.l), bar: r(rad.bar) };
        let density = u32::from(knobs.density_pct).clamp(85, 115);
        tk.size.row_table = if density < 95 {
            24
        } else if density > 105 {
            34
        } else {
            28
        };

        // Motion: the speed first; the mode and the system state zero the durations below.
        let speed = u32::from(knobs.motion.speed_pct).clamp(75, 125);
        for d in durations_mut(&mut tk.motion) {
            *d = (*d * speed + 50) / 100;
        }

        // Overrides by name, each clamped to its token's range.
        for (name, v) in &spec.overrides {
            if let Some(row) = OVERRIDES.iter().find(|o| o.name == name.as_str()) {
                (row.set)(&mut tk, (*v).clamp(row.min, row.max));
            }
        }

        // The system's accessibility state over everything, the theme's a11y overrides first.
        let sys = sys.with_theme(&spec.a11y);
        let mode = knobs.motion.mode;
        let off = matches!(mode, Motion::Reduced | Motion::Off)
            || (mode == Motion::System && sys.reduced_motion)
            || (mode == Motion::Full && spec.a11y.reduced_motion == Some(true));
        if off {
            tk.motion.reduced = true;
            let m = &mut tk.motion;
            for d in durations_mut(m) {
                *d = 0;
            }
            m.caret_blink = 0;
            m.caret_blink_for = 0;
            m.sweep_period = 0;
            m.slide_px = 0;
        }
        tk.text_scale = sys.text_scale;
        if let Some(hc) = sys.high_contrast {
            apply_high_contrast(&mut tk.color, &hc);
            tk.high_contrast = true;
        }
        tk.generation = 0;
        tk.generation = fnv1a64(format!("{tk:?}").as_bytes());
        tk
    }

    /// The overrides resolve ignores or clamps (an unknown name, a value out of range), one line each.
    pub fn resolve_report(spec: &ThemeSpec) -> Vec<String> {
        let mut out = Vec::new();
        for (name, v) in &spec.overrides {
            match OVERRIDES.iter().find(|o| o.name == name.as_str()) {
                None => out.push(format!("override {name}: unknown token, ignored")),
                Some(row) if *v < row.min || *v > row.max => {
                    out.push(format!("override {name}: {v} is outside {}..={}, clamped", row.min, row.max))
                }
                Some(_) => {}
            }
        }
        out
    }
}

fn rgb3(c: super::Rgb) -> (u32, u32, u32) {
    (u32::from(c.0), u32::from(c.1), u32::from(c.2))
}

fn lin(c: u32) -> f64 {
    let s = f64::from(c) / 255.0;
    if s <= 0.03928 {
        s / 12.92
    } else {
        ((s + 0.055) / 1.055).powf(2.4)
    }
}

/// WCAG relative luminance.
fn luminance(c: (u32, u32, u32)) -> f64 {
    0.2126 * lin(c.0) + 0.7152 * lin(c.1) + 0.0722 * lin(c.2)
}

fn contrast(a: (u32, u32, u32), b: (u32, u32, u32)) -> f64 {
    let (la, lb) = (luminance(a), luminance(b));
    (la.max(lb) + 0.05) / (la.min(lb) + 0.05)
}

fn fnv1a64(bytes: &[u8]) -> u64 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for b in bytes {
        h ^= u64::from(*b);
        h = h.wrapping_mul(0x0000_0100_0000_01b3);
    }
    h
}

/// The animation durations (not the delays), for the speed knob and the reduced-motion zeroing.
fn durations_mut(m: &mut MotionTokens) -> [&mut u32; 5] {
    [&mut m.dur_fast, &mut m.dur_normal, &mut m.dur_slow, &mut m.dur_exit, &mut m.dur_progress]
}

/// Every token drawn as white at an alpha: the seed's fg is their rgb.
fn white_inks(c: &mut ColorTokens) -> Vec<&mut Ink> {
    let [h1, h2, h3, h4] = &mut c.heat;
    vec![
        &mut c.fg_primary,
        &mut c.fg_hot,
        &mut c.fg_secondary,
        &mut c.fg_secondary_hot,
        &mut c.fg_tertiary,
        &mut c.fg_disabled,
        &mut c.fill_hover,
        &mut c.fill_press,
        &mut c.fill_selected,
        &mut c.fill_selected_hot,
        &mut c.fill_selected_press,
        &mut c.fill_control,
        &mut c.fill_control_hot,
        &mut c.fill_control_off,
        &mut c.fill_row_selected,
        &mut c.fill_row_selected_hot,
        &mut c.fill_row_selected_inactive,
        &mut c.fill_text_selection,
        &mut c.rim_surface,
        &mut c.rim_control,
        &mut c.rim_hover,
        &mut c.rim_focus,
        &mut c.separator,
        &mut c.scrollbar,
        &mut c.scrollbar_hot,
        h1,
        h2,
        h3,
        h4,
        &mut c.chart_fill,
        &mut c.chart_grid,
        &mut c.toggle_off_track,
        &mut c.toggle_off_rim,
        &mut c.toggle_off_knob,
        &mut c.slider_rail,
        &mut c.seg_track,
        &mut c.seg_rim,
        &mut c.seg_hot,
        &mut c.nav_bar,
    ]
}

/// High contrast (SPEC-platform 5.3.3): the system colours replace the palette.
fn apply_high_contrast(c: &mut ColorTokens, hc: &HcColors) {
    let solid = |rgb: (u32, u32, u32)| Ink { rgb, a: 255 };
    let (win, text, hi, hi_text, gray) = (hc.window, hc.window_text, hc.highlight, hc.highlight_text, hc.gray_text);
    c.bg = solid(win);
    for ink in white_inks(c) {
        *ink = solid(text);
    }
    c.fg_disabled = solid(gray);
    c.fg_hot = solid(hi_text);
    c.fg_on_selected = solid(hi_text);
    for ink in [
        &mut c.fill_hover,
        &mut c.fill_press,
        &mut c.fill_selected,
        &mut c.fill_selected_hot,
        &mut c.fill_selected_press,
        &mut c.fill_row_selected,
        &mut c.fill_row_selected_hot,
        &mut c.fill_row_selected_inactive,
        &mut c.fill_control_hot,
        &mut c.fill_text_selection,
    ] {
        *ink = solid(hi);
    }
    for ink in [&mut c.fill_control, &mut c.fill_control_off] {
        *ink = solid(win);
    }
    c.rim_focus = solid(hi);
    c.heat = [solid(win); 4];
    c.chart_fill = solid(win);
    c.scrim = Ink { rgb: win, a: t::SCRIM };
    c.disabled_veil = Ink { rgb: win, a: 150 };
    c.accent_attention = solid(text);
    c.accent_danger = solid(text);
    c.accent = None;
}

/// One override: a token's name (its constant's), its range and where it lands.
struct OverrideRow {
    name: &'static str,
    min: i64,
    max: i64,
    set: fn(&mut Tokens, i64),
}

macro_rules! rows {
    ($min:expr, $max:expr, $cast:ty; $( $name:literal => |$tk:ident, $v:ident| $body:expr ),* $(,)?) => {
        &[ $( OverrideRow { name: $name, min: $min, max: $max, set: |$tk, $v| { let $v = $v as $cast; $body } } ),* ]
    };
}

static ALPHAS: &[OverrideRow] = rows!(0, 255, u32;
    "FG_PRIMARY" => |t, v| t.color.fg_primary.a = v,
    "FG_HOT" => |t, v| t.color.fg_hot.a = v,
    "FG_SECONDARY" => |t, v| t.color.fg_secondary.a = v,
    "FG_SECONDARY_HOT" => |t, v| t.color.fg_secondary_hot.a = v,
    "FG_TERTIARY" => |t, v| t.color.fg_tertiary.a = v,
    "FG_DISABLED" => |t, v| t.color.fg_disabled.a = v,
    "FILL_HOVER" => |t, v| t.color.fill_hover.a = v,
    "FILL_PRESS" => |t, v| t.color.fill_press.a = v,
    "FILL_SELECTED" => |t, v| t.color.fill_selected.a = v,
    "FILL_SELECTED_PRESS" => |t, v| t.color.fill_selected_press.a = v,
    "FILL_CONTROL" => |t, v| t.color.fill_control.a = v,
    "FILL_CONTROL_HOT" => |t, v| t.color.fill_control_hot.a = v,
    "FILL_CONTROL_OFF" => |t, v| t.color.fill_control_off.a = v,
    "FILL_ROW_SELECTED" => |t, v| t.color.fill_row_selected.a = v,
    "FILL_ROW_SELECTED_HOT" => |t, v| t.color.fill_row_selected_hot.a = v,
    "FILL_ROW_SELECTED_INACTIVE" => |t, v| t.color.fill_row_selected_inactive.a = v,
    "FILL_TEXT_SELECTION" => |t, v| t.color.fill_text_selection.a = v,
    "RIM_SURFACE" => |t, v| t.color.rim_surface.a = v,
    "RIM_CONTROL" => |t, v| t.color.rim_control.a = v,
    "RIM_HOVER" => |t, v| t.color.rim_hover.a = v,
    "RIM_FOCUS" => |t, v| t.color.rim_focus.a = v,
    "SEPARATOR" => |t, v| t.color.separator.a = v,
    "SCRIM" => |t, v| t.color.scrim.a = v,
    "CHART_FILL" => |t, v| t.color.chart_fill.a = v,
    "CHART_GRID" => |t, v| t.color.chart_grid.a = v,
    "HEAT_1" => |t, v| t.color.heat[0].a = v,
    "HEAT_2" => |t, v| t.color.heat[1].a = v,
    "HEAT_3" => |t, v| t.color.heat[2].a = v,
    "HEAT_4" => |t, v| t.color.heat[3].a = v,
    "SCROLLBAR_REST" => |t, v| t.color.scrollbar.a = v,
    "SCROLLBAR_HOT" => |t, v| t.color.scrollbar_hot.a = v,
    "FILL_SELECTED_HOT" => |t, v| t.color.fill_selected_hot.a = v,
    "TOGGLE_OFF_TRACK" => |t, v| t.color.toggle_off_track.a = v,
    "TOGGLE_OFF_RIM" => |t, v| t.color.toggle_off_rim.a = v,
    "TOGGLE_OFF_KNOB" => |t, v| t.color.toggle_off_knob.a = v,
    "SLIDER_RAIL" => |t, v| t.color.slider_rail.a = v,
    "SEG_TRACK" => |t, v| t.color.seg_track.a = v,
    "SEG_RIM" => |t, v| t.color.seg_rim.a = v,
    "SEG_HOT" => |t, v| t.color.seg_hot.a = v,
    "DISABLED_VEIL" => |t, v| t.color.disabled_veil.a = v,
    "NAV_BAR" => |t, v| t.color.nav_bar.a = v,
);

static RADII: &[OverrideRow] = rows!(0, 24, i32;
    "R_XS" => |t, v| t.radius.xs = v,
    "R_S" => |t, v| t.radius.s = v,
    "R_M" => |t, v| t.radius.m = v,
    "R_L" => |t, v| t.radius.l = v,
    "R_BAR" => |t, v| t.radius.bar = v,
);

static SIZES: &[OverrideRow] = rows!(0, 400, i32;
    "CONTROL_H" => |t, v| t.size.control_h = v,
    "ROW_COMPACT" => |t, v| t.size.row_compact = v,
    "ROW_DETAIL" => |t, v| t.size.row_detail = v,
    "ROW_COMFORT" => |t, v| t.size.row_comfort = v,
    "ROW_DENSE" => |t, v| t.size.row_dense = v,
    "ROW_TABLE" => |t, v| t.size.row_table = v,
    "HEADER_H" => |t, v| t.size.header_h = v,
    "TOOLBAR_H" => |t, v| t.size.toolbar_h = v,
    "STATUS_H" => |t, v| t.size.status_h = v,
    "TITLEBAR_H" => |t, v| t.size.titlebar_h = v,
    "CAPTION_BTN_W" => |t, v| t.size.caption_btn_w = v,
    "TAB_H" => |t, v| t.size.tab_h = v,
    "TAB_W_MIN" => |t, v| t.size.tab_w_min = v,
    "TAB_W_MAX" => |t, v| t.size.tab_w_max = v,
    "TAB_ROW_H" => |t, v| t.size.tab_row_h = v,
    "NAV_W" => |t, v| t.size.nav_w = v,
    "COL_MIN_W" => |t, v| t.size.col_min_w = v,
    "COL_RESIZE_HIT" => |t, v| t.size.col_resize_hit = v,
    "SPLITTER_HIT" => |t, v| t.size.splitter_hit = v,
    "SCROLLBAR_W" => |t, v| t.size.scrollbar_w = v,
    "SCROLLBAR_HOT_W" => |t, v| t.size.scrollbar_hot_w = v,
    "SCROLLBAR_HIT" => |t, v| t.size.scrollbar_hit = v,
    "SCROLL_THUMB_MIN" => |t, v| t.size.scroll_thumb_min = v,
    "MENU_ROW" => |t, v| t.size.menu_row = v,
    "MENU_TEXT_COL" => |t, v| t.size.menu_text_col = v,
    "MENU_W_MIN" => |t, v| t.size.menu_w_min = v,
    "MENU_W_MAX" => |t, v| t.size.menu_w_max = v,
    "CHIP_H" => |t, v| t.size.chip_h = v,
    "TOAST_H" => |t, v| t.size.toast_h = v,
    "TOAST_W_MAX" => |t, v| t.size.toast_w_max = v,
    "DIALOG_W" => |t, v| t.size.dialog_w = v,
    "DIALOG_W_WIDE" => |t, v| t.size.dialog_w_wide = v,
    "PROGRESS_H" => |t, v| t.size.progress_h = v,
    "ICON_S" => |t, v| t.size.icon_s = v,
    "ICON_M" => |t, v| t.size.icon_m = v,
    "ICON_L" => |t, v| t.size.icon_l = v,
    "TEXT_COL" => |t, v| t.size.text_col = v,
    "TEXT_COL_ICON" => |t, v| t.size.text_col_icon = v,
    "ICON_TEXT_GAP" => |t, v| t.size.icon_text_gap = v,
    "FOCUS_GAP" => |t, v| t.size.focus_gap = v,
    "DRAG_THRESHOLD" => |t, v| t.size.drag_threshold = v,
    "DOT_ATTENTION" => |t, v| t.size.dot_attention = v,
    "TOGGLE_W" => |t, v| t.size.toggle_w = v,
    "TOGGLE_H" => |t, v| t.size.toggle_h = v,
    "TOGGLE_KNOB" => |t, v| t.size.toggle_knob = v,
    "TOGGLE_KNOB_PRESSED" => |t, v| t.size.toggle_knob_pressed = v,
    "SLIDER_TRACK" => |t, v| t.size.slider_track = v,
    "SLIDER_KNOB_R" => |t, v| t.size.slider_knob_r = v,
    "SLIDER_KNOB_R_HOT" => |t, v| t.size.slider_knob_r_hot = v,
    "S_1" => |t, v| t.space.s1 = v,
    "S_2" => |t, v| t.space.s2 = v,
    "S_3" => |t, v| t.space.s3 = v,
    "S_4" => |t, v| t.space.s4 = v,
    "S_5" => |t, v| t.space.s5 = v,
    "S_6" => |t, v| t.space.s6 = v,
);

static SIZES_MORE: &[OverrideRow] = rows!(0, 400, i32;
    "S_7" => |t, v| t.space.s7 = v,
    "S_8" => |t, v| t.space.s8 = v,
    "SLIDE_PX" => |t, v| t.motion.slide_px = v,
);

static DURATIONS: &[OverrideRow] = rows!(0, 2000, u32;
    "DUR_FAST" => |t, v| t.motion.dur_fast = v,
    "DUR_NORMAL" => |t, v| t.motion.dur_normal = v,
    "DUR_SLOW" => |t, v| t.motion.dur_slow = v,
    "DUR_EXIT" => |t, v| t.motion.dur_exit = v,
    "DUR_PROGRESS" => |t, v| t.motion.dur_progress = v,
    "HOVER_DELAY" => |t, v| t.motion.hover_delay = v,
    "TOOLTIP_DELAY" => |t, v| t.motion.tooltip_delay = v,
    "TOOLTIP_WARM" => |t, v| t.motion.tooltip_warm = v,
    "SUBMENU_DELAY" => |t, v| t.motion.submenu_delay = v,
    "BUSY_DELAY" => |t, v| t.motion.busy_delay = v,
    "SWEEP_PERIOD" => |t, v| t.motion.sweep_period = v,
    "TOAST_HOLD" => |t, v| t.motion.toast_hold = v,
    "TYPEAHEAD_RESET" => |t, v| t.motion.typeahead_reset = v,
    "CARET_BLINK" => |t, v| t.motion.caret_blink = v,
    "CARET_BLINK_FOR" => |t, v| t.motion.caret_blink_for = v,
    "SPRING_DELAY" => |t, v| t.motion.spring_delay = v,
);

/// The override table: every name of table 2.7 in its constant's spelling.
struct Overrides;
static OVERRIDES: Overrides = Overrides;
impl Overrides {
    fn iter(&self) -> impl Iterator<Item = &'static OverrideRow> {
        ALPHAS.iter().chain(RADII.iter()).chain(SIZES.iter()).chain(SIZES_MORE.iter()).chain(DURATIONS.iter())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::theme::{
        A11y, FontSpec, IndicatorStyle, Knobs, Look, MotionSpec, PlateStyle, Preset, Rgb, RimStyle, Seed, Sizes,
    };

    fn spec() -> ThemeSpec {
        ThemeSpec::oled()
    }

    fn oled_file_spec() -> ThemeSpec {
        // The literal of F-P2's documented default, built here, never through oled-config's Default.
        ThemeSpec {
            look: Look {
                preset: Preset::Oled,
                seed: Seed {
                    bg: Rgb(0, 0, 0),
                    fg: Rgb(255, 255, 255),
                    accent: None,
                    attention: Rgb(0xFF, 0x8C, 0x00),
                    danger: Rgb(0xFF, 0x45, 0x3A),
                    white_level: 100,
                },
                knobs: Knobs {
                    bar_opacity: 95,
                    surface_opacity: 95,
                    corners: Corners::Normal,
                    density_pct: 100,
                    rim: RimStyle::Hairline,
                    plate: PlateStyle::Fill,
                    indicator: IndicatorStyle::Line,
                    font: FontSpec { family: String::new(), weight_delta: 0 },
                    sizes: Sizes { icon: 16, text: 13, tray: 16, module_pct: 100 },
                    motion: MotionSpec { mode: Motion::System, speed_pct: 100 },
                },
            },
            overrides: Default::default(),
            a11y: A11y { reduced_motion: None, text_scale_pct: None },
        }
    }

    fn oled_file() -> oled_config::theme::ThemeFile {
        let s = oled_file_spec();
        oled_config::theme::ThemeFile {
            schema: 1,
            generation: 0,
            writer: String::new(),
            language: oled_config::theme::Lang::Auto,
            look: s.look,
            overrides: Default::default(),
            a11y: s.a11y,
        }
    }

    fn hc() -> HcColors {
        HcColors {
            window: (0, 0, 0),
            window_text: (255, 255, 255),
            highlight: (26, 235, 255),
            highlight_text: (0, 0, 0),
            gray_text: (63, 242, 63),
        }
    }

    #[test]
    fn oled_tokens_match_the_tables() {
        let t = Tokens::oled();
        let c = &t.color;
        assert_eq!(c.bg, Ink::solid(0, 0, 0));
        assert_eq!(c.fg_primary.a, t::FG_PRIMARY);
        assert_eq!(
            (c.fg_primary.a, c.fg_hot.a, c.fg_secondary.a, c.fg_secondary_hot.a, c.fg_tertiary.a, c.fg_disabled.a),
            (230, 255, 170, 215, 128, 90)
        );
        assert_eq!(c.fg_on_selected, Ink::solid(0, 0, 0));
        assert_eq!(
            (c.fill_hover.a, c.fill_press.a, c.fill_selected.a, c.fill_selected_hot.a, c.fill_selected_press.a),
            (20, 36, 235, 255, 200)
        );
        assert_eq!((c.fill_control.a, c.fill_control_hot.a, c.fill_control_off.a), (16, 28, 10));
        assert_eq!(
            (c.fill_row_selected.a, c.fill_row_selected_hot.a, c.fill_row_selected_inactive.a, c.fill_text_selection.a),
            (48, 56, 28, 70)
        );
        assert_eq!(
            (c.rim_surface.a, c.rim_control.a, c.rim_hover.a, c.rim_focus.a, c.separator.a),
            (22, 34, 18, 160, 30)
        );
        assert_eq!(c.scrim, Ink { rgb: (0, 0, 0), a: 153 });
        assert_eq!((c.scrollbar.a, c.scrollbar_hot.a), (50, 110));
        assert_eq!([c.heat[0].a, c.heat[1].a, c.heat[2].a, c.heat[3].a], [12, 20, 30, 44]);
        assert_eq!((c.chart_fill.a, c.chart_grid.a), (24, 14));
        assert_eq!(c.accent_attention, Ink::solid(0xFF, 0x8C, 0x00));
        assert_eq!(c.accent_danger, Ink::solid(0xFF, 0x45, 0x3A));
        assert_eq!(c.accent, None);
        assert_eq!((c.toggle_off_track.a, c.toggle_off_rim.a, c.toggle_off_knob.a, c.slider_rail.a), (40, 70, 160, 60));
        assert_eq!((c.seg_track.a, c.seg_rim.a, c.seg_hot.a, c.nav_bar.a), (14, 40, 26, 255));
        assert_eq!(c.disabled_veil, Ink { rgb: (0, 0, 0), a: 150 });
        let s = &t.size;
        assert_eq!(
            (s.control_h, s.row_compact, s.row_detail, s.row_comfort, s.row_dense, s.row_table),
            (28, 34, 28, 44, 24, 28)
        );
        assert_eq!((s.header_h, s.toolbar_h, s.status_h, s.titlebar_h, s.caption_btn_w), (28, 44, 28, 40, 46));
        assert_eq!((s.tab_h, s.tab_w_min, s.tab_w_max, s.tab_row_h, s.nav_w), (32, 96, 220, 36, 200));
        assert_eq!((s.col_min_w, s.col_resize_hit, s.splitter_hit), (48, 8, 6));
        assert_eq!((s.scrollbar_w, s.scrollbar_hot_w, s.scrollbar_hit, s.scroll_thumb_min), (3, 6, 12, 24));
        assert_eq!((s.menu_row, s.menu_text_col, s.menu_w_min, s.menu_w_max), (28, 36, 200, 360));
        assert_eq!(
            (s.chip_h, s.toast_h, s.toast_w_max, s.dialog_w, s.dialog_w_wide, s.progress_h),
            (24, 40, 480, 400, 480, 2)
        );
        assert_eq!(
            (s.icon_s, s.icon_m, s.icon_l, s.text_col, s.text_col_icon, s.icon_text_gap),
            (16, 24, 32, 20, 48, 8)
        );
        assert_eq!((s.focus_gap, s.drag_threshold, s.dot_attention), (2, 6, 6));
        assert_eq!((s.toggle_w, s.toggle_h, s.toggle_knob, s.toggle_knob_pressed), (32, 18, 12, 14));
        assert_eq!((s.slider_track, s.slider_knob_r, s.slider_knob_r_hot), (4, 7, 8));
        let sp = &t.space;
        assert_eq!([sp.s1, sp.s2, sp.s3, sp.s4, sp.s5, sp.s6, sp.s7, sp.s8], [2, 4, 8, 12, 16, 20, 24, 32]);
        assert_eq!(t.radius, RadiusTokens { xs: 4, s: 7, m: 10, l: 14, bar: 11 });
        let m = &t.motion;
        assert_eq!((m.dur_fast, m.dur_normal, m.dur_slow, m.dur_exit, m.dur_progress), (90, 140, 160, 83, 250));
        assert_eq!(
            (m.hover_delay, m.tooltip_delay, m.tooltip_warm, m.submenu_delay, m.busy_delay),
            (400, 500, 1000, 250, 300)
        );
        assert_eq!(
            (m.sweep_period, m.toast_hold, m.typeahead_reset, m.caret_blink, m.caret_blink_for, m.spring_delay),
            (1200, 4000, 1000, 500, 5000, 600)
        );
        assert_eq!(m.slide_px, 4);
        assert_eq!(t.text_scale, 1.0);
        assert!(!t.high_contrast);
    }

    #[test]
    fn resolve_from_theme_file_default_equals_tokens_oled() {
        let r = Tokens::resolve(&ThemeSpec::from(&oled_file()), &SysOverlay::none());
        assert_eq!(r, Tokens::oled());
        assert_eq!(Tokens::resolve(&oled_file_spec(), &SysOverlay::none()), r);
        assert_eq!(Tokens::resolve(&spec(), &SysOverlay::none()), r);
    }

    #[test]
    fn white_level_scales_text_and_selected() {
        let mut s = spec();
        s.look.seed.white_level = 75;
        let c = Tokens::resolve(&s, &SysOverlay::none()).color;
        assert_eq!(c.fg_primary.a, 172);
        assert_eq!(c.fg_secondary.a, 128);
        assert_eq!(c.fill_selected.a, 176);
        assert_eq!(c.fg_tertiary.a, 128);
        assert_eq!(c.fg_disabled.a, 90);
    }

    #[test]
    fn floors_hold_at_70() {
        let mut s = spec();
        s.look.seed.white_level = 70;
        let c = Tokens::resolve(&s, &SysOverlay::none()).color;
        for ink in [c.fg_primary, c.fg_hot, c.fg_secondary, c.fg_secondary_hot, c.fg_tertiary] {
            assert!(ink.a >= 128, "{ink:?}");
        }
        assert_eq!(c.fg_disabled.a, 90);
    }

    #[test]
    fn fg_seed_tints_white_tokens() {
        let mut s = spec();
        s.look.seed.fg = Rgb(255, 240, 220);
        let c = Tokens::resolve(&s, &SysOverlay::none()).color;
        let o = Tokens::oled().color;
        assert_eq!(c.fg_primary, Ink { rgb: (255, 240, 220), a: o.fg_primary.a });
        assert_eq!(c.fill_hover, Ink { rgb: (255, 240, 220), a: o.fill_hover.a });
        assert_eq!(c.rim_focus, Ink { rgb: (255, 240, 220), a: o.rim_focus.a });
        assert_eq!(c.heat[3], Ink { rgb: (255, 240, 220), a: o.heat[3].a });
        assert_eq!(c.accent_attention, o.accent_attention);
        assert_eq!(c.accent_danger, o.accent_danger);
        assert_eq!(c.accent, None);
    }

    #[test]
    fn density_picks_row_table() {
        let rows = |pct: u8| {
            let mut s = spec();
            s.look.knobs.density_pct = pct;
            Tokens::resolve(&s, &SysOverlay::none()).size.row_table
        };
        assert_eq!((rows(85), rows(100), rows(115)), (24, 28, 34));
        assert_eq!((rows(94), rows(95), rows(105), rows(106)), (24, 28, 28, 34));
    }

    #[test]
    fn corners_scale_radii() {
        let radii = |c: Corners| {
            let mut s = spec();
            s.look.knobs.corners = c;
            Tokens::resolve(&s, &SysOverlay::none()).radius
        };
        let r = radii(Corners::Round);
        assert_eq!((r.s, r.m, r.l), (9, 13, 18));
        let h = radii(Corners::Half);
        assert_eq!((h.s, h.m, h.l), (4, 5, 7));
        let q = radii(Corners::Square);
        assert_eq!((q.xs, q.s, q.m, q.l, q.bar), (0, 0, 0, 0, 0));
    }

    fn assert_still(m: &MotionTokens) {
        assert_eq!(
            (m.dur_fast, m.dur_normal, m.dur_slow, m.dur_exit, m.dur_progress, m.caret_blink),
            (0, 0, 0, 0, 0, 0)
        );
        assert!(m.reduced);
    }

    #[test]
    fn reduced_motion_zeroes_durations() {
        for mode in [Motion::Off, Motion::Reduced] {
            let mut s = spec();
            s.look.knobs.motion.mode = mode;
            assert_still(&Tokens::resolve(&s, &SysOverlay::none()).motion);
        }
        let sys = SysOverlay { reduced_motion: true, ..SysOverlay::none() };
        assert_still(&Tokens::resolve(&spec(), &sys).motion);
        let mut s = spec();
        s.a11y = A11y { reduced_motion: Some(true), text_scale_pct: None };
        assert_still(&Tokens::resolve(&s, &SysOverlay::none()).motion);
        let mut full = spec();
        full.look.knobs.motion.mode = Motion::Full;
        assert_eq!(Tokens::resolve(&full, &sys).motion.dur_fast, 90);
    }

    #[test]
    fn theme_a11y_overrides_system() {
        let sys = SysOverlay { reduced_motion: true, text_scale: 1.25, high_contrast: None };
        let mut s = spec();
        s.a11y = A11y { reduced_motion: Some(false), text_scale_pct: Some(150) };
        let t = Tokens::resolve(&s, &sys);
        assert_eq!(t.motion.dur_fast, 90);
        assert!(!t.motion.reduced);
        assert_eq!(t.text_scale, 1.5);
    }

    #[test]
    fn motion_speed_scales() {
        let mut s = spec();
        s.look.knobs.motion.speed_pct = 125;
        let m = Tokens::resolve(&s, &SysOverlay::none()).motion;
        assert_eq!(m.dur_fast, 113);
        assert_eq!(m.dur_normal, 175);
    }

    #[test]
    fn accent_below_4_5_to_1_is_dropped() {
        let mut s = spec();
        s.look.seed.accent = Some(Rgb(40, 40, 200));
        assert_eq!(Tokens::resolve(&s, &SysOverlay::none()).color.accent, None);
        s.look.seed.accent = Some(Rgb(255, 214, 10));
        assert_eq!(Tokens::resolve(&s, &SysOverlay::none()).color.accent, Some(Ink::solid(255, 214, 10)));
    }

    #[test]
    fn overrides_by_name() {
        let mut s = spec();
        s.overrides.insert("R_L".into(), 12);
        s.overrides.insert("NOPE".into(), 1);
        assert_eq!(Tokens::resolve(&s, &SysOverlay::none()).radius.l, 12);
        assert_eq!(Tokens::resolve_report(&s).len(), 1);
        // Out of range: clamped to the token's range (errata), and listed.
        s.overrides.insert("R_L".into(), 99);
        s.overrides.insert("DUR_FAST".into(), -5);
        let t = Tokens::resolve(&s, &SysOverlay::none());
        assert_eq!(t.radius.l, 24);
        assert_eq!(t.motion.dur_fast, 0);
        assert_eq!(Tokens::resolve_report(&s).len(), 3);
    }

    #[test]
    fn every_override_name_is_unique() {
        let mut names: Vec<_> = OVERRIDES.iter().map(|o| o.name).collect();
        let n = names.len();
        names.sort_unstable();
        names.dedup();
        assert_eq!(names.len(), n);
        assert!(n > 100);
    }

    #[test]
    fn every_override_row_changes_the_tokens() {
        let base = Tokens::resolve(&spec(), &SysOverlay::none());
        for row in OVERRIDES.iter() {
            // A value inside the range that differs from the resolved one.
            for v in [row.max, row.min, row.min + 1] {
                let mut s = spec();
                s.overrides.insert(row.name.to_string(), v);
                let r = Tokens::resolve(&s, &SysOverlay::none());
                if r != base {
                    break;
                }
                assert_ne!(v, row.min + 1, "{} changes nothing", row.name);
            }
        }
        // The two rows that were missing: a 200 on NAV_BAR lands.
        let mut s = spec();
        s.overrides.insert("NAV_BAR".into(), 200);
        s.overrides.insert("DISABLED_VEIL".into(), 77);
        let c = Tokens::resolve(&s, &SysOverlay::none()).color;
        assert_eq!((c.nav_bar.a, c.disabled_veil.a), (200, 77));
        // Every numeric token of table 2.7 has a row.
        assert_eq!(OVERRIDES.iter().count(), 120);
    }

    #[test]
    fn high_contrast_maps_every_role() {
        let sys = SysOverlay { high_contrast: Some(hc()), reduced_motion: false, text_scale: 1.0 };
        let t = Tokens::resolve(&spec(), &sys);
        let c = &t.color;
        let h = hc();
        assert!(t.high_contrast);
        assert_eq!(c.bg, Ink::solid(0, 0, 0));
        for ink in [c.fg_primary, c.fg_secondary, c.fg_secondary_hot, c.fg_tertiary] {
            assert_eq!(ink, Ink { rgb: h.window_text, a: 255 });
        }
        assert_eq!(c.fg_disabled, Ink { rgb: h.gray_text, a: 255 });
        assert_eq!(c.fg_hot, Ink { rgb: h.highlight_text, a: 255 });
        assert_eq!(c.fg_on_selected, Ink { rgb: h.highlight_text, a: 255 });
        for ink in [c.fill_hover, c.fill_press, c.fill_selected, c.fill_row_selected] {
            assert_eq!(ink, Ink { rgb: h.highlight, a: 255 });
        }
        assert_eq!(c.rim_focus, Ink { rgb: h.highlight, a: 255 });
        for ink in c.heat {
            assert_eq!(ink, Ink { rgb: h.window, a: 255 });
        }
    }

    #[test]
    fn generation_is_stable() {
        let a = Tokens::resolve(&spec(), &SysOverlay::none());
        assert_eq!(a.generation, Tokens::resolve(&spec(), &SysOverlay::none()).generation);
        let mut s = spec();
        s.look.seed.white_level = 90;
        assert_ne!(a.generation, Tokens::resolve(&s, &SysOverlay::none()).generation);
        assert_ne!(
            a.generation,
            Tokens::resolve(&spec(), &SysOverlay { text_scale: 1.5, ..SysOverlay::none() }).generation
        );
    }
}
