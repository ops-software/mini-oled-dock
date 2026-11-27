//! The Tasks launch contract (SPEC-tasks 5.5.8, ROADMAP X10, X11): the command line every launcher passes and the
//! one WM_COPYDATA frame a running Tasks accepts. Pure; #![forbid(unsafe_code)] holds (crate rule).
//!
//! Payload of Show (little-endian, through wire): page u8, sort u8, from_chord bool, select tag u8 (0 None, 1 Pid,
//! 2 Identity), then pid u32 or str (u32 byte length, at most MAX_IDENTITY_BYTES). Canonical: decode(encode(x)) == x
//! and every decoded frame re-encodes to the same bytes; anything else is a FrameError. The elevated dock only
//! encodes; only tasks.exe (medium) decodes (no elevated parsing).
use super::{wire, Codec, FrameError, Proto};

pub const INSTANCE: Proto = Proto { magic: u32::from_le_bytes(*b"TINS"), version: 1, max_payload: 1024 };
/// The top-level window; readiness = this class + image + create time (X10).
pub const WINDOW_CLASS: &str = "MiniOledTasks";
/// Single instance (advisory for the dock: R6).
pub const MUTEX_NAME: &str = "Local\\mini-oled-dock.tasks";
pub const AUMID: &str = "MiniOledDock.Tasks";
pub const EXE_NAME: &str = "tasks.exe";
pub const MAX_IDENTITY_BYTES: u32 = 512;

/// A page not shipped falls back to Processes.
#[repr(u8)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum Page {
    #[default]
    Processes = 1,
    Performance = 2,
    Startup = 3,
    Services = 4,
}

#[repr(u8)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum SortBy {
    #[default]
    Auto = 0,
    Cpu = 1,
    Memory = 2,
}

/// Identity: the dock's identity key (AUMID or key_from_exe_path).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub enum Select {
    #[default]
    None,
    Pid(u32),
    Identity(Box<str>),
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Show {
    pub page: Page,
    pub select: Select,
    pub sort: SortBy,
    pub from_chord: bool,
}

/// Range 0x0401-0x04FF (files uses 0x0301-0x03FF).
#[repr(u16)]
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum InstanceCmd {
    Show(Show) = 0x0401,
}

const KIND_SHOW: u16 = 0x0401;
const TAG_NONE: u8 = 0;
const TAG_PID: u8 = 1;
const TAG_IDENTITY: u8 = 2;

impl Codec for InstanceCmd {
    const PROTO: Proto = INSTANCE;
    fn kind(&self) -> u16 {
        match self {
            InstanceCmd::Show(_) => KIND_SHOW,
        }
    }
    fn put(&self, w: &mut wire::Writer) {
        match self {
            InstanceCmd::Show(s) => {
                w.u8(s.page as u8);
                w.u8(s.sort as u8);
                w.bool(s.from_chord);
                match &s.select {
                    Select::None => w.u8(TAG_NONE),
                    Select::Pid(pid) => {
                        w.u8(TAG_PID);
                        w.u32(*pid);
                    }
                    Select::Identity(id) => {
                        w.u8(TAG_IDENTITY);
                        w.str(id);
                    }
                }
            }
        }
    }
    fn get(kind: u16, r: &mut wire::Reader<'_>) -> Result<Self, FrameError> {
        match kind {
            KIND_SHOW => {
                let page = r.u8()?;
                let page = page_of(page).ok_or(FrameError::BadValue { field: "Show.page", value: u32::from(page) })?;
                let sort = r.u8()?;
                let sort = sort_of(sort).ok_or(FrameError::BadValue { field: "Show.sort", value: u32::from(sort) })?;
                let from_chord = r.bool("Show.from_chord")?;
                let select = match r.u8()? {
                    TAG_NONE => Select::None,
                    TAG_PID => Select::Pid(r.u32()?),
                    TAG_IDENTITY => {
                        let id = r.str("Show.select", MAX_IDENTITY_BYTES)?;
                        if !identity_ok(id) {
                            return Err(FrameError::Invalid { field: "Show.select" });
                        }
                        Select::Identity(id.into())
                    }
                    tag => return Err(FrameError::BadValue { field: "Show.select", value: u32::from(tag) }),
                };
                Ok(InstanceCmd::Show(Show { page, select, sort, from_chord }))
            }
            k => Err(FrameError::UnknownKind(k)),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ArgError {
    Unknown(String),
    Missing(&'static str),
    Bad(&'static str),
    Twice(&'static str),
}

const PAGE: &str = "--page";
const SELECT: &str = "--select";
const SORT: &str = "--sort";
const FROM_CHORD: &str = "--from-chord";

/// Canonical arguments, in this order, each only when not the default: --page <performance|startup|services>,
/// --select <pid>|<identity>, --sort <cpu|memory>, --from-chord.
pub fn show_args(s: &Show) -> Vec<String> {
    let mut out = Vec::new();
    if s.page != Page::Processes {
        out.push(PAGE.to_owned());
        out.push(page_name(s.page).to_owned());
    }
    match &s.select {
        Select::None => {}
        Select::Pid(pid) => {
            out.push(SELECT.to_owned());
            out.push(pid.to_string());
        }
        Select::Identity(id) => {
            out.push(SELECT.to_owned());
            out.push(id.to_string());
        }
    }
    if s.sort != SortBy::Auto {
        out.push(SORT.to_owned());
        out.push(sort_name(s.sort).to_owned());
    }
    if s.from_chord {
        out.push(FROM_CHORD.to_owned());
    }
    out
}

/// Inverse of show_args; accepts the flags in any order, each at most once; an all-digit select of at most 10 digits
/// that fits u32 is Pid, anything else is Identity (1..=512 UTF-8 bytes, no control character, not starting with "--").
///
/// The default page and sort may be named too ("processes", "auto"); show_args leaves them out.
pub fn parse_show(args: &[String]) -> Result<Show, ArgError> {
    let mut show = Show::default();
    let (mut page, mut select, mut sort, mut chord) = (false, false, false, false);
    let mut it = args.iter();
    while let Some(arg) = it.next() {
        match arg.as_str() {
            PAGE => {
                once(&mut page, PAGE)?;
                let v = it.next().ok_or(ArgError::Missing(PAGE))?;
                show.page = page_named(v).ok_or(ArgError::Bad(PAGE))?;
            }
            SELECT => {
                once(&mut select, SELECT)?;
                let v = it.next().ok_or(ArgError::Missing(SELECT))?;
                show.select = select_of(v).ok_or(ArgError::Bad(SELECT))?;
            }
            SORT => {
                once(&mut sort, SORT)?;
                let v = it.next().ok_or(ArgError::Missing(SORT))?;
                show.sort = sort_named(v).ok_or(ArgError::Bad(SORT))?;
            }
            FROM_CHORD => {
                once(&mut chord, FROM_CHORD)?;
                show.from_chord = true;
            }
            other => return Err(ArgError::Unknown(other.to_owned())),
        }
    }
    Ok(show)
}

/// The WM_COPYDATA parameters: dwData = INSTANCE.magic as usize, lpData = one frame of encode(&InstanceCmd::Show(..)).
pub fn copydata_tag() -> usize {
    INSTANCE.magic as usize
}

fn once(seen: &mut bool, flag: &'static str) -> Result<(), ArgError> {
    if std::mem::replace(seen, true) {
        return Err(ArgError::Twice(flag));
    }
    Ok(())
}

fn page_of(v: u8) -> Option<Page> {
    [Page::Processes, Page::Performance, Page::Startup, Page::Services].into_iter().find(|p| *p as u8 == v)
}

fn sort_of(v: u8) -> Option<SortBy> {
    [SortBy::Auto, SortBy::Cpu, SortBy::Memory].into_iter().find(|s| *s as u8 == v)
}

fn page_name(p: Page) -> &'static str {
    match p {
        Page::Processes => "processes",
        Page::Performance => "performance",
        Page::Startup => "startup",
        Page::Services => "services",
    }
}

fn sort_name(s: SortBy) -> &'static str {
    match s {
        SortBy::Auto => "auto",
        SortBy::Cpu => "cpu",
        SortBy::Memory => "memory",
    }
}

fn page_named(v: &str) -> Option<Page> {
    [Page::Processes, Page::Performance, Page::Startup, Page::Services].into_iter().find(|p| page_name(*p) == v)
}

fn sort_named(v: &str) -> Option<SortBy> {
    [SortBy::Auto, SortBy::Cpu, SortBy::Memory].into_iter().find(|s| sort_name(*s) == v)
}

/// Pid for an all-digit value of at most 10 digits that fits u32; else Identity when identity_ok.
fn select_of(v: &str) -> Option<Select> {
    if !v.is_empty() && v.len() <= 10 && v.bytes().all(|b| b.is_ascii_digit()) {
        if let Ok(pid) = v.parse::<u32>() {
            return Some(Select::Pid(pid));
        }
    }
    identity_ok(v).then(|| Select::Identity(v.into()))
}

/// 1..=MAX_IDENTITY_BYTES UTF-8 bytes, no control character, not starting with "--" (so no flag is read as a value).
fn identity_ok(v: &str) -> bool {
    !v.is_empty() && v.len() <= MAX_IDENTITY_BYTES as usize && !v.chars().any(char::is_control) && !v.starts_with("--")
}
