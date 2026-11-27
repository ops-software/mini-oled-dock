//! Sparkline, AreaChart and the peak-preserving resampler (K4, P2-kit 2.10, K4.1). No hue in charts: the series take the
//! inks the caller gives (the kit's grey text roles), the grid is CHART_GRID, the fill CHART_FILL; the one colour is
//! ACCENT_ATTENTION, past a threshold. Nothing tweens: a value is a column of pixels the moment it arrives.

use crate::cx::{EventCx, LayoutCx, PaintCx};
use crate::event::Event;
use crate::geom::{RectP, SizeP};
use crate::model::Align;
use crate::node::{Node, NodeId, Role, Text, WidgetId};
use crate::screen::Widget;
use crate::text::Trim;
use crate::theme::{Ink, TypeRole};

/// One series of a chart.
pub struct Series<'a> {
    pub values: &'a [f32],
    pub ink: Ink,
}

/// The widest history a trace keeps before layout tells it the width of its strip.
const TRACE_KEEP: usize = 4096;
/// The gap between the labels and the plot, logical px.
const LABEL_GAP: i32 = 6;

/// A series as the chart stores it.
struct Stored {
    values: Vec<f32>,
    ink: Ink,
}

/// What differs between the two charts.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Style {
    /// One 50 % hairline, no labels.
    Strip,
    /// Lines every 25 %, labels at the top and the bottom.
    Area,
}

/// The state both charts share.
struct Plot {
    id: WidgetId,
    summary: Text,
    style: Style,
    series: Vec<Stored>,
    max: f32,
    threshold: Option<f32>,
    /// Trace mode: one sample per device column, the newest at the right edge.
    trace: bool,
    labels: Option<(Text, Text)>,
    rect: RectP,
    /// The label gutter, physical px, set by layout.
    gutter: i32,
}

/// A small line chart with an accessible summary: the stroke of each series over the area under the first.
pub struct Sparkline {
    plot: Plot,
}

/// The larger chart: a Sparkline's drawing with a grid every 25 % and labels at the top and the bottom.
pub struct AreaChart {
    plot: Plot,
}

impl Sparkline {
    pub fn new(id: WidgetId, summary: Text) -> Sparkline {
        Sparkline { plot: Plot::new(id, summary, Style::Strip) }
    }

    /// The series to draw, the value at the top and an optional threshold: the first series' columns above it take
    /// ACCENT_ATTENTION.
    pub fn set(&mut self, series: &[Series<'_>], max: f32, threshold: Option<f32>) {
        self.plot.set(series, max, threshold);
    }

    /// Trace mode (the 12 px header traces): every sample is one device column and a new sample moves the picture
    /// one column left. Only the first series is traced; feed it with push.
    pub fn trace(mut self, on: bool) -> Sparkline {
        self.plot.trace = on;
        self
    }

    /// Appends one sample to the first series (made with the grey of the dock's secondary text when there is none)
    /// and damages the strip. Never smooth: the next paint steps one column.
    pub fn push(&mut self, cx: &mut EventCx, value: f32) {
        let keep = if self.plot.rect.w() > 0 { self.plot.rect.w() as usize } else { TRACE_KEEP };
        let ink = Ink::white(160);
        let s = match self.plot.series.first_mut() {
            Some(s) => s,
            None => {
                self.plot.series.push(Stored { values: Vec::new(), ink });
                &mut self.plot.series[0]
            }
        };
        s.values.push(value);
        if s.values.len() > keep {
            let over = s.values.len() - keep;
            s.values.drain(..over);
        }
        cx.damage(self.plot.rect);
    }

    /// The node of the chart.
    pub fn node_id(&self) -> NodeId {
        self.plot.node_id()
    }
}

impl AreaChart {
    pub fn new(id: WidgetId, summary: Text) -> AreaChart {
        AreaChart { plot: Plot::new(id, summary, Style::Area) }
    }

    /// See Sparkline::set.
    pub fn set(&mut self, series: &[Series<'_>], max: f32, threshold: Option<f32>) {
        self.plot.set(series, max, threshold);
    }

    /// The labels of the top line (the value of `max`) and of the bottom line.
    pub fn set_labels(&mut self, top: Text, bottom: Text) {
        self.plot.labels = Some((top, bottom));
    }

    /// The node of the chart.
    pub fn node_id(&self) -> NodeId {
        self.plot.node_id()
    }
}

impl Plot {
    fn new(id: WidgetId, summary: Text, style: Style) -> Plot {
        Plot {
            id,
            summary,
            style,
            series: Vec::new(),
            max: 0.0,
            threshold: None,
            trace: false,
            labels: None,
            rect: RectP::EMPTY,
            gutter: 0,
        }
    }

    fn node_id(&self) -> NodeId {
        NodeId::new(self.id, 0)
    }

    fn set(&mut self, series: &[Series<'_>], max: f32, threshold: Option<f32>) {
        self.series = series.iter().map(|s| Stored { values: s.values.to_vec(), ink: s.ink }).collect();
        self.max = max;
        self.threshold = threshold;
    }

    fn layout(&mut self, cx: &mut LayoutCx, rect: RectP) {
        self.rect = rect;
        self.gutter = 0;
        if let (Style::Area, Some((top, bottom))) = (self.style, &self.labels) {
            let font = cx.font(TypeRole::Caption);
            let (a, b) = (cx.text().width(top, font), cx.text().width(bottom, font));
            self.gutter = a.max(b) + cx.px(LABEL_GAP);
        }
        cx.push(Node::new(self.node_id(), Role::Image, rect).label(self.summary.clone()));
    }

    /// Where the lines go: the rect without the label gutter, and for the labelled chart a half label line above and
    /// below, so the top and bottom labels centre on the first and last line.
    fn area(&self, half_line: i32) -> RectP {
        let r = RectP { l: self.rect.l + self.gutter, ..self.rect };
        if self.style == Style::Area && self.labels.is_some() {
            RectP { t: r.t + half_line, b: r.b - half_line, ..r }
        } else {
            r
        }
    }

    fn paint(&self, cx: &mut PaintCx) {
        if self.rect.is_empty() {
            return;
        }
        let c = cx.tokens().color.clone();
        let half = cx.px(TypeRole::Caption.spec().line) / 2;
        let area = self.area(half);
        if area.is_empty() {
            return;
        }
        let (base, h, cols) = (area.b, area.h(), area.w() as usize);

        // The grid, under everything.
        let steps: &[i32] = if self.style == Style::Strip { &[2] } else { &[1, 2, 3, 4] };
        for k in steps {
            let y = base - (h * k + 2) / 4;
            cx.hline(area.l, area.r, y.clamp(area.t, base - 1), c.chart_grid);
        }

        if let Some((top, bottom)) = &self.labels {
            if self.style == Style::Area {
                let gap = cx.px(LABEL_GAP);
                for (text, y) in [(top, area.t), (bottom, base)] {
                    let r = RectP { l: self.rect.l, t: y - half, r: area.l - gap, b: y + half };
                    cx.text_in(text, TypeRole::Caption, r, Align::End, Trim::End, c.fg_tertiary);
                }
            }
        }

        // Series 0 first (its fill sits under every stroke), then the others on top.
        let spans = Spans { base, x0: area.l };
        for (n, s) in self.series.iter().enumerate() {
            let vals = self.columns(&s.values, cols);
            let levels: Vec<Option<Level>> = vals.iter().map(|v| v.map(|v| Level::of(v, self.max, h))).collect();
            if n == 0 {
                spans.fill(cx, &levels, c.chart_fill);
            }
            for (i, val) in vals.iter().enumerate() {
                let hot = n == 0 && matches!((self.threshold, val), (Some(t), Some(v)) if *v > t);
                let ink = if n == 0 && i + 1 == levels.len() {
                    c.fg_primary
                } else if hot {
                    c.accent_attention
                } else {
                    s.ink
                };
                spans.stroke(cx, &levels, i, ink);
            }
        }
    }

    /// The value of every device column (None: nothing to draw there).
    fn columns(&self, values: &[f32], cols: usize) -> Vec<Option<f32>> {
        if self.trace {
            // One sample per column, the newest at the right edge.
            let shown = &values[values.len().saturating_sub(cols)..];
            let mut v = vec![None; cols - shown.len()];
            v.extend(shown.iter().map(|x| Some(*x)));
            return v;
        }
        if values.is_empty() {
            return vec![None; cols];
        }
        resample_peak(values, cols).into_iter().map(Some).collect()
    }
}

/// A column's height: whole rows from the baseline and the coverage (0..=255) of the row above them.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Level {
    whole: i32,
    frac: u32,
}

impl Level {
    /// `v` of `max` on a plot `h` px high. A value of nothing still draws the bottom row, so the baseline shows.
    pub(crate) fn of(v: f32, max: f32, h: i32) -> Level {
        let ratio = if max > 0.0 && v > 0.0 { (v / max).min(1.0) } else { 0.0 };
        let top = (ratio * h as f32).clamp(1.0, h.max(1) as f32);
        let whole = top.floor() as i32;
        let frac = ((top - whole as f32) * 255.0).round() as u32;
        if frac >= 255 {
            Level { whole: whole + 1, frac: 0 }
        } else if whole >= h {
            Level { whole: h, frac: 0 }
        } else {
            Level { whole, frac }
        }
    }

    /// The row (from the baseline, 0 = bottom) of the stroke's lower pixel.
    fn low(self) -> i32 {
        self.whole - 1
    }

    /// The highest row the stroke touches.
    fn high(self) -> i32 {
        if self.frac > 0 {
            self.whole
        } else {
            self.whole - 1
        }
    }
}

/// The rows (lowest, highest) column `i` covers with its stroke and its connector to column `i + 1`: what the
/// connectedness test reads.
#[cfg(test)]
pub(crate) fn stroke_rows(levels: &[Option<Level>], i: usize) -> Option<(i32, i32)> {
    let me = levels.get(i).copied().flatten()?;
    let (mut lo, mut hi) = (me.low(), me.high());
    if let Some(next) = levels.get(i + 1).copied().flatten() {
        lo = lo.min(next.high());
        hi = hi.max(next.high());
    }
    Some((lo, hi))
}

/// Draws columns with the baseline and the left edge of one plot.
struct Spans {
    base: i32,
    x0: i32,
}

impl Spans {
    /// The window rows [y0, y1) of the plot rows lo..=hi (from the baseline).
    fn rows(&self, lo: i32, hi: i32) -> (i16, i16) {
        ((self.base - 1 - hi) as i16, (self.base - lo) as i16)
    }

    /// The area under the stroke of every column.
    fn fill(&self, cx: &mut PaintCx, levels: &[Option<Level>], ink: Ink) {
        let spans: Vec<(i16, i16)> = levels
            .iter()
            .map(|l| match l {
                Some(l) if l.low() > 0 => self.rows(0, l.low() - 1),
                _ => (0, 0),
            })
            .collect();
        cx.spans(self.x0, &spans, ink);
    }

    /// Column `i`'s stroke: its own pixel row (and the partly covered one above it) and the connector to the next
    /// column's top.
    fn stroke(&self, cx: &mut PaintCx, levels: &[Option<Level>], i: usize, ink: Ink) {
        let Some(me) = levels[i] else { return };
        let x = self.x0 + i as i32;
        let one = |cx: &mut PaintCx, lo: i32, hi: i32, ink: Ink| {
            if lo <= hi {
                cx.spans(x, &[self.rows(lo, hi)], ink);
            }
        };
        if me.frac > 0 {
            one(cx, me.whole, me.whole, Ink { a: (ink.a * me.frac + 127) / 255, ..ink });
            one(cx, me.low(), me.low(), Ink { a: (ink.a * (255 - me.frac) + 127) / 255, ..ink });
        } else {
            one(cx, me.low(), me.low(), ink);
        }
        if let Some(next) = levels.get(i + 1).copied().flatten() {
            if next.high() > me.high() {
                one(cx, me.high() + 1, next.high(), ink);
            } else if next.high() < me.low() {
                one(cx, next.high(), me.low() - 1, ink);
            }
        }
    }
}

impl Widget for Sparkline {
    type Out = ();

    fn layout(&mut self, cx: &mut LayoutCx, rect: RectP) {
        self.plot.layout(cx, rect);
    }

    fn paint(&self, cx: &mut PaintCx) {
        self.plot.paint(cx);
    }

    fn event(&mut self, cx: &mut EventCx, ev: &Event) -> Option<()> {
        let _ = (cx, ev);
        None
    }

    fn preferred(&mut self, cx: &mut LayoutCx) -> SizeP {
        SizeP { w: cx.px(120), h: cx.px(24) }
    }
}

impl Widget for AreaChart {
    type Out = ();

    fn layout(&mut self, cx: &mut LayoutCx, rect: RectP) {
        self.plot.layout(cx, rect);
    }

    fn paint(&self, cx: &mut PaintCx) {
        self.plot.paint(cx);
    }

    fn event(&mut self, cx: &mut EventCx, ev: &Event) -> Option<()> {
        let _ = (cx, ev);
        None
    }

    fn preferred(&mut self, cx: &mut LayoutCx) -> SizeP {
        SizeP { w: cx.px(240), h: cx.px(96) }
    }
}

/// `values` resampled to `columns`, keeping every peak (a 1-sample spike survives at any width). Pure. Each column
/// takes the largest sample of the stretch it covers; with fewer samples than columns each sample is held. Missing
/// (NaN) samples count as 0; no values give zeros.
pub fn resample_peak(values: &[f32], columns: usize) -> Vec<f32> {
    let n = values.len();
    let clean = |v: f32| if v.is_nan() { 0.0 } else { v };
    (0..columns)
        .map(|c| {
            if n == 0 {
                return 0.0;
            }
            let start = c * n / columns;
            let end = ((c + 1) * n / columns).max(start + 1).min(n);
            values[start..end].iter().copied().map(clean).fold(f32::NEG_INFINITY, f32::max)
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::command::CommandTable;
    use crate::event::Reply;
    use crate::geom::SizeL;
    use crate::harness::{Harness, Image};
    use crate::host::AppMsg;
    use crate::screen::Screen;
    use crate::theme::Tokens;

    /// A chart on its own in a screen; a data message of f32 is pushed as a trace sample.
    struct Rig<W: Widget<Out = ()>> {
        w: W,
        rect: (i32, i32, i32, i32),
        push: fn(&mut W, &mut EventCx, f32),
        cmds: CommandTable,
    }

    impl<W: Widget<Out = ()>> Screen for Rig<W> {
        fn layout(&mut self, cx: &mut LayoutCx, avail: SizeP) -> SizeP {
            let (x, y, w, h) = self.rect;
            self.w.layout(cx, RectP::wh(cx.px(x), cx.px(y), cx.px(w), cx.px(h)));
            avail
        }

        fn paint(&self, cx: &mut PaintCx) {
            self.w.paint(cx);
        }

        fn event(&mut self, cx: &mut EventCx, ev: &Event) -> Reply {
            self.w.event(cx, ev);
            Reply::None
        }

        fn data(&mut self, cx: &mut EventCx, msg: AppMsg) -> Reply {
            if let Ok(v) = msg.downcast::<f32>() {
                (self.push)(&mut self.w, cx, *v);
            }
            Reply::None
        }

        fn commands(&self) -> &CommandTable {
            &self.cmds
        }
    }

    fn grey(img: &Image) -> bool {
        img.px.iter().all(|p| (p >> 16) & 0xFF == (p >> 8) & 0xFF && (p >> 8) & 0xFF == p & 0xFF)
    }

    fn area_rig(values: &[f32], threshold: Option<f32>) -> Rig<AreaChart> {
        let mut ch = AreaChart::new(WidgetId(1), Text::from("CPU 23 %, peak 61 % in the last 60 s"));
        ch.set(&[Series { values, ink: Tokens::oled().color.fg_secondary }], 100.0, threshold);
        ch.set_labels(Text::from("100"), Text::from("0"));
        Rig { w: ch, rect: (8, 8, 200, 80), push: |_, _, _| {}, cmds: CommandTable::new() }
    }

    fn render(r: Rig<AreaChart>, dpi: u32) -> Image {
        Harness::new(r, SizeL { w: 216, h: 96 }, dpi, Tokens::oled()).render()
    }

    #[test]
    fn spike_survives_resampling() {
        let mut v = vec![0.0f32; 10_000];
        v[37] = 1.0;
        for columns in [37, 120, 600] {
            let r = resample_peak(&v, columns);
            assert_eq!(r.len(), columns);
            assert_eq!(r.iter().filter(|x| **x == 1.0).count(), 1, "one column holds the spike at {columns}");
            assert!(r.iter().all(|x| *x == 0.0 || *x == 1.0));
        }
    }

    #[test]
    fn every_sample_is_covered_by_some_column() {
        let v: Vec<f32> = (0..1000).map(|i| ((i * 7919) % 1000) as f32).collect();
        for columns in [1, 3, 37, 999, 1000, 1001, 4000] {
            let r = resample_peak(&v, columns);
            let top = r.iter().copied().fold(0.0, f32::max);
            assert_eq!(top, 999.0, "{columns} columns");
        }
    }

    #[test]
    fn resampling_edges() {
        assert!(resample_peak(&[1.0, 2.0], 0).is_empty());
        assert_eq!(resample_peak(&[], 3), [0.0; 3]);
        assert_eq!(resample_peak(&[f32::NAN, 2.0], 2), [0.0, 2.0]);
        assert_eq!(resample_peak(&[1.0, 2.0], 4), [1.0, 1.0, 2.0, 2.0], "held, not interpolated");
    }

    #[test]
    fn levels_split_whole_rows_and_coverage() {
        assert_eq!(Level::of(0.0, 100.0, 40), Level { whole: 1, frac: 0 }, "the baseline still shows");
        assert_eq!(Level::of(100.0, 100.0, 40), Level { whole: 40, frac: 0 });
        assert_eq!(Level::of(500.0, 100.0, 40), Level { whole: 40, frac: 0 }, "clamped");
        assert_eq!(Level::of(f32::NAN, 100.0, 40), Level { whole: 1, frac: 0 });
        let half = Level::of(50.0, 100.0, 41);
        assert_eq!((half.whole, half.frac), (20, 128), "20.5 rows");
        assert_eq!(Level::of(1.0, 0.0, 40), Level { whole: 1, frac: 0 }, "no scale, no height");
    }

    #[test]
    fn spans_connect_columns() {
        // A sawtooth, a plateau, a spike and a cliff: no vertical gap between neighbours.
        let heights = [3, 3, 30, 4, 5, 5, 40, 40, 1, 20, 21, 22, 2];
        let levels: Vec<Option<Level>> = heights.iter().map(|h| Some(Level::of(*h as f32, 40.0, 40))).collect();
        for i in 0..levels.len() - 1 {
            let (lo, hi) = stroke_rows(&levels, i).unwrap();
            let own = stroke_rows(&levels[i + 1..], 0).unwrap();
            assert!(lo <= own.1 && own.0 <= hi, "columns {i} and {} do not touch: {:?} vs {:?}", i + 1, (lo, hi), own);
        }
        // The drawn picture agrees: every column of the plot has ink and a neighbour's rows touch.
        let vals: Vec<f32> = heights.iter().map(|h| *h as f32).collect();
        let img = render(area_rig(&vals, None), 96);
        let lit = |x: i32, y: i32| img.px[(y * img.w as i32 + x) as usize] & 0xFF > 60;
        for x in 60..208 {
            assert!((8..88).any(|y| lit(x, y)), "column {x} has no stroke");
        }
    }

    #[test]
    fn the_stroke_is_the_top_with_coverage_and_the_fill_sits_below() {
        let flat = [50.0f32; 20];
        let img = render(area_rig(&flat, None), 96);
        let at = |x: i32, y: i32| img.px[(y * img.w as i32 + x) as usize] & 0xFF;
        // Plot: y 14..82 (68 rows, half a caption line in at both ends), x 8..208 gutter excluded; column 150 is mid.
        let col: Vec<u32> = (0..96).map(|y| at(150, y)).collect();
        let stroke_rows: Vec<usize> = (0..96).filter(|y| col[*y] >= 100).collect();
        assert!(!stroke_rows.is_empty() && stroke_rows.len() <= 2, "{col:?}");
        let below = stroke_rows.last().unwrap() + 3;
        assert!(col[below] >= 20 && col[below] <= 60, "area fill is CHART_FILL plus the grid: {}", col[below]);
        let above = stroke_rows[0] - 3;
        assert!(col[above] < 30, "nothing above the line but the grid: {}", col[above]);
    }

    #[test]
    fn a_chart_has_no_hue_until_the_threshold() {
        let vals: Vec<f32> = (0..50).map(|i| (i * 2) as f32).collect();
        assert!(grey(&render(area_rig(&vals, None), 96)), "no threshold");
        assert!(grey(&render(area_rig(&vals, Some(500.0)), 96)), "a threshold nothing reaches");
        assert!(
            !grey(&render(area_rig(&vals, Some(60.0)), 96)),
            "past the threshold the stroke takes the attention colour"
        );
    }

    #[test]
    fn the_last_column_is_primary() {
        let flat = [40.0f32; 8];
        let mut img = render(area_rig(&flat, None), 96);
        // Right edge of the plot: x = 8 + 200 - 1; the neighbour is secondary.
        let rows: Vec<i32> = (14..82).collect();
        let peak =
            |img: &Image, x: i32| rows.iter().map(|y| img.px[(*y * img.w as i32 + x) as usize] & 0xFF).max().unwrap();
        let (last, before) = (peak(&img, 207), peak(&img, 206));
        assert!(last > before, "the newest column is brighter: {last} vs {before}");
        img.px.clear();
    }

    #[test]
    fn labels_and_the_summary_are_named() {
        let h = Harness::new(area_rig(&[10.0, 20.0], None), SizeL { w: 216, h: 96 }, 96, Tokens::oled());
        let dump = h.a11y_dump();
        assert!(dump.contains("Image") && dump.contains("CPU 23 %, peak 61 % in the last 60 s"), "{dump}");
        h.assert_named();
    }

    #[test]
    fn the_four_dpis_keep_the_plot_inside_the_rect() {
        for dpi in [96, 120, 144, 192] {
            let vals: Vec<f32> = (0..300).map(|i| (i % 100) as f32).collect();
            let size = SizeL { w: 216, h: 96 };
            let img = Harness::new(area_rig(&vals, None), size, dpi, Tokens::oled()).render();
            let s = dpi as i32;
            // Nothing is drawn right of the rect (216 - 8 logical px at the right margin) or below it.
            let (right, bottom) = (img.w as i32 - 8 * s / 96, img.h as i32 - 8 * s / 96);
            for y in 0..img.h as i32 {
                for x in right..img.w as i32 {
                    assert_eq!(img.px[(y * img.w as i32 + x) as usize] & 0xFFFFFF, 0, "dpi {dpi} ink at {x},{y}");
                }
            }
            for y in bottom..img.h as i32 {
                for x in 0..img.w as i32 {
                    assert_eq!(img.px[(y * img.w as i32 + x) as usize] & 0xFFFFFF, 0, "dpi {dpi} ink at {x},{y}");
                }
            }
        }
    }

    fn trace_rig() -> Rig<Sparkline> {
        let mut sp = Sparkline::new(WidgetId(1), Text::from("CPU 23 %")).trace(true);
        let seed: Vec<f32> = (0..200).map(|i| ((i * 37) % 90) as f32).collect();
        sp.set(&[Series { values: &seed, ink: Tokens::oled().color.fg_secondary }], 100.0, None);
        Rig { w: sp, rect: (4, 4, 100, 12), push: |s, cx, v| s.push(cx, v), cmds: CommandTable::new() }
    }

    #[test]
    fn trace_steps_by_one_column_per_sample() {
        let size = SizeL { w: 108, h: 20 };
        for dpi in [96, 144] {
            let mut h = Harness::new(trace_rig(), size, dpi, Tokens::oled());
            let before = h.render();
            h.send(Box::new(77.0f32));
            let after = h.render();
            let s = dpi as i32;
            let (x0, x1) = (4 * s / 96, 104 * s / 96);
            // The strip moved one device column left: column x now shows what x + 1 showed, except the last two
            // (the newest sample, and the connector the previous newest one grows towards it).
            for y in 0..before.h as i32 {
                for x in x0..x1 - 2 {
                    let (new, old) = (
                        after.px[(y * after.w as i32 + x) as usize],
                        before.px[(y * before.w as i32 + x + 1) as usize],
                    );
                    assert_eq!(new, old, "dpi {dpi}: column {x} row {y}");
                }
            }
            assert_ne!(after.px, before.px);
        }
    }

    #[test]
    fn a_trace_keeps_no_more_than_its_width() {
        let mut h = Harness::new(trace_rig(), SizeL { w: 108, h: 20 }, 96, Tokens::oled());
        for i in 0..500 {
            h.send(Box::new((i % 90) as f32));
        }
        assert!(h.screen().w.plot.series[0].values.len() <= 100, "{}", h.screen().w.plot.series[0].values.len());
    }

    #[test]
    fn a_strip_draws_one_hairline_and_no_labels() {
        let mut sp = Sparkline::new(WidgetId(1), Text::from("Idle"));
        sp.set(&[Series { values: &[0.0; 30], ink: Tokens::oled().color.fg_secondary }], 100.0, None);
        let rig = Rig { w: sp, rect: (0, 0, 60, 20), push: |_, _, _| {}, cmds: CommandTable::new() };
        let img = Harness::new(rig, SizeL { w: 60, h: 20 }, 96, Tokens::oled()).render();
        let lit_rows: Vec<i32> =
            (0..20).filter(|y| (0..60).any(|x| img.px[(*y * 60 + x) as usize] & 0xFF != 0)).collect();
        assert_eq!(lit_rows.len(), 2, "the hairline at 50 % and the baseline: {lit_rows:?}");
        assert!(img.text_rects.is_empty());
    }
}
