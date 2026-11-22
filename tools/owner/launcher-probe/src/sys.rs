//! Windows side of launcher-probe. Read-only: process handles are PROCESS_QUERY_LIMITED_INFORMATION and tokens
//! are TOKEN_QUERY (plus DUPLICATE and IMPERSONATE for the shell user's known-folder lookup, exactly as the dock's
//! start_import.rs does). Nothing is written anywhere but stdout.
#![deny(clippy::unwrap_used, clippy::expect_used, clippy::indexing_slicing)]

use crate::{fallback_sql, file_name_of, is_named_link, is_pt_image, jstr, scan_cmdpal, scope_clause};
use std::time::{Duration, Instant};
use windows::core::{Interface, PCWSTR, PWSTR};
use windows::Win32::Foundation::{CloseHandle, LocalFree, HANDLE, HLOCAL, HWND};
use windows::Win32::Security::Authorization::ConvertSidToStringSidW;
use windows::Win32::Security::{
    GetTokenInformation, TokenUser, TOKEN_DUPLICATE, TOKEN_IMPERSONATE, TOKEN_QUERY, TOKEN_USER,
};
use windows::Win32::System::Com::{
    CoCreateInstance, CoInitializeEx, CoTaskMemFree, CoUninitialize, CLSCTX_INPROC_SERVER, CLSCTX_LOCAL_SERVER,
    COINIT_APARTMENTTHREADED, COINIT_MULTITHREADED,
};
use windows::Win32::System::ProcessStatus::{
    EnumProcesses, GetProcessMemoryInfo, PROCESS_MEMORY_COUNTERS, PROCESS_MEMORY_COUNTERS_EX,
};
use windows::Win32::System::Search::{
    CSearchManager, IAccessor, ICommandText, IDBCreateCommand, IDBCreateSession, IDBInitialize, IDataInitialize,
    IRowset, ISearchManager, DBACCESSOR_ROWDATA, DBBINDING, DBPART_LENGTH, DBPART_STATUS, DBPART_VALUE, DBTYPE_BYREF,
    DBTYPE_WSTR, HACCESSOR, MSDAINITIALIZE,
};
use windows::Win32::System::SystemInformation::GetTickCount;
use windows::Win32::System::SystemServices::SFGAO_FOLDER;
use windows::Win32::System::Threading::{
    GetCurrentProcess, OpenProcess, OpenProcessToken, QueryFullProcessImageNameW, PROCESS_NAME_WIN32,
    PROCESS_QUERY_LIMITED_INFORMATION,
};
use windows::Win32::UI::Accessibility::{SetWinEventHook, UnhookWinEvent, HWINEVENTHOOK};
use windows::Win32::UI::Shell::{
    BHID_EnumItems, FOLDERID_CommonPrograms, FOLDERID_Profile, FOLDERID_Programs, IEnumShellItems, IShellItem,
    SHCreateItemFromParsingName, SHGetKnownFolderPath, KF_FLAG_DEFAULT, SIGDN_DESKTOPABSOLUTEPARSING,
    SIGDN_NORMALDISPLAY,
};
use windows::Win32::UI::WindowsAndMessaging::{
    DispatchMessageW, GetShellWindow, GetWindowThreadProcessId, MsgWaitForMultipleObjects, PeekMessageW,
    TranslateMessage, EVENT_SYSTEM_FOREGROUND, MSG, PM_REMOVE, QS_ALLINPUT, WINEVENT_OUTOFCONTEXT,
};

/// Closes the handle on drop.
struct Handle(HANDLE);
impl Drop for Handle {
    fn drop(&mut self) {
        // SAFETY: the handle was opened by this tool and is closed exactly once.
        let _ = unsafe { CloseHandle(self.0) };
    }
}

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

fn emit(line: String) {
    println!("{line}");
}

fn err_line(kind: &str, what: &str, e: impl std::fmt::Display) {
    emit(format!("{{\"kind\":{},\"error\":{}}}", jstr(kind), jstr(&format!("{what}: {e}"))));
}

/// The UTF-16 text of a NUL-terminated PWSTR owned by COM, freed here.
fn take_cotask(p: PWSTR) -> String {
    // SAFETY: p was returned by a COM call that allocated it with CoTaskMemAlloc; read once, freed once.
    let s = unsafe { p.to_string() }.unwrap_or_default();
    // SAFETY: same pointer as above, not used afterwards.
    unsafe { CoTaskMemFree(Some(p.0 as *const std::ffi::c_void)) };
    s
}

// ---- sid ----------------------------------------------------------------------------------------------------

fn token_sid(token: HANDLE) -> Result<String, String> {
    let mut len = 0u32;
    // SAFETY: size query only, no buffer.
    let _ = unsafe { GetTokenInformation(token, TokenUser, None, 0, &mut len) };
    if len == 0 {
        return Err("GetTokenInformation size".into());
    }
    // u64 elements keep the buffer 8-byte aligned for TOKEN_USER.
    let mut buf = vec![0u64; (len as usize).div_ceil(8)];
    // SAFETY: buf holds at least len bytes and is writable.
    unsafe { GetTokenInformation(token, TokenUser, Some(buf.as_mut_ptr().cast()), len, &mut len) }
        .map_err(|e| format!("GetTokenInformation: {e}"))?;
    // SAFETY: the buffer now holds a TOKEN_USER written by the call above, suitably aligned.
    let tu = unsafe { &*(buf.as_ptr() as *const TOKEN_USER) };
    let mut s = PWSTR::null();
    // SAFETY: the SID is valid for the life of buf; s receives a LocalAlloc'd string freed below.
    unsafe { ConvertSidToStringSidW(tu.User.Sid, &mut s) }.map_err(|e| format!("ConvertSidToStringSidW: {e}"))?;
    // SAFETY: valid NUL-terminated string from the call above.
    let out = unsafe { s.to_string() }.map_err(|e| e.to_string());
    // SAFETY: allocated with LocalAlloc by ConvertSidToStringSidW, not used afterwards.
    unsafe { LocalFree(Some(HLOCAL(s.0.cast()))) };
    out
}

fn shell_pid() -> Result<u32, String> {
    // SAFETY: plain query.
    let w: HWND = unsafe { GetShellWindow() };
    if w.0.is_null() {
        return Err("no shell window".into());
    }
    let mut pid = 0u32;
    // SAFETY: pid is writable.
    unsafe { GetWindowThreadProcessId(w, Some(&mut pid)) };
    if pid == 0 {
        return Err("no pid for the shell window".into());
    }
    Ok(pid)
}

/// Opens the token of process pid; returns the process handle too (kept alive by the caller).
fn open_token(pid: u32, access: windows::Win32::Security::TOKEN_ACCESS_MASK) -> Result<(Handle, Handle), String> {
    // SAFETY: query-only open.
    let p = unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid) }
        .map_err(|e| format!("OpenProcess({pid}): {e}"))?;
    let p = Handle(p);
    let mut t = HANDLE::default();
    // SAFETY: p is open; t is writable.
    unsafe { OpenProcessToken(p.0, access, &mut t) }.map_err(|e| format!("OpenProcessToken({pid}): {e}"))?;
    Ok((p, Handle(t)))
}

pub fn cmd_sid() {
    let own = {
        let mut t = HANDLE::default();
        // SAFETY: pseudo handle of this process; t is writable.
        match unsafe { OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut t) } {
            Ok(()) => {
                let t = Handle(t);
                token_sid(t.0)
            }
            Err(e) => Err(format!("OpenProcessToken(self): {e}")),
        }
    };
    let pid = shell_pid();
    let shell = match &pid {
        Ok(p) => open_token(*p, TOKEN_QUERY).and_then(|(_p, t)| token_sid(t.0)),
        Err(e) => Err(e.clone()),
    };
    let field = |r: &Result<String, String>| match r {
        Ok(s) => jstr(s),
        Err(_) => "null".to_string(),
    };
    let differ = match (&own, &shell) {
        (Ok(a), Ok(b)) => (a != b).to_string(),
        _ => "null".to_string(),
    };
    emit(format!(
        "{{\"kind\":\"sid\",\"own\":{},\"shell\":{},\"shell_pid\":{},\"differ\":{}}}",
        field(&own),
        field(&shell),
        pid.as_ref().map(|p| p.to_string()).unwrap_or_else(|_| "null".into()),
        differ
    ));
    if let Err(e) = own {
        err_line("sid", "own token", e);
    }
    if let Err(e) = shell {
        err_line("sid", "shell token", e);
    }
}

// ---- wsi ----------------------------------------------------------------------------------------------------

fn private_bytes() -> Option<(usize, usize)> {
    let mut c = PROCESS_MEMORY_COUNTERS_EX {
        cb: std::mem::size_of::<PROCESS_MEMORY_COUNTERS_EX>() as u32,
        ..Default::default()
    };
    // SAFETY: c is writable and cb matches its size; EX starts with the plain counters.
    unsafe {
        GetProcessMemoryInfo(
            GetCurrentProcess(),
            (&mut c as *mut PROCESS_MEMORY_COUNTERS_EX).cast::<PROCESS_MEMORY_COUNTERS>(),
            c.cb,
        )
    }
    .ok()?;
    Some((c.PrivateUsage, c.WorkingSetSize))
}

fn mem_line(phase: &str) {
    match private_bytes() {
        Some((p, w)) => emit(format!(
            "{{\"kind\":\"wsi_mem\",\"phase\":{},\"private_bytes\":{p},\"working_set\":{w}}}",
            jstr(phase)
        )),
        None => err_line("wsi_mem", phase, "GetProcessMemoryInfo failed"),
    }
}

/// Known folders of the user owning the shell window, from that process's token.
fn shell_roots() -> Result<Vec<String>, String> {
    let pid = shell_pid()?;
    let (_p, tok) = open_token(pid, TOKEN_QUERY | TOKEN_DUPLICATE | TOKEN_IMPERSONATE)?;
    let mut out = Vec::new();
    for id in [&FOLDERID_Profile, &FOLDERID_Programs, &FOLDERID_CommonPrograms] {
        // SAFETY: valid folder id and token; the buffer is freed in take_cotask.
        let p = unsafe { SHGetKnownFolderPath(id, KF_FLAG_DEFAULT, Some(tok.0)) }
            .map_err(|e| format!("SHGetKnownFolderPath: {e}"))?;
        out.push(take_cotask(p));
    }
    Ok(out)
}

#[repr(C)]
#[derive(Clone, Copy)]
struct Col {
    status: u32,
    length: usize,
    value: *const u16,
}

const NCOLS: usize = 4;

/// Route a: the query helper builds the SQL, bound to the shell user's roots. Returns (connection, sql).
fn route_a(words: &[&str], scope: &str) -> Result<(String, String), String> {
    // SAFETY: COM calls on the MTA thread; every returned PWSTR is freed in take_cotask.
    unsafe {
        let mgr: ISearchManager = CoCreateInstance(&CSearchManager, None, CLSCTX_LOCAL_SERVER)
            .map_err(|e| format!("CoCreateInstance(CSearchManager): {e}"))?;
        let cat = mgr.GetCatalog(windows::core::w!("SystemIndex")).map_err(|e| format!("GetCatalog: {e}"))?;
        let h = cat.GetQueryHelper().map_err(|e| format!("GetQueryHelper: {e}"))?;
        h.SetQueryContentProperties(windows::core::w!("System.FileName")).map_err(|e| format!("content props: {e}"))?;
        h.SetQueryMaxResults(50).map_err(|e| format!("max results: {e}"))?;
        let cols = wide("System.ItemPathDisplay,System.FileName,System.ItemFolderNameDisplay,System.ItemType");
        h.SetQuerySelectColumns(PCWSTR(cols.as_ptr())).map_err(|e| format!("select columns: {e}"))?;
        let wh = wide(&format!("AND ({scope})"));
        h.SetQueryWhereRestrictions(PCWSTR(wh.as_ptr())).map_err(|e| format!("where restrictions: {e}"))?;
        let user = wide(&words.join(" "));
        let sql = take_cotask(
            h.GenerateSQLFromUserQuery(PCWSTR(user.as_ptr())).map_err(|e| format!("GenerateSQLFromUserQuery: {e}"))?,
        );
        let conn = take_cotask(h.ConnectionString().map_err(|e| format!("ConnectionString: {e}"))?);
        Ok((conn, sql))
    }
}

const CONN_ROUTE_B: &str = "Provider=Search.CollatorDSO.1;Extended Properties='Application=Windows'";
/// DBGUID_DEFAULT {c8b521fb-5cf3-11ce-ade5-00aa0044773d}.
const DBGUID_DEFAULT: windows::core::GUID = windows::core::GUID::from_u128(0xc8b521fb_5cf3_11ce_ade5_00aa0044773d);

/// Runs sql on the OLE DB provider named by conn; prints one line per row and returns the row count.
fn run_oledb(route: &str, conn: &str, sql: &str) -> Result<usize, String> {
    // SAFETY: OLE DB through the windows crate; every pointer handed over outlives its call, the provider-owned
    // strings are read before ReleaseRows and copied.
    unsafe {
        let di: IDataInitialize = CoCreateInstance(&MSDAINITIALIZE, None, CLSCTX_INPROC_SERVER)
            .map_err(|e| format!("CoCreateInstance(MSDAINITIALIZE): {e}"))?;
        let wconn = wide(conn);
        let mut unk = None;
        di.GetDataSource(
            None::<&windows::core::IUnknown>,
            CLSCTX_INPROC_SERVER.0,
            PCWSTR(wconn.as_ptr()),
            &IDBInitialize::IID,
            &mut unk,
        )
        .map_err(|e| format!("GetDataSource: {e}"))?;
        let init: IDBInitialize =
            unk.ok_or("no data source")?.cast().map_err(|e| format!("cast IDBInitialize: {e}"))?;
        init.Initialize().map_err(|e| format!("IDBInitialize::Initialize: {e}"))?;
        let sess: IDBCreateSession = init.cast().map_err(|e| format!("cast IDBCreateSession: {e}"))?;
        let s = sess
            .CreateSession(None::<&windows::core::IUnknown>, &IDBCreateCommand::IID)
            .map_err(|e| format!("CreateSession: {e}"))?;
        let cc: IDBCreateCommand = s.cast().map_err(|e| format!("cast IDBCreateCommand: {e}"))?;
        let c = cc
            .CreateCommand(None::<&windows::core::IUnknown>, &ICommandText::IID)
            .map_err(|e| format!("CreateCommand: {e}"))?;
        let ct: ICommandText = c.cast().map_err(|e| format!("cast ICommandText: {e}"))?;
        let wsql = wide(sql);
        ct.SetCommandText(&DBGUID_DEFAULT, PCWSTR(wsql.as_ptr())).map_err(|e| format!("SetCommandText: {e}"))?;
        let t0 = Instant::now();
        let mut rs = None;
        ct.Execute(None::<&windows::core::IUnknown>, &IRowset::IID, None, None, Some(&mut rs))
            .map_err(|e| format!("Execute: {e}"))?;
        let rowset: IRowset = rs.ok_or("no rowset")?.cast().map_err(|e| format!("cast IRowset: {e}"))?;
        emit(format!(
            "{{\"kind\":\"wsi_exec\",\"route\":{},\"execute_ms\":{}}}",
            jstr(route),
            t0.elapsed().as_millis()
        ));
        mem_line("during_execute");

        let acc: IAccessor = rowset.cast().map_err(|e| format!("cast IAccessor: {e}"))?;
        let mut bindings: Vec<DBBINDING> = Vec::with_capacity(NCOLS);
        for i in 0..NCOLS {
            let base = i * std::mem::size_of::<Col>();
            bindings.push(DBBINDING {
                iOrdinal: i + 1,
                obValue: base + std::mem::offset_of!(Col, value),
                obLength: base + std::mem::offset_of!(Col, length),
                obStatus: base + std::mem::offset_of!(Col, status),
                dwPart: (DBPART_VALUE.0 | DBPART_LENGTH.0 | DBPART_STATUS.0) as u32,
                dwMemOwner: 1, // DBMEMOWNER_PROVIDEROWNED
                eParamIO: 0,
                cbMaxLen: std::mem::size_of::<usize>(),
                wType: (DBTYPE_WSTR.0 | DBTYPE_BYREF.0) as u16,
                ..Default::default()
            });
        }
        let mut hacc = HACCESSOR(0);
        acc.CreateAccessor(
            DBACCESSOR_ROWDATA.0 as u32,
            NCOLS,
            bindings.as_ptr(),
            NCOLS * std::mem::size_of::<Col>(),
            &mut hacc,
            None,
        )
        .map_err(|e| format!("CreateAccessor: {e}"))?;

        let mut n = 0usize;
        let mut first20_ms = None;
        while n < 50 {
            let mut got = 0usize;
            let mut rows = [std::ptr::null_mut::<usize>(); 1];
            rowset.GetNextRows(0, 0, &mut got, &mut rows).map_err(|e| format!("GetNextRows: {e}"))?;
            if got == 0 {
                break;
            }
            let hrow = rows[0] as usize;
            let mut data = [Col { status: 0, length: 0, value: std::ptr::null() }; NCOLS];
            let r = rowset.GetData(hrow, hacc, data.as_mut_ptr().cast());
            let cells: Vec<String> = data
                .iter()
                .map(|c| {
                    if c.status == 0 && !c.value.is_null() {
                        PCWSTR(c.value).to_string().unwrap_or_default()
                    } else {
                        String::new()
                    }
                })
                .collect();
            let _ = rowset.ReleaseRows(1, &hrow, std::ptr::null(), std::ptr::null_mut(), std::ptr::null_mut());
            r.map_err(|e| format!("GetData: {e}"))?;
            n += 1;
            let ms = t0.elapsed().as_millis();
            if n == 20 {
                first20_ms = Some(ms);
            }
            let cell = |i: usize| jstr(cells.get(i).map(|s| s.as_str()).unwrap_or(""));
            emit(format!(
                "{{\"kind\":\"wsi_row\",\"i\":{n},\"ms\":{ms},\"path\":{},\"name\":{},\"folder\":{},\"type\":{}}}",
                cell(0),
                cell(1),
                cell(2),
                cell(3)
            ));
        }
        let _ = acc.ReleaseAccessor(hacc, None);
        emit(format!(
            "{{\"kind\":\"wsi_done\",\"route\":{},\"rows\":{n},\"first20_ms\":{},\"total_ms\":{}}}",
            jstr(route),
            first20_ms.map(|m| m.to_string()).unwrap_or_else(|| "null".into()),
            t0.elapsed().as_millis()
        ));
        mem_line("after_rows");
        Ok(n)
    }
}

fn wsi_thread(route: String, words: Vec<String>) {
    // SAFETY: balanced with CoUninitialize below, after every COM object of this thread is dropped.
    if let Err(e) = unsafe { CoInitializeEx(None, COINIT_MULTITHREADED) }.ok() {
        err_line("wsi", "CoInitializeEx(MTA)", e);
        return;
    }
    let w: Vec<&str> = words.iter().map(|s| s.as_str()).collect();
    let roots = match shell_roots() {
        Ok(r) => r,
        Err(e) => {
            err_line("wsi", "shell roots", e);
            // SAFETY: balanced with the successful CoInitializeEx above.
            unsafe { CoUninitialize() };
            return;
        }
    };
    let root_refs: Vec<&str> = roots.iter().map(|s| s.as_str()).collect();
    let scope = match scope_clause(&root_refs) {
        Ok(s) => s,
        Err(e) => {
            err_line("wsi", "scope clause", e);
            // SAFETY: balanced with the successful CoInitializeEx above.
            unsafe { CoUninitialize() };
            return;
        }
    };
    emit(format!("{{\"kind\":\"wsi_scope\",\"scope\":{}}}", jstr(&scope)));
    let try_a = route != "b";
    let mut done = false;
    if try_a {
        match route_a(&w, &scope) {
            Ok((conn, sql)) => {
                emit(format!(
                    "{{\"kind\":\"wsi_sql\",\"route\":\"a\",\"conn\":{},\"sql\":{}}}",
                    jstr(&conn),
                    jstr(&sql)
                ));
                match run_oledb("a", &conn, &sql) {
                    Ok(_) => done = true,
                    Err(e) => err_line("wsi", "route a execute", e),
                }
            }
            Err(e) => err_line("wsi", "route a bind", e),
        }
    }
    if !done {
        match fallback_sql(&w, &scope) {
            Ok(sql) => {
                emit(format!(
                    "{{\"kind\":\"wsi_sql\",\"route\":\"b\",\"conn\":{},\"sql\":{}}}",
                    jstr(CONN_ROUTE_B),
                    jstr(&sql)
                ));
                if let Err(e) = run_oledb("b", CONN_ROUTE_B, &sql) {
                    err_line("wsi", "route b execute", e);
                }
            }
            Err(e) => err_line("wsi", "route b words", e.text()),
        }
    }
    // SAFETY: balanced with the successful CoInitializeEx above.
    unsafe { CoUninitialize() };
}

pub fn cmd_wsi(args: &[&str]) {
    let mut route = "a".to_string();
    let mut words: Vec<String> = Vec::new();
    let mut it = args.iter();
    while let Some(a) = it.next() {
        if *a == "--route" {
            route = it.next().map(|s| s.to_string()).unwrap_or_default();
        } else {
            words.push(a.to_string());
        }
    }
    if words.is_empty() || (route != "a" && route != "b") {
        eprintln!("usage: launcher-probe wsi [--route a|b] <words>");
        std::process::exit(2);
    }
    mem_line("before");
    let r = std::thread::Builder::new().name("wsi".into()).spawn(move || wsi_thread(route, words));
    match r {
        Ok(h) => {
            let _ = h.join();
        }
        Err(e) => err_line("wsi", "spawn", e),
    }
    mem_line("after_join");
    std::thread::sleep(Duration::from_secs(60));
    mem_line("after_60s");
}

// ---- pt -----------------------------------------------------------------------------------------------------

fn image_path(pid: u32) -> Option<String> {
    // SAFETY: query-only open; the handle is closed by Handle.
    let h = Handle(unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid) }.ok()?);
    let mut buf = vec![0u16; 1024];
    let mut len = buf.len() as u32;
    // SAFETY: buf is writable for len UTF-16 units.
    unsafe { QueryFullProcessImageNameW(h.0, PROCESS_NAME_WIN32, PWSTR(buf.as_mut_ptr()), &mut len) }.ok()?;
    buf.get(..len as usize).map(String::from_utf16_lossy)
}

pub fn cmd_pt() {
    let mut pids = vec![0u32; 4096];
    let mut needed = 0u32;
    // SAFETY: pids is writable for the byte count given.
    if let Err(e) = unsafe { EnumProcesses(pids.as_mut_ptr(), (pids.len() * 4) as u32, &mut needed) } {
        err_line("pt", "EnumProcesses", e);
        return;
    }
    let n = (needed as usize / 4).min(pids.len());
    let mut found = 0;
    for pid in pids.iter().take(n).copied().filter(|p| *p != 0) {
        if let Some(path) = image_path(pid) {
            if is_pt_image(file_name_of(&path)) {
                found += 1;
                emit(format!("{{\"kind\":\"pt\",\"pid\":{pid},\"image\":{}}}", jstr(&path)));
            }
        }
    }
    emit(format!("{{\"kind\":\"pt_done\",\"matches\":{found}}}"));
}

// ---- links --------------------------------------------------------------------------------------------------

pub fn cmd_links() {
    // SAFETY: balanced with CoUninitialize below.
    if let Err(e) = unsafe { CoInitializeEx(None, COINIT_APARTMENTTHREADED) }.ok() {
        err_line("links", "CoInitializeEx", e);
        return;
    }
    links_inner();
    // SAFETY: balanced with the successful CoInitializeEx above.
    unsafe { CoUninitialize() };
}

fn links_inner() {
    // SAFETY: shell COM calls on this STA thread; display-name strings are freed in take_cotask.
    unsafe {
        let folder: IShellItem = match SHCreateItemFromParsingName(
            windows::core::w!("shell:AppsFolder"),
            None::<&windows::Win32::System::Com::IBindCtx>,
        ) {
            Ok(f) => f,
            Err(e) => return err_line("links", "shell:AppsFolder", e),
        };
        let en: IEnumShellItems =
            match folder.BindToHandler(None::<&windows::Win32::System::Com::IBindCtx>, &BHID_EnumItems) {
                Ok(e) => e,
                Err(e) => return err_line("links", "enumerate", e),
            };
        let (mut total, mut shown) = (0u32, 0u32);
        loop {
            let mut item: [Option<IShellItem>; 1] = [None];
            let mut got = 0u32;
            if en.Next(&mut item, Some(&mut got)).is_err() || got == 0 {
                break;
            }
            let Some(it) = item[0].take() else { break };
            total += 1;
            let parsing = it.GetDisplayName(SIGDN_DESKTOPABSOLUTEPARSING).map(take_cotask).unwrap_or_default();
            let is_folder = it.GetAttributes(SFGAO_FOLDER).map(|a| (a.0 & SFGAO_FOLDER.0) != 0).unwrap_or(false);
            if is_named_link(&parsing, is_folder) {
                shown += 1;
                let name = it.GetDisplayName(SIGDN_NORMALDISPLAY).map(take_cotask).unwrap_or_default();
                emit(format!(
                    "{{\"kind\":\"link\",\"name\":{},\"parsing\":{},\"folder\":{is_folder}}}",
                    jstr(&name),
                    jstr(&parsing)
                ));
            }
        }
        emit(format!("{{\"kind\":\"links_done\",\"entries\":{total},\"named_links\":{shown}}}"));
    }
}

// ---- fgwatch ------------------------------------------------------------------------------------------------

unsafe extern "system" fn fg_proc(_h: HWINEVENTHOOK, _event: u32, hwnd: HWND, _o: i32, _c: i32, _t: u32, _ms: u32) {
    if hwnd.0.is_null() {
        return;
    }
    let mut pid = 0u32;
    // SAFETY: pid is writable.
    unsafe { GetWindowThreadProcessId(hwnd, Some(&mut pid)) };
    // Only the exe file name: never a path or a window title.
    let image = image_path(pid).map(|p| file_name_of(&p).to_string()).unwrap_or_default();
    // SAFETY: plain query.
    let t = unsafe { GetTickCount() };
    emit(format!("{{\"t\":{t},\"image\":{}}}", jstr(&image)));
}

pub fn cmd_fgwatch(args: &[&str]) {
    let secs: u64 = match args.first().and_then(|s| s.parse().ok()) {
        Some(s) if (1..=3600).contains(&s) => s,
        _ => {
            eprintln!("usage: launcher-probe fgwatch <seconds 1..3600>");
            std::process::exit(2);
        }
    };
    // SAFETY: out-of-context WinEvent hook, delivered to this thread's message loop below; no key hook.
    let hook = unsafe {
        SetWinEventHook(
            EVENT_SYSTEM_FOREGROUND,
            EVENT_SYSTEM_FOREGROUND,
            None,
            Some(fg_proc),
            0,
            0,
            WINEVENT_OUTOFCONTEXT,
        )
    };
    if hook.0.is_null() {
        err_line("fgwatch", "SetWinEventHook", "returned null");
        return;
    }
    let end = Instant::now() + Duration::from_secs(secs);
    while Instant::now() < end {
        let left = end.saturating_duration_since(Instant::now()).as_millis().min(100) as u32;
        // SAFETY: no handles to wait on, only messages; the loop below dispatches them.
        unsafe { MsgWaitForMultipleObjects(None, false, left, QS_ALLINPUT) };
        let mut msg = MSG::default();
        // SAFETY: msg is writable; this thread owns the hook's queue.
        while unsafe { PeekMessageW(&mut msg, None, 0, 0, PM_REMOVE) }.as_bool() {
            // SAFETY: the message came from PeekMessageW.
            unsafe {
                let _ = TranslateMessage(&msg);
                DispatchMessageW(&msg);
            }
        }
    }
    // SAFETY: hook was installed above.
    let _ = unsafe { UnhookWinEvent(hook) };
    emit(format!("{{\"kind\":\"fgwatch_done\",\"seconds\":{secs}}}"));
}

// ---- cmdpal-items -------------------------------------------------------------------------------------------

pub fn cmd_cmdpal(args: &[&str]) {
    if args.is_empty() {
        eprintln!("usage: launcher-probe cmdpal-items <file>... (paths are given by the owner; none is guessed)");
        std::process::exit(2);
    }
    for f in args {
        let text = match std::fs::read_to_string(f) {
            Ok(t) => t,
            Err(e) => {
                err_line("cmdpal", f, e);
                continue;
            }
        };
        let v: serde_json::Value = match serde_json::from_str(text.trim_start_matches('\u{feff}')) {
            Ok(v) => v,
            Err(e) => {
                err_line("cmdpal", f, e);
                continue;
            }
        };
        for (p, val) in scan_cmdpal(&v, 2000) {
            emit(format!(
                "{{\"kind\":\"cmdpal\",\"file\":{},\"path\":{},\"value\":{}}}",
                jstr(f),
                jstr(&p),
                jstr(&val)
            ));
        }
    }
}
