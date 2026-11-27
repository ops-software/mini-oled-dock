//! Opening items (P3-files-app 2.2; FB-OPEN fills it, F2.1, F2.4) and the shell host's replies: every open goes
//! through the shell host, never in the UI process.

use oled_trust::frame::files::{CallKind, ItemRef, Location, ReqId, ShellEvt, ShellReq, Verb};
use oled_ui::model::{ListModel, RowKey};
use oled_ui::node::Text;
use oled_ui::{EventCx, Reply};
use windows::Win32::System::Diagnostics::Debug::{
    FormatMessageW, FORMAT_MESSAGE_FROM_SYSTEM, FORMAT_MESSAGE_IGNORE_INSERTS,
};

use crate::keys::Cmd;
use crate::listing::WidePath;
use crate::model::id_of;
use crate::msg::Fwd;
use crate::services::ReqOwner;
use crate::shell_client::{FallbackWhy, HostState, ShellReply};
use crate::status::{self, Notice, NoticeAct, NOTICE_HOLD_MS};
use crate::tr::{k, t};
use crate::window::{self, FilesScreen, Shared};
use crate::{icons, pins, types};

/// Items opened at once: more than this and only the focused one opens (Explorer limits multi-open prompts too).
pub const MULTI_OPEN_MAX: usize = 16;

/// HRESULT_FROM_WIN32(ERROR_CANCELLED): the user closed a dialog the verb showed; nothing to report.
const HR_CANCELLED: i32 = 0x8007_04C7_u32 as i32;

/// What a window keeps for the shell host's work (a field of FilesScreen: the owner of the frozen struct has no
/// other place for it).
#[derive(Default)]
pub struct OpenState {
    /// The last Exec this window sent, for the notice's Retry.
    retry: Option<Retry>,
    /// Per-item icon requests and what they brought (F2.2).
    pub(crate) icons: icons::IconState,
}

struct Retry {
    verb: Verb,
    items: Vec<ItemRef>,
    /// The services clock at which the Retry action is no longer on show; 0 until a notice offers it.
    until_ms: u64,
    /// The serial of the notice that offers the Retry (status::Status::serial); 0 until one does.
    serial: u64,
    /// The request that carries these items: only its reply may offer the Retry or name its extension.
    req: ReqId,
}

/// Enter or a double click on a row: a row inside a selection of several opens the whole selection (model order, at
/// most MULTI_OPEN_MAX items; over that only the asked row), any other row opens alone.
pub fn activate_items(s: &mut FilesScreen, cx: &mut EventCx, keys: &[RowKey]) -> Reply {
    let table = &s.tabs.active().table;
    let model = table.model();
    let sel = table.selection();
    let total = ListModel::len(model);
    let mut keys = keys.to_vec();
    let several = sel.count(total);
    if keys.len() == 1 && several > 1 && sel.contains(keys[0]) && several <= MULTI_OPEN_MAX {
        keys.clear();
        sel.for_each(model, &mut |_, key| keys.push(key));
    }
    keys.truncate(if keys.len() > MULTI_OPEN_MAX { 1 } else { MULTI_OPEN_MAX });
    open_keys(s, cx, &keys)
}

/// Folders navigate (the first one; the others go to background tabs once F4.2 can), everything else opens through the
/// shell host in one Exec.
fn open_keys(s: &mut FilesScreen, cx: &mut EventCx, keys: &[RowKey]) -> Reply {
    let model = s.tabs.active().table.model();
    let mut folders = Vec::new();
    let mut files = Vec::new();
    for key in keys {
        let Some(path) = model.path_of(*key) else { continue };
        if model.listing().is_dir(id_of(*key)) {
            folders.push(path);
        } else {
            files.push(ItemRef::Path(path));
        }
    }
    let mut reply = Reply::None;
    let mut folders = folders.into_iter();
    if let Some(first) = folders.next() {
        window::navigate(s, cx, Location::Fs(first), None);
        reply = Reply::Redraw;
    }
    for rest in folders {
        crate::tab::open_background(s, cx, Location::Fs(rest));
    }
    if !files.is_empty() {
        send_exec(s, Verb::Open, files);
    }
    reply
}

/// OpenInExplorer of the current folder (Ctrl+Shift+O and the banners), through the loop guard as every hand-off
/// (F4.13). Locations that are no folder (Home, Options) have nothing to hand over: the not_in_beta notice.
pub fn hand_off_current(s: &mut FilesScreen, cx: &mut EventCx) -> Reply {
    match current_folder(s) {
        Some(p) => crate::handoff::hand_off(s, cx, crate::handoff::Target::Folder(p), false),
        None => {
            let text = t(k::FILES_MENU_NOT_IN_BETA, cx.lang());
            status::notice(s, cx, Notice::new(text));
            Reply::Redraw
        }
    }
}

/// The folder the active tab shows, when it is a file system folder.
fn current_folder(s: &FilesScreen) -> Option<WidePath> {
    match s.tabs.active().nav.location() {
        Location::Fs(p) => Some(p.clone()),
        _ => None,
    }
}

/// The selected rows' paths in model order.
fn selected_paths(s: &FilesScreen) -> Vec<WidePath> {
    let table = &s.tabs.active().table;
    table.model().selected_paths(table.selection())
}

/// Properties of the selection, or of the current folder when nothing is selected.
fn properties(s: &mut FilesScreen) {
    let mut items: Vec<ItemRef> = selected_paths(s).into_iter().map(ItemRef::Path).collect();
    if items.is_empty() {
        items.extend(current_folder(s).map(ItemRef::Path));
    }
    if !items.is_empty() {
        send_exec(s, Verb::Properties, items);
    }
}

/// The shell host's warm-up: only when it is down (a ready or starting one needs nothing).
pub fn prespawn(shared: &Shared) {
    shared.host_warm.set(true);
    if matches!(shared.services.shell.state(), HostState::Down) {
        shared.services.shell.prespawn();
    }
}

/// Timer kind 8, 500 ms after the first frame: the host starts now, and the rows that waited for it ask for their
/// icons.
pub fn on_prespawn_timer(s: &mut FilesScreen, cx: &mut EventCx) {
    prespawn(&s.shared);
    icons::request_visible(s, cx);
}

/// False while the host's three-crash breaker is open: "Show more options" is disabled then (F2.5 reads this).
pub fn more_options_enabled(shared: &Shared) -> bool {
    !matches!(shared.services.shell.state(), HostState::MenuOff)
}

/// An Exec of this window: the request is remembered for Retry. AllowSetForegroundWindow(host_pid) is the client's.
fn send_exec(s: &mut FilesScreen, verb: Verb, items: Vec<ItemRef>) {
    let req = new_exec(&s.shared, s.win, verb, items.clone());
    s.open.retry = Some(Retry { verb, items, until_ms: 0, serial: 0, req });
}

fn new_exec(shared: &Shared, win: u32, verb: Verb, items: Vec<ItemRef>) -> ReqId {
    let req = shared.services.shell.next_req();
    shared.req_owner.borrow_mut().insert(req, ReqOwner::Exec { win });
    shared.services.shell.request(ShellReq::Exec { req, verb, items });
    req
}

/// A request of this window that is not an open (a hand-off, F4.13): the last Exec is no longer what a Retry would
/// send.
pub(crate) fn forget_retry(s: &mut FilesScreen) {
    s.open.retry = None;
}

/// The notice's Retry after a host fallback: sends the last Exec again. None when this window has nothing to resend
/// (the notice was an other group's; the caller refreshes then).
pub fn retry(s: &mut FilesScreen, _cx: &mut EventCx) -> Option<Reply> {
    let now = s.shared.services.clock.now_ms();
    let latest = s.status.serial();
    // Only the notice this Exec posted may resend it: a later notice's Retry is that notice's own.
    let r = s.open.retry.as_mut().filter(|r| r.until_ms > now && r.serial == latest)?;
    r.until_ms = 0;
    let (verb, items) = (r.verb, r.items.clone());
    let req = new_exec(&s.shared, s.win, verb, items);
    if let Some(r) = s.open.retry.as_mut() {
        r.req = req;
    }
    Some(Reply::None)
}

/// Every shell reply, routed by its owner: a window's own request (Icons, Menu, Exec) goes back to that window; type
/// names and pins are process-wide and bump every main window; a class icon of ClassIcons' own bumps them too.
pub fn drain_shell(shared: &Shared) -> Vec<(u32, Fwd)> {
    let mut out = Vec::new();
    let mut bump = false;
    for reply in shared.services.shell.drain() {
        let req = reply_req(&reply);
        let owner = req.and_then(|r| shared.req_owner.borrow().get(&r).copied());
        if let (Some(r), true) = (req, is_last(&reply)) {
            shared.req_owner.borrow_mut().remove(&r);
        }
        match owner {
            Some(o @ (ReqOwner::Icons { win, .. } | ReqOwner::Menu { win } | ReqOwner::Exec { win })) => {
                out.push((win, Fwd::Shell { owner: o, reply }));
            }
            Some(ReqOwner::TypeNames) => bump |= types::on_reply(shared, &reply) || is_last(&reply),
            Some(ReqOwner::Pins) => bump |= pins::on_reply(shared, &reply) || is_last(&reply),
            Some(ReqOwner::ShellNew) => {}
            None => bump |= shared.services.icons.on_shell_reply(&reply),
        }
    }
    if bump {
        out.extend(shared.main_windows().into_iter().map(|w| (w, Fwd::Bump)));
    }
    out
}

/// The request a reply answers (Ready and Busy answer none).
fn reply_req(r: &ShellReply) -> Option<ReqId> {
    use ShellEvt as E;
    match r {
        ShellReply::Fallback { req, .. } => Some(*req),
        ShellReply::Evt(e) => match e {
            E::MenuShown { req }
            | E::MenuDone { req, .. }
            | E::Modern { req, .. }
            | E::ExecDone { req, .. }
            | E::Icon { req, .. }
            | E::Thumb { req, .. }
            | E::ClassIcon { req, .. }
            | E::TypeNames { req, .. }
            | E::ShellNew { req, .. }
            | E::Pins { req, .. }
            | E::Failed { req, .. }
            | E::ReqDone { req } => Some(*req),
            E::Ready { .. } | E::Busy { .. } => None,
        },
    }
}

/// The last reply of its request: nothing more comes for it.
fn is_last(r: &ShellReply) -> bool {
    use ShellEvt as E;
    match r {
        ShellReply::Fallback { .. } => true,
        ShellReply::Evt(e) => matches!(
            e,
            E::MenuDone { .. }
                | E::ExecDone { .. }
                | E::Failed { .. }
                | E::ReqDone { .. }
                | E::Modern { done: true, .. }
        ),
    }
}

/// The reply of an Exec this window sent: a failure shows Windows' own message; a host that did not answer shows
/// "Windows menu didn't respond" with Retry (a tripped breaker names its extension instead).
pub fn on_exec_reply(s: &mut FilesScreen, cx: &mut EventCx, reply: &ShellReply) {
    let lang = cx.lang();
    // Another Exec may have been sent since: only the latest one's reply may speak for the remembered items.
    let latest = reply_req(reply).is_some_and(|r| s.open.retry.as_ref().is_some_and(|x| x.req == r));
    match reply {
        ShellReply::Evt(ShellEvt::ExecDone { hr, .. } | ShellEvt::Failed { hr, .. })
            if *hr < 0 && *hr != HR_CANCELLED =>
        {
            status::notice(s, cx, Notice::new(Text::Owned(system_message(*hr))));
        }
        ShellReply::Fallback { why: FallbackWhy::Disabled, .. } => {
            let mut text = t(k::FILES_MENU_HOST_DOWN, lang).to_string();
            if let Some(ext) = disabled_ext(s).filter(|_| latest) {
                text.push_str(" · .");
                text.push_str(&ext);
            }
            status::notice(s, cx, Notice::new(Text::Owned(text)));
        }
        ShellReply::Fallback { why, .. } => {
            let text = t(k::FILES_MENU_HOST_DOWN, lang);
            let retryable = matches!(why, FallbackWhy::Deadline | FallbackWhy::HostDied | FallbackWhy::Unavailable);
            let mut n = Notice::new(text);
            if retryable && latest {
                n.action = Some((t(k::FILES_ACTION_RETRY, lang), NoticeAct::Retry));
                let until = s.shared.services.clock.now_ms().saturating_add(u64::from(NOTICE_HOLD_MS));
                if let Some(r) = s.open.retry.as_mut() {
                    r.until_ms = until;
                }
            }
            status::notice(s, cx, n);
            let serial = s.status.serial();
            if let Some(r) = s.open.retry.as_mut().filter(|_| latest) {
                r.serial = serial;
            }
        }
        ShellReply::Evt(_) => {}
    }
}

/// The extension of the last Exec's items whose breaker tripped.
fn disabled_ext(s: &FilesScreen) -> Option<String> {
    let r = s.open.retry.as_ref()?;
    r.items.iter().find_map(|it| {
        let ItemRef::Path(p) = it else { return None };
        let name = p.to_string_lossy();
        let ext = name.rsplit_once('.').map(|(_, e)| e.to_ascii_lowercase())?;
        (!ext.contains(['\\', '/']) && s.shared.services.shell.disabled(CallKind::Exec, &ext)).then_some(ext)
    })
}

/// Windows' own text for an HRESULT, in the user's language (FormatMessageW, system table); the hex code when the
/// system has none.
fn system_message(hr: i32) -> String {
    let code = hr as u32;
    // HRESULT_FROM_WIN32 wraps a Win32 error in facility 7: the system table is keyed by the error.
    let id = if code >> 16 == 0x8007 { code & 0xFFFF } else { code };
    let mut buf = [0u16; 512];
    // SAFETY: the buffer is ours and its length is passed; FROM_SYSTEM | IGNORE_INSERTS reads no source or arguments.
    let n = unsafe {
        FormatMessageW(
            FORMAT_MESSAGE_FROM_SYSTEM | FORMAT_MESSAGE_IGNORE_INSERTS,
            None,
            id,
            0,
            windows::core::PWSTR(buf.as_mut_ptr()),
            buf.len() as u32,
            None,
        )
    };
    let text = String::from_utf16_lossy(buf.get(..n as usize).unwrap_or_default());
    let text = text.trim();
    if text.is_empty() {
        format!("0x{code:08X}")
    } else {
        text.to_string()
    }
}

/// Open, Properties, OpenInExplorer.
pub fn cmd(s: &mut FilesScreen, cx: &mut EventCx, c: Cmd) -> Reply {
    match c {
        Cmd::Open => {
            let table = &s.tabs.active().table;
            let mut keys = Vec::new();
            table.selection().for_each(table.model(), &mut |_, key| keys.push(key));
            if keys.len() > MULTI_OPEN_MAX {
                keys = table.focused().into_iter().collect();
            }
            open_keys(s, cx, &keys)
        }
        Cmd::Properties => {
            properties(s);
            Reply::None
        }
        Cmd::OpenInExplorer => hand_off_current(s, cx),
        _ => Reply::None,
    }
}
