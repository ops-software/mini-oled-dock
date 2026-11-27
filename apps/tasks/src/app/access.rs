//! Access facts and refusals (T3.6): the policy's check_target plus the app's own rules, one pure function used by
//! the menu and again by the executor on every key it is about to terminate. The facts of a member come from the
//! latest frame (session, image name, path) and from a probe of the system (two OpenProcess calls), cached per key.

use std::collections::HashMap;

use oled_shellmodel::process::{package_aumid, OwnedProcess};
use oled_sys::admin::policy::{check_target, HelperFacts, ProcEdge, TargetFacts, TargetVerb};
use oled_sys::i18n::S;
use oled_sys::pipe::{peer_of_pid, Integrity};
use oled_sysmon::{ProcKey, ProcTable};
use oled_trust::frame::admin::DenyReason;
use windows::core::BOOL;
use windows::Win32::Foundation::{CloseHandle, FILETIME, HANDLE};
use windows::Win32::System::Threading::{
    GetProcessTimes, IsProcessCritical, OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION, PROCESS_TERMINATE,
};

use super::exec::{KillGuard, INVALID_PARAMETER};
use crate::model::classify::{path_class, ImagePath, PathClass, PathMap};
use crate::model::tree::ancestors;
use crate::strings::k;

/// Why a key is not terminated.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Refusal {
    /// The policy's rule (LT-POLICY's check_target).
    Deny(DenyReason),
    /// The shell process (why.shell).
    Shell,
    /// Elevated, another user's or protected: needs administrator rights (why.admin).
    Limited,
}

/// ERROR_ACCESS_DENIED.
pub const ACCESS_DENIED: u32 = 5;

/// The most probes kept for the session; past it the book starts again (a probe is microseconds).
const PROBES_CAP: usize = 4096;

/// What the system says about one process.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Probe {
    /// Its create time, read through a handle (the frame's when we may not query it); None when it is gone.
    pub live_created: Option<u64>,
    /// OpenProcess(PROCESS_TERMINATE) did not fail with ERROR_ACCESS_DENIED.
    pub can_terminate: bool,
    /// IsProcessCritical (unverified, SPEC-tasks 5.5.9); None when it failed: the name list still applies.
    pub critical_flag: Option<bool>,
}

impl Probe {
    /// What the frame alone says: alive as displayed, terminable, no flag (scenes, and the inline action's rule).
    pub fn assumed(key: ProcKey) -> Probe {
        Probe { live_created: Some(key.created), can_terminate: true, critical_flag: None }
    }
}

/// Who a process runs as, for Reopen: its integrity level and the package AUMID its token carries. The system sets
/// both; the process names neither (it does name its windows' AUMID, and its starter chose the file of its image).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Standing {
    /// None: gone, its pid reused, or not ours to read.
    pub level: Option<Integrity>,
    pub package: Option<String>,
}

/// Where probes come from: the system, or the fixed answers of scenes and tests.
pub trait Prober {
    fn probe(&self, key: ProcKey) -> Probe;
    /// The standing of `key`, read now (never kept: a process may lower its own level). Unknown by default.
    fn standing(&self, key: ProcKey) -> Standing {
        let _ = key;
        Standing::default()
    }
    /// The integrity level Tasks runs at; None (unknown) by default.
    fn own_level(&self) -> Option<Integrity> {
        None
    }
}

/// The system: OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION) for the create time and the critical flag, then
/// OpenProcess(PROCESS_TERMINATE) for the right to end it.
pub struct Win32Prober;

impl Prober for Win32Prober {
    fn probe(&self, key: ProcKey) -> Probe {
        // SAFETY: OpenProcess takes no pointers; the handle it returns is closed below.
        let query = match unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, key.pid) } {
            Ok(h) => {
                let created = creation_time(h);
                let mut critical = BOOL(0);
                // SAFETY: h is an open process handle; critical is a local the call writes.
                let flag = unsafe { IsProcessCritical(h, &mut critical) }.is_ok().then(|| critical.as_bool());
                close(h);
                Ok((created, flag))
            }
            Err(e) => Err(win32_code(&e)),
        };
        // SAFETY: as above.
        let terminate = match unsafe { OpenProcess(PROCESS_TERMINATE, false, key.pid) } {
            Ok(h) => {
                close(h);
                Ok(())
            }
            Err(e) => Err(win32_code(&e)),
        };
        probe_from(key, query, terminate)
    }

    /// The level from the token of the process the key names (its create time read through the same handle), and
    /// the package AUMID of its token (GetApplicationUserModelId) when the create time matches too.
    fn standing(&self, key: ProcKey) -> Standing {
        let level = peer_of_pid(key.pid).ok().filter(|p| p.created == key.created).map(|p| p.integrity);
        let package = OwnedProcess::open(key.pid)
            .filter(|p| creation_time(p.handle()) == Some(key.created))
            .and_then(|p| package_aumid(&p));
        Standing { level, package }
    }

    fn own_level(&self) -> Option<Integrity> {
        peer_of_pid(std::process::id()).ok().map(|p| p.integrity)
    }
}

/// A probe from the two opens: the query handle's create time and critical flag (or its Win32 code), and the
/// terminate open's result. Only ERROR_INVALID_PARAMETER means gone: a process a medium token may not even query
/// (another user's, a hardened service) keeps the frame's create time, so the menu gives the reason (session, name,
/// or needs administrator) instead of treating it as gone; the executor checks the create time again before it acts.
fn probe_from(key: ProcKey, query: Result<(Option<u64>, Option<bool>), u32>, terminate: Result<(), u32>) -> Probe {
    let can_terminate = terminate.map_or_else(|code| code != ACCESS_DENIED, |()| true);
    match query {
        Ok((live_created, critical_flag)) => Probe { live_created, can_terminate, critical_flag },
        Err(INVALID_PARAMETER) => Probe { live_created: None, can_terminate, critical_flag: None },
        Err(_) => Probe { live_created: Some(key.created), can_terminate: false, critical_flag: None },
    }
}

/// Scenes and tests: every process alive as displayed and terminable, except the keys of `limited`; Tasks at `own`
/// (medium unless set) and every process at that level too, except the keys of `below` (low, each with the package
/// AUMID of its token).
#[derive(Clone, Debug)]
pub struct FixtureProber {
    pub limited: Vec<ProcKey>,
    pub below: Vec<(ProcKey, Option<String>)>,
    pub own: Option<Integrity>,
}

impl Default for FixtureProber {
    fn default() -> Self {
        FixtureProber { limited: Vec::new(), below: Vec::new(), own: Some(Integrity::Medium) }
    }
}

impl Prober for FixtureProber {
    fn probe(&self, key: ProcKey) -> Probe {
        Probe { can_terminate: !self.limited.contains(&key), ..Probe::assumed(key) }
    }

    fn standing(&self, key: ProcKey) -> Standing {
        match self.below.iter().find(|(k, _)| *k == key) {
            Some((_, package)) => Standing { level: Some(Integrity::Low), package: package.clone() },
            None => Standing { level: self.own, package: None },
        }
    }

    fn own_level(&self) -> Option<Integrity> {
        self.own
    }
}

/// The probes of the session, by key (a key names one process for good, so a probe stays true while it lives).
#[derive(Debug, Default)]
pub struct AccessBook {
    probes: HashMap<ProcKey, Probe>,
}

impl AccessBook {
    /// The probe of `key`, from the book or from `p` the first time.
    pub fn probe(&mut self, key: ProcKey, p: &dyn Prober) -> Probe {
        if let Some(x) = self.probes.get(&key) {
            return *x;
        }
        if self.probes.len() >= PROBES_CAP {
            self.probes.clear();
        }
        let x = p.probe(key);
        self.probes.insert(key, x);
        x
    }
}

/// One member's facts: the policy's, and whether a medium process may end it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MemberFacts {
    pub target: TargetFacts,
    pub can_terminate: bool,
}

/// The facts of `key`: its session and image name from the frame, its path class from the path map (no image, a
/// pending or an unreadable path count as no path, so the critical name list applies), the rest from the probe.
/// Protected processes cannot be told from elevated ones at medium (open point 8) and session-0 processes are refused
/// before any service rule, so protected and hosts_reboot_service are false.
pub fn facts(key: ProcKey, procs: &ProcTable, paths: &PathMap, system_root: &str, p: Probe) -> MemberFacts {
    let row = procs.find(key);
    let image_name = row.map(|r| String::from_utf16_lossy(procs.names.get(r.image))).unwrap_or_default();
    let (has_image_path, under_system_root) = match paths.get(key) {
        Some(ImagePath::Path(path)) => (true, path_class(Some(path), system_root) == PathClass::SystemRoot),
        _ => (false, false),
    };
    MemberFacts {
        target: TargetFacts {
            key,
            live_created: p.live_created,
            session: row.map_or(0, |r| r.session),
            image_name,
            has_image_path,
            under_system_root,
            critical_flag: p.critical_flag,
            protected: false,
            hosts_reboot_service: false,
        },
        can_terminate: p.can_terminate,
    }
}

/// The dock family's processes in `paths` that `procs` still has: every process whose image is `dock_exe`
/// (the dock, its watchdog and guards run from one exe), at most four.
pub fn dock_family(procs: &ProcTable, paths: &PathMap, dock_exe: Option<&str>) -> [Option<ProcKey>; 4] {
    let mut out = [None; 4];
    let Some(dock) = dock_exe else { return out };
    let found = paths.entries.iter().filter_map(|(key, path)| match path {
        ImagePath::Path(p) if p.eq_ignore_ascii_case(dock) && procs.index_of(*key).is_some() => Some(*key),
        _ => None,
    });
    for (slot, key) in out.iter_mut().zip(found) {
        *slot = Some(key);
    }
    out
}

/// The keys no batch may terminate, from the latest frame: our process (`own_pid`), its parent and ancestor chain,
/// the shell process (`shell_pid`), the dock family and our session.
pub fn guard(
    procs: &ProcTable,
    paths: &PathMap,
    own_pid: u32,
    shell_pid: Option<u32>,
    session: u32,
    dock_exe: Option<&str>,
) -> KillGuard {
    let own = procs.find_pid(own_pid);
    let mut chain = Vec::new();
    if let Some(r) = own {
        ancestors(procs, r.key, &mut chain);
    }
    // ancestors starts with the process itself.
    let own_chain: Vec<ProcKey> = chain.into_iter().skip(1).collect();
    let parent = own.and_then(|r| r.parent).unwrap_or(ProcKey { pid: 0, created: 0 });
    KillGuard {
        helper: HelperFacts { self_pid: own_pid, parent, session, dock_family: dock_family(procs, paths, dock_exe) },
        own_chain,
        shell: shell_pid.and_then(|p| procs.find_pid(p)).map(|r| r.key),
    }
}

/// The process edges of a frame (what policy::descendants walks).
pub fn edges(procs: &ProcTable) -> Vec<ProcEdge> {
    procs.rows.iter().map(|r| ProcEdge { key: r.key, parent_pid: r.parent_pid }).collect()
}

/// check_target with g.helper, then the app's rules: the shell process, our ancestor chain. The shell is refused as
/// the shell even when it is our parent (Tasks started from the desktop), so the reason names what would close.
pub fn refusal(t: &TargetFacts, g: &KillGuard) -> Option<Refusal> {
    match check_target(TargetVerb::Terminate, t, &g.helper) {
        Err(DenyReason::OwnProcess) if g.shell == Some(t.key) => return Some(Refusal::Shell),
        Err(d) => return Some(Refusal::Deny(d)),
        Ok(()) => {}
    }
    if g.shell == Some(t.key) {
        return Some(Refusal::Shell);
    }
    if g.own_chain.contains(&t.key) {
        return Some(Refusal::Deny(DenyReason::OwnProcess));
    }
    None
}

/// The part of the rules a key alone decides, before anything is opened: the idle and system processes, our own
/// process, our parent and ancestor chain, the shell process and the dock family.
pub fn key_refusal(key: ProcKey, g: &KillGuard) -> Option<Refusal> {
    if g.shell == Some(key) {
        return Some(Refusal::Shell);
    }
    let h = &g.helper;
    let own = key.pid == 0
        || key.pid == 4
        || key.pid == h.self_pid
        || key == h.parent
        || h.dock_family.contains(&Some(key))
        || g.own_chain.contains(&key);
    own.then_some(Refusal::Deny(DenyReason::OwnProcess))
}

/// The menu's rule: refusal, then a process a medium process may not end (why.admin).
pub fn menu_refusal(f: &MemberFacts, g: &KillGuard) -> Option<Refusal> {
    refusal(&f.target, g).or((!f.can_terminate).then_some(Refusal::Limited))
}

/// The reason key of a refusal of `key`; None for Gone (the row shows as a ghost instead).
pub fn why(r: Refusal, key: ProcKey, g: &KillGuard) -> Option<S> {
    Some(match r {
        Refusal::Shell => k::TASKS_WHY_SHELL,
        Refusal::Limited => k::TASKS_WHY_ADMIN,
        Refusal::Deny(d) => match d {
            DenyReason::Gone => return None,
            DenyReason::OtherSession => k::TASKS_WHY_SESSION0,
            DenyReason::Critical => k::TASKS_WHY_CRITICAL,
            DenyReason::Protected => k::TASKS_WHY_PROTECTED,
            DenyReason::RebootOnFailure => k::TASKS_WHY_REBOOT,
            // The idle and system processes are Windows itself.
            DenyReason::OwnProcess if key.pid == 0 || key.pid == 4 => k::TASKS_WHY_CRITICAL,
            DenyReason::OwnProcess if g.helper.dock_family.contains(&Some(key)) => k::TASKS_WHY_DOCK,
            DenyReason::OwnProcess => k::TASKS_WHY_SELF,
            // The helper's other reasons do not come from a terminate check at medium.
            _ => k::TASKS_WHY_ADMIN,
        },
    })
}

/// The create time of an open process (GetProcessTimes), as the snapshot reports it (100 ns since 1601).
pub(crate) fn creation_time(h: HANDLE) -> Option<u64> {
    let (mut created, mut exited, mut kernel, mut user) =
        (FILETIME::default(), FILETIME::default(), FILETIME::default(), FILETIME::default());
    // SAFETY: h is an open process handle; the four FILETIMEs are locals the call writes.
    unsafe { GetProcessTimes(h, &mut created, &mut exited, &mut kernel, &mut user) }.ok()?;
    Some((u64::from(created.dwHighDateTime) << 32) | u64::from(created.dwLowDateTime))
}

/// Closes a handle this module or the executor opened.
pub(crate) fn close(h: HANDLE) {
    // SAFETY: h was returned by OpenProcess and is closed exactly once, here.
    let _ = unsafe { CloseHandle(h) };
}

/// The Win32 code of a failed call (the low word of an HRESULT_FROM_WIN32), else the HRESULT itself.
pub(crate) fn win32_code(e: &windows::core::Error) -> u32 {
    let h = e.code().0 as u32;
    if h & 0xFFFF_0000 == 0x8007_0000 {
        h & 0xFFFF
    } else {
        h
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use oled_sysmon::ProcRow;
    use std::sync::Arc;

    const ROOT: &str = "C:\\Windows";

    fn key(pid: u32) -> ProcKey {
        ProcKey { pid, created: 1_000 + u64::from(pid) }
    }

    fn target(pid: u32, image: &str, session: u32) -> TargetFacts {
        let k = key(pid);
        TargetFacts {
            key: k,
            live_created: Some(k.created),
            session,
            image_name: image.to_string(),
            has_image_path: true,
            under_system_root: false,
            critical_flag: Some(false),
            protected: false,
            hosts_reboot_service: false,
        }
    }

    /// Tasks is pid 100 (session 1), started by 90, started by 80; the shell is 70; the dock is 60.
    fn guard_fixture() -> KillGuard {
        KillGuard {
            helper: HelperFacts {
                self_pid: 100,
                parent: key(90),
                session: 1,
                dock_family: [Some(key(60)), None, None, None],
            },
            own_chain: vec![key(90), key(80)],
            shell: Some(key(70)),
        }
    }

    #[test]
    fn refusal_table() {
        let g = guard_fixture();
        let deny = |d| Some(Refusal::Deny(d));
        // A normal process of our session is allowed.
        assert_eq!(refusal(&target(500, "orbit.exe", 1), &g), None);
        // Gone: the create time moved (a reused pid) or cannot be read.
        let mut t = target(501, "orbit.exe", 1);
        t.live_created = Some(t.key.created + 1);
        assert_eq!(refusal(&t, &g), deny(DenyReason::Gone));
        t.live_created = None;
        assert_eq!(refusal(&t, &g), deny(DenyReason::Gone));
        // Session 0 and another user's session.
        assert_eq!(refusal(&target(502, "svchost.exe", 0), &g), deny(DenyReason::OtherSession));
        assert_eq!(refusal(&target(503, "quill.exe", 2), &g), deny(DenyReason::OtherSession));
        // Critical by its flag, and by name under the system root.
        let mut t = target(504, "quill.exe", 1);
        t.critical_flag = Some(true);
        assert_eq!(refusal(&t, &g), deny(DenyReason::Critical));
        let mut t = target(505, "winlogon.exe", 1);
        t.under_system_root = true;
        assert_eq!(refusal(&t, &g), deny(DenyReason::Critical));
        // A critical name elsewhere is only a name.
        assert_eq!(refusal(&target(506, "csrss.exe", 1), &g), None);
        // Protected, and a host of a reboot-on-failure service.
        let mut t = target(507, "quill.exe", 1);
        t.protected = true;
        assert_eq!(refusal(&t, &g), deny(DenyReason::Protected));
        let mut t = target(508, "quill.exe", 1);
        t.hosts_reboot_service = true;
        assert_eq!(refusal(&t, &g), deny(DenyReason::RebootOnFailure));
        // Our own process, our parent, our grandparent, the dock family.
        for pid in [100, 90, 80, 60] {
            assert_eq!(refusal(&target(pid, "x.exe", 1), &g), deny(DenyReason::OwnProcess), "pid {pid}");
        }
        // The shell, even as our parent.
        assert_eq!(refusal(&target(70, "explorer.exe", 1), &g), Some(Refusal::Shell));
        let mut g2 = guard_fixture();
        g2.helper.parent = key(70);
        assert_eq!(refusal(&target(70, "explorer.exe", 1), &g2), Some(Refusal::Shell));
        // There is no suspend in the beta: the never-suspend list refuses nothing.
        for image in ["dwm.exe", "ctfmon.exe", "audiodg.exe", "sihost.exe"] {
            let mut t = target(600, image, 1);
            t.under_system_root = true;
            assert_eq!(refusal(&t, &g), None, "{image}");
        }
        // A process a medium process may not end is limited (the menu's rule).
        let f = MemberFacts { target: target(509, "quill.exe", 1), can_terminate: false };
        assert_eq!(menu_refusal(&f, &g), Some(Refusal::Limited));
        let f = MemberFacts { target: target(509, "quill.exe", 1), can_terminate: true };
        assert_eq!(menu_refusal(&f, &g), None);
        // The policy's reasons come before limited.
        let f = MemberFacts { target: target(510, "svchost.exe", 0), can_terminate: false };
        assert_eq!(menu_refusal(&f, &g), deny(DenyReason::OtherSession));
    }

    #[test]
    fn critical_by_name_without_flag() {
        let g = guard_fixture();
        let mut t = target(520, "csrss.exe", 1);
        t.critical_flag = None;
        t.under_system_root = true;
        assert_eq!(refusal(&t, &g), Some(Refusal::Deny(DenyReason::Critical)));
        // A process with no image path ("Registry", "Memory Compression") is judged by its name.
        let mut t = target(521, "Memory Compression", 1);
        t.critical_flag = None;
        t.has_image_path = false;
        assert_eq!(refusal(&t, &g), Some(Refusal::Deny(DenyReason::Critical)));
    }

    #[test]
    fn key_alone_refuses_the_guarded_keys() {
        let g = guard_fixture();
        for pid in [0, 4, 100, 90, 80, 60] {
            assert_eq!(key_refusal(key(pid), &g), Some(Refusal::Deny(DenyReason::OwnProcess)), "pid {pid}");
        }
        assert_eq!(key_refusal(key(70), &g), Some(Refusal::Shell));
        assert_eq!(key_refusal(key(500), &g), None);
        // The same pid with another create time is another process.
        assert_eq!(key_refusal(ProcKey { pid: 60, created: 1 }, &g), None);
    }

    #[test]
    fn reasons_by_refusal() {
        let g = guard_fixture();
        let w = |r, pid| why(r, key(pid), &g);
        assert_eq!(w(Refusal::Shell, 70), Some(k::TASKS_WHY_SHELL));
        assert_eq!(w(Refusal::Limited, 500), Some(k::TASKS_WHY_ADMIN));
        assert_eq!(w(Refusal::Deny(DenyReason::Gone), 500), None);
        assert_eq!(w(Refusal::Deny(DenyReason::OtherSession), 500), Some(k::TASKS_WHY_SESSION0));
        assert_eq!(w(Refusal::Deny(DenyReason::Critical), 500), Some(k::TASKS_WHY_CRITICAL));
        assert_eq!(w(Refusal::Deny(DenyReason::Protected), 500), Some(k::TASKS_WHY_PROTECTED));
        assert_eq!(w(Refusal::Deny(DenyReason::RebootOnFailure), 500), Some(k::TASKS_WHY_REBOOT));
        assert_eq!(w(Refusal::Deny(DenyReason::OwnProcess), 60), Some(k::TASKS_WHY_DOCK));
        assert_eq!(w(Refusal::Deny(DenyReason::OwnProcess), 100), Some(k::TASKS_WHY_SELF));
        assert_eq!(w(Refusal::Deny(DenyReason::OwnProcess), 4), Some(k::TASKS_WHY_CRITICAL));
    }

    fn table(rows: &[(u32, u32, u32, &str)]) -> ProcTable {
        let mut t = ProcTable::new();
        for &(pid, parent, session, image) in rows {
            let utf16: Vec<u16> = image.encode_utf16().collect();
            let image = t.names.intern(&utf16);
            t.push(ProcRow { key: key(pid), parent_pid: parent, session, image, ..ProcRow::default() });
        }
        t.finish();
        t
    }

    #[test]
    fn facts_and_guard_from_a_frame() {
        let procs = table(&[
            (4, 0, 0, "System"),
            (70, 0, 1, "explorer.exe"),
            (80, 70, 1, "cmd.exe"),
            (90, 80, 1, "launcher.exe"),
            (100, 90, 1, "tasks.exe"),
            (60, 4, 1, "dock.exe"),
            (61, 60, 1, "dock.exe"),
            (300, 70, 1, "csrss.exe"),
        ]);
        let path = |s: &str| ImagePath::Path(Arc::from(s));
        let mut entries = vec![
            (key(4), ImagePath::NoImage),
            (key(60), path("C:\\Apps\\Dock\\dock.exe")),
            (key(61), path("c:\\apps\\dock\\DOCK.EXE")),
            (key(300), path("C:\\Windows\\System32\\csrss.exe")),
        ];
        entries.sort_by_key(|e| e.0);
        let paths = PathMap { entries };
        let g = guard(&procs, &paths, 100, Some(70), 1, Some("C:\\Apps\\Dock\\dock.exe"));
        assert_eq!(g.helper.self_pid, 100);
        assert_eq!(g.helper.parent, key(90));
        assert_eq!(g.own_chain, [key(90), key(80), key(70)], "our parent, its parent and so on");
        assert_eq!(g.shell, Some(key(70)));
        assert_eq!(g.helper.dock_family, [Some(key(60)), Some(key(61)), None, None]);
        let f = facts(key(300), &procs, &paths, ROOT, Probe::assumed(key(300)));
        assert_eq!(
            (f.target.image_name.as_str(), f.target.has_image_path, f.target.under_system_root),
            ("csrss.exe", true, true)
        );
        assert_eq!(refusal(&f.target, &g), Some(Refusal::Deny(DenyReason::Critical)));
        let f = facts(key(4), &procs, &paths, ROOT, Probe::assumed(key(4)));
        assert!(!f.target.has_image_path);
        assert_eq!(f.target.session, 0);
        // Not in the frame: no name, session 0 (refused).
        let f = facts(key(999), &procs, &paths, ROOT, Probe::assumed(key(999)));
        assert_eq!((f.target.session, f.target.image_name.as_str()), (0, ""));
        // Without our own process in the frame (scenes): our pid alone.
        let g = guard(&procs, &paths, u32::MAX, None, 1, None);
        assert!(g.own_chain.is_empty() && g.shell.is_none() && g.helper.dock_family == [None; 4]);
    }

    #[test]
    fn a_process_we_may_not_query_keeps_a_reason() {
        let g = guard_fixture();
        let pk = key(530);
        // Gone: no create time, no reason (the row shows as a ghost).
        let gone = probe_from(pk, Err(87), Err(87));
        assert_eq!(gone.live_created, None);
        // Not ours to query: alive as displayed, not terminable at medium.
        let denied = probe_from(pk, Err(ACCESS_DENIED), Err(ACCESS_DENIED));
        assert_eq!((denied.live_created, denied.can_terminate), (Some(pk.created), false));
        let procs = table(&[(530, 4, 1, "quill.exe"), (531, 4, 2, "quill.exe"), (532, 4, 0, "svchost.exe")]);
        let paths = PathMap::default();
        let reason = |pid: u32| {
            let f = facts(key(pid), &procs, &paths, ROOT, probe_from(key(pid), Err(ACCESS_DENIED), Err(ACCESS_DENIED)));
            menu_refusal(&f, &g).and_then(|r| why(r, key(pid), &g))
        };
        assert_eq!(reason(530), Some(k::TASKS_WHY_ADMIN), "our session: needs administrator");
        assert_eq!(reason(531), Some(k::TASKS_WHY_SESSION0), "another user's session");
        assert_eq!(reason(532), Some(k::TASKS_WHY_SESSION0), "session 0");
        // A query that worked is taken as it reads.
        let ok = probe_from(pk, Ok((Some(7), Some(true))), Ok(()));
        assert_eq!(ok, Probe { live_created: Some(7), can_terminate: true, critical_flag: Some(true) });
    }

    #[test]
    fn the_book_probes_each_key_once() {
        struct Counting(std::cell::Cell<u32>);
        impl Prober for Counting {
            fn probe(&self, key: ProcKey) -> Probe {
                self.0.set(self.0.get() + 1);
                Probe::assumed(key)
            }
        }
        let c = Counting(std::cell::Cell::new(0));
        let mut b = AccessBook::default();
        b.probe(key(1), &c);
        b.probe(key(1), &c);
        b.probe(key(2), &c);
        assert_eq!(c.0.get(), 2);
        let f = FixtureProber { limited: vec![key(2)], ..FixtureProber::default() };
        assert!(f.probe(key(1)).can_terminate && !f.probe(key(2)).can_terminate);
    }

    /// The real reads, on this very process only: its level is Tasks' own, and a key with another create time (a
    /// reused pid) reads as unknown.
    #[test]
    fn the_system_reads_our_own_standing() {
        let pid = std::process::id();
        let created = OwnedProcess::open(pid).and_then(|p| creation_time(p.handle())).expect("our create time");
        let own = Win32Prober.own_level();
        assert!(matches!(own, Some(Integrity::Medium | Integrity::High | Integrity::System)), "{own:?}");
        assert_eq!(Win32Prober.standing(ProcKey { pid, created }).level, own);
        assert_eq!(Win32Prober.standing(ProcKey { pid, created: created + 1 }), Standing::default());
    }
}
