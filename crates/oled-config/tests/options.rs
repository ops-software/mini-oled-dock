//! W5b: options! declares a config once (struct, defaults, keys, descriptors, template), and lint() reports each of
//! D's rules.

use oled_config::*;
use serde_json::Value;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Side {
    #[default]
    Left,
    Right,
}

impl Side {
    fn as_str(self) -> &'static str {
        match self {
            Side::Left => "left",
            Side::Right => "right",
        }
    }
}

impl Kind for Side {
    const KIND: ValueType = ValueType::Enum;
}

/// A nested section, written by a function of its own.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(default)]
pub struct Section {
    pub on: bool,
    pub width: u16,
}

impl Default for Section {
    fn default() -> Self {
        Section { on: true, width: 640 }
    }
}

fn section_text(s: &Sample) -> String {
    format!("{{\n    // On or off.\n    \"on\": {},\n    \"width\": {}\n  }}", s.section.on, s.section.width)
}

fn label_enabled() -> &'static str {
    "Enabled"
}

/// Version 1 called "tags" "labels".
fn rename_labels(v: &mut Value) -> Result<(), MigrateError> {
    if let Some(o) = v.as_object_mut() {
        if let Some(x) = o.remove("labels") {
            o.insert("tags".into(), x);
        }
    }
    Ok(())
}

options! {
    /// Six options: one of each form the dock's file needs.
    #[derive(Eq)]
    pub struct Sample(
        file: "sample.json",
        version: 2,
        mark: "// sample configuration",
        header: "// sample configuration\n// Edit and save.",
        migrations: &[rename_labels, rename_labels],
    ) {
        /// On or off.
        pub enabled: bool = true => {
            json: "enabled",
            slot: 0,
            fmt: Raw,
            gap: false,
            guide: "Turns the sample on.",
            since: 1,
            reason: "The one switch.",
            ui: Some {
                page: PageId("general"),
                section: SecId("main"),
                order: 1,
                control: Control::Toggle,
                label: label_enabled,
                help: || "Turns the sample on",
                glyph: "power",
                keywords: &[|| "switch", || "on"],
                range: None,
                scope: Scope::Global,
                profileable: false,
                visible_when: None,
                cost: None,
                takeover: None,
            },
        },
        /// Logical pixels.
        pub icon_size: u8 = 16 => {
            json: "icon_size",
            slot: 1,
            fmt: Raw,
            gap: true,
            guide: "Icon size in px, 14-32.",
            since: 1,
            reason: "Small screens.",
            ui: Some {
                page: PageId("general"),
                section: SecId("look"),
                order: 2,
                control: Control::Slider,
                label: || "Icon size",
                help: || "In logical pixels",
                glyph: "size",
                keywords: &[],
                range: Some(Range { min: 14, max: 32, step: 1, unit: Unit::Px }),
                scope: Scope::PerBar,
                profileable: true,
                visible_when: Some(Cond { json: "enabled", equals: "true" }),
                cost: Some(CostNote { kib: 1, wakeups_milli_per_s: 0 }),
                takeover: None,
            },
        },
        pub side: Side = Side::Left => {
            json: "side",
            slot: 2,
            fmt: Str(|s| s.as_str()),
            gap: true,
            guide: "Where it sits: \"left\" or \"right\".",
            since: 2,
            reason: "Left-handed users.",
            ui: Some {
                page: PageId("general"),
                section: SecId("look"),
                order: 3,
                control: Control::Segmented(&[(|| "Left", "left"), (|| "Right", "right")]),
                label: || "Side",
                help: || "Where the sample sits",
                glyph: "side",
                keywords: &[|| "left"],
                range: None,
                scope: Scope::PerModule,
                profileable: false,
                visible_when: None,
                cost: None,
                takeover: Some(Takeover::WinE),
            },
        },
        pub title: String = String::from("Hello \"you\"") => {
            json: "title",
            slot: 3,
            fmt: Js,
            gap: true,
            guide: "",
            since: 1,
            reason: "A name.",
            ui: Some {
                page: PageId("about"),
                section: SecId("main"),
                order: 1,
                control: Control::Text { max: 64 },
                label: || "Title",
                help: || "The sample's name",
                glyph: "text",
                keywords: &[],
                range: None,
                scope: Scope::PerApp,
                profileable: false,
                visible_when: None,
                cost: None,
                takeover: None,
            },
        },
        pub tags: Vec<String> = vec!["a".to_string(), "b".to_string()] => {
            json: "tags",
            slot: 4,
            fmt: Block,
            gap: true,
            guide: "Tags, one per line.\nAn empty list is fine.",
            since: 1,
            reason: "Internal.",
            ui: None,
        },
        pub section: Section = Section::default() => {
            json: "section",
            slot: 5,
            fmt: Custom(section_text),
            gap: true,
            guide: "A nested section.",
            since: 1,
            reason: "Internal.",
            ui: None,
        },
    }
}

const SAMPLE_TEMPLATE: &str = r#"// sample configuration
// Edit and save.
{
  // Turns the sample on.
  "enabled": true,

  // Icon size in px, 14-32.
  "icon_size": 16,

  // Where it sits: "left" or "right".
  "side": "left",

  "title": "Hello \"you\"",

  // Tags, one per line.
  // An empty list is fine.
  "tags": [
    "a",
    "b"
  ],

  // A nested section.
  "section": {
    // On or off.
    "on": true,
    "width": 640
  }
}
"#;

#[test]
fn defaults_come_from_the_declaration() {
    let s = Sample::default();
    assert!(s.enabled);
    assert_eq!(s.icon_size, 16);
    assert_eq!(s.side, Side::Left);
    assert_eq!(s.title, "Hello \"you\"");
    assert_eq!(s.tags, ["a", "b"]);
    assert_eq!(s.section, Section::default());
    assert!(s.extra.is_empty());
    assert_eq!((Sample::VERSION, Sample::FILE, Sample::template_mark()), (2, "sample.json", "// sample configuration"));
    assert_eq!(Sample::migrations().len(), 2);
}

#[test]
fn known_keys_in_declaration_order() {
    assert_eq!(Sample::known_keys(), ["enabled", "icon_size", "side", "title", "tags", "section"]);
}

#[test]
fn a_field_with_ui_none_is_accepted() {
    let shown: Vec<&str> = Sample::descriptors().iter().map(|d| d.json).collect();
    assert_eq!(shown, ["enabled", "icon_size", "side", "title"]);
    assert!(Sample::known_keys().contains(&"tags") && Sample::known_keys().contains(&"section"));
}

#[test]
fn descriptors_carry_every_field_as_written() {
    let d = Sample::descriptors();
    assert_eq!(d.len(), 4);
    let kinds: Vec<ValueType> = d.iter().map(|d| d.kind).collect();
    assert_eq!(kinds, [ValueType::Bool, ValueType::Int, ValueType::Enum, ValueType::Str]);
    let e = &d[0];
    assert_eq!(
        (e.path, e.json, e.glyph, e.order, e.since, e.reason),
        ("enabled", "enabled", "power", 1, 1, "The one switch.")
    );
    assert_eq!(((e.default)(), (e.label)(), (e.help)()), (Value::Bool(true), "Enabled", "Turns the sample on"));
    assert_eq!((e.page, e.section, e.scope, e.profileable), (PageId("general"), SecId("main"), Scope::Global, false));
    let words: Vec<&str> = e.keywords.iter().map(|k| k()).collect();
    assert_eq!(words, ["switch", "on"]);
    assert!(matches!(e.control, Control::Toggle));
    assert!(e.range.is_none() && e.visible_when.is_none() && e.cost.is_none() && e.takeover.is_none());
    let i = &d[1];
    assert_eq!((i.path, (i.default)(), (i.label)()), ("icon_size", Value::from(16), "Icon size"));
    assert_eq!(i.range, Some(Range { min: 14, max: 32, step: 1, unit: Unit::Px }));
    assert_eq!((i.scope, i.profileable), (Scope::PerBar, true));
    assert!(matches!(i.visible_when, Some(Cond { json: "enabled", equals: "true" })));
    assert!(matches!(i.cost, Some(CostNote { kib: 1, wakeups_milli_per_s: 0 })));
    assert!(matches!(i.control, Control::Slider));
    let s = &d[2];
    assert_eq!(
        ((s.default)(), s.since, s.takeover, s.scope),
        (Value::from("left"), 2, Some(Takeover::WinE), Scope::PerModule)
    );
    let Control::Segmented(choices) = s.control else { panic!("side is a segmented control") };
    let choices: Vec<(&str, &str)> = choices.iter().map(|(t, v)| (t(), *v)).collect();
    assert_eq!(choices, [("Left", "left"), ("Right", "right")]);
    let t = &d[3];
    assert_eq!(((t.default)(), t.page, t.scope), (Value::from("Hello \"you\""), PageId("about"), Scope::PerApp));
    assert!(matches!(t.control, Control::Text { max: 64 }));
}

#[test]
fn to_template_writes_the_literal_file() {
    assert_eq!(Sample::default().to_template(), SAMPLE_TEMPLATE);
    let mut s = Sample { icon_size: 20, side: Side::Right, tags: Vec::new(), ..Sample::default() };
    s.extra.insert("zeta".into(), serde_json::json!({"x": 1}));
    s.extra.insert("alpha".into(), Value::from(true));
    let text = s.to_template();
    assert!(text.contains("\n  \"icon_size\": 20,\n") && text.contains("\n  \"side\": \"right\",\n"), "{text}");
    assert!(text.contains("\n  \"tags\": [],\n"), "{text}");
    let tail =
        "    \"width\": 640\n  },\n\n  // Kept from a newer version of the dock. Leave as is.\n  \"alpha\": true,\n  \
                \"zeta\": {\"x\":1}\n}\n";
    assert!(text.ends_with(tail), "{text}");
}

#[test]
fn from_value_reads_the_whole_file_or_nothing() {
    let text = "// c\n{ \"icon_size\": 20, \"side\": \"right\", \"newer\": [1] }";
    let v: Value = serde_json::from_str(&jsonc::strip_comments(text)).unwrap();
    let (s, errors) = Sample::from_value(&v, text, None);
    assert!(errors.is_empty());
    assert_eq!((s.icon_size, s.side, s.enabled), (20, Side::Right, true));
    assert_eq!(s.extra.get("newer"), Some(&serde_json::json!([1])));
    // A wrong type: the last good value is kept, the error has the text's position.
    let bad = "{\n  \"enabled\": true,\n  \"icon_size\": \"big\"\n}";
    let v: Value = serde_json::from_str(bad).unwrap();
    let (kept, errors) = Sample::from_value(&v, bad, Some(&s));
    assert_eq!(kept, s);
    assert_eq!(errors.len(), 1);
    assert_eq!((errors[0].line, errors[0].kept), (3, Kept::LastGood));
    assert!(matches!(&errors[0].problem, Problem::Parse(m) if m.starts_with("invalid type: string \"big\"")));
    let (fresh, errors) = Sample::from_value(&v, bad, None);
    assert_eq!((fresh, errors[0].kept), (Sample::default(), Kept::Default));
}

#[test]
fn the_template_reads_back_as_the_same_config() {
    let mut s = Sample { enabled: false, title: "a \"quoted\" \\ name, ü".into(), ..Sample::default() };
    s.extra.insert("later".into(), serde_json::json!({"k": [1, 2]}));
    let text = s.to_template();
    let v: Value = serde_json::from_str(&jsonc::strip_comments(&text)).unwrap();
    assert_eq!(Sample::from_value(&v, &text, None), (s, Vec::new()));
}

options! {
    pub struct Abc(file: "abc.json", version: 1, mark: "// abc", header: "// abc") {
        pub a: u8 = 1 => { json: "a", slot: 3, fmt: Raw, gap: false, guide: "", since: 1, reason: "", ui: None },
        pub b: u8 = 2 => { json: "b", slot: 1, fmt: Raw, gap: false, guide: "", since: 1, reason: "", ui: None },
        pub c: u8 = 3 => { json: "c", slot: 2, fmt: Raw, gap: false, guide: "", since: 1, reason: "", ui: None },
    }
}

#[test]
fn slots_order_the_template() {
    assert_eq!(serde_json::to_string(&Abc::default()).unwrap(), r#"{"a":1,"b":2,"c":3}"#);
    assert_eq!(Abc::default().to_template(), "// abc\n{\n  \"b\": 2,\n  \"c\": 3,\n  \"a\": 1\n}\n");
    assert_eq!(Abc::known_keys(), ["a", "b", "c"]);
    assert!(Abc::descriptors().is_empty() && Abc::migrations().is_empty());
}

#[test]
fn a_store_reads_and_writes_a_declared_config() {
    let nanos = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.subsec_nanos());
    let dir = std::env::temp_dir().join(format!("oled_config_options_{}_{nanos}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("sample.json");
    // A version 1 file without the mark: migrated and rewritten once with the template.
    std::fs::write(&path, "{\"labels\": [\"x\"], \"icon_size\": 18}").unwrap();
    let store: Store<Sample> =
        Store::new(Some(FileSpec { path: path.clone(), kind: FileKind::PerUser, cap: 1 << 16 }), |_| {});
    assert_eq!(store.load(|| None), LoadOutcome::Rewritten);
    assert_eq!((store.get().tags.clone(), store.get().icon_size), (vec!["x".to_string()], 18));
    assert_eq!(std::fs::read_to_string(&path).unwrap(), store.get().to_template());
    let _ = std::fs::remove_dir_all(&dir);
}

/// A row that passes every rule.
fn row(json: &'static str, section: &'static str) -> Descriptor {
    let mut d = Sample::descriptors()[0];
    d.json = json;
    d.path = json;
    d.glyph = "";
    d.section = SecId(section);
    d.page = PageId("p");
    d
}

/// Italian for the test: an empty English text has a translation, one English help has none.
fn italian(f: TextFn) -> &'static str {
    match f() {
        "" => "testo",
        "help only in English" => "",
        other => other,
    }
}

#[test]
fn lint_reports_each_rule_once() {
    const CROWD: [&str; 13] = ["c0", "c1", "c2", "c3", "c4", "c5", "c6", "c7", "c8", "c9", "c10", "c11", "c12"];
    let mut t = Vec::new();
    let mut d = row("lbl", "s");
    d.label = || "";
    t.push(d);
    let mut d = row("hlp", "s");
    d.help = || "help only in English";
    t.push(d);
    let mut d = row("rsn", "s");
    d.reason = "";
    t.push(d);
    let mut d = row("dflt", "s");
    d.range = Some(Range { min: 14, max: 32, step: 1, unit: Unit::Px });
    d.default = || Value::from(40);
    t.push(d);
    for j in ["g1", "g2"] {
        let mut d = row(j, "s");
        d.glyph = "star";
        t.push(d);
    }
    t.push(row("dup", "s"));
    t.push(row("dup", "s"));
    t.extend(CROWD.iter().map(|j| row(j, "crowded")));
    let found = lint(&t, &italian);
    let want = [
        "lbl: empty label (en)",
        "hlp: empty help (it)",
        "rsn: empty reason",
        "dflt: default 40 outside 14..=32",
        "p: glyph star used by g1, g2",
        "p/crowded: 13 rows (at most 12)",
        "dup: json path used 2 times",
    ];
    assert_eq!(found, want);
    // The declared table is clean, and a range default inside the range passes.
    assert!(lint(Sample::descriptors(), &|f| f()).is_empty());
    // An empty glyph is no glyph: never a duplicate.
    assert!(lint(&[row("x", "s"), row("y", "s")], &|f| f()).is_empty());
}

#[test]
fn kinds_of_the_common_types() {
    assert_eq!(<bool as Kind>::KIND, ValueType::Bool);
    assert_eq!((<u8 as Kind>::KIND, <i64 as Kind>::KIND), (ValueType::Int, ValueType::Int));
    assert_eq!(<String as Kind>::KIND, ValueType::Str);
    assert_eq!((<Vec<u8> as Kind>::KIND, <[u16; 2] as Kind>::KIND), (ValueType::List, ValueType::List));
    assert_eq!(<std::collections::BTreeMap<String, String> as Kind>::KIND, ValueType::Map);
    assert_eq!(<Option<u32> as Kind>::KIND, ValueType::Int);
}

/// xorshift64: the fuzz inputs, seeded, so a failure repeats.
fn next(s: &mut u64) -> u64 {
    *s ^= *s << 13;
    *s ^= *s >> 7;
    *s ^= *s << 17;
    *s
}

/// One of `items`, chosen by the next random number.
fn pick(s: &mut u64, items: &[&'static str]) -> &'static str {
    items.get((next(s) % items.len() as u64) as usize).copied().unwrap_or("")
}

/// A JSONC text: an object of Sample's keys (and others) with values of every type, so of wrong ones too, with a
/// BOM, comments and stray pieces mixed in.
fn fuzz_sample_text(s: &mut u64) -> String {
    const KEYS: [&str; 12] = [
        "\"enabled\"",
        "\"icon_size\"",
        "\"side\"",
        "\"title\"",
        "\"tags\"",
        "\"section\"",
        "\"on\"",
        "\"width\"",
        "\"labels\"",
        "\"version\"",
        "\"newer\"",
        "\"\"",
    ];
    const VALUES: [&str; 25] = [
        "true",
        "false",
        "null",
        "0",
        "16",
        "255",
        "256",
        "-1",
        "1.5",
        "-0",
        "1e999",
        "99999999999999999999",
        "\"left\"",
        "\"right\"",
        "\"up\"",
        "\"\"",
        "\"a \\\"b\\\" // c /* d */ \\u00e9\"",
        "[]",
        "[\"a\"]",
        "[1, \"b\"]",
        "{}",
        "{\"on\": false, \"width\": 3}",
        "{\"on\": 1}",
        "{\"width\": 70000, \"x\": [null]}",
        "[[[]]]",
    ];
    const NOISE: [&str; 14] =
        ["/* c */", "// c\n", "\n", "\r\n", " ", ",", ":", "{", "}", "[", "\"", "\\", "/*", "\u{feff}"];
    let mut t = String::new();
    if next(s).is_multiple_of(4) {
        t.push('\u{feff}');
    }
    if next(s).is_multiple_of(2) {
        t.push_str("// sample configuration\n");
    }
    t.push('{');
    for i in 0..next(s) % 9 {
        if i > 0 {
            t.push(',');
        }
        if next(s).is_multiple_of(5) {
            t.push_str(pick(s, &NOISE));
        }
        t.push_str(pick(s, &KEYS));
        t.push_str(": ");
        t.push_str(pick(s, &VALUES));
    }
    if next(s).is_multiple_of(7) {
        t.push_str(pick(s, &NOISE));
    }
    t.push('}');
    t
}

/// The generated from_value over fuzz_sample_text's texts, as the store calls it (the BOM and the comments
/// stripped first), without and with a last good value: it returns; an error is one Problem::Parse that keeps the
/// last good value (or the defaults) and says which; a value read whole writes a template that reads back as itself.
/// Returns how many calls read a value and how many refused the file.
fn fuzz_options_from_value(runs: u32) -> (u32, u32) {
    let mut s = 0x5851_f42d_4c95_7f2du64;
    let last = Sample { icon_size: 20, side: Side::Right, title: "last".into(), ..Sample::default() };
    let (mut read, mut refused) = (0, 0);
    for _ in 0..runs {
        let t = fuzz_sample_text(&mut s);
        let Ok(v) = serde_json::from_str::<Value>(&jsonc::strip_comments(jsonc::strip_bom(&t))) else { continue };
        for last_good in [None, Some(&last)] {
            let (c, errors) = Sample::from_value(&v, &t, last_good);
            match errors.as_slice() {
                [] => {
                    read += 1;
                    let text = c.to_template();
                    let back: Value = serde_json::from_str(&jsonc::strip_comments(&text)).unwrap();
                    assert_eq!(Sample::from_value(&back, &text, None), (c, Vec::new()), "{t:?}");
                }
                [e] => {
                    refused += 1;
                    assert!(matches!(e.problem, Problem::Parse(_)), "{t:?}");
                    let kept = if last_good.is_some() { Kept::LastGood } else { Kept::Default };
                    assert_eq!(e.kept, kept, "{t:?}");
                    assert_eq!(c, last_good.cloned().unwrap_or_default(), "{t:?}");
                    let _ = e.log_line();
                }
                more => panic!("{} errors for {t:?}", more.len()),
            }
        }
    }
    (read, refused)
}

#[test]
fn fuzz_options_from_value_10k() {
    let (read, refused) = fuzz_options_from_value(10_000);
    // Both ways are taken often: the texts are not stuck on one of them.
    assert!(read > 1_000 && refused > 1_000, "{read} read, {refused} refused");
}

#[test]
#[ignore = "nightly fuzz: 1,000,000 inputs"]
fn fuzz_options_from_value_1m() {
    fuzz_options_from_value(1_000_000);
}
