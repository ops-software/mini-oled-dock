# mini-oled-dock 1.4.0: review fixes, self-updater, Settings window

Date: 2026-11-09. Base: `main` at 0c033aa (v1.3.0 + 7). Language of the code, comments, UI strings and release notes: English (Italian labels through `i18n::tr`).

## Intent

Three pieces of work, shipped as 1.4.0:

1. **Fix the 12 findings** of the 2026-11-08 review of the upstream commits (details in `E:\Giovik\Documenti\Dock\docs\review-2026-11-08-findings.json`, one entry per finding with file, line, description, failure scenario and the two verifier reports).
2. **A self-updater** inside `dock.exe`: the dock learns that a new GitHub release exists and can install it itself, reusing the existing `--install` path. This replaces the idea of a separate installer (Inno/WiX): the readiness assessment in `E:\Giovik\Documenti\Dock\docs\review-2026-11-08-installer-readiness.md` explains why.
3. **A real Settings window** instead of the current settings popup. The owner's words: the popup "starts to be too full and too small". The chosen layout is variant A of the mockup: a resizable window with a left navigation of sections (https://claude.ai/artifact/KKCnYQBpWHS687Xqh9sHeA).

Everything else about the dock stays as it is. "Nothing changes behind your back" remains the rule: a user who updates without touching settings keeps the same look and behaviour, with the one exception stated under *Defaults* below.

## Goals, non-goals, anti-goals

Goals
- Every confirmed finding is fixed or explicitly argued down, with a test where the fix is testable in `cargo test`.
- A user on 1.3.0 (or 1.0.0) runs `dock.exe --install` once from the 1.4.0 exe; from then on the dock updates itself from GitHub Releases with no further manual download and no new UAC prompt (the dock is already elevated).
- The Settings window is clear, consistent and complete: every option that exists today is reachable, with an icon, a short description where the label is not self-explanatory, and the same visual language as the bar and the popups (OLED black, white pills, Segoe UI Variable).
- Memory budget kept: about 10 MB private while running, nothing allocated for the window while it is closed, no permanent timers for the updater beyond one 24 h check timer.

Non-goals
- No separate installer exe, no MSI, no winget manifest in this release (winget can come later once the exe has a version resource and silent flags, which this release adds).
- No new popup styles, no changes to the bar's drawing, no new bar features.
- No code signing (not available). Trust of a download rests on HTTPS to github.com and the SHA-256 published in the same release.
- No keyboard-complete UI in the Settings window beyond what is listed under *Keyboard*.

Anti-goals (what would make this a failure even if it works)
- The native taskbar left hidden in any exit path.
- Anything that runs with the elevated token from user-controlled input (the start_import finding is exactly this).
- The dock's RAM going up while the Settings window is closed.
- Settings that silently change on upgrade.

## Constraints

- Rust stable 1.87+, crate `windows` 0.62, no UI framework, no HTTP crate: WinHTTP through the `windows` crate (`Win32_Networking_WinHttp`), SHA-256 through CNG (`Win32_Security_Cryptography`). Both DLLs go in `DELAY_LOAD` in `build.rs`.
- `main` is protected: work on a branch, one pull request per piece (fixes, updater, settings window) in that order, each green on CI (`cargo test --locked` with `DOCK_NO_MANIFEST=1`, `cargo build --release --locked`, `cargo fmt`).
- The dock runs elevated. Files it writes go only in admin-only folders (`%ProgramData%\mini-oled-dock`, `%ProgramFiles%\mini-oled-dock`), never in `%TEMP%` or `%LOCALAPPDATA%`. Everything it reads from user-writable places is untrusted (size cap, no reparse points, parse defensively).
- `--preview <dir>` must keep rendering every popup byte-identically, and must additionally render each Settings window page (PNG at 96 and 144 DPI) for the README.
- Release process unchanged: bump `version` in `Cargo.toml` to `1.4.0`, add `.github/release-notes/v1.4.0.md`, tag `v1.4.0` after merge.

## Part 1: the fixes

Each finding below names the fix the owner has decided; the builder chooses the exact code. Order of work: security first, then taskbar, then the rest.

| # | Finding (file:line) | Decided fix |
|---|---|---|
| 1 | `start_import.rs:130` elevated PowerShell auto-loads `Export-StartLayout` from a user-writable PSModulePath (high) | Never spawn PowerShell from the elevated process. Run it **de-elevated** through the same desktop-shell path used to launch apps (`shell_thread` / IShellDispatch2, the "ALWAYS runs de-elevated" path). Only the same-user UAC case is supported (the logon task already runs as the interactive user; a standard user elevating with another admin's credentials is out of scope, as it is for every de-elevated launch today). Protocol: (a) the dock draws a 128-bit random id with `BCryptGenRandom` and builds `<dir>\start-layout-<id>.json`, where `<dir>` is the **shell user's** `%LOCALAPPDATA%\mini-oled-dock\` resolved with `SHGetKnownFolderPath(FOLDERID_LocalAppData, token)` using the token of the Explorer process the shell thread already locates (never the elevated process's own environment); (b) the de-elevated command is `powershell.exe -NoProfile -NonInteractive -ExecutionPolicy Bypass -Command "try { New-Item -ItemType Directory -Force '<dir>' | Out-Null; Get-ChildItem '<dir>\start-layout-*' | Where-Object LastWriteTime -lt (Get-Date).AddHours(-1) | Remove-Item -Force; Export-StartLayout -Path '<file>.tmp'; Move-Item -Force '<file>.tmp' '<file>' } catch { Set-Content '<file>.err' $_ }"`, so a file at the final name is complete (atomic rename) and a `.err` file means failure; (c) the dock polls every 200 ms for `<file>` or `<file>.err`, 15 s timeout = "Import failed (timeout)"; (d) the dock reads `<file>` as **untrusted input**: open without following reparse points (`FILE_FLAG_OPEN_REPARSE_POINT`, reject if it is one), 4 MB cap, parse with `serde_json`, keep only the fields already used. The dock never deletes or writes anything in that folder; the medium-integrity command cleans files older than one hour on each run. Failure to launch de-elevated reports "Import failed" like today. |
| 2 | `start_import.rs:111` `%TEMP%` fallback (medium) | Remove the fallback entirely. With fix 1 the elevated process writes nothing; `work_dir()` goes away. |
| 3 | `tray_host.rs:2118` AppBar gate accepts `m.pid == dock pid` from a medium sender (medium) | A message that claims the dock's own pid is rejected. The builder first confirms in `tray_host.rs` that the dock never sends itself AppBar messages through this path (the finding's verifiers say so); if a legitimate in-process sender exists, that sender is routed around the gate instead. Add a test with a crafted message. |
| 4 | `shell_thread.rs:1505` admin Start-menu tools launched by bare name (low) | Full paths under `%SystemRoot%\System32` (resolved from `GetSystemDirectoryW`, never from env vars), same for every `runas` entry. Terminal (Store app) keeps its AUMID. |
| 5 | `ui.rs:797` `tray_sync` can restart the tray host during the Explorer-restart pause (low) | `tray_sync` checks the pause state (`taskbar::explorer_restarting()` or equivalent) and does nothing while paused; the resume path triggers one sync. |
| 6 | `taskbar.rs:680` pause window can end with the taskbar in autohide at exit/logoff (low) | On exit (`WM_DOCK_QUIT_REQUEST`, `WM_ENDSESSION`, `WM_QUERYENDSESSION`) the pause is cancelled and the normal restore runs; the watchdog restores whenever the state file exists, pause or not. Test the state machine where it is pure. |
| 7 | `popup_startmenu.rs:175` Start icon cache never freed (medium) | The cache lives in the popup instance and is dropped in `after_close()`. Reopening re-extracts icons (they come from the shell thread lazily, as today). Log the freed size at debug level like `trim(hidden)` does. |
| 8 | `hook.rs:183` keyboard hook watchdog never repairs the hook while a stale `WIN_DOWN` is stuck (low) | The watchdog clears `WIN_DOWN` when `GetAsyncKeyState(VK_LWIN/RWIN)` says the key is up, then repairs. |
| 9 | `popup_startmenu.rs:65` Start menu and popups can exceed the work area at high DPI or small screens (low) | Clamp popup size to the monitor work area minus margins; lists scroll. One shared helper in `popup.rs`. |
| 10 | `popup.rs:1420` Start menu can stay open without keyboard focus over a fullscreen game (low) | If focus cannot be obtained after `FOCUS_RETRY_MS` retries (3), close the Start menu and log it; never leave it open unfocused. |
| 11 | `tray_host.rs:1233` out-of-context WinEvent callback does a cross-process `SetWindowPos` (low) | The callback only posts a message to the tray host thread; the thread does the `SetWindowPos`. |
| 12 | `autostart.rs:227` 8 s stop wait vs 30 s watchdog restore (low) | `STOP_WAIT` becomes 35 s, and `--install` also waits for the watchdog process to exit (`instance_running()` false and no `dock.exe --watchdog` child holding the file) before copying. |

The 7 refuted findings are not touched, except that the release notes mention the two new defaults (Start button, window previews) for users coming from 1.0.0.

## Part 2: installer prerequisites and self-updater

### Prerequisites (all in `--install` / CLI)

- **Version resource**: `build.rs` embeds `FileVersion`, `ProductVersion`, `ProductName` ("mini-oled-dock"), `FileDescription`, `LegalCopyright` ("MIT") from `CARGO_PKG_VERSION` and `Cargo.toml`. Builder picks the crate (`winresource` or `embed-resource`); it must not clash with `embed-manifest`.
- **`--version`**: prints `mini-oled-dock 1.4.0` and exits 0. Used by the updater to check a downloaded exe.
- **`--quiet`**: with `--install` / `--uninstall`, no `MessageBoxW`; result only by exit code and log. Exit codes: 0 ok, 1 failure, 2 bad arguments, 3 could not stop the running instance.
- **`--install` always relaunches** the dock at the end (today only when it had to stop one), unless `--no-relaunch`. Exception: `--install` run from the already-installed exe while that dock is running does not stop it and does not relaunch it; it only re-registers the logon task and the ARP key and exits 0.
- **Atomic replace**: `copy_self` copies to `dock.exe.new` in the install folder and then `MoveFileExW(MOVEFILE_REPLACE_EXISTING)` over `dock.exe`, so Program Files never holds a half-written exe. On a copy failure after the running instance was stopped, the old installed exe is relaunched (today's behaviour, kept); on a `schtasks` or registry failure after a successful copy, the new exe is relaunched anyway and the failure is reported (exit 1). The staging folder is kept on any failure so the update can be retried.
- **Add/Remove Programs**: `--install` writes `HKLM\SOFTWARE\Microsoft\Windows\CurrentVersion\Uninstall\mini-oled-dock` (DisplayName, DisplayVersion, Publisher "Ops Software", InstallLocation, DisplayIcon, UninstallString `"<installed>\dock.exe" --uninstall --remove`, NoModify, NoRepair, EstimatedSize). `--uninstall` deletes the key.
- **`--uninstall --remove`**: stops the running dock (same stop event, waits like `--install`), restores the taskbar, deletes the logon task, deletes the ARP key, and removes `%ProgramFiles%\mini-oled-dock` (the exe deletes itself with `MoveFileExW(MOVEFILE_DELAY_UNTIL_REBOOT)` for the file in use). The data folder is kept unless `--purge-data` is also given (elevated delete of the admin-only folder only).
- `--install` from a **staged** exe (not the installed path) is the self-update case; running `--install` from the already-installed exe keeps today's "nothing to copy" behaviour.

### Updater

- Module `update.rs`, worker thread started on demand (never on the UI thread). WinHTTP, `User-Agent: mini-oled-dock/<version>`, 10 s connect / 30 s receive timeouts, redirects followed (asset downloads redirect to `objects.githubusercontent.com`).
- **Check**: `GET https://api.github.com/repos/ops-software/mini-oled-dock/releases/latest`, parse `tag_name`, `name`, `body`, `published_at`, `assets[].{name,browser_download_url,size}` with `serde_json`. A release is "new" when `tag_name` (`vX.Y.Z`) is greater than `CARGO_PKG_VERSION` by semver (numeric compare of the three parts; anything that does not parse is ignored). Pre-releases are never offered (`/releases/latest` already excludes them). Rate limit or network errors are reported as "could not check" with the HTTP status in the log, never as "up to date".
- **When it checks**: on start, 60 s after the bar is ready, then every 24 h, if `update_check` is on; and on demand from the Settings window ("Check now"). The result (version, date, notes, checked-at) is kept in memory only, plus `last_update_check` (RFC 3339) and `update_available` (tag) in `config.json` so the badge survives a restart without a new request.
- **Download**: both assets (`dock.exe`, `dock.exe.sha256`) to `%ProgramData%\mini-oled-dock\update\<tag>\` created by the elevated dock (inherits the admin-only ACL). Size cap 20 MB. SHA-256 computed with CNG and compared to the `.sha256` file (format `<lowhex>  dock.exe`). Then the staged exe is run with `--version` (`CREATE_NO_WINDOW`, 5 s timeout) and its output must equal the tag's version. Any mismatch deletes the staging folder and reports "download failed verification".
- **Install**: the dock runs `<staged>\dock.exe --install --quiet` (elevated, inherited token, same session) and then exits through its own stop path when the child sets the stop event; the child copies, re-registers the task, writes the ARP key and relaunches. On start the dock deletes from `update\` every folder whose name is a version lower than or equal to the running one; folders with a higher version (a pending or hand-made staging) are kept.
- **UI**: Settings → Updates (see Part 3) with a status card, "Check now", "Update now", "Release notes" (opens the release page in the browser, de-elevated through the shell path), toggles `update_check` and `update_auto_install`, and a short log line of the last action. With `update_auto_install` on, a verified download is installed at once at the next moment the Settings window and every popup are closed and no fullscreen app is in the foreground. A badge (orange dot and count) appears on the Updates nav entry and on the Settings entry of the power popup when a new release is known.
- **Defaults** (owner's call): `update_check` = **true** (one GET per day to api.github.com, badge only), `update_auto_install` = **false**. Both documented in `config.json` and the README.

## Part 3: the Settings window

### Window

- A real top-level window (own class `DockSettings`), `WS_OVERLAPPEDWINDOW` without maximize box, resizable, min 640×460 logical, default 860×600 logical, remembered in `config.json` (`settings_window_size`, logical pixels at 96 DPI, clamped to the work area of the display it opens on). Centred on the bar's display when opened. One instance: opening again activates the existing window.
- Native DWM frame with the dark immersive title bar (`DWMWA_USE_IMMERSIVE_DARK_MODE`), caption and border colour set to the configured background colour (`DWMWA_CAPTION_COLOR`, `DWMWA_BORDER_COLOR`), so move, resize, snap and the close button come for free and match the theme.
- Client area painted with the existing `Surface` (DIB) and `copy_to` on `WM_PAINT`, non-layered. Lives on the UI thread like the popups; it does **not** use the popup "click outside closes" mouse hook, so the bar and popups keep working while it is open. Per-monitor DPI changes (`WM_DPICHANGED`) resize the surface and fonts.
- Coexistence: the window is independent of the popups. Opening it from the power popup closes that popup; popups opened later from the bar work normally with the window open; the bar's auto-hide is unchanged; a fullscreen app in the foreground simply covers the window (it is a normal window and never topmost). `settings_window::is_open()` is the queryable state the auto-install rule uses.
- Opened from: the power popup "Settings" row (today's entry), the Start quick-links menu (new entry "Dock settings"), and `dock.exe --settings` (posts a message to the running instance). Closed with the title-bar X, `Esc`, or when the dock exits. Nothing of the window exists in memory while it is closed (struct dropped, surface freed, icons released).
- The old `SettingsPopup` and its sub-pages (Background, Sizes, Tray chooser, Screen, Start menu row) are removed; `popup_settings.rs` is replaced by `settings_window.rs` (window, nav, paging, input) and `settings_pages.rs` (page content). `picker.rs` (colour picker) is reused inline.

### Layout (variant A)

- Left column 200 logical px: nav entries with icon + label; current entry has a 3 px white bar on the left and the hover fill. Bottom entries: Updates (with badge) and About.
- Right: page title (20 px semibold), one-line subtitle in dim text, groups with an uppercase 11 px letter-spaced header, rows of 44 px with icon (20 px column), label (+ optional 12 px dim description), and the control at the right: segmented pills, toggle, slider with value, swatches, button or chevron. Hover fill on rows. Page content max width 640 px; the page scrolls with the wheel when taller than the window.
- Controls reuse the drawing of today's popups (same toggle, pills, slider from `popup_settings`/`popup_audio`'s shared slider, swatches, picker). Every change applies at once through `config::modify` + `WM_DOCK_CONFIG_APPLY`, exactly as today; slider drags write the file on release.

### Sections and content

1. **Appearance**: live preview of the bar (a mini bar drawn with the real sizes, display mode, background colour and opacity, three sample apps, a volume glyph and the clock), then groups: *Colour* (background swatches: 7 presets + custom with the inline picker; opacity slider), *Sizes* (app icons, app names, tray icons, modules; Reset sizes), *Language* (System / English / Italiano).
2. **Bar**: *Position* (Top / Bottom; Display mode Icon+name / Icon / Name), *Behaviour* (Auto-hide, Window previews, Clock seconds, Volume percent).
3. **Start**: Start button toggle, Start layout (Vertical / Horizontal), Win key opens Start, *Pins*: Import from Windows Start (button, status text as today).
4. **Modules**: Notifications, Volume, Network, System metrics, Clock, Show desktop toggles; *Battery* (see Part 4); *Tray*: Tray icons toggle, "Choose visible icons" opens the chooser **inline** in the page (list of icons with eye toggles, as today's chooser but in the page).
5. **Screen**: Display (Main / numbers / Pointer), shown only when the option exists; with one display the page explains that the bar is on the only display.
6. **Updates**: status card (installed version, available version with date, last check), buttons Check now / Release notes / Update now, toggles Check automatically / Install automatically.
7. **About**: version and credits line, memory (private bytes now, trimmed value while hidden), data folder (Open button, de-elevated Explorer), log (Open), "Restore the Windows taskbar" with an **Exit the dock** button (same action as the power menu's exit).

### Icons

One icon per concept, drawn procedurally as glyphs in `glyphs.rs` (no PNGs, no fonts with symbols): palette, bar, grid (Start), list (modules), monitor, refresh (updates), info, circle (colour), drop (opacity), square (icon size), text (name size), ring (tray), bars (modules/metrics), globe (language), bell, speaker, wifi, clock, desktop, download, folder, log lines, power. The same glyphs are used in the bar and popups where the concept already appears (speaker, bell, clock, wifi, power) so the window looks like the rest of the dock.

### Keyboard

`Esc` closes. `Tab`/`Shift+Tab` move between nav and page; `Up/Down` in the nav change page; wheel scrolls the page. Nothing more in this release.

### Preview and screenshots

`--preview <dir>` renders `settings_appearance.png`, `settings_bar.png`, `settings_start.png`, `settings_modules.png`, `settings_screen.png`, `settings_updates.png`, `settings_about.png` at 96 and 144 DPI, with mock data (English). The README's Settings screenshots are replaced by these.

## Part 4: battery module (laptops)

Owner's request: on a laptop, a battery icon in the bar with Apple-style options.

- New system module `battery` (`battery.rs`), in the modules list like clock and volume, draggable and orderable like the others.
- **Visibility**: `show_battery` = `"auto"` (default) | `true` | `false`. With `"auto"` the module is shown only when `GetSystemPowerStatus` reports a battery (`BatteryFlag` without the 128 "no system battery" bit); on a desktop nothing is created, so the default changes nothing for desktop users. With `true` it is shown even without a battery (icon with a slash, for testing); with `false` never.
- **Icon**: a rounded battery outline with a nub, drawn as a glyph, fill proportional to the charge; a small lightning bolt overlay while charging; red fill below 20 %, orange below 10 % with the attention colour only at or below 10 %. Follows `module_size`.
- **Options** (Settings → Modules → Battery group): `battery_percent` (true: show the percentage; default true), `battery_percent_inside` (false: the number sits at the right of the icon, as today's volume percent; true: the number is drawn inside the battery body, Apple style, in a smaller tabular font; default false).
- **Popup** on left click: charge percentage, state (charging / on battery / fully charged), estimated time remaining or to full when Windows provides it (`BatteryLifeTime`), the active power mode name if `PowerGetActiveScheme` is cheap enough, and a "Power & battery settings" link (`ms-settings:batterysaver`, opened de-elevated). Right click opens the same Windows settings page directly. Same visual language as the network popup.
- **Cost**: nothing allocated when hidden. When visible: `GetSystemPowerStatus` every 30 s while the bar is visible (same gate as the network and metrics modules) plus `WM_POWERBROADCAST` (`PBT_APMPOWERSTATUSCHANGE`) for instant updates when the charger is plugged in or out. No thread.
- **Preview**: `bar_battery.png` (on battery, 64 %, percent beside), `bar_battery_inside.png` (charging, 100 %, percent inside), `popup_battery.png`.
- Settings window: Modules page gets a *Battery* group with the visibility choice (Auto / On / Off), the two toggles, and a note "Shown automatically on laptops". Mock data in `--preview` shows a battery.

## Defaults and config changes

No existing default changes in code. The only behaviour new on upgrade is the daily update check (`update_check` = true, owner's call, see Part 2). The 1.0.0 → 1.1.0 defaults (Start button and window previews on) are release-note text only.

New keys, all with defaults that keep today's behaviour: `update_check` (true, see above), `update_auto_install` (false), `last_update_check` (""), `update_available` (""), `settings_window_size` ([860, 600]), `show_battery` ("auto"), `battery_percent` (true), `battery_percent_inside` (false). Four PRs, not three: fixes, prerequisites + updater, battery module, Settings window (the window is last because its Modules page needs the battery options). No key is removed; `bar_size` migration stays.

## Testing

- `cargo test` covers: semver compare and release JSON parsing (fixtures), `.sha256` parsing, ARP key value building, exit codes, AppBar gate rejection, taskbar pause state machine, work-area clamp, Start-layout JSON parsing with hostile input (huge, nested, wrong types), settings page item lists per config.
- Preview baseline: every current preview PNG stays byte-identical **except** `popup_start*.png` (the quick-links menu gains the "Dock settings" entry) and the removed `popup_settings*.png`; `popup_power.png` is unchanged because the mock data has no pending update. The new settings PNGs are produced and checked visually by the owner.
- Live test by the owner: update from 1.3.0 installed to the 1.4.0 build with `--install`; then a fake "newer" release check against a test tag is not possible on the real repo, so the updater is tested by (a) unit tests, (b) a `--update-check` CLI flag that prints the result of the check and exits, (c) `--update-install <tag>` that runs the verify+install step on `%ProgramData%\mini-oled-dock\update\<tag>\` (only that admin-only location is accepted, never an arbitrary path, so nothing user-writable is ever run elevated). Both flags are documented as diagnostics.

## Left to the builder

- Exact crate for the version resource; how the delay-loaded WinHTTP/BCrypt are wired.
- Internal structure of `settings_pages.rs` (item model, ids, hit testing), as long as every page is data-driven enough to add a row without touching paint code.
- How the mini live preview reuses `render.rs` without duplicating it (a small function in `render.rs` that paints a given list of items into a `Surface` is acceptable).
- Which existing `ui.rs` message carries "settings window open/closed" to the bar, if the bar needs to know at all.
