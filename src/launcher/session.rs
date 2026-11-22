//! One open of the launcher (SPEC 5.2.9, 5.2.11): keys in, effects out. The session decides what a key does and
//! returns the effects in the order the host runs them; launcher_sys.rs executes them (L1a-15c), so nothing here calls
//! Windows and the tests' fake port is a `Vec<Effect>`. Order matters: a switch activates its window while the dock still
//! holds the foreground and hides after; a launch hides first so Explorer gets the foreground grant; a copy hides with
//! the restore so the text lands where the user came from.

use super::field::{Edit, FieldOut, KeyEvent, Motion, TextField};
use super::gesture::{self, Gesture, Refusal};
use super::model::{
    Batch, Choice, CommandId, FileClass, IconRef, Item, ItemKey, Kind, Mode, Mods, Payload, ProviderId, RunLine, Verb,
};
use super::providers::{self, Budget, Context, LauncherOpts, Results};
use super::query;
use super::rank;
use super::runline;
use super::verbs;

/// What the host does for the session, in order ("after close" effects run in `after_close`, L1a-15c).
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum Effect {
    /// Close the launcher; `restore` gives the foreground back to the window of the open.
    Hide {
        restore: bool,
    },
    /// Activate this window (the bar's activation), before the hide.
    Switch(isize),
    /// WM_CLOSE to this window; the launcher stays.
    CloseWindow(isize),
    Launch(LaunchReq),
    /// To the clipboard, after the hide and the restore.
    Copy(Box<str>),
    /// The field's selection to the clipboard now: the launcher stays open.
    CopyNow(Box<str>),
    Power(CommandId),
    /// The window menu of the window of the open, after the restore.
    WindowMenu,
    /// Settings > Launcher (L1b).
    OpenSettings,
    /// Forget a learned row (frecency, L1b).
    Forget(ItemKey),
    /// "Don't show again" on a notice row: the row has left the list; the host remembers the choice (L1b-5).
    DismissNotice(ItemKey),
    /// A notice's choice (5.7): the row has left the list; the host acts on it.
    Choose(Choice),
    /// Forget a run-history line.
    ForgetRun(Box<str>),
    /// Ask the files provider for this generation (L2).
    #[allow(dead_code)] // L2-2 builds it
    RequestFiles(u64),
    /// Nothing matched: ask Windows' typo rewrite for this generation (tier 7, L1a-15d).
    RequestRewrite {
        gen: u64,
    },
    /// Read the clipboard's text and hand it to `Session::paste` (Ctrl+V, Shift+Insert).
    Paste,
    /// The rows changed: lay the panel out again.
    Relayout,
    /// Only the caret, the selection or the selected row changed.
    Redraw,
}

/// A launch, mapped to a shell request by launcher_sys.rs (L1a-12i).
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum LaunchReq {
    App(Box<str>),
    AppAdmin(Box<str>),
    RevealApp(Box<str>),
    /// The index into the held settings list; the controller, which holds the list, takes its target and arguments.
    Setting {
        index: u32,
    },
    OpenPath(Box<str>),
    RevealPath(Box<str>),
    Command(RunLine),
    Console(RunLine),
    /// Built only here, only with a Gesture and the review conditions (L1a-9).
    RunAsTyped {
        line: RunLine,
        gesture: Gesture,
    },
    Url(Box<str>),
    Registry,
}

impl LaunchReq {
    /// The launches that end in Windows' UAC prompt: the hook forgets its tracked modifiers after them (5.2.11).
    pub fn elevates(&self) -> bool {
        matches!(self, LaunchReq::AppAdmin(_) | LaunchReq::RunAsTyped { .. })
    }
}

/// A key the hook kept while the launcher was not yet foreground, rebuilt by the view (L1a-15b).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ReplayKey {
    Text(char),
    Key(KeyEvent),
}

/// The row under the pointer, its generation and since when (the 300 ms rule of 5.2.7).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
struct Hover {
    key: ItemKey,
    gen: u64,
    since_ms: u64,
}

/// The pointer takes the selection only after this much travel since the last key (5.2.10).
const POINTER_TRAVEL_PX: i64 = 8;
/// A destructive row runs on a click only after this long under the pointer, unchanged (5.2.7).
const DESTRUCTIVE_HOVER_MS: u64 = 300;

/// The view's measure of the review detail of a row (5.3): its key, the generation it was measured in and the CAPTION
/// lines its whole text needs.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
struct Review {
    key: ItemKey,
    gen: u64,
    lines: u8,
}

/// The mode chip: a recognised prefix taken out of the field.
#[derive(Clone, PartialEq, Eq, Debug)]
struct Chip {
    id: ProviderId,
    prefix: Box<str>,
}

const VK_BACK: u16 = 0x08;
const VK_TAB: u16 = 0x09;
const VK_RETURN: u16 = 0x0D;
const VK_ESCAPE: u16 = 0x1B;
const VK_SPACE: u16 = 0x20;
const VK_PRIOR: u16 = 0x21;
const VK_NEXT: u16 = 0x22;
const VK_END: u16 = 0x23;
const VK_HOME: u16 = 0x24;
const VK_LEFT: u16 = 0x25;
const VK_UP: u16 = 0x26;
const VK_RIGHT: u16 = 0x27;
const VK_DOWN: u16 = 0x28;
const VK_INSERT: u16 = 0x2D;
const VK_DELETE: u16 = 0x2E;
const VK_APPS: u16 = 0x5D;
const VK_A: u16 = 0x41;
const VK_C: u16 = 0x43;
const VK_E: u16 = 0x45;
const VK_K: u16 = 0x4B;
const VK_OEM_COMMA: u16 = 0xBC;
const VK_V: u16 = 0x56;
const VK_W: u16 = 0x57;
const VK_X: u16 = 0x58;
const VK_F4: u16 = 0x73;
const VK_F10: u16 = 0x79;

const CTRL_SHIFT: Mods = Mods(Mods::CTRL.0 | Mods::SHIFT.0);

/// One open of the launcher.
#[derive(Debug)]
pub struct Session {
    field: TextField,
    chip: Option<Chip>,
    results: Results,
    /// The generation of the current rows: one per change of the text.
    gen: u64,
    /// Keys typed before the launcher was foreground are being replayed (L1a-15b).
    replaying: bool,
    /// Visible rows (PgUp, PgDn).
    rows: u8,
    /// The selection was made by arrow keys or the pointer, not taken by default (Ctrl+W, 5.2.9).
    picked: bool,
    /// The row the last Enter or click ran, when it is one that is learned (L1b-3); the host takes it.
    learn: Option<ItemKey>,
    /// The generation that last asked for the typo rewrite.
    rewrite_asked: Option<u64>,
    budget: Budget,
    /// The mode and the folded text of the current rows (Tab).
    mode: Mode,
    folded: String,
    /// The pointer's first position after the last key, and whether it has travelled 8 px since.
    pointer_from: Option<(i32, i32)>,
    pointer_live: bool,
    hover: Option<Hover>,
    /// Run as administrator acts only on a text the view has measured, for this row and this generation.
    review: Option<Review>,
    /// The action list over the selected row (Ctrl+K, L1b-2).
    actions: Option<ActionList>,
}

/// One entry of the action list: a verb of the row, a notice's choice, or the notice's own "Don't show again".
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Act {
    Verb(Verb),
    Choice(Choice),
    DontShowAgain,
}

/// An entry with what the view needs to draw it.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct ActEntry {
    pub act: Act,
    /// false: shown but inert (Enter does nothing). Run as administrator on a typed line or a file needs the hook's
    /// proof of Ctrl+Shift+Enter, which a key inside the list cannot give.
    pub enabled: bool,
    /// The glyph is drawn in the danger colour (a destructive verb, or Open on a destructive row).
    pub danger: bool,
    /// Open on a running app starts a new instance: "New window", on Shift+Enter (5.2.8).
    pub new_window: bool,
}

/// The action list over one row: its verbs in table order, the highlighted entry, and whether the highlight came from
/// the keyboard (the focus ring shows only then).
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct ActionList {
    row: ItemKey,
    items: Vec<ActEntry>,
    sel: usize,
    ring: bool,
}

impl ActionList {
    pub fn items(&self) -> &[ActEntry] {
        &self.items
    }

    pub fn sel(&self) -> usize {
        self.sel
    }

    pub fn ring(&self) -> bool {
        self.ring
    }
}

/// The entries of the action list of `item`: `verbs_for`'s set in `VERBS` order, then the notice's own. A notice with
/// choices lists them in place of its verb.
fn action_entries(item: &Item) -> Vec<ActEntry> {
    let entry = |act| ActEntry { act, enabled: true, danger: false, new_window: false };
    if let (Kind::Notice, Payload::Choices(choices)) = (item.kind, &item.payload) {
        return choices.iter().map(|c| entry(Act::Choice(*c))).chain([entry(Act::DontShowAgain)]).collect();
    }
    let running = matches!(item.payload, Payload::Group(_) | Payload::Hwnd(_));
    let primary = verbs::primary(item.kind, running);
    let mut out: Vec<ActEntry> = verbs::VERBS
        .iter()
        .filter(|spec| item.verbs.has(spec.verb))
        .map(|spec| ActEntry {
            act: Act::Verb(spec.verb),
            enabled: !(spec.verb == Verb::RunAsAdmin && matches!(item.kind, Kind::RunLine | Kind::File)),
            danger: spec.destructive || (item.destructive && spec.verb == primary),
            new_window: spec.verb == Verb::Open && item.kind == Kind::App && running,
        })
        .collect();
    if item.kind == Kind::Notice && item.verbs.has(Verb::Open) {
        out.push(entry(Act::DontShowAgain));
    }
    out
}

fn mru_window(cx: &Context, group: &str) -> Option<isize> {
    cx.windows.iter().find(|w| *w.group_key == *group).map(|w| w.hwnd)
}

/// The app id of an app row; a running app row carries its group key as payload and its id in its icon.
fn app_id(item: &Item) -> Option<&str> {
    match (&item.payload, &item.icon) {
        (Payload::AppId(id), _) | (Payload::Group(_), IconRef::App(id)) if item.kind == Kind::App => Some(id),
        _ => None,
    }
}

/// A drive-letter path (a group key can also be an AUMID, which has no folder to show).
fn is_drive_path(s: &str) -> bool {
    let b = s.as_bytes();
    b.first().is_some_and(u8::is_ascii_alphabetic) && b.get(1) == Some(&b':') && b.get(2) == Some(&b'\\')
}

fn launch(req: LaunchReq) -> Vec<Effect> {
    vec![Effect::Hide { restore: false }, Effect::Launch(req)]
}

fn copy_after_hide(text: &str) -> Vec<Effect> {
    vec![Effect::Hide { restore: true }, Effect::Copy(text.into())]
}

fn run_line(r: &RunLine) -> LaunchReq {
    if runline::runs_in_console(r) {
        LaunchReq::Console(r.clone())
    } else {
        LaunchReq::Command(r.clone())
    }
}

impl Session {
    /// A new open. With keep_query the previous query comes back fully selected, so typing replaces it (5.2.3).
    pub fn open(opts: &LauncherOpts, last_query: Option<&str>) -> Session {
        let mut field = TextField::new();
        if let Some(q) = last_query.filter(|q| opts.keep_query && !q.is_empty()) {
            field.set_text_selected(q);
        }
        Session {
            field,
            chip: None,
            results: Results::default(),
            gen: 0,
            replaying: false,
            rows: opts.rows,
            picked: false,
            learn: None,
            rewrite_asked: None,
            budget: Budget::default(),
            mode: Mode::Mixed,
            folded: String::new(),
            pointer_from: None,
            pointer_live: false,
            hover: None,
            review: None,
            actions: None,
        }
    }

    pub fn rows(&self) -> &[Item] {
        self.results.rows()
    }

    pub fn selected(&self) -> Option<&Item> {
        self.results.selected()
    }

    pub fn selected_index(&self) -> Option<usize> {
        self.results.selected_index()
    }

    /// The field's text (without the chip's prefix).
    pub fn query_text(&self) -> &str {
        self.field.text()
    }

    pub fn field(&self) -> &TextField {
        &self.field
    }

    pub fn chip(&self) -> Option<ProviderId> {
        self.chip.as_ref().map(|c| c.id)
    }

    /// The mode of the last query: mixed, or one provider's ("<" is the windows).
    pub fn mode(&self) -> Mode {
        self.mode
    }

    pub fn gen(&self) -> u64 {
        self.gen
    }

    /// Some row matched the query (notices and the web row do not count).
    pub fn has_matches(&self) -> bool {
        self.results.has_matches()
    }

    /// The text the view shows whole under the selected row (the review detail of 5.3) before Run as administrator can
    /// act on it: the typed run line, or the path of a file row that offers Run as administrator.
    #[cfg(test)]
    pub fn review_text(&self) -> Option<&str> {
        self.selected().and_then(review_text)
    }

    /// The view measured the review detail of the row `key` (`review_text`) with the real CAPTION font over the review
    /// width: its whole text needs `lines` lines. The measure holds for this generation only.
    pub fn set_review_lines(&mut self, key: ItemKey, lines: u8) {
        self.review = Some(Review { key, gen: self.gen, lines });
    }

    /// Why Ctrl+Shift+Enter on the selected row would be refused now; the view shows `Refusal::detail` in place of the
    /// review. None when the row has nothing to review or the review passes.
    pub fn admin_refusal(&self) -> Option<Refusal> {
        self.selected().and_then(|item| self.review_of(item)).and_then(Result::err)
    }

    /// The review conditions (5.4.9 conditions 3 and 4) of a row that offers Run as administrator on a typed text, with
    /// the view's measure of this row in this generation (none: not measured, refused). None: nothing to review.
    fn review_of(&self, item: &Item) -> Option<Result<(), Refusal>> {
        let text = review_text(item)?;
        let gen = self.gen;
        let lines = self.review.filter(|r| r.key == item.key && r.gen == gen).map_or(0, |r| r.lines);
        Some(gesture::review_ok(text, lines))
    }

    /// The query as typed, prefix included: what keep_query brings back at the next open.
    pub fn last_query(&self) -> String {
        let mut raw = String::from(self.chip.as_ref().map_or("", |c| &*c.prefix));
        raw.push_str(self.field.text());
        raw
    }

    /// Notice rows (5.5.2), shown first from the next query.
    pub fn set_notices(&mut self, notices: Vec<Item>) {
        self.results.set_notices(notices);
    }

    /// One more notice row after those already shown (an open that learns something new), from the next query.
    pub fn push_notice(&mut self, notice: Item) {
        self.results.push_notice(notice);
    }

    /// Runs the query again as a new generation: the rows of a kept query at open.
    pub fn refresh(&mut self, cx: &Context) -> Vec<Effect> {
        self.requery(cx)
    }

    /// A late batch (files, the typo rewrite): false when it belongs to another generation.
    pub fn late(&mut self, b: Batch, cx: &Context) -> bool {
        let _ = cx;
        let took = self.results.late(b, self.gen);
        if self.actions.as_ref().is_some_and(|a| self.selected().map(|i| i.key) != Some(a.row)) {
            self.actions = None;
        }
        took
    }

    /// The action list, when it is open.
    pub fn action_list(&self) -> Option<&ActionList> {
        self.actions.as_ref()
    }

    /// Alt+F4, SC_CLOSE and WM_CLOSE: the final Esc.
    pub fn close_request(&mut self) -> Vec<Effect> {
        vec![Effect::Hide { restore: true }]
    }

    /// The chord again while open: the final Esc.
    pub fn chord_again(&mut self) -> Vec<Effect> {
        vec![Effect::Hide { restore: true }]
    }

    /// Typed characters (WM_CHAR). Control characters are keys, handled by `key`, never text.
    pub fn chars(&mut self, s: &str, cx: &Context) -> Vec<Effect> {
        self.keyed();
        let text: String = s.chars().filter(|c| !c.is_control()).collect();
        if text.is_empty() {
            return Vec::new();
        }
        // Typing closes the action list and goes on searching.
        self.actions = None;
        self.edit(Edit::Insert(text), cx)
    }

    /// The clipboard's text (after `Effect::Paste`): line breaks and tabs become spaces, cut at 256 characters;
    /// control and bidi characters are kept and drawn as markers.
    pub fn paste(&mut self, text: &str, cx: &Context) -> Vec<Effect> {
        self.keyed();
        self.edit(Edit::Insert(text.into()), cx)
    }

    /// The keys the hook kept before the launcher was foreground. They behave as typed, except that Enter never runs a
    /// destructive row (it stays selected: the user typed blind) and Ctrl+Shift+Enter never has a gesture. A key that
    /// closes the launcher ends the replay.
    pub fn replay(&mut self, keys: &[ReplayKey], cx: &Context) -> Vec<Effect> {
        self.replaying = true;
        let mut out = Vec::new();
        for k in keys {
            let fx = match *k {
                ReplayKey::Text(c) => {
                    let mut buf = [0u8; 4];
                    self.chars(c.encode_utf8(&mut buf), cx)
                }
                ReplayKey::Key(ev) => self.key(ev, None, cx),
            };
            let closes = fx.iter().any(|e| matches!(e, Effect::Hide { .. }));
            out.extend(fx);
            if closes {
                break;
            }
        }
        self.replaying = false;
        let relayout = out.contains(&Effect::Relayout);
        let redraw = out.contains(&Effect::Redraw);
        let gen = self.gen;
        out.retain(|e| match e {
            Effect::Relayout | Effect::Redraw => false,
            Effect::RequestRewrite { gen: g } => *g == gen,
            _ => true,
        });
        if !out.iter().any(|e| matches!(e, Effect::Hide { .. })) {
            if relayout {
                out.push(Effect::Relayout);
            } else if redraw {
                out.push(Effect::Redraw);
            }
        }
        out
    }

    /// One key down (WM_KEYDOWN or WM_SYSKEYDOWN). `gesture` is the hook's proof of a person's Ctrl+Shift+Enter.
    pub fn key(&mut self, ev: KeyEvent, gesture: Option<Gesture>, cx: &Context) -> Vec<Effect> {
        self.keyed();
        let m = ev.mods;
        let plain = m == Mods::NONE;
        let ctrl = m.has(Mods::CTRL);
        let shift = m.has(Mods::SHIFT);
        let motion = |by: Motion, word: Motion| Edit::Move { by: if ctrl { word } else { by }, extend: shift };
        if ev.vk == VK_SPACE && m.has(Mods::ALT) {
            return self.chord_again();
        }
        if ev.vk == VK_F4 && m == Mods::ALT {
            return self.close_request();
        }
        if self.actions.is_some() {
            return self.action_key(ev, gesture, cx);
        }
        match ev.vk {
            VK_K if m == Mods::CTRL && !ev.repeat => self.open_actions(true),
            VK_F10 if m == Mods::SHIFT && !ev.repeat => self.open_actions(true),
            VK_APPS if plain && !ev.repeat => self.open_actions(true),
            VK_OEM_COMMA if m == Mods::CTRL && !ev.repeat => {
                vec![Effect::Hide { restore: false }, Effect::OpenSettings]
            }
            VK_X if m == Mods::CTRL => self.edit(Edit::Cut, cx),
            VK_ESCAPE if plain => self.escape(cx),
            VK_RETURN if ev.repeat => Vec::new(),
            VK_RETURN => self.enter(m, gesture, cx),
            VK_E if m == CTRL_SHIFT => self.reveal(cx),
            VK_C if m == Mods::CTRL => self.copy(cx),
            VK_V if m == Mods::CTRL => vec![Effect::Paste],
            VK_INSERT if m == Mods::SHIFT => vec![Effect::Paste],
            VK_A if m == Mods::CTRL => self.edit(Edit::SelectAll, cx),
            VK_W if m == Mods::CTRL && !ev.repeat => self.close_window(cx),
            VK_DELETE if m == Mods::SHIFT && !ev.repeat => self.forget(),
            VK_BACK if plain && self.chip.is_some() && self.field.caret() == 0 && self.field.selection().is_none() => {
                self.unchip(cx)
            }
            VK_BACK if plain => self.edit(Edit::Backspace, cx),
            VK_BACK if m == Mods::CTRL => self.edit(Edit::DeleteWordLeft, cx),
            VK_DELETE if plain => self.edit(Edit::Delete, cx),
            VK_DELETE if m == Mods::CTRL => self.edit(Edit::DeleteWordRight, cx),
            VK_LEFT if !m.has(Mods::ALT) => self.edit(motion(Motion::CharLeft, Motion::WordLeft), cx),
            VK_RIGHT if !m.has(Mods::ALT) => self.edit(motion(Motion::CharRight, Motion::WordRight), cx),
            VK_HOME if m == Mods::CTRL => self.select_edge(true),
            VK_END if m == Mods::CTRL => self.select_edge(false),
            VK_HOME if plain || m == Mods::SHIFT => self.edit(Edit::Move { by: Motion::Home, extend: shift }, cx),
            VK_END if plain || m == Mods::SHIFT => self.edit(Edit::Move { by: Motion::End, extend: shift }, cx),
            VK_UP if plain => self.move_selection(-1),
            VK_DOWN if plain => self.move_selection(1),
            VK_PRIOR if plain => self.move_selection(-isize::from(self.rows.max(1))),
            VK_NEXT if plain => self.move_selection(isize::from(self.rows.max(1))),
            VK_TAB if plain => self.complete(cx),
            _ => Vec::new(),
        }
    }

    // ---------------------------------------------------------------- the pointer

    /// A key: the pointer has to travel again before it takes the selection.
    fn keyed(&mut self) {
        self.pointer_from = None;
        self.pointer_live = false;
    }

    /// Notes which row (and generation) is under the pointer and since when.
    fn hover_at(&mut self, row: Option<usize>, now_ms: u64) {
        let gen = self.gen;
        let under = row.and_then(|i| self.rows().get(i)).map(|r| r.key);
        self.hover = match (under, self.hover) {
            (Some(key), Some(h)) if h.key == key && h.gen == gen => Some(h),
            (Some(key), _) => Some(Hover { key, gen, since_ms: now_ms }),
            (None, _) => None,
        };
    }

    /// The pointer moved to (x, y) over `row` (the view's hit test). It takes the selection only after 8 px of travel
    /// since the last key, so a resting pointer never steals the keyboard's choice.
    pub fn pointer(&mut self, x: i32, y: i32, row: Option<usize>, now_ms: u64) -> Vec<Effect> {
        self.hover_at(row, now_ms);
        let (x0, y0) = *self.pointer_from.get_or_insert((x, y));
        if !self.pointer_live {
            let dx = i64::from(x) - i64::from(x0);
            let dy = i64::from(y) - i64::from(y0);
            let travel = dx.saturating_mul(dx).saturating_add(dy.saturating_mul(dy));
            self.pointer_live = travel >= POINTER_TRAVEL_PX * POINTER_TRAVEL_PX;
        }
        match row {
            Some(i) if self.pointer_live => self.pick(i),
            _ => Vec::new(),
        }
    }

    /// A click on `row` runs its primary verb. A destructive row runs only when that row, of this generation, has been
    /// under the pointer for 300 ms; an earlier click (or one on a row that just moved there) only selects it.
    pub fn click(&mut self, row: usize, now_ms: u64, cx: &Context) -> Vec<Effect> {
        if self.actions.is_some() {
            return self.close_actions();
        }
        self.hover_at(Some(row), now_ms);
        let Some(item) = self.rows().get(row).cloned() else { return Vec::new() };
        let selected = self.pick(row);
        if item.destructive {
            let gen = self.gen;
            let held = self.hover.is_some_and(|h| {
                h.key == item.key && h.gen == gen && now_ms.saturating_sub(h.since_ms) >= DESTRUCTIVE_HOVER_MS
            });
            if !held {
                return selected;
            }
        }
        let run = self.primary(&item, cx);
        if run.is_empty() {
            selected
        } else {
            run
        }
    }

    // ---------------------------------------------------------------- the field and the query

    fn edit(&mut self, e: Edit, cx: &Context) -> Vec<Effect> {
        let insert = matches!(e, Edit::Insert(_));
        match self.field.apply(e) {
            FieldOut::Unchanged => Vec::new(),
            FieldOut::CaretOnly => vec![Effect::Redraw],
            FieldOut::Changed => {
                if insert {
                    self.take_chip(cx);
                }
                self.requery(cx)
            }
            // Ctrl+X: the text goes to the clipboard now, the launcher stays.
            FieldOut::Cut(text) => {
                let mut fx = vec![Effect::CopyNow(text.into())];
                fx.extend(self.requery(cx));
                fx
            }
        }
    }

    /// A prefix just typed at the start of the field becomes the mode chip (5.2.4).
    fn take_chip(&mut self, cx: &Context) {
        if self.chip.is_some() {
            return;
        }
        let text = self.field.text();
        let Some((len, id, _)) = cx.opts.prefixes.find(text) else { return };
        if self.field.caret() < len {
            return;
        }
        let (Some(prefix), Some(rest)) = (text.get(..len), text.get(len..)) else { return };
        let caret = self.field.caret() - len;
        self.chip = Some(Chip { id, prefix: prefix.into() });
        let rest = String::from(rest);
        self.field.set_text(&rest);
        self.field.set_caret(caret);
    }

    /// Backspace at caret 0: the chip goes back into the field as text, the caret after it.
    fn unchip(&mut self, cx: &Context) -> Vec<Effect> {
        let Some(chip) = self.chip.take() else { return Vec::new() };
        let mut text = String::from(&*chip.prefix);
        text.push_str(self.field.text());
        self.field.set_text(&text);
        self.field.set_caret(chip.prefix.len());
        self.requery(cx)
    }

    /// The text changed: a new generation, new rows, the default selection.
    fn requery(&mut self, cx: &Context) -> Vec<Effect> {
        self.gen += 1;
        self.picked = false;
        self.actions = None;
        let raw = self.last_query();
        let q = query::parse_query(&raw, &cx.opts.prefixes, cx.num, self.gen);
        let top = providers::query_all(&q, cx, &mut self.budget);
        self.results.sync(&q, top, usize::from(cx.opts.rows));
        self.mode = q.mode;
        self.folded.clone_from(&q.folded);
        let mut fx = vec![Effect::Relayout];
        let nothing = q.mode == Mode::Mixed && !q.folded.is_empty() && !self.results.has_matches();
        if nothing && self.rewrite_asked != Some(self.gen) {
            self.rewrite_asked = Some(self.gen);
            fx.push(Effect::RequestRewrite { gen: self.gen });
        }
        fx
    }

    /// The Esc ladder: (the action list closes first, `action_key`), then the query and the chip, then the final Esc.
    fn escape(&mut self, cx: &Context) -> Vec<Effect> {
        if !self.field.is_empty() || self.chip.is_some() {
            self.field.clear();
            self.chip = None;
            return self.requery(cx);
        }
        vec![Effect::Hide { restore: true }]
    }

    /// Tab: in Run mode the selected suggestion (or the first one) goes into the field; otherwise the selected row's
    /// name when it starts with the query.
    fn complete(&mut self, cx: &Context) -> Vec<Effect> {
        let pick = if self.mode == Mode::Only(ProviderId::Run) {
            let sel = self.selected().filter(|r| r.kind == Kind::RunSuggestion);
            sel.or_else(|| self.rows().iter().find(|r| r.kind == Kind::RunSuggestion)).map(|r| r.title.clone())
        } else {
            self.selected().map(|r| r.title.clone()).filter(|t| {
                let mut f = String::with_capacity(t.len());
                rank::fold(t, &mut f);
                !self.folded.is_empty() && f.starts_with(&self.folded)
            })
        };
        let Some(text) = pick else { return Vec::new() };
        self.set_field(&text, cx)
    }

    /// Puts a completion in the field, after the prefix (as a chip or as typed text), and runs it.
    fn set_field(&mut self, text: &str, cx: &Context) -> Vec<Effect> {
        let inline = match self.chip {
            Some(_) => None,
            None => cx.opts.prefixes.find(self.field.text()).and_then(|(len, ..)| self.field.text().get(..len)),
        };
        let mut new = String::from(inline.unwrap_or(""));
        new.push_str(text);
        if new == self.field.text() {
            return Vec::new();
        }
        self.field.set_text(&new);
        self.requery(cx)
    }

    // ---------------------------------------------------------------- the selection

    fn move_selection(&mut self, delta: isize) -> Vec<Effect> {
        let n = self.rows().len();
        if n == 0 {
            return Vec::new();
        }
        let last = n - 1;
        let next = match self.selected_index() {
            None => 0,
            Some(i) => i.saturating_add_signed(delta).min(last),
        };
        self.pick(next)
    }

    fn select_edge(&mut self, first: bool) -> Vec<Effect> {
        match self.rows().len() {
            0 => Vec::new(),
            n => self.pick(if first { 0 } else { n - 1 }),
        }
    }

    /// The user chose a row (arrows, pointer).
    fn pick(&mut self, i: usize) -> Vec<Effect> {
        let moved = self.selected_index() != Some(i);
        if !self.results.select_index(i) {
            return Vec::new();
        }
        self.picked = true;
        if moved {
            vec![Effect::Redraw]
        } else {
            Vec::new()
        }
    }

    // ---------------------------------------------------------------- the action list (L1b-2)

    /// Opens the action list over the selected row: Ctrl+K, Shift+F10 and the Menu key (`ring`: the focus ring shows),
    /// or a right click (`context_menu`). A row with nothing to offer, a replay and a list already open do nothing.
    fn open_actions(&mut self, ring: bool) -> Vec<Effect> {
        if self.replaying || self.actions.is_some() {
            return Vec::new();
        }
        let Some(item) = self.selected() else { return Vec::new() };
        let items = action_entries(item);
        if items.is_empty() {
            return Vec::new();
        }
        // The list opens on what Enter would do, so Ctrl+K and Enter repeat it.
        let running = matches!(item.payload, Payload::Group(_) | Payload::Hwnd(_));
        let primary = Act::Verb(verbs::primary(item.kind, running));
        // A notice's choices: its first one (0).
        let sel = items.iter().position(|e| e.act == primary && e.enabled).unwrap_or(0);
        self.actions = Some(ActionList { row: item.key, items, sel, ring });
        vec![Effect::Relayout]
    }

    /// A right click on a row: it is selected and its action list opens (the focus ring stays off). With a list open the
    /// click only closes it.
    pub fn context_menu(&mut self, row: usize) -> Vec<Effect> {
        if self.actions.is_some() {
            return self.close_actions();
        }
        if self.results.select_index(row) {
            self.picked = true;
        }
        self.open_actions(false)
    }

    /// Closes the action list (Esc, a click outside it, a second Ctrl+K).
    pub fn close_actions(&mut self) -> Vec<Effect> {
        if self.actions.take().is_some() {
            vec![Effect::Relayout]
        } else {
            Vec::new()
        }
    }

    /// The pointer is over entry `k` of the list: it takes the highlight, without the focus ring.
    pub fn action_hover(&mut self, k: usize) -> Vec<Effect> {
        let Some(list) = self.actions.as_mut() else { return Vec::new() };
        if k >= list.items.len() || (list.sel == k && !list.ring) {
            return Vec::new();
        }
        list.sel = k;
        list.ring = false;
        vec![Effect::Redraw]
    }

    /// A click on entry `k` of the list.
    pub fn action_click(&mut self, k: usize, cx: &Context) -> Vec<Effect> {
        let Some(list) = self.actions.as_mut() else { return Vec::new() };
        if k >= list.items.len() {
            return Vec::new();
        }
        list.sel = k;
        list.ring = false;
        self.run_action(cx)
    }

    /// A key while the list is open: Up, Down, Home, End and Enter move and run inside it, Esc and the opening keys
    /// close it, Backspace and Delete close it and edit the field; every other key waits.
    fn action_key(&mut self, ev: KeyEvent, gesture: Option<Gesture>, cx: &Context) -> Vec<Effect> {
        let m = ev.mods;
        let plain = m == Mods::NONE;
        match ev.vk {
            VK_ESCAPE if plain => self.close_actions(),
            VK_K if m == Mods::CTRL => self.close_actions(),
            VK_F10 if m == Mods::SHIFT => self.close_actions(),
            VK_APPS if plain => self.close_actions(),
            VK_UP if plain => self.action_move(-1),
            VK_DOWN if plain => self.action_move(1),
            VK_HOME if plain => self.action_move(isize::MIN),
            VK_END if plain => self.action_move(isize::MAX),
            VK_RETURN if plain && !ev.repeat => self.run_action(cx),
            // Editing chords close the list and act on the field, as typing does.
            VK_BACK | VK_DELETE if plain || m == Mods::CTRL => self.close_and_edit(ev, gesture, cx),
            VK_X | VK_A | VK_V if m == Mods::CTRL => self.close_and_edit(ev, gesture, cx),
            VK_INSERT if m == Mods::SHIFT => self.close_and_edit(ev, gesture, cx),
            VK_LEFT | VK_RIGHT if !m.has(Mods::ALT) => self.close_and_edit(ev, gesture, cx),
            _ => Vec::new(),
        }
    }

    /// Closes the action list and hands the key to the field; the panel gives the list's room back.
    fn close_and_edit(&mut self, ev: KeyEvent, gesture: Option<Gesture>, cx: &Context) -> Vec<Effect> {
        self.actions = None;
        let mut fx = self.key(ev, gesture, cx);
        if !fx.contains(&Effect::Relayout) {
            fx.push(Effect::Relayout);
        }
        fx
    }

    fn action_move(&mut self, delta: isize) -> Vec<Effect> {
        let Some(list) = self.actions.as_mut() else { return Vec::new() };
        let last = list.items.len().saturating_sub(1);
        let sel = list.sel.saturating_add_signed(delta).min(last);
        let changed = sel != list.sel || !list.ring;
        list.sel = sel;
        list.ring = true;
        if changed {
            vec![Effect::Redraw]
        } else {
            Vec::new()
        }
    }

    /// Runs the highlighted entry and closes the list.
    fn run_action(&mut self, cx: &Context) -> Vec<Effect> {
        let Some(list) = self.actions.as_ref() else { return Vec::new() };
        let Some(entry) = list.items.get(list.sel).copied() else { return self.close_actions() };
        if !entry.enabled {
            return Vec::new();
        }
        let row = list.row;
        self.actions = None;
        let Some(item) = self.selected().filter(|i| i.key == row).cloned() else { return vec![Effect::Relayout] };
        let mut fx = match entry.act {
            Act::Verb(v) => self.run_verb(&item, v, cx),
            Act::Choice(c) => self.choose(&item, c),
            Act::DontShowAgain => {
                self.results.remove(item.key);
                vec![Effect::DismissNotice(item.key)]
            }
        };
        // The panel gives the list's room back, unless the launcher closes anyway.
        if !fx.iter().any(|e| matches!(e, Effect::Hide { .. } | Effect::Relayout)) {
            fx.push(Effect::Relayout);
        }
        fx
    }

    /// One verb on the selected row, by its menu entry rather than its chord.
    fn run_verb(&mut self, item: &Item, v: Verb, cx: &Context) -> Vec<Effect> {
        match v {
            // A new instance for an app (also a running one); every other row opens as Enter does.
            Verb::Open if item.kind == Kind::App => {
                let fx = app_id(item).map_or_else(Vec::new, |id| launch(LaunchReq::App(id.into())));
                self.note_pick(item, fx)
            }
            Verb::Open | Verb::Switch => self.primary(item, cx),
            Verb::RunAsAdmin => self.run_as_admin(item, None, cx),
            Verb::Reveal => self.reveal(cx),
            Verb::Copy => self.copy_row(cx),
            Verb::CloseWindow => self.close_window_of_selected(cx),
            Verb::Forget => self.forget(),
            Verb::RunInConsole => self.shift_enter(item),
        }
    }

    // ---------------------------------------------------------------- verbs

    /// Enter and its modifiers on the selected row (table 5.2.8).
    fn enter(&mut self, m: Mods, gesture: Option<Gesture>, cx: &Context) -> Vec<Effect> {
        let Some(item) = self.selected().cloned() else { return Vec::new() };
        if self.replaying && (item.destructive || item.kind == Kind::Notice) {
            // Typed blind before the launcher showed: it stays selected for a real Enter (a notice's choice included).
            return Vec::new();
        }
        match m {
            Mods::NONE => self.primary(&item, cx),
            Mods::SHIFT => self.shift_enter(&item),
            Mods::CTRL => self.reveal(cx),
            CTRL_SHIFT => self.run_as_admin(&item, gesture, cx),
            _ => Vec::new(),
        }
    }

    /// The key of the row the last action ran, when frecency learns it: apps, settings and the non-destructive
    /// commands. Answers, run lines, web rows, windows and files are never learned.
    pub fn take_learn(&mut self) -> Option<ItemKey> {
        self.learn.take()
    }

    fn note_pick(&mut self, item: &Item, fx: Vec<Effect>) -> Vec<Effect> {
        let learned = matches!(
            (item.from, item.kind),
            (ProviderId::Apps, Kind::App)
                | (ProviderId::Settings, Kind::Setting)
                | (ProviderId::Commands, Kind::Command)
        );
        if learned && !item.destructive && !fx.is_empty() {
            self.learn = Some(item.key);
        }
        fx
    }

    fn primary(&mut self, item: &Item, cx: &Context) -> Vec<Effect> {
        let fx = self.primary_effects(item, cx);
        self.note_pick(item, fx)
    }

    fn primary_effects(&mut self, item: &Item, cx: &Context) -> Vec<Effect> {
        let running = matches!(item.payload, Payload::Group(_) | Payload::Hwnd(_));
        if !item.verbs.has(verbs::primary(item.kind, running)) {
            return Vec::new();
        }
        match &item.payload {
            Payload::AppId(id) => launch(LaunchReq::App(id.clone())),
            Payload::Group(g) => match mru_window(cx, g) {
                Some(h) => vec![Effect::Switch(h), Effect::Hide { restore: false }],
                None => Vec::new(),
            },
            Payload::Hwnd(h) => vec![Effect::Switch(*h), Effect::Hide { restore: false }],
            Payload::Setting(index) => launch(LaunchReq::Setting { index: *index }),
            Payload::Command(id) => command(*id),
            Payload::Value { shown, .. } => copy_after_hide(shown),
            Payload::RunLine(r) => launch(run_line(r)),
            Payload::Url(u) => launch(LaunchReq::Url(u.clone())),
            Payload::Path { path, .. } => launch(LaunchReq::OpenPath(path.clone())),
            Payload::Choices(choices) => choices.first().map_or_else(Vec::new, |c| self.choose(item, *c)),
            Payload::None => Vec::new(),
        }
    }

    /// A notice's choice: the row leaves the list for the rest of this open, and the host acts on the choice.
    fn choose(&mut self, item: &Item, c: Choice) -> Vec<Effect> {
        self.results.remove(item.key);
        vec![Effect::Choose(c), Effect::Relayout]
    }

    /// Shift+Enter: a new instance of an app, the raw value of an answer, a line in a console kept open.
    fn shift_enter(&mut self, item: &Item) -> Vec<Effect> {
        match (&item.kind, &item.payload) {
            (Kind::App, _) => {
                let fx = app_id(item).map_or_else(Vec::new, |id| launch(LaunchReq::App(id.into())));
                self.note_pick(item, fx)
            }
            (Kind::Answer, Payload::Value { raw, .. }) => copy_after_hide(raw),
            (Kind::RunLine | Kind::RunSuggestion, Payload::RunLine(r)) => launch(LaunchReq::Console(r.clone())),
            _ => Vec::new(),
        }
    }

    /// Ctrl+Enter and Ctrl+Shift+E: show the app, the program of a window or the path in its folder.
    fn reveal(&mut self, cx: &Context) -> Vec<Effect> {
        let Some(item) = self.selected() else { return Vec::new() };
        if !item.verbs.has(Verb::Reveal) {
            return Vec::new();
        }
        let req = match (&item.kind, &item.payload) {
            (Kind::App, _) => app_id(item).map(|id| LaunchReq::RevealApp(id.into())),
            (Kind::Window, Payload::Group(g)) => is_drive_path(g).then(|| LaunchReq::RevealPath(g.clone())),
            (Kind::Window, Payload::Hwnd(h)) => cx
                .windows
                .iter()
                .find(|w| w.hwnd == *h)
                .filter(|w| is_drive_path(&w.group_key))
                .map(|w| LaunchReq::RevealPath(w.group_key.clone())),
            (_, Payload::Path { path, .. }) => Some(LaunchReq::RevealPath(path.clone())),
            _ => None,
        };
        req.map_or_else(Vec::new, launch)
    }

    /// Ctrl+Shift+Enter. An app asks UAC through Explorer (no gesture needed: the app is the catalogue's); a run
    /// suggestion only goes into the field, where it becomes typed text. The typed line and an executable file need the
    /// hook-proven gesture and the review conditions (5.4.9); a file needs an exe, msi, bat or cmd path that is not on
    /// the network. The view resets the hook's tracked modifiers after the launch.
    fn run_as_admin(&mut self, item: &Item, gesture: Option<Gesture>, cx: &Context) -> Vec<Effect> {
        match (&item.kind, &item.payload) {
            (Kind::App, _) if item.verbs.has(Verb::RunAsAdmin) => {
                app_id(item).map_or_else(Vec::new, |id| launch(LaunchReq::AppAdmin(id.into())))
            }
            (Kind::RunSuggestion, Payload::RunLine(r)) => {
                let line = r.line.clone();
                self.set_field(&line, cx)
            }
            (Kind::RunLine, Payload::RunLine(r)) => match gesture {
                Some(gesture) if self.review_of(item) == Some(Ok(())) => {
                    launch(LaunchReq::RunAsTyped { line: r.clone(), gesture })
                }
                _ => Vec::new(),
            },
            (Kind::File, Payload::Path { path, .. }) => match (gesture, gesture::elevated_file_line(path)) {
                (Some(gesture), Some(line)) if self.review_of(item) == Some(Ok(())) => {
                    launch(LaunchReq::RunAsTyped { line, gesture })
                }
                _ => Vec::new(),
            },
            _ => Vec::new(),
        }
    }

    /// Ctrl+C: the field's selection now; otherwise the selected row's text, after the hide.
    fn copy(&mut self, cx: &Context) -> Vec<Effect> {
        if let Some(sel) = self.field.selected_text() {
            return vec![Effect::CopyNow(sel.into())];
        }
        self.copy_row(cx)
    }

    /// The selected row's text, after the hide.
    fn copy_row(&mut self, cx: &Context) -> Vec<Effect> {
        let Some(item) = self.selected() else { return Vec::new() };
        if !item.verbs.has(Verb::Copy) {
            return Vec::new();
        }
        let text: Option<&str> = match &item.payload {
            Payload::Value { shown, .. } => Some(shown),
            Payload::RunLine(r) => Some(&r.line),
            Payload::Url(u) => Some(u),
            Payload::Path { path, .. } => Some(path),
            Payload::Hwnd(h) => cx.windows.iter().find(|w| w.hwnd == *h).and_then(|w| w.title.as_deref()),
            _ => Some(&item.title),
        };
        text.map_or_else(Vec::new, copy_after_hide)
    }

    /// Ctrl+W: WM_CLOSE to the window of a row the user chose by arrow or pointer, never to the default row; the row
    /// goes, the launcher stays.
    fn close_window(&mut self, cx: &Context) -> Vec<Effect> {
        if !self.picked {
            return Vec::new();
        }
        self.close_window_of_selected(cx)
    }

    /// WM_CLOSE to the window of the selected row (the action list's Close window is a choice of the user's, so it does
    /// not need the arrow or pointer pick).
    fn close_window_of_selected(&mut self, cx: &Context) -> Vec<Effect> {
        let Some(item) = self.selected() else { return Vec::new() };
        if !item.verbs.has(Verb::CloseWindow) {
            return Vec::new();
        }
        let hwnd = match &item.payload {
            Payload::Hwnd(h) => Some(*h),
            Payload::Group(g) => mru_window(cx, g),
            _ => None,
        };
        let Some(h) = hwnd else { return Vec::new() };
        let key = item.key;
        self.results.remove(key);
        self.picked = false;
        vec![Effect::CloseWindow(h), Effect::Relayout]
    }

    /// Shift+Delete: a history line is forgotten and leaves the list; an app's learned rank is forgotten (L1b).
    fn forget(&mut self) -> Vec<Effect> {
        let Some(item) = self.selected() else { return Vec::new() };
        if !item.verbs.has(Verb::Forget) {
            return Vec::new();
        }
        match (&item.kind, &item.payload) {
            (Kind::RunSuggestion, Payload::RunLine(r)) => {
                let (key, line) = (item.key, r.line.clone());
                self.results.remove(key);
                vec![Effect::ForgetRun(line), Effect::Relayout]
            }
            _ => vec![Effect::Forget(item.key)],
        }
    }
}

/// The text a row shows whole before Run as administrator (5.3 review detail): a typed run line, or the path of a file
/// that `run_as_admin` would run (`gesture::elevated_file_line`), and only on a row that offers the verb.
fn review_text(item: &Item) -> Option<&str> {
    if !item.verbs.has(Verb::RunAsAdmin) {
        return None;
    }
    match (&item.kind, &item.payload) {
        (Kind::RunLine, Payload::RunLine(r)) => Some(&r.line),
        (Kind::File, Payload::Path { path, dir: false, class: FileClass::Executable })
            if gesture::elevated_file_line(path).is_some() =>
        {
            Some(path)
        }
        _ => None,
    }
}

/// What a command row runs: power commands after the hide, the window menu after the restore.
fn command(id: CommandId) -> Vec<Effect> {
    match id {
        CommandId::Lock | CommandId::Sleep | CommandId::Restart | CommandId::ShutDown | CommandId::SignOut => {
            vec![Effect::Hide { restore: false }, Effect::Power(id)]
        }
        CommandId::Registry => launch(LaunchReq::Registry),
        CommandId::WindowMenu => vec![Effect::Hide { restore: true }, Effect::WindowMenu],
        // L2-6.
        CommandId::ClipboardHistory => Vec::new(),
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::indexing_slicing)]
mod tests {
    use super::*;
    use crate::launcher::model::VerbSet;
    use crate::launcher::providers::tests::{Fx, NOTEPAD_KEY};

    const NOTEPAD_ID: &str = "Microsoft.WindowsNotepad_8wekyb3d8bbwe!App";
    const CALC_ID: &str = "Microsoft.WindowsCalculator_8wekyb3d8bbwe!App";

    fn k(vk: u16, mods: Mods) -> KeyEvent {
        KeyEvent { vk, mods, repeat: false, sys: mods.has(Mods::ALT) }
    }

    fn open(fx: &Fx) -> Session {
        Session::open(&fx.opts, None)
    }

    /// The session after typing `text` one character at a time.
    fn typed(fx: &Fx, text: &str) -> Session {
        let mut s = open(fx);
        for c in text.chars() {
            let mut b = [0u8; 4];
            s.chars(c.encode_utf8(&mut b), &fx.cx());
        }
        s
    }

    fn press(s: &mut Session, fx: &Fx, vk: u16, mods: Mods) -> Vec<Effect> {
        s.key(k(vk, mods), None, &fx.cx())
    }

    fn sel_title(s: &Session) -> Option<&str> {
        s.selected().map(|r| &*r.title)
    }

    #[test]
    fn only_apps_settings_and_plain_commands_are_learned() {
        let fx = Fx::new();
        let learned = |text: &str, m: Mods| {
            let mut s = typed(&fx, text);
            press(&mut s, &fx, VK_RETURN, m);
            s.take_learn()
        };
        let key = |p: ProviderId, id: &str| Some(ItemKey::of(p, id));
        assert_eq!(learned("calcolatrice", Mods::NONE), key(ProviderId::Apps, CALC_ID));
        // A new instance and a running app's switch count as picks of the app.
        assert_eq!(learned("blocco", Mods::SHIFT), key(ProviderId::Apps, NOTEPAD_ID));
        assert_eq!(learned("blocco", Mods::NONE), key(ProviderId::Apps, NOTEPAD_ID));
        assert_eq!(learned("schermo", Mods::NONE), key(ProviderId::Settings, "Schermo"));
        assert_eq!(learned("lock", Mods::NONE), key(ProviderId::Commands, "Lock"));
        // Never: a destructive command, a typed line, an answer, a web row, a window the catalogue does not know.
        assert_eq!(learned("shut down", Mods::NONE), None);
        assert_eq!(learned("> ping 1.1.1.1", Mods::NONE), None);
        assert_eq!(learned("2+2", Mods::NONE), None);
        assert_eq!(learned("zzqq", Mods::NONE), None);
        assert_eq!(learned("footool", Mods::NONE), None);
        // A key that does nothing learns nothing, and the pick is taken once.
        assert_eq!(learned("calcolatrice", Mods::ALT), None);
        let mut s = typed(&fx, "lock");
        press(&mut s, &fx, VK_RETURN, Mods::NONE);
        assert!(s.take_learn().is_some() && s.take_learn().is_none());
    }

    #[test]
    fn enter_on_running_app_switches_shift_enter_opens() {
        let fx = Fx::new();
        let mut s = typed(&fx, "blocco");
        assert_eq!(sel_title(&s), Some("Blocco note"));
        assert_eq!(
            press(&mut s, &fx, VK_RETURN, Mods::NONE),
            [Effect::Switch(0x1001), Effect::Hide { restore: false }]
        );
        assert_eq!(
            press(&mut s, &fx, VK_RETURN, Mods::SHIFT),
            [Effect::Hide { restore: false }, Effect::Launch(LaunchReq::App(NOTEPAD_ID.into()))]
        );
        assert_eq!(
            press(&mut s, &fx, VK_RETURN, Mods::CTRL),
            [Effect::Hide { restore: false }, Effect::Launch(LaunchReq::RevealApp(NOTEPAD_ID.into()))]
        );
        assert_eq!(
            press(&mut s, &fx, VK_RETURN, CTRL_SHIFT),
            [Effect::Hide { restore: false }, Effect::Launch(LaunchReq::AppAdmin(NOTEPAD_ID.into()))]
        );
        // A held Enter does nothing more.
        let held = KeyEvent { repeat: true, ..k(VK_RETURN, Mods::NONE) };
        assert!(s.key(held, None, &fx.cx()).is_empty());
        // A running program the catalogue does not know switches to its window too.
        let mut s = typed(&fx, "footool");
        assert_eq!(
            press(&mut s, &fx, VK_RETURN, Mods::NONE),
            [Effect::Switch(0x2002), Effect::Hide { restore: false }]
        );
        // A group with no window left does nothing.
        let mut fx2 = Fx::new();
        let mut s = typed(&fx2, "blocco");
        fx2.windows.clear();
        assert!(press(&mut s, &fx2, VK_RETURN, Mods::NONE).is_empty());
    }

    #[test]
    fn sl_enter_never_sleeps() {
        let fx = Fx::new();
        for q in ["s", "sl", "sle", "re", "sh", "spe"] {
            let mut s = typed(&fx, q);
            let fx_enter = press(&mut s, &fx, VK_RETURN, Mods::NONE);
            assert!(!fx_enter.iter().any(|e| matches!(e, Effect::Power(_))), "{q}: {fx_enter:?}");
        }
        // Chosen by arrow, it runs: no confirmation dialog (Q4).
        let mut s = typed(&fx, "sl");
        press(&mut s, &fx, VK_DOWN, Mods::NONE);
        assert_eq!(
            press(&mut s, &fx, VK_RETURN, Mods::NONE),
            [Effect::Hide { restore: false }, Effect::Power(CommandId::Sleep)]
        );
    }

    #[test]
    fn esc_ladder() {
        let fx = Fx::new();
        let mut s = typed(&fx, "calc");
        assert!(!s.rows().is_empty());
        assert_eq!(press(&mut s, &fx, VK_ESCAPE, Mods::NONE), [Effect::Relayout]);
        assert_eq!((s.query_text(), s.rows().len()), ("", 0));
        assert_eq!(press(&mut s, &fx, VK_ESCAPE, Mods::NONE), [Effect::Hide { restore: true }]);
        // The chip goes with the query.
        let mut s = typed(&fx, "=3");
        assert_eq!((s.chip(), s.query_text()), (Some(ProviderId::Calc), "3"));
        assert_eq!(press(&mut s, &fx, VK_ESCAPE, Mods::NONE), [Effect::Relayout]);
        assert_eq!((s.chip(), s.query_text()), (None, ""));
        assert_eq!(press(&mut s, &fx, VK_ESCAPE, Mods::NONE), [Effect::Hide { restore: true }]);
    }

    #[test]
    fn chord_again_is_final_esc() {
        let fx = Fx::new();
        let mut s = typed(&fx, "calc");
        assert_eq!(s.chord_again(), [Effect::Hide { restore: true }]);
        assert_eq!(press(&mut s, &fx, VK_SPACE, Mods::ALT), [Effect::Hide { restore: true }]);
        assert_eq!(press(&mut s, &fx, VK_SPACE, Mods::ALT.with(Mods::SHIFT)), [Effect::Hide { restore: true }]);
        assert_eq!(s.query_text(), "calc", "the query is the controller's to keep or drop");
    }

    #[test]
    fn close_request_is_final_esc() {
        let fx = Fx::new();
        let mut s = typed(&fx, "calc");
        assert_eq!(s.close_request(), [Effect::Hide { restore: true }]);
        assert_eq!(press(&mut s, &fx, VK_F4, Mods::ALT), [Effect::Hide { restore: true }]);
    }

    #[test]
    fn ctrl_c_copies_field_selection_else_row() {
        let fx = Fx::new();
        let mut s = typed(&fx, "calc");
        assert_eq!(press(&mut s, &fx, VK_A, Mods::CTRL), [Effect::Redraw]);
        assert_eq!(press(&mut s, &fx, VK_C, Mods::CTRL), [Effect::CopyNow("calc".into())]);
        press(&mut s, &fx, VK_RIGHT, Mods::NONE);
        assert_eq!(
            press(&mut s, &fx, VK_C, Mods::CTRL),
            [Effect::Hide { restore: true }, Effect::Copy("Calcolatrice".into())]
        );
        // A "<" row copies the window's title.
        let mut s = typed(&fx, "<senza");
        assert_eq!(
            press(&mut s, &fx, VK_C, Mods::CTRL),
            [Effect::Hide { restore: true }, Effect::Copy("Senza titolo - Blocco note".into())]
        );
        // Nothing selected: nothing.
        let mut s = typed(&fx, "sl");
        assert!(press(&mut s, &fx, VK_C, Mods::CTRL).is_empty());
    }

    #[test]
    fn copy_restores_then_copies() {
        let fx = Fx::new();
        let mut s = typed(&fx, "=3,5*2");
        assert_eq!(s.selected().unwrap().kind, Kind::Answer);
        assert_eq!(
            press(&mut s, &fx, VK_RETURN, Mods::NONE),
            [Effect::Hide { restore: true }, Effect::Copy("7".into())]
        );
        let mut s = typed(&fx, "1000,5*2");
        assert_eq!(
            press(&mut s, &fx, VK_RETURN, Mods::SHIFT),
            [Effect::Hide { restore: true }, Effect::Copy("2001".into())],
            "the raw value"
        );
        // The window menu restores first too.
        let mut s = typed(&fx, "window menu");
        assert_eq!(press(&mut s, &fx, VK_RETURN, Mods::NONE), [Effect::Hide { restore: true }, Effect::WindowMenu]);
    }

    #[test]
    fn launches_hide_without_restore() {
        let fx = Fx::new();
        let hide = Effect::Hide { restore: false };
        let cases: [(&str, Effect); 6] = [
            ("calcolatrice", Effect::Launch(LaunchReq::App(CALC_ID.into()))),
            ("lock", Effect::Power(CommandId::Lock)),
            ("schermo", Effect::Launch(LaunchReq::Setting { index: 1 })),
            ("www.example.com", Effect::Launch(LaunchReq::Url("https://www.example.com".into()))),
            (":", Effect::Launch(LaunchReq::Registry)),
            ("zzqq", Effect::Launch(LaunchReq::Url("https://www.google.com/search?q=zzqq".into()))),
        ];
        for (q, then) in cases {
            let mut s = typed(&fx, q);
            assert_eq!(press(&mut s, &fx, VK_RETURN, Mods::NONE), [hide.clone(), then], "{q}");
        }
        // Run lines: a known console program in a console, Shift+Enter any line in a console.
        let mut s = typed(&fx, ">ping 1.1.1.1");
        let Some(Effect::Launch(LaunchReq::Console(r))) = press(&mut s, &fx, VK_RETURN, Mods::NONE).pop() else {
            panic!()
        };
        assert_eq!(&*r.line, "ping 1.1.1.1");
        let mut s = typed(&fx, ">notepad x.txt");
        let fx_enter = press(&mut s, &fx, VK_RETURN, Mods::NONE);
        assert!(matches!(&fx_enter[..], [Effect::Hide { restore: false }, Effect::Launch(LaunchReq::Command(_))]));
        let fx_shift = press(&mut s, &fx, VK_RETURN, Mods::SHIFT);
        assert!(matches!(&fx_shift[..], [Effect::Hide { restore: false }, Effect::Launch(LaunchReq::Console(_))]));
    }

    #[test]
    fn ctrl_w_never_on_the_default_row() {
        let fx = Fx::new();
        let mut s = typed(&fx, "<");
        assert_eq!(s.rows().len(), 2);
        assert!(press(&mut s, &fx, VK_W, Mods::CTRL).is_empty(), "the default row");
        press(&mut s, &fx, VK_DOWN, Mods::NONE);
        assert_eq!(press(&mut s, &fx, VK_W, Mods::CTRL), [Effect::CloseWindow(0x2002), Effect::Relayout]);
        assert_eq!(s.rows().len(), 1, "the row goes, the launcher stays");
        assert!(press(&mut s, &fx, VK_W, Mods::CTRL).is_empty(), "the row that took its place is not chosen yet");
        // An app row chosen by arrow closes its most recent window.
        let mut s = typed(&fx, "blocco");
        assert!(press(&mut s, &fx, VK_W, Mods::CTRL).is_empty());
        assert!(press(&mut s, &fx, VK_UP, Mods::NONE).is_empty(), "an arrow on the row it is on chooses it");
        assert_eq!(press(&mut s, &fx, VK_W, Mods::CTRL), [Effect::CloseWindow(0x1001), Effect::Relayout]);
        // Not on an app that is not running, and the choice is forgotten at the next keystroke.
        let mut s = typed(&fx, "calcolatric");
        press(&mut s, &fx, VK_DOWN, Mods::NONE);
        assert!(press(&mut s, &fx, VK_W, Mods::CTRL).is_empty());
        let mut s = typed(&fx, "<");
        press(&mut s, &fx, VK_DOWN, Mods::NONE);
        s.chars("f", &fx.cx());
        assert!(press(&mut s, &fx, VK_W, Mods::CTRL).is_empty());
    }

    #[test]
    fn ctrl_w_never_hands_the_selection_to_a_destructive_row() {
        use crate::launcher::catalog::CatApp;
        use crate::launcher::model::WindowRow;
        use crate::start_search::Keys;
        let mut fx = Fx::new();
        fx.apps.push(CatApp::new("ShareX", "ShareX", Keys::default(), 50));
        fx.windows.push(WindowRow {
            hwnd: 0x3003,
            group_key: "c:\\tools\\sharex.exe".into(),
            aumid: None,
            name: "ShareX".into(),
            name_folded: "sharex".into(),
            title: Some("ShareX".into()),
            hung: false,
        });
        let mut s = typed(&fx, "sh");
        let titles: Vec<&str> = s.rows().iter().map(|r| &*r.title).collect();
        assert_eq!(titles[..2], ["ShareX", "Shut down"], "the running app sits right above Shut down");
        assert!(s.rows()[1].destructive);
        assert_eq!(sel_title(&s), Some("ShareX"));
        press(&mut s, &fx, VK_UP, Mods::NONE);
        assert_eq!(press(&mut s, &fx, VK_W, Mods::CTRL), [Effect::CloseWindow(0x3003), Effect::Relayout]);
        assert_eq!(s.rows()[0].title.as_ref(), "Shut down", "Shut down took its place");
        assert!(!s.selected().is_some_and(|r| r.destructive), "never selected by default below 4 characters");
        let out = press(&mut s, &fx, VK_RETURN, Mods::NONE);
        assert!(!out.iter().any(|e| matches!(e, Effect::Power(_))), "{out:?}");
        // Chosen by arrow, it runs (5.2.7).
        press(&mut s, &fx, VK_HOME, Mods::CTRL);
        assert_eq!(
            press(&mut s, &fx, VK_RETURN, Mods::NONE),
            [Effect::Hide { restore: false }, Effect::Power(CommandId::ShutDown)]
        );
    }

    #[test]
    fn open_path_is_passed_untouched() {
        let fx = Fx::new();
        let path = "C:\\Users\\%USERNAME%\\a b.txt";
        let mut s = typed(&fx, path);
        assert_eq!(
            press(&mut s, &fx, VK_RETURN, Mods::NONE),
            [Effect::Hide { restore: false }, Effect::Launch(LaunchReq::OpenPath(path.into()))]
        );
        assert_eq!(
            press(&mut s, &fx, VK_E, CTRL_SHIFT),
            [Effect::Hide { restore: false }, Effect::Launch(LaunchReq::RevealPath(path.into()))]
        );
        // An executable path is shown in its folder, never opened, until live check 6.
        let mut s = typed(&fx, "c:\\tools\\setup.exe");
        assert!(press(&mut s, &fx, VK_RETURN, Mods::NONE).is_empty());
        press(&mut s, &fx, VK_DOWN, Mods::NONE);
        assert!(matches!(s.selected().unwrap().payload, Payload::Path { class: FileClass::Executable, .. }));
        assert!(press(&mut s, &fx, VK_RETURN, Mods::NONE).is_empty());
        assert_eq!(
            press(&mut s, &fx, VK_RETURN, Mods::CTRL),
            [Effect::Hide { restore: false }, Effect::Launch(LaunchReq::RevealPath("c:\\tools\\setup.exe".into()))]
        );
    }

    #[test]
    fn keep_query_selected_on_open() {
        let mut fx = Fx::new();
        fx.opts.keep_query = true;
        let mut s = Session::open(&fx.opts, Some("calc"));
        assert_eq!(s.query_text(), "calc");
        assert_eq!(s.field().selection(), Some(0..4));
        assert_eq!(s.refresh(&fx.cx()), [Effect::Relayout]);
        assert_eq!(sel_title(&s), Some("Calcolatrice"));
        s.chars("x", &fx.cx());
        assert_eq!(s.query_text(), "x", "typing replaces it");
        fx.opts.keep_query = false;
        let s = Session::open(&fx.opts, Some("calc"));
        assert_eq!((s.query_text(), s.field().selection()), ("", None));
        // What comes back next time is the query as typed, prefix included.
        let s = typed(&fx, "=3+4");
        assert_eq!((s.query_text(), s.last_query().as_str()), ("3+4", "=3+4"));
    }

    #[test]
    fn replayed_enter_never_runs_a_destructive_row() {
        let fx = Fx::new();
        let mut s = open(&fx);
        let mut keys: Vec<ReplayKey> = "shut".chars().map(ReplayKey::Text).collect();
        keys.push(ReplayKey::Key(k(VK_RETURN, Mods::NONE)));
        let out = s.replay(&keys, &fx.cx());
        assert!(!out.iter().any(|e| matches!(e, Effect::Power(_) | Effect::Hide { .. })), "{out:?}");
        assert_eq!(s.selected().unwrap().payload, Payload::Command(CommandId::ShutDown), "selected, waiting");
        assert_eq!(
            press(&mut s, &fx, VK_RETURN, Mods::NONE),
            [Effect::Hide { restore: false }, Effect::Power(CommandId::ShutDown)]
        );
        // Every other key behaves as typed, and a replayed Enter acts on the replayed text's rows.
        let mut s = open(&fx);
        let mut keys: Vec<ReplayKey> = "calc".chars().map(ReplayKey::Text).collect();
        keys.push(ReplayKey::Key(k(VK_RETURN, Mods::NONE)));
        keys.push(ReplayKey::Text('z'));
        let out = s.replay(&keys, &fx.cx());
        assert_eq!(out, [Effect::Hide { restore: false }, Effect::Launch(LaunchReq::App(CALC_ID.into()))]);
        assert_eq!(s.query_text(), "calc", "keys after the close are not replayed");
        // The repaints of a replay come once, at its end.
        let mut s = open(&fx);
        let out = s.replay(&[ReplayKey::Text('s'), ReplayKey::Text('t')], &fx.cx());
        assert_eq!(out, [Effect::Relayout]);
    }

    #[test]
    fn no_rows_requests_a_rewrite_once() {
        let fx = Fx::new();
        let mut s = open(&fx);
        let mut asked = Vec::new();
        for c in ["x", "q", "z", "j"] {
            let out = s.chars(c, &fx.cx());
            let gens: Vec<u64> = out
                .iter()
                .filter_map(|e| match e {
                    Effect::RequestRewrite { gen } => Some(*gen),
                    _ => None,
                })
                .collect();
            assert!(gens.len() <= 1);
            asked.extend(gens);
        }
        assert_eq!(asked, [1, 2, 3, 4], "one per generation");
        assert!(press(&mut s, &fx, VK_LEFT, Mods::NONE).iter().all(|e| !matches!(e, Effect::RequestRewrite { .. })));
        // Rows, an empty query or another mode: no rewrite.
        let mut s = open(&fx);
        assert!(s.chars("c", &fx.cx()).iter().all(|e| !matches!(e, Effect::RequestRewrite { .. })));
        let mut s = typed(&fx, ">");
        assert!(s.chars("q", &fx.cx()).iter().all(|e| !matches!(e, Effect::RequestRewrite { .. })));
    }

    #[test]
    fn backspace_at_caret_0_turns_the_chip_into_text() {
        let fx = Fx::new();
        let mut s = typed(&fx, "=3");
        press(&mut s, &fx, VK_HOME, Mods::NONE);
        assert_eq!(press(&mut s, &fx, VK_BACK, Mods::NONE), [Effect::Relayout]);
        assert_eq!((s.chip(), s.query_text(), s.field().caret()), (None, "=3", 1));
        press(&mut s, &fx, VK_BACK, Mods::NONE);
        assert_eq!(s.query_text(), "3");
        // A prefix becomes a chip only when typed at the start, and a word prefix needs its space.
        let s = typed(&fx, "file");
        assert_eq!(s.chip(), None);
        let s = typed(&fx, "file ");
        assert_eq!((s.chip(), s.query_text()), (Some(ProviderId::Files), ""));
    }

    #[test]
    fn enter_after_a_late_batch_acts_on_the_selected_row() {
        let fx = Fx::new();
        let mut s = typed(&fx, "st");
        // Steam, Sleep ("standby" is a name of it, L3 T5), Visual Studio Code.
        press(&mut s, &fx, VK_DOWN, Mods::NONE);
        press(&mut s, &fx, VK_DOWN, Mods::NONE);
        assert_eq!(sel_title(&s), Some("Visual Studio Code"));
        let row = crate::launcher::providers::tests::file_row("studio.txt", FileClass::Other);
        assert!(s.late(Batch { gen: s.gen(), from: ProviderId::Files, items: vec![row.clone()], done: true }, &fx.cx()));
        assert!(!s.late(Batch { gen: s.gen() - 1, from: ProviderId::Files, items: vec![row], done: true }, &fx.cx()));
        assert_eq!(
            press(&mut s, &fx, VK_RETURN, Mods::NONE),
            [Effect::Hide { restore: false }, Effect::Launch(LaunchReq::App("Microsoft.VisualStudioCode".into()))]
        );
    }

    #[test]
    fn run_mode_tab_shift_delete_and_suggestions() {
        let fx = Fx::new();
        let mut s = typed(&fx, ">pi");
        assert_eq!(s.selected().unwrap().kind, Kind::RunLine, "the typed line first");
        assert_eq!(press(&mut s, &fx, VK_TAB, Mods::NONE), [Effect::Relayout]);
        assert_eq!((s.chip(), s.query_text()), (Some(ProviderId::Run), "ping 1.1.1.1"));
        // Ctrl+Shift+Enter on a suggestion only puts it in the field; the typed line needs the view's measure first.
        let mut s = typed(&fx, ">no");
        press(&mut s, &fx, VK_DOWN, Mods::NONE);
        assert_eq!(s.selected().unwrap().kind, Kind::RunSuggestion);
        let out = s.key(k(VK_RETURN, CTRL_SHIFT), Some(Gesture::for_test(7)), &fx.cx());
        assert_eq!(out, [Effect::Relayout]);
        assert_eq!(s.query_text(), "notepad c:\\temp\\a.txt");
        assert_eq!(s.selected().unwrap().kind, Kind::RunLine);
        assert!(s.key(k(VK_RETURN, CTRL_SHIFT), Some(Gesture::for_test(8)), &fx.cx()).is_empty());
        // Shift+Delete forgets a history line, never a known command.
        let mut s = typed(&fx, ">p");
        let ping = s.rows().iter().position(|r| &*r.title == "ping 1.1.1.1").unwrap();
        for _ in 0..ping {
            press(&mut s, &fx, VK_DOWN, Mods::NONE);
        }
        assert_eq!(
            press(&mut s, &fx, VK_DELETE, Mods::SHIFT),
            [Effect::ForgetRun("ping 1.1.1.1".into()), Effect::Relayout]
        );
        assert!(s.rows().iter().all(|r| &*r.title != "ping 1.1.1.1"));
        let known = s.rows().iter().position(|r| r.kind == Kind::RunSuggestion).unwrap();
        s.results.select_index(known);
        assert!(press(&mut s, &fx, VK_DELETE, Mods::SHIFT).is_empty());
        // Tab in Mixed mode completes a name that starts with the query.
        let mut s = typed(&fx, "calco");
        press(&mut s, &fx, VK_TAB, Mods::NONE);
        assert_eq!(s.query_text(), "Calcolatrice");
        let _ = NOTEPAD_KEY;
    }

    #[test]
    fn runas_conditions() {
        let fx = Fx::new();
        let cse = |s: &mut Session, g: Option<Gesture>| s.key(k(VK_RETURN, CTRL_SHIFT), g, &fx.cx());
        let hide = Effect::Hide { restore: false };
        // A suggestion only goes into the field, where it becomes the typed line.
        let mut s = typed(&fx, ">no");
        press(&mut s, &fx, VK_DOWN, Mods::NONE);
        assert_eq!(s.selected().unwrap().kind, Kind::RunSuggestion);
        assert_eq!((s.review_text(), s.admin_refusal()), (None, None));
        assert_eq!(cse(&mut s, Some(Gesture::for_test(1))), [Effect::Relayout]);
        let line = s.selected().unwrap().clone();
        let Payload::RunLine(run) = &line.payload else { panic!("{line:?}") };
        assert_eq!(line.kind, Kind::RunLine);
        assert_eq!(s.review_text(), Some("notepad c:\\temp\\a.txt"));
        // The typed line: the view's measure and the gesture, both.
        assert_eq!(s.admin_refusal(), Some(Refusal::TooLong), "not measured yet");
        assert!(cse(&mut s, Some(Gesture::for_test(2))).is_empty());
        s.set_review_lines(line.key, 1);
        assert_eq!(s.admin_refusal(), None);
        assert!(cse(&mut s, None).is_empty(), "no gesture");
        let runas = |g: u32| Effect::Launch(LaunchReq::RunAsTyped { line: run.clone(), gesture: Gesture::for_test(g) });
        assert_eq!(cse(&mut s, Some(Gesture::for_test(3))), [hide.clone(), runas(3)]);
        // A line that needs 4 lines is disabled, with its detail line; 3 lines pass.
        s.set_review_lines(line.key, 4);
        assert_eq!(s.admin_refusal(), Some(Refusal::TooLong));
        assert_eq!(s.admin_refusal().and_then(Refusal::detail), Some("Line too long to review"));
        assert!(cse(&mut s, Some(Gesture::for_test(4))).is_empty());
        s.set_review_lines(line.key, 3);
        assert_eq!(cse(&mut s, Some(Gesture::for_test(5))), [hide.clone(), runas(5)]);
        // A measure holds for its row and its generation only.
        s.set_review_lines(ItemKey(1), 1);
        assert_eq!(s.admin_refusal(), Some(Refusal::TooLong), "another row's measure");
        s.set_review_lines(line.key, 1);
        s.chars(" ", &fx.cx());
        assert_eq!(s.selected().unwrap().key, line.key, "the same line");
        assert_eq!(s.admin_refusal(), Some(Refusal::TooLong), "a new generation needs a new measure");
        assert!(cse(&mut s, Some(Gesture::for_test(6))).is_empty());
        // A replayed Ctrl+Shift+Enter has no gesture.
        s.set_review_lines(line.key, 1);
        assert!(s.replay(&[ReplayKey::Key(k(VK_RETURN, CTRL_SHIFT))], &fx.cx()).is_empty());
        // A bidi override disables it, measured or not.
        let mut s = typed(&fx, ">notepad a\u{202E}txt.exe");
        let key = s.selected().unwrap().key;
        s.set_review_lines(key, 1);
        assert_eq!(s.admin_refusal(), Some(Refusal::Markers));
        assert!(cse(&mut s, Some(Gesture::for_test(7))).is_empty());
        // Executable rows offer only Reveal and Copy while MOTW_PROMPT_CONFIRMED is false: nothing to review or run.
        const { assert!(!providers::MOTW_PROMPT_CONFIRMED) };
        let mut s = typed(&fx, "c:\\tools\\setup.exe");
        let i = s.rows().iter().position(|r| r.kind == Kind::File).unwrap();
        s.results.select_index(i);
        let row = s.selected().unwrap().clone();
        assert_eq!(row.verbs, VerbSet::empty().with(Verb::Reveal).with(Verb::Copy));
        assert_eq!(s.review_text(), None);
        s.set_review_lines(row.key, 1);
        assert!(cse(&mut s, Some(Gesture::for_test(8))).is_empty());
        assert!(press(&mut s, &fx, VK_RETURN, Mods::NONE).is_empty());
        // Once live check 6 passes: a local exe runs reviewed and quoted; a UNC path and a script are refused.
        let exe = |path: &str| {
            let mut it = crate::launcher::providers::tests::file_row("setup.exe", FileClass::Executable);
            it.key = ItemKey::of(ProviderId::Files, path);
            it.verbs = verbs::verbs_for_file(path, FileClass::Executable, true, true);
            it.payload = Payload::Path { path: path.into(), dir: false, class: FileClass::Executable };
            it
        };
        let paths =
            ["c:\\a b\\setup.exe", "\\\\server\\share\\setup.exe", "//server/share/setup.exe", "c:\\x\\run.ps1"];
        let mut s = typed(&fx, "setup");
        let batch =
            Batch { gen: s.gen(), from: ProviderId::Files, items: paths.iter().map(|p| exe(p)).collect(), done: true };
        assert!(s.late(batch, &fx.cx()));
        let pick = |s: &mut Session, p: &str| {
            let i = s.rows().iter().position(|r| r.key == ItemKey::of(ProviderId::Files, p)).unwrap();
            s.results.select_index(i);
            s.set_review_lines(ItemKey::of(ProviderId::Files, p), 1);
        };
        pick(&mut s, paths[0]);
        assert_eq!(s.review_text(), Some(paths[0]), "the full path is reviewed");
        let quoted = RunLine { line: "\"c:\\a b\\setup.exe\"".into(), file: 1..17, console: false };
        assert_eq!(
            cse(&mut s, Some(Gesture::for_test(9))),
            [hide.clone(), Effect::Launch(LaunchReq::RunAsTyped { line: quoted, gesture: Gesture::for_test(9) })]
        );
        assert!(cse(&mut s, None).is_empty(), "no gesture");
        for p in &paths[1..3] {
            pick(&mut s, p);
            assert_eq!(s.admin_refusal(), Some(Refusal::Unc), "{p}");
            assert!(cse(&mut s, Some(Gesture::for_test(10))).is_empty(), "{p}");
        }
        // A script is not offered at all: nothing to review, nothing refused, nothing run.
        pick(&mut s, paths[3]);
        assert!(!s.selected().unwrap().verbs.has(Verb::RunAsAdmin), "only exe, msi, bat and cmd");
        assert_eq!((s.review_text(), s.admin_refusal()), (None, None));
        assert!(cse(&mut s, Some(Gesture::for_test(11))).is_empty());
        // The review uses the action's filter, even on a row that claims the verb.
        let mut forced = exe(paths[3]);
        forced.verbs = verbs::verbs_for(Kind::File, false, Some(FileClass::Executable), true, true);
        assert_eq!(review_text(&forced), None);
    }

    #[test]
    fn selection_keys_never_wrap() {
        let fx = Fx::new();
        let mut s = typed(&fx, "st");
        let n = s.rows().len();
        assert_eq!(press(&mut s, &fx, VK_UP, Mods::NONE), Vec::<Effect>::new(), "already on the first row");
        assert_eq!(press(&mut s, &fx, VK_NEXT, Mods::NONE), [Effect::Redraw]);
        assert_eq!(s.selected_index(), Some(n - 1));
        assert!(press(&mut s, &fx, VK_DOWN, Mods::NONE).is_empty());
        press(&mut s, &fx, VK_HOME, Mods::CTRL);
        assert_eq!(s.selected_index(), Some(0));
        press(&mut s, &fx, VK_END, Mods::CTRL);
        assert_eq!(s.selected_index(), Some(n - 1));
        press(&mut s, &fx, VK_PRIOR, Mods::NONE);
        assert_eq!(s.selected_index(), Some(0));
        // Ctrl+V and Shift+Insert ask for the clipboard; the text comes back through paste.
        assert_eq!(press(&mut s, &fx, VK_V, Mods::CTRL), [Effect::Paste]);
        assert_eq!(press(&mut s, &fx, VK_INSERT, Mods::SHIFT), [Effect::Paste]);
        s.paste("a\r\nb", &fx.cx());
        assert_eq!(s.query_text(), "sta  b");
        // Characters from WM_CHAR never carry control codes into the field.
        s.chars("\u{1}\r", &fx.cx());
        assert_eq!(s.query_text(), "sta  b");
    }

    #[test]
    fn destructive_click_needs_300_ms() {
        let fx = Fx::new();
        let mut s = typed(&fx, "riav");
        let i = s.rows().iter().position(|r| r.payload == Payload::Command(CommandId::Restart)).unwrap();
        s.pointer(10, 10, Some(i), 1_000);
        assert!(!s.click(i, 1_299, &fx.cx()).iter().any(|e| matches!(e, Effect::Power(_))), "too early: selects");
        assert_eq!(s.selected_index(), Some(i));
        assert_eq!(s.click(i, 1_300, &fx.cx()), [Effect::Hide { restore: false }, Effect::Power(CommandId::Restart)]);
        // A click with no hover before it starts the 300 ms.
        let mut s = typed(&fx, "riav");
        assert!(!s.click(i, 5_000, &fx.cx()).iter().any(|e| matches!(e, Effect::Power(_))));
        assert_eq!(s.click(i, 5_300, &fx.cx()), [Effect::Hide { restore: false }, Effect::Power(CommandId::Restart)]);
        // Moving to another row and back starts again.
        let mut s = typed(&fx, "riav");
        s.pointer(10, 10, Some(i), 0);
        s.pointer(10, 60, None, 200);
        s.pointer(10, 10, Some(i), 400);
        assert!(!s.click(i, 600, &fx.cx()).iter().any(|e| matches!(e, Effect::Power(_))));
        // Any other row runs at the first click.
        let mut s = typed(&fx, "calcolatrice");
        assert_eq!(
            s.click(0, 0, &fx.cx()),
            [Effect::Hide { restore: false }, Effect::Launch(LaunchReq::App(CALC_ID.into()))]
        );
        assert!(s.click(99, 0, &fx.cx()).is_empty());
    }

    #[test]
    fn pointer_needs_8px() {
        let fx = Fx::new();
        let mut s = typed(&fx, "st");
        assert_eq!(s.selected_index(), Some(0));
        assert!(s.pointer(100, 100, Some(2), 0).is_empty());
        assert!(s.pointer(103, 104, Some(2), 10).is_empty(), "5 px");
        assert!(s.pointer(105, 106, Some(2), 20).is_empty(), "7.8 px");
        assert_eq!(s.selected_index(), Some(0));
        assert_eq!(s.pointer(106, 106, Some(2), 30), [Effect::Redraw], "8.5 px");
        assert_eq!(s.selected_index(), Some(2));
        assert_eq!(s.pointer(107, 120, Some(3), 40), [Effect::Redraw]);
        assert!(s.pointer(107, 121, None, 50).is_empty(), "off the rows the selection stays");
        assert_eq!(s.selected_index(), Some(3));
        // A key: the pointer has to travel again.
        press(&mut s, &fx, VK_DOWN, Mods::NONE);
        assert_eq!(s.selected_index(), Some(4));
        assert!(s.pointer(108, 106, Some(1), 60).is_empty());
        assert!(s.pointer(110, 110, Some(1), 70).is_empty());
        assert_eq!(s.selected_index(), Some(4));
        // A row chosen by the pointer counts as chosen for Ctrl+W.
        let mut s = typed(&fx, "<");
        s.pointer(0, 0, Some(1), 0);
        s.pointer(0, 20, Some(1), 10);
        assert_eq!(press(&mut s, &fx, VK_W, Mods::CTRL), [Effect::CloseWindow(0x2002), Effect::Relayout]);
        // Far-away coordinates do not overflow.
        let mut s = typed(&fx, "st");
        s.pointer(i32::MIN, i32::MIN, Some(1), 0);
        assert_eq!(s.pointer(i32::MAX, i32::MAX, Some(1), 1), [Effect::Redraw]);
    }

    #[test]
    fn click_on_a_row_that_just_moved_only_selects() {
        let fx = Fx::new();
        let mut s = typed(&fx, "ri");
        assert_eq!(sel_title(&s), Some("Window menu"), "\"ridimensiona\" is one of its names (L3 T5)");
        s.pointer(10, 10, Some(0), 0);
        // A keystroke later the row under the resting pointer is Restart, of a new generation.
        s.chars("a", &fx.cx());
        assert_eq!(s.rows()[0].payload, Payload::Command(CommandId::Restart));
        let out = s.click(0, 1_000, &fx.cx());
        assert!(!out.iter().any(|e| matches!(e, Effect::Power(_) | Effect::Hide { .. })), "{out:?}");
        assert_eq!(s.selected_index(), Some(0));
        assert_eq!(s.click(0, 1_300, &fx.cx()), [Effect::Hide { restore: false }, Effect::Power(CommandId::Restart)]);
    }

    // ---------------------------------------------------------------- L1b-1 keys and L1b-2 action list

    #[test]
    fn shift_motions_select_and_ctrl_x_cuts_to_the_clipboard() {
        let fx = Fx::new();
        let mut s = typed(&fx, "open file");
        // Ctrl+Left twice: before "file", before "open"; Shift+Ctrl+Right selects the word after the caret.
        assert_eq!(press(&mut s, &fx, VK_LEFT, Mods::CTRL), [Effect::Redraw]);
        assert_eq!(s.field().caret(), 5);
        press(&mut s, &fx, VK_LEFT, Mods::CTRL);
        assert_eq!(s.field().caret(), 0);
        assert_eq!(press(&mut s, &fx, VK_RIGHT, CTRL_SHIFT), [Effect::Redraw]);
        assert_eq!(s.field().selection(), Some(0..5));
        // Ctrl+X: the selection goes to the clipboard now, the text changes, the launcher stays.
        let out = press(&mut s, &fx, 0x58, Mods::CTRL);
        assert_eq!(out.first(), Some(&Effect::CopyNow("open ".into())));
        assert!(out.contains(&Effect::Relayout), "{out:?}");
        assert_eq!((s.query_text(), s.field().selection()), ("file", None));
        // Nothing selected: Ctrl+X does nothing.
        assert!(press(&mut s, &fx, 0x58, Mods::CTRL).is_empty());
        // Shift+Home selects to the start and Ctrl+C copies the selection.
        press(&mut s, &fx, VK_END, Mods::NONE);
        press(&mut s, &fx, VK_HOME, Mods::SHIFT);
        assert_eq!(press(&mut s, &fx, VK_C, Mods::CTRL), [Effect::CopyNow("file".into())]);
        // Ctrl+Delete removes up to the next word.
        let mut s = typed(&fx, "one two");
        press(&mut s, &fx, VK_HOME, Mods::NONE);
        assert!(!press(&mut s, &fx, VK_DELETE, Mods::CTRL).is_empty());
        assert_eq!(s.query_text(), "two");
    }

    fn open_list(s: &mut Session, fx: &Fx) -> Vec<Effect> {
        press(s, fx, 0x4B, Mods::CTRL)
    }

    fn acts(s: &Session) -> Vec<Act> {
        s.action_list().map(|l| l.items().iter().map(|e| e.act).collect()).unwrap_or_default()
    }

    #[test]
    fn ctrl_comma_opens_settings_after_the_hide() {
        let fx = Fx::new();
        let mut s = typed(&fx, "blocco");
        assert_eq!(
            press(&mut s, &fx, VK_OEM_COMMA, Mods::CTRL),
            [Effect::Hide { restore: false }, Effect::OpenSettings]
        );
        // A bare comma is text; a held Ctrl+, does not repeat; the action list takes the key first.
        let mut s = typed(&fx, "a");
        assert!(!press(&mut s, &fx, VK_OEM_COMMA, Mods::NONE).contains(&Effect::OpenSettings));
        let held = KeyEvent { repeat: true, ..k(VK_OEM_COMMA, Mods::CTRL) };
        assert!(s.key(held, None, &fx.cx()).is_empty());
    }

    #[test]
    fn action_list_keys() {
        use Act::Verb as V;
        let fx = Fx::new();
        let mut s = typed(&fx, "blocco");
        assert_eq!(sel_title(&s), Some("Blocco note"));
        // Ctrl+K opens it on the row's own Enter verb, with the focus ring (keyboard).
        assert_eq!(open_list(&mut s, &fx), [Effect::Relayout]);
        assert_eq!(
            acts(&s),
            [
                V(Verb::Open),
                V(Verb::Switch),
                V(Verb::RunAsAdmin),
                V(Verb::Reveal),
                V(Verb::Copy),
                V(Verb::CloseWindow),
                V(Verb::Forget)
            ]
        );
        let l = s.action_list().unwrap();
        assert_eq!((l.sel(), l.ring()), (1, true));
        // Ctrl+K again closes it; Shift+F10 and the Menu key open it.
        assert_eq!(open_list(&mut s, &fx), [Effect::Relayout]);
        assert!(s.action_list().is_none());
        assert_eq!(press(&mut s, &fx, 0x79, Mods::SHIFT), [Effect::Relayout]);
        assert_eq!(press(&mut s, &fx, 0x5D, Mods::NONE), [Effect::Relayout]);
        assert!(s.action_list().is_none());
        press(&mut s, &fx, 0x5D, Mods::NONE);
        // Up, Down, Home, End move inside it and stop at the ends.
        assert_eq!(press(&mut s, &fx, VK_DOWN, Mods::NONE), [Effect::Redraw]);
        assert_eq!(s.action_list().map(ActionList::sel), Some(2));
        assert_eq!(press(&mut s, &fx, VK_END, Mods::NONE), [Effect::Redraw]);
        assert!(press(&mut s, &fx, VK_DOWN, Mods::NONE).is_empty());
        assert_eq!(s.action_list().map(ActionList::sel), Some(6));
        press(&mut s, &fx, VK_HOME, Mods::NONE);
        assert!(press(&mut s, &fx, VK_UP, Mods::NONE).is_empty());
        assert_eq!(s.action_list().map(ActionList::sel), Some(0));
        // The row behind it did not move, and keys the list does not know wait.
        assert_eq!(sel_title(&s), Some("Blocco note"));
        assert!(press(&mut s, &fx, VK_LEFT, Mods::ALT).is_empty());
        assert!(press(&mut s, &fx, VK_C, Mods::CTRL).is_empty());
        assert!(s.action_list().is_some());
        // A held Enter does nothing.
        let held = KeyEvent { repeat: true, ..k(VK_RETURN, Mods::NONE) };
        assert!(s.key(held, None, &fx.cx()).is_empty());
        // Enter runs the entry and closes the list: Open on a running app is a new instance.
        assert_eq!(
            press(&mut s, &fx, VK_RETURN, Mods::NONE),
            [Effect::Hide { restore: false }, Effect::Launch(LaunchReq::App(NOTEPAD_ID.into()))]
        );
        assert!(s.action_list().is_none());
        // Every other verb runs as its chord does.
        let run = |s: &mut Session, k: usize| -> Vec<Effect> {
            open_list(s, &fx);
            press(s, &fx, VK_HOME, Mods::NONE);
            for _ in 0..k {
                press(s, &fx, VK_DOWN, Mods::NONE);
            }
            press(s, &fx, VK_RETURN, Mods::NONE)
        };
        assert_eq!(run(&mut s, 1), [Effect::Switch(0x1001), Effect::Hide { restore: false }]);
        assert_eq!(
            run(&mut s, 2),
            [Effect::Hide { restore: false }, Effect::Launch(LaunchReq::AppAdmin(NOTEPAD_ID.into()))]
        );
        assert_eq!(
            run(&mut s, 3),
            [Effect::Hide { restore: false }, Effect::Launch(LaunchReq::RevealApp(NOTEPAD_ID.into()))]
        );
        assert_eq!(run(&mut s, 4), [Effect::Hide { restore: true }, Effect::Copy("Blocco note".into())]);
        assert_eq!(run(&mut s, 6), [Effect::Forget(s.selected().unwrap().key), Effect::Relayout]);
        // Close window needs no arrow pick here (the list is a choice) and the launcher stays.
        assert_eq!(run(&mut s, 5), [Effect::CloseWindow(0x1001), Effect::Relayout]);
        assert!(s.action_list().is_none());
    }

    #[test]
    fn action_list_esc_typing_and_backspace() {
        let fx = Fx::new();
        let mut s = typed(&fx, "blocco");
        open_list(&mut s, &fx);
        // Esc closes only the list.
        assert_eq!(press(&mut s, &fx, VK_ESCAPE, Mods::NONE), [Effect::Relayout]);
        assert_eq!(s.query_text(), "blocco");
        // A letter closes it and goes on searching.
        open_list(&mut s, &fx);
        let out = s.chars("x", &fx.cx());
        assert!(s.action_list().is_none() && out.contains(&Effect::Relayout), "{out:?}");
        assert_eq!(s.query_text(), "bloccox");
        // Backspace closes it and edits.
        let mut s = typed(&fx, "blocco");
        open_list(&mut s, &fx);
        let out = press(&mut s, &fx, VK_BACK, Mods::NONE);
        assert!(s.action_list().is_none() && out.contains(&Effect::Relayout), "{out:?}");
        assert_eq!(s.query_text(), "blocc");
        // Editing chords close it too and act on the field: a paste, a word delete, select all, a caret move.
        for (vk, m) in [(VK_V, Mods::CTRL), (VK_INSERT, Mods::SHIFT)] {
            open_list(&mut s, &fx);
            let out = press(&mut s, &fx, vk, m);
            assert!(
                s.action_list().is_none() && out.contains(&Effect::Paste) && out.contains(&Effect::Relayout),
                "{out:?}"
            );
        }
        s.paste("z", &fx.cx());
        assert_eq!(s.query_text(), "bloccz");
        open_list(&mut s, &fx);
        press(&mut s, &fx, VK_BACK, Mods::CTRL);
        assert!(s.action_list().is_none());
        assert_eq!(s.query_text(), "");
        let mut s = typed(&fx, "blocco");
        for (vk, m) in [(VK_A, Mods::CTRL), (VK_LEFT, Mods::NONE), (VK_RIGHT, Mods::CTRL), (VK_X, Mods::CTRL)] {
            open_list(&mut s, &fx);
            let out = press(&mut s, &fx, vk, m);
            assert!(s.action_list().is_none() && out.contains(&Effect::Relayout), "{vk:#x}: {out:?}");
        }
        // The chord and Alt+F4 still close the launcher itself.
        open_list(&mut s, &fx);
        assert_eq!(press(&mut s, &fx, VK_SPACE, Mods::ALT), [Effect::Hide { restore: true }]);
        assert_eq!(press(&mut s, &fx, VK_F4, Mods::ALT), [Effect::Hide { restore: true }]);
        // Esc, once the list is closed, goes on down the ladder.
        let mut s = typed(&fx, "blocco");
        open_list(&mut s, &fx);
        press(&mut s, &fx, VK_ESCAPE, Mods::NONE);
        assert_eq!(press(&mut s, &fx, VK_ESCAPE, Mods::NONE), [Effect::Relayout]);
        assert_eq!(s.query_text(), "");
    }

    #[test]
    fn right_click_selects_and_opens_the_list_without_the_ring() {
        let fx = Fx::new();
        let mut s = typed(&fx, "note");
        assert!(s.rows().len() > 1);
        assert_eq!(s.context_menu(1), [Effect::Relayout]);
        let l = s.action_list().unwrap();
        assert!(!l.ring());
        assert_eq!(s.selected_index(), Some(1));
        // A pointer over entry 2 takes the highlight; a second right click closes only the list.
        let other = (s.action_list().unwrap().sel() + 1) % s.action_list().unwrap().items().len();
        assert_eq!(s.action_hover(other), [Effect::Redraw]);
        assert!(s.action_hover(other).is_empty());
        assert_eq!(s.action_list().map(ActionList::sel), Some(other));
        assert_eq!(s.context_menu(0), [Effect::Relayout]);
        assert!(s.action_list().is_none());
        assert_eq!(s.selected_index(), Some(1));
    }

    #[test]
    fn the_list_never_opens_without_something_to_offer_or_in_a_replay() {
        let fx = Fx::new();
        // No rows.
        let mut s = open(&fx);
        assert!(open_list(&mut s, &fx).is_empty());
        // Typed blind before the launcher showed: a replayed Ctrl+K opens nothing.
        let mut s = typed(&fx, "blocco");
        let out = s.replay(&[ReplayKey::Key(k(0x4B, Mods::CTRL))], &fx.cx());
        assert!(out.is_empty() && s.action_list().is_none(), "{out:?}");
        // A notice with no verb (the refusal row) has no list.
        let mut s = open(&fx);
        s.set_notices(vec![notice(VerbSet::empty())]);
        s.chars("z", &fx.cx());
        press(&mut s, &fx, VK_DOWN, Mods::NONE);
        assert_eq!(sel_title(&s), Some("A notice"));
        assert!(open_list(&mut s, &fx).is_empty());
    }

    fn notice(verbs: VerbSet) -> Item {
        use crate::launcher::model::Score;
        Item {
            key: ItemKey::of(ProviderId::Apps, "a-notice"),
            from: ProviderId::Apps,
            kind: Kind::Notice,
            title: "A notice".into(),
            detail: None,
            icon: IconRef::Glyph(crate::launcher::model::GlyphId::Info),
            score: Score { tier: 0, learned: std::cmp::Reverse(0), idle: true, kind: 0, rank: 0, len: 0 },
            verbs,
            payload: Payload::None,
            destructive: false,
        }
    }

    #[test]
    fn a_notice_with_an_action_offers_dont_show_again() {
        let fx = Fx::new();
        let mut s = open(&fx);
        let n = notice(VerbSet::empty().with(Verb::Open));
        let key = n.key;
        s.set_notices(vec![n]);
        s.chars("z", &fx.cx());
        press(&mut s, &fx, VK_DOWN, Mods::NONE);
        assert_eq!(sel_title(&s), Some("A notice"));
        open_list(&mut s, &fx);
        assert_eq!(acts(&s), [Act::Verb(Verb::Open), Act::DontShowAgain]);
        press(&mut s, &fx, VK_END, Mods::NONE);
        let out = press(&mut s, &fx, VK_RETURN, Mods::NONE);
        assert_eq!(out, [Effect::DismissNotice(key), Effect::Relayout]);
        assert!(s.rows().iter().all(|r| r.key != key), "the row is gone");
        assert!(s.action_list().is_none());
    }

    #[test]
    fn run_as_administrator_on_a_typed_line_is_listed_but_inert() {
        let fx = Fx::new();
        let mut s = typed(&fx, ">ping 1.1.1.1");
        assert_eq!(s.selected().unwrap().kind, Kind::RunLine);
        open_list(&mut s, &fx);
        let l = s.action_list().unwrap();
        let admin = l.items().iter().position(|e| e.act == Act::Verb(Verb::RunAsAdmin));
        if let Some(i) = admin {
            assert!(!l.items()[i].enabled, "no gesture can come from a key inside the list");
            press(&mut s, &fx, VK_HOME, Mods::NONE);
            for _ in 0..i {
                press(&mut s, &fx, VK_DOWN, Mods::NONE);
            }
            assert!(press(&mut s, &fx, VK_RETURN, Mods::NONE).is_empty());
            assert!(s.action_list().is_some(), "an inert entry leaves the list open");
        }
        // The run line's own verbs run: Run in a console.
        press(&mut s, &fx, VK_END, Mods::NONE);
        let last = s.action_list().map(ActionList::sel);
        assert!(last.is_some());
    }

    #[test]
    fn danger_marks_the_destructive_entries() {
        let fx = Fx::new();
        let mut s = typed(&fx, "blocco");
        open_list(&mut s, &fx);
        let danger: Vec<Act> = s.action_list().unwrap().items().iter().filter(|e| e.danger).map(|e| e.act).collect();
        assert_eq!(danger, [Act::Verb(Verb::CloseWindow), Act::Verb(Verb::Forget)]);
        // Open on a destructive row (Shut down) is dangerous too.
        let mut s = typed(&fx, "shut down");
        assert!(s.selected().is_some_and(|i| i.destructive), "{:?}", sel_title(&s));
        open_list(&mut s, &fx);
        let open = s.action_list().unwrap().items().iter().find(|e| e.act == Act::Verb(Verb::Open)).copied();
        assert_eq!(open.map(|e| e.danger), Some(true));
    }

    #[test]
    fn a_late_batch_that_drops_the_row_closes_the_list() {
        let fx = Fx::new();
        let mut s = typed(&fx, "blocco");
        open_list(&mut s, &fx);
        // Closing the window of the row from outside (it vanished): the next late batch finds no row.
        s.results.remove(s.selected().unwrap().key);
        let b = Batch { gen: s.gen(), from: ProviderId::Files, items: Vec::new(), done: true };
        s.late(b, &fx.cx());
        assert!(s.action_list().is_none());
    }

    /// The PowerToys notice (5.7): Enter takes the chord, the list offers the three choices and Don't show again.
    fn choices_notice() -> Item {
        let mut n = notice(VerbSet::empty().with(Verb::Open));
        n.payload = Payload::Choices(&[Choice::TakeChord, Choice::LeaveChord, Choice::UseAltShiftSpace]);
        n
    }

    #[test]
    fn a_notice_with_choices_runs_them_and_stays_gone() {
        let fx = Fx::new();
        // On the empty field the notice is the default: Enter takes the chord, and the row leaves for this open.
        let mut s = open(&fx);
        let n = choices_notice();
        let key = n.key;
        s.set_notices(vec![n.clone()]);
        let _ = s.refresh(&fx.cx());
        assert_eq!(sel_title(&s), Some("A notice"));
        assert_eq!(press(&mut s, &fx, VK_RETURN, Mods::NONE), [Effect::Choose(Choice::TakeChord), Effect::Relayout]);
        assert!(s.rows().iter().all(|r| r.key != key));
        s.chars("z", &fx.cx());
        assert!(s.rows().iter().all(|r| r.key != key), "a keystroke does not bring it back");
        // The action list: the choices in order, then Don't show again; each runs once.
        for (i, want) in [Choice::TakeChord, Choice::LeaveChord, Choice::UseAltShiftSpace].into_iter().enumerate() {
            let mut s = open(&fx);
            s.set_notices(vec![n.clone()]);
            let _ = s.refresh(&fx.cx());
            open_list(&mut s, &fx);
            assert_eq!(
                acts(&s),
                [
                    Act::Choice(Choice::TakeChord),
                    Act::Choice(Choice::LeaveChord),
                    Act::Choice(Choice::UseAltShiftSpace),
                    Act::DontShowAgain
                ]
            );
            assert_eq!(s.action_list().map(ActionList::sel), Some(0), "the list opens on what Enter does");
            for _ in 0..i {
                press(&mut s, &fx, VK_DOWN, Mods::NONE);
            }
            assert_eq!(press(&mut s, &fx, VK_RETURN, Mods::NONE), [Effect::Choose(want), Effect::Relayout]);
            assert!(s.rows().iter().all(|r| r.key != key) && s.action_list().is_none());
        }
        // Don't show again: dismissed, and gone for the next keystrokes too.
        let mut s = open(&fx);
        s.set_notices(vec![n.clone()]);
        let _ = s.refresh(&fx.cx());
        open_list(&mut s, &fx);
        press(&mut s, &fx, VK_END, Mods::NONE);
        assert_eq!(press(&mut s, &fx, VK_RETURN, Mods::NONE), [Effect::DismissNotice(key), Effect::Relayout]);
        s.chars("z", &fx.cx());
        assert!(s.rows().iter().all(|r| r.key != key));
    }

    #[test]
    fn a_notice_with_choices_never_takes_a_typed_enter() {
        let fx = Fx::new();
        // Typed text: the notice stays on top but the first match is the default, so Enter runs what was typed for.
        let mut s = open(&fx);
        s.set_notices(vec![choices_notice()]);
        for c in "calcolatrice".chars() {
            let mut b = [0u8; 4];
            s.chars(c.encode_utf8(&mut b), &fx.cx());
        }
        assert_eq!(s.rows().first().map(|r| r.kind), Some(Kind::Notice));
        assert_eq!(sel_title(&s), Some("Calcolatrice"));
        let out = press(&mut s, &fx, VK_RETURN, Mods::NONE);
        assert!(out.iter().any(|e| matches!(e, Effect::Launch(LaunchReq::App(_)))), "{out:?}");
        // Picked by hand it still runs.
        let mut s = open(&fx);
        s.set_notices(vec![choices_notice()]);
        s.chars("calcolatrice", &fx.cx());
        press(&mut s, &fx, VK_UP, Mods::NONE);
        assert_eq!(sel_title(&s), Some("A notice"));
        assert_eq!(press(&mut s, &fx, VK_RETURN, Mods::NONE), [Effect::Choose(Choice::TakeChord), Effect::Relayout]);
        // An Enter typed blind before the launcher showed never chooses.
        let mut s = open(&fx);
        s.set_notices(vec![choices_notice()]);
        let _ = s.refresh(&fx.cx());
        assert_eq!(sel_title(&s), Some("A notice"));
        let out = s.replay(&[ReplayKey::Key(k(VK_RETURN, Mods::NONE))], &fx.cx());
        assert!(!out.iter().any(|e| matches!(e, Effect::Choose(_))), "{out:?}");
        assert_eq!(sel_title(&s), Some("A notice"), "it stays for a real Enter");
    }
}
