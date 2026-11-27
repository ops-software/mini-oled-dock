//! One UI process per session (P3-files-app 2.2; F1a.15-F1a.17): the elevated-start guard, the single-instance
//! mutex, the message-only IPC window that takes commands from other processes, and the process identity.
//!
//! What another process sends (WM_COPYDATA) is capped, decoded whole and only navigates: every path passes WidePath,
//! nothing is executed, and the window procedure answers within the send by posting to the hub, never by navigating
//! inside it (U5). The forwarder sends only to a window whose process runs this very image file, in this session,
//! at this process's integrity or above and as this user, so a squatting window of the same class gets nothing.
#![deny(clippy::unwrap_used, clippy::expect_used, clippy::indexing_slicing)]

use std::ffi::c_void;
use std::os::windows::ffi::{OsStrExt, OsStringExt};
use std::os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle, RawHandle};
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::Arc;

use oled_sys::pipe::Integrity;
use oled_trust::frame::files::{InstanceCmd, Location, WidePath, INSTANCE};
use oled_ui::model::{ScrollTo, Selection};
use oled_ui::{EventCx, Reply};
use windows::core::{HSTRING, PCWSTR, PWSTR};
use windows::Win32::Foundation::{
    CloseHandle, GetLastError, SetLastError, ERROR_ALREADY_EXISTS, ERROR_CLASS_ALREADY_EXISTS, ERROR_TIMEOUT, FILETIME,
    HANDLE, HINSTANCE, HWND, LPARAM, LRESULT, WAIT_TIMEOUT, WIN32_ERROR, WPARAM,
};
use windows::Win32::Security::{GetTokenInformation, TokenElevationType, TOKEN_ELEVATION_TYPE, TOKEN_QUERY};
use windows::Win32::Storage::FileSystem::{
    CreateFileW, GetFileInformationByHandle, GetFinalPathNameByHandleW, BY_HANDLE_FILE_INFORMATION,
    FILE_ATTRIBUTE_DIRECTORY, FILE_FLAG_BACKUP_SEMANTICS, FILE_LIST_DIRECTORY, FILE_NAME_NORMALIZED,
    FILE_READ_ATTRIBUTES, FILE_SHARE_DELETE, FILE_SHARE_READ, FILE_SHARE_WRITE, GETFINALPATHNAMEBYHANDLE_FLAGS,
    OPEN_EXISTING, VOLUME_NAME_DOS,
};
use windows::Win32::System::DataExchange::COPYDATASTRUCT;
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::System::SystemInformation::{GetSystemTimeAsFileTime, GetSystemWindowsDirectoryW};
use windows::Win32::System::Threading::{
    CreateMutexW, CreateProcessW, GetCurrentProcess, GetProcessTimes, OpenProcessToken, WaitForSingleObject,
    CREATE_NO_WINDOW, PROCESS_INFORMATION, STARTUPINFOW,
};
use windows::Win32::UI::Shell::SetCurrentProcessExplicitAppUserModelID;
use windows::Win32::UI::WindowsAndMessaging::{
    AllowSetForegroundWindow, CreateWindowExW, DefWindowProcW, DestroyWindow, FindWindowExW, GetWindowLongPtrW,
    GetWindowThreadProcessId, RegisterClassExW, RegisterWindowMessageW, SendMessageTimeoutW, SetWindowLongPtrW,
    UnregisterClassW, CREATESTRUCTW, GWLP_USERDATA, HWND_MESSAGE, SMTO_ABORTIFHUNG, WINDOW_EX_STYLE, WINDOW_STYLE,
    WM_COPYDATA, WM_NCCREATE, WM_NCDESTROY, WNDCLASSEXW,
};

use crate::cli::{self, Cli};
use crate::hub::Hub;
use crate::listing::collate::utf16_to_wtf8;
use crate::msg::Msg;
use crate::window::{self, FilesScreen, LoopReq, Show, WinKind};
use crate::{linger, model, names, status, tab};

/// The largest frame the IPC window takes (COPYDATASTRUCT.cbData).
pub const MAX_FRAME: usize = 65_536;
/// Windows of the IPC class a forward looks at (a flood of squatting windows stays finite).
const MAX_CANDIDATES: usize = 64;
/// SendMessageTimeoutW's bound (with SMTO_ABORTIFHUNG a hung instance answers at once).
const SEND_TIMEOUT_MS: u32 = 2_000;

// ---- F1a.15: the elevated-start guard ------------------------------------------------------------------------------

/// TOKEN_ELEVATION_TYPE.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Elevation {
    Default,
    Full,
    Limited,
}

/// TOKEN_ELEVATION_TYPE's value (winnt.h): 2 Full (a UAC-elevated token), 3 Limited (an administrator's filtered
/// token); 1 Default (UAC off, a standard user, a hosted runner) and any other value read as Default. Pure.
pub fn elevation_of(raw: i32) -> Elevation {
    match raw {
        2 => Elevation::Full,
        3 => Elevation::Limited,
        _ => Elevation::Default,
    }
}

/// GetTokenInformation(TokenElevationType) of this process; an error reads as Default.
pub fn elevation() -> Elevation {
    let mut token = HANDLE::default();
    // SAFETY: this process's pseudo handle; \`token\` is writable and owned below.
    if unsafe { OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token) }.is_err() {
        return Elevation::Default;
    }
    // SAFETY: a fresh handle, owned by nothing else.
    let token = unsafe { OwnedHandle::from_raw_handle(token.0 as RawHandle) };
    let mut kind = TOKEN_ELEVATION_TYPE::default();
    let mut len = 0u32;
    // SAFETY: the token is open while \`token\` lives; \`kind\` is as large as the size passed; \`len\` is writable.
    let read = unsafe {
        GetTokenInformation(
            HANDLE(token.as_raw_handle()),
            TokenElevationType,
            Some(std::ptr::from_mut(&mut kind).cast()),
            std::mem::size_of::<TOKEN_ELEVATION_TYPE>() as u32,
            &mut len,
        )
    };
    if read.is_err() {
        return Elevation::Default;
    }
    elevation_of(kind.0)
}

/// A UAC-elevated token hands off to File Explorer (P1-files-engine OP5c's rule).
pub fn must_hand_off(e: Elevation) -> bool {
    e == Elevation::Full
}

/// A UAC-elevated window start: its target goes to File Explorer (which never stays elevated, so the folder opens
/// at medium) and nothing else starts: no config, no window, no host. Exits EXIT_ELEVATED_HANDOFF; a target that is
/// no path is the usage error. Only Cli::Window hands off: another form starts nothing here.
pub fn elevated_handoff(cli: &Cli) -> i32 {
    let Cli::Window { from_verb, .. } = cli else {
        return names::EXIT_ELEVATED_HANDOFF;
    };
    let cwd = current_dir().ok_or_else(|| "files: the current folder is not a path".to_string());
    let cmd = match cwd.and_then(|cwd| cli::to_instance(cli, &cwd)) {
        Ok(c) => c,
        Err(e) => {
            oled_sys::log!("{e}");
            return names::EXIT_USAGE;
        }
    };
    let verb = if *from_verb { " from verb" } else { "" };
    let Some(handoff) = checked_handoff(&cmd) else {
        oled_sys::log!("files: elevated start{verb}: the target is not a folder, nothing is handed off");
        return names::EXIT_ELEVATED_HANDOFF;
    };
    match start_explorer(&handoff.args, handoff.pin) {
        Ok(()) => oled_sys::log!("files: elevated start handed to File Explorer{verb}"),
        Err(e) => oled_sys::log!("files: elevated start{verb}: File Explorer did not start ({e})"),
    }
    names::EXIT_ELEVATED_HANDOFF
}

/// What a hand-off to File Explorer takes: its arguments and, for a folder, the pin that holds that folder.
pub struct Handoff {
    pub args: Vec<u16>,
    pub pin: Option<FolderPin>,
}

/// The hand-off of a command, after a folder target has been pinned: None when the target of NewWindow or OpenTab is
/// not a folder now (a file, a link to one, or nothing), so a file is never given to File Explorer, which would run
/// it. The folder arguments name the pinned folder's own final path.
pub fn checked_handoff(cmd: &InstanceCmd) -> Option<Handoff> {
    match cmd {
        InstanceCmd::NewWindow(Location::Fs(p)) | InstanceCmd::OpenTab(Location::Fs(p)) => {
            let pin = FolderPin::open(p)?;
            Some(Handoff { args: folder_arg(pin.path()), pin: Some(pin) })
        }
        _ => Some(Handoff { args: handoff_args(cmd), pin: None }),
    }
}

/// A folder held open so that nothing can rename, move or delete it, or rename a folder above it, while File
/// Explorer takes its path: opened for listing and attributes, shared for reading and writing but not for deletion
/// (a rename needs that share too). A link on the way is resolved once, at the open: `path` is the final path of
/// the opened folder, so a link re-pointed afterwards changes nothing. Dropping it closes the handle.
pub struct FolderPin {
    _handle: OwnedHandle,
    path: Vec<u16>,
}

impl FolderPin {
    /// Pins the folder `p` names; None when it is not a folder, cannot be opened this way, or has no DOS path.
    pub fn open(p: &WidePath) -> Option<FolderPin> {
        let z: Vec<u16> = p.units().iter().copied().chain([0]).collect();
        // SAFETY: `z` is NUL-terminated and lives for the call; no security attributes, no template.
        let h = unsafe {
            CreateFileW(
                PCWSTR(z.as_ptr()),
                FILE_LIST_DIRECTORY.0 | FILE_READ_ATTRIBUTES.0,
                FILE_SHARE_READ | FILE_SHARE_WRITE,
                None,
                OPEN_EXISTING,
                FILE_FLAG_BACKUP_SEMANTICS,
                None,
            )
        }
        .ok()?;
        // SAFETY: `h` was just returned open by CreateFileW and nothing else owns it.
        let handle = unsafe { OwnedHandle::from_raw_handle(h.0) };
        let h = HANDLE(handle.as_raw_handle());
        let mut info = BY_HANDLE_FILE_INFORMATION::default();
        // SAFETY: `h` is open for the whole call; `info` is a writable struct of the declared type.
        unsafe { GetFileInformationByHandle(h, &mut info) }.ok()?;
        if info.dwFileAttributes & FILE_ATTRIBUTE_DIRECTORY.0 == 0 {
            return None;
        }
        let path = dos_path(final_path(h)?)?;
        if !parses_as_stored(&path) || !same_object(&path, &info) {
            return None;
        }
        Some(FolderPin { _handle: handle, path })
    }

    /// The final path of the pinned folder, without the \\?\ prefix.
    pub fn path(&self) -> &[u16] {
        &self.path
    }
}

/// No component of `path` ends in a dot or a space: Win32 and the shell trim those, so such a name would be read as
/// another one (a folder 'x.' as the file 'x').
fn parses_as_stored(path: &[u16]) -> bool {
    path.split(|u| *u == u16::from(b'\\'))
        .all(|c| c.last().is_none_or(|u| *u != u16::from(b'.') && *u != u16::from(b' ')))
}

/// `path`, opened the way Win32 and the shell parse it (no \\?\ prefix), is the object `pinned` describes: same
/// volume and same file index. False when it cannot be opened.
fn same_object(path: &[u16], pinned: &BY_HANDLE_FILE_INFORMATION) -> bool {
    let z: Vec<u16> = path.iter().copied().chain([0]).collect();
    // SAFETY: `z` is NUL-terminated and lives for the call; no security attributes, no template.
    let opened = unsafe {
        CreateFileW(
            PCWSTR(z.as_ptr()),
            FILE_READ_ATTRIBUTES.0,
            FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE,
            None,
            OPEN_EXISTING,
            FILE_FLAG_BACKUP_SEMANTICS,
            None,
        )
    };
    let Ok(h) = opened else { return false };
    // SAFETY: `h` was just returned open by CreateFileW and nothing else owns it.
    let handle = unsafe { OwnedHandle::from_raw_handle(h.0) };
    let mut info = BY_HANDLE_FILE_INFORMATION::default();
    // SAFETY: the handle is open for the whole call; `info` is a writable struct of the declared type.
    if unsafe { GetFileInformationByHandle(HANDLE(handle.as_raw_handle()), &mut info) }.is_err() {
        return false;
    }
    (info.dwVolumeSerialNumber, info.nFileIndexHigh, info.nFileIndexLow)
        == (pinned.dwVolumeSerialNumber, pinned.nFileIndexHigh, pinned.nFileIndexLow)
}

/// GetFinalPathNameByHandleW (normalized, DOS volume names) of an open handle.
fn final_path(h: HANDLE) -> Option<Vec<u16>> {
    let mut buf = vec![0u16; 512];
    for _ in 0..2 {
        let flags = GETFINALPATHNAMEBYHANDLE_FLAGS(FILE_NAME_NORMALIZED.0 | VOLUME_NAME_DOS.0);
        // SAFETY: `h` is a valid handle; `buf` is writable for its whole length.
        let n = unsafe { GetFinalPathNameByHandleW(h, &mut buf, flags) } as usize;
        if n == 0 {
            return None;
        }
        if n < buf.len() {
            buf.truncate(n);
            return Some(buf);
        }
        buf = vec![0u16; n + 1];
    }
    None
}

/// The final path without its \\?\ prefix: `C:\x` from `\\?\C:\x`, `\\server\share\x` from `\\?\UNC\server\share\x`.
/// None for any other form (a volume GUID name, a device path), which is not handed to File Explorer.
fn dos_path(full: Vec<u16>) -> Option<Vec<u16>> {
    let text = |s: &str| s.encode_utf16().collect::<Vec<u16>>();
    if let Some(rest) = full.strip_prefix(text(r"\\?\UNC\").as_slice()) {
        return Some(text(r"\\").into_iter().chain(rest.iter().copied()).collect());
    }
    let rest = full.strip_prefix(text(r"\\?\").as_slice())?;
    let (letter, colon) = (rest.first()?, rest.get(1)?);
    let is_letter = u8::try_from(*letter).is_ok_and(|d| d.is_ascii_alphabetic());
    (is_letter && *colon == u16::from(b':')).then(|| rest.to_vec())
}

/// File Explorer's arguments for a hand-off by the requested path: none for Home, the quoted folder, or "/select,"
/// and the quoted item for a file (its folder opens with it selected, as a window of ours would show it; a file is
/// never opened). A closing backslash is not added: the shell's parser takes "C:\x.exe\" for the file, so it is no
/// defence; a folder is pinned instead (`checked_handoff`). Pure.
pub fn handoff_args(cmd: &InstanceCmd) -> Vec<u16> {
    match cmd {
        InstanceCmd::NewWindow(Location::Fs(p)) | InstanceCmd::OpenTab(Location::Fs(p)) => folder_arg(p.units()),
        InstanceCmd::Select { item, .. } => {
            let mut a: Vec<u16> = "/select,".encode_utf16().collect();
            a.extend(quote_arg(item.units()));
            a
        }
        _ => Vec::new(),
    }
}

/// A folder as one argument: quoted, with closing backslashes dropped (a doubled one would not survive a parser that
/// only strips the quotes); a drive root keeps its single backslash and stays bare, as "C:" alone means a folder.
fn folder_arg(units: &[u16]) -> Vec<u16> {
    const BACKSLASH: u16 = b'\\' as u16;
    if matches!(units, [_, c, b] if *c == u16::from(b':') && *b == BACKSLASH) {
        return units.to_vec();
    }
    let end = units.iter().rposition(|u| *u != BACKSLASH).map_or(0, |i| i + 1);
    quote_arg(units.get(..end).unwrap_or(units))
}

/// One argument quoted by CommandLineToArgvW's rules: in double quotes, every run of backslashes before a quote (the
/// closing one included) doubled, an inner quote escaped. Pure.
pub fn quote_arg(arg: &[u16]) -> Vec<u16> {
    const QUOTE: u16 = b'"' as u16;
    const BACKSLASH: u16 = b'\\' as u16;
    let mut out = Vec::with_capacity(arg.len() + 2);
    out.push(QUOTE);
    let mut run = 0usize;
    for &u in arg {
        match u {
            BACKSLASH => run += 1,
            QUOTE => {
                out.extend(std::iter::repeat_n(BACKSLASH, run * 2 + 1));
                out.push(QUOTE);
                run = 0;
            }
            _ => {
                out.extend(std::iter::repeat_n(BACKSLASH, run));
                out.push(u);
                run = 0;
            }
        }
    }
    out.extend(std::iter::repeat_n(BACKSLASH, run * 2));
    out.push(QUOTE);
    out
}

/// This process's current folder as a WidePath.
fn current_dir() -> Option<WidePath> {
    let d = std::env::current_dir().ok()?;
    let units: Vec<u16> = d.as_os_str().encode_wide().collect();
    WidePath::new(&units).ok()
}

/// The system's Windows folder (GetSystemWindowsDirectoryW), never the environment's %WINDIR%: an elevated process
/// must not start an image a caller's environment names.
fn windows_dir() -> Option<PathBuf> {
    let mut buf = [0u16; 261];
    // SAFETY: the buffer is writable for its whole length.
    let n = unsafe { GetSystemWindowsDirectoryW(Some(&mut buf)) } as usize;
    let units = buf.get(..n).filter(|u| !u.is_empty() && n < 261)?;
    Some(PathBuf::from(std::ffi::OsString::from_wide(units)))
}

/// How long one wait for File Explorer's launcher lasts before the delay is logged and the wait goes on.
const PIN_WAIT_MS: u32 = 10_000;

/// Waits for the process `process` to end, with no upper bound: after every `slice_ms` without an end, `on_slice`
/// runs and the wait goes on. A pin must outlive the launcher, however long a starved or hung one takes, so a timeout
/// is never a reason to let go. Returns at the end of the process, or when the wait itself fails.
pub fn wait_for_exit(process: HANDLE, slice_ms: u32, mut on_slice: impl FnMut()) {
    loop {
        // SAFETY: the wait is on a handle the caller keeps open for the call.
        if unsafe { WaitForSingleObject(process, slice_ms) } != WAIT_TIMEOUT {
            return;
        }
        on_slice();
    }
}

/// <Windows>\explorer.exe with `args`, started in the Windows folder. `pin` (the folder the arguments name) stays
/// held across the start and until the new process ends, however long that takes (a delay is logged): that process
/// hands the path on to File Explorer and exits, and as long as it runs the folder cannot be swapped for a program.
/// Whether the launcher resolves the path itself or forwards the string to the shell, which parses it after the
/// launcher has exited, has not been checked live; the hold covers only the first case.
fn start_explorer(args: &[u16], pin: Option<FolderPin>) -> Result<(), String> {
    let dir = windows_dir().ok_or_else(|| "no Windows folder".to_string())?;
    let exe = dir.join("explorer.exe");
    let exe_units: Vec<u16> = exe.as_os_str().encode_wide().collect();
    let app: Vec<u16> = exe_units.iter().copied().chain([0]).collect();
    let mut line = quote_arg(&exe_units);
    if !args.is_empty() {
        line.push(u16::from(b' '));
        line.extend_from_slice(args);
    }
    line.push(0);
    let cwd: Vec<u16> = dir.as_os_str().encode_wide().chain([0]).collect();
    let si = STARTUPINFOW { cb: std::mem::size_of::<STARTUPINFOW>() as u32, ..Default::default() };
    let mut pi = PROCESS_INFORMATION::default();
    // CREATE_NO_WINDOW is the package's rule for every start (lint no_window_spawns); a GUI program ignores it.
    // SAFETY: \`app\` and \`cwd\` are NUL-terminated, \`line\` is a writable NUL-terminated buffer, \`si\` and \`pi\` live
    // for the call; no handle is inherited.
    unsafe {
        CreateProcessW(
            PCWSTR(app.as_ptr()),
            Some(PWSTR(line.as_mut_ptr())),
            None,
            None,
            false,
            CREATE_NO_WINDOW,
            None,
            PCWSTR(cwd.as_ptr()),
            &si,
            &mut pi,
        )
    }
    .map_err(|e| e.to_string())?;
    if pin.is_some() {
        wait_for_exit(pi.hProcess, PIN_WAIT_MS, || {
            oled_sys::log!("files: elevated start: File Explorer's launcher still runs, the folder stays pinned");
        });
    }
    // SAFETY: both handles were returned open by CreateProcessW and are closed once.
    unsafe {
        let _ = CloseHandle(pi.hThread);
        let _ = CloseHandle(pi.hProcess);
    }
    drop(pin);
    Ok(())
}

// ---- F1a.16: single instance and the IPC window -------------------------------------------------------------------

/// The mutex and the IPC class of this process.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct InstanceNames {
    pub mutex: String,
    pub ipc_class: String,
}

impl InstanceNames {
    /// Release builds: the constants. Debug builds: suffixed "-<FILES_TEST_TAG>" when that variable holds a plain tag
    /// (1 to 32 lower-case letters and digits), so tests never meet each other or an installed instance.
    pub fn for_process() -> InstanceNames {
        if cfg!(debug_assertions) {
            if let Some(tag) = std::env::var(names::TEST_TAG_VAR).ok().filter(|t| plain_tag(t)) {
                return InstanceNames::tagged(&tag);
            }
        }
        InstanceNames { mutex: names::MUTEX.to_string(), ipc_class: names::IPC_CLASS.to_string() }
    }

    /// "<MUTEX>-<tag>" and "<IPC_CLASS>-<tag>".
    pub fn tagged(tag: &str) -> InstanceNames {
        InstanceNames { mutex: format!("{}-{tag}", names::MUTEX), ipc_class: format!("{}-{tag}", names::IPC_CLASS) }
    }
}

fn plain_tag(t: &str) -> bool {
    (1..=32).contains(&t.len()) && t.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit())
}

#[derive(Debug)]
pub enum ForwardError {
    NoInstance,
    Refused,
    Timeout,
}

/// Hands \`cmd\` to the running instance: the first window of the IPC class whose process passes the image check
/// (same_image as this exe, same session, integrity at least ours, same token user) gets it through WM_COPYDATA.
/// Ok = ms from this process's creation (logged); Refused = it answered 0; Timeout = it did not answer in 2 s (or
/// hangs); NoInstance = no window passed the check.
pub fn try_forward(n: &InstanceNames, cmd: &InstanceCmd) -> Result<u32, ForwardError> {
    match std::env::current_exe() {
        Ok(exe) => try_forward_as(n, cmd, &exe),
        Err(_) => Err(ForwardError::NoInstance),
    }
}

/// try_forward with the image a candidate must run given: try_forward passes this exe; a test process, whose own
/// image is the test binary, passes files.exe.
pub fn try_forward_as(n: &InstanceNames, cmd: &InstanceCmd, image: &Path) -> Result<u32, ForwardError> {
    let mut frame = Vec::new();
    if oled_trust::frame::encode(cmd, &mut frame).is_err() || frame.len() > MAX_FRAME {
        return Err(ForwardError::Refused);
    }
    let Some(me) = Me::read(image) else { return Err(ForwardError::NoInstance) };
    let class = HSTRING::from(n.ipc_class.as_str());
    let mut prev: Option<HWND> = None;
    for _ in 0..MAX_CANDIDATES {
        // SAFETY: \`class\` is NUL-terminated; the parent is the message-only parent, \`prev\` a window found before or none.
        let Ok(hwnd) = (unsafe { FindWindowExW(Some(HWND_MESSAGE), prev, &class, PCWSTR::null()) }) else { break };
        prev = Some(hwnd);
        let Some(pid) = me.accepts(hwnd) else { continue };
        // SAFETY: no pointers; lets that process bring its window to the front for this command.
        let _ = unsafe { AllowSetForegroundWindow(pid) };
        match send_frame(hwnd, &frame) {
            Sent::Accepted => {
                let ms = ms_since_start();
                oled_sys::log!("files: forwarded in {ms} ms");
                return Ok(ms);
            }
            Sent::Refused => return Err(ForwardError::Refused),
            Sent::Timeout => return Err(ForwardError::Timeout),
            Sent::Gone => {}
        }
    }
    Err(ForwardError::NoInstance)
}

/// What a candidate's process must match.
struct Me {
    image: PathBuf,
    session: u32,
    integrity: Integrity,
    user: String,
}

impl Me {
    /// None when this process cannot read its own facts: then nothing is accepted (the start opens its own window).
    fn read(image: &Path) -> Option<Me> {
        let own = oled_sys::pipe::peer_of_pid(std::process::id()).ok()?;
        let user = oled_sys::pipe::current_user_sid().ok()?;
        Some(Me { image: image.to_path_buf(), session: own.session, integrity: own.integrity, user })
    }

    /// The pid of the window's process when it passes the check.
    fn accepts(&self, hwnd: HWND) -> Option<u32> {
        let mut pid = 0u32;
        // SAFETY: \`pid\` is writable; a window that is gone gives 0.
        unsafe { GetWindowThreadProcessId(hwnd, Some(&mut pid)) };
        if pid == 0 {
            return None;
        }
        let peer = oled_sys::pipe::peer_of_pid(pid).ok()?;
        let ok = oled_sys::ident::same_image(&peer.image, &self.image)
            && peer.session == self.session
            && peer.integrity >= self.integrity
            && oled_sys::pipe::user_sid_of_pid(pid).ok()? == self.user;
        ok.then_some(pid)
    }
}

enum Sent {
    Accepted,
    Refused,
    Timeout,
    /// The window went away (or would not take messages from us): the next candidate is tried.
    Gone,
}

fn send_frame(hwnd: HWND, frame: &[u8]) -> Sent {
    let cds = COPYDATASTRUCT {
        dwData: INSTANCE.magic as usize,
        cbData: frame.len() as u32,
        lpData: frame.as_ptr().cast_mut().cast::<c_void>(),
    };
    let mut result = 0usize;
    // SAFETY: clears this thread's last error, so the one read below is the send's.
    unsafe { SetLastError(WIN32_ERROR(0)) };
    // SAFETY: \`cds\` and the frame it points to live for the call (the system copies them to the receiver);
    // \`result\` is writable.
    let sent = unsafe {
        SendMessageTimeoutW(
            hwnd,
            WM_COPYDATA,
            WPARAM(0),
            LPARAM(std::ptr::from_ref(&cds) as isize),
            SMTO_ABORTIFHUNG,
            SEND_TIMEOUT_MS,
            Some(&mut result),
        )
    };
    if sent.0 != 0 {
        return if result == 1 { Sent::Accepted } else { Sent::Refused };
    }
    // SAFETY: reads this thread's last error, set by the send.
    match unsafe { GetLastError() } {
        ERROR_TIMEOUT | WIN32_ERROR(0) => Sent::Timeout,
        _ => Sent::Gone,
    }
}

/// Milliseconds from this process's creation to now.
fn ms_since_start() -> u32 {
    let mut times = [FILETIME::default(); 4];
    let [created, exited, kernel, user] = &mut times;
    // SAFETY: this process's pseudo handle; the four FILETIMEs are writable.
    if unsafe { GetProcessTimes(GetCurrentProcess(), created, exited, kernel, user) }.is_err() {
        return 0;
    }
    // SAFETY: no arguments.
    let now = unsafe { GetSystemTimeAsFileTime() };
    let ticks = |f: &FILETIME| (u64::from(f.dwHighDateTime) << 32) | u64::from(f.dwLowDateTime);
    let [created, ..] = times;
    u32::try_from(ticks(&now).saturating_sub(ticks(&created)) / 10_000).unwrap_or(u32::MAX)
}

pub enum MutexState {
    Owned(OwnedHandle),
    Busy,
}

/// CreateMutexW of the names' mutex: Owned when this call created it (held for the life of the process), Busy when
/// it existed already (the handle is closed) or cannot be made.
pub fn take_mutex(n: &InstanceNames) -> MutexState {
    let name = HSTRING::from(n.mutex.as_str());
    // SAFETY: clears this thread's last error, so the one read below is CreateMutexW's.
    unsafe { SetLastError(WIN32_ERROR(0)) };
    // SAFETY: \`name\` is NUL-terminated; no attributes; not initially owned (the mutex's existence is the lock).
    let created = unsafe { CreateMutexW(None, false, &name) };
    // SAFETY: reads this thread's last error, set by the call above.
    let last = unsafe { GetLastError() };
    match created {
        Ok(h) => {
            // SAFETY: a fresh handle, owned by nothing else (closed at once when the mutex was not ours).
            let h = unsafe { OwnedHandle::from_raw_handle(h.0 as RawHandle) };
            if last == ERROR_ALREADY_EXISTS {
                MutexState::Busy
            } else {
                MutexState::Owned(h)
            }
        }
        Err(_) => MutexState::Busy,
    }
}

/// The message-only IPC window (HWND_MESSAGE parent: never visible, never top-level, never foreground). Not Send:
/// it is destroyed on the thread that made it, the one whose messages it receives.
pub struct Ipc {
    hwnd: HWND,
    class: HSTRING,
    instance: HINSTANCE,
    /// The window procedure's state, through GWLP_USERDATA: freed only after the window is destroyed.
    _state: Box<IpcState>,
}

struct IpcState {
    hub: Arc<Hub>,
    /// RegisterWindowMessageW(UPDATED_MESSAGE); 0 when it could not be registered.
    updated: u32,
}

impl std::fmt::Debug for Ipc {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Ipc").field("hwnd", &self.hwnd.0).finish()
    }
}

impl Drop for Ipc {
    fn drop(&mut self) {
        // SAFETY: our own window, on its own thread (Ipc is not Send); the slot is cleared first, so no message after
        // this reads the state, which is freed after the window is gone.
        unsafe {
            SetWindowLongPtrW(self.hwnd, GWLP_USERDATA, 0);
            let _ = DestroyWindow(self.hwnd);
            let _ = UnregisterClassW(&self.class, Some(self.instance));
        }
    }
}

/// Registers the names' IPC class and creates its message-only window on this thread: WM_COPYDATA is decoded
/// (decode_copydata) and posted to the hub as Msg::Instance (to the most recently active window), answered 1, or
/// refused with 0; the registered UPDATED_MESSAGE is posted as Msg::Updated { version: wParam } (every window). No
/// message filter is added, so a lower-integrity process cannot send it anything (UIPI).
pub fn create_ipc(n: &InstanceNames, hub: Arc<Hub>) -> Result<Ipc, String> {
    // SAFETY: the module handle of this executable.
    let module = unsafe { GetModuleHandleW(None) }.map_err(|e| format!("no module handle: {e}"))?;
    let instance = HINSTANCE::from(module);
    let class = HSTRING::from(n.ipc_class.as_str());
    let wc = WNDCLASSEXW {
        cbSize: std::mem::size_of::<WNDCLASSEXW>() as u32,
        lpfnWndProc: Some(ipc_proc),
        hInstance: instance,
        lpszClassName: PCWSTR(class.as_ptr()),
        ..Default::default()
    };
    // SAFETY: \`wc\` and the class name it points to live for the call.
    if unsafe { RegisterClassExW(&wc) } == 0 {
        // SAFETY: reads this thread's last error, set by the call above.
        let e = unsafe { GetLastError() };
        if e != ERROR_CLASS_ALREADY_EXISTS {
            return Err(format!("RegisterClassExW failed ({})", e.0));
        }
    }
    // SAFETY: a NUL-terminated name.
    let updated = unsafe { RegisterWindowMessageW(&HSTRING::from(oled_sys::selftest::UPDATED_MESSAGE)) };
    let state = Box::new(IpcState { hub, updated });
    let param = std::ptr::from_ref::<IpcState>(&state).cast::<c_void>();
    // SAFETY: the class registered above; a message-only window, no menu; \`param\` points to the state the window
    // procedure keeps, which Ipc frees only after the window is destroyed.
    let hwnd = unsafe {
        CreateWindowExW(
            WINDOW_EX_STYLE(0),
            &class,
            PCWSTR::null(),
            WINDOW_STYLE(0),
            0,
            0,
            0,
            0,
            Some(HWND_MESSAGE),
            None,
            Some(instance),
            Some(param),
        )
    }
    .map_err(|e| format!("CreateWindowExW failed: {e}"))?;
    Ok(Ipc { hwnd, class, instance, _state: state })
}

unsafe extern "system" fn ipc_proc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    if msg == WM_NCCREATE {
        let cs = lp.0 as *const CREATESTRUCTW;
        if !cs.is_null() {
            // SAFETY: WM_NCCREATE's lParam is this CreateWindowExW call's CREATESTRUCTW; lpCreateParams is create_ipc's
            // state, which outlives the window.
            unsafe { SetWindowLongPtrW(hwnd, GWLP_USERDATA, (*cs).lpCreateParams as isize) };
        }
    }
    // SAFETY: reads this window's own slot.
    let state = unsafe { GetWindowLongPtrW(hwnd, GWLP_USERDATA) } as *const IpcState;
    if !state.is_null() {
        // SAFETY: set at WM_NCCREATE to the state Ipc owns; Ipc clears the slot before it destroys the window and
        // frees the state only afterwards.
        let state = unsafe { &*state };
        if msg == WM_COPYDATA {
            // SAFETY: WM_COPYDATA's lParam points to a COPYDATASTRUCT valid for the duration of the message.
            return LRESULT(isize::from(unsafe { on_copydata(state, lp) }));
        }
        if state.updated != 0 && msg == state.updated {
            state.hub.post(Msg::Updated { version: u32::try_from(wp.0).unwrap_or(0) });
            return LRESULT(0);
        }
        if msg == WM_NCDESTROY {
            // SAFETY: our own window's slot.
            unsafe { SetWindowLongPtrW(hwnd, GWLP_USERDATA, 0) };
        }
    }
    // SAFETY: the default handling of a message of our own window.
    unsafe { DefWindowProcW(hwnd, msg, wp, lp) }
}

/// One WM_COPYDATA: the bytes copied out of the message, decoded and posted. Never navigates: the window gets the
/// command as a message after the send has returned.
///
/// SAFETY: \`lp\` is WM_COPYDATA's lParam, valid for the duration of the message.
unsafe fn on_copydata(state: &IpcState, lp: LPARAM) -> bool {
    let cds = lp.0 as *const COPYDATASTRUCT;
    if cds.is_null() {
        return false;
    }
    // SAFETY: the caller's contract.
    let cds = unsafe { &*cds };
    let len = cds.cbData as usize;
    if len > MAX_FRAME || (len > 0 && cds.lpData.is_null()) {
        return false;
    }
    let bytes: &[u8] = if len == 0 {
        &[]
    } else {
        // SAFETY: lpData holds cbData readable bytes for the duration of the message (checked non-null above).
        unsafe { std::slice::from_raw_parts(cds.lpData.cast::<u8>(), len) }
    };
    match decode_copydata(cds.dwData, bytes) {
        Some(cmd) => {
            state.hub.post(Msg::Instance(cmd));
            true
        }
        None => false,
    }
}

/// The command of one WM_COPYDATA, or None (answered 0): dwData must be INSTANCE's magic, the frame at most MAX_FRAME
/// bytes and decoded whole (nothing after it), every path a WidePath, a Select's item inside its dir, and a location
/// one a window can show (a folder or Home): Search and Options are refused in this beta. Pure.
pub fn decode_copydata(dw_data: usize, bytes: &[u8]) -> Option<InstanceCmd> {
    if dw_data != INSTANCE.magic as usize || bytes.len() > MAX_FRAME {
        return None;
    }
    let (cmd, used) = oled_trust::frame::decode::<InstanceCmd>(bytes).ok()?;
    (used == bytes.len() && accepted(&cmd)).then_some(cmd)
}

fn accepted(cmd: &InstanceCmd) -> bool {
    let path = |p: &WidePath| WidePath::new(p.units()).is_ok_and(|q| q == *p);
    match cmd {
        InstanceCmd::NewWindow(at) | InstanceCmd::OpenTab(at) => match at {
            Location::Fs(p) => path(p),
            Location::Home => true,
            Location::Search { .. } | Location::Options => false,
        },
        InstanceCmd::Select { dir, item } => {
            path(dir) && path(item) && item.parent().is_some_and(|parent| same_path(&parent, dir))
        }
        InstanceCmd::Probe { path: p } => path(p),
    }
}

/// The IPC window first (a new start then finds nothing to forward to), then the mutex.
pub fn release(m: MutexState, ipc: Option<Ipc>) {
    drop(ipc);
    drop(m);
}

/// A command from another process, on the UI thread (the data hook, Msg::Instance): linger::on_instance first; then
/// only loop requests and messages, never a window call (the loop callback shows the window: restore if minimised,
/// SetForegroundWindow, allowed because the sender called AllowSetForegroundWindow for this process).
/// NewWindow: a new window cascaded from this one. Select: this window when its active tab shows dir (the item is
/// selected and centred, the window comes to the front), else another window already on dir (the command is posted
/// to it), else a new window on dir with the item selected. OpenTab: a tab here (then the front), else a new window.
/// Probe opens nothing; Search and Options never arrive (decode_copydata refuses them).
pub fn apply(s: &mut FilesScreen, cx: &mut EventCx, cmd: InstanceCmd) -> Reply {
    if linger::on_instance(s, cx, &cmd) {
        return Reply::Redraw;
    }
    let shared = Rc::clone(s.shared());
    let me = s.win();
    let new_window = |first: Location, select: Option<WidePath>| {
        window::request_loop(&shared, LoopReq::Main { first, select, cascade_from: Some(me) });
    };
    match cmd {
        InstanceCmd::NewWindow(at) if shows(&at) => {
            new_window(at, None);
            Reply::None
        }
        InstanceCmd::OpenTab(at) if shows(&at) => {
            if tab::open_here(s, cx, at.clone(), true) {
                window::request_loop(&shared, LoopReq::Show { win: me, how: Show::Front });
                Reply::Relayout
            } else {
                new_window(at, None);
                Reply::None
            }
        }
        InstanceCmd::Select { dir, item } => {
            let here = matches!(s.tabs.active().nav.location(), Location::Fs(p) if same_path(p, &dir));
            if here {
                select_here(s, cx, &item);
                window::request_loop(&shared, LoopReq::Show { win: me, how: Show::Front });
                return Reply::Redraw;
            }
            let other = shared
                .windows
                .borrow()
                .iter()
                .find(|w| {
                    w.win != me
                        && w.kind == WinKind::Main
                        && matches!(&w.location, Some(Location::Fs(p)) if same_path(p, &dir))
                })
                .map(|w| w.win);
            let posted = other.is_some_and(|win| {
                let cmd = InstanceCmd::Select { dir: dir.clone(), item: item.clone() };
                shared.hub.post_to(win, Msg::Instance(cmd))
            });
            if !posted {
                new_window(Location::Fs(dir), Some(item));
            }
            Reply::None
        }
        _ => Reply::None,
    }
}

/// A location a window can show: a folder or Home.
fn shows(at: &Location) -> bool {
    matches!(at, Location::Fs(_) | Location::Home)
}

/// Selects and centres the item in the active tab. One not listed yet (a listing still loading, a file just made) is
/// left alone: the folder is not listed again, so a listing in progress is not restarted.
fn select_here(s: &mut FilesScreen, cx: &mut EventCx, item: &WidePath) {
    let mut name = Vec::new();
    if let Some(units) = item.file_name() {
        utf16_to_wtf8(units, &mut name);
    }
    let table = &mut s.tabs.active_mut().table;
    let found = table.model().listing().find(&name);
    if let Some(id) = found {
        let k = model::key_of(id);
        table.set_selection(cx, Selection::Single(k));
        table.set_focused(cx, k, ScrollTo::Center);
        status::selection_changed(s, cx);
    }
}

/// Two paths name the same folder: equal ignoring case (each UTF-16 unit folded to lower case where that is one unit).
fn same_path(a: &WidePath, b: &WidePath) -> bool {
    let (a, b) = (a.units(), b.units());
    a.len() == b.len() && a.iter().zip(b).all(|(&x, &y)| x == y || fold(x) == fold(y))
}

fn fold(u: u16) -> u16 {
    let Some(c) = char::from_u32(u32::from(u)) else { return u };
    let mut lower = c.to_lowercase();
    match (lower.next(), lower.next()) {
        (Some(l), None) => u16::try_from(u32::from(l)).unwrap_or(u),
        _ => u,
    }
}

// ---- F1a.17: process identity -------------------------------------------------------------------------------------

/// SetCurrentProcessExplicitAppUserModelID(AUMID), before the first window (X11): the taskbar groups the windows
/// under the shortcut P2-plumbing's I1 writes with the same id.
pub fn set_identity() {
    // SAFETY: a NUL-terminated string that lives for the call.
    if let Err(e) = unsafe { SetCurrentProcessExplicitAppUserModelID(&HSTRING::from(names::AUMID)) } {
        oled_sys::log!("files: the process identity was not set ({e})");
    }
}

// ---- the instance tests' IPC role (debug builds only) --------------------------------------------------------------

/// files.exe --role=test-ipc (debug builds only), for tests/instance.rs. Needs FILES_TEST_TAG (the real names are
/// never taken) and FILES_TEST_PARENT (it exits when that process exits). Plain form: takes the mutex, creates the IPC
/// window, prints "ready <pid>" and appends every command it receives as one JSON line to FILES_TEST_OUT, a file in a
/// mod-files-test- folder. "--mutex-only": two threads call take_mutex at once and print "owned" or "busy" each; the
/// results are held until the parent exits.
#[cfg(debug_assertions)]
pub fn test_ipc_role(args: &[String]) -> i32 {
    test_role::run(args)
}

#[cfg(debug_assertions)]
mod test_role {
    use std::io::Write;
    use std::os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle, RawHandle};
    use std::path::PathBuf;
    use std::sync::{Arc, Barrier, Mutex, PoisonError};

    use oled_trust::frame::files::{InstanceCmd, Location, WidePath};
    use oled_ui::host::AppMsg;
    use windows::Win32::Foundation::{HANDLE, WAIT_OBJECT_0};
    use windows::Win32::System::Threading::{OpenProcess, WaitForSingleObject, INFINITE, PROCESS_SYNCHRONIZE};
    use windows::Win32::UI::WindowsAndMessaging::{
        DispatchMessageW, MsgWaitForMultipleObjectsEx, PeekMessageW, TranslateMessage, MSG, MWMO_INPUTAVAILABLE,
        PM_REMOVE, QS_ALLINPUT, WM_QUIT,
    };

    use super::{create_ipc, release, take_mutex, InstanceNames, MutexState};
    use crate::hub::{Hub, Poster};
    use crate::msg::Msg;
    use crate::names;
    use crate::own_files::OwnDir;

    /// The folder prefix of a test's TempRoot (tests/common/temp.rs).
    const TEST_DIR_PREFIX: &str = "mod-files-test-";

    pub(super) fn run(args: &[String]) -> i32 {
        let names = InstanceNames::for_process();
        if names.mutex == names::MUTEX {
            return names::EXIT_USAGE;
        }
        let Some(parent) = parent() else { return names::EXIT_USAGE };
        match args {
            [] => serve(&names, &parent),
            [flag] if flag == "--mutex-only" => race(&names, &parent),
            _ => names::EXIT_USAGE,
        }
    }

    fn say(line: &str) {
        let mut out = std::io::stdout().lock();
        let _ = writeln!(out, "{line}");
        let _ = out.flush();
    }

    /// FILES_TEST_PARENT, opened for waiting.
    fn parent() -> Option<OwnedHandle> {
        let pid = std::env::var("FILES_TEST_PARENT").ok()?.parse::<u32>().ok()?;
        // SAFETY: a plain open by id for waiting on; the handle is owned below.
        let h = unsafe { OpenProcess(PROCESS_SYNCHRONIZE, false, pid) }.ok()?;
        // SAFETY: a fresh handle, owned by nothing else.
        Some(unsafe { OwnedHandle::from_raw_handle(h.0 as RawHandle) })
    }

    /// FILES_TEST_OUT, opened for appending through own_files (the only writer of the package) when it lies in a
    /// test folder.
    fn out_file() -> Option<std::fs::File> {
        let path = PathBuf::from(std::env::var_os("FILES_TEST_OUT")?);
        let dir = path.parent()?;
        let name = path.file_name()?.to_str()?;
        let fenced = dir
            .components()
            .any(|c| c.as_os_str().to_str().is_some_and(|s| s.to_ascii_lowercase().starts_with(TEST_DIR_PREFIX)));
        if !path.is_absolute() || !fenced {
            return None;
        }
        OwnDir::open(dir).ok()?.append(name).ok()
    }

    fn serve(names: &InstanceNames, parent: &OwnedHandle) -> i32 {
        let Some(out) = out_file() else { return names::EXIT_USAGE };
        let mutex = take_mutex(names);
        if matches!(mutex, MutexState::Busy) {
            say("busy");
            return 1;
        }
        let hub = Hub::new();
        hub.attach(1, Box::new(OutPoster(Mutex::new(out))));
        let ipc = match create_ipc(names, hub) {
            Ok(i) => i,
            Err(e) => {
                say(&format!("error {e}"));
                return 1;
            }
        };
        say(&format!("ready {}", std::process::id()));
        pump_until(parent);
        release(mutex, Some(ipc));
        names::EXIT_OK
    }

    fn race(names: &InstanceNames, parent: &OwnedHandle) -> i32 {
        let start = Arc::new(Barrier::new(2));
        let threads: Vec<_> = (0..2)
            .map(|_| {
                let (names, start) = (names.clone(), Arc::clone(&start));
                std::thread::spawn(move || {
                    start.wait();
                    take_mutex(&names)
                })
            })
            .collect();
        let states: Vec<MutexState> = threads.into_iter().filter_map(|t| t.join().ok()).collect();
        for s in &states {
            say(if matches!(s, MutexState::Owned(_)) { "owned" } else { "busy" });
        }
        // Held until the parent exits, so the other process of the race still meets this one's mutex.
        // SAFETY: a live process handle; waiting has no other effect.
        unsafe { WaitForSingleObject(HANDLE(parent.as_raw_handle()), INFINITE) };
        drop(states);
        names::EXIT_OK
    }

    /// Dispatches this thread's messages (the IPC window's) until the parent exits.
    fn pump_until(parent: &OwnedHandle) {
        let handles = [HANDLE(parent.as_raw_handle())];
        loop {
            // SAFETY: a live wait handle of this process.
            let r = unsafe { MsgWaitForMultipleObjectsEx(Some(&handles), INFINITE, QS_ALLINPUT, MWMO_INPUTAVAILABLE) };
            if r == WAIT_OBJECT_0 {
                return;
            }
            if r.0 != WAIT_OBJECT_0.0 + 1 {
                // WAIT_FAILED cannot come from a live handle; never spin on it.
                std::thread::sleep(std::time::Duration::from_millis(50));
                continue;
            }
            let mut msg = MSG::default();
            // SAFETY: \`msg\` is writable; messages of this thread only.
            while unsafe { PeekMessageW(&mut msg, None, 0, 0, PM_REMOVE) }.as_bool() {
                if msg.message == WM_QUIT {
                    return;
                }
                // SAFETY: a message this thread just received.
                unsafe {
                    let _ = TranslateMessage(&msg);
                    DispatchMessageW(&msg);
                }
            }
        }
    }

    /// The window the hub posts to: each command becomes one JSON line of the out file.
    struct OutPoster(Mutex<std::fs::File>);

    impl Poster for OutPoster {
        fn post(&self, m: AppMsg) -> bool {
            let Ok(m) = m.downcast::<Msg>() else { return false };
            let line = match *m {
                Msg::Instance(c) => command_json(&c),
                Msg::Updated { version } => serde_json::json!({ "updated": version }),
                _ => return true,
            };
            let mut f = self.0.lock().unwrap_or_else(PoisonError::into_inner);
            let _ = writeln!(f, "{line}");
            let _ = f.flush();
            true
        }
    }

    fn command_json(c: &InstanceCmd) -> serde_json::Value {
        let path = |p: &WidePath| serde_json::Value::from(p.to_string_lossy());
        let at = |l: &Location| match l {
            Location::Fs(p) => serde_json::json!({ "fs": path(p) }),
            Location::Home => serde_json::json!("home"),
            Location::Search { .. } => serde_json::json!("search"),
            Location::Options => serde_json::json!("options"),
        };
        match c {
            InstanceCmd::NewWindow(l) => serde_json::json!({ "new_window": at(l) }),
            InstanceCmd::OpenTab(l) => serde_json::json!({ "open_tab": at(l) }),
            InstanceCmd::Select { dir, item } => {
                serde_json::json!({ "select": { "dir": path(dir), "item": path(item) } })
            }
            InstanceCmd::Probe { path: p } => serde_json::json!({ "probe": path(p) }),
        }
    }
}

#[cfg(test)]
mod tests {
    use oled_ui::harness::Harness;
    use oled_ui::{SizeL, Tokens};

    use super::*;
    use crate::fakes::{self, Fakes};
    use crate::window::STARTUP_TAB;

    fn wp(s: &str) -> WidePath {
        WidePath::parse(s).unwrap_or_else(|e| panic!("{s}: {e:?}"))
    }

    #[test]
    fn select_in_this_window_selects_and_comes_to_the_front() {
        let f = Fakes::new();
        let shared = f.shared(oled_ui::UiLang::EN);
        let dir = wp(r"C:\mod-files-made-up\projects");
        let mut s = FilesScreen::new(Rc::clone(&shared), 1, Location::Fs(dir.clone()), None, STARTUP_TAB);
        {
            let l = s.tabs.active_mut().table.model_mut().listing_mut();
            l.append(fakes::listing(&[("notes.txt", false, 10, 0), ("report-final-v2.pdf", false, 20, 0)]));
            assert!(l.merge_pending());
        }
        let mut h = Harness::new(s, SizeL { w: 800, h: 500 }, 96, Tokens::oled());
        let item = wp(r"C:\mod-files-made-up\PROJECTS\report-final-v2.pdf");
        h.send(Box::new(Msg::Instance(InstanceCmd::Select { dir: wp(r"C:\MOD-FILES-MADE-UP\projects"), item })));
        let table = &h.screen().tabs().active().table;
        let id = table.model().listing().find(b"report-final-v2.pdf").unwrap_or_else(|| panic!("listed"));
        assert_eq!(table.selection(), &Selection::Single(model::key_of(id)), "selected, case ignored");
        assert_eq!(table.focused(), Some(model::key_of(id)));
        assert_eq!(*shared.loop_requests.borrow(), [LoopReq::Show { win: 1, how: Show::Front }]);
    }

    #[test]
    fn select_of_an_item_not_listed_only_comes_to_the_front() {
        let f = Fakes::new();
        let shared = f.shared(oled_ui::UiLang::EN);
        let dir = wp(r"C:\mod-files-made-up\projects");
        let s = FilesScreen::new(Rc::clone(&shared), 1, Location::Fs(dir.clone()), None, STARTUP_TAB);
        let gen = s.tabs().active().nav.gen();
        let mut h = Harness::new(s, SizeL { w: 800, h: 500 }, 96, Tokens::oled());
        let item = wp(r"C:\mod-files-made-up\projects\made-just-now.txt");
        h.send(Box::new(Msg::Instance(InstanceCmd::Select { dir: dir.clone(), item })));
        let tab = h.screen().tabs().active();
        assert_eq!(tab.table.selection(), &Selection::None, "nothing to select");
        assert_eq!(tab.nav.location(), &Location::Fs(dir));
        assert_eq!(tab.nav.gen(), gen, "the folder is not listed again");
        assert_eq!(*shared.loop_requests.borrow(), [LoopReq::Show { win: 1, how: Show::Front }]);
    }

    #[test]
    fn copydata_decoding_table() {
        let dir = wp(r"C:\mod-files-made-up\projects");
        let frame = |c: &InstanceCmd| {
            let mut v = Vec::new();
            oled_trust::frame::encode(c, &mut v).unwrap_or_else(|e| panic!("{e:?}"));
            v
        };
        let magic = INSTANCE.magic as usize;
        for ok in [
            InstanceCmd::NewWindow(Location::Home),
            InstanceCmd::NewWindow(Location::Fs(dir.clone())),
            InstanceCmd::OpenTab(Location::Fs(dir.clone())),
            InstanceCmd::Select { dir: dir.clone(), item: wp(r"C:\mod-files-made-up\projects\a.txt") },
            InstanceCmd::Probe { path: dir.clone() },
        ] {
            assert_eq!(decode_copydata(magic, &frame(&ok)), Some(ok.clone()), "{ok:?}");
        }
        let good = frame(&InstanceCmd::NewWindow(Location::Fs(dir.clone())));
        assert_eq!(decode_copydata(magic ^ 1, &good), None, "another dwData");
        assert_eq!(decode_copydata(magic, good.get(..good.len() - 1).unwrap_or_default()), None, "truncated");
        assert_eq!(decode_copydata(magic, &[good.as_slice(), &[0]].concat()), None, "trailing bytes");
        assert_eq!(decode_copydata(magic, &[]), None, "empty");
        assert_eq!(decode_copydata(magic, &vec![0u8; MAX_FRAME + 1]), None, "over the cap");
        let search = InstanceCmd::NewWindow(Location::Search { root: dir.clone(), query: "x".into() });
        assert_eq!(decode_copydata(magic, &frame(&search)), None, "Search is refused");
        assert_eq!(decode_copydata(magic, &frame(&InstanceCmd::OpenTab(Location::Options))), None, "Options too");
        let elsewhere = InstanceCmd::Select { dir: dir.clone(), item: wp(r"C:\mod-files-made-up\other\a.txt") };
        assert_eq!(decode_copydata(magic, &frame(&elsewhere)), None, "an item outside its dir");
    }

    /// The file information of the folder `p`, as a pin would read it from its handle.
    fn info_of(p: &Path) -> BY_HANDLE_FILE_INFORMATION {
        let z: Vec<u16> = p.as_os_str().encode_wide().chain([0]).collect();
        // SAFETY: `z` is NUL-terminated and lives for the call; no security attributes, no template.
        let h = unsafe {
            CreateFileW(
                PCWSTR(z.as_ptr()),
                FILE_READ_ATTRIBUTES.0,
                FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE,
                None,
                OPEN_EXISTING,
                FILE_FLAG_BACKUP_SEMANTICS,
                None,
            )
        }
        .unwrap_or_else(|e| panic!("{}: {e}", p.display()));
        // SAFETY: `h` was just returned open by CreateFileW and nothing else owns it.
        let handle = unsafe { OwnedHandle::from_raw_handle(h.0) };
        let mut info = BY_HANDLE_FILE_INFORMATION::default();
        // SAFETY: the handle is open for the whole call; `info` is a writable struct of the declared type.
        unsafe { GetFileInformationByHandle(HANDLE(handle.as_raw_handle()), &mut info) }
            .unwrap_or_else(|e| panic!("{e}"));
        info
    }

    #[test]
    fn same_object_tells_the_pinned_folder_from_another() {
        let here = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        let above = here.parent().unwrap_or_else(|| panic!("a parent")).to_path_buf();
        let units = |p: &Path| p.as_os_str().encode_wide().collect::<Vec<u16>>();
        let pinned = info_of(&here);
        assert!(same_object(&units(&here), &pinned), "the folder itself");
        assert!(!same_object(&units(&above), &pinned), "another folder on the same volume");
        assert!(!same_object(&units(&here.join("no-such-folder")), &pinned), "nothing");
    }

    #[test]
    fn paths_compare_ignoring_case() {
        assert!(same_path(&wp(r"C:\Reports\Àbc"), &wp(r"c:\reports\àBC")));
        assert!(!same_path(&wp(r"C:\Reports\a"), &wp(r"C:\Reports\b")));
        assert!(!same_path(&wp(r"C:\Reports"), &wp(r"C:\Reports\b")));
    }
}
