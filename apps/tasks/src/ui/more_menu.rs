//! The More menu: refresh speed, Paused, density, Always on top, Open sorted by, Open Windows Task Manager, Keyboard
//! shortcuts, About and the cost of Tasks itself. Filled by TA-T1a (T1.4); density by TA-T5b (T5.6).

use std::borrow::Cow;

use oled_sysmon::fmt::{self, Cell};
use oled_ui::node::Text;
use oled_ui::{Anchor, CommandId, CommandTable, EventCx, MenuItem, UiLang};

use super::commands::{
    ABOUT, ALWAYS_ON_TOP, DENSITY_COMPACT, DENSITY_STANDARD, OPEN_SORTED_AUTO, OPEN_SORTED_CPU, OPEN_SORTED_MEMORY,
    OPEN_WINDOWS_TM, REFRESH_FIVE, REFRESH_HALF, REFRESH_ONE, REFRESH_PAUSED, REFRESH_TWO,
};
use super::screen::TasksScreen;
use super::view::CostMark;
use crate::app::options::{Density, OpenSort, TasksOptions};
use crate::app::sampling::{self, Status};
use crate::engine::cadence::Refresh;
use crate::strings::{self, k, t};

/// An item that is not a plain command of the table: it carries a check mark.
fn custom(id: CommandId, label: Text, checked: Option<bool>, chord: Option<Text>, note: Option<Text>) -> MenuItem {
    MenuItem::Custom { id, label, glyph: None, chord, enabled: true, checked, note, danger: false }
}

/// The chord of a command as the menu shows it.
fn chord_of(cmds: &CommandTable, id: CommandId, lang: UiLang) -> Option<Text> {
    cmds.get(id).and_then(|c| c.chord).map(|ch| Cow::Owned(ch.label(lang)))
}

/// The menu: the items of the beta in the order of the plan, the cost line last.
pub fn items(lang: UiLang, o: &TasksOptions, cmds: &CommandTable, cost: String) -> Vec<MenuItem> {
    let refresh = |id, key, r: Refresh| custom(id, t(lang, key), Some(o.refresh == r), None, None);
    let sorted = |id, label: Text, p: OpenSort| custom(id, label, Some(o.open_sorted_by == p), None, None);
    vec![
        MenuItem::Submenu {
            label: t(lang, k::TASKS_MORE_REFRESH),
            glyph: None,
            items: vec![
                refresh(REFRESH_HALF, k::TASKS_REFRESH_HALF, Refresh::Half),
                refresh(REFRESH_ONE, k::TASKS_REFRESH_ONE, Refresh::One),
                refresh(REFRESH_TWO, k::TASKS_REFRESH_TWO, Refresh::Two),
                refresh(REFRESH_FIVE, k::TASKS_REFRESH_FIVE, Refresh::Five),
                MenuItem::Separator,
                custom(
                    REFRESH_PAUSED,
                    t(lang, k::TASKS_MORE_PAUSED),
                    Some(o.refresh == Refresh::Paused),
                    chord_of(cmds, super::commands::PAUSE, lang),
                    None,
                ),
            ],
        },
        MenuItem::Submenu {
            label: t(lang, k::TASKS_MORE_DENSITY),
            glyph: None,
            items: vec![
                custom(
                    DENSITY_STANDARD,
                    t(lang, k::TASKS_MORE_STANDARD),
                    Some(o.density == Density::Standard),
                    chord_of(cmds, DENSITY_STANDARD, lang),
                    None,
                ),
                custom(
                    DENSITY_COMPACT,
                    t(lang, k::TASKS_MORE_COMPACT),
                    Some(o.density == Density::Compact),
                    chord_of(cmds, DENSITY_COMPACT, lang),
                    None,
                ),
            ],
        },
        custom(ALWAYS_ON_TOP, t(lang, k::TASKS_MORE_ON_TOP), Some(o.always_on_top), None, None),
        MenuItem::Submenu {
            label: t(lang, k::TASKS_MORE_SORTED_BY),
            glyph: None,
            items: vec![
                sorted(OPEN_SORTED_AUTO, t(lang, k::TASKS_MORE_AUTO), OpenSort::Auto),
                sorted(OPEN_SORTED_CPU, t(lang, k::TASKS_COL_CPU), OpenSort::Cpu),
                sorted(OPEN_SORTED_MEMORY, t(lang, k::TASKS_COL_MEMORY), OpenSort::Memory),
            ],
        },
        MenuItem::Separator,
        custom(
            OPEN_WINDOWS_TM,
            t(lang, k::TASKS_MORE_WINDOWS_TM),
            None,
            None,
            Some(t(lang, k::TASKS_MORE_WINDOWS_TM_DESC)),
        ),
        MenuItem::Separator,
        MenuItem::Command(ABOUT),
        MenuItem::Header(cost.into()),
    ]
}

/// Opens the menu under the More button. The cost line is measured now (there is no timer).
pub fn open(s: &mut TasksScreen, cx: &mut EventCx) {
    let cost = cost_line(s, cx.lang());
    let options = s.deps.options.get();
    let items = items(cx.lang(), &options, &s.cmds, cost);
    cx.open_menu(items, Anchor::Below(s.chrome.more_rect()));
    super::order::note_menu_open(&mut s.p.order);
}

/// What this process has used: its CPU time, its private bytes, the wall clock and its creation time (100 ns units).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Usage {
    pub created: u64,
    pub cpu_100ns: u64,
    pub wall_100ns: u64,
    pub private_bytes: u64,
}

/// The share of the whole processor a process took: its CPU time over the wall time and the logical CPUs.
pub fn cost_pct(d_cpu_100ns: u64, d_wall_100ns: u64, cpus: u32) -> f32 {
    if d_wall_100ns == 0 {
        return 0.0;
    }
    (d_cpu_100ns as f64 / (d_wall_100ns as f64 * f64::from(cpus.max(1))) * 100.0).clamp(0.0, 100.0) as f32
}

/// GetProcessTimes and the private usage of this process; None when either call fails.
fn own_usage() -> Option<Usage> {
    use windows::Win32::Foundation::FILETIME;
    use windows::Win32::System::ProcessStatus::{K32GetProcessMemoryInfo, PROCESS_MEMORY_COUNTERS_EX};
    use windows::Win32::System::SystemInformation::GetSystemTimeAsFileTime;
    use windows::Win32::System::Threading::{GetCurrentProcess, GetProcessTimes};
    let ft = |f: FILETIME| (u64::from(f.dwHighDateTime) << 32) | u64::from(f.dwLowDateTime);
    let (mut created, mut exited, mut kernel, mut user) =
        (FILETIME::default(), FILETIME::default(), FILETIME::default(), FILETIME::default());
    // SAFETY: the pseudo handle of this process and four FILETIME locals that outlive the call.
    unsafe { GetProcessTimes(GetCurrentProcess(), &mut created, &mut exited, &mut kernel, &mut user) }.ok()?;
    let mut counters = PROCESS_MEMORY_COUNTERS_EX {
        cb: std::mem::size_of::<PROCESS_MEMORY_COUNTERS_EX>() as u32,
        ..PROCESS_MEMORY_COUNTERS_EX::default()
    };
    // SAFETY: a PROCESS_MEMORY_COUNTERS_EX local with its cb set; the API reads at most cb bytes of it (the _EX layout
    // starts with PROCESS_MEMORY_COUNTERS, which the function takes a pointer to).
    let ok = unsafe {
        K32GetProcessMemoryInfo(
            GetCurrentProcess(),
            (&mut counters as *mut PROCESS_MEMORY_COUNTERS_EX).cast(),
            counters.cb,
        )
    };
    if !ok.as_bool() {
        return None;
    }
    // SAFETY: no pointers.
    let now = unsafe { GetSystemTimeAsFileTime() };
    Some(Usage {
        created: ft(created),
        cpu_100ns: ft(kernel).saturating_add(ft(user)),
        wall_100ns: ft(now),
        private_bytes: counters.PrivateUsage as u64,
    })
}

/// "Tasks itself: 0.3 % · 38.2 MB": measured over the time since the last open (the first one: since the start).
fn cost_line(s: &TasksScreen, lang: UiLang) -> String {
    let nf = s.deps.nf;
    let measured = s.v.fixture_cost().or_else(|| {
        let u = own_usage()?;
        let mark = s.v.cost_mark().unwrap_or(CostMark { cpu_100ns: 0, wall_100ns: u.created });
        s.v.set_cost_mark(CostMark { cpu_100ns: u.cpu_100ns, wall_100ns: u.wall_100ns });
        let pct = cost_pct(
            u.cpu_100ns.saturating_sub(mark.cpu_100ns),
            u.wall_100ns.saturating_sub(mark.wall_100ns),
            s.v.machine().logical_cpus,
        );
        Some((pct, u.private_bytes))
    });
    let (cpu, mem) = (measured.map(|m| m.0), measured.map(|m| m.1));
    let mut c = Cell::EMPTY;
    let cpu_text = match cpu {
        Some(p) => {
            fmt::percent(p, nf, &mut c);
            if c.is_empty() { "0" } else { c.text() }.to_string()
        }
        None => Cell::unavailable().text().to_string(),
    };
    let mem_text = match mem {
        Some(b) => {
            let mut m = Cell::EMPTY;
            fmt::bytes(b, nf, &mut m);
            format!("{} {}", m.text(), m.unit().as_str())
        }
        None => Cell::unavailable().text().to_string(),
    };
    let name = strings::app_name(lang);
    oled_sys::i18n::subst(
        &t(lang, k::TASKS_MORE_COST),
        &[("name", name.as_ref()), ("cpu", &cpu_text), ("mem", &mem_text)],
    )
}

/// Windows' own Task Manager through the launcher; a failure shows no dialog and logs its code.
pub fn open_taskmgr(s: &TasksScreen) {
    if let Err(code) = s.deps.launcher.open_taskmgr() {
        oled_sys::log!("tasks: Windows Task Manager did not open ({code:#x})");
    }
}

/// Changes an option and writes the file; when the file refuses (a read-only folder, no file at all in a scene) the
/// choice still holds for the session.
fn set_option(s: &TasksScreen, f: impl Fn(&mut TasksOptions)) {
    if s.deps.options.modify(&f).is_err() {
        oled_sys::log!("tasks: an option was not saved");
        let mut c = (*s.deps.options.get()).clone();
        f(&mut c);
        s.deps.options.set(c);
    }
}

/// Sets the refresh option, the sampler's control and the toolbar's status together.
pub fn set_refresh(s: &mut TasksScreen, cx: &mut EventCx, r: Refresh) {
    set_option(s, |o| o.refresh = r);
    s.v.note_refresh(r);
    s.ctl.paused = s.v.paused();
    let options = s.deps.options.get();
    let c = sampling::control_of(&s.ctl, &options, cx.now_ms());
    s.deps.sampling.set_control(c);
    let status = if r == Refresh::Paused { Status::Paused } else { sampling::status(&c, s.deps.sampling.stats().slow) };
    s.chrome.set_status(cx, status);
}

/// DENSITY_*, OPEN_WINDOWS_TM, REFRESH_*, ALWAYS_ON_TOP, OPEN_SORTED_*.
pub fn on_command(s: &mut TasksScreen, cx: &mut EventCx, id: CommandId) {
    match id {
        OPEN_WINDOWS_TM => open_taskmgr(s),
        REFRESH_HALF => set_refresh(s, cx, Refresh::Half),
        REFRESH_ONE => set_refresh(s, cx, Refresh::One),
        REFRESH_TWO => set_refresh(s, cx, Refresh::Two),
        REFRESH_FIVE => set_refresh(s, cx, Refresh::Five),
        REFRESH_PAUSED => set_refresh(s, cx, Refresh::Paused),
        ALWAYS_ON_TOP => {
            let on = !s.deps.options.get().always_on_top;
            set_option(s, |o| o.always_on_top = on);
            cx.set_topmost(on);
        }
        OPEN_SORTED_AUTO | OPEN_SORTED_CPU | OPEN_SORTED_MEMORY => {
            let pin = match id {
                OPEN_SORTED_CPU => OpenSort::Cpu,
                OPEN_SORTED_MEMORY => OpenSort::Memory,
                _ => OpenSort::Auto,
            };
            set_option(s, |o| o.open_sorted_by = pin);
        }
        DENSITY_STANDARD | DENSITY_COMPACT => {
            let to = if id == DENSITY_COMPACT { Density::Compact } else { Density::Standard };
            // Every notch of Ctrl+wheel and every key repeat arrives here: write the file only for a real change.
            if s.deps.options.get().density != to {
                set_option(s, |o| o.density = to);
            }
            apply_density(s, cx);
        }
        _ => {}
    }
}

/// The logical height of a compact row (Standard is the table's own).
pub const COMPACT_ROW_H: i32 = 24;

/// The logical height of a row at a density, `token` being the table's own (Standard): what a sliding row travels by.
pub fn row_height(density: Density, token: i32) -> i32 {
    if density == Density::Compact {
        COMPACT_ROW_H
    } else {
        token
    }
}

/// Gives the table the row height of the density option when it does not have it yet. The gauge header and the
/// toolbar keep their heights. Cheap to call on every event: it does nothing once the table agrees.
pub fn apply_density(s: &mut TasksScreen, cx: &mut EventCx) {
    let want = s.deps.options.get().density;
    if s.p.keys.density == Some(want) {
        return;
    }
    s.p.keys.density = Some(want);
    s.table.set_row_height(cx, (want == Density::Compact).then_some(COMPACT_ROW_H));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_row_slides_by_the_height_it_has() {
        assert_eq!(row_height(Density::Standard, 32), 32, "the token's");
        assert_eq!(row_height(Density::Compact, 32), COMPACT_ROW_H, "the compact row is shorter");
    }

    #[test]
    fn cost_is_a_share_of_the_whole_processor() {
        // One second of CPU time over ten seconds on four logical CPUs: 2.5 %.
        assert_eq!(cost_pct(10_000_000, 100_000_000, 4), 2.5);
        assert_eq!(cost_pct(5, 0, 4), 0.0, "no time has passed");
        assert_eq!(cost_pct(u64::MAX, 1, 1), 100.0, "never above the whole processor");
    }

    #[test]
    fn the_cost_line_is_measured_for_this_process() {
        let u = own_usage().expect("this process can be measured");
        assert!(u.private_bytes > 0 && u.cpu_100ns > 0);
        assert!(u.wall_100ns > u.created, "the clock is past the creation time");
    }
}
