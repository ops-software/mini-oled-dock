//! S-CAPTION's rig (K3.5): `--measure caption` opens the caption page on the primary monitor with Caption::Custom
//! taking effect for this run only (EventLoop::caption_spike; CUSTOM_CAPTION stays as committed) and runs until the
//! window is closed. The live tester drives it (LIVE-K3 step 2: the Snap Layouts flyout over our maximise button,
//! moving by the empty strip, double-click, the system menu by right-click and Alt+Space, the top edge, the UIA names of
//! the three buttons); this mode only prints what it opened. The page is opened as it is (not wrapped), so its
//! caption_hit reaches the host.

use oled_ui::host::{AppId, Caption, EventLoop, Placement, PlacementKey};
use oled_ui::strings::{Lang, UiLang};
use oled_ui::theme::ThemeSpec;
use windows::Win32::Foundation::POINT;
use windows::Win32::Graphics::Gdi::{
    GetMonitorInfoW, MonitorFromPoint, MONITORINFO, MONITORINFOEXW, MONITOR_DEFAULTTOPRIMARY,
};
use windows::Win32::UI::HiDpi::{GetDpiForMonitor, MDT_EFFECTIVE_DPI};

use super::anim::OnMonitor;
use crate::app::spec;
use crate::cli::MeasureOpts;
use crate::pages;

/// The primary monitor's device name and effective DPI.
fn primary() -> Option<(String, u32)> {
    // SAFETY: MonitorFromPoint with MONITOR_DEFAULTTOPRIMARY always returns a monitor.
    let h = unsafe { MonitorFromPoint(POINT { x: 0, y: 0 }, MONITOR_DEFAULTTOPRIMARY) };
    let mut mi = MONITORINFOEXW::default();
    mi.monitorInfo.cbSize = std::mem::size_of::<MONITORINFOEXW>() as u32;
    // SAFETY: `mi` is a writable MONITORINFOEXW whose cbSize says so.
    if !unsafe { GetMonitorInfoW(h, (&mut mi as *mut MONITORINFOEXW).cast::<MONITORINFO>()) }.as_bool() {
        return None;
    }
    let n = mi.szDevice.iter().position(|c| *c == 0).unwrap_or(mi.szDevice.len());
    let (mut dx, mut dy) = (96u32, 96u32);
    // SAFETY: the out parameters are writable u32s.
    if unsafe { GetDpiForMonitor(h, MDT_EFFECTIVE_DPI, &mut dx, &mut dy) }.is_err() {
        dx = 96;
    }
    Some((String::from_utf16_lossy(&mi.szDevice[..n]), dx))
}

/// The line this mode prints when the window is open.
pub fn caption_line(page: &str) -> String {
    format!("{{\"page\":\"{page}\",\"caption\":\"custom\"}}")
}

pub fn run(opts: &MeasureOpts) -> i32 {
    let _ = opts;
    let Some(page) = pages::by_name("caption", UiLang::EN) else {
        eprintln!("measure caption: the caption page is not ready");
        return 2;
    };
    let mut lp = match EventLoop::new(AppId("gallery"), Lang::En, ThemeSpec::oled()) {
        Ok(lp) => lp,
        Err(e) => {
            eprintln!("measure caption: {e:?}");
            return 1;
        }
    };
    lp.caption_spike();
    let size = page.size;
    if let Some((monitor, dpi)) = primary() {
        let rect = oled_ui::geom::RectP::wh(64, 64, size.w, size.h);
        lp.set_placement_store(Box::new(OnMonitor(Placement { monitor, rect, dpi, maximized: false })));
    }
    let key = Some(PlacementKey("caption"));
    if let Err(e) = lp.open(spec("Gallery", size, Caption::Custom, key), page.screen) {
        eprintln!("measure caption: {e:?}");
        return 1;
    }
    println!("{}", caption_line(page.name));
    lp.run()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_rig_line_is_json() {
        let v: serde_json::Value = serde_json::from_str(&caption_line("caption")).unwrap();
        assert_eq!((v["page"].as_str(), v["caption"].as_str()), (Some("caption"), Some("custom")));
    }

    #[test]
    fn the_rig_page_is_ready_and_answers_caption_hit() {
        use oled_ui::harness::Harness;
        use oled_ui::theme::Tokens;
        let p = pages::by_name("caption", UiLang::EN).expect("the caption page");
        let h = Harness::new(p.screen, p.size, 96, Tokens::oled());
        let hit = h.screen().caption_hit(oled_ui::geom::PointP { x: 20, y: 10 });
        assert_eq!(
            hit,
            oled_ui::screen::CaptionHit::Caption,
            "opened unwrapped, the page's caption_hit reaches the host"
        );
    }
}
