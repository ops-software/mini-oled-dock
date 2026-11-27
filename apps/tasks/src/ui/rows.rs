//! The rows of the table: one RowEntry per visible line (section, group, process, the hardware row, a ghost), their
//! stable keys and the TableModel the kit draws them through (rows are never widgets). The types and the keys are
//! TF0's; the model's cells are filled by TA-T1a (T1.2).
use std::borrow::Cow;
use std::cell::Cell;
use std::collections::HashMap;
use std::ops::Range;

use oled_sysmon::fmt::{self, NumFmt, Tone, Unit};
use oled_sysmon::heat::HeatStep;
use oled_sysmon::ProcKey;
use oled_ui::model::{
    Align, CellKind, CellOut, ColId, Column, FgRole, HeatStep as KitHeat, ListModel, RowKey, RowOut, SortDir,
    TableModel, TreeInfo,
};
use oled_ui::node::Text;
use oled_ui::UiLang;

use crate::model::group::AppKey;
use crate::model::view::{ColumnId, Section};
use crate::strings::{k, t};

/// What a row is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RowKind {
    Section(Section),
    Group(AppKey),
    Proc(ProcKey),
    Hardware,
    /// An ended process kept in place for a while; its key is its process's.
    Ghost(ProcKey),
}

/// A row's values; None until measured ("—").
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct RowVals {
    pub cpu: Option<f32>,
    pub mem: Option<u64>,
    pub io: Option<f32>,
    pub pid: Option<u32>,
    /// CPU, Memory, I/O.
    pub heat: [HeatStep; 3],
}

#[derive(Clone, Debug, PartialEq)]
pub struct RowEntry {
    pub key: RowKey,
    pub kind: RowKind,
    pub section: Option<Section>,
    /// The row it sorts under: a section row for groups, a group row for members, the parent process in Tree; None at
    /// top level and in List.
    pub parent: Option<RowKey>,
    pub depth: u16,
    pub vals: RowVals,
    pub out: RowOut,
    /// (dy, opacity), (0, 255) at rest; written in paint by TA-T2M.
    pub motion: Cell<(i16, u8)>,
    pub expandable: bool,
    pub expanded: bool,
}

impl RowEntry {
    /// Section and Hardware rows are anchors: order::apply never moves them.
    pub fn is_anchor(&self) -> bool {
        matches!(self.kind, RowKind::Section(_) | RowKind::Hardware)
    }
}

/// FNV-1a 64 of a tag byte and the fields' little-endian bytes.
fn fnv(tag: u8, fields: &[u64]) -> RowKey {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    let mut eat = |b: u8| {
        h ^= u64::from(b);
        h = h.wrapping_mul(0x0000_0100_0000_01b3);
    };
    eat(tag);
    for f in fields {
        for b in f.to_le_bytes() {
            eat(b);
        }
    }
    RowKey(h)
}

/// 1 + the section.
pub fn section_key(s: Section) -> RowKey {
    let n = match s {
        Section::Apps => 0,
        Section::Background => 1,
        Section::Windows => 2,
    };
    fnv(1, &[n])
}

/// 2 + the AppKey's discriminant and id.
pub fn group_key(a: AppKey) -> RowKey {
    match a {
        AppKey::App(id) => fnv(2, &[0, u64::from(id.0)]),
        AppKey::Exe(id) => fnv(2, &[1, u64::from(id.0)]),
        AppKey::Image(id) => fnv(2, &[2, u64::from(id.0)]),
        AppKey::Single(k) => fnv(2, &[3, u64::from(k.pid), k.created]),
    }
}

/// 3 + pid and create time; a ghost keeps its process's key.
pub fn proc_key(k: ProcKey) -> RowKey {
    fnv(3, &[u64::from(k.pid), k.created])
}

/// 4.
pub fn hardware_key() -> RowKey {
    fnv(4, &[])
}

/// The column ids of the table: the beta's five columns.
pub const COL_NAME: ColId = ColId(0);
pub const COL_CPU: ColId = ColId(1);
pub const COL_MEMORY: ColId = ColId(2);
pub const COL_IO: ColId = ColId(3);
pub const COL_PID: ColId = ColId(4);

/// The table column of a model column; None for a column the beta does not draw.
pub fn col_id(c: ColumnId) -> Option<ColId> {
    match c {
        ColumnId::Name => Some(COL_NAME),
        ColumnId::Cpu => Some(COL_CPU),
        ColumnId::Memory => Some(COL_MEMORY),
        ColumnId::Io => Some(COL_IO),
        ColumnId::Pid => Some(COL_PID),
        _ => None,
    }
}

/// The model column of a table column; None for the spacer.
pub fn column_of(id: ColId) -> Option<ColumnId> {
    match id {
        COL_NAME => Some(ColumnId::Name),
        COL_CPU => Some(ColumnId::Cpu),
        COL_MEMORY => Some(ColumnId::Memory),
        COL_IO => Some(ColumnId::Io),
        COL_PID => Some(ColumnId::Pid),
        _ => None,
    }
}

/// The table's columns of the beta (Name, CPU, Memory, I/O, PID) with the titles and descriptions in `lang`. PID is not in the default set: the column chooser is Phase 4's.
pub fn columns(lang: UiLang) -> Vec<Column> {
    let col = |id, title: Text, width, min, flex, align, desc: Option<Text>| Column {
        id,
        title,
        width,
        min,
        flex,
        align,
        sort: None,
        first_sort: if flex { SortDir::Asc } else { SortDir::Desc },
        visible: true,
        hideable: false,
        tip: desc,
    };
    let mut pid = col(COL_PID, t(lang, k::TASKS_COL_PID), 64, 48, false, Align::End, None);
    pid.visible = false;
    vec![
        col(COL_NAME, t(lang, k::TASKS_COL_NAME), 200, 200, true, Align::Start, None),
        col(COL_CPU, t(lang, k::TASKS_COL_CPU), 64, 48, false, Align::Decimal, Some(t(lang, k::TASKS_COL_CPU_DESC))),
        col(
            COL_MEMORY,
            t(lang, k::TASKS_COL_MEMORY),
            80,
            48,
            false,
            Align::Decimal,
            Some(t(lang, k::TASKS_COL_MEMORY_DESC)),
        ),
        col(COL_IO, t(lang, k::TASKS_COL_IO), 88, 48, false, Align::Decimal, Some(t(lang, k::TASKS_COL_IO_DESC))),
        pid,
    ]
}

/// The step of the kit's heat plate: S1..S4 are 1..=4, S0 has none.
fn kit_heat(h: HeatStep) -> Option<KitHeat> {
    match h {
        HeatStep::S0 => None,
        HeatStep::S1 => Some(KitHeat(1)),
        HeatStep::S2 => Some(KitHeat(2)),
        HeatStep::S3 => Some(KitHeat(3)),
        HeatStep::S4 => Some(KitHeat(4)),
    }
}

/// The text colour of a figure at a heat step (SPEC-tasks 5.3.4).
fn heat_fg(h: HeatStep) -> FgRole {
    match h {
        HeatStep::S0 => FgRole::Tertiary,
        HeatStep::S1 => FgRole::Secondary,
        HeatStep::S2 | HeatStep::S3 => FgRole::Primary,
        HeatStep::S4 => FgRole::Hot,
    }
}

/// CPU: "—" before the first rate, else one decimal.
pub fn cpu_cell(v: Option<f32>, nf: NumFmt) -> fmt::Cell {
    match v {
        None => fmt::Cell::unavailable(),
        Some(x) => {
            let mut c = fmt::Cell::EMPTY;
            fmt::percent(x, nf, &mut c);
            c
        }
    }
}

/// Memory: three significant digits and a unit.
pub fn mem_cell(v: Option<u64>, nf: NumFmt) -> fmt::Cell {
    match v {
        None => fmt::Cell::unavailable(),
        Some(x) => {
            let mut c = fmt::Cell::EMPTY;
            fmt::bytes(x, nf, &mut c);
            c
        }
    }
}

/// I/O: "—" before the first rate, zero draws nothing.
pub fn io_cell(v: Option<f32>, nf: NumFmt) -> fmt::Cell {
    match v {
        None => fmt::Cell::unavailable(),
        Some(x) => {
            let mut c = fmt::Cell::EMPTY;
            fmt::rate(if x.is_finite() && x > 0.0 { x as u64 } else { 0 }, nf, &mut c);
            c
        }
    }
}

/// One row's three metric cells, in column order.
pub fn metric_cells(v: &RowVals, nf: NumFmt) -> [fmt::Cell; 3] {
    [cpu_cell(v.cpu, nf), mem_cell(v.mem, nf), io_cell(v.io, nf)]
}

/// The row's signature: what a repaint of the row would change (the cells, the heat, the row look, the dim).
pub fn row_signature(e: &RowEntry, nf: NumFmt, dim: bool) -> u64 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    let mut mix = |x: u64| {
        for b in x.to_le_bytes() {
            h ^= u64::from(b);
            h = h.wrapping_mul(0x0000_0100_0000_01b3);
        }
    };
    for (i, c) in metric_cells(&e.vals, nf).iter().enumerate() {
        mix(c.signature());
        mix(e.vals.heat[i] as u64);
    }
    mix(e.vals.pid.map_or(u64::MAX, u64::from));
    let o = e.out;
    let flags = [o.section, o.ghost, o.busy, o.error, o.fresh, o.disabled, dim, e.expandable, e.expanded];
    mix(flags.iter().fold(0u64, |a, f| (a << 1) | u64::from(*f)));
    mix(u64::from(e.depth));
    h
}

/// The signature of a row nobody asked for: a row that comes into the window compares as changed once.
const SIG_OUTSIDE: u64 = u64::MAX;

/// The value a column sorts by; None sorts last. The System Idle Process (pid 0) is the unused processor, not a
/// workload: it never sorts among the processes by a measure.
pub fn sort_value(r: &RowEntry, c: ColumnId) -> Option<f64> {
    let idle = matches!(r.kind, RowKind::Proc(k) | RowKind::Ghost(k) if k.pid == 0);
    match c {
        ColumnId::Cpu if !idle => r.vals.cpu.map(f64::from),
        ColumnId::Memory if !idle => r.vals.mem.map(|m| m as f64),
        ColumnId::Io if !idle => r.vals.io.map(f64::from),
        ColumnId::Pid => r.vals.pid.map(f64::from),
        _ => None,
    }
}

/// The table model: the published rows, their labels and an index by key.
pub struct TasksTable {
    pub rows: Vec<RowEntry>,
    index: HashMap<RowKey, usize>,
    generation: u64,
    names: Vec<String>,
    sigs: Vec<u64>,
    sigs_next: Vec<u64>,
    changed: Vec<usize>,
    nf: NumFmt,
    lang: UiLang,
    dim: bool,
    /// The labels were written in another language than `lang`: the next install writes them again.
    names_stale: bool,
    /// How many cells were asked for (the tests read it: only the visible rows are formatted).
    cell_calls: Cell<u64>,
    /// How many row signatures were computed (each formats the row's three metric cells).
    sig_calls: u64,
}

impl Default for TasksTable {
    fn default() -> TasksTable {
        TasksTable {
            rows: Vec::new(),
            index: HashMap::new(),
            generation: 0,
            names: Vec::new(),
            sigs: Vec::new(),
            sigs_next: Vec::new(),
            changed: Vec::new(),
            nf: NumFmt::EN,
            lang: UiLang::EN,
            dim: false,
            names_stale: false,
            cell_calls: Cell::new(0),
            sig_calls: 0,
        }
    }
}

impl TasksTable {
    /// The language and number format the cells are written in.
    pub fn configure(&mut self, lang: UiLang, nf: NumFmt) {
        self.names_stale |= self.lang != lang;
        self.lang = lang;
        self.nf = nf;
    }

    /// The text of every value cell dims to FG_SECONDARY while a snapshot fails; true when this changed it.
    pub fn set_dim(&mut self, on: bool) -> bool {
        std::mem::replace(&mut self.dim, on) != on
    }

    pub fn number_format(&self) -> NumFmt {
        self.nf
    }

    /// The cells asked for since the model was made.
    pub fn cell_calls(&self) -> u64 {
        self.cell_calls.get()
    }

    /// The row signatures computed since the model was made (three formatted cells each).
    pub fn signature_calls(&self) -> u64 {
        self.sig_calls
    }

    /// The rows that changed in the last install (empty after a structure change: every row is new).
    pub fn changed_rows(&self) -> &[usize] {
        &self.changed
    }

    /// Takes `rows` (the caller keeps the old rows' storage), writes each row's label with `name_of` when the keys or
    /// their order changed (or the language did), and compares the signature of each row of `window` (the rows on
    /// screen) with the one it had; a row outside the window keeps a sentinel, so it counts as changed once, when it
    /// scrolls in. Returns true when the keys or their order changed, or the labels were written again (the language
    /// changed): the table lays out anew. The generation moves whenever anything visible
    /// changed.
    pub fn install(
        &mut self,
        rows: &mut Vec<RowEntry>,
        window: Range<usize>,
        name_of: &mut dyn FnMut(&RowEntry, &mut String),
    ) -> bool {
        let same = self.rows.len() == rows.len() && self.rows.iter().zip(rows.iter()).all(|(a, b)| a.key == b.key);
        let (nf, dim) = (self.nf, self.dim);
        self.sigs_next.clear();
        for (i, e) in rows.iter().enumerate() {
            if window.contains(&i) {
                self.sig_calls += 1;
                self.sigs_next.push(row_signature(e, nf, dim));
            } else {
                self.sigs_next.push(SIG_OUTSIDE);
            }
        }
        self.changed.clear();
        if same && self.sigs.len() == self.sigs_next.len() {
            for (i, (a, b)) in self.sigs.iter().zip(&self.sigs_next).enumerate() {
                if a != b {
                    self.changed.push(i);
                }
            }
        }
        std::mem::swap(&mut self.rows, rows);
        std::mem::swap(&mut self.sigs, &mut self.sigs_next);
        let rename = !same || std::mem::take(&mut self.names_stale);
        if !same {
            self.index.clear();
            self.index.extend(self.rows.iter().enumerate().map(|(i, r)| (r.key, i)));
        }
        if rename {
            self.names.resize_with(self.rows.len(), String::new);
            for (e, s) in self.rows.iter().zip(self.names.iter_mut()) {
                s.clear();
                name_of(e, s);
            }
        }
        if rename || !self.changed.is_empty() {
            self.generation += 1;
        }
        rename
    }

    /// Writes the three metric cells of `e` for column `col`.
    fn metric(&self, e: &RowEntry, which: usize, out: &mut CellOut) {
        let cell = metric_cells(&e.vals, self.nf)[which];
        let kind = match which {
            0 => CellKind::Percent,
            1 => CellKind::Bytes,
            _ => CellKind::Num,
        };
        out.align = Align::Decimal;
        out.kind = kind;
        if cell.text() == "\u{2014}" {
            out.set_text(kind, cell.text());
            out.fg = FgRole::Tertiary;
            out.tip = Some(t(self.lang, k::TASKS_UNAVAILABLE_WAIT));
            return;
        }
        let unit = cell.unit();
        out.set_text(kind, cell.text());
        if unit != Unit::None {
            out.text.push(' ');
            let start = out.text.len() as u32;
            out.text.push_str(unit.as_str());
            let end = out.text.len() as u32;
            // The unit sits in its own cell in FG_TERTIARY (the digit atlas draws the tail in the unit ink as well).
            out.push_span(start, end, FgRole::Tertiary);
        }
        let step = e.vals.heat[which];
        out.heat = kit_heat(step);
        let ink = if cell.tone() == Tone::Tertiary { FgRole::Tertiary } else { heat_fg(step) };
        // Dimmed text is capped, never lifted: tertiary stays tertiary, every brighter ink becomes secondary.
        out.fg = if self.dim && ink != FgRole::Tertiary { FgRole::Secondary } else { ink };
    }
}

impl ListModel for TasksTable {
    fn len(&self) -> usize {
        self.rows.len()
    }

    fn key(&self, row: usize) -> RowKey {
        self.rows.get(row).map_or(RowKey(0), |r| r.key)
    }

    fn row_of(&self, key: RowKey) -> Option<usize> {
        self.index.get(&key).copied()
    }

    fn label(&self, row: usize) -> Cow<'_, str> {
        Cow::Borrowed(self.names.get(row).map_or("", String::as_str))
    }

    fn generation(&self) -> u64 {
        self.generation
    }
}

impl TableModel for TasksTable {
    fn cell(&self, row: usize, col: ColId, out: &mut CellOut) {
        out.clear();
        self.cell_calls.set(self.cell_calls.get() + 1);
        let Some(e) = self.rows.get(row) else { return };
        match col {
            COL_NAME => {
                out.set_text(CellKind::Text, self.names.get(row).map_or("", String::as_str));
                out.role = oled_ui::theme::TypeRole::Body;
                out.fg =
                    if matches!(e.kind, RowKind::Ghost(_)) || self.dim { FgRole::Secondary } else { FgRole::Primary };
            }
            COL_CPU => self.metric(e, 0, out),
            COL_MEMORY => self.metric(e, 1, out),
            COL_IO => self.metric(e, 2, out),
            COL_PID => {
                if let Some(pid) = e.vals.pid {
                    out.set_text(CellKind::Num, &pid.to_string());
                    out.align = Align::End;
                    out.fg = FgRole::Secondary;
                }
            }
            _ => {}
        }
    }

    fn row(&self, row: usize, out: &mut RowOut) {
        if let Some(e) = self.rows.get(row) {
            *out = e.out;
        }
    }

    fn tree(&self, row: usize) -> Option<TreeInfo> {
        let e = self.rows.get(row)?;
        (e.depth > 0 || e.expandable).then_some(TreeInfo {
            depth: e.depth,
            expandable: e.expandable,
            expanded: e.expanded,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::group::{ExeId, IdentityId, ImageId};

    fn proc(pid: u32, cpu: Option<f32>, mem: u64, io: Option<f32>) -> RowEntry {
        let key = ProcKey { pid, created: 1 };
        RowEntry {
            key: proc_key(key),
            kind: RowKind::Proc(key),
            section: None,
            parent: None,
            depth: 0,
            vals: RowVals { cpu, mem: Some(mem), io, pid: Some(pid), heat: [HeatStep::S0; 3] },
            out: RowOut::default(),
            motion: Cell::new((0, 255)),
            expandable: false,
            expanded: false,
        }
    }

    fn install(t: &mut TasksTable, mut rows: Vec<RowEntry>) -> bool {
        let all = 0..rows.len();
        t.install(&mut rows, all, &mut |e, out| out.push_str(&format!("p{}", e.key.0 % 1000)))
    }

    fn text(t: &TasksTable, row: usize, col: ColId) -> (String, FgRole) {
        let mut out = CellOut::default();
        t.cell(row, col, &mut out);
        (out.text.clone(), out.fg)
    }

    #[test]
    fn cell_formats() {
        let mut t = TasksTable::default();
        let rows = vec![
            proc(1, Some(12.04), 3_328_180_224, Some(0.0)),
            proc(2, Some(0.0), 612 << 20, Some(1.5 * 1_048_576.0)),
            proc(3, Some(0.03), 1 << 20, None),
            proc(4, Some(100.0), 5 << 10, Some(64.0 * 1024.0)),
        ];
        install(&mut t, rows);
        assert_eq!(text(&t, 0, COL_CPU).0, "12.0");
        assert_eq!(text(&t, 1, COL_CPU).0, "", "an exact zero draws nothing");
        assert_eq!(
            text(&t, 2, COL_CPU),
            ("0.0".to_string(), FgRole::Tertiary),
            "a trace of CPU is \"0.0\" in tertiary"
        );
        assert_eq!(text(&t, 3, COL_CPU).0, "100");
        let mut out = CellOut::default();
        t.cell(0, COL_MEMORY, &mut out);
        assert_eq!(out.text, "3.10 GB");
        assert_eq!((out.spans[0].start, out.spans[0].end, out.spans[0].fg), (5, 7, FgRole::Tertiary));
        assert_eq!(out.n_spans, 1, "the unit is the one span");
        assert_eq!(text(&t, 0, COL_IO).0, "", "zero I/O draws nothing");
        assert_eq!(text(&t, 1, COL_IO).0, "1.50 MB/s");
        assert_eq!(text(&t, 3, COL_IO).0, "64.0 KB/s");
        assert_eq!(text(&t, 2, COL_IO).0, "\u{2014}", "no rate yet: the dash");
        // Italian: the decimal comma.
        t.configure(UiLang::IT, NumFmt::IT);
        assert_eq!(text(&t, 0, COL_MEMORY).0, "3,10 GB");
        assert_eq!(text(&t, 0, COL_CPU).0, "12,0");
    }

    #[test]
    fn dim_caps_the_ink_and_never_lifts_it() {
        let mut t = TasksTable::default();
        let mut hot = proc(1, Some(30.0), 1 << 30, Some(1.0));
        hot.vals.heat = [HeatStep::S4, HeatStep::S2, HeatStep::S0];
        let calm = proc(2, Some(1.0), 1 << 30, Some(1.0));
        install(&mut t, vec![hot, calm]);
        assert_eq!(text(&t, 1, COL_CPU).1, FgRole::Tertiary, "S0 figure at rest");
        assert_eq!(text(&t, 0, COL_CPU).1, FgRole::Hot);
        assert_eq!(text(&t, 0, COL_MEMORY).1, FgRole::Primary);
        t.set_dim(true);
        assert_eq!(text(&t, 1, COL_CPU).1, FgRole::Tertiary, "a tertiary figure stays tertiary while dimmed");
        assert_eq!(text(&t, 1, COL_MEMORY).1, FgRole::Tertiary);
        assert_eq!(text(&t, 0, COL_MEMORY).1, FgRole::Secondary, "a primary figure becomes secondary");
        assert_eq!(text(&t, 0, COL_CPU).1, FgRole::Secondary, "and a hot one");
    }

    #[test]
    fn only_the_window_is_signed() {
        let mut t = TasksTable::default();
        let rows = || (0..300).map(|i| proc(10 + i, Some(1.0 + i as f32), 50 << 20, Some(0.0))).collect::<Vec<_>>();
        let go = |t: &mut TasksTable, w: Range<usize>, mut r: Vec<RowEntry>| {
            t.install(&mut r, w, &mut |_, out| out.push('x'))
        };
        go(&mut t, 0..12, rows());
        assert_eq!(t.signature_calls(), 12, "twelve rows on screen, twelve signatures");
        let mut r = rows();
        r[3].vals.cpu = Some(99.0);
        r[200].vals.cpu = Some(99.0);
        go(&mut t, 0..12, r);
        assert_eq!(t.signature_calls(), 24);
        assert_eq!(t.changed_rows(), [3], "a change below the window is not looked at");
        // Scrolled to row 200: it comes in as changed once, then it is quiet.
        go(&mut t, 195..207, rows());
        assert!(t.changed_rows().contains(&200));
        go(&mut t, 195..207, rows());
        assert!(t.changed_rows().is_empty());
    }

    #[test]
    fn a_new_language_writes_the_labels_again() {
        let mut t = TasksTable::default();
        let words = |lang: UiLang, t: &mut TasksTable| {
            let mut rows = vec![proc(1, Some(1.0), 1 << 20, None)];
            let n = rows.len();
            t.install(&mut rows, 0..n, &mut |_, out| out.push_str(if lang.italian { "ciao" } else { "hello" }))
        };
        assert!(words(UiLang::EN, &mut t));
        assert_eq!(t.label(0), "hello");
        assert!(!words(UiLang::EN, &mut t), "nothing changed");
        t.configure(UiLang::IT, NumFmt::IT);
        assert!(words(UiLang::IT, &mut t), "a language switch lays the table out again");
        assert_eq!(t.label(0), "ciao");
        assert!(!words(UiLang::IT, &mut t));
    }

    #[test]
    fn the_idle_process_sorts_last_by_every_measure() {
        let idle = ProcKey { pid: 0, created: 0 };
        let mut e = proc(0, Some(94.0), 0, Some(5.0));
        e.kind = RowKind::Proc(idle);
        let p = proc(7, Some(0.5), 1 << 20, Some(1.0));
        for c in [ColumnId::Cpu, ColumnId::Memory, ColumnId::Io] {
            assert_eq!(sort_value(&e, c), None);
            assert!(sort_value(&p, c).is_some());
        }
        assert_eq!(sort_value(&e, ColumnId::Pid), Some(0.0));
    }

    #[test]
    fn heat_sets_the_plate_and_the_ink() {
        let mut t = TasksTable::default();
        let mut hot = proc(1, Some(30.0), 1 << 30, Some(1.0));
        hot.vals.heat = [HeatStep::S4, HeatStep::S2, HeatStep::S0];
        install(&mut t, vec![hot]);
        let mut out = CellOut::default();
        t.cell(0, COL_CPU, &mut out);
        assert_eq!((out.heat, out.fg), (Some(KitHeat(4)), FgRole::Hot));
        t.cell(0, COL_MEMORY, &mut out);
        assert_eq!((out.heat, out.fg), (Some(KitHeat(2)), FgRole::Primary));
        t.cell(0, COL_IO, &mut out);
        assert_eq!((out.heat, out.fg), (None, FgRole::Tertiary));
    }

    #[test]
    fn unchanged_tick_damages_nothing() {
        let mut t = TasksTable::default();
        let rows = || (0..5).map(|i| proc(10 + i, Some(1.0 + i as f32), 50 << 20, Some(0.0))).collect::<Vec<_>>();
        assert!(install(&mut t, rows()), "the first install is a structure change");
        let generation = t.generation();
        assert!(!install(&mut t, rows()), "the same rows: no structure change");
        assert!(t.changed_rows().is_empty(), "no row repaints");
        assert_eq!(t.generation(), generation, "and the generation stays");
    }

    #[test]
    fn one_changed_value_damages_one_row() {
        let mut t = TasksTable::default();
        let mut rows: Vec<RowEntry> = (0..5).map(|i| proc(10 + i, Some(1.0), 50 << 20, Some(0.0))).collect();
        install(&mut t, rows.clone());
        let generation = t.generation();
        rows[3].vals.cpu = Some(2.5);
        assert!(!install(&mut t, rows.clone()));
        assert_eq!(t.changed_rows(), [3]);
        assert_eq!(t.generation(), generation + 1);
        // A heat step alone repaints the row too.
        rows[1].vals.heat[0] = HeatStep::S1;
        install(&mut t, rows.clone());
        assert_eq!(t.changed_rows(), [1]);
        // A reorder is a structure change.
        rows.swap(0, 4);
        assert!(install(&mut t, rows));
        assert_eq!(t.row_of(proc_key(ProcKey { pid: 14, created: 1 })), Some(0));
    }

    #[test]
    fn keys_differ_by_kind_and_fields() {
        let p = ProcKey { pid: 7, created: 9 };
        let keys = [
            section_key(Section::Apps),
            section_key(Section::Background),
            section_key(Section::Windows),
            group_key(AppKey::App(IdentityId(1))),
            group_key(AppKey::Exe(ExeId(1))),
            group_key(AppKey::Image(ImageId(1))),
            group_key(AppKey::Single(p)),
            proc_key(p),
            proc_key(ProcKey { pid: 7, created: 10 }),
            hardware_key(),
        ];
        let set: std::collections::HashSet<_> = keys.iter().collect();
        assert_eq!(set.len(), keys.len());
        assert_eq!(proc_key(p), proc_key(ProcKey { pid: 7, created: 9 }), "stable");
        assert_eq!(hardware_key(), fnv(4, &[]));
    }
}
