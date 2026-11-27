//! Contract test: the dock's theme as the apps resolve it (filled by DS-THEME)

use oled_config::theme::{parse, to_json, Lang};
use oled_ui::theme::{theme_lang, SysOverlay, ThemeSpec, Tokens};

use crate::config::{Background, Config, Language, BACKGROUND_PRESETS};
use crate::theme_sync::project;

/// The text tokens that are white at some alpha: the dock has no white-level control yet, so they never move.
fn text_tokens(t: &Tokens) -> Vec<(&'static str, oled_ui::theme::Ink)> {
    let c = &t.color;
    vec![
        ("fg_primary", c.fg_primary),
        ("fg_hot", c.fg_hot),
        ("fg_secondary", c.fg_secondary),
        ("fg_secondary_hot", c.fg_secondary_hot),
        ("fg_tertiary", c.fg_tertiary),
        ("fg_disabled", c.fg_disabled),
    ]
}

#[test]
fn projection_resolves_to_the_dock_colours_in_the_apps() {
    let mut backgrounds: Vec<Background> = BACKGROUND_PRESETS.iter().map(|(_, b)| *b).collect();
    backgrounds.push(Background(0x1C1C1E));
    let oled = Tokens::oled();
    for bg in backgrounds {
        for (language, want) in [(Language::Auto, Lang::Auto), (Language::En, Lang::En), (Language::It, Lang::It)] {
            for bar_opacity in [40u8, 80, 95] {
                let cfg = Config { background: bg, language, bar_opacity, ..Config::default() };
                let tf = project(&cfg, 1);
                let spec = ThemeSpec::from(&tf);
                let tokens = Tokens::resolve(&spec, &SysOverlay::none());
                let (r, g, b) = cfg.background.rgb();
                assert_eq!(tokens.color.bg.rgb, (r, g, b), "background {:?}", bg);
                for ((name, got), (_, base)) in text_tokens(&tokens).into_iter().zip(text_tokens(&oled)) {
                    assert_eq!(got, base, "{name} for background {bg:?}, opacity {bar_opacity}");
                }
                let i18n = match language {
                    Language::Auto => oled_sys::i18n::Lang::Auto,
                    Language::En => oled_sys::i18n::Lang::En,
                    Language::It => oled_sys::i18n::Lang::It,
                };
                assert_eq!(theme_lang(tf.language), i18n);
                assert_eq!(tf.language, want);
                let bytes = to_json(&tf);
                assert_eq!(parse(bytes.as_bytes()), Ok((tf.clone(), vec![])), "round trip for {bg:?}");
            }
        }
    }
}

#[test]
fn dock_default_is_the_apps_default() {
    let tf = project(&Config::default(), 0);
    let tokens = Tokens::resolve(&ThemeSpec::from(&tf), &SysOverlay::none());
    let mut want = Tokens::oled();
    want.generation = tokens.generation;
    assert_eq!(tokens, want);
}
