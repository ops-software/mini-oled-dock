//! The gauge header: the CPU, Memory and I/O totals with their traces, the column titles and the sort, the held look.
//! Filled by TA-T1a (T1.3), TA-HDR (TH1, the traces) and TA-T2M (TH2, set_held's look).

use oled_sysmon::fmt::{self, NumFmt};
use oled_sysmon::heat::HeatStep;
use oled_ui::model::{Align, ColId};
use oled_ui::node::Text;
use oled_ui::text::{TextEngine, Trim};
use oled_ui::{
    Actions, CommandId, Event, EventCx, Focusable, Glyph, Ink, Key, LayoutCx, MouseButton, Node, NodeId, PaintCx,
    PointP, RectP, Role, Scale, TypeRole, Widget,
};

use super::commands::{SORT_COL_1, SORT_COL_2, SORT_COL_3, SORT_COL_4};
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
    /// The order is held (TA-T2M draws it). Stub (kept by T1a and TA-HDR): stores the flag.
    pub fn set_held(&mut self, cx: &mut EventCx, held: bool) {
        let _ = cx;
        self.held = held;
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
        let changed = count != self.count || totals != self.totals || g.heat != self.gauges.heat || sort != self.sort;
        self.nf = nf;
        self.sort = sort;
        self.gauges = g;
        if !changed {
            return;
        }
        self.count = count;
        self.totals = totals;
        let (scale, ts) = (cx.scale(), cx.tokens().text_scale);
        self.measure(cx.text(), scale, ts);
        if self.rect.is_empty() {
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

    fn node_of(&self, i: usize) -> NodeId {
        NodeId::new(HEADER, 1 + i as u32)
    }

    fn slot_of(&self, n: NodeId) -> Option<usize> {
        let i = (n.slot() as usize).checked_sub(1)?;
        (n.owner() == HEADER && i < self.slots.len()).then_some(i)
    }

    /// The accessible name of a cell: its title and its total, "CPU 88.7 %".
    fn name_of(&self, i: usize) -> Text {
        let Some(s) = self.slots.get(i) else { return Text::Borrowed("") };
        let total = match s.col {
            ColumnId::Name => self.count.as_str(),
            ColumnId::Cpu => self.totals[0].as_str(),
            ColumnId::Memory => self.totals[1].as_str(),
            ColumnId::Io => self.totals[2].as_str(),
            _ => "",
        };
        Text::Owned(format!("{} {}", s.label, total))
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

impl Widget for GaugeHeader {
    type Out = HeaderOut;

    fn layout(&mut self, cx: &mut LayoutCx, rect: RectP) {
        self.rect = rect;
        let cols = rows::columns(cx.lang());
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
                .label(self.name_of(i))
                .focus(if i == self.cur { Focusable::Tab } else { Focusable::Roving })
                .actions(Actions::CLICK | Actions::CONTEXT_MENU | Actions::FOCUS);
            if let Some(d) = &s.desc {
                node = node.description(d.clone());
            }
            cx.push(node);
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
            let label_w = self.label_w.get(i).copied().unwrap_or(0);
            let chev = cx.px(8);
            let chev_y = rect.t + cx.px(10);
            let sorted = self.sort.column == s.col;
            let glyph = if self.sort.descending { Glyph::SortDown } else { Glyph::SortUp };
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
                    cx.glyph(RectP::wh(s.rect.r - cx.px(10), chev_y, chev, chev), glyph, c.fg_secondary);
                }
                let which = match s.col {
                    ColumnId::Cpu => 0,
                    ColumnId::Memory => 1,
                    _ => 2,
                };
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
                    cx.glyph(RectP::wh(left + label_w + cx.px(4), chev_y, chev, chev), glyph, c.fg_secondary);
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
