//! The loader flags of the built dock.exe (R0c): its static imports are looked up in System32 only, so a DLL planted
//! next to a downloaded copy is never loaded, not even before `main` runs (where `SetDefaultDllDirectories` takes over).
//! A small PE reader, here only: the DOS header, the PE32+ optional header, the load configuration directory.

use std::path::Path;

use windows::Win32::System::Diagnostics::Debug::IMAGE_LOAD_CONFIG_DIRECTORY64;

/// `LOAD_LIBRARY_SEARCH_SYSTEM32`, the value of /DEPENDENTLOADFLAG in build.rs.
const SEARCH_SYSTEM32: u16 = 0x800;
/// `IMAGE_DIRECTORY_ENTRY_LOAD_CONFIG`.
const LOAD_CONFIG: usize = 10;
/// Offset of `DependentLoadFlags` in `IMAGE_LOAD_CONFIG_DIRECTORY64` (winnt.h).
const DEPENDENT_LOAD_FLAGS: usize = 78;

fn u16_at(b: &[u8], at: usize) -> Option<u16> {
    Some(u16::from_le_bytes(b.get(at..at.checked_add(2)?)?.try_into().ok()?))
}

fn u32_at(b: &[u8], at: usize) -> Option<u32> {
    Some(u32::from_le_bytes(b.get(at..at.checked_add(4)?)?.try_into().ok()?))
}

/// `DependentLoadFlags` of a PE32+ image, or why it cannot be read. Total over any bytes.
fn dependent_load_flags(b: &[u8]) -> Result<u16, String> {
    let short = || "the image is cut short".to_string();
    if b.get(..2) != Some(b"MZ".as_slice()) {
        return Err("no MZ header".into());
    }
    let pe = u32_at(b, 0x3c).ok_or_else(short)? as usize;
    if b.get(pe..pe.saturating_add(4)) != Some(b"PE\0\0".as_slice()) {
        return Err("no PE signature".into());
    }
    let coff = pe + 4;
    let sections = usize::from(u16_at(b, coff + 2).ok_or_else(short)?);
    let optional = coff + 20;
    let optional_size = usize::from(u16_at(b, coff + 16).ok_or_else(short)?);
    if u16_at(b, optional) != Some(0x20b) {
        return Err("not a PE32+ image".into());
    }
    if u32_at(b, optional + 108).ok_or_else(short)? as usize <= LOAD_CONFIG {
        return Err("no load configuration directory".into());
    }
    let entry = optional + 112 + LOAD_CONFIG * 8;
    let (rva, size) = (u32_at(b, entry).ok_or_else(short)?, u32_at(b, entry + 4).ok_or_else(short)?);
    if rva == 0 || (size as usize) < DEPENDENT_LOAD_FLAGS + 2 {
        return Err(format!("a load configuration of {size} bytes"));
    }
    for i in 0..sections {
        let s = optional + optional_size + i * 40;
        let (virtual_size, address) = (u32_at(b, s + 8).ok_or_else(short)?, u32_at(b, s + 12).ok_or_else(short)?);
        let raw = u32_at(b, s + 20).ok_or_else(short)?;
        if rva >= address && rva - address < virtual_size {
            let at = (rva - address) as usize + raw as usize;
            // The structure's own Size must cover the field too.
            if (u32_at(b, at).ok_or_else(short)? as usize) < DEPENDENT_LOAD_FLAGS + 2 {
                return Err("the load configuration ends before DependentLoadFlags".into());
            }
            return u16_at(b, at + DEPENDENT_LOAD_FLAGS).ok_or_else(short);
        }
    }
    Err("the load configuration lies in no section".into())
}

#[test]
fn the_exe_asks_system32_for_its_imports() {
    assert_eq!(std::mem::offset_of!(IMAGE_LOAD_CONFIG_DIRECTORY64, DependentLoadFlags), DEPENDENT_LOAD_FLAGS);
    let exe = Path::new(env!("CARGO_BIN_EXE_dock"));
    let bytes = std::fs::read(exe).unwrap_or_else(|e| panic!("{}: {e}", exe.display()));
    assert_eq!(dependent_load_flags(&bytes), Ok(SEARCH_SYSTEM32), "{}", exe.display());
}

#[test]
fn the_reader_is_total() {
    let bytes = std::fs::read(env!("CARGO_BIN_EXE_dock")).unwrap();
    assert!(dependent_load_flags(b"").is_err());
    assert!(dependent_load_flags(b"MZ").is_err());
    // Every cut of the headers, and a few past them, is an error or the right answer, never a panic.
    for n in (0..4096.min(bytes.len())).chain((4096..bytes.len()).step_by(4093)) {
        let r = dependent_load_flags(bytes.get(..n).unwrap_or_default());
        assert!(r.is_err() || r == Ok(SEARCH_SYSTEM32), "cut at {n}: {r:?}");
    }
    let pe = u32_at(&bytes, 0x3c).unwrap() as usize;
    let mut moved = bytes.clone();
    moved[0x3c..0x40].copy_from_slice(&u32::MAX.to_le_bytes());
    assert!(dependent_load_flags(&moved).is_err(), "a PE offset past the end");
    let mut pe32 = bytes.clone();
    pe32[pe + 24..pe + 26].copy_from_slice(&0x10bu16.to_le_bytes());
    assert_eq!(dependent_load_flags(&pe32), Err("not a PE32+ image".to_string()));
}
