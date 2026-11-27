//! Named pipes with peer checks (F-P3).
//!
//! One server instance per name: FILE_FLAG_FIRST_PIPE_INSTANCE makes a name that already exists `Squatted`, and one
//! instance at most means nobody adds a second one beside ours. Local clients only (PIPE_REJECT_REMOTE_CLIENTS); a
//! protected DACL naming SYSTEM and the given SIDs, nothing else. Clients connect with identification-level quality
//! of service, so a server never acts with a client's token. Frames are a u32 little-endian length and that many
//! bytes; the reader checks the length against the caller's cap before it allocates. Every read, write and accept
//! is overlapped and bounded: a timeout cancels the operation and waits for the cancellation, so no buffer outlives
//! its call. A peer is known by pid, image (compared by file id: `ident::same_image`), session and integrity.
//!
//! THREADING: a Conn is used by one thread at a time (`&mut self`); Conns share nothing.

use std::os::windows::ffi::OsStringExt;
use std::os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle};
use std::path::PathBuf;
use std::time::{Duration, Instant};

use windows::core::{HSTRING, PCWSTR, PWSTR};
use windows::Win32::Foundation::{
    LocalFree, ERROR_ACCESS_DENIED, ERROR_BROKEN_PIPE, ERROR_FILE_NOT_FOUND, ERROR_IO_PENDING, ERROR_NO_DATA,
    ERROR_OPERATION_ABORTED, ERROR_PIPE_BUSY, ERROR_PIPE_CONNECTED, ERROR_PIPE_NOT_CONNECTED, FILETIME, GENERIC_READ,
    GENERIC_WRITE, HANDLE, HLOCAL, WAIT_OBJECT_0,
};
use windows::Win32::Security::Authorization::ConvertSidToStringSidW;
use windows::Win32::Security::Cryptography::{BCryptGenRandom, BCRYPT_USE_SYSTEM_PREFERRED_RNG};
use windows::Win32::Security::{
    GetSidSubAuthority, GetSidSubAuthorityCount, GetTokenInformation, TokenIntegrityLevel, TokenSessionId, TokenUser,
    PSID, SECURITY_ATTRIBUTES, TOKEN_INFORMATION_CLASS, TOKEN_MANDATORY_LABEL, TOKEN_QUERY, TOKEN_USER,
};
use windows::Win32::Storage::FileSystem::{
    CreateFileW, ReadFile, WriteFile, FILE_FLAG_FIRST_PIPE_INSTANCE, FILE_FLAG_OVERLAPPED, FILE_SHARE_NONE,
    OPEN_EXISTING, PIPE_ACCESS_DUPLEX, SECURITY_IDENTIFICATION, SECURITY_SQOS_PRESENT,
};
use windows::Win32::System::Pipes::{
    ConnectNamedPipe, CreateNamedPipeW, DisconnectNamedPipe, GetNamedPipeClientProcessId, GetNamedPipeServerProcessId,
    WaitNamedPipeW, PIPE_READMODE_BYTE, PIPE_REJECT_REMOTE_CLIENTS, PIPE_TYPE_BYTE, PIPE_WAIT,
};
use windows::Win32::System::Threading::{
    CreateEventW, GetCurrentProcess, GetProcessTimes, OpenProcess, OpenProcessToken, QueryFullProcessImageNameW,
    WaitForSingleObject, INFINITE, PROCESS_NAME_WIN32, PROCESS_QUERY_LIMITED_INFORMATION,
};
use windows::Win32::System::IO::{CancelIoEx, GetOverlappedResult, OVERLAPPED};

use crate::datadir::{is_sid_string, SecDesc, ROOT_NAME};

pub const FRAME_MAX: usize = 4 * 1024 * 1024; // hard ceiling (the agent's largest request is about 2 MiB); each protocol sets its own lower cap

/// The longest part of a pipe name (app, purpose, nonce).
const PART_MAX: usize = 32;
/// The pipe's buffer sizes (advisory: a larger frame takes several reads).
const PIPE_BUFFER: u32 = 64 * 1024;
/// How long a send waits for the peer to take its bytes: a peer that reads nothing for this long is broken.
const SEND_TIMEOUT: Duration = Duration::from_secs(10);
/// The pause between two connection attempts while the server's name does not exist yet.
const CONNECT_RETRY: Duration = Duration::from_millis(10);
/// Mandatory-label RIDs (winnt.h): the lower bound of each integrity level.
const LOW_RID: u32 = 0x1000;
const MEDIUM_RID: u32 = 0x2000;
const HIGH_RID: u32 = 0x3000;
const SYSTEM_RID: u32 = 0x4000;

pub struct PipeName(String);
impl PipeName {
    pub fn new(app: &str, purpose: &str, nonce: Option<&str>) -> Result<PipeName, PipeError> {
        if !is_name_part(app) || !is_name_part(purpose) || !nonce.is_none_or(is_name_part) {
            return Err(PipeError::Name);
        }
        let mut name = format!("{ROOT_NAME}.{app}.{purpose}");
        if let Some(nonce) = nonce {
            name.push('.');
            name.push_str(nonce);
        }
        Ok(PipeName(name))
    } // ascii [a-z0-9-], each ≤ 32
    pub fn full(&self) -> String {
        format!(r"\\.\pipe\{}", self.0)
    }
} // \\.\pipe\mini-oled-dock.<app>.<purpose>[.<nonce>]

/// One part of a pipe or event name: 1 to 32 of [a-z0-9-]. Pure.
pub(crate) fn is_name_part(s: &str) -> bool {
    !s.is_empty() && s.len() <= PART_MAX && s.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
}

pub fn nonce() -> String {
    let mut bytes = [0u8; 16];
    // SAFETY: `bytes` is writable for its whole length; no algorithm handle with the system-preferred RNG flag.
    let status = unsafe { BCryptGenRandom(None, &mut bytes, BCRYPT_USE_SYSTEM_PREFERRED_RNG) };
    if status.is_err() {
        fallback_random(&mut bytes);
    }
    bytes.iter().map(|b| format!("{b:02x}")).collect()
} // 32 lowercase hex chars from BCryptGenRandom

/// Random bytes when BCryptGenRandom fails (it does not on a working system): std's hasher keys, which the
/// standard library seeds from the OS once per thread, mixed with the clock and the pid.
fn fallback_random(out: &mut [u8; 16]) {
    use std::hash::{BuildHasher, Hasher};
    let clock = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_nanos());
    for (i, chunk) in out.chunks_mut(8).enumerate() {
        let mut h = std::collections::hash_map::RandomState::new().build_hasher();
        h.write_usize(i);
        h.write_u128(clock);
        h.write_u32(std::process::id());
        for (b, r) in chunk.iter_mut().zip(h.finish().to_le_bytes()) {
            *b = r;
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Integrity {
    Untrusted,
    Low,
    Medium,
    High,
    System,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Peer {
    pub pid: u32,
    pub image: std::path::PathBuf,
    pub session: u32,
    pub integrity: Integrity,
    pub created: u64,
}
#[derive(Clone, Debug, Default)]
pub struct PeerRule {
    pub image: Option<std::path::PathBuf>,
    pub same_session: bool,
    pub max_integrity: Option<Integrity>,
    pub min_integrity: Option<Integrity>,
    pub pid: Option<u32>,
}
#[derive(Debug)]
pub enum PipeError {
    Name,
    Squatted,
    Timeout,
    Closed,
    TooLarge(usize),
    Peer(String),
    Io(std::io::Error),
}

impl std::fmt::Display for PipeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            PipeError::Name => f.write_str("not a valid pipe or event name"),
            PipeError::Squatted => f.write_str("the name is already taken"),
            PipeError::Timeout => f.write_str("timed out"),
            PipeError::Closed => f.write_str("the other end closed the connection"),
            PipeError::TooLarge(n) => write!(f, "a frame of {n} bytes is over the cap"),
            PipeError::Peer(why) => write!(f, "peer refused: {why}"),
            PipeError::Io(e) => write!(f, "{e}"),
        }
    }
}

/// A Win32 error as a PipeError: the ends of a connection are `Closed`, the rest `Io`.
fn pipe_error(e: &windows::core::Error) -> PipeError {
    let closed = [ERROR_BROKEN_PIPE, ERROR_NO_DATA, ERROR_PIPE_NOT_CONNECTED];
    if closed.iter().any(|c| e.code() == c.to_hresult()) {
        PipeError::Closed
    } else {
        PipeError::Io(std::io::Error::from_raw_os_error(e.code().0 & 0xFFFF))
    }
}

/// The handle of an OwnedHandle, for calls that take a HANDLE.
fn raw(h: &OwnedHandle) -> HANDLE {
    HANDLE(h.as_raw_handle())
}

/// A fresh manual-reset event, not signaled, unnamed: the completion event of one Conn's overlapped calls.
fn manual_event() -> Result<OwnedHandle, PipeError> {
    // SAFETY: no attributes and no name; the handle returned is owned by the caller.
    let h = unsafe { CreateEventW(None, true, false, PCWSTR::null()) }.map_err(|e| pipe_error(&e))?;
    // SAFETY: a fresh handle, owned by nothing else.
    Ok(unsafe { OwnedHandle::from_raw_handle(h.0) })
}

/// The protected DACL of a server: SYSTEM and each SID with read and write, nothing else; None if a SID is not one.
pub(crate) fn server_sddl(allow_sids: &[String]) -> Option<String> {
    let mut sddl = String::from("D:P(A;;GRGW;;;SY)");
    for sid in allow_sids {
        if !is_sid_string(sid) {
            return None;
        }
        sddl.push_str(&format!("(A;;GRGW;;;{sid})"));
    }
    Some(sddl)
}

pub struct Server {
    pipe: OwnedHandle,
    event: OwnedHandle,
}
impl Server {
    pub fn create(name: &PipeName, allow_sids: &[String]) -> Result<Server, PipeError> {
        let sddl = server_sddl(allow_sids).ok_or(PipeError::Name)?;
        let sd = SecDesc::from_sddl(&sddl).map_err(|e| PipeError::Io(std::io::Error::other(e)))?;
        let sa = SECURITY_ATTRIBUTES {
            nLength: std::mem::size_of::<SECURITY_ATTRIBUTES>() as u32,
            lpSecurityDescriptor: sd.as_ptr().0,
            bInheritHandle: false.into(),
        };
        let event = manual_event()?;
        let full = HSTRING::from(name.full());
        // SAFETY: `full` is NUL-terminated and `sa` (with the descriptor `sd` owns) lives for the call.
        let pipe = unsafe {
            CreateNamedPipeW(
                &full,
                FILE_FLAG_FIRST_PIPE_INSTANCE | FILE_FLAG_OVERLAPPED | PIPE_ACCESS_DUPLEX,
                PIPE_TYPE_BYTE | PIPE_READMODE_BYTE | PIPE_WAIT | PIPE_REJECT_REMOTE_CLIENTS,
                1,
                PIPE_BUFFER,
                PIPE_BUFFER,
                0,
                Some(&sa),
            )
        };
        if pipe.is_invalid() {
            let e = std::io::Error::last_os_error();
            let taken = [ERROR_ACCESS_DENIED.0, ERROR_PIPE_BUSY.0].map(|c| c as i32);
            return Err(if e.raw_os_error().is_some_and(|c| taken.contains(&c)) {
                PipeError::Squatted
            } else {
                PipeError::Io(e)
            });
        }
        // SAFETY: a fresh pipe handle, owned by nothing else.
        let pipe = unsafe { OwnedHandle::from_raw_handle(pipe.0) };
        Ok(Server { pipe, event })
    }
    //   FILE_FLAG_FIRST_PIPE_INSTANCE | FILE_FLAG_OVERLAPPED | PIPE_ACCESS_DUPLEX, PIPE_TYPE_BYTE | PIPE_REJECT_REMOTE_CLIENTS, one instance,
    //   protected DACL: SYSTEM and each listed SID GENERIC_READ | GENERIC_WRITE, nothing else; an existing name is Squatted
    pub fn accept(self, timeout: std::time::Duration) -> Result<Conn, PipeError> {
        let mut conn = self.into_conn();
        conn.wait_client(Instant::now().checked_add(timeout))?;
        Ok(conn)
    }

    /// The instance as a connection not yet accepted: a host that serves one client after another waits with
    /// `Conn::wait_client` and lets each go with `Conn::disconnect`, never releasing the name.
    pub(crate) fn into_conn(self) -> Conn {
        Conn::new(self.pipe, self.event)
    }
}
pub fn connect(name: &PipeName, timeout: std::time::Duration) -> Result<Conn, PipeError> {
    let deadline = Instant::now().checked_add(timeout);
    let full = HSTRING::from(name.full());
    loop {
        // SAFETY: `full` is NUL-terminated; no security attributes or template. Identification-level quality of
        // service: the server can learn who connects but never act as this process.
        let opened = unsafe {
            CreateFileW(
                &full,
                (GENERIC_READ | GENERIC_WRITE).0,
                FILE_SHARE_NONE,
                None,
                OPEN_EXISTING,
                FILE_FLAG_OVERLAPPED | SECURITY_SQOS_PRESENT | SECURITY_IDENTIFICATION,
                None,
            )
        };
        let left = deadline.map(|d| d.saturating_duration_since(Instant::now()));
        match opened {
            // SAFETY: a fresh pipe handle, owned by nothing else.
            Ok(h) => return Ok(Conn::new(unsafe { OwnedHandle::from_raw_handle(h.0) }, manual_event()?)),
            Err(_) if left.is_some_and(|l| l.is_zero()) => return Err(PipeError::Timeout),
            Err(e) if e.code() == ERROR_PIPE_BUSY.to_hresult() => {
                let ms = left.map_or(INFINITE - 1, |l| millis(l).min(50));
                // SAFETY: `full` is NUL-terminated. A false return (time out, no pipe) only means: try again.
                let _ = unsafe { WaitNamedPipeW(&full, ms) };
            }
            Err(e) if e.code() == ERROR_FILE_NOT_FOUND.to_hresult() => {
                std::thread::sleep(left.map_or(CONNECT_RETRY, |l| l.min(CONNECT_RETRY)));
            }
            Err(e) => return Err(pipe_error(&e)),
        }
    }
}

/// Whole milliseconds of `d`, rounded up, below INFINITE.
fn millis(d: Duration) -> u32 {
    u32::try_from(d.as_micros().div_ceil(1000)).unwrap_or(INFINITE - 1).min(INFINITE - 1)
}

/// Runs one overlapped operation on `h` with `event` and waits for its end until `deadline` (None: no limit). At the
/// deadline the operation is cancelled and its end awaited, so neither `ov` nor the buffer it uses is touched after
/// this returns. Returns the bytes moved (also those of a cancelled operation) and the outcome.
fn overlapped(
    h: HANDLE,
    event: HANDLE,
    deadline: Option<Instant>,
    start: impl FnOnce(*mut OVERLAPPED) -> windows::core::Result<()>,
) -> (u32, Result<(), PipeError>) {
    let mut ov = OVERLAPPED { hEvent: event, ..Default::default() };
    match start(&mut ov) {
        Ok(()) => {}
        Err(e) if e.code() == ERROR_IO_PENDING.to_hresult() => {}
        Err(e) => return (0, Err(pipe_error(&e))),
    }
    let ms = deadline.map_or(INFINITE, |d| millis(d.saturating_duration_since(Instant::now())));
    // SAFETY: `event` is a valid event handle the caller owns.
    let waited = unsafe { WaitForSingleObject(event, ms) };
    let cancelled = waited != WAIT_OBJECT_0;
    if cancelled {
        // SAFETY: `ov` is the OVERLAPPED of the operation started on `h` above; cancelling one that ended is harmless.
        let _ = unsafe { CancelIoEx(h, Some(&ov)) };
    }
    let mut moved = 0u32;
    // SAFETY: `ov` lives until this call has seen the operation end (bWait = true); `moved` is writable.
    let ended = unsafe { GetOverlappedResult(h, &ov, &mut moved, true) };
    match ended {
        Ok(()) => (moved, Ok(())),
        Err(e) if cancelled && e.code() == ERROR_OPERATION_ABORTED.to_hresult() => (moved, Err(PipeError::Timeout)),
        Err(e) => (moved, Err(pipe_error(&e))),
    }
}

/// The frame decoder: a u32 little-endian length, then that many bytes. It hands out the buffer the next read
/// fills (`space`) and takes the count read (`advance`), so a read that ends in a timeout keeps its bytes and the
/// next receive resumes. The body is allocated only once its length passed the cap. Total: any byte sequence, cut
/// anywhere, gives frames, TooLarge or "not yet".
#[derive(Default)]
pub(crate) struct FrameReader {
    head: [u8; 4],
    head_got: usize,
    body: Option<Vec<u8>>,
    body_got: usize,
}

impl FrameReader {
    /// The bytes the next read may fill: the rest of the length prefix, then the rest of the body.
    pub(crate) fn space(&mut self) -> &mut [u8] {
        match &mut self.body {
            Some(body) => body.get_mut(self.body_got..).unwrap_or_default(),
            None => self.head.get_mut(self.head_got..).unwrap_or_default(),
        }
    }

    /// Takes `n` bytes just read into `space()`: Some(frame) once a frame is whole. TooLarge(len) when the prefix
    /// names more than `cap` (or FRAME_MAX) bytes; the reader is then empty again and nothing was allocated for it.
    pub(crate) fn advance(&mut self, n: usize, cap: usize) -> Result<Option<Vec<u8>>, PipeError> {
        match &self.body {
            None => {
                self.head_got = self.head_got.saturating_add(n).min(self.head.len());
                if self.head_got < self.head.len() {
                    return Ok(None);
                }
                let len = u32::from_le_bytes(self.head) as usize;
                *self = FrameReader::default();
                if len > cap.min(FRAME_MAX) {
                    return Err(PipeError::TooLarge(len));
                }
                self.body = Some(vec![0; len]);
            }
            Some(body) => self.body_got = self.body_got.saturating_add(n).min(body.len()),
        }
        match &self.body {
            Some(body) if self.body_got == body.len() => {
                let frame = self.body.take().unwrap_or_default();
                *self = FrameReader::default();
                Ok(Some(frame))
            }
            _ => Ok(None),
        }
    }

    /// The bytes held for a frame being read (its body's allocation): never above the cap it was read with.
    #[cfg(test)]
    pub(crate) fn held(&self) -> usize {
        self.body.as_ref().map_or(0, Vec::capacity)
    }
}

pub struct Conn {
    pipe: OwnedHandle,
    event: OwnedHandle,
    frame: FrameReader,
    /// A frame was cut (an error mid-frame, a frame over the cap, a failed send): the stream cannot be trusted again.
    broken: bool,
}
impl Conn {
    fn new(pipe: OwnedHandle, event: OwnedHandle) -> Conn {
        Conn { pipe, event, frame: FrameReader::default(), broken: false }
    }

    pub fn client(&self) -> Result<Peer, PipeError> {
        let mut pid = 0u32;
        // SAFETY: the pipe handle is valid while `self` lives; `pid` is writable.
        unsafe { GetNamedPipeClientProcessId(raw(&self.pipe), &mut pid) }.map_err(|e| pipe_error(&e))?;
        peer_of_pid(pid)
    } // GetNamedPipeClientProcessId + peer_of_pid
    pub fn server(&self) -> Result<Peer, PipeError> {
        let mut pid = 0u32;
        // SAFETY: the pipe handle is valid while `self` lives; `pid` is writable.
        unsafe { GetNamedPipeServerProcessId(raw(&self.pipe), &mut pid) }.map_err(|e| pipe_error(&e))?;
        peer_of_pid(pid)
    } // GetNamedPipeServerProcessId + peer_of_pid
    pub fn check(peer: &Peer, rule: &PeerRule) -> Result<(), PipeError> {
        let refuse = |why: &str| Err(PipeError::Peer(why.into()));
        if rule.pid.is_some_and(|pid| pid != peer.pid) {
            return refuse("another process");
        }
        if rule.same_session && peer.session != current_session()? {
            return refuse("another session");
        }
        if rule.max_integrity.is_some_and(|max| peer.integrity > max) {
            return refuse("integrity above the allowed level");
        }
        if rule.min_integrity.is_some_and(|min| peer.integrity < min) {
            return refuse("integrity below the required level");
        }
        if rule.image.as_ref().is_some_and(|image| !crate::ident::same_image(&peer.image, image)) {
            return refuse("another image");
        }
        Ok(())
    }
    pub fn send(&mut self, frame: &[u8]) -> Result<(), PipeError> {
        let len = u32::try_from(frame.len()).ok().filter(|&l| l as usize <= FRAME_MAX);
        let Some(len) = len else { return Err(PipeError::TooLarge(frame.len())) };
        if self.broken {
            return Err(PipeError::Closed);
        }
        let deadline = Instant::now().checked_add(SEND_TIMEOUT);
        let sent = self.write_all(&len.to_le_bytes(), deadline).and_then(|()| self.write_all(frame, deadline));
        if sent.is_err() {
            self.broken = true;
        }
        sent
    } // u32 LE length, then the bytes
    pub fn recv(&mut self, cap: usize, timeout: Option<std::time::Duration>) -> Result<Vec<u8>, PipeError> {
        if self.broken {
            return Err(PipeError::Closed);
        }
        let deadline = timeout.and_then(|t| Instant::now().checked_add(t));
        let (pipe, event) = (raw(&self.pipe), raw(&self.event));
        loop {
            let space = self.frame.space();
            // SAFETY: `space` is writable and outlives the operation, which `overlapped` waits out.
            let (n, read) =
                overlapped(pipe, event, deadline, |ov| unsafe { ReadFile(pipe, Some(space), None, Some(ov)) });
            match (self.frame.advance(n as usize, cap), read) {
                (Ok(Some(frame)), _) => return Ok(frame),
                (Ok(None), Ok(())) => {}
                // The bytes read so far stay in the decoder: the next receive resumes the frame.
                (Ok(None), Err(PipeError::Timeout)) => return Err(PipeError::Timeout),
                (Ok(None), Err(e)) | (Err(e), _) => {
                    self.broken = true;
                    return Err(e);
                }
            }
        }
    }

    /// Writes all of `buf` before `deadline`.
    fn write_all(&mut self, mut buf: &[u8], deadline: Option<Instant>) -> Result<(), PipeError> {
        let (pipe, event) = (raw(&self.pipe), raw(&self.event));
        while !buf.is_empty() {
            let chunk = buf;
            // SAFETY: `chunk` is readable and outlives the operation, which `overlapped` waits out.
            let (n, written) =
                overlapped(pipe, event, deadline, |ov| unsafe { WriteFile(pipe, Some(chunk), None, Some(ov)) });
            written?;
            if n == 0 {
                return Err(PipeError::Closed);
            }
            buf = buf.get(n as usize..).unwrap_or_default();
        }
        Ok(())
    }

    /// Server side: waits for a client to connect, until `deadline` (None: no limit).
    pub(crate) fn wait_client(&mut self, deadline: Option<Instant>) -> Result<(), PipeError> {
        let pipe = raw(&self.pipe);
        // SAFETY: the pipe handle and `ov` are valid for the operation, which `overlapped` waits out.
        let (_, connected) =
            overlapped(pipe, raw(&self.event), deadline, |ov| unsafe { ConnectNamedPipe(pipe, Some(ov)) });
        match connected {
            Err(PipeError::Io(e)) if e.raw_os_error() == Some(ERROR_PIPE_CONNECTED.0 as i32) => Ok(()),
            other => other,
        }
    }

    /// Server side: drops the current client, so the same instance can wait for the next one (the name is never
    /// released in between, so nobody can take it).
    pub(crate) fn disconnect(&mut self) {
        // SAFETY: the pipe handle is valid while `self` lives; disconnecting an idle instance is harmless.
        let _ = unsafe { DisconnectNamedPipe(raw(&self.pipe)) };
        self.frame = FrameReader::default();
        self.broken = false;
    }
}

pub fn peer_of_pid(pid: u32) -> Result<Peer, PipeError> {
    let process = open_process(pid)?;
    let image = process_image(&process).ok_or_else(|| PipeError::Peer(format!("no image path for pid {pid}")))?;
    let token = open_token(&process)?;
    let session = token_session(&token)?;
    let integrity = token_integrity(&token)?;
    let mut times = [FILETIME::default(); 4];
    let [created, exited, kernel, user] = &mut times;
    // SAFETY: the process handle is valid while `process` lives; the four FILETIMEs are writable.
    unsafe { GetProcessTimes(raw(&process), created, exited, kernel, user) }.map_err(|e| pipe_error(&e))?;
    let created = (u64::from(times[0].dwHighDateTime) << 32) | u64::from(times[0].dwLowDateTime);
    Ok(Peer { pid, image, session, integrity, created })
} // QueryFullProcessImageNameW (final path), ProcessIdToSessionId, token integrity, GetProcessTimes
pub fn user_sid_of_pid(pid: u32) -> Result<String, PipeError> {
    let token = open_token(&open_process(pid)?)?;
    token_user_sid(&token)
} // the token user of that process (Explorer's: the real user under Administrator protection)
pub fn current_user_sid() -> Result<String, PipeError> {
    token_user_sid(&own_token()?)
}

/// The session this process runs in (the token's, as ProcessIdToSessionId reads it).
pub(crate) fn current_session() -> Result<u32, PipeError> {
    token_session(&own_token()?)
}
/// The integrity level of this process.
pub(crate) fn current_integrity() -> Result<Integrity, PipeError> {
    token_integrity(&own_token()?)
}

/// A process handle that may query names, times and the token (PROCESS_QUERY_LIMITED_INFORMATION).
fn open_process(pid: u32) -> Result<OwnedHandle, PipeError> {
    // SAFETY: plain open of a process by id; the handle returned is owned by the caller.
    let h = unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid) }
        .map_err(|_| PipeError::Peer(format!("cannot open pid {pid}")))?;
    // SAFETY: a fresh handle, owned by nothing else.
    Ok(unsafe { OwnedHandle::from_raw_handle(h.0) })
}

/// The Win32 path of a process's image (QueryFullProcessImageNameW), None when it cannot be read.
fn process_image(process: &OwnedHandle) -> Option<PathBuf> {
    for size in [1024usize, 32 * 1024] {
        let mut buf = vec![0u16; size];
        let mut len = size as u32;
        // SAFETY: `buf` holds `len` writable UTF-16 units; the handle is valid while `process` lives.
        let read =
            unsafe { QueryFullProcessImageNameW(raw(process), PROCESS_NAME_WIN32, PWSTR(buf.as_mut_ptr()), &mut len) };
        if read.is_ok() {
            let path = buf.get(..len as usize)?;
            return Some(PathBuf::from(std::ffi::OsString::from_wide(path)));
        }
    }
    None
}

/// The Win32 path of the image of process `pid`, None when it cannot be read (no such process, no access).
pub(crate) fn image_of_pid(pid: u32) -> Option<PathBuf> {
    process_image(&open_process(pid).ok()?)
}

fn open_token(process: &OwnedHandle) -> Result<OwnedHandle, PipeError> {
    let mut token = HANDLE::default();
    // SAFETY: the process handle is valid while `process` lives; `token` is writable.
    unsafe { OpenProcessToken(raw(process), TOKEN_QUERY, &mut token) }
        .map_err(|_| PipeError::Peer("cannot read the token of the peer".into()))?;
    // SAFETY: a fresh token handle, owned by nothing else.
    Ok(unsafe { OwnedHandle::from_raw_handle(token.0) })
}

fn own_token() -> Result<OwnedHandle, PipeError> {
    let mut token = HANDLE::default();
    // SAFETY: the pseudo-handle of this process; `token` is writable.
    unsafe { OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token) }.map_err(|e| pipe_error(&e))?;
    // SAFETY: a fresh token handle, owned by nothing else.
    Ok(unsafe { OwnedHandle::from_raw_handle(token.0) })
}

/// One token information class into a pointer-aligned buffer of the size the token asks for (at most 4 KiB).
fn token_info(token: &OwnedHandle, class: TOKEN_INFORMATION_CLASS) -> Result<Vec<u64>, PipeError> {
    let mut len = 0u32;
    // SAFETY: a size query: no buffer, `len` writable; the expected "buffer too small" error is ignored.
    let _ = unsafe { GetTokenInformation(raw(token), class, None, 0, &mut len) };
    if len == 0 || len > 4096 {
        return Err(PipeError::Peer("unreadable token information".into()));
    }
    let mut buf = vec![0u64; (len as usize).div_ceil(8)];
    // SAFETY: `buf` holds at least `len` writable, pointer-aligned bytes.
    unsafe { GetTokenInformation(raw(token), class, Some(buf.as_mut_ptr().cast()), len, &mut len) }
        .map_err(|e| pipe_error(&e))?;
    Ok(buf)
}

fn token_session(token: &OwnedHandle) -> Result<u32, PipeError> {
    let mut session = 0u32;
    let mut len = 0u32;
    // SAFETY: `session` is a writable u32, the size this class returns.
    unsafe {
        GetTokenInformation(raw(token), TokenSessionId, Some(std::ptr::from_mut(&mut session).cast()), 4, &mut len)
    }
    .map_err(|e| pipe_error(&e))?;
    Ok(session)
}

/// The integrity level of a token, from the last sub-authority of its mandatory label (the tray host's check,
/// generalised to every level).
fn token_integrity(token: &OwnedHandle) -> Result<Integrity, PipeError> {
    let buf = token_info(token, TokenIntegrityLevel)?;
    // SAFETY: the call succeeded, so `buf` starts with a TOKEN_MANDATORY_LABEL whose SID points inside `buf`
    // (alive here); a valid SID's count and sub-authorities are readable.
    let rid = unsafe {
        let label = &*buf.as_ptr().cast::<TOKEN_MANDATORY_LABEL>();
        let sid = label.Label.Sid;
        let count = *GetSidSubAuthorityCount(sid);
        if count == 0 {
            return Err(PipeError::Peer("a mandatory label without a level".into()));
        }
        *GetSidSubAuthority(sid, u32::from(count) - 1)
    };
    Ok(integrity_of_rid(rid))
}

/// The level of a mandatory-label RID: each level covers the RIDs from its own up to the next one's. Pure.
pub(crate) fn integrity_of_rid(rid: u32) -> Integrity {
    match rid {
        r if r >= SYSTEM_RID => Integrity::System,
        r if r >= HIGH_RID => Integrity::High,
        r if r >= MEDIUM_RID => Integrity::Medium,
        r if r >= LOW_RID => Integrity::Low,
        _ => Integrity::Untrusted,
    }
}

fn token_user_sid(token: &OwnedHandle) -> Result<String, PipeError> {
    let buf = token_info(token, TokenUser)?;
    // SAFETY: the call succeeded, so `buf` starts with a TOKEN_USER whose SID points inside `buf` (alive here).
    let sid = unsafe { (*buf.as_ptr().cast::<TOKEN_USER>()).User.Sid };
    sid_string(sid).ok_or_else(|| PipeError::Peer("the token user has no string form".into()))
}

/// The string form of a SID (S-1-...).
fn sid_string(sid: PSID) -> Option<String> {
    let mut s = PWSTR::null();
    // SAFETY: `sid` points to a valid SID; `s` receives a LocalAlloc'd string freed below.
    unsafe { ConvertSidToStringSidW(sid, &mut s) }.ok()?;
    // SAFETY: `s` is the NUL-terminated string the call returned.
    let out = unsafe { s.to_string() }.ok();
    // SAFETY: allocated by ConvertSidToStringSidW with LocalAlloc; not used afterwards.
    unsafe { LocalFree(Some(HLOCAL(s.0.cast()))) };
    out
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    /// SplitMix64: the fuzz tests' generator (no dependency; the same cases on every run).
    pub(crate) struct Rng(pub(crate) u64);
    impl Rng {
        pub(crate) fn next(&mut self) -> u64 {
            self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
            let mut z = self.0;
            z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
            z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
            z ^ (z >> 31)
        }
        pub(crate) fn below(&mut self, n: usize) -> usize {
            if n == 0 {
                0
            } else {
                (self.next() % n as u64) as usize
            }
        }
    }

    /// What a stream decodes to by the plain rule: frames until the input ends or a length is over the cap.
    pub(crate) fn reference(stream: &[u8], cap: usize) -> (Vec<Vec<u8>>, bool) {
        let mut frames = Vec::new();
        let mut at = 0;
        loop {
            let Some(head) = stream.get(at..at + 4) else { return (frames, false) };
            let len = u32::from_le_bytes(head.try_into().unwrap()) as usize;
            if len > cap.min(FRAME_MAX) {
                return (frames, true);
            }
            let Some(body) = stream.get(at + 4..at + 4 + len) else { return (frames, false) };
            frames.push(body.to_vec());
            at += 4 + len;
        }
    }

    /// A few frames whose lengths are in range, at the cap, one over it, huge or random; some bodies cut short.
    pub(crate) fn random_frames(rng: &mut Rng, cap: usize) -> Vec<u8> {
        let mut s = Vec::new();
        for _ in 0..=rng.below(4) {
            let len = match rng.below(7) {
                0 => 0,
                1 => cap,
                2 => cap + 1,
                3 => u32::MAX as usize,
                4 => rng.next() as u32 as usize,
                _ => rng.below(cap + 1),
            };
            s.extend_from_slice(&(len as u32).to_le_bytes());
            let body = len.min(cap + 8);
            let body = if rng.below(5) == 0 { rng.below(body + 1) } else { body };
            s.extend((0..body).map(|_| rng.next() as u8));
        }
        if rng.below(4) == 0 {
            s.truncate(rng.below(s.len() + 1));
        }
        s
    }

    /// A cap from tiny to above the pipe's buffer.
    pub(crate) fn random_cap(rng: &mut Rng) -> usize {
        match rng.below(4) {
            0 => rng.below(16),
            1 => rng.below(300),
            2 => rng.below(5000),
            _ => rng.below(70_000),
        }
    }

    /// Feeds `stream` through the decoder as `recv` does, cut at random (and with empty reads, as a timeout gives).
    fn decode(rng: &mut Rng, stream: &[u8], cap: usize) -> (Vec<Vec<u8>>, bool) {
        let mut r = FrameReader::default();
        let mut frames = Vec::new();
        let mut at = 0;
        while at < stream.len() {
            let space = r.space();
            assert!(!space.is_empty());
            let n = if rng.below(16) == 0 { 0 } else { space.len().min(stream.len() - at).min(1 + rng.below(9000)) };
            space[..n].copy_from_slice(&stream[at..at + n]);
            at += n;
            match r.advance(n, cap) {
                Ok(Some(frame)) => frames.push(frame),
                Ok(None) => {}
                Err(PipeError::TooLarge(len)) => {
                    assert!(len > cap.min(FRAME_MAX));
                    assert_eq!(r.held(), 0, "nothing is allocated for a refused frame");
                    return (frames, true);
                }
                Err(e) => panic!("{e:?}"),
            }
            assert!(r.held() <= cap, "{} bytes held over the cap {cap}", r.held());
        }
        (frames, false)
    }

    fn fuzz_frames(cases: u32, seed: u64) {
        let mut rng = Rng(seed);
        for _ in 0..cases {
            let cap = random_cap(&mut rng);
            let stream = random_frames(&mut rng, cap);
            assert_eq!(decode(&mut rng, &stream, cap), reference(&stream, cap));
        }
    }

    #[test]
    fn fuzz_pipe_frames_10k() {
        fuzz_frames(10_000, 1);
    }

    #[test]
    #[ignore = "nightly: 1,000,000 inputs"]
    fn fuzz_pipe_frames_1m() {
        fuzz_frames(1_000_000, 2);
    }

    #[test]
    fn integrity_levels_follow_the_label_rids() {
        let levels = [0, 0x1000, 0x1fff, 0x2000, 0x2100, 0x3000, 0x4000, 0x5000].map(integrity_of_rid);
        use Integrity::*;
        assert_eq!(levels, [Untrusted, Low, Low, Medium, Medium, High, System, System]);
    }

    #[test]
    fn the_dacl_names_only_system_and_the_given_sid() {
        use windows::Win32::Security::Authorization::{GetSecurityInfo, SE_KERNEL_OBJECT};
        use windows::Win32::Security::{
            GetSecurityDescriptorControl, ACL, DACL_SECURITY_INFORMATION, PSECURITY_DESCRIPTOR, SE_DACL_PROTECTED,
        };
        let sid = current_user_sid().unwrap();
        let name = PipeName::new("oled-test", "dacl", Some(&nonce())).unwrap();
        let server = Server::create(&name, std::slice::from_ref(&sid)).unwrap();
        let dacl = crate::datadir::read_security(raw(&server.pipe)).unwrap().dacl.unwrap();
        let sids: Vec<String> = dacl.iter().map(|a| a.sid.clone().unwrap_or_default()).collect();
        assert_eq!(sids, [crate::datadir::SID_SYSTEM.to_string(), sid]);
        assert!(dacl.iter().all(|a| !a.deny));
        // Read and write only: never DELETE, WRITE_DAC or WRITE_OWNER.
        assert!(dacl.iter().all(|a| a.mask & 0x000D_0000 == 0), "{dacl:?}");
        let mut sd = PSECURITY_DESCRIPTOR::default();
        let mut acl: *mut ACL = std::ptr::null_mut();
        // SAFETY: the pipe handle is valid while `server` lives; the out-params are writable; `sd` is freed below.
        let err = unsafe {
            GetSecurityInfo(
                raw(&server.pipe),
                SE_KERNEL_OBJECT,
                DACL_SECURITY_INFORMATION,
                None,
                None,
                Some(&mut acl),
                None,
                Some(&mut sd),
            )
        };
        assert_eq!(err.0, 0);
        let (mut control, mut revision) = (0u16, 0u32);
        // SAFETY: `sd` is the descriptor GetSecurityInfo returned.
        let read = unsafe { GetSecurityDescriptorControl(sd, &mut control, &mut revision) };
        // SAFETY: allocated by GetSecurityInfo with LocalAlloc; not used afterwards.
        unsafe { LocalFree(Some(HLOCAL(sd.0))) };
        read.unwrap();
        assert_ne!(control & SE_DACL_PROTECTED.0, 0, "protected: nothing inherited");
        assert!(server_sddl(&["x)(A;;GA;;;WD".to_string()]).is_none(), "only a SID is spliced into the SDDL");
    }
}
