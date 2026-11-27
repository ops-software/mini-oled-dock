//! The natural-order gate (NAT4): the 50,000-name corpus sorted by natural_cmp must equal the order of the system
//! collator (StrCmpLogicalW, then ordinal) on this machine, and the order of this build and locale is kept as a
//! fixture so that a later Windows update shows up as drift. Plain tests: Windows CI and every coder machine.

mod common;

use core::cmp::Ordering;
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::OnceLock;

use common::corpus::corpus;
use common::temp::artifact;
use files_engine::listing::collate::wtf8_to_utf16;
use files_engine::listing::{natural_cmp, self_check, Collator, LogicalCollator, SortKeyCollator};
use windows::core::PCWSTR;
use windows::Win32::Globalization::GetUserDefaultLocaleName;
use windows::Win32::System::LibraryLoader::{GetModuleHandleW, GetProcAddress};
use windows::Win32::System::SystemInformation::OSVERSIONINFOW;

const CORPUS_VERSION: u16 = 1;

/// A stable merge sort of `v` by `cmp`, the same code for the system order and for natural_cmp, so that the two
/// orders can differ only where the two comparators do (a comparator that is not a total order cannot make the sorts
/// diverge by itself).
fn merge_sort(v: &mut Vec<u32>, cmp: &dyn Fn(u32, u32) -> Ordering) {
    if v.len() < 2 {
        return;
    }
    let mut right = v.split_off(v.len() / 2);
    merge_sort(v, cmp);
    merge_sort(&mut right, cmp);
    let left = std::mem::take(v);
    let (mut l, mut r) = (left.into_iter().peekable(), right.into_iter().peekable());
    while let (Some(x), Some(y)) = (l.peek(), r.peek()) {
        let next = if cmp(*y, *x) == Ordering::Less { r.next() } else { l.next() };
        v.extend(next);
    }
    v.extend(l);
    v.extend(r);
}

fn wide(name: &[u8]) -> Vec<u16> {
    let mut w = Vec::with_capacity(name.len());
    wtf8_to_utf16(name, &mut w);
    w
}

/// The system order of two names: StrCmpLogicalW on the UTF-16 forms, then ordinal on the bytes.
fn system_cmp(a: &[u8], b: &[u8]) -> Ordering {
    LogicalCollator.cmp_wide(&wide(a), &wide(b)).then_with(|| a.cmp(b))
}

fn system_equal(a: &[u8], b: &[u8]) -> bool {
    LogicalCollator.cmp_wide(&wide(a), &wide(b)) == Ordering::Equal
}

struct Setup {
    names: Vec<Vec<u8>>,
    /// The system order S: corpus indices, sorted.
    system: Vec<u32>,
}

fn setup() -> &'static Setup {
    static S: OnceLock<Setup> = OnceLock::new();
    S.get_or_init(|| {
        let names = corpus(CORPUS_VERSION);
        let wides: Vec<Vec<u16>> = names.iter().map(|n| wide(n)).collect();
        let mut system: Vec<u32> = (0..names.len() as u32).collect();
        merge_sort(&mut system, &|a, b| {
            LogicalCollator
                .cmp_wide(&wides[a as usize], &wides[b as usize])
                .then_with(|| names[a as usize].cmp(&names[b as usize]))
        });
        Setup { names, system }
    })
}

/// "<build>-<locale>": the Windows build number (RtlGetVersion) and the user's locale name.
fn machine_key() -> String {
    type RtlGetVersion = unsafe extern "system" fn(*mut OSVERSIONINFOW) -> i32;
    let ntdll: Vec<u16> = "ntdll.dll".encode_utf16().chain(Some(0)).collect();
    let mut info =
        OSVERSIONINFOW { dwOSVersionInfoSize: std::mem::size_of::<OSVERSIONINFOW>() as u32, ..Default::default() };
    // SAFETY: ntdll is loaded in every process; RtlGetVersion has this signature and fills `info`, whose size field is set.
    let build = unsafe {
        let module = GetModuleHandleW(PCWSTR(ntdll.as_ptr())).expect("ntdll is loaded");
        let proc = GetProcAddress(module, windows::core::s!("RtlGetVersion")).expect("ntdll exports RtlGetVersion");
        let f: RtlGetVersion = std::mem::transmute(proc);
        assert_eq!(f(&mut info), 0, "RtlGetVersion succeeds");
        info.dwBuildNumber
    };
    let mut buf = [0u16; 85];
    // SAFETY: the buffer is 85 units, the longest locale name plus its NUL.
    let n = unsafe { GetUserDefaultLocaleName(&mut buf) };
    assert!(n > 1, "the user locale has a name");
    let locale = String::from_utf16_lossy(&buf[..(n as usize - 1)]);
    let locale: String = locale.chars().filter(|c| c.is_ascii_alphanumeric() || *c == '-').collect();
    format!("{build}-{locale}")
}

fn fixtures_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests").join("fixtures").join("natural")
}

/// "FNAT", corpus version u16, count u32, 6 reserved bytes, then `count` u16 indices, little endian.
fn encode_fixture(order: &[u32]) -> Vec<u8> {
    let mut out = Vec::with_capacity(16 + order.len() * 2);
    out.extend_from_slice(b"FNAT");
    out.extend_from_slice(&CORPUS_VERSION.to_le_bytes());
    out.extend_from_slice(&(order.len() as u32).to_le_bytes());
    out.extend_from_slice(&[0u8; 6]);
    for i in order {
        out.extend_from_slice(&(*i as u16).to_le_bytes());
    }
    out
}

fn decode_fixture(bytes: &[u8]) -> Result<Vec<u32>, String> {
    if bytes.len() < 16 || &bytes[..4] != b"FNAT" {
        return Err("not a FNAT fixture".into());
    }
    let version = u16::from_le_bytes([bytes[4], bytes[5]]);
    let count = u32::from_le_bytes([bytes[6], bytes[7], bytes[8], bytes[9]]) as usize;
    if version != CORPUS_VERSION || bytes.len() != 16 + count * 2 {
        return Err(format!("fixture of corpus version {version}, {count} names, {} bytes", bytes.len()));
    }
    Ok(bytes[16..].chunks_exact(2).map(|c| u32::from(u16::from_le_bytes([c[0], c[1]]))).collect())
}

fn show(name: &[u8]) -> String {
    let mut w = Vec::new();
    wtf8_to_utf16(name, &mut w);
    format!("{:?}", String::from_utf16_lossy(&w))
}

#[test]
fn fixture_codec_round_trips() {
    let order: Vec<u32> = vec![3, 0, 2, 1, 49_999];
    let bytes = encode_fixture(&order);
    assert_eq!(bytes.len(), 16 + 10);
    assert_eq!(decode_fixture(&bytes), Ok(order));
    assert!(decode_fixture(&bytes[..20]).is_err());
    assert!(decode_fixture(b"XXXX0000000000000000").is_err());
}

#[test]
fn natural_order_equals_the_system_order_on_the_corpus() {
    assert!(self_check(), "the fast path passes its own probe on this machine (else the gate would test nothing)");
    let s = setup();
    // Step 2a: the whole corpus sorted by natural_cmp is exactly S.
    let mut ours: Vec<u32> = (0..s.names.len() as u32).collect();
    merge_sort(&mut ours, &|a, b| natural_cmp(&s.names[a as usize], &s.names[b as usize]));
    if ours != s.system {
        let first = ours.iter().zip(&s.system).position(|(x, y)| x != y).unwrap_or(0);
        panic!(
            "natural_cmp order differs from the system order from position {first}: ours {} system {}",
            show(&s.names[ours[first] as usize]),
            show(&s.names[s.system[first] as usize])
        );
    }
    // Step 2b: every adjacent pair of S agrees with the system wherever the system does not say Equal.
    let mut bad = Vec::new();
    for w in s.system.windows(2) {
        let (a, b) = (&s.names[w[0] as usize], &s.names[w[1] as usize]);
        if !system_equal(a, b) && natural_cmp(a, b) != system_cmp(a, b) {
            bad.push((show(a), show(b)));
        }
    }
    assert!(bad.is_empty(), "{} adjacent pairs differ, first: {:?}", bad.len(), bad.first());
    // Step 2c: 200,000 seeded random pairs.
    let mut seed: u64 = 0x5DEE_CE66_D1CE_4E5B;
    let mut next = move || {
        seed ^= seed >> 12;
        seed ^= seed << 25;
        seed ^= seed >> 27;
        (seed.wrapping_mul(0x2545_F491_4F6C_DD1D) >> 11) as usize
    };
    let mut bad = Vec::new();
    for _ in 0..200_000 {
        let (a, b) = (&s.names[next() % s.names.len()], &s.names[next() % s.names.len()]);
        if !system_equal(a, b) && natural_cmp(a, b) != system_cmp(a, b) {
            bad.push((show(a), show(b)));
        }
    }
    assert!(bad.is_empty(), "{} random pairs differ, first: {:?}", bad.len(), bad.first());
}

#[test]
fn fixture_of_this_machine_and_tolerance_report() {
    let s = setup();
    let key = machine_key();
    let mine = fixtures_dir().join(format!("{key}.bin"));
    let ranks = |order: &[u32]| -> HashMap<u32, usize> { order.iter().enumerate().map(|(r, i)| (*i, r)).collect() };
    match std::fs::read(&mine) {
        Ok(bytes) => {
            let committed = decode_fixture(&bytes).unwrap_or_else(|e| panic!("{}: {e}", mine.display()));
            if committed != s.system {
                // An in-build Windows update: reported, not failed (the hard gate is the other test).
                let rank = ranks(&committed);
                let mut diff: Vec<String> = s
                    .system
                    .windows(2)
                    .filter(|w| rank.get(&w[0]) > rank.get(&w[1]))
                    .map(|w| format!("{} before {}", show(&s.names[w[0] as usize]), show(&s.names[w[1] as usize])))
                    .collect();
                let text = format!("drift {key}: {} adjacent pairs now order differently\n{}\n", diff.len(), {
                    diff.truncate(50);
                    diff.join("\n")
                });
                let p = artifact(&format!("natural/drift-{key}.txt"), text.as_bytes());
                eprintln!("natural order drift on {key}: see {}", p.display());
            }
        }
        Err(_) => {
            let p = artifact(&format!("natural/{key}.bin"), &encode_fixture(&s.system));
            eprintln!(
                "notice: no fixture for {key}; wrote {} (commit it to apps/files/tests/fixtures/natural)",
                p.display()
            );
        }
    }
    // Step 4: this machine's order against every other committed fixture (never a failure).
    let rank = ranks(&s.system);
    let mut report = format!("natural order of {key} against the committed fixtures\n");
    let mut others = 0;
    let mut files: Vec<PathBuf> = std::fs::read_dir(fixtures_dir())
        .map(|rd| rd.flatten().map(|e| e.path()).filter(|p| p.extension().is_some_and(|x| x == "bin")).collect())
        .unwrap_or_default();
    files.sort();
    for f in files {
        let stem = f.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
        if stem == key {
            continue;
        }
        others += 1;
        match std::fs::read(&f).map_err(|e| e.to_string()).and_then(|b| decode_fixture(&b)) {
            Ok(order) => {
                let diff: Vec<String> = order
                    .windows(2)
                    .filter(|w| rank.get(&w[0]) > rank.get(&w[1]))
                    .map(|w| format!("{} before {}", show(&s.names[w[0] as usize]), show(&s.names[w[1] as usize])))
                    .collect();
                report.push_str(&format!("{stem}: {} pairs order differently\n", diff.len()));
                for d in diff.iter().take(20) {
                    report.push_str(&format!("  {d}\n"));
                }
            }
            Err(e) => report.push_str(&format!("{stem}: unreadable ({e})\n")),
        }
    }
    report.push_str(&format!("{others} other fixtures; builds 26100 and 26200 have none unless listed above\n"));
    let p = artifact("natural/report.txt", report.as_bytes());
    eprintln!("tolerance report: {}", p.display());
}

/// NAT4 step 5: the sort-key collator, which sort_rows uses for the non-plain names of a large folder, orders the
/// corpus exactly as the system order does (keys, then ordinal). If it ever stops, sort_rows must not use it.
#[test]
fn sort_keys_give_the_system_order_on_the_corpus() {
    let s = setup();
    let keys: Vec<Vec<u8>> = s.names.iter().map(|n| SortKeyCollator.key(&wide(n))).collect();
    let mut by_key: Vec<u32> = (0..s.names.len() as u32).collect();
    merge_sort(&mut by_key, &|a, b| {
        keys[a as usize].cmp(&keys[b as usize]).then_with(|| s.names[a as usize].cmp(&s.names[b as usize]))
    });
    if by_key != s.system {
        let first = by_key.iter().zip(&s.system).position(|(x, y)| x != y).unwrap_or(0);
        panic!(
            "the sort-key order differs from the system order from position {first}: keys {} system {}",
            show(&s.names[by_key[first] as usize]),
            show(&s.names[s.system[first] as usize])
        );
    }
}
