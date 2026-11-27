//! The path page (K3.3): the breadcrumb at rest, folded to fit a narrow line, offline, with an error and its reason, and in
//! edit mode.

use super::{s, Page};
use oled_ui::command::CommandTable;
use oled_ui::cx::{EventCx, LayoutCx, PaintCx};
use oled_ui::event::{Event, Reply};
use oled_ui::geom::{RectP, SizeL, SizeP};
use oled_ui::node::{Text, WidgetId};
use oled_ui::screen::{Screen, Widget};
use oled_ui::strings::UiLang;
use oled_ui::widgets::{PathBar, PathSegment};

const SIZE: SizeL = SizeL { w: 720, h: 336 };
const ROW_H: i32 = 64;

struct Bars {
    bars: Vec<(PathBar, i32)>,
    commands: CommandTable,
}

impl Screen for Bars {
    fn layout(&mut self, cx: &mut LayoutCx, avail: SizeP) -> SizeP {
        for (i, (b, w)) in self.bars.iter_mut().enumerate() {
            let r = RectP::wh(cx.px(16), cx.px(8 + ROW_H * i as i32), cx.px(*w), cx.px(44));
            b.layout(cx, r);
        }
        let _ = avail;
        avail
    }

    fn paint(&self, cx: &mut PaintCx) {
        for (b, _) in &self.bars {
            b.paint(cx);
        }
    }

    fn event(&mut self, cx: &mut EventCx, ev: &Event) -> Reply {
        for (b, _) in &mut self.bars {
            b.event(cx, ev);
        }
        Reply::None
    }

    fn commands(&self) -> &CommandTable {
        &self.commands
    }
}

fn segs(lang: UiLang, n: usize) -> Vec<PathSegment> {
    let names: [(&'static str, &'static str); 7] = [
        ("Local Disk (C:)", "Disco locale (C:)"),
        ("Users", "Utenti"),
        ("Projects", "Progetti"),
        ("Design", "Progettazione"),
        ("Reports", "Rapporti"),
        ("2026", "2026"),
        ("Final", "Definitivo"),
    ];
    names
        .iter()
        .take(n)
        .enumerate()
        .map(|(i, (en, it))| PathSegment { key: i as u64 + 1, label: s(lang, en, it), tip: None })
        .collect()
}

/// The page: five bars.
pub fn page(lang: UiLang) -> Option<Page> {
    let edit_text = "C:\\Users\\Projects\\Design";
    let bar = |id: u32, n: usize| PathBar::new(WidgetId(id)).with_path(segs(lang, n), edit_text.to_string());
    let mut offline = bar(3, 4);
    offline.set_offline(true);
    let reason: Text = s(lang, "This folder is not available.", "Questa cartella non è disponibile.");
    let mut error = bar(4, 4);
    error.preview_error(Some(reason));
    let mut edit = bar(5, 4);
    edit.preview_edit(edit_text, None);
    let bars = vec![(bar(1, 5), 688), (bar(2, 7), 300), (offline, 688), (error, 688), (edit, 688)];
    Some(Page { name: "path", size: SIZE, states: 1, screen: Box::new(Bars { bars, commands: CommandTable::new() }) })
}
