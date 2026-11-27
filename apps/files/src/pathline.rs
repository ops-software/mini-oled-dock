//! The path line (P3-files-app 2.2; FB-CHROME the breadcrumb, FD-PATH the edit, F1b.2 and F4.4-F4.6).

use oled_trust::frame::files::{Location, ShellReq};
use oled_ui::node::Text;
use oled_ui::widgets::{PathBar, PathOut, PathSegment};
use oled_ui::{Anchor, Event, EventCx, Key, LayoutCx, MenuId, PaintCx, RectP, Reply, UiLang, Widget, WidgetId};

use crate::complete::{self, Completer};
use crate::handoff;
use crate::keys::Cmd;
use crate::listing::collate::utf16_to_wtf8;
use crate::listing::WidePath;
use crate::msg::{JobDone, KnownName};
use crate::nav::Page;
use crate::parse::{parse, Env, Parsed};
use crate::services::ReqOwner;
use crate::toolbar::place_name;
use crate::tr::{k, t, tf};
use crate::window::{self, FilesScreen, Shared};

/// The longest command the host takes (ShellReq::RunTyped text, 8 KiB).
const COMMAND_MAX: usize = 8 * 1024;

pub(crate) const ID_PATH: WidgetId = WidgetId(23);

/// The height of the bar's cell: the toolbar's button row (logical px).
const CELL_H: i32 = 28;

/// One segment of the shown location: its label, the place a click on it goes to, and its own folder name (what the
/// level below it focuses when a click comes back up to this one).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Seg {
    pub label: String,
    pub at: Location,
    pub name: Box<[u8]>,
}

/// The breadcrumb of a location: a drive shows "C:" then each folder, a UNC root "\\server\share" as one segment,
/// Home the app's word for it. Pure.
pub fn segments(at: &Location, lang: UiLang) -> Vec<Seg> {
    let root = match at {
        Location::Fs(p) => p,
        Location::Search { root, .. } => root,
        Location::Home => {
            let label = place_name(at, lang);
            return vec![Seg { label, at: Location::Home, name: Box::default() }];
        }
        Location::Options => return Vec::new(),
    };
    let mut chain = Vec::new();
    let mut cur = Some(root.clone());
    while let Some(p) = cur {
        cur = p.parent();
        chain.push(p);
    }
    chain.reverse();
    chain
        .into_iter()
        .map(|p| {
            let (label, name) = match p.file_name() {
                Some(u) => {
                    let mut w = Vec::new();
                    utf16_to_wtf8(u, &mut w);
                    (String::from_utf16_lossy(u), w)
                }
                None => {
                    let full = p.to_string_lossy();
                    let label = full.trim_end_matches('\\').to_string();
                    (label.clone(), label.into_bytes())
                }
            };
            Seg { label, at: Location::Fs(p), name: name.into_boxed_slice() }
        })
        .collect()
}

/// The whole path as the edit mode starts with it.
fn edit_text(at: &Location, lang: UiLang) -> String {
    match at {
        Location::Fs(p) => p.to_string_lossy(),
        Location::Search { root, .. } => root.to_string_lossy(),
        other => place_name(other, lang),
    }
}

/// A typed path waiting for its Stat: folder, file or neither. command: the text that runs when nothing is there.
struct PendingStat {
    req: u64,
    text: String,
    path: WidePath,
    command: Option<(String, WidePath)>,
}

/// A siblings list asked for: the folder whose subfolders it shows and the name of the child the path goes through.
struct Siblings {
    req: u64,
    parent: WidePath,
    current: Box<[u8]>,
    at: Anchor,
}

pub struct PathLine {
    bar: PathBar,
    /// The location the bar shows and its segments (a segment's key is its index).
    shown: Option<Location>,
    segs: Vec<Seg>,
    pub(crate) completer: Completer,
    /// The refusal shown under the line.
    error: Option<String>,
    stat: Option<PendingStat>,
    siblings: Option<Siblings>,
    list: Option<OpenList>,
}

/// The open siblings list: its id, the folder and the names it shows, the highlighted one (the keys stay with us).
struct OpenList {
    id: MenuId,
    parent: WidePath,
    names: Vec<Box<str>>,
    hl: Option<usize>,
}

impl OpenList {
    fn texts(&self) -> Vec<Text> {
        self.names.iter().map(|d| Text::Owned(d.to_string())).collect()
    }
}

impl std::fmt::Debug for PathLine {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("PathLine")
    }
}

impl Default for PathLine {
    fn default() -> PathLine {
        PathLine::new()
    }
}

impl PathLine {
    pub fn new() -> PathLine {
        PathLine {
            bar: PathBar::new(ID_PATH),
            shown: None,
            segs: Vec::new(),
            completer: Completer::new(),
            error: None,
            stat: None,
            siblings: None,
            list: None,
        }
    }

    /// The segments the bar shows now.
    pub fn segs(&self) -> &[Seg] {
        &self.segs
    }

    /// The completion rows under the edit field.
    pub(crate) fn set_completions(&mut self, cx: &mut EventCx, items: Vec<Text>) {
        self.bar.set_completions(cx, items);
    }

    fn path_of(&self, lang: UiLang, at: &Location) -> (Vec<Seg>, Vec<PathSegment>, String) {
        let segs = segments(at, lang);
        let shown = segs
            .iter()
            .enumerate()
            .map(|(i, s)| PathSegment {
                key: i as u64,
                label: Text::Owned(s.label.clone()),
                tip: Some(Text::Owned(edit_text(&s.at, lang))),
            })
            .collect();
        (segs, shown, edit_text(at, lang))
    }

    /// Layout-time sync: the bar follows the active tab without an EventCx.
    fn follow(&mut self, lang: UiLang, at: &Location) {
        if self.shown.as_ref() == Some(at) {
            return;
        }
        let (segs, shown, text) = self.path_of(lang, at);
        let bar = std::mem::replace(&mut self.bar, PathBar::new(ID_PATH));
        self.bar = bar.with_path(shown, text);
        self.segs = segs;
        self.shown = Some(at.clone());
    }
}

/// The bar fills the location area, as high as the toolbar's buttons.
pub fn layout(s: &mut FilesScreen, cx: &mut LayoutCx, r: RectP) {
    let lang = s.shared.lang.get();
    let at = s.tabs.active().nav.location().clone();
    let offline = *s.tabs.active().nav.page() == Page::Offline;
    let h = cx.px(CELL_H);
    let top = (r.t + r.b) / 2 - h / 2;
    let me = &mut s.chrome.path;
    me.follow(lang, &at);
    me.bar.set_offline(offline);
    me.bar.layout(cx, RectP { t: top, b: top + h, ..r });
}

pub fn paint(s: &FilesScreen, cx: &mut PaintCx) {
    s.chrome.path.bar.paint(cx);
}

/// The bar shows `at`.
pub fn set_location(s: &mut FilesScreen, cx: &mut EventCx, at: &Location) {
    let lang = s.shared.lang.get();
    let me = &mut s.chrome.path;
    if me.shown.as_ref() == Some(at) {
        return;
    }
    let (segs, shown, text) = me.path_of(lang, at);
    me.bar.set_path(cx, shown, text);
    me.segs = segs;
    me.shown = Some(at.clone());
}

/// The bar's events: it takes the pointer and keys of its own nodes; its answers go to on_out.
pub fn event(s: &mut FilesScreen, cx: &mut EventCx, ev: &Event) -> Option<Reply> {
    if s.chrome.path.list.is_some() {
        if let Some(r) = list_event(s, cx, ev) {
            return Some(r);
        }
    }
    let out = s.chrome.path.bar.event(cx, ev)?;
    Some(on_out(s, cx, out))
}

/// Goes to the folder the list shows at `index`.
fn pick(s: &mut FilesScreen, cx: &mut EventCx, l: &OpenList, index: usize) {
    let Some(name) = l.names.get(index) else { return };
    if let Ok(to) = l.parent.join(&name.encode_utf16().collect::<Vec<u16>>()) {
        window::navigate(s, cx, Location::Fs(to), None);
    }
}

/// The open siblings list: a click picks (ListPicked), Up and Down move the highlight, Enter picks, Esc closes, a
/// letter jumps to the next name that starts with it.
fn list_event(s: &mut FilesScreen, cx: &mut EventCx, ev: &Event) -> Option<Reply> {
    let l = s.chrome.path.list.as_mut()?;
    let n = l.names.len();
    match ev {
        Event::ListPicked { menu, index } if *menu == l.id => {
            let l = s.chrome.path.list.take()?;
            pick(s, cx, &l, *index);
            Some(Reply::Relayout)
        }
        Event::MenuClosed(menu) if *menu == l.id => {
            s.chrome.path.list = None;
            None
        }
        Event::Key { key, down: true, .. } if *key == Key::DOWN || *key == Key::UP => {
            let down = *key == Key::DOWN;
            l.hl = Some(match (l.hl, down) {
                (None, true) => 0,
                (None, false) | (Some(0), false) => n - 1,
                (Some(i), true) => (i + 1) % n,
                (Some(i), false) => i - 1,
            });
            cx.update_list(l.id, l.texts(), l.hl);
            cx.consume();
            Some(Reply::Redraw)
        }
        Event::Key { key, down: true, .. } if *key == Key::ENTER => {
            cx.consume();
            let l = s.chrome.path.list.take()?;
            cx.close_menus();
            pick(s, cx, &l, l.hl.unwrap_or(0));
            Some(Reply::Relayout)
        }
        Event::Key { key, down: true, .. } if *key == Key::ESC => {
            cx.consume();
            s.chrome.path.list = None;
            cx.close_menus();
            Some(Reply::Redraw)
        }
        Event::Text(c) if !c.is_control() => {
            let from = l.hl.map_or(0, |i| i + 1);
            let c = c.to_lowercase().next();
            let hit = (0..n).map(|d| (from + d) % n).find(|i| l.names[*i].to_lowercase().chars().next() == c);
            if hit.is_some() {
                l.hl = hit;
                cx.update_list(l.id, l.texts(), l.hl);
                cx.consume();
            }
            Some(Reply::Redraw)
        }
        _ => None,
    }
}

/// The environment the typed path is read in: the process variables, the profile and the cached known folders.
pub fn env_of(shared: &Shared) -> Env {
    let known: Vec<(KnownName, WidePath)> = shared
        .session
        .borrow()
        .known
        .iter()
        .filter_map(|(name, path)| {
            let n = match name.as_str() {
                "desktop" => KnownName::Desktop,
                "downloads" => KnownName::Downloads,
                "documents" => KnownName::Documents,
                "pictures" => KnownName::Pictures,
                "music" => KnownName::Music,
                "videos" => KnownName::Videos,
                "profile" => KnownName::Profile,
                _ => return None,
            };
            Some((n, WidePath::parse(path).ok()?))
        })
        .collect();
    let profile = known.iter().find(|(n, _)| *n == KnownName::Profile).map(|(_, p)| p.clone());
    let vars = std::env::vars_os().filter_map(|(k, v)| Some((k.into_string().ok()?, v.into_string().ok()?))).collect();
    Env { vars, profile, known }
}

/// The folder relative paths are read against: the tab's own, else the profile folder.
pub fn cwd_of(s: &FilesScreen, env: &Env) -> Option<WidePath> {
    match s.tabs.active().nav.location() {
        Location::Fs(p) | Location::Search { root: p, .. } => Some(p.clone()),
        _ => env.profile.clone().or_else(|| WidePath::parse(r"C:\").ok()),
    }
}

fn next_req(s: &FilesScreen) -> u64 {
    let r = s.shared.next_req.get();
    s.shared.next_req.set(r + 1);
    r
}

/// FA-WIN's NotFound, and the path field's own refusals: the reason shows under the path line until the next
/// navigation; None clears it.
pub fn set_error(s: &mut FilesScreen, cx: &mut EventCx, msg: Option<Text>) {
    s.chrome.path.error = msg.as_ref().map(ToString::to_string);
    s.chrome.path.bar.set_error(cx, msg);
}

/// True while the path field is shown.
pub fn editing(s: &FilesScreen) -> bool {
    s.chrome.path.bar.is_editing()
}

/// The refusal shown under the path line, if any.
pub fn error_of(s: &FilesScreen) -> Option<String> {
    s.chrome.path.error.clone()
}

fn not_found(s: &mut FilesScreen, cx: &mut EventCx, text: &str) {
    let msg = tf(k::FILES_STATE_NOT_FOUND, cx.lang(), &[("path", text)]);
    set_error(s, cx, Some(msg));
}

/// A click on a segment goes to that ancestor and focuses the folder we came from; Refresh is Cmd::Refresh; a
/// chevron asks for the level's subfolders; the edit mode's text is parsed on Enter and completed while typed.
pub fn on_out(s: &mut FilesScreen, cx: &mut EventCx, out: PathOut) -> Reply {
    match out {
        PathOut::Navigate(key) => {
            let segs = &s.chrome.path.segs;
            let Some(to) = usize::try_from(key).ok().and_then(|i| segs.get(i)) else { return Reply::None };
            let focus = usize::try_from(key).ok().and_then(|i| segs.get(i + 1)).map(|c| c.name.clone());
            let at = to.at.clone();
            window::navigate(s, cx, at, focus);
            Reply::Relayout
        }
        PathOut::Refresh => crate::keys::run(s, cx, Cmd::Refresh),
        PathOut::EditStarted => {
            set_error(s, cx, None);
            Reply::Relayout
        }
        PathOut::EditCancelled => {
            s.chrome.path.completer.reset();
            Reply::Relayout
        }
        PathOut::CompletionRequested(text) => {
            complete::request(s, cx, &text);
            Reply::Relayout
        }
        PathOut::EditCommitted(text) => {
            s.chrome.path.completer.reset();
            commit(s, cx, &text);
            Reply::Relayout
        }
        PathOut::SiblingsRequested(key, at) => {
            siblings(s, cx, key, at);
            Reply::Redraw
        }
        _ => Reply::Redraw,
    }
}

/// Enter in the path field: the text is read once, here.
fn commit(s: &mut FilesScreen, cx: &mut EventCx, text: &str) {
    let env = env_of(&s.shared);
    let Some(cwd) = cwd_of(s, &env) else { return };
    match parse(text, &cwd, &env) {
        Parsed::Path(p) => start_stat(s, text, p, None),
        Parsed::Shell(name) => match env.known.iter().find(|(n, _)| *n == name) {
            Some((_, p)) => window::navigate(s, cx, Location::Fs(p.clone()), None),
            None => not_found(s, cx, text),
        },
        Parsed::Virtual(target) => {
            handoff::hand_off(s, cx, target, true);
        }
        Parsed::Command(c) => match cwd.join(&c.encode_utf16().collect::<Vec<u16>>()) {
            // A bare word that names a child of this folder goes there; anything else runs.
            Ok(child) => start_stat(s, text, child, Some((c, cwd))),
            Err(_) => run_typed(s, cx, &c, &cwd),
        },
        Parsed::Invalid(code) => match code.as_str() {
            "empty" => {}
            "too_long" => set_error(s, cx, Some(t(k::FILES_STATE_TOO_LONG, cx.lang()))),
            _ => set_error(s, cx, Some(t(k::FILES_STATE_BAD_PATH, cx.lang()))),
        },
    }
}

/// Folder, file or nothing: a Bg job on the path's lane; on_stat decides.
fn start_stat(s: &mut FilesScreen, text: &str, p: WidePath, command: Option<(String, WidePath)>) {
    let req = next_req(s);
    s.chrome.path.stat = Some(PendingStat { req, text: text.to_string(), path: p.clone(), command });
    let lane = crate::bg::lane_of(&p, &s.shared.drives.borrow());
    s.shared.services.bg.run(
        s.win,
        lane,
        Box::new(move || {
            // SAFETY: this thread's error mode only; it keeps a missing medium from raising a system box.
            let _ = unsafe {
                windows::Win32::System::Diagnostics::Debug::SetThreadErrorMode(
                    windows::Win32::System::Diagnostics::Debug::SEM_FAILCRITICALERRORS,
                    None,
                )
            };
            let found = std::fs::metadata(complete::fs_path(&p)).ok().map(|m| m.is_dir());
            Some(JobDone::Stat { req, path: p, found })
        }),
    );
}

/// The user's own Enter on text that is not a path: the host runs it as the Run box does, in this folder.
fn run_typed(s: &mut FilesScreen, cx: &mut EventCx, text: &str, dir: &WidePath) {
    if text.len() > COMMAND_MAX {
        set_error(s, cx, Some(t(k::FILES_STATE_BAD_PATH, cx.lang())));
        return;
    }
    let shell = &s.shared.services.shell;
    let req = shell.next_req();
    s.shared.req_owner.borrow_mut().insert(req, ReqOwner::Exec { win: s.win });
    shell.request(ShellReq::RunTyped { req, text: text.into(), dir: dir.clone() });
}

/// The Stat of a typed path: a folder is entered, a file's folder is entered with the file selected, nothing is "Can't
/// find". True when the request was this module's.
pub fn on_stat(s: &mut FilesScreen, cx: &mut EventCx, req: u64, path: WidePath, found: Option<bool>) -> bool {
    let _ = path;
    if s.chrome.path.stat.as_ref().is_none_or(|st| st.req != req) {
        return false;
    }
    let Some(st) = s.chrome.path.stat.take() else { return false };
    match (found, st.command) {
        (Some(true), _) => window::navigate(s, cx, Location::Fs(st.path), None),
        (Some(false), _) => match (st.path.parent(), st.path.file_name()) {
            (Some(dir), Some(name)) => {
                let mut focus = Vec::new();
                utf16_to_wtf8(name, &mut focus);
                window::navigate(s, cx, Location::Fs(dir), Some(focus.into_boxed_slice()));
            }
            _ => not_found(s, cx, &st.text),
        },
        (None, Some((c, cwd))) => run_typed(s, cx, &c, &cwd),
        (None, None) => not_found(s, cx, &st.text),
    }
    true
}

/// A chevron: the subfolders of that level are listed on the Bg lane, and on_siblings opens the list.
fn siblings(s: &mut FilesScreen, cx: &mut EventCx, key: u64, at: Anchor) {
    let _ = cx;
    let segs = &s.chrome.path.segs;
    let Some(i) = usize::try_from(key).ok().filter(|i| *i < segs.len()) else { return };
    let Location::Fs(parent) = segs[i].at.clone() else { return };
    let current = segs.get(i + 1).map(|c| c.name.clone()).unwrap_or_default();
    let req = next_req(s);
    s.chrome.path.siblings = Some(Siblings { req, parent: parent.clone(), current, at });
    let hidden = s.shared.opts.borrow().hidden_items;
    let lane = crate::bg::lane_of(&parent, &s.shared.drives.borrow());
    s.shared.services.bg.run(
        s.win,
        lane,
        Box::new(move || {
            let dirs = complete::list_dirs(&parent, hidden);
            Some(JobDone::Children { req, parent, dirs, done: true })
        }),
    );
}

/// "\\ / :" typed in the list (F4.6) moves the path into the field with the character after it, completion open.
pub fn type_into(s: &mut FilesScreen, cx: &mut EventCx, c: char) -> bool {
    if !matches!(c, '\\' | '/' | ':') || s.chrome.path.bar.is_editing() || s.rename.active() {
        return false;
    }
    let (Location::Fs(p) | Location::Search { root: p, .. }) = s.tabs.active().nav.location() else { return false };
    let mut text = p.to_string_lossy();
    match c {
        ':' => text.push(':'),
        _ if !text.ends_with('\\') => text.push('\\'),
        _ => {}
    }
    set_error(s, cx, None);
    s.chrome.path.bar.begin_edit_with(cx, &text);
    complete::request(s, cx, &text);
    true
}

/// The subfolders of a level arrived (the answer of siblings()): the list opens under the chevron with the folder the
/// path goes through highlighted. True when the request was this module's.
pub fn on_siblings(s: &mut FilesScreen, cx: &mut EventCx, req: u64, dirs: Vec<Box<str>>) -> bool {
    if s.chrome.path.siblings.as_ref().is_none_or(|sb| sb.req != req) {
        return false;
    }
    let Some(sb) = s.chrome.path.siblings.take() else { return false };
    if dirs.is_empty() {
        return true;
    }
    let highlight = dirs.iter().position(|d| d.as_bytes().eq_ignore_ascii_case(&sb.current));
    let items = dirs.iter().map(|d| Text::Owned(d.to_string())).collect();
    let id = cx.open_list(items, sb.at, highlight);
    s.chrome.path.list = Some(OpenList { id, parent: sb.parent, names: dirs, hl: highlight });
    true
}

/// EditPath (Ctrl+L, Alt+D) selects the whole path; EditPathHistory (F4) also lists the recent folders.
pub fn cmd(s: &mut FilesScreen, cx: &mut EventCx, c: Cmd) -> Reply {
    if !matches!(c, Cmd::EditPath | Cmd::EditPathHistory) {
        return Reply::None;
    }
    set_error(s, cx, None);
    s.chrome.path.bar.begin_edit(cx);
    if c == Cmd::EditPathHistory {
        let rows = complete::history_of(s, complete::MAX_ROWS).into_iter().map(Text::Owned).collect();
        s.chrome.path.set_completions(cx, rows);
    }
    Reply::Relayout
}
