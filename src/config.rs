//! Configuration `%ProgramData%\mini-oled-dock\config.json` (folder and file security: `datadir`).
//!
//! The config is global and shared across threads: `get()` returns an immutable snapshot
//! (`Arc<Config>`) that anyone (UI, shell thread) can hold without locking. Changes go
//! through `modify()` (read-modify-write with an atomic write: temp file + rename, `datadir::write_atomic`) and through
//! `poll_changed()` (re-read if the mtime changes, called by the UI thread every 2 s).
//!
//! Command-line options override the config ONLY through the `effective_*` functions.

use std::cell::Cell;
use std::collections::BTreeMap;
use std::sync::{Arc, Mutex, OnceLock};

#[cfg(test)]
use oled_config::Options;
use oled_config::{
    options, Cond, ConfigError, Control, FileKind, FileSpec, Kept, Kind, LoadOutcome, MigrateError, PageId, Problem,
    Range, Reload, Scope, SecId, Store, Unit, ValueType,
};
use serde::{Deserialize, Serialize};

use crate::i18n::{k, t};

/// `Config::monitor` value meaning "follow the pointer": the Win key brings the bar to the display
/// under the mouse pointer (Settings -> Screen -> "Pointer").
pub const MONITOR_POINTER: &str = "pointer";

/// Language of the dock's own texts (`i18n`).
#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq, Default)]
#[serde(rename_all = "lowercase")]
pub enum Language {
    /// The Windows display language (Italian when it is Italian, English otherwise).
    #[default]
    Auto,
    En,
    It,
}

/// Layout of the dock's own Start menu (`popup_startmenu`). Written as "compact" / "windows11"
/// (read also as "vertical" / "horizontal", the names up to 1.4.0); an unknown value falls back to
/// compact instead of rejecting the whole file.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum StartStyle {
    /// Search, two rows of pins and the alphabetical list in one column (the original menu).
    #[default]
    Compact,
    /// Windows 11 organization: pages of pinned tiles, "All apps" on its own page, user and power bar.
    Windows11,
}

impl StartStyle {
    /// The config value.
    pub fn as_str(self) -> &'static str {
        match self {
            StartStyle::Compact => "compact",
            StartStyle::Windows11 => "windows11",
        }
    }

    /// Parses the config value (any case, old names included); anything else is `Compact`.
    pub fn parse(s: &str) -> StartStyle {
        let t = s.trim().to_ascii_lowercase();
        if matches!(t.as_str(), "windows11" | "windows 11" | "win11" | "horizontal") {
            StartStyle::Windows11
        } else {
            StartStyle::Compact
        }
    }
}

impl Serialize for StartStyle {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(self.as_str())
    }
}

impl<'de> Deserialize<'de> for StartStyle {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let s = String::deserialize(d)?;
        Ok(StartStyle::parse(&s))
    }
}

/// Battery module visibility. Written as `"auto"` / `true` / `false`; an unreadable value is `Auto`
/// instead of rejecting the file. `Auto` shows the module only when the machine has a battery.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum BatteryShow {
    /// Shown only on a machine with a battery (decided at runtime from the last sample).
    #[default]
    Auto,
    /// Always shown (an icon with a slash when there is no battery).
    On,
    /// Never shown.
    Off,
}

impl BatteryShow {
    /// Whether the module is in the bar, given whether a battery was seen.
    pub fn shown(self, battery_present: bool) -> bool {
        match self {
            BatteryShow::Auto => battery_present,
            BatteryShow::On => true,
            BatteryShow::Off => false,
        }
    }

    fn parse_str(s: &str) -> BatteryShow {
        let s = s.trim();
        if s.eq_ignore_ascii_case("true") {
            BatteryShow::On
        } else if s.eq_ignore_ascii_case("false") {
            BatteryShow::Off
        } else {
            BatteryShow::Auto
        }
    }
}

impl Serialize for BatteryShow {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        match self {
            BatteryShow::Auto => s.serialize_str("auto"),
            BatteryShow::On => s.serialize_bool(true),
            BatteryShow::Off => s.serialize_bool(false),
        }
    }
}

impl<'de> Deserialize<'de> for BatteryShow {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        // Any JSON value is accepted: a boolean is a valid setting, everything unreadable is `Auto`.
        Ok(match serde_json::Value::deserialize(d)? {
            serde_json::Value::Bool(true) => BatteryShow::On,
            serde_json::Value::Bool(false) => BatteryShow::Off,
            serde_json::Value::String(s) => BatteryShow::parse_str(&s),
            _ => BatteryShow::Auto,
        })
    }
}

/// How the hidden bar is revealed at the screen edge: `Dwell` = rest the pointer there, `Push` = push against
/// the edge. Written as "dwell" / "push"; an unreadable value is `Dwell` instead of rejecting the file.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Reveal {
    #[default]
    Dwell,
    Push,
}

impl Serialize for Reveal {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(match self {
            Reveal::Dwell => "dwell",
            Reveal::Push => "push",
        })
    }
}

impl<'de> Deserialize<'de> for Reveal {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        Ok(match serde_json::Value::deserialize(d)? {
            serde_json::Value::String(s) if s.trim().eq_ignore_ascii_case("push") => Reveal::Push,
            _ => Reveal::Dwell,
        })
    }
}

/// Which keyboard chords the dock owns (only while the native taskbar is hidden). All on by default.
#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq)]
#[serde(default)]
pub struct KeyboardConfig {
    /// Win+1..9 and Win+0 (also with Shift and Alt).
    pub win_numbers: bool,
    /// Win+T and Win+Shift+T.
    pub win_t: bool,
    /// Win+B.
    pub win_b: bool,
    /// Digit chips on the bar while Win is held.
    pub peek: bool,
}

impl Default for KeyboardConfig {
    fn default() -> Self {
        KeyboardConfig { win_numbers: true, win_t: true, win_b: true, peek: true }
    }
}

/// Range of `Config::hide_delay_ms` (ms).
pub const HIDE_DELAY_RANGE: (u32, u32) = (0, 5000);
/// Range of `Config::show_delay_ms` (ms).
pub const SHOW_DELAY_RANGE: (u32, u32) = (0, 1000);
/// Range of `Config::push_threshold` (raw counts within one second).
pub const PUSH_THRESHOLD_RANGE: (u32, u32) = (20, 1000);
/// Hide delay of a file that has no `hide_delay_ms` key (every file written before 1.6).
pub const LEGACY_HIDE_DELAY_MS: u32 = 2500;
/// Hide delay a fresh install writes.
pub const FRESH_HIDE_DELAY_MS: u32 = 1000;
const DEFAULT_SHOW_DELAY_MS: u32 = 150;
const DEFAULT_PUSH_THRESHOLD: u32 = 100;

/// Serde default of `Config::tray_privacy_migrated`: a missing key means the migration still has to run.
fn no_privacy_migration_yet() -> bool {
    false
}

/// Position of the bar.
#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq, Default)]
#[serde(rename_all = "lowercase")]
pub enum Position {
    Top,
    #[default]
    Bottom,
}

/// What each dock entry shows: icon + label, icon only, or label only.
#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq, Default)]
#[serde(rename_all = "snake_case")]
pub enum DisplayMode {
    #[default]
    IconText,
    Icon,
    Text,
}

/// Legacy bar size (before the independent sizes): read only to migrate an old config once
/// (`migrate_bar_size`), never written.
#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq, Default)]
#[serde(rename_all = "lowercase")]
pub enum BarSize {
    Small,
    #[default]
    Medium,
    Large,
}

/// Background colour of the bar and the popups, 0xRRGGBB. Written as "#RRGGBB"; the legacy
/// values "black" and "dark" are still accepted. An unreadable value falls back to black instead
/// of rejecting the whole file.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub struct Background(pub u32);

impl Background {
    /// Pure black (OLED), the default.
    pub const BLACK: Background = Background(0x000000);
    /// The legacy "dark" tone (very dark grey).
    pub const DARK: Background = Background(0x1C1C1E);

    /// Background colour (not premultiplied).
    pub fn rgb(self) -> (u32, u32, u32) {
        ((self.0 >> 16) & 255, (self.0 >> 8) & 255, self.0 & 255)
    }

    /// "#RRGGBB" (upper case).
    pub fn hex(self) -> String {
        format!("#{:06X}", self.0 & 0xFF_FFFF)
    }

    /// Parses "#RRGGBB" / "RRGGBB" / "#RGB" (any case) or the legacy "black" / "dark".
    pub fn parse(s: &str) -> Option<Background> {
        let t = s.trim();
        if t.eq_ignore_ascii_case("black") {
            return Some(Background::BLACK);
        }
        if t.eq_ignore_ascii_case("dark") {
            return Some(Background::DARK);
        }
        let h = t.strip_prefix('#').unwrap_or(t);
        if !h.chars().all(|c| c.is_ascii_hexdigit()) {
            return None;
        }
        match h.len() {
            6 => u32::from_str_radix(h, 16).ok().map(Background),
            3 => {
                let v = u32::from_str_radix(h, 16).ok()?;
                let (r, g, b) = ((v >> 8) & 15, (v >> 4) & 15, v & 15);
                Some(Background(((r * 17) << 16) | ((g * 17) << 8) | (b * 17)))
            }
            _ => None,
        }
    }
}

impl Serialize for Background {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(&self.hex())
    }
}

impl<'de> Deserialize<'de> for Background {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let s = String::deserialize(d)?;
        Ok(Background::parse(&s).unwrap_or_default())
    }
}

/// Preset background colours offered by the Settings window: dark tones that keep white text readable.
pub const BACKGROUND_PRESETS: [(&str, Background); 7] = [
    ("Black", Background::BLACK),
    ("Graphite", Background::DARK),
    ("Slate", Background(0x1A222D)),
    ("Navy", Background(0x0E1A33)),
    ("Plum", Background(0x261630)),
    ("Wine", Background(0x2E1119)),
    ("Forest", Background(0x10261B)),
];

// ------------------------------------------------------------- audio popup layout

/// Sections of the audio popup (`popup_audio`), as stored in `AudioLayout`: master volume.
pub const AUDIO_MASTER: u8 = 0;
/// Output devices.
pub const AUDIO_OUTPUT: u8 = 1;
/// Spatial sound formats.
pub const AUDIO_SPATIAL: u8 = 2;
/// Per-app volume mixer.
pub const AUDIO_MIXER: u8 = 3;
/// Number of sections.
const AUDIO_SECTIONS: u8 = 4;
/// Letter of each section in the config string (index = section).
const AUDIO_LETTERS: [char; 4] = ['m', 'o', 's', 'x'];

/// Layout of the audio popup: order, expanded / collapsed and hidden sections. 6 bytes in memory,
/// a short string in the file (`encode` / `parse`): one letter per section in display order
/// (M master, O output, S spatial, X mixer), upper case = expanded, lower case = collapsed, a
/// leading `-` = hidden. Default `"MOSX"`.
///
/// Invariant (kept by every method): `order` holds each section exactly once.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct AudioLayout {
    /// Display order of the sections.
    order: [u8; 4],
    /// Bit `section` set = expanded.
    open: u8,
    /// Bit `section` set = hidden.
    hidden: u8,
}

impl Default for AudioLayout {
    /// The original popup: all four sections visible and expanded.
    fn default() -> Self {
        AudioLayout { order: [AUDIO_MASTER, AUDIO_OUTPUT, AUDIO_SPATIAL, AUDIO_MIXER], open: 0x0F, hidden: 0 }
    }
}

impl AudioLayout {
    /// Sections in display order.
    pub fn order(&self) -> &[u8] {
        &self.order
    }

    /// Section expanded.
    pub fn is_open(&self, section: u8) -> bool {
        section < AUDIO_SECTIONS && self.open & (1 << section) != 0
    }

    pub fn is_hidden(&self, section: u8) -> bool {
        section < AUDIO_SECTIONS && self.hidden & (1 << section) != 0
    }

    /// Expands / collapses a section.
    pub fn toggle_open(&mut self, section: u8) {
        if section < AUDIO_SECTIONS {
            self.open ^= 1 << section;
        }
    }

    /// Shows / hides a section.
    pub fn toggle_hidden(&mut self, section: u8) {
        if section < AUDIO_SECTIONS {
            self.hidden ^= 1 << section;
        }
    }

    /// true if `section` is on screen with its content: visible and expanded. Only then does the
    /// popup read its data from Windows.
    pub fn loadable(&self, section: u8) -> bool {
        section < AUDIO_SECTIONS && !self.is_hidden(section) && self.is_open(section)
    }

    /// Moves `section` so that it ends up at position `index` of the order (clamped to the last
    /// one); the others keep their relative order. false if nothing changed.
    pub fn move_to(&mut self, section: u8, index: usize) -> bool {
        let Some(from) = self.order.iter().position(|&v| v == section) else { return false };
        let to = index.min(self.order.len() - 1);
        if from == to {
            return false;
        }
        if from < to {
            self.order[from..=to].rotate_left(1);
        } else {
            self.order[to..=from].rotate_right(1);
        }
        true
    }

    /// The config string (see the type doc), e.g. `"MOSX"` or `"M-oSX"`.
    pub fn encode(&self) -> String {
        let mut out = String::with_capacity(8);
        for &s in &self.order {
            if self.is_hidden(s) {
                out.push('-');
            }
            let c = AUDIO_LETTERS[s as usize];
            out.push(if self.is_open(s) { c.to_ascii_uppercase() } else { c });
        }
        out
    }

    /// Parses the config string. Tolerant: unknown characters and repeated sections are ignored,
    /// missing sections come back at the end, visible and expanded. Never fails. A string saved by
    /// an older version with the "More" group (`G(...)` / `g(...)`) is flattened: the grouped
    /// sections take the group's place, with their own flags (the group's flag is dropped).
    pub fn parse(text: &str) -> AudioLayout {
        let mut l = AudioLayout { order: [0; 4], open: 0, hidden: 0 };
        let mut n = 0;
        let mut seen = 0u8;
        let mut hide = false;
        for c in text.chars() {
            match c {
                '-' => hide = true,
                // Old group syntax: the marker and its brackets are dropped, a stray '-' with them.
                'g' | 'G' | '(' | ')' => hide = false,
                _ => {
                    let Some(s) = AUDIO_LETTERS.iter().position(|&x| x == c.to_ascii_lowercase()) else { continue };
                    let s = s as u8;
                    if seen & (1 << s) == 0 {
                        seen |= 1 << s;
                        if c.is_ascii_uppercase() {
                            l.open |= 1 << s;
                        }
                        if hide {
                            l.hidden |= 1 << s;
                        }
                        l.order[n] = s;
                        n += 1;
                    }
                    hide = false;
                }
            }
        }
        for s in 0..AUDIO_SECTIONS {
            if seen & (1 << s) == 0 {
                l.open |= 1 << s;
                l.order[n] = s;
                n += 1;
            }
        }
        l
    }
}

impl Serialize for AudioLayout {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(&self.encode())
    }
}

impl<'de> Deserialize<'de> for AudioLayout {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let s = String::deserialize(d)?;
        Ok(AudioLayout::parse(&s))
    }
}

/// Range of a size setting: `min..=max`; `default` reproduces the original look.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SizeRange {
    pub min: u8,
    pub max: u8,
    pub default: u8,
}

impl SizeRange {
    pub fn clamp(self, v: u8) -> u8 {
        v.clamp(self.min, self.max)
    }
}

/// App icon side (logical px).
pub const APP_ICON_SIZE: SizeRange = SizeRange { min: 14, max: 32, default: 16 };
/// App name text height (logical px; 13 px is the Windows UI text size at 100 %).
pub const APP_TEXT_SIZE: SizeRange = SizeRange { min: 10, max: 18, default: 13 };
/// Tray icon side (logical px).
pub const TRAY_ICON_SIZE: SizeRange = SizeRange { min: 12, max: 24, default: 16 };
/// Scale of the system modules on the right (percent).
pub const MODULE_SIZE: SizeRange = SizeRange { min: 75, max: 150, default: 100 };

/// The providers a `launcher.providers` map may name, in the order the template lists them.
pub(crate) const LAUNCHER_PROVIDERS: [&str; 10] =
    ["apps", "windows", "files", "settings", "calc", "commands", "run", "web", "datetime", "registry"];

/// The launcher's prefix defaults (SPEC-launcher 5.6).
pub(crate) const LAUNCHER_PREFIXES: [(&str, &str); 8] = [
    ("=", "calc"),
    (">", "run"),
    ("<", "windows"),
    ("??", "web"),
    ("$", "settings"),
    (")", "datetime"),
    (":", "registry"),
    ("file ", "files"),
];

/// The values of the launcher's closed lists, the default first.
const LAUNCHER_CHORDS: [&str; 3] = ["off", "alt+space", "alt+shift+space"];
const LAUNCHER_WITH_POWERTOYS: [&str; 2] = ["yield", "take"];
const LAUNCHER_MONITORS: [&str; 3] = ["pointer", "active", "primary"];
const LAUNCHER_OVER_FULLSCREEN: [&str; 2] = ["pass", "open"];
const LAUNCHER_WEB_ENGINES: [&str; 4] = ["google", "bing", "duckduckgo", "custom"];

/// `launcher` section of the config (SPEC-launcher 5.6). Plain data: nothing here names a program, a path or a verb.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct LauncherConfig {
    pub enabled: bool,
    /// "off", "alt+space" or "alt+shift+space".
    pub chord: String,
    /// "yield" or "take": what the chord does while PowerToys Command Palette or PowerToys Run runs (SPEC-launcher 5.7,
    /// `launcher::powertoys`). Only with the chord "alt+space".
    pub with_powertoys: String,
    /// "pointer", "active" or "primary".
    pub monitor: String,
    /// Distance of the panel from the top of the display, percent of its height (10..=50).
    pub top_percent: u8,
    /// Panel width in logical px (560..=880, multiples of 40).
    pub width: u16,
    /// Visible result rows (4..=12).
    pub rows: u8,
    pub animate: bool,
    pub keep_query: bool,
    /// "pass" or "open".
    pub over_fullscreen: String,
    pub learn: bool,
    pub announce_selection: bool,
    pub providers: BTreeMap<String, bool>,
    pub prefixes: BTreeMap<String, String>,
    /// Words that open something when typed alone (plan L2 D9): word to target. Kept as written, `sanitize` only caps
    /// it; `launcher::aliases::AliasTable` ignores (and counts) the entries it refuses, so a save never deletes them.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub aliases: BTreeMap<String, String>,
    /// Folders of the user's own that file search covers too (SPEC 5.4.6): at most 8 are read, a drive root allowed.
    /// Kept as written: the dock ignores (and counts in the log) the entries `files::root_of` refuses, so a save never
    /// deletes them.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub file_folders: Vec<String>,
    /// "google", "bing", "duckduckgo" or "custom".
    pub web_engine: String,
    /// The search URL template of the "custom" engine, "" otherwise.
    pub web_url: String,
    /// true once the one-time launcher offer in the Start menu (SPEC-launcher 5.2.1) was answered: a chord taken from
    /// it or "Don't show again". The offer is shown only while this is false and the chord is "off".
    pub offer_done: bool,
    /// true once the launcher's PowerToys notice (SPEC-launcher 5.7) was answered: any choice, or "Don't show again". A
    /// chord owner found later is then yielded to (or taken, with "take") without asking again.
    pub powertoys_done: bool,
}

pub const LAUNCHER_TOP_PERCENT: (u8, u8, u8) = (10, 50, 24);
pub const LAUNCHER_WIDTH: (u16, u16, u16) = (560, 880, 640);
pub const LAUNCHER_ROWS: (u8, u8, u8) = (4, 12, 8);
/// `launcher.aliases`: at most this many entries are kept, each word and target of at most `ALIAS_TEXT_CAP` characters.
pub const ALIAS_ENTRIES_CAP: usize = 256;
pub const ALIAS_TEXT_CAP: usize = 1_024;

impl Default for LauncherConfig {
    fn default() -> Self {
        LauncherConfig {
            enabled: true,
            chord: "off".into(),
            with_powertoys: "yield".into(),
            monitor: "pointer".into(),
            top_percent: LAUNCHER_TOP_PERCENT.2,
            width: LAUNCHER_WIDTH.2,
            rows: LAUNCHER_ROWS.2,
            animate: false,
            keep_query: false,
            over_fullscreen: "pass".into(),
            learn: true,
            announce_selection: false,
            providers: LAUNCHER_PROVIDERS.iter().fold(BTreeMap::new(), |mut m, p| {
                m.insert(p.to_string(), true);
                m
            }),
            prefixes: LAUNCHER_PREFIXES.iter().fold(BTreeMap::new(), |mut m, (p, n)| {
                m.insert(p.to_string(), n.to_string());
                m
            }),
            aliases: BTreeMap::new(),
            file_folders: Vec::new(),
            web_engine: "google".into(),
            web_url: String::new(),
            offer_done: false,
            powertoys_done: false,
        }
    }
}

/// Puts `v` back to `default` unless it is one of `allowed`; the message names the key.
fn fix_choice(v: &mut String, allowed: &[&str], default: &str, key: &str, out: &mut Vec<String>) {
    if !allowed.contains(&v.as_str()) {
        out.push(format!("launcher.{key}: unknown value, using \"{default}\""));
        *v = default.to_string();
    }
}

impl LauncherConfig {
    /// Brings every key into its closed list or range and returns one message per fix. Never runs anything.
    pub fn sanitize(&mut self) -> Vec<String> {
        let mut out = Vec::new();
        fix_choice(&mut self.chord, &LAUNCHER_CHORDS, "off", "chord", &mut out);
        fix_choice(&mut self.with_powertoys, &LAUNCHER_WITH_POWERTOYS, "yield", "with_powertoys", &mut out);
        fix_choice(&mut self.monitor, &LAUNCHER_MONITORS, "pointer", "monitor", &mut out);
        fix_choice(&mut self.over_fullscreen, &LAUNCHER_OVER_FULLSCREEN, "pass", "over_fullscreen", &mut out);
        let (lo, hi, _) = LAUNCHER_TOP_PERCENT;
        let v = self.top_percent.clamp(lo, hi);
        if v != self.top_percent {
            out.push("launcher.top_percent: out of range, clamped".into());
            self.top_percent = v;
        }
        let (lo, hi, _) = LAUNCHER_ROWS;
        let v = self.rows.clamp(lo, hi);
        if v != self.rows {
            out.push("launcher.rows: out of range, clamped".into());
            self.rows = v;
        }
        let (lo, hi, _) = LAUNCHER_WIDTH;
        let v = (self.width.clamp(lo, hi) + 20) / 40 * 40;
        let v = v.clamp(lo, hi);
        if v != self.width {
            out.push("launcher.width: out of range or not a multiple of 40, adjusted".into());
            self.width = v;
        }
        // providers: unknown names dropped, apps always on, a missing one stays at its default (on)
        let before = self.providers.len();
        self.providers.retain(|k, _| crate::launcher::model::ProviderId::from_name(k).is_some());
        if self.providers.len() != before {
            out.push("launcher.providers: unknown names dropped".into());
        }
        for p in LAUNCHER_PROVIDERS {
            self.providers.entry(p.to_string()).or_insert(true);
        }
        if self.providers.get("apps") != Some(&true) {
            out.push("launcher.providers.apps: always on".into());
            self.providers.insert("apps".into(), true);
        }
        // prefixes: 1-5 characters, known provider, unique, a word prefix ends with a space
        // sorted, each prefix once (lower case)
        let mut seen: Vec<String> = Vec::new();
        let mut dropped = false;
        self.prefixes.retain(|p, name| {
            let chars = p.chars().count();
            let closed = !p.chars().any(char::is_alphabetic) || p.ends_with(' ');
            // invisible prefixes (whitespace only, control or bidi/format marks) cannot be typed or seen
            let visible = !p.trim().is_empty()
                && !p.chars().any(|c| c.is_control() || matches!(c, '\u{200b}'..='\u{200f}' | '\u{202a}'..='\u{202e}' | '\u{2060}'..='\u{2069}' | '\u{feff}'));
            let ok = (1..=5).contains(&chars)
                && closed
                && visible
                && crate::launcher::model::ProviderId::from_name(name).is_some()
                && {
                    let lower = p.to_ascii_lowercase();
                    let at = seen.binary_search(&lower);
                    if let Err(at) = at {
                        seen.insert(at, lower);
                    }
                    at.is_err()
                };
            dropped |= !ok;
            ok
        });
        if dropped {
            out.push("launcher.prefixes: invalid entries dropped".into());
        }
        // aliases: kept as written (the alias table ignores and counts what it refuses, plan L2 D9); only the hard caps,
        // so a file that is far too large cannot make the table's build long. The message names no word and no target.
        let before = self.aliases.len();
        self.aliases.retain(|w, t| w.chars().count() <= ALIAS_TEXT_CAP && t.chars().count() <= ALIAS_TEXT_CAP);
        if let Some(first_over) = self.aliases.keys().nth(ALIAS_ENTRIES_CAP).cloned() {
            self.aliases.split_off(&first_over);
        }
        let over = before - self.aliases.len();
        if over > 0 {
            out.push(format!("launcher.aliases: {over} entries over a cap dropped"));
        }
        // web: a custom template must pass the validator, else Google
        fix_choice(&mut self.web_engine, &LAUNCHER_WEB_ENGINES, "google", "web_engine", &mut out);
        if self.web_engine == "custom" {
            if let Err(why) = crate::launcher::web::validate_template(&self.web_url) {
                out.push(format!("launcher.web_url: {why}, using google"));
                self.web_engine = "google".into();
                self.web_url.clear();
            }
        }
        out
    }

    /// The chord to hook; None when the launcher is disabled or the chord is "off".
    pub fn chord_spec(&self) -> Option<crate::launcher::model::ChordSpec> {
        if !self.enabled {
            return None;
        }
        crate::launcher::model::ChordSpec::parse(&self.chord)
    }
}

/// Default opacity of the bar background (percent).
pub const DEFAULT_BAR_OPACITY: u8 = 95;

/// Smallest Settings window client area (logical px at 96 DPI).
pub const SETTINGS_WINDOW_MIN: (i32, i32) = (640, 460);
/// Default Settings window client area (logical px at 96 DPI).
pub const SETTINGS_WINDOW_DEFAULT: (i32, i32) = (860, 600);
/// Largest sane Settings window dimension; anything above is treated as corrupt.
const SETTINGS_WINDOW_MAX: u16 = 16384;

/// An app pinned to the dock.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq, Default)]
#[serde(default)]
pub struct PinnedApp {
    /// Grouping key (AUMID or normalized lowercase exe path).
    pub key: String,
    /// Display name.
    pub name: String,
    /// Launch target: exe path or `shell:AppsFolder\<AUMID>`.
    pub launch: String,
    /// Optional custom icon: plain file name inside `%ProgramData%\mini-oled-dock\icons\`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub icon: Option<String>,
}

/// A folder of the Start menu pinned grid (`Config::start_folders`).
#[derive(Serialize, Deserialize, Clone, Debug, Default, PartialEq, Eq)]
#[serde(default)]
pub struct StartFolder {
    pub name: String,
    /// AppsFolder ids of the pinned apps inside, in order.
    pub apps: Vec<String>,
}

// The kinds of the fields shown in Settings (`Descriptor::kind`).
impl Kind for Language {
    const KIND: ValueType = ValueType::Enum;
}

impl Kind for Position {
    const KIND: ValueType = ValueType::Enum;
}

impl Kind for DisplayMode {
    const KIND: ValueType = ValueType::Enum;
}

impl Kind for Reveal {
    const KIND: ValueType = ValueType::Enum;
}

impl Kind for StartStyle {
    const KIND: ValueType = ValueType::Enum;
}

impl Kind for BatteryShow {
    const KIND: ValueType = ValueType::Enum;
}

impl Kind for Background {
    const KIND: ValueType = ValueType::Color;
}

impl Kind for KeyboardConfig {
    const KIND: ValueType = ValueType::Object;
}

impl Kind for LauncherConfig {
    const KIND: ValueType = ValueType::Object;
}

/// The first two lines of the auto-hide guide (`timing_guide`): the two delays, the value a fresh install writes and
/// the value of a missing key in one sentence.
macro_rules! timing_guide_head {
    () => {
        "Auto-hide timing (ms): show_delay_ms 0-1000 at the screen edge (default 150),\n\
         hide_delay_ms 0-5000 after the pointer leaves the bar (a new install writes 1000; a missing key reads 2500).\n"
    };
}

/// The guide line of `reveal` and `push_threshold` (`reveal_guide`): while the Push reveal is not shipped nothing reads
/// the two keys.
macro_rules! reveal_guide_line {
    (true) => {
        "reveal: \"dwell\" (rest the pointer at the edge) or \"push\" \
         (push against the edge, push_threshold 20-1000 counts)."
    };
    (false) => {
        "reveal and push_threshold are not used yet: the bar always shows on a rest at the edge. \
         The keys stay for a later release."
    };
}

/// The guide line of `reveal` and `push_threshold`, as the file writes it (`timing_guide`'s last line).
#[cfg(test)]
fn reveal_guide(push_available: bool) -> &'static str {
    if push_available {
        reveal_guide_line!(true)
    } else {
        reveal_guide_line!(false)
    }
}

/// The guide above `show_delay_ms`, `hide_delay_ms`, `reveal` and `push_threshold`.
fn timing_guide(push_available: bool) -> &'static str {
    if push_available {
        concat!(timing_guide_head!(), reveal_guide_line!(true))
    } else {
        concat!(timing_guide_head!(), reveal_guide_line!(false))
    }
}

options! {
    /// Contents of `config.json`. Missing fields = defaults (`#[serde(default)]`). Declared once: the struct in its
    /// field order (serde's, so the snapshots'), and per field its place in the written file (`slot`), how it is
    /// written, its guide and, for an option Settings shows, its descriptor.
    #[derive(Eq)]
    pub struct Config(
        file: "config.json",
        version: 1,
        mark: TEMPLATE_MARK,
        header: HEADER,
        migrations: &[migrate_v0],
    ) {
        /// Language of the dock: "auto" (Windows display language), "en" or "it" (`i18n`).
        pub language: Language = Language::Auto => {
            json: "language",
            slot: 0,
            fmt: Js,
            gap: false,
            guide: "Language of the dock: \"auto\" (follows the Windows display language), \"en\" or \"it\".\n\
                 Also sets the language of the clock, date and calendar names. Normally chosen in Settings.",
            since: 1,
            reason: "The dock speaks English or Italian whatever the Windows display language is.",
            ui: Some {
                page: PageId("appearance"),
                section: SecId("language"),
                order: 7,
                control: Control::Segmented(&[(|| t(k::SETTINGS_PAGES_SYSTEM), "auto"), (|| "English", "en"), (|| "Italiano", "it")]),
                label: || t(k::SETTINGS_PAGES_LANGUAGE),
                help: || "",
                glyph: "globe_shape",
                keywords: &[],
                range: None,
                scope: Scope::Global,
                profileable: false,
                visible_when: None,
                cost: None,
                takeover: None,
            },
        },
        pub position: Position = Position::Bottom => {
            json: "position",
            slot: 1,
            fmt: Js,
            gap: true,
            guide: "Where the bar sits on screen: \"top\" or \"bottom\".",
            since: 1,
            reason: "Some people want the bar at the top of the screen.",
            ui: Some {
                page: PageId("bar"),
                section: SecId("position"),
                order: 1,
                control: Control::Segmented(&[(|| t(k::SETTINGS_PAGES_TOP), "top"), (|| t(k::SETTINGS_PAGES_BOTTOM), "bottom")]),
                label: || t(k::SETTINGS_PAGES_POSITION),
                help: || "",
                glyph: "bar_shape",
                keywords: &[],
                range: None,
                scope: Scope::Global,
                profileable: false,
                visible_when: None,
                cost: None,
                takeover: None,
            },
        },
        /// Display the bar sits on: empty = the primary monitor (default), otherwise a GDI device name
        /// such as "\\.\DISPLAY2". A display that is not connected falls back to the primary one.
        /// `MONITOR_POINTER` ("pointer") = the Win key brings the bar to the display under the pointer.
        pub monitor: String = String::new() => {
            json: "monitor",
            slot: 2,
            fmt: Js,
            gap: true,
            guide: "Display the bar sits on: \"\" = the primary monitor, or a device name such as \"\\\\\\\\.\\\\DISPLAY2\".\n\
                 Normally chosen in Settings -> Screen (shown with two or more displays). If that display is\n\
                 not connected the bar uses the primary monitor, and moves back when it returns.\n\
                 \"pointer\" = the Win key shows the bar (and a double tap the Start menu) on the display under the pointer.",
            since: 1,
            reason: "With several displays the bar can sit on another one, or follow the pointer.",
            ui: Some {
                page: PageId("screen"),
                section: SecId("display"),
                order: 1,
                control: Control::Custom("screens"),
                label: || t(k::COMMON_DISPLAY),
                help: || t(k::SETTINGS_PAGES_POINTER_THE_WIN_KEY_BRINGS),
                glyph: "monitor_shape",
                keywords: &[],
                range: None,
                scope: Scope::Global,
                profileable: false,
                visible_when: None,
                cost: None,
                takeover: None,
            },
        },
        pub autohide: bool = true => {
            json: "autohide",
            slot: 11,
            fmt: Raw,
            gap: true,
            guide: "Hide the bar when the pointer leaves it, show it when you touch the screen edge.",
            since: 1,
            reason: "The bar takes no screen space until the pointer reaches the edge.",
            ui: Some {
                page: PageId("bar"),
                section: SecId("behaviour"),
                order: 3,
                control: Control::Toggle,
                label: || t(k::COMMON_AUTO_HIDE),
                help: || t(k::SETTINGS_PAGES_SHOWS_UP_WITH_THE_WIN),
                glyph: "chevron_down_shape",
                keywords: &[],
                range: None,
                scope: Scope::Global,
                profileable: false,
                visible_when: None,
                cost: None,
                takeover: None,
            },
        },
        /// Auto-hide: ms the pointer may be off the bar before it hides, 0..=5000 (`HIDE_DELAY_RANGE`).
        /// A missing key reads 2500; a fresh install writes 1000.
        pub hide_delay_ms: u32 = LEGACY_HIDE_DELAY_MS => {
            json: "hide_delay_ms",
            slot: 13,
            fmt: Raw,
            gap: false,
            guide: "",
            since: 1,
            reason: "How long the bar waits before it hides suits the hand that uses it.",
            ui: Some {
                page: PageId("bar"),
                section: SecId("behaviour"),
                order: 5,
                control: Control::Slider,
                label: || t(k::SETTINGS_PAGES_HIDE_DELAY),
                help: || t(k::SETTINGS_PAGES_AFTER_THE_POINTER_LEAVES_THE),
                glyph: "clock_shape",
                keywords: &[],
                range: Some(Range { min: HIDE_DELAY_RANGE.0 as i64, max: HIDE_DELAY_RANGE.1 as i64, step: 100, unit: Unit::Ms }),
                scope: Scope::Global,
                profileable: false,
                visible_when: None,
                cost: None,
                takeover: None,
            },
        },
        /// Auto-hide: ms the pointer rests at the screen edge before the bar shows, 0..=1000.
        pub show_delay_ms: u32 = DEFAULT_SHOW_DELAY_MS => {
            json: "show_delay_ms",
            slot: 12,
            fmt: Raw,
            gap: true,
            guide: timing_guide(crate::ui::PUSH_AVAILABLE),
            since: 1,
            reason: "A short rest at the edge keeps the bar from showing by accident.",
            ui: Some {
                page: PageId("bar"),
                section: SecId("behaviour"),
                order: 4,
                control: Control::Slider,
                label: || t(k::SETTINGS_PAGES_SHOW_DELAY),
                help: || t(k::SETTINGS_PAGES_POINTER_RESTING_AT_THE_SCREEN),
                glyph: "clock_shape",
                keywords: &[],
                range: Some(Range { min: SHOW_DELAY_RANGE.0 as i64, max: SHOW_DELAY_RANGE.1 as i64, step: 50, unit: Unit::Ms }),
                scope: Scope::Global,
                profileable: false,
                visible_when: None,
                cost: None,
                takeover: None,
            },
        },
        /// How the hidden bar is revealed at the edge.
        pub reveal: Reveal = Reveal::Dwell => {
            json: "reveal",
            slot: 14,
            fmt: Js,
            gap: false,
            guide: "",
            since: 1,
            reason: "Pushing against the edge suits people who rest the pointer there for other reasons.",
            ui: Some {
                page: PageId("bar"),
                section: SecId("behaviour"),
                order: 6,
                control: Control::Segmented(&[(|| t(k::SETTINGS_PAGES_REST), "dwell"), (|| t(k::SETTINGS_PAGES_PUSH), "push")]),
                label: || t(k::SETTINGS_PAGES_SHOW_AT_THE_EDGE),
                help: || "",
                glyph: "chevron_down_shape",
                keywords: &[],
                range: None,
                scope: Scope::Global,
                profileable: false,
                visible_when: None,
                cost: None,
                takeover: None,
            },
        },
        /// Push reveal: raw pointer counts within one second that count as a push, 20..=1000.
        #[serde(deserialize_with = "read_push_threshold")]
        pub push_threshold: u32 = DEFAULT_PUSH_THRESHOLD => {
            json: "push_threshold",
            slot: 15,
            fmt: Raw,
            gap: false,
            guide: "",
            since: 1,
            reason: "How hard a push must be depends on the mouse and its speed.",
            ui: None,
        },
        /// Owned keyboard chords.
        pub keyboard: KeyboardConfig = KeyboardConfig::default() => {
            json: "keyboard",
            slot: 16,
            fmt: Js,
            gap: true,
            guide: "Keyboard: win_numbers = Win+1..9/0 (also with Shift and Alt), win_t = Win+T and Win+Shift+T, win_b = Win+B,\n\
                 peek = digit chips while Win is held. Owned only while hide_native_taskbar is true.",
            since: 1,
            reason: "The Win key chords are the dock's only while the native taskbar is hidden; some people want Windows' own.",
            ui: Some {
                page: PageId("bar"),
                section: SecId("behaviour"),
                order: 10,
                control: Control::Toggle,
                label: || t(k::SETTINGS_PAGES_KEYBOARD_SHORTCUTS),
                help: || t(k::SETTINGS_PAGES_WIN_1_9_OPEN_THE),
                glyph: "keycap_shape",
                keywords: &[],
                range: None,
                scope: Scope::Global,
                profileable: false,
                visible_when: None,
                cost: None,
                takeover: None,
            },
        },
        /// true once the privacy-indicator migration (`migrate_privacy`) has run for this file. A file without
        /// the key reads false (so the migration runs); a config built in memory has nothing to migrate.
        #[serde(default = "no_privacy_migration_yet")]
        pub tray_privacy_migrated: bool = true => {
            json: "tray_privacy_migrated",
            slot: 42,
            fmt: Raw,
            gap: true,
            guide: "Internal: set once the privacy indicators (microphone, location) were let out of the list above. Leave as is.",
            since: 1,
            reason: "Internal: the privacy indicators leave the hidden list once, never again after the user's own choice.",
            ui: None,
        },
        /// Window picker with live DWM thumbnails when the pointer rests on an app with 2+ windows
        /// (`picker`). false = no hover timer is armed and no picker window is ever created.
        pub window_previews: bool = true => {
            json: "window_previews",
            slot: 17,
            fmt: Raw,
            gap: true,
            guide: "Resting the pointer on an app with several windows shows a live preview of each one (with its\n\
                 title); click a preview to switch to that window. false = no previews and no extra window.",
            since: 1,
            reason: "Live previews cost a window and a timer; some people never need them.",
            ui: Some {
                page: PageId("bar"),
                section: SecId("behaviour"),
                order: 7,
                control: Control::Toggle,
                label: || t(k::SETTINGS_PAGES_WINDOW_PREVIEWS),
                help: || t(k::SETTINGS_PAGES_REST_THE_POINTER_ON_AN),
                glyph: "eye_icon",
                keywords: &[],
                range: None,
                scope: Scope::Global,
                profileable: false,
                visible_when: None,
                cost: None,
                takeover: None,
            },
        },
        /// Every Win tap opens / closes the Start menu together with the bar, as one show/hide
        /// animation (`ui::start_with_bar`). false = the original behaviour: Win toggles the bar,
        /// double tap = Start.
        pub win_opens_start: bool = false => {
            json: "win_opens_start",
            slot: 18,
            fmt: Raw,
            gap: true,
            guide: "true = the Win key opens the Start menu together with the bar and the next tap closes both\n\
                 (one animation). false = the Win key shows / hides the bar and a double tap opens the Start menu.",
            since: 1,
            reason: "One tap of Win shows the bar and the Start menu together, as on the native taskbar.",
            ui: Some {
                page: PageId("start"),
                section: SecId("start_menu"),
                order: 3,
                control: Control::Toggle,
                label: || t(k::SETTINGS_PAGES_WIN_KEY_OPENS_START),
                help: || t(k::SETTINGS_PAGES_TOGETHER_WITH_THE_BAR),
                glyph: "keycap_shape",
                keywords: &[],
                range: None,
                scope: Scope::Global,
                profileable: false,
                visible_when: None,
                cost: None,
                takeover: None,
            },
        },
        pub hide_native_taskbar: bool = true => {
            json: "hide_native_taskbar",
            slot: 21,
            fmt: Raw,
            gap: true,
            guide: "Hide Windows' own taskbar while the dock runs (restored when the dock exits).",
            since: 1,
            reason: "The dock replaces the taskbar; keeping both is for trying the dock out.",
            ui: None,
        },
        pub pinned: Vec<PinnedApp> = Vec::new() => {
            json: "pinned",
            slot: 43,
            fmt: Block,
            gap: true,
            guide: "Pinned apps, in order. Normally managed with Pin/Unpin in the app menu. Fields:\n\
                 \x20 \"key\"    grouping key: the AUMID, or the lowercase exe path\n\
                 \x20 \"name\"   fallback label, refreshed automatically from the app itself; use the \"names\"\n\
                 \x20          map to set a fixed label\n\
                 \x20 \"launch\" exe path, or \"shell:AppsFolder\\\\<AUMID>\" for Store/packaged apps\n\
                 \x20 \"icon\"   optional custom icon: plain PNG file name inside %ProgramData%\\mini-oled-dock\\icons\\\n\
                 \x20          (e.g. \"discord.png\"; paths, \"..\", drive letters and UNC paths are rejected)\n\
                 List the AUMIDs of installed apps with:  Get-StartApps   (PowerShell)",
            since: 1,
            reason: "The apps pinned to the bar, in order.",
            ui: None,
        },
        /// Apps pinned at the top of the Start menu opened by the Win double tap (`popup_startmenu`),
        /// in order: AppsFolder ids (`shell_thread::StartApp::id`). Separate from the bar's `pinned`.
        pub start_pins: Vec<String> = Vec::new() => {
            json: "start_pins",
            slot: 44,
            fmt: Block,
            gap: true,
            guide: "Apps pinned at the top of the Start menu that a double tap of the Win key opens (separate from\n\
                 the bar pins above). Normally managed with a right click on an app in that menu. Each entry is\n\
                 the app's id inside shell:AppsFolder (an AUMID for Store apps); see:  Get-StartApps  (PowerShell)",
            since: 1,
            reason: "The apps pinned in the dock's Start menu, in order.",
            ui: None,
        },
        /// Folders of that pinned grid: each groups some of the `start_pins` under one tile, shown where
        /// its first pinned app is. An app listed here but not pinned is ignored.
        pub start_folders: Vec<StartFolder> = Vec::new() => {
            json: "start_folders",
            slot: 45,
            fmt: Block,
            gap: true,
            guide: "Folders of those pins: each one shows some of the pinned apps under a single tile, placed where\n\
                 its first app is. Normally managed by dragging a pinned app onto another one in the menu.\n\
                 Example: [{ \"name\": \"Games\", \"apps\": [\"<id>\", \"<id>\"] }]",
            since: 1,
            reason: "Folders group pinned apps under one tile.",
            ui: None,
        },
        /// Layout of that Start menu: "compact" (default) or "windows11" (`StartStyle`).
        pub start_style: StartStyle = StartStyle::Compact => {
            json: "start_style",
            slot: 19,
            fmt: Js,
            gap: true,
            guide: "Layout of the dock's Start menu: \"compact\" (a quick launcher: search, one strip of pinned icons,\n\
                 the list of all apps) or \"windows11\" (the full menu: one scrolling page with the pinned tiles\n\
                 and all apps, user name and folder shortcuts at the bottom). Normally chosen in Settings. The old names \"vertical\" and\n\
                 \"horizontal\" are still read; other values = \"compact\".",
            since: 1,
            reason: "A quick launcher or the full Windows 11 layout.",
            ui: Some {
                page: PageId("start"),
                section: SecId("start_menu"),
                order: 2,
                control: Control::Segmented(&[(|| t(k::SETTINGS_PAGES_COMPACT), "compact"), (|| t(k::SETTINGS_PAGES_WINDOWS_11), "windows11")]),
                label: || t(k::SETTINGS_PAGES_START_LAYOUT),
                help: || "",
                glyph: "grid_shape",
                keywords: &[],
                range: None,
                scope: Scope::Global,
                profileable: false,
                visible_when: None,
                cost: None,
                takeover: None,
            },
        },
        /// Windows 11 layout: "All apps" as a grid of icons instead of a list.
        pub start_grid: bool = false => {
            json: "start_grid",
            slot: 20,
            fmt: Raw,
            gap: true,
            guide: "\"All apps\" in the \"windows11\" layout: false = a list, true = a grid of icons. Normally chosen\n\
                 with the button next to \"All apps\" in the menu.",
            since: 1,
            reason: "All apps as a grid of icons or as a list.",
            ui: None,
        },
        /// Name overrides: key = lowercase exe file name (e.g. "code.exe") or AUMID;
        /// value = name to show.
        pub names: BTreeMap<String, String> = BTreeMap::new() => {
            json: "names",
            slot: 49,
            fmt: Block,
            gap: true,
            guide: "Display-name overrides. Key = exe file name lowercase (e.g. \"code.exe\") or AUMID;\n\
                 value = the name to show. Example: { \"code.exe\": \"VS Code\" }",
            since: 1,
            reason: "A name of one's own for an app whose own name is long or unclear.",
            ui: None,
        },
        pub show_date: bool = true => {
            json: "show_date",
            slot: 22,
            fmt: Raw,
            gap: true,
            guide: "Show the date next to the clock.",
            since: 1,
            reason: "The date next to the clock takes room some people need.",
            ui: None,
        },
        pub show_volume_percent: bool = true => {
            json: "show_volume_percent",
            slot: 26,
            fmt: Raw,
            gap: true,
            guide: "Show the percentage next to the volume icon.",
            since: 1,
            reason: "The number next to the speaker is a matter of taste.",
            ui: Some {
                page: PageId("bar"),
                section: SecId("behaviour"),
                order: 9,
                control: Control::Toggle,
                label: || t(k::SETTINGS_PAGES_VOLUME_PERCENT),
                help: || t(k::SETTINGS_PAGES_NEXT_TO_THE_SPEAKER),
                glyph: "speaker_icon",
                keywords: &[],
                range: None,
                scope: Scope::Global,
                profileable: false,
                visible_when: None,
                cost: None,
                takeover: None,
            },
        },
        /// hosts the tray icons of other apps in the dock (`tray_host`). false = never started.
        pub show_tray: bool = true => {
            json: "show_tray",
            slot: 27,
            fmt: Raw,
            gap: true,
            guide: "Host other apps' tray icons inside the dock. All visible icons are shown (no limit);\n\
                 the bar widens and narrows dynamically.",
            since: 1,
            reason: "The tray icons of other apps, hosted in the bar.",
            ui: Some {
                page: PageId("modules"),
                section: SecId("tray"),
                order: 10,
                control: Control::Toggle,
                label: || t(k::SETTINGS_PAGES_TRAY_ICONS),
                help: || "",
                glyph: "ring_shape",
                keywords: &[],
                range: None,
                scope: Scope::Global,
                profileable: false,
                visible_when: None,
                cost: None,
                takeover: None,
            },
        },
        /// true = every tray icon is shown in the bar and `tray_hidden` is ignored (kept in the file untouched, so
        /// switching this off brings the earlier choices back). Default: false.
        pub tray_show_all: bool = false => {
            json: "tray_show_all",
            slot: 28,
            fmt: Raw,
            gap: true,
            guide: "true = show every tray icon in the bar, without choosing them one by one. \"tray_hidden\" is then ignored\n\
                 (but kept as it is, so switching this off brings your earlier choices back). Icons that appear later are\n\
                 shown too.",
            since: 1,
            reason: "Every tray icon in the bar without choosing them one by one.",
            ui: Some {
                page: PageId("modules"),
                section: SecId("tray"),
                order: 11,
                control: Control::Toggle,
                label: || t(k::SETTINGS_PAGES_SHOW_ALL),
                help: || t(k::SETTINGS_PAGES_SHOW_EVERY_TRAY_ICON_IN),
                glyph: "eye_icon",
                keywords: &[],
                range: None,
                scope: Scope::Global,
                profileable: false,
                visible_when: Some(Cond { json: "show_tray", equals: "true" }),
                cost: None,
                takeover: None,
            },
        },
        /// Per-module visibility of the system modules (see `MODULE_NAMES` / `module_visible`).
        /// A hidden module is not laid out and costs nothing: no timer, no polling, nothing loaded.
        pub show_notifications: bool = true => {
            json: "show_notifications",
            slot: 29,
            fmt: Raw,
            gap: true,
            guide: "System modules on the right of the bar: true = shown, false = removed from the bar.\n\
                 A hidden module costs nothing (no timer, no polling, no system library loaded).\n\
                 The power button is always shown (it holds the dock's own menu).",
            since: 1,
            reason: "A module nobody uses costs nothing when it is hidden.",
            ui: Some {
                page: PageId("modules"),
                section: SecId("modules"),
                order: 1,
                control: Control::Toggle,
                label: || t(k::COMMON_NOTIFICATIONS),
                help: || "",
                glyph: "bell_shape",
                keywords: &[],
                range: None,
                scope: Scope::Global,
                profileable: false,
                visible_when: None,
                cost: None,
                takeover: None,
            },
        },
        pub show_volume: bool = true => {
            json: "show_volume",
            slot: 30,
            fmt: Raw,
            gap: false,
            guide: "",
            since: 1,
            reason: "A module nobody uses costs nothing when it is hidden.",
            ui: Some {
                page: PageId("modules"),
                section: SecId("modules"),
                order: 2,
                control: Control::Toggle,
                label: || t(k::SETTINGS_PAGES_VOLUME),
                help: || "",
                glyph: "speaker_icon",
                keywords: &[],
                range: None,
                scope: Scope::Global,
                profileable: false,
                visible_when: None,
                cost: None,
                takeover: None,
            },
        },
        /// Network module: Wi-Fi / ethernet glyph with internet state, popup with the details (`net`).
        pub show_network: bool = true => {
            json: "show_network",
            slot: 31,
            fmt: Raw,
            gap: true,
            guide: "Network: Wi-Fi signal bars or ethernet glyph, with a small globe when the internet is reachable\n\
                 and a small x when it is not. Click for the connection details. Refreshed every ~6 s while\n\
                 the bar is visible.",
            since: 1,
            reason: "A module nobody uses costs nothing when it is hidden.",
            ui: Some {
                page: PageId("modules"),
                section: SecId("modules"),
                order: 3,
                control: Control::Toggle,
                label: || t(k::SETTINGS_PAGES_NETWORK),
                help: || "",
                glyph: "wifi_icon",
                keywords: &[],
                range: None,
                scope: Scope::Global,
                profileable: false,
                visible_when: None,
                cost: None,
                takeover: None,
            },
        },
        /// CPU / RAM usage module (`metrics`).
        pub show_metrics: bool = false => {
            json: "show_metrics",
            slot: 32,
            fmt: Raw,
            gap: true,
            guide: "CPU and RAM usage in percent (two small lines). Refreshed every 2 s while the bar is visible.\n\
                 Click to open Task Manager.",
            since: 1,
            reason: "CPU and RAM at a glance, off by default.",
            ui: Some {
                page: PageId("modules"),
                section: SecId("modules"),
                order: 4,
                control: Control::Toggle,
                label: || t(k::SETTINGS_PAGES_SYSTEM_METRICS),
                help: || "",
                glyph: "bars_shape",
                keywords: &[],
                range: None,
                scope: Scope::Global,
                profileable: false,
                visible_when: None,
                cost: None,
                takeover: None,
            },
        },
        /// Battery module: "auto" (only on a laptop) | true | false (`BatteryShow`).
        pub show_battery: BatteryShow = BatteryShow::Auto => {
            json: "show_battery",
            slot: 33,
            fmt: Js,
            gap: true,
            guide: "Battery: an Apple-style icon filled in proportion to the charge, with a bolt while charging.\n\
                 Click for the details. Refreshed every 30 s while the bar is visible, and at once when the\n\
                 charger is plugged or unplugged.\n\
                 \x20 show_battery            \"auto\" (only on a laptop) | true (always; an icon with a slash when there\n\
                 \x20                         is no battery) | false\n\
                 \x20 battery_percent         show the number\n\
                 \x20 battery_percent_inside  put the number inside the battery body (Apple style) instead of beside it",
            since: 1,
            reason: "The battery only matters on a laptop.",
            ui: Some {
                page: PageId("modules"),
                section: SecId("battery"),
                order: 7,
                control: Control::Segmented(&[(|| t(k::SETTINGS_PAGES_AUTO), "auto"), (|| t(k::SETTINGS_PAGES_ON), "true"), (|| t(k::SETTINGS_PAGES_OFF), "false")]),
                label: || t(k::SETTINGS_PAGES_SHOW_BATTERY),
                help: || t(k::SETTINGS_PAGES_SHOWN_AUTOMATICALLY_ON_LAPTOPS),
                glyph: "battery_icon",
                keywords: &[],
                range: None,
                scope: Scope::Global,
                profileable: false,
                visible_when: None,
                cost: None,
                takeover: None,
            },
        },
        /// Battery module: show the charge percentage.
        pub battery_percent: bool = true => {
            json: "battery_percent",
            slot: 34,
            fmt: Raw,
            gap: false,
            guide: "",
            since: 1,
            reason: "The charge as a number beside the battery.",
            ui: Some {
                page: PageId("modules"),
                section: SecId("battery"),
                order: 8,
                control: Control::Toggle,
                label: || t(k::SETTINGS_PAGES_PERCENTAGE),
                help: || "",
                glyph: "text_shape",
                keywords: &[],
                range: None,
                scope: Scope::Global,
                profileable: false,
                visible_when: None,
                cost: None,
                takeover: None,
            },
        },
        /// Battery module: put the percentage inside the battery body instead of beside it.
        pub battery_percent_inside: bool = false => {
            json: "battery_percent_inside",
            slot: 35,
            fmt: Raw,
            gap: false,
            guide: "",
            since: 1,
            reason: "The number inside the battery body, as on a Mac.",
            ui: Some {
                page: PageId("modules"),
                section: SecId("battery"),
                order: 9,
                control: Control::Toggle,
                label: || t(k::SETTINGS_PAGES_PERCENTAGE_INSIDE_THE_ICON),
                help: || t(k::SETTINGS_PAGES_APPLE_STYLE),
                glyph: "battery_icon",
                keywords: &[],
                range: None,
                scope: Scope::Global,
                profileable: false,
                visible_when: None,
                cost: None,
                takeover: None,
            },
        },
        pub show_clock: bool = true => {
            json: "show_clock",
            slot: 36,
            fmt: Raw,
            gap: true,
            guide: "",
            since: 1,
            reason: "A module nobody uses costs nothing when it is hidden.",
            ui: Some {
                page: PageId("modules"),
                section: SecId("modules"),
                order: 5,
                control: Control::Toggle,
                label: || t(k::SETTINGS_PAGES_CLOCK),
                help: || "",
                glyph: "clock_shape",
                keywords: &[],
                range: None,
                scope: Scope::Global,
                profileable: false,
                visible_when: None,
                cost: None,
                takeover: None,
            },
        },
        pub show_desktop_button: bool = true => {
            json: "show_desktop_button",
            slot: 37,
            fmt: Raw,
            gap: false,
            guide: "",
            since: 1,
            reason: "A module nobody uses costs nothing when it is hidden.",
            ui: Some {
                page: PageId("modules"),
                section: SecId("modules"),
                order: 6,
                control: Control::Toggle,
                label: || t(k::SETTINGS_PAGES_SHOW_DESKTOP),
                help: || "",
                glyph: "show_desktop_shape",
                keywords: &[],
                range: None,
                scope: Scope::Global,
                profileable: false,
                visible_when: None,
                cost: None,
                takeover: None,
            },
        },
        /// Start button: fixed item at the left end of the bar (not a movable module); see `module_visible("start")`.
        pub show_start_button: bool = true => {
            json: "show_start_button",
            slot: 38,
            fmt: Raw,
            gap: true,
            guide: "Windows logo button at the left end of the bar: left click opens Start, right click the Win+X menu.",
            since: 1,
            reason: "Some people open Start only with the Win key.",
            ui: Some {
                page: PageId("start"),
                section: SecId("start_menu"),
                order: 1,
                control: Control::Toggle,
                label: || t(k::SETTINGS_PAGES_START_BUTTON),
                help: || "",
                glyph: "windows_logo_shape",
                keywords: &[],
                range: None,
                scope: Scope::Global,
                profileable: false,
                visible_when: None,
                cost: None,
                takeover: None,
            },
        },
        /// Entry display: "icon_text" | "icon" | "text".
        pub display_mode: DisplayMode = DisplayMode::IconText => {
            json: "display_mode",
            slot: 3,
            fmt: Js,
            gap: true,
            guide: "What each entry shows: \"icon_text\" (icon + name), \"icon\" (icon only) or \"text\" (name only).\n\
                 The indicator bar under each entry adapts to the chosen mode.",
            since: 1,
            reason: "Icons and names, icons only for a short bar, or names only.",
            ui: Some {
                page: PageId("bar"),
                section: SecId("position"),
                order: 2,
                control: Control::Segmented(&[(|| t(k::SETTINGS_PAGES_ICON_NAME), "icon_text"), (|| t(k::SETTINGS_PAGES_ICON), "icon"), (|| t(k::SETTINGS_PAGES_NAME), "text")]),
                label: || t(k::SETTINGS_PAGES_DISPLAY_MODE),
                help: || "",
                glyph: "text_shape",
                keywords: &[],
                range: None,
                scope: Scope::Global,
                profileable: false,
                visible_when: None,
                cost: None,
                takeover: None,
            },
        },
        /// App icon side, logical px (`APP_ICON_SIZE`).
        pub app_icon_size: u8 = APP_ICON_SIZE.default => {
            json: "app_icon_size",
            slot: 4,
            fmt: Raw,
            gap: true,
            guide: "Sizes, each independent (the bar height follows the tallest content):\n\
                 \x20 app_icon_size   app icons, 14-32 px (default 16)\n\
                 \x20 app_text_size   app names, 10-18 px (default 13)\n\
                 \x20 tray_icon_size  tray icons, 12-24 px (default 16)\n\
                 \x20 module_size     start button, notifications, volume, network, metrics, battery, clock, show desktop and\n\
                 \x20                 power, 75-150 % (default 100)\n\
                 Logical pixels: the Windows display scaling is applied on top.",
            since: 1,
            reason: "Icon size suits the display and the eyes.",
            ui: Some {
                page: PageId("appearance"),
                section: SecId("sizes"),
                order: 3,
                control: Control::Slider,
                label: || t(k::SETTINGS_PAGES_APP_ICONS),
                help: || "",
                glyph: "square_shape",
                keywords: &[],
                range: Some(size_range(APP_ICON_SIZE, Unit::Px)),
                scope: Scope::Global,
                profileable: false,
                visible_when: None,
                cost: None,
                takeover: None,
            },
        },
        /// App name text height, logical px (`APP_TEXT_SIZE`).
        pub app_text_size: u8 = APP_TEXT_SIZE.default => {
            json: "app_text_size",
            slot: 5,
            fmt: Raw,
            gap: false,
            guide: "",
            since: 1,
            reason: "Name size suits the display and the eyes.",
            ui: Some {
                page: PageId("appearance"),
                section: SecId("sizes"),
                order: 4,
                control: Control::Slider,
                label: || t(k::SETTINGS_PAGES_APP_NAMES),
                help: || "",
                glyph: "text_shape",
                keywords: &[],
                range: Some(size_range(APP_TEXT_SIZE, Unit::Px)),
                scope: Scope::Global,
                profileable: false,
                visible_when: None,
                cost: None,
                takeover: None,
            },
        },
        /// Tray icon side, logical px (`TRAY_ICON_SIZE`).
        pub tray_icon_size: u8 = TRAY_ICON_SIZE.default => {
            json: "tray_icon_size",
            slot: 6,
            fmt: Raw,
            gap: false,
            guide: "",
            since: 1,
            reason: "Tray icon size suits the display and the eyes.",
            ui: Some {
                page: PageId("appearance"),
                section: SecId("sizes"),
                order: 5,
                control: Control::Slider,
                label: || t(k::SETTINGS_PAGES_TRAY_ICONS),
                help: || "",
                glyph: "ring_shape",
                keywords: &[],
                range: Some(size_range(TRAY_ICON_SIZE, Unit::Px)),
                scope: Scope::Global,
                profileable: false,
                visible_when: None,
                cost: None,
                takeover: None,
            },
        },
        /// Scale of the system modules in percent (`MODULE_SIZE`).
        pub module_size: u8 = MODULE_SIZE.default => {
            json: "module_size",
            slot: 7,
            fmt: Raw,
            gap: false,
            guide: "",
            since: 1,
            reason: "The system modules scale apart from the apps.",
            ui: Some {
                page: PageId("appearance"),
                section: SecId("sizes"),
                order: 6,
                control: Control::Slider,
                label: || t(k::SETTINGS_PAGES_MODULES),
                help: || "",
                glyph: "bars_shape",
                keywords: &[],
                range: Some(size_range(MODULE_SIZE, Unit::Percent)),
                scope: Scope::Global,
                profileable: false,
                visible_when: None,
                cost: None,
                takeover: None,
            },
        },
        /// Background colour of the bar and the popups ("#RRGGBB").
        pub background: Background = Background::BLACK => {
            json: "background",
            slot: 8,
            fmt: Js,
            gap: true,
            guide: "Background colour of the bar and the popups as a hex code, #RRGGBB (e.g. #000000, pure black,\n\
                 ideal for OLED). The Settings window offers dark presets and a colour picker.",
            since: 1,
            reason: "Black for OLED, or a dark tone of one's own.",
            ui: Some {
                page: PageId("appearance"),
                section: SecId("colour"),
                order: 1,
                control: Control::Swatches,
                label: || t(k::SETTINGS_PAGES_BACKGROUND),
                help: || "",
                glyph: "circle_shape",
                keywords: &[],
                range: None,
                scope: Scope::Global,
                profileable: false,
                visible_when: None,
                cost: None,
                takeover: None,
            },
        },
        /// Opacity of the bar and popup backgrounds in percent (0..=100; text and icons stay fully opaque).
        pub bar_opacity: u8 = DEFAULT_BAR_OPACITY => {
            json: "bar_opacity",
            slot: 9,
            fmt: Custom(bar_opacity_text),
            gap: true,
            guide: "Opacity of the bar and popup backgrounds in percent, 0-100 (0 = only text and icons are\n\
                 visible; they still react to the mouse). Text and icons always stay fully opaque.",
            since: 1,
            reason: "A see-through bar for people who like the wallpaper.",
            ui: Some {
                page: PageId("appearance"),
                section: SecId("colour"),
                order: 2,
                control: Control::Slider,
                label: || t(k::SETTINGS_PAGES_OPACITY),
                help: || "",
                glyph: "drop_shape",
                keywords: &[],
                range: Some(Range { min: 0, max: 100, step: 1, unit: Unit::Percent }),
                scope: Scope::Global,
                profileable: false,
                visible_when: None,
                cost: None,
                takeover: None,
            },
        },
        /// Size of the Settings window in logical pixels (96 DPI), [width, height]. Use `settings_size`.
        pub settings_window_size: [u16; 2] = [SETTINGS_WINDOW_DEFAULT.0 as u16, SETTINGS_WINDOW_DEFAULT.1 as u16] => {
            json: "settings_window_size",
            slot: 10,
            fmt: Js,
            gap: true,
            guide: "Size of the Settings window in logical pixels (96 DPI), [width, height]. Remembered when you\n\
                 resize it; at least [640, 460]; clamped to the display it opens on.",
            since: 1,
            reason: "Internal: the Settings window keeps the size it was given.",
            ui: None,
        },
        /// Clock picture for GetTimeFormatEx (default "HH:mm").
        pub time_format: String = "HH:mm".to_string() => {
            json: "time_format",
            slot: 23,
            fmt: Js,
            gap: true,
            guide: "Clock format (Windows time picture, uses the dock language, or your system locale with \"auto\").\n\
                 \x20 H / HH = hour 0-23      h / hh = hour 1-12      m / mm = minutes\n\
                 \x20 s / ss = seconds        t / tt = AM/PM marker\n\
                 Examples: \"HH:mm\" -> 14:05 (24h)    \"h:mm tt\" -> 2:05 PM (12h)",
            since: 1,
            reason: "The clock as the user reads time: 24 h, 12 h, with seconds.",
            ui: None,
        },
        /// If true and `time_format` has no "s", ":ss" is appended.
        pub show_seconds: bool = false => {
            json: "show_seconds",
            slot: 24,
            fmt: Raw,
            gap: true,
            guide: "true = always show seconds (\":ss\" is appended if time_format has no \"s\").\n\
                 The clock then refreshes every second instead of every minute.",
            since: 1,
            reason: "Seconds on the clock, at the cost of one wake-up a second.",
            ui: Some {
                page: PageId("bar"),
                section: SecId("behaviour"),
                order: 8,
                control: Control::Toggle,
                label: || t(k::SETTINGS_PAGES_CLOCK_SECONDS),
                help: || "",
                glyph: "clock_shape",
                keywords: &[],
                range: None,
                scope: Scope::Global,
                profileable: false,
                visible_when: None,
                cost: None,
                takeover: None,
            },
        },
        /// Date picture for GetDateFormatEx (default "ddd d MMM"; empty = no date).
        pub date_format: String = "ddd d MMM".to_string() => {
            json: "date_format",
            slot: 25,
            fmt: Js,
            gap: true,
            guide: "Date format (Windows date picture). Empty string \"\" = no date.\n\
                 \x20 d / dd = day of month   ddd / dddd = short / full weekday name\n\
                 \x20 M / MM = month number   MMM / MMMM = short / full month name\n\
                 \x20 yy / yyyy = year\n\
                 Examples: \"ddd d MMM\" -> Fri 23 Oct    \"dd/MM/yyyy\" -> 23/10/2026",
            since: 1,
            reason: "The date as the user writes it, or none.",
            ui: None,
        },
        /// GUIDs (lowercase, no braces) of the system tray icons NOT to show
        /// (they keep being answered and forwarded to Explorer).
        pub tray_hidden_guids: Vec<String> = DEFAULT_TRAY_HIDDEN_GUIDS.iter().map(|s| s.to_string()).collect() => {
            json: "tray_hidden_guids",
            slot: 41,
            fmt: Block,
            gap: true,
            guide: "System tray icons the dock never shows (volume, network, power: the dock draws those itself).",
            since: 1,
            reason: "System tray icons the dock draws itself are never shown twice.",
            ui: None,
        },
        /// Tray icons the user chose as "not in the bar" (they stay in the chevron popup).
        /// Per-icon key (`tray_host::item_keys`): lowercase GUID without braces if the icon has one,
        /// otherwise `<lowercase exe name>|<n>` (e.g. "discord.exe|0"), n = the icon's 0-based place among that
        /// exe's icons without a GUID. The `<exe>#<uID>` keys of releases before 1.5.1 are rewritten when the exe's
        /// icons register (`tray_host::key::migrate`). Default: empty.
        pub tray_hidden: Vec<String> = Vec::new() => {
            json: "tray_hidden",
            slot: 39,
            fmt: Block,
            gap: true,
            guide: "Tray icons you chose to keep out of the bar (they stay in the tray popup).\n\
                 Normally managed from the tray popup. A key is the icon's GUID (lowercase, no braces),\n\
                 e.g. \"7820ae73-23e3-4229-82c1-e41cb67d5b9c\", or for an icon without one\n\
                 \"<exe name lowercase>|<n>\", n counting that app's icons from 0, e.g. \"discord.exe|0\".",
            since: 1,
            reason: "Tray icons the user keeps out of the bar.",
            ui: Some {
                page: PageId("modules"),
                section: SecId("tray"),
                order: 12,
                control: Control::List,
                label: || t(k::SETTINGS_PAGES_CHOOSE_VISIBLE_ICONS),
                help: || "",
                glyph: "eye_icon",
                keywords: &[],
                range: None,
                scope: Scope::Global,
                profileable: false,
                visible_when: Some(Cond { json: "show_tray", equals: "true" }),
                cost: None,
                takeover: None,
            },
        },
        /// Relative order of the app entries that are NOT pinned (their keys). Pinned entries use the
        /// `pinned` array order. Apps missing from the list go last, in the order they were first seen.
        /// Written when the user drags an entry; capped at `APP_ORDER_CAP` keys.
        pub app_order: Vec<String> = Vec::new() => {
            json: "app_order",
            slot: 46,
            fmt: Block,
            gap: true,
            guide: "Order of the open apps that are NOT pinned (their keys). Pinned apps follow the \"pinned\" order\n\
                 above. Normally managed by dragging an entry in the bar; apps not listed go last, in the order\n\
                 they were first seen. At most 64 keys are kept.",
            since: 1,
            reason: "Internal: the order the user dragged the open apps into.",
            ui: None,
        },
        /// Order of the tray icons (same key as `tray_hidden`). Unknown icons go last, in registration order.
        pub tray_order: Vec<String> = Vec::new() => {
            json: "tray_order",
            slot: 40,
            fmt: Block,
            gap: true,
            guide: "Order of the tray icons in the bar (same keys as \"tray_hidden\"). Normally managed by\n\
                 dragging an icon in the bar; icons not listed go last, in the order they registered.",
            since: 1,
            reason: "Internal: the order the user dragged the tray icons into.",
            ui: None,
        },
        /// Order of the system modules: "metrics", "notifications", "network", "volume", "clock",
        /// "show_desktop", "power". Validated on use (`render::modules_from_names`): unknown names are
        /// ignored, a missing module goes back next to its default neighbour.
        pub modules_order: Vec<String> = DEFAULT_MODULES_ORDER.iter().map(|s| s.to_string()).collect() => {
            json: "modules_order",
            slot: 47,
            fmt: Block,
            gap: true,
            guide: "Order of the system buttons on the right of the bar. Normally managed by dragging them.\n\
                 Names: \"metrics\", \"notifications\", \"network\", \"volume\", \"battery\", \"clock\", \"show_desktop\", \"power\".\n\
                 Missing names go back next to their default neighbour, unknown names are ignored.\n\
                 Hiding a module (the show_* switches above) keeps its place in this list.",
            since: 1,
            reason: "The order the user dragged the system modules into.",
            ui: None,
        },
        /// Layout of the volume popup (`AudioLayout`): section order, expanded / collapsed and hidden.
        /// Written as a short string, e.g. "MOSX" (the default).
        pub audio_layout: AudioLayout = AudioLayout::default() => {
            json: "audio_layout",
            slot: 48,
            fmt: Js,
            gap: true,
            guide: "Volume panel layout. Normally changed in the panel itself: click a section title to expand or\n\
                 collapse it, or use the pencil button at the bottom (Customize) to drag sections into a new\n\
                 order and to hide them. One letter per section, in display order: M = master volume,\n\
                 O = output devices, S = spatial sound, X = volume mixer. Upper case = expanded, lower case =\n\
                 collapsed, a leading \"-\" hides the section. Missing sections come back at the end.\n\
                 Example: \"M-OsX\". Default: \"MOSX\".\n\
                 A collapsed or hidden section reads nothing from Windows.",
            since: 1,
            reason: "The volume panel's sections in the user's order, expanded or hidden.",
            ui: None,
        },
        /// true once the `notif` thread has already asked for notification access once
        /// (`RequestAccessAsync`): the prompt is not shown again on every start.
        pub notif_prompted: bool = false => {
            json: "notif_prompted",
            slot: 55,
            fmt: Raw,
            gap: true,
            guide: "Internal: set once the notification-access prompt has been shown. Leave as is.",
            since: 1,
            reason: "Internal: the notification-access prompt is shown once.",
            ui: None,
        },
        /// true = once a day ask GitHub whether a newer release exists (badge only).
        pub update_check: bool = true => {
            json: "update_check",
            slot: 50,
            fmt: Raw,
            gap: true,
            guide: "Updates. Once a day the dock asks GitHub (api.github.com) whether a newer release exists and\n\
                 shows a badge on Settings; nothing is downloaded until you choose \"Update now\" in Settings ->\n\
                 Updates. false = never ask by itself (\"Check now\" still works).",
            since: 1,
            reason: "Some people want no network call they did not ask for.",
            ui: Some {
                page: PageId("updates"),
                section: SecId("automatic"),
                order: 1,
                control: Control::Toggle,
                label: || t(k::SETTINGS_PAGES_CHECK_AUTOMATICALLY),
                help: || t(k::SETTINGS_PAGES_ONCE_A_DAY_ON_GITHUB),
                glyph: "refresh_shape",
                keywords: &[],
                range: None,
                scope: Scope::Global,
                profileable: false,
                visible_when: None,
                cost: None,
                takeover: None,
            },
        },
        /// true = also download, verify and install a new release by itself when it is quiet.
        pub update_auto_install: bool = false => {
            json: "update_auto_install",
            slot: 51,
            fmt: Raw,
            gap: true,
            guide: "true = also download, verify (SHA-256 + version) and install a new release by itself, the next\n\
                 time no popup or Settings window is open and no fullscreen app is in front.",
            since: 1,
            reason: "Updates without a click, for people who never want to think about them.",
            ui: Some {
                page: PageId("updates"),
                section: SecId("automatic"),
                order: 2,
                control: Control::Toggle,
                label: || t(k::SETTINGS_PAGES_INSTALL_AUTOMATICALLY),
                help: || t(k::SETTINGS_PAGES_WHEN_THE_DOCK_IS_IDLE),
                glyph: "download_shape",
                keywords: &[],
                range: None,
                scope: Scope::Global,
                profileable: false,
                visible_when: None,
                cost: None,
                takeover: None,
            },
        },
        /// Internal: UTC time (RFC 3339) of the last successful update check, "" = never.
        pub last_update_check: String = String::new() => {
            json: "last_update_check",
            slot: 52,
            fmt: Js,
            gap: true,
            guide: "Internal: time of the last successful check (UTC) and the newest release found. Leave as is.",
            since: 1,
            reason: "Internal: the daily check runs once a day.",
            ui: None,
        },
        /// Internal: tag of the newest release found by the last check, "" = none.
        pub update_available: String = String::new() => {
            json: "update_available",
            slot: 53,
            fmt: Js,
            gap: false,
            guide: "",
            since: 1,
            reason: "Internal: the badge on Settings survives a restart.",
            ui: None,
        },
        /// The launcher (Alt+Space style search box); off until a chord is set.
        #[serde(deserialize_with = "read_launcher")]
        pub launcher: LauncherConfig = LauncherConfig::default() => {
            json: "launcher",
            slot: 54,
            fmt: Custom(launcher_text),
            gap: true,
            guide: "Launcher: a search box over the desktop (apps, open windows, settings, commands, calculator).\n\
                 It stays off until \"chord\" is set. Unknown or out-of-range values fall back to the default.",
            since: 1,
            reason: "A search box over the desktop for people who start everything from the keyboard.",
            ui: Some {
                page: PageId("launcher"),
                section: SecId("launcher"),
                order: 1,
                control: Control::Custom("launcher"),
                label: || t(k::COMMON_LAUNCHER),
                help: || t(k::SETTINGS_PAGES_FIND_AND_START_ANYTHING_FROM),
                glyph: "magnifier",
                keywords: &[],
                range: None,
                scope: Scope::Global,
                profileable: false,
                visible_when: None,
                cost: None,
                takeover: None,
            },
        },
        /// true = the Start menu has an entry for the Tasks app once it is installed.
        pub suite_tasks: bool = true => {
            json: "suite_tasks",
            slot: 56,
            fmt: Raw,
            gap: true,
            guide: "Suite. true = the Start menu has an entry for the Tasks app once it is installed;\n\
                 false = no entry (the app itself stays installed). Applied by the elevated dock.",
            since: 1,
            reason: "A user who does not want the entry can switch it off.",
            ui: None,
        },
        /// true = the Start menu has an entry for the Files app once it is installed.
        pub suite_files: bool = true => {
            json: "suite_files",
            slot: 57,
            fmt: Raw,
            gap: false,
            guide: "true = the Start menu has an entry for the Files app once it is installed (as suite_tasks).",
            since: 1,
            reason: "A user who does not want the entry can switch it off.",
            ui: None,
        },
        /// true = Ctrl+Shift+Esc opens Tasks instead of Windows Task Manager; off by default.
        pub takeover_ctrl_shift_esc: bool = false => {
            json: "takeover_ctrl_shift_esc",
            slot: 58,
            fmt: Raw,
            gap: false,
            guide: "Ctrl+Shift+Esc opens Tasks instead of Windows Task Manager (off by default; not available while Tasks is beta).",
            since: 1,
            reason: "opt-in takeover of Ctrl+Shift+Esc (ROADMAP X10, X34)",
            ui: Some {
                page: PageId("suite"),
                section: SecId("keyboard"),
                order: 1,
                control: Control::Toggle,
                label: || t(k::SUITE_CSE_LABEL),
                help: || t(k::SUITE_CSE_HELP),
                glyph: "keycap_shape",
                keywords: &[],
                range: None,
                scope: Scope::Global,
                profileable: false,
                visible_when: None,
                cost: None,
                takeover: Some(oled_config::Takeover::CtrlShiftEsc),
            },
        },
    }
}

/// Maximum number of keys kept in `app_order` / `tray_order` (the oldest, not-seen-recently ones fall off).
pub const APP_ORDER_CAP: usize = 64;
pub const TRAY_ORDER_CAP: usize = 128;

/// Default order of the system modules (names accepted by `modules_order`).
pub const DEFAULT_MODULES_ORDER: [&str; 8] =
    ["metrics", "notifications", "network", "volume", "battery", "clock", "show_desktop", "power"];

/// The default list of releases before 1.6: volume, network, power, location, microphone. A file whose
/// list equals it (once, `migrate_privacy`) takes `DEFAULT_TRAY_HIDDEN_GUIDS` instead.
pub const OLD_DEFAULT_TRAY_HIDDEN_GUIDS: [&str; 5] = [
    "7820ae73-23e3-4229-82c1-e41cb67d5b9c",
    "7820ae74-23e3-4229-82c1-e41cb67d5b9c",
    "7820ae75-23e3-4229-82c1-e41cb67d5b9c",
    "7820ae77-23e3-4229-82c1-e41cb67d5b9c",
    "7820ae82-23e3-4229-82c1-e41cb67d5b9c",
];

/// GUIDs of the system tray icons hidden by default: volume, network, power (the dock draws or
/// duplicates all three). The privacy indicators are not in it.
pub const DEFAULT_TRAY_HIDDEN_GUIDS: [&str; 3] = [
    "7820ae73-23e3-4229-82c1-e41cb67d5b9c",
    "7820ae74-23e3-4229-82c1-e41cb67d5b9c",
    "7820ae75-23e3-4229-82c1-e41cb67d5b9c",
];

/// The tray icons of the system's privacy indicators: location, microphone.
pub const PRIVACY_GUIDS: [&str; 2] = ["7820ae77-23e3-4229-82c1-e41cb67d5b9c", "7820ae82-23e3-4229-82c1-e41cb67d5b9c"];

/// Whether `key` (a tray icon GUID, any case, no braces) is one of the privacy indicators.
pub fn is_privacy_guid(key: &str) -> bool {
    PRIVACY_GUIDS.iter().any(|g| g.eq_ignore_ascii_case(key.trim()))
}

/// Equal as sets, ASCII case-insensitive, no other entry.
fn same_set(list: &[String], want: &[&str]) -> bool {
    list.iter().all(|g| want.iter().any(|w| w.eq_ignore_ascii_case(g)))
        && want.iter().all(|w| list.iter().any(|g| g.eq_ignore_ascii_case(w)))
}

/// Once per file (`tray_privacy_migrated`, here `done`): a hidden-icons list equal to the old default takes the new
/// one, so the microphone and location indicators show. Any other list is the user's and stays. Returns
/// whether the list changed.
fn privacy_step(done: &mut bool, list: &mut Vec<String>) -> bool {
    if *done {
        return false;
    }
    *done = true;
    if !same_set(list, &OLD_DEFAULT_TRAY_HIDDEN_GUIDS) {
        return false;
    }
    *list = DEFAULT_TRAY_HIDDEN_GUIDS.iter().map(|s| s.to_string()).collect();
    true
}

/// `privacy_step` over a config.
#[cfg(test)]
fn migrate_privacy(c: &mut Config) -> bool {
    privacy_step(&mut c.tray_privacy_migrated, &mut c.tray_hidden_guids)
}

/// The config file, from `init` on.
static STORE: OnceLock<Store<Config>> = OnceLock::new();
/// Before `init`, and in the modes that never call it (`--preview`): the defaults, never written.
static UNLOADED: OnceLock<Store<Config>> = OnceLock::new();

fn store() -> &'static Store<Config> {
    match STORE.get() {
        Some(s) => s,
        None => UNLOADED.get_or_init(|| Store::new(None, on_set)),
    }
}

/// Runs after every change of the snapshot (start, Settings, hand edit, drag): the dock language follows the config,
/// and so do the theme file (TH2) and the entry points (I1b).
fn on_set(c: &Config) {
    crate::i18n::apply(c.language);
    crate::theme_sync::on_config(c);
    crate::entry_points::on_config(c);
}

// -------------------------------------------------------------------- access

/// Current config snapshot (cheap: clones an `Arc`). Available from any thread.
/// Before `init()` it returns the defaults.
pub fn get() -> Arc<Config> {
    store().get()
}

pub(crate) fn set(c: Config) {
    store().set(c);
}

/// Parses a config text as the store reads its file (a BOM, comments, the migrations, the readers' fixes). Used to
/// import an older copy, and by the tests.
fn parse(text: &str) -> Result<Config, String> {
    oled_config::parse_text::<Config>(text, None).map(|(c, _)| c).map_err(|e| error_text(&e))
}

/// A whole-file error as serde_json words it: the message, then " at line L column C" when the position is known (the
/// text the dock has always logged).
fn error_text(e: &oled_config::ConfigError) -> String {
    match &e.problem {
        oled_config::Problem::Parse(m) if e.line > 0 => format!("{m} at line {} column {}", e.line, e.col),
        oled_config::Problem::Parse(m) => m.clone(),
        _ => e.log_line(),
    }
}

/// `parse`, and what the read noticed (`LoadNotes`).
#[cfg(test)]
fn parse_with_notes(text: &str) -> Result<(Config, LoadNotes), String> {
    READING.with(|r| r.set(None));
    let c = parse(text)?;
    Ok((c, READING.with(Cell::take).map(|r| r.notes).unwrap_or_default()))
}

/// `parse` over `last_good`, and the errors of the text: the store's, then the launcher's fixes (as `errors` lists
/// them after a load).
#[cfg(test)]
fn parse_with_errors(text: &str, last_good: Option<&Config>) -> Result<(Config, Vec<ConfigError>), String> {
    READING.with(|r| r.set(None));
    let (c, mut errors) = oled_config::parse_text::<Config>(text, last_good).map_err(|e| error_text(&e))?;
    let launcher = READING.with(Cell::take).map(|r| r.launcher).unwrap_or_default();
    errors.extend(located(launcher, text));
    Ok((c, errors))
}

/// The file `c` makes: `Config::to_template`.
#[cfg(test)]
fn render(c: &Config) -> String {
    c.to_template()
}

/// What a load noticed about the file, for Settings and the log. `privacy_migrated` is filled by `migrate_v0`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct LoadNotes {
    /// `show_battery` is present and is neither a boolean nor "auto", "true" or "false": "auto" is used.
    pub show_battery_invalid: bool,
    /// The privacy migration changed the hidden-icons list in this load.
    pub privacy_migrated: bool,
    /// The file had no valid `tray_privacy_migrated` and this load set it: the file is rewritten so that the flag is
    /// stored, whether or not the list changed (at the start by the store, as a migrated file; after a hand edit by
    /// `poll_changed`), once it has no errors.
    pub privacy_flag_set: bool,
    /// A key added in 1.6 (a delay, `push_threshold`, `keyboard` or one of its switches, `tray_privacy_migrated`)
    /// held a value of the wrong type: it was ignored (one error of the read) and the key reads its default, or at a
    /// reload its last good value.
    pub new_keys_invalid: bool,
}

/// The notes of the last successful load (init or a reload), for Settings.
pub fn load_notes() -> LoadNotes {
    *NOTES.lock().unwrap_or_else(|p| p.into_inner())
}

static NOTES: Mutex<LoadNotes> = Mutex::new(LoadNotes {
    show_battery_invalid: false,
    privacy_migrated: false,
    privacy_flag_set: false,
    new_keys_invalid: false,
});

/// What the read of a config text noticed beyond its values: the notes, and the fixes the launcher's reader made.
#[derive(Default)]
struct Reading {
    notes: LoadNotes,
    launcher: Vec<ConfigError>,
}

thread_local! {
    /// The record of the last config text read on this thread (`migrate_v0` starts it and `read_launcher` adds to it
    /// while the store reads), taken by `init` and `poll_changed` after their read.
    static READING: Cell<Option<Reading>> = const { Cell::new(None) };
}

/// The launcher's fixes in the file as last read (none after our own write), each with its line.
static LAUNCHER_ERRORS: Mutex<Vec<ConfigError>> = Mutex::new(Vec::new());

/// The errors of the file as last read, none after our own write: each value that did not load (its last good value,
/// or the default, stands in), in the order of the file, then the launcher's fixes. For Settings (P3-dock-suite).
#[cfg_attr(not(test), allow(dead_code))]
pub fn errors() -> Arc<[ConfigError]> {
    let read = store().errors();
    let launcher = LAUNCHER_ERRORS.lock().unwrap_or_else(|p| p.into_inner());
    if launcher.is_empty() {
        return read;
    }
    read.iter().chain(launcher.iter()).cloned().collect()
}

/// After a read of the file by the store: its notes and the launcher's fixes, located in the file and logged once.
fn publish(reading: Reading) {
    store_notes(reading.notes);
    let launcher = if reading.launcher.is_empty() { Vec::new() } else { located(reading.launcher, &file_text()) };
    for e in &launcher {
        crate::log!("config: {}", e.log_line());
    }
    *LAUNCHER_ERRORS.lock().unwrap_or_else(|p| p.into_inner()) = launcher;
}

/// The text of the config file ("" when it cannot be read): only to find the line of an error.
fn file_text() -> String {
    crate::datadir::config_path()
        .and_then(|p| crate::datadir::read_file(&p, crate::datadir::CONFIG_MAX_BYTES, false).ok())
        .and_then(|b| String::from_utf8(b).ok())
        .unwrap_or_default()
}

fn store_notes(n: LoadNotes) {
    *NOTES.lock().unwrap_or_else(|p| p.into_inner()) = n;
    if n.show_battery_invalid {
        crate::log!("config error: show_battery has an unknown value, \"auto\" is used");
    }
    if n.new_keys_invalid {
        crate::log!("config error: a new key has a value of the wrong type, its default is used");
    }
}

/// After every write of ours: the file just written holds a valid show_battery, new keys of the right type and a
/// sanitized launcher section, so the notes about an invalid show_battery or new key and the launcher's fixes are stale.
fn written() {
    let mut notes = NOTES.lock().unwrap_or_else(|p| p.into_inner());
    notes.show_battery_invalid = false;
    notes.new_keys_invalid = false;
    drop(notes);
    LAUNCHER_ERRORS.lock().unwrap_or_else(|p| p.into_inner()).clear();
}

/// Version 0 (every file so far: the dock has never written a "version") to 1, run by every read of a file: the legacy
/// `bar_size` becomes the four sizes, `"keyboard": true` or `false` the four switches, the privacy indicators leave an
/// untouched old hidden list once (the flag is set), and what the read notices is noted (`LoadNotes`, on this thread).
/// Each changes only a file that needs it, so a file this release wrote is not rewritten.
fn migrate_v0(v: &mut serde_json::Value) -> Result<(), MigrateError> {
    let Some(o) = v.as_object_mut() else { return Ok(()) };
    migrate_bar_size(o)?;
    let new_keys_invalid = new_keys_of_the_wrong_type(o);
    let privacy_flag_set = !matches!(o.get("tray_privacy_migrated"), Some(serde_json::Value::Bool(true)));
    let privacy_migrated = migrate_privacy_value(o);
    let show_battery_invalid = match o.get("show_battery") {
        None | Some(serde_json::Value::Bool(_)) => false,
        Some(serde_json::Value::String(s)) => {
            let s = s.trim();
            !(s.eq_ignore_ascii_case("auto") || s.eq_ignore_ascii_case("true") || s.eq_ignore_ascii_case("false"))
        }
        Some(_) => true,
    };
    let notes = LoadNotes { show_battery_invalid, privacy_migrated, privacy_flag_set, new_keys_invalid };
    READING.with(|r| r.set(Some(Reading { notes, launcher: Vec::new() })));
    Ok(())
}

/// The keys of the four keyboard switches (`KeyboardConfig`).
const KEYBOARD_FLAGS: [&str; 4] = ["win_numbers", "win_t", "win_b", "peek"];

/// The keys added in 1.6 as a file holds them: `"keyboard": true` or `false` becomes the four switches, as 1.6.0 read
/// it. Returns whether one of them (a delay, `push_threshold`, `keyboard` or one of its switches,
/// `tray_privacy_migrated`) has a value of the wrong type (for the three numbers: anything but a whole number from 0
/// up), for `LoadNotes::new_keys_invalid`; the read itself drops such a value, one error each. A whole number above the
/// range is not of the wrong type: the range check (or `read_push_threshold`'s clamp) takes it.
fn new_keys_of_the_wrong_type(o: &mut serde_json::Map<String, serde_json::Value>) -> bool {
    use serde_json::Value;
    let not_a_count = |v: &Value| v.as_u64().is_none();
    let mut wrong =
        ["hide_delay_ms", "show_delay_ms", "push_threshold"].iter().any(|k| o.get(*k).is_some_and(not_a_count));
    wrong |= o.get("tray_privacy_migrated").is_some_and(|f| !f.is_boolean());
    match o.get("keyboard") {
        None => {}
        Some(Value::Bool(on)) => {
            let on = *on;
            let all = KEYBOARD_FLAGS.iter().map(|f| (f.to_string(), Value::Bool(on))).collect();
            o.insert("keyboard".to_string(), Value::Object(all));
        }
        Some(Value::Object(m)) => wrong |= KEYBOARD_FLAGS.iter().any(|f| m.get(*f).is_some_and(|x| !x.is_boolean())),
        Some(_) => wrong = true,
    }
    wrong
}

/// The keys of the four sizes, in the order of `migrate_bar_size`'s values.
const SIZE_KEYS: [(&str, SizeRange); 4] = [
    ("app_icon_size", APP_ICON_SIZE),
    ("app_text_size", APP_TEXT_SIZE),
    ("tray_icon_size", TRAY_ICON_SIZE),
    ("module_size", MODULE_SIZE),
];

/// Converts a legacy `bar_size` (one knob scaling the whole bar: small 7/8, medium 1, large 5/4)
/// into the four independent sizes, unless those were already changed (a hand edit wins). Removes
/// `bar_size`, which is never written back. An unknown value makes the file invalid, as it always has.
fn migrate_bar_size(o: &mut serde_json::Map<String, serde_json::Value>) -> Result<(), MigrateError> {
    let Some(size) = o.remove("bar_size") else { return Ok(()) };
    let size = Option::<BarSize>::deserialize(&size).map_err(|e| MigrateError(format!("bar_size: {e}")))?;
    let sizes: [u8; 4] = match size {
        Some(BarSize::Small) => [14, 11, 14, 88],
        Some(BarSize::Large) => [20, 16, 20, 125],
        Some(BarSize::Medium) | None => return Ok(()),
    };
    let untouched =
        SIZE_KEYS.iter().all(|(key, range)| o.get(*key).is_none_or(|v| v.as_u64() == Some(u64::from(range.default))));
    if untouched {
        for ((key, _), n) in SIZE_KEYS.iter().zip(sizes) {
            o.insert((*key).to_string(), n.into());
        }
    }
    Ok(())
}

/// `migrate_privacy` over a file's keys: a `tray_privacy_migrated` that is not true (missing, false, unreadable)
/// becomes true, and a hidden list equal to the old default takes the new one (a list of the wrong type is left for the
/// read to report). Returns whether the list changed.
fn migrate_privacy_value(o: &mut serde_json::Map<String, serde_json::Value>) -> bool {
    if matches!(o.get("tray_privacy_migrated"), Some(serde_json::Value::Bool(true))) {
        return false;
    }
    o.insert("tray_privacy_migrated".to_string(), true.into());
    let mut list: Vec<String> = match o.get("tray_hidden_guids") {
        None => DEFAULT_TRAY_HIDDEN_GUIDS.iter().map(|s| s.to_string()).collect(),
        Some(v) => match Vec::<String>::deserialize(v) {
            Ok(l) => l,
            Err(_) => return false,
        },
    };
    let changed = privacy_step(&mut false, &mut list);
    if changed {
        o.insert("tray_hidden_guids".to_string(), list.into());
    }
    changed
}

/// `push_threshold` as read: clamped into `PUSH_THRESHOLD_RANGE`, a number too big for u32 too (Settings has no row for
/// it, so no descriptor gives it a range to check).
fn read_push_threshold<'de, D: serde::Deserializer<'de>>(d: D) -> Result<u32, D::Error> {
    u64::deserialize(d)
        .map(|v| u32::try_from(v).unwrap_or(u32::MAX).clamp(PUSH_THRESHOLD_RANGE.0, PUSH_THRESHOLD_RANGE.1))
}

/// The `launcher` section as read: every key brought into its closed list or range, each fix one error of the read
/// (`launcher_error`), recorded on this thread with the notes. A number out of its range takes its default, as a
/// top-level one does, not the value `sanitize` clamps it to: its error says the default was kept.
fn read_launcher<'de, D: serde::Deserializer<'de>>(d: D) -> Result<LauncherConfig, D::Error> {
    let before = LauncherConfig::deserialize(d)?;
    let mut l = before.clone();
    let errors: Vec<ConfigError> = l.sanitize().iter().map(|m| launcher_error(m, &before)).collect();
    if l.top_percent != before.top_percent {
        l.top_percent = LAUNCHER_TOP_PERCENT.2;
    }
    if l.rows != before.rows {
        l.rows = LAUNCHER_ROWS.2;
    }
    if l.width != before.width {
        l.width = LAUNCHER_WIDTH.2;
    }
    READING.with(|r| {
        let mut reading = r.take();
        if let Some(x) = reading.as_mut() {
            x.launcher = errors;
        }
        r.set(reading);
    });
    Ok(l)
}

/// One fix of `LauncherConfig::sanitize` (its message, the section before the fix) as an error under
/// "launcher.<key>"; its line and column are found later in the text (`located`).
fn launcher_error(message: &str, before: &LauncherConfig) -> ConfigError {
    let path = message.split_once(": ").map_or(message, |(p, _)| p);
    let range = |r: (i64, i64), got: i64| Problem::Range { min: r.0, max: r.1, got };
    let problem = match path.strip_prefix("launcher.").unwrap_or(path) {
        "chord" => Problem::UnknownValue { allowed: &LAUNCHER_CHORDS },
        "with_powertoys" => Problem::UnknownValue { allowed: &LAUNCHER_WITH_POWERTOYS },
        "monitor" => Problem::UnknownValue { allowed: &LAUNCHER_MONITORS },
        "over_fullscreen" => Problem::UnknownValue { allowed: &LAUNCHER_OVER_FULLSCREEN },
        "web_engine" => Problem::UnknownValue { allowed: &LAUNCHER_WEB_ENGINES },
        "top_percent" => {
            let (lo, hi, _) = LAUNCHER_TOP_PERCENT;
            range((lo.into(), hi.into()), before.top_percent.into())
        }
        "rows" => {
            let (lo, hi, _) = LAUNCHER_ROWS;
            range((lo.into(), hi.into()), before.rows.into())
        }
        "width" if !(LAUNCHER_WIDTH.0..=LAUNCHER_WIDTH.1).contains(&before.width) => {
            range((LAUNCHER_WIDTH.0.into(), LAUNCHER_WIDTH.1.into()), before.width.into())
        }
        "width" => Problem::Type { expected: "560 to 880 in steps of 40" },
        "providers" => Problem::UnknownValue { allowed: &LAUNCHER_PROVIDERS },
        "prefixes" => Problem::Type {
            expected: "prefixes of 1 to 5 characters (a word ends with a space), each once, for a known provider",
        },
        "providers.apps" => Problem::Type { expected: "true (apps are always on)" },
        // Entries are dropped for a word or target over the text cap, else for their number.
        "aliases" if before.aliases.iter().any(|(w, t)| w.chars().count().max(t.chars().count()) > ALIAS_TEXT_CAP) => {
            Problem::TooLong { max: ALIAS_TEXT_CAP }
        }
        "aliases" => {
            let n = |n: usize| i64::try_from(n).unwrap_or(i64::MAX);
            range((0, n(ALIAS_ENTRIES_CAP)), n(before.aliases.len()))
        }
        "web_url" => Problem::Type { expected: "an https:// address with {q}" },
        _ => Problem::Type { expected: "a value of its list" },
    };
    ConfigError { path: path.to_string(), line: 0, col: 0, problem, kept: Kept::Default }
}

/// `errors` with the line and column of their key in `text` (a BOM skipped).
fn located(mut errors: Vec<ConfigError>, text: &str) -> Vec<ConfigError> {
    let text = oled_config::jsonc::strip_bom(text);
    for e in &mut errors {
        (e.line, e.col) = oled_config::jsonc::locate(text, &e.path).unwrap_or((0, 0));
    }
    errors
}

/// The config of a fresh install: today's defaults, a one-second hide delay and nothing left to migrate.
#[allow(clippy::field_reassign_with_default)]
fn fresh_config() -> Config {
    // Assigned after the call, not in a struct update: the update form inlines a second copy of `Config::default`
    // into the exe (about 1 KiB).
    let mut c = Config::default();
    c.hide_delay_ms = FRESH_HIDE_DELAY_MS;
    c
}

/// Marker on the first line of every file we write; a config without it is a legacy plain JSON
/// file and gets migrated to the commented template.
const TEMPLATE_MARK: &str = "// mini-oled-dock configuration";

/// The text above the opening brace of the file: the mark, then how to edit it.
const HEADER: &str = "// mini-oled-dock configuration\n\
    // Edit and save: changes are picked up within ~2 seconds, no restart needed.\n\
    // The folder is admin-only: edit this file with Notepad (it saves in place) or an elevated editor.\n\
    // Editors that save by writing a new file and renaming it over this one cannot write here.\n\
    // Comments (// and /* */) are allowed. If the file has a syntax error the previous\n\
    // settings are kept; if the dock must save a change (pin, tray icon) before you fix it,\n\
    // your file is first copied to config.json.bak so no edit is lost.";

/// `bar_opacity` as the file writes it: at most 100.
fn bar_opacity_text(c: &Config) -> String {
    c.bar_opacity.min(100).to_string()
}

/// The `launcher` section as the file writes it, its comments included.
fn launcher_text(c: &Config) -> String {
    let l = &c.launcher;
    // Pretty JSON of a map or list, its continuation lines under a 4-space key.
    let block = |v: String| v.replace('\n', "\n  ");
    format!(
        r#"{{
    // false = the launcher is never started, whatever the chord says.
    "enabled": {l_enabled},
    // The key chord that opens it: "off", "alt+space" or "alt+shift+space".
    "chord": {l_chord},
    // While PowerToys Command Palette or PowerToys Run runs and "chord" is "alt+space": "yield" lets Alt+Space reach
    // it as long as it answers (after two presses in a row with no answer the launcher keeps the chord until it exits,
    // the dock restarts or this value changes); "take" keeps Alt+Space for the launcher (turn its shortcut off in
    // PowerToys, or it may still open on some presses). With "yield" and "powertoys_done" false, the first Alt+Space
    // the dock sees after finding it opens the launcher with the PowerToys question instead of reaching it.
    "with_powertoys": {l_with_powertoys},
    // Display the box opens on: "pointer", "active" or "primary".
    "monitor": {l_monitor},
    // Distance of the box from the top of the display, percent of its height (10-50).
    "top_percent": {l_top_percent},
    // Width in px, 560 to 880 in steps of 40.
    "width": {l_width},
    // Result rows shown at once (4-12).
    "rows": {l_rows},
    // Reserved: animation of the box.
    "animate": {l_animate},
    // true = the box reopens with the last text selected.
    "keep_query": {l_keep_query},
    // Over a fullscreen window: "pass" (the chord goes through) or "open".
    "over_fullscreen": {l_over_fullscreen},
    // true = results you pick often move up.
    "learn": {l_learn},
    // Reserved: spoken announcement of the selected row.
    "announce_selection": {l_announce},
    // Sources of results: apps (always on), windows, files, settings, calc, commands, run, web, datetime, registry.
    "providers": {l_providers},
    // Text that starts a query for one source, 1-5 characters (a word ends with a space), e.g. "= 2+2".
    "prefixes": {l_prefixes},
    // Words that open something when typed alone in Mixed mode: "word": "app:<id>", an http(s) address, "ms-settings:...", a path or a command line (at most 64).
    "aliases": {l_aliases},
    // More folders for file search, e.g. "D:\\Archive" or a whole drive "E:\\" (at most 8; not Windows, Program Files or other profiles).
    "file_folders": {l_file_folders},
    // Web search: "google", "bing", "duckduckgo" or "custom" (then "web_url" is an https:// address with {{q}}).
    "web_engine": {l_web_engine},
    "web_url": {l_web_url},
    // Internal: set once the launcher offer in Start was answered (a chord taken, or Don't show again). Leave as is.
    "offer_done": {l_offer_done},
    // Internal: set once that PowerToys question was answered (any choice, or Don't show again). Leave as is.
    "powertoys_done": {l_powertoys_done}
  }}"#,
        l_enabled = l.enabled,
        l_chord = oled_config::jsonc::js(&l.chord),
        l_with_powertoys = oled_config::jsonc::js(&l.with_powertoys),
        l_monitor = oled_config::jsonc::js(&l.monitor),
        l_top_percent = l.top_percent,
        l_width = l.width,
        l_rows = l.rows,
        l_animate = l.animate,
        l_keep_query = l.keep_query,
        l_over_fullscreen = oled_config::jsonc::js(&l.over_fullscreen),
        l_learn = l.learn,
        l_announce = l.announce_selection,
        l_providers = block(oled_config::jsonc::js_block(&l.providers)),
        l_prefixes = block(oled_config::jsonc::js_block(&l.prefixes)),
        l_aliases = block(oled_config::jsonc::js_block(&l.aliases)),
        l_file_folders = block(oled_config::jsonc::js_block(&l.file_folders)),
        l_web_engine = oled_config::jsonc::js(&l.web_engine),
        l_web_url = oled_config::jsonc::js(&l.web_url),
        l_offer_done = l.offer_done,
        l_powertoys_done = l.powertoys_done,
    )
}

/// `Range` of a size setting, for its descriptor.
const fn size_range(s: SizeRange, unit: Unit) -> Range {
    Range { min: s.min as i64, max: s.max as i64, step: 1, unit }
}

/// Reads the config from disk; if the file is missing it creates it with the defaults; if it is unreadable/invalid
/// it logs and uses the defaults WITHOUT overwriting the file. Sets the global snapshot and remembers
/// the mtime for `poll_changed`. Called once at startup from the main thread.
pub fn init() -> Arc<Config> {
    let spec = crate::datadir::config_path().map(|path| FileSpec {
        path,
        kind: FileKind::AdminRootUserEditable,
        cap: crate::datadir::CONFIG_MAX_BYTES,
    });
    if spec.is_some() {
        report_legacy_dir();
    }
    let store = STORE.get_or_init(|| Store::new(spec, on_set));
    READING.with(|r| r.set(None));
    let outcome = store.load(|| Some(migrate_legacy().unwrap_or_else(fresh_config)));
    if let (LoadOutcome::Read | LoadOutcome::Rewritten, Some(reading)) = (&outcome, READING.with(Cell::take)) {
        let notes = reading.notes;
        publish(reading);
        if notes.privacy_migrated {
            crate::log!("config: privacy indicators no longer hidden by default (list migrated)");
        }
        if outcome == LoadOutcome::Rewritten {
            written();
        }
    }
    store.get()
}

/// One-time migration: reads the legacy `%LOCALAPPDATA%\Dock\config.json` (safely, see
/// `datadir::read_legacy_config`) and returns it parsed; the caller writes it through the safe path.
/// Nothing under the legacy folder is ever written, renamed or deleted.
fn migrate_legacy() -> Option<Config> {
    // First choice: the config of a loosely secured data folder moved aside this run (`datadir`).
    match crate::datadir::read_previous_config() {
        Ok(None) => {}
        Ok(Some(bytes)) => match String::from_utf8(bytes).map_err(|e| e.to_string()).and_then(|t| parse(&t)) {
            Ok(c) => {
                crate::log!("config: settings recovered from the data folder moved aside");
                return Some(c);
            }
            Err(e) => crate::log!("config: config.json of the moved data folder not recovered ({e})"),
        },
        Err(e) => crate::log!("config: config.json of the moved data folder not recovered: {e}"),
    }
    match crate::datadir::read_legacy_config() {
        Ok(None) => None,
        Ok(Some(bytes)) => match String::from_utf8(bytes).map_err(|e| e.to_string()).and_then(|t| parse(&t)) {
            Ok(c) => {
                crate::log!("config: settings migrated from the legacy %LOCALAPPDATA%\\Dock\\config.json");
                Some(c)
            }
            Err(e) => {
                crate::log!("config: legacy %LOCALAPPDATA%\\Dock\\config.json not migrated ({e}): using the defaults");
                None
            }
        },
        Err(e) => {
            crate::log!("config: legacy config not migrated: {e}");
            None
        }
    }
}

/// The legacy-folder line names the folders by description, never by path: they hold the profile folder's name.
const LEGACY_DIR_LINE: &str = "config: the old data folder (%LOCALAPPDATA%\\Dock) is no longer used. Move your custom \
     icons from its icons subfolder to the icons folder of the data folder, then delete the old folder (no admin \
     rights needed).";

/// Logs that the legacy folder is no longer used (custom icons must be moved by hand). Never touches it.
fn report_legacy_dir() {
    let Some(old) = crate::datadir::legacy_dir() else { return };
    // symlink_metadata does not follow a reparse point: this only reports.
    if std::fs::symlink_metadata(&old).is_ok() {
        crate::log!("{LEGACY_DIR_LINE}");
    }
}

/// true if the file's mtime differs from the one remembered at our last read/write (a hand edit that
/// `poll_changed` has not loaded yet). Does not consume the change.
pub fn external_edit_pending() -> bool {
    store().external_edit_pending()
}

// The watch stays here rather than in `Store::watch`: its callback counts itself in the wake probe
// (`config.watch_callbacks`), which oled-config cannot reach.
/// Raw handle of the folder's change notification (`FindFirstChangeNotificationW`), 0 = no watch.
static WATCH_CHANGE: std::sync::atomic::AtomicIsize = std::sync::atomic::AtomicIsize::new(0);
/// Raw handle of the thread-pool wait registered on it, 0 = none.
static WATCH_WAIT: std::sync::atomic::AtomicIsize = std::sync::atomic::AtomicIsize::new(0);
/// The UI window the callback posts to.
static WATCH_UI: std::sync::atomic::AtomicIsize = std::sync::atomic::AtomicIsize::new(0);

/// true while the folder watch runs; when it does not, the housekeeping timer polls the mtime every 2 s instead.
pub fn watching() -> bool {
    WATCH_WAIT.load(std::sync::atomic::Ordering::Acquire) != 0
}

/// Starts the wait on the data folder's change notification (no timer, no thread of ours: a thread-pool wait runs
/// `on_folder_change` in its wait thread). Returns false (and logs) when it cannot be set up; the caller then polls.
pub fn start_watch(ui: windows::Win32::Foundation::HWND) -> bool {
    use std::sync::atomic::Ordering::{Release, SeqCst};
    use windows::Win32::Foundation::HANDLE;
    use windows::Win32::Storage::FileSystem::{
        FindCloseChangeNotification, FindFirstChangeNotificationW, FILE_NOTIFY_CHANGE_FILE_NAME,
        FILE_NOTIFY_CHANGE_LAST_WRITE, FILE_NOTIFY_CHANGE_SIZE,
    };
    use windows::Win32::System::Threading::{RegisterWaitForSingleObject, INFINITE, WT_EXECUTEINWAITTHREAD};

    if watching() {
        return true;
    }
    let fail = |e: String| {
        crate::log!("config: change watch unavailable ({e}), polling every 2 s");
        false
    };
    let Some(path) = crate::datadir::config_path() else { return fail("no data folder".into()) };
    let Some(dir) = path.parent() else { return fail("no data folder".into()) };
    let wide: Vec<u16> = std::os::windows::ffi::OsStrExt::encode_wide(dir.as_os_str()).chain(Some(0)).collect();
    // SAFETY: `wide` is a NUL-terminated path that outlives the call.
    let change = match unsafe {
        FindFirstChangeNotificationW(
            windows::core::PCWSTR(wide.as_ptr()),
            false,
            FILE_NOTIFY_CHANGE_FILE_NAME | FILE_NOTIFY_CHANGE_LAST_WRITE | FILE_NOTIFY_CHANGE_SIZE,
        )
    } {
        Ok(h) => h,
        Err(e) => return fail(e.to_string()),
    };
    WATCH_CHANGE.store(change.0 as isize, SeqCst);
    WATCH_UI.store(ui.0 as isize, SeqCst);
    let mut wait = HANDLE::default();
    // SAFETY: `change` is a valid notification handle owned by the statics until `stop_watch`; the callback is an
    // `extern "system"` fn that stays valid for the whole process. WT_EXECUTEINWAITTHREAD: one callback per
    // notification, never two at once (the callback re-arms the handle before the wait thread waits again).
    let reg = unsafe {
        RegisterWaitForSingleObject(&mut wait, change, Some(on_folder_change), None, INFINITE, WT_EXECUTEINWAITTHREAD)
    };
    if let Err(e) = reg {
        WATCH_CHANGE.store(0, SeqCst);
        // SAFETY: the handle came from FindFirstChangeNotificationW and is closed once.
        let _ = unsafe { FindCloseChangeNotification(change) };
        return fail(e.to_string());
    }
    WATCH_WAIT.store(wait.0 as isize, Release);
    true
}

/// Ends the watch: waits for a running callback, then closes the notification. Idempotent; true when a notification
/// handle was closed (the watch was still held).
pub fn stop_watch() -> bool {
    use std::sync::atomic::Ordering::SeqCst;
    use windows::Win32::Foundation::{HANDLE, INVALID_HANDLE_VALUE};
    use windows::Win32::Storage::FileSystem::FindCloseChangeNotification;
    use windows::Win32::System::Threading::UnregisterWaitEx;

    let wait = WATCH_WAIT.swap(0, SeqCst);
    if wait != 0 {
        // SAFETY: `wait` came from RegisterWaitForSingleObject and is unregistered once (the swap above);
        // INVALID_HANDLE_VALUE waits for a callback in progress, so the notification is idle when it is closed.
        if let Err(e) = unsafe { UnregisterWaitEx(HANDLE(wait as *mut core::ffi::c_void), Some(INVALID_HANDLE_VALUE)) }
        {
            crate::log!("config: UnregisterWaitEx failed: {e}");
        }
    }
    let change = WATCH_CHANGE.swap(0, SeqCst);
    if change != 0 {
        // SAFETY: the handle came from FindFirstChangeNotificationW and is closed once (the swap above).
        let _ = unsafe { FindCloseChangeNotification(HANDLE(change as *mut core::ffi::c_void)) };
    }
    change != 0
}

/// Thread-pool callback of the folder notification (wait thread): re-arms the handle first, then posts one message
/// only when `config.json` itself changed. Never logs (a log line would signal this callback again).
unsafe extern "system" fn on_folder_change(_context: *mut core::ffi::c_void, _timed_out: bool) {
    use std::sync::atomic::Ordering::SeqCst;
    use windows::Win32::Foundation::{HANDLE, HWND, LPARAM, WPARAM};
    use windows::Win32::Storage::FileSystem::FindNextChangeNotification;
    use windows::Win32::UI::WindowsAndMessaging::PostMessageW;

    probe!(crate::probe::bump(crate::probe::C::ConfigWatch));
    let change = WATCH_CHANGE.load(SeqCst);
    let ui = HWND(WATCH_UI.load(SeqCst) as *mut core::ffi::c_void);
    if change == 0 || ui.0.is_null() {
        return;
    }
    // SAFETY: the handle stays open until `stop_watch`, which first waits for this callback to return.
    if unsafe { FindNextChangeNotification(HANDLE(change as *mut core::ffi::c_void)) }.is_err() {
        // wParam 1 = the watch is broken: the UI thread falls back to the 2 s poll. First end the wait (non-blocking
        // form, allowed in the callback) so a still-signaled handle cannot re-fire this callback in a loop.
        let w = WATCH_WAIT.swap(0, SeqCst);
        if w != 0 {
            // SAFETY: `w` came from RegisterWaitForSingleObject and is unregistered once (the swap above).
            let _ = unsafe {
                windows::Win32::System::Threading::UnregisterWaitEx(HANDLE(w as *mut core::ffi::c_void), None)
            };
        }
        // SAFETY: PostMessageW to the dock window from any thread; if it no longer exists it just fails.
        let _ = unsafe { PostMessageW(Some(ui), crate::msg::WM_DOCK_CONFIG_CHANGED, WPARAM(1), LPARAM(0)) };
        return;
    }
    if external_edit_pending() {
        // SAFETY: as above.
        let _ = unsafe { PostMessageW(Some(ui), crate::msg::WM_DOCK_CONFIG_CHANGED, WPARAM(0), LPARAM(0)) };
    }
}

/// Checks the mtime of `config.json`; if it changed, re-reads, updates the global snapshot and
/// returns `true` (the UI must rebuild its entries). Does not block: one stat + read of a
/// small file. Called by the UI thread after a change notification of the folder (`start_watch`), or every 2 s
/// (`TIMER_HOUSEKEEPING`) while that watch is not running.
///
/// If the new content is invalid (half-saved edit or syntax error) the previous
/// snapshot is kept, a message is logged once and `false` is returned; the file is re-read on the next
/// change. Returns `true` only if the parsed content differs from the snapshot.
pub fn poll_changed() -> bool {
    READING.with(|r| r.set(None));
    let r = store().reload_if_changed();
    match &r {
        Reload::Invalid(_) => LAUNCHER_ERRORS.lock().unwrap_or_else(|p| p.into_inner()).clear(),
        _ => {
            if let Some(reading) = READING.with(Cell::take) {
                let flag_set = reading.notes.privacy_flag_set;
                publish(reading);
                if flag_set && store().errors().is_empty() {
                    // A file edited or restored by hand without the flag: stored, so the migration runs once (the
                    // start rewrites such a file itself, as a migrated one).
                    if let Err(e) = modify(|_| {}) {
                        crate::log!("config: storing the privacy flag failed: {e}");
                    }
                }
            }
        }
    }
    r == Reload::Changed
}

/// Atomic read-modify-write of the config: applies `f` to a copy of the current snapshot,
/// writes the file (temp + rename) and, if that succeeds, updates the global snapshot.
/// Used by Pin/Unpin. Write errors -> `Err` (the snapshot stays unchanged).
pub fn modify<F: FnOnce(&mut Config)>(f: F) -> Result<(), String> {
    before_write()?;
    store().modify(f).map_err(|e| e.to_string())?;
    written();
    Ok(())
}

/// Atomically writes `c` to `config.json` (`.tmp` file in the same folder + rename).
/// Updates the global snapshot and the remembered mtime (so `poll_changed` does not re-read
/// our own write).
// No caller in the dock since the store creates a missing file itself; kept for the API's callers to come.
#[allow(dead_code)]
pub fn save(c: &Config) -> Result<(), String> {
    before_write()?;
    store().save(c).map_err(|e| e.to_string())?;
    written();
    Ok(())
}

/// Before a write of ours (`modify`, `save`): the launcher's fixes in the file it replaces saved aside. One body for
/// every closure `modify` is called with.
#[inline(never)]
fn before_write() -> Result<(), String> {
    // The file from `init` on, while the data folder is writable (else the store refuses the write itself).
    let path = STORE.get().and_then(|_| crate::datadir::config_path()).filter(|_| crate::datadir::writable());
    back_up_launcher_fixes(path.as_deref())
}

/// Before a write of ours over `path`: a file that loads only through fixes of its launcher section (`read_launcher`)
/// is copied to `<file>.bak` first, as the store copies a file with errors of its own (it sees only those), so what
/// the fixes drop (aliases, prefixes, provider names) outlives the rewrite. Err when the copy fails: then nothing is
/// written. A file that cannot be read or parsed is the store's to copy.
fn back_up_launcher_fixes(path: Option<&std::path::Path>) -> Result<(), String> {
    let Some(path) = path else { return Ok(()) };
    let read = crate::datadir::read_file(path, crate::datadir::CONFIG_MAX_BYTES, false);
    let Some(text) = read.ok().and_then(|b| String::from_utf8(b).ok()) else { return Ok(()) };
    READING.with(|r| r.set(Some(Reading::default())));
    let parsed = parse(&text).is_ok();
    let fixed = READING.with(Cell::take).is_some_and(|r| !r.launcher.is_empty());
    if !(parsed && fixed) {
        return Ok(());
    }
    let mut bak = path.as_os_str().to_owned();
    bak.push(".bak");
    let bak = std::path::PathBuf::from(bak);
    // The error names the file it failed on.
    crate::datadir::write_atomic(&bak, text.as_bytes(), crate::datadir::FileAcl::Inherit)
        .map_err(|e| format!("backup: {e}"))?;
    crate::log!("config: the launcher section has errors, the file was copied to config.json.bak before rewriting");
    Ok(())
}

// ------------------------------------------------------------- module visibility

/// Names accepted by `Config::module_visible` / `set_module_visible`: the `modules_order` names
/// plus "tray" (`show_tray`). The power button is not listed: it is always shown. The Start button
/// ("start", `show_start_button`) is a fixed item, not a module: the two methods accept it but it is
/// not listed here (the Settings window adds its switch by hand). The Settings window lists its toggles
/// explicitly (`settings_pages::MODULE_TOGGLES`), so only the tests use this one.
#[cfg_attr(not(test), allow(dead_code))]
pub const MODULE_NAMES: [&str; 7] = ["notifications", "volume", "network", "metrics", "clock", "show_desktop", "tray"];

impl Config {
    fn module_flag(&mut self, name: &str) -> Option<&mut bool> {
        Some(match name {
            "notifications" => &mut self.show_notifications,
            "volume" => &mut self.show_volume,
            "network" => &mut self.show_network,
            "metrics" => &mut self.show_metrics,
            "clock" => &mut self.show_clock,
            "show_desktop" => &mut self.show_desktop_button,
            "start" => &mut self.show_start_button,
            "tray" => &mut self.show_tray,
            _ => return None,
        })
    }

    /// true if module `name` (a `MODULE_NAMES` entry, "start" or "power") is shown. Unknown names: true.
    pub fn module_visible(&self, name: &str) -> bool {
        match name {
            "notifications" => self.show_notifications,
            "volume" => self.show_volume,
            "network" => self.show_network,
            "metrics" => self.show_metrics,
            "clock" => self.show_clock,
            "show_desktop" => self.show_desktop_button,
            "start" => self.show_start_button,
            "tray" => self.show_tray,
            _ => true,
        }
    }

    /// Shows/hides module `name` (a `MODULE_NAMES` entry or "start"). false if the name is unknown.
    pub fn set_module_visible(&mut self, name: &str, on: bool) -> bool {
        match self.module_flag(name) {
            Some(f) => {
                *f = on;
                true
            }
            None => false,
        }
    }
}

impl Config {
    /// Bar background alpha (0..=255) from `bar_opacity` (0..=100 %).
    pub fn bar_alpha(&self) -> u32 {
        (self.bar_opacity.min(100) as u32 * 255 + 50) / 100
    }

    /// Settings window client size in logical px: a dimension that is 0 or above 16384 falls back to
    /// its default, then the result is raised to the minimum.
    pub fn settings_size(&self) -> (i32, i32) {
        let dim = |v: u16, default: i32, min: i32| {
            let v = if v == 0 || v > SETTINGS_WINDOW_MAX { default } else { v as i32 };
            v.max(min)
        };
        let [w, h] = self.settings_window_size;
        (
            dim(w, SETTINGS_WINDOW_DEFAULT.0, SETTINGS_WINDOW_MIN.0),
            dim(h, SETTINGS_WINDOW_DEFAULT.1, SETTINGS_WINDOW_MIN.1),
        )
    }

    /// The four sizes clamped to their ranges: (app icon px, app text px, tray icon px, module %).
    pub fn sizes(&self) -> (u8, u8, u8, u8) {
        (
            APP_ICON_SIZE.clamp(self.app_icon_size),
            APP_TEXT_SIZE.clamp(self.app_text_size),
            TRAY_ICON_SIZE.clamp(self.tray_icon_size),
            MODULE_SIZE.clamp(self.module_size),
        )
    }
}

// ------------------------------------------------------------------ pin / unpin

impl Config {
    /// Index of the pinned entry with this key (case-insensitive comparison).
    pub fn pinned_index(&self, key: &str) -> Option<usize> {
        self.pinned.iter().position(|p| p.key.eq_ignore_ascii_case(key))
    }

    pub fn is_pinned(&self, key: &str) -> bool {
        self.pinned_index(key).is_some()
    }
}

/// Appends `app` to the pins and saves. If the key is already pinned it does nothing (Ok).
pub fn pin(app: PinnedApp) -> Result<(), String> {
    if get().is_pinned(&app.key) {
        return Ok(());
    }
    modify(|c| {
        if !c.is_pinned(&app.key) {
            c.pinned.push(app);
        }
    })
}

/// Updates the stored display name of the pin `key` (fallback name) when it differs from `name`
/// (the live resolver's result). No pin, empty `name` or same name: nothing is written.
pub fn sync_pinned_name(key: &str, name: &str) -> Result<(), String> {
    if name.trim().is_empty() || !pinned_name_differs(&get(), key, name) {
        return Ok(());
    }
    // A hand edit not yet picked up by `poll_changed` would be overwritten by the in-memory snapshot:
    // skip, the self-heal runs again at the next resolve.
    if external_edit_pending() {
        return Ok(());
    }
    modify(|c| set_pinned_name(c, key, name))
}

fn pinned_name_differs(c: &Config, key: &str, name: &str) -> bool {
    c.pinned_index(key).is_some_and(|i| c.pinned[i].name != name)
}

fn set_pinned_name(c: &mut Config, key: &str, name: &str) {
    if let Some(i) = c.pinned_index(key) {
        c.pinned[i].name = name.to_string();
    }
}

/// Self-heal of a pin whose app updated itself into a new versioned folder (Discord: `app-1.0.9265`
/// -> `app-1.0.9267`): stores `exe`, the path the app has now, as the pin's launch target. Only
/// when the stored target is the same exe in another versioned folder (`apps::same_exe_other_version`):
/// an AppsFolder target or a launcher chosen by hand is never touched.
pub fn sync_pinned_launch(key: &str, exe: &str) -> Result<(), String> {
    if !pinned_launch_outdated(&get(), key, exe) || external_edit_pending() {
        return Ok(());
    }
    modify(|c| {
        if pinned_launch_outdated(c, key, exe) {
            if let Some(i) = c.pinned_index(key) {
                c.pinned[i].launch = exe.to_string();
            }
        }
    })
}

fn pinned_launch_outdated(c: &Config, key: &str, exe: &str) -> bool {
    c.pinned_index(key).is_some_and(|i| crate::apps::same_exe_other_version(&c.pinned[i].launch, exe))
}

/// Removes the pin with this key and saves. If it does not exist nothing is written (Ok).
pub fn unpin(key: &str) -> Result<(), String> {
    if !get().is_pinned(key) {
        return Ok(());
    }
    modify(|c| c.pinned.retain(|p| !p.key.eq_ignore_ascii_case(key)))
}

impl Config {
    /// true if the AppsFolder app `id` is pinned in the Start menu (case-insensitive).
    pub fn is_start_pinned(&self, id: &str) -> bool {
        self.start_pins.iter().any(|p| p.eq_ignore_ascii_case(id))
    }

    /// Index of the Start menu folder holding the app `id` (case-insensitive), if any.
    pub fn start_folder_of(&self, id: &str) -> Option<usize> {
        self.start_folders.iter().position(|f| f.apps.iter().any(|a| a.eq_ignore_ascii_case(id)))
    }
}

/// Longest Start menu folder name kept (chars).
const START_FOLDER_NAME_MAX: usize = 32;

/// Takes the app `id` out of every Start menu folder; a folder left empty is dropped.
fn start_folder_remove_in(c: &mut Config, id: &str) {
    for f in &mut c.start_folders {
        f.apps.retain(|a| !a.eq_ignore_ascii_case(id));
    }
    c.start_folders.retain(|f| !f.apps.is_empty());
}

/// The pin that places the folder `folder` in the grid: its first app in `start_pins` order.
fn start_folder_anchor(c: &Config, folder: usize) -> Option<String> {
    let f = c.start_folders.get(folder)?;
    c.start_pins.iter().find(|p| f.apps.iter().any(|a| a.eq_ignore_ascii_case(p))).cloned()
}

/// Moves the pins `ids` (kept in their order) next to the pin `anchor`: right after it when
/// `after`, else right before it. Ids that are not pinned are ignored; nothing moves when the
/// anchor is one of them or is not pinned.
fn start_pin_move_in(c: &mut Config, ids: &[String], anchor: &str, after: bool) {
    let is_moved = |p: &str| ids.iter().any(|i| i.eq_ignore_ascii_case(p));
    if is_moved(anchor) || !c.is_start_pinned(anchor) {
        return;
    }
    let (moved, mut rest): (Vec<String>, Vec<String>) = c.start_pins.drain(..).partition(|p| is_moved(p));
    let at = rest.iter().position(|p| p.eq_ignore_ascii_case(anchor)).map_or(rest.len(), |i| i + usize::from(after));
    rest.splice(at..at, moved);
    c.start_pins = rest;
}

/// The pinned app `src` was dropped on the pinned app `target`: it joins the folder of `target`,
/// or the two make a new folder called `new_name`. The folder keeps the place of `target` (the
/// pin of `src` moves right after the folder's first app). Nothing when either is not pinned.
fn start_folder_join_in(c: &mut Config, src: &str, target: &str, new_name: &str) {
    if src.eq_ignore_ascii_case(target) || !c.is_start_pinned(src) || !c.is_start_pinned(target) {
        return;
    }
    start_folder_remove_in(c, src);
    // The pin that places the folder, taken before `src` is one of its apps.
    let anchor = match c.start_folder_of(target) {
        Some(f) => {
            let anchor = start_folder_anchor(c, f);
            c.start_folders[f].apps.push(src.to_string());
            anchor
        }
        None => {
            let name = new_name.trim().chars().take(START_FOLDER_NAME_MAX).collect();
            c.start_folders.push(StartFolder { name, apps: vec![target.to_string(), src.to_string()] });
            Some(target.to_string())
        }
    };
    if let Some(anchor) = anchor {
        start_pin_move_in(c, &[src.to_string()], &anchor, true);
    }
}

/// Takes the app `id` out of its folder: it stays pinned, right after the folder it left.
fn start_folder_leave_in(c: &mut Config, id: &str) {
    let Some(f) = c.start_folder_of(id) else { return };
    let name = c.start_folders[f].name.clone();
    start_folder_remove_in(c, id);
    // By name: the folder may have been dropped (it was its last app) and the indices shifted.
    let anchor = c.start_folders.iter().position(|x| x.name == name).and_then(|i| start_folder_anchor(c, i));
    if let Some(anchor) = anchor {
        start_pin_move_in(c, &[id.to_string()], &anchor, true);
    }
}

/// Inside the folder `folder`: moves the app `id` next to the app `anchor` (after it when `after`).
fn start_folder_reorder_in(c: &mut Config, folder: usize, id: &str, anchor: &str, after: bool) {
    let Some(f) = c.start_folders.get_mut(folder) else { return };
    let find = |apps: &[String], x: &str| apps.iter().position(|a| a.eq_ignore_ascii_case(x));
    let (Some(from), Some(_)) = (find(&f.apps, id), find(&f.apps, anchor)) else { return };
    if id.eq_ignore_ascii_case(anchor) {
        return;
    }
    let app = f.apps.remove(from);
    let at = find(&f.apps, anchor).map_or(f.apps.len(), |i| i + usize::from(after));
    f.apps.insert(at, app);
}

/// Renames the folder `folder` (trimmed, at most `START_FOLDER_NAME_MAX` chars; an empty name is refused).
fn start_folder_rename_in(c: &mut Config, folder: usize, name: &str) {
    let name: String = name.trim().chars().take(START_FOLDER_NAME_MAX).collect();
    if let (Some(f), false) = (c.start_folders.get_mut(folder), name.is_empty()) {
        f.name = name;
    }
}

/// Start menu drag: moves pins next to another pin (see `start_pin_move_in`) and saves.
pub fn start_pin_move(ids: Vec<String>, anchor: &str, after: bool) -> Result<(), String> {
    modify(|c| start_pin_move_in(c, &ids, anchor, after))
}

/// Start menu drag: a pinned app dropped on another (see `start_folder_join_in`); saves.
pub fn start_folder_join(src: &str, target: &str, new_name: &str) -> Result<(), String> {
    modify(|c| start_folder_join_in(c, src, target, new_name))
}

/// Start menu: takes the app `id` out of its folder (it stays pinned) and saves.
pub fn start_folder_leave(id: &str) -> Result<(), String> {
    modify(|c| start_folder_leave_in(c, id))
}

/// Start menu drag inside an open folder (see `start_folder_reorder_in`); saves.
pub fn start_folder_reorder(folder: usize, id: &str, anchor: &str, after: bool) -> Result<(), String> {
    modify(|c| start_folder_reorder_in(c, folder, id, anchor, after))
}

/// Start menu: renames a folder (see `start_folder_rename_in`) and saves.
pub fn start_folder_rename(folder: usize, name: &str) -> Result<(), String> {
    modify(|c| start_folder_rename_in(c, folder, name))
}

/// Start menu: drops the folder `folder` (its apps stay pinned) and saves.
pub fn start_folder_dissolve(folder: usize) -> Result<(), String> {
    modify(|c| {
        if folder < c.start_folders.len() {
            c.start_folders.remove(folder);
        }
    })
}

/// Pins (appended last) or unpins the AppsFolder app `id` in the Start menu and saves.
fn toggle_start_pin_in(c: &mut Config, id: &str) {
    if c.is_start_pinned(id) {
        c.start_pins.retain(|p| !p.eq_ignore_ascii_case(id));
        start_folder_remove_in(c, id);
    } else if !id.trim().is_empty() {
        c.start_pins.push(id.to_string());
    }
}

/// Start menu right click: pins the app if it is not pinned, unpins it otherwise.
pub fn toggle_start_pin(id: &str) -> Result<(), String> {
    modify(|c| toggle_start_pin_in(c, id))
}

#[cfg(test)]
#[path = "../tests/fixtures/config_tests.rs"]
mod fixtures;

#[cfg(test)]
#[allow(clippy::items_after_test_module)] // clippy: kept as is; moving the items below the tests is churn in a file other streams own
mod tests {
    use super::*;

    #[test]
    fn legacy_dir_line_names_no_profile_path() {
        assert!(LEGACY_DIR_LINE.starts_with("config: the old data folder (%LOCALAPPDATA%\\Dock) is no longer used."));
        assert!(!LEGACY_DIR_LINE.contains(":\\"));
        assert!(!LEGACY_DIR_LINE.contains("Users"));
    }

    #[test]
    fn launcher_defaults_when_missing() {
        let c = parse("{}").unwrap();
        assert_eq!(c.launcher.chord, "off");
        assert!(!c.launcher.offer_done && c.launcher.enabled);
        assert!(!c.launcher.powertoys_done && c.launcher.with_powertoys == "yield");
        assert_eq!(c.launcher.chord_spec(), None);
        assert_eq!(c.launcher, LauncherConfig::default());
        let mut d = LauncherConfig::default();
        assert!(d.sanitize().is_empty(), "the defaults need no fix");
        assert_eq!(d, LauncherConfig::default());
        // the template with the defaults reads back as the defaults
        let r = render(&Config::default());
        assert_eq!(parse(&r).unwrap().launcher, LauncherConfig::default());
        // the PowerToys answer (SPEC-launcher 5.7) is kept through the template
        let mut c = Config::default();
        c.launcher.with_powertoys = "take".into();
        c.launcher.powertoys_done = true;
        assert_eq!(parse(&render(&c)).unwrap().launcher, c.launcher);
    }

    fn sane(f: impl FnOnce(&mut LauncherConfig)) -> (LauncherConfig, Vec<String>) {
        let mut l = LauncherConfig::default();
        f(&mut l);
        let msgs = l.sanitize();
        (l, msgs)
    }

    #[test]
    fn launcher_sanitize_vectors() {
        let (l, m) = sane(|l| l.chord = "ctrl+alt+space".into());
        assert_eq!((l.chord.as_str(), m.len()), ("off", 1));
        let (l, _) = sane(|l| l.chord = "alt+shift+space".into());
        assert!(l.chord_spec().is_some());
        let (l, _) = sane(|l| {
            l.chord = "alt+space".into();
            l.enabled = false;
        });
        assert_eq!(l.chord_spec(), None);
        let (l, _) = sane(|l| {
            l.with_powertoys = "x".into();
            l.monitor = "x".into();
            l.over_fullscreen = "x".into();
        });
        assert_eq!((&l.with_powertoys[..], &l.monitor[..], &l.over_fullscreen[..]), ("yield", "pointer", "pass"));
        let (l, _) = sane(|l| {
            l.top_percent = 3;
            l.rows = 99;
            l.width = 1000;
        });
        assert_eq!((l.top_percent, l.rows, l.width), (10, 12, 880));
        let (l, _) = sane(|l| l.width = 100);
        assert_eq!(l.width, 560);
        let (l, _) = sane(|l| l.width = 661);
        assert_eq!(l.width, 680);
        let (l, _) = sane(|l| l.width = 659);
        assert_eq!(l.width, 640);
        let (l, _) = sane(|l| {
            l.providers.insert("apps".into(), false);
            l.providers.insert("bogus".into(), true);
            l.providers.insert("calc".into(), false);
        });
        assert_eq!(l.providers.get("apps"), Some(&true));
        assert_eq!(l.providers.get("calc"), Some(&false));
        assert!(!l.providers.contains_key("bogus"));
        let (l, _) = sane(|l| {
            l.prefixes.clear();
            for (p, n) in [
                ("x", "calc"),
                ("file", "files"),
                ("f ", "files"),
                ("toolong", "web"),
                ("", "web"),
                ("=", "calc"),
                ("%", "nope"),
                (" ", "calc"),
                ("\t", "calc"),
                ("\n", "calc"),
                ("\u{202e}", "calc"),
                ("a\u{202e} ", "files"),
            ] {
                l.prefixes.insert(p.into(), n.into());
            }
        });
        let keys: Vec<&str> = l.prefixes.keys().map(String::as_str).collect();
        assert_eq!(keys, ["=", "f "]);
    }

    #[test]
    fn launcher_web_url_rules() {
        let (l, _) = sane(|l| {
            l.web_engine = "custom".into();
            l.web_url = "https://example.com/?q={q}".into();
        });
        assert_eq!(l.web_engine, "custom");
        for bad in ["https://user:pw@example.com/?q={q}", "http://example.com/?q={q}", "", "javascript:alert(1)"] {
            let (l, m) = sane(|l| {
                l.web_engine = "custom".into();
                l.web_url = bad.into();
            });
            assert_eq!((l.web_engine.as_str(), l.web_url.as_str()), ("google", ""), "{bad:?}");
            assert_eq!(m.len(), 1);
        }
        let (l, _) = sane(|l| l.web_engine = "yahoo".into());
        assert_eq!(l.web_engine, "google");
    }

    #[test]
    fn launcher_aliases_kept_as_written() {
        // What the alias table refuses stays in the file: a custom scheme, a word with a space, a 17-character word.
        let c = parse(
            r#"{"launcher": {"aliases": {"game": "steam://run/1", "bad word": "x", "seventeen-chars-x": "calc",
            "docs": "https://example.com/docs"}}}"#,
        )
        .unwrap();
        let words: Vec<&str> = c.launcher.aliases.keys().map(String::as_str).collect();
        assert_eq!(words, ["bad word", "docs", "game", "seventeen-chars-x"]);
        assert_eq!(c.launcher.aliases.get("game").map(String::as_str), Some("steam://run/1"));
        // Hard caps only: at most 256 entries, a word and a target of at most 1,024 characters each; one message.
        let (l, m) = sane(|l| {
            for i in 0..257 {
                l.aliases.insert(format!("w{i:03}"), "calc".into());
            }
            l.aliases.insert("long".into(), "x".repeat(1_025));
            l.aliases.insert("y".repeat(1_025), "calc".into());
            l.aliases.insert("edge".into(), "x".repeat(1_024));
        });
        assert_eq!(l.aliases.len(), 256);
        assert!(!l.aliases.contains_key("long") && !l.aliases.keys().any(|k| k.len() > 1_024));
        let capped: Vec<&String> = m.iter().filter(|s| s.starts_with("launcher.aliases")).collect();
        assert_eq!(capped, ["launcher.aliases: 4 entries over a cap dropped"], "{m:?}");
        assert!(m.iter().all(|s| !s.contains("calc") && !s.contains('w')), "no word or target in the log: {m:?}");
        let (l, m) = sane(|l| {
            l.aliases.insert("gh".into(), "https://example.com/".into());
        });
        assert!(m.is_empty() && l.aliases.len() == 1, "{m:?}");
    }

    #[test]
    fn aliases_survive_render() {
        let mut c = Config::default();
        assert!(render(&c).contains("\"aliases\": {}"), "written always, {{}} when empty");
        for (w, t) in [
            ("docs", "https://example.com/docs"),
            ("game", "steam://run/1"),
            ("bad word", "x"),
            ("win", "C:\\Windows"),
            ("q", "\"C:\\Program Files\\Example\\tool.exe\" --a \"b c\""),
            ("u", "ünïcødé → ok"),
        ] {
            c.launcher.aliases.insert(w.into(), t.into());
        }
        let r = render(&c);
        assert_eq!(parse(&r).unwrap(), c, "every entry, those the table refuses included");
        assert_eq!(render(&parse(&r).unwrap()), r);
    }

    #[test]
    fn file_folders_kept_as_written_and_rendered() {
        let mut c = Config::default();
        assert!(render(&c).contains("\"file_folders\": []"), "written always, [] when empty");
        // What the acceptance rule refuses stays in the file: a drive root, a relative path, Windows' own folder.
        let c2 = parse(r#"{"launcher": {"file_folders": ["D:\\", "relative", "C:\\Windows", "E:/Archive"]}}"#).unwrap();
        assert_eq!(c2.launcher.file_folders, ["D:\\", "relative", "C:\\Windows", "E:/Archive"]);
        c.launcher.file_folders = c2.launcher.file_folders;
        let r = render(&c);
        assert_eq!(parse(&r).unwrap(), c, "every entry, those the dock refuses included");
        assert_eq!(render(&parse(&r).unwrap()), r);
        // The comment's examples are JSON text: copied into the list, each is a folder the dock accepts.
        let line = r.lines().find(|l| l.contains("More folders for file search")).unwrap();
        let examples: Vec<String> =
            line.split('"').skip(1).step_by(2).map(|q| serde_json::from_str(&format!("\"{q}\"")).unwrap()).collect();
        assert_eq!(examples, ["D:\\Archive", "E:\\"]);
        let f = crate::launcher::files::Fences {
            windows: "C:\\Windows".into(),
            program_files: vec!["C:\\Program Files".into()],
            profiles: "C:\\Users".into(),
            profile: "C:\\Users\\a".into(),
            accounts: vec!["C:\\Users\\a".into()],
            same: crate::launcher_sys::same_text,
        };
        for e in &examples {
            assert_eq!(crate::launcher::files::root_of(e, &f, true).as_deref(), Some(e.as_str()), "{e:?}");
        }
    }

    #[test]
    fn launcher_section_roundtrips_and_parse_sanitizes() {
        let c = parse(r#"{"launcher": {"chord": "alt+space", "rows": 20, "prefixes": {"file ": "files"}}}"#).unwrap();
        assert_eq!(c.launcher.rows, LAUNCHER_ROWS.2, "out of its range: the default");
        assert_eq!(c.launcher.chord_spec().map(|s| s.vk), Some(0x20));
        let r = render(&c);
        assert_eq!(parse(&r).unwrap(), c);
        assert_eq!(render(&parse(&r).unwrap()), r);
    }

    #[test]
    fn timing_keys_default_and_clamp() {
        let c = parse("{}").unwrap();
        assert_eq!((c.hide_delay_ms, c.show_delay_ms, c.reveal, c.push_threshold), (2500, 150, Reveal::Dwell, 100));
        // The two delays have a range in Settings: a value outside it is an error and the default stands in.
        let (c, errors) = parse_with_errors("{\"hide_delay_ms\": 9000, \"show_delay_ms\": 20000}", None).unwrap();
        assert_eq!((c.hide_delay_ms, c.show_delay_ms, errors.len()), (2500, 150, 2));
        // The push threshold has no Settings row: it is still clamped as it is read.
        assert_eq!(parse("{\"push_threshold\": 1}").unwrap().push_threshold, 20);
        assert_eq!(parse("{\"push_threshold\": 5000}").unwrap().push_threshold, 1000);
        assert_eq!(parse("{\"hide_delay_ms\": 0}").unwrap().hide_delay_ms, 0);
        // A negative number is below the range: that key alone falls back, the file is read.
        let (c, errors) = parse_with_errors("{\"hide_delay_ms\": -5, \"autohide\": false}", None).unwrap();
        assert_eq!((c.hide_delay_ms, c.autohide), (2500, false));
        assert_eq!(errors[0].problem, Problem::Range { min: 0, max: 5000, got: -5 });
        // A number too big for u32 is above the range like any other: that key falls back; the push threshold, clamped
        // as it is read, takes its maximum.
        let (c, errors) = parse_with_errors("{\"hide_delay_ms\": 5000000000}", None).unwrap();
        assert_eq!(c.hide_delay_ms, 2500);
        assert_eq!(errors[0].problem, Problem::Range { min: 0, max: 5000, got: 5_000_000_000 });
        assert_eq!(parse("{\"push_threshold\": 99999999999}").unwrap().push_threshold, 1000);
    }

    #[test]
    fn new_keys_of_the_wrong_type_keep_the_default() {
        // Every natural hand edit with the wrong type: the file still loads, the key reads its default, the rest of
        // the file survives and the load leaves a note.
        let wrong = [
            ("hide_delay_ms", "\"1000\""),
            ("hide_delay_ms", "1500.5"),
            ("hide_delay_ms", "-1"),
            ("hide_delay_ms", "null"),
            ("hide_delay_ms", "true"),
            ("show_delay_ms", "\"fast\""),
            ("show_delay_ms", "[]"),
            ("push_threshold", "2.5"),
            ("push_threshold", "-20"),
            ("keyboard", "null"),
            ("keyboard", "3"),
            ("keyboard", "\"off\""),
            ("tray_privacy_migrated", "\"yes\""),
            ("tray_privacy_migrated", "1"),
        ];
        let want = Config::default();
        for (key, val) in wrong {
            let text = format!("{{ \"{key}\": {val}, \"autohide\": false, \"position\": \"top\" }}");
            let (c, notes) = parse_with_notes(&text).unwrap_or_else(|e| panic!("{text}: {e}"));
            assert!(notes.new_keys_invalid, "{text}");
            assert!(!c.autohide && c.position == Position::Top, "the rest of the file must survive: {text}");
            assert_eq!(
                (c.hide_delay_ms, c.show_delay_ms, c.push_threshold, c.keyboard),
                (want.hide_delay_ms, want.show_delay_ms, want.push_threshold, want.keyboard),
                "{text}"
            );
        }
        // A wrong switch inside `keyboard` leaves the note too; the section is one value, so it reads its default as a
        // whole (the store's rule for a wrong value inside a section, `a_wrong_value_inside_a_section_says_...`).
        let (c, notes) = parse_with_notes("{\"keyboard\": {\"win_t\": \"no\", \"win_b\": false}}").unwrap();
        assert!(notes.new_keys_invalid);
        assert_eq!(c.keyboard, KeyboardConfig::default());
        // Right types leave no note, including the keys the file does not have.
        for ok in ["{}", "{\"hide_delay_ms\": 0, \"show_delay_ms\": 9999, \"keyboard\": {\"peek\": false}}"] {
            assert!(!parse_with_notes(ok).unwrap().1.new_keys_invalid, "{ok}");
        }
        // A syntax error is still the whole file's; a key that is not new in 1.6 loses its value alone too, without
        // the note.
        assert!(parse("{ \"hide_delay_ms\": ").is_err());
        let (c, notes) = parse_with_notes("{\"autohide\": \"yes\", \"position\": \"top\"}").unwrap();
        assert!(c.autohide && c.position == Position::Top && !notes.new_keys_invalid);
    }

    #[test]
    fn keyboard_true_or_false_sets_all_four_switches() {
        let off = KeyboardConfig { win_numbers: false, win_t: false, win_b: false, peek: false };
        let (c, notes) = parse_with_notes("{\"keyboard\": false}").unwrap();
        assert_eq!(c.keyboard, off);
        assert!(!notes.new_keys_invalid);
        assert_eq!(parse("{\"keyboard\": true}").unwrap().keyboard, KeyboardConfig::default());
        // A file that was edited that way is written back as the object.
        assert_eq!(parse(&render(&parse("{\"keyboard\": false}").unwrap())).unwrap().keyboard, off);
    }

    #[test]
    fn wrong_new_key_can_be_written_back_clean() {
        // After the dock writes the file, the keys hold valid values and a reload leaves no note.
        let c = parse("{ \"hide_delay_ms\": \"1000\" }").unwrap();
        assert!(!parse_with_notes(&render(&c)).unwrap().1.new_keys_invalid);
    }

    #[test]
    fn reveal_parses_leniently() {
        for (text, want) in [
            ("\"push\"", Reveal::Push),
            ("\"PUSH\"", Reveal::Push),
            ("\"dwell\"", Reveal::Dwell),
            ("\"x\"", Reveal::Dwell),
            ("3", Reveal::Dwell),
            ("null", Reveal::Dwell),
        ] {
            let c = parse(&format!("{{ \"reveal\": {text}, \"autohide\": false }}")).unwrap();
            assert_eq!(c.reveal, want, "{text}");
            assert!(!c.autohide, "the rest of the file must survive: {text}");
        }
    }

    #[test]
    fn keyboard_defaults_and_partial() {
        let all = KeyboardConfig { win_numbers: true, win_t: true, win_b: true, peek: true };
        assert_eq!(parse("{}").unwrap().keyboard, all);
        assert_eq!(Config::default().keyboard, all);
        let c = parse("{\"keyboard\": {\"win_t\": false}}").unwrap();
        assert_eq!(c.keyboard, KeyboardConfig { win_t: false, ..all });
    }

    #[test]
    fn fresh_install_gets_one_second() {
        assert_eq!(fresh_config().hide_delay_ms, 1000);
        assert!(fresh_config().tray_privacy_migrated);
        // A file without the key has not been migrated yet (the parse then runs the migration and sets it).
        assert!(!serde_json::from_str::<Config>("{}").unwrap().tray_privacy_migrated);
        assert!(parse("{}").unwrap().tray_privacy_migrated);
        assert_eq!(Config::default().hide_delay_ms, 2500);
    }

    #[test]
    fn the_guide_says_what_the_timing_keys_do() {
        // Push is not shipped: the guide says the two keys are not used yet, and offers "push" only when it is.
        assert!(reveal_guide(false).contains("not used yet"));
        assert!(!reveal_guide(false).contains("\"push\" ("));
        assert!(reveal_guide(true).contains("\"push\""));
        assert!(!reveal_guide(true).contains("not used yet"));
        let r = render(&Config::default());
        assert!(r.contains(reveal_guide(crate::ui::PUSH_AVAILABLE)), "{r}");
        // The delay: the fresh-install value and the value of a missing key, in one sentence.
        assert!(r.contains("a new install writes 1000; a missing key reads 2500"), "{r}");
        assert_eq!(fresh_config().hide_delay_ms, 1000);
        assert_eq!(parse("{}").unwrap().hide_delay_ms, 2500);
    }

    #[test]
    fn render_roundtrip_new_keys() {
        let c = Config {
            hide_delay_ms: 0,
            show_delay_ms: 700,
            reveal: Reveal::Push,
            push_threshold: 640,
            keyboard: KeyboardConfig { win_numbers: false, win_t: true, win_b: false, peek: false },
            tray_privacy_migrated: true,
            ..Config::default()
        };
        let r = render(&c);
        assert_eq!(parse(&r).unwrap(), c);
        assert_eq!(render(&parse(&r).unwrap()), r);
        assert!(r.contains("\"reveal\": \"push\""), "{r}");
    }

    fn guids(list: &[&str]) -> Config {
        Config {
            tray_hidden_guids: list.iter().map(|s| s.to_string()).collect(),
            tray_privacy_migrated: false,
            ..Config::default()
        }
    }

    fn new_default() -> Vec<String> {
        DEFAULT_TRAY_HIDDEN_GUIDS.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn privacy_migration_only_for_the_old_default() {
        // The old default, in any order and case: the new default, flag set, changed.
        let mut c = guids(&[
            "7820AE82-23E3-4229-82C1-E41CB67D5B9C",
            "7820ae75-23e3-4229-82c1-e41cb67d5b9c",
            "7820ae74-23e3-4229-82c1-e41cb67d5b9c",
            "7820ae77-23e3-4229-82c1-e41cb67d5b9c",
            "7820ae73-23e3-4229-82c1-e41cb67d5b9c",
        ]);
        assert!(migrate_privacy(&mut c));
        assert_eq!(c.tray_hidden_guids, new_default());
        assert!(c.tray_privacy_migrated);
        // The old default plus one more: the user's list, kept; the flag is set anyway.
        let mut c = guids(&[
            "7820ae73-23e3-4229-82c1-e41cb67d5b9c",
            "7820ae74-23e3-4229-82c1-e41cb67d5b9c",
            "7820ae75-23e3-4229-82c1-e41cb67d5b9c",
            "7820ae77-23e3-4229-82c1-e41cb67d5b9c",
            "7820ae82-23e3-4229-82c1-e41cb67d5b9c",
            "00000000-0000-0000-0000-000000000000",
        ]);
        let before = c.tray_hidden_guids.clone();
        assert!(!migrate_privacy(&mut c) && c.tray_hidden_guids == before && c.tray_privacy_migrated);
        // An empty list, a partial list and the new default stay as they are.
        for list in [&[][..], &["7820ae77-23e3-4229-82c1-e41cb67d5b9c"][..], &DEFAULT_TRAY_HIDDEN_GUIDS[..]] {
            let mut c = guids(list);
            let before = c.tray_hidden_guids.clone();
            assert!(!migrate_privacy(&mut c) && c.tray_hidden_guids == before && c.tray_privacy_migrated);
        }
    }

    #[test]
    fn privacy_migration_runs_once() {
        // A user who hides the indicators again (flag set, old list) keeps them hidden.
        let mut c = guids(&OLD_DEFAULT_TRAY_HIDDEN_GUIDS);
        c.tray_privacy_migrated = true;
        assert!(!migrate_privacy(&mut c));
        assert_eq!(c.tray_hidden_guids.len(), 5);
        // Through the parser: a file with the old list and no flag migrates, and its rewrite does not again.
        let old = render(&guids(&OLD_DEFAULT_TRAY_HIDDEN_GUIDS));
        let (c, notes) = parse_with_notes(&old).unwrap();
        assert!(notes.privacy_migrated && c.tray_hidden_guids == new_default());
        let (_, again) = parse_with_notes(&render(&c)).unwrap();
        assert!(!again.privacy_migrated);
        // The same list with the flag set is the user's choice.
        let mut kept = guids(&OLD_DEFAULT_TRAY_HIDDEN_GUIDS);
        kept.tray_privacy_migrated = true;
        let (c, notes) = parse_with_notes(&render(&kept)).unwrap();
        assert!(!notes.privacy_migrated && c.tray_hidden_guids.len() == 5);
    }

    #[test]
    fn privacy_flag_is_stored_whenever_a_load_set_it() {
        // A file with the user's own list and no flag: the list stays and this load sets the flag, so the file is
        // rewritten (before, only a changed list was): the store rewrites a file a migration changed
        // (`fixtures::a_file_without_the_privacy_flag_is_rewritten_once`), `poll_changed` stores it after a hand edit.
        let list = vec!["00000000-0000-0000-0000-000000000000".to_string()];
        let own = Config { tray_hidden_guids: list, ..Config::default() };
        let text = render(&Config { tray_privacy_migrated: false, ..own.clone() });
        let (c, notes) = parse_with_notes(&text).unwrap();
        assert!(!notes.privacy_migrated && notes.privacy_flag_set);
        assert_eq!(c.tray_hidden_guids, own.tray_hidden_guids);
        // A key that is missing altogether is the same.
        let (_, notes) = parse_with_notes("{}").unwrap();
        assert!(notes.privacy_flag_set);
        // The rewritten file carries the flag: nothing more to write, and a list restored by hand to the old
        // default is the user's choice, not migrated a second time.
        let stored = render(&c);
        let (_, again) = parse_with_notes(&stored).unwrap();
        assert!(!again.privacy_flag_set);
        let restored = Config { tray_hidden_guids: OLD_DEFAULT_TRAY_HIDDEN_GUIDS.map(String::from).to_vec(), ..c };
        let (back, notes) = parse_with_notes(&render(&restored)).unwrap();
        assert!(!notes.privacy_migrated && !notes.privacy_flag_set && back.tray_hidden_guids.len() == 5);
    }

    #[test]
    fn show_battery_note() {
        let note = |v: &str| parse_with_notes(&format!("{{ \"show_battery\": {v} }}")).unwrap().1.show_battery_invalid;
        for bad in ["\"yes\"", "1", "null", "{}", "[]", "\"\""] {
            assert!(note(bad), "{bad}");
        }
        for good in ["true", "false", "\"AUTO\"", "\"false\"", "\"True\"", "\" auto \""] {
            assert!(!note(good), "{good}");
        }
        assert!(!parse_with_notes("{}").unwrap().1.show_battery_invalid);
        // The value itself still reads as "auto".
        assert_eq!(parse("{\"show_battery\": \"yes\"}").unwrap().show_battery, BatteryShow::Auto);
    }

    #[test]
    fn privacy_guids_are_the_indicators() {
        assert!(is_privacy_guid("7820AE82-23E3-4229-82C1-E41CB67D5B9C"));
        assert!(is_privacy_guid("7820ae77-23e3-4229-82c1-e41cb67d5b9c"));
        assert!(!is_privacy_guid("7820ae73-23e3-4229-82c1-e41cb67d5b9c"));
        assert!(!is_privacy_guid(""));
        assert!(DEFAULT_TRAY_HIDDEN_GUIDS.iter().all(|g| !is_privacy_guid(g)));
    }

    #[test]
    fn tray_show_all_defaults_off_and_round_trips() {
        assert!(!parse("{}").unwrap().tray_show_all);
        let c = parse("{\"tray_show_all\": true, \"tray_hidden\": [\"a.exe|0\"]}").unwrap();
        assert!(c.tray_show_all && c.extra.is_empty());
        let back = parse(&render(&c)).unwrap();
        assert!(back.tray_show_all);
        assert_eq!(back.tray_hidden, vec!["a.exe|0".to_string()]);
    }

    #[test]
    fn parse_partial_uses_defaults() {
        let c = parse("{\"position\":\"top\",\"pinned\":[{\"key\":\"a\"}]}").unwrap();
        assert_eq!(c.position, Position::Top);
        assert!(c.autohide && c.show_date && c.window_previews);
        assert_eq!(c.pinned[0].key, "a");
        assert!(c.pinned[0].icon.is_none());
    }

    #[test]
    fn removed_keys_are_ignored() {
        // The tray opt-in moved to HKLM: an old config that still has the key must parse.
        let c = parse("{\"trust_elevated_tray_icons\": true, \"show_tray\": false}").unwrap();
        assert!(!c.show_tray);
    }

    #[test]
    fn parse_bom_and_invalid() {
        assert!(parse("\u{feff}{}").is_ok());
        assert!(parse("{ not json").is_err());
    }

    #[test]
    fn comments_are_stripped_outside_strings() {
        let t = "// hi\n{ /* b */ \"names\": {\"a\": \"x // y \\\" /* z */\"} } // end";
        let c = parse(t).unwrap();
        assert_eq!(c.names["a"], "x // y \" /* z */");
    }

    #[test]
    fn update_keys_default_and_roundtrip() {
        let d = Config::default();
        assert!(d.update_check && !d.update_auto_install);
        assert!(d.last_update_check.is_empty() && d.update_available.is_empty());
        let c = Config {
            update_check: false,
            update_auto_install: true,
            last_update_check: "2026-11-09T12:34:56Z".into(),
            update_available: "v1.4.1".into(),
            ..Config::default()
        };
        let r = render(&c);
        assert!(r.contains("\"update_check\": false") && r.contains("\"update_auto_install\": true"));
        assert!(
            r.contains("\"last_update_check\": \"2026-11-09T12:34:56Z\"")
                && r.contains("\"update_available\": \"v1.4.1\"")
        );
        assert_eq!(parse(&r).unwrap(), c);
        assert!(parse("{}").unwrap().update_check); // an old config.json gets the default
    }

    #[test]
    fn settings_window_size_defaults_and_clamps() {
        let c = Config::default();
        assert_eq!(c.settings_window_size, [860, 600]);
        assert_eq!(c.settings_size(), (860, 600));
        let r = render(&c);
        assert!(r.contains("\"settings_window_size\": [860,600]"));
        assert_eq!(parse(&r).unwrap(), c);
        assert_eq!(parse("{\"settings_window_size\": [100, 100]}").unwrap().settings_size(), (640, 460));
        assert_eq!(parse("{\"settings_window_size\": [0, 20000]}").unwrap().settings_size(), (860, 600));
        assert_eq!(parse("{}").unwrap().settings_window_size, [860, 600]);
    }

    #[test]
    fn template_roundtrip() {
        let mut c = Config::default();
        c.pinned.push(PinnedApp {
            key: "k".into(),
            name: "N".into(),
            launch: "shell:AppsFolder\\X".into(),
            icon: None,
        });
        c.names.insert("code.exe".into(), "VS Code".into());
        c.show_seconds = true;
        c.display_mode = DisplayMode::Text;
        c.position = Position::Top;
        c.monitor = "\\\\.\\DISPLAY2".into();
        c.app_icon_size = 24;
        c.app_text_size = 11;
        c.tray_icon_size = 20;
        c.module_size = 125;
        c.background = Background(0x2E1119);
        c.bar_opacity = 0;
        c.settings_window_size = [1000, 700];
        c.window_previews = false;
        c.win_opens_start = true;
        c.language = Language::It;
        let r = render(&c);
        assert!(r.contains("\"window_previews\": false"));
        assert!(r.contains("\"win_opens_start\": true"));
        assert!(r.contains("\"language\": \"it\""));
        assert!(r.contains("Get-StartApps") && r.contains("\"background\": \"#2E1119\""));
        assert!(!r.contains("\"bar_size\":"));
        assert_eq!(parse(&r).unwrap(), c);
        assert_eq!(parse(&render(&Config::default())).unwrap(), Config::default());
    }

    #[test]
    fn battery_defaults_and_roundtrip() {
        let d = Config::default();
        assert_eq!(d.show_battery, BatteryShow::Auto);
        assert!(d.battery_percent && !d.battery_percent_inside);
        assert_eq!(d.modules_order.len(), 8);
        assert_eq!(d.modules_order[4], "battery");
        assert!(render(&d).contains("\"show_battery\": \"auto\""));
        let c = Config {
            show_battery: BatteryShow::On,
            battery_percent: false,
            battery_percent_inside: true,
            ..Config::default()
        };
        let r = render(&c);
        assert!(r.contains("\"show_battery\": true") && r.contains("\"battery_percent\": false"));
        assert!(r.contains("\"battery_percent_inside\": true"));
        assert_eq!(parse(&r).unwrap(), c);
    }

    #[test]
    fn battery_show_parses_leniently() {
        for (text, want) in [
            ("\"auto\"", BatteryShow::Auto),
            ("\"AUTO\"", BatteryShow::Auto),
            ("true", BatteryShow::On),
            ("false", BatteryShow::Off),
            ("\"true\"", BatteryShow::On),
            ("\"false\"", BatteryShow::Off),
            ("\"yes\"", BatteryShow::Auto),
            ("1", BatteryShow::Auto),
            ("null", BatteryShow::Auto),
        ] {
            let c = parse(&format!("{{ \"show_battery\": {text}, \"autohide\": false }}")).unwrap();
            assert_eq!(c.show_battery, want, "{text}");
            assert!(!c.autohide, "the rest of the file must survive: {text}");
        }
    }

    #[test]
    fn battery_show_resolves_auto_with_presence() {
        assert!(!BatteryShow::Auto.shown(false) && BatteryShow::Auto.shown(true));
        assert!(BatteryShow::On.shown(false) && !BatteryShow::Off.shown(true));
    }

    #[test]
    fn order_fields_roundtrip() {
        let c = Config {
            app_order: vec!["a".into(), "b".into()],
            tray_order: vec!["x#1".into()],
            modules_order: vec!["power".into(), "clock".into()],
            ..Config::default()
        };
        let r = render(&c);
        assert!(r.contains("\"modules_order\""));
        assert_eq!(parse(&r).unwrap(), c);
        assert_eq!(Config::default().modules_order.len(), 8);
    }

    #[test]
    fn module_visibility() {
        let mut c = Config::default();
        assert!(c.module_visible("network") && !c.module_visible("metrics") && c.module_visible("power"));
        for n in MODULE_NAMES {
            assert!(c.set_module_visible(n, false));
            assert!(!c.module_visible(n));
        }
        assert!(!c.set_module_visible("power", false));
        // The Start button is a fixed item: not in MODULE_NAMES, but the switch accepts it.
        assert!(c.set_module_visible("start", false) && !c.show_start_button);
        c.show_metrics = true;
        c.show_desktop_button = false;
        c.show_start_button = false;
        let r = render(&c);
        assert!(r.contains("\"show_network\"") && r.contains("\"show_desktop_button\": false"));
        assert!(r.contains("\"show_start_button\": false"));
        assert_eq!(parse(&r).unwrap(), c);
    }

    #[test]
    fn pinned_name_sync() {
        let mut c = Config::default();
        c.pinned.push(PinnedApp { key: "Stremio".into(), name: "Freedom to Stream".into(), ..Default::default() });
        assert!(pinned_name_differs(&c, "stremio", "Stremio"));
        assert!(!pinned_name_differs(&c, "stremio", "Freedom to Stream"));
        assert!(!pinned_name_differs(&c, "other", "Stremio"));
        set_pinned_name(&mut c, "stremio", "Stremio");
        assert_eq!(c.pinned[0].name, "Stremio");
        assert!(!pinned_name_differs(&c, "stremio", "Stremio"));
    }

    #[test]
    fn start_pins_toggle_and_roundtrip() {
        let mut c = Config::default();
        toggle_start_pin_in(&mut c, "Microsoft.WindowsCalculator_8wekyb3d8bbwe!App");
        toggle_start_pin_in(&mut c, "{6D809377-6AF0-444B-8957-A3773F02200E}\\Git\\git-bash.exe");
        assert_eq!(c.start_pins.len(), 2);
        assert!(c.is_start_pinned("microsoft.windowscalculator_8wekyb3d8bbwe!app"));
        let r = render(&c);
        assert!(r.contains("\"start_pins\""));
        assert_eq!(parse(&r).unwrap(), c);
        // Second toggle (any case) unpins; an empty id is never pinned.
        toggle_start_pin_in(&mut c, "MICROSOFT.WINDOWSCALCULATOR_8WEKYB3D8BBWE!APP");
        toggle_start_pin_in(&mut c, " ");
        assert_eq!(c.start_pins, vec!["{6D809377-6AF0-444B-8957-A3773F02200E}\\Git\\git-bash.exe".to_string()]);
        assert!(Config::default().start_pins.is_empty());
    }

    #[test]
    fn start_folders_group_pins_and_roundtrip() {
        let ids = |v: &[&str]| v.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        let mut c = Config::default();
        for id in ["a", "b", "c", "d", "e"] {
            toggle_start_pin_in(&mut c, id);
        }
        // Only pinned apps make a folder; an app on itself does nothing.
        start_folder_join_in(&mut c, "zzz", "a", "Games");
        start_folder_join_in(&mut c, "a", "a", "Games");
        assert!(c.start_folders.is_empty());
        // "d" dropped on "b": a folder at the place of "b", the pin of "d" right after it.
        start_folder_join_in(&mut c, "d", "b", "Games");
        assert_eq!(c.start_folders, vec![StartFolder { name: "Games".into(), apps: ids(&["b", "d"]) }]);
        assert_eq!(c.start_pins, ids(&["a", "b", "d", "c", "e"]));
        // "A" dropped on the folder (on any of its apps): it joins, the folder stays where it is.
        start_folder_join_in(&mut c, "A", "d", "unused");
        assert_eq!(c.start_folders[0].apps, ids(&["b", "d", "A"]));
        assert_eq!(c.start_pins, ids(&["b", "a", "d", "c", "e"]));
        assert_eq!((c.start_folder_of("a"), c.start_folder_of("c")), (Some(0), None));
        assert_eq!(parse(&render(&c)).unwrap(), c);
        // Order inside the folder, and its name.
        start_folder_reorder_in(&mut c, 0, "a", "b", false);
        assert_eq!(c.start_folders[0].apps, ids(&["A", "b", "d"]));
        start_folder_reorder_in(&mut c, 0, "a", "d", true);
        assert_eq!(c.start_folders[0].apps, ids(&["b", "d", "A"]));
        start_folder_reorder_in(&mut c, 0, "a", "nope", true);
        start_folder_reorder_in(&mut c, 9, "a", "b", true);
        assert_eq!(c.start_folders[0].apps, ids(&["b", "d", "A"]));
        start_folder_rename_in(&mut c, 0, "  Work  ");
        start_folder_rename_in(&mut c, 0, "   ");
        start_folder_rename_in(&mut c, 0, &"x".repeat(100));
        assert_eq!(c.start_folders[0].name.len(), START_FOLDER_NAME_MAX);
        // Moving pins: "e" before "c"; the whole folder (its pins) after "e".
        start_pin_move_in(&mut c, &ids(&["e"]), "c", false);
        assert_eq!(c.start_pins, ids(&["b", "a", "d", "e", "c"]));
        start_pin_move_in(&mut c, &ids(&["b", "a", "d"]), "e", true);
        assert_eq!(c.start_pins, ids(&["e", "b", "a", "d", "c"]));
        // An anchor that is moved or not pinned: nothing changes.
        start_pin_move_in(&mut c, &ids(&["e"]), "e", true);
        start_pin_move_in(&mut c, &ids(&["e"]), "nope", true);
        assert_eq!(c.start_pins, ids(&["e", "b", "a", "d", "c"]));
        // Leaving the folder keeps the pin, right after the folder; unpinning leaves the folder
        // too; a folder left empty goes.
        start_folder_leave_in(&mut c, "D");
        assert!(c.is_start_pinned("d") && c.start_folder_of("d").is_none());
        assert_eq!(c.start_pins, ids(&["e", "b", "d", "a", "c"]));
        toggle_start_pin_in(&mut c, "b");
        start_folder_leave_in(&mut c, "a");
        assert!(c.start_folders.is_empty() && c.is_start_pinned("a"));
        assert!(parse("{}").unwrap().start_folders.is_empty());
    }

    #[test]
    fn start_style_roundtrip_and_fallback() {
        assert_eq!(Config::default().start_style, StartStyle::Compact);
        let r = render(&Config::default());
        assert!(r.contains("\"start_style\": \"compact\""));
        let c = Config { start_style: StartStyle::Windows11, ..Config::default() };
        let r = render(&c);
        assert!(r.contains("\"start_style\": \"windows11\""));
        assert_eq!(parse(&r).unwrap(), c);
        assert_eq!(parse(&serde_json::to_string(&c).unwrap()).unwrap(), c);
        // Any case is read, and the names used up to 1.4.0; unknown values fall back to compact
        // without rejecting the file.
        assert_eq!(parse("{\"start_style\": \"Windows11\"}").unwrap().start_style, StartStyle::Windows11);
        assert_eq!(parse("{\"start_style\": \"Horizontal\"}").unwrap().start_style, StartStyle::Windows11);
        assert_eq!(parse("{\"start_style\": \"vertical\"}").unwrap().start_style, StartStyle::Compact);
        let odd = parse("{\"start_style\": \"diagonal\", \"autohide\": false}").unwrap();
        assert_eq!(odd.start_style, StartStyle::Compact);
        assert!(!odd.autohide);
        // An old config without the key gets the original menu.
        assert_eq!(parse("{}").unwrap().start_style, StartStyle::Compact);
    }

    #[test]
    fn bar_alpha_is_clamped() {
        let mut c = Config::default();
        assert_eq!(c.bar_alpha(), 242);
        c.bar_opacity = 100;
        assert_eq!(c.bar_alpha(), 255);
        c.bar_opacity = 0;
        assert_eq!(c.bar_alpha(), 0);
        c.bar_opacity = 50;
        assert_eq!(c.bar_alpha(), 128);
        c.bar_opacity = 200;
        assert_eq!(c.bar_alpha(), 255);
    }

    #[test]
    fn background_values() {
        let bg = |v: &str| parse(&format!("{{\"background\": \"{v}\", \"autohide\": false}}")).unwrap().background;
        assert_eq!(bg("black"), Background::BLACK);
        assert_eq!(bg("Dark"), Background::DARK);
        assert_eq!(bg("#1a2B3c"), Background(0x1A2B3C));
        assert_eq!(bg("#fff"), Background(0xFFFFFF));
        // Garbage falls back to black instead of rejecting the file.
        assert_eq!(bg("#12345"), Background::BLACK);
        assert_eq!(bg("teal"), Background::BLACK);
        assert_eq!(Background(0x0E1A33).hex(), "#0E1A33");
        // Presets are dark: white text stays readable (Rec. 601 brightness below 60 / 255).
        assert!(BACKGROUND_PRESETS.iter().all(|(_, b)| {
            let (r, g, b) = b.rgb();
            (r * 299 + g * 587 + b * 114) / 1000 < 60
        }));
    }

    #[test]
    fn legacy_bar_size_migrates() {
        let c = parse("{\"bar_size\": \"large\"}").unwrap();
        assert_eq!(c.sizes(), (20, 16, 20, 125));
        assert!(c.extra.is_empty(), "bar_size is read once, never kept");
        assert_eq!(parse("{\"bar_size\": \"small\"}").unwrap().sizes(), (14, 11, 14, 88));
        assert_eq!(parse("{\"bar_size\": \"medium\"}").unwrap(), Config::default());
        // New keys already set: they win.
        assert_eq!(parse("{\"bar_size\": \"large\", \"app_icon_size\": 30}").unwrap().sizes(), (30, 13, 16, 100));
        // Out-of-range values are errors and the defaults stand in (the clamps on use stay as a second fence).
        assert_eq!(parse("{\"app_icon_size\": 200, \"module_size\": 5}").unwrap().sizes(), (16, 13, 16, 100));
    }

    #[test]
    fn audio_layout_default_and_serialization() {
        // Default: the original popup, written as "MOSX"; an old config without the key gets it.
        let d = AudioLayout::default();
        assert_eq!(d.encode(), "MOSX");
        assert_eq!(d.order(), &[AUDIO_MASTER, AUDIO_OUTPUT, AUDIO_SPATIAL, AUDIO_MIXER]);
        assert!((0..4).all(|s| d.is_open(s) && !d.is_hidden(s) && d.loadable(s)));
        assert_eq!(parse("{\"show_tray\": false}").unwrap().audio_layout, d);
        assert_eq!(AudioLayout::parse(""), d);
        assert_eq!(AudioLayout::parse("MOSX"), d);
        assert_eq!(std::mem::size_of::<AudioLayout>(), 6);
        // Collapsed and hidden sections round-trip through the template.
        let c = Config { audio_layout: AudioLayout::parse("M-oXS"), ..Config::default() };
        assert_eq!(c.audio_layout.encode(), "M-oXS");
        assert!(c.audio_layout.is_hidden(AUDIO_OUTPUT) && !c.audio_layout.is_open(AUDIO_OUTPUT));
        let r = render(&c);
        assert!(r.contains("\"audio_layout\": \"M-oXS\""));
        assert_eq!(parse(&r).unwrap(), c);
        // Tolerant parse: junk and repeats ignored, missing sections appended.
        assert_eq!(AudioLayout::parse("x, m ?? X").encode(), "xmOS");
        assert_eq!(AudioLayout::parse("so").encode(), "soMX");
        // A value of the wrong type is an error of that key alone, like any other field: the default stands in.
        let (c, errors) = parse_with_errors("{\"audio_layout\": 5, \"show_tray\": false}", None).unwrap();
        assert_eq!((c.audio_layout, c.show_tray, errors.len()), (d, false, 1));
    }

    #[test]
    fn audio_layout_old_group_strings_are_flattened() {
        // The grouped sections take the group's place with their own flags; the group's own flag
        // (G expanded / g collapsed) is ignored.
        assert_eq!(AudioLayout::parse("M-oG(sX)").encode(), "M-osX");
        assert_eq!(AudioLayout::parse("Og(-xM)S").encode(), "O-xMS");
        assert_eq!(AudioLayout::parse("g(m)OSX").encode(), "mOSX");
        assert_eq!(AudioLayout::parse("G()so").encode(), "soMX");
        assert_eq!(AudioLayout::parse("gG(m)O").encode(), "mOSX");
        // A stray '-' before the group marker hides nothing.
        assert_eq!(AudioLayout::parse("-G(s)MOX").encode(), "sMOX");
        // Old configs load through the file parser too.
        let c = parse("{\"audio_layout\": \"G(Xs)Mo\"}").unwrap();
        assert_eq!(c.audio_layout.order(), &[AUDIO_MIXER, AUDIO_SPATIAL, AUDIO_MASTER, AUDIO_OUTPUT]);
        assert!(c.audio_layout.loadable(AUDIO_MIXER) && !c.audio_layout.loadable(AUDIO_SPATIAL));
    }

    #[test]
    fn audio_layout_move_to() {
        let mut l = AudioLayout::default();
        assert!(!l.move_to(AUDIO_MASTER, 0));
        assert!(!l.move_to(9, 0));
        assert!(l.move_to(AUDIO_MASTER, 2));
        assert_eq!(l.encode(), "OSMX");
        assert!(l.move_to(AUDIO_MIXER, 0));
        assert_eq!(l.encode(), "XOSM");
        // Past the end = last.
        assert!(l.move_to(AUDIO_OUTPUT, 99));
        assert_eq!(l.encode(), "XSMO");
        assert!(!l.move_to(AUDIO_OUTPUT, 3));
        // Flags travel with the section.
        l.toggle_hidden(AUDIO_SPATIAL);
        l.toggle_open(AUDIO_SPATIAL);
        assert!(l.move_to(AUDIO_SPATIAL, 3));
        assert_eq!(l.encode(), "XMO-s");
        assert_eq!(AudioLayout::parse(&l.encode()), l);
    }

    #[test]
    fn audio_layout_loadable() {
        let mut l = AudioLayout::default();
        l.toggle_open(AUDIO_OUTPUT);
        assert!(!l.loadable(AUDIO_OUTPUT));
        l.toggle_open(AUDIO_OUTPUT);
        l.toggle_hidden(AUDIO_OUTPUT);
        assert!(!l.loadable(AUDIO_OUTPUT));
        l.toggle_hidden(AUDIO_OUTPUT);
        assert!(l.loadable(AUDIO_OUTPUT));
        assert!(!l.loadable(4));
    }

    #[test]
    fn roundtrip() {
        let mut c = Config::default();
        c.pinned.push(PinnedApp { key: "k".into(), name: "N".into(), launch: "L".into(), icon: Some("i.png".into()) });
        c.names.insert("code.exe".into(), "VS Code".into());
        assert_eq!(parse(&serde_json::to_string_pretty(&c).unwrap()).unwrap(), c);
    }

    #[test]
    fn a_write_in_the_folder_signals_the_notification() {
        use windows::Win32::Foundation::{HANDLE, WAIT_OBJECT_0};
        use windows::Win32::Storage::FileSystem::{
            FindCloseChangeNotification, FindFirstChangeNotificationW, FindNextChangeNotification,
            FILE_NOTIFY_CHANGE_FILE_NAME, FILE_NOTIFY_CHANGE_LAST_WRITE, FILE_NOTIFY_CHANGE_SIZE,
        };
        use windows::Win32::System::Threading::WaitForSingleObject;

        let dir = std::env::temp_dir().join(format!("dock-cfgwatch-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let wide: Vec<u16> = std::os::windows::ffi::OsStrExt::encode_wide(dir.as_os_str()).chain(Some(0)).collect();
        // SAFETY: `wide` is NUL-terminated and outlives the call.
        let h: HANDLE = unsafe {
            FindFirstChangeNotificationW(
                windows::core::PCWSTR(wide.as_ptr()),
                false,
                FILE_NOTIFY_CHANGE_FILE_NAME | FILE_NOTIFY_CHANGE_LAST_WRITE | FILE_NOTIFY_CHANGE_SIZE,
            )
        }
        .unwrap();
        let file = dir.join("config.json");
        std::fs::write(&file, b"{}").unwrap();
        // SAFETY: `h` is the notification handle opened above.
        assert_eq!(unsafe { WaitForSingleObject(h, 2000) }, WAIT_OBJECT_0);
        // SAFETY: as above: re-arms the handle.
        unsafe { FindNextChangeNotification(h) }.unwrap();
        std::fs::write(&file, b"{\"a\":1}").unwrap();
        // SAFETY: as above.
        assert_eq!(unsafe { WaitForSingleObject(h, 2000) }, WAIT_OBJECT_0);
        // SAFETY: closes the handle once.
        unsafe { FindCloseChangeNotification(h) }.unwrap();
        let _ = std::fs::remove_dir_all(&dir);
    }
}

// ---------------------------------------------------- effective options (CLI > config)

/// Effective position: `--top`/`--bottom` if present, otherwise the config.
pub fn effective_position() -> Position {
    match crate::args().top {
        Some(true) => Position::Top,
        Some(false) => Position::Bottom,
        None => get().position,
    }
}

/// Effective auto-hide: `--no-autohide` forces false, otherwise the config.
pub fn effective_autohide() -> bool {
    !crate::args().no_autohide && get().autohide
}

/// Hide the native taskbar: `--keep-taskbar` forces false, otherwise the config.
pub fn effective_hide_native_taskbar() -> bool {
    !crate::args().keep_taskbar && get().hide_native_taskbar
}
