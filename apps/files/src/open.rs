//! Opening items (P3-files-app 2.2; FB-OPEN fills it, F2.1, F2.4) and the shell host's replies: every open goes
//! through the shell host, never in the UI process.

use oled_ui::model::RowKey;
use oled_ui::{EventCx, Reply};

use crate::keys::Cmd;
use crate::msg::Fwd;
use crate::services::ReqOwner;
use crate::shell_client::ShellReply;
use crate::status::{self, Notice};
use crate::tr::{k, t};
use crate::window::{FilesScreen, Shared};
use crate::{pins, types};

/// Filled by FB-OPEN; until then nothing is opened.
pub fn activate_items(s: &mut FilesScreen, cx: &mut EventCx, keys: &[RowKey]) -> Reply {
    let _ = (s, cx, keys);
    Reply::None
}

/// OpenInExplorer of the current folder. Filled by FB-OPEN; until then the not_in_beta notice.
pub fn hand_off_current(s: &mut FilesScreen, cx: &mut EventCx) -> Reply {
    let text = t(k::FILES_MENU_NOT_IN_BETA, cx.lang());
    status::notice(s, cx, Notice::new(text));
    Reply::Redraw
}

/// Filled by FB-OPEN; until then nothing.
pub fn prespawn(shared: &Shared) {
    let _ = shared;
}

/// Timer kind 8. Filled by FB-OPEN; until then nothing.
pub fn on_prespawn_timer(s: &mut FilesScreen, cx: &mut EventCx) {
    let _ = (s, cx);
}

/// Every shell reply, routed by its owner: a window's own request (Icons, Menu, Exec) goes back to that window; type
/// names and pins are process-wide and bump every main window; a class icon of ClassIcons' own bumps them too.
/// FB-OPEN keeps this routing and adds its own handling.
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
fn reply_req(r: &ShellReply) -> Option<oled_trust::frame::files::ReqId> {
    use oled_trust::frame::files::ShellEvt as E;
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
    use oled_trust::frame::files::ShellEvt as E;
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

/// Filled by FB-OPEN; until then nothing.
pub fn on_exec_reply(s: &mut FilesScreen, cx: &mut EventCx, reply: &ShellReply) {
    let _ = (s, cx, reply);
}

/// Open, Properties, OpenInExplorer. Filled by FB-OPEN; until then nothing.
pub fn cmd(s: &mut FilesScreen, cx: &mut EventCx, c: Cmd) -> Reply {
    let _ = (s, cx, c);
    Reply::None
}
