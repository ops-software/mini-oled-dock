//! enum-probe: lists a folder headless, N times, and reports per run the time to enumerate and merge, the time to
//! sort by name and the store's heap bytes. Reads only, except that --make creates empty files through LiveRoot.
//!
//!   enum-probe --root <dir> [--make <n>] [--runs <k>] [--json <name>]
//!
//! <dir> must be a mod-files-live-* folder (or inside one) under %TEMP%, as LiveRoot::at demands. --json writes the
//! report as <name> inside that folder, through LiveRoot; the report is always printed as well. A probe never
//! touches any other folder.

#[path = "common/live.rs"]
mod live;

use std::path::PathBuf;
use std::time::Instant;

use files_engine::listing::{
    CancelToken, EntryStore, Enumerator, FindEnumerator, Gen, GroupBy, SortDir, SortKey, WidePath,
};
use live::LiveRoot;

struct Args {
    root: PathBuf,
    make: u32,
    runs: u32,
    json: Option<String>,
}

fn parse() -> Result<Args, String> {
    let mut args = Args { root: PathBuf::new(), make: 0, runs: 5, json: None };
    let mut it = std::env::args().skip(1);
    while let Some(flag) = it.next() {
        let mut value = || it.next().ok_or_else(|| format!("{flag} needs a value"));
        match flag.as_str() {
            "--root" => args.root = PathBuf::from(value()?),
            "--make" => args.make = value()?.parse().map_err(|_| "--make takes a number".to_string())?,
            "--runs" => args.runs = value()?.parse().map_err(|_| "--runs takes a number".to_string())?,
            "--json" => args.json = Some(value()?),
            other => return Err(format!("unknown argument {other}")),
        }
    }
    if args.root.as_os_str().is_empty() {
        return Err("--root <dir> is required".into());
    }
    if args.runs == 0 {
        return Err("--runs must be at least 1".into());
    }
    Ok(args)
}

fn main() {
    let args = match parse() {
        Ok(a) => a,
        Err(e) => {
            eprintln!("enum-probe: {e}\nusage: enum-probe --root <dir> [--make <n>] [--runs <k>] [--json <name>]");
            std::process::exit(2);
        }
    };
    let root = match LiveRoot::at(&args.root) {
        Ok(r) => r,
        Err(e) => {
            eprintln!("enum-probe: {e}");
            std::process::exit(2);
        }
    };
    if args.make > 0 {
        root.make_n("", args.make, 0);
    }
    let units: Vec<u16> = {
        use std::os::windows::ffi::OsStrExt;
        root.path().as_os_str().encode_wide().collect()
    };
    let dir = match WidePath::new(&units) {
        Ok(d) => d,
        Err(e) => {
            eprintln!("enum-probe: bad path: {e:?}");
            std::process::exit(2);
        }
    };

    let mut runs = Vec::new();
    for _ in 0..args.runs {
        let t = Instant::now();
        let mut store = EntryStore::new(dir.clone(), Gen(1));
        FindEnumerator::default().run(&dir, Gen(1), &CancelToken::new(), &mut |b| {
            store.append(b);
            store.merge_pending();
        });
        let list_ms = t.elapsed().as_secs_f64() * 1000.0;
        let t = Instant::now();
        store.sort(SortKey::Name, SortDir::Asc, GroupBy::None);
        let sort_ms = t.elapsed().as_secs_f64() * 1000.0;
        runs.push(serde_json::json!({
            "entries": store.len(),
            "list_ms": (list_ms * 100.0).round() / 100.0,
            "sort_ms": (sort_ms * 100.0).round() / 100.0,
            "heap_bytes": store.heap_bytes(),
            "error": store.error().map(|e| format!("{e:?}")),
        }));
    }
    let report = serde_json::json!({ "runs": runs });
    let text = serde_json::to_string_pretty(&report).unwrap_or_default();
    println!("{text}");
    if let Some(name) = args.json {
        root.file(&name, text.as_bytes());
    }
}
