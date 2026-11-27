//! The tasks-admin frames (P1-tasks-engine TA1, TA1b): round trips of every kind with the payload layouts pinned,
//! canonical bytes, one refusal per rule, the FNV-1a vectors of name_hash, a 500,000-input fuzz smoke on every push,
//! 1,000,000 inputs per frame type nightly, and the seed corpus of the cargo-fuzz targets (crates/oled-trust/fuzz).
//! Every fuzz layer runs the same harness bodies, fuzz_request and fuzz_reply.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use oled_trust::frame::admin::*;

const REQUEST_KINDS: [u16; 8] = [1, 2, 3, 4, 5, 6, 7, 8];
const REPLY_KINDS: [u16; 4] = [0x81, 0x82, 0x83, 0x84];
const PRIORITIES: [PriorityClass; 5] = [
    PriorityClass::Idle,
    PriorityClass::BelowNormal,
    PriorityClass::Normal,
    PriorityClass::AboveNormal,
    PriorityClass::High,
];
const OPS: [SvcOp; 3] = [SvcOp::Start, SvcOp::Stop, SvcOp::Restart];
const SOURCES: [StartupSource; 3] = [StartupSource::HklmRun, StartupSource::HklmRun32, StartupSource::CommonStartup];
const REASONS: [DenyReason; 13] = [
    DenyReason::Gone,
    DenyReason::OtherSession,
    DenyReason::Critical,
    DenyReason::Protected,
    DenyReason::OwnProcess,
    DenyReason::RebootOnFailure,
    DenyReason::UnknownService,
    DenyReason::ServiceImagePath,
    DenyReason::UnknownItem,
    DenyReason::NeedsConsent,
    DenyReason::RateLimited,
    DenyReason::Disabled,
    DenyReason::BadFrame,
];

// ---- helpers

/// A 10-byte header with the right magic and version.
fn header(kind: u16, len: u16) -> Vec<u8> {
    let mut h = MAGIC.to_le_bytes().to_vec();
    h.extend_from_slice(&PROTO.to_le_bytes());
    h.extend_from_slice(&kind.to_le_bytes());
    h.extend_from_slice(&len.to_le_bytes());
    h
}

/// A frame built by hand: the header with len = the payload's size, then the payload.
fn raw(kind: u16, payload: &[u8]) -> Vec<u8> {
    let mut f = header(kind, u16::try_from(payload.len()).unwrap());
    f.extend_from_slice(payload);
    f
}

fn enc_req(r: &AdminRequest) -> Vec<u8> {
    let mut v = Vec::new();
    encode_request(r, &mut v).unwrap_or_else(|e| panic!("{r:?} does not encode: {e:?}"));
    v
}

fn enc_rep(r: &AdminReply) -> Vec<u8> {
    let mut v = Vec::new();
    encode_reply(r, &mut v).unwrap_or_else(|e| panic!("{r:?} does not encode: {e:?}"));
    v
}

fn kind_of(frame: &[u8]) -> u16 {
    u16::from_le_bytes([frame[6], frame[7]])
}

fn key(pid: u32, created: u64) -> ProcKey {
    ProcKey { pid, created }
}

/// n distinct keys.
fn keys(n: usize) -> KeyList {
    KeyList::new((0..n).map(|i| key(1_000 + 4 * i as u32, 0x01D9_0000_0000_0000 + i as u64)).collect()).unwrap()
}

fn units(s: &str) -> Vec<u16> {
    s.encode_utf16().collect()
}

/// UTF-16LE bytes of a unit list.
fn le(units: &[u16]) -> Vec<u8> {
    units.iter().flat_map(|u| u.to_le_bytes()).collect()
}

fn key_bytes(k: ProcKey) -> Vec<u8> {
    let mut v = k.pid.to_le_bytes().to_vec();
    v.extend_from_slice(&k.created.to_le_bytes());
    v
}

/// A service name of exactly n units, letters and digits only.
fn long_name(n: usize) -> String {
    (0..n).map(|i| char::from(b"abcdefghijklmnopqrstuvwxyz0123456789"[i % 36])).collect()
}

/// A Text of exactly 16,384 units: three-byte UTF-8 characters, a surrogate pair, a NUL and ASCII, so the decoded
/// string is the largest the cap allows.
fn longest_text() -> Box<str> {
    let mut s: String = std::iter::repeat_n('\u{4E2D}', MAX_TEXT_UNITS - 4).collect();
    s.push('\u{1F600}');
    s.push('\0');
    s.push('a');
    assert_eq!(s.encode_utf16().count(), MAX_TEXT_UNITS);
    s.into_boxed_str()
}

/// One sample or more of every request kind, every value of every nested enum among them, and the largest cases
/// (64 keys, a 256-unit service name). The seed corpus of the cargo-fuzz targets is these frames.
fn request_vectors() -> Vec<AdminRequest> {
    let k = key(0x0403_0201, 0x0C0B_0A09_0807_0605);
    let mut v = vec![
        AdminRequest::Hello { proto: PROTO },
        AdminRequest::Hello { proto: 0 },
        AdminRequest::Hello { proto: u16::MAX },
        AdminRequest::Terminate { keys: keys(1), tree: false },
        AdminRequest::Terminate { keys: keys(MAX_KEYS), tree: true },
        AdminRequest::Terminate { keys: KeyList::new(vec![key(7, 2), key(7, 1), key(0, 0)]).unwrap(), tree: true },
    ];
    v.extend(PRIORITIES.map(|class| AdminRequest::SetPriority { key: k, class }));
    v.extend([
        AdminRequest::SetEfficiency { key: k, on: true },
        AdminRequest::SetEfficiency { key: key(u32::MAX, u64::MAX), on: false },
        AdminRequest::QueryCommandLine { key: k },
        AdminRequest::QueryCommandLine { key: key(0, 0) },
    ]);
    v.extend(OPS.map(|op| AdminRequest::ServiceControl { name: ServiceName::new("Spooler").unwrap(), op }));
    v.extend([
        AdminRequest::ServiceControl { name: ServiceName::new(&long_name(MAX_SERVICE_NAME)).unwrap(), op: SvcOp::Stop },
        AdminRequest::ServiceControl { name: ServiceName::new("Dienst \u{e9}\u{1F600}").unwrap(), op: SvcOp::Start },
        AdminRequest::ServiceControl { name: ServiceName::new("a").unwrap(), op: SvcOp::Restart },
    ]);
    for (i, source) in SOURCES.into_iter().enumerate() {
        let on = i % 2 == 0;
        let item = MachineStartupId { source, ordinal: i as u16, name_hash: name_hash(&units("Updater")) };
        v.push(AdminRequest::StartupSetEnabled { item, on });
    }
    let item = MachineStartupId { source: StartupSource::HklmRun, ordinal: u16::MAX, name_hash: u32::MAX };
    v.push(AdminRequest::StartupSetEnabled { item, on: false });
    v.push(AdminRequest::RestartDock);
    v
}

/// One sample or more of every reply kind, every DenyReason, and the 16,384-unit Text.
fn reply_vectors() -> Vec<AdminReply> {
    let mut v = vec![
        AdminReply::Ok,
        AdminReply::Text("".into()),
        AdminReply::Text("C:\\Program Files\\App\\app.exe --flag \"a b\"".into()),
        AdminReply::Text("\u{e9}\u{4E2D}\u{1F600}\0\r\n".into()),
        AdminReply::Text(longest_text()),
    ];
    v.extend(REASONS.map(AdminReply::Denied));
    v.extend([
        AdminReply::Failed { win32: 0 },
        AdminReply::Failed { win32: 5 },
        AdminReply::Failed { win32: u32::MAX },
    ]);
    v
}

// ---- round trips

#[test]
fn round_trip_every_request_and_reply() {
    let mut seen = Vec::new();
    for r in request_vectors() {
        let f = enc_req(&r);
        assert_eq!(f[..4], *b"MOTA", "{r:?}: magic");
        assert_eq!(f[4..6], PROTO.to_le_bytes(), "{r:?}: version");
        assert_eq!(usize::from(u16::from_le_bytes([f[8], f[9]])), f.len() - 10, "{r:?}: len is the payload's size");
        assert!(f.len() - 10 <= MAX_REQUEST_PAYLOAD, "{r:?}: within the request cap");
        assert_eq!(decode_request(&f), Ok(r.clone()));
        assert_eq!(decode_reply(&f), Err(AdminError::Kind(kind_of(&f))), "{r:?}: a request is never a reply");
        seen.push(kind_of(&f));
    }
    for r in reply_vectors() {
        let f = enc_rep(&r);
        assert_eq!(f[..4], *b"MOTA", "{r:?}: magic");
        assert_eq!(usize::from(u16::from_le_bytes([f[8], f[9]])), f.len() - 10, "{r:?}: len is the payload's size");
        assert!(f.len() - 10 <= MAX_REPLY_PAYLOAD, "{r:?}: within the reply cap");
        assert_eq!(decode_reply(&f), Ok(r.clone()));
        assert_eq!(decode_request(&f), Err(AdminError::Kind(kind_of(&f))), "{r:?}: a reply is never a request");
        seen.push(kind_of(&f));
    }
    for k in REQUEST_KINDS.iter().chain(&REPLY_KINDS) {
        assert!(seen.contains(k), "kind {k:#04x} has no vector");
    }

    // The largest frames sit at their caps exactly.
    let f = enc_req(&AdminRequest::Terminate { keys: keys(MAX_KEYS), tree: true });
    assert_eq!(f.len(), 10 + 1 + 12 * MAX_KEYS + 1);
    let name = ServiceName::new(&long_name(MAX_SERVICE_NAME)).unwrap();
    assert_eq!(enc_req(&AdminRequest::ServiceControl { name, op: SvcOp::Stop }).len(), 10 + 2 + 2 * 256 + 1);
    assert_eq!(enc_rep(&AdminReply::Text(longest_text())).len(), 10 + MAX_REPLY_PAYLOAD);

    // The payload layouts of P1-tasks-engine 2.5, byte for byte.
    let k = key(0x0403_0201, 0x0C0B_0A09_0807_0605);
    let kb: [u8; 12] = [1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12];
    let pinned: Vec<(Vec<u8>, u16, Vec<u8>)> = vec![
        (enc_req(&AdminRequest::Hello { proto: 0x0201 }), 1, vec![1, 2]),
        (
            enc_req(&AdminRequest::Terminate { keys: KeyList::new(vec![k]).unwrap(), tree: true }),
            2,
            [&[1][..], &kb, &[1]].concat(),
        ),
        (enc_req(&AdminRequest::SetPriority { key: k, class: PriorityClass::High }), 3, [&kb[..], &[5]].concat()),
        (enc_req(&AdminRequest::SetEfficiency { key: k, on: false }), 4, [&kb[..], &[0]].concat()),
        (enc_req(&AdminRequest::QueryCommandLine { key: k }), 5, kb.to_vec()),
        (
            enc_req(&AdminRequest::ServiceControl { name: ServiceName::new("Ab").unwrap(), op: SvcOp::Stop }),
            6,
            vec![2, 0, b'A', 0, b'b', 0, 2],
        ),
        (
            enc_req(&AdminRequest::StartupSetEnabled {
                item: MachineStartupId {
                    source: StartupSource::CommonStartup,
                    ordinal: 0x0201,
                    name_hash: 0x0605_0403,
                },
                on: true,
            }),
            7,
            vec![3, 1, 2, 3, 4, 5, 6, 1],
        ),
        (enc_req(&AdminRequest::RestartDock), 8, vec![]),
        (enc_rep(&AdminReply::Ok), 0x81, vec![]),
        (enc_rep(&AdminReply::Text("h\u{e9}".into())), 0x82, vec![2, 0, b'h', 0, 0xE9, 0]),
        (enc_rep(&AdminReply::Denied(DenyReason::Critical)), 0x83, vec![3]),
        (enc_rep(&AdminReply::Failed { win32: 0x0403_0201 }), 0x84, vec![1, 2, 3, 4]),
    ];
    for (frame, kind, payload) in pinned {
        assert_eq!(frame, raw(kind, &payload), "kind {kind:#04x}");
    }
    for (r, n) in REASONS.iter().zip(1u8..) {
        assert_eq!(enc_rep(&AdminReply::Denied(*r)), raw(0x83, &[n]), "{r:?}");
    }
    for (c, n) in PRIORITIES.iter().zip(1u8..) {
        assert_eq!(*enc_req(&AdminRequest::SetPriority { key: k, class: *c }).last().unwrap(), n, "{c:?}");
    }

    // encode appends: what the buffer held stays in front.
    let mut out = vec![0xEE, 0xEE];
    encode_request(&AdminRequest::RestartDock, &mut out).unwrap();
    encode_reply(&AdminReply::Ok, &mut out).unwrap();
    assert_eq!(out, [vec![0xEE, 0xEE], raw(8, &[]), raw(0x81, &[])].concat());
}

/// encode(decode(b)) == b and decode(encode(x)) == x over 10,000 seeded valid frames.
#[test]
fn canonical_bytes() {
    let mut rng = Rng::new(0xC0DE_C0DE);
    for i in 0..10_000 {
        if i % 2 == 0 {
            let r = any_request(&mut rng);
            let b = enc_req(&r);
            let back = decode_request(&b).unwrap_or_else(|e| panic!("frame {i}: {r:?} does not decode: {e:?}"));
            assert_eq!(back, r, "frame {i}");
            assert_eq!(enc_req(&back), b, "frame {i}");
        } else {
            let r = any_reply(&mut rng);
            let b = enc_rep(&r);
            let back = decode_reply(&b).unwrap_or_else(|e| panic!("frame {i}: {r:?} does not decode: {e:?}"));
            assert_eq!(back, r, "frame {i}");
            assert_eq!(enc_rep(&back), b, "frame {i}");
        }
    }
}

// ---- refusals

#[track_caller]
fn req_refused(frame: &[u8], want: AdminError) {
    assert_eq!(decode_request(frame), Err(want));
    fuzz_request(frame);
}

#[track_caller]
fn rep_refused(frame: &[u8], want: AdminError) {
    assert_eq!(decode_reply(frame), Err(want));
    fuzz_reply(frame);
}

#[test]
fn rejects_each_rule() {
    use AdminError::*;
    let hello = enc_req(&AdminRequest::Hello { proto: PROTO });
    let k = key_bytes(key(42, 7));

    // The header: too short, magic, version, kind.
    req_refused(&[], Header);
    req_refused(&hello[..9], Header);
    rep_refused(&hello[..5], Header);
    let mut f = hello.clone();
    f[3] = b'B';
    req_refused(&f, Magic);
    for v in [0u16, 2, u16::MAX] {
        let mut f = hello.clone();
        f[4..6].copy_from_slice(&v.to_le_bytes());
        req_refused(&f, Version);
    }
    for kind in [0u16, 9, 0x80, 0x81, 0x0101, u16::MAX] {
        req_refused(&raw(kind, &[]), Kind(kind));
    }
    for kind in [0u16, 1, 8, 0x80, 0x85, 0x0181, u16::MAX] {
        rep_refused(&raw(kind, &[]), Kind(kind));
    }
    // The header is judged before the payload: a bad magic over a bad payload is Magic.
    let mut f = raw(2, &[0]);
    f[0] = 0;
    req_refused(&f, Magic);

    // len against the bytes and the content.
    req_refused(&raw(1, &[1]), Length); // len shorter than the content
    req_refused(&raw(1, &[1, 0, 0]), Length); // len longer than the content
    req_refused(&raw(3, &k), Length); // SetPriority without its class byte
    rep_refused(&raw(0x84, &[1, 2, 3]), Length);
    rep_refused(&raw(0x81, &[0]), Length);
    let mut cut = hello.clone();
    cut.pop();
    req_refused(&cut, Length); // fewer bytes than len
    let mut f = hello.clone();
    f.push(0);
    req_refused(&f, Trailing); // a byte after the frame
    let mut f = enc_req(&AdminRequest::RestartDock);
    f.extend_from_slice(&hello);
    req_refused(&f, Trailing); // two frames in one buffer
    let mut f = enc_rep(&AdminReply::Ok);
    f.push(0);
    rep_refused(&f, Trailing);

    // The caps of whole payloads, judged from the header before the payload is read.
    req_refused(&raw(6, &vec![0; MAX_REQUEST_PAYLOAD + 1]), TooLong);
    req_refused(&header(2, 1_025), TooLong);
    req_refused(&header(2, u16::MAX), TooLong);
    rep_refused(&header(0x82, (MAX_REPLY_PAYLOAD + 1) as u16), TooLong);
    let over: Vec<u8> = [&16_385u16.to_le_bytes()[..], &vec![b'a'; 2 * 16_385]].concat();
    rep_refused(&raw(0x82, &over), TooLong); // a Text over 16,384 units in an honest frame
    rep_refused(&raw(0x82, &16_385u16.to_le_bytes()), TooLong); // the count is judged before the units are read
    rep_refused(&raw(0x82, &u16::MAX.to_le_bytes()), TooLong);

    // Terminate: count 1..=64, no duplicate, tree 0 or 1.
    req_refused(&raw(2, &[0, 0]), Field("keys"));
    let mut p = vec![65u8];
    for i in 0..65u32 {
        p.extend_from_slice(&key_bytes(key(i, 1)));
    }
    p.push(0);
    req_refused(&raw(2, &p), Field("keys"));
    req_refused(&raw(2, &[65]), Field("keys")); // judged before the keys are read
    req_refused(&raw(2, &[u8::MAX]), Field("keys"));
    req_refused(&raw(2, &[[2u8].as_slice(), &k, &k, &[0]].concat()), Field("keys"));
    req_refused(&raw(2, &[[1u8].as_slice(), &k, &[2]].concat()), Field("tree"));
    req_refused(&raw(2, &[[2u8].as_slice(), &k].concat()), Length); // fewer keys than the count

    // Enums and bools.
    for class in [0u8, 6, u8::MAX] {
        req_refused(&raw(3, &[&k[..], &[class]].concat()), Field("class")); // 6 would be Realtime
    }
    for on in [2u8, u8::MAX] {
        req_refused(&raw(4, &[&k[..], &[on]].concat()), Field("on"));
        req_refused(&raw(7, &[1, 0, 0, 0, 0, 0, 0, on]), Field("on"));
    }
    for op in [0u8, 4, u8::MAX] {
        req_refused(&raw(6, &[1, 0, b'a', 0, op]), Field("op"));
    }
    for source in [0u8, 4, u8::MAX] {
        req_refused(&raw(7, &[source, 0, 0, 0, 0, 0, 0, 1]), Field("source"));
    }
    for reason in [0u8, 14, u8::MAX] {
        rep_refused(&raw(0x83, &[reason]), Field("reason"));
    }

    // Service names: 1..=256 units, valid UTF-16, no NUL, backslash or slash.
    let svc = |name: &[u16]| raw(6, &[&(name.len() as u16).to_le_bytes()[..], &le(name), &[1]].concat());
    req_refused(&svc(&[]), Field("service name"));
    req_refused(&svc(&units(&long_name(257))), Field("service name"));
    req_refused(&raw(6, &[1, 1, b'a', 0, 1]), Field("service name")); // 257 announced, judged before reading
    for bad in ["a\0b", "a\\b", "a/b", "\\", "\0"] {
        req_refused(&svc(&units(bad)), Field("service name"));
    }
    for bad in [vec![0xD800], vec![0x61, 0xDC00], vec![0xD800, 0x61], vec![0xDBFF]] {
        req_refused(&svc(&bad), Field("service name"));
    }
    assert!(decode_request(&svc(&units(&long_name(256)))).is_ok());
    assert!(decode_request(&svc(&[0xD83D, 0xDE00])).is_ok(), "a paired surrogate is a character");

    // Text: valid UTF-16.
    for bad in [vec![0xD800], vec![0xDC00, 0x61], vec![0x61, 0xD83D]] {
        rep_refused(&raw(0x82, &[&(bad.len() as u16).to_le_bytes()[..], &le(&bad)].concat()), Field("text"));
    }
    rep_refused(&raw(0x82, &[2, 0, b'a', 0]), Length); // two units announced, one present

    // encode refuses what decode would refuse, and leaves the buffer as it was.
    let mut too_long: String = long_name(MAX_TEXT_UNITS);
    too_long.push('x');
    let mut out = vec![9u8];
    assert_eq!(encode_reply(&AdminReply::Text(too_long.into()), &mut out), Err(TooLong));
    assert_eq!(out, [9]);
    let pair_over: String = std::iter::repeat_n('\u{1F600}', MAX_TEXT_UNITS / 2).chain(['a']).collect();
    assert_eq!(encode_reply(&AdminReply::Text(pair_over.into()), &mut out), Err(TooLong));
    assert_eq!(out, [9]);
    assert!(encode_reply(&AdminReply::Text(long_name(MAX_TEXT_UNITS).into()), &mut out).is_ok());
}

// ---- name_hash

/// FNV-1a 32 over bytes, written out here so name_hash is checked against an independent copy.
fn fnv1a(bytes: &[u8]) -> u32 {
    bytes.iter().fold(0x811C_9DC5, |h, b| (h ^ u32::from(*b)).wrapping_mul(0x0100_0193))
}

#[test]
fn name_hash_vectors() {
    // The reference against the published FNV-1a 32 vectors.
    assert_eq!(fnv1a(b""), 0x811C_9DC5);
    assert_eq!(fnv1a(b"a"), 0xE40C_292C);
    assert_eq!(fnv1a(b"foobar"), 0xBF9C_F968);

    assert_eq!(name_hash(&[]), 0x811C_9DC5);
    assert_eq!(name_hash(&units("Run")), name_hash(&units("run")));
    assert_eq!(name_hash(&units("RUN")), name_hash(&units("run")));
    assert_eq!(name_hash(&units("run")), fnv1a(&[b'r', 0, b'u', 0, b'n', 0]), "the little-endian bytes of each unit");
    assert_eq!(name_hash(&units("A-Z_09")), fnv1a(&le(&units("a-z_09"))));
    let wide = [0x00C9, 0x4E2D, 0xD83D, 0xDE00];
    assert_eq!(name_hash(&wide), fnv1a(&le(&wide)), "units outside A-Z are hashed as they are");
    assert_ne!(name_hash(&units("\u{c9}")), name_hash(&units("\u{e9}")), "only ASCII is lowered");
    assert_ne!(name_hash(&units("run")), name_hash(&units("runs")));
}

// ---- fuzz

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

    fn coin(&mut self) -> bool {
        self.next() & 1 == 1
    }

    fn bytes(&mut self, n: usize) -> Vec<u8> {
        let mut v = Vec::with_capacity(n + 8);
        while v.len() < n {
            v.extend_from_slice(&self.next().to_le_bytes());
        }
        v.truncate(n);
        v
    }

    fn pick<T: Copy>(&mut self, items: &[T]) -> T {
        items[self.below(items.len())]
    }

    /// 1..=max, mostly small.
    fn size(&mut self, max: usize) -> usize {
        if self.below(16) == 0 {
            1 + self.below(max)
        } else {
            1 + self.below(max.min(8))
        }
    }
}

fn any_key(rng: &mut Rng) -> ProcKey {
    // Few pids, so the same pid with another creation time comes up.
    let pid = if rng.coin() { rng.below(8) as u32 * 4 } else { rng.next() as u32 };
    key(pid, rng.next() >> rng.below(64))
}

fn any_keys(rng: &mut Rng) -> KeyList {
    let n = rng.size(MAX_KEYS);
    let mut v: Vec<ProcKey> = Vec::with_capacity(n);
    while v.len() < n {
        let k = any_key(rng);
        if !v.contains(&k) {
            v.push(k);
        }
    }
    KeyList::new(v).unwrap()
}

/// Any Unicode scalar value: ASCII (NUL and controls included), the rest of the BMP, or past it.
fn any_char(rng: &mut Rng) -> char {
    loop {
        let c = match rng.below(4) {
            0 | 1 => rng.below(0x80) as u32,
            2 => 0x80 + rng.below(0xFF80) as u32,
            _ => 0x1_0000 + rng.below(0x10_0000) as u32,
        };
        if let Some(c) = char::from_u32(c) {
            return c;
        }
    }
}

/// A string of at most the given number of UTF-16 units (exactly that many unless a surrogate pair did not fit at
/// the end).
fn any_string(rng: &mut Rng, units: usize, char_of: impl Fn(&mut Rng) -> char) -> String {
    let mut s = String::new();
    let mut n = 0;
    while n < units {
        let c = char_of(rng);
        if n + c.len_utf16() > units {
            break;
        }
        n += c.len_utf16();
        s.push(c);
    }
    s
}

fn any_name(rng: &mut Rng) -> ServiceName {
    const CHARS: [char; 12] = ['a', 'Z', '0', ' ', '-', '_', '.', '\u{e9}', '\u{df}', '\u{4E2D}', '\u{1F600}', '~'];
    let n = rng.size(MAX_SERVICE_NAME);
    let mut s = any_string(rng, n, |r| r.pick(&CHARS));
    if s.is_empty() {
        s.push('s');
    }
    ServiceName::new(&s).unwrap()
}

fn any_request(rng: &mut Rng) -> AdminRequest {
    match rng.pick(&REQUEST_KINDS) {
        1 => AdminRequest::Hello { proto: rng.next() as u16 },
        2 => AdminRequest::Terminate { keys: any_keys(rng), tree: rng.coin() },
        3 => AdminRequest::SetPriority { key: any_key(rng), class: rng.pick(&PRIORITIES) },
        4 => AdminRequest::SetEfficiency { key: any_key(rng), on: rng.coin() },
        5 => AdminRequest::QueryCommandLine { key: any_key(rng) },
        6 => AdminRequest::ServiceControl { name: any_name(rng), op: rng.pick(&OPS) },
        7 => AdminRequest::StartupSetEnabled {
            item: MachineStartupId {
                source: rng.pick(&SOURCES),
                ordinal: rng.next() as u16,
                name_hash: rng.next() as u32,
            },
            on: rng.coin(),
        },
        _ => AdminRequest::RestartDock,
    }
}

fn any_reply(rng: &mut Rng) -> AdminReply {
    match rng.pick(&REPLY_KINDS) {
        0x81 => AdminReply::Ok,
        0x82 => {
            let n = if rng.below(200) == 0 { MAX_TEXT_UNITS - rng.below(4) } else { rng.below(64) };
            AdminReply::Text(any_string(rng, n, any_char).into())
        }
        0x83 => AdminReply::Denied(rng.pick(&REASONS)),
        _ => AdminReply::Failed { win32: rng.next() as u32 },
    }
}

#[derive(Clone, Copy, Debug)]
enum Dir {
    Request,
    Reply,
}

impl Dir {
    fn kinds(self) -> &'static [u16] {
        match self {
            Dir::Request => &REQUEST_KINDS,
            Dir::Reply => &REPLY_KINDS,
        }
    }

    fn any_frame(self, rng: &mut Rng) -> Vec<u8> {
        match self {
            Dir::Request => enc_req(&any_request(rng)),
            Dir::Reply => enc_rep(&any_reply(rng)),
        }
    }

    /// The harness body every fuzz layer runs (it panics on a breach), and whether the input decoded.
    fn run(self, input: &[u8]) -> bool {
        match self {
            Dir::Request => {
                fuzz_request(input);
                decode_request(input).is_ok()
            }
            Dir::Reply => {
                fuzz_reply(input);
                decode_reply(input).is_ok()
            }
        }
    }
}

/// A random buffer of 0..1,100 bytes: plain noise, noise under a valid header with an honest len, or under a random len.
fn random_buffer(rng: &mut Rng, dir: Dir) -> Vec<u8> {
    let n = if rng.coin() { rng.below(1_100) } else { rng.below(48) };
    let mut b = rng.bytes(n);
    let flavour = rng.below(3);
    if flavour > 0 && n >= 10 {
        let kind = if rng.below(8) == 0 { rng.pick(&[0, 9, 0x80, 0x85, 1, 0x81]) } else { rng.pick(dir.kinds()) };
        let len = if flavour == 1 { (n - 10) as u16 } else { rng.next() as u16 };
        b[..10].copy_from_slice(&header(kind, len));
    }
    b
}

/// A valid frame changed once: bit flips, a length lie, a cut, a splice of two frames, or another kind.
fn mutation(rng: &mut Rng, dir: Dir) -> Vec<u8> {
    let mut f = dir.any_frame(rng);
    match rng.below(5) {
        0 => {
            for _ in 0..1 + rng.below(4) {
                let at = rng.below(f.len());
                f[at] ^= 1 << rng.below(8);
            }
        }
        1 => {
            let len = u16::from_le_bytes([f[8], f[9]]);
            let d = 1 + rng.below(3) as u16;
            let lie = match rng.below(3) {
                0 => len.wrapping_add(d),
                1 => len.wrapping_sub(d),
                _ => rng.next() as u16,
            };
            f[8..10].copy_from_slice(&lie.to_le_bytes());
            if rng.coin() {
                let fill = rng.next() as u8;
                f.resize(10 + usize::from(lie), fill); // the buffer agrees with the lie
            }
        }
        2 => {
            let at = rng.below(f.len());
            f.truncate(at);
        }
        3 => {
            let g = dir.any_frame(rng);
            let (a, b) = (rng.below(f.len() + 1), rng.below(g.len() + 1));
            f.truncate(a);
            f.extend_from_slice(&g[b..]);
        }
        _ => {
            let kind = rng.pick(dir.kinds());
            f[6..8].copy_from_slice(&kind.to_le_bytes());
        }
    }
    f
}

/// Runs the given number of random buffers, then of mutated frames, through dir's harness body; returns how many
/// decoded.
fn fuzz(dir: Dir, random: usize, mutations: usize, seed: u64) -> usize {
    let mut rng = Rng::new(seed);
    let mut accepted = 0;
    for _ in 0..random {
        accepted += usize::from(dir.run(&random_buffer(&mut rng, dir)));
    }
    for _ in 0..mutations {
        accepted += usize::from(dir.run(&mutation(&mut rng, dir)));
    }
    let total = random + mutations;
    assert!(accepted >= total / 100, "{dir:?}: only {accepted} of {total} inputs decoded: the inputs stay shallow");
    accepted
}

/// 500,000 seeded inputs, half per direction: 300,000 random buffers and 200,000 mutations of valid frames.
#[test]
fn fuzz_smoke() {
    fuzz(Dir::Request, 150_000, 100_000, 0x5EED_0001);
    fuzz(Dir::Reply, 150_000, 100_000, 0x5EED_0002);
}

// ---- the nightly fuzz and the cargo-fuzz seed corpus (TA1b)

/// OLED_FUZZ_SEED as a u64 (decimal, or hex after 0x) when set, else 0x5EED; printed first, so a failing night can be
/// replayed with the seed its log shows.
fn nightly_seed(test: &str) -> u64 {
    let seed = match std::env::var("OLED_FUZZ_SEED") {
        Ok(s) => {
            let t = s.trim();
            let parsed = match t.strip_prefix("0x").or_else(|| t.strip_prefix("0X")) {
                Some(hex) => u64::from_str_radix(hex, 16),
                None => t.parse(),
            };
            parsed.unwrap_or_else(|e| panic!("OLED_FUZZ_SEED={s:?} is not a u64: {e}"))
        }
        Err(_) => 0x5EED,
    };
    println!("{test}: seed {seed:#x} (set OLED_FUZZ_SEED={seed:#x} to replay)");
    seed
}

#[test]
#[ignore = "fuzz: 1,000,000 inputs, nightly (fuzz-nightly.yml runs every ignored fuzz_ test in release)"]
fn fuzz_admin_request_1m() {
    fuzz(Dir::Request, 600_000, 400_000, nightly_seed("fuzz_admin_request_1m"));
}

#[test]
#[ignore = "fuzz: 1,000,000 inputs, nightly (fuzz-nightly.yml runs every ignored fuzz_ test in release)"]
fn fuzz_admin_reply_1m() {
    fuzz(Dir::Reply, 600_000, 400_000, nightly_seed("fuzz_admin_reply_1m"));
}

fn corpus_dir(target: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("fuzz").join("corpus").join(target)
}

/// The seed files of one cargo-fuzz target: its round-trip vectors as "<kind>-<n>.bin", kind in hex and n counting
/// the vectors of that kind.
fn seed_files(frames: Vec<Vec<u8>>) -> Vec<(String, Vec<u8>)> {
    let mut count: BTreeMap<u16, usize> = BTreeMap::new();
    frames
        .into_iter()
        .map(|f| {
            let k = kind_of(&f);
            let n = count.entry(k).or_default();
            let name = format!("{k:02x}-{n}.bin");
            *n += 1;
            (name, f)
        })
        .collect()
}

fn request_seeds() -> Vec<(String, Vec<u8>)> {
    seed_files(request_vectors().iter().map(enc_req).collect())
}

fn reply_seeds() -> Vec<(String, Vec<u8>)> {
    seed_files(reply_vectors().iter().map(enc_rep).collect())
}

/// The .bin files of one corpus folder by name. Files libFuzzer adds during a local run are named by their hash with
/// no extension and are not judged here.
fn corpus_files(target: &str) -> BTreeMap<String, Vec<u8>> {
    let dir = corpus_dir(target);
    let entries = std::fs::read_dir(&dir).unwrap_or_else(|e| panic!("{}: {e} (run write_seed_corpus)", dir.display()));
    entries
        .map(|e| e.unwrap().path())
        .filter(|p| p.extension().is_some_and(|x| x == "bin"))
        .map(|p| (p.file_name().unwrap().to_string_lossy().into_owned(), std::fs::read(&p).unwrap()))
        .collect()
}

/// Every seed file decodes and encodes back to its own bytes; every kind has a seed; the largest frames are there;
/// and every round-trip vector has its file with its current bytes.
#[test]
fn seed_corpus_is_current() {
    let requests = corpus_files("admin_request");
    let mut kinds = Vec::new();
    let (mut keys_64, mut name_256) = (false, false);
    for (name, bytes) in &requests {
        let r = decode_request(bytes).unwrap_or_else(|e| panic!("admin_request/{name} does not decode: {e:?}"));
        assert_eq!(enc_req(&r), *bytes, "admin_request/{name} does not encode back to its own bytes");
        fuzz_request(bytes);
        kinds.push(kind_of(bytes));
        match &r {
            AdminRequest::Terminate { keys, .. } => keys_64 |= keys.as_slice().len() == MAX_KEYS,
            AdminRequest::ServiceControl { name, .. } => {
                name_256 |= name.as_str().encode_utf16().count() == MAX_SERVICE_NAME
            }
            _ => {}
        }
    }
    for k in REQUEST_KINDS {
        assert!(kinds.contains(&k), "admin_request: no seed of kind {k:#04x}");
    }
    assert!(keys_64, "admin_request: no seed of the 64-key Terminate");
    assert!(name_256, "admin_request: no seed of the 256-unit ServiceControl");

    let replies = corpus_files("admin_reply");
    let mut kinds = Vec::new();
    let mut text_max = false;
    for (name, bytes) in &replies {
        let r = decode_reply(bytes).unwrap_or_else(|e| panic!("admin_reply/{name} does not decode: {e:?}"));
        assert_eq!(enc_rep(&r), *bytes, "admin_reply/{name} does not encode back to its own bytes");
        fuzz_reply(bytes);
        kinds.push(kind_of(bytes));
        if let AdminReply::Text(t) = &r {
            text_max |= t.encode_utf16().count() == MAX_TEXT_UNITS;
        }
    }
    for k in REPLY_KINDS {
        assert!(kinds.contains(&k), "admin_reply: no seed of kind {k:#04x}");
    }
    assert!(text_max, "admin_reply: no seed of the 16,384-unit Text");

    for (target, files, seeds) in
        [("admin_request", &requests, request_seeds()), ("admin_reply", &replies, reply_seeds())]
    {
        for (name, bytes) in seeds {
            assert!(files.get(&name) == Some(&bytes), "{target}/{name} is missing or stale: run write_seed_corpus");
        }
    }
}

/// Writes the round-trip vectors into the two corpus folders. Run by hand when a vector is added; a file that exists
/// with other bytes is never overwritten (the test fails and names it).
#[test]
#[ignore = "writes crates/oled-trust/fuzz/corpus; run by hand when a vector is added"]
fn write_seed_corpus() {
    for (target, seeds) in [("admin_request", request_seeds()), ("admin_reply", reply_seeds())] {
        let dir = corpus_dir(target);
        std::fs::create_dir_all(&dir).unwrap();
        for (name, bytes) in seeds {
            let path = dir.join(&name);
            match std::fs::read(&path) {
                Ok(old) => assert!(old == bytes, "{target}/{name} exists with other bytes: not overwritten"),
                Err(_) => std::fs::write(&path, &bytes).unwrap(),
            }
        }
    }
}
