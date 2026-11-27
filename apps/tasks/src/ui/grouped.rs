//! Grouped mode: the Apps, Background and Part of Windows sections, groups and their members, hot rows that escape,
//! the header's view-mode menu and the view commands. Filled by TA-T3a (T3.1).

use std::cell::Cell;
use std::collections::{HashMap, HashSet};

use oled_shellmodel::identity::key_from_exe_path;
use oled_sysmon::heat::HeatStep;
use oled_sysmon::{ProcKey, ProcTable};
use oled_ui::model::{KeySet, ListModel, RowKey, RowOut, ScrollTo, Selection, TableOut};
use oled_ui::{Anchor, CommandId, EventCx, MenuItem, PointP};
use windows::Win32::System::RemoteDesktop::ProcessIdToSessionId;
use windows::Win32::System::Threading::GetCurrentProcessId;
use windows::Win32::UI::WindowsAndMessaging::{GetShellWindow, GetWindowThreadProcessId};

use super::commands::{VIEW_GROUPED, VIEW_LIST, VIEW_TREE};
use super::list;
use super::rows::{group_key, hardware_key, proc_key, section_key, Decor, RowEntry, RowKind, RowVals};
use super::screen::TasksScreen;
use super::view::View;
use crate::app::resolver::NameBook;
use crate::app::state::{self, StateHandle};
use crate::engine::frame::ServiceMap;
use crate::model::classify::ImagePath;
use crate::model::group::{group, totals, AppKey, AppWindow, Group, GroupInput, Grouping, IdentityId};
use crate::model::view::{Section, ViewMode};
use crate::strings::{k, t};

/// The most rows a collapsed section shows under its header ("hot rows escape").
pub const MAX_ESCAPED: usize = 3;

/// What the groups are built against: the shell process, our own process and our session. The window reads it from
/// the system at every build; scenes and tests fix it.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct HostEnv {
    pub shell_pid: Option<u32>,
    pub own_pid: u32,
    pub session: u32,
}

impl HostEnv {
    /// The process of the shell window, this process and its session.
    pub fn live() -> HostEnv {
        let mut shell = 0u32;
        // SAFETY: GetShellWindow takes nothing; the pid is written to a local that outlives the call.
        unsafe {
            let w = GetShellWindow();
            if !w.0.is_null() {
                GetWindowThreadProcessId(w, Some(&mut shell));
            }
        }
        // SAFETY: GetCurrentProcessId takes no arguments and cannot fail.
        let own_pid = unsafe { GetCurrentProcessId() };
        let mut session = 0u32;
        // SAFETY: a local u32 to write.
        let _ = unsafe { ProcessIdToSessionId(own_pid, &mut session) };
        HostEnv { shell_pid: (shell != 0).then_some(shell), own_pid, session }
    }
}

/// What a row key toggles.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Toggle {
    Section(Section),
    Group,
}

/// Expanded groups, the grouping of the last build and the scratch of the build.
#[derive(Default)]
pub struct GroupedState {
    grouping: Grouping,
    windows: Vec<AppWindow>,
    hosts: Vec<ProcKey>,
    /// Group rows the user opened (in memory, for the session).
    expanded: HashSet<RowKey>,
    /// The rows of the last build that open and close.
    toggles: HashMap<RowKey, Toggle>,
    /// Fixed by scenes and tests; None: the system's.
    env: Option<HostEnv>,
    /// A row that escaped the collapsed Part of Windows is a service host: its names need the service list.
    escaped_svchost: bool,
    /// A search stands: every section and group is built open (the search filter keeps what matches).
    query_open: bool,
}

impl GroupedState {
    /// While a search stands every section and group is built open, so every process is a row (T5.2).
    pub fn set_query_open(&mut self, on: bool) {
        self.query_open = on;
    }

    /// Fixes the shell, own process and session the groups are built against (scenes and tests).
    pub fn set_env(&mut self, e: HostEnv) {
        self.env = Some(e);
    }

    /// The groups of the last build.
    pub fn grouping(&self) -> &Grouping {
        &self.grouping
    }

    /// Whether the group row of this key is open.
    pub fn is_expanded(&self, key: RowKey) -> bool {
        self.expanded.contains(&key)
    }
}

/// The mode the window opens in (the remembered one). Part of Windows opens collapsed: the state is told so.
pub fn initial_mode(state: &StateHandle) -> ViewMode {
    if state.collapsed(Section::Windows).is_none() {
        state.set_collapsed(Section::Windows, true);
    }
    state.view().unwrap_or(ViewMode::Grouped)
}

/// Whether a section is collapsed: what the state remembers, else only Part of Windows.
fn collapsed(v: &View, s: Section) -> bool {
    v.state().and_then(|st| st.collapsed(s)).unwrap_or(s == Section::Windows)
}

fn blank(key: RowKey, kind: RowKind) -> RowEntry {
    RowEntry {
        key,
        kind,
        section: None,
        parent: None,
        depth: 0,
        vals: RowVals::default(),
        out: RowOut::default(),
        motion: Cell::new((0, 255)),
        expandable: false,
        expanded: false,
    }
}

/// What a group shows: the sums of its members, "—" until one of them has a rate.
fn group_vals(v: &View, procs: &ProcTable, g: &Group) -> RowVals {
    let t = totals(g, procs, v.rates());
    let any = g.members.iter().any(|k| v.rates().get(*k).is_some_and(|r| r.primed));
    RowVals {
        cpu: any.then_some(t.cpu),
        mem: Some(t.mem),
        io: any.then_some(t.io_bps),
        pid: None,
        heat: [HeatStep::S0; 3],
    }
}

fn is_idle(g: &Group) -> bool {
    matches!(g.key, AppKey::Single(k) if k.pid == 0)
}

fn image_of(v: &View, k: ProcKey) -> String {
    let mut s = String::new();
    v.proc_name(k, &mut s);
    s
}

/// The name of a group: the one the resolver gave its program, else the image name of its oldest process.
fn group_head(v: &View, names: &NameBook, procs: &ProcTable, g: &Group) -> String {
    let first = g.members.first().copied();
    let exe_of = |k: ProcKey| procs.index_of(k).and_then(|i| v.info_rows().get(i)).and_then(|i| i.exe);
    let known = match g.key {
        AppKey::App(id) => names
            .name_of_identity(id, &v.interned(id.0))
            .or_else(|| first.and_then(exe_of).and_then(|e| names.name_of_exe(e))),
        AppKey::Exe(id) => names.name_of_exe(id),
        AppKey::Image(_) | AppKey::Single(_) => None,
    };
    match (known, first) {
        (Some(n), _) => n.to_string(),
        (None, Some(k)) => image_of(v, k),
        (None, None) => String::new(),
    }
}

/// The label of a service host: "Service host: <first service by display name>" and "+n" for the others.
fn service_host(v: &View, services: &ServiceMap, pid: u32) -> Option<Decor> {
    let list = services.for_pid(pid);
    let first = list.iter().min_by_key(|e| e.display.to_lowercase())?;
    let lang = v.lang();
    let head = oled_sys::i18n::subst(&t(lang, k::TASKS_ROW_SERVICE_HOST), &[("service", &first.display)]);
    let tail = if list.len() > 1 {
        format!(" {}", oled_sys::i18n::subst(&t(lang, k::TASKS_ROW_MORE), &[("n", &(list.len() - 1).to_string())]))
    } else {
        String::new()
    };
    Some(Decor { head, tail, hung: false })
}

fn is_service_host(v: &View, k: ProcKey) -> bool {
    v.procs().find(k).is_some_and(|r| {
        let mut n = String::new();
        v.proc_name(r.key, &mut n);
        n.eq_ignore_ascii_case("svchost.exe")
    })
}

/// The rows of Grouped mode: per section its row, then (open) its groups and their members, or (collapsed) the
/// groups that run hot. Rows carry their sort scope: a group's parent is its section, a member's its group.
pub fn build(v: &View, names: &NameBook, st: &mut GroupedState, rows: &mut Vec<RowEntry>) {
    v.clear_decor();
    st.toggles.clear();
    st.escaped_svchost = false;
    let frame = v.frame();
    let procs = &frame.procs;
    let env = st.env.unwrap_or_else(HostEnv::live);

    // The window of every app: the resolver's identity of it, until it answers the exe's key.
    st.windows.clear();
    for w in &frame.windows {
        let key = match names.identity(w.hwnd, w.owner.pid) {
            Some(i) => i.key.clone(),
            None => match frame.paths.get(w.owner) {
                Some(ImagePath::Path(p)) => key_from_exe_path(p),
                _ => continue,
            },
        };
        st.windows.push(AppWindow { owner: w.owner, identity: IdentityId(v.intern(&key)), hung: w.hung });
    }

    // The hosts never absorb children: the shell, the dock family (dock.exe beside tasks.exe) and this process.
    st.hosts.clear();
    st.hosts.extend(env.shell_pid.and_then(|p| procs.find_pid(p)).map(|r| r.key));
    st.hosts.extend(procs.find_pid(env.own_pid).map(|r| r.key));
    if let Some(dir) = v.install_dir() {
        let dock = dir.join("dock.exe").to_string_lossy().replace('/', "\\");
        for (key, path) in &frame.paths.entries {
            if matches!(path, ImagePath::Path(p) if p.eq_ignore_ascii_case(&dock)) && procs.index_of(*key).is_some() {
                st.hosts.push(*key);
            }
        }
    }
    group(
        &GroupInput { procs, info: v.info_rows(), windows: &st.windows, session: env.session, hosts: &st.hosts },
        &mut st.grouping,
    );

    let mut hot_escaped: Vec<(f32, RowEntry)> = Vec::new();
    for section in [Section::Apps, Section::Background, Section::Windows] {
        let all = &st.grouping.groups;
        let lo = all.partition_point(|g| g.section < section);
        let hi = all.partition_point(|g| g.section <= section);
        let groups = &all[lo..hi];
        let closed = !st.query_open && collapsed(v, section);
        let skey = section_key(section);

        // The section row: how many groups, the sums of their processes.
        let mut sec = blank(skey, RowKind::Section(section));
        sec.section = Some(section);
        sec.out.section = true;
        sec.expandable = true;
        sec.expanded = !closed;
        let (mut cpu, mut mem, mut io, mut any, mut n) = (0.0f32, 0u64, 0.0f32, false, 0usize);
        for g in groups.iter().filter(|g| !is_idle(g)) {
            let gv = group_vals(v, procs, g);
            n += 1;
            mem = mem.saturating_add(gv.mem.unwrap_or(0));
            if let (Some(c), Some(i)) = (gv.cpu, gv.io) {
                cpu += c;
                io += i;
                any = true;
            }
        }
        sec.vals = RowVals { cpu: any.then_some(cpu), mem: Some(mem), io: any.then_some(io), ..RowVals::default() };
        v.set_decor(skey, Decor { head: String::new(), tail: format!(" ({n})"), hung: false });
        st.toggles.insert(skey, Toggle::Section(section));
        rows.push(sec);

        if section == Section::Windows && !closed {
            let mut hw = blank(hardware_key(), RowKind::Hardware);
            hw.section = Some(section);
            // It sorts under its section (an anchor there: the first row under Part of Windows).
            hw.parent = Some(skey);
            // No actions and no target: the later groups (actions, selection) read this flag.
            hw.out.disabled = true;
            hw.vals.cpu = Some(v.dpc_irq_pct());
            rows.push(hw);
        }

        hot_escaped.clear();
        let mut built: Vec<(RowEntry, Vec<RowEntry>, String)> = Vec::with_capacity(groups.len());
        for g in groups {
            let mut members: Vec<RowEntry> = Vec::new();
            let parts = st_parts(&st.expanded, &mut st.toggles, st.query_open);
            let Some((top, name)) = group_row(v, names, parts, g, skey, &mut members) else {
                continue;
            };
            if closed {
                if top.vals.heat.iter().any(|h| *h >= HeatStep::S3) {
                    hot_escaped.push((top.vals.cpu.unwrap_or(0.0), top));
                }
            } else {
                built.push((top, members, name));
            }
        }
        let sort = v.sort();
        built.sort_by(|a, b| list::cmp_by_sort(sort, &a.0, &b.0, Some(&a.2), Some(&b.2)).then_with(|| a.2.cmp(&b.2)));
        for (top, mut members, _) in built {
            rows.push(top);
            rows.append(&mut members);
        }
        // A collapsed section keeps at most MAX_ESCAPED of its hot rows, the busiest processors first.
        hot_escaped.sort_by(|a, b| b.0.partial_cmp(&a.0).unwrap_or(std::cmp::Ordering::Equal));
        hot_escaped.truncate(MAX_ESCAPED);
        for (_, e) in hot_escaped.drain(..) {
            if section == Section::Windows {
                if let RowKind::Proc(k) = e.kind {
                    st.escaped_svchost |= is_service_host(v, k);
                }
            }
            rows.push(e);
        }
    }
}

/// The parts of the state a group row writes to.
struct Parts<'a> {
    expanded: &'a HashSet<RowKey>,
    /// A search stands: every group is open.
    all_open: bool,
    toggles: &'a mut HashMap<RowKey, Toggle>,
}

fn st_parts<'a>(expanded: &'a HashSet<RowKey>, toggles: &'a mut HashMap<RowKey, Toggle>, all_open: bool) -> Parts<'a> {
    Parts { expanded, all_open, toggles }
}

/// The row of one group (and, when it is open, its members in `members`). A group of one process of Part of Windows
/// is that process's own row.
fn group_row(
    v: &View,
    names: &NameBook,
    parts: Parts<'_>,
    g: &Group,
    parent: RowKey,
    members: &mut Vec<RowEntry>,
) -> Option<(RowEntry, String)> {
    let procs = &v.frame().procs;
    if let (AppKey::Single(k), 1) = (g.key, g.members.len()) {
        let p = procs.find(k)?;
        let mut e = v.proc_entry(p);
        e.section = Some(g.section);
        e.parent = Some(parent);
        if is_service_host(v, k) {
            if let Some(d) = service_host(v, &v.frame().services, k.pid) {
                v.set_decor(e.key, d);
            }
        }
        let name = image_of(v, k).to_lowercase();
        return Some((e, name));
    }
    let key = group_key(g.key);
    let many = g.members.len() > 1;
    let open = many && (parts.all_open || parts.expanded.contains(&key));
    let mut e = blank(key, RowKind::Group(g.key));
    e.section = Some(g.section);
    e.parent = Some(parent);
    e.vals = group_vals(v, procs, g);
    v.fill_heat(key, &mut e.vals);
    e.expandable = many;
    e.expanded = open;
    let head = group_head(v, names, procs, g);
    let name = head.to_lowercase();
    v.set_decor(
        key,
        Decor {
            head,
            tail: if many { format!(" ({})", g.members.len()) } else { String::new() },
            hung: g.hung_windows > 0,
        },
    );
    if many {
        parts.toggles.insert(key, Toggle::Group);
    }
    if open {
        let mut kids: Vec<(RowEntry, String)> = Vec::with_capacity(g.members.len());
        for k in &g.members {
            let Some(p) = procs.find(*k) else { continue };
            let mut m = v.proc_entry(p);
            m.section = Some(g.section);
            m.parent = Some(key);
            m.depth = 1;
            v.set_decor(m.key, Decor { head: String::new(), tail: format!("{}{}", list::SEP, k.pid), hung: false });
            kids.push((m, image_of(v, *k).to_lowercase()));
        }
        sort_scope(v, &mut kids);
        members.extend(kids.into_iter().map(|(m, _)| m));
    }
    Some((e, name))
}

/// The rows of one scope (the groups of a section, the members of a group) in the order of the sort, ties by name.
/// order::apply keeps what it finds; the build does not leave an open group's members between its siblings.
fn sort_scope<T>(v: &View, scope: &mut [(RowEntry, T)])
where
    T: AsRef<str>,
{
    let sort = v.sort();
    scope.sort_by(|a, b| {
        list::cmp_by_sort(sort, &a.0, &b.0, Some(a.1.as_ref()), Some(b.1.as_ref()))
            .then_with(|| a.1.as_ref().cmp(b.1.as_ref()))
    });
}

fn chord_of(id: CommandId, lang: oled_ui::UiLang) -> Option<oled_ui::node::Text> {
    let table = super::commands::table(lang).ok()?;
    let ch = table.get(id)?.chord?;
    Some(std::borrow::Cow::Owned(ch.label(lang)))
}

/// The header's right click: Grouped, Tree, List, the current one checked.
pub fn header_menu(cx: &mut EventCx, at: PointP, mode: ViewMode) {
    let lang = cx.lang();
    let item = |id, key, m: ViewMode| MenuItem::Custom {
        id,
        label: t(lang, key),
        glyph: None,
        chord: chord_of(id, lang),
        enabled: true,
        checked: Some(mode == m),
        note: None,
        danger: false,
    };
    let items = vec![
        item(VIEW_GROUPED, k::TASKS_VIEW_GROUPED, ViewMode::Grouped),
        item(VIEW_TREE, k::TASKS_VIEW_TREE, ViewMode::Tree),
        item(VIEW_LIST, k::TASKS_VIEW_LIST, ViewMode::List),
    ];
    cx.open_menu(items, Anchor::Point(at));
}

/// ExpandRequested and Activated on a section or group row (Tree's nodes answer ExpandRequested too); true when the
/// rows change (the pipeline runs).
pub fn on_table_out(cx: &mut EventCx, st: &mut GroupedState, out: &TableOut, state: &StateHandle) -> bool {
    let (key, want) = match out {
        TableOut::ExpandRequested(k, open) => (*k, Some(*open)),
        TableOut::Activated(k) => (*k, None),
        _ => return false,
    };
    match st.toggles.get(&key).copied() {
        Some(Toggle::Section(s)) => {
            let now = state.collapsed(s).unwrap_or(s == Section::Windows);
            let to = want.map_or(!now, |open| !open);
            if to == now {
                return false;
            }
            state.set_collapsed(s, to);
            state::note_change(cx, state);
            true
        }
        Some(Toggle::Group) => {
            let now = st.expanded.contains(&key);
            let to = want.unwrap_or(!now);
            if to == now {
                return false;
            }
            if to {
                st.expanded.insert(key);
            } else {
                st.expanded.remove(&key);
            }
            true
        }
        None => match out {
            TableOut::ExpandRequested(k, open) => {
                state.set_tree_collapsed(k.0, !*open);
                true
            }
            _ => false,
        },
    }
}

/// Part of Windows is open, or a row that escaped it is a service host: its names need the service list.
pub fn windows_rows_visible(st: &GroupedState, v: &View) -> bool {
    v.mode() == ViewMode::Grouped && (!collapsed(v, Section::Windows) || st.escaped_svchost)
}

/// VIEW_GROUPED, VIEW_TREE, VIEW_LIST: the mode is switched, stored, and the selection moves with it (a group to its
/// oldest process, a process to its group).
pub fn on_command(s: &mut TasksScreen, cx: &mut EventCx, id: CommandId) {
    let to = match id {
        VIEW_GROUPED => ViewMode::Grouped,
        VIEW_TREE => ViewMode::Tree,
        VIEW_LIST => ViewMode::List,
        _ => return,
    };
    if s.v.mode() == to {
        return;
    }
    let mut picked: Vec<RowKind> = Vec::new();
    {
        let model = s.table.model();
        s.table.selection().for_each(model, &mut |i, _| {
            if let Some(r) = model.rows.get(i) {
                picked.push(r.kind);
            }
        });
    }
    let focused = s
        .table
        .focused()
        .and_then(|k| s.table.model().row_of(k))
        .and_then(|i| s.table.model().rows.get(i).map(|r| r.kind));
    s.v.set_mode(to);
    s.deps.state.set_view(to);
    state::note_change(cx, &s.deps.state);
    let now = cx.now_ms();
    s.run_pipeline(cx, now);
    let target = |kind: RowKind| -> Option<RowKey> {
        let g = &s.p.grouped.grouping;
        match kind {
            RowKind::Group(ak) => {
                let first = g.groups.iter().find(|x| x.key == ak)?.members.first().copied()?;
                Some(proc_key(first))
            }
            RowKind::Proc(pk) | RowKind::Ghost(pk) => {
                if to != ViewMode::Grouped {
                    return Some(proc_key(pk));
                }
                let i = s.v.procs().index_of(pk)?;
                let grp = g.groups.get(*g.member_of.get(i)? as usize)?;
                match grp.key {
                    AppKey::Single(_) if grp.members.len() == 1 => Some(proc_key(pk)),
                    ak => Some(group_key(ak)),
                }
            }
            RowKind::Section(_) | RowKind::Hardware => None,
        }
    };
    let model = s.table.model();
    let mut keys: Vec<RowKey> = picked.into_iter().filter_map(target).filter(|k| model.row_of(*k).is_some()).collect();
    keys.dedup();
    let focus = focused.and_then(target).filter(|k| model.row_of(*k).is_some()).or_else(|| keys.first().copied());
    let selection = match keys.as_slice() {
        [] => Selection::None,
        [one] => Selection::Single(*one),
        many => {
            let mut set = KeySet::new();
            for k in many {
                set.insert(*k);
            }
            Selection::Keys(set)
        }
    };
    s.table.set_selection(cx, selection);
    if let Some(k) = focus {
        s.table.set_focused(cx, k, ScrollTo::Nearest);
    }
    // The rows kept by their kinds may lie in several sections of the new view.
    super::selection::keep_one_section(cx, &mut s.table);
}
