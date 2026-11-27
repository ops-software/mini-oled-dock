//! The AccessKit bridge (K2b.9, SPEC-platform 5.3.7; the a11y feature). One accesskit_windows Adapter per kit window,
//! made right after CreateWindowExW returns (never inside WM_GETOBJECT: AccessKit issue 37) and lazy until a client sends
//! WM_GETOBJECT, so a window nobody reads builds no tree and wakes for nothing. Once a client is connected, each layout
//! and each focus move hands it the nodes whose hash changed (tree.rs). The window's own focus is looked up at those
//! points, on EVENT_OBJECT_FOCUS for this thread's windows and on every foreground change (the only sign of another
//! thread taking the focus), since a window's activation and deactivation reach no kit hook. A
//! client's action requests arrive on its threads, wait in the ActionQueue and reach the UI thread as WM_OLED_A11Y,
//! where they become Event::A11y in order with the window's other input. UIA events are raised only once no borrow is
//! held: raising one can bring a nested WM_GETOBJECT.
//!
//! A window's bridge hangs off the window as a property (an Rc whose pointer SetPropW holds from creation to
//! WM_DESTROY): the window's state belongs to the host, and the kit keeps no thread-local state.

use std::cell::RefCell;
use std::ffi::c_void;
use std::rc::Rc;
use std::sync::Arc;

use accesskit::{
    Action, ActionData, ActionHandler, ActionRequest, ActivationHandler, Invalid, Live, Node, NodeId as AkId, Rect,
    Role as AkRole, Toggled, TreeId, TreeInfo, TreeUpdate,
};
use accesskit_windows::{Adapter, QueuedEvents};
use windows::core::{w, BOOL, PCWSTR};
use windows::Win32::Foundation::{HANDLE, HWND, LPARAM, LRESULT, WPARAM};
use windows::Win32::System::Threading::{GetCurrentProcessId, GetCurrentThreadId};
use windows::Win32::UI::Accessibility::{
    IRawElementProviderSimple, SetWinEventHook, UiaReturnRawElementProvider, UnhookWinEvent, HWINEVENTHOOK,
};
use windows::Win32::UI::Input::KeyboardAndMouse::GetFocus;
use windows::Win32::UI::WindowsAndMessaging::{
    EnumThreadWindows, GetForegroundWindow, GetPropW, GetWindowThreadProcessId, PostMessageW, RemovePropW, SetPropW,
    EVENT_OBJECT_FOCUS, EVENT_SYSTEM_FOREGROUND, WINEVENT_OUTOFCONTEXT,
};

use super::tree::{self, A11yNode, A11yTree, Pushed, ROOT};
use super::ActionQueue;
use crate::dispatch::Input;
use crate::event::A11yAction;
use crate::host::window::{self, Sys, WinHost};
use crate::host::WM_OLED_A11Y;
use crate::node::{Actions, NodeFlags, NodeId, NodeValue, Role};

/// The window property that holds a window's bridge.
const PROP: PCWSTR = w!("oled-ui.a11y");
/// WM_OLED_A11Y's wParam: a client's action requests wait in the queue.
const WAKE_ACTIONS: usize = 0;
/// WM_OLED_A11Y's wParam: the client got a placeholder (the window's state was busy at its WM_GETOBJECT); the whole
/// tree follows.
const WAKE_TREE: usize = 1;
/// TreeInfo's toolkit name.
const TOOLKIT: &str = "oled-ui";

/// A window's bridge.
struct Bridge {
    hwnd: HWND,
    /// The app's id: the root's automation id.
    app: &'static str,
    adapter: RefCell<Adapter>,
    st: RefCell<State>,
    /// Shared with the action handler.
    queue: Arc<ActionQueue>,
}

/// What the bridge knows of its client.
#[derive(Default)]
struct State {
    /// A client sent WM_GETOBJECT: the adapter holds a tree (or a placeholder) and takes updates.
    live: bool,
    /// The window had the keyboard focus at the last look.
    window_focused: bool,
    /// The client's copy of the tree.
    pushed: Pushed,
    /// The focus watches (watch_focus), from the first WM_GETOBJECT on.
    hooks: Vec<HWINEVENTHOOK>,
}

impl Drop for State {
    fn drop(&mut self) {
        for h in self.hooks.drain(..) {
            // SAFETY: a hook watch_focus installed, removed once.
            let _ = unsafe { UnhookWinEvent(h) };
        }
    }
}

impl Bridge {
    /// Tells the adapter whether the window holds the keyboard focus, when that changed; the events go to `out`.
    fn sync_window_focus(&self, st: &mut State, ad: &mut Adapter, out: &mut Vec<QueuedEvents>) {
        let focused = has_focus(self.hwnd);
        if focused != st.window_focused {
            st.window_focused = focused;
            out.extend(ad.update_window_focus_state(focused));
        }
    }

    /// Hands the client what changed since its copy (the whole tree after a placeholder); the events to raise. Nothing
    /// while no client is connected, or when no node hash and not the focus changed.
    fn update(&self, host: &WinHost) -> Vec<QueuedEvents> {
        let mut out = Vec::new();
        let (Ok(mut st), Ok(mut ad)) = (self.st.try_borrow_mut(), self.adapter.try_borrow_mut()) else { return out };
        if !st.live {
            return out;
        }
        self.sync_window_focus(&mut st, &mut ad, &mut out);
        let tree = snapshot(host);
        if let Some(nodes) = st.pushed.changes(&tree) {
            let upd = update_of(&tree, nodes, !st.pushed.has_tree(), self.app);
            let mut applied = false;
            out.extend(ad.update_if_active(|| {
                applied = true;
                upd
            }));
            if applied {
                st.pushed.commit(&tree);
            }
        }
        out
    }

    /// A watched focus or foreground change: the window may have gained or lost the keyboard focus.
    fn focus_event(&self) -> Vec<QueuedEvents> {
        let mut out = Vec::new();
        if let (Ok(mut st), Ok(mut ad)) = (self.st.try_borrow_mut(), self.adapter.try_borrow_mut()) {
            if st.live {
                self.sync_window_focus(&mut st, &mut ad, &mut out);
            }
        }
        out
    }
}

/// The tree of the window's last layout.
fn snapshot(host: &WinHost) -> A11yTree {
    tree::build(&host.disp.frame, host.client_rect(), host.disp.state.focused)
}

/// True when `hwnd` holds the keyboard focus (holds_focus).
fn has_focus(hwnd: HWND) -> bool {
    // SAFETY: GetFocus reads this thread's focus window, GetForegroundWindow the session's, GetWindowThreadProcessId a
    // window's thread (0 for none); no precondition.
    let (focus, foreground) = unsafe { (GetFocus(), GetWindowThreadProcessId(GetForegroundWindow(), None)) };
    // SAFETY: reads this thread's id; no precondition.
    holds_focus(hwnd, focus, foreground, unsafe { GetCurrentThreadId() })
}

/// `hwnd` holds the keyboard focus when it is its thread's focus window and the thread owns the foreground window: the
/// foreground can move on before the thread has handled its deactivation, while its focus still names the window.
fn holds_focus(hwnd: HWND, focus: HWND, foreground_thread: u32, thread: u32) -> bool {
    focus == hwnd && foreground_thread == thread
}

/// The bridge of a window of this thread, with a strong reference of its own (the bridge outlives an on_destroy that
/// runs while the caller holds it).
fn of(hwnd: HWND) -> Option<Rc<Bridge>> {
    // SAFETY: reading a window's property has no precondition.
    let p = unsafe { GetPropW(hwnd, PROP) }.0 as *const Bridge;
    if p.is_null() {
        return None;
    }
    // An Rc is only ever touched on its own thread.
    // SAFETY: both read thread ids; no precondition.
    if unsafe { GetWindowThreadProcessId(hwnd, None) != GetCurrentThreadId() } {
        return None;
    }
    // SAFETY: a non-null property of a window of this thread is the pointer attach got from Rc::into_raw; the reference
    // it stands for is released only by on_destroy, which removes the property first; the count added here is released
    // by the Rc returned.
    unsafe {
        Rc::increment_strong_count(p);
        Some(Rc::from_raw(p))
    }
}

/// Posts WM_OLED_A11Y to the window.
fn wake(hwnd: HWND, what: usize) -> bool {
    // SAFETY: posting to a window by handle has no precondition; a window that is gone makes it fail.
    let posted = unsafe { PostMessageW(Some(hwnd), WM_OLED_A11Y, WPARAM(what), LPARAM(0)) };
    posted.is_ok()
}

/// Raises `events` once the host's borrow has ended.
fn raise_later(host: &mut WinHost, events: Vec<QueuedEvents>) {
    if !events.is_empty() {
        host.defer(Sys::Call(Box::new(move |_| events.into_iter().for_each(QueuedEvents::raise))));
    }
}

/// After creation: the window's bridge, made once the host's borrow has ended (UIA initialises inside Adapter::new)
/// and still before the window is shown.
pub(crate) fn on_created(host: &mut WinHost) {
    let app = host.lp.app.0;
    host.defer(Sys::Call(Box::new(move |hwnd| attach(hwnd, app))));
}

fn attach(hwnd: HWND, app: &'static str) {
    if of(hwnd).is_some() {
        return;
    }
    let queue = Arc::new(ActionQueue::default());
    // The handler runs on the client's threads: it keeps the handle as a number, not as an HWND.
    let target = hwnd.0 as isize;
    let post = move || wake(HWND(target as *mut c_void), WAKE_ACTIONS);
    let adapter = Adapter::new(hwnd, false, Handler { queue: Arc::clone(&queue), post });
    let b = Bridge { hwnd, app, adapter: RefCell::new(adapter), st: RefCell::default(), queue };
    let raw = Rc::into_raw(Rc::new(b));
    // SAFETY: our window, on its thread; the property holds the reference into_raw made until on_destroy takes it back.
    if unsafe { SetPropW(hwnd, PROP, Some(HANDLE(raw as *mut c_void))) }.is_err() {
        // SAFETY: the reference into_raw made, never stored.
        drop(unsafe { Rc::from_raw(raw) });
    }
}

/// After a layout: the client's update.
pub(crate) fn after_layout(host: &mut WinHost) {
    if let Some(b) = of(host.hwnd) {
        let events = b.update(host);
        raise_later(host, events);
    }
}

/// After a focus move: the client's update (the new focus, and any node that changed meanwhile).
pub(crate) fn on_focus_changed(host: &mut WinHost) {
    after_layout(host);
}

/// WM_DESTROY: the property goes first (no later call finds the bridge) and the queue refuses requests; once the
/// host's borrow has ended UIA lets go of the providers it got for the window.
pub(crate) fn on_destroy(host: &mut WinHost, hwnd: HWND) {
    // SAFETY: our window's own property; once removed nothing reads it again.
    let Ok(p) = (unsafe { RemovePropW(hwnd, PROP) }) else { return };
    if p.0.is_null() {
        return;
    }
    // SAFETY: the reference attach stored in the property, taken back once (the property is gone).
    let b = unsafe { Rc::from_raw(p.0 as *const Bridge) };
    b.queue.close();
    host.defer(Sys::Call(Box::new(move |hwnd| {
        if b.st.try_borrow().is_ok_and(|st| st.live) {
            // SAFETY: WM_DESTROY of our window: a null provider tells UIA to release the window's providers.
            let _ =
                unsafe { UiaReturnRawElementProvider(hwnd, WPARAM(0), LPARAM(0), None::<&IRawElementProviderSimple>) };
        }
        drop(b);
    })));
}

/// WM_GETOBJECT: the client's root provider, or None for DefWindowProcW (another object id, no bridge, or a nested
/// message while the bridge is busy).
pub(crate) fn on_getobject(hwnd: HWND, wp: WPARAM, lp: LPARAM) -> Option<LRESULT> {
    let b = of(hwnd)?;
    let found = {
        let mut st = b.st.try_borrow_mut().ok()?;
        let mut ad = b.adapter.try_borrow_mut().ok()?;
        if !st.live {
            // Still inactive: the adapter only notes the window's focus and raises nothing.
            st.window_focused = has_focus(hwnd);
            let _ = ad.update_window_focus_state(st.window_focused);
        }
        let mut act = Activation { hwnd, app: b.app, tree: None };
        let found = ad.handle_wm_getobject(wp, lp, &mut act);
        if let Some(t) = act.tree.take() {
            st.pushed.commit(&t);
        }
        if found.is_some() && !st.live {
            st.live = true;
            st.hooks = watch_focus();
            if !st.pushed.has_tree() {
                wake(hwnd, WAKE_TREE);
            }
        }
        found
    };
    // UiaReturnRawElementProvider (inside into) can send a nested WM_GETOBJECT: no borrow is held any more.
    found.map(Into::into)
}

/// WM_OLED_A11Y: the client's requests become Event::A11y, in order with the window's other input; after a
/// placeholder, the whole tree is handed over.
pub(crate) fn on_action(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    let Some(b) = of(hwnd) else { return LRESULT(0) };
    let _ = window::handle(hwnd, msg, wp, lp, |h| {
        if wp.0 == WAKE_TREE {
            let events = b.update(h);
            raise_later(h, events);
        } else {
            for (node, action) in b.queue.drain() {
                h.input(Input::A11y(node, action));
            }
        }
    });
    LRESULT(0)
}

/// Starts watching the keyboard focus (out of context: no code is loaded into any other process). The hooks Windows
/// refuses are left out.
fn watch_focus() -> Vec<HWINEVENTHOOK> {
    // SAFETY: both read ids; no precondition.
    let (process, thread) = unsafe { (GetCurrentProcessId(), GetCurrentThreadId()) };
    let hook = |(event, process, thread)| {
        // SAFETY: an out-of-context hook; on_focus_event is a plain function.
        let h = unsafe {
            SetWinEventHook(event, event, None, Some(on_focus_event), process, thread, WINEVENT_OUTOFCONTEXT)
        };
        (!h.0.is_null()).then_some(h)
    };
    watches(process, thread).into_iter().filter_map(hook).collect()
}

/// The focus watches as (event, process, thread), 0 for any: the focus moving onto a window of this thread, and every
/// foreground change of the session, the only sign this thread gets of another one taking the focus
/// (EVENT_OBJECT_FOCUS is raised on the thread that gains it). Each wakes the thread once a client is connected.
fn watches(process: u32, thread: u32) -> [(u32, u32, u32); 2] {
    [(EVENT_OBJECT_FOCUS, process, thread), (EVENT_SYSTEM_FOREGROUND, 0, 0)]
}

/// A watched event: every bridge of this thread looks at its window's focus again (the window that lost the focus is
/// not the one the event names; neither activation reaches a kit hook).
unsafe extern "system" fn on_focus_event(_: HWINEVENTHOOK, _: u32, _: HWND, _: i32, _: i32, _: u32, _: u32) {
    for b in thread_windows().into_iter().filter_map(of) {
        b.focus_event().into_iter().for_each(QueuedEvents::raise);
    }
}

/// The top-level windows of this thread (every kit window is one).
fn thread_windows() -> Vec<HWND> {
    unsafe extern "system" fn each(hwnd: HWND, lp: LPARAM) -> BOOL {
        // SAFETY: `lp` is the address of the Vec below, alive for the enumeration.
        let out = unsafe { &mut *(lp.0 as *mut Vec<HWND>) };
        out.push(hwnd);
        BOOL(1)
    }
    let mut out: Vec<HWND> = Vec::new();
    let lp = LPARAM(std::ptr::from_mut(&mut out) as isize);
    // SAFETY: the callback only pushes into `out`, which outlives the call.
    let _ = unsafe { EnumThreadWindows(GetCurrentThreadId(), Some(each), lp) };
    out
}

/// The lazy activation: the first WM_GETOBJECT builds the whole tree from the window's last layout.
struct Activation {
    hwnd: HWND,
    app: &'static str,
    /// The tree handed over.
    tree: Option<A11yTree>,
}

impl ActivationHandler for Activation {
    fn request_initial_tree(&mut self) -> Option<TreeUpdate> {
        // A WM_GETOBJECT sent while a handler of the window runs finds its state borrowed: the client gets a
        // placeholder and the tree follows by WM_OLED_A11Y.
        let tree = window::with_host(self.hwnd, |h| snapshot(h))?;
        let upd = update_of(&tree, tree.nodes.iter().collect(), true, self.app);
        self.tree = Some(tree);
        Some(upd)
    }
}

/// The action handler AccessKit calls on a client's thread: it queues the request and wakes the UI thread once.
struct Handler<P: Fn() -> bool + Send + 'static> {
    queue: Arc<ActionQueue>,
    /// Posts WM_OLED_A11Y; false when it could not.
    post: P,
}

impl<P: Fn() -> bool + Send + 'static> ActionHandler for Handler<P> {
    fn do_action(&mut self, req: ActionRequest) {
        let node = NodeId(req.target_node.0);
        if req.target_tree != TreeId::ROOT || node == ROOT {
            return;
        }
        let Some(action) = action_of(&req) else { return };
        if self.queue.push(node, action) && !(self.post)() {
            self.queue.post_failed();
        }
    }
}

/// The longest value a client may set, in bytes: a client may run in another process, and with ActionQueue::CAP this
/// bounds what the queue holds (16 MiB). No kit widget takes a value anywhere near this long.
const MAX_VALUE: usize = 64 * 1024;

/// The kit's name for a client's request; None for the ones no kit widget answers.
fn action_of(req: &ActionRequest) -> Option<A11yAction> {
    Some(match req.action {
        Action::Click => A11yAction::Click,
        Action::Focus => A11yAction::Focus,
        Action::Expand => A11yAction::Expand,
        Action::Collapse => A11yAction::Collapse,
        Action::ScrollIntoView => A11yAction::ScrollIntoView,
        Action::Increment => A11yAction::Increment,
        Action::Decrement => A11yAction::Decrement,
        Action::ShowContextMenu => A11yAction::ShowContextMenu,
        Action::SetValue => match &req.data {
            // A longer one is refused before it is copied.
            Some(ActionData::Value(s)) if s.len() <= MAX_VALUE => A11yAction::SetValue(s.to_string()),
            Some(ActionData::NumericValue(v)) => A11yAction::SetValue(v.to_string()),
            _ => return None,
        },
        _ => return None,
    })
}

/// The update that carries `nodes`; a whole one also carries the tree's root and toolkit.
fn update_of(tree: &A11yTree, nodes: Vec<&A11yNode>, whole: bool, app: &'static str) -> TreeUpdate {
    TreeUpdate {
        nodes: nodes.into_iter().map(|n| (ak(n.id), node_of(n, app))).collect(),
        tree: whole.then(|| {
            let mut info = TreeInfo::new(ak(ROOT));
            info.toolkit_name = Some(TOOLKIT.to_string());
            info
        }),
        tree_id: TreeId::ROOT,
        focus: ak(tree.focus.unwrap_or(ROOT)),
    }
}

fn ak(id: NodeId) -> AkId {
    AkId(id.0)
}

/// The kit's actions and AccessKit's names for them (Actions::CLOSE has none: a closable item has its own button).
const ACTIONS: [(Actions, Action); 9] = [
    (Actions::CLICK, Action::Click),
    (Actions::FOCUS, Action::Focus),
    (Actions::EXPAND, Action::Expand),
    (Actions::COLLAPSE, Action::Collapse),
    (Actions::SCROLL_INTO_VIEW, Action::ScrollIntoView),
    (Actions::INCREMENT, Action::Increment),
    (Actions::DECREMENT, Action::Decrement),
    (Actions::SET_VALUE, Action::SetValue),
    (Actions::CONTEXT_MENU, Action::ShowContextMenu),
];

/// AccessKit's role for a kit role (SPEC-platform 5.3.7).
fn role_of(r: Role) -> AkRole {
    match r {
        Role::Window => AkRole::Window,
        Role::Group => AkRole::Group,
        Role::Pane => AkRole::Pane,
        Role::Button => AkRole::Button,
        Role::CheckBox => AkRole::CheckBox,
        Role::Switch => AkRole::Switch,
        Role::Slider => AkRole::Slider,
        Role::RadioGroup => AkRole::RadioGroup,
        Role::Radio => AkRole::RadioButton,
        Role::TextInput => AkRole::TextInput,
        Role::SearchInput => AkRole::SearchInput,
        Role::Label => AkRole::Label,
        Role::Image => AkRole::Image,
        Role::List => AkRole::List,
        Role::ListItem => AkRole::ListItem,
        Role::Table => AkRole::Table,
        Role::Row => AkRole::Row,
        Role::Cell => AkRole::Cell,
        Role::ColumnHeader => AkRole::ColumnHeader,
        Role::Tree => AkRole::Tree,
        Role::TreeItem => AkRole::TreeItem,
        Role::TabList => AkRole::TabList,
        Role::Tab => AkRole::Tab,
        Role::TabPanel => AkRole::TabPanel,
        Role::Menu => AkRole::Menu,
        Role::MenuItem => AkRole::MenuItem,
        Role::MenuItemCheckBox => AkRole::MenuItemCheckBox,
        // AccessKit has no plain separator: its Splitter is UIA's Separator.
        Role::Separator | Role::Splitter => AkRole::Splitter,
        Role::Toolbar => AkRole::Toolbar,
        Role::Tooltip => AkRole::Tooltip,
        Role::Alert => AkRole::Alert,
        Role::Dialog => AkRole::Dialog,
        Role::ProgressIndicator => AkRole::ProgressIndicator,
        Role::ScrollBar => AkRole::ScrollBar,
        Role::Status => AkRole::Status,
        Role::Link => AkRole::Link,
        Role::TitleBar => AkRole::TitleBar,
    }
}

/// The roles whose items are selected or not: the state is always set on them.
fn selectable(r: Role) -> bool {
    matches!(r, Role::Row | Role::ListItem | Role::Tab | Role::TreeItem | Role::Cell)
}

/// The roles that are checked or not: the state is always set on them.
fn checkable(r: Role) -> bool {
    matches!(r, Role::CheckBox | Role::Switch | Role::Radio | Role::MenuItemCheckBox)
}

/// The AccessKit node of a tree node.
fn node_of(n: &A11yNode, app: &'static str) -> Node {
    let mut node = Node::new(role_of(n.role));
    if n.id == ROOT {
        node.set_author_id(app);
    }
    match (&n.name, n.role) {
        // A label's name is its value in AccessKit.
        (Some(t), Role::Label) => node.set_value(t.as_ref()),
        (Some(t), _) => node.set_label(t.as_ref()),
        (None, _) => {}
    }
    if let Some(d) = &n.description {
        node.set_description(d.as_ref());
    }
    let f = n.flags;
    if f.contains(NodeFlags::DISABLED) {
        node.set_disabled();
    }
    if f.contains(NodeFlags::BUSY) {
        node.set_busy();
    }
    if f.contains(NodeFlags::INVALID) {
        node.set_invalid(Invalid::True);
    }
    if f.contains(NodeFlags::EXPANDABLE) || f.contains(NodeFlags::EXPANDED) {
        node.set_expanded(f.contains(NodeFlags::EXPANDED));
    }
    if selectable(n.role) || f.contains(NodeFlags::SELECTED) {
        node.set_selected(f.contains(NodeFlags::SELECTED));
    }
    let toggle = match &n.value {
        Some(NodeValue::Toggle(on)) => Some(*on),
        _ => None,
    };
    if f.contains(NodeFlags::MIXED) {
        node.set_toggled(Toggled::Mixed);
    } else if f.contains(NodeFlags::CHECKED) || toggle == Some(true) {
        node.set_toggled(Toggled::True);
    } else if toggle == Some(false) || checkable(n.role) {
        node.set_toggled(Toggled::False);
    }
    match &n.value {
        Some(NodeValue::Text(t)) if n.role != Role::Label => node.set_value(t.as_ref()),
        Some(NodeValue::Range { value, min, max, step }) => {
            node.set_numeric_value(*value);
            node.set_min_numeric_value(*min);
            node.set_max_numeric_value(*max);
            if *step > 0.0 {
                node.set_numeric_value_step(*step);
            }
        }
        Some(NodeValue::Percent(p)) => {
            node.set_numeric_value(f64::from(*p));
            node.set_min_numeric_value(0.0);
            node.set_max_numeric_value(100.0);
        }
        // AccessKit's level is 0-based, the kit's 1-based.
        Some(NodeValue::Level(l)) => node.set_level(usize::from(l.saturating_sub(1))),
        _ => {}
    }
    if let Some(p) = n.position {
        // 0-based in AccessKit; UIA's PositionInSet adds the 1 back.
        let i = p.saturating_sub(1) as usize;
        node.set_position_in_set(i);
        if n.role == Role::Row {
            node.set_row_index(i);
        }
    }
    if let Some(s) = n.set_size {
        node.set_size_of_set(s as usize);
        if n.role == Role::Table {
            node.set_row_count(s as usize);
        }
    }
    if n.focusable {
        node.add_action(Action::Focus);
    }
    for (bit, action) in ACTIONS {
        if n.actions.0 & bit.0 != 0 {
            node.add_action(action);
        }
    }
    if n.role == Role::Alert {
        node.set_live(Live::Polite);
    }
    let b = n.bounds;
    node.set_bounds(Rect { x0: f64::from(b.l), y0: f64::from(b.t), x1: f64::from(b.r), y1: f64::from(b.b) });
    node.set_children(n.children.iter().map(|c| ak(*c)).collect::<Vec<_>>());
    node
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicU32, Ordering};

    use super::*;
    use crate::geom::RectP;
    use crate::node::{Frame, Node as KitNode, Text, WidgetId};

    fn frame() -> (Frame, impl Fn(u32) -> NodeId) {
        let id = |s| NodeId::new(WidgetId(2), s);
        let r = RectP::wh(10, 20, 100, 28);
        let mut f = Frame::new();
        let table = NodeValue::Position { index: 512, count: 100_000 };
        f.push(KitNode::new(id(0), Role::Table, r).label(Text::from("Files")).value(table));
        let row = NodeValue::Position { index: 512, count: 100_000 };
        let flags = NodeFlags::SELECTED | NodeFlags::EXPANDABLE;
        let row = KitNode::new(id(512), Role::Row, r).parent(id(0)).label(Text::from("item 511")).value(row);
        f.push(row.flags(flags).focus(crate::node::Focusable::Tab).actions(Actions::CLICK | Actions::EXPAND));
        f.push(KitNode::new(id(1), Role::Label, r).label(Text::from("Name")));
        f.push(KitNode::new(id(2), Role::CheckBox, r).label(Text::from("Hidden")).value(NodeValue::Toggle(true)));
        let range = NodeValue::Range { value: 25.0, min: 0.0, max: 100.0, step: 5.0 };
        f.push(KitNode::new(id(3), Role::Slider, r).label(Text::from("Volume")).value(range));
        f.push(KitNode::new(id(4), Role::Button, r).label(Text::from("OK")).flags(NodeFlags::DISABLED));
        f.push(KitNode::new(id(5), Role::Alert, r).label(Text::from("Copied")));
        f.push(KitNode::new(id(6), Role::TreeItem, r).label(Text::from("Docs")).value(NodeValue::Level(2)));
        (f, id)
    }

    #[test]
    fn nodes_map_to_accesskit() {
        let (f, id) = frame();
        let t = tree::build(&f, RectP::wh(0, 0, 640, 480), Some(id(512)));
        let u = update_of(&t, t.nodes.iter().collect(), true, "gallery");
        assert_eq!(u.tree.as_ref().map(|i| (i.root, i.toolkit_name.as_deref())), Some((ak(ROOT), Some(TOOLKIT))));
        assert_eq!((u.tree_id, u.focus, u.nodes.len()), (TreeId::ROOT, ak(id(512)), t.nodes.len()));
        let get = |i: NodeId| &u.nodes.iter().find(|(k, _)| *k == ak(i)).expect("in the update").1;
        let root = get(ROOT);
        assert_eq!((root.role(), root.author_id()), (AkRole::Window, Some("gallery")));
        let table = get(id(0));
        assert_eq!(
            (table.role(), table.size_of_set(), table.row_count()),
            (AkRole::Table, Some(100_000), Some(100_000))
        );
        assert_eq!(table.children(), &[ak(id(512))]);
        let row = get(id(512));
        assert_eq!((row.role(), row.label()), (AkRole::Row, Some("item 511")));
        assert_eq!((row.position_in_set(), row.row_index()), (Some(511), Some(511)), "UIA adds the 1 back: 512");
        assert_eq!((row.is_selected(), row.is_expanded()), (Some(true), Some(false)));
        assert!(row.supports_action(Action::Focus) && row.supports_action(Action::Click));
        assert!(row.supports_action(Action::Expand) && !row.supports_action(Action::Collapse));
        assert_eq!(row.bounds(), Some(Rect { x0: 10.0, y0: 20.0, x1: 110.0, y1: 48.0 }));
        let label = get(id(1));
        assert_eq!((label.role(), label.value(), label.label()), (AkRole::Label, Some("Name"), None));
        assert_eq!(get(id(2)).toggled(), Some(Toggled::True));
        let slider = get(id(3));
        assert_eq!(
            (slider.numeric_value(), slider.max_numeric_value(), slider.numeric_value_step()),
            (Some(25.0), Some(100.0), Some(5.0))
        );
        assert!(get(id(4)).is_disabled());
        assert_eq!(get(id(5)).live(), Some(Live::Polite));
        assert_eq!(get(id(6)).level(), Some(1), "0-based in AccessKit");
    }

    #[test]
    fn handler_queues_and_posts_once() {
        let queue = Arc::new(ActionQueue::default());
        let posts = Arc::new(AtomicU32::new(0));
        let p = Arc::clone(&posts);
        let post = move || {
            p.fetch_add(1, Ordering::SeqCst);
            true
        };
        let mut h = Handler { queue: Arc::clone(&queue), post };
        let node = NodeId::new(WidgetId(2), 5);
        let req = |action, data| ActionRequest { action, target_tree: TreeId::ROOT, target_node: ak(node), data };
        h.do_action(req(Action::Click, None));
        h.do_action(req(Action::SetValue, Some(ActionData::NumericValue(25.0))));
        h.do_action(req(Action::Blur, None));
        h.do_action(ActionRequest { target_node: ak(ROOT), ..req(Action::Focus, None) });
        assert_eq!(posts.load(Ordering::SeqCst), 1, "one wake-up for the burst");
        let got = queue.drain();
        assert_eq!(got, vec![(node, A11yAction::Click), (node, A11yAction::SetValue("25".into()))]);
    }

    #[test]
    fn an_overlong_value_is_refused() {
        let queue = Arc::new(ActionQueue::default());
        let mut h = Handler { queue: Arc::clone(&queue), post: || true };
        let node = NodeId::new(WidgetId(2), 5);
        let set = |s: String| ActionRequest {
            action: Action::SetValue,
            target_tree: TreeId::ROOT,
            target_node: ak(node),
            data: Some(ActionData::Value(s.into())),
        };
        h.do_action(set("x".repeat(MAX_VALUE + 1)));
        // Two bytes a character: exactly MAX_VALUE bytes still passes.
        h.do_action(set("\u{e9}".repeat(MAX_VALUE / 2)));
        let got: Vec<_> = queue
            .drain()
            .into_iter()
            .map(|(n, a)| (n, matches!(a, A11yAction::SetValue(s) if s.len() == MAX_VALUE)))
            .collect();
        assert_eq!(got, vec![(node, true)], "only the value of MAX_VALUE bytes reached the queue");
    }

    #[test]
    fn losing_the_focus_to_another_thread_is_seen() {
        // EVENT_OBJECT_FOCUS is raised on the thread that gains the focus: only the foreground watch, system-wide,
        // tells this thread that another one took it.
        let w = watches(7, 9);
        assert!(w.contains(&(EVENT_OBJECT_FOCUS, 7, 9)));
        assert!(w.contains(&(EVENT_SYSTEM_FOREGROUND, 0, 0)), "the foreground watch covers every process");
        let (me, other) = (HWND(8 as *mut c_void), HWND(16 as *mut c_void));
        assert!(holds_focus(me, me, 9, 9));
        assert!(!holds_focus(me, me, 4, 9), "another thread owns the foreground: the stale focus does not count");
        assert!(!holds_focus(me, other, 9, 9));
    }
}
