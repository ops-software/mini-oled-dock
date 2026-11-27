//! Captures one process snapshot into an .osnap file with every private image name overwritten. Read-only: it asks
//! the system for the process list and writes the one file named by --out (never over an existing file).
//! File format: 32-byte header (b"OSMSNAP1", build u32, entries u32, base u64, len u32, reserved u32 = 0), then len bytes.
use oled_sysmon::snapshot::{sanitize_len, SnapshotReader, INVENTED_BASE};
use oled_sysmon::ProcTable;
use std::io::Write;
use std::path::{Component, Path};
use windows::Wdk::System::SystemServices::RtlGetVersion;
use windows::Win32::System::SystemInformation::OSVERSIONINFOW;

const MAGIC: &[u8; 8] = b"OSMSNAP1";

fn header(build: u32, entries: u32, base: u64, len: u32) -> [u8; 32] {
    let mut h = [0u8; 32];
    h[0..8].copy_from_slice(MAGIC);
    h[8..12].copy_from_slice(&build.to_le_bytes());
    h[12..16].copy_from_slice(&entries.to_le_bytes());
    h[16..24].copy_from_slice(&base.to_le_bytes());
    h[24..28].copy_from_slice(&len.to_le_bytes());
    h
}

/// The output must name a file inside an existing folder, with no parent-folder step in the path.
fn check_out(path: &Path) -> Result<(), &'static str> {
    if path.file_name().is_none() {
        return Err("--out must name a file");
    }
    if path.components().any(|c| matches!(c, Component::ParentDir)) {
        return Err("--out must not contain ..");
    }
    match path.parent() {
        Some(p) if p.as_os_str().is_empty() || p.is_dir() => Ok(()),
        _ => Err("the folder of --out does not exist"),
    }
}

fn build_number() -> u32 {
    let mut v =
        OSVERSIONINFOW { dwOSVersionInfoSize: std::mem::size_of::<OSVERSIONINFOW>() as u32, ..Default::default() };
    // SAFETY: v is a valid local OSVERSIONINFOW with its size set.
    let _ = unsafe { RtlGetVersion(&mut v) };
    v.dwBuildNumber
}

fn run(args: &[String]) -> Result<String, String> {
    let out = match args {
        [flag, path] if flag == "--out" => Path::new(path),
        _ => return Err("usage: capture --out <new file>".into()),
    };
    check_out(out).map_err(str::to_string)?;
    let mut reader = SnapshotReader::new();
    let mut table = ProcTable::new();
    reader.read(&mut table).map_err(|e| e.to_string())?;
    let (raw, base) = reader.raw();
    let mut copy = raw.to_vec();
    let (_, kept) = sanitize_len(&mut copy, base).map_err(|e| e.to_string())?;
    copy.truncate(kept);
    let len = u32::try_from(copy.len()).map_err(|_| "the snapshot is too large".to_string())?;
    let head = header(build_number(), reader.stats().entries, INVENTED_BASE as u64, len);
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(out)
        .map_err(|e| format!("cannot create the output file: {e}"))?;
    file.write_all(&head).and_then(|_| file.write_all(&copy)).map_err(|e| format!("write failed: {e}"))?;
    Ok(format!("{} {}", out.display(), reader.stats().entries))
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match run(&args) {
        Ok(line) => println!("{line}"),
        Err(e) => {
            eprintln!("error: {e}");
            std::process::exit(2)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn header_layout() {
        let h = header(26300, 7, 0x1122_3344_5566_7788, 4096);
        assert_eq!(&h[0..8], b"OSMSNAP1");
        assert_eq!(u32::from_le_bytes(h[8..12].try_into().unwrap()), 26300);
        assert_eq!(u32::from_le_bytes(h[12..16].try_into().unwrap()), 7);
        assert_eq!(u64::from_le_bytes(h[16..24].try_into().unwrap()), 0x1122_3344_5566_7788);
        assert_eq!(u32::from_le_bytes(h[24..28].try_into().unwrap()), 4096);
        assert_eq!(&h[28..32], &[0, 0, 0, 0]);
    }

    #[test]
    fn out_path_rules() {
        let dir = std::env::temp_dir();
        assert!(check_out(&dir.join("a.osnap")).is_ok());
        assert!(check_out(Path::new("a.osnap")).is_ok());
        assert!(check_out(&dir.join("..").join("a.osnap")).is_err());
        assert!(check_out(&dir.join("missing-folder-for-the-test").join("a.osnap")).is_err());
        assert!(check_out(Path::new("")).is_err());
        assert!(check_out(Path::new("..")).is_err());
    }

    #[test]
    fn usage_is_checked_before_anything_is_read() {
        assert!(run(&[]).is_err());
        assert!(run(&["--out".to_string()]).is_err());
        assert!(run(&["--to".to_string(), "x".to_string()]).is_err());
    }
}
