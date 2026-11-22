//! The launcher's own catalogue (SPEC 5.4.2, Open point 7): a folded copy of the Start catalogue, fed by the same
//! `StartApps` event (L1a-16 fills it), and the folded settings rows. Every name is folded once, here, when the data is
//! loaded: nothing is folded per candidate per keystroke.

use std::collections::HashMap;

use super::keywords;
use super::model::SettingRow;
use super::rank::{self, Cand, Indexed};
use crate::icons::IconBitmap;
use crate::shell_thread::StartApp;
use crate::start_search::{AppIndex, Keys, Setting};

/// One app of the catalogue, folded at load.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct CatApp {
    /// The AppsFolder id (an AUMID or a path), as Start launches it.
    pub id: Box<str>,
    /// The name as the native Start shows it.
    pub name: Box<str>,
    pub folded: Box<str>,
    /// Start offsets of the words of `folded`.
    pub words: Box<[u16]>,
    pub initials: Box<str>,
    /// Windows' synonyms of the app (AppIndex).
    pub keys: Keys,
    /// The other language's names (`keywords::BILINGUAL`), already folded.
    pub extra: &'static [&'static str],
    /// Microsoft's popularity rank, smaller is better.
    pub rank: u32,
}

impl CatApp {
    /// Folds the name and looks up the hidden keywords of the id.
    pub fn new(id: &str, name: &str, keys: Keys, rank: u32) -> CatApp {
        let x = Indexed::new(name);
        let mut folded_id = String::with_capacity(id.len());
        rank::fold(id, &mut folded_id);
        CatApp {
            id: id.into(),
            name: name.into(),
            folded: x.folded.into(),
            words: x.words.into(),
            initials: x.initials.into(),
            keys,
            extra: keywords::bilingual_for(&folded_id),
            rank,
        }
    }

    pub fn cand(&self) -> Cand<'_> {
        Cand {
            folded: &self.folded,
            words: &self.words,
            initials: &self.initials,
            keys: &self.keys,
            extra: self.extra,
            rank: self.rank,
        }
    }
}

/// The resident catalogue: the apps and when they were listed (refreshed after a close when older than 10 minutes).
#[derive(Clone, Default, PartialEq, Eq, Debug)]
pub struct Catalog {
    pub apps: Vec<CatApp>,
    pub listed_at_ms: u64,
}

impl SettingRow {
    /// One setting folded at load; `keys` is the setting's folded copy of its keywords.
    pub fn new(index: u32, name: &str, keys: Keys, rank: u32) -> SettingRow {
        let x = Indexed::new(name);
        SettingRow {
            index,
            name: name.into(),
            folded: x.folded.into(),
            words: x.words.into(),
            initials: x.initials.into(),
            keys,
            rank,
        }
    }

    pub fn cand(&self) -> Cand<'_> {
        Cand {
            folded: &self.folded,
            words: &self.words,
            initials: &self.initials,
            keys: &self.keys,
            extra: &[],
            rank: self.rank,
        }
    }
}

/// A folded, tidied copy of `k`: the launcher's queries are folded, so its keywords are too.
fn fold_keys(k: &Keys) -> Keys {
    let mut out = Keys::default();
    let mut buf = String::new();
    for (key, penalty) in k.iter() {
        buf.clear();
        rank::fold(key, &mut buf);
        out.push(&buf, penalty);
    }
    out.tidy()
}

/// The catalogue of one `StartApps` event: names folded, words and initials built, the keywords of the installed apps
/// copied out of Windows' app index (which the caller drops) and the bilingual names attached; sorted by name.
pub fn ingest(list: &[StartApp], index: Option<&AppIndex>) -> Vec<CatApp> {
    let mut apps: Vec<CatApp> = list
        .iter()
        .map(|a| {
            let (keys, rank) = index.map_or((Keys::default(), 0), |x| {
                let rank = x.ranks.get(&a.id.to_lowercase()).copied().unwrap_or(x.default_rank);
                (x.synonyms.get(&a.name.to_lowercase()).map(fold_keys).unwrap_or_default(), rank)
            });
            CatApp::new(&a.id, &a.name, keys, rank)
        })
        .collect();
    apps.sort_by(|a, b| a.folded.cmp(&b.folded).then_with(|| a.name.cmp(&b.name)));
    apps.shrink_to_fit();
    apps
}

/// The settings catalogue folded once, when it arrives: `index` is the position in `list`, the held list the launch
/// targets come from.
pub fn fold_settings(list: &[Setting]) -> Vec<SettingRow> {
    list.iter()
        .enumerate()
        .map(|(i, s)| SettingRow::new(u32::try_from(i).unwrap_or(u32::MAX), &s.name, fold_keys(&s.keys), s.rank))
        .collect()
}

/// Icons kept between opens (at most `ICON_CACHE_CAP`), the least recently used dropped first.
pub const ICON_CACHE_CAP: usize = 40;

#[derive(Default)]
pub struct IconCache {
    map: HashMap<String, (IconBitmap, u64)>,
    tick: u64,
}

impl IconCache {
    /// Keeps `bmp` under the lower-case `key`; the oldest entry goes when the cache is full.
    pub fn insert(&mut self, key: &str, bmp: IconBitmap) {
        self.tick += 1;
        self.map.insert(key.to_lowercase(), (bmp, self.tick));
        while self.map.len() > ICON_CACHE_CAP {
            let oldest = self.map.iter().min_by_key(|(_, (_, t))| *t).map(|(k, _)| k.clone());
            match oldest {
                Some(k) => self.map.remove(&k),
                None => break,
            };
        }
    }

    /// Marks `key` as just used.
    pub fn touch(&mut self, key: &str) {
        self.tick += 1;
        let tick = self.tick;
        if let Some(e) = self.map.get_mut(key) {
            e.1 = tick;
        }
    }

    pub fn get(&self, key: &str) -> Option<&IconBitmap> {
        self.map.get(key).map(|(b, _)| b)
    }

    pub fn iter(&self) -> impl Iterator<Item = (&str, &IconBitmap)> {
        self.map.iter().map(|(k, (b, _))| (k.as_str(), b))
    }

    pub fn len(&self) -> usize {
        self.map.len()
    }

    pub fn clear(&mut self) {
        self.map = HashMap::new();
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::indexing_slicing)]
mod tests {
    use super::*;

    #[test]
    fn cat_app_folds_once_and_finds_its_keywords() {
        let a = CatApp::new("Microsoft.WindowsCalculator_8wekyb3d8bbwe!App", "Calcolatrice", Keys::default(), 3);
        assert_eq!((&*a.folded, &*a.initials, &*a.words), ("calcolatrice", "c", &[0u16][..]));
        assert!(a.extra.contains(&"calculator"));
        assert_eq!(rank::match_tiers(&a.cand(), "calculator"), Some(1));
        let s = SettingRow::new(7, "Accessibilità", Keys::default(), 1);
        assert_eq!((&*s.folded, s.index), ("accessibilita", 7));
        assert_eq!(rank::match_tiers(&s.cand(), "accessibilita"), Some(0));
    }

    fn setting(name: &str, keys: &[(&str, u32)], rank: u32) -> Setting {
        Setting {
            name: name.into(),
            lower: name.to_lowercase(),
            keys: Keys::from(keys),
            rank,
            target: std::borrow::Cow::Borrowed("ms-settings:"),
            args: None,
        }
    }

    #[test]
    fn fold_settings_once() {
        let list = [
            setting("Accessibilità", &[("Accessibilità", 3), ("Contrasto", 5)], 2),
            setting("Schermo", &[], 1),
            setting("Rete e Internet", &[("Wi-Fi", 4)], 7),
        ];
        let rows = fold_settings(&list);
        assert_eq!(rows.len(), 3);
        assert_eq!((&*rows[0].folded, rows[0].index, rows[2].index), ("accessibilita", 0, 2));
        let keys: Vec<(&str, u32)> = rows[0].keys.iter().collect();
        assert_eq!(keys, [("accessibilita", 3), ("contrasto", 5)], "keywords folded and sorted");
        assert_eq!(&*rows[2].initials, "rei");
        // A query is matched against the folded rows without allocating.
        let (hit, allocs) = crate::launcher::testkit::allocations_during(|| {
            let mut found = 0;
            for _ in 0..10 {
                found += usize::from(rank::match_tiers(&rows[0].cand(), "accessibilita").is_some());
            }
            found
        });
        assert_eq!((hit, allocs), (10, 0));
    }

    #[test]
    fn ingest_folds_and_copies_keys() {
        let mut index = AppIndex::default();
        index.synonyms.insert("connessione desktop remoto".into(), Keys::from(&[("RDP", 10), ("Remoto", 40)][..]));
        index.ranks.insert("rdc.exe".into(), 3);
        index.default_rank = 99;
        let list = [
            StartApp { id: "Zeta.exe".into(), name: "Zeta".into() },
            StartApp { id: "RDC.exe".into(), name: "Connessione Desktop Remoto".into() },
            StartApp { id: "Àlfa.exe".into(), name: "Àlfa".into() },
        ];
        let apps = ingest(&list, Some(&index));
        let names: Vec<&str> = apps.iter().map(|a| &*a.name).collect();
        assert_eq!(names, ["Àlfa", "Connessione Desktop Remoto", "Zeta"]);
        let rdc = &apps[1];
        assert_eq!(rdc.rank, 3);
        assert_eq!(rank::match_tiers(&rdc.cand(), "rdp"), Some(1), "the synonym finds it");
        assert_eq!((apps[0].rank, apps[2].rank), (99, 99), "an app without a rank gets the default");
        // No index: no keywords, rank 0.
        let bare = ingest(&list, None);
        assert!(bare.iter().all(|a| a.rank == 0 && a.keys.iter().next().is_none()));
    }

    #[test]
    fn icon_cache_keeps_the_newest_forty() {
        let mut c = IconCache::default();
        for i in 0..ICON_CACHE_CAP {
            c.insert(&format!("APP{i}"), IconBitmap::new(2, 2));
        }
        assert_eq!(c.len(), ICON_CACHE_CAP);
        c.touch("app0");
        c.insert("new", IconBitmap::new(2, 2));
        assert_eq!(c.len(), ICON_CACHE_CAP);
        assert!(c.get("app0").is_some(), "used last, kept");
        assert!(c.get("app1").is_none(), "the least recently used went");
        assert!(c.get("new").is_some());
        c.clear();
        assert_eq!(c.len(), 0);
    }
}
