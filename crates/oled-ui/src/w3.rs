//! The kit's one import point for the moved renderer (P1-workspace-v2: FZ-GFX, FZ-TOKENS, FZ-I18N) and for P2-plumbing's
//! frozen theme and descriptor types: one line per item, the single place to edit if a path ever moves (P2-kit 2.1).
//! Scale and Canvas are `pub use` because the crate root and geom re-export them; every other line is crate-only.
//! Not here: oled_i18n_build::generate (a build-dependency, called by build.rs) and scroll.rs's BAR_INSET (private to
//! scroll.rs; ScrollModel::thumb applies it).
#![allow(unused_imports)]

pub use crate::gfx::canvas::Canvas;
pub use crate::gfx::scale::Scale;

pub(crate) use crate::gfx::canvas::glyph_hit_counts;
pub(crate) use crate::gfx::glyphs;
pub(crate) use crate::gfx::glyphs::device_shape;
pub(crate) use crate::gfx::glyphs::set_scale;
pub(crate) use crate::gfx::glyphs::DeviceGlyph;
pub(crate) use crate::gfx::image::opaque_bbox;
pub(crate) use crate::gfx::image::premultiply_px;
pub(crate) use crate::gfx::image::resize_area;
pub(crate) use crate::gfx::image::resize_is_identity;
pub(crate) use crate::gfx::image::unpremultiply_px;
pub(crate) use crate::gfx::image::IconBitmap;
pub(crate) use crate::gfx::panel::panel_pixels;
pub(crate) use crate::gfx::pixel::area_sample;
pub(crate) use crate::gfx::pixel::argb;
pub(crate) use crate::gfx::pixel::div255;
pub(crate) use crate::gfx::pixel::in_rect;
pub(crate) use crate::gfx::pixel::over;
pub(crate) use crate::gfx::pixel::rect_wh;
pub(crate) use crate::gfx::pixel::rr_cov;
pub(crate) use crate::gfx::pixel::white;
pub(crate) use crate::gfx::wic::decode_image_bytes;
pub(crate) use crate::gfx::wic::encode_png;
pub(crate) use crate::gfx::wic::is_png;
pub(crate) use crate::scroll::ScrollModel;
pub(crate) use crate::scroll::SCROLLBAR_HIDE_MS;
pub(crate) use crate::scroll::SCROLL_STEP;
pub(crate) use crate::text::gdi::create_font;
pub(crate) use crate::text::gdi::delete_font;
pub(crate) use crate::text::gdi::line_height;
pub(crate) use crate::text::gdi::present_layered;
pub(crate) use crate::text::gdi::Gdi;
pub(crate) use crate::text::gdi::NO_TEXT;
pub(crate) use crate::tokens;
pub(crate) use oled_config::desc::Descriptor;
pub(crate) use oled_config::theme::A11y;
pub(crate) use oled_config::theme::Corners;
pub(crate) use oled_config::theme::FontSpec;
pub(crate) use oled_config::theme::IndicatorStyle;
pub(crate) use oled_config::theme::Knobs;
pub(crate) use oled_config::theme::Lang as ThemeLang;
pub(crate) use oled_config::theme::Look;
pub(crate) use oled_config::theme::Motion;
pub(crate) use oled_config::theme::MotionSpec;
pub(crate) use oled_config::theme::PlateStyle;
pub(crate) use oled_config::theme::Preset;
pub(crate) use oled_config::theme::Rgb;
pub(crate) use oled_config::theme::RimStyle;
pub(crate) use oled_config::theme::Seed;
pub(crate) use oled_config::theme::Sizes;
pub(crate) use oled_config::theme::ThemeFile;
pub(crate) use oled_config::theme::ThemeWatch;
pub(crate) use oled_sys::i18n::italian;
pub(crate) use oled_sys::i18n::key;
pub(crate) use oled_sys::i18n::Lang;
pub(crate) use oled_sys::i18n::Strings;
pub(crate) use oled_sys::i18n::Table;
pub(crate) use oled_sys::i18n::S;
