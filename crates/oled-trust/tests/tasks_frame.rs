//! The Tasks launch contract (P3-tasks-app TF1, F-T1): the canonical command line and its parser, the Show frame's
//! payload pinned byte by byte, a 10,000-value round-trip property over both forms, a 20,000-input total-decode test
//! on every push and a 1,000,000-input fuzz nightly.

use oled_trust::frame::tasks::*;
use oled_trust::frame::{decode, encode, wire, Codec, FrameError, HEADER_LEN};

fn s(v: &[&str]) -> Vec<String> {
    v.iter().map(|x| (*x).to_owned()).collect()
}

fn show(page: Page, select: Select, sort: SortBy, from_chord: bool) -> Show {
    Show { page, select, sort, from_chord }
}

fn frame(sh: &Show) -> Vec<u8> {
    let mut v = Vec::new();
    encode(&InstanceCmd::Show(sh.clone()), &mut v).unwrap_or_else(|e| panic!("{sh:?} does not encode: {e:?}"));
    v
}

/// A whole frame of kind 0x0401 around a hand-written payload.
fn raw(payload: &[u8]) -> Vec<u8> {
    let mut f = (u32::try_from(payload.len()).unwrap() + 2).to_le_bytes().to_vec();
    f.extend_from_slice(&0x0401u16.to_le_bytes());
    f.extend_from_slice(payload);
    f
}

fn dec(buf: &[u8]) -> Result<Show, FrameError> {
    decode::<InstanceCmd>(buf).map(|(InstanceCmd::Show(sh), _)| sh)
}

// ---- constants and the frame layout

#[test]
fn contract_constants_pinned() {
    assert_eq!(INSTANCE.magic.to_le_bytes(), *b"TINS");
    assert_eq!((INSTANCE.version, INSTANCE.max_payload), (1, 1024));
    assert_eq!(copydata_tag(), u32::from_le_bytes(*b"TINS") as usize);
    assert_eq!(WINDOW_CLASS, "MiniOledTasks");
    assert_eq!(MUTEX_NAME, "Local\\mini-oled-dock.tasks");
    assert_eq!(AUMID, "MiniOledDock.Tasks");
    assert_eq!(EXE_NAME, "tasks.exe");
    assert_eq!(MAX_IDENTITY_BYTES, 512);
    assert_eq!(
        [Page::Processes as u8, Page::Performance as u8, Page::Startup as u8, Page::Services as u8],
        [1, 2, 3, 4]
    );
    assert_eq!([SortBy::Auto as u8, SortBy::Cpu as u8, SortBy::Memory as u8], [0, 1, 2]);
    assert_eq!(Show::default(), show(Page::Processes, Select::None, SortBy::Auto, false));
    assert_eq!(InstanceCmd::Show(Show::default()).kind(), 0x0401);
    assert_eq!(<InstanceCmd as Codec>::PROTO, INSTANCE);
}

#[test]
fn show_payload_layout() {
    let none = frame(&Show::default());
    assert_eq!(none, [6, 0, 0, 0, 0x01, 0x04, 1, 0, 0, 0], "len 2 + 4, kind 0x0401, page, sort, chord, tag 0");
    let pid = frame(&show(Page::Startup, Select::Pid(0x0102_0304), SortBy::Memory, true));
    assert_eq!(pid, [10, 0, 0, 0, 0x01, 0x04, 3, 2, 1, 1, 4, 3, 2, 1]);
    let id = frame(&show(Page::Services, Select::Identity("ab".into()), SortBy::Cpu, false));
    assert_eq!(id, [12, 0, 0, 0, 0x01, 0x04, 4, 1, 0, 2, 2, 0, 0, 0, b'a', b'b']);
    for f in [&none, &pid, &id] {
        let (InstanceCmd::Show(back), used) = decode::<InstanceCmd>(f).unwrap();
        assert_eq!(used, f.len());
        assert_eq!(frame(&back), *f);
    }
    let longest = frame(&show(Page::Processes, Select::Identity("x".repeat(512).into()), SortBy::Auto, false));
    assert_eq!(longest.len(), HEADER_LEN + 4 + 4 + 512, "the largest Show is far under max_payload");
}

#[test]
fn decode_refuses_each_rule() {
    let bad = |payload: &[u8]| dec(&raw(payload));
    assert_eq!(bad(&[0, 0, 0, 0]), Err(FrameError::BadValue { field: "Show.page", value: 0 }));
    assert_eq!(bad(&[5, 0, 0, 0]), Err(FrameError::BadValue { field: "Show.page", value: 5 }));
    assert_eq!(bad(&[1, 3, 0, 0]), Err(FrameError::BadValue { field: "Show.sort", value: 3 }));
    assert_eq!(bad(&[1, 0, 2, 0]), Err(FrameError::BadValue { field: "Show.from_chord", value: 2 }));
    assert_eq!(bad(&[1, 0, 0, 3]), Err(FrameError::BadValue { field: "Show.select", value: 3 }));
    assert_eq!(bad(&[1, 0, 0, 0, 9]), Err(FrameError::Trailing { extra: 1 }));
    assert_eq!(bad(&[1, 0, 0, 1, 1, 2, 3]), Err(FrameError::Invalid { field: "payload" }), "a pid cut short");
    let mut long = vec![1, 0, 0, 2];
    long.extend_from_slice(&513u32.to_le_bytes());
    long.extend_from_slice(&[b'a'; 513]);
    assert_eq!(bad(&long), Err(FrameError::Cap { field: "Show.select", len: 513, max: 512 }));
    assert_eq!(bad(&[1, 0, 0, 2, 2, 0, 0, 0, 0xC3, 0x28]), Err(FrameError::BadUtf8 { field: "Show.select" }));
    assert_eq!(bad(&[1, 0, 0, 2, 0, 0, 0, 0]), Err(FrameError::Invalid { field: "Show.select" }), "empty");
    assert_eq!(bad(&[1, 0, 0, 2, 2, 0, 0, 0, b'a', 7]), Err(FrameError::Invalid { field: "Show.select" }), "BEL");
    assert_eq!(bad(&[1, 0, 0, 2, 3, 0, 0, 0, b'-', b'-', b'x']), Err(FrameError::Invalid { field: "Show.select" }));
    let mut other = raw(&[1, 0, 0, 0]);
    other[4] = 0x02;
    assert_eq!(dec(&other), Err(FrameError::UnknownKind(0x0402)));
    let mut huge = vec![0u8; 4];
    huge[..4].copy_from_slice(&1027u32.to_le_bytes());
    assert_eq!(dec(&huge), Err(FrameError::TooLong { len: 1025, max: 1024 }));
}

// ---- the command line

#[test]
fn show_args_canonical() {
    assert!(show_args(&Show::default()).is_empty(), "the default Show gives no argument");
    let sh = show(Page::Startup, Select::Pid(42), SortBy::Memory, true);
    assert_eq!(show_args(&sh), s(&["--page", "startup", "--select", "42", "--sort", "memory", "--from-chord"]));
    let sh = show(Page::Performance, Select::Identity("Orbit.App".into()), SortBy::Cpu, false);
    assert_eq!(show_args(&sh), s(&["--page", "performance", "--select", "Orbit.App", "--sort", "cpu"]));
    assert_eq!(show_args(&show(Page::Services, Select::None, SortBy::Auto, false)), s(&["--page", "services"]));
    assert_eq!(show_args(&show(Page::Processes, Select::None, SortBy::Auto, true)), s(&["--from-chord"]));
}

#[test]
fn parse_any_order() {
    let want = show(Page::Startup, Select::Pid(42), SortBy::Memory, true);
    for args in [
        &["--page", "startup", "--select", "42", "--sort", "memory", "--from-chord"][..],
        &["--from-chord", "--sort", "memory", "--select", "42", "--page", "startup"],
        &["--select", "42", "--from-chord", "--page", "startup", "--sort", "memory"],
    ] {
        assert_eq!(parse_show(&s(args)), Ok(want.clone()), "{args:?}");
    }
    assert_eq!(parse_show(&[]), Ok(Show::default()));
    assert_eq!(
        parse_show(&s(&["--page", "processes", "--sort", "auto"])),
        Ok(Show::default()),
        "defaults may be named"
    );
    assert_eq!(parse_show(&s(&["--select", "quill.exe"])).map(|x| x.select), Ok(Select::Identity("quill.exe".into())));
}

#[test]
fn parse_refuses() {
    let p = |v: &[&str]| parse_show(&s(v));
    assert_eq!(p(&["--verbose"]), Err(ArgError::Unknown("--verbose".into())));
    assert_eq!(p(&["42"]), Err(ArgError::Unknown("42".into())), "a value without its flag");
    assert_eq!(p(&["--page", "startup", "--page", "services"]), Err(ArgError::Twice("--page")));
    assert_eq!(p(&["--from-chord", "--from-chord"]), Err(ArgError::Twice("--from-chord")));
    assert_eq!(p(&["--sort", "cpu", "--sort", "cpu"]), Err(ArgError::Twice("--sort")));
    assert_eq!(p(&["--select", "1", "--select", "2"]), Err(ArgError::Twice("--select")));
    assert_eq!(p(&["--select"]), Err(ArgError::Missing("--select")));
    assert_eq!(p(&["--page"]), Err(ArgError::Missing("--page")));
    assert_eq!(p(&["--sort"]), Err(ArgError::Missing("--sort")));
    assert_eq!(p(&["--select", ""]), Err(ArgError::Bad("--select")));
    let long = "a".repeat(513);
    assert_eq!(p(&["--select", long.as_str()]), Err(ArgError::Bad("--select")), "513 bytes");
    let at_cap = "\u{e9}".repeat(256);
    assert_eq!(
        p(&["--select", at_cap.as_str()]).map(|x| x.select),
        Ok(Select::Identity(at_cap.clone().into())),
        "512 bytes"
    );
    assert_eq!(p(&["--select", "orbit\u{7}"]), Err(ArgError::Bad("--select")), "a control character");
    assert_eq!(p(&["--select", "--from-chord"]), Err(ArgError::Bad("--select")), "an identity starting with --");
    assert_eq!(p(&["--page", "x"]), Err(ArgError::Bad("--page")));
    assert_eq!(p(&["--page", "Startup"]), Err(ArgError::Bad("--page")), "names are lower case");
    assert_eq!(p(&["--sort", "gpu"]), Err(ArgError::Bad("--sort")));
    assert_eq!(p(&["--from-chord=1"]), Err(ArgError::Unknown("--from-chord=1".into())));
}

#[test]
fn pid_or_identity() {
    let sel = |v: &str| parse_show(&s(&["--select", v])).map(|x| x.select);
    assert_eq!(sel("4294967295"), Ok(Select::Pid(u32::MAX)));
    assert_eq!(sel("0"), Ok(Select::Pid(0)));
    assert_eq!(sel("4294967296"), Ok(Select::Identity("4294967296".into())), "does not fit u32");
    assert_eq!(sel("12345678901"), Ok(Select::Identity("12345678901".into())), "11 digits");
    assert_eq!(sel("0123x"), Ok(Select::Identity("0123x".into())));
    assert_eq!(sel("-1"), Ok(Select::Identity("-1".into())));
    let path = "c:\\program files\\orbit\\orbit.exe";
    assert_eq!(sel(path), Ok(Select::Identity(path.into())));
}

// ---- the property and the fuzz

/// xorshift64* (Vigna), seeded through splitmix64 so any seed (0 included) gives a good stream.
struct Rng(u64);

impl Rng {
    fn new(seed: u64) -> Rng {
        let mut z = seed.wrapping_add(0x9E37_79B9_7F4A_7C15);
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        Rng((z ^ (z >> 31)) | 1)
    }

    fn next(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x >> 12;
        x ^= x << 25;
        x ^= x >> 27;
        self.0 = x;
        x.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }

    fn below(&mut self, n: usize) -> usize {
        (self.next() % n.max(1) as u64) as usize
    }

    fn bytes(&mut self, n: usize) -> Vec<u8> {
        (0..n).map(|_| self.next() as u8).collect()
    }
}

/// Characters of 1 to 4 UTF-8 bytes, path and AUMID punctuation among them; never a control character.
const ALPHABET: [char; 16] =
    ['a', 'Z', '7', '.', '!', '_', '\\', ':', ' ', '-', '\u{e9}', '\u{3b1}', '\u{6f22}', '\u{1F600}', '{', '0'];

/// A valid identity that the command line cannot read as a pid: 1..=512 bytes, never all digits, never "--" first.
fn identity(rng: &mut Rng) -> Box<str> {
    let cap = if rng.below(8) == 0 { 512 } else { 1 + rng.below(64) };
    let mut v = String::new();
    while v.len() < cap {
        let c = ALPHABET[rng.below(ALPHABET.len())];
        if v.len() + c.len_utf8() > cap {
            break;
        }
        v.push(c);
    }
    if v.is_empty() || v.starts_with("--") || v.bytes().all(|b| b.is_ascii_digit()) {
        v.replace_range(..v.chars().next().map_or(0, char::len_utf8), "x");
    }
    v.into()
}

fn random_show(rng: &mut Rng) -> Show {
    let page = [Page::Processes, Page::Performance, Page::Startup, Page::Services][rng.below(4)];
    let sort = [SortBy::Auto, SortBy::Cpu, SortBy::Memory][rng.below(3)];
    let select = match rng.below(3) {
        0 => Select::None,
        1 => Select::Pid(if rng.below(4) == 0 { u32::MAX } else { rng.next() as u32 }),
        _ => Select::Identity(identity(rng)),
    };
    show(page, select, sort, rng.below(2) == 1)
}

#[test]
fn roundtrip_property() {
    let mut rng = Rng::new(0x7A5C);
    let mut max_seen = 0;
    for _ in 0..10_000 {
        let sh = random_show(&mut rng);
        if let Select::Identity(id) = &sh.select {
            max_seen = max_seen.max(id.len());
        }
        assert_eq!(parse_show(&show_args(&sh)), Ok(sh.clone()), "{sh:?}");
        let f = frame(&sh);
        assert_eq!(decode::<InstanceCmd>(&f), Ok((InstanceCmd::Show(sh.clone()), f.len())), "{sh:?}");
    }
    assert!(max_seen > 500, "identities near the 512-byte cap were generated");
}

/// One input through decode: never a panic; an Ok re-encodes to the bytes it was decoded from.
fn decode_one(buf: &[u8]) -> bool {
    match decode::<InstanceCmd>(buf) {
        Ok((cmd, used)) => {
            let mut again = Vec::new();
            encode(&cmd, &mut again).unwrap_or_else(|e| panic!("{cmd:?} decoded but does not encode: {e:?}"));
            assert_eq!(again.as_slice(), &buf[..used], "{cmd:?} is not canonical");
            true
        }
        Err(_) => false,
    }
}

/// random buffers of 0..64 bytes, as many payloads of 0..64 bytes behind a correct header, and as many one-byte
/// mutations (any position, any value) of valid frames; the count of inputs that decoded.
fn fuzz(n: usize, seed: u64) -> usize {
    let mut rng = Rng::new(seed);
    let mut ok = 0;
    for _ in 0..n {
        let len = rng.below(64);
        ok += usize::from(decode_one(&rng.bytes(len)));
        let len = rng.below(64);
        ok += usize::from(decode_one(&raw(&rng.bytes(len))));
        let mut f = frame(&random_show(&mut rng));
        let at = rng.below(f.len());
        f[at] = rng.next() as u8;
        ok += usize::from(decode_one(&f));
    }
    ok
}

#[test]
fn decode_total() {
    let ok = fuzz(10_000, 0xD3C0);
    assert!(ok > 1_000, "the mutations keep many frames valid ({ok})");
}

/// OLED_FUZZ_SEED as a u64 (decimal, or hex after 0x) when set, else 0x5EED; printed first, so a failing night can be
/// replayed with the seed its log shows.
fn nightly_seed(test: &str) -> u64 {
    let seed = match std::env::var("OLED_FUZZ_SEED") {
        Ok(v) => {
            let t = v.trim();
            let parsed = match t.strip_prefix("0x").or_else(|| t.strip_prefix("0X")) {
                Some(hex) => u64::from_str_radix(hex, 16),
                None => t.parse(),
            };
            parsed.unwrap_or_else(|e| panic!("OLED_FUZZ_SEED={v:?} is not a u64: {e}"))
        }
        Err(_) => 0x5EED,
    };
    println!("{test}: seed {seed:#x} (set OLED_FUZZ_SEED={seed:#x} to replay)");
    seed
}

#[test]
#[ignore = "fuzz: 1,000,000 inputs, nightly (fuzz-nightly.yml runs every ignored fuzz_ test in release)"]
fn fuzz_tasks_instance() {
    fuzz(333_334, nightly_seed("fuzz_tasks_instance"));
}

#[test]
fn wire_reader_sees_the_same_payload() {
    // The payload read back field by field with the wire primitives, as a second decoder would.
    let f = frame(&show(Page::Performance, Select::Identity("Orbit.App".into()), SortBy::Cpu, true));
    let mut r = wire::Reader::new(&f[HEADER_LEN..]);
    assert_eq!((r.u8().unwrap(), r.u8().unwrap(), r.bool("c").unwrap(), r.u8().unwrap()), (2, 1, true, 2));
    assert_eq!(r.str("id", 512).unwrap(), "Orbit.App");
    r.finish().unwrap();
}
