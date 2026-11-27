//! The suite manifest (suite.json): components, hashes and the convergence plan (F-P6).
//! The parser is total and capped: an elevated process reads files and downloads a medium process may have touched.

use std::collections::HashSet;
use std::fmt::{self, Write as _};

use serde::de::{self, Deserialize, Deserializer, MapAccess, SeqAccess, Visitor};

use super::release::Version;
pub const SUITE_ASSET: &str = "suite.json";
pub const SUITE_MAX_BYTES: u64 = 64 * 1024;
pub const SUITE_SCHEMA: u16 = 1;
pub const COMPONENT_MAX_BYTES: u64 = 32 * 1024 * 1024;
pub const SUITE_FILES: [&str; 2] = ["suite.json", "suite.json.tmp"]; // install-relative files of the suite itself
pub const LANG_DIR: &str = "lang";
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum ComponentId {
    Dock,
    Tasks,
    TasksAdmin,
    Files,
    LangIt,
}
impl ComponentId {
    // implemented in PF1 (P-FC ships it in 1.7.0)
    pub const ALL: [ComponentId; 5] =
        [ComponentId::Dock, ComponentId::Tasks, ComponentId::TasksAdmin, ComponentId::Files, ComponentId::LangIt]; // in this order; the manifest lists components in it
    pub fn id(self) -> &'static str {
        match self {
            ComponentId::Dock => "dock",
            ComponentId::Tasks => "tasks",
            ComponentId::TasksAdmin => "tasks-admin",
            ComponentId::Files => "files",
            ComponentId::LangIt => "lang-it",
        }
    }
    pub fn file(self) -> &'static str {
        match self {
            ComponentId::Dock => "dock.exe",
            ComponentId::Tasks => "tasks.exe",
            ComponentId::TasksAdmin => "tasks-admin.exe",
            ComponentId::Files => "files.exe",
            ComponentId::LangIt => "lang\\it.lang",
        }
    }
    pub fn prev(self) -> &'static str {
        match self {
            ComponentId::Dock => "dock.prev.exe",
            ComponentId::Tasks => "tasks.prev.exe",
            ComponentId::TasksAdmin => "tasks-admin.prev.exe",
            ComponentId::Files => "files.prev.exe",
            ComponentId::LangIt => "lang\\it.prev.lang",
        }
    }
    pub fn asset(self) -> &'static str {
        match self {
            ComponentId::Dock => "dock.exe",
            ComponentId::Tasks => "tasks.exe",
            ComponentId::TasksAdmin => "tasks-admin.exe",
            ComponentId::Files => "files.exe",
            ComponentId::LangIt => "lang-it.lang",
        }
    }
    pub fn install_names(self) -> [String; 4] {
        let (file, prev) = (self.file(), self.prev());
        [file.to_string(), format!("{file}.new"), prev.to_string(), format!("{prev}.new")]
    }
    pub fn parse(s: &str) -> Option<ComponentId> {
        ComponentId::ALL.into_iter().find(|c| c.id() == s)
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Stage {
    Beta,
    Stable,
} // ROADMAP X34; "beta" | "stable"
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Component {
    pub id: ComponentId,
    pub version: Version,
    pub sha256: [u8; 32],
    pub size: u64,
    pub stage: Stage,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SuiteManifest {
    pub schema: u16,
    pub version: Version,
    pub components: Vec<Component>,
}
/// suite.json's bytes to a manifest (F-P6's rules): at most SUITE_MAX_BYTES; one JSON object without a key seen twice;
/// "schema" an integer >= 1 (schema 1 refuses a field it does not know, a later schema is read with its unknown fields
/// ignored); "version" canonical X.Y.Z; per component "id" known (an unknown id is skipped) and unique, "file" that id's
/// file() with '/' or '\', "version" the manifest's, "sha256" 64 hex digits, "size" 1..=COMPONENT_MAX_BYTES, "stage"
/// beta or stable; dock required. The components come back in ComponentId::ALL order.
pub fn parse_manifest(bytes: &[u8]) -> Result<SuiteManifest, String> {
    if bytes.len() as u64 > SUITE_MAX_BYTES {
        return Err(format!("suite.json: more than {SUITE_MAX_BYTES} bytes"));
    }
    let root: Json = serde_json::from_slice(bytes).map_err(|e| format!("suite.json: {e}"))?;
    let Json::Obj(top) = root else { return Err("suite.json: not a JSON object".into()) };
    let schema = match field(&top, "schema")? {
        Json::Num(Some(n)) if (1..=u64::from(u16::MAX)).contains(n) => *n as u16,
        _ => return Err("suite.json: \"schema\" is not an integer from 1 to 65535".into()),
    };
    let strict = schema == SUITE_SCHEMA;
    if strict {
        known_keys(&top, &["schema", "version", "components"], "the manifest")?;
    }
    let version = canonical_version(field(&top, "version")?).ok_or("suite.json: \"version\" is not X.Y.Z")?;
    let Json::Arr(items) = field(&top, "components")? else {
        return Err("suite.json: \"components\" is not a list".into());
    };
    let mut components: Vec<Component> = Vec::new();
    for item in items {
        let Json::Obj(c) = item else { return Err("suite.json: a component is not an object".into()) };
        let Json::Str(name) = field(c, "id")? else {
            return Err("suite.json: a component \"id\" is not a string".into());
        };
        let Some(id) = ComponentId::parse(name) else { continue }; // a component of a later release
        let what = id.id();
        if components.iter().any(|k| k.id == id) {
            return Err(format!("suite.json: {what} is listed twice"));
        }
        if strict {
            known_keys(c, &["id", "file", "version", "sha256", "size", "stage"], what)?;
        }
        match field(c, "file")? {
            Json::Str(f) if f.replace('/', "\\") == id.file() => {}
            _ => return Err(format!("suite.json: {what}: \"file\" is not {}", id.file())),
        }
        if canonical_version(field(c, "version")?) != Some(version) {
            return Err(format!("suite.json: {what}: \"version\" is not the manifest's {version}"));
        }
        let sha256 = match field(c, "sha256")? {
            Json::Str(h) => parse_hex32(h),
            _ => None,
        }
        .ok_or_else(|| format!("suite.json: {what}: \"sha256\" is not 64 hex digits"))?;
        let size = match field(c, "size")? {
            Json::Num(Some(n)) if (1..=COMPONENT_MAX_BYTES).contains(n) => *n,
            _ => return Err(format!("suite.json: {what}: \"size\" is not an integer from 1 to {COMPONENT_MAX_BYTES}")),
        };
        let stage = match field(c, "stage")? {
            Json::Str(s) if s == "beta" => Stage::Beta,
            Json::Str(s) if s == "stable" => Stage::Stable,
            _ => return Err(format!("suite.json: {what}: \"stage\" is not beta or stable")),
        };
        components.push(Component { id, version, sha256, size, stage });
    }
    if !components.iter().any(|c| c.id == ComponentId::Dock) {
        return Err("suite.json: no dock component".into());
    }
    components.sort_by_key(|c| c.id);
    Ok(SuiteManifest { schema, version, components })
}

/// F-P6's exact text: UTF-8, LF line ends and a final LF, two-space indent, one component per line in ComponentId::ALL
/// order, keys in F-P6's order, "file" with '/', lower-case hex.
pub fn encode_manifest(m: &SuiteManifest) -> String {
    let mut parts: Vec<&Component> = m.components.iter().collect();
    parts.sort_by_key(|c| c.id);
    let mut s = String::with_capacity(64 + 200 * parts.len());
    // Writing into a String cannot fail.
    let _ = write!(s, "{{\n  \"schema\": {},\n  \"version\": \"{}\",\n  \"components\": [\n", m.schema, m.version);
    for (i, c) in parts.iter().enumerate() {
        let _ = writeln!(
            s,
            "    {{\"id\": \"{}\", \"file\": \"{}\", \"version\": \"{}\", \"sha256\": \"{}\", \"size\": {}, \"stage\": \"{}\"}}{}",
            c.id.id(),
            c.id.file().replace('\\', "/"),
            c.version,
            Hex(&c.sha256),
            c.size,
            match c.stage {
                Stage::Beta => "beta",
                Stage::Stable => "stable",
            },
            if i + 1 < parts.len() { "," } else { "" }
        );
    }
    s.push_str("  ]\n}\n");
    s
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct OnDisk {
    pub id: ComponentId,
    pub current: Option<[u8; 32]>,
    pub prev: Option<[u8; 32]>,
    pub staged: Option<[u8; 32]>,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Source {
    Stage,
    Prev,
    Download,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Step {
    Keep(ComponentId),
    Add(ComponentId, Source),
    Replace(ComponentId, Source),
    Skip(ComponentId),
}
pub fn plan_convergence(
    own: &SuiteManifest,
    disk: &[OnDisk],
    bad: &[(ComponentId, [u8; 32])],
    network: bool,
) -> Vec<Step> {
    let mut steps = Vec::new();
    for id in ComponentId::ALL {
        if id == ComponentId::Dock {
            continue; // the guard owns dock.exe
        }
        let on_disk = disk.iter().find(|d| d.id == id);
        let current = on_disk.and_then(|d| d.current);
        let Some(want) = own.components.iter().find(|c| c.id == id).map(|c| c.sha256) else {
            if current.is_some() {
                steps.push(Step::Keep(id)); // on disk, not in this release: left alone
            }
            continue;
        };
        if current == Some(want) {
            steps.push(Step::Keep(id));
            continue;
        }
        let source = if bad.contains(&(id, want)) {
            None // three strikes for this build: wait for another hash
        } else if on_disk.and_then(|d| d.prev) == Some(want) {
            Some(Source::Prev)
        } else if on_disk.and_then(|d| d.staged) == Some(want) {
            Some(Source::Stage)
        } else if network {
            Some(Source::Download)
        } else {
            None
        };
        steps.push(match source {
            None => Step::Skip(id),
            Some(s) if current.is_some() => Step::Replace(id, s),
            Some(s) => Step::Add(id, s),
        });
    }
    steps
}
//   never a step for Dock (the guard owns dock.exe); Keep when current == own hash; else Prev if prev == own hash, else Stage if
//   staged == own hash, else Download if network, else Skip; Skip also for a bad (id, hash) with three strikes; a component on
//   disk but absent from own is Keep

// ---------------------------------------------------------------- the strict JSON tree

/// A JSON value whose objects keep their keys in order and refuse a key seen twice (serde_json's own Value keeps the
/// last one silently). Nesting is bounded by serde_json's recursion limit, size by SUITE_MAX_BYTES.
enum Json {
    Null,
    Bool,
    /// Some for an integer that fits u64; None for any other number.
    Num(Option<u64>),
    Str(String),
    Arr(Vec<Json>),
    Obj(Vec<(String, Json)>),
}

impl<'de> Deserialize<'de> for Json {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Json, D::Error> {
        d.deserialize_any(JsonVisitor)
    }
}

struct JsonVisitor;

impl<'de> Visitor<'de> for JsonVisitor {
    type Value = Json;

    fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("a JSON value")
    }
    fn visit_unit<E: de::Error>(self) -> Result<Json, E> {
        Ok(Json::Null)
    }
    fn visit_bool<E: de::Error>(self, _: bool) -> Result<Json, E> {
        Ok(Json::Bool)
    }
    fn visit_u64<E: de::Error>(self, n: u64) -> Result<Json, E> {
        Ok(Json::Num(Some(n)))
    }
    fn visit_i64<E: de::Error>(self, n: i64) -> Result<Json, E> {
        Ok(Json::Num(u64::try_from(n).ok()))
    }
    fn visit_f64<E: de::Error>(self, _: f64) -> Result<Json, E> {
        Ok(Json::Num(None))
    }
    fn visit_str<E: de::Error>(self, s: &str) -> Result<Json, E> {
        Ok(Json::Str(s.to_owned()))
    }
    fn visit_string<E: de::Error>(self, s: String) -> Result<Json, E> {
        Ok(Json::Str(s))
    }
    fn visit_seq<A: SeqAccess<'de>>(self, mut seq: A) -> Result<Json, A::Error> {
        let mut items = Vec::new();
        while let Some(item) = seq.next_element::<Json>()? {
            items.push(item);
        }
        Ok(Json::Arr(items))
    }
    fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<Json, A::Error> {
        let mut seen: HashSet<String> = HashSet::new();
        let mut fields = Vec::new();
        while let Some(key) = map.next_key::<String>()? {
            if !seen.insert(key.clone()) {
                return Err(de::Error::custom("a key appears twice in one object"));
            }
            fields.push((key, map.next_value::<Json>()?));
        }
        Ok(Json::Obj(fields))
    }
}

/// The value of a required key.
fn field<'a>(obj: &'a [(String, Json)], key: &str) -> Result<&'a Json, String> {
    obj.iter().find(|(k, _)| k == key).map(|(_, v)| v).ok_or_else(|| format!("suite.json: \"{key}\" is missing"))
}

/// Schema 1 names every field it may hold.
fn known_keys(obj: &[(String, Json)], known: &[&str], what: &str) -> Result<(), String> {
    match obj.iter().find(|(k, _)| !known.contains(&k.as_str())) {
        Some(_) => Err(format!("suite.json: {what} holds a field schema {SUITE_SCHEMA} does not know")),
        None => Ok(()),
    }
}

/// X.Y.Z exactly as Version writes it (no 'v', no leading zero, no space).
fn canonical_version(v: &Json) -> Option<Version> {
    let Json::Str(s) = v else { return None };
    Version::parse(s).filter(|v| v.to_string() == *s)
}

/// 64 hex digits, either case.
fn parse_hex32(s: &str) -> Option<[u8; 32]> {
    let b = s.as_bytes();
    if b.len() != 64 {
        return None;
    }
    let digit = |c: u8| (c as char).to_digit(16).map(|d| d as u8);
    let mut out = [0u8; 32];
    for (o, pair) in out.iter_mut().zip(b.chunks_exact(2)) {
        let &[hi, lo] = pair else { return None };
        *o = (digit(hi)? << 4) | digit(lo)?;
    }
    Some(out)
}

/// Lower-case hex of a hash.
struct Hex<'a>(&'a [u8; 32]);

impl fmt::Display for Hex<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.iter().try_for_each(|b| write!(f, "{b:02x}"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// F-P6's two-component example, exactly.
    const EXAMPLE: &str = r#"{
  "schema": 1,
  "version": "1.8.0",
  "components": [
    {"id": "dock", "file": "dock.exe", "version": "1.8.0", "sha256": "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad", "size": 1966080, "stage": "stable"},
    {"id": "tasks", "file": "tasks.exe", "version": "1.8.0", "sha256": "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855", "size": 917504, "stage": "beta"}
  ]
}
"#;

    const H_DOCK: &str = "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad";
    const H_TASKS: &str = "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855";
    const H_ADMIN: &str = "00112233445566778899aabbccddeeff00112233445566778899aabbccddeeff";
    const H_FILES: &str = "ffeeddccbbaa99887766554433221100ffeeddccbbaa99887766554433221100";
    const H_LANG: &str = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";

    /// A five-component manifest in F-P6's exact text.
    const FIVE: &str = r#"{
  "schema": 1,
  "version": "1.8.0",
  "components": [
    {"id": "dock", "file": "dock.exe", "version": "1.8.0", "sha256": "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad", "size": 1966080, "stage": "stable"},
    {"id": "tasks", "file": "tasks.exe", "version": "1.8.0", "sha256": "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855", "size": 917504, "stage": "beta"},
    {"id": "tasks-admin", "file": "tasks-admin.exe", "version": "1.8.0", "sha256": "00112233445566778899aabbccddeeff00112233445566778899aabbccddeeff", "size": 409600, "stage": "beta"},
    {"id": "files", "file": "files.exe", "version": "1.8.0", "sha256": "ffeeddccbbaa99887766554433221100ffeeddccbbaa99887766554433221100", "size": 1048576, "stage": "beta"},
    {"id": "lang-it", "file": "lang/it.lang", "version": "1.8.0", "sha256": "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef", "size": 40960, "stage": "stable"}
  ]
}
"#;

    fn v(s: &str) -> Version {
        Version::parse(s).expect("a test version")
    }

    fn hash(hex: &str) -> [u8; 32] {
        let mut out = [0u8; 32];
        for (i, b) in out.iter_mut().enumerate() {
            *b = u8::from_str_radix(&hex[2 * i..2 * i + 2], 16).expect("test hex");
        }
        out
    }

    fn comp(id: ComponentId, sha: &str, size: u64, stage: Stage) -> Component {
        Component { id, version: v("1.8.0"), sha256: hash(sha), size, stage }
    }

    /// One component line in any shape the tests need.
    fn line(id: &str, file: &str, version: &str, sha: &str, size: &str, stage: &str) -> String {
        format!(
            r#"{{"id": "{id}", "file": "{file}", "version": "{version}", "sha256": "{sha}", "size": {size}, "stage": "{stage}"}}"#
        )
    }

    fn doc(schema: &str, version: &str, lines: &[String]) -> String {
        format!("{{\"schema\": {schema}, \"version\": \"{version}\", \"components\": [{}]}}", lines.join(", "))
    }

    fn dock_line() -> String {
        line("dock", "dock.exe", "1.8.0", H_DOCK, "1966080", "stable")
    }

    fn tasks_line() -> String {
        line("tasks", "tasks.exe", "1.8.0", H_TASKS, "917504", "beta")
    }

    fn parse_str(s: &str) -> Result<SuiteManifest, String> {
        parse_manifest(s.as_bytes())
    }

    #[test]
    fn the_example_is_exact() {
        let m = parse_str(EXAMPLE).expect("the example parses");
        assert_eq!(m.schema, 1);
        assert_eq!(m.version, v("1.8.0"));
        assert_eq!(
            m.components,
            vec![
                comp(ComponentId::Dock, H_DOCK, 1_966_080, Stage::Stable),
                comp(ComponentId::Tasks, H_TASKS, 917_504, Stage::Beta)
            ]
        );
        assert_eq!(encode_manifest(&m), EXAMPLE, "encode_manifest writes F-P6's text byte for byte");
    }

    #[test]
    fn parse_and_encode_round_trip() {
        let m = parse_str(FIVE).expect("five components parse");
        assert_eq!(
            m.components,
            vec![
                comp(ComponentId::Dock, H_DOCK, 1_966_080, Stage::Stable),
                comp(ComponentId::Tasks, H_TASKS, 917_504, Stage::Beta),
                comp(ComponentId::TasksAdmin, H_ADMIN, 409_600, Stage::Beta),
                comp(ComponentId::Files, H_FILES, 1_048_576, Stage::Beta),
                comp(ComponentId::LangIt, H_LANG, 40_960, Stage::Stable),
            ]
        );
        assert_eq!(encode_manifest(&m), FIVE);
        // CRLF, tabs and extra spaces, another key order, components out of order, '\\' in the path, upper-case hex.
        let messy = format!(
            "{{\r\n\t\"components\" : [\r\n {},\r\n {},\r\n {},\r\n {},\r\n {} ],\r\n  \"version\":\"1.8.0\" ,  \"schema\" :1\r\n}}\r\n",
            r#"{"stage":"beta","size":917504,"sha256":"E3B0C44298FC1C149AFBF4C8996FB92427AE41E4649B934CA495991B7852B855","version":"1.8.0","file":"tasks.exe","id":"tasks"}"#,
            r#"{ "id" : "lang-it" , "file" : "lang\\it.lang" , "version" : "1.8.0" , "sha256" : "0123456789ABCDEF0123456789abcdef0123456789ABCDEF0123456789abcdef" , "size" : 40960 , "stage" : "stable" }"#,
            r#"{"file":"dock.exe","id":"dock","sha256":"BA7816BF8F01CFEA414140DE5DAE2223B00361A396177A9CB410FF61F20015AD","stage":"stable","size":1966080,"version":"1.8.0"}"#,
            r#"{"size":1048576,"stage":"beta","id":"files","version":"1.8.0","file":"files.exe","sha256":"ffeeddccbbaa99887766554433221100ffeeddccbbaa99887766554433221100"}"#,
            r#"{"id":"tasks-admin","sha256":"00112233445566778899aabbccddeeff00112233445566778899aabbccddeeff","file":"tasks-admin.exe","size":409600,"version":"1.8.0","stage":"beta"}"#,
        );
        assert_eq!(parse_str(&messy), Ok(m.clone()));
        assert_eq!(parse_str(&FIVE.replace('\n', "\r\n")), Ok(m.clone()), "CRLF line ends");
        // Built in another order, the encoder still writes ComponentId::ALL's.
        let mut shuffled = m.clone();
        shuffled.components.reverse();
        assert_eq!(encode_manifest(&shuffled), FIVE);
    }

    #[test]
    fn refusals() {
        let ok = |s: &str| parse_str(s).map(|m| m.components.iter().map(|c| c.id).collect::<Vec<_>>());
        assert_eq!(
            ok(&doc("1", "1.8.0", &[dock_line(), tasks_line()])),
            Ok(vec![ComponentId::Dock, ComponentId::Tasks])
        );
        // no dock
        assert!(parse_str(&doc("1", "1.8.0", &[tasks_line()])).is_err());
        assert!(parse_str(&doc("1", "1.8.0", &[])).is_err());
        // duplicate id
        assert!(parse_str(&doc("1", "1.8.0", &[dock_line(), tasks_line(), tasks_line()])).is_err());
        // a component version differing from the manifest version
        let older = line("tasks", "tasks.exe", "1.7.9", H_TASKS, "917504", "beta");
        assert!(parse_str(&doc("1", "1.8.0", &[dock_line(), older])).is_err());
        // sizes: 1..=COMPONENT_MAX_BYTES
        let sized =
            |size: &str| doc("1", "1.8.0", &[dock_line(), line("tasks", "tasks.exe", "1.8.0", H_TASKS, size, "beta")]);
        assert!(parse_str(&sized(&COMPONENT_MAX_BYTES.to_string())).is_ok());
        for bad in
            [(COMPONENT_MAX_BYTES + 1).to_string(), "0".into(), "-1".into(), "1.5".into(), "1e3".into(), "\"9\"".into()]
        {
            assert!(parse_str(&sized(&bad)).is_err(), "size {bad}");
        }
        assert!(parse_str(&sized("18446744073709551616")).is_err(), "above u64");
        // the hash: 64 hex digits, either case
        let bad_hashes = [
            H_TASKS[..63].to_string(),
            format!("{H_TASKS}0"),
            format!("g{}", &H_TASKS[1..]),
            String::new(),
            format!(" {}", &H_TASKS[1..]),
        ];
        for bad in &bad_hashes {
            let l = line("tasks", "tasks.exe", "1.8.0", bad, "917504", "beta");
            assert!(parse_str(&doc("1", "1.8.0", &[dock_line(), l])).is_err(), "hash {bad:?}");
        }
        // the file is the id's, with '/' or '\\'
        for (file, good) in [
            ("lang/it.lang", true),
            ("lang\\\\it.lang", true),
            ("lang/IT.lang", false),
            ("it.lang", false),
            ("tasks.exe", false),
        ] {
            let l = line("lang-it", file, "1.8.0", H_LANG, "40960", "stable");
            assert_eq!(parse_str(&doc("1", "1.8.0", &[dock_line(), l])).is_ok(), good, "file {file}");
        }
        let swapped = line("dock", "tasks.exe", "1.8.0", H_DOCK, "1966080", "stable");
        assert!(parse_str(&doc("1", "1.8.0", &[swapped])).is_err());
        // the stage
        assert!(parse_str(&doc(
            "1",
            "1.8.0",
            &[dock_line(), line("tasks", "tasks.exe", "1.8.0", H_TASKS, "917504", "alpha")]
        ))
        .is_err());
        // the version: canonical X.Y.Z
        for bad in ["v1.8.0", "1.08.0", "1.8", "1.8.0.1", " 1.8.0", ""] {
            let lines = [line("dock", "dock.exe", bad, H_DOCK, "1966080", "stable")];
            assert!(parse_str(&doc("1", bad, &lines)).is_err(), "version {bad:?}");
        }
        // the schema: an integer >= 1 that fits u16
        for bad in ["0", "-1", "1.0", "\"1\"", "65536", "null"] {
            assert!(parse_str(&doc(bad, "1.8.0", &[dock_line()])).is_err(), "schema {bad}");
        }
        // schema 1 knows every field; a schema above 1 is read with its unknown fields ignored
        let extra_top = |schema: &str| {
            format!("{{\"schema\": {schema}, \"version\": \"1.8.0\", \"channel\": {{\"x\": [1, 2]}}, \"components\": [{}]}}", dock_line())
        };
        let extra_comp = |schema: &str| {
            let l = dock_line().replace("\"stage\"", "\"signed\": true, \"stage\"");
            doc(schema, "1.8.0", &[l, tasks_line()])
        };
        assert!(parse_str(&extra_top("1")).is_err());
        assert!(parse_str(&extra_comp("1")).is_err());
        let two = parse_str(&extra_top("2")).expect("schema 2 with an unknown top-level field");
        assert_eq!((two.schema, two.components.len()), (2, 1));
        let two = parse_str(&extra_comp("2")).expect("schema 2 with an unknown component field");
        assert_eq!(two.components.len(), 2);
        // unknown ids are skipped, whatever they hold
        let future = r#"{"id": "paint", "file": "paint.exe", "version": "9.9.9", "size": 0}"#.to_string();
        assert_eq!(ok(&doc("1", "1.8.0", &[future.clone(), dock_line(), future])), Ok(vec![ComponentId::Dock]));
        // a missing field
        let no_size = dock_line().replace(", \"size\": 1966080", "");
        assert!(parse_str(&doc("1", "1.8.0", &[no_size])).is_err());
        assert!(parse_str(r#"{"schema": 1, "version": "1.8.0"}"#).is_err());
        // duplicate keys, at any level (serde_json alone would keep the last one)
        let dup_top = format!(
            "{{\"schema\": 1, \"version\": \"1.8.0\", \"version\": \"1.8.0\", \"components\": [{}]}}",
            dock_line()
        );
        assert!(parse_str(&dup_top).is_err());
        let dup_comp = dock_line().replace("\"size\": 1966080", "\"size\": 1, \"size\": 1966080");
        assert!(parse_str(&doc("1", "1.8.0", &[dup_comp])).is_err());
        let dup_deep = format!(
            "{{\"schema\": 2, \"version\": \"1.8.0\", \"x\": {{\"a\": 1, \"a\": 2}}, \"components\": [{}]}}",
            dock_line()
        );
        assert!(parse_str(&dup_deep).is_err());
        // not a JSON object, a BOM, trailing bytes
        for bad in ["", "[]", "null", "1", "\"x\""] {
            assert!(parse_str(bad).is_err(), "{bad:?}");
        }
        assert!(parse_manifest(&[&[0xEF, 0xBB, 0xBF][..], EXAMPLE.as_bytes()].concat()).is_err());
        assert!(parse_str(&format!("{EXAMPLE}x")).is_err());
        // the size cap: SUITE_MAX_BYTES with trailing spaces parses, one byte more does not
        let mut at_cap = EXAMPLE.as_bytes().to_vec();
        at_cap.resize(SUITE_MAX_BYTES as usize, b' ');
        assert!(parse_manifest(&at_cap).is_ok());
        at_cap.push(b' ');
        assert!(parse_manifest(&at_cap).is_err());
    }

    // ---------------------------------------------------------------- the convergence plan

    const OWN: [u8; 32] = [0x11; 32];
    const OTHER: [u8; 32] = [0x22; 32];

    fn own_of(ids: &[ComponentId]) -> SuiteManifest {
        SuiteManifest {
            schema: SUITE_SCHEMA,
            version: v("1.8.0"),
            components: ids
                .iter()
                .map(|&id| Component { id, version: v("1.8.0"), sha256: OWN, size: 1, stage: Stage::Beta })
                .collect(),
        }
    }

    #[test]
    fn plan_truth_table() {
        let states = [None, Some(OWN), Some(OTHER)];
        let mut rows = 0;
        for id in ComponentId::ALL.into_iter().filter(|&id| id != ComponentId::Dock) {
            let own = own_of(&[ComponentId::Dock, id]);
            for current in states {
                for prev in states {
                    for staged in states {
                        for is_bad in [false, true] {
                            for network in [false, true] {
                                let disk = [OnDisk { id, current, prev, staged }];
                                let bad: Vec<(ComponentId, [u8; 32])> =
                                    if is_bad { vec![(id, OWN)] } else { vec![(id, OTHER)] };
                                // F-P6's rule, row by row.
                                let fetch =
                                    |s: Source| if current.is_some() { Step::Replace(id, s) } else { Step::Add(id, s) };
                                let want = if current == Some(OWN) {
                                    Step::Keep(id)
                                } else if is_bad {
                                    Step::Skip(id)
                                } else if prev == Some(OWN) {
                                    fetch(Source::Prev)
                                } else if staged == Some(OWN) {
                                    fetch(Source::Stage)
                                } else if network {
                                    fetch(Source::Download)
                                } else {
                                    Step::Skip(id)
                                };
                                let got = plan_convergence(&own, &disk, &bad, network);
                                assert_eq!(got, vec![want], "{id:?} current {current:?} prev {prev:?} staged {staged:?} bad {is_bad} network {network}");
                                rows += 1;
                            }
                        }
                    }
                }
            }
        }
        assert_eq!(rows, 4 * 27 * 4);
        // Spot rows, written out.
        let own = own_of(&[ComponentId::Dock, ComponentId::Tasks]);
        let one = |current, prev, staged, network| {
            plan_convergence(&own, &[OnDisk { id: ComponentId::Tasks, current, prev, staged }], &[], network)
        };
        assert_eq!(one(None, None, Some(OWN), false), vec![Step::Add(ComponentId::Tasks, Source::Stage)]);
        assert_eq!(one(Some(OTHER), Some(OWN), Some(OWN), true), vec![Step::Replace(ComponentId::Tasks, Source::Prev)]);
        assert_eq!(one(Some(OTHER), None, None, true), vec![Step::Replace(ComponentId::Tasks, Source::Download)]);
        assert_eq!(one(None, None, None, false), vec![Step::Skip(ComponentId::Tasks)]);
        // Nothing on disk at all: the component is missing.
        assert_eq!(plan_convergence(&own, &[], &[], true), vec![Step::Add(ComponentId::Tasks, Source::Download)]);
        // A component on disk but absent from own is kept; one absent from both gets no step.
        let disk = [
            OnDisk { id: ComponentId::Files, current: Some(OTHER), prev: None, staged: None },
            OnDisk { id: ComponentId::LangIt, current: None, prev: Some(OTHER), staged: Some(OTHER) },
        ];
        assert_eq!(
            plan_convergence(&own, &disk, &[], true),
            vec![Step::Add(ComponentId::Tasks, Source::Download), Step::Keep(ComponentId::Files)]
        );
        // Steps come in ComponentId::ALL order.
        let all = own_of(&ComponentId::ALL);
        let steps = plan_convergence(&all, &[], &[], true);
        let ids: Vec<ComponentId> = steps
            .iter()
            .map(|s| match *s {
                Step::Keep(id) | Step::Add(id, _) | Step::Replace(id, _) | Step::Skip(id) => id,
            })
            .collect();
        assert_eq!(ids, vec![ComponentId::Tasks, ComponentId::TasksAdmin, ComponentId::Files, ComponentId::LangIt]);
    }

    fn touches_dock(steps: &[Step]) -> bool {
        steps.iter().any(|s| {
            matches!(
                *s,
                Step::Keep(ComponentId::Dock)
                    | Step::Add(ComponentId::Dock, _)
                    | Step::Replace(ComponentId::Dock, _)
                    | Step::Skip(ComponentId::Dock)
            )
        })
    }

    #[test]
    fn dock_is_never_a_step() {
        let states = [None, Some(OWN), Some(OTHER)];
        for own in [own_of(&[ComponentId::Dock]), own_of(&ComponentId::ALL), own_of(&[ComponentId::Tasks])] {
            for current in states {
                for prev in states {
                    for staged in states {
                        for network in [false, true] {
                            let disk = [OnDisk { id: ComponentId::Dock, current, prev, staged }];
                            for bad in [&[][..], &[(ComponentId::Dock, OWN)][..]] {
                                assert!(!touches_dock(&plan_convergence(&own, &disk, bad, network)));
                            }
                        }
                    }
                }
            }
        }
    }

    // ---------------------------------------------------------------- fuzz

    /// A small seeded generator (xorshift64*), so the random tests repeat.
    struct Rng(u64);
    impl Rng {
        fn next(&mut self) -> u64 {
            self.0 ^= self.0 >> 12;
            self.0 ^= self.0 << 25;
            self.0 ^= self.0 >> 27;
            self.0.wrapping_mul(0x2545_F491_4F6C_DD1D)
        }
        fn below(&mut self, n: usize) -> usize {
            (self.next() % n.max(1) as u64) as usize
        }
    }

    /// Whatever the bytes: no panic; a manifest that parses encodes to a text that parses back to it, and its plan
    /// never steps the dock.
    fn exercise(bytes: &[u8]) {
        if let Ok(m) = parse_manifest(bytes) {
            let text = encode_manifest(&m);
            assert_eq!(parse_manifest(text.as_bytes()).as_ref(), Ok(&m), "{text}");
            assert!(!touches_dock(&plan_convergence(&m, &[], &[], true)));
        }
    }

    const TOKENS: [&[u8]; 16] = [
        b"{",
        b"}",
        b"[",
        b"]",
        b",",
        b":",
        b"\"",
        b"\\",
        b"\"id\"",
        b"\"dock\"",
        b"\"lang-it\"",
        b"1.8.0",
        b"\"schema\"",
        b"-1",
        b"1e999",
        b"\"size\": 18446744073709551615",
    ];

    fn fuzz(rounds: u32, seed: u64) {
        let mut r = Rng(seed);
        let seeds = [EXAMPLE.as_bytes(), FIVE.as_bytes()];
        for i in 0..rounds {
            let mut v: Vec<u8> = if i % 4 == 0 {
                (0..r.below(300)).map(|_| r.next() as u8).collect()
            } else {
                seeds[r.below(seeds.len())].to_vec()
            };
            if i % 4 != 0 {
                for _ in 0..1 + r.below(6) {
                    if v.is_empty() {
                        break;
                    }
                    let at = r.below(v.len());
                    match r.below(5) {
                        0 => v[at] = r.next() as u8,
                        1 => v.truncate(at),
                        2 => {
                            v.remove(at);
                        }
                        3 => {
                            let t = TOKENS[r.below(TOKENS.len())];
                            v.splice(at..at, t.iter().copied());
                        }
                        _ => {
                            let end = (at + 1 + r.below(40)).min(v.len());
                            let piece = v[at..end].to_vec();
                            let to = r.below(v.len());
                            v.splice(to..to, piece);
                        }
                    }
                }
            }
            exercise(&v);
        }
    }

    #[test]
    fn fuzz_suite_10k() {
        fuzz(10_000, 0x5017_E0A1);
        // deep nesting, huge numbers, the size limit
        exercise("[".repeat(60_000).as_bytes());
        exercise(format!("{{\"a\":{}", "{\"a\":".repeat(5_000)).as_bytes());
        exercise(br#"{"schema": 1e400, "version": "1.8.0", "components": []}"#);
        exercise(br#"{"schema": 18446744073709551616, "version": "99999.99999.99999", "components": []}"#);
        exercise(&vec![b' '; SUITE_MAX_BYTES as usize + 1]);
        exercise(&vec![b'['; SUITE_MAX_BYTES as usize]);
    }

    #[test]
    #[ignore = "1,000,000 inputs: run with cargo test --release -- --ignored fuzz_"]
    fn fuzz_suite_1m() {
        fuzz(1_000_000, 0xFEED_5017);
    }
}
