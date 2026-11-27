//! ui-probe: the live tester's helper for the details window. A release-built tool, never shipped.
//!
//!   ui-probe make --root <LiveRoot path> --n <n> [--size <bytes>]
//!   ui-probe memory --entries <n>
//!
//! make creates n files (f000001.bin, ...) of <size> bytes (0 by default) in a folder the tester made: <root> must be
//! a mod-files-live-* folder under %TEMP% (LiveRoot::at refuses anything else). memory builds the same model the
//! window holds, headless (the entry store inside the table model inside the kit's virtual table, nothing drawn), and
//! prints the process's PrivateUsage: informative, no gate. Bad arguments exit 2.

#[path = "common/live.rs"]
mod live;

use std::path::PathBuf;
use std::rc::Rc;

use files_engine::fakes;
use files_engine::format::{Fmt, LocalTime};
use files_engine::listing::{EntryStore, Gen, WidePath};
use files_engine::model::{FilesTable, ViewOpts};
use files_engine::tab::TABLE_ID;
use files_engine::types::TypeNames;
use files_engine::view::columns::default_columns;
use files_engine::window::private_bytes;
use oled_ui::model::{ListModel, SelectionMode, TableStyle};
use oled_ui::widgets::VirtualTable;
use oled_ui::UiLang;

/// What the command line asks for.
#[derive(Debug, PartialEq, Eq)]
enum Args {
    Make { root: PathBuf, n: u32, size: u64 },
    Memory { entries: u32 },
}

const USAGE: &str = "usage: ui-probe make --root <path> --n <n> [--size <bytes>] | ui-probe memory --entries <n>";

fn number<T: std::str::FromStr>(v: Option<&String>) -> Result<T, String> {
    v.and_then(|v| v.parse().ok()).ok_or_else(|| USAGE.to_string())
}

fn parse_args(args: &[String]) -> Result<Args, String> {
    let (cmd, rest) = args.split_first().ok_or_else(|| USAGE.to_string())?;
    let mut root = None;
    let (mut n, mut size, mut entries) = (None, 0u64, None);
    let mut it = rest.iter();
    while let Some(flag) = it.next() {
        match (cmd.as_str(), flag.as_str()) {
            ("make", "--root") => root = it.next().map(PathBuf::from),
            ("make", "--n") => n = Some(number::<u32>(it.next())?),
            ("make", "--size") => size = number(it.next())?,
            ("memory", "--entries") => entries = Some(number::<u32>(it.next())?),
            _ => return Err(USAGE.to_string()),
        }
    }
    match (cmd.as_str(), root, n, entries) {
        ("make", Some(root), Some(n), None) if n > 0 => Ok(Args::Make { root, n, size }),
        ("memory", None, None, Some(entries)) if entries > 0 => Ok(Args::Memory { entries }),
        _ => Err(USAGE.to_string()),
    }
}

fn main() {
    let argv: Vec<String> = std::env::args().skip(1).collect();
    let args = match parse_args(&argv) {
        Ok(a) => a,
        Err(e) => {
            eprintln!("ui-probe: {e}");
            std::process::exit(2);
        }
    };
    match args {
        Args::Make { root, n, size } => {
            let root = match live::LiveRoot::at(&root) {
                Ok(r) => r,
                Err(e) => {
                    eprintln!("ui-probe: {e}");
                    std::process::exit(2);
                }
            };
            let made = root.make_n("", n, size);
            println!("probe: made {} files of {size} bytes", made.len());
        }
        Args::Memory { entries } => {
            let before = private_bytes();
            let mut store = EntryStore::new(WidePath::parse(r"C:\made-up\probe").expect("a path"), Gen(0));
            store.append(fakes::listing_n(entries, 7));
            store.merge_pending();
            let now = LocalTime { year: 2026, month: 11, day: 21, hour: 14, minute: 40 };
            let model = FilesTable::new(
                Box::new(store),
                Rc::new(Fmt::fixed_en()),
                Rc::new(TypeNames::empty()),
                ViewOpts::default(),
                now,
            );
            let table = VirtualTable::new(
                TABLE_ID,
                model,
                default_columns(UiLang::EN),
                SelectionMode::Multi,
                TableStyle::FILES,
            );
            let after = private_bytes();
            println!(
                "probe: entries {} PrivateUsage {after} bytes ({} bytes over the start)",
                ListModel::len(table.model()),
                after.saturating_sub(before)
            );
        }
    }
}
