//! Made-up listings close to the approved mockup, for scenes and goldens (FA-WIN, F1a.12): no name from a real
//! machine.

use std::rc::Rc;

use oled_trust::frame::files::Location;
use oled_ui::harness::Harness;

use crate::fakes::{self, Fakes, FT_2026_11_21};
use crate::format::Fmt;
use crate::listing::{Batch, Gen, RawEntry, WidePath};
use crate::window::FilesScreen;

/// The folder every details scene shows.
pub const ROOT: &str = r"C:\made-up\documents";

const ATTR_DIR: u32 = 0x10;
const ATTR_ARCHIVE: u32 = 0x20;
const ATTR_HIDDEN: u32 = 0x2;
const ATTR_REPARSE_POINT: u32 = 0x400;
const ATTR_RECALL_ON_DATA_ACCESS: u32 = 0x40_0000;
const REPARSE_MOUNT_POINT: u32 = 0xA000_0003;

const TICKS_PER_MIN: i64 = 600_000_000;

/// The FakeClock's "now": 2026-11-21 14:40.
pub const NOW: i64 = FT_2026_11_21 + (14 * 60 + 40) * TICKS_PER_MIN;

fn ago_min(m: i64) -> i64 {
    NOW - m * TICKS_PER_MIN
}

fn ago_days(d: i64) -> i64 {
    ago_min(d * 24 * 60)
}

/// The path as the engine takes it.
pub fn wp(s: &str) -> WidePath {
    WidePath::parse(s).expect("a made-up path parses")
}

/// The details scenes' folder: two folders, a junction, a hidden file and six files, dates relative to NOW.
pub fn documents() -> Batch {
    let rows: [(&str, u32, u32, u64, i64); 10] = [
        ("invoices", ATTR_DIR, 0, 0, ago_days(3)),
        ("projects", ATTR_DIR, 0, 0, ago_days(9)),
        ("link-to-projects", ATTR_DIR | ATTR_REPARSE_POINT, REPARSE_MOUNT_POINT, 0, ago_days(30)),
        ("report-final-v2.pdf", ATTR_ARCHIVE, 0, 3_400_000, ago_days(5)),
        ("report-final-v3.pdf", ATTR_ARCHIVE, 0, 3_650_000, ago_min(95)),
        ("contract-draft-for-the-apartment-on-the-long-street-07.docx", ATTR_ARCHIVE, 0, 48_500, ago_days(14)),
        ("photos-raw.zip", ATTR_ARCHIVE | ATTR_RECALL_ON_DATA_ACCESS, 0, 912_000_000, ago_days(40)),
        ("setup-tool-1.4.2.exe", ATTR_ARCHIVE, 0, 74_200_000, ago_days(120)),
        ("notes.txt", ATTR_ARCHIVE, 0, 1_200, ago_min(20)),
        (".settings", ATTR_ARCHIVE | ATTR_HIDDEN, 0, 310, ago_days(200)),
    ];
    let mut b = Batch { gen: Gen(0), names: Vec::new(), entries: Vec::new(), done: true, error: None, elapsed_us: 0 };
    for (name, attrs, reparse_tag, size, mtime) in rows {
        let name_off = b.names.len() as u32;
        b.names.extend_from_slice(name.as_bytes());
        b.entries.push(RawEntry {
            name_off,
            name_len: name.len() as u16,
            attrs,
            reparse_tag,
            size,
            mtime,
            ctime: mtime,
        });
    }
    b
}

/// A window of `fakes` over `dir` with the cell's formatter and its language; the listing (or error) of `dir` must
/// be queued on the fakes before.
pub fn open(fakes: &Fakes, fmt: Fmt, dir: &str) -> FilesScreen {
    let shared = fakes.shared(fmt.lang());
    *shared.fmt.borrow_mut() = Rc::new(fmt);
    let tab = shared.new_tab();
    FilesScreen::new(shared, 1, Location::Fs(wp(dir)), None, tab)
}

/// The first frame, the batches that arrived, the frame after them.
pub fn settle(h: &mut Harness<FilesScreen>, fakes: &Fakes) {
    h.render();
    fakes.pump(h, 1);
    h.render();
}

/// `n` made-up entries (the 100,000 scene).
pub fn many(n: u32) -> Batch {
    fakes::listing_n(n, 7)
}
