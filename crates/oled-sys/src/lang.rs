//! The it.lang string file format (F-P12).
// PF1 freeze: the signatures below are frozen; bodies arrive with W4b1.
#![allow(dead_code, unused_variables)]

pub const LANG_MAGIC: [u8; 4] = *b"OLNG";
pub const LANG_FORMAT: u16 = 1;
pub const LANG_MAX_BYTES: usize = 1024 * 1024;
pub const LANG_MAX_KEYS: u32 = 16_384;
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum LangError {
    Magic,
    Format(u16),
    Short,
    Count,
    Order,
    Span,
    Utf8,
    Tag,
    TooLarge,
}
pub struct LangFile {
    _private: (),
}
impl LangFile {
    pub fn parse(bytes: Vec<u8>) -> Result<LangFile, LangError> {
        unimplemented!("PF1 stub")
    } // total; validates everything once, then get() cannot fail
    pub fn tag(&self) -> &str {
        unimplemented!("PF1 stub")
    } // "it"
    pub fn suite_version(&self) -> (u16, u16, u16) {
        unimplemented!("PF1 stub")
    }
    #[allow(clippy::len_without_is_empty)] // the frozen interface has no is_empty
    pub fn len(&self) -> usize {
        unimplemented!("PF1 stub")
    }
    pub fn get(&self, hash: u64) -> Option<&str> {
        unimplemented!("PF1 stub")
    } // binary search
}
pub fn encode(tag: &str, suite_version: (u16, u16, u16), entries: &[(u64, &str)]) -> Result<Vec<u8>, LangError> {
    unimplemented!("PF1 stub")
} // sorts by hash
  // layout (SPEC-platform 5.3.8, details here): 0 "OLNG" | 4 u16 format = 1 | 6 u16 reserved = 0 | 8 u32 count | 12 u32 blob_len
  // | 16 [u8; 2] tag (ASCII lowercase) | 18 u16 major | 20 u16 minor | 22 u16 patch | 24 count x (u64 hash, u32 offset, u32 len)
  // | blob (UTF-8); all little-endian; hashes strictly ascending; every span inside the blob and valid UTF-8; nothing after the blob.
