//! The window frame (DWM attributes, the system caption) and the live system settings (K1a.11, KA2).

use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, WPARAM};
use windows::Win32::Graphics::Dwm::{
    DwmSetWindowAttribute, DWMWA_BORDER_COLOR, DWMWA_CAPTION_COLOR, DWMWA_TEXT_COLOR, DWMWA_USE_IMMERSIVE_DARK_MODE,
    DWMWINDOWATTRIBUTE,
};
use windows::Win32::UI::HiDpi::GetSystemMetricsForDpi;
use windows::Win32::UI::WindowsAndMessaging::{
    LoadImageW, SendMessageW, HICON, ICON_BIG, ICON_SMALL, IMAGE_ICON, LR_DEFAULTCOLOR, SM_CXICON, SM_CXSMICON,
    WM_SETICON,
};

use super::window::{self, Sys, WinHost};
use super::{def_proc, WindowKind};

/// A frame attribute's value: a constant, the window's background colour, or its resting rim colour.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum FrameValue {
    Const(u32),
    Bg,
    Rim,
}

/// The system caption's attributes, in the order and with the values of the dock's Settings window: dark mode, caption
/// in the background colour, the border in the dock's resting rim colour, white caption text.
pub(crate) const FRAME_ATTRIBUTES: [(DWMWINDOWATTRIBUTE, FrameValue); 4] = [
    (DWMWA_USE_IMMERSIVE_DARK_MODE, FrameValue::Const(1)),
    (DWMWA_CAPTION_COLOR, FrameValue::Bg),
    (DWMWA_BORDER_COLOR, FrameValue::Rim),
    (DWMWA_TEXT_COLOR, FrameValue::Const(0x00FF_FFFF)),
];

/// An rgb colour as a COLORREF value (0x00BBGGRR).
pub(crate) fn colorref(rgb: (u32, u32, u32)) -> u32 {
    (rgb.0 & 0xFF) | ((rgb.1 & 0xFF) << 8) | ((rgb.2 & 0xFF) << 16)
}

/// The attribute values for a background colour.
pub(crate) fn attribute_values(bg: (u32, u32, u32)) -> [(DWMWINDOWATTRIBUTE, u32); 4] {
    FRAME_ATTRIBUTES.map(|(a, v)| {
        (
            a,
            match v {
                FrameValue::Const(c) => c,
                FrameValue::Bg => colorref(bg),
                FrameValue::Rim => colorref(crate::gfx::panel::rim_rgb(bg)),
            },
        )
    })
}

/// After creation, and when the tokens change: the frame's DWM attributes (opaque windows), and the icon from the
/// spec's resource. The calls run once the borrow has ended (they may send messages to the window).
pub(crate) fn apply(host: &mut WinHost, hwnd: HWND) {
    let _ = hwnd;
    if !matches!(host.kind, WindowKind::Opaque { .. }) {
        return;
    }
    let values = attribute_values(host.tokens().color.bg.rgb);
    let icon = host.icon_resource.take();
    let dpi = host.scale.dpi;
    let loaded: Option<[Option<HICON>; 2]> =
        icon.map(|id| [load_icon(id, SM_CXICON, dpi), load_icon(id, SM_CXSMICON, dpi)]);
    if let Some(icons) = loaded {
        host.icons = icons;
    }
    host.defer(Sys::Call(Box::new(move |hwnd| {
        for (attr, v) in values {
            // SAFETY: `v` lives for the call and the size is that of the DWORD these attributes take.
            let _ = unsafe { DwmSetWindowAttribute(hwnd, attr, (&v as *const u32).cast(), 4) };
        }
        if let Some(icons) = loaded {
            for (which, icon) in [(ICON_BIG, icons[0]), (ICON_SMALL, icons[1])] {
                if let Some(i) = icon {
                    // SAFETY: our window; the icon stays alive with the window (WinHost frees it after WM_NCDESTROY).
                    unsafe { SendMessageW(hwnd, WM_SETICON, Some(WPARAM(which as usize)), Some(LPARAM(i.0 as isize))) };
                }
            }
        }
    })));
}

/// An icon resource of the executable at the system size `metric` for `dpi`.
fn load_icon(
    id: u16,
    metric: windows::Win32::UI::WindowsAndMessaging::SYSTEM_METRICS_INDEX,
    dpi: u32,
) -> Option<HICON> {
    // SAFETY: no precondition.
    let px = unsafe { GetSystemMetricsForDpi(metric, dpi) };
    // SAFETY: MAKEINTRESOURCE(id) names a resource of this executable; the icon is freed by WinHost's drop.
    let h = unsafe {
        LoadImageW(
            Some(window::instance()),
            windows::core::PCWSTR(id as usize as *const u16),
            IMAGE_ICON,
            px,
            px,
            LR_DEFAULTCOLOR,
        )
    };
    h.ok().map(|h| HICON(h.0))
}

/// WM_SETTINGCHANGE, WM_SYSCOLORCHANGE, WM_THEMECHANGED: the system state is read again (SysOverlay, the wheel); a new
/// generation makes every window relayout and repaint at the idle pass.
pub(crate) fn on_setting_change(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    if let Some(cell) = window::cell_of(hwnd) {
        cell.lp.read_system();
    }
    def_proc(hwnd, msg, wp, lp)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dwm_attributes_match_settings_window() {
        // The dock's Settings window sets the same four, in this order, with the same border colour (its own
        // apply_frame reads FRAME_ATTRIBUTES).
        let bg = (0x1C, 0x2D, 0x3E);
        let want = [
            (DWMWA_USE_IMMERSIVE_DARK_MODE, 1),
            (DWMWA_CAPTION_COLOR, 0x003E_2D1C),
            (DWMWA_BORDER_COLOR, colorref(crate::gfx::panel::rim_rgb(bg))),
            (DWMWA_TEXT_COLOR, 0x00FF_FFFF),
        ];
        assert_eq!(attribute_values(bg), want);
        assert_eq!(attribute_values((0, 0, 0))[1].1, 0, "the OLED black caption");
        assert_eq!(attribute_values((0, 0, 0))[2].1, 0x0016_1616, "the rim over black is visible");
    }
}
