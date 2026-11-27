//! The agent's records: tags are fixed forever (F-P8). Pure, no unsafe.
#![forbid(unsafe_code)]
// PF1 freeze: the types below are frozen; the codecs arrive with AG1.
#![allow(dead_code, unused_variables)]

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
pub fn encode_request(r: &Request) -> Result<Vec<u8>, FrameError> {
    unimplemented!("PF1 stub")
}
pub fn decode_request(b: &[u8]) -> Result<Request, FrameError> {
    unimplemented!("PF1 stub")
}
pub fn encode_reply(r: &Reply) -> Result<Vec<u8>, FrameError> {
    unimplemented!("PF1 stub")
}
pub fn decode_reply(b: &[u8]) -> Result<Reply, FrameError> {
    unimplemented!("PF1 stub")
}
