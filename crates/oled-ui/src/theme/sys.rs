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
        use windows::core::w;
        use windows::Win32::Graphics::Gdi::{
            GetSysColor, COLOR_GRAYTEXT, COLOR_HIGHLIGHT, COLOR_HIGHLIGHTTEXT, COLOR_WINDOW, COLOR_WINDOWTEXT,
            SYS_COLOR_INDEX,
        };
        use windows::Win32::System::Registry::{RegGetValueW, HKEY_CURRENT_USER, RRF_RT_REG_DWORD};
        use windows::Win32::UI::Accessibility::{HCF_HIGHCONTRASTON, HIGHCONTRASTW};
        use windows::Win32::UI::WindowsAndMessaging::{
            SystemParametersInfoW, SPI_GETCLIENTAREAANIMATION, SPI_GETHIGHCONTRAST, SYSTEM_PARAMETERS_INFO_UPDATE_FLAGS,
        };

        let rgb = |i: SYS_COLOR_INDEX| {
            // SAFETY: GetSysColor takes an index and returns a 0x00BBGGRR value; it has no other effect.
            let c = unsafe { GetSysColor(i) };
            (c & 0xFF, (c >> 8) & 0xFF, (c >> 16) & 0xFF)
        };
        let mut hc = HIGHCONTRASTW { cbSize: std::mem::size_of::<HIGHCONTRASTW>() as u32, ..Default::default() };
        // SAFETY: `hc` is a live HIGHCONTRASTW with cbSize set, the size SPI_GETHIGHCONTRAST expects; the call only fills it.
        let ok = unsafe {
            SystemParametersInfoW(
                SPI_GETHIGHCONTRAST,
                hc.cbSize,
                Some(&mut hc as *mut HIGHCONTRASTW as *mut std::ffi::c_void),
                SYSTEM_PARAMETERS_INFO_UPDATE_FLAGS(0),
            )
        };
        let high_contrast = (ok.is_ok() && (hc.dwFlags.0 & HCF_HIGHCONTRASTON.0) != 0).then(|| HcColors {
            window: rgb(COLOR_WINDOW),
            window_text: rgb(COLOR_WINDOWTEXT),
            highlight: rgb(COLOR_HIGHLIGHT),
            highlight_text: rgb(COLOR_HIGHLIGHTTEXT),
            gray_text: rgb(COLOR_GRAYTEXT),
        });

        let mut anim = windows::core::BOOL(1);
        // SAFETY: `anim` is a live BOOL, the out parameter SPI_GETCLIENTAREAANIMATION fills.
        let got = unsafe {
            SystemParametersInfoW(
                SPI_GETCLIENTAREAANIMATION,
                0,
                Some(&mut anim as *mut windows::core::BOOL as *mut std::ffi::c_void),
                SYSTEM_PARAMETERS_INFO_UPDATE_FLAGS(0),
            )
        };
        let reduced_motion = got.is_ok() && !anim.as_bool();

        // TextScaleFactor (a percentage, 100..=225), read-only; 100 when absent or unreadable.
        let mut pct: u32 = 100;
        let mut size = std::mem::size_of::<u32>() as u32;
        // SAFETY: the data pointer and size describe `pct`, a live u32; the value is only read.
        let status = unsafe {
            RegGetValueW(
                HKEY_CURRENT_USER,
                w!("Software\\Microsoft\\Accessibility"),
                w!("TextScaleFactor"),
                RRF_RT_REG_DWORD,
                None,
                Some(&mut pct as *mut u32 as *mut std::ffi::c_void),
                Some(&mut size),
            )
        };
        if status.0 != 0 {
            pct = 100;
        }
        SysOverlay { high_contrast, reduced_motion, text_scale: (pct as f32 / 100.0).clamp(1.0, 2.25) }
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
