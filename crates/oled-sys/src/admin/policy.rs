//! Refusal rules of the admin helper: pure functions over facts; the executor gathers the facts with OS calls.
//!
//! Every rule refuses when a fact is unknown: an unknown live create time is Gone, an unknown critical flag still
//! checks the name list, a program the rules cannot tell apart from its arguments is refused.
#![deny(clippy::unwrap_used, clippy::expect_used, clippy::indexing_slicing)]
use std::collections::HashSet;

use oled_trust::frame::admin::{AdminRequest, DenyReason, ProcKey, PROTO};

pub const CRITICAL_IMAGES: [&str; 10] = [
    "System",
    "Registry",
    "smss.exe",
    "csrss.exe",
    "wininit.exe",
    "winlogon.exe",
    "services.exe",
    "lsass.exe",
    "Memory Compression",
    "Secure System",
];

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mode {
    Session,
    OneShot,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Policy {
    pub mode: Mode,
    pub admin_protection: bool,
    pub command_line_enabled: bool,
}

/// The rules on a request before any target is looked at.
pub fn admit_request(req: &AdminRequest, p: &Policy) -> Result<(), DenyReason> {
    if let AdminRequest::Hello { proto } = req {
        if *proto != PROTO {
            return Err(DenyReason::BadFrame);
        }
    }
    let session = p.mode == Mode::Session;
    // No session exists under Administrator protection; one that does anyway gets nothing (defence in depth).
    if session && p.admin_protection {
        return Err(DenyReason::Disabled);
    }
    match req {
        AdminRequest::Hello { .. }
        | AdminRequest::Terminate { .. }
        | AdminRequest::SetPriority { .. }
        | AdminRequest::SetEfficiency { .. } => Ok(()),
        // Consent per action: a one-shot helper is that consent, a session is not.
        AdminRequest::ServiceControl { .. } | AdminRequest::StartupSetEnabled { .. } | AdminRequest::RestartDock => {
            if session {
                Err(DenyReason::NeedsConsent)
            } else {
                Ok(())
            }
        }
        AdminRequest::QueryCommandLine { .. } => {
            if p.command_line_enabled && !p.admin_protection {
                Ok(())
            } else {
                Err(DenyReason::Disabled)
            }
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TargetVerb {
    Terminate,
    SetPriority,
    SetEfficiency,
    QueryCommandLine,
    RestartDock,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TargetFacts {
    pub key: ProcKey,
    pub live_created: Option<u64>,
    pub session: u32,
    pub image_name: String,
    pub has_image_path: bool,
    pub under_system_root: bool,
    pub critical_flag: Option<bool>,
    pub protected: bool,
    pub hosts_reboot_service: bool,
}

/// dock_family: dock, watchdog, guard, update guard.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct HelperFacts {
    pub self_pid: u32,
    pub parent: ProcKey,
    pub session: u32,
    pub dock_family: [Option<ProcKey>; 4],
}

/// The rules on one target, in order: Gone, OwnProcess, OtherSession, Critical, RebootOnFailure, Protected (the
/// three process verbs only). QueryCommandLine reads and changes nothing: it applies only Gone and OtherSession.
pub fn check_target(verb: TargetVerb, t: &TargetFacts, h: &HelperFacts) -> Result<(), DenyReason> {
    // (1) The key must still name the live process: a recycled pid or an unknown create time is Gone.
    if t.live_created != Some(t.key.created) {
        return Err(DenyReason::Gone);
    }
    let reads_only = verb == TargetVerb::QueryCommandLine;
    // (2)
    if !reads_only && is_own(verb, t.key, h) {
        return Err(DenyReason::OwnProcess);
    }
    // (3) Session 0 holds the services; another session is another user's.
    if t.session == 0 || t.session != h.session {
        return Err(DenyReason::OtherSession);
    }
    if reads_only {
        return Ok(());
    }
    // (4)
    if is_critical_image(&t.image_name, t.has_image_path, t.under_system_root) || t.critical_flag == Some(true) {
        return Err(DenyReason::Critical);
    }
    // (5)
    if t.hosts_reboot_service {
        return Err(DenyReason::RebootOnFailure);
    }
    // (6)
    let changes_the_process =
        matches!(verb, TargetVerb::Terminate | TargetVerb::SetPriority | TargetVerb::SetEfficiency);
    if changes_the_process && t.protected {
        return Err(DenyReason::Protected);
    }
    Ok(())
}

/// Rule (2): the idle and system processes, the helper, its parent and the dock family. RestartDock turns it round:
/// the dock is its one target and every other process is refused.
fn is_own(verb: TargetVerb, key: ProcKey, h: &HelperFacts) -> bool {
    let idle_system_or_self = key.pid == 0 || key.pid == 4 || key.pid == h.self_pid;
    let [dock, ..] = h.dock_family;
    if verb == TargetVerb::RestartDock {
        return idle_system_or_self || dock != Some(key);
    }
    idle_system_or_self || key == h.parent || h.dock_family.contains(&Some(key))
}

/// A name of CRITICAL_IMAGES (ASCII case-insensitive) on a process with no image path or one under SystemRoot: a
/// "csrss.exe" in a download folder is not protected by its name.
pub fn is_critical_image(image_name: &str, has_image_path: bool, under_system_root: bool) -> bool {
    (!has_image_path || under_system_root) && CRITICAL_IMAGES.iter().any(|c| c.eq_ignore_ascii_case(image_name))
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ProcEdge {
    pub key: ProcKey,
    pub parent_pid: u32,
}

/// Root first, breadth first. A ProcEdge is a child of k when its parent_pid is k's pid and it was created after k:
/// a recycled pid adopts nothing. Each key at most once; `out` is cleared first.
pub fn descendants(root: ProcKey, procs: &[ProcEdge], out: &mut Vec<ProcKey>) {
    out.clear();
    out.push(root);
    // Edges by parent pid; the sort is stable, so siblings keep the order of the list.
    let mut by_parent: Vec<&ProcEdge> = procs.iter().collect();
    by_parent.sort_by_key(|e| e.parent_pid);
    let mut seen: HashSet<ProcKey> = HashSet::with_capacity(16);
    seen.insert(root);
    let mut next = 0;
    while let Some(&k) = out.get(next) {
        next += 1;
        let first = by_parent.partition_point(|e| e.parent_pid < k.pid);
        let siblings = by_parent.get(first..).unwrap_or_default();
        for e in siblings.iter().take_while(|e| e.parent_pid == k.pid) {
            if e.key.created > k.created && seen.insert(e.key) {
                out.push(e.key);
            }
        }
    }
}

/// Judges the program of a service's command line (QueryServiceConfig's binary path), never what its arguments do: it
/// must lie under one of `roots` (folders a standard user cannot write, such as SystemRoot and the program folders),
/// in none of their folders that a standard user can write (WRITABLE_IN_ROOT) and, when unquoted, hold no space after
/// the root and be followed by no "..", because Windows would try each space-separated prefix of the line as a
/// program, the shorter ones first and the longer ones when the program is missing.
pub fn service_image_allowed(binary_path: &str, roots: &[&str]) -> Result<(), DenyReason> {
    let Some((program, quoted)) = program_of(binary_path) else {
        return Err(DenyReason::ServiceImagePath);
    };
    // Interpreters and loaders (cmd, powershell, rundll32, ...) pass too: their arguments are the administrator's.
    if is_plain_absolute(program) && roots.iter().any(|root| lies_under(program, root, quoted)) {
        Ok(())
    } else {
        Err(DenyReason::ServiceImagePath)
    }
}

/// The program of a command line and whether it was quoted. Quoted: the text inside the first pair of quotes.
/// Unquoted: the shortest prefix ending in ".exe" (ASCII case-insensitive) followed by the end or a space, else the
/// whole line when it is one name. None when no program can be told apart: an unclosed quote, a line with no ".exe"
/// and more text after its first name (when that name plus ".exe" is missing, Windows goes on to try the longer
/// space-separated prefixes, which these rules would not have judged), or an unquoted program followed by ".."
/// (when the program is missing, Windows tries it with part of the arguments and normalises the result:
/// `C:\Windows\x.exe \..\..\Users\Public\evil` runs C:\Users\Public\evil.exe).
fn program_of(line: &str) -> Option<(&str, bool)> {
    if let Some(rest) = line.strip_prefix('"') {
        let end = rest.find('"')?;
        return rest.get(..end).map(|p| (p, true));
    }
    let bytes = line.as_bytes();
    for (i, w) in bytes.windows(4).enumerate() {
        let end = i + 4;
        if w.eq_ignore_ascii_case(b".exe") && matches!(bytes.get(end), None | Some(b' ')) {
            // Without "..", every longer prefix lies in a folder named after the program, in the program's folder.
            if line.get(end..).is_none_or(|args| args.contains("..")) {
                return None;
            }
            return line.get(..end).map(|p| (p, false));
        }
    }
    if line.contains(' ') {
        None
    } else {
        Some((line, false))
    }
}

/// Drive-absolute (`X:\`), no device or long-path prefix (`\\?\`, `\\.\`), no '/', no "..", no ':' after the drive
/// (an alternate stream), and none of the characters a file name cannot hold.
fn is_plain_absolute(p: &str) -> bool {
    let b = p.as_bytes();
    let drive_absolute = matches!(b, [d, b':', b'\\', ..] if d.is_ascii_alphabetic());
    let bad_char = |c: &u8| *c < 0x20 || matches!(*c, b'"' | b'*' | b'?' | b'<' | b'>' | b'|' | b'/');
    drive_absolute
        && !p.contains(r"\\?\")
        && !p.contains(r"\\.\")
        && !p.contains("..")
        && !b.iter().skip(2).any(|c| *c == b':')
        && !b.iter().any(bad_char)
}

/// `program` is `root` (ASCII case-insensitive, a trailing '\' ignored), a '\' and at least one more character;
/// unquoted, the part after the root holds no space (the root's own spaces lead only to its parent folders, which a
/// standard user cannot write either). A root must name a folder: a drive alone or a relative root admits nothing.
/// A program in a folder of WRITABLE_IN_ROOT, or below one, does not lie under the root.
fn lies_under(program: &str, root: &str, quoted: bool) -> bool {
    let root = root.trim_end_matches('\\');
    if !matches!(root.as_bytes(), [d, b':', b'\\', _, ..] if d.is_ascii_alphabetic()) {
        return false;
    }
    let (Some(head), Some(rest)) = (program.get(..root.len()), program.get(root.len()..)) else {
        return false;
    };
    let Some(inside) = rest.strip_prefix('\\') else {
        return false;
    };
    head.eq_ignore_ascii_case(root)
        && !inside.is_empty()
        && (quoted || !inside.contains(' '))
        && !in_writable_folder(inside)
}

/// Folders inside SystemRoot where a standard user can create files or folders (Users, Authenticated Users, Everyone or
/// Performance Log Users hold that right in a stock Windows 11), relative to a root and refused below every root.
/// Folders a third party made writable are not here: only an ACL check by the executor can see them (T9).
const WRITABLE_IN_ROOT: [&str; 16] = [
    "Temp",
    "Tasks",
    "tracing",
    r"debug\WIA",
    "PLA",
    r"Registration\CRMLog",
    r"System32\Tasks",
    r"System32\Tasks_Migrated",
    r"System32\spool",
    r"System32\com\dmp",
    r"System32\FxsTmp",
    r"System32\LogFiles\WMI",
    r"System32\Microsoft\Crypto\RSA\MachineKeys",
    r"SysWOW64\Tasks",
    r"SysWOW64\com\dmp",
    r"SysWOW64\FxsTmp",
];

/// `inside` (a path relative to a root) lies in a folder of WRITABLE_IN_ROOT or below it. The segments are compared
/// as Windows normalises them (ASCII case-insensitive): empty and "." segments drop out, trailing dots and spaces do
/// not count.
fn in_writable_folder(inside: &str) -> bool {
    let segments = || inside.split('\\').map(|s| s.trim_end_matches(['.', ' '])).filter(|s| !s.is_empty());
    WRITABLE_IN_ROOT.iter().any(|folder| {
        let mut path = segments();
        folder.split('\\').all(|f| path.next().is_some_and(|s| s.eq_ignore_ascii_case(f))) && path.next().is_some()
    })
}

/// A service whose failure action reboots the machine is never stopped.
pub fn service_stop_allowed(reboot_on_failure: bool) -> Result<(), DenyReason> {
    if reboot_on_failure {
        Err(DenyReason::RebootOnFailure)
    } else {
        Ok(())
    }
}

/// At most 20 in any 1000 ms.
pub const RATE_PER_S: usize = 20;

#[derive(Clone, Debug, Default)]
pub struct RateLimiter {
    stamps: [u64; 20],
    next: usize,
    filled: usize,
}

impl RateLimiter {
    pub fn new() -> RateLimiter {
        RateLimiter::default()
    }
    /// `now_ms` from a monotonic clock. Admits when fewer than 20 were admitted or the oldest of the last 20 is at
    /// least 1,000 ms old; a refusal is not recorded, so a flood locks nothing.
    pub fn admit(&mut self, now_ms: u64) -> bool {
        let len = self.stamps.len();
        if self.filled >= len {
            // Full: `next` holds the oldest admit time.
            let oldest = self.stamps.get(self.next).copied().unwrap_or(u64::MAX);
            if now_ms.saturating_sub(oldest) < 1000 {
                return false;
            }
        }
        let Some(slot) = self.stamps.get_mut(self.next) else {
            return false;
        };
        *slot = now_ms;
        self.next = (self.next + 1) % len;
        self.filled = (self.filled + 1).min(len);
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use oled_trust::frame::admin::AdminRequest;

    #[test]
    fn frozen_policy_types_compile() {
        let key = ProcKey { pid: 4, created: 9 };
        let p = Policy { mode: Mode::Session, admin_protection: false, command_line_enabled: false };
        let t = TargetFacts {
            key,
            live_created: Some(9),
            session: 1,
            image_name: "a.exe".into(),
            has_image_path: true,
            under_system_root: false,
            critical_flag: None,
            protected: false,
            hosts_reboot_service: false,
        };
        let h = HelperFacts { self_pid: 1, parent: key, session: 1, dock_family: [None; 4] };
        let _ = (p, t, h, ProcEdge { key, parent_pid: 1 }, RateLimiter::new(), RATE_PER_S, CRITICAL_IMAGES.len());
        let _ = (TargetVerb::Terminate, Mode::OneShot, AdminRequest::RestartDock);
        let _: fn(&AdminRequest, &Policy) -> Result<(), DenyReason> = admit_request;
        let _: fn(TargetVerb, &TargetFacts, &HelperFacts) -> Result<(), DenyReason> = check_target;
        let _: fn(&str, bool, bool) -> bool = is_critical_image;
        let _: fn(ProcKey, &[ProcEdge], &mut Vec<ProcKey>) = descendants;
        let _: fn(&str, &[&str]) -> Result<(), DenyReason> = service_image_allowed;
        let _: fn(bool) -> Result<(), DenyReason> = service_stop_allowed;
        let _: fn(&mut RateLimiter, u64) -> bool = RateLimiter::admit;
    }
}
