//! Config fixtures of every shape from 1.0.0 to 1.4.2, and their snapshots; the template of `options!` against the
//! 1.6 writer (`legacy`), the declaration order and the descriptors' lint.
//!
//! Included from `src/config.rs` as a child module (`#[path]`), so the private `parse` and `render` are
//! reachable. Fixture inputs are never normalised (the BOM, line endings and truncation are the point);
//! only the snapshots are compared with line endings normalised. `UPDATE_SNAPSHOTS=1` rewrites them.

use super::*;
use oled_config::{ConfigError, Kept, Problem};
use std::path::{Path, PathBuf};

fn dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/config")
}

fn name(f: &Path) -> String {
    f.file_name().unwrap().to_string_lossy().into_owned()
}

/// Every fixture input (`*.json` except `*.snap.json`), sorted by name.
fn fixtures() -> impl Iterator<Item = PathBuf> {
    let mut v: Vec<PathBuf> = std::fs::read_dir(dir())
        .unwrap()
        .map(|e| e.unwrap().path())
        .filter(|p| name(p).ends_with(".json") && !name(p).ends_with(".snap.json"))
        .collect();
    v.sort();
    v.into_iter()
}

/// The file as text, a BOM kept (`parse` strips it itself).
fn read_utf8_keep_bom(f: &Path) -> String {
    String::from_utf8(std::fs::read(f).unwrap()).unwrap_or_else(|e| panic!("{f:?} is not UTF-8: {e}"))
}

fn read(file: &str) -> String {
    read_utf8_keep_bom(&dir().join(file))
}

fn norm(s: &str) -> String {
    s.replace("\r\n", "\n").trim_end().to_string()
}

fn snapshot_of(text: &str) -> String {
    match parse(text) {
        Ok(c) => serde_json::to_string_pretty(&c).unwrap(),
        Err(e) => format!("{{\"error\": {e:?}}}"),
    }
}

#[test]
fn every_fixture_parses_to_its_snapshot() {
    let update = std::env::var_os("UPDATE_SNAPSHOTS").is_some();
    let mut n = 0;
    for f in fixtures() {
        let got = snapshot_of(&read_utf8_keep_bom(&f));
        let snap = f.with_extension("snap.json");
        if update {
            std::fs::write(&snap, &got).unwrap();
        } else {
            let want = std::fs::read_to_string(&snap).expect("missing snapshot: run with UPDATE_SNAPSHOTS=1");
            assert_eq!(norm(&got), norm(&want), "{f:?}");
        }
        n += 1;
    }
    assert!(n >= 26, "fixtures went missing: {n}"); // 16 tag shapes + 9 edge shapes + the owner's file
}

#[test]
fn fixtures_that_parse_roundtrip() {
    // parse(render(c)) == c and render is idempotent, for every fixture that parses
    for f in fixtures() {
        if let Ok(c) = parse(&read_utf8_keep_bom(&f)) {
            let r = render(&c);
            assert_eq!(parse(&r).unwrap(), c, "{f:?}");
            assert_eq!(render(&parse(&r).unwrap()), r, "{f:?}");
        }
    }
}

#[test]
fn every_release_fixture_is_accepted() {
    for f in fixtures().filter(|f| name(f).starts_with("v1.")) {
        assert!(parse(&read_utf8_keep_bom(&f)).is_ok(), "{f:?}");
    }
}

#[test]
fn the_bom_fixture_keeps_its_bytes() {
    let b = std::fs::read(dir().join("bom.json")).unwrap();
    assert_eq!(&b[..3], &[0xEF, 0xBB, 0xBF], "a checkout rewrote bom.json: check .gitattributes");
}

#[test]
fn the_truncated_fixture_is_still_truncated() {
    let b = std::fs::read(dir().join("invalid-truncated.json")).unwrap();
    assert!(!b.trim_ascii_end().ends_with(b"}"), "a checkout or editor repaired invalid-truncated.json");
}

#[test]
fn the_1_4_2_default_file_is_todays_default() {
    // Its old default list was migrated on the way in: it is today's list.
    assert_eq!(parse(&read("v1.4.2-default.json")).unwrap(), Config::default());
}

#[test]
fn owner_fixture_sets_alt_space() {
    let c = parse(&read("owner-1.5.0.json")).unwrap();
    assert_eq!(c.launcher.chord, "alt+space");
    assert!(c.launcher.chord_spec().is_some());
    // the rest of the file is the 1.4.2 owner file
    let mut old = parse(&read("owner-1.4.2.json")).unwrap();
    old.launcher.chord = "alt+space".into();
    assert_eq!(c, old);
}

#[test]
fn launcher_chord_off_by_default() {
    // the default and every file written by 1.4.3 (its config is the 1.4.2 shape) leave the chord unset
    assert_eq!(Config::default().launcher.chord_spec(), None);
    for f in ["v1.4.2-default.json", "v1.4.2-custom.json", "owner-1.4.2.json"] {
        let c = parse(&read(f)).unwrap();
        assert_eq!(c.launcher.chord, "off", "{f}");
        assert_eq!(c.launcher.chord_spec(), None, "{f}");
    }
}

#[test]
fn unknown_keys_survive_a_save() {
    // Keys a newer release wrote are kept at this release's saves, so going back to it and forward again
    // keeps that release's settings.
    let first = parse(&read("unknown-keys.json")).unwrap();
    let want: serde_json::Map<String, serde_json::Value> =
        serde_json::from_str(r#"{"a_key_from_the_future": {"nested": [1, 2, 3]}, "another_future_key": true}"#)
            .unwrap();
    assert_eq!(first.extra, want);
    let r = render(&first);
    assert_eq!(r.matches("\"a_key_from_the_future\":").count(), 1, "{r}");
    let again = parse(&r).unwrap();
    assert_eq!(again.extra, want);
    assert_eq!(again, first); // every known field too
    assert_eq!(render(&again), r);
}

#[test]
fn known_and_legacy_keys_are_never_extra() {
    assert!(parse(&read("v1.4.2-custom.json")).unwrap().extra.is_empty());
    assert!(parse("{\"bar_size\":\"large\"}").unwrap().extra.is_empty());
    assert!(Config::default().extra.is_empty());
    // No unknown key: not one byte of the file changes.
    assert!(render(&Config::default()).ends_with("  \"notif_prompted\": false\n}\n"));
}

#[test]
fn rendered_keys_are_known_and_never_extra() {
    // The known keys are every key serde writes for a `Config` (`known_keys_match_serde` pins their order).
    let serde_keys = match serde_json::to_value(Config::default()).unwrap() {
        serde_json::Value::Object(m) => m.keys().cloned().collect::<Vec<_>>(),
        other => panic!("{other}"),
    };
    let mut want: Vec<&str> = serde_keys.iter().map(String::as_str).collect();
    let mut have = Config::known_keys().to_vec();
    want.sort_unstable();
    have.sort_unstable();
    assert_eq!(have, want);
    // A key `render` writes that a later parse took for an unknown one (a future top-level
    // `skip_serializing_if`, say) would be kept as extra and written twice.
    let r = oled_config::jsonc::strip_comments(&render(&Config::default()));
    let written: serde_json::Map<String, serde_json::Value> = serde_json::from_str(&r).unwrap();
    assert!(parse(&r).unwrap().extra.is_empty());
    for f in fixtures() {
        if let Ok(c) = parse(&read_utf8_keep_bom(&f)) {
            for k in c.extra.keys() {
                assert!(!written.contains_key(k) && k != "bar_size", "{f:?}: {k}");
            }
        }
    }
}

// ------------------------------------------------------------------ W5c: one declaration, the same file

/// The fields of `Config` in the order 1.6 declared them: serde writes them in this order, so every snapshot does.
const DECLARED: [&str; 56] = [
    "language",
    "position",
    "monitor",
    "autohide",
    "hide_delay_ms",
    "show_delay_ms",
    "reveal",
    "push_threshold",
    "keyboard",
    "tray_privacy_migrated",
    "window_previews",
    "win_opens_start",
    "hide_native_taskbar",
    "pinned",
    "start_pins",
    "start_folders",
    "start_style",
    "start_grid",
    "names",
    "show_date",
    "show_volume_percent",
    "show_tray",
    "tray_show_all",
    "show_notifications",
    "show_volume",
    "show_network",
    "show_metrics",
    "show_battery",
    "battery_percent",
    "battery_percent_inside",
    "show_clock",
    "show_desktop_button",
    "show_start_button",
    "display_mode",
    "app_icon_size",
    "app_text_size",
    "tray_icon_size",
    "module_size",
    "background",
    "bar_opacity",
    "settings_window_size",
    "time_format",
    "show_seconds",
    "date_format",
    "tray_hidden_guids",
    "tray_hidden",
    "app_order",
    "tray_order",
    "modules_order",
    "audio_layout",
    "notif_prompted",
    "update_check",
    "update_auto_install",
    "last_update_check",
    "update_available",
    "launcher",
];

/// The top-level keys of a pretty-printed JSON object, in their order.
fn top_keys(pretty: &str) -> Vec<String> {
    pretty
        .lines()
        .filter(|l| l.starts_with("  \""))
        .filter_map(|l| {
            let rest = &l[2..];
            let end = rest.find("\":")?;
            serde_json::from_str::<String>(&rest[..=end]).ok()
        })
        .collect()
}

#[test]
fn declaration_order_is_unchanged() {
    let pretty = serde_json::to_string_pretty(&Config::default()).unwrap();
    assert_eq!(top_keys(&pretty), DECLARED);
}

#[test]
fn known_keys_match_serde() {
    // The keys the reader knows are the keys serde writes, in the same order.
    assert_eq!(Config::known_keys(), DECLARED);
}

#[test]
fn public_functions_keep_their_signatures() {
    // Named, never called: the live functions read and write the machine's data folder.
    let _: fn() -> Arc<Config> = get;
    let _: fn(Config) = set;
    let _: fn() -> Arc<Config> = init;
    let _: fn() -> bool = poll_changed;
    type Edit = fn(&mut Config);
    let _: fn(Edit) -> Result<(), String> = modify::<Edit>;
    let _: fn(&Config) -> Result<(), String> = save;
    let _: fn(windows::Win32::Foundation::HWND) -> bool = start_watch;
    let _: fn() -> bool = stop_watch;
    let _: fn() -> bool = watching;
    let _: fn() -> bool = external_edit_pending;
    let _: fn() -> LoadNotes = load_notes;
    let _: fn() -> Arc<[ConfigError]> = errors;
}

#[test]
fn descriptors_lint_with_todays_exceptions() {
    // Today's exceptions are checked in: a new finding, or one fixed, changes the list in the commit that causes it.
    let it = |f: oled_config::TextFn| crate::i18n::with_italian(true, f);
    let got = oled_config::lint(Config::descriptors(), &it);
    let allowed: Vec<&str> = include_str!("descriptor-lint-allow.txt").lines().collect();
    assert_eq!(got, allowed);
}

/// xorshift64: the random configs, seeded, so a failure repeats.
struct Rng(u64);

impl Rng {
    fn step(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        self.0 = x;
        x
    }

    fn below(&mut self, n: u64) -> u64 {
        self.step() % n
    }

    fn flip(&mut self) -> bool {
        self.step() & 1 == 1
    }

    fn pick<'a>(&mut self, from: &[&'a str]) -> &'a str {
        from[self.below(from.len() as u64) as usize]
    }

    /// Up to 6 pieces: quotes, backslashes, comment marks, non-ASCII, control characters.
    fn text(&mut self) -> String {
        const PIECES: [&str; 24] = [
            "a",
            "Z",
            "0",
            " ",
            "\"",
            "\\",
            "/",
            "//",
            "/*",
            "*/",
            "\u{fc}",
            "\u{65e5}\u{672c}",
            "\u{2192}",
            "\u{1f600}",
            "{",
            "}",
            "[",
            ",",
            ":",
            "\n",
            "\t",
            "\u{0}",
            "\u{7f}",
            "'",
        ];
        (0..self.below(7)).map(|_| self.pick(&PIECES)).collect()
    }

    fn list(&mut self) -> Vec<String> {
        (0..self.below(6)).map(|_| self.text()).collect()
    }

    fn texts(&mut self, at_most: u64) -> BTreeMap<String, String> {
        (0..self.below(at_most + 1)).map(|_| (self.text(), self.text())).collect()
    }
}

/// Every field at a random value in its range (the opacity above 100 too: the file writes at most 100).
fn random_config(r: &mut Rng) -> Config {
    Config {
        language: [Language::Auto, Language::En, Language::It][r.below(3) as usize],
        position: if r.flip() { Position::Top } else { Position::Bottom },
        monitor: r.text(),
        autohide: r.flip(),
        hide_delay_ms: r.below(5001) as u32,
        show_delay_ms: r.below(1001) as u32,
        reveal: if r.flip() { Reveal::Push } else { Reveal::Dwell },
        push_threshold: 20 + r.below(981) as u32,
        keyboard: KeyboardConfig { win_numbers: r.flip(), win_t: r.flip(), win_b: r.flip(), peek: r.flip() },
        tray_privacy_migrated: r.flip(),
        window_previews: r.flip(),
        win_opens_start: r.flip(),
        hide_native_taskbar: r.flip(),
        pinned: (0..r.below(6))
            .map(|_| PinnedApp { key: r.text(), name: r.text(), launch: r.text(), icon: r.flip().then(|| r.text()) })
            .collect(),
        start_pins: r.list(),
        start_folders: (0..r.below(4)).map(|_| StartFolder { name: r.text(), apps: r.list() }).collect(),
        start_style: if r.flip() { StartStyle::Windows11 } else { StartStyle::Compact },
        start_grid: r.flip(),
        names: r.texts(3),
        show_date: r.flip(),
        show_volume_percent: r.flip(),
        show_tray: r.flip(),
        tray_show_all: r.flip(),
        show_notifications: r.flip(),
        show_volume: r.flip(),
        show_network: r.flip(),
        show_metrics: r.flip(),
        show_battery: [BatteryShow::Auto, BatteryShow::On, BatteryShow::Off][r.below(3) as usize],
        battery_percent: r.flip(),
        battery_percent_inside: r.flip(),
        show_clock: r.flip(),
        show_desktop_button: r.flip(),
        show_start_button: r.flip(),
        display_mode: [DisplayMode::IconText, DisplayMode::Icon, DisplayMode::Text][r.below(3) as usize],
        app_icon_size: (14 + r.below(19)) as u8,
        app_text_size: (10 + r.below(9)) as u8,
        tray_icon_size: (12 + r.below(13)) as u8,
        module_size: (75 + r.below(76)) as u8,
        background: Background((r.step() & 0xFF_FFFF) as u32),
        bar_opacity: r.below(121) as u8,
        settings_window_size: [r.below(4000) as u16, r.below(3000) as u16],
        time_format: r.text(),
        show_seconds: r.flip(),
        date_format: r.text(),
        tray_hidden_guids: r.list(),
        tray_hidden: r.list(),
        app_order: r.list(),
        tray_order: r.list(),
        modules_order: r.list(),
        audio_layout: AudioLayout::parse(r.pick(&["MOSX", "M-oXS", "xmOS", "soMX", "-m-o-s-x", ""])),
        notif_prompted: r.flip(),
        update_check: r.flip(),
        update_auto_install: r.flip(),
        last_update_check: r.text(),
        update_available: r.text(),
        launcher: LauncherConfig {
            enabled: r.flip(),
            chord: r.pick(&["off", "alt+space", "alt+shift+space"]).into(),
            with_powertoys: r.pick(&["yield", "take"]).into(),
            monitor: r.pick(&["pointer", "active", "primary"]).into(),
            top_percent: (10 + r.below(41)) as u8,
            width: (560 + 40 * r.below(9)) as u16,
            rows: (4 + r.below(9)) as u8,
            animate: r.flip(),
            keep_query: r.flip(),
            over_fullscreen: r.pick(&["pass", "open"]).into(),
            learn: r.flip(),
            announce_selection: r.flip(),
            providers: LAUNCHER_PROVIDERS.iter().map(|p| (p.to_string(), r.flip())).collect(),
            prefixes: r.texts(3),
            aliases: r.texts(3),
            file_folders: r.list(),
            web_engine: r.pick(&["google", "bing", "duckduckgo", "custom"]).into(),
            web_url: r.text(),
            offer_done: r.flip(),
            powertoys_done: r.flip(),
        },
        extra: (0..r.below(3))
            .map(|i| {
                let v = match r.below(3) {
                    0 => serde_json::json!(true),
                    1 => serde_json::json!({"k": [1, "x"]}),
                    _ => serde_json::json!(r.text()),
                };
                (format!("future_{i}"), v)
            })
            .collect(),
    }
}

#[test]
fn template_is_byte_identical() {
    // options! writes what the 1.6 writer wrote: the defaults, every fixture that parses, and 1,000 random configs.
    let check = |c: &Config, what: &str| assert_eq!(c.to_template(), legacy::render(c), "{what}");
    check(&Config::default(), "default");
    check(&fresh_config(), "fresh install");
    for f in fixtures() {
        if let Ok(c) = parse(&read_utf8_keep_bom(&f)) {
            check(&c, &name(&f));
        }
    }
    let mut r = Rng(0x2545_f491_4f6c_dd1d);
    for i in 0..1_000 {
        check(&random_config(&mut r), &format!("random config {i}"));
    }
}

// ------------------------------------------------------------------ W5d: one bad value keeps the rest

#[test]
fn a_bad_type_loses_one_field_only() {
    let text = "{\n  \"bar_opacity\": \"x\",\n  \"autohide\": false\n}";
    let (c, errors) = parse_with_errors(text, None).unwrap();
    assert!(!c.autohide);
    assert_eq!(c.bar_opacity, DEFAULT_BAR_OPACITY);
    let want = ConfigError {
        path: "bar_opacity".into(),
        line: 2,
        col: 3,
        problem: Problem::Type { expected: "a number" },
        kept: Kept::Default,
    };
    assert_eq!(errors, [want]);
    // A key no descriptor describes says what serde wanted.
    let (c, errors) = parse_with_errors("{\"names\": [1], \"show_date\": false}", None).unwrap();
    assert!(!c.show_date && c.names.is_empty());
    assert_eq!(errors[0].problem, Problem::Type { expected: "an object" });
}

#[test]
fn a_wrong_value_inside_a_section_says_what_was_expected_there() {
    // The section is an object, as its descriptor says: the error names what the value inside it should have been.
    let (_, errors) = parse_with_errors("{\"launcher\": {\"rows\": \"x\"}}", None).unwrap();
    let got: Vec<(&str, &Problem)> = errors.iter().map(|e| (e.path.as_str(), &e.problem)).collect();
    assert_eq!(got, [("launcher", &Problem::Type { expected: "a number" })]);
    let (_, errors) = parse_with_errors("{\"keyboard\": {\"win_t\": \"x\"}}", None).unwrap();
    assert_eq!(errors[0].problem, Problem::Type { expected: "a boolean" });
    // A section that is not an object says so.
    let (_, errors) = parse_with_errors("{\"keyboard\": 3}", None).unwrap();
    assert_eq!(errors[0].problem, Problem::Type { expected: "an object" });
}

#[test]
fn out_of_range_keeps_the_last_good_value() {
    let good = parse("{\"app_icon_size\": 20}").unwrap();
    let (c, errors) = parse_with_errors("{\n  \"app_icon_size\": 40\n}", Some(&good)).unwrap();
    assert_eq!(c.app_icon_size, 20);
    let want = ConfigError {
        path: "app_icon_size".into(),
        line: 2,
        col: 3,
        problem: Problem::Range { min: 14, max: 32, got: 40 },
        kept: Kept::LastGood,
    };
    assert_eq!(errors, [want]);
    assert_eq!(errors[0].log_line(), "line 2, app_icon_size: 40 is above the maximum 32 (kept the last good value)");
    // A wrong type keeps the last good value too.
    let (c, errors) = parse_with_errors("{\"app_icon_size\": true}", Some(&good)).unwrap();
    assert_eq!((c.app_icon_size, errors[0].kept), (20, Kept::LastGood));
}

#[test]
fn at_first_load_the_default_is_kept() {
    let (c, errors) = parse_with_errors("{\n  \"app_icon_size\": 40,\n  \"bar_opacity\": 101\n}", None).unwrap();
    assert_eq!((c.app_icon_size, c.bar_opacity), (APP_ICON_SIZE.default, DEFAULT_BAR_OPACITY));
    let got: Vec<(&str, u32, Kept)> = errors.iter().map(|e| (e.path.as_str(), e.line, e.kept)).collect();
    assert_eq!(got, [("app_icon_size", 2, Kept::Default), ("bar_opacity", 3, Kept::Default)]);
}

#[test]
fn launcher_sanitize_messages_become_errors() {
    // Each fix the section's sanitizer makes is one error under "launcher.<key>", at its line, of the problem it is;
    // the fixes stay, and a number out of its range takes its default, as the error says.
    let check = |text: &str, want: &[(&str, u32, u32, Problem)]| {
        let v: serde_json::Value = serde_json::from_str(text).unwrap();
        let mut fixed: LauncherConfig = serde_json::from_value(v["launcher"].clone()).unwrap();
        let messages = fixed.sanitize();
        let (c, errors) = parse_with_errors(text, None).unwrap();
        let paths: Vec<&str> = errors.iter().map(|e| e.path.as_str()).collect();
        let said: Vec<&str> = messages.iter().filter_map(|m| m.split_once(": ").map(|(p, _)| p)).collect();
        assert_eq!(paths, said, "one error per message");
        let got: Vec<(&str, u32, u32, Problem)> =
            errors.iter().map(|e| (e.path.as_str(), e.line, e.col, e.problem.clone())).collect();
        assert_eq!(got, want);
        assert!(errors.iter().all(|e| e.kept == Kept::Default));
        // Every number of these texts is out of its range.
        (fixed.top_percent, fixed.rows, fixed.width) = (LAUNCHER_TOP_PERCENT.2, LAUNCHER_ROWS.2, LAUNCHER_WIDTH.2);
        assert_eq!(c.launcher, fixed);
        errors
    };
    let range = |min: i64, max: i64, got: i64| Problem::Range { min, max, got };
    let long = "x".repeat(ALIAS_TEXT_CAP + 1);
    let every = format!(
        r#"{{
  "launcher": {{
    "chord": "ctrl+x",
    "with_powertoys": "maybe",
    "monitor": "left",
    "over_fullscreen": "x",
    "top_percent": 99,
    "rows": 99,
    "width": 900,
    "providers": {{ "apps": false, "bogus": true }},
    "prefixes": {{ "toolong ": "apps" }},
    "aliases": {{ "w": "{long}" }},
    "web_engine": "custom",
    "web_url": "ftp://x"
  }}
}}"#
    );
    let errors = check(
        &every,
        &[
            ("launcher.chord", 3, 5, Problem::UnknownValue { allowed: &["off", "alt+space", "alt+shift+space"] }),
            ("launcher.with_powertoys", 4, 5, Problem::UnknownValue { allowed: &["yield", "take"] }),
            ("launcher.monitor", 5, 5, Problem::UnknownValue { allowed: &["pointer", "active", "primary"] }),
            ("launcher.over_fullscreen", 6, 5, Problem::UnknownValue { allowed: &["pass", "open"] }),
            ("launcher.top_percent", 7, 5, range(10, 50, 99)),
            ("launcher.rows", 8, 5, range(4, 12, 99)),
            ("launcher.width", 9, 5, range(560, 880, 900)),
            ("launcher.providers", 10, 5, Problem::UnknownValue { allowed: &LAUNCHER_PROVIDERS }),
            ("launcher.providers.apps", 10, 20, Problem::Type { expected: "true (apps are always on)" }),
            (
                "launcher.prefixes",
                11,
                5,
                Problem::Type {
                    expected:
                        "prefixes of 1 to 5 characters (a word ends with a space), each once, for a known provider",
                },
            ),
            ("launcher.aliases", 12, 5, Problem::TooLong { max: ALIAS_TEXT_CAP }),
            ("launcher.web_url", 14, 5, Problem::Type { expected: "an https:// address with {q}" }),
        ],
    );
    assert_eq!(errors[5].log_line(), "line 8, launcher.rows: 99 is above the maximum 12 (kept the default)");
    // The other side of each range, a width between two steps, too many aliases and an unknown engine.
    let aliases: Vec<String> = (0..ALIAS_ENTRIES_CAP + 44).map(|i| format!("\"a{i:03}\": \"calc\"")).collect();
    let others = format!(
        r#"{{
  "launcher": {{
    "top_percent": 5,
    "rows": 2,
    "width": 570,
    "aliases": {{ {} }},
    "web_engine": "yahoo"
  }}
}}"#,
        aliases.join(", ")
    );
    check(
        &others,
        &[
            ("launcher.top_percent", 3, 5, range(10, 50, 5)),
            ("launcher.rows", 4, 5, range(4, 12, 2)),
            ("launcher.width", 5, 5, Problem::Type { expected: "560 to 880 in steps of 40" }),
            ("launcher.aliases", 6, 5, range(0, 256, 300)),
            (
                "launcher.web_engine",
                7,
                5,
                Problem::UnknownValue { allowed: &["google", "bing", "duckduckgo", "custom"] },
            ),
        ],
    );
    // A message of a later sanitizer still becomes an error of its key.
    let later = launcher_error("launcher.later: a new fix", &LauncherConfig::default());
    assert_eq!(
        (later.path.as_str(), later.problem),
        ("launcher.later", Problem::Type { expected: "a value of its list" })
    );
    // The defaults need no fix.
    assert!(parse_with_errors("{}", None).unwrap().1.is_empty());
}
/// A folder of its own under the temp folder, removed at the end.
struct Scratch(PathBuf);

impl Scratch {
    fn new(tag: &str) -> Scratch {
        static N: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
        let n = N.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        let p = std::env::temp_dir().join(format!("dock_config_{tag}_{}_{n}", std::process::id()));
        std::fs::create_dir_all(&p).unwrap();
        Scratch(p)
    }

    fn file(&self) -> PathBuf {
        self.0.join("config.json")
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// A store over `dir`'s config.json that touches nothing of the dock (no language, theme or entry points).
fn store_in(dir: &Scratch) -> oled_config::Store<Config> {
    let spec = oled_config::FileSpec {
        path: dir.file(),
        kind: oled_config::FileKind::PerUser,
        cap: crate::datadir::CONFIG_MAX_BYTES,
    };
    oled_config::Store::new(Some(spec), |_| {})
}

/// A hand edit with a modification time of its own (two writes within one clock tick can share one).
fn edit(p: &Path, text: &str) {
    static STEP: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);
    std::fs::write(p, text).unwrap();
    let step = STEP.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
    let t = std::time::SystemTime::now() + std::time::Duration::from_secs(3_600 + step);
    std::fs::File::options().write(true).open(p).unwrap().set_modified(t).unwrap();
}

#[test]
fn a_save_while_the_file_has_errors_backs_it_up_first() {
    let dir = Scratch::new("backup");
    let text = format!("{TEMPLATE_MARK}\n{{\n  \"app_icon_size\": 99,\n  \"show_seconds\": false\n}}\n");
    std::fs::write(dir.file(), &text).unwrap();
    let s = store_in(&dir);
    assert_eq!(s.load(|| None), oled_config::LoadOutcome::Read);
    assert_eq!(s.errors().len(), 1);
    assert_eq!(std::fs::read_to_string(dir.file()).unwrap(), text, "a load never rewrites a file with errors");
    s.modify(|c| c.show_seconds = true).unwrap();
    assert_eq!(std::fs::read_to_string(dir.0.join("config.json.bak")).unwrap(), text);
    let now = s.get();
    assert!(now.show_seconds && now.app_icon_size == APP_ICON_SIZE.default);
    assert_eq!(std::fs::read_to_string(dir.file()).unwrap(), now.to_template());
    assert!(s.errors().is_empty());
}

#[test]
fn a_save_while_the_launcher_has_fixes_backs_it_up_first() {
    // The launcher's fixes are not errors of the store (its reader makes them): the dock copies the file itself.
    let dir = Scratch::new("launcher_backup");
    let text = format!(
        "{TEMPLATE_MARK}\n{{\n  \"tray_privacy_migrated\": true,\n  \"launcher\": {{ \"chord\": \"x\" }}\n}}\n"
    );
    std::fs::write(dir.file(), &text).unwrap();
    let s = store_in(&dir);
    assert_eq!(s.load(|| None), oled_config::LoadOutcome::Read);
    assert!(s.errors().is_empty(), "no error of the store's own");
    back_up_launcher_fixes(Some(&dir.file())).unwrap();
    s.modify(|c| c.show_seconds = true).unwrap();
    let bak = dir.0.join("config.json.bak");
    assert_eq!(std::fs::read_to_string(&bak).unwrap(), text);
    assert_eq!(s.get().launcher.chord, "off");
    // A file the launcher's reader takes as it is, or none, is not copied.
    std::fs::remove_file(&bak).unwrap();
    back_up_launcher_fixes(Some(&dir.file())).unwrap();
    back_up_launcher_fixes(Some(&dir.0.join("missing.json"))).unwrap();
    back_up_launcher_fixes(None).unwrap();
    assert!(!bak.exists());
}

#[test]
fn a_file_without_the_privacy_flag_is_rewritten_once() {
    // The flag is set by the read (`migrate_v0`), so the start rewrites the file even when the list stays the user's;
    // the file then carries the flag and the next start leaves it alone.
    let dir = Scratch::new("privacy_flag");
    let own = Config { tray_hidden_guids: vec!["00000000-0000-0000-0000-000000000000".into()], ..Config::default() };
    std::fs::write(dir.file(), render(&Config { tray_privacy_migrated: false, ..own.clone() })).unwrap();
    let s = store_in(&dir);
    assert_eq!(s.load(|| None), oled_config::LoadOutcome::Rewritten);
    assert_eq!(std::fs::read_to_string(dir.file()).unwrap(), render(&own));
    assert_eq!(store_in(&dir).load(|| None), oled_config::LoadOutcome::Read);
}

#[test]
fn errors_clear_after_a_good_edit() {
    let dir = Scratch::new("clear");
    std::fs::write(dir.file(), format!("{TEMPLATE_MARK}\n{{ \"bar_opacity\": 300, \"autohide\": false }}")).unwrap();
    let s = store_in(&dir);
    s.load(|| None);
    assert_eq!((s.get().bar_opacity, s.get().autohide, s.errors().len()), (DEFAULT_BAR_OPACITY, false, 1));
    edit(&dir.file(), &format!("{TEMPLATE_MARK}\n{{ \"bar_opacity\": 30, \"autohide\": false }}"));
    assert_eq!(s.reload_if_changed(), oled_config::Reload::Changed);
    assert!(s.errors().is_empty());
    assert_eq!(s.get().bar_opacity, 30);
    // A bad edit after a good one: the last good value stands in.
    edit(&dir.file(), &format!("{TEMPLATE_MARK}\n{{ \"bar_opacity\": -1, \"autohide\": true }}"));
    assert_eq!(s.reload_if_changed(), oled_config::Reload::Changed);
    assert_eq!((s.get().bar_opacity, s.get().autohide), (30, true));
    assert_eq!(s.errors()[0].kept, Kept::LastGood);
}

/// Config-like texts: the real keys with values of every type and range, comments and broken pieces, read at a first
/// load and over a last good config. The read is total, its errors are all of single keys, and what it gives is
/// written and read back unchanged.
fn fuzz_config(runs: u32) {
    const VALUES: [&str; 22] = [
        "true",
        "false",
        "0",
        "1",
        "-5",
        "16",
        "300",
        "99999999999999999999",
        "1.5",
        "\"\"",
        "\"x\"",
        "\"#fff\"",
        "\"auto\"",
        "\"push\"",
        "[]",
        "[1, \"a\"]",
        "{}",
        "{\"chord\": 5}",
        "{\"rows\": 99, \"x\": [1]}",
        "null",
        "\"MOSX\"",
        "[\"a.exe|0\"]",
    ];
    const JUNK: [&str; 8] =
        ["", "// c\n", "/* b */", ",", "\u{feff}", "\"bar_size\": \"large\",", "\"version\": 0,", "\n"];
    let mut r = Rng(0x9e37_79b9_7f4a_7c15);
    let last = random_config(&mut r);
    for _ in 0..runs {
        let mut text = String::from("{");
        let n = r.below(8);
        for i in 0..n {
            let key = if r.below(10) == 0 { "future_key" } else { r.pick(&DECLARED) };
            text.push_str(&format!("{}\"{key}\": {}", r.pick(&JUNK), r.pick(&VALUES)));
            if i + 1 < n {
                text.push(',');
            }
        }
        text.push('}');
        let over = if r.flip() { Some(&last) } else { None };
        if let Ok((c, errors)) = parse_with_errors(&text, over) {
            assert!(errors.iter().all(|e| !e.path.is_empty()), "{text}: {errors:?}");
            assert_eq!(parse(&c.to_template()).as_ref(), Ok(&c), "{text}");
        }
    }
}

#[test]
fn fuzz_config_parse_10k() {
    fuzz_config(10_000);
}

#[test]
#[ignore = "nightly fuzz: 1,000,000 inputs"]
fn fuzz_config_parse_1m() {
    fuzz_config(1_000_000);
}

/// L-W5 (a), a read-only live check on a machine with the dock installed:
/// `OLED_CONFIG_FILE=<the data folder>\config.json cargo test --bin dock owner_file_is_byte_identical -- --ignored`.
/// The template of that file equals the 1.6 writer's, and both equal the file's bytes when it carries the mark (a file
/// the dock wrote). Without the variable it checks nothing.
#[test]
#[ignore = "a live check: reads the file OLED_CONFIG_FILE names"]
fn owner_file_is_byte_identical() {
    let Some(path) = std::env::var_os("OLED_CONFIG_FILE") else {
        println!("owner_file_is_byte_identical: OLED_CONFIG_FILE is not set, nothing checked");
        return;
    };
    let text = read_utf8_keep_bom(Path::new(&path));
    let c = parse(&text).unwrap();
    let written = c.to_template();
    assert_eq!(written, legacy::render(&c));
    if text.contains(TEMPLATE_MARK) {
        assert_eq!(written, text, "the file the dock wrote is the template of what it reads");
    }
}

/// Mean time of one parse of the owner's 1.5.0 file, against the 1.6 read of the same text (budget: 1.1 x).
/// `cargo test --release --bin dock bench_parse -- --ignored --nocapture`
#[test]
#[ignore = "a bench: run it on a release build"]
fn bench_parse() {
    let text = read("owner-1.5.0.json");
    let mean_us = |f: &dyn Fn(&str) -> bool| {
        let start = std::time::Instant::now();
        for _ in 0..1_000 {
            assert!(f(&text));
        }
        start.elapsed().as_secs_f64() * 1e6 / 1_000.0
    };
    let old = mean_us(&|t| legacy::parse(t).is_ok());
    let new = mean_us(&|t| parse(t).is_ok());
    println!("bench_parse: {new:.1} us per parse, the 1.6 read {old:.1} us ({:.2} x, budget 1.1 x)", new / old);
}

/// The 1.6 writer, moved here verbatim: the template must stay byte for byte what it wrote.
mod legacy {
    use crate::config::*;
    use serde::Serialize;

    /// The 1.6 read, for `bench_parse`: the struct and the value parsed from the text one after the other, then the
    /// fixes (this release's readers stand in for 1.6's clamp and sanitize passes).
    pub(super) fn parse(text: &str) -> Result<Config, String> {
        let t = oled_config::jsonc::strip_comments(oled_config::jsonc::strip_bom(text));
        let mut c = serde_json::from_str::<Config>(&t).map_err(|e| e.to_string())?;
        let mut v: serde_json::Value = serde_json::from_str(&t).map_err(|e| e.to_string())?;
        migrate_v0(&mut v).map_err(|e| e.0)?;
        if let serde_json::Value::Object(mut all) = v {
            for k in Config::known_keys() {
                all.remove(*k);
            }
            c.extra = all;
        }
        let _ = migrate_privacy(&mut c);
        Ok(c)
    }

    fn js<T: Serialize + ?Sized>(v: &T) -> String {
        serde_json::to_string(v).unwrap_or_else(|_| "null".to_string())
    }

    /// Pretty JSON of a collection, continuation lines indented to sit under a 2-space key.
    fn js_block<T: Serialize + ?Sized>(v: &T) -> String {
        serde_json::to_string_pretty(v).unwrap_or_else(|_| "null".to_string()).replace('\n', "\n  ")
    }

    /// The `extra` keys as the last lines of the file, one compact line each: "" when there are none, so
    /// such a file is written as before.
    fn render_extra(extra: &serde_json::Map<String, serde_json::Value>) -> String {
        if extra.is_empty() {
            return String::new();
        }
        let lines: Vec<String> = extra.iter().map(|(k, v)| format!("  {}: {}", js(k), js(v))).collect();
        format!(",\n\n  // Kept from a newer version of the dock. Leave as is.\n{}", lines.join(",\n"))
    }

    /// Serializes `c` as a commented JSONC file: an English guide above every field, current values
    /// inline, so the comments survive every programmatic rewrite.
    pub(super) fn render(c: &Config) -> String {
        let mode = match c.display_mode {
            DisplayMode::IconText => "icon_text",
            DisplayMode::Icon => "icon",
            DisplayMode::Text => "text",
        };
        let pos = match c.position {
            Position::Top => "top",
            Position::Bottom => "bottom",
        };
        let bg = c.background.hex();
        let lang = match c.language {
            Language::Auto => "auto",
            Language::En => "en",
            Language::It => "it",
        };
        format!(
            r#"{TEMPLATE_MARK}
// Edit and save: changes are picked up within ~2 seconds, no restart needed.
// The folder is admin-only: edit this file with Notepad (it saves in place) or an elevated editor.
// Editors that save by writing a new file and renaming it over this one cannot write here.
// Comments (// and /* */) are allowed. If the file has a syntax error the previous
// settings are kept; if the dock must save a change (pin, tray icon) before you fix it,
// your file is first copied to config.json.bak so no edit is lost.
{{
  // Language of the dock: "auto" (follows the Windows display language), "en" or "it".
  // Also sets the language of the clock, date and calendar names. Normally chosen in Settings.
  "language": "{lang}",

  // Where the bar sits on screen: "top" or "bottom".
  "position": "{pos}",

  // Display the bar sits on: "" = the primary monitor, or a device name such as "\\\\.\\DISPLAY2".
  // Normally chosen in Settings -> Screen (shown with two or more displays). If that display is
  // not connected the bar uses the primary monitor, and moves back when it returns.
  // "pointer" = the Win key shows the bar (and a double tap the Start menu) on the display under the pointer.
  "monitor": {monitor},

  // What each entry shows: "icon_text" (icon + name), "icon" (icon only) or "text" (name only).
  // The indicator bar under each entry adapts to the chosen mode.
  "display_mode": "{mode}",

  // Sizes, each independent (the bar height follows the tallest content):
  //   app_icon_size   app icons, 14-32 px (default 16)
  //   app_text_size   app names, 10-18 px (default 13)
  //   tray_icon_size  tray icons, 12-24 px (default 16)
  //   module_size     start button, notifications, volume, network, metrics, battery, clock, show desktop and
  //                   power, 75-150 % (default 100)
  // Logical pixels: the Windows display scaling is applied on top.
  "app_icon_size": {icon_size},
  "app_text_size": {text_size},
  "tray_icon_size": {tray_size},
  "module_size": {module_size},

  // Background colour of the bar and the popups as a hex code, #RRGGBB (e.g. #000000, pure black,
  // ideal for OLED). The Settings window offers dark presets and a colour picker.
  "background": "{bg}",

  // Opacity of the bar and popup backgrounds in percent, 0-100 (0 = only text and icons are
  // visible; they still react to the mouse). Text and icons always stay fully opaque.
  "bar_opacity": {opacity},

  // Size of the Settings window in logical pixels (96 DPI), [width, height]. Remembered when you
  // resize it; at least [640, 460]; clamped to the display it opens on.
  "settings_window_size": {settings_size},

  // Hide the bar when the pointer leaves it, show it when you touch the screen edge.
  "autohide": {autohide},

  // Auto-hide timing (ms): show_delay_ms 0-1000 at the screen edge (default 150),
  // hide_delay_ms 0-5000 after the pointer leaves the bar (a new install writes 1000; a missing key reads 2500).
  // {reveal_guide}
  "show_delay_ms": {show_delay},
  "hide_delay_ms": {hide_delay},
  "reveal": {reveal},
  "push_threshold": {push_threshold},

  // Keyboard: win_numbers = Win+1..9/0 (also with Shift and Alt), win_t = Win+T and Win+Shift+T, win_b = Win+B,
  // peek = digit chips while Win is held. Owned only while hide_native_taskbar is true.
  "keyboard": {keyboard},

  // Resting the pointer on an app with several windows shows a live preview of each one (with its
  // title); click a preview to switch to that window. false = no previews and no extra window.
  "window_previews": {window_previews},

  // true = the Win key opens the Start menu together with the bar and the next tap closes both
  // (one animation). false = the Win key shows / hides the bar and a double tap opens the Start menu.
  "win_opens_start": {win_opens_start},

  // Layout of the dock's Start menu: "compact" (a quick launcher: search, one strip of pinned icons,
  // the list of all apps) or "windows11" (the full menu: one scrolling page with the pinned tiles
  // and all apps, user name and folder shortcuts at the bottom). Normally chosen in Settings. The old names "vertical" and
  // "horizontal" are still read; other values = "compact".
  "start_style": "{start_style}",

  // "All apps" in the "windows11" layout: false = a list, true = a grid of icons. Normally chosen
  // with the button next to "All apps" in the menu.
  "start_grid": {start_grid},

  // Hide Windows' own taskbar while the dock runs (restored when the dock exits).
  "hide_native_taskbar": {hide_native},

  // Show the date next to the clock.
  "show_date": {show_date},

  // Clock format (Windows time picture, uses the dock language, or your system locale with "auto").
  //   H / HH = hour 0-23      h / hh = hour 1-12      m / mm = minutes
  //   s / ss = seconds        t / tt = AM/PM marker
  // Examples: "HH:mm" -> 14:05 (24h)    "h:mm tt" -> 2:05 PM (12h)
  "time_format": {time_format},

  // true = always show seconds (":ss" is appended if time_format has no "s").
  // The clock then refreshes every second instead of every minute.
  "show_seconds": {show_seconds},

  // Date format (Windows date picture). Empty string "" = no date.
  //   d / dd = day of month   ddd / dddd = short / full weekday name
  //   M / MM = month number   MMM / MMMM = short / full month name
  //   yy / yyyy = year
  // Examples: "ddd d MMM" -> Fri 23 Oct    "dd/MM/yyyy" -> 23/10/2026
  "date_format": {date_format},

  // Show the percentage next to the volume icon.
  "show_volume_percent": {show_volume},

  // Host other apps' tray icons inside the dock. All visible icons are shown (no limit);
  // the bar widens and narrows dynamically.
  "show_tray": {show_tray},

  // true = show every tray icon in the bar, without choosing them one by one. "tray_hidden" is then ignored
  // (but kept as it is, so switching this off brings your earlier choices back). Icons that appear later are
  // shown too.
  "tray_show_all": {tray_show_all},

  // System modules on the right of the bar: true = shown, false = removed from the bar.
  // A hidden module costs nothing (no timer, no polling, no system library loaded).
  // The power button is always shown (it holds the dock's own menu).
  "show_notifications": {show_notif},
  "show_volume": {show_vol_mod},

  // Network: Wi-Fi signal bars or ethernet glyph, with a small globe when the internet is reachable
  // and a small x when it is not. Click for the connection details. Refreshed every ~6 s while
  // the bar is visible.
  "show_network": {show_network},

  // CPU and RAM usage in percent (two small lines). Refreshed every 2 s while the bar is visible.
  // Click to open Task Manager.
  "show_metrics": {show_metrics},

  // Battery: an Apple-style icon filled in proportion to the charge, with a bolt while charging.
  // Click for the details. Refreshed every 30 s while the bar is visible, and at once when the
  // charger is plugged or unplugged.
  //   show_battery            "auto" (only on a laptop) | true (always; an icon with a slash when there
  //                           is no battery) | false
  //   battery_percent         show the number
  //   battery_percent_inside  put the number inside the battery body (Apple style) instead of beside it
  "show_battery": {show_battery},
  "battery_percent": {battery_percent},
  "battery_percent_inside": {battery_percent_inside},

  "show_clock": {show_clock},
  "show_desktop_button": {show_desk},

  // Windows logo button at the left end of the bar: left click opens Start, right click the Win+X menu.
  "show_start_button": {show_start},

  // Tray icons you chose to keep out of the bar (they stay in the tray popup).
  // Normally managed from the tray popup. A key is the icon's GUID (lowercase, no braces),
  // e.g. "7820ae73-23e3-4229-82c1-e41cb67d5b9c", or for an icon without one
  // "<exe name lowercase>|<n>", n counting that app's icons from 0, e.g. "discord.exe|0".
  "tray_hidden": {tray_hidden},

  // Order of the tray icons in the bar (same keys as "tray_hidden"). Normally managed by
  // dragging an icon in the bar; icons not listed go last, in the order they registered.
  "tray_order": {tray_order},

  // System tray icons the dock never shows (volume, network, power: the dock draws those itself).
  "tray_hidden_guids": {tray_guids},

  // Internal: set once the privacy indicators (microphone, location) were let out of the list above. Leave as is.
  "tray_privacy_migrated": {tray_privacy_migrated},

  // Pinned apps, in order. Normally managed with Pin/Unpin in the app menu. Fields:
  //   "key"    grouping key: the AUMID, or the lowercase exe path
  //   "name"   fallback label, refreshed automatically from the app itself; use the "names"
  //            map to set a fixed label
  //   "launch" exe path, or "shell:AppsFolder\\<AUMID>" for Store/packaged apps
  //   "icon"   optional custom icon: plain PNG file name inside %ProgramData%\mini-oled-dock\icons\
  //            (e.g. "discord.png"; paths, "..", drive letters and UNC paths are rejected)
  // List the AUMIDs of installed apps with:  Get-StartApps   (PowerShell)
  "pinned": {pinned},

  // Apps pinned at the top of the Start menu that a double tap of the Win key opens (separate from
  // the bar pins above). Normally managed with a right click on an app in that menu. Each entry is
  // the app's id inside shell:AppsFolder (an AUMID for Store apps); see:  Get-StartApps  (PowerShell)
  "start_pins": {start_pins},

  // Folders of those pins: each one shows some of the pinned apps under a single tile, placed where
  // its first app is. Normally managed by dragging a pinned app onto another one in the menu.
  // Example: [{{ "name": "Games", "apps": ["<id>", "<id>"] }}]
  "start_folders": {start_folders},

  // Order of the open apps that are NOT pinned (their keys). Pinned apps follow the "pinned" order
  // above. Normally managed by dragging an entry in the bar; apps not listed go last, in the order
  // they were first seen. At most 64 keys are kept.
  "app_order": {app_order},

  // Order of the system buttons on the right of the bar. Normally managed by dragging them.
  // Names: "metrics", "notifications", "network", "volume", "battery", "clock", "show_desktop", "power".
  // Missing names go back next to their default neighbour, unknown names are ignored.
  // Hiding a module (the show_* switches above) keeps its place in this list.
  "modules_order": {modules_order},

  // Volume panel layout. Normally changed in the panel itself: click a section title to expand or
  // collapse it, or use the pencil button at the bottom (Customize) to drag sections into a new
  // order and to hide them. One letter per section, in display order: M = master volume,
  // O = output devices, S = spatial sound, X = volume mixer. Upper case = expanded, lower case =
  // collapsed, a leading "-" hides the section. Missing sections come back at the end.
  // Example: "M-OsX". Default: "MOSX".
  // A collapsed or hidden section reads nothing from Windows.
  "audio_layout": {audio_layout},

  // Display-name overrides. Key = exe file name lowercase (e.g. "code.exe") or AUMID;
  // value = the name to show. Example: {{ "code.exe": "VS Code" }}
  "names": {names},

  // Updates. Once a day the dock asks GitHub (api.github.com) whether a newer release exists and
  // shows a badge on Settings; nothing is downloaded until you choose "Update now" in Settings ->
  // Updates. false = never ask by itself ("Check now" still works).
  "update_check": {update_check},

  // true = also download, verify (SHA-256 + version) and install a new release by itself, the next
  // time no popup or Settings window is open and no fullscreen app is in front.
  "update_auto_install": {update_auto_install},

  // Internal: time of the last successful check (UTC) and the newest release found. Leave as is.
  "last_update_check": {last_update_check},
  "update_available": {update_available},

  // Launcher: a search box over the desktop (apps, open windows, settings, commands, calculator).
  // It stays off until "chord" is set. Unknown or out-of-range values fall back to the default.
  "launcher": {{
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
  }},

  // Internal: set once the notification-access prompt has been shown. Leave as is.
  "notif_prompted": {notif}{extra}
}}
"#,
            monitor = js(&c.monitor),
            opacity = c.bar_opacity.min(100),
            settings_size = js(&c.settings_window_size),
            icon_size = c.app_icon_size,
            text_size = c.app_text_size,
            tray_size = c.tray_icon_size,
            module_size = c.module_size,
            autohide = c.autohide,
            show_delay = c.show_delay_ms,
            hide_delay = c.hide_delay_ms,
            reveal_guide = reveal_guide(crate::ui::PUSH_AVAILABLE),
            reveal = if c.reveal == Reveal::Push { "\"push\"" } else { "\"dwell\"" },
            push_threshold = c.push_threshold,
            keyboard = format_args!(
                "{{\"win_numbers\":{},\"win_t\":{},\"win_b\":{},\"peek\":{}}}",
                c.keyboard.win_numbers, c.keyboard.win_t, c.keyboard.win_b, c.keyboard.peek
            ),
            window_previews = c.window_previews,
            win_opens_start = c.win_opens_start,
            start_style = c.start_style.as_str(),
            start_grid = c.start_grid,
            hide_native = c.hide_native_taskbar,
            show_date = c.show_date,
            time_format = js(&c.time_format),
            show_seconds = c.show_seconds,
            date_format = js(&c.date_format),
            show_volume = c.show_volume_percent,
            show_tray = c.show_tray,
            tray_show_all = c.tray_show_all,
            show_notif = c.show_notifications,
            show_vol_mod = c.show_volume,
            show_network = c.show_network,
            show_metrics = c.show_metrics,
            show_battery = js(&c.show_battery),
            battery_percent = c.battery_percent,
            battery_percent_inside = c.battery_percent_inside,
            show_clock = c.show_clock,
            show_desk = c.show_desktop_button,
            show_start = c.show_start_button,
            tray_hidden = js_block(&c.tray_hidden),
            tray_order = js_block(&c.tray_order),
            app_order = js_block(&c.app_order),
            modules_order = js_block(&c.modules_order),
            audio_layout = js(&c.audio_layout.encode()),
            tray_guids = js_block(&c.tray_hidden_guids),
            tray_privacy_migrated = c.tray_privacy_migrated,
            pinned = js_block(&c.pinned),
            start_pins = js_block(&c.start_pins),
            start_folders = js_block(&c.start_folders),
            names = js_block(&c.names),
            notif = c.notif_prompted,
            update_check = c.update_check,
            update_auto_install = c.update_auto_install,
            last_update_check = js(&c.last_update_check),
            update_available = js(&c.update_available),
            l_enabled = c.launcher.enabled,
            l_chord = js(&c.launcher.chord),
            l_with_powertoys = js(&c.launcher.with_powertoys),
            l_monitor = js(&c.launcher.monitor),
            l_top_percent = c.launcher.top_percent,
            l_width = c.launcher.width,
            l_rows = c.launcher.rows,
            l_animate = c.launcher.animate,
            l_keep_query = c.launcher.keep_query,
            l_over_fullscreen = js(&c.launcher.over_fullscreen),
            l_learn = c.launcher.learn,
            l_announce = c.launcher.announce_selection,
            l_providers = js_block(&c.launcher.providers).replace("\n", "\n  "),
            l_prefixes = js_block(&c.launcher.prefixes).replace("\n", "\n  "),
            l_aliases = js_block(&c.launcher.aliases).replace("\n", "\n  "),
            l_file_folders = js_block(&c.launcher.file_folders).replace("\n", "\n  "),
            l_web_engine = js(&c.launcher.web_engine),
            l_web_url = js(&c.launcher.web_url),
            l_offer_done = c.launcher.offer_done,
            l_powertoys_done = c.launcher.powertoys_done,
            extra = render_extra(&c.extra),
        )
    }
}
