//! The synchronous providers (SPEC 5.4.1): what the session's view of the config is (`LauncherOpts`), what one
//! keystroke reads (`Context`, in memory only), where the ranked rows are kept (`TopK`, the only allocation of a
//! query together with the result rows) and the providers themselves, run by `query_all` in the order of 5.4.1.
//! Nothing here reads a file, the registry or the network; time is only counted (`Budget`), never asserted.

use super::catalog::CatApp;
use super::commands::{self, Command, COMMANDS};
use super::gesture::GESTURE_TIME_MATCHES;
use super::model::{
    Batch, CommandId, FileClass, GlyphId, IconRef, Item, ItemKey, Kind, Mode, NumberStyle, Payload, ProviderId,
    ProviderMask, Query, Score, SettingRow, Verb, VerbSet, WindowRow,
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
    pub apps: &'a [CatApp],
    pub settings: &'a [SettingRow],
    /// One per window, most recently used first.
    pub windows: &'a [WindowRow],
    /// Run history, newest first.
    pub history: &'a [Box<str>],
    pub opts: &'a LauncherOpts,
    /// The locale's number marks, read at open.
    pub num: NumberStyle,
    #[allow(dead_code)] // L1b-3 (frecency) and L2-4 (date and time) read the clock of the keystroke
    pub now_unix: i64,
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
    a.score
        .cmp(&b.score)
        .then_with(|| a.title.chars().flat_map(char::to_lowercase).cmp(b.title.chars().flat_map(char::to_lowercase)))
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

/// The synchronous providers in the order of 5.4.1 (DateTime joins in L2). In Mixed mode the open windows are merged
/// into the apps; `Windows` runs only in its own mode ("<").
pub const ORDER: [ProviderId; 8] = [
    ProviderId::Calc,
    ProviderId::Apps,
    ProviderId::Windows,
    ProviderId::Commands,
    ProviderId::Settings,
    ProviderId::Run,
    ProviderId::Web,
    ProviderId::Registry,
];

/// The providers that rank candidates by tiers: only they take part in the tier 6 pass.
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

/// The implicit run line of Mixed mode sorts below every row matched at tiers 0-5 (5.2.4: "below the app rows").
const IMPLICIT_RUN_TIER: u8 = 6;

/// A provider's time budget per keystroke (5.4.1), counted in every build and never asserted (house rule 8).
pub const BUDGET_US: u64 = 2_000;

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

/// Every provider that runs for this query, in the order of 5.4.1, into one `TopK`. Tier 6 runs only when no
/// ranked candidate matched at tiers 0-5. Each provider is timed against its budget.
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
    if hits == 0 && !q.folded.is_empty() {
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
        ProviderId::Settings => settings(q, cx, fuzzy, top),
        ProviderId::Run => {
            run(q, cx, top);
            0
        }
        ProviderId::Web => {
            web(q, cx, top);
            0
        }
        ProviderId::Registry => registry(q, fuzzy, top, s),
        ProviderId::Units | ProviderId::DateTime | ProviderId::Files => 0,
    }
}

fn tier_of(c: &Cand, q: &str, fuzzy: bool) -> Option<u8> {
    if fuzzy {
        rank::match_fuzzy(c, q)
    } else {
        rank::match_tiers(c, q)
    }
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

// ---- apps and the open windows merged into them

/// The bar's group of this catalogue app: the same AUMID (ignoring case) or the same folded name as the bar's.
fn same_program(app: &CatApp, w: &WindowRow) -> bool {
    w.aumid.as_deref().is_some_and(|a| a.eq_ignore_ascii_case(&app.id)) || *w.name_folded == *app.folded
}

fn app_item(app: &CatApp, win: Option<&WindowRow>, score: Score) -> Item {
    Item {
        key: ItemKey::of(ProviderId::Apps, &app.id),
        from: ProviderId::Apps,
        kind: Kind::App,
        title: app.name.clone(),
        detail: None,
        // A running app keeps its catalogue icon, and with it its id (the session's Open, Reveal and Run as
        // administrator of a running app read it there).
        icon: IconRef::App(app.id.clone()),
        score,
        verbs: verbs(Kind::App, win.is_some(), None),
        payload: match win {
            Some(w) => Payload::Group(w.group_key.clone()),
            None => Payload::AppId(app.id.clone()),
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
            (&raw_folded, 1)
        }
        _ => (&q.folded, u8::MAX),
    };
    let mut hits = 0;
    for app in cx.apps {
        let c = app.cand();
        let Some(tier) = tier_of(&c, text, fuzzy).filter(|t| *t <= max_tier) else { continue };
        hits += 1;
        // Look for the running window only when the row could stay: running is the better of the two scores.
        if !top.accepts(&c.score(tier, true, KIND_APP)) {
            continue;
        }
        let win = cx.windows.iter().find(|w| same_program(app, w));
        let score = c.score(tier, win.is_some(), KIND_APP);
        if top.accepts(&score) {
            top.push(app_item(app, win, score));
        }
    }
    if !cx.opts.mask.has(ProviderId::Windows) {
        return hits;
    }
    let no_keys = Keys::default();
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
        if !top.accepts(&score) || cx.apps.iter().any(|a| same_program(a, w)) {
            continue;
        }
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
    Some(c.score(tier, false, KIND_COMMAND))
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
        let Some(score) = command_score(cmd, i, q, fuzzy, s) else { continue };
        hits += 1;
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
fn settings(q: &Query, cx: &Context, fuzzy: bool, top: &mut TopK) -> usize {
    let mut hits = 0;
    for row in cx.settings {
        let c = row.cand();
        let Some(tier) = tier_of(&c, &q.folded, fuzzy) else { continue };
        hits += 1;
        let score = c.score(tier, false, KIND_SETTING);
        if top.accepts(&score) {
            top.push(Item {
                key: ItemKey::of(ProviderId::Settings, &row.name),
                from: ProviderId::Settings,
                kind: Kind::Setting,
                title: row.name.clone(),
                detail: None,
                icon: IconRef::Glyph(GlyphId::Gear),
                score,
                verbs: verbs(Kind::Setting, false, None),
                payload: Payload::Setting(row.index),
                destructive: false,
            });
        }
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
            let score = Score {
                tier: IMPLICIT_RUN_TIER,
                learned: Reverse(0),
                idle: true,
                kind: KIND_RUN,
                rank: 0,
                len: chars_u16(text),
            };
            if top.accepts(&score) {
                let detail = run_detail(&line.line);
                top.push(run_item(line, Kind::RunLine, detail, verbs(Kind::RunLine, false, None), score));
            }
        }
        Some(Implicit::Path) => {
            let dir = text.ends_with(['\\', '/']);
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

/// The rows of the current query in the merge order of 5.2.6 (notices, the answer, the ranked rows, late rows, the
/// web row) and the selection, kept by `ItemKey`. Late rows are only ever added below the synchronous rows and never
/// move a row at or above the selection; every row belongs to the generation of the last `sync`.
#[derive(Debug, Default)]
pub struct Results {
    rows: Vec<Item>,
    sel: Option<ItemKey>,
    gen: u64,
    /// Notices, the answer and the ranked rows: the rows of the keystroke's own frame.
    sync_len: usize,
    /// Late rows right after them, above the web row.
    late_len: usize,
    /// The query was in Mixed mode, where an executable file row is never the default.
    mixed: bool,
    notices: Vec<Item>,
}

impl Results {
    /// The notice rows (5.5.2, 5.7), shown first from the next `sync`.
    pub fn set_notices(&mut self, notices: Vec<Item>) {
        self.notices = notices;
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

    /// Whether anything but notices and the web row answered: when nothing did, tier 7 is worth asking.
    pub fn has_matches(&self) -> bool {
        self.rows.iter().any(|r| r.kind != Kind::Notice && r.key != web_search_key())
    }

    /// The synchronous rows of a keystroke. A new generation drops every older row and takes the default selection; the
    /// same generation again (a refresh) keeps its late rows and the selected key when it is still there.
    pub fn sync(&mut self, q: &Query, top: TopK) {
        let same = q.gen == self.gen;
        let keep = if same { self.sel } else { None };
        let late = if same { self.take_late() } else { Vec::new() };
        self.rows.clear();
        self.rows.extend(self.notices.iter().cloned());
        let mut web = None;
        let mut ranked = Vec::with_capacity(top.len());
        for item in top.into_sorted() {
            if item.key == web_search_key() {
                web = Some(item);
            } else if item.kind == Kind::Answer {
                self.rows.push(item);
            } else {
                ranked.push(item);
            }
        }
        self.rows.append(&mut ranked);
        self.sync_len = self.rows.len();
        self.late_len = late.len();
        self.rows.extend(late);
        self.rows.extend(web);
        self.gen = q.gen;
        self.mixed = q.mode == Mode::Mixed;
        self.sel = keep.filter(|k| self.rows.iter().any(|r| r.key == *k)).or_else(|| self.default_key(q));
    }

    /// The rows added after the last `sync` (they sit after the synchronous rows, around the web row).
    fn take_late(&mut self) -> Vec<Item> {
        let start = self.sync_len.min(self.rows.len());
        self.rows.drain(start..).filter(|r| r.key != web_search_key()).collect()
    }

    fn may_default(&self, r: &Item) -> bool {
        let blocked = self.mixed && is_executable_file(r);
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

    /// A late batch (files, tier 7). False, and nothing changes, unless it is of the current generation. Its rows go
    /// below the synchronous and earlier late rows, above the web row, unless the web row is selected: then below it.
    /// A row already shown is not added twice. With nothing selected, the first late row that may be the default is.
    pub fn late(&mut self, b: Batch, current_gen: u64) -> bool {
        if b.gen != current_gen || b.gen != self.gen {
            return false;
        }
        let first_late = self.sync_len + self.late_len;
        let below_web = self.selected_index().is_some_and(|i| i >= first_late);
        let mut at = if below_web { self.rows.len() } else { first_late };
        let from = at;
        for item in b.items {
            if self.rows.iter().any(|r| r.key == item.key) {
                continue;
            }
            self.rows.insert(at, item);
            at += 1;
            if !below_web {
                self.late_len += 1;
            }
        }
        if self.sel.is_none() {
            let added = self.rows.get(from..at).unwrap_or(&[]);
            self.sel = added.iter().find(|r| !r.destructive && self.may_default(r)).map(|r| r.key);
        }
        true
    }

    /// Takes a row out (a closed window, a forgotten line). The selection moves to the row that takes its place only
    /// when that row may be the default and is not destructive (5.2.7): nobody chose it, and Enter runs what is
    /// selected. Otherwise nothing is selected.
    pub fn remove(&mut self, key: ItemKey) -> Option<Item> {
        let i = self.rows.iter().position(|r| r.key == key)?;
        let item = self.rows.remove(i);
        if i < self.sync_len {
            self.sync_len -= 1;
        } else if i < self.sync_len + self.late_len {
            self.late_len -= 1;
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
    }

    impl Fx {
        pub(crate) fn new() -> Fx {
            let (apps, settings, windows, history) = fixture();
            Fx { apps, settings, windows, history, opts: LauncherOpts::default() }
        }

        pub(crate) fn cx(&self) -> Context<'_> {
            Context {
                apps: &self.apps,
                settings: &self.settings,
                windows: &self.windows,
                history: &self.history,
                opts: &self.opts,
                num: NUM_IT,
                now_unix: 1_790_000_000,
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
            res.sync(&q, query_all(&q, &self.cx(), &mut Budget::default()));
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
        assert_eq!(titles, ["Steam", "Visual Studio Code", "Restart", "Registry Editor", "Sleep"]);
        assert_eq!(res.selected().unwrap().title.as_ref(), "Steam");
        assert!(res.select_index(1), "the user moves to Visual Studio Code");
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
        assert_eq!(ORDER, [Calc, Apps, Windows, Commands, Settings, Run, Web, Registry]);
        let fx = Fx::new();
        assert_eq!(fx.query("x").1, [Calc, Apps, Commands, Settings, Run, Web, Registry]);
        assert_eq!(fx.query("<x").1, [Windows]);
        assert_eq!(fx.query("=1").1, [Calc]);
        assert_eq!(fx.query(">ping").1, [Run]);
        assert_eq!(fx.query("file explorer").1, [Apps], "a word prefix keeps the apps");
        // Rows of the same tier and kind keep the order of the providers: the answer of an expression comes first.
        assert_eq!(fx.rows("2+3")[0].kind, Kind::Answer);
    }

    #[test]
    fn tier6_runs_only_when_nothing_matched() {
        let fx = Fx::new();
        let rows = fx.rows("clacolatrice");
        assert_eq!((&*rows[0].title, rows[0].score.tier), ("Calcolatrice", 6));
        // "stea" matches Steam by prefix: no fuzzy rows next to it.
        assert!(fx.rows("stea").iter().all(|r| r.score.tier < 6));
    }

    #[test]
    fn word_prefix_keeps_apps_at_tier_1() {
        let mut fx = Fx::new();
        fx.apps.push(CatApp::new("Microsoft.Windows.Explorer2", "File Explorer", Keys::default(), 1));
        let rows = fx.rows("file explorer");
        assert_eq!(rows.len(), 1, "{rows:#?}");
        assert_eq!(&*rows[0].title, "File Explorer");
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
