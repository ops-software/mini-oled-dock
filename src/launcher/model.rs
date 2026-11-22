//! The launcher's model (SPEC-launcher 5.4.1): plain data and the small rules that belong to it. Pure: no Windows.
//! Not here (they borrow types of later tasks): `Context`, `LauncherOpts`, `TopK`, `Results` and the synchronous
//! `Provider` trait, which are declared by providers.rs (L1a-8).

use crate::start_search::Keys;
use core::cmp::Reverse;
use core::ops::Range;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

// ---------------------------------------------------------------- providers

/// The providers of the launcher. The discriminant is the bit number in a `ProviderMask` and a byte of the item key.
#[repr(u8)]
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum ProviderId {
    Apps,
    Windows,
    Settings,
    Commands,
    Calc,
    Units,
    DateTime,
    Run,
    Web,
    Files,
    Registry,
}

impl ProviderId {
    /// The names a config file may use (5.6). Units answers under `calc`.
    pub fn from_name(name: &str) -> Option<ProviderId> {
        Some(match name {
            "apps" => ProviderId::Apps,
            "windows" => ProviderId::Windows,
            "files" => ProviderId::Files,
            "settings" => ProviderId::Settings,
            "calc" => ProviderId::Calc,
            "commands" => ProviderId::Commands,
            "run" => ProviderId::Run,
            "web" => ProviderId::Web,
            "datetime" => ProviderId::DateTime,
            "registry" => ProviderId::Registry,
            _ => return None,
        })
    }

    #[cfg(test)]
    pub fn name(self) -> &'static str {
        match self {
            ProviderId::Apps => "apps",
            ProviderId::Windows => "windows",
            ProviderId::Settings => "settings",
            ProviderId::Commands => "commands",
            ProviderId::Calc | ProviderId::Units => "calc",
            ProviderId::DateTime => "datetime",
            ProviderId::Run => "run",
            ProviderId::Web => "web",
            ProviderId::Files => "files",
            ProviderId::Registry => "registry",
        }
    }
}

/// The providers that are switched on. Apps cannot be switched off (5.6).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct ProviderMask(u16);

impl ProviderMask {
    const APPS: u16 = 1 << (ProviderId::Apps as u16);

    pub fn all() -> ProviderMask {
        ProviderMask((1u16 << (ProviderId::Registry as u16 + 1)) - 1)
    }

    /// Units answers under `calc`, so it shares Calc's bit.
    fn bit(id: ProviderId) -> u16 {
        let id = if id == ProviderId::Units { ProviderId::Calc } else { id };
        1u16 << (id as u16)
    }

    pub fn with(self, id: ProviderId, on: bool) -> ProviderMask {
        let bit = Self::bit(id);
        let bits = if on { self.0 | bit } else { self.0 & !bit };
        ProviderMask(bits | Self::APPS)
    }

    pub fn has(self, id: ProviderId) -> bool {
        self.0 & Self::bit(id) != 0
    }
}

impl Default for ProviderMask {
    fn default() -> Self {
        ProviderMask::all()
    }
}

// ---------------------------------------------------------------- query

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Mode {
    Mixed,
    Only(ProviderId),
}

/// The locale's marks (from `GetLocaleInfoEx` at open, handed in by the caller).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct NumberStyle {
    pub decimal: char,
    pub group: char,
}

/// One parsed keystroke's worth of input (`query::parse_query`).
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Query<'a> {
    /// What the field holds, prefix included.
    pub raw: &'a str,
    /// `raw` without the recognised prefix: always a suffix of it.
    pub text: &'a str,
    /// `text`, folded once (lowercase, accents removed).
    pub folded: String,
    pub mode: Mode,
    pub gen: u64,
    pub num: NumberStyle,
    /// The recognised prefix's provider: the mode chip.
    pub chip: Option<ProviderId>,
    /// A word prefix is active: the apps provider still answers the raw text at tier 1 or better ("file explorer").
    pub apps_also: bool,
}

/// A query that a worker thread owns.
#[allow(dead_code)] // L2-2: the files worker takes it
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct OwnedQuery {
    pub text: String,
    pub folded: String,
    pub mode: Mode,
    pub gen: u64,
    pub num: NumberStyle,
}

impl Query<'_> {
    #[allow(dead_code)] // L2-2: the files worker is sent it
    pub fn to_owned_query(&self) -> OwnedQuery {
        OwnedQuery {
            text: self.text.to_string(),
            folded: self.folded.clone(),
            mode: self.mode,
            gen: self.gen,
            num: self.num,
        }
    }
}

/// Checked between row fetches by an asynchronous provider.
#[allow(dead_code)] // L2-2: the files worker checks it
#[derive(Clone, Default, Debug)]
pub struct CancelToken(Arc<AtomicBool>);

#[allow(dead_code)] // L2-2: the files worker checks it
impl CancelToken {
    pub fn cancel(&self) {
        self.0.store(true, Ordering::Relaxed);
    }

    pub fn is_cancelled(&self) -> bool {
        self.0.load(Ordering::Relaxed)
    }
}

// ---------------------------------------------------------------- items

/// A stable identity of a row: FNV-1a 64 over the provider byte and the row's stable id, so keys survive restarts.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub struct ItemKey(pub u64);

impl ItemKey {
    pub fn of(provider: ProviderId, stable_id: &str) -> ItemKey {
        const OFFSET: u64 = 0xcbf2_9ce4_8422_2325;
        const PRIME: u64 = 0x0000_0100_0000_01b3;
        let mut h = OFFSET;
        for b in core::iter::once(provider as u8).chain(stable_id.bytes()) {
            h ^= u64::from(b);
            h = h.wrapping_mul(PRIME);
        }
        ItemKey(h)
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Kind {
    App,
    Window,
    Setting,
    Command,
    Answer,
    RunLine,
    RunSuggestion,
    File,
    Folder,
    Web,
    Notice,
}

/// The order of results (5.2.5): the derived order compares the fields as declared, smaller is better.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug)]
pub struct Score {
    pub tier: u8,
    pub learned: Reverse<u16>,
    /// Not running: a running row sorts first.
    pub idle: bool,
    pub kind: u8,
    pub rank: u32,
    pub len: u16,
}

/// The system commands of 1.5.0 (their table, names and keywords are in commands.rs, L1a-7).
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum CommandId {
    Lock,
    Sleep,
    Restart,
    ShutDown,
    SignOut,
    Registry,
    WindowMenu,
    #[allow(dead_code)] // L2-6: the Clipboard history command
    ClipboardHistory,
}

/// A line typed in the run mode, split by syntax only (5.4.9).
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct RunLine {
    pub line: Box<str>,
    /// Byte range of the file part inside `line`.
    pub file: Range<usize>,
    pub console: bool,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum FileClass {
    Executable,
    #[allow(dead_code)] // L2-1: the files provider's classes
    Document,
    #[allow(dead_code)] // L2-1
    Image,
    #[allow(dead_code)] // L2-1
    Archive,
    Other,
}

#[derive(Clone, PartialEq, Eq, Debug)]
pub enum Payload {
    AppId(Box<str>),
    /// The bar's group key.
    Group(Box<str>),
    Hwnd(isize),
    /// Index into the held settings list.
    Setting(u32),
    Command(CommandId),
    Value {
        shown: Box<str>,
        raw: Box<str>,
    },
    RunLine(RunLine),
    Url(Box<str>),
    Path {
        path: Box<str>,
        dir: bool,
        class: FileClass,
    },
    None,
}

/// The glyphs a row can carry (the shapes are in glyphs.rs; L1a-14 draws them).
#[repr(u8)]
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum GlyphId {
    Magnifier,
    Equals,
    Prompt,
    Window,
    Document,
    #[allow(dead_code)] // L2-3: file rows
    Image,
    #[allow(dead_code)] // L2-3: file rows
    Archive,
    Executable,
    Copy,
    Globe,
    #[allow(dead_code)] // L2-4: date and time
    Clock,
    Folder,
    Info,
    Gear,
    Lock,
    Moon,
    Restart,
    Power,
    Exit,
}

#[derive(Clone, PartialEq, Eq, Debug)]
pub enum IconRef {
    /// A catalogue app, by its id.
    App(Box<str>),
    /// The bar's icon of a group key.
    Bar(Box<str>),
    Glyph(GlyphId),
    #[cfg(test)]
    Placeholder,
}

#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Item {
    pub key: ItemKey,
    pub from: ProviderId,
    pub kind: Kind,
    pub title: Box<str>,
    pub detail: Option<Box<str>>,
    pub icon: IconRef,
    pub score: Score,
    pub verbs: VerbSet,
    pub payload: Payload,
    pub destructive: bool,
}

/// The rows an asynchronous provider hands back, tagged with the generation of the query that asked.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Batch {
    pub gen: u64,
    pub from: ProviderId,
    pub items: Vec<Item>,
    pub done: bool,
}

/// An asynchronous provider (files, L2): its own worker thread, abandon-and-replace.
#[allow(dead_code)] // L2-2: the files worker implements it
pub trait AsyncProvider: Send {
    fn id(&self) -> ProviderId;
    fn wants(&self, q: &Query) -> bool;
    fn run(&mut self, q: &OwnedQuery, cancel: &CancelToken, sink: &mut dyn FnMut(Batch));
    fn debounce_ms(&self) -> u32 {
        50
    }
    /// After this the busy mark stays, rows are still accepted.
    fn first_batch_ms(&self) -> u32 {
        250
    }
    /// Later batches are dropped.
    fn deadline_ms(&self) -> u32 {
        1_000
    }
}

// ---------------------------------------------------------------- verbs and chords

/// The eight verbs (5.2.8). The discriminant is the bit in a `VerbSet` and the index in `verbs::VERBS`.
#[repr(u8)]
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Verb {
    Open,
    Switch,
    RunAsAdmin,
    Reveal,
    Copy,
    CloseWindow,
    Forget,
    RunInConsole,
}

/// Why a verb exists (5.1 scope tags).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Tag {
    OwnerAsked,
    Parity,
    Required,
}

#[derive(Clone, Copy, PartialEq, Eq, Default, Debug)]
pub struct VerbSet(u8);

impl VerbSet {
    pub const fn empty() -> VerbSet {
        VerbSet(0)
    }

    pub const fn with(self, v: Verb) -> VerbSet {
        VerbSet(self.0 | 1 << (v as u8))
    }

    pub const fn has(self, v: Verb) -> bool {
        self.0 & (1 << (v as u8)) != 0
    }

    pub const fn without(self, v: Verb) -> VerbSet {
        VerbSet(self.0 & !(1 << (v as u8)))
    }

    pub const fn is_empty(self) -> bool {
        self.0 == 0
    }
}

/// Modifier keys of a chord or a key event.
#[derive(Clone, Copy, PartialEq, Eq, Default, Debug)]
pub struct Mods(pub u8);

impl Mods {
    pub const NONE: Mods = Mods(0);
    pub const ALT: Mods = Mods(1);
    pub const CTRL: Mods = Mods(2);
    pub const SHIFT: Mods = Mods(4);
    pub const WIN: Mods = Mods(8);

    pub const fn has(self, other: Mods) -> bool {
        self.0 & other.0 == other.0
    }

    pub const fn with(self, other: Mods) -> Mods {
        Mods(self.0 | other.0)
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct KeyChord {
    pub vk: u16,
    pub mods: Mods,
}

/// The chord that summons the launcher (5.6): the hook and the config both read it through `parse`.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct ChordSpec {
    pub vk: u16,
    pub mods: Mods,
}

impl ChordSpec {
    const VK_SPACE: u16 = 0x20;

    /// "alt+space" and "alt+shift+space"; anything else (including "off") is None.
    pub fn parse(s: &str) -> Option<ChordSpec> {
        let mods = match s {
            "alt+space" => Mods::ALT,
            "alt+shift+space" => Mods::ALT.with(Mods::SHIFT),
            _ => return None,
        };
        Some(ChordSpec { vk: Self::VK_SPACE, mods })
    }
}

// `tag` is the scope record of 5.1: only the tests read it.
#[allow(dead_code)]
#[derive(Clone, Copy)]
pub struct VerbSpec {
    pub verb: Verb,
    pub chord: Option<KeyChord>,
    pub label: fn() -> &'static str,
    pub glyph: GlyphId,
    pub destructive: bool,
    pub tag: Tag,
}

// ---------------------------------------------------------------- rows of the context

/// One open window, in most-recently-used order (built by launcher_sys at each open, L1a-16).
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct WindowRow {
    pub hwnd: isize,
    pub group_key: Box<str>,
    pub aumid: Option<Box<str>>,
    pub name: Box<str>,
    pub name_folded: Box<str>,
    pub title: Option<Box<str>>,
    pub hung: bool,
}

/// One Windows setting with its folded name (built from `start_search::settings()`, L1a-16).
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct SettingRow {
    /// Index into the held settings list.
    pub index: u32,
    pub name: Box<str>,
    pub folded: Box<str>,
    /// Start offsets of the words of `folded`.
    pub words: Box<[u16]>,
    pub initials: Box<str>,
    pub keys: Keys,
    pub rank: u32,
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::indexing_slicing)]
mod tests {
    use super::*;

    #[test]
    fn chord_spec_parse() {
        let a = ChordSpec::parse("alt+space").unwrap();
        assert_eq!((a.vk, a.mods), (0x20, Mods::ALT));
        let b = ChordSpec::parse("alt+shift+space").unwrap();
        assert_eq!((b.vk, b.mods), (0x20, Mods(5)));
        for bad in ["off", "ctrl+alt+space", "", "Alt+Space", "alt+space "] {
            assert_eq!(ChordSpec::parse(bad), None, "{bad:?}");
        }
    }

    #[test]
    fn provider_mask_keeps_apps() {
        assert!(ProviderMask::all().with(ProviderId::Apps, false).has(ProviderId::Apps));
        let m = ProviderMask::all().with(ProviderId::Web, false);
        assert!(!m.has(ProviderId::Web));
        assert!(m.has(ProviderId::Files));
        assert!(m.with(ProviderId::Web, true).has(ProviderId::Web));
        assert_eq!(ProviderMask::all().with(ProviderId::Registry, true), ProviderMask::all());
    }

    #[test]
    fn provider_names_round_trip() {
        let names = ["apps", "windows", "files", "settings", "calc", "commands", "run", "web", "datetime", "registry"];
        for n in names {
            assert_eq!(ProviderId::from_name(n).unwrap().name(), n);
        }
        assert_eq!(ProviderId::from_name("units"), None);
        assert_eq!(ProviderId::from_name("Apps"), None);
        assert_eq!(ProviderId::Units.name(), "calc");
    }

    #[test]
    fn units_follows_calc_in_the_mask() {
        let off = ProviderMask::all().with(ProviderId::Calc, false);
        assert!(!off.has(ProviderId::Calc) && !off.has(ProviderId::Units));
        assert!(off.with(ProviderId::Units, true).has(ProviderId::Calc));
        assert!(ProviderMask::all().has(ProviderId::Units));
        assert!(off.has(ProviderId::Apps));
    }

    #[test]
    fn item_key_is_stable() {
        let id = "Microsoft.WindowsCalculator_8wekyb3d8bbwe!App";
        let a = ItemKey::of(ProviderId::Apps, id);
        assert_eq!(a, ItemKey::of(ProviderId::Apps, id));
        assert_ne!(a, ItemKey::of(ProviderId::Windows, id));
        assert_eq!(a, ItemKey(PINNED_KEY));
    }
    const PINNED_KEY: u64 = 0xa1a9_d031_1782_3a38;

    #[test]
    fn verb_set_and_mods() {
        let s = VerbSet::empty().with(Verb::Open).with(Verb::Forget);
        assert!(s.has(Verb::Open) && s.has(Verb::Forget) && !s.has(Verb::Copy));
        assert!(!s.without(Verb::Forget).has(Verb::Forget) && s.without(Verb::Forget).has(Verb::Open));
        assert!(VerbSet::empty().is_empty());
        assert!(Mods(7).has(Mods::CTRL.with(Mods::SHIFT)));
        assert!(!Mods::CTRL.has(Mods::ALT));
    }

    #[test]
    fn score_orders_by_tier_then_learned_then_running() {
        let base = Score { tier: 1, learned: Reverse(0), idle: true, kind: 0, rank: 5, len: 4 };
        assert!(Score { tier: 0, ..base } < base);
        assert!(Score { learned: Reverse(3), ..base } < base);
        assert!(Score { idle: false, ..base } < base);
        assert!(Score { len: 3, ..base } < base);
    }

    #[test]
    fn cancel_token_is_shared() {
        let t = CancelToken::default();
        let u = t.clone();
        assert!(!u.is_cancelled());
        t.cancel();
        assert!(u.is_cancelled());
    }
}
