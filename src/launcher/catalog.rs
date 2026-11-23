//! The launcher's own catalogue (SPEC 5.4.2, Open point 7): a folded copy of the Start catalogue, fed by the same
//! `StartApps` event (L1a-16 fills it), and the folded settings rows. Every name is folded once, here, when the data is
//! loaded: nothing is folded per candidate per keystroke.

use std::collections::HashMap;
use std::rc::Rc;

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

/// The settings entries of `list` as the encoder takes them (L3 T18), each folded once: the row (names, words, initials and
/// keywords folded; the position among the entries is its index), the launch target and arguments, and Start's packed
/// keywords as 1.4.3 searched them (`start_search::start_keys_of`; cut after a whole line when longer than an arena
/// string). A setting no arena can hold (a name, target or argument over 4 KiB; never seen) is left out instead of
/// failing the whole catalogue.
pub fn setting_entries(list: &[Setting]) -> Vec<SettingIn> {
    let fits = |s: &&Setting| {
        !s.name.is_empty()
            && s.name.len() <= NAME_MAX
            && s.target.len() <= NAME_MAX
            && s.args.as_ref().is_none_or(|a| a.len() <= NAME_MAX)
    };
    list.iter()
        .filter(fits)
        .enumerate()
        .map(|(i, s)| SettingIn {
            row: SettingRow::new(u32::try_from(i).unwrap_or(u32::MAX), &s.name, fold_keys(&s.keys), s.rank),
            target: s.target.to_string(),
            args: s.args.clone(),
            start_keys: start_copy(&crate::start_search::start_keys_of(&s.keys)).packed().to_string(),
        })
        .collect()
}

/// The settings catalogue of `list` as the bytes of its arena under `key` (the line that names what it was built from):
/// what the `--start-settings` child writes, and what the dock encodes itself when the child fails (L3 T18).
pub fn encode_setting_list(key: &str, list: &[Setting]) -> Result<Vec<u8>, ArenaError> {
    arena::encode_settings(key, &setting_entries(list), Caps::SETTINGS)
}

/// The settings catalogue of `list` as a checked arena under `key` (the tests' and the ranking table's way to the real
/// entry builder and encoder).
#[cfg(test)]
pub fn build_settings_arena(key: &str, list: &[Setting]) -> Result<Arena, ArenaError> {
    Arena::new(encode_setting_list(key, list)?, Kind::Settings, Some(key), Caps::SETTINGS)
}

/// The bytes the icons kept between opens may take (MEM-2): each entry counts its pixels (w x h x 4) and its key.
pub const ICON_CACHE_BYTES: usize = 160 * 1024;

/// What an entry of the icon cache counts against `ICON_CACHE_BYTES`.
fn icon_entry_bytes(key: &str, bmp: &IconBitmap) -> usize {
    bmp.pixels.len().saturating_mul(4).saturating_add(key.len())
}

/// The icons kept between opens, at most `ICON_CACHE_BYTES`, the least recently used (painted at a close, or arrived)
/// dropped first. Each bitmap is shared: an open's rows hold the same `Rc`s, never copies.
#[derive(Default)]
pub struct IconCache {
    map: HashMap<String, (Rc<IconBitmap>, u64)>,
    tick: u64,
    /// What the entries count, kept as they come and go.
    bytes: usize,
}

impl IconCache {
    /// Keeps `bmp` under the lower-case `key`; the least recently used entries go while the cache is over its cap. An
    /// icon larger than the whole cap is not kept (it would push every other one out).
    pub fn insert(&mut self, key: &str, bmp: Rc<IconBitmap>) {
        let key = key.to_lowercase();
        let cost = icon_entry_bytes(&key, &bmp);
        if cost > ICON_CACHE_BYTES {
            return;
        }
        self.tick += 1;
        let key_len = key.len();
        if let Some((old, _)) = self.map.insert(key, (bmp, self.tick)) {
            self.bytes = self.bytes.saturating_sub(old.pixels.len().saturating_mul(4).saturating_add(key_len));
        }
        self.bytes = self.bytes.saturating_add(cost);
        // The new entry is the most recent: it is never the one that goes.
        while self.bytes > ICON_CACHE_BYTES {
            let Some(oldest) = self.map.iter().min_by_key(|(_, (_, t))| *t).map(|(k, _)| k.clone()) else { break };
            if let Some((b, _)) = self.map.remove(&oldest) {
                self.bytes = self.bytes.saturating_sub(icon_entry_bytes(&oldest, &b));
            }
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

    #[cfg(test)]
    pub fn get(&self, key: &str) -> Option<&Rc<IconBitmap>> {
        self.map.get(key).map(|(b, _)| b)
    }

    pub fn iter(&self) -> impl Iterator<Item = (&str, &Rc<IconBitmap>)> {
        self.map.iter().map(|(k, (b, _))| (k.as_str(), b))
    }

    /// What the cache holds, as its cap counts it.
    #[cfg(any(test, feature = "owner-probe"))]
    pub fn bytes(&self) -> usize {
        self.bytes
    }

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
        let rows: Vec<SettingRow> = setting_entries(&list).into_iter().map(|e| e.row).collect();
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

    /// L3 T18: the settings entry builder keeps what Start searches with and what a setting opens, and leaves out what no
    /// arena can hold instead of failing the whole catalogue; the positions are the indexes the launcher launches by.
    #[test]
    fn setting_entries_keep_starts_copy_and_the_targets() {
        let mut huge = Keys::default();
        for i in 0..8_000 {
            huge.push(&format!("parola{i:05}"), i);
        }
        assert!(huge.packed().len() > arena::STR_MAX);
        let mut panel = setting("Pannello di controllo", &[("Control Panel", 2)], 4);
        panel.target = std::borrow::Cow::Owned(r"C:\Windows\System32\control.exe".into());
        panel.args = Some("/name Microsoft.Mouse".into());
        let list = [
            setting("Accessibilità", &[("Contrasto più", 5)], 2),
            setting(&"x".repeat(5_000), &[], 1),
            panel,
            setting("", &[], 1),
            setting("Molte parole", &[], 9),
        ];
        let mut many = list;
        many[4].keys = huge;
        let ins = setting_entries(&many);
        let names: Vec<&str> = ins.iter().map(|e| &*e.row.name).collect();
        assert_eq!(names, ["Accessibilità", "Pannello di controllo", "Molte parole"]);
        assert_eq!(ins.iter().map(|e| e.row.index).collect::<Vec<_>>(), [0, 1, 2], "the position is the index");
        assert_eq!(ins[0].start_keys, "contrasto più\t5\n", "Start's keywords as Start searches them, unfolded");
        assert_eq!(ins[0].row.keys.iter().collect::<Vec<_>>(), [("contrasto piu", 5)], "the launcher's folded");
        assert_eq!(
            (ins[1].target.as_str(), ins[1].args.as_deref()),
            (r"C:\Windows\System32\control.exe", Some("/name Microsoft.Mouse"))
        );
        assert!(ins[2].start_keys.len() <= arena::STR_MAX && ins[2].start_keys.ends_with('\n'));
        let a = build_settings_arena("k|fmt 1", &many).unwrap();
        let v = a.view();
        assert_eq!(
            (v.len(), v.name(1), v.target(1)),
            (3, "Pannello di controllo", (ins[1].target.as_str(), ins[1].args.as_deref()))
        );
        assert_eq!((v.start_keys(0), v.rank(2)), ("contrasto più\t5\n", 9));
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

    /// L3 review: past the arena's caps a listing still gives Start a catalogue (house rule 7 of plan L3): 2,100 apps
    /// keep the 2,000 best ranked by Start's rank (the rank of each app's own id), in name order, with what Start and
    /// the launcher search them by.
    #[test]
    fn a_listing_past_the_caps_keeps_the_best_ranked() {
        let mut index = AppIndex { default_rank: 5_000, ..AppIndex::default() };
        let list: Vec<StartApp> =
            (0..2_100).map(|i| StartApp { id: format!("App{i:04}.exe"), name: format!("App {i:04}") }).collect();
        for i in 100..2_100 {
            index.ranks.insert(format!("app{i:04}.exe"), i);
        }
        index.synonyms.insert("app 0100".into(), Keys::from(&[("Primo", 1)][..]));
        let a = build_apps(&list, Some(&index)).unwrap();
        let v = a.view();
        assert_eq!(v.len(), Caps::APPS.entries);
        assert_eq!((v.name(0), v.name(1_999)), ("App 0100", "App 2099"), "the 100 unranked apps went");
        assert!((1..v.len()).all(|i| v.name(i - 1) < v.name(i)), "name order");
        assert_eq!((v.start_keys(0), v.start_rank(0), v.id(0)), ("primo\t1\n", 100, "App0100.exe"));
        assert_eq!(rank::match_tiers(&v.cand(0), "app 0100"), Some(0));
    }

    #[test]
    fn icon_cache_drops_the_least_recently_used() {
        let icon = || Rc::new(IconBitmap::new(48, 48));
        let mut c = IconCache::default();
        for i in 0..17 {
            c.insert(&format!("APP{i:02}"), icon());
        }
        assert_eq!(c.len(), 17, "17 icons of 48 px fit");
        c.touch("app00");
        c.insert("new", icon());
        assert_eq!(c.len(), 17);
        assert!(c.get("app00").is_some(), "used last, kept");
        assert!(c.get("app01").is_none(), "the least recently used went");
        assert!(c.get("new").is_some());
        // The same key again counts once.
        c.insert("NEW", icon());
        assert_eq!((c.len(), c.bytes()), (17, 16 * (48 * 48 * 4 + 5) + 48 * 48 * 4 + 3));
        // An icon larger than the whole cap is not kept, and pushes nothing out.
        c.insert("huge", Rc::new(IconBitmap::new(256, 256)));
        assert_eq!((c.len(), c.get("huge").is_none()), (17, true));
    }

    /// L3 T20 (MEM-2): the cache is capped by bytes, each entry counting its pixels and its key: with 110-byte ids,
    /// floor(163,840 / (px x px x 4 + 110)) icons stay, the newest, and the cache never holds more than 160 KiB.
    #[test]
    fn icon_cache_capped_by_bytes() {
        let id = |i: usize| format!("{i:04}{}", "x".repeat(106));
        assert_eq!(id(7).len(), 110);
        for (px, kept) in [(30u32, 44usize), (36, 30), (48, 17)] {
            let mut c = IconCache::default();
            for i in 0..100 {
                c.insert(&id(i), Rc::new(IconBitmap::new(px, px)));
                assert!(c.bytes() <= ICON_CACHE_BYTES, "{px} px, {i} icons: {} B", c.bytes());
            }
            assert_eq!(c.len(), kept, "{px} px");
            assert_eq!(c.bytes(), kept * (px as usize * px as usize * 4 + 110));
            assert!(c.get(&id(99)).is_some() && c.get(&id(100 - kept)).is_some() && c.get(&id(99 - kept)).is_none());
        }
    }
}
