//! Recorded traces and the verdict replayed on them.
//!
//! A trace is UTF-8 text, one record per line, fields separated by one space, floats with up to three decimals:
//! a header line, then ticks (`t`) each followed by its groups (`g`). Tokens are opaque (a letter and two to four
//! digits): a trace never carries a name, a path or a process id.
use crate::model::group::{AppKey, IdentityId};
use crate::model::verdict::{judge, GroupStat, SysState, Verdict, VerdictState};
use crate::model::view::Section;
use std::collections::HashMap;
use std::fmt::Write as _;

#[derive(Clone, Debug, PartialEq)]
pub struct TraceHeader {
    pub version: u32,
    pub tick_ms: u32,
    pub logical_cpus: u32,
    pub total_phys: u64,
    pub build: u32,
    pub note: String,
}

#[derive(Clone, Debug, PartialEq)]
pub struct TraceGroup {
    pub token: String,
    pub section: Section,
    pub cpu: f32,
    pub mem: u64,
    pub io_bps: f32,
    pub hung: bool,
    pub has_windows: bool,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Tick {
    pub t_ms: u64,
    pub sys: SysState,
    pub groups: Vec<TraceGroup>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Trace {
    pub header: TraceHeader,
    pub ticks: Vec<Tick>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TraceError {
    /// Line 1 is missing or malformed.
    Header,
    /// A line that is not a record, is out of place or has the wrong number of fields (1-based).
    Line(usize),
    /// A field that does not parse or is out of range (1-based line, field name).
    Field(usize, &'static str),
}

const NOTE_MAX: usize = 32;

/// A token is one lower-case letter and two to four digits.
fn valid_token(s: &str) -> bool {
    let b = s.as_bytes();
    (3..=5).contains(&b.len()) && b[0].is_ascii_lowercase() && b[1..].iter().all(u8::is_ascii_digit)
}

fn valid_note(s: &str) -> bool {
    s.len() <= NOTE_MAX && s.bytes().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == b'_' || c == b'-')
}

/// Digits with at most one '.' and at most three decimals; no sign, no exponent, no "nan" or "inf".
fn parse_dec(s: &str) -> Option<f32> {
    let (int, frac) = match s.split_once('.') {
        Some((i, f)) => (i, f),
        None => (s, ""),
    };
    let digits = |p: &str| p.bytes().all(|c| c.is_ascii_digit());
    if int.is_empty() || !digits(int) || !digits(frac) || frac.len() > 3 || (s.contains('.') && frac.is_empty()) {
        return None;
    }
    s.parse::<f32>().ok().filter(|v| v.is_finite())
}

/// The value of the next field when it is named key.
fn kv<'a>(it: &mut std::str::Split<'a, char>, key: &str) -> Option<&'a str> {
    it.next()?.strip_prefix(key)?.strip_prefix('=')
}

fn parse_header(line: &str) -> Result<TraceHeader, TraceError> {
    let bad = || TraceError::Header;
    let mut it = line.split(' ');
    if it.next() != Some("tasks-trace") || it.next() != Some("1") {
        return Err(bad());
    }
    let tick_ms = kv(&mut it, "tick_ms").and_then(|v| v.parse().ok()).ok_or_else(bad)?;
    let logical_cpus: u32 = kv(&mut it, "cpus").and_then(|v| v.parse().ok()).ok_or_else(bad)?;
    let total_phys = kv(&mut it, "phys").and_then(|v| v.parse().ok()).ok_or_else(bad)?;
    let build = kv(&mut it, "build").and_then(|v| v.parse().ok()).ok_or_else(bad)?;
    let note = kv(&mut it, "note").filter(|v| valid_note(v)).ok_or_else(bad)?;
    if it.next().is_some() || logical_cpus == 0 {
        return Err(bad());
    }
    Ok(TraceHeader { version: 1, tick_ms, logical_cpus, total_phys, build, note: note.to_string() })
}

fn parse_tick(n: usize, rest: std::str::Split<'_, char>, cpus: u32, last_ms: Option<u64>) -> Result<Tick, TraceError> {
    let mut it = rest;
    let t_ms: u64 = it.next().and_then(|v| v.parse().ok()).ok_or(TraceError::Field(n, "t"))?;
    if last_ms.is_some_and(|l| t_ms < l) {
        return Err(TraceError::Field(n, "t"));
    }
    let cpu_pct = kv(&mut it, "cpu").and_then(parse_dec).ok_or(TraceError::Field(n, "cpu"))?;
    let mem_load_pct: u8 =
        kv(&mut it, "mem").and_then(|v| v.parse().ok()).filter(|v| *v <= 100).ok_or(TraceError::Field(n, "mem"))?;
    let commit_pct = kv(&mut it, "commit").and_then(parse_dec).ok_or(TraceError::Field(n, "commit"))?;
    let dpc_irq_pct = kv(&mut it, "dpc").and_then(parse_dec).ok_or(TraceError::Field(n, "dpc"))?;
    let paused = match kv(&mut it, "paused") {
        Some("0") => false,
        Some("1") => true,
        _ => return Err(TraceError::Field(n, "paused")),
    };
    let read_error = match kv(&mut it, "err") {
        Some("-") => None,
        Some(v) => Some(v.parse::<i32>().map_err(|_| TraceError::Field(n, "err"))?),
        None => return Err(TraceError::Field(n, "err")),
    };
    if it.next().is_some() {
        return Err(TraceError::Line(n));
    }
    let sys = SysState { cpu_pct, mem_load_pct, commit_pct, dpc_irq_pct, logical_cpus: cpus, paused, read_error };
    Ok(Tick { t_ms, sys, groups: Vec::new() })
}

fn parse_flag(s: Option<&str>) -> Option<bool> {
    match s? {
        "0" => Some(false),
        "1" => Some(true),
        _ => None,
    }
}

fn parse_group(n: usize, rest: std::str::Split<'_, char>) -> Result<TraceGroup, TraceError> {
    let mut it = rest;
    let token = it.next().filter(|t| valid_token(t)).ok_or(TraceError::Field(n, "token"))?;
    let section = match it.next() {
        Some("A") => Section::Apps,
        Some("B") => Section::Background,
        Some("W") => Section::Windows,
        _ => return Err(TraceError::Field(n, "section")),
    };
    let cpu = it.next().and_then(parse_dec).ok_or(TraceError::Field(n, "cpu"))?;
    let mem = it.next().and_then(|v| v.parse().ok()).ok_or(TraceError::Field(n, "mem"))?;
    let io_bps = it.next().and_then(parse_dec).ok_or(TraceError::Field(n, "io"))?;
    let hung = parse_flag(it.next()).ok_or(TraceError::Field(n, "hung"))?;
    let has_windows = parse_flag(it.next()).ok_or(TraceError::Field(n, "windows"))?;
    if it.next().is_some() {
        return Err(TraceError::Line(n));
    }
    Ok(TraceGroup { token: token.to_string(), section, cpu, mem, io_bps, hung, has_windows })
}

pub fn parse_trace(text: &str) -> Result<Trace, TraceError> {
    let mut lines = text.lines();
    let header = parse_header(lines.next().ok_or(TraceError::Header)?)?;
    let mut ticks: Vec<Tick> = Vec::new();
    for (i, line) in lines.enumerate() {
        let n = i + 2;
        let mut it = line.split(' ');
        match it.next() {
            Some("t") => {
                let tick = parse_tick(n, it, header.logical_cpus, ticks.last().map(|t| t.t_ms))?;
                ticks.push(tick);
            }
            Some("g") => {
                let group = parse_group(n, it)?;
                let tick = ticks.last_mut().ok_or(TraceError::Line(n))?;
                if tick.groups.iter().any(|g| g.token == group.token) {
                    return Err(TraceError::Field(n, "token"));
                }
                tick.groups.push(group);
            }
            _ => return Err(TraceError::Line(n)),
        }
    }
    Ok(Trace { header, ticks })
}

/// Up to three decimals, trailing zeros dropped; a negative or non-finite value is written as 0.
fn put_dec(out: &mut String, v: f32) {
    let v = if v.is_finite() && v > 0.0 { v } else { 0.0 };
    let start = out.len();
    let _ = write!(out, "{v:.3}");
    while out.len() > start && out.ends_with('0') {
        out.pop();
    }
    if out.ends_with('.') {
        out.pop();
    }
}

pub fn write_trace(t: &Trace, out: &mut String) {
    let h = &t.header;
    let _ = writeln!(
        out,
        "tasks-trace {} tick_ms={} cpus={} phys={} build={} note={}",
        h.version, h.tick_ms, h.logical_cpus, h.total_phys, h.build, h.note
    );
    for tick in &t.ticks {
        let s = &tick.sys;
        let _ = write!(out, "t {} cpu=", tick.t_ms);
        put_dec(out, s.cpu_pct);
        let _ = write!(out, " mem={} commit=", s.mem_load_pct);
        put_dec(out, s.commit_pct);
        out.push_str(" dpc=");
        put_dec(out, s.dpc_irq_pct);
        let _ = write!(out, " paused={} err=", u8::from(s.paused));
        match s.read_error {
            Some(e) => {
                let _ = write!(out, "{e}");
            }
            None => out.push('-'),
        }
        out.push('\n');
        for g in &tick.groups {
            let sec = match g.section {
                Section::Apps => 'A',
                Section::Background => 'B',
                Section::Windows => 'W',
            };
            let _ = write!(out, "g {} {sec} ", g.token);
            put_dec(out, g.cpu);
            let _ = write!(out, " {} ", g.mem);
            put_dec(out, g.io_bps);
            let _ = writeln!(out, " {} {}", u8::from(g.hung), u8::from(g.has_windows));
        }
    }
}

/// Token to its number, by first appearance.
fn token_ids(t: &Trace) -> HashMap<&str, u32> {
    let mut ids = HashMap::new();
    for g in t.ticks.iter().flat_map(|tick| tick.groups.iter()) {
        let next = ids.len() as u32;
        ids.entry(g.token.as_str()).or_insert(next);
    }
    ids
}

/// (t_ms, verdict, more) per tick; tokens become AppKey::App(IdentityId(n)) by first appearance.
pub fn replay(t: &Trace) -> Vec<(u64, Verdict, u8)> {
    let Some(first) = t.ticks.first() else {
        return Vec::new();
    };
    let ids = token_ids(t);
    let mut state = VerdictState::new(first.t_ms);
    let mut stats: Vec<GroupStat> = Vec::new();
    let mut out = Vec::with_capacity(t.ticks.len());
    for tick in &t.ticks {
        stats.clear();
        stats.extend(tick.groups.iter().filter_map(|g| {
            Some(GroupStat {
                key: AppKey::App(IdentityId(*ids.get(g.token.as_str())?)),
                section: g.section,
                cpu: g.cpu,
                mem: g.mem,
                io_bps: g.io_bps,
                hung: g.hung,
                has_windows: g.has_windows,
            })
        }));
        let sys = SysState { logical_cpus: t.header.logical_cpus, ..tick.sys };
        let verdict = judge(&sys, &stats, &mut state, tick.t_ms);
        out.push((tick.t_ms, verdict, state.more()));
    }
    out
}

/// What the line shows: the variant and the app it names.
type Shown = (std::mem::Discriminant<Verdict>, Option<AppKey>);

fn shown(v: &Verdict) -> Shown {
    let app = match *v {
        Verdict::NotResponding { app, .. } | Verdict::CoreBound { app } => Some(app),
        Verdict::MemoryFull { top, .. } | Verdict::CpuHog { top, .. } | Verdict::IoHeavy { top, .. } => Some(top),
        Verdict::CpuBusy { busiest, .. } | Verdict::Calm { busiest, .. } => busiest,
        Verdict::Looking | Verdict::Paused | Verdict::ReadError { .. } | Verdict::Drivers { .. } => None,
    };
    (std::mem::discriminant(v), app)
}

/// A verdict that reports a problem (rules 1 to 7), not Looking, Paused, a read error or Calm.
fn is_issue(v: &Verdict) -> bool {
    !matches!(v, Verdict::Looking | Verdict::Paused | Verdict::ReadError { .. } | Verdict::Calm { .. })
}

/// Flaps: a change of what the line shows that is undone within window_ms. Counted: an issue variant giving way to
/// another issue variant and coming back (A to B to A), and the named app of one variant changing and changing back.
/// A calm line that turns into an issue and back is the issue's own life, not a flap.
pub fn flaps(v: &[(u64, Verdict, u8)], window_ms: u64) -> u32 {
    // Runs of equal shown identity, with the time each began.
    let mut runs: Vec<(Shown, &Verdict, u64)> = Vec::new();
    for (t, verdict, _) in v {
        let id = shown(verdict);
        if runs.last().is_none_or(|r| r.0 != id) {
            runs.push((id, verdict, *t));
        }
    }
    let mut n = 0;
    for w in runs.windows(3) {
        let (a, b, c) = (&w[0], &w[1], &w[2]);
        let same_variant = a.0 .0 == b.0 .0;
        let both_issues = is_issue(a.1) && is_issue(b.1);
        if a.0 == c.0 && a.0 != b.0 && (same_variant || both_issues) && c.2.saturating_sub(b.2) <= window_ms {
            n += 1;
        }
    }
    n
}

pub fn token_key(t: &Trace, token: &str) -> Option<AppKey> {
    token_ids(t).get(token).map(|n| AppKey::App(IdentityId(*n)))
}
