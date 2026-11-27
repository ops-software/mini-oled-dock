//! --list-monitors (K1a.10): one line per monitor: index, device, resolution, DPI and the current refresh rate. It only
//! reads (EnumDisplayMonitors, GetMonitorInfoW, GetDpiForMonitor, EnumDisplaySettingsW with ENUM_CURRENT_SETTINGS) and
//! never changes a mode.

use windows::core::{BOOL, PCWSTR};
use windows::Win32::Foundation::{LPARAM, RECT};
use windows::Win32::Graphics::Gdi::{
    EnumDisplayMonitors, EnumDisplaySettingsW, GetMonitorInfoW, DEVMODEW, ENUM_CURRENT_SETTINGS, HDC, HMONITOR,
    MONITORINFO, MONITORINFOEXW,
};
use windows::Win32::UI::HiDpi::{GetDpiForMonitor, MDT_EFFECTIVE_DPI};

use crate::cli::MeasureOpts;

/// One monitor: "<index> <device> <w>x<h> <dpi> dpi <hz> Hz".
pub fn monitor_line(index: usize, device: &str, w: i32, h: i32, dpi: u32, hz: u32) -> String {
    format!("{index} {device} {w}x{h} {dpi} dpi {hz} Hz")
}

/// A monitor's device name, rect (physical px) and effective DPI.
struct Mon {
    device: String,
    rect: RECT,
    dpi: u32,
}

fn monitors() -> Vec<Mon> {
    unsafe extern "system" fn each(h: HMONITOR, _dc: HDC, _clip: *mut RECT, data: LPARAM) -> BOOL {
        // SAFETY: `data` is the Vec monitors() passed, alive and borrowed by nobody else during the enumeration.
        let out = unsafe { &mut *(data.0 as *mut Vec<Mon>) };
        let mut mi = MONITORINFOEXW::default();
        mi.monitorInfo.cbSize = std::mem::size_of::<MONITORINFOEXW>() as u32;
        // SAFETY: `mi` is a writable MONITORINFOEXW whose cbSize says so.
        if unsafe { GetMonitorInfoW(h, (&mut mi as *mut MONITORINFOEXW).cast::<MONITORINFO>()) }.as_bool() {
            let n = mi.szDevice.iter().position(|c| *c == 0).unwrap_or(mi.szDevice.len());
            let (mut dx, mut dy) = (96u32, 96u32);
            // SAFETY: the out parameters are writable u32s.
            if unsafe { GetDpiForMonitor(h, MDT_EFFECTIVE_DPI, &mut dx, &mut dy) }.is_err() {
                dx = 96;
            }
            out.push(Mon {
                device: String::from_utf16_lossy(&mi.szDevice[..n]),
                rect: mi.monitorInfo.rcMonitor,
                dpi: dx,
            });
        }
        BOOL(1)
    }
    let mut v: Vec<Mon> = Vec::new();
    // SAFETY: the callback only touches `v`, which outlives the synchronous enumeration.
    let _ = unsafe { EnumDisplayMonitors(None, None, Some(each), LPARAM(&mut v as *mut Vec<Mon> as isize)) };
    v
}

/// The current refresh rate of a display device (0 when Windows does not say).
pub(super) fn refresh_hz(device: &str) -> u32 {
    let wide: Vec<u16> = device.encode_utf16().chain([0]).collect();
    let mut dm = DEVMODEW { dmSize: std::mem::size_of::<DEVMODEW>() as u16, ..Default::default() };
    // SAFETY: `wide` is NUL-terminated and outlives the call; `dm` is writable with dmSize set. Read only.
    if unsafe { EnumDisplaySettingsW(PCWSTR(wide.as_ptr()), ENUM_CURRENT_SETTINGS, &mut dm) }.as_bool() {
        dm.dmDisplayFrequency
    } else {
        0
    }
}

/// Prints one line per monitor; exit 1 when there is none.
pub fn run(opts: &MeasureOpts) -> i32 {
    let _ = opts;
    let mons = monitors();
    for (i, m) in mons.iter().enumerate() {
        let (w, h) = (m.rect.right - m.rect.left, m.rect.bottom - m.rect.top);
        println!("{}", monitor_line(i, &m.device, w, h, m.dpi, refresh_hz(&m.device)));
    }
    if mons.is_empty() {
        eprintln!("list-monitors: no monitor");
        return 1;
    }
    0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn monitor_line_format() {
        assert_eq!(monitor_line(0, r"\\.\DISPLAY1", 2560, 1440, 144, 165), r"0 \\.\DISPLAY1 2560x1440 144 dpi 165 Hz");
        assert_eq!(monitor_line(2, "", 1024, 768, 96, 0), "2  1024x768 96 dpi 0 Hz");
    }
}
