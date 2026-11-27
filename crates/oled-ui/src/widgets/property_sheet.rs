//! PropertySheet (K4.2): an options page built only from oled_config::desc::Descriptor values (P2-plumbing's F-P1).
//! Each row is one descriptor: the icon, the label (Body) and the help under it (Caption, secondary) at the left and the
//! control of the descriptor's kind at the right. The kit calls no oled-config function: labels and help are the
//! descriptor's own TextFn values, defaults its own fn. Swatches, ChordPicker, ActionPicker, List and Custom are the
//! dock's controls: they show as a disabled row with the descriptor's label.

use std::rc::Rc;

use oled_config::desc::{Cond, Control, Descriptor, Range, SecId, TextFn, Unit};

use super::{
    Button, ButtonKind, ButtonOut, FieldKind, FieldOut, SegOut, Segmented, Slider, SliderOut, TextField, Toggle,
    ToggleOut,
};
use crate::cx::{Anchor, EventCx, LayoutCx, MenuId, PaintCx};
use crate::event::Event;
use crate::geom::{PointP, RectP, SizeP};
use crate::glyph::Glyph;
use crate::model::Align;
use crate::node::{Node, NodeFlags, NodeId, Role, Text, WidgetId};
use crate::screen::Widget;
use crate::text::{LayoutOpts, TextLayout, Trim};
use crate::theme::TypeRole;

/// A value of an option: what a control shows and reports.
#[derive(Clone, Debug, PartialEq)]
pub enum PropValue {
    Bool(bool),
    Int(i64),
    /// A string, and the value string of a Segmented or Select choice.
    Text(String),
}

/// What the sheet tells its screen: the descriptor's JSON key and the new value.
pub type PropOut = (&'static str, PropValue);

/// Logical px: the left margin, the icon, the label column's start, the right margin, the gap before the control.
const LEFT: i32 = 12;
const ICON: i32 = 16;
const LABEL_X: i32 = 40;
const RIGHT: i32 = 16;
const GAP: i32 = 12;
/// The top and bottom padding of a row with help, a section header's height, and the gap above an untitled section.
const PAD: i32 = 8;
const SECTION_H: i32 = 32;
const SECTION_GAP: i32 = 12;
/// A text field's width and a slider's value text room are the controls' own; this is the field's.
const FIELD_W: i32 = 180;

/// The control of a row.
enum Ctl {
    Toggle(Toggle),
    Choice {
        w: Segmented,
        values: Vec<&'static str>,
    },
    Slider {
        w: Slider,
        range: Range,
    },
    Select {
        w: Button,
        values: Vec<&'static str>,
        open: Option<MenuId>,
    },
    Field(TextField),
    /// The dock's own controls: nothing to operate.
    Fixed,
}

/// One row: the descriptor, its control, its value and its place.
struct Row {
    d: Descriptor,
    ctl: Ctl,
    value: PropValue,
    glyph: Option<Glyph>,
    shown: bool,
    /// The row's top relative to the sheet and its height, physical px.
    top: i32,
    height: i32,
    /// The first line (the label's and the control's box), the help's place and its wrapped layout.
    line: RectP,
    help_at: PointP,
    help: Option<Rc<TextLayout>>,
    label_room: RectP,
    /// The control's width, physical px.
    ctl_w: i32,
}

// A sheet has a dozen rows: boxing each would only add an allocation per row.
#[allow(clippy::large_enum_variant)]
enum Item {
    /// A section's header: where it goes and how high, relative to the sheet; `shown` when a row of it shows.
    Header {
        sec: SecId,
        shown: bool,
        top: i32,
        height: i32,
    },
    Row(Row),
}

/// An options page built from descriptors.
pub struct PropertySheet {
    id: WidgetId,
    items: Vec<Item>,
    titles: Vec<(SecId, Text)>,
    rect: RectP,
}

fn text(f: TextFn) -> Text {
    Text::Borrowed(f())
}

/// The names a descriptor's glyph field can take and the shapes they mean (names are identifiers, not UI text).
#[rustfmt::skip]
const GLYPHS: [(&str, Glyph); 22] = [
    ("gear", Glyph::Gear), ("settings", Glyph::Gear), ("power", Glyph::Power), ("folder", Glyph::Folder), // lint: not UI
    ("clock", Glyph::Clock), ("time", Glyph::Clock), ("globe", Glyph::Globe), ("language", Glyph::Globe), // lint: not UI
    ("eye", Glyph::Eye), ("grid", Glyph::Grid), ("list", Glyph::List), ("monitor", Glyph::Monitor), // lint: not UI
    ("display", Glyph::Monitor), ("window", Glyph::Window), ("lock", Glyph::Lock), ("pin", Glyph::Pin), // lint: not UI
    ("info", Glyph::Info), ("cpu", Glyph::Cpu), ("memory", Glyph::Memory), ("disk", Glyph::Disk), // lint: not UI
    ("network", Glyph::Network), ("shield", Glyph::Shield), // lint: not UI
];

/// The glyph a descriptor's name asks for; None for a name the kit has no shape for (the icon column stays empty).
fn glyph_named(name: &str) -> Option<Glyph> {
    GLYPHS.iter().find(|g| g.0 == name).map(|g| g.1)
}

/// The unit's suffix after a value.
fn suffix(u: Unit) -> &'static str {
    match u {
        Unit::None => "",
        Unit::Px => " px", // lint: not UI
        Unit::Percent => "%",
        Unit::Ms => " ms",     // lint: not UI
        Unit::Seconds => " s", // lint: not UI
    }
}

/// The value a descriptor starts from: its default when the default has the control's type, else the control's own
/// first value.
fn initial(d: &Descriptor) -> PropValue {
    let v = (d.default)();
    match d.control {
        Control::Toggle => PropValue::Bool(v.as_bool().unwrap_or(false)),
        Control::Slider => {
            let min = d.range.map_or(0, |r| r.min);
            PropValue::Int(v.as_i64().unwrap_or(min))
        }
        Control::Segmented(opts) | Control::Select(opts) => {
            let first = opts.first().map_or("", |o| o.1);
            PropValue::Text(v.as_str().unwrap_or(first).to_string())
        }
        _ => PropValue::Text(v.as_str().unwrap_or("").to_string()),
    }
}

fn make_ctl(id: WidgetId, d: &Descriptor, value: &PropValue) -> Ctl {
    let label = text(d.label);
    match (d.control, value) {
        (Control::Toggle, PropValue::Bool(on)) => Ctl::Toggle(Toggle::new(id, label, *on)),
        (Control::Segmented(opts), PropValue::Text(cur)) => {
            let selected = opts.iter().position(|o| o.1 == cur);
            let labels = opts.iter().map(|o| text(o.0)).collect();
            Ctl::Choice { w: Segmented::new(id, label, labels, selected), values: opts.iter().map(|o| o.1).collect() }
        }
        (Control::Slider, PropValue::Int(v)) => {
            let range = d.range.unwrap_or(Range { min: 0, max: 100, step: 1, unit: Unit::None });
            let to32 = |n: i64| i32::try_from(n).unwrap_or(if n < 0 { i32::MIN } else { i32::MAX });
            let mut w = Slider::new(id, label, to32(range.min), to32(range.max), to32(range.step.max(1)), to32(*v));
            w.set_value_text(Some(Text::from(format!("{}{}", w.value(), suffix(range.unit))))); // lint: not UI
            Ctl::Slider { w, range }
        }
        (Control::Select(opts), PropValue::Text(cur)) => {
            let shown = opts.iter().find(|o| o.1 == cur).map_or(Text::Borrowed(""), |o| text(o.0));
            Ctl::Select {
                w: Button::new(id, shown, ButtonKind::Secondary),
                values: opts.iter().map(|o| o.1).collect(),
                open: None,
            }
        }
        (Control::Text { max }, PropValue::Text(_)) => {
            let mut w = TextField::new(id, FieldKind::Plain);
            w.set_max_chars(u32::from(max));
            w.set_label(label);
            Ctl::Field(w)
        }
        _ => Ctl::Fixed,
    }
}

impl PropertySheet {
    /// A sheet of \`descriptors\`, in the order of their \`order\` (the given order among equals), a header before each
    /// section. Row i has the widget id \`id + 1 + i\`.
    pub fn new(id: WidgetId, descriptors: &[Descriptor]) -> PropertySheet {
        let mut sorted: Vec<Descriptor> = descriptors.to_vec();
        sorted.sort_by_key(|d| d.order);
        let mut items = Vec::new();
        let mut last: Option<SecId> = None;
        for (i, d) in sorted.iter().enumerate() {
            if last != Some(d.section) {
                items.push(Item::Header { sec: d.section, shown: true, top: 0, height: 0 });
                last = Some(d.section);
            }
            let value = initial(d);
            let ctl = make_ctl(WidgetId(id.0 + 1 + i as u32), d, &value);
            items.push(Item::Row(Row {
                d: *d,
                ctl,
                value,
                glyph: glyph_named(d.glyph),
                shown: true,
                top: 0,
                height: 0,
                line: RectP::EMPTY,
                help_at: PointP { x: 0, y: 0 },
                help: None,
                label_room: RectP::EMPTY,
                ctl_w: 0,
            }));
        }
        PropertySheet { id, items, titles: Vec::new(), rect: RectP::EMPTY }
    }

    /// The headings of the sections (a section without one has no header, only its gap).
    pub fn set_section_titles(&mut self, titles: Vec<(SecId, Text)>) {
        self.titles = titles;
    }

    /// The sheet's node (slot 0); each row's help is slot 1 + its index, a Select row's label slot 1 + the item count + its index.
    pub fn node_id(&self) -> NodeId {
        NodeId::new(self.id, 0)
    }

    fn rows(&self) -> impl Iterator<Item = &Row> {
        self.items.iter().filter_map(|it| if let Item::Row(r) = it { Some(r) } else { None })
    }

    /// The value of the row of that JSON key.
    pub fn value(&self, json: &str) -> Option<&PropValue> {
        self.rows().find(|r| r.d.json == json).map(|r| &r.value)
    }

    /// Puts a value into the row of that JSON key (and into its control); a value of the wrong type for the control is
    /// ignored. Rows that depend on it (visible_when) appear or go at the next layout.
    pub fn set_value(&mut self, cx: &mut EventCx, json: &str, v: PropValue) {
        let Some(row) = self.items.iter_mut().find_map(|it| match it {
            Item::Row(r) if r.d.json == json => Some(r),
            _ => None,
        }) else {
            return;
        };
        let ok = match (&mut row.ctl, &v) {
            (Ctl::Toggle(w), PropValue::Bool(b)) => {
                w.set_on(*b);
                true
            }
            (Ctl::Choice { w, values }, PropValue::Text(s)) => {
                w.set_selected(values.iter().position(|x| x == s));
                true
            }
            (Ctl::Slider { w, range }, PropValue::Int(n)) => {
                w.set_value(i32::try_from(*n).unwrap_or(0));
                w.set_value_text(Some(Text::from(format!("{}{}", w.value(), suffix(range.unit)))));
                true
            }
            (Ctl::Select { w, values, .. }, PropValue::Text(s)) => {
                if let (Some(i), Control::Select(opts)) = (values.iter().position(|x| x == s), row.d.control) {
                    w.set_label(text(opts[i].0));
                }
                true
            }
            (Ctl::Field(w), PropValue::Text(s)) => {
                w.set_text(cx, s, false);
                true
            }
            _ => false,
        };
        if ok {
            row.value = v;
            cx.relayout();
        }
    }

    /// True when the row shows: its visible_when condition holds on the current values.
    fn condition_holds(&self, c: Option<Cond>) -> bool {
        let Some(c) = c else { return true };
        match self.value(c.json) {
            Some(PropValue::Bool(b)) => (if *b { "true" } else { "false" }) == c.equals, // lint: not UI
            Some(PropValue::Int(n)) => n.to_string() == c.equals,
            Some(PropValue::Text(s)) => s == c.equals,
            None => true,
        }
    }

    /// Places every row for `width` (physical px) and returns the height of the sheet.
    fn measure(&mut self, cx: &mut LayoutCx, width: i32) -> i32 {
        let sz = cx.tokens().size.clone();
        let (comfort, ctl_h, toggle_w) = (cx.px(sz.row_comfort), cx.px(sz.control_h), cx.px(sz.toggle_w));
        let (label_x, right, gap, pad) = (cx.px(LABEL_X), cx.px(RIGHT), cx.px(GAP), cx.px(PAD));
        let (section_h, section_gap, two) = (cx.px(SECTION_H), cx.px(SECTION_GAP), cx.px(2));
        let caption = cx.font(TypeRole::Caption);
        let shown: Vec<bool> = self
            .items
            .iter()
            .map(|it| match it {
                Item::Row(r) => self.condition_holds(r.d.visible_when),
                Item::Header { .. } => true,
            })
            .collect();
        let titled: Vec<bool> = self
            .items
            .iter()
            .map(|it| matches!(it, Item::Header { sec, .. } if self.titles.iter().any(|t| t.0 == *sec)))
            .collect();
        let mut y = 0;
        let mut header: Option<usize> = None;
        let mut placed: Vec<(usize, i32, i32)> = Vec::new();
        for (n, it) in self.items.iter_mut().enumerate() {
            let Item::Row(r) = it else {
                header = Some(n);
                continue;
            };
            r.shown = shown[n];
            if !r.shown {
                continue;
            }
            if let Some(h) = header.take() {
                let height = if titled[h] {
                    section_h
                } else if y > 0 {
                    section_gap
                } else {
                    0
                };
                placed.push((h, y, height));
                y += height;
            }
            r.ctl_w = match &mut r.ctl {
                Ctl::Toggle(_) => toggle_w,
                Ctl::Choice { w, .. } => w.preferred(cx).w,
                Ctl::Slider { w, .. } => w.preferred(cx).w,
                Ctl::Select { w, .. } => w.preferred(cx).w,
                Ctl::Field(_) => cx.px(FIELD_W),
                Ctl::Fixed => 0,
            };
            let room = (width - label_x - right - r.ctl_w - gap).max(0);
            let help = (r.d.help)();
            r.help = (!help.is_empty()).then(|| {
                let opts = LayoutOpts { max_width: Some(room), trim: Trim::End, wrap: true };
                cx.text().layout(help, caption, &opts)
            });
            let (top_pad, height) = match &r.help {
                None => ((comfort - ctl_h) / 2, comfort),
                Some(h) => (pad, comfort.max(pad + ctl_h - two + h.height + pad)),
            };
            r.top = y;
            r.height = height;
            r.line = RectP { l: label_x, t: top_pad, r: width - right, b: top_pad + ctl_h };
            r.label_room = RectP { l: label_x, t: r.line.t, r: label_x + room, b: r.line.b };
            r.help_at = PointP { x: label_x, y: r.line.b - two };
            y += height;
        }
        for it in &mut self.items {
            if let Item::Header { shown, .. } = it {
                *shown = false;
            }
        }
        for (h, top, height) in placed {
            if let Item::Header { shown, top: t, height: ht, .. } = &mut self.items[h] {
                (*shown, *t, *ht) = (true, top, height);
            }
        }
        y
    }

    /// Opens the list of a Select row.
    fn open_select(row: &mut Row, cx: &mut EventCx, rect: RectP) {
        let Control::Select(opts) = row.d.control else { return };
        let Ctl::Select { values, open, .. } = &mut row.ctl else { return };
        let items = opts.iter().map(|o| text(o.0)).collect();
        let current = match &row.value {
            PropValue::Text(s) => values.iter().position(|v| v == s),
            _ => None,
        };
        *open = Some(cx.open_list(items, Anchor::Below(rect), current));
    }

    /// The row's value after the user picked option `index` of its Select list.
    fn choose(row: &mut Row, index: usize) -> Option<PropOut> {
        let Ctl::Select { values, w, .. } = &mut row.ctl else { return None };
        let value = (*values.get(index)?).to_string();
        if let Control::Select(opts) = row.d.control {
            w.set_label(text(opts[index].0));
        }
        row.value = PropValue::Text(value);
        Some((row.d.json, row.value.clone()))
    }
}

impl Widget for PropertySheet {
    type Out = PropOut;

    fn layout(&mut self, cx: &mut LayoutCx, rect: RectP) {
        self.rect = rect;
        let height = self.measure(cx, rect.w());
        cx.push(Node::new(self.node_id(), Role::Group, RectP { b: rect.t + height, ..rect }));
        let two = cx.px(2);
        let count = self.items.len() as u32;
        for (n, it) in self.items.iter_mut().enumerate() {
            let Item::Row(r) = it else { continue };
            if !r.shown {
                continue;
            }
            let line = r.line.offset(rect.l, rect.t + r.top);
            let ctl = RectP { l: line.r - r.ctl_w, ..line };
            match &mut r.ctl {
                Ctl::Toggle(w) => w.layout(cx, line),
                Ctl::Choice { w, .. } => w.layout(cx, ctl),
                Ctl::Slider { w, .. } => w.layout(cx, ctl),
                Ctl::Select { w, .. } => {
                    w.layout(cx, ctl);
                    // The button is named by its value; the row's label is drawn beside it, so it is a node too.
                    let room = r.label_room.offset(rect.l, rect.t + r.top);
                    cx.push(
                        Node::new(NodeId::new(self.id, 1 + count + n as u32), Role::Label, room).label(text(r.d.label)),
                    );
                }
                Ctl::Field(w) => w.layout(cx, ctl),
                Ctl::Fixed => {
                    let node = Node::new(NodeId::new(self.id, 1 + n as u32), Role::Label, line)
                        .flags(NodeFlags::DISABLED)
                        .label(text(r.d.label))
                        .description(text(r.d.help));
                    cx.push(node);
                    continue;
                }
            }
            if let Some(h) = &r.help {
                let at = RectP::wh(rect.l + r.help_at.x, rect.t + r.top + r.help_at.y, h.width, h.height + two);
                cx.push(Node::new(NodeId::new(self.id, 1 + n as u32), Role::Label, at).label(text(r.d.help)));
            }
        }
    }

    fn paint(&self, cx: &mut PaintCx) {
        let c = cx.tokens().color.clone();
        let (origin, width) = (self.rect, self.rect.w());
        let icon = cx.px(ICON);
        for it in &self.items {
            match it {
                Item::Header { sec, shown: true, top, height } if *height > 0 => {
                    let Some((_, title)) = self.titles.iter().find(|t| t.0 == *sec) else { continue };
                    let r = RectP {
                        l: origin.l + cx.px(LEFT),
                        t: origin.t + top,
                        r: origin.l + width - cx.px(RIGHT),
                        b: origin.t + top + height,
                    };
                    cx.text_in(title, TypeRole::Section, r, Align::Start, Trim::End, c.fg_secondary);
                }
                Item::Header { .. } => {}
                Item::Row(r) if r.shown => {
                    let rect = RectP::wh(origin.l, origin.t + r.top, width, r.height);
                    if !cx.visible(rect) {
                        continue;
                    }
                    let dead = matches!(r.ctl, Ctl::Fixed);
                    let (fg, soft) = if dead { (c.fg_disabled, c.fg_disabled) } else { (c.fg_primary, c.fg_secondary) };
                    let line = r.line.offset(origin.l, origin.t + r.top);
                    if let Some(g) = r.glyph {
                        let at = RectP::wh(origin.l + cx.px(LEFT), (line.t + line.b) / 2 - icon / 2, icon, icon);
                        cx.glyph(at, g, soft);
                    }
                    if !matches!(r.ctl, Ctl::Toggle(_)) {
                        let room = r.label_room.offset(origin.l, origin.t + r.top);
                        cx.text_in(&text(r.d.label), TypeRole::Body, room, Align::Start, Trim::End, fg);
                    }
                    if let Some(h) = &r.help {
                        let at = PointP { x: origin.l + r.help_at.x, y: origin.t + r.top + r.help_at.y };
                        cx.text(h, at, soft);
                    }
                    match &r.ctl {
                        Ctl::Toggle(w) => w.paint(cx),
                        Ctl::Choice { w, .. } => w.paint(cx),
                        Ctl::Slider { w, .. } => w.paint(cx),
                        Ctl::Select { w, .. } => w.paint(cx),
                        Ctl::Field(w) => w.paint(cx),
                        Ctl::Fixed => {}
                    }
                }
                Item::Row(_) => {}
            }
        }
    }

    fn event(&mut self, cx: &mut EventCx, ev: &Event) -> Option<PropOut> {
        let (left, top) = (self.rect.l, self.rect.t);
        let mut out = None;
        let mut changed = false;
        for it in &mut self.items {
            let Item::Row(r) = it else { continue };
            if !r.shown {
                continue;
            }
            let json = r.d.json;
            let line = r.line.offset(left, top + r.top);
            let found: Option<PropValue> = match &mut r.ctl {
                Ctl::Toggle(w) => w.event(cx, ev).map(|ToggleOut::Changed(b)| PropValue::Bool(b)),
                Ctl::Choice { w, values } => match w.event(cx, ev) {
                    Some(SegOut::Selected(i)) => values.get(i).map(|v| PropValue::Text((*v).to_string())),
                    None => None,
                },
                Ctl::Slider { w, range } => match w.event(cx, ev) {
                    Some(SliderOut::Changed(v)) => {
                        w.set_value_text(Some(Text::from(format!("{v}{}", suffix(range.unit))))); // lint: not UI
                        None
                    }
                    Some(SliderOut::Committed(v)) => {
                        w.set_value_text(Some(Text::from(format!("{v}{}", suffix(range.unit))))); // lint: not UI
                        Some(PropValue::Int(i64::from(v)))
                    }
                    None => None,
                },
                Ctl::Field(w) => match w.event(cx, ev) {
                    Some(FieldOut::Committed(s)) => Some(PropValue::Text(s)),
                    _ => None,
                },
                Ctl::Select { w, open, .. } => {
                    let clicked = w.event(cx, ev) == Some(ButtonOut::Clicked);
                    match ev {
                        Event::MenuClosed(m) if *open == Some(*m) => *open = None,
                        _ => {}
                    }
                    let picked = match ev {
                        Event::ListPicked { menu, index } if *open == Some(*menu) => Some(*index),
                        _ => None,
                    };
                    if clicked {
                        let rect = RectP { l: line.r - r.ctl_w, ..line };
                        Self::open_select(r, cx, rect);
                    }
                    if let Some(i) = picked {
                        if let Some((_, v)) = Self::choose(r, i) {
                            if let Ctl::Select { open, .. } = &mut r.ctl {
                                *open = None;
                            }
                            out = Some((json, v));
                            changed = true;
                        }
                    }
                    None
                }
                Ctl::Fixed => None,
            };
            if let Some(v) = found {
                r.value = v.clone();
                out = Some((json, v));
                changed = true;
            }
            if out.is_some() {
                break;
            }
        }
        if changed {
            // A row that depends on this value may appear or go.
            cx.relayout();
        }
        out
    }

    fn preferred(&mut self, cx: &mut LayoutCx) -> SizeP {
        let w = cx.px(480);
        SizeP { w, h: self.measure(cx, w) }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::geom::{PointL, SizeL};
    use crate::harness::Harness;
    use crate::theme::Tokens;
    use crate::widgets::paint::Host;
    use oled_config::desc::{PageId, Scope, ValueType};

    const OPTS: &[(TextFn, &str)] = &[(|| "Small", "s"), (|| "Large", "l")];
    const MODES: &[(TextFn, &str)] = &[(|| "Mode A", "a"), (|| "Mode B", "b"), (|| "Mode C", "c")];

    fn desc(json: &'static str, order: u16, control: Control, label: TextFn, help: TextFn) -> Descriptor {
        Descriptor {
            path: json,
            json,
            kind: ValueType::Bool,
            default: || Default::default(),
            range: None,
            label,
            help,
            glyph: "",
            page: PageId("test"),
            section: SecId("main"),
            order,
            keywords: &[],
            scope: Scope::Global,
            profileable: false,
            visible_when: None,
            cost: None,
            control,
            since: 0,
            takeover: None,
            reason: "test",
        }
    }

    /// One descriptor of each control kind, in a scrambled order.
    fn descs() -> Vec<Descriptor> {
        let mut slider = desc("ui.opacity", 3, Control::Slider, || "Opacity", || "");
        slider.range = Some(Range { min: 0, max: 100, step: 5, unit: Unit::Percent });
        let mut dependent = desc("ui.extra", 7, Control::Toggle, || "Extra", || "");
        dependent.visible_when = Some(Cond { json: "ui.icons", equals: "true" });
        let mut icons = desc(
            "ui.icons",
            1,
            Control::Toggle,
            || "Show icons",
            || "Draws a label under each icon when there is room for it.",
        );
        icons.glyph = "eye";
        vec![
            dependent,
            desc("ui.colors", 6, Control::Swatches, || "Colours", || "Picked in the dock."),
            desc("ui.mode", 5, Control::Select(MODES), || "Mode", || ""),
            slider,
            desc("ui.size", 2, Control::Segmented(OPTS), || "Size", || ""),
            desc("ui.name", 4, Control::Text { max: 20 }, || "Name", || ""),
            icons,
        ]
    }

    const SIZE: SizeL = SizeL { w: 480, h: 420 };

    fn harness(dpi: u32) -> Harness<Host<PropertySheet>> {
        let sheet = PropertySheet::new(WidgetId(1), &descs());
        Harness::new(Host::new(sheet, (0, 0, 480, 420)), SIZE, dpi, Tokens::oled())
    }

    #[test]
    fn rows_come_from_descriptors_only() {
        let h = harness(96);
        let dump = h.a11y_dump();
        for label in ["Show icons", "Size", "Opacity", "Mode", "Colours"] {
            assert!(dump.contains(&format!("\"{label}\"")), "{label}: {dump}");
        }
        assert!(dump.contains("Draws a label under each icon"), "the help is the descriptor's: {dump}");
        assert!(dump.contains("Switch") && dump.contains("RadioGroup") && dump.contains("Slider"), "{dump}");
        assert!(!dump.contains("Extra"), "a row whose condition does not hold is not there: {dump}");
        h.assert_named();
        // No descriptors, no rows: the sheet is its group node and nothing else.
        let empty =
            Harness::new(Host::new(PropertySheet::new(WidgetId(1), &[]), (0, 0, 480, 100)), SIZE, 96, Tokens::oled());
        assert_eq!(empty.a11y_dump().matches("Switch").count() + empty.a11y_dump().matches("Slider").count(), 0);
        assert!(empty.screen().w.value("ui.icons").is_none());
    }

    #[test]
    fn rows_follow_the_order_field_and_start_from_the_default() {
        let h = harness(96);
        let sheet = &h.screen().w;
        let order: Vec<&str> = sheet.rows().map(|r| r.d.json).collect();
        assert_eq!(order, ["ui.icons", "ui.size", "ui.opacity", "ui.name", "ui.mode", "ui.colors", "ui.extra"]);
        assert_eq!(
            sheet.value("ui.icons"),
            Some(&PropValue::Bool(false)),
            "a default of another type gives the control's own start"
        );
        assert_eq!(sheet.value("ui.opacity"), Some(&PropValue::Int(0)));
        assert_eq!(sheet.value("ui.size"), Some(&PropValue::Text("s".to_string())));
    }

    #[test]
    fn one_line_rows_are_comfortable_and_described_rows_grow() {
        for dpi in [96, 144] {
            let mut h = harness(dpi);
            let _ = h.render();
            let sheet = &h.screen().w;
            let heights: Vec<(&str, i32)> = sheet.rows().filter(|r| r.shown).map(|r| (r.d.json, r.height)).collect();
            let comfort = Scale { dpi }.px(44);
            for (json, height) in &heights {
                if *json == "ui.icons" || *json == "ui.colors" {
                    assert!(*height > comfort, "{json} has help: {height}");
                } else {
                    assert_eq!(*height, comfort, "{json} at {dpi} dpi");
                }
            }
        }
    }

    #[test]
    fn the_toggle_reports_its_key_and_value_and_shows_what_depends_on_it() {
        let mut h = harness(96);
        h.click(PointL { x: 480 - 16 - 16, y: 22 });
        assert_eq!(h.screen().outs, [("ui.icons", PropValue::Bool(true))]);
        let dump = h.a11y_dump();
        assert!(dump.contains("Extra"), "the dependent row appeared: {dump}");
        h.click(PointL { x: 480 - 16 - 16, y: 22 });
        assert_eq!(h.screen().outs.last(), Some(&("ui.icons", PropValue::Bool(false))));
        assert!(!h.a11y_dump().contains("Extra"));
    }

    #[test]
    fn the_slider_reports_on_commit_in_steps() {
        let mut h = harness(96);
        for _ in 0..8 {
            if h.focused().map(|n| n.owner().0) == Some(4) {
                break;
            }
            h.key("Tab");
        }
        assert_eq!(h.focused().map(|n| n.owner().0), Some(4), "the slider is the third row");
        h.key("Right");
        assert_eq!(h.screen().outs, [("ui.opacity", PropValue::Int(5))]);
        assert_eq!(h.screen().w.value("ui.opacity"), Some(&PropValue::Int(5)));
    }

    #[test]
    fn a_choice_reports_its_value_string() {
        let mut h = harness(96);
        let _ = h.render();
        // The segmented control of row 2 is right-aligned: click its second segment.
        let sheet = &h.screen().w;
        let row = sheet.rows().find(|r| r.d.json == "ui.size").map(|r| (r.top, r.line, r.ctl_w)).unwrap();
        let (top, line, w) = row;
        let at = PointL { x: line.r - w / 4, y: top + (line.t + line.b) / 2 };
        h.click(at);
        assert_eq!(h.screen().outs, [("ui.size", PropValue::Text("l".to_string()))]);
    }

    #[test]
    fn a_select_opens_its_options_and_the_pick_is_the_value() {
        let mut h = harness(96);
        let _ = h.render();
        let (top, line, w) =
            h.screen().w.rows().find(|r| r.d.json == "ui.mode").map(|r| (r.top, r.line, r.ctl_w)).unwrap();
        h.click(PointL { x: line.r - w / 2, y: top + (line.t + line.b) / 2 });
        assert_eq!(h.menus().len(), 1, "the list opened");
        assert_eq!(h.menus()[0].items.len(), 3);
        let sheet = &mut h.screen_mut().w;
        let row = sheet.items.iter_mut().find_map(|it| match it {
            Item::Row(r) if r.d.json == "ui.mode" => Some(r),
            _ => None,
        });
        let out = PropertySheet::choose(row.unwrap(), 2);
        assert_eq!(out, Some(("ui.mode", PropValue::Text("c".to_string()))));
        assert_eq!(sheet.value("ui.mode"), Some(&PropValue::Text("c".to_string())));
        assert!(PropertySheet::choose(
            sheet
                .items
                .iter_mut()
                .find_map(|it| match it {
                    Item::Row(r) => Some(r),
                    _ => None,
                })
                .unwrap(),
            0
        )
        .is_none());
    }

    #[test]
    fn the_dock_controls_are_disabled_rows_with_the_descriptors_label() {
        let h = harness(96);
        let dump = h.a11y_dump();
        let line = dump.lines().find(|l| l.contains("Colours")).unwrap_or_default();
        assert!(line.contains("disabled"), "{line}");
        let sheet = &h.screen().w;
        assert!(sheet.rows().any(|r| matches!(r.ctl, Ctl::Fixed)));
    }

    #[test]
    fn section_headings_come_from_the_titles_given() {
        let mut h = harness(96);
        let plain = h.render().text_rects.len();
        h.screen_mut().w.set_section_titles(vec![(SecId("main"), Text::from("General"))]);
        let titled = h.set_tokens(Tokens::oled()).render().text_rects.len();
        assert!(titled > plain, "{plain} -> {titled}");
    }

    /// A sheet in a screen that takes (key, value) as a data message.
    struct Rig {
        sheet: PropertySheet,
        cmds: crate::command::CommandTable,
    }

    impl crate::screen::Screen for Rig {
        fn layout(&mut self, cx: &mut LayoutCx, avail: SizeP) -> SizeP {
            self.sheet.layout(cx, RectP::wh(0, 0, avail.w, avail.h));
            avail
        }

        fn paint(&self, cx: &mut PaintCx) {
            self.sheet.paint(cx);
        }

        fn event(&mut self, cx: &mut EventCx, ev: &Event) -> crate::event::Reply {
            self.sheet.event(cx, ev);
            crate::event::Reply::None
        }

        fn data(&mut self, cx: &mut EventCx, msg: crate::host::AppMsg) -> crate::event::Reply {
            if let Ok(m) = msg.downcast::<(&'static str, PropValue)>() {
                self.sheet.set_value(cx, m.0, m.1);
            }
            crate::event::Reply::None
        }

        fn commands(&self) -> &crate::command::CommandTable {
            &self.cmds
        }
    }

    #[test]
    fn set_value_moves_the_control_and_ignores_the_wrong_type() {
        let rig = Rig { sheet: PropertySheet::new(WidgetId(1), &descs()), cmds: crate::command::CommandTable::new() };
        let mut h = Harness::new(rig, SIZE, 96, Tokens::oled());
        h.send(Box::new(("ui.opacity", PropValue::Int(40))));
        assert_eq!(h.screen().sheet.value("ui.opacity"), Some(&PropValue::Int(40)));
        assert!(h.a11y_dump().contains("Slider \"Opacity\" [focusable] 40"), "{}", h.a11y_dump());
        h.send(Box::new(("ui.opacity", PropValue::Bool(true))));
        h.send(Box::new(("ui.icons", PropValue::Int(3))));
        h.send(Box::new(("no.such.key", PropValue::Bool(true))));
        assert_eq!(h.screen().sheet.value("ui.opacity"), Some(&PropValue::Int(40)));
        assert_eq!(h.screen().sheet.value("ui.icons"), Some(&PropValue::Bool(false)));
        h.send(Box::new(("ui.size", PropValue::Text("l".to_string()))));
        assert!(h.a11y_dump().contains("Radio \"Large\" [focusable, selected]"), "{}", h.a11y_dump());
        h.send(Box::new(("ui.icons", PropValue::Bool(true))));
        assert!(h.a11y_dump().contains("Extra"), "a value set from outside shows what depends on it");
    }

    use crate::gfx::scale::Scale;
}
