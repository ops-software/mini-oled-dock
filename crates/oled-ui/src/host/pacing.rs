//! The frame clock's pacing (K1a.8 candidate A, then K3.4 candidate B, KA2 then KE2): a timer exists only while a tween lives.
#![allow(dead_code)] // Reached once the host creates windows (K1a.7).

use windows::Win32::Foundation::HANDLE;

use super::window::WinHost;
use crate::cx::{AnimId, Ease};

/// Effect::Animate: the tween starts and the frame clock is armed. Until K1a.8: no timer.
pub(crate) fn arm(host: &mut WinHost, id: AnimId, to: f32, dur_ms: u32, ease: Ease) {
    let _ = (host, id, to, dur_ms, ease);
}

/// No tween left: the frame clock stops. Until K1a.8: nothing.
pub(crate) fn disarm(host: &mut WinHost) {
    let _ = host;
}

/// One frame of the clock. Until K1a.8: nothing.
pub(crate) fn on_frame_timer(host: &mut WinHost) {
    let _ = host;
}

/// The handle the loop also waits on (candidate B's waitable timer). Until K3.4: none.
pub(crate) fn wait_handle() -> Option<HANDLE> {
    None
}
