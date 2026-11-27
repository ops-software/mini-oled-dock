//! The dispatcher: the one routing core of the Harness and the real host, so window behaviour is tested headless
//! (P2-kit 2.13, K1a.4). K0.1b writes the skeleton: the complete Input and Effect enums, the state it holds and every
//! hook call of 2.13 at its point; the routing rules of SPEC-platform 5.3.5 (K1a.4) are written around those calls.

use crate::anim::{AnimClock, HOVER_CHANNEL};
use crate::command::CommandId;
use crate::cx::{Anchor, AnimId, Clipboard, Ease, Env, EventCx, FrontResult, LayoutCx, MenuId, MenuItem, Res};
use crate::event::{A11yAction, Event, FocusReason, ImeEvent, Key, Mods, MouseButton, Reply, TimerId, WheelDelta};
use crate::geom::{PointP, RectP, SizeP};
use crate::host::toast::{self, ToastStack};
use crate::host::AppMsg;
use crate::node::{Focusable, Frame, NodeFlags, NodeId, Role, Text};
use crate::screen::Screen;
use crate::widgets::menu::{self, MenuPress, MenuStack};
use crate::widgets::tooltip::{self, TipInput, TipState};

/// What the host (or the Harness) feeds the dispatcher.
#[allow(dead_code)] // Leave, Close and A11y come from the host (K1a.8) and the bridge (KG).
pub(crate) enum Input {
    Move(PointP, Mods),
    Down(PointP, MouseButton, u8, Mods),
    Up(PointP, MouseButton, Mods),
    Leave,
    Wheel(PointP, WheelDelta, bool, Mods),
    /// Key, down, repeat, modifiers.
    Key(Key, bool, bool, Mods),
    Char(char),
    Ime(ImeEvent),
    Active(bool),
    Close,
    Timer(TimerId),
    A11y(NodeId, A11yAction),
    Data(AppMsg),
    /// A WM_COPYDATA's tag and bytes (TK1): Screen::copy_data, whose answer waits in Dispatcher::copied.
    CopyData(usize, Vec<u8>),
    /// A message routed with EventLoop::route_message, its wParam and lParam (TK1): Screen::app_message.
    Routed(u32, usize, isize),
}

/// What the screen asked for while handling an input; the host (window::apply_effects) or the Harness applies them
/// after the event returns.
#[derive(Clone, Debug, PartialEq)]
#[allow(dead_code)] // ShowTip and HideTip come from the tooltip (K1b.4).
pub(crate) enum Effect {
    Damage(RectP),
    Redraw,
    Relayout,
    SetFocus(NodeId),
    Capture,
    Release,
    SetTimer(TimerId, u32),
    CancelTimer(TimerId),
    Animate { id: AnimId, to: f32, dur_ms: u32, ease: Ease },
    OpenMenu { id: MenuId, items: Vec<MenuItem>, at: Anchor, opener: Option<NodeId> },
    OpenList { id: MenuId, items: Vec<Text>, at: Anchor, highlight: Option<usize> },
    UpdateList { id: MenuId, items: Vec<Text>, highlight: Option<usize> },
    CloseMenus,
    Scroll { viewport: RectP, dy: i32 },
    SetCaret(Option<RectP>),
    ShowTip { node: NodeId, rect: RectP, text: Text, chord: Option<Text> },
    HideTip,
    FocusChanged(Option<NodeId>),
    CloseWindow,
    BringToFront,
    Topmost(bool),
}

/// A pressed pointer button: where it went down, which button and how many clicks, and whether a drag has begun.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Press {
    pub(crate) origin: PointP,
    pub(crate) button: MouseButton,
    pub(crate) clicks: u8,
    pub(crate) dragging: bool,
}

/// Pointer and focus state between inputs.
#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct InteractionState {
    pub(crate) hot: Option<NodeId>,
    pub(crate) pressed: Option<NodeId>,
    pub(crate) focused: Option<NodeId>,
    pub(crate) captured: Option<NodeId>,
    pub(crate) focus_visible: bool,
    pub(crate) window_active: bool,
    pub(crate) press: Option<Press>,
    /// The last pointer position: hot is re-resolved from it after a relayout.
    pub(crate) last_pointer: Option<PointP>,
}

/// One window's routing state.
pub(crate) struct Dispatcher {
    pub(crate) frame: Frame,
    pub(crate) state: InteractionState,
    pub(crate) anim: AnimClock,
    /// Armed one-shot timers and their due times.
    pub(crate) timers: Vec<(TimerId, u64)>,
    pub(crate) menus: MenuStack,
    pub(crate) tips: TipState,
    pub(crate) toasts: ToastStack,
    pub(crate) effects: Vec<Effect>,
    /// An unhandled Esc closes the window (WindowSpec::esc_closes).
    pub(crate) esc_closes: bool,
    /// The outcome of the window's last bring_to_front, once applied (EventCx::front_result).
    pub(crate) front: Option<FrontResult>,
    /// Screen::copy_data's answer to the last Input::CopyData.
    pub(crate) copied: bool,
    menu_ids: u32,
}

impl Dispatcher {
    pub(crate) fn new() -> Dispatcher {
        Dispatcher {
            frame: Frame::new(),
            state: InteractionState { window_active: true, ..InteractionState::default() },
            anim: AnimClock::new(),
            timers: Vec::new(),
            menus: MenuStack::default(),
            tips: TipState::default(),
            toasts: ToastStack::default(),
            effects: Vec::new(),
            esc_closes: false,
            front: None,
            copied: false,
            menu_ids: 0,
        }
    }

    /// A new menu id (never 0).
    pub(crate) fn next_menu_id(&mut self) -> MenuId {
        self.menu_ids = self.menu_ids.wrapping_add(1).max(1);
        MenuId(self.menu_ids)
    }

    /// Lays the screen out: the frame is rebuilt from what the screen pushes. Afterwards hot is re-resolved by
    /// hit-testing the last pointer position (virtual rows reuse NodeIds for other rows after a sort) and the focus
    /// is kept by NodeId, or dropped when its node is gone.
    pub(crate) fn layout(&mut self, screen: &mut dyn Screen, env: Env<'_>, res: &mut Res, avail: SizeP) -> SizeP {
        self.frame.clear();
        let again = env;
        let mut cx = LayoutCx::new(env, self, res);
        let size = screen.layout(&mut cx, avail);
        toast::layout(&mut cx, size);
        if let Some(p) = self.state.last_pointer {
            // Through set_hot: the old node fades out, the new one fades in and both are damaged.
            let new = self.frame.hit(p).map(|n| n.id);
            self.set_hot(new, &again);
        }
        let frame = &self.frame;
        self.anim.retain_hover(|id| frame.get(id).is_some(), again.now_ms);
        if let Some(f) = self.state.focused {
            if self.frame.get(f).is_none() {
                self.state.focused = None;
                self.effects.push(Effect::FocusChanged(None));
            }
        }
        size
    }

    /// The earliest internal deadline (tooltip delay, toast hold, submenu delay) or armed timer, if any.
    pub(crate) fn next_due(&self) -> Option<u64> {
        let internal = [tooltip::next_due(&self.tips), toast::next_due(&self.toasts), menu::next_due(&self.menus)];
        internal.into_iter().flatten().chain(self.timers.iter().map(|t| t.1)).min()
    }

    /// Runs the internal deadlines due at `now_ms`.
    pub(crate) fn on_due(&mut self, now_ms: u64) {
        tooltip::on_due(&mut self.tips, now_ms, &mut self.effects);
        toast::on_due(&mut self.toasts, now_ms, &mut self.effects);
        menu::on_due(&mut self.menus, now_ms, &mut self.effects);
    }

    /// Removes the timers due at `now_ms` and returns them in order of due time (ties in arming order).
    pub(crate) fn take_due_timers(&mut self, now_ms: u64) -> Vec<TimerId> {
        let mut due: Vec<(TimerId, u64)> = self.timers.iter().copied().filter(|t| t.1 <= now_ms).collect();
        due.sort_by_key(|t| t.1);
        self.timers.retain(|t| t.1 > now_ms);
        due.into_iter().map(|t| t.0).collect()
    }

    /// The focus ring and its gap around `r`: what a hot or focus change must repaint.
    fn halo(&self, r: RectP, env: &Env<'_>) -> RectP {
        let g = env.scale.px(env.tokens.size.focus_gap) + env.scale.ring();
        r.inset(-g, -g)
    }

    fn damage_node(&mut self, id: Option<NodeId>, env: &Env<'_>) {
        if let Some(r) = id.and_then(|i| self.frame.get(i)).map(|n| n.rect) {
            let r = self.halo(r, env);
            self.effects.push(Effect::Damage(r));
        }
    }

    /// Changes the hot node: damages the old and the new one and starts the hover fade of flagged nodes (DUR_FAST in,
    /// DUR_NORMAL out, out-cubic).
    fn set_hot(&mut self, to: Option<NodeId>, env: &Env<'_>) {
        let from = self.state.hot;
        if from == to {
            return;
        }
        self.state.hot = to;
        for (id, target, dur) in [(from, 0.0, env.tokens.motion.dur_normal), (to, 1.0, env.tokens.motion.dur_fast)] {
            // A node that never faded was at its resting value: 1 when it was hot, else 0.
            let first = 1.0 - target;
            let Some(id) = id else { continue };
            self.damage_node(Some(id), env);
            if self.frame.get(id).is_some_and(|n| n.flags.contains(NodeFlags::HOVER_FADE)) {
                let anim = AnimId { node: id, channel: HOVER_CHANNEL };
                self.anim.start_from(anim, first, target, dur, Ease::OutCubic, env.now_ms);
            }
        }
    }

    /// Routes one input to the screen (SPEC-platform 5.3.5). Returns the screen's reply; the effects wait in
    /// `effects` for the caller.
    pub(crate) fn input(
        &mut self,
        screen: &mut dyn Screen,
        inp: Input,
        env: Env<'_>,
        res: &mut Res,
        clipboard: &mut dyn Clipboard,
    ) -> Reply {
        // The menus the screen opened are laid out with its command table before any input can reach them (K2a.9).
        menu::resolve(&mut self.menus, screen.commands(), &env, &mut *res.text);
        match inp {
            Input::Move(pos, mods) => self.pointer_move(screen, env, res, clipboard, pos, mods),
            Input::Down(pos, button, clicks, mods) => {
                if toast::hit(&mut self.toasts, pos).is_some() {
                    self.effects.push(Effect::Relayout);
                    return match toast::take_action(&mut self.toasts) {
                        Some(id) => self.command(screen, env, res, clipboard, id),
                        None => Reply::None,
                    };
                }
                if menu::on_pointer_down(&mut self.menus, pos, &mut self.effects) == MenuPress::Swallow {
                    return self.menu_events(screen, env, res, clipboard);
                }
                tooltip::on_input(&mut self.tips, TipInput::Press, &mut self.effects);
                let hit = self.frame.hit(pos).cloned();
                let target = hit.as_ref().map(|n| n.id);
                self.state.pressed = target;
                self.state.captured = target;
                self.damage_node(target, &env);
                self.state.press = target.map(|_| Press { origin: pos, button, clicks, dragging: false });
                if self.state.focus_visible {
                    self.damage_node(self.state.focused, &env);
                }
                self.state.focus_visible = false;
                if let Some(n) = hit.filter(|n| {
                    n.focus != Focusable::No && !n.flags.contains(NodeFlags::DISABLED) && button == MouseButton::Left
                }) {
                    self.set_focus(screen, env, res, clipboard, Some(n.id), FocusReason::Pointer);
                }
                self.deliver(screen, env, res, clipboard, target, &Event::PointerDown { pos, button, clicks, mods }).0
            }
            Input::Up(pos, button, mods) => {
                let up_hit = self.frame.hit(pos).map(|n| n.id);
                let pressed = self.state.pressed.take();
                let press = self.state.press.take();
                let target = self.state.captured.take().or(up_hit);
                self.damage_node(pressed, &env);
                let mut reply =
                    self.deliver(screen, env, res, clipboard, target, &Event::PointerUp { pos, button, mods }).0;
                match press {
                    Some(p) if p.dragging => {
                        let end = Event::DragEnd { pos, mods, cancelled: false };
                        reply = merge(reply, self.deliver(screen, env, res, clipboard, target, &end).0);
                    }
                    _ if pressed.is_some() && pressed == up_hit => {
                        let clicks = press.map_or(1, |p| p.clicks);
                        let click = Event::Click { pos, button, clicks, mods };
                        reply = merge(reply, self.deliver(screen, env, res, clipboard, pressed, &click).0);
                    }
                    _ => {}
                }
                reply
            }
            Input::Leave => {
                toast::on_hover(&mut self.toasts, None, env.now_ms);
                tooltip::on_hover(&mut self.tips, None, env.now_ms, &mut self.effects);
                self.state.last_pointer = None;
                self.set_hot(None, &env);
                self.deliver(screen, env, res, clipboard, None, &Event::PointerLeave).0
            }
            Input::Wheel(pos, delta, horizontal, mods) => {
                tooltip::on_input(&mut self.tips, TipInput::Wheel, &mut self.effects);
                let target = self.frame.hit(pos).map(|n| n.id);
                self.deliver(screen, env, res, clipboard, target, &Event::Wheel { pos, delta, horizontal, mods }).0
            }
            Input::Key(key, down, repeat, mods) => self.key(screen, env, res, clipboard, key, down, repeat, mods),
            Input::Char(c) => {
                let target = self.state.focused;
                self.deliver(screen, env, res, clipboard, target, &Event::Text(c)).0
            }
            Input::Ime(e) => {
                let target = self.state.focused;
                self.deliver(screen, env, res, clipboard, target, &Event::Ime(e)).0
            }
            Input::Active(on) => {
                let mut closed = Reply::None;
                if !on {
                    menu::close_all(&mut self.menus, &mut self.effects);
                    closed = self.menu_events(screen, env, res, clipboard);
                    tooltip::on_input(&mut self.tips, TipInput::Deactivate, &mut self.effects);
                }
                self.state.window_active = on;
                self.damage_node(self.state.focused, &env);
                let ev = Event::WindowActive(on);
                merge(closed, self.deliver(screen, env, res, clipboard, self.state.focused, &ev).0)
            }
            Input::Close => self.deliver(screen, env, res, clipboard, None, &Event::CloseRequested).0,
            Input::Timer(id) => {
                self.timers.retain(|t| t.0 != id);
                let target = if id.is_kit() { Some(kit_target((id.0 >> 8) & 0x7F_FFFF)) } else { None };
                self.deliver(screen, env, res, clipboard, target, &Event::Timer(id)).0
            }
            Input::A11y(node, action) => {
                self.deliver(screen, env, res, clipboard, Some(node), &Event::A11y(node, action)).0
            }
            Input::Data(msg) => {
                let target = self.state.focused;
                let mut cx = EventCx::new(env, self, res, clipboard, target);
                screen.data(&mut cx, msg)
            }
            Input::CopyData(tag, bytes) => {
                let target = self.state.focused;
                let mut cx = EventCx::new(env, self, res, clipboard, target);
                let taken = screen.copy_data(&mut cx, tag, &bytes);
                self.copied = taken;
                Reply::None
            }
            Input::Routed(msg, wparam, lparam) => {
                let target = self.state.focused;
                let mut cx = EventCx::new(env, self, res, clipboard, target);
                screen.app_message(&mut cx, msg, wparam, lparam)
            }
        }
    }

    fn pointer_move(
        &mut self,
        screen: &mut dyn Screen,
        env: Env<'_>,
        res: &mut Res,
        clipboard: &mut dyn Clipboard,
        pos: PointP,
        mods: Mods,
    ) -> Reply {
        self.state.last_pointer = Some(pos);
        toast::on_hover(&mut self.toasts, Some(pos), env.now_ms);
        let hit = self.frame.hit(pos).cloned();
        tooltip::on_hover(&mut self.tips, hit.as_ref(), env.now_ms, &mut self.effects);
        self.set_hot(hit.map(|n| n.id), &env);
        let target = self.state.captured.or(self.state.hot);
        let mut reply = self.deliver(screen, env, res, clipboard, target, &Event::PointerMove { pos, mods }).0;
        // A drag begins once the pointer has moved DRAG_THRESHOLD from where it went down.
        if let Some(mut p) = self.state.press {
            let thr = i64::from(env.scale.px(env.tokens.size.drag_threshold));
            let (dx, dy) = (i64::from(pos.x - p.origin.x), i64::from(pos.y - p.origin.y));
            if p.dragging {
                reply =
                    merge(reply, self.deliver(screen, env, res, clipboard, target, &Event::DragMove { pos, mods }).0);
            } else if dx * dx + dy * dy >= thr * thr {
                p.dragging = true;
                self.state.press = Some(p);
                let start = Event::DragStart { origin: p.origin, pos, button: p.button, mods };
                reply = merge(reply, self.deliver(screen, env, res, clipboard, target, &start).0);
            }
        }
        reply
    }

    #[allow(clippy::too_many_arguments)]
    fn key(
        &mut self,
        screen: &mut dyn Screen,
        env: Env<'_>,
        res: &mut Res,
        clipboard: &mut dyn Clipboard,
        key: Key,
        down: bool,
        repeat: bool,
        mods: Mods,
    ) -> Reply {
        tooltip::on_input(&mut self.tips, TipInput::Key, &mut self.effects);
        if down && key == Key::F6 && toast::on_f6(&mut self.toasts) {
            self.effects.push(Effect::Redraw);
            return Reply::None;
        }
        // A toast that holds the toast focus takes Enter (its action) and Esc (dismiss).
        if down {
            if let Some(action) = toast::on_key(&mut self.toasts, key) {
                self.effects.push(Effect::Relayout);
                return match action {
                    Some(id) => self.command(screen, env, res, clipboard, id),
                    None => Reply::None,
                };
            }
        }
        // While a menu (not a list popup) is open the keys go to the menu stack first.
        if menu::is_open(&self.menus) && menu::on_key(&mut self.menus, key, mods, down, &mut self.effects) {
            return self.menu_events(screen, env, res, clipboard);
        }
        if down && is_navigation_key(key) && !self.state.focus_visible {
            self.state.focus_visible = true;
            self.damage_node(self.state.focused, &env);
        }
        // Esc cancels a drag in progress.
        if down && key == Key::ESC && self.state.press.is_some_and(|p| p.dragging) {
            let target = self.state.captured.take();
            self.state.press = None;
            self.state.pressed = None;
            let pos = self.state.last_pointer.unwrap_or_default();
            return self.deliver(screen, env, res, clipboard, target, &Event::DragEnd { pos, mods, cancelled: true }).0;
        }
        let target = self.state.focused;
        let (reply, consumed) =
            self.deliver(screen, env, res, clipboard, target, &Event::Key { key, down, repeat, mods });
        if consumed || !down {
            return reply;
        }
        // Unconsumed: the screen's chords (a disabled command swallows its chord), then the kit's own keys.
        match screen.commands().by_chord(crate::command::Chord::new(mods, key)).map(|c| (c.id, c.enabled)) {
            Some((id, true)) => return self.command(screen, env, res, clipboard, id),
            Some((_, false)) => return reply,
            None => {}
        }
        match key {
            Key::TAB if !mods.ctrl() && !mods.alt() => {
                self.move_focus(screen, env, res, clipboard, !mods.shift());
                reply
            }
            Key::F6 if !mods.ctrl() && !mods.alt() => {
                self.next_pane(screen, env, res, clipboard, !mods.shift());
                reply
            }
            Key::F10 if mods.shift() && !mods.ctrl() && !mods.alt() => {
                self.context_click(screen, env, res, clipboard, mods)
            }
            Key::APPS => self.context_click(screen, env, res, clipboard, mods),
            Key(0x57) if mods.ctrl() && !mods.alt() => {
                self.deliver(screen, env, res, clipboard, None, &Event::CloseRequested).0
            }
            Key::F4 if mods.alt() => self.deliver(screen, env, res, clipboard, None, &Event::CloseRequested).0,
            Key::ESC if self.esc_closes => self.deliver(screen, env, res, clipboard, None, &Event::CloseRequested).0,
            _ => reply,
        }
    }

    /// Shift+F10 and the Menu key: a right click at the focused node's centre.
    fn context_click(
        &mut self,
        screen: &mut dyn Screen,
        env: Env<'_>,
        res: &mut Res,
        clipboard: &mut dyn Clipboard,
        mods: Mods,
    ) -> Reply {
        let Some(node) = self.state.focused.and_then(|f| self.frame.get(f)) else { return Reply::None };
        let r = node.rect;
        let pos = PointP { x: (r.l + r.r) / 2, y: (r.t + r.b) / 2 };
        let target = Some(node.id);
        let click = Event::Click { pos, button: MouseButton::Right, clicks: 1, mods };
        self.deliver(screen, env, res, clipboard, target, &click).0
    }

    /// The Tab stops in frame order: Tab nodes, and a Roving node only while it holds the focus.
    fn tab_stops(&self) -> Vec<NodeId> {
        self.frame
            .iter()
            .filter(|n| !n.flags.contains(NodeFlags::DISABLED))
            .filter(|n| n.focus == Focusable::Tab || (n.focus == Focusable::Roving && Some(n.id) == self.state.focused))
            .map(|n| n.id)
            .collect()
    }

    /// Tab and Shift+Tab: the next or previous stop, wrapping.
    fn move_focus(
        &mut self,
        screen: &mut dyn Screen,
        env: Env<'_>,
        res: &mut Res,
        clipboard: &mut dyn Clipboard,
        forward: bool,
    ) {
        let stops = self.tab_stops();
        if stops.is_empty() {
            return;
        }
        let n = stops.len();
        let to = match self.state.focused.and_then(|f| stops.iter().position(|s| *s == f)) {
            None if forward => 0,
            None => n - 1,
            Some(i) if forward => (i + 1) % n,
            Some(i) => (i + n - 1) % n,
        };
        self.set_focus(screen, env, res, clipboard, Some(stops[to]), FocusReason::Keyboard);
    }

    /// F6 and Shift+F6: the next or previous Pane node after the one holding the focus.
    fn next_pane(
        &mut self,
        screen: &mut dyn Screen,
        env: Env<'_>,
        res: &mut Res,
        clipboard: &mut dyn Clipboard,
        forward: bool,
    ) {
        let panes: Vec<NodeId> = self.frame.iter().filter(|n| n.role == Role::Pane).map(|n| n.id).collect();
        if panes.is_empty() {
            return;
        }
        // The pane the focus is in: the focused node itself or its nearest Pane ancestor.
        let mut cur = self.state.focused;
        let mut here = None;
        while let Some(id) = cur {
            if panes.contains(&id) {
                here = Some(id);
                break;
            }
            cur = self.frame.get(id).and_then(|n| n.parent);
        }
        let n = panes.len();
        let to = match here.and_then(|h| panes.iter().position(|p| *p == h)) {
            None if forward => 0,
            None => n - 1,
            Some(i) if forward => (i + 1) % n,
            Some(i) => (i + n - 1) % n,
        };
        self.set_focus(screen, env, res, clipboard, Some(panes[to]), FocusReason::Keyboard);
    }

    /// Sends Event::Command(id) to the screen: a kit id goes to its owner's node 0, an app id to the focused node.
    pub(crate) fn command(
        &mut self,
        screen: &mut dyn Screen,
        env: Env<'_>,
        res: &mut Res,
        clipboard: &mut dyn Clipboard,
        id: CommandId,
    ) -> Reply {
        let target = if id.is_kit() { Some(kit_target((id.0 >> 16) & 0x7FFF)) } else { self.state.focused };
        self.deliver(screen, env, res, clipboard, target, &Event::Command(id)).0
    }

    /// The menus' outcomes (a pick, a list pick, a close) to the screen in order, each to its target (the menu's opener,
    /// a kit id's owner; the focused node when there is none), right after the input that caused them (K2a.9). The host
    /// calls it too after a click on a popup.
    pub(crate) fn menu_events(
        &mut self,
        screen: &mut dyn Screen,
        env: Env<'_>,
        res: &mut Res,
        clipboard: &mut dyn Clipboard,
    ) -> Reply {
        let mut reply = Reply::None;
        for (target, ev) in menu::take_events(&mut self.menus) {
            let target = target.or(self.state.focused);
            reply = merge(reply, self.deliver(screen, env, res, clipboard, target, &ev).0);
        }
        reply
    }

    /// Moves the focus (FocusOut, then FocusIn with the reason) and records Effect::FocusChanged, which the host hands
    /// to hooks::on_focus_changed. A keyboard move shows the ring, a pointer move hides it.
    pub(crate) fn set_focus(
        &mut self,
        screen: &mut dyn Screen,
        env: Env<'_>,
        res: &mut Res,
        clipboard: &mut dyn Clipboard,
        to: Option<NodeId>,
        reason: FocusReason,
    ) {
        match reason {
            FocusReason::Keyboard => self.state.focus_visible = true,
            FocusReason::Pointer => self.state.focus_visible = false,
            FocusReason::Programmatic | FocusReason::WindowActivated => {}
        }
        if self.state.focused == to {
            self.damage_node(to, &env);
            return;
        }
        let from = self.state.focused;
        self.damage_node(from, &env);
        if from.is_some() {
            self.deliver(screen, env, res, clipboard, from, &Event::FocusOut);
        }
        self.state.focused = to;
        self.damage_node(to, &env);
        if to.is_some() {
            self.deliver(screen, env, res, clipboard, to, &Event::FocusIn(reason));
        }
        self.effects.push(Effect::FocusChanged(to));
    }

    /// One event to the screen with `target`; returns its reply and whether a widget consumed it.
    fn deliver(
        &mut self,
        screen: &mut dyn Screen,
        env: Env<'_>,
        res: &mut Res,
        clipboard: &mut dyn Clipboard,
        target: Option<NodeId>,
        ev: &Event,
    ) -> (Reply, bool) {
        let mut cx = EventCx::new(env, self, res, clipboard, target);
        let reply = screen.event(&mut cx, ev);
        (reply, cx.consumed())
    }
}

/// The first reply that asks for something.
fn merge(a: Reply, b: Reply) -> Reply {
    if a == Reply::None {
        b
    } else {
        a
    }
}

/// Keys that show the focus ring: Tab, the arrows, F6, Home, End, PgUp and PgDn.
fn is_navigation_key(k: Key) -> bool {
    matches!(
        k,
        Key::TAB
            | Key::LEFT
            | Key::UP
            | Key::RIGHT
            | Key::DOWN
            | Key::F6
            | Key::HOME
            | Key::END
            | Key::PAGE_UP
            | Key::PAGE_DOWN
    )
}

/// The node a kit id is delivered to: its owner's node 0.
fn kit_target(owner: u32) -> NodeId {
    NodeId::new(crate::node::WidgetId(owner), 0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::command::{Chord, Command as Cmd, CommandTable};
    use crate::cx::{EventCx, LayoutCx, PaintCx};
    use crate::geom::{PointL, SizeL};
    use crate::harness::Harness;
    use crate::node::{Node, WidgetId};
    use crate::theme::Tokens;

    type TestNode = ((i32, i32, i32, i32), Focusable, Role);

    /// Three buttons-like nodes (or the given ones) and a log of what reached them.
    struct TestScreen {
        cmds: CommandTable,
        log: Vec<String>,
        /// Logical x, y, w, h, focus and role of each node, pushed in this order.
        nodes: Vec<TestNode>,
        fade: bool,
        consume: Option<Key>,
        click_focuses: Option<u32>,
        arm_timers: bool,
        shift: i32,
    }

    impl TestScreen {
        fn new() -> TestScreen {
            let nodes = (0..3).map(|i| ((10 + 60 * i, 10, 50, 28), Focusable::Tab, Role::Button)).collect();
            TestScreen {
                cmds: CommandTable::new(),
                log: Vec::new(),
                nodes,
                fade: false,
                consume: None,
                click_focuses: None,
                arm_timers: false,
                shift: 0,
            }
        }
    }

    impl Screen for TestScreen {
        fn layout(&mut self, cx: &mut LayoutCx, avail: SizeP) -> SizeP {
            for (i, ((x, y, w, h), focus, role)) in self.nodes.iter().enumerate() {
                let r = RectP::wh(cx.px(x + self.shift), cx.px(*y), cx.px(*w), cx.px(*h));
                let mut n = Node::new(NodeId::new(WidgetId(i as u32 + 1), 0), *role, r).focus(*focus);
                if self.fade {
                    n = n.flags(NodeFlags::HOVER_FADE);
                }
                cx.push(n);
            }
            avail
        }

        fn paint(&self, _cx: &mut PaintCx) {}

        fn event(&mut self, cx: &mut EventCx, ev: &Event) -> Reply {
            let t = cx.target().map_or(0, |t| t.owner().0);
            match ev {
                Event::Click { button, pos, .. } => {
                    self.log.push(format!("{t}:click-{button:?}@{},{}", pos.x, pos.y));
                    if let Some(to) = self.click_focuses {
                        cx.set_focus(NodeId::new(WidgetId(to), 0));
                        self.log.push(format!("focused-now:{:?}", cx.focused().map(|n| n.owner().0)));
                    }
                    if self.arm_timers {
                        cx.set_timer(TimerId(2), 50);
                        cx.set_timer(TimerId(1), 20);
                    }
                }
                Event::DragStart { .. } => self.log.push(format!("{t}:dragstart")),
                Event::DragEnd { cancelled, .. } => self.log.push(format!("{t}:dragend-{cancelled}")),
                Event::FocusIn(r) => self.log.push(format!("{t}:in-{r:?}")),
                Event::FocusOut => self.log.push(format!("{t}:out")),
                Event::Command(id) => self.log.push(format!("{t}:cmd{:#x}", id.0)),
                Event::Timer(id) => self.log.push(format!("{t}:timer{:#x}", id.0)),
                Event::CloseRequested => self.log.push("close".to_string()),
                Event::Key { key, down: true, .. } => {
                    self.log.push(format!("{t}:key{:#x}", key.0));
                    if self.consume == Some(*key) {
                        cx.consume();
                    }
                }
                _ => {}
            }
            Reply::None
        }

        fn commands(&self) -> &CommandTable {
            &self.cmds
        }
    }

    fn harness(s: TestScreen) -> Harness<TestScreen> {
        Harness::new(s, SizeL { w: 260, h: 60 }, 96, Tokens::oled())
    }

    fn centre(i: i32) -> PointL {
        PointL { x: 35 + 60 * i, y: 24 }
    }

    fn pt(i: i32) -> PointP {
        PointP { x: 35 + 60 * i, y: 24 }
    }

    fn focused(h: &mut Harness<TestScreen>) -> Option<u32> {
        h.focused().map(|n| n.owner().0)
    }

    fn command(id: u32, chord: &str) -> Cmd {
        Cmd::new(CommandId(id), Text::from("c")).chord(Chord::parse(chord).unwrap())
    }

    #[test]
    fn hit_is_topmost() {
        let mut s = TestScreen::new();
        s.nodes =
            vec![((10, 10, 100, 40), Focusable::Tab, Role::Pane), ((30, 15, 40, 20), Focusable::Tab, Role::Button)];
        let mut h = harness(s);
        h.click(PointL { x: 40, y: 20 });
        h.click(PointL { x: 15, y: 40 });
        let clicks: Vec<_> = h.screen().log.iter().filter(|l| l.contains("click")).cloned().collect();
        assert_eq!(clicks, ["2:click-Left@40,20", "1:click-Left@15,40"]);
    }

    #[test]
    fn click_needs_down_and_up_on_the_same_node() {
        let mut h = harness(TestScreen::new());
        h.input(Input::Down(pt(0), MouseButton::Left, 1, Mods::NONE));
        h.input(Input::Up(pt(1), MouseButton::Left, Mods::NONE));
        assert!(!h.screen().log.iter().any(|l| l.contains("click")), "{:?}", h.screen().log);
        h.click(centre(1));
        assert!(h.screen().log.iter().any(|l| l.starts_with("2:click-Left")));
    }

    #[test]
    fn drag_starts_after_threshold() {
        let mut h = harness(TestScreen::new());
        let p = pt(0);
        h.input(Input::Move(p, Mods::NONE));
        h.input(Input::Down(p, MouseButton::Left, 1, Mods::NONE));
        h.input(Input::Move(PointP { x: p.x + 3, y: p.y }, Mods::NONE));
        assert!(!h.screen().log.iter().any(|l| l.contains("drag")));
        h.input(Input::Move(PointP { x: p.x + 8, y: p.y }, Mods::NONE));
        h.input(Input::Move(PointP { x: p.x + 12, y: p.y }, Mods::NONE));
        h.input(Input::Up(PointP { x: p.x + 12, y: p.y }, MouseButton::Left, Mods::NONE));
        let log: Vec<_> =
            h.screen().log.iter().filter(|l| l.contains("drag") || l.contains("click")).cloned().collect();
        assert_eq!(log, ["1:dragstart", "1:dragend-false"], "a drag is not a click");
    }

    #[test]
    fn tab_order_is_node_order_and_wraps() {
        let mut h = harness(TestScreen::new());
        let order: Vec<_> = (0..5)
            .map(|_| {
                h.key("Tab");
                focused(&mut h)
            })
            .collect();
        assert_eq!(order, [Some(1), Some(2), Some(3), Some(1), Some(2)]);
        h.key("Shift+Tab");
        assert_eq!(focused(&mut h), Some(1));
        h.key("Shift+Tab");
        assert_eq!(focused(&mut h), Some(3), "backwards wraps to the last");
    }

    #[test]
    fn roving_counts_once() {
        let mut s = TestScreen::new();
        s.nodes = vec![
            ((10, 10, 20, 20), Focusable::Tab, Role::Button),
            ((40, 10, 20, 20), Focusable::Roving, Role::ListItem),
            ((70, 10, 20, 20), Focusable::Roving, Role::ListItem),
            ((100, 10, 20, 20), Focusable::Roving, Role::ListItem),
            ((130, 10, 20, 20), Focusable::Tab, Role::Button),
        ];
        let mut h = harness(s);
        h.click(PointL { x: 80, y: 20 }); // focuses the second roving item
        assert_eq!(focused(&mut h), Some(3));
        h.key("Tab");
        assert_eq!(focused(&mut h), Some(5), "the group is one stop");
        h.key("Shift+Tab");
        assert_eq!(focused(&mut h), Some(1), "an unfocused roving node is not a stop");
    }

    #[test]
    fn press_hides_ring_keys_show_it() {
        let mut h = harness(TestScreen::new());
        h.key("Tab");
        assert!(h.dispatcher().state.focus_visible);
        h.click(centre(1));
        assert!(!h.dispatcher().state.focus_visible);
        h.key("Right");
        assert!(h.dispatcher().state.focus_visible);
        h.click(centre(2));
        h.key("a");
        assert!(!h.dispatcher().state.focus_visible, "a letter does not show the ring");
    }

    #[test]
    fn unconsumed_chord_runs_command() {
        let mut s = TestScreen::new();
        s.cmds.add(command(5, "Ctrl+K")).unwrap();
        let mut h = harness(s);
        h.key("Tab").key("Ctrl+K");
        assert!(h.screen().log.iter().any(|l| l == "1:cmd0x5"), "{:?}", h.screen().log);
    }

    #[test]
    fn disabled_command_swallows_its_chord() {
        let mut s = TestScreen::new();
        s.cmds.add(command(5, "Tab")).unwrap();
        s.cmds.set_enabled(CommandId(5), false);
        let mut h = harness(s);
        h.key("Tab");
        assert_eq!(focused(&mut h), None, "the disabled command took Tab");
        assert!(!h.screen().log.iter().any(|l| l.contains("cmd")));
    }

    #[test]
    fn consumed_key_never_reaches_commands() {
        let mut s = TestScreen::new();
        s.cmds.add(command(7, "Enter")).unwrap();
        s.consume = Some(Key::ENTER);
        let mut h = harness(s);
        h.key("Tab").key("Enter");
        assert!(!h.screen().log.iter().any(|l| l.contains("cmd")), "{:?}", h.screen().log);
    }

    #[test]
    fn esc_ladder() {
        // The screen takes Esc first (a field cancels).
        let mut s = TestScreen::new();
        s.consume = Some(Key::ESC);
        let mut h = harness(s);
        h.dispatcher().esc_closes = true;
        h.key("Esc");
        assert!(!h.screen().log.iter().any(|l| l == "close"));
        // Nobody wants it: the window closes only when it was asked to.
        let mut h = harness(TestScreen::new());
        h.key("Esc");
        assert!(!h.screen().log.iter().any(|l| l == "close"));
        let mut h = harness(TestScreen::new());
        h.dispatcher().esc_closes = true;
        h.key("Esc");
        assert!(h.screen().log.iter().any(|l| l == "close"));
        // A drag in progress ends cancelled and keeps the window.
        let mut h = harness(TestScreen::new());
        h.dispatcher().esc_closes = true;
        let p = pt(0);
        h.input(Input::Down(p, MouseButton::Left, 1, Mods::NONE));
        h.input(Input::Move(PointP { x: p.x + 20, y: p.y }, Mods::NONE));
        h.key("Esc");
        assert!(h.screen().log.iter().any(|l| l == "1:dragend-true"));
        assert!(!h.screen().log.iter().any(|l| l == "close"));
    }

    #[test]
    fn ctrl_w_and_alt_f4_ask_to_close() {
        let mut h = harness(TestScreen::new());
        h.key("Ctrl+W");
        h.key("Alt+F4");
        assert_eq!(h.screen().log.iter().filter(|l| *l == "close").count(), 2);
    }

    #[test]
    fn shift_f10_opens_context_at_focus() {
        let mut h = harness(TestScreen::new());
        h.key("Tab").key("Tab").key("Shift+F10");
        assert!(h.screen().log.iter().any(|l| l == "2:click-Right@95,24"), "{:?}", h.screen().log);
    }

    #[test]
    fn effects_apply_after_the_event() {
        let mut s = TestScreen::new();
        s.click_focuses = Some(3);
        let mut h = harness(s);
        h.click(centre(0));
        let log = &h.screen().log;
        assert!(log.iter().any(|l| l == "focused-now:Some(1)"), "the handler still saw its own focus: {log:?}");
        assert_eq!(focused(&mut h), Some(3));
    }

    #[test]
    fn hover_fade_animates_and_ends_idle() {
        let mut s = TestScreen::new();
        s.fade = true;
        let mut h = harness(s);
        let fade = AnimId { node: NodeId::new(WidgetId(1), 0), channel: HOVER_CHANNEL };
        h.hover(centre(0));
        assert!(!h.is_idle(), "the hover tween is live");
        h.advance(45);
        let mid = h.dispatcher().anim.value(fade, 45).unwrap();
        assert!(mid > 0.0 && mid < 1.0, "{mid}");
        h.advance(200);
        assert!(h.is_idle());
        assert_eq!(h.dispatcher().anim.value(fade, 245), Some(1.0));
        h.input(Input::Leave);
        h.advance(200);
        assert!(h.is_idle());
        assert_eq!(h.dispatcher().anim.value(fade, 445), Some(0.0));
    }

    #[test]
    fn timers_fire_in_order() {
        let mut s = TestScreen::new();
        s.arm_timers = true;
        let mut h = harness(s);
        h.click(centre(0));
        h.advance(100);
        let timers: Vec<_> = h.screen().log.iter().filter(|l| l.contains("timer")).cloned().collect();
        assert_eq!(timers, ["0:timer0x1", "0:timer0x2"]);
    }

    #[test]
    fn hot_follows_the_pointer_after_relayout() {
        let mut h = harness(TestScreen::new());
        h.hover(centre(0));
        assert_eq!(h.dispatcher().state.hot, Some(NodeId::new(WidgetId(1), 0)));
        h.screen_mut().shift = -60; // the nodes slide one slot to the left
        h.set_tokens(Tokens::oled());
        assert_eq!(h.dispatcher().state.hot, Some(NodeId::new(WidgetId(2), 0)), "by position, not by id");
    }

    #[test]
    fn hover_fades_follow_the_pointer_after_relayout() {
        let mut s = TestScreen::new();
        s.fade = true;
        let mut h = harness(s);
        let ch = |i: u32| AnimId { node: NodeId::new(WidgetId(i), 0), channel: HOVER_CHANNEL };
        h.hover(centre(0));
        h.advance(200);
        assert_eq!(h.dispatcher().anim.value(ch(1), 200), Some(1.0));
        h.screen_mut().shift = -60;
        h.set_tokens(Tokens::oled());
        h.advance(200);
        assert_eq!(h.dispatcher().anim.value(ch(1), 400), Some(0.0), "the old node fades out");
        assert_eq!(h.dispatcher().anim.value(ch(2), 400), Some(1.0), "the new node fades in");
    }

    #[test]
    fn focus_survives_relayout_by_id_and_drops_with_its_node() {
        let mut h = harness(TestScreen::new());
        h.key("Tab").key("Tab");
        h.set_tokens(Tokens::oled());
        assert_eq!(focused(&mut h), Some(2));
        h.screen_mut().nodes.truncate(1);
        h.set_tokens(Tokens::oled());
        assert_eq!(focused(&mut h), None);
    }

    #[test]
    fn kit_ids_reach_their_owner() {
        let mut h = harness(TestScreen::new());
        h.input(Input::Timer(TimerId::kit(WidgetId(7), 1)));
        h.command(CommandId::kit(WidgetId(9), 3));
        let log = &h.screen().log;
        assert!(log.iter().any(|l| l == "7:timer0x80000701"), "{log:?}");
        assert!(log.iter().any(|l| l == "9:cmd0x80090003"), "{log:?}");
    }

    #[test]
    fn focus_moves_send_out_then_in_with_the_reason() {
        let mut h = harness(TestScreen::new());
        h.key("Tab").key("Tab");
        h.click(centre(2));
        let log: Vec<_> = h.screen().log.iter().filter(|l| l.contains("in-") || l.contains("out")).cloned().collect();
        assert_eq!(log, ["1:in-Keyboard", "1:out", "2:in-Keyboard", "2:out", "3:in-Pointer"]);
    }
}
