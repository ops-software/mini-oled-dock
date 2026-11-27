//! fs6-probe: spike FS-6 of the files engine (plans P1-files-engine, SP2). Asking the shell for class icons
//! (SHGetFileInfoW with SHGFI_USEFILEATTRIBUTES, for the system image list index and for the icon location) can load
//! icon handlers into the caller. If every module it loads is Microsoft's, class icons can be read in the UI process
//! (CLASS_ICONS_IN_UI = true); otherwise they come from the shell host. "Microsoft's" is decided by signature only
//! (trust.rs). List L1 is 300 common extensions written in the probe; list L2 is every extension registered on the
//! machine (the subkeys of HKEY_CLASSES_ROOT that start with a dot, read with KEY_READ; nothing is ever written),
//! minus L1. The record keeps counts and timings only; L2's names and the non-Microsoft modules' paths go to
//! --scratch, when given, and nowhere else. No file is touched (SHGFI_USEFILEATTRIBUTES), no window is created.

use std::path::{Path, PathBuf};
use std::time::Instant;

use fs3_spike::exts::L1;
use fs3_spike::guard;
use fs3_spike::json::J;
use fs3_spike::trust::{self, Kind};
use fs3_spike::util::{hr_hex, system_facts, wide_str, Sta};

use windows::core::{w, HRESULT};
use windows::Win32::Foundation::ERROR_SUCCESS;
use windows::Win32::Storage::FileSystem::FILE_ATTRIBUTE_NORMAL;
use windows::Win32::System::Registry::{RegCloseKey, RegEnumKeyExW, RegOpenKeyExW, HKEY, HKEY_CLASSES_ROOT, KEY_READ};
use windows::Win32::UI::Shell::{
    SHGetFileInfoW, SHFILEINFOW, SHGFI_ICONLOCATION, SHGFI_SYSICONINDEX, SHGFI_USEFILEATTRIBUTES,
};

const USAGE: &str = "usage: fs6-probe --json <file> [--scratch <dir>]
  --json: the record (counts and timings only); its folder lies under %TEMP% in a mod-files-* folder
  --scratch: where L2's extension names and the non-Microsoft modules' paths are written (same rule); without it
    they are written nowhere";

#[derive(Debug, PartialEq, Eq)]
struct Args {
    json: PathBuf,
    scratch: Option<PathBuf>,
}

fn parse(args: &[String]) -> Result<Args, String> {
    let (mut json, mut scratch) = (None, None);
    let mut it = args.iter();
    while let Some(a) = it.next() {
        match a.as_str() {
            "--json" => json = Some(PathBuf::from(it.next().ok_or("--json needs a file")?)),
            "--scratch" => scratch = Some(PathBuf::from(it.next().ok_or("--scratch needs a folder")?)),
            other => return Err(format!("unknown option {other}")),
        }
    }
    Ok(Args { json: json.ok_or("--json is required")?, scratch })
}

/// Every extension registered on the machine (lower case, without the dot): the HKCR subkeys that start with ".".
fn registered_extensions() -> Result<Vec<String>, String> {
    let mut hk = HKEY::default();
    // SAFETY: opens HKCR for reading; closed below.
    let r = unsafe { RegOpenKeyExW(HKEY_CLASSES_ROOT, w!(""), None, KEY_READ, &mut hk) };
    if r != ERROR_SUCCESS {
        return Err(format!("RegOpenKeyExW(HKCR): {}", r.0));
    }
    let mut out = Vec::new();
    let mut i = 0u32;
    loop {
        let mut name = [0u16; 256];
        let mut len = name.len() as u32;
        // SAFETY: name is writable for len units.
        let r = unsafe {
            RegEnumKeyExW(hk, i, Some(windows::core::PWSTR(name.as_mut_ptr())), &mut len, None, None, None, None)
        };
        if r != ERROR_SUCCESS {
            break; // ERROR_NO_MORE_ITEMS (or an error: the list read so far is used)
        }
        let s = String::from_utf16_lossy(&name[..len as usize]);
        if let Some(ext) = s.strip_prefix('.') {
            if !ext.is_empty() {
                out.push(ext.to_lowercase());
            }
        }
        i += 1;
    }
    // SAFETY: the key opened above.
    let _ = unsafe { RegCloseKey(hk) };
    out.sort();
    out.dedup();
    Ok(out)
}

/// L2: the registered extensions that are not in L1.
fn l2_from(registered: &[String]) -> Vec<String> {
    registered.iter().filter(|e| !L1.iter().any(|l| l.eq_ignore_ascii_case(e))).cloned().collect()
}

fn percentile(sorted: &[u64], p: usize) -> u64 {
    if sorted.is_empty() {
        return 0;
    }
    sorted[(sorted.len() - 1) * p / 100]
}

fn timing(mut v: Vec<u64>) -> J {
    v.sort_unstable();
    J::obj().set("calls", v.len()).set("p50_us", percentile(&v, 50)).set("max_us", v.last().copied().unwrap_or(0))
}

/// One SHGetFileInfoW call on ".ext" by attributes only, timed in microseconds.
fn file_info(ext: &str, flags: windows::Win32::UI::Shell::SHGFI_FLAGS) -> (u64, bool) {
    let w = wide_str(&format!(".{ext}"));
    let mut sfi = SHFILEINFOW::default();
    let t = Instant::now();
    // SAFETY: w is NUL-terminated; sfi is sized; SHGFI_USEFILEATTRIBUTES touches no file.
    let r = unsafe {
        SHGetFileInfoW(
            windows::core::PCWSTR(w.as_ptr()),
            FILE_ATTRIBUTE_NORMAL,
            Some(&mut sfi),
            std::mem::size_of::<SHFILEINFOW>() as u32,
            SHGFI_USEFILEATTRIBUTES | flags,
        )
    };
    (u64::try_from(t.elapsed().as_micros()).unwrap_or(u64::MAX), r != 0)
}

struct ListResult {
    json: J,
    not_microsoft: Vec<String>,
    /// At least one call answered: a list whose every call failed loaded nothing and measured nothing.
    measured: bool,
}

/// A list measured something unless every one of its two calls per extension failed (an empty list measures nothing).
fn measured(extensions: usize, calls_failed: usize) -> bool {
    calls_failed < 2 * extensions
}

/// CLASS_ICONS_IN_UI over both lists, each (name, measured): false as soon as one new module is not Microsoft's;
/// true only when both lists were measured and none was found; otherwise not measured (None), with the lists that
/// were not.
fn class_icons_in_ui<'a>(not_microsoft: usize, lists: &[(&'a str, bool)]) -> (Option<bool>, Vec<&'a str>) {
    let gaps: Vec<&'a str> = lists.iter().filter(|(_, m)| !m).map(|(n, _)| *n).collect();
    if not_microsoft > 0 {
        (Some(false), gaps)
    } else if gaps.is_empty() {
        (Some(true), gaps)
    } else {
        (None, gaps)
    }
}

fn run_list(name: &str, exts: &[String]) -> Result<ListResult, String> {
    let before = trust::modules().ok_or(format!("{name}: the module snapshot before the calls failed"))?;
    let (mut idx, mut loc) = (Vec::new(), Vec::new());
    let mut failed = 0usize;
    for e in exts {
        let (t1, ok1) = file_info(e, SHGFI_SYSICONINDEX);
        let (t2, ok2) = file_info(e, SHGFI_ICONLOCATION);
        idx.push(t1);
        loc.push(t2);
        failed += usize::from(!ok1) + usize::from(!ok2);
    }
    let after = trust::modules().ok_or(format!("{name}: the module snapshot after the calls failed"))?;
    let new: Vec<&String> = after.iter().filter(|m| !before.contains(m)).collect();
    let (mut ms, mut mismatch, mut unsigned, mut other) = (0usize, 0usize, 0usize, 0usize);
    let mut kinds = (0usize, 0usize);
    let mut not_microsoft = Vec::new();
    for m in &new {
        let v = trust::classify(Path::new(m.as_str()));
        match v.kind {
            Kind::Embedded => kinds.0 += 1,
            Kind::Catalog => kinds.1 += 1,
            Kind::Unsigned => {}
        }
        if v.microsoft() {
            ms += 1;
            continue;
        }
        if v.mismatch() {
            mismatch += 1;
        } else if v.kind == Kind::Unsigned {
            unsigned += 1;
        } else {
            other += 1;
        }
        not_microsoft.push(format!(
            "{m}\t{:?}\t{}\t{}\t{}",
            v.kind,
            hr_hex(HRESULT(v.trust)),
            v.microsoft_root,
            v.company.unwrap_or_default()
        ));
    }
    let json = J::obj()
        .set("extensions", exts.len())
        .set("calls_failed", failed)
        .set("modules_before", before.len())
        .set("modules_after", after.len())
        .set("measured", measured(exts.len(), failed))
        .set("new_modules", new.len())
        .set("signed_embedded", kinds.0)
        .set("signed_catalog", kinds.1)
        .set("microsoft", ms)
        .set("not_microsoft", not_microsoft.len())
        .set("not_microsoft_unsigned", unsigned)
        .set("not_microsoft_company_mismatch", mismatch)
        .set("not_microsoft_other_signer_or_invalid", other)
        .set("sysiconindex", timing(idx))
        .set("iconlocation", timing(loc));
    Ok(ListResult { json, not_microsoft, measured: measured(exts.len(), failed) })
}

fn write_scratch(dir: &Option<PathBuf>, name: &str, lines: &[String]) -> J {
    let Some(d) = dir else {
        return J::from("not written (no --scratch)");
    };
    let p = d.join(name);
    match std::fs::write(&p, lines.join("\n") + "\n") {
        Ok(()) => J::from(format!("{} lines in --scratch", lines.len())),
        Err(e) => J::from(format!("cannot write: {e}")),
    }
}

fn run(a: &Args) -> Result<(PathBuf, J), String> {
    let json = guard::prepare_record(&a.json).map_err(|e| format!("refused --json {}: {e}", a.json.display()))?;
    let scratch = match &a.scratch {
        Some(s) => Some(guard::prepare_root(s).map_err(|e| format!("refused --scratch {}: {e}", s.display()))?),
        None => None,
    };
    let _sta = Sta::init()?;
    let registered = registered_extensions()?;
    let l1: Vec<String> = L1.iter().map(|s| s.to_string()).collect();
    let l2 = l2_from(&registered);
    let r1 = run_list("L1", &l1)?;
    let r2 = run_list("L2", &l2)?;
    let bad = r1.not_microsoft.len() + r2.not_microsoft.len();
    let (value, gaps) = class_icons_in_ui(bad, &[("L1", r1.measured), ("L2", r2.measured)]);
    let mut verdict = J::obj().set("constant", "CLASS_ICONS_IN_UI").set("value", value).set(
        "rule",
        "false when a new module lacks a valid signature chaining to a Microsoft root; true only when both lists were measured and none did; otherwise null (not measured)",
    );
    if !gaps.is_empty() {
        verdict = verdict.set("not_measured", gaps.iter().map(|g| J::from(*g)).collect::<Vec<_>>());
    }
    let mut modules: Vec<String> = r1.not_microsoft.iter().map(|m| format!("L1\t{m}")).collect();
    modules.extend(r2.not_microsoft.iter().map(|m| format!("L2\t{m}")));
    let rec = J::obj()
        .set("probe", "fs6-probe")
        .set("system", system_facts())
        .set("registered_extensions", registered.len())
        .set("L1", r1.json)
        .set("L2", r2.json)
        .set("scratch_l2_names", write_scratch(&scratch, "fs6-l2-extensions.txt", &l2))
        .set("scratch_modules", write_scratch(&scratch, "fs6-not-microsoft-modules.txt", &modules))
        .set("verdict", verdict);
    Ok((json, rec))
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let a = match parse(&args) {
        Ok(a) => a,
        Err(e) => {
            eprintln!("fs6-probe: {e}\n{USAGE}");
            std::process::exit(2);
        }
    };
    match run(&a) {
        Ok((json, rec)) => {
            if let Err(e) = std::fs::write(&json, rec.render()) {
                eprintln!("fs6-probe: cannot write {}: {e}", json.display());
                std::process::exit(1);
            }
            let v = rec.get("verdict").and_then(|v| v.get("value")).and_then(J::as_bool);
            let v = v.map_or_else(|| "not measured".to_string(), |b| b.to_string());
            println!("fs6-probe: CLASS_ICONS_IN_UI = {v}; record {}", json.display());
        }
        Err(e) => {
            eprintln!("fs6-probe: {e}");
            std::process::exit(2);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn arguments() {
        let s = |v: &[&str]| v.iter().map(|x| x.to_string()).collect::<Vec<_>>();
        assert_eq!(parse(&s(&["--json", "j"])).unwrap(), Args { json: "j".into(), scratch: None });
        assert_eq!(parse(&s(&["--json", "j", "--scratch", "d"])).unwrap().scratch, Some("d".into()));
        assert!(parse(&s(&[])).is_err());
        assert!(parse(&s(&["--json"])).is_err());
        assert!(parse(&s(&["--json", "j", "--root", "x"])).is_err());
    }

    #[test]
    fn l2_drops_what_l1_holds_and_percentiles() {
        let reg = vec!["txt".to_string(), "pdf".to_string(), "zzz-test".to_string()];
        assert_eq!(l2_from(&reg), vec!["zzz-test".to_string()]);
        assert_eq!(percentile(&[1, 2, 3, 4, 5], 50), 3);
        assert_eq!(percentile(&[7], 50), 7);
        assert_eq!(percentile(&[], 50), 0);
    }

    #[test]
    fn a_list_whose_calls_all_failed_is_not_a_pass() {
        assert!(measured(300, 0) && measured(300, 599));
        assert!(!measured(300, 600), "every call failed");
        assert!(!measured(0, 0), "an empty list measures nothing");
        assert_eq!(class_icons_in_ui(0, &[("L1", true), ("L2", true)]), (Some(true), vec![]));
        assert_eq!(class_icons_in_ui(2, &[("L1", true), ("L2", true)]), (Some(false), vec![]));
        assert_eq!(class_icons_in_ui(0, &[("L1", true), ("L2", false)]), (None, vec!["L2"]));
        assert_eq!(class_icons_in_ui(1, &[("L1", false), ("L2", true)]), (Some(false), vec!["L1"]), "found is found");
    }
}
