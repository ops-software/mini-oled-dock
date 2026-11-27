//! The gallery on screen (K1a.11, KA2): the index and the selected page beside it, or one page with --page.

use std::borrow::Cow;

use oled_ui::command::CommandTable;
use oled_ui::cx::{EventCx, LayoutCx, PaintCx};
use oled_ui::event::{Event, Reply};
use oled_ui::geom::{RectP, SizeL, SizeP};
use oled_ui::host::{AppId, AppMsg, Caption, EventLoop, PlacementKey, WindowKind, WindowSpec};
use oled_ui::node::WidgetId;
use oled_ui::screen::{Screen, Widget};
use oled_ui::strings::{Lang, UiLang};
use oled_ui::theme::ThemeSpec;

use crate::cli::{CaptionArg, WindowOpts};
use crate::pages::index::{Index, IndexOut};
use crate::pages::{self, FocusRow, Page};
use crate::placement::FileStore;

/// The gallery window's logical size.
pub const GALLERY_SIZE: SizeL = SizeL { w: 1100, h: 720 };
/// The index column's logical width, at the window's right edge.
const INDEX_W: i32 = 240;
/// The index's widget ids start here, far above the pages' own.
const INDEX_ID: WidgetId = WidgetId(0x4000);

/// The pages the index lists: every ready page but the index itself, as registry indices.
pub fn listed(lang: UiLang) -> Vec<usize> {
    pages::REGISTRY
        .iter()
        .enumerate()
        .filter(|(_, (n, f))| *n != "index" && f(lang).is_some())
        .map(|(i, _)| i)
        .collect()
}

/// A page and the index beside it. The page lays out from the window's top-left corner, in the area left of the index.
pub struct GalleryScreen {
    lang: UiLang,
    listed: Vec<usize>,
    index: Index,
    page: Option<Page>,
    empty: CommandTable,
}

impl GalleryScreen {
    /// The gallery with its first listed page open.
    pub fn new(lang: UiLang) -> GalleryScreen {
        let listed = listed(lang);
        let names = listed.iter().map(|i| pages::REGISTRY[*i].0).collect();
        let page = listed.first().and_then(|i| (pages::REGISTRY[*i].1)(lang));
        GalleryScreen { lang, listed, index: Index::new(INDEX_ID, names), page, empty: CommandTable::new() }
    }

    /// The name of the page shown.
    pub fn page_name(&self) -> Option<&'static str> {
        self.page.as_ref().map(|p| p.name)
    }
}

impl Screen for GalleryScreen {
    fn layout(&mut self, cx: &mut LayoutCx, avail: SizeP) -> SizeP {
        let iw = cx.px(INDEX_W).min(avail.w);
        if let Some(p) = self.page.as_mut() {
            p.screen.layout(cx, SizeP { w: avail.w - iw, h: avail.h });
        }
        self.index.layout(cx, RectP::wh(avail.w - iw, 0, iw, avail.h));
        avail
    }

    fn paint(&self, cx: &mut PaintCx) {
        if let Some(p) = self.page.as_ref() {
            p.screen.paint(cx);
        }
        self.index.paint(cx);
    }

    fn event(&mut self, cx: &mut EventCx, ev: &Event) -> Reply {
        if matches!(ev, Event::CloseRequested) {
            return Reply::Close;
        }
        if let Some(IndexOut::Open(i)) = self.index.event(cx, ev) {
            if let Some(&reg) = self.listed.get(i) {
                self.page = (pages::REGISTRY[reg].1)(self.lang);
                cx.relayout();
                cx.redraw();
            }
            return Reply::None;
        }
        match self.page.as_mut() {
            Some(p) => p.screen.event(cx, ev),
            None => Reply::None,
        }
    }

    fn data(&mut self, cx: &mut EventCx, msg: AppMsg) -> Reply {
        match self.page.as_mut() {
            Some(p) => p.screen.data(cx, msg),
            None => Reply::None,
        }
    }

    fn commands(&self) -> &CommandTable {
        self.page.as_ref().map_or(&self.empty, |p| p.screen.commands())
    }
}

/// One page alone at its size; the window closes on request.
pub struct PageScreen(pub Page);

impl Screen for PageScreen {
    fn layout(&mut self, cx: &mut LayoutCx, avail: SizeP) -> SizeP {
        self.0.screen.layout(cx, avail)
    }

    fn paint(&self, cx: &mut PaintCx) {
        self.0.screen.paint(cx);
    }

    fn event(&mut self, cx: &mut EventCx, ev: &Event) -> Reply {
        if matches!(ev, Event::CloseRequested) {
            return Reply::Close;
        }
        self.0.screen.event(cx, ev)
    }

    fn data(&mut self, cx: &mut EventCx, msg: AppMsg) -> Reply {
        self.0.screen.data(cx, msg)
    }

    fn commands(&self) -> &CommandTable {
        self.0.screen.commands()
    }
}

/// The spec of a gallery window: the system caption unless --caption custom asks (effective once S-CAPTION passed).
pub fn spec(title: &'static str, size: SizeL, caption: Caption, placement: Option<PlacementKey>) -> WindowSpec {
    WindowSpec {
        kind: WindowKind::Opaque { min: SizeL { w: 320, h: 240 }, caption },
        class: "MiniOledGallery",
        title: Cow::Borrowed(title),
        size,
        owner: None,
        esc_closes: false,
        placement,
        icon_resource: None,
        opened_by_chord: false,
    }
}

/// Opens the gallery window (or the page of --page) and runs its loop.
pub fn run(opts: &WindowOpts) -> i32 {
    let lang = Lang::Auto;
    let ui = UiLang::from_lang(lang);
    let mut lp = match EventLoop::new(AppId("gallery"), lang, ThemeSpec::oled()) {
        Ok(lp) => lp,
        Err(e) => {
            eprintln!("gallery: {e:?}");
            return 1;
        }
    };
    lp.set_placement_store(Box::new(FileStore::beside_exe()));
    let caption = match opts.caption {
        Some(CaptionArg::Custom) => Caption::Custom,
        _ => Caption::System,
    };
    let opened = match opts.page.as_deref() {
        None => lp.open(spec("Gallery", GALLERY_SIZE, caption, Some(PlacementKey("gallery"))), GalleryScreen::new(ui)),
        Some(name) => {
            let Some(page) = pages::by_name(name, ui) else {
                eprintln!("gallery: {name:?} is not a ready page");
                return 2;
            };
            let size = page.size;
            lp.open(spec("Gallery", size, caption, None), PageScreen(page))
        }
    };
    let w = match opened {
        Ok(w) => w,
        Err(e) => {
            eprintln!("gallery: {e:?}");
            return 1;
        }
    };
    if let Some(row) = opts.focus_row {
        lp.proxy(w).send(Box::new(FocusRow(row)));
    }
    lp.run()
}

#[cfg(test)]
mod tests {
    use super::*;
    use oled_ui::geom::PointL;
    use oled_ui::harness::Harness;
    use oled_ui::theme::Tokens;

    #[test]
    fn the_index_lists_every_ready_page_but_itself() {
        let l = listed(UiLang::EN);
        assert!(!l.is_empty());
        assert!(l.iter().all(|i| pages::REGISTRY[*i].0 != "index"));
        assert_eq!(GalleryScreen::new(UiLang::EN).page_name(), Some(pages::REGISTRY[l[0]].0), "the first one is open");
    }

    #[test]
    fn picking_an_entry_opens_its_page() {
        let l = listed(UiLang::EN);
        let mut h = Harness::new(GalleryScreen::new(UiLang::EN), GALLERY_SIZE, 96, Tokens::oled());
        h.assert_named();
        let second = l.len().min(2) - 1;
        // The index column starts INDEX_W from the right edge; its entries are 34 px apart from 16 px down.
        h.click(PointL { x: GALLERY_SIZE.w - INDEX_W / 2, y: 16 + 34 * second as i32 + 10 });
        assert_eq!(h.screen().page_name(), Some(pages::REGISTRY[l[second]].0));
        let img = h.render();
        assert_eq!((img.w, img.h), (1100, 720));
    }

    #[test]
    fn close_requests_close_the_window() {
        let mut h = Harness::new(GalleryScreen::new(UiLang::EN), GALLERY_SIZE, 96, Tokens::oled());
        h.key("Alt+F4");
        assert!(h.last_outputs().iter().any(|o| o == "Close" || o == "CloseWindow"), "{:?}", h.last_outputs());
        let p = pages::by_name("buttons", UiLang::EN).unwrap();
        let size = p.size;
        let mut h = Harness::new(PageScreen(p), size, 96, Tokens::oled());
        h.key("Ctrl+W");
        assert!(h.last_outputs().iter().any(|o| o == "Close"), "{:?}", h.last_outputs());
    }
}
