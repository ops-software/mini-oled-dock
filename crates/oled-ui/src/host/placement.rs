//! Window placement (P2-kit 2.5; K1a.10, KA2): where a window reopens, per monitor and DPI.

use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, WPARAM};

use super::def_proc;
use super::window::WinHost;
use crate::geom::RectP;

/// The key a window's placement is stored under.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct PlacementKey(pub &'static str);

/// Where a window was.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Placement {
    /// The monitor's device name.
    pub monitor: String,
    /// Logical px, relative to that monitor's work area.
    pub rect: RectP,
    pub dpi: u32,
    pub maximized: bool,
}

/// Where an app keeps its windows' placements.
pub trait PlacementStore {
    fn load(&self, key: PlacementKey) -> Option<Placement>;
    fn save(&mut self, key: PlacementKey, p: &Placement);
}

/// WM_EXITSIZEMOVE: saves the placement. Filled by K1a.10.
#[allow(dead_code)]
pub(crate) fn on_exitsizemove(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    def_proc(hwnd, msg, wp, lp)
}

/// On destroy: saves the placement. Filled by K1a.10.
#[allow(dead_code)]
pub(crate) fn on_destroy(host: &mut WinHost, hwnd: HWND) {
    let _ = (host, hwnd);
}
