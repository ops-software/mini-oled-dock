//! Context menus in the shell host (SH5a-SH5c, F 5.4.4): resolve the items to one folder's children, build the classic
//! menu (QueryContextMenu inside a crumbs guard), let the user choose (or, in test mode, pick by verb), intercept the
//! canonical verbs the UI runs through its own paths (delete, cut, copy, paste, paste link, rename: never invoked
//! here) and invoke the rest under the busy protocol.

use oled_trust::frame::files::{CallKind, CanonVerb, ItemRef, MenuOutcome, ReqId, ShellEvt, WidePath};
use windows::core::{w, Interface, Result, PCSTR, PCWSTR, PSTR};
use windows::Win32::Foundation::{E_ABORT, E_INVALIDARG, HWND, LPARAM, LRESULT, POINT, WPARAM};
use windows::Win32::System::Com::{IBindCtx, IDataObject};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::Shell::Common::ITEMIDLIST;
use windows::Win32::UI::Shell::{
    IContextMenu, IContextMenu2, IContextMenu3, ILFindLastID, ILFree, IShellExtInit, IShellFolder, SHBindToObject,
    SHParseDisplayName, CMF_CANRENAME, CMF_EXTENDEDVERBS, CMF_NORMAL, CMIC_MASK_PTINVOKE, CMINVOKECOMMANDINFOEX,
    GCS_VERBW, SEE_MASK_FLAG_NO_UI, SEE_MASK_UNICODE,
};
use windows::Win32::UI::WindowsAndMessaging::{
    CreatePopupMenu, CreateWindowExW, DefWindowProcW, DestroyMenu, DestroyWindow, EndMenu, GetMenuItemCount,
    GetMenuItemID, GetSubMenu, GetWindowLongPtrW, KillTimer, PostMessageW, RegisterClassW, SetForegroundWindow,
    SetTimer, SetWindowLongPtrW, TrackPopupMenuEx, GWLP_USERDATA, HMENU, SW_SHOWNORMAL, TPM_RETURNCMD, TPM_RIGHTBUTTON,
    WM_CANCELMODE, WM_DRAWITEM, WM_INITMENUPOPUP, WM_MEASUREITEM, WM_MENUCHAR, WM_NULL, WM_TIMER, WNDCLASSW,
    WS_EX_TOOLWINDOW, WS_POPUP,
};

use crate::shell::crumbs;
use crate::shell::host::{fault, HostCfg};
use crate::shell::writer::Writer;

/// The class of the host's menu owner window (a 1x1 WS_POPUP tool window, owned by nobody, never shown).
pub const OWNER_CLASS: PCWSTR = w!("MiniOledFilesShell");
/// shobjidl_core.h defines these two as the SEE_MASK values of the same bits; windows 0.62.2 has only the latter.
const CMIC_MASK_UNICODE: u32 = SEE_MASK_UNICODE;
const CMIC_MASK_FLAG_NO_UI: u32 = SEE_MASK_FLAG_NO_UI;

/// The command ids QueryContextMenu may use.
pub const FIRST_ID: u32 = 1;
pub const LAST_ID: u32 = 0x7FFF;

/// One Menu request.
#[derive(Clone, Debug)]
pub struct MenuReq {
    pub req: ReqId,
    pub items: Vec<ItemRef>,
    pub at: (i32, i32),
    pub extended: bool,
    /// The UI's window: logged only, never attached to (its input queue stays its own).
    pub owner: u64,
}

/// The canonical verbs the UI runs itself (G5): a chosen one is answered Intercepted and never invoked.
pub fn canonical(verb: &str) -> Option<CanonVerb> {
    Some(match verb.to_ascii_lowercase().as_str() {
        "delete" => CanonVerb::Delete,
        "cut" => CanonVerb::Cut,
        "copy" => CanonVerb::Copy,
        "paste" => CanonVerb::Paste,
        "pastelink" => CanonVerb::PasteLink,
        "rename" => CanonVerb::Rename,
        _ => return None,
    })
}

/// The items as children of one folder: (the folder, the items), or E_INVALIDARG for no item, an item that is not a
/// path, a root, or items of two folders.
pub fn one_parent(items: &[ItemRef]) -> std::result::Result<(WidePath, Vec<WidePath>), i32> {
    let mut paths = Vec::with_capacity(items.len());
    for it in items {
        match it {
            ItemRef::Path(p) => paths.push(p.clone()),
            ItemRef::Pidl(_) => return Err(E_INVALIDARG.0),
        }
    }
    let parent = paths.first().and_then(WidePath::parent).ok_or(E_INVALIDARG.0)?;
    if paths.iter().any(|p| p.parent().as_ref() != Some(&parent)) {
        return Err(E_INVALIDARG.0);
    }
    Ok((parent, paths))
}

/// An id list from SHParseDisplayName, freed on drop.
struct Pidl(*mut ITEMIDLIST);

impl Drop for Pidl {
    fn drop(&mut self) {
        // SAFETY: allocated by SHParseDisplayName, freed once.
        unsafe { ILFree(Some(self.0)) };
    }
}

fn parse(p: &WidePath) -> Result<Pidl> {
    let name: Vec<u16> = p.units().iter().copied().chain(Some(0)).collect();
    let mut pidl = std::ptr::null_mut();
    // SAFETY: a NUL-terminated name; `pidl` receives an id list owned by the Pidl.
    unsafe { SHParseDisplayName(PCWSTR(name.as_ptr()), None::<&IBindCtx>, &mut pidl, 0, None) }?;
    Ok(Pidl(pidl))
}

/// The owner window of one menu, destroyed on drop.
pub struct Owner(pub HWND);

impl Drop for Owner {
    fn drop(&mut self) {
        // SAFETY: created on this thread by Owner::new, destroyed once.
        let _ = unsafe { DestroyWindow(self.0) };
    }
}

impl Owner {
    /// A hidden 1x1 tool window at `at` on this (the menu STA's) thread.
    pub fn new(at: (i32, i32)) -> Result<Owner> {
        static CLASS: std::sync::OnceLock<()> = std::sync::OnceLock::new();
        // SAFETY: the module handle of this exe.
        let instance = unsafe { GetModuleHandleW(None) }?;
        CLASS.get_or_init(|| {
            let wc = WNDCLASSW {
                lpfnWndProc: Some(owner_proc),
                hInstance: instance.into(),
                lpszClassName: OWNER_CLASS,
                ..Default::default()
            };
            // SAFETY: a class of static strings and a static window procedure; registered once per process.
            unsafe { RegisterClassW(&wc) };
        });
        // SAFETY: the class registered above; no parent, no menu, no creation data; never shown here.
        let hwnd = unsafe {
            CreateWindowExW(
                WS_EX_TOOLWINDOW,
                OWNER_CLASS,
                w!(""),
                WS_POPUP,
                at.0,
                at.1,
                1,
                1,
                None,
                None,
                Some(instance.into()),
                None,
            )
        }?;
        Ok(Owner(hwnd))
    }
}

/// How the owner window forwards a menu message to the extensions (owner_msg_routing).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MsgRoute {
    /// IContextMenu3::HandleMenuMsg2 only (its result is the answer).
    Msg2Only,
    /// HandleMenuMsg2 when the menu has IContextMenu3, else IContextMenu2::HandleMenuMsg.
    Msg2OrMsg,
}

/// The messages of a shown menu that belong to the extensions that own its items.
pub fn route_owner_msg(msg: u32) -> Option<MsgRoute> {
    match msg {
        WM_MENUCHAR => Some(MsgRoute::Msg2Only),
        WM_INITMENUPOPUP | WM_DRAWITEM | WM_MEASUREITEM => Some(MsgRoute::Msg2OrMsg),
        _ => None,
    }
}

/// The timer of the debug-only self-dismissal (FILES_TEST_MENU_DISMISS_MS).
const DISMISS_TIMER: usize = 1;

/// The menu on screen, for the owner window's procedure (the menu STA shows one menu at a time).
struct Shown {
    top: HMENU,
    req: ReqId,
    w: Writer,
    ext: String,
    cm2: Option<IContextMenu2>,
    cm3: Option<IContextMenu3>,
    told: bool,
}

/// The Shown of an owner window, kept in its GWLP_USERDATA while the menu is on screen (show sets and clears it).
fn shown_of(hwnd: HWND) -> Option<&'static std::cell::RefCell<Shown>> {
    // SAFETY: the window's user data is 0 or the pointer show() stored, valid until show() clears and frees it; only
    // this thread (the window's) reads it.
    let p = unsafe { GetWindowLongPtrW(hwnd, GWLP_USERDATA) } as *const std::cell::RefCell<Shown>;
    // SAFETY: as above.
    (!p.is_null()).then(|| unsafe { &*p })
}

/// The owner window's procedure: MenuShown at the top menu's first WM_INITMENUPOPUP, the extensions' menu messages
/// inside a crumbs guard, and the debug self-dismissal.
unsafe extern "system" fn owner_proc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    if msg == WM_TIMER && wp.0 == DISMISS_TIMER {
        // SAFETY: this window's own timer; posting and ending the menu of this thread need no pointers.
        unsafe {
            let _ = KillTimer(Some(hwnd), DISMISS_TIMER);
            let _ = PostMessageW(Some(hwnd), WM_CANCELMODE, WPARAM(0), LPARAM(0));
            let _ = EndMenu();
        }
        return LRESULT(0);
    }
    if msg == WM_INITMENUPOPUP {
        if let Some(cell) = shown_of(hwnd) {
            if let Ok(mut s) = cell.try_borrow_mut() {
                if !s.told && wp.0 == s.top.0 as usize {
                    s.told = true;
                    s.w.send(&ShellEvt::MenuShown { req: s.req });
                }
            }
        }
    }
    if let Some(route) = route_owner_msg(msg) {
        // The interfaces are taken out first: an extension may send messages back here while it runs.
        let found = shown_of(hwnd)
            .and_then(|cell| cell.try_borrow().ok().map(|s| (s.cm2.clone(), s.cm3.clone(), s.ext.clone())));
        if let Some((cm2, cm3, ext)) = found {
            let _g = crumbs::enter(CallKind::HandleMenuMsg, &ext);
            if let Some(cm3) = cm3 {
                let mut result = LRESULT(0);
                // SAFETY: the message as the system passed it; `result` is writable.
                if unsafe { cm3.HandleMenuMsg2(msg, wp, lp, Some(&mut result)) }.is_ok() {
                    return result;
                }
            } else if let (Some(cm2), MsgRoute::Msg2OrMsg) = (cm2, route) {
                // SAFETY: as above.
                if unsafe { cm2.HandleMenuMsg(msg, wp, lp) }.is_ok() {
                    return LRESULT(i32::from(msg != WM_INITMENUPOPUP) as isize);
                }
            }
        }
    }
    // SAFETY: the arguments the system passed.
    unsafe { DefWindowProcW(hwnd, msg, wp, lp) }
}

/// Shows `menu` at the point from the owner window and returns the chosen id (0 when dismissed). The UI allowed this
/// process to take the foreground (AllowSetForegroundWindow) before it asked.
fn show(cm: &IContextMenu, menu: HMENU, owner: HWND, m: &MenuReq, ext: &str, w: &Writer, cfg: &HostCfg) -> u32 {
    let (cm2, cm3) = {
        let _g = crumbs::enter(CallKind::HandleMenuMsg, ext);
        (cm.cast::<IContextMenu2>().ok(), cm.cast::<IContextMenu3>().ok())
    };
    let state = Box::new(std::cell::RefCell::new(Shown {
        top: menu,
        req: m.req,
        w: w.clone(),
        ext: ext.to_string(),
        cm2,
        cm3,
        told: false,
    }));
    let state = Box::into_raw(state);
    // SAFETY: this thread's own window; the pointer stays valid until it is cleared and freed below.
    unsafe { SetWindowLongPtrW(owner, GWLP_USERDATA, state as isize) };
    // SAFETY: this thread's own window and timer.
    unsafe {
        let _ = SetForegroundWindow(owner);
        if let Some(ms) = cfg.dismiss_ms {
            SetTimer(Some(owner), DISMISS_TIMER, ms, None);
        }
    }
    // SAFETY: a live menu and window of this thread's; TPM_RETURNCMD makes the result the chosen id.
    let chosen = unsafe { TrackPopupMenuEx(menu, (TPM_RETURNCMD | TPM_RIGHTBUTTON).0, m.at.0, m.at.1, owner, None) };
    // SAFETY: as above; WM_NULL lets the menu's own loop end cleanly (the documented TrackPopupMenu pattern).
    unsafe {
        let _ = KillTimer(Some(owner), DISMISS_TIMER);
        let _ = PostMessageW(Some(owner), WM_NULL, WPARAM(0), LPARAM(0));
    }
    // SAFETY: cleared first, so no message reads the pointer any more; then freed once (from Box::into_raw above).
    let shown = unsafe {
        SetWindowLongPtrW(owner, GWLP_USERDATA, 0);
        Box::from_raw(state)
    };
    let _g = crumbs::enter(CallKind::HandleMenuMsg, ext);
    drop(shown);
    u32::try_from(chosen.0).unwrap_or(0)
}

/// The menu's handlers, released inside a crumbs guard (a Release runs third-party code too).
struct Held {
    cm: Option<IContextMenu>,
    ext: String,
}

impl Held {
    fn get(&self) -> &IContextMenu {
        // Some until Drop.
        self.cm.as_ref().unwrap_or_else(|| unreachable!("taken only in Drop"))
    }
}

impl Drop for Held {
    fn drop(&mut self) {
        let _g = crumbs::enter(CallKind::QueryContextMenu, &self.ext);
        self.cm.take();
    }
}

/// A popup menu, destroyed on drop.
struct Menu(HMENU);

impl Drop for Menu {
    fn drop(&mut self) {
        // SAFETY: created by CreatePopupMenu, destroyed once.
        let _ = unsafe { DestroyMenu(self.0) };
    }
}

/// The folder's IContextMenu for `children` (the id lists stay alive in the returned Vec while it is used). With
/// FILES_FAULT_DLL (debug builds), the fault DLL's FaultMenu initialized on the same items instead (registration-free).
fn context_menu(
    parent: &WidePath,
    children: &[WidePath],
    owner: HWND,
    cfg: &HostCfg,
) -> Result<(IContextMenu, Vec<Pidl>)> {
    let folder_pidl = parse(parent)?;
    // SAFETY: a valid id list of the parent folder.
    let folder: IShellFolder = unsafe { SHBindToObject(None::<&IShellFolder>, folder_pidl.0, None::<&IBindCtx>) }?;
    let full = children.iter().map(parse).collect::<Result<Vec<Pidl>>>()?;
    // SAFETY: each id list is valid; the last ids point into them and live as long as `full`.
    let ids: Vec<*const ITEMIDLIST> = full.iter().map(|p| unsafe { ILFindLastID(p.0) }.cast_const()).collect();
    if let Some(dll) = &cfg.fault_dll {
        // SAFETY: as below, for the items' data object.
        let data: IDataObject = unsafe { folder.GetUIObjectOf(owner, &ids, None) }?;
        let init: IShellExtInit = fault::create(dll, &fault::CLSID_FAULT_MENU)?;
        // SAFETY: the folder's id list and the items' data object, both alive for the call.
        unsafe { init.Initialize(Some(folder_pidl.0.cast_const()), &data, None) }?;
        return Ok((init.cast()?, full));
    }
    // SAFETY: the children's relative ids of this folder, valid while `full` lives.
    let cm: IContextMenu = unsafe { folder.GetUIObjectOf(owner, &ids, None) }?;
    Ok((cm, full))
}

/// Invokes command `id` under the busy protocol: refused (E_ABORT) once a kill was decided; else Busy on, the call
/// inside a crumbs guard, the guard dropped, Busy off.
fn invoke(cm: &IContextMenu, id: u32, m: &MenuReq, owner: HWND, ext: &str, w: &Writer, cfg: &HostCfg) -> MenuOutcome {
    let Ok(busy) = crumbs::busy_enter() else {
        return MenuOutcome::Failed(E_ABORT.0);
    };
    w.send(&ShellEvt::Busy { on: true });
    let verb = (id - FIRST_ID) as usize;
    let mask = CMIC_MASK_UNICODE | CMIC_MASK_PTINVOKE | if cfg.test_no_ui { CMIC_MASK_FLAG_NO_UI } else { 0 };
    let info = CMINVOKECOMMANDINFOEX {
        cbSize: std::mem::size_of::<CMINVOKECOMMANDINFOEX>() as u32,
        fMask: mask,
        hwnd: owner,
        lpVerb: PCSTR(verb as *const u8),
        lpVerbW: PCWSTR(verb as *const u16),
        nShow: SW_SHOWNORMAL.0,
        ptInvoke: POINT { x: m.at.0, y: m.at.1 },
        ..Default::default()
    };
    let done = {
        let _g = crumbs::enter(CallKind::InvokeCommand, ext);
        // SAFETY: an EX structure whose head is the plain one, sized in cbSize; the verb is an id, not a pointer.
        unsafe { cm.InvokeCommand(std::ptr::from_ref(&info).cast()) }
    };
    drop(busy);
    w.send(&ShellEvt::Busy { on: false });
    match done {
        Ok(()) => MenuOutcome::Invoked,
        Err(e) => MenuOutcome::Failed(e.code().0),
    }
}

/// The language-independent verb of command `id`, inside a crumbs guard.
fn verb_of(cm: &IContextMenu, id: u32, ext: &str) -> Option<String> {
    let mut buf = [0u16; 260];
    let _g = crumbs::enter(CallKind::VerbName, ext);
    // SAFETY: for GCS_VERBW the buffer holds 260 UTF-16 units, as passed.
    unsafe {
        cm.GetCommandString((id - FIRST_ID) as usize, GCS_VERBW, None, PSTR(buf.as_mut_ptr().cast()), buf.len() as u32)
    }
    .ok()?;
    let n = buf.iter().position(|&u| u == 0).unwrap_or(buf.len());
    Some(String::from_utf16_lossy(buf.get(..n).unwrap_or_default()))
}

/// The id whose verb is `want` (case ignored), searching submenus too.
fn find_verb(cm: &IContextMenu, menu: HMENU, ext: &str, want: &str) -> Option<u32> {
    // SAFETY: a live menu of this thread's.
    let n = unsafe { GetMenuItemCount(Some(menu)) };
    for i in 0..n.max(0) {
        // SAFETY: as above; `i` is a position in it.
        let sub = unsafe { GetSubMenu(menu, i) };
        if !sub.0.is_null() {
            if let Some(id) = find_verb(cm, sub, ext, want) {
                return Some(id);
            }
            continue;
        }
        // SAFETY: as above.
        let id = unsafe { GetMenuItemID(menu, i) };
        if (FIRST_ID..=LAST_ID).contains(&id) && verb_of(cm, id, ext).is_some_and(|v| v.eq_ignore_ascii_case(want)) {
            return Some(id);
        }
    }
    None
}

/// Serves one Menu request on the menu STA.
pub fn run(m: MenuReq, w: &Writer, cfg: &HostCfg) {
    let req = m.req;
    match menu(m, w, cfg) {
        Ok(done) => w.send(&done),
        Err(hr) => w.send(&ShellEvt::Failed { req, hr }),
    }
}

fn menu(m: MenuReq, w: &Writer, cfg: &HostCfg) -> std::result::Result<ShellEvt, i32> {
    let (parent, children) = one_parent(&m.items)?;
    let ext = children.first().map(crumbs::ext_of).unwrap_or_default();
    oled_sys::log!("shell: menu {} for {} item(s), owner {:#x}", m.req.0, children.len(), m.owner);
    let owner = Owner::new(m.at).map_err(|e| e.code().0)?;
    // SAFETY: no preconditions; destroyed by Menu's drop (after the menu's handlers are released below).
    let menu = Menu(unsafe { CreatePopupMenu() }.map_err(|e| e.code().0)?);
    let flags = CMF_NORMAL | CMF_CANRENAME | if m.extended { CMF_EXTENDEDVERBS } else { 0 };
    // Building the menu loads and runs the handlers: one QueryContextMenu crumb covers it all.
    let (cm, _ids) = {
        let _g = crumbs::enter(CallKind::QueryContextMenu, &ext);
        let (cm, ids) = context_menu(&parent, &children, owner.0, cfg).map_err(|e| e.code().0)?;
        let cm = Held { cm: Some(cm), ext: ext.clone() };
        // SAFETY: a live menu of this thread's and the id range it may use.
        let hr = unsafe { cm.get().QueryContextMenu(menu.0, 0, FIRST_ID, LAST_ID, flags) };
        if hr.is_err() {
            return Err(hr.0);
        }
        (cm, ids)
    };
    let cm = cm.get();
    let chosen = if cfg.test_no_ui {
        w.send(&ShellEvt::MenuShown { req: m.req });
        cfg.pick.as_deref().and_then(|p| find_verb(cm, menu.0, &ext, p)).unwrap_or(0)
    } else {
        show(cm, menu.0, owner.0, &m, &ext, w, cfg)
    };
    if chosen == 0 {
        return Ok(ShellEvt::MenuDone { req: m.req, outcome: MenuOutcome::Dismissed, items: Vec::new() });
    }
    let verb = verb_of(cm, chosen, &ext).unwrap_or_default();
    if let Some(cv) = canonical(&verb) {
        return Ok(ShellEvt::MenuDone { req: m.req, outcome: MenuOutcome::Intercepted(cv), items: m.items });
    }
    let outcome = invoke(cm, chosen, &m, owner.0, &ext, w, cfg);
    Ok(ShellEvt::MenuDone { req: m.req, outcome, items: Vec::new() })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn canonical_verbs_table() {
        for (v, want) in [
            ("delete", Some(CanonVerb::Delete)),
            ("Delete", Some(CanonVerb::Delete)),
            ("cut", Some(CanonVerb::Cut)),
            ("copy", Some(CanonVerb::Copy)),
            ("paste", Some(CanonVerb::Paste)),
            ("pastelink", Some(CanonVerb::PasteLink)),
            ("RENAME", Some(CanonVerb::Rename)),
            ("open", None),
            ("properties", None),
            ("modfault.write", None),
            ("copyto", None),
            ("", None),
        ] {
            assert_eq!(canonical(v), want, "{v}");
        }
    }

    #[test]
    fn owner_msg_routing() {
        use windows::Win32::UI::WindowsAndMessaging::{WM_COMMAND, WM_MENUSELECT, WM_PAINT};
        assert_eq!(route_owner_msg(WM_MENUCHAR), Some(MsgRoute::Msg2Only), "only HandleMenuMsg2 answers it");
        for m in [WM_INITMENUPOPUP, WM_DRAWITEM, WM_MEASUREITEM] {
            assert_eq!(route_owner_msg(m), Some(MsgRoute::Msg2OrMsg), "{m:#x}");
        }
        for m in [WM_TIMER, WM_COMMAND, WM_MENUSELECT, WM_PAINT, WM_CANCELMODE, WM_NULL] {
            assert_eq!(route_owner_msg(m), None, "{m:#x}");
        }
    }

    #[test]
    fn items_must_share_one_parent() {
        let p = |s: &str| ItemRef::Path(WidePath::parse(s).unwrap());
        let (parent, kids) = one_parent(&[p(r"C:\a\x.txt"), p(r"C:\a\y")]).unwrap();
        assert_eq!(parent, WidePath::parse(r"C:\a").unwrap());
        assert_eq!(kids.len(), 2);
        assert_eq!(one_parent(&[p(r"C:\a\x.txt"), p(r"C:\b\y")]), Err(E_INVALIDARG.0), "two folders");
        assert_eq!(one_parent(&[]), Err(E_INVALIDARG.0), "no item");
        assert_eq!(one_parent(&[p(r"C:\")]), Err(E_INVALIDARG.0), "a root has no parent folder here");
        assert_eq!(one_parent(&[ItemRef::Pidl(Box::new([1, 2]))]), Err(E_INVALIDARG.0), "no id lists");
        assert!(one_parent(&[p(r"C:\x.txt")]).is_ok(), "a file at a root");
    }
}
