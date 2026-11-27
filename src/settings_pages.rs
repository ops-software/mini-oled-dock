//! Page model of the Settings window: pure data, no drawing and no Win32.
//!
//! `rows(page, config, ctx)` turns the config and a little live state into a list of `Row`s, and
//! `click` maps a click on a part of a row to an `Effect`. Adding an option means adding a row.
//! `settings_view` lays the rows out and paints them, `settings_window` applies the effects.

use crate::config::{
    self, Background, BatteryShow, Config, DisplayMode, Language, Position, SizeRange, StartStyle, APP_ICON_SIZE,
    APP_TEXT_SIZE, BACKGROUND_PRESETS, MODULE_SIZE, TRAY_ICON_SIZE,
};
use crate::glyphs;
use crate::i18n::{k, t};
use crate::popup_tray::TrayRow;
use crate::start_import::{self, FailReason};
use crate::ui::Screen;
use crate::update::{Outcome, Phase, UpdateStatus};
use crate::update_release::{release_page_url, safe_release_url};
use oled_sys::i18n::S;

/// A monochrome icon shape: `(u, v)` in a 16 x 16 box -> inside.
pub(crate) type Glyph = fn(f32, f32) -> bool;

/// Pages of the window, in navigation order.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Page {
    Appearance,
    Bar,
    Start,
    Launcher,
    Modules,
    Screen,
    Updates,
    About,
}

impl Page {
    pub(crate) const ALL: [Page; 8] = [
        Page::Appearance,
        Page::Bar,
        Page::Start,
        Page::Launcher,
        Page::Modules,
        Page::Screen,
        Page::Updates,
        Page::About,
    ];

    pub(crate) fn title(self) -> &'static str {
        match self {
            Page::Appearance => t(k::SETTINGS_PAGES_APPEARANCE),
            Page::Bar => t(k::SETTINGS_PAGES_BAR),
            Page::Start => "Start",
            Page::Launcher => "Launcher",
            Page::Modules => t(k::SETTINGS_PAGES_MODULES),
            Page::Screen => t(k::SETTINGS_PAGES_SCREEN),
            Page::Updates => t(k::SETTINGS_PAGES_UPDATES),
            Page::About => t(k::SETTINGS_PAGES_ABOUT),
        }
    }

    pub(crate) fn subtitle(self) -> &'static str {
        match self {
            Page::Appearance => t(k::SETTINGS_PAGES_COLOUR_SIZES_AND_LANGUAGE_OF),
            Page::Bar => t(k::SETTINGS_PAGES_WHERE_THE_BAR_SITS_AND),
            Page::Start => t(k::SETTINGS_PAGES_THE_START_BUTTON_AND_THE),
            Page::Launcher => t(k::SETTINGS_PAGES_FIND_AND_START_ANYTHING_FROM),
            Page::Modules => t(k::SETTINGS_PAGES_WHAT_THE_BAR_SHOWS_ON),
            Page::Screen => t(k::SETTINGS_PAGES_THE_DISPLAY_THE_BAR_SITS),
            Page::Updates => t(k::SETTINGS_PAGES_NEW_VERSIONS_FROM_GITHUB),
            Page::About => t(k::SETTINGS_PAGES_VERSION_MEMORY_AND_FILES),
        }
    }

    pub(crate) fn icon(self) -> Glyph {
        match self {
            Page::Appearance => glyphs::palette_shape,
            Page::Bar => glyphs::bar_shape,
            Page::Start => glyphs::grid_shape,
            Page::Launcher => crate::popup::popup_startmenu::magnifier,
            Page::Modules => glyphs::list_shape,
            Page::Screen => glyphs::monitor_shape,
            Page::Updates => glyphs::refresh_shape,
            Page::About => glyphs::info_shape,
        }
    }

    /// File-name part of the preview PNGs.
    pub(crate) fn slug(self) -> &'static str {
        match self {
            Page::Appearance => "appearance",
            Page::Bar => "bar",
            Page::Start => "start",
            Page::Launcher => "launcher",
            Page::Modules => "modules",
            Page::Screen => "screen",
            Page::Updates => "updates",
            Page::About => "about",
        }
    }

    /// Shown after a gap at the bottom of the navigation.
    pub(crate) fn at_bottom(self) -> bool {
        matches!(self, Page::Updates | Page::About)
    }
}

/// A setting changed by a choice or a switch.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Setting {
    /// `config.language`: System / English / Italiano.
    Language,
    /// `config.start_style`: Compact / Windows 11.
    StartStyle,
    Position,
    /// `config.monitor`: the screen choice, labelled "Display" on the Screen page.
    Monitor,
    /// `config.display_mode`: Icon + name / Icon / Name, labelled "Display mode" on the Bar page.
    DisplayMode,
    AutoHide,
    Previews,
    /// `config.win_opens_start`: Win opens / closes the Start menu together with the bar.
    WinStart,
    /// `config.reveal`: Rest (dwell) / Push at the screen edge.
    Reveal,
    /// The owned keyboard chords as one switch: on when any of the four `config.keyboard` flags is.
    KeyboardChords,
    Seconds,
    VolumePercent,
    /// `Config::module_visible` name.
    Module(&'static str),
    /// `config.show_battery`: Auto / On / Off.
    BatteryShow,
    BatteryPercent,
    BatteryInside,
    UpdateCheck,
    UpdateAutoInstall,
    /// Admin-only `HKLM\SOFTWARE\mini-oled-dock\TrustElevatedTrayIcons`, NOT in the config: the state
    /// comes from `Ctx::trust_elevated` and a click gives `Effect::TrustElevated`.
    TrustElevatedTray,
    /// `config.tray_show_all`: every tray icon in the bar (`tray_hidden` ignored, kept).
    TrayShowAll,
    /// `launcher.enabled`.
    LauncherOn,
    /// `launcher.chord`: Alt+Space / Alt+Shift+Space / Off.
    LauncherChord,
    /// `launcher.with_powertoys`: leave Alt+Space to Command Palette / take it.
    LauncherPowerToys,
    /// `launcher.monitor`: Pointer / Active window / Primary.
    LauncherMonitor,
    /// `launcher.over_fullscreen`: Pass / Open.
    LauncherFullscreen,
    /// `launcher.providers` entry (apps cannot be switched off).
    LauncherProvider(&'static str),
    /// `launcher.web_engine`: Google / Bing / DuckDuckGo / Custom (the template is set in the config file).
    LauncherWeb,
    /// `launcher.learn`: frecency and run history.
    LauncherLearn,
}

/// A value set by a slider.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Slider {
    Opacity,
    AppIcon,
    AppText,
    TrayIcon,
    Module,
    /// `config.show_delay_ms`, in steps of 50 ms.
    ShowDelay,
    /// `config.hide_delay_ms`, in steps of 100 ms.
    HideDelay,
    /// `launcher.width`, in steps of 40.
    LauncherWidth,
    /// `launcher.rows`.
    LauncherRows,
}

/// A button that runs something instead of changing a setting.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Action {
    ResetSizes,
    ImportStart,
    CheckUpdates,
    ReleaseNotes,
    InstallUpdate,
    OpenDataFolder,
    OpenLog,
    ExitDock,
    /// Settings > Launcher: the run history and the learned order go.
    ForgetHistory,
    /// Settings > Launcher: `config.json` in Notepad.
    EditConfig,
    /// Settings > Launcher: the launcher opens on its alias prompt.
    AddLauncherAlias,
    /// Settings > Launcher: Remove on an alias row, the `aliases::word_hash` of its word.
    RemoveLauncherAlias(u64),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Button {
    pub action: Action,
    pub text: &'static str,
    pub enabled: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Control {
    /// Group header; the value is drawn dim at the right.
    Header {
        value: Option<String>,
    },
    /// Mini live bar (Appearance).
    Preview,
    Toggle {
        setting: Setting,
        on: bool,
        enabled: bool,
    },
    Choice {
        setting: Setting,
        options: Vec<&'static str>,
        selected: Option<usize>,
    },
    Slider {
        slider: Slider,
        value: i32,
        min: i32,
        max: i32,
        unit: &'static str,
    },
    /// The presets, then the custom swatch (index `BACKGROUND_PRESETS.len()`).
    Swatches {
        selected: usize,
        picker_open: bool,
    },
    Picker,
    Buttons(Vec<Button>),
    Chooser {
        open: bool,
    },
    /// A tray app of the inline chooser, by its `tray_host` key, never by index.
    /// `enabled` is false while `config.tray_show_all` is on: the switch reads as on and is dimmed.
    TrayApp {
        key: String,
        visible: bool,
        enabled: bool,
    },
    Status(String),
    Note,
}

#[derive(Clone, Debug)]
pub(crate) struct Row {
    pub icon: Option<Glyph>,
    pub label: String,
    pub desc: Option<String>,
    pub control: Control,
}

/// Same label, description and control; the icon is ignored.
pub(crate) fn same_rows(a: &[Row], b: &[Row]) -> bool {
    a.len() == b.len() && a.iter().zip(b).all(|(x, y)| x.label == y.label && x.desc == y.desc && x.control == y.control)
}

/// Memory figures of the About page (bytes).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct Memory {
    pub now: Option<usize>,
    pub trimmed: Option<usize>,
}

/// Live state the rows depend on besides the config.
pub(crate) struct Ctx<'a> {
    pub screens: &'a [Screen],
    pub picker_open: bool,
    pub chooser_open: bool,
    pub tray: &'a [TrayRow],
    pub import: start_import::Status,
    pub update: &'a UpdateStatus,
    pub memory: Memory,
    pub data_dir: Option<&'a str>,
    pub log_path: Option<&'a str>,
    /// `tray_host::trust_elevated_tray_icons` (the HKLM switch).
    pub trust_elevated: bool,
    /// A PowerToys chord owner (Command Palette or PowerToys Run) is running (`popup_launcher::pt_detected`).
    pub powertoys: bool,
    /// The last chord picked on the Launcher page was refused because another app holds it: `Some(shift)` is Alt+Shift+Space.
    pub chord_held: Option<bool>,
    /// `config::load_notes().show_battery_invalid`: config.json held an unknown `show_battery` value.
    pub battery_note: bool,
    /// `config::load_notes().new_keys_invalid`: config.json held a value of the wrong type in a key new in 1.6.
    pub keys_note: bool,
}

// ------------------------------------------------------------------ helpers moved from the old popup

/// Row label of a `config::MODULE_NAMES` entry (or "start").
pub(crate) fn module_label(name: &str) -> &'static str {
    match name {
        "notifications" => t(k::COMMON_NOTIFICATIONS),
        "volume" => t(k::SETTINGS_PAGES_VOLUME),
        "network" => t(k::SETTINGS_PAGES_NETWORK),
        "metrics" => t(k::SETTINGS_PAGES_SYSTEM_METRICS),
        "clock" => t(k::SETTINGS_PAGES_CLOCK),
        "show_desktop" => t(k::SETTINGS_PAGES_SHOW_DESKTOP),
        "start" => t(k::SETTINGS_PAGES_START_BUTTON),
        "tray" => t(k::SETTINGS_PAGES_TRAY_ICONS),
        _ => t(k::SETTINGS_PAGES_MODULE),
    }
}

/// Labels of the segments of a choice (`sc`: the displays, for the screen choice).
pub(crate) fn options(s: Setting, sc: &[Screen]) -> Vec<&'static str> {
    match s {
        // Each language in its own name, so it can be found whatever the current one is.
        Setting::Language => vec![t(k::SETTINGS_PAGES_SYSTEM), "English", "Italiano"],
        Setting::StartStyle => vec![t(k::SETTINGS_PAGES_COMPACT), t(k::SETTINGS_PAGES_WINDOWS_11)],
        Setting::Position => vec![t(k::SETTINGS_PAGES_TOP), t(k::SETTINGS_PAGES_BOTTOM)],
        Setting::Monitor => {
            // Last segment: follow the pointer (`config::MONITOR_POINTER`).
            let main = t(k::SETTINGS_PAGES_MAIN);
            sc.iter()
                .map(|d| if d.primary { main } else { d.label })
                .chain(std::iter::once(t(k::SETTINGS_PAGES_POINTER)))
                .collect()
        }
        Setting::DisplayMode => {
            vec![t(k::SETTINGS_PAGES_ICON_NAME), t(k::SETTINGS_PAGES_ICON), t(k::SETTINGS_PAGES_NAME)]
        }
        Setting::Reveal => vec![t(k::SETTINGS_PAGES_REST), t(k::SETTINGS_PAGES_PUSH)],
        Setting::BatteryShow => vec![t(k::SETTINGS_PAGES_AUTO), t(k::SETTINGS_PAGES_ON), t(k::SETTINGS_PAGES_OFF)],
        Setting::LauncherChord => {
            vec![t(k::SETTINGS_PAGES_ALT_SPACE), t(k::SETTINGS_PAGES_ALT_SHIFT_SPACE), t(k::SETTINGS_PAGES_OFF)]
        }
        Setting::LauncherPowerToys => vec![t(k::SETTINGS_PAGES_LEAVE_IT), t(k::SETTINGS_PAGES_TAKE_IT)],
        Setting::LauncherMonitor => {
            vec![t(k::SETTINGS_PAGES_POINTER), t(k::SETTINGS_PAGES_ACTIVE_WINDOW), t(k::SETTINGS_PAGES_PRIMARY)]
        }
        Setting::LauncherFullscreen => vec![t(k::SETTINGS_PAGES_PASS), t(k::COMMON_OPEN)],
        Setting::LauncherWeb => vec!["Google", "Bing", "DuckDuckGo", t(k::SETTINGS_PAGES_CUSTOM)],
        _ => Vec::new(),
    }
}

/// Selected segment of a choice. Screen: the chosen display, or "Main" (the primary, first in `sc`)
/// when none is chosen or the chosen one is not connected (that is where the bar is).
pub(crate) fn selected(c: &Config, s: Setting, sc: &[Screen]) -> Option<usize> {
    Some(match s {
        Setting::Language => match c.language {
            Language::Auto => 0,
            Language::En => 1,
            Language::It => 2,
        },
        Setting::StartStyle => usize::from(c.start_style == StartStyle::Windows11),
        Setting::Position => usize::from(c.position == Position::Bottom),
        Setting::Reveal => usize::from(c.reveal == config::Reveal::Push),
        Setting::Monitor => {
            if c.monitor.eq_ignore_ascii_case(config::MONITOR_POINTER) {
                sc.len()
            } else {
                sc.iter().position(|d| !d.primary && d.device.eq_ignore_ascii_case(&c.monitor)).unwrap_or(0)
            }
        }
        Setting::DisplayMode => match c.display_mode {
            DisplayMode::IconText => 0,
            DisplayMode::Icon => 1,
            DisplayMode::Text => 2,
        },
        Setting::BatteryShow => match c.show_battery {
            BatteryShow::Auto => 0,
            BatteryShow::On => 1,
            BatteryShow::Off => 2,
        },
        Setting::LauncherChord => match c.launcher.chord.as_str() {
            "alt+space" => 0,
            "alt+shift+space" => 1,
            _ => 2,
        },
        Setting::LauncherPowerToys => usize::from(c.launcher.with_powertoys == "take"),
        Setting::LauncherMonitor => match c.launcher.monitor.as_str() {
            "active" => 1,
            "primary" => 2,
            _ => 0,
        },
        Setting::LauncherFullscreen => usize::from(c.launcher.over_fullscreen == "open"),
        Setting::LauncherWeb => match c.launcher.web_engine.as_str() {
            "bing" => 1,
            "duckduckgo" => 2,
            "custom" => 3,
            _ => 0,
        },
        _ => return None,
    })
}

/// Sets segment `j` of choice `s`. Screen: the primary is stored as "" (the bar then follows the
/// primary monitor even if Windows changes which one it is).
pub(crate) fn choose(c: &mut Config, s: Setting, j: usize, sc: &[Screen]) {
    match s {
        Setting::Language => c.language = [Language::Auto, Language::En, Language::It][j.min(2)],
        Setting::StartStyle => c.start_style = if j == 0 { StartStyle::Compact } else { StartStyle::Windows11 },
        Setting::Position => c.position = if j == 0 { Position::Top } else { Position::Bottom },
        Setting::Monitor => {
            c.monitor = if j == sc.len() {
                config::MONITOR_POINTER.to_string()
            } else {
                sc.get(j).filter(|d| !d.primary).map(|d| d.device.clone()).unwrap_or_default()
            };
        }
        Setting::DisplayMode => {
            c.display_mode = [DisplayMode::IconText, DisplayMode::Icon, DisplayMode::Text][j.min(2)];
        }
        Setting::Reveal => c.reveal = if j == 0 { config::Reveal::Dwell } else { config::Reveal::Push },
        Setting::BatteryShow => c.show_battery = [BatteryShow::Auto, BatteryShow::On, BatteryShow::Off][j.min(2)],
        Setting::LauncherChord => {
            c.launcher.chord = ["alt+space", "alt+shift+space", "off"][j.min(2)].into();
            // Chosen here: the one-time offer in the Start menu has nothing left to ask.
            c.launcher.offer_done = true;
        }
        Setting::LauncherPowerToys => {
            c.launcher.with_powertoys = if j == 0 { "yield" } else { "take" }.into();
            c.launcher.powertoys_done = true;
        }
        Setting::LauncherMonitor => c.launcher.monitor = ["pointer", "active", "primary"][j.min(2)].into(),
        Setting::LauncherFullscreen => c.launcher.over_fullscreen = if j == 0 { "pass" } else { "open" }.into(),
        Setting::LauncherWeb => {
            let engine = ["google", "bing", "duckduckgo", "custom"][j.min(3)];
            // Custom needs a template that passes the validator, and the Settings window has no text field: with none
            // in the config file the click changes nothing.
            if engine != "custom" || crate::launcher::web::validate_template(&c.launcher.web_url).is_ok() {
                c.launcher.web_engine = engine.into();
            }
        }
        _ => {}
    }
}

/// The notice under the Shortcut row after a refused pick.
fn chord_held_text(shift: bool) -> &'static str {
    if shift {
        t(k::COMMON_ANOTHER_APP_HOLDS_ALT_SHIFT)
    } else {
        t(k::SETTINGS_PAGES_ANOTHER_APP_HOLDS_ALT_SPACE)
    }
}

/// The chord an edit would newly take, to be probed first: `Some(shift)` for a pick of Alt+Space or Alt+Shift+Space that
/// differs from the config's. Off, the current chord and every other edit need no probe.
pub(crate) fn chord_to_probe(e: Edit, current: &str) -> Option<bool> {
    match e {
        Edit::Choose(Setting::LauncherChord, 0) if current != "alt+space" => Some(false),
        Edit::Choose(Setting::LauncherChord, 1) if current != "alt+shift+space" => Some(true),
        _ => None,
    }
}

pub(crate) fn is_on(c: &Config, s: Setting) -> bool {
    match s {
        Setting::AutoHide => c.autohide,
        Setting::Previews => c.window_previews,
        Setting::WinStart => c.win_opens_start,
        Setting::KeyboardChords => {
            let k = c.keyboard;
            k.win_numbers || k.win_t || k.win_b || k.peek
        }
        Setting::Seconds => c.show_seconds,
        Setting::VolumePercent => c.show_volume_percent,
        Setting::BatteryPercent => c.battery_percent,
        Setting::BatteryInside => c.battery_percent_inside,
        Setting::UpdateCheck => c.update_check,
        Setting::UpdateAutoInstall => c.update_auto_install,
        Setting::TrayShowAll => c.tray_show_all,
        Setting::Module(n) => c.module_visible(n),
        Setting::LauncherOn => c.launcher.enabled,
        Setting::LauncherLearn => c.launcher.learn,
        Setting::LauncherProvider(n) => c.launcher.providers.get(n).copied().unwrap_or(true),
        _ => false,
    }
}

pub(crate) fn flip(c: &mut Config, s: Setting) {
    match s {
        Setting::AutoHide => c.autohide = !c.autohide,
        Setting::Previews => c.window_previews = !c.window_previews,
        Setting::WinStart => c.win_opens_start = !c.win_opens_start,
        // On gives the chords their defaults (so changing a default changes this switch with it), off turns all four off.
        Setting::KeyboardChords => {
            c.keyboard = if is_on(c, s) {
                config::KeyboardConfig { win_numbers: false, win_t: false, win_b: false, peek: false }
            } else {
                config::KeyboardConfig::default()
            }
        }
        Setting::Seconds => c.show_seconds = !c.show_seconds,
        Setting::VolumePercent => c.show_volume_percent = !c.show_volume_percent,
        Setting::BatteryPercent => c.battery_percent = !c.battery_percent,
        Setting::BatteryInside => c.battery_percent_inside = !c.battery_percent_inside,
        Setting::UpdateCheck => c.update_check = !c.update_check,
        Setting::UpdateAutoInstall => c.update_auto_install = !c.update_auto_install,
        Setting::TrayShowAll => c.tray_show_all = !c.tray_show_all,
        Setting::Module(n) => {
            let on = c.module_visible(n);
            c.set_module_visible(n, !on);
        }
        Setting::LauncherOn => c.launcher.enabled = !c.launcher.enabled,
        Setting::LauncherLearn => c.launcher.learn = !c.launcher.learn,
        // Apps are always on (the config sanitizer holds the same line).
        Setting::LauncherProvider("apps") => {}
        Setting::LauncherProvider(n) => {
            let on = c.launcher.providers.get(n).copied().unwrap_or(true);
            c.launcher.providers.insert(n.to_string(), !on);
        }
        _ => {}
    }
}

// ------------------------------------------------------------------ sliders

/// `(min, max, unit)` of a slider.
pub(crate) fn slider_range(sl: Slider) -> (i32, i32, &'static str) {
    let r = |s: SizeRange, unit| (s.min as i32, s.max as i32, unit);
    match sl {
        Slider::Opacity => (0, 100, "%"),
        Slider::AppIcon => r(APP_ICON_SIZE, " px"),
        Slider::AppText => r(APP_TEXT_SIZE, " px"),
        Slider::TrayIcon => r(TRAY_ICON_SIZE, " px"),
        Slider::Module => r(MODULE_SIZE, "%"),
        Slider::ShowDelay => (config::SHOW_DELAY_RANGE.0 as i32, config::SHOW_DELAY_RANGE.1 as i32, " ms"),
        Slider::HideDelay => (config::HIDE_DELAY_RANGE.0 as i32, config::HIDE_DELAY_RANGE.1 as i32, " ms"),
        Slider::LauncherWidth => (config::LAUNCHER_WIDTH.0 as i32, config::LAUNCHER_WIDTH.1 as i32, " px"),
        Slider::LauncherRows => (config::LAUNCHER_ROWS.0 as i32, config::LAUNCHER_ROWS.1 as i32, ""),
    }
}

pub(crate) fn slider_value(c: &Config, sl: Slider) -> i32 {
    match sl {
        Slider::LauncherWidth => return i32::from(c.launcher.width),
        Slider::LauncherRows => return i32::from(c.launcher.rows),
        Slider::ShowDelay => return c.show_delay_ms as i32,
        Slider::HideDelay => return c.hide_delay_ms as i32,
        _ => {}
    }
    let (icon, text, tray, module) = c.sizes();
    (match sl {
        Slider::Opacity => c.bar_opacity.min(100),
        Slider::AppIcon => icon,
        Slider::AppText => text,
        Slider::TrayIcon => tray,
        Slider::Module => module,
        Slider::LauncherWidth | Slider::LauncherRows | Slider::ShowDelay | Slider::HideDelay => 0,
    }) as i32
}

/// Sets slider `sl` to `v` (clamped to its range).
pub(crate) fn set_slider(c: &mut Config, sl: Slider, v: i32) {
    let (lo, hi, _) = slider_range(sl);
    let v = v.clamp(lo, hi);
    // The width moves in steps of 40 (the config sanitizer holds the same grid).
    if sl == Slider::LauncherWidth {
        c.launcher.width = ((v + 20) / 40 * 40).clamp(lo, hi) as u16;
        return;
    }
    // The delays move in steps of 50 and 100 ms: rounded down to the step, then clamped.
    if let Slider::ShowDelay | Slider::HideDelay = sl {
        let step = if sl == Slider::ShowDelay { 50 } else { 100 };
        let v = (v / step * step).clamp(lo, hi) as u32;
        if sl == Slider::ShowDelay {
            c.show_delay_ms = v;
        } else {
            c.hide_delay_ms = v;
        }
        return;
    }
    let v = v as u8;
    match sl {
        Slider::Opacity => c.bar_opacity = v,
        Slider::AppIcon => c.app_icon_size = v,
        Slider::AppText => c.app_text_size = v,
        Slider::TrayIcon => c.tray_icon_size = v,
        Slider::Module => c.module_size = v,
        Slider::LauncherRows => c.launcher.rows = v,
        Slider::LauncherWidth | Slider::ShowDelay | Slider::HideDelay => {}
    }
}

pub(crate) fn reset_sizes(c: &mut Config) {
    c.app_icon_size = APP_ICON_SIZE.default;
    c.app_text_size = APP_TEXT_SIZE.default;
    c.tray_icon_size = TRAY_ICON_SIZE.default;
    c.module_size = MODULE_SIZE.default;
}

pub(crate) fn sizes_are_default(c: &Config) -> bool {
    c.sizes() == (APP_ICON_SIZE.default, APP_TEXT_SIZE.default, TRAY_ICON_SIZE.default, MODULE_SIZE.default)
}

// ------------------------------------------------------------------ colour

/// HSV (hue 0..360, saturation and value 0..1) -> RGB 0..=255.
pub(crate) fn hsv_to_rgb(h: f32, s: f32, v: f32) -> (u32, u32, u32) {
    let h = h.rem_euclid(360.0) / 60.0;
    let (s, v) = (s.clamp(0.0, 1.0), v.clamp(0.0, 1.0));
    let c = v * s;
    let x = c * (1.0 - (h % 2.0 - 1.0).abs());
    let (r, g, b) = match h as u32 {
        0 => (c, x, 0.0),
        1 => (x, c, 0.0),
        2 => (0.0, c, x),
        3 => (0.0, x, c),
        4 => (x, 0.0, c),
        _ => (c, 0.0, x),
    };
    let m = v - c;
    let q = |f: f32| ((f + m) * 255.0 + 0.5).clamp(0.0, 255.0) as u32;
    (q(r), q(g), q(b))
}

/// RGB -> HSV (hue 0 for greys).
pub(crate) fn rgb_to_hsv((r, g, b): (u32, u32, u32)) -> (f32, f32, f32) {
    let (r, g, b) = (r as f32 / 255.0, g as f32 / 255.0, b as f32 / 255.0);
    let max = r.max(g).max(b);
    let min = r.min(g).min(b);
    let d = max - min;
    let h = if d <= 0.0 {
        0.0
    } else if max == r {
        60.0 * ((g - b) / d).rem_euclid(6.0)
    } else if max == g {
        60.0 * ((b - r) / d + 2.0)
    } else {
        60.0 * ((r - g) / d + 4.0)
    };
    let s = if max <= 0.0 { 0.0 } else { d / max };
    (h, s, max)
}

pub(crate) fn background_of((h, s, v): (f32, f32, f32)) -> Background {
    let (r, g, b) = hsv_to_rgb(h, s, v);
    Background(r << 16 | g << 8 | b)
}

/// Name of preset `i` in the dock language (the table keeps the English one).
pub(crate) fn preset_name(i: usize) -> &'static str {
    const NAMES: [S; 7] = [
        k::SETTINGS_PAGES_PRESET_BLACK,
        k::SETTINGS_PAGES_PRESET_GRAPHITE,
        k::SETTINGS_PAGES_PRESET_SLATE,
        k::SETTINGS_PAGES_PRESET_NAVY,
        k::SETTINGS_PAGES_PRESET_PLUM,
        k::SETTINGS_PAGES_PRESET_WINE,
        k::SETTINGS_PAGES_PRESET_FOREST,
    ];
    NAMES.get(i).map_or("", |&key| t(key))
}

/// Index of the preset equal to `bg`.
pub(crate) fn preset_index(bg: Background) -> Option<usize> {
    BACKGROUND_PRESETS.iter().position(|(_, p)| *p == bg)
}

/// Swatch `k` (presets, then the custom one) is the current colour.
#[cfg(test)]
pub(crate) fn swatch_selected(bg: Background, k: usize) -> bool {
    match preset_index(bg) {
        Some(p) => p == k,
        None => k == BACKGROUND_PRESETS.len(),
    }
}

/// Text of the import row for `st`.
pub(crate) fn import_label(st: start_import::Status) -> String {
    match st {
        start_import::Status::Idle => t(k::SETTINGS_PAGES_IMPORT_FROM_WINDOWS_START).to_string(),
        start_import::Status::Running => t(k::SETTINGS_PAGES_IMPORTING).to_string(),
        start_import::Status::Failed(why) => match why {
            FailReason::Export => t(k::SETTINGS_PAGES_IMPORT_FAILED_WINDOWS_DID_NOT),
            FailReason::CannotStart => t(k::SETTINGS_PAGES_IMPORT_FAILED_THE_EXPORT_COULD),
            FailReason::NoSystemDir => t(k::SETTINGS_PAGES_IMPORT_FAILED_THE_SYSTEM_FOLDER),
            FailReason::Layout => t(k::SETTINGS_PAGES_IMPORT_FAILED_THE_LAYOUT_FILE),
        }
        .to_string(),
        start_import::Status::TimedOut => t(k::SETTINGS_PAGES_IMPORT_FAILED_TIMEOUT).to_string(),
        start_import::Status::Imported(n) => {
            let key = if n == 1 { k::SETTINGS_PAGES_IMPORT_DONE_ONE } else { k::SETTINGS_PAGES_IMPORT_DONE_MANY };
            oled_sys::i18n::subst(t(key), &[("n", &n.to_string())])
        }
    }
}

// ------------------------------------------------------------------ edits and clicks

/// A change to the config, applied by the window (`config::modify` or `config::set`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Edit {
    Choose(Setting, usize),
    Flip(Setting),
    Slider(Slider, i32),
    Background(Background),
    ResetSizes,
    /// Takes out the alias whose word has this `aliases::word_hash` (a list changed in between never loses another).
    RemoveLauncherAlias(u64),
}

pub(crate) fn apply_edit(c: &mut Config, e: Edit, screens: &[Screen]) {
    match e {
        Edit::Choose(s, j) => choose(c, s, j, screens),
        Edit::Flip(s) => flip(c, s),
        Edit::Slider(sl, v) => set_slider(c, sl, v),
        Edit::Background(b) => c.background = b,
        Edit::ResetSizes => reset_sizes(c),
        Edit::RemoveLauncherAlias(h) => {
            crate::launcher::aliases::remove_by_hash(&mut c.launcher.aliases, h);
        }
    }
}

/// What a click on a row does.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Effect {
    None,
    Save(Edit),
    TogglePicker,
    ToggleChooser,
    TrayVisible { key: String, hidden: bool },
    TrustElevated(bool),
    Run(Action),
}

/// Effect of a click on part `part` of `row` (a segment, a swatch or a button).
pub(crate) fn click(row: &Row, part: Option<usize>, ctx: &Ctx<'_>) -> Effect {
    match &row.control {
        Control::Choice { setting, options, selected } => match part {
            Some(j) if j < options.len() && *selected != Some(j) => Effect::Save(Edit::Choose(*setting, j)),
            _ => Effect::None,
        },
        Control::Toggle { setting: Setting::TrustElevatedTray, on, enabled: true } => Effect::TrustElevated(!*on),
        Control::Toggle { setting, enabled, .. } => {
            if *enabled {
                Effect::Save(Edit::Flip(*setting))
            } else {
                Effect::None
            }
        }
        Control::Swatches { selected, .. } => match part {
            Some(k) if k == BACKGROUND_PRESETS.len() => Effect::TogglePicker,
            Some(k) if k < BACKGROUND_PRESETS.len() && *selected != k => {
                Effect::Save(Edit::Background(BACKGROUND_PRESETS[k].1))
            }
            _ => Effect::None,
        },
        Control::Buttons(buttons) => match part.and_then(|j| buttons.get(j)) {
            Some(b) if b.enabled => match b.action {
                Action::ResetSizes => Effect::Save(Edit::ResetSizes),
                Action::RemoveLauncherAlias(h) => Effect::Save(Edit::RemoveLauncherAlias(h)),
                a => Effect::Run(a),
            },
            _ => Effect::None,
        },
        Control::Chooser { .. } => Effect::ToggleChooser,
        // The row carries the app's key; it acts only while that app is still in the live list.
        Control::TrayApp { key, visible, enabled: true } if ctx.tray.iter().any(|t| t.key == *key) => {
            Effect::TrayVisible { key: key.clone(), hidden: *visible }
        }
        _ => Effect::None,
    }
}

// ------------------------------------------------------------------ rows

fn row(icon: Glyph, label: &str, desc: Option<&str>, control: Control) -> Row {
    Row { icon: Some(icon), label: label.to_string(), desc: desc.map(str::to_string), control }
}

fn header(label: &str, value: Option<String>) -> Row {
    Row { icon: None, label: label.to_string(), desc: None, control: Control::Header { value } }
}

fn toggle(icon: Glyph, label: &str, desc: Option<&str>, c: &Config, setting: Setting) -> Row {
    row(icon, label, desc, Control::Toggle { setting, on: is_on(c, setting), enabled: true })
}

fn choice(icon: Glyph, label: &str, desc: Option<&str>, c: &Config, setting: Setting, sc: &[Screen]) -> Row {
    let control = Control::Choice { setting, options: options(setting, sc), selected: selected(c, setting, sc) };
    row(icon, label, desc, control)
}

fn slider_row(icon: Glyph, label: &str, c: &Config, slider: Slider) -> Row {
    let (min, max, unit) = slider_range(slider);
    row(icon, label, None, Control::Slider { slider, value: slider_value(c, slider), min, max, unit })
}

fn buttons(icon: Glyph, label: &str, desc: Option<&str>, list: Vec<Button>) -> Row {
    row(icon, label, desc, Control::Buttons(list))
}

/// Modules with a plain on/off row on the Modules page (explicit: `config::MODULE_NAMES` also holds "tray").
pub(crate) const MODULE_TOGGLES: [&str; 6] = ["notifications", "volume", "network", "metrics", "clock", "show_desktop"];

/// `YYYY-MM-DD` of an RFC 3339 time; anything else is returned unchanged.
pub(crate) fn short_date(rfc3339: &str) -> &str {
    let d = rfc3339.get(..10).unwrap_or("");
    let b = d.as_bytes();
    let shaped = b.len() == 10
        && b.iter().enumerate().all(|(i, c)| if i == 4 || i == 7 { *c == b'-' } else { c.is_ascii_digit() });
    if shaped {
        d
    } else {
        rfc3339
    }
}

/// `"{:.1} MB"` of `bytes`.
pub(crate) fn mb(bytes: usize) -> String {
    format!("{:.1} MB", bytes as f64 / 1_048_576.0)
}

/// Rows of `page`.
pub(crate) fn rows(page: Page, c: &Config, ctx: &Ctx<'_>) -> Vec<Row> {
    match page {
        Page::Appearance => appearance(c, ctx),
        Page::Bar => bar(c, ctx),
        Page::Start => start(c, ctx),
        Page::Launcher => launcher(c, ctx),
        Page::Modules => modules(c, ctx),
        Page::Screen => screen(c, ctx),
        Page::Updates => updates(c, ctx),
        Page::About => about(ctx),
    }
}

fn modules(c: &Config, ctx: &Ctx<'_>) -> Vec<Row> {
    let icon = |n: &str| -> Glyph {
        match n {
            "notifications" => glyphs::bell_shape,
            "volume" => glyphs::speaker_icon,
            "network" => glyphs::wifi_icon,
            "metrics" => glyphs::bars_shape,
            "clock" => glyphs::clock_shape,
            _ => glyphs::show_desktop_shape,
        }
    };
    let mut v = vec![header(t(k::SETTINGS_PAGES_MODULES), None)];
    for n in MODULE_TOGGLES {
        v.push(toggle(icon(n), module_label(n), None, c, Setting::Module(n)));
    }
    v.push(header(t(k::SETTINGS_PAGES_BATTERY), None));
    v.push(choice(
        glyphs::battery_icon,
        t(k::SETTINGS_PAGES_SHOW_BATTERY),
        Some(if ctx.battery_note {
            t(k::SETTINGS_PAGES_CONFIG_JSON_HAS_AN_UNKNOWN)
        } else {
            t(k::SETTINGS_PAGES_SHOWN_AUTOMATICALLY_ON_LAPTOPS)
        }),
        c,
        Setting::BatteryShow,
        ctx.screens,
    ));
    v.push(toggle(glyphs::text_shape, t(k::SETTINGS_PAGES_PERCENTAGE), None, c, Setting::BatteryPercent));
    v.push(row(
        glyphs::battery_icon,
        t(k::SETTINGS_PAGES_PERCENTAGE_INSIDE_THE_ICON),
        Some(t(k::SETTINGS_PAGES_APPLE_STYLE)),
        Control::Toggle { setting: Setting::BatteryInside, on: c.battery_percent_inside, enabled: c.battery_percent },
    ));
    v.push(header(t(k::SETTINGS_PAGES_TRAY), None));
    v.push(toggle(glyphs::ring_shape, module_label("tray"), None, c, Setting::Module("tray")));
    if c.module_visible("tray") {
        v.push(row(
            glyphs::lock_shape,
            t(k::SETTINGS_PAGES_ICONS_OF_APPS_RUN_AS),
            Some(t(k::SETTINGS_PAGES_ALLOW_CLICKS_ON_THEIR_ICONS)),
            Control::Toggle { setting: Setting::TrustElevatedTray, on: ctx.trust_elevated, enabled: true },
        ));
        v.push(toggle(
            glyphs::eye_icon,
            t(k::SETTINGS_PAGES_SHOW_ALL),
            Some(t(k::SETTINGS_PAGES_SHOW_EVERY_TRAY_ICON_IN)),
            c,
            Setting::TrayShowAll,
        ));
        v.push(row(
            glyphs::eye_icon,
            t(k::SETTINGS_PAGES_CHOOSE_VISIBLE_ICONS),
            None,
            Control::Chooser { open: ctx.chooser_open },
        ));
        if ctx.chooser_open {
            if ctx.tray.is_empty() {
                v.push(Row {
                    icon: None,
                    label: t(k::COMMON_NO_TRAY_APPS).to_string(),
                    desc: None,
                    control: Control::Note,
                });
            }
            for tray in ctx.tray {
                let no_room = tray.visible && !tray.in_bar;
                v.push(Row {
                    icon: None,
                    label: tray.name.clone(),
                    desc: no_room.then(|| t(k::SETTINGS_PAGES_NO_ROOM_IN_THE_BAR).to_string()),
                    control: Control::TrayApp {
                        key: tray.key.clone(),
                        visible: tray.visible || c.tray_show_all,
                        enabled: !c.tray_show_all,
                    },
                });
            }
        }
    }
    v
}

fn updates(c: &Config, ctx: &Ctx<'_>) -> Vec<Row> {
    let st = ctx.update;
    let idle = st.phase == Phase::Idle;
    let available = match &st.available {
        Some(a) if a.date.is_empty() => a.tag.clone(),
        Some(a) => format!("{} \u{b7} {}", a.tag, short_date(&a.date)),
        None if st.last_check.is_empty() => t(k::SETTINGS_PAGES_NOT_CHECKED_YET).to_string(),
        None => t(k::SETTINGS_PAGES_UP_TO_DATE).to_string(),
    };
    let last_check = if st.last_check.is_empty() {
        t(k::SETTINGS_PAGES_NEVER).to_string()
    } else {
        short_date(&st.last_check).to_string()
    };
    // The outcome of the last action, never the running phase (the Note row shows that).
    let last_action = if st.outcome == Outcome::None {
        t(k::SETTINGS_PAGES_NONE_YET).to_string()
    } else {
        UpdateStatus { phase: Phase::Idle, ..st.clone() }.line()
    };
    let notes_ok = st.available.as_ref().is_some_and(|a| safe_release_url(&release_page_url(Some(&a.tag))));
    let mut v = vec![
        header(t(k::SETTINGS_PAGES_STATUS), None),
        row(
            glyphs::info_shape,
            t(k::SETTINGS_PAGES_INSTALLED_VERSION),
            None,
            Control::Status(st.installed.to_string()),
        ),
        row(glyphs::refresh_shape, t(k::SETTINGS_PAGES_AVAILABLE), None, Control::Status(available)),
        row(glyphs::clock_shape, t(k::SETTINGS_PAGES_LAST_CHECK), None, Control::Status(last_check)),
        row(glyphs::log_shape, t(k::SETTINGS_PAGES_LAST_ACTION), None, Control::Status(last_action)),
        row(
            glyphs::refresh_shape,
            "",
            None,
            Control::Buttons(vec![
                Button { action: Action::CheckUpdates, text: t(k::SETTINGS_PAGES_CHECK_NOW), enabled: idle },
                Button { action: Action::ReleaseNotes, text: t(k::SETTINGS_PAGES_RELEASE_NOTES), enabled: notes_ok },
                Button {
                    action: Action::InstallUpdate,
                    text: t(k::SETTINGS_PAGES_UPDATE_NOW),
                    enabled: idle && st.available.is_some(),
                },
            ]),
        ),
    ];
    if !idle {
        v.push(Row { icon: None, label: st.line(), desc: None, control: Control::Note });
    }
    v.push(header(t(k::SETTINGS_PAGES_AUTOMATIC), None));
    v.push(toggle(
        glyphs::refresh_shape,
        t(k::SETTINGS_PAGES_CHECK_AUTOMATICALLY),
        Some(t(k::SETTINGS_PAGES_ONCE_A_DAY_ON_GITHUB)),
        c,
        Setting::UpdateCheck,
    ));
    v.push(toggle(
        glyphs::download_shape,
        t(k::SETTINGS_PAGES_INSTALL_AUTOMATICALLY),
        Some(t(k::SETTINGS_PAGES_WHEN_THE_DOCK_IS_IDLE)),
        c,
        Setting::UpdateAutoInstall,
    ));
    v
}

fn about(ctx: &Ctx<'_>) -> Vec<Row> {
    let mem = |b: Option<usize>, none: &str| b.map_or_else(|| none.to_string(), mb);
    let files = |icon: Glyph, label: &str, path: Option<&str>, action| {
        buttons(icon, label, path, vec![Button { action, text: t(k::COMMON_OPEN), enabled: path.is_some() }])
    };
    vec![
        header(t(k::COMMON_DOCK), None),
        row(
            glyphs::info_shape,
            &format!("mini-oled-dock {}", crate::preview::version_text()),
            Some(t(k::SETTINGS_PAGES_OPS_SOFTWARE_MIT_LICENCE)),
            Control::Note,
        ),
        header(t(k::SETTINGS_PAGES_MEMORY), None),
        row(
            glyphs::bars_shape,
            t(k::SETTINGS_PAGES_MEMORY_NOW),
            None,
            Control::Status(mem(ctx.memory.now, "\u{2014}")),
        ),
        row(
            glyphs::bars_shape,
            t(k::SETTINGS_PAGES_AFTER_TRIMMING),
            Some(t(k::SETTINGS_PAGES_WHILE_THE_BAR_IS_HIDDEN)),
            Control::Status(mem(ctx.memory.trimmed, t(k::SETTINGS_PAGES_NOT_TRIMMED_YET))),
        ),
        header(t(k::COMMON_FILES), None),
        files(glyphs::folder_shape, t(k::SETTINGS_PAGES_DATA_FOLDER), ctx.data_dir, Action::OpenDataFolder),
        files(glyphs::log_shape, "Log", ctx.log_path, Action::OpenLog),
        header(t(k::SETTINGS_PAGES_TASKBAR), None),
        buttons(
            glyphs::power_shape,
            t(k::SETTINGS_PAGES_RESTORE_THE_WINDOWS_TASKBAR),
            Some(t(k::SETTINGS_PAGES_EXITS_THE_DOCK)),
            vec![Button { action: Action::ExitDock, text: t(k::COMMON_EXIT_DOCK), enabled: true }],
        ),
    ]
}

fn appearance(c: &Config, ctx: &Ctx<'_>) -> Vec<Row> {
    let colour = match preset_index(c.background) {
        Some(k) => preset_name(k).to_string(),
        None => t(k::SETTINGS_PAGES_CUSTOM).to_string(),
    };
    let swatch = preset_index(c.background).unwrap_or(BACKGROUND_PRESETS.len());
    let mut v = vec![
        Row { icon: None, label: String::new(), desc: None, control: Control::Preview },
        header(t(k::SETTINGS_PAGES_COLOUR), Some(colour)),
        row(
            glyphs::circle_shape,
            t(k::SETTINGS_PAGES_BACKGROUND),
            None,
            Control::Swatches { selected: swatch, picker_open: ctx.picker_open },
        ),
    ];
    if ctx.picker_open {
        v.push(Row { icon: None, label: String::new(), desc: None, control: Control::Picker });
    }
    v.push(slider_row(glyphs::drop_shape, t(k::SETTINGS_PAGES_OPACITY), c, Slider::Opacity));
    v.push(header(t(k::SETTINGS_PAGES_SIZES), None));
    v.push(slider_row(glyphs::square_shape, t(k::SETTINGS_PAGES_APP_ICONS), c, Slider::AppIcon));
    v.push(slider_row(glyphs::text_shape, t(k::SETTINGS_PAGES_APP_NAMES), c, Slider::AppText));
    v.push(slider_row(glyphs::ring_shape, t(k::SETTINGS_PAGES_TRAY_ICONS), c, Slider::TrayIcon));
    v.push(slider_row(glyphs::bars_shape, t(k::SETTINGS_PAGES_MODULES), c, Slider::Module));
    v.push(buttons(
        glyphs::refresh_shape,
        t(k::SETTINGS_PAGES_RESET_SIZES),
        None,
        vec![Button { action: Action::ResetSizes, text: t(k::SETTINGS_PAGES_RESET), enabled: !sizes_are_default(c) }],
    ));
    v.push(header(t(k::SETTINGS_PAGES_LANGUAGE), None));
    v.push(choice(glyphs::globe_shape, t(k::SETTINGS_PAGES_LANGUAGE), None, c, Setting::Language, ctx.screens));
    v
}

fn bar(c: &Config, ctx: &Ctx<'_>) -> Vec<Row> {
    let sc = ctx.screens;
    let mut v = vec![
        header(t(k::SETTINGS_PAGES_POSITION), None),
        choice(glyphs::bar_shape, t(k::SETTINGS_PAGES_POSITION), None, c, Setting::Position, sc),
        choice(glyphs::text_shape, t(k::SETTINGS_PAGES_DISPLAY_MODE), None, c, Setting::DisplayMode, sc),
        header(t(k::SETTINGS_PAGES_BEHAVIOUR), None),
        toggle(
            glyphs::chevron_down_shape,
            t(k::COMMON_AUTO_HIDE),
            Some(if ctx.keys_note {
                t(k::SETTINGS_PAGES_CONFIG_JSON_HAS_AN_UNREADABLE)
            } else {
                t(k::SETTINGS_PAGES_SHOWS_UP_WITH_THE_WIN)
            }),
            c,
            Setting::AutoHide,
        ),
        delay_row(
            t(k::SETTINGS_PAGES_SHOW_DELAY),
            t(k::SETTINGS_PAGES_POINTER_RESTING_AT_THE_SCREEN),
            c,
            Slider::ShowDelay,
        ),
        delay_row(
            t(k::SETTINGS_PAGES_HIDE_DELAY),
            t(k::SETTINGS_PAGES_AFTER_THE_POINTER_LEAVES_THE),
            c,
            Slider::HideDelay,
        ),
    ];
    if crate::ui::PUSH_AVAILABLE {
        v.push(choice(glyphs::chevron_down_shape, t(k::SETTINGS_PAGES_SHOW_AT_THE_EDGE), None, c, Setting::Reveal, sc));
    }
    v.extend([
        toggle(
            glyphs::eye_icon,
            t(k::SETTINGS_PAGES_WINDOW_PREVIEWS),
            Some(t(k::SETTINGS_PAGES_REST_THE_POINTER_ON_AN)),
            c,
            Setting::Previews,
        ),
        toggle(glyphs::clock_shape, t(k::SETTINGS_PAGES_CLOCK_SECONDS), None, c, Setting::Seconds),
        toggle(
            glyphs::speaker_icon,
            t(k::SETTINGS_PAGES_VOLUME_PERCENT),
            Some(t(k::SETTINGS_PAGES_NEXT_TO_THE_SPEAKER)),
            c,
            Setting::VolumePercent,
        ),
        toggle(
            glyphs::keycap_shape,
            t(k::SETTINGS_PAGES_KEYBOARD_SHORTCUTS),
            Some(t(k::SETTINGS_PAGES_WIN_1_9_OPEN_THE)),
            c,
            Setting::KeyboardChords,
        ),
    ]);
    v
}

/// A slider row with a description (the two delays of the Bar page).
fn delay_row(label: &str, desc: &str, c: &Config, slider: Slider) -> Row {
    let (min, max, unit) = slider_range(slider);
    row(
        glyphs::clock_shape,
        label,
        Some(desc),
        Control::Slider { slider, value: slider_value(c, slider), min, max, unit },
    )
}

fn start(c: &Config, ctx: &Ctx<'_>) -> Vec<Row> {
    let sc = ctx.screens;
    let idle_desc = t(k::SETTINGS_PAGES_REPLACES_THE_PINNED_APPS_OF);
    let desc = if ctx.import == start_import::Status::Idle { idle_desc.to_string() } else { import_label(ctx.import) };
    vec![
        header(t(k::SETTINGS_PAGES_START_MENU), None),
        toggle(glyphs::windows_logo_shape, t(k::SETTINGS_PAGES_START_BUTTON), None, c, Setting::Module("start")),
        choice(glyphs::grid_shape, t(k::SETTINGS_PAGES_START_LAYOUT), None, c, Setting::StartStyle, sc),
        toggle(
            glyphs::keycap_shape,
            t(k::SETTINGS_PAGES_WIN_KEY_OPENS_START),
            Some(t(k::SETTINGS_PAGES_TOGETHER_WITH_THE_BAR)),
            c,
            Setting::WinStart,
        ),
        header(t(k::SETTINGS_PAGES_PINNED_APPS), None),
        buttons(
            glyphs::download_shape,
            t(k::SETTINGS_PAGES_IMPORT_FROM_WINDOWS_START),
            Some(&desc),
            vec![Button {
                action: Action::ImportStart,
                text: t(k::SETTINGS_PAGES_IMPORT),
                enabled: ctx.import != start_import::Status::Running,
            }],
        ),
    ]
}

/// The first prefix of the config that names provider `name`, as the page shows it ("file " is "file" and a space).
fn prefix_phrase(c: &Config, name: &str) -> Option<String> {
    let p = c.launcher.prefixes.iter().find(|(_, n)| n.as_str() == name).map(|(p, _)| p.as_str())?;
    Some(if p.ends_with(' ') {
        format!("\"{}\" {}", p.trim_end(), t(k::SETTINGS_PAGES_AND_A_SPACE))
    } else {
        format!("\"{p}\"")
    })
}

/// One provider of "What it finds": name, glyph, label (EN, IT) and description (EN, IT). The description of a provider
/// with a prefix gets ". Prefix <p>" after it; Files has its own tail.
type ProviderText = (&'static str, Glyph, S, S);

fn provider_texts() -> [ProviderText; 10] {
    [
        ("apps", glyphs::grid_shape, k::SETTINGS_PAGES_PROVIDER_APPS, k::SETTINGS_PAGES_PROVIDER_APPS_DESC),
        ("windows", glyphs::window_shape, k::SETTINGS_PAGES_PROVIDER_WINDOWS, k::SETTINGS_PAGES_PROVIDER_WINDOWS_DESC),
        ("files", glyphs::folder_shape, k::SETTINGS_PAGES_PROVIDER_FILES, k::SETTINGS_PAGES_PROVIDER_FILES_DESC),
        ("settings", glyphs::gear_shape, k::SETTINGS_PAGES_PROVIDER_SETTINGS, k::SETTINGS_PAGES_PROVIDER_SETTINGS_DESC),
        ("calc", glyphs::equals_shape, k::SETTINGS_PAGES_PROVIDER_CALC, k::SETTINGS_PAGES_PROVIDER_CALC_DESC),
        (
            "commands",
            glyphs::power_shape,
            k::SETTINGS_PAGES_PROVIDER_COMMANDS,
            k::SETTINGS_PAGES_PROVIDER_COMMANDS_DESC,
        ),
        ("run", glyphs::prompt_shape, k::SETTINGS_PAGES_PROVIDER_RUN, k::SETTINGS_PAGES_PROVIDER_RUN_DESC),
        ("web", glyphs::globe_shape, k::SETTINGS_PAGES_PROVIDER_WEB, k::SETTINGS_PAGES_PROVIDER_WEB_DESC),
        (
            "datetime",
            glyphs::clock_shape,
            k::SETTINGS_PAGES_PROVIDER_DATETIME,
            k::SETTINGS_PAGES_PROVIDER_DATETIME_DESC,
        ),
        ("registry", glyphs::list_shape, k::SETTINGS_PAGES_PROVIDER_REGISTRY, k::SETTINGS_PAGES_PROVIDER_REGISTRY_DESC),
    ]
}

/// The toggle of one provider in "What it finds".
fn provider_row(c: &Config, (name, icon, label, desc): ProviderText) -> Row {
    let (label, base) = (t(label), t(desc));
    let desc = match (name, prefix_phrase(c, name)) {
        ("apps", _) | (_, None) => base.to_string(),
        ("files", Some(p)) => {
            format!("{base}; {} {p} {}", t(k::SETTINGS_PAGES_TYPE), t(k::SETTINGS_PAGES_FOR_FILES_ONLY))
        }
        (_, Some(p)) => format!("{base}. {} {p}", t(k::SETTINGS_PAGES_PREFIX)),
    };
    let setting = Setting::LauncherProvider(name);
    row(icon, label, Some(&desc), Control::Toggle { setting, on: is_on(c, setting), enabled: name != "apps" })
}

fn launcher(c: &Config, ctx: &Ctx<'_>) -> Vec<Row> {
    let sc = ctx.screens;
    let mut v = vec![
        header(t(k::COMMON_LAUNCHER), None),
        toggle(
            crate::popup::popup_startmenu::magnifier,
            t(k::COMMON_LAUNCHER),
            Some(t(k::SETTINGS_PAGES_FIND_AND_START_ANYTHING_FROM)),
            c,
            Setting::LauncherOn,
        ),
        choice(
            glyphs::keycap_shape,
            t(k::SETTINGS_PAGES_SHORTCUT),
            Some(t(k::SETTINGS_PAGES_WITH_ALT_SPACE_TAKEN_TYPE)),
            c,
            Setting::LauncherChord,
            sc,
        ),
    ];
    if let Some(shift) = ctx.chord_held {
        v.push(row(
            glyphs::info_shape,
            t(k::SETTINGS_PAGES_NOT_CHANGED),
            None,
            Control::Status(chord_held_text(shift).to_string()),
        ));
    }
    if ctx.powertoys && c.launcher.chord == "alt+space" {
        v.push(choice(
            glyphs::info_shape,
            "PowerToys",
            Some(t(k::SETTINGS_PAGES_COMMAND_PALETTE_ALSO_USES_ALT)),
            c,
            Setting::LauncherPowerToys,
            sc,
        ));
    }
    v.push(header(t(k::COMMON_WINDOW), None));
    v.push(choice(glyphs::monitor_shape, t(k::SETTINGS_PAGES_OPENS_ON), None, c, Setting::LauncherMonitor, sc));
    v.push(slider_row(glyphs::window_shape, t(k::SETTINGS_PAGES_WIDTH), c, Slider::LauncherWidth));
    v.push(slider_row(glyphs::list_shape, t(k::SETTINGS_PAGES_ROWS), c, Slider::LauncherRows));
    v.push(choice(
        glyphs::show_desktop_shape,
        t(k::SETTINGS_PAGES_OVER_FULL_SCREEN_APPS),
        Some(t(k::SETTINGS_PAGES_GAMES_KEEP_THEIR_KEYS_EXCLUSIVE)),
        c,
        Setting::LauncherFullscreen,
        sc,
    ));
    v.push(header(t(k::SETTINGS_PAGES_WHAT_IT_FINDS), None));
    for text in provider_texts() {
        v.push(provider_row(c, text));
    }
    v.push(header(t(k::SETTINGS_PAGES_WEB_SEARCH), None));
    let custom_desc = match crate::launcher::web::validate_template(&c.launcher.web_url) {
        Ok(tpl) if c.launcher.web_engine == "custom" => format!("{}: {}", t(k::SETTINGS_PAGES_CUSTOM), tpl.host()),
        _ => t(k::SETTINGS_PAGES_CUSTOM_IS_SET_IN_THE).to_string(),
    };
    v.push(row(
        glyphs::globe_shape,
        t(k::SETTINGS_PAGES_SEARCH_ENGINE),
        Some(&custom_desc),
        Control::Choice {
            setting: Setting::LauncherWeb,
            options: options(Setting::LauncherWeb, sc),
            selected: selected(c, Setting::LauncherWeb, sc),
        },
    ));
    v.push(header(t(k::SETTINGS_PAGES_HISTORY), None));
    v.push(toggle(
        glyphs::clock_shape,
        t(k::SETTINGS_PAGES_LEARN_FROM_WHAT_I_PICK),
        Some(t(k::SETTINGS_PAGES_KEPT_FOR_YOUR_ACCOUNT_ONLY)),
        c,
        Setting::LauncherLearn,
    ));
    v.push(buttons(
        glyphs::refresh_shape,
        t(k::SETTINGS_PAGES_FORGET_HISTORY),
        Some(t(k::SETTINGS_PAGES_THE_COMMANDS_YOU_RAN_AND)),
        vec![Button { action: Action::ForgetHistory, text: t(k::COMMON_FORGET), enabled: true }],
    ));
    v.push(header(t(k::SETTINGS_PAGES_ALIASES), None));
    v.extend(alias_rows(c));
    v.push(buttons(
        glyphs::text_shape,
        t(k::SETTINGS_PAGES_ADD_AN_ALIAS),
        Some(t(k::SETTINGS_PAGES_A_WORD_THAT_OPENS_AN)),
        // The prompt is the launcher's own field: it needs the launcher shortcut.
        vec![Button {
            action: Action::AddLauncherAlias,
            text: t(k::SETTINGS_PAGES_ADD),
            enabled: c.launcher.chord_spec().is_some(),
        }],
    ));
    v.push(header(t(k::SETTINGS_PAGES_PREFIXES), None));
    // The defaults in their table order, then the owner's own.
    let own = |k: &&str| !config::LAUNCHER_PREFIXES.iter().any(|(p, _)| p == k);
    let mut keys: Vec<&str> =
        config::LAUNCHER_PREFIXES.iter().map(|(p, _)| *p).filter(|p| c.launcher.prefixes.contains_key(*p)).collect();
    keys.extend(c.launcher.prefixes.keys().map(String::as_str).filter(own));
    let table = keys.iter().map(|p| p.trim_end()).collect::<Vec<_>>().join("  ");
    v.push(row(
        glyphs::text_shape,
        t(k::SETTINGS_PAGES_PREFIXES),
        Some(t(k::SETTINGS_PAGES_TYPED_FIRST_THEY_PICK_ONE)),
        Control::Status(table),
    ));
    v.push(buttons(
        glyphs::pencil_shape,
        t(k::SETTINGS_PAGES_CONFIG_FILE),
        Some(t(k::SETTINGS_PAGES_THIS_WINDOW_HAS_NO_TEXT)),
        vec![Button { action: Action::EditConfig, text: t(k::SETTINGS_PAGES_EDIT_CONFIG), enabled: true }],
    ));
    v
}

/// The glyph of an alias's target kind (an entry the launcher refuses shows the info mark).
fn alias_glyph(target: &str) -> Glyph {
    use crate::launcher::aliases::{classify, AliasTarget};
    match classify(target) {
        Ok(AliasTarget::App(_)) => glyphs::grid_shape,
        Ok(AliasTarget::Url(_)) => glyphs::globe_shape,
        Ok(AliasTarget::Settings(_)) => glyphs::gear_shape,
        Ok(AliasTarget::Path { dir: true, .. }) => glyphs::folder_shape,
        Ok(AliasTarget::Path { dir: false, .. }) => glyphs::document_shape,
        Ok(AliasTarget::Command(_)) => glyphs::prompt_shape,
        Err(_) => glyphs::info_shape,
    }
}

/// `s` cut to `max` characters with an ellipsis (a config value can be 1,024 characters long).
fn ellipsize(s: &str, max: usize) -> String {
    match s.char_indices().nth(max) {
        Some((i, _)) => format!("{}\u{2026}", s[..i].trim_end()),
        None => s.to_string(),
    }
}

/// One row per alias of `launcher.aliases`, sorted by word (the map's order), the ones the launcher ignores too, so
/// they can be removed. Remove carries the hash of the word.
fn alias_rows(c: &Config) -> Vec<Row> {
    let remove = t(k::SETTINGS_PAGES_REMOVE);
    c.launcher
        .aliases
        .iter()
        .map(|(word, target)| {
            let action = Action::RemoveLauncherAlias(crate::launcher::aliases::word_hash(word));
            buttons(
                alias_glyph(target),
                &ellipsize(word, 32),
                Some(&ellipsize(target, 60)),
                vec![Button { action, text: remove, enabled: true }],
            )
        })
        .collect()
}

fn screen(c: &Config, ctx: &Ctx<'_>) -> Vec<Row> {
    if ctx.screens.len() < 2 {
        return vec![Row {
            icon: None,
            label: t(k::SETTINGS_PAGES_THE_BAR_IS_ON_THE).to_string(),
            desc: None,
            control: Control::Note,
        }];
    }
    vec![
        header(t(k::COMMON_DISPLAY), None),
        choice(
            glyphs::monitor_shape,
            t(k::COMMON_DISPLAY),
            Some(t(k::SETTINGS_PAGES_POINTER_THE_WIN_KEY_BRINGS)),
            c,
            Setting::Monitor,
            ctx.screens,
        ),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::update::{Outcome, Phase};
    use crate::update_release::Version;

    fn idle() -> UpdateStatus {
        UpdateStatus {
            installed: Version { major: 1, minor: 4, patch: 0 },
            phase: Phase::Idle,
            available: None,
            last_check: String::new(),
            outcome: Outcome::None,
            staged: false,
        }
    }

    fn ctx<'a>(screens: &'a [Screen], st: &'a UpdateStatus) -> Ctx<'a> {
        Ctx {
            screens,
            picker_open: false,
            chooser_open: false,
            tray: &[],
            import: start_import::Status::Idle,
            update: st,
            memory: Memory::default(),
            data_dir: None,
            log_path: None,
            trust_elevated: false,
            powertoys: false,
            chord_held: None,
            battery_note: false,
            keys_note: false,
        }
    }

    fn test_screens() -> Vec<Screen> {
        let d = |device: &str, label, primary| Screen { device: device.into(), label, primary };
        vec![d("\\\\.\\DISPLAY1", "1", true), d("\\\\.\\DISPLAY2", "2", false), d("\\\\.\\DISPLAY3", "3", false)]
    }

    #[test]
    fn choices_roundtrip() {
        let mut c = Config::default();
        let sc = test_screens();
        for set in [
            Setting::Language,
            Setting::StartStyle,
            Setting::Position,
            Setting::Monitor,
            Setting::DisplayMode,
            Setting::BatteryShow,
        ] {
            for j in 0..options(set, &sc).len() {
                apply_edit(&mut c, Edit::Choose(set, j), &sc);
                assert_eq!(selected(&c, set, &sc), Some(j));
            }
        }
    }

    #[test]
    fn screen_choice() {
        let sc = test_screens();
        assert_eq!(options(Setting::Monitor, &sc), ["Main", "2", "3", "Pointer"]);
        // "Pointer" is stored as `MONITOR_POINTER` and selected back.
        let mut p = Config::default();
        choose(&mut p, Setting::Monitor, 3, &sc);
        assert_eq!(p.monitor, config::MONITOR_POINTER);
        assert_eq!(selected(&p, Setting::Monitor, &sc), Some(3));
        let mut c = Config::default();
        assert_eq!(selected(&c, Setting::Monitor, &sc), Some(0));
        choose(&mut c, Setting::Monitor, 2, &sc);
        assert_eq!(c.monitor, "\\\\.\\DISPLAY3");
        // The primary is stored as "" (follow the primary monitor).
        choose(&mut c, Setting::Monitor, 0, &sc);
        assert!(c.monitor.is_empty());
        // A chosen display that is not connected: the bar is on the primary, "Main" is shown.
        c.monitor = "\\\\.\\DISPLAY9".into();
        assert_eq!(selected(&c, Setting::Monitor, &sc), Some(0));
    }

    #[test]
    fn toggles_flip() {
        let mut c = Config::default();
        for set in [
            Setting::AutoHide,
            Setting::Previews,
            Setting::WinStart,
            Setting::Seconds,
            Setting::VolumePercent,
            Setting::BatteryPercent,
            Setting::BatteryInside,
            Setting::UpdateCheck,
            Setting::UpdateAutoInstall,
            Setting::TrayShowAll,
            Setting::Module("metrics"),
            Setting::Module("tray"),
        ] {
            let was = is_on(&c, set);
            apply_edit(&mut c, Edit::Flip(set), &[]);
            assert_eq!(is_on(&c, set), !was, "{set:?}");
        }
    }

    #[test]
    fn sliders_clamp_and_reset() {
        let mut c = Config::default();
        for sl in [Slider::Opacity, Slider::AppIcon, Slider::AppText, Slider::TrayIcon, Slider::Module] {
            let (lo, hi, _) = slider_range(sl);
            apply_edit(&mut c, Edit::Slider(sl, hi + 50), &[]);
            assert_eq!(slider_value(&c, sl), hi);
            apply_edit(&mut c, Edit::Slider(sl, lo - 50), &[]);
            assert_eq!(slider_value(&c, sl), lo);
        }
        assert_eq!(c.bar_opacity, 0);
        assert!(!sizes_are_default(&c));
        apply_edit(&mut c, Edit::ResetSizes, &[]);
        assert!(sizes_are_default(&c));
    }

    #[test]
    fn hsv_roundtrip() {
        for (_, bg) in BACKGROUND_PRESETS {
            assert_eq!(background_of(rgb_to_hsv(bg.rgb())), bg);
        }
        for rgb in [0xFF0000u32, 0x00FF00, 0x0000FF, 0x123456, 0xFFFFFF, 0x808080] {
            let bg = Background(rgb);
            assert_eq!(background_of(rgb_to_hsv(bg.rgb())), bg);
        }
        assert_eq!(hsv_to_rgb(359.9, 1.0, 1.0).0, 255);
    }

    #[test]
    fn every_preset_has_an_italian_name() {
        assert_eq!(BACKGROUND_PRESETS.len(), 7, "update the names in preset_name");
        // The names as the code held them before the keyed strings: English from the table, Italian beside it.
        const OLD_IT: [&str; 7] = ["Nero", "Grafite", "Ardesia", "Blu notte", "Prugna", "Vinaccia", "Verde bosco"];
        for (i, (en, _)) in BACKGROUND_PRESETS.iter().enumerate() {
            assert_eq!(crate::i18n::with_italian(false, || preset_name(i)), *en);
            assert_eq!(crate::i18n::with_italian(true, || preset_name(i)), OLD_IT[i]);
        }
        assert_eq!(preset_name(BACKGROUND_PRESETS.len()), "");
    }

    #[test]
    fn swatch_selection() {
        assert!(swatch_selected(Background::BLACK, 0));
        assert!(swatch_selected(Background::DARK, 1));
        assert!(swatch_selected(Background(0x123456), BACKGROUND_PRESETS.len()));
        assert!(!swatch_selected(Background(0x123456), 0));
    }

    #[test]
    fn import_labels_cover_every_status() {
        for st in [
            start_import::Status::Idle,
            start_import::Status::Running,
            start_import::Status::Failed(FailReason::Export),
            start_import::Status::TimedOut,
            start_import::Status::Imported(3),
        ] {
            assert!(!import_label(st).is_empty());
        }
        assert_eq!(import_label(start_import::Status::Imported(1)), "Imported 1 app");
    }

    #[test]
    fn imported_count_equals_the_old_text_in_both_languages() {
        // The old code: Italian always "Importate {n} app"; English "Imported 1 app" for one, else "Imported {n} apps".
        for n in [0u32, 1, 2, 17] {
            let old_en = if n == 1 { "Imported 1 app".to_string() } else { format!("Imported {n} apps") };
            let old_it = format!("Importate {n} app");
            let en = crate::i18n::with_italian(false, || import_label(start_import::Status::Imported(n)));
            let it = crate::i18n::with_italian(true, || import_label(start_import::Status::Imported(n)));
            assert_eq!((en, it), (old_en, old_it), "n = {n}");
        }
    }

    #[test]
    fn battery_row_shows_the_note() {
        let st = idle();
        let c = Config::default();
        let desc = |note: bool| {
            let mut cx = ctx(&[], &st);
            cx.battery_note = note;
            let r = rows(Page::Modules, &c, &cx);
            r.iter()
                .find(|r| matches!(r.control, Control::Choice { setting: Setting::BatteryShow, .. }))
                .and_then(|r| r.desc.clone())
                .unwrap()
        };
        assert_eq!(desc(false), "Shown automatically on laptops");
        assert_eq!(desc(true), "config.json has an unknown value here: Auto is used");
        assert_eq!(
            crate::i18n::with_italian(true, || desc(true)),
            "config.json ha un valore sconosciuto qui: si usa Automatica"
        );
    }

    #[test]
    fn bar_page_shows_the_new_keys_note() {
        let st = idle();
        let c = Config::default();
        let desc = |note: bool| {
            let mut cx = ctx(&[], &st);
            cx.keys_note = note;
            let r = rows(Page::Bar, &c, &cx);
            r.iter()
                .find(|r| matches!(r.control, Control::Toggle { setting: Setting::AutoHide, .. }))
                .and_then(|r| r.desc.clone())
                .unwrap()
        };
        assert_ne!(desc(true), desc(false));
        assert_eq!(desc(true), "config.json has an unreadable value in a new key: its default is used");
        assert_eq!(
            crate::i18n::with_italian(true, || desc(true)),
            "config.json ha un valore illeggibile in una chiave nuova: si usa quello predefinito"
        );
    }

    #[test]
    fn failed_import_names_its_reason() {
        let mut seen = std::collections::HashSet::new();
        for why in FailReason::ALL {
            let en = import_label(start_import::Status::Failed(why));
            assert!(en.starts_with("Import failed: "), "{en}");
            assert!(seen.insert(en), "one text per reason");
        }
        let mut seen_it = std::collections::HashSet::new();
        for why in FailReason::ALL {
            let it = crate::i18n::with_italian(true, || import_label(start_import::Status::Failed(why)));
            assert!(it.starts_with("Importazione non riuscita: "), "{it}");
            assert!(seen_it.insert(it), "one Italian text per reason");
        }
    }

    #[test]
    fn every_page_has_a_title_slug_and_icon() {
        for p in Page::ALL {
            assert!(!p.title().is_empty() && !p.subtitle().is_empty() && !p.slug().is_empty());
        }
        assert_eq!(Page::ALL.iter().filter(|p| p.at_bottom()).count(), 2);
    }

    #[test]
    fn appearance_rows_follow_the_config() {
        let st = idle();
        let mut c = Config::default();
        let r = rows(Page::Appearance, &c, &ctx(&[], &st));
        assert!(matches!(r[0].control, Control::Preview));
        assert_eq!(r.iter().filter(|r| matches!(r.control, Control::Header { .. })).count(), 3);
        assert!(r.iter().any(|r| r.control == Control::Swatches { selected: 0, picker_open: false }));
        assert!(!r.iter().any(|r| r.control == Control::Picker));
        let reset = r
            .iter()
            .find_map(|r| match &r.control {
                Control::Buttons(b) => Some(b[0].clone()),
                _ => None,
            })
            .unwrap();
        assert_eq!((reset.action, reset.enabled), (Action::ResetSizes, false));
        c.app_icon_size = 24;
        c.background = Background(0x123456);
        let mut cx = ctx(&[], &st);
        cx.picker_open = true;
        let r = rows(Page::Appearance, &c, &cx);
        let k =
            r.iter().position(|r| matches!(r.control, Control::Swatches { selected: 7, picker_open: true })).unwrap();
        assert_eq!(r[k + 1].control, Control::Picker);
        assert!(r.iter().any(|r| matches!(&r.control, Control::Buttons(b) if b[0].enabled)));
    }

    #[test]
    fn bar_and_start_rows() {
        let st = idle();
        let c = Config::default();
        let bar = rows(Page::Bar, &c, &ctx(&[], &st));
        for s in [Setting::AutoHide, Setting::Previews, Setting::Seconds, Setting::VolumePercent] {
            assert!(bar.iter().any(|r| matches!(r.control, Control::Toggle { setting, .. } if setting == s)), "{s:?}");
        }
        assert!(bar
            .iter()
            .any(|r| matches!(r.control, Control::Choice { setting: Setting::DisplayMode, selected: Some(0), .. })));
        let mut cx = ctx(&[], &st);
        let start = rows(Page::Start, &c, &cx);
        assert!(start
            .iter()
            .any(|r| matches!(r.control, Control::Toggle { setting: Setting::Module("start"), on: true, .. })));
        assert!(start.iter().any(|r| matches!(r.control, Control::Choice { setting: Setting::StartStyle, .. })));
        cx.import = start_import::Status::Running;
        let busy = rows(Page::Start, &c, &cx);
        let imp = busy
            .iter()
            .find(|r| matches!(&r.control, Control::Buttons(b) if b[0].action == Action::ImportStart))
            .unwrap();
        assert!(matches!(&imp.control, Control::Buttons(b) if !b[0].enabled));
        assert_eq!(imp.desc.as_deref(), Some(import_label(start_import::Status::Running).as_str()));
    }

    #[test]
    fn screen_page_offers_the_choice_only_with_two_displays() {
        let st = idle();
        let c = Config::default();
        let sc = test_screens();
        let one = rows(Page::Screen, &c, &ctx(&sc[..1], &st));
        assert_eq!(one.len(), 1);
        assert_eq!(one[0].control, Control::Note);
        let many = rows(Page::Screen, &c, &ctx(&sc, &st));
        assert!(many.iter().any(|r| matches!(&r.control,
            Control::Choice { setting: Setting::Monitor, options, selected: Some(0) } if options.len() == 4)));
    }

    #[test]
    fn click_maps_controls_to_effects() {
        let st = idle();
        let cx = ctx(&[], &st);
        let row = |control| Row { icon: None, label: String::new(), desc: None, control };
        let ch = row(Control::Choice { setting: Setting::Position, options: vec!["a", "b"], selected: Some(1) });
        assert_eq!(click(&ch, Some(1), &cx), Effect::None);
        assert_eq!(click(&ch, Some(0), &cx), Effect::Save(Edit::Choose(Setting::Position, 0)));
        assert_eq!(click(&ch, Some(5), &cx), Effect::None);
        let t = row(Control::Toggle { setting: Setting::AutoHide, on: true, enabled: true });
        assert_eq!(click(&t, None, &cx), Effect::Save(Edit::Flip(Setting::AutoHide)));
        let off = row(Control::Toggle { setting: Setting::BatteryInside, on: false, enabled: false });
        assert_eq!(click(&off, None, &cx), Effect::None);
        let sw = row(Control::Swatches { selected: 0, picker_open: false });
        assert_eq!(click(&sw, Some(7), &cx), Effect::TogglePicker);
        assert_eq!(click(&sw, Some(0), &cx), Effect::None);
        assert_eq!(click(&sw, Some(1), &cx), Effect::Save(Edit::Background(BACKGROUND_PRESETS[1].1)));
        let b = row(Control::Buttons(vec![
            Button { action: Action::ResetSizes, text: "Reset", enabled: true },
            Button { action: Action::OpenLog, text: "Open", enabled: false },
        ]));
        assert_eq!(click(&b, Some(0), &cx), Effect::Save(Edit::ResetSizes));
        assert_eq!(click(&b, Some(1), &cx), Effect::None);
        assert_eq!(click(&b, Some(2), &cx), Effect::None);
    }

    fn tray_rows() -> Vec<TrayRow> {
        let r = |key: &str, name: &str, visible, in_bar| TrayRow {
            key: key.into(),
            name: name.into(),
            bitmap: None,
            visible,
            in_bar,
        };
        vec![
            r("dropbox.exe#1", "Dropbox", true, true),
            r("slack.exe#1", "Slack", true, false),
            r("teams.exe#1", "Teams", false, false),
        ]
    }

    #[test]
    fn bar_page_has_the_timing_rows() {
        let st = idle();
        let c = Config::default();
        let r = rows(Page::Bar, &c, &ctx(&[], &st));
        let k = r.iter().position(|r| matches!(r.control, Control::Toggle { setting: Setting::AutoHide, .. })).unwrap();
        assert!(matches!(
            r[k + 1].control,
            Control::Slider { slider: Slider::ShowDelay, value: 150, min: 0, max: 1000, .. }
        ));
        assert!(matches!(
            r[k + 2].control,
            Control::Slider { slider: Slider::HideDelay, value: 2500, min: 0, max: 5000, .. }
        ));
        assert!(r[k + 1].desc.is_some() && r[k + 2].desc.is_some());
        assert_eq!(
            r.iter().any(|r| matches!(r.control, Control::Choice { setting: Setting::Reveal, .. })),
            crate::ui::PUSH_AVAILABLE
        );
        assert!(r
            .iter()
            .any(|r| matches!(r.control, Control::Toggle { setting: Setting::KeyboardChords, on: true, .. })));
    }

    #[test]
    fn delay_sliders_snap_and_clamp() {
        let mut c = Config::default();
        set_slider(&mut c, Slider::ShowDelay, 1234);
        assert_eq!(c.show_delay_ms, 1000);
        set_slider(&mut c, Slider::ShowDelay, 73);
        assert_eq!(c.show_delay_ms, 50);
        set_slider(&mut c, Slider::HideDelay, 1234);
        assert_eq!(c.hide_delay_ms, 1200);
        set_slider(&mut c, Slider::HideDelay, 99999);
        assert_eq!(c.hide_delay_ms, 5000);
        set_slider(&mut c, Slider::HideDelay, -40);
        assert_eq!(c.hide_delay_ms, 0);
        assert_eq!(slider_value(&c, Slider::HideDelay), 0);
    }

    #[test]
    fn reveal_choice_round_trips() {
        let mut c = Config::default();
        assert_eq!(selected(&c, Setting::Reveal, &[]), Some(0));
        choose(&mut c, Setting::Reveal, 1, &[]);
        assert_eq!(c.reveal, config::Reveal::Push);
        assert_eq!(selected(&c, Setting::Reveal, &[]), Some(1));
        choose(&mut c, Setting::Reveal, 0, &[]);
        assert_eq!(c.reveal, config::Reveal::Dwell);
    }

    #[test]
    fn keyboard_toggle_follows_the_defaults() {
        let mut c = Config::default();
        assert!(is_on(&c, Setting::KeyboardChords));
        flip(&mut c, Setting::KeyboardChords);
        assert_eq!(c.keyboard, config::KeyboardConfig { win_numbers: false, win_t: false, win_b: false, peek: false });
        assert!(!is_on(&c, Setting::KeyboardChords));
        flip(&mut c, Setting::KeyboardChords);
        assert_eq!(c.keyboard, config::KeyboardConfig::default());
        // One flag left on: the switch reads on, and a click turns the rest off.
        c.keyboard = config::KeyboardConfig { win_numbers: false, win_t: false, win_b: true, peek: false };
        assert!(is_on(&c, Setting::KeyboardChords));
        flip(&mut c, Setting::KeyboardChords);
        assert!(!is_on(&c, Setting::KeyboardChords));
    }

    #[test]
    fn modules_page_lists_toggles_battery_and_tray() {
        let st = idle();
        let mut c = Config::default();
        let r = rows(Page::Modules, &c, &ctx(&[], &st));
        let names: Vec<&str> = r
            .iter()
            .filter_map(|r| match r.control {
                Control::Toggle { setting: Setting::Module(n), .. } => Some(n),
                _ => None,
            })
            .collect();
        assert_eq!(names, ["notifications", "volume", "network", "metrics", "clock", "show_desktop", "tray"]);
        let bat =
            r.iter().find(|r| matches!(r.control, Control::Choice { setting: Setting::BatteryShow, .. })).unwrap();
        assert!(matches!(&bat.control, Control::Choice { options, selected: Some(0), .. } if options.len() == 3));
        assert!(bat.desc.is_some());
        assert!(r
            .iter()
            .any(|r| r.control == Control::Toggle { setting: Setting::BatteryInside, on: false, enabled: true }));
        assert!(r.iter().any(|r| r.control == Control::Chooser { open: false }));
        c.battery_percent = false;
        c.set_module_visible("tray", false);
        let r = rows(Page::Modules, &c, &ctx(&[], &st));
        assert!(r
            .iter()
            .any(|r| r.control == Control::Toggle { setting: Setting::BatteryInside, on: false, enabled: false }));
        assert!(!r.iter().any(|r| matches!(r.control, Control::Chooser { .. })));
    }

    #[test]
    fn elevated_tray_toggle_reflects_and_flips_the_hklm_switch() {
        let st = idle();
        let mut c = Config::default();
        let find = |r: &[Row]| {
            r.iter().position(|r| matches!(r.control, Control::Toggle { setting: Setting::TrustElevatedTray, .. }))
        };
        for on in [false, true] {
            let mut cx = ctx(&[], &st);
            cx.trust_elevated = on;
            let r = rows(Page::Modules, &c, &cx);
            let k = find(&r).unwrap();
            assert_eq!(r[k].control, Control::Toggle { setting: Setting::TrustElevatedTray, on, enabled: true });
            assert!(r[k].desc.is_some());
            // In the Tray section, between the module toggle and the "Show all" switch above the chooser.
            assert_eq!(r[k - 1].control, Control::Toggle { setting: Setting::Module("tray"), on: true, enabled: true });
            assert_eq!(r[k + 1].control, Control::Toggle { setting: Setting::TrayShowAll, on: false, enabled: true });
            assert_eq!(r[k + 2].control, Control::Chooser { open: false });
            // The click asks for the HKLM write, never a config edit.
            assert_eq!(click(&r[k], None, &cx), Effect::TrustElevated(!on));
        }
        // The config never holds it: flipping the setting does nothing.
        let before = c.clone();
        flip(&mut c, Setting::TrustElevatedTray);
        assert!(!is_on(&c, Setting::TrustElevatedTray));
        assert_eq!(format!("{before:?}"), format!("{c:?}"));
        // Hidden with the tray module.
        c.set_module_visible("tray", false);
        assert!(find(&rows(Page::Modules, &c, &ctx(&[], &st))).is_none());
    }

    #[test]
    fn battery_choice_roundtrip() {
        let mut c = Config::default();
        for j in 0..3 {
            apply_edit(&mut c, Edit::Choose(Setting::BatteryShow, j), &[]);
            assert_eq!(selected(&c, Setting::BatteryShow, &[]), Some(j));
        }
    }

    #[test]
    fn show_all_switch_precedes_the_list_and_disables_the_rows() {
        let st = idle();
        let tray = tray_rows();
        let mut cx = ctx(&[], &st);
        cx.tray = &tray;
        cx.chooser_open = true;
        let mut c = Config::default();
        let r = rows(Page::Modules, &c, &cx);
        let sw =
            r.iter().position(|r| matches!(r.control, Control::Toggle { setting: Setting::TrayShowAll, .. })).unwrap();
        let ch = r.iter().position(|r| matches!(r.control, Control::Chooser { .. })).unwrap();
        assert!(sw < ch && r[sw].desc.is_some());
        assert_eq!(click(&r[sw], None, &cx), Effect::Save(Edit::Flip(Setting::TrayShowAll)));
        // Teams is hidden; while Show all is on it reads as on, nothing can be clicked, and tray_hidden stays.
        c.tray_hidden = vec!["teams.exe#1".into()];
        apply_edit(&mut c, Edit::Flip(Setting::TrayShowAll), &[]);
        assert!(c.tray_show_all);
        let r = rows(Page::Modules, &c, &cx);
        let k = r.iter().position(|r| matches!(r.control, Control::Chooser { .. })).unwrap();
        assert_eq!(r[k + 3].control, Control::TrayApp { key: "teams.exe#1".into(), visible: true, enabled: false });
        assert_eq!(click(&r[k + 3], None, &cx), Effect::None);
        assert_eq!(click(&r[k + 1], None, &cx), Effect::None);
        apply_edit(&mut c, Edit::Flip(Setting::TrayShowAll), &[]);
        assert_eq!(c.tray_hidden, vec!["teams.exe#1".to_string()]);
    }

    #[test]
    fn tray_chooser_rows_are_inline() {
        let st = idle();
        let tray = tray_rows();
        let mut cx = ctx(&[], &st);
        cx.tray = &tray;
        cx.chooser_open = true;
        let r = rows(Page::Modules, &Config::default(), &cx);
        let k = r.iter().position(|r| r.control == Control::Chooser { open: true }).unwrap();
        assert_eq!(r[k + 1].control, Control::TrayApp { key: "dropbox.exe#1".into(), visible: true, enabled: true });
        assert_eq!(r[k + 2].label, "Slack");
        assert!(r[k + 2].desc.is_some()); // visible but no room in the bar
        assert_eq!(click(&r[k], None, &cx), Effect::ToggleChooser);
        assert_eq!(click(&r[k + 2], None, &cx), Effect::TrayVisible { key: "slack.exe#1".into(), hidden: true });
        // Slack (the middle app) exits while the rows on screen are still [Dropbox, Slack, Teams].
        let mid: Vec<TrayRow> = tray_rows().into_iter().filter(|t| t.name != "Slack").collect();
        let mut cx2 = ctx(&[], &st);
        cx2.tray = &mid;
        assert_eq!(click(&r[k + 2], None, &cx2), Effect::None); // never Teams, its new neighbour
        assert_eq!(click(&r[k + 3], None, &cx2), Effect::TrayVisible { key: "teams.exe#1".into(), hidden: false });
        // The tail shrank: the last row acts on nothing.
        cx2.tray = &tray[..2];
        assert_eq!(click(&r[k + 3], None, &cx2), Effect::None);
        cx2.tray = &[];
        cx2.chooser_open = true;
        let empty = rows(Page::Modules, &Config::default(), &cx2);
        assert_eq!(empty.last().unwrap().control, Control::Note);
    }

    fn release(tag: &str) -> crate::update::Available {
        crate::update::Available {
            tag: tag.into(),
            version: Version { major: 1, minor: 4, patch: 1 },
            date: "2026-11-20".into(),
            notes: String::new(),
        }
    }

    #[test]
    fn updates_page_follows_the_status() {
        let buttons = |r: &[Row]| {
            r.iter()
                .find_map(|r| match &r.control {
                    Control::Buttons(b) if b.len() == 3 => Some(b.clone()),
                    _ => None,
                })
                .unwrap()
        };
        let mut st = idle();
        let c = Config::default();
        let r = rows(Page::Updates, &c, &ctx(&[], &st));
        assert_eq!(
            buttons(&r).iter().map(|b| (b.action, b.enabled)).collect::<Vec<_>>(),
            [(Action::CheckUpdates, true), (Action::ReleaseNotes, false), (Action::InstallUpdate, false)]
        );
        assert!(r.iter().any(|r| r.control == Control::Status("1.4.0".into())));
        assert!(r.iter().any(|r| r.control == Control::Status("Not checked yet".into())));
        assert!(r.iter().any(|r| r.control == Control::Status("Never".into())));
        assert!(r.iter().any(|r| r.control == Control::Status("None yet".into())));
        st.available = Some(release("v1.4.1"));
        st.outcome = Outcome::Available;
        st.last_check = "2026-11-20T10:00:00Z".into();
        let r = rows(Page::Updates, &c, &ctx(&[], &st));
        assert!(r
            .iter()
            .any(|r| matches!(&r.control, Control::Status(s) if s.contains("v1.4.1") && s.contains("2026-11-20"))));
        assert!(buttons(&r).iter().all(|b| b.enabled));
        // A running operation disables Check and Update and shows its line.
        st.phase = Phase::Checking;
        let r = rows(Page::Updates, &c, &ctx(&[], &st));
        assert!(!buttons(&r)[0].enabled && !buttons(&r)[2].enabled);
        assert!(r.iter().any(|r| r.control == Control::Note && r.label == st.line()));
        // The Last action row keeps the previous outcome while the phase runs.
        let kept = UpdateStatus { phase: Phase::Idle, ..st.clone() }.line();
        assert_ne!(kept, st.line());
        assert!(r.iter().any(|r| r.control == Control::Status(kept.clone())));
        // Back to Idle after a failure: no note, the outcome stays as the last action.
        st.phase = Phase::Idle;
        st.outcome = Outcome::CouldNotCheck(crate::update::CheckFail::Http(403));
        let r = rows(Page::Updates, &c, &ctx(&[], &st));
        assert!(buttons(&r)[0].enabled);
        assert!(!r.iter().any(|r| r.control == Control::Note));
        assert!(r.iter().any(|r| r.control == Control::Status(st.line())));
        // A first check that failed leaves last_check empty: still "Not checked yet".
        st.available = None;
        st.last_check = String::new();
        st.outcome = Outcome::CouldNotCheck(crate::update::CheckFail::NoConnection);
        let r = rows(Page::Updates, &c, &ctx(&[], &st));
        assert!(r.iter().any(|r| r.control == Control::Status("Not checked yet".into())));
        assert!(!r.iter().any(|r| r.control == Control::Status("Up to date".into())));
        assert!(r
            .iter()
            .any(|r| r.control == Control::Toggle { setting: Setting::UpdateCheck, on: true, enabled: true }));
        assert!(r
            .iter()
            .any(|r| r.control == Control::Toggle { setting: Setting::UpdateAutoInstall, on: false, enabled: true }));
        // Checked, nothing newer.
        st.available = None;
        st.outcome = Outcome::UpToDate;
        let r = rows(Page::Updates, &c, &ctx(&[], &st));
        assert!(r.iter().any(|r| r.control == Control::Status("Up to date".into())));
    }

    #[test]
    fn exit_label_matches_the_power_menu() {
        let st = idle();
        let r = rows(Page::About, &Config::default(), &ctx(&[], &st));
        let text = r
            .iter()
            .find_map(|r| match &r.control {
                Control::Buttons(b) if b[0].action == Action::ExitDock => Some(b[0].text),
                _ => None,
            })
            .unwrap();
        assert_eq!(text, crate::popup_power::label(crate::popup_power::ID_EXIT));
    }

    #[test]
    fn about_page_rows() {
        let st = idle();
        let mut cx = ctx(&[], &st);
        cx.memory = Memory { now: Some(10_276_044), trimmed: None };
        let r = rows(Page::About, &Config::default(), &cx);
        assert!(r.iter().any(|r| r.label.contains(env!("CARGO_PKG_VERSION"))));
        assert!(r.iter().any(|r| r.control == Control::Status("9.8 MB".into())));
        let open = r.iter().find_map(|r| match &r.control {
            Control::Buttons(b) if b[0].action == Action::OpenDataFolder => Some(b[0].enabled),
            _ => None,
        });
        assert_eq!(open, Some(false));
        assert!(r
            .iter()
            .any(|r| matches!(&r.control, Control::Buttons(b) if b[0].action == Action::ExitDock && b[0].enabled)));
    }

    fn launcher_rows(c: &Config, powertoys: bool) -> Vec<Row> {
        let st = idle();
        let mut cx = ctx(&[], &st);
        cx.powertoys = powertoys;
        rows(Page::Launcher, c, &cx)
    }

    fn alias_config() -> Config {
        let mut c = Config::default();
        c.launcher.chord = "alt+space".into();
        for (w, t) in [
            ("web", "https://example.com/"),
            ("calc", "app:Contoso.Calc_1234!App"),
            ("proj", "D:\\Projects"),
            ("steam", "steam://run/1"),
        ] {
            c.launcher.aliases.insert(w.into(), t.into());
        }
        c
    }

    fn alias_buttons(r: &[Row]) -> Vec<(String, Option<String>, Action, bool)> {
        r.iter()
            .filter_map(|x| match &x.control {
                Control::Buttons(b) if matches!(b[0].action, Action::RemoveLauncherAlias(_)) => {
                    Some((x.label.clone(), x.desc.clone(), b[0].action, b[0].enabled))
                }
                _ => None,
            })
            .collect()
    }

    #[test]
    fn launcher_page_lists_aliases() {
        use crate::launcher::aliases::word_hash;
        let c = alias_config();
        let r = launcher_rows(&c, false);
        // The header comes before the rows, the Add row after them, the prefixes after that.
        let pos = |f: &dyn Fn(&Row) -> bool| r.iter().position(f).unwrap();
        let head = pos(&|x| matches!(x.control, Control::Header { .. }) && x.label == "Aliases");
        let add = pos(&|x| x.label == "Add an alias");
        let prefixes = pos(&|x| matches!(x.control, Control::Header { .. }) && x.label == "Prefixes");
        assert!(head < add && add < prefixes);
        // One row per entry in word order (the ignored "steam:" one too), the word as title, the target as description.
        let list = alias_buttons(&r);
        let words: Vec<&str> = list.iter().map(|l| l.0.as_str()).collect();
        assert_eq!(words, ["calc", "proj", "steam", "web"]);
        assert_eq!(list[3].1.as_deref(), Some("https://example.com/"));
        for (w, _, a, enabled) in &list {
            assert_eq!(*a, Action::RemoveLauncherAlias(word_hash(w)));
            assert!(enabled);
        }
        assert!((head + 1..add).all(|i| matches!(&r[i].control, Control::Buttons(_))));
        // The kinds have their own glyphs, a refused target the info mark.
        let is = |t: &str, f: Glyph| std::ptr::fn_addr_eq(alias_glyph(t), f);
        assert!(is("https://example.com/", glyphs::globe_shape));
        assert!(is("app:Contoso.Calc_1234!App", glyphs::grid_shape));
        assert!(is("ms-settings:display", glyphs::gear_shape));
        assert!(is("C:\\Windows", glyphs::folder_shape));
        assert!(is("C:\\Windows\\win.ini", glyphs::document_shape));
        assert!(is("steam://run/1", glyphs::info_shape));
        // A long target is cut, a long word too.
        let mut long = Config::default();
        long.launcher.aliases.insert("x".repeat(100), "y".repeat(900));
        let l = alias_buttons(&launcher_rows(&long, false));
        assert_eq!(l[0].0.chars().count(), 33);
        assert_eq!(l[0].1.as_ref().map(|d| d.chars().count()), Some(61));
        // Italian texts.
        let r = crate::i18n::with_italian(true, || launcher_rows(&alias_config(), false));
        assert!(r.iter().any(|x| x.label == "Aggiungi un alias"));
        assert!(r.iter().any(|x| matches!(&x.control, Control::Buttons(b) if b[0].text == "Rimuovi")));
    }

    #[test]
    fn remove_alias_by_hash_removes_only_that_one() {
        use crate::launcher::aliases::word_hash;
        let mut c = alias_config();
        let r = launcher_rows(&c, false);
        let row = r.iter().find(|x| x.label == "proj").unwrap();
        let e = click(row, Some(0), &ctx(&[], &idle()));
        assert_eq!(e, Effect::Save(Edit::RemoveLauncherAlias(word_hash("proj"))));
        let Effect::Save(edit) = e else { unreachable!() };
        apply_edit(&mut c, edit, &[]);
        let left: Vec<&str> = c.launcher.aliases.keys().map(String::as_str).collect();
        assert_eq!(left, ["calc", "steam", "web"]);
        // A refused entry is removable too, and nothing else of the config moves.
        apply_edit(&mut c, Edit::RemoveLauncherAlias(word_hash("steam")), &[]);
        assert_eq!(c.launcher.aliases.len(), 2);
        assert_eq!(c.launcher.chord, "alt+space");
    }

    #[test]
    fn remove_alias_unknown_hash_is_noop() {
        use crate::launcher::aliases::word_hash;
        let mut c = alias_config();
        let before = c.clone();
        apply_edit(&mut c, Edit::RemoveLauncherAlias(word_hash("gone")), &[]);
        apply_edit(&mut c, Edit::RemoveLauncherAlias(0), &[]);
        assert_eq!(c, before);
        // An empty list gives no alias rows, only the Add row.
        let r = launcher_rows(&Config::default(), false);
        assert!(alias_buttons(&r).is_empty());
        assert!(r.iter().any(|x| x.label == "Add an alias"));
    }

    #[test]
    fn add_alias_disabled_with_the_chord_off() {
        let add = |c: &Config| {
            launcher_rows(c, false).into_iter().find_map(|x| match x.control {
                Control::Buttons(b) if b[0].action == Action::AddLauncherAlias => Some((b[0].enabled, b[0].text)),
                _ => None,
            })
        };
        let mut c = Config::default();
        assert_eq!(c.launcher.chord_spec(), None, "off by default");
        assert_eq!(add(&c), Some((false, "Add\u{2026}")));
        let row = launcher_rows(&c, false).into_iter().find(|x| x.label == "Add an alias").unwrap();
        assert_eq!(click(&row, Some(0), &ctx(&[], &idle())), Effect::None);
        c.launcher.chord = "alt+space".into();
        assert_eq!(add(&c), Some((true, "Add\u{2026}")));
        let row = launcher_rows(&c, false).into_iter().find(|x| x.label == "Add an alias").unwrap();
        assert_eq!(click(&row, Some(0), &ctx(&[], &idle())), Effect::Run(Action::AddLauncherAlias));
    }

    #[test]
    fn launcher_rows_en_it() {
        // Every provider names its texts in both languages, and the provider table is the config's list.
        let texts = provider_texts();
        assert_eq!(texts.map(|p| p.0), config::LAUNCHER_PROVIDERS);
        // (provider, label EN, label IT, description EN, description IT) as the table held them before the keyed strings.
        let old = [
            (
                "apps",
                "Apps",
                "App",
                "Installed apps and shortcuts. Always on",
                "App installate e collegamenti. Sempre attivo",
            ),
            ("windows", "Open windows", "Finestre aperte", "Switch to a window", "Passa a una finestra"),
            (
                "files",
                "Files",
                "File",
                "Names starting with the typed words, from the Windows Search index",
                "Nomi che iniziano con le parole scritte, dall'indice di Windows Search",
            ),
            (
                "settings",
                "Windows settings",
                "Impostazioni di Windows",
                "Pages of the Settings app",
                "Pagine dell'app Impostazioni",
            ),
            ("calc", "Calculator", "Calcolatrice", "Sums typed in the box", "Calcoli scritti nella casella"),
            (
                "commands",
                "Commands",
                "Comandi",
                "Lock, sleep, restart and the other power commands",
                "Blocca, sospendi, riavvia e gli altri comandi di alimentazione",
            ),
            ("run", "Run", "Esegui", "Start a typed command line", "Avvia una riga di comando scritta"),
            (
                "web",
                "Web search",
                "Ricerca sul web",
                "Search the web with the engine below",
                "Cerca sul web con il motore qui sotto",
            ),
            (
                "datetime",
                "Date and time",
                "Data e ora",
                "Today, the time, a date's weekday",
                "Oggi, l'ora, il giorno di una data",
            ),
            (
                "registry",
                "Registry",
                "Registro",
                "Open a registry key in Regedit",
                "Apri una chiave di registro in Regedit",
            ),
        ];
        assert_eq!(texts.len(), old.len());
        for ((name, _, label, desc), (old_name, label_en, label_it, desc_en, desc_it)) in texts.into_iter().zip(old) {
            assert_eq!(name, old_name);
            for (key, en, it) in [(label, label_en, label_it), (desc, desc_en, desc_it)] {
                assert_eq!(crate::i18n::with_italian(false, || t(key)), en, "{name}");
                assert_eq!(crate::i18n::with_italian(true, || t(key)), it, "{name}");
                assert_ne!(en, it, "{name}: the Italian text is not the English one");
            }
        }
        // Every row of the page outside the providers has a description, but the ones that need none.
        let r = launcher_rows(&Config::default(), true);
        let bare = ["Launcher", "Opens on", "Width", "Rows"];
        for x in &r {
            let header = matches!(x.control, Control::Header { .. });
            assert!(x.desc.is_some() || header || bare.contains(&x.label.as_str()), "{}", x.label);
        }
        assert_eq!(Page::Launcher.title(), "Launcher");
        assert_eq!(Page::Launcher.slug(), "launcher");
        assert!(
            Page::ALL.iter().position(|p| *p == Page::Launcher)
                == Page::ALL.iter().position(|p| *p == Page::Start).map(|i| i + 1)
        );
        assert_eq!(Page::ALL.len(), 8);
    }

    #[test]
    fn launcher_page_rows_and_prefixes() {
        let mut c = Config::default();
        let r = launcher_rows(&c, false);
        let toggle_of = |r: &[Row], s: Setting| {
            r.iter().find_map(|x| match x.control {
                Control::Toggle { setting, on, enabled } if setting == s => Some((on, enabled, x.desc.clone())),
                _ => None,
            })
        };
        // Apps: present, on and disabled; the other providers can be switched.
        assert_eq!(toggle_of(&r, Setting::LauncherProvider("apps")).map(|t| (t.0, t.1)), Some((true, false)));
        for p in config::LAUNCHER_PROVIDERS.into_iter().filter(|p| *p != "apps") {
            assert_eq!(toggle_of(&r, Setting::LauncherProvider(p)).map(|t| (t.0, t.1)), Some((true, true)), "{p}");
        }
        // A click on the disabled Apps row does nothing; a flip of apps does nothing either.
        let apps = r.iter().find(|x| {
            x.control == Control::Toggle { setting: Setting::LauncherProvider("apps"), on: true, enabled: false }
        });
        assert_eq!(click(apps.unwrap(), None, &ctx(&[], &idle())), Effect::None);
        flip(&mut c, Setting::LauncherProvider("apps"));
        assert!(is_on(&c, Setting::LauncherProvider("apps")));
        // Descriptions name the prefix of the config, "file " as the word and a space.
        let files = toggle_of(&r, Setting::LauncherProvider("files")).and_then(|t| t.2).unwrap();
        assert!(files.contains("\"file\" and a space"), "{files}");
        let calc = toggle_of(&r, Setting::LauncherProvider("calc")).and_then(|t| t.2).unwrap();
        assert!(calc.ends_with("Prefix \"=\""), "{calc}");
        c.launcher.prefixes.remove("=");
        c.launcher.prefixes.insert("#".into(), "calc".into());
        let r = launcher_rows(&c, false);
        assert!(toggle_of(&r, Setting::LauncherProvider("calc")).and_then(|t| t.2).unwrap().ends_with("Prefix \"#\""));
        let table = r.iter().find_map(|x| match &x.control {
            Control::Status(s) if s.contains("??") => Some(s.clone()),
            _ => None,
        });
        let table = table.unwrap();
        assert!(table.contains('#'), "{table}");
        assert!(table.ends_with("file  #"), "{table}");
        // The table reads in the defaults' order, the owner's own prefixes after them.
        let d = launcher_rows(&Config::default(), false);
        assert!(d.iter().any(|x| x.control == Control::Status("=  >  <  ??  $  )  :  file".into())));
        // The PowerToys row is there only when one runs and the chord is Alt+Space.
        let has_pt = |r: &[Row]| {
            r.iter().any(|x| matches!(x.control, Control::Choice { setting: Setting::LauncherPowerToys, .. }))
        };
        c.launcher.chord = "alt+space".into();
        assert!(!has_pt(&launcher_rows(&c, false)));
        assert!(has_pt(&launcher_rows(&c, true)));
        c.launcher.chord = "alt+shift+space".into();
        assert!(!has_pt(&launcher_rows(&c, true)));
    }

    #[test]
    fn launcher_choices_and_sliders() {
        let mut c = Config::default();
        let sc = [];
        for set in
            [Setting::LauncherChord, Setting::LauncherPowerToys, Setting::LauncherMonitor, Setting::LauncherFullscreen]
        {
            for j in 0..options(set, &sc).len() {
                apply_edit(&mut c, Edit::Choose(set, j), &sc);
                assert_eq!(selected(&c, set, &sc), Some(j), "{set:?} {j}");
            }
        }
        // A chord chosen here answers the Start menu's one-time offer; the PowerToys choice answers its notice.
        let mut d = Config::default();
        apply_edit(&mut d, Edit::Choose(Setting::LauncherChord, 1), &sc);
        assert_eq!((d.launcher.chord.as_str(), d.launcher.offer_done), ("alt+shift+space", true));
        apply_edit(&mut d, Edit::Choose(Setting::LauncherPowerToys, 1), &sc);
        assert_eq!((d.launcher.with_powertoys.as_str(), d.launcher.powertoys_done), ("take", true));
        // Only a pick of another chord is probed; Off and the current chord are not.
        let pick = |j| Edit::Choose(Setting::LauncherChord, j);
        assert_eq!(chord_to_probe(pick(0), "off"), Some(false));
        assert_eq!(chord_to_probe(pick(1), "alt+space"), Some(true));
        assert_eq!(chord_to_probe(pick(0), "alt+space"), None);
        assert_eq!(chord_to_probe(pick(2), "alt+space"), None);
        assert_eq!(chord_to_probe(Edit::Choose(Setting::LauncherMonitor, 0), "off"), None);
        // A refused pick leaves a line under the Shortcut row.
        let st = idle();
        let mut cx = ctx(&sc, &st);
        cx.chord_held = Some(true);
        let r = rows(Page::Launcher, &Config::default(), &cx);
        assert!(r.iter().any(|x| x.control == Control::Status("Another app holds Alt+Shift+Space".into())));
        assert!(!launcher_rows(&Config::default(), false).iter().any(|x| x.label == "Not changed"));
        // Every value the page writes is one the config accepts untouched.
        for set in
            [Setting::LauncherChord, Setting::LauncherPowerToys, Setting::LauncherMonitor, Setting::LauncherFullscreen]
        {
            for j in 0..options(set, &sc).len() {
                let mut x = Config::default();
                apply_edit(&mut x, Edit::Choose(set, j), &sc);
                assert!(x.launcher.sanitize().is_empty(), "{set:?} {j}");
            }
        }
        // Web: Custom needs a valid template in the file, else the click changes nothing.
        let mut w = Config::default();
        apply_edit(&mut w, Edit::Choose(Setting::LauncherWeb, 3), &sc);
        assert_eq!(w.launcher.web_engine, "google");
        w.launcher.web_url = "https://search.example.org/?q={q}".into();
        apply_edit(&mut w, Edit::Choose(Setting::LauncherWeb, 3), &sc);
        assert_eq!((w.launcher.web_engine.as_str(), selected(&w, Setting::LauncherWeb, &sc)), ("custom", Some(3)));
        let st = idle();
        let r = rows(Page::Launcher, &w, &ctx(&[], &st));
        assert!(r.iter().any(|x| x.desc.as_deref() == Some("Custom: search.example.org")));
        for (j, e) in ["bing", "duckduckgo"].into_iter().enumerate() {
            apply_edit(&mut w, Edit::Choose(Setting::LauncherWeb, j + 1), &sc);
            assert_eq!(w.launcher.web_engine, e);
        }
        // Sliders: width in steps of 40 inside 560..=880, rows 4..=12; both read back what was set.
        assert_eq!(slider_range(Slider::LauncherWidth), (560, 880, " px"));
        assert_eq!(slider_range(Slider::LauncherRows).0, 4);
        for (v, want) in [(0, 560), (579, 560), (581, 600), (639, 640), (700, 720), (9999, 880)] {
            set_slider(&mut c, Slider::LauncherWidth, v);
            assert_eq!(slider_value(&c, Slider::LauncherWidth), want, "{v}");
            assert!(c.launcher.sanitize().is_empty());
        }
        for (v, want) in [(0, 4), (7, 7), (99, 12)] {
            set_slider(&mut c, Slider::LauncherRows, v);
            assert_eq!(slider_value(&c, Slider::LauncherRows), want);
        }
        // Switches.
        flip(&mut c, Setting::LauncherOn);
        flip(&mut c, Setting::LauncherLearn);
        assert!(!is_on(&c, Setting::LauncherOn) && !is_on(&c, Setting::LauncherLearn));
    }

    #[test]
    fn launcher_toggle_reaches_opts() {
        let mut c = Config::default();
        assert!(crate::popup::popup_launcher::opts_from_config(&c.launcher)
            .mask
            .has(crate::launcher::model::ProviderId::Files));
        flip(&mut c, Setting::LauncherProvider("files"));
        let mask = crate::popup::popup_launcher::opts_from_config(&c.launcher).mask;
        assert!(!mask.has(crate::launcher::model::ProviderId::Files));
        assert!(mask.has(crate::launcher::model::ProviderId::Calc));
        // The same row switched back on.
        flip(&mut c, Setting::LauncherProvider("files"));
        assert!(crate::popup::popup_launcher::opts_from_config(&c.launcher)
            .mask
            .has(crate::launcher::model::ProviderId::Files));
    }

    #[test]
    fn small_formatters() {
        assert_eq!(short_date("2026-11-20T10:00:00Z"), "2026-11-20");
        assert_eq!(short_date(""), "");
        assert_eq!(short_date("garbage"), "garbage");
        assert_eq!(mb(10_276_044), "9.8 MB");
        assert_eq!(mb(0), "0.0 MB");
    }
}
