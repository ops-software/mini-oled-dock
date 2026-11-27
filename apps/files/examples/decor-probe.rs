//! The icons.bin probe (IC4): builds a file from the system's class icons, maps it and prints what it holds.
//!
//!   decor-probe build <dir> [px ...]   extracts a fixed list of extensions at the sizes (default 16 and 24), writes
//!                                      the next icons-<n>.bin into <dir>
//!   decor-probe map <dir>              opens the highest valid file: time to map and check it
//!   decor-probe stats <dir>            entries, sizes, bytes and hits per extension
//!
//! <dir> is an absolute folder given by the caller; nothing else on the machine is written. Class icons are read
//! through the shell (read-only calls on a thread with COM); no window is shown.
use std::path::{Path, PathBuf};
use std::time::Instant;

use files_engine::decor::extract::class_icon;
use files_engine::decor::iconsbin::{build, check, write_next, IconsBin, Item};
use files_engine::own_files::OwnDir;
use windows::Win32::System::Com::{CoInitializeEx, COINIT_APARTMENTTHREADED};

const EXTS: [&str; 24] = [
    "", "txt", "md", "json", "toml", "rs", "html", "css", "js", "pdf", "doc", "docx", "xls", "xlsx", "png", "jpg",
    "gif", "zip", "7z", "exe", "dll", "mp3", "mp4", "iso",
];

fn filetime_now() -> i64 {
    let unix = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_nanos() / 100);
    (unix as i64) + 116_444_736_000_000_000
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let (Some(cmd), Some(dir)) = (args.first(), args.get(1)) else {
        eprintln!("usage: decor-probe build|map|stats <absolute dir> [px ...]");
        std::process::exit(2);
    };
    let dir = PathBuf::from(dir);
    let code = match cmd.as_str() {
        "build" => run_build(&dir, &args[2..]),
        "map" => run_map(&dir),
        "stats" => run_stats(&dir),
        _ => {
            eprintln!("unknown command {cmd}");
            2
        }
    };
    std::process::exit(code);
}

fn run_build(dir: &Path, sizes: &[String]) -> i32 {
    let sizes: Vec<u16> =
        if sizes.is_empty() { vec![16, 24] } else { sizes.iter().filter_map(|s| s.parse().ok()).collect() };
    // SAFETY: one init for the life of this process.
    let _ = unsafe { CoInitializeEx(None, COINIT_APARTMENTTHREADED) };
    let started = Instant::now();
    let mut items = Vec::new();
    for ext in EXTS {
        for &px in &sizes {
            if let Some(bgra) = class_icon(ext, px) {
                items.push(Item { ext: ext.to_string(), px, bgra, hits: 1 });
            }
        }
    }
    let extracted = items.len();
    let took = started.elapsed();
    let bytes = build(items, filetime_now());
    let Ok(own) = OwnDir::open(dir) else {
        eprintln!("cannot open the folder");
        return 1;
    };
    match write_next(&own, &bytes) {
        Ok(n) => {
            println!("extracted {extracted} icons in {took:?}; wrote icons-{n}.bin, {} bytes", bytes.len());
            0
        }
        Err(e) => {
            eprintln!("cannot write: {e}");
            1
        }
    }
}

fn run_map(dir: &Path) -> i32 {
    let started = Instant::now();
    let bin = IconsBin::open(dir);
    let opened = started.elapsed();
    let Some(bin) = bin else {
        println!("no valid icons-<n>.bin in the folder");
        return 1;
    };
    println!(
        "open (list, map, check): {opened:?}; icons-{}.bin, {} entries, built {}",
        bin.counter(),
        bin.entries(),
        bin.built()
    );
    let name = format!("icons-{}.bin", bin.counter());
    if let Ok(bytes) = std::fs::read(dir.join(&name)) {
        let t = Instant::now();
        let ok = check(&bytes).is_ok();
        println!(
            "check alone over {} bytes: {:?} ({})",
            bytes.len(),
            t.elapsed(),
            if ok { "valid" } else { "invalid" }
        );
    }
    let t = Instant::now();
    let found = EXTS.iter().filter(|e| bin.get(e.as_bytes(), 16).is_some()).count();
    println!("{found} of {} lookups at 16 px hit, {:?}", EXTS.len(), t.elapsed());
    0
}

fn run_stats(dir: &Path) -> i32 {
    let Some(bin) = IconsBin::open(dir) else {
        println!("no valid icons-<n>.bin in the folder");
        return 1;
    };
    println!("icons-{}.bin: {} entries, sizes {:?}, built {}", bin.counter(), bin.entries(), bin.sizes(), bin.built());
    for (ext, px, hits) in bin.list() {
        let len = bin.get(ext.as_bytes(), px).map_or(0, <[u8]>::len);
        println!("{:>8} {px:>4} px {len:>8} bytes {hits:>6} hits", if ext.is_empty() { "(folder)" } else { &ext });
    }
    0
}
