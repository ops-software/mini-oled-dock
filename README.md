# <img src="assets/icon-256.png" alt="" width="40" height="40" align="absmiddle"> mini-oled-dock

[![CI](https://github.com/ops-software/mini-oled-dock/actions/workflows/ci.yml/badge.svg)](https://github.com/ops-software/mini-oled-dock/actions/workflows/ci.yml)
[![Latest release](https://img.shields.io/github/v/release/ops-software/mini-oled-dock)](https://github.com/ops-software/mini-oled-dock/releases/latest)
[![License: MIT](https://img.shields.io/badge/license-MIT-blue.svg)](LICENSE)

A minimal, ultra-light, OLED-black floating dock for Windows 11, written in Rust with raw Win32 and no UI framework.

![The bar](docs/screenshots/bar_top_icon_text.png)

It replaces the native taskbar with a small centred pill that hides when you do not need it. Backgrounds are pure black (OLED pixels switch off), the bar shows program names only (window titles appear in the hover previews), tray icons of other apps are hosted in the bar, and every popup is drawn by hand.

- **Tiny:** the executable is about 2.2 MB and uses roughly 11 MB of RAM (about 18 MB while the Start menu is open, mostly released a minute after it closes), trimmed to under 1 MB while hidden (numbers from the author's machine).
- **Pay only for what you use:** a hidden module has no timer, no polling and loads no system library; the network, battery and metrics modules are polled only while the bar is visible.
- **Safe to try:** a watchdog process always restores the native taskbar, and `dock.exe --restore` works even when no dock is running.
- **Nothing changes behind your back:** every new feature is an option, off by default, so an update keeps the dock exactly as you left it. The exceptions of 1.6.0 are the keyboard shortcuts of the bar (on by default, each can be switched off in Settings or `config.json`) and the microphone and location indicators of Windows, which now show in the tray (see `tray_hidden_guids`). A `config.json` written by an older version keeps its 2.5 s hide delay. 1.7.0 adds no new default behaviour of the bar; what it changes without being asked works behind the scenes (the exe asks for no rights itself, the Start catalogue is read by a helper at your own level, a value outside its range in `config.json` reads its default, the look is written to `suite\theme.json`; see the [release notes](.github/release-notes/v1.7.0.md)).

## Screenshots

*Rendered offscreen by `dock.exe --preview` with mock data.*

| Start menu (compact) | Start menu: search | Start menu (Windows 11) |
|---|---|---|
| ![](docs/screenshots/popup_startmenu.png) | ![](docs/screenshots/popup_startmenu_search.png) | ![](docs/screenshots/popup_startmenu_win11.png) |

| Windows 11 layout: all apps as a grid | Windows 11 layout: a folder | Quick-links menu |
|---|---|---|
| ![](docs/screenshots/popup_startmenu_win11_grid.png) | ![](docs/screenshots/popup_startmenu_win11_folder.png) | ![](docs/screenshots/popup_start.png) |

| Notifications | Audio: spatial sound and mixer | Audio: customize mode |
|---|---|---|
| ![](docs/screenshots/popup_notif.png) | ![](docs/screenshots/popup_audio.png) | ![](docs/screenshots/popup_audio_customize.png) |

| Calendar | Network | Power |
|---|---|---|
| ![](docs/screenshots/popup_calendar.png) | ![](docs/screenshots/popup_network.png) | ![](docs/screenshots/popup_power.png) |

| Battery bar (percent beside) | Battery bar (percent inside, charging) | Battery popup |
|---|---|---|
| ![](docs/screenshots/bar_battery.png) | ![](docs/screenshots/bar_battery_inside.png) | ![](docs/screenshots/popup_battery.png) |

| Window previews | Tray: choose visible icons | Settings: Appearance |
|---|---|---|
| ![](docs/screenshots/popup_windows.png) | ![](docs/screenshots/popup_tray_customize.png) | ![](docs/screenshots/settings_appearance.png) |

| Settings: Bar | Settings: Start | Settings: Modules |
|---|---|---|
| ![](docs/screenshots/settings_bar.png) | ![](docs/screenshots/settings_start.png) | ![](docs/screenshots/settings_modules.png) |

| Settings: Screen | Settings: Updates | Settings: About |
|---|---|---|
| ![](docs/screenshots/settings_screen.png) | ![](docs/screenshots/settings_updates.png) | ![](docs/screenshots/settings_about.png) |

## Features

### The bar

- Centred bar sized to its content: pinned apps first, then open apps. An underline marks open windows (orange when an app asks for attention).
- Display modes: icon + name, icon only, or name only.
- Independent sizes for app icons, app names, tray icons and the system modules (the bar height follows the tallest content).
- Background colour from dark presets or any colour through a built-in picker (pure black by default), background opacity from 0 to 100 %.
- Drag and drop reordering of apps, tray icons and system modules.
- Window previews: hovering an app with several windows shows live DWM thumbnails with titles; click one to switch.
- Auto-hide with a slide animation: the bar shows on the Win key, or when the pointer rests on the screen edge for 150 ms (**Show delay**), and hides 1 s after the pointer leaves it (**Hide delay**, 0 to 5 s; a `config.json` written before 1.6 keeps 2.5 s). With **Show at the edge** set to **Push** (off by default) the bar shows when you push the mouse against the edge instead: the pointer has to be on the edge row and move towards the edge by enough raw mouse counts within a second (`push_threshold`, 100 by default); touch, pen and remote sessions never push, and the bar does not show over a full-screen window. A show started from the keyboard or the screen edge stays at least 2.5 s, so a hide delay of 0 never hides a bar you have just opened.
- Keyboard: Win+1...9 and Win+0 act on the first ten apps, Win held for a moment shows their numbers, Win+T, Win+Shift+T and Win+B put the keyboard focus on the bar, and once it is there arrows, Enter, Esc and typing a name work on it (see [Keyboard](#keyboard)).
- Mouse: Shift and middle-click gestures on apps, the wheel on an app cycles its windows, the wheel on empty space sets the volume, and a right click on empty space opens a small dock menu (see [Mouse](#mouse)).
- Show desktop button.

### Win key and Start menu

- **Win toggles the dock** (Win+other-key shortcuts still work), including over fullscreen games.
- **Start menu written from scratch**, on any display: a quick double tap of Win, or a left click on the Start button, opens it next to the Start button on the display under the pointer. It lists every app of the native Start menu (classic programs and Store apps); type to search, right-click an app for its menu (pin, run as administrator, open file location, uninstall).
- **Search like the native Start**: it reads the search data Windows itself ships, so `rdp` finds Remote Desktop Connection and `lol` finds League of Legends, typos are understood, the more popular app comes first, and settings are found too (`bluetooth` opens that page of Settings; apps are always listed before settings). Nothing is a list of ours; where Windows has no entry there is no match.
- **Two Start layouts with different jobs**:
  - *Compact* (the default), a quick launcher: search, one strip of pinned icons (the name shows when the pointer is on one; the wheel slides the strip) and the list of all apps.
  - *Windows 11*, the full menu: one scrolling page with the pinned tiles and, under them, all apps as a list or as a grid; at the bottom the user name and shortcuts to Settings, File Explorer, Documents and Downloads.
  - Neither has a power button: the bar always has one.
- **Folders and letters**: in the Windows 11 layout drag a pinned app onto another to make a folder, onto a folder to add it, out of an open folder to take it out; click a folder's name to rename it. In both layouts drag a pin to the side of another to reorder, and click a letter of "All apps" to jump to another one. **Import from Windows Start** copies your Windows pins in the same order.
- **Win opens Start** (optional): one tap of Win opens the dock and Start together in a single animation.
- **Quick-links menu** on right click of the Start button, like the native Win+X one (Installed apps, Device Manager, Terminal (admin), Task Manager, Run, Sign out, ...).

### Launcher

An optional search box on a shortcut key, **off until you pick one**: Settings → Launcher → Shortcut (Alt+Space or Alt+Shift+Space), or `"launcher": { "chord": "alt+space" }` in `config.json`. The launcher itself adds no hook, timer or process until you pick a shortcut. The Start menu, for everyone, now never waits for a window that has stopped responding (in 1.4.3 a hung foreground program could freeze the dock, and the Windows taskbar then came back), makes its icons and settings list in short-lived copies of `dock.exe`, unloads unneeded COM libraries about 11 minutes later, and until you answer it begins its search results with a row offering the launcher.

- **One box, many kinds**: apps (the native Start catalogue, Store apps included), settings, files and folders, an answer to a sum or a unit conversion, today's date and time (typing "date", "time" or "week" always shows its row), commands (Lock, Sleep, Restart, Shut down, Sign out, Window menu, Clipboard history), a command line or a web search. The best name match comes first; what you pick often rises.
- **Prefixes** (typed at the start; each is a setting): `=` calculator and units, `>` run a command, `<` open windows (switch or close), `??` web search, `$` Windows settings, `)` date and time, `:` Registry Editor, `file ` files only.
- **Keys**: Enter opens, Shift+Enter opens a new instance, Ctrl+Enter shows the file in its folder, Ctrl+C copies, Ctrl+K opens the row's action list, Tab completes, Shift+Delete forgets a row, Esc clears and then closes.
- **Aliases**: your own words for a program, folder, page or link (`launcher.aliases`, at most 64):
  `"aliases": { "docs": "https://example.com", "notes": "app:Microsoft.WindowsNotepad_8wekyb3d8bbwe!App", "work": "D:\\Projects" }`.
  Add one from a row's action list (Assign alias), remove it in Settings → Launcher. Aliases are saved in `config.json`, which other accounts of this PC can read.
- **Files**: names that start with the typed words, from the Windows Search index, in your profile, the Start menu folders and your Desktop, Documents, Downloads, Pictures, Music and Videos folders wherever they are on a local drive (a personal folder set to the top of a drive (`D:\`) is searched only when you list it in `file_folders`; one on a network share is not searched), plus up to 8 folders of your own: `"file_folders": ["D:\\Archive", "E:\\"]` in the `launcher` section of `config.json` (a whole data drive is allowed; never a network path, a short `~1` name, the Windows or Program Files folders or another account's profile). A short-lived helper process searches; it exists only while the launcher is open. It searches with the dock's administrator rights, so in a folder you list in `file_folders` it can show the names of files another account of this PC keeps there: only you and other administrators can change that list (other accounts can only read `config.json`), your personal folders never count a whole drive, and no folder searched may hold or lie in another account's profile, the Windows folder or Program Files. Needs the dock installed (`dock.exe --install`): a dock started from another folder shows no file rows. Off under Administrator protection.
- **Privacy and rights**: what it starts runs at your normal rights through the desktop shell; a program that itself asks for administrator rights shows the usual UAC prompt, and the launcher asks for elevation only when you choose to (Ctrl+Shift+Enter on a row, or Run as administrator in an app's action list), through the same prompt. Nothing you type, copy or pick is logged; the commands you run and your picks are kept per user and can be switched off or cleared; the clipboard is read only when you paste into the box, and a paste marked private is refused. Aliases are kept in `config.json`.
- **Cost** (measured on pre-release builds of 1.5.0 on the developer's machine, Windows 11 build 26300, synthetic keys, while compilers ran on the same PC): per key at most 3.4 ms of work, and a key waited in the queue 0.47 ms at the median and 1.9 ms or less at the 95th percentile; at rest with the launcher on, 36 KB more memory than with it off and the same hidden wake-ups as 1.4.3; peak while open 0.6 to 2.5 MB, given back on close.
- **Run lines with a `%VAR%`** go through `cmd.exe` at your normal rights so the variable is expanded with your own environment; as at a command prompt, Windows' downloaded-file warning does not appear for a program named that way, and a console window may flash. System keys (CapsLock, volume, PrintScreen) pass through to Windows right after the shortcut.
- **PowerToys Command Palette** also wants Alt+Space: the launcher yields while it answers and offers the choice once; turn its shortcut off in PowerToys to take the key. The dock never changes PowerToys.
- **To turn it off**: Settings → Launcher → Shortcut → Off (or `"chord": "off"`).

### Multi-monitor

- The bar can sit on any connected display (Settings → **Screen**: **Main**, the display number, or **Pointer**) and follows that display's DPI; a disconnected display falls back to the primary one.
- With **Pointer**, the Win key or the screen edge of any display brings the bar to that display.

### Popups and modules

- **Notifications:** click one to switch to the app that sent it, X or **Clear all** remove them from Windows' notification history too; Do not disturb.
- **Audio:** volume, mute, output device, spatial sound formats available on the current output (Windows Sonic, Dolby Atmos, ...) and a per-app mixer. Sections can be collapsed, hidden and reordered; a collapsed or hidden section loads nothing.
- **Network:** Wi-Fi signal bars or ethernet, with a globe badge when the internet is reachable; click for name, IPv4 and signal.
- **Battery** (laptops): an Apple-style battery icon with the fill proportional to the charge, a bolt while charging, and the percentage beside the icon or inside it. Orange fill under 20 %, red at 10 % and below. Shown automatically only when Windows reports a battery, so a desktop sees no change; click for charge, state, time remaining on battery and the power plan.
- **System metrics** (off by default): CPU and RAM usage; click opens Task Manager.
- **Calendar**, **power** menu and an **app menu** (new window, pin / unpin, close).
- **Keyboard in every popup:** the popups of the bar (volume, network, battery, notifications, calendar, power, app menu, dock menu, quick links, tray) take Tab, the arrows, Enter and Esc (see [Keyboard](#keyboard)).
- **Tray icon hosting**, with an overflow popup and a view to choose which icons stay in the bar.

### Settings and system

- **Settings window** (power button → **Settings**, Start quick links → **Dock settings**, or `dock.exe --settings`): eight pages (Appearance, Bar, Start, Launcher, Modules, Screen, Updates, About), every option applied live and saved to the commented `config.json`, which also reloads on its own when edited by hand.
- **Language:** System (follows Windows), English or Italian, applied at once, clock and calendar included.
- Autostart without UAC prompts (scheduled task).
- Watchdog process that always restores the native taskbar.

## Install

**Download:** a ready-built `dock.exe` (with its SHA256) is attached to every [release](https://github.com/ops-software/mini-oled-dock/releases/latest). Run `.\dock.exe --install` from the folder you downloaded it to (see below); to update, do the same with the newer exe. Releases are built and tested by GitHub Actions from the tagged source.

**Build from source:** requires Windows 11 and a stable Rust toolchain (1.87+, MSVC target).

```powershell
git clone https://github.com/ops-software/mini-oled-dock
cd mini-oled-dock
cargo build --release
.\target\release\dock.exe
```

The dock needs administrator rights (UAC prompt): a non-elevated process cannot manage windows of elevated apps, receive the Win key over them, or fully manage the native taskbar. Since 1.7.0 the exe itself carries no elevation request (its manifest says asInvoker): started without rights, the dock and the modes that change the machine or the taskbar (`--install`, `--uninstall`, `--rollback`, `--restore`, `--update-check`, `--update-install`) start the same exe again through the consent prompt and, from a copy that is not the installed one (and except for the dock itself, `--update-install` and `--rollback`), wait for it and return its exit code; the logon task still starts the dock elevated with no prompt. `--version`, `--settings`, `--list-apps` and `--preview` run at your own level, with no prompt. The modes the dock starts itself (the watchdog, the update guard) refuse to run without administrator rights, and `--agent` (below) refuses to run with them. Since 1.7.0 the exe also loads DLLs from the Windows system folder only, so a DLL left next to a downloaded copy is never loaded. Apps launched from the dock do NOT inherit the elevated token: they are started through the desktop shell as your normal user (if that is not possible the launch is refused).

To start with Windows (or use the toggle in the power popup):

```powershell
.\target\release\dock.exe --install     # copies the exe to %ProgramFiles%\mini-oled-dock, registers a logon task and an Add/Remove Programs entry, starts the dock
.\target\release\dock.exe --uninstall   # removes the logon task and the Add/Remove Programs entry
.\target\release\dock.exe --version     # prints "mini-oled-dock X.Y.Z"
```

`--install` replaces the installed `dock.exe` in one step: the new copy is first written next to it as `dock.exe.new`, so a failed copy never leaves a half-written file. A running dock is asked to exit, and the installer waits up to 35 s until neither the dock nor its taskbar-restoring watchdog is left. Then the new copy is started (`--no-relaunch` skips that). If a dock is still running when `--install` ends (for example, `--install` run from the installed copy while it runs), it is left running and nothing is restarted. `--quiet` shows no message box, for scripts and unattended runs. Exit codes: `0` success, `1` failure, `2` bad arguments, `3` the running dock could not be stopped.

The logon task runs the dock elevated, so the installed copy lives in an admin-only folder (`%ProgramFiles%\mini-oled-dock\dock.exe`, inheriting the Program Files permissions) that normal programs cannot modify. Its data lives in `%ProgramData%\mini-oled-dock` (see [Data folder](#data-folder)).

**Update:** the dock checks GitHub Releases once a day (`update_check`, one request to api.github.com, nothing is downloaded by the check). When a newer version exists, an orange dot with a count on **Settings** in the power popup shows it. Nothing is installed unless you set `update_auto_install` to `true`: then a verified release is installed when no popup is open and no fullscreen app is in front. The installer downloads `dock.exe` and its `.sha256` into the admin-only folder `%ProgramData%\mini-oled-dock\update\<tag>\`, checks the hash against the `.sha256` published with the release and, when GitHub reports one for the file, against the SHA-256 GitHub itself computed for the upload (a mismatch stops the update), runs the new exe's `--version`, and then hands the install to the update guard (below). No UAC prompt is shown, and the dock restarts from the new copy by itself. Trust rests on HTTPS to github.com, on the hash published with the same release and, when GitHub reports one, on its own digest of the file; the exe is not code-signed.

**Update guard.** Each update is judged by the version you had: it installs the new release, starts it, and puts itself back if the new dock does not keep running with a responsive window (30 s, at most 60 s). Settings > Updates then says "Update to X rolled back" ("Aggiornamento a X annullato"). A rolled-back version is not installed automatically again; **Update now** retries it. The first update from 1.5.0 or older to 1.5.1 is not judged, because those versions have no guard.

**Going back by hand.** The previous version stays as `%ProgramFiles%\mini-oled-dock\dock.prev.exe`. To go back, run `dock.exe --rollback` (elevated; the older version's installer then shows the result, or nothing with `--quiet`); if `dock.exe` does not start at all, run `dock.prev.exe --install`. Going back to 1.5.0 or older: switch off Settings > Updates > Install automatically first, because those versions do not know rolled-back releases and install the newer one again at their next daily check. Settings that only the newer version knows are kept in `config.json` when you go back to 1.5.1 or later; going back to 1.5.0 or older resets them to their defaults at that version's first save of the settings. `--uninstall --remove` deletes `dock.prev.exe` with the rest of the install folder.

Going back to 1.4.x: copy `config.json` first, because 1.4.x drops the launcher settings and aliases the first time it writes the file.

Manual updating still works: run `--install` from a downloaded `dock.exe`. Diagnostics: `dock.exe --update-check` makes one check and prints the result (exit `0` when GitHub answered, `1` when it could not be checked); `dock.exe --update-install <tag>` verifies a release already staged in `update\<tag>\` and starts the update guard for it (run it from the installed `dock.exe`; it accepts no other path); `dock.exe --rollback` is described above.

If you installed an older version, an old copy may be left in `%LOCALAPPDATA%\Dock\bin`: `--install` reports it but does not delete it (an elevated delete in a user-writable folder is unsafe), so delete that folder yourself once the old dock has exited.

**Uninstall:** `--uninstall` removes the logon task and the Add/Remove Programs entry and keeps `%ProgramFiles%\mini-oled-dock` (it may be the copy currently running). `--uninstall --remove`, which Settings → Apps → Installed apps also runs, stops the dock, restores the taskbar (and stops with an error, deleting nothing, if Explorer did not answer: run `dock.exe --restore`, then uninstall again), deletes `%ProgramFiles%\mini-oled-dock` (a file still in use goes at the next restart) and the downloaded updates, and keeps your settings, logs and icons. Since 1.7.0 it also removes the files of the suite's other programs if they are there (`tasks.exe`, `tasks-admin.exe`, `files.exe`, `lang\`, `suite.json`, with their staging and previous copies) and the shortcuts to `tasks.exe` or `files.exe` that sit directly in the all-users Start menu Programs folder; 1.7.0 installs none of them. `--uninstall --remove --purge-data` also deletes `%ProgramData%\mini-oled-dock`. Add `--quiet` to either form for a silent run.

## Usage

- **Show / hide:** Win key alone, or rest the pointer on the screen edge (a bar at the bottom: the bottom edge; at the top: the top edge) for the show delay. With Settings → Screen → **Pointer** the Win key brings the bar to the display under the pointer (if the bar is visible on another display it moves there instead of hiding).
- **Start menu:** double tap the Win key (second tap within 300 ms; the first tap is not delayed) or left click the Start button. Type to search, Enter launches the first result, arrows and the wheel scroll, right click opens the menu of an app (pin / unpin, run as administrator, open file location, uninstall), dragging a pinned app reorders it or (Windows 11 layout) groups it into a folder, a click on a letter of "All apps" opens the letter grid, a click on a folder opens it, the **Grid** / **List** button next to "All apps" (Windows 11 layout) switches the view, Esc closes the open menu, then clears the search, then closes. Right click on the Start button still opens the quick-links menu.
- **Notifications:** click one to switch to its app, X removes it, **Clear all** empties the list and the Windows history.
- **App entries:** left click activates, minimises or cycles windows; middle click or Shift+click starts a new instance; Shift+middle click closes all windows of the app (like **Close N windows** in its menu; Shift must be down at the press and at the release); the wheel over an app activates its previous (wheel up) or next (wheel down) window, skipping minimised ones, at most about eight steps a second; right click opens the app menu (the app's own jump list as in the Windows taskbar - its tasks and categories, without the Recent / Frequent files - then New window, Pin / Unpin, Close); resting the pointer on an app with several windows shows a preview of each (click one to switch to it).
- **Volume:** the wheel changes it by 2 % a notch (over the volume icon or over empty space of the bar; wheel down never unmutes, Ctrl+wheel does nothing), middle click mutes, left click opens the audio popup (output, spatial sound, per-app mixer). Click a section header to collapse it; the pencil at the bottom enters Customize mode (eye = hide / show, drag a header to reorder).
- **Network:** left click opens the network popup (connection, internet, IPv4, signal, "Network settings"); right click opens Windows' network settings.
- **Battery:** left click opens the battery popup (charge, state, time remaining on battery, power plan, "Power & battery settings"); right click opens Windows' battery saver settings. The wheel is not used.
- **System metrics:** click opens Task Manager.
- **Settings window:** power button, then **Settings** (or Start quick links → **Dock settings**, or `dock.exe --settings`). Resizable, with a dark title bar in the bar's colour and a navigation on the left; `Esc` closes, `Tab` moves between the navigation and the page, `Up`/`Down` change page.
  - **Appearance:** a live preview of the bar, the background colour (seven dark presets: Black, Graphite, Slate, Navy, Plum, Wine, Forest, and a custom swatch that opens a colour picker with saturation/value square, hue strip, preview and hex code), the **Opacity** slider (0-100 %, background only: at 0 % only text and icons are visible and the bar still reacts to the mouse), sliders for app icons, app names, tray icons and modules, **Reset sizes**, and the language.
  - **Bar:** position, display mode, auto-hide, **Show delay** (0-1000 ms, 50 ms steps), **Hide delay** (0-5000 ms, 100 ms steps), **Show at the edge** (**Rest** or **Push**), window previews, clock seconds, volume percent, and **Keyboard shortcuts** (one switch for the four `keyboard` options: on sets all four to on, off sets all four to off; it shows on when any of them is on).
  - **Launcher:** the shortcut, where it opens, what it searches, prefixes, aliases and history (see [Launcher](#launcher)).
  - **Start:** the Start button, the Start layout, Win key opens Start, and the import of pinned apps from Windows Start.
  - **Modules:** one switch per module, the **Battery** group (show: Auto, On or Off; percentage; percentage inside the icon), the **Tray** group (tray icons, **Icons of apps run as administrator**, see [Tray icons of elevated apps](#configuration)) and the tray icon chooser, inline.
  - **Screen:** which display the bar sits on (**Main**, the display number, or **Pointer**).
  - **Updates:** installed and available version, last check, **Check now**, **Release notes**, **Update now** (also retries a release that was rolled back), and the switches for the daily check and the automatic install.
  - **About:** version, memory, the data folder and the log, and **Exit dock** (restores the Windows taskbar).

  Sliders follow a drag or a click on the track; the mouse wheel scrolls the page. Every change applies at once and is saved to `config.json` (the comments are kept); while you drag, the file is written when you release the button. The window remembers its size (`settings_window_size`).
- **Updates:** an orange dot on Settings in the power popup means a new release is available.
- **Exit:** `Ctrl+Alt+Q`, or the power popup, or `dock.exe --restore`.

### Keyboard

The dock owns these Win-key chords **only while it hides the native taskbar** (`hide_native_taskbar` on and no `--keep-taskbar`); otherwise Windows keeps them. Settings → Bar → **Keyboard shortcuts** is one switch for all four options below. To switch one off alone, use `keyboard` in `config.json` (below); `"keyboard": false` switches all four off and `"keyboard": true` all four on.

| Keys | What it does | Option |
|---|---|---|
| Win+1 ... Win+9, Win+0 | Acts on the 1st to 10th app of the bar (pinned apps, then open ones), like a left click: start it, bring it up, minimise it, or cycle its windows. Nothing happens when that slot holds no app. | `win_numbers` |
| Win+Shift+digit | A new instance of that app (like a middle click) | `win_numbers` |
| Win+Alt+digit | Gives the bar the keyboard and opens that app's menu | `win_numbers` |
| Win+T | Gives the bar the keyboard, focus on the first app | `win_t` |
| Win+Shift+T | The same, focus on the last app | `win_t` |
| Win+B | The same, focus on the first tray icon (the first module when the bar has no tray icons) | `win_b` |
| Win held for about 0.6 s, alone | **Peek:** shows the bar if it is hidden and draws the digits 1 to 9 and 0 on the first ten apps; releasing Win hides a bar that Peek showed (unless the pointer or the keyboard is on it). Not over a full-screen window; a bar Peek showed hides by itself after 10 s at most. Needs `win_numbers` too. | `peek` |

A tap of Win, a double tap, Win+other keys that are not in the table (Win+E, Win+D, Win+R, ...) and the launcher shortcut behave as before. A chord key is swallowed with its auto-repeat, so a digit held after Win is released never types. If the dock's window does not answer a chord for 1.5 s, the chords pass to Windows again.

While Remote Desktop Connection, the Remote Desktop client of the Windows App or Azure Virtual Desktop, or Hyper-V Virtual Machine Connection is in front, the chords and Peek go to the remote session, as in 1.5.2. The same holds for a UAC prompt shown without the secure desktop and for a window of a program the dock is not allowed to inspect. After a UAC prompt, a lock, sleep or a remote connect or disconnect, the dock forgets a Win, Shift or Alt it could not see released, so a digit, T or B typed right after is typed normally. The dock also notices by itself when its keyboard hook was not served in time (the PC froze for a moment or was overloaded): a keyboard callback that took too long, a key that arrived late, or, while Win is held after a dock shortcut, a short check that came late. It then forgets the Win key it believed held. If Win really is still held at that moment, the next digit is typed once as a plain character; press Win again and it works at once. The short check runs only while Win is held after a dock shortcut.

Limits:
- If you keep holding Win after a dock shortcut and pause for more than about 1.5 s, the next key that is not a dock shortcut (L, D, E, an arrow) is typed as a plain key instead of working with Win, so Win+L does not lock. Dock shortcuts (digits, T, B) keep working for 10 s after the last one. Release Win and press the whole combination again.
- If Windows loses the release of Win while the PC is frozen for a moment, keys typed in the next seconds can still be taken as dock shortcuts in two cases the stall check does not cover: a Win press not yet used for a dock shortcut, and a Win press used for a Windows shortcut such as Win+E. The dock clears this by itself, after about 2 s in the second case and about 12 s at most in the first.
- Win+Shift+T is also the default shortcut of PowerToys Text Extractor, and the dock owns it. Switch `win_t` off, or change the shortcut in PowerToys. The same holds for any other program that uses one of the chords above.
- A Win press held 0.6 s or longer and then released shows and hides the bar (Peek) and does not toggle the dock; with `win_opens_start` on, Start does not open after such a slow press. If you press slowly, or use Filter Keys, switch `peek` off.

**On the bar.** Win+T, Win+Shift+T, Win+B and Win+Alt+digit show a hidden bar (it stays at least 2.5 s) and give it the keyboard; a ring marks the focused item. Moving the mouse hides the ring. A navigation key pressed on the bar gives it the keyboard too. After a plain tap of Win the bar has the focus but not the keyboard: typing starts no type-ahead, and the bar hides as usual. With no key for 4 s and the pointer away from the bar, the keyboard on the bar ends (with auto-hide off too) and the previous window gets the focus back; time in a popup does not count.

| Key | What it does |
|---|---|
| Left / Right | Previous / next item, wrapping around the whole bar |
| Home / End | First / last item of the current group |
| Tab / Shift+Tab | The first item of the next / previous group (Start button, apps, tray icons, modules) |
| Enter or Space | Left click on the item |
| Shift+Enter | Middle click (a new instance for an app, mute for volume) |
| Menu key or Shift+F10 | Right click |
| Up (Down when the bar is at the top) | An app with two or more windows (and `window_previews` on): the window picker; any other app: its menu; the other items: their popup. The popup starts with the keyboard on its first control. |
| Ctrl+Left / Ctrl+Right | Moves the item one place inside its group and saves the order, like a drag |
| Letters | Type-ahead (only once the bar has the keyboard on purpose, see above): focus jumps to the next app whose name starts with what you typed (a pause of 1 s starts a new word; pressing the same letter again cycles through the apps that start with it) |
| Esc | Hides the bar (with auto-hide on), or gives the focus back to the window that had it |

In the window picker opened from the keyboard: the arrows move the selection, Enter switches to that window, Delete closes it, Esc closes the picker. Holding a key repeats only the keys that move (arrows, Home, End, Tab).

**In popups.** Tab / Down and Shift+Tab / Up move between the controls (Home and End jump to the first and last), Enter or Space presses the control, Esc closes the popup. A held Enter or Space acts once; only the keys that move repeat. The key that opened the popup (Up, Down, Menu key, F10) does not repeat inside it until you press it again. In the volume popup Left and Right on a slider change it by 2 % (10 % with Shift). The Start menu and the launcher keep their own keys, described above.

### Mouse

| On | Click | Middle | Right | Shift+click | Shift+middle | Wheel |
|---|---|---|---|---|---|---|
| App | Activate, minimise or cycle | New instance | App menu | New instance | Close every window of the app | Up: previous window, down: next |
| Empty space of the bar | | | Dock menu | | | Volume +2 % / -2 % |
| Start button | Start menu | | Quick-links menu | as click | | |
| Volume | Audio popup | Mute | Windows sound settings | as click | as middle | Volume +2 % / -2 % |
| Network, battery | Popup | | Windows network / battery saver settings | as click | | |
| Notifications, clock, power, overflow chevron | Popup | | | as click | | |
| System metrics | Task Manager | | | as click | | |
| Show desktop | Show desktop | | | as click | | |
| Tray icon | Left click of the icon | Middle click of the icon | Right click of the icon | as click | as middle | |

Shift changes only apps; on every other part it is the same as without Shift. Shift+middle closes the windows of an app only if Shift is down at the press and at the release. The wheel is ignored with Ctrl held (over the volume icon too) and, while a popup is open, everywhere except the volume item. Wheel down never unmutes; wheel up on a muted PC does. The **dock menu** (right click on empty space of the bar) has a header "Dock" and three rows: **Auto-hide** (a check shows it is on; click to switch it), **Task Manager** and **Settings**.

## Configuration

`%ProgramData%\mini-oled-dock\config.json` is created on first start as commented JSON (`//` and `/* */` allowed) with a guide above every option. It reloads automatically within about 2 s. Missing fields take their default; on a syntax error the previous settings are kept and the error is logged. Since 1.7.0 a value of the wrong type, or outside the range Settings gives that option (a delay, a size, the opacity and the like), loses only its own key: that key reads its default (at a reload, its last good value), the rest of the file loads, and the log says which key and where. For the keys new in 1.6 (`hide_delay_ms`, `show_delay_ms`, `push_threshold`, `keyboard` and its four switches, `tray_privacy_migrated`) Settings → Bar also shows a note at the Auto-hide row until the next save. The file the dock writes has the same keys and layout as 1.6.0's (only the guide line above `reveal` changed).

**Editing by hand:** open it with Notepad (no admin rights needed). Notepad saves in place, which is allowed; editors that save by writing a new file and renaming it over the old one (many code editors do) cannot save here, so use Notepad or run your editor elevated. If a programmatic save happens while your edit has a syntax error, your version is first copied to `config.json.bak` next to it.

| Option | Default | Meaning |
|---|---|---|
| `position` | `"bottom"` | `"top"` or `"bottom"` |
| `monitor` | `""` | Display the bar sits on: `""` = primary monitor, or a device name such as `"\\\\.\\DISPLAY2"`. A display that is not connected falls back to the primary one. `"pointer"` = the Win key brings the bar to the display under the mouse pointer |
| `display_mode` | `"icon_text"` | `"icon_text"`, `"icon"` or `"text"` |
| `app_icon_size` | `16` | App icon size in logical px, 14-32 |
| `app_text_size` | `13` | App name text size in logical px, 10-18 |
| `tray_icon_size` | `16` | Tray icon size in logical px, 12-24 |
| `module_size` | `100` | Scale of the Start button and the system modules (notifications, volume, network, battery, metrics, clock, show desktop, power) in percent, 75-150 |
| `background` | `"#000000"` | Background colour of the bar and the popups, `"#RRGGBB"` (the old values `"black"` and `"dark"` are still accepted) |
| `bar_opacity` | `95` | Bar background opacity in percent, 0-100 (text and icons stay opaque) |
| `autohide` | `true` | Hide the bar when the pointer leaves it |
| `show_delay_ms` | `150` | Auto-hide: how long the pointer must rest on the screen edge before the hidden bar shows, 0-1000 ms (Settings → Bar → **Show delay**). Since 1.7.0 a value outside the range is an error: the key reads its default and the rest of the file loads |
| `hide_delay_ms` | `1000` in a new file, `2500` when the key is missing | Auto-hide: how long the bar stays after the pointer leaves it, 0-5000 ms (Settings → Bar → **Hide delay**). A file written before 1.6 has no such key and keeps 2.5 s; a value outside the range reads that default (2.5 s). A show by keyboard or screen edge lasts at least 2.5 s whatever this value is |
| `reveal` | `"dwell"` | How the hidden bar shows at the screen edge (Settings → Bar → **Show at the edge**: **Rest** or **Push**): `"dwell"` = rest the pointer there for the show delay; `"push"` = push the mouse against the edge (see the auto-hide line under [The bar](#the-bar)). Any other value reads `"dwell"` |
| `push_threshold` | `100` | Push: how many raw mouse counts towards the edge, within one second, show the bar; 20-1000, a value outside is clamped. No Settings row |
| `keyboard` | `{ "win_numbers": true, "win_t": true, "win_b": true, "peek": true }` | The Win-key chords the dock owns, see [Keyboard](#keyboard): `win_numbers` Win+1...9/0 (also with Shift and Alt), `win_t` Win+T and Win+Shift+T, `win_b` Win+B, `peek` the digit numbers shown while Win is held (needs `win_numbers`). A key you leave out stays on. `true` or `false` alone (`"keyboard": false`) sets all four. A wrong type in one switch is ignored (it stays on). Settings has one switch for all four. All off while `hide_native_taskbar` is `false`; the chords also go to remote-desktop and VM windows and a UAC prompt |
| `window_previews` | `true` | Live previews of an app's windows when the pointer rests on an app with two or more |
| `hide_native_taskbar` | `true` | Hide the Windows taskbar while the dock runs |
| `show_date` / `date_format` | `true` / `"ddd d MMM"` | Date under the time (Windows date picture) |
| `time_format` / `show_seconds` | `"HH:mm"` / `false` | Clock format (Windows time picture) |
| `show_volume_percent` | `true` | Show `NN%` next to the speaker |
| `show_tray` | `true` | Host other apps' tray icons (switching it off stops the tray host; the icons go back to Explorer) |
| `show_network` | `true` | Network module: Wi-Fi bars / ethernet, internet badge, details popup |
| `show_battery` | `"auto"` | Battery module: `"auto"` (only when the PC has a battery), `true` (always) or `false` (never). Any other value is read as `"auto"`, logged, and Settings → Modules says so next to the row until the file holds a valid one |
| `battery_percent` | `true` | Show the charge percentage with the battery icon |
| `battery_percent_inside` | `false` | Draw the percentage inside a larger battery icon instead of beside it |
| `show_metrics` | `false` | CPU / RAM usage module (click opens Task Manager) |
| `show_notifications` / `show_volume` / `show_clock` / `show_desktop_button` | `true` | Show the other system modules (a hidden module costs nothing) |
| `show_start_button` | `true` | Start button at the left end of the bar (a fixed item, not a movable module) |
| `tray_hidden_guids` | the volume, network and power icons | System tray icons (GUIDs, lowercase, no braces) the dock never shows in its bar, because it draws volume, network and power itself. Since 1.6.0 the microphone and location indicators of Windows are not in the list, so they show in the tray (hide one with Settings → Modules → Tray, or add `7820ae82-23e3-4229-82c1-e41cb67d5b9c` (microphone) or `7820ae77-23e3-4229-82c1-e41cb67d5b9c` (location) here). A file that still holds exactly the list of 1.5.x and older (those three plus the two indicators) is switched once to the new list when 1.6.0 first reads it, and rewritten; any other list is yours and is kept. A list restored by hand later is not migrated again. After a rollback to 1.5.x the two indicators show in the tray; hide them again in Settings → Modules → Tray. `tray_privacy_migrated` records that this ran: leave it as it is (setting it to `true` by hand turns the migration off) |
| `pinned` | `[]` | Pinned apps, in display order |
| `start_pins` | `[]` | Apps pinned at the top of the dock's Start menu (separate from `pinned`; right click an app in the menu to add or remove it) |
| `start_folders` | `[]` | Folders of those pins: `[{ "name": "Games", "apps": ["<id>", ...] }]`. Managed by dragging pinned apps in the menu |
| `names` | `{}` | Display-name overrides (key: lowercase exe name or AUMID) |
| `language` | `"auto"` | Dock language: `"auto"` (Windows display language), `"en"` or `"it"` |
| `win_opens_start` | `false` | One tap of Win opens the dock and the Start menu together |
| `start_style` | `"compact"` | Start menu layout: `"compact"` (quick launcher) or `"windows11"` (full menu); the old names `"vertical"` / `"horizontal"` are still read |
| `start_grid` | `false` | Windows 11 layout: "All apps" as a grid of icons instead of a list (the button next to "All apps") |
| `audio_layout` | `"MOSX"` | Volume popup sections in order (M master, O output, S spatial, X mixer; lowercase = collapsed, `-` in front = hidden); normally set from the popup |
| `update_check` | `true` | Ask GitHub once a day whether a newer release exists (badge only); the request is made by a short-lived copy of `dock.exe` (`--update-fetch`) that exits at once |
| `update_auto_install` | `false` | Download, verify and install new releases by themselves |
| `launcher` | `{ "chord": "off", ... }` | The launcher (see Launcher above). `chord`: `"off"`, `"alt+space"` or `"alt+shift+space"`; `enabled`; `with_powertoys` (`"yield"` or `"take"`); `monitor` (`"pointer"`, `"active"`, `"primary"`); `top_percent` 10-50; `width` 560-880; `rows` 4-12; `keep_query`; `over_fullscreen` (`"pass"` or `"open"`); `learn`; `providers` (switch each source on or off); `prefixes`; `aliases`; `file_folders` (up to 8 more folders for file search, e.g. `["D:\\Archive", "E:\\"]`; file search already covers your profile, the Start menu folders and your Desktop, Documents, Downloads, Pictures, Music and Videos folders wherever they are on a local drive (a personal folder set to the top of a drive (`D:\`) only when you list it here), and only what Windows Search has indexed; a whole data drive is allowed, network paths, the Windows and Program Files folders and other accounts' profiles never are, a refused entry is ignored and counted in `dock.log`; the search runs with the dock's administrator rights, so it can show the names of files other accounts keep in a folder listed here); `web_engine` (`"google"`, `"bing"`, `"duckduckgo"`, `"custom"`) and `web_url` for `"custom"` |

`last_update_check` and `update_available` are written by the dock itself. A top-level key this version does not know (one a newer version wrote) is kept as it is when the dock saves the file. Hand edits to `config.json` are applied when the file changes; the dock no longer polls it every 2 seconds (it polls again only if the change watch fails).

The bar height follows the tallest content; the defaults give the original 36 px bar. An old `bar_size` value (`"small"`, `"medium"`, `"large"`) is converted once into the four sizes.

`tray_show_all` (default `false`) shows every tray icon in the bar: `tray_hidden` is then ignored but left in the file, so switching it off brings your earlier choices back (Settings, Modules, Tray, "Show all"). Release 1.5.1 does not know this key and keeps it in the file untouched.

Tray visibility and ordering (`tray_hidden`, `app_order`, `tray_order`, `modules_order`) are written by the dock itself. A tray icon with no GUID is keyed by its program and its place among that program's icons (`name.exe|0`), so a program that picks a new internal id at every start keeps its hidden and ordered position; the keys of 1.5.0 and older (`name.exe#id`) are rewritten the first time the program's icons are seen. 1.5.0 and older do not know the new keys: there, such icons hidden after the change show again and those ordered after it return to the default order. Icons with a GUID are not affected.

**Tray icons of elevated apps.** The dock runs as administrator, so by default it refuses to send click, hover and menu messages to a tray icon whose window belongs to another elevated program (for example MSI Afterburner run as administrator): a tray registration can come from any program and names the window and the message itself, and Windows does not tell the dock which program sent it, so otherwise any normal program could register a fake icon pointing at an elevated window and use the dock to deliver messages Windows would block (a UIPI bypass). To use such icons, turn on **Settings → Modules → Tray → Icons of apps run as administrator**. The switch is not in `config.json` (any program running as you can write that file): it is the admin-only registry value `HKLM\SOFTWARE\mini-oled-dock\TrustElevatedTrayIcons` (DWORD, 1 = on), which the elevated dock writes when you flip the toggle and which takes effect at once. It can also be set by hand from an elevated PowerShell (picked up within about 10 s; `0` or deleting the key turns it off):

```powershell
New-Item -Path HKLM:\SOFTWARE\mini-oled-dock -Force; Set-ItemProperty HKLM:\SOFTWARE\mini-oled-dock TrustElevatedTrayIcons 1 -Type DWord
```

Even with the switch on, the dock sends a message to an elevated window only if: the window still belongs to the process that owned it when the icon was registered, and that process still runs the same program file the dock recorded for the icon then; and the message is an application message (`WM_USER` to `0xFFFF`: private, `WM_APP` and registered messages), never a system message below `WM_USER` such as `WM_COMMAND` or `WM_SYSCOMMAND`; and the window is a top-level or message-only window, never a child control (controls read `WM_USER`-range messages as commands). What remains possible with the switch on: a normal program can still make the dock send an application message (`WM_USER` to `0xFFFF`), with a value it partly chooses, to a window of a program running as administrator, when you click or hover an icon it registered. The process and program-file checks only stop a closed program's window or process id from being reused; they do not limit which elevated window can be named. Turn the switch on only if you trust the programs you run. Icons of programs that do not run as administrator are not affected by the switch. A refused click is logged once per window in `dock.log`, naming the Settings option and the registry value.

A pinned app looks like this; `launch` is an exe path or `shell:AppsFolder\<AUMID>` (list AUMIDs with `Get-StartApps`), `icon` is an optional PNG in `%ProgramData%\mini-oled-dock\icons\` (you can drop files there without admin rights; at most 4 MB, no shortcuts, links or hard links), given as a plain file name only (paths, `..`, drive letters and UNC paths are rejected). Pinning from the right-click menu fills it in for you. Apps are always started without administrator rights, through the desktop shell, even though the dock itself is elevated; if that is not possible the launch is refused rather than run elevated. The two admin entries of the Start quick-links menu (Terminal and PowerShell) ask Explorer to start them with the `runas` verb, so they go through the normal UAC prompt; the dock never uses its own token.

```jsonc
{ "key": "c:\\program files\\discord\\discord.exe", "name": "Discord",
  "launch": "C:\\Program Files\\Discord\\Discord.exe", "icon": "discord.png" }
```

Run `dock.exe --list-apps` to see how each open window was named. Command-line flags (`--top`, `--bottom`, `--no-autohide`, `--keep-taskbar`, ...) override the config; `dock.exe --help` lists them all.

## Recovering the taskbar

The dock saves the original taskbar state before hiding it, and a separate watchdog process restores it if the dock crashes or hangs.

The watchdog restores the taskbar when the dock stops answering for more than about 6.6 seconds. If the dock answers again, it is meant to hide the taskbar again at its next safety check, as long as its watchdog is still running; it does not do so after `--restore`, `--install`, `--uninstall` or Exit. Explorer's own saved auto-hide setting is set back to off after a restore, so a taskbar that comes back after an uninstall is not left in auto-hide. At a sign-out or shutdown the saved taskbar state is meant to be kept for the next sign-in.

If something still goes wrong:

1. Run `dock.exe --restore` from any terminal. It works even when no dock is running.
2. If the taskbar is still hidden, restart Explorer:

```powershell
Stop-Process -Name explorer -Force; Start-Process explorer
```

## Limitations

- One bar, on one display at a time (the primary monitor unless another is chosen in Settings, or the display under the pointer with **Pointer**); the dock does not reserve work area, so maximised windows go under the bar.
- The notification list needs Windows notification access (asked once). Windows does not let an unpackaged app activate or dismiss another app's notification directly: a click on a notification brings up the app that sent it (its window, or a launch), and Clear all / X go through each app's notification history. Switching the audio output and Do not disturb use undocumented COM interfaces that a Windows update could break.
- Tray hosting only works while the native taskbar is hidden.
- Developed and tested on Windows 11 only.
- Since 1.7.0 the shell work on files a program of yours could plant (the Start apps list and Start's icons, the targets and names of per-user Start menu shortcuts, version strings and icons of programs you can write to) is done by a helper, `dock.exe --agent <nonce>`, which the dock starts through Explorer so that it runs as you at normal integrity. It starts when that work is first needed, talks to the dock through two named pipes with a random name that only SYSTEM and the user of the desktop shell can open (each side checks that the other end is this same exe, in the same session, at the right integrity level), ends after 30 seconds without work, and is replaced by the old way (a child of the dock, then the dock itself, both elevated) when it cannot serve. It never starts when the desktop shell itself runs elevated (UAC off). Three failures within ten minutes stop the dock from trying again until it restarts; the log counts which way each listing was served. The old way is also still used for the notification icons, the names of the volume mixer and of tray icons, version strings when the helper does not answer within its deadline, and the Apps folder name and path of an app: there the elevated dock has the Windows shell read a file a program running as you could plant. This is a known, accepted residual.

## Data folder

Everything the dock stores is in `%ProgramData%\mini-oled-dock`:

| Path | What | Who can write |
|---|---|---|
| `config.json` (+ `config.json.bak`) | settings | you (edit in place) and administrators; the `.bak` copy is admin-only |
| `icons\` | your custom PNGs | you can add, replace and delete files |
| `cache\` | extracted icon cache (PNG); `settings-catalog.bin`, the Windows Settings list that the launcher and Start search read (rebuilt when stale or damaged); `cache\<SID>\` holds one user's launcher history (commands run, picks as hashes), readable by that user and administrators | administrators only |
| `update\<tag>\` | downloaded update (`dock.exe`, `dock.exe.sha256`), `dock-guard.exe` (the copy of the old version that judges the update) and `rolled-back` (written when that release was rolled back); deleted once that version or a newer one runs | administrators only |
| `suite\theme.json` | the dock's look (language, background colour, opacity, the four sizes) and the dock's version, written by the elevated dock at its start and whenever the look changes, for the suite's apps to come; nothing in 1.7.0 reads it. Any user of the PC can read it; it holds nothing about you. Delete it and the dock writes it again | administrators only |
| `journal\` | an empty folder that an elevated start creates; nothing in 1.7.0 uses it | administrators only |
| `dock.log`, `dock.log.1` | log, rotated at 2 MB; one local clock for the dock and its watchdog; it carries no window titles or tooltips (a window is named by its class) | administrators only |
| `taskbar-state` | the original taskbar state, used to restore it; kept across a sign-out or shutdown and deleted by every other restore | administrators only |

The dock runs elevated, so its data must not live where normal programs can plant links: an elevated write, rename or delete that follows a junction or hard link planted in a user-writable folder (such as the old `%LOCALAPPDATA%\Dock`) can be turned into an arbitrary file write or delete. The folder is therefore created by the elevated dock with its own protected permissions (administrators and SYSTEM full control, users read only); only `config.json` (read and write its content, not delete or rename it) and `icons\` (add files) are opened up for you, and custom icons are read with checks that refuse links and anything outside that folder. If the folder exists but was not created by an administrator (or is a link), the dock renames it away or, failing that, refuses it and runs without saving anything; the log (or DebugView, when there is no log) says to delete it from an elevated prompt.

**Upgrading from an older version:** on the first start the old `%LOCALAPPDATA%\Dock\config.json` is read once and saved in the new folder. Custom icons are not copied: move your PNGs from `%LOCALAPPDATA%\Dock\icons` to `%ProgramData%\mini-oled-dock\icons` yourself, then delete `%LOCALAPPDATA%\Dock` (the dock never writes or deletes anything there).

## Build notes

Every build carries the same asInvoker manifest (see [Install](#install)); `DOCK_NO_MANIFEST` is still accepted and has no effect. To render the screenshots, run `dock.exe --preview <folder>` without administrator rights.

## Releases

| Version | Date | Highlights |
|---|---|---|
| [1.7.0](.github/release-notes/v1.7.0.md) | unreleased | The exe asks for no rights itself and each mode gets the level it needs, a helper at your own level reads the Start catalogue and icons, Push reveal at the screen edge, a wrong value in `config.json` loses only its own key, the look is written to `suite\theme.json`, the loader reads system DLLs only |
| [1.6.0](.github/release-notes/v1.6.0.md) | unreleased | The keyboard: Win+digit chords, Peek, focus and type-ahead on the bar, keys in every popup; Shift gestures, wheel cycle and volume, the dock menu; show and hide delays; the microphone and location indicators show in the tray |
| [1.5.2](.github/release-notes/v1.5.2.md) | 2026-11-24 | Tray "Show all", more Start menu shortcut names, a dead watchdog is replaced, installer closes restore the taskbar |
| [1.5.1](.github/release-notes/v1.5.1.md) | 2026-11-24 | A quieter hidden dock, the updater checks GitHub's digest and puts the old version back when a new one fails, `--rollback`, the taskbar meant to be hidden again after a watchdog restore, no titles in the log |
| [1.5.0](.github/release-notes/v1.5.0.md) | 2026-11-23 | The launcher (off by default), no freeze on a hung foreground window, taskbar edge fixes, full third-party licence texts |
| [1.4.3](.github/release-notes/v1.4.3.md) | 2026-11-22 | Taskbar auto-hide: checks whether another bar holds the screen edge first |
| [1.4.2](.github/release-notes/v1.4.2.md) | 2026-11-12 | Hotfix: the self-updater can download its first update |
| [1.4.1](.github/release-notes/v1.4.1.md) | 2026-11-12 | Start menu rework, the app's jump list on right click, pins survive app updates |
| [1.4.0](.github/release-notes/v1.4.0.md) | 2026-11-09 | Self-updater, battery module, Settings window, review fixes |
| [1.3.0](.github/release-notes/v1.3.0.md) | 2026-10-29 | Keeps working across Explorer restarts, multi-monitor edge fix, −511 lines de-duplication |
| [1.2.1](.github/release-notes/v1.2.1.md) | 2026-10-29 | Hotfix: the first click on a popup is no longer lost |
| [1.2.0](.github/release-notes/v1.2.0.md) | 2026-10-29 | Horizontal Start, Win opens Start, IT/EN language, spatial sound and mixer, customizable volume popup |
| [1.1.0](.github/release-notes/v1.1.0.md) | 2026-10-29 | Start menu from scratch, Win follows the pointer, working notifications, window previews, multi-monitor |
| 1.0.0 | 2026-10-23 | First release |

Downloads are on the [releases page](https://github.com/ops-software/mini-oled-dock/releases). A detailed Italian write-up of the 1.2.0 changes is in [docs/MODIFICHE-2026-10-29.md](docs/MODIFICHE-2026-10-29.md).

## Contributing

`main` is protected: work on a branch and open a pull request. CI (`.github/workflows/ci.yml`) runs on every pull request on Windows: `cargo test --locked` with `DOCK_NO_MANIFEST=1` (so tests run without a UAC prompt) and a `cargo build --release --locked`. Run the same locally before pushing, and `cargo fmt` (see `rustfmt.toml`).

- Keep the default behaviour unchanged: a new feature is an option, off by default.
- Keep the memory budget around 10 MB: load data only when needed, no new permanent timers or threads.
- If a change touches drawing, render the previews (`dock.exe --preview <folder>`) before and after and compare them.

**Releasing:** bump `version` in `Cargo.toml`, add `.github/release-notes/vX.Y.Z.md`, merge, then push the tag `vX.Y.Z`. Before tagging, run `pwsh tools/screenshots.ps1` and commit the refreshed screenshots. The release workflow checks that the tag matches `Cargo.toml`, builds `dock.exe` and publishes it with its SHA256.

## Credits and license

Created by [gitgiovik](https://github.com/gitgiovik). Maintained by [Ops Software](https://github.com/ops-software).

Huge thanks to the contributors:

- [Desox](https://github.com/Desox96): the Start button, quick-links menu and window previews; tray menu placement and foreground fixes, Win opening Start, the IT/EN language switch, spatial audio and the per-app mixer, the customizable volume popup and the horizontal Start layout.
- [Ragnatela666](https://github.com/Ragnatela666): multi-monitor screen choice, the AppBar position fix, the Start menu written from scratch, Win following the pointer, working notifications, CI and the release workflow, releases 1.1.0 to 1.3.0.
- [VanheXen](https://github.com/VanheXen): the lost first click fix, the multi-monitor edge fix, the de-duplication refactor (over 500 lines removed with byte-identical rendering) and the Explorer restart fix.

The 1.1.0-1.3.0 work was developed in [Ragnatela666/mini-oled-dock](https://github.com/Ragnatela666/mini-oled-dock), where the pull requests linked in the release notes live.

A few Windows shell protocol structures in `src/tray_host.rs` are adapted from [lightbar](https://github.com/SauerExe/lightbar) (MIT); see [THIRD_PARTY_NOTICES.md](THIRD_PARTY_NOTICES.md). Other shell projects were studied for protocol knowledge only; no AGPL code was copied.

[MIT](LICENSE) &copy; 2026 gitgiovik
