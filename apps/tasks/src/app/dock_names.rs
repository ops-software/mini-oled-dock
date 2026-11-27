//! The dock's renames: the "names" object of <root>\config.json, read once at start, read-only and capped (open point
//! 13). Never written.

use std::collections::BTreeMap;
use std::path::Path;

/// The file is read through the capped reader: a larger one is refused.
pub const MAX_FILE: u64 = 1 << 20;
/// Most entries kept; the first ones in key order.
pub const MAX_ENTRIES: usize = 256;
/// Longest key and value, in characters.
pub const MAX_KEY_CHARS: usize = 260;
pub const MAX_VALUE_CHARS: usize = 64;

/// The names of the file's bytes: only the "names" object, with comments allowed; keys and values trimmed, a key longer
/// than 260 or a value longer than 64 characters, an empty one and a value that is not a string are ignored, and at
/// most 256 entries are kept (in key order). Anything else, or damage, gives an empty map.
pub fn parse(bytes: &[u8]) -> BTreeMap<String, String> {
    let mut out = BTreeMap::new();
    let Ok(text) = std::str::from_utf8(bytes) else { return out };
    let text = text.strip_prefix('\u{feff}').unwrap_or(text);
    let Ok(root) = serde_json::from_str::<serde_json::Value>(&oled_config::jsonc::strip_comments(text)) else {
        return out;
    };
    let Some(names) = root.get("names").and_then(serde_json::Value::as_object) else { return out };
    for (k, v) in names {
        if out.len() >= MAX_ENTRIES {
            break;
        }
        let (Some(v), k) = (v.as_str(), k.trim()) else { continue };
        let v = v.trim();
        if k.is_empty() || v.is_empty() || k.chars().count() > MAX_KEY_CHARS || v.chars().count() > MAX_VALUE_CHARS {
            continue;
        }
        out.insert(k.to_string(), v.to_string());
    }
    out
}

/// <root>\config.json through the capped reader (no reparse point, a single link, at most 1 MiB). Empty when there is no
/// root or no usable file.
pub fn load(root: Option<&Path>) -> BTreeMap<String, String> {
    let Some(root) = root else { return BTreeMap::new() };
    match oled_sys::datadir::read_file(&root.join("config.json"), MAX_FILE, true) {
        Ok(bytes) => parse(&bytes),
        Err(_) => BTreeMap::new(),
    }
}
