//! Typed frames between our own processes (SPEC-platform 5.4.3-5.4.5, SPEC-files 5.5.1). Pure: no OS calls, no unsafe.
//!
//! The framing (encode, decode, the Hello and the stream Assembler) and split_by_bytes are here, the little-endian
//! primitives in wire.rs, and the files protocols with their codecs and WidePath in files.rs.
// Decodes bytes from another process: no unwrap, no expect, no indexing.
#![deny(clippy::unwrap_used, clippy::expect_used, clippy::indexing_slicing)]

// P1-tasks-engine fills frame/admin.rs (FZ2 creates it with its doc line only)
pub mod admin;
// L-F
pub mod files;
// P3-tasks-app fills frame/tasks.rs (TF1; FZ2 creates it with its doc line only)
pub mod tasks;
pub mod wire;

/// Wire layout of every frame of the files protocols (ops, shell, instance): len: u32 LE (bytes after this field
/// = 2 + payload), kind: u16 LE, payload. The admin protocol of SPEC-platform 5.4.3 (10-byte header magic u32,
/// version u16, kind u16, len u16) is not this layout: frame/admin.rs keeps its own private Header (P1-tasks-engine 2.5).
pub const HEADER_LEN: usize = 6;
/// Kind 0 is the Hello frame of every protocol; no message enum uses it.
pub const HELLO_KIND: u16 = 0x0000;
/// Senders split list frames by bytes as well as by count: one frame's payload stays at most max_payload - SPLIT_MARGIN.
pub const SPLIT_MARGIN: u32 = 4096;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Proto {
    pub magic: u32,
    pub version: u16,
    pub max_payload: u32,
}

/// First frame on every pipe, both ways. payload = magic u32, proto u16, build [u8; 16] (22 bytes).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Hello {
    pub magic: u32,
    pub proto: u16,
    pub build: [u8; 16],
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum FrameError {
    Short,                          // not enough bytes for a whole frame yet
    TooLong { len: u32, max: u32 }, // the stream is poisoned: close it; from encode: the message does not fit
    UnknownKind(u16),
    BadMagic(u32),
    BadProto { got: u16, want: u16 },
    Trailing { extra: u32 }, // payload longer than the message it decoded to
    BadUtf8 { field: &'static str },
    Cap { field: &'static str, len: u32, max: u32 },
    BadValue { field: &'static str, value: u32 }, // a bool not 0/1, an enum tag out of range
    Invalid { field: &'static str },              // a semantic check failed (a path, a name)
}

pub trait Codec: Sized {
    const PROTO: Proto;
    fn kind(&self) -> u16; // the pinned discriminant
    fn put(&self, w: &mut wire::Writer);
    fn get(kind: u16, r: &mut wire::Reader<'_>) -> Result<Self, FrameError>;
}

/// The encoded size in bytes of one element of a list field (WidePath, ItemRef, UndoEntry, Change, ModernItem,
/// ShellNewType, Box<str>), so a sender can split before encoding.
pub trait WireLen {
    fn wire_len(&self) -> usize;
}

/// Appends one frame; TooLong over PROTO.max_payload, out unchanged.
pub fn encode<C: Codec>(msg: &C, out: &mut Vec<u8>) -> Result<(), FrameError> {
    let start = out.len();
    let max = C::PROTO.max_payload;
    let mut w = wire::Writer::from_vec(std::mem::take(out));
    w.u32(0); // len: written below, once the payload is known
    w.u16(msg.kind());
    msg.put(&mut w);
    let overflowed = w.overflowed();
    *out = w.into_inner();
    let payload = out.len().saturating_sub(start + HEADER_LEN);
    let len = u32::try_from(payload).ok().filter(|&n| n <= max && !overflowed).and_then(|n| n.checked_add(2));
    match (len, out.get_mut(start..start + 4)) {
        (Some(len), Some(head)) => {
            head.copy_from_slice(&len.to_le_bytes());
            Ok(())
        }
        _ => {
            out.truncate(start);
            // A length prefix that could not hold its field (Writer::wide past u16::MAX units) never fits either.
            let len = if overflowed { u32::MAX } else { u32::try_from(payload).unwrap_or(u32::MAX) };
            Err(FrameError::TooLong { len, max })
        }
    }
}

/// One frame from buf's start; usize = bytes used.
///
/// Short only while buf does not hold the whole frame yet. A whole frame whose payload ends before its message is
/// Invalid { field: "payload" } (not Short, which a stream reader would wait on), one whose payload goes on after
/// its message is Trailing, and kind 0 (the Hello) is UnknownKind(0).
pub fn decode<C: Codec>(buf: &[u8]) -> Result<(C, usize), FrameError> {
    let (kind, mut r, used) = open_frame(buf, C::PROTO.max_payload)?;
    if kind == HELLO_KIND {
        return Err(FrameError::UnknownKind(kind));
    }
    let msg = C::get(kind, &mut r).map_err(whole)?;
    r.finish()?;
    Ok((msg, used))
}

/// Payload bytes of a Hello: magic u32, proto u16, build [u8; 16].
const HELLO_PAYLOAD: u32 = 22;

pub fn encode_hello(p: &Proto, build: [u8; 16], out: &mut Vec<u8>) {
    let mut w = wire::Writer::from_vec(std::mem::take(out));
    w.u32(2 + HELLO_PAYLOAD);
    w.u16(HELLO_KIND);
    w.u32(p.magic);
    w.u16(p.version);
    w.fixed(&build);
    *out = w.into_inner();
}

/// BadMagic, BadProto; build is not compared.
pub fn decode_hello(p: &Proto, buf: &[u8]) -> Result<(Hello, usize), FrameError> {
    let (kind, mut r, used) = open_frame(buf, p.max_payload)?;
    if kind != HELLO_KIND {
        return Err(FrameError::UnknownKind(kind));
    }
    // Magic and version first, so a peer of another protocol or version is named as such whatever its Hello holds.
    let magic = r.u32().map_err(whole)?;
    if magic != p.magic {
        return Err(FrameError::BadMagic(magic));
    }
    let proto = r.u16().map_err(whole)?;
    if proto != p.version {
        return Err(FrameError::BadProto { got: proto, want: p.version });
    }
    let build = r.fixed::<16>().map_err(whole)?;
    r.finish()?;
    Ok((Hello { magic, proto, build }, used))
}

/// The kind, a reader over the payload and the whole frame's length, once buf holds the whole frame.
fn open_frame(buf: &[u8], max_payload: u32) -> Result<(u16, wire::Reader<'_>, usize), FrameError> {
    let total = frame_len(buf, max_payload)?.ok_or(FrameError::Short)?;
    let frame = buf.get(..total).ok_or(FrameError::Short)?;
    let mut r = wire::Reader::new(frame.get(4..).unwrap_or_default());
    let kind = r.u16()?;
    Ok((kind, r, total))
}

/// The whole length (header included) of the frame at buf's start; None until its len field is there. A len field
/// that cannot belong to a frame of this protocol is an error before any payload is read: TooLong over max_payload,
/// Invalid { field: "len" } when it does not even cover the kind.
fn frame_len(buf: &[u8], max_payload: u32) -> Result<Option<usize>, FrameError> {
    let Some(head) = buf.first_chunk::<4>() else {
        return Ok(None);
    };
    let len = u32::from_le_bytes(*head);
    let payload = len.checked_sub(2).ok_or(FrameError::Invalid { field: "len" })?;
    if payload > max_payload {
        return Err(FrameError::TooLong { len: payload, max: max_payload });
    }
    Ok(Some(4 + len as usize))
}

/// Inside a whole frame, running out of bytes means the payload is shorter than its message.
fn whole(e: FrameError) -> FrameError {
    match e {
        FrameError::Short => FrameError::Invalid { field: "payload" },
        e => e,
    }
}

/// Consecutive slices of items such that each slice has 1..=max_count elements and fixed + 4 + the sum of their
/// wire_len() is at most p.max_payload - SPLIT_MARGIN. Empty input gives no slice. Every single element of the
/// files protocols fits alone (the largest, an UndoEntry with three 32,767-unit paths, is 196,626 bytes).
///
/// An element that does not fit even alone still gets a slice of its own (encode then answers TooLong), so nothing
/// is dropped; a max_count of 0 counts as 1.
pub fn split_by_bytes<'a, T: WireLen>(items: &'a [T], fixed: usize, p: &Proto, max_count: u32) -> Vec<&'a [T]> {
    let limit = p.max_payload.saturating_sub(SPLIT_MARGIN) as usize;
    let max_count = max_count.max(1) as usize;
    let base = fixed.saturating_add(4);
    let mut out = Vec::new();
    let (mut start, mut bytes, mut count) = (0usize, base, 0usize);
    for (i, item) in items.iter().enumerate() {
        let len = item.wire_len();
        if count > 0 && (count == max_count || bytes.saturating_add(len) > limit) {
            out.extend(items.get(start..i));
            (start, bytes, count) = (i, base, 0);
        }
        bytes = bytes.saturating_add(len);
        count += 1;
    }
    if count > 0 {
        out.extend(items.get(start..));
    }
    out
}

/// Cuts a byte stream into whole frames. The first len field that cannot belong to a frame (TooLong, or one that does
/// not cover the kind) poisons the stream: every later call returns that error and pushed bytes are dropped.
pub struct Assembler {
    buf: Vec<u8>,
    /// Bytes at buf's start already handed out; dropped at the next push, so each byte moves at most once.
    at: usize,
    max_payload: u32,
    poisoned: Option<FrameError>,
}

impl Assembler {
    pub fn new(max_payload: u32) -> Self {
        Assembler { buf: Vec::new(), at: 0, max_payload, poisoned: None }
    }

    pub fn push(&mut self, bytes: &[u8]) {
        if self.poisoned.is_some() {
            return;
        }
        if self.at > 0 {
            self.buf.drain(..self.at.min(self.buf.len()));
            self.at = 0;
        }
        self.buf.extend_from_slice(bytes);
    }

    /// One whole frame (header included); after TooLong every call errs.
    pub fn next_frame(&mut self) -> Result<Option<Vec<u8>>, FrameError> {
        if let Some(e) = &self.poisoned {
            return Err(e.clone());
        }
        let rest = self.buf.get(self.at..).unwrap_or_default();
        let frame = match frame_len(rest, self.max_payload) {
            Ok(Some(total)) => rest.get(..total).map(<[u8]>::to_vec),
            Ok(None) => None,
            Err(e) => {
                self.poisoned = Some(e.clone());
                self.buf = Vec::new();
                self.at = 0;
                return Err(e);
            }
        };
        if let Some(f) = &frame {
            self.at += f.len();
            if self.at >= self.buf.len() {
                self.buf.clear();
                self.at = 0;
            }
        }
        Ok(frame)
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::indexing_slicing)]

    use super::files::{WidePath, OPS};
    use super::*;

    /// An element of a known size.
    struct Sized(usize);
    impl WireLen for Sized {
        fn wire_len(&self) -> usize {
            self.0
        }
    }

    fn lens(slices: &[&[Sized]]) -> Vec<usize> {
        slices.iter().map(|s| s.len()).collect()
    }

    fn check_caps<T: WireLen>(items: &[T], slices: &[&[T]], fixed: usize, p: &Proto, max_count: u32) {
        let limit = (p.max_payload - SPLIT_MARGIN) as usize;
        let mut total = 0;
        for s in slices {
            assert!(!s.is_empty() && s.len() <= max_count as usize, "count cap");
            let bytes: usize = fixed + 4 + s.iter().map(WireLen::wire_len).sum::<usize>();
            assert!(bytes <= limit || s.len() == 1, "byte cap: {bytes} > {limit}");
            total += s.len();
        }
        assert_eq!(total, items.len(), "the slices cover the input");
        // Consecutive, in order: each slice starts where the previous one ended.
        let mut at = items.as_ptr();
        for s in slices {
            assert_eq!(s.as_ptr(), at);
            at = s.as_ptr_range().end;
        }
    }

    #[test]
    fn split_by_bytes_table() {
        let p = OPS;
        let limit = (p.max_payload - SPLIT_MARGIN) as usize;
        let none: [Sized; 0] = [];
        assert!(split_by_bytes(&none, 10, &p, 2_048).is_empty(), "empty input gives no slice");
        let one = [Sized(100)];
        assert_eq!(lens(&split_by_bytes(&one, 10, &p, 2_048)), [1]);
        let small: Vec<Sized> = (0..5).map(|_| Sized(10)).collect();
        assert_eq!(lens(&split_by_bytes(&small, 10, &p, 2)), [2, 2, 1], "count cap");
        assert_eq!(lens(&split_by_bytes(&small, 10, &p, 0)), [1, 1, 1, 1, 1], "a cap of 0 counts as 1");
        // Byte cap: fixed 6 + 4 + two halves fit exactly; a third element starts a new slice.
        let half = (limit - 10) / 2;
        let exact = [Sized(half), Sized(half), Sized(1)];
        assert_eq!(lens(&split_by_bytes(&exact, 6, &p, 2_048)), [2, 1]);
        let over = [Sized(half), Sized(half + 1), Sized(1)];
        assert_eq!(lens(&split_by_bytes(&over, 6, &p, 2_048)), [1, 2]);
        let huge = [Sized(1), Sized(limit * 2), Sized(1)];
        assert_eq!(lens(&split_by_bytes(&huge, 6, &p, 2_048)), [1, 1, 1], "an oversize element stands alone");
        // A mix of deep and short paths.
        let deep: Vec<u16> = "C:\\".encode_utf16().chain(std::iter::repeat_n(u16::from(b'a'), 32_000)).collect();
        let deep = WidePath::new(&deep).unwrap();
        let short = WidePath::parse("C:\\a\\b.txt").unwrap();
        let mix: Vec<WidePath> = (0..200).map(|i| if i % 3 == 0 { deep.clone() } else { short.clone() }).collect();
        let slices = split_by_bytes(&mix, 8, &p, 64);
        assert!(slices.len() > 1);
        check_caps(&mix, &slices, 8, &p, 64);
        let back: Vec<WidePath> = slices.iter().flat_map(|s| s.iter().cloned()).collect();
        assert_eq!(back, mix, "the concatenation equals the input");
    }

    /// A small protocol for the framing tests (the files protocols' codecs are tested in tests/frame_files.rs).
    #[derive(Debug, PartialEq, Eq)]
    enum Toy {
        Ping(u32),
        Blob(Vec<u8>),
    }
    const TOY: Proto = Proto { magic: u32::from_le_bytes(*b"TOY1"), version: 3, max_payload: 64 };
    impl Codec for Toy {
        const PROTO: Proto = TOY;
        fn kind(&self) -> u16 {
            match self {
                Toy::Ping(_) => 1,
                Toy::Blob(_) => 2,
            }
        }
        fn put(&self, w: &mut wire::Writer) {
            match self {
                Toy::Ping(n) => w.u32(*n),
                Toy::Blob(b) => w.bytes(b),
            }
        }
        fn get(kind: u16, r: &mut wire::Reader<'_>) -> Result<Self, FrameError> {
            match kind {
                1 => Ok(Toy::Ping(r.u32()?)),
                2 => Ok(Toy::Blob(r.bytes("blob", 1_000)?.to_vec())),
                k => Err(FrameError::UnknownKind(k)),
            }
        }
    }

    /// Units on the wire through Writer::wide, under a payload limit that does not stop 65,536 of them by bytes.
    #[derive(Debug)]
    struct Wide(Vec<u16>);
    impl Codec for Wide {
        const PROTO: Proto = Proto { magic: 0, version: 1, max_payload: 1 << 20 };
        fn kind(&self) -> u16 {
            1
        }
        fn put(&self, w: &mut wire::Writer) {
            w.wide(&self.0);
        }
        fn get(_: u16, r: &mut wire::Reader<'_>) -> Result<Self, FrameError> {
            Ok(Wide(r.wide("wide", u16::MAX)?))
        }
    }

    fn frame(msg: &Toy) -> Vec<u8> {
        let mut v = Vec::new();
        encode(msg, &mut v).unwrap();
        v
    }

    #[test]
    fn encode_then_decode() {
        let f = frame(&Toy::Ping(7));
        assert_eq!(f, [6, 0, 0, 0, 1, 0, 7, 0, 0, 0], "len = 2 + payload, kind, payload");
        let mut tail = f.clone();
        tail.extend_from_slice(&[9, 9]);
        assert_eq!(decode::<Toy>(&tail), Ok((Toy::Ping(7), f.len())), "the whole length, nothing after it");
        let b = frame(&Toy::Blob(vec![1, 2, 3]));
        assert_eq!(decode::<Toy>(&b), Ok((Toy::Blob(vec![1, 2, 3]), b.len())));
        let mut out = vec![0xAA];
        encode(&Toy::Ping(1), &mut out).unwrap();
        assert_eq!(out.len(), 1 + 10, "encode appends");
        assert_eq!(decode::<Toy>(&out[1..]).unwrap().0, Toy::Ping(1));
    }

    #[test]
    fn encode_over_max_payload_is_too_long_and_leaves_out_unchanged() {
        let mut out = vec![1, 2, 3];
        assert_eq!(encode(&Toy::Blob(vec![0; 61]), &mut out), Err(FrameError::TooLong { len: 65, max: 64 }));
        assert_eq!(out, [1, 2, 3]);
        encode(&Toy::Blob(vec![0; 60]), &mut out).unwrap();
        assert_eq!(out.len(), 3 + HEADER_LEN + 64, "max_payload itself fits");
        let mut out = vec![5];
        assert_eq!(encode(&Wide(vec![1; 65_536]), &mut out), Err(FrameError::TooLong { len: u32::MAX, max: 1 << 20 }));
        assert_eq!(out, [5], "a field whose length prefix overflows is never sent cut short");
        encode(&Wide(vec![1; 65_535]), &mut out).unwrap();
        assert_eq!(decode::<Wide>(&out[1..]).unwrap().0 .0.len(), 65_535);
    }

    #[test]
    fn decode_refuses_a_long_len_before_reading_the_payload() {
        let head = [67u8, 0, 0, 0, 1, 0]; // len = max_payload + 3; the payload never comes
        assert_eq!(decode::<Toy>(&head), Err(FrameError::TooLong { len: 65, max: 64 }));
        assert_eq!(decode::<Toy>(&head[..4]), Err(FrameError::TooLong { len: 65, max: 64 }), "the len field decides");
        assert_eq!(decode::<Toy>(&[66u8, 0, 0, 0, 2, 0]), Err(FrameError::Short), "max_payload + 2: wait for it");
        assert_eq!(decode::<Toy>(&[1, 0, 0, 0, 0]), Err(FrameError::Invalid { field: "len" }), "len covers the kind");
    }

    #[test]
    fn decode_short_unknown_and_trailing() {
        let f = frame(&Toy::Ping(7));
        for cut in 0..f.len() {
            assert_eq!(decode::<Toy>(&f[..cut]), Err(FrameError::Short), "cut at {cut}");
        }
        assert_eq!(decode::<Toy>(&[6, 0, 0, 0, 0, 0, 7, 0, 0, 0]), Err(FrameError::UnknownKind(0)), "kind 0 is Hello");
        assert_eq!(decode::<Toy>(&[6, 0, 0, 0, 9, 0, 7, 0, 0, 0]), Err(FrameError::UnknownKind(9)));
        assert_eq!(decode::<Toy>(&[7, 0, 0, 0, 1, 0, 7, 0, 0, 0, 5]), Err(FrameError::Trailing { extra: 1 }));
        assert_eq!(decode::<Toy>(&[5, 0, 0, 0, 1, 0, 7, 0, 0]), Err(FrameError::Invalid { field: "payload" }));
    }

    #[test]
    fn assembler_split_at_every_byte_yields_one_frame() {
        let f = frame(&Toy::Blob((0..40).collect()));
        for cut in 0..=f.len() {
            let mut a = Assembler::new(TOY.max_payload);
            a.push(&f[..cut]);
            if cut < f.len() {
                assert_eq!(a.next_frame(), Ok(None), "cut at {cut}");
            }
            a.push(&f[cut..]);
            let got: Vec<Vec<u8>> = std::iter::from_fn(|| a.next_frame().unwrap()).collect();
            assert_eq!(got, std::slice::from_ref(&f), "cut at {cut}");
        }
        let mut a = Assembler::new(TOY.max_payload);
        let mut got = Vec::new();
        for b in &f {
            a.push(std::slice::from_ref(b));
            while let Some(x) = a.next_frame().unwrap() {
                got.push(x);
            }
        }
        assert_eq!(got, [f], "byte by byte");
    }

    #[test]
    fn assembler_two_frames_in_one_push() {
        let (p, b) = (frame(&Toy::Ping(1)), frame(&Toy::Blob(vec![2; 9])));
        let mut both = p.clone();
        both.extend_from_slice(&b);
        both.extend_from_slice(&p[..3]); // and the start of a third
        let mut a = Assembler::new(TOY.max_payload);
        a.push(&both);
        assert_eq!(a.next_frame(), Ok(Some(p.clone())));
        assert_eq!(a.next_frame(), Ok(Some(b)));
        assert_eq!(a.next_frame(), Ok(None));
        a.push(&p[3..]);
        assert_eq!(a.next_frame(), Ok(Some(p.clone())));
        assert_eq!(a.next_frame(), Ok(None));
    }

    #[test]
    fn assembler_is_poisoned_after_too_long() {
        let good = frame(&Toy::Ping(1));
        let mut a = Assembler::new(TOY.max_payload);
        a.push(&good);
        a.push(&[67, 0, 0, 0]);
        assert_eq!(a.next_frame(), Ok(Some(good.clone())), "frames before the bad header still come out");
        let e = FrameError::TooLong { len: 65, max: 64 };
        assert_eq!(a.next_frame(), Err(e.clone()));
        a.push(&good);
        assert_eq!(a.next_frame(), Err(e.clone()), "every later call errs");
        assert_eq!(a.next_frame(), Err(e));
        let mut a = Assembler::new(TOY.max_payload);
        a.push(&[0, 0, 0, 0, 0, 0]);
        assert_eq!(a.next_frame(), Err(FrameError::Invalid { field: "len" }));
        a.push(&good);
        assert_eq!(a.next_frame(), Err(FrameError::Invalid { field: "len" }));
    }

    #[test]
    fn hello_mismatch() {
        let build = *b"1.2.3\0\0\0\0\0\0\0\0\0\0\0";
        let mut out = Vec::new();
        encode_hello(&TOY, build, &mut out);
        assert_eq!(out.len(), HEADER_LEN + 22);
        assert_eq!(out[..6], [24, 0, 0, 0, 0, 0], "len 2 + 22, kind 0");
        assert_eq!(decode_hello(&TOY, &out), Ok((Hello { magic: TOY.magic, proto: 3, build }, 28)));
        let magic = Proto { magic: u32::from_le_bytes(*b"TOY2"), ..TOY };
        assert_eq!(decode_hello(&magic, &out), Err(FrameError::BadMagic(TOY.magic)));
        let version = Proto { version: 4, ..TOY };
        assert_eq!(decode_hello(&version, &out), Err(FrameError::BadProto { got: 3, want: 4 }));
        let other = *b"9.9.9-other\0\0\0\0\0";
        let mut theirs = Vec::new();
        encode_hello(&TOY, other, &mut theirs);
        assert_eq!(decode_hello(&TOY, &theirs).map(|(h, _)| h.build), Ok(other), "another build id is not compared");
        assert_eq!(decode_hello(&TOY, &frame(&Toy::Ping(1))), Err(FrameError::UnknownKind(1)));
        for cut in 0..out.len() {
            assert_eq!(decode_hello(&TOY, &out[..cut]), Err(FrameError::Short), "cut at {cut}");
        }
        let mut long = out.clone();
        long[0] = 25;
        long.push(0);
        assert_eq!(decode_hello(&TOY, &long), Err(FrameError::Trailing { extra: 1 }));
        assert_eq!(decode::<Toy>(&out), Err(FrameError::UnknownKind(0)), "no message decoder takes a Hello");
    }
}
