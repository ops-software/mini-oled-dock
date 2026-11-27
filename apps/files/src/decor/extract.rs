//! Class icon extraction at the exact size (IC2). Filled by F-ICONS.

/// The class icon of `ext` ("" with the folder attribute for a folder) at `px`, premultiplied BGRA (px * px * 4).
pub fn class_icon(ext: &str, px: u16) -> Option<Vec<u8>> {
    let _ = (ext, px);
    todo!("F-ICONS")
}
