//! The K0.1c freeze (P2-kit 2.6, the widget part of 2.8, 2.9 and 2.10): the widget and data-view types used the way an
//! app uses them. This file must keep compiling unchanged: a breaking change to a frozen item shows here.

use std::borrow::Cow;
use std::ops::Range;
use std::rc::Rc;

use oled_ui::gfx::image::IconBitmap;
use oled_ui::harness::Harness;
use oled_ui::model::*;
use oled_ui::text::Trim;
use oled_ui::verbs::{check_table, Verb};
use oled_ui::widgets::*;
use oled_ui::*;
// Through an alias: tests/workspace.rs reads the plain "Command" + "::new(" as a process spawn.
use oled_ui::Command as Cmd;

const TABLE: WidgetId = WidgetId(1);
const FIELD: WidgetId = WidgetId(2);
const TABS: WidgetId = WidgetId(3);
const PATH: WidgetId = WidgetId(4);
const NEXT_TAB: CommandId = CommandId(1);
const CLOSE: CommandId = CommandId(2);

/// An app's rows: (key, name).
struct Demo(Vec<(u64, String)>);

impl ListModel for Demo {
    fn len(&self) -> usize {
        self.0.len()
    }
    fn key(&self, row: usize) -> RowKey {
        RowKey(self.0[row].0)
    }
    fn row_of(&self, key: RowKey) -> Option<usize> {
        self.0.iter().position(|r| r.0 == key.0)
    }
    fn label(&self, row: usize) -> Cow<'_, str> {
        Cow::Borrowed(&self.0[row].1)
    }
    fn generation(&self) -> u64 {
        self.0.len() as u64
    }
}

impl TableModel for Demo {
    fn cell(&self, row: usize, col: ColId, out: &mut CellOut) {
        out.clear();
        let _ = col;
        out.set_text(CellKind::Text, &self.0[row].1);
    }
}

struct DemoScreen {
    table: VirtualTable<Demo>,
    field: TextField,
    tabs: TabStrip,
    path: PathBar,
    cmds: CommandTable,
    menus: u32,
}

impl DemoScreen {
    fn new() -> DemoScreen {
        let columns = vec![Column {
            id: ColId(0),
            title: Cow::Borrowed("Name"),
            width: 240,
            min: 48,
            flex: true,
            align: Align::Start,
            sort: Some(SortDir::Asc),
            first_sort: SortDir::Asc,
            visible: true,
            hideable: false,
            tip: None,
        }];
        let model = Demo(vec![(1, "one".into()), (2, "two".into())]);
        let mut cmds = CommandTable::new();
        let next = Cmd::new(NEXT_TAB, Cow::Borrowed("Next tab")).chord(Chord::new(Mods::CTRL, Key::TAB));
        assert!(cmds.add(next).is_ok());
        assert!(cmds.add(Verb::Close.command(CLOSE, UiLang::EN)).is_ok());
        DemoScreen {
            table: VirtualTable::new(TABLE, model, columns, SelectionMode::Multi, TableStyle::FILES),
            field: TextField::new(FIELD, FieldKind::Search),
            tabs: TabStrip::new(TABS),
            path: PathBar::new(PATH),
            cmds,
            menus: 0,
        }
    }
}

impl Screen for DemoScreen {
    fn layout(&mut self, cx: &mut LayoutCx, avail: SizeP) -> SizeP {
        let h = cx.px(36);
        self.tabs.layout(cx, RectP::wh(0, 0, avail.w, h));
        self.path.layout(cx, RectP::wh(0, h, avail.w / 2, h));
        self.field.layout(cx, RectP::wh(avail.w / 2, h, avail.w / 2, h));
        self.table.layout(cx, RectP::wh(0, 2 * h, avail.w, avail.h - 2 * h));
        avail
    }

    fn paint(&self, cx: &mut PaintCx) {
        self.tabs.paint(cx);
        self.path.paint(cx);
        self.field.paint(cx);
        self.table.paint(cx);
    }

    fn event(&mut self, cx: &mut EventCx, ev: &Event) -> Reply {
        if let Event::Command(NEXT_TAB) = ev {
            let _ = self.tabs.select_next(cx, false);
            return Reply::Redraw;
        }
        match cx.target().map(|t| t.owner()) {
            Some(TABLE) => {
                if let Some(TableOut::ContextMenu(row, at)) = self.table.event(cx, ev) {
                    let _ = row;
                    self.menus += 1;
                    let _ = cx.open_menu(vec![MenuItem::Command(CLOSE)], Anchor::Point(at));
                }
            }
            Some(FIELD) => {
                if let Some(FieldOut::Committed(text)) = self.field.event(cx, ev) {
                    cx.toast(ToastSpec { text: Cow::Owned(text), action: None, progress: None, hold_ms: None });
                }
            }
            Some(TABS) => {
                let _ = self.tabs.event(cx, ev);
            }
            Some(PATH) => {
                let _ = self.path.event(cx, ev);
            }
            _ => {}
        }
        Reply::None
    }

    fn commands(&self) -> &CommandTable {
        &self.cmds
    }
}

#[test]
fn freeze_api_is_usable_by_an_app() {
    let mut h = Harness::new(DemoScreen::new(), SizeL { w: 640, h: 480 }, 96, Tokens::oled());
    h.key("Ctrl+Tab");
    h.click(PointL { x: 100, y: 200 });
    let _ = h.render();
    assert_eq!(h.screen().menus, 0, "stubs: nothing happens");
    assert!(h.is_idle());
    assert_eq!(Verb::Close.label(UiLang::IT), "Chiudi");
    assert_eq!(Verb::Delete.description(UiLang::EN), "Moves it to the Recycle Bin. You can restore it.");
    assert!(check_table(h.screen().commands(), UiLang::EN).is_ok());
}

/// A stand-in for the widget's generic parameter in the signature checks.
type T = VirtualTable<Demo>;

#[test]
#[allow(clippy::type_complexity)]
fn signatures_are_frozen() {
    widget_signatures();
}

#[allow(clippy::type_complexity, clippy::extra_unused_lifetimes)]
fn widget_signatures<'a>() {
    // ---- 2.6
    let _e = [ButtonKind::Secondary, ButtonKind::Primary];
    let _e = [ButtonOut::Clicked];
    let _f: fn(WidgetId, Text, ButtonKind) -> Button = Button::new;
    let _f: fn(&mut Button, Text) = Button::set_label;
    let _f: fn(&mut Button, bool) = Button::set_enabled;
    let _f: fn(&mut Button, bool) = Button::set_busy;
    let _f: fn(&Button) -> NodeId = Button::node_id;
    let _w: Option<&dyn Widget<Out = ButtonOut>> = None::<&Button>.map(|b| b as _);
    let _f: fn(WidgetId, Glyph, Text) -> IconButton = IconButton::new;
    let _f: fn(&mut IconButton, Option<bool>) = IconButton::set_toggled;
    let _f: fn(&mut IconButton, bool) = IconButton::set_enabled;
    let _w: Option<&dyn Widget<Out = ButtonOut>> = None::<&IconButton>.map(|b| b as _);
    let _f: fn(WidgetId, Text, TypeRole, FgRole) -> TextLabel = TextLabel::new;
    let _f: fn(&mut TextLabel, Text) = TextLabel::set_text;
    let _f: fn(&mut TextLabel, Trim) = TextLabel::set_trim;
    let _w: Option<&dyn Widget<Out = ()>> = None::<&TextLabel>.map(|b| b as _);
    let _e = [ToggleOut::Changed(true)];
    let _f: fn(WidgetId, Text, bool) -> Toggle = Toggle::new;
    let _f: fn(&Toggle) -> bool = Toggle::on;
    let _f: fn(&mut Toggle, bool) = Toggle::set_on;
    let _f: fn(&mut Toggle, bool) = Toggle::set_enabled;
    let _f: fn(&mut Toggle, bool) = Toggle::set_busy;
    let _w: Option<&dyn Widget<Out = ToggleOut>> = None::<&Toggle>.map(|b| b as _);
    let _e = [CheckOut::Changed(true)];
    let _f: fn(WidgetId, Text, bool) -> Checkbox = Checkbox::new;
    let _f: fn(&mut Checkbox, bool) = Checkbox::set_mixed;
    let _f: fn(&Checkbox) -> bool = Checkbox::checked;
    let _f: fn(&mut Checkbox, bool) = Checkbox::set_checked;
    let _f: fn(&mut Checkbox, bool) = Checkbox::set_enabled;
    let _w: Option<&dyn Widget<Out = CheckOut>> = None::<&Checkbox>.map(|b| b as _);
    let _e = [SliderOut::Changed(1), SliderOut::Committed(2)];
    let _f: fn(WidgetId, Text, i32, i32, i32, i32) -> Slider = Slider::new;
    let _f: fn(&Slider) -> i32 = Slider::value;
    let _f: fn(&mut Slider, i32) = Slider::set_value;
    let _f: fn(&mut Slider, Option<Text>) = Slider::set_value_text;
    let _w: Option<&dyn Widget<Out = SliderOut>> = None::<&Slider>.map(|b| b as _);
    let _e = [SegOut::Selected(0)];
    let _f: fn(WidgetId, Text, Vec<Text>, Option<usize>) -> Segmented = Segmented::new;
    let _f: fn(&Segmented) -> Option<usize> = Segmented::selected;
    let _f: fn(&mut Segmented, Option<usize>) = Segmented::set_selected;
    let _w: Option<&dyn Widget<Out = SegOut>> = None::<&Segmented>.map(|b| b as _);
    let _s = SidebarItem {
        key: 1,
        label: Cow::Borrowed("Home"),
        glyph: Some(Glyph::Folder),
        icon: None::<Rc<IconBitmap>>,
        depth: 0,
        expandable: true,
        expanded: false,
        section: false,
        trailing: Some((Cow::Borrowed("3"), FgRole::Tertiary)),
        disabled: false,
        busy: false,
    };
    let _s = SidebarStyle { row_h: 36, inset: 8 };
    let _e = [SidebarOut::Activated(1), SidebarOut::Expand(1, true), SidebarOut::ContextMenu(1, PointP::default())];
    let _f: fn(WidgetId, SidebarStyle) -> SidebarList = SidebarList::new;
    let _f: fn(&mut SidebarList, &mut EventCx<'a>, Vec<SidebarItem>) = SidebarList::set_items;
    let _f: fn(&mut SidebarList, Option<u64>) = SidebarList::set_current;
    let _f: fn(&mut SidebarList, Option<u64>) = SidebarList::set_drop_target;
    let _w: Option<&dyn Widget<Out = SidebarOut>> = None::<&SidebarList>.map(|b| b as _);
    let _e = [ScrollOut::Scrolled(0)];
    let _f: fn(WidgetId) -> ScrollBar = ScrollBar::new;
    let _f: fn(&mut ScrollBar, RectP, i64, i64) = ScrollBar::set;
    let _f: fn(&ScrollBar) -> i64 = ScrollBar::offset;
    let _w: Option<&dyn Widget<Out = ScrollOut>> = None::<&ScrollBar>.map(|b| b as _);
    let _f: fn(&mut PaintCx<'a>, PointP, &str, Option<&str>) -> RectP = paint_chip;
    let _f: fn(&mut LayoutCx<'a>, &str, Option<&str>) -> SizeP = chip_size;
    // ---- 2.8, the widget part
    let _f: fn(WidgetId, Demo, Vec<Column>, SelectionMode, TableStyle) -> T = VirtualTable::new;
    let _f: fn(&T) -> &Demo = VirtualTable::model;
    let _f: fn(&mut T) -> &mut Demo = VirtualTable::model_mut;
    let _f: fn(&mut T, &mut EventCx<'a>, bool) = VirtualTable::model_changed;
    let _f: fn(&T) -> &Selection = VirtualTable::selection;
    let _f: fn(&mut T, &mut EventCx<'a>, Selection) = VirtualTable::set_selection;
    let _f: fn(&T) -> Option<RowKey> = VirtualTable::focused;
    let _f: fn(&mut T, &mut EventCx<'a>, RowKey, ScrollTo) = VirtualTable::set_focused;
    let _f: fn(&T) -> &[Column] = VirtualTable::columns;
    let _f: fn(&mut T, &mut EventCx<'a>, Vec<Column>) = VirtualTable::set_columns;
    let _f: fn(&mut T, &mut EventCx<'a>, RowKey, ColId, FieldKind) -> bool = VirtualTable::begin_rename;
    let _f: fn(&T) -> Range<usize> = VirtualTable::visible_rows;
    let _f: fn(&T, usize) -> Option<RectP> = VirtualTable::row_rect;
    let _f: fn(&T) -> RectP = VirtualTable::viewport;
    let _w: Option<&dyn Widget<Out = TableOut>> = None::<&T>.map(|b| b as _);
    let list: VirtualList<Demo> = VirtualTable::new(
        WidgetId(9),
        ListTable(Demo(Vec::new())),
        Vec::new(),
        SelectionMode::Single,
        TableStyle::TASKS,
    );
    let _m: &Demo = &list.model().0;
    let _o: &dyn TableModel = &ListTable(Demo(Vec::new()));
    let _e = [Verb::Close, Verb::End, Verb::EndNow, Verb::Delete, Verb::DeletePermanently, Verb::Restart];
    let _f: fn(Verb, UiLang) -> Text = Verb::label;
    let _f: fn(Verb, UiLang) -> Text = Verb::description;
    let _f: fn(Verb) -> Option<Chord> = Verb::chord;
    let _f: fn(Verb) -> Option<Chord> = Verb::alt_chord;
    let _f: fn(Verb, CommandId, UiLang) -> Command = Verb::command;
    let _f: fn(&CommandTable, UiLang) -> Result<(), String> = check_table;
    // ---- 2.9
    let _f: fn(WidgetId, FieldKind) -> TextField = TextField::new;
    let _f: fn(&TextField) -> &str = TextField::text;
    let _f: fn(&mut TextField, &mut EventCx<'a>, &str, bool) = TextField::set_text;
    let _f: fn(&TextField) -> Range<usize> = TextField::selection;
    let _f: fn(&mut TextField, &mut EventCx<'a>, Range<usize>) = TextField::select;
    let _f: fn(&mut TextField, Text) = TextField::set_placeholder;
    let _f: fn(&mut TextField, Text) = TextField::set_label;
    let _f: fn(&mut TextField, &mut EventCx<'a>, Option<Text>) = TextField::set_error;
    let _f: fn(&mut TextField, bool) = TextField::set_busy;
    let _f: fn(&mut TextField, bool) = TextField::set_clear_button;
    let _f: fn(&mut TextField, u32) = TextField::set_max_chars;
    let _f: fn(&mut TextField, Option<fn(char) -> bool>) = TextField::set_filter;
    let _f: fn(&mut TextField, &mut EventCx<'a>, Vec<Text>) = TextField::set_completions;
    let _f: fn(&TextField) -> NodeId = TextField::node_id;
    let _f: fn(&TextField) -> bool = TextField::is_composing;
    let _w: Option<&dyn Widget<Out = FieldOut>> = None::<&TextField>.map(|b| b as _);
    let _e = [PageTabsOut::Selected(0)];
    let _f: fn(WidgetId, Vec<Text>) -> PageTabs = PageTabs::new;
    let _f: fn(&PageTabs) -> usize = PageTabs::selected;
    let _f: fn(&mut PageTabs, &mut EventCx<'a>, usize) = PageTabs::set_selected;
    let _w: Option<&dyn Widget<Out = PageTabsOut>> = None::<&PageTabs>.map(|b| b as _);
    let _s =
        ToolbarButton { command: CommandId(1), glyph: Glyph::Refresh, tip: Cow::Borrowed("Refresh"), toggled: None };
    let _e = [
        ToolbarOut::Command(CommandId(1)),
        ToolbarOut::Tabs(PageTabsOut::Selected(0)),
        ToolbarOut::Field(FieldOut::Changed),
    ];
    let _f: fn(WidgetId, Option<PageTabs>, Vec<ToolbarButton>, Option<TextField>) -> Toolbar = Toolbar::new;
    let _f: fn(&mut Toolbar) -> Option<&mut PageTabs> = Toolbar::tabs_mut;
    let _f: fn(&mut Toolbar) -> Option<&mut TextField> = Toolbar::field_mut;
    let _f: fn(&mut Toolbar, Option<(Text, FgRole)>) = Toolbar::set_status;
    let _w: Option<&dyn Widget<Out = ToolbarOut>> = None::<&Toolbar>.map(|b| b as _);
    let _s = DialogSpec {
        title: Cow::Borrowed("End"),
        body: Cow::Borrowed("Stops it."),
        buttons: vec![(Cow::Borrowed("Cancel"), CommandId(1)), (Cow::Borrowed("End"), CommandId(2))],
        wide: false,
    };
    let _e = [DialogOut::Chose(CommandId(1)), DialogOut::Cancelled];
    let _f: fn(WidgetId, DialogSpec) -> Dialog = Dialog::new;
    let _w: Option<&dyn Widget<Out = DialogOut>> = None::<&Dialog>.map(|b| b as _);
    let _s = Notice { text: Cow::Borrowed("Copied"), action: Some((Cow::Borrowed("Undo"), CommandId(3))) };
    let _e = [StatusOut::Action(CommandId(3))];
    let _f: fn(WidgetId) -> StatusLine = StatusLine::new;
    let _f: fn(&mut StatusLine, Vec<(Text, FgRole)>) = StatusLine::set_items;
    let _f: fn(&mut StatusLine, &mut EventCx<'a>, Option<Notice>) = StatusLine::set_notice;
    let _w: Option<&dyn Widget<Out = StatusOut>> = None::<&StatusLine>.map(|b| b as _);
    // ---- 2.10
    let _s = TabSpec { key: 1, label: Cow::Borrowed("C:"), tip: None, icon: None, busy: false, error: false };
    let _e = [
        TabOut::Selected(1),
        TabOut::CloseRequested(1),
        TabOut::NewRequested,
        TabOut::Moved { key: 1, to: 0 },
        TabOut::ContextMenu(1, PointP::default()),
    ];
    let _f: fn(WidgetId) -> TabStrip = TabStrip::new;
    let _f: fn(&mut TabStrip, &mut EventCx<'a>, Vec<TabSpec>, u64) = TabStrip::set_tabs;
    let _f: fn(&TabStrip) -> u64 = TabStrip::selected;
    let _f: fn(&TabStrip, PointP) -> CaptionHit = TabStrip::caption_hit;
    let _f: fn(&mut TabStrip, &mut EventCx<'a>, bool) -> Option<u64> = TabStrip::select_next;
    let _f: fn(&mut TabStrip, &mut EventCx<'a>, usize) -> Option<u64> = TabStrip::select_index;
    let _w: Option<&dyn Widget<Out = TabOut>> = None::<&TabStrip>.map(|b| b as _);
    let _s = PathSegment { key: 1, label: Cow::Borrowed("Documents"), tip: None };
    let _e = [
        PathOut::Navigate(1),
        PathOut::SiblingsRequested(1, Anchor::Below(RectP::EMPTY)),
        PathOut::EditStarted,
        PathOut::EditCommitted(String::new()),
        PathOut::EditCancelled,
        PathOut::CompletionRequested(String::new()),
        PathOut::Refresh,
    ];
    let _f: fn(WidgetId) -> PathBar = PathBar::new;
    let _f: fn(&mut PathBar, &mut EventCx<'a>, Vec<PathSegment>, String) = PathBar::set_path;
    let _f: fn(&mut PathBar, &mut EventCx<'a>) = PathBar::begin_edit;
    let _f: fn(&mut PathBar, &mut EventCx<'a>, Vec<Text>) = PathBar::set_completions;
    let _f: fn(&mut PathBar, bool) = PathBar::set_offline;
    let _f: fn(&mut PathBar, &mut EventCx<'a>, Option<Text>) = PathBar::set_error;
    let _w: Option<&dyn Widget<Out = PathOut>> = None::<&PathBar>.map(|b| b as _);
    let _e = [SplitOut::Moved(1), SplitOut::Reset];
    let _f: fn(WidgetId, i32, i32, i32) -> SplitView = SplitView::new;
    let _f: fn(&SplitView) -> i32 = SplitView::first;
    let _f: fn(&mut SplitView, i32) = SplitView::set_first;
    let _f: fn(&SplitView, RectP) -> (RectP, RectP) = SplitView::panes;
    let _w: Option<&dyn Widget<Out = SplitOut>> = None::<&SplitView>.map(|b| b as _);
    let _f: fn(WidgetId) -> Progress = Progress::new;
    let _f: fn(&mut Progress, &mut EventCx<'a>, Option<f32>, bool) = Progress::set;
    let _e = [CaptionOut::Minimize, CaptionOut::MaximizeRestore, CaptionOut::Close];
    let _f: fn(WidgetId) -> CaptionButtons = CaptionButtons::new;
    let _f: fn(&CaptionButtons, PointP) -> CaptionHit = CaptionButtons::hit;
    let _w: Option<&dyn Widget<Out = CaptionOut>> = None::<&CaptionButtons>.map(|b| b as _);
    let values = [1.0_f32, 2.0];
    let _s = Series { values: &values, ink: Ink::white(230) };
    let _f: fn(WidgetId, Text) -> Sparkline = Sparkline::new;
    let _f: fn(&mut Sparkline, &[Series<'_>], f32, Option<f32>) = Sparkline::set;
    let _f: fn(&[f32], usize) -> Vec<f32> = resample_peak;
    let _o: Option<PropertySheet> = None;
}

/// TK2 (F-T4): RowMotion, TableModel::motion, VirtualTable::set_row_height and the three glyphs.
#[test]
fn freeze_tk2_items() {
    struct Moving(Demo);
    impl ListModel for Moving {
        fn len(&self) -> usize {
            self.0.len()
        }
        fn key(&self, row: usize) -> RowKey {
            self.0.key(row)
        }
        fn row_of(&self, key: RowKey) -> Option<usize> {
            self.0.row_of(key)
        }
        fn label(&self, row: usize) -> Cow<'_, str> {
            self.0.label(row)
        }
        fn generation(&self) -> u64 {
            0
        }
    }
    impl TableModel for Moving {
        fn cell(&self, row: usize, _col: ColId, out: &mut CellOut) {
            out.set_text(CellKind::Text, &self.0 .0[row].1);
        }
        fn motion(&self, row: usize) -> RowMotion {
            if row == 0 {
                RowMotion { dy: 6, opacity: 128 }
            } else {
                RowMotion::NONE
            }
        }
    }
    assert_eq!(RowMotion::NONE, RowMotion { dy: 0, opacity: 255 });
    assert_eq!(RowMotion::default(), RowMotion::NONE);
    let m = Moving(Demo(vec![(1, "a".into()), (2, "b".into())]));
    assert_eq!(m.motion(0).dy, 6);
    assert_eq!(m.motion(1), RowMotion::NONE);
    let _f: for<'a> fn(&mut VirtualTable<Moving>, &mut EventCx<'a>, Option<i32>) =
        VirtualTable::<Moving>::set_row_height;
    let _g = [Glyph::Pause, Glyph::SortUpOutline, Glyph::SortDownOutline];
    for g in _g {
        let _shape: fn(f32, f32) -> bool = g.shape();
    }
}
