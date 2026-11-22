//! The synchronous providers (SPEC 5.4.1): what the session's view of the config is (`LauncherOpts`), what one
//! keystroke reads (`Context`, in memory only), where the ranked rows are kept (`TopK`, the only allocation of a
//! query together with the result rows) and the providers themselves, run by `query_all` in the order of 5.4.1.
//! Nothing here reads a file, the registry or the network; time is only counted (`Budget`), never asserted.

use super::arena::CatalogView;
use super::commands::{self, Command, COMMANDS};
use super::datetime::{self, TimeStyle};
use super::frecency::Frecency;
use super::gesture::GESTURE_TIME_MATCHES;
use super::index::{Hit, IndexView};
use super::model::{
    Batch, CommandId, FileClass, GlyphId, IconRef, Item, ItemKey, Kind, Mode, NumberStyle, Payload, ProviderId,
    ProviderMask, Query, Score, Verb, VerbSet, WindowRow,
};
use super::query::{self, Implicit, PrefixTable};
use super::rank::{self, Cand, Indexed};
use super::runline::{self, Source};
use super::strings;
use super::style;
use super::verbs::{verbs_for, verbs_for_file};
use super::web::{self, Template};
use crate::start_search::Keys;
use core::cmp::{Ordering, Reverse};
use std::time::Instant;

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
    /// The catalogues, read in place (L3 T16): names, word starts and the dictionaries of the hidden and fuzzy tiers. The
    /// settings view is empty while the settings are not held.
    pub apps: CatalogView<'a>,
    pub settings: CatalogView<'a>,
    /// One per window, most recently used first.
    pub windows: &'a [WindowRow],
    /// Run history, newest first.
    pub history: &'a [Box<str>],
    pub opts: &'a LauncherOpts,
    /// The locale's number marks, read at open.
    pub num: NumberStyle,
    /// What the user picked (L1b-3); the clock of the keystroke ages it.
    pub learned: &'a Frecency,
    pub now_unix: i64,
    /// The local time's offset from UTC in minutes, read at open (date and time rows).
    pub utc_offset_min: i32,
    /// The locale's 12/24-hour choice and date order, read at open.
    pub time_style: TimeStyle,
    /// The window in front when the chord came can take the window menu: there, not the desktop, not one of the dock's
    /// own windows. Without one the Window menu row is disabled.
    pub window_target: bool,
}

/// The best rows of a query: a fixed capacity of 3 x the visible rows, kept in result order.
#[derive(Debug)]
pub struct TopK {
    cap: usize,
    items: Vec<Item>,
}

/// Result order: the `Score` (5.2.5), then alphabetical (case-insensitive).
fn order(a: &Item, b: &Item) -> Ordering {
    by_rank((&a.score, &a.title), (&b.score, &b.title))
}

/// `order` on a score and a title, before any row is built.
fn by_rank(a: (&Score, &str), b: (&Score, &str)) -> Ordering {
    a.0.cmp(b.0).then_with(|| a.1.chars().flat_map(char::to_lowercase).cmp(b.1.chars().flat_map(char::to_lowercase)))
}

/// One provider's best `cap` candidates as (score, title, what builds the row), in `TopK`'s order and with its ties
/// (after their equals; at capacity only a better one enters). A row is built only for the candidates kept here, so a
/// bigger catalogue never builds more rows (LC-3); `best` is allocated once with room for `cap`.
fn keep_best<'a, T>(best: &mut Vec<(Score, &'a str, T)>, cap: usize, score: Score, title: &'a str, what: T) {
    if best.len() >= cap {
        match best.last() {
            Some(worst) if by_rank((&score, title), (&worst.0, worst.1)) == Ordering::Less => {
                best.pop();
            }
            _ => return,
        }
    }
    let at = best.partition_point(|x| by_rank((&x.0, x.1), (&score, title)) != Ordering::Greater);
    best.insert(at, (score, title, what));
}

impl TopK {
    pub fn new(rows: u8) -> TopK {
        let cap = 3 * usize::from(rows);
        TopK { cap, items: Vec::with_capacity(cap) }
    }

    #[cfg(test)]
    pub fn cap(&self) -> usize {
        self.cap
    }

    pub fn len(&self) -> usize {
        self.items.len()
    }

    #[cfg(test)]
    pub fn is_empty(&self) -> bool {
        self.items.is_empty()
    }

    #[cfg(test)]
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

// ---------------------------------------------------------------- the providers

/// The synchronous providers in the order of 5.4.1. In Mixed mode the open windows are merged into the apps; `Windows`
/// runs only in its own mode ("<").
pub const ORDER: [ProviderId; 9] = [
    ProviderId::Calc,
    ProviderId::DateTime,
    ProviderId::Apps,
    ProviderId::Windows,
    ProviderId::Commands,
    ProviderId::Settings,
    ProviderId::Run,
    ProviderId::Web,
    ProviderId::Registry,
];

/// The providers that rank candidates by tiers: only they take part in the fuzzy (tier 8) pass.
const RANKED: [ProviderId; 5] =
    [ProviderId::Apps, ProviderId::Windows, ProviderId::Commands, ProviderId::Settings, ProviderId::Registry];

/// Live check 6 (5.4.6): until it passes, executable file rows offer neither Open nor Run as administrator.
pub const MOTW_PROMPT_CONFIRMED: bool = false;

/// "<" mode lists at most this many windows (5.4.3).
const MAX_WINDOW_ROWS: usize = 128;

// Kind ranks inside a tier (5.2.5): app, window, command, setting, run line, folder, file.
const KIND_APP: u8 = 0;
const KIND_WINDOW: u8 = 1;
const KIND_COMMAND: u8 = 2;
const KIND_SETTING: u8 = 3;
const KIND_RUN: u8 = 4;
const KIND_FOLDER: u8 = 5;
const KIND_FILE: u8 = 6;

/// Rows that the merge order places rather than the ranking (the answer, a typed address, the web row) carry the best
/// score, so a full `TopK` never drops them.
const PINNED: Score = Score { tier: 0, learned: Reverse(u16::MAX), idle: false, kind: 0, rank: 0, len: 0 };

/// In Mixed mode at most this many settings rows that only a hidden word matched (tiers 4, 6 and 7).
const HIDDEN_SETTINGS_CAP: usize = 3;

#[cfg(test)]
thread_local! {
    /// The fuzzy passes `query_all` ran on this thread (L3 T8: none for a query that already has an answer).
    pub(crate) static FUZZY_PASSES: std::cell::Cell<u64> = const { std::cell::Cell::new(0) };
}

/// A provider's time budget per keystroke (5.4.1), counted in every build and never asserted (house rule 8).
pub const BUDGET_US: u64 = 1_000;

/// Overruns of the budget in a row, one counter per provider.
#[derive(Clone, Copy, Default, PartialEq, Eq, Debug)]
pub struct Budget {
    over_in_row: [u8; 11],
}

impl Budget {
    /// Counts one run of `id`: over the budget increments its counter, within it resets it. The third overrun in a
    /// row returns true once (the caller logs it) and starts counting again.
    pub fn record(&mut self, id: ProviderId, elapsed_us: u64) -> bool {
        let Some(n) = self.over_in_row.get_mut(id as usize) else { return false };
        if elapsed_us <= BUDGET_US {
            *n = 0;
            return false;
        }
        *n = n.saturating_add(1);
        if *n >= 3 {
            *n = 0;
            return true;
        }
        false
    }
}

/// The key of the web row (and of its "??" hint): the merge order puts it last and selects it only when alone.
pub fn web_search_key() -> ItemKey {
    ItemKey::of(ProviderId::Web, "search")
}

/// Whether `id` runs for this query: switched on, and in Mixed mode or its own mode. In another provider's mode the
/// apps still run while a word prefix is active (at tier 1 or better, `apps`).
fn runs(id: ProviderId, q: &Query, mask: ProviderMask) -> bool {
    if !mask.has(id) {
        return false;
    }
    match q.mode {
        Mode::Mixed => id != ProviderId::Windows,
        Mode::Only(p) => p == id || (id == ProviderId::Apps && q.apps_also),
    }
}

/// Every provider that runs for this query, in the order of 5.4.1, into one `TopK`. Tier 8 runs only when no
/// ranked candidate matched at tiers 0-7. Each provider is timed against its budget.
pub fn query_all(q: &Query, cx: &Context, budget: &mut Budget) -> TopK {
    query_all_traced(q, cx, budget, &mut |_| {})
}

/// `query_all`, telling `trace` each provider it calls (the order and mask tests).
fn query_all_traced(q: &Query, cx: &Context, budget: &mut Budget, trace: &mut dyn FnMut(ProviderId)) -> TopK {
    let mut top = TopK::new(cx.opts.rows);
    let mut s = Scratch::default();
    let mut spent = [0u64; 11];
    let mut ran = [false; 11];
    let mut hits = 0usize;
    for id in ORDER {
        if !runs(id, q, cx.opts.mask) {
            continue;
        }
        trace(id);
        let t = Instant::now();
        hits += run_one(id, q, cx, false, &mut top, &mut s);
        spend(&mut spent, &mut ran, id, t);
    }
    // Fuzzy is the last resort: nothing ranked matched and nothing else answered (a run, path, URL, answer or registry row
    // is an answer; the web row is not).
    let answered = top.items.iter().any(|i| i.key != web_search_key());
    if hits == 0 && !q.folded.is_empty() && !answered {
        #[cfg(test)]
        FUZZY_PASSES.with(|n| n.set(n.get() + 1));
        for id in ORDER {
            if RANKED.contains(&id) && ran.get(id as usize).copied().unwrap_or(false) {
                let t = Instant::now();
                run_one(id, q, cx, true, &mut top, &mut s);
                spend(&mut spent, &mut ran, id, t);
            }
        }
    }
    for id in ORDER {
        if !ran.get(id as usize).copied().unwrap_or(false) {
            continue;
        }
        let us = spent.get(id as usize).copied().unwrap_or(0);
        if budget.record(id, us) {
            crate::log!("launcher: provider {id:?} over budget 3 times in a row ({us} us)");
        }
    }
    top
}

fn spend(spent: &mut [u64; 11], ran: &mut [bool; 11], id: ProviderId, since: Instant) {
    let us = u64::try_from(since.elapsed().as_micros()).unwrap_or(u64::MAX);
    if let Some(total) = spent.get_mut(id as usize) {
        *total = total.saturating_add(us);
    }
    if let Some(r) = ran.get_mut(id as usize) {
        *r = true;
    }
}

/// Buffers reused across the candidates of one query: names indexed per keystroke never allocate per candidate.
#[derive(Default)]
struct Scratch {
    name: Indexed,
    title: Indexed,
    /// The dictionary's hidden matches of the query (L3 T9), sorted by entry.
    hidden: Vec<Hit>,
    /// The dictionary's near-prefix matches of the query for the fuzzy tier (L3 T10), sorted by entry.
    near: Vec<u32>,
}

/// The fuzzy tier over a catalogue with a dictionary: the query's characters, whether the subsequence test may run, and
/// the entries with a near-prefix match in `out`.
struct FuzzyQuery {
    chars: [char; 32],
    len: usize,
    subsequence: bool,
}

impl FuzzyQuery {
    fn chars(&self) -> &[char] {
        self.chars.get(..self.len).unwrap_or_default()
    }
}

/// `rank::match_fuzzy`'s work for the whole catalogue at once.
fn fuzzy_matches(v: IndexView, q: &str, out: &mut Vec<u32>) -> FuzzyQuery {
    out.clear();
    // A query of more than 32 characters has no fuzzy tier: nothing matches.
    let Some((chars, len)) = rank::fuzzy_chars(q) else {
        return FuzzyQuery { chars: ['\0'; 32], len: 0, subsequence: false };
    };
    let qc = chars.get(..len).unwrap_or_default();
    v.fuzzy(qc, rank::fuzzy_dist(len), out);
    // A character in no name rules the subsequence test out for every candidate.
    FuzzyQuery { chars, len, subsequence: v.foreign(qc) == 0 }
}

/// The fuzzy tier of entry `i` (`near` sorted by entry, `cur` moving forward): the subsequence test on its shown name or a
/// near-prefix match of one of its names.
fn fuzzy_of(f: &FuzzyQuery, c: &Cand, near: &[u32], cur: &mut usize, i: usize) -> Option<u8> {
    let i = u32::try_from(i).unwrap_or(u32::MAX);
    while near.get(*cur).is_some_and(|&e| e < i) {
        *cur += 1;
    }
    let by_near = near.get(*cur) == Some(&i);
    (by_near || f.subsequence && rank::subsequence_name(c, f.chars())).then_some(rank::T_FUZZY)
}

/// The dictionary answer for `q` over a catalogue, into `out`.
fn hidden_matches(v: IndexView, q: &str, out: &mut Vec<Hit>) {
    out.clear();
    let q = q.trim();
    v.hidden(q, q.chars().count(), out);
}

/// The hidden match of entry `i` in `hits` (sorted by entry); `cur` only moves forward, so a pass over the entries in order
/// costs one step per hit.
fn hidden_of(hits: &[Hit], cur: &mut usize, i: usize) -> Option<(u8, u32)> {
    let i = u32::try_from(i).unwrap_or(u32::MAX);
    while hits.get(*cur).is_some_and(|h| h.0 < i) {
        *cur += 1;
    }
    hits.get(*cur).filter(|h| h.0 == i).map(|h| (h.1, h.2))
}

/// Runs one provider; returns how many candidates it matched by tiers (0 for the providers that do not rank).
fn run_one(id: ProviderId, q: &Query, cx: &Context, fuzzy: bool, top: &mut TopK, s: &mut Scratch) -> usize {
    match id {
        ProviderId::Calc => {
            calc(q, top);
            0
        }
        ProviderId::Apps => apps(q, cx, fuzzy, top, s),
        ProviderId::Windows => windows(q, cx, fuzzy, top, s),
        ProviderId::Commands => commands(q, cx, fuzzy, top, s),
        ProviderId::Settings => settings(q, cx, fuzzy, top, s),
        ProviderId::Run => {
            run(q, cx, top);
            0
        }
        ProviderId::Web => {
            web(q, cx, top);
            0
        }
        ProviderId::Registry => registry(q, fuzzy, top, s),
        ProviderId::DateTime => {
            datetime(q, cx, top);
            0
        }
        ProviderId::Units | ProviderId::Files => 0,
    }
}

fn tier_of(c: &Cand, q: &str, fuzzy: bool) -> Option<u8> {
    if fuzzy {
        rank::match_fuzzy(c, q)
    } else {
        rank::match_tiers(c, q)
    }
}

/// The boost of a row the user picked before (L1b-3): nothing is hashed while nothing was learned, and a lookup
/// allocates nothing.
fn learned_boost(cx: &Context, provider: ProviderId, id: &str) -> Reverse<u16> {
    if cx.learned.is_empty() {
        return Reverse(0);
    }
    Reverse(cx.learned.boost(ItemKey::of(provider, id), cx.now_unix))
}

fn index_u32(i: usize) -> u32 {
    u32::try_from(i).unwrap_or(u32::MAX)
}

fn chars_u16(s: &str) -> u16 {
    u16::try_from(s.chars().count()).unwrap_or(u16::MAX)
}

fn verbs(kind: Kind, running: bool, class: Option<FileClass>) -> VerbSet {
    verbs_for(kind, running, class, MOTW_PROMPT_CONFIRMED, GESTURE_TIME_MATCHES)
}

/// A file row's verbs: Run as administrator only where the session can run its path elevated.
fn file_verbs(path: &str, class: FileClass) -> VerbSet {
    verbs_for_file(path, class, MOTW_PROMPT_CONFIRMED, GESTURE_TIME_MATCHES)
}

// ---- calculator

/// The answer row: in "=" mode for any expression, in Mixed mode only for one with an operator (a bare number does not
/// answer). No row for anything that does not evaluate.
fn calc(q: &Query, top: &mut TopK) {
    let text = q.text.trim();
    let wanted = match q.mode {
        Mode::Mixed => super::calc::is_implicit(text, q.num),
        Mode::Only(_) => !text.is_empty(),
    };
    if !wanted {
        return;
    }
    let Some(a) = super::calc::eval(text, q.num) else { return };
    let detail = match &a.thousands_note {
        Some(note) => format!("{} · {note}", a.normalised),
        None => a.normalised.clone(),
    };
    top.push(Item {
        key: ItemKey::of(ProviderId::Calc, "answer"),
        from: ProviderId::Calc,
        kind: Kind::Answer,
        title: a.shown.as_str().into(),
        detail: Some(detail.into()),
        icon: IconRef::Glyph(GlyphId::Equals),
        score: PINNED,
        verbs: verbs(Kind::Answer, false, None),
        payload: Payload::Value { shown: a.shown.into(), raw: a.raw.into() },
        destructive: false,
    });
}

// ---- date and time

/// ")" mode lists the eight rows in their order (each at its own rank, so they sort as listed); Mixed mode answers an
/// exact keyword with one row at the hidden-word tier with no learned boost, which a name always outranks.
fn datetime(q: &Query, cx: &Context, top: &mut TopK) {
    match q.mode {
        Mode::Only(ProviderId::DateTime) => {
            for i in 0..datetime::ROWS {
                let score = Score { rank: index_u32(i), ..PINNED };
                if let Some(r) = datetime::row(i, cx.now_unix, cx.utc_offset_min, cx.time_style) {
                    top.push(datetime_item(r, score));
                }
            }
        }
        Mode::Mixed => {
            let Some(i) = datetime::keyword_row(&q.folded) else { return };
            let score = Score { tier: rank::T_SYN_EQ, learned: Reverse(0), kind: KIND_COMMAND, ..PINNED };
            if let Some(r) = datetime::row(i, cx.now_unix, cx.utc_offset_min, cx.time_style) {
                top.push(datetime_item(r, score));
            }
        }
        Mode::Only(_) => {}
    }
}

fn datetime_item(r: datetime::Row, score: Score) -> Item {
    Item {
        key: ItemKey::of(ProviderId::DateTime, r.id),
        from: ProviderId::DateTime,
        kind: Kind::Answer,
        title: r.value.as_str().into(),
        detail: Some(r.label.into()),
        icon: IconRef::Glyph(GlyphId::Clock),
        score,
        verbs: verbs(Kind::Answer, false, None),
        payload: Payload::Value { shown: r.value.as_str().into(), raw: r.value.into() },
        destructive: false,
    }
}

// ---- apps and the open windows merged into them

/// The bar's group of this catalogue app: the same AUMID (ignoring case) or the same folded name as the bar's.
fn same_program(apps: &CatalogView, i: usize, w: &WindowRow) -> bool {
    w.aumid.as_deref().is_some_and(|a| a.eq_ignore_ascii_case(apps.id(i))) || *w.name_folded == *apps.cand(i).folded
}

fn app_item(apps: &CatalogView, i: usize, win: Option<&WindowRow>, score: Score) -> Item {
    let (id, name): (Box<str>, Box<str>) = (apps.id(i).into(), apps.name(i).into());
    Item {
        key: ItemKey::of(ProviderId::Apps, &id),
        from: ProviderId::Apps,
        kind: Kind::App,
        title: name,
        detail: None,
        // A running app keeps its catalogue icon, and with it its id (the session's Open, Reveal and Run as
        // administrator of a running app read it there).
        icon: IconRef::App(id.clone()),
        score,
        verbs: verbs(Kind::App, win.is_some(), None),
        payload: match win {
            Some(w) => Payload::Group(w.group_key.clone()),
            None => Payload::AppId(id),
        },
        destructive: false,
    }
}

/// The catalogue apps, each merged with its running group; then, when the windows provider is on, one Window row per
/// running program the catalogue does not know (its name as the bar shows it, never a title). While another
/// provider's word prefix is active, only matches at tier 1 or better on the raw text count ("file explorer").
fn apps(q: &Query, cx: &Context, fuzzy: bool, top: &mut TopK, s: &mut Scratch) -> usize {
    let raw_folded;
    let (text, max_tier): (&str, u8) = match q.mode {
        Mode::Only(p) if p != ProviderId::Apps => {
            let mut f = String::with_capacity(q.raw.len());
            rank::fold(q.raw.trim(), &mut f);
            raw_folded = f;
            (&raw_folded, rank::T_PREFIX)
        }
        _ => (&q.folded, u8::MAX),
    };
    let mut hits = 0;
    let mut best = Vec::with_capacity(top.cap);
    // The hidden tiers come from the dictionary.
    // With a cap at the names there is nothing hidden to read: those tiers could not pass the cap.
    let names_only = !fuzzy && max_tier != u8::MAX;
    let hidden_on = !fuzzy && !names_only;
    if hidden_on {
        hidden_matches(cx.apps.index(), text, &mut s.hidden);
    }
    let fz = (fuzzy && max_tier == u8::MAX).then(|| fuzzy_matches(cx.apps.index(), text, &mut s.near));
    let (mut at, mut near_at) = (0, 0);
    for i in 0..cx.apps.len() {
        let c = cx.apps.cand(i);
        let hid = if hidden_on { hidden_of(&s.hidden, &mut at, i) } else { None };
        let tier = if fuzzy {
            match &fz {
                Some(f) => fuzzy_of(f, &c, &s.near, &mut near_at, i),
                None => rank::match_fuzzy(&c, text),
            }
        } else {
            rank::match_tiers_with(&c, text, hid)
        };
        let Some(tier) = tier.filter(|t| *t <= max_tier) else { continue };
        hits += 1;
        let score_of = |running: bool| rank::score_hidden(&c, tier, running, KIND_APP, hid);
        // Look for the running window only when the row could stay: running is the better of the two scores.
        let learned = learned_boost(cx, ProviderId::Apps, cx.apps.id(i));
        if !top.accepts(&Score { learned, ..score_of(true) }) {
            continue;
        }
        let win = cx.windows.iter().position(|w| same_program(&cx.apps, i, w));
        let score = Score { learned, ..score_of(win.is_some()) };
        if top.accepts(&score) {
            keep_best(&mut best, top.cap, score, cx.apps.name(i), (i, win));
        }
    }
    for (score, _, (i, win)) in best {
        top.push(app_item(&cx.apps, i, win.and_then(|w| cx.windows.get(w)), score));
    }
    if !cx.opts.mask.has(ProviderId::Windows) {
        return hits;
    }
    let no_keys = Keys::default();
    let mut best = Vec::with_capacity(top.cap);
    for (i, w) in cx.windows.iter().enumerate().take(MAX_WINDOW_ROWS) {
        let seen = cx.windows.get(..i).is_some_and(|before| before.iter().any(|o| o.group_key == w.group_key));
        if seen {
            continue;
        }
        s.name.reset(&w.name);
        let c = s.name.cand(&no_keys, &[], index_u32(i));
        let Some(tier) = tier_of(&c, text, fuzzy).filter(|t| *t <= max_tier) else { continue };
        hits += 1;
        let score = c.score(tier, true, KIND_WINDOW);
        if !top.accepts(&score) || (0..cx.apps.len()).any(|i| same_program(&cx.apps, i, w)) {
            continue;
        }
        keep_best(&mut best, top.cap, score, &w.name, w);
    }
    for (score, _, w) in best {
        top.push(Item {
            key: ItemKey::of(ProviderId::Windows, &w.group_key),
            from: ProviderId::Windows,
            kind: Kind::Window,
            title: w.name.clone(),
            detail: None,
            icon: IconRef::Bar(w.group_key.clone()),
            score,
            verbs: verbs(Kind::Window, true, None),
            payload: Payload::Group(w.group_key.clone()),
            destructive: false,
        });
    }
    hits
}

/// "<" mode: one row per window in most-recently-used order (at most 128), the program name as the title and the
/// window title as the detail, matched on both. "<" alone lists them all.
fn windows(q: &Query, cx: &Context, fuzzy: bool, top: &mut TopK, s: &mut Scratch) -> usize {
    let no_keys = Keys::default();
    let mut hits = 0;
    for (i, w) in cx.windows.iter().enumerate().take(MAX_WINDOW_ROWS) {
        let tier = if q.folded.is_empty() {
            (!fuzzy).then_some(0)
        } else {
            s.name.reset(&w.name);
            let by_name = tier_of(&s.name.cand(&no_keys, &[], 0), &q.folded, fuzzy);
            let by_title = w.title.as_deref().and_then(|t| {
                s.title.reset(t);
                tier_of(&s.title.cand(&no_keys, &[], 0), &q.folded, fuzzy)
            });
            by_name.into_iter().chain(by_title).min()
        };
        let Some(tier) = tier else { continue };
        hits += 1;
        let score = Score {
            tier,
            learned: Reverse(0),
            idle: false,
            kind: KIND_WINDOW,
            rank: index_u32(i),
            len: chars_u16(&w.name),
        };
        if !top.accepts(&score) {
            continue;
        }
        let mut detail = String::from(w.title.as_deref().unwrap_or(""));
        if w.hung {
            detail.push_str(strings::not_responding());
        }
        top.push(Item {
            key: ItemKey::of(ProviderId::Windows, &format!("hwnd:{:x}", w.hwnd)),
            from: ProviderId::Windows,
            kind: Kind::Window,
            title: w.name.clone(),
            detail: (!detail.is_empty()).then(|| detail.into()),
            icon: IconRef::Bar(w.group_key.clone()),
            score,
            verbs: verbs(Kind::Window, true, None),
            payload: Payload::Hwnd(w.hwnd),
            destructive: false,
        });
    }
    hits
}

// ---- commands, registry, settings

fn command_glyph(id: CommandId) -> GlyphId {
    match id {
        CommandId::Lock => GlyphId::Lock,
        CommandId::Sleep => GlyphId::Moon,
        CommandId::Restart => GlyphId::Restart,
        CommandId::ShutDown => GlyphId::Power,
        CommandId::SignOut => GlyphId::Exit,
        CommandId::Registry => GlyphId::Gear,
        CommandId::WindowMenu => GlyphId::Window,
        CommandId::ClipboardHistory => GlyphId::Copy,
    }
}

fn command_item(cmd: &Command, from: ProviderId, title: &str, score: Score) -> Item {
    Item {
        key: ItemKey::of(from, cmd.en),
        from,
        kind: Kind::Command,
        title: title.into(),
        detail: None,
        icon: IconRef::Glyph(command_glyph(cmd.id)),
        score,
        verbs: verbs(Kind::Command, false, None),
        payload: Payload::Command(cmd.id),
        destructive: cmd.destructive,
    }
}

/// One command matched by its name in the dock language and by its keywords in both languages (rank `i`): its score,
/// None when it does not match.
fn command_score(cmd: &Command, i: usize, q: &Query, fuzzy: bool, s: &mut Scratch) -> Option<Score> {
    let no_keys = Keys::default();
    s.name.reset(cmd.name());
    let c = s.name.cand(&no_keys, cmd.keywords, index_u32(i));
    let tier = tier_of(&c, &q.folded, fuzzy)?;
    Some(rank::score_for(&c, &q.folded, tier, false, KIND_COMMAND))
}

/// One command row (`command_score`).
fn command_row(
    cmd: &Command,
    i: usize,
    from: ProviderId,
    q: &Query,
    fuzzy: bool,
    top: &mut TopK,
    s: &mut Scratch,
) -> usize {
    let Some(score) = command_score(cmd, i, q, fuzzy, s) else { return 0 };
    if top.accepts(&score) {
        top.push(command_item(cmd, from, cmd.name(), score));
    }
    1
}

/// The system commands; Registry Editor is the registry provider's. Window menu is disabled (no Enter, the reason as
/// its detail) when the chord came over no window it can act on.
fn commands(q: &Query, cx: &Context, fuzzy: bool, top: &mut TopK, s: &mut Scratch) -> usize {
    let mut hits = 0;
    for (i, cmd) in COMMANDS.iter().enumerate().filter(|(_, c)| c.id != CommandId::Registry) {
        let Some(mut score) = command_score(cmd, i, q, fuzzy, s) else { continue };
        hits += 1;
        if !cmd.destructive {
            score.learned = learned_boost(cx, ProviderId::Commands, cmd.en);
        }
        if top.accepts(&score) {
            let mut item = command_item(cmd, ProviderId::Commands, cmd.name(), score);
            if cmd.id == CommandId::WindowMenu && !cx.window_target {
                item.verbs = item.verbs.without(Verb::Open);
                item.detail = Some(strings::no_window_to_act_on().into());
            }
            top.push(item);
        }
    }
    hits
}

/// ":" shows one row "Open Registry Editor" whatever follows; in Mixed mode the command is matched like the others.
fn registry(q: &Query, fuzzy: bool, top: &mut TopK, s: &mut Scratch) -> usize {
    let Some(cmd) = commands::command(CommandId::Registry) else { return 0 };
    match q.mode {
        Mode::Mixed => command_row(cmd, COMMANDS.len(), ProviderId::Registry, q, fuzzy, top, s),
        Mode::Only(_) if fuzzy => 0,
        Mode::Only(_) => {
            top.push(command_item(cmd, ProviderId::Registry, strings::open_registry_editor(), PINNED));
            1
        }
    }
}

/// The settings catalogue, already folded and split at load: nothing is folded per candidate.
fn settings(q: &Query, cx: &Context, fuzzy: bool, top: &mut TopK, s: &mut Scratch) -> usize {
    let mut hits = 0;
    let mut best = Vec::with_capacity(top.cap);
    // Mixed mode: the rows only a hidden word matched are the best HIDDEN_SETTINGS_CAP of their own (L3 T6); "$" has no cap.
    let capped = q.mode == Mode::Mixed;
    let mut hidden = Vec::with_capacity(if capped { HIDDEN_SETTINGS_CAP } else { 0 });
    if !fuzzy {
        hidden_matches(cx.settings.index(), &q.folded, &mut s.hidden);
    }
    let fz = fuzzy.then(|| fuzzy_matches(cx.settings.index(), &q.folded, &mut s.near));
    let (mut at, mut near_at) = (0, 0);
    for i in 0..cx.settings.len() {
        let c = cx.settings.cand(i);
        let hid = if fuzzy { None } else { hidden_of(&s.hidden, &mut at, i) };
        let tier = match &fz {
            Some(f) => fuzzy_of(f, &c, &s.near, &mut near_at, i),
            None => rank::match_tiers_with(&c, &q.folded, hid),
        };
        let Some(tier) = tier else { continue };
        hits += 1;
        let learned = learned_boost(cx, ProviderId::Settings, cx.settings.name(i));
        let score = Score { learned, ..rank::score_hidden(&c, tier, false, KIND_SETTING, hid) };
        if capped && matches!(tier, rank::T_SYN_EQ | rank::T_SYN_PREFIX | rank::T_BAG_EQ) {
            keep_best(&mut hidden, HIDDEN_SETTINGS_CAP, score, cx.settings.name(i), i);
        } else if top.accepts(&score) {
            keep_best(&mut best, top.cap, score, cx.settings.name(i), i);
        }
    }
    best.extend(hidden);
    for (score, _, i) in best {
        let name = cx.settings.name(i);
        top.push(Item {
            key: ItemKey::of(ProviderId::Settings, name),
            from: ProviderId::Settings,
            kind: Kind::Setting,
            title: name.into(),
            detail: None,
            icon: IconRef::Glyph(GlyphId::Gear),
            score,
            verbs: verbs(Kind::Setting, false, None),
            payload: Payload::Setting(u32::try_from(i).unwrap_or(u32::MAX)),
            destructive: false,
        });
    }
    hits
}

// ---- run lines and typed paths

/// The class of a typed path by its extension alone (5.4.6); L2's files provider adds the document, image and archive
/// classes.
pub fn file_class(path: &str) -> FileClass {
    const EXECUTABLE: [&str; 12] =
        ["exe", "com", "bat", "cmd", "ps1", "vbs", "js", "msi", "scr", "lnk", "url", "appref-ms"];
    let name = path.rsplit(['\\', '/']).next().unwrap_or(path);
    match name.rsplit_once('.') {
        Some((_, ext)) if EXECUTABLE.iter().any(|e| e.eq_ignore_ascii_case(ext)) => FileClass::Executable,
        _ => FileClass::Other,
    }
}

fn run_detail(line: &str) -> Box<str> {
    let mut d = String::from(strings::run_by_windows());
    d.push_str(line);
    d.into()
}

fn run_item(line: super::model::RunLine, kind: Kind, detail: Box<str>, verbs: VerbSet, score: Score) -> Item {
    Item {
        key: ItemKey::of(ProviderId::Run, &line.line),
        from: ProviderId::Run,
        kind,
        title: line.line.clone(),
        detail: Some(detail),
        icon: IconRef::Glyph(GlyphId::Prompt),
        score,
        verbs,
        payload: Payload::RunLine(line),
        destructive: false,
    }
}

/// ">" mode: the typed line, then history and known commands (`runline::suggest`, in its order). Mixed mode: a line
/// whose first word is a known command gets a run row below the app rows, and a typed path or address an Open row.
/// All by syntax only: nothing is looked up on disk.
fn run(q: &Query, cx: &Context, top: &mut TopK) {
    let text = q.text.trim();
    if let Mode::Only(_) = q.mode {
        let mut out = Vec::new();
        runline::suggest(text, cx.history, &mut out);
        for (i, sug) in out.iter().enumerate() {
            let Some(line) = runline::classify(&sug.text) else { continue };
            let score = Score { tier: 0, learned: Reverse(0), idle: true, kind: KIND_RUN, rank: index_u32(i), len: 0 };
            let (kind, detail, vs) = match sug.source {
                Source::Typed => (Kind::RunLine, run_detail(&line.line), verbs(Kind::RunLine, false, None)),
                Source::History => {
                    (Kind::RunSuggestion, run_detail(&line.line), verbs(Kind::RunSuggestion, false, None))
                }
                // A known command is not history: nothing to forget.
                Source::Known(k) => (
                    Kind::RunSuggestion,
                    k.description().into(),
                    verbs(Kind::RunSuggestion, false, None).without(Verb::Forget),
                ),
            };
            if top.accepts(&score) {
                top.push(run_item(line, kind, detail, vs, score));
            }
        }
        return;
    }
    match query::implicit(text, &|w| runline::known_for(w).is_some()) {
        Some(Implicit::Command) => {
            let Some(line) = runline::classify(text) else { return };
            // `Results::sync` places it: right after a top hit that matched by a name, else first. Pinned, so a full
            // `TopK` never drops it.
            let score = Score { kind: KIND_RUN, ..PINNED };
            if top.accepts(&score) {
                let detail = run_detail(&line.line);
                top.push(run_item(line, Kind::RunLine, detail, verbs(Kind::RunLine, false, None), score));
            }
        }
        Some(Implicit::Path) => {
            let dir = names_folder(text);
            let (kind, class, glyph) = match (dir, file_class(text)) {
                (true, _) => (Kind::Folder, FileClass::Other, GlyphId::Folder),
                (false, FileClass::Executable) => (Kind::File, FileClass::Executable, GlyphId::Executable),
                (false, class) => (Kind::File, class, GlyphId::Document),
            };
            let score = Score { kind: if dir { KIND_FOLDER } else { KIND_FILE }, ..PINNED };
            top.push(Item {
                key: ItemKey::of(ProviderId::Run, text),
                from: ProviderId::Run,
                kind,
                title: text.into(),
                detail: None,
                icon: IconRef::Glyph(glyph),
                score,
                verbs: if dir { verbs(kind, false, Some(class)) } else { file_verbs(text, class) },
                payload: Payload::Path { path: text.into(), dir, class },
                destructive: false,
            });
        }
        Some(Implicit::Url) | None => {}
    }
}

/// A typed path names a folder when it ends in a separator or its last part has no dot (`C:\Windows`, `shell:startup`).
/// Syntax only: no file-system call (5.4.9). A settings URI is never a folder.
pub fn names_folder(path: &str) -> bool {
    if path.ends_with(['\\', '/']) {
        return true;
    }
    if path.get(..12).is_some_and(|p| p.eq_ignore_ascii_case("ms-settings:")) {
        return false;
    }
    let name = path.rsplit(['\\', '/', ':']).next().unwrap_or(path);
    !name.is_empty() && !name.contains('.')
}

// ---- web

fn web_item(key: ItemKey, title: String, url: Option<String>) -> Item {
    Item {
        key,
        from: ProviderId::Web,
        kind: Kind::Web,
        title: title.into(),
        detail: None,
        icon: IconRef::Glyph(GlyphId::Globe),
        score: PINNED,
        verbs: if url.is_some() { verbs(Kind::Web, false, None) } else { VerbSet::empty() },
        payload: url.map_or(Payload::None, |u| Payload::Url(u.into())),
        destructive: false,
    }
}

/// A typed address gets "Open"; from 3 characters (from 1 in "??" mode) the web row names the engine's host. "??"
/// alone shows a hint row that does nothing.
fn web(q: &Query, cx: &Context, top: &mut TopK) {
    let text = q.text.trim();
    let host = cx.opts.web.host();
    let search = |top: &mut TopK| {
        let url = web::url_for(&cx.opts.web, text);
        top.push(web_item(web_search_key(), strings::search_web(host, text), Some(url)));
    };
    match q.mode {
        Mode::Mixed => {
            if query::implicit(text, &|_| false) == Some(Implicit::Url) {
                if let Some(url) = web::parse_typed_url(text) {
                    top.push(web_item(ItemKey::of(ProviderId::Web, "url"), url.clone(), Some(url)));
                }
            }
            if text.chars().count() >= 3 {
                search(top);
            }
        }
        Mode::Only(_) if text.is_empty() => top.push(web_item(web_search_key(), strings::type_to_search(host), None)),
        Mode::Only(_) => search(top),
    }
}

// ---------------------------------------------------------------- results

fn is_executable_file(r: &Item) -> bool {
    matches!(r.payload, Payload::Path { dir: false, class: FileClass::Executable, .. })
}

/// The rows of the current query in the merge order of 5.2.6 (notices, the answer, the ranked rows with the implicit run
/// row after the top hit, late rows) and the selection, kept by `ItemKey`. The web row is not a ranked row: it has its
/// own slot, the position `min(n, visible - 1)` among the `n` other rows, so it is on screen however many rows match.
/// Late rows are only ever added below the synchronous rows and never move a row at or above the selection; every row
/// belongs to the generation of the last `sync`.
#[derive(Debug, Default)]
pub struct Results {
    /// The view order, the web row included at `web_at`.
    rows: Vec<Item>,
    sel: Option<ItemKey>,
    gen: u64,
    /// Notices, the answer and the ranked rows (the web row not counted): the rows of the keystroke's own frame.
    sync_len: usize,
    /// Late rows right after them (the web row not counted).
    late_len: usize,
    /// Where the web row sits in `rows`.
    web_at: Option<usize>,
    /// The rows the view shows (`LauncherOpts::rows`), as the last `sync` was given them.
    visible: usize,
    /// The query was in Mixed mode, where an executable file row is never the default.
    mixed: bool,
    /// The field holds text: a notice's choice is never the default then, so Enter runs what was typed for.
    typed: bool,
    notices: Vec<Item>,
}

impl Results {
    /// The notice rows (5.5.2, 5.7), shown first from the next `sync`.
    pub fn set_notices(&mut self, notices: Vec<Item>) {
        self.notices = notices;
    }

    /// One more notice row, after the others (it replaces one with the same key), shown from the next `sync`.
    pub fn push_notice(&mut self, notice: Item) {
        self.notices.retain(|n| n.key != notice.key);
        self.notices.push(notice);
    }

    pub fn rows(&self) -> &[Item] {
        &self.rows
    }

    pub fn selected(&self) -> Option<&Item> {
        let key = self.sel?;
        self.rows.iter().find(|r| r.key == key)
    }

    pub fn selected_index(&self) -> Option<usize> {
        let key = self.sel?;
        self.rows.iter().position(|r| r.key == key)
    }

    /// Selects the row at `i` (arrows, pointer); false when there is none.
    pub fn select_index(&mut self, i: usize) -> bool {
        match self.rows.get(i) {
            Some(r) => {
                self.sel = Some(r.key);
                true
            }
            None => false,
        }
    }

    /// Whether anything but notices and the web row answered: when nothing did, the typo rewrite is worth asking.
    pub fn has_matches(&self) -> bool {
        self.rows.iter().any(|r| r.kind != Kind::Notice && r.key != web_search_key())
    }

    /// Where the web row goes among `n` other rows: after them, but never below the last visible row.
    fn web_slot(&self, n: usize) -> usize {
        n.min(self.visible.saturating_sub(1))
    }

    /// Puts the web row (when there is one) at its slot among the other rows. It only ever moves down: the other rows
    /// only grow in number between two syncs.
    fn place_web(&mut self) {
        let Some(w) = self.web_at else { return };
        if w >= self.rows.len() {
            return;
        }
        let item = self.rows.remove(w);
        let at = self.web_slot(self.rows.len());
        self.rows.insert(at, item);
        self.web_at = Some(at);
    }

    /// The position in `rows` of the `k`-th row that is not the web row.
    fn row_of_other(&self, k: usize) -> usize {
        match self.web_at {
            Some(w) if k >= w => k + 1,
            _ => k,
        }
    }

    /// `rows.insert`, keeping `web_at` right.
    fn insert_row(&mut self, at: usize, item: Item) {
        self.rows.insert(at, item);
        if let Some(w) = self.web_at.as_mut() {
            if at <= *w {
                *w += 1;
            }
        }
    }

    /// The synchronous rows of a keystroke; `visible` is the number of rows the view shows. A new generation drops every
    /// older row and takes the default selection; the same generation again (a refresh) keeps its late rows and the
    /// selected key when it is still there.
    pub fn sync(&mut self, q: &Query, top: TopK, visible: usize) {
        let same = q.gen == self.gen;
        let keep = if same { self.sel } else { None };
        let late = if same { self.take_late() } else { Vec::new() };
        self.visible = visible.max(1);
        self.rows.clear();
        self.web_at = None;
        self.rows.extend(self.notices.iter().cloned());
        let mut web = None;
        let mut ranked = Vec::with_capacity(top.len());
        for item in top.into_sorted() {
            if item.key == web_search_key() {
                web = Some(item);
            } else if item.kind == Kind::Answer && item.score.tier == PINNED.tier {
                // Only an answer of the best tier is lifted above the ranked rows; any other stays among them.
                self.rows.push(item);
            } else {
                ranked.push(item);
            }
        }
        if q.mode == Mode::Mixed {
            // The implicit run row follows a top hit that matched by a name (tiers 0-2), else it is the first ranked row.
            if let Some(i) = ranked.iter().position(|r| r.from == ProviderId::Run && r.kind == Kind::RunLine) {
                let run = ranked.remove(i);
                let named_top = ranked.first().is_some_and(|r| r.score.tier <= rank::T_WORD);
                ranked.insert(usize::from(named_top).min(ranked.len()), run);
            }
        }
        self.rows.append(&mut ranked);
        self.sync_len = self.rows.len();
        self.late_len = late.len();
        self.rows.extend(late);
        if let Some(w) = web {
            self.rows.push(w);
            self.web_at = Some(self.rows.len() - 1);
            self.place_web();
        }
        self.gen = q.gen;
        self.mixed = q.mode == Mode::Mixed;
        self.typed = !q.raw.is_empty();
        self.sel = keep.filter(|k| self.rows.iter().any(|r| r.key == *k)).or_else(|| self.default_key(q));
    }

    /// The rows added after the last `sync`, the web row taken out (it is placed again by the next `sync`).
    fn take_late(&mut self) -> Vec<Item> {
        if let Some(w) = self.web_at.take() {
            if w < self.rows.len() {
                self.rows.remove(w);
            }
        }
        let start = self.sync_len.min(self.rows.len());
        self.rows.drain(start..).collect()
    }

    fn may_default(&self, r: &Item) -> bool {
        let blocked = (self.mixed && is_executable_file(r)) || (self.typed && r.kind == Kind::Notice);
        !r.verbs.is_empty() && !blocked
    }

    /// The first row that may be the default (destructive rows by the rule of 5.2.7, executable files in Mixed mode and
    /// rows with no action skipped); the web row only when nothing else answered.
    fn default_key(&self, q: &Query) -> Option<ItemKey> {
        let web = web_search_key();
        let first =
            self.rows.iter().filter(|r| r.key != web).find(|r| self.may_default(r) && rank::default_allowed(r, q));
        if let Some(r) = first {
            return Some(r.key);
        }
        let alone = self.rows.iter().all(|r| r.key == web || r.kind == Kind::Notice);
        self.rows.iter().find(|r| alone && r.key == web && !r.verbs.is_empty()).map(|r| r.key)
    }

    /// A late batch (files, L2). False, and nothing changes, unless it is of the current generation. Its rows go below
    /// the synchronous and earlier late rows and before the web row's slot (the web row moves down while it is the last
    /// row, and stays at the last visible row once the rows fill the view), unless the selection is at or below the web
    /// row: then they go after everything and nothing moves. A row already shown is not added twice. With nothing
    /// selected, the first late row that may be the default is.
    pub fn late(&mut self, b: Batch, current_gen: u64) -> bool {
        if b.gen != current_gen || b.gen != self.gen {
            return false;
        }
        let below_web = match (self.web_at, self.selected_index()) {
            (Some(w), Some(i)) => i >= w,
            _ => false,
        };
        let mut at = if below_web { self.rows.len() } else { self.row_of_other(self.sync_len + self.late_len) };
        let from = at;
        for item in b.items {
            if self.rows.iter().any(|r| r.key == item.key) {
                continue;
            }
            self.insert_row(at, item);
            at += 1;
            if !below_web {
                self.late_len += 1;
            }
        }
        if self.sel.is_none() {
            let added = self.rows.get(from..at).unwrap_or(&[]);
            self.sel = added.iter().find(|r| !r.destructive && self.may_default(r)).map(|r| r.key);
        }
        if !below_web {
            self.place_web();
        }
        true
    }

    /// Takes a row out (a closed window, a forgotten line, an answered notice, which stays out for the rest of the open).
    /// The selection moves to the row that takes its place only when that row may be the default and is not
    /// destructive (5.2.7): nobody chose it, and Enter runs what is selected. Otherwise nothing is selected.
    pub fn remove(&mut self, key: ItemKey) -> Option<Item> {
        self.notices.retain(|n| n.key != key);
        let i = self.rows.iter().position(|r| r.key == key)?;
        let was_other = self.web_at != Some(i);
        let k = match self.web_at {
            Some(w) if i > w => i - 1,
            _ => i,
        };
        let item = self.rows.remove(i);
        match self.web_at {
            Some(w) if w == i => self.web_at = None,
            Some(w) if i < w => self.web_at = Some(w - 1),
            _ => {}
        }
        if was_other {
            if k < self.sync_len {
                self.sync_len -= 1;
            } else if k < self.sync_len + self.late_len {
                self.late_len -= 1;
            }
            // The web row only moves down, so only while the selection is above it.
            let selected_below = match (self.web_at, self.selected_index()) {
                (Some(w), Some(s)) => s >= w,
                _ => false,
            };
            if !selected_below {
                self.place_web();
            }
        }
        if self.sel == Some(key) {
            let next = self.rows.get(i).or_else(|| self.rows.last());
            self.sel = next.filter(|r| !r.destructive && self.may_default(r)).map(|r| r.key);
        }
        Some(item)
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::indexing_slicing)]
pub(crate) mod tests {
    use super::*;
    use crate::launcher::arena::{self, Arena};
    use crate::launcher::catalog::CatApp;
    use crate::launcher::model::SettingRow;

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

    // ---- the fixture every provider, results and session test uses

    pub(crate) const NUM_IT: NumberStyle = NumberStyle { decimal: ',', group: '.' };
    pub(crate) const NOTEPAD_KEY: &str = "c:\\windows\\system32\\notepad.exe";
    pub(crate) const FOOTOOL_KEY: &str = "c:\\tools\\footool.exe";

    /// Calcolatrice, Blocco note, Visual Studio Code, Steam, Esplora file; the settings "Accessibilità" and "Schermo"; a
    /// running Notepad (bar name "Blocco note") and an unmatched running FooTool; two history lines.
    pub(crate) fn fixture() -> (Vec<CatApp>, Vec<SettingRow>, Vec<WindowRow>, Vec<Box<str>>) {
        let apps = vec![
            CatApp::new("Microsoft.WindowsCalculator_8wekyb3d8bbwe!App", "Calcolatrice", Keys::default(), 10),
            CatApp::new("Microsoft.WindowsNotepad_8wekyb3d8bbwe!App", "Blocco note", Keys::default(), 20),
            CatApp::new("Microsoft.VisualStudioCode", "Visual Studio Code", Keys::default(), 30),
            CatApp::new("{7C5A40EF-A0FB-4BFC-874A-C0F2E0B9FA8E}\\Steam\\steam.exe", "Steam", Keys::default(), 40),
            CatApp::new("Microsoft.Windows.Explorer", "Esplora file", Keys::default(), 5),
        ];
        let settings = vec![
            SettingRow::new(0, "Accessibilità", Keys::default(), 1),
            SettingRow::new(1, "Schermo", Keys::from(&[("display", 0), ("monitor", 1)][..]), 2),
        ];
        let window = |hwnd: isize, key: &str, name: &str, title: &str| {
            let mut folded = String::new();
            rank::fold(name, &mut folded);
            WindowRow {
                hwnd,
                group_key: key.into(),
                aumid: None,
                name: name.into(),
                name_folded: folded.into(),
                title: Some(title.into()),
                hung: false,
            }
        };
        let windows = vec![
            window(0x1001, NOTEPAD_KEY, "Blocco note", "Senza titolo - Blocco note"),
            window(0x2002, FOOTOOL_KEY, "FooTool", "FooTool - main"),
        ];
        let history = vec!["ping 1.1.1.1".into(), "notepad c:\\temp\\a.txt".into()];
        (apps, settings, windows, history)
    }

    /// The fixture and the options, owned, with a `Context` over them.
    pub(crate) struct Fx {
        pub apps: Vec<CatApp>,
        pub settings: Vec<SettingRow>,
        pub windows: Vec<WindowRow>,
        pub history: Vec<Box<str>>,
        pub opts: LauncherOpts,
        pub learned: Frecency,
    }

    impl Fx {
        pub(crate) fn new() -> Fx {
            let (apps, settings, windows, history) = fixture();
            Fx { apps, settings, windows, history, opts: LauncherOpts::default(), learned: Frecency::new() }
        }

        pub(crate) fn cx(&self) -> Context<'_> {
            // The tests change `apps` and `settings` between keystrokes: the arenas are rebuilt (and leaked: a test process
            // is short) for every context, so they always match the rows.
            let apps: &'static Arena = Box::leak(Box::new(arena::of_apps(&self.apps)));
            let settings: &'static Arena = Box::leak(Box::new(arena::of_settings(&self.settings)));
            Context {
                apps: apps.view(),
                settings: settings.view(),
                windows: &self.windows,
                history: &self.history,
                opts: &self.opts,
                num: NUM_IT,
                now_unix: 1_790_000_000,
                utc_offset_min: 60,
                time_style: TimeStyle::default(),
                learned: &self.learned,
                window_target: true,
            }
        }

        /// The rows of a query and the providers that were called.
        pub(crate) fn query(&self, raw: &str) -> (Vec<Item>, Vec<ProviderId>) {
            let q = query::parse_query(raw, &self.opts.prefixes, NUM_IT, 1);
            let mut called = Vec::new();
            let top = query_all_traced(&q, &self.cx(), &mut Budget::default(), &mut |id| called.push(id));
            (top.into_sorted(), called)
        }

        /// The rows without the web row (which the merge order puts last, 8c).
        pub(crate) fn rows(&self, raw: &str) -> Vec<Item> {
            self.query(raw).0.into_iter().filter(|r| r.key != web_search_key()).collect()
        }
    }

    fn titled<'a>(rows: &'a [Item], title: &str) -> Vec<&'a Item> {
        rows.iter().filter(|r| &*r.title == title).collect()
    }

    // ---- results

    impl Fx {
        /// The results of a query of generation `gen`.
        pub(crate) fn sync(&self, res: &mut Results, raw: &str, gen: u64) {
            let q = query::parse_query(raw, &self.opts.prefixes, NUM_IT, gen);
            res.sync(&q, query_all(&q, &self.cx(), &mut Budget::default()), usize::from(self.opts.rows));
        }
    }

    pub(crate) fn file_row(name: &str, class: FileClass) -> Item {
        let path = format!("c:\\x\\{name}");
        Item {
            key: ItemKey::of(ProviderId::Files, name),
            from: ProviderId::Files,
            kind: Kind::File,
            title: name.into(),
            detail: None,
            icon: IconRef::Glyph(GlyphId::Document),
            score: PINNED,
            verbs: file_verbs(&path, class),
            payload: Payload::Path { path: path.into(), dir: false, class },
            destructive: false,
        }
    }

    fn keys(res: &Results) -> Vec<ItemKey> {
        res.rows().iter().map(|r| r.key).collect()
    }

    fn files(gen: u64, names: &[&str]) -> Batch {
        let items = names.iter().map(|n| file_row(n, FileClass::Other)).collect();
        Batch { gen, from: ProviderId::Files, items, done: true }
    }

    #[test]
    fn stability_merge_with_a_late_provider() {
        let fx = Fx::new();
        let mut res = Results::default();
        fx.sync(&mut res, "st", 1);
        let titles: Vec<&str> = res.rows().iter().map(|r| &*r.title).collect();
        // "standby" is a name of Sleep (L3 T5), so it starts with "st" like Steam does.
        assert_eq!(titles, ["Steam", "Sleep", "Visual Studio Code", "Restart", "Registry Editor"]);
        assert_eq!(res.selected().unwrap().title.as_ref(), "Steam");
        assert!(res.select_index(2), "the user moves to Visual Studio Code");
        let before = keys(&res);
        assert!(res.late(files(1, &["budget.ods", "studio.txt"]), 1));
        let after = keys(&res);
        assert_eq!(after[..5], before[..], "no row at or above the selection moved");
        assert_eq!(after.len(), 7);
        assert_eq!(res.selected().unwrap().title.as_ref(), "Visual Studio Code", "Enter acts on the selected row");
        // Late rows go above the web row...
        fx.sync(&mut res, "stea", 2);
        assert_eq!(res.rows().last().unwrap().key, web_search_key());
        assert!(res.late(files(2, &["steam.log"]), 2));
        assert_eq!(res.rows().last().unwrap().key, web_search_key());
        assert_eq!(res.rows()[res.rows().len() - 2].title.as_ref(), "steam.log");
        // ... unless the web row is selected: then below it, and it stays where it was.
        fx.sync(&mut res, "zzqq", 3);
        assert_eq!(res.selected().unwrap().key, web_search_key());
        assert!(res.late(files(3, &["zzqq.txt"]), 3));
        assert_eq!(res.rows()[0].key, web_search_key());
        assert_eq!(res.selected().unwrap().key, web_search_key());
        // A row already there is not added twice.
        assert!(res.late(files(3, &["zzqq.txt"]), 3));
        assert_eq!(res.rows().len(), 2);
    }

    fn web_index(res: &Results) -> usize {
        res.rows().iter().position(|r| r.key == web_search_key()).unwrap()
    }

    #[test]
    fn run_row_after_the_top_name_hit() {
        let mut fx = Fx::new();
        fx.apps = vec![CatApp::new("Microsoft.WindowsCalculator_8wekyb3d8bbwe!App", "Calculator", Keys::default(), 5)];
        for i in 0..11 {
            fx.apps.push(CatApp::new(&format!("Calc{i}.exe"), &format!("Calc {i:02}"), Keys::default(), 10 + i));
        }
        let mut res = Results::default();
        fx.sync(&mut res, "calc", 1);
        assert!(res.rows().len() > 12, "twelve apps and the run row: {}", res.rows().len());
        assert_eq!(res.rows()[0].title.as_ref(), "Calculator");
        assert_eq!((res.rows()[1].kind, res.rows()[1].title.as_ref()), (Kind::RunLine, "calc"));
        assert_eq!(res.selected_index(), Some(0), "the name hit stays the default");
        assert_eq!(web_index(&res), 7, "the web row is the last of the 8 visible");
    }

    #[test]
    fn run_row_first_after_a_hidden_top() {
        let mut fx = Fx::new();
        fx.apps = vec![CatApp::new(
            "{1AC14E77-02E7-4E5D-B744-2EB1AE5198B7}\\cmd.exe",
            "Command Prompt",
            Keys::from(&[("ipconfig", 1_500)][..]),
            5,
        )];
        let mut res = Results::default();
        fx.sync(&mut res, "ipconfig", 1);
        assert_eq!((res.rows()[0].kind, res.rows()[0].title.as_ref()), (Kind::RunLine, "ipconfig"));
        assert_eq!(res.rows()[1].title.as_ref(), "Command Prompt", "found by its synonym only");
        assert_eq!(res.selected_index(), Some(0), "the run row is the default");
    }

    #[test]
    fn web_row_always_visible_from_3_chars() {
        let mut fx = Fx::new();
        fx.apps = (0..20)
            .map(|i| CatApp::new(&format!("Zeta{i}.exe"), &format!("Zeta {i:02}"), Keys::default(), i))
            .collect();
        let mut res = Results::default();
        fx.sync(&mut res, "zet", 1);
        assert_eq!(res.rows().len(), 21);
        assert_eq!(web_index(&res), 7, "row 8 of 8 whatever matched");
        assert_ne!(res.selected().unwrap().key, web_search_key());
        // Two matches: the web row follows them.
        fx.apps.truncate(2);
        fx.sync(&mut res, "zet", 2);
        assert_eq!(web_index(&res), 2);
        assert_eq!(res.selected_index(), Some(0));
    }

    #[test]
    fn late_rows_fill_up_to_the_web_slot() {
        let mut fx = Fx::new();
        fx.apps =
            (0..5).map(|i| CatApp::new(&format!("Zeta{i}.exe"), &format!("Zeta {i}"), Keys::default(), i)).collect();
        let mut res = Results::default();
        fx.sync(&mut res, "zet", 1);
        assert_eq!(web_index(&res), 5, "five rows, then the web row");
        let before = keys(&res);
        assert!(res.late(files(1, &["zet.a", "zet.b", "zet.c", "zet.d"]), 1));
        let titles: Vec<&str> = res.rows().iter().map(|r| &*r.title).collect();
        assert_eq!(titles[..5], ["Zeta 0", "Zeta 1", "Zeta 2", "Zeta 3", "Zeta 4"]);
        assert_eq!(titles[5..7], ["zet.a", "zet.b"], "two late rows fill the view");
        assert_eq!(web_index(&res), 7, "the web row is the 8th");
        assert_eq!(titles[8..], ["zet.c", "zet.d"], "the other two go below it");
        assert_eq!(keys(&res)[..5], before[..5], "no row at or above the selection moved");
        // With the web row selected, the whole batch goes after it and it does not move.
        fx.sync(&mut res, "zet", 2);
        let web = web_index(&res);
        assert!(res.select_index(web));
        assert!(res.late(files(2, &["zet.a", "zet.b", "zet.c", "zet.d"]), 2));
        assert_eq!(web_index(&res), 5);
        assert_eq!(res.selected().unwrap().key, web_search_key());
        assert_eq!(res.rows().len(), 10);
        assert!(res.rows()[6..].iter().all(|r| r.from == ProviderId::Files));
        // A row taken out above the web row pulls it up only as far as the slot allows.
        let key = res.rows()[0].key;
        assert!(res.remove(key).is_some());
        assert_eq!(res.rows().len(), 9);
    }

    #[test]
    fn stale_generation_dropped() {
        let fx = Fx::new();
        let mut res = Results::default();
        fx.sync(&mut res, "st", 4);
        let before = keys(&res);
        assert!(!res.late(files(3, &["old.txt"]), 4), "an older batch");
        assert!(!res.late(files(4, &["old.txt"]), 5), "the session moved on");
        assert_eq!(keys(&res), before);
        assert!(res.late(files(4, &["kept.txt"]), 4));
        fx.sync(&mut res, "ste", 5);
        assert!(res.rows().iter().all(|r| r.from != ProviderId::Files), "older rows are gone at the next keystroke");
        // A refresh of the same generation keeps its late rows and the selected key.
        assert!(res.late(files(5, &["steam.cfg"]), 5));
        res.select_index(res.rows().iter().position(|r| &*r.title == "steam.cfg").unwrap());
        fx.sync(&mut res, "ste", 5);
        assert_eq!(res.selected().unwrap().title.as_ref(), "steam.cfg");
        assert_eq!(res.rows().last().unwrap().key, web_search_key());
    }

    #[test]
    fn web_row_from_3_chars_selected_only_alone() {
        let fx = Fx::new();
        let mut res = Results::default();
        fx.sync(&mut res, "zz", 1);
        assert!(res.rows().is_empty() && res.selected().is_none());
        fx.sync(&mut res, "zzq", 2);
        assert_eq!(keys(&res), [web_search_key()]);
        assert_eq!(res.selected().unwrap().key, web_search_key());
        assert!(!res.has_matches());
        fx.sync(&mut res, "ste", 3);
        assert_eq!(res.rows().last().unwrap().key, web_search_key());
        assert_eq!(res.selected().unwrap().title.as_ref(), "Steam");
        assert!(res.has_matches());
        // A destructive row that may not be the default does not let the web row take it.
        fx.sync(&mut res, "sle", 4);
        assert!(res.rows().iter().any(|r| r.payload == Payload::Command(CommandId::Sleep)));
        assert_eq!(res.selected(), None);
    }

    #[test]
    fn default_skips_destructive_and_executables() {
        let fx = Fx::new();
        let mut res = Results::default();
        fx.sync(&mut res, "sl", 1);
        assert_eq!(res.rows().len(), 1);
        assert_eq!(res.rows()[0].payload, Payload::Command(CommandId::Sleep));
        assert_eq!(res.selected(), None, "\"sl\" never selects Sleep");
        fx.sync(&mut res, "sleep", 2);
        assert_eq!(res.selected().unwrap().payload, Payload::Command(CommandId::Sleep));
        fx.sync(&mut res, "C:\\tools\\setup.exe", 3);
        assert!(is_executable_file(&res.rows()[0]));
        assert_eq!(res.selected(), None, "an executable file row is never the default in Mixed mode");
        fx.sync(&mut res, "C:\\tools\\readme.txt", 4);
        assert_eq!(res.selected().unwrap().kind, Kind::File);
        // A late executable row is not taken as the default either; a document is.
        fx.sync(&mut res, "zz", 5);
        let batch = |items| Batch { gen: 5, from: ProviderId::Files, items, done: false };
        assert!(res.late(batch(vec![file_row("setup.exe", FileClass::Executable)]), 5));
        assert_eq!(res.selected(), None);
        assert!(res.late(batch(vec![file_row("zz.txt", FileClass::Other)]), 5));
        assert_eq!(res.selected().unwrap().title.as_ref(), "zz.txt");
    }

    #[test]
    fn merge_order_notices_answer_ranked_web() {
        let fx = Fx::new();
        let mut res = Results::default();
        let mut notice = file_row("notice", FileClass::Other);
        notice.kind = Kind::Notice;
        notice.verbs = VerbSet::empty();
        res.set_notices(vec![notice]);
        fx.sync(&mut res, "3*12", 1);
        let kinds: Vec<Kind> = res.rows().iter().map(|r| r.kind).collect();
        assert_eq!(kinds, [Kind::Notice, Kind::Answer, Kind::Web]);
        assert_eq!(res.rows()[1].title.as_ref(), "36");
        assert_eq!(res.selected().unwrap().kind, Kind::Answer, "a notice without an action is not the default");
        // Removing the selected row selects the one that takes its place.
        let answer = res.rows()[1].key;
        assert_eq!(res.remove(answer).unwrap().kind, Kind::Answer);
        assert_eq!(res.selected().unwrap().key, web_search_key());
        assert!(res.remove(answer).is_none());
    }

    #[test]
    fn learned_pick_rises_inside_its_tier_only() {
        let mut fx = Fx::new();
        let base: Vec<Item> = fx.rows("o");
        let first = base.iter().find(|r| r.from == ProviderId::Apps).unwrap();
        // The last app of the first app's tier: a learned pick overtakes its equals, never a better tier.
        let same_tier = base.iter().rposition(|r| r.score.tier == first.score.tier && r.from == ProviderId::Apps);
        let last = &base[same_tier.unwrap()];
        assert_ne!(last.key, first.key, "{base:#?}");
        let before = base.iter().position(|r| r.key == last.key).unwrap();
        fx.learned.pick(last.key, 1_790_000_000, false);
        let rows = fx.rows("o");
        let after = rows.iter().position(|r| r.key == last.key).unwrap();
        assert!(after < before, "{before} -> {after}");
        assert!(rows.iter().take(after).all(|r| r.score.tier <= last.score.tier), "only inside the tier");
        assert_eq!(rows.first().map(|r| &*r.title), Some("Shut down"), "a better tier stays first");
        assert_eq!(rows[after].score.learned, Reverse(16));
        // A better tier still wins over any learned count.
        let first_tier = rows.iter().map(|r| r.score.tier).min().unwrap();
        assert_eq!(rows[0].score.tier, first_tier);
        // Settings learn too.
        let mut fx = Fx::new();
        fx.learned.pick(ItemKey::of(ProviderId::Settings, "Schermo"), 1_790_000_000, false);
        let row = fx.rows("schermo").into_iter().find(|r| r.kind == Kind::Setting);
        assert_eq!(row.map(|r| r.score.learned), Some(Reverse(16)));
    }

    #[test]
    fn destructive_rows_ignore_what_is_learned() {
        let mut fx = Fx::new();
        for cmd in COMMANDS.iter().filter(|c| c.destructive) {
            // Even a state edited by hand cannot lift them.
            fx.learned.entries_for_test(ItemKey::of(ProviderId::Commands, cmd.en));
        }
        let rows = fx.rows("shut down");
        let down = rows.iter().find(|r| r.destructive).unwrap();
        assert_eq!(down.score.learned, Reverse(0));
        let mut fx = Fx::new();
        fx.learned.pick(ItemKey::of(ProviderId::Commands, "Lock"), 1_790_000_000, false);
        let rows = fx.rows("lock");
        assert_eq!(rows.iter().find(|r| r.kind == Kind::Command).unwrap().score.learned, Reverse(16));
    }

    #[test]
    fn running_app_merges_with_catalog_row() {
        let fx = Fx::new();
        let rows = fx.rows("blocco");
        let notepad = titled(&rows, "Blocco note");
        assert_eq!(notepad.len(), 1, "{rows:#?}");
        let row = notepad[0];
        assert_eq!(row.kind, Kind::App);
        assert!(!row.score.idle);
        assert_eq!(row.payload, Payload::Group(NOTEPAD_KEY.into()));
        assert_eq!(row.icon, IconRef::App("Microsoft.WindowsNotepad_8wekyb3d8bbwe!App".into()));
        assert!(row.verbs.has(Verb::Switch) && row.verbs.has(Verb::CloseWindow));
        assert!(rows.iter().all(|r| r.kind != Kind::Window));
        // Matched by AUMID too, whatever the bar calls it.
        let mut fx = Fx::new();
        fx.windows[0].name_folded = "something else".into();
        fx.windows[0].aumid = Some("microsoft.windowsnotepad_8wekyb3d8bbwe!app".into());
        assert_eq!(titled(&fx.rows("blocco"), "Blocco note")[0].payload, Payload::Group(NOTEPAD_KEY.into()));
        // A catalogue app with no window is not running.
        let calc = titled(&fx.rows("calcolatrice"), "Calcolatrice")[0].clone();
        assert!(calc.score.idle && calc.payload == Payload::AppId(fx.apps[0].id.clone()));
    }

    #[test]
    fn unmatched_running_program_is_a_window_row() {
        let fx = Fx::new();
        let rows = fx.rows("footool");
        assert_eq!(rows.len(), 1, "{rows:#?}");
        let row = &rows[0];
        assert_eq!((row.kind, &*row.title, row.detail.as_deref()), (Kind::Window, "FooTool", None));
        assert_eq!(row.payload, Payload::Group(FOOTOOL_KEY.into()));
        assert_eq!(row.icon, IconRef::Bar(FOOTOOL_KEY.into()));
        assert!(!row.score.idle);
        // Its title is never matched in Mixed mode.
        assert!(fx.rows("main").iter().all(|r| r.kind != Kind::Window));
        // Off with the windows provider.
        let mut fx = Fx::new();
        fx.opts.mask = ProviderMask::all().with(ProviderId::Windows, false);
        assert!(fx.rows("footool").is_empty());
    }

    #[test]
    fn windows_mode_one_row_per_window_with_title() {
        let mut fx = Fx::new();
        let all = fx.rows("<");
        assert_eq!(all.len(), 2);
        assert_eq!(all[0].payload, Payload::Hwnd(0x1001), "most recently used first");
        assert_eq!(all[0].detail.as_deref(), Some("Senza titolo - Blocco note"));
        assert!(all.iter().all(|r| r.kind == Kind::Window && !r.score.idle));
        // Matched on the title too.
        assert_eq!(fx.rows("<senza")[0].payload, Payload::Hwnd(0x1001));
        fx.windows[1].hung = true;
        let foo = fx.rows("<foo");
        assert_eq!(foo.len(), 1);
        let detail = foo[0].detail.as_deref().unwrap();
        assert!(detail.starts_with("FooTool - main") && detail.ends_with(strings::not_responding()), "{detail}");
        // The apps do not answer in "<" mode.
        assert!(fx.rows("<calc").is_empty());
    }

    #[test]
    fn disabled_provider_never_answers() {
        let mut fx = Fx::new();
        assert!(fx.rows("schermo").iter().any(|r| r.kind == Kind::Setting));
        fx.opts.mask = ProviderMask::all().with(ProviderId::Settings, false);
        let (rows, called) = fx.query("schermo");
        assert!(rows.iter().all(|r| r.kind != Kind::Setting), "{rows:#?}");
        assert!(!called.contains(&ProviderId::Settings));
        assert!(fx.query("$schermo").1.is_empty(), "its own mode calls nothing");
        // Apps cannot be switched off.
        fx.opts.mask = ProviderMask::all().with(ProviderId::Apps, false);
        assert!(fx.rows("calc").iter().any(|r| &*r.title == "Calcolatrice"));
    }

    #[test]
    fn custom_engine_host_in_web_row() {
        let mut fx = Fx::new();
        fx.opts.web = web::validate_template("https://search.example.org/?q={q}").unwrap();
        let rows = fx.query("hello world").0;
        let web = rows.iter().find(|r| r.key == web_search_key()).unwrap();
        assert!(web.title.contains("search.example.org"), "{}", web.title);
        assert!(web.title.contains("hello world"));
        let Payload::Url(url) = &web.payload else { panic!("{:?}", web.payload) };
        assert_eq!(&**url, "https://search.example.org/?q=hello%20world");
        // Only from 3 characters in Mixed mode, from 1 in "??" mode; "??" alone is a hint that does nothing.
        assert!(fx.query("he").0.iter().all(|r| r.key != web_search_key()));
        assert!(fx.query("??h").0.iter().any(|r| r.key == web_search_key() && r.payload != Payload::None));
        let hint = fx.query("??").0;
        assert_eq!(hint.len(), 1);
        assert!(hint[0].title.contains("search.example.org") && hint[0].verbs.is_empty());
    }

    #[test]
    fn accented_setting_found_by_folded_query() {
        let fx = Fx::new();
        let tier = |raw: &str| {
            let rows = fx.rows(raw);
            rows.iter().find(|r| &*r.title == "Accessibilità").map(|r| r.score.tier)
        };
        assert_eq!(tier("accessibilita"), Some(0), "the whole name, folded");
        assert_eq!(tier("Accessibilità"), Some(0));
        assert_eq!(tier("accessib"), Some(1));
        assert_eq!(tier("$access"), Some(1));
        // Windows' synonyms of a setting.
        assert!(fx.rows("display").iter().any(|r| &*r.title == "Schermo"));
    }

    #[test]
    fn budget_logs_after_three_overruns() {
        let mut b = Budget::default();
        let id = ProviderId::Settings;
        assert!(!b.record(id, 2_500));
        assert!(!b.record(id, 2_500));
        assert!(b.record(id, 2_500), "the third overrun in a row");
        assert!(!b.record(id, 2_500), "and it starts again");
        assert!(!b.record(id, 2_500));
        assert!(!b.record(id, 100), "within the budget resets");
        assert!(!b.record(id, 2_500));
        assert!(!b.record(id, 2_500));
        assert!(!b.record(ProviderId::Apps, 2_500), "one counter per provider");
        assert!(b.record(id, 2_500));
        assert!(
            !b.record(id, BUDGET_US) && !b.record(id, 2_001) && !b.record(id, 2_001),
            "the budget itself is not over"
        );
    }

    #[test]
    fn query_all_order_is_5_4_1() {
        use ProviderId::*;
        assert_eq!(ORDER, [Calc, DateTime, Apps, Windows, Commands, Settings, Run, Web, Registry]);
        let fx = Fx::new();
        assert_eq!(fx.query("x").1, [Calc, DateTime, Apps, Commands, Settings, Run, Web, Registry]);
        assert_eq!(fx.query(")").1, [DateTime]);
        assert_eq!(fx.query("<x").1, [Windows]);
        assert_eq!(fx.query("=1").1, [Calc]);
        assert_eq!(fx.query(">ping").1, [Run]);
        assert_eq!(fx.query("file explorer").1, [Apps], "a word prefix keeps the apps");
        // Rows of the same tier and kind keep the order of the providers: the answer of an expression comes first.
        assert_eq!(fx.rows("2+3")[0].kind, Kind::Answer);
    }

    #[test]
    fn results_lift_only_pinned_answers() {
        let mut fx = Fx::new();
        fx.apps.push(CatApp::new("Vendor.Ora", "Ora", Keys::default(), 50));
        let mut res = Results::default();
        // A tier-0 answer (the calculator's) goes above the ranked rows.
        fx.sync(&mut res, "2+3", 1);
        assert_eq!(res.rows()[0].kind, Kind::Answer);
        // A hidden-word answer (the date keyword) stays among them: an app named like it is above.
        fx.sync(&mut res, "ora", 2);
        let titles: Vec<&str> = res.rows().iter().map(|r| &*r.title).collect();
        assert_eq!(titles[0], "Ora", "{titles:?}");
        assert!(res.rows().iter().any(|r| r.from == ProviderId::DateTime));
        assert_eq!(res.selected().unwrap().title.as_ref(), "Ora");
    }

    #[test]
    fn datetime_rows_keep_their_order() {
        let fx = Fx::new();
        let mut res = Results::default();
        fx.sync(&mut res, ")", 1);
        let shown: Vec<String> =
            res.rows().iter().filter(|r| r.from == ProviderId::DateTime).map(|r| r.title.to_string()).collect();
        let want: Vec<String> =
            datetime::rows(1_790_000_000, 60, TimeStyle::default()).into_iter().map(|r| r.value).collect();
        assert_eq!(shown, want);
        assert_eq!(shown.len(), 8);
        assert!(res.rows().iter().all(|r| r.kind != Kind::Answer || r.from == ProviderId::DateTime));
        // Whatever follows the prefix, the same eight rows.
        fx.sync(&mut res, ")xyz", 2);
        assert_eq!(res.rows().iter().filter(|r| r.from == ProviderId::DateTime).count(), 8);
    }

    #[test]
    fn datetime_rows_in_their_mode() {
        let mut fx = Fx::new();
        let own = |fx: &Fx, raw: &str| fx.rows(raw).into_iter().filter(|r| r.from == ProviderId::DateTime).count();
        assert_eq!(own(&fx, "ora"), 1);
        assert_eq!(own(&fx, "date"), 1);
        assert_eq!(own(&fx, "ora x"), 0, "only the exact keyword answers");
        assert_eq!(own(&fx, "or"), 0);
        assert_eq!(own(&fx, "= ora"), 0, "the calculator's mode runs no date row");
        assert_eq!(own(&fx, ")"), 8);
        let week = fx.rows("week");
        assert!(week.iter().any(|r| r.from == ProviderId::DateTime && r.title.starts_with("Week ")));
        fx.opts.mask = ProviderMask::all().with(ProviderId::DateTime, false);
        assert_eq!(own(&fx, "ora"), 0);
        assert_eq!(own(&fx, ")"), 0);
        assert!(!fx.query("ora").1.contains(&ProviderId::DateTime));
    }

    #[test]
    fn datetime_keyword_never_beats_a_name() {
        let mut fx = Fx::new();
        fx.settings.push(SettingRow::new(2, "Data e ora", Keys::default(), 3));
        let rows = fx.rows("data");
        let at = |f: &dyn Fn(&Item) -> bool| rows.iter().position(f).unwrap();
        let setting = at(&|r| &*r.title == "Data e ora");
        let date = at(&|r| r.from == ProviderId::DateTime);
        assert!(setting < date, "{rows:#?}");
        // A learned row of the keyword's tier stays above it too (no learned boost on the date row).
        assert_eq!(rows[date].score.learned, Reverse(0));
    }

    #[test]
    fn datetime_allocates_nothing_without_a_keyword() {
        use crate::launcher::testkit::allocations_during;
        let fx = Fx::new();
        let cx = fx.cx();
        let mut top = TopK::new(8);
        for raw in ["calc", "", "x", "ora x", "= 1"] {
            let q = query::parse_query(raw, &fx.opts.prefixes, NUM_IT, 1);
            let (_, n) = allocations_during(|| datetime(&q, &cx, &mut top));
            assert_eq!(n, 0, "{raw:?}");
        }
        assert!(top.is_empty());
    }

    #[test]
    fn fuzzy_runs_only_when_nothing_answered() {
        let fx = Fx::new();
        let rows = fx.rows("clacolatrice");
        assert_eq!((&*rows[0].title, rows[0].score.tier), ("Calcolatrice", 8));
        // "stea" matches Steam by prefix: no fuzzy rows next to it.
        assert!(fx.rows("stea").iter().all(|r| r.score.tier < rank::T_FUZZY));
    }

    #[test]
    fn fuzzy_never_after_an_answer() {
        let fx = Fx::new();
        let passes = || FUZZY_PASSES.with(std::cell::Cell::get);
        let before = passes();
        for raw in ["c:\\windows\\system32", "https://x.example", "ipconfig /all", "12*(3+4)", "> notepad"] {
            let rows = fx.query(raw).0;
            assert!(!rows.is_empty(), "{raw}: an answer row");
            assert!(rows.iter().all(|r| r.score.tier != rank::T_FUZZY), "{raw}");
        }
        assert_eq!(passes(), before, "no fuzzy pass at all");
        // The pass does run for a query nothing answered.
        let _ = fx.query("clacolatrice");
        assert_eq!(passes(), before + 1);
    }

    #[test]
    fn fuzzy_ignores_keywords() {
        let mut fx = Fx::new();
        fx.settings = vec![
            SettingRow::new(0, "Frob panel", Keys::from(&[("quantumflux", 1)][..]), 1),
            SettingRow::new(1, "Zoomable options", Keys::default(), 2),
        ];
        let settings = |fx: &Fx, raw: &str| -> Vec<String> {
            fx.rows(raw).into_iter().filter(|r| r.kind == Kind::Setting).map(|r| r.title.to_string()).collect()
        };
        assert!(settings(&fx, "quantumflix").is_empty(), "one edit from a keyword: not found");
        assert_eq!(settings(&fx, "zoomabel"), ["Zoomable options"], "one edit from the name: found");
    }

    #[test]
    fn at_most_three_hidden_settings_in_mixed() {
        use crate::start_search::XML_KEYWORD_PENALTY as XML;
        let mut fx = Fx::new();
        fx.settings = (0..6)
            .map(|i| SettingRow::new(i, &format!("Panel {i}"), Keys::from(&[("frobnicate", XML)][..]), 10 + i))
            .collect();
        let settings = |raw: &str| fx.rows(raw).into_iter().filter(|r| r.kind == Kind::Setting).count();
        assert_eq!(settings("frobnicate"), 3, "Mixed mode: the best three of the hidden-only rows");
        assert_eq!(settings("$frobnicate"), 6, "the Settings mode has no cap");
    }

    #[test]
    fn word_prefix_keeps_apps_at_tier_1() {
        let mut fx = Fx::new();
        fx.apps.push(CatApp::new("Microsoft.Windows.Explorer2", "File Explorer", Keys::default(), 1));
        let rows = fx.rows("file explorer");
        // The fixture's Esplora file is File Explorer too: its other-language name is a name (L3 T5).
        let titles: Vec<&str> = rows.iter().map(|r| &*r.title).collect();
        assert_eq!(titles, ["File Explorer", "Esplora file"], "{rows:#?}");
        assert!(fx.rows("file xplorer").is_empty(), "nothing worse than tier 1");
    }

    #[test]
    fn implicit_rows_by_syntax() {
        let fx = Fx::new();
        let ping = fx.rows("ping 1.1.1.1");
        let run = ping.iter().find(|r| r.kind == Kind::RunLine).unwrap();
        assert_eq!(&*run.title, "ping 1.1.1.1");
        assert_eq!(run.detail.as_deref().map(|d| d.ends_with("ping 1.1.1.1")), Some(true));
        let Payload::RunLine(line) = &run.payload else { panic!() };
        assert!(line.console);
        let path = &fx.rows("C:\\Users\\%USERNAME%\\a b.txt")[0];
        assert_eq!(path.kind, Kind::File);
        let Payload::Path { path: p, dir, class } = &path.payload else { panic!() };
        assert_eq!((&**p, *dir, *class), ("C:\\Users\\%USERNAME%\\a b.txt", false, FileClass::Other));
        assert!(path.verbs.has(Verb::Open));
        let exe = &fx.rows("c:\\tools\\setup.EXE")[0];
        assert!(!exe.verbs.has(Verb::Open), "executables open only after live check 6");
        assert_eq!(fx.rows("C:\\tools\\")[0].kind, Kind::Folder);
        // A typed folder has no dot in its last part: the folder glyph, by syntax alone (5.4.9).
        for typed in ["C:\\Windows", "\\\\srv\\share", "shell:startup"] {
            let row = &fx.rows(typed)[0];
            assert_eq!((row.kind, &row.icon), (Kind::Folder, &IconRef::Glyph(GlyphId::Folder)), "{typed}");
            assert!(matches!(&row.payload, Payload::Path { dir: true, .. }), "{typed}");
        }
        let settings_uri = &fx.rows("ms-settings:display")[0];
        assert_eq!(settings_uri.kind, Kind::File, "a settings URI is not a folder");
        assert!(names_folder("C:\\tools\\") && !names_folder("C:\\tools\\setup.exe") && !names_folder("ms-settings:"));
        let url = &fx.rows("www.example.com")[0];
        assert_eq!((url.kind, url.payload.clone()), (Kind::Web, Payload::Url("https://www.example.com".into())));
        assert!(fx.rows(":").iter().any(|r| r.payload == Payload::Command(CommandId::Registry)));
        let history = fx.rows(">");
        assert_eq!(&*history[0].title, "ping 1.1.1.1", "history first, newest first");
        assert_eq!(history[0].kind, Kind::RunSuggestion);
        assert!(history.iter().any(|r| r.kind == Kind::RunSuggestion && !r.verbs.has(Verb::Forget)), "known commands");
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

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::indexing_slicing)]
mod bench {
    //! LC-3: what one keystroke costs over a big catalogue. Two counts that run with every test run (allocations and
    //! tier checks per query: they count, they never time) and, in the CI bench job only (release, `--ignored`), the
    //! time of the same queries against a reference workload of the same process.
    use super::*;
    use crate::launcher::arena::{self, Arena};
    use crate::launcher::catalog::CatApp;
    use crate::launcher::model::SettingRow;
    use crate::launcher::rank::TIER_CHECKS;
    use crate::launcher::testkit::{allocations_during, XorShift};
    use std::time::Instant;

    /// The names of the fixture: 150 English and 150 Italian words (accents included, for the folding).
    const WORDS: &str =
        "app audio backup battery bluetooth browser calculator calendar camera capture chat clock cloud code \
        color command connect contacts control copy data date defender desktop device display disk document \
        download drive editor email explorer family feedback file finder firewall folder font game graphics \
        group health help history home image input insider install keyboard language launcher library light \
        link local lock mail manager maps media memory message microsoft mixer mobile monitor mouse movie \
        music network news note notepad office online options paint panel password pen phone photo picture \
        player policy power preview print privacy profile project quick radio record recovery region remote \
        reader report screen search security server service settings setup share shell show snip sound \
        speech spell start steam sticky storage store studio sync system task terminal text theme time timer \
        tips tool touch tracker update usb user video view viewer virtual voice volume wallet weather web \
        widget window wireless word accesso account aggiornamento aiuto alimentazione amministratore \
        anteprima appunti archivio attività avvio barra batteria blocco calcolatrice calendario cartella \
        cattura chiave colore comandi condivisione configurazione connessione console contatti controllo \
        copia cronologia dati dispositivo disco documenti energia esplora filmati finestra foto gestione \
        giochi grafica gruppo immagini impostazioni informazioni ingresso installazione lettore lingua lista \
        mappe memoria messaggi microfono musica notifiche nuovo opzioni ora orologio pannello percorso posta \
        processi profilo programma progetto protezione rapido registro rete ricerca risorse schermo \
        scrivania servizi sicurezza sistema sincronizzazione sfondo stampa stampante strumenti suono \
        supporto tastiera tema terminale testo unità utente visualizzatore voce zona accessibilità aspetto \
        avanzate area bloccare caratteri cestino chiamate cuffie data disattiva elenco esegui fotocamera \
        famiglia guida indirizzo lavoro luminosità modalità mostra nascondi notte orario pagina personalizza \
        posizione predefinite preferiti puntatore riproduzione salva scarica schede segnale sensore sorgente \
        spazio stato sveglia tempo trasferimento ultimo vista volume wireless attivazione bilanciamento \
        collegamento cancella diagnostica estensioni lavagna navigazione privata ripristino";

    fn words() -> Vec<&'static str> {
        let w: Vec<&str> = WORDS.split_whitespace().collect();
        assert_eq!(w.len(), 300);
        w
    }

    const NUM: NumberStyle = NumberStyle { decimal: ',', group: '.' };
    /// Queries per run, and the bound of 5.5.7 for one query on the runner (the providers' share of a keystroke).
    const QUERIES: usize = 1_000;
    /// What one query may allocate whatever the size of the catalogue: the TopK, one list of the best per big provider
    /// (apps, their unmatched windows, settings), the rows of at most `cap` (24) candidates each (three strings for an
    /// app or a window row, one for a setting), the commands and the fixed rows (answer, run, web). 118 measured at
    /// 2,000 apps and 126 at 4,000; 64 (LC-3's figure) is below what 24 app rows alone need (72).
    const MAX_ALLOCATIONS: u64 = 160;
    const PER_QUERY_BUDGET_US: f64 = 4_000.0;

    /// The catalogue of one test: apps, settings, windows and run history, with the default options.
    struct Big {
        apps: Vec<CatApp>,
        settings: Vec<SettingRow>,
        windows: Vec<WindowRow>,
        history: Vec<Box<str>>,
        apps_arena: Arena,
        settings_arena: Arena,
        opts: LauncherOpts,
        /// A full 1,000-entry state: the keystroke reads it for every matched candidate.
        learned: Frecency,
    }

    impl Big {
        /// `scale` 1: 2,000 apps, 1,500 settings, 60 windows, 50 history lines; 2 doubles each. Every kind has its own
        /// generator (seed 0xB0B and the kind), so the first half of scale 2 is scale 1.
        fn new(scale: usize) -> Big {
            let words = words();
            let name = |r: &mut XorShift| {
                let n = 1 + (r.next_u64() % 3) as usize;
                let words: Vec<&str> = (0..n).map(|_| *r.pick(&words)).collect();
                let mut s = words.join(" ");
                if let Some(c) = s.get_mut(..1) {
                    c.make_ascii_uppercase();
                }
                s
            };
            let mut r = XorShift::new(0xB0B);
            let apps: Vec<CatApp> = (0..2_000 * scale)
                .map(|i| {
                    let n = name(&mut r);
                    CatApp::new(&format!("Vendor.App{i}!App"), &n, Keys::default(), (r.next_u64() % 1_000) as u32)
                })
                .collect();
            let mut r = XorShift::new(0xB0B + 1);
            let settings: Vec<SettingRow> = (0..1_500 * scale)
                .map(|i| {
                    let n = name(&mut r);
                    let keys = [(*r.pick(&words), 0), (*r.pick(&words), 1)];
                    SettingRow::new(i as u32, &n, Keys::from(&keys[..]), (r.next_u64() % 1_000) as u32)
                })
                .collect();
            let mut r = XorShift::new(0xB0B + 2);
            let windows = (0..60 * scale)
                .map(|i| {
                    let n = name(&mut r);
                    let mut folded = String::new();
                    rank::fold(&n, &mut folded);
                    WindowRow {
                        hwnd: 0x1000 + i as isize,
                        group_key: format!("c:\\program files\\tool{i}\\tool{i}.exe").into(),
                        aumid: None,
                        name: n.into(),
                        name_folded: folded.into(),
                        title: None,
                        hung: false,
                    }
                })
                .collect();
            let mut r = XorShift::new(0xB0B + 3);
            let history = (0..50 * scale).map(|_| format!("ping {}", r.pick(&words)).into()).collect();
            let mut learned = Frecency::new();
            for (i, app) in apps.iter().enumerate().step_by(2).take(600) {
                learned.pick(ItemKey::of(ProviderId::Apps, &app.id), 1_789_000_000 + i as i64, false);
            }
            for row in settings.iter().step_by(3).take(400) {
                learned.pick(ItemKey::of(ProviderId::Settings, &row.name), 1_780_000_000, false);
            }
            let (apps_arena, settings_arena) = (arena::of_apps(&apps), arena::of_settings(&settings));
            Big { apps, settings, windows, history, apps_arena, settings_arena, opts: LauncherOpts::default(), learned }
        }

        fn cx(&self) -> Context<'_> {
            Context {
                apps: self.apps_arena.view(),
                settings: self.settings_arena.view(),
                windows: &self.windows,
                history: &self.history,
                opts: &self.opts,
                num: NUM,
                learned: &self.learned,
                now_unix: 1_790_000_000,
                utc_offset_min: 60,
                time_style: TimeStyle::default(),
                window_target: true,
            }
        }

        /// The candidates one query ranks by tiers: every app, setting and window, and the commands.
        fn candidates(&self) -> u64 {
            (self.apps.len() + self.settings.len() + self.windows.len() + COMMANDS.len()) as u64
        }
    }

    /// `QUERIES` queries of 1 to 12 characters: most a cut of one or two words, some random letters (the misses that
    /// reach tier 6).
    fn queries() -> Vec<String> {
        let words = words();
        let mut r = XorShift::new(0xB0B + 4);
        let mut qs: Vec<String> = (0..QUERIES)
            .map(|_| {
                let len = 1 + (r.next_u64() % 12) as usize;
                let text: String = if r.next_u64() % 10 < 7 {
                    let two = format!("{} {}", r.pick(&words), r.pick(&words));
                    two.chars().take(len).collect()
                } else {
                    (0..len).map(|_| char::from(b'a' + (r.next_u64() % 26) as u8)).collect()
                };
                if text.trim().is_empty() {
                    "a".into()
                } else {
                    text
                }
            })
            .collect();
        // The date and time provider: its prefix lists every row, a keyword answers one (DT-1).
        qs.extend([")".to_string(), "ora".to_string()]);
        qs
    }

    /// The most allocations one query of `qs` made inside `query_all`.
    fn worst_allocations(big: &Big, qs: &[String]) -> u64 {
        let cx = big.cx();
        let mut worst = 0;
        for raw in qs {
            let q = query::parse_query(raw, &big.opts.prefixes, NUM, 1);
            let (_, n) = allocations_during(|| query_all(&q, &cx, &mut Budget::default()));
            worst = worst.max(n);
        }
        worst
    }

    /// KEY-3: the most edit-distance cells one query computes over the bench's candidates, for each of the 1,000 queries
    /// and every prefix of each (the keystrokes of typing it). The pass costs a bounded amount whatever the query.
    const MAX_OSA_CELLS: u64 = 50_000;

    #[test]
    fn osa_cells_bounded() {
        use crate::launcher::rank::OSA_CELLS;
        let big = Big::new(1);
        let cx = big.cx();
        let mut worst = (0, String::new());
        for raw in queries() {
            let mut end = 0;
            for ch in raw.chars() {
                end += ch.len_utf8();
                let part = &raw[..end];
                let q = query::parse_query(part, &big.opts.prefixes, NUM, 1);
                let before = OSA_CELLS.with(std::cell::Cell::get);
                let _ = query_all(&q, &cx, &mut Budget::default());
                let cells = OSA_CELLS.with(std::cell::Cell::get) - before;
                if cells > worst.0 {
                    worst = (cells, part.to_string());
                }
            }
        }
        println!("most OSA cells in one query: {} ({:?})", worst.0, worst.1);
        assert!(worst.0 <= MAX_OSA_CELLS, "{} OSA cells for {:?}", worst.0, worst.1);
    }

    /// L3 T9 and T10, per candidate: for every one of the 1,000 queries and every prefix of each, the dictionaries answer
    /// like the scans over the bench's apps and settings (the hidden tiers: `hidden` against `hidden_best`; the fuzzy
    /// tier: `fuzzy` plus `subsequence_name` against `match_fuzzy`). No `query_all` runs, so the slow side is cheap.
    #[test]
    fn dictionaries_equal_scan_on_the_bench() {
        let big = Big::new(1);
        let apps: Vec<_> = big.apps.iter().map(CatApp::cand).collect();
        let settings: Vec<_> = big.settings.iter().map(SettingRow::cand).collect();
        let (mut hidden_hits, mut fuzzy_hits) = (0usize, 0usize);
        let app_names: Vec<(&str, &str)> = big.apps.iter().map(|a| (&*a.name, &*a.id)).collect();
        let setting_names: Vec<(&str, &str)> = big.settings.iter().map(|s| (&*s.name, "")).collect();
        let (app_view, setting_view) = (big.apps_arena.view(), big.settings_arena.view());
        for (cands, cv, names) in [(&apps, &app_view, &app_names), (&settings, &setting_view, &setting_names)] {
            let view = cv.index();
            assert_eq!(view.entries(), cands.len());
            for (i, want) in names.iter().enumerate() {
                assert_eq!((cv.name(i), cv.id(i)), *want, "entry {i}");
            }
            let (mut hits, mut near) = (Vec::new(), Vec::new());
            for raw in queries() {
                let mut end = 0;
                for ch in raw.chars() {
                    end += ch.len_utf8();
                    let part = &raw[..end];
                    let q = part.trim();
                    let chars = q.chars().count();
                    view.hidden(q, chars, &mut hits);
                    let fz = rank::fuzzy_chars(part).map(|(c, m)| (c, m, rank::fuzzy_dist(m)));
                    if let Some((c, m, dist)) = &fz {
                        view.fuzzy(&c[..*m], *dist, &mut near);
                    }
                    let mut next = 0;
                    for (i, c) in cands.iter().enumerate() {
                        let found = if hits.get(next).is_some_and(|h| h.0 as usize == i) {
                            next += 1;
                            Some((hits[next - 1].1, hits[next - 1].2))
                        } else {
                            None
                        };
                        assert_eq!(found, rank::hidden_best(c, q, chars), "{part:?} entry {i}");
                        hidden_hits += usize::from(found.is_some());
                        // What the dock runs per candidate: the arena's candidate against the in-memory one.
                        let vc = cv.cand(i);
                        let tier = rank::match_tiers_with(&vc, q, found);
                        assert_eq!(tier, rank::match_tiers(c, q), "{part:?} entry {i}: tier");
                        if let Some(t) = tier {
                            for k in [0, 1] {
                                assert_eq!(
                                    rank::score_hidden(&vc, t, false, k, found),
                                    rank::score_for(c, q, t, false, k),
                                    "{part:?} entry {i}: score"
                                );
                            }
                        }
                        if let Some((chars, m, _)) = &fz {
                            let qc = &chars[..*m];
                            let in_dict = near.binary_search(&(i as u32)).is_ok();
                            let want = rank::match_fuzzy(c, part).is_some();
                            assert_eq!(in_dict || rank::subsequence_name(c, qc), want, "{part:?} entry {i}");
                            fuzzy_hits += usize::from(want);
                            assert_eq!(
                                rank::subsequence_name(&cv.cand(i), qc),
                                rank::subsequence_name(c, qc),
                                "{part:?} entry {i}: subsequence"
                            );
                        }
                    }
                    assert_eq!(next, hits.len(), "{part:?}: every hit belongs to an entry");
                }
            }
        }
        assert!(hidden_hits > 0 && fuzzy_hits > 0);
    }

    /// MEM-2: the 400-app catalogue (the first 400 apps of the bench) is one block of at most 160 KiB.
    #[test]
    fn apps_arena_fits() {
        let big = Big::new(1);
        let first: Vec<CatApp> = big.apps.iter().take(400).cloned().collect();
        let a = arena::of_apps(&first);
        println!("400 apps: {} B in one block", a.raw().len());
        assert!(a.bytes() <= 160 * 1024);
        assert_eq!(a.bytes(), a.raw().len(), "the block carries no growth slack");
        assert_eq!(a.len(), 400);
    }

    /// Prints the size of the bench's 1,500-setting arena by section, and what `check` costs (run by hand: `cargo test
    /// providers::bench::settings_arena_size_printed -- --ignored --nocapture`).
    #[test]
    #[ignore = "prints the size of the bench's settings arena"]
    fn settings_arena_size_printed() {
        let big = Big::new(1);
        let a = &big.settings_arena;
        print!("{}", a.layout().section_report());
        let mut times: Vec<std::time::Duration> = (0..51)
            .map(|_| {
                let t = Instant::now();
                std::hint::black_box(arena::check(a.raw(), arena::Kind::Settings, None, arena::Caps::BENCH)).unwrap();
                t.elapsed()
            })
            .collect();
        times.sort();
        println!("{} settings, total {} B, check median {:?}", big.settings.len(), a.raw().len(), times[25]);
    }

    /// KEY-2: no keyword line is read on the key path: the hidden tiers come from the dictionaries.
    #[test]
    fn no_keyword_parsed_per_key() {
        use crate::start_search::KEY_SCANS;
        let big = Big::new(1);
        let cx = big.cx();
        for raw in queries() {
            let q = query::parse_query(&raw, &big.opts.prefixes, NUM, 1);
            let before = KEY_SCANS.with(std::cell::Cell::get);
            let _ = query_all(&q, &cx, &mut Budget::default());
            assert_eq!(KEY_SCANS.with(std::cell::Cell::get), before, "{raw:?} read keyword text");
        }
    }

    #[test]
    fn no_allocation_per_candidate() {
        let qs = queries();
        let one = worst_allocations(&Big::new(1), &qs);
        let two = worst_allocations(&Big::new(2), &qs);
        assert!(one <= MAX_ALLOCATIONS, "a query made {one} allocations: rows built for candidates that cannot stay");
        assert!(two <= MAX_ALLOCATIONS, "twice the candidates took a query to {two} allocations (from {one})");
    }

    #[test]
    fn comparisons_are_linear() {
        let big = Big::new(1);
        let cx = big.cx();
        let candidates = big.candidates();
        for raw in &queries() {
            let q = query::parse_query(raw, &big.opts.prefixes, NUM, 1);
            let before = TIER_CHECKS.with(std::cell::Cell::get);
            let _ = query_all(&q, &cx, &mut Budget::default());
            let checks = TIER_CHECKS.with(std::cell::Cell::get) - before;
            assert!(checks <= 6 * candidates, "{raw:?}: {checks} tier checks for {candidates} candidates");
        }
    }

    /// The shortest of five runs of `f`, in ns.
    fn min_of_five(mut f: impl FnMut()) -> u128 {
        (0..5)
            .map(|_| {
                let t = Instant::now();
                f();
                t.elapsed().as_nanos()
            })
            .min()
            .unwrap_or(0)
    }

    #[test]
    #[ignore = "release bench (CI job bench): cargo test --release launcher::providers::bench -- --ignored --nocapture"]
    fn ratio_to_reference() {
        let big = Big::new(1);
        let cx = big.cx();
        let qs = queries();
        let launcher_ns = min_of_five(|| {
            for raw in &qs {
                let q = query::parse_query(raw, &big.opts.prefixes, NUM, 1);
                std::hint::black_box(query_all(&q, &cx, &mut Budget::default()));
            }
        });
        // The reference: folding and sorting the same 4,000 names, 10 times.
        let names: Vec<&str> = big
            .apps
            .iter()
            .map(|a| &*a.name)
            .chain(big.settings.iter().map(|s| &*s.name))
            .chain(big.windows.iter().map(|w| &*w.name))
            .cycle()
            .take(4_000)
            .collect();
        let reference_ns = min_of_five(|| {
            for _ in 0..10 {
                let mut folded: Vec<String> = names
                    .iter()
                    .map(|n| {
                        let mut f = String::new();
                        rank::fold(n, &mut f);
                        f
                    })
                    .collect();
                folded.sort();
                std::hint::black_box(folded);
            }
        });
        let ratio = launcher_ns as f64 / reference_ns.max(1) as f64;
        let per_query_us = launcher_ns as f64 / QUERIES as f64 / 1_000.0;
        println!("bench: launcher {launcher_ns} ns, reference {reference_ns} ns, ratio {ratio:.3}, per query {per_query_us:.1} us");
        if per_query_us > PER_QUERY_BUDGET_US {
            println!("bench: over the {PER_QUERY_BUDGET_US} us of one query (5.5.7): stop and report, no calibration");
        }
        // The gate: budgets.json's row, once the owner approved its calibration (pending: printed, not gated).
        let budgets: serde_json::Value =
            serde_json::from_str(include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/budgets.json"))).unwrap();
        let row = budgets["rows"].as_array().unwrap().iter().find(|r| r["id"] == "bench.launcher_query_ratio").unwrap();
        if row["status"] == "active" {
            let value = row["value"].as_f64().unwrap();
            assert!(ratio * 100.0 <= value, "ratio x 100 = {:.1}, over the calibrated {value}", ratio * 100.0);
        }
    }
}
