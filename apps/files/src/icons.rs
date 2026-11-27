//! Icons in the table (P3-files-app 2.2; FB-OPEN fills it, F2.2): class icons through ClassIcons, item icons from the
//! shell host for the visible rows only.

use oled_ui::EventCx;

use crate::services::ReqOwner;
use crate::shell_client::ShellReply;
use crate::window::{FilesScreen, Shared};

/// After layout, a scroll or a batch: pushes ready icons through FilesTable::set_icon only. Filled by FB-OPEN; until
/// then nothing.
pub fn request_visible(s: &mut FilesScreen, cx: &mut EventCx) {
    let _ = (s, cx);
}

/// Filled by FB-OPEN; until then nothing.
pub fn on_icon_reply(s: &mut FilesScreen, cx: &mut EventCx, owner: ReqOwner, reply: &ShellReply) {
    let _ = (s, cx, owner, reply);
}

/// Filled by FB-OPEN; until then nothing.
pub fn drop_item_icons(s: &mut FilesScreen) {
    let _ = s;
}

/// After the first frame: the class icons' weekly rebuild.
pub fn on_first_frame(shared: &Shared) {
    shared.services.icons.rebuild_if_due();
}

/// SHCNE_ASSOCCHANGED: class icons and type names are stale.
pub fn on_assoc_changed(shared: &Shared) {
    shared.services.icons.assoc_changed();
    shared.types.clear();
}
