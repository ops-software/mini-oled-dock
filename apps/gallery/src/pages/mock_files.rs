//! The mock_files page (K3.3): the files window before the files app exists, made only of kit widgets: a TabStrip in the
//! caption row, a Toolbar with Back, Forward and Up, the PathBar and a filter field, a SplitView with a SidebarList and a
//! VirtualTable (the FILES look) over 100,000 synthetic entries, and a StatusLine. Its keyboard script is K3's acceptance.

use std::borrow::Cow;

use super::table::{columns, select, Demo};
use super::{s, Page};
use oled_ui::command::{Chord, Command as Cmd, CommandId, CommandTable};
use oled_ui::cx::{EventCx, LayoutCx, PaintCx};
use oled_ui::event::{Event, Reply};
use oled_ui::geom::{RectP, SizeL, SizeP};
use oled_ui::gfx::image::IconBitmap;
use oled_ui::model::{
    Badge, CellKind, CellOut, ColId, FgRole, IconRef, ListModel, RowKey, SelectionMode, TableModel, TableOut,
    TableStyle,
};
use oled_ui::node::{NodeId, Text, WidgetId};
use oled_ui::screen::{Screen, Widget};
use oled_ui::strings::UiLang;
use oled_ui::text::Trim;
use oled_ui::widgets::{
    FieldKind, PathBar, PathOut, PathSegment, SidebarItem, SidebarList, SidebarStyle, SplitView, StatusLine, TabSpec,
    TabStrip, TextField, Toolbar, ToolbarButton, VirtualTable,
};
use oled_ui::Glyph;

const SIZE: SizeL = SizeL { w: 960, h: 600 };
/// The entries of the listing.
pub const ENTRIES: usize = 100_000;

/// Widget ids of the page.
const STRIP: WidgetId = WidgetId(1);
const TOOLBAR: WidgetId = WidgetId(2);
const FILTER: WidgetId = WidgetId(3);
const PATH: WidgetId = WidgetId(4);
const SPLIT: WidgetId = WidgetId(5);
const SIDE: WidgetId = WidgetId(6);
const TABLE: WidgetId = WidgetId(7);
const STATUS: WidgetId = WidgetId(8);

/// The page's commands: the Tabs row's chords, the path chords and the regions.
const NEXT_TAB: CommandId = CommandId(1);
const PREV_TAB: CommandId = CommandId(2);
const EDIT_PATH: CommandId = CommandId(3);
const EDIT_PATH_F4: CommandId = CommandId(4);
const NEXT_REGION: CommandId = CommandId(5);
const PREV_REGION: CommandId = CommandId(6);
const BACK: CommandId = CommandId(7);
const FORWARD: CommandId = CommandId(8);
const UP: CommandId = CommandId(9);
/// Ctrl+1..Ctrl+9: CommandId(DIGIT + 1)..CommandId(DIGIT + 9).
const DIGIT: u32 = 20;

/// The sidebar's current place: its key, and its node (slot 1 + its index: the "Pinned" header is index 0).
const SIDE_CURRENT: u64 = 2;
const SIDE_CURRENT_SLOT: u32 = 3;

/// A listing without storage: names with extensions, some long enough to trim in the middle, cloud and link badges and
/// cut rows.
struct Listing {
    sizes: Demo,
}

fn name_of(i: usize) -> String {
    const EXT: [&str; 5] = ["txt", "rs", "png", "docx", "zip"];
    let ext = EXT[i % EXT.len()];
    if i % 9 == 4 {
        format!("A long descriptive file name that cannot fit its column {i}.{ext}")
    } else {
        format!("item {i}.{ext}")
    }
}

impl ListModel for Listing {
    fn len(&self) -> usize {
        ENTRIES
    }

    fn key(&self, row: usize) -> RowKey {
        RowKey(row as u64)
    }

    fn row_of(&self, key: RowKey) -> Option<usize> {
        ((key.0 as usize) < ENTRIES).then_some(key.0 as usize)
    }

    fn label(&self, row: usize) -> Cow<'_, str> {
        Cow::Owned(name_of(row))
    }

    fn generation(&self) -> u64 {
        0
    }
}

impl TableModel for Listing {
    fn cell(&self, row: usize, col: ColId, out: &mut CellOut) {
        if col.0 != 0 {
            return self.sizes.cell(row, col, out);
        }
        let name = name_of(row);
        out.set_text(CellKind::Text, &name);
        out.icon = Some(IconRef(0));
        out.trim = Trim::Middle { tail: 10 };
        if let Some(dot) = name.rfind('.') {
            out.push_span(dot as u32, name.len() as u32, FgRole::Secondary);
        }
        out.badge = match row % 23 {
            3 => Some(Badge::CloudOnline),
            7 => Some(Badge::CloudLocal),
            11 => Some(Badge::CloudPinned),
            13 => Some(Badge::Link),
            _ => None,
        };
        out.cut = row % 41 == 5;
    }

    fn icon(&self, icon: IconRef, px: i32) -> Option<&IconBitmap> {
        self.sizes.icon(icon, px)
    }
}

fn tab(key: u64, label: Text) -> TabSpec {
    TabSpec { key, label, tip: None, icon: None, busy: false, error: false }
}

fn place(key: u64, label: Text, glyph: Glyph, trailing: Option<(Text, FgRole)>) -> SidebarItem {
    SidebarItem {
        key,
        label,
        glyph: Some(glyph),
        icon: None,
        depth: 0,
        expandable: false,
        expanded: false,
        section: false,
        trailing,
        disabled: false,
        busy: false,
    }
}

fn section(key: u64, label: Text) -> SidebarItem {
    SidebarItem { section: true, glyph: None, ..place(key, label, Glyph::Folder, None) }
}

/// The page's screen.
pub struct MockFiles {
    strip: TabStrip,
    toolbar: Toolbar,
    path: PathBar,
    split: SplitView,
    side: SidebarList,
    table: VirtualTable<Listing>,
    status: StatusLine,
    rects: Rects,
    commands: CommandTable,
    /// What the screen did with the widgets' outputs, for the keyboard script.
    pub log: Vec<String>,
}

/// The rects of the last layout.
#[derive(Default)]
struct Rects {
    body: RectP,
}

impl MockFiles {
    pub fn new(lang: UiLang) -> MockFiles {
        let strip = TabStrip::new(STRIP).with_tabs(
            vec![
                tab(1, s(lang, "Downloads", "Scaricati")),
                tab(2, s(lang, "Documents", "Documenti")),
                tab(3, s(lang, "Pictures", "Immagini")),
                tab(4, s(lang, "Projects", "Progetti")),
            ],
            1,
        );
        let button = |cmd: CommandId, glyph, en: &'static str, it: &'static str| ToolbarButton {
            command: cmd,
            glyph,
            tip: s(lang, en, it),
            toggled: None,
        };
        let mut filter = TextField::new(FILTER, FieldKind::Search);
        filter.set_placeholder(s(lang, "Search Downloads", "Cerca in Scaricati"));
        filter.set_label(s(lang, "Filter", "Filtro"));
        filter.set_width(200);
        let toolbar = Toolbar::new(
            TOOLBAR,
            None,
            vec![
                button(BACK, Glyph::ChevronLeft, "Back", "Indietro"),
                button(FORWARD, Glyph::ChevronRight, "Forward", "Avanti"),
                button(UP, Glyph::ChevronUp, "Up", "Su"),
            ],
            Some(filter),
        );
        let seg = |key, en: &'static str, it: &'static str| PathSegment { key, label: s(lang, en, it), tip: None };
        let path = PathBar::new(PATH).with_path(
            vec![
                seg(1, "Local Disk (C:)", "Disco locale (C:)"),
                seg(2, "Users", "Utenti"),
                seg(3, "Downloads", "Scaricati"),
            ],
            "C:\\Users\\Downloads".to_string(),
        );
        let split = SplitView::new(SPLIT, 160, 400, 200);
        let free = |t: Text, role| Some((t, role));
        let side = SidebarList::new(SIDE, SidebarStyle { row_h: 28, inset: 8 }).with_items(vec![
            section(100, s(lang, "Pinned", "Fissati")),
            place(1, s(lang, "Desktop", "Desktop"), Glyph::Monitor, None),
            place(SIDE_CURRENT, s(lang, "Downloads", "Scaricati"), Glyph::Download, None),
            place(3, s(lang, "Documents", "Documenti"), Glyph::Document, None),
            place(4, s(lang, "Pictures", "Immagini"), Glyph::Image, None),
            section(101, s(lang, "This PC", "Questo PC")),
            place(
                5,
                s(lang, "Local Disk (C:)", "Disco locale (C:)"),
                Glyph::Disk,
                free(s(lang, "212 GB free", "212 GB liberi"), FgRole::Tertiary),
            ),
            place(
                6,
                s(lang, "Data (D:)", "Dati (D:)"),
                Glyph::Disk,
                free(s(lang, "9 GB free", "9 GB liberi"), FgRole::Attention),
            ),
        ]);
        let mut side = side;
        side.set_current(Some(SIDE_CURRENT));
        let mut cols = columns(lang);
        cols.truncate(2);
        let model = Listing { sizes: Demo::synthetic(ENTRIES) };
        let mut table = VirtualTable::new(TABLE, model, cols, SelectionMode::Multi, TableStyle::FILES)
            .with_selection(select(&[1, 2, 3]))
            .with_focused(RowKey(2));
        table.set_label(s(lang, "Files", "File"));
        let mut status = StatusLine::new(STATUS);
        status.set_items(vec![
            (s(lang, "100,000 items", "100.000 elementi"), FgRole::Tertiary),
            (s(lang, "3 selected", "3 selezionati"), FgRole::Secondary),
        ]);
        let mut commands = CommandTable::new();
        let mut bind = |id: CommandId, label: &'static str, chord: &str, alt: Option<&str>| {
            let Some(main) = Chord::parse(chord) else { return };
            let mut c = Cmd::new(id, Text::from(label)).chord(main);
            if let Some(a) = alt.and_then(Chord::parse) {
                c = c.alt_chord(a);
            }
            let _ = commands.add(c);
        };
        bind(NEXT_TAB, "Next tab", "Ctrl+Tab", Some("Ctrl+PgDn"));
        bind(PREV_TAB, "Previous tab", "Ctrl+Shift+Tab", Some("Ctrl+PgUp"));
        bind(EDIT_PATH, "Edit path", "Ctrl+L", Some("Alt+D"));
        bind(EDIT_PATH_F4, "Edit path (F4)", "F4", None);
        bind(NEXT_REGION, "Next region", "F6", None);
        bind(PREV_REGION, "Previous region", "Shift+F6", None);
        for d in 1..=9u32 {
            bind(CommandId(DIGIT + d), "Tab by number", &format!("Ctrl+{d}"), None);
        }
        MockFiles {
            strip,
            toolbar,
            path,
            split,
            side,
            table,
            status,
            rects: Rects::default(),
            commands,
            log: Vec::new(),
        }
    }

    /// F6 and Shift+F6: the focus goes from the sidebar to the table and back.
    fn switch_region(&mut self, cx: &mut EventCx) {
        let in_side = cx.focused().is_some_and(|n| n.owner() == SIDE);
        let to = if in_side { NodeId::new(TABLE, 3) } else { NodeId::new(SIDE, SIDE_CURRENT_SLOT) };
        cx.set_focus(to);
    }
}

impl Screen for MockFiles {
    fn layout(&mut self, cx: &mut LayoutCx, avail: SizeP) -> SizeP {
        let w = avail.w;
        let (strip_h, bar_h, status_h) = (cx.px(40), cx.px(44), cx.px(28));
        self.strip.layout(cx, RectP::wh(0, 0, w, strip_h));
        let bar = RectP::wh(0, strip_h, w, bar_h);
        self.toolbar.layout(cx, bar);
        // The path line fills what the three buttons and the filter field leave.
        let (l, r) = (cx.px(16 + 3 * 28 + 2 * 4 + 12), w - cx.px(16 + 200 + 8));
        self.path.layout(cx, RectP { l, r, ..bar });
        let body = RectP { l: 0, t: bar.b, r: w, b: avail.h - status_h };
        self.rects.body = body;
        self.split.layout(cx, body);
        let (first, second) = self.split.panes(body);
        self.side.layout(cx, first);
        self.table.layout(cx, second);
        self.status.layout(cx, RectP { l: 0, t: body.b, r: w, b: avail.h });
        avail
    }

    fn paint(&self, cx: &mut PaintCx) {
        self.strip.paint(cx);
        self.toolbar.paint(cx);
        self.path.paint(cx);
        self.side.paint(cx);
        self.table.paint(cx);
        self.split.paint(cx);
        self.status.paint(cx);
    }

    fn event(&mut self, cx: &mut EventCx, ev: &Event) -> Reply {
        if let Event::Command(id) = ev {
            match *id {
                NEXT_TAB => {
                    let k = self.strip.select_next(cx, false);
                    self.log.push(format!("tab {k:?}"));
                }
                PREV_TAB => {
                    let k = self.strip.select_next(cx, true);
                    self.log.push(format!("tab {k:?}"));
                }
                CommandId(n) if (DIGIT + 1..=DIGIT + 8).contains(&n) => {
                    let k = self.strip.select_index(cx, (n - DIGIT - 1) as usize);
                    self.log.push(format!("tab {k:?}"));
                }
                CommandId(n) if n == DIGIT + 9 => {
                    let k = self.strip.select_index(cx, usize::MAX);
                    self.log.push(format!("tab {k:?}"));
                }
                EDIT_PATH | EDIT_PATH_F4 => self.path.begin_edit(cx),
                NEXT_REGION | PREV_REGION => self.switch_region(cx),
                _ => {}
            }
            self.toolbar.event(cx, ev);
            return Reply::None;
        }
        if let Some(o) = self.strip.event(cx, ev) {
            self.log.push(format!("{o:?}"));
        }
        self.toolbar.event(cx, ev);
        match self.path.event(cx, ev) {
            Some(PathOut::CompletionRequested(text)) => {
                let items = (1..=3).map(|i| Text::from(format!("{text}{i}"))).collect();
                self.path.set_completions(cx, items);
            }
            Some(o) => self.log.push(format!("{o:?}")),
            None => {}
        }
        if let Some(o) = self.split.event(cx, ev) {
            self.log.push(format!("{o:?}"));
        }
        if let Some(o) = self.side.event(cx, ev) {
            self.log.push(format!("{o:?}"));
        }
        if let Some(o) = self.table.event(cx, ev) {
            if !matches!(o, TableOut::FocusChanged(_)) {
                self.log.push(format!("{o:?}"));
            }
        }
        self.status.event(cx, ev);
        Reply::None
    }

    fn commands(&self) -> &CommandTable {
        &self.commands
    }

    fn caption_hit(&self, p: oled_ui::geom::PointP) -> oled_ui::screen::CaptionHit {
        self.strip.caption_hit(p)
    }
}

pub fn page(lang: UiLang) -> Option<Page> {
    Some(Page { name: "mock_files", size: SIZE, states: 1, screen: Box::new(MockFiles::new(lang)) })
}

#[cfg(test)]
mod tests {
    use super::*;
    use oled_ui::harness::Harness;
    use oled_ui::theme::Tokens;

    fn harness() -> Harness<MockFiles> {
        Harness::new(MockFiles::new(UiLang::EN), SIZE, 96, Tokens::oled())
    }

    fn owner(h: &Harness<MockFiles>) -> Option<WidgetId> {
        h.focused().map(|n| n.owner())
    }

    #[test]
    fn mock_files_keyboard_script() {
        let mut h = harness();
        // The Tabs row: the chords reach select_next and select_index through the page's own binding.
        assert_eq!(h.screen().strip.selected(), 1);
        h.key("Ctrl+Tab");
        assert_eq!(h.screen().strip.selected(), 2);
        h.key("Ctrl+PgDn");
        assert_eq!(h.screen().strip.selected(), 3);
        h.key("Ctrl+Shift+Tab");
        h.key("Ctrl+PgUp");
        assert_eq!(h.screen().strip.selected(), 1);
        h.key("Ctrl+9");
        assert_eq!(h.screen().strip.selected(), 4, "the last tab");
        h.key("Ctrl+7");
        assert_eq!(h.screen().strip.selected(), 4, "no seventh tab: no effect");
        h.key("Ctrl+2");
        assert_eq!(h.screen().strip.selected(), 2);

        // F6 moves between the sidebar and the table.
        h.key("F6");
        assert_eq!(owner(&h), Some(SIDE));
        h.key("F6");
        assert_eq!(owner(&h), Some(TABLE));
        h.key("Shift+F6");
        assert_eq!(owner(&h), Some(SIDE));
        // Down in the sidebar: the focus moves among the places (not onto the section header).
        h.key("Down");
        assert_eq!(h.focused(), Some(NodeId::new(SIDE, 4)));

        // Tab reaches the splitter; Alt+Shift+Right widens the sidebar by 16 px.
        let mut tabs = 0;
        while owner(&h) != Some(SPLIT) && tabs < 40 {
            h.key("Tab");
            tabs += 1;
        }
        assert_eq!(owner(&h), Some(SPLIT), "the splitter is a Tab stop");
        let width = |h: &mut Harness<MockFiles>| h.screen().split.first();
        assert_eq!(width(&mut h), 200);
        h.key("Alt+Shift+Right");
        assert_eq!(width(&mut h), 216);
        assert!(h.screen().log.iter().any(|l| l.contains("Moved(216)")), "{:?}", h.screen().log);

        // Ctrl+L turns the path into a field with the path selected; typing lists completions; Esc closes the list, then
        // restores the breadcrumb.
        h.key("Ctrl+L");
        assert!(h.screen().path.is_editing());
        assert_eq!(owner(&h), Some(WidgetId(PATH.0 | 0x4000_0000)));
        h.type_text("C:\\Us");
        assert_eq!(h.menus().len(), 1, "completions");
        h.key("Esc");
        assert!(h.menus().is_empty());
        h.key("Esc");
        assert!(!h.screen().path.is_editing());
        assert_eq!(owner(&h), Some(PATH), "the focus is back on the breadcrumb");
        assert!(h.screen().log.iter().any(|l| l.contains("EditCancelled")));
        h.assert_named();
    }

    #[test]
    fn mock_files_has_100k_rows_and_rests() {
        let mut h = harness();
        assert!(h.a11y_dump().contains("of 100000"), "{}", h.a11y_dump());
        h.render();
        h.advance(1000);
        assert!(h.is_idle(), "the page keeps no clock at rest");
    }
}
