//! The hook points (P2-kit 2.13), complete in K0.1b and never edited: each runs the owners' stubs in a fixed order, so
//! no group edits another group's file to plug itself in. Each callee returns its documented neutral value until its
//! owner lands.
#![allow(dead_code)] // Called by the host from K1a.7 on (after_paint_band: the Harness already).

use windows::Win32::Foundation::{HANDLE, HWND};

use super::window::WinHost;
use super::{a11y, caption, frame, ime, menu, pacing, placement, toast, tooltip};
use crate::cx::PaintCx;
use crate::node::NodeId;

/// After the window exists: the frame (KA2), the custom caption (KE2), the IME (KD2), accessibility (KG).
pub(crate) fn after_create(host: &mut WinHost, hwnd: HWND) {
    frame::apply(host, hwnd);
    caption::on_created(host, hwnd);
    ime::on_created(host, hwnd);
    a11y::on_created(host, hwnd);
}

/// After every layout: accessibility (KG), the tooltip (KB).
pub(crate) fn after_layout(host: &mut WinHost) {
    a11y::after_layout(host);
    tooltip::after_layout(host);
}

/// After the screen's paint in each band (the banded presenter and Harness::render): toasts (KD2), the custom
/// caption's buttons (KE2).
pub(crate) fn after_paint_band(cx: &mut PaintCx<'_>) {
    toast::paint(cx);
    caption::paint(cx);
}

/// After the focus moved: the IME (KD2), accessibility (KG).
pub(crate) fn on_focus_changed(host: &mut WinHost, node: Option<NodeId>) {
    ime::on_focus_changed(host, node);
    a11y::on_focus_changed(host, node);
}

/// On WM_DESTROY: placement (KA2), menus (KC2), the tooltip (KB), the IME (KD2), accessibility (KG).
pub(crate) fn on_destroy(host: &mut WinHost, hwnd: HWND) {
    placement::on_destroy(host, hwnd);
    menu::on_owner_destroy(host, hwnd);
    tooltip::on_owner_destroy(host, hwnd);
    ime::on_destroy(host, hwnd);
    a11y::on_destroy(host, hwnd);
}

/// Extra handles for the loop's MsgWaitForMultipleObjectsEx: the frame pacing's (KA2, then KE2).
pub(crate) fn wait_handles(out: &mut Vec<HANDLE>) {
    if let Some(h) = pacing::wait_handle() {
        out.push(h);
    }
}

#[cfg(test)]
mod tests {
    use windows::Win32::Foundation::{HANDLE, HWND, LPARAM, LRESULT, WPARAM};

    use super::super::window::WinHost;
    use super::super::*;
    use crate::cx::{Anchor, AnimId, Ease, Env, MenuId, MenuItem, PaintCx, Res, ToastSpec};
    use crate::dispatch::Effect;
    use crate::event::{Key, Mods};
    use crate::geom::{PointP, RectP};
    use crate::harness::{Image, MenuSnapshot};
    use crate::node::{Node, NodeId, Text};
    use crate::theme::{Ink, TypeRole};
    use crate::widgets::menu::{MenuPress, MenuStack};
    use crate::widgets::tooltip::{TipInput, TipState};

    /// One line per callee of P2-kit 2.13: a callee moved, renamed or retyped fails to compile here.
    #[test]
    #[allow(clippy::type_complexity)]
    fn hook_points_exist() {
        type Msg = fn(HWND, u32, WPARAM, LPARAM) -> LRESULT;
        type MsgOpt = fn(HWND, u32, WPARAM, LPARAM) -> Option<LRESULT>;
        // host/wndproc.rs
        let _f: unsafe extern "system" fn(HWND, u32, WPARAM, LPARAM) -> LRESULT = wndproc::wndproc;
        let _f: Msg = window::on_nccreate;
        let _f: Msg = window::on_ncdestroy;
        let _f: Msg = window::on_size;
        let _f: Msg = window::on_erasebkgnd;
        let _f: Msg = window::on_retry;
        let _f: Msg = crate::present::banded::on_paint;
        let _f: Msg = input::on_input;
        let _f: Msg = dpi::on_dpichanged;
        let _f: Msg = placement::on_exitsizemove;
        let _f: Msg = proxy::drain;
        let _f: Msg = frame::on_setting_change;
        let _f: MsgOpt = a11y::on_getobject;
        let _f: Msg = a11y::on_action;
        let _f: MsgOpt = caption::on_nc;
        let _f: fn(HWND, LPARAM) -> Option<LRESULT> = caption::system_menu;
        let _f: MsgOpt = ime::on_ime;
        let _f: fn(HWND, WPARAM) = menu::on_activate_app;
        // host/hooks.rs
        let _f: fn(&mut WinHost, HWND) = hooks::after_create;
        let _f: fn(&mut WinHost, HWND) = frame::apply;
        let _f: fn(&mut WinHost, HWND) = caption::on_created;
        let _f: fn(&mut WinHost, HWND) = ime::on_created;
        let _f: fn(&mut WinHost, HWND) = a11y::on_created;
        let _f: fn(&mut WinHost) = hooks::after_layout;
        let _f: fn(&mut WinHost) = a11y::after_layout;
        let _f: fn(&mut WinHost) = tooltip::after_layout;
        let _f: fn(&mut PaintCx<'_>) = hooks::after_paint_band;
        let _f: fn(&mut PaintCx<'_>) = toast::paint;
        let _f: fn(&mut PaintCx<'_>) = caption::paint;
        let _f: fn(&mut WinHost, Option<NodeId>) = hooks::on_focus_changed;
        let _f: fn(&mut WinHost, Option<NodeId>) = ime::on_focus_changed;
        let _f: fn(&mut WinHost, Option<NodeId>) = a11y::on_focus_changed;
        let _f: fn(&mut WinHost, HWND) = hooks::on_destroy;
        let _f: fn(&mut WinHost, HWND) = placement::on_destroy;
        let _f: fn(&mut WinHost, HWND) = menu::on_owner_destroy;
        let _f: fn(&mut WinHost, HWND) = tooltip::on_owner_destroy;
        let _f: fn(&mut WinHost, HWND) = ime::on_destroy;
        let _f: fn(&mut WinHost, HWND) = a11y::on_destroy;
        let _f: fn(&mut Vec<HANDLE>) = hooks::wait_handles;
        let _f: fn() -> Option<HANDLE> = pacing::wait_handle;
        // host/window.rs apply_effects
        let _f: fn(&mut WinHost, Vec<Effect>) = window::apply_effects;
        let _f: fn(&mut WinHost, MenuId, Vec<MenuItem>, Anchor, Option<NodeId>) = menu::open;
        let _f: fn(&mut WinHost, MenuId, Vec<Text>, Anchor, Option<usize>) = menu::open_list;
        let _f: fn(&mut WinHost, MenuId, Vec<Text>, Option<usize>) = menu::update_list;
        let _f: fn(&mut WinHost) = menu::close_all;
        let _f: fn(&mut WinHost, RectP, i32) = crate::present::scroll::apply;
        let _f: fn(&mut WinHost, Option<RectP>) = ime::set_caret;
        let _f: fn(&mut WinHost, NodeId, RectP, Text, Option<Text>) = tooltip::show;
        let _f: fn(&mut WinHost) = tooltip::hide;
        let _f: fn(&mut WinHost, AnimId, f32, u32, Ease) = pacing::arm;
        let _f: fn(&mut WinHost) = pacing::disarm;
        let _f: fn(&mut WinHost) = pacing::on_frame_timer;
        // dispatch.rs
        let _f: fn(&mut toast::ToastStack, PointP) -> Option<usize> = toast::hit;
        let _f: fn(&mut toast::ToastStack) -> bool = toast::on_f6;
        let _f: fn(&mut toast::ToastStack, u64, &mut Vec<Effect>) = toast::on_due;
        let _f: fn(&toast::ToastStack) -> Option<u64> = toast::next_due;
        let _f: fn(&mut toast::ToastStack, ToastSpec, u64) = toast::push;
        let _f: fn(&mut MenuStack, PointP, &mut Vec<Effect>) -> MenuPress = crate::widgets::menu::on_pointer_down;
        let _f: fn(&mut MenuStack, Key, Mods, bool, &mut Vec<Effect>) -> bool = crate::widgets::menu::on_key;
        let _f: fn(&MenuStack) -> bool = crate::widgets::menu::is_open;
        let _f: fn(&mut MenuStack, &mut Vec<Effect>) = crate::widgets::menu::close_all;
        let _f: fn(&mut MenuStack, u64, &mut Vec<Effect>) = crate::widgets::menu::on_due;
        let _f: fn(&MenuStack) -> Option<u64> = crate::widgets::menu::next_due;
        let _f: fn(&mut TipState, TipInput, &mut Vec<Effect>) = crate::widgets::tooltip::on_input;
        let _f: fn(&mut TipState, Option<&Node>, u64, &mut Vec<Effect>) = crate::widgets::tooltip::on_hover;
        let _f: fn(&mut TipState, u64, &mut Vec<Effect>) = crate::widgets::tooltip::on_due;
        let _f: fn(&TipState) -> Option<u64> = crate::widgets::tooltip::next_due;
        // harness/mod.rs
        let _f: fn(&mut Image, RectP, i32) -> bool = crate::present::scroll::emulate;
        let _f: fn(&MenuSnapshot, &crate::command::CommandTable, Env<'_>, &mut Res) -> Option<Image> =
            menu::snapshot_image;
        // cx.rs
        let _f: fn(Caption, bool) -> Caption = caption::effective;
        let _f: fn(&mut PaintCx<'_>, &str, PointP, TypeRole, Ink, Ink, bool) -> i32 = crate::text::digits::draw;
        let _f: fn(&mut PaintCx<'_>, i32, &[(i16, i16)], Ink) = crate::gfx::spans::draw;
    }

    #[test]
    fn custom_caption_needs_the_switch() {
        assert_eq!(caption::effective(Caption::Custom, false), Caption::System);
        assert_eq!(caption::effective(Caption::System, true), Caption::System);
        assert_eq!(caption::effective(Caption::Custom, true), Caption::Custom, "the S-CAPTION rig");
    }
}
