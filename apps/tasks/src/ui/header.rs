//! The gauge header: the CPU, Memory and I/O totals with their traces, the column titles and the sort, the held look.
//! Filled by TA-T1a (T1.3), TA-HDR (TH1, the traces) and TA-T2M (TH2, set_held's look).

use oled_sysmon::fmt::{self, NumFmt};
use oled_sysmon::heat::HeatStep;
use oled_ui::model::{Align, ColId};
use oled_ui::node::Text;
use oled_ui::text::{TextEngine, Trim};
use oled_ui::{
    Actions, CommandId, Event, EventCx, Focusable, Glyph, Ink, Key, LayoutCx, MouseButton, Node, NodeId, PaintCx,
    PointP, RectP, Role, Scale, TypeRole, UiLang, Widget,
};

use super::commands::{SORT_COL_1, SORT_COL_2, SORT_COL_3, SORT_COL_4};
use super::history::{self, History};
use super::rows::{self, column_of};
use super::screen::{TasksScreen, HEADER};
use super::view::{Gauges, SortState};
use crate::model::view::ColumnId;
use crate::strings::{k, t};

/// What the header tells the screen.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HeaderOut {
    /// A column title was clicked.
    Sort(ColumnId),
    /// A right click: the view-mode menu.
    Menu(PointP),
}

/// One column of the header: its cell, its words.
struct Slot {
    col: ColumnId,
    rect: RectP,
    label: Text,
    desc: Option<Text>,
}

pub struct GaugeHeader {
    rect: RectP,
    held: bool,
    history: History,
    /// The sequence number of the last frame pushed into the traces.
    last_seq: u64,
    slots: Vec<Slot>,
    gauges: Gauges,
    sort: SortState,
    nf: NumFmt,
    /// The totals as shown: the process count, then CPU, Memory and I/O.
    count: String,
    totals: [String; 3],
    /// Widths (physical px) of the totals and of the labels, measured when they are set or the layout runs.
    total_w: [i32; 3],
    count_w: i32,
    label_w: Vec<i32>,
    /// The cell that is the Tab stop.
    cur: usize,
}

impl Default for GaugeHeader {
    fn default() -> GaugeHeader {
        GaugeHeader {
            rect: RectP::EMPTY,
            held: false,
            history: History::default(),
            last_seq: 0,
            slots: Vec::new(),
            gauges: Gauges::default(),
            sort: SortState::default(),
            nf: NumFmt::EN,
            count: String::new(),
            totals: Default::default(),
            total_w: [0; 3],
            count_w: 0,
            label_w: Vec::new(),
            cur: 0,
        }
    }
}

/// The text colour of a total at a heat step (the figure rules of a cell).
fn step_fg(h: HeatStep, c: &oled_ui::ColorTokens) -> Ink {
    match h {
        HeatStep::S0 => c.fg_tertiary,
        HeatStep::S1 => c.fg_secondary,
        HeatStep::S2 | HeatStep::S3 => c.fg_primary,
        HeatStep::S4 => c.fg_hot,
    }
}

fn step_index(h: HeatStep) -> Option<usize> {
    match h {
        HeatStep::S0 => None,
        HeatStep::S1 => Some(0),
        HeatStep::S2 => Some(1),
        HeatStep::S3 => Some(2),
        HeatStep::S4 => Some(3),
    }
}

/// An ink at a fraction of its alpha.
fn faded(i: Ink, t: f32) -> Ink {
    Ink { a: (i.a as f32 * t.clamp(0.0, 1.0)).round() as u32, ..i }
}

impl GaugeHeader {
    /// The order is held: the sorted column draws its chevron outlined with a pause glyph beside it (TH2). The pause
    /// glyph is a node of its own (the tip), so the layout runs again when the value changes.
    pub fn set_held(&mut self, cx: &mut EventCx, held: bool) {
        if self.held == held {
            return;
        }
        self.held = held;
        let scale = cx.scale();
        if let Some(s) = self.slots.iter().find(|s| s.col == self.sort.column) {
            cx.damage(RectP { t: self.rect.t + scale.px(6), b: self.rect.t + scale.px(22), ..s.rect });
        }
        cx.relayout();
    }

    /// The sort chevron's rect and the pause glyph's rect beside it, in the cell of slot `i`.
    fn sort_marks(&self, i: usize, px: &dyn Fn(i32) -> i32) -> (RectP, RectP) {
        let (s, lw) = (&self.slots[i], self.label_w.get(i).copied().unwrap_or(0));
        let (chev_y, pause_y) = (self.rect.t + px(10), self.rect.t + px(8));
        if s.col == ColumnId::Name {
            let chev = RectP::wh(s.rect.l + px(16) + lw + px(4), chev_y, px(8), px(8));
            (chev, RectP::wh(chev.r + px(4), pause_y, px(12), px(12)))
        } else {
            let right = s.rect.r - px(12);
            (
                RectP::wh(s.rect.r - px(10), chev_y, px(8), px(8)),
                RectP::wh(right - lw - px(4) - px(12), pause_y, px(12), px(12)),
            )
        }
    }

    pub fn held(&self) -> bool {
        self.held
    }

    /// The totals and the sort of the view, after every pipeline run. Repaints the header only when a shown word changed.
    pub fn set_gauges(&mut self, cx: &mut EventCx, g: Gauges, sort: SortState, nf: NumFmt) {
        let lang = cx.lang();
        let mut count = fmt::Cell::EMPTY;
        fmt::count(g.procs as u64, nf, &mut count);
        let count = oled_sys::i18n::subst(&t(lang, k::TASKS_HEADER_PROCESSES), &[("n", count.text())]);
        let dash = fmt::Cell::unavailable();
        let cpu = match g.cpu {
            None => dash.text().to_string(),
            Some(v) => {
                let mut c = fmt::Cell::EMPTY;
                fmt::percent(v, nf, &mut c);
                format!("{} %", if c.is_empty() { "0" } else { c.text() })
            }
        };
        let mem = g.mem_load.map_or_else(|| dash.text().to_string(), |v| format!("{v} %"));
        let io = match g.io {
            None => dash.text().to_string(),
            Some(v) => {
                let mut c = fmt::Cell::EMPTY;
                fmt::rate(if v.is_finite() && v > 0.0 { v as u64 } else { 0 }, nf, &mut c);
                if c.is_empty() {
                    "0 B/s".to_string()
                } else {
                    format!("{} {}", c.text(), c.unit().as_str())
                }
            }
        };
        let totals = [cpu, mem, io];
        // One sample per good frame; a read error or a stopped sampler brings no frame and so no sample.
        let fresh = g.seq != 0 && g.seq != self.last_seq;
        if fresh {
            self.last_seq = g.seq;
            self.history.push(cx.now_ms(), g.cpu, g.mem_load, g.io);
        }

        let columns_moved = g.show_pid != self.gauges.show_pid;
        let changed = columns_moved
            || count != self.count
            || totals != self.totals
            || g.heat != self.gauges.heat
            || sort != self.sort;
        self.nf = nf;
        self.sort = sort;
        self.gauges = g;
        if !changed {
            if fresh {
                // The traces repaint alone: three 12 px rects, no blit.
                let scale = cx.scale();
                for r in self.trace_rects(scale).into_iter().flatten() {
                    cx.damage(r);
                }
            }
            return;
        }
        self.count = count;
        self.totals = totals;
        let (scale, ts) = (cx.scale(), cx.tokens().text_scale);
        self.measure(cx.text(), scale, ts);
        if self.rect.is_empty() || columns_moved {
            cx.relayout();
        } else {
            cx.damage(self.rect);
        }
    }

    /// The widths the paint needs: the totals (the heat plate sits behind them) and the labels (the chevron of the name
    /// column follows its label).
    fn measure(&mut self, eng: &mut dyn TextEngine, scale: Scale, text_scale: f32) {
        let strong = eng.font(TypeRole::BodyStrong, None, scale, text_scale);
        let caption = eng.font(TypeRole::Caption, None, scale, text_scale);
        self.count_w = eng.width(&self.count, strong);
        for (w, s) in self.total_w.iter_mut().zip(&self.totals) {
            *w = eng.width(s, strong);
        }
        self.label_w = self.slots.iter().map(|s| eng.width(&s.label, caption)).collect();
    }

    /// The three trace rects (CPU, Memory, I/O), None before the first layout.
    fn trace_rects(&self, scale: Scale) -> [Option<RectP>; 3] {
        let mut out = [None; 3];
        for s in &self.slots {
            let which = match s.col {
                ColumnId::Cpu => 0,
                ColumnId::Memory => 1,
                ColumnId::Io => 2,
                _ => continue,
            };
            let top = self.rect.t + scale.px(history::TRACE_TOP);
            let r = RectP { l: s.rect.l, r: s.rect.r - scale.px(12), t: top, b: top + scale.px(history::TRACE_H) };
            out[which] = (!r.is_empty()).then_some(r);
        }
        out
    }

    /// The samples behind the traces.
    pub fn history(&self) -> &History {
        &self.history
    }

    fn node_of(&self, i: usize) -> NodeId {
        NodeId::new(HEADER, 1 + i as u32)
    }

    fn slot_of(&self, n: NodeId) -> Option<usize> {
        let i = (n.slot() as usize).checked_sub(1)?;
        (n.owner() == HEADER && i < self.slots.len()).then_some(i)
    }

    /// The accessible name of a cell: its title and its total, "CPU, total 88.7 %".
    fn name_of(&self, lang: UiLang, i: usize) -> Text {
        let Some(s) = self.slots.get(i) else { return Text::Borrowed("") };
        let total = match s.col {
            ColumnId::Name => self.count.as_str(),
            ColumnId::Cpu => self.totals[0].as_str(),
            ColumnId::Memory => self.totals[1].as_str(),
            ColumnId::Io => self.totals[2].as_str(),
            _ => "",
        };
        if total.is_empty() {
            return Text::Owned(s.label.to_string());
        }
        if s.col == ColumnId::Name {
            return Text::Owned(format!("{}, {}", s.label, total));
        }
        let args = [("label", &*s.label), ("total", total)];
        Text::Owned(oled_sys::i18n::subst(&t(lang, k::TASKS_A11Y_HEADER_TOTAL), &args))
    }

    /// The columns' rects: the table's own widths (Column::width, the flex one takes the rest, none below its minimum)
    /// laid left to right from `rect.l`. Kept in step with the table by sharing rows::columns.
    pub fn column_rects(cols: &[oled_ui::model::Column], rect: RectP, scale: Scale, min_w: i32) -> Vec<(ColId, RectP)> {
        let total = scale.logical(rect.w());
        let w_of = |c: &oled_ui::model::Column| c.width.max(c.min).max(min_w);
        let fixed: i32 = cols.iter().filter(|c| c.visible && !c.flex).map(w_of).sum();
        let mut x = rect.l;
        let mut out = Vec::new();
        for c in cols.iter().filter(|c| c.visible) {
            let w = if c.flex { (total - fixed).max(w_of(c)) } else { w_of(c) };
            let w = scale.px(w);
            out.push((c.id, RectP { l: x, t: rect.t, r: x + w, b: rect.b }));
            x += w;
        }
        out
    }
}

impl GaugeHeader {
    /// The traces under the three numeric columns: the fill from the baseline at chart_fill, the line along the top at
    /// FG_SECONDARY, each one PaintCx::spans call over the device columns of its rect.
    fn paint_traces(&self, cx: &mut PaintCx, c: &oled_ui::ColorTokens) {
        if self.history.is_empty() {
            return;
        }
        let scale = cx.scale();
        let hair = history::hairline(scale.dpi as f32 / 96.0);
        let height = cx.px(history::TRACE_H);
        for (which, r) in self.trace_rects(scale).into_iter().enumerate() {
            let Some(r) = r else { continue };
            if !cx.visible(r) {
                continue;
            }
            let t = self.history.trace(which, r.w(), height, hair, r.b);
            if t.fill.is_empty() {
                continue;
            }
            cx.spans(r.l + t.x_off, &t.fill, c.chart_fill);
            cx.spans(r.l + t.x_off, &t.line, c.fg_secondary);
        }
    }
}

impl Widget for GaugeHeader {
    type Out = HeaderOut;

    fn layout(&mut self, cx: &mut LayoutCx, rect: RectP) {
        self.rect = rect;
        let cols = rows::columns_with(cx.lang(), self.gauges.show_pid);
        self.slots.clear();
        for (id, r) in GaugeHeader::column_rects(&cols, rect, cx.scale(), oled_ui::tokens::COL_MIN_W) {
            let Some(col) = column_of(id) else { continue };
            if let Some(c) = cols.iter().find(|c| c.id == id) {
                self.slots.push(Slot { col, rect: r, label: c.title.clone(), desc: c.tip.clone() });
            }
        }
        let (scale, ts) = (cx.scale(), cx.tokens().text_scale);
        self.measure(cx.text(), scale, ts);
        self.cur = self.cur.min(self.slots.len().saturating_sub(1));
        for i in 0..self.slots.len() {
            let s = &self.slots[i];
            let plate = RectP { t: rect.t + cx.px(2), b: rect.b - cx.px(2), ..s.rect };
            let mut node = Node::new(self.node_of(i), Role::ColumnHeader, plate)
                .label(self.name_of(cx.lang(), i))
                .focus(if i == self.cur { Focusable::Tab } else { Focusable::Roving })
                .actions(Actions::CLICK | Actions::CONTEXT_MENU | Actions::FOCUS);
            if let Some(d) = &s.desc {
                node = node.description(d.clone());
            }
            cx.push(node);
        }
        // The held look's pause glyph: its tip is the held explanation.
        if self.held {
            if let Some(i) = self.slots.iter().position(|s| s.col == self.sort.column) {
                let (_, pause) = self.sort_marks(i, &|v| cx.px(v));
                let tip = Text::Owned(t(cx.lang(), k::TASKS_HELD_TIP).to_string());
                cx.push(
                    Node::new(NodeId::new(HEADER, 1 + self.slots.len() as u32), Role::Image, pause)
                        .label(tip.clone())
                        .description(tip),
                );
            }
        }
    }

    fn paint(&self, cx: &mut PaintCx) {
        let rect = self.rect;
        if rect.is_empty() || !cx.visible(rect) {
            return;
        }
        let c = cx.tokens().color.clone();
        let radius = cx.px(cx.tokens().radius.s);
        for (i, s) in self.slots.iter().enumerate() {
            let id = self.node_of(i);
            let plate = RectP { t: rect.t + cx.px(2), b: rect.b - cx.px(2), ..s.rect };
            if cx.pressed(id) {
                cx.fill(plate, radius, c.fill_press);
            } else {
                let t = cx.hover_t(id);
                if t > 0.0 {
                    cx.fill(plate, radius, faded(c.fill_hover, t));
                }
            }
            let numeric = s.col != ColumnId::Name;
            let label_r = RectP { t: rect.t + cx.px(6), b: rect.t + cx.px(6) + cx.px(16), ..plate };
            let total_r = RectP { t: rect.t + cx.px(24), b: rect.t + cx.px(24) + cx.px(18), ..plate };
            let sorted = self.sort.column == s.col;
            let glyph = match (self.sort.descending, self.held) {
                (true, false) => Glyph::SortDown,
                (false, false) => Glyph::SortUp,
                (true, true) => Glyph::SortDownOutline,
                (false, true) => Glyph::SortUpOutline,
            };
            let (chev_r, pause_r) = self.sort_marks(i, &|v| cx.px(v));
            if sorted && self.held {
                cx.glyph(pause_r, Glyph::Pause, c.fg_secondary);
            }
            if numeric {
                let right = s.rect.r - cx.px(12);
                cx.text_in(
                    &s.label,
                    TypeRole::Caption,
                    RectP { l: s.rect.l, r: right, ..label_r },
                    Align::End,
                    Trim::End,
                    c.fg_secondary,
                );
                if sorted {
                    cx.glyph(chev_r, glyph, c.fg_secondary);
                }
                // The PID column (List mode) has no total.
                let which = match s.col {
                    ColumnId::Cpu => Some(0),
                    ColumnId::Memory => Some(1),
                    ColumnId::Io => Some(2),
                    _ => None,
                };
                if let Some(which) = which {
                    let step = self.gauges.heat[which];
                    let w = self.total_w[which];
                    if let Some(n) = step_index(step) {
                        let pad = cx.px(3);
                        cx.fill(RectP { l: right - w - pad, r: right + pad, ..total_r }, 0, c.heat[n]);
                    }
                    let dash = self.totals[which] == "\u{2014}";
                    let fg = if dash { c.fg_tertiary } else { step_fg(step, &c) };
                    cx.text_in(
                        &self.totals[which],
                        TypeRole::BodyStrong,
                        RectP { l: s.rect.l, r: right, ..total_r },
                        Align::End,
                        Trim::End,
                        fg,
                    );
                }
            } else {
                let left = s.rect.l + cx.px(16);
                cx.text_in(
                    &s.label,
                    TypeRole::Caption,
                    RectP { l: left, r: s.rect.r - cx.px(8), ..label_r },
                    Align::Start,
                    Trim::End,
                    c.fg_secondary,
                );
                if sorted {
                    cx.glyph(chev_r, glyph, c.fg_secondary);
                }
                cx.text_in(
                    &self.count,
                    TypeRole::BodyStrong,
                    RectP { l: left, r: s.rect.r - cx.px(8), ..total_r },
                    Align::Start,
                    Trim::End,
                    c.fg_primary,
                );
            }
            if cx.focused(id) {
                cx.focus_ring(plate, radius, false);
            }
        }
        self.paint_traces(cx, &c);
        cx.hline(rect.l, rect.r, rect.b - 1, c.separator);
    }

    fn event(&mut self, cx: &mut EventCx, ev: &Event) -> Option<HeaderOut> {
        let i = self.slot_of(cx.target()?)?;
        let col = self.slots.get(i)?.col;
        let below = |slot: &Slot| PointP { x: slot.rect.l, y: slot.rect.b };
        match ev {
            Event::FocusIn(_) => {
                if self.cur != i {
                    self.cur = i;
                    cx.relayout();
                }
                None
            }
            Event::Click { button: MouseButton::Left, .. } | Event::A11y(_, oled_ui::A11yAction::Click) => {
                Some(HeaderOut::Sort(col))
            }
            Event::Click { button: MouseButton::Right, pos, .. } => Some(HeaderOut::Menu(*pos)),
            Event::A11y(_, oled_ui::A11yAction::ShowContextMenu) => {
                self.slots.get(i).map(|s| HeaderOut::Menu(below(s)))
            }
            Event::Key { key: Key::ENTER, down: true, .. } => {
                cx.consume();
                Some(HeaderOut::Sort(col))
            }
            Event::Key { key: Key::SPACE, down, .. } => {
                cx.consume();
                (!*down).then_some(HeaderOut::Sort(col))
            }
            Event::Key { key: Key::APPS, down: true, .. } => {
                cx.consume();
                self.slots.get(i).map(|s| HeaderOut::Menu(below(s)))
            }
            Event::Key { key, down: true, .. } if *key == Key::LEFT || *key == Key::RIGHT => {
                let to = if *key == Key::LEFT { i.saturating_sub(1) } else { (i + 1).min(self.slots.len() - 1) };
                cx.consume();
                cx.set_focus(self.node_of(to));
                None
            }
            _ => None,
        }
    }
}

/// SORT_COL_1 .. SORT_COL_4: the nth visible column.
pub fn on_command(s: &mut TasksScreen, cx: &mut EventCx, id: CommandId) {
    let n = match id {
        SORT_COL_1 => 0,
        SORT_COL_2 => 1,
        SORT_COL_3 => 2,
        SORT_COL_4 => 3,
        _ => return,
    };
    let cols = rows::columns(cx.lang());
    let Some(col) = cols.iter().filter(|c| c.visible).nth(n).and_then(|c| column_of(c.id)) else { return };
    s.v.set_sort(col);
    let now = cx.now_ms();
    s.run_pipeline(cx, now);
}
