//! Sparkline and the peak-preserving resampler (K4, P2-kit 2.10): filled by K4.1. No hue in charts.

use crate::cx::{EventCx, LayoutCx, PaintCx};
use crate::event::Event;
use crate::geom::RectP;
use crate::node::{Text, WidgetId};
use crate::screen::Widget;
use crate::theme::Ink;

/// One series of a chart.
pub struct Series<'a> {
    pub values: &'a [f32],
    pub ink: Ink,
}

/// A small line chart with an accessible summary.
#[allow(dead_code)] // The fields are read by K4.1.
pub struct Sparkline {
    id: WidgetId,
    summary: Text,
    max: f32,
    threshold: Option<f32>,
}

impl Sparkline {
    pub fn new(id: WidgetId, summary: Text) -> Sparkline {
        Sparkline { id, summary, max: 0.0, threshold: None }
    }

    /// The series to draw, the value at the top and an optional threshold line. Filled by K4.1.
    pub fn set(&mut self, series: &[Series<'_>], max: f32, threshold: Option<f32>) {
        let _ = series;
        self.max = max;
        self.threshold = threshold;
    }
}

impl Widget for Sparkline {
    type Out = ();

    /// Filled by K4.1; until then it pushes nothing.
    fn layout(&mut self, cx: &mut LayoutCx, rect: RectP) {
        let _ = (cx, rect);
    }

    /// Filled by K4.1; until then it draws nothing.
    fn paint(&self, cx: &mut PaintCx) {
        let _ = cx;
    }

    /// Filled by K4.1; until then nothing happens.
    fn event(&mut self, cx: &mut EventCx, ev: &Event) -> Option<()> {
        let _ = (cx, ev);
        None
    }
}

/// `values` resampled to `columns`, keeping every peak (a 1-sample spike survives at any width). Pure. Filled by K4.1;
/// until then empty.
pub fn resample_peak(values: &[f32], columns: usize) -> Vec<f32> {
    let _ = (values, columns);
    Vec::new()
}
