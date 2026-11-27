//! Tokens and the theme (F-K1, P2-kit 2.2, 2.7): colour as Ink, type roles, the token tables an app paints with, and
//! the one ThemeSpec of the suite (P2-plumbing's Look plus overrides and the accessibility overrides).

mod resolve;
mod sys;

pub use oled_config::theme::{
    A11y, Corners, FontSpec, IndicatorStyle, Knobs, Look, Motion, MotionSpec, PlateStyle, Preset, Rgb, RimStyle, Seed,
    Sizes, ThemeFile,
};
pub use sys::{HcColors, SysOverlay};

/// A colour: straight rgb plus alpha 0..=255, the form W3's Canvas takes, so a kit pixel equals a dock pixel by
/// construction.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Ink {
    pub rgb: (u32, u32, u32),
    pub a: u32,
}

impl Ink {
    /// White at alpha `a`.
    pub const fn white(a: u32) -> Ink {
        Ink { rgb: (255, 255, 255), a }
    }

    /// An opaque colour.
    pub const fn solid(r: u32, g: u32, b: u32) -> Ink {
        Ink { rgb: (r, g, b), a: 255 }
    }

    /// The same colour at alpha `a`.
    pub const fn with_a(self, a: u32) -> Ink {
        Ink { rgb: self.rgb, a }
    }

    /// The alpha times `t`, rounded (and kept in 0..=255).
    pub fn faded(self, t: f32) -> Ink {
        Ink { rgb: self.rgb, a: ((self.a as f32) * t).round().clamp(0.0, 255.0) as u32 }
    }
}

/// A type role (SPEC-platform 5.3.3).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Default)]
pub enum TypeRole {
    Small,
    Caption,
    #[default]
    Body,
    BodyStrong,
    Section,
    Title,
    Display,
}

/// A role's size and line height (logical px), weight and face.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TypeSpec {
    pub size: i32,
    pub line: i32,
    pub weight: u16,
    pub display_face: bool,
}

impl TypeRole {
    /// Small 11/14/400, Caption 12/16/400, Body 13/18/400, BodyStrong 13/18/600, Section 12/16/600, Title 20/28/600 and
    /// Display 32/40/300 on the display face.
    pub const fn spec(self) -> TypeSpec {
        let (size, line, weight, display_face) = match self {
            TypeRole::Small => (11, 14, 400, false),
            TypeRole::Caption => (12, 16, 400, false),
            TypeRole::Body => (13, 18, 400, false),
            TypeRole::BodyStrong => (13, 18, 600, false),
            TypeRole::Section => (12, 16, 600, false),
            TypeRole::Title => (20, 28, 600, true),
            TypeRole::Display => (32, 40, 300, true),
        };
        TypeSpec { size, line, weight, display_face }
    }
}

/// The colour tokens (table 2.7).
#[derive(Clone, Debug, PartialEq)]
pub struct ColorTokens {
    pub bg: Ink,
    pub fg_primary: Ink,
    pub fg_hot: Ink,
    pub fg_secondary: Ink,
    pub fg_secondary_hot: Ink,
    pub fg_tertiary: Ink,
    pub fg_disabled: Ink,
    pub fg_on_selected: Ink,
    pub fill_hover: Ink,
    pub fill_press: Ink,
    pub fill_selected: Ink,
    pub fill_selected_hot: Ink,
    pub fill_selected_press: Ink,
    pub fill_control: Ink,
    pub fill_control_hot: Ink,
    pub fill_control_off: Ink,
    pub fill_row_selected: Ink,
    pub fill_row_selected_hot: Ink,
    pub fill_row_selected_inactive: Ink,
    pub fill_text_selection: Ink,
    pub rim_surface: Ink,
    pub rim_control: Ink,
    pub rim_hover: Ink,
    pub rim_focus: Ink,
    pub separator: Ink,
    pub scrim: Ink,
    pub scrollbar: Ink,
    pub scrollbar_hot: Ink,
    pub heat: [Ink; 4],
    pub chart_fill: Ink,
    pub chart_grid: Ink,
    pub accent_attention: Ink,
    pub accent_danger: Ink,
    pub accent: Option<Ink>,
    // Kit tokens that name pixels the dock ships today (R1), table 2.7.
    pub toggle_off_track: Ink,
    pub toggle_off_rim: Ink,
    pub toggle_off_knob: Ink,
    pub slider_rail: Ink,
    pub seg_track: Ink,
    pub seg_rim: Ink,
    pub seg_hot: Ink,
    pub disabled_veil: Ink,
    pub nav_bar: Ink,
}

/// The size tokens, logical px (table 2.7).
#[derive(Clone, Debug, PartialEq)]
pub struct SizeTokens {
    pub control_h: i32,
    pub row_compact: i32,
    pub row_detail: i32,
    pub row_comfort: i32,
    pub row_dense: i32,
    pub row_table: i32,
    pub header_h: i32,
    pub toolbar_h: i32,
    pub status_h: i32,
    pub titlebar_h: i32,
    pub caption_btn_w: i32,
    pub tab_h: i32,
    pub tab_w_min: i32,
    pub tab_w_max: i32,
    pub tab_row_h: i32,
    pub nav_w: i32,
    pub col_min_w: i32,
    pub col_resize_hit: i32,
    pub splitter_hit: i32,
    pub scrollbar_w: i32,
    pub scrollbar_hot_w: i32,
    pub scrollbar_hit: i32,
    pub scroll_thumb_min: i32,
    pub menu_row: i32,
    pub menu_text_col: i32,
    pub menu_w_min: i32,
    pub menu_w_max: i32,
    pub chip_h: i32,
    pub toast_h: i32,
    pub toast_w_max: i32,
    pub dialog_w: i32,
    pub dialog_w_wide: i32,
    pub progress_h: i32,
    pub icon_s: i32,
    pub icon_m: i32,
    pub icon_l: i32,
    pub text_col: i32,
    pub text_col_icon: i32,
    pub icon_text_gap: i32,
    pub focus_gap: i32,
    pub drag_threshold: i32,
    pub dot_attention: i32,
    pub toggle_w: i32,
    pub toggle_h: i32,
    pub toggle_knob: i32,
    pub toggle_knob_pressed: i32,
    pub slider_track: i32,
    pub slider_knob_r: i32,
    pub slider_knob_r_hot: i32,
}

/// The spacing steps S_1..S_8, logical px.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SpaceTokens {
    pub s1: i32,
    pub s2: i32,
    pub s3: i32,
    pub s4: i32,
    pub s5: i32,
    pub s6: i32,
    pub s7: i32,
    pub s8: i32,
}

/// The corner radii, logical px (a pill is h / 2 at the call site).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RadiusTokens {
    pub xs: i32,
    pub s: i32,
    pub m: i32,
    pub l: i32,
    pub bar: i32,
}

/// Durations in ms (all 0 under reduced motion) and the slide distance in logical px.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct MotionTokens {
    pub dur_fast: u32,
    pub dur_normal: u32,
    pub dur_slow: u32,
    pub dur_exit: u32,
    pub dur_progress: u32,
    pub hover_delay: u32,
    pub tooltip_delay: u32,
    pub tooltip_warm: u32,
    pub submenu_delay: u32,
    pub busy_delay: u32,
    pub sweep_period: u32,
    pub toast_hold: u32,
    pub typeahead_reset: u32,
    pub caret_blink: u32,
    pub caret_blink_for: u32,
    pub spring_delay: u32,
    pub slide_px: i32,
    pub reduced: bool,
}

/// Every token a kit window paints with, resolved from a ThemeSpec and the system's accessibility state.
#[derive(Clone, Debug, PartialEq)]
pub struct Tokens {
    pub color: ColorTokens,
    pub size: SizeTokens,
    pub space: SpaceTokens,
    pub radius: RadiusTokens,
    pub motion: MotionTokens,
    pub text_scale: f32,
    pub generation: u64,
    pub high_contrast: bool,
}

/// The suite's one theme: P2-plumbing's Look, the token overrides by name and the accessibility overrides (ROADMAP X7).
#[derive(Clone, Debug, PartialEq)]
pub struct ThemeSpec {
    pub look: Look,
    pub overrides: std::collections::BTreeMap<String, i64>,
    pub a11y: A11y,
}

impl From<&ThemeFile> for ThemeSpec {
    /// The look, overrides and a11y cloned; schema, generation, writer and language are not the kit's.
    fn from(f: &ThemeFile) -> ThemeSpec {
        ThemeSpec { look: f.look.clone(), overrides: f.overrides.clone(), a11y: f.a11y }
    }
}

impl ThemeSpec {
    /// F-P2's documented default Look written out field by field (it never calls Look::default or any oled-config
    /// function), no overrides, no accessibility override.
    pub fn oled() -> ThemeSpec {
        ThemeSpec {
            look: Look {
                preset: Preset::Oled,
                seed: Seed {
                    bg: Rgb(0x00, 0x00, 0x00),
                    fg: Rgb(0xFF, 0xFF, 0xFF),
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
            overrides: std::collections::BTreeMap::new(),
            a11y: A11y { reduced_motion: None, text_scale_pct: None },
        }
    }
}

/// The theme file's language as the i18n one (the two enums are foreign to each other): Auto, En and It one to one.
pub fn theme_lang(l: oled_config::theme::Lang) -> oled_sys::i18n::Lang {
    match l {
        oled_config::theme::Lang::Auto => oled_sys::i18n::Lang::Auto,
        oled_config::theme::Lang::En => oled_sys::i18n::Lang::En,
        oled_config::theme::Lang::It => oled_sys::i18n::Lang::It,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ink_helpers() {
        assert_eq!(Ink::white(20), Ink { rgb: (255, 255, 255), a: 20 });
        assert_eq!(Ink::solid(1, 2, 3).with_a(9), Ink { rgb: (1, 2, 3), a: 9 });
        assert_eq!(Ink::white(230).faded(0.5), Ink::white(115));
        assert_eq!(Ink::white(200).faded(2.0), Ink::white(255));
    }

    #[test]
    fn type_roles_match_the_table() {
        assert_eq!(TypeRole::Small.spec(), TypeSpec { size: 11, line: 14, weight: 400, display_face: false });
        assert_eq!(TypeRole::BodyStrong.spec(), TypeSpec { size: 13, line: 18, weight: 600, display_face: false });
        assert_eq!(TypeRole::Display.spec(), TypeSpec { size: 32, line: 40, weight: 300, display_face: true });
        assert_eq!(TypeRole::default(), TypeRole::Body);
    }

    #[test]
    fn languages_map_one_to_one() {
        assert_eq!(theme_lang(oled_config::theme::Lang::Auto), oled_sys::i18n::Lang::Auto);
        assert_eq!(theme_lang(oled_config::theme::Lang::En), oled_sys::i18n::Lang::En);
        assert_eq!(theme_lang(oled_config::theme::Lang::It), oled_sys::i18n::Lang::It);
    }
}
