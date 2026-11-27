//! The system clipboard behind EventCx::clipboard (K1a.8, KA2): read only on an explicit paste, written only on copy or cut.
#![allow(dead_code)] // Reached once the host creates windows (K1a.7).

use crate::cx::Clipboard;

/// The Windows clipboard. Filled by K1a.8; until then empty and refusing.
pub(crate) struct SysClipboard;

impl Clipboard for SysClipboard {
    fn text(&mut self) -> Option<String> {
        None
    }

    fn set_text(&mut self, s: &str) -> bool {
        let _ = s;
        false
    }

    fn has_files(&mut self) -> bool {
        false
    }
}
