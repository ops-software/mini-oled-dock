//! The `options!` macro: one declaration gives a config struct, its defaults, its top-level keys, its descriptors,
//! its commented template and its reader (`Options`), in the grammar of F-P1.
//!
//! Fields are written in **declaration order**: the generated struct's field order, which serde follows when it
//! serializes. Each field names its **slot**, its place in the written template: the template writes the fields sorted
//! by slot, so the file's order and the struct's order are independent. Per field, in this order: `json`; `slot`
//! (unique; gaps allowed; a duplicate fails to compile); `fmt` (`Raw` = Display, `Js` = compact JSON, `Block` =
//! pretty JSON indented under its key, `Str(fn(&T) -> &'static str)` = that text in quotes, `Custom(fn(&Self) ->
//! String)` = the exact text after the key, for whatever the others cannot write); `gap` (a blank line before it);
//! `guide` (comment lines, split on `\n`, each written as `  // <line>`; `""` writes none); `since`; `reason`; `ui`
//! (`None` for an internal key, else `Some { page, section, order, control, label, help, glyph, keywords, range,
//! scope, profileable, visible_when, cost, takeover }`, each an expression of the `Descriptor` field of that name).
//! The struct line carries `file`, `version`, `mark`, `header` (the text above the opening brace, starting with
//! the mark, checked at compile time) and, optionally, `migrations` (`Options::migrations`, index = source version).
//!
//! The macro generates the struct (deriving Clone, Debug, PartialEq, serde's Serialize and Deserialize, with
//! `#[serde(default)]` and a last field `extra` that keeps the unknown keys), `Default` from the per-field defaults,
//! and `Options`: `known_keys` in declaration order, `descriptors` (the fields with `ui: Some`, in declaration
//! order; a field's `Descriptor::kind` is its type's `Kind`), `to_template` (the header, then the fields by slot,
//! the last one without a comma, then the unknown keys as `jsonc::render_extra` writes them), and `from_value`, which
//! reads the whole file or nothing (one wrong type is one `Problem::Parse` error with its line and column; the
//! defaults or the last good value are kept), as the dock's config.rs 1.5.2 read its file. A field's default must
//! not name `Self`.
//!
//! ```
//! use oled_config::*;
//!
//! options! {
//!     /// A sample file.
//!     #[derive(Eq)]
//!     pub struct Sample(file: "sample.json", version: 1, mark: "// sample", header: "// sample\n// Edit and save.") {
//!         /// Whether the sample runs.
//!         pub enabled: bool = true => {
//!             json: "enabled", slot: 0, fmt: Raw, gap: false, guide: "Turns the sample on.", since: 1,
//!             reason: "The one switch.", ui: None,
//!         },
//!     }
//! }
//! assert_eq!(Sample::default().to_template(), "// sample\n// Edit and save.\n{\n  // Turns the sample on.\n  \"enabled\": true\n}\n");
//! ```
//!
//! The keys of a field are in a fixed order; another order does not compile:
//!
//! ```compile_fail
//! use oled_config::*;
//!
//! options! {
//!     pub struct Bad(file: "bad.json", version: 1, mark: "// bad", header: "// bad") {
//!         pub a: bool = true => {
//!             slot: 0, json: "a", fmt: Raw, gap: false, guide: "", since: 1, reason: "r", ui: None,
//!         },
//!     }
//! }
//! ```
//!
//! Nor do two fields with one slot:
//!
//! ```compile_fail
//! use oled_config::*;
//!
//! options! {
//!     pub struct Bad(file: "bad.json", version: 1, mark: "// bad", header: "// bad") {
//!         pub a: bool = true => { json: "a", slot: 0, fmt: Raw, gap: false, guide: "", since: 1, reason: "r", ui: None },
//!         pub b: bool = true => { json: "b", slot: 0, fmt: Raw, gap: false, guide: "", since: 1, reason: "r", ui: None },
//!     }
//! }
//! ```

/// Declares a config once: struct, defaults, keys, descriptors, template (see the module documentation).
#[macro_export]
macro_rules! options {
    // ---- internal rules first: a declaration never starts with `@`
    (@migrations) => { &[] };
    (@migrations $m:expr) => { $m };
    (@fmt [$v:expr] [$s:expr] $ty:ty, Raw) => { $crate::__options::raw(&$v) };
    (@fmt [$v:expr] [$s:expr] $ty:ty, Js) => { $crate::jsonc::js(&$v) };
    (@fmt [$v:expr] [$s:expr] $ty:ty, Block) => { $crate::jsonc::js_block(&$v) };
    (@fmt [$v:expr] [$s:expr] $ty:ty, Str($f:expr)) => {{
        let f: fn(&$ty) -> &'static str = $f;
        $crate::__options::quoted(f(&$v))
    }};
    (@fmt [$v:expr] [$s:expr] $ty:ty, Custom($f:expr)) => {{
        let f: fn(&Self) -> ::std::string::String = $f;
        f($s)
    }};
    (@desc [$field:ident $ty:ty] [$default:expr] [$json:expr] [$since:expr] [$reason:expr] None) => {
        ::core::option::Option::None
    };
    (@desc [$field:ident $ty:ty] [$default:expr] [$json:expr] [$since:expr] [$reason:expr] Some {
        page: $page:expr,
        section: $section:expr,
        order: $order:expr,
        control: $control:expr,
        label: $label:expr,
        help: $help:expr,
        glyph: $glyph:expr,
        keywords: $keywords:expr,
        range: $range:expr,
        scope: $scope:expr,
        profileable: $profileable:expr,
        visible_when: $visible_when:expr,
        cost: $cost:expr,
        takeover: $takeover:expr $(,)?
    }) => {
        ::core::option::Option::Some($crate::Descriptor {
            path: ::core::stringify!($field),
            json: $json,
            kind: <$ty as $crate::Kind>::KIND,
            default: || $crate::__options::to_value::<$ty>(&$default),
            range: $range,
            label: $label,
            help: $help,
            glyph: $glyph,
            page: $page,
            section: $section,
            order: $order,
            keywords: $keywords,
            scope: $scope,
            profileable: $profileable,
            visible_when: $visible_when,
            cost: $cost,
            control: $control,
            since: $since,
            takeover: $takeover,
            reason: $reason,
        })
    };
    // ---- a declaration
    (
        $(#[$sattr:meta])*
        $svis:vis struct $name:ident (
            file: $file:expr,
            version: $version:expr,
            mark: $mark:expr,
            header: $header:expr
            $(, migrations: $migrations:expr)? $(,)?
        ) {
            $(
                $(#[$fattr:meta])*
                $fvis:vis $field:ident : $ty:ty = $default:expr => {
                    json: $json:expr,
                    slot: $slot:expr,
                    fmt: $fmt:ident $(($fmtfn:expr))?,
                    gap: $gap:expr,
                    guide: $guide:expr,
                    since: $since:expr,
                    reason: $reason:expr,
                    ui: $ui:ident $({ $($uibody:tt)* })? $(,)?
                }
            ),* $(,)?
        }
    ) => {
        $(#[$sattr])*
        #[derive(Clone, Debug, PartialEq, ::serde::Serialize, ::serde::Deserialize)]
        #[serde(default)]
        $svis struct $name {
            $(
                $(#[$fattr])*
                $fvis $field: $ty,
            )*
            /// Top-level keys this version does not know, kept as read and written last (never shown in Settings).
            #[serde(skip)]
            pub extra: $crate::__options::Map,
        }

        impl ::core::default::Default for $name {
            fn default() -> Self {
                Self { $($field: $default,)* extra: $crate::__options::Map::new() }
            }
        }

        const _: () = {
            ::core::assert!(
                $crate::__options::slots_unique(&[$($slot),*]),
                ::core::concat!("options!: two fields of ", ::core::stringify!($name), " share a slot")
            );
            ::core::assert!(
                $crate::__options::starts_with($header, $mark),
                ::core::concat!("options!: the header of ", ::core::stringify!($name), " does not start with its mark")
            );
        };

        impl $crate::Options for $name {
            const VERSION: u16 = $version;
            const FILE: &'static str = $file;

            fn descriptors() -> &'static [$crate::Descriptor] {
                const ALL: &[::core::option::Option<$crate::Descriptor>] = &[$(
                    $crate::options!(@desc [$field $ty] [$default] [$json] [$since] [$reason] $ui $({ $($uibody)* })?)
                ),*];
                const N: usize = $crate::__options::count_some(ALL);
                const DESCS: &[$crate::Descriptor] = &$crate::__options::compact::<N>(ALL);
                DESCS
            }

            fn known_keys() -> &'static [&'static str] {
                &[$($json),*]
            }

            fn from_value(
                v: &$crate::__options::Value,
                text: &str,
                last_good: ::core::option::Option<&Self>,
            ) -> (Self, ::std::vec::Vec<$crate::ConfigError>) {
                match $crate::__options::whole::<Self>(v, text) {
                    ::core::result::Result::Ok(mut c) => {
                        c.extra = $crate::__options::unknown_keys(v, <Self as $crate::Options>::known_keys());
                        (c, ::std::vec::Vec::new())
                    }
                    ::core::result::Result::Err(e) => $crate::__options::kept(e, last_good),
                }
            }

            fn to_template(&self) -> ::std::string::String {
                let mut fields = [$(
                    $crate::__options::Field {
                        slot: $slot,
                        gap: $gap,
                        guide: $guide,
                        json: $json,
                        value: $crate::options!(@fmt [self.$field] [self] $ty, $fmt $(($fmtfn))?),
                    }
                ),*];
                $crate::__options::template($header, &mut fields, &self.extra)
            }

            fn template_mark() -> &'static str {
                $mark
            }

            fn migrations(
            ) -> &'static [fn(&mut $crate::__options::Value) -> ::core::result::Result<(), $crate::MigrateError>] {
                $crate::options!(@migrations $($migrations)?)
            }
        }
    };
}

/// The runtime of the `options!` expansion (`$crate::__options`). Not an API: it changes with the macro.
pub mod rt {
    use serde::de::DeserializeOwned;
    use serde::Serialize;

    use crate::desc::{Control, Descriptor, PageId, Scope, SecId, ValueType};
    use crate::error::{ConfigError, Kept};
    use crate::jsonc;

    pub use serde_json::Value;

    /// The unknown keys of a file.
    pub type Map = serde_json::Map<String, Value>;

    /// One field of a template, its value already written.
    pub struct Field {
        pub slot: u16,
        pub gap: bool,
        pub guide: &'static str,
        pub json: &'static str,
        pub value: String,
    }

    /// The whole file: the header, an opening brace, the fields by slot (each with its gap and guide lines, all but
    /// the last with a comma), the unknown keys, a closing brace and a final newline.
    pub fn template(header: &str, fields: &mut [Field], extra: &Map) -> String {
        fields.sort_unstable_by_key(|f| f.slot);
        let mut out = String::with_capacity(header.len() + 64 * fields.len() + 8);
        out.push_str(header);
        if !header.ends_with('\n') {
            out.push('\n');
        }
        out.push_str("{\n");
        let last = fields.len().saturating_sub(1);
        for (i, f) in fields.iter().enumerate() {
            if f.gap {
                out.push('\n');
            }
            if !f.guide.is_empty() {
                for line in f.guide.split('\n') {
                    out.push_str("  //");
                    if !line.is_empty() {
                        out.push(' ');
                        out.push_str(line);
                    }
                    out.push('\n');
                }
            }
            out.push_str("  ");
            out.push_str(&jsonc::js(f.json));
            out.push_str(": ");
            out.push_str(&f.value);
            if i < last {
                out.push_str(",\n");
            }
        }
        out.push_str(&jsonc::render_extra(extra));
        out.push_str("\n}\n");
        out
    }

    /// `Raw`: the value as Display writes it.
    pub fn raw<T: std::fmt::Display + ?Sized>(v: &T) -> String {
        v.to_string()
    }

    /// `Str`: the text in quotes, as written (the strings of an enum need no escape).
    pub fn quoted(s: &str) -> String {
        format!("\"{s}\"")
    }

    /// A field's default as JSON, for `Descriptor::default`.
    pub fn to_value<T: Serialize>(v: &T) -> Value {
        serde_json::to_value(v).unwrap_or(Value::Null)
    }

    /// The whole struct from the file's value. On a wrong type, the error of the text itself, which carries the line
    /// and column (the value has none), when the text gives one.
    pub fn whole<C: DeserializeOwned>(v: &Value, text: &str) -> Result<C, ConfigError> {
        C::deserialize(v).map_err(|e| {
            let positioned = serde_json::from_str::<C>(&jsonc::strip_comments(jsonc::strip_bom(text))).err();
            ConfigError::from_serde(positioned.as_ref().unwrap_or(&e), Kept::Default)
        })
    }

    /// The top-level keys of `v` that are not in `known`, with their values.
    pub fn unknown_keys(v: &Value, known: &[&str]) -> Map {
        match v {
            Value::Object(all) => {
                all.iter().filter(|(k, _)| !known.contains(&k.as_str())).map(|(k, x)| (k.clone(), x.clone())).collect()
            }
            _ => Map::new(),
        }
    }

    /// A whole-file error: the last good value (or the defaults) is kept, and the error says which.
    pub fn kept<C: Clone + Default>(mut e: ConfigError, last_good: Option<&C>) -> (C, Vec<ConfigError>) {
        e.kept = if last_good.is_some() { Kept::LastGood } else { Kept::Default };
        (last_good.cloned().unwrap_or_default(), vec![e])
    }

    /// true when no two slots are equal (a compile-time check of every declaration).
    pub const fn slots_unique(slots: &[u16]) -> bool {
        let mut i = 0;
        while i < slots.len() {
            let mut j = i + 1;
            while j < slots.len() {
                if slots[i] == slots[j] {
                    return false;
                }
                j += 1;
            }
            i += 1;
        }
        true
    }

    /// `str::starts_with` at compile time.
    pub const fn starts_with(s: &str, prefix: &str) -> bool {
        let (s, p) = (s.as_bytes(), prefix.as_bytes());
        if p.len() > s.len() {
            return false;
        }
        let mut i = 0;
        while i < p.len() {
            if s[i] != p[i] {
                return false;
            }
            i += 1;
        }
        true
    }

    /// How many fields have a descriptor.
    pub const fn count_some(all: &[Option<Descriptor>]) -> usize {
        let (mut i, mut n) = (0, 0);
        while i < all.len() {
            if all[i].is_some() {
                n += 1;
            }
            i += 1;
        }
        n
    }

    /// The descriptors of the fields that have one, in declaration order (`N` = `count_some(all)`).
    pub const fn compact<const N: usize>(all: &[Option<Descriptor>]) -> [Descriptor; N] {
        let mut out = [PLACEHOLDER; N];
        let (mut i, mut j) = (0, 0);
        while i < all.len() {
            if let Some(d) = all[i] {
                if j < N {
                    out[j] = d;
                }
                j += 1;
            }
            i += 1;
        }
        assert!(j == N, "options!: the descriptor count is wrong");
        out
    }

    fn null() -> Value {
        Value::Null
    }

    fn empty() -> &'static str {
        ""
    }

    /// What `compact` fills its array with before it copies the descriptors in.
    const PLACEHOLDER: Descriptor = Descriptor {
        path: "",
        json: "",
        kind: ValueType::Object,
        default: null,
        range: None,
        label: empty,
        help: empty,
        glyph: "",
        page: PageId(""),
        section: SecId(""),
        order: 0,
        keywords: &[],
        scope: Scope::Global,
        profileable: false,
        visible_when: None,
        cost: None,
        control: Control::Custom(""),
        since: 0,
        takeover: None,
        reason: "",
    };
}
