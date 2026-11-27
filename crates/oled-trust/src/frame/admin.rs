//! Admin frames (SPEC-platform 5.4.3): the tasks-admin requests and replies, and their codec. A frame is a private
//! 10-byte Header (magic u32, version u16, kind u16, len u16, little-endian; not the 6-byte layout of the files
//! protocols, see frame/mod.rs) and a payload of exactly len bytes, laid out per kind as P1-tasks-engine 2.5 fixes.
//! Decoding is strict: the header is judged before any field, every length is checked against its cap before anything
//! is read or allocated, every enum and bool byte must be in range and nothing may follow the payload. The encoding is
//! therefore canonical: decode(encode(x)) == x, and encode(decode(b)) == b for every b that decodes.

use std::ops::RangeInclusive;

pub const MAGIC: u32 = u32::from_le_bytes(*b"MOTA");
pub const PROTO: u16 = 1;
pub const MAX_REQUEST_PAYLOAD: usize = 1024;
/// A Text reply: u16 count + UTF-16.
pub const MAX_REPLY_PAYLOAD: usize = 2 + 2 * MAX_TEXT_UNITS;
pub const MAX_KEYS: usize = 64;
/// UTF-16 units.
pub const MAX_SERVICE_NAME: usize = 256;
/// 32 KiB (QueryCommandLine).
pub const MAX_TEXT_UNITS: usize = 16 * 1024;

/// Little-endian, 10 bytes: magic u32, version u16, kind u16, len u16.
struct Header {
    magic: u32,
    version: u16,
    kind: u16,
    len: u16,
}

const HEADER_LEN: usize = 10;

const KIND_HELLO: u16 = 1;
const KIND_TERMINATE: u16 = 2;
const KIND_SET_PRIORITY: u16 = 3;
const KIND_SET_EFFICIENCY: u16 = 4;
const KIND_QUERY_COMMAND_LINE: u16 = 5;
const KIND_SERVICE_CONTROL: u16 = 6;
const KIND_STARTUP_SET_ENABLED: u16 = 7;
const KIND_RESTART_DOCK: u16 = 8;
const KIND_OK: u16 = 0x81;
const KIND_TEXT: u16 = 0x82;
const KIND_DENIED: u16 = 0x83;
const KIND_FAILED: u16 = 0x84;

impl Header {
    /// Judges the header of one whole frame and returns it with its payload, in this order: Header (fewer than 10
    /// bytes), Magic, Version, Kind (not in kinds), TooLong (len over max_payload), Length (fewer bytes than len
    /// follow the header) and Trailing (more).
    fn decode(buf: &[u8], kinds: RangeInclusive<u16>, max_payload: usize) -> Result<(Header, &[u8]), AdminError> {
        let Some((head, payload)) = buf.split_at_checked(HEADER_LEN) else {
            return Err(AdminError::Header);
        };
        let [m0, m1, m2, m3, v0, v1, k0, k1, l0, l1] =
            <[u8; HEADER_LEN]>::try_from(head).map_err(|_| AdminError::Header)?;
        let h = Header {
            magic: u32::from_le_bytes([m0, m1, m2, m3]),
            version: u16::from_le_bytes([v0, v1]),
            kind: u16::from_le_bytes([k0, k1]),
            len: u16::from_le_bytes([l0, l1]),
        };
        if h.magic != MAGIC {
            return Err(AdminError::Magic);
        }
        if h.version != PROTO {
            return Err(AdminError::Version);
        }
        if !kinds.contains(&h.kind) {
            return Err(AdminError::Kind(h.kind));
        }
        let len = usize::from(h.len);
        if len > max_payload {
            return Err(AdminError::TooLong);
        }
        match payload.len().cmp(&len) {
            std::cmp::Ordering::Less => Err(AdminError::Length),
            std::cmp::Ordering::Greater => Err(AdminError::Trailing),
            std::cmp::Ordering::Equal => Ok((h, payload)),
        }
    }

    /// Appends the 10 header bytes.
    fn encode(&self, out: &mut Vec<u8>) {
        out.extend_from_slice(&self.magic.to_le_bytes());
        out.extend_from_slice(&self.version.to_le_bytes());
        out.extend_from_slice(&self.kind.to_le_bytes());
        out.extend_from_slice(&self.len.to_le_bytes());
    }
}

/// Appends one frame: the header, then the payload body writes. TooLong when the payload is over max_payload;
/// on any error out is left as it was.
fn encode_frame(
    out: &mut Vec<u8>,
    kind: u16,
    max_payload: usize,
    body: impl FnOnce(&mut Vec<u8>) -> Result<(), AdminError>,
) -> Result<(), AdminError> {
    let start = out.len();
    Header { magic: MAGIC, version: PROTO, kind, len: 0 }.encode(out);
    let done = body(out).and_then(|()| {
        let n = out.len() - start - HEADER_LEN;
        let len = u16::try_from(n).ok().filter(|_| n <= max_payload).ok_or(AdminError::TooLong)?;
        // The len field is the last two header bytes.
        let at = start + HEADER_LEN - 2;
        out.get_mut(at..at + 2).ok_or(AdminError::Header)?.copy_from_slice(&len.to_le_bytes());
        Ok(())
    });
    if done.is_err() {
        out.truncate(start);
    }
    done
}

/// Bounds-checked little-endian reads over one payload; reading past its end is Length (len shorter than the
/// content), and so are bytes left over at the end (len longer than the content).
struct Cursor<'a> {
    rest: &'a [u8],
}

impl<'a> Cursor<'a> {
    fn take(&mut self, n: usize) -> Result<&'a [u8], AdminError> {
        let (head, rest) = self.rest.split_at_checked(n).ok_or(AdminError::Length)?;
        self.rest = rest;
        Ok(head)
    }
    fn array<const N: usize>(&mut self) -> Result<[u8; N], AdminError> {
        self.take(N).and_then(|s| <[u8; N]>::try_from(s).map_err(|_| AdminError::Length))
    }
    fn u8(&mut self) -> Result<u8, AdminError> {
        self.array::<1>().map(u8::from_le_bytes)
    }
    fn u16(&mut self) -> Result<u16, AdminError> {
        self.array().map(u16::from_le_bytes)
    }
    fn u32(&mut self) -> Result<u32, AdminError> {
        self.array().map(u32::from_le_bytes)
    }
    fn u64(&mut self) -> Result<u64, AdminError> {
        self.array().map(u64::from_le_bytes)
    }
    /// 0 or 1; any other byte is Field(field).
    fn bool(&mut self, field: &'static str) -> Result<bool, AdminError> {
        match self.u8()? {
            0 => Ok(false),
            1 => Ok(true),
            _ => Err(AdminError::Field(field)),
        }
    }
    fn key(&mut self) -> Result<ProcKey, AdminError> {
        Ok(ProcKey { pid: self.u32()?, created: self.u64()? })
    }
    /// units UTF-16LE units (the caller has checked the count against its cap) as a String; an unpaired surrogate is
    /// Field(field). One allocation, of the exact UTF-8 size.
    fn utf16(&mut self, units: usize, field: &'static str) -> Result<String, AdminError> {
        let raw = self.take(2 * units)?;
        let decoded =
            || char::decode_utf16(raw.chunks_exact(2).map(|c| <[u8; 2]>::try_from(c).map_or(0, u16::from_le_bytes)));
        let mut size = 0;
        for c in decoded() {
            size += c.map_err(|_| AdminError::Field(field))?.len_utf8();
        }
        let mut s = String::with_capacity(size);
        s.extend(decoded().map(|c| c.unwrap_or(char::REPLACEMENT_CHARACTER)));
        Ok(s)
    }
    /// Length when bytes are left: the frame's len was longer than its content.
    fn end(self) -> Result<(), AdminError> {
        if self.rest.is_empty() {
            Ok(())
        } else {
            Err(AdminError::Length)
        }
    }
}

fn put_key(out: &mut Vec<u8>, k: &ProcKey) {
    out.extend_from_slice(&k.pid.to_le_bytes());
    out.extend_from_slice(&k.created.to_le_bytes());
}

/// u16 unit count, then the UTF-16LE units of s; err when the count is outside units.
fn put_utf16(out: &mut Vec<u8>, s: &str, units: RangeInclusive<usize>, err: AdminError) -> Result<(), AdminError> {
    let n = s.encode_utf16().count();
    if !units.contains(&n) {
        return Err(err);
    }
    out.extend_from_slice(&u16::try_from(n).map_err(|_| err)?.to_le_bytes());
    out.reserve(2 * n);
    for u in s.encode_utf16() {
        out.extend_from_slice(&u.to_le_bytes());
    }
    Ok(())
}

/// created = CreateTime FILETIME (100 ns since 1601); the PID-reuse guard. The canonical home: oled_sysmon::ProcKey re-exports it.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct ProcKey {
    pub pid: u32,
    pub created: u64,
}

/// Realtime is not representable.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[repr(u8)]
pub enum PriorityClass {
    Idle = 1,
    BelowNormal = 2,
    Normal = 3,
    AboveNormal = 4,
    High = 5,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[repr(u8)]
pub enum SvcOp {
    Start = 1,
    Stop = 2,
    Restart = 3,
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct ServiceName(String);

impl ServiceName {
    /// 1..=256 UTF-16 units, no NUL, '\\' or '/'.
    pub fn new(s: &str) -> Result<ServiceName, AdminError> {
        ServiceName::check(s)?;
        Ok(ServiceName(s.to_owned()))
    }
    /// The decoder's form: takes the string it has already built.
    fn from_string(s: String) -> Result<ServiceName, AdminError> {
        ServiceName::check(&s)?;
        Ok(ServiceName(s))
    }
    fn check(s: &str) -> Result<(), AdminError> {
        let units = s.encode_utf16().count();
        if units == 0 || units > MAX_SERVICE_NAME {
            return Err(AdminError::Field("service name"));
        }
        if s.contains(['\0', '\\', '/']) {
            return Err(AdminError::Field("service name"));
        }
        Ok(())
    }
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[repr(u8)]
pub enum StartupSource {
    HklmRun = 1,
    HklmRun32 = 2,
    CommonStartup = 3,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct MachineStartupId {
    pub source: StartupSource,
    pub ordinal: u16,
    pub name_hash: u32,
}

/// FNV-1a 32 over the little-endian bytes of the ASCII-lowercased units.
pub fn name_hash(value_name_utf16: &[u16]) -> u32 {
    let mut h: u32 = 0x811C_9DC5;
    for &u in value_name_utf16 {
        let lower = if (u16::from(b'A')..=u16::from(b'Z')).contains(&u) { u + 0x20 } else { u };
        for b in lower.to_le_bytes() {
            h ^= u32::from(b);
            h = h.wrapping_mul(0x0100_0193);
        }
    }
    h
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct KeyList(Vec<ProcKey>);

impl KeyList {
    /// 1..=64 keys, no duplicate.
    pub fn new(keys: Vec<ProcKey>) -> Result<KeyList, AdminError> {
        if keys.is_empty() || keys.len() > MAX_KEYS {
            return Err(AdminError::Field("keys"));
        }
        let mut sorted = keys.clone();
        sorted.sort_unstable();
        if sorted.windows(2).any(|w| w.first() == w.get(1)) {
            return Err(AdminError::Field("keys"));
        }
        Ok(KeyList(keys))
    }
    pub fn as_slice(&self) -> &[ProcKey] {
        &self.0
    }
}

/// The comment after each variant is its frame kind.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum AdminRequest {
    /// 1
    Hello { proto: u16 },
    /// 2
    Terminate { keys: KeyList, tree: bool },
    /// 3
    SetPriority { key: ProcKey, class: PriorityClass },
    /// 4
    SetEfficiency { key: ProcKey, on: bool },
    /// 5: T9 only, after its security review; refused under AP.
    QueryCommandLine { key: ProcKey },
    /// 6: consent per action.
    ServiceControl { name: ServiceName, op: SvcOp },
    /// 7: consent per action.
    StartupSetEnabled { item: MachineStartupId, on: bool },
    /// 8: consent per action.
    RestartDock,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[repr(u8)]
pub enum DenyReason {
    Gone = 1,
    OtherSession = 2,
    Critical = 3,
    Protected = 4,
    OwnProcess = 5,
    RebootOnFailure = 6,
    UnknownService = 7,
    ServiceImagePath = 8,
    UnknownItem = 9,
    NeedsConsent = 10,
    RateLimited = 11,
    Disabled = 12,
    BadFrame = 13,
}

/// Kinds 0x81, 0x82, 0x83, 0x84.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum AdminReply {
    Ok,
    Text(Box<str>),
    Denied(DenyReason),
    Failed { win32: u32 },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AdminError {
    Header,
    Magic,
    Version,
    Kind(u16),
    Length,
    Field(&'static str),
    Trailing,
    TooLong,
}

fn priority_class(b: u8) -> Option<PriorityClass> {
    Some(match b {
        1 => PriorityClass::Idle,
        2 => PriorityClass::BelowNormal,
        3 => PriorityClass::Normal,
        4 => PriorityClass::AboveNormal,
        5 => PriorityClass::High,
        _ => return None,
    })
}

fn svc_op(b: u8) -> Option<SvcOp> {
    Some(match b {
        1 => SvcOp::Start,
        2 => SvcOp::Stop,
        3 => SvcOp::Restart,
        _ => return None,
    })
}

fn startup_source(b: u8) -> Option<StartupSource> {
    Some(match b {
        1 => StartupSource::HklmRun,
        2 => StartupSource::HklmRun32,
        3 => StartupSource::CommonStartup,
        _ => return None,
    })
}

fn deny_reason(b: u8) -> Option<DenyReason> {
    use DenyReason::*;
    Some(match b {
        1 => Gone,
        2 => OtherSession,
        3 => Critical,
        4 => Protected,
        5 => OwnProcess,
        6 => RebootOnFailure,
        7 => UnknownService,
        8 => ServiceImagePath,
        9 => UnknownItem,
        10 => NeedsConsent,
        11 => RateLimited,
        12 => Disabled,
        13 => BadFrame,
        _ => return None,
    })
}

fn request_kind(r: &AdminRequest) -> u16 {
    match r {
        AdminRequest::Hello { .. } => KIND_HELLO,
        AdminRequest::Terminate { .. } => KIND_TERMINATE,
        AdminRequest::SetPriority { .. } => KIND_SET_PRIORITY,
        AdminRequest::SetEfficiency { .. } => KIND_SET_EFFICIENCY,
        AdminRequest::QueryCommandLine { .. } => KIND_QUERY_COMMAND_LINE,
        AdminRequest::ServiceControl { .. } => KIND_SERVICE_CONTROL,
        AdminRequest::StartupSetEnabled { .. } => KIND_STARTUP_SET_ENABLED,
        AdminRequest::RestartDock => KIND_RESTART_DOCK,
    }
}

/// Appends one request frame; on an error (a value the decoder would refuse) out is left as it was.
pub fn encode_request(r: &AdminRequest, out: &mut Vec<u8>) -> Result<(), AdminError> {
    encode_frame(out, request_kind(r), MAX_REQUEST_PAYLOAD, |w| {
        match r {
            AdminRequest::Hello { proto } => w.extend_from_slice(&proto.to_le_bytes()),
            AdminRequest::Terminate { keys, tree } => {
                let keys = keys.as_slice();
                let n = u8::try_from(keys.len()).ok().filter(|n| (1..=MAX_KEYS).contains(&usize::from(*n)));
                w.push(n.ok_or(AdminError::Field("keys"))?);
                for k in keys {
                    put_key(w, k);
                }
                w.push(u8::from(*tree));
            }
            AdminRequest::SetPriority { key, class } => {
                put_key(w, key);
                w.push(*class as u8);
            }
            AdminRequest::SetEfficiency { key, on } => {
                put_key(w, key);
                w.push(u8::from(*on));
            }
            AdminRequest::QueryCommandLine { key } => put_key(w, key),
            AdminRequest::ServiceControl { name, op } => {
                put_utf16(w, name.as_str(), 1..=MAX_SERVICE_NAME, AdminError::Field("service name"))?;
                w.push(*op as u8);
            }
            AdminRequest::StartupSetEnabled { item, on } => {
                w.push(item.source as u8);
                w.extend_from_slice(&item.ordinal.to_le_bytes());
                w.extend_from_slice(&item.name_hash.to_le_bytes());
                w.push(u8::from(*on));
            }
            AdminRequest::RestartDock => {}
        }
        Ok(())
    })
}

/// One whole request frame, nothing before or after it.
pub fn decode_request(buf: &[u8]) -> Result<AdminRequest, AdminError> {
    let (h, payload) = Header::decode(buf, KIND_HELLO..=KIND_RESTART_DOCK, MAX_REQUEST_PAYLOAD)?;
    let mut c = Cursor { rest: payload };
    let r = match h.kind {
        KIND_HELLO => AdminRequest::Hello { proto: c.u16()? },
        KIND_TERMINATE => {
            let n = usize::from(c.u8()?);
            if n == 0 || n > MAX_KEYS {
                return Err(AdminError::Field("keys"));
            }
            let mut keys = Vec::with_capacity(n);
            for _ in 0..n {
                keys.push(c.key()?);
            }
            let tree = c.bool("tree")?;
            AdminRequest::Terminate { keys: KeyList::new(keys)?, tree }
        }
        KIND_SET_PRIORITY => {
            let key = c.key()?;
            let class = priority_class(c.u8()?).ok_or(AdminError::Field("class"))?;
            AdminRequest::SetPriority { key, class }
        }
        KIND_SET_EFFICIENCY => AdminRequest::SetEfficiency { key: c.key()?, on: c.bool("on")? },
        KIND_QUERY_COMMAND_LINE => AdminRequest::QueryCommandLine { key: c.key()? },
        KIND_SERVICE_CONTROL => {
            let units = usize::from(c.u16()?);
            if units == 0 || units > MAX_SERVICE_NAME {
                return Err(AdminError::Field("service name"));
            }
            let name = ServiceName::from_string(c.utf16(units, "service name")?)?;
            let op = svc_op(c.u8()?).ok_or(AdminError::Field("op"))?;
            AdminRequest::ServiceControl { name, op }
        }
        KIND_STARTUP_SET_ENABLED => {
            let source = startup_source(c.u8()?).ok_or(AdminError::Field("source"))?;
            let item = MachineStartupId { source, ordinal: c.u16()?, name_hash: c.u32()? };
            AdminRequest::StartupSetEnabled { item, on: c.bool("on")? }
        }
        KIND_RESTART_DOCK => AdminRequest::RestartDock,
        // Header::decode admits only the kinds above.
        other => return Err(AdminError::Kind(other)),
    };
    c.end()?;
    Ok(r)
}

/// Appends one reply frame; on an error (a Text over MAX_TEXT_UNITS: TooLong) out is left as it was.
pub fn encode_reply(r: &AdminReply, out: &mut Vec<u8>) -> Result<(), AdminError> {
    let kind = match r {
        AdminReply::Ok => KIND_OK,
        AdminReply::Text(_) => KIND_TEXT,
        AdminReply::Denied(_) => KIND_DENIED,
        AdminReply::Failed { .. } => KIND_FAILED,
    };
    encode_frame(out, kind, MAX_REPLY_PAYLOAD, |w| {
        match r {
            AdminReply::Ok => {}
            AdminReply::Text(t) => put_utf16(w, t, 0..=MAX_TEXT_UNITS, AdminError::TooLong)?,
            AdminReply::Denied(reason) => w.push(*reason as u8),
            AdminReply::Failed { win32 } => w.extend_from_slice(&win32.to_le_bytes()),
        }
        Ok(())
    })
}

/// One whole reply frame, nothing before or after it.
pub fn decode_reply(buf: &[u8]) -> Result<AdminReply, AdminError> {
    let (h, payload) = Header::decode(buf, KIND_OK..=KIND_FAILED, MAX_REPLY_PAYLOAD)?;
    let mut c = Cursor { rest: payload };
    let r = match h.kind {
        KIND_OK => AdminReply::Ok,
        KIND_TEXT => {
            let units = usize::from(c.u16()?);
            if units > MAX_TEXT_UNITS {
                return Err(AdminError::TooLong);
            }
            AdminReply::Text(c.utf16(units, "text")?.into_boxed_str())
        }
        KIND_DENIED => AdminReply::Denied(deny_reason(c.u8()?).ok_or(AdminError::Field("reason"))?),
        KIND_FAILED => AdminReply::Failed { win32: c.u32()? },
        // Header::decode admits only the kinds above.
        other => return Err(AdminError::Kind(other)),
    };
    c.end()?;
    Ok(r)
}

/// The one body of every fuzz harness: decode; on Ok, re-encode and assert the bytes are equal. Panics only on an
/// invariant breach, which is the finding. Every message carries the input in hex: on Windows a panic aborts through a
/// fast fail that libFuzzer cannot catch, so it writes no crash file and the log is where the input survives. Not
/// called by shipped code.
#[doc(hidden)]
pub fn fuzz_request(data: &[u8]) {
    if let Ok(r) = decode_request(data) {
        let mut v = Vec::new();
        let res = encode_request(&r, &mut v);
        assert!(res.is_ok(), "a decoded request does not encode: {res:?}; input {data:02x?}");
        assert!(v == data, "a decoded request encodes to other bytes; input {data:02x?}, re-encoded {v:02x?}");
    }
}

#[doc(hidden)]
pub fn fuzz_reply(data: &[u8]) {
    if let Ok(r) = decode_reply(data) {
        let mut v = Vec::new();
        let res = encode_reply(&r, &mut v);
        assert!(res.is_ok(), "a decoded reply does not encode: {res:?}; input {data:02x?}");
        assert!(v == data, "a decoded reply encodes to other bytes; input {data:02x?}, re-encoded {v:02x?}");
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::indexing_slicing)]

    use super::*;

    #[test]
    fn frozen_admin_types_compile() {
        let key = ProcKey { pid: 4, created: 9 };
        let keys = KeyList::new(vec![key]).unwrap();
        assert_eq!(keys.as_slice(), &[key]);
        assert!(KeyList::new(vec![]).is_err());
        assert!(KeyList::new(vec![key, key]).is_err());
        let name = ServiceName::new("Spooler").unwrap();
        assert_eq!(name.as_str(), "Spooler");
        assert!(ServiceName::new("").is_err());
        assert!(ServiceName::new("a/b").is_err());
        let item = MachineStartupId { source: StartupSource::HklmRun, ordinal: 1, name_hash: 2 };
        let _ = [
            AdminRequest::Hello { proto: PROTO },
            AdminRequest::Terminate { keys, tree: true },
            AdminRequest::SetPriority { key, class: PriorityClass::Normal },
            AdminRequest::SetEfficiency { key, on: true },
            AdminRequest::QueryCommandLine { key },
            AdminRequest::ServiceControl { name, op: SvcOp::Restart },
            AdminRequest::StartupSetEnabled { item, on: false },
            AdminRequest::RestartDock,
        ];
        let _ = [
            AdminReply::Ok,
            AdminReply::Text("x".into()),
            AdminReply::Denied(DenyReason::BadFrame),
            AdminReply::Failed { win32: 5 },
        ];
        let _ = (AdminError::Kind(1), MAGIC, MAX_REQUEST_PAYLOAD, MAX_REPLY_PAYLOAD);
        let _: fn(&AdminRequest, &mut Vec<u8>) -> Result<(), AdminError> = encode_request;
        let _: fn(&[u8]) -> Result<AdminRequest, AdminError> = decode_request;
        let _: fn(&AdminReply, &mut Vec<u8>) -> Result<(), AdminError> = encode_reply;
        let _: fn(&[u8]) -> Result<AdminReply, AdminError> = decode_reply;
        let _: fn(&[u16]) -> u32 = name_hash;
        let _: fn(&[u8]) = fuzz_request;
        let _: fn(&[u8]) = fuzz_reply;
    }
}
