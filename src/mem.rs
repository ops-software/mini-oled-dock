//! Memory diagnostics and trimming.
//!
//! The dock is hidden almost all the time, so after the startup burst (WIC, shell icon
//! extraction, WinRT activation) it is worth giving freed heap blocks and resident pages back
//! to the OS. `trim` does that; `log_memory` writes one line with the numbers to `dock.log`.

use crate::log;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::Mutex;
use std::time::{Duration, Instant};
use windows::Win32::Foundation::{HANDLE, HMODULE};
use windows::Win32::System::Com::{CoFreeUnusedLibraries, CoInitializeEx, CoUninitialize, COINIT_MULTITHREADED};
use windows::Win32::System::Memory::{GetProcessHeaps, HeapCompact, HEAP_FLAGS};
use windows::Win32::System::ProcessStatus::{
    EmptyWorkingSet, EnumProcessModules, GetModuleBaseNameW, GetProcessMemoryInfo, PROCESS_MEMORY_COUNTERS,
    PROCESS_MEMORY_COUNTERS_EX,
};
use windows::Win32::System::Threading::{
    GetCurrentProcess, GetGuiResources, GetProcessHandleCount, GR_GDIOBJECTS, GR_USEROBJECTS,
};

/// Maximum number of process heaps handled by `trim`.
const MAX_HEAPS: usize = 64;

/// Private bytes right after the last `trim("hidden")` (0 = none yet).
static LAST_HIDDEN_TRIM: AtomicUsize = AtomicUsize::new(0);

/// Set when data was just freed: the next hidden period trims again (`take_rearm`, consumed by `trim_tick`).
static REARM: AtomicBool = AtomicBool::new(false);

/// When COM's second MTA pass is due for the shell thread's own COM work (`shell_thread::take_com_work`): set by the
/// trim whose MTA pass marked it, consumed by `second_pass_tick`, which re-arms the idle trim then.
static SECOND_PASS: Mutex<Option<Instant>> = Mutex::new(None);

/// More than COM's 10 minute delay for free-threaded DLLs between the pass that marks a DLL and the one that unloads it.
const SECOND_PASS_AFTER: Duration = Duration::from_secs(660);

/// The second pass after this trim: shell COM work since the last trim moves it to 11 minutes from now (the work's
/// DLLs are marked by this trim's MTA pass); otherwise the one already planned stays.
fn next_second_pass(com_work: bool, planned: Option<Instant>, now: Instant) -> Option<Instant> {
    if com_work {
        Some(now + SECOND_PASS_AFTER)
    } else {
        planned
    }
}

/// Whether the planned second pass is due at `now`.
fn second_pass_due(planned: Option<Instant>, now: Instant) -> bool {
    planned.is_some_and(|t| now >= t)
}

/// Each housekeeping tick (`main.rs` `trim_tick`, before `take_rearm`): a due second pass re-arms the idle trim, whose
/// MTA pass unloads what the shell's launches and AppsFolder look-ups left (live-b L-5: two trims 496 s apart never did).
pub fn second_pass_tick(now: Instant) {
    let mut g = SECOND_PASS.lock().unwrap_or_else(|e| e.into_inner());
    if second_pass_due(*g, now) {
        *g = None;
        drop(g);
        rearm();
    }
}

/// Most modules whose base and name are tracked (the process has about 80; the count itself is never capped).
const MAX_MODULES: usize = 512;

/// The module set seen by the last `log_memory`: bases and their names (parallel, at most `MAX_MODULES`), None until
/// the first sample, which is only a baseline.
static MODULES: Mutex<Option<(Vec<usize>, Vec<String>)>> = Mutex::new(None);

/// Bases that appeared in `now` and bases that left `prev`, each in the order of its own list, duplicates once.
/// An empty `prev` returns everything as added: the caller treats the first sample as a baseline and logs no names.
fn module_delta(prev: &[usize], now: &[usize]) -> (Vec<usize>, Vec<usize>) {
    let mut added: Vec<usize> = Vec::new();
    for &b in now {
        if !prev.contains(&b) && !added.contains(&b) {
            added.push(b);
        }
    }
    let mut removed: Vec<usize> = Vec::new();
    for &b in prev {
        if !now.contains(&b) && !removed.contains(&b) {
            removed.push(b);
        }
    }
    (added, removed)
}

/// Loaded modules of this process: the true count and the first `MAX_MODULES` bases. No heap allocation for the call.
fn enum_modules() -> Option<(usize, Vec<usize>)> {
    let mut mods = [HMODULE::default(); MAX_MODULES];
    let mut needed = 0u32;
    // SAFETY: the array is writable for `cb` bytes and `needed` for the call; the pseudo handle is always valid.
    let r = unsafe {
        EnumProcessModules(GetCurrentProcess(), mods.as_mut_ptr(), std::mem::size_of_val(&mods) as u32, &mut needed)
    };
    if r.is_err() {
        return None;
    }
    let total = needed as usize / std::mem::size_of::<HMODULE>();
    Some((total, mods.iter().take(total.min(MAX_MODULES)).map(|m| m.0 as usize).collect()))
}

/// Base name of a loaded module ("?" when it cannot be read).
fn module_name(base: usize) -> String {
    let mut buf = [0u16; 260];
    // SAFETY: the buffer is writable for its length; a stale handle only makes the call fail.
    let n = unsafe { GetModuleBaseNameW(GetCurrentProcess(), Some(HMODULE(base as *mut _)), &mut buf) } as usize;
    if n == 0 {
        "?".to_string()
    } else {
        String::from_utf16_lossy(&buf[..n.min(buf.len())])
    }
}

/// The `RAM modules` line for a module set: None for the first call (it only stores the baseline and its names) and
/// when the set did not change. Names of kept and removed modules come from the stored list, `name_of` is asked only
/// for new bases.
fn module_line(
    state: &mut Option<(Vec<usize>, Vec<String>)>,
    now: Vec<usize>,
    name_of: impl Fn(usize) -> String,
) -> Option<String> {
    let Some((prev, names)) = state.as_mut() else {
        let names = now.iter().map(|&b| name_of(b)).collect();
        *state = Some((now, names));
        return None;
    };
    let (added, removed) = module_delta(prev, &now);
    if added.is_empty() && removed.is_empty() {
        return None;
    }
    let mut parts: Vec<String> = Vec::with_capacity(added.len() + removed.len());
    let mut new_names: Vec<String> = Vec::with_capacity(now.len());
    for &b in &now {
        match prev.iter().position(|&p| p == b) {
            Some(i) => new_names.push(std::mem::take(&mut names[i])),
            None => new_names.push(name_of(b)),
        }
    }
    for &b in &added {
        let i = now.iter().position(|&x| x == b).unwrap_or(0);
        parts.push(format!("+{}", new_names[i]));
    }
    for &b in &removed {
        let i = prev.iter().position(|&x| x == b).unwrap_or(0);
        parts.push(format!("-{}", names[i]));
    }
    *names = new_names;
    *prev = now;
    Some(parts.join(" "))
}

/// Logs `RAM modules +a.dll -b.dll` when the module set changed since the last call (the first call is a baseline).
/// Returns the module count.
fn log_module_changes() -> Option<usize> {
    let (total, now) = enum_modules()?;
    let mut guard = MODULES.lock().unwrap_or_else(|e| e.into_inner());
    let line = module_line(&mut guard, now, module_name);
    drop(guard);
    if let Some(l) = line {
        log!("RAM modules {l}");
    }
    Some(total)
}

/// Snapshot of the process memory counters (bytes).
struct Counters {
    private: usize,
    working_set: usize,
    peak_working_set: usize,
}

fn counters() -> Option<Counters> {
    let mut c = PROCESS_MEMORY_COUNTERS_EX::default();
    let size = std::mem::size_of::<PROCESS_MEMORY_COUNTERS_EX>() as u32;
    c.cb = size;
    // SAFETY: PROCESS_MEMORY_COUNTERS_EX starts with the PROCESS_MEMORY_COUNTERS fields; `cb`
    // is the real size of the struct, which stays writable for the whole call.
    let r = unsafe {
        GetProcessMemoryInfo(
            GetCurrentProcess(),
            (&mut c as *mut PROCESS_MEMORY_COUNTERS_EX).cast::<PROCESS_MEMORY_COUNTERS>(),
            size,
        )
    };
    match r {
        Ok(()) => Some(Counters {
            private: c.PrivateUsage,
            working_set: c.WorkingSetSize,
            peak_working_set: c.PeakWorkingSetSize,
        }),
        Err(e) => {
            log!("GetProcessMemoryInfo failed: {e}");
            None
        }
    }
}

/// Logs private bytes, working set, peak working set, handle count and GDI/USER object counts.
pub fn log_memory() {
    let Some(c) = counters() else { return };
    let mut handles = 0u32;
    // SAFETY: the pseudo handle is always valid and `handles` is writable for the call.
    if let Err(e) = unsafe { GetProcessHandleCount(GetCurrentProcess(), &mut handles) } {
        log!("GetProcessHandleCount failed: {e}");
    }
    // SAFETY: plain query on the current process pseudo handle; returns 0 on failure.
    let (gdi, user) = unsafe {
        (GetGuiResources(GetCurrentProcess(), GR_GDIOBJECTS), GetGuiResources(GetCurrentProcess(), GR_USEROBJECTS))
    };
    let modules = log_module_changes().unwrap_or(0);
    log!(
        "RAM private={} KB  ws={} KB  peak_ws={} KB  handles={handles}  gdi={gdi}  user={user}  modules={modules}",
        c.private / 1024,
        c.working_set / 1024,
        c.peak_working_set / 1024
    );
}

/// Private bytes of the process now, or None when the query fails.
pub fn private_bytes() -> Option<usize> {
    counters().map(|c| c.private)
}

/// Private bytes right after the last trim while the bar was hidden (None = not trimmed yet).
pub fn last_hidden_trim() -> Option<usize> {
    match LAST_HIDDEN_TRIM.load(Ordering::Relaxed) {
        0 => None,
        b => Some(b),
    }
}

/// Asks the idle trim to run again in the next hidden period (data was freed, the earlier trim did not see it).
pub fn rearm() {
    REARM.store(true, Ordering::Relaxed);
}

/// Consumes the flag of `rearm`.
pub fn take_rearm() -> bool {
    REARM.swap(false, Ordering::Relaxed)
}

/// A release of resident data (Start's after 60 s, the launcher's after 10 minutes) is done: the memory goes back to the OS
/// at once only while the bar is hidden and no popup or Settings window is open (never under the user's hands), and the
/// idle trim is re-armed whatever happened (Open point 6).
pub fn released_data() {
    rearm();
    if !crate::ui::is_visible() && !crate::popup::is_open() && !crate::settings_window::is_open() {
        trim("release");
    }
}

/// Unloads the in-proc COM servers with no object left: here on the calling (UI, STA) thread, and from a short MTA thread.
/// The MTA call only marks candidates the first time and unloads them when called again 10 or more minutes later (COM's
/// own protection for free-threaded DLLs), so it is made at every trim and never with a delay of 0.
fn free_com_libraries() {
    // SAFETY: plain call; on a thread without COM it does nothing.
    unsafe { CoFreeUnusedLibraries() };
    let spawned = std::thread::Builder::new().name("com-free".into()).stack_size(64 * 1024).spawn(|| {
        // SAFETY: COM for this thread only, paired with CoUninitialize when the init succeeded.
        let hr = unsafe { CoInitializeEx(None, COINIT_MULTITHREADED) };
        // SAFETY: as above.
        unsafe { CoFreeUnusedLibraries() };
        if hr.is_ok() {
            // SAFETY: paired with the successful CoInitializeEx above.
            unsafe { CoUninitialize() };
        }
        // What the unloaded DLLs freed on detach is only returned to the system by a compaction after the unload.
        compact_heaps();
    });
    if let Err(e) = spawned {
        log!("trim: cannot start the com-free thread: {e}");
    }
    // The shell thread's apartment: the proxy/stub DLLs of its desktop-shell launches are unloaded by its own pass.
    crate::shell_thread::request(crate::shell_thread::ShellRequest::ComFree);
    let mut g = SECOND_PASS.lock().unwrap_or_else(|e| e.into_inner());
    *g = next_second_pass(crate::shell_thread::take_com_work(), *g, Instant::now());
}

/// Compacts every process heap; returns how many there are (0 when they cannot be listed, which is logged).
/// Safe from any thread: compaction is thread-safe on serialized heaps and merely returns 0 on failure.
fn compact_heaps() -> usize {
    let mut heaps = [HANDLE::default(); MAX_HEAPS];
    // SAFETY: the slice is writable and its length is the buffer capacity passed to the API.
    let n = unsafe { GetProcessHeaps(&mut heaps) } as usize;
    if n == 0 {
        log!("GetProcessHeaps returned no heaps: {}", windows::core::Error::from_thread());
    }
    for h in heaps.iter().take(n.min(MAX_HEAPS)) {
        // SAFETY: handle returned by GetProcessHeaps for this process.
        let _ = unsafe { HeapCompact(*h, HEAP_FLAGS(0)) };
    }
    n
}

/// Compacts every process heap (decommits coalesced free blocks) and empties the working set.
/// Call only while the dock is idle: the next show takes a few soft page faults.
pub fn trim(reason: &str) {
    let before = counters();
    free_com_libraries();
    let n = compact_heaps();
    // SAFETY: pseudo handle of the current process has the required access rights.
    if let Err(e) = unsafe { EmptyWorkingSet(GetCurrentProcess()) } {
        log!("EmptyWorkingSet failed: {e}");
    }
    let after = counters();
    if reason == "hidden" {
        if let Some(a) = &after {
            LAST_HIDDEN_TRIM.store(a.private, Ordering::Relaxed);
        }
    }
    match (before, after) {
        (Some(b), Some(a)) => log!(
            "trim({reason}): heaps={n} private {} -> {} KB, ws {} -> {} KB",
            b.private / 1024,
            a.private / 1024,
            b.working_set / 1024,
            a.working_set / 1024
        ),
        _ => log!("trim({reason}): heaps={n}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shell_com_work_plans_one_second_pass() {
        let t = Instant::now();
        // No shell COM work: nothing planned, and a plan already made stays as it is.
        assert_eq!(next_second_pass(false, None, t), None);
        let planned = Some(t + Duration::from_secs(30));
        assert_eq!(next_second_pass(false, planned, t), planned);
        // Work since the last trim: 11 minutes from this trim (more than COM's 10), a later trim's work moves it on.
        assert_eq!(next_second_pass(true, None, t), Some(t + SECOND_PASS_AFTER));
        assert_eq!(next_second_pass(true, planned, t + Duration::from_secs(60)), Some(t + Duration::from_secs(720)));
        assert!(SECOND_PASS_AFTER > Duration::from_secs(600));
        // Due at its time, not a moment before; nothing planned is never due.
        let p = Some(t + SECOND_PASS_AFTER);
        assert!(!second_pass_due(p, t + Duration::from_secs(659)));
        assert!(second_pass_due(p, t + SECOND_PASS_AFTER));
        assert!(!second_pass_due(None, t + Duration::from_secs(10_000)));
    }

    #[test]
    fn module_delta_cases() {
        assert_eq!(module_delta(&[1, 2, 3], &[1, 2, 3]), (vec![], vec![]));
        assert_eq!(module_delta(&[1, 2, 3], &[1, 3, 4]), (vec![4], vec![2]));
        // Stable order: each list in the order of its own input.
        assert_eq!(module_delta(&[9, 5, 7], &[8, 6, 5]), (vec![8, 6], vec![9, 7]));
        // An empty prev adds everything: the caller treats the first sample as a baseline and logs no names.
        assert_eq!(module_delta(&[], &[1, 2]), (vec![1, 2], vec![]));
        // Duplicates count once.
        assert_eq!(module_delta(&[1, 1], &[2, 2, 1]), (vec![2], vec![]));
        assert_eq!(module_delta(&[1, 1, 3], &[2]), (vec![2], vec![1, 3]));
    }

    #[test]
    fn module_line_baseline_and_names() {
        let mut st = None;
        let names = |b: usize| ["?", "a.dll", "b.dll", "c.dll", "d.dll"][b].to_string();
        // The first sample is a baseline: no line, names stored.
        assert_eq!(module_line(&mut st, vec![1, 2, 3], names), None);
        // The removed name comes from the stored list, not from name_of.
        let line = module_line(&mut st, vec![1, 3, 4], |b| if b == 4 { "d.dll".into() } else { "?".into() });
        assert_eq!(line.as_deref(), Some("+d.dll -b.dll"));
        // Unchanged set: no line, and nothing asked for names.
        assert_eq!(module_line(&mut st, vec![1, 3, 4], |_| unreachable!()), None);
        // Kept names survived the move.
        let line = module_line(&mut st, vec![1], |_| unreachable!());
        assert_eq!(line.as_deref(), Some("-c.dll -d.dll"));
    }

    #[test]
    fn module_count_is_live() {
        let (total, bases) = enum_modules().expect("EnumProcessModules");
        assert!(total >= 3 && bases.len() == total.min(MAX_MODULES));
        assert!(module_name(bases[0]).to_lowercase().ends_with(".exe"));
    }

    #[test]
    fn hidden_trim_is_remembered() {
        assert!(private_bytes().is_some_and(|b| b > 0));
        trim("hidden");
        assert!(last_hidden_trim().is_some_and(|b| b > 0));
    }
}
