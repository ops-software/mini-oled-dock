# Review Fixes (Part 1 of 1.4.0) Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Fix the 12 confirmed findings of the 2026-11-08 review of mini-oled-dock (security first, then taskbar, then the rest), and write the two release-note lines for the refuted findings.

**Architecture:** Each finding gets one task. Where the code is pure Win32, the decision is extracted into a small pure function in the same file and unit-tested; the Win32 call site only applies the decision. The highest-severity fix (1) removes the elevated `powershell.exe` entirely: the export runs de-elevated through the existing desktop-shell path (`shell_thread::desktop_shell_execute`) and the elevated dock only reads a result file it treats as untrusted. No config key is added or changed; no existing default changes.

**Tech Stack:** Rust stable (`rust-version = "1.87"`), crate `windows` 0.62, `serde_json`, raw Win32, no UI framework. Tests are in-file `#[cfg(test)] mod tests`.

**Spec:** `E:\Giovik\Documenti\Dock\dock\docs\superpowers\specs\2026-11-09-settings-window-updater-fixes-design.md` (Part 1). Finding details, failure scenarios and verifier call paths: `E:\Giovik\Documenti\Dock\docs\review-2026-11-08-findings.json` (array of 12, in the same order as the tasks below; the `line` values in it are review-time lines, the real lines are quoted in each task).

## Global Constraints

- Repository `E:\Giovik\Documenti\Dock\dock`, branch `release/1.4.0`. Work on a branch `fixes/1.4.0-review` cut from it; `main` is protected; one pull request per piece (this plan is the first PR), green on CI: `cargo test --locked` with `DOCK_NO_MANIFEST=1`, `cargo build --release --locked`, `cargo fmt` (`rustfmt.toml`: `max_width = 120`, `use_small_heuristics = "Max"`).
- Language of code, comments, UI strings and release notes: English; Italian labels only through `i18n::tr(en, it)`.
- Run tests as `DOCK_NO_MANIFEST=1 CARGO_TARGET_DIR=target-test cargo test --locked [filter]` (Git Bash; PowerShell: `$env:DOCK_NO_MANIFEST=1; $env:CARGO_TARGET_DIR='target-test'; cargo test --locked [filter]`). Cargo accepts one filter per command: when a step lists two, run them as two commands.
- Crate `windows` 0.62, no UI framework, no HTTP crate. A new Win32 DLL used only by the dock goes in `DELAY_LOAD` in `build.rs`.
- The dock runs elevated. It writes files only in admin-only folders (`%ProgramData%\mini-oled-dock`, `%ProgramFiles%\mini-oled-dock`), never in `%TEMP%` or `%LOCALAPPDATA%`. Everything it reads from user-writable places is untrusted (size cap, no reparse points, parse defensively).
- `--preview <dir>` must keep rendering every popup byte-identically. Build with `DOCK_NO_MANIFEST=1` and run `dock.exe --preview <dir>`.
- No existing config default changes: "nothing changes behind your back".
- Anti-goals: the native taskbar left hidden in any exit path; anything running with the elevated token from user-controlled input; the dock's RAM going up while a popup is closed.
- Every commit message is in English and ends with the line `Claude-Session: https://claude.ai/code/session_01FHR79Qfsfd7dTKsSjKFXxX` (the commands below pass it as the last `-m`).
- Order of work is fixed: security (1-4), taskbar (5-6), the rest (7-12), docs (13). Messages for the dock window live in `src\msg.rs`; messages private to the tray-host window stay next to the other `WM_HOST_*` constants in `src\tray_host.rs` (`WM_APP + n`, lines 349-351 and 1075).
- **Preview baseline recipe** (used by tasks 7 and 9): before touching any code run `DOCK_NO_MANIFEST=1 CARGO_TARGET_DIR=target-preview cargo build --release --locked`, then `target-preview\release\dock.exe --preview ..\preview\fixes-base`. After a task: same build, `--preview ..\preview\fixes-new`, then in PowerShell `Compare-Object (Get-ChildItem ..\preview\fixes-base -Recurse -File | Get-FileHash) (Get-ChildItem ..\preview\fixes-new -Recurse -File | Get-FileHash) -Property Hash` must print nothing.

## Review Focus

Inputs and conditions the spec implies but the 12 decided fixes do not obviously test, most likely first. Each has its test in the task named in brackets.

1. [Task 7] **Start menu reopened after it was closed**: the icons are gone from the dock but the shell thread still believes it "already sent" them, so the second open would show blank tiles forever. Expected: reopening shows the icons again (the shell side forgets too).
2. [Task 1] **User profile path with an apostrophe or non-ASCII characters** (`C:\Users\O'Brien\AppData\Local`): the PowerShell command must still be one valid literal per path. Expected: import works; the `'` is doubled.
3. [Task 3] **A real AppBar app** (a medium-integrity sidebar reserving screen space) must not crash or hang the tray host when its message is now rejected; it gets a `0` reply and the rejection is logged with the claimed pid. Expected: the app keeps running; one log line per rejection.
4. [Task 9] **Short or high-DPI work area** (1080p at 200 %, 768p at 125 %, a vertical monitor): every popup ends inside the work area minus the margin; the Start menu keeps its search box and, in the horizontal style, its bottom bar clickable; the tray chooser shows fewer rows. Expected: the Start menu and the tray chooser lists scroll, nothing off screen. The audio popup and the calendar have no scrolling: they are only cut at the work-area edge (scope note in Task 9).
5. [Task 5] **`show_tray` switched off while Explorer is restarting**: when the pause ends the tray host must not come back if the setting is now off. Expected: the resume path asks `tray_sync` (which reads the current config) instead of starting the host unconditionally.

---

### Task 1: Finding 1 - run Export-StartLayout de-elevated, never from the elevated process

Severity high. `src\start_import.rs` `export_layout()` (lines 126-153; `powershell_path()` at line 120 is a separate helper that goes away with it, and `job()` starts at line 155) starts `powershell.exe` as a child of the elevated dock; the cmdlet is found through the user-writable `PSModulePath`. Fix: ask the desktop shell (Explorer, medium integrity) to start PowerShell; hand the result back through a file in the shell user's own `%LOCALAPPDATA%\mini-oled-dock\`; read it as untrusted input.

**Files:**
- Modify: `Cargo.toml` (add `"Win32_Security_Cryptography"` to the `windows` features, after `"Win32_Security_Authorization"`)
- Modify: `build.rs` (add `"bcrypt.dll"` to `DELAY_LOAD`)
- Modify: `src\shell_thread.rs` (`desktop_shell_execute`, lines 1582-1625: split; add three `pub(crate)` functions)
- Modify: `src\start_import.rs` (new protocol; `job()` lines 155-200; remove `export_layout`, `powershell_path`, `PS_TIMEOUT`; `MAX_JSON` becomes 4 MiB; module doc lines 1-18)
- Modify: `src\popup_settings.rs` (`import_label`, lines 455-470, and its test near line 1172: new `Status::TimedOut`)
- Test: in-file `mod tests` of `src\start_import.rs` and `src\shell_thread.rs`

**Interfaces:**
- Produces in `shell_thread.rs`:
  - `pub(crate) fn explorer_pid() -> windows::core::Result<u32>` (COM STA thread; pid of the process owning the desktop window found by the same `FindWindowSW(CSIDL_DESKTOP)` that `desktop_shell_execute` uses).
  - `pub(crate) fn desktop_open(file: &str, args: Option<&str>, dir: Option<&str>) -> windows::core::Result<()>` (= `desktop_shell_execute(.., Verb::Open)`; `Verb` stays private).
  - `pub(crate) fn system_dir() -> Option<PathBuf>`: `GetSystemDirectoryW` (feature `Win32_System_SystemInformation` is already enabled), never an env var. Task 4 consumes it.
- Produces in `start_import.rs` (private unless noted):
  - `const LAYOUT_TIMEOUT: Duration = Duration::from_secs(15)`, `const POLL_MS: u64 = 200`, `const MAX_JSON: u64 = 4 << 20`.
  - `enum Poll { Wait, Ready, Failed, TimedOut }` and `fn poll_step(file_exists: bool, err_exists: bool, elapsed: Duration) -> Poll`.
  - `fn hex16(id: &[u8; 16]) -> String` (32 lowercase hex chars) and `fn layout_file_name(id: &[u8; 16]) -> String` (`start-layout-<hex>.json`).
  - `fn export_args(dir: &str, file: &str) -> String` (the whole argument string for `powershell.exe`).
  - `fn random_id() -> Result<[u8; 16], String>` (`BCryptGenRandom` with `BCRYPT_USE_SYSTEM_PREFERRED_RNG`, null algorithm handle).
  - `fn shell_user_local_appdata(explorer_pid: u32) -> Result<PathBuf, String>`: `OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION)` -> `OpenProcessToken(TOKEN_QUERY | TOKEN_DUPLICATE | TOKEN_IMPERSONATE)` -> `SHGetKnownFolderPath(&FOLDERID_LocalAppData, KF_FLAG_DEFAULT, Some(token))`, then `\mini-oled-dock`; handles closed on every path.
  - `enum JobError { Failed(String), TimedOut }` with `impl From<String> for JobError` (= `Failed`, so the existing `?` on `String` errors from `map_layout` and `config::modify` keeps compiling); `pub(crate) enum Status` gains `TimedOut`; STATUS encoding becomes `IDLE=0, RUNNING=1, FAILED=2, TIMEDOUT=3, DONE=4` (`DONE + n` = n apps imported). In `decode` (lines 55-62) the `TIMEDOUT => Status::TimedOut` arm MUST sit before the catch-all `n => Status::Imported(n - DONE)`: with `DONE = 4` a `3` reaching the catch-all would compute `3 - 4` (debug panic, wrong status in release). The `decode(TIMEDOUT)` assertion below pins the order.
  - `fn finish_word(r: &Result<usize, JobError>) -> u32`: `Ok(n)` -> `DONE + n as u32`, `Err(TimedOut)` -> `TIMEDOUT`, `Err(Failed(_))` -> `FAILED`; the one place that turns a job result into the status word (`start()` calls it).

- [ ] **Step 1: Write the failing tests** (`start_import.rs` `mod tests`; `system_dir` test in `shell_thread.rs` `mod tests`)

```rust
#[test]
fn layout_file_name_is_128_bit_hex() {
    let id = [0xAB, 0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 0xFF];
    assert_eq!(hex16(&id), "ab000102030405060708090a0b0c0dff");
    assert_eq!(layout_file_name(&id), "start-layout-ab000102030405060708090a0b0c0dff.json");
}

#[test]
fn export_args_are_one_quoted_literal_per_path() {
    let dir = r"C:\Users\O'Brien\AppData\Local\mini-oled-dock";
    let file = r"C:\Users\O'Brien\AppData\Local\mini-oled-dock\start-layout-ab.json";
    let a = export_args(dir, file);
    assert!(a.starts_with("-NoProfile -NonInteractive -ExecutionPolicy Bypass -WindowStyle Hidden -Command \""));
    assert!(a.contains(r"'C:\Users\O''Brien\AppData\Local\mini-oled-dock'"), "apostrophe doubled: {a}");
    assert!(a.contains(r"Export-StartLayout -Path 'C:\Users\O''Brien\AppData\Local\mini-oled-dock\start-layout-ab.json.tmp'"));
    assert!(a.contains("Move-Item -Force") && a.contains(".json.err'") && a.contains("AddHours(-1)"));
    assert_eq!(a.matches('"').count(), 2, "only the -Command wrapper uses double quotes");
}

#[test]
fn poll_decides_ready_failed_timeout() {
    let s = Duration::from_secs;
    assert_eq!(poll_step(false, false, s(0)), Poll::Wait);
    assert_eq!(poll_step(true, false, s(1)), Poll::Ready);
    assert_eq!(poll_step(false, true, s(1)), Poll::Failed);
    assert_eq!(poll_step(true, true, s(1)), Poll::Ready, "a complete file wins over a stale .err");
    assert_eq!(poll_step(false, false, s(14)), Poll::Wait);
    assert_eq!(poll_step(false, false, s(15)), Poll::TimedOut);
}

#[test]
fn hostile_layout_json_is_rejected_or_skipped_without_panic() {
    let mut cat = || -> Vec<(StartApp, Option<String>)> { panic!("catalog not needed") };
    let deep = format!("{}{}", "[".repeat(100_000), "]".repeat(100_000));
    assert!(map_layout(&deep, &mut cat, &|_| None).is_err(), "serde_json recursion limit");
    assert!(map_layout(r#"{"pinnedList":"x"}"#, &mut cat, &|_| None).is_err());
    let odd = r#"{"pinnedList":[1,null,[],{"packagedAppId":5},{"packagedAppId":""}]}"#;
    assert!(map_layout(odd, &mut cat, &|_| None).unwrap().is_empty());
    assert_eq!(decode(IDLE), Status::Idle);
    assert_eq!(decode(TIMEDOUT), Status::TimedOut, "the TIMEDOUT arm precedes the Imported catch-all");
    assert_eq!(decode(DONE + 7), Status::Imported(7));
}

#[test]
fn job_results_map_to_status_words() {
    assert_eq!(finish_word(&Ok(7)), DONE + 7);
    assert_eq!(finish_word(&Err(JobError::TimedOut)), TIMEDOUT);
    assert_eq!(finish_word(&Err(JobError::Failed("x".into()))), FAILED);
    // A failing setup step (random id, shell user folder, Explorer not reachable) is reported like any
    // other failure: the `String` converts to `Failed`, the popup shows "Import failed".
    let setup: Result<usize, JobError> = Err(String::from("BCryptGenRandom failed").into());
    assert_eq!(finish_word(&setup), FAILED);
}

#[test]
fn setup_steps_return_errors_instead_of_panicking() {
    let (a, b) = (random_id().expect("BCryptGenRandom"), random_id().unwrap());
    assert_ne!(a, b);
    assert_ne!(a, [0u8; 16]);
    assert!(shell_user_local_appdata(0).is_err(), "no such process: Err, never the dock's own folder");
    // If SHGetKnownFolderPath rejects the test process token on the build machine, drop only this
    // positive assertion (the owner test in Step 6 covers it); keep the three above.
    let dir = shell_user_local_appdata(std::process::id()).expect("own token");
    assert!(dir.is_absolute() && dir.ends_with("mini-oled-dock"), "{dir:?}");
}

// shell_thread.rs
#[test]
fn system_dir_comes_from_the_api() {
    let d = system_dir().expect("GetSystemDirectoryW");
    assert!(d.is_absolute() && d.join("kernel32.dll").is_file());
}
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `DOCK_NO_MANIFEST=1 CARGO_TARGET_DIR=target-test cargo test --locked start_import`
Expected: FAIL to compile (`hex16`, `export_args`, `poll_step`, `Status::TimedOut`, `system_dir` not defined).

- [ ] **Step 3: Implement the pure functions** in `src\start_import.rs`: `poll_step`, `hex16`, `layout_file_name`, `export_args(dir: &str, file: &str) -> String`. `export_args` returns exactly `-NoProfile -NonInteractive -ExecutionPolicy Bypass -WindowStyle Hidden -Command "<script>"`, where `<script>` is the spec's command with every path inserted as a single-quoted PowerShell literal (a `'` in a path is doubled):

`try { New-Item -ItemType Directory -Force '<dir>' | Out-Null; Get-ChildItem '<dir>\start-layout-*' | Where-Object LastWriteTime -lt (Get-Date).AddHours(-1) | Remove-Item -Force; Export-StartLayout -Path '<file>.tmp'; Move-Item -Force '<file>.tmp' '<file>' } catch { Set-Content '<file>.err' $_ }`

`-WindowStyle Hidden` is the only addition to the spec's command line: `IShellDispatch2::ShellExecute` is called with `SW_SHOWNORMAL` and would flash a console window. Update the STATUS constants and `decode` (arm order as stated under Interfaces); add `finish_word`; keep `status_word`, `status`, `reset`, `start` as they are except that `start()` stores `finish_word(&result)`.

- [ ] **Step 4: Implement the Win32 parts**
  1. `Cargo.toml` feature and `build.rs` `DELAY_LOAD` entry.
  2. `shell_thread.rs`: move the lookup lines of `desktop_shell_execute` (`CoCreateInstance(ShellWindows)` through `FindWindowSW(...)`) into `fn find_desktop() -> windows::core::Result<(IDispatch, HWND)>` used by both `desktop_shell_execute` and the new `explorer_pid()`; add `desktop_open` and `system_dir`. `launch` / `launch_admin` behaviour is unchanged.
  3. `start_import.rs`: `random_id`, `shell_user_local_appdata` (the elevated process's own `%LOCALAPPDATA%` is never used).
  4. Rewrite `job()`: `CoInitializeEx(None, COINIT_APARTMENTTHREADED)` at its start with an RAII guard that calls `CoUninitialize` (today it is initialised after the file is read, `CoInitializeEx` at line 172). Every setup step has an explicit mapping, all to `JobError::Failed` (status "Import failed", the detail goes to the log, there is no elevated fallback): `pid = shell_thread::explorer_pid()` -> on `Err(e)` `Failed(format!("Explorer's desktop is not available (Explorer stopped, or restarted and not yet registered with ShellWindows): {e}"))` (this is the "Explorer is up but its desktop is not registered yet" case: `FindWindowSW` fails for a few seconds after a restart, see the doc of `shell_thread::desktop_registered`); `dir = shell_user_local_appdata(pid)` -> `Err(String)` becomes `Failed` through `From<String>`; `id = random_id()` -> same; `file = dir.join(layout_file_name(&id))`; `shell_thread::desktop_open(<system_dir()>\WindowsPowerShell\v1.0\powershell.exe, Some(&export_args(dir, file)), None)` -> on `Err` `Failed("cannot start the de-elevated export: ...")`; a `None` from `system_dir()` -> `Failed("no system directory")`. Then loop every `POLL_MS` ms on `poll_step(symlink_metadata(file).is_ok(), symlink_metadata(file + ".err").is_ok(), t.elapsed())` (`symlink_metadata` never follows links): `Ready` -> `crate::datadir::read_file(&file, MAX_JSON, true)` (already opens with `FILE_FLAG_OPEN_REPARSE_POINT` and rejects reparse points and multi-link files) then `String::from_utf8`; `Failed` -> `JobError::Failed`; `TimedOut` -> `JobError::TimedOut`. The elevated dock never writes, renames or deletes anything in that folder and never reads the `.err` content (existence only). The rest of `job()` (COM catalog, `map_layout`, `config::modify`) is unchanged.
  5. `popup_settings.rs` `import_label`: add `Status::TimedOut => tr("Import failed (timeout)", "Importazione non riuscita (timeout)").to_string()`; add `Status::TimedOut` to the status list of the test near line 1172.
  6. Rewrite the module doc of `start_import.rs` (lines 1-18) for the new protocol and delete the sentences about the data folder and the temp folder.

- [ ] **Step 5: Run the full suite**

Run: `DOCK_NO_MANIFEST=1 CARGO_TARGET_DIR=target-test cargo test --locked`
Expected: PASS (including the existing `layout_maps_ids_links_and_skips_the_rest`). `work_dir()` is now unused (dead-code warning): Task 2 deletes it.

- [ ] **Step 6: Owner tests live** (pure Win32, no automation): with the dock elevated, Settings -> Appearance -> Import from Windows Start imports the pins; Task Manager (Details, "Elevated" column) never shows an elevated `powershell.exe`; with Explorer stopped the row shows "Import failed" (not a hang); right after Explorer restarts (taskbar visible again but its desktop not yet registered with ShellWindows, a few seconds) the row also shows "Import failed", the log line says "Explorer's desktop is not available", and clicking again a few seconds later imports; no console window flashes; `%LOCALAPPDATA%\mini-oled-dock\start-layout-*.json` files older than one hour disappear at the next import.

- [ ] **Step 7: Commit**

```bash
git add Cargo.toml build.rs src/shell_thread.rs src/start_import.rs src/popup_settings.rs
git commit -m "fix(start-import): run Export-StartLayout de-elevated through the desktop shell" -m "The elevated dock no longer spawns powershell.exe (module hijack through the user-writable PSModulePath). The export runs as the interactive user, writes <LocalAppData of the shell user>\mini-oled-dock\start-layout-<128-bit id>.json atomically, and the dock reads it as untrusted input (no reparse points, 4 MiB cap, serde_json). A 15 s timeout reports 'Import failed (timeout)'." -m "Claude-Session: https://claude.ai/code/session_01FHR79Qfsfd7dTKsSjKFXxX"
```

---

### Task 2: Finding 2 - remove the %TEMP% fallback (`work_dir()`)

Severity medium. `src\start_import.rs` `work_dir()` (lines 111-118) returns `std::env::temp_dir()` when the data folder is unusable. After Task 1 the elevated process writes no file, so the function is dead; this task deletes it and pins the invariant with a test.

**Files:**
- Modify: `src\start_import.rs` (delete `work_dir`; drop imports that become unused)
- Test: `src\start_import.rs` `mod tests`

**Interfaces:**
- Consumes: Task 1's `job()` (no longer references `work_dir`).
- Produces: nothing; the module has no function that returns a location for the dock to write.

- [ ] **Step 1: Write the failing test**

```rust
#[test]
fn elevated_code_has_no_temp_folder_and_no_work_dir() {
    // Source guard for finding 2. The needles are built with concat! so this test does not match itself.
    let src = include_str!("start_import.rs");
    assert!(!src.contains(concat!("temp", "_dir()")), "no %TEMP% fallback");
    assert!(!src.contains(concat!("fn work", "_dir")), "no elevated work dir");
    assert!(!src.contains(concat!("remove", "_file(")), "the dock never deletes in the shell user's folder");
}
```

- [ ] **Step 2: Run it to verify it fails**

Run: `DOCK_NO_MANIFEST=1 CARGO_TARGET_DIR=target-test cargo test --locked elevated_code_has_no_temp_folder`
Expected: FAIL on the `fn work_dir` assertion.

- [ ] **Step 3: Delete `work_dir()`** and any import it alone used. Check with `grep -n "work_dir\|temp_dir\|remove_file" src/start_import.rs` that only the test needles remain.

- [ ] **Step 4: Run the full suite**

Run: `DOCK_NO_MANIFEST=1 CARGO_TARGET_DIR=target-test cargo test --locked`
Expected: PASS, no `dead_code` warning from `start_import.rs`.

- [ ] **Step 5: Commit**

```bash
git add src/start_import.rs
git commit -m "fix(start-import): drop the %TEMP% fallback of the layout file" -m "With the export running de-elevated the elevated process writes nothing, so work_dir() is gone and a source guard test keeps temp_dir out of the module." -m "Claude-Session: https://claude.ai/code/session_01FHR79Qfsfd7dTKsSjKFXxX"
```

---

### Task 3: Finding 3 - AppBar gate must reject a message that claims the dock's own pid

Severity medium. `src\tray_host.rs` `appbar_block_allowed` (line 2118) has the arm `Some(w) if block_pid == own => w != 0 && w != own && medium(w)`: `m.pid` (`AppBarMsg.pid`, offset 56, sender-controlled) equal to the dock's pid makes `SHLockShared(m.shared, m.pid)` (in `get_taskbar_pos`, line 2134, and `forward_with_shared`, line 2160) resolve a sender-chosen handle value in the dock's OWN handle table.

**Files:**
- Modify: `src\tray_host.rs` (`handle_appbar` 2082-2110 split into a pure gate and the Win32 call; `appbar_block_allowed` 2111-2124 including its doc comment; comment at 2087-2090; log at 2093-2099; test `appbar_block_gating` at 2436)
- Test: `src\tray_host.rs` `mod tests`

**Interfaces:**
- Produces: `fn appbar_block_allowed(block_pid: u32, own: u32, wnd_pid: Option<u32>, medium: impl Fn(u32) -> bool) -> bool` (same signature): any `block_pid == own` is now `false`.
- Produces: `fn appbar_gate(m: &AppBarMsg, wnd_pid: Option<u32>, own: u32, medium: impl Fn(u32) -> bool) -> Option<String>`: `None` = forward; `Some(text)` = reject, `text` being the exact log line (names `m.message`, the claimed `m.pid` as `block pid <n>`, the window pid or `-`, and `claims_own = <bool>`). It returns `None` for every message other than `ABM_GETTASKBARPOS | ABM_QUERYPOS | ABM_SETPOS` (those are not gated today either). `handle_appbar` becomes: build `m`, `let wnd_pid = ...`, `if let Some(text) = appbar_gate(&m, wnd_pid, GetCurrentProcessId(), pid_at_most_medium) { log!("{text}"); return LRESULT(0); }`, then the existing `match m.message`.

- [ ] **Step 1: Investigate before changing anything** (read-only; record the result in the commit body)
  1. `grep -n "SHAppBarMessage\|ABM_GETTASKBARPOS\|ABM_QUERYPOS\|ABM_SETPOS" src/*.rs`. Expected finding: the only in-process sender is `taskbar.rs` `appbar_shell` (line 244), used for `ABM_GETSTATE` / `ABM_SETSTATE` only, which `handle_appbar` does not gate (it forwards them with `forward_sync`). So the dock never sends itself the three gated messages and the spec's premise holds for the dock's own code.
  2. Known conflict, reported and not hidden: the comment above `appbar_block_allowed` and the test `appbar_block_gating` say that a REAL medium-integrity AppBar app arrives with `block_pid == own` (shell32 allocates the shared block for the tray window's process) and that the pre-1.3 rule rejected it. The decided fix (spec row 3) rejects exactly that case. Implement the decided fix, log every rejection with the claimed pid, and ask the owner in the pull request to run Step 6. If Step 6 shows a real AppBar app losing its reservation, STOP: do not weaken the gate; hand back to the owner with the alternative the finding itself proposes (authenticate the handle, i.e. prove the section was created for this message, instead of trusting `m.pid`).

- [ ] **Step 2: Write the failing tests** (the second replaces `appbar_block_gating`, which asserts the weakness; the first builds a real `AppBarMsg`, serialises it and goes through `appbar_gate` and `handle_appbar`)

```rust
fn crafted(message: u32, pid: u32) -> AppBarMsg {
    let mut m: AppBarMsg = from_prefix(&[]); // all zero
    m.message = message;
    m.shared = 0x1234; // a guessed handle value: must never be locked
    m.pid = pid;
    m
}

fn appbar_bytes(m: &AppBarMsg) -> Vec<u8> {
    // SAFETY: AppBarMsg is repr(C), Copy and made only of integers (explicit _pad fields, size asserted 0x40).
    unsafe { std::slice::from_raw_parts((m as *const AppBarMsg).cast::<u8>(), std::mem::size_of::<AppBarMsg>()) }.to_vec()
}

#[test]
fn crafted_appbar_message_with_the_dock_pid_is_rejected() {
    // SAFETY: no preconditions.
    let own = unsafe { GetCurrentProcessId() };
    let medium = |_: u32| true;
    for msg in [ABM_GETTASKBARPOS, ABM_QUERYPOS, ABM_SETPOS] {
        let m = crafted(msg, own);
        let why = appbar_gate(&m, None, own, medium).expect("a claimed own pid is rejected");
        assert!(why.contains(&format!("block pid {own}")) && why.contains("claims_own = true"), "{why}");
        // The real entry point answers 0 for the same bytes and never reaches SHLockShared (m.shared is garbage).
        assert_eq!(handle_appbar(WPARAM(0), LPARAM(0), &appbar_bytes(&m)).0, 0);
    }
    // A legitimate medium sender passes the gate; a message that is not gated is never rejected by it.
    assert!(appbar_gate(&crafted(ABM_SETPOS, 8120), Some(8120), own, medium).is_none());
    assert!(appbar_gate(&crafted(4 /* ABM_GETSTATE */, own), None, own, medium).is_none());
}

#[test]
fn appbar_gate_rejects_a_claimed_own_pid() {
    const OWN: u32 = 100;
    let medium = |p: u32| p != 0 && p != 666; // 666 = elevated process
    // Crafted message: block pid = the dock's pid, window of any medium process -> rejected.
    assert!(!appbar_block_allowed(OWN, OWN, Some(8120), medium));
    assert!(!appbar_block_allowed(OWN, OWN, Some(666), medium));
    assert!(!appbar_block_allowed(OWN, OWN, Some(OWN), |_| true));
    assert!(!appbar_block_allowed(OWN, OWN, None, |_| true), "no window + own pid is rejected too");
    // Unchanged: the block lives in the window owner's process and that process is at most medium.
    assert!(appbar_block_allowed(8120, OWN, Some(8120), medium));
    assert!(!appbar_block_allowed(666, OWN, Some(8120), medium));
    assert!(!appbar_block_allowed(666, OWN, Some(666), medium));
    assert!(!appbar_block_allowed(8120, OWN, Some(9000), medium), "block pid must be the window owner's");
    assert!(appbar_block_allowed(8120, OWN, None, medium));
    assert!(!appbar_block_allowed(0, OWN, None, medium));
}
```

- [ ] **Step 3: Run it to verify it fails**

Run: `DOCK_NO_MANIFEST=1 CARGO_TARGET_DIR=target-test cargo test --locked appbar_gate`, then `... cargo test --locked crafted_appbar_message`
Expected: FAIL (`crafted_appbar_message...` does not compile, `appbar_gate` is missing; `appbar_gate_rejects...` fails on `assert!(!appbar_block_allowed(OWN, OWN, Some(8120), medium))`).

- [ ] **Step 4: Implement**: first line of `appbar_block_allowed` is `if block_pid == own { return false; }`, then the two remaining arms (`None => medium(block_pid)`, `Some(w) => w == block_pid && medium(block_pid)`). Rewrite its doc comment and the comment at 2087-2090 (a claimed own pid is never trusted: it would resolve the handle in the dock's own table). Extract `appbar_gate` from `handle_appbar` as described under Interfaces; the rejection log text it returns (formerly lines 2093-2099) keeps its current wording and adds `claims_own = {m.pid == own}`, printed as `claims_own = true|false`.

- [ ] **Step 5: Run the full suite**

Run: `DOCK_NO_MANIFEST=1 CARGO_TARGET_DIR=target-test cargo test --locked`
Expected: PASS.

- [ ] **Step 6: Owner test live**: with a medium-integrity AppBar app running (a sidebar that reserves screen space), the dock keeps running, one `tray: AppBar ... rejected` line per attempt is in the log, no hang. Report whether the app lost its reservation (see Step 1.2).

- [ ] **Step 7: Commit**

```bash
git add src/tray_host.rs
git commit -m "fix(tray): reject AppBar messages that claim the dock's own pid" -m "A medium sender could set AppBarMsg.pid to the dock's pid so SHLockShared resolved a guessed handle in the elevated dock's own handle table. The dock never sends itself ABM_GETTASKBARPOS/QUERYPOS/SETPOS (taskbar.rs only sends GETSTATE/SETSTATE)." -m "Claude-Session: https://claude.ai/code/session_01FHR79Qfsfd7dTKsSjKFXxX"
```

---

### Task 4: Finding 4 - admin Start-menu tools launched by full path

Low. `src\shell_thread.rs` `AdminTool::exe()` (lines 110-115) returns bare `"wt.exe"` / `"powershell.exe"`; `launch_admin` (line 1505) passes them to `desktop_shell_execute(exe, None, None, Verb::RunAs)`. Explorer resolves bare names through HKCU App Paths, the user PATH and the WindowsApps alias folder. `Verb::RunAs` is built only in `launch_admin`, so this is the only `runas` entry.

**Files:**
- Modify: `src\shell_thread.rs` (`AdminTool` lines 102-116, `launch_admin` lines 1505-1511)
- Test: `src\shell_thread.rs` `mod tests` (starts at line 1676)

**Interfaces:**
- Consumes: `shell_thread::system_dir() -> Option<PathBuf>` (Task 1).
- Produces: `impl AdminTool { fn target(self, system_dir: &Path) -> String }` replacing `exe()`: `PowerShell` -> `<system_dir>\WindowsPowerShell\v1.0\powershell.exe`; `Terminal` -> `shell:AppsFolder\Microsoft.WindowsTerminal_8wekyb3d8bbwe!App` (the Store app keeps its AUMID, as in the spec).

- [ ] **Step 1: Write the failing test**

```rust
#[test]
fn admin_tools_launch_by_absolute_path_or_aumid() {
    let sys = Path::new(r"C:\Windows\System32");
    assert_eq!(AdminTool::PowerShell.target(sys), r"C:\Windows\System32\WindowsPowerShell\v1.0\powershell.exe");
    assert_eq!(AdminTool::Terminal.target(sys), r"shell:AppsFolder\Microsoft.WindowsTerminal_8wekyb3d8bbwe!App");
    for tool in [AdminTool::Terminal, AdminTool::PowerShell] {
        let s = tool.target(sys);
        assert!(s.contains('\\'), "never a bare executable name: {s}");
        assert!(!s.eq_ignore_ascii_case("wt.exe") && !s.eq_ignore_ascii_case("powershell.exe"));
    }
}
```

- [ ] **Step 2: Run to verify it fails**

Run: `DOCK_NO_MANIFEST=1 CARGO_TARGET_DIR=target-test cargo test --locked admin_tools_launch`
Expected: FAIL to compile (`target` not defined).

- [ ] **Step 3: Implement** `AdminTool::target` (delete `exe`). `launch_admin` becomes: `let Some(sys) = system_dir() else { log!("shell: LaunchAdmin: no system directory: REFUSED"); return };` then `desktop_shell_execute(&tool.target(&sys), None, None, Verb::RunAs)`; the log lines print the full target. Update the doc comments of `AdminTool` ("a constant resolved here") accordingly.

- [ ] **Step 4: Run the full suite**

Run: `DOCK_NO_MANIFEST=1 CARGO_TARGET_DIR=target-test cargo test --locked`
Expected: PASS.

- [ ] **Step 5: Owner test live**: Start quick-links menu, "PowerShell (admin)": the UAC prompt names `C:\Windows\System32\WindowsPowerShell\v1.0\powershell.exe`. "Terminal (admin)": the UAC prompt names Windows Terminal. If `runas` on the AUMID does not elevate on the owner's machine, remove the "Terminal (admin)" row from `ROWS` in `src\popup_start.rs` (line 51; the finding allows dropping an entry that cannot be verified) and say so in the commit.

- [ ] **Step 6: Commit**

```bash
git add src/shell_thread.rs
git commit -m "fix(shell): launch the admin Start-menu tools by absolute System32 path" -m "wt.exe and powershell.exe were resolved by Explorer through HKCU App Paths / PATH / WindowsApps. PowerShell now uses GetSystemDirectoryW; Terminal uses its AUMID." -m "Claude-Session: https://claude.ai/code/session_01FHR79Qfsfd7dTKsSjKFXxX"
```

---

### Task 5: Finding 5 - `tray_sync` must do nothing while Explorer is restarting

Low. `src\ui.rs` `tray_sync()` (lines 797-816) starts the tray host when `show_tray && !tray_host::is_started() && taskbar::is_hidden()`. During the pause after `taskbar::explorer_gone()` (taskbar.rs line 680) `is_started()` is false and `HIDDEN` is true, so a config apply (`on_config_changed`, ui.rs:746; reached from `WM_DOCK_CONFIG_APPLY`, pin/unpin, drag reorder, or the housekeeping timer when `config.json` changed) recreates a `Shell_TrayWnd` and Explorer never registers its desktop. The pause flag is `taskbar::HOST_PAUSED_AT` (private, line 61).

**Files:**
- Modify: `src\taskbar.rs` (add `host_paused`; `on_reannounce_timer`, lines 858-884)
- Modify: `src\ui.rs` (`tray_sync` becomes `pub(crate)`, decision extracted; tests in the existing `mod tests` at line 3318)
- Test: `src\ui.rs` `mod tests`

**Interfaces:**
- Produces:
  - `taskbar::host_paused() -> bool` (`HOST_PAUSED_AT != 0`).
  - `enum TraySync { Start, Stop, Nothing }` and `fn tray_sync_action(show_tray: bool, started: bool, hidden: bool, paused: bool) -> TraySync` in `ui.rs`.
  - `pub(crate) fn tray_sync()` (was private).
- Consumes: `tray_host::is_started()`, `taskbar::is_hidden()`.

- [ ] **Step 1: Write the failing test** (`ui.rs` `mod tests`)

```rust
#[test]
fn tray_sync_waits_for_explorer() {
    use TraySync::*;
    //                          show   started hidden paused
    assert_eq!(tray_sync_action(true,  false,  true,  false), Start);
    assert_eq!(tray_sync_action(true,  false,  true,  true),  Nothing, "Explorer restarting: no Shell_TrayWnd of ours");
    assert_eq!(tray_sync_action(true,  false,  false, false), Nothing, "native taskbar not hidden by us");
    assert_eq!(tray_sync_action(false, true,   true,  false), Stop);
    assert_eq!(tray_sync_action(false, true,   true,  true),  Stop, "switching off is always allowed");
    assert_eq!(tray_sync_action(false, false,  true,  false), Nothing);
    assert_eq!(tray_sync_action(true,  true,   true,  false), Nothing);
}
```

- [ ] **Step 2: Run to verify it fails**

Run: `DOCK_NO_MANIFEST=1 CARGO_TARGET_DIR=target-test cargo test --locked tray_sync_waits`
Expected: FAIL to compile (`TraySync` not defined).

- [ ] **Step 3: Implement** `TraySync`, `tray_sync_action` and `taskbar::host_paused()`. `tray_sync()` reads the three live values plus `taskbar::host_paused()`, calls `tray_sync_action` and keeps the existing Start / Stop bodies (including `suspend_mouse_hook` / `resume_mouse_hook` and `announce_tray`). In `taskbar::on_reannounce_timer`, in the branch that ends the pause (lines ~866-883), keep the `HOST_PAUSED_AT.store(0)` and the `if !HIDDEN { return }` guard and the log line, then replace the direct `crate::tray_host::start(...)` + `announce_tray()` by `crate::ui::tray_sync();` - the single sync the resume path owes; it reads the CURRENT `show_tray`. Remove the now-unused `started` match and mouse-hook handling from that branch (they live in `tray_sync`).

Task 6 rewrites `on_reannounce_timer` again (around `pause_step`) and restates this Resume branch: apply Task 5 first.

- [ ] **Step 4: Run the full suite**

Run: `DOCK_NO_MANIFEST=1 CARGO_TARGET_DIR=target-test cargo test --locked`
Expected: PASS.

- [ ] **Step 5: Owner test live**: `taskkill /f /im explorer.exe`, then within 1 s toggle any setting or pin an app: the log has no `tray host started` before `Explorer back after`; Explorer's tray returns and, after the pause, the dock's tray icons return. Repeat with the Tray icons setting (`show_tray`) switched OFF during the pause: the host does not come back.

- [ ] **Step 6: Commit**

```bash
git add src/taskbar.rs src/ui.rs
git commit -m "fix(tray): tray_sync does nothing while Explorer restarts, resume path syncs once" -m "A config apply during the Explorer-restart pause restarted the tray host and recreated a foreign Shell_TrayWnd. The resume path now calls tray_sync, so the current show_tray setting decides." -m "Claude-Session: https://claude.ai/code/session_01FHR79Qfsfd7dTKsSjKFXxX"
```

---

### Task 6: Finding 6 - exit during the Explorer-restart pause must cancel the pause

Low. While paused (`HOST_PAUSED_AT != 0`, Explorer's taskbar gone) `taskbar::restore_force` (line ~740) finds no `Shell_TrayWnd`, logs "restore postponed (state file kept)" and returns; the pause machinery (`TIMER_TRAY_REANNOUNCE`, `on_reannounce_timer`) may still start the host after exit began. Exit paths today: `main.rs` `WM_DOCK_QUIT_REQUEST` (line 376), `WM_HOTKEY` and `WM_CLOSE` (PostQuitMessage, then `cleanup` -> `taskbar::shutdown()`), `WM_ENDSESSION` with `wp != 0` (line 396, calls `taskbar::restore()`), `WM_DESTROY`. `WM_QUERYENDSESSION` is not handled today (DefWindowProc answers TRUE). The watchdog (`run_watchdog` in `watchdog.rs`; lines 375-392 are the `OpenEventW` / heartbeat setup, the exit-code logic is the `r == WAIT_OBJECT_0` branch at lines 415-435) already runs `restore()` after a clean exit when the state file exists; this task makes that decision a tested function.

**Files:**
- Modify: `src\taskbar.rs` (state machine, `begin_exit`, `on_reannounce_timer`, `explorer_restarted`)
- Modify: `src\main.rs` (`taskbar::begin_exit()` first in `WM_DOCK_QUIT_REQUEST`, in `WM_HOTKEY`'s exit branch, in `WM_CLOSE`, and in `WM_ENDSESSION` when `wp != 0`; new `WM_QUERYENDSESSION` arm)
- Modify: `src\watchdog.rs` (extract the post-exit decision)
- Test: `src\taskbar.rs` `mod tests` (line 918) and `src\watchdog.rs` (new `mod tests`)

**Interfaces:**
- Produces in `taskbar.rs`:
  - `enum PauseStep { Wait, Resume, GiveUp }` and `fn pause_step(paused_at: u64, now: u64, desktop_ready: bool, exiting: bool) -> PauseStep`: `exiting` -> `GiveUp`; else `!desktop_ready && now - paused_at < HOST_RESUME_MAX_MS` -> `Wait`; else `Resume`.
  - `pub fn begin_exit()`: `RESTORING.store(true)`, `HOST_PAUSED_AT.store(0)`, `KillTimer(TIMER_TRAY_REANNOUNCE)` on the dock window; idempotent, UI thread. Its doc comment must say: this is only the EARLY part of the exit. `taskbar::restore()` and `restore_force()` remain the exit path that puts the taskbar back (they already set `RESTORING` themselves, lines 729 and 744); `begin_exit` merely makes the exit visible sooner, so that `pause_step`, `hide_all` (which returns when `RESTORING` is set, lines 570 and 589) and `explorer_restarted` stop acting before `restore()` runs, and it cancels the pause and its timer, which `restore()` does not.
  - `on_reannounce_timer` (apply after Task 5, it rewrites the same function) branches on `pause_step(paused, now_ms(), desktop_registered(), RESTORING.load(Relaxed))` when `paused != 0`: `Wait` -> `arm_reannounce(1000)`; `GiveUp` -> `HOST_PAUSED_AT.store(0)` and return (no host start, no "starting our tray host again" log); `Resume` -> exactly Task 5's resume branch, restated so nothing is lost: `HOST_PAUSED_AT.store(0)`, the `if !HIDDEN.load(Relaxed) { return }` guard stays, the `taskbar: Explorer back after {waited} ms, starting our tray host again` log line stays, then `crate::ui::tray_sync()`. The non-paused tail (`if HIDDEN && tray_host::is_running() { announce_tray() }`) is unchanged. `explorer_restarted` skips `tray_host::raise()` / `arm_reannounce(300)` when `RESTORING` is set.
- Produces in `watchdog.rs`: `fn exit_needs_restore(exit_code: Option<u32>, state_file_exists: bool) -> bool` (`None` = `GetExitCodeProcess` failed -> true; `Some(0)` -> `state_file_exists`; `Some(n)`, `n != 0` -> true), used by `run_watchdog` in the `r == WAIT_OBJECT_0` branch.
- `WM_QUERYENDSESSION`: deliberately NOT given `begin_exit`. A shutdown can still be cancelled by another app after the query; setting `RESTORING` (never cleared) or dropping the pause there would leave a surviving dock without its tray host. Add an explicit `WM_QUERYENDSESSION => LRESULT(1)` arm in `main.rs` `wnd_proc` that only logs, so the decision is visible in code. This is the one deviation from the spec text (which lists the message); the reason goes in the commit body.

- [ ] **Step 1: Write the failing tests**

```rust
// taskbar.rs
#[test]
fn pause_state_machine() {
    use PauseStep::*;
    let max = HOST_RESUME_MAX_MS;
    assert_eq!(pause_step(1000, 1500, false, false), Wait, "desktop not registered yet");
    assert_eq!(pause_step(1000, 1500, true, false), Resume);
    assert_eq!(pause_step(1000, 1000 + max - 1, false, false), Wait);
    assert_eq!(pause_step(1000, 1000 + max, false, false), Resume, "stop waiting, start anyway");
    assert_eq!(pause_step(1000, 1500, true, true), GiveUp, "exit began: never start a host");
    assert_eq!(pause_step(1000, 1500, false, true), GiveUp);
}

// watchdog.rs (new mod tests)
#[test]
fn watchdog_restores_whenever_the_state_file_is_left() {
    assert!(exit_needs_restore(Some(0), true), "clean exit with Explorer absent: state file kept");
    assert!(!exit_needs_restore(Some(0), false));
    assert!(exit_needs_restore(Some(1), false));
    assert!(exit_needs_restore(Some(0xC000_0005), true));
    assert!(exit_needs_restore(None, false), "exit code unreadable: restore anyway");
}
```

- [ ] **Step 2: Run to verify they fail**

Run: `DOCK_NO_MANIFEST=1 CARGO_TARGET_DIR=target-test cargo test --locked pause_state_machine`, then `... cargo test --locked watchdog_restores`
Expected: FAIL to compile (functions not defined).

- [ ] **Step 3: Implement** `pause_step`, `begin_exit`, the `on_reannounce_timer` rewrite, `exit_needs_restore` and its use in `run_watchdog`, the `main.rs` calls and the `WM_QUERYENDSESSION` arm.

- [ ] **Step 4: Run the full suite**

Run: `DOCK_NO_MANIFEST=1 CARGO_TARGET_DIR=target-test cargo test --locked`
Expected: PASS.

- [ ] **Step 5: Owner test live**: `taskkill /f /im explorer.exe` and, within the pause, Ctrl+Alt+Q: the log has no `starting our tray host again` after the exit; when Explorer comes back the taskbar is not left in autohide (the watchdog restores it, or the next start does). Sign out during a pause: the next logon shows a normal taskbar.

- [ ] **Step 6: Commit**

```bash
git add src/taskbar.rs src/main.rs src/watchdog.rs
git commit -m "fix(taskbar): cancel the Explorer-restart pause when the dock exits" -m "begin_exit() clears the pause and its timer on every exit path so the tray host is never restarted mid-exit; the pause logic is a pure state machine and the watchdog's post-exit decision is a tested function. WM_QUERYENDSESSION is intentionally left non-destructive because a shutdown can still be cancelled." -m "Claude-Session: https://claude.ai/code/session_01FHR79Qfsfd7dTKsSjKFXxX"
```

---

### Task 7: Finding 7 - free the Start menu icon cache when the popup closes

Medium (memory). `src\popup_startmenu.rs`: the thread-local `STORE` (lines 179-181; `Store { apps, loaded, icons, icon_px }`, lines 168-177) lives for the whole process; the only clears are a larger `icon_px` (line 221) and the test reset in `preview_new` (line 557). The shell thread also remembers `start_sent` (`shell_thread.rs` line 354), so without a matching reset a reopen would not resend any icon. The review's line 175 is stale; use the lines above.

Decision on structure: the spec says the cache "lives in the popup instance". Events reach it through free functions (`popup::on_start_apps` / `on_start_icons`, popup.rs lines 620-634), so the store stays a UI-thread `thread_local` that is populated only while a Start menu is open and is emptied in `after_close()` (line 1184): same observable behaviour, nothing held while the menu is closed.

**Files:**
- Modify: `src\popup_startmenu.rs` (`Store.open`, `release_store`, `after_close`, `store_apps`, `store_icons`, `request_catalog`, `preview_new`; module doc lines 36-40)
- Modify: `src\shell_thread.rs` (new `ShellRequest::StartRelease`, free function `start_release`, arm in `handle_request` near line 585)
- Test: `src\popup_startmenu.rs` `mod tests` (line 1197), `src\shell_thread.rs` `mod tests`

**Interfaces:**
- Produces:
  - `Store` gains `open: bool`; `fn release_store() -> usize` returns the bytes of icon bitmaps freed (`width * height * 4` each) and resets the store to `Store::default()`; it logs `startmenu: freed {n} KB of icons ({k} apps)` with `log!` at the same level `trim(hidden)` uses.
  - `store_apps` / `store_icons` drop their data (and `store_icons` returns false) when `!open`; `request_catalog` and `preview_new` set `open = true`.
  - `ShellRequest::StartRelease` (no fields) and `fn start_release(start_px: &mut u32, sent: &mut HashSet<String>, pending: &mut VecDeque<ShellRequest>)`: sets `*start_px = 0`, clears `sent`, removes queued `StartIcons` batches (this also stops icon extraction for a closed menu).
  - `after_close` first statements: `release_store(); shell_thread::request(ShellRequest::StartRelease);`, before the pending launch / power action handling.
- Consumes: `IconBitmap { width, height, pixels }` and `IconBitmap::new(w, h)` (`icons.rs` lines 64-75).

- [ ] **Step 1: Write the failing tests**

```rust
// popup_startmenu.rs
#[test]
fn closing_frees_the_store_and_late_events_are_dropped() {
    let apps = vec![
        ("a".to_string(), "Alpha".to_string(), IconBitmap::new(32, 32)),
        ("b".to_string(), "Beta".to_string(), IconBitmap::new(32, 32)),
    ];
    drop(preview_new(apps, "", StartStyle::Vertical, false, false));
    assert_eq!(release_store(), 2 * 32 * 32 * 4);
    STORE.with(|s| {
        let s = s.borrow();
        assert!(s.apps.is_empty() && s.icons.is_empty() && !s.loaded && !s.open);
    });
    store_apps(vec![StartApp { id: "x".into(), name: "X".into() }]);
    assert!(!store_icons(32, vec![("x".into(), Some(IconBitmap::new(32, 32)))]), "closed menu ignores late icons");
    STORE.with(|s| assert!(s.borrow().apps.is_empty()));
}

// shell_thread.rs
#[test]
fn start_release_makes_a_reopen_resend_everything() {
    let mut px = 48u32;
    let mut sent: HashSet<String> = ["a".to_string(), "b".to_string()].into();
    let mut pending: VecDeque<ShellRequest> = VecDeque::new();
    pending.push_back(ShellRequest::StartIcons { icon_px: 48, ids: vec!["c".into()] });
    pending.push_back(ShellRequest::VolumeRefresh);
    start_release(&mut px, &mut sent, &mut pending);
    assert_eq!((px, sent.len(), pending.len()), (0, 0, 1));
    assert!(matches!(pending[0], ShellRequest::VolumeRefresh));
}
```

- [ ] **Step 2: Run to verify they fail**

Run: `DOCK_NO_MANIFEST=1 CARGO_TARGET_DIR=target-test cargo test --locked closing_frees`, then `... cargo test --locked start_release_makes`
Expected: FAIL to compile (`release_store`, `start_release`, `Store.open` not defined).

- [ ] **Step 3: Implement** the interfaces. The `Shell` handler for `StartRelease` calls `start_release(&mut self.start_px, &mut self.start_sent, &mut self.pending)`.

- [ ] **Step 4: Run the full suite, then the preview recipe**

Run: `DOCK_NO_MANIFEST=1 CARGO_TARGET_DIR=target-test cargo test --locked` -> PASS. Then the **Preview baseline recipe** (Global Constraints): `Compare-Object` prints nothing (the preview path never calls `after_close`).

- [ ] **Step 5: Owner test live**: open the Start menu (icons load) and close it; the dock's private bytes (About memory line or Task Manager) drop by about 1-5 MB after the next idle trim. Reopen: the icons reappear within about a second.

- [ ] **Step 6: Commit**

```bash
git add src/popup_startmenu.rs src/shell_thread.rs
git commit -m "fix(startmenu): free the icon cache when the Start menu closes" -m "The catalog and icons are emptied in after_close() and ignored while no Start menu is open; the shell thread forgets what it already sent (StartRelease) so a reopen repopulates the menu. Debug log of the freed size." -m "Claude-Session: https://claude.ai/code/session_01FHR79Qfsfd7dTKsSjKFXxX"
```

---

### Task 8: Finding 8 - keyboard hook watchdog must repair the hook when `WIN_DOWN` is stale

Low. `src\hook.rs` `reinstall()` (lines 180-193) returns whenever `WIN_DOWN` is true. After Win+L the Win release goes to the secure desktop, `WIN_DOWN` and `WIN_OTHER` stay true (the stale reset in `other_key_down`, lines 290-296, never runs while `WIN_OTHER` is true), and if Windows then drops the LL hook the 2 s watchdog (`TIMER_HOUSEKEEPING`, main.rs line 277) skips every reinstall. `hook.rs` has no tests today: this task adds `mod tests`.

**Files:**
- Modify: `src\hook.rs` (`reinstall`; import `GetAsyncKeyState`)
- Test: `src\hook.rs` (new `#[cfg(test)] mod tests`)

**Interfaces:**
- Produces: `enum Repair { Go, ClearThenGo, Skip }` and `fn repair_plan(win_down: bool, lwin_pressed: bool, rwin_pressed: bool) -> Repair`: not `win_down` -> `Go`; `win_down` and either key physically down -> `Skip`; `win_down` and both up -> `ClearThenGo`.
- Consumes: `GetAsyncKeyState(VK_LWIN.0 as i32) < 0` (high bit = down; `Win32_UI_Input_KeyboardAndMouse` is already enabled).

- [ ] **Step 1: Write the failing test**

```rust
#[test]
fn watchdog_repairs_a_stale_win_state() {
    use Repair::*;
    assert_eq!(repair_plan(false, false, false), Go);
    assert_eq!(repair_plan(false, true, false), Go, "not tracked as held: nothing to protect");
    assert_eq!(repair_plan(true, true, false), Skip, "Win really held: do not touch the hook mid-combination");
    assert_eq!(repair_plan(true, false, true), Skip);
    assert_eq!(repair_plan(true, false, false), ClearThenGo, "Win+L left WIN_DOWN stuck");
}
```

- [ ] **Step 2: Run to verify it fails**

Run: `DOCK_NO_MANIFEST=1 CARGO_TARGET_DIR=target-test cargo test --locked watchdog_repairs`
Expected: FAIL to compile (`Repair` not defined).

- [ ] **Step 3: Implement** `repair_plan`; in `reinstall()` after the `HOOK_RAW == 0` check read `GetAsyncKeyState` for `VK_LWIN` / `VK_RWIN` and match: `Skip` -> return; `ClearThenGo` -> `WIN_DOWN.store(false)`, `WIN_OTHER.store(false)`, `WIN_DOUBLE.store(false)`, `LAST_ALONE_UP.store(0)`, `log!("hook watchdog: stale Win state cleared")`; then the existing reinstall body. Update the doc comment of `reinstall`.

- [ ] **Step 4: Run the full suite**

Run: `DOCK_NO_MANIFEST=1 CARGO_TARGET_DIR=target-test cargo test --locked`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add src/hook.rs
git commit -m "fix(hook): the watchdog clears a stale Win state before repairing the hook" -m "reinstall() skipped forever while WIN_DOWN was stuck after Win+L. It now asks GetAsyncKeyState for VK_LWIN/VK_RWIN and clears the stale flags when both are up." -m "Claude-Session: https://claude.ai/code/session_01FHR79Qfsfd7dTKsSjKFXxX"
```

---

### Task 9: Finding 9 - clamp popups to the monitor work area

Low. The Start menu is a fixed 520x600 (`W`, `H`, lines 65-66) or 640x620 (`HW`, `HH`, lines 95-96) logical px; `popup::compute_pos` (popup.rs line 827) only clamps the position and, when the popup is taller than the work area, pins it to the top so the bottom bar and the end of the list fall off screen. Fix: one shared helper in `popup.rs` caps every popup; the two popups that have a long list lay themselves out inside the cap and scroll: the Start menu (rows dropped, app list scrolls) and the tray chooser (it already scrolls with the wheel, `MAX_ROWS = 14`, which at 200 % is 14 x 68 + 114 = 1066 px, taller than a 1080p work area).

**Scope against spec row 9 ("lists scroll"), stated so nobody discovers it later.** The other popups are not made scrollable here. The calendar (about 300 logical px) and the network, power, notification and app popups are small and fixed. The audio popup is the one unbounded case (its Output, Spatial audio and Mixer lists grow with devices and sessions, `popup_audio.rs` `size()` lines 640-735) and the mouse wheel there already means "change the volume", so a scrolling list would need a design decision. For all of these the shared helper guarantees only that the popup is never larger than the work area minus the margins: when its content is taller, the bottom is cut off (every `Surface` drawing primitive is bounds-checked, `surface.rs` line 698, so painting past the clipped DIB is safe) and the user can collapse the audio sections with the pencil button. This deviation goes in the pull request text; a scrolling audio list is a follow-up for the owner to commission.

**Files:**
- Modify: `src\popup.rs` (`fit_to_work`, `max_size`, `Content::limit` at line ~264, `render()` lines 960-985; new `#[cfg(test)] mod tests`)
- Modify: `src\popup_tray.rs` (`chooser_rows`, `TrayPopup.max_h` / `visible`, `size` lines 134-168, `wheel` lines 276-288, the struct literals at lines 100, 117 and 332)
- Modify: `src\popup_startmenu.rs` (`layout`, `layout_h`, `Geo.page_size`, the `H_PAGE` uses at lines 345-350, 629, 635; `StartMenu.max`)
- Test: `src\popup.rs` (new `mod tests`), `src\popup_tray.rs` `mod tests`, `src\popup_startmenu.rs` `mod tests` (existing layout tests at lines 1217-1290 and 1371 get the new trailing argument)

**Interfaces:**
- Produces in `popup.rs`:
  - `pub(crate) fn fit_to_work(w: i32, h: i32, work: &RECT, margin: i32) -> (i32, i32)`: `(w.min(work_w - 2*margin).max(1), h.min(work_h - 2*margin).max(1))`.
  - `pub(crate) fn max_size(a: &Anchor) -> (i32, i32)`: `work_area(&a.bar)` minus `2 * Metrics { dpi: a.dpi }.px(POPUP_SCREEN_MARGIN)` on each axis.
  - `Content::limit(&mut self, _max: (i32, i32)) {}` (new trait method, default no-op).
  - In `render()`: `o.content.limit(max_size(&o.anchor))` before `o.content.size(&surf)`, then `fit_to_work` on the returned size for every popup (a content without `limit` is clipped at the work-area edge, never larger than the work area; see the scope note).
- Produces in `popup_tray.rs`:
  - `fn chooser_rows(max_h: i32, fixed_h: i32, row_h: i32) -> usize` = `((max_h - fixed_h) / row_h.max(1)).clamp(1, MAX_ROWS as i32) as usize` (`fixed_h` = everything in the Customize view except the rows: `2 * pad + 2 * m.px(4) + m.px(1) + m.px(32)`).
  - `TrayPopup` gains `max_h: i32` (default `i32::MAX`, set by `limit(max) { self.max_h = max.1 }`) and `visible: usize` (default `MAX_ROWS`, set in `size()` from `chooser_rows`); `size()` (lines 144-145) and `wheel()` (line 277) use `self.visible` instead of `MAX_ROWS`.
- Produces in `popup_startmenu.rs`:
  - `pub(crate) fn layout(m: Metrics, pins: usize, max: (i32, i32)) -> Geo` and `pub(crate) fn layout_h(m: Metrics, pins: usize, page: usize, list: bool, button_w: Option<i32>, max: (i32, i32)) -> Geo`; `const UNCAPPED: (i32, i32) = (i32::MAX, i32::MAX)` for the existing tests; panel `w = m.px(W).min(max.0)`, `h = m.px(H).min(max.1)` (same for `HW` / `HH`).
  - Vertical rule: shown pin rows = the largest `r` in `0..=MAX_PIN_ROWS` that leaves at least `MIN_LIST_ROWS = 3` (new const) list rows. Horizontal rule: tile rows `r` = the largest in `1..=H_ROWS` that fits in `body`; `Geo.page_size = H_COLS * r` (new field; `H_PAGE` stays the uncapped value); every use of `H_PAGE` as a page size reads `self.geo.page_size`.
  - `StartMenu.max: (i32, i32)` (default `UNCAPPED`) set by `limit` and passed by `refresh()` (lines 602 and 608).

- [ ] **Step 1: Write the failing tests**

```rust
// popup.rs
#[test]
fn popup_is_clamped_to_the_work_area() {
    let work = RECT { left: 0, top: 0, right: 1920, bottom: 1040 };
    assert_eq!(fit_to_work(1000, 600, &work, 16), (1000, 600), "fits: unchanged");
    assert_eq!(fit_to_work(1280, 1240, &work, 16), (1280, 1008), "1080p at 200 %: 640x620 logical");
    assert_eq!(fit_to_work(5000, 5000, &work, 16), (1888, 1008));
    assert_eq!(fit_to_work(50, 50, &RECT { left: 0, top: 0, right: 40, bottom: 40 }, 16), (8, 8), "40 - 2 * 16");
    assert_eq!(fit_to_work(10, 10, &RECT { left: 0, top: 0, right: 20, bottom: 20 }, 16), (1, 1), "20 - 32 < 0: never zero or negative");
    assert_eq!(fit_to_work(10, 10, &RECT { left: 0, top: 0, right: 10, bottom: 10 }, 16), (1, 1), "never zero");
}

// popup_tray.rs
#[test]
fn tray_chooser_rows_follow_the_cap() {
    // 200 %: row 68, fixed part 16 + 18 + 64 + 16 = 114, 1080p work area minus margins 1008.
    assert_eq!(chooser_rows(1008, 114, 68), 13);
    assert_eq!(chooser_rows(i32::MAX, 114, 68), MAX_ROWS, "uncapped: today's 14 rows");
    assert_eq!(chooser_rows(100, 114, 68), 1, "never zero");
}

// popup_startmenu.rs
#[test]
fn start_menu_fits_a_short_work_area() {
    let m = Metrics { dpi: 192 }; // 200 %
    let max = (m.px(W) + 100, 1008); // 1080p work area minus margins
    let g = layout(m, 12, max);
    assert!(g.h <= max.1 && g.list.bottom <= g.h && g.rows >= 3, "{g:?}");
    assert!(g.tiles.iter().all(|t| t.bottom <= g.list_header.top));
    let h = layout_h(m, 20, 0, false, Some(90), max);
    assert!(h.h <= max.1);
    let bar = h.bar.unwrap();
    assert!(bar.bottom <= h.h && h.grid.bottom <= bar.top);
    assert!(h.page_size >= H_COLS && h.page_size <= H_PAGE && h.tiles.len() == h.page_size.min(20));
    // Uncapped results are unchanged.
    assert_eq!(layout(Metrics { dpi: 96 }, 12, UNCAPPED).h, H);
    assert_eq!(layout_h(Metrics { dpi: 96 }, 18, 0, false, Some(90), UNCAPPED).page_size, H_PAGE);
}
```

- [ ] **Step 2: Run to verify they fail**

Run: `DOCK_NO_MANIFEST=1 CARGO_TARGET_DIR=target-test cargo test --locked popup_is_clamped`, then `... cargo test --locked tray_chooser_rows`, then `... cargo test --locked start_menu_fits`
Expected: FAIL to compile.

- [ ] **Step 3: Implement** the interfaces; add the trailing `UNCAPPED` to every existing `layout(...)` / `layout_h(...)` call in the `popup_startmenu.rs` tests (lines 1221-1223, 1235, 1243, 1252-1253, 1257, 1271, 1283, 1301, 1371).

- [ ] **Step 4: Run the full suite, then the preview recipe**

Run: `DOCK_NO_MANIFEST=1 CARGO_TARGET_DIR=target-test cargo test --locked` -> PASS. **Preview baseline recipe**: `Compare-Object` prints nothing (preview sizes are far below any work area, so the clamp must be a no-op there).

- [ ] **Step 5: Owner test live**: at 1920x1080 and 200 % open the horizontal Start menu: the search box and the bottom bar (user name, power button) are on screen and clickable; the pinned page shows fewer rows; the app list scrolls. Open the tray overflow in Customize mode with many icons: it shows 13 rows instead of 14, the wheel scrolls the rest, "Done" is on screen. Open the calendar: it is whole. Open the audio popup with several apps in its Mixer: it never extends past the work area, but its bottom is cut off (known, see the scope note; the pencil button collapses sections to bring the link row back). Report whether that audio cut-off is acceptable or needs the follow-up.

- [ ] **Step 6: Commit**

```bash
git add src/popup.rs src/popup_tray.rs src/popup_startmenu.rs
git commit -m "fix(popup): clamp popups to the work area, Start menu lays out inside the cap" -m "One shared helper (popup::fit_to_work) caps every popup to the monitor work area minus the margins; the Start menu layouts and the tray chooser take the cap, drop rows as needed and keep scrolling. The audio popup and the calendar are only cut at the work-area edge (no scrolling). Uncapped layouts and all preview images are unchanged." -m "Claude-Session: https://claude.ai/code/session_01FHR79Qfsfd7dTKsSjKFXxX"
```

---

### Task 10: Finding 10 - close the Start menu when it cannot get keyboard focus

Low. `src\popup.rs` `on_focus_retry` (lines 1357-1388) retries `ui::force_foreground` `FOCUS_RETRIES` times (6, line 369; about 0.5 s at `FOCUS_RETRY_MS = 80`, line 367) and then gives up silently; `on_tick` (line 1420) only closes when the foreground differs from `fg_at_open`, so over a fullscreen game that keeps the foreground the Start menu stays open unfocused. The spec writes "(3)" retries; the constant is 6 and is shared by every popup, so it is left as it is (changing it would alter the other popups' timing): say so in the pull request.

**Files:**
- Modify: `src\popup.rs` (`on_focus_retry`)
- Test: `src\popup.rs` `mod tests` (created by Task 9, which must land first)

**Interfaces:**
- Produces: `fn focus_giveup(kind: PopupKind, tries_left: u8, focused: bool) -> bool`: true only for `PopupKind::StartMenu` with `tries_left == 0` and `!focused`.
- Consumes: `PopupKind` (popup.rs line 81), `close_inner(reason: &str)`, `ui::force_foreground(hwnd) -> bool`.

- [ ] **Step 1: Write the failing test**

```rust
#[test]
fn start_menu_never_stays_open_unfocused() {
    assert!(focus_giveup(PopupKind::StartMenu, 0, false));
    assert!(!focus_giveup(PopupKind::StartMenu, 0, true));
    assert!(!focus_giveup(PopupKind::StartMenu, 2, false), "retries left: keep trying");
    assert!(!focus_giveup(PopupKind::Power, 0, false), "only the Start menu needs the keyboard");
}
```

- [ ] **Step 2: Run to verify it fails**

Run: `DOCK_NO_MANIFEST=1 CARGO_TARGET_DIR=target-test cargo test --locked start_menu_never_stays`
Expected: FAIL to compile.

- [ ] **Step 3: Implement** `focus_giveup`. In `on_focus_retry`, read `o.kind` in the existing `with(...)` block together with `left`; after `let ok = crate::ui::force_foreground(hwnd);` and the log line compute `let focused = ok || GetForegroundWindow() == hwnd;` and `if focus_giveup(kind, left, focused) { log!("popup: Start menu has no keyboard focus after the last retry, closing"); close_inner("no keyboard focus"); return; }`. Keep the re-arm branch for `left > 0`.

- [ ] **Step 4: Run the full suite**

Run: `DOCK_NO_MANIFEST=1 CARGO_TARGET_DIR=target-test cargo test --locked`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add src/popup.rs
git commit -m "fix(popup): close the Start menu when keyboard focus cannot be obtained" -m "After the last foreground retry fails the Start menu is closed and the reason is logged; it is never left open unfocused over a fullscreen game. The retry count (6) is shared by all popups and is unchanged." -m "Claude-Session: https://claude.ai/code/session_01FHR79Qfsfd7dTKsSjKFXxX"
```

---

### Task 11: Finding 11 - tray host: the WinEvent callback only posts, the host thread moves the window asynchronously

Low. `src\tray_host.rs` `place_winevent_proc` (lines 1190-1240) is an out-of-context `SetWinEventHook` callback (installed in `place_arm`, line 1131, on the host thread) that calls `SetWindowPos` on another process's window (line 1233) synchronously: a hung app blocks the thread that also serves `Shell_NotifyIcon` and AppBar requests for every other app. Note for the implementer: out-of-context callbacks are delivered on the installing thread, so posting alone does not unblock it; the move must also stop being synchronous. Both are done (the spec's post-a-message structure plus `SWP_ASYNCWINDOWPOS`, the flag the dock already uses for cross-process calls in `taskbar.rs` lines 596 (`hide_all`) and 771 (`restore_force`), and in `tray_host.rs` lines 1377 and 1422).

**Files:**
- Modify: `src\tray_host.rs` (new `WM_HOST_PLACE_MOVE`, the callback, `host_wnd_proc` arm near line 1669)
- Test: `src\tray_host.rs` `mod tests` (line 2284)

**Interfaces:**
- Produces:
  - `const WM_HOST_PLACE_MOVE: u32 = WM_APP + 5;` (next free after `WM_HOST_PLACE_ARM = WM_APP + 4`, line 1075; `grep -n "WM_APP + 5" src/tray_host.rs` currently finds nothing).
  - `fn place_should_handle(p: &Placement, event: u32, raw_hwnd: isize, pid_matches: bool, now: u64) -> bool`: false when `now > p.until_ms`, or `!pid_matches`, or (for `EVENT_OBJECT_LOCATIONCHANGE`) `raw_hwnd != p.placed || p.moves >= PLACE_MAX_MOVES`, or (for the other events) `p.placed != 0`.
  - The callback keeps only the null-hwnd and `id_object` / `id_child` filtering and then `PostMessageW(host, WM_HOST_PLACE_MOVE, WPARAM(hwnd), LPARAM(event))`; the host thread's `WM_HOST_PLACE_MOVE` arm runs the former body (placement lock, `place_should_handle`, `is_menu_candidate`, `place_target`) and calls `SetWindowPos(.., SWP_NOSIZE | SWP_NOZORDER | SWP_NOACTIVATE | SWP_ASYNCWINDOWPOS)`.

- [ ] **Step 1: Write the failing test**

```rust
#[test]
fn placement_events_are_filtered_before_any_window_call() {
    use windows::Win32::UI::WindowsAndMessaging::{EVENT_OBJECT_LOCATIONCHANGE as LOC, EVENT_OBJECT_SHOW as SHOW};
    let rc = RECT { left: 0, top: 0, right: 10, bottom: 10 };
    let p = Placement { pid: 7, item: rc, bar: rc, top: false, until_ms: 5_000, placed: 0, moves: 0 };
    assert!(place_should_handle(&p, SHOW, 0x10, true, 1_000));
    assert!(!place_should_handle(&p, SHOW, 0x10, true, 5_001), "armed window expired");
    assert!(!place_should_handle(&p, SHOW, 0x10, false, 1_000), "other process");
    assert!(!place_should_handle(&p, LOC, 0x10, true, 1_000), "location change before any placement");
    let placed = Placement { placed: 0x10, moves: 1, ..p };
    assert!(place_should_handle(&placed, LOC, 0x10, true, 1_000));
    assert!(!place_should_handle(&placed, LOC, 0x20, true, 1_000), "a different window");
    assert!(!place_should_handle(&placed, SHOW, 0x20, true, 1_000), "only the first menu is taken");
    assert!(!place_should_handle(&Placement { moves: PLACE_MAX_MOVES, ..placed }, LOC, 0x10, true, 1_000));
}
```

- [ ] **Step 2: Run to verify it fails**

Run: `DOCK_NO_MANIFEST=1 CARGO_TARGET_DIR=target-test cargo test --locked placement_events_are_filtered`
Expected: FAIL to compile.

- [ ] **Step 3: Implement** as in Interfaces. `EVENT_SYSTEM_MENUPOPUPSTART` keeps its present semantics (no `id_object` filter).

- [ ] **Step 4: Run the full suite**

Run: `DOCK_NO_MANIFEST=1 CARGO_TARGET_DIR=target-test cargo test --locked`
Expected: PASS.

- [ ] **Step 5: Owner test live**: right click a tray icon of an app with a context menu: the menu still appears centred on the icon above the bar, as before. Suspend the app's UI thread right after the click (Process Explorer "Suspend"): the other tray icons keep updating.

- [ ] **Step 6: Commit**

```bash
git add src/tray_host.rs
git commit -m "fix(tray): menu placement no longer blocks the tray host thread" -m "The WinEvent callback only posts WM_HOST_PLACE_MOVE; the host thread filters (pure place_should_handle) and moves the foreign window with SWP_ASYNCWINDOWPOS, so a hung app cannot stall Shell_NotifyIcon and AppBar handling." -m "Claude-Session: https://claude.ai/code/session_01FHR79Qfsfd7dTKsSjKFXxX"
```

---

### Task 12: Finding 12 - `--install` waits long enough for the old instance and its watchdog

Low (unchanged since 1.0.0). `src\autostart.rs` `copy_self` (lines 233-267; its doc comment starts at line 230 and the stale "waits up to 3 s" is line 231) retries the copy for `STOP_WAIT = 8 s` (line 39) after signalling the stop event; the old `dock.exe --watchdog` child holds the exe image and, if the state file is left behind, waits up to `RESTORE_WAIT = 30 s` (`watchdog.rs` line 59) for Explorer. On timeout `copy_self` relaunches the OLD exe and returns `Err` before `register_task`.

**The old watchdog must be detectable without its cooperation.** The case that matters is updating FROM 1.3.0 or 1.0.0, whose watchdog never creates any named object a new build could look for (a new named mutex would only exist for watchdogs started by the new build, so it would be useless for exactly this update). So the check is by process image: any process other than the installer itself whose image file is the destination `dock.exe` (the running dock, or its `--watchdog` child, of any version) means "still in use". The installer runs from the staged copy or from another path, so it never matches itself; the pid is excluded anyway.

**Files:**
- Modify: `src\autostart.rs` (`STOP_WAIT`, `copy_self` loop and doc comment, new `keep_waiting` and `exe_in_use_by_other`)
- Modify: `src\watchdog.rs` (`RESTORE_WAIT` at line 59 becomes `pub(crate)`; nothing else, no new mutex)
- Test: `src\autostart.rs` `mod tests` (line 380)

**Interfaces:**
- Produces:
  - `const STOP_WAIT: Duration = Duration::from_secs(35);` and `const STOP_GRACE: Duration = Duration::from_secs(3);`.
  - `fn keep_waiting(elapsed: Duration, instance_up: bool, exe_in_use: bool) -> bool`: `elapsed >= STOP_WAIT` -> false; `elapsed < STOP_GRACE` -> true; else `instance_up || exe_in_use` (both gone and the copy still fails: something else holds the file, stop waiting and report).
  - `fn exe_in_use_by_other(dest: &Path) -> bool`: `EnumProcesses` (feature `Win32_System_ProcessStatus`, already enabled; grow the pid buffer until the returned size is smaller than the buffer), skip `GetCurrentProcessId()`, for each pid take `crate::apps::exe_path_of_pid(pid)` (`pub(crate)`, `apps.rs` line 710; `None` when the process cannot be opened is skipped) and compare with `same_file(&Path::new(&path), dest)` (`autostart.rs` line 223, canonicalising and case-insensitive).
- Consumes: `watchdog::instance_running() -> bool`, `apps::exe_path_of_pid`.
- Known limit, stated in the doc comment of `exe_in_use_by_other`: a process whose image path cannot be queried (a protected process) is invisible to it; such a holder is not a dock process, and the copy then fails after `STOP_GRACE` with the "locked by something else" error.

- [ ] **Step 1: Write the failing tests**

```rust
#[test]
fn install_waits_longer_than_the_watchdog_restore() {
    assert!(STOP_WAIT >= crate::watchdog::RESTORE_WAIT + Duration::from_secs(5));
    assert_eq!(STOP_WAIT, Duration::from_secs(35));
}

#[test]
fn keep_waiting_while_the_old_instance_or_watchdog_lives() {
    let s = Duration::from_secs;
    assert!(keep_waiting(s(0), false, false), "grace right after the stop signal");
    assert!(!keep_waiting(s(10), false, false), "both gone and the copy still fails: report, do not wait");
    assert!(keep_waiting(s(10), true, false));
    assert!(keep_waiting(s(34), false, true), "watchdog still waiting for Explorer");
    assert!(!keep_waiting(s(35), true, true), "hard limit");
}

#[test]
fn an_old_watchdog_is_seen_by_its_image_not_by_a_mutex() {
    // A 1.3.0 watchdog is just a process running the installed exe; any running exe stands in for it.
    let ping = crate::shell_thread::system_dir().expect("system dir").join("ping.exe");
    let mut child = std::process::Command::new(&ping)
        .args(["-n", "8", "127.0.0.1"])
        .stdout(std::process::Stdio::null())
        .spawn()
        .expect("spawn ping.exe");
    let seen = exe_in_use_by_other(&ping);
    let _ = child.kill();
    let _ = child.wait();
    assert!(seen, "a process running the destination exe is detected");
    assert!(!exe_in_use_by_other(&std::env::current_exe().unwrap()), "the installer itself does not count");
    assert!(!exe_in_use_by_other(Path::new(r"C:\no\such\dock.exe")));
}
```

- [ ] **Step 2: Run to verify they fail**

Run: `DOCK_NO_MANIFEST=1 CARGO_TARGET_DIR=target-test cargo test --locked install_waits`, then `... cargo test --locked keep_waiting`, then `... cargo test --locked an_old_watchdog`
Expected: FAIL to compile (`keep_waiting`, `exe_in_use_by_other`, `RESTORE_WAIT` visibility).

- [ ] **Step 3: Implement.** In `copy_self` replace `while t.elapsed() < STOP_WAIT` by `while keep_waiting(t.elapsed(), watchdog::instance_running(), exe_in_use_by_other(dest))`; the final error text distinguishes "the old instance or its watchdog did not exit within 35 s" from "the file is locked by something else". The relaunch-the-old-exe behaviour on failure is unchanged. Fix the stale doc comment of `copy_self` (line 231, "waits up to 3 s").

- [ ] **Step 4: Run the full suite**

Run: `DOCK_NO_MANIFEST=1 CARGO_TARGET_DIR=target-test cargo test --locked`
Expected: PASS.

- [ ] **Step 5: Owner test live** (the case that matters is updating FROM an old build): install the 1.3.0 exe and run it, `taskkill /f /im explorer.exe`, then run `dock.exe --install` from the new build: it waits (up to 35 s) while the OLD watchdog holds the exe; start Explorer (`explorer.exe`) and the install completes and the dock relaunches from the new copy. Repeat once with the 1.0.0 exe. With a normal Explorer the install still finishes in about 2 s.

- [ ] **Step 6: Commit**

```bash
git add src/autostart.rs src/watchdog.rs
git commit -m "fix(install): wait 35 s for the old instance and its watchdog before copying" -m "STOP_WAIT outlasts the watchdog's 30 s Explorer wait; --install also knows when nothing of the old instance is left by looking for any other process running the destination exe, which works for the watchdog of every earlier version (1.0.0 and 1.3.0 create no named object), and stops waiting early then." -m "Claude-Session: https://claude.ai/code/session_01FHR79Qfsfd7dTKsSjKFXxX"
```

---

### Task 13: Release notes - Fixes section and the two defaults for users coming from 1.0.0

Docs only. The 7 refuted findings are not touched; the spec asks that the release notes mention the two defaults that changed between 1.0.0 and 1.1.0 (`show_start_button` and `window_previews`, both `true` in `Config::default()`, `config.rs` lines 509 and 493).

**Files:**
- Create: `.github\release-notes\v1.4.0.md` (existing files in that folder: `v1.1.0.md` ... `v1.3.0.md`; follow `v1.3.0.md`'s style)
- Test: none (docs); verification is the `grep` in Step 2.

**Interfaces:**
- Produces: a `### Fixes` section that later plans (updater, battery module, Settings window) append to. The file carries no version-bump instructions; the `Cargo.toml` bump to `1.4.0` belongs to the release step, not to this plan.

- [ ] **Step 1: Create `.github\release-notes\v1.4.0.md`** with, in this order:
  - one intro line: `Changes since [1.3.0](https://github.com/ops-software/mini-oled-dock/releases/tag/v1.3.0): security and stability fixes from a full review of the code added since 1.0.0.` (later plans extend it);
  - `### Fixes` with one bullet per fixed finding in plain user language, 12 bullets, each starting with the bold symptom: Import from Windows Start no longer runs PowerShell with administrator rights; the import no longer touches the temp folder; AppBar messages cannot target the dock's own handles; "Terminal (admin)" and "PowerShell (admin)" launch by absolute path; no tray host restart while Explorer restarts; the taskbar is restored when the dock exits during an Explorer restart; Start menu icons are freed when it closes; the Win key hook is repaired after Win+L; the Start menu and popups fit small or high-DPI screens; the Start menu closes if it cannot get keyboard focus; tray menu placement can no longer stall the tray; `--install` waits for the old watchdog;
  - a `### If you are updating from 1.0.0` sub-section with the two refuted-findings lines: ``- **Start button on by default.** Since 1.1.0 a Start button is shown at the left of the bar (the Start button setting, or `"show_start_button": false` in `config.json`). A config file that does not mention it gets the new default.`` and ``- **Window previews on by default.** Since 1.1.0, resting the pointer on an app with several windows shows a live preview of each one (the Window previews setting, or `"window_previews": false`).``;
  - the standard `### Install / update` block copied from `v1.3.0.md` unchanged.

- [ ] **Step 2: Verify**

Run: `grep -c "^- \*\*" .github/release-notes/v1.4.0.md` -> at least 14; `grep -n "show_start_button\|window_previews" .github/release-notes/v1.4.0.md` -> both keys present.

- [ ] **Step 3: Commit**

```bash
git add .github/release-notes/v1.4.0.md
git commit -m "docs(release): v1.4.0 notes - review fixes and the 1.1.0 defaults" -m "Claude-Session: https://claude.ai/code/session_01FHR79Qfsfd7dTKsSjKFXxX"
```

---

## Self-review notes

- Spec coverage: findings 1-12 map to Tasks 1-12 in the spec's order (finding 2 is its own Task 2: Task 1 is the protocol, Task 2 the removal plus its guard); the two refuted-finding lines are Task 13. Parts 2-4 of the spec are out of scope for this plan.
- Spec points the plan could not follow literally, each recorded in its task: row 3 rejects a claimed own pid although the code documents that real AppBar traffic uses it (Task 3, with a stop condition); row 6 lists `WM_QUERYENDSESSION` but cancelling a pause there is unsafe because a shutdown can be cancelled (Task 6); row 9 says "lists scroll" for popups generally but only the Start menu and the tray chooser scroll, the audio popup and the calendar are only clipped at the work-area edge (Task 9 scope note); row 10 says 3 retries but `FOCUS_RETRIES` is 6 and shared (Task 10); row 11 says the callback only posts, but it already runs on the host thread, so the move is also made asynchronous (Task 11); row 1's PowerShell command gains `-WindowStyle Hidden` (Task 1; the `export_args` test therefore asserts the spec's command plus that one flag); row 5 names `taskbar::explorer_restarting()` "or equivalent" and the plan calls it `taskbar::host_paused()` (Task 5); row 12 waits for the old watchdog by process image, not by a mutex, because earlier versions create none (Task 12).
- Cross-task names: `system_dir()` is produced by Task 1 and consumed by Task 4; `tray_sync_action` / `tray_sync` / `host_paused` (Task 5) are consumed by Task 6's `on_reannounce_timer`; the `popup.rs` `mod tests` is created by Task 9 and extended by Task 10, so Task 9 must land before Task 10.
