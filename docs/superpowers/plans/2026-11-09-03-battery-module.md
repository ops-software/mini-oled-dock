# Battery Module Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** On a laptop the bar shows an Apple-style battery icon (fill proportional to charge, bolt while charging, percentage beside or inside the icon), with a left-click popup and options reachable from the existing Settings popup.

**Architecture:** A new data module `battery.rs` wraps `GetSystemPowerStatus` and decides state, tint and text; it has no thread and no timer. `render.rs` gets a new `ItemId::Battery` module (glyphs in `glyphs.rs`), `ui.rs` samples it on a 30 s timer armed only while the bar is visible and on `WM_POWERBROADCAST`, and a new `popup_battery.rs` (modelled on `popup_network.rs`) shows the details. Visibility is tri-state (`show_battery`: `"auto"` | `true` | `false`) and `"auto"` is resolved at runtime from the last sample, never from the config alone, so desktops and `--preview` are unchanged.

**Tech Stack:** Rust 1.87+, crate `windows` 0.62 (`Win32_System_Power` is already enabled; `powrprof.dll` is already in `DELAY_LOAD`), raw Win32, no new dependency, no new Cargo feature.

**Spec:** `docs/superpowers/specs/2026-11-09-settings-window-updater-fixes-design.md`, Part 4 only (and the three battery keys under *Defaults and config changes*). This is the third of four PRs (fixes, updater, **battery**, Settings window). The Settings window does not exist yet, so Task 7 adds the three options to the old `popup_settings.rs`; the window PR re-homes them.

## Global Constraints

- Language of code, comments, UI strings: English; Italian only through `i18n::tr(en, it)` (both strings are `&'static str`).
- New config keys, defaults that keep today's behaviour: `show_battery` = `"auto"`, `battery_percent` = `true`, `battery_percent_inside` = `false`. No existing default changes, no key removed.
- "Pay only for what you use": nothing allocated, no timer, no thread when the module is hidden; when visible `GetSystemPowerStatus` every 30 s **only while the bar is visible** plus `WM_POWERBROADCAST` / `PBT_APMPOWERSTATUSCHANGE`. No thread. "Only while visible" covers every sample, not just the timer: the hide path and a power broadcast received while the bar is hidden take no sample; a show takes one sample just before its first redraw, so the module is in the first frame (Task 5).
- The dock runs elevated: `ms-settings:batterysaver` is opened only through `ShellRequest::Launch` (`popup::launch`, `ui::launch_target_str`), which runs de-elevated.
- Message, timer and menu ids are assigned only in `src/msg.rs`.
- Tests run as: `DOCK_NO_MANIFEST=1 CARGO_TARGET_DIR=target-test cargo test --locked [filter]`. Preview renders: build with `DOCK_NO_MANIFEST=1` and run `dock.exe --preview <dir>`.
- `cargo fmt` clean (`rustfmt.toml`: `max_width = 120`, `use_small_heuristics = "Max"`); every `unsafe` block keeps a `// SAFETY:` comment (existing style).
- `--preview` baseline: every existing preview PNG stays byte-identical **except** the five renders of the old popup's Main page (`popup_settings`, `popup_settings_it`, `popup_settings_navy`, `popup_settings_screen`, `popup_settings_pointer`, each also as `_96`), which gain three rows (they are removed by the Settings window PR anyway). The sub-page renders (`popup_settings_background`, `_presets`, `_sizes`, `_startmenu`) and every `bar_*` / other `popup_*` file stay identical. The spec's own baseline exception (`popup_start*.png`, the "Dock settings" entry) belongs to the Settings window PR, not to this one. New files (`bar_battery*`, `popup_battery*`) are additions, not changes. Verified in Task 8.
- Commit messages in English, each ending with the line `Claude-Session: https://claude.ai/code/session_01FHR79Qfsfd7dTKsSjKFXxX`.

## Review Focus

Input classes the spec implies but a straight reading of the tasks would not test. Each has its pin in the named task.

1. **Desktop PC with `"auto"`** must create nothing: no battery item in the layout, no timer, default bar width unchanged. (Task 2 `polling_wanted`, Task 4 `visible_modules_hides_battery_without_one`, Task 8 byte-identical baseline.)
2. **Windows reports "unknown"**: `BatteryFlag` 255, `BatteryLifePercent` 255, `BatteryLifeTime` 0xFFFFFFFF, or flag bit 128 ("no system battery"). Expected: treated as no battery / no time estimate, never a 255 % battery or a "4294967295 s" row. (Task 2 `from_raw_*`.)
3. **Hand-edited config** with `"show_battery": "yes"`, `1`, `null` or `"AUTO"`: the file must still load (the rest of the settings kept) and fall back to `auto`. This is more lenient than `start_style`, which only survives unknown *strings* (its `String::deserialize` rejects `1` and `null`, and the whole file then fails to parse): `BatteryShow` reads any JSON value, because a boolean is a valid setting. (Task 1 `battery_show_parses_leniently`.)
4. **Plugged in but not charging** (battery at 100 %, or a charge-limit mode below 100 %): must read "Fully charged" / "Plugged in", not "On battery" and not "Charging"; no remaining-time row. (Task 2 `state_*`, Task 6 `title_by_state`, `rows_follow_what_windows_reports`.)
5. **Module switched off or the `modules_order` of an old config** (no `"battery"` in it): the old order must gain `battery` right after `volume`, drag-reorder must keep a hidden battery's slot, and an open battery popup must close when the module is switched off. (Task 4 `default_order_places_battery_after_volume` and the updated ui test `hidden_modules_keep_their_slots`, Task 5 `next_battery_follows_show_and_visibility` for the state, Task 6 `battery_popup_closes_when_the_module_leaves_the_bar` for the popup.)
6. **Bar hidden, and the first show**: a hidden bar takes no sample (hide path, power broadcast while hidden), and the first frame after a show already contains the battery module (the sample precedes the redraw). (Task 5 `next_battery_follows_show_and_visibility`, plus the manual check in Task 5 Step 4.)
7. **Digits inside the battery** must not shift sideways as the charge changes (the spec asks for tabular digits): every character is drawn in a fixed-width cell. (Task 4 `battery_digits_are_tabular`.)

Decisions the spec left open, fixed here (change the constant, not the structure, if the owner disagrees):

- Tint (`battery::Tint`): while plugged in or charging the fill is always white. On battery: `percent <= 10` is `Critical` (the dock's attention orange, `ATTENTION_RGB`), `percent < 20` is `Low` (red `(255, 69, 58)`), otherwise `Normal`. The spec's "red below 20 %, orange below 10 %" is read as: orange replaces red at 10 % and below.
- Time remaining: only `BatteryLifeTime` (valid on battery). No "time to full" (it needs `CallNtPowerInformation`, not "cheap"); while charging the popup shows no time row.
- The power row is labelled "Power plan" (`PowerGetActiveScheme`: Power saver / Balanced / High performance). Windows 11's "power mode" overlay has no function in the `windows` crate, so it is not read; an unknown plan GUID hides the row.
- Percentage inside the icon: a larger icon (`BATT_INSIDE_BOX` = 30 logical px box) with digits only (no `%`) in the `Txt::Small` font, drawn tabular: one character at a time, each centred in a cell as wide as `measure("8", Txt::Small)` (`render::tabular_digits`), so the digits do not depend on whether the loaded face has fixed-width digits; over a fill drawn at reduced alpha (`BATT_INSIDE_FILL_ALPHA` = 110) so the white digits stay readable. While charging in this mode the bolt cannot sit under the digits, so it is drawn left of the icon in an extra `BATT_BOLT_SLOT` = 7 px slot (item width grows by that slot only while charging).
- Sampling: `battery_refresh(visible)` is the only place that calls `GetSystemPowerStatus`. A hidden bar takes no sample and keeps the last stored value; `show_dock` samples once before its first redraw (the module is in the first frame), `battery_sync()` only arms or kills `TIMER_BATTERY`.
- Settings popup rows: the three battery rows sit between the last ordinary module toggle (Show desktop) and the Tray toggle, so the Tray toggle stays directly above its "Choose visible icons" link.

### Deviations from the spec (owner approves in review; Task 9 Step 3 amends the spec so the two agree after merge)

1. **No "time to full".** Spec Part 4: "estimated time remaining or to full". This plan shows only the time remaining on battery (see *Time remaining* above).
2. **"Power plan", not "power mode".** Spec Part 4: "the active power mode name". This plan shows the active power plan (see above).
3. **`--preview` mock data.** Spec Part 4: "Mock data in `--preview` shows a battery". The spec's own baseline rule (Testing) says every current PNG stays byte-identical. Both hold only if the battery appears in the NEW previews and not in the old ones, so: the default preview has no battery (all existing `bar_*` PNGs unchanged) and `bar_battery*`, `popup_battery*` show the mock battery (`preview::mock_battery`).
4. **Settings window Battery group.** Spec Part 4: the Modules page of the Settings window gets a *Battery* group with a "Shown automatically on laptops" note. That window is the fourth PR (the spec orders it last because its Modules page needs the battery options), so it is not built here. This PR provides what that group needs (`BatteryShow`, the three `Config` fields, the Task 7 label strings, `preview::mock_battery`). The old popup has no note row type, so Task 7 adds no note.

---

### Task 1: Config keys `show_battery`, `battery_percent`, `battery_percent_inside`

**Files:**
- Modify: `src/config.rs` (enum near `StartStyle` ~line 33; `Config` struct ~line 402-418; `DEFAULT_MODULES_ORDER` ~line 473; `Default` impl ~line 498-512; `render()` template ~line 785-800 and its `format!` arguments ~line 878-882; tests ~line 1310-1345)

**Interfaces:**
- Produces: `pub enum BatteryShow { Auto, On, Off }` (`Default` = `Auto`; `Clone, Copy, Debug, PartialEq, Eq`) with `pub fn shown(self, battery_present: bool) -> bool` (`Auto` -> `battery_present`, `On` -> `true`, `Off` -> `false`); `Serialize` writes `"auto"` / `true` / `false`; `Deserialize` is lenient (below).
- Produces: `Config::show_battery: BatteryShow`, `Config::battery_percent: bool` (default `true`), `Config::battery_percent_inside: bool` (default `false`).
- Produces: `DEFAULT_MODULES_ORDER: [&str; 8] = ["metrics", "notifications", "network", "volume", "battery", "clock", "show_desktop", "power"]`.
- `MODULE_NAMES` / `module_visible` / `set_module_visible` are **not** changed: the battery is tri-state and has its own field.

- [ ] **Step 0: Render the baseline previews (once, before touching code)**

Run: `DOCK_NO_MANIFEST=1 CARGO_TARGET_DIR=target-preview cargo build --locked` then `target-preview\debug\dock.exe --preview target-preview\baseline`
Expected: `target-preview\baseline` holds the current PNGs (about 100 files). Task 8 compares against them. (`target-*` is git-ignored.)

- [ ] **Step 1: Write the failing tests** in `config.rs` `mod tests`

```rust
#[test]
fn battery_defaults_and_roundtrip() {
    let d = Config::default();
    assert_eq!(d.show_battery, BatteryShow::Auto);
    assert!(d.battery_percent && !d.battery_percent_inside);
    assert_eq!(d.modules_order.len(), 8);
    assert_eq!(d.modules_order[4], "battery");
    assert!(render(&d).contains("\"show_battery\": \"auto\""));
    let c = Config { show_battery: BatteryShow::On, battery_percent: false, battery_percent_inside: true, ..Config::default() };
    let r = render(&c);
    assert!(r.contains("\"show_battery\": true") && r.contains("\"battery_percent\": false"));
    assert!(r.contains("\"battery_percent_inside\": true"));
    assert_eq!(parse(&r).unwrap(), c);
}

#[test]
fn battery_show_parses_leniently() {
    for (text, want) in [
        ("\"auto\"", BatteryShow::Auto), ("\"AUTO\"", BatteryShow::Auto), ("true", BatteryShow::On),
        ("false", BatteryShow::Off), ("\"true\"", BatteryShow::On), ("\"false\"", BatteryShow::Off),
        ("\"yes\"", BatteryShow::Auto), ("1", BatteryShow::Auto), ("null", BatteryShow::Auto),
    ] {
        let c = parse(&format!("{{ \"show_battery\": {text}, \"autohide\": false }}")).unwrap();
        assert_eq!(c.show_battery, want, "{text}");
        assert!(!c.autohide, "the rest of the file must survive: {text}");
    }
}

#[test]
fn battery_show_resolves_auto_with_presence() {
    assert!(!BatteryShow::Auto.shown(false) && BatteryShow::Auto.shown(true));
    assert!(BatteryShow::On.shown(false) && !BatteryShow::Off.shown(true));
}
```

Also change the existing `order_fields_roundtrip` assertion `modules_order.len(), 7` to `8`.

- [ ] **Step 2: Run to verify they fail**

Run: `DOCK_NO_MANIFEST=1 CARGO_TARGET_DIR=target-test cargo test --locked battery`
Expected: compile error, `BatteryShow` not found.

- [ ] **Step 3: Implement**

- `pub enum BatteryShow` and `pub fn shown(self, battery_present: bool) -> bool` in `config.rs` after `StartStyle`, with the doc line: written as `"auto"` / `true` / `false`; an unreadable value is `Auto` instead of rejecting the file.
- `impl Serialize for BatteryShow` (`serialize_str("auto")` / `serialize_bool`) and `impl<'de> Deserialize<'de> for BatteryShow` (not modelled on `StartStyle`, which starts with `String::deserialize` and therefore fails on `1` or `null`): deserialize a `serde_json::Value`, then `Bool(b)` -> `On`/`Off`, `String` equal ignoring ASCII case to `auto` / `true` / `false` -> the matching variant, anything else -> `Auto`.
- Three fields on `Config` (doc comments as the neighbours), defaults in `Default`, `DEFAULT_MODULES_ORDER` to 8 entries (type length `8`).
- `render()`: after the `show_metrics` block add the commented template block (English guide above each key, values via `js(&c.show_battery)` and `c.battery_percent`, `c.battery_percent_inside`): `show_battery` is `"auto"` (shown only on a laptop), `true` (always, icon with a slash when no battery) or `false`; `battery_percent` shows the number; `battery_percent_inside` puts it inside the battery body (Apple style) instead of beside it. Also add `"battery"` to the `modules_order` names comment and to the `module_size` comment list.

- [ ] **Step 4: Run to verify they pass**

Run: `DOCK_NO_MANIFEST=1 CARGO_TARGET_DIR=target-test cargo test --locked config`
Expected: PASS, including the unchanged `module_visibility` test.

- [ ] **Step 5: Commit**

```bash
git add src/config.rs
git commit -m "Add show_battery, battery_percent and battery_percent_inside config keys" -m "Claude-Session: https://claude.ai/code/session_01FHR79Qfsfd7dTKsSjKFXxX"
```

---

### Task 2: `battery.rs` data module (status, state, tint, time text, power plan)

**Files:**
- Create: `src/battery.rs`
- Modify: `src/main.rs` (add `mod battery;` right after `mod autostart;` and before `mod calendar;`: the list at lines 33-68 is alphabetical, `apps, audio, autostart, calendar, clock, config, ...`; the header doc lists threads, not modules, and the battery has no thread, so it is not touched)

**Interfaces:**
- Consumes: `config::BatteryShow` (Task 1).
- Produces (all in `crate::battery`):
  - `#[derive(Clone, Copy, Debug, PartialEq, Eq)] pub struct BatteryStatus { pub present: bool, pub percent: u8, pub ac: bool, pub charging: bool, pub seconds_left: Option<u32> }` with `pub const NONE: BatteryStatus` (all false/0/None) and `impl Default` returning `NONE`.
  - `#[derive(Clone, Copy, Debug, PartialEq, Eq)] pub enum ChargeState { OnBattery, Charging, Full, PluggedIn }`, `#[derive(Clone, Copy, Debug, PartialEq, Eq)] pub enum Tint { Normal, Low, Critical }` (the tests compare them with `assert_eq!`).
  - `BatteryStatus::from_raw(ac_line: u8, flag: u8, percent: u8, life_secs: u32) -> BatteryStatus`, `BatteryStatus::state(&self) -> ChargeState`, `BatteryStatus::tint(&self) -> Tint`.
  - `pub fn query() -> Option<BatteryStatus>` (`None` only if `GetSystemPowerStatus` fails).
  - `pub fn polling_wanted(show: BatteryShow, present: bool, visible: bool) -> bool`.
  - `pub fn format_remaining(secs: u32) -> String`.
  - `#[derive(Clone, Copy, Debug, PartialEq, Eq)] pub enum PowerMode { PowerSaver, Balanced, HighPerformance }`, `pub fn power_mode() -> Option<PowerMode>`.

- [ ] **Step 1: Write the failing tests** (`#[cfg(test)] mod tests` in `battery.rs`)

```rust
#[test]
fn from_raw_unknown_and_absent_values() {
    assert!(!BatteryStatus::from_raw(1, 128, 255, u32::MAX).present);          // desktop: "no system battery"
    assert!(!BatteryStatus::from_raw(0, 255, 255, u32::MAX).present);          // status unknown
    assert!(!BatteryStatus::from_raw(0, 1, 255, u32::MAX).present);            // percent unknown
    assert!(BatteryStatus::from_raw(1, 128, 255, u32::MAX).ac);                // AC is still reported
    let b = BatteryStatus::from_raw(0, 1, 64, 11520);
    assert_eq!((b.present, b.percent, b.ac, b.charging, b.seconds_left), (true, 64, false, false, Some(11520)));
    assert_eq!(BatteryStatus::from_raw(0, 1, 64, u32::MAX).seconds_left, None); // time unknown
    assert_eq!(BatteryStatus::from_raw(1, 9, 80, 5000).seconds_left, None);     // no estimate while plugged in
}

#[test]
fn state_plugged_in_cases() {
    assert_eq!(BatteryStatus::from_raw(0, 1, 64, 100).state(), ChargeState::OnBattery);
    assert_eq!(BatteryStatus::from_raw(1, 9, 80, u32::MAX).state(), ChargeState::Charging);   // flag bit 8
    assert_eq!(BatteryStatus::from_raw(1, 1, 100, u32::MAX).state(), ChargeState::Full);
    assert_eq!(BatteryStatus::from_raw(1, 1, 90, u32::MAX).state(), ChargeState::PluggedIn); // charge limit
}

#[test]
fn tint_thresholds() {
    let on = |p| BatteryStatus::from_raw(0, 1, p, u32::MAX).tint();
    assert_eq!((on(100), on(20), on(19), on(11), on(10), on(1)),
               (Tint::Normal, Tint::Normal, Tint::Low, Tint::Low, Tint::Critical, Tint::Critical));
    assert_eq!(BatteryStatus::from_raw(1, 9, 5, u32::MAX).tint(), Tint::Normal); // charging: never alarming
}

#[test]
fn polling_only_when_there_is_something_to_poll() {
    use crate::config::BatteryShow::*;
    assert!(polling_wanted(Auto, true, true));
    assert!(!polling_wanted(Auto, false, true));  // desktop: no timer
    assert!(!polling_wanted(On, false, true));    // shown without a battery: nothing to refresh
    assert!(!polling_wanted(Off, true, true) && !polling_wanted(Auto, true, false)); // off, or bar hidden
}

#[test]
fn remaining_time_text() {
    assert_eq!(format_remaining(11520), "3 h 12 min");
    assert_eq!(format_remaining(2700), "45 min");
    assert_eq!(format_remaining(3600), "1 h");
    assert_eq!(format_remaining(30), "1 min");
}

#[test]
fn power_plan_guids() {
    let g = |v: u128| windows::core::GUID::from_u128(v);
    assert_eq!(mode_from_guid(&g(0x381b4222_f694_41f0_9685_ff5bb260df2e)), Some(PowerMode::Balanced));
    assert_eq!(mode_from_guid(&g(0x8c5e7fda_e8bf_4a96_9a85_a6e23a8c635c)), Some(PowerMode::HighPerformance));
    assert_eq!(mode_from_guid(&g(0xa1841308_3541_4fab_bc81_f71556f20b4a)), Some(PowerMode::PowerSaver));
    assert_eq!(mode_from_guid(&g(1)), None);
}
```

- [ ] **Step 2: Run to verify they fail**

Run: `DOCK_NO_MANIFEST=1 CARGO_TARGET_DIR=target-test cargo test --locked battery::`
Expected: FAIL, module/functions not defined.

- [ ] **Step 3: Implement `src/battery.rs`**

Module doc in the style of `net.rs` (COST / THREADING headers): COST = one kernel32 call per sample, no allocation, no thread; `powrprof` is touched only when the popup opens. THREADING = UI thread.

- `from_raw`: `present` is false when `flag & 128 != 0`, `flag == 255` or `percent > 100` (then everything but `ac` is default); `ac = ac_line == 1`; `charging = flag & 8 != 0`; `seconds_left = Some(life_secs)` only when `present && !ac && life_secs != u32::MAX`.
- `state`: `charging` -> `Charging`; else `ac` -> `Full` if `percent >= 100` else `PluggedIn`; else `OnBattery`. `tint`: per the decisions block above.
- `query()`: `GetSystemPowerStatus(&mut SYSTEM_POWER_STATUS)`, then `from_raw(ACLineStatus, BatteryFlag, BatteryLifePercent, BatteryLifeTime)`.
- `polling_wanted`: `visible && present && show != BatteryShow::Off`.
- `format_remaining`: minutes rounded down, minimum 1; `"{h} h {m} min"`, `"{h} h"` when `m == 0`, `"{m} min"` under an hour. Same text in English and Italian, so no `tr`.
- `power_mode()`: `PowerGetActiveScheme(None, &mut p)` (returns `WIN32_ERROR`), copy the GUID, free `p` with `LocalFree`, map with the private `fn mode_from_guid(&GUID) -> Option<PowerMode>` (GUIDs in the test above).

- [ ] **Step 4: Run to verify they pass**

Run: `DOCK_NO_MANIFEST=1 CARGO_TARGET_DIR=target-test cargo test --locked battery::`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add src/battery.rs src/main.rs
git commit -m "Add battery module: power status, charge state, tint and power plan" -m "Claude-Session: https://claude.ai/code/session_01FHR79Qfsfd7dTKsSjKFXxX"
```

---

### Task 3: Battery glyphs

**Files:**
- Modify: `src/glyphs.rs` (new functions after `ethernet_shape` / `net_badge_shape`, ~line 325; test in the existing `shapes_hit_expected_points` or a new test in the same `mod tests`)

**Interfaces:**
- Produces (16x16 space, evaluated by `Canvas::glyph` / `Surface::glyph_rgb` with 4x4 supersampling; each draws ONE colour layer, the caller stacks them):
  - `pub fn battery_outline_shape(u: f32, v: f32) -> bool`: rounded body outline (centre line rect `(1.5, 4.5)-(13.0, 11.5)`, corner radius 2.0, stroke `half()`) plus the nub (filled, `u` 13.5..15.0, `v` 6.5..9.5).
  - `pub fn battery_fill_shape(u: f32, v: f32, percent: u8, bolt: bool) -> bool`: the interior rect `u` 3.0..11.5, `v` 6.0..10.0 filled from the left up to `3.0 + 8.5 * percent / 100`; with `bolt` the pixels within 0.9 of the bolt polygon are cut out (same technique as `bell_badge_shape`).
  - `pub fn bolt_shape(u: f32, v: f32) -> bool`: filled polygon `(8.6,5.2) (6.0,8.2) (7.6,8.2) (6.8,10.8) (9.4,7.6) (7.8,7.6)`, even-odd; add a private `fn in_polygon(u, v, pts: &[(f32, f32)]) -> bool`.
  - `pub fn battery_off_shape(u: f32, v: f32) -> bool`: `battery_outline_shape` cut around a diagonal `(2.0,2.0)-(14.0,14.0)` plus the diagonal bar (copy `bell_off_shape`).
- Stroke and edge coordinates go through `snap()` / `edge()` like the neighbouring glyphs.

- [ ] **Step 1: Write the failing test**

```rust
#[test]
fn battery_shapes_hit_expected_points() {
    assert!(battery_outline_shape(1.5, 8.0) && battery_outline_shape(7.0, 4.5)); // left edge, top edge
    assert!(!battery_outline_shape(7.0, 8.0));                                    // hollow body
    assert!(battery_outline_shape(14.3, 8.0) && !battery_outline_shape(14.3, 5.0)); // nub
    assert!(battery_fill_shape(5.0, 8.0, 50, false) && !battery_fill_shape(9.0, 8.0, 50, false));
    assert!(battery_fill_shape(11.0, 8.0, 100, false) && !battery_fill_shape(3.5, 8.0, 0, false));
    assert!(!battery_fill_shape(3.5, 5.2, 100, false)); // never outside the interior
    assert!(battery_fill_shape(7.7, 7.9, 100, false) && !battery_fill_shape(7.7, 7.9, 100, true)); // bolt cut-out
    assert!(bolt_shape(7.7, 7.9) && !bolt_shape(3.5, 8.0));
    assert!(battery_off_shape(8.0, 8.0) && battery_off_shape(1.5, 8.0));
}
```

- [ ] **Step 2: Run to verify it fails**

Run: `DOCK_NO_MANIFEST=1 CARGO_TARGET_DIR=target-test cargo test --locked battery_shapes`
Expected: FAIL, functions not found.

- [ ] **Step 3: Implement** the four functions and `in_polygon` with the geometry above. The numbers are the contract of the test; if a point fails because of stroke snapping, move the tested point by less than one stroke width, not the geometry.

- [ ] **Step 4: Run to verify it passes**

Run: `DOCK_NO_MANIFEST=1 CARGO_TARGET_DIR=target-test cargo test --locked glyphs`
Expected: PASS (existing glyph tests untouched).

- [ ] **Step 5: Commit**

```bash
git add src/glyphs.rs
git commit -m "Add battery, bolt and no-battery glyphs" -m "Claude-Session: https://claude.ai/code/session_01FHR79Qfsfd7dTKsSjKFXxX"
```

---

### Task 4: The battery module in the bar (render, layout, minimal UI wiring)

**Files:**
- Modify: `src/render.rs` (`ItemId` ~line 258-278; `SystemView` ~line 311-327; `MODULES` ~line 338-347; `visible_modules` ~line 375; `SysWidths` / `sys_widths` ~line 630-665; `module_width` ~line 683; `paint_inner` match, after `ItemId::Network` ~line 1204; tests ~line 1270+)
- Modify: `src/ui.rs` (`struct UiState` at line 312 gets `battery: crate::battery::BatteryStatus`; `pub fn redraw` at line 963: its `render::visible_modules(&cfg)` call is at line 985 and the `SystemView` literal at line 1028; `fn zone_of` at line 2007; `fn click_left` at line 2404; `fn on_rbutton_up` at line 1915)
- Modify: `src/preview.rs` (`fn render_bar` at line 345: its `render::visible_modules(&cfg)` call is at line 359 and the `SystemView` literal at line 389; pass the no-battery defaults so the existing PNGs do not change)

**Interfaces:**
- Consumes: `battery::BatteryStatus`, `ChargeState`, `Tint` (Task 2); glyphs (Task 3); `config::BatteryShow::shown` (Task 1).
- Produces:
  - `ItemId::Battery`; `MODULES: [(&str, ItemId); 8]` with `("battery", ItemId::Battery)` right after `("volume", ...)`.
  - `pub fn visible_modules(cfg: &Config, battery_present: bool) -> Vec<ItemId>` (was one parameter): `Battery` is kept iff `cfg.show_battery.shown(battery_present)`.
  - `#[derive(Clone, Copy, Debug, Default)] pub struct BatteryView { pub status: BatteryStatus, pub percent: bool, pub inside: bool }` and `SystemView::battery: BatteryView`.
  - `#[derive(Clone, Debug, PartialEq, Eq)] pub enum BatteryText { None, Beside(String), Inside(String) }` and `pub fn battery_text(v: &BatteryView) -> BatteryText`: `None` when `!status.present` or `!percent`; `Inside("64")` (digits only) when `inside`; otherwise `Beside("64%")`.
  - `fn tabular_digits(s: &str, cell: i32) -> Vec<(char, i32)>`: each character of `s` with the x offset of its cell, `i * cell` (the caller draws it centred in the cell).
  - Constants in `render.rs`: `BATT_INSIDE_BOX: i32 = 30`, `BATT_BOLT_SLOT: i32 = 7`, `BATT_INSIDE_FILL_ALPHA: u32 = 110`, `BATT_LOW_RGB: (u32, u32, u32) = (255, 69, 58)`.
- Interim UI behaviour (replaced by Task 6): left and right click on the battery open `ms-settings:batterysaver` with `launch_target_str`; `zone_of(Battery)` is `Zone::Modules`.

- [ ] **Step 1: Write the failing tests** in `render.rs` `mod tests` (and update the ui test, below)

First, in `render.rs` `mod tests`:

- Below `use super::*;` add `use crate::config::{BatteryShow, Config};` (`render.rs` names `Config` only by full path, so the glob does not provide it). `BatteryStatus` and `BatteryView` come through the glob, because Step 3 puts `use crate::battery::{BatteryStatus, ChargeState, Tint};` at the top of `render.rs` and defines `BatteryView` there.
- Create the helper `test_scene` by moving the `Scene { ... }` literal out of the closure in `start_button_is_the_first_item_only_when_on` (render.rs:1288-1318) into a function next to `render_init_for_test`, and add `battery: BatteryView::default()` to its `SystemView`. Its signature is `fn test_scene<'a>(m: Metrics, sizes: BarSizes, modules: &'a [ItemId], start_button: bool) -> Scene<'a>`; the existing test then reads `let scene = |start_button| test_scene(m, sizes, &modules, start_button);` and is otherwise unchanged. The literal needs the new field anyway, or the test module stops compiling.

Then the new tests:

```rust
#[test]
fn battery_text_rules() {
    let st = |p| BatteryStatus::from_raw(0, 1, p, u32::MAX);
    let v = |status, percent, inside| BatteryView { status, percent, inside };
    assert!(matches!(battery_text(&v(st(64), true, false)), BatteryText::Beside(s) if s == "64%"));
    assert!(matches!(battery_text(&v(st(100), true, true)), BatteryText::Inside(s) if s == "100"));
    assert!(matches!(battery_text(&v(st(64), false, true)), BatteryText::None));              // percent off wins
    assert!(matches!(battery_text(&v(BatteryStatus::NONE, true, false)), BatteryText::None)); // no battery
}

#[test]
fn visible_modules_hides_battery_without_one() {
    let cfg = Config::default(); // show_battery = auto
    assert!(!visible_modules(&cfg, false).contains(&ItemId::Battery));
    assert!(visible_modules(&cfg, true).contains(&ItemId::Battery));
    let on = Config { show_battery: BatteryShow::On, ..Config::default() };
    assert!(visible_modules(&on, false).contains(&ItemId::Battery));
    let off = Config { show_battery: BatteryShow::Off, ..Config::default() };
    assert!(!visible_modules(&off, true).contains(&ItemId::Battery));
}

#[test]
fn default_order_places_battery_after_volume() {
    let order = modules_from_names(&Config::default().modules_order);
    let at = |id| order.iter().position(|x| *x == id).unwrap();
    assert_eq!(at(ItemId::Battery), at(ItemId::Volume) + 1);
    // An old config without "battery": it lands right after volume.
    let old: Vec<String> = ["metrics", "notifications", "network", "volume", "clock", "show_desktop", "power"]
        .map(String::from).to_vec();
    let m = modules_from_names(&old);
    assert_eq!(m.len(), 8);
    assert_eq!(m[4], ItemId::Battery);
}

#[test]
fn battery_width_follows_the_options() {
    let m = Metrics { dpi: 96 };
    let sizes = BarSizes::default();
    render_init_for_test(m, sizes);
    let width = |status: BatteryStatus, percent, inside| {
        let modules = [ItemId::Battery, ItemId::Power];
        let mut scene = test_scene(m, sizes, &modules, false);
        scene.system.battery = BatteryView { status, percent, inside };
        let l = layout(&scene);
        let r = l.rect_of(ItemId::Battery).unwrap();
        r.right - r.left
    };
    let batt = BatteryStatus::from_raw(0, 1, 64, u32::MAX);
    let charging = BatteryStatus::from_raw(1, 9, 64, u32::MAX);
    let plain = width(batt, false, false);
    assert_eq!(plain, width(BatteryStatus::NONE, true, false));      // slash icon: same as the plain icon
    assert!(width(batt, true, false) > plain);                       // percentage beside
    assert!(width(batt, true, true) > plain);                        // larger icon with digits inside
    assert!(width(charging, true, true) > width(batt, true, true));  // bolt slot only while charging
    assert_eq!(width(charging, true, false), width(batt, true, false)); // beside mode: bolt is inside the icon
}

#[test]
fn battery_digits_are_tabular() {
    // Every character gets a cell of the same width, so "111" and "888" take the same room and no digit
    // moves sideways when the charge changes; the caller centres each glyph in its cell.
    let cell = 7;
    for s in ["1", "64", "100", "88"] {
        let v = tabular_digits(s, cell);
        assert_eq!(v.len(), s.chars().count());
        assert!(v.iter().enumerate().all(|(i, (_, x))| *x == i as i32 * cell), "{s}");
    }
    assert_eq!(tabular_digits("100", cell).iter().map(|(c, _)| *c).collect::<String>(), "100");
}
```

In `ui.rs` `hidden_modules_keep_their_slots`: `full.len()` becomes `8`; `full[0] == Metrics` and `full[2] == Network` stay; the final expectation becomes `["metrics", "power", "notifications", "network", "battery", "volume", "clock", "show_desktop"]`.

- [ ] **Step 2: Run to verify they fail**

Run: `DOCK_NO_MANIFEST=1 CARGO_TARGET_DIR=target-test cargo test --locked render::`
Expected: compile errors (`ItemId::Battery`, `BatteryView`, `visible_modules` arity).

- [ ] **Step 3: Implement**

- `render.rs`: everything in the Interfaces block. `sys_widths` gains `battery: i32` (0 when `Battery` is not in `scene.modules`, as `metrics` does): plain icon or no battery = `bell`; `Beside` = `bell + SYS_TEXT_GAP + measure(slot)` with the slot `"100%"` at 100 else `"88%"` (the volume rule); `Inside` = `2 * TRAY_PAD + BATT_INSIDE_BOX` plus `BATT_BOLT_SLOT` while `status.charging`. `module_width` maps `ItemId::Battery`.
- Paint (module metrics `mm`, `hot` / `glyph_a` as the volume arm): no battery -> `battery_off_shape` at `sys_icon`. Otherwise layers at `glyph_a`: outline; fill with `battery_fill_shape(u, v, percent, charging)` in the tint colour (`Normal` WHITE, `Low` `BATT_LOW_RGB`, `Critical` `ATTENTION_RGB`); bolt (`bolt_shape`, WHITE, alpha 255) when charging. `Beside`: glyph at `r.left + sys_pad`, text like the volume percent. `Inside`: glyph box `mm.px(BATT_INSIDE_BOX)` centred on `mcy`, fill alpha `glyph_a * BATT_INSIDE_FILL_ALPHA / 255` with `bolt = false`, digits in `Txt::Small` drawn through `tabular_digits(text, measure("8", Txt::Small, mm))`, each glyph centred in its cell and the whole block (`n * cell` wide) centred on the body (`u` = 7.25 of the box); while charging the bolt is drawn at `sys_icon` size with its ink centred in the extra slot on the left and the body shifted right by that slot.
- `ui.rs`: field `battery` (default via `BatteryStatus::default()`); `redraw` computes `let battery = UI.with(|u| u.borrow().battery);` before `visible_modules(&cfg, battery.present)` and fills `SystemView::battery` from `cfg.battery_percent` / `cfg.battery_percent_inside`; arms for `ItemId::Battery` in `zone_of`, `click_left`, `on_rbutton_up` as in the Interfaces block. The `ui.rs` module doc gets one line for the new module (`main.rs` is not touched in this task: its header lists threads, not modules).
- `preview.rs`: `visible_modules(&cfg, false)` and `BatteryView::default()` for now.

- [ ] **Step 4: Run to verify they pass**

Run: `DOCK_NO_MANIFEST=1 CARGO_TARGET_DIR=target-test cargo test --locked`
Expected: PASS, whole suite.

- [ ] **Step 5: Commit**

```bash
git add src/render.rs src/ui.rs src/preview.rs
git commit -m "Draw the battery module in the bar" -m "Claude-Session: https://claude.ai/code/session_01FHR79Qfsfd7dTKsSjKFXxX"
```

---

### Task 5: Sampling in the UI (30 s timer, power broadcast, module off)

This task must compile and pass on its own: it does not touch `popup::on_battery` or `PopupKind::Battery` (Task 6 adds both, and the two `ui.rs` lines that use them).

**Files:**
- Modify: `src/msg.rs` (new timer right after `TIMER_PICKER_LEAVE`, msg.rs:124; the popup timers start at the comment on line 126; `TIMER_BATTERY = 13` is unused: 1-12 and 30-32 are taken)
- Modify: `src/ui.rs` (`use crate::msg::{...}` at lines 84-88 gains `TIMER_BATTERY`; the `windows::Win32::UI::WindowsAndMessaging` import at lines 68-77 gains `WM_POWERBROADCAST` and `PBT_APMPOWERSTATUSCHANGE`; `pub fn handle_message` at line 464; `fn on_timer` at line 557; `pub fn on_config_changed` at line 730, its `sysmon_sync()` call at line 754; `pub fn show_dock` at line 2808, its `redraw()` call just before the `GEOM` reread and its `sysmon_sync()` call at line 2833; `fn hide` at line 2862 (called by `hide_dock`, line 2857), its `sysmon_sync()` call at line 2879; the new functions go beside `fn sysmon_sync` at line 2921)

**Interfaces:**
- Consumes: `battery::{query, polling_wanted, BatteryStatus}` (Task 2); `UiState::battery` (Task 4); `redraw_if_visible` (ui.rs:956).
- Produces:
  - `msg::TIMER_BATTERY: usize = 13` (doc: 30 s on the dock window, ONLY while the bar is visible and a battery is present and the module is not off; armed and killed by `ui::battery_sync`; handled by `ui`).
  - `const BATTERY_POLL_MS: u32 = 30_000;` in `ui.rs`.
  - `fn next_battery(show: BatteryShow, visible: bool, current: BatteryStatus, sample: impl FnOnce() -> Option<BatteryStatus>) -> BatteryStatus`: `Off` -> `BatteryStatus::NONE` (sample not called); not `visible` -> `current` (sample not called: a hidden bar takes no sample and keeps the last value); otherwise `sample().unwrap_or(NONE)`.
  - `fn battery_refresh(visible: bool)`: `st.battery = next_battery(cfg.show_battery, visible, st.battery, battery::query)`; when the stored value changed, `redraw_if_visible()` (Task 6 adds the forward to the open popup). `visible` is a parameter, not read from `VISIBLE`, because `show_dock` calls it before it sets `VISIBLE`. The only caller of `battery::query`.
  - `fn battery_sync()`: no sampling; arms `TIMER_BATTERY` (`BATTERY_POLL_MS`) iff `battery::polling_wanted(cfg.show_battery, st.battery.present, VISIBLE.load(Relaxed))`, kills it otherwise.
  - `fn battery_tick()`: `battery_refresh(VISIBLE.load(Relaxed)); battery_sync();` (a battery can appear or vanish between two samples), shared by the `on_timer` arm and the power broadcast.
  - `WM_POWERBROADCAST` in `handle_message`: for `wParam == PBT_APMPOWERSTATUSCHANGE` call `battery_tick()`; always `Some(LRESULT(1))` (TRUE grants the event). While the bar is hidden `battery_tick` takes no sample.
- Call sites (every place `sysmon_sync()` is called, plus the one ordering rule): `show_dock`: when `!was_visible`, `battery_refresh(true)` immediately BEFORE the `redraw()` call (so the first frame already has the module), and `battery_sync()` next to `sysmon_sync()` after `VISIBLE.store(true)`; `hide`: `battery_sync()` next to `sysmon_sync()` (kills the timer, no sample); `on_config_changed`: `battery_refresh(VISIBLE.load(Relaxed)); battery_sync();` right after `sysmon_sync()` (so `Off` clears the stored status at once).

- [ ] **Step 1: Write the failing test** in `ui.rs` `mod tests`

```rust
#[test]
fn next_battery_follows_show_and_visibility() {
    use crate::battery::BatteryStatus;
    use crate::config::BatteryShow::*;
    let b = BatteryStatus::from_raw(0, 1, 64, 600);
    let boom = || -> Option<BatteryStatus> { panic!("a sample was taken") };
    assert_eq!(next_battery(Off, true, b, || Some(b)), BatteryStatus::NONE); // switched off: nothing stale is drawn
    assert_eq!(next_battery(Off, false, b, boom), BatteryStatus::NONE);      // ... and nothing is sampled
    assert_eq!(next_battery(Auto, false, b, boom), b);                       // bar hidden: no sample, last value kept
    assert_eq!(next_battery(Auto, true, BatteryStatus::NONE, || Some(b)), b);
    assert_eq!(next_battery(On, true, b, || None), BatteryStatus::NONE);     // failed sample: no battery, no panic
}
```

- [ ] **Step 2: Run to verify it fails**

Run: `DOCK_NO_MANIFEST=1 CARGO_TARGET_DIR=target-test cargo test --locked next_battery`
Expected: FAIL, `next_battery` not found.

- [ ] **Step 3: Implement** `TIMER_BATTERY`, `next_battery`, `battery_refresh`, `battery_sync`, `battery_tick`, the `on_timer` arm, the `WM_POWERBROADCAST` arm (both constants exist in `windows` 0.62 under `Win32::UI::WindowsAndMessaging`), and the call sites listed above. The `WM_POWERBROADCAST` arm must not touch the UI borrow while a redraw is in flight: it calls the same functions the timer calls. The module doc of `ui.rs` gets one bullet under "System monitor". Do NOT call `popup::on_battery`, `PopupKind::Battery` or `popup::current()` here: they do not exist until Task 6, which adds the popup forward to `battery_refresh` and the close-when-off check to `on_config_changed`.

- [ ] **Step 4: Run to verify it passes**

Run: `DOCK_NO_MANIFEST=1 CARGO_TARGET_DIR=target-test cargo test --locked`
Expected: PASS (the whole suite, this commit compiles without Task 6). Manual checks on a laptop (owner): (1) unplug / plug the charger with the bar visible: the icon changes within a second without waiting 30 s; (2) with auto-hide on, let the bar hide, unplug, then show it: the icon is already right in the first frame (no one-frame-late appearance) and no 30 s wait.

- [ ] **Step 5: Commit**

```bash
git add src/msg.rs src/ui.rs
git commit -m "Poll the battery every 30 s while the bar is visible and on power broadcasts" -m "Claude-Session: https://claude.ai/code/session_01FHR79Qfsfd7dTKsSjKFXxX"
```

---

### Task 6: Battery popup and click routing

**Files:**
- Create: `src/popup_battery.rs`
- Modify: `src/main.rs` (`mod popup_battery;` after `mod popup_audio;`... alphabetical, before `popup_calendar`)
- Modify: `src/popup.rs` (`enum PopupKind` at line 81; `enum DataEvent` at line 244; `State` / `STATE` at lines 372-393 (`last_volume` at 379); new `on_battery` beside `on_network` (line 600); `fn make_content` at line 749; `fn open` starts at line 841 and its line 856, `let Some((volume, net)) = with(|st| (st.last_volume, st.last_net.clone())) else { return };`, also reads `st.last_battery`)
- Modify: `src/ui.rs` (`fn item_for_popup` at line 867; the `click_left` Battery arm (fn at line 2404); `battery_refresh` (Task 5) additionally calls `popup::on_battery(status)` when the stored status changed; `on_config_changed` (line 730) closes an open battery popup through the new `battery_popup_should_close`)

**Interfaces:**
- Consumes: `battery::{BatteryStatus, ChargeState, PowerMode, power_mode, format_remaining}` (Task 2); glyphs (Task 3).
- Produces:
  - `PopupKind::Battery = 11`.
  - `DataEvent::Battery(BatteryStatus)`; `pub fn on_battery(status: BatteryStatus)` in `popup.rs` (stores `State::last_battery`, dispatches when a battery popup is open; same shape as `on_network`); `make_content(kind, volume, net, battery)` and `open` pass `st.last_battery`.
  - `pub(crate) fn new(status: BatteryStatus, mode: Option<PowerMode>) -> Box<dyn Content>` in `popup_battery.rs` (`popup.rs` passes `battery::power_mode()` so the preview can pass a mock). Const `ID_SETTINGS: u32 = 1`.
  - Private, tested: `fn title(s: &BatteryStatus) -> (String, String)` and `fn rows(s: &BatteryStatus, mode: Option<PowerMode>) -> Vec<(&'static str, String)>`.
  - In `ui.rs`, tested: `fn battery_popup_should_close(visible: &[ItemId], current: Option<PopupKind>) -> bool`: true iff `current == Some(PopupKind::Battery)` and `ItemId::Battery` is not in `visible`.
- Behaviour: left click on the item `toggle_popup(PopupKind::Battery, id)` when `popup::is_available()`, else `launch_target_str("ms-settings:batterysaver")`; right click `launch_target_str("ms-settings:batterysaver")` (already there from Task 4); the popup's link row calls `popup::launch("ms-settings:batterysaver")` in `after_close`, like `popup_network.rs`.

i18n strings (en / it) used by the popup: "On battery" / "A batteria"; "Charging" / "In carica"; "Fully charged" / "Completamente carica"; "Plugged in" / "Collegato alla corrente"; "No battery" / "Nessuna batteria"; "Time remaining" / "Tempo rimanente"; "Power plan" / "Piano di alimentazione"; plan names "Power saver" / "Risparmio energia", "Balanced" / "Bilanciato", "High performance" / "Prestazioni elevate"; link "Power & battery settings" / "Impostazioni alimentazione e batteria".

- [ ] **Step 1: Write the failing tests** (`mod tests` in `popup_battery.rs`, and one in `ui.rs` `mod tests`)

```rust
#[test]
fn title_by_state() {
    let t = |ac, flag, p| title(&BatteryStatus::from_raw(ac, flag, p, u32::MAX));
    assert_eq!(t(0, 1, 64), ("64%".to_string(), "On battery".to_string()));
    assert_eq!(t(1, 9, 80), ("80%".to_string(), "Charging".to_string()));
    assert_eq!(t(1, 1, 100), ("100%".to_string(), "Fully charged".to_string()));
    assert_eq!(t(1, 1, 90), ("90%".to_string(), "Plugged in".to_string()));
    assert_eq!(title(&BatteryStatus::NONE), ("No battery".to_string(), String::new()));
}

#[test]
fn rows_follow_what_windows_reports() {
    let on = BatteryStatus::from_raw(0, 1, 64, 11520);
    assert_eq!(rows(&on, Some(PowerMode::Balanced)),
               [("Time remaining", "3 h 12 min".to_string()), ("Power plan", "Balanced".to_string())]);
    let charging = BatteryStatus::from_raw(1, 9, 64, 11520);
    assert_eq!(rows(&charging, Some(PowerMode::Balanced)), [("Power plan", "Balanced".to_string())]);
    assert!(rows(&on, None).iter().all(|(l, _)| *l != "Power plan"));              // unknown plan: no row
    assert!(rows(&BatteryStatus::from_raw(0, 1, 64, u32::MAX), None).is_empty());  // nothing known: no rows
}
```

In `ui.rs` `mod tests`:

```rust
#[test]
fn battery_popup_closes_when_the_module_leaves_the_bar() {
    use crate::popup::PopupKind::{Audio, Battery};
    assert!(battery_popup_should_close(&[ItemId::Volume], Some(Battery)));          // module switched off or battery gone
    assert!(!battery_popup_should_close(&[ItemId::Volume, ItemId::Battery], Some(Battery)));
    assert!(!battery_popup_should_close(&[ItemId::Volume], Some(Audio)));           // another popup is not ours to close
    assert!(!battery_popup_should_close(&[], None));
}
```

- [ ] **Step 2: Run to verify they fail**

Run: `DOCK_NO_MANIFEST=1 CARGO_TARGET_DIR=target-test cargo test --locked battery`
Expected: FAIL (`popup_battery` module and `battery_popup_should_close` missing).

- [ ] **Step 3: Implement**

`popup_battery.rs` mirrors `popup_network.rs`: width 280 logical, header (24 px battery glyph with the same layers as the bar, drawn with `Surface::glyph_rgb`, then `title()` as bold line + small line), a separator and the detail rows (label dim at the text column, value right-aligned) only when `rows()` is non-empty, a separator, then the link row `ID_SETTINGS` with `row_plate`. `Content::data` handles `DataEvent::Battery` (`Reply::Relayout` when the row count changed, else `Reply::Redraw`; `Reply::None` when equal). `kind()` returns `PopupKind::Battery`. Wire `popup.rs`, `ui.rs` and `main.rs` as listed. Module doc in the style of `popup_network.rs`. Two `ui.rs` lines that Task 5 could not contain because they use this task's types, to be added now, explicitly:

1. In `battery_refresh`, after storing a changed status: `popup::on_battery(status);`.
2. In `on_config_changed`, after the `battery_refresh(..)` / `battery_sync()` calls: `if battery_popup_should_close(&render::visible_modules(&config::get(), battery_present), popup::current()) { popup::close("battery module off"); }`, with `battery_present` read as `UI.with(|u| u.borrow().battery.present)`.

- [ ] **Step 4: Run to verify they pass**

Run: `DOCK_NO_MANIFEST=1 CARGO_TARGET_DIR=target-test cargo test --locked`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add src/popup_battery.rs src/popup.rs src/ui.rs src/main.rs
git commit -m "Add the battery popup and its click routing" -m "Claude-Session: https://claude.ai/code/session_01FHR79Qfsfd7dTKsSjKFXxX"
```

---

### Task 7: Battery options in the old Settings popup

**Files:**
- Modify: `src/popup_settings.rs` (module doc ~line 6; `Setting` enum ~line 82; `items` Main page ~line 170-178; `options` ~line 217; `selected` ~line 242; `choose` ~line 267; `is_on` ~line 285; `flip` ~line 295; tests ~line 1085 and ~line 1129)

**Interfaces:**
- Consumes: `config::BatteryShow`, the three `Config` fields (Task 1).
- Produces: `Setting::Battery` (choice: segments "Auto" / "On" / "Off", Italian "Auto" / "Sì" / "No"; maps to `BatteryShow::{Auto, On, Off}`), `Setting::BatteryPercent` (toggle `battery_percent`), `Setting::BatteryInside` (toggle `battery_percent_inside`). Rows, placed in the Modules section after the module toggles other than Tray and before the Tray toggle (`MODULE_NAMES` ends with `"tray"`: appending after the whole loop would put the rows between the Tray toggle and the `Item::TrayChooser` link that follows it): `Item::Choice(Setting::Battery, tr("Battery", "Batteria"))`, `Item::Toggle(Setting::BatteryPercent, tr("Battery percentage", "Percentuale batteria"))`, `Item::Toggle(Setting::BatteryInside, tr("Percentage inside", "Percentuale dentro"))`. These are the strings the Settings window PR reuses for its Battery group.
- The three rows are always listed (the choice, not the toggles, decides visibility). Applying goes through the existing `config::modify` + `WM_DOCK_CONFIG_APPLY` path; nothing new to wire.

- [ ] **Step 1: Write the failing tests**

Extend `choices_roundtrip`: add `Setting::Battery` to the list of settings (3 segments, `selected` returns the index chosen). Extend `toggles_flip`: add `Setting::BatteryPercent` and `Setting::BatteryInside`. New test:

```rust
#[test]
fn battery_rows_are_in_the_modules_section() {
    let c = Config { show_tray: true, ..Config::default() };
    let v = items(&c, Page::Main, false, &[]);
    let at = |f: fn(&Item) -> bool| v.iter().position(f).unwrap();
    let choice = at(|i| matches!(i, Item::Choice(Setting::Battery, _)));
    let pct = at(|i| matches!(i, Item::Toggle(Setting::BatteryPercent, _)));
    let inside = at(|i| matches!(i, Item::Toggle(Setting::BatteryInside, _)));
    let modules = at(|i| matches!(i, Item::Section(s) if *s == "Modules"));
    let tray = at(|i| matches!(i, Item::Toggle(Setting::Module("tray"), _)));
    let chooser = at(|i| matches!(i, Item::TrayChooser));
    assert!(modules < choice && choice < pct && pct < inside && inside < tray);
    assert_eq!(chooser, tray + 1); // the Tray toggle stays directly above its "Choose visible icons" link
    assert_eq!(options(Setting::Battery, &[]), ["Auto", "On", "Off"]);
    let mut d = Config::default();
    choose(&mut d, Setting::Battery, 2, &[]);
    assert_eq!(d.show_battery, BatteryShow::Off);
    assert!(is_on(&c, Setting::BatteryPercent) && !is_on(&c, Setting::BatteryInside)); // defaults
}
```

- [ ] **Step 2: Run to verify they fail**

Run: `DOCK_NO_MANIFEST=1 CARGO_TARGET_DIR=target-test cargo test --locked popup_settings`
Expected: compile error (`Setting::Battery` not found).

- [ ] **Step 3: Implement** the three `Setting` variants and extend `items`, `options`, `selected`, `choose`, `is_on`, `flip`; add `BatteryShow` to the `use crate::config::{...}` list at line 29 (the tests reach it through `use super::*`); in `items`, `v.extend` the `config::MODULE_NAMES` toggles without `"tray"`, then the three battery rows, then the `"tray"` toggle, then (unchanged) `if c.show_tray { v.push(Item::TrayChooser) }`; update the module doc line "Modules:" to mention the battery rows.

- [ ] **Step 4: Run to verify they pass**

Run: `DOCK_NO_MANIFEST=1 CARGO_TARGET_DIR=target-test cargo test --locked`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add src/popup_settings.rs
git commit -m "Offer the battery options in the Settings popup" -m "Claude-Session: https://claude.ai/code/session_01FHR79Qfsfd7dTKsSjKFXxX"
```

---

### Task 8: Preview renders and baseline check

**Files:**
- Modify: `src/preview.rs` (header doc; thread-locals at line 34; mock data ~line 340; `fn render_bar` at line 345 (`visible_modules` call at 359, `SystemView` literal at 389); `fn make_popup` at line 531; `pub fn run` at line 692, per-DPI loop from line 707)

**Interfaces:**
- Consumes: `render::BatteryView`, `battery::{BatteryStatus, PowerMode}`, `popup_battery::new` (Tasks 4 and 6).
- Produces: PNGs `bar_battery.png` / `bar_battery_96.png` (on battery, 64 %, percent beside), `bar_battery_inside.png` / `_96` (charging, 100 %, percent inside), `popup_battery.png` / `_96` (on battery, 64 %, 3 h 12 min, Balanced). Same suffix rule as every other preview (`DPIS`).
- Add `thread_local! { static BATTERY: Cell<BatteryStatus> = const { Cell::new(BatteryStatus::NONE) } }` read by `render_bar` (default = no battery, so every existing bar PNG is unchanged) and `fn mock_battery() -> BatteryStatus` (`from_raw(0, 1, 64, 11520)`).

- [ ] **Step 1: Write the failing test** in `preview.rs` (a new `#[cfg(test)] mod tests { use super::*; ... }`: `Config` and `ItemId` are already imported by `preview.rs`, and `BatteryStatus` is imported by Step 3's non-test code, so the glob provides all of them)

```rust
#[test]
fn battery_mock_shows_the_module_and_the_default_does_not() {
    let cfg = Config::default();
    assert!(!render::visible_modules(&cfg, BatteryStatus::NONE.present).contains(&ItemId::Battery));
    assert!(render::visible_modules(&cfg, mock_battery().present).contains(&ItemId::Battery));
    let b = mock_battery();
    assert_eq!((b.percent, b.ac, b.charging, b.seconds_left), (64, false, false, Some(11520)));
}
```

- [ ] **Step 2: Run to verify it fails**

Run: `DOCK_NO_MANIFEST=1 CARGO_TARGET_DIR=target-test cargo test --locked battery_mock`
Expected: FAIL (`mock_battery` not found).

- [ ] **Step 3: Implement**

`render_bar` reads `BATTERY`, passes `status.present` to `visible_modules` and fills `SystemView::battery` from the status and `cfg.battery_percent` / `cfg.battery_percent_inside` (so `STYLE` closures can set the inside flag). In `run`, inside the per-DPI loop after the network variants, for `("bar_battery", mock_battery(), no_style)` and `("bar_battery_inside", BatteryStatus::from_raw(1, 9, 100, u32::MAX), |c| c.battery_percent_inside = true)`: set `BATTERY` and `STYLE`, call `bar_png(... DisplayMode::IconText, ..., wifi)`, reset both. `make_popup("popup_battery")` returns `crate::popup_battery::new(mock_battery(), Some(PowerMode::Balanced))`; add `("popup_battery", 0)` to the popup list in `run`. Update the header doc (hooks / files).

- [ ] **Step 4: Run tests, then render and compare**

Run: `DOCK_NO_MANIFEST=1 CARGO_TARGET_DIR=target-test cargo test --locked`
Expected: PASS.

Run: `DOCK_NO_MANIFEST=1 CARGO_TARGET_DIR=target-preview cargo build --locked` then `target-preview\debug\dock.exe --preview target-preview\after`
Expected: exit code 0, "written ..." for every file.

Run (PowerShell): `Get-ChildItem target-preview\baseline -Filter *.png | ForEach-Object { $a = "target-preview\after\$($_.Name)"; if ((Get-FileHash $_.FullName).Hash -ne (Get-FileHash $a).Hash) { $_.Name } } | Where-Object { $_ -notmatch '^popup_settings(_it|_navy|_screen|_pointer)?(_96)?\.png$' }`
Expected: no output. The filter removes the ten expected differences (the five Main-page renders of the old popup, each at 120 and 96 DPI: `popup_settings`, `_it`, `_navy`, `_screen`, `_pointer`). Run it once more without the final `Where-Object` and check that those ten are what is listed. Every other name is a regression: in particular `popup_settings_background`, `_presets`, `_sizes`, `_startmenu` (sub-pages, no new rows), any `bar_*`, and every other `popup_*`. Fix before committing.

Then open `bar_battery.png`, `bar_battery_inside.png` and `popup_battery.png` (and the `_96` ones) and check by eye: outline and nub crisp, fill proportional (64 %), bolt visible in the 100 % image, digits readable inside the body, popup rows aligned like `popup_network.png`.

- [ ] **Step 5: Commit**

```bash
git add src/preview.rs
git commit -m "Render the battery bar and popup previews" -m "Claude-Session: https://claude.ai/code/session_01FHR79Qfsfd7dTKsSjKFXxX"
```

---

### Task 9: Documentation (README, release notes, screenshots)

**Files:**
- Modify: `README.md` ("Pay only for what you use" line 14; screenshots tables ~line 22-45; "Popups and modules" ~line 72-80; "Usage" ~line 116-130; config table ~line 148-165)
- Modify or create: `.github/release-notes/v1.4.0.md` (the earlier PRs of this release may already have created it: append, never overwrite; if it does not exist yet, create it with the heading `# mini-oled-dock 1.4.0` first)
- Create: `docs/screenshots/bar_battery.png`, `docs/screenshots/bar_battery_inside.png`, `docs/screenshots/popup_battery.png` (copies of the 120 DPI files from `target-preview\after`)
- Modify: `docs/superpowers/specs/2026-11-09-settings-window-updater-fixes-design.md` (Part 4, lines 132-137: amend to the approved deviations, see Step 3)

**Interfaces:** none (text only). Facts to state, all from the tasks above: `show_battery` `"auto"` (default; only on a laptop) / `true` / `false`; `battery_percent` `true`; `battery_percent_inside` `false`; left click opens the popup (charge, state, time remaining on battery, power plan, "Power & battery settings"); right click opens `ms-settings:batterysaver`; polled every 30 s only while the bar is visible plus instant updates on plug / unplug; a desktop sees no change; options live in Settings -> Modules; the module can be dragged like the others; red fill under 20 %, orange at 10 % and below.

- [ ] **Step 1: Check the docs are missing the feature**

Run: `git grep -n -i "battery" -- README.md .github/release-notes`
Expected: no output.

- [ ] **Step 2: Write the docs**

- README: one paragraph bullet under "Popups and modules" (**Battery** (laptops): ...), a "Usage" bullet (left / right click, wheel not used), the three config rows in the table (`show_battery`, `battery_percent`, `battery_percent_inside`), the word "battery" added to the polled-modules clause of line 14 and to the `module_size` list, and a screenshots row with the three images (use the same table style as the Network row).
- Release notes: a `## Battery` section (also say "power plan", not "power mode"): what it is, the three keys with defaults, the "desktop unchanged" sentence, the Settings location, the known limitation "time to full is not shown (Windows only reports the time remaining on battery)".

- [ ] **Step 3: Amend the spec** (the deviations listed under *Decisions* in this plan, once the owner has approved them; if the owner rejects one, change the code instead and skip that edit)

In Part 4 of the spec: (a) *Popup* line: "estimated time remaining or to full" becomes "estimated time remaining on battery (`BatteryLifeTime`; no time to full: Windows reports no cheap value for it)", and "the active power mode name" becomes "the active power plan name (Power saver / Balanced / High performance)"; (b) *Preview* line: add "The default preview bar has no battery, so existing PNGs stay byte-identical; only the `bar_battery*` and `popup_battery` previews show the mock battery"; (c) last line: "Mock data in `--preview` shows a battery" becomes "The Battery group lands with the Settings window PR (fourth PR); until then the options are in the old Settings popup".

- [ ] **Step 4: Copy the screenshots**

Run: `copy target-preview\after\bar_battery.png docs\screenshots\` (same for `bar_battery_inside.png`, `popup_battery.png`).
Expected: three new files; the images referenced in the README exist.

- [ ] **Step 5: Verify**

Run: `git grep -n -i "battery" -- README.md .github/release-notes` and open the README table rows.
Expected: matches in both files; every image path in the new README rows exists under `docs/screenshots`.

- [ ] **Step 6: Commit**

```bash
git add README.md .github/release-notes/v1.4.0.md docs/superpowers/specs/2026-11-09-settings-window-updater-fixes-design.md docs/screenshots/bar_battery.png docs/screenshots/bar_battery_inside.png docs/screenshots/popup_battery.png
git commit -m "Document the battery module" -m "Claude-Session: https://claude.ai/code/session_01FHR79Qfsfd7dTKsSjKFXxX"
```

---

## Self-review (spec coverage)

- Module `battery.rs`, ordering and dragging like the others: Tasks 2, 4 (`MODULES`, default order, `zone_of`; hidden slot kept by the existing `reorder_visible_modules`, test updated).
- `show_battery` tri-state with runtime `auto`: Tasks 1, 4, 5. Desktop creates nothing: Review Focus 1.
- Icon, fill, bolt, tints, `module_size`: Tasks 3, 4.
- `battery_percent` / `battery_percent_inside`: Tasks 1, 4, 7.
- Popup (charge, state, time, power plan, link) and right click: Tasks 6, 4. Deviations from the spec are listed under "Deviations from the spec" in Decisions (no time-to-full, "Power plan", preview mock data, Settings window group) and the tint reading under "Decisions"; Task 9 Step 3 amends the spec.
- Cost (30 s, visible only, `WM_POWERBROADCAST`, no thread): Task 5.
- Previews `bar_battery`, `bar_battery_inside`, `popup_battery`: Task 8. Old popup rows: Task 7. Release notes + README: Task 9.
- Not in this plan (belongs to the Settings window PR, deviation 4): the *Battery* group on the Modules page with the "Shown automatically on laptops" note. That PR reuses `config::BatteryShow`, the three `Config` fields and the label strings of Task 7, and uses `preview::mock_battery` for its `settings_modules.png` so that image shows a battery.
- Tabular digits (spec: "smaller tabular font"): Task 4 `tabular_digits` + `battery_digits_are_tabular`. Sampling only while visible and the first frame: Task 5 `next_battery_follows_show_and_visibility` and the manual checks.
