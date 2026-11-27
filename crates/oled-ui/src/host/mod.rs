//! The window host (F-K1, P2-kit 2.5): the EventLoop, windows, the proxy and the hook points of 2.13. K0.1b writes the
//! types and every cross-module call as a one-line delegation to a stub in its owner's file; KA2 (K1a.7-K1a.11) builds
//! the loop and the window.

pub(crate) mod a11y;
pub(crate) mod caption;
pub(crate) mod clipboard;
pub(crate) mod dpi;
pub(crate) mod frame;
pub(crate) mod hooks;
pub(crate) mod ime;
pub(crate) mod input;
pub(crate) mod menu;
pub(crate) mod pacing;
pub(crate) mod placement;
pub(crate) mod proxy;
pub(crate) mod run;
pub(crate) mod toast;
pub(crate) mod tooltip;
pub(crate) mod window;
pub(crate) mod wndproc;

use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, WPARAM};
use windows::Win32::UI::WindowsAndMessaging::{DefWindowProcW, WM_APP};

use crate::geom::SizeL;
use crate::node::Text;
pub use placement::{Placement, PlacementKey, PlacementStore};
pub use proxy::Proxy;
pub use run::{EventLoop, LoopCx, LoopStats, Show};

/// An app's id: "tasks", "files", "gallery" (X11 names).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct AppId(pub &'static str);

/// Which caption a window draws.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Caption {
    System,
    Custom,
}

/// An opaque main window or a layered popup.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WindowKind {
    Opaque { min: SizeL, caption: Caption },
    Layered { activate: bool, topmost: bool },
}

/// How a window is opened.
pub struct WindowSpec {
    pub kind: WindowKind,
    /// "MiniOled<App>".
    pub class: &'static str,
    /// The app name only.
    pub title: Text,
    pub size: SizeL,
    pub owner: Option<HWND>,
    /// Default false (X17).
    pub esc_closes: bool,
    pub placement: Option<PlacementKey>,
    pub icon_resource: Option<u16>,
    /// Centred on the pointer's monitor at size (5.3.1).
    pub opened_by_chord: bool,
}

/// A window of the EventLoop.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct WindowRef(pub u32);

/// A handle the EventLoop waits on.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct HandleId(pub u32);

/// A message for Screen::data.
pub type AppMsg = Box<dyn std::any::Any + Send>;

/// Why the host could not do something.
#[derive(Debug)]
pub enum UiError {
    Win32(windows::core::Error),
    TooManyHandles,
    NoSuchWindow,
    Gfx(String),
    NotReady(&'static str),
}

/// Replays a message that arrived while the window's state was borrowed (K1a.7).
pub(crate) const WM_OLED_RETRY: u32 = WM_APP + 1;
/// Proxy::send's wake-up: drain the window's message queue into Screen::data.
pub(crate) const WM_OLED_DATA: u32 = WM_APP + 2;
/// An accessibility action marshalled to the UI thread (KG).
pub(crate) const WM_OLED_A11Y: u32 = WM_APP + 3;

/// DefWindowProcW: what every host stub returns until its owner fills it.
pub(crate) fn def_proc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    // SAFETY: called only from the window procedure with the arguments it received.
    unsafe { DefWindowProcW(hwnd, msg, wp, lp) }
}
