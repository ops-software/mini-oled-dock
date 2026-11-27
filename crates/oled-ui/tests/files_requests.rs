//! Kit requests of the files app (K-R1): dimmed icons, the rename field's accessor, danger buttons and notes on
//! enabled menu items. Everything runs on the headless Harness: no window on this machine.

use oled_ui::command::CommandTable;
use oled_ui::cx::{Anchor, EventCx, LayoutCx, MenuItem, PaintCx};
use oled_ui::event::{Event, MouseButton, Reply};
use oled_ui::geom::{PointL, RectP, SizeL, SizeP};
use oled_ui::gfx::image::IconBitmap;
use oled_ui::harness::Harness;
use oled_ui::host::AppMsg;
use oled_ui::model::{
    Align, CellKind, CellOut, ColId, Column, IconRef, ListModel, RowKey, SelectionMode, SortDir, TableModel, TableStyle,
};
use oled_ui::screen::{Screen, Widget};
use oled_ui::theme::Tokens;
use oled_ui::widgets::{Button, ButtonKind, FieldKind, VirtualTable};
use oled_ui::{CommandId, Text, WidgetId};

const SIZE: SizeL = SizeL { w: 400, h: 120 };

/// Two rows with the same opaque white icon; row 1 asks for it dimmed.
struct Rows {
    icon: IconBitmap,
    dim_second: bool,
}

impl Rows {
    fn new(dim_second: bool) -> Rows {
        let mut icon = IconBitmap::new(16, 16);
        icon.pixels.fill(0xFFFF_FFFF);
        Rows { icon, dim_second }
    }
}

impl ListModel for Rows {
    fn len(&self) -> usize {
        2
    }
    fn key(&self, row: usize) -> RowKey {
        RowKey(row as u64)
    }
    fn row_of(&self, key: RowKey) -> Option<usize> {
        (key.0 < 2).then_some(key.0 as usize)
    }
    fn label(&self, row: usize) -> std::borrow::Cow<'_, str> {
        std::borrow::Cow::Owned(format!("row {row}"))
    }
    fn generation(&self) -> u64 {
        0
    }
}

impl TableModel for Rows {
    fn cell(&self, row: usize, col: ColId, out: &mut CellOut) {
        out.clear();
        out.set_text(CellKind::Text, &format!("row {row}"));
        if col.0 == 0 {
            out.icon = Some(IconRef(1));
            out.dim_icon = self.dim_second && row == 1;
        }
    }
    fn icon(&self, _icon: IconRef, _px: i32) -> Option<&IconBitmap> {
        Some(&self.icon)
    }
}

type Op = Box<dyn FnOnce(&mut VirtualTable<Rows>, &mut EventCx) + Send>;

struct TableScreen {
    t: VirtualTable<Rows>,
    cmds: CommandTable,
}

impl Screen for TableScreen {
    fn layout(&mut self, cx: &mut LayoutCx, avail: SizeP) -> SizeP {
        self.t.layout(cx, RectP::wh(0, 0, cx.px(400), cx.px(120)));
        avail
    }
    fn paint(&self, cx: &mut PaintCx) {
        self.t.paint(cx);
    }
    fn event(&mut self, cx: &mut EventCx, ev: &Event) -> Reply {
        self.t.event(cx, ev);
        Reply::None
    }
    fn data(&mut self, cx: &mut EventCx, msg: AppMsg) -> Reply {
        if let Ok(op) = msg.downcast::<Op>() {
            (*op)(&mut self.t, cx);
        }
        Reply::None
    }
    fn commands(&self) -> &CommandTable {
        &self.cmds
    }
}

fn table_harness(dim_second: bool) -> Harness<TableScreen> {
    let col = Column {
        id: ColId(0),
        title: Text::from("Name"),
        width: 300,
        min: 48,
        flex: false,
        align: Align::Start,
        sort: None,
        first_sort: SortDir::Asc,
        visible: true,
        hideable: false,
        tip: None,
    };
    let t = VirtualTable::new(WidgetId(1), Rows::new(dim_second), vec![col], SelectionMode::Multi, TableStyle::FILES);
    Harness::new(TableScreen { t, cmds: CommandTable::new() }, SIZE, 96, Tokens::oled())
}

/// The brightest blue channel inside the horizontal band of icon x 14..26 on the rows' y range `ys`.
fn peak(h: &mut Harness<TableScreen>, ys: std::ops::Range<u32>) -> u32 {
    let img = h.render();
    let mut best = 0;
    for y in ys {
        for x in 14..26u32 {
            best = best.max(img.px[(y * img.w + x) as usize] & 0xFF);
        }
    }
    best
}

#[test]
fn dim_icon_halves_icon_alpha() {
    let mut h = table_harness(true);
    h.render();
    let r0 = h.screen().t.row_rect(0).unwrap();
    let r1 = h.screen().t.row_rect(1).unwrap();
    let full = peak(&mut h, r0.t as u32..r0.b as u32);
    let dim = peak(&mut h, r1.t as u32..r1.b as u32);
    assert!(full >= 250, "an opaque white icon is drawn in full: {full}");
    assert!((120..=136).contains(&dim), "the dimmed icon is at half alpha: {dim}");
    // The default is off and clear() resets it.
    let mut c = CellOut::default();
    assert!(!c.dim_icon);
    c.dim_icon = true;
    c.clear();
    assert!(!c.dim_icon);
}

#[test]
fn rename_field_mut_is_some_only_while_renaming() {
    let mut h = table_harness(false);
    h.render();
    let none = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(true));
    let probe = |h: &mut Harness<TableScreen>, expect_some: bool, begin: bool| {
        let seen = none.clone();
        let op: Op = Box::new(move |t, cx| {
            if begin {
                assert!(t.begin_rename(cx, RowKey(0), ColId(0), FieldKind::Plain));
            }
            let some = t.rename_field_mut().is_some();
            seen.store(some == expect_some, std::sync::atomic::Ordering::SeqCst);
        });
        h.send(Box::new(op));
        assert!(none.load(std::sync::atomic::Ordering::SeqCst), "expected Some = {expect_some}");
    };
    probe(&mut h, false, false);
    probe(&mut h, true, true);
    h.key("Esc");
    probe(&mut h, false, false);
}

struct ButtonScreen {
    b: Button,
    cmds: CommandTable,
}

impl Screen for ButtonScreen {
    fn layout(&mut self, cx: &mut LayoutCx, avail: SizeP) -> SizeP {
        self.b.layout(cx, RectP::wh(cx.px(10), cx.px(8), cx.px(100), cx.px(28)));
        avail
    }
    fn paint(&self, cx: &mut PaintCx) {
        self.b.paint(cx);
    }
    fn event(&mut self, cx: &mut EventCx, ev: &Event) -> Reply {
        self.b.event(cx, ev);
        Reply::None
    }
    fn commands(&self) -> &CommandTable {
        &self.cmds
    }
}

/// The count of pixels in the button's rect that are red-dominant (the danger accent) and of grey ones (text ink).
fn red_pixels(danger: bool, kind: ButtonKind, enabled: bool) -> usize {
    let mut b = Button::new(WidgetId(1), Text::from("Delete"), kind);
    b.set_danger(danger);
    b.set_enabled(enabled);
    let screen = ButtonScreen { b, cmds: CommandTable::new() };
    let mut h = Harness::new(screen, SizeL { w: 120, h: 44 }, 96, Tokens::oled());
    let img = h.render();
    img.px
        .iter()
        .filter(|p| {
            let (r, g, bl) = ((**p >> 16) & 0xFF, (**p >> 8) & 0xFF, **p & 0xFF);
            r > 120 && r > g + 40 && r > bl + 40
        })
        .count()
}

#[test]
fn danger_button_label_colour() {
    assert_eq!(red_pixels(false, ButtonKind::Secondary, true), 0, "no red without the flag");
    assert!(red_pixels(true, ButtonKind::Secondary, true) > 20, "the label is in the danger accent");
    assert_eq!(red_pixels(true, ButtonKind::Secondary, false), 0, "a disabled button stays disabled grey");
}

struct MenuScreen {
    cmds: CommandTable,
    note: bool,
}

impl Screen for MenuScreen {
    fn layout(&mut self, cx: &mut LayoutCx, avail: SizeP) -> SizeP {
        let r = RectP::wh(cx.px(10), cx.px(10), cx.px(100), cx.px(28));
        cx.push(
            oled_ui::Node::new(oled_ui::NodeId::new(WidgetId(1), 0), oled_ui::Role::ListItem, r)
                .label(Text::from("row"))
                .focus(oled_ui::Focusable::Tab),
        );
        avail
    }
    fn paint(&self, _cx: &mut PaintCx) {}
    fn event(&mut self, cx: &mut EventCx, ev: &Event) -> Reply {
        if let Event::Click { button: MouseButton::Right, pos, .. } = ev {
            let note = self.note.then(|| Text::from("Only for this folder"));
            cx.open_menu(
                vec![MenuItem::Custom {
                    id: CommandId(5),
                    label: Text::from("Apply"),
                    glyph: None,
                    chord: None,
                    enabled: true,
                    checked: None,
                    note,
                    danger: false,
                }],
                Anchor::Point(*pos),
            );
        }
        Reply::None
    }
    fn commands(&self) -> &CommandTable {
        &self.cmds
    }
}

fn menu_image(note: bool) -> oled_ui::harness::Image {
    let screen = MenuScreen { cmds: CommandTable::new(), note };
    let mut h = Harness::new(screen, SizeL { w: 400, h: 300 }, 96, Tokens::oled());
    h.right_click(PointL { x: 20, y: 20 });
    h.menu_image(0).expect("the menu is open")
}

#[test]
fn enabled_note_is_drawn() {
    let plain = menu_image(false);
    let noted = menu_image(true);
    assert!(noted.h > plain.h, "the note takes a line under the enabled item ({} vs {})", noted.h, plain.h);
    let lit = |i: &oled_ui::harness::Image| i.px.iter().filter(|p| **p & 0xFF > 60).count();
    assert!(lit(&noted) > lit(&plain), "and its text is drawn");
}
