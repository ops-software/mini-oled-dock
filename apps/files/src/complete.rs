//! Path completion (P3-files-app 2.2; FD-PATH fills it, F4.5): children enumerated off the UI thread.

use std::collections::{HashMap, HashSet};
use std::ffi::OsString;
use std::os::windows::ffi::OsStringExt;
use std::os::windows::fs::MetadataExt;
use std::path::PathBuf;

use oled_trust::frame::files::Location;
use oled_ui::node::Text;
use oled_ui::EventCx;
use windows::Win32::System::Diagnostics::Debug::{SetThreadErrorMode, SEM_FAILCRITICALERRORS};

use crate::drives::DriveKind;
use crate::listing::natural::natural_cmp;
use crate::listing::WidePath;
use crate::msg::JobDone;
use crate::parse::{parse, Env, Parsed, SHELL_KNOWN, SHELL_VIRTUAL};
use crate::pathline;
use crate::window::FilesScreen;

/// The rows under the field, and the folders one level lists.
pub const MAX_ROWS: usize = 8;
pub const MAX_DIRS: usize = 512;
/// A parent's folders are kept this long.
pub const CACHE_MS: u64 = 5_000;

#[derive(Debug)]
struct Pending {
    req: u64,
}

#[derive(Debug, Default)]
pub struct Completer {
    /// The text the rows are for: an answer for an older one is dropped.
    latest: String,
    /// The cache key of the typed parent folder.
    key: Option<String>,
    pending: Option<Pending>,
    /// Every request of ours still out, so that the sidebar and the siblings never take its answer.
    mine: HashSet<u64>,
    cache: HashMap<String, (u64, Vec<Box<str>>)>,
}

impl Completer {
    pub fn new() -> Completer {
        Completer::default()
    }

    /// The field is closed: answers still out are ignored.
    pub fn reset(&mut self) {
        self.latest.clear();
        self.key = None;
        self.pending = None;
    }
}

/// A path as the file system calls take it (the \\?\ form, no terminator).
pub fn fs_path(p: &WidePath) -> PathBuf {
    let mut u = p.raw();
    u.pop();
    PathBuf::from(OsString::from_wide(&u))
}

/// The folders of a directory, at most MAX_DIRS, natural order; hidden and system ones only when asked.
pub fn list_dirs(p: &WidePath, hidden: bool) -> Vec<Box<str>> {
    // SAFETY: this thread's error mode only; it keeps a missing medium from raising a system box.
    let _ = unsafe { SetThreadErrorMode(SEM_FAILCRITICALERRORS, None) };
    let mut names: Vec<String> = Vec::new();
    if let Ok(rd) = std::fs::read_dir(fs_path(p)) {
        for e in rd.flatten() {
            let Ok(m) = e.metadata() else { continue };
            let a = m.file_attributes();
            if a & 0x10 == 0 || (!hidden && a & 0x6 != 0) {
                continue;
            }
            names.push(e.file_name().to_string_lossy().into_owned());
            if names.len() >= MAX_DIRS {
                break;
            }
        }
    }
    names.sort_by(|a, b| natural_cmp(a.as_bytes(), b.as_bytes()));
    names.into_iter().map(String::into_boxed_str).collect()
}

/// Whether the folders of a typed parent may be listed while the user types: only on a drive this PC holds as its own
/// (`X:\...` on a letter `kind_of` knows that is not a network drive). A UNC or WebDAV path, a mapped network drive
/// and a letter the drive cache does not know are never listed: listing is a request to the server, which authenticates
/// as the user (the NTLM hash leaks) before anything was confirmed. Pure.
pub fn may_list(parent: &WidePath, kind_of: impl Fn(u8) -> Option<DriveKind>) -> bool {
    if parent.is_unc() {
        return false;
    }
    let letter = parent.units().first().and_then(|&u| u8::try_from(u).ok()).filter(u8::is_ascii_alphabetic);
    let kind = letter.and_then(|l| kind_of(l.to_ascii_uppercase()));
    kind.is_some_and(|k| !matches!(k, DriveKind::Network | DriveKind::Unknown))
}

/// Whether a folder may be listed through its path: no folder from the drive's root down to it, itself included, is a
/// link (a junction or a symbolic link: one can lead to a remote share). `is_link` answers for one folder (None: it
/// cannot be read, which refuses too); it is asked from the root down and never past the first link, so nothing is
/// read through one.
pub fn no_link_on_the_way(dir: &WidePath, mut is_link: impl FnMut(&WidePath) -> Option<bool>) -> bool {
    let mut chain: Vec<WidePath> = std::iter::successors(Some(dir.clone()), WidePath::parent).collect();
    chain.reverse();
    chain.iter().all(|d| is_link(d) == Some(false))
}

/// Whether a folder is a link (a reparse point that names another place: a junction or a symbolic link), read without
/// following it; None when it cannot be read.
fn is_link(d: &WidePath) -> Option<bool> {
    std::fs::symlink_metadata(fs_path(d)).ok().map(|m| m.file_type().is_symlink())
}

/// The typed text split after its last separator: the parent part (with the separator) and the name typed so far.
fn split(text: &str) -> (&str, &str) {
    match text.rfind(['\\', '/']) {
        Some(i) => text.split_at(i + 1),
        None => ("", text),
    }
}

/// The score of a fuzzy match: the characters of q in order inside c, those that start a segment counting more.
fn fuzzy(q: &str, c: &str) -> Option<u32> {
    let mut score = 0;
    let mut want = q.chars().flat_map(char::to_lowercase).peekable();
    let mut prev = '\\';
    for ch in c.chars() {
        let Some(w) = want.peek().copied() else { break };
        if ch.to_lowercase().next() == Some(w) {
            score += if matches!(prev, '\\' | '/' | ' ' | '-' | '_' | ':') { 3 } else { 1 };
            want.next();
        }
        prev = ch;
    }
    want.peek().is_none().then_some(score)
}

/// The rows for the typed text, in the order of 5.2.3, at most MAX_ROWS and without repeats: child folders of the
/// typed parent (prefix matches, then substring), %VARIABLES%, shell: names, then pins and history by fuzzy match.
/// Pure.
pub fn rows(text: &str, dirs: &[Box<str>], env: &Env, pool: &[String]) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    let add = |s: String, out: &mut Vec<String>| {
        if !out.iter().any(|o| o.eq_ignore_ascii_case(&s)) && !s.eq_ignore_ascii_case(text) {
            out.push(s);
        }
    };
    let (dir_text, typed) = split(text);
    if !text.starts_with('%') && !text.to_ascii_lowercase().starts_with("shell:") {
        let low = typed.to_lowercase();
        let (mut starts, mut inside) = (Vec::new(), Vec::new());
        for d in dirs {
            let l = d.to_lowercase();
            if l.starts_with(&low) {
                starts.push(d);
            } else if l.contains(&low) {
                inside.push(d);
            }
        }
        for d in starts.into_iter().chain(inside) {
            add(format!("{dir_text}{d}"), &mut out);
        }
    }
    if let Some(name) = text.strip_prefix('%').filter(|n| !n.contains('%')) {
        let mut names: Vec<&str> = env.vars.iter().map(|(n, _)| n.as_str()).filter(|n| !n.starts_with('=')).collect();
        names.sort_by_key(|n| n.to_uppercase());
        for n in names.into_iter().filter(|n| n.to_uppercase().starts_with(&name.to_uppercase())) {
            add(format!("%{n}%"), &mut out);
        }
    }
    let low = text.to_ascii_lowercase();
    // "sh" and "shell:" list every name, "shell:do" those that start so.
    let all = "shell:".starts_with(low.as_str());
    if text.len() >= 2 && (all || low.starts_with("shell:")) {
        for n in SHELL_KNOWN.iter().chain(SHELL_VIRTUAL.iter()) {
            let full = format!("shell:{n}");
            if all || full.starts_with(&low) {
                add(full, &mut out);
            }
        }
    }
    if !text.is_empty() {
        let mut scored: Vec<(u32, &String)> = pool.iter().filter_map(|c| fuzzy(text, c).map(|s| (s, c))).collect();
        scored.sort_by(|a, b| b.0.cmp(&a.0));
        for (_, c) in scored {
            add(c.clone(), &mut out);
        }
    }
    out.truncate(MAX_ROWS);
    out
}

/// The pins and the tab's history as completion candidates (history newest first).
fn pool_of(s: &FilesScreen) -> Vec<String> {
    let mut pool: Vec<String> = crate::pins::paths(&s.shared).iter().map(WidePath::to_string_lossy).collect();
    pool.extend(history_of(s, 100));
    pool
}

/// The folders the tab has been in, newest first.
pub fn history_of(s: &FilesScreen, max: usize) -> Vec<String> {
    let mut seen = HashSet::new();
    s.tabs
        .active()
        .nav
        .history(true, 100)
        .into_iter()
        .filter_map(|(_, l)| if let Location::Fs(p) = l { Some(p.to_string_lossy()) } else { None })
        .filter(|p| seen.insert(p.to_lowercase()))
        .take(max)
        .collect()
}

fn next_req(s: &FilesScreen) -> u64 {
    let r = s.shared.next_req.get();
    s.shared.next_req.set(r + 1);
    r
}

/// The rows of the text now typed, from what is cached.
fn show(s: &mut FilesScreen, cx: &mut EventCx) {
    let env = pathline::env_of(&s.shared);
    let me = &s.chrome.path.completer;
    let dirs: Vec<Box<str>> = me.key.as_ref().and_then(|k| me.cache.get(k)).map(|(_, d)| d.clone()).unwrap_or_default();
    let list = rows(&me.latest, &dirs, &env, &pool_of(s));
    let items = list.into_iter().map(Text::Owned).collect();
    s.chrome.path.set_completions(cx, items);
}

/// PathOut::CompletionRequested: the rows that need no file system show at once; the typed parent's folders come from
/// the cache (5 s) or from a Bg job on the parent's lane, and the rows are made again when they arrive. Only a parent
/// may_list allows is listed, and the job lists nothing through a link (no_link_on_the_way).
pub fn request(s: &mut FilesScreen, cx: &mut EventCx, text: &str) {
    let now = s.shared.services.clock.now_ms();
    let env = pathline::env_of(&s.shared);
    let (dir_text, _) = split(text);
    let parent = match (dir_text.is_empty(), pathline::cwd_of(s, &env)) {
        (true, Some(cwd)) if !text.starts_with('%') => Some(cwd),
        (true, _) => None,
        (false, Some(cwd)) => match parse(dir_text, &cwd, &env) {
            Parsed::Path(p) => Some(p),
            _ => None,
        },
        (false, None) => None,
    };
    let kind_of = |l: u8| s.shared.drives.borrow().all().iter().find(|d| d.letter == l).map(|d| d.kind);
    let parent = parent.filter(|p| may_list(p, kind_of));
    let key = parent.as_ref().map(|p| p.to_string_lossy().to_lowercase());
    let me = &mut s.chrome.path.completer;
    me.latest = text.to_string();
    me.pending = None;
    let fresh = key.as_ref().and_then(|k| me.cache.get(k)).is_some_and(|(at, _)| now.saturating_sub(*at) < CACHE_MS);
    me.key = key;
    if let (Some(p), false) = (parent, fresh) {
        let req = next_req(s);
        let me = &mut s.chrome.path.completer;
        me.pending = Some(Pending { req });
        me.mine.insert(req);
        let hidden = s.shared.opts.borrow().hidden_items;
        let lane = crate::bg::lane_of(&p, &s.shared.drives.borrow());
        s.shared.services.bg.run(
            s.win,
            lane,
            Box::new(move || {
                let dirs = if no_link_on_the_way(&p, is_link) { list_dirs(&p, hidden) } else { Vec::new() };
                Some(JobDone::Children { req, parent: p, dirs, done: true })
            }),
        );
    }
    show(s, cx);
}

/// A parent's folders arrived. True when the request was this module's (an answer for a text typed since is cached and
/// otherwise ignored).
pub fn on_children(
    s: &mut FilesScreen,
    cx: &mut EventCx,
    req: u64,
    parent: &WidePath,
    dirs: &[Box<str>],
    done: bool,
) -> bool {
    let now = s.shared.services.clock.now_ms();
    let me = &mut s.chrome.path.completer;
    if !me.mine.remove(&req) {
        return false;
    }
    me.cache.retain(|_, (at, _)| now.saturating_sub(*at) < CACHE_MS);
    if done {
        me.cache.insert(parent.to_string_lossy().to_lowercase(), (now, dirs.to_vec()));
    }
    if me.pending.as_ref().is_some_and(|p| p.req == req) {
        me.pending = None;
        show(s, cx);
    }
    true
}
