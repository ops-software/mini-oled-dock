//! Menu and list popup windows (K2a.9, KC2): one layered owned window per open level, of class "MiniOledMenu",
//! WS_EX_LAYERED | WS_EX_NOACTIVATE | WS_EX_TOOLWINDOW, never topmost, shown without activation, so the owner keeps the
//! activation and the keyboard and no modal loop runs anywhere (no TrackPopupMenu, no COM re-entrancy).
//!
//! The levels live in the owner's Dispatcher (widgets::menu::MenuStack); sync() makes the windows follow them: it lays
//! out what the screen opened, places each level against the work area of its anchor's monitor, paints the levels that
//! changed with the owner's DPI, tokens and text engine into their own LayeredPresenter, and leaves the window calls
//! (create, UpdateLayeredWindowIndirect, show, destroy) for after the owner's borrow. A change the stack makes outside
//! this module (a key, the submenu delay) posts WM_OLED_MENU to a popup, which syncs. The popups' pointer input goes to
//! the stack through the owner's state; their outcomes reach the screen through Dispatcher::menu_events. Opening fades in
//! over DUR_NORMAL with a SLIDE_PX slide (a 16 ms timer on the popup while it lasts; nothing under reduced motion);
//! closing is instant. Owned windows go with their owner; on_owner_destroy frees the rest.

use windows::core::{HSTRING, PCWSTR};
use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, POINT, RECT, WPARAM};
use windows::Win32::Graphics::Gdi::{
    ClientToScreen, GetMonitorInfoW, MonitorFromRect, MONITORINFO, MONITOR_DEFAULTTONEAREST,
};
use windows::Win32::UI::Input::KeyboardAndMouse::{TrackMouseEvent, TME_LEAVE, TRACKMOUSEEVENT};
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, DestroyWindow, GetClassInfoExW, GetWindow, GetWindowLongPtrW, IsWindow, KillTimer,
    LoadCursorW, PostMessageW, RegisterClassExW, SetTimer, SetWindowLongPtrW, ShowWindow, GWLP_USERDATA, GW_OWNER,
    IDC_ARROW, SHOW_WINDOW_CMD, SW_SHOWNOACTIVATE, WINDOW_EX_STYLE, WM_APP, WM_DPICHANGED, WM_LBUTTONDOWN,
    WM_LBUTTONUP, WM_MOUSEACTIVATE, WM_MOUSEMOVE, WM_RBUTTONDOWN, WM_RBUTTONUP, WM_TIMER, WNDCLASSEXW, WS_EX_LAYERED,
    WS_EX_NOACTIVATE, WS_EX_TOOLWINDOW, WS_POPUP,
};

use super::tooltip::Desk;
use super::window::{self, Sys, WinHost};
use crate::command::CommandTable;
use crate::cx::{Anchor, Env, MenuId, MenuItem, PaintCx, Res};
use crate::dispatch::Dispatcher;
use crate::geom::{PointP, RectP};
use crate::harness::{Image, MenuSnapshot};
use crate::node::{NodeId, Text};
use crate::present::layered::{self, LayeredPresenter, Present};
use crate::present::Damage;
use crate::w3::Canvas;
use crate::widgets::menu;

/// The popups' window class.
pub(crate) const CLASS: &str = "MiniOledMenu";

/// A popup's extended styles: layered, never activated, no taskbar button; not topmost, not click-through.
pub(crate) const EX_STYLE: WINDOW_EX_STYLE = WINDOW_EX_STYLE(WS_EX_LAYERED.0 | WS_EX_NOACTIVATE.0 | WS_EX_TOOLWINDOW.0);

/// How a popup is shown: without activation.
pub(crate) const SHOW: SHOW_WINDOW_CMD = SW_SHOWNOACTIVATE;

/// Posted to a popup: its owner's menus changed where no window could follow at once (a key, the submenu delay).
pub(crate) const WM_OLED_MENU: u32 = WM_APP + 0x20;

/// The opening fade's timer on a popup and its frame.
const TIMER_FADE: usize = 1;
const FADE_FRAME_MS: u32 = 16;

/// WM_MOUSEACTIVATE's MA_NOACTIVATE; WM_MOUSELEAVE (winuser.h; the windows crate has it under UI::Controls only).
const MA_NOACTIVATE: isize = 3;
const WM_MOUSELEAVE: u32 = 0x02A3;

/// A popup's opening fade: since `start`, over `dur` ms, from `dy` px off its place.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Fade {
    pub(crate) start: u64,
    pub(crate) dur: u32,
    pub(crate) dy: i32,
}

impl Fade {
    /// The alpha and the vertical offset at `now` (out-cubic); None once it is over.
    pub(crate) fn at(&self, now: u64) -> Option<(u8, i32)> {
        let t = now.saturating_sub(self.start) as f32 / self.dur.max(1) as f32;
        if t >= 1.0 {
            return None;
        }
        let e = 1.0 - (1.0 - t.max(0.0)).powi(3);
        Some(((255.0 * e).round() as u8, (self.dy as f32 * (1.0 - e)).round() as i32))
    }
}

/// The window of one open level.
pub(crate) struct Popup {
    pub(crate) menu: MenuId,
    pub(crate) hwnd: Option<HWND>,
    /// Taken out while it paints (the paint reads the owner's Dispatcher, which holds it).
    presenter: Option<LayeredPresenter>,
    /// The present of the last paint, at the popup's place on the screen.
    painted: Option<Present>,
    /// The stack revision it was painted at.
    rev: Option<u64>,
    presented: bool,
    shown: bool,
    pub(crate) fade: Option<Fade>,
}

impl Popup {
    fn new(menu: MenuId) -> Popup {
        Popup {
            menu,
            hwnd: None,
            presenter: Some(LayeredPresenter::new()),
            painted: None,
            rev: None,
            presented: false,
            shown: false,
            fade: None,
        }
    }

    /// The pixels painted for it, the DIB of its presenter (tests).
    #[cfg(test)]
    pub(crate) fn pixels(&mut self) -> (i32, i32, Vec<u32>) {
        let c = self.presenter.as_mut().map(|p| p.canvas());
        c.map_or((0, 0, Vec::new()), |c| (c.w, c.h, c.buf.to_vec()))
    }
}

/// The popup windows of one owner, kept in its MenuStack.
#[derive(Default)]
pub(crate) struct Windows {
    pub(crate) popups: Vec<Popup>,
    /// The windows of closed levels, destroyed after the borrow.
    pub(crate) dead: Vec<HWND>,
    /// A WM_OLED_MENU is on its way.
    poked: bool,
}

impl Windows {
    /// The stack changed where no window could follow at once: a popup is told to sync once the handler is over.
    pub(crate) fn poke(&mut self) {
        if self.poked {
            return;
        }
        if let Some(w) = self.popups.iter().find_map(|p| p.hwnd) {
            // SAFETY: a popup window of this thread; posting carries no pointer and sends nothing now.
            if unsafe { PostMessageW(Some(w), WM_OLED_MENU, WPARAM(0), LPARAM(0)) }.is_ok() {
                self.poked = true;
            }
        }
    }

    fn popup_mut(&mut self, id: MenuId) -> Option<&mut Popup> {
        self.popups.iter_mut().find(|p| p.menu == id)
    }
}

/// The real desktop: the owner's client origin and the work area of a monitor.
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

/// The rect an anchor sits on (its monitor is the popup's).
fn anchor_rect(at: Anchor) -> RectP {
    match at {
        Anchor::Point(p) => RectP::wh(p.x, p.y, 1, 1),
        Anchor::Below(r) | Anchor::Above(r) | Anchor::Right(r) => r,
    }
}

/// Makes the popups what the owner's stack says (see the module comment); the window calls run after the borrow.
pub(crate) fn sync(host: &mut WinHost) {
    if follow(host, &Win32Desk) {
        let owner = host.hwnd;
        host.defer(Sys::Call(Box::new(move |_| apply(owner))));
    }
}

/// sync's part inside the borrow: true when window calls must follow.
pub(crate) fn follow(host: &mut WinHost, desk: &dyn Desk) -> bool {
    let tokens = host.tokens();
    let env = host.env(&tokens);
    {
        let WinHost { screen, disp, res, .. } = &mut *host;
        menu::resolve(&mut disp.menus, screen.commands(), &env, &mut *res.text);
    }
    let owner = host.hwnd;
    let stack = &mut host.disp.menus;
    stack.windows.poked = false;
    if stack.menus.is_empty() && stack.lists.is_empty() && stack.windows.popups.is_empty() {
        return !stack.windows.dead.is_empty();
    }
    let origin = desk.client_origin(owner);
    let gap = env.scale.px(menu::ANCHOR_GAP);
    // In order: a submenu's anchor is its parent's row, so the parent is placed first.
    let mut moved = Vec::new();
    for i in 0..stack.menus.len() + stack.lists.len() {
        let (menus, lists) = (&mut stack.menus, &mut stack.lists);
        let parent_row = (i > 0 && i < menus.len()).then(|| {
            let p = &menus[i - 1];
            p.child.and_then(|r| p.row_rect(r))
        });
        let l = if i < menus.len() { &mut menus[i] } else { &mut lists[i - menus.len()] };
        if l.placed {
            continue;
        }
        if let Some(Some(row)) = parent_row {
            l.anchor = Anchor::Right(row);
        }
        let Some((size, pad)) = l.layout.as_ref().map(|x| (x.size, x.pad)) else { continue };
        let at = menu::shift(l.anchor, origin.x, origin.y);
        let work = desk.work_area(anchor_rect(at));
        let p = menu::place(at, size, work, gap, pad);
        l.rect = RectP::wh(p.x - origin.x, p.y - origin.y, size.w, size.h);
        l.placed = true;
        moved.push(l.id);
    }
    // The windows of closed levels go; each open level has one.
    let live: Vec<MenuId> = stack.menus.iter().chain(&stack.lists).map(|l| l.id).collect();
    let w = &mut stack.windows;
    for p in w.popups.iter().filter(|p| !live.contains(&p.menu)) {
        w.dead.extend(p.hwnd);
    }
    w.popups.retain(|p| live.contains(&p.menu));
    for id in &live {
        if !w.popups.iter().any(|p| p.menu == *id) {
            w.popups.push(Popup::new(*id));
        }
    }
    // What changed or moved is painted again (a menu is small: the whole panel).
    for p in w.popups.iter_mut().filter(|p| moved.contains(&p.menu)) {
        p.rev = None;
    }
    let rev = stack.rev;
    let stale: Vec<MenuId> = w.popups.iter().filter(|p| p.rev != Some(rev)).map(|p| p.menu).collect();
    let motion = (tokens.motion.dur_normal, env.scale.px(tokens.motion.slide_px));
    for id in &stale {
        paint(host, env, origin, *id, rev, motion);
    }
    let w = &host.disp.menus.windows;
    !w.dead.is_empty() || w.popups.iter().any(|p| p.hwnd.is_none() || !p.presented)
}

/// Paints level `id` into its popup's presenter, at the stack revision `rev`; a popup painted for the first time
/// starts its fade (`motion`: DUR_NORMAL and the slide in px; no fade when the duration is 0).
fn paint(host: &mut WinHost, env: Env<'_>, origin: PointP, id: MenuId, rev: u64, motion: (u32, i32)) {
    let Some(mut pres) = host.disp.menus.windows.popup_mut(id).and_then(|p| p.presenter.take()) else { return };
    let painted = {
        let WinHost { disp, res, .. } = &mut *host;
        let disp: &Dispatcher = disp;
        disp.menus.level(id).and_then(|level| {
            let size = level.layout.as_ref()?.size;
            let all = RectP::wh(0, 0, size.w, size.h);
            let mut damage = Damage::default();
            damage.add(all, all);
            let pos = PointP { x: level.rect.l + origin.x, y: level.rect.t + origin.y };
            let below = level.rect.t >= anchor_rect(level.anchor).t;
            let p = pres.render(size, &damage, &mut |canvas, o, clip| {
                let mut cx = PaintCx::new(env, canvas, o, clip, disp, res, None);
                menu::paint_level(&mut cx, level, PointP { x: 0, y: 0 });
            })?;
            Some((Present { pos: Some(pos), ..p }, below))
        })
    };
    let now = env.now_ms;
    let Some(popup) = host.disp.menus.windows.popup_mut(id) else { return };
    popup.presenter = Some(pres);
    popup.rev = Some(rev);
    popup.presented = false;
    popup.painted = painted.map(|(p, _)| p);
    if !popup.shown && popup.fade.is_none() && motion.0 > 0 {
        let below = painted.is_none_or(|(_, b)| b);
        popup.fade = Some(Fade { start: now, dur: motion.0, dy: if below { -motion.1 } else { motion.1 } });
    }
}

/// One window call of a popup: its handle, what to present and whether it must be shown (and its fade timer armed).
struct Step {
    hwnd: HWND,
    present: Present,
    show: bool,
    timer: bool,
}

/// sync's window calls, with no state borrowed: the windows of closed levels are destroyed, the missing ones made, then
/// each painted popup is presented and shown.
fn apply(owner: HWND) {
    let dead = window::with_host(owner, |h| std::mem::take(&mut h.disp.menus.windows.dead)).unwrap_or_default();
    for w in dead {
        destroy(w);
    }
    let missing: Vec<MenuId> = window::with_host(owner, |h| {
        h.disp.menus.windows.popups.iter().filter(|p| p.hwnd.is_none()).map(|p| p.menu).collect()
    })
    .unwrap_or_default();
    for id in missing {
        let Some(w) = create(owner, id) else { continue };
        let kept = window::with_host(owner, |h| match h.disp.menus.windows.popup_mut(id) {
            Some(p) => {
                p.hwnd = Some(w);
                true
            }
            None => false,
        });
        if kept != Some(true) {
            destroy(w);
        }
    }
    // Read just before the calls, with nothing running in between: the DIBs they point into are alive.
    let steps = window::with_host(owner, |h| {
        let now = h.lp.now_ms();
        let mut out = Vec::new();
        for p in &mut h.disp.menus.windows.popups {
            let (Some(hwnd), Some(mut present)) = (p.hwnd, p.painted) else { continue };
            if p.presented {
                continue;
            }
            let fading = p.fade.and_then(|f| f.at(now));
            if let (Some((alpha, dy)), Some(pos)) = (fading, present.pos) {
                present.alpha = alpha;
                present.pos = Some(PointP { x: pos.x, y: pos.y + dy });
            } else {
                p.fade = None;
            }
            out.push(Step { hwnd, present, show: !p.shown, timer: fading.is_some() });
            p.presented = true;
            p.shown = true;
        }
        out
    })
    .unwrap_or_default();
    for s in steps {
        let _ = layered::present(s.hwnd, &s.present);
        if s.show {
            // SAFETY: a popup window of this thread; SHOW never activates it.
            let _ = unsafe { ShowWindow(s.hwnd, SHOW) };
        }
        if s.timer {
            // SAFETY: a popup window of this thread; no timer procedure: WM_TIMER comes to menu_proc.
            unsafe { SetTimer(Some(s.hwnd), TIMER_FADE, FADE_FRAME_MS, None) };
        }
    }
}

/// A fade frame of a popup: its alpha and place move on; the timer stops at the end.
fn fade_step(hwnd: HWND) {
    let Some(owner) = owner_of(hwnd) else { return };
    let id = menu_of(hwnd);
    let frame = window::with_host(owner, |h| {
        let now = h.lp.now_ms();
        let p = h.disp.menus.windows.popup_mut(id)?;
        let base = p.painted?;
        let f = p.fade?;
        let pos = base.pos?;
        Some(match f.at(now) {
            Some((alpha, dy)) => {
                (Present { src: None, alpha, pos: Some(PointP { x: pos.x, y: pos.y + dy }), ..base }, false)
            }
            None => {
                p.fade = None;
                (Present { src: None, alpha: 255, ..base }, true)
            }
        })
    })
    .flatten();
    let done = match frame {
        Some((p, done)) => {
            let _ = layered::present(hwnd, &p);
            done
        }
        None => true,
    };
    if done {
        // SAFETY: the popup's own timer; killing a timer that is gone fails harmlessly.
        let _ = unsafe { KillTimer(Some(hwnd), TIMER_FADE) };
    }
}

/// A popup's owner (GetWindow sends no message).
fn owner_of(hwnd: HWND) -> Option<HWND> {
    // SAFETY: reading a window's owner has no precondition.
    unsafe { GetWindow(hwnd, GW_OWNER) }.ok().filter(|h| !h.is_invalid())
}

/// The level a popup shows (its GWLP_USERDATA).
fn menu_of(hwnd: HWND) -> MenuId {
    // SAFETY: reading a window's own long has no precondition.
    MenuId(unsafe { GetWindowLongPtrW(hwnd, GWLP_USERDATA) } as u32)
}

/// Destroys a popup window if it still exists (Windows destroys owned windows with their owner).
fn destroy(w: HWND) {
    // SAFETY: a popup window this thread made, destroyed with no state borrowed; IsWindow guards one already gone.
    unsafe {
        if IsWindow(Some(w)).as_bool() {
            let _ = DestroyWindow(w);
        }
    }
}

/// The popup window of level `id` for `owner` (its class registered on first use).
fn create(owner: HWND, id: MenuId) -> Option<HWND> {
    let hinst = window::instance();
    let class = HSTRING::from(CLASS);
    let mut wc = WNDCLASSEXW { cbSize: std::mem::size_of::<WNDCLASSEXW>() as u32, ..Default::default() };
    // SAFETY: `wc` is a writable WNDCLASSEXW with cbSize set; the name outlives the call.
    if unsafe { GetClassInfoExW(Some(hinst), &class, &mut wc) }.is_err() {
        // SAFETY: IDC_ARROW is a system cursor; the handle is shared and never freed.
        let cursor = unsafe { LoadCursorW(None, IDC_ARROW) }.unwrap_or_default();
        let wc = WNDCLASSEXW {
            cbSize: std::mem::size_of::<WNDCLASSEXW>() as u32,
            lpfnWndProc: Some(menu_proc),
            hInstance: hinst,
            hCursor: cursor,
            lpszClassName: PCWSTR(class.as_ptr()),
            ..Default::default()
        };
        // SAFETY: `wc` is complete and its class name outlives the call. A failure shows in CreateWindowExW.
        let _ = unsafe { RegisterClassExW(&wc) };
    }
    // SAFETY: the class is registered with this instance; the owner is a window of this thread; no title.
    let w = unsafe {
        CreateWindowExW(EX_STYLE, &class, PCWSTR::null(), WS_POPUP, 0, 0, 0, 0, Some(owner), None, Some(hinst), None)
    }
    .ok()?;
    // SAFETY: our new window's own long; the level's id, no pointer.
    unsafe { SetWindowLongPtrW(w, GWLP_USERDATA, id.0 as isize) };
    Some(w)
}

/// What the pointer did on a popup.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Pointer {
    Move(PointP),
    Leave,
    Down(PointP),
    Up(PointP),
}

/// The pointer on level `id`'s popup (`p` in the popup's client, which is the panel), in the owner's state: the stack
/// moves on, a pick reaches the screen, the deadline timer follows the submenu delay, the popups follow.
pub(crate) fn on_pointer(host: &mut WinHost, id: MenuId, what: Pointer) {
    let row_at = |host: &WinHost, p: PointP| host.disp.menus.level(id).and_then(|l| l.layout.as_ref()?.row_at(p));
    let now = host.lp.now_ms();
    let delay = host.tokens().motion.submenu_delay;
    match what {
        Pointer::Move(p) => {
            let row = row_at(host, p);
            menu::pointer_move(&mut host.disp.menus, id, row, now, delay);
        }
        Pointer::Leave => menu::pointer_move(&mut host.disp.menus, id, None, now, delay),
        Pointer::Down(p) => {
            let row = row_at(host, p);
            menu::pointer_press(&mut host.disp.menus, id, row);
        }
        Pointer::Up(p) => {
            let row = row_at(host, p);
            let d = &mut host.disp;
            menu::pointer_release(&mut d.menus, id, row, &mut d.effects);
            deliver(host);
        }
    }
    host.after_dispatch();
    sync(host);
}

/// The menus' outcomes to the screen, then their reply and effects.
fn deliver(host: &mut WinHost) {
    let tokens = host.tokens();
    let env = host.env(&tokens);
    let reply = {
        let WinHost { screen, disp, res, clipboard, .. } = &mut *host;
        disp.menu_events(&mut **screen, env, res, clipboard)
    };
    host.reply(reply);
    host.apply_pending();
}

/// The popups' window procedure: never activated by a click; the pointer goes to the owner's stack.
unsafe extern "system" fn menu_proc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    let point = || super::input::lparam_point(lp.0);
    let pointer = match msg {
        WM_MOUSEACTIVATE => return LRESULT(MA_NOACTIVATE),
        WM_MOUSEMOVE => {
            let mut t = TRACKMOUSEEVENT {
                cbSize: std::mem::size_of::<TRACKMOUSEEVENT>() as u32,
                dwFlags: TME_LEAVE,
                hwndTrack: hwnd,
                dwHoverTime: 0,
            };
            // SAFETY: a complete TRACKMOUSEEVENT for this window; the call sends no message.
            let _ = unsafe { TrackMouseEvent(&mut t) };
            Pointer::Move(point())
        }
        WM_MOUSELEAVE => Pointer::Leave,
        WM_LBUTTONDOWN | WM_RBUTTONDOWN => Pointer::Down(point()),
        WM_LBUTTONUP | WM_RBUTTONUP => Pointer::Up(point()),
        WM_TIMER if wp.0 == TIMER_FADE => {
            fade_step(hwnd);
            return LRESULT(0);
        }
        WM_OLED_MENU => {
            if let Some(owner) = owner_of(hwnd) {
                let _ = window::with_host(owner, sync);
            }
            return LRESULT(0);
        }
        // A popup is drawn at its owner's DPI: on a monitor of another DPI it keeps its size.
        WM_DPICHANGED => return LRESULT(0),
        // SAFETY: the arguments this procedure received.
        _ => return unsafe { DefWindowProcW(hwnd, msg, wp, lp) },
    };
    if let Some(owner) = owner_of(hwnd) {
        let id = menu_of(hwnd);
        let _ = window::with_host(owner, |h| on_pointer(h, id, pointer));
    }
    LRESULT(0)
}

/// WM_ACTIVATEAPP: another app took the activation (wParam FALSE): every menu and list closes.
pub(crate) fn on_activate_app(hwnd: HWND, wp: WPARAM) {
    if wp.0 != 0 {
        return;
    }
    let _ = window::with_host(hwnd, |h| {
        let d = &mut h.disp;
        menu::close_all(&mut d.menus, &mut d.effects);
        deliver(h);
        h.after_dispatch();
        sync(h);
    });
}

/// The owner is destroyed: its menus go without an outcome, their pixels are freed, and the windows that still exist
/// are destroyed after the borrow.
pub(crate) fn on_owner_destroy(host: &mut WinHost, hwnd: HWND) {
    let _ = hwnd;
    let stack = &mut host.disp.menus;
    let mut dead = std::mem::take(&mut stack.windows.dead);
    dead.extend(stack.windows.popups.drain(..).filter_map(|p| p.hwnd));
    menu::closed_by_screen(stack);
    if !dead.is_empty() {
        host.defer(Sys::Call(Box::new(move |_| {
            for w in dead {
                destroy(w);
            }
        })));
    }
}

/// Effect::OpenMenu: the stack has the menu already (EventCx::open_menu); the windows follow.
pub(crate) fn open(host: &mut WinHost, id: MenuId, items: Vec<MenuItem>, at: Anchor, opener: Option<NodeId>) {
    let _ = (id, items, at, opener);
    sync(host);
}

/// Effect::OpenList: as open.
pub(crate) fn open_list(host: &mut WinHost, id: MenuId, items: Vec<Text>, at: Anchor, highlight: Option<usize>) {
    let _ = (id, items, at, highlight);
    sync(host);
}

/// Effect::UpdateList: as open.
pub(crate) fn update_list(host: &mut WinHost, id: MenuId, items: Vec<Text>, highlight: Option<usize>) {
    let _ = (id, items, highlight);
    sync(host);
}

/// Effect::CloseMenus: the windows of the closed levels go.
pub(crate) fn close_all(host: &mut WinHost) {
    sync(host);
}

/// Harness::menu_image: the menu as its window draws it, laid out and painted by the same code, on transparent pixels.
pub(crate) fn snapshot_image(snap: &MenuSnapshot, cmds: &CommandTable, env: Env<'_>, res: &mut Res) -> Option<Image> {
    let layout = menu::lay_out_items(&snap.items, snap.at, cmds, &env, &mut *res.text);
    let (w, h) = (layout.size.w, layout.size.h);
    if w <= 0 || h <= 0 {
        return None;
    }
    let mut px = vec![0u32; (w as usize) * (h as usize)];
    let mut text_rects = Vec::new();
    {
        let canvas = Canvas { buf: &mut px, w, h, clip: None };
        let disp = Dispatcher::new();
        let mut cx =
            PaintCx::new(env, canvas, PointP::default(), RectP::wh(0, 0, w, h), &disp, res, Some(&mut text_rects));
        menu::paint_panel(&mut cx, &layout, PointP::default(), snap.highlight, None);
    }
    text_rects.sort_by_key(|r| (r.t, r.l, r.r, r.b));
    text_rects.dedup();
    Some(Image { w: w as u32, h: h as u32, px, text_rects })
}

#[cfg(test)]
mod tests {
    use std::cell::RefCell;
    use std::rc::Rc;

    use super::*;
    use crate::command::{CommandId, CommandTable};
    use crate::cx::{EventCx, LayoutCx};
    use crate::dispatch::Input;
    use crate::event::{Event, Key, Mods, MouseButton, Reply};
    use crate::geom::{PointL, SizeL, SizeP};
    use crate::harness::Harness;
    use crate::host::{AppId, EventLoop};
    use crate::node::{Focusable, Node, Role, WidgetId};
    use crate::screen::Screen;
    use crate::strings::Lang;
    use crate::theme::ThemeSpec;
    use windows::Win32::UI::WindowsAndMessaging::{WS_EX_APPWINDOW, WS_EX_TOPMOST, WS_EX_TRANSPARENT};

    /// One monitor's work area and where the owner's client starts on it.
    struct OneDesk {
        origin: PointP,
        work: RectP,
    }

    impl Desk for OneDesk {
        fn client_origin(&self, owner: HWND) -> PointP {
            let _ = owner;
            self.origin
        }

        fn work_area(&self, r: RectP) -> RectP {
            let _ = r;
            self.work
        }
    }

    fn hwnd(n: usize) -> HWND {
        HWND(n as *mut core::ffi::c_void)
    }

    fn custom(id: u32, label: &'static str, danger: bool) -> MenuItem {
        MenuItem::Custom {
            id: CommandId(id),
            label: Text::from(label),
            glyph: None,
            chord: danger.then(|| Text::from("Del")),
            enabled: true,
            checked: None,
            note: None,
            danger,
        }
    }

    type Log = Rc<RefCell<Vec<String>>>;

    /// A row that opens Open, a separator, Sort (Name, Date) and Delete at a right click; the outcomes go to a log.
    struct Sc {
        cmds: CommandTable,
        log: Log,
    }

    impl Screen for Sc {
        fn layout(&mut self, cx: &mut LayoutCx, avail: SizeP) -> SizeP {
            let r = RectP::wh(cx.px(10), cx.px(10), cx.px(300), cx.px(28));
            let n = Node::new(NodeId::new(WidgetId(1), 0), Role::ListItem, r);
            cx.push(n.label(Text::from("row")).focus(Focusable::Tab));
            avail
        }

        fn paint(&self, _cx: &mut PaintCx) {}

        fn event(&mut self, cx: &mut EventCx, ev: &Event) -> Reply {
            match ev {
                Event::Click { button: MouseButton::Right, pos, .. } => {
                    let sort = MenuItem::Submenu {
                        label: Text::from("Sort"),
                        glyph: None,
                        items: vec![custom(2, "Name", false), custom(3, "Date", false)],
                    };
                    let items = vec![custom(1, "Open", false), MenuItem::Separator, sort, custom(4, "Delete", true)];
                    cx.open_menu(items, Anchor::Point(*pos));
                }
                Event::Command(id) => self.log.borrow_mut().push(format!("cmd{}", id.0)),
                Event::MenuClosed(_) => self.log.borrow_mut().push("closed".to_string()),
                _ => {}
            }
            Reply::None
        }

        fn commands(&self) -> &CommandTable {
            &self.cmds
        }
    }

    fn sc(log: &Log) -> Sc {
        Sc { cmds: CommandTable::new(), log: Rc::clone(log) }
    }

    /// The state of a window with no window behind it, and the log of its screen: the window calls it leaves are taken,
    /// never run.
    fn headless(lp: &EventLoop) -> (WinHost, Log) {
        let log = Log::default();
        let mut host = WinHost::headless(lp.shared(), Box::new(sc(&log)), 96, SizeP { w: 400, h: 300 });
        host.layout();
        (host, log)
    }

    fn right_click(host: &mut WinHost, x: i32, y: i32) {
        let p = PointP { x, y };
        host.input(Input::Move(p, Mods::NONE));
        host.input(Input::Down(p, MouseButton::Right, 1, Mods::NONE));
        host.input(Input::Up(p, MouseButton::Right, Mods::NONE));
    }

    fn top(host: &WinHost, row: usize) -> i32 {
        host.disp.menus.menus.last().unwrap().layout.as_ref().unwrap().rows[row].top
    }

    #[test]
    fn the_popup_window_never_activates() {
        for s in [WS_EX_LAYERED, WS_EX_NOACTIVATE, WS_EX_TOOLWINDOW] {
            assert_eq!(EX_STYLE.0 & s.0, s.0, "{s:?}");
        }
        let never = WS_EX_APPWINDOW.0 | WS_EX_TOPMOST.0 | WS_EX_TRANSPARENT.0;
        assert_eq!(EX_STYLE.0 & never, 0, "never topmost, never in the taskbar, takes its clicks");
        assert_eq!((SHOW, CLASS), (SW_SHOWNOACTIVATE, "MiniOledMenu"));
    }

    #[test]
    fn the_hosts_menu_follows_the_stack_and_draws_what_menu_image_draws() {
        let lp = EventLoop::new(AppId("test"), Lang::En, ThemeSpec::oled()).unwrap();
        let (mut host, log) = headless(&lp);
        right_click(&mut host, 50, 20);
        assert_eq!(host.disp.menus.windows.popups.len(), 1, "one popup for the open menu");
        assert!(
            matches!(std::mem::take(&mut host.deferred).as_slice(), [.., Sys::Call(_)]),
            "its window follows later"
        );
        let (w, h, px) = host.disp.menus.windows.popups[0].pixels();
        let tokens = (*host.tokens()).clone();
        let mut harness = Harness::new(sc(&log), SizeL { w: 400, h: 300 }, 96, tokens);
        harness.right_click(PointL { x: 50, y: 20 });
        let img = harness.menu_image(0).expect("the Harness draws the menu");
        assert_eq!((w as u32, h as u32), (img.w, img.h));
        assert!(px == img.px, "the popup's pixels are menu_image's");
        assert_eq!(px[0] >> 24, 0, "a rounded corner is transparent");
        assert_eq!(px[(h / 2 * w + w / 2) as usize] >> 24, 255, "the panel is opaque");
    }

    #[test]
    fn a_menu_is_placed_on_the_work_area_of_its_monitor() {
        let lp = EventLoop::new(AppId("test"), Lang::En, ThemeSpec::oled()).unwrap();
        let (mut host, _) = headless(&lp);
        right_click(&mut host, 50, 20);
        let size = host.disp.menus.menus[0].layout.as_ref().unwrap().size;
        // The client starts at (900, 600) on a 1000 x 700 work area: the point (950, 620) has no room right or below.
        host.disp.menus.menus[0].placed = false;
        let desk = OneDesk { origin: PointP { x: 900, y: 600 }, work: RectP { l: 0, t: 0, r: 1000, b: 700 } };
        assert!(follow(&mut host, &desk));
        let r = host.disp.menus.menus[0].rect;
        let at = PointP { x: 950 - size.w, y: 620 - size.h };
        assert_eq!(PointP { x: r.l + 900, y: r.t + 600 }, at, "flipped left and up, kept in client coordinates");
        assert_eq!(host.disp.menus.windows.popups[0].painted.and_then(|p| p.pos), Some(at), "where the window goes");
    }

    #[test]
    fn a_click_on_a_popup_reaches_the_screen_and_its_window_goes() {
        let lp = EventLoop::new(AppId("test"), Lang::En, ThemeSpec::oled()).unwrap();
        let (mut host, log) = headless(&lp);
        right_click(&mut host, 50, 20);
        let id = host.disp.menus.menus[0].id;
        host.disp.menus.windows.popups[0].hwnd = Some(hwnd(10));
        let at = PointP { x: 30, y: top(&host, 3) + 5 };
        on_pointer(&mut host, id, Pointer::Down(at));
        assert_eq!(host.disp.menus.menus[0].pressed, Some(3), "Delete shows pressed");
        on_pointer(&mut host, id, Pointer::Up(at));
        assert_eq!(*log.borrow(), ["cmd4"]);
        assert!(host.disp.menus.menus.is_empty() && host.disp.menus.windows.popups.is_empty());
        assert_eq!(host.disp.menus.windows.dead, [hwnd(10)], "its window is destroyed after the borrow");
    }

    #[test]
    fn hover_opens_the_submenu_after_the_delay_and_the_deadline_follows() {
        let lp = EventLoop::new(AppId("test"), Lang::En, ThemeSpec::oled()).unwrap();
        let (mut host, _) = headless(&lp);
        right_click(&mut host, 50, 20);
        let id = host.disp.menus.menus[0].id;
        let sort = PointP { x: 30, y: top(&host, 2) + 5 };
        on_pointer(&mut host, id, Pointer::Move(sort));
        assert_eq!(host.disp.menus.menus[0].highlight, Some(2));
        let due = host.deadline.expect("the deadline timer is armed for the submenu delay");
        assert!(host.disp.menus.menus.len() == 1);
        host.disp.on_due(due);
        host.apply_pending();
        assert_eq!(host.disp.menus.menus.len(), 2, "open after the delay");
        sync(&mut host);
        assert_eq!(host.disp.menus.windows.popups.len(), 2, "and it has its window");
        let (root, sub) = (host.disp.menus.menus[0].rect, host.disp.menus.menus[1].rect);
        assert_eq!(sub.l, root.r, "beside its parent");
        // The pointer goes back to Open: the submenu closes after the delay, and no timer is left.
        let open = PointP { x: 30, y: top(&host, 0) + 5 };
        on_pointer(&mut host, id, Pointer::Move(open));
        let due = host.deadline.unwrap();
        host.disp.on_due(due);
        host.apply_pending();
        host.after_dispatch();
        assert_eq!(host.disp.menus.menus.len(), 1);
        assert_eq!(host.deadline, None);
    }

    #[test]
    fn keys_repaint_the_popup_and_enter_picks() {
        let lp = EventLoop::new(AppId("test"), Lang::En, ThemeSpec::oled()).unwrap();
        let (mut host, log) = headless(&lp);
        right_click(&mut host, 50, 20);
        let before = host.disp.menus.windows.popups[0].pixels().2;
        host.input(Input::Key(Key::DOWN, true, false, Mods::NONE));
        sync(&mut host);
        let rev = host.disp.menus.rev;
        assert_eq!(host.disp.menus.windows.popups[0].rev, Some(rev), "painted again at the new revision");
        let after = host.disp.menus.windows.popups[0].pixels().2;
        assert!(before != after, "the highlight shows");
        host.input(Input::Key(Key::ENTER, true, false, Mods::NONE));
        assert_eq!(*log.borrow(), ["cmd1"]);
        assert!(host.disp.menus.windows.popups.is_empty());
    }

    #[test]
    fn deactivation_and_the_owners_destruction_free_the_popups() {
        let lp = EventLoop::new(AppId("test"), Lang::En, ThemeSpec::oled()).unwrap();
        let (mut host, log) = headless(&lp);
        right_click(&mut host, 50, 20);
        host.input(Input::Active(false));
        assert_eq!(*log.borrow(), ["closed"]);
        assert!(host.disp.menus.windows.popups.is_empty());
        host.input(Input::Active(true));
        right_click(&mut host, 50, 20);
        host.disp.menus.windows.popups[0].hwnd = Some(hwnd(11));
        std::mem::take(&mut host.deferred);
        let owner = host.hwnd;
        on_owner_destroy(&mut host, owner);
        assert!(host.disp.menus.menus.is_empty() && host.disp.menus.windows.popups.is_empty());
        assert!(matches!(std::mem::take(&mut host.deferred).as_slice(), [Sys::Call(_)]), "its window is destroyed");
        assert_eq!(log.borrow().len(), 1, "the screen is going: no outcome");
    }

    #[test]
    fn the_fade_runs_out_cubic_and_ends() {
        let f = Fade { start: 100, dur: 140, dy: -4 };
        assert_eq!(f.at(100), Some((0, -4)));
        let (a, dy) = f.at(170).unwrap();
        assert!((215..=230).contains(&a) && dy == -1, "{a} {dy}");
        assert_eq!(f.at(240), None, "over: the timer stops");
        assert_eq!(f.at(50), Some((0, -4)), "a clock before the start counts as the start");
    }
}
