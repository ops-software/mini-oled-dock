//! Class icons per (ext, px) over icons.bin and a source, the decor thread or the shell host (IC3). Filled by F-ICONS.

use crate::shell_client::{ShellClient, ShellReply};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ClassIconSource {
    InProcess,
    ShellHost,
}

/// Premultiplied BGRA, px * px * 4 bytes.
pub enum IconRef {
    Ready(std::sync::Arc<[u8]>),
    Pending,
    Generic,
}

/// A cache per (ext, px) over IconsBin and the source.
pub struct ClassIcons {
    _private: (),
}

impl ClassIcons {
    pub fn new(
        source: ClassIconSource,
        data_dir: std::path::PathBuf,
        shell: Option<std::sync::Arc<ShellClient>>,
        notify: std::sync::Arc<dyn Fn() + Send + Sync>,
    ) -> Self {
        let _ = (source, data_dir, shell, notify);
        todo!("F-ICONS")
    }
    /// Never blocks.
    pub fn get(&self, ext: &[u8], px: u16) -> IconRef {
        let _ = (ext, px);
        todo!("F-ICONS")
    }
    /// A painted row: counts toward the top 256.
    pub fn note(&self, ext: &[u8]) {
        let _ = ext;
        todo!("F-ICONS")
    }
    /// True if it was a ClassIcon of ours.
    pub fn on_shell_reply(&self, r: &ShellReply) -> bool {
        let _ = r;
        todo!("F-ICONS")
    }
    pub fn assoc_changed(&self) {
        todo!("F-ICONS")
    }
    /// Weekly; call after the first frame.
    pub fn rebuild_if_due(&self) {
        todo!("F-ICONS")
    }
}

/// FS-6 sets it (live check LIVE-F1): until then class icons come from the shell host and icons.bin is read in the UI.
pub const CLASS_ICONS_IN_UI: bool = false;
