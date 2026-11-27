//! The gallery's PlacementStore (K1a.11, KA2): gallery-placement.json beside gallery.exe, never in the dock's data
//! folders: its path comes from std::env::current_exe only. Written atomically (a .tmp file, then a rename over the old
//! one); with no usable folder the placements live in memory only.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use oled_ui::geom::RectP;
use oled_ui::host::{Placement, PlacementKey, PlacementStore};
use serde_json::{json, Map, Value};

/// The file's name, beside gallery.exe.
pub const FILE_NAME: &str = "gallery-placement.json";

/// `<the exe's folder>\gallery-placement.json`; None when the path has no folder.
pub fn path_for(exe: &Path) -> Option<PathBuf> {
    exe.parent().filter(|d| !d.as_os_str().is_empty()).map(|d| d.join(FILE_NAME))
}

/// The placements of a file: `{ "<key>": { "monitor", "rect": [l, t, r, b], "dpi", "maximized" } }`. Garbage, a
/// truncated file or an entry with a field missing or of the wrong kind gives nothing for it.
pub fn parse(bytes: &[u8]) -> BTreeMap<String, Placement> {
    let Ok(Value::Object(top)) = serde_json::from_slice::<Value>(bytes) else { return BTreeMap::new() };
    let int = |v: &Value| v.as_i64().and_then(|n| i32::try_from(n).ok());
    let entry = |v: &Value| -> Option<Placement> {
        let monitor = v.get("monitor")?.as_str()?.to_string();
        let r: Vec<i32> = v.get("rect")?.as_array()?.iter().map(int).collect::<Option<_>>()?;
        let [l, t, rr, b] = r[..] else { return None };
        let dpi = u32::try_from(v.get("dpi")?.as_u64()?).ok()?;
        let maximized = v.get("maximized")?.as_bool()?;
        Some(Placement { monitor, rect: RectP { l, t, r: rr, b }, dpi, maximized })
    };
    top.iter().filter_map(|(k, v)| entry(v).map(|p| (k.clone(), p))).collect()
}

/// The file's text of the placements.
pub fn to_json(map: &BTreeMap<String, Placement>) -> String {
    let mut top = Map::new();
    for (k, p) in map {
        let r = p.rect;
        top.insert(
            k.clone(),
            json!({ "monitor": p.monitor, "rect": [r.l, r.t, r.r, r.b], "dpi": p.dpi, "maximized": p.maximized }),
        );
    }
    Value::Object(top).to_string()
}

/// The placements in gallery-placement.json.
pub struct FileStore {
    path: Option<PathBuf>,
    map: BTreeMap<String, Placement>,
}

impl FileStore {
    /// The file beside the running gallery.exe.
    pub fn beside_exe() -> FileStore {
        FileStore::load_from(std::env::current_exe().ok().and_then(|e| path_for(&e)))
    }

    /// The file in `dir` (tests).
    pub fn at(dir: &Path) -> FileStore {
        FileStore::load_from(Some(dir.join(FILE_NAME)))
    }

    fn load_from(path: Option<PathBuf>) -> FileStore {
        let map = path.as_deref().and_then(|p| std::fs::read(p).ok()).map(|b| parse(&b)).unwrap_or_default();
        FileStore { path, map }
    }

    /// The whole map to the file: a .tmp beside it, then a rename over the old one.
    fn write(&self) -> std::io::Result<()> {
        let Some(p) = &self.path else { return Ok(()) };
        let tmp = p.with_extension("json.tmp");
        std::fs::write(&tmp, to_json(&self.map))?;
        std::fs::rename(&tmp, p)
    }
}

impl PlacementStore for FileStore {
    fn load(&self, key: PlacementKey) -> Option<Placement> {
        self.map.get(key.0).cloned()
    }

    fn save(&mut self, key: PlacementKey, p: &Placement) {
        self.map.insert(key.0.to_string(), p.clone());
        if let Err(e) = self.write() {
            eprintln!("gallery: placement: {e}");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A fresh folder under the temp dir, removed on drop.
    struct Temp(PathBuf);

    impl Temp {
        fn new(tag: &str) -> Temp {
            let p = std::env::temp_dir().join(format!("gallery-placement-{tag}-{}", std::process::id()));
            let _ = std::fs::remove_dir_all(&p);
            std::fs::create_dir_all(&p).unwrap();
            Temp(p)
        }
    }

    impl Drop for Temp {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn sample() -> Placement {
        Placement {
            monitor: r"\\.\DISPLAY2".to_string(),
            rect: RectP { l: -40, t: 12, r: 1060, b: 732 },
            dpi: 144,
            maximized: true,
        }
    }

    #[test]
    fn file_store_round_trip() {
        let dir = Temp::new("round");
        let mut s = FileStore::at(&dir.0);
        assert_eq!(s.load(PlacementKey("gallery")), None);
        s.save(PlacementKey("gallery"), &sample());
        assert_eq!(s.load(PlacementKey("gallery")), Some(sample()));
        let again = FileStore::at(&dir.0);
        assert_eq!(again.load(PlacementKey("gallery")), Some(sample()), "read back from the file");
        assert!(!dir.0.join("gallery-placement.json.tmp").exists(), "the temporary file was renamed");
    }

    #[test]
    fn file_store_corrupt_file_is_none() {
        let dir = Temp::new("corrupt");
        let file = dir.0.join(FILE_NAME);
        let whole = to_json(&BTreeMap::from([("gallery".to_string(), sample())]));
        for bad in
            ["{not json", &whole[..whole.len() / 2], "[1, 2]", r#"{"gallery": {"monitor": "A", "rect": [1, 2, 3]}}"#]
        {
            std::fs::write(&file, bad).unwrap();
            let mut s = FileStore::at(&dir.0);
            assert_eq!(s.load(PlacementKey("gallery")), None, "{bad}");
            s.save(PlacementKey("other"), &sample());
            let fixed = std::fs::read(&file).unwrap();
            assert_eq!(parse(&fixed).len(), 1, "replaced by a valid file at the next save");
        }
    }

    #[test]
    fn file_store_path_is_beside_the_exe() {
        let exe = Path::new(r"C:\x\y\gallery.exe");
        assert_eq!(path_for(exe), Some(PathBuf::from(r"C:\x\y\gallery-placement.json")));
        assert_eq!(path_for(Path::new("gallery.exe")), None, "no folder: memory only");
        let src = include_str!("placement.rs");
        let code = &src[..src.find("#[cfg(test)]").unwrap()];
        for var in ["APPDATA", "ProgramData", "var(", "var_os("] {
            assert!(!code.contains(var), "the store reads no {var}");
        }
        let mut mem = FileStore::load_from(None);
        mem.save(PlacementKey("k"), &sample());
        assert_eq!(mem.load(PlacementKey("k")), Some(sample()), "memory only still answers");
    }
}
