//! The system's accessibility state over any theme (SPEC-platform 5.3.3): high contrast, reduced motion, text scale.

use super::A11y;

/// What Windows asks of every window, applied after the theme.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct SysOverlay {
    pub high_contrast: Option<HcColors>,
    pub reduced_motion: bool,
    pub text_scale: f32,
}

/// The high-contrast system colours, as (r, g, b).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct HcColors {
    pub window: (u32, u32, u32),
    pub window_text: (u32, u32, u32),
    pub highlight: (u32, u32, u32),
    pub highlight_text: (u32, u32, u32),
    pub gray_text: (u32, u32, u32),
}

impl SysOverlay {
    /// No high contrast, motion on, text scale 1.0.
    pub fn none() -> SysOverlay {
        SysOverlay { high_contrast: None, reduced_motion: false, text_scale: 1.0 }
    }

    /// The system's state: SPI_GETHIGHCONTRAST, SPI_GETCLIENTAREAANIMATION and TextScaleFactor. Filled by K1a.1; until
    /// then it returns none().
    pub fn read() -> SysOverlay {
        SysOverlay::none()
    }

    /// theme.json's accessibility overrides: reduced_motion Some(v) replaces the system value; text_scale_pct Some(p)
    /// replaces the text scale with p / 100, clamped to 1.0..=2.25.
    pub fn with_theme(self, a: &A11y) -> SysOverlay {
        let mut o = self;
        if let Some(v) = a.reduced_motion {
            o.reduced_motion = v;
        }
        if let Some(p) = a.text_scale_pct {
            o.text_scale = (f32::from(p) / 100.0).clamp(1.0, 2.25);
        }
        o
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn theme_overrides_replace_the_system_values() {
        let sys = SysOverlay { high_contrast: None, reduced_motion: true, text_scale: 1.25 };
        assert_eq!(sys.with_theme(&A11y { reduced_motion: None, text_scale_pct: None }), sys);
        let o = sys.with_theme(&A11y { reduced_motion: Some(false), text_scale_pct: Some(150) });
        assert!(!o.reduced_motion);
        assert_eq!(o.text_scale, 1.5);
        assert_eq!(sys.with_theme(&A11y { reduced_motion: None, text_scale_pct: Some(400) }).text_scale, 2.25);
        assert_eq!(sys.with_theme(&A11y { reduced_motion: None, text_scale_pct: Some(50) }).text_scale, 1.0);
        assert_eq!(SysOverlay::none(), SysOverlay { high_contrast: None, reduced_motion: false, text_scale: 1.0 });
    }
}
