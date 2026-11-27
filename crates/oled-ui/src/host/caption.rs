//! The custom caption (K3.5, KE2). An opaque window that asks for Caption::Custom gets it only while the custom caption
//! is on: CUSTOM_CAPTION, which stays false until S-CAPTION passed (only a separate one-line commit flips it), or S-CAPTION's
//! rig through EventLoop::caption_spike. Every other window keeps the system caption and every default here.
//!
//! With the custom caption, WM_NCCALCSIZE keeps the left, right and bottom frame and removes the top caption, so DWM
//! keeps the border, the shadow, the rounded corners and Snap while the client reaches the window's top edge (a
//! maximised window's client starts the frame's thickness lower, on the screen). WM_NCHITTEST maps the client's top:
//! the resize band of a restored window (HTTOP, HTTOPLEFT, HTTOPRIGHT), then the screen's caption_hit (HTCAPTION on
//! empty strip space, HTMINBUTTON, HTMAXBUTTON for the Snap Layouts flyout, HTCLOSE, else HTCLIENT). The buttons are the
//! screen's CaptionButtons widget, painted in the screen's paint: their hover follows WM_NCMOUSEMOVE and
//! WM_NCMOUSELEAVE through the dispatcher, a press is kept from DefWindowProc (which would draw the old buttons), and
//! the action runs on WM_NCLBUTTONUP over the button that took the press, as WM_SYSCOMMAND. Alt+Space opens the system
//! menu through DefWindowProc. A right-click on the strip (HTCAPTION) does not: DefWindowProc shows nothing on a window
//! whose caption WM_NCCALCSIZE removed, so WM_NCRBUTTONUP opens the window's system menu itself (system_menu): the
//! commands enabled for the window's state, tracked at the click, the pick posted as WM_SYSCOMMAND.

use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, POINT, WPARAM};
use windows::Win32::Graphics::Gdi::ScreenToClient;
use windows::Win32::UI::HiDpi::GetSystemMetricsForDpi;
use windows::Win32::UI::Input::KeyboardAndMouse::{TrackMouseEvent, TME_LEAVE, TME_NONCLIENT, TRACKMOUSEEVENT};
use windows::Win32::UI::WindowsAndMessaging::{
    EnableMenuItem, GetSystemMenu, GetWindowLongW, IsIconic, IsZoomed, PostMessageW, SetForegroundWindow,
    SetMenuDefaultItem, SetWindowPos, TrackPopupMenuEx, GWL_STYLE, HTCAPTION, HTCLIENT, HTCLOSE, HTMAXBUTTON,
    HTMINBUTTON, HTSYSMENU, HTTOP, HTTOPLEFT, HTTOPRIGHT, MF_BYCOMMAND, MF_ENABLED, MF_GRAYED, NCCALCSIZE_PARAMS,
    SC_CLOSE, SC_MAXIMIZE, SC_MINIMIZE, SC_MOVE, SC_RESTORE, SC_SIZE, SM_CXPADDEDBORDER, SM_CYFRAME, SWP_FRAMECHANGED,
    SWP_NOACTIVATE, SWP_NOMOVE, SWP_NOOWNERZORDER, SWP_NOSIZE, SWP_NOZORDER, TPM_LEFTALIGN, TPM_RETURNCMD,
    TPM_RIGHTBUTTON, TPM_TOPALIGN, WM_NCCALCSIZE, WM_NCHITTEST, WM_NCLBUTTONDBLCLK, WM_NCLBUTTONDOWN, WM_NCLBUTTONUP,
    WM_NCMOUSELEAVE, WM_NCMOUSEMOVE, WM_NCRBUTTONUP, WM_NULL, WM_SYSCOMMAND, WS_MAXIMIZEBOX, WS_MINIMIZEBOX,
    WS_THICKFRAME,
};

use super::window::{self, Sys, WinHost};
use super::{def_proc, Caption, WindowKind};
use crate::command::CommandId;
use crate::cx::PaintCx;
use crate::dispatch::Input;
use crate::event::Mods;
use crate::geom::{PointP, RectP};
use crate::node::Role;
use crate::screen::CaptionHit;
use crate::widgets::caption_buttons::{ITEM_MAXIMIZED, ITEM_RESTORED};

/// Decided by S-CAPTION (K3.5): false until it passes; only a separate one-line commit sets it true.
pub(crate) const CUSTOM_CAPTION: bool = false;

/// The caption a window really gets: Custom only when `custom_on` (CUSTOM_CAPTION, or S-CAPTION's rig through
/// EventLoop::caption_spike).
pub(crate) fn effective(requested: Caption, custom_on: bool) -> Caption {
    if custom_on {
        requested
    } else {
        Caption::System
    }
}

/// True when the window draws the custom caption.
fn custom(h: &WinHost) -> bool {
    let on = CUSTOM_CAPTION || h.lp.caption_spike.get();
    matches!(h.kind, WindowKind::Opaque { .. }) && effective(h.caption, on) == Caption::Custom
}

/// The frame's thickness at `dpi`: the top resize band of a restored window and a maximised window's top inset.
pub(crate) fn resize_band(dpi: u32) -> i32 {
    // SAFETY: no precondition; system metrics are only read.
    unsafe { GetSystemMetricsForDpi(SM_CYFRAME, dpi) + GetSystemMetricsForDpi(SM_CXPADDEDBORDER, dpi) }
}

/// WM_NCHITTEST for a client point of a custom-caption window (pure): the top `band` px of a restored window resize
/// (its first and last two bands across are the corners), then the screen's answer for the strip.
pub(crate) fn nc_hit(p: PointP, width: i32, band: i32, maximized: bool, strip: CaptionHit) -> u32 {
    if !maximized && p.y < band {
        let corner = 2 * band;
        return if p.x < corner {
            HTTOPLEFT
        } else if p.x >= width - corner {
            HTTOPRIGHT
        } else {
            HTTOP
        };
    }
    match strip {
        CaptionHit::Caption => HTCAPTION,
        CaptionHit::MinButton => HTMINBUTTON,
        CaptionHit::MaxButton => HTMAXBUTTON,
        CaptionHit::CloseButton => HTCLOSE,
        CaptionHit::Client => HTCLIENT,
    }
}

/// The system command of a caption button released over itself (pure): minimise, maximise or restore, close.
pub(crate) fn action(ht: u32, maximized: bool) -> Option<u32> {
    match ht {
        HTMINBUTTON => Some(SC_MINIMIZE),
        HTMAXBUTTON if maximized => Some(SC_RESTORE),
        HTMAXBUTTON => Some(SC_MAXIMIZE),
        HTCLOSE => Some(SC_CLOSE),
        _ => None,
    }
}

/// What a right button released over a part of the window opens (pure): the system menu over the empty strip (and the
/// system-menu corner); the buttons, the resize band and the client (an app's own control in the strip) keep theirs.
pub(crate) fn opens_system_menu(ht: u32) -> bool {
    matches!(ht, HTCAPTION | HTSYSMENU)
}

/// What the wndproc does with a non-client right-button release (pure).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum NcRoute {
    /// The kit opens the window's system menu itself.
    SystemMenu,
    /// DefWindowProc (or the arm for that message) answers.
    Default,
}

/// The route of a non-client message over hit-test code `ht` (pure): a right-button release over the strip opens the menu.
pub(crate) fn nc_route(msg: u32, ht: u32) -> NcRoute {
    if msg == WM_NCRBUTTONUP && opens_system_menu(ht) {
        NcRoute::SystemMenu
    } else {
        NcRoute::Default
    }
}

/// The window's state as the system menu needs it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct MenuState {
    pub maximized: bool,
    pub minimized: bool,
    pub can_size: bool,
    pub can_minimize: bool,
    pub can_maximize: bool,
}

/// Which system commands the menu enables (pure): Restore when maximised or minimised; Move and Size only on a restored
/// window (Size also needs a sizing frame); Minimize unless minimised and the window has the box; Maximize unless
/// maximised and the window has the box; Close always.
pub(crate) fn menu_enabled(s: MenuState) -> [(u32, bool); 6] {
    let restored = !s.maximized && !s.minimized;
    [
        (SC_RESTORE, s.maximized || s.minimized),
        (SC_MOVE, restored),
        (SC_SIZE, restored && s.can_size),
        (SC_MINIMIZE, s.can_minimize && !s.minimized),
        (SC_MAXIMIZE, s.can_maximize && !s.maximized),
        (SC_CLOSE, true),
    ]
}

/// The default (bold) item, as Windows' own system menu has it: Close, and Restore only when minimised. A maximised
/// window shows Close in bold as Alt+Space does, so both ways open the same menu.
pub(crate) fn menu_default(s: MenuState) -> u32 {
    if s.minimized {
        SC_RESTORE
    } else {
        SC_CLOSE
    }
}

fn menu_state(hwnd: HWND) -> MenuState {
    // SAFETY: our window; reads its style and state, sends no message.
    let style = unsafe { GetWindowLongW(hwnd, GWL_STYLE) } as u32;
    MenuState {
        maximized: zoomed(hwnd),
        // SAFETY: as above.
        minimized: unsafe { IsIconic(hwnd) }.as_bool(),
        can_size: style & WS_THICKFRAME.0 != 0,
        can_minimize: style & WS_MINIMIZEBOX.0 != 0,
        can_maximize: style & WS_MAXIMIZEBOX.0 != 0 && style & WS_THICKFRAME.0 != 0,
    }
}

/// WM_NCRBUTTONUP over the strip: the window's system menu at the click (`lp`: screen coordinates, so the right
/// monitor at any DPI), the commands enabled for the state, and the pick posted as WM_SYSCOMMAND, so the system's own
/// move, size, minimise, maximise, restore and close run. No window state is borrowed while the menu's loop runs.
pub(crate) fn system_menu(hwnd: HWND, lp: LPARAM) -> Option<LRESULT> {
    // The guard is per loop (LoopShared::menu_up), so per UI thread: another thread's window keeps its own menu. The
    // Rc is taken out under a short borrow; nothing is borrowed while the menu's loop runs.
    let shared = window::with_host(hwnd, |h| h.lp.clone())?;
    if shared.menu_up.replace(true) {
        return Some(LRESULT(0));
    }
    let pt = POINT { x: (lp.0 & 0xFFFF) as u16 as i16 as i32, y: ((lp.0 >> 16) & 0xFFFF) as u16 as i16 as i32 };
    let state = menu_state(hwnd);
    // SAFETY: our window; the menu belongs to it and is not destroyed here (GetSystemMenu with bRevert FALSE).
    let menu = unsafe { GetSystemMenu(hwnd, false) };
    if menu.is_invalid() {
        shared.menu_up.set(false);
        return None;
    }
    for (cmd, on) in menu_enabled(state) {
        let flag = if on { MF_ENABLED } else { MF_GRAYED };
        // SAFETY: `menu` is our window's system menu; a command it lacks is simply not found.
        let _ = unsafe { EnableMenuItem(menu, cmd, MF_BYCOMMAND | flag) };
    }
    // SAFETY: as above; the default item is only drawn bold.
    let _ = unsafe { SetMenuDefaultItem(menu, menu_default(state), 0) };
    // SAFETY: our window, which the click just made the foreground one when it was not.
    let _ = unsafe { SetForegroundWindow(hwnd) };
    let flags = TPM_RETURNCMD | TPM_RIGHTBUTTON | TPM_LEFTALIGN | TPM_TOPALIGN;
    // SAFETY: a modal loop on this thread; messages for the window arrive in the wndproc, which holds no borrow here.
    let pick = unsafe { TrackPopupMenuEx(menu, flags.0, pt.x, pt.y, hwnd, None) }.0 as u32;
    shared.menu_up.set(false);
    // SAFETY: posting to our own window (the documented WM_NULL after a popup, then the command); no pointers.
    unsafe {
        let _ = PostMessageW(Some(hwnd), WM_NULL, WPARAM(0), LPARAM(0));
        if pick != 0 {
            let _ = PostMessageW(Some(hwnd), WM_SYSCOMMAND, WPARAM(pick as usize), lp);
        }
    }
    Some(LRESULT(0))
}

fn is_button(ht: u32) -> bool {
    matches!(ht, HTMINBUTTON | HTMAXBUTTON | HTCLOSE)
}

fn zoomed(hwnd: HWND) -> bool {
    // SAFETY: reads our window's state; sends no message.
    unsafe { IsZoomed(hwnd) }.as_bool()
}

/// The client point of a non-client message's lParam (screen coordinates, signed 16-bit each).
fn client_point(hwnd: HWND, lp: LPARAM) -> Option<PointP> {
    let mut pt = POINT { x: (lp.0 & 0xFFFF) as u16 as i16 as i32, y: ((lp.0 >> 16) & 0xFFFF) as u16 as i16 as i32 };
    // SAFETY: our window; `pt` is writable.
    unsafe { ScreenToClient(hwnd, &mut pt) }.as_bool().then_some(PointP { x: pt.x, y: pt.y })
}

/// After creation: a custom-caption window's frame is computed again (WM_NCCALCSIZE), once the borrow has ended.
pub(crate) fn on_created(host: &mut WinHost, hwnd: HWND) {
    let _ = hwnd;
    if !custom(host) {
        return;
    }
    host.defer(Sys::Call(Box::new(|hwnd| {
        let flags = SWP_FRAMECHANGED | SWP_NOMOVE | SWP_NOSIZE | SWP_NOZORDER | SWP_NOOWNERZORDER | SWP_NOACTIVATE;
        // SAFETY: our window, with no state borrowed; nothing moves, the frame is recomputed.
        let _ = unsafe { SetWindowPos(hwnd, None, 0, 0, 0, 0, flags) };
    })));
}

/// The non-client messages (the wndproc's NC arms). None: DefWindowProcW, which is every answer of a window without
/// the custom caption.
pub(crate) fn on_nc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> Option<LRESULT> {
    let dpi = window::with_host(hwnd, |h| custom(h).then_some(h.scale.dpi)).flatten()?;
    match msg {
        WM_NCCALCSIZE => calc_size(hwnd, msg, wp, lp, dpi),
        WM_NCHITTEST => hit_test(hwnd, msg, wp, lp, dpi),
        WM_NCMOUSEMOVE => {
            pointer_move(hwnd, lp);
            None
        }
        WM_NCMOUSELEAVE => {
            pointer_leave(hwnd);
            None
        }
        WM_NCLBUTTONDOWN | WM_NCLBUTTONDBLCLK => press(hwnd, wp, lp),
        WM_NCLBUTTONUP => release(hwnd, wp, lp),
        _ if nc_route(msg, wp.0 as u32) == NcRoute::SystemMenu => system_menu(hwnd, lp),
        _ => None,
    }
}

/// WM_NCCALCSIZE with wParam TRUE: the default frame, then the top back where it was (lower by the frame's thickness
/// when maximised, so the strip is on the screen). The caption buttons learn whether the window is maximised.
fn calc_size(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM, dpi: u32) -> Option<LRESULT> {
    if wp.0 == 0 {
        return None;
    }
    let params = lp.0 as *mut NCCALCSIZE_PARAMS;
    // SAFETY: with wParam TRUE, lParam points to the NCCALCSIZE_PARAMS of this message, valid while it runs.
    let top = unsafe { (*params).rgrc[0].top };
    let r = def_proc(hwnd, msg, wp, lp);
    let max = zoomed(hwnd);
    let inset = if max { resize_band(dpi) } else { 0 };
    // SAFETY: as above; DefWindowProcW has returned, so this is the only access.
    unsafe { (*params).rgrc[0].top = top + inset };
    let _ = window::with_host(hwnd, |h| tell_maximized(h, max));
    Some(r)
}

/// The caption buttons' owner (found by the screen's caption_hit at its button nodes) gets its kit command: maximised
/// or restored. The widget changes only when the state did.
fn tell_maximized(h: &mut WinHost, maximized: bool) {
    let centre = |r: RectP| PointP { x: (r.l + r.r) / 2, y: (r.t + r.b) / 2 };
    let owner = h
        .disp
        .frame
        .iter()
        .find(|n| n.role == Role::Button && h.screen.caption_hit(centre(n.rect)) == CaptionHit::MaxButton)
        .map(|n| n.id.owner());
    let Some(owner) = owner else { return };
    let id = CommandId::kit(owner, if maximized { ITEM_MAXIMIZED } else { ITEM_RESTORED });
    let tokens = h.tokens();
    let env = h.env(&tokens);
    let WinHost { screen, disp, res, clipboard, .. } = h;
    let reply = disp.command(&mut **screen, env, res, clipboard, id);
    h.reply(reply);
    h.apply_pending();
    h.after_dispatch();
}

/// WM_NCHITTEST: outside the client (the side and bottom frames) the default; inside it the map of nc_hit.
fn hit_test(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM, dpi: u32) -> Option<LRESULT> {
    let def = def_proc(hwnd, msg, wp, lp);
    if def.0 != HTCLIENT as isize {
        return Some(def);
    }
    let p = client_point(hwnd, lp)?;
    let max = zoomed(hwnd);
    let band = resize_band(dpi);
    let code = window::with_host(hwnd, |h| nc_hit(p, h.client.w, band, max, h.screen.caption_hit(p)))?;
    Some(LRESULT(code as isize))
}

/// WM_NCMOUSEMOVE: the pointer is over the strip; the dispatcher's hover follows it (the buttons' hover fade), and
/// WM_NCMOUSELEAVE is asked for. DefWindowProcW then sees it too (the Snap Layouts flyout over HTMAXBUTTON).
fn pointer_move(hwnd: HWND, lp: LPARAM) {
    let Some(p) = client_point(hwnd, lp) else { return };
    let mut t = TRACKMOUSEEVENT {
        cbSize: std::mem::size_of::<TRACKMOUSEEVENT>() as u32,
        dwFlags: TME_LEAVE | TME_NONCLIENT,
        hwndTrack: hwnd,
        dwHoverTime: 0,
    };
    // SAFETY: `t` is a complete TRACKMOUSEEVENT for our window; asking again while tracking is harmless.
    let _ = unsafe { TrackMouseEvent(&mut t) };
    let _ = window::with_host(hwnd, |h| h.input(Input::Move(p, Mods::NONE)));
}

/// WM_NCMOUSELEAVE: the hover ends and a caption button's press with it (released elsewhere, it does nothing).
fn pointer_leave(hwnd: HWND) {
    let _ = window::with_host(hwnd, |h| {
        end_press(h);
        h.input(Input::Leave);
    });
}

/// A caption button's press ends: its node repaints without the press.
fn end_press(h: &mut WinHost) -> Option<crate::node::NodeId> {
    let id = h.disp.state.pressed.take()?;
    if let Some(r) = h.disp.frame.get(id).map(|n| n.rect) {
        let halo = h.halo(r);
        h.add_damage(halo);
    }
    Some(id)
}

/// WM_NCLBUTTONDOWN or DBLCLK over a caption button: the button shows its press, and DefWindowProcW never sees it
/// (it would draw the old buttons). Elsewhere (HTCAPTION, the resize band) the default moves, sizes or maximises.
fn press(hwnd: HWND, wp: WPARAM, lp: LPARAM) -> Option<LRESULT> {
    if !is_button(wp.0 as u32) {
        return None;
    }
    if let Some(p) = client_point(hwnd, lp) {
        let _ = window::with_host(hwnd, |h| {
            let _ = end_press(h);
            if let Some((id, r)) = h.disp.frame.hit(p).map(|n| (n.id, n.rect)) {
                h.disp.state.pressed = Some(id);
                let halo = h.halo(r);
                h.add_damage(halo);
            }
        });
    }
    Some(LRESULT(0))
}

/// WM_NCLBUTTONUP over a caption button: if it is the one that took the press, its action runs (posted as
/// WM_SYSCOMMAND, so the system's minimise, maximise, restore and close run as for its own buttons).
fn release(hwnd: HWND, wp: WPARAM, lp: LPARAM) -> Option<LRESULT> {
    let ht = wp.0 as u32;
    let p = client_point(hwnd, lp);
    let same = window::with_host(hwnd, |h| {
        let pressed = end_press(h);
        pressed.is_some() && pressed == p.and_then(|p| h.disp.frame.hit(p)).map(|n| n.id)
    })
    .unwrap_or(false);
    if !is_button(ht) {
        return None;
    }
    if let Some(cmd) = action(ht, zoomed(hwnd)).filter(|_| same) {
        // SAFETY: posting to our own window; the message carries no pointer.
        let _ = unsafe { PostMessageW(Some(hwnd), WM_SYSCOMMAND, WPARAM(cmd as usize), LPARAM(0)) };
    }
    Some(LRESULT(0))
}

/// The caption buttons in a band (hooks::after_paint_band): nothing to draw over the screen. They are the screen's
/// CaptionButtons, painted in its own paint, because their hover, press and maximised state live in the widget and the
/// dispatcher, which a band hook does not see.
pub(crate) fn paint(cx: &mut PaintCx<'_>) {
    let _ = cx;
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::command::CommandTable;
    use crate::cx::{EventCx, LayoutCx};
    use crate::event::{Event, Reply};
    use crate::geom::{SizeL, SizeP};
    use crate::harness::Harness;
    use crate::node::WidgetId;
    use crate::screen::{Screen, Widget};
    use crate::theme::Tokens;
    use crate::widgets::CaptionButtons;

    /// An app's chrome: a TITLEBAR_H strip with the caption buttons when the effective caption is Custom (always, with
    /// `force`, as the rig's page does), content below.
    struct Chrome {
        buttons: CaptionButtons,
        force: bool,
        strip: RectP,
        laid: Option<Caption>,
        cmds: CommandTable,
    }

    impl Chrome {
        fn new(force: bool) -> Chrome {
            Chrome {
                buttons: CaptionButtons::new(WidgetId(7)),
                force,
                strip: RectP::EMPTY,
                laid: None,
                cmds: CommandTable::new(),
            }
        }
    }

    impl Screen for Chrome {
        fn layout(&mut self, cx: &mut LayoutCx, avail: SizeP) -> SizeP {
            let c = cx.effective_caption();
            self.laid = Some(c);
            if c == Caption::Custom || self.force {
                self.strip = RectP::wh(0, 0, avail.w, cx.px(cx.tokens().size.titlebar_h));
                self.buttons.layout(cx, self.strip);
            } else {
                self.strip = RectP::EMPTY;
            }
            avail
        }

        fn paint(&self, cx: &mut PaintCx) {
            self.buttons.paint(cx);
        }

        fn event(&mut self, cx: &mut EventCx, ev: &Event) -> Reply {
            let _ = self.buttons.event(cx, ev);
            Reply::None
        }

        fn commands(&self) -> &CommandTable {
            &self.cmds
        }

        fn caption_hit(&self, p: PointP) -> CaptionHit {
            if !self.strip.contains(p) {
                return CaptionHit::Client;
            }
            match self.buttons.hit(p) {
                CaptionHit::Client => CaptionHit::Caption,
                b => b,
            }
        }
    }

    #[test]
    fn nchittest_map() {
        for dpi in [96, 144] {
            for maximized in [false, true] {
                let w = 640 * dpi as i32 / 96;
                let h = Harness::new(Chrome::new(true), SizeL { w: 640, h: 400 }, dpi, Tokens::oled());
                let s = h.screen();
                let band = resize_band(dpi);
                assert!(band > 0 && band < 40, "{dpi}: {band}");
                let px = |v: i32| v * dpi as i32 / 96;
                let at = |x: i32, y: i32| nc_hit(PointP { x, y }, w, band, maximized, s.caption_hit(PointP { x, y }));
                let label = format!("{dpi} dpi, maximised {maximized}");
                // The top resize band of a restored window; a maximised one has none.
                let top = |code: u32| if maximized { None } else { Some(code) };
                for (x, want, strip) in
                    [(1, HTTOPLEFT, HTCAPTION), (w / 2, HTTOP, HTCAPTION), (w - 1, HTTOPRIGHT, HTCLOSE)]
                {
                    let got = at(x, 0);
                    assert_eq!(got, top(want).unwrap_or(strip), "{label}: ({x}, 0)");
                }
                // Below the band: the strip.
                let y = band.max(px(20));
                assert_eq!(at(w / 2, y), HTCAPTION, "{label}: empty strip space");
                assert_eq!(at(w - px(23), y), HTCLOSE, "{label}");
                assert_eq!(at(w - px(46 + 23), y), HTMAXBUTTON, "{label}: the Snap Layouts flyout");
                assert_eq!(at(w - px(92 + 23), y), HTMINBUTTON, "{label}");
                assert_eq!(at(w - px(138) - 1, y), HTCAPTION, "{label}: just left of the buttons");
                assert_eq!(at(w / 2, px(40)), HTCLIENT, "{label}: under the strip");
                assert_eq!(at(w - px(23), px(40)), HTCLIENT, "{label}: under the close button");
            }
        }
    }

    #[test]
    fn effective_caption_is_system_by_default() {
        let h = Harness::new(Chrome::new(false), SizeL { w: 640, h: 400 }, 96, Tokens::oled()).caption(Caption::Custom);
        assert_eq!(h.screen().laid, Some(Caption::System), "Custom was asked for, System is given");
        assert!(!h.a11y_dump().contains("Button"), "no caption node: {}", h.a11y_dump());
        // CUSTOM_CAPTION as committed: true only after S-CAPTION passed, in its own commit.
        assert_eq!(effective(Caption::Custom, CUSTOM_CAPTION), Caption::System);
    }

    #[test]
    fn fallback_constant_switches_effective_caption() {
        assert_eq!(effective(Caption::System, false), Caption::System);
        assert_eq!(effective(Caption::Custom, false), Caption::System, "the fallback: the system caption");
        assert_eq!(effective(Caption::System, true), Caption::System, "a window that asks for the system one keeps it");
        assert_eq!(effective(Caption::Custom, true), Caption::Custom, "on: Custom takes effect");
    }

    #[test]
    fn right_button_opens_the_system_menu_over_the_strip_only() {
        for ht in [HTCAPTION, HTSYSMENU] {
            assert!(opens_system_menu(ht), "{ht}");
        }
        for ht in [HTMINBUTTON, HTMAXBUTTON, HTCLOSE, HTTOP, HTTOPLEFT, HTTOPRIGHT, HTCLIENT] {
            assert!(!opens_system_menu(ht), "{ht} keeps its own answer");
        }
        // Over the strip of the real hit map: the empty space and nothing else.
        let h = Harness::new(Chrome::new(true), SizeL { w: 640, h: 400 }, 96, Tokens::oled());
        let s = h.screen();
        let at = |x: i32, y: i32| nc_hit(PointP { x, y }, 640, resize_band(96), false, s.caption_hit(PointP { x, y }));
        assert!(opens_system_menu(at(300, 20)), "empty strip");
        assert!(!opens_system_menu(at(640 - 20, 20)), "close button");
        assert!(!opens_system_menu(at(300, 60)), "client under the strip");
    }

    #[test]
    fn right_button_release_is_routed() {
        assert_eq!(nc_route(WM_NCRBUTTONUP, HTCAPTION), NcRoute::SystemMenu);
        for ht in [HTCLOSE, HTMAXBUTTON, HTMINBUTTON, HTTOP, HTCLIENT] {
            assert_eq!(nc_route(WM_NCRBUTTONUP, ht), NcRoute::Default, "{ht}");
        }
        assert_eq!(nc_route(WM_NCLBUTTONUP, HTCAPTION), NcRoute::Default, "only the right button");
    }

    #[test]
    fn menu_commands_follow_the_window_state() {
        let on = |s: MenuState, cmd: u32| menu_enabled(s).iter().find(|(c, _)| *c == cmd).map(|(_, e)| *e);
        let restored =
            MenuState { maximized: false, minimized: false, can_size: true, can_minimize: true, can_maximize: true };
        assert_eq!(on(restored, SC_RESTORE), Some(false));
        assert_eq!(on(restored, SC_MOVE), Some(true));
        assert_eq!(on(restored, SC_SIZE), Some(true));
        assert_eq!(on(restored, SC_MINIMIZE), Some(true));
        assert_eq!(on(restored, SC_MAXIMIZE), Some(true));
        assert_eq!(on(restored, SC_CLOSE), Some(true));
        assert_eq!(menu_default(restored), SC_CLOSE);
        let max = MenuState { maximized: true, ..restored };
        assert_eq!(
            [SC_RESTORE, SC_MOVE, SC_SIZE, SC_MINIMIZE, SC_MAXIMIZE].map(|c| on(max, c)),
            [Some(true), Some(false), Some(false), Some(true), Some(false)]
        );
        assert_eq!(menu_default(max), SC_CLOSE, "as Alt+Space's menu: Close is bold on a maximised window");
        let fixed = MenuState { can_size: false, can_maximize: false, ..restored };
        assert_eq!(
            (on(fixed, SC_SIZE), on(fixed, SC_MAXIMIZE), on(fixed, SC_MOVE)),
            (Some(false), Some(false), Some(true))
        );
        let icon = MenuState { minimized: true, ..restored };
        assert_eq!(menu_default(icon), SC_RESTORE);
        assert_eq!(
            (on(icon, SC_RESTORE), on(icon, SC_MINIMIZE), on(icon, SC_MOVE)),
            (Some(true), Some(false), Some(false))
        );
    }

    #[test]
    fn released_buttons_run_their_system_command() {
        assert_eq!(action(HTMINBUTTON, false), Some(SC_MINIMIZE));
        assert_eq!(action(HTMAXBUTTON, false), Some(SC_MAXIMIZE));
        assert_eq!(action(HTMAXBUTTON, true), Some(SC_RESTORE));
        assert_eq!(action(HTCLOSE, true), Some(SC_CLOSE));
        assert_eq!(action(HTCAPTION, false), None);
        assert!(is_button(HTMAXBUTTON) && !is_button(HTCAPTION) && !is_button(HTTOP));
    }
}
