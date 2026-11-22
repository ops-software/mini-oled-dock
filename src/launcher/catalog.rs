//! The launcher's own catalogue (SPEC 5.4.2, Open point 7): a folded copy of the Start catalogue, fed by the same
//! `StartApps` event (L1a-16 fills it), and the folded settings rows. Every name is folded once, here, when the data is
//! loaded: nothing is folded per candidate per keystroke.

use std::collections::HashMap;

use super::arena::{self, Arena, ArenaError, Caps, Kind, SettingIn};
#[cfg(test)]
use super::index::IndexBuf;
use super::keywords;
use super::model::SettingRow;
use super::rank::{self, Cand, Indexed, Words};
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
    pub words: Box<[u8]>,
    pub initials: Box<str>,
    /// Windows' synonyms of the app (AppIndex).
    pub keys: Keys,
    /// The other names of an inbox program (`keywords::INBOX`), already folded: matched like the shown name.
    pub extra: &'static [&'static str],
    /// The index of the inbox entry in `keywords::INBOX` (what the arena stores).
    pub inbox: Option<u16>,
    /// Microsoft's popularity rank, smaller is better.
    pub rank: u32,
    /// What Start searches the app with (L3 T17: its rows are made from the same arena): Windows' synonyms of the name as
    /// Windows wrote them (lowercase, unfolded, no inbox synonym) and the rank of the app's own id (no classic fallback).
    pub start_keys: Keys,
    pub start_rank: u32,
}

impl CatApp {
    /// Folds the name and looks up the inbox entry of the id or the name: its names become `extra`, its synonyms join
    /// the keywords with penalty 1.
    pub fn new(id: &str, name: &str, keys: Keys, rank: u32) -> CatApp {
        let x = Indexed::new(name);
        let (keys, extra, inbox) = match inbox_of(id, &x.folded) {
            Some((at, e)) => {
                let mut k = keys;
                for s in e.synonyms {
                    k.push(s, 1);
                }
                (k.tidy(), e.names, Some(at))
            }
            None => (keys, &[][..], None),
        };
        CatApp {
            id: id.into(),
            name: name.into(),
            folded: x.folded.into(),
            words: x.words.into(),
            initials: x.initials.into(),
            keys,
            extra,
            inbox,
            rank,
            start_keys: Keys::default(),
            start_rank: rank,
        }
    }

    pub fn cand(&self) -> Cand<'_> {
        Cand {
            folded: &self.folded,
            words: Words::new(&self.words),
            initials: &self.initials,
            keys: (&self.keys).into(),
            extra: self.extra,
            rank: self.rank,
        }
    }
}

/// The inbox entry of an app by its id (folded here) or, failing that, its folded name.
fn inbox_of(id: &str, folded_name: &str) -> Option<(u16, &'static keywords::Inbox)> {
    let mut folded_id = String::with_capacity(id.len());
    rank::fold(id, &mut folded_id);
    keywords::inbox_for(&folded_id, folded_name)
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
            words: Words::new(&self.words),
            initials: &self.initials,
            keys: (&self.keys).into(),
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

/// The popularity rank of an app: its own id's, else the smallest rank of the classic programs its inbox entry lists
/// (an inbox app has a new id and no rank of its own), else the index's default.
fn rank_of(a: &StartApp, x: &AppIndex) -> u32 {
    if let Some(r) = x.ranks.get(&a.id.to_lowercase()) {
        return *r;
    }
    let mut folded_name = String::with_capacity(a.name.len());
    rank::fold(&a.name, &mut folded_name);
    let classic = inbox_of(&a.id, &folded_name)
        .and_then(|(_, e)| e.classic.iter().filter_map(|id| x.ranks.get(*id)).min().copied());
    classic.unwrap_or(x.default_rank)
}

/// The longest id or name an entry may have: every string of an arena entry is at most this long (the folded name may
/// grow a little).
const NAME_MAX: usize = 4 * 1024;

/// `keys` cut after its last whole line that fits one arena string (Start's copy of one app's synonyms; this machine's
/// longest is about 6 KB).
fn start_copy(keys: &Keys) -> Keys {
    let text = keys.packed();
    if text.len() <= arena::STR_MAX {
        return keys.clone();
    }
    let head = text.get(..arena::STR_MAX).unwrap_or_default();
    Keys::from_packed(head.get(..head.rfind('\n').map_or(0, |n| n + 1)).unwrap_or_default())
}

/// The catalogue of one listing: names folded, words and initials built, the keywords of the installed apps copied out
/// of Windows' app index (which the caller drops), the inbox names attached, and Start's own copy of the synonyms and the
/// rank; sorted by name. An app no arena can hold (no id, no name, an id or name over 4 KiB) is left out.
pub fn ingest(list: &[StartApp], index: Option<&AppIndex>) -> Vec<CatApp> {
    let mut apps: Vec<CatApp> = list
        .iter()
        .filter(|a| !a.id.is_empty() && !a.name.is_empty() && a.id.len() <= NAME_MAX && a.name.len() <= NAME_MAX)
        .map(|a| {
            let Some(x) = index else { return CatApp::new(&a.id, &a.name, Keys::default(), 0) };
            let synonyms = x.synonyms.get(&a.name.to_lowercase());
            let keys = synonyms.map(fold_keys).unwrap_or_default();
            let mut app = CatApp::new(&a.id, &a.name, keys, rank_of(a, x));
            app.start_keys = synonyms.map(start_copy).unwrap_or_default();
            app.start_rank = x.ranks.get(&a.id.to_lowercase()).copied().unwrap_or(x.default_rank);
            app
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

/// The apps catalogue of one listing as the bytes of its arena: what the `--start-apps` child writes (L3 T17).
pub fn encode_app_list(list: &[StartApp], index: Option<&AppIndex>) -> Result<Vec<u8>, ArenaError> {
    arena::encode_apps(&ingest(list, index), Caps::APPS)
}

/// The apps catalogue of one listing as an arena (L3 T16): `ingest`, encoded, checked and the folded entries dropped, so
/// the dock keeps one block of bytes instead of thousands. Made in process only when the `--start-apps` child failed.
pub fn build_apps(list: &[StartApp], index: Option<&AppIndex>) -> Result<Arena, ArenaError> {
    Arena::new(encode_app_list(list, index)?, Kind::Apps, None, Caps::APPS)
}

/// The key line of an arena made in this process (a cache file's key names what it was built from, T18).
pub const IN_PROCESS_KEY: &str = "in-process|fmt 2";

/// The keyword dictionary of the apps (L3 T9), built when the catalogue arrives.
#[cfg(test)]
pub fn index_apps(apps: &[CatApp]) -> IndexBuf {
    IndexBuf::build(apps.iter().enumerate().map(|(i, a)| (u32::try_from(i).unwrap_or(u32::MAX), a.cand())))
}

/// The keyword dictionary of the settings rows (L3 T9), built when the rows are folded.
#[cfg(test)]
pub fn index_settings(rows: &[SettingRow]) -> IndexBuf {
    IndexBuf::build(rows.iter().enumerate().map(|(i, r)| (u32::try_from(i).unwrap_or(u32::MAX), r.cand())))
}

/// The launcher's settings as one arena: made from the held list off the UI thread (`start_search` builds it on a settings
/// thread, L3 KEY-11) and shared with the list it indexes (the entry at position i is the setting at index i).
#[derive(Default, Debug)]
pub struct FoldedSettings {
    pub arena: Arena,
}

impl FoldedSettings {
    /// `fold_settings` of `list`, encoded and checked.
    pub fn new(list: &[Setting]) -> Result<FoldedSettings, ArenaError> {
        let ins: Vec<SettingIn> = fold_settings(list)
            .into_iter()
            .zip(list)
            .map(|(row, s)| SettingIn {
                row,
                target: s.target.to_string(),
                args: s.args.as_ref().map(|a| a.to_string()),
                start_keys: String::new(),
            })
            .collect();
        let bytes = arena::encode_settings(IN_PROCESS_KEY, &ins, Caps::SETTINGS)?;
        Ok(FoldedSettings { arena: Arena::new(bytes, Kind::Settings, Some(IN_PROCESS_KEY), Caps::SETTINGS)? })
    }
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

    #[cfg(test)]
    pub fn len(&self) -> usize {
        self.map.len()
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::indexing_slicing)]
mod tests {
    use super::*;

    #[test]
    fn cat_app_folds_once_and_finds_its_keywords() {
        let a = CatApp::new("Microsoft.WindowsCalculator_8wekyb3d8bbwe!App", "Calcolatrice", Keys::default(), 3);
        assert_eq!((&*a.folded, &*a.initials, &*a.words), ("calcolatrice", "c", &[0u8, 0][..]));
        assert!(a.extra.contains(&"calculator"));
        assert_eq!(rank::match_tiers(&a.cand(), "calculator"), Some(0), "the other language's name is a name");
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
        assert_eq!(rank::match_tiers(&rdc.cand(), "rdp"), Some(4), "the synonym finds it");
        assert_eq!((apps[0].rank, apps[2].rank), (99, 99), "an app without a rank gets the default");
        // No index: no keywords, rank 0.
        let bare = ingest(&list, None);
        assert!(bare.iter().all(|a| a.rank == 0));
        let hidden = bare.iter().filter(|a| a.keys.iter().next().is_some()).count();
        assert_eq!(hidden, 1, "only the inbox entry (Remote Desktop) has keywords, its own synonyms");
    }

    #[test]
    fn inbox_rank_falls_back_to_the_classic_program() {
        let mut index = AppIndex { default_rank: 12_813, ..AppIndex::default() };
        index.ranks.insert("{1ac14e77-02e7-4e5d-b744-2eb1ae5198b7}\\notepad.exe".into(), 3_114);
        index.ranks.insert("microsoft.windowscalculator_8wekyb3d8bbwe!app".into(), 77);
        let list = [
            StartApp { id: "Microsoft.WindowsNotepad_8wekyb3d8bbwe!App".into(), name: "Blocco note".into() },
            StartApp { id: "Microsoft.WindowsCalculator_8wekyb3d8bbwe!App".into(), name: "Calcolatrice".into() },
            StartApp { id: "Unknown.exe".into(), name: "Unknown".into() },
        ];
        let apps = ingest(&list, Some(&index));
        let rank = |name: &str| apps.iter().find(|a| &*a.name == name).unwrap().rank;
        assert_eq!(rank("Blocco note"), 3_114, "no rank of its own: the classic program's");
        assert_eq!(rank("Calcolatrice"), 77, "its own rank is kept");
        assert_eq!(rank("Unknown"), 12_813, "no entry: the default");
    }

    /// L3 T17: the entry builder keeps Start's own copy (Windows' synonyms as written, the rank of the app's own id), and
    /// leaves out what no arena can hold instead of failing the whole catalogue.
    #[test]
    fn ingest_keeps_starts_copy_and_drops_what_no_arena_holds() {
        let mut index = AppIndex { default_rank: 50, ..AppIndex::default() };
        index.synonyms.insert("connessione desktop remoto".into(), Keys::from(&[("RDP", 10), ("Remoto", 40)][..]));
        index.ranks.insert("{1ac14e77-02e7-4e5d-b744-2eb1ae5198b7}\\notepad.exe".into(), 3);
        let mut huge = Keys::default();
        for i in 0..8_000 {
            huge.push(&format!("typo{i:05}"), i);
        }
        assert!(huge.packed().len() > arena::STR_MAX);
        index.synonyms.insert("huge".into(), huge);
        let list = [
            StartApp { id: "RDC.exe".into(), name: "Connessione Desktop Remoto".into() },
            StartApp { id: "Microsoft.WindowsNotepad_8wekyb3d8bbwe!App".into(), name: "Blocco note".into() },
            StartApp { id: String::new(), name: "No id".into() },
            StartApp { id: "Long.exe".into(), name: "x".repeat(70_000) },
            StartApp { id: "Huge.exe".into(), name: "Huge".into() },
        ];
        let apps = ingest(&list, Some(&index));
        let names: Vec<&str> = apps.iter().map(|a| &*a.name).collect();
        assert_eq!(names, ["Blocco note", "Connessione Desktop Remoto", "Huge"]);
        assert_eq!((apps[0].rank, apps[0].start_rank), (3, 50), "Start keeps the rank of the app's own id");
        assert_eq!(apps[1].start_keys, Keys::from(&[("RDP", 10), ("Remoto", 40)][..]), "as Windows wrote them");
        let cut = apps[2].start_keys.packed();
        assert!(cut.len() <= arena::STR_MAX && cut.ends_with('\n') && cut.starts_with("typo00000\t0\n"));
        let a = build_apps(&list, Some(&index)).unwrap();
        assert_eq!((a.len(), a.view().start_keys(1), a.view().start_rank(0)), (3, apps[1].start_keys.packed(), 50));
        // No index: no keywords, rank 0, as Start had it.
        assert!(ingest(&list, None).iter().all(|a| a.start_keys.packed().is_empty() && a.start_rank == 0));
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
    }
}
