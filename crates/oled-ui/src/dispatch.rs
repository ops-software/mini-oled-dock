//! The dispatcher: the one routing core of the Harness and the real host, so window behaviour is tested headless
//! (P2-kit 2.13, K1a.4). K0.1b writes the skeleton: the complete Input and Effect enums, the state it holds and every
//! hook call of 2.13 at its point; K1a.4 (KA1) writes the routing rules of SPEC-platform 5.3.5 around those calls.

use crate::anim::AnimClock;
use crate::command::CommandId;
use crate::cx::{Anchor, AnimId, Clipboard, Ease, Env, EventCx, LayoutCx, MenuId, MenuItem, Res};
use crate::event::{A11yAction, Event, FocusReason, ImeEvent, Key, Mods, MouseButton, Reply, TimerId, WheelDelta};
use crate::geom::{PointP, RectP, SizeP};
use crate::host::toast::{self, ToastStack};
use crate::host::AppMsg;
use crate::node::{Frame, NodeId, Text};
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
            menu_ids: 0,
        }
    }

    /// A new menu id (never 0).
    pub(crate) fn next_menu_id(&mut self) -> MenuId {
        self.menu_ids = self.menu_ids.wrapping_add(1).max(1);
        MenuId(self.menu_ids)
    }

    /// Lays the screen out: the frame is rebuilt from what the screen pushes.
    pub(crate) fn layout(&mut self, screen: &mut dyn Screen, env: Env<'_>, res: &mut Res, avail: SizeP) -> SizeP {
        self.frame.clear();
        let mut cx = LayoutCx::new(env, self, res);
        screen.layout(&mut cx, avail)
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

    /// Routes one input to the screen (SPEC-platform 5.3.5; the rules are K1a.4's). Returns the screen's reply; the
    /// effects wait in `effects` for the caller.
    pub(crate) fn input(
        &mut self,
        screen: &mut dyn Screen,
        inp: Input,
        env: Env<'_>,
        res: &mut Res,
        clipboard: &mut dyn Clipboard,
    ) -> Reply {
        match inp {
            Input::Move(pos, mods) => {
                let hit = self.frame.hit(pos).cloned();
                tooltip::on_hover(&mut self.tips, hit.as_ref(), env.now_ms, &mut self.effects);
                self.state.hot = hit.map(|n| n.id);
                let target = self.state.captured.or(self.state.hot);
                self.deliver(screen, env, res, clipboard, target, &Event::PointerMove { pos, mods }).0
            }
            Input::Down(pos, button, clicks, mods) => {
                if toast::hit(&mut self.toasts, pos).is_some() {
                    return Reply::None;
                }
                if menu::on_pointer_down(&mut self.menus, pos, &mut self.effects) == MenuPress::Swallow {
                    return Reply::None;
                }
                tooltip::on_input(&mut self.tips, TipInput::Press, &mut self.effects);
                let target = self.frame.hit(pos).map(|n| n.id);
                self.state.pressed = target;
                self.state.focus_visible = false;
                self.deliver(screen, env, res, clipboard, target, &Event::PointerDown { pos, button, clicks, mods }).0
            }
            Input::Up(pos, button, mods) => {
                let target = self.state.captured.or_else(|| self.frame.hit(pos).map(|n| n.id));
                let pressed = self.state.pressed.take();
                let reply =
                    self.deliver(screen, env, res, clipboard, target, &Event::PointerUp { pos, button, mods }).0;
                if pressed.is_some() && pressed == target {
                    let click = Event::Click { pos, button, clicks: 1, mods };
                    return self.deliver(screen, env, res, clipboard, target, &click).0;
                }
                reply
            }
            Input::Leave => {
                tooltip::on_hover(&mut self.tips, None, env.now_ms, &mut self.effects);
                self.state.hot = None;
                self.deliver(screen, env, res, clipboard, None, &Event::PointerLeave).0
            }
            Input::Wheel(pos, delta, horizontal, mods) => {
                tooltip::on_input(&mut self.tips, TipInput::Wheel, &mut self.effects);
                let target = self.frame.hit(pos).map(|n| n.id);
                self.deliver(screen, env, res, clipboard, target, &Event::Wheel { pos, delta, horizontal, mods }).0
            }
            Input::Key(key, down, repeat, mods) => {
                tooltip::on_input(&mut self.tips, TipInput::Key, &mut self.effects);
                if down && key == Key::F6 && toast::on_f6(&mut self.toasts) {
                    return Reply::None;
                }
                if menu::is_open(&self.menus) && menu::on_key(&mut self.menus, key, mods, down, &mut self.effects) {
                    return Reply::None;
                }
                let target = self.state.focused;
                let (reply, consumed) =
                    self.deliver(screen, env, res, clipboard, target, &Event::Key { key, down, repeat, mods });
                if consumed || !down {
                    return reply;
                }
                // Unconsumed: the screen's chords (a disabled command swallows its chord). Tab, F6, Shift+F10, Ctrl+W,
                // Alt+F4 and the Esc ladder: K1a.4.
                let chord = crate::command::Chord::new(mods, key);
                match screen.commands().by_chord(chord).map(|c| (c.id, c.enabled)) {
                    Some((id, true)) => self.command(screen, env, res, clipboard, id),
                    _ => reply,
                }
            }
            Input::Char(c) => {
                let target = self.state.focused;
                self.deliver(screen, env, res, clipboard, target, &Event::Text(c)).0
            }
            Input::Ime(e) => {
                let target = self.state.focused;
                self.deliver(screen, env, res, clipboard, target, &Event::Ime(e)).0
            }
            Input::Active(on) => {
                if !on {
                    menu::close_all(&mut self.menus, &mut self.effects);
                    tooltip::on_input(&mut self.tips, TipInput::Deactivate, &mut self.effects);
                }
                self.state.window_active = on;
                let ev = Event::WindowActive(on);
                self.deliver(screen, env, res, clipboard, self.state.focused, &ev).0
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
        }
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

    /// Moves the focus (FocusOut, then FocusIn with the reason) and records Effect::FocusChanged. Filled by K1a.4 with
    /// focus_visible and the hooks::on_focus_changed hand-off.
    pub(crate) fn set_focus(
        &mut self,
        screen: &mut dyn Screen,
        env: Env<'_>,
        res: &mut Res,
        clipboard: &mut dyn Clipboard,
        to: Option<NodeId>,
        reason: FocusReason,
    ) {
        if self.state.focused == to {
            return;
        }
        let from = self.state.focused;
        if from.is_some() {
            self.deliver(screen, env, res, clipboard, from, &Event::FocusOut);
        }
        self.state.focused = to;
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

/// The node a kit id is delivered to: its owner's node 0.
fn kit_target(owner: u32) -> NodeId {
    NodeId::new(crate::node::WidgetId(owner), 0)
}
