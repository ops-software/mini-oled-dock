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
use crate::i18n::tr;
use crate::popup_tray::TrayRow;
use crate::start_import;
use crate::ui::Screen;
use crate::update::{Outcome, Phase, UpdateStatus};
use crate::update_release::{release_page_url, safe_release_url};

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
            Page::Appearance => tr("Appearance", "Aspetto"),
            Page::Bar => tr("Bar", "Barra"),
            Page::Start => "Start",
            Page::Launcher => "Launcher",
            Page::Modules => tr("Modules", "Moduli"),
            Page::Screen => tr("Screen", "Schermo"),
            Page::Updates => tr("Updates", "Aggiornamenti"),
            Page::About => tr("About", "Informazioni"),
        }
    }

    pub(crate) fn subtitle(self) -> &'static str {
        match self {
            Page::Appearance => tr("Colour, sizes and language of the bar", "Colore, dimensioni e lingua della barra"),
            Page::Bar => tr("Where the bar sits and how it behaves", "Dove sta la barra e come si comporta"),
            Page::Start => tr("The Start button and the Start menu", "Il pulsante Start e il menu Start"),
            Page::Launcher => tr("Find and start anything from one bar", "Trova e avvia qualsiasi cosa da una barra"),
            Page::Modules => tr("What the bar shows on the right", "Cosa mostra la barra a destra"),
            Page::Screen => tr("The display the bar sits on", "Lo schermo su cui sta la barra"),
            Page::Updates => tr("New versions from GitHub", "Nuove versioni da GitHub"),
            Page::About => tr("Version, memory and files", "Versione, memoria e file"),
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
    TrayApp {
        key: String,
        visible: bool,
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
}

// ------------------------------------------------------------------ helpers moved from the old popup

/// Row label of a `config::MODULE_NAMES` entry (or "start").
pub(crate) fn module_label(name: &str) -> &'static str {
    match name {
        "notifications" => tr("Notifications", "Notifiche"),
        "volume" => tr("Volume", "Volume"),
        "network" => tr("Network", "Rete"),
        "metrics" => tr("System metrics", "Risorse di sistema"),
        "clock" => tr("Clock", "Orologio"),
        "show_desktop" => tr("Show desktop", "Mostra desktop"),
        "start" => tr("Start button", "Pulsante Start"),
        "tray" => tr("Tray icons", "Icone area notifiche"),
        _ => tr("Module", "Modulo"),
    }
}

/// Labels of the segments of a choice (`sc`: the displays, for the screen choice).
pub(crate) fn options(s: Setting, sc: &[Screen]) -> Vec<&'static str> {
    match s {
        // Each language in its own name, so it can be found whatever the current one is.
        Setting::Language => vec![tr("System", "Sistema"), "English", "Italiano"],
        Setting::StartStyle => vec![tr("Compact", "Compatto"), tr("Windows 11", "Windows 11")],
        Setting::Position => vec![tr("Top", "In alto"), tr("Bottom", "In basso")],
        Setting::Monitor => {
            // Last segment: follow the pointer (`config::MONITOR_POINTER`).
            let main = tr("Main", "Principale");
            sc.iter()
                .map(|d| if d.primary { main } else { d.label })
                .chain(std::iter::once(tr("Pointer", "Puntatore")))
                .collect()
        }
        Setting::DisplayMode => vec![tr("Icon + name", "Icona + nome"), tr("Icon", "Icona"), tr("Name", "Nome")],
        Setting::BatteryShow => vec![tr("Auto", "Automatica"), tr("On", "S\u{ec}"), tr("Off", "No")],
        Setting::LauncherChord => {
            vec![tr("Alt+Space", "Alt+Spazio"), tr("Alt+Shift+Space", "Alt+Maiusc+Spazio"), tr("Off", "No")]
        }
        Setting::LauncherPowerToys => vec![tr("Leave it", "Lascialo"), tr("Take it", "Prendilo")],
        Setting::LauncherMonitor => {
            vec![tr("Pointer", "Puntatore"), tr("Active window", "Finestra attiva"), tr("Primary", "Principale")]
        }
        Setting::LauncherFullscreen => vec![tr("Pass", "Passa"), tr("Open", "Apri")],
        Setting::LauncherWeb => vec!["Google", "Bing", "DuckDuckGo", tr("Custom", "Personalizzato")],
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
        tr("Another app holds Alt+Shift+Space", "Un'altra app usa Alt+Maiusc+Spazio")
    } else {
        tr("Another app holds Alt+Space", "Un'altra app usa Alt+Spazio")
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
        Setting::Seconds => c.show_seconds,
        Setting::VolumePercent => c.show_volume_percent,
        Setting::BatteryPercent => c.battery_percent,
        Setting::BatteryInside => c.battery_percent_inside,
        Setting::UpdateCheck => c.update_check,
        Setting::UpdateAutoInstall => c.update_auto_install,
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
        Setting::Seconds => c.show_seconds = !c.show_seconds,
        Setting::VolumePercent => c.show_volume_percent = !c.show_volume_percent,
        Setting::BatteryPercent => c.battery_percent = !c.battery_percent,
        Setting::BatteryInside => c.battery_percent_inside = !c.battery_percent_inside,
        Setting::UpdateCheck => c.update_check = !c.update_check,
        Setting::UpdateAutoInstall => c.update_auto_install = !c.update_auto_install,
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
        Slider::LauncherWidth => (config::LAUNCHER_WIDTH.0 as i32, config::LAUNCHER_WIDTH.1 as i32, " px"),
        Slider::LauncherRows => (config::LAUNCHER_ROWS.0 as i32, config::LAUNCHER_ROWS.1 as i32, ""),
    }
}

pub(crate) fn slider_value(c: &Config, sl: Slider) -> i32 {
    match sl {
        Slider::LauncherWidth => return i32::from(c.launcher.width),
        Slider::LauncherRows => return i32::from(c.launcher.rows),
        _ => {}
    }
    let (icon, text, tray, module) = c.sizes();
    (match sl {
        Slider::Opacity => c.bar_opacity.min(100),
        Slider::AppIcon => icon,
        Slider::AppText => text,
        Slider::TrayIcon => tray,
        Slider::Module => module,
        Slider::LauncherWidth | Slider::LauncherRows => 0,
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
    let v = v as u8;
    match sl {
        Slider::Opacity => c.bar_opacity = v,
        Slider::AppIcon => c.app_icon_size = v,
        Slider::AppText => c.app_text_size = v,
        Slider::TrayIcon => c.tray_icon_size = v,
        Slider::Module => c.module_size = v,
        Slider::LauncherRows => c.launcher.rows = v,
        Slider::LauncherWidth => {}
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

/// Name of preset `k` in the dock language (the table keeps the English one).
pub(crate) fn preset_name(k: usize) -> &'static str {
    const IT: [&str; 7] = ["Nero", "Grafite", "Ardesia", "Blu notte", "Prugna", "Vinaccia", "Verde bosco"];
    let en = BACKGROUND_PRESETS.get(k).map_or("", |p| p.0);
    crate::i18n::tr(en, IT.get(k).copied().unwrap_or(en))
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
        start_import::Status::Idle => tr("Import from Windows Start", "Importa dallo Start di Windows").to_string(),
        start_import::Status::Running => tr("Importing\u{2026}", "Importazione\u{2026}").to_string(),
        start_import::Status::Failed => tr("Import failed", "Importazione non riuscita").to_string(),
        start_import::Status::TimedOut => {
            tr("Import failed (timeout)", "Importazione non riuscita (timeout)").to_string()
        }
        start_import::Status::Imported(n) => {
            if crate::i18n::italian() {
                format!("Importate {n} app")
            } else if n == 1 {
                "Imported 1 app".to_string()
            } else {
                format!("Imported {n} apps")
            }
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
}

pub(crate) fn apply_edit(c: &mut Config, e: Edit, screens: &[Screen]) {
    match e {
        Edit::Choose(s, j) => choose(c, s, j, screens),
        Edit::Flip(s) => flip(c, s),
        Edit::Slider(sl, v) => set_slider(c, sl, v),
        Edit::Background(b) => c.background = b,
        Edit::ResetSizes => reset_sizes(c),
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
                a => Effect::Run(a),
            },
            _ => Effect::None,
        },
        Control::Chooser { .. } => Effect::ToggleChooser,
        // The row carries the app's key; it acts only while that app is still in the live list.
        Control::TrayApp { key, visible } if ctx.tray.iter().any(|t| t.key == *key) => {
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
    let mut v = vec![header(tr("Modules", "Moduli"), None)];
    for n in MODULE_TOGGLES {
        v.push(toggle(icon(n), module_label(n), None, c, Setting::Module(n)));
    }
    v.push(header(tr("Battery", "Batteria"), None));
    v.push(choice(
        glyphs::battery_icon,
        tr("Show battery", "Mostra batteria"),
        Some(tr("Shown automatically on laptops", "Mostrata automaticamente sui portatili")),
        c,
        Setting::BatteryShow,
        ctx.screens,
    ));
    v.push(toggle(glyphs::text_shape, tr("Percentage", "Percentuale"), None, c, Setting::BatteryPercent));
    v.push(row(
        glyphs::battery_icon,
        tr("Percentage inside the icon", "Percentuale dentro l'icona"),
        Some(tr("Apple style", "Stile Apple")),
        Control::Toggle { setting: Setting::BatteryInside, on: c.battery_percent_inside, enabled: c.battery_percent },
    ));
    v.push(header(tr("Tray", "Area di notifica"), None));
    v.push(toggle(glyphs::ring_shape, module_label("tray"), None, c, Setting::Module("tray")));
    if c.module_visible("tray") {
        v.push(row(
            glyphs::lock_shape,
            tr("Icons of apps run as administrator", "Icone delle app avviate come amministratore"),
            Some(tr(
                "Allow clicks on their icons, for example MSI Afterburner",
                "Consenti i clic sulle loro icone, ad esempio MSI Afterburner",
            )),
            Control::Toggle { setting: Setting::TrustElevatedTray, on: ctx.trust_elevated, enabled: true },
        ));
        v.push(row(
            glyphs::eye_icon,
            tr("Choose visible icons", "Scegli le icone visibili"),
            None,
            Control::Chooser { open: ctx.chooser_open },
        ));
        if ctx.chooser_open {
            if ctx.tray.is_empty() {
                v.push(Row {
                    icon: None,
                    label: tr("No tray apps", "Nessuna app nell'area di notifica").to_string(),
                    desc: None,
                    control: Control::Note,
                });
            }
            for t in ctx.tray {
                let no_room = t.visible && !t.in_bar;
                v.push(Row {
                    icon: None,
                    label: t.name.clone(),
                    desc: no_room.then(|| tr("No room in the bar", "Spazio insufficiente nella barra").to_string()),
                    control: Control::TrayApp { key: t.key.clone(), visible: t.visible },
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
        None if st.last_check.is_empty() => tr("Not checked yet", "Non ancora controllato").to_string(),
        None => tr("Up to date", "Aggiornato").to_string(),
    };
    let last_check =
        if st.last_check.is_empty() { tr("Never", "Mai").to_string() } else { short_date(&st.last_check).to_string() };
    // The outcome of the last action, never the running phase (the Note row shows that).
    let last_action = if st.outcome == Outcome::None {
        tr("None yet", "Nessuna").to_string()
    } else {
        UpdateStatus { phase: Phase::Idle, ..st.clone() }.line()
    };
    let notes_ok = st.available.as_ref().is_some_and(|a| safe_release_url(&release_page_url(Some(&a.tag))));
    let mut v = vec![
        header(tr("Status", "Stato"), None),
        row(
            glyphs::info_shape,
            tr("Installed version", "Versione installata"),
            None,
            Control::Status(st.installed.to_string()),
        ),
        row(glyphs::refresh_shape, tr("Available", "Disponibile"), None, Control::Status(available)),
        row(glyphs::clock_shape, tr("Last check", "Ultimo controllo"), None, Control::Status(last_check)),
        row(glyphs::log_shape, tr("Last action", "Ultima operazione"), None, Control::Status(last_action)),
        row(
            glyphs::refresh_shape,
            "",
            None,
            Control::Buttons(vec![
                Button { action: Action::CheckUpdates, text: tr("Check now", "Controlla ora"), enabled: idle },
                Button {
                    action: Action::ReleaseNotes,
                    text: tr("Release notes", "Note di rilascio"),
                    enabled: notes_ok,
                },
                Button {
                    action: Action::InstallUpdate,
                    text: tr("Update now", "Aggiorna ora"),
                    enabled: idle && st.available.is_some(),
                },
            ]),
        ),
    ];
    if !idle {
        v.push(Row { icon: None, label: st.line(), desc: None, control: Control::Note });
    }
    v.push(header(tr("Automatic", "Automatico"), None));
    v.push(toggle(
        glyphs::refresh_shape,
        tr("Check automatically", "Controlla automaticamente"),
        Some(tr("Once a day on GitHub", "Una volta al giorno su GitHub")),
        c,
        Setting::UpdateCheck,
    ));
    v.push(toggle(
        glyphs::download_shape,
        tr("Install automatically", "Installa automaticamente"),
        Some(tr("When the dock is idle", "Quando il dock \u{e8} inattivo")),
        c,
        Setting::UpdateAutoInstall,
    ));
    v
}

fn about(ctx: &Ctx<'_>) -> Vec<Row> {
    let mem = |b: Option<usize>, none: &str| b.map_or_else(|| none.to_string(), mb);
    let files = |icon: Glyph, label: &str, path: Option<&str>, action| {
        buttons(icon, label, path, vec![Button { action, text: tr("Open", "Apri"), enabled: path.is_some() }])
    };
    vec![
        header(tr("Dock", "Dock"), None),
        row(
            glyphs::info_shape,
            &format!("mini-oled-dock {}", crate::preview::version_text()),
            Some(tr("Ops Software \u{b7} MIT licence", "Ops Software \u{b7} licenza MIT")),
            Control::Note,
        ),
        header(tr("Memory", "Memoria"), None),
        row(
            glyphs::bars_shape,
            tr("Memory now", "Memoria ora"),
            None,
            Control::Status(mem(ctx.memory.now, "\u{2014}")),
        ),
        row(
            glyphs::bars_shape,
            tr("After trimming", "Dopo la pulizia"),
            Some(tr("While the bar is hidden", "Mentre la barra \u{e8} nascosta")),
            Control::Status(mem(ctx.memory.trimmed, tr("Not trimmed yet", "Non ancora eseguita"))),
        ),
        header(tr("Files", "File"), None),
        files(glyphs::folder_shape, tr("Data folder", "Cartella dati"), ctx.data_dir, Action::OpenDataFolder),
        files(glyphs::log_shape, "Log", ctx.log_path, Action::OpenLog),
        header(tr("Taskbar", "Barra delle applicazioni"), None),
        buttons(
            glyphs::power_shape,
            tr("Restore the Windows taskbar", "Ripristina la barra di Windows"),
            Some(tr("Exits the dock", "Chiude il dock")),
            vec![Button { action: Action::ExitDock, text: tr("Exit the dock", "Esci dal dock"), enabled: true }],
        ),
    ]
}

fn appearance(c: &Config, ctx: &Ctx<'_>) -> Vec<Row> {
    let colour = match preset_index(c.background) {
        Some(k) => preset_name(k).to_string(),
        None => tr("Custom", "Personalizzato").to_string(),
    };
    let swatch = preset_index(c.background).unwrap_or(BACKGROUND_PRESETS.len());
    let mut v = vec![
        Row { icon: None, label: String::new(), desc: None, control: Control::Preview },
        header(tr("Colour", "Colore"), Some(colour)),
        row(
            glyphs::circle_shape,
            tr("Background", "Sfondo"),
            None,
            Control::Swatches { selected: swatch, picker_open: ctx.picker_open },
        ),
    ];
    if ctx.picker_open {
        v.push(Row { icon: None, label: String::new(), desc: None, control: Control::Picker });
    }
    v.push(slider_row(glyphs::drop_shape, tr("Opacity", "Opacit\u{e0}"), c, Slider::Opacity));
    v.push(header(tr("Sizes", "Dimensioni"), None));
    v.push(slider_row(glyphs::square_shape, tr("App icons", "Icone app"), c, Slider::AppIcon));
    v.push(slider_row(glyphs::text_shape, tr("App names", "Nomi app"), c, Slider::AppText));
    v.push(slider_row(glyphs::ring_shape, tr("Tray icons", "Icone area notifiche"), c, Slider::TrayIcon));
    v.push(slider_row(glyphs::bars_shape, tr("Modules", "Moduli"), c, Slider::Module));
    v.push(buttons(
        glyphs::refresh_shape,
        tr("Reset sizes", "Ripristina dimensioni"),
        None,
        vec![Button { action: Action::ResetSizes, text: tr("Reset", "Ripristina"), enabled: !sizes_are_default(c) }],
    ));
    v.push(header(tr("Language", "Lingua"), None));
    v.push(choice(glyphs::globe_shape, tr("Language", "Lingua"), None, c, Setting::Language, ctx.screens));
    v
}

fn bar(c: &Config, ctx: &Ctx<'_>) -> Vec<Row> {
    let sc = ctx.screens;
    vec![
        header(tr("Position", "Posizione"), None),
        choice(glyphs::bar_shape, tr("Position", "Posizione"), None, c, Setting::Position, sc),
        choice(glyphs::text_shape, tr("Display mode", "Mostra"), None, c, Setting::DisplayMode, sc),
        header(tr("Behaviour", "Comportamento"), None),
        toggle(
            glyphs::chevron_down_shape,
            tr("Auto-hide", "Nascondi automaticamente"),
            Some(tr(
                "Shows up with the Win key or at the screen edge",
                "Compare con il tasto Win o al bordo dello schermo",
            )),
            c,
            Setting::AutoHide,
        ),
        toggle(
            glyphs::eye_icon,
            tr("Window previews", "Anteprime finestre"),
            Some(tr(
                "Rest the pointer on an app with several windows",
                "Fermando il puntatore su un'app con pi\u{f9} finestre",
            )),
            c,
            Setting::Previews,
        ),
        toggle(glyphs::clock_shape, tr("Clock seconds", "Secondi nell'orologio"), None, c, Setting::Seconds),
        toggle(
            glyphs::speaker_icon,
            tr("Volume percent", "Percentuale volume"),
            Some(tr("Next to the speaker", "Accanto all'altoparlante")),
            c,
            Setting::VolumePercent,
        ),
    ]
}

fn start(c: &Config, ctx: &Ctx<'_>) -> Vec<Row> {
    let sc = ctx.screens;
    let idle_desc =
        tr("Replaces the pinned apps of the dock's Start menu", "Sostituisce le app aggiunte al menu Start del dock");
    let desc = if ctx.import == start_import::Status::Idle { idle_desc.to_string() } else { import_label(ctx.import) };
    vec![
        header(tr("Start menu", "Menu Start"), None),
        toggle(glyphs::windows_logo_shape, tr("Start button", "Pulsante Start"), None, c, Setting::Module("start")),
        choice(glyphs::grid_shape, tr("Start layout", "Layout Start"), None, c, Setting::StartStyle, sc),
        toggle(
            glyphs::keycap_shape,
            tr("Win key opens Start", "Il tasto Win apre Start"),
            Some(tr("Together with the bar", "Insieme alla barra")),
            c,
            Setting::WinStart,
        ),
        header(tr("Pinned apps", "App aggiunte"), None),
        buttons(
            glyphs::download_shape,
            tr("Import from Windows Start", "Importa dallo Start di Windows"),
            Some(&desc),
            vec![Button {
                action: Action::ImportStart,
                text: tr("Import", "Importa"),
                enabled: ctx.import != start_import::Status::Running,
            }],
        ),
    ]
}

/// The first prefix of the config that names provider `name`, as the page shows it ("file " is "file" and a space).
fn prefix_phrase(c: &Config, name: &str) -> Option<String> {
    let p = c.launcher.prefixes.iter().find(|(_, n)| n.as_str() == name).map(|(p, _)| p.as_str())?;
    Some(if p.ends_with(' ') {
        format!("\"{}\" {}", p.trim_end(), tr("and a space", "e uno spazio"))
    } else {
        format!("\"{p}\"")
    })
}

/// One provider of "What it finds": name, glyph, label (EN, IT) and description (EN, IT). The description of a provider
/// with a prefix gets ". Prefix <p>" after it; Files has its own tail.
type ProviderText = (&'static str, Glyph, [&'static str; 2], [&'static str; 2]);

fn provider_texts() -> [ProviderText; 10] {
    [
        (
            "apps",
            glyphs::grid_shape,
            ["Apps", "App"],
            ["Installed apps and shortcuts. Always on", "App installate e collegamenti. Sempre attivo"],
        ),
        (
            "windows",
            glyphs::window_shape,
            ["Open windows", "Finestre aperte"],
            ["Switch to a window", "Passa a una finestra"],
        ),
        (
            "files",
            glyphs::folder_shape,
            ["Files", "File"],
            [
                "Names starting with the typed words, from the Windows Search index",
                "Nomi che iniziano con le parole scritte, dall'indice di Windows Search",
            ],
        ),
        (
            "settings",
            glyphs::gear_shape,
            ["Windows settings", "Impostazioni di Windows"],
            ["Pages of the Settings app", "Pagine dell'app Impostazioni"],
        ),
        (
            "calc",
            glyphs::equals_shape,
            ["Calculator", "Calcolatrice"],
            ["Sums typed in the box", "Calcoli scritti nella casella"],
        ),
        (
            "commands",
            glyphs::power_shape,
            ["Commands", "Comandi"],
            [
                "Lock, sleep, restart and the other power commands",
                "Blocca, sospendi, riavvia e gli altri comandi di alimentazione",
            ],
        ),
        (
            "run",
            glyphs::prompt_shape,
            ["Run", "Esegui"],
            ["Start a typed command line", "Avvia una riga di comando scritta"],
        ),
        (
            "web",
            glyphs::globe_shape,
            ["Web search", "Ricerca sul web"],
            ["Search the web with the engine below", "Cerca sul web con il motore qui sotto"],
        ),
        (
            "datetime",
            glyphs::clock_shape,
            ["Date and time", "Data e ora"],
            ["Today, the time, a date's weekday", "Oggi, l'ora, il giorno di una data"],
        ),
        (
            "registry",
            glyphs::list_shape,
            ["Registry", "Registro"],
            ["Open a registry key in Regedit", "Apri una chiave di registro in Regedit"],
        ),
    ]
}

/// The toggle of one provider in "What it finds".
fn provider_row(c: &Config, (name, icon, label, desc): ProviderText) -> Row {
    let (label, base) = (tr(label[0], label[1]), tr(desc[0], desc[1]));
    let desc = match (name, prefix_phrase(c, name)) {
        ("apps", _) | (_, None) => base.to_string(),
        ("files", Some(p)) => {
            format!("{base}; {} {p} {}", tr("type", "scrivi"), tr("for files only", "per i soli file"))
        }
        (_, Some(p)) => format!("{base}. {} {p}", tr("Prefix", "Prefisso")),
    };
    let setting = Setting::LauncherProvider(name);
    row(icon, label, Some(&desc), Control::Toggle { setting, on: is_on(c, setting), enabled: name != "apps" })
}

fn launcher(c: &Config, ctx: &Ctx<'_>) -> Vec<Row> {
    let sc = ctx.screens;
    let mut v = vec![
        header(tr("Launcher", "Launcher"), None),
        toggle(
            crate::popup::popup_startmenu::magnifier,
            tr("Launcher", "Launcher"),
            Some(tr("Find and start anything from one bar", "Trova e avvia qualsiasi cosa da una barra")),
            c,
            Setting::LauncherOn,
        ),
        choice(
            glyphs::keycap_shape,
            tr("Shortcut", "Scorciatoia"),
            Some(tr(
                "With Alt+Space taken, type \"window menu\" for a window's menu",
                "Con Alt+Spazio preso, scrivi \"menu finestra\" per il menu della finestra",
            )),
            c,
            Setting::LauncherChord,
            sc,
        ),
    ];
    if let Some(shift) = ctx.chord_held {
        v.push(row(
            glyphs::info_shape,
            tr("Not changed", "Non cambiata"),
            None,
            Control::Status(chord_held_text(shift).to_string()),
        ));
    }
    if ctx.powertoys && c.launcher.chord == "alt+space" {
        v.push(choice(
            glyphs::info_shape,
            "PowerToys",
            Some(tr("Command Palette also uses Alt+Space", "Anche Command Palette usa Alt+Spazio")),
            c,
            Setting::LauncherPowerToys,
            sc,
        ));
    }
    v.push(header(tr("Window", "Finestra"), None));
    v.push(choice(glyphs::monitor_shape, tr("Opens on", "Si apre su"), None, c, Setting::LauncherMonitor, sc));
    v.push(slider_row(glyphs::window_shape, tr("Width", "Larghezza"), c, Slider::LauncherWidth));
    v.push(slider_row(glyphs::list_shape, tr("Rows", "Righe"), c, Slider::LauncherRows));
    v.push(choice(
        glyphs::show_desktop_shape,
        tr("Over full-screen apps", "Sopra le app a schermo intero"),
        Some(tr(
            "Games keep their keys; exclusive full screen always passes",
            "I giochi tengono i loro tasti; lo schermo intero esclusivo passa sempre",
        )),
        c,
        Setting::LauncherFullscreen,
        sc,
    ));
    v.push(header(tr("What it finds", "Cosa trova"), None));
    for text in provider_texts() {
        v.push(provider_row(c, text));
    }
    v.push(header(tr("Web search", "Ricerca sul web"), None));
    let custom_desc = match crate::launcher::web::validate_template(&c.launcher.web_url) {
        Ok(t) if c.launcher.web_engine == "custom" => format!("{}: {}", tr("Custom", "Personalizzato"), t.host()),
        _ => tr("Custom is set in the config file", "Personalizzato si imposta nel file di configurazione").to_string(),
    };
    v.push(row(
        glyphs::globe_shape,
        tr("Search engine", "Motore di ricerca"),
        Some(&custom_desc),
        Control::Choice {
            setting: Setting::LauncherWeb,
            options: options(Setting::LauncherWeb, sc),
            selected: selected(c, Setting::LauncherWeb, sc),
        },
    ));
    v.push(header(tr("History", "Cronologia"), None));
    v.push(toggle(
        glyphs::clock_shape,
        tr("Learn from what I pick", "Impara da ci\u{f2} che scelgo"),
        Some(tr("Kept for your account only", "Conservato solo per il tuo account")),
        c,
        Setting::LauncherLearn,
    ));
    v.push(buttons(
        glyphs::refresh_shape,
        tr("Forget history", "Dimentica la cronologia"),
        Some(tr("The commands you ran and the order it learned", "I comandi eseguiti e l'ordine appreso")),
        vec![Button { action: Action::ForgetHistory, text: tr("Forget", "Dimentica"), enabled: true }],
    ));
    v.push(header(tr("Prefixes", "Prefissi"), None));
    // The defaults in their table order, then the owner's own.
    let mut keys: Vec<&str> = c.launcher.prefixes.keys().map(String::as_str).collect();
    keys.sort_by_key(|k| config::LAUNCHER_PREFIXES.iter().position(|(p, _)| p == k).unwrap_or(usize::MAX));
    let table = keys.iter().map(|p| p.trim_end()).collect::<Vec<_>>().join("  ");
    v.push(row(
        glyphs::text_shape,
        tr("Prefixes", "Prefissi"),
        Some(tr("Typed first, they pick one source", "Scritti all'inizio, scelgono una sola fonte")),
        Control::Status(table),
    ));
    v.push(buttons(
        glyphs::pencil_shape,
        tr("Config file", "File di configurazione"),
        Some(tr("This window has no text field yet", "Questa finestra non ha ancora un campo di testo")),
        vec![Button { action: Action::EditConfig, text: tr("Edit config", "Modifica"), enabled: true }],
    ));
    v
}

fn screen(c: &Config, ctx: &Ctx<'_>) -> Vec<Row> {
    if ctx.screens.len() < 2 {
        return vec![Row {
            icon: None,
            label: tr(
                "The bar is on the only display. Connect another one to choose.",
                "La barra \u{e8} sull'unico schermo. Collegane un altro per scegliere.",
            )
            .to_string(),
            desc: None,
            control: Control::Note,
        }];
    }
    vec![
        header(tr("Display", "Schermo"), None),
        choice(
            glyphs::monitor_shape,
            tr("Display", "Schermo"),
            Some(tr(
                "Pointer: the Win key brings the bar to the display under the pointer",
                "Puntatore: il tasto Win porta la barra sullo schermo sotto il puntatore",
            )),
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
        assert_eq!(BACKGROUND_PRESETS.len(), 7, "update the Italian names in preset_name");
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
            start_import::Status::Failed,
            start_import::Status::TimedOut,
            start_import::Status::Imported(3),
        ] {
            assert!(!import_label(st).is_empty());
        }
        assert_eq!(import_label(start_import::Status::Imported(1)), "Imported 1 app");
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
            // In the Tray section, between the module toggle and the chooser.
            assert_eq!(r[k - 1].control, Control::Toggle { setting: Setting::Module("tray"), on: true, enabled: true });
            assert_eq!(r[k + 1].control, Control::Chooser { open: false });
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
    fn tray_chooser_rows_are_inline() {
        let st = idle();
        let tray = tray_rows();
        let mut cx = ctx(&[], &st);
        cx.tray = &tray;
        cx.chooser_open = true;
        let r = rows(Page::Modules, &Config::default(), &cx);
        let k = r.iter().position(|r| r.control == Control::Chooser { open: true }).unwrap();
        assert_eq!(r[k + 1].control, Control::TrayApp { key: "dropbox.exe#1".into(), visible: true });
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

    #[test]
    fn launcher_rows_en_it() {
        // Every provider names its texts in both languages, and the provider table is the config's list.
        let t = provider_texts();
        assert_eq!(t.map(|p| p.0), config::LAUNCHER_PROVIDERS);
        for (name, _, label, desc) in t {
            for pair in [label, desc] {
                assert!(!pair[0].is_empty() && !pair[1].is_empty(), "{name}");
                assert_ne!(pair[0], pair[1], "{name}: the Italian text is not the English one");
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
