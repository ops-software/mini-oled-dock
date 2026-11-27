//! Little-endian primitives of the frame codecs. Every getter checks the remaining length first (no indexing); a
//! getter that fails leaves the reader where it was.

use super::FrameError;

pub struct Writer {
    buf: Vec<u8>,
    /// A length prefix could not hold its list (more than u16::MAX units, more than u32::MAX bytes): encode refuses
    /// the frame (TooLong) instead of sending a truncated field.
    overflow: bool,
}

#[allow(clippy::new_without_default, clippy::len_without_is_empty)] // the frozen interface: new() and len() only
impl Writer {
    pub fn new() -> Self {
        Writer { buf: Vec::new(), overflow: false }
    }
    pub fn len(&self) -> usize {
        self.buf.len()
    }
    pub fn into_inner(self) -> Vec<u8> {
        self.buf
    }
    pub fn u8(&mut self, v: u8) {
        self.buf.push(v);
    }
    pub fn u16(&mut self, v: u16) {
        self.buf.extend_from_slice(&v.to_le_bytes());
    }
    pub fn u32(&mut self, v: u32) {
        self.buf.extend_from_slice(&v.to_le_bytes());
    }
    pub fn u64(&mut self, v: u64) {
        self.buf.extend_from_slice(&v.to_le_bytes());
    }
    pub fn i32(&mut self, v: i32) {
        self.buf.extend_from_slice(&v.to_le_bytes());
    }
    pub fn i64(&mut self, v: i64) {
        self.buf.extend_from_slice(&v.to_le_bytes());
    }
    pub fn bool(&mut self, v: bool) {
        self.u8(u8::from(v));
    }
    /// u32 length, then the bytes. Past u32::MAX bytes the prefix saturates, only that many bytes follow and the
    /// writer is marked overflowed.
    pub fn bytes(&mut self, v: &[u8]) {
        let n = u32::try_from(v.len()).unwrap_or_else(|_| {
            self.overflow = true;
            u32::MAX
        });
        self.u32(n);
        self.buf.extend_from_slice(v.get(..n as usize).unwrap_or(v));
    }
    /// u32 byte length, then UTF-8.
    pub fn str(&mut self, v: &str) {
        self.bytes(v.as_bytes());
    }
    /// u16 unit count, then UTF-16LE units. Past u16::MAX units the count saturates, only that many units follow and
    /// the writer is marked overflowed.
    pub fn wide(&mut self, v: &[u16]) {
        let n = u16::try_from(v.len()).unwrap_or_else(|_| {
            self.overflow = true;
            u16::MAX
        });
        let v = v.get(..usize::from(n)).unwrap_or(v);
        self.u16(n);
        self.buf.reserve(2 * v.len());
        for u in v {
            self.buf.extend_from_slice(&u.to_le_bytes());
        }
    }
    pub fn fixed<const N: usize>(&mut self, v: &[u8; N]) {
        self.buf.extend_from_slice(v);
    }

    /// A writer that appends to `buf` (encode writes a frame straight into the caller's buffer).
    pub(super) fn from_vec(buf: Vec<u8>) -> Self {
        Writer { buf, overflow: false }
    }
    pub(super) fn overflowed(&self) -> bool {
        self.overflow
    }
}

pub struct Reader<'a> {
    buf: &'a [u8],
    at: usize,
}

impl<'a> Reader<'a> {
    pub fn new(buf: &'a [u8]) -> Self {
        Reader { buf, at: 0 }
    }
    pub fn remaining(&self) -> usize {
        self.buf.len().saturating_sub(self.at)
    }
    /// Trailing when bytes are left.
    pub fn finish(self) -> Result<(), FrameError> {
        match self.remaining() {
            0 => Ok(()),
            n => Err(FrameError::Trailing { extra: u32::try_from(n).unwrap_or(u32::MAX) }),
        }
    }
    pub fn u8(&mut self) -> Result<u8, FrameError> {
        self.fixed::<1>().map(u8::from_le_bytes)
    }
    pub fn u16(&mut self) -> Result<u16, FrameError> {
        self.fixed().map(u16::from_le_bytes)
    }
    pub fn u32(&mut self) -> Result<u32, FrameError> {
        self.fixed().map(u32::from_le_bytes)
    }
    pub fn u64(&mut self) -> Result<u64, FrameError> {
        self.fixed().map(u64::from_le_bytes)
    }
    pub fn i32(&mut self) -> Result<i32, FrameError> {
        self.fixed().map(i32::from_le_bytes)
    }
    pub fn i64(&mut self) -> Result<i64, FrameError> {
        self.fixed().map(i64::from_le_bytes)
    }
    /// 0 or 1; any other byte is BadValue.
    pub fn bool(&mut self, field: &'static str) -> Result<bool, FrameError> {
        let at = self.at;
        match self.u8()? {
            0 => Ok(false),
            1 => Ok(true),
            v => {
                self.at = at;
                Err(FrameError::BadValue { field, value: u32::from(v) })
            }
        }
    }
    /// A u32 length, at most `max` (Cap), then that many bytes.
    pub fn bytes(&mut self, field: &'static str, max: u32) -> Result<&'a [u8], FrameError> {
        let at = self.at;
        let n = self.u32()?;
        let out = if n > max { Err(FrameError::Cap { field, len: n, max }) } else { self.take(n as usize) };
        if out.is_err() {
            self.at = at;
        }
        out
    }
    /// A u32 byte length, at most `max_bytes` (Cap), then UTF-8 (BadUtf8).
    pub fn str(&mut self, field: &'static str, max_bytes: u32) -> Result<&'a str, FrameError> {
        let at = self.at;
        let out = self
            .bytes(field, max_bytes)
            .and_then(|b| std::str::from_utf8(b).map_err(|_| FrameError::BadUtf8 { field }));
        if out.is_err() {
            self.at = at;
        }
        out
    }
    /// A u16 unit count, at most `max_units` (Cap), then UTF-16LE units (an odd or missing byte is Short).
    pub fn wide(&mut self, field: &'static str, max_units: u16) -> Result<Vec<u16>, FrameError> {
        let at = self.at;
        let n = self.u16()?;
        let raw = if n > max_units {
            Err(FrameError::Cap { field, len: u32::from(n), max: u32::from(max_units) })
        } else {
            self.take(2 * usize::from(n))
        };
        match raw {
            Ok(raw) => Ok(raw.chunks_exact(2).map(|c| <[u8; 2]>::try_from(c).map_or(0, u16::from_le_bytes)).collect()),
            Err(e) => {
                self.at = at;
                Err(e)
            }
        }
    }
    pub fn fixed<const N: usize>(&mut self) -> Result<[u8; N], FrameError> {
        self.take(N).and_then(|s| <[u8; N]>::try_from(s).map_err(|_| FrameError::Short))
    }
    /// List length: <= max and <= remaining bytes (every element takes at least one byte), so a caller may allocate
    /// that many elements before reading them.
    pub fn count(&mut self, field: &'static str, max: u32) -> Result<u32, FrameError> {
        let at = self.at;
        let n = self.u32()?;
        let left = u32::try_from(self.remaining()).unwrap_or(u32::MAX);
        let out = if n > max {
            Err(FrameError::Cap { field, len: n, max })
        } else if n > left {
            Err(FrameError::Cap { field, len: n, max: left })
        } else {
            Ok(n)
        };
        if out.is_err() {
            self.at = at;
        }
        out
    }

    /// The next `n` bytes; Short (and nothing consumed) when fewer are left.
    fn take(&mut self, n: usize) -> Result<&'a [u8], FrameError> {
        let end = self.at.checked_add(n).ok_or(FrameError::Short)?;
        let s = self.buf.get(self.at..end).ok_or(FrameError::Short)?;
        self.at = end;
        Ok(s)
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::indexing_slicing)]

    use super::*;

    #[test]
    fn primitives_round_trip_at_min_and_max() {
        let mut w = Writer::new();
        assert_eq!(w.len(), 0);
        for v in [0, u8::MAX] {
            w.u8(v);
        }
        for v in [0, u16::MAX] {
            w.u16(v);
        }
        for v in [0, u32::MAX] {
            w.u32(v);
        }
        for v in [0, u64::MAX] {
            w.u64(v);
        }
        for v in [i32::MIN, 0, i32::MAX] {
            w.i32(v);
        }
        for v in [i64::MIN, 0, i64::MAX] {
            w.i64(v);
        }
        w.bool(false);
        w.bool(true);
        w.bytes(&[]);
        w.bytes(&[0, 255, 7]);
        w.str("");
        w.str("h\u{e9}llo \u{1F600}");
        w.wide(&[]);
        w.wide(&[0, 0xD800, u16::MAX]);
        w.fixed(&[1u8, 2, 3, 4]);
        w.fixed::<0>(&[]);
        let len = w.len();
        let buf = w.into_inner();
        assert_eq!(buf.len(), len);
        let mut r = Reader::new(&buf);
        assert_eq!(r.remaining(), len);
        assert_eq!((r.u8().unwrap(), r.u8().unwrap()), (0, u8::MAX));
        assert_eq!((r.u16().unwrap(), r.u16().unwrap()), (0, u16::MAX));
        assert_eq!((r.u32().unwrap(), r.u32().unwrap()), (0, u32::MAX));
        assert_eq!((r.u64().unwrap(), r.u64().unwrap()), (0, u64::MAX));
        assert_eq!([r.i32().unwrap(), r.i32().unwrap(), r.i32().unwrap()], [i32::MIN, 0, i32::MAX]);
        assert_eq!([r.i64().unwrap(), r.i64().unwrap(), r.i64().unwrap()], [i64::MIN, 0, i64::MAX]);
        assert_eq!((r.bool("b").unwrap(), r.bool("b").unwrap()), (false, true));
        assert_eq!(r.bytes("b", 0).unwrap(), &[] as &[u8]);
        assert_eq!(r.bytes("b", 3).unwrap(), &[0, 255, 7]);
        assert_eq!(r.str("s", 0).unwrap(), "");
        assert_eq!(r.str("s", 11).unwrap(), "h\u{e9}llo \u{1F600}");
        assert_eq!(r.wide("w", 0).unwrap(), Vec::<u16>::new());
        assert_eq!(r.wide("w", 3).unwrap(), vec![0, 0xD800, u16::MAX]);
        assert_eq!(r.fixed::<4>().unwrap(), [1, 2, 3, 4]);
        assert_eq!(r.fixed::<0>().unwrap(), [0u8; 0]);
        assert_eq!(r.remaining(), 0);
        r.finish().unwrap();
    }

    #[test]
    fn little_endian_layout() {
        let mut w = Writer::new();
        w.u16(0x0102);
        w.u32(0x0304_0506);
        w.u64(0x0708_090A_0B0C_0D0E);
        w.i32(-2);
        w.str("ab");
        w.wide(&[0x0041, 0x20AC]);
        let want: &[u8] = &[
            0x02, 0x01, 0x06, 0x05, 0x04, 0x03, 0x0E, 0x0D, 0x0C, 0x0B, 0x0A, 0x09, 0x08, 0x07, 0xFE, 0xFF, 0xFF, 0xFF,
            2, 0, 0, 0, b'a', b'b', 2, 0, 0x41, 0, 0xAC, 0x20,
        ];
        assert_eq!(w.into_inner(), want);
    }

    /// Every getter on a buffer one byte short returns Short and consumes nothing.
    #[test]
    fn reader_refuses_short_input() {
        #[track_caller]
        fn short<T: std::fmt::Debug>(full: &[u8], get: impl Fn(&mut Reader<'_>) -> Result<T, FrameError>) {
            let mut r = Reader::new(full);
            get(&mut r).unwrap();
            assert_eq!(r.remaining(), 0, "the sample is exactly one value");
            let cut = &full[..full.len() - 1];
            let mut r = Reader::new(cut);
            assert_eq!(get(&mut r).unwrap_err(), FrameError::Short);
            assert_eq!(r.remaining(), cut.len(), "nothing consumed");
        }
        short(&[1], |r| r.u8());
        short(&[1, 2], |r| r.u16());
        short(&[1, 2, 3, 4], |r| r.u32());
        short(&[1; 8], |r| r.u64());
        short(&[1, 2, 3, 4], |r| r.i32());
        short(&[1; 8], |r| r.i64());
        short(&[1], |r| r.bool("b"));
        short(&[2, 0, 0, 0, 9, 9], |r| r.bytes("b", 10).map(<[u8]>::to_vec));
        short(&[0, 0, 0, 0], |r| r.bytes("b", 10).map(<[u8]>::to_vec));
        short(&[2, 0, 0, 0, b'o', b'k'], |r| r.str("s", 10).map(str::to_owned));
        short(&[1, 0, 0x41, 0], |r| r.wide("w", 10));
        short(&[0, 0], |r| r.wide("w", 10));
        short(&[1, 2, 3], |r| r.fixed::<3>());
        short(&[0, 0, 0, 0], |r| r.count("c", 10));
    }

    #[test]
    fn caps_and_bad_values_name_their_field() {
        let mut w = Writer::new();
        w.str("too long");
        let buf = w.into_inner();
        let mut r = Reader::new(&buf);
        assert_eq!(r.str("name", 7), Err(FrameError::Cap { field: "name", len: 8, max: 7 }));
        assert_eq!(r.remaining(), buf.len(), "a refused field consumes nothing");
        assert_eq!(r.str("name", 8), Ok("too long"));

        let mut r = Reader::new(&[3, 0, 0, 0, 1, 2, 3]);
        assert_eq!(r.bytes("pidl", 2), Err(FrameError::Cap { field: "pidl", len: 3, max: 2 }));
        let mut r = Reader::new(&[2, 0, 0, 0, 0xC3, 0x28]);
        assert_eq!(r.str("label", 10), Err(FrameError::BadUtf8 { field: "label" }));
        assert_eq!(r.remaining(), 6);

        let mut r = Reader::new(&[2]);
        assert_eq!(r.bool("on"), Err(FrameError::BadValue { field: "on", value: 2 }));
        assert_eq!(r.remaining(), 1);
        let mut r = Reader::new(&[255]);
        assert_eq!(r.bool("on"), Err(FrameError::BadValue { field: "on", value: 255 }));

        let mut r = Reader::new(&[3, 0, 1, 0, 2, 0, 3, 0]);
        assert_eq!(r.wide("path", 2), Err(FrameError::Cap { field: "path", len: 3, max: 2 }));
        assert_eq!(r.wide("path", 3), Ok(vec![1, 2, 3]));
        // Two units announced, three bytes present: an odd byte count.
        let mut r = Reader::new(&[2, 0, 1, 0, 2]);
        assert_eq!(r.wide("path", 10), Err(FrameError::Short));
    }

    #[test]
    fn count_is_bounded_by_its_cap_and_the_bytes_left() {
        let mut r = Reader::new(&[3, 0, 0, 0, 9, 9, 9]);
        assert_eq!(r.count("items", 2), Err(FrameError::Cap { field: "items", len: 3, max: 2 }));
        assert_eq!(r.count("items", 3), Ok(3));
        let mut r = Reader::new(&[4, 0, 0, 0, 9, 9, 9]);
        assert_eq!(r.count("items", 2_048), Err(FrameError::Cap { field: "items", len: 4, max: 3 }));
        assert_eq!(r.remaining(), 7);
        let mut r = Reader::new(&[0xFF, 0xFF, 0xFF, 0xFF]);
        assert_eq!(r.count("items", u32::MAX), Err(FrameError::Cap { field: "items", len: u32::MAX, max: 0 }));
        let mut r = Reader::new(&[0, 0, 0, 0]);
        assert_eq!(r.count("items", 0), Ok(0));
    }

    #[test]
    fn finish_with_bytes_left_is_trailing() {
        let mut r = Reader::new(&[1, 2, 3]);
        r.u8().unwrap();
        assert_eq!(r.finish(), Err(FrameError::Trailing { extra: 2 }));
        assert_eq!(Reader::new(&[]).finish(), Ok(()));
    }

    #[test]
    fn oversized_lists_mark_the_writer() {
        let mut w = Writer::new();
        w.wide(&[7; 65_535]);
        assert!(!w.overflowed());
        let mut w = Writer::new();
        w.wide(&vec![7; 65_536]);
        assert!(w.overflowed(), "a count that cannot be written is never silently cut");
        let buf = w.into_inner();
        assert_eq!(buf.len(), 2 + 2 * 65_535, "the stream stays self-consistent");
        let mut r = Reader::new(&buf);
        assert_eq!(r.wide("w", u16::MAX).unwrap().len(), 65_535);
        r.finish().unwrap();
        let mut w = Writer::from_vec(vec![9, 9]);
        w.u8(1);
        assert_eq!(w.into_inner(), [9, 9, 1], "from_vec appends");
    }
}
