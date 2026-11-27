//! make-icon: draws the application icon of the files app, one PNG per size, into an existing folder:
//!
//!   make-icon <folder>
//!
//! A black OLED tile (8 px margin at 256, radius 52.8, a 2 px #3A3A3A rim, as the dock's icon) with the kit's folder
//! glyph in white. The sizes are 16, 20, 24, 32, 40, 48, 64 and 256; the files are files-<size>.png. The
//! PNGs are packed into assets/files.ico with PowerShell (ICONDIR, then one ICONDIRENTRY per PNG, then the PNGs). A
//! release-built tool, never shipped; the folder must exist (exit 2 otherwise).

use oled_ui::harness::Image;
use oled_ui::{Canvas, Glyph};
use windows::Win32::Foundation::RECT;

const SIZES: [i32; 8] = [16, 20, 24, 32, 40, 48, 64, 256];

/// The icon at `size` px: premultiplied pixels, transparent outside the tile.
fn draw(size: i32) -> Image {
    let mut px = vec![0u32; (size * size) as usize];
    let mut c = Canvas { buf: &mut px, w: size, h: size, clip: None };
    let k = size as f32 / 256.0;
    let (margin, radius) = ((8.0 * k).round() as i32, (52.8 * k).round() as i32);
    // The rim is 2 px at 256; from 32 px up it keeps at least one whole pixel.
    let rim = if size >= 32 { ((2.0 * k).round() as i32).max(1) } else { 0 };
    let tile = RECT { left: margin, top: margin, right: size - margin, bottom: size - margin };
    c.fill_rr(tile, radius, (0x3A, 0x3A, 0x3A), 255);
    let inner = RECT { left: tile.left + rim, top: tile.top + rim, right: tile.right - rim, bottom: tile.bottom - rim };
    c.fill_rr(inner, (radius - rim).max(0), (0, 0, 0), 255);
    // The folder, a little over half the tile, centred.
    let glyph = ((size as f32 * 0.58).round() as i32).max(8);
    let at = (size - glyph) / 2;
    c.glyph(at, at, glyph, (255, 255, 255), 255, &Glyph::Folder.shape());
    Image { w: size as u32, h: size as u32, px, text_rects: Vec::new() }
}

fn main() {
    let Some(dir) = std::env::args().nth(1).map(std::path::PathBuf::from) else {
        eprintln!("usage: make-icon <existing folder>");
        std::process::exit(2);
    };
    if !dir.is_dir() {
        eprintln!("make-icon: {} is not an existing folder", dir.display());
        std::process::exit(2);
    }
    for size in SIZES {
        let file = dir.join(format!("files-{size}.png"));
        if let Err(e) = draw(size).write_png(&file) {
            eprintln!("make-icon: {}: {e}", file.display());
            std::process::exit(1);
        }
    }
}
