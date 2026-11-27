//! The headless Harness (P2-kit 2.5): unit tests, goldens, the gallery and the app plans drive a screen through the
//! real dispatcher with a fake clock, a fake clipboard and a fake IME. K0.1b writes the skeleton (the band loop with
//! its hook, the effect loop, the dump); K1a.5 (KA1) fills the rest.

mod dump;
pub mod golden;
mod image;

pub use image::Image;

use crate::command::CommandId;
use crate::cx::{Anchor, Clipboard, Env, MenuId, MenuItem, PaintCx, Res, ToastSpec};
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

    /// Fires timers and internal deadlines in order of due time (K1a.5 adds the 16 ms animation frames).
    pub fn advance(&mut self, ms: u32) -> &mut Self {
        let end = self.now + u64::from(ms);
        while let Some(due) = self.disp.next_due().filter(|d| *d <= end) {
            self.now = self.now.max(due);
            let fired: Vec<_> = self.disp.timers.iter().filter(|t| t.1 <= self.now).map(|t| t.0).collect();
            self.disp.timers.retain(|t| t.1 > self.now);
            for id in fired {
                self.input(Input::Timer(id));
            }
            self.disp.on_due(self.now);
            self.apply();
            if self.disp.next_due() == Some(due) {
                break; // a deadline that does not move would loop forever
            }
        }
        self.now = end;
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
        let bh = band_height(w);
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
    fn input(&mut self, inp: Input) {
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
