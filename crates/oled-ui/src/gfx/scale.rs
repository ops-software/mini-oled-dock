//! DPI scale: logical to physical pixels (moved from render.rs by W3).

/// DPI scale: converts logical measures to physical pixels.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Scale {
    /// Effective DPI of the monitor (96 = 100%).
    pub dpi: u32,
}

impl Scale {
    /// Scale factor (dpi / 96).
    pub fn scale(&self) -> f32 {
        self.dpi as f32 / 96.0
    }

    /// Logical measure -> physical pixels (rounded).
    pub fn px(&self, logical: i32) -> i32 {
        ((logical as f32) * self.scale()).round() as i32
    }
}

impl Default for Scale {
    fn default() -> Self {
        Scale { dpi: 96 }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn px_rounds_each_value_on_its_own() {
        assert_eq!(Scale { dpi: 96 }.px(7), 7);
        assert_eq!(Scale { dpi: 120 }.px(7), 9);
        assert_eq!(Scale { dpi: 120 }.px(11), 14);
        assert_eq!(Scale { dpi: 144 }.px(7), 11);
        assert_eq!(Scale { dpi: 192 }.px(7), 14);
        assert_eq!(Scale { dpi: 120 }.scale(), 1.25);
    }

    #[test]
    fn default_is_96_dpi() {
        assert_eq!(Scale::default(), Scale { dpi: 96 });
    }
}
