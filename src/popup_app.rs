//! App MENU popup (right click on an app item): header with the name, separator, the app's own
//! jump list when it has one (`jumplist`: its categories, then its tasks, as the native taskbar
//! shows them), then "New window", "Pin to dock" / "Unpin from dock", "Close window(s)". Same
//! style as the power popup. A dock choice is communicated to the dock with
//! `WM_DOCK_APP_MENU_CMD` AFTER the popup closes; a jump list entry is started then, through
//! `ShellRequest::Launch` (the desktop shell, never elevated).
//!
//! Declared as a child module of `popup` (`#[path]`).

use windows::Win32::Foundation::{LPARAM, WPARAM};
use windows::Win32::UI::WindowsAndMessaging::PostMessageW;

use crate::jumplist::Entry;
use crate::log;
use crate::msg::{ID_MENU_CLOSE_ALL, ID_MENU_NEW_WINDOW, ID_MENU_PIN, ID_MENU_UNPIN, WM_DOCK_APP_MENU_CMD};
use crate::popup::{center_y, hline, lift, row_plate, AppMenuInfo, Content, HitMap, PopupKind, Reply};
use crate::surface::{rect_wh, Font, Surface, TEXT2_ALPHA, TEXT3_ALPHA, TEXT_ALPHA, WHITE};

/// Id of the first jump list row; the following ones count up in menu order (`AppPopup::links`).
const ID_JUMP_BASE: u32 = 100;

/// What a line of the menu is.
enum Line {
    /// Name of a jump list category.
    Head(String),
    Sep,
    Row { id: u32, text: String, enabled: bool },
}

pub(crate) struct AppPopup {
    info: AppMenuInfo,
    hits: HitMap,
    /// Command to send after closing.
    pending: Option<u32>,
    /// Jump list entry to start after closing: `(target, arguments)`.
    launch: Option<(String, String)>,
    /// `(target, arguments)` of the jump list rows, by `id - ID_JUMP_BASE`.
    links: Vec<(String, String)>,
    /// The lines below the header with their top edge, laid out by `size` (physical px).
    lines: Vec<(Line, i32)>,
    /// Cached geometry (physical px).
    sep_y: i32,
    pad_text: i32,
    width: i32,
    /// Name already shortened.
    title: String,
}

/// Creates the content (initial state).
pub(crate) fn new(info: AppMenuInfo) -> Box<dyn Content> {
    Box::new(AppPopup {
        info,
        hits: HitMap::default(),
        pending: None,
        launch: None,
        links: Vec::new(),
        lines: Vec::new(),
        sep_y: 0,
        pad_text: 0,
        width: 0,
        title: String::new(),
    })
}

impl AppPopup {
    /// The dock's own rows in order: (id, text, enabled).
    fn rows(&self) -> Vec<(u32, String, bool)> {
        let mut v = Vec::new();
        if self.info.can_launch {
            v.push((ID_MENU_NEW_WINDOW, crate::i18n::tr("New window", "Nuova finestra").to_string(), true));
        }
        if self.info.pinned {
            v.push((ID_MENU_UNPIN, crate::i18n::tr("Unpin from dock", "Rimuovi dal dock").to_string(), true));
        } else {
            v.push((ID_MENU_PIN, crate::i18n::tr("Pin to dock", "Aggiungi al dock").to_string(), self.info.can_launch));
        }
        let close = match self.info.windows {
            0 | 1 => crate::i18n::tr("Close window", "Chiudi finestra").to_string(),
            n if crate::i18n::italian() => format!("Chiudi {n} finestre"),
            n => format!("Close {n} windows"),
        };
        v.push((ID_MENU_CLOSE_ALL, close, self.info.windows > 0));
        v
    }

    /// Every line below the header: the jump list (if any) closed by a separator, then `rows`.
    /// Fills `links` with what each jump list row starts.
    fn all_lines(&mut self) -> Vec<Line> {
        let mut v = Vec::new();
        self.links.clear();
        for g in &self.info.jump {
            let name = if g.name.is_empty() { crate::i18n::tr("Tasks", "Attivit\u{e0}") } else { g.name.as_str() };
            v.push(Line::Head(name.to_string()));
            for e in &g.entries {
                match e {
                    Entry::Separator => v.push(Line::Sep),
                    Entry::Link { title, target, args } => {
                        let id = ID_JUMP_BASE + self.links.len() as u32;
                        self.links.push((target.clone(), args.clone()));
                        v.push(Line::Row { id, text: title.clone(), enabled: true });
                    }
                }
            }
        }
        if !v.is_empty() {
            v.push(Line::Sep);
        }
        v.extend(self.rows().into_iter().map(|(id, text, enabled)| Line::Row { id, text, enabled }));
        v
    }
}

impl Content for AppPopup {
    fn kind(&self) -> PopupKind {
        PopupKind::AppMenu
    }

    fn size(&mut self, s: &Surface) -> (i32, i32) {
        let m = s.metrics();
        let (inset, row, head, gap) = (m.px(8), m.px(34), m.px(24), m.px(2));
        self.pad_text = m.px(12);
        let mut lines = self.all_lines();
        // Width = widest text + padding, within min/max (wider when the app brings its own rows).
        let mut tw = s.text_width(&self.info.name, Font::Bold);
        for l in &lines {
            tw = tw.max(match l {
                Line::Row { text, .. } => s.text_width(text, Font::Normal),
                Line::Head(text) => s.text_width(text, Font::Small),
                Line::Sep => 0,
            });
        }
        let max_w = if self.links.is_empty() { m.px(300) } else { m.px(340) };
        let w = (tw + 2 * self.pad_text + 2 * inset).clamp(m.px(190), max_w);
        self.width = w;
        let text_w = w - 2 * inset - 2 * self.pad_text;
        self.title = s.ellipsize(&self.info.name, Font::Bold, text_w);
        self.hits.clear();
        let mut y = inset + row;
        self.sep_y = y + gap;
        y += gap * 2 + m.px(1);
        self.lines.clear();
        for mut l in lines.drain(..) {
            let h = match &mut l {
                Line::Row { id, text, enabled } => {
                    *text = s.ellipsize(text, Font::Normal, text_w);
                    if *enabled {
                        self.hits.add(*id, rect_wh(inset, y, w - 2 * inset, row));
                    }
                    row
                }
                Line::Head(text) => {
                    *text = s.ellipsize(text, Font::Small, text_w);
                    head
                }
                Line::Sep => gap * 2 + m.px(1),
            };
            self.lines.push((l, y));
            y += h;
        }
        (w, y + inset)
    }

    fn paint(&self, s: &mut Surface, hot: Option<u32>, pressed: Option<u32>) {
        s.panel();
        let m = s.metrics();
        let (inset, row, head, gap) = (m.px(8), m.px(34), m.px(24), m.px(2));
        let (x0, x1) = (inset + self.pad_text, self.width - inset - self.pad_text);
        let top = rect_wh(inset, inset, self.width - 2 * inset, row);
        let hy = center_y(&top, s.line_height(Font::Bold));
        s.text(&self.title, x0, hy, Font::Bold, WHITE, TEXT2_ALPHA);
        hline(s, x0, x1, self.sep_y);
        for (line, y) in &self.lines {
            match line {
                Line::Row { id, text, enabled } => {
                    let rc = rect_wh(inset, *y, self.width - 2 * inset, row);
                    let h = *enabled && row_plate(s, rc, *id, hot, pressed);
                    let alpha = if *enabled { TEXT_ALPHA } else { TEXT2_ALPHA };
                    let ty = center_y(&rc, s.line_height(Font::Normal));
                    s.text(text, x0, ty, Font::Normal, WHITE, lift(alpha, h));
                }
                Line::Head(text) => {
                    let rc = rect_wh(inset, *y, self.width - 2 * inset, head);
                    let ty = center_y(&rc, s.line_height(Font::Small));
                    s.text(text, x0, ty, Font::Small, WHITE, TEXT3_ALPHA);
                }
                Line::Sep => hline(s, x0, x1, *y + gap),
            }
        }
    }

    fn hit(&self, x: i32, y: i32) -> Option<u32> {
        self.hits.at(x, y)
    }

    fn click(&mut self, id: u32) -> Reply {
        match id.checked_sub(ID_JUMP_BASE) {
            Some(i) => self.launch = self.links.get(i as usize).cloned(),
            None => {
                self.pending = Some(id);
                crate::popup::set_app_menu_key(self.info.key.clone());
            }
        }
        Reply::Close
    }

    fn after_close(&mut self) {
        if let Some((target, args)) = self.launch.take() {
            let args = Some(args).filter(|a| !a.is_empty());
            crate::shell_thread::request(crate::shell_thread::ShellRequest::Launch { target, args });
        }
        if let Some(cmd) = self.pending.take() {
            // SAFETY: PostMessageW to the dock window; if it no longer exists it just fails.
            if let Err(e) =
                unsafe { PostMessageW(Some(crate::hwnd()), WM_DOCK_APP_MENU_CMD, WPARAM(cmd as usize), LPARAM(0)) }
            {
                log!("popup_app: PostMessageW(WM_DOCK_APP_MENU_CMD) failed: {e}");
            }
        }
    }
}
