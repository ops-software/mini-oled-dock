//! The tooltip and name-chip window (K1b.4, KB).
#![allow(dead_code)] // Reached once the host creates windows (K1a.7).

use windows::Win32::Foundation::HWND;

use super::window::WinHost;
use crate::geom::RectP;
use crate::node::{NodeId, Text};

/// After every layout: the shown tip follows its node. Until KB: nothing.
pub(crate) fn after_layout(host: &mut WinHost) {
    let _ = host;
}

/// The owner is destroyed: the tip window goes too. Until KB: nothing.
pub(crate) fn on_owner_destroy(host: &mut WinHost, hwnd: HWND) {
    let _ = (host, hwnd);
}

/// Effect::ShowTip. Until KB: nothing.
pub(crate) fn show(host: &mut WinHost, node: NodeId, rect: RectP, text: Text, chord: Option<Text>) {
    let _ = (host, node, rect, text, chord);
}

/// Effect::HideTip. Until KB: nothing.
pub(crate) fn hide(host: &mut WinHost) {
    let _ = host;
}
