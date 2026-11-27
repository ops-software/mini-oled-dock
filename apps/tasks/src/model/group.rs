//! Grouping of processes into Apps, Background and Part of Windows.
use crate::model::classify::PathClass;
use crate::model::rates::RateBook;
use crate::model::verdict::GroupStat;
use crate::model::view::Section;
use oled_sysmon::{ProcKey, ProcTable};
use std::collections::HashMap;

/// Window AUMID, package AUMID or exe key.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct IdentityId(pub u32);
/// Lower-cased full image path.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct ExeId(pub u32);
/// Lower-cased image file name.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct ImageId(pub u32);

/// The consumer's one interner for identities, lower-cased exe paths and lower-cased image names. Append-only:
/// an id stays valid and equal for the interner's life, whatever table or tick the string came from.
#[derive(Debug, Default)]
pub struct Interner {
    map: HashMap<Box<str>, u32>,
    items: Vec<Box<str>>,
}

impl Interner {
    pub fn new() -> Interner {
        Interner::default()
    }
    pub fn intern(&mut self, s: &str) -> u32 {
        if let Some(&id) = self.map.get(s) {
            return id;
        }
        let id = self.items.len() as u32;
        let boxed: Box<str> = s.into();
        self.items.push(boxed.clone());
        self.map.insert(boxed, id);
        id
    }
    pub fn get(&self, id: u32) -> &str {
        self.items.get(id as usize).map(|b| &**b).unwrap_or("")
    }
    pub fn len(&self) -> usize {
        self.items.len()
    }
    pub fn is_empty(&self) -> bool {
        self.items.is_empty()
    }
}

/// The selection key of Grouped mode; stable across ticks.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum AppKey {
    App(IdentityId),
    Exe(ExeId),
    Image(ImageId),
    Single(ProcKey),
}

/// One per ProcTable row, same order (InfoBook).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ProcInfo {
    pub key: ProcKey,
    pub exe: Option<ExeId>,
    pub image: ImageId,
    pub class: PathClass,
}

/// Windows that passed the filter, Z order.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct AppWindow {
    pub owner: ProcKey,
    pub identity: IdentityId,
    pub hung: bool,
}

/// hosts never absorb children: the window app passes the shell process, dock.exe and tasks.exe; the trace
/// recorder passes itself and its whole ancestor chain (tree::ancestors).
pub struct GroupInput<'a> {
    pub procs: &'a ProcTable,
    pub info: &'a [ProcInfo],
    pub windows: &'a [AppWindow],
    pub session: u32,
    pub hosts: &'a [ProcKey],
}

#[derive(Clone, Debug, PartialEq)]
pub struct Group {
    pub key: AppKey,
    pub section: Section,
    pub members: Vec<ProcKey>,
    pub windows: u32,
    pub hung_windows: u32,
}

#[derive(Clone, Debug, Default)]
pub struct Grouping {
    pub groups: Vec<Group>,
    /// member_of[row of procs] = index into groups.
    pub member_of: Vec<u32>,
}

/// Longest parent walk when a process looks for an app to join.
const MAX_ABSORB_STEPS: usize = 64;

/// The identity a windowless process joins: the first ancestor with an app identity, walking through non-app
/// ancestors and stopping at a host. A host itself never joins anything.
fn absorbed(procs: &ProcTable, own: &[Option<IdentityId>], hosts: &[ProcKey], row: usize) -> Option<IdentityId> {
    if hosts.contains(&procs.rows[row].key) {
        return None;
    }
    let mut cur = procs.rows[row].parent;
    for _ in 0..MAX_ABSORB_STEPS {
        let key = cur?;
        if hosts.contains(&key) {
            return None;
        }
        let j = procs.index_of(key)?;
        if let Some(id) = own[j] {
            return Some(id);
        }
        cur = procs.rows[j].parent;
    }
    None
}

pub fn group(input: &GroupInput, out: &mut Grouping) {
    let procs = input.procs;
    let n = procs.rows.len();
    out.groups.clear();
    out.member_of.clear();
    out.member_of.resize(n, 0);
    let mut own: Vec<Option<IdentityId>> = vec![None; n];
    let mut wins: Vec<(u32, u32)> = vec![(0, 0); n];
    for w in input.windows {
        if let Some(i) = procs.index_of(w.owner) {
            // Several identities on one process: the first in Z order wins.
            own[i].get_or_insert(w.identity);
            wins[i].0 += 1;
            wins[i].1 += w.hung as u32;
        }
    }
    // (section, key, created, pid, row): one sort gives the group order and, inside a group, the member order.
    let mut order: Vec<(Section, AppKey, u64, u32, u32)> = Vec::with_capacity(n);
    for (i, row) in procs.rows.iter().enumerate() {
        let identity = own[i].or_else(|| absorbed(procs, &own, input.hosts, i));
        let (section, key) = match identity {
            Some(id) => (Section::Apps, AppKey::App(id)),
            None => {
                let info = input.info.get(i);
                let class = info.map_or(PathClass::Unknown, |x| x.class);
                let other_session = row.session != 0 && row.session != input.session;
                if class != PathClass::NoImage && other_session {
                    // Another user's session: Background, whatever the path.
                    match info {
                        Some(ProcInfo { exe: Some(e), .. }) => (Section::Background, AppKey::Exe(*e)),
                        Some(x) => (Section::Background, AppKey::Image(x.image)),
                        None => (Section::Background, AppKey::Single(row.key)),
                    }
                } else if matches!(class, PathClass::SystemRoot | PathClass::NoImage) || row.session == 0 {
                    (Section::Windows, AppKey::Single(row.key))
                } else {
                    match info {
                        Some(ProcInfo { exe: Some(e), .. }) => (Section::Background, AppKey::Exe(*e)),
                        Some(x) => (Section::Background, AppKey::Image(x.image)),
                        None => (Section::Background, AppKey::Single(row.key)),
                    }
                }
            }
        };
        order.push((section, key, row.key.created, row.key.pid, i as u32));
    }
    order.sort_unstable();
    for &(section, key, _, _, row) in &order {
        let row = row as usize;
        if out.groups.last().is_none_or(|g| g.section != section || g.key != key) {
            out.groups.push(Group { key, section, members: Vec::new(), windows: 0, hung_windows: 0 });
        }
        let index = out.groups.len() - 1;
        let g = &mut out.groups[index];
        g.members.push(procs.rows[row].key);
        g.windows += wins[row].0;
        g.hung_windows += wins[row].1;
        out.member_of[row] = index as u32;
    }
}

/// cpu and io_bps sum the members' EMA values (Rates::cpu, Rates::io_bps: the value the rows show and sort by);
/// an unprimed member adds 0. group_stats copies them into GroupStat, so the verdict reads the same numbers the
/// rows show.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Totals {
    pub cpu: f32,
    pub mem: u64,
    pub io_bps: f32,
    pub private_bytes: u64,
    pub ws: u64,
    pub threads: u32,
    pub handles: u32,
    pub primed: bool,
}

pub fn totals(g: &Group, procs: &ProcTable, rates: &RateBook) -> Totals {
    let mut t = Totals::default();
    let mut all_primed = !g.members.is_empty();
    for key in &g.members {
        if let Some(row) = procs.find(*key) {
            t.mem = t.mem.saturating_add(row.ws_private);
            t.private_bytes = t.private_bytes.saturating_add(row.private_bytes);
            t.ws = t.ws.saturating_add(row.ws);
            t.threads = t.threads.saturating_add(row.threads);
            t.handles = t.handles.saturating_add(row.handles);
        }
        match rates.get(*key) {
            Some(r) if r.primed => {
                t.cpu += r.cpu;
                t.io_bps += r.io_bps;
            }
            _ => all_primed = false,
        }
    }
    t.primed = all_primed;
    t
}

pub fn group_stats(g: &Grouping, procs: &ProcTable, rates: &RateBook, out: &mut Vec<GroupStat>) {
    out.clear();
    for group in &g.groups {
        let t = totals(group, procs, rates);
        out.push(GroupStat {
            key: group.key,
            section: group.section,
            cpu: t.cpu,
            mem: t.mem,
            io_bps: t.io_bps,
            hung: group.hung_windows > 0,
            has_windows: group.windows > 0,
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::classify::{ImagePath, InfoBook, PathMap};
    use oled_sysmon::ProcRow;

    const ROOT: &str = "C:\\Windows";
    const SYS: &str = "c:\\windows\\system32\\";

    /// A process of a fixture: path "?" is pending, "-" no image, "!" unknown, anything else a path.
    struct P {
        pid: u32,
        ppid: u32,
        session: u32,
        name: &'static str,
        path: &'static str,
    }

    fn p(pid: u32, ppid: u32, session: u32, name: &'static str, path: &'static str) -> P {
        P { pid, ppid, session, name, path }
    }

    fn key(pid: u32) -> ProcKey {
        ProcKey { pid, created: pid as u64 * 10 }
    }

    fn table(specs: &[P], intern_first: &[&str], filler: usize) -> (ProcTable, PathMap) {
        let mut t = ProcTable::new();
        for n in intern_first {
            let u: Vec<u16> = n.encode_utf16().collect();
            t.names.intern(&u);
        }
        for i in 0..filler {
            let u: Vec<u16> = format!("filler{i}").encode_utf16().collect();
            t.names.intern(&u);
        }
        let mut map = PathMap::default();
        for s in specs {
            let u: Vec<u16> = s.name.encode_utf16().collect();
            let image = t.names.intern(&u);
            t.push(ProcRow { key: key(s.pid), parent_pid: s.ppid, session: s.session, image, ..ProcRow::default() });
            match s.path {
                "?" => {}
                "-" => map.entries.push((key(s.pid), ImagePath::NoImage)),
                "!" => map.entries.push((key(s.pid), ImagePath::Unknown)),
                other => map.entries.push((key(s.pid), ImagePath::Path(other.into()))),
            }
        }
        t.finish();
        map.entries.sort_by_key(|e| e.0);
        (t, map)
    }

    fn win(pid: u32, id: u32) -> AppWindow {
        AppWindow { owner: key(pid), identity: IdentityId(id), hung: false }
    }

    fn run(t: &ProcTable, map: &PathMap, windows: &[AppWindow], hosts: &[ProcKey]) -> (Grouping, Interner) {
        let mut names = Interner::new();
        let info = InfoBook::new().update(t, map, ROOT, &mut names).to_vec();
        let mut g = Grouping::default();
        group(&GroupInput { procs: t, info: &info, windows, session: 1, hosts }, &mut g);
        (g, names)
    }

    fn group_of(g: &Grouping, t: &ProcTable, pid: u32) -> Group {
        g.groups[g.member_of[t.index_of(key(pid)).unwrap()] as usize].clone()
    }

    fn chrome_specs() -> Vec<P> {
        let mut v = vec![p(10, 1, 1, "chrome.exe", "d:\\apps\\chrome.exe")];
        for pid in 11..=15 {
            v.push(p(pid, 10, 1, "chrome.exe", "d:\\apps\\chrome.exe"));
        }
        v.push(p(16, 11, 1, "crashpad.exe", "d:\\apps\\crashpad.exe"));
        v
    }

    #[test]
    fn interner_is_append_only_and_shares_ids() {
        let mut n = Interner::new();
        assert!(n.is_empty());
        let a = n.intern("a");
        let b = n.intern("b");
        assert_ne!(a, b);
        assert_eq!(n.intern("a"), a);
        assert_eq!((n.get(a), n.get(b), n.get(99)), ("a", "b", ""));
        assert_eq!(n.len(), 2);
    }

    #[test]
    fn chrome_tree_is_one_app() {
        let (t, map) = table(&chrome_specs(), &[], 0);
        let (g, _) = run(&t, &map, &[win(10, 1)], &[]);
        assert_eq!(g.groups.len(), 1);
        assert_eq!(g.groups[0].section, Section::Apps);
        assert_eq!(g.groups[0].key, AppKey::App(IdentityId(1)));
        assert_eq!(g.groups[0].members.len(), 7);
        assert_eq!(g.groups[0].members[0], key(10));
        assert_eq!(g.groups[0].windows, 1);
        assert_eq!(g.member_of, vec![0; 7]);
    }

    #[test]
    fn conhost_sits_inside_its_app() {
        let conhost = format!("{SYS}conhost.exe");
        let conhost: &'static str = Box::leak(conhost.into_boxed_str());
        let specs = [p(10, 1, 1, "app.exe", "d:\\a\\app.exe"), p(11, 10, 1, "conhost.exe", conhost)];
        let (t, map) = table(&specs, &[], 0);
        let (g, _) = run(&t, &map, &[win(10, 1)], &[]);
        assert_eq!(g.groups.len(), 1);
        assert_eq!(g.groups[0].members, vec![key(10), key(11)]);
    }

    #[test]
    fn steam_game_stays_its_own_app() {
        let specs = [p(10, 1, 1, "steam.exe", "d:\\s\\steam.exe"), p(11, 10, 1, "game.exe", "d:\\g\\game.exe")];
        let (t, map) = table(&specs, &[], 0);
        let (g, _) = run(&t, &map, &[win(10, 1), win(11, 2)], &[]);
        assert_eq!(g.groups.len(), 2);
        assert!(g.groups.iter().all(|x| x.section == Section::Apps && x.members.len() == 1));
    }

    #[test]
    fn first_identity_of_a_process_wins() {
        let specs = [p(10, 1, 1, "app.exe", "d:\\a\\app.exe")];
        let (t, map) = table(&specs, &[], 0);
        let hung = AppWindow { hung: true, ..win(10, 7) };
        let (g, _) = run(&t, &map, &[win(10, 5), hung], &[]);
        assert_eq!(g.groups.len(), 1);
        assert_eq!(g.groups[0].key, AppKey::App(IdentityId(5)));
        assert_eq!((g.groups[0].windows, g.groups[0].hung_windows), (2, 1));
    }

    #[test]
    fn explorer_does_not_absorb_startup_apps() {
        let specs =
            [p(5, 1, 1, "explorer.exe", "c:\\windows\\explorer.exe"), p(6, 5, 1, "runkey.exe", "d:\\x\\runkey.exe")];
        let (t, map) = table(&specs, &[], 0);
        let (g, names) = run(&t, &map, &[win(5, 1)], &[key(5)]);
        assert_eq!(g.groups.len(), 2);
        let child = group_of(&g, &t, 6);
        assert_eq!(child.section, Section::Background);
        match child.key {
            AppKey::Exe(e) => assert_eq!(names.get(e.0), "d:\\x\\runkey.exe"),
            other => panic!("{other:?}"),
        }
        assert_eq!(group_of(&g, &t, 5).section, Section::Apps);
        // Without the host guard the Run-key child would join Explorer's group.
        let (g, _) = run(&t, &map, &[win(5, 1)], &[]);
        assert_eq!(g.groups.len(), 1);
    }

    #[test]
    fn svchost_rows_are_single() {
        let sc = "c:\\windows\\system32\\services.exe";
        let sv = "c:\\windows\\system32\\svchost.exe";
        let specs = [
            p(3, 1, 0, "services.exe", sc),
            p(4, 3, 0, "svchost.exe", sv),
            p(5, 3, 0, "svchost.exe", sv),
            p(6, 3, 0, "svchost.exe", sv),
        ];
        let (t, map) = table(&specs, &[], 0);
        let (g, _) = run(&t, &map, &[], &[]);
        assert_eq!(g.groups.len(), 4);
        for x in &g.groups {
            assert_eq!(x.section, Section::Windows);
            assert!(matches!(x.key, AppKey::Single(_)));
            assert_eq!(x.members.len(), 1);
        }
    }

    #[test]
    fn session_zero_with_an_unknown_path_is_part_of_windows() {
        let specs = [p(3, 1, 0, "mystery.exe", "!"), p(4, 1, 0, "pending.exe", "?")];
        let (t, map) = table(&specs, &[], 0);
        let (g, _) = run(&t, &map, &[], &[]);
        assert!(g.groups.iter().all(|x| x.section == Section::Windows));
    }

    #[test]
    fn other_user_session_is_background() {
        let specs = [
            p(3, 1, 2, "csrss.exe", "c:\\windows\\system32\\csrss.exe"),
            p(4, 1, 1, "own.exe", "c:\\windows\\system32\\own.exe"),
            p(5, 1, 2, "none.exe", "-"),
        ];
        let (t, map) = table(&specs, &[], 0);
        let (g, _) = run(&t, &map, &[], &[]);
        assert_eq!(group_of(&g, &t, 3).section, Section::Background);
        assert_eq!(group_of(&g, &t, 4).section, Section::Windows);
        assert_eq!(group_of(&g, &t, 5).section, Section::Windows);
    }

    #[test]
    fn pid_zero_and_system_are_windows() {
        let specs = [p(0, 0, 0, "", "-"), p(4, 0, 0, "System", "-")];
        let mut t = ProcTable::new();
        let mut map = PathMap::default();
        for s in &specs {
            let u: Vec<u16> = s.name.encode_utf16().collect();
            let image = t.names.intern(&u);
            t.push(ProcRow {
                key: ProcKey { pid: s.pid, created: s.pid as u64 * 10 + 1 },
                parent_pid: s.ppid,
                session: s.session,
                image,
                ..ProcRow::default()
            });
            map.entries.push((ProcKey { pid: s.pid, created: s.pid as u64 * 10 + 1 }, ImagePath::NoImage));
        }
        t.finish();
        let (g, _) = run(&t, &map, &[], &[]);
        assert_eq!(g.groups.len(), 2);
        assert!(g.groups.iter().all(|x| x.section == Section::Windows));
    }

    #[test]
    fn unknown_path_groups_by_image() {
        let specs = [p(10, 1, 1, "Tool.exe", "?"), p(11, 1, 1, "tool.EXE", "!"), p(12, 1, 1, "other.exe", "?")];
        let (t, map) = table(&specs, &[], 0);
        let (g, names) = run(&t, &map, &[], &[]);
        assert_eq!(g.groups.len(), 2);
        let tool = group_of(&g, &t, 10);
        assert_eq!(tool.section, Section::Background);
        assert_eq!(tool.members, vec![key(10), key(11)]);
        match tool.key {
            AppKey::Image(i) => assert_eq!(names.get(i.0), "tool.exe"),
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn image_key_is_stable_across_tables() {
        let a = [p(10, 1, 1, "tool.exe", "?"), p(11, 1, 1, "aaa.exe", "?"), p(12, 1, 1, "bbb.exe", "?")];
        let b = [p(10, 1, 1, "bbb.exe", "?"), p(11, 1, 1, "Tool.EXE", "?"), p(12, 1, 1, "aaa.exe", "?")];
        let c = [p(10, 1, 1, "aaa.exe", "?"), p(11, 1, 1, "bbb.exe", "?"), p(12, 1, 1, "tool.exe", "?")];
        let ta = table(&a, &[], 0);
        let tb = table(&b, &["zzz.exe", "bbb.exe"], 0);
        let tc = table(&c, &[], oled_sysmon::table::NAME_COMPACT_AT + 100);
        assert!(tc.0.names.len() < 10, "the third table was compacted");
        let mut names = Interner::new();
        let mut keys = Vec::new();
        for ((t, map), tool_pid) in [(&ta, 10), (&tb, 11), (&tc, 12)] {
            let info = InfoBook::new().update(t, map, ROOT, &mut names).to_vec();
            let mut g = Grouping::default();
            group(&GroupInput { procs: t, info: &info, windows: &[], session: 1, hosts: &[] }, &mut g);
            assert_eq!(g.groups.len(), 3);
            keys.push(group_of(&g, t, tool_pid).key);
        }
        assert_eq!(keys[0], keys[1]);
        assert_eq!(keys[1], keys[2]);
        assert!(matches!(keys[0], AppKey::Image(_)));
    }

    fn testee_specs() -> [P; 4] {
        [
            p(10, 1, 1, "term.exe", "d:\\t\\term.exe"),
            p(11, 10, 1, "shell.exe", "d:\\t\\shell.exe"),
            p(12, 11, 1, "recorder.exe", "d:\\r\\recorder.exe"),
            p(13, 12, 1, "testee.exe", "d:\\r\\testee.exe"),
        ]
    }

    #[test]
    fn testee_under_a_terminal_absorbs_nothing_with_the_chain_as_hosts() {
        let (t, map) = table(&testee_specs(), &[], 0);
        let mut chain = Vec::new();
        crate::model::tree::ancestors(&t, key(12), &mut chain);
        assert_eq!(chain, vec![key(12), key(11), key(10)]);
        let (g, names) = run(&t, &map, &[win(10, 1)], &chain);
        let testee = group_of(&g, &t, 13);
        assert_eq!(testee.section, Section::Background);
        assert_eq!(testee.members, vec![key(13)]);
        match testee.key {
            AppKey::Exe(e) => assert_eq!(names.get(e.0), "d:\\r\\testee.exe"),
            other => panic!("{other:?}"),
        }
        // The recorder and the shell are hosts: they never join the terminal either.
        assert_eq!(group_of(&g, &t, 12).members, vec![key(12)]);
        assert_eq!(group_of(&g, &t, 11).members, vec![key(11)]);
        // Without hosts the whole chain is swallowed by the terminal's app, the failure the recorder avoids.
        let (g, _) = run(&t, &map, &[win(10, 1)], &[]);
        assert_eq!(g.groups.len(), 1);
        assert_eq!(group_of(&g, &t, 13).members.len(), 4);
    }

    #[test]
    fn pid_reuse_never_adopts() {
        // Pid 10 was created after pid 20 named it as parent: the link is not made, so pid 20 is a root.
        let mut t = ProcTable::new();
        t.push(ProcRow { key: ProcKey { pid: 10, created: 500 }, ..ProcRow::default() });
        t.push(ProcRow { key: ProcKey { pid: 20, created: 100 }, parent_pid: 10, session: 1, ..ProcRow::default() });
        t.finish();
        let map = PathMap {
            entries: vec![
                (ProcKey { pid: 10, created: 500 }, ImagePath::Path("d:\\a\\app.exe".into())),
                (ProcKey { pid: 20, created: 100 }, ImagePath::Path("d:\\a\\child.exe".into())),
            ],
        };
        let w = AppWindow { owner: ProcKey { pid: 10, created: 500 }, identity: IdentityId(1), hung: false };
        let (g, _) = run(&t, &map, &[w], &[]);
        assert_eq!(g.groups.len(), 2);
        assert!(g.groups.iter().any(|x| x.section == Section::Background));
    }

    #[test]
    fn a_deep_chain_stops_at_64_steps() {
        let mut specs = vec![p(10, 1, 1, "app.exe", "d:\\a\\app.exe")];
        for pid in 11..=80 {
            specs.push(p(pid, pid - 1, 1, "helper.exe", "d:\\a\\helper.exe"));
        }
        let (t, map) = table(&specs, &[], 0);
        let (g, _) = run(&t, &map, &[win(10, 1)], &[]);
        // Pid 74 is 64 links below pid 10 and still joins; pid 75 is 65 and does not.
        assert_eq!(group_of(&g, &t, 74).section, Section::Apps);
        assert_eq!(group_of(&g, &t, 75).section, Section::Background);
    }

    fn rated() -> (ProcTable, RateBook) {
        let specs = [p(10, 1, 1, "app.exe", "d:\\a\\app.exe"), p(11, 10, 1, "helper.exe", "d:\\a\\helper.exe")];
        let (mut now, _) = table(&specs, &[], 0);
        let mut prev_rows = ProcTable::new();
        for r in &now.rows {
            prev_rows.push(*r);
        }
        prev_rows.finish();
        for (i, r) in now.rows.iter_mut().enumerate() {
            r.cpu_100ns = 5_000_000 * (i as u64 + 1);
            r.io_read = 1_000_000 * (i as u64 + 1);
            r.ws_private = 100 * (i as u64 + 1);
            r.private_bytes = 1000 * (i as u64 + 1);
            r.ws = 10_000 * (i as u64 + 1);
            r.threads = 3 + i as u32;
            r.handles = 50 * (i as u32 + 1);
        }
        let mut book = RateBook::new();
        book.update(&prev_rows, &now, 1000, 4);
        (now, book)
    }

    #[test]
    fn totals_saturate_on_huge_values() {
        let (mut t, book) = rated();
        for r in t.rows.iter_mut() {
            r.ws_private = u64::MAX;
            r.private_bytes = u64::MAX;
            r.ws = u64::MAX;
        }
        let g = Group {
            key: AppKey::App(IdentityId(1)),
            section: Section::Apps,
            members: vec![key(10), key(11)],
            windows: 0,
            hung_windows: 0,
        };
        let tot = totals(&g, &t, &book);
        assert_eq!((tot.mem, tot.private_bytes, tot.ws), (u64::MAX, u64::MAX, u64::MAX));
    }

    #[test]
    fn totals_sum_members() {
        let (t, book) = rated();
        let g = Group {
            key: AppKey::App(IdentityId(1)),
            section: Section::Apps,
            members: vec![key(10), key(11)],
            windows: 1,
            hung_windows: 0,
        };
        let tot = totals(&g, &t, &book);
        let (a, b) = (book.get(key(10)).unwrap(), book.get(key(11)).unwrap());
        assert!(tot.primed);
        assert_eq!(tot.cpu, a.cpu + b.cpu);
        assert_eq!(tot.io_bps, a.io_bps + b.io_bps);
        assert!((tot.cpu - (12.5 + 25.0)).abs() < 1e-3);
        assert_eq!((tot.mem, tot.private_bytes, tot.ws), (300, 3000, 30_000));
        assert_eq!((tot.threads, tot.handles), (7, 150));
        // A member the book does not know leaves the group unprimed and adds nothing.
        let mut with_stranger = g.clone();
        with_stranger.members.push(key(99));
        let t2 = totals(&with_stranger, &t, &book);
        assert!(!t2.primed);
        assert_eq!(t2.cpu, tot.cpu);
        assert!(!totals(&Group { members: Vec::new(), ..g }, &t, &book).primed);
    }

    #[test]
    fn group_stats_match_totals() {
        let (t, book) = rated();
        let mut names = Interner::new();
        let map = PathMap {
            entries: vec![
                (key(10), ImagePath::Path("d:\\a\\app.exe".into())),
                (key(11), ImagePath::Path("d:\\a\\helper.exe".into())),
            ],
        };
        let info = InfoBook::new().update(&t, &map, ROOT, &mut names).to_vec();
        let hung = AppWindow { hung: true, ..win(11, 1) };
        let windows = [win(10, 1), hung];
        let mut g = Grouping::default();
        group(&GroupInput { procs: &t, info: &info, windows: &windows, session: 1, hosts: &[] }, &mut g);
        let mut stats = vec![GroupStat {
            key: AppKey::Single(key(0)),
            section: Section::Windows,
            cpu: 9.0,
            mem: 9,
            io_bps: 9.0,
            hung: false,
            has_windows: false,
        }];
        group_stats(&g, &t, &book, &mut stats);
        assert_eq!(stats.len(), g.groups.len());
        let tot = totals(&g.groups[0], &t, &book);
        assert_eq!((stats[0].cpu, stats[0].mem, stats[0].io_bps), (tot.cpu, tot.mem, tot.io_bps));
        assert_eq!((stats[0].key, stats[0].section), (g.groups[0].key, Section::Apps));
        assert!(stats[0].hung && stats[0].has_windows);
    }
}
