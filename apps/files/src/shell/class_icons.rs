//! Class icons served by the shell host (IC3: extract::class_icon on the image STA inside an enter guard, one ClassIcon
//! frame per extension, then ReqDone). Until then every request answers E_NOTIMPL.

use oled_trust::frame::files::{ReqId, ShellEvt};
use windows::Win32::Foundation::E_NOTIMPL;

pub fn serve(req: ReqId, exts: &[Box<str>], px: u16, out: &mut dyn FnMut(ShellEvt)) -> Result<(), i32> {
    let _ = (req, exts, px, out);
    Err(E_NOTIMPL.0)
}
