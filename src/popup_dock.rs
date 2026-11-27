//! The dock menu (SPEC-dock 5.2.1): right click on empty bar space. Header "Dock", then Auto-hide (a check at the
//! right edge while it is on), Task Manager and Settings. Same look as the app menu (`popup_app`): rows of 34 px, inset
//! 8, text column pad 12. A click stores the row and closes; the action runs in `after_close`, with the popup hidden.
//!
//! Declared as a child module of `popup` (`#[path]`).

use windows::Win32::Foundation::{LPARAM, RECT, WPARAM};
use windows::Win32::UI::WindowsAndMessaging::PostMessageW;

use crate::i18n::{k, t};
use crate::log;
use crate::msg::WM_DOCK_CONFIG_APPLY;
use crate::popup::{center_y, hline, lift, row_plate, Content, HitMap, PopupKind, Reply};
use crate::surface::{rect_wh, Font, Surface, TEXT2_ALPHA, TEXT_ALPHA, WHITE};

/// Row ids.
const ID_AUTOHIDE: u32 = 1;
const ID_TASKMGR: u32 = 2;
const ID_SETTINGS: u32 = 3;

/// The rows in order: (id, label, check shown). Pure, for the layout, the paint and the tests.
fn rows(autohide: bool) -> [(u32, &'static str, bool); 3] {
    [
        (ID_AUTOHIDE, t(k::COMMON_AUTO_HIDE), autohide),
        (ID_TASKMGR, t(k::DOCK_TASK_MANAGER), false),
        (ID_SETTINGS, t(k::COMMON_SETTINGS), false),
    ]
}

pub(crate) struct DockMenu {
    hits: HitMap,
    /// Row chosen, run after closing.
    pending: Option<u32>,
    /// Auto-hide as it was when the menu opened (the check).
    autohide: bool,
    width: i32,
    sep_y: i32,
}

/// Creates the content (initial state).
pub(crate) fn new() -> Box<dyn Content> {
    preview_new(crate::config::get().autohide)
}

/// The content with a fixed Auto-hide state, for the golden.
pub(crate) fn preview_new(autohide: bool) -> Box<dyn Content> {
    Box::new(DockMenu { hits: HitMap::default(), pending: None, autohide, width: 0, sep_y: 0 })
}

impl Content for DockMenu {
    fn kind(&self) -> PopupKind {
        PopupKind::DockMenu
    }

    fn size(&mut self, s: &Surface) -> (i32, i32) {
        let m = s.metrics();
        let (inset, row, gap, pad_text) = (m.px(8), m.px(34), m.px(2), m.px(12));
        let mut tw = s.text_width(t(k::COMMON_DOCK), Font::Bold);
        for (_, text, _) in rows(self.autohide) {
            tw = tw.max(s.text_width(text, Font::Normal));
        }
        // The check sits at the right edge, in its own 16 px slot.
        let w = (tw + 2 * pad_text + 2 * inset + m.px(16 + 8)).clamp(m.px(190), m.px(300));
        self.width = w;
        self.hits.clear();
        let mut y = inset + row;
        self.sep_y = y + gap;
        y += gap * 2 + m.px(1);
        for (id, _, _) in rows(self.autohide) {
            self.hits.add(id, rect_wh(inset, y, w - 2 * inset, row));
            y += row;
        }
        (w, y + inset)
    }

    fn paint(&self, s: &mut Surface, hot: Option<u32>, pressed: Option<u32>) {
        s.panel();
        let m = s.metrics();
        let (inset, row, pad_text, icon) = (m.px(8), m.px(34), m.px(12), m.px(16));
        let (x0, x1) = (inset + pad_text, self.width - inset - pad_text);
        let top = rect_wh(inset, inset, self.width - 2 * inset, row);
        s.text(t(k::COMMON_DOCK), x0, center_y(&top, s.line_height(Font::Bold)), Font::Bold, WHITE, TEXT2_ALPHA);
        hline(s, x0, x1, self.sep_y);
        for (id, text, check) in rows(self.autohide) {
            let Some(rc) = self.hits.rect(id) else { continue };
            let h = row_plate(s, rc, id, hot, pressed);
            let ty = center_y(&rc, s.line_height(Font::Normal));
            s.text(text, x0, ty, Font::Normal, WHITE, lift(TEXT_ALPHA, h));
            if check {
                let cx = rc.right - pad_text - icon;
                s.glyph(cx, rc.top + ((rc.bottom - rc.top) - icon) / 2, icon, TEXT_ALPHA, &crate::glyphs::check_shape);
            }
        }
    }

    fn hit(&self, x: i32, y: i32) -> Option<u32> {
        self.hits.at(x, y)
    }

    fn focus_targets(&self) -> Vec<(u32, RECT)> {
        self.hits.entries().to_vec()
    }

    fn click(&mut self, id: u32) -> Reply {
        self.pending = Some(id);
        Reply::Close
    }

    fn after_close(&mut self) {
        match self.pending.take() {
            Some(ID_AUTOHIDE) => {
                if let Err(e) = crate::config::modify(|c| c.autohide = !c.autohide) {
                    log!("dock menu: auto-hide not saved: {e}");
                }
                // SAFETY: PostMessageW to the dock window (`main` runs `ui::on_config_changed`); if it no longer
                // exists it just fails.
                if let Err(e) = unsafe { PostMessageW(Some(crate::hwnd()), WM_DOCK_CONFIG_APPLY, WPARAM(0), LPARAM(0)) }
                {
                    log!("dock menu: PostMessageW(WM_DOCK_CONFIG_APPLY) failed: {e}");
                }
            }
            Some(ID_TASKMGR) => crate::popup::launch("taskmgr.exe"),
            Some(ID_SETTINGS) => crate::settings_window::open(),
            _ => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dock_menu_rows() {
        let on = rows(true);
        assert_eq!(on.map(|r| r.0), [1, 2, 3]);
        assert_eq!(on.map(|r| r.1), ["Auto-hide", "Task Manager", "Settings"]);
        assert_eq!(on.map(|r| r.2), [true, false, false], "the check only on Auto-hide, and only while it is on");
        assert_eq!(rows(false).map(|r| r.2), [false, false, false]);
    }

    #[test]
    fn dock_menu_click_closes() {
        let mut p = DockMenu { hits: HitMap::default(), pending: None, autohide: false, width: 0, sep_y: 0 };
        assert_eq!(p.click(2), Reply::Close);
        assert_eq!(p.pending, Some(2));
    }

    #[test]
    fn dock_menu_keyboard_targets() {
        let mut p = preview_new(true);
        assert_eq!(
            crate::popup::target_ids(&mut *p),
            [ID_AUTOHIDE, ID_TASKMGR, ID_SETTINGS],
            "the header is not a target"
        );
    }
}
