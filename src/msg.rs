//! `WM_APP + N` messages, timer ids and menu command ids shared between modules.
//!
//! Rule: every message in this list is addressed to the dock window (UI thread)
//! and is posted with `PostMessageW` (never `SendMessage` from other threads: it would block).
//! This is the ONLY place where numbers are assigned; whoever adds a message puts it here.

use windows::Win32::UI::WindowsAndMessaging::WM_APP;

// ------------------------------------------------- messages to the dock window

/// hook -> UI: the Win key was pressed and released on its own, toggle the dock.
/// wParam = 0, lParam = 0. Posted by the `hook` thread.
pub const WM_DOCK_TOGGLE: u32 = WM_APP + 1;
/// hook -> UI, log only. wParam = `hook::EV_*` (PRESSED/ALONE/COMBO/SENDFAIL), lParam = 0.
pub const WM_DOCK_WINKEY: u32 = WM_APP + 2;
/// hook -> UI, diagnostics (`--diag`). wParam = bitfield described in `hook::diag`,
/// lParam = HWND of the foreground window (as isize).
pub const WM_DOCK_DIAG: u32 = WM_APP + 3;
/// apps (WinEvent callback, run on the UI thread) -> UI: the window list may have
/// changed. wParam = WinEvent event code (EVENT_*), lParam = HWND (isize).
/// The UI thread starts (or restarts) the `TIMER_APPS_DEBOUNCE` debounce timer (100 ms).
pub const WM_DOCK_APPS_DIRTY: u32 = WM_APP + 4;
/// taskbar (WinEvent callback, UI thread) -> UI: a native taskbar-class window was
/// shown/created. wParam = event code, lParam = HWND (isize). The UI calls
/// `taskbar::on_winevent(event, hwnd)`.
pub const WM_DOCK_TASKBAR_EVENT: u32 = WM_APP + 5;
/// shell thread -> UI: there are new `ShellEvent`s in the queue. wParam = 0, lParam = 0.
/// The UI calls `shell_thread::drain_events()` (non-blocking) and applies them.
/// Consecutive posts may be coalesced: ALWAYS drain the whole queue.
pub const WM_DOCK_SHELL_READY: u32 = WM_APP + 6;
/// audio (COM callback, arbitrary thread) / shell thread -> UI: the volume or mute state changed.
/// wParam = master level 0..=100 (rounded); lParam = 1 if muted, 0 otherwise,
/// -1 = no render device available (wParam ignored).
pub const WM_DOCK_VOLUME: u32 = WM_APP + 7;
/// watchdog (`stop-watch` thread) -> UI: another `--restore` instance asked to quit.
/// wParam = 0, lParam = 0. The UI performs the same clean exit as Ctrl+Alt+Q.
pub const WM_DOCK_QUIT_REQUEST: u32 = WM_APP + 8;
/// tray host (`tray-host` thread) -> UI: the list of tray icons changed.
/// wParam = 1 only a bitmap changed (redraw), 2 the number/order of visible icons changed (redo the
/// layout). lParam = 0. `main` calls `ui::on_tray_changed(wParam)` and `popup::on_tray_changed()`.
/// Consecutive posts are coalesced: the UI always re-reads `tray_host::snapshot()`.
pub const WM_DOCK_TRAY_CHANGED: u32 = WM_APP + 9;
/// notif (`notif` thread) -> UI: the notification/DND state changed.
/// wParam = 1 list/count/access changed, 2 only the "Do Not Disturb" state changed; lParam = 0.
/// `main` calls `ui::on_notif_changed()` and `popup::on_notif_changed()`; the data is read with
/// `notif::snapshot()` (short clone under a Mutex).
pub const WM_DOCK_NOTIF: u32 = WM_APP + 11;
/// popup (UI thread, popup window) -> dock window: a popup was closed (click outside, Esc,
/// toggle, action). wParam = `popup::PopupKind as usize`, lParam = 0. Posted (not a direct call)
/// to avoid re-entering the UI from the popup's wnd_proc. `main` calls `ui::on_popup_closed()`.
pub const WM_DOCK_POPUP_CLOSED: u32 = WM_APP + 12;
/// app menu popup (UI thread, popup window) -> dock window: the user picked an entry of an app's
/// themed context menu. wParam = `ID_MENU_*` (PIN/UNPIN/CLOSE_ALL/NEW_WINDOW), lParam = 0.
/// The app key is read with `popup::take_app_menu_key()`. Handled by `ui::handle_message`.
pub const WM_DOCK_APP_MENU_CMD: u32 = WM_APP + 13;
/// popup mouse hook (WH_MOUSE_LL, runs on the UI thread, installed only while a popup is open) ->
/// POPUP window (not the dock's): a mouse button went down outside the popup. wParam = lParam = 0.
/// Posted (never handled inside the hook callback); the popup closes itself.
pub const WM_POPUP_OUTSIDE: u32 = WM_APP + 14;
/// Settings window (UI thread, `DockSettings` window) -> dock window: a setting was changed and saved
/// (`config::modify`). wParam = lParam = 0. Posted so the bar is rebuilt OUTSIDE the popup's
/// wnd_proc (no re-entry). `main` calls `ui::on_config_changed()`.
pub const WM_DOCK_CONFIG_APPLY: u32 = WM_APP + 15;
/// hook -> UI: the Win key was tapped alone twice in quick succession (`hook::DOUBLE_TAP_MS`): open the
/// dock's Start menu (`popup_startmenu`). wParam = 0, lParam = 0. Posted by the `hook` thread
/// INSTEAD of the second `WM_DOCK_TOGGLE`. `main` calls `ui::open_start_menu()`.
pub const WM_DOCK_START_MENU: u32 = WM_APP + 16;
/// Popups, the quick-links menu and `--settings` -> dock window: open the Settings window (or bring
/// the open one to the front). wParam = 0 (`SETTINGS_PAGE_LAUNCHER` from the launcher's Ctrl+,), lParam = 0. `main` calls
/// `settings_window::open()`, or `open_at(Page::Launcher)` for the launcher's value.
pub const WM_DOCK_OPEN_SETTINGS: u32 = WM_APP + 40;
/// wParam of `WM_DOCK_OPEN_SETTINGS`: open on the Launcher page.
pub const SETTINGS_PAGE_LAUNCHER: usize = 1;
/// update worker (`update` thread) -> UI: the update status changed. wParam = lParam = 0.
/// Consecutive posts may be coalesced: re-read `update::status()`.
pub const WM_DOCK_UPDATE: u32 = WM_APP + 17;
/// hook -> UI: the launcher's chord was swallowed. wParam = the sequence number (`hook::LAUNCHER_SEQ`), lParam =
/// `kb.time` of the key (GetTickCount clock). The UI calls `hook::ack_launcher(seq)` before anything else, then
/// `hook::post_verdict` decides (open, close or drop).
pub const WM_DOCK_LAUNCHER: u32 = WM_APP + 24;
/// hook -> UI: the chord passed under the YIELD flag (PowerToys answers it). wParam = 0, lParam = `kb.time`.
pub const WM_DOCK_LAUNCHER_YIELDED: u32 = WM_APP + 25;
/// launcher -> dock window: the close a type-ahead replay asked for at the launcher's first activation, posted so it
/// runs outside that activation (`popup_launcher::on_focus`). wParam = the open's sequence, lParam = 0.
pub const WM_DOCK_LAUNCHER_CLOSE: u32 = WM_APP + 26;
/// hook -> UI: the chord passed under the FULLSCREEN flag, or failed open. wParam = the newest sequence the hook issued
/// (`hook::LAUNCHER_SEQ`), lParam = `kb.time`. The UI calls `hook::resync_launcher(wParam)` first (every post before
/// the note was handled, a lost one included), then computes the pass flags again: a window can leave fullscreen
/// without a foreground change.
pub const WM_DOCK_LAUNCHER_PASSED: u32 = WM_APP + 27;
/// PowerToys (SPEC-launcher 5.7), only while the chord is Alt+Space. wParam 0: a chord owner the launcher waits on exited
/// (posted by the thread-pool wait of `launcher_sys`); 1-4: the answer to the PowerToys notice (posted by the launcher, so
/// the config is written outside the popup's content). lParam = 0.
pub const WM_DOCK_LAUNCHER_PT: u32 = WM_APP + 28;
/// launcher -> dock window: an alias was saved or removed in the launcher (plan L2-A2), posted so config.json is written
/// outside the popup's content and the key handler. The change waits in `popup_launcher::hold_alias_op`'s slot (a newer
/// one replaces it). wParam = lParam = 0. Posted only while a chord is set.
pub const WM_DOCK_LAUNCHER_ALIAS: u32 = WM_APP + 29;

// `WM_APP + 10` stays RESERVED: it was the UI -> hook thread message `WM_HOOK_REINSTALL`, retired by wsC Q6; do not
// reuse it.
/// config watch -> dock window (`config::on_folder_change`, a thread-pool wait thread): `config.json` changed on disk
/// (wParam 0: apply it after a 250 ms coalescing timer) or the folder watch broke (wParam 1: fall back to the 2 s poll).
pub const WM_DOCK_CONFIG_CHANGED: u32 = WM_APP + 18;

// `WM_APP + 19` is RESERVED by WS-C: `WM_HOOK_NOTE`, the hook thread's thread
// message (defined privately in `hook.rs`). `WM_HOOK_NOTE`'s wParam values: 4 = activity (wsC Q6: foreground change,
// bar shown or hidden, pointer motion, resume); 1-3 and 5-15 stay reserved for WS-C; 16-31 the launcher (16 =
// `NOTE_LAUNCHER_FAIL_OPEN`). `WM_APP + 20..23` stay for shell-thread messages.

// -------------------------------------- THREAD messages (PostThreadMessageW)

/// UI (or other thread) -> shell thread: there is at least one new `ShellRequest` in the queue.
/// wParam = 0, lParam = 0. The shell thread drains it all. Posted by `shell_thread::request`.
pub const WM_SHELL_WAKE: u32 = WM_APP + 20;
/// audio (`IMMNotificationClient` callback, arbitrary thread) -> shell thread: the default
/// render device was changed/removed/added. The shell thread calls
/// `AudioController::reopen_endpoint` and posts `WM_DOCK_VOLUME`. wParam = 0, lParam = 0.
pub const WM_SHELL_AUDIO_DEVICE_CHANGED: u32 = WM_APP + 21;
/// audio (short-lived `spatial-set` thread) -> shell thread: a `SetDefaultSpatialAudioFormatAsync`
/// finished (whatever its status). If the audio popup is open the shell thread re-reads the spatial
/// sound state and pushes `ShellEvent::Spatial`. wParam = 0, lParam = 0.
pub const WM_SHELL_SPATIAL_CHANGED: u32 = WM_APP + 22;
// The shell thread is asked to exit with WM_QUIT (PostThreadMessageW(WM_QUIT)).

// --------------------------------------------------------------- timers (SetTimer)

/// 50 ms, always on: auto-hide, screen edge, topmost re-assertion. Handled by `ui`.
pub const TIMER_POLL: usize = 1;
/// 2 s, armed only while it has work (`main::housekeeping_wanted`: the bar is shown, an automatic install is pending
/// or the config watch failed): config mtime check while the folder watch is down, visible-idle trim, automatic
/// install. Handled by `main`.
pub const TIMER_HOUSEKEEPING: usize = 2;
/// 30 s while the bar is shown: RAM log (private working set); while hidden one line follows each hidden trim.
/// Handled by `main`.
pub const TIMER_RAM: usize = 3;
/// 3 s (`watchdog::HEARTBEAT_MS`): keepalive heartbeat of an idle dock + taskbar safety check
/// (`taskbar::on_safety_timer`); while messages flow the heartbeat comes from `watchdog::beat_if_due` in `wnd_proc` (dock
/// window, the shell hook's messages excepted) and in the loop (other windows, thread messages), at most once a second.
/// Handled by `main`.
pub const TIMER_SECOND: usize = 4;
/// 5 s while the bar is shown: window list reconciliation (`ui::on_reconcile`). Handled by `ui`.
pub const TIMER_RECONCILE: usize = 5;
/// 100 ms one-shot: debounce after `WM_DOCK_APPS_DIRTY`. Handled by `ui`.
pub const TIMER_APPS_DEBOUNCE: usize = 6;
/// 15 ms: hover plate animation; `ui` starts it when the animation begins and calls
/// `KillTimer` when idle.
pub const TIMER_ANIM: usize = 7;
/// Minute-aligned (one-shot, re-armed on every tick with `clock::ms_until_next_minute`).
/// Handled by `ui`.
pub const TIMER_CLOCK: usize = 8;
/// 300 ms one-shot on the dock window: second `TaskbarCreated` announcement after an Explorer
/// restart (the new Shell_TrayWnd is created in front of our tray host). Armed by
/// `taskbar::on_taskbar_created`; `main` kills it and calls `taskbar::on_reannounce_timer()`.
pub const TIMER_TRAY_REANNOUNCE: usize = 9;

/// 2 s on the dock window, ONLY while the bar is visible and the network and/or metrics module is
/// on (`ui::sysmon_sync` arms and kills it): CPU/RAM sample, network query every few ticks.
/// Handled by `ui`.
pub const TIMER_SYSMON: usize = 10;
/// 400 ms one-shot on the dock window: the pointer rested on an app item with 2+ windows, open the
/// window picker. Armed by `ui::set_hover` only for such items (and only with `window_previews`
/// on), killed when the hover changes. Handled by `ui`.
pub const TIMER_HOVER_PICKER: usize = 11;
/// 300 ms one-shot on the PICKER window: the pointer left the picker and the app item; closes the
/// picker unless the pointer came back. Armed and handled only by `picker`.
pub const TIMER_PICKER_LEAVE: usize = 12;
/// 30 s on the dock window, ONLY while the bar is visible, a battery is present and the module is not
/// off; armed and killed by `ui::battery_sync`. Handled by `ui`.
pub const TIMER_BATTERY: usize = 13;
/// Dock window, armed only while `update_check` is on (`update::sync_timer`): first 60 s after start,
/// then the remaining time to 24 h since `last_update_check`, 24 h after a check, 1 h after a failed
/// scheduled check. Handled by `main` -> `update::on_timer`.
pub const TIMER_UPDATE_CHECK: usize = 14;
/// any thread -> dock window: start a new hidden-trim period (`mem::rearm` posts it; `main::trim_kick`). wParam =
/// lParam = 0. Not `WM_APP + 29` (the launcher's alias message).
pub const WM_DOCK_TRIM_KICK: u32 = WM_APP + 30;
/// One-shot on the dock window while the bar is hidden: the hidden trim (4 s after a hide, a popup or Settings close
/// or a data release; `main::trim_kick`) and the planned second COM pass. Kills itself, then `trim_tick`.
/// Handled by `main`.
pub const TIMER_TRIM: usize = 15;
/// One-shot 250 ms on the dock window after a config change notification (`WM_DOCK_CONFIG_CHANGED`): coalesces an
/// editor's several writes, so a half-written file is not read. Kills itself, then applies. Handled by `main`.
pub const TIMER_CONFIG_COALESCE: usize = 16;

// POPUP window timers (different hwnd from the dock's; handled only by `popup`).
/// 15 ms: popup open fade and hover plate; off when idle.
pub const TIMER_POPUP_ANIM: usize = 30;
/// 1 s while a popup is open: calendar clock, power popup confirmation countdown,
/// safety timer. Off on close.
pub const TIMER_POPUP_TICK: usize = 31;
/// Short one-shot retries (`FOCUS_RETRY_MS`) after open while the popup did NOT get the foreground
/// (keyboard focus: the Start menu search box). Off once it is foreground, after the last try, on close.
pub const TIMER_POPUP_FOCUS: usize = 32;
/// 60 s one-shot armed when the Start menu closes, killed when one opens: frees the Start catalog
/// and icon cache (`popup_startmenu::release`) if no Start menu is open by then.
pub const TIMER_POPUP_START_RELEASE: usize = 33;
/// 11 min one-shot (on the popup window) armed when the Start data is released, killed when a Start menu opens: it
/// re-arms the idle trim so the next hidden trim makes COM's second, MTA `CoFreeUnusedLibraries` call (M1 F4).
/// 34 is the launcher's, 40 to 42 belong to the dock window, so 43.
pub const TIMER_POPUP_COM_FREE: usize = 43;
// The launcher's one-shots, on the DOCK window (a popup window can be destroyed, and its timers with it):
// `ui::on_timer` forwards 34-36 and 39 to `popup_launcher::on_timer`, which kills each before acting; `popup::shutdown` kills
// them all. None is armed while the chord is off.
/// 120 ms one-shot while the launcher is open: the panel shrinks once no key came for that long.
pub const TIMER_LAUNCHER_SHRINK: usize = 34;
/// 10 s one-shot after the dock starts: the launcher's catalogue is listed.
pub const TIMER_LAUNCHER_WARM: usize = 35;
/// 2 s one-shot after a launcher close: its state files are saved.
pub const TIMER_LAUNCHER_SAVE: usize = 36;
// 37: unused. It was the launcher's 10-minute release of its settings rows; since L3 T19 the settings are mapped at each
// open and dropped at its close.
// 38: unused. It was to end the launcher's files worker 60 s after a close; since plan L2 D2 the files helper ends at the
// close itself (`launcher_files::end`), so nothing of files stays while the launcher is closed.
/// 60 s one-shot after the chord becomes Alt+Space (the dock's start): PowerToys detection again; `ANSWER_MS`
/// (`launcher::powertoys`) after a chord passed under YIELD: the end of its answer window (L1b-5).
pub const TIMER_LAUNCHER_PT: usize = 39;

// SETTINGS window timers (`DockSettings` hwnd; handled only by `settings_window`). Timer ids are per
// HWND, so they could not clash with the dock window's private ones (`ui.rs`: 40, 41, 42), but 50 to
// 52 keep this registry unambiguous.
/// 1 s, ONLY while the Settings window is open: import and update status, memory figures.
pub const TIMER_SETTINGS_TICK: usize = 50;
/// One-shot: sends the bar rebuild (`WM_DOCK_CONFIG_APPLY`) that the drag throttle skipped.
pub const TIMER_SETTINGS_APPLY: usize = 51;
/// One-shot: redraws the mini bar of the Appearance page that its throttle skipped (drag, resize).
pub const TIMER_SETTINGS_BAR: usize = 52;

// --------------------------------------------- context menu command ids

/// "Pin" (pins the entry to the dock).
pub const ID_MENU_PIN: u32 = 1;
/// "Unpin".
pub const ID_MENU_UNPIN: u32 = 2;
/// "Close all windows" (WM_CLOSE to each one).
pub const ID_MENU_CLOSE_ALL: u32 = 3;
/// "New window" (launches a new instance, like a middle click).
pub const ID_MENU_NEW_WINDOW: u32 = 5;

#[cfg(test)]
mod tests {
    /// Every `const NAME: <ty> = <prefix> + n;` (or `= n;` when the prefix is empty) of `src`, as (name, n).
    fn scan(src: &str, name_prefix: &str, ty: &str, value_prefix: &str) -> Vec<(String, u32)> {
        let mut out = Vec::new();
        for line in src.lines() {
            let line = line.trim_start();
            let line = line.strip_prefix("pub(crate) ").or_else(|| line.strip_prefix("pub ")).unwrap_or(line);
            let Some(rest) = line.strip_prefix("const ") else { continue };
            let Some((name, rest)) = rest.split_once(':') else { continue };
            if !name.starts_with(name_prefix) {
                continue;
            }
            let Some(rest) = rest.trim_start().strip_prefix(ty) else { continue };
            let Some(value) = rest.trim_start().strip_prefix('=') else { continue };
            let value = value.trim().trim_end_matches(';').trim();
            let number = if value_prefix.is_empty() { value } else { value.strip_prefix(value_prefix).unwrap_or("") };
            if let Ok(n) = number.trim().parse::<u32>() {
                out.push((name.to_string(), n));
            }
        }
        out
    }

    fn assert_distinct(what: &str, ids: &[(String, u32)]) {
        for (i, (a, x)) in ids.iter().enumerate() {
            for (b, y) in &ids[i + 1..] {
                assert!(x != y, "{what}: {a} and {b} both use {x}");
            }
        }
    }

    #[test]
    fn message_and_timer_ids_are_distinct() {
        // WM_APP + n of msg.rs: one window, one id space.
        let msgs = scan(include_str!("msg.rs"), "WM_", "u32", "WM_APP +");
        assert!(msgs.len() >= 28, "msg.rs scan found only {} WM_APP ids", msgs.len());
        assert_distinct("msg.rs WM_APP", &msgs);

        // Timer ids of the dock window: msg.rs's TIMER_ consts and ui.rs's private ones.
        let mut timers = scan(include_str!("msg.rs"), "TIMER_", "usize", "");
        assert!(timers.len() >= 28, "msg.rs scan found only {} timer ids", timers.len());
        let ui = scan(include_str!("ui.rs"), "TIMER_", "usize", "");
        assert!(ui.len() >= 3, "ui.rs scan found only {} private timer ids", ui.len());
        timers.extend(ui);
        assert_distinct("timers", &timers);

        // The tray host window has its own WM_APP space.
        let host = scan(include_str!("tray_host.rs"), "WM_HOST_", "u32", "WM_APP +");
        assert!(host.len() >= 6, "tray_host.rs scan found only {} WM_HOST_ ids", host.len());
        assert_distinct("tray_host.rs WM_HOST_", &host);
    }
}
