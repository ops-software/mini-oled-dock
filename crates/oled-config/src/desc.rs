//! Option descriptors (ROADMAP X6: the union of the dock's and the apps' descriptors), their lint, and the JSON kind
//! of a field type.

pub type TextFn = fn() -> &'static str; // a localized string, from whatever i18n form the caller uses
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ValueType {
    Bool,
    Int,
    Str,
    Enum,
    Color,
    List,
    Map,
    Object,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Unit {
    None,
    Px,
    Percent,
    Ms,
    Seconds,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Range {
    pub min: i64,
    pub max: i64,
    pub step: i64,
    pub unit: Unit,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PageId(pub &'static str);
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SecId(pub &'static str);
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Scope {
    Global,
    PerBar,
    PerModule,
    PerApp,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Takeover {
    CtrlShiftEsc,
    WinE,
    FolderVerbs,
}
#[derive(Clone, Copy, Debug)]
pub struct Cond {
    pub json: &'static str,
    pub equals: &'static str,
}
#[derive(Clone, Copy, Debug)]
pub struct CostNote {
    pub kib: u16,
    pub wakeups_milli_per_s: u16,
}
#[derive(Clone, Copy, Debug)]
pub enum Control {
    Toggle,
    Segmented(&'static [(TextFn, &'static str)]),
    Slider,
    Swatches,
    Select(&'static [(TextFn, &'static str)]),
    Text { max: u16 },
    ChordPicker,
    ActionPicker,
    List,
    Custom(&'static str),
}
#[derive(Clone, Copy, Debug)]
pub struct Descriptor {
    // ROADMAP X6: the union of P's Descriptor and D's OptDesc
    pub path: &'static str,
    pub json: &'static str,
    pub kind: ValueType,
    pub default: fn() -> serde_json::Value,
    pub range: Option<Range>,
    pub label: TextFn,
    pub help: TextFn,
    pub glyph: &'static str,
    pub page: PageId,
    pub section: SecId,
    pub order: u16,
    pub keywords: &'static [TextFn],
    pub scope: Scope,
    pub profileable: bool,
    pub visible_when: Option<Cond>,
    pub cost: Option<CostNote>,
    pub control: Control,
    pub since: u16,
    pub takeover: Option<Takeover>,
    pub reason: &'static str,
}
/// D's lints over a descriptor table, one English line per finding, in a fixed order: per row (label, help, reason,
/// default), then per page the glyphs used twice, then the (page, section) pairs with more than 12 rows, then the
/// json paths used twice. `it` runs a TextFn with Italian on. An empty glyph is no glyph and is never a duplicate.
pub fn lint(descs: &[Descriptor], it: &dyn Fn(TextFn) -> &'static str) -> Vec<String> {
    let mut out = Vec::new();
    for d in descs {
        let j = d.json;
        if (d.label)().is_empty() {
            out.push(format!("{j}: empty label (en)"));
        }
        if it(d.label).is_empty() {
            out.push(format!("{j}: empty label (it)"));
        }
        if (d.help)().is_empty() {
            out.push(format!("{j}: empty help (en)"));
        }
        if it(d.help).is_empty() {
            out.push(format!("{j}: empty help (it)"));
        }
        if d.reason.is_empty() {
            out.push(format!("{j}: empty reason"));
        }
        if let Some(r) = d.range {
            let v = (d.default)();
            let inside = match (v.as_i64(), v.as_f64()) {
                (Some(n), _) => (r.min..=r.max).contains(&n),
                (None, Some(f)) => f >= r.min as f64 && f <= r.max as f64,
                (None, None) => false,
            };
            if !inside {
                out.push(format!("{j}: default {v} outside {}..={}", r.min, r.max));
            }
        }
    }
    // Glyphs used twice on one page, each (page, glyph) once, in the order of their first row.
    for (i, d) in descs.iter().enumerate() {
        let same = |e: &&Descriptor| e.page == d.page && e.glyph == d.glyph;
        if d.glyph.is_empty() || descs.iter().take(i).any(|e| same(&e)) {
            continue;
        }
        let users: Vec<&str> = descs.iter().filter(same).map(|e| e.json).collect();
        if users.len() > 1 {
            out.push(format!("{}: glyph {} used by {}", d.page.0, d.glyph, users.join(", ")));
        }
    }
    for (i, d) in descs.iter().enumerate() {
        let same = |e: &&Descriptor| e.page == d.page && e.section == d.section;
        if descs.iter().take(i).any(|e| same(&e)) {
            continue;
        }
        let n = descs.iter().filter(same).count();
        if n > MAX_ROWS_PER_SECTION {
            out.push(format!("{}/{}: {n} rows (at most {MAX_ROWS_PER_SECTION})", d.page.0, d.section.0));
        }
    }
    for (i, d) in descs.iter().enumerate() {
        if descs.iter().take(i).any(|e| e.json == d.json) {
            continue;
        }
        let n = descs.iter().filter(|e| e.json == d.json).count();
        if n > 1 {
            out.push(format!("{}: json path used {n} times", d.json));
        }
    }
    out
}

/// D's limit of rows in one section of a page.
const MAX_ROWS_PER_SECTION: usize = 12;

/// The JSON kind of a field type: `options!` reads it for `Descriptor::kind` of every field shown in Settings. A
/// config's own enums and sections implement it in one line (`impl Kind for Position { const KIND: ValueType =
/// ValueType::Enum; }`).
pub trait Kind {
    const KIND: ValueType;
}

macro_rules! kind {
    ($k:ident: $($t:ty),*) => { $(impl Kind for $t { const KIND: ValueType = ValueType::$k; })* };
}
kind!(Bool: bool);
kind!(Int: u8, u16, u32, u64, usize, i8, i16, i32, i64, isize);
kind!(Str: String, &'static str);
impl<T> Kind for Vec<T> {
    const KIND: ValueType = ValueType::List;
}
impl<T, const N: usize> Kind for [T; N] {
    const KIND: ValueType = ValueType::List;
}
impl<K, V> Kind for std::collections::BTreeMap<K, V> {
    const KIND: ValueType = ValueType::Map;
}
impl<K, V, S> Kind for std::collections::HashMap<K, V, S> {
    const KIND: ValueType = ValueType::Map;
}
impl<T: Kind> Kind for Option<T> {
    const KIND: ValueType = T::KIND;
}

/// The runtime of the `options!` expansion, reachable at the crate root as `__options`. Not an API.
#[doc(hidden)]
pub use crate::options::rt as __options;
