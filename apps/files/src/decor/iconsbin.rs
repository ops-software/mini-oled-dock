//! icons.bin: the file format, its writer and its checked, read-only mapping (IC1). Filled by F-ICONS.

/// A read-only mapping, validated.
pub struct IconsBin {
    _private: (),
}

impl IconsBin {
    pub fn open(data_dir: &std::path::Path) -> Option<IconsBin> {
        let _ = data_dir;
        todo!("F-ICONS")
    }
    pub fn get(&self, ext: &[u8], px: u16) -> Option<&[u8]> {
        let _ = (ext, px);
        todo!("F-ICONS")
    }
    pub fn built(&self) -> i64 {
        todo!("F-ICONS")
    }
    pub fn entries(&self) -> usize {
        todo!("F-ICONS")
    }
}
