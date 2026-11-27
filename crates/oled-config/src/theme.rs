//! The theme file <root>\suite\theme.json (F-P2).

pub const THEME_SCHEMA: u16 = 1;
pub const THEME_DIR: &str = "suite"; // <root>\suite\theme.json, <root> = %ProgramData%\mini-oled-dock
pub const THEME_FILE: &str = "theme.json";
pub const THEME_MAX_BYTES: u64 = 64 * 1024;
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Rgb(pub u8, pub u8, pub u8); // JSON "#RRGGBB"
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Lang {
    #[default]
    Auto,
    En,
    It,
} // "auto" "en" "it"
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Preset {
    Oled,
    Classic14,
    Hairline,
    Night,
    Graphite,
    Contrast,
    Custom,
}
// "oled" "classic14" "hairline" "night" "graphite" "contrast" "custom"
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Corners {
    Square,
    Half,
    Normal,
    Round,
} // JSON numbers 0, 0.5, 1, 1.3
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RimStyle {
    Off,
    Hairline,
    Strong,
} // "off" "hairline" "strong"
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PlateStyle {
    Fill,
    Rim,
    Underline,
} // "fill" "rim" "underline"
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum IndicatorStyle {
    Line,
    Dot,
    Off,
} // "line" "dot" "off"
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Motion {
    System,
    Full,
    Reduced,
    Off,
} // "system" "full" "reduced" "off"
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Seed {
    pub bg: Rgb,
    pub fg: Rgb,
    pub accent: Option<Rgb>,
    pub attention: Rgb,
    pub danger: Rgb,
    pub white_level: u8, /* 70..=100 % */
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FontSpec {
    pub family: String,   /* "" = the dock's face, <= 64 chars */
    pub weight_delta: i8, /* -100..=100 */
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Sizes {
    pub icon: u8,       /* 14..=32 */
    pub text: u8,       /* 10..=18 */
    pub tray: u8,       /* 12..=24 */
    pub module_pct: u8, /* 75..=150 */
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct MotionSpec {
    pub mode: Motion,
    pub speed_pct: u8, /* 75..=125 */
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Knobs {
    pub bar_opacity: u8,     /* 0..=100 % */
    pub surface_opacity: u8, /* 0..=100 % */
    pub corners: Corners,
    pub density_pct: u8, /* 85..=115 */
    pub rim: RimStyle,
    pub plate: PlateStyle,
    pub indicator: IndicatorStyle,
    pub font: FontSpec,
    pub sizes: Sizes,
    pub motion: MotionSpec,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Look {
    pub preset: Preset,
    pub seed: Seed,
    pub knobs: Knobs,
}
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct A11y {
    pub reduced_motion: Option<bool>,
    pub text_scale_pct: Option<u16>, /* 100..=225 */
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ThemeFile {
    pub schema: u16,
    pub generation: u64,
    pub writer: String, /* <= 64 chars */
    pub language: Lang,
    pub look: Look,
    pub overrides: std::collections::BTreeMap<String, i64>, /* <= 64 entries, keys [a-z0-9_.]{1,64} */
    pub a11y: A11y,
}
use std::collections::BTreeMap;

use std::path::{Path, PathBuf};

use serde_json::{Map, Value};
use windows::core::HSTRING;
use windows::Win32::Foundation::HANDLE;
use windows::Win32::Storage::FileSystem::{
    FindCloseChangeNotification, FindFirstChangeNotificationW, FindNextChangeNotification,
    FILE_NOTIFY_CHANGE_FILE_NAME, FILE_NOTIFY_CHANGE_LAST_WRITE,
};

const NOTES_MAX: usize = 64;
const OVERRIDES_MAX: usize = 64;
const TEXT_MAX: usize = 64;

const LANGS: &[(&str, Lang)] = &[("auto", Lang::Auto), ("en", Lang::En), ("it", Lang::It)];
const PRESETS: &[(&str, Preset)] = &[
    ("oled", Preset::Oled),
    ("classic14", Preset::Classic14),
    ("hairline", Preset::Hairline),
    ("night", Preset::Night),
    ("graphite", Preset::Graphite),
    ("contrast", Preset::Contrast),
    ("custom", Preset::Custom),
];
const RIMS: &[(&str, RimStyle)] =
    &[("off", RimStyle::Off), ("hairline", RimStyle::Hairline), ("strong", RimStyle::Strong)];
const PLATES: &[(&str, PlateStyle)] =
    &[("fill", PlateStyle::Fill), ("rim", PlateStyle::Rim), ("underline", PlateStyle::Underline)];
const INDICATORS: &[(&str, IndicatorStyle)] =
    &[("line", IndicatorStyle::Line), ("dot", IndicatorStyle::Dot), ("off", IndicatorStyle::Off)];
const MOTIONS: &[(&str, Motion)] =
    &[("system", Motion::System), ("full", Motion::Full), ("reduced", Motion::Reduced), ("off", Motion::Off)];
const CORNERS: &[(&str, f64, Corners)] = &[
    ("0", 0.0, Corners::Square),
    ("0.5", 0.5, Corners::Half),
    ("1", 1.0, Corners::Normal),
    ("1.3", 1.3, Corners::Round),
];

fn name_of<T: Copy + PartialEq>(table: &'static [(&'static str, T)], v: T) -> &'static str {
    table.iter().find(|(_, t)| *t == v).map_or("", |(n, _)| n)
}

fn corners_text(c: Corners) -> &'static str {
    CORNERS.iter().find(|(_, _, v)| *v == c).map_or("1", |(n, _, _)| n)
}

fn default_seed() -> Seed {
    Seed {
        bg: Rgb(0, 0, 0),
        fg: Rgb(255, 255, 255),
        accent: None,
        attention: Rgb(0xFF, 0x8C, 0x00),
        danger: Rgb(0xFF, 0x45, 0x3A),
        white_level: 100,
    }
}

fn default_knobs() -> Knobs {
    Knobs {
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
    }
}

impl Default for ThemeFile {
    /// Exactly the values of tests/fixtures/theme-default.json.
    fn default() -> Self {
        ThemeFile {
            schema: THEME_SCHEMA,
            generation: 0,
            writer: String::new(),
            language: Lang::Auto,
            look: Look { preset: Preset::Oled, seed: default_seed(), knobs: default_knobs() },
            overrides: BTreeMap::new(),
            a11y: A11y::default(),
        }
    }
}

// ------------------------------------------------------------------ parse

/// A JSON value shortened for a note (escaped by the JSON writer, so it carries no control characters).
fn show(v: &Value) -> String {
    let s = v.to_string();
    if s.chars().count() > 40 {
        let cut: String = s.chars().take(37).collect();
        format!("{cut}...")
    } else {
        s
    }
}

fn read_int(v: &Value) -> Option<i64> {
    let Value::Number(n) = v else { return None };
    if let Some(i) = n.as_i64() {
        return Some(i);
    }
    if n.as_u64().is_some() {
        return Some(i64::MAX);
    }
    let f = n.as_f64()?;
    (f.is_finite() && f.fract() == 0.0).then_some(f as i64)
}

fn parse_rgb(s: &str) -> Option<Rgb> {
    let hex = s.strip_prefix('#')?;
    if hex.len() != 6 || !hex.bytes().all(|b| b.is_ascii_hexdigit()) {
        return None;
    }
    let h = |i: usize| hex.get(i..i + 2).and_then(|p| u8::from_str_radix(p, 16).ok());
    Some(Rgb(h(0)?, h(2)?, h(4)?))
}

fn rgb_text(c: Rgb) -> String {
    format!("#{:02X}{:02X}{:02X}", c.0, c.1, c.2)
}

fn valid_override_key(k: &str) -> bool {
    (1..=64).contains(&k.len()) && k.bytes().all(|b| matches!(b, b'a'..=b'z' | b'0'..=b'9' | b'_' | b'.'))
}

struct Reader {
    notes: Vec<String>,
}

impl Reader {
    fn note(&mut self, s: String) {
        if self.notes.len() < NOTES_MAX {
            self.notes.push(s);
        }
    }

    fn section<'a>(
        &mut self,
        o: Option<&'a Map<String, Value>>,
        key: &str,
        path: &str,
    ) -> Option<&'a Map<String, Value>> {
        match o?.get(key) {
            None => None,
            Some(Value::Object(m)) => Some(m),
            Some(_) => {
                self.note(format!("{path}: not an object (defaults kept)"));
                None
            }
        }
    }

    fn choice<T: Copy>(
        &mut self,
        o: Option<&Map<String, Value>>,
        key: &str,
        path: &str,
        table: &[(&str, T)],
        default: T,
    ) -> T {
        let Some(v) = o.and_then(|o| o.get(key)) else { return default };
        if let Value::String(s) = v {
            if let Some((_, t)) = table.iter().find(|(n, _)| n == s) {
                return *t;
            }
        }
        let names: Vec<&str> = table.iter().map(|(n, _)| *n).collect();
        self.note(format!("{path}: {} is not one of {} (default kept)", show(v), names.join(", ")));
        default
    }

    fn int(&mut self, o: Option<&Map<String, Value>>, key: &str, path: &str, min: i64, max: i64, default: i64) -> i64 {
        let Some(v) = o.and_then(|o| o.get(key)) else { return default };
        let Some(n) = read_int(v) else {
            self.note(format!("{path}: {} is not a whole number (default kept)", show(v)));
            return default;
        };
        if n < min {
            self.note(format!("{path}: {n} is below {min} (clamped)"));
            min
        } else if n > max {
            self.note(format!("{path}: {n} is above {max} (clamped)"));
            max
        } else {
            n
        }
    }

    fn rgb(&mut self, o: Option<&Map<String, Value>>, key: &str, path: &str, default: Rgb) -> Rgb {
        let Some(v) = o.and_then(|o| o.get(key)) else { return default };
        if let Some(c) = v.as_str().and_then(parse_rgb) {
            return c;
        }
        self.note(format!("{path}: {} is not \"#RRGGBB\" (default kept)", show(v)));
        default
    }

    fn text(&mut self, o: Option<&Map<String, Value>>, key: &str, path: &str) -> String {
        let Some(v) = o.and_then(|o| o.get(key)) else { return String::new() };
        match v.as_str() {
            Some(s) if s.chars().count() <= TEXT_MAX => s.to_owned(),
            Some(_) => {
                self.note(format!("{path}: longer than {TEXT_MAX} characters (default kept)"));
                String::new()
            }
            None => {
                self.note(format!("{path}: {} is not a string (default kept)", show(v)));
                String::new()
            }
        }
    }

    fn corners(&mut self, o: Option<&Map<String, Value>>, path: &str) -> Corners {
        let Some(v) = o.and_then(|o| o.get("corners")) else { return Corners::Normal };
        if let Value::Number(n) = v {
            if let Some(f) = n.as_f64() {
                if let Some((_, _, c)) = CORNERS.iter().find(|(_, x, _)| *x == f) {
                    return *c;
                }
            }
        }
        self.note(format!("{path}: {} is not one of 0, 0.5, 1, 1.3 (default kept)", show(v)));
        Corners::Normal
    }

    fn accent(&mut self, o: Option<&Map<String, Value>>, path: &str) -> Option<Rgb> {
        match o.and_then(|o| o.get("accent")) {
            None | Some(Value::Null) => None,
            Some(v) => {
                let c = v.as_str().and_then(parse_rgb);
                if c.is_none() {
                    self.note(format!("{path}: {} is not \"#RRGGBB\" or null (default kept)", show(v)));
                }
                c
            }
        }
    }

    fn generation(&mut self, o: &Map<String, Value>) -> u64 {
        let Some(v) = o.get("generation") else { return 0 };
        if let Some(n) = v.as_u64() {
            return n;
        }
        match read_int(v) {
            Some(n) if n < 0 => {
                self.note(format!("generation: {n} is below 0 (clamped)"));
                0
            }
            Some(n) => n as u64,
            None => {
                self.note(format!("generation: {} is not a whole number (default kept)", show(v)));
                0
            }
        }
    }

    fn overrides(&mut self, o: &Map<String, Value>) -> BTreeMap<String, i64> {
        let mut out = BTreeMap::new();
        let Some(v) = o.get("overrides") else { return out };
        let Value::Object(m) = v else {
            self.note("overrides: not an object (defaults kept)".to_owned());
            return out;
        };
        for (k, val) in m {
            if !valid_override_key(k) {
                self.note("overrides: a key is not 1 to 64 characters of a-z 0-9 _ . (entry ignored)".to_owned());
                continue;
            }
            match read_int(val) {
                Some(n) => {
                    out.insert(k.clone(), n);
                }
                None => self.note(format!("overrides.{k}: {} is not a whole number (entry ignored)", show(val))),
            }
        }
        if out.len() > OVERRIDES_MAX {
            self.note(format!("overrides: more than {OVERRIDES_MAX} entries (the extra ones are ignored)"));
            let keep: Vec<String> = out.keys().take(OVERRIDES_MAX).cloned().collect();
            out.retain(|k, _| keep.contains(k));
        }
        out
    }
}

/// Reads a theme file. Err only when the whole file is unusable (too large, not UTF-8, not a JSON object); every
/// field is read on its own, a bad one takes its default (or is clamped) and adds a note. Total: never panics.
pub fn parse(bytes: &[u8]) -> Result<(ThemeFile, Vec<String>), String> {
    if bytes.len() as u64 > THEME_MAX_BYTES {
        return Err(format!("larger than {THEME_MAX_BYTES} bytes"));
    }
    let bytes = bytes.strip_prefix(&[0xEF, 0xBB, 0xBF]).unwrap_or(bytes);
    let text = std::str::from_utf8(bytes).map_err(|_| "not valid UTF-8".to_owned())?;
    let root: Value = serde_json::from_str(text).map_err(|e| format!("not valid JSON: {e}"))?;
    let Value::Object(root) = root else { return Err("not a JSON object".to_owned()) };

    let mut r = Reader { notes: Vec::new() };
    let mut t = ThemeFile::default();

    if root.contains_key("schema") {
        let s = r.int(Some(&root), "schema", "schema", 1, i64::MAX, 1);
        if s > i64::from(THEME_SCHEMA) {
            r.note(format!("schema: {s} is newer than {THEME_SCHEMA} (read as {THEME_SCHEMA})"));
        }
    }
    t.generation = r.generation(&root);
    t.writer = r.text(Some(&root), "writer", "writer");
    t.language = r.choice(Some(&root), "language", "language", LANGS, Lang::Auto);

    let look = r.section(Some(&root), "look", "look");
    match look.and_then(|l| l.get("preset")) {
        None => {}
        Some(Value::String(s)) => match PRESETS.iter().find(|(n, _)| n == s) {
            Some((_, p)) => t.look.preset = *p,
            None => {
                r.note(format!(
                    "look.preset: {} is not a known preset (read as custom)",
                    show(&Value::String(s.clone()))
                ));
                t.look.preset = Preset::Custom;
            }
        },
        Some(v) => r.note(format!("look.preset: {} is not a string (default kept)", show(v))),
    }

    let d = default_seed();
    let seed = r.section(look, "seed", "look.seed");
    t.look.seed = Seed {
        bg: r.rgb(seed, "bg", "look.seed.bg", d.bg),
        fg: r.rgb(seed, "fg", "look.seed.fg", d.fg),
        accent: r.accent(seed, "look.seed.accent"),
        attention: r.rgb(seed, "attention", "look.seed.attention", d.attention),
        danger: r.rgb(seed, "danger", "look.seed.danger", d.danger),
        white_level: r.int(seed, "white_level", "look.seed.white_level", 70, 100, 100) as u8,
    };

    let d = default_knobs();
    let k = r.section(look, "knobs", "look.knobs");
    let font = r.section(k, "font", "look.knobs.font");
    let sizes = r.section(k, "sizes", "look.knobs.sizes");
    let motion = r.section(k, "motion", "look.knobs.motion");
    t.look.knobs = Knobs {
        bar_opacity: r.int(k, "bar_opacity", "look.knobs.bar_opacity", 0, 100, 95) as u8,
        surface_opacity: r.int(k, "surface_opacity", "look.knobs.surface_opacity", 0, 100, 95) as u8,
        corners: r.corners(k, "look.knobs.corners"),
        density_pct: r.int(k, "density_pct", "look.knobs.density_pct", 85, 115, 100) as u8,
        rim: r.choice(k, "rim", "look.knobs.rim", RIMS, d.rim),
        plate: r.choice(k, "plate", "look.knobs.plate", PLATES, d.plate),
        indicator: r.choice(k, "indicator", "look.knobs.indicator", INDICATORS, d.indicator),
        font: FontSpec {
            family: r.text(font, "family", "look.knobs.font.family"),
            weight_delta: r.int(font, "weight_delta", "look.knobs.font.weight_delta", -100, 100, 0) as i8,
        },
        sizes: Sizes {
            icon: r.int(sizes, "icon", "look.knobs.sizes.icon", 14, 32, 16) as u8,
            text: r.int(sizes, "text", "look.knobs.sizes.text", 10, 18, 13) as u8,
            tray: r.int(sizes, "tray", "look.knobs.sizes.tray", 12, 24, 16) as u8,
            module_pct: r.int(sizes, "module_pct", "look.knobs.sizes.module_pct", 75, 150, 100) as u8,
        },
        motion: MotionSpec {
            mode: r.choice(motion, "mode", "look.knobs.motion.mode", MOTIONS, Motion::System),
            speed_pct: r.int(motion, "speed_pct", "look.knobs.motion.speed_pct", 75, 125, 100) as u8,
        },
    };

    t.overrides = r.overrides(&root);

    let a = r.section(Some(&root), "a11y", "a11y");
    t.a11y.reduced_motion = match a.and_then(|a| a.get("reduced_motion")) {
        None | Some(Value::Null) => None,
        Some(Value::Bool(b)) => Some(*b),
        Some(v) => {
            r.note(format!("a11y.reduced_motion: {} is not true, false or null (default kept)", show(v)));
            None
        }
    };
    t.a11y.text_scale_pct = match a.and_then(|a| a.get("text_scale_pct")) {
        None | Some(Value::Null) => None,
        Some(v) if read_int(v).is_some() => {
            Some(r.int(a, "text_scale_pct", "a11y.text_scale_pct", 100, 225, 100) as u16)
        }
        Some(v) => {
            r.note(format!("a11y.text_scale_pct: {} is not a whole number or null (default kept)", show(v)));
            None
        }
    };

    Ok((t, r.notes))
}

// ------------------------------------------------------------------ write

fn json_str(s: &str) -> String {
    serde_json::to_string(s).unwrap_or_else(|_| "\"\"".to_owned())
}

fn opt_json<T: std::fmt::Display>(v: Option<T>) -> String {
    v.map_or_else(|| "null".to_owned(), |v| v.to_string())
}

/// The text of a theme file in the one layout F-P2 fixes (LF, no BOM, two-space indent, fixed key order).
pub fn to_json(t: &ThemeFile) -> String {
    let s = &t.look.seed;
    let k = &t.look.knobs;
    let accent = s.accent.map_or_else(|| "null".to_owned(), |c| format!("\"{}\"", rgb_text(c)));
    let overrides = if t.overrides.is_empty() {
        "{}".to_owned()
    } else {
        let rows: Vec<String> = t.overrides.iter().map(|(k, v)| format!("    {}: {v}", json_str(k))).collect();
        format!("{{\n{}\n  }}", rows.join(",\n"))
    };
    format!(
        concat!(
            "{{\n",
            "  \"schema\": {schema},\n",
            "  \"generation\": {generation},\n",
            "  \"writer\": {writer},\n",
            "  \"language\": \"{language}\",\n",
            "  \"look\": {{\n",
            "    \"preset\": \"{preset}\",\n",
            "    \"seed\": {{\n",
            "      \"bg\": \"{bg}\",\n",
            "      \"fg\": \"{fg}\",\n",
            "      \"accent\": {accent},\n",
            "      \"attention\": \"{attention}\",\n",
            "      \"danger\": \"{danger}\",\n",
            "      \"white_level\": {white_level}\n",
            "    }},\n",
            "    \"knobs\": {{\n",
            "      \"bar_opacity\": {bar_opacity},\n",
            "      \"surface_opacity\": {surface_opacity},\n",
            "      \"corners\": {corners},\n",
            "      \"density_pct\": {density_pct},\n",
            "      \"rim\": \"{rim}\",\n",
            "      \"plate\": \"{plate}\",\n",
            "      \"indicator\": \"{indicator}\",\n",
            "      \"font\": {{ \"family\": {family}, \"weight_delta\": {weight_delta} }},\n",
            "      \"sizes\": {{ \"icon\": {icon}, \"text\": {text}, \"tray\": {tray}, \"module_pct\": {module_pct} }},\n",
            "      \"motion\": {{ \"mode\": \"{motion}\", \"speed_pct\": {speed_pct} }}\n",
            "    }}\n",
            "  }},\n",
            "  \"overrides\": {overrides},\n",
            "  \"a11y\": {{ \"reduced_motion\": {reduced_motion}, \"text_scale_pct\": {text_scale_pct} }}\n",
            "}}\n"
        ),
        schema = t.schema,
        generation = t.generation,
        writer = json_str(&t.writer),
        language = name_of(LANGS, t.language),
        preset = name_of(PRESETS, t.look.preset),
        bg = rgb_text(s.bg),
        fg = rgb_text(s.fg),
        accent = accent,
        attention = rgb_text(s.attention),
        danger = rgb_text(s.danger),
        white_level = s.white_level,
        bar_opacity = k.bar_opacity,
        surface_opacity = k.surface_opacity,
        corners = corners_text(k.corners),
        density_pct = k.density_pct,
        rim = name_of(RIMS, k.rim),
        plate = name_of(PLATES, k.plate),
        indicator = name_of(INDICATORS, k.indicator),
        family = json_str(&k.font.family),
        weight_delta = k.font.weight_delta,
        icon = k.sizes.icon,
        text = k.sizes.text,
        tray = k.sizes.tray,
        module_pct = k.sizes.module_pct,
        motion = name_of(MOTIONS, k.motion.mode),
        speed_pct = k.motion.speed_pct,
        overrides = overrides,
        reduced_motion = opt_json(t.a11y.reduced_motion),
        text_scale_pct = opt_json(t.a11y.text_scale_pct),
    )
}

/// `<root>\suite\theme.json` of the registered data root, else of the machine's data root if that root is admin-only
/// as it is. Never creates anything.
pub fn theme_path() -> Option<std::path::PathBuf> {
    match oled_sys::datadir::root() {
        Some(r) => Some(r.path.join(THEME_DIR).join(THEME_FILE)),
        None => theme_path_in(&oled_sys::datadir::lookup_root()?),
    }
}

/// `<root>\suite\theme.json` under a root nobody registered, only if the root is admin-only as it is (the check
/// `open_existing` makes before it registers one): a root any user could have created (none existed yet, or
/// `open_existing` refused it) is not read, and the reader keeps the defaults.
fn theme_path_in(root: &Path) -> Option<PathBuf> {
    oled_sys::datadir::is_admin_only_dir(root).then(|| root.join(THEME_DIR).join(THEME_FILE))
}

/// A reader of the theme file for apps. It watches `<root>\suite` only (not the root, which every dock.log write
/// signals), so it wakes when the theme writer touches the folder and at no other time.
pub struct ThemeWatch {
    file: Option<PathBuf>,
    current: ThemeFile,
    handle: Option<HANDLE>,
    last_generation: Option<u64>,
}

impl ThemeWatch {
    /// Never fails: no folder means the defaults and no handle.
    pub fn open() -> ThemeWatch {
        Self::at_file(theme_path())
    }

    /// Tests and tools: `<root>\suite\theme.json` under another root.
    pub fn open_at(root: &Path) -> ThemeWatch {
        Self::at_file(Some(root.join(THEME_DIR).join(THEME_FILE)))
    }

    fn at_file(file: Option<PathBuf>) -> ThemeWatch {
        let handle = file.as_deref().and_then(Path::parent).and_then(|dir| {
            let name = HSTRING::from(dir.as_os_str());
            // SAFETY: `name` is a valid, NUL-terminated wide string that outlives the call; the returned handle is
            // owned by this value and closed in Drop.
            unsafe {
                FindFirstChangeNotificationW(&name, false, FILE_NOTIFY_CHANGE_LAST_WRITE | FILE_NOTIFY_CHANGE_FILE_NAME)
            }
            .ok()
            .filter(|h| !h.is_invalid())
        });
        let mut w = ThemeWatch { file, current: ThemeFile::default(), handle, last_generation: None };
        if let Some(t) = w.read() {
            w.last_generation = Some(t.generation);
            w.current = t;
        }
        w
    }

    /// The file as it is now: `None` when it is missing, unreadable, too large or not a JSON object.
    fn read(&self) -> Option<ThemeFile> {
        let bytes = oled_sys::datadir::read_file(self.file.as_deref()?, THEME_MAX_BYTES, true).ok()?;
        parse(&bytes).ok().map(|(t, _notes)| t)
    }

    pub fn current(&self) -> &ThemeFile {
        &self.current
    }

    /// Signaled when `<root>\suite` changes (last write or a file name); `None` when there is no folder.
    pub fn change_handle(&self) -> Option<HANDLE> {
        self.handle
    }

    /// Re-arms the notification. The caller then arms its one 60 ms one-shot timer and calls `reload` when it fires.
    pub fn on_signaled(&mut self) {
        if let Some(h) = self.handle {
            // SAFETY: `h` is the live notification handle this value owns.
            let _ = unsafe { FindNextChangeNotification(h) };
        }
    }

    /// `Some` only for a valid file with a generation other than the one held; an invalid or missing file keeps the
    /// current theme.
    pub fn reload(&mut self) -> Option<&ThemeFile> {
        let t = self.read()?;
        if self.last_generation == Some(t.generation) {
            return None;
        }
        self.last_generation = Some(t.generation);
        self.current = t;
        Some(&self.current)
    }
}

impl Drop for ThemeWatch {
    fn drop(&mut self) {
        if let Some(h) = self.handle.take() {
            // SAFETY: `h` is the notification handle this value opened and nobody else closes.
            let _ = unsafe { FindCloseChangeNotification(h) };
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const FIXTURE: &str = include_str!("../../../tests/fixtures/theme-default.json");

    /// A small seeded generator (xorshift64*), so the random tests repeat.
    struct Rng(u64);
    impl Rng {
        fn next(&mut self) -> u64 {
            self.0 ^= self.0 >> 12;
            self.0 ^= self.0 << 25;
            self.0 ^= self.0 >> 27;
            self.0.wrapping_mul(0x2545_F491_4F6C_DD1D)
        }
        fn range(&mut self, lo: i64, hi: i64) -> i64 {
            lo + (self.next() % (hi - lo + 1) as u64) as i64
        }
        fn pick<T: Copy>(&mut self, v: &[T]) -> T {
            v[(self.next() % v.len() as u64) as usize]
        }
        fn rgb(&mut self) -> Rgb {
            Rgb(self.next() as u8, self.next() as u8, self.next() as u8)
        }
        fn text(&mut self, max: usize) -> String {
            const CHARS: &[char] = &['a', 'Z', '0', ' ', '"', '\\', '\n', '\u{e9}', '\u{4e2d}', '-', '/'];
            let n = self.range(0, max as i64) as usize;
            (0..n).map(|_| self.pick(CHARS)).collect()
        }
    }

    fn all<T: Copy>(t: &[(&'static str, T)]) -> Vec<T> {
        t.iter().map(|(_, v)| *v).collect()
    }

    fn random_theme(r: &mut Rng) -> ThemeFile {
        let mut overrides = BTreeMap::new();
        for _ in 0..r.range(0, 12) {
            const KEY: &[u8] = b"abcxyz019_.";
            let n = r.range(1, 20) as usize;
            let key: String = (0..n).map(|_| KEY[(r.next() % KEY.len() as u64) as usize] as char).collect();
            overrides.insert(key, r.next() as i64);
        }
        ThemeFile {
            schema: THEME_SCHEMA,
            generation: r.next(),
            writer: r.text(64),
            language: r.pick(&all(LANGS)),
            look: Look {
                preset: r.pick(&all(PRESETS)),
                seed: Seed {
                    bg: r.rgb(),
                    fg: r.rgb(),
                    accent: if r.next() & 1 == 0 { None } else { Some(r.rgb()) },
                    attention: r.rgb(),
                    danger: r.rgb(),
                    white_level: r.range(70, 100) as u8,
                },
                knobs: Knobs {
                    bar_opacity: r.range(0, 100) as u8,
                    surface_opacity: r.range(0, 100) as u8,
                    corners: r.pick(&[Corners::Square, Corners::Half, Corners::Normal, Corners::Round]),
                    density_pct: r.range(85, 115) as u8,
                    rim: r.pick(&all(RIMS)),
                    plate: r.pick(&all(PLATES)),
                    indicator: r.pick(&all(INDICATORS)),
                    font: FontSpec { family: r.text(64), weight_delta: r.range(-100, 100) as i8 },
                    sizes: Sizes {
                        icon: r.range(14, 32) as u8,
                        text: r.range(10, 18) as u8,
                        tray: r.range(12, 24) as u8,
                        module_pct: r.range(75, 150) as u8,
                    },
                    motion: MotionSpec { mode: r.pick(&all(MOTIONS)), speed_pct: r.range(75, 125) as u8 },
                },
            },
            overrides,
            a11y: A11y {
                reduced_motion: match r.next() % 3 {
                    0 => None,
                    1 => Some(true),
                    _ => Some(false),
                },
                text_scale_pct: if r.next() & 1 == 0 { None } else { Some(r.range(100, 225) as u16) },
            },
        }
    }

    fn in_range(t: &ThemeFile) -> bool {
        let k = &t.look.knobs;
        (70..=100).contains(&t.look.seed.white_level)
            && k.bar_opacity <= 100
            && k.surface_opacity <= 100
            && (85..=115).contains(&k.density_pct)
            && (-100..=100).contains(&k.font.weight_delta)
            && k.font.family.chars().count() <= 64
            && (14..=32).contains(&k.sizes.icon)
            && (10..=18).contains(&k.sizes.text)
            && (12..=24).contains(&k.sizes.tray)
            && (75..=150).contains(&k.sizes.module_pct)
            && (75..=125).contains(&k.motion.speed_pct)
            && t.writer.chars().count() <= 64
            && t.overrides.len() <= 64
            && t.overrides.keys().all(|k| valid_override_key(k))
            && t.a11y.text_scale_pct.is_none_or(|n| (100..=225).contains(&n))
            && t.schema == THEME_SCHEMA
    }

    fn parse_str(s: &str) -> (ThemeFile, Vec<String>) {
        parse(s.as_bytes()).expect("test input is a JSON object")
    }

    #[test]
    fn default_is_todays_oled() {
        let d = ThemeFile::default();
        assert_eq!((d.schema, d.generation, d.writer.as_str(), d.language), (1, 0, "", Lang::Auto));
        assert_eq!(d.look.preset, Preset::Oled);
        let s = &d.look.seed;
        assert_eq!((s.bg, s.fg, s.accent), (Rgb(0, 0, 0), Rgb(255, 255, 255), None));
        assert_eq!((s.attention, s.danger, s.white_level), (Rgb(255, 140, 0), Rgb(255, 69, 58), 100));
        let k = &d.look.knobs;
        assert_eq!((k.bar_opacity, k.surface_opacity, k.corners, k.density_pct), (95, 95, Corners::Normal, 100));
        assert_eq!((k.rim, k.plate, k.indicator), (RimStyle::Hairline, PlateStyle::Fill, IndicatorStyle::Line));
        assert_eq!((k.font.family.as_str(), k.font.weight_delta), ("", 0));
        assert_eq!((k.sizes.icon, k.sizes.text, k.sizes.tray, k.sizes.module_pct), (16, 13, 16, 100));
        assert_eq!((k.motion.mode, k.motion.speed_pct), (Motion::System, 100));
        assert!(d.overrides.is_empty());
        assert_eq!(d.a11y, A11y { reduced_motion: None, text_scale_pct: None });
        // and the fixture reads back as the same value, with no note
        let (t, notes) = parse_str(FIXTURE);
        assert_eq!(t, d);
        assert!(notes.is_empty());
    }

    #[test]
    fn writer_text_is_the_fixture() {
        let text = to_json(&ThemeFile::default());
        assert_eq!(text, FIXTURE);
        assert!(!text.contains('\r'));
        assert!(!text.starts_with('\u{feff}'));
        assert!(text.ends_with("}\n"));
    }

    #[test]
    fn json_round_trip() {
        let d = ThemeFile::default();
        let (back, notes) = parse_str(&to_json(&d));
        assert_eq!(back, d);
        assert!(notes.is_empty());
        let mut r = Rng(0x9E37_79B9_7F4A_7C15);
        for n in 0..500 {
            let t = random_theme(&mut r);
            let text = to_json(&t);
            let (back, notes) = parse_str(&text);
            assert_eq!(back, t, "file {n}:\n{text}");
            assert!(notes.is_empty(), "file {n}: {notes:?}");
            assert_eq!(to_json(&back), text);
        }
    }

    #[test]
    fn parse_clamps_and_reports() {
        let (t, n) = parse_str(r#"{"look":{"seed":{"white_level":50}}}"#);
        assert_eq!(t.look.seed.white_level, 70);
        assert_eq!(n.len(), 1);
        assert!(n[0].contains("look.seed.white_level"), "{n:?}");

        let (t, n) = parse_str(r#"{"look":{"knobs":{"density_pct":200}}}"#);
        assert_eq!(t.look.knobs.density_pct, 115);
        assert_eq!(n.len(), 1);

        let (t, n) = parse_str(r#"{"look":{"preset":"neon"}}"#);
        assert_eq!(t.look.preset, Preset::Custom);
        assert_eq!(n.len(), 1);

        let (t, n) = parse_str(r#"{"look":{"knobs":{"rim":"glow"}}}"#);
        assert_eq!(t.look.knobs.rim, RimStyle::Hairline);
        assert_eq!(n, vec!["look.knobs.rim: \"glow\" is not one of off, hairline, strong (default kept)".to_owned()]);

        let (t, n) = parse_str(r##"{"look":{"seed":{"bg":"#12345"}}}"##);
        assert_eq!(t.look.seed.bg, Rgb(0, 0, 0));
        assert_eq!(n.len(), 1);
        let (t, n) = parse_str(r##"{"look":{"seed":{"bg":"#aBc012","accent":"#ffffff"}}}"##);
        assert_eq!((t.look.seed.bg, t.look.seed.accent), (Rgb(0xAB, 0xC0, 0x12), Some(Rgb(255, 255, 255))));
        assert!(n.is_empty());
        // a sign is not a hex digit
        let (_, n) = parse_str(r##"{"look":{"seed":{"fg":"#+1+2+3"}}}"##);
        assert_eq!(n.len(), 1);

        // unknown fields and a missing "look" are silent
        let (t, n) = parse_str(r#"{"future":{"x":1},"look":{"extra":true}}"#);
        assert_eq!(t, ThemeFile::default());
        assert!(n.is_empty());
        let (t, n) = parse_str("{}");
        assert_eq!(t, ThemeFile::default());
        assert!(n.is_empty());

        // a newer schema is read the same way and noted once
        let (t, n) = parse_str(r#"{"schema":7,"generation":3}"#);
        assert_eq!((t.schema, t.generation, n.len()), (THEME_SCHEMA, 3, 1));

        // wrong types, sections that are not objects, long strings
        let (t, n) = parse_str(r#"{"look":5,"language":3,"writer":true,"generation":-4}"#);
        assert_eq!(
            (t.look, t.language, t.writer.as_str(), t.generation),
            (ThemeFile::default().look, Lang::Auto, "", 0)
        );
        assert_eq!(n.len(), 4, "{n:?}");
        let long = "x".repeat(65);
        let (t, n) = parse_str(&format!(r#"{{"writer":"{long}"}}"#));
        assert_eq!((t.writer.as_str(), n.len()), ("", 1));

        // overrides: bad keys and values are dropped, more than 64 are cut
        let (t, n) = parse_str(r#"{"overrides":{"a.b":3,"Bad":1,"c":"x"}}"#);
        assert_eq!(t.overrides.len(), 1);
        assert_eq!(t.overrides.get("a.b"), Some(&3));
        assert_eq!(n.len(), 2);
        let many: Vec<String> = (0..70).map(|i| format!("\"k{i:02}\":{i}")).collect();
        let (t, n) = parse_str(&format!(r#"{{"overrides":{{{}}}}}"#, many.join(",")));
        assert_eq!((t.overrides.len(), n.len()), (64, 1));

        // a11y
        let (t, n) = parse_str(r#"{"a11y":{"reduced_motion":true,"text_scale_pct":300}}"#);
        assert_eq!((t.a11y.reduced_motion, t.a11y.text_scale_pct), (Some(true), Some(225)));
        assert_eq!(n.len(), 1);

        // a BOM is skipped; the whole file is refused only when it is unusable
        let mut b = vec![0xEF, 0xBB, 0xBF];
        b.extend_from_slice(FIXTURE.as_bytes());
        assert_eq!(parse(&b).map(|x| x.0), Ok(ThemeFile::default()));
        assert!(parse(b"[1,2]").is_err());
        assert!(parse(b"{").is_err());
        assert!(parse(&[0xFF, 0xFE, b'{', b'}']).is_err());
    }

    #[test]
    fn enums_are_lowercase_strings() {
        fn check<T: Copy + PartialEq + std::fmt::Debug>(table: &'static [(&'static str, T)], expect: &[&str]) {
            let names: Vec<&str> = table.iter().map(|(n, _)| *n).collect();
            assert_eq!(names, expect);
            for (n, v) in table {
                assert_eq!(name_of(table, *v), *n);
            }
        }
        check(LANGS, &["auto", "en", "it"]);
        check(PRESETS, &["oled", "classic14", "hairline", "night", "graphite", "contrast", "custom"]);
        check(RIMS, &["off", "hairline", "strong"]);
        check(PLATES, &["fill", "rim", "underline"]);
        check(INDICATORS, &["line", "dot", "off"]);
        check(MOTIONS, &["system", "full", "reduced", "off"]);
        // both ways through the file
        let mut r = Rng(7);
        for _ in 0..50 {
            let t = random_theme(&mut r);
            let (back, _) = parse_str(&to_json(&t));
            assert_eq!((back.language, back.look.preset), (t.language, t.look.preset));
            assert_eq!((back.look.knobs.rim, back.look.knobs.plate), (t.look.knobs.rim, t.look.knobs.plate));
            assert_eq!(back.look.knobs.motion.mode, t.look.knobs.motion.mode);
        }
        // a name of another case is not a name
        let (t, n) = parse_str(r#"{"language":"IT"}"#);
        assert_eq!((t.language, n.len()), (Lang::Auto, 1));
    }

    #[test]
    fn corners_are_numbers() {
        for (text, c) in
            [("0", Corners::Square), ("0.5", Corners::Half), ("1", Corners::Normal), ("1.3", Corners::Round)]
        {
            let (t, n) = parse_str(&format!(r#"{{"look":{{"knobs":{{"corners":{text}}}}}}}"#));
            assert_eq!((t.look.knobs.corners, n.len()), (c, 0), "{text}");
            let mut d = ThemeFile::default();
            d.look.knobs.corners = c;
            assert!(to_json(&d).contains(&format!("\"corners\": {text},")), "{text}");
        }
        for bad in ["0.7", "\"1\"", "true", "null", "[1]"] {
            let (t, n) = parse_str(&format!(r#"{{"look":{{"knobs":{{"corners":{bad}}}}}}}"#));
            assert_eq!((t.look.knobs.corners, n.len()), (Corners::Normal, 1), "{bad}");
        }
    }

    fn exercise(bytes: &[u8]) {
        if let Ok((t, _)) = parse(bytes) {
            assert!(in_range(&t), "out of range from {:?}", String::from_utf8_lossy(bytes));
        }
    }

    fn fuzz(rounds: u32, seed: u64) {
        let mut r = Rng(seed);
        let base = FIXTURE.as_bytes();
        let known = to_json(&random_theme(&mut Rng(seed ^ 1)));
        for i in 0..rounds {
            let mut v: Vec<u8> = match i % 3 {
                0 => (0..r.range(0, 300)).map(|_| r.next() as u8).collect(),
                1 => base.to_vec(),
                _ => known.clone().into_bytes(),
            };
            if i % 3 != 0 {
                for _ in 0..r.range(1, 6) {
                    if v.is_empty() {
                        break;
                    }
                    let at = (r.next() % v.len() as u64) as usize;
                    match r.range(0, 4) {
                        0 => v[at] = r.next() as u8,
                        1 => v.truncate(at),
                        2 => v.insert(at, r.pick(b"{}[]\",:-.0123456789tfn#")),
                        3 => {
                            v.remove(at);
                        }
                        _ => {
                            let end = (at + 8).min(v.len());
                            let piece = v[at..end].to_vec();
                            v.splice(at..at, piece);
                        }
                    }
                }
            }
            exercise(&v);
        }
    }

    #[test]
    fn fuzz_theme_10k() {
        fuzz(10_000, 0xC0FF_EE11);
        // deep nesting, a huge number, a huge string, the size limit
        exercise("[".repeat(60_000).as_bytes());
        exercise(format!("{{\"a\":{}", "{\"a\":".repeat(5_000)).as_bytes());
        exercise(br#"{"generation":1e400,"look":{"knobs":{"bar_opacity":1e999,"density_pct":-1e999}}}"#);
        exercise(br#"{"overrides":{"a":18446744073709551615,"b":-9223372036854775808}}"#);
        assert!(parse(&vec![b' '; THEME_MAX_BYTES as usize + 1]).is_err());
        assert!(parse(&vec![b' '; THEME_MAX_BYTES as usize]).is_err()); // spaces only: not an object
        let mut at_limit = FIXTURE.as_bytes().to_vec();
        at_limit.resize(THEME_MAX_BYTES as usize, b' ');
        assert!(parse(&at_limit).is_ok());
    }

    #[test]
    #[ignore = "1,000,000 inputs: run with cargo test --release -- --ignored fuzz_"]
    fn fuzz_theme_1m() {
        fuzz(1_000_000, 0xBAD5_EED5);
    }

    // ---------------------------------------------------------------- ThemeWatch

    use windows::Win32::Foundation::WAIT_OBJECT_0;
    use windows::Win32::System::Threading::WaitForSingleObject;

    struct TempRoot(PathBuf);
    impl TempRoot {
        fn new(with_suite: bool) -> TempRoot {
            // The clock alone is not a name: two tests that start in the same tick would share one folder.
            static SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
            let (pid, seq) = (std::process::id(), SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed));
            let mut n = Rng(std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_or(1, |d| d.as_nanos() as u64)
                | 1);
            let dir = std::env::temp_dir().join(format!("oled-theme-test-{pid}-{seq}-{:016x}", n.next()));
            std::fs::create_dir_all(&dir).expect("create the temp root");
            if with_suite {
                std::fs::create_dir_all(dir.join(THEME_DIR)).expect("create the suite folder");
            }
            TempRoot(dir)
        }
        fn file(&self) -> PathBuf {
            self.0.join(THEME_DIR).join(THEME_FILE)
        }
        fn put(&self, generation: u64) {
            let t = ThemeFile { generation, ..ThemeFile::default() };
            std::fs::write(self.file(), to_json(&t)).expect("write the theme file");
        }
    }
    impl Drop for TempRoot {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn signaled(w: &ThemeWatch, ms: u32) -> bool {
        let h = w.change_handle().expect("a change handle");
        // SAFETY: the handle is the live notification handle of `w`.
        unsafe { WaitForSingleObject(h, ms) == WAIT_OBJECT_0 }
    }

    #[test]
    fn reload_sees_a_new_generation() {
        let root = TempRoot::new(true);
        let mut w = ThemeWatch::open_at(&root.0);
        assert_eq!(*w.current(), ThemeFile::default());
        assert!(w.change_handle().is_some());
        root.put(5);
        assert!(signaled(&w, 1000), "the handle was not signaled within 1 s");
        w.on_signaled();
        let t = w.reload().expect("a new generation");
        assert_eq!(t.generation, 5);
        assert_eq!(w.current().generation, 5);
        // armed again: the next write signals once more
        root.put(6);
        assert!(signaled(&w, 1000));
        w.on_signaled();
        assert_eq!(w.reload().map(|t| t.generation), Some(6));
    }

    #[test]
    fn same_generation_is_none() {
        let root = TempRoot::new(true);
        let mut w = ThemeWatch::open_at(&root.0);
        root.put(3);
        assert_eq!(w.reload().map(|t| t.generation), Some(3));
        assert!(w.reload().is_none());
        root.put(3);
        assert!(w.reload().is_none());
        // a file that was there at open counts as seen
        let mut w2 = ThemeWatch::open_at(&root.0);
        assert_eq!(w2.current().generation, 3);
        assert!(w2.reload().is_none());
    }

    #[test]
    fn an_invalid_file_keeps_the_current() {
        let root = TempRoot::new(true);
        let mut w = ThemeWatch::open_at(&root.0);
        root.put(4);
        assert_eq!(w.reload().map(|t| t.generation), Some(4));
        for bad in [&b"{ not json"[..], b"[1,2,3]", &[0xFF, 0xFE, 0x00], b""] {
            std::fs::write(root.file(), bad).expect("write");
            assert!(w.reload().is_none());
            assert_eq!(w.current().generation, 4);
        }
        std::fs::write(root.file(), vec![b' '; THEME_MAX_BYTES as usize + 1]).expect("write");
        assert!(w.reload().is_none());
        std::fs::remove_file(root.file()).expect("remove");
        assert!(w.reload().is_none());
        assert_eq!(w.current().generation, 4);
        root.put(9);
        assert_eq!(w.reload().map(|t| t.generation), Some(9));
    }

    #[test]
    fn no_folder_means_defaults_and_no_handle() {
        let root = TempRoot::new(false);
        let mut w = ThemeWatch::open_at(&root.0);
        assert_eq!(*w.current(), ThemeFile::default());
        assert!(w.change_handle().is_none());
        w.on_signaled();
        assert!(w.reload().is_none());
        // a root that does not exist at all
        let gone = root.0.join("nothing-here");
        let mut w = ThemeWatch::open_at(&gone);
        assert!(w.change_handle().is_none() && w.reload().is_none());
        assert!(!gone.exists(), "the reader never creates");
    }

    /// A root nobody registered is read only when it and its `suite` folder are admin-only, as `open_existing` and
    /// `suite_dir` require: one any user could have made (as this test's own temp folder) gives no path, so the
    /// reader keeps the defaults and watches nothing.
    #[test]
    fn an_untrusted_root_gives_no_theme_path() {
        let root = TempRoot::new(true);
        root.put(42);
        assert_eq!(ThemeWatch::open_at(&root.0).current().generation, 42, "the file itself is valid");
        assert_eq!(theme_path_in(&root.0), None, "a root this user could write is not trusted");
        assert_eq!(theme_path_in(&root.0.join("nothing-here")), None, "no root, no path");
        let w = ThemeWatch::at_file(theme_path_in(&root.0));
        assert_eq!(*w.current(), ThemeFile::default());
        assert!(w.change_handle().is_none());
    }

    #[test]
    fn the_root_does_not_wake_the_reader() {
        let root = TempRoot::new(true);
        let w = ThemeWatch::open_at(&root.0);
        std::fs::write(root.0.join("dock.log"), b"a line").expect("write");
        assert!(!signaled(&w, 300), "a write in the root woke the reader");
        root.put(1);
        assert!(signaled(&w, 1000));
    }

    #[test]
    #[ignore = "bench: cargo test --release -- --ignored bench_reload_2k --nocapture"]
    fn bench_reload_2k() {
        let root = TempRoot::new(true);
        let mut t = ThemeFile { writer: "bench".to_owned(), ..ThemeFile::default() };
        for i in 0..32 {
            t.overrides.insert(format!("k.{i:02}.padding.padding.padding"), i);
        }
        let text = to_json(&t);
        assert!((1800..2300).contains(&text.len()), "{} bytes", text.len());
        std::fs::write(root.file(), &text).expect("write");
        let mut w = ThemeWatch::open_at(&root.0);
        let start = std::time::Instant::now();
        for _ in 0..1000 {
            w.last_generation = None; // each reload reads, parses and replaces the held value
            assert!(w.reload().is_some());
        }
        let mean_ms = start.elapsed().as_secs_f64() * 1000.0 / 1000.0;
        println!("bench_reload_2k: {} bytes, mean {mean_ms:.4} ms per reload", text.len());
        assert!(mean_ms <= 0.5, "mean {mean_ms} ms");
    }
}
