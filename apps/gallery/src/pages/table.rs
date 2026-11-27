//! The table page (K2a.5): a files-style table (plates, header) over a tasks-style one (bars, no header) on the shared
//! demo model, and the pieces the table100k and mock_tasks pages are made of.

use std::borrow::Cow;

use super::{s, FocusRow, Page};
use oled_ui::command::CommandId;
use oled_ui::command::CommandTable;
use oled_ui::cx::{EventCx, LayoutCx, PaintCx};
use oled_ui::event::{Event, Reply};
use oled_ui::geom::{RectP, SizeL, SizeP};
use oled_ui::gfx::image::IconBitmap;
use oled_ui::host::AppMsg;
use oled_ui::model::{
    Align, Badge, CellKind, CellOut, ColId, Column, FgRole, HeatStep, IconRef, ListModel, RowAction, RowKey, RowMotion,
    RowOut, ScrollTo, Selection, SelectionMode, SortDir, TableModel, TableOut, TableStyle, TreeInfo,
};
use oled_ui::node::WidgetId;
use oled_ui::screen::{Screen, Widget};
use oled_ui::strings::UiLang;
use oled_ui::text::Trim;
use oled_ui::widgets::VirtualTable;

/// One row of the demo model: every look a row can have.
#[derive(Clone, Debug, Default)]
pub struct Row {
    pub key: u64,
    pub name: String,
    pub size: u64,
    pub cpu: f32,
    pub depth: u16,
    pub expandable: bool,
    pub expanded: bool,
    pub section: bool,
    pub ghost: bool,
    pub cut: bool,
    pub fresh: bool,
    pub error: bool,
    pub heat: u8,
    pub badge: Option<Badge>,
    pub note: Option<&'static str>,
    pub action: bool,
    pub icon: Option<u32>,
}

/// The demo model: stored rows, or `synthetic` generated ones (no storage: a hundred thousand rows cost nothing).
pub struct Demo {
    pub rows: Vec<Row>,
    pub synthetic: usize,
    pub generation: u64,
    /// Stem / extension spans in the name (the files look).
    pub ext_spans: bool,
    pub action_label: &'static str,
    /// One row painted in motion (a slide and a fade), by row index.
    pub motion: Option<(usize, RowMotion)>,
    icons: Vec<IconBitmap>,
}

impl Demo {
    pub fn new(rows: Vec<Row>) -> Demo {
        Demo {
            rows,
            synthetic: 0,
            generation: 0,
            ext_spans: false,
            action_label: "Close",
            motion: None,
            icons: icons(),
        }
    }

    pub fn synthetic(n: usize) -> Demo {
        Demo { synthetic: n, ..Demo::new(Vec::new()) }
    }

    fn synth(i: usize) -> Row {
        Row {
            key: i as u64,
            name: format!("item {i}.txt"),
            size: (i as u64 % 977) * 4096 + i as u64 % 13,
            cpu: (i % 400) as f32 / 4.0,
            icon: Some(0),
            ..Row::default()
        }
    }

    fn row(&self, i: usize) -> Cow<'_, Row> {
        if self.synthetic > 0 {
            Cow::Owned(Demo::synth(i))
        } else {
            Cow::Borrowed(&self.rows[i])
        }
    }
}

/// Three 16 px icons: plain rounded squares in three greys, enough to show where an icon sits.
fn icons() -> Vec<IconBitmap> {
    [200u32, 150, 110]
        .into_iter()
        .map(|g| {
            let mut b = IconBitmap::new(16, 16);
            for y in 0..16u32 {
                for x in 0..16u32 {
                    let corner = !(2..=13).contains(&x) && !(2..=13).contains(&y);
                    if !corner && (2..14).contains(&x) && (1..15).contains(&y) {
                        b.pixels[(y * 16 + x) as usize] = 0xFF00_0000 | (g << 16) | (g << 8) | g;
                    }
                }
            }
            b
        })
        .collect()
}

/// "612 MB", "1.2 GB", "340 KB".
fn size_text(bytes: u64) -> String {
    const UNITS: [&str; 5] = ["B", "KB", "MB", "GB", "TB"];
    let (mut v, mut u) = (bytes as f64, 0);
    while v >= 1000.0 && u + 1 < UNITS.len() {
        v /= 1024.0;
        u += 1;
    }
    if u == 0 || v >= 100.0 {
        format!("{} {}", v.round() as u64, UNITS[u])
    } else {
        format!("{:.1} {}", v, UNITS[u])
    }
}

impl ListModel for Demo {
    fn len(&self) -> usize {
        if self.synthetic > 0 {
            self.synthetic
        } else {
            self.rows.len()
        }
    }

    fn key(&self, row: usize) -> RowKey {
        RowKey(if self.synthetic > 0 { row as u64 } else { self.rows[row].key })
    }

    fn row_of(&self, key: RowKey) -> Option<usize> {
        if self.synthetic > 0 {
            return ((key.0 as usize) < self.synthetic).then_some(key.0 as usize);
        }
        self.rows.iter().position(|r| r.key == key.0)
    }

    fn label(&self, row: usize) -> Cow<'_, str> {
        match self.row(row) {
            Cow::Borrowed(r) => Cow::Borrowed(r.name.as_str()),
            Cow::Owned(r) => Cow::Owned(r.name),
        }
    }

    fn generation(&self) -> u64 {
        self.generation
    }
}

impl TableModel for Demo {
    fn motion(&self, row: usize) -> RowMotion {
        self.motion.filter(|(r, _)| *r == row).map_or(RowMotion::NONE, |(_, m)| m)
    }

    fn cell(&self, row: usize, col: ColId, out: &mut CellOut) {
        let r = self.row(row);
        if r.section && col.0 != 0 {
            out.set_text(CellKind::Text, "");
            return;
        }
        match col.0 {
            0 => {
                out.set_text(CellKind::Text, &r.name);
                out.icon = r.icon.map(IconRef);
                out.badge = r.badge;
                out.cut = r.cut;
                out.trim = Trim::Middle { tail: 10 };
                if self.ext_spans {
                    if let Some(dot) = r.name.rfind('.') {
                        out.push_span(dot as u32, r.name.len() as u32, FgRole::Secondary);
                    }
                }
                if let Some(n) = r.note {
                    out.note = Some((Cow::Borrowed(n), FgRole::Tertiary));
                }
                if r.action {
                    out.action = Some(RowAction {
                        label: Cow::Borrowed(self.action_label),
                        command: CommandId(100 + (r.key % 1000) as u32),
                    });
                }
            }
            1 => {
                out.set_text(CellKind::Bytes, &size_text(r.size));
                out.align = Align::Decimal;
            }
            2 => {
                out.set_text(CellKind::Percent, &format!("{:.1}%", r.cpu));
                out.align = Align::Decimal;
                out.heat = (r.heat > 0).then_some(HeatStep(r.heat));
                out.fg = FgRole::Secondary;
            }
            _ => {
                out.set_text(CellKind::Text, if r.error { "Failed" } else { "Ready" });
                out.fg = if r.error { FgRole::Danger } else { FgRole::Secondary };
            }
        }
    }

    fn row(&self, row: usize, out: &mut RowOut) {
        let r = self.row(row);
        *out = RowOut { section: r.section, ghost: r.ghost, error: r.error, fresh: r.fresh, ..RowOut::default() };
    }

    fn tree(&self, row: usize) -> Option<TreeInfo> {
        let r = self.row(row);
        (r.depth > 0 || r.expandable).then_some(TreeInfo {
            depth: r.depth,
            expandable: r.expandable,
            expanded: r.expanded,
        })
    }

    fn icon(&self, icon: IconRef, _px: i32) -> Option<&IconBitmap> {
        self.icons.get(icon.0 as usize)
    }
}

/// The demo's columns: name (flex), size, CPU and status.
pub fn columns(lang: UiLang) -> Vec<Column> {
    let col = |id: u16, title, width, flex, align, hideable| Column {
        id: ColId(id),
        title,
        width,
        min: 48,
        flex,
        align,
        sort: None,
        first_sort: SortDir::Asc,
        visible: true,
        hideable,
        tip: None,
    };
    let mut c = vec![
        col(0, s(lang, "Name", "Nome"), 240, true, Align::Start, false),
        col(1, s(lang, "Size", "Dimensione"), 110, false, Align::Decimal, true),
        col(2, s(lang, "CPU", "CPU"), 90, false, Align::Decimal, true),
        col(3, s(lang, "Status", "Stato"), 110, false, Align::Start, true),
    ];
    c[1].sort = Some(SortDir::Asc);
    c
}

/// A rect in logical px: left, top, width, height.
pub type LogicalRect = (i32, i32, i32, i32);

/// A screen of tables at fixed rects (logical px) over the demo model; the shared body of the table pages.
pub struct Tables {
    pub tables: Vec<(RectP, VirtualTable<Demo>)>,
    pub outs: Vec<TableOut>,
    pub cmds: CommandTable,
}

impl Tables {
    /// `rects` are logical (l, t, w, h).
    pub fn new(tables: Vec<(LogicalRect, VirtualTable<Demo>)>) -> Tables {
        let tables =
            tables.into_iter().map(|((l, t, w, h), table)| (RectP { l, t, r: l + w, b: t + h }, table)).collect();
        Tables { tables, outs: Vec::new(), cmds: CommandTable::new() }
    }
}

impl Screen for Tables {
    fn layout(&mut self, cx: &mut LayoutCx, avail: SizeP) -> SizeP {
        for (r, t) in &mut self.tables {
            t.layout(cx, RectP { l: cx.px(r.l), t: cx.px(r.t), r: cx.px(r.r), b: cx.px(r.b) });
        }
        avail
    }

    fn paint(&self, cx: &mut PaintCx) {
        for (_, t) in &self.tables {
            t.paint(cx);
        }
    }

    fn event(&mut self, cx: &mut EventCx, ev: &Event) -> Reply {
        for (_, t) in &mut self.tables {
            if let Some(o) = t.event(cx, ev) {
                self.outs.push(o);
            }
            self.outs.extend(t.take_extra());
        }
        Reply::None
    }

    fn data(&mut self, cx: &mut EventCx, msg: AppMsg) -> Reply {
        if let Some(FocusRow(n)) = msg.downcast_ref::<FocusRow>() {
            for (_, t) in &mut self.tables {
                if *n < t.model().len() {
                    let k = t.model().key(*n);
                    t.set_focused(cx, k, ScrollTo::Center);
                }
            }
        }
        Reply::None
    }

    fn commands(&self) -> &CommandTable {
        &self.cmds
    }
}

fn named(n: u64, name: &str) -> Row {
    Row {
        key: n,
        name: name.to_string(),
        size: 1024 * 1024 * (n * 37 % 900 + 3),
        cpu: (n * 7 % 90) as f32 / 3.0,
        icon: Some((n % 3) as u32),
        ..Row::default()
    }
}

/// The files-style rows: runs of selection, every badge, cut, fresh, ghost, a folder with children.
pub fn files_rows() -> Vec<Row> {
    let mut rows: Vec<Row> = [
        "Annual report.docx",
        "Budget 2026.xlsx",
        "holiday-photos-from-the-long-summer-trip-to-the-coast-2025.zip",
        "notes.txt",
        "Projects",
        "main.rs",
        "lib.rs",
        "Cargo.toml",
        "readme.md",
        "backup.tar.gz",
    ]
    .iter()
    .enumerate()
    .map(|(i, n)| named(i as u64, n))
    .collect();
    rows[0].badge = Some(Badge::CloudOnline);
    rows[1].badge = Some(Badge::CloudLocal);
    rows[2].badge = Some(Badge::CloudPinned);
    rows[3].badge = Some(Badge::Link);
    rows[4].icon = Some(1);
    for r in &mut rows[5..8] {
        r.depth = 1;
    }
    rows[6].cut = true;
    rows[7].fresh = true;
    rows[8].ghost = true;
    rows[9].badge = Some(Badge::Lock);
    rows[9].note = Some("Locked");
    rows
}

/// The tasks-style rows: sections, tree rows, heat 1-4, notes, an error and the hover action.
pub fn tasks_rows() -> Vec<Row> {
    let mut rows: Vec<Row> = Vec::new();
    rows.push(Row { key: 1000, name: "Apps".into(), section: true, ..Row::default() });
    for (i, n) in ["browser", "editor", "terminal", "mixer", "player", "chat"].iter().enumerate() {
        let mut r = named(i as u64, n);
        r.heat = (i % 5) as u8;
        r.action = true;
        rows.push(r);
    }
    rows[1].expandable = true;
    rows[1].expanded = true;
    for i in 0..2 {
        let mut c = named(20 + i, "renderer");
        c.depth = 1;
        c.heat = 2;
        rows.push(c);
    }
    rows[4].note = Some("Not responding");
    rows[4].error = true;
    rows.push(Row { key: 1001, name: "Background".into(), section: true, ..Row::default() });
    for (i, n) in ["indexer", "updater", "sync", "audio-host"].iter().enumerate() {
        let mut r = named(40 + i as u64, n);
        r.heat = if i == 0 { 4 } else { 0 };
        r.badge = (i == 1).then_some(Badge::Shield);
        rows.push(r);
    }
    rows
}

/// A selection of the keys.
pub fn select(keys: &[u64]) -> Selection {
    let mut s = Selection::None;
    for (i, k) in keys.iter().enumerate() {
        if i == 0 {
            s.click(RowKey(*k));
        } else {
            s.toggle(RowKey(*k));
        }
    }
    s
}

const SIZE: SizeL = SizeL { w: 720, h: 540 };

pub fn page(lang: UiLang) -> Option<Page> {
    let mut files = Demo::new(files_rows());
    files.ext_spans = true;
    let mut top = VirtualTable::new(WidgetId(1), files, columns(lang), SelectionMode::Multi, TableStyle::FILES)
        .with_selection(select(&[1, 2, 3, 6, 7]))
        .with_focused(RowKey(2));
    top.set_label(s(lang, "Files", "File"));
    let mut cols = columns(lang);
    cols.truncate(3);
    let tasks = Demo::new(tasks_rows());
    let mut bottom = VirtualTable::new(WidgetId(2), tasks, cols, SelectionMode::Multi, TableStyle::TASKS)
        .with_selection(select(&[2, 20]))
        .with_focused(RowKey(2));
    bottom.set_label(s(lang, "Tasks", "Attivita"));
    let screen = Tables::new(vec![((16, 16, 688, 292), top), ((16, 324, 688, 200), bottom)]);
    Some(Page { name: "table", size: SIZE, states: 1, screen: Box::new(screen) })
}
