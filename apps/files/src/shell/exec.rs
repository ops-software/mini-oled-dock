//! Open, open with, properties, "open in Explorer" and typed commands in the shell host (SH6). Each runs inside
//! busy_enter, with no deadline (a verb may take as long as it needs; a kill decided before it starts refuses it with
//! E_ABORT), and its ShellExecuteExW inside a crumbs guard. The shell reports a failed open with its own UI, as
//! Explorer does; in test mode SEE_MASK_FLAG_NO_UI keeps every error box away. Pin, Unpin, Share and the modern
//! verbs answer E_NOTIMPL until the files app adds them (shell/pins.rs, shell/modern.rs).

use std::ffi::OsString;
use std::os::windows::ffi::{OsStrExt, OsStringExt};
use std::path::{Path, PathBuf};

use oled_trust::frame::files::{CallKind, ItemRef, ReqId, ShellEvt, Verb, WidePath};
use windows::core::{HSTRING, PCWSTR};
use windows::Win32::Foundation::{ERROR_FILE_NOT_FOUND, ERROR_SUCCESS, E_ABORT, E_INVALIDARG, E_NOTIMPL};
use windows::Win32::Storage::FileSystem::{GetFileAttributesW, FILE_ATTRIBUTE_DIRECTORY, INVALID_FILE_ATTRIBUTES};
use windows::Win32::System::Registry::{RegGetValueW, HKEY_CURRENT_USER, HKEY_LOCAL_MACHINE, RRF_RT_REG_SZ};
use windows::Win32::System::SystemInformation::{GetSystemDirectoryW, GetSystemWindowsDirectoryW};
use windows::Win32::UI::Shell::{
    ShellExecuteExW, SEE_MASK_FLAG_NO_UI, SEE_MASK_INVOKEIDLIST, SEE_MASK_NOASYNC, SHELLEXECUTEINFOW,
};
use windows::Win32::UI::WindowsAndMessaging::SW_SHOWNORMAL;

use crate::shell::crumbs;
use crate::shell::host::HostCfg;
use crate::shell::writer::Writer;

/// How a verb runs.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ExecPlan {
    /// ShellExecuteExW of each item with this verb (None: the default verb); idlist adds SEE_MASK_INVOKEIDLIST.
    Shell { verb: Option<&'static str>, idlist: bool },
    /// %WINDIR%\explorer.exe with the quoted path.
    Explorer,
}

/// The plan of a verb, or the HRESULT it fails with.
pub fn plan(v: Verb) -> Result<ExecPlan, i32> {
    match v {
        Verb::Open => Ok(ExecPlan::Shell { verb: None, idlist: false }),
        Verb::OpenWith => Ok(ExecPlan::Shell { verb: Some("openas"), idlist: false }),
        Verb::Properties => Ok(ExecPlan::Shell { verb: Some("properties"), idlist: true }),
        Verb::OpenInExplorer => Ok(ExecPlan::Explorer),
        Verb::Pin | Verb::Unpin | Verb::Share | Verb::InvokeModern(_) => Err(E_NOTIMPL.0),
    }
}

fn wide(units: &[u16]) -> Vec<u16> {
    units.iter().copied().chain(Some(0)).collect()
}

/// One ShellExecuteExW, inside a crumbs guard of kind Exec; its HRESULT.
fn execute(verb: Option<&str>, file: &[u16], params: Option<&[u16]>, dir: Option<&[u16]>, mask: u32, ext: &str) -> i32 {
    let verb = verb.map(HSTRING::from);
    let file = wide(file);
    let params = params.map(wide);
    let dir = dir.map(wide);
    let pc = |v: &Option<Vec<u16>>| v.as_ref().map_or(PCWSTR::null(), |v| PCWSTR(v.as_ptr()));
    let mut info = SHELLEXECUTEINFOW {
        cbSize: std::mem::size_of::<SHELLEXECUTEINFOW>() as u32,
        fMask: mask,
        lpVerb: verb.as_ref().map_or(PCWSTR::null(), |v| PCWSTR(v.as_ptr())),
        lpFile: PCWSTR(file.as_ptr()),
        lpParameters: pc(&params),
        lpDirectory: pc(&dir),
        nShow: SW_SHOWNORMAL.0,
        ..Default::default()
    };
    let _g = crumbs::enter(CallKind::Exec, ext);
    // SAFETY: every string is NUL-terminated and lives for the call; the structure is sized in cbSize.
    match unsafe { ShellExecuteExW(&mut info) } {
        Ok(()) => 0,
        Err(e) => e.code().0,
    }
}

/// Runs the work under the busy protocol and answers ExecDone with its HRESULT (E_ABORT once a kill was decided).
fn busy_exec(req: ReqId, w: &Writer, work: impl FnOnce() -> i32) {
    let Ok(busy) = crumbs::busy_enter() else {
        w.send(&ShellEvt::ExecDone { req, hr: E_ABORT.0 });
        return;
    };
    w.send(&ShellEvt::Busy { on: true });
    let hr = work();
    drop(busy);
    w.send(&ShellEvt::Busy { on: false });
    w.send(&ShellEvt::ExecDone { req, hr });
}

/// Serves ShellReq::Exec on the exec STA.
pub fn run(req: ReqId, verb: Verb, items: &[ItemRef], w: &Writer, cfg: &HostCfg) {
    let plan = match plan(verb) {
        Ok(p) => p,
        Err(hr) => return w.send(&ShellEvt::Failed { req, hr }),
    };
    let mut paths: Vec<&WidePath> = Vec::with_capacity(items.len());
    for it in items {
        match it {
            ItemRef::Path(p) => paths.push(p),
            ItemRef::Pidl(_) => return w.send(&ShellEvt::Failed { req, hr: E_INVALIDARG.0 }),
        }
    }
    if paths.is_empty() {
        return w.send(&ShellEvt::Failed { req, hr: E_INVALIDARG.0 });
    }
    let no_ui = if cfg.test_no_ui { SEE_MASK_FLAG_NO_UI } else { 0 };
    busy_exec(req, w, || {
        let mut first = 0;
        for p in paths {
            let ext = crumbs::ext_of(p);
            let hr = match plan {
                ExecPlan::Shell { verb, idlist } => {
                    let mask = SEE_MASK_NOASYNC | no_ui | if idlist { SEE_MASK_INVOKEIDLIST } else { 0 };
                    execute(verb, p.units(), None, None, mask, &ext)
                }
                ExecPlan::Explorer => {
                    let windir = std::env::var_os("WINDIR").unwrap_or_else(|| "C:\\Windows".into());
                    let exe: Vec<u16> =
                        std::path::Path::new(&windir).join("explorer.exe").as_os_str().encode_wide().collect();
                    let quote = Some(u16::from(b'"'));
                    let quoted: Vec<u16> = quote.into_iter().chain(p.units().iter().copied()).chain(quote).collect();
                    execute(None, &exe, Some(&quoted), None, SEE_MASK_NOASYNC | no_ui, &ext)
                }
            };
            if first == 0 && hr < 0 {
                first = hr;
            }
        }
        first
    });
}

/// Serves ShellReq::RunTyped on the exec STA: the text as the Run box takes it, started in the given folder; its
/// program is looked up and started by run_typed_with, both inside the busy protocol.
pub fn run_typed(req: ReqId, text: &str, dir: &WidePath, w: &Writer, cfg: &HostCfg) {
    let mask = SEE_MASK_NOASYNC | if cfg.test_no_ui { SEE_MASK_FLAG_NO_UI } else { 0 };
    busy_exec(req, w, || {
        run_typed_with(text, find_on_system, |file, params| execute(None, file, params, Some(dir.units()), mask, ""))
    });
}

/// The start of a typed command with its two seams, the lookup and the start: its program is looked up everywhere
/// ShellExecuteExW looks except the folder the command starts in (typed_plan, with `find`), and `exec` is given the
/// full path found and the parameters. A program found nowhere never reaches `exec`: nothing starts, and its HRESULT
/// (ERROR_FILE_NOT_FOUND) is returned.
pub fn run_typed_with(
    text: &str,
    find: impl Fn(&str) -> Option<PathBuf>,
    exec: impl FnOnce(&[u16], Option<&[u16]>) -> i32,
) -> i32 {
    match typed_plan(text, find) {
        Ok((file, params)) => exec(&file, params.as_deref()),
        Err(hr) => hr,
    }
}

/// A typed command split into its program and the rest: a first word in double quotes, else the text up to the first
/// space or tab; the rest without the blanks before it.
pub fn split_command(text: &str) -> (&str, &str) {
    let t = text.trim_start();
    let (program, rest) = match t.strip_prefix('"') {
        Some(quoted) => quoted.split_once('"').unwrap_or((quoted, "")),
        None => t.split_once([' ', '\t']).unwrap_or((t, "")),
    };
    (program, rest.trim_start())
}

/// A program name with no folder, drive, stream or scheme in it.
fn is_bare(name: &str) -> bool {
    !name.is_empty() && !name.contains(['\\', '/', ':'])
}

/// A program of the two forms the hand-offs send: a shell: name, in any case, or a server (\\).
fn handed_off(program: &str) -> bool {
    program.starts_with(r"\\") || program.get(..6).is_some_and(|p| p.eq_ignore_ascii_case("shell:"))
}

/// What ShellExecuteExW is given for a typed command, as (file, parameters): a bare program becomes the full path
/// `find` gives (find_program over this system) and the rest of the text its parameters; a bare program found
/// nowhere is ERROR_FILE_NOT_FOUND, and nothing starts. Text whose program is one of the hand-offs' forms (handed_off)
/// is given as it is; any other program (the empty one of text that opens with "", a relative path) is
/// ERROR_FILE_NOT_FOUND too, since ShellExecuteExW would strip its quotes and look it up in the folder the command
/// starts in. That folder is no input: it is never searched.
pub fn typed_plan(text: &str, find: impl Fn(&str) -> Option<PathBuf>) -> Result<(Vec<u16>, Option<Vec<u16>>), i32> {
    let missing = ERROR_FILE_NOT_FOUND.to_hresult().0;
    let (program, rest) = split_command(text);
    if !is_bare(program) {
        return handed_off(program).then(|| (text.encode_utf16().collect(), None)).ok_or(missing);
    }
    let full = find(program).ok_or(missing)?;
    let params = (!rest.is_empty()).then(|| rest.encode_utf16().collect());
    Ok((full.as_os_str().encode_wide().collect(), params))
}

/// The full path of the program a typed command names, looked up where ShellExecuteExW looks for one (the Windows
/// folder, the system folder, the folders of PATH, then the App Paths registration) except its first place, the folder
/// the command starts in: a program planted in the folder being browsed (a download, an archive, a stick, a share that
/// answers as it likes) never runs in place of the one the user meant. Only a bare name is looked up: under itself when
/// it has an extension, then with each extension of `pathext` in its order, folder by folder in the order of `dirs`;
/// `app_path` answers last. None when nothing is found. Pure: the facts are injected.
pub fn find_program(
    name: &str,
    pathext: &str,
    dirs: &[PathBuf],
    is_file: impl Fn(&Path) -> bool,
    app_path: impl Fn(&str) -> Option<PathBuf>,
) -> Option<PathBuf> {
    if !is_bare(name) {
        return None;
    }
    let mut names: Vec<String> = Vec::new();
    if Path::new(name).extension().is_some() {
        names.push(name.to_string());
    }
    let exts = pathext.split(';').map(str::trim).filter(|e| e.len() > 1 && e.starts_with('.'));
    names.extend(exts.map(|e| format!("{name}{e}")));
    dirs.iter().find_map(|d| names.iter().map(|n| d.join(n)).find(|p| is_file(p))).or_else(|| app_path(name))
}

/// find_program over this system: PATHEXT (".COM;.EXE;.BAT;.CMD" when it is unset), search_dirs, is_file and
/// app_path.
fn find_on_system(name: &str) -> Option<PathBuf> {
    let pathext = std::env::var("PATHEXT").unwrap_or_else(|_| ".COM;.EXE;.BAT;.CMD".to_string());
    find_program(name, &pathext, &search_dirs(), is_file, app_path)
}

/// The system's Windows folder and its system folder (never the environment's), then each absolute folder of PATH: a
/// relative one, such as ".", would be the folder the command starts in.
fn search_dirs() -> Vec<PathBuf> {
    let mut dirs = Vec::new();
    let mut buf = [0u16; 261];
    // SAFETY: a writable buffer whose length is passed.
    let n = unsafe { GetSystemWindowsDirectoryW(Some(&mut buf)) } as usize;
    dirs.extend(buf.get(..n).filter(|u| !u.is_empty()).map(|u| PathBuf::from(OsString::from_wide(u))));
    // SAFETY: as above.
    let n = unsafe { GetSystemDirectoryW(Some(&mut buf)) } as usize;
    dirs.extend(buf.get(..n).filter(|u| !u.is_empty()).map(|u| PathBuf::from(OsString::from_wide(u))));
    if let Some(path) = std::env::var_os("PATH") {
        dirs.extend(std::env::split_paths(&path).filter(|d| d.is_absolute()));
    }
    dirs
}

/// A file at `p`, not a folder (GetFileAttributesW does not follow a link, so an app execution alias is found).
fn is_file(p: &Path) -> bool {
    let name = HSTRING::from(p.as_os_str());
    // SAFETY: a NUL-terminated path, alive for the call.
    let attrs = unsafe { GetFileAttributesW(&name) };
    attrs != INVALID_FILE_ATTRIBUTES && attrs & FILE_ATTRIBUTE_DIRECTORY.0 == 0
}

/// The App Paths registration of a program, the current user's then the machine's: its full path (a value to expand
/// is expanded), None when there is none.
fn app_path(name: &str) -> Option<PathBuf> {
    let exe = if Path::new(name).extension().is_some() { name.to_string() } else { format!("{name}.exe") };
    let key = HSTRING::from(format!(r"Software\Microsoft\Windows\CurrentVersion\App Paths\{exe}"));
    [HKEY_CURRENT_USER, HKEY_LOCAL_MACHINE].into_iter().find_map(|root| {
        let mut text = [0u16; 1024];
        let mut bytes = std::mem::size_of_val(&text) as u32;
        let data = Some(text.as_mut_ptr().cast());
        // SAFETY: a NUL-terminated key; `data` points at `bytes` writable bytes, which the call ends with a NUL.
        let r = unsafe { RegGetValueW(root, &key, PCWSTR::null(), RRF_RT_REG_SZ, None, data, Some(&mut bytes)) };
        if r != ERROR_SUCCESS {
            return None;
        }
        let len = text.iter().position(|&u| u == 0).unwrap_or(text.len());
        let value = String::from_utf16(text.get(..len)?).ok()?;
        Some(PathBuf::from(value.trim().trim_matches('"'))).filter(|p| p.is_absolute())
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testkit::TempRoot;
    use std::cell::RefCell;

    /// A typed program is looked up where ShellExecuteExW looks, but never in the folder the command starts in (no
    /// input names it): a bare name under itself when it has an extension, then with each extension of PATHEXT in that
    /// order, folder by folder in the order given, and App Paths last. A name with a folder, a drive, a stream or a
    /// scheme in it is no program name and is never looked up.
    #[test]
    fn typed_programs_are_looked_up_outside_the_folder_they_start_in() {
        let windows = PathBuf::from(r"C:\made-up\win");
        let (system, tools) = (windows.join("sys"), PathBuf::from(r"C:\made-up\tools"));
        let dirs = [windows.clone(), system.clone(), tools.clone()];
        let files = [
            system.join("cmd.exe"),
            system.join("ncpa.cpl"),
            tools.join("made-up-tool.cmd"),
            tools.join("made-up-tool.exe"),
            windows.join("made-up-both.exe"),
            system.join("made-up-both.exe"),
        ];
        // Names compare as the file system compares them: ignoring case.
        let is_file = |p: &Path| files.iter().any(|f| f.to_string_lossy().eq_ignore_ascii_case(&p.to_string_lossy()));
        let ext = ".COM;.EXE;.BAT;.CMD";
        let find = |name: &str| find_program(name, ext, &dirs, is_file, |_| None);
        assert_eq!(find("cmd"), Some(system.join("cmd.EXE")));
        assert_eq!(find("cmd.exe"), Some(system.join("cmd.exe")), "a name with its extension");
        assert_eq!(find("ncpa.cpl"), Some(system.join("ncpa.cpl")), "an extension PATHEXT does not list");
        assert_eq!(find("made-up-tool"), Some(tools.join("made-up-tool.EXE")), "PATHEXT's order in a folder");
        assert_eq!(find("made-up-both"), Some(windows.join("made-up-both.EXE")), "the folders in their order");
        assert_eq!(find("made-up-missing"), None);
        let app = PathBuf::from(r"C:\made-up\apps\app.exe");
        let app_path = |n: &str| (n == "made-up-app" || n == "cmd").then(|| app.clone());
        assert_eq!(find_program("made-up-app", ext, &dirs, is_file, app_path), Some(app.clone()), "App Paths");
        assert_eq!(find_program("cmd", ext, &dirs, is_file, app_path), Some(system.join("cmd.EXE")), "folders first");
        for name in [r"sub\cmd", "sub/cmd", "C:cmd", "shell:cmd", ""] {
            assert_eq!(find_program(name, ext, &dirs, |_| true, |_| Some(app.clone())), None, "{name}");
        }
    }

    /// A typed command starts the full path its program was found at, the rest of the text as its parameters, and a
    /// program found nowhere starts nothing (ERROR_FILE_NOT_FOUND). Text that does not start with a bare name (the
    /// hand-offs' shell: names and servers) is given as it is and nothing is looked up.
    #[test]
    fn typed_commands_start_the_program_found_or_nothing() {
        let tool = PathBuf::from(r"C:\made-up\tools\made-up-tool.exe");
        let asked = RefCell::new(Vec::<String>::new());
        let find = |name: &str| {
            asked.borrow_mut().push(name.to_string());
            (name == "made-up-tool" || name == "made up tool").then(|| tool.clone())
        };
        let w = |s: &str| s.encode_utf16().collect::<Vec<u16>>();
        let full = w(r"C:\made-up\tools\made-up-tool.exe");
        assert_eq!(typed_plan("made-up-tool --flag x", find), Ok((full.clone(), Some(w("--flag x")))));
        assert_eq!(typed_plan("  made-up-tool", find), Ok((full.clone(), None)));
        assert_eq!(typed_plan(r#""made up tool"  "a b""#, find), Ok((full.clone(), Some(w(r#""a b""#)))));
        assert_eq!(typed_plan("made-up-missing --flag", find), Err(ERROR_FILE_NOT_FOUND.to_hresult().0));
        assert_eq!(*asked.borrow(), ["made-up-tool", "made-up-tool", "made up tool", "made-up-missing"]);
        asked.borrow_mut().clear();
        for text in ["shell:RecycleBinFolder", r"\\server-made-up"] {
            assert_eq!(typed_plan(text, find), Ok((w(text), None)), "{text}");
        }
        assert!(asked.borrow().is_empty(), "{:?}", asked.borrow());
    }

    /// A program that is no bare name starts only in the two forms the hand-offs send, a shell: name in any case and a
    /// server (\\), as the text is. Any other is never given to ShellExecuteExW, which would strip its quotes and look
    /// it up in the folder the command starts in: the empty program of text that opens with "" (typed """cmd"""), a
    /// relative path. Each is ERROR_FILE_NOT_FOUND, even though a bare lookup would find the name.
    #[test]
    fn typed_commands_with_no_bare_program_start_only_the_hand_offs() {
        let tool = PathBuf::from(r"C:\made-up\tools\made-up-tool.exe");
        let find = |name: &str| (name == "cmd" || name == "made-up-tool").then(|| tool.clone());
        let refused = [
            r#"""cmd"""#,
            r#"""made-up-tool"""#,
            r#"""cmd"" x"#,
            r#""" cmd"#,
            r"sub\cmd",
            r".\cmd",
            "made-up/cmd x",
            "C:cmd",
            r"%made-up\cmd%",
        ];
        for text in refused {
            let mut called = false;
            let hr = run_typed_with(text, find, |_, _| {
                called = true;
                0
            });
            assert_eq!(hr, ERROR_FILE_NOT_FOUND.to_hresult().0, "{text}");
            assert!(!called, "{text} reached ShellExecuteExW");
        }
        let w = |s: &str| s.encode_utf16().collect::<Vec<u16>>();
        let handed = [
            "shell:RecycleBinFolder",
            "SHELL:MyComputerFolder",
            "shell:Common Programs",
            r"\\server-made-up",
            r"\\server-made-up\",
            r"\\?\UNC\server-made-up",
        ];
        for text in handed {
            let mut started = Vec::new();
            let hr = run_typed_with(text, find, |file, params| {
                started.push((file.to_vec(), params.map(<[u16]>::to_vec)));
                0
            });
            assert_eq!(hr, 0, "{text}");
            assert_eq!(started, [(w(text), None)], "{text}");
        }
    }

    /// Over this system, cmd is the system folder's own, found without the folder a command starts in.
    #[test]
    fn typed_cmd_is_the_systems() {
        let found = find_on_system("cmd").expect("cmd is found");
        let mut buf = [0u16; 261];
        // SAFETY: a writable buffer whose length is passed.
        let n = unsafe { GetSystemDirectoryW(Some(&mut buf)) } as usize;
        let system = PathBuf::from(OsString::from_wide(buf.get(..n).unwrap_or_default()));
        let want = system.join("cmd.exe");
        assert!(found.to_string_lossy().eq_ignore_ascii_case(&want.to_string_lossy()), "{found:?}");
    }

    /// A typed command is started through its lookup: the system's cmd, never the cmd.bat planted in the folder the
    /// command starts in, with the rest of the text as its parameters; a program found nowhere but in that folder is
    /// never started (ERROR_FILE_NOT_FOUND). Nothing starts here: the start is a recorder.
    #[test]
    fn typed_commands_never_start_a_program_planted_in_their_folder() {
        let root = TempRoot::new("typed_planted");
        root.file("cmd.bat", b"@exit /b 1\r\n");
        root.file("made-up-tool.cmd", b"@exit /b 1\r\n");
        let ext = ".COM;.EXE;.BAT;.CMD";
        let found_in = |dirs: &[PathBuf], name: &str| find_program(name, ext, dirs, is_file, |_| None);
        // A lookup that begins in that folder, as ShellExecuteExW's does, would start the planted programs.
        let with_folder: Vec<PathBuf> = std::iter::once(root.path().to_path_buf()).chain(search_dirs()).collect();
        for name in ["cmd", "made-up-tool"] {
            assert!(found_in(&with_folder, name).is_some_and(|p| p.starts_with(root.path())), "{name} is planted");
        }
        let mut system = Vec::new();
        let mut buf = [0u16; 261];
        // SAFETY: a writable buffer whose length is passed.
        let n = unsafe { GetSystemWindowsDirectoryW(Some(&mut buf)) } as usize;
        system.push(OsString::from_wide(buf.get(..n).unwrap_or_default()));
        // SAFETY: as above.
        let n = unsafe { GetSystemDirectoryW(Some(&mut buf)) } as usize;
        system.push(OsString::from_wide(buf.get(..n).unwrap_or_default()));
        let find = |name: &str| found_in(&search_dirs(), name);
        let mut started = Vec::new();
        let hr = run_typed_with("cmd made-up.txt", find, |file, params| {
            started.push((PathBuf::from(OsString::from_wide(file)), params.map(String::from_utf16_lossy)));
            0
        });
        assert_eq!(hr, 0);
        assert_eq!(started.len(), 1, "{started:?}");
        let (file, params) = &started[0];
        assert!(!file.starts_with(root.path()), "{file:?}");
        let folder = file.parent().map(Path::as_os_str).unwrap_or_default();
        assert!(system.iter().any(|s| s.eq_ignore_ascii_case(folder)), "{file:?} is not under {system:?}");
        assert_eq!(params.as_deref(), Some("made-up.txt"));
        let mut called = false;
        let hr = run_typed_with("made-up-tool", find, |_, _| {
            called = true;
            0
        });
        assert_eq!(hr, ERROR_FILE_NOT_FOUND.to_hresult().0);
        assert!(!called, "a program found nowhere but in the folder is never started");
    }

    #[test]
    fn exec_unknown_verbs_fail_notimpl() {
        assert_eq!(plan(Verb::Open), Ok(ExecPlan::Shell { verb: None, idlist: false }));
        assert_eq!(plan(Verb::OpenWith), Ok(ExecPlan::Shell { verb: Some("openas"), idlist: false }));
        assert_eq!(plan(Verb::Properties), Ok(ExecPlan::Shell { verb: Some("properties"), idlist: true }));
        assert_eq!(plan(Verb::OpenInExplorer), Ok(ExecPlan::Explorer));
        for v in [Verb::Pin, Verb::Unpin, Verb::Share, Verb::InvokeModern(3)] {
            assert_eq!(plan(v), Err(E_NOTIMPL.0), "{v:?}");
        }
    }
}
