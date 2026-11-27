//! The metrics menu (dock-suite 2.7): the menu of the bar's CPU and memory items. Header (the module's name), a
//! separator, then Tasks (with the Beta tag, only when usable) and Windows Task Manager, drawn like the dock menu
//! (`popup_dock`: rows of 34 px, inset 8, text column pad 12, width 190..300 at 96 DPI).
//!
//! Declared as a child module of `popup` (`#[path]`).

use windows::Win32::Foundation::RECT;

use crate::i18n::{k, t};
use crate::popup::{center_y, hline, row_plate, Content, HitMap, PopupKind, Reply};
use crate::surface::{rect_wh, Font, Surface, TEXT2_ALPHA, TEXT_ALPHA, WHITE};
use crate::ui::suite_links::{self, App};
use oled_trust::frame::tasks::{Page, Show, SortBy};

/// Row ids.
const ID_TASKS: u32 = 1;
const ID_TASKMGR: u32 = 2;

/// The pages of Tasks this menu opens (the beta has the process list only; the later steps add more).
const TASKS_PAGES_IN_MENU: &[Page] = &[Page::Processes];

/// One row: id, label, Beta tag shown.
type Row = (u32, &'static str, bool);

/// The rows in order. Pure (the tag reads the cached stage).
fn rows(tasks: bool) -> Vec<Row> {
    let mut v = Vec::new();
    if tasks && TASKS_PAGES_IN_MENU.contains(&Page::Processes) {
        v.push((ID_TASKS, suite_links::name(App::Tasks), suite_links::beta(App::Tasks)));
    }
    v.push((ID_TASKMGR, t(k::LINKS_WINDOWS_TASK_MANAGER), false));
    v
}

pub(crate) struct MetricsMenu {
    hits: HitMap,
    /// Row chosen, run after closing.
    pending: Option<u32>,
    /// Tasks has a row, as it was when the menu opened.
    tasks: bool,
    width: i32,
    sep_y: i32,
}

/// Creates the content.
pub(crate) fn new() -> Box<dyn Content> {
    suite_links::refresh();
    preview_new(suite_links::usable(App::Tasks))
}

/// The content for the golden; `tasks` says whether Tasks is usable.
pub(crate) fn preview_new(tasks: bool) -> Box<dyn Content> {
    Box::new(MetricsMenu { hits: HitMap::default(), pending: None, tasks, width: 0, sep_y: 0 })
}

fn header() -> &'static str {
    crate::settings_pages::module_label("metrics")
}

impl Content for MetricsMenu {
    fn kind(&self) -> PopupKind {
        PopupKind::MetricsMenu
    }

    fn size(&mut self, s: &Surface) -> (i32, i32) {
        let m = s.metrics();
        let (inset, row, gap, pad_text) = (m.px(8), m.px(34), m.px(2), m.px(12));
        let mut tw = s.text_width(header(), Font::Bold);
        let beta = t(k::COMMON_BETA);
        for (_, text, tag) in rows(self.tasks) {
            let tag_w = if tag { suite_links::tag_width(s, beta) + m.px(12) } else { 0 };
            tw = tw.max(s.text_width(text, Font::Normal) + tag_w);
        }
        let w = (tw + 2 * pad_text + 2 * inset).clamp(m.px(190), m.px(300));
        self.width = w;
        self.hits.clear();
        let mut y = inset + row;
        self.sep_y = y + gap;
        y += gap * 2 + m.px(1);
        for (id, ..) in rows(self.tasks) {
            self.hits.add(id, rect_wh(inset, y, w - 2 * inset, row));
            y += row;
        }
        (w, y + inset)
    }

    fn paint(&self, s: &mut Surface, hot: Option<u32>, pressed: Option<u32>) {
        s.panel();
        let m = s.metrics();
        let (inset, row, pad_text) = (m.px(8), m.px(34), m.px(12));
        let (x0, x1) = (inset + pad_text, self.width - inset - pad_text);
        let top = rect_wh(inset, inset, self.width - 2 * inset, row);
        s.text(header(), x0, center_y(&top, s.line_height(Font::Bold)), Font::Bold, WHITE, TEXT2_ALPHA);
        hline(s, x0, x1, self.sep_y);
        for (id, text, tag) in rows(self.tasks) {
            let Some(rc) = self.hits.rect(id) else { continue };
            let h = row_plate(s, rc, id, hot, pressed);
            let ty = center_y(&rc, s.line_height(Font::Normal));
            s.text(text, x0, ty, Font::Normal, WHITE, crate::popup::lift(TEXT_ALPHA, h));
            if tag {
                suite_links::paint_tag(s, rc, t(k::COMMON_BETA));
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
            Some(ID_TASKS) => {
                suite_links::open_tasks(Show { sort: SortBy::Cpu, ..Show::default() });
            }
            Some(ID_TASKMGR) => suite_links::open_windows_task_manager(),
            _ => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn metrics_menu_rows() {
        assert_eq!(rows(true).iter().map(|r| r.0).collect::<Vec<_>>(), [ID_TASKS, ID_TASKMGR]);
        assert_eq!(rows(false).iter().map(|r| r.0).collect::<Vec<_>>(), [ID_TASKMGR]);
        assert!(!rows(true)[1].2, "no tag on Windows' tool");
        let mut p = preview_new(true);
        assert_eq!(crate::popup::target_ids(&mut *p), [ID_TASKS, ID_TASKMGR], "the header is not a target");
        let mut q = preview_new(false);
        assert_eq!(crate::popup::target_ids(&mut *q), [ID_TASKMGR]);
    }

    #[test]
    fn click_closes_and_keeps_the_row() {
        let mut p = MetricsMenu { hits: HitMap::default(), pending: None, tasks: true, width: 0, sep_y: 0 };
        assert_eq!(p.kind(), PopupKind::MetricsMenu);
        assert_eq!(p.click(ID_TASKS), Reply::Close);
        assert_eq!(p.pending, Some(ID_TASKS));
    }

    #[test]
    fn tasks_opens_sorted_by_cpu() {
        let src = include_str!("popup_metrics.rs").replace("\r\n", "\n");
        assert!(src.contains("Show { sort: SortBy::Cpu, ..Show::default() }"));
        assert_eq!(
            suite_links::tasks_params(&Show { sort: SortBy::Cpu, ..Show::default() }).as_deref(),
            Some("--sort cpu")
        );
    }

    #[test]
    fn menus_refresh_before_rows() {
        let src = include_str!("popup_metrics.rs").replace("\r\n", "\n");
        let body = &src[src.find("pub(crate) fn new()").unwrap()..];
        let body = &body[..body.find("\n}\n").unwrap()];
        assert!(body.find("suite_links::refresh()").unwrap() < body.find("usable(").unwrap());
    }
}
