//! Whether a delete can go to the Recycle Bin (OP2): pure classify_facts over facts read by gather_facts. The UI
//! shows its bar for every answer but Yes; FOF_WANTNUKEWARNING stays the backstop of every Recycle (OP2 flags).

use oled_trust::frame::files::WidePath;
use windows::core::PCWSTR;
use windows::Win32::Storage::FileSystem::{
    GetDiskFreeSpaceExW, GetDriveTypeW, GetFileAttributesExW, GetFileExInfoStandard, GetVolumeNameForVolumeMountPointW,
    GetVolumePathNameW, FILE_ATTRIBUTE_DIRECTORY, WIN32_FILE_ATTRIBUTE_DATA,
};
use windows::Win32::System::Registry::{RegGetValueW, HKEY_CURRENT_USER, RRF_RT_REG_DWORD};
use windows::Win32::UI::Shell::{SHQueryRecycleBinW, SHQUERYRBINFO};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NoBin {
    Network,
    Removable,
    OverQuota,
    Disabled,
    Unknown,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Recyclability {
    Yes,
    No(NoBin),
}
pub struct BinFacts {
    pub drive_type: u32,
    pub unc: bool,
    pub size: u64,
    pub bin_free: Option<u64>,
    pub bin_disabled: Option<bool>,
}

/// GetDriveTypeW values (winbase.h).
const DRIVE_REMOVABLE: u32 = 2;
const DRIVE_FIXED: u32 = 3;
const DRIVE_REMOTE: u32 = 4;
const DRIVE_CDROM: u32 = 5;

/// Pure. Network and removable volumes first, then the bin's own settings; Yes only when every fact is known.
pub fn classify_facts(f: &BinFacts) -> Recyclability {
    if f.unc || f.drive_type == DRIVE_REMOTE {
        return Recyclability::No(NoBin::Network);
    }
    if f.drive_type == DRIVE_REMOVABLE || f.drive_type == DRIVE_CDROM {
        return Recyclability::No(NoBin::Removable);
    }
    if f.bin_disabled == Some(true) {
        return Recyclability::No(NoBin::Disabled);
    }
    if f.bin_free.is_some_and(|free| f.size > free) {
        return Recyclability::No(NoBin::OverQuota);
    }
    if f.drive_type == DRIVE_FIXED && f.bin_disabled == Some(false) && f.bin_free.is_some() {
        return Recyclability::Yes;
    }
    Recyclability::No(NoBin::Unknown)
}

/// The per-volume Recycle Bin settings of the current user (read only; value names per F 5.5.9, unverified).
const BIN_KEY: &str = "Software\\Microsoft\\Windows\\CurrentVersion\\Explorer\\BitBucket\\Volume\\";

/// OS reads only; off the UI thread. Any read that fails leaves its fact None, so the answer becomes Unknown. The
/// size of a folder is not walked (0): the bar and FOF_WANTNUKEWARNING cover a folder over the bin's quota.
pub fn gather_facts(item: &WidePath) -> BinFacts {
    if item.is_unc() {
        // Network: no read at all (a dead share must not hold the caller).
        return BinFacts { drive_type: DRIVE_REMOTE, unc: true, size: 0, bin_free: None, bin_disabled: None };
    }
    let raw = item.raw();
    let root = volume_root(item, &raw);
    // SAFETY: `root` is NUL-terminated and lives for the call.
    let drive_type = unsafe { GetDriveTypeW(PCWSTR(root.as_ptr())) };
    let size = file_size(&raw).unwrap_or(0);
    let guid = volume_guid(&root);
    let (max_mib, nuke) = match &guid {
        Some(g) => {
            (reg_dword(&format!("{BIN_KEY}{g}"), "MaxCapacity"), reg_dword(&format!("{BIN_KEY}{g}"), "NukeOnDelete"))
        }
        None => (None, None),
    };
    let used = bin_used(&root);
    let bin_free = match (max_mib, used) {
        (Some(mib), Some(used)) => Some((u64::from(mib) << 20).saturating_sub(used)),
        _ => None,
    };
    BinFacts { drive_type, unc: false, size, bin_free, bin_disabled: nuke.map(|n| n != 0) }
}

/// Whether this user turned the Recycle Bin off for the volume of `item` (NukeOnDelete=1 under its BitBucket key, the
/// bin's "Don't move files to the Recycle Bin"). On such a volume the shell deletes a Recycle for good without any
/// dialog, FOF_WANTNUKEWARNING notwithstanding (FS-3 c4 on the disposable runner). False when it cannot be read, and
/// for a share (no read at all).
pub fn bin_turned_off(item: &WidePath) -> bool {
    if item.is_unc() {
        return false;
    }
    let raw = item.raw();
    let root = volume_root(item, &raw);
    volume_guid(&root).and_then(|g| reg_dword(&format!("{BIN_KEY}{g}"), "NukeOnDelete")).is_some_and(|n| n != 0)
}

/// The volume root of `item` ("\\?\X:\" or a mount folder), NUL-terminated; the drive or share root when the
/// volume path cannot be read.
fn volume_root(item: &WidePath, raw: &[u16]) -> Vec<u16> {
    let mut buf = vec![0u16; 1024];
    // SAFETY: `raw` is NUL-terminated; the buffer is writable for its whole length.
    if unsafe { GetVolumePathNameW(PCWSTR(raw.as_ptr()), &mut buf) }.is_ok() {
        let n = buf.iter().position(|&u| u == 0).unwrap_or(buf.len());
        buf.truncate(n);
        if buf.last() != Some(&u16::from(b'\\')) {
            buf.push(u16::from(b'\\'));
        }
        buf.push(0);
        return buf;
    }
    let mut p = item.clone();
    while let Some(parent) = p.parent() {
        p = parent;
    }
    let mut v = p.units().to_vec();
    if v.last() != Some(&u16::from(b'\\')) {
        v.push(u16::from(b'\\'));
    }
    v.push(0);
    v
}

/// "{GUID}" of the volume mounted at `root`.
fn volume_guid(root: &[u16]) -> Option<String> {
    let mut buf = [0u16; 64];
    // SAFETY: `root` is NUL-terminated and ends with a backslash; the buffer is writable for its whole length.
    unsafe { GetVolumeNameForVolumeMountPointW(PCWSTR(root.as_ptr()), &mut buf) }.ok()?;
    let n = buf.iter().position(|&u| u == 0)?;
    let name = String::from_utf16(buf.get(..n)?).ok()?;
    let open = name.find('{')?;
    let close = name.find('}')?;
    name.get(open..=close).map(str::to_string)
}

fn reg_dword(sub: &str, value: &str) -> Option<u32> {
    let sub = windows::core::HSTRING::from(sub);
    let value = windows::core::HSTRING::from(value);
    let mut data = 0u32;
    let mut len = 4u32;
    // SAFETY: both strings are NUL-terminated; `data` is a writable u32 of `len` bytes. A read: nothing is written.
    let r = unsafe {
        RegGetValueW(
            HKEY_CURRENT_USER,
            &sub,
            &value,
            RRF_RT_REG_DWORD,
            None,
            Some(std::ptr::from_mut(&mut data).cast()),
            Some(&mut len),
        )
    };
    r.is_ok().then_some(data)
}

/// Bytes in the bin of the volume at `root`.
fn bin_used(root: &[u16]) -> Option<u64> {
    let mut info = SHQUERYRBINFO { cbSize: std::mem::size_of::<SHQUERYRBINFO>() as u32, ..Default::default() };
    // SAFETY: `root` is NUL-terminated; `info` is writable with its size set.
    unsafe { SHQueryRecycleBinW(PCWSTR(root.as_ptr()), &mut info) }.ok()?;
    u64::try_from(info.i64Size).ok()
}

fn file_size(raw: &[u16]) -> Option<u64> {
    let mut data = WIN32_FILE_ATTRIBUTE_DATA::default();
    // SAFETY: `raw` is NUL-terminated; `data` is the structure GetFileExInfoStandard fills.
    unsafe { GetFileAttributesExW(PCWSTR(raw.as_ptr()), GetFileExInfoStandard, std::ptr::from_mut(&mut data).cast()) }
        .ok()?;
    if data.dwFileAttributes & FILE_ATTRIBUTE_DIRECTORY.0 != 0 {
        return Some(0);
    }
    Some((u64::from(data.nFileSizeHigh) << 32) | u64::from(data.nFileSizeLow))
}

/// Free bytes of the volume at `root` (kept for the UI's own texts; not a classifier fact).
pub fn volume_free(item: &WidePath) -> Option<u64> {
    let raw = item.raw();
    let root = volume_root(item, &raw);
    let mut free = 0u64;
    // SAFETY: `root` is NUL-terminated; `free` is writable.
    unsafe { GetDiskFreeSpaceExW(PCWSTR(root.as_ptr()), Some(&mut free), None, None) }.ok()?;
    Some(free)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn facts(drive_type: u32, unc: bool, size: u64, bin_free: Option<u64>, bin_disabled: Option<bool>) -> BinFacts {
        BinFacts { drive_type, unc, size, bin_free, bin_disabled }
    }

    #[test]
    fn classify_facts_table() {
        use NoBin::*;
        use Recyclability::{No, Yes};
        let rows = [
            (facts(DRIVE_FIXED, true, 1, Some(10), Some(false)), No(Network)),
            (facts(DRIVE_REMOTE, false, 1, Some(10), Some(false)), No(Network)),
            (facts(DRIVE_REMOVABLE, false, 1, Some(10), Some(false)), No(Removable)),
            (facts(DRIVE_CDROM, false, 1, Some(10), Some(false)), No(Removable)),
            (facts(DRIVE_FIXED, false, 1, Some(10), Some(true)), No(Disabled)),
            (facts(DRIVE_FIXED, false, 11, Some(10), Some(false)), No(OverQuota)),
            (facts(DRIVE_FIXED, false, 10, Some(10), Some(false)), Yes),
            (facts(DRIVE_FIXED, false, 0, Some(0), Some(false)), Yes),
            (facts(DRIVE_FIXED, false, 1, None, Some(false)), No(Unknown)),
            (facts(DRIVE_FIXED, false, 1, Some(10), None), No(Unknown)),
            (facts(DRIVE_FIXED, false, 1, None, None), No(Unknown)),
            (facts(0, false, 1, Some(10), Some(false)), No(Unknown)),
            (facts(1, false, 1, Some(10), Some(false)), No(Unknown)),
            (facts(6, false, 1, Some(10), Some(false)), No(Unknown)),
            (facts(DRIVE_REMOTE, false, 1, None, Some(true)), No(Network)),
            (facts(DRIVE_REMOVABLE, false, 1, Some(0), Some(true)), No(Removable)),
            (facts(DRIVE_FIXED, false, 11, Some(10), Some(true)), No(Disabled)),
        ];
        for (f, want) in rows {
            assert_eq!(
                classify_facts(&f),
                want,
                "{} {} {} {:?} {:?}",
                f.drive_type,
                f.unc,
                f.size,
                f.bin_free,
                f.bin_disabled
            );
        }
    }

    #[test]
    fn gather_facts_reads_only() {
        let root = crate::testkit::TempRoot::new("classify");
        let file = root.file("a.txt", b"12345");
        let item = WidePath::parse(&file.display().to_string()).unwrap();
        let before = root.hash_tree("");
        let f = gather_facts(&item);
        assert!(!f.unc);
        assert_eq!(f.size, 5);
        assert!(!matches!(classify_facts(&f), Recyclability::No(NoBin::Network)), "a temp folder is local");
        assert_eq!(root.hash_tree(""), before, "nothing was written");
        let share = gather_facts(&WidePath::parse("\\\\mod-files-absent\\share\\x").unwrap());
        assert_eq!(classify_facts(&share), Recyclability::No(NoBin::Network), "UNC is network, without any read");
    }
}
