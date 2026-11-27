//! The tooltip and name-chip window: one layered, click-through window per EventLoop, of class "MiniOledTip", owned by
//! the window whose node it names, never activated, never focused and without a title. It shows the chip paint_chip
//! draws.
//!
//! The dispatcher's TipState decides when a tip shows and hides (widgets/tooltip.rs: the delay, the warm rule, the
//! press, key, wheel and deactivation); its ShowTip and HideTip end here. show() measures and paints the chip at the
//! owner's DPI with the owner's text engine, places it against the node and the work area of the node's monitor, and
//! leaves the window calls (the window made on first use, UpdateLayeredWindowIndirect, a show without activation) for
//! after the owner's borrow; hide() leaves SW_HIDE. after_layout() takes the tip away when the pointer is no longer on
//! its node and moves it (or draws it again at a new DPI or with new tokens) with its node. No timer of its own: the
//! delay is the dispatcher's deadline, armed only while a tip is pending.

use std::rc::Rc;

use windows::core::{HSTRING, PCWSTR};
use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, POINT, RECT, WPARAM};
use windows::Win32::Graphics::Gdi::{
    ClientToScreen, GetMonitorInfoW, MonitorFromRect, MONITORINFO, MONITOR_DEFAULTTONEAREST,
};
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, DestroyWindow, GetClassInfoExW, IsWindow, RegisterClassExW, ShowWindow,
    SHOW_WINDOW_CMD, SW_HIDE, SW_SHOWNOACTIVATE, WINDOW_EX_STYLE, WM_DPICHANGED, WM_ENABLE, WM_MOUSEACTIVATE,
    WM_NCHITTEST, WM_SHOWWINDOW, WM_SIZE, WNDCLASSEXW, WS_EX_LAYERED, WS_EX_NOACTIVATE, WS_EX_TOOLWINDOW,
    WS_EX_TRANSPARENT, WS_POPUP,
};

use super::run::LoopShared;
use super::window::{self, Sys, WinHost};
use crate::cx::{Env, LayoutCx, PaintCx, Res};
use crate::dispatch::{Dispatcher, Effect};
use crate::geom::{PointP, RectP, SizeP};
use crate::node::{NodeId, Text};
use crate::present::layered::{self, LayeredPresenter, Present};
use crate::present::Damage;
use crate::theme::Tokens;
use crate::widgets::tooltip::{self as tips, chip_size, paint_chip, TipInput};

/// The window class of the chip.
pub(crate) const CLASS: &str = "MiniOledTip";

/// The chip's distance below (or above) the node, logical px.
pub(crate) const GAP: i32 = 8;

/// The tip window's extended styles: layered, click-through, never activated, no taskbar button.
pub(crate) const EX_STYLE: WINDOW_EX_STYLE =
    WINDOW_EX_STYLE(WS_EX_LAYERED.0 | WS_EX_TRANSPARENT.0 | WS_EX_NOACTIVATE.0 | WS_EX_TOOLWINDOW.0);

/// How the tip window is shown: without activation, so the owner keeps the focus.
pub(crate) const SHOW: SHOW_WINDOW_CMD = SW_SHOWNOACTIVATE;

/// WM_MOUSEACTIVATE's MA_NOACTIVATE and WM_NCHITTEST's HTTRANSPARENT.
const MA_NOACTIVATE: isize = 3;
const HTTRANSPARENT: isize = -1;

/// WM_SHOWWINDOW's lParam when Windows itself shows the owned windows of an owner that was restored or shown
/// (SW_PARENTOPENING), and WM_SIZE's SIZE_MINIMIZED.
const SW_PARENTOPENING: isize = 3;
const SIZE_MINIMIZED: usize = 1;

/// What the tip window answers to WM_SHOWWINDOW: true when it must stay hidden. The chip is the kit's alone: only the
/// kit shows it, for a node under the pointer now. Windows shows an owner's popups again when the owner is restored or
/// shown (SW_PARENTOPENING) behind the kit's back, and the chip would come back at its old place.
pub(crate) fn refuses_show(shown: bool, reason: isize) -> bool {
    shown && reason == SW_PARENTOPENING
}

/// Whether a message to the owner takes its chip away: minimised (WM_SIZE), hidden (WM_SHOWWINDOW with wParam 0) or
/// disabled (WM_ENABLE with wParam 0).
pub(crate) fn owner_message_hides_chip(msg: u32, wp: usize) -> bool {
    match msg {
        WM_SIZE => wp == SIZE_MINIMIZED,
        WM_SHOWWINDOW | WM_ENABLE => wp == 0,
        _ => false,
    }
}

/// The chip's top-left for a node: GAP below its rect and centred on it; flipped above when it would leave the work area at
/// the bottom, and clamped inside the work area horizontally (and at the top when neither side has room).
pub(crate) fn place(node: RectP, chip: SizeP, work: RectP, gap: i32) -> PointP {
    let x = ((node.l + node.r) / 2 - chip.w / 2).min(work.r - chip.w).max(work.l);
    let below = node.b + gap;
    let y = if below + chip.h <= work.b { below } else { node.t - gap - chip.h };
    PointP { x, y: y.max(work.t) }
}

/// Where the owner's client is on the screen and the work area of the monitor a screen rect is on.
pub(crate) trait Desk {
    fn client_origin(&self, owner: HWND) -> PointP;
    fn work_area(&self, r: RectP) -> RectP;
}

/// The real desktop.
struct Win32Desk;

impl Desk for Win32Desk {
    fn client_origin(&self, owner: HWND) -> PointP {
        let mut p = POINT::default();
        // SAFETY: our window; `p` is writable. ClientToScreen sends no message.
        let _ = unsafe { ClientToScreen(owner, &mut p) };
        PointP { x: p.x, y: p.y }
    }

    fn work_area(&self, r: RectP) -> RectP {
        let rc = RECT::from(r);
        // SAFETY: `rc` lives for the call; MONITOR_DEFAULTTONEAREST always gives a monitor.
        let mon = unsafe { MonitorFromRect(&rc, MONITOR_DEFAULTTONEAREST) };
        let mut mi = MONITORINFO { cbSize: std::mem::size_of::<MONITORINFO>() as u32, ..Default::default() };
        // SAFETY: `mi` is writable with cbSize set.
        if unsafe { GetMonitorInfoW(mon, &mut mi) }.as_bool() {
            mi.rcWork.into()
        } else {
            r
        }
    }
}

/// The owner window's parts the tip reads and draws with.
pub(crate) struct Owner<'a> {
    pub(crate) hwnd: HWND,
    pub(crate) env: Env<'a>,
    pub(crate) disp: &'a mut Dispatcher,
    pub(crate) res: &'a mut Res,
}

/// The tip on screen (or about to be).
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Shown {
    pub(crate) owner: HWND,
    pub(crate) node: NodeId,
    /// The node's rect in the owner's client, physical px.
    pub(crate) anchor: RectP,
    pub(crate) text: Text,
    pub(crate) chord: Option<Text>,
    /// The chip on the screen, physical px.
    pub(crate) rect: RectP,
    /// What it was drawn with: the owner's DPI and token generation.
    pub(crate) dpi: u32,
    pub(crate) generation: u64,
}

/// The window calls that make the tip window what the state says.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Plan {
    Nothing,
    Hide(HWND),
    /// Present the chip in the window made for `owner` (`window`, or a new one; `stale`: one made for another owner,
    /// destroyed first).
    Present {
        owner: HWND,
        window: Option<HWND>,
        stale: Option<HWND>,
    },
}

/// The tip of one UI thread.
pub(crate) struct Tip {
    /// The tip window and the owner it was made for.
    pub(crate) window: Option<(HWND, HWND)>,
    pub(crate) shown: Option<Shown>,
    presenter: LayeredPresenter,
    /// The present of the chip in the presenter's DIB (valid until the next paint or reset).
    painted: Option<Present>,
    /// The window shows `painted`.
    presented: bool,
    /// The window is visible.
    visible: bool,
}

impl Tip {
    pub(crate) fn new() -> Tip {
        Tip {
            window: None,
            shown: None,
            presenter: LayeredPresenter::new(),
            painted: None,
            presented: false,
            visible: false,
        }
    }

    /// ShowTip: the chip for `node` (`rect` in the owner's client) measured, placed and painted; true when the window
    /// must follow.
    pub(crate) fn show(
        &mut self,
        o: &mut Owner<'_>,
        desk: &dyn Desk,
        node: NodeId,
        rect: RectP,
        text: Text,
        chord: Option<Text>,
    ) -> bool {
        // The node may have moved since the pointer came to rest on it.
        let rect = o.disp.frame.get(node).map_or(rect, |n| n.rect);
        let size = chip_size(&mut LayoutCx::new(o.env, o.disp, o.res), &text, chord.as_deref());
        let origin = desk.client_origin(o.hwnd);
        let anchor = rect.offset(origin.x, origin.y);
        let at = place(anchor, size, desk.work_area(anchor), o.env.scale.px(GAP));
        let all = RectP::wh(0, 0, size.w, size.h);
        let mut damage = Damage::default();
        damage.add(all, all);
        let (env, disp, res) = (o.env, &*o.disp, &mut *o.res);
        let painted = self.presenter.render(size, &damage, &mut |canvas, origin, clip| {
            let mut cx = PaintCx::new(env, canvas, origin, clip, disp, res, None);
            paint_chip(&mut cx, PointP { x: 0, y: 0 }, &text, chord.as_deref());
        });
        self.painted = painted.map(|p| Present { pos: Some(at), ..p });
        self.presented = false;
        self.shown = self.painted.is_some().then(|| Shown {
            owner: o.hwnd,
            node,
            anchor: rect,
            text,
            chord,
            rect: RectP::wh(at.x, at.y, size.w, size.h),
            dpi: env.scale.dpi,
            generation: env.tokens.generation,
        });
        true
    }

    /// HideTip from `owner`: the chip goes if it is that window's; true when the window must follow.
    pub(crate) fn hide(&mut self, owner: HWND) -> bool {
        if self.shown.as_ref().is_none_or(|s| s.owner != owner) {
            return false;
        }
        self.shown = None;
        self.painted = None;
        true
    }

    /// ShowTip and HideTip; other effects are not the tip's.
    pub(crate) fn apply(&mut self, o: &mut Owner<'_>, desk: &dyn Desk, e: Effect) -> bool {
        match e {
            Effect::ShowTip { node, rect, text, chord } => self.show(o, desk, node, rect, text, chord),
            Effect::HideTip => self.hide(o.hwnd),
            _ => false,
        }
    }

    /// After a layout of the owner: the tip is resolved again from the pointer (it goes when the pointer is no longer
    /// on its node, and a pending one is cancelled), then the tip that stays follows its node: a move, a new DPI or new
    /// tokens draw and place it again. True when the window must follow.
    pub(crate) fn after_layout(&mut self, o: &mut Owner<'_>, desk: &dyn Desk) -> bool {
        let hit = o.disp.state.last_pointer.and_then(|p| o.disp.frame.hit(p)).cloned();
        let mut fx = Vec::new();
        tips::on_hover(&mut o.disp.tips, hit.as_ref(), o.env.now_ms, &mut fx);
        let mut changed = false;
        for e in fx {
            changed |= self.apply(o, desk, e);
        }
        let Some(s) = self.shown.as_ref().filter(|s| s.owner == o.hwnd) else { return changed };
        let Some(rect) = o.disp.frame.get(s.node).map(|n| n.rect) else { return self.hide(o.hwnd) || changed };
        if rect == s.anchor && s.dpi == o.env.scale.dpi && s.generation == o.env.tokens.generation {
            return changed;
        }
        let (node, text, chord) = (s.node, s.text.clone(), s.chord.clone());
        self.show(o, desk, node, rect, text, chord) || changed
    }

    /// The owner is destroyed: its chip goes, the pixels are freed when nothing else is shown, and the window made for
    /// it (if it still exists: Windows destroys owned windows before their owner) is returned to be destroyed.
    pub(crate) fn forget(&mut self, owner: HWND) -> Option<HWND> {
        if self.shown.as_ref().is_some_and(|s| s.owner == owner) {
            self.shown = None;
        }
        if self.shown.is_none() {
            self.painted = None;
            self.presenter.reset();
        }
        let (w, made_for) = self.window?;
        if made_for != owner {
            return None;
        }
        self.window = None;
        self.visible = false;
        Some(w)
    }

    /// What the window needs now.
    pub(crate) fn plan(&self) -> Plan {
        match (&self.shown, self.window) {
            (None, Some((w, _))) if self.visible => Plan::Hide(w),
            (None, _) => Plan::Nothing,
            (Some(s), Some((w, o))) if o == s.owner => {
                if self.presented {
                    Plan::Nothing
                } else {
                    Plan::Present { owner: s.owner, window: Some(w), stale: None }
                }
            }
            (Some(s), other) => Plan::Present { owner: s.owner, window: None, stale: other.map(|(w, _)| w) },
        }
    }

    /// The tip window was destroyed.
    fn window_gone(&mut self, hwnd: HWND) {
        if self.window.is_some_and(|(w, _)| w == hwnd) {
            self.window = None;
            self.visible = false;
            self.presented = false;
        }
    }

    /// The pixels painted for the chip, the DIB of the presenter (tests).
    #[cfg(test)]
    fn pixels(&mut self) -> (i32, i32, Vec<u32>) {
        let c = self.presenter.canvas();
        (c.w, c.h, c.buf.to_vec())
    }
}

/// Runs `f` on the loop's tip; None while it is in use (a window call made inside it).
fn with_tip<R>(lp: &LoopShared, f: impl FnOnce(&mut Tip) -> R) -> Option<R> {
    lp.tip.try_borrow_mut().ok().map(|mut t| f(&mut t))
}

/// The owner's parts, from its state.
fn owner<'a>(host: &'a mut WinHost, tokens: &'a Tokens) -> Owner<'a> {
    let env = host.env(tokens);
    Owner { hwnd: host.hwnd, env, disp: &mut host.disp, res: &mut host.res }
}

/// The window follows once the owner's borrow has ended.
fn follow_later(host: &mut WinHost) {
    let lp = Rc::clone(&host.lp);
    host.defer(Sys::Call(Box::new(move |_| sync(&lp))));
}

/// After every layout: the tip is resolved again from the pointer and follows its node; a tip the relayout made pending
/// arms the dispatcher's deadline.
pub(crate) fn after_layout(host: &mut WinHost) {
    let (lp, tokens) = (Rc::clone(&host.lp), host.tokens());
    if with_tip(&lp, |t| t.after_layout(&mut owner(host, &tokens), &Win32Desk)) == Some(true) {
        follow_later(host);
    }
    super::input::sync_deadline(host);
}

/// The owner is destroyed: the tip window goes too.
pub(crate) fn on_owner_destroy(host: &mut WinHost, hwnd: HWND) {
    if let Some(w) = with_tip(&Rc::clone(&host.lp), |t| t.forget(hwnd)).flatten() {
        host.defer(Sys::Call(Box::new(move |_| {
            // SAFETY: the tip window this thread made, destroyed with no state borrowed; IsWindow guards a window
            // Windows already destroyed with its owner.
            unsafe {
                if IsWindow(Some(w)).as_bool() {
                    let _ = DestroyWindow(w);
                }
            }
        })));
    }
}

/// Effect::ShowTip.
pub(crate) fn show(host: &mut WinHost, node: NodeId, rect: RectP, text: Text, chord: Option<Text>) {
    let (lp, tokens) = (Rc::clone(&host.lp), host.tokens());
    if with_tip(&lp, |t| t.show(&mut owner(host, &tokens), &Win32Desk, node, rect, text, chord)) == Some(true) {
        follow_later(host);
    }
}

/// The owner was minimised, hidden or disabled: the tip goes through the kit's own path (as a deactivation does: the node
/// it was for stays dismissed until the pointer leaves it, the pending one is cancelled), so that its state is right
/// when the owner is restored. Nothing is armed.
pub(crate) fn on_owner_away(host: &mut WinHost) {
    let mut fx = Vec::new();
    tips::on_input(&mut host.disp.tips, TipInput::Deactivate, &mut fx);
    host.disp.effects.extend(fx);
    host.apply_pending();
    // The state of the tip window itself, whatever the dispatcher knew.
    hide(host);
    super::input::sync_deadline(host);
}

/// A message to an owner window that may take its chip away (queued in order when the state is borrowed).
pub(crate) fn on_owner_message(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) {
    if owner_message_hides_chip(msg, wp.0) {
        let _ = window::handle(hwnd, msg, wp, lp, on_owner_away);
    }
}

/// Effect::HideTip.
pub(crate) fn hide(host: &mut WinHost) {
    let hwnd = host.hwnd;
    if with_tip(&host.lp, |t| t.hide(hwnd)) == Some(true) {
        follow_later(host);
    }
}

/// Makes the tip window what the state says. No state is borrowed: the calls send messages to the tip window.
fn sync(lp: &LoopShared) {
    let Some(plan) = with_tip(lp, |t| t.plan()) else { return };
    match plan {
        Plan::Nothing => {}
        Plan::Hide(w) => {
            // SAFETY: the tip window of this thread; hiding it activates nothing.
            let _ = unsafe { ShowWindow(w, SW_HIDE) };
            with_tip(lp, |t| t.visible = false);
        }
        Plan::Present { owner, window, stale } => {
            if let Some(old) = stale {
                with_tip(lp, |t| t.window_gone(old));
                // SAFETY: a tip window of this thread made for another owner; no state is borrowed.
                let _ = unsafe { DestroyWindow(old) };
            }
            // SAFETY: IsWindow reads no state.
            let w = match window.filter(|w| unsafe { IsWindow(Some(*w)) }.as_bool()) {
                Some(w) => w,
                None => {
                    let Some(w) = create(owner) else { return };
                    with_tip(lp, |t| t.window = Some((w, owner)));
                    w
                }
            };
            // Read just before the call, with nothing running in between: the DIB it points into is alive.
            let Some(p) = with_tip(lp, |t| t.painted).flatten() else { return };
            let _ = layered::present(w, &p);
            // SAFETY: the tip window of this thread; SHOW never activates it.
            let _ = unsafe { ShowWindow(w, SHOW) };
            with_tip(lp, |t| {
                t.presented = true;
                t.visible = true;
            });
        }
    }
}

/// The tip window for `owner` (its class registered on first use).
fn create(owner: HWND) -> Option<HWND> {
    let hinst = window::instance();
    let class = HSTRING::from(CLASS);
    let mut wc = WNDCLASSEXW { cbSize: std::mem::size_of::<WNDCLASSEXW>() as u32, ..Default::default() };
    // SAFETY: `wc` is a writable WNDCLASSEXW with cbSize set; the name outlives the call.
    if unsafe { GetClassInfoExW(Some(hinst), &class, &mut wc) }.is_err() {
        let wc = WNDCLASSEXW {
            cbSize: std::mem::size_of::<WNDCLASSEXW>() as u32,
            lpfnWndProc: Some(tip_proc),
            hInstance: hinst,
            lpszClassName: PCWSTR(class.as_ptr()),
            ..Default::default()
        };
        // SAFETY: `wc` is complete and its class name outlives the call. A failure (another thread registered it
        // meanwhile) shows in CreateWindowExW.
        let _ = unsafe { RegisterClassExW(&wc) };
    }
    // SAFETY: the class is registered with this instance; the owner is a window of this thread; no title.
    unsafe {
        CreateWindowExW(EX_STYLE, &class, PCWSTR::null(), WS_POPUP, 0, 0, 0, 0, Some(owner), None, Some(hinst), None)
    }
    .ok()
}

/// The tip window's procedure: never activated by a click, transparent to the pointer, its size its owner's choice.
unsafe extern "system" fn tip_proc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    match msg {
        WM_MOUSEACTIVATE => LRESULT(MA_NOACTIVATE),
        WM_NCHITTEST => LRESULT(HTTRANSPARENT),
        // The chip is drawn at its owner's DPI: on a monitor of another DPI it keeps its size.
        WM_DPICHANGED => LRESULT(0),
        // Windows shows the chip again with a restored owner: it stays hidden until the kit shows it.
        WM_SHOWWINDOW if refuses_show(wp.0 != 0, lp.0) => {
            // SAFETY: this very window, on its thread; no state of the kit is borrowed in this procedure.
            let _ = unsafe { ShowWindow(hwnd, SW_HIDE) };
            LRESULT(0)
        }
        // SAFETY: the arguments this procedure received.
        _ => unsafe { DefWindowProcW(hwnd, msg, wp, lp) },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::command::CommandTable;
    use crate::cx::EventCx;
    use crate::dispatch::Input;
    use crate::event::{Event, Mods, MouseButton, Reply};
    use crate::geom::Scale;
    use crate::harness::FakeClipboard;
    use crate::host::{AppId, Caption, EventLoop};
    use crate::node::{Node, Role, WidgetId};
    use crate::screen::Screen;
    use crate::strings::{Lang, UiLang};
    use crate::text::GdiTextEngine;
    use crate::theme::ThemeSpec;
    use crate::tokens::TOOLTIP_DELAY;
    use crate::w3::Canvas;

    const WORK: RectP = RectP { l: 0, t: 0, r: 1000, b: 700 };
    const CHIP: SizeP = SizeP { w: 100, h: 24 };

    #[test]
    fn tip_goes_8_below_and_centred() {
        let p = place(RectP::wh(100, 100, 28, 28), CHIP, WORK, 8);
        assert_eq!(p, PointP { x: 114 - 50, y: 136 });
    }

    #[test]
    fn tip_flips_above_at_the_bottom() {
        // The node ends at 690: 698 + 24 would leave the work area (700), so the chip goes above it.
        let p = place(RectP::wh(100, 662, 28, 28), CHIP, WORK, 8);
        assert_eq!(p.y, 662 - 8 - 24);
        // Just fits below: it stays below.
        let p = place(RectP::wh(100, 640, 28, 28), CHIP, WORK, 8);
        assert_eq!(p.y, 676);
        // No room on either side: kept inside at the top.
        let tall = RectP { l: 0, t: 0, r: 1000, b: 30 };
        let p = place(RectP::wh(10, 2, 20, 20), CHIP, tall, 8);
        assert_eq!(p.y, 0);
    }

    #[test]
    fn tip_is_clamped_horizontally() {
        assert_eq!(place(RectP::wh(0, 100, 20, 28), CHIP, WORK, 8).x, 0, "left edge");
        assert_eq!(place(RectP::wh(985, 100, 15, 28), CHIP, WORK, 8).x, 900, "right edge");
    }

    /// Two monitors: the primary one and one at its left with a taller taskbar.
    struct FakeDesk {
        origin: PointP,
    }

    const PRIMARY: RectP = RectP { l: 0, t: 0, r: 1920, b: 1040 };
    const LEFT: RectP = RectP { l: -1280, t: 0, r: 0, b: 970 };

    impl Desk for FakeDesk {
        fn client_origin(&self, owner: HWND) -> PointP {
            let _ = owner;
            self.origin
        }

        fn work_area(&self, r: RectP) -> RectP {
            if (r.l + r.r) / 2 < 0 {
                LEFT
            } else {
                PRIMARY
            }
        }
    }

    fn hwnd(n: usize) -> HWND {
        HWND(n as *mut core::ffi::c_void)
    }

    /// One owner window's parts at a DPI.
    struct Parts {
        disp: Dispatcher,
        res: Res,
        tokens: Tokens,
        dpi: u32,
        now: u64,
    }

    impl Parts {
        fn new(dpi: u32) -> Parts {
            Parts {
                disp: Dispatcher::new(),
                res: Res::new(Box::new(GdiTextEngine::new().unwrap())),
                tokens: Tokens::oled(),
                dpi,
                now: 0,
            }
        }

        fn owner(&mut self, w: HWND) -> Owner<'_> {
            Owner { hwnd: w, env: env_of(self.dpi, self.now, &self.tokens), disp: &mut self.disp, res: &mut self.res }
        }

        fn chip_size(&mut self, text: &str) -> SizeP {
            let env = env_of(self.dpi, self.now, &self.tokens);
            chip_size(&mut LayoutCx::new(env, &mut self.disp, &mut self.res), text, None)
        }
    }

    fn env_of(dpi: u32, now: u64, tokens: &Tokens) -> Env<'_> {
        Env {
            scale: Scale { dpi },
            tokens,
            lang: UiLang::EN,
            now_ms: now,
            caption: Caption::System,
            caption_spike: false,
        }
    }

    /// Icon buttons with tips, at logical rects (scaled at layout).
    struct Buttons {
        rects: Vec<RectP>,
        commands: CommandTable,
    }

    impl Screen for Buttons {
        fn layout(&mut self, cx: &mut LayoutCx, avail: SizeP) -> SizeP {
            for (i, r) in self.rects.iter().enumerate() {
                let rect = RectP::wh(cx.px(r.l), cx.px(r.t), cx.px(r.w()), cx.px(r.h()));
                let n = Node::new(NodeId::new(WidgetId(7), i as u32), Role::Button, rect);
                cx.push(n.label(Text::from("Search")).description(Text::from("Search")));
            }
            avail
        }

        fn paint(&self, cx: &mut PaintCx) {
            let _ = cx;
        }

        fn event(&mut self, cx: &mut EventCx, ev: &Event) -> Reply {
            let _ = (cx, ev);
            Reply::None
        }

        fn commands(&self) -> &CommandTable {
            &self.commands
        }
    }

    fn buttons(rects: &[RectP]) -> Buttons {
        Buttons { rects: rects.to_vec(), commands: CommandTable::new() }
    }

    /// The dispatcher's layout of `screen` at 400 x 300 logical px.
    fn layout(p: &mut Parts, screen: &mut Buttons) {
        let size = Scale { dpi: p.dpi }.size(crate::geom::SizeL { w: 400, h: 300 });
        p.disp.layout(screen, env_of(p.dpi, p.now, &p.tokens), &mut p.res, size);
    }

    /// One input through the dispatcher; its tip effects applied to `tip` as the host does.
    fn input(p: &mut Parts, screen: &mut Buttons, tip: &mut Tip, w: HWND, desk: &FakeDesk, inp: Input) {
        let mut clip = FakeClipboard { text: None, files: false };
        p.disp.input(screen, inp, env_of(p.dpi, p.now, &p.tokens), &mut p.res, &mut clip);
        due(p, tip, w, desk);
    }

    /// The tip effects waiting in the dispatcher, applied to `tip`.
    fn due(p: &mut Parts, tip: &mut Tip, w: HWND, desk: &FakeDesk) {
        let fx = std::mem::take(&mut p.disp.effects);
        for e in fx {
            tip.apply(&mut p.owner(w), desk, e);
        }
    }

    /// The deadline fires at `now`.
    fn fire(p: &mut Parts, tip: &mut Tip, w: HWND, desk: &FakeDesk, now: u64) {
        p.now = now;
        p.disp.on_due(now);
        due(p, tip, w, desk);
    }

    #[test]
    fn a_hover_shows_the_chip_after_the_delay_and_leave_press_and_deactivation_hide_it() {
        let (w, desk) = (hwnd(1), FakeDesk { origin: PointP { x: 300, y: 200 } });
        let mut p = Parts::new(96);
        let mut screen = buttons(&[RectP::wh(20, 20, 28, 28), RectP::wh(60, 20, 28, 28)]);
        let mut tip = Tip::new();
        layout(&mut p, &mut screen);
        let over = |i: i32| Input::Move(PointP { x: 34 + 40 * i, y: 34 }, Mods::NONE);
        input(&mut p, &mut screen, &mut tip, w, &desk, over(0));
        assert!(tip.shown.is_none(), "not before the delay");
        assert_eq!(p.disp.next_due(), Some(500), "one deadline, TOOLTIP_DELAY away");
        fire(&mut p, &mut tip, w, &desk, 499);
        assert!(tip.shown.is_none());
        fire(&mut p, &mut tip, w, &desk, 500);
        let s = tip.shown.clone().expect("the chip after 500 ms");
        assert_eq!((s.owner, s.node, &*s.text), (w, NodeId::new(WidgetId(7), 0), "Search"));
        assert_eq!(s.rect.t, 200 + 48 + 8, "8 px below the button, in screen coordinates");
        assert_eq!(tip.plan(), Plan::Present { owner: w, window: None, stale: None }, "the window is made at once");
        assert_eq!(p.disp.next_due(), None, "no timer while the tip is up");
        input(&mut p, &mut screen, &mut tip, w, &desk, Input::Leave);
        assert!(tip.shown.is_none(), "the pointer left the window");
        assert_eq!(p.disp.next_due(), None, "nothing hovered: no timer");
        // Warm: back on a button within TOOLTIP_WARM, at once; then a press takes it away.
        p.now = 700;
        input(&mut p, &mut screen, &mut tip, w, &desk, over(1));
        assert_eq!(tip.shown.as_ref().map(|s| s.node), Some(NodeId::new(WidgetId(7), 1)), "warm: no wait");
        let press = Input::Down(PointP { x: 74, y: 34 }, MouseButton::Left, 1, Mods::NONE);
        input(&mut p, &mut screen, &mut tip, w, &desk, press);
        assert!(tip.shown.is_none(), "a press hides it");
        // Another button, the full delay, then the window loses the activation.
        p.now = 3000;
        input(&mut p, &mut screen, &mut tip, w, &desk, over(0));
        fire(&mut p, &mut tip, w, &desk, 3500);
        assert!(tip.shown.is_some());
        input(&mut p, &mut screen, &mut tip, w, &desk, Input::Active(false));
        assert!(tip.shown.is_none(), "deactivation hides it");
        // And a key.
        p.now = 6000;
        input(&mut p, &mut screen, &mut tip, w, &desk, Input::Active(true));
        input(&mut p, &mut screen, &mut tip, w, &desk, over(1));
        fire(&mut p, &mut tip, w, &desk, 6500);
        assert!(tip.shown.is_some());
        input(&mut p, &mut screen, &mut tip, w, &desk, Input::Key(crate::event::Key::ESC, true, false, Mods::NONE));
        assert!(tip.shown.is_none(), "a key hides it");
    }

    #[test]
    fn the_window_shows_the_chip_paint_chip_draws() {
        let (w, desk) = (hwnd(1), FakeDesk { origin: PointP { x: 300, y: 200 } });
        let mut p = Parts::new(96);
        let mut tip = Tip::new();
        let node = NodeId::new(WidgetId(7), 0);
        let rect = RectP::wh(100, 50, 28, 28);
        assert!(tip.show(&mut p.owner(w), &desk, node, rect, Text::from("Search"), Some(Text::from("Ctrl+F"))));
        let s = tip.shown.clone().unwrap();
        let painted = tip.painted.unwrap();
        assert_eq!(painted.pos, Some(PointP { x: s.rect.l, y: s.rect.t }), "presented where it was placed");
        assert_eq!((painted.size.w, painted.size.h), (s.rect.w(), s.rect.h()));
        assert_eq!(painted.dirty, RectP::wh(0, 0, s.rect.w(), s.rect.h()), "every pixel of the chip");
        assert_eq!(s.rect.h(), 24, "CHIP_H at 96 dpi");
        assert_eq!(s.rect.l, 300 + 114 - s.rect.w() / 2, "centred on the button");
        // The pixels: paint_chip's, drawn the same way into a plain buffer.
        let (cw, ch, got) = tip.pixels();
        let mut want = vec![0u32; (cw * ch) as usize];
        {
            let env = env_of(p.dpi, p.now, &p.tokens);
            let canvas = Canvas { buf: &mut want, w: cw, h: ch, clip: None };
            let mut cx =
                PaintCx::new(env, canvas, PointP::default(), RectP::wh(0, 0, cw, ch), &p.disp, &mut p.res, None);
            paint_chip(&mut cx, PointP::default(), "Search", Some("Ctrl+F"));
        }
        assert!(got == want, "the window's pixels are the chip's");
        assert_eq!(got[0] >> 24, 0, "a rounded corner is transparent");
        assert_eq!(got[(ch / 2 * cw + 4) as usize] >> 24, 255, "the chip is opaque");
    }

    #[test]
    fn the_chip_flips_above_on_the_nodes_monitor_and_stays_inside_it() {
        // The owner's client on the left monitor, a button near its bottom and its left edge.
        let (w, desk) = (hwnd(1), FakeDesk { origin: PointP { x: -1270, y: 900 } });
        let mut p = Parts::new(96);
        let mut tip = Tip::new();
        let rect = RectP::wh(0, 30, 28, 28);
        tip.show(&mut p.owner(w), &desk, NodeId::new(WidgetId(7), 0), rect, Text::from("Search"), None);
        let s = tip.shown.clone().unwrap();
        assert_eq!(s.rect.t, 930 - 8 - 24, "958 + 8 + 24 would leave the left monitor's work area (970): above");
        assert_eq!(s.rect.l, LEFT.l, "clamped to the left monitor, not the primary one");
        // The same node on the primary monitor, whose work area is taller: below.
        let desk = FakeDesk { origin: PointP { x: 30, y: 900 } };
        tip.show(&mut p.owner(w), &desk, NodeId::new(WidgetId(7), 0), rect, Text::from("Search"), None);
        assert_eq!(tip.shown.unwrap().rect.t, 958 + 8);
    }

    #[test]
    fn the_chip_takes_the_owners_dpi() {
        let (w, desk) = (hwnd(1), FakeDesk { origin: PointP { x: 0, y: 0 } });
        let mut small = Parts::new(96);
        let mut big = Parts::new(144);
        let mut tip = Tip::new();
        let node = NodeId::new(WidgetId(7), 0);
        tip.show(&mut small.owner(w), &desk, node, RectP::wh(100, 50, 28, 28), Text::from("Search"), None);
        let at96 = tip.shown.clone().unwrap().rect;
        tip.show(&mut big.owner(w), &desk, node, RectP::wh(150, 75, 42, 42), Text::from("Search"), None);
        let at144 = tip.shown.clone().unwrap().rect;
        assert_eq!((at96.h(), at144.h()), (24, 36), "CHIP_H at the owner's scale");
        assert_eq!((at96.t, at144.t), (78 + 8, 117 + 12), "the gap too");
        assert_eq!(at144.w(), big.chip_size("Search").w);
        assert!(at144.w() > at96.w(), "the text at the owner's DPI");
        assert_eq!(tip.shown.unwrap().dpi, 144);
    }

    #[test]
    fn after_a_relayout_the_chip_follows_its_node_and_goes_with_it() {
        let (w, desk) = (hwnd(1), FakeDesk { origin: PointP { x: 300, y: 200 } });
        let mut p = Parts::new(96);
        let mut tip = Tip::new();
        let mut screen = buttons(&[RectP::wh(20, 20, 28, 28)]);
        layout(&mut p, &mut screen);
        input(&mut p, &mut screen, &mut tip, w, &desk, Input::Move(PointP { x: 34, y: 34 }, Mods::NONE));
        fire(&mut p, &mut tip, w, &desk, 500);
        let before = tip.shown.clone().unwrap().rect;
        assert!(!tip.after_layout(&mut p.owner(w), &desk), "an unchanged layout moves nothing");
        // The button moves 4 px down, still under the pointer: the chip follows.
        screen.rects[0] = RectP::wh(20, 24, 28, 28);
        layout(&mut p, &mut screen);
        assert!(tip.after_layout(&mut p.owner(w), &desk));
        assert_eq!(tip.shown.clone().unwrap().rect, before.offset(0, 4));
        assert_eq!(tip.plan(), Plan::Present { owner: w, window: None, stale: None });
        // New tokens: drawn again, same place.
        p.tokens.generation += 1;
        layout(&mut p, &mut screen);
        assert!(tip.after_layout(&mut p.owner(w), &desk));
        assert_eq!(tip.shown.as_ref().map(|s| s.generation), Some(p.tokens.generation));
        // The button goes away: so does the chip, and nothing is pending.
        screen.rects.clear();
        layout(&mut p, &mut screen);
        assert!(tip.after_layout(&mut p.owner(w), &desk));
        assert!(tip.shown.is_none(), "its anchor is gone");
        assert_eq!(p.disp.next_due(), None, "no timer");
        // A button under the resting pointer after a relayout: it waits the delay (the pointer did not move).
        p.now = 5000;
        screen.rects.push(RectP::wh(20, 20, 28, 28));
        layout(&mut p, &mut screen);
        tip.after_layout(&mut p.owner(w), &desk);
        assert!(tip.shown.is_none());
        assert_eq!(p.disp.next_due(), Some(5500));
    }

    #[test]
    fn a_pending_tip_shows_where_its_node_is_when_it_is_due() {
        let (w, desk) = (hwnd(1), FakeDesk { origin: PointP { x: 300, y: 200 } });
        let mut p = Parts::new(96);
        let mut tip = Tip::new();
        let mut screen = buttons(&[RectP::wh(20, 20, 28, 28)]);
        layout(&mut p, &mut screen);
        input(&mut p, &mut screen, &mut tip, w, &desk, Input::Move(PointP { x: 34, y: 34 }, Mods::NONE));
        // The button moves 4 px down under the resting pointer while the tip waits.
        screen.rects[0] = RectP::wh(20, 24, 28, 28);
        layout(&mut p, &mut screen);
        assert!(!tip.after_layout(&mut p.owner(w), &desk), "the pending tip keeps waiting");
        fire(&mut p, &mut tip, w, &desk, 500);
        assert_eq!(tip.shown.map(|s| (s.anchor, s.rect.t)), Some((RectP::wh(20, 24, 28, 28), 200 + 52 + 8)));
    }

    #[test]
    fn hide_and_forget_are_for_the_owner_of_the_chip() {
        let desk = FakeDesk { origin: PointP { x: 0, y: 0 } };
        let (a, b) = (hwnd(1), hwnd(2));
        let mut p = Parts::new(96);
        let mut tip = Tip::new();
        let node = NodeId::new(WidgetId(7), 0);
        tip.show(&mut p.owner(a), &desk, node, RectP::wh(10, 10, 28, 28), Text::from("Search"), None);
        assert!(!tip.hide(b), "another window's HideTip");
        assert_eq!(tip.forget(b), None);
        assert!(tip.shown.is_some());
        assert!(tip.hide(a));
        assert!(tip.shown.is_none());
        assert_eq!(tip.plan(), Plan::Nothing, "never shown: nothing to hide");
        // Owner destroyed: its pixels go (the DIB is freed) and so does the window made for it.
        tip.show(&mut p.owner(a), &desk, node, RectP::wh(10, 10, 28, 28), Text::from("Search"), None);
        tip.window = Some((hwnd(10), a));
        assert_eq!(tip.forget(a), Some(hwnd(10)));
        assert!(tip.shown.is_none() && tip.painted.is_none() && tip.window.is_none());
        assert_eq!(tip.pixels().0, 0, "the DIB is freed");
    }

    #[test]
    fn the_window_is_made_once_per_owner() {
        let desk = FakeDesk { origin: PointP { x: 0, y: 0 } };
        let (a, b, wa) = (hwnd(1), hwnd(2), hwnd(10));
        let mut p = Parts::new(96);
        let mut tip = Tip::new();
        let node = NodeId::new(WidgetId(7), 0);
        let mut show = |tip: &mut Tip, o: HWND| {
            tip.show(&mut p.owner(o), &desk, node, RectP::wh(10, 10, 28, 28), Text::from("Search"), None);
        };
        show(&mut tip, a);
        assert_eq!(tip.plan(), Plan::Present { owner: a, window: None, stale: None });
        // What sync does once the window exists and shows the chip.
        tip.window = Some((wa, a));
        tip.presented = true;
        tip.visible = true;
        assert_eq!(tip.plan(), Plan::Nothing, "up to date");
        show(&mut tip, a);
        assert_eq!(tip.plan(), Plan::Present { owner: a, window: Some(wa), stale: None }, "the same window");
        tip.presented = true;
        assert!(tip.hide(a));
        assert_eq!(tip.plan(), Plan::Hide(wa));
        tip.visible = false;
        assert_eq!(tip.plan(), Plan::Nothing, "hidden already");
        show(&mut tip, b);
        assert_eq!(tip.plan(), Plan::Present { owner: b, window: None, stale: Some(wa) }, "made again for b");
        tip.window_gone(wa);
        assert_eq!(tip.plan(), Plan::Present { owner: b, window: None, stale: None });
    }

    #[test]
    fn the_tip_window_never_activates() {
        for s in [WS_EX_LAYERED, WS_EX_TRANSPARENT, WS_EX_NOACTIVATE, WS_EX_TOOLWINDOW] {
            assert_eq!(EX_STYLE.0 & s.0, s.0, "{s:?}");
        }
        use windows::Win32::UI::WindowsAndMessaging::{WS_EX_APPWINDOW, WS_EX_TOPMOST};
        assert_eq!(EX_STYLE.0 & (WS_EX_APPWINDOW.0 | WS_EX_TOPMOST.0), 0);
        assert_eq!(SHOW, SW_SHOWNOACTIVATE);
        assert_eq!(CLASS, "MiniOledTip");
    }

    /// The state of a window with no window behind it, laid out with one icon button at (20, 20): the host's own tip
    /// functions run on it, and the window calls they leave are taken, never run.
    fn headless(lp: &EventLoop) -> WinHost {
        let screen = Box::new(buttons(&[RectP::wh(20, 20, 28, 28)]));
        let mut host = WinHost::headless(lp.shared(), screen, 96, SizeP { w: 400, h: 300 });
        host.layout();
        assert!(host.deferred.is_empty() && host.deadline.is_none(), "nothing hovered: no call and no timer");
        host
    }

    /// The chip up for the button, put there by Tip::show itself (as a due ShowTip leaves it).
    fn chip_up(host: &mut WinHost) {
        let (lp, tokens) = (Rc::clone(&host.lp), host.tokens());
        let desk = FakeDesk { origin: PointP { x: 0, y: 0 } };
        let (node, rect) = (NodeId::new(WidgetId(7), 0), RectP::wh(20, 20, 28, 28));
        assert!(lp.tip.borrow_mut().show(&mut owner(host, &tokens), &desk, node, rect, Text::from("Search"), None));
    }

    /// The window calls the host left are exactly one, the tip window's (taken and dropped, never run).
    fn one_call(host: &mut WinHost) -> bool {
        matches!(std::mem::take(&mut host.deferred).as_slice(), [Sys::Call(_)])
    }

    #[test]
    fn the_hosts_show_tip_puts_the_chip_up_and_the_window_follows() {
        let lp = EventLoop::new(AppId("test"), Lang::En, ThemeSpec::oled()).unwrap();
        let mut host = headless(&lp);
        let (node, rect, text) = (NodeId::new(WidgetId(7), 0), RectP::wh(20, 20, 28, 28), Text::from("Search"));
        host.disp.effects.push(Effect::ShowTip { node, rect, text, chord: None });
        host.apply_pending();
        let shown = host.lp.tip.borrow().shown.as_ref().map(|s| (s.owner, s.node));
        assert_eq!(shown, Some((host.hwnd, node)), "the chip is up for the window's node");
        assert!(one_call(&mut host), "the window follows once the borrow ends");
    }

    #[test]
    fn the_hosts_hide_tip_takes_the_chip_away_and_the_window_follows() {
        let lp = EventLoop::new(AppId("test"), Lang::En, ThemeSpec::oled()).unwrap();
        let mut host = headless(&lp);
        chip_up(&mut host);
        host.disp.effects.push(Effect::HideTip);
        host.apply_pending();
        assert!(host.lp.tip.borrow().shown.is_none(), "the chip is gone");
        assert!(one_call(&mut host), "the window hides once the borrow ends");
    }

    #[test]
    fn a_relayout_of_the_host_takes_the_chip_away_with_its_node() {
        let lp = EventLoop::new(AppId("test"), Lang::En, ThemeSpec::oled()).unwrap();
        let mut host = headless(&lp);
        chip_up(&mut host);
        host.layout();
        let up = host.lp.tip.borrow().shown.is_some();
        assert!(up && host.deferred.is_empty(), "the node is still there: the chip stays and nothing is called");
        host.screen = Box::new(buttons(&[]));
        host.layout();
        assert!(host.lp.tip.borrow().shown.is_none(), "the node is gone and so is the chip");
        assert!(one_call(&mut host), "the window hides once the borrow ends");
        assert_eq!((host.deadline, lp.stats().timers_armed), (None, 0), "nothing pending: no timer");
    }

    #[test]
    fn the_tip_window_stays_hidden_when_windows_shows_it_with_its_restored_owner() {
        assert!(refuses_show(true, SW_PARENTOPENING), "the owner restored or shown: Windows shows its popups");
        assert!(!refuses_show(false, SW_PARENTOPENING), "hiding is never refused");
        assert!(!refuses_show(true, 0), "the kit's own ShowWindow");
        assert!(!refuses_show(true, 1), "the owner closing");
    }

    #[test]
    fn minimising_hiding_or_disabling_the_owner_takes_the_chip_away() {
        assert!(owner_message_hides_chip(WM_SIZE, SIZE_MINIMIZED));
        assert!(!owner_message_hides_chip(WM_SIZE, 0), "a restore or a resize");
        assert!(!owner_message_hides_chip(WM_SIZE, 2));
        assert!(owner_message_hides_chip(WM_SHOWWINDOW, 0));
        assert!(!owner_message_hides_chip(WM_SHOWWINDOW, 1));
        assert!(owner_message_hides_chip(WM_ENABLE, 0));
        assert!(!owner_message_hides_chip(WM_ENABLE, 1));
        assert!(!owner_message_hides_chip(WM_DPICHANGED, 0));
    }

    #[test]
    fn an_owner_going_away_takes_the_chip_and_a_pending_tip_away_without_a_timer() {
        let lp = EventLoop::new(AppId("test"), Lang::En, ThemeSpec::oled()).unwrap();
        let mut host = headless(&lp);
        chip_up(&mut host);
        on_owner_away(&mut host);
        assert!(host.lp.tip.borrow().shown.is_none(), "the chip is gone");
        assert!(tips::next_due(&host.disp.tips).is_none(), "nothing pending in the dispatcher");
        assert!(one_call(&mut host), "the window hides once the borrow ends");
        assert_eq!((host.deadline, lp.stats().timers_armed), (None, 0), "no timer");
    }

    /// The dispatcher's own tip, as the pointer resting on the button leaves it: pending, with the timer's deadline.
    fn pending_in_dispatcher(host: &mut WinHost, now: u64) {
        let mut fx = Vec::new();
        tips::on_hover(&mut host.disp.tips, host.disp.frame.hit(PointP { x: 34, y: 34 }), now, &mut fx);
        assert!(fx.is_empty(), "the delay is running");
        host.deadline = tips::next_due(&host.disp.tips);
        assert_eq!(host.deadline, Some(now + u64::from(TOOLTIP_DELAY)), "armed, as the host's timer would be");
    }

    /// What a hover over the button does to the dispatcher's tip now.
    fn hover(host: &mut WinHost, over: bool, now: u64) -> Vec<Effect> {
        let mut fx = Vec::new();
        let node = if over { host.disp.frame.hit(PointP { x: 34, y: 34 }) } else { None };
        tips::on_hover(&mut host.disp.tips, node, now, &mut fx);
        fx
    }

    #[test]
    fn an_owner_going_away_cancels_the_pending_tip_of_the_dispatcher_and_dismisses_its_node() {
        let lp = EventLoop::new(AppId("test"), Lang::En, ThemeSpec::oled()).unwrap();
        let mut host = headless(&lp);
        pending_in_dispatcher(&mut host, 1000);
        on_owner_away(&mut host);
        assert_eq!(tips::next_due(&host.disp.tips), None, "the pending tip is cancelled: nothing may fire while away");
        assert_eq!(host.deadline, None, "and no timer is left");
        // Restored with the pointer still on the button: the node stays dismissed, no tip and no delay.
        let fx = hover(&mut host, true, 5000);
        assert!(fx.is_empty() && tips::next_due(&host.disp.tips).is_none(), "no chip comes back by itself");
        // The pointer leaves and returns: the normal delay.
        assert!(hover(&mut host, false, 6000).is_empty());
        assert!(hover(&mut host, true, 7000).is_empty());
        assert_eq!(tips::next_due(&host.disp.tips), Some(7000 + u64::from(TOOLTIP_DELAY)), "the normal delay");
    }

    #[test]
    fn an_owner_going_away_takes_the_dispatchers_shown_tip_away_through_its_own_path() {
        let lp = EventLoop::new(AppId("test"), Lang::En, ThemeSpec::oled()).unwrap();
        let mut host = headless(&lp);
        pending_in_dispatcher(&mut host, 1000);
        let mut fx = Vec::new();
        tips::on_due(&mut host.disp.tips, 1000 + u64::from(TOOLTIP_DELAY), &mut fx);
        assert!(matches!(fx.as_slice(), [Effect::ShowTip { .. }]), "the dispatcher shows the tip");
        host.deadline = None;
        on_owner_away(&mut host);
        // A tip the dispatcher still counted as shown would emit HideTip when the pointer leaves.
        let fx = hover(&mut host, false, 9000);
        assert!(fx.is_empty(), "the dispatcher no longer counts a tip as shown: {}", fx.len());
        assert!(hover(&mut host, true, 9100).is_empty());
        assert_eq!(tips::next_due(&host.disp.tips), Some(9100 + u64::from(TOOLTIP_DELAY)), "no warm show either");
        assert_eq!(host.deadline, None);
    }

    #[test]
    fn the_hosts_destroy_takes_the_chip_and_its_window_away() {
        let lp = EventLoop::new(AppId("test"), Lang::En, ThemeSpec::oled()).unwrap();
        let mut host = headless(&lp);
        chip_up(&mut host);
        let w = host.hwnd;
        host.lp.tip.borrow_mut().window = Some((hwnd(10), w));
        on_owner_destroy(&mut host, w);
        let gone = {
            let t = host.lp.tip.borrow();
            t.shown.is_none() && t.window.is_none()
        };
        assert!(gone, "the chip and the window made for this owner are forgotten");
        assert!(one_call(&mut host), "the tip window is destroyed once the borrow ends");
    }
}
