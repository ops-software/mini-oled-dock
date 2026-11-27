//! The headless Harness (P2-kit 2.5): unit tests, goldens, the gallery and the app plans drive a screen through the
//! real dispatcher with a fake clock, a fake clipboard and a fake IME. K0.1b writes the skeleton (the band loop with
//! its hook, the effect loop, the dump); K1a.5 (KA1) fills the rest.

mod dump;
pub mod golden;
mod image;

pub use image::Image;

use crate::command::CommandId;
use crate::cx::{Anchor, Clipboard, Env, FrontResult, MenuId, MenuItem, PaintCx, Res, ToastSpec};
use crate::dispatch::{Dispatcher, Effect, Input};
use crate::event::{FocusReason, ImeEvent, Mods, MouseButton, WheelDelta};
use crate::geom::{PointL, PointP, RectP, Scale, SizeL};
use crate::host::{AppMsg, Caption};
use crate::node::NodeId;
use crate::present::band_height;
use crate::screen::Screen;
use crate::strings::{Lang, UiLang};
use crate::text::GdiTextEngine;
use crate::theme::Tokens;
use crate::w3::Canvas;

/// The Harness's clipboard.
pub struct FakeClipboard {
    pub text: Option<String>,
    pub files: bool,
}

impl Clipboard for FakeClipboard {
    fn text(&mut self) -> Option<String> {
        self.text.clone()
    }

    fn set_text(&mut self, s: &str) -> bool {
        self.text = Some(s.to_string());
        true
    }

    fn has_files(&mut self) -> bool {
        self.files
    }
}

/// A menu or list popup the screen opened.
pub struct MenuSnapshot {
    pub id: MenuId,
    pub items: Vec<MenuItem>,
    pub at: Anchor,
    pub highlight: Option<usize>,
}

/// What a screen asked of its window that only a real window does (TK1), recorded by the Harness.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WindowRequest {
    BringToFront,
    Topmost(bool),
}

/// A screen in a headless window.
pub struct Harness<S: Screen> {
    screen: S,
    disp: Dispatcher,
    res: Res,
    tokens: Tokens,
    size: SizeL,
    scale: Scale,
    lang: UiLang,
    caption: Caption,
    now: u64,
    clipboard: FakeClipboard,
    menus: Vec<MenuSnapshot>,
    outputs: Vec<String>,
    last: Option<Image>,
    dirty_layout: bool,
    requests: Vec<WindowRequest>,
}

impl<S: Screen> Harness<S> {
    /// `screen` at `size` logical px and `dpi`, English, active, laid out once.
    pub fn new(screen: S, size: SizeL, dpi: u32, tokens: Tokens) -> Harness<S> {
        let text = match GdiTextEngine::new() {
            Ok(t) => t,
            Err(e) => panic!("Harness: no text engine: {e:?}"),
        };
        let mut h = Harness {
            screen,
            disp: Dispatcher::new(),
            res: Res::new(Box::new(text)),
            tokens,
            size,
            scale: Scale { dpi },
            lang: UiLang::EN,
            caption: Caption::System,
            now: 0,
            clipboard: FakeClipboard { text: None, files: false },
            menus: Vec::new(),
            outputs: Vec::new(),
            last: None,
            dirty_layout: false,
            requests: Vec::new(),
        };
        h.layout();
        h
    }

    /// The language; Auto counts as En in the Harness.
    pub fn lang(mut self, lang: Lang) -> Harness<S> {
        self.lang.italian = lang == Lang::It;
        self.layout();
        self
    }

    /// The pseudo-locale on top of the language.
    pub fn pseudo(mut self, on: bool) -> Harness<S> {
        self.lang.pseudo = on;
        self.layout();
        self
    }

    /// The requested caption; the effective one follows host::caption::effective.
    pub fn caption(mut self, c: Caption) -> Harness<S> {
        self.caption = c;
        self.layout();
        self
    }

    /// "Ctrl+Shift+Tab": the key down and up, through the real dispatcher.
    pub fn key(&mut self, chord: &str) -> &mut Self {
        let Some(c) = crate::command::Chord::parse(chord) else {
            panic!("Harness::key: {chord:?} is not a chord");
        };
        self.input(Input::Key(c.key, true, false, c.mods));
        self.input(Input::Key(c.key, false, false, c.mods));
        self
    }

    /// One Event::Text per character.
    pub fn type_text(&mut self, s: &str) -> &mut Self {
        for c in s.chars() {
            self.input(Input::Char(c));
        }
        self
    }

    /// Start, the composition, then (with a result) Result and End.
    pub fn ime(&mut self, composition: &str, result: Option<&str>) -> &mut Self {
        self.input(Input::Ime(ImeEvent::Start));
        self.input(Input::Ime(ImeEvent::Composition { text: composition.to_string(), caret: composition.len() }));
        if let Some(r) = result {
            self.input(Input::Ime(ImeEvent::Result(r.to_string())));
            self.input(Input::Ime(ImeEvent::End));
        }
        self
    }

    pub fn click(&mut self, at: PointL) -> &mut Self {
        self.press(at, MouseButton::Left, 1)
    }

    pub fn double_click(&mut self, at: PointL) -> &mut Self {
        self.press(at, MouseButton::Left, 1);
        self.press(at, MouseButton::Left, 2)
    }

    pub fn right_click(&mut self, at: PointL) -> &mut Self {
        self.press(at, MouseButton::Right, 1)
    }

    pub fn hover(&mut self, at: PointL) -> &mut Self {
        let p = self.scale.point(at);
        self.input(Input::Move(p, Mods::NONE));
        self
    }

    /// Press at `from`, move to `to` in four steps, release.
    pub fn drag(&mut self, from: PointL, to: PointL) -> &mut Self {
        let (a, b) = (self.scale.point(from), self.scale.point(to));
        self.input(Input::Move(a, Mods::NONE));
        self.input(Input::Down(a, MouseButton::Left, 1, Mods::NONE));
        for i in 1..=4 {
            let p = PointP { x: a.x + (b.x - a.x) * i / 4, y: a.y + (b.y - a.y) * i / 4 };
            self.input(Input::Move(p, Mods::NONE));
        }
        self.input(Input::Up(b, MouseButton::Left, Mods::NONE));
        self
    }

    pub fn wheel(&mut self, at: PointL, notches: f32) -> &mut Self {
        let p = self.scale.point(at);
        self.input(Input::Move(p, Mods::NONE));
        self.input(Input::Wheel(p, WheelDelta::Lines(notches), false, Mods::NONE));
        self
    }

    /// Screen::data.
    pub fn send(&mut self, msg: AppMsg) -> &mut Self {
        self.input(Input::Data(msg));
        self
    }

    /// A WM_COPYDATA with `tag` (dwData) and `bytes`, as the window delivers it: Screen::copy_data's answer, or false
    /// without a call for a payload above 64 KiB.
    pub fn copy_data(&mut self, tag: usize, bytes: &[u8]) -> bool {
        if !crate::host::window::copy_data_fits(bytes.len()) {
            return false;
        }
        self.disp.copied = false;
        self.input(Input::CopyData(tag, bytes.to_vec()));
        std::mem::take(&mut self.disp.copied)
    }

    /// A routed message (EventLoop::route_message) as the window delivers it: Screen::app_message.
    pub fn app_message(&mut self, msg: u32, wparam: usize, lparam: isize) -> &mut Self {
        self.input(Input::Routed(msg, wparam, lparam));
        self
    }

    /// What the screen asked of its window (bring_to_front, set_topmost), oldest first. A headless window always gets
    /// the foreground: front_result reads Foreground after a bring_to_front.
    pub fn window_requests(&self) -> &[WindowRequest] {
        &self.requests
    }

    /// Moves the fake clock by `ms` in steps of at most 16 ms (the frame clock), stopping at every timer and internal
    /// deadline on the way, which fire in order of due time.
    pub fn advance(&mut self, ms: u32) -> &mut Self {
        let end = self.now + u64::from(ms);
        let mut stalled = 0;
        while self.now < end {
            let frame = (self.now + 16).min(end);
            let to = match self.disp.next_due() {
                Some(due) if due <= frame => due.max(self.now),
                _ => frame,
            };
            stalled = if to == self.now { stalled + 1 } else { 0 };
            // A deadline that does not move would loop forever: after a few rounds at one instant, take the frame step.
            self.now = if stalled > 8 { frame } else { to };
            for id in self.disp.take_due_timers(self.now) {
                self.input(Input::Timer(id));
            }
            self.disp.on_due(self.now);
            self.apply();
        }
        self
    }

    pub fn set_active(&mut self, on: bool) -> &mut Self {
        self.input(Input::Active(on));
        self
    }

    pub fn set_tokens(&mut self, t: Tokens) -> &mut Self {
        self.tokens = t;
        self.layout();
        self
    }

    /// The window as the banded presenter draws it: each band cleared to bg, the screen's paint, then
    /// hooks::after_paint_band (toasts, the custom caption).
    pub fn render(&mut self) -> Image {
        let bh = band_height(self.scale.size(self.size).w.max(0));
        self.render_banded(bh)
    }

    /// render() with an explicit band height: the tests compare it with one full-height band.
    pub(crate) fn render_banded(&mut self, band_rows: i32) -> Image {
        if self.dirty_layout {
            self.layout();
        }
        let size = self.scale.size(self.size);
        let (w, h) = (size.w.max(0), size.h.max(0));
        let bg = self.tokens.color.bg;
        let bg_px = crate::w3::argb(
            bg.a,
            crate::w3::div255(bg.rgb.0 * bg.a),
            crate::w3::div255(bg.rgb.1 * bg.a),
            crate::w3::div255(bg.rgb.2 * bg.a),
        );
        let mut px = vec![bg_px; (w * h) as usize];
        let mut text_rects = Vec::new();
        let env = Env {
            scale: self.scale,
            tokens: &self.tokens,
            lang: self.lang,
            now_ms: self.now,
            caption: self.caption,
            caption_spike: false,
        };
        let bh = band_rows.max(1);
        let mut top = 0;
        while top < h {
            let rows = bh.min(h - top);
            let band = &mut px[(top * w) as usize..((top + rows) * w) as usize];
            let canvas = Canvas { buf: band, w, h: rows, clip: None };
            let origin = PointP { x: 0, y: top };
            let clip = RectP::wh(0, top, w, rows);
            let mut cx = PaintCx::new(env, canvas, origin, clip, &self.disp, &mut self.res, Some(&mut text_rects));
            self.screen.paint(&mut cx);
            crate::host::hooks::after_paint_band(&mut cx);
            top += rows;
        }
        // A run that straddles two bands was logged by each of them.
        text_rects.sort_by_key(|r| (r.t, r.l, r.r, r.b));
        text_rects.dedup();
        let img = Image { w: w as u32, h: h as u32, px, text_rects };
        self.last = Some(Image { w: img.w, h: img.h, px: img.px.clone(), text_rects: img.text_rects.clone() });
        img
    }

    /// The menu `index` as its window draws it (K2a.9); None until then.
    pub fn menu_image(&mut self, index: usize) -> Option<Image> {
        let env = Env {
            scale: self.scale,
            tokens: &self.tokens,
            lang: self.lang,
            now_ms: self.now,
            caption: self.caption,
            caption_spike: false,
        };
        let snap = self.menus.get(index)?;
        crate::host::menu::snapshot_image(snap, self.screen.commands(), env, &mut self.res)
    }

    /// One line per node: indent, role, "name", [states], value.
    pub fn a11y_dump(&self) -> String {
        dump::dump(&self.disp.frame)
    }

    /// Panics listing the interactive nodes without a name (ROADMAP 3.1 gate b).
    pub fn assert_named(&self) {
        let unnamed: Vec<String> = self
            .disp
            .frame
            .iter()
            .filter(|n| n.focus != crate::node::Focusable::No || n.actions.0 != 0)
            .filter(|n| match &n.label {
                crate::node::Label::Text(t) => t.trim().is_empty(),
                crate::node::Label::None => true,
            })
            .map(|n| format!("{:?} {:?}", n.role, n.id))
            .collect();
        assert!(unnamed.is_empty(), "interactive nodes without a name: {unnamed:?}");
    }

    /// No tween live, no timer armed, no internal deadline pending.
    pub fn is_idle(&self) -> bool {
        self.disp.timers.is_empty() && !self.disp.anim.is_live(self.now) && self.disp.next_due().is_none()
    }

    /// The dispatcher, for the kit's own tests.
    #[cfg(test)]
    pub(crate) fn dispatcher(&mut self) -> &mut Dispatcher {
        &mut self.disp
    }

    /// Event::Command(id) as the dispatcher sends it (a kit id to its owner), then the effects.
    #[cfg(test)]
    pub(crate) fn command(&mut self, id: CommandId) {
        let env = Env {
            scale: self.scale,
            tokens: &self.tokens,
            lang: self.lang,
            now_ms: self.now,
            caption: self.caption,
            caption_spike: false,
        };
        self.disp.command(&mut self.screen, env, &mut self.res, &mut self.clipboard, id);
        self.apply();
    }

    pub fn screen(&self) -> &S {
        &self.screen
    }

    pub fn screen_mut(&mut self) -> &mut S {
        &mut self.screen
    }

    pub fn clipboard(&mut self) -> &mut FakeClipboard {
        &mut self.clipboard
    }

    pub fn menus(&self) -> &[MenuSnapshot] {
        &self.menus
    }

    pub fn toasts(&self) -> &[ToastSpec] {
        &self.disp.toasts.specs
    }

    pub fn focused(&self) -> Option<NodeId> {
        self.disp.state.focused
    }

    /// The Debug text of each reply, for scripts.
    pub fn last_outputs(&self) -> &[String] {
        &self.outputs
    }

    fn press(&mut self, at: PointL, button: MouseButton, clicks: u8) -> &mut Self {
        let p = self.scale.point(at);
        self.input(Input::Move(p, Mods::NONE));
        self.input(Input::Down(p, button, clicks, Mods::NONE));
        self.input(Input::Up(p, button, Mods::NONE));
        self
    }

    fn layout(&mut self) {
        let env = Env {
            scale: self.scale,
            tokens: &self.tokens,
            lang: self.lang,
            now_ms: self.now,
            caption: self.caption,
            caption_spike: false,
        };
        let avail = self.scale.size(self.size);
        self.disp.layout(&mut self.screen, env, &mut self.res, avail);
        self.dirty_layout = false;
    }

    /// One input through the dispatcher, then its effects.
    pub(crate) fn input(&mut self, inp: Input) {
        let env = Env {
            scale: self.scale,
            tokens: &self.tokens,
            lang: self.lang,
            now_ms: self.now,
            caption: self.caption,
            caption_spike: false,
        };
        let reply = self.disp.input(&mut self.screen, inp, env, &mut self.res, &mut self.clipboard);
        self.outputs.push(format!("{reply:?}"));
        match reply {
            crate::event::Reply::Relayout => self.dirty_layout = true,
            crate::event::Reply::Close => self.outputs.push("CloseWindow".to_string()),
            _ => {}
        }
        self.apply();
    }

    /// Applies the effects the way the host does after an event (K1a.5 completes the list).
    fn apply(&mut self) {
        for _round in 0..16 {
            let effects = std::mem::take(&mut self.disp.effects);
            if effects.is_empty() {
                break;
            }
            for e in effects {
                self.apply_one(e);
            }
        }
        if self.dirty_layout {
            self.layout();
        }
    }

    fn apply_one(&mut self, e: Effect) {
        match e {
            Effect::Relayout => self.dirty_layout = true,
            Effect::SetFocus(id) => {
                let env = Env {
                    scale: self.scale,
                    tokens: &self.tokens,
                    lang: self.lang,
                    now_ms: self.now,
                    caption: self.caption,
                    caption_spike: false,
                };
                let reason = FocusReason::Programmatic;
                self.disp.set_focus(&mut self.screen, env, &mut self.res, &mut self.clipboard, Some(id), reason);
            }
            Effect::Capture => self.disp.state.captured = self.disp.state.pressed.or(self.disp.state.hot),
            Effect::Release => self.disp.state.captured = None,
            Effect::SetTimer(id, after) => {
                self.disp.timers.retain(|t| t.0 != id);
                self.disp.timers.push((id, self.now + u64::from(after)));
            }
            Effect::CancelTimer(id) => self.disp.timers.retain(|t| t.0 != id),
            Effect::Animate { id, to, dur_ms, ease } => self.disp.anim.start(id, to, dur_ms, ease, self.now),
            Effect::OpenMenu { id, items, at, .. } => self.menus.push(MenuSnapshot { id, items, at, highlight: None }),
            Effect::OpenList { id, items, at, highlight } => {
                let items = list_items(items);
                self.menus.push(MenuSnapshot { id, items, at, highlight });
            }
            Effect::UpdateList { id, items, highlight } => {
                if let Some(m) = self.menus.iter_mut().find(|m| m.id == id) {
                    m.items = list_items(items);
                    m.highlight = highlight;
                }
            }
            Effect::CloseMenus => self.menus.clear(),
            Effect::Scroll { viewport, dy } => {
                if let Some(img) = self.last.as_mut() {
                    // Until K2a.10 emulate moves nothing and the next render repaints the viewport.
                    let _ = crate::present::scroll::emulate(img, viewport, dy);
                }
            }
            Effect::CloseWindow => self.outputs.push("CloseWindow".to_string()),
            Effect::BringToFront => {
                self.requests.push(WindowRequest::BringToFront);
                self.disp.front = Some(FrontResult::Foreground);
            }
            Effect::Topmost(on) => self.requests.push(WindowRequest::Topmost(on)),
            Effect::Damage(_)
            | Effect::Redraw
            | Effect::SetCaret(_)
            | Effect::ShowTip { .. }
            | Effect::HideTip
            | Effect::FocusChanged(_) => {}
        }
    }
}

/// A list popup's texts as menu entries (their index is their id).
fn list_items(items: Vec<crate::node::Text>) -> Vec<MenuItem> {
    items
        .into_iter()
        .enumerate()
        .map(|(i, label)| MenuItem::Custom {
            id: CommandId(i as u32),
            label,
            glyph: None,
            chord: None,
            enabled: true,
            checked: None,
            note: None,
            danger: false,
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::command::CommandTable;
    use crate::cx::{EventCx, LayoutCx};
    use crate::event::{Event, Reply};
    use crate::geom::SizeP;
    use crate::model::Align;
    use crate::node::{Node, NodeFlags, NodeValue, Role, Text, WidgetId};
    use crate::text::Trim;
    use crate::theme::{Ink, TypeRole};

    /// Rows of fills and text, and a log of the IME events it got.
    struct Probe {
        cmds: CommandTable,
        rows: i32,
        ime: Vec<ImeEvent>,
    }

    impl Probe {
        fn new(rows: i32) -> Probe {
            Probe { cmds: CommandTable::new(), rows, ime: Vec::new() }
        }
    }

    impl Screen for Probe {
        fn layout(&mut self, cx: &mut LayoutCx, avail: SizeP) -> SizeP {
            let w = WidgetId(1);
            let root =
                Node::new(NodeId::new(w, 0), Role::Window, RectP::wh(0, 0, avail.w, avail.h)).label(Text::from("Main"));
            cx.push(root);
            let at = |i| NodeId::new(w, i);
            let r = RectP::wh(10, 10, 80, 28);
            cx.push(
                Node::new(at(1), Role::Button, r)
                    .parent(at(0))
                    .label(Text::from("Close"))
                    .focus(crate::node::Focusable::Tab),
            );
            cx.push(
                Node::new(at(2), Role::Switch, r)
                    .parent(at(0))
                    .label(Text::from("Dark"))
                    .flags(NodeFlags::CHECKED | NodeFlags::FOCUSABLE)
                    .value(NodeValue::Toggle(true)),
            );
            cx.push(Node::new(at(3), Role::Slider, r).parent(at(1)).label(Text::from("Vol")).value(NodeValue::Range {
                value: 3.0,
                min: 0.0,
                max: 10.0,
                step: 1.0,
            }));
            cx.push(
                Node::new(at(4), Role::Label, r)
                    .parent(at(0))
                    .flags(NodeFlags::A11Y_HIDDEN)
                    .label(Text::from("hidden")),
            );
            avail
        }

        fn paint(&self, cx: &mut PaintCx) {
            let fg = cx.tokens().color.fg_primary;
            for i in 0..self.rows {
                let r = RectP::wh(cx.px(20), cx.px(10 + i * 54), cx.px(300), cx.px(50));
                if !cx.visible(r) {
                    continue;
                }
                cx.fill(r, cx.px(6), Ink::white(16 + (i as u32 % 5) * 12));
                cx.text_in(
                    &format!("Row {i} of the page"),
                    TypeRole::Body,
                    r.inset(cx.px(12), 0),
                    Align::Start,
                    Trim::End,
                    fg,
                );
            }
        }

        fn event(&mut self, _cx: &mut EventCx, ev: &Event) -> Reply {
            if let Event::Ime(e) = ev {
                self.ime.push(e.clone());
            }
            Reply::None
        }

        fn commands(&self) -> &CommandTable {
            &self.cmds
        }
    }

    #[test]
    fn banded_equals_single_band() {
        let size = SizeL { w: 3840, h: 2160 };
        let mut h = Harness::new(Probe::new(40), size, 96, Tokens::oled());
        let banded = h.render();
        let one = h.render_banded(2160);
        assert_eq!((banded.w, banded.h), (3840, 2160));
        assert!(banded.px == one.px, "banded and single-band renders differ");
        assert_eq!(banded.text_rects.len(), 40, "every row logged its text rect once");
        assert!(banded.px.iter().any(|p| *p != banded.px[0]), "something was drawn");
    }

    #[test]
    fn dump_format() {
        let h = Harness::new(Probe::new(0), SizeL { w: 200, h: 100 }, 96, Tokens::oled());
        assert_eq!(
            h.a11y_dump(),
            "Window \"Main\"\n  Button \"Close\" [focusable]\n  Switch \"Dark\" [focusable, checked] on\n    Slider \"Vol\" 3 (0..10)\n"
        );
    }

    #[test]
    fn fake_ime_sequence() {
        let mut h = Harness::new(Probe::new(0), SizeL { w: 200, h: 100 }, 96, Tokens::oled());
        h.ime("ni", Some("\u{4f60}"));
        assert_eq!(
            h.screen().ime,
            [
                ImeEvent::Start,
                ImeEvent::Composition { text: "ni".to_string(), caret: 2 },
                ImeEvent::Result("\u{4f60}".to_string()),
                ImeEvent::End
            ]
        );
        h.screen_mut().ime.clear();
        h.ime("n", None);
        assert_eq!(h.screen().ime.len(), 2, "a composition left open has no Result and no End");
    }

    #[test]
    fn language_and_pseudo_reach_the_screen() {
        let h = Harness::new(Probe::new(0), SizeL { w: 10, h: 10 }, 96, Tokens::oled()).lang(Lang::It).pseudo(true);
        assert!(h.lang.italian && h.lang.pseudo);
        let h = Harness::new(Probe::new(0), SizeL { w: 10, h: 10 }, 96, Tokens::oled()).lang(Lang::Auto);
        assert!(!h.lang.italian, "Auto counts as English");
    }

    /// Records what reaches it through the TK1 hooks, and asks for the window requests on a key.
    struct Recorder {
        cmds: CommandTable,
        answer: bool,
        seen: std::rc::Rc<std::cell::RefCell<Vec<String>>>,
    }

    impl Recorder {
        fn new(answer: bool) -> Recorder {
            Recorder { cmds: CommandTable::new(), answer, seen: Default::default() }
        }
    }

    impl Screen for Recorder {
        fn layout(&mut self, _cx: &mut LayoutCx, avail: SizeP) -> SizeP {
            avail
        }

        fn paint(&self, _cx: &mut PaintCx) {}

        fn event(&mut self, cx: &mut EventCx, ev: &Event) -> Reply {
            if let Event::Key { down: true, .. } = ev {
                self.seen.borrow_mut().push(format!("front before: {:?}", cx.front_result()));
                cx.bring_to_front();
                cx.set_topmost(true);
                cx.set_topmost(false);
            }
            Reply::None
        }

        fn copy_data(&mut self, cx: &mut EventCx, tag: usize, bytes: &[u8]) -> bool {
            self.seen.borrow_mut().push(format!("copy {tag:#x} {bytes:?} front {:?}", cx.front_result()));
            self.answer
        }

        fn app_message(&mut self, _cx: &mut EventCx, msg: u32, wparam: usize, lparam: isize) -> Reply {
            self.seen.borrow_mut().push(format!("msg {msg:#x} {wparam} {lparam}"));
            Reply::Redraw
        }

        fn commands(&self) -> &CommandTable {
            &self.cmds
        }
    }

    fn seen(h: &Harness<Recorder>) -> Vec<String> {
        h.screen().seen.borrow().clone()
    }

    #[test]
    fn copy_data_reaches_the_screen() {
        let mut h = Harness::new(Recorder::new(true), SizeL { w: 10, h: 10 }, 96, Tokens::oled());
        assert!(h.copy_data(0x5449_4E53, b"abc"), "the screen's answer");
        h.screen_mut().answer = false;
        assert!(!h.copy_data(7, &[]), "the screen's answer");
        assert_eq!(seen(&h), ["copy 0x54494e53 [97, 98, 99] front None", "copy 0x7 [] front None"]);
        // Through a Box<dyn Screen>, as the window holds it.
        let rec = Recorder::new(true);
        let log = std::rc::Rc::clone(&rec.seen);
        let boxed: Box<dyn Screen> = Box::new(rec);
        let mut h = Harness::new(boxed, SizeL { w: 10, h: 10 }, 96, Tokens::oled());
        assert!(h.copy_data(1, b"z"));
        h.app_message(0xC123, 4, -5);
        assert_eq!(*log.borrow(), ["copy 0x1 [122] front None", "msg 0xc123 4 -5"], "the Box forwards both hooks");
    }

    #[test]
    fn copy_data_over_64_kib_is_refused() {
        let mut h = Harness::new(Recorder::new(true), SizeL { w: 10, h: 10 }, 96, Tokens::oled());
        assert!(h.copy_data(1, &vec![0u8; 64 * 1024]), "64 KiB reaches the screen");
        assert!(!h.copy_data(2, &vec![0u8; 64 * 1024 + 1]), "one byte more is refused");
        assert_eq!(seen(&h).len(), 1, "a refused payload reaches no screen");
        assert!(!crate::host::window::copy_data_fits(64 * 1024 + 1), "the window's own check");
    }

    #[test]
    fn routed_message_reaches_the_screen() {
        let mut h = Harness::new(Recorder::new(true), SizeL { w: 10, h: 10 }, 96, Tokens::oled());
        h.app_message(0xC0DE, 0x0001_0602, 0).app_message(0x8005, 0, -1);
        assert_eq!(seen(&h), ["msg 0xc0de 67074 0", "msg 0x8005 0 -1"]);
        assert_eq!(h.last_outputs().last().map(String::as_str), Some("Redraw"), "the screen's reply");
    }

    #[test]
    fn bring_to_front_is_recorded() {
        let mut h = Harness::new(Recorder::new(true), SizeL { w: 10, h: 10 }, 96, Tokens::oled());
        assert!(h.window_requests().is_empty());
        h.key("a");
        assert_eq!(
            h.window_requests(),
            [WindowRequest::BringToFront, WindowRequest::Topmost(true), WindowRequest::Topmost(false)]
        );
        h.copy_data(3, b"");
        h.key("b");
        let log = seen(&h);
        assert_eq!(log[0], "front before: None");
        assert_eq!(log[1], "copy 0x3 [] front Some(Foreground)", "a headless window gets the foreground");
        assert_eq!(log[2], "front before: Some(Foreground)");
        assert_eq!(h.window_requests().len(), 6);
    }

    #[test]
    fn the_fake_clock_steps_by_frames_and_stops_at_deadlines() {
        let mut h = Harness::new(Probe::new(0), SizeL { w: 10, h: 10 }, 96, Tokens::oled());
        h.advance(100);
        assert!(h.is_idle());
        assert_eq!(h.now, 100);
        h.dispatcher().timers.push((crate::event::TimerId(1), 130));
        assert!(!h.is_idle());
        h.advance(40);
        assert!(h.is_idle(), "the timer fired at its due time");
    }
}
