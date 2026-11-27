//! Class icons served by the shell host (IC3): extract::class_icon on the image STA inside an enter guard, one
//! ClassIcon frame per extension that has an icon, then the caller's ReqDone.

use oled_trust::frame::files::{CallKind, Pixels, ReqId, ShellEvt, MAX_PIXELS_SIDE};
use windows::Win32::Foundation::E_INVALIDARG;

use crate::decor::extract;
use crate::shell::crumbs;

pub fn serve(req: ReqId, exts: &[Box<str>], px: u16, out: &mut dyn FnMut(ShellEvt)) -> Result<(), i32> {
    if px == 0 || px > MAX_PIXELS_SIDE {
        return Err(E_INVALIDARG.0);
    }
    for ext in exts {
        let icon = {
            let _g = crumbs::enter(CallKind::ClassIcon, ext);
            extract::class_icon(ext, px)
        };
        let Some(bgra) = icon else { continue };
        out(ShellEvt::ClassIcon { req, ext: ext.clone(), px: Pixels { w: px, h: px, bgra: bgra.into() } });
    }
    Ok(())
}
