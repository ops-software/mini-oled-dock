//! About (P3-files-app 2.2, F4.15): the More menu's About opens a kit Dialog over the window, titled "<name> Beta"
//! while STAGE_BETA (the name alone after it), with the version and licence line and one Close button. Close or Esc
//! shuts it; while it is open it takes every key, click and command, so nothing behind it acts.

use oled_ui::node::NodeId;
use oled_ui::widgets::{Dialog, DialogOut, DialogSpec};
use oled_ui::{CommandId, Event, EventCx, LayoutCx, PaintCx, RectP, Reply, UiLang, Widget, WidgetId};

use crate::keys::Cmd;
use crate::names;
use crate::nav::Page;
use crate::tab;
use crate::tr::{k, tf};
use crate::window::FilesScreen;

/// The dialog; its Close button is the next id.
pub const ID_DIALOG: WidgetId = WidgetId(390);
/// What the Close button answers (the dialog's own, never dispatched).
const CLOSE: CommandId = CommandId(0x2F00);

/// The About dialog of a window (a field of FilesScreen: the owner of the frozen struct has no other place for it).
#[derive(Default)]
pub struct About {
    open: bool,
    /// The dialog and the language it was built in (a language change builds it again).
    built: Option<(UiLang, Dialog)>,
}

impl std::fmt::Debug for About {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("About").field("open", &self.open).finish()
    }
}

impl About {
    pub fn is_open(&self) -> bool {
        self.open
    }
}

/// About: the dialog, with the focus on its Close button.
pub fn cmd(s: &mut FilesScreen, cx: &mut EventCx, c: Cmd) -> Reply {
    if c != Cmd::About {
        return Reply::None;
    }
    let lang = cx.lang();
    let d = build(lang);
    d.focus(cx);
    s.about = About { open: true, built: Some((lang, d)) };
    Reply::Relayout
}

/// The dialog open without an EventCx (the preview scene): the next layout builds it in the window's language.
pub fn show(s: &mut FilesScreen) {
    s.about = About { open: true, built: None };
}

fn build(lang: UiLang) -> Dialog {
    let name = lang.kit(oled_ui::strings::k::KIT_APP_FILES);
    let title = if names::STAGE_BETA { tf(k::FILES_APP_TITLE_BETA, lang, &[("name", &name)]) } else { name };
    let version = env!("CARGO_PKG_VERSION");
    let body = tf(k::FILES_ABOUT_BODY, lang, &[("version", version)]);
    let close = lang.kit(oled_ui::strings::k::KIT_CLOSE);
    Dialog::new(ID_DIALOG, DialogSpec { title, body, buttons: vec![(close, CLOSE)], wide: false })
}

/// Over the whole client, laid out after everything else so it is on top.
pub fn layout(s: &mut FilesScreen, cx: &mut LayoutCx, client: RectP) {
    if !s.about.open {
        return;
    }
    let lang = cx.lang();
    if s.about.built.as_ref().is_none_or(|(l, _)| *l != lang) {
        s.about.built = Some((lang, build(lang)));
    }
    if let Some((_, d)) = s.about.built.as_mut() {
        d.layout(cx, client);
    }
}

/// Painted last.
pub fn paint(s: &FilesScreen, cx: &mut PaintCx) {
    if let (true, Some((_, d))) = (s.about.open, s.about.built.as_ref()) {
        d.paint(cx);
    }
}

/// First in the window's event order: while open, the dialog sees every event and takes every input one.
pub fn event(s: &mut FilesScreen, cx: &mut EventCx, ev: &Event) -> Option<Reply> {
    if !s.about.open {
        return None;
    }
    let out = s.about.built.as_mut().and_then(|(_, d)| d.event(cx, ev));
    match out {
        Some(DialogOut::Chose(_) | DialogOut::Cancelled) => {
            close(s, cx);
            Some(Reply::Relayout)
        }
        None if is_input(ev) => Some(Reply::None),
        None => None,
    }
}

/// The keys, the pointer and the commands (the window's chords and menu picks) the dialog keeps from the window.
fn is_input(ev: &Event) -> bool {
    matches!(
        ev,
        Event::PointerMove { .. }
            | Event::PointerDown { .. }
            | Event::PointerUp { .. }
            | Event::Click { .. }
            | Event::DragStart { .. }
            | Event::DragMove { .. }
            | Event::DragEnd { .. }
            | Event::Wheel { .. }
            | Event::Key { .. }
            | Event::Text(_)
            | Event::Ime(_)
            | Event::Command(_)
    )
}

/// The dialog goes; the list takes the focus back.
fn close(s: &mut FilesScreen, cx: &mut EventCx) {
    s.about = About::default();
    if !matches!(s.tabs.active().nav.page(), Page::Home) {
        cx.set_focus(NodeId::new(tab::TABLE_ID, 0));
    }
}
