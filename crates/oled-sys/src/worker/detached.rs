//! Detached workers: a breakaway process with no stdio, reached through its pipe (F-P4).
//!
//! The host serves `mini-oled-dock.<app>.<role>.<session>` (FIRST_PIPE_INSTANCE: a squatter makes it give up; DACL
//! SYSTEM plus its own token user) and admits only clients of its image, session and at least its integrity. The
//! spawner connects within `deadlines.start` and accepts the server only if it is the process it started (first
//! connect) or runs `spec.exe` (reconnect), in this session, at exactly its own integrity and as its own user:
//! anything else is `Untrusted`.

use std::os::windows::ffi::OsStrExt;
use std::os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle};
use std::path::Path;
use std::time::{Duration, Instant};

use windows::core::{PCWSTR, PWSTR};
use windows::Win32::Foundation::{CloseHandle, ERROR_ACCESS_DENIED, HANDLE};
use windows::Win32::System::Threading::{
    CreateProcessW, OpenProcess, TerminateProcess, WaitForSingleObject, CREATE_BREAKAWAY_FROM_JOB, CREATE_NO_WINDOW,
    PROCESS_INFORMATION, PROCESS_QUERY_LIMITED_INFORMATION, PROCESS_SYNCHRONIZE, PROCESS_TERMINATE,
    STARTF_FORCEOFFFEEDBACK, STARTUPINFOW,
};

use super::{bound, exit_code, worker, Inner, Worker, WorkerError, WorkerSpec};
use crate::pipe::{self, Conn, Integrity, Peer, PeerRule, PipeError, PipeName, Server};

/// The longest single connection attempt: between two, the spawner looks whether its worker has already ended.
const SLICE: Duration = Duration::from_millis(50);

/// The pipe of (app, role) in this process's session.
fn pipe_name(app: &str, role: &str) -> Result<PipeName, WorkerError> {
    let session = pipe::current_session()?;
    Ok(PipeName::new(app, role, Some(&session.to_string()))?)
}

/// The host's end: the pipe of (app, role), not yet connected. `Pipe(Squatted)` when another process holds the name.
pub(crate) fn serve(app: &str, role: &str) -> Result<Conn, WorkerError> {
    let name = pipe_name(app, role)?;
    let user = pipe::current_user_sid()?;
    Ok(Server::create(&name, &[user])?.into_conn())
}

pub(crate) fn spawn(spec: &WorkerSpec) -> Result<Worker, WorkerError> {
    let deadline = Instant::now().checked_add(spec.deadlines.start);
    let name = pipe_name(spec.app, spec.role)?;
    let own = pipe::current_integrity()?;
    let (process, pid) = start(spec)?;
    let rule = server_rule(&spec.exe, own, Some(pid));
    match connect_checked(&name, &rule, deadline, Some(&process)) {
        Ok((conn, _)) => Ok(worker(spec, Inner::Detached { process, pid, conn })),
        Err(e) => {
            terminate(&process);
            Err(e)
        }
    }
}

pub(crate) fn reconnect(spec: &WorkerSpec) -> Result<Worker, WorkerError> {
    let deadline = Instant::now().checked_add(spec.deadlines.start);
    let name = pipe_name(spec.app, spec.role)?;
    let own = pipe::current_integrity()?;
    let rule = server_rule(&spec.exe, own, None);
    let (conn, peer) = connect_checked(&name, &rule, deadline, None)?;
    let access = PROCESS_TERMINATE | PROCESS_SYNCHRONIZE | PROCESS_QUERY_LIMITED_INFORMATION;
    // SAFETY: plain open of a process by id; the handle returned is owned below.
    let h = unsafe { OpenProcess(access, false, peer.pid) }
        .map_err(|_| WorkerError::Untrusted("the host process cannot be opened".into()))?;
    // SAFETY: a fresh handle, owned by nothing else.
    let process = unsafe { OwnedHandle::from_raw_handle(h.0) };
    Ok(worker(spec, Inner::Detached { process, pid: peer.pid, conn }))
}

/// Connects to `name` before `deadline` and keeps the connection only if its server passes `rule`. While it waits,
/// a `process` the caller started that ends makes it `Exited`.
fn connect_checked(
    name: &PipeName,
    rule: &PeerRule,
    deadline: Option<Instant>,
    process: Option<&OwnedHandle>,
) -> Result<(Conn, Peer), WorkerError> {
    let own_user = pipe::current_user_sid()?;
    loop {
        if let Some(code) = process.and_then(|p| exit_code(HANDLE(p.as_raw_handle()), 0)) {
            return Err(WorkerError::Exited(Some(code)));
        }
        let left = deadline.map_or(SLICE, |d| d.saturating_duration_since(Instant::now()));
        if left.is_zero() {
            return Err(WorkerError::Deadline);
        }
        match pipe::connect(name, left.min(SLICE)) {
            Ok(conn) => {
                let peer = conn.server()?;
                let user = pipe::user_sid_of_pid(peer.pid).map_err(untrusted)?;
                check_server(&peer, &user, &own_user, rule)?;
                return Ok((conn, peer));
            }
            Err(PipeError::Timeout) => {}
            Err(e) => return Err(WorkerError::Pipe(e)),
        }
    }
}

/// What a client accepts as the server of a Detached pipe: `exe`, in this session, at exactly the client's
/// integrity `own` (and the process `pid` when the client has just started it). A host runs at its spawner's level
/// (`start` passes this process's token), so a server below it is a squatter: any process at that level can start
/// the same image and take the predictable name first. Pure.
pub(crate) fn server_rule(exe: &Path, own: Integrity, pid: Option<u32>) -> PeerRule {
    PeerRule {
        image: Some(exe.to_path_buf()),
        same_session: true,
        max_integrity: Some(own),
        min_integrity: Some(own),
        pid,
    }
}

/// What a Detached host accepts as its client: `image`, in this session, at no lower integrity than the host's
/// `own` (an elevated host's pipe has the default Medium label, so the kernel alone lets a medium process of the
/// same user in). Pure.
pub(crate) fn client_rule(image: &Path, own: Integrity) -> PeerRule {
    PeerRule { image: Some(image.to_path_buf()), same_session: true, min_integrity: Some(own), ..PeerRule::default() }
}

/// The client's whole check of a server: `rule`, then the server's token user must be the client's (a squatter's
/// pipe carries a DACL of its own choosing, so only this keeps another account's process out). Pure but for
/// `Conn::check`'s session query.
fn check_server(peer: &Peer, server_user: &str, own_user: &str, rule: &PeerRule) -> Result<(), WorkerError> {
    Conn::check(peer, rule).map_err(untrusted)?;
    if server_user != own_user {
        return Err(WorkerError::Untrusted("another user".into()));
    }
    Ok(())
}

/// A refused peer is `Untrusted`; any other pipe error stays one.
fn untrusted(e: PipeError) -> WorkerError {
    match e {
        PipeError::Peer(why) => WorkerError::Untrusted(why),
        e => WorkerError::Pipe(e),
    }
}

/// Starts `spec.exe` with no window, no stdio and no inherited handle, out of this process's job when the job allows
/// it (as the watchdog is started), else inside it.
fn start(spec: &WorkerSpec) -> Result<(OwnedHandle, u32), WorkerError> {
    let args: Vec<&str> = spec.args.iter().map(String::as_str).collect();
    let line = bound::command_line(&spec.exe, &args).map_err(WorkerError::Spawn)?;
    let app: Vec<u16> = spec.exe.as_os_str().encode_wide().chain(std::iter::once(0)).collect();
    let si = STARTUPINFOW {
        cb: std::mem::size_of::<STARTUPINFOW>() as u32,
        dwFlags: STARTF_FORCEOFFFEEDBACK,
        ..Default::default()
    };
    let attempts = [(CREATE_BREAKAWAY_FROM_JOB | CREATE_NO_WINDOW, true), (CREATE_NO_WINDOW, false)];
    let mut last = None;
    for (flags, breakaway) in attempts {
        let mut line = line.clone();
        let mut pi = PROCESS_INFORMATION::default();
        // SAFETY: `app` and `line` are NUL-terminated and live for the call (`line` mutable, as CreateProcessW wants);
        // `si` and `pi` are valid; no handle is inherited.
        let created = unsafe {
            CreateProcessW(
                PCWSTR(app.as_ptr()),
                Some(PWSTR(line.as_mut_ptr())),
                None,
                None,
                false,
                flags,
                None,
                PCWSTR::null(),
                &si,
                &mut pi,
            )
        };
        match created {
            Ok(()) => {
                // SAFETY: both handles were just returned by CreateProcessW: the thread's is closed once here, the
                // process's is owned by the result.
                let process = unsafe {
                    let _ = CloseHandle(pi.hThread);
                    OwnedHandle::from_raw_handle(pi.hProcess.0)
                };
                return Ok((process, pi.dwProcessId));
            }
            Err(e) if breakaway && e.code() == ERROR_ACCESS_DENIED.to_hresult() => last = Some(e),
            Err(e) => return Err(WorkerError::Spawn(std::io::Error::from_raw_os_error(e.code().0 & 0xFFFF))),
        }
    }
    let code = last.map_or(0, |e| e.code().0 & 0xFFFF);
    Err(WorkerError::Spawn(std::io::Error::from_raw_os_error(code)))
}

/// Ends a worker this spawner started and could not accept, and waits a moment for its end.
fn terminate(process: &OwnedHandle) {
    let h = HANDLE(process.as_raw_handle());
    // SAFETY: the process handle is valid while `process` lives; an ended process refuses, harmlessly.
    let _ = unsafe { TerminateProcess(h, 1) };
    // SAFETY: as above; waiting has no other effect.
    let _ = unsafe { WaitForSingleObject(h, 1000) };
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pipe::Integrity::{High, Low, Medium, System, Untrusted};

    const LEVELS: [Integrity; 5] = [Untrusted, Low, Medium, High, System];

    /// This test process as a peer (its image, session and integrity) and its token user.
    fn me() -> (Peer, String) {
        (pipe::peer_of_pid(std::process::id()).unwrap(), pipe::current_user_sid().unwrap())
    }

    /// A Detached host runs at its spawner's integrity (`start` passes this process's token), so a server above or
    /// below the client is never its worker: below it is a squatter that started the same image.
    #[test]
    fn a_server_runs_at_exactly_the_clients_integrity() {
        let (peer, user) = me();
        let exe = std::env::current_exe().unwrap();
        for own in LEVELS {
            for pid in [None, Some(peer.pid)] {
                let rule = server_rule(&exe, own, pid);
                for level in LEVELS {
                    let got = check_server(&Peer { integrity: level, ..peer.clone() }, &user, &user, &rule);
                    let refused = matches!(got, Err(WorkerError::Untrusted(_)));
                    assert_eq!(refused, level != own, "client {own:?}, server {level:?}: {got:?}");
                    assert_eq!(got.is_ok(), level == own, "client {own:?}, server {level:?}");
                }
            }
        }
    }

    /// The server's pipe carries a DACL of its own choosing, so only the token user keeps another account out.
    #[test]
    fn a_server_of_another_user_is_refused() {
        let (peer, user) = me();
        let rule = server_rule(&std::env::current_exe().unwrap(), peer.integrity, Some(peer.pid));
        assert!(check_server(&peer, &user, &user, &rule).is_ok());
        let other = "S-1-5-21-1000000001-1000000002-1000000003-1001";
        assert!(matches!(check_server(&peer, other, &user, &rule), Err(WorkerError::Untrusted(_))));
    }

    /// A host admits clients of its own integrity or above, never below (an elevated host's pipe has the default
    /// Medium label, so the kernel alone would let a medium process of the same user in).
    #[test]
    fn a_host_admits_no_client_below_itself() {
        let (peer, _) = me();
        let exe = std::env::current_exe().unwrap();
        for own in LEVELS {
            let rule = client_rule(&exe, own);
            for level in LEVELS {
                let got = Conn::check(&Peer { integrity: level, ..peer.clone() }, &rule);
                assert_eq!(got.is_ok(), level >= own, "host {own:?}, client {level:?}: {got:?}");
            }
        }
    }
}
