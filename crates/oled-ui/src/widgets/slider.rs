//! Slider (F-K1, P2-kit 2.6): the frozen type; filled by K1b.1.

use crate::cx::{EventCx, LayoutCx, PaintCx};
use crate::event::Event;
use crate::geom::RectP;
use crate::node::{Text, WidgetId};
use crate::screen::Widget;

/// What a slider tells its screen: Changed while it moves, Committed when it is let go.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SliderOut {
    Changed(i32),
    Committed(i32),
}

/// A slider from min to max by step, with an optional value text.
#[allow(dead_code)] // The fields are read by K1b.1.
pub struct Slider {
    id: WidgetId,
    label: Text,
    min: i32,
    max: i32,
    step: i32,
    value: i32,
    value_text: Option<Text>,
}

impl Slider {
    pub fn new(id: WidgetId, label: Text, min: i32, max: i32, step: i32, value: i32) -> Slider {
        Slider { id, label, min, max, step, value: value.clamp(min, max.max(min)), value_text: None }
    }

    pub fn value(&self) -> i32 {
        self.value
    }

    /// The value, kept within min..=max.
    pub fn set_value(&mut self, v: i32) {
        self.value = v.clamp(self.min, self.max.max(self.min));
    }

    pub fn set_value_text(&mut self, t: Option<Text>) {
        self.value_text = t;
    }
}

impl Widget for Slider {
    type Out = SliderOut;

    /// Filled by K1b.1; until then it pushes nothing.
    fn layout(&mut self, cx: &mut LayoutCx, rect: RectP) {
        let _ = (cx, rect);
    }

    /// Filled by K1b.1; until then it draws nothing.
    fn paint(&self, cx: &mut PaintCx) {
        let _ = cx;
    }

    /// Filled by K1b.1; until then nothing happens.
    fn event(&mut self, cx: &mut EventCx, ev: &Event) -> Option<SliderOut> {
        let _ = (cx, ev);
        None
    }
}
