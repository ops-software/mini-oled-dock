//! The launcher's own catalogue (SPEC 5.4.2, Open point 7): a folded copy of the Start catalogue, fed by the same
//! `StartApps` event (L1a-16 fills it), and the folded settings rows. Every name is folded once, here, when the data is
//! loaded: nothing is folded per candidate per keystroke.

use super::keywords;
use super::model::SettingRow;
use super::rank::{self, Cand, Indexed};
use crate::start_search::Keys;

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
}
