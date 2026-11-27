//! The agent's records: tags are fixed forever (F-P8). Pure, no unsafe.
//!
//! What the elevated dock and its medium agent (`dock.exe --agent`, `agent.rs`) say to each other: `Request` frames go
//! from the dock to the agent (decoded at medium, at most `REQ_MAX` bytes), `Reply` frames from the agent to the dock
//! (decoded ELEVATED, at most `REC_MAX` bytes: the dock's only decoder of agent bytes besides the two it already had for
//! its children's output, the apps arena and the icon frames, which the agent's `Chunk`s carry).
//!
//! Encoding, hand-written (no serde): a record is its tag (u8), then its fields in declaration order. u16, u32, u64:
//! little-endian. bool: u8 0 or 1 (anything else is `Bool`). Option<T>: u8 0 (None) or 1 followed by T (anything else is
//! `Bool`). String: u16 byte length, then that many UTF-8 bytes; a field capped in UTF-16 units is refused (`Cap`) from its
//! byte length alone when that is above 3 x the cap, and otherwise checked in units after decoding. Vec<String> (the icon
//! ids): u16 count, then each string. Vec<u8> (chunk bytes, icon pixels): u32 length, then the bytes. Every count and length
//! is checked against its field's cap and against the bytes left in the frame before anything is allocated; bytes after
//! the last field are `Trailing`. An encoder that would pass a cap returns `Cap` and gives no bytes, so every frame
//! the encoders make decodes to the same value, and every frame the decoders accept encodes to the same bytes.
#![forbid(unsafe_code)]
#![deny(clippy::unwrap_used, clippy::expect_used, clippy::indexing_slicing)]

pub const AGENT_PROTO: u16 = 1;
pub const REQ_MAX: usize = 2 * 1024 * 1024 + 1024; // dock -> agent (decoded at medium); fits pipe::FRAME_MAX
pub const REC_MAX: usize = 64 * 1024; // agent -> dock (decoded elevated): one reply frame, SPEC-platform 5.4.4's cap
pub const CHUNK_MAX: usize = 60 * 1024;
pub const ICONS_IDS_MAX: usize = 2000;
pub const ICON_ID_MAX_BYTES: usize = 1024; // = shell_thread.rs ICON_IDS_MAX, ICON_ID_MAX_BYTES (1624-1625)
pub const EXE_ICON_PX: [u16; 4] = [16, 24, 32, 64]; // ExeInfo icon sizes; 64 x 64 x 4 = 16 KiB at most
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Request {
    Hello { proto: u16, pid: u32 },                             /*1*/
    ListApps { id: u32 },                                       /*2*/
    Icons { id: u32, px: u16 /* 8..=256 */, ids: Vec<String> }, /*3*/
    StartLinks { id: u32 },                                     /*4*/
    ExeInfo { id: u32, path: String /* <= 1024 UTF-16 units */, px: u16 /* one of EXE_ICON_PX */ }, /*5*/
    UserJournal { id: u32 },                                    /*6*/
    Cancel { id: u32 },                                         /*7*/
    Bye,                                                        /*8*/
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Reply {
    Hello { proto: u16, pid: u32 },                                   /*1*/
    Chunk { id: u32, last: bool, bytes: Vec<u8> /* <= CHUNK_MAX */ }, /*2*/
    Link(LinkRec),                                                    /*3*/
    ExeInfo(ExeInfoRec),                                              /*4*/
    Journal(JournalRec),                                              /*5*/
    End { id: u32, status: u16 },                                     /*6*/
}
//   tags 7 EditText, 8 RecentItems, 9 DropRequest are reserved (SPEC-platform 5.4.4); decoding them is FrameError::Reserved until their consumer
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LinkRec {
    pub id: u32,
    pub rel: String,    /* <= 260 units */
    pub target: String, /* <= 1024 */
    pub name: String,   /* <= 260 */
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ExeInfoRec {
    pub id: u32,
    pub description: Option<String>,
    pub product: Option<String>,
    pub company: Option<String>,      /* each <= 260 units */
    pub icon: Option<(u16, Vec<u8>)>, /* px one of EXE_ICON_PX, BGRA premultiplied, len == px*px*4 */
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct JournalRec {
    pub id: u32,
    pub seq: u64,
    pub kind: String,   /* <= 64 bytes */
    pub target: String, /* <= 260 units */
    pub ts: u64,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FrameError {
    Empty,
    Tag(u8),
    Reserved(u8),
    Short,
    Bool(u8),
    Cap(&'static str),
    Utf8,
    Trailing,
}
/// The caps of the string fields, in UTF-16 units (`LinkRec`, `ExeInfoRec`, `JournalRec`, `Request::ExeInfo`).
const NAME_UNITS: usize = 260;
const PATH_UNITS: usize = 1024;
/// `JournalRec::kind`, in bytes.
const KIND_MAX_BYTES: usize = 64;
/// The sizes an `Icons` request may ask for (the dock's `ICON_PX_RANGE`).
const ICONS_PX: std::ops::RangeInclusive<u16> = 8..=256;
/// Bytes of the largest `ExeInfo` icon: 64 x 64 BGRA.
const EXE_ICON_MAX_BYTES: usize = 64 * 64 * 4;

/// The bytes of one icon of `px` x `px`, when `px` is one of `EXE_ICON_PX`.
fn exe_icon_len(px: u16) -> Option<usize> {
    EXE_ICON_PX.contains(&px).then(|| usize::from(px) * usize::from(px) * 4)
}

/// The number of UTF-16 units of `s`.
fn units(s: &str) -> usize {
    s.encode_utf16().count()
}

/// A frame being written.
struct Out(Vec<u8>);

impl Out {
    fn tag(t: u8) -> Out {
        Out(vec![t])
    }
    fn u16(&mut self, v: u16) {
        self.0.extend_from_slice(&v.to_le_bytes());
    }
    fn u32(&mut self, v: u32) {
        self.0.extend_from_slice(&v.to_le_bytes());
    }
    fn u64(&mut self, v: u64) {
        self.0.extend_from_slice(&v.to_le_bytes());
    }
    fn bool(&mut self, v: bool) {
        self.0.push(u8::from(v));
    }
    /// A string of at most `cap` UTF-16 units.
    fn text(&mut self, s: &str, cap: usize, what: &'static str) -> Result<(), FrameError> {
        if units(s) > cap {
            return Err(FrameError::Cap(what));
        }
        self.raw_text(s, what)
    }
    /// A string of at most `cap` bytes.
    fn text_bytes(&mut self, s: &str, cap: usize, what: &'static str) -> Result<(), FrameError> {
        if s.len() > cap {
            return Err(FrameError::Cap(what));
        }
        self.raw_text(s, what)
    }
    fn raw_text(&mut self, s: &str, what: &'static str) -> Result<(), FrameError> {
        let len = u16::try_from(s.len()).map_err(|_| FrameError::Cap(what))?;
        self.u16(len);
        self.0.extend_from_slice(s.as_bytes());
        Ok(())
    }
    fn opt_text(&mut self, s: Option<&str>, cap: usize, what: &'static str) -> Result<(), FrameError> {
        self.bool(s.is_some());
        s.map_or(Ok(()), |s| self.text(s, cap, what))
    }
    /// A byte vector of at most `cap` bytes.
    fn bytes(&mut self, b: &[u8], cap: usize, what: &'static str) -> Result<(), FrameError> {
        let len = u32::try_from(b.len()).ok().filter(|&l| l as usize <= cap).ok_or(FrameError::Cap(what))?;
        self.u32(len);
        self.0.extend_from_slice(b);
        Ok(())
    }
}

/// A frame being read: what is left of it.
struct In<'a>(&'a [u8]);

impl<'a> In<'a> {
    fn take(&mut self, n: usize) -> Result<&'a [u8], FrameError> {
        if n > self.0.len() {
            return Err(FrameError::Short);
        }
        let (head, rest) = self.0.split_at(n);
        self.0 = rest;
        Ok(head)
    }
    fn array<const N: usize>(&mut self) -> Result<[u8; N], FrameError> {
        <[u8; N]>::try_from(self.take(N)?).map_err(|_| FrameError::Short)
    }
    fn u8(&mut self) -> Result<u8, FrameError> {
        Ok(u8::from_le_bytes(self.array()?))
    }
    fn u16(&mut self) -> Result<u16, FrameError> {
        Ok(u16::from_le_bytes(self.array()?))
    }
    fn u32(&mut self) -> Result<u32, FrameError> {
        Ok(u32::from_le_bytes(self.array()?))
    }
    fn u64(&mut self) -> Result<u64, FrameError> {
        Ok(u64::from_le_bytes(self.array()?))
    }
    /// A bool, or the presence byte of an Option.
    fn bool(&mut self) -> Result<bool, FrameError> {
        match self.u8()? {
            0 => Ok(false),
            1 => Ok(true),
            b => Err(FrameError::Bool(b)),
        }
    }
    /// A string of at most `cap` UTF-16 units: a byte length above 3 x `cap` is refused before anything is read.
    fn text(&mut self, cap: usize, what: &'static str) -> Result<String, FrameError> {
        let s = self.raw_text(cap.saturating_mul(3), what)?;
        if units(&s) > cap {
            return Err(FrameError::Cap(what));
        }
        Ok(s)
    }
    /// A string of at most `cap` bytes.
    fn text_bytes(&mut self, cap: usize, what: &'static str) -> Result<String, FrameError> {
        self.raw_text(cap, what)
    }
    fn raw_text(&mut self, max_bytes: usize, what: &'static str) -> Result<String, FrameError> {
        let len = usize::from(self.u16()?);
        if len > max_bytes {
            return Err(FrameError::Cap(what));
        }
        let bytes = self.take(len)?;
        std::str::from_utf8(bytes).map(str::to_string).map_err(|_| FrameError::Utf8)
    }
    fn opt_text(&mut self, cap: usize, what: &'static str) -> Result<Option<String>, FrameError> {
        if self.bool()? {
            self.text(cap, what).map(Some)
        } else {
            Ok(None)
        }
    }
    /// A byte vector of at most `cap` bytes.
    fn bytes(&mut self, cap: usize, what: &'static str) -> Result<Vec<u8>, FrameError> {
        let len = usize::try_from(self.u32()?).unwrap_or(usize::MAX);
        if len > cap {
            return Err(FrameError::Cap(what));
        }
        Ok(self.take(len)?.to_vec())
    }
    /// The frame is over: anything left is `Trailing`.
    fn end(&self) -> Result<(), FrameError> {
        if self.0.is_empty() {
            Ok(())
        } else {
            Err(FrameError::Trailing)
        }
    }
}

pub fn encode_request(r: &Request) -> Result<Vec<u8>, FrameError> {
    let out = match r {
        Request::Hello { proto, pid } => {
            let mut o = Out::tag(1);
            o.u16(*proto);
            o.u32(*pid);
            o
        }
        Request::ListApps { id } => id_only(2, *id),
        Request::Icons { id, px, ids } => {
            if !ICONS_PX.contains(px) {
                return Err(FrameError::Cap("icons.px"));
            }
            let count = u16::try_from(ids.len()).ok().filter(|&n| usize::from(n) <= ICONS_IDS_MAX);
            let count = count.ok_or(FrameError::Cap("icons.ids"))?;
            let mut o = Out::tag(3);
            o.u32(*id);
            o.u16(*px);
            o.u16(count);
            for s in ids {
                o.text_bytes(s, ICON_ID_MAX_BYTES, "icons.id")?;
            }
            o
        }
        Request::StartLinks { id } => id_only(4, *id),
        Request::ExeInfo { id, path, px } => {
            if exe_icon_len(*px).is_none() {
                return Err(FrameError::Cap("exe_info.px"));
            }
            let mut o = Out::tag(5);
            o.u32(*id);
            o.text(path, PATH_UNITS, "exe_info.path")?;
            o.u16(*px);
            o
        }
        Request::UserJournal { id } => id_only(6, *id),
        Request::Cancel { id } => id_only(7, *id),
        Request::Bye => Out::tag(8),
    };
    Ok(out.0)
}

/// A record whose only field is its request id.
fn id_only(tag: u8, id: u32) -> Out {
    let mut o = Out::tag(tag);
    o.u32(id);
    o
}

pub fn decode_request(b: &[u8]) -> Result<Request, FrameError> {
    if b.len() > REQ_MAX {
        return Err(FrameError::Cap("request"));
    }
    let (&tag, rest) = b.split_first().ok_or(FrameError::Empty)?;
    let mut i = In(rest);
    let r = match tag {
        1 => Request::Hello { proto: i.u16()?, pid: i.u32()? },
        2 => Request::ListApps { id: i.u32()? },
        3 => {
            let id = i.u32()?;
            let px = i.u16()?;
            if !ICONS_PX.contains(&px) {
                return Err(FrameError::Cap("icons.px"));
            }
            let count = usize::from(i.u16()?);
            if count > ICONS_IDS_MAX {
                return Err(FrameError::Cap("icons.ids"));
            }
            // Each id takes at least its two length bytes.
            if count.saturating_mul(2) > i.0.len() {
                return Err(FrameError::Short);
            }
            let mut ids = Vec::with_capacity(count);
            for _ in 0..count {
                ids.push(i.text_bytes(ICON_ID_MAX_BYTES, "icons.id")?);
            }
            Request::Icons { id, px, ids }
        }
        4 => Request::StartLinks { id: i.u32()? },
        5 => {
            let id = i.u32()?;
            let path = i.text(PATH_UNITS, "exe_info.path")?;
            let px = i.u16()?;
            if exe_icon_len(px).is_none() {
                return Err(FrameError::Cap("exe_info.px"));
            }
            Request::ExeInfo { id, path, px }
        }
        6 => Request::UserJournal { id: i.u32()? },
        7 => Request::Cancel { id: i.u32()? },
        8 => Request::Bye,
        t => return Err(FrameError::Tag(t)),
    };
    i.end()?;
    Ok(r)
}

pub fn encode_reply(r: &Reply) -> Result<Vec<u8>, FrameError> {
    let out = match r {
        Reply::Hello { proto, pid } => {
            let mut o = Out::tag(1);
            o.u16(*proto);
            o.u32(*pid);
            o
        }
        Reply::Chunk { id, last, bytes } => {
            let mut o = Out::tag(2);
            o.u32(*id);
            o.bool(*last);
            o.bytes(bytes, CHUNK_MAX, "chunk.bytes")?;
            o
        }
        Reply::Link(l) => {
            let mut o = Out::tag(3);
            o.u32(l.id);
            o.text(&l.rel, NAME_UNITS, "link.rel")?;
            o.text(&l.target, PATH_UNITS, "link.target")?;
            o.text(&l.name, NAME_UNITS, "link.name")?;
            o
        }
        Reply::ExeInfo(e) => {
            let mut o = Out::tag(4);
            o.u32(e.id);
            o.opt_text(e.description.as_deref(), NAME_UNITS, "exe_info.description")?;
            o.opt_text(e.product.as_deref(), NAME_UNITS, "exe_info.product")?;
            o.opt_text(e.company.as_deref(), NAME_UNITS, "exe_info.company")?;
            o.bool(e.icon.is_some());
            if let Some((px, pixels)) = &e.icon {
                let len = exe_icon_len(*px).ok_or(FrameError::Cap("exe_info.px"))?;
                if pixels.len() != len {
                    return Err(FrameError::Cap("exe_info.icon"));
                }
                o.u16(*px);
                o.bytes(pixels, EXE_ICON_MAX_BYTES, "exe_info.icon")?;
            }
            o
        }
        Reply::Journal(j) => {
            let mut o = Out::tag(5);
            o.u32(j.id);
            o.u64(j.seq);
            o.text_bytes(&j.kind, KIND_MAX_BYTES, "journal.kind")?;
            o.text(&j.target, NAME_UNITS, "journal.target")?;
            o.u64(j.ts);
            o
        }
        Reply::End { id, status } => {
            let mut o = Out::tag(6);
            o.u32(*id);
            o.u16(*status);
            o
        }
    };
    Ok(out.0)
}

pub fn decode_reply(b: &[u8]) -> Result<Reply, FrameError> {
    if b.len() > REC_MAX {
        return Err(FrameError::Cap("reply"));
    }
    let (&tag, rest) = b.split_first().ok_or(FrameError::Empty)?;
    let mut i = In(rest);
    let r = match tag {
        1 => Reply::Hello { proto: i.u16()?, pid: i.u32()? },
        2 => Reply::Chunk { id: i.u32()?, last: i.bool()?, bytes: i.bytes(CHUNK_MAX, "chunk.bytes")? },
        3 => Reply::Link(LinkRec {
            id: i.u32()?,
            rel: i.text(NAME_UNITS, "link.rel")?,
            target: i.text(PATH_UNITS, "link.target")?,
            name: i.text(NAME_UNITS, "link.name")?,
        }),
        4 => {
            let id = i.u32()?;
            let description = i.opt_text(NAME_UNITS, "exe_info.description")?;
            let product = i.opt_text(NAME_UNITS, "exe_info.product")?;
            let company = i.opt_text(NAME_UNITS, "exe_info.company")?;
            let icon = if i.bool()? {
                let px = i.u16()?;
                let len = exe_icon_len(px).ok_or(FrameError::Cap("exe_info.px"))?;
                // Exactly px x px x 4 bytes, checked before anything is taken.
                if usize::try_from(i.u32()?).ok() != Some(len) {
                    return Err(FrameError::Cap("exe_info.icon"));
                }
                Some((px, i.take(len)?.to_vec()))
            } else {
                None
            };
            Reply::ExeInfo(ExeInfoRec { id, description, product, company, icon })
        }
        5 => Reply::Journal(JournalRec {
            id: i.u32()?,
            seq: i.u64()?,
            kind: i.text_bytes(KIND_MAX_BYTES, "journal.kind")?,
            target: i.text(NAME_UNITS, "journal.target")?,
            ts: i.u64()?,
        }),
        6 => Reply::End { id: i.u32()?, status: i.u16()? },
        // EditText, RecentItems, DropRequest (SPEC-platform 5.4.4): refused until their consumer exists.
        t @ 7..=9 => return Err(FrameError::Reserved(t)),
        t => return Err(FrameError::Tag(t)),
    };
    i.end()?;
    Ok(r)
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::indexing_slicing)]
mod tests {
    use super::*;
    use crate::launcher::testkit::XorShift;

    /// `n` UTF-16 units of three UTF-8 bytes each: the longest encoding a unit cap allows.
    fn wide(n: usize) -> String {
        "\u{20ac}".repeat(n)
    }

    fn ascii(n: usize) -> String {
        "a".repeat(n)
    }

    fn icon(px: u16) -> Option<(u16, Vec<u8>)> {
        let n = usize::from(px) * usize::from(px) * 4;
        Some((px, (0..n).map(|i| i as u8).collect()))
    }

    fn requests() -> Vec<Request> {
        vec![
            Request::Hello { proto: AGENT_PROTO, pid: 0 },
            Request::Hello { proto: u16::MAX, pid: u32::MAX },
            Request::ListApps { id: 0 },
            Request::ListApps { id: u32::MAX },
            Request::Icons { id: 1, px: 8, ids: vec![] },
            Request::Icons { id: 2, px: 32, ids: vec!["Microsoft.App_x!App".into(), String::new(), wide(3)] },
            Request::Icons { id: u32::MAX, px: 256, ids: vec![ascii(ICON_ID_MAX_BYTES); ICONS_IDS_MAX] },
            Request::StartLinks { id: 3 },
            Request::ExeInfo { id: 4, path: String::new(), px: 16 },
            Request::ExeInfo { id: 5, path: wide(1024), px: 64 },
            Request::UserJournal { id: 6 },
            Request::Cancel { id: 7 },
            Request::Bye,
        ]
    }

    fn replies() -> Vec<Reply> {
        vec![
            Reply::Hello { proto: AGENT_PROTO, pid: 1 },
            Reply::Chunk { id: 0, last: false, bytes: vec![] },
            Reply::Chunk { id: 1, last: true, bytes: vec![7; 100] },
            Reply::Chunk { id: u32::MAX, last: true, bytes: vec![0xAB; CHUNK_MAX] },
            Reply::Link(LinkRec { id: 2, rel: String::new(), target: String::new(), name: String::new() }),
            Reply::Link(LinkRec { id: 3, rel: wide(260), target: wide(1024), name: wide(260) }),
            Reply::ExeInfo(ExeInfoRec { id: 4, description: None, product: None, company: None, icon: None }),
            Reply::ExeInfo(ExeInfoRec {
                id: 5,
                description: Some(wide(260)),
                product: Some(wide(260)),
                company: Some(wide(260)),
                icon: icon(64),
            }),
            Reply::ExeInfo(ExeInfoRec {
                id: 6,
                description: Some("Tool".into()),
                product: None,
                company: None,
                icon: icon(16),
            }),
            Reply::Journal(JournalRec { id: 7, seq: 0, kind: String::new(), target: String::new(), ts: 0 }),
            Reply::Journal(JournalRec { id: 8, seq: u64::MAX, kind: ascii(64), target: wide(260), ts: u64::MAX }),
            Reply::End { id: 9, status: 0 },
            Reply::End { id: u32::MAX, status: u16::MAX },
        ]
    }

    #[test]
    fn round_trip() {
        for r in requests() {
            let b = encode_request(&r).unwrap();
            assert!(b.len() <= REQ_MAX, "{} bytes", b.len());
            assert_eq!(decode_request(&b), Ok(r));
        }
        for r in replies() {
            let b = encode_reply(&r).unwrap();
            assert!(b.len() <= REC_MAX, "{} bytes", b.len());
            assert_eq!(decode_reply(&b), Ok(r));
        }
        // The largest frame of each record, as F-P8 computes it.
        let size = |r: &Request| encode_request(r).unwrap().len();
        let rsize = |r: &Reply| encode_reply(r).unwrap().len();
        assert_eq!(size(&requests()[6]), 2_052_009);
        assert_eq!(rsize(&replies()[3]), 61_450);
        assert_eq!(rsize(&replies()[5]), 4_643);
        assert_eq!(rsize(&replies()[7]), 18_745);
        assert_eq!(rsize(&replies()[10]), 869);
    }

    // Both caps fit the pipe's hard ceiling.
    const _: () = assert!(REQ_MAX <= oled_sys::pipe::FRAME_MAX && REC_MAX <= oled_sys::pipe::FRAME_MAX);

    /// A string field as the encoder writes it, whatever its length.
    fn raw_str(s: &str) -> Vec<u8> {
        let mut v = (s.len() as u16).to_le_bytes().to_vec();
        v.extend_from_slice(s.as_bytes());
        v
    }

    fn cat(parts: &[&[u8]]) -> Vec<u8> {
        parts.concat()
    }

    #[test]
    fn caps() {
        use FrameError::{Cap, Short};
        let link = |rel: String, target: String, name: String| Reply::Link(LinkRec { id: 1, rel, target, name });
        let raw_link = |rel: &str, target: &str, name: &str| {
            cat(&[&[3], &1u32.to_le_bytes(), &raw_str(rel), &raw_str(target), &raw_str(name)])
        };
        // Each string one unit over its cap, encoded and decoded (ASCII: under the byte rule, over the unit rule).
        assert_eq!(encode_reply(&link(wide(261), String::new(), String::new())), Err(Cap("link.rel")));
        assert_eq!(encode_reply(&link(String::new(), wide(1025), String::new())), Err(Cap("link.target")));
        assert_eq!(encode_reply(&link(String::new(), String::new(), wide(261))), Err(Cap("link.name")));
        assert_eq!(decode_reply(&raw_link(&ascii(261), "", "")), Err(Cap("link.rel")));
        assert_eq!(decode_reply(&raw_link("", &ascii(1025), "")), Err(Cap("link.target")));
        assert_eq!(decode_reply(&raw_link("", "", &ascii(261))), Err(Cap("link.name")));
        // A byte length above 3 x the unit cap is Cap before the bytes are looked at.
        assert_eq!(decode_reply(&cat(&[&[3], &1u32.to_le_bytes(), &781u16.to_le_bytes()])), Err(Cap("link.rel")));
        // A length above the bytes left is Short.
        assert_eq!(decode_reply(&cat(&[&[3], &1u32.to_le_bytes(), &500u16.to_le_bytes(), b"abc"])), Err(Short));
        let info = |d: Option<String>, p: Option<String>, c: Option<String>| {
            Reply::ExeInfo(ExeInfoRec { id: 1, description: d, product: p, company: c, icon: None })
        };
        assert_eq!(encode_reply(&info(Some(wide(261)), None, None)), Err(Cap("exe_info.description")));
        assert_eq!(encode_reply(&info(None, Some(wide(261)), None)), Err(Cap("exe_info.product")));
        assert_eq!(encode_reply(&info(None, None, Some(wide(261)))), Err(Cap("exe_info.company")));
        let raw_info = cat(&[&[4], &1u32.to_le_bytes(), &[1], &raw_str(&ascii(261)), &[0, 0, 0]]);
        assert_eq!(decode_reply(&raw_info), Err(Cap("exe_info.description")));
        let journal = |kind: String, target: String| Reply::Journal(JournalRec { id: 1, seq: 2, kind, target, ts: 3 });
        assert_eq!(encode_reply(&journal(ascii(65), String::new())), Err(Cap("journal.kind")));
        assert_eq!(encode_reply(&journal(String::new(), wide(261))), Err(Cap("journal.target")));
        let raw_journal = |kind: &str, target: &str| {
            cat(&[
                &[5],
                &1u32.to_le_bytes(),
                &2u64.to_le_bytes(),
                &raw_str(kind),
                &raw_str(target),
                &3u64.to_le_bytes(),
            ])
        };
        assert_eq!(decode_reply(&raw_journal(&ascii(65), "")), Err(Cap("journal.kind")));
        assert_eq!(decode_reply(&raw_journal("", &ascii(261))), Err(Cap("journal.target")));
        assert!(decode_reply(&raw_journal(&ascii(64), &ascii(260))).is_ok());
        // Icons: 2,001 ids, an id of 1,025 bytes, a size out of 8..=256.
        let icons = |px: u16, ids: Vec<String>| Request::Icons { id: 1, px, ids };
        assert_eq!(encode_request(&icons(32, vec![String::new(); 2001])), Err(Cap("icons.ids")));
        assert_eq!(encode_request(&icons(32, vec![ascii(1025)])), Err(Cap("icons.id")));
        assert_eq!(encode_request(&icons(7, vec![])), Err(Cap("icons.px")));
        assert_eq!(encode_request(&icons(257, vec![])), Err(Cap("icons.px")));
        let raw_icons = |px: u16, count: u16, rest: &[u8]| {
            cat(&[&[3], &1u32.to_le_bytes(), &px.to_le_bytes(), &count.to_le_bytes(), rest])
        };
        assert_eq!(decode_request(&raw_icons(32, 2001, &[])), Err(Cap("icons.ids")));
        assert_eq!(decode_request(&raw_icons(32, 1, &raw_str(&ascii(1025)))), Err(Cap("icons.id")));
        assert_eq!(decode_request(&raw_icons(300, 0, &[])), Err(Cap("icons.px")));
        // A count the frame cannot hold is Short before the list is allocated.
        assert_eq!(decode_request(&raw_icons(32, 2000, &[0, 0])), Err(Short));
        // ExeInfo: a path one unit over, a size not in EXE_ICON_PX.
        let exe = |path: String, px: u16| Request::ExeInfo { id: 1, path, px };
        assert_eq!(encode_request(&exe(wide(1025), 32)), Err(Cap("exe_info.path")));
        assert_eq!(encode_request(&exe(String::new(), 48)), Err(Cap("exe_info.px")));
        let raw_exe = |path: &str, px: u16| cat(&[&[5], &1u32.to_le_bytes(), &raw_str(path), &px.to_le_bytes()]);
        assert_eq!(decode_request(&raw_exe(&ascii(1025), 32)), Err(Cap("exe_info.path")));
        assert_eq!(decode_request(&raw_exe("", 48)), Err(Cap("exe_info.px")));
        assert!(decode_request(&raw_exe(&ascii(1024), 24)).is_ok());
        // The ExeInfo icon: a size not in EXE_ICON_PX, a length other than px * px * 4.
        let with_icon = |icon: Option<(u16, Vec<u8>)>| {
            Reply::ExeInfo(ExeInfoRec { id: 1, description: None, product: None, company: None, icon })
        };
        assert_eq!(encode_reply(&with_icon(Some((48, vec![0; 48 * 48 * 4])))), Err(Cap("exe_info.px")));
        assert_eq!(encode_reply(&with_icon(Some((16, vec![0; 16 * 16 * 4 - 1])))), Err(Cap("exe_info.icon")));
        assert_eq!(encode_reply(&with_icon(Some((16, vec![0; 16 * 16 * 4 + 1])))), Err(Cap("exe_info.icon")));
        let raw_icon = |px: u16, len: u32, pixels: usize| {
            cat(&[&[4], &1u32.to_le_bytes(), &[0, 0, 0, 1], &px.to_le_bytes(), &len.to_le_bytes(), &vec![0; pixels]])
        };
        assert_eq!(decode_reply(&raw_icon(48, 48 * 48 * 4, 48 * 48 * 4)), Err(Cap("exe_info.px")));
        assert_eq!(decode_reply(&raw_icon(16, 16 * 16 * 4 - 1, 16 * 16 * 4 - 1)), Err(Cap("exe_info.icon")));
        assert_eq!(decode_reply(&raw_icon(16, u32::MAX, 0)), Err(Cap("exe_info.icon")));
        assert_eq!(decode_reply(&raw_icon(16, 16 * 16 * 4, 10)), Err(Short));
        assert!(decode_reply(&raw_icon(24, 24 * 24 * 4, 24 * 24 * 4)).is_ok());
        // A chunk one byte over, encoded and decoded; a length the frame cannot hold.
        let chunk = Reply::Chunk { id: 1, last: true, bytes: vec![0; CHUNK_MAX + 1] };
        assert_eq!(encode_reply(&chunk), Err(Cap("chunk.bytes")));
        let raw_chunk = |len: u32, n: usize| cat(&[&[2], &1u32.to_le_bytes(), &[1], &len.to_le_bytes(), &vec![0; n]]);
        assert_eq!(decode_reply(&raw_chunk(CHUNK_MAX as u32 + 1, CHUNK_MAX + 1)), Err(Cap("chunk.bytes")));
        assert_eq!(decode_reply(&raw_chunk(u32::MAX, 0)), Err(Cap("chunk.bytes")));
        assert_eq!(decode_reply(&raw_chunk(100, 99)), Err(Short));
        // Whole frames over their side's cap.
        assert_eq!(decode_reply(&vec![1; REC_MAX + 1]), Err(Cap("reply")));
        assert_eq!(decode_request(&vec![1; REQ_MAX + 1]), Err(Cap("request")));
    }

    #[test]
    fn encodings() {
        use FrameError::{Bool, Empty, Short, Tag, Trailing, Utf8};
        assert_eq!(encode_request(&Request::Hello { proto: 1, pid: 7 }).unwrap(), [1, 1, 0, 7, 0, 0, 0]);
        assert_eq!(encode_reply(&Reply::Hello { proto: 1, pid: 7 }).unwrap(), [1, 1, 0, 7, 0, 0, 0]);
        assert_eq!(encode_reply(&Reply::End { id: 3, status: 0 }).unwrap(), [6, 3, 0, 0, 0, 0, 0]);
        assert_eq!(encode_request(&Request::Bye).unwrap(), [8]);
        assert_eq!(encode_request(&Request::Cancel { id: 0x0102_0304 }).unwrap(), [7, 4, 3, 2, 1]);
        // bool 2, an Option byte 2.
        assert_eq!(decode_reply(&[2, 1, 0, 0, 0, 2, 0, 0, 0, 0]), Err(Bool(2)));
        assert_eq!(decode_reply(&[4, 1, 0, 0, 0, 2]), Err(Bool(2)));
        assert_eq!(decode_reply(&[4, 1, 0, 0, 0, 0, 0, 0, 7]), Err(Bool(7)));
        // One byte after the last field.
        assert_eq!(decode_request(&[1, 1, 0, 7, 0, 0, 0, 0]), Err(Trailing));
        assert_eq!(decode_reply(&[6, 3, 0, 0, 0, 0, 0, 9]), Err(Trailing));
        assert_eq!(decode_request(&[8, 0]), Err(Trailing));
        // Nothing, a cut field, an unknown tag, bytes that are not UTF-8.
        assert_eq!(decode_request(&[]), Err(Empty));
        assert_eq!(decode_reply(&[]), Err(Empty));
        assert_eq!(decode_request(&[1, 1, 0, 7]), Err(Short));
        for t in [0u8, 9, 10, 255] {
            assert_eq!(decode_request(&[t, 0, 0, 0, 0]), Err(Tag(t)));
        }
        for t in [0u8, 10, 255] {
            assert_eq!(decode_reply(&[t, 0, 0, 0, 0]), Err(Tag(t)));
        }
        assert_eq!(decode_request(&cat(&[&[5], &1u32.to_le_bytes(), &[1, 0, 0xFF], &16u16.to_le_bytes()])), Err(Utf8));
    }

    #[test]
    fn reserved_tags_are_refused() {
        for t in [7u8, 8, 9] {
            assert_eq!(decode_reply(&[t]), Err(FrameError::Reserved(t)));
            assert_eq!(decode_reply(&[t, 1, 0, 0, 0, 0, 0]), Err(FrameError::Reserved(t)));
        }
    }

    /// One of the mutations a seed frame gets: bytes flipped, set, inserted, removed or cut; a length-sized field
    /// written at a random place (0, a cap, one over, the most); bytes appended.
    fn mutate(rng: &mut XorShift, seed: &[u8]) -> Vec<u8> {
        let mut b = seed.to_vec();
        let below = |rng: &mut XorShift, n: usize| if n == 0 { 0 } else { (rng.next_u64() % n as u64) as usize };
        for _ in 0..=below(rng, 4) {
            let at = below(rng, b.len() + 1);
            match below(rng, 8) {
                0 if at < b.len() => b[at] ^= 1 << below(rng, 8),
                1 if at < b.len() => b[at] = rng.next_u64() as u8,
                2 => b.truncate(at),
                3 => {
                    let n = 1 + below(rng, 8);
                    let tail = b.split_off(at);
                    b.extend((0..n).map(|_| rng.next_u64() as u8));
                    b.extend(tail);
                }
                4 => {
                    let end = (at + 1 + below(rng, 16)).min(b.len());
                    if at < end {
                        b.drain(at..end);
                    }
                }
                5 => {
                    let v: u32 = *rng.pick(&[
                        0,
                        1,
                        2,
                        64,
                        65,
                        260,
                        261,
                        781,
                        1024,
                        1025,
                        3073,
                        2000,
                        2001,
                        61_440,
                        61_441,
                        16_384,
                        u32::MAX,
                    ]);
                    let wide = below(rng, 2) == 0;
                    let bytes = if wide { v.to_le_bytes().to_vec() } else { (v as u16).to_le_bytes().to_vec() };
                    for (i, x) in bytes.into_iter().enumerate() {
                        if let Some(slot) = b.get_mut(at + i) {
                            *slot = x;
                        }
                    }
                }
                6 => b.extend((0..1 + below(rng, 4)).map(|_| rng.next_u64() as u8)),
                _ if !b.is_empty() => b[0] = below(rng, 11) as u8,
                _ => {}
            }
        }
        b
    }

    fn fuzz_requests(cases: u32, seed: u64) {
        let seeds: Vec<Vec<u8>> = requests().iter().map(|r| encode_request(r).unwrap()).collect();
        let mut rng = XorShift::new(seed);
        for _ in 0..cases {
            let seed = rng_pick(&mut rng, &seeds);
            let frame = mutate(&mut rng, seed);
            if let Ok(r) = decode_request(&frame) {
                assert_eq!(encode_request(&r).as_deref(), Ok(&frame[..]), "{r:?}");
            }
        }
    }

    fn fuzz_replies(cases: u32, seed: u64) {
        let seeds: Vec<Vec<u8>> = replies().iter().map(|r| encode_reply(r).unwrap()).collect();
        let mut rng = XorShift::new(seed);
        for _ in 0..cases {
            let seed = rng_pick(&mut rng, &seeds);
            let frame = mutate(&mut rng, seed);
            if let Ok(r) = decode_reply(&frame) {
                assert_eq!(encode_reply(&r).as_deref(), Ok(&frame[..]), "{r:?}");
            }
        }
    }

    /// A seed frame; the 2 MiB one only once in 64 picks, so the 10,000 cases stay quick.
    fn rng_pick<'a>(rng: &mut XorShift, seeds: &'a [Vec<u8>]) -> &'a [u8] {
        loop {
            let s = rng.pick(seeds);
            if s.len() < 100_000 || rng.next_u64().is_multiple_of(64) {
                return s;
            }
        }
    }

    #[test]
    fn fuzz_agent_request_10k() {
        fuzz_requests(10_000, 0xA6E1);
    }

    #[test]
    fn fuzz_agent_reply_10k() {
        fuzz_replies(10_000, 0xA6E2);
    }

    #[test]
    #[ignore = "nightly: 1,000,000 inputs"]
    fn fuzz_agent_request_1m() {
        fuzz_requests(1_000_000, 0xA6E3);
    }

    #[test]
    #[ignore = "nightly: 1,000,000 inputs"]
    fn fuzz_agent_reply_1m() {
        fuzz_replies(1_000_000, 0xA6E4);
    }
}
