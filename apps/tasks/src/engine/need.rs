//! The set of readings a view needs.
use crate::model::view::{ColumnId, Page, ViewMode};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[repr(u8)]
pub enum Need {
    Procs,
    Windows,
    Hung,
    PerCore,
    MemLists,
    Disks,
    Nics,
    GpuTotal,
    GpuPerProc,
    Services,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct NeedSet(pub u16);

impl NeedSet {
    pub const EMPTY: NeedSet = NeedSet(0);
    pub const fn with(self, n: Need) -> NeedSet {
        NeedSet(self.0 | (1 << n as u8))
    }
    pub const fn has(self, n: Need) -> bool {
        self.0 & (1 << n as u8) != 0
    }
    pub const fn is_empty(self) -> bool {
        self.0 == 0
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Strip {
    Cpu,
    Memory,
    Disk,
    Network,
    Gpu,
}

pub struct ViewNeeds<'a> {
    pub page: Page,
    pub mode: ViewMode,
    pub columns: &'a [ColumnId],
    pub strips: &'a [Strip],
    pub expanded: &'a [Strip],
    pub windows_rows_visible: bool,
    pub verdict_on: bool,
}
// No pane_open: no sampler depends on the details pane (its facts are lazy per-row queries in P3); a later need adds the
// field and its need_set rule in one freeze-change PR.

pub fn need_set(v: &ViewNeeds) -> NeedSet {
    let processes = v.page == Page::Processes;
    let performance = v.page == Page::Performance;
    let grouped = processes && v.mode == ViewMode::Grouped;
    let has_col = |c: ColumnId| v.columns.contains(&c);
    let has_strip = |s: Strip| v.strips.contains(&s);
    let mut set = NeedSet::EMPTY;
    if v.page != Page::Startup || v.verdict_on {
        set = set.with(Need::Procs);
    }
    let windows = grouped || v.verdict_on;
    let hung = windows || has_col(ColumnId::Status);
    if hung {
        // Hung always brings Windows: the hung check walks the window list.
        set = set.with(Need::Windows).with(Need::Hung);
    }
    if windows {
        set = set.with(Need::Windows);
    }
    if grouped || v.verdict_on || (performance && has_strip(Strip::Cpu)) {
        set = set.with(Need::PerCore);
    }
    if performance && v.expanded.contains(&Strip::Memory) {
        set = set.with(Need::MemLists);
    }
    if performance && has_strip(Strip::Disk) {
        set = set.with(Need::Disks);
    }
    if performance && has_strip(Strip::Network) {
        set = set.with(Need::Nics);
    }
    if performance && has_strip(Strip::Gpu) {
        set = set.with(Need::GpuTotal);
    }
    if has_col(ColumnId::Gpu) || has_col(ColumnId::GpuEngine) || has_col(ColumnId::GpuMemory) {
        set = set.with(Need::GpuPerProc);
    }
    if v.page == Page::Services || (processes && v.windows_rows_visible) {
        set = set.with(Need::Services);
    }
    set
}

#[cfg(test)]
mod tests {
    use super::*;

    const ALL: [Need; 10] = [
        Need::Procs,
        Need::Windows,
        Need::Hung,
        Need::PerCore,
        Need::MemLists,
        Need::Disks,
        Need::Nics,
        Need::GpuTotal,
        Need::GpuPerProc,
        Need::Services,
    ];

    fn set_of(list: &[Need]) -> NeedSet {
        list.iter().fold(NeedSet::EMPTY, |s, n| s.with(*n))
    }

    fn base<'a>() -> ViewNeeds<'a> {
        ViewNeeds {
            page: Page::Processes,
            mode: ViewMode::Grouped,
            columns: &[ColumnId::Name, ColumnId::Cpu, ColumnId::Memory],
            strips: &[],
            expanded: &[],
            windows_rows_visible: true,
            verdict_on: false,
        }
    }

    #[test]
    fn need_set_table() {
        use Need::*;
        let cases: Vec<(&str, ViewNeeds, Vec<Need>)> = vec![
            ("default", base(), vec![Procs, Windows, Hung, PerCore, Services]),
            (
                "windows section collapsed",
                ViewNeeds { windows_rows_visible: false, ..base() },
                vec![Procs, Windows, Hung, PerCore],
            ),
            ("tree needs no windows", ViewNeeds { mode: ViewMode::Tree, ..base() }, vec![Procs, Services]),
            ("list needs no windows", ViewNeeds { mode: ViewMode::List, ..base() }, vec![Procs, Services]),
            (
                "tree with the verdict",
                ViewNeeds { mode: ViewMode::Tree, verdict_on: true, ..base() },
                vec![Procs, Windows, Hung, PerCore, Services],
            ),
            (
                "status column alone brings hung and windows",
                ViewNeeds { mode: ViewMode::List, columns: &[ColumnId::Status], windows_rows_visible: false, ..base() },
                vec![Procs, Windows, Hung],
            ),
            (
                "gpu column",
                ViewNeeds { mode: ViewMode::List, columns: &[ColumnId::Gpu], windows_rows_visible: false, ..base() },
                vec![Procs, GpuPerProc],
            ),
            (
                "gpu columns",
                ViewNeeds {
                    mode: ViewMode::List,
                    columns: &[ColumnId::GpuEngine],
                    windows_rows_visible: false,
                    ..base()
                },
                vec![Procs, GpuPerProc],
            ),
            (
                "gpu memory column",
                ViewNeeds {
                    mode: ViewMode::List,
                    columns: &[ColumnId::GpuMemory],
                    windows_rows_visible: false,
                    ..base()
                },
                vec![Procs, GpuPerProc],
            ),
            (
                "performance cpu strip",
                ViewNeeds { page: Page::Performance, strips: &[Strip::Cpu], ..base() },
                vec![Procs, PerCore],
            ),
            (
                "performance every strip, memory expanded",
                ViewNeeds {
                    page: Page::Performance,
                    strips: &[Strip::Cpu, Strip::Memory, Strip::Disk, Strip::Network, Strip::Gpu],
                    expanded: &[Strip::Memory],
                    ..base()
                },
                vec![Procs, PerCore, MemLists, Disks, Nics, GpuTotal],
            ),
            (
                "performance memory strip not expanded",
                ViewNeeds { page: Page::Performance, strips: &[Strip::Memory], ..base() },
                vec![Procs],
            ),
            (
                "performance with the verdict",
                ViewNeeds { page: Page::Performance, verdict_on: true, ..base() },
                vec![Procs, Windows, Hung, PerCore],
            ),
            ("services page", ViewNeeds { page: Page::Services, ..base() }, vec![Procs, Services]),
            (
                "strips outside performance are ignored",
                ViewNeeds { strips: &[Strip::Disk, Strip::Gpu], ..base() },
                vec![Procs, Windows, Hung, PerCore, Services],
            ),
        ];
        for (name, view, want) in cases {
            assert_eq!(need_set(&view), set_of(&want), "{name}");
        }
    }

    #[test]
    fn startup_page_needs_nothing_in_step_a() {
        let v = ViewNeeds { page: Page::Startup, windows_rows_visible: false, ..base() };
        let v = ViewNeeds { mode: ViewMode::List, ..v };
        assert!(need_set(&v).is_empty());
        let on = ViewNeeds { verdict_on: true, ..v };
        assert!(need_set(&on).has(Need::Procs));
    }

    #[test]
    fn set_bits_are_distinct() {
        let mut seen = NeedSet::EMPTY;
        for n in ALL {
            assert!(!seen.has(n));
            seen = seen.with(n);
        }
    }
}
