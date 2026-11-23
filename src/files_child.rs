//! `dock.exe --launcher-files` (plan L2-F2): the launcher's file search, in a short-lived child of the dock, so the
//! search client (OLE DB and the Windows Search DLLs, which stay loaded once used) never enters the dock (D1). The dock
//! starts it when the first files request of an open is due and ends it at the close (launcher_files.rs). It checks once
//! that it can answer (its user against the shell's, the WSearch service, the shell user's roots, the index) and
//! says so in its state frame; then it answers each request line on stdin with one rows frame on stdout, until stdin
//! ends (Appendix A of the plan). It runs with the dock's token and reads index columns only: it never opens a listed
//! file, and a row outside its roots is skipped here, never sent. For each row it sends it asks Windows whether the
//! type of the name's extension is dangerous (`AssocIsDangerous`, once per extension per query: the shell's association
//! code stays out of the dock too) and says so in the row's flags. It has no logger and writes nothing but its frames
//! (lint `search_stays_in_the_files_helper`): the words it gets are typed text, given to Windows Search only.
#![deny(clippy::unwrap_used, clippy::expect_used, clippy::indexing_slicing)]

use crate::launcher::files::{self, under, Fences, FileHit};
use crate::launcher::providers;
use crate::launcher_sys::same_text;
use std::io::{BufRead, Read, Write};
use windows::core::{w, Interface, PCWSTR, PWSTR};
use windows::Win32::Foundation::{CloseHandle, HANDLE};
use windows::Win32::Security::{
    EqualSid, GetTokenInformation, TokenUser, TOKEN_DUPLICATE, TOKEN_IMPERSONATE, TOKEN_QUERY, TOKEN_USER,
};
use windows::Win32::System::Com::{
    CoCreateInstance, CoInitializeEx, CoTaskMemFree, CoUninitialize, CLSCTX_INPROC_SERVER, CLSCTX_LOCAL_SERVER,
    COINIT_MULTITHREADED,
};
use windows::Win32::System::Search::{
    CSearchManager, IAccessor, ICommandText, IDBCreateCommand, IDBCreateSession, IDBInitialize, IDataInitialize,
    IRowset, ISearchManager, ISearchQueryHelper, DBACCESSOR_ROWDATA, DBBINDING, DBMEMOWNER_PROVIDEROWNED,
    DBPART_LENGTH, DBPART_STATUS, DBPART_VALUE, DBTYPE_BYREF, DBTYPE_WSTR, HACCESSOR, MSDAINITIALIZE,
};
use windows::Win32::System::Services::{
    CloseServiceHandle, OpenSCManagerW, OpenServiceW, QueryServiceStatus, SC_MANAGER_CONNECT, SERVICE_QUERY_STATUS,
    SERVICE_RUNNING, SERVICE_STATUS,
};
use windows::Win32::System::Threading::{
    GetCurrentProcess, OpenProcess, OpenProcessToken, PROCESS_QUERY_LIMITED_INFORMATION,
};
use windows::Win32::UI::Shell::{
    AssocIsDangerous, FOLDERID_CommonPrograms, FOLDERID_Desktop, FOLDERID_Documents, FOLDERID_Downloads,
    FOLDERID_Music, FOLDERID_Pictures, FOLDERID_Profile, FOLDERID_ProgramFiles, FOLDERID_ProgramFilesX86,
    FOLDERID_UserProfiles, FOLDERID_Videos, FOLDERID_Windows, SHGetKnownFolderPath, KF_FLAG_DEFAULT,
    KF_FLAG_DONT_VERIFY, KNOWN_FOLDER_FLAG,
};
use windows::Win32::UI::WindowsAndMessaging::{GetShellWindow, GetWindowThreadProcessId};

/// Exit codes: 0 stdin ended (or a state other than 0 was sent), 2 a request line the helper refuses, 3 stdout failed.
const EXIT_BAD_LINE: i32 = 2;
const EXIT_WRITE: i32 = 3;
/// System.ItemType of a folder.
const DIRECTORY: &str = "Directory";
/// The longest System.ItemType read (UTF-16 units).
const TYPE_UNITS: usize = 64;
/// The longest System.ItemUrl read: `file:` and the longest path the dock accepts.
const URL_UNITS: usize = 5 + files::PATH_UNITS;
/// DBGUID_DEFAULT {c8b521fb-5cf3-11ce-ade5-00aa0044773d}: the command's dialect.
const DBGUID_DEFAULT: windows::core::GUID = windows::core::GUID::from_u128(0xc8b521fb_5cf3_11ce_ade5_00aa0044773d);
/// DBSTATUS_S_ISNULL: the column has no value.
const DBSTATUS_S_ISNULL: u32 = 3;

// ---------------------------------------------------------------- rows (tested)

/// One column of a row as the rowset gave it: no value, a text read by its bound length, or one refused (longer than
/// its cap, an odd byte length, bad UTF-16, a status other than OK).
#[derive(Clone, PartialEq, Eq, Debug)]
enum Cell {
    Null,
    Text(String),
    Bad,
}

/// One row: System.ItemUrl, System.FileName, System.ItemFolderNameDisplay, System.ItemType. The URL, not
/// System.ItemPathDisplay: on a localized Windows the display path names the folders as Explorer shows them
/// ("C:\Utenti\..."), a path that exists nowhere, so neither the root fence nor Open could use it.
#[derive(Clone, PartialEq, Eq, Debug)]
struct RawRow {
    url: Cell,
    name: Cell,
    folder: Cell,
    kind: Cell,
}

/// The rows of one query, one at a time (the rowset here, a list in the tests).
trait Rows {
    fn next(&mut self) -> Option<RawRow>;
}

/// The file-system path of an index URL: `file:` (any case), then the path with `/` for each separator
/// (`file:C:/Users/a/b.txt`, `file://server/share/b.txt`); a file name cannot hold a `/`. None for any other scheme.
fn path_of_url(url: &str) -> Option<String> {
    let rest = url.get(..5).filter(|s| s.eq_ignore_ascii_case("file:")).and(url.get(5..))?;
    Some(rest.replace('/', "\\"))
}

/// A row the dock may be sent: its URL names a file-system path, every check of the dock's reader
/// (`files::check_hit`) passes, and the path lies under one of the helper's roots (the SQL Windows builds around
/// the typed text is the only other fence). None: it is skipped, and only counted.
fn row_of(raw: RawRow, roots: &[String]) -> Option<FileHit> {
    let (Cell::Text(url), Cell::Text(name)) = (raw.url, raw.name) else { return None };
    let path = path_of_url(&url)?;
    let folder = match raw.folder {
        Cell::Text(f) => f,
        Cell::Null => String::new(),
        Cell::Bad => return None,
    };
    let dir = matches!(&raw.kind, Cell::Text(k) if k.eq_ignore_ascii_case(DIRECTORY));
    files::check_hit(&path, &name, &folder).ok()?;
    if !roots.iter().any(|r| under(&path, r, same_text)) {
        return None;
    }
    Some(FileHit { path: path.into(), name: name.into(), folder: folder.into(), dir, dangerous: false })
}

/// At most 50 rows of a query, mapped: the hits in Windows' order and the number skipped. Each hit with an extension
/// (`providers::extension` of its name, which `check_hit` holds to the path's last component with no dot segment, so
/// it is the one `file_class` reads of the path) is marked dangerous when `ask` says so; `ask` is called once per
/// extension of the query (ASCII case-insensitive), never for a skipped row or a name without one.
fn collect(rows: &mut dyn Rows, roots: &[String], ask: &mut dyn FnMut(&str) -> bool) -> (Vec<FileHit>, u32) {
    let mut hits = Vec::new();
    let mut skipped = 0u32;
    for _ in 0..files::MAX_ROWS {
        let Some(raw) = rows.next() else { break };
        match row_of(raw, roots) {
            Some(h) => hits.push(h),
            None => skipped += 1,
        }
    }
    // An earlier hit of the same extension holds Windows' answer (at most 50 hits: no map needed).
    for i in 0..hits.len() {
        let (done, rest) = hits.split_at_mut(i);
        let Some(h) = rest.first_mut() else { break };
        let Some(ext) = providers::extension(&h.name) else { continue };
        let same = |d: &&FileHit| providers::extension(&d.name).is_some_and(|e| e.eq_ignore_ascii_case(ext));
        h.dangerous = match done.iter().find(same) {
            Some(d) => d.dangerous,
            None => ask(ext),
        };
    }
    (hits, skipped)
}

/// What one request gets: its hits and skipped rows, or None when the query failed.
type Answer = Option<(Vec<FileHit>, u32)>;

/// The helper's whole conversation: the header and the state frame, then (state 0 only) one rows frame per request
/// line, each flushed at once. The words of a line go to `query` joined by single spaces; a line with none gets an
/// empty frame. 0 at the end of stdin (or after a state other than 0), 2 at a line the helper refuses, 3 when stdout
/// fails.
fn serve(state: u32, input: &mut impl BufRead, out: &mut impl Write, query: &mut dyn FnMut(&str) -> Answer) -> i32 {
    let mut frame = files::HEADER.to_vec();
    files::encode_state(state, &mut frame);
    if out.write_all(&frame).and_then(|()| out.flush()).is_err() {
        return EXIT_WRITE;
    }
    if state != files::STATE_READY {
        return 0;
    }
    let mut line = Vec::with_capacity(files::MAX_LINE_BYTES + 1);
    loop {
        line.clear();
        let cap = u64::try_from(files::MAX_LINE_BYTES + 1).unwrap_or(u64::MAX);
        match input.by_ref().take(cap).read_until(b'\n', &mut line) {
            Ok(0) | Err(_) => return 0,
            Ok(_) => {}
        }
        let Ok((gen, text)) = files::parse_request_line(&line) else { return EXIT_BAD_LINE };
        let words = files::words_of(text);
        let answer = if words.is_empty() { Some((Vec::new(), 0)) } else { query(&words.join(" ")) };
        frame.clear();
        match answer {
            Some((hits, skipped)) => files::encode_rows(gen, files::STATUS_OK, skipped, &hits, &mut frame),
            None => files::encode_rows(gen, files::STATUS_FAILED, 0, &[], &mut frame),
        }
        if out.write_all(&frame).and_then(|()| out.flush()).is_err() {
            return EXIT_WRITE;
        }
    }
}

// ---------------------------------------------------------------- Windows

/// Runs the helper (the child mode `--launcher-files`, dispatched by main before the data folder, the logger, the
/// mutex and the watchdog). `stall_ms`, from `--stall` in owner-probe builds only, sleeps before each query. `extra`,
/// from `--root`: the folders of `launcher.file_folders` the dock accepted, checked again (`with_folders`).
pub(crate) fn run(stall_ms: u32, extra: &[String]) -> i32 {
    // SAFETY: the first COM call of the helper's only thread; paired with CoUninitialize below.
    let com = unsafe { CoInitializeEx(None, COINIT_MULTITHREADED) }.is_ok();
    let (state, search) = if com { setup(stall_ms, extra) } else { (files::STATE_NO_INDEX, None) };
    let code = serve(state, &mut std::io::stdin().lock(), &mut std::io::stdout().lock(), &mut |text| {
        search.as_ref().and_then(|s| s.query(text))
    });
    drop(search);
    if com {
        // SAFETY: paired with the successful CoInitializeEx above; the search objects were released just before.
        unsafe { CoUninitialize() };
    }
    code
}

/// Closes the handle on drop.
struct Handle(HANDLE);

impl Drop for Handle {
    fn drop(&mut self) {
        // SAFETY: a handle this helper opened, closed once.
        let _ = unsafe { CloseHandle(self.0) };
    }
}

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

/// The text of a string a COM call allocated (a known folder, the SQL, the connection string), freed here.
fn take_cotask(p: PWSTR) -> Option<String> {
    // SAFETY: `p` was returned by the call that allocated it with CoTaskMemAlloc, NUL-terminated; read once, freed once.
    let s = unsafe { p.to_string() }.ok();
    // SAFETY: as above; not used afterwards.
    unsafe { CoTaskMemFree(Some(p.0 as *const core::ffi::c_void)) };
    s
}

/// The TOKEN_USER of a token, in an 8-byte-aligned buffer of at most 1 KiB.
fn token_user(token: HANDLE) -> Option<Vec<u64>> {
    let mut len = 0u32;
    // SAFETY: a size query only, no buffer.
    let _ = unsafe { GetTokenInformation(token, TokenUser, None, 0, &mut len) };
    if !(std::mem::size_of::<TOKEN_USER>()..=1024).contains(&(len as usize)) {
        return None;
    }
    let mut buf = vec![0u64; (len as usize).div_ceil(8)];
    // SAFETY: `buf` holds at least `len` writable bytes.
    unsafe { GetTokenInformation(token, TokenUser, Some(buf.as_mut_ptr().cast()), len, &mut len) }.ok()?;
    Some(buf)
}

/// Both buffers hold a TOKEN_USER (`token_user`) of the same user.
fn same_user(a: &[u64], b: &[u64]) -> bool {
    // SAFETY: each buffer was filled by GetTokenInformation(TokenUser), is at least size_of::<TOKEN_USER>() bytes and
    // 8-byte aligned; the SIDs point inside them and live as long.
    unsafe {
        let (a, b) = (&*a.as_ptr().cast::<TOKEN_USER>(), &*b.as_ptr().cast::<TOKEN_USER>());
        EqualSid(a.User.Sid, b.User.Sid).is_ok()
    }
}

/// The token of the shell window's process (query, duplicate and impersonate: the known-folder lookup needs them).
fn shell_token() -> Option<Handle> {
    // SAFETY: plain queries; the handles opened are owned by `Handle`.
    unsafe {
        let w = GetShellWindow();
        if w.0.is_null() {
            return None;
        }
        let mut pid = 0u32;
        GetWindowThreadProcessId(w, Some(&mut pid));
        if pid == 0 {
            return None;
        }
        let process = Handle(OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid).ok()?);
        let mut t = HANDLE::default();
        OpenProcessToken(process.0, TOKEN_QUERY | TOKEN_DUPLICATE | TOKEN_IMPERSONATE, &mut t).ok()?;
        Some(Handle(t))
    }
}

/// The WSearch service is running; a failure to ask counts as running (the index check follows).
fn search_running() -> bool {
    // SAFETY: the service handles are opened with query rights only and closed before the return.
    unsafe {
        let Ok(scm) = OpenSCManagerW(PCWSTR::null(), PCWSTR::null(), SC_MANAGER_CONNECT) else { return true };
        let running = match OpenServiceW(scm, w!("WSearch"), SERVICE_QUERY_STATUS) {
            Ok(svc) => {
                let mut st = SERVICE_STATUS::default();
                let r = QueryServiceStatus(svc, &mut st).map_or(true, |()| st.dwCurrentState == SERVICE_RUNNING);
                let _ = CloseServiceHandle(svc);
                r
            }
            Err(_) => true,
        };
        let _ = CloseServiceHandle(scm);
        running
    }
}

/// The helper's first two roots (SPEC 5.4.6): the shell user's profile, then the common Start Menu Programs folder
/// unless it holds the profile. Both paths come from HKLM, which only an administrator can change. The user's own
/// Programs folder is not a root: it lies under the profile by default, and its path comes from HKCU, which any process
/// of the user can point at C:\Users or C:\, where the helper's elevated token would let the index list other accounts'
/// files; the known folders that HKCU also holds join only as `files::root_of` accepts them (`with_folders`).
fn roots_of(profile: String, common: String) -> Vec<String> {
    if under(&profile, &common, same_text) {
        vec![profile]
    } else {
        vec![profile, common]
    }
}

/// The user's known folders that join the roots wherever they point (moved with Explorer's Location tab or by Folder
/// Redirection, often to another drive): their paths come from HKCU, so each is a root only as `files::root_of`
/// accepts it. They are resolved with KF_FLAG_DONT_VERIFY: the helper reads their path text only and never touches the
/// folders, so a sleeping disk or an unreachable share cannot hold the state frame (a folder that is not there gives no
/// rows).
const KNOWN: [windows::core::GUID; 6] =
    [FOLDERID_Desktop, FOLDERID_Documents, FOLDERID_Downloads, FOLDERID_Pictures, FOLDERID_Music, FOLDERID_Videos];

/// `roots` (`roots_of`'s two), then each of `known` that `files::root_of` accepts (no drive root), then the first 8 of
/// `extra`, the folders of `launcher.file_folders` the dock passed (a drive root allowed), checked again here: the
/// helper trusts none of them. With no fences, `roots` alone. None of them holds another (`files::add_roots`).
fn with_folders(mut roots: Vec<String>, known: &[String], extra: &[String], fences: Option<&Fences>) -> Vec<String> {
    if let Some(f) = fences {
        files::add_roots(&mut roots, known.iter().map(String::as_str), f, false);
        files::add_roots(&mut roots, extra.iter().take(files::MAX_EXTRA_ROOTS).map(String::as_str), f, true);
    }
    roots
}

/// A known folder's path for `token`'s user.
fn known_folder(id: &windows::core::GUID, token: &Handle, flags: KNOWN_FOLDER_FLAG) -> Option<String> {
    // SAFETY: a valid folder id and token; the string is freed by take_cotask.
    let p = unsafe { SHGetKnownFolderPath(id, flags, Some(token.0)) }.ok()?;
    take_cotask(p)
}

/// The roots of `with_folders`, every folder resolved with the shell's token.
fn shell_roots(token: &Handle, extra: &[String]) -> Option<Vec<String>> {
    let path = |id: &windows::core::GUID| known_folder(id, token, KF_FLAG_DEFAULT);
    let profile = path(&FOLDERID_Profile)?;
    let fences = || {
        Some(Fences {
            windows: path(&FOLDERID_Windows)?,
            program_files: vec![path(&FOLDERID_ProgramFiles)?, path(&FOLDERID_ProgramFilesX86)?],
            profiles: path(&FOLDERID_UserProfiles)?,
            profile: profile.clone(),
            accounts: crate::launcher_sys::profile_folders()?,
            same: same_text,
        })
    };
    let fences = fences();
    let known: Vec<String> = KNOWN.iter().filter_map(|id| known_folder(id, token, KF_FLAG_DONT_VERIFY)).collect();
    Some(with_folders(roots_of(profile, path(&FOLDERID_CommonPrograms)?), &known, extra, fences.as_ref()))
}

/// The checks of the state frame, in order: the SIDs, the service, the roots, the index.
fn setup(stall_ms: u32, extra: &[String]) -> (u32, Option<Search>) {
    let Some(shell) = shell_token() else { return (files::STATE_NO_ROOTS, None) };
    let own = {
        let mut t = HANDLE::default();
        // SAFETY: this process's pseudo handle; `t` is writable and owned by `Handle` below.
        unsafe { OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut t) }.ok().and_then(|()| {
            let t = Handle(t);
            token_user(t.0)
        })
    };
    match (own, token_user(shell.0)) {
        (Some(a), Some(b)) if !same_user(&a, &b) => return (files::STATE_SID_DIFFERS, None),
        (Some(_), Some(_)) => {}
        _ => return (files::STATE_NO_ROOTS, None),
    }
    if !search_running() {
        return (files::STATE_SEARCH_OFF, None);
    }
    let Some(roots) = shell_roots(&shell, extra) else { return (files::STATE_NO_ROOTS, None) };
    let refs: Vec<&str> = roots.iter().map(String::as_str).collect();
    let Ok(scope) = files::scope_clause(&refs) else { return (files::STATE_NO_ROOTS, None) };
    match Search::new(&scope, roots, stall_ms) {
        Some(s) => (files::STATE_READY, Some(s)),
        None => (files::STATE_NO_INDEX, None),
    }
}

/// The query helper, configured once (route a of S-WSI), and an open session of the index's data source.
struct Search {
    helper: ISearchQueryHelper,
    command: IDBCreateCommand,
    _source: IDBInitialize,
    roots: Vec<String>,
    stall_ms: u32,
}

impl Search {
    fn new(scope: &str, roots: Vec<String>, stall_ms: u32) -> Option<Search> {
        // SAFETY: COM calls on this MTA thread; every string handed over outlives its call; returned strings are freed
        // by take_cotask.
        unsafe {
            let mgr: ISearchManager = CoCreateInstance(&CSearchManager, None, CLSCTX_LOCAL_SERVER).ok()?;
            let helper = mgr.GetCatalog(w!("SystemIndex")).ok()?.GetQueryHelper().ok()?;
            helper.SetQueryContentProperties(w!("System.FileName")).ok()?;
            helper.SetQueryMaxResults(50).ok()?;
            // The helper's row order stands for the rank (D7): Windows' best 50 rows.
            helper.SetQuerySorting(w!("System.Search.Rank DESC")).ok()?;
            let cols = w!("System.ItemUrl,System.FileName,System.ItemFolderNameDisplay,System.ItemType");
            helper.SetQuerySelectColumns(cols).ok()?;
            let restriction = wide(&format!("AND ({scope})"));
            helper.SetQueryWhereRestrictions(PCWSTR(restriction.as_ptr())).ok()?;
            let conn = wide(&take_cotask(helper.ConnectionString().ok()?)?);
            let init: IDataInitialize = CoCreateInstance(&MSDAINITIALIZE, None, CLSCTX_INPROC_SERVER).ok()?;
            let mut source = None;
            init.GetDataSource(
                None::<&windows::core::IUnknown>,
                CLSCTX_INPROC_SERVER.0,
                PCWSTR(conn.as_ptr()),
                &IDBInitialize::IID,
                &mut source,
            )
            .ok()?;
            let source: IDBInitialize = source?.cast().ok()?;
            source.Initialize().ok()?;
            let session: IDBCreateSession = source.cast().ok()?;
            let command: IDBCreateCommand =
                session.CreateSession(None::<&windows::core::IUnknown>, &IDBCreateCommand::IID).ok()?.cast().ok()?;
            Some(Search { helper, command, _source: source, roots, stall_ms })
        }
    }

    /// One query: Windows' SQL for the typed words, executed, its rows mapped. None: a step failed.
    fn query(&self, text: &str) -> Answer {
        // SAFETY: as in `new`; the rowset's strings are read by `Rowset` before their rows are released.
        unsafe {
            let user = wide(text);
            let sql = wide(&take_cotask(self.helper.GenerateSQLFromUserQuery(PCWSTR(user.as_ptr())).ok()?)?);
            let c: ICommandText =
                self.command.CreateCommand(None::<&windows::core::IUnknown>, &ICommandText::IID).ok()?.cast().ok()?;
            c.SetCommandText(&DBGUID_DEFAULT, PCWSTR(sql.as_ptr())).ok()?;
            if self.stall_ms > 0 {
                std::thread::sleep(std::time::Duration::from_millis(u64::from(self.stall_ms)));
            }
            let mut rs = None;
            c.Execute(None::<&windows::core::IUnknown>, &IRowset::IID, None, None, Some(&mut rs)).ok()?;
            let mut rows = Rowset::new(rs?.cast().ok()?)?;
            Some(collect(&mut rows, &self.roots, &mut dangerous_type))
        }
    }
}

/// Windows' word on a type: `AssocIsDangerous` on "." and `ext` (its built-in list of types that can run code, and any
/// type whose class carries FTA_AlwaysUnsafe). `ext` is part of a name `files::check_hit` passed: no NUL in it.
fn dangerous_type(ext: &str) -> bool {
    // "." then the extension, at most 259 units (a name holds at most 260), then at least one NUL: no allocation.
    let mut assoc = [0u16; files::NAME_UNITS + 2];
    if let Some((dot, rest)) = assoc.split_first_mut() {
        *dot = u16::from(b'.');
        for (slot, u) in rest.iter_mut().zip(ext.encode_utf16().take(files::NAME_UNITS)) {
            *slot = u;
        }
    }
    // SAFETY: `assoc` ends with a NUL (at most NAME_UNITS units follow the dot) and outlives the call, which reads it.
    unsafe { AssocIsDangerous(PCWSTR(assoc.as_ptr())) }.as_bool()
}

/// One bound column: what GetData writes (provider-owned UTF-16 and its length in bytes).
#[repr(C)]
#[derive(Clone, Copy)]
struct Col {
    status: u32,
    length: usize,
    value: *const u16,
}

const NO_COL: Col = Col { status: 0, length: 0, value: std::ptr::null() };
const NCOLS: usize = 4;

/// A rowset with an accessor for the four columns, released at drop.
struct Rowset {
    rowset: IRowset,
    accessor: IAccessor,
    handle: HACCESSOR,
}

impl Rowset {
    fn new(rowset: IRowset) -> Option<Rowset> {
        let accessor: IAccessor = rowset.cast().ok()?;
        let size = std::mem::size_of::<Col>();
        let bindings: Vec<DBBINDING> = (0..NCOLS)
            .map(|i| DBBINDING {
                iOrdinal: i + 1,
                obValue: i * size + std::mem::offset_of!(Col, value),
                obLength: i * size + std::mem::offset_of!(Col, length),
                obStatus: i * size + std::mem::offset_of!(Col, status),
                dwPart: (DBPART_VALUE.0 | DBPART_LENGTH.0 | DBPART_STATUS.0) as u32,
                dwMemOwner: DBMEMOWNER_PROVIDEROWNED.0 as u32,
                cbMaxLen: std::mem::size_of::<usize>(),
                wType: (DBTYPE_WSTR.0 | DBTYPE_BYREF.0) as u16,
                ..Default::default()
            })
            .collect();
        let mut handle = HACCESSOR(0);
        // SAFETY: `bindings` holds NCOLS entries that live for the call; `handle` is writable.
        unsafe {
            accessor.CreateAccessor(
                DBACCESSOR_ROWDATA.0 as u32,
                NCOLS,
                bindings.as_ptr(),
                NCOLS * size,
                &mut handle,
                None,
            )
        }
        .ok()?;
        Some(Rowset { rowset, accessor, handle })
    }
}

impl Drop for Rowset {
    fn drop(&mut self) {
        // SAFETY: the accessor this rowset created, released once.
        let _ = unsafe { self.accessor.ReleaseAccessor(self.handle, None) };
    }
}

/// A column's text, read by its bound length and never past `max_units` (never up to a NUL).
///
/// # Safety
/// `c` was written by GetData for a row not yet released: `value` points at `length` bytes of UTF-16.
unsafe fn cell(c: &Col, max_units: usize) -> Cell {
    if c.status == DBSTATUS_S_ISNULL || c.value.is_null() {
        return Cell::Null;
    }
    if c.status != 0 || !c.length.is_multiple_of(2) || c.length / 2 > max_units {
        return Cell::Bad;
    }
    // SAFETY: the caller's contract; the length was capped above.
    let units = unsafe { std::slice::from_raw_parts(c.value, c.length / 2) };
    String::from_utf16(units).map_or(Cell::Bad, Cell::Text)
}

impl Rows for Rowset {
    fn next(&mut self) -> Option<RawRow> {
        let mut got = 0usize;
        let mut h = [0usize; 1];
        let mut handles = [h.as_mut_ptr()];
        // SAFETY: the caller owns the row-handle buffer (its pointer is not null), so the provider writes the one handle
        // into `h` and allocates nothing; `data` has room for the accessor's four columns; the row is released after
        // its columns were copied.
        unsafe {
            self.rowset.GetNextRows(0, 0, &mut got, &mut handles).ok()?;
            if got == 0 {
                return None;
            }
            let [row] = h;
            let mut data = [NO_COL; NCOLS];
            let read = self.rowset.GetData(row, self.handle, data.as_mut_ptr().cast());
            let [p, n, f, k] = data;
            let raw = match read {
                Ok(()) => RawRow {
                    url: cell(&p, URL_UNITS),
                    name: cell(&n, files::NAME_UNITS),
                    folder: cell(&f, files::FOLDER_UNITS),
                    kind: cell(&k, TYPE_UNITS),
                },
                Err(_) => RawRow { url: Cell::Bad, name: Cell::Bad, folder: Cell::Bad, kind: Cell::Bad },
            };
            let _ = self.rowset.ReleaseRows(1, &row, std::ptr::null(), std::ptr::null_mut(), std::ptr::null_mut());
            Some(raw)
        }
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::indexing_slicing)]
mod tests {
    use super::*;
    use crate::launcher::files::{Frame, FrameReader};

    fn text(s: &str) -> Cell {
        Cell::Text(s.into())
    }

    /// A row whose URL is `file:` and `path` with forward slashes, as the index writes it.
    fn raw(path: &str, name: &str, folder: Cell, kind: &str) -> RawRow {
        RawRow { url: text(&format!("file:{}", path.replace('\\', "/"))), name: text(name), folder, kind: text(kind) }
    }

    struct Fake(std::vec::IntoIter<RawRow>);

    impl Rows for Fake {
        fn next(&mut self) -> Option<RawRow> {
            self.0.next()
        }
    }

    fn roots() -> Vec<String> {
        roots_of("C:\\Users\\a".into(), "C:\\ProgramData\\Microsoft\\Windows\\Start Menu\\Programs\\".into())
    }

    #[test]
    fn row_mapping() {
        let u = |s: &str| text(s);
        let long = format!("C:\\Users\\a\\{}", "d".repeat(1_020));
        let rows = vec![
            raw("C:\\Users\\a\\Docs\\quokka.txt", "quokka.txt", u("Docs"), "Text Document"),
            raw("C:\\Users\\a\\quokka-dir", "quokka-dir", u("a"), "Directory"),
            raw("C:\\users\\A\\Q.lnk", "q.lnk", Cell::Null, "directory"),
            raw(
                "C:\\ProgramData\\Microsoft\\Windows\\Start Menu\\Programs\\Tool.lnk",
                "Tool.lnk",
                u("Programs"),
                ".lnk",
            ),
            // Skipped: a control character, a relative path, a name that is not the last part, an over-long path (as
            // read, and as a text), a URL of another scheme or none, a long-path prefix, another profile, a root's
            // lookalike, the root itself, no name, a refused folder.
            raw("C:\\Users\\a\\b\u{7}.txt", "b\u{7}.txt", u("a"), "x"),
            raw("Users\\a\\b.txt", "b.txt", u("a"), "x"),
            raw("C:\\Users\\a\\b.txt", "c.txt", u("a"), "x"),
            RawRow { url: Cell::Bad, name: text("b.txt"), folder: u("a"), kind: u("x") },
            RawRow { url: text("mapi://C:/Users/a/b.txt"), name: text("b.txt"), folder: u("a"), kind: u("x") },
            RawRow { url: text("C:/Users/a/b.txt"), name: text("b.txt"), folder: u("a"), kind: u("x") },
            raw(&long, &"d".repeat(1_020), u("a"), "x"),
            raw("\\\\?\\C:\\Users\\a\\b.txt", "b.txt", u("a"), "x"),
            raw("C:\\Users\\b\\quokka.txt", "quokka.txt", u("b"), "x"),
            raw("C:\\Users\\ab\\quokka.txt", "quokka.txt", u("ab"), "x"),
            raw("C:\\Users\\a", "a", u("Users"), "Directory"),
            RawRow { url: text("file:C:/Users/a/b.txt"), name: Cell::Null, folder: u("a"), kind: u("x") },
            raw("C:\\Users\\a\\b.txt", "b.txt", Cell::Bad, "x"),
        ];
        let (hits, skipped) = collect(&mut Fake(rows.into_iter()), &roots(), &mut |_| false);
        let got: Vec<(&str, &str, &str, bool)> = hits.iter().map(|h| (&*h.path, &*h.name, &*h.folder, h.dir)).collect();
        assert_eq!(
            got,
            [
                ("C:\\Users\\a\\Docs\\quokka.txt", "quokka.txt", "Docs", false),
                ("C:\\Users\\a\\quokka-dir", "quokka-dir", "a", true),
                ("C:\\users\\A\\Q.lnk", "q.lnk", "", true),
                ("C:\\ProgramData\\Microsoft\\Windows\\Start Menu\\Programs\\Tool.lnk", "Tool.lnk", "Programs", false),
            ]
        );
        assert_eq!(skipped, 13);
        assert_eq!(path_of_url("FILE:C:/a b/c.txt").as_deref(), Some("C:\\a b\\c.txt"));
        assert_eq!(path_of_url("file://srv/share/c.txt").as_deref(), Some("\\\\srv\\share\\c.txt"));
        assert_eq!(path_of_url("file:"), Some(String::new()));
        for not_a_file in ["", "fil", "http://x/c.txt", "files:C:/a"] {
            assert_eq!(path_of_url(not_a_file), None, "{not_a_file:?}");
        }
        // At most 50 rows are read.
        let many: Vec<RawRow> =
            (0..80).map(|i| raw(&format!("C:\\Users\\a\\f{i}"), &format!("f{i}"), u("a"), "x")).collect();
        let mut fake = Fake(many.into_iter());
        let (hits, skipped) = collect(&mut fake, &roots(), &mut |_| false);
        assert_eq!((hits.len(), skipped, fake.0.len()), (50, 0, 30));
    }

    #[test]
    fn no_root_holds_another_profile() {
        let profile = "C:\\Users\\a";
        assert_eq!(roots().len(), 2, "the profile and the common Programs folder");
        // A common Programs folder that holds the profile is refused (any case, a trailing separator or none).
        for common in ["C:\\Users", "c:\\users\\", "C:\\", "C:"] {
            assert_eq!(roots_of(profile.into(), common.into()), [profile], "{common:?}");
        }
        let rows = || {
            vec![
                raw("C:\\Users\\b\\quokka.txt", "quokka.txt", text("b"), "x"),
                raw("C:\\Users\\a\\quokka.txt", "quokka.txt", text("a"), "x"),
            ]
        };
        let fence = roots_of(profile.into(), "C:\\Users".into());
        let (hits, skipped) = collect(&mut Fake(rows().into_iter()), &fence, &mut |_| false);
        assert_eq!((hits.len(), &*hits[0].path, skipped), (1, "C:\\Users\\a\\quokka.txt", 1), "another account's row");
        // The user's own Start Menu Programs folder lies under the profile: its rows pass as the profile's.
        let start = "C:\\Users\\a\\AppData\\Roaming\\Microsoft\\Windows\\Start Menu\\Programs\\Tool.lnk";
        let one = vec![raw(start, "Tool.lnk", text("Programs"), "x")];
        let (hits, skipped) = collect(&mut Fake(one.into_iter()), &roots(), &mut |_| false);
        assert_eq!((hits.len(), skipped), (1, 0));
    }

    fn fences() -> Fences {
        Fences {
            windows: "C:\\Windows".into(),
            program_files: vec!["C:\\Program Files".into(), "C:\\Program Files (x86)".into()],
            profiles: "C:\\Users".into(),
            profile: "C:\\Users\\a".into(),
            accounts: vec!["C:\\Users\\a".into(), "C:\\Users\\b".into()],
            same: same_text,
        }
    }

    fn strings(v: &[&str]) -> Vec<String> {
        v.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn known_folders_join_the_roots_wherever_they_point() {
        let f = fences();
        let common = "C:\\ProgramData\\Microsoft\\Windows\\Start Menu\\Programs\\";
        // Documents, Downloads, Pictures, Music and Videos moved to other drives; the Desktop left in the profile.
        let known = strings(&[
            "C:\\Users\\a\\Desktop",
            "D:\\Docs",
            "D:\\Downloads",
            "E:\\Media\\Pictures",
            "E:\\Media\\Music",
            "e:\\media\\music\\",
        ]);
        let want = ["C:\\Users\\a", common, "D:\\Docs", "D:\\Downloads", "E:\\Media\\Pictures", "E:\\Media\\Music"];
        assert_eq!(with_folders(roots(), &known, &[], Some(&f)), want);
        // A known folder pointed at another account's profile (in any spelling), the profiles folder, a drive root, a
        // share, the Windows tree or an odd path is refused.
        let bad = strings(&[
            "C:\\Users\\b\\Documents",
            "C:\\Users\\a\\..\\b\\Documents",
            "C:\\Users\\B~1\\Documents",
            "C:\\Users",
            "D:\\",
            "C:\\",
            "C:\\Windows\\Temp",
            "\\\\nas\\share\\Docs",
            "\\\\localhost\\Users\\b\\Documents",
            "\\\\?\\C:\\Users\\b\\Documents",
            "\\\\nas\\c$\\Users",
            "Docs",
        ]);
        assert_eq!(with_folders(roots(), &bad, &[], Some(&f)), roots());
        // The user's own folders may be a drive root; never a share, C:\ or another profile; at most 8 are read.
        let extra = strings(&[
            "D:\\",
            "\\\\nas\\share",
            "C:\\",
            "C:\\Users\\b",
            "F:\\Archive",
            "G:\\Photos",
            "H:\\Music",
            "I:\\Old",
            "J:\\Ninth",
        ]);
        let want = [
            "C:\\Users\\a",
            common,
            "E:\\Media\\Pictures",
            "E:\\Media\\Music",
            "D:\\",
            "F:\\Archive",
            "G:\\Photos",
            "H:\\Music",
            "I:\\Old",
        ];
        assert_eq!(with_folders(roots(), &known, &extra, Some(&f)), want);
        // No fences: the two roots alone.
        assert_eq!(with_folders(roots(), &known, &extra, None), roots());
    }

    #[test]
    fn the_scope_binds_the_accepted_roots() {
        let known = strings(&["D:\\Docs", "C:\\Users\\b\\Documents"]);
        let extra = strings(&["\\\\nas\\share", "E:\\O'Brien", "C:\\", "f:/Data/./x/../Média."]);
        let roots = with_folders(roots(), &known, &extra, Some(&fences()));
        let refs: Vec<&str> = roots.iter().map(String::as_str).collect();
        // Each root in the text Windows reads it as: the scope and the row fence use that text.
        assert_eq!(
            files::scope_clause(&refs).unwrap(),
            "SCOPE='file:C:\\Users\\a' OR SCOPE='file:C:\\ProgramData\\Microsoft\\Windows\\Start Menu\\Programs\\' OR \
             SCOPE='file:D:\\Docs' OR SCOPE='file:E:\\O''Brien' OR SCOPE='file:f:\\Data\\Média'"
        );
        // The rows: only those under an accepted root pass the fence, in any case of its letters.
        let rows = [
            "D:\\Docs\\quokka.txt",
            "E:\\O'Brien\\quokka.txt",
            "F:\\DATA\\MÉDIA\\quokka.txt",
            "\\\\nas\\share\\quokka.txt",
            "C:\\Users\\b\\Documents\\quokka.txt",
            "D:\\Docs2\\quokka.txt",
            "D:\\quokka.txt",
            "C:\\quokka.txt",
            "F:\\Data\\x\\quokka.txt",
        ];
        let raws: Vec<RawRow> = rows.iter().map(|p| raw(p, "quokka.txt", text("x"), "x")).collect();
        let (hits, skipped) = collect(&mut Fake(raws.into_iter()), &roots, &mut |_| false);
        let paths: Vec<&str> = hits.iter().map(|h| &*h.path).collect();
        assert_eq!((paths, skipped), (rows[..3].to_vec(), 6));
    }

    #[test]
    fn windows_is_asked_once_per_extension() {
        let u = |s: &str| text(s);
        let rows = vec![
            raw("C:\\Users\\a\\quokka.EXE", "quokka.EXE", u("a"), "x"),
            raw("C:\\Users\\a\\b\\quokka.exe", "quokka.exe", u("b"), "x"),
            raw("C:\\Users\\a\\plan.qqq", "plan.qqq", u("a"), "x"),
            raw("C:\\Users\\a\\plan.txt", "plan.txt", u("a"), "x"),
            raw("C:\\Users\\a\\plan.Txt. .", "plan.Txt. .", u("a"), "x"),
            raw("C:\\Users\\a\\README", "README", u("a"), "x"),
            raw("C:\\Users\\a\\tools.qqq", "tools.qqq", u("a"), "Directory"),
            // Skipped before Windows is asked: outside the roots.
            raw("C:\\Users\\b\\other.zzz", "other.zzz", u("b"), "x"),
        ];
        let mut asked = Vec::new();
        let (hits, skipped) = collect(&mut Fake(rows.into_iter()), &roots(), &mut |ext| {
            asked.push(ext.to_string());
            ext.eq_ignore_ascii_case("exe") || ext == "qqq"
        });
        assert_eq!(asked, ["EXE", "qqq", "txt"], "once per extension of the query, in any case; none without one");
        let got: Vec<(&str, bool)> = hits.iter().map(|h| (&*h.name, h.dangerous)).collect();
        let want = [
            ("quokka.EXE", true),
            ("quokka.exe", true),
            ("plan.qqq", true),
            ("plan.txt", false),
            ("plan.Txt. .", false),
            ("README", false),
            ("tools.qqq", true),
        ];
        assert_eq!((got.as_slice(), skipped), (want.as_slice(), 1));
        // The next query asks again: no answer is kept between queries.
        let again = vec![raw("C:\\Users\\a\\quokka.exe", "quokka.exe", u("a"), "x")];
        let mut n = 0;
        let (hits, _) = collect(&mut Fake(again.into_iter()), &roots(), &mut |_| {
            n += 1;
            false
        });
        assert_eq!((n, hits[0].dangerous), (1, false));
    }

    #[test]
    fn windows_names_its_own_dangerous_types() {
        // A read of the type associations only: an exe is dangerous in Windows' built-in list, a text file is not.
        assert!(dangerous_type("exe") && dangerous_type("EXE") && dangerous_type("vbs"));
        assert!(!dangerous_type("txt"));
    }

    #[test]
    fn cells_are_read_by_length() {
        let units: Vec<u16> = "quokka.txt\u{0}tail".encode_utf16().collect();
        let col = |status, length| Col { status, length, value: units.as_ptr() };
        // SAFETY (each call): `units` outlives the calls and holds at least `length` bytes.
        unsafe {
            assert_eq!(cell(&col(0, 20), 260), text("quokka.txt"));
            assert_eq!(cell(&col(0, 30), 260), text("quokka.txt\u{0}tail"), "the length, not a NUL, ends it");
            assert_eq!(cell(&col(0, 6), 260), text("quo"));
            assert_eq!(cell(&col(0, 20), 9), Cell::Bad, "over its cap");
            assert_eq!(cell(&col(0, 5), 260), Cell::Bad, "an odd length");
            assert_eq!(cell(&col(2, 20), 260), Cell::Bad, "truncated");
            assert_eq!(cell(&col(DBSTATUS_S_ISNULL, 20), 260), Cell::Null);
            assert_eq!(cell(&NO_COL, 260), Cell::Null);
        }
        let bad = [0xD800u16, 0x41];
        // SAFETY: `bad` holds 4 bytes.
        assert_eq!(unsafe { cell(&Col { status: 0, length: 4, value: bad.as_ptr() }, 260) }, Cell::Bad);
    }

    fn frames(out: &[u8]) -> Vec<Frame> {
        let mut r = FrameReader::default();
        let mut cur = std::io::Cursor::new(out);
        let mut v = Vec::new();
        while let Some(f) = r.next(&mut cur).unwrap() {
            v.push(f);
        }
        v
    }

    #[test]
    fn request_loop_over_fake_io() {
        // Windows' word on the type (bit 1) reaches the dock as the helper's query gave it.
        let hit = FileHit {
            path: "C:\\u\\quokka.txt".into(),
            name: "quokka.txt".into(),
            folder: "u".into(),
            dir: false,
            dangerous: true,
        };
        let mut asked = Vec::new();
        let mut out = Vec::new();
        let input = b"1 quokka\n2   ext:pdf  plan \n3 fail\n4  \n";
        let code = serve(0, &mut &input[..], &mut out, &mut |t| {
            asked.push(t.to_string());
            (t != "fail").then(|| (vec![hit.clone()], 2))
        });
        assert_eq!(code, 0, "stdin ended");
        assert_eq!(
            asked,
            ["quokka", "ext:pdf plan", "fail"],
            "the words, one space between; a blank line asks nothing"
        );
        let rows = |gen, status, skipped, hits: &[FileHit]| Frame::Rows { gen, status, skipped, hits: hits.to_vec() };
        assert_eq!(
            frames(&out),
            [
                Frame::State(0),
                rows(1, 0, 2, std::slice::from_ref(&hit)),
                rows(2, 0, 2, std::slice::from_ref(&hit)),
                rows(3, 1, 0, &[]),
                rows(4, 0, 0, &[])
            ]
        );
        // A bad line ends it with 2, after the lines before it were answered.
        for bad in [&b"1 a\n2\x07 b\n"[..], b"1 a\nx b\n", b"1 a\n1 b", &[b'1'; 1_200]] {
            let mut out = Vec::new();
            let code = serve(0, &mut &bad[..], &mut out, &mut |_| Some((Vec::new(), 0)));
            assert_eq!(code, 2, "{bad:?}");
            assert!(frames(&out).len() <= 2);
        }
        // A state other than 0: the header and that frame only, and stdin is not read.
        let mut out = Vec::new();
        assert_eq!(serve(1, &mut &b"1 a\n"[..], &mut out, &mut |_| panic!("no query")), 0);
        assert_eq!(frames(&out), [Frame::State(1)]);
        // A write that fails ends it at once.
        struct Broken;
        impl Write for Broken {
            fn write(&mut self, _: &[u8]) -> std::io::Result<usize> {
                Err(std::io::ErrorKind::BrokenPipe.into())
            }
            fn flush(&mut self) -> std::io::Result<()> {
                Ok(())
            }
        }
        assert_eq!(serve(0, &mut &b"1 a\n"[..], &mut Broken, &mut |_| panic!("no query")), 3);
    }
}
