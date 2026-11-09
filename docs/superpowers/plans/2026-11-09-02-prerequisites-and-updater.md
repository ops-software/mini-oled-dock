# Installer Prerequisites and Self-Updater Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Make `dock.exe --install` a complete, silent, atomic installer (version resource, `--version`, `--quiet`, exit codes, always-relaunch, Add/Remove Programs entry, real uninstall), then build a self-updater in `dock.exe`. It checks GitHub Releases, downloads and verifies a new `dock.exe` into an admin-only staging folder, and hands it to that exe's `--install --quiet`.

**Architecture:** `autostart.rs` stays the only installer. It gains an atomic copy (`dock.exe.new`, then `MoveFileExW`), a 35 s stop wait (`stop_and_wait`) and pure relaunch and exit-code rules. The wait ends only when the dock's single-instance mutex is gone **and** no other process runs the installed `dock.exe` (the dock and its `--watchdog` child, found by image path). `--uninstall --remove` uses the same wait. `arp.rs` writes the Uninstall registry key. The updater has four parts. `update_release.rs` holds the pure logic: semver, release JSON, `.sha256` files, URLs, scheduling. `winhttp.rs` is a blocking HTTPS GET. `sha256.rs` is CNG. `update.rs` holds the state, the on-demand worker threads, the timer and the install handoff. `datadir.rs` owns every file operation in `%ProgramData%\mini-oled-dock\update\<tag>\`. The Settings window (a later plan) reads `update::status()` and calls `update::check_now()`, `update::install_now()` and `update::open_release_page()`. This plan adds the badge to the power popup's Settings row.

**Tech Stack:** Rust 1.87+, `windows` 0.62 (WinHTTP, CNG, Registry), `serde_json`, `winresource` (build-dependency, version resource), the existing `embed-manifest`.

**Spec:** `docs/superpowers/specs/2026-11-09-settings-window-updater-fixes-design.md`, Part 2 ("installer prerequisites and self-updater"), plus the Part 1 row 12 (`STOP_WAIT`) it depends on. Background: `E:\Giovik\Documenti\Dock\docs\review-2026-11-08-installer-readiness.md`.

**Relation to plan 01 (fixes):** finding 12 (`STOP_WAIT` 35 s + watchdog wait) is implemented here in Task 3. If plan 01 already changed `STOP_WAIT`, keep the 35 s value and this plan's `stop_and_wait`. Plan 01's fix 1 may already add the `Win32_Security_Cryptography` feature (for `BCryptGenRandom`). If it has, Task 9 only adds `bcrypt.dll` to `DELAY_LOAD` when it is missing.

**Version bump:** no task in this plan changes `version` in `Cargo.toml`; it stays `1.3.0` on the branch. The bump to `1.4.0` is the spec's release step (Constraints: "bump `version` in `Cargo.toml` to `1.4.0`, add `.github/release-notes/v1.4.0.md`, tag `v1.4.0` after merge"), and plan 01 Task 13 says the same ("the `Cargo.toml` bump to `1.4.0` belongs to the release step"). So every version this plan expects from a build is `1.3.0`. A manual check that needs a newer exe builds one with a local, uncommitted bump (Task 14 Step 5).

**Deferred to the Settings-window plan** (spec Part 2 *UI* and Part 3; no task here): the Settings → Updates page (status card, Check now / Release notes / Update now, the `update_check` / `update_auto_install` toggles, the last-action line), the badge on the Updates nav entry, and the `settings_window::is_open()` term of the auto-install gate. This plan provides what they call: `update::status()`, `UpdateStatus::line()` / `badge()`, `update::check_now()`, `update::install_now()`, `update::open_release_page()`, `popup_power::settings_badge`, `render::ATTENTION_RGB`, and the `TIMER_HOUSEKEEPING` expression of Task 15 that the window plan extends.

## Global Constraints

- Rust stable 1.87+, crate `windows` 0.62, no UI framework, no HTTP crate: WinHTTP through `Win32_Networking_WinHttp`, SHA-256 through CNG `Win32_Security_Cryptography`. Both DLLs (`winhttp.dll`, `bcrypt.dll`) go in `DELAY_LOAD` in `build.rs`.
- The dock runs elevated. Files it writes go only in admin-only folders (`%ProgramData%\mini-oled-dock`, `%ProgramFiles%\mini-oled-dock`), never in `%TEMP%` or `%LOCALAPPDATA%`. Everything it reads from user-writable places (including `config.json`) is untrusted.
- Nothing runs with the elevated token from user-controlled input. The only exe the updater ever starts is `%ProgramData%\mini-oled-dock\update\<canonical tag>\dock.exe`, after `datadir` has checked the folder (admin-only, no reparse point) and the file (no reparse point, single link, final path inside that folder).
- The running dock is stopped only through the `Local\DockStop` event (`watchdog::signal_stop_to_instance`). Never `taskkill`/`TerminateProcess` it: a non-zero exit leaves the taskbar hidden until the watchdog restores it.
- Memory: nothing on the UI thread blocks (all network, hashing, child processes and file I/O run on throwaway threads named `update`, `update-cleanup`). The updater adds exactly one dock-window timer (`TIMER_UPDATE_CHECK`). The power popup polls the badge in its existing 1 s `tick`.
- Defaults: `update_check` = `true`, `update_auto_install` = `false`, `last_update_check` = `""`, `update_available` = `""`. No other default changes.
- Repository for releases: `ops-software/mini-oled-dock` (hard-coded constant `update_release::REPO`; `Cargo.toml` `repository` still says `gitgiovik` and is corrected in Task 1).
- `--preview <dir>` keeps rendering every current PNG byte-identically. This plan adds one new preview, `popup_power_update.png` (and `popup_power_update_96.png`, `preview.rs:46` `DPIS`).
- English code, comments, UI strings, release notes. Italian UI labels through `crate::i18n::tr(en, it)` (it returns `&'static str`; compose variable parts with `format!`).
- Every `unsafe` block carries a `// SAFETY:` comment, as in the rest of the codebase.
- Tests: `DOCK_NO_MANIFEST=1 CARGO_TARGET_DIR=target-test cargo test --locked [filter]` (PowerShell: `$env:DOCK_NO_MANIFEST='1'; $env:CARGO_TARGET_DIR='target-test'; cargo test --locked [filter]`). Before each commit: `cargo fmt`. The release build `cargo build --release --locked` must stay green.
- Preview renders: build with `DOCK_NO_MANIFEST=1` (use `CARGO_TARGET_DIR=target-preview`) and run `target-preview\release\dock.exe --preview <dir>` (or `debug`).
- Work on branch `release/1.4.0`. Commit messages in English, each ending with the line `Claude-Session: https://claude.ai/code/session_01FHR79Qfsfd7dTKsSjKFXxX` (pass it as a second `-m`).
- Unit tests never call `datadir::init()`, so every `datadir` path is `None`/`Err` in tests. Code that touches the data folder is tested through its pure parts only.

## Review Focus

1. **The `.sha256` asset as `release.yml` really writes it** (`Out-File -Encoding ascii`: `<hex>  dock.exe` + CRLF, possibly a BOM or uppercase hex if the workflow changes) must verify. A reasonable person expects an update not to fail on line endings. The test goes in Task 8 (`sha256_file_formats`).
2. **GitHub answers something other than a release**: rate limit 403/429, a captive portal returning HTML with status 200, a 404 on a fork without releases. A person expects "could not check", never "up to date", and the badge kept. The tests go in Task 8 (`release_json_rejects_hostile_input`, HTML case) and Task 12 (`check_results_update_the_status`, 403 keeps the badge).
3. **A hand-edited `config.json`** with `update_available: "v99.0.0"`, `"../v9.9.9"` or garbage. A person expects at most a badge for a valid newer tag, and never an install of anything that has not been freshly fetched and verified. The tests go in Task 12 (`status_from_config_trusts_only_newer_tags`) and Task 14 (`install_target_needs_a_fresh_newer_release`).
4. **`--install` while the old dock just crashed** and its watchdog holds `dock.exe` for up to 30 s while it restores the taskbar. The stop event is missing then. A person expects the installer to wait up to 35 s until neither the dock's mutex nor any process running the installed `dock.exe` (the watchdog) is left, then replace it, not fail after 8 s. The tests go in Task 3 (`retry_for_waits_and_gives_up`, `stop_wait_covers_watchdog_restore`, `image_holders_finds_a_running_image`).
5. **Leftover staging folders**: an install that failed, the folder of the version now running (its installer child may still be alive), a hand-made higher one, and a name that is not a canonical tag (`v1.04.0`, `notes`, `..`). A person expects cleanup to delete only canonical tags at or below the running version, never to follow or misuse an odd name, and to shrug off a folder it cannot delete yet. The tests go in Task 5 (`plain_dir_names`) and Task 8 (`stale_stage_folders`).

---

## File Structure

| File | Responsibility |
|---|---|
| `build.rs` | Manifest (unchanged), **version resource** (winresource), `DELAY_LOAD` += `winhttp.dll`, `bcrypt.dll` |
| `Cargo.toml` | `winresource` build-dep, features `Win32_Networking_WinHttp`, `Win32_Security_Cryptography`, `repository` URL |
| `src/main.rs` | CLI parsing (`parse_args_from`), new flags, `--quiet` result handling (`cli_report`), `parse_error_exit`, `--version`, `--update-check`, `--update-install`, purge after the logger stops, wiring of the update timer/messages |
| `src/autostart.rs` | Atomic install, stop/wait (`STOP_WAIT` 35 s, `retry_for`, `image_holders`, `stop_and_wait`), relaunch rules, exit codes, `--uninstall [--remove]` |
| `src/arp.rs` (new) | `HKLM\...\Uninstall\mini-oled-dock` values, write and delete |
| `src/datadir.rs` | `update\` and `update\<tag>\` staging folders (create, list, read, write, checked path, remove) and `purge()` |
| `src/update_release.rs` (new) | Pure: `Version`, tags, URLs, host allow-list, release JSON, `.sha256`, `--version` output, stale stages, RFC 3339, check schedule |
| `src/winhttp.rs` (new) | Blocking HTTPS GET with timeouts, size cap, final URL |
| `src/sha256.rs` (new) | `sha256()` through CNG `BCryptHash` |
| `src/update.rs` (new) | `UpdateStatus`, worker threads, check, stage + verify, install handoff, startup cleanup, timer, auto-install gate, CLI entry points |
| `src/watchdog.rs` | `spawn_breakaway()` extracted from `launch_watchdog` (reused by the installer handoff) |
| `src/msg.rs` | `WM_DOCK_UPDATE = WM_APP + 17`, `TIMER_UPDATE_CHECK = 13` |
| `src/config.rs` | Keys `update_check`, `update_auto_install`, `last_update_check`, `update_available` |
| `src/ui.rs` | `foreground_is_fullscreen()` |
| `src/popup_power.rs`, `src/render.rs`, `src/preview.rs` | Badge on the Settings row, `ATTENTION_RGB` made `pub(crate)`, `popup_power_update.png` |
| `README.md`, `.github/release-notes/v1.4.0.md` | Install / Update / Uninstall / Configuration / Data folder docs, "Updates" release-note section |

---

### Task 1: Version resource and `--version`

**Files:**
- Modify: `Cargo.toml:1-12` (`repository`), `Cargo.toml:75-76` (`[build-dependencies]`)
- Modify: `build.rs:24-49`
- Modify: `src/main.rs:136-167` (`Args`), `src/main.rs:176-202` (`parse_args`), `src/main.rs:854-875` (`main`, before `--preview`)
- Test: `src/main.rs` (new `#[cfg(test)] mod tests` at the end)

**Interfaces:**
- Produces: `fn version_line() -> String` in `main.rs` (`"mini-oled-dock <CARGO_PKG_VERSION>"`); `Args::version: bool`. The output of `dock.exe --version` is that line plus a newline on stdout, exit 0. Task 13 compares it.

- [ ] **Step 1: Write the failing test** (in a new `mod tests` at the end of `src/main.rs`)

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn version_line_matches_cargo() {
        assert_eq!(version_line(), format!("mini-oled-dock {}", env!("CARGO_PKG_VERSION")));
    }
}
```

- [ ] **Step 2: Run it.** `DOCK_NO_MANIFEST=1 CARGO_TARGET_DIR=target-test cargo test --locked version_line`. Expected: FAIL to compile, `version_line` not found.

- [ ] **Step 3: Implement**
  - `Cargo.toml`: `repository = "https://github.com/ops-software/mini-oled-dock"`. Add `winresource = "0.1.19"` under `[build-dependencies]`, or the newest 0.1.x published at least two weeks before you add it. Then run `cargo check` once **without** `--locked`, so `Cargo.lock` records the new dependency, and commit `Cargo.lock`.
  - `build.rs`: inside the `CARGO_CFG_WINDOWS` block, after `embed_manifest`, build a `winresource::WindowsResource::new()` with `ProductName` = `"mini-oled-dock"`, `FileDescription` = `"mini-oled-dock"` (Task Manager shows FileDescription as the process name, so not the long Cargo description), `LegalCopyright` = `"MIT"`, `OriginalFilename` = `"dock.exe"`, `InternalName` = `"dock"`. FileVersion and ProductVersion come from `CARGO_PKG_VERSION` (winresource's default). Call `.compile()`. On `Err`, print `cargo:warning=cannot embed the version resource: {e}`, the same policy as the manifest. **Never call `set_manifest*`.** On MSVC, embed-manifest adds the manifest through `/MANIFEST:EMBED /MANIFESTINPUT` (checked in embed-manifest 1.5.1 `src/embed/mod.rs:93-99`), so a winresource `.res` without RT_MANIFEST links next to it. A second manifest would be a duplicate resource. Add `println!("cargo:rerun-if-changed=Cargo.toml");`.
  - `main.rs`: `fn version_line() -> String`. Add `Args::version` (doc comment `--version: print "mini-oled-dock X.Y.Z" and exit 0`) and the arm `"--version" => a.version = true`. At the very top of `main`, after `ARGS.get_or_init`: `if args.version { attach_console(); println!("{}", version_line()); return ExitCode::SUCCESS; }`. This runs before `--preview`, without the data folder, the logger or the mutex. When stdout is a pipe (Task 13) `attach_console` keeps the valid pipe handle.

- [ ] **Step 4: Run it.** Same command. Expected: PASS.

- [ ] **Step 5: Verify the binary.**
  - Run `cargo build --release --locked`.
  - Run `(Get-Item target\release\dock.exe).VersionInfo | Format-List FileVersion,ProductVersion,ProductName,FileDescription,LegalCopyright`. Expected: `1.3.0.0` / `1.3.0` (this plan does not bump the version, see *Version bump*), `mini-oled-dock`, `mini-oled-dock`, `MIT`.
  - Run `findstr /m /c:"requireAdministrator" target\release\dock.exe`. Expected: the file name is printed, so the manifest is still embedded.
  - Run `target\release\dock.exe --version | Write-Output` from an elevated terminal. Expected: `mini-oled-dock 1.3.0`.

- [ ] **Step 6: Commit**

```bash
git add Cargo.toml Cargo.lock build.rs src/main.rs
git commit -m "build: embed a version resource, add --version" -m "Claude-Session: https://claude.ai/code/session_01FHR79Qfsfd7dTKsSjKFXxX"
```

---

### Task 2: CLI flags, exit codes and `--quiet`

**Files:**
- Modify: `src/main.rs:136-167` (`Args`), `176-202` (`parse_args`), `831-841` (`usage`), `854-912` (`main`)
- Modify: `src/autostart.rs:36-41` (exit-code constants)
- Test: `src/main.rs` `mod tests`

**Interfaces:**
- Consumes: `Args::version` (Task 1).
- Produces:
  - `Args` fields `quiet: bool`, `no_relaunch: bool`, `remove: bool`, `purge_data: bool`, `update_check: bool`, `update_install: Option<String>`.
  - `fn parse_args_from(it: impl Iterator<Item = String>) -> Result<Args, String>`. `parse_args()` becomes `parse_args_from(std::env::args().skip(1))`.
  - In `autostart.rs`: `pub const EXIT_OK: i32 = 0; pub const EXIT_FAILED: i32 = 1; pub const EXIT_BAD_ARGS: i32 = 2; pub const EXIT_STOP_FAILED: i32 = 3;`.
  - In `main.rs`: `#[derive(Clone, Copy, Debug, PartialEq, Eq)] enum CliReport { Console, MessageBox, LogOnly }` and `fn cli_report(quiet: bool, has_console: bool) -> CliReport` (`LogOnly` whenever `quiet`, else `Console` / `MessageBox`). It is the only decision that leads to `MessageBoxW` in the install/uninstall block.
  - In `main.rs`: `fn parse_error_exit(msg: &str) -> i32`: `EXIT_OK` for the empty message of `--help`, else `EXIT_BAD_ARGS`.
  - Task 3 calls `autostart::run_install_cli(no_relaunch: bool)` and Task 6 calls `autostart::run_uninstall_cli(remove: bool)`. Until those tasks land, keep the zero-argument calls.

- [ ] **Step 1: Write the failing tests**

```rust
fn p(a: &[&str]) -> Result<Args, String> {
    parse_args_from(a.iter().map(|s| s.to_string()))
}

#[test]
fn cli_new_flags() {
    let a = p(&["--install", "--quiet", "--no-relaunch"]).unwrap();
    assert!(a.install && a.quiet && a.no_relaunch);
    let u = p(&["--uninstall", "--remove", "--purge-data", "--quiet"]).unwrap();
    assert!(u.uninstall && u.remove && u.purge_data && u.quiet);
    assert_eq!(p(&["--update-install", "v1.4.1"]).unwrap().update_install.as_deref(), Some("v1.4.1"));
    assert!(p(&["--update-check"]).unwrap().update_check);
    assert!(p(&["--version"]).unwrap().version);
}

#[test]
fn cli_rejects_bad_combinations() {
    let bad: [&[&str]; 8] = [
        &["--quiet"],
        &["--no-relaunch"],
        &["--remove"],
        &["--uninstall", "--purge-data"],
        &["--install", "--remove"],
        &["--install", "--uninstall"],
        &["--uninstall", "--no-relaunch"],
        &["--update-install"],
    ];
    for b in bad {
        assert!(matches!(p(b), Err(e) if !e.is_empty()), "{b:?}");
    }
}

#[test]
fn cli_report_quiet_never_shows_a_box() {
    assert_eq!(cli_report(true, false), CliReport::LogOnly); // --quiet from UAC / a service: no box
    assert_eq!(cli_report(true, true), CliReport::LogOnly);
    assert_eq!(cli_report(false, true), CliReport::Console);
    assert_eq!(cli_report(false, false), CliReport::MessageBox); // today's behaviour without --quiet
}

#[test]
fn cli_exit_codes() {
    use crate::autostart::{EXIT_BAD_ARGS, EXIT_FAILED, EXIT_OK, EXIT_STOP_FAILED};
    assert_eq!((EXIT_OK, EXIT_FAILED, EXIT_BAD_ARGS, EXIT_STOP_FAILED), (0, 1, 2, 3));
    assert_eq!(parse_error_exit(""), EXIT_OK); // --help
    assert_eq!(parse_error_exit(&p(&["--quiet"]).unwrap_err()), EXIT_BAD_ARGS);
    assert_eq!(parse_error_exit(&p(&["--bogus"]).unwrap_err()), EXIT_BAD_ARGS);
}
```

The exit codes of `run_install_cli` / `run_uninstall_cli` themselves are pinned by `install_report_exit_codes` (Task 3) and `uninstall_report_exit_codes` (Task 6): both functions return their report's `exit_code()`.

- [ ] **Step 2: Run them.** `... cargo test --locked cli_`. Expected: FAIL to compile, `parse_args_from` and the new fields are missing.

- [ ] **Step 3: Implement**
  - Parse arms: `--quiet`, `--no-relaunch`, `--remove`, `--purge-data`, `--update-check`, and `--update-install <tag>`, which errors with `"--update-install requires <tag>"` when the tag is missing.
  - After the loop, validate:
    - `--install` and `--uninstall` are mutually exclusive.
    - `--quiet` requires `--install` or `--uninstall`.
    - `--no-relaunch` requires `--install`.
    - `--remove` requires `--uninstall`.
    - `--purge-data` requires `--remove`.
  - Each rule returns a non-empty English error. In `main`, the parse-error arm returns `ExitCode::from(parse_error_exit(&msg) as u8)` instead of today's inline `if msg.is_empty() { 0 } else { 2 }`.
  - `usage()` lists `[--install [--quiet] [--no-relaunch]] [--uninstall [--remove [--purge-data]] [--quiet]] [--version] [--update-check] [--update-install <tag>]`.
  - `main` install/uninstall block (`main.rs:887-912`). Today it calls `take_say_buffer()` once, after `logger::stop`, and shows it with `MessageBoxW` when `!has_console`. With `--quiet` that path must never run:
    - `let report = cli_report(args.quiet, !args.quiet && attach_console());`, so `attach_console` is not even called with `--quiet`.
    - Unless `report == CliReport::Console`, call `logger::buffer_say()` (as today without a console).
    - `LogOnly`: after the CLI function returns and **before** `logger::stop`, drain `logger::take_say_buffer()` and `log!("{line}")` each line, then call `logger::buffer_say()` again, so that a later `say` (the `--purge-data` result of Task 6, after `logger::stop`) stays in a buffer nobody shows. The exit code carries that result.
    - Replace today's `if !has_console { ... MessageBoxW ... }` with `if report == CliReport::MessageBox { ... }`. This is the only `MessageBoxW` call in the block.
    - Without `--quiet`, behaviour is unchanged.

- [ ] **Step 4: Run them.** Same filter. Expected: PASS. Also run the whole suite: `... cargo test --locked`. Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add src/main.rs src/autostart.rs
git commit -m "cli: add --quiet, --no-relaunch, --remove, --purge-data, update flags and exit codes" -m "Claude-Session: https://claude.ai/code/session_01FHR79Qfsfd7dTKsSjKFXxX"
```

---

### Task 3: Atomic install, 35 s stop wait, relaunch rules

**Files:**
- Modify: `src/autostart.rs:37-41` (`STOP_WAIT`), `223-279` (`same_file`, `copy_self`, `enable`), `317-358` (`relaunch`, `run_install_cli`), tests `379-395`
- Modify: `src/popup_power.rs:247` (caller of `enable(false)`)
- Modify: `src/main.rs` install call → `autostart::run_install_cli(args.no_relaunch)`

**Interfaces:**
- Consumes:
  - `EXIT_*` (Task 2).
  - `watchdog::signal_stop_to_instance() -> bool` (`watchdog.rs:331`): signals `Local\DockStop`; `false` when no running dock created the event.
  - `watchdog::instance_running() -> bool` (`watchdog.rs:317`): `OpenMutexW` of the dock's `Local\DockInstance` mutex. It sees **only the dock**: the watchdog never takes that mutex (module doc, `watchdog.rs:19-21`). The watchdog is observed through `image_holders` below.
  - `apps::exe_path_of_pid(pid: u32) -> Option<String>` (`apps.rs:710`, `QueryFullProcessImageNameW` with `PROCESS_QUERY_LIMITED_INFORMATION`).
- Produces:

```rust
const STOP_WAIT: Duration = Duration::from_secs(35);
const RETRY_STEP: Duration = Duration::from_millis(250);
/// After the last holder exited: time for the kernel to release the image section.
const RELEASE_GRACE: Duration = Duration::from_secs(5);
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum CopyOutcome { AlreadyInstalled, Copied, Failed }
#[derive(Debug)]
pub(crate) enum InstallError { Failed(String), CouldNotStop(String) }   // + Display (the message)
impl InstallError { pub(crate) fn exit_code(&self) -> i32 }               // Failed = 1, CouldNotStop = 3
pub(crate) struct InstallReport { pub copy: CopyOutcome, pub stop_signalled: bool, pub error: Option<InstallError> }
impl InstallReport { pub(crate) fn exit_code(&self) -> i32 }              // error.map_or(EXIT_OK, |e| e.exit_code())
fn staging_path(dest: &Path) -> PathBuf                                    // <dir>\dock.exe.new
fn replace_file(src: &Path, dest: &Path) -> std::io::Result<()>            // MoveFileExW(REPLACE_EXISTING | WRITE_THROUGH)
pub(crate) fn retry_for<E>(max: Duration, step: Duration, op: impl FnMut() -> Result<(), E>) -> Result<u32, E>
pub(crate) fn image_holders(exe: &Path) -> Vec<u32>                       // pids other than this process whose image path == exe
pub(crate) fn stop_and_wait(dest: &Path) -> Result<bool, InstallError>    // Ok(stop_signalled); Err(CouldNotStop) after STOP_WAIT
pub(crate) fn should_relaunch(still_running: bool, no_relaunch: bool) -> bool
fn copy_self(dest: &Path, allow_stop: bool) -> InstallReport
pub fn enable(allow_stop: bool) -> InstallReport
pub fn run_install_cli(no_relaunch: bool) -> i32
```

- [ ] **Step 1: Write the failing tests** (in `autostart.rs` `mod tests`)

```rust
#[test]
fn relaunch_rules() {
    // Spec: --install always relaunches, unless --no-relaunch. The one exception is a dock still
    // running at the end (the installed exe re-run over its own running dock, or a dock that could
    // not be stopped). A failed copy with no dock running starts the installed copy, if there is one.
    assert!(should_relaunch(false, false));
    assert!(!should_relaunch(true, false));
    assert!(!should_relaunch(false, true), "--no-relaunch wins");
    assert!(!should_relaunch(true, true));
}

#[test]
fn retry_for_waits_and_gives_up() {
    let mut n = 0;
    let r = retry_for(Duration::from_secs(1), Duration::from_millis(1), || {
        n += 1;
        if n < 3 { Err(()) } else { Ok(()) }
    });
    assert_eq!(r, Ok(3));
    let t = Instant::now();
    assert_eq!(retry_for(Duration::from_millis(30), Duration::from_millis(5), || Err::<(), _>("held")), Err("held"));
    assert!(t.elapsed() >= Duration::from_millis(30));
}

#[test]
fn stop_wait_covers_watchdog_restore() {
    // The watchdog may wait up to 30 s for Shell_TrayWnd before it restores and exits.
    assert!(STOP_WAIT >= Duration::from_secs(35));
    assert_eq!(
        staging_path(Path::new(r"C:\Program Files\mini-oled-dock\dock.exe")),
        PathBuf::from(r"C:\Program Files\mini-oled-dock\dock.exe.new")
    );
    assert_eq!(InstallError::Failed("x".into()).exit_code(), EXIT_FAILED);
    assert_eq!(InstallError::CouldNotStop("x".into()).exit_code(), EXIT_STOP_FAILED);
}

#[test]
fn image_holders_finds_a_running_image() {
    // The child stands in for the watchdog: a process that runs a given exe and holds no mutex.
    let ping = PathBuf::from(std::env::var_os("SystemRoot").unwrap()).join("System32").join("PING.EXE");
    let mut child = std::process::Command::new(&ping)
        .args(["-n", "5", "127.0.0.1"])
        .stdout(std::process::Stdio::null())
        .spawn()
        .unwrap();
    let found = image_holders(&ping).contains(&child.id());
    let _ = child.kill();
    let _ = child.wait();
    assert!(found);
    assert!(image_holders(&std::env::current_exe().unwrap()).is_empty()); // this process is never counted
}

#[test]
fn install_report_exit_codes() {
    let r = |copy, error| InstallReport { copy, stop_signalled: false, error };
    assert_eq!(r(CopyOutcome::Copied, None).exit_code(), EXIT_OK);
    assert_eq!(r(CopyOutcome::AlreadyInstalled, None).exit_code(), EXIT_OK);
    // Copied, but schtasks or the ARP key failed: the new exe is relaunched and the exit code is 1.
    assert_eq!(r(CopyOutcome::Copied, Some(InstallError::Failed("schtasks".into()))).exit_code(), EXIT_FAILED);
    assert_eq!(r(CopyOutcome::Failed, Some(InstallError::CouldNotStop("held".into()))).exit_code(), EXIT_STOP_FAILED);
}
```

- [ ] **Step 2: Run them.** `... cargo test --locked autostart::`. Expected: FAIL to compile.

- [ ] **Step 3: Implement**
  - `retry_for`: the first attempt runs immediately. Sleep `step` between attempts until `max` has elapsed. Return `Ok(attempts)` on success, otherwise the last error.
  - `image_holders(exe)`: `K32EnumProcesses` (feature `Win32_System_ProcessStatus`, already enabled; it is the kernel32 export, so `psapi.dll` is not needed). Grow the pid buffer while the returned size fills it. Skip pid 0 and `GetCurrentProcessId()`. Keep the pids whose `crate::apps::exe_path_of_pid(pid)` equals `exe.to_string_lossy()`, ignoring ASCII case. Skip a process that cannot be opened: the dock and its watchdog run elevated as the same user, so the elevated installer can always open them.
  - `stop_and_wait(dest)`:
    1. `signalled = signal_stop_to_instance()`, logged. Continue when it is false: the old dock may have crashed while its watchdog is still restoring the taskbar (up to 30 s, `watchdog.rs:59` `RESTORE_WAIT`).
    2. `retry_for(STOP_WAIT, RETRY_STEP, || if instance_running() || !image_holders(dest).is_empty() { Err(()) } else { Ok(()) })`. This is the spec's wait: "`instance_running()` false and no `dock.exe --watchdog` child holding the file".
    3. Return `Ok(signalled)`. On timeout, log the remaining pids and return `Err(CouldNotStop("the running dock or its watchdog did not exit within 35 s"))`.
  - `copy_self`, in this order:
    1. If `same_file`, return `AlreadyInstalled`.
    2. Run `create_dir_all`, delete a stale `dock.exe.new` (ignore `NotFound`), then `fs::copy(me, new)`. If the copy fails, return `Failed` + `InstallError::Failed`, with no stop and no retry.
    3. Call `replace_file(new, dest)`. If it succeeds, return `Copied` (nothing was running from `dest`).
    4. If `!allow_stop`, delete `.new` and return `Failed`.
    5. Call `stop_and_wait(dest)`. On `Err(e)`, delete `.new` and return `Failed` with `error = Some(e)`.
    6. Call `retry_for(RELEASE_GRACE, RETRY_STEP, || replace_file(new, dest))`. On success, log the attempts and return `Copied`. On failure, delete `.new` and return `Failed` + `InstallError::Failed`.

    `dock.exe` is never written in place.
  - Keep the old copy relaunch (today's `copy_self:262-266`), but move it to `run_install_cli` through `should_relaunch`.
  - `enable`: `copy_self`. If `copy != Failed`, run `register_task(&dest)`; an error sets `error = Some(Failed(...))` but keeps `copy`, so the new exe is still relaunched. Task 4 inserts `arp::write` after `register_task`.
  - `run_install_cli`:
    1. `report = enable(true)`.
    2. `still_running = instance_running()`, sampled after `enable`. It is true only in two cases: the installed exe was re-run over its own running dock (`AlreadyInstalled`, nothing stopped), or `stop_and_wait` gave up.
    3. If `should_relaunch(still_running, no_relaunch)` and `dest.exists()`, call `relaunch(&dest)`. This starts the new exe after `Copied`, the installed exe after `AlreadyInstalled` with no dock running, and the old installed exe after a `Failed` copy, whether a dock was stopped or not (the spec's "always relaunches").
    4. Keep today's `say` messages. Call `report_legacy_copy()` only when `error.is_none()`.
    5. Return `report.exit_code()` (`EXIT_FAILED` when `installed_exe()` itself fails).
  - `popup_power.rs:247`: `let r = match crate::autostart::enable(false).error { None => Ok(()), Some(e) => Err(e.to_string()) };`.

- [ ] **Step 4: Run them.** Same filter, then the whole suite. Expected: PASS.

- [ ] **Step 5: Manual check (elevated, owner).**
  1. With 1.3.0 installed and running, run the new `target\release\dock.exe --install`. Expected: the old dock exits, the new one starts, exit code 0 (`$LASTEXITCODE`).
  2. Run `& "$env:ProgramFiles\mini-oled-dock\dock.exe" --install` while it runs. Expected: exit 0, the dock keeps running (same PID).
  3. Exit the dock, then run `--install` again. Expected: the dock starts.
  4. Crash case: end the dock process in Task Manager (the `dock.exe` without `--watchdog` on its command line), then at once run `target\release\dock.exe --install`. Expected: `dock.log` shows the wait until the watchdog has restored the taskbar and exited, then the replace. The exit code is 0 and the dock starts.

- [ ] **Step 6: Commit**

```bash
git add src/autostart.rs src/popup_power.rs src/main.rs
git commit -m "install: atomic replace, 35 s wait for the dock and its watchdog, always relaunch" -m "Claude-Session: https://claude.ai/code/session_01FHR79Qfsfd7dTKsSjKFXxX"
```

---

### Task 4: Add/Remove Programs entry

**Files:**
- Create: `src/arp.rs`
- Modify: `src/main.rs:32-67` (`mod arp;`), `src/autostart.rs` (`enable`, after `register_task`)

**Interfaces:**
- Produces:

```rust
pub const ARP_KEY: &str = r"SOFTWARE\Microsoft\Windows\CurrentVersion\Uninstall\mini-oled-dock";
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RegValue { Sz(String), Dword(u32) }
pub fn arp_values(install_dir: &Path, version: &str, exe_bytes: u64) -> Vec<(&'static str, RegValue)>
pub fn write(install_dir: &Path) -> Result<(), String>   // reads dock.exe size, writes all values
pub fn delete() -> Result<(), String>                     // missing key = Ok
```

- [ ] **Step 1: Write the failing test** (`src/arp.rs` `mod tests`)

```rust
#[test]
fn arp_values_for_install_dir() {
    let v = arp_values(Path::new(r"C:\Program Files\mini-oled-dock"), "1.4.0", 700_001);
    let get = |k: &str| v.iter().find(|(n, _)| *n == k).map(|(_, x)| x.clone());
    let sz = |s: &str| Some(RegValue::Sz(s.to_string()));
    assert_eq!(get("DisplayName"), sz("mini-oled-dock"));
    assert_eq!(get("DisplayVersion"), sz("1.4.0"));
    assert_eq!(get("Publisher"), sz("Ops Software"));
    assert_eq!(get("InstallLocation"), sz(r"C:\Program Files\mini-oled-dock"));
    assert_eq!(get("DisplayIcon"), sz(r"C:\Program Files\mini-oled-dock\dock.exe,0"));
    assert_eq!(get("UninstallString"), sz(r#""C:\Program Files\mini-oled-dock\dock.exe" --uninstall --remove"#));
    assert_eq!(get("QuietUninstallString"), sz(r#""C:\Program Files\mini-oled-dock\dock.exe" --uninstall --remove --quiet"#));
    assert_eq!(get("NoModify"), Some(RegValue::Dword(1)));
    assert_eq!(get("NoRepair"), Some(RegValue::Dword(1)));
    assert_eq!(get("EstimatedSize"), Some(RegValue::Dword(684))); // KB, rounded up
    assert_eq!(v.len(), 10);
}
```

- [ ] **Step 2: Run it.** `... cargo test --locked arp::`. Expected: FAIL to compile.

- [ ] **Step 3: Implement** `src/arp.rs`.
  - `QuietUninstallString` is the one value beyond the spec's list. It gives winget and Intune a silent uninstall for free.
  - `write`: `RegCreateKeyExW(HKEY_LOCAL_MACHINE, ARP_KEY, 0, None, REG_OPTION_NON_VOLATILE, KEY_SET_VALUE | KEY_WOW64_64KEY, None, &mut key, None)`, then `RegSetValueExW` per value: `REG_SZ` as UTF-16 with its terminating NUL and the byte length, `REG_DWORD` as 4 LE bytes. Close the key with `RegCloseKey`, also on error.
  - `delete`: `RegDeleteKeyExW(HKEY_LOCAL_MACHINE, ARP_KEY, KEY_WOW64_64KEY.0, 0)`. `ERROR_FILE_NOT_FOUND` means `Ok`.
  - Version: `env!("CARGO_PKG_VERSION")`.
  - The module doc states that the key is written only elevated, from `enable()` (the `--install` and popup-toggle paths), and deleted by `--uninstall`.
  - `enable()` calls `arp::write(&install_dir()?)` after `register_task`. A failure sets `error = Failed("Add/Remove Programs entry: ...")` and keeps `copy`, so the new exe is still relaunched with exit code 1, as the spec says.

- [ ] **Step 4: Run it.** Same filter. Expected: PASS.

- [ ] **Step 5: Manual check (elevated).** After `--install`, run `Get-ItemProperty 'HKLM:\SOFTWARE\Microsoft\Windows\CurrentVersion\Uninstall\mini-oled-dock'`. Expected: the 10 values. The dock appears in Settings → Apps → Installed apps.

- [ ] **Step 6: Commit**

```bash
git add src/arp.rs src/main.rs src/autostart.rs
git commit -m "install: register in Add/Remove Programs" -m "Claude-Session: https://claude.ai/code/session_01FHR79Qfsfd7dTKsSjKFXxX"
```

---

### Task 5: Staging folders and purge in `datadir`

**Files:**
- Modify: `src/datadir.rs:74-77` (constant), `224-236` (`Root`, unchanged fields), new functions after `read_icon` (`datadir.rs:520-536`), tests at `datadir.rs:985+`

**Interfaces:**
- Produces (all require an initialized, **writable** root; otherwise `Err`/empty):

```rust
const UPDATE_NAME: &str = "update";
pub(crate) fn is_plain_dir_name(s: &str) -> bool                 // 1..=32 chars of [A-Za-z0-9.], first char not '.', not containing ".."
pub fn update_stage_dir(tag: &str) -> Result<PathBuf, String>    // creates/validates <root>\update and <root>\update\<tag>
pub fn update_stage_names() -> Vec<String>                       // real subfolders of an EXISTING <root>\update (never creates it)
pub fn write_stage_file(tag: &str, name: &str, bytes: &[u8]) -> Result<(), String>   // write_atomic(.., FileAcl::Inherit)
pub fn read_stage_file(tag: &str, name: &str, cap: u64) -> std::io::Result<Vec<u8>>
pub fn checked_stage_path(tag: &str, name: &str) -> std::io::Result<PathBuf>       // path safe to execute
pub fn remove_update_stage(tag: &str) -> Result<(), String>
pub fn remove_update_dir() -> Result<(), String>                 // whole <root>\update (missing = Ok)
pub fn purge() -> Result<(), String>                             // remove_dir_all(<root>); call only after logger::stop
```

- [ ] **Step 1: Write the failing tests**

```rust
#[test]
fn plain_dir_names() {
    for ok in ["v1.4.0", "v10.0.12"] {
        assert!(is_plain_dir_name(ok), "{ok}");
    }
    let long = "a".repeat(33);
    for bad in ["", ".", "..", ".v1", "v1..4", "v1/4", r"v1\4", "v1.4.0 ", "C:", "v1:4", long.as_str()] {
        assert!(!is_plain_dir_name(bad), "{bad:?}");
    }
}

#[test]
fn staging_needs_an_initialized_writable_root() {
    // Unit tests never call init(): nothing may be created or touched.
    assert!(update_stage_dir("v1.4.0").is_err());
    assert!(update_stage_names().is_empty());
    assert!(read_stage_file("v1.4.0", "dock.exe", 10).is_err());
    assert!(checked_stage_path("..", "dock.exe").is_err());
    assert!(write_stage_file("v1.4.0", "dock.exe", b"x").is_err());
    assert!(purge().is_err());
}
```

- [ ] **Step 2: Run them.** `... cargo test --locked datadir::`. Expected: FAIL to compile.

- [ ] **Step 3: Implement**
  - Every function first checks `writable()`. It also checks `is_plain_dir_name(tag)`, and `name` is one of `"dock.exe"` / `"dock.exe.sha256"` (`const STAGE_FILES: [&str; 2]`).
  - `update_stage_dir`: `ensure_subdir(&root.join(UPDATE_NAME), None, true, &mut notes)`, then the same for `update\<tag>`. The first `None` result returns `Err(notes.join("; "))`. Both folders inherit the protected root DACL (no explicit SDDL), and `classify` must say `Use`.
  - `checked_stage_path` / `read_stage_file`, done like `read_icon` (`datadir.rs:520-536`):
    1. `open_dir` the stage folder, then `dir_facts`, which must classify `Use` (admin-only, not a reparse point).
    2. Take the folder's `final_path`.
    3. `open_checked(file, true)`: no reparse point, single link.
    4. `final_path_matches(file_final, dir_final, name)`.

    Return the path, or the bytes through `read_capped`.
  - `update_stage_names`: `read_dir` of `<root>\update`. Keep entries whose `symlink_metadata` is a real directory (not a reparse point) and whose name is `is_plain_dir_name`.
  - `remove_update_stage`, `remove_update_dir`, `purge`: `std::fs::remove_dir_all`. Since Rust 1.58.1 it does not follow junctions or symlinks inside the tree, which matters because `icons\` is user-writable. Write that in the doc comment. `purge` deletes the root itself, and its doc says the logger must already be stopped (`dock.log` is open while it runs).

- [ ] **Step 4: Run them.** Same filter. Expected: PASS (including the existing `datadir` tests).

- [ ] **Step 5: Commit**

```bash
git add src/datadir.rs
git commit -m "datadir: admin-only update staging folders and purge" -m "Claude-Session: https://claude.ai/code/session_01FHR79Qfsfd7dTKsSjKFXxX"
```

---

### Task 6: `--uninstall`, `--uninstall --remove`, `--purge-data`

**Files:**
- Modify: `src/autostart.rs:298-315` (`disable`, unchanged), `360-377` (`run_uninstall_cli`)
- Modify: `src/main.rs:887-912` (uninstall call and purge after `logger::stop`)

**Interfaces:**
- Consumes: `retry_for`, `STOP_WAIT`, `RETRY_STEP`, `RELEASE_GRACE`, `stop_and_wait`, `InstallError`, `EXIT_*` (Tasks 2-3), `arp::delete()` (Task 4), `datadir::remove_update_dir()`, `datadir::purge()` (Task 5), `taskbar::restore_force_waiting(Duration)` (`taskbar.rs:806`).
- Produces:

```rust
fn removal_targets(install_dir: &Path) -> [PathBuf; 3]           // dock.exe, dock.exe.new, autostart-task.xml
fn remove_or_schedule(path: &Path) -> Result<bool, String>       // Ok(true) deleted now, Ok(false) scheduled for reboot
#[derive(Debug, Default)]
pub(crate) struct UninstallReport { pub stop: Option<InstallError>, pub failures: Vec<String>, pub scheduled: bool }
impl UninstallReport { pub(crate) fn exit_code(&self) -> i32 }   // stop error -> its code (3); any failure -> 1; else 0
pub fn run_uninstall_cli(remove: bool) -> i32                    // returns the report's exit_code()
```

- [ ] **Step 1: Write the failing tests**

```rust
#[test]
fn removal_targets_are_only_our_files() {
    let d = Path::new(r"C:\Program Files\mini-oled-dock");
    assert_eq!(
        removal_targets(d),
        [d.join("dock.exe"), d.join("dock.exe.new"), d.join("autostart-task.xml")]
    );
}

#[test]
fn uninstall_report_exit_codes() {
    assert_eq!(UninstallReport::default().exit_code(), EXIT_OK);
    // A file in use deleted at the next restart is still a successful uninstall.
    assert_eq!(UninstallReport { scheduled: true, ..Default::default() }.exit_code(), EXIT_OK);
    assert_eq!(UninstallReport { failures: vec!["ARP".into()], ..Default::default() }.exit_code(), EXIT_FAILED);
    let stuck = UninstallReport { stop: Some(InstallError::CouldNotStop("x".into())), ..Default::default() };
    assert_eq!(stuck.exit_code(), EXIT_STOP_FAILED);
}
```

- [ ] **Step 2: Run them.** `... cargo test --locked autostart::`. Expected: FAIL to compile.

- [ ] **Step 3: Implement**
  - Plain `--uninstall`: `disable()`, then `arp::delete()`; each error is pushed to `failures`. Update the message to "Automatic start and the Add/Remove Programs entry removed. The installed copy ... is kept".
  - `--uninstall --remove`, in this order:
    1. `stop_and_wait(&installed_exe)` (Task 3): the same wait as `--install`, so it ends only when the dock's mutex is gone and no process other than this one runs the installed `dock.exe` (the watchdog may still be restoring the taskbar for up to 30 s). Started from Add/Remove Programs, the uninstaller *is* the installed exe; `image_holders` skips its own pid, so the wait still ends. On `Err(e)`, set `stop = Some(e)` and return `EXIT_STOP_FAILED` before touching anything.
    2. `taskbar::restore_force_waiting(Duration::from_secs(5))`.
    3. `disable()`, then `arp::delete()`.
    4. Refuse to delete files if `symlink_metadata(install_dir)` is not a real directory.
    5. For each `removal_targets` entry that exists, run `remove_or_schedule`. It first tries `retry_for(RELEASE_GRACE, RETRY_STEP, || fs::remove_file(p))`: every holder has already exited in step 1, and the grace only covers the kernel releasing the image. If that fails (in practice only the exe of this very process, when started from ARP), it calls `MoveFileExW(p, PCWSTR::null(), MOVEFILE_DELAY_UNTIL_REBOOT)` and sets `scheduled`.
    6. `fs::remove_dir(install_dir)`. Only an empty folder is removed; nothing unknown is deleted. On failure call `MoveFileExW(dir, PCWSTR::null(), MOVEFILE_DELAY_UNTIL_REBOOT)`.
    7. `datadir::remove_update_dir()`.
    8. `say` one summary line, plus "Some files are removed at the next restart." when `scheduled`.

    Every failed step is pushed to `failures`. Return `report.exit_code()`.
  - `main.rs`: call `autostart::run_uninstall_cli(args.remove)`. After `logger::stop(log_thread)`, if `code == 0 && args.purge_data`, call `datadir::purge()`. On `Err(e)`, `logger::say(&format!("Removing the data folder failed: {e}"))` and set `code = 1`. On `Ok`, `say("Data folder removed.")`. This runs before the message box or console output, so the result is still shown (with `--quiet` it stays in the unshown buffer, Task 2, and only the exit code reports it).

- [ ] **Step 4: Run them.** Same filter, then the whole suite. Expected: PASS.

- [ ] **Step 5: Manual check (elevated, owner).**
  1. Run `dock.exe --uninstall --remove` with the dock running. Expected: the dock exits, the taskbar is back, the task and ARP key are gone, `%ProgramFiles%\mini-oled-dock` is gone (or scheduled), and `%ProgramData%\mini-oled-dock` is kept without `update\`.
  2. Reinstall, then uninstall from Settings → Apps. Expected: the same result, with a UAC prompt.
  3. Run `--uninstall --remove --purge-data`. Expected: the data folder is gone too.

- [ ] **Step 6: Commit**

```bash
git add src/autostart.rs src/main.rs
git commit -m "uninstall: --remove stops the dock and deletes the install, --purge-data removes the data folder" -m "Claude-Session: https://claude.ai/code/session_01FHR79Qfsfd7dTKsSjKFXxX"
```

---

### Task 7: Versions, tags, URLs (`update_release.rs`, part 1)

**Files:**
- Create: `src/update_release.rs`
- Modify: `src/main.rs` (`mod update_release;`)

**Interfaces:**
- Produces:

```rust
pub const REPO: &str = "ops-software/mini-oled-dock";
pub const LATEST_API_URL: &str = "https://api.github.com/repos/ops-software/mini-oled-dock/releases/latest";
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Default)]
pub struct Version { pub major: u32, pub minor: u32, pub patch: u32 }   // field order = semver order
impl Version {
    pub fn parse(s: &str) -> Option<Version>   // "X.Y.Z" or "vX.Y.Z", parts 1-5 ASCII digits
    pub fn current() -> Version                // CARGO_PKG_VERSION (Default if it ever fails to parse)
    pub fn tag(self) -> String                 // "vX.Y.Z"
}
impl std::fmt::Display for Version             // "X.Y.Z"
pub fn parse_tag(s: &str) -> Option<Version>   // canonical "vX.Y.Z" only: Version::parse(s) with tag() == s
pub fn is_newer(tag: &str, current: Version) -> bool
pub fn asset_url(tag: &str, name: &str) -> String
pub fn release_page_url(tag: Option<&str>) -> String
pub fn host_allowed(host: &str) -> bool        // api.github.com, github.com, *.githubusercontent.com (case-insensitive)
```

- [ ] **Step 1: Write the failing tests**

```rust
#[test]
fn version_parse_and_order() {
    assert_eq!(Version::parse("1.4.0"), Some(Version { major: 1, minor: 4, patch: 0 }));
    assert_eq!(Version::parse("v1.4.0"), Version::parse("1.4.0"));
    for bad in ["", "1.4", "1.4.0.1", "v1.4.0-rc1", "1.4.0+meta", " 1.4.0", "1.4.x", "1..0", "v-1.4.0", "123456.0.0", "V1.4.0"] {
        assert_eq!(Version::parse(bad), None, "{bad}");
    }
    assert!(Version::parse("1.10.0") > Version::parse("1.9.9"));
    assert!(Version::parse("2.0.0") > Version::parse("1.99.99"));
    assert_eq!(Version::current().to_string(), env!("CARGO_PKG_VERSION"));
}

#[test]
fn tags_are_canonical() {
    assert_eq!(parse_tag("v1.4.1"), Version::parse("1.4.1"));
    for bad in ["1.4.1", "v1.04.1", "v01.4.1", "../v1.4.1", "v1.4.1 ", "v1.4.1/", r"v1.4.1\x"] {
        assert_eq!(parse_tag(bad), None, "{bad}");
    }
    let cur = Version::parse("1.4.0").unwrap();
    assert!(is_newer("v1.4.1", cur) && is_newer("v1.5.0", cur) && is_newer("v2.0.0", cur));
    assert!(!is_newer("v1.4.0", cur) && !is_newer("v1.3.9", cur) && !is_newer("garbage", cur));
}

#[test]
fn urls_and_hosts() {
    assert_eq!(asset_url("v1.4.1", "dock.exe"), "https://github.com/ops-software/mini-oled-dock/releases/download/v1.4.1/dock.exe");
    assert_eq!(release_page_url(Some("v1.4.1")), "https://github.com/ops-software/mini-oled-dock/releases/tag/v1.4.1");
    assert_eq!(release_page_url(None), "https://github.com/ops-software/mini-oled-dock/releases/latest");
    for ok in ["api.github.com", "github.com", "objects.githubusercontent.com", "release-assets.githubusercontent.com", "GitHub.com"] {
        assert!(host_allowed(ok), "{ok}");
    }
    for bad in ["", "evil.com", "github.com.evil.com", "xgithub.com", ".githubusercontent.com", "githubusercontent.com.evil"] {
        assert!(!host_allowed(bad), "{bad}");
    }
}
```

- [ ] **Step 2: Run them.** `... cargo test --locked update_release::`. Expected: FAIL to compile.

- [ ] **Step 3: Implement** the signatures above. `release_page_url(Some(t))` must only accept a tag that passes `parse_tag`; anything else falls back to `/releases/latest`, so no string from JSON or config ever becomes a URL path.

- [ ] **Step 4: Run them.** Same filter. Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add src/update_release.rs src/main.rs
git commit -m "update: semver, canonical tags, release URLs and host allow-list" -m "Claude-Session: https://claude.ai/code/session_01FHR79Qfsfd7dTKsSjKFXxX"
```

---

### Task 8: Release JSON, `.sha256`, version output, stale stages, schedule (`update_release.rs`, part 2)

**Files:**
- Modify: `src/update_release.rs`

**Interfaces:**
- Consumes: Task 7.
- Produces:

```rust
pub const API_MAX_BYTES: u64 = 1024 * 1024;
pub const EXE_MAX_BYTES: u64 = 20 * 1024 * 1024;
pub const SHA_MAX_BYTES: u64 = 1024;
pub const NOTES_MAX_CHARS: usize = 16 * 1024;
pub const EXE_ASSET: &str = "dock.exe";
pub const SHA_ASSET: &str = "dock.exe.sha256";
pub const FIRST_CHECK_MS: u32 = 60_000;
pub const CHECK_PERIOD_MS: u32 = 86_400_000;
pub const RETRY_MS: u32 = 3_600_000;
#[derive(Clone, Debug, PartialEq, Eq)] pub struct Asset { pub url: String, pub size: u64 }
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Release { pub tag: String, pub version: Version, pub name: String, pub notes: String, pub published: String, pub exe: Asset, pub sha256: Asset }
pub fn parse_release(json: &[u8]) -> Result<Release, String>
pub fn parse_sha256_file(bytes: &[u8]) -> Option<[u8; 32]>
pub fn version_output_matches(stdout: &[u8], v: Version) -> bool     // trimmed stdout == "mini-oled-dock X.Y.Z"
pub fn stale_stages(names: &[String], current: Version) -> Vec<String>   // canonical tags <= current, input order
pub fn rfc3339_utc(unix: u64) -> String                              // "YYYY-MM-DDTHH:MM:SSZ"
pub fn parse_rfc3339_utc(s: &str) -> Option<u64>                     // exactly that form, valid calendar date
pub fn next_check_delay_ms(now: u64, last: Option<u64>) -> u32       // 0 = check now
// Module level, directly above `#[cfg(test)] mod tests` and NOT inside it, so that Tasks 12-14 can
// import it as `crate::update_release::RELEASE_FIXTURE`.
#[cfg(test)] pub(crate) const RELEASE_FIXTURE: &str = /* the JSON below */;
```

- [ ] **Step 1: Write the failing tests.** `RELEASE_FIXTURE` goes at module level, above `mod tests` (see Interfaces). The rest goes inside `#[cfg(test)] mod tests { use super::*; ... }`.

```rust
// Module level, outside `mod tests`:
#[cfg(test)]
pub(crate) const RELEASE_FIXTURE: &str = r#"{"tag_name":"v1.4.1","name":"mini-oled-dock 1.4.1","body":"Fixes","published_at":"2026-11-20T10:00:00Z","draft":false,"prerelease":false,"html_url":"https://evil.example/","assets":[{"name":"dock.exe","size":720896,"browser_download_url":"https://github.com/ops-software/mini-oled-dock/releases/download/v1.4.1/dock.exe"},{"name":"dock.exe.sha256","size":76,"browser_download_url":"https://github.com/ops-software/mini-oled-dock/releases/download/v1.4.1/dock.exe.sha256"}]}"#;

// Inside `mod tests`:
const ABC: &str = "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"; // SHA-256("abc")

#[test]
fn release_json_parses() {
    let r = parse_release(RELEASE_FIXTURE.as_bytes()).unwrap();
    assert_eq!((r.tag.as_str(), r.version), ("v1.4.1", Version::parse("1.4.1").unwrap()));
    assert_eq!((r.notes.as_str(), r.published.as_str()), ("Fixes", "2026-11-20T10:00:00Z"));
    assert_eq!(r.exe, Asset { url: asset_url("v1.4.1", EXE_ASSET), size: 720_896 });
    assert_eq!(r.sha256.url, asset_url("v1.4.1", SHA_ASSET));
}

#[test]
fn release_json_rejects_hostile_input() {
    let swap = |a: &str, b: &str| RELEASE_FIXTURE.replace(a, b);
    let bad = [
        swap("\"tag_name\":\"v1.4.1\"", "\"tag_name\":\"../../x\""),
        swap("\"prerelease\":false", "\"prerelease\":true"),
        swap("\"draft\":false", "\"draft\":true"),
        swap("download/v1.4.1/dock.exe\"", "download/v1.4.0/dock.exe\""), // URL not for this tag
        swap("\"size\":720896", "\"size\":20971521"),                    // over 20 MB
        swap("\"size\":720896", "\"size\":0"),
        swap("\"name\":\"dock.exe\"", "\"name\":\"other.exe\""),        // asset missing
        "<html>captive portal</html>".to_string(),
        "[".repeat(10_000),
        r#"{"tag_name": 5}"#.to_string(),
    ];
    for b in bad {
        assert!(parse_release(b.as_bytes()).is_err(), "{:.60}", b);
    }
    let long = swap("\"Fixes\"", &format!("\"{}\"", "é".repeat(20_000)));
    assert_eq!(parse_release(long.as_bytes()).unwrap().notes.chars().count(), NOTES_MAX_CHARS);
}

#[test]
fn sha256_file_formats() {
    let mut want = [0u8; 32];
    for (i, b) in want.iter_mut().enumerate() {
        *b = u8::from_str_radix(&ABC[2 * i..2 * i + 2], 16).unwrap();
    }
    // release.yml: Out-File -Encoding ascii -> "<hex>  dock.exe\r\n"
    assert_eq!(parse_sha256_file(format!("{ABC}  dock.exe\r\n").as_bytes()), Some(want));
    assert_eq!(parse_sha256_file(format!("{}  dock.exe", ABC.to_uppercase()).as_bytes()), Some(want));
    assert_eq!(parse_sha256_file(format!("\u{feff}{ABC} *dock.exe\n").as_bytes()), Some(want));
    for bad in [
        String::new(),
        ABC.to_string(),
        format!("{ABC}  other.exe"),
        format!("{}  dock.exe", &ABC[1..]),
        format!("{ABC}  dock.exe extra"),
        format!("{ABC}g  dock.exe"),
    ] {
        assert_eq!(parse_sha256_file(bad.as_bytes()), None, "{bad}");
    }
    assert_eq!(parse_sha256_file(&[b'a'; 2000]), None);
}

#[test]
fn version_output() {
    let v = Version::parse("1.4.1").unwrap();
    assert!(version_output_matches(b"mini-oled-dock 1.4.1\r\n", v));
    assert!(!version_output_matches(b"mini-oled-dock 1.4.0\r\n", v));
    assert!(!version_output_matches(b"mini-oled-dock 1.4.1 extra", v));
    assert!(!version_output_matches(b"", v));
}

#[test]
fn stale_stage_folders() {
    let names: Vec<String> = ["v1.3.0", "v1.4.0", "v1.4.1", "notes", "v1.04.0"].map(String::from).to_vec();
    assert_eq!(stale_stages(&names, Version::parse("1.4.0").unwrap()), ["v1.3.0", "v1.4.0"]);
}

#[test]
fn rfc3339_and_schedule() {
    assert_eq!(rfc3339_utc(0), "1970-01-01T00:00:00Z");
    assert_eq!(rfc3339_utc(1_794_227_696), "2026-11-09T12:34:56Z");
    assert_eq!(rfc3339_utc(1_709_251_199), "2024-02-29T23:59:59Z");
    assert_eq!(parse_rfc3339_utc("2026-11-09T12:34:56Z"), Some(1_794_227_696));
    for bad in ["", "2026-11-09", "2026-13-01T00:00:00Z", "2025-02-29T00:00:00Z", "2026-11-09T12:34:56+01:00", "2026-11-09T12:34:56.1Z"] {
        assert_eq!(parse_rfc3339_utc(bad), None, "{bad}");
    }
    let now = 1_794_227_696;
    assert_eq!(next_check_delay_ms(now, None), 0);
    assert_eq!(next_check_delay_ms(now, Some(now + 600)), 0); // clock moved back
    assert_eq!(next_check_delay_ms(now, Some(now - 3_600)), 23 * 3_600_000);
    assert_eq!(next_check_delay_ms(now, Some(now - 86_390)), FIRST_CHECK_MS); // floor of 60 s
    assert_eq!(next_check_delay_ms(now, Some(now - 90_000)), 0);
}
```

- [ ] **Step 2: Run them.** `... cargo test --locked update_release::`. Expected: FAIL to compile.

- [ ] **Step 3: Implement**
  - `parse_release`: private `#[derive(Deserialize)] #[serde(default)]` structs (`tag_name`, `name`, `body`, `published_at`, `draft`, `prerelease`, `assets[].{name, size, browser_download_url}`), parsed with `serde_json::from_slice`. The default recursion limit rejects the nested input.
    - Reject the input on: a non-canonical tag, a draft or pre-release, a missing `dock.exe` or `dock.exe.sha256` asset, an asset URL that is not exactly `asset_url(tag, name)`, an exe size outside `1..=EXE_MAX_BYTES`, or a sha size outside `1..=SHA_MAX_BYTES`.
    - `notes` keeps the first `NOTES_MAX_CHARS` chars.
    - `published` is kept only if `parse_rfc3339_utc` accepts it, otherwise `""`.
    - `html_url` is ignored.
  - `parse_sha256_file`: cap `SHA_MAX_BYTES` and ASCII only after stripping a UTF-8 BOM. The first line must hold exactly two whitespace-separated tokens: 64 hex digits (either case) and `dock.exe` or `*dock.exe`.
  - `rfc3339_utc` / `parse_rfc3339_utc`: the days-from-civil and civil-from-days algorithms (H. Hinnant), no crate.
  - `next_check_delay_ms`: `None` or `last > now` gives 0. Otherwise compute `elapsed = now - last`: if `elapsed >= 86_400`, return 0, else return `max((86_400 - elapsed) * 1000, FIRST_CHECK_MS)`.

- [ ] **Step 4: Run them.** Same filter. Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add src/update_release.rs
git commit -m "update: release JSON, .sha256 and --version parsing, stale stages, check schedule" -m "Claude-Session: https://claude.ai/code/session_01FHR79Qfsfd7dTKsSjKFXxX"
```

---

### Task 9: SHA-256 through CNG

**Files:**
- Create: `src/sha256.rs`
- Modify: `Cargo.toml:21-73` (feature `"Win32_Security_Cryptography"`, alphabetical, if not already present), `build.rs:11-22` (`"bcrypt.dll"` in `DELAY_LOAD`), `src/main.rs` (`mod sha256;`)

**Interfaces:**
- Produces: `pub fn sha256(data: &[u8]) -> Result<[u8; 32], String>` in `src/sha256.rs`.

- [ ] **Step 1: Write the failing test**

```rust
#[test]
fn known_vectors() {
    let hex = |h: [u8; 32]| h.iter().map(|b| format!("{b:02x}")).collect::<String>();
    assert_eq!(hex(sha256(b"abc").unwrap()), "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad");
    assert_eq!(hex(sha256(b"").unwrap()), "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855");
    assert_eq!(hex(sha256(&vec![0u8; 1 << 20]).unwrap()), "30e14955ebf1352266dc2ff8067e68104607e750abb9d3b36582b8af909fcb58");
}
```

- [ ] **Step 2: Run it.** `... cargo test --locked sha256::`. Expected: FAIL to compile.

- [ ] **Step 3: Implement.** Use one-shot `BCryptHash(BCRYPT_SHA256_ALG_HANDLE, None, data, &mut out)` (pseudo-handle, Windows 10+; no provider to open or close). A non-success `NTSTATUS` becomes `Err(format!("BCryptHash: 0x{:08X}", status.0))`.

- [ ] **Step 4: Run it.** Same filter. Expected: PASS.

- [ ] **Step 5: Verify the delay load.** After `cargo build --release --locked`, run `dumpbin /imports target\release\dock.exe | Select-String -Context 0,3 bcrypt` from a VS developer prompt. Expected: `bcrypt.dll` appears in the "delay load imports" section.

- [ ] **Step 6: Commit**

```bash
git add Cargo.toml Cargo.lock build.rs src/sha256.rs src/main.rs
git commit -m "update: SHA-256 through CNG (bcrypt.dll delay-loaded)" -m "Claude-Session: https://claude.ai/code/session_01FHR79Qfsfd7dTKsSjKFXxX"
```

---

### Task 10: Blocking HTTPS GET through WinHTTP

**Files:**
- Create: `src/winhttp.rs`
- Modify: `Cargo.toml:21-73` (feature `"Win32_Networking_WinHttp"`), `build.rs:11-22` (`"winhttp.dll"`), `src/main.rs` (`mod winhttp;`)

**Interfaces:**
- Produces:

```rust
pub struct Response { pub status: u16, pub body: Vec<u8>, pub final_url: String }
#[derive(Debug)] pub enum HttpError { BadUrl, Network(String), TooLarge }   // + Display
pub fn user_agent() -> String                                                // "mini-oled-dock/<CARGO_PKG_VERSION>"
pub fn split_https_url(url: &str) -> Option<(String, String)>                // (host, path+query)
pub fn get(url: &str, headers: &[(&str, &str)], cap: u64) -> Result<Response, HttpError>
```

- [ ] **Step 1: Write the failing tests**

```rust
#[test]
fn https_urls_only() {
    assert_eq!(
        split_https_url("https://api.github.com/repos/a/b/releases/latest"),
        Some(("api.github.com".into(), "/repos/a/b/releases/latest".into()))
    );
    assert_eq!(split_https_url("https://github.com"), Some(("github.com".into(), "/".into())));
    for bad in ["http://github.com/x", "https://", "https://user@github.com/x", "https://github.com:8443/x", "ftp://x/y", "https://git hub.com/x", "HTTPS://github.com/x"] {
        assert_eq!(split_https_url(bad), None, "{bad}");
    }
    assert_eq!(user_agent(), format!("mini-oled-dock/{}", env!("CARGO_PKG_VERSION")));
}

#[test]
#[ignore = "network: run with --ignored"]
fn live_latest_release() {
    let r = get(crate::update_release::LATEST_API_URL, &[("Accept", "application/vnd.github+json")], 1 << 20).unwrap();
    assert!(r.status == 200 || r.status == 403, "{}", r.status);
}
```

- [ ] **Step 2: Run them.** `... cargo test --locked winhttp::`. Expected: FAIL to compile.

- [ ] **Step 3: Implement `get`**, with a small RAII guard that calls `WinHttpCloseHandle` on drop.
  1. `WinHttpOpen(user_agent, WINHTTP_ACCESS_TYPE_AUTOMATIC_PROXY, None, None, 0)`.
  2. `WinHttpSetTimeouts(h, 10_000, 10_000, 30_000, 30_000)`: resolve, connect, send, receive, as in the spec.
  3. `WinHttpSetOption(WINHTTP_OPTION_SECURE_PROTOCOLS, TLS1_2 | TLS1_3)`. The values are 0x800 and 0x2000; use literals if the constants are missing in 0.62.
  4. `WinHttpSetOption(WINHTTP_OPTION_REDIRECT_POLICY, WINHTTP_OPTION_REDIRECT_POLICY_DISALLOW_HTTPS_TO_HTTP)`. Redirects are followed automatically; asset downloads redirect to `*.githubusercontent.com`.
  5. `WinHttpConnect(host, 443)`, then `WinHttpOpenRequest("GET", path, None, None, None, WINHTTP_FLAG_SECURE)`, then `WinHttpAddRequestHeaders` for each header.
  6. `WinHttpSendRequest`, then `WinHttpReceiveResponse`.
  7. Read the status with `WinHttpQueryHeaders(WINHTTP_QUERY_STATUS_CODE | WINHTTP_QUERY_FLAG_NUMBER)`.
  8. Read the final URL with `WinHttpQueryOption(WINHTTP_OPTION_URL)`.
  9. Loop `WinHttpQueryDataAvailable` / `WinHttpReadData` into `body`. Return `TooLarge` as soon as `body.len() > cap`.

  Every WinAPI failure becomes `Network(<function>: <error>)`. A non-2xx status is still `Ok`; the caller decides. The function blocks, and its doc says "never on the UI thread".

- [ ] **Step 4: Run them.** Same filter. Expected: PASS (1 ignored). Then run `... cargo test --locked live_latest_release -- --ignored`. Expected: PASS when online.

- [ ] **Step 5: Verify the delay load.** Run `dumpbin /imports target\release\dock.exe` after a release build. Expected: `winhttp.dll` appears among the delay load imports.

- [ ] **Step 6: Commit**

```bash
git add Cargo.toml Cargo.lock build.rs src/winhttp.rs src/main.rs
git commit -m "update: HTTPS GET through WinHTTP (winhttp.dll delay-loaded)" -m "Claude-Session: https://claude.ai/code/session_01FHR79Qfsfd7dTKsSjKFXxX"
```

---

### Task 11: Config keys for updates

**Files:**
- Modify: `src/config.rs:375-466` (struct `Config`, after `notif_prompted`), `486-530` (`Default`), `676-898` (`render` template and arguments), tests `1246+`
- Modify: `README.md:138-163` (Configuration table)

**Interfaces:**
- Produces: `Config::update_check: bool` (true), `Config::update_auto_install: bool` (false), `Config::last_update_check: String` (""), and `Config::update_available: String` ("").

- [ ] **Step 1: Write the failing test**

```rust
#[test]
fn update_keys_default_and_roundtrip() {
    let d = Config::default();
    assert!(d.update_check && !d.update_auto_install);
    assert!(d.last_update_check.is_empty() && d.update_available.is_empty());
    let c = Config {
        update_check: false,
        update_auto_install: true,
        last_update_check: "2026-11-09T12:34:56Z".into(),
        update_available: "v1.4.1".into(),
        ..Config::default()
    };
    let r = render(&c);
    assert!(r.contains("\"update_check\": false") && r.contains("\"update_auto_install\": true"));
    assert!(r.contains("\"last_update_check\": \"2026-11-09T12:34:56Z\"") && r.contains("\"update_available\": \"v1.4.1\""));
    assert_eq!(parse(&r).unwrap(), c);
    assert!(parse("{}").unwrap().update_check); // an old config.json gets the default
}
```

- [ ] **Step 2: Run it.** `... cargo test --locked update_keys`. Expected: FAIL to compile.

- [ ] **Step 3: Implement.** Add the four fields with doc comments, the defaults, and a template block placed just before `notif_prompted`. The strings go through `js()`, the booleans go in plain. The template comments:

```text
  // Updates. Once a day the dock asks GitHub (api.github.com) whether a newer release exists and
  // shows a badge on Settings; nothing is downloaded until you choose "Update now" in Settings ->
  // Updates. false = never ask by itself ("Check now" still works).
  "update_check": {update_check},

  // true = also download, verify (SHA-256 + version) and install a new release by itself, the next
  // time no popup or Settings window is open and no fullscreen app is in front.
  "update_auto_install": {update_auto_install},

  // Internal: time of the last successful check (UTC) and the newest release found. Leave as is.
  "last_update_check": {last_update_check},
  "update_available": {update_available},
```

  Add two rows to the README table, after `audio_layout`: `update_check` | `true` | "Ask GitHub once a day whether a newer release exists (badge only)", and `update_auto_install` | `false` | "Download, verify and install new releases by themselves". Below the table, add one line: "`last_update_check` and `update_available` are written by the dock itself."

- [ ] **Step 4: Run it.** `... cargo test --locked config::`. Expected: PASS (`template_roundtrip` still passes).

- [ ] **Step 5: Commit**

```bash
git add src/config.rs README.md
git commit -m "config: update_check, update_auto_install, last_update_check, update_available" -m "Claude-Session: https://claude.ai/code/session_01FHR79Qfsfd7dTKsSjKFXxX"
```

---

### Task 12: Update state, check worker, `--update-check`

**Files:**
- Create: `src/update.rs`
- Modify: `src/msg.rs:63-67` (after `WM_DOCK_START_MENU`), `src/main.rs` (`mod update;`, `--update-check` mode in `main` before the single-instance mutex)

**Interfaces:**
- Consumes: `update_release::*` (Tasks 7-8), `winhttp::get` (Task 10), `Config` keys (Task 11), `config::get()`, `config::modify(f)`, `crate::hwnd()`, `shell_thread::request(ShellRequest::Launch { target, args })`.
- Produces (the API the Settings window plan consumes):

```rust
// msg.rs
/// update worker (`update` thread) -> UI: the update status changed. wParam = lParam = 0.
/// `main` calls `update::on_changed(hwnd)`. Consecutive posts may be coalesced: re-read `update::status()`.
pub const WM_DOCK_UPDATE: u32 = WM_APP + 17;

// update.rs
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum Phase { #[default] Idle, Checking, Downloading, Verifying, Installing }
#[derive(Clone, Debug, PartialEq, Eq, Default)]
pub enum Outcome { #[default] None, UpToDate, Available, CouldNotCheck(Option<u16>), DownloadFailed, VerifyFailed, Ready, InstallFailed(i32) }
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Available { pub tag: String, pub version: Version, pub date: String, pub notes: String }   // date "YYYY-MM-DD" or ""
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct UpdateStatus { pub installed: Version, pub phase: Phase, pub available: Option<Available>,
                          pub last_check: String, pub outcome: Outcome,
                          pub staged: bool }   // true = `available`'s tag is downloaded and verified. Set only by
                                               // `stage()` (Task 13); cleared by `stage()` on failure and by
                                               // `apply_check` when nothing newer exists.
impl UpdateStatus {
    pub fn badge(&self) -> u32      // 1 when `available` is Some, else 0
    pub fn line(&self) -> String    // short localized status line (labels below)
}
#[derive(Debug)] pub(crate) enum CheckError { Http(u16), Network(String), Invalid(String) }
pub(crate) fn status_from_config(c: &Config, installed: Version) -> UpdateStatus
pub(crate) fn apply_check(st: &mut UpdateStatus, r: &Result<Release, CheckError>, installed: Version, now: u64)
fn fetch_latest() -> Result<Release, CheckError>
fn with_state<R>(f: impl FnOnce(&mut UpdateStatus) -> R) -> R   // fills a None STATE from config on first use
fn notify()                              // posts WM_DOCK_UPDATE to crate::hwnd()
pub fn status() -> UpdateStatus          // snapshot, through with_state
pub fn check_now() -> bool               // false = an update operation is already running
pub fn open_release_page()               // de-elevated, through the shell thread
pub fn run_check_cli() -> i32            // --update-check
```

- [ ] **Step 1: Write the failing tests**

```rust
use crate::update_release::{parse_release, Release, Version, RELEASE_FIXTURE};

#[test]
fn status_from_config_trusts_only_newer_tags() {
    let cur = Version::parse("1.4.0").unwrap();
    let with = |t: &str| Config { update_available: t.into(), last_update_check: "2026-11-09T12:34:56Z".into(), ..Config::default() };
    let s = status_from_config(&with("v1.4.1"), cur);
    assert_eq!(s.available.as_ref().map(|a| a.tag.as_str()), Some("v1.4.1"));
    assert_eq!((s.badge(), s.staged, s.last_check.as_str()), (1, false, "2026-11-09T12:34:56Z"));
    for t in ["", "v1.4.0", "v1.3.9", "garbage", "../v9.9.9", "v99.0.0-rc1"] {
        assert_eq!(status_from_config(&with(t), cur).badge(), 0, "{t}");
    }
}

#[test]
fn check_results_update_the_status() {
    let cur = Version::parse("1.4.0").unwrap();
    let rel = parse_release(RELEASE_FIXTURE.as_bytes()).unwrap();
    let mut s = status_from_config(&Config::default(), cur);
    apply_check(&mut s, &Ok(rel.clone()), cur, 1_794_227_696);
    assert_eq!((s.outcome.clone(), s.badge()), (Outcome::Available, 1));
    assert_eq!(s.available.as_ref().unwrap().date, "2026-11-20");
    assert_eq!(s.last_check, "2026-11-09T12:34:56Z");
    apply_check(&mut s, &Err(CheckError::Http(403)), cur, 1_794_300_000);
    assert_eq!(s.outcome, Outcome::CouldNotCheck(Some(403)));
    assert_eq!((s.badge(), s.last_check.as_str()), (1, "2026-11-09T12:34:56Z")); // a failed check changes nothing else
    let same = Release { tag: "v1.4.0".into(), version: cur, ..rel };
    apply_check(&mut s, &Ok(same), cur, 1_794_300_000);
    assert_eq!((s.outcome.clone(), s.badge()), (Outcome::UpToDate, 0));
}
```

- [ ] **Step 2: Run them.** `... cargo test --locked update::`. Expected: FAIL to compile.

- [ ] **Step 3: Implement**
  - State: `static STATE: Mutex<Option<UpdateStatus>>`, read and written only through `with_state`. On first use `with_state` fills a `None` with `status_from_config(&config::get(), Version::current())`. So from this task on, every phase and outcome the workers set (here and in Tasks 13-14) is visible through `status()`, including in the manual checks of Tasks 13-14, before Task 15 exists. Task 15's `init` only replaces the value with a fresh one read after `config::init`. Also `static LATEST: Mutex<Option<Release>>` (the last fetched newer release, in memory only). `static BUSY: AtomicBool` serializes all worker operations.
  - `fn spawn_worker(job: impl FnOnce() + Send + 'static) -> bool`: compare-and-swap `BUSY`, then `std::thread::Builder::new().name("update".into())`. The job clears `BUSY` when it ends. If the spawn fails, clear `BUSY` and log.
  - `fn notify()`: `PostMessageW(Some(crate::hwnd()), WM_DOCK_UPDATE, ...)`. With no window (CLI) it fails harmlessly.
  - `fetch_latest`:
    1. Call `winhttp::get(LATEST_API_URL, &[("Accept", "application/vnd.github+json"), ("X-GitHub-Api-Version", "2022-11-28")], API_MAX_BYTES)`. `Network` and `TooLarge` map to `CheckError::Network`.
    2. If the final URL's host fails `host_allowed`, return `Invalid`.
    3. If the status is not 200, return `Http(status)` and log `update: check failed, HTTP {status}`.
    4. Otherwise `parse_release`; an error returns `Invalid`.
  - `apply_check`:
    - `Ok(rel)` with `rel.version > installed`: set `available = Some(...)` (`date = published[..10]` or `""`, `notes = rel.notes`), `outcome = Available`, `last_check = rfc3339_utc(now)`.
    - `Ok(rel)` that is not newer: clear `available` and `staged`, `outcome = UpToDate`, update `last_check`.
    - `Err(Http(c))`: `CouldNotCheck(Some(c))`. Any other `Err`: `CouldNotCheck(None)`. Only `outcome` changes.
  - `check_now`: a worker runs `phase = Checking` → `notify` → `fetch_latest` → `apply_check`. A newer `Ok` is stored in `LATEST`. Then `phase = Idle`.
    - If `last_update_check` or `update_available` differ from the config, call `config::modify(|c| { c.last_update_check = ..; c.update_available = tag or "" })`. Do not post `WM_DOCK_CONFIG_APPLY`; the bar does not change.
    - Then `notify`.
    - Log one line per check: `update: up to date (latest vX)`, `update: vX available`, or `update: could not check (...)`.
  - `open_release_page`: `release_page_url(status().available.map(|a| a.tag).as_deref())`, then `shell_thread::request(ShellRequest::Launch { target: url, args: None })`. `shell_thread::launch` always runs de-elevated or refuses (`shell_thread.rs:1466-1500`).
  - `line()` labels. Format variable parts with `format!`; `tr` supplies only the fixed text:

    | State | English | Italian |
    |---|---|---|
    | `Checking` | Checking for updates… | Ricerca aggiornamenti… |
    | `Downloading` | Downloading {tag}… | Download di {tag}… |
    | `Verifying` | Verifying the download… | Verifica del download… |
    | `Installing` | Installing {tag}… | Installazione di {tag}… |
    | `None` | Not checked yet | Non ancora verificato |
    | `UpToDate` | Up to date | Aggiornato |
    | `Available` | Version {v} available | Disponibile la versione {v} |
    | `CouldNotCheck(Some(c))` | Could not check for updates (HTTP {c}) | Impossibile cercare aggiornamenti (HTTP {c}) |
    | `CouldNotCheck(None)` | Could not check for updates (no connection) | Impossibile cercare aggiornamenti (nessuna connessione) |
    | `DownloadFailed` | Download failed | Download non riuscito |
    | `VerifyFailed` | Download failed verification | Il download non ha superato la verifica |
    | `Ready` | Version {v} downloaded and verified | Versione {v} scaricata e verificata |
    | `InstallFailed(c)` | Installation failed (exit code {c}) | Installazione non riuscita (codice {c}) |

    A non-`Idle` phase wins over `outcome`.
  - `run_check_cli`: synchronous `fetch_latest`, no config write. It prints one `logger::say` line:
    - `mini-oled-dock {cur}: up to date (latest {tag})`, exit 0.
    - `mini-oled-dock {cur}: {tag} available ({date}) {release_page_url}`, exit 0.
    - `mini-oled-dock {cur}: could not check ({error})`, exit 1.
  - `main.rs`: `if args.update_check { if attach_console() { logger::enable_stdout(); } let (t, _) = start_logging(""); let code = update::run_check_cli(); logger::stop(t); return ExitCode::from(code as u8); }`. Put it after `--restore` and before the single-instance mutex.

- [ ] **Step 4: Run them.** Same filter, then the whole suite. Expected: PASS.

- [ ] **Step 5: Manual check.** Run `target\release\dock.exe --update-check` from an elevated terminal. Expected: `mini-oled-dock 1.3.0: up to date (latest v1.3.0)` (or the current latest), exit 0. Unplug the network and run it again. Expected: `could not check (...)`, exit 1.

- [ ] **Step 6: Commit**

```bash
git add src/update.rs src/msg.rs src/main.rs
git commit -m "update: status, background check against GitHub Releases, --update-check" -m "Claude-Session: https://claude.ai/code/session_01FHR79Qfsfd7dTKsSjKFXxX"
```

---

### Task 13: Download, verify, stage

**Files:**
- Modify: `src/update.rs`

**Interfaces:**
- Consumes: `with_state`, `notify`, `UpdateStatus` (Task 12), `datadir::{update_stage_dir, write_stage_file, read_stage_file, checked_stage_path, remove_update_stage}` (Task 5), `sha256::sha256` (Task 9), `winhttp::get` (Task 10), `parse_sha256_file`, `version_output_matches`, `host_allowed`, `EXE_MAX_BYTES`, `SHA_MAX_BYTES` (Tasks 7-8).
- Produces:

```rust
#[derive(Debug)] pub(crate) enum StageError { Download(String), Verify(String) }
pub(crate) fn verify_bytes(exe: &[u8], sha_file: &[u8]) -> Result<(), String>
pub(crate) fn run_capture(exe: &Path, args: &[&str], timeout: Duration) -> Result<(i32, Vec<u8>), String>
fn download(rel: &Release) -> Result<(), StageError>
pub(crate) fn verify_stage(tag: &str) -> Result<PathBuf, StageError>   // Ok = path of the verified staged dock.exe
fn stage(rel: &Release) -> Result<PathBuf, StageError>                 // reuse a stage that verifies, else download + verify;
                                                                        // the only place that sets UpdateStatus.staged = true
```

- [ ] **Step 1: Write the failing tests**

```rust
#[test]
fn verify_bytes_compares_the_published_hash() {
    let sha = b"ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad  dock.exe\r\n";
    assert!(verify_bytes(b"abc", sha).is_ok());
    assert!(verify_bytes(b"abd", sha).is_err());
    assert!(verify_bytes(b"abc", b"not a hash").is_err());
}

fn system32(exe: &str) -> PathBuf {
    PathBuf::from(std::env::var_os("SystemRoot").unwrap()).join("System32").join(exe)
}

#[test]
fn run_capture_reads_stdout_and_times_out() {
    let (code, out) = run_capture(&system32("cmd.exe"), &["/c", "echo mini-oled-dock 1.4.1"], Duration::from_secs(5)).unwrap();
    assert_eq!(code, 0);
    assert!(crate::update_release::version_output_matches(&out, Version::parse("1.4.1").unwrap()));
    let t = Instant::now();
    assert!(run_capture(&system32("ping.exe"), &["-n", "6", "127.0.0.1"], Duration::from_millis(300)).is_err());
    assert!(t.elapsed() < Duration::from_secs(3)); // killed, not waited for
}
```

- [ ] **Step 2: Run them.** `... cargo test --locked update::`. Expected: FAIL to compile.

- [ ] **Step 3: Implement**
  - `verify_bytes`: `parse_sha256_file(sha_file)` must succeed, and `sha256(exe)` must equal it.
  - `run_capture`: `std::process::Command` with `creation_flags(CREATE_NO_WINDOW.0)`, stdout piped, stdin and stderr null. Poll `try_wait` every 50 ms like `autostart::run_schtasks`. On timeout, `kill()` + `wait()` and return `Err("timeout")`. Read stdout (cap 4 KB) after the exit.
  - `download`: for `rel.exe` then `rel.sha256`:
    1. `winhttp::get(url, &[("Accept", "application/octet-stream")], cap)`, with `cap` = `EXE_MAX_BYTES` / `SHA_MAX_BYTES`.
    2. Require status 200, `host_allowed` on the final URL host, and `body.len() as u64 == asset.size`.
    3. `update_stage_dir(&rel.tag)`, then `write_stage_file(&rel.tag, name, &body)`.

    Drop each body right after writing it. Every failure is `StageError::Download(msg)` and leaves the staging folder for the next attempt.
  - `verify_stage(tag)`:
    1. `parse_tag(tag)` must succeed (else `Verify`).
    2. `read_stage_file(tag, "dock.exe", EXE_MAX_BYTES)` and `read_stage_file(tag, "dock.exe.sha256", SHA_MAX_BYTES)`.
    3. `verify_bytes`.
    4. `checked_stage_path(tag, "dock.exe")`.
    5. `run_capture(&path, &["--version"], Duration::from_secs(5))`: the exit code must be 0 and `version_output_matches(out, version)` true.

    On any failure, call `remove_update_stage(tag)`, log `update: {tag} failed verification: {why}`, and return `Verify`. A missing file is `Download`, so `stage` will download it.
  - `stage`: `verify_stage(&rel.tag)`. If it fails with `Download(_)` (nothing or partial staged), call `download(rel)` and then `verify_stage` again. Update `phase` (`Downloading`, `Verifying`) through `with_state` and `notify()` at each step. At the end, through `with_state`:
    - `Ok`: `staged = true`, `outcome = Ready`, `phase = Idle`.
    - `Err(Download(_))`: `staged = false`, `outcome = DownloadFailed`, `phase = Idle`.
    - `Err(Verify(_))`: `staged = false`, `outcome = VerifyFailed`, `phase = Idle`.

    Then `notify()`. `stage` is the single owner of `staged = true`. Its callers (`install_now`, Task 14, and the auto-install check worker, Task 15) never set `staged` or these outcomes themselves.
  - Since `verify_stage` runs the staged exe, the doc comment says why it is safe: SHA-256 from the same release over HTTPS; the folder and file checked by `datadir`; the exe is in an admin-only folder created by this elevated process.

- [ ] **Step 4: Run them.** Same filter. Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add src/update.rs
git commit -m "update: download to the admin-only staging folder, verify SHA-256 and --version" -m "Claude-Session: https://claude.ai/code/session_01FHR79Qfsfd7dTKsSjKFXxX"
```

---

### Task 14: Install handoff, startup cleanup, `--update-install <tag>`

**Files:**
- Modify: `src/watchdog.rs:185-235` (`launch_watchdog` → shared `spawn_breakaway`)
- Modify: `src/update.rs`, `src/main.rs` (`--update-install` mode, next to `--update-check`)

**Interfaces:**
- Consumes: `stage`, `verify_stage` (Task 13), `fetch_latest`, `apply_check`, `with_state`, `notify`, `LATEST` (Task 12), `datadir::{update_stage_names, remove_update_stage}`, `stale_stages` (Tasks 5, 8).
- Produces:

```rust
// watchdog.rs: CreateProcessW with CREATE_BREAKAWAY_FROM_JOB | CREATE_NO_WINDOW, falling back to
// CREATE_NO_WINDOW; returns the process handle (thread handle closed). launch_watchdog uses it and drops the handle.
pub(crate) fn spawn_breakaway(exe: &Path, args: &str) -> Result<std::os::windows::io::OwnedHandle, String>

// update.rs
pub(crate) const INSTALL_ARGS: &str = "--install --quiet";
const INSTALL_WAIT: Duration = Duration::from_secs(120);
pub(crate) fn install_target(latest: Option<&Release>, installed: Version) -> Option<Release>  // Some only if newer
fn wait_exit(process: &OwnedHandle, timeout: Duration) -> Option<i32>
pub fn install_now() -> bool          // false = an update operation is already running
pub fn run_install_cli(tag: &str) -> i32
fn cleanup_stages()
```

- [ ] **Step 1: Write the failing test**

```rust
#[test]
fn install_target_needs_a_fresh_newer_release() {
    let cur = Version::parse("1.4.0").unwrap();
    let rel = parse_release(RELEASE_FIXTURE.as_bytes()).unwrap();
    assert_eq!(install_target(None, cur), None); // a badge from config.json alone never installs
    assert_eq!(install_target(Some(&rel), cur), Some(rel.clone()));
    assert_eq!(install_target(Some(&rel), Version::parse("1.4.1").unwrap()), None);
    assert_eq!(INSTALL_ARGS, "--install --quiet");
}
```

- [ ] **Step 2: Run it.** `... cargo test --locked install_target`. Expected: FAIL to compile.

- [ ] **Step 3: Implement**
  - `spawn_breakaway`: move the loop body of `launch_watchdog` (`watchdog.rs:190-234`) into it. The command line is `"\"{exe}\" {args}"`. Wrap `pi.hProcess` in an `OwnedHandle`, and close `pi.hThread` at once. Breakaway is needed here for the same reason as for the watchdog: the dock may run inside the Task Scheduler job, and the installer must outlive the dock it stops.
  - `install_now`: a worker runs these steps.
    1. `rel = install_target(LATEST, installed)`. If it is `None`, run `fetch_latest` + `apply_check` (the check path of Task 12) and try again. If it is still `None`, set `outcome = UpToDate`, `notify`, and return.
    2. Call `stage(&rel)`. It sets `staged`, `outcome` and `phase` itself (Task 13). On `Err`, return.
    3. Set `phase = Installing` and `notify`. Log `update: installing {tag} from {path}`.
    4. Call `spawn_breakaway(&path, INSTALL_ARGS)`. The child signals `Local\DockStop`; the dock leaves through its normal stop path (`WM_DOCK_QUIT_REQUEST`, cleanup, taskbar restored, exit 0). The child then replaces, re-registers, writes the ARP key and relaunches (Tasks 3-4).
    5. `wait_exit(&child, INSTALL_WAIT)`. If the child exits non-zero while the dock is still alive, set `outcome = InstallFailed(code)`, `phase = Idle`, `notify`, and keep the staging folder. A spawn error is `InstallFailed(-1)`.
  - `wait_exit`: `WaitForSingleObject(handle, ms)`. On `WAIT_OBJECT_0`, return `GetExitCodeProcess`; on timeout, `None`.
  - `cleanup_stages`, run from `init` (Task 15) on a thread named `update-cleanup` and only if `datadir::writable()`: for each name in `stale_stages(&datadir::update_stage_names(), Version::current())`, call `remove_update_stage`. Log failures only. The just-finished installer may still be running from the folder of the current version; the next start deletes it.
  - `run_install_cli(tag)`:
    1. `parse_tag(tag)`, else print `invalid tag` and return `EXIT_BAD_ARGS`.
    2. `datadir::writable()`, else 1.
    3. `verify_stage(tag)`, else print the reason and return 1.
    4. `spawn_breakaway(path, INSTALL_ARGS)`, then `wait_exit(INSTALL_WAIT)`, and return the child's exit code (1 on timeout). Print `Installed {tag}.` or `Installer exited with {code}.`.

    The only accepted input is a canonical tag, mapped to `<data>\update\<tag>`; there is no path argument.
  - `main.rs`: `if let Some(tag) = &args.update_install { attach console + start_logging(""); let code = update::run_install_cli(tag); logger::stop(..); return ExitCode::from(code.clamp(0, 255) as u8); }`.
  - In `main.rs:376-381`, change the `WM_DOCK_QUIT_REQUEST` log text to `"quit request (stop event: --restore, --install or --uninstall): exiting"`.

- [ ] **Step 4: Run it.** Same filter, then the whole suite. Expected: PASS (including the `watchdog` code paths in the build).

- [ ] **Step 5: Manual check (owner, elevated).**
  1. Create `%ProgramData%\mini-oled-dock\update\v1.4.0\` from an elevated prompt (`New-Item`, which inherits the admin-only ACL).
  2. Build a test exe at version 1.4.0 with a local, **uncommitted** bump (see *Version bump*): set `version = "1.4.0"` in `Cargo.toml`, run `cargo build --release` (without `--locked`, because `Cargo.lock` records the package version), copy `target\release\dock.exe` out, then `git checkout Cargo.toml Cargo.lock` and rebuild 1.3.0 with `cargo build --release --locked`. Copy the 1.4.0 exe into the folder with its `.sha256`, generated with the `release.yml` command (`"$h  dock.exe" | Out-File -Encoding ascii dock.exe.sha256`).
  3. With 1.3.0 installed and running, run `dock.exe --update-install v1.4.0`. Expected: the old dock exits (taskbar restored), the 1.4.0 dock starts from Program Files, the exit code is 0, and `dock.log` shows the stop, replace, task, ARP and relaunch lines.
  4. Corrupt one byte of the staged exe and run it again. Expected: exit 1, "failed verification", and the folder is deleted.

- [ ] **Step 6: Commit**

```bash
git add src/watchdog.rs src/update.rs src/main.rs
git commit -m "update: hand the verified exe to --install --quiet, clean old stages, --update-install" -m "Claude-Session: https://claude.ai/code/session_01FHR79Qfsfd7dTKsSjKFXxX"
```

---

### Task 15: Check timer, auto-install gate, dock wiring

**Files:**
- Modify: `src/msg.rs:121-124` (timer constant), `src/update.rs`
- Modify: `src/main.rs:98-103` (imports), `277-289` (`TIMER_HOUSEKEEPING`), `297-299` (new `WM_TIMER` arm), `368-371` (`WM_DOCK_CONFIG_APPLY` + new `WM_DOCK_UPDATE` arm), `642-655` (`cleanup` timer list), `818-819` (`setup_and_loop`, after `set_timer(h, TIMER_SECOND, 1000)`)
- Modify: `src/ui.rs` (new `pub fn foreground_is_fullscreen()` next to `fullscreen_window`, `ui.rs:3038-3090`)

**Interfaces:**
- Consumes: Tasks 12-14; `popup::is_open() -> bool`, `popup::tick_soon(ms: u32)`.
- Produces:

```rust
// msg.rs
/// Dock window, armed only while `update_check` is on (`update::sync_timer`): first 60 s after start, then the
/// remaining time to 24 h since `last_update_check`, 24 h after a check, 1 h after a failed scheduled check.
/// Handled by `main` -> `update::on_timer`.
pub const TIMER_UPDATE_CHECK: usize = 13;

// update.rs
pub fn init(h: HWND)                        // status from config, spawn update-cleanup, sync_timer
pub fn sync_timer(h: HWND)                  // arm (FIRST_CHECK_MS) / kill per config::get().update_check
pub fn on_timer(h: HWND)
pub fn on_changed(h: HWND)                  // WM_DOCK_UPDATE on the UI thread
pub fn auto_install_pending() -> bool
pub fn on_idle(busy: bool)                  // from TIMER_HOUSEKEEPING when auto_install_pending()
pub(crate) fn auto_install_due(auto_install: bool, staged: bool, phase: Phase, busy: bool) -> bool

// ui.rs
pub fn foreground_is_fullscreen() -> bool   // fullscreen_window(rcMonitor of MonitorFromWindow(GetForegroundWindow(), MONITOR_DEFAULTTONEAREST)).is_some()
```

- [ ] **Step 1: Write the failing test**

```rust
#[test]
fn auto_install_gate() {
    assert!(auto_install_due(true, true, Phase::Idle, false));
    assert!(!auto_install_due(false, true, Phase::Idle, false)); // option off
    assert!(!auto_install_due(true, false, Phase::Idle, false)); // nothing verified yet
    assert!(!auto_install_due(true, true, Phase::Downloading, false));
    assert!(!auto_install_due(true, true, Phase::Idle, true)); // popup / Settings / fullscreen app
}
```

- [ ] **Step 2: Run it.** `... cargo test --locked auto_install_gate`. Expected: FAIL to compile.

- [ ] **Step 3: Implement**
  - `init(h)`: replace the state with `status_from_config(&config::get(), Version::current())` through `with_state` (the config is loaded by then; a value filled earlier by `with_state` is overwritten). Spawn `update-cleanup` (Task 14). Call `sync_timer(h)`.
  - `sync_timer`: `static ARMED: AtomicBool`. If `update_check` is on and not armed, call `SetTimer(h, TIMER_UPDATE_CHECK, FIRST_CHECK_MS)`. If it is off and armed, call `KillTimer`. Log `update: automatic check on/off`.
  - `on_timer`:
    1. `delay = next_check_delay_ms(now_unix, parse_rfc3339_utc(&config::get().last_update_check))`, where `now_unix` comes from `SystemTime::now()` relative to `UNIX_EPOCH`.
    2. If `delay == 0`: set `SCHEDULED` (AtomicBool), call `check_now()`, then `SetTimer(.., CHECK_PERIOD_MS)`. Otherwise call `SetTimer(.., delay)`.

    Re-arming replaces the interval, so there is still one timer.
  - The worker of a check started with `update_auto_install` on, whose result is `Available`, continues with `stage(&rel)` in the same worker run. `stage` sets `staged = true` and `outcome = Ready` on success (Task 13); the worker sets nothing itself. Nothing is downloaded with the option off.
  - `on_changed(h)`:
    1. If `SCHEDULED` is set and the status is no longer `Checking`, clear the flag. If the outcome is `CouldNotCheck`, call `SetTimer(h, TIMER_UPDATE_CHECK, RETRY_MS)`.
    2. Call `popup::tick_soon(10)` so an open power popup redraws its badge.
  - `auto_install_pending()`: `config::get().update_auto_install && status().staged && phase == Idle`.
  - `on_idle(busy)`: if `auto_install_due(..)`, log `update: auto-install {tag}` and call `install_now()`.
  - `main.rs`:
    - Add `TIMER_UPDATE_CHECK => { update::on_timer(h); LRESULT(0) }` in `WM_TIMER`.
    - Add `WM_DOCK_UPDATE => { update::on_changed(h); LRESULT(0) }`.
    - In `TIMER_HOUSEKEEPING`, after `trim_tick()`: `if update::auto_install_pending() { update::on_idle(popup::is_open() || ui::foreground_is_fullscreen()); }`. The Settings-window plan adds `|| settings_window::is_open()` to this expression.
    - Call `update::sync_timer(h)` after `ui::on_config_changed()` in both the `WM_DOCK_CONFIG_APPLY` arm and the `config::poll_changed()` branch.
    - Call `update::init(h)` in `setup_and_loop` after the service timers.
    - Add `TIMER_UPDATE_CHECK` to the `cleanup` `KillTimer` list.

- [ ] **Step 4: Run it.** Same filter, then the whole suite. Expected: PASS.

- [ ] **Step 5: Manual check.**
  1. Set `last_update_check` to `""` and start `dock.exe --console`. Expected: after about 60 s, `update: up to date (latest v1.3.0)` in the log, and `config.json` gets `last_update_check`.
  2. Restart the dock. Expected: no request in the first minute; the log says the next check is due in about 24 h.
  3. Set `"update_check": false` and save. Expected: `update: automatic check off`.

- [ ] **Step 6: Commit**

```bash
git add src/msg.rs src/update.rs src/main.rs src/ui.rs
git commit -m "update: 60 s + 24 h check timer, auto-install when nothing is open" -m "Claude-Session: https://claude.ai/code/session_01FHR79Qfsfd7dTKsSjKFXxX"
```

---

### Task 16: Update badge on the power popup's Settings row

**Files:**
- Modify: `src/render.rs:102` (`pub(crate) const ATTENTION_RGB`)
- Modify: `src/popup_power.rs:88-123` (struct and `new`), `166-193` (`paint`), `203-210` (`tick`)
- Modify: `src/preview.rs:561` (`make_popup`) and `src/preview.rs:812-838` (render list)

**Interfaces:**
- Consumes: `update::status().badge() -> u32` (Task 12).
- Produces: `pub(crate) fn settings_badge(count: u32) -> Option<String>`, `pub(crate) fn preview_with_badge(count: u32) -> Box<dyn Content>` (both in `popup_power.rs`). The Settings-window plan reuses `settings_badge` and `ATTENTION_RGB` for the Updates nav entry.

- [ ] **Step 1: Write the failing test** (in a new `mod tests` in `popup_power.rs`)

```rust
#[test]
fn settings_badge_text() {
    assert_eq!(settings_badge(0), None);
    assert_eq!(settings_badge(1).as_deref(), Some("1"));
    assert_eq!(settings_badge(12).as_deref(), Some("9+"));
}
```

- [ ] **Step 2: Run it.** `... cargo test --locked settings_badge`. Expected: FAIL to compile.

- [ ] **Step 3: Render the baseline before touching the paint code.** Build with `DOCK_NO_MANIFEST=1 CARGO_TARGET_DIR=target-preview cargo build --release`, then run `target-preview\release\dock.exe --preview $env:USERPROFILE\dock-preview\base` (any folder you own: the preview build is not elevated and the folder is only used for the comparison).

- [ ] **Step 4: Implement**
  - `PowerPopup` gets `badge: u32` and `forced_badge: Option<u32>`. `new()` sets `badge = crate::update::status().badge()`. `tick()` re-reads it (unless forced) and returns `Reply::Redraw` when it changes.
  - In `paint`, for `ID_SETTINGS` with `Some(text) = settings_badge(self.badge)`, draw the spec's "orange dot and count", right-aligned before the chevron: an orange dot that carries the count. For one digit it is a circle; for `9+` it stretches into a pill.
    - height `m.px(16)`, width `max(m.px(16), text_width + m.px(8))` (so `"1"` gives a 16 px circle);
    - `s.fill_rr(rc, m.px(8), crate::render::ATTENTION_RGB, 255)`;
    - `text` in `Font::Small`, `WHITE`, alpha `TEXT_ALPHA`, centred;
    - a gap of `m.px(6)` to the chevron.
  - Nothing is drawn when the count is 0, so `popup_power.png` stays byte-identical.
  - `preview_with_badge(n)` is `new()` with `forced_badge = Some(n)`.
  - `preview.rs`: add `"popup_power_update" => crate::popup_power::preview_with_badge(1)` and the list entry `("popup_power_update", 4)` after `("popup_power", 4)`.

- [ ] **Step 5: Run the tests.** Same filter, then the whole suite. Expected: PASS.

- [ ] **Step 6: Preview check.** Rebuild the preview build and render into a new folder. Then compare with `Get-FileHash` against the baseline. Expected:
  - every baseline PNG is identical, including `popup_power.png` and `popup_power_96.png` (`preview.rs:46`: `DPIS = [(120, ""), (96, "_96")]`);
  - `popup_power_update.png` and `popup_power_update_96.png` are new;
  - the owner checks them visually: an orange 16 px dot with a white "1" on the Settings row, before the chevron.

- [ ] **Step 7: Commit**

```bash
git add src/render.rs src/popup_power.rs src/preview.rs
git commit -m "power popup: update badge on the Settings row" -m "Claude-Session: https://claude.ai/code/session_01FHR79Qfsfd7dTKsSjKFXxX"
```

---

### Task 17: README and release notes

**Files:**
- Modify: `README.md:88-114` (Install, Update, Uninstall), `README.md:207-219` (Data folder table), `README.md:116-130` (Usage, one bullet)
- Create or modify: `.github/release-notes/v1.4.0.md` (append an `## Updates` section; create the file with just that section if plan 01 has not created it yet)

**Interfaces:**
- Consumes: the flags, keys and behaviour of Tasks 1-16. The text states only what those tasks implement.

- [ ] **Step 1: Write the README changes**
  - **Install:**
    - `--install` copies atomically to `%ProgramFiles%\mini-oled-dock`, registers the logon task and the Add/Remove Programs entry, and always starts the dock at the end. `--no-relaunch` skips the start. Run from the installed copy while it is running, it only re-registers.
    - `--quiet` shows no message box. Exit codes: 0 ok, 1 failure, 2 bad arguments, 3 the running dock could not be stopped.
    - `dock.exe --version` prints the version.
  - **Update:** replaces today's paragraph.
    - The dock checks GitHub Releases once a day (`update_check`). A newer version shows an orange badge on the power popup's Settings entry.
    - **Update now** in Settings → Updates downloads `dock.exe` and its SHA-256 into the admin-only `%ProgramData%\mini-oled-dock\update\<tag>`, checks the hash and the exe's own `--version`, and runs it with `--install --quiet`. No UAC prompt; the dock restarts by itself.
    - `update_auto_install` does this automatically when no popup or Settings window is open and no fullscreen app is in front.
    - Trust rests on HTTPS to github.com and the hash in the same release. The exe is not code-signed.
    - Manual updating with a downloaded exe and `--install` still works.
    - Diagnostics: `--update-check` (prints the result) and `--update-install <tag>` (verifies and installs what is already staged in that folder; no other path is accepted).
  - **Uninstall:**
    - `--uninstall` removes the task and the Add/Remove Programs entry.
    - `--uninstall --remove`, also run by Settings → Apps → Installed apps, stops the dock, restores the taskbar and deletes `%ProgramFiles%\mini-oled-dock` (a file in use goes at the next restart). It keeps your settings.
    - `--purge-data` also deletes `%ProgramData%\mini-oled-dock`.
  - **Data folder table:** a new row: `update\<tag>\` | downloaded update (`dock.exe`, `dock.exe.sha256`), deleted once that version or a newer one runs | administrators only.
  - **Usage:** one bullet: "**Updates:** an orange dot on Settings in the power popup means a new release is available."

- [ ] **Step 2: Write the release notes.** The `## Updates` section of `v1.4.0.md` covers:
  - the self-updater: what it does, the default `update_check: true` (one request per day to api.github.com, badge only), and the default `update_auto_install: false`;
  - the "first time" step: users on 1.3.0 or 1.0.0 run `dock.exe --install` once from the 1.4.0 exe, and later releases arrive through the dock itself;
  - the Add/Remove Programs entry and real uninstall (`--remove`, `--purge-data`);
  - `--quiet`, `--version`, the exit codes, the version resource;
  - the install now always starts the dock and waits up to 35 s for the old dock and its watchdog;
  - the two diagnostic flags.

  Write it in English, in the style of `v1.3.0.md` (bold lead-in per bullet).

- [ ] **Step 3: Verify.** Run `Select-String -Path README.md,.github/release-notes/v1.4.0.md -Pattern 'update_check','update_auto_install','--purge-data','--update-install','--quiet'`. Expected: each one appears in both files where relevant. Every flag named in the docs must exist in `usage()` (`main.rs`). Compare by reading `dock.exe --help` output from a test build.

- [ ] **Step 4: Commit**

```bash
git add README.md .github/release-notes/v1.4.0.md
git commit -m "docs: installer flags, self-updater, uninstall and update settings" -m "Claude-Session: https://claude.ai/code/session_01FHR79Qfsfd7dTKsSjKFXxX"
```
