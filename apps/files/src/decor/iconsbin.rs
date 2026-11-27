//! icons.bin: the file format, its writer and its checked, read-only mapping (IC1).
//!
//! File icons-<n>.bin in the app's data folder (n a decimal counter, the highest valid n is used, older ones are
//! removed at the next start through OwnDir::remove, never while mapped).
//!
//! Layout, all integers little endian:
//! - header, 64 bytes: magic "MODFICON", version u32 (1), size count u32 (1 to 4), four u16 sizes (ascending, the
//!   unused ones 0), entry count u32, 4 reserved bytes, built FILETIME i64, FNV-1a 64 checksum of the index u64,
//!   16 reserved bytes;
//! - index, entry count entries of 48 bytes sorted by (ext, px): ext 16 UTF-16 units, lower case, NUL padded
//!   (32 bytes), px u16, flags u16 (0), offset u32, len u32, hits u32;
//! - pixel blobs, premultiplied BGRA, px * px * 4 bytes, each at an offset that is a multiple of 16.
//!
//! check validates everything but the pixels: bad pixels draw wrong colours, nothing worse.
// Reads bytes the disk hands back: no unwrap, no expect, no indexing.
#![deny(clippy::unwrap_used, clippy::expect_used, clippy::indexing_slicing)]

use std::collections::HashMap;
use std::io;
use std::path::Path;

use windows::core::HSTRING;
use windows::Win32::Foundation::{CloseHandle, HANDLE};
use windows::Win32::Storage::FileSystem::{
    CreateFileW, FILE_ATTRIBUTE_NORMAL, FILE_GENERIC_READ, FILE_SHARE_DELETE, FILE_SHARE_READ, OPEN_EXISTING,
};
use windows::Win32::System::Memory::{
    CreateFileMappingW, MapViewOfFile, UnmapViewOfFile, FILE_MAP_READ, MEMORY_MAPPED_VIEW_ADDRESS, PAGE_READONLY,
};

use crate::own_files::OwnDir;

pub const MAGIC: [u8; 8] = *b"MODFICON";
pub const VERSION: u32 = 1;
pub const HEADER_LEN: usize = 64;
pub const ENTRY_LEN: usize = 48;
/// UTF-16 units of an extension in the index.
pub const EXT_UNITS: usize = 16;
/// The most sizes one file holds.
pub const MAX_SIZES: usize = 4;
/// The most extensions one file holds.
pub const MAX_EXTS: usize = 256;
/// The largest side of a stored icon.
pub const MAX_PX: u16 = 256;
/// The whole file (budget row files.icons_bin_bytes).
pub const CAP_BYTES: usize = 2 * 1024 * 1024;

const OFF_VERSION: usize = 8;
const OFF_NSIZES: usize = 12;
const OFF_SIZES: usize = 16;
const OFF_COUNT: usize = 24;
const OFF_RESERVED_A: usize = 28;
const OFF_BUILT: usize = 32;
const OFF_SUM: usize = 40;
const OFF_RESERVED_B: usize = 48;

/// Why a file was refused.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum IconsBinError {
    TooShort,
    TooLarge,
    Magic,
    Version,
    Sizes,
    Reserved,
    Count,
    Checksum,
    Ext,
    Order,
    Px,
    Flags,
    Range,
    Len,
}

/// What check learned about a valid file.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Layout {
    pub built: i64,
    pub count: usize,
    pub sizes: Vec<u16>,
    /// The first byte after the index, rounded up to 16: no blob starts before it.
    pub data_start: usize,
}

fn u16_at(b: &[u8], o: usize) -> Option<u16> {
    Some(u16::from_le_bytes(b.get(o..o.checked_add(2)?)?.try_into().ok()?))
}
fn u32_at(b: &[u8], o: usize) -> Option<u32> {
    Some(u32::from_le_bytes(b.get(o..o.checked_add(4)?)?.try_into().ok()?))
}
fn u64_at(b: &[u8], o: usize) -> Option<u64> {
    Some(u64::from_le_bytes(b.get(o..o.checked_add(8)?)?.try_into().ok()?))
}

/// FNV-1a 64.
pub fn fnv1a(bytes: &[u8]) -> u64 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for &b in bytes {
        h ^= u64::from(b);
        h = h.wrapping_mul(0x0000_0100_0000_01b3);
    }
    h
}

fn align16(n: usize) -> usize {
    n.div_ceil(16).saturating_mul(16)
}

/// The index form of an extension: lower case UTF-16, NUL padded. None when it does not fit or holds a dot, a
/// separator or a control character.
pub fn ext_key(ext: &[u8]) -> Option<[u16; EXT_UNITS]> {
    let s = std::str::from_utf8(ext).ok()?;
    let mut out = [0u16; EXT_UNITS];
    let mut n = 0;
    for c in s.chars() {
        if c == '.' || c == '/' || c == '\\' || c == ':' || c.is_control() {
            return None;
        }
        for lc in c.to_lowercase() {
            let mut buf = [0u16; 2];
            for u in lc.encode_utf16(&mut buf).iter() {
                *out.get_mut(n)? = *u;
                n += 1;
            }
        }
    }
    Some(out)
}

/// An index extension holds nothing after its first NUL but NULs, no upper case ASCII, dot, separator or control unit.
fn ext_valid(units: &[u16]) -> bool {
    let mut ended = false;
    for &u in units {
        if u == 0 {
            ended = true;
            continue;
        }
        if ended || u < 0x20 || u == 0x7f || u == u16::from(b'.') || u == u16::from(b'/') || u == u16::from(b'\\') {
            return false;
        }
        if (u16::from(b'A')..=u16::from(b'Z')).contains(&u) {
            return false;
        }
    }
    true
}

fn read_ext(b: &[u8], o: usize) -> Option<[u16; EXT_UNITS]> {
    let mut out = [0u16; EXT_UNITS];
    for (i, slot) in out.iter_mut().enumerate() {
        *slot = u16_at(b, o.checked_add(i.checked_mul(2)?)?)?;
    }
    Some(out)
}

/// One index entry, as stored.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Entry {
    pub ext: [u16; EXT_UNITS],
    pub px: u16,
    pub flags: u16,
    pub offset: u32,
    pub len: u32,
    pub hits: u32,
}

fn entry_at(b: &[u8], i: usize) -> Option<Entry> {
    let o = HEADER_LEN.checked_add(i.checked_mul(ENTRY_LEN)?)?;
    Some(Entry {
        ext: read_ext(b, o)?,
        px: u16_at(b, o + 32)?,
        flags: u16_at(b, o + 34)?,
        offset: u32_at(b, o + 36)?,
        len: u32_at(b, o + 40)?,
        hits: u32_at(b, o + 44)?,
    })
}

/// Validates a whole file image (not the pixels).
pub fn check(b: &[u8]) -> Result<Layout, IconsBinError> {
    use IconsBinError as E;
    if b.len() < HEADER_LEN {
        return Err(E::TooShort);
    }
    if b.len() > CAP_BYTES {
        return Err(E::TooLarge);
    }
    if b.get(..8) != Some(&MAGIC[..]) {
        return Err(E::Magic);
    }
    if u32_at(b, OFF_VERSION) != Some(VERSION) {
        return Err(E::Version);
    }
    let nsizes = u32_at(b, OFF_NSIZES).ok_or(E::TooShort)? as usize;
    if nsizes == 0 || nsizes > MAX_SIZES {
        return Err(E::Sizes);
    }
    let mut sizes = Vec::with_capacity(nsizes);
    for i in 0..MAX_SIZES {
        let s = u16_at(b, OFF_SIZES + i * 2).ok_or(E::TooShort)?;
        if i < nsizes {
            if s == 0 || s > MAX_PX || sizes.last().is_some_and(|&p| p >= s) {
                return Err(E::Sizes);
            }
            sizes.push(s);
        } else if s != 0 {
            return Err(E::Sizes);
        }
    }
    let count = u32_at(b, OFF_COUNT).ok_or(E::TooShort)? as usize;
    if count > MAX_EXTS * MAX_SIZES {
        return Err(E::Count);
    }
    let index_end = HEADER_LEN + count * ENTRY_LEN;
    let index = b.get(HEADER_LEN..index_end).ok_or(E::Count)?;
    let dirty = |o: usize, n: usize| b.get(o..o + n).is_none_or(|s| s.iter().any(|&x| x != 0));
    if dirty(OFF_RESERVED_A, 4) || dirty(OFF_RESERVED_B, HEADER_LEN - OFF_RESERVED_B) {
        return Err(E::Reserved);
    }
    if u64_at(b, OFF_SUM) != Some(fnv1a(index)) {
        return Err(E::Checksum);
    }
    let data_start = align16(index_end);
    let mut prev: Option<([u16; EXT_UNITS], u16)> = None;
    let mut exts = 0usize;
    for i in 0..count {
        let e = entry_at(b, i).ok_or(E::Count)?;
        if !ext_valid(&e.ext) {
            return Err(E::Ext);
        }
        if !sizes.contains(&e.px) {
            return Err(E::Px);
        }
        if e.flags != 0 {
            return Err(E::Flags);
        }
        let off = e.offset as usize;
        let end = off.checked_add(e.len as usize).ok_or(E::Range)?;
        if off < data_start || end > b.len() || !off.is_multiple_of(16) {
            return Err(E::Range);
        }
        if e.len as usize != usize::from(e.px) * usize::from(e.px) * 4 {
            return Err(E::Len);
        }
        match prev {
            Some((pe, pp)) if (pe, pp) >= (e.ext, e.px) => return Err(E::Order),
            Some((pe, _)) if pe == e.ext => {}
            _ => exts += 1,
        }
        prev = Some((e.ext, e.px));
    }
    if exts > MAX_EXTS {
        return Err(E::Count);
    }
    let built = u64_at(b, OFF_BUILT).ok_or(E::TooShort)? as i64;
    Ok(Layout { built, count, sizes, data_start })
}

/// One icon to store.
#[derive(Clone, Debug)]
pub struct Item {
    pub ext: String,
    pub px: u16,
    /// Premultiplied BGRA, px * px * 4 bytes.
    pub bgra: Vec<u8>,
    pub hits: u32,
}

/// Builds the file image in memory. Items that do not fit the rules (extension, size, pixel count) are left out; a
/// duplicate (ext, px) keeps the first; at most MAX_SIZES sizes (the smallest) and MAX_EXTS extensions are kept, and
/// the extensions with the fewest hits go until the file fits CAP_BYTES.
pub fn build(items: Vec<Item>, built: i64) -> Vec<u8> {
    let mut good: Vec<([u16; EXT_UNITS], Item)> = items
        .into_iter()
        .filter_map(|i| {
            let key = ext_key(i.ext.as_bytes())?;
            let n = usize::from(i.px);
            (i.px > 0 && i.px <= MAX_PX && i.bgra.len() == n * n * 4).then_some((key, i))
        })
        .collect();
    let mut sizes: Vec<u16> = good.iter().map(|(_, i)| i.px).collect();
    sizes.sort_unstable();
    sizes.dedup();
    sizes.truncate(MAX_SIZES);
    good.retain(|(_, i)| sizes.contains(&i.px));
    good.sort_by_key(|(k, i)| (*k, i.px));
    good.dedup_by(|b, a| a.0 == b.0 && a.1.px == b.1.px);
    // Rank the extensions: most hits first, then by name; keep the prefix that fits.
    let mut hits: HashMap<[u16; EXT_UNITS], u64> = HashMap::new();
    for (k, i) in &good {
        *hits.entry(*k).or_insert(0) += u64::from(i.hits);
    }
    let mut ranked: Vec<[u16; EXT_UNITS]> = hits.keys().copied().collect();
    ranked.sort_by(|a, b| hits.get(b).cmp(&hits.get(a)).then(a.cmp(b)));
    ranked.truncate(MAX_EXTS);
    let mut kept: Vec<[u16; EXT_UNITS]> = Vec::new();
    let (mut entries, mut blobs) = (0usize, 0usize);
    for k in ranked {
        let (n, bytes) = good
            .iter()
            .filter(|(gk, _)| *gk == k)
            .fold((0usize, 0usize), |(n, b), (_, i)| (n + 1, b + align16(i.bgra.len())));
        if align16(HEADER_LEN + (entries + n) * ENTRY_LEN) + blobs + bytes > CAP_BYTES {
            break;
        }
        entries += n;
        blobs += bytes;
        kept.push(k);
    }
    good.retain(|(k, _)| kept.contains(k));
    let data_start = align16(HEADER_LEN + good.len() * ENTRY_LEN);
    let mut index = Vec::with_capacity(good.len() * ENTRY_LEN);
    let mut data: Vec<u8> = Vec::new();
    for (k, i) in &good {
        let offset = data_start + data.len();
        for u in k {
            index.extend_from_slice(&u.to_le_bytes());
        }
        index.extend_from_slice(&i.px.to_le_bytes());
        index.extend_from_slice(&0u16.to_le_bytes());
        index.extend_from_slice(&(offset as u32).to_le_bytes());
        index.extend_from_slice(&(i.bgra.len() as u32).to_le_bytes());
        index.extend_from_slice(&i.hits.to_le_bytes());
        data.extend_from_slice(&i.bgra);
        data.resize(align16(data.len()), 0);
    }
    let mut out = Vec::with_capacity(data_start + data.len());
    out.extend_from_slice(&MAGIC);
    out.extend_from_slice(&VERSION.to_le_bytes());
    out.extend_from_slice(&(sizes.len() as u32).to_le_bytes());
    for i in 0..MAX_SIZES {
        out.extend_from_slice(&sizes.get(i).copied().unwrap_or(0).to_le_bytes());
    }
    out.extend_from_slice(&(good.len() as u32).to_le_bytes());
    out.extend_from_slice(&[0; 4]);
    out.extend_from_slice(&built.to_le_bytes());
    out.extend_from_slice(&fnv1a(&index).to_le_bytes());
    out.extend_from_slice(&[0; HEADER_LEN - OFF_RESERVED_B]);
    out.extend_from_slice(&index);
    out.resize(data_start, 0);
    out.extend_from_slice(&data);
    out
}

/// The counter in a file name "icons-<n>.bin" (canonical decimal), or None.
fn counter_of(name: &std::ffi::OsStr) -> Option<u32> {
    let s = name.to_str()?;
    let n: u32 = s.strip_prefix("icons-")?.strip_suffix(".bin")?.parse().ok()?;
    (s == name_of(n)).then_some(n)
}

fn name_of(n: u32) -> String {
    format!("icons-{n}.bin")
}

fn counters(dir: &OwnDir) -> Vec<u32> {
    let mut v: Vec<u32> = dir.list("").map(|l| l.iter().filter_map(|n| counter_of(n)).collect()).unwrap_or_default();
    v.sort_unstable_by(|a, b| b.cmp(a));
    v
}

/// Writes the image as icons-<n+1>.bin, n the highest counter present (valid or not); returns the new counter.
pub fn write_next(dir: &OwnDir, bytes: &[u8]) -> io::Result<u32> {
    let n = counters(dir).first().copied().unwrap_or(0).saturating_add(1);
    dir.write_atomic(&name_of(n), bytes)?;
    Ok(n)
}

/// A read-only view of a file, unmapped on drop.
struct View {
    base: MEMORY_MAPPED_VIEW_ADDRESS,
    len: usize,
}

impl View {
    fn bytes(&self) -> &[u8] {
        // SAFETY: the view maps len bytes, read-only, until Drop; a mapped section keeps the file from being
        // truncated, so the pages stay readable.
        unsafe { std::slice::from_raw_parts(self.base.Value.cast::<u8>(), self.len) }
    }
}

impl Drop for View {
    fn drop(&mut self) {
        // SAFETY: mapped in map_file, unmapped once, here.
        let _ = unsafe { UnmapViewOfFile(self.base) };
    }
}

// SAFETY: the view is read-only memory that nothing writes; it can be read from any thread.
unsafe impl Send for View {}
// SAFETY: as above.
unsafe impl Sync for View {}

/// Maps the file read-only (share read and delete, so a later rebuild can replace it) and checks it.
fn map_file(path: &Path) -> Option<(View, Layout)> {
    let md = std::fs::symlink_metadata(path).ok()?;
    let len = usize::try_from(md.len()).ok()?;
    if !md.is_file() || !(HEADER_LEN..=CAP_BYTES).contains(&len) {
        return None;
    }
    let wide = HSTRING::from(path.as_os_str());
    // SAFETY: a NUL-terminated path; read access, share read and delete, no security attributes or template.
    let file: HANDLE = unsafe {
        CreateFileW(
            &wide,
            FILE_GENERIC_READ.0,
            FILE_SHARE_READ | FILE_SHARE_DELETE,
            None,
            OPEN_EXISTING,
            FILE_ATTRIBUTE_NORMAL,
            None,
        )
    }
    .ok()?;
    // SAFETY: a valid file handle; a read-only section of the whole file.
    let section = unsafe { CreateFileMappingW(file, None, PAGE_READONLY, 0, 0, None) };
    // SAFETY: the handle was opened above and is closed once; the section keeps the file.
    let _ = unsafe { CloseHandle(file) };
    let section = section.ok()?;
    // SAFETY: a valid section handle; the whole section, read-only.
    let base = unsafe { MapViewOfFile(section, FILE_MAP_READ, 0, 0, 0) };
    // SAFETY: the section handle is closed once; the view stays valid without it.
    let _ = unsafe { CloseHandle(section) };
    if base.Value.is_null() {
        return None;
    }
    let view = View { base, len };
    let layout = check(view.bytes()).ok()?;
    Some((view, layout))
}

/// A read-only mapping, validated.
pub struct IconsBin {
    view: View,
    layout: Layout,
    counter: u32,
}

impl IconsBin {
    /// The highest valid icons-<n>.bin of the folder, mapped; the other icons-<n>.bin files are removed (this one
    /// is the only one mapped, and a file mapped elsewhere is only marked for deletion).
    pub fn open(data_dir: &Path) -> Option<IconsBin> {
        let dir = OwnDir::open(data_dir).ok()?;
        let all = counters(&dir);
        let found = all.iter().find_map(|&n| {
            let (view, layout) = map_file(&dir.path().join(name_of(n)))?;
            Some(IconsBin { view, layout, counter: n })
        })?;
        for n in all.into_iter().filter(|&n| n != found.counter) {
            let _ = dir.remove(&name_of(n));
        }
        Some(found)
    }

    /// The pixels of ext (lower case, no dot, "" = folder) at px, premultiplied BGRA.
    pub fn get(&self, ext: &[u8], px: u16) -> Option<&[u8]> {
        let key = ext_key(ext)?;
        let b = self.view.bytes();
        let (mut lo, mut hi) = (0usize, self.layout.count);
        while lo < hi {
            let mid = lo + (hi - lo) / 2;
            let e = entry_at(b, mid)?;
            match (e.ext, e.px).cmp(&(key, px)) {
                std::cmp::Ordering::Less => lo = mid + 1,
                std::cmp::Ordering::Greater => hi = mid,
                std::cmp::Ordering::Equal => {
                    let start = e.offset as usize;
                    return b.get(start..start.checked_add(e.len as usize)?);
                }
            }
        }
        None
    }

    /// FILETIME the file was built.
    pub fn built(&self) -> i64 {
        self.layout.built
    }

    /// Index entries (an extension at two sizes is two).
    pub fn entries(&self) -> usize {
        self.layout.count
    }

    /// The counter in the file name.
    pub fn counter(&self) -> u32 {
        self.counter
    }

    /// The sizes the file holds.
    pub fn sizes(&self) -> &[u16] {
        &self.layout.sizes
    }

    /// (ext, px, hits) of every entry, in index order.
    pub fn list(&self) -> Vec<(String, u16, u32)> {
        let b = self.view.bytes();
        (0..self.layout.count)
            .filter_map(|i| {
                let e = entry_at(b, i)?;
                let n = e.ext.iter().position(|&u| u == 0).unwrap_or(EXT_UNITS);
                let s = String::from_utf16(e.ext.get(..n)?).ok()?;
                Some((s, e.px, e.hits))
            })
            .collect()
    }
}
