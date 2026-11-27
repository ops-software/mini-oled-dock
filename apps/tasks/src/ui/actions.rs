//! Row verbs and their states (T3.8): Close or End by row, the inline action, busy rows, ended rows with Reopen, the
//! outcome notes, and the commands that act. Every act goes through deps.exec (the Executor); nothing asks for a
//! confirmation (a menu is protection enough), and nothing is ended by itself.

use std::collections::HashMap;

use oled_shellmodel::identity::aumid_from_launch;
use oled_sys::pipe::Integrity;
use oled_sysmon::heat::HeatStep;
use oled_sysmon::ProcKey;
use oled_ui::model::{FgRole, ListModel, RowAction, RowKey, TableOut};
use oled_ui::node::Text;
use oled_ui::verbs::Verb;
use oled_ui::widgets::VirtualTable;
use oled_ui::{CommandId, EventCx, TimerId, UiLang};

use super::commands::{CLOSE_OR_END, END_NOW, END_TREE, REOPEN};
use super::grouped::HostEnv;
use super::rows::{group_key, proc_key, Decor, RowAct, RowEntry, RowKind, TasksTable};
use super::screen::{TasksScreen, TIMER_CLOSE_CHECK, TIMER_NOTES};
use super::view::View;
use super::{context_menu, order};
use crate::app::access::{self, AccessBook, FixtureProber, Prober, Standing, Win32Prober};
use crate::app::exec::{ActVerb, ActionDone, Batch, KillGuard, Outcome, Target};
use crate::app::Deps;
use crate::model::classify::{ImagePath, PathClass};
use crate::model::group::{AppKey, Grouping};
use crate::model::view::{Section, ViewMode};
use crate::strings::{k, t};

/// A row in flight shows "Closing..." or "Ending..." only after this (a quick act shows nothing).
pub const BUSY_DELAY_MS: u64 = 300;
/// An ended Apps group offers Reopen this long.
pub const REOPEN_MS: u64 = 10_000;
/// An outcome note (and the "Ended" of a row we ended) shows this long.
pub const NOTE_MS: u64 = 6_000;
/// A Close is looked at again after this: still running, its row says so and offers End now.
pub const CLOSE_CHECK_MS: u64 = 5_000;
/// A batch whose result never comes (no executor attached) stops counting as in flight after this.
const BUSY_MAX_MS: u64 = 10_000;
/// The launch prefix of a packaged app (the dock's launch string).
const APPS_FOLDER: &str = "shell:AppsFolder\\";

/// The verb a row offers.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RowVerb {
    /// A responding Apps group with windows: WM_CLOSE, so it may ask to save.
    Close,
    /// A hung or windowless Apps group, every Background row, a member, a Tree or List process.
    End,
    /// A Part of Windows row: from the menu only, with its warning.
    EndTask,
}

/// What the build knows of a group, copied each pass (note_groups) for the rows' verbs and the batches.
#[derive(Clone, Copy, Debug)]
struct GroupFact {
    key: AppKey,
    section: Section,
    start: usize,
    len: usize,
    windows: u32,
    hung: u32,
}

#[derive(Debug, Default)]
struct GroupBook {
    index: HashMap<RowKey, GroupFact>,
    members: Vec<ProcKey>,
}

impl GroupBook {
    fn members(&self, f: &GroupFact) -> &[ProcKey] {
        self.members.get(f.start..f.start + f.len).unwrap_or(&[])
    }
}

/// Where a row is in its act.
#[derive(Clone, Debug, PartialEq)]
enum Phase {
    /// A batch is out since `since`.
    Busy { verb: ActVerb, since: u64 },
    /// WM_CLOSE posted; the row is looked at again at `at`.
    Closing { since: u64, at: u64 },
    /// Still running after the close check: End now is offered.
    StillOpen,
    /// Ended by us: "Ended" until `until`, with Reopen for an Apps group.
    Ended { until: u64, reopen: bool },
    /// An outcome note until `until`.
    Note { until: u64, text: Text, fg: FgRole },
}

/// A row we acted on: its phase and what it was when the batch was made (its frozen values, and its look and
/// place for a ghost).
#[derive(Clone, Debug)]
struct RowState {
    phase: Phase,
    entry: RowEntry,
    label: String,
    index: usize,
    members: Vec<ProcKey>,
    launch: Option<String>,
    apps: bool,
    guard: KillGuard,
}

/// Rows in flight and their outcomes, the Reopen window, the note expiries.
#[derive(Default)]
pub struct ActionState {
    /// Fixed by scenes and tests (with the fixture prober); None: the system's.
    env: Option<HostEnv>,
    fixture: Option<FixtureProber>,
    access: AccessBook,
    groups: GroupBook,
    states: HashMap<RowKey, RowState>,
    /// A table output that needs the whole screen (an inline action, the context menu): taken at the end of the call.
    pending: Option<TableOut>,
    /// The rows the open context menu acts on.
    menu: Option<Vec<RowKey>>,
    notes_armed: Option<u64>,
    close_armed: Option<u64>,
}

impl ActionState {
    /// Fixes the shell, own process and session the guard is built against (scenes and tests); the system is then
    /// never probed: every process counts as alive as displayed and terminable, except the fixture's limited keys.
    pub fn set_env(&mut self, e: HostEnv) {
        self.env = Some(e);
        self.fixture.get_or_insert_with(FixtureProber::default);
    }

    /// The keys a fixed environment's probe reports as limited (needs administrator).
    pub fn set_fixture_limited(&mut self, keys: Vec<ProcKey>) {
        self.fixture.get_or_insert_with(FixtureProber::default).limited = keys;
    }

    /// The keys a fixed environment reports as running below Tasks, each with the package AUMID of its token.
    pub fn set_fixture_below(&mut self, below: Vec<(ProcKey, Option<String>)>) {
        self.fixture.get_or_insert_with(FixtureProber::default).below = below;
    }

    /// The level a fixed environment reports for Tasks and every process not below it; medium unless set.
    pub fn set_fixture_own(&mut self, own: Option<Integrity>) {
        self.fixture.get_or_insert_with(FixtureProber::default).own = own;
    }

    /// True while a batch is out or a Close waits for its check (the order holds).
    pub fn is_busy(&self) -> bool {
        self.states.values().any(|s| matches!(s.phase, Phase::Busy { .. } | Phase::Closing { .. }))
    }

    /// The rows the open context menu acts on.
    pub fn menu_target(&self) -> Option<&[RowKey]> {
        self.menu.as_deref()
    }

    /// The context menu opened over these rows: a pick acts on them.
    pub(crate) fn set_menu(&mut self, keys: Vec<RowKey>) {
        self.menu = Some(keys);
    }

    pub(crate) fn env(&self) -> HostEnv {
        self.env.unwrap_or_else(HostEnv::live)
    }

    /// The probe of `key`: the system's, or the fixture's in scenes and tests.
    pub fn probe(&mut self, key: ProcKey) -> access::Probe {
        let p: &dyn Prober = match &self.fixture {
            Some(f) => f,
            None => &Win32Prober,
        };
        self.access.probe(key, p)
    }

    /// Tasks' own level and the standings of `keys`, read now: the system's, or the fixture's.
    fn standings(&self, keys: &[ProcKey]) -> (Option<Integrity>, Vec<Standing>) {
        let p: &dyn Prober = match &self.fixture {
            Some(f) => f,
            None => &Win32Prober,
        };
        (p.own_level(), keys.iter().map(|k| p.standing(*k)).collect())
    }

    /// The group facts of a row of the last build.
    fn group(&self, key: RowKey) -> Option<(GroupFact, &[ProcKey])> {
        let f = *self.groups.index.get(&key)?;
        Some((f, self.groups.members(&f)))
    }
}

/// The screen's pipeline gives the actions the groups of the build it just ran (Grouped mode).
pub fn note_groups(st: &mut ActionState, g: &Grouping) {
    let book = &mut st.groups;
    book.index.clear();
    book.members.clear();
    for grp in &g.groups {
        let key = match (grp.key, grp.members.len()) {
            (AppKey::Single(p), 1) => proc_key(p),
            (k, _) => group_key(k),
        };
        let start = book.members.len();
        book.members.extend_from_slice(&grp.members);
        let fact = GroupFact {
            key: grp.key,
            section: grp.section,
            start,
            len: grp.members.len(),
            windows: grp.windows,
            hung: grp.hung_windows,
        };
        book.index.insert(key, fact);
    }
}

/// The verb of a row: Close for a responding Apps group with windows, End for a hung or windowless one, every
/// Background row, a member and a Tree or List process, End task under Part of Windows; none for sections, the
/// hardware row and ghosts.
pub fn verb_of(r: &RowEntry, group: Option<(u32, u32)>) -> Option<RowVerb> {
    if r.out.ghost || r.out.disabled {
        return None;
    }
    match r.kind {
        RowKind::Section(_) | RowKind::Hardware | RowKind::Ghost(_) => return None,
        RowKind::Group(_) | RowKind::Proc(_) => {}
    }
    Some(match r.section {
        Some(Section::Windows) => RowVerb::EndTask,
        Some(Section::Apps) if r.depth == 0 => match group {
            Some((windows, hung)) if windows > 0 && hung == 0 => RowVerb::Close,
            _ => RowVerb::End,
        },
        _ => RowVerb::End,
    })
}

/// The verb of a row of the last build. Tree and List rows carry no section: there a Part of Windows process gets
/// End task too (the menu only, with its warning), since the verb follows the kind of process, not the view.
pub fn row_verb(st: &ActionState, v: &View, r: &RowEntry) -> Option<RowVerb> {
    let g = st.group(r.key).map(|(f, _)| (f.windows, f.hung));
    let verb = verb_of(r, g)?;
    match (r.section, r.kind) {
        (None, RowKind::Proc(p)) if part_of_windows(v, p, st.env().session) => Some(RowVerb::EndTask),
        _ => Some(verb),
    }
}

/// Whether a process is Part of Windows by model::group's rule for a process with no window of its own: a path
/// under the Windows folder or no image, or session 0, unless it runs from an image in another user's session.
fn part_of_windows(v: &View, p: ProcKey, session: u32) -> bool {
    let procs = v.procs();
    let Some(i) = procs.index_of(p) else { return false };
    let Some(row) = procs.rows.get(i) else { return false };
    if v.frame().windows.iter().any(|w| w.owner == p) {
        return false;
    }
    let class = v.info_rows().get(i).filter(|x| x.key == p).map_or(PathClass::Unknown, |x| x.class);
    if class != PathClass::NoImage && row.session != 0 && row.session != session {
        return false;
    }
    matches!(class, PathClass::SystemRoot | PathClass::NoImage) || row.session == 0
}

/// The processes a row stands for: a group's members, else the process itself.
pub(crate) fn members_of(st: &ActionState, r: &RowEntry) -> Vec<ProcKey> {
    match r.kind {
        RowKind::Group(_) => st.group(r.key).map(|(_, m)| m.to_vec()).unwrap_or_default(),
        RowKind::Proc(p) => vec![p],
        _ => Vec::new(),
    }
}

/// The keys no batch may reach, from the latest frame (the dock family lives beside tasks.exe).
pub(crate) fn guard_of(st: &ActionState, v: &View, install_dir: Option<&std::path::Path>) -> KillGuard {
    let env = st.env();
    let dock = install_dir.map(|d| d.join("dock.exe").to_string_lossy().replace('/', "\\"));
    access::guard(v.procs(), &v.frame().paths, env.own_pid, env.shell_pid, env.session, dock.as_deref())
}

/// The inline action of a row: Close or End on the top rows of Apps and Background in Grouped mode, unless the
/// keys alone refuse every member (the shell, Tasks, its ancestors, the dock) or the row is another session's.
fn inline_action(st: &ActionState, r: &RowEntry, v: &View, g: &KillGuard, lang: UiLang) -> Option<RowAction> {
    if v.mode() != ViewMode::Grouped || r.depth != 0 || !matches!(r.section, Some(Section::Apps | Section::Background))
    {
        return None;
    }
    let verb = row_verb(st, v, r)?;
    // The members without a copy: this runs for every top row on every pass.
    let single: [ProcKey; 1];
    let members: &[ProcKey] = match r.kind {
        RowKind::Group(_) => st.group(r.key).map_or(&[][..], |(_, m)| m),
        RowKind::Proc(p) => {
            single = [p];
            &single
        }
        _ => return None,
    };
    let root = *members.first()?;
    let session = v.procs().find(root).map(|p| p.session);
    if session != Some(g.helper.session) || members.iter().all(|m| access::key_refusal(*m, g).is_some()) {
        return None;
    }
    let label = if verb == RowVerb::Close { Verb::Close.label(lang) } else { Verb::End.label(lang) };
    Some(RowAction { label, command: CLOSE_OR_END })
}

/// Moves a state on with the clock and the frame; false when it is over.
fn advance(s: &mut RowState, v: &View, now: u64) -> bool {
    let alive = |s: &RowState| s.members.iter().any(|m| v.procs().find(*m).is_some());
    match s.phase {
        Phase::Busy { since, .. } => now < since + BUSY_MAX_MS,
        Phase::Closing { at, .. } if now >= at => {
            let open = alive(s);
            if open {
                s.phase = Phase::StillOpen;
            }
            open
        }
        Phase::Closing { .. } => true,
        Phase::StillOpen => alive(s),
        Phase::Ended { until, .. } | Phase::Note { until, .. } => now < until,
    }
}

fn shows_busy(phase: &Phase, now: u64) -> Option<ActVerb> {
    match *phase {
        Phase::Busy { verb, since } if now >= since + BUSY_DELAY_MS => Some(verb),
        Phase::Closing { since, .. } if now >= since + BUSY_DELAY_MS => Some(ActVerb::Close),
        _ => None,
    }
}

/// An ended row's look: a ghost with its last values, quiet.
fn ghost_look(r: &mut RowEntry) {
    r.out.ghost = true;
    r.out.disabled = true;
    r.out.busy = false;
    r.out.fresh = false;
    r.vals.heat = [HeatStep::S0; 3];
    r.expandable = false;
    r.expanded = false;
}

/// The pipeline step: busy, ended and noted rows, the inline actions, and the ghosts of the rows we ended.
#[allow(clippy::ptr_arg)] // the frozen signature: ghosts are inserted
pub fn decorate(v: &View, st: &mut ActionState, rows: &mut Vec<RowEntry>, now: u64) {
    let lang = v.lang();
    st.states.retain(|_, s| advance(s, v, now));
    let guard = (v.mode() == ViewMode::Grouped).then(|| guard_of(st, v, v.install_dir()));
    let mut revived: Vec<RowKey> = Vec::new();
    for r in rows.iter_mut() {
        let Some(s) = st.states.get(&r.key) else {
            if let Some(a) = guard.as_ref().and_then(|g| inline_action(st, r, v, g, lang)) {
                v.set_act(r.key, RowAct { note: None, action: Some(a) });
            }
            continue;
        };
        let act = match &s.phase {
            Phase::Busy { .. } | Phase::Closing { .. } => match shows_busy(&s.phase, now) {
                Some(verb) => {
                    r.out.busy = true;
                    r.vals = s.entry.vals;
                    let key = if verb == ActVerb::Close { k::TASKS_ROW_CLOSING } else { k::TASKS_ROW_ENDING };
                    RowAct { note: Some((t(lang, key), FgRole::Secondary)), action: None }
                }
                None => RowAct::default(),
            },
            Phase::StillOpen => RowAct {
                note: Some((t(lang, k::TASKS_ROW_STILL_OPEN), FgRole::Attention)),
                action: Some(RowAction { label: Verb::EndNow.label(lang), command: END_NOW }),
            },
            Phase::Ended { reopen, .. } => {
                let live = !r.out.ghost && !matches!(r.kind, RowKind::Ghost(_));
                if live && !s.members.iter().any(|m| v.procs().find(*m).is_some()) {
                    // The same group runs again (reopened, or started anew): its row is a live row now.
                    revived.push(r.key);
                    continue;
                }
                ghost_look(r);
                if *reopen {
                    RowAct {
                        note: Some((t(lang, k::TASKS_ROW_ENDED_REOPEN), FgRole::Disabled)),
                        action: Some(RowAction { label: t(lang, k::TASKS_ROW_REOPEN), command: REOPEN }),
                    }
                } else {
                    RowAct { note: Some((t(lang, k::TASKS_ROW_ENDED), FgRole::Disabled)), action: None }
                }
            }
            Phase::Note { text, fg, .. } => RowAct { note: Some((text.clone(), *fg)), action: None },
        };
        if act != RowAct::default() {
            v.set_act(r.key, act);
        }
    }
    for key in revived {
        st.states.remove(&key);
    }
    insert_ghosts(v, st, rows, lang);
}

/// The rows we ended that left the list stay as ghosts, in their place, for the rest of their time.
fn insert_ghosts(v: &View, st: &ActionState, rows: &mut Vec<RowEntry>, lang: UiLang) {
    let mut ended: Vec<(&RowKey, &RowState)> =
        st.states.iter().filter(|(_, s)| matches!(s.phase, Phase::Ended { .. })).collect();
    if ended.is_empty() {
        return;
    }
    ended.sort_by_key(|(_, s)| s.index);
    for (key, s) in ended {
        if rows.iter().any(|r| r.key == *key) {
            continue;
        }
        let mut g = s.entry.clone();
        if let RowKind::Proc(p) = g.kind {
            g.kind = RowKind::Ghost(p);
        }
        ghost_look(&mut g);
        // Inside its section's run (from the section row to the next section row), else at its index.
        let (lo, hi) = match g.section {
            Some(sec) => {
                let lo = rows.iter().position(|r| r.kind == RowKind::Section(sec)).map_or(0, |i| i + 1);
                let hi = rows
                    .iter()
                    .skip(lo)
                    .position(|r| matches!(r.kind, RowKind::Section(_)))
                    .map_or(rows.len(), |i| lo + i);
                (lo, hi)
            }
            None => (0, rows.len()),
        };
        let at = s.index.clamp(lo, hi.max(lo));
        v.set_decor(*key, Decor { head: s.label.clone(), tail: String::new(), hung: false });
        let reopen = matches!(s.phase, Phase::Ended { reopen: true, .. });
        let act = if reopen {
            RowAct {
                note: Some((t(lang, k::TASKS_ROW_ENDED_REOPEN), FgRole::Disabled)),
                action: Some(RowAction { label: t(lang, k::TASKS_ROW_REOPEN), command: REOPEN }),
            }
        } else {
            RowAct { note: Some((t(lang, k::TASKS_ROW_ENDED), FgRole::Disabled)), action: None }
        };
        v.set_act(*key, act);
        rows.insert(at.min(rows.len()), g);
    }
}

/// TIMER_CLOSE_CHECK, TIMER_NOTES: spent; the screen runs the pipeline after this, which looks at the clock, and the
/// end of the call arms the next one.
pub fn on_timer(cx: &mut EventCx, st: &mut ActionState, id: TimerId) {
    let _ = cx;
    if id == TIMER_NOTES {
        st.notes_armed = None;
    } else if id == TIMER_CLOSE_CHECK {
        st.close_armed = None;
    }
}

/// RowAction and ContextMenu: both need the whole screen (the groups, the order's hold, the notice line), so they are
/// taken at the end of the call (end_call).
pub fn on_table_out(
    cx: &mut EventCx,
    st: &mut ActionState,
    out: &TableOut,
    table: &VirtualTable<TasksTable>,
    v: &View,
    deps: &Deps,
) {
    let _ = (cx, table, v, deps);
    if matches!(out, TableOut::RowAction(..) | TableOut::ContextMenu(..)) {
        st.pending = Some(out.clone());
    }
}

/// An outcome from the executor: the row's note, its ghost, Reopen, or the close check.
pub fn on_done(cx: &mut EventCx, st: &mut ActionState, m: ActionDone) {
    let (now, lang) = (cx.now_ms(), cx.lang());
    let Some(s) = st.states.get_mut(&m.row) else { return };
    let Phase::Busy { since, verb } = s.phase else { return };
    if verb == ActVerb::Close && m.outcome == Outcome::Gone {
        // Nothing was posted (no window of the row qualified any more) and nothing ended: the row is not shown as
        // Ended; its state is over and the frame tells the rest.
        st.states.remove(&m.row);
        return;
    }
    let note = |key, fg| Phase::Note { until: now + NOTE_MS, text: t(lang, key), fg };
    s.phase = match m.outcome {
        Outcome::Posted => Phase::Closing { since, at: now + CLOSE_CHECK_MS },
        Outcome::Done if m.refused > 0 => {
            let text = oled_sys::i18n::subst(&t(lang, k::TASKS_ROW_ENDED_KEPT), &[("n", &m.refused.to_string())]);
            Phase::Note { until: now + NOTE_MS, text: text.into(), fg: FgRole::Secondary }
        }
        Outcome::Done if s.apps && s.launch.is_some() => Phase::Ended { until: now + REOPEN_MS, reopen: true },
        Outcome::Done | Outcome::Gone => Phase::Ended { until: now + NOTE_MS, reopen: false },
        Outcome::NeedsAdmin => note(k::TASKS_ROW_NEEDS_ADMIN, FgRole::Danger),
        Outcome::Refused(r) => {
            let root = s.members.first().copied().unwrap_or(ProcKey { pid: 0, created: 0 });
            match access::why(r, root, &s.guard) {
                Some(why) => {
                    let text = oled_sys::i18n::subst(&t(lang, k::TASKS_ROW_REFUSED), &[("why", &t(lang, why))]);
                    Phase::Note { until: now + NOTE_MS, text: text.into(), fg: FgRole::Attention }
                }
                None => Phase::Ended { until: now + NOTE_MS, reopen: false },
            }
        }
        Outcome::Failed(code) => {
            let text = oled_sys::i18n::subst(&t(lang, k::TASKS_ROW_END_FAILED), &[("code", &code.to_string())]);
            Phase::Note { until: now + NOTE_MS, text: text.into(), fg: FgRole::Danger }
        }
    };
}

/// CLOSE_OR_END, END_NOW, END_TREE, REOPEN: on the rows of the context menu when one was open, else on the
/// selection (or the focused row).
pub fn on_command(s: &mut TasksScreen, cx: &mut EventCx, id: CommandId) {
    let now = cx.now_ms();
    let (keys, from_menu) = match s.p.actions.menu.take() {
        Some(keys) => (keys, true),
        None => (selected_keys(&s.table), false),
    };
    act(s, cx, id, &keys, from_menu, now);
}

/// The end of every event and data call (screen.rs): a menu that ended forgets its rows, the table's pending output
/// acts, the order holds while a row is in flight, and the two one-shot timers follow the deadlines.
pub fn end_call(s: &mut TasksScreen, cx: &mut EventCx, now: u64) {
    if s.p.actions.menu.is_some() && !order::menu_open(&s.p.order) {
        s.p.actions.menu = None;
    }
    match s.p.actions.pending.take() {
        Some(TableOut::RowAction(key, id)) => act(s, cx, id, &[key], true, now),
        Some(TableOut::ContextMenu(key, at)) => context_menu::open(cx, s, key, at),
        _ => {}
    }
    order::set_row_busy(&mut s.p.order, s.p.actions.is_busy());
    sync_timers(cx, &mut s.p.actions, now);
}

/// The selected rows' keys in table order, or the focused row's when nothing is selected.
pub(crate) fn selected_keys(table: &VirtualTable<TasksTable>) -> Vec<RowKey> {
    let mut keys = Vec::new();
    table.selection().for_each(table.model(), &mut |_, key| keys.push(key));
    if keys.is_empty() {
        keys.extend(table.focused().filter(|k| table.model().row_of(*k).is_some()));
    }
    keys
}

/// The launch string of an Apps group: the Apps folder entry of its windows' AUMID, else its root's exe.
fn launch_of(s: &TasksScreen, app: AppKey, members: &[ProcKey]) -> Option<String> {
    let frame = s.v.frame();
    for w in frame.windows.iter().filter(|w| members.contains(&w.owner)) {
        if let Some(id) = s.p.names.identity(w.hwnd, w.owner.pid) {
            if let Some(a) = id.aumid.clone().or_else(|| aumid_from_launch(&id.key)) {
                return Some(format!("{APPS_FOLDER}{a}"));
            }
        }
    }
    if let AppKey::App(id) = app {
        if let Some(a) = aumid_from_launch(&s.v.interned(id.0)) {
            return Some(format!("{APPS_FOLDER}{a}"));
        }
    }
    match frame.paths.get(*members.first()?) {
        Some(ImagePath::Path(p)) => Some(p.to_string()),
        _ => None,
    }
}

/// What Reopen may start for an Apps group, only from a Tasks at medium: an elevated Tasks would start the string at
/// its own level, resolved then through what a medium process may write (the file at an image path in a user folder,
/// a per-user shortcut carrying an AUMID), so it starts nothing. `launch` is launch_of's: a window's AUMID, the
/// group's key or the root's image path, all named by the group's own processes (a window's AUMID is set by its owner
/// or a process above it, the image is the file its starter chose). It stands only when the root and every member
/// owning a window run at medium or above: then it names nothing they could not start themselves. Below that, only
/// the package AUMID of the root's token (set by the system: its package's own app starts, in its own container);
/// else nothing. `sources` holds the root first. Pure.
pub fn reopen_launch(own: Option<Integrity>, sources: &[Standing], launch: Option<String>) -> Option<String> {
    let own = own.filter(|l| *l == Integrity::Medium)?;
    let root = sources.first()?;
    if sources.iter().all(|s| s.level.is_some_and(|l| l >= own)) {
        return launch;
    }
    let package = root.package.as_deref().filter(|a| !a.is_empty())?;
    Some(format!("{APPS_FOLDER}{package}"))
}

/// launch_of judged by reopen_launch on the standings read now, as End goes out: the root first, then each other
/// member owning a window of the latest frame (the windows launch_of reads).
fn reopen_of(s: &TasksScreen, app: AppKey, members: &[ProcKey]) -> Option<String> {
    let mut sources = vec![*members.first()?];
    for w in &s.v.frame().windows {
        if members.contains(&w.owner) && !sources.contains(&w.owner) {
            sources.push(w.owner);
        }
    }
    let (own, standings) = s.p.actions.standings(&sources);
    reopen_launch(own, &standings, launch_of(s, app, members))
}

/// The batches of one command over `keys`. A keyboard Del or Shift+Del on Part of Windows (its section, or a Part of
/// Windows process in Tree or List) is refused with the notice line (the menu's End task is the way); a row already in
/// flight is left alone.
fn act(s: &mut TasksScreen, cx: &mut EventCx, id: CommandId, keys: &[RowKey], from_menu: bool, now: u64) {
    let lang = cx.lang();
    if id == REOPEN {
        if reopen(s, keys) {
            s.run_pipeline(cx, now);
        }
        return;
    }
    let rows: Vec<(usize, RowEntry, String)> = keys
        .iter()
        .filter_map(|key| {
            let model = s.table.model();
            let i = model.row_of(*key)?;
            Some((i, model.rows.get(i)?.clone(), model.name(i).to_string()))
        })
        .collect();
    if !from_menu
        && matches!(id, CLOSE_OR_END | END_NOW)
        && rows.iter().any(|(_, r, _)| {
            r.section == Some(Section::Windows) || row_verb(&s.p.actions, &s.v, r) == Some(RowVerb::EndTask)
        })
    {
        s.notices.show(cx, t(lang, k::TASKS_WHY_DEL_WINDOWS), None);
        return;
    }
    let guard = guard_of(&s.p.actions, &s.v, s.deps.install_dir.as_deref());
    let edges = access::edges(s.v.procs());
    // One batch per verb, and per reach: a group and End process tree take the descendants, a process row alone ends
    // only itself.
    let mut batches: Vec<(ActVerb, bool, Vec<Target>)> = Vec::new();
    for (index, r, label) in rows {
        let busy =
            s.p.actions
                .states
                .get(&r.key)
                .is_some_and(|st| matches!(st.phase, Phase::Busy { .. } | Phase::Closing { .. }));
        let Some(verb) = row_verb(&s.p.actions, &s.v, &r).filter(|_| !busy) else { continue };
        let is_proc = matches!(r.kind, RowKind::Proc(_));
        let (av, tree) = match (id, verb) {
            (CLOSE_OR_END, RowVerb::Close) => (ActVerb::Close, false),
            (CLOSE_OR_END, _) => (ActVerb::End, !is_proc),
            (END_NOW, _) => (ActVerb::EndNow, !is_proc),
            (END_TREE, _) if is_proc => (ActVerb::EndTree, true),
            _ => continue,
        };
        let members = members_of(&s.p.actions, &r);
        if members.is_empty() {
            continue;
        }
        let windows: Vec<isize> = if av == ActVerb::Close {
            s.v.frame().windows.iter().filter(|w| members.contains(&w.owner)).map(|w| w.hwnd).collect()
        } else {
            Vec::new()
        };
        let group = s.p.actions.group(r.key).map(|(f, _)| f);
        let apps = matches!(group, Some(f) if f.section == Section::Apps) && matches!(r.kind, RowKind::Group(_));
        let launch = match group {
            Some(f) if apps && av != ActVerb::Close => reopen_of(s, f.key, &members),
            _ => None,
        };
        let target = Target { row: r.key, members: members.clone(), windows, launch: launch.clone() };
        match batches.iter_mut().find(|(v, t, _)| *v == av && *t == tree) {
            Some((_, _, targets)) => targets.push(target),
            None => batches.push((av, tree, vec![target])),
        }
        let state = RowState {
            phase: Phase::Busy { verb: av, since: now },
            entry: r,
            label,
            index,
            members,
            launch,
            apps,
            guard: guard.clone(),
        };
        s.p.actions.states.insert(state.entry.key, state);
    }
    let submitted = !batches.is_empty();
    for (verb, tree, targets) in batches {
        let edges = if tree { edges.clone() } else { Vec::new() };
        s.deps.exec.submit(Batch { verb, targets, guard: guard.clone(), edges });
    }
    // The rows in flight lose their inline action at once.
    if submitted {
        s.run_pipeline(cx, now);
    }
}

/// Reopen of the rows that offer it: their launch strings through the executor (the launcher on the act thread); the
/// ghost goes at once. True when a batch went out.
fn reopen(s: &mut TasksScreen, keys: &[RowKey]) -> bool {
    let mut targets = Vec::new();
    let mut guard = None;
    for key in keys {
        let Some(st) = s.p.actions.states.get(key) else { continue };
        if !matches!(st.phase, Phase::Ended { reopen: true, .. }) {
            continue;
        }
        let Some(launch) = st.launch.clone() else { continue };
        guard.get_or_insert_with(|| st.guard.clone());
        targets.push(Target { row: *key, members: Vec::new(), windows: Vec::new(), launch: Some(launch) });
        s.p.actions.states.remove(key);
    }
    match (guard, targets.is_empty()) {
        (Some(guard), false) => {
            s.deps.exec.submit(Batch { verb: ActVerb::Reopen, targets, guard, edges: Vec::new() });
            true
        }
        _ => false,
    }
}

/// The next deadline of each timer: a busy note to show, a note or a ghost to end, a busy batch to give up on
/// (TIMER_NOTES), a close check (TIMER_CLOSE_CHECK). No timer exists while nothing waits.
fn sync_timers(cx: &mut EventCx, st: &mut ActionState, now: u64) {
    let mut notes: Option<u64> = None;
    let mut close: Option<u64> = None;
    let at = |slot: &mut Option<u64>, d: u64| {
        if d > now {
            *slot = Some(slot.map_or(d, |x| x.min(d)));
        }
    };
    for s in st.states.values() {
        match s.phase {
            Phase::Busy { since, .. } => {
                at(&mut notes, since + BUSY_DELAY_MS);
                at(&mut notes, since + BUSY_MAX_MS);
            }
            Phase::Closing { since, at: check } => {
                at(&mut notes, since + BUSY_DELAY_MS);
                at(&mut close, check);
            }
            Phase::Ended { until, .. } | Phase::Note { until, .. } => at(&mut notes, until),
            Phase::StillOpen => {}
        }
    }
    let after = |d: u64| d.saturating_sub(now).clamp(1, u64::from(u32::MAX)) as u32;
    for (id, want, armed) in
        [(TIMER_NOTES, notes, &mut st.notes_armed), (TIMER_CLOSE_CHECK, close, &mut st.close_armed)]
    {
        match want {
            Some(d) if *armed != Some(d) => {
                cx.set_timer(id, after(d));
                *armed = Some(d);
            }
            None if armed.take().is_some() => cx.cancel_timer(id),
            _ => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use oled_sys::pipe::Integrity::{High, Low, Medium, System, Untrusted};
    use oled_ui::harness::Harness;
    use oled_ui::PointL;

    use super::*;
    use crate::app::args::PreviewLang;
    use crate::app::exec::RecordingExecutor;
    use crate::app::launch::RecordingLauncher;
    use crate::ui::scene::{self, Scene, SceneWindow};
    use crate::ui::screen::InjectCommand;

    const ORBIT_EXE: &str = "C:\\Program Files\\Orbit\\orbit.exe";

    /// A key of the T3c scene.
    fn key(pid: u32) -> ProcKey {
        ProcKey { pid, created: 133_500_000_000_000_000 + u64::from(pid) * 1000 }
    }

    fn at(level: Option<Integrity>) -> Standing {
        Standing { level, package: None }
    }

    /// Reopen starts only from a Tasks at medium: what a group's processes named when none of them runs below it,
    /// else only the package of the root's token.
    #[test]
    fn reopen_trusts_only_processes_at_our_level() {
        let named = || Some(ORBIT_EXE.to_string());
        // At our level or above: the launch as the identity gave it.
        assert_eq!(reopen_launch(Some(Medium), &[at(Some(Medium))], named()), named());
        assert_eq!(reopen_launch(Some(Medium), &[at(Some(Medium)), at(Some(High))], named()), named());
        // A root or a window owner below us, or one we could not read: never what it named.
        let below: [&[Standing]; 4] =
            [&[at(Some(Low))], &[at(Some(Untrusted))], &[at(None)], &[at(Some(Medium)), at(Some(Low))]];
        for sources in below {
            assert_eq!(reopen_launch(Some(Medium), sources, named()), None, "{sources:?}");
        }
        // An elevated Tasks starts nothing, even what elevated programs named: it would resolve the string at its own
        // level through what a medium process may write (the file at an image path, a per-user shortcut of an AUMID).
        for own in [High, System] {
            assert_eq!(reopen_launch(Some(own), &[at(Some(own))], named()), None, "{own:?}");
            assert_eq!(reopen_launch(Some(own), &[at(Some(High)), at(Some(System))], named()), None, "{own:?}");
        }
        // Nor what a medium app named, nor the package of a token.
        let packaged = |level| Standing { level: Some(level), package: Some("Orbit.Pkg_x!App".to_string()) };
        assert_eq!(reopen_launch(Some(High), &[packaged(Medium)], named()), None);
        assert_eq!(reopen_launch(Some(High), &[packaged(Low)], named()), None);
        // Below us: the package of the root's token (an app container), whatever its windows named.
        let pkg = Some("shell:AppsFolder\\Orbit.Pkg_x!App".to_string());
        let other = Some("shell:AppsFolder\\Other.Pkg_y!App".to_string());
        assert_eq!(reopen_launch(Some(Medium), &[packaged(Low)], other.clone()), pkg);
        assert_eq!(reopen_launch(Some(Medium), &[packaged(Low)], None), pkg);
        // Only the root's: the package of a lower window owner is not the root's app.
        assert_eq!(reopen_launch(Some(Medium), &[at(Some(Medium)), packaged(Low)], other), None);
        // An empty package, an unknown own level, no root: nothing.
        let empty = Standing { level: Some(Low), package: Some(String::new()) };
        assert_eq!(reopen_launch(Some(Medium), &[empty], named()), None);
        assert_eq!(reopen_launch(None, &[at(Some(System))], named()), None);
        assert_eq!(reopen_launch(Some(Medium), &[], named()), None);
    }

    /// The T3c scene, edited: Orbit is pid 2000 (its window) with 2100, 2110 and 2200 (no window) below it.
    fn rig(edit: impl FnOnce(&mut Scene)) -> (Harness<TasksScreen>, Arc<RecordingExecutor>) {
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("scenes").join("t3c_actions.json");
        let mut s = Scene::load(&path).unwrap_or_else(|e| panic!("{e}"));
        edit(&mut s);
        let exec = Arc::new(RecordingExecutor::default());
        let h = scene::open(&s, PreviewLang::En, 96, Arc::new(RecordingLauncher::default()), exec.clone())
            .unwrap_or_else(|e| panic!("{e}"));
        (h, exec)
    }

    /// Ends Orbit now (Shift+Del) and answers Done: the launch string the End batch carried, and whether the row then
    /// offers Reopen.
    fn end_orbit(h: &mut Harness<TasksScreen>, exec: &RecordingExecutor) -> (Option<String>, bool) {
        let (row, row_key) = {
            let g = h.screen().parts().grouped.grouping();
            let grp = g.groups.iter().find(|x| x.members.first() == Some(&key(2000))).expect("Orbit's group");
            let row_key = group_key(grp.key);
            (h.screen().table().model().row_of(row_key).expect("its row"), row_key)
        };
        let r = h.screen().table().row_rect(row).expect("on screen");
        h.click(PointL { x: (r.l + r.r) / 2, y: (r.t + r.b) / 2 });
        h.key("Shift+Del");
        let launch = exec.batches.lock().unwrap().last().expect("the End batch").targets[0].launch.clone();
        h.send(Box::new(ActionDone { row: row_key, outcome: Outcome::Done, refused: 0 }));
        let phase = h.screen().parts().actions.states.get(&row_key).map(|s| s.phase.clone());
        (launch, matches!(phase, Some(Phase::Ended { reopen: true, .. })))
    }

    /// The standings are read as End goes out, before the batch: a group whose root, or a member owning one of its
    /// windows, runs below Tasks, and any group an elevated Tasks ends, ends with no launch string and offers no
    /// Reopen, and a Reopen then starts nothing.
    #[test]
    fn a_lower_group_ends_without_a_launch_string() {
        // At our level: Orbit's image path, as before.
        let (mut h, exec) = rig(|_| {});
        assert_eq!(end_orbit(&mut h, &exec), (Some(ORBIT_EXE.to_string()), true));
        // Its root below Tasks.
        let (mut h, exec) = rig(|_| {});
        h.screen_mut().p.actions.set_fixture_below(vec![(key(2000), None)]);
        assert_eq!(end_orbit(&mut h, &exec), (None, false));
        h.send(Box::new(InjectCommand(REOPEN)));
        assert!(exec.batches.lock().unwrap().iter().all(|b| b.verb != ActVerb::Reopen), "nothing to start");
        // Its root an app container: the package of its token.
        let (mut h, exec) = rig(|_| {});
        h.screen_mut().p.actions.set_fixture_below(vec![(key(2000), Some("Orbit.Pkg_x!App".to_string()))]);
        assert_eq!(end_orbit(&mut h, &exec), (Some("shell:AppsFolder\\Orbit.Pkg_x!App".to_string()), true));
        // A member below Tasks that owns a window of the group takes it away too.
        let (mut h, exec) =
            rig(|s| s.windows.push(SceneWindow { pid: 2100, identity: "orbit-id".into(), hung: false }));
        h.screen_mut().p.actions.set_fixture_below(vec![(key(2100), None)]);
        assert_eq!(end_orbit(&mut h, &exec), (None, false));
        // One with no window (a sandboxed helper) named nothing: Reopen stays.
        let (mut h, exec) = rig(|_| {});
        h.screen_mut().p.actions.set_fixture_below(vec![(key(2110), None)]);
        assert_eq!(end_orbit(&mut h, &exec), (Some(ORBIT_EXE.to_string()), true));
        // An elevated Tasks ending an elevated Orbit: no launch string, no Reopen.
        let (mut h, exec) = rig(|_| {});
        h.screen_mut().p.actions.set_fixture_own(Some(High));
        assert_eq!(end_orbit(&mut h, &exec), (None, false));
    }
}
