//! The view: the frames taken from the sampler, the merge into rows, the sort and the publish into the table model.

use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::path::PathBuf;

use oled_sysmon::heat::{heat, HeatStep, Machine, MetricKind};
use oled_sysmon::percore::{dpc_irq_percent, CoreTimes};
use oled_sysmon::{ProcKey, ProcRow, ProcTable};
use oled_trust::frame::tasks::SortBy;
use oled_ui::model::{RowKey, RowOut};
use oled_ui::widgets::VirtualTable;
use oled_ui::{CommandId, EventCx, UiLang};

use super::commands::{PAUSE, SAMPLE_NOW};
use super::order;
use super::rows::{proc_key, Decor, RowAct, RowEntry, RowKind, RowVals, TasksTable, COL_PID};
use super::screen::TasksScreen;
use crate::app::options::OpenSort;
use crate::app::sampling::Sampling;
use crate::app::state::StateHandle;
use crate::app::Deps;
use crate::engine::cadence::Refresh;
use crate::engine::frame::Frame;
use crate::model::classify::InfoBook;
use crate::model::group::{Interner, ProcInfo};
use crate::model::order::auto_sort;
use crate::model::rates::RateBook;
use crate::model::view::{ColumnId, Section, ViewMode};
use crate::strings::{k, t};

/// The sort of the list: a column and its direction; auto follows model::order::auto_sort.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SortState {
    pub column: ColumnId,
    pub descending: bool,
    pub auto: bool,
}

impl Default for SortState {
    fn default() -> SortState {
        SortState { column: ColumnId::Cpu, descending: true, auto: true }
    }
}

/// The machine's totals of the last good frame, as the gauge header shows them; None until measured.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Gauges {
    /// The sequence number of the good frame these totals come from; 0 before the first one.
    pub seq: u64,
    pub procs: usize,
    /// Whole-processor share, 0..=100.
    pub cpu: Option<f32>,
    /// Physical memory load, 0..=100.
    pub mem_load: Option<u8>,
    /// Bytes per second over the primed processes.
    pub io: Option<f32>,
    /// CPU, Memory, I/O.
    pub heat: [HeatStep; 3],
    /// The PID column shows (List mode only): the gauge header lays its cells out with it.
    pub show_pid: bool,
}

/// What the cost line of the More menu measured last: the process's CPU time and the wall time of that moment.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct CostMark {
    pub cpu_100ns: u64,
    pub wall_100ns: u64,
}

pub struct View {
    mode: ViewMode,
    sort: SortState,
    paused: bool,
    /// The refresh before the pause (what PAUSE returns to).
    resume: Refresh,
    /// The newest good frame (an error frame leaves its processes in place) and the table of the one before it.
    frame: Frame,
    prev: ProcTable,
    /// The host clock (µs) of the frame that `frame.procs` holds, and of the one `prev` holds: a failed snapshot moves
    /// `frame.t_us` but not the processes, so the rates divide by these, not by the error frame's own interval.
    good_t_us: u64,
    prev_t_us: u64,
    info: InfoBook,
    /// One entry per row of the newest good frame's processes (the InfoBook's, copied when the frame is merged).
    info_rows: Vec<ProcInfo>,
    rates: RateBook,
    /// The one interner of identities, lower-cased exe paths and image names; the builds intern identities through
    /// `intern`, so it sits in a RefCell.
    names: RefCell<Interner>,
    /// The window's remembered state and the folder of tasks.exe, taken from the first merge's Deps.
    state: Option<StateHandle>,
    install_dir: Option<PathBuf>,
    /// The cores of the frame before the newest good one, and the interrupt and driver share between the two.
    prev_cores: Vec<CoreTimes>,
    dpc_irq: f32,
    /// The labels of the rows the build of the mode wrote this pass (see TasksTable::set_decor).
    decor: RefCell<HashMap<RowKey, Decor>>,
    /// The notes and inline actions the actions step wrote this pass (see TasksTable::set_acts).
    acts: RefCell<HashMap<RowKey, RowAct>>,
    /// The heat step of every (row key, column 0 CPU / 1 Memory / 2 I/O) that is above S0, as last published.
    heat: HashMap<(u64, u8), HeatStep>,
    heat_next: HashMap<(u64, u8), HeatStep>,
    machine: Machine,
    gauges: Gauges,
    read_error: Option<u32>,
    /// A frame arrived that merge has not taken yet.
    fresh: bool,
    /// The automatic sort of the open has been decided.
    sort_decided: bool,
    /// The stored Always on top has been read, and still has to reach the window (the next publish does it).
    topmost_seen: bool,
    topmost_pending: bool,
    lang: UiLang,
    nf: oled_sysmon::fmt::NumFmt,
    cost: Cell<Option<CostMark>>,
    /// A scene's fixed cost line: (CPU share, private bytes), so a menu picture does not depend on the machine.
    cost_fixture: Option<(f32, u64)>,
    /// The labels of the ended processes the list keeps as ghosts (order.rs sets them; the frame no longer knows them).
    ended: Vec<(ProcKey, String)>,
}

impl Default for View {
    fn default() -> View {
        View {
            mode: ViewMode::default(),
            sort: SortState::default(),
            paused: false,
            resume: crate::app::options::DEFAULT_REFRESH,
            frame: Frame::default(),
            prev: ProcTable::new(),
            good_t_us: 0,
            prev_t_us: 0,
            info: InfoBook::new(),
            info_rows: Vec::new(),
            rates: RateBook::new(),
            names: RefCell::new(Interner::new()),
            state: None,
            install_dir: None,
            prev_cores: Vec::new(),
            dpc_irq: 0.0,
            decor: RefCell::new(HashMap::new()),
            acts: RefCell::new(HashMap::new()),
            heat: HashMap::new(),
            heat_next: HashMap::new(),
            machine: Machine { logical_cpus: 1, total_phys: 0 },
            gauges: Gauges::default(),
            read_error: None,
            fresh: false,
            sort_decided: false,
            topmost_seen: false,
            topmost_pending: false,
            lang: UiLang::EN,
            nf: oled_sysmon::fmt::NumFmt::EN,
            cost: Cell::new(None),
            cost_fixture: None,
            ended: Vec::new(),
        }
    }
}

/// The column the list opens sorted by: Show.sort first, then the option's pin, else the automatic rule.
pub fn open_sort(mem_load_pct: u8, pin: OpenSort, show: SortBy) -> ColumnId {
    match (show, pin) {
        (SortBy::Cpu, _) => ColumnId::Cpu,
        (SortBy::Memory, _) => ColumnId::Memory,
        (SortBy::Auto, OpenSort::Cpu) => ColumnId::Cpu,
        (SortBy::Auto, OpenSort::Memory) => ColumnId::Memory,
        (SortBy::Auto, OpenSort::Auto) => auto_sort(mem_load_pct, false),
    }
}

/// The step of a header total that is a share of capacity (SPEC-tasks open point 3): S1 25, S2 50, S3 75, S4 90 percent,
/// leaving a step only below 80 % of its threshold.
pub fn share_step(pct: f32, prev: HeatStep) -> HeatStep {
    const STEPS: [(HeatStep, f32); 4] =
        [(HeatStep::S1, 25.0), (HeatStep::S2, 50.0), (HeatStep::S3, 75.0), (HeatStep::S4, 90.0)];
    if !pct.is_finite() {
        return HeatStep::S0;
    }
    let mut raw = HeatStep::S0;
    for (s, th) in STEPS {
        if pct >= th {
            raw = s;
        }
    }
    if raw >= prev {
        return raw;
    }
    let mut keep = raw;
    for (s, th) in STEPS {
        if s > raw && s <= prev && pct >= 0.8 * th {
            keep = s;
        }
    }
    keep
}

impl View {
    /// prev.copy_from(frame), then take. True when a frame was taken (a failed snapshot is a frame too: its error
    /// shows over the last rows).
    pub fn pull(&mut self, s: &Sampling) -> bool {
        self.prev.copy_from(&self.frame.procs);
        self.prev_t_us = self.good_t_us;
        let got = s.take(&mut self.frame);
        if got && self.frame.error.is_none() {
            self.good_t_us = self.frame.t_us;
        }
        self.fresh |= got;
        got
    }

    /// A frame from InjectFrame (scenes, Harness tests): the same copy, then the frame replaces the View's.
    pub fn push_frame(&mut self, f: Frame) {
        self.prev.copy_from(&self.frame.procs);
        self.prev_t_us = self.good_t_us;
        if f.error.is_some() {
            // As SamplerHost::take: a failed snapshot keeps the last good processes and carries only its error.
            self.frame.error = f.error;
            self.frame.seq = f.seq;
            self.frame.t_us = f.t_us;
            self.frame.dt_ms = f.dt_ms;
        } else {
            self.frame = f;
            self.good_t_us = self.frame.t_us;
        }
        self.fresh = true;
    }

    /// The frame's processes merged with their facts and rates; only the first run after a new frame does the work.
    pub fn merge(&mut self, deps: &Deps) {
        let options = deps.options.get();
        if !std::mem::replace(&mut self.topmost_seen, true) {
            self.topmost_pending = options.always_on_top;
        }
        self.note_refresh(options.refresh);
        if self.state.is_none() {
            self.state = Some(deps.state.clone());
            self.install_dir.clone_from(&deps.install_dir);
        }
        if !std::mem::take(&mut self.fresh) {
            return;
        }
        if let Some(e) = self.frame.error {
            self.read_error = Some(e.code() as u32);
            return;
        }
        self.read_error = None;
        let procs = &self.frame.procs;
        let info = self.info.update(procs, &self.frame.paths, &deps.system_root, self.names.get_mut());
        self.info_rows.clear();
        self.info_rows.extend_from_slice(info);
        self.dpc_irq = dpc_irq_percent(&self.prev_cores, &self.frame.sys.cores);
        self.prev_cores.clear();
        self.prev_cores.extend_from_slice(&self.frame.sys.cores);
        let cpus = self.frame.sys.logical_cpus;
        // The interval between the two tables, not between the last two frames (an error may lie between them).
        let dt_ms = match self.good_t_us.checked_sub(self.prev_t_us) {
            Some(us) if self.prev_t_us > 0 && us >= 1000 => (us / 1000).min(u64::from(u32::MAX)) as u32,
            _ => self.frame.dt_ms,
        };
        self.rates.update(&self.prev, procs, dt_ms, cpus);
        let mem = self.frame.sys.mem;
        self.machine = Machine { logical_cpus: cpus.max(1), total_phys: mem.map_or(0, |m| m.total_phys) };
        let io: Option<f32> = {
            let mut sum = 0.0f32;
            let mut any = false;
            for r in &procs.rows {
                if let Some(x) = self.rates.get(r.key).filter(|x| x.primed) {
                    sum += x.io_bps;
                    any = true;
                }
            }
            any.then_some(sum)
        };
        let cpu = self.frame.sys.cpu.map(|c| c.fraction());
        let mem_load = mem.map(|m| m.load_pct);
        let prev = self.gauges.heat;
        self.gauges = Gauges {
            seq: self.frame.seq,
            procs: procs.rows.len(),
            cpu,
            mem_load,
            io,
            heat: [
                cpu.map_or(HeatStep::S0, |v| share_step(v, prev[0])),
                mem_load.map_or(HeatStep::S0, |v| share_step(f32::from(v), prev[1])),
                io.map_or(HeatStep::S0, |v| heat(MetricKind::Io, f64::from(v), prev[2], &self.machine)),
            ],
            show_pid: self.mode == ViewMode::List,
        };
        if !self.sort_decided {
            self.sort_decided = true;
            if self.sort.auto {
                self.sort.column = open_sort(mem_load.unwrap_or(0), options.open_sorted_by, deps.show.sort);
                self.sort.descending = true;
            }
        }
    }

    /// The newest frame taken (an error frame leaves its processes in place).
    pub fn frame(&self) -> &Frame {
        &self.frame
    }

    /// One entry per process of the newest good frame, in the table's order.
    pub fn info_rows(&self) -> &[ProcInfo] {
        &self.info_rows
    }

    /// The window's remembered state; None before the first merge.
    pub fn state(&self) -> Option<&StateHandle> {
        self.state.as_ref()
    }

    /// The folder of tasks.exe (the dock family lives beside it).
    pub fn install_dir(&self) -> Option<&std::path::Path> {
        self.install_dir.as_deref()
    }

    /// The interrupt and driver share of the processor over the last two good frames.
    pub fn dpc_irq_pct(&self) -> f32 {
        self.dpc_irq
    }

    /// The id of a string in the one interner (identities, lower-cased exe paths, lower-cased image names).
    pub fn intern(&self, s: &str) -> u32 {
        self.names.borrow_mut().intern(s)
    }

    /// The string behind an id of the interner.
    pub fn interned(&self, id: u32) -> String {
        self.names.borrow().get(id).to_string()
    }

    /// The label of a row that shows more than its image name (written by the build of the mode, every pass).
    pub fn set_decor(&self, key: RowKey, d: Decor) {
        self.decor.borrow_mut().insert(key, d);
    }

    /// A row's note and inline action (actions::decorate, every pass).
    pub fn set_act(&self, key: RowKey, a: RowAct) {
        self.acts.borrow_mut().insert(key, a);
    }

    /// A build starts: the labels of the last pass are forgotten.
    pub fn clear_decor(&self) {
        self.decor.borrow_mut().clear();
    }

    /// The processes the newest frame has and the one before did not.
    pub fn added(&self) -> &[ProcKey] {
        &self.frame.added
    }

    /// The processes the newest frame lost.
    pub fn gone(&self) -> &[ProcKey] {
        &self.frame.gone
    }

    /// The image name of a process of the table before the newest frame (a process that has just ended).
    pub fn prev_name(&self, key: ProcKey, out: &mut String) -> bool {
        match self.prev.find(key) {
            Some(row) => {
                out.extend(
                    char::decode_utf16(self.prev.names.get(row.image).iter().copied()).map(|r| r.unwrap_or('\u{FFFD}')),
                );
                true
            }
            None => false,
        }
    }

    /// The labels of the ghost rows, replacing the ones before.
    pub fn set_ended(&mut self, names: &[(ProcKey, String)]) {
        if self.ended.as_slice() != names {
            self.ended.clear();
            self.ended.extend(names.iter().cloned());
        }
    }

    /// The processes of the newest good frame.
    pub fn procs(&self) -> &ProcTable {
        &self.frame.procs
    }

    pub fn rates(&self) -> &RateBook {
        &self.rates
    }

    pub fn machine(&self) -> Machine {
        self.machine
    }

    pub fn gauges(&self) -> Gauges {
        self.gauges
    }

    pub fn lang(&self) -> UiLang {
        self.lang
    }

    pub fn number_format(&self) -> oled_sysmon::fmt::NumFmt {
        self.nf
    }

    /// The step a cell had at the last publish (S0 when it had none).
    pub fn heat_step(&self, key: RowKey, col: u8) -> HeatStep {
        self.heat.get(&(key.0, col)).copied().unwrap_or(HeatStep::S0)
    }

    /// The row of one process: its values, the heat its cells reach now (from the step they had) and the look of a
    /// plain row.
    pub fn proc_entry(&self, p: &ProcRow) -> RowEntry {
        let key = proc_key(p.key);
        let rate = self.rates.get(p.key).filter(|r| r.primed);
        let mut vals = RowVals {
            cpu: rate.map(|r| r.cpu),
            mem: Some(p.ws_private),
            io: rate.map(|r| r.io_bps),
            pid: Some(p.key.pid),
            heat: [HeatStep::S0; 3],
        };
        // The idle process is the unused processor, not a workload: it has no heat (SPEC-tasks 5.3.4).
        if p.key.pid != 0 {
            self.fill_heat(key, &mut vals);
        }
        RowEntry {
            key,
            kind: RowKind::Proc(p.key),
            section: None,
            parent: None,
            depth: 0,
            vals,
            out: RowOut::default(),
            motion: Cell::new((0, 255)),
            expandable: false,
            expanded: false,
        }
    }

    /// The heat steps of `vals` for the row `key`, from the steps the row had.
    pub fn fill_heat(&self, key: RowKey, vals: &mut RowVals) {
        let m = &self.machine;
        let prev = |c: u8| self.heat_step(key, c);
        if let Some(v) = vals.cpu {
            vals.heat[0] = heat(MetricKind::Cpu, f64::from(v), prev(0), m);
        }
        if let (Some(v), true) = (vals.mem, m.total_phys > 0) {
            vals.heat[1] = heat(MetricKind::Memory, v as f64, prev(1), m);
        }
        if let Some(v) = vals.io {
            vals.heat[2] = heat(MetricKind::Io, f64::from(v), prev(2), m);
        }
    }

    /// The label of a process row: its image name.
    pub fn proc_name(&self, key: ProcKey, out: &mut String) {
        if key.pid == 0 {
            out.push_str(&t(self.lang, k::TASKS_ROW_IDLE));
            return;
        }
        if let Some(row) = self.frame.procs.find(key) {
            out.extend(
                char::decode_utf16(self.frame.procs.names.get(row.image).iter().copied())
                    .map(|r| r.unwrap_or('\u{FFFD}')),
            );
        } else if let Some((_, name)) = self.ended.iter().find(|e| e.0 == key) {
            out.push_str(name);
        }
    }

    /// The label of any row.
    fn name_of(&self, e: &RowEntry, out: &mut String) {
        match e.kind {
            RowKind::Proc(k) | RowKind::Ghost(k) => self.proc_name(k, out),
            RowKind::Section(s) => {
                let key = match s {
                    Section::Apps => k::TASKS_SECTION_APPS,
                    Section::Background => k::TASKS_SECTION_BACKGROUND,
                    Section::Windows => k::TASKS_SECTION_WINDOWS,
                };
                out.push_str(&t(self.lang, key));
            }
            RowKind::Hardware => out.push_str(&t(self.lang, k::TASKS_ROW_HARDWARE)),
            RowKind::Group(_) => {}
        }
    }

    /// rows swapped into the model, model_changed(keep_anchor = true) only when keys or order changed, the changed rows
    /// damaged.
    #[allow(clippy::ptr_arg)] // the frozen signature: the rows are swapped into the model
    pub fn publish(&mut self, cx: &mut EventCx, table: &mut VirtualTable<TasksTable>, rows: &mut Vec<RowEntry>) {
        if std::mem::take(&mut self.topmost_pending) {
            cx.set_topmost(true);
        }
        // The heat steps of the rows now shown are the next rows' previous steps; a row that left forgets its own.
        self.heat_next.clear();
        for r in rows.iter() {
            for (c, step) in r.vals.heat.iter().enumerate() {
                if *step != HeatStep::S0 {
                    self.heat_next.insert((r.key.0, c as u8), *step);
                }
            }
        }
        std::mem::swap(&mut self.heat, &mut self.heat_next);
        // The window's language is the one the labels speak (a live switch reaches the rows here).
        if cx.lang() != self.lang {
            self.lang = cx.lang();
            self.nf = if self.lang.italian { oled_sysmon::fmt::NumFmt::IT } else { oled_sysmon::fmt::NumFmt::EN };
        }
        // Only the rows on screen are signed (and so formatted); the rest are looked at when they scroll in.
        // The PID column is List mode's.
        let want_pid = self.mode == ViewMode::List;
        if table.columns().iter().any(|c| c.id == COL_PID && c.visible != want_pid) {
            let mut cols = table.columns().to_vec();
            for c in cols.iter_mut().filter(|c| c.id == COL_PID) {
                c.visible = want_pid;
            }
            table.set_columns(cx, cols);
        }
        let window = table.visible_rows();
        let model = table.model_mut();
        model.configure(self.lang, self.nf);
        let mut labels = std::mem::take(self.decor.get_mut());
        model.set_decor(&mut labels);
        labels.clear();
        *self.decor.get_mut() = labels;
        let mut acts = std::mem::take(self.acts.get_mut());
        model.set_acts(&mut acts);
        acts.clear();
        *self.acts.get_mut() = acts;
        model.set_dim(self.read_error.is_some());
        let this = &*self;
        let structure = model.install(rows, window, &mut |e, out| this.name_of(e, out));
        if structure {
            table.model_changed(cx, true);
            return;
        }
        let visible = table.visible_rows();
        let vp = table.viewport();
        for &row in table.model().changed_rows() {
            if let (true, Some(r)) = (visible.contains(&row), table.row_rect(row)) {
                cx.damage(oled_ui::RectP { l: vp.l, r: vp.r, ..r });
            }
        }
    }

    pub fn mode(&self) -> ViewMode {
        self.mode
    }

    pub fn set_mode(&mut self, m: ViewMode) {
        self.mode = m;
        self.gauges.show_pid = m == ViewMode::List;
    }

    pub fn paused(&self) -> bool {
        self.paused
    }

    /// The option's refresh: it is the pause, or the speed PAUSE returns to.
    pub fn note_refresh(&mut self, r: Refresh) {
        self.paused = r == Refresh::Paused;
        if !self.paused {
            self.resume = r;
        }
    }

    /// The refresh PAUSE goes back to.
    pub fn resume_refresh(&self) -> Refresh {
        self.resume
    }

    /// The whole-processor share of the last frame, for the sampler's idle rule.
    pub fn machine_cpu_pct(&self) -> f32 {
        self.gauges.cpu.unwrap_or(0.0)
    }

    /// The Win32 code of a failed snapshot while the error band shows.
    pub fn read_error(&self) -> Option<u32> {
        self.read_error
    }

    pub fn sort(&self) -> SortState {
        self.sort
    }

    /// A header click: that column, or the other direction when it is already the sort. Numbers go descending first,
    /// the name ascending.
    pub fn set_sort(&mut self, c: ColumnId) {
        let descending = if self.sort.column == c { !self.sort.descending } else { c != ColumnId::Name };
        self.sort = SortState { column: c, descending, auto: false };
        self.sort_decided = true;
    }

    /// A sort fixed by a scene or a pinned start: it stays for the session.
    pub fn pin_sort(&mut self, column: ColumnId, descending: bool) {
        self.sort = SortState { column, descending, auto: false };
        self.sort_decided = true;
    }

    /// Scenes: the cost line shows these figures instead of the process's own.
    pub fn set_fixture_cost(&mut self, cpu_pct: f32, private_bytes: u64) {
        self.cost_fixture = Some((cpu_pct, private_bytes));
    }

    pub fn fixture_cost(&self) -> Option<(f32, u64)> {
        self.cost_fixture
    }

    /// The cost line's last measurement.
    pub fn cost_mark(&self) -> Option<CostMark> {
        self.cost.get()
    }

    pub fn set_cost_mark(&self, m: CostMark) {
        self.cost.set(Some(m));
    }
}

/// SAMPLE_NOW and PAUSE.
pub fn on_command(s: &mut TasksScreen, cx: &mut EventCx, id: CommandId) {
    match id {
        SAMPLE_NOW => {
            s.deps.sampling.sample_now();
            // Refresh now also lets go of the held order.
            order::release(&mut s.p.order);
            let now = cx.now_ms();
            s.run_pipeline(cx, now);
        }
        PAUSE => {
            let to = if s.v.paused() { s.v.resume_refresh() } else { Refresh::Paused };
            super::more_menu::set_refresh(s, cx, to);
        }
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn view() -> View {
        View { machine: Machine { logical_cpus: 16, total_phys: 32 << 30 }, ..View::default() }
    }

    #[test]
    fn heat_exit_hysteresis() {
        // S2 starts at 50 % of one core: 3.125 % of the whole processor on 16 logical CPUs.
        let mut v = view();
        let key = RowKey(7);
        let cpu = |v: &View, pct: f32| {
            let mut vals = RowVals { cpu: Some(pct), ..RowVals::default() };
            v.fill_heat(key, &mut vals);
            vals.heat[0]
        };
        assert_eq!(cpu(&v, 3.125), HeatStep::S2);
        v.heat.insert((key.0, 0), HeatStep::S2);
        assert_eq!(cpu(&v, 3.125 * 0.85), HeatStep::S2, "85 % of the threshold: still S2");
        assert_eq!(cpu(&v, 3.125 * 0.75), HeatStep::S1, "75 %: down to S1");
        assert_eq!(cpu(&view(), 3.125 * 0.85), HeatStep::S1, "a cell that was not at S2 does not get it from 85 %");
    }

    #[test]
    fn a_machine_without_memory_figures_has_no_memory_heat() {
        let v = View::default();
        let mut vals = RowVals { mem: Some(30 << 30), ..RowVals::default() };
        v.fill_heat(RowKey(1), &mut vals);
        assert_eq!(vals.heat[1], HeatStep::S0);
    }

    #[test]
    fn totals_use_the_share_of_capacity() {
        assert_eq!(share_step(24.9, HeatStep::S0), HeatStep::S0);
        assert_eq!(share_step(25.0, HeatStep::S0), HeatStep::S1);
        assert_eq!(share_step(90.0, HeatStep::S0), HeatStep::S4);
        assert_eq!(share_step(21.0, HeatStep::S1), HeatStep::S1, "above 80 % of 25");
        assert_eq!(share_step(19.0, HeatStep::S1), HeatStep::S0);
        assert_eq!(share_step(73.0, HeatStep::S4), HeatStep::S4, "above 80 % of 90");
        assert_eq!(share_step(70.0, HeatStep::S4), HeatStep::S3);
    }

    #[test]
    fn sorting_a_header_click_cycles() {
        let mut v = View::default();
        v.set_sort(ColumnId::Memory);
        assert_eq!((v.sort().column, v.sort().descending, v.sort().auto), (ColumnId::Memory, true, false));
        v.set_sort(ColumnId::Memory);
        assert!(!v.sort().descending);
        v.set_sort(ColumnId::Name);
        assert!(!v.sort().descending);
        v.set_sort(ColumnId::Io);
        assert!(v.sort().descending);
    }
}
