//! names.bin: resolved names and one icon size, keyed by the exe path hash, size and last-write time; at most 400
//! entries and 1 MiB, a total parser (open point 11).
//!
//! Layout (little-endian): "TNAM" (u32), version 1 (u16), icon side in pixels (u16), count (u32, at most 400); then per
//! entry: the FNV-1a of the lower-cased exe path (u64), the file size (u64), the last-write time (i64, FILETIME), the name
//! (u16 UTF-16 units, then the units), has_icon (u8) and, when set, side x side x 4 bytes of premultiplied BGRA.

use std::path::Path;

use oled_sys::datadir::{read_file, write_atomic, FileAcl};
use windows::core::HSTRING;
use windows::Win32::Storage::FileSystem::{GetFileAttributesExW, GetFileExInfoStandard, WIN32_FILE_ATTRIBUTE_DATA};

/// The file's name in our local folder.
pub const FILE_NAME: &str = "names.bin";
/// Most entries and most bytes of the file; the parser refuses more and the writer drops the oldest to fit.
pub const MAX_ENTRIES: usize = 400;
pub const MAX_BYTES: usize = 1 << 20;
/// Longest name (UTF-16 units) and largest icon side (pixels) the parser accepts.
pub const MAX_NAME_UNITS: usize = 256;
pub const MAX_SIDE: u16 = 64;
/// New entries after which the resolver writes the file again.
pub const WRITE_EVERY: u32 = 50;

const MAGIC: [u8; 4] = *b"TNAM";
const VERSION: u16 = 1;
const HEADER: usize = 12;

/// FNV-1a of the lower-cased path (the key of an entry).
pub fn path_hash(exe: &str) -> u64 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for b in exe.to_lowercase().bytes() {
        h ^= u64::from(b);
        h = h.wrapping_mul(0x0000_0100_0000_01b3);
    }
    h
}

/// The size and the last-write time of a file (FILETIME ticks), None when it cannot be read.
pub fn stamp_of(path: &str) -> Option<(u64, i64)> {
    let wide = HSTRING::from(path);
    let mut data = WIN32_FILE_ATTRIBUTE_DATA::default();
    // SAFETY: `wide` is NUL-terminated and lives for the call; `data` is writable and has the size the class asks for.
    unsafe {
        GetFileAttributesExW(&wide, GetFileExInfoStandard, std::ptr::addr_of_mut!(data).cast()).ok()?;
    }
    let size = (u64::from(data.nFileSizeHigh) << 32) | u64::from(data.nFileSizeLow);
    let time = (i64::from(data.ftLastWriteTime.dwHighDateTime) << 32) | i64::from(data.ftLastWriteTime.dwLowDateTime);
    Some((size, time))
}

/// One resolved program.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Entry {
    pub hash: u64,
    pub size: u64,
    pub mtime: i64,
    pub name: String,
    /// Premultiplied 0xAARRGGBB pixels, side x side, when the entry has an icon.
    pub icon: Option<Vec<u32>>,
}

/// The file in memory: the icon side and the entries, the most recently used first.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct NamesBin {
    pub side: u16,
    pub entries: Vec<Entry>,
    /// Entries added since the last write.
    pub fresh: u32,
}

/// Why a file was discarded (one log line, no content).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Bad {
    TooLarge,
    Magic,
    Version,
    Side,
    Count,
    Truncated,
    Name,
}

impl Bad {
    pub fn as_str(self) -> &'static str {
        match self {
            Bad::TooLarge => "too large",
            Bad::Magic => "not a names file",
            Bad::Version => "unknown version",
            Bad::Side => "bad icon size",
            Bad::Count => "too many entries",
            Bad::Truncated => "truncated",
            Bad::Name => "bad name",
        }
    }
}

struct Reader<'a> {
    b: &'a [u8],
    at: usize,
}

impl<'a> Reader<'a> {
    fn take(&mut self, n: usize) -> Result<&'a [u8], Bad> {
        let end = self.at.checked_add(n).ok_or(Bad::Truncated)?;
        let s = self.b.get(self.at..end).ok_or(Bad::Truncated)?;
        self.at = end;
        Ok(s)
    }

    fn arr<const N: usize>(&mut self) -> Result<[u8; N], Bad> {
        self.take(N)?.try_into().map_err(|_| Bad::Truncated)
    }

    fn u8(&mut self) -> Result<u8, Bad> {
        Ok(self.arr::<1>()?[0])
    }

    fn u16(&mut self) -> Result<u16, Bad> {
        Ok(u16::from_le_bytes(self.arr()?))
    }

    fn u32(&mut self) -> Result<u32, Bad> {
        Ok(u32::from_le_bytes(self.arr()?))
    }

    fn u64(&mut self) -> Result<u64, Bad> {
        Ok(u64::from_le_bytes(self.arr()?))
    }
}

/// Total: every length is checked against the bytes that remain; the count, the names and the icon side are capped.
pub fn parse(bytes: &[u8]) -> Result<NamesBin, Bad> {
    if bytes.len() > MAX_BYTES {
        return Err(Bad::TooLarge);
    }
    let mut r = Reader { b: bytes, at: 0 };
    if r.arr::<4>()? != MAGIC {
        return Err(Bad::Magic);
    }
    if r.u16()? != VERSION {
        return Err(Bad::Version);
    }
    let side = r.u16()?;
    if side > MAX_SIDE {
        return Err(Bad::Side);
    }
    let count = r.u32()? as usize;
    if count > MAX_ENTRIES {
        return Err(Bad::Count);
    }
    let pixels = usize::from(side) * usize::from(side);
    let mut entries = Vec::with_capacity(count.min(MAX_ENTRIES));
    for _ in 0..count {
        let hash = r.u64()?;
        let size = r.u64()?;
        let mtime = i64::from_le_bytes(r.arr()?);
        let units = usize::from(r.u16()?);
        if units > MAX_NAME_UNITS {
            return Err(Bad::Name);
        }
        let raw = r.take(units * 2)?;
        let wide: Vec<u16> = raw.chunks_exact(2).map(|c| u16::from_le_bytes([c[0], c[1]])).collect();
        let name = String::from_utf16(&wide).map_err(|_| Bad::Name)?;
        let icon = match r.u8()? {
            0 => None,
            1 if pixels > 0 => {
                let raw = r.take(pixels * 4)?;
                Some(raw.chunks_exact(4).map(|c| u32::from_le_bytes([c[0], c[1], c[2], c[3]])).collect())
            }
            _ => return Err(Bad::Truncated),
        };
        entries.push(Entry { hash, size, mtime, name, icon });
    }
    Ok(NamesBin { side, entries, fresh: 0 })
}

/// The file's bytes: at most 400 entries and 1 MiB, the most recently used (the first) kept.
pub fn encode(b: &NamesBin) -> Vec<u8> {
    let pixels = usize::from(b.side) * usize::from(b.side);
    let mut body = Vec::new();
    let mut count = 0u32;
    for e in b.entries.iter().take(MAX_ENTRIES) {
        let name: Vec<u16> = e.name.encode_utf16().take(MAX_NAME_UNITS).collect();
        let icon = e.icon.as_ref().filter(|p| p.len() == pixels && pixels > 0);
        let size = 8 + 8 + 8 + 2 + name.len() * 2 + 1 + icon.map_or(0, |_| pixels * 4);
        if HEADER + body.len() + size > MAX_BYTES {
            break;
        }
        body.extend(e.hash.to_le_bytes());
        body.extend(e.size.to_le_bytes());
        body.extend(e.mtime.to_le_bytes());
        body.extend((name.len() as u16).to_le_bytes());
        body.extend(name.iter().flat_map(|u| u.to_le_bytes()));
        match icon {
            Some(p) => {
                body.push(1);
                body.extend(p.iter().flat_map(|px| px.to_le_bytes()));
            }
            None => body.push(0),
        }
        count += 1;
    }
    let mut out = Vec::with_capacity(HEADER + body.len());
    out.extend(MAGIC);
    out.extend(VERSION.to_le_bytes());
    out.extend(b.side.to_le_bytes());
    out.extend(count.to_le_bytes());
    out.extend(body);
    out
}

impl NamesBin {
    /// An empty book for icons of this side.
    pub fn new(side: u16) -> NamesBin {
        NamesBin { side, ..NamesBin::default() }
    }

    fn position(&self, exe: &str) -> Option<usize> {
        let h = path_hash(exe);
        self.entries.iter().position(|e| e.hash == h)
    }

    /// The entry of `exe` when its size and last-write time are those the file has now (`stamp`); a stale one is dropped.
    /// A hit moves to the front (most recently used).
    pub fn lookup(&mut self, exe: &str, stamp: impl Fn(&str) -> Option<(u64, i64)>) -> Option<&Entry> {
        let i = self.position(exe)?;
        let now = stamp(exe);
        if now != Some((self.entries[i].size, self.entries[i].mtime)) {
            self.entries.remove(i);
            return None;
        }
        let e = self.entries.remove(i);
        self.entries.insert(0, e);
        self.entries.first()
    }

    fn slot(&mut self, exe: &str, stamp: (u64, i64)) -> &mut Entry {
        match self.position(exe) {
            Some(i) if (self.entries[i].size, self.entries[i].mtime) == stamp => {
                let e = self.entries.remove(i);
                self.entries.insert(0, e);
            }
            found => {
                if let Some(i) = found {
                    self.entries.remove(i);
                }
                self.fresh += 1;
                self.entries.insert(
                    0,
                    Entry { hash: path_hash(exe), size: stamp.0, mtime: stamp.1, name: String::new(), icon: None },
                );
                self.entries.truncate(MAX_ENTRIES);
            }
        }
        &mut self.entries[0]
    }

    /// Records the name of `exe` (file stamp `stamp`).
    pub fn record_name(&mut self, exe: &str, stamp: (u64, i64), name: &str) {
        self.slot(exe, stamp).name = name.to_string();
    }

    /// Records the icon of `exe` at `side`: icons of another side are dropped (the file holds one size).
    pub fn record_icon(&mut self, exe: &str, stamp: (u64, i64), side: u16, pixels: &[u32]) {
        if side == 0 || side > MAX_SIDE || pixels.len() != usize::from(side) * usize::from(side) {
            return;
        }
        if side != self.side {
            self.side = side;
            for e in &mut self.entries {
                e.icon = None;
            }
        }
        self.slot(exe, stamp).icon = Some(pixels.to_vec());
    }

    /// true after `WRITE_EVERY` new entries since the last write.
    pub fn due(&self) -> bool {
        self.fresh >= WRITE_EVERY
    }
}

/// <local>\names.bin, parsed; None when there is no file, it cannot be read or it is damaged (one log line).
pub fn load(local: Option<&Path>) -> Option<NamesBin> {
    let path = local?.join(FILE_NAME);
    let bytes = match read_file(&path, MAX_BYTES as u64, true) {
        Ok(b) => b,
        Err(e) => {
            if e.kind() != std::io::ErrorKind::NotFound {
                oled_sys::log!("tasks: names.bin unreadable ({:?})", e.kind());
            }
            return None;
        }
    };
    match parse(&bytes) {
        Ok(b) => Some(b),
        Err(bad) => {
            oled_sys::log!("tasks: names.bin discarded ({})", bad.as_str());
            None
        }
    }
}

/// Writes <local>\names.bin atomically (the folder's DACL), and counts the entries as written.
pub fn save(local: Option<&Path>, b: &mut NamesBin) {
    let Some(dir) = local else { return };
    match write_atomic(&dir.join(FILE_NAME), &encode(b), FileAcl::Inherit) {
        Ok(()) => b.fresh = 0,
        Err(_) => oled_sys::log!("tasks: names.bin not written"),
    }
}
