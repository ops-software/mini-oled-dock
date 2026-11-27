//! The dock's glyphs: the shapes live in oled-ui; the audio device kinds map onto its DeviceGlyph.
pub use oled_ui::gfx::glyphs::*;

use crate::audio::DeviceKind;
use oled_ui::gfx::glyphs::DeviceGlyph;

fn glyph_of(k: DeviceKind) -> DeviceGlyph {
    match k {
        DeviceKind::Speaker => DeviceGlyph::Speaker,
        DeviceKind::Headphones => DeviceGlyph::Headphones,
        DeviceKind::Headset => DeviceGlyph::Headset,
        DeviceKind::LineOut => DeviceGlyph::LineOut,
        DeviceKind::Display => DeviceGlyph::Display,
        DeviceKind::Digital => DeviceGlyph::Digital,
    }
}

/// Shadows the glob: the dock's callers keep passing a DeviceKind.
pub fn device_shape(u: f32, v: f32, kind: DeviceKind) -> bool {
    oled_ui::gfx::glyphs::device_shape(u, v, glyph_of(kind))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn glyph_of_maps_every_device_kind() {
        let pairs = [
            (DeviceKind::Speaker, DeviceGlyph::Speaker),
            (DeviceKind::Headphones, DeviceGlyph::Headphones),
            (DeviceKind::Headset, DeviceGlyph::Headset),
            (DeviceKind::LineOut, DeviceGlyph::LineOut),
            (DeviceKind::Display, DeviceGlyph::Display),
            (DeviceKind::Digital, DeviceGlyph::Digital),
        ];
        for (k, glyph) in pairs {
            assert_eq!(glyph_of(k), glyph);
            assert_eq!(device_shape(8.0, 8.0, k), oled_ui::gfx::glyphs::device_shape(8.0, 8.0, glyph));
        }
    }
}
