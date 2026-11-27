//! The it.lang generator: the same bytes as `oled_sys::lang::encode` (this crate is std only, so it repeats the
//! 24-byte header writer instead of depending on oled-sys; a test pins the two against one fixture).

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use crate::{check, fnv1a64, parse};

const MAX_BYTES: usize = 1024 * 1024;
const MAX_KEYS: usize = 16_384;

/// Reads `<dir>/en.toml` and `<dir>/<tag>.toml` of every folder (the same checks as `generate`: no key on one side
/// only, the same {placeholders}), refuses a key found in two folders, and writes the file: entries sorted by hash.
pub fn build(dirs: &[PathBuf], tag: &str, version: (u16, u16, u16)) -> Result<Vec<u8>, String> {
    let t = tag.as_bytes();
    if t.len() != 2 || !t.iter().all(u8::is_ascii_lowercase) {
        return Err(format!("the tag \"{tag}\" is not two lowercase ASCII letters"));
    }
    let mut seen: HashMap<String, PathBuf> = HashMap::new();
    let mut rows: Vec<(u64, String)> = Vec::new();
    for dir in dirs {
        let read = |name: String| -> Result<Vec<(String, String)>, String> {
            let path = dir.join(&name);
            let text = std::fs::read_to_string(&path).map_err(|e| format!("{}: {e}", path.display()))?;
            parse(&text).map_err(|e| format!("{}:{e}", path.display()))
        };
        let en = read("en.toml".to_string())?;
        let own = read(format!("{tag}.toml"))?;
        check(&en, &own).map_err(|e| format!("{}: {e}", dir.display()))?;
        for (k, v) in own {
            if let Some(first) = seen.insert(k.clone(), dir.clone()) {
                return Err(format!(
                    "key \"{k}\" is in two strings folders: {} and {}",
                    first.display(),
                    dir.display()
                ));
            }
            rows.push((fnv1a64(&k), v));
        }
    }
    rows.sort_by_key(|r| r.0);
    if let Some(w) = rows.windows(2).find(|w| w[0].0 == w[1].0) {
        return Err(format!("two keys have the hash {:#018x} (texts \"{}\" and \"{}\")", w[0].0, w[0].1, w[1].1));
    }
    if rows.len() > MAX_KEYS {
        return Err(format!("{} keys, at most {MAX_KEYS}", rows.len()));
    }
    let blob_len: usize = rows.iter().map(|r| r.1.len()).sum();
    let total = 24 + rows.len() * 16 + blob_len;
    if total > MAX_BYTES {
        return Err(format!("{total} bytes, at most {MAX_BYTES}"));
    }
    let mut out = Vec::with_capacity(total);
    out.extend_from_slice(b"OLNG");
    out.extend_from_slice(&1u16.to_le_bytes());
    out.extend_from_slice(&0u16.to_le_bytes());
    out.extend_from_slice(&(rows.len() as u32).to_le_bytes());
    out.extend_from_slice(&(blob_len as u32).to_le_bytes());
    out.extend_from_slice(t);
    for v in [version.0, version.1, version.2] {
        out.extend_from_slice(&v.to_le_bytes());
    }
    let mut off = 0u32;
    for (h, s) in &rows {
        out.extend_from_slice(&h.to_le_bytes());
        out.extend_from_slice(&off.to_le_bytes());
        out.extend_from_slice(&(s.len() as u32).to_le_bytes());
        off += s.len() as u32;
    }
    for (_, s) in &rows {
        out.extend_from_slice(s.as_bytes());
    }
    Ok(out)
}

/// The strings folders under a workspace root: `<root>/strings`, `<root>/crates/*/strings`, `<root>/apps/*/strings`
/// (only those that exist), in a fixed order.
pub fn find_dirs(root: &Path) -> Result<Vec<PathBuf>, String> {
    let mut out = Vec::new();
    let top = root.join("strings");
    if top.is_dir() {
        out.push(top);
    }
    for group in ["crates", "apps"] {
        let g = root.join(group);
        if !g.is_dir() {
            continue;
        }
        let mut subs: Vec<PathBuf> = std::fs::read_dir(&g)
            .map_err(|e| format!("{}: {e}", g.display()))?
            .filter_map(|e| e.ok().map(|e| e.path().join("strings")))
            .filter(|p| p.is_dir())
            .collect();
        subs.sort();
        out.extend(subs);
    }
    Ok(out)
}

/// "1.8.0" -> (1, 8, 0).
pub fn parse_version(s: &str) -> Result<(u16, u16, u16), String> {
    let bad = || format!("the version \"{s}\" is not major.minor.patch");
    let mut it = s.split('.');
    let mut next = || it.next().and_then(|p| p.parse::<u16>().ok()).ok_or_else(bad);
    let v = (next()?, next()?, next()?);
    if s.split('.').count() != 3 {
        return Err(bad());
    }
    Ok(v)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU32, Ordering};

    /// A temp directory removed on drop.
    struct Dir(PathBuf);
    impl Dir {
        fn new() -> Dir {
            static N: AtomicU32 = AtomicU32::new(0);
            let p = std::env::temp_dir().join(format!(
                "oled-lang-{}-{}",
                std::process::id(),
                N.fetch_add(1, Ordering::Relaxed)
            ));
            std::fs::create_dir_all(&p).unwrap();
            Dir(p)
        }
        fn put(&self, rel: &str, text: &str) {
            let p = self.0.join(rel);
            std::fs::create_dir_all(p.parent().unwrap()).unwrap();
            std::fs::write(p, text).unwrap();
        }
    }
    impl Drop for Dir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn fixture() -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../oled-sys/tests/fixtures")
    }

    #[test]
    fn lang_build_matches_the_fixture() {
        let got = build(&[fixture().join("lang-sample")], "it", (1, 8, 0)).unwrap();
        let want = std::fs::read(fixture().join("lang-sample.lang")).unwrap();
        assert_eq!(got, want);
    }

    #[test]
    fn lang_build_refuses() {
        let d = Dir::new();
        d.put("a/en.toml", "\"x.one\" = \"One\"\n");
        d.put("a/it.toml", "\"x.one\" = \"Uno\"\n");
        d.put("b/en.toml", "\"x.one\" = \"One\"\n");
        d.put("b/it.toml", "\"x.one\" = \"Uno\"\n");
        let two = build(&[d.0.join("a"), d.0.join("b")], "it", (1, 8, 0)).unwrap_err();
        assert!(two.contains("two strings folders"), "{two}");
        d.put("c/en.toml", "\"x.two\" = \"Two\"\n\"x.three\" = \"Three\"\n");
        d.put("c/it.toml", "\"x.two\" = \"Due\"\n");
        assert!(build(&[d.0.join("c")], "it", (1, 8, 0)).unwrap_err().contains("en.toml only"));
        d.put("e/en.toml", "\"x.two\" = \"Two {n}\"\n");
        d.put("e/it.toml", "\"x.two\" = \"Due {m}\"\n");
        assert!(build(&[d.0.join("e")], "it", (1, 8, 0)).unwrap_err().contains("placeholders"));
        assert!(build(&[d.0.join("a")], "ITA", (1, 8, 0)).is_err());
        assert!(build(&[d.0.join("missing")], "it", (1, 8, 0)).is_err());
    }

    #[test]
    fn versions_parse() {
        assert_eq!(parse_version("1.8.0"), Ok((1, 8, 0)));
        for bad in ["", "1", "1.8", "1.8.0.1", "1.8.x", "1.8.70000", "a.b.c"] {
            assert!(parse_version(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn find_dirs_lists_the_existing_folders() {
        let d = Dir::new();
        d.put("strings/en.toml", "");
        d.put("crates/x/strings/en.toml", "");
        d.put("crates/y/src/lib.rs", "");
        d.put("apps/z/strings/en.toml", "");
        let got = find_dirs(&d.0).unwrap();
        assert_eq!(got, vec![d.0.join("strings"), d.0.join("crates/x/strings"), d.0.join("apps/z/strings")]);
    }
}
