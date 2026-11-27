//! icons.bin (IC1): the format, its writer and its checked, read-only mapping.
mod common;

use common::temp::TempRoot;
use files_engine::decor::iconsbin::{
    build, check, write_next, IconsBin, IconsBinError as E, Item, CAP_BYTES, ENTRY_LEN, HEADER_LEN,
};
use files_engine::own_files::OwnDir;

fn pixels(px: u16, seed: u8) -> Vec<u8> {
    (0..usize::from(px) * usize::from(px) * 4).map(|i| (i as u8).wrapping_mul(7).wrapping_add(seed)).collect()
}

fn item(ext: &str, px: u16, hits: u32) -> Item {
    Item { ext: ext.into(), px, bgra: pixels(px, ext.len() as u8 + px as u8), hits }
}

fn sample() -> Vec<u8> {
    let mut v = Vec::new();
    for (i, e) in ["", "doc", "txt", "zip"].iter().enumerate() {
        v.push(item(e, 16, 10 + i as u32));
        v.push(item(e, 24, 10 + i as u32));
    }
    build(v, 123_456_789)
}

fn put16(b: &mut [u8], o: usize, v: u16) {
    b[o..o + 2].copy_from_slice(&v.to_le_bytes());
}
fn put32(b: &mut [u8], o: usize, v: u32) {
    b[o..o + 4].copy_from_slice(&v.to_le_bytes());
}
/// Recomputes the index checksum, so a broken index field is seen by its own check and not by the checksum.
fn resum(b: &mut [u8]) {
    let count = u32::from_le_bytes(b[24..28].try_into().unwrap()) as usize;
    let sum = files_engine::decor::iconsbin::fnv1a(&b[HEADER_LEN..HEADER_LEN + count * ENTRY_LEN]);
    b[40..48].copy_from_slice(&sum.to_le_bytes());
}
fn entry(i: usize) -> usize {
    HEADER_LEN + i * ENTRY_LEN
}
fn bump32(b: &mut [u8], o: usize, by: u32) {
    let v = u32::from_le_bytes(b[o..o + 4].try_into().unwrap());
    put32(b, o, v + by);
}
fn swap_entries(b: &mut [u8], x: usize, y: usize) {
    for k in 0..ENTRY_LEN {
        b.swap(entry(x) + k, entry(y) + k);
    }
}
fn copy_entry(b: &mut [u8], from: usize, to: usize) {
    let e: Vec<u8> = b[entry(from)..entry(from) + ENTRY_LEN].to_vec();
    b[entry(to)..entry(to) + ENTRY_LEN].copy_from_slice(&e);
}
type Mutation = Box<dyn Fn(&mut Vec<u8>)>;
/// A mutation of the index followed by a new checksum.
fn sum_after(f: impl Fn(&mut Vec<u8>) + 'static) -> Mutation {
    Box::new(move |b| {
        f(b);
        resum(b);
    })
}

#[test]
fn round_trip() {
    let bytes = sample();
    let l = check(&bytes).expect("a built file checks");
    assert_eq!((l.count, l.sizes.clone(), l.built), (8, vec![16, 24], 123_456_789));
    assert_eq!(bytes.len() % 16, 0, "blobs stay 16-byte aligned");
    let root = TempRoot::new("iconsbin-rt");
    let dir = OwnDir::open(root.path()).expect("own dir");
    assert_eq!(write_next(&dir, &bytes).expect("written"), 1);
    let bin = IconsBin::open(root.path()).expect("maps");
    assert_eq!((bin.entries(), bin.built(), bin.counter()), (8, 123_456_789, 1));
    assert_eq!(bin.get(b"txt", 16), Some(&pixels(16, 3 + 16)[..]));
    assert_eq!(bin.get(b"TXT", 24), Some(&pixels(24, 3 + 24)[..]), "lookups are case blind");
    assert_eq!(bin.get(b"", 16).map(<[u8]>::len), Some(16 * 16 * 4), "the folder entry");
    assert_eq!(bin.get(b"doc", 32), None, "a size that is not there");
    assert_eq!(bin.get(b"png", 16), None, "an extension that is not there");
    assert_eq!(bin.get(b"a.b", 16), None);
    assert_eq!(bin.get(&[0xff], 16), None, "not UTF-8");
    assert_eq!(bin.list().len(), 8);
    assert_eq!(bin.sizes(), &[16, 24]);
}

#[test]
fn highest_valid_wins_and_others_go() {
    let root = TempRoot::new("iconsbin-n");
    let one = build(vec![item("a", 16, 1)], 1);
    let two = build(vec![item("b", 16, 1)], 2);
    root.file("icons-1.bin", &one);
    root.file("icons-2.bin", &two);
    root.file("icons-3.bin", b"not an icons file at all, and long enough to pass a size test........................");
    root.file("icons-04.bin", &one);
    root.file("notes.txt", b"x");
    let bin = IconsBin::open(root.path()).expect("the highest valid");
    assert_eq!((bin.counter(), bin.built()), (2, 2));
    let left = OwnDir::open(root.path()).expect("own dir").list("").expect("list");
    let left: Vec<String> = left.iter().map(|n| n.to_string_lossy().into_owned()).collect();
    assert!(left.contains(&"icons-2.bin".to_string()) && left.contains(&"notes.txt".to_string()), "{left:?}");
    assert!(!left.contains(&"icons-1.bin".to_string()) && !left.contains(&"icons-3.bin".to_string()), "{left:?}");
    assert!(left.contains(&"icons-04.bin".to_string()), "a name that is not canonical is not ours");
    assert_eq!(write_next(&OwnDir::open(root.path()).expect("own dir"), &one).expect("next"), 3);
    assert!(IconsBin::open(root.path()).is_some_and(|b| b.counter() == 3), "the new file is the one at the next start");
}

#[test]
fn open_refuses_nothing_valid() {
    let root = TempRoot::new("iconsbin-none");
    assert!(IconsBin::open(root.path()).is_none(), "no file");
    root.file("icons-1.bin", &[0u8; 100]);
    assert!(IconsBin::open(root.path()).is_none(), "only a broken file");
}

#[test]
fn check_rejects() {
    let good = sample();
    assert!(check(&good).is_ok());
    // (name, mutation, expected error)
    type Case = (&'static str, Mutation, E);
    let cases: Vec<Case> = vec![
        ("empty", Box::new(|b| b.clear()), E::TooShort),
        ("short header", Box::new(|b| b.truncate(63)), E::TooShort),
        ("too large", Box::new(|b| b.resize(CAP_BYTES + 1, 0)), E::TooLarge),
        ("magic", Box::new(|b| b[0] = b'X'), E::Magic),
        ("version", Box::new(|b| put32(b, 8, 2)), E::Version),
        ("no sizes", Box::new(|b| put32(b, 12, 0)), E::Sizes),
        ("five sizes", Box::new(|b| put32(b, 12, 5)), E::Sizes),
        ("a zero size", Box::new(|b| put16(b, 16, 0)), E::Sizes),
        ("sizes not ascending", Box::new(|b| put16(b, 18, 8)), E::Sizes),
        ("an unused size", Box::new(|b| put16(b, 20, 32)), E::Sizes),
        ("a size over 256", Box::new(|b| put16(b, 18, 257)), E::Sizes),
        ("count over the cap", Box::new(|b| put32(b, 24, 1025)), E::Count),
        ("count past the file", Box::new(|b| put32(b, 24, 1000)), E::Count),
        ("reserved a", Box::new(|b| b[28] = 1), E::Reserved),
        ("reserved b", Box::new(|b| b[63] = 1), E::Reserved),
        ("checksum", Box::new(|b| b[40] ^= 1), E::Checksum),
        ("index byte without a new sum", Box::new(|b| b[entry(0) + 40] ^= 1), E::Checksum),
        ("upper case ext", sum_after(|b| put16(b, entry(2), u16::from(b'D'))), E::Ext),
        ("a unit after a NUL", sum_after(|b| put16(b, entry(2) + 14, u16::from(b'x'))), E::Ext),
        ("a dot in the ext", sum_after(|b| put16(b, entry(2), u16::from(b'.'))), E::Ext),
        ("px not in sizes", sum_after(|b| put16(b, entry(1) + 32, 20)), E::Px),
        ("flags", sum_after(|b| put16(b, entry(1) + 34, 1)), E::Flags),
        ("offset into the index", sum_after(|b| put32(b, entry(1) + 36, 64)), E::Range),
        ("offset not aligned", sum_after(|b| bump32(b, entry(1) + 36, 4)), E::Range),
        ("blob past the end", sum_after(|b| bump32(b, entry(7) + 36, 1 << 20)), E::Range),
        ("len not px*px*4", sum_after(|b| put32(b, entry(1) + 40, 100)), E::Len),
        ("order swapped", sum_after(|b| swap_entries(b, 2, 3)), E::Order),
        ("duplicate entry", sum_after(|b| copy_entry(b, 2, 3)), E::Order),
    ];
    assert!(cases.len() >= 20);
    for (name, f, want) in cases {
        let mut b = good.clone();
        f(&mut b);
        assert_eq!(check(&b), Err(want), "{name}");
    }
}

#[test]
fn pixels_are_not_checksummed() {
    let mut b = sample();
    let last = b.len() - 1;
    b[last] ^= 0xff;
    assert!(check(&b).is_ok(), "bad pixels draw wrong colours, nothing worse");
}

#[test]
fn fuzz_iconsbin() {
    let good = sample();
    let mut s: u64 = 0x9e37_79b9_7f4a_7c15;
    let mut next = move || {
        s ^= s << 13;
        s ^= s >> 7;
        s ^= s << 17;
        s
    };
    let mut accepted = 0u32;
    for i in 0..100_000u32 {
        let input: Vec<u8> = if i % 2 == 0 {
            let n = (next() % 600) as usize;
            (0..n).map(|_| next() as u8).collect()
        } else {
            let mut b = good.clone();
            for _ in 0..1 + next() % 4 {
                let at = (next() as usize) % b.len();
                match next() % 3 {
                    0 => b[at] = next() as u8,
                    1 => b[at] ^= 1 << (next() % 8),
                    _ => b.truncate(at.max(1)),
                }
                if b.is_empty() {
                    break;
                }
            }
            b
        };
        if let Ok(l) = check(&input) {
            accepted += 1;
            assert!(l.count * ENTRY_LEN + HEADER_LEN <= input.len());
        }
    }
    assert!(accepted > 0, "some mutations (pixel bytes) stay valid, so the checker is not rejecting everything");
}

#[test]
fn fits_2mib() {
    let mut v = Vec::new();
    for n in 0..256u32 {
        for px in [24u16, 32] {
            let bgra = vec![n as u8; usize::from(px) * usize::from(px) * 4];
            v.push(Item { ext: format!("e{n:03}"), px, bgra, hits: n });
        }
    }
    let b = build(v, 1);
    assert!(b.len() < CAP_BYTES, "{}", b.len());
    assert!(b.len() > 1_600_000, "256 extensions at 24 and 32 px are about 1.6 MiB: {}", b.len());
    assert_eq!(check(&b).expect("valid").count, 512);
}

#[test]
fn over_the_cap_the_lowest_hits_go() {
    let mut v = Vec::new();
    for n in 0..256u32 {
        for px in [64u16, 96] {
            let bgra = vec![1; usize::from(px) * usize::from(px) * 4];
            v.push(Item { ext: format!("e{n:03}"), px, bgra, hits: n });
        }
    }
    let b = build(v, 1);
    assert!(b.len() <= CAP_BYTES);
    let l = check(&b).expect("valid");
    assert!(l.count < 512 && l.count > 0 && l.count.is_multiple_of(2), "{}", l.count);
    let root = TempRoot::new("iconsbin-cap");
    root.file("icons-1.bin", &b);
    let bin = IconsBin::open(root.path()).expect("maps");
    assert!(bin.get(b"e255", 64).is_some(), "the most used stays");
    assert!(bin.get(b"e000", 64).is_none(), "the least used goes");
}

#[test]
fn writer_leaves_out_what_does_not_fit() {
    let long = Item { ext: "x".repeat(17), px: 16, bgra: pixels(16, 0), hits: 1 };
    let dotted = Item { ext: "a.b".into(), px: 16, bgra: pixels(16, 0), hits: 1 };
    let short = Item { ext: "ok".into(), px: 16, bgra: vec![0; 10], hits: 1 };
    let upper = Item { ext: "TXT".into(), px: 16, bgra: pixels(16, 0), hits: 1 };
    let dup = Item { ext: "txt".into(), px: 16, bgra: pixels(16, 9), hits: 5 };
    let l = check(&build(vec![long, dotted, short, upper, dup], 7)).expect("valid");
    assert_eq!(l.count, 1, "one TXT survives: too long, a dot, a wrong pixel count and a duplicate are dropped");
    assert!(check(&build(Vec::new(), 7)).is_err(), "an empty file has no size and is not written");
}
