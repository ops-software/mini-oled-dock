//! kit_min (K2a.7, KC1): the smallest app on the kit (EventLoop, one opaque window, a VirtualTable of 10 rows, a
//! TextField, a Menu and a Toolbar), whose release size is the exe_size.kit_min row. Built release it measures what the
//! kit adds to an exe; while TextField, Toolbar and Menu are stubs it measures stubs and the budget gates nothing.
//! Run it and it opens its window and exits when the window closes.

use std::borrow::Cow;

use gallery::pages::table::{columns, Demo, Row};
use oled_ui::command::{CommandId, CommandTable};
use oled_ui::cx::{Anchor, EventCx, LayoutCx, MenuItem, PaintCx};
use oled_ui::event::{Event, MouseButton, Reply};
use oled_ui::geom::{PointP, RectP, SizeL, SizeP};
use oled_ui::host::{AppId, Caption, EventLoop, WindowKind, WindowSpec};
use oled_ui::model::{SelectionMode, TableStyle};
use oled_ui::node::WidgetId;
use oled_ui::screen::{Screen, Widget};
use oled_ui::strings::{Lang, UiLang};
use oled_ui::theme::ThemeSpec;
use oled_ui::widgets::{FieldKind, TextField, Toolbar, VirtualTable};

/// A toolbar over a field over a table of ten rows; a right click opens a menu.
struct KitMin {
    toolbar: Toolbar,
    field: TextField,
    table: VirtualTable<Demo>,
    cmds: CommandTable,
}

impl Screen for KitMin {
    fn layout(&mut self, cx: &mut LayoutCx, avail: SizeP) -> SizeP {
        let (bar, field) = (cx.px(44), cx.px(36));
        self.toolbar.layout(cx, RectP { l: 0, t: 0, r: avail.w, b: bar });
        self.field.layout(cx, RectP { l: cx.px(8), t: bar, r: avail.w - cx.px(8), b: bar + field });
        self.table.layout(cx, RectP { l: 0, t: bar + field, r: avail.w, b: avail.h });
        avail
    }

    fn paint(&self, cx: &mut PaintCx) {
        self.toolbar.paint(cx);
        self.field.paint(cx);
        self.table.paint(cx);
    }

    fn event(&mut self, cx: &mut EventCx, ev: &Event) -> Reply {
        if let Event::Click { pos, button: MouseButton::Right, .. } = ev {
            let item = MenuItem::Custom {
                id: CommandId(1),
                label: Cow::Borrowed("Quit"),
                glyph: None,
                chord: None,
                enabled: true,
                checked: None,
                note: None,
                danger: false,
            };
            cx.open_menu(vec![item], Anchor::Point(PointP { x: pos.x, y: pos.y }));
        }
        if let Event::Command(CommandId(1)) = ev {
            return Reply::Close;
        }
        self.toolbar.event(cx, ev);
        self.field.event(cx, ev);
        self.table.event(cx, ev);
        Reply::None
    }

    fn commands(&self) -> &CommandTable {
        &self.cmds
    }
}

fn main() {
    let rows: Vec<Row> = (0..10)
        .map(|i| Row { key: i, name: format!("row {i}"), size: 4096 * (i + 1), cpu: i as f32, ..Row::default() })
        .collect();
    let screen = KitMin {
        toolbar: Toolbar::new(WidgetId(1), None, Vec::new(), None),
        field: TextField::new(WidgetId(2), FieldKind::Search),
        table: VirtualTable::new(
            WidgetId(3),
            Demo::new(rows),
            columns(UiLang::EN),
            SelectionMode::Single,
            TableStyle::FILES,
        ),
        cmds: CommandTable::new(),
    };
    let spec = WindowSpec {
        kind: WindowKind::Opaque { min: SizeL { w: 320, h: 240 }, caption: Caption::System },
        class: "MiniOledKitMin",
        title: Cow::Borrowed("kit_min"),
        size: SizeL { w: 640, h: 480 },
        owner: None,
        esc_closes: true,
        placement: None,
        icon_resource: None,
        opened_by_chord: false,
    };
    let mut lp = match EventLoop::new(AppId("kit_min"), Lang::En, ThemeSpec::oled()) {
        Ok(lp) => lp,
        Err(e) => {
            eprintln!("kit_min: {e:?}");
            std::process::exit(1);
        }
    };
    if let Err(e) = lp.open(spec, screen) {
        eprintln!("kit_min: {e:?}");
        std::process::exit(1);
    }
    std::process::exit(lp.run());
}
