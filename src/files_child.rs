//! `dock.exe --launcher-files` (plan L2-F2): the launcher's file search, in a short-lived child of the dock, so the
//! search client (OLE DB and the Windows Search DLLs, which stay loaded once used) never enters the dock (D1). The dock
//! starts it when the first files request of an open is due and ends it at the close (launcher_files.rs). It checks once
//! that it can answer (its user against the shell's, the WSearch service, the shell user's three roots, the index) and
//! says so in its state frame; then it answers each request line on stdin with one rows frame on stdout, until stdin
//! ends (Appendix A of the plan). It runs with the dock's token and reads index columns only: it never opens a listed
//! file, and a row outside its three roots is skipped here, never sent. It has no logger and writes nothing but its
//! frames (lint `search_stays_in_the_files_helper`): the words it gets are typed text, given to Windows Search only.
#![deny(clippy::unwrap_used, clippy::expect_used, clippy::indexing_slicing)]

use crate::launcher::files::{self, FileHit};
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
    FOLDERID_CommonPrograms, FOLDERID_Profile, FOLDERID_Programs, SHGetKnownFolderPath, KF_FLAG_DEFAULT,
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

/// `path` lies under `root`: the root (ASCII case-insensitive, a trailing separator ignored), a separator, then more.
fn under(path: &str, root: &str) -> bool {
    let root = root.strip_suffix('\\').unwrap_or(root).as_bytes();
    let p = path.as_bytes();
    !root.is_empty()
        && p.len() > root.len() + 1
        && p.get(..root.len()).is_some_and(|head| head.eq_ignore_ascii_case(root))
        && p.get(root.len()) == Some(&b'\\')
}

/// The file-system path of an index URL: `file:` (any case), then the path with `/` for each separator
/// (`file:C:/Users/a/b.txt`, `file://server/share/b.txt`); a file name cannot hold a `/`. None for any other scheme.
fn path_of_url(url: &str) -> Option<String> {
    let rest = url.get(..5).filter(|s| s.eq_ignore_ascii_case("file:")).and(url.get(5..))?;
    Some(rest.replace('/', "\\"))
}

/// A row the dock may be sent: its URL names a file-system path, every check of the dock's reader
/// (`files::check_hit`) passes, and the path lies under one of the helper's three roots (the SQL Windows builds around
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
    if !roots.iter().any(|r| under(&path, r)) {
        return None;
    }
    Some(FileHit { path: path.into(), name: name.into(), folder: folder.into(), dir })
}

/// At most 50 rows of a query, mapped: the hits in Windows' order and the number skipped.
fn collect(rows: &mut dyn Rows, roots: &[String]) -> (Vec<FileHit>, u32) {
    let mut hits = Vec::new();
    let mut skipped = 0u32;
    for _ in 0..files::MAX_ROWS {
        let Some(raw) = rows.next() else { break };
        match row_of(raw, roots) {
            Some(h) => hits.push(h),
            None => skipped += 1,
        }
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
/// mutex and the watchdog). `stall_ms`, from `--stall` in owner-probe builds only, sleeps before each query.
pub(crate) fn run(stall_ms: u32) -> i32 {
    // SAFETY: the first COM call of the helper's only thread; paired with CoUninitialize below.
    let com = unsafe { CoInitializeEx(None, COINIT_MULTITHREADED) }.is_ok();
    let (state, search) = if com { setup(stall_ms) } else { (files::STATE_NO_INDEX, None) };
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

/// The shell user's profile and both Start Menu Programs folders, resolved with the shell's token (SPEC 5.4.6).
fn shell_roots(token: &Handle) -> Option<Vec<String>> {
    let mut out = Vec::with_capacity(3);
    for id in [&FOLDERID_Profile, &FOLDERID_Programs, &FOLDERID_CommonPrograms] {
        // SAFETY: a valid folder id and token; the string is freed by take_cotask.
        let p = unsafe { SHGetKnownFolderPath(id, KF_FLAG_DEFAULT, Some(token.0)) }.ok()?;
        out.push(take_cotask(p)?);
    }
    Some(out)
}

/// The checks of the state frame, in order: the SIDs, the service, the roots, the index.
fn setup(stall_ms: u32) -> (u32, Option<Search>) {
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
    let Some(roots) = shell_roots(&shell) else { return (files::STATE_NO_ROOTS, None) };
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
            Some(collect(&mut rows, &self.roots))
        }
    }
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
        [
            "C:\\Users\\a",
            "C:\\Users\\a\\AppData\\Roaming\\Microsoft\\Windows\\Start Menu\\Programs",
            "C:\\ProgramData\\Microsoft\\Windows\\Start Menu\\Programs\\",
        ]
        .map(String::from)
        .to_vec()
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
        let (hits, skipped) = collect(&mut Fake(rows.into_iter()), &roots());
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
        let (hits, skipped) = collect(&mut fake, &roots());
        assert_eq!((hits.len(), skipped, fake.0.len()), (50, 0, 30));
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
        let hit =
            FileHit { path: "C:\\u\\quokka.txt".into(), name: "quokka.txt".into(), folder: "u".into(), dir: false };
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
