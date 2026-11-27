//! The K0.1a freeze (P2-kit 2.2, 2.3, 2.4 and the model part of 2.8): every item used the way an app uses it. This file
//! must keep compiling unchanged: a breaking change to a frozen item shows here.

use std::borrow::Cow;
use std::collections::BTreeMap;
use std::rc::Rc;

use oled_ui::model::*;
use oled_ui::strings::{k, pseudo};
use oled_ui::text::*;
use oled_ui::tokens;
use oled_ui::widgets::{FieldKind, FieldOut};
use oled_ui::*;
// Through an alias: tests/workspace.rs reads the plain "Command" + "::new(" as a process spawn.
use oled_ui::Command as Cmd;
use windows::Win32::Foundation::RECT;

/// F-P2's documented default theme file, written as a literal (ThemeFile::default is a PF1 stub until TH1).
fn oled_file() -> ThemeFile {
    ThemeFile {
        schema: 1,
        generation: 0,
        writer: String::new(),
        language: oled_config::theme::Lang::Auto,
        look: Look {
            preset: Preset::Oled,
            seed: Seed {
                bg: Rgb(0x00, 0x00, 0x00),
                fg: Rgb(0xFF, 0xFF, 0xFF),
                accent: None,
                attention: Rgb(0xFF, 0x8C, 0x00),
                danger: Rgb(0xFF, 0x45, 0x3A),
                white_level: 100,
            },
            knobs: Knobs {
                bar_opacity: 95,
                surface_opacity: 95,
                corners: Corners::Normal,
                density_pct: 100,
                rim: RimStyle::Hairline,
                plate: PlateStyle::Fill,
                indicator: IndicatorStyle::Line,
                font: FontSpec { family: String::new(), weight_delta: 0 },
                sizes: Sizes { icon: 16, text: 13, tray: 16, module_pct: 100 },
                motion: MotionSpec { mode: Motion::System, speed_pct: 100 },
            },
        },
        overrides: BTreeMap::new(),
        a11y: A11y { reduced_motion: None, text_scale_pct: None },
    }
}

/// An app's rows: (key, name).
struct Rows(Vec<(u64, String)>);

impl ListModel for Rows {
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

impl TableModel for Rows {
    fn cell(&self, row: usize, col: ColId, out: &mut CellOut) {
        out.clear();
        match col {
            ColId(0) => out.set_text(CellKind::Text, &self.0[row].1),
            _ => out.set_text(CellKind::Num, "1").push_span(0, 1, FgRole::Secondary),
        };
    }
}

#[test]
fn core_types_are_usable() {
    let id = NodeId::new(WidgetId(7), 3);
    assert_eq!(id.owner(), WidgetId(7));
    assert_eq!(id.slot(), 3);
    assert_eq!(Chord::parse("Ctrl+Shift+Tab"), Some(Chord::new(Mods::CTRL | Mods::SHIFT, Key::TAB)));
    assert_eq!(Tokens::oled().color.fg_primary, Ink::white(230));
    for (dpi, want) in [(96, 2), (120, 2), (144, 2), (192, 3)] {
        assert_eq!(Scale { dpi }.ring(), tokens::rim_focus_px(dpi));
        assert_eq!(Scale { dpi }.ring(), want, "{dpi} DPI");
    }
    let r = focus_ring(RectP { l: 10, t: 5, r: 36, b: 31 }, 7, Scale { dpi: 96 });
    assert_eq!((r.rect, r.radius, r.width), (RectP { l: 6, t: 1, r: 40, b: 35 }, 11, 2));
    let plate = RectP { l: 20, t: 10, r: 72, b: 62 };
    let r = focus_ring(plate, 14, Scale { dpi: 192 });
    assert_eq!((r.rect, r.radius, r.width), (plate.inset(-7, -7), 21, 3));
    assert_eq!(UiLang::EN.kit(k::KIT_CLOSE), "Close");
    assert_eq!(UiLang::IT.kit(k::KIT_CLOSE), "Chiudi");
    let p = UiLang { italian: false, pseudo: true }.kit(k::KIT_CLOSE);
    assert!(p.starts_with('[') && p.len() > "Close".len(), "{p}");
    assert_eq!(ThemeSpec::from(&oled_file()), ThemeSpec::oled());
    let kit = CommandId::kit(WidgetId(3), 1);
    assert!(kit.is_kit());
    let mut table = CommandTable::new();
    assert!(table.add(Cmd::new(kit, Cow::Borrowed("Kit"))).is_err());
    assert!(table
        .add(Cmd::new(CommandId(1), Cow::Borrowed("Open")).chord(Chord::new(Mods::CTRL, Key::letter('O'))))
        .is_ok());
    let rows = Rows(vec![(1, "one".into()), (2, "two".into())]);
    let mut out = CellOut::default();
    rows.cell(1, ColId(0), &mut out);
    assert_eq!((rows.len(), rows.row_of(RowKey(2)), out.text.as_str()), (2, Some(1), "two"));
    assert!(KeySet::new().is_empty());
    assert_eq!(FieldKind::Rename { stem_end: 4 }, FieldKind::Rename { stem_end: 4 });
    assert_ne!(FieldOut::Changed, FieldOut::Cancelled);
}

#[test]
#[allow(clippy::type_complexity)]
fn signatures_are_frozen() {
    // ---- geom (2.2)
    let _f: fn(i32, i32, i32, i32) -> RectP = RectP::wh;
    let _f: fn(&RectP) -> i32 = RectP::w;
    let _f: fn(&RectP) -> i32 = RectP::h;
    let _f: fn(&RectP) -> bool = RectP::is_empty;
    let _f: fn(&RectP, PointP) -> bool = RectP::contains;
    let _f: fn(&RectP, RectP) -> RectP = RectP::intersect;
    let _f: fn(&RectP, RectP) -> RectP = RectP::union;
    let _f: fn(&RectP, i32, i32) -> RectP = RectP::inset;
    let _f: fn(&RectP, i32, i32) -> RectP = RectP::offset;
    let _f: fn(&RectP) -> i64 = RectP::area;
    let _c: RectP = RectP::EMPTY;
    let _f: fn(RectP) -> RECT = RECT::from;
    let _f: fn(RECT) -> RectP = RectP::from;
    let _f: fn(&Scale, SizeL) -> SizeP = Scale::size;
    let _f: fn(&Scale, PointL) -> PointP = Scale::point;
    let _f: fn(&Scale, i32) -> i32 = Scale::logical;
    let _f: fn(&Scale) -> i32 = Scale::ring;
    let _f: fn(&Scale) -> f32 = Scale::scale;
    let _f: fn(&Scale, i32) -> i32 = Scale::px;
    let _f: fn(RectP, i32, Scale) -> FocusRing = focus_ring;
    let _f: fn(RectP, i32, Scale) -> FocusRing = focus_ring_inset;
    let _s = FocusRing { rect: RectP::EMPTY, radius: 0, width: 0 };
    let _s = (PointP { x: 0, y: 0 }, SizeP { w: 0, h: 0 }, PointL { x: 0, y: 0 }, SizeL { w: 0, h: 0 });
    // ---- theme (2.2)
    let _f: fn(u32) -> Ink = Ink::white;
    let _f: fn(u32, u32, u32) -> Ink = Ink::solid;
    let _f: fn(Ink, u32) -> Ink = Ink::with_a;
    let _f: fn(Ink, f32) -> Ink = Ink::faded;
    let _s = Ink { rgb: (0, 0, 0), a: 0 };
    let _f: fn(TypeRole) -> TypeSpec = TypeRole::spec;
    let _s = TypeSpec { size: 0, line: 0, weight: 0, display_face: false };
    let _e = [
        TypeRole::Small,
        TypeRole::Caption,
        TypeRole::Body,
        TypeRole::BodyStrong,
        TypeRole::Section,
        TypeRole::Title,
        TypeRole::Display,
    ];
    let _f: fn(&ThemeFile) -> ThemeSpec = |f| ThemeSpec::from(f);
    let _f: fn() -> ThemeSpec = ThemeSpec::oled;
    let _s = ThemeSpec {
        look: ThemeSpec::oled().look,
        overrides: BTreeMap::<String, i64>::new(),
        a11y: A11y { reduced_motion: Some(true), text_scale_pct: Some(150) },
    };
    let _f: fn() -> SysOverlay = SysOverlay::none;
    let _f: fn() -> SysOverlay = SysOverlay::read;
    let _f: fn(SysOverlay, &A11y) -> SysOverlay = SysOverlay::with_theme;
    let _s = SysOverlay { high_contrast: None::<HcColors>, reduced_motion: false, text_scale: 1.0 };
    let _s = HcColors {
        window: (0, 0, 0),
        window_text: (0, 0, 0),
        highlight: (0, 0, 0),
        highlight_text: (0, 0, 0),
        gray_text: (0, 0, 0),
    };
    let _f: fn() -> Tokens = Tokens::oled;
    let _f: fn(&ThemeSpec, &SysOverlay) -> Tokens = Tokens::resolve;
    let _f: fn(&ThemeSpec) -> Vec<String> = Tokens::resolve_report;
    let _f: fn(oled_config::theme::Lang) -> oled_sys::i18n::Lang = theme_lang;
    let t = Tokens::oled();
    let _s: (&ColorTokens, &SizeTokens, &SpaceTokens, &RadiusTokens, &MotionTokens, f32, u64, bool) =
        (&t.color, &t.size, &t.space, &t.radius, &t.motion, t.text_scale, t.generation, t.high_contrast);
    let _s: (Option<Ink>, [Ink; 4], Ink, Ink) = (t.color.accent, t.color.heat, t.color.nav_bar, t.color.disabled_veil);
    let _s: (i32, i32, i32) = (t.size.row_table, t.size.slider_knob_r_hot, t.space.s8 + t.radius.bar);
    let _s: (u32, i32, bool) = (t.motion.spring_delay, t.motion.slide_px, t.motion.reduced);
    // ---- text and glyphs (2.3)
    let _s = FontId(0);
    let _e = [Trim::None, Trim::End, Trim::Middle { tail: 10 }, Trim::Start];
    let _s = LayoutOpts { max_width: Some(1), trim: Trim::End, wrap: false };
    let _s = TextLayout {
        text: String::new(),
        font: FontId(0),
        width: 0,
        height: 0,
        lines: vec![(0..0, 0)],
        trimmed: false,
    };
    let _s = TextHit { index: 0, trailing: false };
    let _s = FontMetrics { ascent: 0, descent: 0, line_height: 0 };
    let _f: fn() -> Result<GdiTextEngine, host::UiError> = GdiTextEngine::new;
    let _f: fn(&mut GdiTextEngine, u32) = GdiTextEngine::evict_dpi;
    let _f: fn(&mut GdiTextEngine, TypeRole, Option<u16>, Scale, f32) -> FontId = <GdiTextEngine as TextEngine>::font;
    let _f: fn(&mut GdiTextEngine, &str, FontId, &LayoutOpts) -> Rc<TextLayout> = <GdiTextEngine as TextEngine>::layout;
    let _f: fn(&mut GdiTextEngine, &str, FontId) -> i32 = <GdiTextEngine as TextEngine>::width;
    let _f: fn(&mut GdiTextEngine, &mut Canvas<'_>, &TextLayout, PointP, Ink) = <GdiTextEngine as TextEngine>::draw;
    let _f: fn(&GdiTextEngine, &TextLayout, i32) -> TextHit = <GdiTextEngine as TextEngine>::hit_test;
    let _f: fn(&GdiTextEngine, &TextLayout, usize) -> i32 = <GdiTextEngine as TextEngine>::caret_x;
    let _f: fn(&GdiTextEngine, FontId) -> FontMetrics = <GdiTextEngine as TextEngine>::metrics;
    let _o: Option<&mut dyn TextEngine> = None;
    let _e = [Glyph::ChevronUp, Glyph::Prompt, Glyph::Search, Glyph::Error];
    // ---- node (2.4)
    let _t: Text = Cow::Borrowed("x");
    let _f: fn(WidgetId, u32) -> NodeId = NodeId::new;
    let _f: fn(NodeId) -> WidgetId = NodeId::owner;
    let _f: fn(NodeId) -> u32 = NodeId::slot;
    let _e = [Role::Window, Role::Button, Role::Table, Role::Row, Role::TitleBar];
    let _f: fn(NodeFlags, NodeFlags) -> bool = NodeFlags::contains;
    let _f: fn(NodeFlags, NodeFlags) -> NodeFlags = <NodeFlags as std::ops::BitOr>::bitor;
    let _c = [
        NodeFlags::FOCUSABLE,
        NodeFlags::DISABLED,
        NodeFlags::SELECTED,
        NodeFlags::CHECKED,
        NodeFlags::EXPANDED,
        NodeFlags::BUSY,
        NodeFlags::INVALID,
        NodeFlags::EXPANDABLE,
        NodeFlags::HOVER_FADE,
        NodeFlags::MIXED,
        NodeFlags::A11Y_HIDDEN,
    ];
    let _e = [Focusable::No, Focusable::Tab, Focusable::Roving];
    let _e = [Label::None, Label::Text(Cow::Borrowed("x"))];
    let _e = [
        NodeValue::Text(Cow::Borrowed("x")),
        NodeValue::Range { value: 0.0, min: 0.0, max: 1.0, step: 0.1 },
        NodeValue::Toggle(true),
        NodeValue::Position { index: 1, count: 2 },
        NodeValue::Level(1),
        NodeValue::Percent(0.5),
    ];
    let _c = [
        Actions::CLICK,
        Actions::FOCUS,
        Actions::EXPAND,
        Actions::COLLAPSE,
        Actions::SCROLL_INTO_VIEW,
        Actions::INCREMENT,
        Actions::DECREMENT,
        Actions::SET_VALUE,
        Actions::CONTEXT_MENU,
        Actions::CLOSE,
    ];
    let _f: fn(Actions, Actions) -> Actions = <Actions as std::ops::BitOr>::bitor;
    let _e = [Cursor::Arrow, Cursor::IBeam, Cursor::SizeWE, Cursor::SizeNS, Cursor::Hand];
    let _f: fn(NodeId, Role, RectP) -> Node = Node::new;
    let _f: fn(Node, NodeId) -> Node = Node::parent;
    let _f: fn(Node, NodeFlags) -> Node = Node::flags;
    let _f: fn(Node, Focusable) -> Node = Node::focus;
    let _f: fn(Node, Text) -> Node = Node::label;
    let _f: fn(Node, Text) -> Node = Node::description;
    let _f: fn(Node, NodeValue) -> Node = Node::value;
    let _f: fn(Node, Actions) -> Node = Node::actions;
    let _f: fn(Node, Cursor) -> Node = Node::cursor;
    let n = Node::new(NodeId(0), Role::Group, RectP::EMPTY);
    let _s: (
        NodeId,
        Option<NodeId>,
        WidgetId,
        Role,
        RectP,
        NodeFlags,
        Focusable,
        &Label,
        &Option<Text>,
        &Option<NodeValue>,
        Actions,
        Cursor,
    ) = (
        n.id,
        n.parent,
        n.owner,
        n.role,
        n.rect,
        n.flags,
        n.focus,
        &n.label,
        &n.description,
        &n.value,
        n.actions,
        n.cursor,
    );
    let _f: fn(&Frame, NodeId) -> Option<&Node> = Frame::get;
    let _f: fn(&Frame, PointP) -> Option<&Node> = Frame::hit;
    let _f: fn(&Frame) -> std::slice::Iter<'_, Node> = Frame::iter;
    let _f: fn(&Frame) -> usize = Frame::len;
    // ---- event (2.4)
    let _c = [Mods::NONE, Mods::CTRL, Mods::SHIFT, Mods::ALT, Mods::WIN];
    let _f: fn(Mods) -> bool = Mods::ctrl;
    let _f: fn(Mods) -> bool = Mods::shift;
    let _f: fn(Mods) -> bool = Mods::alt;
    let _f: fn(Mods) -> bool = Mods::win;
    let _f: fn(Mods, Mods) -> bool = Mods::contains;
    let _f: fn(Mods, Mods) -> Mods = <Mods as std::ops::BitOr>::bitor;
    let _c = [
        Key::BACK,
        Key::TAB,
        Key::ENTER,
        Key::ESC,
        Key::SPACE,
        Key::PAGE_UP,
        Key::PAGE_DOWN,
        Key::END,
        Key::HOME,
        Key::LEFT,
        Key::UP,
        Key::RIGHT,
        Key::DOWN,
        Key::INSERT,
        Key::DELETE,
        Key::APPS,
        Key::F1,
        Key::F2,
        Key::F4,
        Key::F5,
        Key::F6,
        Key::F10,
        Key::ADD,
        Key::SUBTRACT,
        Key::OEM_PLUS,
        Key::OEM_COMMA,
        Key::OEM_MINUS,
        Key::OEM_PERIOD,
    ];
    let _f: fn(char) -> Key = Key::letter;
    let _f: fn(u8) -> Key = Key::digit;
    let _e = [MouseButton::Left, MouseButton::Right, MouseButton::Middle, MouseButton::X1, MouseButton::X2];
    let _e = [WheelDelta::Lines(1.0), WheelDelta::Pixels(1.0)];
    let _e = [
        ImeEvent::Start,
        ImeEvent::Composition { text: String::new(), caret: 0 },
        ImeEvent::Result(String::new()),
        ImeEvent::End,
    ];
    let _e = [FocusReason::Keyboard, FocusReason::Pointer, FocusReason::Programmatic, FocusReason::WindowActivated];
    let _e = [
        A11yAction::Click,
        A11yAction::Focus,
        A11yAction::Expand,
        A11yAction::Collapse,
        A11yAction::ScrollIntoView,
        A11yAction::Increment,
        A11yAction::Decrement,
        A11yAction::SetValue(String::new()),
        A11yAction::ShowContextMenu,
    ];
    let _f: fn(WidgetId, u8) -> TimerId = TimerId::kit;
    let _f: fn(TimerId) -> bool = TimerId::is_kit;
    let p = PointP::default();
    let m = Mods::NONE;
    let b = MouseButton::Left;
    let _e = [
        Event::PointerMove { pos: p, mods: m },
        Event::PointerDown { pos: p, button: b, clicks: 1, mods: m },
        Event::PointerUp { pos: p, button: b, mods: m },
        Event::Click { pos: p, button: b, clicks: 1, mods: m },
        Event::DragStart { origin: p, pos: p, button: b, mods: m },
        Event::DragMove { pos: p, mods: m },
        Event::DragEnd { pos: p, mods: m, cancelled: false },
        Event::PointerLeave,
        Event::Wheel { pos: p, delta: WheelDelta::Lines(1.0), horizontal: false, mods: m },
        Event::Key { key: Key::TAB, down: true, repeat: false, mods: m },
        Event::Text('a'),
        Event::Ime(ImeEvent::Start),
        Event::FocusIn(FocusReason::Keyboard),
        Event::FocusOut,
        Event::Command(CommandId(1)),
        Event::ListPicked { menu: MenuId(1), index: 0 },
        Event::MenuClosed(MenuId(1)),
        Event::Timer(TimerId(1)),
        Event::A11y(NodeId(1), A11yAction::Click),
        Event::WindowActive(true),
        Event::CloseRequested,
    ];
    let _e = [Reply::None, Reply::Damage(RectP::EMPTY), Reply::Redraw, Reply::Relayout, Reply::Close];
    // ---- command (2.4)
    let _c: u32 = CommandId::KIT_BASE;
    let _f: fn(WidgetId, u16) -> CommandId = CommandId::kit;
    let _f: fn(CommandId) -> bool = CommandId::is_kit;
    let _f: fn(Mods, Key) -> Chord = Chord::new;
    let _f: fn(&str) -> Option<Chord> = Chord::parse;
    let _f: fn(&Chord, UiLang) -> String = Chord::label;
    let _s = Chord { mods: Mods::NONE, key: Key::F2 };
    let _f: fn(CommandId, Text) -> Command = Command::new;
    let _f: fn(Command, Chord) -> Command = Command::chord;
    let _f: fn(Command, Chord) -> Command = Command::alt_chord;
    let _f: fn(Command, Glyph) -> Command = Command::glyph;
    let _f: fn(Command, Text) -> Command = Command::description;
    let c = Cmd::new(CommandId(2), Cow::Borrowed("x"));
    let _s: (CommandId, &Text, Option<Glyph>, Option<Chord>, Option<Chord>, bool, Option<bool>, &Option<Text>) =
        (c.id, &c.label, c.glyph, c.chord, c.alt_chord, c.enabled, c.checked, &c.description);
    let _f: fn() -> CommandTable = CommandTable::new;
    let _f: fn(&mut CommandTable, Command) -> Result<(), String> = CommandTable::add;
    let _f: fn(&CommandTable, CommandId) -> Option<&Command> = CommandTable::get;
    let _f: fn(&mut CommandTable, CommandId) -> Option<&mut Command> = CommandTable::get_mut;
    let _f: fn(&CommandTable, Chord) -> Option<&Command> = CommandTable::by_chord;
    let _f: fn(&mut CommandTable, CommandId, bool) = CommandTable::set_enabled;
    let _f: fn(&mut CommandTable, CommandId, Option<bool>) = CommandTable::set_checked;
    let _f: fn(&CommandTable) -> std::slice::Iter<'_, Command> = CommandTable::iter;
    let _d = CommandTable::default();
    // ---- strings (2.4)
    let _e = [Lang::Auto, Lang::En, Lang::It];
    let _c = [UiLang::EN, UiLang::IT, UiLang { italian: true, pseudo: true }];
    let _f: fn(Lang) -> UiLang = UiLang::from_lang;
    let _f: fn(UiLang, &'static oled_sys::i18n::Table, oled_sys::i18n::S) -> Text = UiLang::text;
    let _f: fn(UiLang, oled_sys::i18n::S) -> Text = UiLang::kit;
    let _f: fn(&str) -> String = pseudo;
    let _k: oled_sys::i18n::S = k::KIT_APP_FILES;
    // ---- model (2.8)
    let _s = (RowKey(0), ColId(0), TreeInfo::default(), RowOut::default(), HeatStep(1), IconRef(0));
    let _e = [CellKind::Text, CellKind::Num, CellKind::Bytes, CellKind::Percent, CellKind::Icon, CellKind::Spark];
    let _e = [
        FgRole::Primary,
        FgRole::Hot,
        FgRole::Secondary,
        FgRole::Tertiary,
        FgRole::Disabled,
        FgRole::Attention,
        FgRole::Danger,
    ];
    let _e = [Align::Start, Align::End, Align::Decimal];
    let _s = Span { start: 0, end: 0, fg: FgRole::Primary };
    let _e = [
        Badge::Link,
        Badge::CloudOnline,
        Badge::CloudLocal,
        Badge::CloudPinned,
        Badge::Error,
        Badge::Shield,
        Badge::Lock,
    ];
    let _s = RowAction { label: Cow::Borrowed("End"), command: CommandId(3) };
    let _s = TreeInfo { depth: 0, expandable: false, expanded: false };
    let _s = RowOut { section: false, ghost: false, busy: false, error: false, fresh: false, disabled: false };
    let _f: fn(&Rows) -> usize = <Rows as ListModel>::len;
    let _f: fn(&Rows, usize) -> RowKey = <Rows as ListModel>::key;
    let _f: fn(&Rows, RowKey) -> Option<usize> = <Rows as ListModel>::row_of;
    let _f: fn(&Rows, usize) -> Cow<'_, str> = <Rows as ListModel>::label;
    let _f: fn(&Rows) -> u64 = <Rows as ListModel>::generation;
    let _f: fn(&Rows, usize, ColId, &mut CellOut) = <Rows as TableModel>::cell;
    let _f: fn(&Rows, usize, &mut RowOut) = <Rows as TableModel>::row;
    let _f: fn(&Rows, usize) -> Option<TreeInfo> = <Rows as TableModel>::tree;
    let _f: fn(&Rows, IconRef, i32) -> Option<&gfx::image::IconBitmap> = <Rows as TableModel>::icon;
    let _f: fn(&Rows, usize, ColId) -> Option<&[f32]> = <Rows as TableModel>::spark;
    let _o: Option<&dyn TableModel> = None;
    let _f: fn(&mut CellOut) = CellOut::clear;
    let _f: for<'a> fn(&'a mut CellOut, CellKind, &str) -> &'a mut CellOut = CellOut::set_text;
    let _f: fn(&mut CellOut, u32, u32, FgRole) -> &mut CellOut = CellOut::push_span;
    let o = CellOut::default();
    let _s: (CellKind, &String, [Span; 4], u8, FgRole, TypeRole, Trim, Align) =
        (o.kind, &o.text, o.spans, o.n_spans, o.fg, o.role, o.trim, o.align);
    let _s: (
        Option<HeatStep>,
        Option<IconRef>,
        Option<Badge>,
        bool,
        &Option<(Text, FgRole)>,
        &Option<RowAction>,
        &Option<Text>,
    ) = (o.heat, o.icon, o.badge, o.cut, &o.note, &o.action, &o.tip);
    let _f: fn() -> KeySet = KeySet::new;
    let _f: fn(&KeySet) -> usize = KeySet::len;
    let _f: fn(&KeySet) -> bool = KeySet::is_empty;
    let _f: fn(&KeySet, RowKey) -> bool = KeySet::contains;
    let _f: fn(&mut KeySet, RowKey) -> bool = KeySet::insert;
    let _f: fn(&mut KeySet, RowKey) -> bool = KeySet::remove;
    let _f: fn(&mut KeySet) = KeySet::clear;
    let _f: fn(&KeySet) -> Box<dyn Iterator<Item = RowKey> + '_> = KeySet::iter;
    let _f: fn(&KeySet) -> usize = KeySet::heap_bytes;
    let _e = [
        Selection::None,
        Selection::Single(RowKey(1)),
        Selection::Keys(KeySet::new()),
        Selection::AllExcept(KeySet::default()),
    ];
    let _f: fn(&Selection, RowKey) -> bool = Selection::contains;
    let _f: fn(&Selection) -> bool = Selection::is_empty;
    let _f: fn(&Selection, usize) -> usize = Selection::count;
    let _f: fn(&Selection) -> Option<RowKey> = Selection::single;
    let _f: fn(&Selection, &dyn ListModel, &mut dyn FnMut(usize, RowKey)) = Selection::for_each;
    let _f: fn(&mut Selection, &dyn ListModel) = Selection::retain_present;
    let _f: fn(&mut Selection, RowKey) = Selection::click;
    let _f: fn(&mut Selection, RowKey) = Selection::toggle;
    let _f: fn(&mut Selection) = Selection::select_all;
    let _f: fn(&mut Selection, &dyn ListModel, RowKey, RowKey, bool) = Selection::extend;
    let _e = [SelectionMode::None, SelectionMode::Single, SelectionMode::Multi];
    let _e = [SortDir::Asc, SortDir::Desc];
    let _s = Column {
        id: ColId(0),
        title: Cow::Borrowed("Name"),
        width: 200,
        min: 48,
        flex: true,
        align: Align::Start,
        sort: Some(SortDir::Asc),
        first_sort: SortDir::Asc,
        visible: true,
        hideable: false,
        tip: None,
    };
    let r = RowKey(1);
    let _e = [
        TableOut::Activated(r),
        TableOut::SortRequested(ColId(0), SortDir::Desc),
        TableOut::SelectionChanged,
        TableOut::FocusChanged(Some(r)),
        TableOut::ContextMenu(None, PointP::default()),
        TableOut::ColumnsChanged,
        TableOut::ExpandRequested(r, true),
        TableOut::RowAction(r, CommandId(1)),
        TableOut::RenameRequested(r),
        TableOut::RenameCommitted(r, String::new()),
        TableOut::RenameCancelled(r),
    ];
    let _e = [SelectionLook::Plate, SelectionLook::Bar];
    let _c = [TableStyle::FILES, TableStyle::TASKS];
    let _s = TableStyle {
        selection: SelectionLook::Bar,
        row_inset: 8,
        hover_rim: true,
        header: false,
        icon_x: 32,
        merge_runs: false,
    };
    let _e = [ScrollTo::Nearest, ScrollTo::Center, ScrollTo::Top];
    // ---- widgets::text_field, the K0.1a part (2.9)
    let _e = [FieldKind::Search, FieldKind::Plain, FieldKind::Path, FieldKind::Rename { stem_end: 0 }];
    let _e = [
        FieldOut::Changed,
        FieldOut::Committed(String::new()),
        FieldOut::Cancelled,
        FieldOut::CompletionRequested(String::new()),
        FieldOut::CompletionAccepted(String::new()),
        FieldOut::CommitAndMove { text: String::new(), back: true },
        FieldOut::Refused('*'),
    ];
}
