//! PathBar (F-K3, P2-kit 2.10; K3.3): the breadcrumb of the files toolbar (SPEC-files 5.2.3). At rest it is only text:
//! ancestors in FG_SECONDARY, the current folder in FG_PRIMARY BodyStrong, 12 px chevrons between the segments with 16 px
//! hit targets (a chevron asks the app for that level's subfolders), a Refresh glyph at the right end. When space runs
//! short the leading segments fold into a "..." segment, but the root and the last two always stay. A click on empty
//! space, or the app's Ctrl+L, Alt+D and F4 (it calls `begin_edit`), turn the line into a Path TextField with the whole
//! path selected. One Tab stop with roving Left/Right/Home/End over the segments and Refresh.

use super::paint::{lerp_ink, text_centered};
use super::text_field::{FieldKind, FieldOut, TextField};
use crate::cx::{Anchor, EventCx, LayoutCx, PaintCx};
use crate::event::{A11yAction, Event, Key, MouseButton};
use crate::geom::RectP;
use crate::glyph::Glyph;
use crate::model::Align;
use crate::node::{Actions, Focusable, Node, NodeFlags, NodeId, Role, Text, WidgetId};
use crate::screen::Widget;
use crate::strings::k;
use crate::text::Trim;
use crate::theme::{Ink, TypeRole};

/// One segment of a path.
pub struct PathSegment {
    pub key: u64,
    pub label: Text,
    pub tip: Option<Text>,
}

/// What a path bar tells its screen.
#[derive(Clone, Debug, PartialEq)]
#[non_exhaustive]
pub enum PathOut {
    Navigate(u64),
    SiblingsRequested(u64, Anchor),
    EditStarted,
    EditCommitted(String),
    EditCancelled,
    CompletionRequested(String),
    Refresh,
}

/// The line's left padding, a segment's padding either side of its label, the chevron's hit box and glyph, the Refresh
/// box and its glyph, the gap before it (logical px).
const LEFT_PAD: i32 = 4;
const SEG_PAD: i32 = 6;
const CHEV_HIT: i32 = 16;
const CHEV_GLYPH: i32 = 12;
const REFRESH: i32 = 24;
const REFRESH_GLYPH: i32 = 16;
const GAP: i32 = 4;
/// The gap between the line and the reason under it, and the room the reason takes (logical px).
const REASON_GAP: i32 = 2;
/// The segment text that stands for the folded levels.
const ELLIPSIS: &str = "\u{2026}"; // lint: not UI
/// The id bit of the edit field: the bar owns two widget ids, its own and this one.
const FIELD_ID_BIT: u32 = 0x4000_0000;

/// Node slots: the group, the segments from 1, the fold, the chevrons, Refresh.
const FOLD_SLOT: u32 = 0x1000;
const CHEV_BASE: u32 = 0x2000;
const REFRESH_SLOT: u32 = 0x3000;

/// How many leading segments fold (all but the root and the last two) so that the rest fits `avail`. `widths` are the
/// segments' widths with their chevrons, `ellipsis` the width of the "..." segment. Pure.
pub(crate) fn fold_count(widths: &[i32], avail: i32, ellipsis: i32) -> usize {
    let n = widths.len();
    if n <= 3 || widths.iter().sum::<i32>() <= avail {
        return 0;
    }
    let max = n - 3;
    for f in 1..=max {
        let shown = widths[0] + ellipsis + widths[1 + f..].iter().sum::<i32>();
        if shown <= avail {
            return f;
        }
    }
    max
}

/// What a node of the bar is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Part {
    Group,
    Seg(usize),
    Fold,
    Chev(usize),
    Refresh,
}

/// One shown item: the segment it is (None: the fold), its plate and its chevron box (physical px).
#[derive(Clone, Copy, Debug)]
struct Item {
    seg: Option<usize>,
    plate: RectP,
    chev: RectP,
}

/// The path bar: segments, or an edit field.
pub struct PathBar {
    id: WidgetId,
    segs: Vec<PathSegment>,
    edit_text: String,
    completions: Vec<Text>,
    offline: bool,
    error: Option<Text>,
    field: TextField,
    editing: bool,
    /// The line, the shown items in order, how many leading segments are folded and the Refresh box, from the last layout.
    line: RectP,
    items: Vec<Item>,
    folded: usize,
    refresh: RectP,
}

impl PathBar {
    pub fn new(id: WidgetId) -> PathBar {
        let field = TextField::new(WidgetId(id.0 | FIELD_ID_BIT), FieldKind::Path);
        PathBar {
            id,
            segs: Vec::new(),
            edit_text: String::new(),
            completions: Vec::new(),
            offline: false,
            error: None,
            field,
            editing: false,
            line: RectP::EMPTY,
            items: Vec::new(),
            folded: 0,
            refresh: RectP::EMPTY,
        }
    }

    /// The bar with its path from the start (a screen builds its widgets before it has an EventCx).
    pub fn with_path(mut self, segs: Vec<PathSegment>, edit_text: String) -> PathBar {
        self.segs = segs;
        self.edit_text = edit_text;
        self
    }

    /// The segments, and the text the edit mode starts with.
    pub fn set_path(&mut self, cx: &mut EventCx, segs: Vec<PathSegment>, edit_text: String) {
        self.segs = segs;
        self.edit_text = edit_text;
        cx.relayout();
    }

    /// Switches to the edit field with the whole path selected (the app's Ctrl+L, Alt+D and F4).
    pub fn begin_edit(&mut self, cx: &mut EventCx) {
        if self.editing {
            return;
        }
        self.editing = true;
        let text = self.edit_text.clone();
        self.field.set_text(cx, &text, true);
        cx.set_focus(self.field.node_id());
        cx.relayout();
    }

    /// For galleries and goldens: the bar in edit mode showing `text` selected, painted as focused.
    pub fn preview_edit(&mut self, text: &str, error: Option<Text>) {
        self.editing = true;
        self.field.preview(text, true, Some(0..text.len()), None, error);
    }

    /// For galleries and goldens: the bar showing an error and its reason without a window around it.
    pub fn preview_error(&mut self, msg: Option<Text>) {
        self.error = msg;
    }

    /// True while the bar shows the edit field.
    pub fn is_editing(&self) -> bool {
        self.editing
    }

    /// Path completions: at most 8 rows under the field while it is open.
    pub fn set_completions(&mut self, cx: &mut EventCx, items: Vec<Text>) {
        self.completions = items.clone();
        if self.editing {
            self.field.set_completions(cx, items);
        }
    }

    pub fn set_offline(&mut self, on: bool) {
        self.offline = on;
    }

    pub fn set_error(&mut self, cx: &mut EventCx, msg: Option<Text>) {
        self.error = msg;
        cx.relayout();
    }

    // ---- state

    fn node(&self, slot: u32) -> NodeId {
        NodeId::new(self.id, slot)
    }

    fn item_node(&self, it: &Item) -> NodeId {
        match it.seg {
            Some(i) => self.node(1 + i as u32),
            None => self.node(FOLD_SLOT),
        }
    }

    fn part(&self, n: NodeId) -> Option<Part> {
        if n.owner() != self.id {
            return None;
        }
        let count = self.segs.len() as u32;
        match n.slot() {
            0 => Some(Part::Group),
            FOLD_SLOT => Some(Part::Fold),
            REFRESH_SLOT => Some(Part::Refresh),
            s if s >= CHEV_BASE && s - CHEV_BASE < count => Some(Part::Chev((s - CHEV_BASE) as usize)),
            s if s >= 1 && s - 1 < count => Some(Part::Seg((s - 1) as usize)),
            _ => None,
        }
    }

    /// The key a click on the fold asks the app about: the last folded level.
    fn fold_key(&self) -> Option<u64> {
        if self.folded == 0 {
            return None;
        }
        self.segs.get(self.folded).map(|s| s.key)
    }

    /// The item whose node is `n`.
    fn item_at(&self, part: Part) -> Option<&Item> {
        match part {
            Part::Seg(i) => self.items.iter().find(|it| it.seg == Some(i)),
            Part::Fold => self.items.iter().find(|it| it.seg.is_none()),
            _ => None,
        }
    }

    fn siblings(&self, part: Part) -> Option<PathOut> {
        match part {
            Part::Seg(i) | Part::Chev(i) => {
                let it = self.items.iter().find(|it| it.seg == Some(i))?;
                Some(PathOut::SiblingsRequested(self.segs.get(i)?.key, Anchor::Below(it.chev)))
            }
            Part::Fold => {
                let it = self.item_at(part)?;
                Some(PathOut::SiblingsRequested(self.fold_key()?, Anchor::Below(it.plate)))
            }
            _ => None,
        }
    }

    /// The focus order: the shown items, then Refresh.
    fn order(&self) -> Vec<NodeId> {
        let mut v: Vec<NodeId> = self.items.iter().map(|it| self.item_node(it)).collect();
        v.push(self.node(REFRESH_SLOT));
        v
    }

    fn rove(&mut self, cx: &mut EventCx, from: NodeId, key: Key) -> bool {
        let order = self.order();
        let Some(at) = order.iter().position(|n| *n == from) else { return false };
        let to = match key {
            Key::LEFT => at.saturating_sub(1),
            Key::RIGHT => (at + 1).min(order.len() - 1),
            Key::HOME => 0,
            Key::END => order.len() - 1,
            _ => return false,
        };
        cx.consume();
        cx.set_focus(order[to]);
        true
    }

    /// Back to the breadcrumb: the field lets go, the focus returns to the current folder.
    fn end_edit(&mut self, cx: &mut EventCx, refocus: bool) {
        self.editing = false;
        self.field.release(cx);
        if let Some(last) = self.segs.len().checked_sub(1).filter(|_| refocus) {
            cx.set_focus(self.node(1 + last as u32));
        }
        cx.relayout();
    }

    fn activate(&mut self, part: Part) -> Option<PathOut> {
        match part {
            Part::Seg(i) => self.segs.get(i).map(|s| PathOut::Navigate(s.key)),
            Part::Fold | Part::Chev(_) => self.siblings(part),
            Part::Refresh => Some(PathOut::Refresh),
            Part::Group => None,
        }
    }

    fn edit_event(&mut self, cx: &mut EventCx, ev: &Event) -> Option<PathOut> {
        let out = self.field.event(cx, ev);
        let mine = cx.target() == Some(self.field.node_id());
        match out {
            Some(FieldOut::Committed(s)) => {
                self.end_edit(cx, true);
                Some(PathOut::EditCommitted(s))
            }
            Some(FieldOut::Cancelled) => {
                self.end_edit(cx, true);
                Some(PathOut::EditCancelled)
            }
            // Typing asks the app for completions of what is there now; so does Down.
            Some(FieldOut::Changed) => Some(PathOut::CompletionRequested(self.field.text().to_string())),
            Some(FieldOut::CompletionRequested(s)) => Some(PathOut::CompletionRequested(s)),
            // Esc on an empty field is left to the window by the field: here it still ends the edit.
            None if mine && !cx.consumed() && matches!(ev, Event::Key { key: Key::ESC, down: true, .. }) => {
                cx.consume();
                self.end_edit(cx, true);
                Some(PathOut::EditCancelled)
            }
            // Clicking away leaves the edit as Esc does.
            None if matches!(ev, Event::FocusOut) && mine => {
                self.end_edit(cx, false);
                Some(PathOut::EditCancelled)
            }
            _ => None,
        }
    }
}

impl Widget for PathBar {
    type Out = PathOut;

    fn layout(&mut self, cx: &mut LayoutCx, rect: RectP) {
        let h = cx.px(cx.tokens().size.control_h).min(rect.h());
        let top = rect.t + (rect.h() - h) / 2;
        self.line = RectP { t: top, b: top + h, ..rect };
        let group = self.node(0);
        cx.push(Node::new(group, Role::Toolbar, self.line).label(cx.lang().kit(k::KIT_PATH)));
        self.items.clear();
        self.folded = 0;
        if self.editing {
            self.field.set_label(cx.lang().kit(k::KIT_PATH));
            self.field.layout(cx, self.line);
            return;
        }
        let lang = cx.lang();
        let (pad, chev_w, gap) = (cx.px(SEG_PAD), cx.px(CHEV_HIT), cx.px(GAP));
        let side = cx.px(REFRESH);
        let n = self.segs.len();
        let last = n.saturating_sub(1);
        let mut widths = Vec::with_capacity(n);
        for (i, s) in self.segs.iter().enumerate() {
            let role = if i == last { TypeRole::BodyStrong } else { TypeRole::Body };
            let font = cx.font(role);
            widths.push(cx.text().width(&s.label, font) + 2 * pad + chev_w);
        }
        let font = cx.font(TypeRole::Body);
        let ell = cx.text().width(ELLIPSIS, font) + 2 * pad + chev_w;
        self.refresh = RectP::wh(self.line.r - side - cx.px(2), self.line.t + (h - side) / 2, side, side);
        let left = self.line.l + cx.px(LEFT_PAD);
        let avail = (self.refresh.l - gap - left).max(0);
        self.folded = fold_count(&widths, avail, ell);
        let mut x = left;
        let mut place = |seg: Option<usize>, w: i32, items: &mut Vec<Item>| {
            let plate = RectP { l: x, r: x + w - chev_w, t: self.line.t, b: self.line.b };
            let chev = RectP { l: x + w - chev_w, r: x + w, t: self.line.t, b: self.line.b };
            items.push(Item { seg, plate, chev });
            x += w;
        };
        for i in 0..n {
            if i == 0 {
                place(Some(0), widths[0], &mut self.items);
                if self.folded > 0 {
                    place(None, ell, &mut self.items);
                }
            } else if i > self.folded {
                place(Some(i), widths[i], &mut self.items);
            }
        }
        // The nodes: segments (the current folder is the Tab stop), the fold, the chevrons, Refresh.
        for it in &self.items {
            let node_id = self.item_node(it);
            let (label, tip, role) = match it.seg {
                Some(i) => (self.segs[i].label.clone(), self.segs[i].tip.clone(), Role::Link),
                None => (lang.kit(k::KIT_MORE), None, Role::Link),
            };
            let focus = if it.seg == Some(last) { Focusable::Tab } else { Focusable::Roving };
            let mut node = Node::new(node_id, role, it.plate)
                .parent(group)
                .flags(NodeFlags::HOVER_FADE)
                .focus(focus)
                .label(label)
                .actions(Actions::CLICK | Actions::FOCUS);
            if let Some(t) = tip {
                node = node.description(t);
            }
            cx.push(node);
            if let Some(i) = it.seg {
                let name = lang.kit(k::KIT_SUBFOLDERS);
                cx.push(
                    Node::new(self.node(CHEV_BASE + i as u32), Role::Button, it.chev)
                        .parent(group)
                        .flags(NodeFlags::HOVER_FADE)
                        .label(name.clone())
                        .description(name)
                        .actions(Actions::CLICK),
                );
            }
        }
        let name = lang.kit(k::KIT_REFRESH);
        cx.push(
            Node::new(self.node(REFRESH_SLOT), Role::Button, self.refresh)
                .parent(group)
                .flags(NodeFlags::HOVER_FADE)
                .focus(Focusable::Roving)
                .label(name.clone())
                .description(name)
                .actions(Actions::CLICK | Actions::FOCUS),
        );
    }

    fn paint(&self, cx: &mut PaintCx) {
        let c = cx.tokens().color.clone();
        let radius = cx.px(cx.tokens().radius.s);
        if self.editing {
            self.field.paint(cx);
        } else {
            let last = self.segs.len().saturating_sub(1);
            let ellipsis: Text = Text::Borrowed(ELLIPSIS);
            let fade = |cx: &PaintCx, id: NodeId| lerp_ink(Ink { a: 0, ..c.fill_hover }, c.fill_hover, cx.hover_t(id));
            for it in &self.items {
                let id = self.item_node(it);
                let plate = if cx.pressed(id) { c.fill_press } else { fade(cx, id) };
                cx.fill(it.plate, radius, plate);
                let hot = cx.hover_t(id);
                let (text, role, ink) = match it.seg {
                    Some(i) if i == last => (&self.segs[i].label, TypeRole::BodyStrong, c.fg_primary),
                    Some(i) => (&self.segs[i].label, TypeRole::Body, lerp_ink(c.fg_secondary, c.fg_secondary_hot, hot)),
                    None => (&ellipsis, TypeRole::Body, lerp_ink(c.fg_secondary, c.fg_secondary_hot, hot)),
                };
                let ink = if self.offline { c.accent_danger } else { ink };
                text_centered(cx, text, role, it.plate.inset(cx.px(SEG_PAD), 0), ink);
                // The chevron after the segment (the fold's is part of its own plate's box).
                let cid = it.seg.map(|i| self.node(CHEV_BASE + i as u32));
                if let Some(cid) = cid {
                    let plate = if cx.pressed(cid) { c.fill_press } else { fade(cx, cid) };
                    cx.fill(it.chev, radius, plate);
                }
                let g = cx.px(CHEV_GLYPH);
                let gr = RectP::wh(it.chev.l + (it.chev.w() - g) / 2, it.chev.t + (it.chev.h() - g) / 2, g, g);
                cx.glyph(gr, Glyph::ChevronRight, c.fg_tertiary);
                if cx.focused(id) {
                    cx.focus_ring(it.plate, radius, false);
                }
            }
            let rid = self.node(REFRESH_SLOT);
            let hot = cx.hover_t(rid);
            let plate = if cx.pressed(rid) { c.fill_press } else { fade(cx, rid) };
            cx.fill(self.refresh, radius, plate);
            let g = cx.px(REFRESH_GLYPH);
            let r = self.refresh;
            let gr = RectP::wh(r.l + (r.w() - g) / 2, r.t + (r.h() - g) / 2, g, g);
            cx.glyph(gr, Glyph::Refresh, lerp_ink(c.fg_secondary, c.fg_secondary_hot, hot));
            if cx.focused(rid) {
                cx.focus_ring(self.refresh, radius, false);
            }
        }
        if let Some(reason) = &self.error {
            if !self.editing {
                cx.stroke(self.line, radius, cx.px(1).max(1), c.accent_danger);
            }
            let r = RectP { t: self.line.b + cx.px(REASON_GAP), b: self.line.b + cx.px(REASON_GAP + 16), ..self.line };
            cx.text_in(reason, TypeRole::Caption, r.inset(cx.px(SEG_PAD), 0), Align::Start, Trim::End, c.accent_danger);
        }
    }

    fn event(&mut self, cx: &mut EventCx, ev: &Event) -> Option<PathOut> {
        if self.editing {
            return self.edit_event(cx, ev);
        }
        let target = cx.target()?;
        let part = self.part(target)?;
        match ev {
            Event::Click { button: MouseButton::Left, .. } | Event::A11y(_, A11yAction::Click) => match part {
                Part::Group => {
                    self.begin_edit(cx);
                    Some(PathOut::EditStarted)
                }
                _ => self.activate(part),
            },
            Event::Key { key, down: true, mods, .. } if !mods.ctrl() && !mods.alt() => {
                if self.rove(cx, target, *key) {
                    return None;
                }
                match (*key, part) {
                    (Key::ENTER, _) => {
                        cx.consume();
                        self.activate(part)
                    }
                    (Key::DOWN, Part::Seg(_) | Part::Fold) => {
                        cx.consume();
                        self.siblings(part)
                    }
                    (Key::SPACE, _) => {
                        cx.consume();
                        None
                    }
                    _ => None,
                }
            }
            Event::Key { key: Key::SPACE, down: false, mods, .. } if !mods.ctrl() && !mods.alt() => {
                cx.consume();
                self.activate(part)
            }
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::command::{Chord, Command as Cmd, CommandId, CommandTable};
    use crate::cx::{LayoutCx, PaintCx};
    use crate::event::Reply;
    use crate::geom::{PointL, SizeL, SizeP};
    use crate::harness::Harness;
    use crate::host::AppMsg;
    use crate::screen::Screen;
    use crate::theme::Tokens;

    /// A window as the files app builds it: the bar, and Ctrl+L, Alt+D and F4 bound to begin_edit.
    struct App {
        bar: PathBar,
        outs: Vec<PathOut>,
        cmds: CommandTable,
    }

    enum Msg {
        Error(Option<&'static str>),
        Offline(bool),
        Completions(usize),
    }

    fn segs(n: usize) -> Vec<PathSegment> {
        (0..n)
            .map(|i| PathSegment {
                key: i as u64 + 1,
                label: Text::from(if i == 0 { "Local Disk".to_string() } else { format!("Folder {i}") }),
                tip: None,
            })
            .collect()
    }

    fn app(n: usize) -> App {
        let mut cmds = CommandTable::new();
        let c = Cmd::new(CommandId(1), Text::from("Edit path"))
            .chord(Chord::parse("Ctrl+L").unwrap())
            .alt_chord(Chord::parse("Alt+D").unwrap());
        cmds.add(c).unwrap();
        cmds.add(Cmd::new(CommandId(2), Text::from("Edit path too")).chord(Chord::parse("F4").unwrap())).unwrap();
        let bar = PathBar::new(WidgetId(1)).with_path(segs(n), "C:\\Users\\Folder 1".to_string());
        App { bar, outs: Vec::new(), cmds }
    }

    impl Screen for App {
        fn layout(&mut self, cx: &mut LayoutCx, avail: SizeP) -> SizeP {
            self.bar.layout(cx, RectP::wh(0, 0, avail.w, cx.px(44)));
            avail
        }

        fn paint(&self, cx: &mut PaintCx) {
            self.bar.paint(cx);
        }

        fn event(&mut self, cx: &mut EventCx, ev: &Event) -> Reply {
            if let Event::Command(CommandId(1 | 2)) = ev {
                self.bar.begin_edit(cx);
            } else if let Some(o) = self.bar.event(cx, ev) {
                self.outs.push(o);
            }
            Reply::None
        }

        fn data(&mut self, cx: &mut EventCx, msg: AppMsg) -> Reply {
            if let Ok(m) = msg.downcast::<Msg>() {
                match *m {
                    Msg::Error(e) => self.bar.set_error(cx, e.map(Text::from)),
                    Msg::Offline(on) => {
                        self.bar.set_offline(on);
                        cx.redraw();
                    }
                    Msg::Completions(n) => {
                        let items = (0..n).map(|i| Text::from(format!("C:\\Users\\Folder {i}"))).collect();
                        self.bar.set_completions(cx, items);
                    }
                }
            }
            Reply::None
        }

        fn commands(&self) -> &CommandTable {
            &self.cmds
        }
    }

    fn harness(n: usize, w: i32) -> Harness<App> {
        Harness::new(app(n), SizeL { w, h: 60 }, 96, Tokens::oled())
    }

    fn rect(h: &mut Harness<App>, slot: u32) -> Option<RectP> {
        h.dispatcher().frame.get(NodeId::new(WidgetId(1), slot)).map(|n| n.rect)
    }

    fn mid(r: RectP) -> PointL {
        PointL { x: (r.l + r.r) / 2, y: (r.t + r.b) / 2 }
    }

    fn focus(h: &Harness<App>) -> u32 {
        h.focused().map_or(u32::MAX, |n| n.slot())
    }

    #[test]
    fn fold_keeps_root_and_last_two() {
        let w = [50, 60, 70, 80, 90];
        assert_eq!(fold_count(&w, 350, 30), 0, "it fits");
        assert_eq!(fold_count(&w, 340, 30), 1, "the first level after the root folds first");
        assert_eq!(fold_count(&w, 300, 30), 2);
        assert_eq!(fold_count(&w, 10, 30), 2, "never more than all but the root and the last two");
        assert_eq!(fold_count(&[50, 60, 70], 10, 30), 0, "three segments never fold");
        assert_eq!(fold_count(&[50], 10, 30), 0);
        assert_eq!(fold_count(&[], 10, 30), 0);
        // Through the widget: the root, the fold and the last two are nodes; the folded levels are not.
        let mut h = harness(8, 330);
        assert!(rect(&mut h, 1).is_some(), "the root stays");
        assert!(rect(&mut h, FOLD_SLOT).is_some(), "the fold segment");
        assert!(rect(&mut h, 7).is_some() && rect(&mut h, 8).is_some(), "the last two stay");
        assert!(rect(&mut h, 2).is_none() && rect(&mut h, 3).is_none(), "the leading levels are folded");
        let mut h = harness(8, 900);
        assert!(rect(&mut h, FOLD_SLOT).is_none() && rect(&mut h, 2).is_some(), "wide: nothing folds");
    }

    #[test]
    fn chevron_requests_siblings_with_anchor() {
        let mut h = harness(4, 600);
        let chev = rect(&mut h, CHEV_BASE + 1).unwrap();
        assert_eq!((chev.w(), chev.h() > 16), (16, true), "a 16 px hit target");
        h.click(mid(chev));
        assert_eq!(h.screen().outs, [PathOut::SiblingsRequested(2, Anchor::Below(chev))]);
        // Clicking a segment navigates; Refresh refreshes.
        let seg = rect(&mut h, 3).unwrap();
        h.click(mid(seg));
        let refresh = rect(&mut h, REFRESH_SLOT).unwrap();
        assert_eq!((refresh.w(), refresh.h()), (24, 24));
        h.click(mid(refresh));
        assert_eq!(h.screen().outs[1..], [PathOut::Navigate(3), PathOut::Refresh]);
    }

    #[test]
    fn path_is_one_tab_stop_and_roves() {
        let mut h = harness(4, 600);
        h.key("Tab");
        assert_eq!(focus(&h), 4, "the current folder is the Tab stop");
        h.key("Left");
        assert_eq!(focus(&h), 3);
        h.key("Home");
        assert_eq!(focus(&h), 1);
        h.key("Left");
        assert_eq!(focus(&h), 1, "no wrap");
        h.key("End");
        assert_eq!(focus(&h), REFRESH_SLOT, "Refresh is the last stop");
        h.key("Right");
        assert_eq!(focus(&h), REFRESH_SLOT);
        h.key("Left");
        assert_eq!(focus(&h), 4);
        h.key("Home");
        h.key("Tab");
        assert_eq!(focus(&h), 4, "one Tab stop: Tab goes to the current folder, not to the next segment");
        h.assert_named();
        // The fold is a stop between the root and the next segment.
        let mut h = harness(8, 330);
        h.key("Tab");
        h.key("Home");
        h.key("Right");
        assert_eq!(focus(&h), FOLD_SLOT);
        h.key("Right");
        assert_eq!(focus(&h), 7, "the first level that stayed");
    }

    #[test]
    fn path_enter_navigates_to_the_focused_segment() {
        let mut h = harness(4, 600);
        h.key("Tab");
        h.key("Left");
        h.key("Enter");
        assert_eq!(h.screen().outs, [PathOut::Navigate(3)]);
        h.key("Home");
        h.key("Space");
        assert_eq!(h.screen().outs[1], PathOut::Navigate(1));
        h.key("End");
        h.key("Enter");
        assert_eq!(h.screen().outs[2], PathOut::Refresh);
    }

    #[test]
    fn path_down_requests_siblings() {
        let mut h = harness(4, 600);
        h.key("Tab");
        h.key("Down");
        let chev = rect(&mut h, CHEV_BASE + 3).unwrap();
        assert_eq!(h.screen().outs, [PathOut::SiblingsRequested(4, Anchor::Below(chev))]);
        // On the fold: the last folded level's key and the fold's own box.
        let mut h = harness(8, 330);
        h.key("Tab");
        h.key("Home");
        h.key("Right");
        h.key("Down");
        let fold = rect(&mut h, FOLD_SLOT).unwrap();
        let folded = h.screen().bar.folded;
        assert!(folded >= 1);
        assert_eq!(h.screen().outs, [PathOut::SiblingsRequested(folded as u64 + 1, Anchor::Below(fold))]);
    }

    #[test]
    fn edit_mode_selects_all_and_restores_on_esc() {
        let mut h = harness(4, 600);
        // A click on empty path-line space starts the edit.
        let (last, refresh) = (rect(&mut h, CHEV_BASE + 3).unwrap(), rect(&mut h, REFRESH_SLOT).unwrap());
        h.click(PointL { x: (last.r + refresh.l) / 2, y: 22 });
        assert_eq!(h.screen().outs, [PathOut::EditStarted]);
        assert!(h.a11y_dump().contains("TextInput"), "{}", h.a11y_dump());
        assert!(rect(&mut h, 1).is_none(), "the breadcrumb is gone");
        // The whole path is selected: typing replaces it.
        h.type_text("D");
        assert_eq!(h.screen().outs[1], PathOut::CompletionRequested("D".to_string()));
        h.key("Esc");
        assert_eq!(h.screen().outs[2], PathOut::EditCancelled);
        assert!(rect(&mut h, 1).is_some(), "Esc restores the breadcrumb");
        assert_eq!(focus(&h), 4, "with the focus on the current folder");
        // The app's chords: Ctrl+L, Alt+D and F4 call begin_edit; Enter commits the text.
        for chord in ["Ctrl+L", "Alt+D", "F4"] {
            let mut h = harness(4, 600);
            h.key(chord);
            assert!(h.a11y_dump().contains("TextInput"), "{chord}");
            h.key("Enter");
            assert_eq!(h.screen().outs, [PathOut::EditCommitted("C:\\Users\\Folder 1".to_string())], "{chord}");
        }
        // Esc on an empty field still ends the edit.
        let mut h = harness(4, 600);
        h.key("Ctrl+L");
        h.key("Backspace");
        h.key("Esc");
        assert_eq!(h.screen().outs, [PathOut::CompletionRequested(String::new()), PathOut::EditCancelled]);
        assert!(rect(&mut h, 1).is_some());
        assert!(h.is_idle(), "no field timer survives the edit");
    }

    #[test]
    fn completions_list_at_most_eight_rows() {
        let mut h = harness(4, 600);
        h.key("Ctrl+L");
        h.send(Box::new(Msg::Completions(12)));
        assert_eq!(h.menus().len(), 1, "one list under the field");
        assert_eq!(h.menus()[0].items.len(), 8);
        h.key("Esc");
    }

    #[test]
    fn error_and_offline_colour() {
        let reds = |h: &mut Harness<App>| {
            let img = h.render();
            img.px.iter().filter(|p| (**p >> 16) & 0xFF > 200 && **p & 0xFF < 120).count()
        };
        let mut h = harness(4, 600);
        assert_eq!(reds(&mut h), 0, "nothing red at rest");
        h.send(Box::new(Msg::Offline(true)));
        assert!(reds(&mut h) > 0, "offline: the text is ACCENT_DANGER");
        h.send(Box::new(Msg::Offline(false)));
        h.send(Box::new(Msg::Error(Some("Access denied"))));
        assert!(reds(&mut h) > 100, "error: a rim and the reason");
        h.send(Box::new(Msg::Error(None)));
        assert_eq!(reds(&mut h), 0);
    }
}
