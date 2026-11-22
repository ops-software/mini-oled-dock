//! The synchronous providers (SPEC 5.4.1): what the session's view of the config is (`LauncherOpts`), what one
//! keystroke reads (`Context`, in memory only) and where the ranked rows are kept (`TopK`, the only allocation of a
//! query together with the result rows).

use super::catalog::CatApp;
use super::model::{Item, NumberStyle, ProviderMask, Score, SettingRow, WindowRow};
use super::query::PrefixTable;
use super::runline::Known;
use super::style;
use super::web::Template;
use core::cmp::Ordering;

/// The session's whole view of the config (built by the controller from `config.launcher`, L1a-15d).
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct LauncherOpts {
    pub prefixes: PrefixTable,
    pub mask: ProviderMask,
    pub web: Template,
    pub keep_query: bool,
    /// Visible rows (4-12).
    pub rows: u8,
}

impl Default for LauncherOpts {
    /// The defaults of 5.6.
    fn default() -> Self {
        LauncherOpts {
            prefixes: PrefixTable::defaults(),
            mask: ProviderMask::all(),
            web: Template::default(),
            keep_query: false,
            rows: style::ROWS_DEFAULT,
        }
    }
}

/// What one keystroke reads: borrowed from data the controller owns, built per keystroke without a copy.
#[derive(Clone, Copy)]
pub struct Context<'a> {
    pub apps: &'a [CatApp],
    pub settings: &'a [SettingRow],
    /// One per window, most recently used first.
    pub windows: &'a [WindowRow],
    /// Run history, newest first.
    pub history: &'a [Box<str>],
    pub known: &'static [Known],
    pub opts: &'a LauncherOpts,
    /// The locale's number marks, read at open.
    pub num: NumberStyle,
    pub now_unix: i64,
}

/// The best rows of a query: a fixed capacity of 3 x the visible rows, kept in result order.
#[derive(Debug)]
pub struct TopK {
    cap: usize,
    items: Vec<Item>,
}

/// Result order: the `Score` (5.2.5), then alphabetical (case-insensitive).
fn order(a: &Item, b: &Item) -> Ordering {
    a.score
        .cmp(&b.score)
        .then_with(|| a.title.chars().flat_map(char::to_lowercase).cmp(b.title.chars().flat_map(char::to_lowercase)))
}

impl TopK {
    pub fn new(rows: u8) -> TopK {
        let cap = 3 * usize::from(rows);
        TopK { cap, items: Vec::with_capacity(cap) }
    }

    pub fn cap(&self) -> usize {
        self.cap
    }

    pub fn len(&self) -> usize {
        self.items.len()
    }

    pub fn is_empty(&self) -> bool {
        self.items.is_empty()
    }

    pub fn items(&self) -> &[Item] {
        &self.items
    }

    /// Whether a row of this score would be kept: so a provider builds (and allocates) only the rows that stay.
    pub fn accepts(&self, score: &Score) -> bool {
        self.items.len() < self.cap || self.items.last().is_some_and(|worst| *score < worst.score)
    }

    /// Below capacity the item is inserted in order (after its equals); at capacity it replaces the worst only when
    /// it is better. Never grows the vector past its capacity.
    pub fn push(&mut self, item: Item) {
        if self.items.len() >= self.cap {
            match self.items.last() {
                Some(worst) if order(&item, worst) == Ordering::Less => {
                    self.items.pop();
                }
                _ => return,
            }
        }
        let at = self.items.partition_point(|x| order(x, &item) != Ordering::Greater);
        self.items.insert(at, item);
    }

    /// The rows, best first.
    pub fn into_sorted(self) -> Vec<Item> {
        self.items
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::indexing_slicing)]
pub(crate) mod tests {
    use super::*;
    use crate::launcher::model::{IconRef, ItemKey, Kind, Payload, ProviderId, VerbSet};
    use core::cmp::Reverse;

    pub(crate) fn item(title: &str, score: Score) -> Item {
        Item {
            key: ItemKey::of(ProviderId::Apps, title),
            from: ProviderId::Apps,
            kind: Kind::App,
            title: title.into(),
            detail: None,
            icon: IconRef::Placeholder,
            score,
            verbs: VerbSet::empty(),
            payload: Payload::None,
            destructive: false,
        }
    }

    fn tier(t: u8) -> Score {
        Score { tier: t, learned: Reverse(0), idle: true, kind: 0, rank: 0, len: 1 }
    }

    #[test]
    fn topk_keeps_best_by_score() {
        let mut top = TopK::new(1);
        assert_eq!(top.cap(), 3);
        for t in [5, 1, 3, 0, 6, 2, 4] {
            top.push(item(&format!("t{t}"), tier(t)));
        }
        let tiers: Vec<u8> = top.into_sorted().iter().map(|i| i.score.tier).collect();
        assert_eq!(tiers, [0, 1, 2]);
    }

    #[test]
    fn topk_capacity_is_3x_rows() {
        let mut top = TopK::new(8);
        assert_eq!(top.cap(), 24);
        let room = top.items.capacity();
        assert!(room >= 24);
        for i in 0..40u8 {
            top.push(item(&format!("r{i}"), tier(i % 7)));
        }
        assert_eq!(top.len(), 24);
        assert_eq!(top.items.capacity(), room, "the vector never grows");
        assert!(top.items().windows(2).all(|w| w[0].score <= w[1].score));
        assert!(!top.accepts(&tier(6)) && top.accepts(&tier(0)));
        let mut none = TopK::new(0);
        none.push(item("x", tier(0)));
        assert!(none.is_empty());
    }

    #[test]
    fn score_order_inside_a_tier() {
        let base = Score { tier: 2, learned: Reverse(0), idle: true, kind: 0, rank: 5, len: 4 };
        // The derive order of Score's fields is the order of 5.2.5.
        assert!(Score { idle: false, kind: 3, rank: 9, len: 9, ..base } < base, "running first");
        assert!(Score { kind: 0, rank: 9, len: 9, ..base } < Score { kind: 1, rank: 0, len: 0, ..base }, "kind");
        assert!(Score { rank: 1, len: 9, ..base } < Score { rank: 2, len: 0, ..base }, "rank");
        assert!(Score { len: 3, ..base } < base, "shorter name");
        let rows = [
            ("running", Score { idle: false, kind: 2, ..base }),
            ("app", Score { kind: 0, rank: 1, ..base }),
            ("short", Score { kind: 0, rank: 2, len: 3, ..base }),
            ("alpha", Score { kind: 0, rank: 2, len: 5, ..base }),
            ("Beta", Score { kind: 0, rank: 2, len: 5, ..base }),
            ("window", Score { kind: 1, rank: 0, ..base }),
        ];
        let mut top = TopK::new(2);
        for i in [4, 2, 5, 0, 3, 1] {
            let (t, s) = rows[i];
            top.push(item(t, s));
        }
        let titles: Vec<String> = top.into_sorted().iter().map(|i| i.title.to_string()).collect();
        assert_eq!(titles, ["running", "app", "short", "alpha", "Beta", "window"]);
    }
}
