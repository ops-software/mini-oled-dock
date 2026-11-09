# Settings Window Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Replace the Settings popup with a resizable top-level Settings window (variant A: left navigation, seven data-driven pages). The window also hosts the battery options (Part 4) and the updater controls (Part 2).

**Architecture:** There are three new files. `settings_pages.rs` is pure. It turns `(Page, &Config, &Ctx)` into a `Vec<Row>` and maps a click on a row part to an `Effect`, so adding an option means adding a row. `settings_view.rs` lays out and paints the nav and the rows on a `surface::Surface`, using the popups' own drawing helpers. It also renders the `--preview` PNGs without a window. `settings_window.rs` owns the `DockSettings` HWND on the UI thread: DWM dark frame, `WM_PAINT` through `Surface::copy_to`, input, DPI, keyboard and timers. It applies effects through `config::modify` and `WM_DOCK_CONFIG_APPLY`, the same way the popup did. Once every caller points at the window, `popup_settings.rs` and its plumbing are deleted.

**Tech Stack:** Rust 1.87+ and `windows` 0.62. The features it needs (`Win32_Graphics_Dwm`, `Win32_UI_HiDpi`, `Win32_UI_WindowsAndMessaging`, `Win32_Graphics_Gdi`) are already enabled in `Cargo.toml`. The config uses `serde_json`. No new crate.

**Spec:** `docs/superpowers/specs/2026-11-09-settings-window-updater-fixes-design.md`. This plan covers Part 3, plus the Settings-window parts of Part 2 (*UI*) and Part 4 (*Settings window*).

## Global Constraints

- Code, comments, UI strings and release notes are in English. Every visible string goes through `i18n::tr(en, it)`. Non-ASCII Italian characters are written as `\u{..}` escapes, as in the existing code (`"Opacit\u{e0}"`).
- Tests: `DOCK_NO_MANIFEST=1 CARGO_TARGET_DIR=target-test cargo test --locked` (Git Bash). Run `cargo fmt --check` before every commit.
- Preview renders: `DOCK_NO_MANIFEST=1 CARGO_TARGET_DIR=target-preview cargo build --locked`, then `target-preview/debug/dock.exe --preview <dir>`.
- Preview DPIs: the existing popup and bar PNGs are rendered at `DPIS = [(120, ""), (96, "_96")]` (`src/preview.rs:46`). `DPIS` stays unchanged, because the byte-identical baseline depends on it. The Settings PNGs use their own `SETTINGS_DPIS = [(144, ""), (96, "_96")]` (spec: 96 and 144 DPI), so `docs/screenshots` holds 120 DPI popups next to 144 DPI Settings pages.
- Preview baseline: every PNG written today must stay byte-identical, with three exceptions. `popup_start.png` and `popup_start_96.png` change (new row). The `popup_settings*.png` files are removed. `popup_power*.png` stays unchanged, because the preview has no pending update.
- Nothing of the window exists while it is closed. The `Win` struct is dropped, its `Surface` freed, the `DockSettings` class unregistered and its timers killed. There is no thread, and no timer runs while the window is closed.
- Window: class `DockSettings`, style `WS_OVERLAPPEDWINDOW & !WS_MAXIMIZEBOX`, minimum client area 640×460 logical, default 860×600. The size is stored in `settings_window_size` in logical px at 96 DPI and clamped to the work area of the display the window opens on. The window opens centred on the bar's display, is never topmost and has one instance: opening it again activates the existing window.
- DWM: `DWMWA_USE_IMMERSIVE_DARK_MODE` (20) = TRUE. `DWMWA_CAPTION_COLOR` (35) and `DWMWA_BORDER_COLOR` (34) are the config `background` as a COLORREF, and `DWMWA_TEXT_COLOR` (36) is white. All four are re-applied when the background changes.
- Layout (logical px): nav column 200 wide. Rows 44 high. Group headers uppercase, 11 px (`Font::Small`), 1 px extra letter spacing, `TEXT3_ALPHA`. Labels `Font::Normal`. Descriptions 12 px (`Font::Caption`, `TEXT3_ALPHA`). Page title 20 px semibold (`Font::Title`), with a one-line subtitle in `Font::Caption`, `TEXT2_ALPHA`. Page content is at most 640 wide. The current nav entry has a 3 px white bar on its left plus the hover plate (Task 7, `current_nav_entry_is_marked`).
- Every change applies at once. A click saves through `config::modify` and posts `WM_DOCK_CONFIG_APPLY`. A slider or picker drag only sets the value in memory (`config::set`), throttled to one rebuild per 30 ms, and writes the file on release or close.
- Keyboard: `Esc` closes. `Tab` and `Shift+Tab` toggle focus between nav and page. `Up`/`Down` change page while the nav has focus and scroll one row while the page has focus. The wheel scrolls the page, also over a slider. Nothing else. The old popup's wheel-over-slider steps (`popup_settings.rs:1054-1066`, README line 129) are dropped on purpose: in a scrolling page they would hijack the scroll whenever a slider passes under the pointer. Task 11 removes that README sentence and says so in the release notes.
- Nothing runs from network data. "Release notes" opens only URLs that start with `https://github.com/ops-software/mini-oled-dock/releases/`, through the de-elevated shell path (`ui::launch_target_str`).
- Commits are in English. Each message ends with a blank line and then `Claude-Session: https://claude.ai/code/session_01FHR79Qfsfd7dTKsSjKFXxX`. Pass it as a second `-m`.

## Prerequisites (landed by plans 01–03)

This plan consumes the names below. Before Task 1, run `grep -n "pub" src/update.rs src/battery.rs` and `grep -n "show_battery\|battery_percent\|update_check\|update_auto_install" src/config.rs`. Where the landed names differ, use them. Only names change, never the behaviour this plan describes. Tasks 2 (`battery_icon`), 5, 6, 8 and 11 do not compile until plans 01-03 are merged into the branch: do not start Task 1 before both greps find every name.

- `crate::update::check_now()`, `crate::update::install_now()` (results ignored with `let _ =`), `crate::update::status() -> UpdateStatus`.
- `UpdateStatus { installed: String, available: Option<Release>, last_check: Option<String>, state: UpdateState, last_action: String }`. `last_action` is the short localized line of the last check, download or install outcome ("" = nothing has run yet). It stays after `state` returns to `Idle`. Plan 02 exposes this text as `UpdateStatus::line()` over its `outcome` field; if that is how it landed, call the method where this plan reads the field.
- `Release { tag: String, name: String, published_at: String, notes: String, url: String }`.
- `UpdateState { Idle, Checking, Downloading, Verifying, Installing, Failed(String) }`.
- `Config::update_check: bool` (default true) and `Config::update_auto_install: bool` (default false).
- `Config::show_battery: BatteryShow` with variants `Auto`, `On`, `Off` (JSON `"auto"`/`true`/`false`), `Config::battery_percent: bool` (default true) and `Config::battery_percent_inside: bool` (default false).
- `glyphs::battery_shape(u: f32, v: f32, charge: f32, charging: bool) -> bool`.
- `start_import::{start, reset, status, status_word, Status}`. Plan 01 may add a `Status` variant: give it a label in `import_label`.
- If plan 02 left a stub `src/settings_window.rs` (`pub fn is_open() -> bool { false }`), Task 8 replaces it.

## Review Focus

1. **Window bigger than the display.** A saved `settings_window_size` is larger than the display the window opens on (a laptop undocked from a 4K monitor), or the work area is smaller than the minimum. The window must still open fully on screen, with the title bar reachable. Pinned by Task 8, `initial_rect_centres_and_clamps`.
2. **DPI changes.** The window moves between monitors with different DPI and is resized there. The saved size must stay logical, never doubling or shrinking on each restart. Pinned by Task 8, `logical_size_survives_dpi_changes`.
3. **Closing mid-drag.** Esc, the title-bar X or a dock exit arrives while a slider or the colour picker is still held. The value on screen must be the value saved, in `config.json` and in the bar: when the last drag move was skipped by the throttle, closing both writes the file and posts `WM_DOCK_CONFIG_APPLY`. Pinned by Task 8, `pending_saves_on_finish`.
4. **Tray app exits under the chooser.** A tray app exits while "Choose visible icons" is open. A click must act on the app whose name was shown, never on a neighbour and never out of bounds, whether the app was in the middle of the list or at its end. The row carries the app's `key`, not an index. Pinned by Task 6, `tray_chooser_rows_are_inline`.
5. **Crowded choices.** Six displays plus Pointer, Italian labels, window at its minimum width: a segmented choice must never draw over its label. Pinned by Task 7, `choices_never_cover_their_label`.

---

## File map

| File | Responsibility |
|---|---|
| `src/settings_pages.rs` (new) | Pure page model: `Page`, `Row`, `Control`, `Setting`, `Slider`, `Action`, `Edit`, `Effect`, `Ctx`, `rows()`, `click()`, `apply_edit()`, plus the pure helpers moved from `popup_settings.rs` |
| `src/settings_view.rs` (new) | Layout (`Geometry`), hit ids, painting of nav and rows, scroll clamp, picker mapping, mini live bar, `preview_pixels` |
| `src/settings_window.rs` (new) | `DockSettings` window: create/activate/close, DWM frame, `WM_PAINT`/`WM_SIZE`/`WM_DPICHANGED`/`WM_GETMINMAXINFO`/`WM_EXITSIZEMOVE`, mouse, wheel, keys, timers, config apply, `is_open()` |
| `src/surface.rs` | `Font::Title`, `Font::Caption`, `Surface::blend_pixels`, `BADGE_RGB` |
| `src/popup.rs` | `badge()` helper. `PopupKind::Settings`, `next_tray_chooser` and `TRAY_CHOOSER` removed |
| `src/glyphs.rs` | Settings icon set |
| `src/render.rs` | `paint_offscreen()` for the mini bar |
| `src/config.rs` | `settings_window_size`, `Config::settings_size()` |
| `src/mem.rs` | `private_bytes()`, `last_hidden_trim()` |
| `src/msg.rs` | `WM_DOCK_OPEN_SETTINGS` (`WM_APP + 40`), `TIMER_SETTINGS_TICK` (50), `TIMER_SETTINGS_APPLY` (51) |
| `src/main.rs` | `mod` lines, `--settings`, `WM_DOCK_OPEN_SETTINGS`, cleanup, trim gate |
| `src/ui.rs` | Config/tray change hooks, `dock_hmonitor` made `pub(crate)`, Settings arm removed |
| `src/apps.rs` | `DockSettings` windows are not bar apps |
| `src/popup_power.rs`, `src/popup_startmenu.rs`, `src/popup_start.rs` | Entry points open the window. Badge on the Settings row |
| `src/popup_tray.rs` | `TrayRow` and `customize_rows()` exposed. Direct-chooser mode removed |
| `src/preview.rs` | `settings_<page>.png` at 144 and 96 DPI (`SETTINGS_DPIS`; `DPIS` stays 120/96). `popup_settings*` removed. `mock_icon` made `pub(crate)` |
| `src/start_import.rs` | Doc comments only: "Settings popup" becomes "Settings window" (Task 10) |
| `src/popup_settings.rs` | Deleted (Task 10) |
| `README.md`, `docs/screenshots/`, `.github/release-notes/v1.4.0.md` | Screenshots table and release notes (Task 11) |

## Labels (English / Italian, all through `i18n::tr`)

| Where | English | Italian |
|---|---|---|
| Window title, quick-links row | Dock settings | Impostazioni dock |
| Page Appearance | Appearance · Colour, sizes and language of the bar | Aspetto · Colore, dimensioni e lingua della barra |
| Page Bar | Bar · Where the bar sits and how it behaves | Barra · Dove sta la barra e come si comporta |
| Page Start | Start · The Start button and the Start menu | Start · Il pulsante Start e il menu Start |
| Page Modules | Modules · What the bar shows on the right | Moduli · Cosa mostra la barra a destra |
| Page Screen | Screen · The display the bar sits on | Schermo · Lo schermo su cui sta la barra |
| Page Updates | Updates · New versions from GitHub | Aggiornamenti · Nuove versioni da GitHub |
| Page About | About · Version, memory and files | Informazioni · Versione, memoria e file |
| Headers | Colour, Sizes, Language, Position, Behaviour, Start menu, Pinned apps, Modules, Battery, Tray, Display, Status, Automatic, Dock, Memory, Files, Taskbar | Colore, Dimensioni, Lingua, Posizione, Comportamento, Menu Start, App aggiunte, Moduli, Batteria, Area di notifica, Schermo, Stato, Automatico, Dock, Memoria, File, Barra delle applicazioni |
| Appearance rows | Background · Opacity · App icons · App names · Tray icons · Modules · Reset sizes [Reset] · Language | Sfondo · Opacit\u{e0} · Icone app · Nomi app · Icone area notifiche · Moduli · Ripristina dimensioni [Ripristina] · Lingua |
| Colour header value | preset name (`preset_name`) or Custom | nome del preset o Personalizzato |
| Bar rows | Position (Top/Bottom) · Display mode (Icon + name/Icon/Name) · Auto-hide "Shows up with the Win key or at the screen edge" · Window previews "Rest the pointer on an app with several windows" · Clock seconds · Volume percent "Next to the speaker" | Posizione (In alto/In basso) · Mostra (Icona + nome/Icona/Nome) · Nascondi automaticamente "Compare con il tasto Win o al bordo dello schermo" · Anteprime finestre "Fermando il puntatore su un'app con pi\u{f9} finestre" · Secondi nell'orologio · Percentuale volume "Accanto all'altoparlante" |
| Start rows | Start button · Start layout (Vertical/Horizontal) · Win key opens Start "Together with the bar" · Import from Windows Start [Import], idle description "Replaces the pinned apps of the dock's Start menu" | Pulsante Start · Layout Start (Verticale/Orizzontale) · Il tasto Win apre Start "Insieme alla barra" · Importa dallo Start di Windows [Importa], "Sostituisce le app aggiunte al menu Start del dock" |
| Modules rows | `module_label` names · Show battery (Auto/On/Off) "Shown automatically on laptops" · Percentage · Percentage inside the icon "Apple style" · Tray icons · Choose visible icons · tray row "No room in the bar" · empty "No tray apps" | (as `module_label`) · Mostra batteria (Automatica/S\u{ec}/No) "Mostrata automaticamente sui portatili" · Percentuale · Percentuale dentro l'icona "Stile Apple" · Icone area notifiche · Scegli le icone visibili · "Spazio insufficiente nella barra" · "Nessuna app nell'area di notifica" |
| Screen rows | Display (Main/2…/Pointer) "Pointer: the Win key brings the bar to the display under the pointer" · note "The bar is on the only display. Connect another one to choose." | Schermo (Principale/2…/Puntatore) "Puntatore: il tasto Win porta la barra sullo schermo sotto il puntatore" · "La barra \u{e8} sull'unico schermo. Collegane un altro per scegliere." |
| Updates rows | Installed version · Available: "{tag} · {date}" / Up to date / Not checked yet · Last check: date / Never · Last action: `last_action` / None yet · [Check now] [Release notes] [Update now] · Checking… / Downloading… / Verifying… / Installing… / Failed: {msg} · Check automatically "Once a day on GitHub" · Install automatically "When the dock is idle" | Versione installata · Disponibile: "{tag} · {data}" / Aggiornato / Non ancora controllato · Ultimo controllo: data / Mai · Ultima operazione: `last_action` / Nessuna · [Controlla ora] [Note di rilascio] [Aggiorna ora] · Controllo… / Download… / Verifica… / Installazione… / Non riuscito: {msg} · Controlla automaticamente "Una volta al giorno su GitHub" · Installa automaticamente "Quando il dock \u{e8} inattivo" |
| About rows | "mini-oled-dock {version}" "Ops Software · MIT licence" · Memory now · After trimming "While the bar is hidden" / Not trimmed yet · Data folder [Open] · Log [Open] · Restore the Windows taskbar "Exits the dock" [Exit the dock] | "mini-oled-dock {versione}" "Ops Software · licenza MIT" · Memoria ora · Dopo la pulizia "Mentre la barra \u{e8} nascosta" / Non ancora eseguita · Cartella dati [Apri] · Log [Apri] · Ripristina la barra di Windows "Chiude il dock" [Esci dal dock] |

## Before Task 1: preview baseline

- [ ] Build and render the baseline once, before any change:

```bash
DOCK_NO_MANIFEST=1 CARGO_TARGET_DIR=target-preview cargo build --locked
target-preview/debug/dock.exe --preview target-preview/baseline
```
Expected: `written …` lines and exit 0. Later tasks compare a fresh render in `<dir>` with:
```bash
for f in target-preview/baseline/*.png; do cmp -s "$f" "<dir>/$(basename "$f")" || echo "DIFF $(basename "$f")"; done
```

---

### Task 1: Surface fonts, pixel blend and badge helper

**Files:**
- Modify: `src/surface.rs:58` (add `BADGE_RGB` after `WHITE`), `src/surface.rs:63-87` (`Font` enum and `idx`), `src/surface.rs:452-490` (`Surface` fields `fonts`/`lhs`, `delete_fonts`, `build_fonts`), `src/surface.rs:655` (add `blend_pixels` after `copy_to`)
- Modify: `src/popup.rs:1430-1478` (add `badge` among the content helpers)
- Test: new `#[cfg(test)] mod tests` at the end of `src/surface.rs` and of `src/popup.rs`

**Interfaces:**
- Produces: `Font::Title` (20 px, weight 600, faces `["Segoe UI Variable Display", "Segoe UI"]`) and `Font::Caption` (12 px, weight 400, `TEXT_FACES`). Font arrays become `[HFONT; 6]` / `[i32; 6]`.
- Produces: `pub const BADGE_RGB: (u32, u32, u32) = (255, 140, 0);` (the bar's attention orange).
- Produces: `impl Surface { pub fn blend_pixels(&mut self, x: i32, y: i32, w: i32, h: i32, px: &[u32]) }`. It composites premultiplied 0xAARRGGBB source-over through `Canvas::blend` and honours `set_clip`. It does nothing when `px.len() < (w * h) as usize`.
- Produces: `pub(crate) fn badge(s: &mut Surface, right: i32, cy: i32, count: u32) -> RECT` in `popup.rs`. It draws a pill 16 logical high and `max(16, text + 10)` wide, right edge at `right`, centred on `cy`, filled with `BADGE_RGB` at alpha 255, with `count` centred in black `Font::Small`. Returns the pill rect.

- [ ] **Step 1: Write the failing tests**

```rust
// src/surface.rs
#[cfg(test)]
mod tests {
    use super::*;
    const M: Metrics = Metrics { dpi: 96 };

    #[test]
    fn title_and_caption_fonts_exist() {
        let s = Surface::new(M).expect("surface");
        assert!(s.line_height(Font::Title) > s.line_height(Font::Normal));
        let c = s.line_height(Font::Caption);
        assert!(c >= s.line_height(Font::Small) && c <= s.line_height(Font::Normal));
        assert!(s.text_width("Appearance", Font::Title) > s.text_width("Appearance", Font::Bold));
    }

    #[test]
    fn blend_pixels_composites_and_clips() {
        let mut s = Surface::new(M).expect("surface");
        s.resize(4, 4).unwrap();
        s.fill_rr(rect_wh(0, 0, 4, 4), 0, (0, 0, 0), 255);
        s.blend_pixels(2, 2, 3, 3, &[0xFFFF_FFFF; 9]); // partly outside the DIB
        s.blend_pixels(0, 0, 2, 2, &[0x8000_0000; 4]); // half-transparent black over black
        let (_, _, px) = s.snapshot_pixels().unwrap();
        assert_eq!(px[0], 0xFF00_0000);
        assert_eq!(px[2 * 4 + 2], 0xFFFF_FFFF);
        assert_eq!(px[3 * 4 + 3], 0xFFFF_FFFF);
        s.blend_pixels(0, 0, 4, 4, &[0xFFFF_FFFF; 3]); // short buffer: ignored
        assert_eq!(s.snapshot_pixels().unwrap().2[0], 0xFF00_0000);
    }
}

// src/popup.rs
#[cfg(test)]
mod tests {
    use super::*;
    use crate::surface::rect_wh;

    #[test]
    fn badge_is_an_orange_pill_right_aligned() {
        let mut s = Surface::new(Metrics { dpi: 96 }).expect("surface");
        s.resize(60, 30).unwrap();
        s.fill_rr(rect_wh(0, 0, 60, 30), 0, (0, 0, 0), 255);
        let rc = badge(&mut s, 50, 15, 1);
        assert_eq!((rc.right, rc.bottom - rc.top), (50, 16));
        assert!(rc.right - rc.left >= 16);
        let (w, _, px) = s.snapshot_pixels().unwrap();
        assert_eq!(px[(15 * w + rc.left + 3) as usize], 0xFFFF_8C00);
    }
}
```

- [ ] **Step 2: Run the tests and watch them fail**

Run: `DOCK_NO_MANIFEST=1 CARGO_TARGET_DIR=target-test cargo test --locked surface::tests popup::tests`
Expected: compile errors (`no variant Title`, `no method blend_pixels`, `cannot find function badge`).

- [ ] **Step 3: Implement the two fonts, `Surface::blend_pixels`, `BADGE_RGB` and `popup::badge(s: &mut Surface, right: i32, cy: i32, count: u32) -> RECT` as specified in Interfaces**

The scratch DIB height grows with the taller font. Existing text pixels stay the same, because `Gdi::text` clips to the line height of the font in use.

- [ ] **Step 4: Run the tests and the preview comparison**

Run: `DOCK_NO_MANIFEST=1 CARGO_TARGET_DIR=target-test cargo test --locked`
Expected: PASS.
Run: the preview build, `dock.exe --preview target-preview/after1`, then the `cmp` loop with `<dir>` = `target-preview/after1`.
Expected: no `DIFF` lines.

Run: `cargo fmt --check`. Expected: no output. If it lists files, run `cargo fmt` and the tests again.

- [ ] **Step 5: Commit**

```bash
git add src/surface.rs src/popup.rs
git commit -m "surface: title and caption fonts, pixel blend, update badge helper" -m "Claude-Session: https://claude.ai/code/session_01FHR79Qfsfd7dTKsSjKFXxX"
```

---

### Task 2: Settings icon set

**Files:**
- Modify: `src/glyphs.rs:459` (new section `// ---- Settings window icons` before `#[cfg(test)]`), `src/glyphs.rs:460-495` (tests)

**Interfaces:**
- Produces new shapes. Each is `pub fn NAME(u: f32, v: f32) -> bool`, has the same stroke as the others (`half()`, centred on `ax()`), and stays inside 1.5..14.5:
  - `palette_shape`: palette outline with three holes (Appearance)
  - `bar_shape`: wide rounded pill with three dots (Bar, Position)
  - `grid_shape`: 2×2 outlined squares (Start, Start layout)
  - `list_shape`: three lines with bullets (Modules)
  - `monitor_shape`: screen outline with a thick bottom strip, no stand (Screen, Display)
  - `refresh_shape`: two arcs with arrowheads (Updates, Available, Check automatically)
  - `info_shape`: ring with an "i" (About, version)
  - `circle_shape`: ring with the left half filled (Background)
  - `drop_shape`: water-drop outline (Opacity)
  - `square_shape`: rounded square outline (App icons)
  - `text_shape`: a stroked "A" (App names, Display mode, Percentage)
  - `ring_shape`: ring of radius 5.5 with an empty centre (Tray icons)
  - `bars_shape`: three ascending vertical bars (Modules size, System metrics, Memory)
  - `globe_shape`: circle with equator and meridian (Language)
  - `clock_shape`: ring with two hands (Clock, Clock seconds, Last check)
  - `download_shape`: arrow down into a tray (Import, Update now, Install automatically)
  - `folder_shape`: folder outline (Data folder)
  - `log_shape`: page outline with three lines (Log)
  - `keycap_shape`: rounded key outline with a small inner square (Win key opens Start)
- Produces wrappers around existing shapes, each `pub fn NAME(u: f32, v: f32) -> bool`:
  - `speaker_icon` = `speaker_shape(u, v, 3, false)`
  - `wifi_icon` = `wifi_shape(u, v, 4)`
  - `eye_icon` = `eye_shape(u, v, false)`
  - `battery_icon` = `battery_shape(u, v, 0.6, false)`
- Existing shapes used as they are: `bell_shape`, `show_desktop_shape`, `power_shape`, `windows_logo_shape`, `chevron_down_shape`, `chevron_right_shape`, `close_shape`.

- [ ] **Step 1: Write the failing test** (append to `glyphs::tests`)

```rust
#[test]
fn settings_glyphs_are_distinct_and_sized() {
    let set: [(&str, fn(f32, f32) -> bool); 19] = [
        ("palette", palette_shape), ("bar", bar_shape), ("grid", grid_shape), ("list", list_shape),
        ("monitor", monitor_shape), ("refresh", refresh_shape), ("info", info_shape),
        ("circle", circle_shape), ("drop", drop_shape), ("square", square_shape), ("text", text_shape),
        ("ring", ring_shape), ("bars", bars_shape), ("globe", globe_shape), ("clock", clock_shape),
        ("download", download_shape), ("folder", folder_shape), ("log", log_shape), ("keycap", keycap_shape),
    ];
    let sample = |g: fn(f32, f32) -> bool| -> Vec<bool> {
        (0..64 * 64).map(|i| g((i % 64) as f32 / 4.0 + 0.125, (i / 64) as f32 / 4.0 + 0.125)).collect()
    };
    let mut seen: Vec<Vec<bool>> = Vec::new();
    for (name, g) in set {
        let s = sample(g);
        let cov = s.iter().filter(|b| **b).count() as f32 / s.len() as f32;
        assert!((0.04..0.6).contains(&cov), "{name}: coverage {cov}");
        assert!(!g(0.5, 0.5) && !g(15.5, 0.5) && !g(0.5, 15.5) && !g(15.5, 15.5), "{name} touches a corner");
        assert!(!seen.contains(&s), "{name} is identical to another glyph");
        seen.push(s);
    }
    assert!(!ring_shape(8.0, 8.0) && !clock_shape(3.5, 3.5));
    assert_eq!(speaker_icon(2.5, 8.0), speaker_shape(2.5, 8.0, 3, false));
}
```

- [ ] **Step 2: Run the test and watch it fail**

Run: `DOCK_NO_MANIFEST=1 CARGO_TARGET_DIR=target-test cargo test --locked glyphs::tests::settings_glyphs`
Expected: compile errors (missing functions).

- [ ] **Step 3: Implement the shapes and wrappers listed in Interfaces**

- [ ] **Step 4: Run the test and watch it pass**

Run: `DOCK_NO_MANIFEST=1 CARGO_TARGET_DIR=target-test cargo test --locked glyphs::`
Expected: PASS.

Run: `cargo fmt --check`. Expected: no output. If it lists files, run `cargo fmt` and the tests again.

- [ ] **Step 5: Commit**

```bash
git add src/glyphs.rs
git commit -m "glyphs: icon set for the Settings window" -m "Claude-Session: https://claude.ai/code/session_01FHR79Qfsfd7dTKsSjKFXxX"
```

---

### Task 3: Offscreen bar painting for the mini live preview

**Files:**
- Modify: `src/render.rs:1249-1253` (add `paint_offscreen` after `snapshot_pixels`), `src/render.rs:1255+` (factor a scene helper out of `start_button_is_the_first_item_only_when_on` at `:1286-1316`; new test)
- Modify: `src/preview.rs:83` (`fn mock_icon` becomes `pub(crate)`)

**Interfaces:**
- Produces: `pub(crate) fn paint_offscreen(scene: &Scene<'_>) -> Option<(i32, i32, Vec<u32>)>`. It returns the bar's premultiplied pixels for `scene`, laid out and painted by the real `layout`/`resize`/`paint` path at `scene.metrics`/`scene.sizes`. It takes the bar's `Gfx` out of `GFX` (`c.replace(None)`), runs `init`, `layout`, `resize`, `paint`, `snapshot_pixels` and `destroy`, then always puts the saved `Gfx` back. It works with no bar `Gfx` at all (preview mode). It never pumps messages.
- Produces: `pub(crate) fn mock_icon(letter: char, rgb: u32, size: i32) -> IconBitmap` (unchanged body).
- Produces (tests only): `fn test_scene(m: Metrics, sizes: BarSizes, modules: &[ItemId]) -> Scene<'_>`, the scene literal of the existing test with `start_button: false`.

- [ ] **Step 1: Write the failing test** (in `render::tests`)

```rust
#[test]
fn paint_offscreen_leaves_the_bar_untouched() {
    let m = Metrics { dpi: 96 };
    let sizes = BarSizes::default();
    render_init_for_test(m, sizes);
    resize(40, bar_height(m, sizes)).unwrap();
    let before = snapshot_pixels();
    let big = Metrics { dpi: 144 };
    let modules = [ItemId::Volume, ItemId::Clock];
    let scene = test_scene(big, sizes, &modules);
    let (w, h, px) = paint_offscreen(&scene).expect("offscreen bar");
    assert_eq!(h, bar_height(big, sizes));
    assert_eq!(px.len(), (w * h) as usize);
    assert_eq!(snapshot_pixels(), before);
    destroy();
    assert!(paint_offscreen(&scene).is_some());
    assert!(snapshot_pixels().is_none());
}
```

- [ ] **Step 2: Run the test and watch it fail**

Run: `DOCK_NO_MANIFEST=1 CARGO_TARGET_DIR=target-test cargo test --locked render::tests::paint_offscreen`
Expected: FAIL (`cannot find function paint_offscreen`, `test_scene`).

- [ ] **Step 3: Implement `paint_offscreen(scene: &Scene<'_>) -> Option<(i32, i32, Vec<u32>)>` and `test_scene`, and make `mock_icon` `pub(crate)`**

- [ ] **Step 4: Run the tests and watch them pass**

Run: `DOCK_NO_MANIFEST=1 CARGO_TARGET_DIR=target-test cargo test --locked render::`
Expected: PASS.

Run: `cargo fmt --check`. Expected: no output. If it lists files, run `cargo fmt` and the tests again.

- [ ] **Step 5: Commit**

```bash
git add src/render.rs src/preview.rs
git commit -m "render: paint a scene offscreen without touching the bar" -m "Claude-Session: https://claude.ai/code/session_01FHR79Qfsfd7dTKsSjKFXxX"
```

---

### Task 4: `settings_window_size` and memory counters

**Files:**
- Modify: `src/config.rs:349-358` (constants), `:378-467` (field after `notif_prompted`), `:486-529` (`Default`), `:676-898` (`render`: one template block with a named argument), `:1150-1168` (method next to `sizes()`), `:1247+` (test; `template_roundtrip` at `:1279-1310`)
- Modify: `src/mem.rs:75-102` (`trim` records the result), plus new functions and a `#[cfg(test)] mod tests`

**Interfaces:**
- Produces: `pub settings_window_size: [u16; 2]`, default `[860, 600]`.
- Produces: `pub const SETTINGS_WINDOW_MIN: (i32, i32) = (640, 460);` and `pub const SETTINGS_WINDOW_DEFAULT: (i32, i32) = (860, 600);`
- Produces: `impl Config { pub fn settings_size(&self) -> (i32, i32) }`. Each dimension that is 0 or above 16384 is replaced by its default; the result is then raised to at least the minimum.
- Produces: template text, placed after `"bar_opacity"`:
  ```
    // Size of the Settings window in logical pixels (96 DPI), [width, height]. Remembered when you
    // resize it; at least [640, 460]; clamped to the display it opens on.
    "settings_window_size": {settings_size},
  ```
  where `settings_size = js(&c.settings_window_size)`.
- Produces: `pub fn private_bytes() -> Option<usize>` (from `counters()`), and `pub fn last_hidden_trim() -> Option<usize>`: private bytes right after the last `trim("hidden")`, held in a `static LAST_HIDDEN_TRIM: AtomicUsize` (0 = none).

- [ ] **Step 1: Write the failing tests**

```rust
// src/config.rs tests
#[test]
fn settings_window_size_defaults_and_clamps() {
    let c = Config::default();
    assert_eq!(c.settings_window_size, [860, 600]);
    assert_eq!(c.settings_size(), (860, 600));
    let r = render(&c);
    assert!(r.contains("\"settings_window_size\": [860,600]"));
    assert_eq!(parse(&r).unwrap(), c);
    assert_eq!(parse("{\"settings_window_size\": [100, 100]}").unwrap().settings_size(), (640, 460));
    assert_eq!(parse("{\"settings_window_size\": [0, 20000]}").unwrap().settings_size(), (860, 600));
    assert_eq!(parse("{}").unwrap().settings_window_size, [860, 600]);
}

// src/mem.rs
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn hidden_trim_is_remembered() {
        assert!(private_bytes().is_some_and(|b| b > 0));
        trim("hidden");
        assert!(last_hidden_trim().is_some_and(|b| b > 0));
    }
}
```

- [ ] **Step 2: Run the tests and watch them fail**

Run: `DOCK_NO_MANIFEST=1 CARGO_TARGET_DIR=target-test cargo test --locked settings_window_size hidden_trim`
Expected: compile errors.

- [ ] **Step 3: Implement the field, the constants, `Config::settings_size(&self) -> (i32, i32)`, the template block, `mem::private_bytes() -> Option<usize>` and `mem::last_hidden_trim() -> Option<usize>`**

Add `c.settings_window_size = [1000, 700];` to `template_roundtrip` as well.

- [ ] **Step 4: Run all tests and watch them pass**

Run: `DOCK_NO_MANIFEST=1 CARGO_TARGET_DIR=target-test cargo test --locked`
Expected: PASS.

Run: `cargo fmt --check`. Expected: no output. If it lists files, run `cargo fmt` and the tests again.

- [ ] **Step 5: Commit**

```bash
git add src/config.rs src/mem.rs
git commit -m "config: settings_window_size; mem: private bytes and last hidden trim" -m "Claude-Session: https://claude.ai/code/session_01FHR79Qfsfd7dTKsSjKFXxX"
```

---

### Task 5: Page model, part 1 (types, moved helpers, Appearance, Bar, Start, Screen)

**Files:**
- Create: `src/settings_pages.rs`
- Modify: `src/main.rs:33-67` (add `mod settings_pages;` after `mod render;`)
- Modify: `src/popup_tray.rs:40-47` (`struct Row` becomes `pub(crate) struct TrayRow` with `pub(crate)` fields `key, name, bitmap, visible, in_bar`), `:79-94` (`customize_rows` becomes `pub(crate) fn customize_rows() -> Vec<TrayRow>`), and every use of `Row` in the file
- Test: `#[cfg(test)] mod tests` in `src/settings_pages.rs`

**Interfaces:**
- Consumes: `config::{Config, Background, BACKGROUND_PRESETS, Language, Position, DisplayMode, StartStyle, SizeRange, APP_ICON_SIZE, APP_TEXT_SIZE, TRAY_ICON_SIZE, MODULE_SIZE, MONITOR_POINTER}`, `ui::Screen`, `start_import::Status`, `update::UpdateStatus`, the glyphs from Task 2, `popup_tray::TrayRow`.
- Produces:

```rust
pub(crate) type Glyph = fn(f32, f32) -> bool;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Page { Appearance, Bar, Start, Modules, Screen, Updates, About }
impl Page {
    pub(crate) const ALL: [Page; 7];                 // in nav order
    pub(crate) fn title(self) -> &'static str;       // "Appearance"
    pub(crate) fn subtitle(self) -> &'static str;    // Labels table
    pub(crate) fn icon(self) -> Glyph;               // palette, bar, grid, list, monitor, refresh, info
    pub(crate) fn slug(self) -> &'static str;        // "appearance" … "about" (file names)
    pub(crate) fn at_bottom(self) -> bool;           // Updates, About
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Setting { Language, StartStyle, Position,
    Monitor,      // `config.monitor`: the screen choice, labelled "Display" on the Screen page
    DisplayMode,  // `config.display_mode`: Icon + name / Icon / Name, labelled "Display mode" on the Bar page
    AutoHide, Previews, WinStart,
    Seconds, VolumePercent, Module(&'static str), BatteryShow, BatteryPercent, BatteryInside,
    UpdateCheck, UpdateAutoInstall }

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Slider { Opacity, AppIcon, AppText, TrayIcon, Module }

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Action { ResetSizes, ImportStart, CheckUpdates, ReleaseNotes, InstallUpdate,
    OpenDataFolder, OpenLog, ExitDock }

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Button { pub action: Action, pub text: &'static str, pub enabled: bool }

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Control {
    Header { value: Option<String> },   // group header; value drawn dim at the right
    Preview,                            // mini live bar (Appearance)
    Toggle { setting: Setting, on: bool, enabled: bool },
    Choice { setting: Setting, options: Vec<&'static str>, selected: Option<usize> },
    Slider { slider: Slider, value: i32, min: i32, max: i32, unit: &'static str },
    Swatches { selected: usize, picker_open: bool },   // 7 presets + custom (index 7)
    Picker,
    Buttons(Vec<Button>),
    Chooser { open: bool },
    TrayApp { key: String, visible: bool },   // tray_host key of the app, never an index
    Status(String),
    Note,
}

#[derive(Clone, Debug)]
pub(crate) struct Row { pub icon: Option<Glyph>, pub label: String, pub desc: Option<String>, pub control: Control }
pub(crate) fn same_rows(a: &[Row], b: &[Row]) -> bool;   // label, desc, control; icon ignored

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct Memory { pub now: Option<usize>, pub trimmed: Option<usize> }

pub(crate) struct Ctx<'a> {
    pub screens: &'a [Screen], pub picker_open: bool, pub chooser_open: bool,
    pub tray: &'a [TrayRow], pub import: start_import::Status, pub update: &'a UpdateStatus,
    pub memory: Memory, pub data_dir: Option<&'a str>, pub log_path: Option<&'a str>,
}

pub(crate) fn rows(page: Page, c: &Config, ctx: &Ctx<'_>) -> Vec<Row>;  // Modules/Updates/About: Vec::new() until Task 6

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Edit { Choose(Setting, usize), Flip(Setting), Slider(Slider, i32), Background(Background), ResetSizes }
pub(crate) fn apply_edit(c: &mut Config, e: Edit, screens: &[Screen]);

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Effect { None, Save(Edit), TogglePicker, ToggleChooser, TrayVisible { key: String, hidden: bool }, Run(Action) }
pub(crate) fn click(row: &Row, part: Option<usize>, ctx: &Ctx<'_>) -> Effect;
```

- Moved helpers (copied from `popup_settings.rs:201-421` and `:455-470`, bodies unchanged, extended for the new `Setting` variants; the originals go away in Task 10). `popup_settings.rs:99` calls the display-mode setting `Setting::Display`; the moved code and tests name it `Setting::DisplayMode`, so no row labelled "Display" maps to a variant called `Display`: `module_label`, `options`, `selected`, `choose`, `is_on`, `flip`, `slider_range`, `slider_value`, `set_slider`, `reset_sizes`, `sizes_are_default`, `hsv_to_rgb`, `rgb_to_hsv`, `background_of`, `preset_name`, `preset_index`, `swatch_selected`, `import_label`. All are `pub(crate)`. `VolumePercent` maps to `show_volume_percent`, `BatteryShow` to `show_battery`, `BatteryPercent`/`BatteryInside` to their bools and `UpdateCheck`/`UpdateAutoInstall` to theirs.
- Page content in this task, with the icon in brackets:
  - Appearance: `Preview`; header Colour (value = preset name or "Custom"); Background [circle] `Swatches`; `Picker` right after the swatches when `ctx.picker_open`; Opacity [drop] slider; header Sizes; App icons [square], App names [text], Tray icons [ring], Modules [bars] sliders; Reset sizes [refresh] with `Buttons([ResetSizes "Reset", enabled = !sizes_are_default])`; header Language; Language [globe] `Choice`.
  - Bar: header Position; Position [bar] `Choice`; Display mode [text] `Choice`; header Behaviour; Auto-hide [chevron_down], Window previews [eye_icon], Clock seconds [clock], Volume percent [speaker_icon] as toggles, with the descriptions from the Labels table.
  - Start: header Start menu; Start button [windows_logo] toggle (`Module("start")`); Start layout [grid] `Choice`; Win key opens Start [keycap] toggle; header Pinned apps; Import from Windows Start [download]. Its `desc` is the idle text while `ctx.import == Idle`, otherwise `import_label(ctx.import)`. Its control is `Buttons([ImportStart "Import", enabled = ctx.import != Running])`.
  - Screen: with `ctx.screens.len() > 1`, header Display and a Display [monitor] `Choice(Monitor)` with the Pointer description. Otherwise a single `Note` row with the single-display text.
- `click` rules:
  - `Choice` with part `j`: returns `None` when `j` is selected or out of range, else `Save(Choose(setting, j))`.
  - `Toggle`: returns `None` when disabled, else `Save(Flip)`.
  - `Swatches` with part `k`: `k == 7` returns `TogglePicker`; a preset already selected returns `None`; any other preset returns `Save(Background(BACKGROUND_PRESETS[k].1))`.
  - `Buttons` with part `j`: returns `None` when the button is disabled or missing. Otherwise `ResetSizes` returns `Save(Edit::ResetSizes)` and every other action returns `Run(action)`.
  - Everything else returns `None`. Sliders and the picker are dragged, never clicked.

- [ ] **Step 1: Write the failing tests** (move `choices_roundtrip`, `screen_choice`, `toggles_flip`, `sliders_clamp_and_reset`, `hsv_roundtrip`, `every_preset_has_an_italian_name`, `swatch_selection` and `test_screens` from `popup_settings.rs:1074-1244`, adapted to `apply_edit`, and add the tests below)

```rust
fn idle() -> UpdateStatus {
    UpdateStatus { installed: "1.4.0".into(), available: None, last_check: None, state: UpdateState::Idle, last_action: String::new() }
}
fn ctx<'a>(screens: &'a [Screen], st: &'a UpdateStatus) -> Ctx<'a> {
    Ctx { screens, picker_open: false, chooser_open: false, tray: &[], import: start_import::Status::Idle,
          update: st, memory: Memory::default(), data_dir: None, log_path: None }
}

#[test]
fn every_page_has_a_title_slug_and_icon() {
    for p in Page::ALL { assert!(!p.title().is_empty() && !p.subtitle().is_empty() && !p.slug().is_empty()); }
    assert_eq!(Page::ALL.iter().filter(|p| p.at_bottom()).count(), 2);
}

#[test]
fn appearance_rows_follow_the_config() {
    let st = idle();
    let mut c = Config::default();
    let r = rows(Page::Appearance, &c, &ctx(&[], &st));
    assert!(matches!(r[0].control, Control::Preview));
    assert_eq!(r.iter().filter(|r| matches!(r.control, Control::Header { .. })).count(), 3);
    assert!(r.iter().any(|r| r.control == Control::Swatches { selected: 0, picker_open: false }));
    assert!(!r.iter().any(|r| r.control == Control::Picker));
    let reset = r.iter().find_map(|r| match &r.control { Control::Buttons(b) => Some(b[0].clone()), _ => None }).unwrap();
    assert_eq!((reset.action, reset.enabled), (Action::ResetSizes, false));
    c.app_icon_size = 24;
    c.background = Background(0x123456);
    let mut cx = ctx(&[], &st);
    cx.picker_open = true;
    let r = rows(Page::Appearance, &c, &cx);
    let k = r.iter().position(|r| matches!(r.control, Control::Swatches { selected: 7, picker_open: true })).unwrap();
    assert_eq!(r[k + 1].control, Control::Picker);
    assert!(r.iter().any(|r| matches!(&r.control, Control::Buttons(b) if b[0].enabled)));
}

#[test]
fn bar_and_start_rows() {
    let st = idle();
    let c = Config::default();
    let bar = rows(Page::Bar, &c, &ctx(&[], &st));
    for s in [Setting::AutoHide, Setting::Previews, Setting::Seconds, Setting::VolumePercent] {
        assert!(bar.iter().any(|r| matches!(r.control, Control::Toggle { setting, .. } if setting == s)), "{s:?}");
    }
    assert!(bar.iter().any(|r| matches!(r.control, Control::Choice { setting: Setting::DisplayMode, selected: Some(0), .. })));
    let mut cx = ctx(&[], &st);
    let start = rows(Page::Start, &c, &cx);
    assert!(start.iter().any(|r| matches!(r.control, Control::Toggle { setting: Setting::Module("start"), on: true, .. })));
    assert!(start.iter().any(|r| matches!(r.control, Control::Choice { setting: Setting::StartStyle, .. })));
    cx.import = start_import::Status::Running;
    let busy = rows(Page::Start, &c, &cx);
    let imp = busy.iter().find(|r| matches!(&r.control, Control::Buttons(b) if b[0].action == Action::ImportStart)).unwrap();
    assert!(matches!(&imp.control, Control::Buttons(b) if !b[0].enabled));
    assert_eq!(imp.desc.as_deref(), Some(import_label(start_import::Status::Running).as_str()));
}

#[test]
fn screen_page_offers_the_choice_only_with_two_displays() {
    let st = idle();
    let c = Config::default();
    let sc = test_screens(); // three displays, as in popup_settings
    let one = rows(Page::Screen, &c, &ctx(&sc[..1], &st));
    assert_eq!(one.len(), 1);
    assert_eq!(one[0].control, Control::Note);
    let many = rows(Page::Screen, &c, &ctx(&sc, &st));
    assert!(many.iter().any(|r| matches!(&r.control, Control::Choice { setting: Setting::Monitor, options, selected: Some(0) } if options.len() == 4)));
}

#[test]
fn click_maps_controls_to_effects() {
    let st = idle();
    let cx = ctx(&[], &st);
    let row = |control| Row { icon: None, label: String::new(), desc: None, control };
    let ch = row(Control::Choice { setting: Setting::Position, options: vec!["a", "b"], selected: Some(1) });
    assert_eq!(click(&ch, Some(1), &cx), Effect::None);
    assert_eq!(click(&ch, Some(0), &cx), Effect::Save(Edit::Choose(Setting::Position, 0)));
    assert_eq!(click(&ch, Some(5), &cx), Effect::None);
    let t = row(Control::Toggle { setting: Setting::AutoHide, on: true, enabled: true });
    assert_eq!(click(&t, None, &cx), Effect::Save(Edit::Flip(Setting::AutoHide)));
    let off = row(Control::Toggle { setting: Setting::BatteryInside, on: false, enabled: false });
    assert_eq!(click(&off, None, &cx), Effect::None);
    let sw = row(Control::Swatches { selected: 0, picker_open: false });
    assert_eq!(click(&sw, Some(7), &cx), Effect::TogglePicker);
    assert_eq!(click(&sw, Some(0), &cx), Effect::None);
    assert_eq!(click(&sw, Some(1), &cx), Effect::Save(Edit::Background(BACKGROUND_PRESETS[1].1)));
    let b = row(Control::Buttons(vec![
        Button { action: Action::ResetSizes, text: "Reset", enabled: true },
        Button { action: Action::OpenLog, text: "Open", enabled: false },
    ]));
    assert_eq!(click(&b, Some(0), &cx), Effect::Save(Edit::ResetSizes));
    assert_eq!(click(&b, Some(1), &cx), Effect::None);
    assert_eq!(click(&b, Some(2), &cx), Effect::None);
}
```

- [ ] **Step 2: Run the tests and watch them fail**

Run: `DOCK_NO_MANIFEST=1 CARGO_TARGET_DIR=target-test cargo test --locked settings_pages::`
Expected: compile errors (the module does not exist).

- [ ] **Step 3: Implement `settings_pages.rs` with the types, `rows()` for the four pages, `click()`, `apply_edit()` and the moved helpers; expose `TrayRow`/`customize_rows` in `popup_tray.rs`**

- [ ] **Step 4: Run all tests and watch them pass**

Run: `DOCK_NO_MANIFEST=1 CARGO_TARGET_DIR=target-test cargo test --locked`
Expected: PASS. Unused-code warnings for items used only from Task 7 on are acceptable until then; do not add `allow` attributes.

Run: `cargo fmt --check`. Expected: no output. If it lists files, run `cargo fmt` and the tests again.

- [ ] **Step 5: Commit**

```bash
git add src/settings_pages.rs src/main.rs src/popup_tray.rs
git commit -m "settings: data-driven page model (appearance, bar, start, screen)" -m "Claude-Session: https://claude.ai/code/session_01FHR79Qfsfd7dTKsSjKFXxX"
```

---

### Task 6: Page model, part 2 (Modules with battery and inline tray chooser, Updates, About)

**Files:**
- Modify: `src/settings_pages.rs` (`rows()` for the three pages, `click()` for `Chooser`/`TrayApp`, new helpers, tests)

**Interfaces:**
- Consumes: Task 5 types, `update::{UpdateStatus, UpdateState, Release}`, `config::BatteryShow`, `Config::{show_battery, battery_percent, battery_percent_inside, update_check, update_auto_install}`.
- Produces:
  - `pub(crate) const MODULE_TOGGLES: [&str; 6] = ["notifications", "volume", "network", "metrics", "clock", "show_desktop"];` Use an explicit list rather than `config::MODULE_NAMES`, which holds "tray" and may grow.
  - `pub(crate) fn short_date(rfc3339: &str) -> &str`: the first 10 characters when they have the shape `YYYY-MM-DD`, otherwise the input unchanged.
  - `pub(crate) fn mb(bytes: usize) -> String`: `"{:.1} MB"` of bytes / 1048576.
  - `pub(crate) fn safe_release_url(url: &str) -> bool`: `url.starts_with("https://github.com/ops-software/mini-oled-dock/releases/")` and no whitespace or `"` in it.
- Modules page:
  - Header Modules, then one toggle per `MODULE_TOGGLES` entry with `module_label` and its icon: bell, speaker_icon, wifi_icon, bars, clock, show_desktop.
  - Header Battery. Show battery [battery_icon] `Choice(BatteryShow)` with options Auto/On/Off and the "Shown automatically on laptops" description. Percentage [text] toggle. Percentage inside the icon [battery_icon] toggle with the "Apple style" description, `enabled = battery_percent`.
  - Header Tray. Tray icons [ring] toggle (`Module("tray")`). When `show_tray` is on, a "Choose visible icons" [eye_icon] row with `Chooser { open: ctx.chooser_open }`. When it is open, one row per `ctx.tray` entry follows: `icon: None`, `label = name`, `desc = "No room in the bar"` when `visible && !in_bar`, control `TrayApp { key: key.clone(), visible }`. An empty tray list gives one `Note` row instead.
- Updates page:
  - Header Status. Installed version [info] `Status(installed)`. Available [refresh] `Status`, which reads "{tag} · {short_date}", or "Up to date" after a check, or "Not checked yet". Last check [clock] `Status`, which reads `short_date` or "Never". Last action [log] `Status`, which reads `last_action`, or "None yet" when it is empty (spec: "a short log line of the last action"; it stays after the state returns to `Idle`).
  - A row with `label = ""` and `Buttons`: Check now (enabled when the state is `Idle` or `Failed`), Release notes (enabled when `available` has a `safe_release_url`), Update now (enabled when `available.is_some()` and the state is `Idle` or `Failed`).
  - When the state is not `Idle`, a `Note` row with the state text (Labels table).
  - Header Automatic. Check automatically [refresh] and Install automatically [download] toggles.
- About page:
  - Header Dock: "mini-oled-dock {CARGO_PKG_VERSION}" [info] with the credits description.
  - Header Memory: Memory now [bars] `Status(mb or "—")`; After trimming [bars] `Status(mb or "Not trimmed yet")`.
  - Header Files: Data folder [folder] with `desc = data_dir` and `Buttons([OpenDataFolder "Open", enabled = data_dir.is_some()])`; Log [log], the same with `OpenLog`.
  - Header Taskbar: Restore the Windows taskbar [power] with `Buttons([ExitDock "Exit the dock", enabled true])`.
- `click`: `Chooser` returns `ToggleChooser`. `TrayApp { key, visible }` returns `TrayVisible { key: key.clone(), hidden: visible }` when `ctx.tray` still holds an entry with that `key`, otherwise `None`. Rows and `ctx.tray` are refreshed at different moments (`on_tray_changed` and the 1 s tick), so nothing may resolve a row through a position in the list.

- [ ] **Step 1: Write the failing tests**

```rust
fn tray_rows() -> Vec<TrayRow> {
    let r = |key: &str, name: &str, visible, in_bar| TrayRow { key: key.into(), name: name.into(), bitmap: None, visible, in_bar };
    vec![r("dropbox.exe#1", "Dropbox", true, true), r("slack.exe#1", "Slack", true, false), r("teams.exe#1", "Teams", false, false)]
}

#[test]
fn modules_page_lists_toggles_battery_and_tray() {
    let st = idle();
    let mut c = Config::default();
    let r = rows(Page::Modules, &c, &ctx(&[], &st));
    let names: Vec<&str> = r.iter().filter_map(|r| match r.control { Control::Toggle { setting: Setting::Module(n), .. } => Some(n), _ => None }).collect();
    assert_eq!(names, ["notifications", "volume", "network", "metrics", "clock", "show_desktop", "tray"]);
    let bat = r.iter().find(|r| matches!(r.control, Control::Choice { setting: Setting::BatteryShow, .. })).unwrap();
    assert!(matches!(&bat.control, Control::Choice { options, selected: Some(0), .. } if options.len() == 3));
    assert!(bat.desc.is_some());
    assert!(r.iter().any(|r| r.control == Control::Toggle { setting: Setting::BatteryInside, on: false, enabled: true }));
    assert!(r.iter().any(|r| r.control == Control::Chooser { open: false }));
    c.battery_percent = false;
    c.show_tray = false;
    let r = rows(Page::Modules, &c, &ctx(&[], &st));
    assert!(r.iter().any(|r| r.control == Control::Toggle { setting: Setting::BatteryInside, on: false, enabled: false }));
    assert!(!r.iter().any(|r| matches!(r.control, Control::Chooser { .. })));
}

#[test]
fn battery_choice_roundtrip() {
    let mut c = Config::default();
    for j in 0..3 {
        apply_edit(&mut c, Edit::Choose(Setting::BatteryShow, j), &[]);
        assert_eq!(selected(&c, Setting::BatteryShow, &[]), Some(j));
    }
}

#[test]
fn tray_chooser_rows_are_inline() {
    let st = idle();
    let tray = tray_rows();
    let mut cx = ctx(&[], &st);
    cx.tray = &tray;
    cx.chooser_open = true;
    let r = rows(Page::Modules, &Config::default(), &cx);
    let k = r.iter().position(|r| r.control == Control::Chooser { open: true }).unwrap();
    assert_eq!(r[k + 1].control, Control::TrayApp { key: "dropbox.exe#1".into(), visible: true });
    assert_eq!(r[k + 2].label, "Slack");
    assert!(r[k + 2].desc.is_some()); // visible but no room in the bar
    assert_eq!(click(&r[k], None, &cx), Effect::ToggleChooser);
    assert_eq!(click(&r[k + 2], None, &cx), Effect::TrayVisible { key: "slack.exe#1".into(), hidden: true });
    // Slack (the middle app) exits while the rows on screen are still [Dropbox, Slack, Teams].
    let mid: Vec<TrayRow> = tray_rows().into_iter().filter(|t| t.name != "Slack").collect();
    let mut cx2 = ctx(&[], &st);
    cx2.tray = &mid;
    assert_eq!(click(&r[k + 2], None, &cx2), Effect::None); // never Teams, its new neighbour
    assert_eq!(click(&r[k + 3], None, &cx2), Effect::TrayVisible { key: "teams.exe#1".into(), hidden: false });
    // The tail shrank: the last row acts on nothing.
    cx2.tray = &tray[..2];
    assert_eq!(click(&r[k + 3], None, &cx2), Effect::None);
    cx2.tray = &[];
    cx2.chooser_open = true;
    let empty = rows(Page::Modules, &Config::default(), &cx2);
    assert_eq!(empty.last().unwrap().control, Control::Note);
}

#[test]
fn updates_page_follows_the_state() {
    let buttons = |r: &[Row]| r.iter().find_map(|r| match &r.control { Control::Buttons(b) if b.len() == 3 => Some(b.clone()), _ => None }).unwrap();
    let mut st = idle();
    let c = Config::default();
    let b = buttons(&rows(Page::Updates, &c, &ctx(&[], &st)));
    assert_eq!(b.iter().map(|b| (b.action, b.enabled)).collect::<Vec<_>>(),
        [(Action::CheckUpdates, true), (Action::ReleaseNotes, false), (Action::InstallUpdate, false)]);
    st.available = Some(Release { tag: "v1.4.1".into(), name: "mini-oled-dock 1.4.1".into(),
        published_at: "2026-11-20T10:00:00Z".into(), notes: String::new(),
        url: "https://github.com/ops-software/mini-oled-dock/releases/tag/v1.4.1".into() });
    let r = rows(Page::Updates, &c, &ctx(&[], &st));
    assert!(r.iter().any(|r| matches!(&r.control, Control::Status(s) if s.contains("v1.4.1") && s.contains("2026-11-20"))));
    assert!(buttons(&r).iter().all(|b| b.enabled));
    st.state = UpdateState::Checking;
    let r = rows(Page::Updates, &c, &ctx(&[], &st));
    assert!(!buttons(&r)[0].enabled && !buttons(&r)[2].enabled);
    assert!(r.iter().any(|r| r.control == Control::Note));
    st.state = UpdateState::Failed("HTTP 403".into());
    let r = rows(Page::Updates, &c, &ctx(&[], &st));
    assert!(buttons(&r)[0].enabled);
    assert!(r.iter().any(|r| r.control == Control::Note && r.label.contains("HTTP 403")));
    assert!(r.iter().any(|r| r.control == Control::Toggle { setting: Setting::UpdateCheck, on: true, enabled: true }));
    assert!(r.iter().any(|r| r.control == Control::Toggle { setting: Setting::UpdateAutoInstall, on: false, enabled: true }));
    // Back to Idle: the outcome of the last action is still shown.
    st.state = UpdateState::Idle;
    st.last_action = "Could not check (HTTP 403)".into();
    let r = rows(Page::Updates, &c, &ctx(&[], &st));
    assert!(!r.iter().any(|r| r.control == Control::Note));
    assert!(r.iter().any(|r| r.control == Control::Status("Could not check (HTTP 403)".into())));
    st.last_action.clear();
    let r = rows(Page::Updates, &c, &ctx(&[], &st));
    assert!(r.iter().any(|r| r.control == Control::Status("None yet".into())));
}

#[test]
fn about_page_rows() {
    let st = idle();
    let mut cx = ctx(&[], &st);
    cx.memory = Memory { now: Some(10_276_044), trimmed: None };
    let r = rows(Page::About, &Config::default(), &cx);
    assert!(r.iter().any(|r| r.label.contains(env!("CARGO_PKG_VERSION"))));
    assert!(r.iter().any(|r| r.control == Control::Status("9.8 MB".into())));
    let open = r.iter().find_map(|r| match &r.control { Control::Buttons(b) if b[0].action == Action::OpenDataFolder => Some(b[0].enabled), _ => None });
    assert_eq!(open, Some(false));
    assert!(r.iter().any(|r| matches!(&r.control, Control::Buttons(b) if b[0].action == Action::ExitDock && b[0].enabled)));
}

#[test]
fn small_formatters() {
    assert_eq!(short_date("2026-11-20T10:00:00Z"), "2026-11-20");
    assert_eq!(short_date(""), "");
    assert_eq!(short_date("garbage"), "garbage");
    assert_eq!(mb(10_276_044), "9.8 MB");
    assert_eq!(mb(0), "0.0 MB");
    assert!(safe_release_url("https://github.com/ops-software/mini-oled-dock/releases/tag/v1.4.1"));
    assert!(!safe_release_url("https://evil.example/releases/"));
    assert!(!safe_release_url("C:\\Windows\\System32\\cmd.exe"));
    assert!(!safe_release_url("https://github.com/ops-software/mini-oled-dock/releases/x\" --flag"));
}
```

- [ ] **Step 2: Run the tests and watch them fail**

Run: `DOCK_NO_MANIFEST=1 CARGO_TARGET_DIR=target-test cargo test --locked settings_pages::`
Expected: the new tests FAIL (empty rows, missing helpers).

- [ ] **Step 3: Implement the three pages, the helpers and the two `click` arms as specified in Interfaces**

- [ ] **Step 4: Run the tests and watch them pass**

Run: `DOCK_NO_MANIFEST=1 CARGO_TARGET_DIR=target-test cargo test --locked settings_pages::`
Expected: PASS.

Run: `cargo fmt --check`. Expected: no output. If it lists files, run `cargo fmt` and the tests again.

- [ ] **Step 5: Commit**

```bash
git add src/settings_pages.rs
git commit -m "settings: modules (battery, inline tray chooser), updates and about pages" -m "Claude-Session: https://claude.ai/code/session_01FHR79Qfsfd7dTKsSjKFXxX"
```

---

### Task 7: View (layout, painting, hit ids, scrolling, mini bar, preview pixels)

**Files:**
- Create: `src/settings_view.rs`
- Modify: `src/main.rs:33-67` (add `mod settings_view;`)
- Test: `#[cfg(test)] mod tests` in `src/settings_view.rs`

**Interfaces:**
- Consumes: Task 1 (`Font::Title`, `Font::Caption`, `blend_pixels`, `popup::badge`), Task 2 glyphs, Task 3 `render::paint_offscreen` and `preview::mock_icon`, Tasks 5–6 (`Page`, `Row`, `Control`, `Ctx`, `hsv_to_rgb`, `preset_index`, …). Also the popup helpers `popup::{HitMap, row_plate, lift, toggle_switch, hline, center_y, paint_slider, slider_value_at}`.
- Produces:

```rust
pub(crate) const NAV_W: i32 = 200;          // logical px; physical = Metrics::px
pub(crate) const ROW_H: i32 = 44;            // normal row
pub(crate) const HEADER_H: i32 = 32;         // NOTE_H 32, TRAY_ROW_H 36, PREVIEW_H 120, PICKER_H 170
pub(crate) const WRAPPED_ROW_H: i32 = 72;    // a choice whose segments go on a second line
pub(crate) const PAGE_MAX_W: i32 = 640;      // PAGE_PAD_X 32, PAGE_TOP 24, title block 64
pub(crate) const SCROLL_STEP: i32 = 48;      // per wheel notch
pub(crate) const CAPTION_H: i32 = 32;        // fake caption, preview PNG only

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Focus { Nav, Page }
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Hit { Nav(Page), Row(usize, Option<usize>) }
pub(crate) fn hit_id(h: Hit) -> u32;          // Nav k -> k + 1; Row(i, part) -> 1000 + i * 16 + part.map_or(0, |j| j + 1)
pub(crate) fn decode(id: u32) -> Option<Hit>;

pub(crate) struct BarPixels { pub w: i32, pub h: i32, pub px: Vec<u32> }
pub(crate) struct Frame<'a> {
    pub page: Page, pub rows: &'a [Row], pub tray: &'a [TrayRow], pub hot: Option<u32>, pub pressed: Option<u32>,
    pub focus: Focus, pub hsv: (f32, f32, f32), pub bar: Option<&'a BarPixels>, pub badge: bool,
}
#[derive(Default)]
pub(crate) struct Geometry {
    pub hits: HitMap, pub nav: Vec<(Page, RECT)>, pub rows: Vec<RECT>, pub viewport: RECT,
    pub content_h: i32, pub scroll: i32, pub sv: RECT, pub hue: RECT, pub knob_r: i32, pub preview: RECT,
}
pub(crate) fn layout(s: &Surface, client: (i32, i32), f: &Frame<'_>, scroll: i32) -> Geometry;
pub(crate) fn paint(s: &mut Surface, g: &Geometry, f: &Frame<'_>);
pub(crate) fn clamp_scroll(scroll: i32, content_h: i32, viewport_h: i32) -> i32;
pub(crate) fn label_right(s: &Surface, row: &RECT, label: &str) -> i32;
pub(crate) fn segments_left(s: &Surface, row: &RECT, options: &[&str]) -> i32;
pub(crate) fn sv_at(sv: RECT, x: i32, y: i32) -> (f32, f32);   // (saturation, value), clamped 0..=1
pub(crate) fn hue_at(hue: RECT, x: i32) -> f32;                 // 0.0..=359.9
pub(crate) fn mini_bar(m: Metrics, c: &Config, max_w: i32) -> Option<BarPixels>;
pub(crate) fn preview_pixels(m: Metrics, page: Page, c: &Config, ctx: &Ctx<'_>) -> Option<(i32, i32, Vec<u32>)>;
```

- Layout decisions (logical px):
  - The nav column is 200 wide and filled with WHITE at alpha 8, with a 1 px `SEP_ALPHA` line at its right edge. Entries are 36 high with an 8 inset: icon 16 at x 20, label at x 48. The badge sits right-aligned at 200 − 16. The top entries start at y 12. About sits at the bottom (bottom − 8), with Updates above it.
  - Current nav entry (spec: "a 3 px white bar on the left and the hover fill"): `s.plate(rc, m.px(ROW_RADIUS), HOVER_ALPHA)` is drawn on it whether or not it is hot (`PRESS_ALPHA` while pressed), then a bar 3 wide, WHITE at alpha 255, radius 1, from `rc.top + 10` to `rc.bottom − 10`, at `x = rc.left`. Its label and icon use `lift(TEXT_ALPHA, true)`.
  - Page area: x from `NAV_W + PAGE_PAD_X`, width `min(client_w − NAV_W − 2·PAGE_PAD_X, PAGE_MAX_W)`. The title (`Font::Title`) and subtitle scroll with the content. `viewport` is the page area below y 0, clipped with `Surface::set_clip`.
  - Row anatomy: hover plate over the row rect. Icon 16 centred in a 20 column at x + 12. Label at x + 44, with the optional description below it. Control right-aligned at right − 12.
  - Segments are 24 high, `Font::Small`, 9 px horizontal padding, at least 30 wide, drawn exactly like `popup_settings.rs:820-858`. When `segments_left < label_right + 6`, the row wraps: height `WRAPPED_ROW_H`, segments right-aligned on a second line.
  - Slider: track 180 wide, value text in a 52 column to its right; hit part 0 is the track band.
  - Swatches: diameter 20, 6 apart, ring as in `popup_settings.rs:637-671` (`paint_swatches`).
  - The picker spans the row's text column: SV area 112 high, hue strip 12, hex row 28. The drawing comes from `popup_settings.rs:673-702` (`paint_picker`). Hit part 0 is SV, part 1 is hue.
  - Buttons: pills 28 high, `Font::Normal`, 14 px padding, 8 apart. Fill WHITE alpha 30, 46 on hover, `PRESS_ALPHA` when pressed. Disabled buttons have text at `TEXT3_ALPHA` and no hit.
  - Chooser: `chevron_right_shape`, or `chevron_down_shape` when open. Tray app rows: the 16 px bitmap of the `Frame.tray` entry with the row's `key` (no bitmap when that entry is gone; never looked up by position), then the name, `toggle_switch` at the right. The spec's "eye toggles" are today's chooser's pill toggles (`popup_tray.rs`), kept as they are; `eye_icon` marks only the "Choose visible icons" row.
  - `Status`: text in `TEXT2_ALPHA`, right-aligned. `Note`: label in `Font::Caption`, `TEXT2_ALPHA`, full width.
  - The preview card is 120 high, radius 10, filled through `fill_fn` with a vertical gradient from #2a2f3a to #10131a. `bar` is blended centred on it with `blend_pixels`.
  - Focus: with `Focus::Nav`, a 1 px ring at alpha 110 surrounds the current nav entry.
  - The window background is the opaque config `background`; every pixel is written, as `copy_to` needs.
- `mini_bar`: three sample apps, built with `mock_icon` at `BarSizes::from_config(c).icon_px(m)`: ("Files", 'F', 0xE8A317, Active), ("Browser", 'B', 0x0F6CBD, Open) and ("Mail", 'M', 0xD44638, None, dimmed). Modules `[Volume, Clock]`, no tray, `start_button: false`, time "14:32", `date: Some("Fri 23 Oct")` when `show_date`, volume 60 with the config's percent, `max_width = max_w`. It goes through `render::paint_offscreen`.
- `preview_pixels`: a client of `m.px(860) × m.px(600)` under a fake caption `m.px(CAPTION_H)` high. The caption uses the background colour, the window title in `Font::Normal` at x 12, `close_shape` at the right and a 1 px `PANEL_BORDER_ALPHA` border around the whole image. Nav focus, no hover, `badge = ctx.update.available.is_some()`. For Appearance it also includes the `mini_bar`.

- [ ] **Step 1: Write the failing tests**

```rust
const M: Metrics = Metrics { dpi: 96 };

fn frame<'a>(page: Page, rows: &'a [Row]) -> Frame<'a> {
    Frame { page, rows, tray: &[], hot: None, pressed: None, focus: Focus::Nav, hsv: (0.0, 0.0, 0.0), bar: None, badge: false }
}
// `idle_status()` and `test_ctx(&st)`: copies of the `idle()`/`ctx(&[], &st)` helpers of settings_pages tests.

#[test]
fn hit_ids_roundtrip() {
    for p in Page::ALL { assert_eq!(decode(hit_id(Hit::Nav(p))), Some(Hit::Nav(p))); }
    for h in [Hit::Row(0, None), Hit::Row(12, Some(7)), Hit::Row(3, Some(14))] { assert_eq!(decode(hit_id(h)), Some(h)); }
    assert_eq!(decode(0), None);
}

#[test]
fn layout_places_nav_and_rows() {
    let s = Surface::new(M).expect("surface");
    let st = idle_status();
    let rows = settings_pages::rows(Page::Appearance, &Config::default(), &test_ctx(&st));
    let g = layout(&s, (860, 600), &frame(Page::Appearance, &rows), 0);
    assert_eq!(g.nav.len(), 7);
    let about = g.nav.iter().find(|(p, _)| *p == Page::About).unwrap().1;
    assert_eq!(about.bottom, 600 - 8);
    assert!(g.nav.iter().all(|(_, r)| r.right <= NAV_W));
    assert_eq!(g.rows.len(), rows.len());
    for w in g.rows.windows(2) { assert!(w[0].bottom <= w[1].top); }
    for (r, rc) in rows.iter().zip(&g.rows) {
        assert!(rc.left >= NAV_W && rc.right - rc.left <= PAGE_MAX_W);
        if matches!(r.control, Control::Toggle { .. } | Control::Slider { .. } | Control::Buttons(_)) { assert_eq!(rc.bottom - rc.top, ROW_H); }
    }
    assert_eq!(g.hits.at(about.left + 2, about.top + 2), Some(hit_id(Hit::Nav(Page::About))));
}

#[test]
fn choices_never_cover_their_label() {
    let s = Surface::new(M).expect("surface");
    let row = RECT { left: NAV_W + 32, top: 0, right: 640 - 32, bottom: ROW_H }; // minimum window width
    // The widest labels in both languages, with their options as they are drawn.
    for (label, opts) in [
        ("Display mode", vec!["Icon + name", "Icon", "Name"]), ("Mostra", vec!["Icona + nome", "Icona", "Nome"]),
        ("Start layout", vec!["Vertical", "Horizontal"]), ("Layout Start", vec!["Verticale", "Orizzontale"]),
        ("Mostra batteria", vec!["Automatica", "S\u{ec}", "No"]), ("Lingua", vec!["Sistema", "English", "Italiano"]),
        ("Posizione", vec!["In alto", "In basso"]),
    ] {
        assert!(label_right(&s, &row, label) + 6 <= segments_left(&s, &row, &opts), "{label} does not fit inline");
    }
    // Six displays + Pointer do not fit inline: the row wraps, segments stay inside it, below the label.
    let six: Vec<&'static str> = vec!["Principale", "2", "3", "4", "5", "6", "Puntatore"];
    assert!(label_right(&s, &row, "Schermo") + 6 > segments_left(&s, &row, &six));
    let ctl = Control::Choice { setting: Setting::Monitor, options: six.clone(), selected: Some(0) };
    let rows = [Row { icon: Some(crate::glyphs::monitor_shape), label: "Schermo".into(), desc: None, control: ctl }];
    let g = layout(&s, (640, 460), &frame(Page::Screen, &rows), 0);
    let rc = g.rows[0];
    assert_eq!(rc.bottom - rc.top, WRAPPED_ROW_H);
    for j in 0..six.len() {
        let seg = g.hits.rect(hit_id(Hit::Row(0, Some(j)))).unwrap();
        assert!(seg.left >= rc.left && seg.right <= rc.right && seg.top >= rc.top + ROW_H / 2);
    }
}

#[test]
fn current_nav_entry_is_marked() {
    let mut s = Surface::new(M).expect("surface");
    s.resize(860, 600).unwrap();
    let st = idle_status();
    let rows = settings_pages::rows(Page::Bar, &Config::default(), &test_ctx(&st));
    let f = Frame { focus: Focus::Page, ..frame(Page::Bar, &rows) }; // no focus ring in the way
    let g = layout(&s, (860, 600), &f, 0);
    paint(&mut s, &g, &f);
    let (w, _, px) = s.snapshot_pixels().unwrap();
    let at = |x: i32, y: i32| px[(y * w + x) as usize];
    let nav = |p: Page| g.nav.iter().find(|(q, _)| *q == p).unwrap().1;
    let (cur, other) = (nav(Page::Bar), nav(Page::Appearance));
    let cy = |r: RECT| (r.top + r.bottom) / 2;
    assert_eq!(at(cur.left + 1, cy(cur)), 0xFFFF_FFFF); // the 3 px white bar
    assert_ne!(at(other.left + 1, cy(other)), 0xFFFF_FFFF);
    assert!(at(cur.right - 6, cy(cur)) & 0xFF > at(other.right - 6, cy(other)) & 0xFF); // the hover fill, not hot
}

#[test]
fn scroll_and_picker_mapping() {
    assert_eq!(clamp_scroll(-5, 1000, 400), 0);
    assert_eq!(clamp_scroll(900, 1000, 400), 600);
    assert_eq!(clamp_scroll(50, 300, 400), 0);
    let sv = rect_wh(10, 10, 101, 101);
    assert_eq!(sv_at(sv, 10, 10), (0.0, 1.0));
    assert_eq!(sv_at(sv, 110, 110), (1.0, 0.0));
    assert_eq!(sv_at(sv, 500, -50), (1.0, 1.0));
    let hue = rect_wh(0, 0, 361, 12);
    assert_eq!(hue_at(hue, -5), 0.0);
    assert!((hue_at(hue, 360) - 359.9).abs() < 0.01);
}

#[test]
fn preview_pixels_have_the_window_size() {
    let st = idle_status();
    let c = Config::default();
    for (dpi, w, h) in [(96, 860, 632), (144, 1290, 948)] {
        let (pw, ph, px) = preview_pixels(Metrics { dpi }, Page::Appearance, &c, &test_ctx(&st)).expect("pixels");
        assert_eq!((pw, ph), (w, h));
        assert_eq!(px.len(), (w * h) as usize);
    }
    let bar = mini_bar(M, &c, 600).expect("mini bar");
    assert_eq!(bar.h, crate::render::bar_height(M, crate::render::BarSizes::from_config(&c)));
}
```

- [ ] **Step 2: Run the tests and watch them fail**

Run: `DOCK_NO_MANIFEST=1 CARGO_TARGET_DIR=target-test cargo test --locked settings_view::`
Expected: compile errors (the module does not exist).

- [ ] **Step 3: Implement `settings_view.rs` as specified in Interfaces and the layout decisions**

Letter-spaced headers are drawn one character at a time (`text_width` + 1 logical px), upper-cased with `to_uppercase()`.

- [ ] **Step 4: Run the tests and watch them pass**

Run: `DOCK_NO_MANIFEST=1 CARGO_TARGET_DIR=target-test cargo test --locked settings_view::`
Expected: PASS.

Run: `cargo fmt --check`. Expected: no output. If it lists files, run `cargo fmt` and the tests again.

- [ ] **Step 5: Commit**

```bash
git add src/settings_view.rs src/main.rs
git commit -m "settings: view layout and painting, mini live bar, preview pixels" -m "Claude-Session: https://claude.ai/code/session_01FHR79Qfsfd7dTKsSjKFXxX"
```

---

### Task 8: The Settings window

**Files:**
- Create: `src/settings_window.rs` (replaces a plan-02 stub if there is one)
- Modify: `src/msg.rs:67` (after `WM_DOCK_START_MENU`: `WM_DOCK_OPEN_SETTINGS`), `src/msg.rs:134` (after `TIMER_POPUP_FOCUS`: the settings timers)
- Modify: `src/main.rs:33-67` (`mod settings_window;`), `:100-104` (`use msg::…`), `:235` (trim gate), `:354-358` (`WM_DOCK_TRAY_CHANGED`), `:368-371` (add the `WM_DOCK_OPEN_SETTINGS` arm next to `WM_DOCK_CONFIG_APPLY`), `:664` (cleanup)
- Modify: `src/ui.rs:730-772` (`on_config_changed` calls `crate::settings_window::on_config_changed()` last), `src/ui.rs:2668` (`fn dock_hmonitor` becomes `pub(crate)`)
- Modify: `src/apps.rs:1065-1073` (class filter goes through a new pure `excluded_class`), `src/apps.rs:1091+` (test)
- Test: `#[cfg(test)] mod tests` in `src/settings_window.rs`, plus the `apps.rs` test

**Interfaces:**
- Consumes: Tasks 4–7, `ui::{screens, force_foreground, monitor_info, launch_target_str, dock_hmonitor}`, `config::{get, set, modify}`, `start_import::{start, reset, status}`, `update::{check_now, install_now, status}`, `tray_host::set_user_hidden`, `popup_tray::customize_rows`, `power::run(PowerAction::ExitDock)`, `shell_thread::request(ShellRequest::Launch { target, args })`, `datadir::{config_path, log_path}`, `mem::{private_bytes, last_hidden_trim}`.
- Produces (`msg.rs`):
  - `pub const WM_DOCK_OPEN_SETTINGS: u32 = WM_APP + 40;`, documented as: popups, the quick-links menu and `--settings` post it to the dock window; wParam = lParam = 0; `main` calls `settings_window::open()`.
  - `pub const TIMER_SETTINGS_TICK: usize = 50;`: 1 s, on the settings window, only while it is open.
  - `pub const TIMER_SETTINGS_APPLY: usize = 51;`: one-shot flush of a rebuild the throttle skipped.
  - Why 50/51: `msg.rs` is the shared timer registry, and `ui.rs` keeps private dock-window timers at 40, 41 and 42 (`TIMER_WIDTH`, `TIMER_ATTN`, `TIMER_VIS`, `ui.rs:123-137`). Timer ids are per HWND, so equal ids would not clash, but 50/51 keep the registry unambiguous. Say so in the doc comment. `WM_APP + 40` is free (`msg.rs` uses +1..+9, +11..+16 and +20..+22; +10 is reserved).
- Produces (public API, UI thread only):
  - `pub fn open()`: creates the window, or restores (`SW_RESTORE` when `IsIconic`) and activates the existing one with `ui::force_foreground`.
  - `pub fn close(reason: &str)`: runs `finish`, `DestroyWindow`, `UnregisterClassW` and drops the state. Idempotent.
  - `pub fn is_open() -> bool`.
  - `pub fn on_config_changed()`: re-reads the config, rebuilds rows, re-applies the DWM colours and the title (language) and repaints. `hsv` is re-read from the background unless the picker is being dragged.
  - `pub fn on_tray_changed()`: re-reads `customize_rows()` when the chooser is open.
  - `pub fn shutdown()`: `close("shutdown")`.
- Produces (pure, tested):
  - `fn initial_rect(work: RECT, dpi: u32, logical: (i32, i32), frame: RECT) -> RECT`. `frame` is `AdjustWindowRectExForDpi` applied to an empty rect (negative left/top insets). The client size is `logical` at `dpi`, capped to the work area minus the frame but never below the minimum. The window is centred on the work area; if it is bigger than the work area, its top-left corner sits at the work area's top-left.
  - `fn logical_size(client_w: i32, client_h: i32, dpi: u32) -> [u16; 2]`: rounded and raised to at least the minimum.
  - `fn min_track(dpi: u32, frame: RECT) -> (i32, i32)`.
  - `fn colorref(bg: Background) -> u32`: 0x00BBGGRR.
  - `fn key_action(vk: u16, shift: bool, focus: Focus, page: Page) -> (Focus, KeyAction)` with `#[derive(Debug, PartialEq, Eq)] enum KeyAction { None, Close, Redraw, Goto(Page), Scroll(i32) }`.
  - `#[derive(Default)] struct Pending { unsaved: bool, apply_pending: bool, last_apply: u64 }` with `fn changed(&mut self, save: bool, now: u64) -> bool` (true = post `WM_DOCK_CONFIG_APPLY` now) and `fn finish(&mut self) -> Finish`, where `#[derive(Debug, PartialEq, Eq)] enum Finish { Nothing, Save, Apply, SaveAndApply }`. `const APPLY_THROTTLE_MS: u64 = 30;`. The rules mirror `popup_settings.rs:553-592` (`change`/`finish`):
    - `changed(false, now)` (a drag move, value set with `config::set`): `unsaved = true`; when `now − last_apply >= 30` it sets `last_apply = now`, clears `apply_pending` and returns true; otherwise it sets `apply_pending = true` and returns false, and the caller arms `TIMER_SETTINGS_APPLY` for the rest of the 30 ms + 5.
    - `changed(true, now)` (a click, already written by `config::modify`) bypasses the throttle: it clears `unsaved` and `apply_pending`, sets `last_apply = now` and always returns true. `config::modify` writes the whole in-memory snapshot, so it also saves an unsaved drag value.
    - `finish()` clears both flags and returns `SaveAndApply` when both were set, `Save` when only `unsaved` was, `Apply` when only `apply_pending` was, else `Nothing`.
    - The caller runs `Save` as `config::modify(|_| {})` (`config.rs:1048-1055` only writes the file and sets the snapshot; the bar already shows the value). `Apply` posts `WM_DOCK_CONFIG_APPLY`. `SaveAndApply` does both, write first. `Apply` and `SaveAndApply` also `KillTimer(TIMER_SETTINGS_APPLY)`.
- Produces (`apps.rs`): `fn excluded_class(class: &str) -> bool` covers `Progman`, `WorkerW`, `Windows.UI.Core.CoreWindow`, `Windows.Internal.Shell.TabProxyWindow` and `DockSettings`. The Settings window is a dock window, not a bar app.
- Window behaviour:
  - Opening: register the class on first open (`CS_HREDRAW | CS_VREDRAW`, arrow cursor, no background brush). Read the DPI from `GetDpiForMonitor(ui::dock_hmonitor())` and the work area from `ui::monitor_info`. Compute `initial_rect(…, config::get().settings_size(), …)`. Create the window with `CreateWindowExW(0, "DockSettings", title, WS_OVERLAPPEDWINDOW & !WS_MAXIMIZEBOX, …)` and no owner, so it can sit behind the bar and popups. Then set the DWM attributes, call `start_import::reset()`, `ShowWindow(SW_SHOW)` and `ui::force_foreground`. Finally arm `TIMER_SETTINGS_TICK` at 1000 ms.
  - Painting: `WM_PAINT` only `copy_to`s the surface; `repaint()` paints into the surface and invalidates. `WM_ERASEBKGND` returns 1. `WM_SIZE` resizes the surface to the client size and re-lays out. `WM_DPICHANGED` builds a new `Surface` at the new DPI, applies the suggested `RECT` with `SetWindowPos` and re-lays out. `WM_GETMINMAXINFO` sets `ptMinTrackSize = min_track(GetDpiForWindow, frame)`. `WM_EXITSIZEMOVE` saves `logical_size(client)` through `config::modify` when it changed, with no `WM_DOCK_CONFIG_APPLY`.
  - Mouse:
    - `WM_MOUSEMOVE` tracks the hot id (`TrackMouseEvent`) and drags while pressed. `WM_MOUSELEAVE` clears the hot id.
    - `WM_LBUTTONDOWN` takes `SetCapture`, sets pressed and starts a slider or picker drag immediately. Slider values come from `popup::slider_value_at` on the track hit rect; the picker maps through `sv_at`/`hue_at`. A drag move applies `apply_edit` to a clone of `config::get()`, stores it with `config::set` and calls `pending.changed(false, now)` (post `WM_DOCK_CONFIG_APPLY` when it returns true).
    - `WM_LBUTTONUP` releases capture. When the release lands on the pressed id it calls `click` and runs the `Effect`; then it always runs `finish`. `WM_CAPTURECHANGED` runs `finish` too.
    - `WM_SETCURSOR` shows the hand over enabled targets.
    - `WM_MOUSEWHEEL` accumulates `WHEEL_DELTA` and scrolls by `SCROLL_STEP` per notch, clamped. It scrolls also over a slider: the popup's wheel-over-slider steps are dropped (Global Constraints, Keyboard).
  - Keys: `WM_KEYDOWN` goes through `key_action`. `Goto` switches the page: scroll 0, chooser closed, `picker_open = (page == Appearance && preset_index(bg).is_none())`, hsv re-read.
  - Closing: `WM_CLOSE` calls `close("title bar")` and returns 0.
  - Ticks: `TIMER_SETTINGS_TICK` rebuilds `Ctx` (import status, `update::status()`, memory) and the rows, and repaints only when `!same_rows`. `TIMER_SETTINGS_APPLY` kills itself and, when `apply_pending` is still set, clears it, sets `last_apply = now` and posts `WM_DOCK_CONFIG_APPLY` (the file is still written by `finish`).
  - Effects:
    - `Save(e)`: `config::modify(|c| apply_edit(c, e, &screens))`. On error, log it and apply in memory with `config::set`. Then `pending.changed(true, now)` (always true) and post `WM_DOCK_CONFIG_APPLY`.
    - `TogglePicker` and `ToggleChooser` flip the flag and re-lay out; `ToggleChooser` re-reads `customize_rows()` when it opens.
    - `TrayVisible` calls `tray_host::set_user_hidden(&key, hidden)`.
    - `Run(ImportStart)` calls `start_import::start()`. `Run(CheckUpdates)` calls `update::check_now()`. `Run(InstallUpdate)` calls `update::install_now()`.
    - `Run(ReleaseNotes)` calls `ui::launch_target_str(url)` only if `safe_release_url(url)`.
    - `Run(OpenDataFolder)` sends `ShellRequest::Launch { target: "explorer.exe".into(), args: Some(format!("\"{dir}\"")) }`, where `dir` is the parent of `config_path()`. `Run(OpenLog)` calls `ui::launch_target_str(log_path)`.
    - `Run(ExitDock)` calls `close("exit")` and then `power::run(PowerAction::ExitDock)`.
  - Mini bar cache: `Option<(Arc<Config>, u32, BarPixels)>`, rebuilt only on the Appearance page when the config `Arc` (`Arc::ptr_eq`) or the DPI changed, with `max_w` = the preview card width − 32 logical.
  - State: a `thread_local! RefCell<Option<Win>>`, taken with `try_borrow_mut`; a re-entry is logged and ignored. No borrow is held across `SetWindowPos`, `ShowWindow`, `DestroyWindow` or `force_foreground`.
- Wiring:
  - `main::wnd_proc` handles `WM_DOCK_OPEN_SETTINGS => { settings_window::open(); LRESULT(0) }`.
  - `WM_DOCK_TRAY_CHANGED` also calls `settings_window::on_tray_changed()`.
  - `cleanup` calls `settings_window::shutdown()` right after `picker::shutdown()`.
  - `trim_tick` treats an open window like an open popup: `let popup = popup::is_open() || settings_window::is_open();`.

- [ ] **Step 1: Write the failing tests**

```rust
// src/settings_window.rs
#[cfg(test)]
mod tests {
    use super::*;
    use windows::Win32::UI::Input::KeyboardAndMouse::{VK_DOWN, VK_ESCAPE, VK_TAB, VK_UP};
    const FRAME: RECT = RECT { left: -8, top: -31, right: 8, bottom: 8 };

    #[test]
    fn initial_rect_centres_and_clamps() {
        let work = RECT { left: 0, top: 0, right: 1920, bottom: 1040 };
        let r = initial_rect(work, 96, (860, 600), FRAME);
        assert_eq!((r.right - r.left, r.bottom - r.top), (876, 639));
        assert_eq!((r.left, r.top), ((1920 - 876) / 2, (1040 - 639) / 2));
        let r = initial_rect(work, 144, (860, 600), FRAME);
        assert_eq!((r.right - r.left, r.bottom - r.top), (1290 + 16, 900 + 39));
        // Saved on a 4K display, opened on a small second display.
        let small = RECT { left: 1920, top: 0, right: 3286, bottom: 728 };
        let r = initial_rect(small, 96, (3000, 2000), FRAME);
        assert!(r.left >= small.left && r.top >= small.top && r.right <= small.right && r.bottom <= small.bottom);
        // Work area below the minimum: minimum size, title bar at the top-left of the work area.
        let tiny = RECT { left: 0, top: 0, right: 600, bottom: 400 };
        let r = initial_rect(tiny, 96, (860, 600), FRAME);
        assert_eq!((r.left, r.top, r.right - r.left, r.bottom - r.top), (0, 0, 640 + 16, 460 + 39));
    }

    #[test]
    fn logical_size_survives_dpi_changes() {
        assert_eq!(logical_size(1290, 900, 144), [860, 600]);
        assert_eq!(logical_size(1075, 750, 120), [860, 600]);
        assert_eq!(logical_size(1291, 901, 144), [861, 601]);
        assert_eq!(logical_size(100, 100, 96), [640, 460]);
        assert_eq!(min_track(144, FRAME), (960 + 16, 690 + 39));
    }

    #[test]
    fn caption_colours_are_colorrefs() {
        assert_eq!(colorref(Background(0x112233)), 0x0033_2211);
        assert_eq!(colorref(Background::BLACK), 0);
    }

    #[test]
    fn keyboard_moves_between_nav_and_page() {
        use Focus::{Nav, Page as P};
        let k = |vk: u16, shift, f, p| key_action(vk, shift, f, p);
        assert_eq!(k(VK_ESCAPE.0, false, Nav, Page::Bar), (Nav, KeyAction::Close));
        assert_eq!(k(VK_ESCAPE.0, false, P, Page::Bar), (P, KeyAction::Close));
        assert_eq!(k(VK_TAB.0, false, Nav, Page::Bar), (P, KeyAction::Redraw));
        assert_eq!(k(VK_TAB.0, true, P, Page::Bar), (Nav, KeyAction::Redraw));
        assert_eq!(k(VK_DOWN.0, false, Nav, Page::Appearance), (Nav, KeyAction::Goto(Page::Bar)));
        assert_eq!(k(VK_UP.0, false, Nav, Page::Appearance), (Nav, KeyAction::None));
        assert_eq!(k(VK_DOWN.0, false, Nav, Page::About), (Nav, KeyAction::None));
        assert_eq!(k(VK_DOWN.0, false, P, Page::Bar), (P, KeyAction::Scroll(1)));
        assert_eq!(k(VK_UP.0, false, P, Page::Bar), (P, KeyAction::Scroll(-1)));
        assert_eq!(k(0x41, false, Nav, Page::Bar), (Nav, KeyAction::None));
    }

    #[test]
    fn pending_saves_on_finish() {
        let mut p = Pending::default();
        assert!(p.changed(false, 100));       // first drag move: rebuild now
        assert_eq!(p.finish(), Finish::Save); // released: the bar already shows it, only the file is written
        assert_eq!(p.finish(), Finish::Nothing);
        assert!(p.changed(false, 200));
        assert!(!p.changed(false, 210));      // inside the 30 ms throttle: skipped
        assert!(p.unsaved && p.apply_pending);
        assert_eq!(p.finish(), Finish::SaveAndApply); // release, Esc, X or exit: file written AND bar rebuilt
        assert_eq!(p.finish(), Finish::Nothing);
        assert!(p.changed(false, 300));
        assert!(!p.changed(false, 310));      // skipped
        assert!(p.changed(true, 320));        // a click inside the throttle window: saved and applied at once
        assert!(!p.unsaved && !p.apply_pending);
        assert_eq!(p.finish(), Finish::Nothing);
        p.apply_pending = true;               // only a skipped rebuild left: finish applies it
        assert_eq!(p.finish(), Finish::Apply);
    }
}

// src/apps.rs tests
#[test]
fn settings_window_is_not_an_app() {
    assert!(excluded_class("DockSettings") && excluded_class("Progman") && excluded_class("WorkerW"));
    assert!(!excluded_class("Notepad"));
}
```

- [ ] **Step 2: Run the tests and watch them fail**

Run: `DOCK_NO_MANIFEST=1 CARGO_TARGET_DIR=target-test cargo test --locked settings_window:: apps::tests::settings_window`
Expected: compile errors.

- [ ] **Step 3: Implement `settings_window.rs`, the `msg.rs` constants and the wiring in `main.rs`, `ui.rs` and `apps.rs` as specified in Interfaces**

- [ ] **Step 4: Run all tests and watch them pass**

Run: `DOCK_NO_MANIFEST=1 CARGO_TARGET_DIR=target-test cargo test --locked`
Expected: PASS.

Run: `cargo fmt --check`. Expected: no output. If it lists files, run `cargo fmt` and the tests again.

- [ ] **Step 5: Commit**

```bash
git add src/settings_window.rs src/msg.rs src/main.rs src/ui.rs src/apps.rs
git commit -m "settings: DockSettings window with DWM dark frame, DPI, keyboard and live apply" -m "Claude-Session: https://claude.ai/code/session_01FHR79Qfsfd7dTKsSjKFXxX"
```

---

### Task 9: Entry points (power popup, Start flyout, quick-links menu, `--settings`)

**Files:**
- Modify: `src/popup_power.rs:1-3` (doc), `:33-40` (`Choice::Settings` doc), `:77-80` (helpers), `:88-123` (fields `pending: Option<Choice>` and `update_badge: bool`), `:166-193` (badge), `:212-230` (`click`/`after_close`), plus a new `#[cfg(test)] mod tests`
- Modify: `src/popup_startmenu.rs:505` (`power_pending: Option<Choice>`), `:705-715` (`choose_power`), `:1184-1188` (`after_close`), `:1356-1361` (test)
- Modify: `src/popup_start.rs:22-61` (`Act::DockSettings` and its row), `:133-146` (`after_close`), plus a new `#[cfg(test)] mod tests`
- Modify: `src/main.rs:133-166` (`Args::settings`), `:176-205` (`parse_args_from`), `:831-840` (`usage`), `:912-920` (CLI branch after `--restore`), plus a new `#[cfg(test)] mod tests`

**Interfaces:**
- Consumes: `msg::WM_DOCK_OPEN_SETTINGS`, `popup::badge`, `update::status()`.
- Produces:
  - `pub(crate) fn open_settings_window()` in `popup_power.rs`: posts `WM_DOCK_OPEN_SETTINGS` to `crate::hwnd()` and logs a failure.
  - `pub(crate) fn run_choice(c: Choice)`: `Run(a)` calls `power::run(a)`; `Settings` calls `open_settings_window()`.
  - `Choice::Settings` now means "close the popup, then open the Settings window".
  - The power popup and the Start flyout answer `Reply::Close` for it and call `run_choice` in `after_close`.
  - `PowerPopup::update_badge` is set in `new()` from `crate::update::status().available.is_some()`. When true, `badge(s, rc.right - pad_text - chevron_w - m.px(6), centre, 1)` is drawn on the Settings row. Preview pixels do not change, because the preview has no available update.
  - Quick-links row `Some((Act::DockSettings, "Dock settings", "Impostazioni dock"))` sits right after the `ms-settings:` row. `ROWS` becomes `[Option<Row>; 19]`. `after_close` calls `crate::popup_power::open_settings_window()`.
  - `pub settings: bool` in `Args`, set by `"--settings"`.
  - `fn parse_args_from(it: impl Iterator<Item = String>) -> Result<Args, String>`, with `parse_args()` = `parse_args_from(std::env::args().skip(1))`.
  - `fn open_settings_cli() -> u8`: `FindWindowW(w!("DockWindow"), PCWSTR::null())`. If nothing is found it writes "dock: no running dock found" to stderr and returns 1. Otherwise it calls `GetWindowThreadProcessId` → `AllowSetForegroundWindow(pid)` → `PostMessageW(WM_DOCK_OPEN_SETTINGS)` and returns 0, or 1 if the post fails. `main` runs it with `attach_console()` before the single-instance check. `usage()` lists `[--settings]`.

- [ ] **Step 1: Write the failing tests**

```rust
// src/popup_power.rs
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn settings_closes_the_popup_then_opens_the_window() {
        let mut p = PowerPopup { hits: HitMap::default(), pending: None, sep_y: 0, pad_text: 0, width: 0,
            autostart: Arc::new(AtomicU8::new(AS_OFF)), seen: AS_OFF, update_badge: false };
        assert_eq!(p.click(ID_SETTINGS), Reply::Close);
        assert_eq!(p.pending, Some(Choice::Settings));
        assert_eq!(p.click(ID_LOCK), Reply::Close);
        assert_eq!(p.pending, Some(Choice::Run(PowerAction::Lock)));
    }
}

// src/popup_startmenu.rs (replaces lines 1356-1361)
assert_eq!(c.click(ID_FLY + popup_power::ID_SETTINGS), Reply::Close);
assert_eq!(c.power_pending, Some(Choice::Settings));
c.power_pending = None;
c.click(ID_POWER);
assert_eq!(c.click(ID_FLY + popup_power::ID_LOCK), Reply::Close);
assert_eq!(c.power_pending, Some(Choice::Run(PowerAction::Lock)));
c.power_pending = None;

// src/popup_start.rs
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn dock_settings_follows_windows_settings() {
        let i = ROWS.iter().position(|r| matches!(r, Some((Act::DockSettings, _, _)))).expect("row");
        assert!(matches!(ROWS[i - 1], Some((Act::Launch("ms-settings:"), _, _))));
        let mut p = StartPopup { hits: HitMap::default(), pending: None, width: 0 };
        assert_eq!(p.click(i as u32 + 1), Reply::Close);
        assert!(matches!(p.pending, Some(Act::DockSettings)));
    }
}

// src/main.rs
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn settings_flag_parses() {
        let a = parse_args_from(["--settings".to_string()].into_iter()).unwrap();
        assert!(a.settings && !a.install);
        assert!(!parse_args_from(std::iter::empty()).unwrap().settings);
        assert!(parse_args_from(["--setings".to_string()].into_iter()).is_err());
    }
}
```

- [ ] **Step 2: Run the tests and watch them fail**

Run: `DOCK_NO_MANIFEST=1 CARGO_TARGET_DIR=target-test cargo test --locked popup_power:: popup_start:: popup_startmenu:: tests::settings_flag`
Expected: FAIL or compile errors (`Reply::Switch` returned, no `Act::DockSettings`, no `parse_args_from`).

- [ ] **Step 3: Implement the changes listed in Interfaces**

Remove `PopupKind::Settings` from these files' imports where it becomes unused; the variant itself goes in Task 10.

- [ ] **Step 4: Run all tests and render the previews**

Run: `DOCK_NO_MANIFEST=1 CARGO_TARGET_DIR=target-test cargo test --locked`
Expected: PASS.
Run: the preview build, `--preview target-preview/after9`, then the `cmp` loop with `<dir>` = `target-preview/after9`.
Expected: `DIFF` only for `popup_start.png` and `popup_start_96.png`.

Run: `cargo fmt --check`. Expected: no output. If it lists files, run `cargo fmt` and the tests again.

- [ ] **Step 5: Commit**

```bash
git add src/popup_power.rs src/popup_startmenu.rs src/popup_start.rs src/main.rs
git commit -m "settings: open the window from the power popup, Start flyout, quick links and --settings" -m "Claude-Session: https://claude.ai/code/session_01FHR79Qfsfd7dTKsSjKFXxX"
```

---

### Task 10: Remove the Settings popup

**Files:**
- Delete: `src/popup_settings.rs`
- Modify: `src/main.rs:57` (drop `mod popup_settings;`)
- Modify: `src/popup.rs:1` (module doc), `:89` (drop `Settings = 8`; leave the comment `// 8 was Settings (removed in 1.4.0)`; the other values stay), `:161-165` (drop `next_tray_chooser`), `:186-187` (drop `TRAY_CHOOSER`), `:463-464` (drop the Power/Settings special case), `:755-761` (`TrayOverflow` builds `crate::popup_tray::new()` only), `:764` (drop the arm), `:1484` (doc: "Settings window and Audio popup")
- Modify: `src/popup_tray.rs:1-11` (doc), `:55-56` (drop `direct`), `:112-127` (drop `new_chooser`), `:235` (drop the `ID_DONE if self.direct` arm), `:321-342` (`preview_new` without `direct`)
- Modify: `src/ui.rs:872` (`PopupKind::Power => Some(ItemId::Power)`)
- Modify: `src/preview.rs:14` (doc), `:29` (drop `use crate::popup_settings::Page`), `:562-603` and `:613` (drop the `popup_settings*` arms), `:818-830` (drop the nine `popup_settings*` names from the list)
- Modify: `src/msg.rs:60-63` (doc: "Settings window"), `src/start_import.rs:1,11,46,73` (doc: "Settings window")

**Interfaces:**
- Consumes: nothing new. After this task no code path builds a popup of kind 8.
- Produces: none. `popup_tray`'s customize mode stays (it is still rendered by `popup_tray_customize.png`), but it can only be reached from `preview_new`.

- [ ] **Step 1: Write the failing check**

```bash
grep -rn "popup_settings\|PopupKind::Settings\|next_tray_chooser\|new_chooser\|TRAY_CHOOSER" src
```

- [ ] **Step 2: Run it and confirm the matches**

Expected: lines in `main.rs`, `popup.rs`, `popup_tray.rs`, `ui.rs` and `preview.rs`.

- [ ] **Step 3: Delete the file and edit the references listed in Files**

- [ ] **Step 4: Run the check, the tests and the preview comparison**

Run: the grep again. Expected: no output.
Run: `DOCK_NO_MANIFEST=1 CARGO_TARGET_DIR=target-test cargo test --locked`. Expected: PASS, with no unused-code warning in the settings modules.
Run: the preview build, `--preview target-preview/after10`, then the `cmp` loop with `<dir>` = `target-preview/after10`. Expected: `DIFF` only for `popup_start*.png` and for the nine missing `popup_settings*` files (both DPIs).

Run: `cargo fmt --check`. Expected: no output. If it lists files, run `cargo fmt` and the tests again.

- [ ] **Step 5: Commit**

```bash
git add -A src
git commit -m "settings: remove the Settings popup and its chooser plumbing" -m "Claude-Session: https://claude.ai/code/session_01FHR79Qfsfd7dTKsSjKFXxX"
```

---

### Task 11: Preview PNGs, README and release notes

**Files:**
- Modify: `src/preview.rs:1-14` (doc), `:692-863` (`run`: a settings loop after the DPI loop), plus a new `fn settings_png`
- Modify: `README.md:38-44` (screenshot tables), `README.md:83` and `:125-131` (feature text)
- Add: `docs/screenshots/settings_{appearance,bar,start,modules,screen,updates,about}.png` (the 144 DPI files; the popup screenshots there stay 120 DPI). Replace `docs/screenshots/popup_start.png`. Delete `docs/screenshots/popup_settings.png`, `popup_settings_it.png`, `popup_settings_pointer.png`, `popup_settings_screen.png` and `popup_settings_startmenu.png`.
- Modify or create: `.github/release-notes/v1.4.0.md` (append a section)

**Interfaces:**
- Consumes: `settings_view::preview_pixels`, `settings_pages::{Page, Ctx, Memory}`, `popup_tray::TrayRow`, `update::{UpdateStatus, Release, UpdateState}`.
- Produces: `fn settings_png(dir: &Path, file: &str, m: Metrics, page: Page) -> bool`. It sets the mock config and ctx, calls `preview_pixels`, composites the result on `wallpaper` with a 40 logical px margin and writes it with `write_png`. It runs for `const SETTINGS_DPIS: [(u32, &str); 2] = [(144, ""), (96, "_96")];` × `Page::ALL` and writes `settings_<slug><suffix>.png`, 14 files. `SETTINGS_DPIS` is a separate constant next to `DPIS` (`preview.rs:46`), and the loop is a separate loop after the `for (dpi, suffix) in DPIS` loop (`preview.rs:707`). `DPIS` stays `[(120, ""), (96, "_96")]`, because the byte-identical baseline of every other PNG depends on it.
  - Mock config: `Config::default()` (preview is English).
  - Mock ctx: two displays (`\\.\DISPLAY1` primary, `\\.\DISPLAY2`); the tray built from the first three `TRAY_NAMES` with mock icons; chooser and picker closed; import `Idle`; update `installed` = `CARGO_PKG_VERSION`, available v1.4.1 published "2026-11-20T10:00:00Z" with a valid release URL, `last_check` "2026-11-21T09:12:00Z", state `Idle`; `last_action` "Found v1.4.1"; memory 10_276_044 / 6_396_314; data folder `C:\ProgramData\mini-oled-dock`, log `C:\ProgramData\mini-oled-dock\dock.log`.
- Produces README tables, replacing lines 38-44. The "Tray: choose visible icons" column stays: `popup_tray_customize.png` is still rendered by `--preview` and still in `docs/screenshots`.

```
| Window previews | Tray: choose visible icons | Settings: Appearance |
|---|---|---|
| ![](docs/screenshots/popup_windows.png) | ![](docs/screenshots/popup_tray_customize.png) | ![](docs/screenshots/settings_appearance.png) |

| Settings: Bar | Settings: Start | Settings: Modules |
|---|---|---|
| ![](docs/screenshots/settings_bar.png) | ![](docs/screenshots/settings_start.png) | ![](docs/screenshots/settings_modules.png) |

| Settings: Screen | Settings: Updates | Settings: About |
|---|---|---|
| ![](docs/screenshots/settings_screen.png) | ![](docs/screenshots/settings_updates.png) | ![](docs/screenshots/settings_about.png) |
```

- Produces README text.
  - Line 83 becomes: `**Settings window** (power button → **Settings**, Start quick links → **Dock settings**, or `dock.exe --settings`): seven pages (Appearance, Bar, Start, Modules, Screen, Updates, About), every option applied live and saved to the commented `config.json`.`
  - The Settings bullet at 125-131 becomes one bullet per page, with the same facts (presets, picker, opacity, sizes, sliders, Reset sizes), plus the battery group, the inline icon chooser, the Updates controls, About, and the keyboard keys `Esc`, `Tab`, `Up/Down`. The sentence at line 129 becomes: "Sliders follow a drag or a click on the track; the mouse wheel scrolls the page. Every change applies at once and is saved to `config.json` (the comments are kept); while you drag, the file is written when you release the button." The wheel-over-slider steps are gone (Global Constraints, Keyboard).
- Produces the release-notes section, appended as written here:

```markdown
### Settings window

The settings popup had become too full for its size. It is replaced by a real window: power button → **Settings**, Start quick links → **Dock settings**, or `dock.exe --settings`.

- Resizable, dark title bar in the bar's background colour, left navigation: **Appearance** (live preview of the bar, colour, opacity, sizes, language), **Bar**, **Start**, **Modules** (with the new **Battery** group and the tray icon chooser inline), **Screen**, **Updates**, **About** (memory, data folder, log, exit the dock).
- Every change still applies at once and is saved to `config.json`. The window remembers its size (`settings_window_size`).
- `Esc` closes, `Tab` moves between the navigation and the page, `Up`/`Down` change page, the wheel scrolls. The wheel no longer steps a slider under the pointer: it scrolls the page.
- Nothing of the window stays in memory once it is closed.
```

- [ ] **Step 1: Write the failing check**

```bash
DOCK_NO_MANIFEST=1 CARGO_TARGET_DIR=target-preview cargo build --locked && \
target-preview/debug/dock.exe --preview target-preview/after11 && ls target-preview/after11/settings_*.png 2>/dev/null | wc -l
```

- [ ] **Step 2: Run it and confirm it fails**

Expected: `0`.

- [ ] **Step 3: Implement `settings_png` and the loop; update the README, the screenshots and the release notes**

Copy the seven 144 DPI `settings_<slug>.png` files and the new `popup_start.png` from `target-preview/after11` into `docs/screenshots`, and delete the five `popup_settings*.png` there.

- [ ] **Step 4: Run the check, the tests and the comparison**

Run: the command from Step 1. Expected: `14`.
Run: the `cmp` loop with `<dir>` = `target-preview/after11`. Expected: `DIFF` only for `popup_start*.png` and the removed `popup_settings*` files.
Run: `DOCK_NO_MANIFEST=1 CARGO_TARGET_DIR=target-test cargo test --locked` and `cargo fmt --check`. Expected: PASS.
The owner checks the 14 PNGs visually.

- [ ] **Step 5: Commit**

```bash
git add src/preview.rs README.md docs/screenshots .github/release-notes/v1.4.0.md
git commit -m "settings: preview PNGs per page, README screenshots, release notes" -m "Claude-Session: https://claude.ai/code/session_01FHR79Qfsfd7dTKsSjKFXxX"
```

---

## Spec coverage (self-review)

| Spec requirement (Part 3, plus the Settings parts of Parts 2 and 4) | Task |
|---|---|
| Own class, style, min/default size, `settings_window_size`, clamp, centred on the bar's display, one instance | 4, 8 |
| DWM dark title bar, caption and border colour | 8 |
| `Surface` + `copy_to`, non-layered, no mouse hook, `WM_DPICHANGED` | 1, 7, 8 |
| Coexistence, `is_open()` for the auto-install rule, never topmost | 8 |
| Opened from the power popup, quick links and `--settings`; closed by X, Esc or dock exit; nothing in memory while closed | 8, 9 |
| `popup_settings.rs` and its sub-pages removed | 10 |
| Variant A layout values | 7 |
| Controls reuse the popup drawing; live apply; drag saves on release | 5, 7, 8 |
| Seven sections with their content, battery group (Part 4), Updates UI with badge (Part 2) | 5, 6, 7, 9 |
| Icons as glyphs, shared with the bar where the concept exists | 2 |
| Keyboard | 8 |
| Preview PNGs at 96/144, README, release notes | 11 |
| Preview baseline unchanged except `popup_start*` and the removed `popup_settings*` | Before Task 1, 1, 9, 10, 11 |

Note on the spec: Part 3 says "`picker.rs` (colour picker) is reused inline". `src/picker.rs` is the window-thumbnail picker. The colour picker lives in `popup_settings.rs` (`hsv`, `paint_picker`, `PART_SV`/`PART_HUE`), and that is the code Task 7 ports. `picker.rs` serves only as the reference pattern for a non-layered `Surface` window painted with `copy_to` (`picker.rs:372-435`, `:788-802`).
