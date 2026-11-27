//! Icons in the table (P3-files-app 2.2; FB-OPEN fills it, F2.2): class icons through ClassIcons, item icons from the
//! shell host for the visible rows only. The table's icon table is written only through FilesTable::set_icon.

use std::collections::HashMap;
use std::rc::Rc;
use std::sync::Arc;

use oled_trust::frame::files::{Gen, ItemRef, ShellEvt, ShellReq};
use oled_ui::model::ListModel;
use oled_ui::EventCx;

use crate::decor::cache::IconRef;
use crate::listing::{EntryId, TabId};
use crate::model::{id_of, IconKey};
use crate::services::ReqOwner;
use crate::shell::images::OWN_ICON_EXTS;
use crate::shell_client::ShellReply;
use crate::window::{FilesScreen, Shared};

/// Per-item icons the table keeps (FilesTable's own LRU): the window's record of what it asked for follows the same
/// number, so a row whose icon was dropped is asked for again.
pub const ITEM_ICONS_MAX: usize = 512;

/// Whether a file's own icon is asked of the host: an extension of the host's list (shell::images::OWN_ICON_EXTS: the
/// icon is inside the file; shortcuts, consoles, command files and customised folders name an icon location outside
/// themselves that the shell would resolve and load, which can reach a remote server, so they keep their class or
/// folder icon), no link, nothing offline or dehydrated (the attributes the listing already holds), and a location of
/// the form `X:\...` (never UNC, a device or a URL; the host checks the drive kind and the file again before any load).
pub fn wants_item_icon(ext: &[u8], attrs: u32, path: &[u16]) -> bool {
    const REPARSE_POINT: u32 = 0x400;
    const OFFLINE: u32 = 0x1000;
    const RECALL_ON_OPEN: u32 = 0x4_0000;
    const RECALL_ON_DATA_ACCESS: u32 = 0x40_0000;
    let local_form =
        matches!(path, [l, c, ..] if u8::try_from(*l).is_ok_and(|b| b.is_ascii_alphabetic()) && *c == u16::from(b':'));
    OWN_ICON_EXTS.iter().any(|e| e.as_bytes() == ext)
        && attrs & (REPARSE_POINT | OFFLINE | RECALL_ON_OPEN | RECALL_ON_DATA_ACCESS) == 0
        && local_form
}

/// What a window knows about its per-item icon requests, by tab.
#[derive(Default)]
pub struct IconState {
    tabs: HashMap<TabId, TabIcons>,
}

#[derive(Default)]
struct TabIcons {
    /// The listing the record belongs to: another one (a navigation) starts it again.
    list_gen: Option<Gen>,
    /// The table's item-icon epoch the record belongs to: icons dropped from the table start it again.
    epoch: u64,
    /// The request on the wire, if any: only its replies are taken.
    req: Option<IconReq>,
    /// Rows asked for and what they brought, with the order they came in.
    have: HashMap<EntryId, u64>,
    stamp: u64,
}

struct IconReq {
    gen: Gen,
    /// The entry of each item of the request, by index.
    ids: Vec<EntryId>,
}

impl TabIcons {
    fn note(&mut self, id: EntryId) {
        self.stamp += 1;
        self.have.insert(id, self.stamp);
        if self.have.len() > ITEM_ICONS_MAX {
            if let Some(oldest) = self.have.iter().min_by_key(|(_, s)| **s).map(|(id, _)| *id) {
                self.have.remove(&oldest);
            }
        }
    }
}

/// After layout, a scroll or a batch: pushes ready icons through FilesTable::set_icon only, and asks the shell host for
/// the item icons of the visible rows. Nothing before the first frame (the rows ask again on the next event).
pub fn request_visible(s: &mut FilesScreen, cx: &mut EventCx) {
    let shared = Rc::clone(&s.shared);
    if !shared.first_frame_done.get() {
        s.visible_due = true;
        return;
    }
    let px = u16::try_from(cx.px(oled_ui::tokens::ICON_S)).unwrap_or(16);
    let tab = s.tabs.active().id;
    s.open.icons.tabs.retain(|id, _| s.tabs.by_id(*id).is_some());
    let visible = Visible::of(s);
    let mut changed = false;
    // Class icons: what the service has is stored; what it is still making arrives with a Fwd::Bump.
    let table = &mut s.tabs.active_mut().table;
    for ext in &visible.exts {
        shared.services.icons.note(ext);
        if let IconRef::Ready(bgra) = shared.services.icons.get(ext, px) {
            changed |= table.model_mut().set_icon(IconKey::Class(ext.clone()), px, bgra);
        }
    }
    if changed {
        cx.redraw();
    }
    ask_items(s, &shared, tab, px, &visible.item_rows);
}

/// What the visible rows need.
struct Visible {
    /// Distinct lower-case extensions of the files shown.
    exts: Vec<Box<[u8]>>,
    /// Rows (entry, path) whose icon is the item's own.
    item_rows: Vec<(EntryId, crate::listing::WidePath)>,
}

impl Visible {
    fn of(s: &FilesScreen) -> Visible {
        let table = &s.tabs.active().table;
        let model = table.model();
        let listing = model.listing();
        let mut v = Visible { exts: Vec::new(), item_rows: Vec::new() };
        for row in table.visible_rows() {
            let key = ListModel::key(model, row);
            let id = id_of(key);
            if listing.entry(id).is_none() {
                continue;
            }
            let is_dir = listing.is_dir(id);
            let ext = listing.ext(id).unwrap_or_default().to_ascii_lowercase();
            let attrs = listing.entry(id).map_or(0, |e| e.attrs);
            if is_dir {
                continue;
            }
            if let Some(p) = model.path_of(key).filter(|p| wants_item_icon(&ext, attrs, p.units())) {
                v.item_rows.push((id, p));
            }
            if !v.exts.iter().any(|x| **x == *ext) {
                v.exts.push(ext.into_boxed_slice());
            }
        }
        v
    }
}

/// One Icons request for the rows that have no item icon yet, newest viewport first: a request still on the wire is
/// cancelled by the newer one.
fn ask_items(s: &mut FilesScreen, shared: &Shared, tab: TabId, px: u16, rows: &[(EntryId, crate::listing::WidePath)]) {
    // The host starts 500 ms after the first frame, not by an icon request: the prespawn timer (or a right button
    // down) asks again for the rows that waited.
    if !shared.host_warm.get() {
        return;
    }
    let list_gen = s.tabs.active().table.model().listing().gen();
    let epoch = s.tabs.active().table.model().item_epoch();
    let st = s.open.icons.tabs.entry(tab).or_default();
    if st.list_gen != Some(list_gen) || st.epoch != epoch {
        *st = TabIcons { list_gen: Some(list_gen), epoch, ..TabIcons::default() };
    }
    let wanted: Vec<&(EntryId, crate::listing::WidePath)> =
        rows.iter().filter(|(id, _)| !st.have.contains_key(id)).collect();
    if wanted.is_empty() {
        return;
    }
    if let Some(r) = &st.req {
        if wanted.iter().all(|(id, _)| r.ids.contains(id)) {
            return;
        }
    }
    let gen = Gen(shared.next_req.get() + 1);
    shared.next_req.set(gen.0);
    let shell = &shared.services.shell;
    // The host's cancel is process-wide (every request below the generation, whichever window sent it), so it is sent
    // only when no other window or tab has an Icons request on the wire.
    let others = shared
        .req_owner
        .borrow()
        .values()
        .any(|o| matches!(o, ReqOwner::Icons { win, tab: t, .. } if (*win, *t) != (s.win, tab)));
    if st.req.take().is_some() && !others {
        // The host drops the image work of generations below this one.
        shell.request(ShellReq::CancelGen { gen });
    }
    let (ids, items): (Vec<EntryId>, Vec<ItemRef>) =
        wanted.into_iter().map(|(id, p)| (*id, ItemRef::Path(p.clone()))).unzip();
    st.req = Some(IconReq { gen, ids });
    if let Some(t) = s.tabs.by_id_mut(tab) {
        t.icon_gen = gen;
    }
    let req = shell.next_req();
    shared.req_owner.borrow_mut().insert(req, ReqOwner::Icons { win: s.win, tab, gen });
    shell.request(ShellReq::Icons { req, gen, items, px });
}

/// A reply of this window's Icons request: an image goes to the table through set_icon; the end of the request (or a
/// fallback) closes it. A reply of a request that is no longer the tab's newest, or of a listing the tab left, is
/// dropped.
pub fn on_icon_reply(s: &mut FilesScreen, cx: &mut EventCx, owner: ReqOwner, reply: &ShellReply) {
    let ReqOwner::Icons { tab, gen, .. } = owner else { return };
    let Some((listing_gen, epoch)) =
        s.tabs.by_id(tab).map(|t| (t.table.model().listing().gen(), t.table.model().item_epoch()))
    else {
        return;
    };
    let Some(st) = s.open.icons.tabs.get_mut(&tab) else { return };
    if st.list_gen != Some(listing_gen) || st.epoch != epoch || st.req.as_ref().map(|r| r.gen) != Some(gen) {
        return;
    }
    match reply {
        ShellReply::Evt(ShellEvt::Icon { gen: g, index, px, .. }) if *g == gen => {
            let Some(id) = st.req.as_ref().and_then(|r| r.ids.get(*index as usize).copied()) else { return };
            st.note(id);
            if px.w == 0 || px.w != px.h {
                return;
            }
            let bgra: Arc<[u8]> = Arc::from(px.bgra.as_ref());
            let Some(t) = s.tabs.by_id_mut(tab) else { return };
            if t.table.model_mut().set_icon(IconKey::Item(id), px.w, bgra) {
                cx.redraw();
            }
        }
        ShellReply::Evt(ShellEvt::ReqDone { .. } | ShellEvt::Failed { .. }) | ShellReply::Fallback { .. } => {
            // Rows that got no image are not asked for again for this listing.
            if let Some(r) = st.req.take() {
                for id in r.ids {
                    if !st.have.contains_key(&id) {
                        st.note(id);
                    }
                }
            }
        }
        ShellReply::Evt(_) => {}
    }
}

/// Memory pressure: what the window knows about item icons goes; the replies still on the wire are dropped as stale.
pub fn drop_item_icons(s: &mut FilesScreen) {
    s.open.icons.tabs.clear();
    for t in s.tabs.iter_mut() {
        t.table.model_mut().drop_item_icons();
    }
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
