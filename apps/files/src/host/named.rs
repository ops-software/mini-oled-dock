//! The ops pipe (OP5a): the server the ops host serves and the client transport, built to F-P3's wire and rules so
//! SW1's swap to oled_sys::pipe changes no byte: one instance per name (FILE_FLAG_FIRST_PIPE_INSTANCE: a name that
//! exists is Squatted, which the host maps to exit 4), local clients only (PIPE_REJECT_REMOTE_CLIENTS), a protected
//! DACL naming the user and SYSTEM, clients connecting at identification level. Each message is a u32 LE length and
//! that many bytes, one frame of the files protocols; a length over OPS.max_payload + HEADER_LEN is refused before
//! anything is allocated and poisons the connection.
//!
//! After ConnectNamedPipe the server checks its client before any frame moves: the same session, an integrity at
//! least its own (a lower process may run the same image) and the host's own image (peer_ok; a debug build in test
//! mode also accepts any image of the same cargo profile folder, the test executables and probes; a probe build,
//! OPS_PROBE_BUILD, also the ops probe beside it).
//!
//! A connection is full duplex: one thread reads while another writes, each with its own OVERLAPPED and event.

use std::io;
use std::os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{channel, Receiver, Sender};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

use oled_sys::pipe::{peer_of_pid, Conn, Integrity, Peer, PeerRule, PipeName};
use oled_trust::frame::files::OPS;
use oled_trust::frame::HEADER_LEN;
use windows::core::{HSTRING, PCWSTR};
use windows::Win32::Foundation::{
    LocalFree, ERROR_ACCESS_DENIED, ERROR_BROKEN_PIPE, ERROR_FILE_NOT_FOUND, ERROR_IO_PENDING, ERROR_NO_DATA,
    ERROR_OPERATION_ABORTED, ERROR_PIPE_BUSY, ERROR_PIPE_CONNECTED, ERROR_PIPE_NOT_CONNECTED, GENERIC_READ,
    GENERIC_WRITE, HANDLE, HLOCAL, WAIT_OBJECT_0,
};
use windows::Win32::Security::Authorization::{ConvertStringSecurityDescriptorToSecurityDescriptorW, SDDL_REVISION_1};
use windows::Win32::Security::{PSECURITY_DESCRIPTOR, SECURITY_ATTRIBUTES};
use windows::Win32::Storage::FileSystem::{
    CreateFileW, ReadFile, WriteFile, FILE_FLAG_FIRST_PIPE_INSTANCE, FILE_FLAG_OVERLAPPED, FILE_SHARE_NONE,
    OPEN_EXISTING, PIPE_ACCESS_DUPLEX, SECURITY_IDENTIFICATION, SECURITY_SQOS_PRESENT,
};
use windows::Win32::System::Pipes::{
    ConnectNamedPipe, CreateNamedPipeW, DisconnectNamedPipe, GetNamedPipeClientProcessId, GetNamedPipeServerProcessId,
    WaitNamedPipeW, PIPE_READMODE_BYTE, PIPE_REJECT_REMOTE_CLIENTS, PIPE_TYPE_BYTE, PIPE_WAIT,
};
use windows::Win32::System::Threading::{CreateEventW, WaitForSingleObject, INFINITE};
use windows::Win32::System::IO::{CancelIoEx, GetOverlappedResult, OVERLAPPED};

/// Frames of one connection, both ways.
pub trait FrameIo: Send {
    fn send_frame(&mut self, f: &[u8]) -> io::Result<()>;
    /// The next frame; None when the peer closed the connection.
    fn recv_frame(&mut self) -> io::Result<Option<Vec<u8>>>;
}

#[derive(Debug)]
pub enum NamedError {
    /// Not a valid pipe name.
    Name,
    /// The name exists already: another host, or a squatter.
    Squatted,
    /// No server serves the name.
    Absent,
    /// The peer failed the check.
    Peer(String),
    Io(io::Error),
}

impl std::fmt::Display for NamedError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            NamedError::Name => f.write_str("not a valid pipe name"),
            NamedError::Squatted => f.write_str("the pipe name is taken"),
            NamedError::Absent => f.write_str("no server on the pipe"),
            NamedError::Peer(why) => write!(f, "peer refused: {why}"),
            NamedError::Io(e) => write!(f, "{e}"),
        }
    }
}

/// The longest frame (header included) the ops pipe carries.
pub const FRAME_CAP: usize = OPS.max_payload as usize + HEADER_LEN;
/// The pipe's buffer sizes.
const BUFFER: u32 = 65_536;
/// A peer that takes none of a frame for this long is broken.
const SEND_TIMEOUT: Duration = Duration::from_secs(10);

/// This process as a peer (image, session, integrity), read once.
pub fn own_peer() -> Option<&'static Peer> {
    static OWN: OnceLock<Option<Peer>> = OnceLock::new();
    OWN.get_or_init(|| peer_of_pid(std::process::id()).ok()).as_ref()
}

/// The ops pipe's name: mini-oled-dock.files.ops.<session>-<proto>-<build8>[-<tag>] (F-P3's PipeName form, one
/// nonce of [a-z0-9-]).
pub fn ops_pipe_name(session: u32, proto: u16, build8: &str, tag: Option<&str>) -> Result<String, NamedError> {
    let mut nonce = format!("{session}-{proto}-{build8}");
    if let Some(t) = tag {
        nonce.push('-');
        nonce.push_str(t);
    }
    Ok(PipeName::new("files", "ops", Some(&nonce)).map_err(|_| NamedError::Name)?.full())
}

/// Whether a peer's image is ours: the same path (case-insensitive), or in test mode any image under our cargo
/// profile folder (the folder of our own executable). Pure.
pub fn peer_ok(own: &Path, peer: &Path, test_mode: bool) -> bool {
    let lower = |p: &Path| p.to_string_lossy().to_lowercase().replace('/', "\\");
    let (own_s, peer_s) = (lower(own), lower(peer));
    if own_s == peer_s {
        return true;
    }
    if !test_mode {
        return false;
    }
    let Some(profile) = own.parent().map(lower) else { return false };
    let prefix = format!("{}\\", profile.trim_end_matches('\\'));
    peer_s.starts_with(&prefix) && !peer_s.contains("\\..\\")
}

/// The live tester's builds only: files.exe built with FILES_OPS_PROBE set in the build's environment (cargo rebuilds
/// when it changes) also admits the ops probe beside it, so the release probe drives a release host (LIVE-F2 steps 1
/// and 2). No shipped build is made with it; without it the host admits only its own image outside test mode.
pub const OPS_PROBE_BUILD: bool = option_env!("FILES_OPS_PROBE").is_some();

/// Whether `peer` is the ops probe beside `own`: ops-probe.exe in its folder (the tester's copies) or in its examples
/// folder (as cargo builds it), case-insensitive. Pure.
pub fn probe_ok(own: &Path, peer: &Path) -> bool {
    let lower = |p: &Path| p.to_string_lossy().to_lowercase().replace('/', "\\");
    let Some(dir) = own.parent().map(lower) else { return false };
    let dir = dir.trim_end_matches('\\');
    let peer = lower(peer);
    [format!("{dir}\\ops-probe.exe"), format!("{dir}\\examples\\ops-probe.exe")].contains(&peer)
}

/// The images the server admits: its own (peer_ok, with its test-mode rule) and, in a probe build only, the ops probe
/// beside it (probe_ok). Pure.
pub fn image_ok(own: &Path, peer: &Path, test_mode: bool, probe_build: bool) -> bool {
    peer_ok(own, peer, test_mode) || (probe_build && probe_ok(own, peer))
}

fn err_of(e: &windows::core::Error) -> io::Error {
    io::Error::from_raw_os_error(e.code().0 & 0xFFFF)
}

fn is_closed(e: &windows::core::Error) -> bool {
    [ERROR_BROKEN_PIPE, ERROR_NO_DATA, ERROR_PIPE_NOT_CONNECTED].iter().any(|c| e.code() == c.to_hresult())
}

fn new_event() -> io::Result<OwnedHandle> {
    // SAFETY: an unnamed manual-reset event; the handle returned is owned below.
    let h = unsafe { CreateEventW(None, true, false, PCWSTR::null()) }.map_err(|e| err_of(&e))?;
    // SAFETY: a fresh handle, owned by nothing else.
    Ok(unsafe { OwnedHandle::from_raw_handle(h.0) })
}

/// Runs one overlapped operation on `h` and waits for its end (`timeout` None: no limit). A timed-out operation is
/// cancelled and its end awaited, so neither the OVERLAPPED nor the buffer is used after this returns.
fn overlapped(
    h: HANDLE,
    event: HANDLE,
    timeout: Option<Duration>,
    start: impl FnOnce(*mut OVERLAPPED) -> windows::core::Result<()>,
) -> Result<u32, windows::core::Error> {
    let mut ov = OVERLAPPED { hEvent: event, ..Default::default() };
    match start(&mut ov) {
        Ok(()) => {}
        Err(e) if e.code() == ERROR_IO_PENDING.to_hresult() => {}
        Err(e) => return Err(e),
    }
    let ms = timeout.map_or(INFINITE, |t| u32::try_from(t.as_millis()).unwrap_or(INFINITE - 1));
    // SAFETY: `event` is a valid event handle owned by the connection.
    if unsafe { WaitForSingleObject(event, ms) } != WAIT_OBJECT_0 {
        // SAFETY: cancels only the operation of `ov`; one that already ended is not affected.
        let _ = unsafe { CancelIoEx(h, Some(&ov)) };
    }
    let mut n = 0u32;
    // SAFETY: `ov` lives until the operation has ended (bWait = true); `n` is writable.
    unsafe { GetOverlappedResult(h, &ov, &mut n, true) }.map(|()| n)
}

struct Inner {
    pipe: Arc<OwnedHandle>,
    read_ev: OwnedHandle,
    write_ev: OwnedHandle,
    reading: Mutex<()>,
    writing: Mutex<()>,
    /// A frame was cut or refused: the stream cannot be trusted again.
    broken: AtomicBool,
}

/// One connection, either end; clones share it (one reader thread and one writer thread at a time).
#[derive(Clone)]
pub struct PipeConn {
    inner: Arc<Inner>,
}

impl PipeConn {
    fn new(pipe: Arc<OwnedHandle>) -> io::Result<PipeConn> {
        Ok(PipeConn {
            inner: Arc::new(Inner {
                pipe,
                read_ev: new_event()?,
                write_ev: new_event()?,
                reading: Mutex::new(()),
                writing: Mutex::new(()),
                broken: AtomicBool::new(false),
            }),
        })
    }

    fn h(&self) -> HANDLE {
        HANDLE(self.inner.pipe.as_raw_handle())
    }

    /// The process at the other end of a client connection (the server).
    pub fn server_pid(&self) -> Option<u32> {
        let mut pid = 0u32;
        // SAFETY: a live pipe handle; `pid` is writable.
        unsafe { GetNamedPipeServerProcessId(self.h(), &mut pid) }.ok().map(|()| pid)
    }

    /// The process at the other end of a server connection (the client).
    pub fn client_pid(&self) -> Option<u32> {
        let mut pid = 0u32;
        // SAFETY: a live pipe handle; `pid` is writable.
        unsafe { GetNamedPipeClientProcessId(self.h(), &mut pid) }.ok().map(|()| pid)
    }

    /// Ends every read and write in progress on this connection (they return an error or the end).
    pub fn cancel(&self) {
        self.inner.broken.store(true, Ordering::Release);
        // SAFETY: cancels this process's I/O on a live handle; harmless when none is pending.
        let _ = unsafe { CancelIoEx(self.h(), None) };
    }

    /// Writes raw bytes (tests of the length check only).
    #[doc(hidden)]
    pub fn write_raw(&self, bytes: &[u8]) -> io::Result<()> {
        let _g = self.inner.writing.lock().unwrap_or_else(|e| e.into_inner());
        self.write_all(bytes)
    }

    fn write_all(&self, mut buf: &[u8]) -> io::Result<()> {
        let h = self.h();
        let ev = HANDLE(self.inner.write_ev.as_raw_handle());
        let deadline = Instant::now() + SEND_TIMEOUT;
        while !buf.is_empty() {
            let chunk = buf;
            let left = deadline.saturating_duration_since(Instant::now());
            // SAFETY: `chunk` is readable and outlives the operation, which `overlapped` waits out.
            let n = overlapped(h, ev, Some(left), |ov| unsafe { WriteFile(h, Some(chunk), None, Some(ov)) }).map_err(
                |e| {
                    if is_closed(&e) {
                        io::Error::from(io::ErrorKind::BrokenPipe)
                    } else {
                        err_of(&e)
                    }
                },
            )?;
            if n == 0 {
                return Err(io::Error::from(io::ErrorKind::BrokenPipe));
            }
            buf = buf.get(n as usize..).unwrap_or_default();
        }
        Ok(())
    }

    /// Fills `buf`; Ok(false) when the peer closed before the first byte.
    fn read_exact(&self, buf: &mut [u8]) -> io::Result<bool> {
        let h = self.h();
        let ev = HANDLE(self.inner.read_ev.as_raw_handle());
        let mut got = 0usize;
        while got < buf.len() {
            let space = buf.get_mut(got..).unwrap_or_default();
            // SAFETY: `space` is writable and outlives the operation, which `overlapped` waits out.
            match overlapped(h, ev, None, |ov| unsafe { ReadFile(h, Some(space), None, Some(ov)) }) {
                Ok(0) => return if got == 0 { Ok(false) } else { Err(io::Error::from(io::ErrorKind::UnexpectedEof)) },
                Ok(n) => got += n as usize,
                Err(e) if is_closed(&e) || e.code() == ERROR_OPERATION_ABORTED.to_hresult() => {
                    return if got == 0 { Ok(false) } else { Err(io::Error::from(io::ErrorKind::UnexpectedEof)) };
                }
                Err(e) => return Err(err_of(&e)),
            }
        }
        Ok(true)
    }
}

impl FrameIo for PipeConn {
    fn send_frame(&mut self, f: &[u8]) -> io::Result<()> {
        if self.inner.broken.load(Ordering::Acquire) {
            return Err(io::Error::from(io::ErrorKind::BrokenPipe));
        }
        let len = u32::try_from(f.len()).ok().filter(|&n| n as usize <= FRAME_CAP);
        let Some(len) = len else { return Err(io::Error::new(io::ErrorKind::InvalidInput, "frame over the cap")) };
        let _g = self.inner.writing.lock().unwrap_or_else(|e| e.into_inner());
        let sent = self.write_all(&len.to_le_bytes()).and_then(|()| self.write_all(f));
        if sent.is_err() {
            self.inner.broken.store(true, Ordering::Release);
        }
        sent
    }

    fn recv_frame(&mut self) -> io::Result<Option<Vec<u8>>> {
        if self.inner.broken.load(Ordering::Acquire) {
            return Ok(None);
        }
        let _g = self.inner.reading.lock().unwrap_or_else(|e| e.into_inner());
        let mut head = [0u8; 4];
        if !self.read_exact(&mut head)? {
            return Ok(None);
        }
        let len = u32::from_le_bytes(head) as usize;
        if len > FRAME_CAP {
            // Refused before anything is allocated; the stream is poisoned.
            self.inner.broken.store(true, Ordering::Release);
            return Err(io::Error::new(io::ErrorKind::InvalidData, format!("a frame of {len} bytes is over the cap")));
        }
        let mut body = vec![0u8; len];
        if !self.read_exact(&mut body)? && len > 0 {
            return Err(io::Error::from(io::ErrorKind::UnexpectedEof));
        }
        Ok(Some(body))
    }
}

/// A security descriptor made from SDDL, freed with LocalFree.
struct Sd(PSECURITY_DESCRIPTOR);

impl Sd {
    fn from_sddl(sddl: &str) -> io::Result<Sd> {
        let s = HSTRING::from(sddl);
        let mut sd = PSECURITY_DESCRIPTOR::default();
        // SAFETY: `s` is NUL-terminated; `sd` receives a LocalAlloc'd descriptor freed by Drop.
        unsafe { ConvertStringSecurityDescriptorToSecurityDescriptorW(&s, SDDL_REVISION_1, &mut sd, None) }
            .map_err(|e| err_of(&e))?;
        Ok(Sd(sd))
    }
}

impl Drop for Sd {
    fn drop(&mut self) {
        // SAFETY: allocated by ConvertStringSecurityDescriptorToSecurityDescriptorW; not used afterwards.
        unsafe { LocalFree(Some(HLOCAL(self.0 .0))) };
    }
}

/// The server end: one pipe instance that serves one client after another, never releasing the name.
pub struct NamedServer {
    pipe: Arc<OwnedHandle>,
    event: OwnedHandle,
    own_image: PathBuf,
    test_mode: bool,
}

impl NamedServer {
    /// Creates the pipe `name` (a full \\.\pipe\ name). `own_image` is what clients must run (test mode: or any
    /// image of its cargo profile folder).
    pub fn create(name: &str, own_image: &Path, test_mode: bool) -> Result<NamedServer, NamedError> {
        let user = oled_sys::pipe::current_user_sid().map_err(|e| NamedError::Io(io::Error::other(e.to_string())))?;
        let sd = Sd::from_sddl(&format!("D:P(A;;GA;;;{user})(A;;GA;;;SY)")).map_err(NamedError::Io)?;
        let sa = SECURITY_ATTRIBUTES {
            nLength: std::mem::size_of::<SECURITY_ATTRIBUTES>() as u32,
            lpSecurityDescriptor: sd.0 .0,
            bInheritHandle: false.into(),
        };
        let wide = HSTRING::from(name);
        // SAFETY: `wide` is NUL-terminated; `sa` and the descriptor it points to live for the call.
        let h = unsafe {
            CreateNamedPipeW(
                &wide,
                PIPE_ACCESS_DUPLEX | FILE_FLAG_FIRST_PIPE_INSTANCE | FILE_FLAG_OVERLAPPED,
                PIPE_TYPE_BYTE | PIPE_READMODE_BYTE | PIPE_WAIT | PIPE_REJECT_REMOTE_CLIENTS,
                1,
                BUFFER,
                BUFFER,
                0,
                Some(&sa),
            )
        };
        if h.is_invalid() {
            let e = io::Error::last_os_error();
            let taken = [ERROR_ACCESS_DENIED.0 as i32, ERROR_PIPE_BUSY.0 as i32];
            return Err(if e.raw_os_error().is_some_and(|c| taken.contains(&c)) {
                NamedError::Squatted
            } else {
                NamedError::Io(e)
            });
        }
        // SAFETY: a fresh pipe handle, owned by nothing else.
        let pipe = Arc::new(unsafe { OwnedHandle::from_raw_handle(h.0) });
        Ok(NamedServer {
            pipe,
            event: new_event().map_err(NamedError::Io)?,
            own_image: own_image.to_path_buf(),
            test_mode,
        })
    }

    /// Waits for the next client that passes the check (a refused one is disconnected and the wait goes on).
    pub fn accept(&self) -> Result<PipeConn, NamedError> {
        loop {
            self.wait_client()?;
            let conn = PipeConn::new(self.pipe.clone()).map_err(NamedError::Io)?;
            match conn
                .client_pid()
                .ok_or_else(|| NamedError::Peer("no client pid".into()))
                .and_then(|pid| self.check(pid))
            {
                Ok(()) => return Ok(conn),
                Err(e) => {
                    oled_sys::log!("files ops: a client was refused: {e}");
                    self.disconnect();
                }
            }
        }
    }

    /// Drops the current client, so the same instance can wait for the next one.
    pub fn disconnect(&self) {
        // SAFETY: a live pipe handle; disconnecting an idle instance is harmless.
        let _ = unsafe { DisconnectNamedPipe(HANDLE(self.pipe.as_raw_handle())) };
    }

    fn wait_client(&self) -> Result<(), NamedError> {
        let h = HANDLE(self.pipe.as_raw_handle());
        let ev = HANDLE(self.event.as_raw_handle());
        // SAFETY: the pipe handle and the OVERLAPPED live until `overlapped` has seen the operation end.
        match overlapped(h, ev, None, |ov| unsafe { ConnectNamedPipe(h, Some(ov)) }) {
            Ok(_) => Ok(()),
            Err(e) if e.code() == ERROR_PIPE_CONNECTED.to_hresult() => Ok(()),
            Err(e) => Err(NamedError::Io(err_of(&e))),
        }
    }

    /// The client `pid`: our image (peer_ok), our session, an integrity at least ours.
    fn check(&self, pid: u32) -> Result<(), NamedError> {
        let peer = peer_of_pid(pid).map_err(|e| NamedError::Peer(e.to_string()))?;
        let own = own_peer().ok_or_else(|| NamedError::Peer("own token unreadable".into()))?;
        client_check(&peer, own.integrity, &self.own_image, self.test_mode)
    }
}

/// What the server admits: our image (image_ok), our session, no integrity below ours. Pure but for the session
/// query of Conn::check.
pub fn client_check(peer: &Peer, own: Integrity, own_image: &Path, test_mode: bool) -> Result<(), NamedError> {
    let rule = PeerRule { same_session: true, min_integrity: Some(own), ..PeerRule::default() };
    Conn::check(peer, &rule).map_err(|e| NamedError::Peer(e.to_string()))?;
    if !image_ok(own_image, &peer.image, test_mode, OPS_PROBE_BUILD) {
        return Err(NamedError::Peer(format!("another image: {}", peer.image.display())));
    }
    Ok(())
}

/// What a client admits as its ops host: exactly `exe`, in our session, at exactly our integrity, as our user (a
/// predictable name can be taken first by a lower process running the same image). Pure but for the session query.
pub fn server_check(
    peer: &Peer,
    server_user: &str,
    own_user: &str,
    own: Integrity,
    exe: &Path,
) -> Result<(), NamedError> {
    let rule =
        PeerRule { same_session: true, min_integrity: Some(own), max_integrity: Some(own), ..PeerRule::default() };
    Conn::check(peer, &rule).map_err(|e| NamedError::Peer(e.to_string()))?;
    if !peer_ok(exe, &peer.image, false) {
        return Err(NamedError::Peer(format!("another image: {}", peer.image.display())));
    }
    if server_user != own_user {
        return Err(NamedError::Peer("another user".into()));
    }
    Ok(())
}

/// Connects to the server of `name` within `timeout`: a busy pipe is waited for (WaitNamedPipeW); an absent one
/// is Absent at once unless `wait_absent` (a host just started).
pub fn connect(name: &str, timeout: Duration, wait_absent: bool) -> Result<PipeConn, NamedError> {
    let deadline = Instant::now() + timeout;
    let wide = HSTRING::from(name);
    loop {
        // SAFETY: `wide` is NUL-terminated; no security attributes or template. Identification level: the server
        // can learn who connects but never act as this process.
        let opened = unsafe {
            CreateFileW(
                &wide,
                (GENERIC_READ | GENERIC_WRITE).0,
                FILE_SHARE_NONE,
                None,
                OPEN_EXISTING,
                FILE_FLAG_OVERLAPPED | SECURITY_SQOS_PRESENT | SECURITY_IDENTIFICATION,
                None,
            )
        };
        let left = deadline.saturating_duration_since(Instant::now());
        match opened {
            Ok(h) => {
                // SAFETY: a fresh pipe handle, owned by nothing else.
                let pipe = Arc::new(unsafe { OwnedHandle::from_raw_handle(h.0) });
                return PipeConn::new(pipe).map_err(NamedError::Io);
            }
            Err(e) if e.code() == ERROR_FILE_NOT_FOUND.to_hresult() && !wait_absent => return Err(NamedError::Absent),
            Err(_) if left.is_zero() => return Err(NamedError::Absent),
            Err(e) if e.code() == ERROR_PIPE_BUSY.to_hresult() => {
                let ms = u32::try_from(left.as_millis()).unwrap_or(INFINITE - 1).clamp(1, 200);
                // SAFETY: `wide` is NUL-terminated; a false return only means: try again.
                let _ = unsafe { WaitNamedPipeW(&wide, ms) };
            }
            Err(e) if e.code() == ERROR_FILE_NOT_FOUND.to_hresult() => std::thread::sleep(Duration::from_millis(10)),
            Err(e) => return Err(NamedError::Io(err_of(&e))),
        }
    }
}

/// In-memory FrameIo pairs (tests): each end sends to the other; dropping every clone of one end is the other's EOF.
#[derive(Clone)]
pub struct MemIo {
    tx: Sender<Vec<u8>>,
    rx: Arc<Mutex<Receiver<Vec<u8>>>>,
    sent: Arc<Mutex<Vec<usize>>>,
}

impl MemIo {
    /// The sizes of the frames this end sent, in order.
    pub fn sent_sizes(&self) -> Vec<usize> {
        self.sent.lock().unwrap_or_else(|e| e.into_inner()).clone()
    }
}

pub fn mem_pair() -> (MemIo, MemIo) {
    let (a_tx, b_rx) = channel();
    let (b_tx, a_rx) = channel();
    let a = MemIo { tx: a_tx, rx: Arc::new(Mutex::new(a_rx)), sent: Arc::default() };
    let b = MemIo { tx: b_tx, rx: Arc::new(Mutex::new(b_rx)), sent: Arc::default() };
    (a, b)
}

impl FrameIo for MemIo {
    fn send_frame(&mut self, f: &[u8]) -> io::Result<()> {
        if f.len() > FRAME_CAP {
            return Err(io::Error::new(io::ErrorKind::InvalidInput, "frame over the cap"));
        }
        self.sent.lock().unwrap_or_else(|e| e.into_inner()).push(f.len());
        self.tx.send(f.to_vec()).map_err(|_| io::Error::from(io::ErrorKind::BrokenPipe))
    }

    fn recv_frame(&mut self) -> io::Result<Option<Vec<u8>>> {
        Ok(self.rx.lock().unwrap_or_else(|e| e.into_inner()).recv().ok())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn peer_ok_table() {
        let own = Path::new("C:\\t\\target-test\\debug\\files.exe");
        let rows = [
            ("C:\\t\\target-test\\debug\\files.exe", false, true),
            ("c:\\T\\TARGET-TEST\\DEBUG\\FILES.EXE", false, true),
            ("C:\\t\\target-test\\debug\\deps\\ops_client-1234.exe", false, false),
            ("C:\\t\\target-test\\debug\\deps\\ops_client-1234.exe", true, true),
            ("C:\\t\\target-test\\debug\\examples\\ops-probe.exe", true, true),
            ("C:\\t\\target-test\\release\\files.exe", true, false),
            ("C:\\t\\target-test\\debugx\\files.exe", true, false),
            ("C:\\other\\files.exe", true, false),
            ("C:\\t\\target-test\\debug\\..\\release\\x.exe", true, false),
        ];
        for (peer, test, want) in rows {
            assert_eq!(peer_ok(own, Path::new(peer), test), want, "{peer} test={test}");
        }
    }

    #[test]
    fn probe_ok_table() {
        let own = Path::new("C:\\t\\target-rel\\release\\files.exe");
        let rows = [
            ("C:\\t\\target-rel\\release\\examples\\ops-probe.exe", true),
            ("c:\\T\\TARGET-REL\\RELEASE\\EXAMPLES\\OPS-PROBE.EXE", true),
            ("C:\\t\\target-rel\\release\\ops-probe.exe", true),
            ("C:/t/target-rel/release/examples/ops-probe.exe", true),
            ("C:\\t\\target-rel\\release\\examples\\shell-probe.exe", false),
            ("C:\\t\\target-rel\\release\\examples\\sub\\ops-probe.exe", false),
            ("C:\\t\\target-rel\\release\\deps\\ops-probe.exe", false),
            ("C:\\t\\target-rel\\debug\\examples\\ops-probe.exe", false),
            ("C:\\t\\target-rel\\release\\files.exe", false),
            ("C:\\other\\ops-probe.exe", false),
        ];
        for (peer, want) in rows {
            assert_eq!(probe_ok(own, Path::new(peer)), want, "{peer}");
        }
        let probe = Path::new("C:\\t\\target-rel\\release\\examples\\ops-probe.exe");
        assert!(!image_ok(own, probe, false, false), "a shipped release host refuses the probe");
        assert!(image_ok(own, probe, false, true), "a probe build admits it");
        assert!(image_ok(own, own, false, false) && image_ok(own, own, false, true), "its own image always");
        let other = Path::new("C:\\t\\target-rel\\release\\other.exe");
        assert!(!image_ok(own, other, false, true), "a probe build admits nothing else");
    }

    #[test]
    fn pipe_names() {
        let n = ops_pipe_name(1, 1, "0a1b2c3d", None).unwrap();
        assert_eq!(n, "\\\\.\\pipe\\mini-oled-dock.files.ops.1-1-0a1b2c3d");
        let t = ops_pipe_name(12, 1, "0a1b2c3d", Some("abc123xyz")).unwrap();
        assert_eq!(t, "\\\\.\\pipe\\mini-oled-dock.files.ops.12-1-0a1b2c3d-abc123xyz");
        assert!(matches!(ops_pipe_name(1, 1, "0A1B", None), Err(NamedError::Name)), "upper case is not a name part");
    }

    #[test]
    fn mem_pair_carries_frames_both_ways() {
        let (mut a, mut b) = mem_pair();
        a.send_frame(b"one").unwrap();
        b.send_frame(b"two").unwrap();
        assert_eq!(b.recv_frame().unwrap().as_deref(), Some(&b"one"[..]));
        assert_eq!(a.recv_frame().unwrap().as_deref(), Some(&b"two"[..]));
        assert_eq!(a.sent_sizes(), [3]);
        drop(a);
        assert_eq!(b.recv_frame().unwrap(), None, "the other end is gone");
    }
}
