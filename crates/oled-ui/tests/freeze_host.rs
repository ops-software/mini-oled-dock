//! The K0.1b freeze (P2-kit 2.5): the contexts, Screen, Widget, the host and the Harness, used the way an app uses
//! them. This file must keep compiling unchanged: a breaking change to a frozen item shows here.

use std::borrow::Cow;

use oled_ui::harness::{golden, FakeClipboard, Harness, Image, MenuSnapshot};
use oled_ui::host::*;
use oled_ui::model::Align;
use oled_ui::present::band_height;
use oled_ui::text::{FontId, TextEngine, Trim};
use oled_ui::*;
use windows::Win32::Foundation::{HANDLE, HWND};

/// Two buttons-like nodes.
struct Two {
    cmds: CommandTable,
    events: u32,
}

impl Two {
    fn new() -> Two {
        Two { cmds: CommandTable::new(), events: 0 }
    }
}

impl Screen for Two {
    fn layout(&mut self, cx: &mut LayoutCx, avail: SizeP) -> SizeP {
        let (w, h) = (cx.px(80), cx.px(28));
        let one = Node::new(NodeId::new(WidgetId(1), 0), Role::Button, RectP::wh(0, 0, w, h));
        cx.push(one.label(Cow::Borrowed("One")).focus(Focusable::Tab).actions(Actions::CLICK));
        let two = Node::new(NodeId::new(WidgetId(2), 0), Role::Button, RectP::wh(w + cx.px(8), 0, w, h));
        cx.push(
            two.label(Cow::Borrowed("Two")).focus(Focusable::Tab).actions(Actions::CLICK).flags(NodeFlags::HOVER_FADE),
        );
        avail
    }

    fn paint(&self, cx: &mut PaintCx) {
        let ink = cx.tokens().color.fill_control;
        if cx.visible(RectP::wh(0, 0, 10, 10)) {
            cx.fill(RectP::wh(0, 0, 10, 10), cx.px(7), ink);
        }
    }

    fn event(&mut self, cx: &mut EventCx, ev: &Event) -> Reply {
        self.events += 1;
        if let Event::Click { .. } = ev {
            if let Some(t) = cx.target() {
                cx.set_focus(t);
            }
        }
        Reply::None
    }

    fn commands(&self) -> &CommandTable {
        &self.cmds
    }
}

#[test]
fn host_api_is_usable() {
    let mut h = Harness::new(Two::new(), SizeL { w: 400, h: 300 }, 144, Tokens::oled());
    let img = h.render();
    assert_eq!((img.w, img.h), (600, 450));
    // Black outside the 10 x 10 fill of Two::paint, so the test does not depend on PaintCx::fill's body (K1a.4).
    let outside = |i: usize| i % img.w as usize >= 15 || i / img.w as usize >= 15;
    assert!(img.px.iter().enumerate().all(|(i, &p)| !outside(i) || p == 0xFF00_0000), "a black image");
    assert!(h.is_idle());
    let dump = h.a11y_dump();
    assert_eq!(dump.lines().count(), 2, "{dump}");
    assert!(dump.contains("\"One\"") && dump.contains("\"Two\""), "{dump}");
    h.assert_named();
    assert_eq!(band_height(1920), 136);
    assert_eq!(band_height(3840), 68);
    assert_eq!(band_height(100), 256);
    assert_eq!(band_height(20000), 32);
    h.click(PointL { x: 10, y: 10 });
    assert_eq!(h.focused(), Some(NodeId::new(WidgetId(1), 0)));
    h.key("Tab").type_text("ab").ime("x", Some("y")).hover(PointL { x: 100, y: 10 }).advance(500);
    h.double_click(PointL { x: 100, y: 10 })
        .right_click(PointL { x: 1, y: 1 })
        .drag(PointL { x: 1, y: 1 }, PointL { x: 50, y: 1 });
    h.wheel(PointL { x: 1, y: 1 }, -1.0).send(Box::new(7u32)).set_active(false).set_active(true);
    assert!(h.screen().events > 10);
    h.screen_mut().events = 0;
    h.clipboard().text = Some("x".into());
    assert!(h.menus().is_empty() && h.toasts().is_empty() && h.menu_image(0).is_none());
    assert!(!h.last_outputs().is_empty());
    let h = Harness::new(Two::new(), SizeL { w: 100, h: 50 }, 96, Tokens::oled()).lang(Lang::It).pseudo(true);
    let h = h.caption(Caption::Custom);
    assert_eq!(h.a11y_dump().lines().count(), 2);
}

/// A window as an app opens one (never called: no test creates a window, P2-kit 0.4 rule 1).
#[allow(dead_code)]
fn build() -> Result<(), UiError> {
    let mut lp = EventLoop::new(AppId("gallery"), Lang::Auto, ThemeSpec::oled())?;
    let spec = WindowSpec {
        kind: WindowKind::Opaque { min: SizeL { w: 320, h: 240 }, caption: Caption::System },
        class: "MiniOledGallery",
        title: Cow::Borrowed("Gallery"),
        size: SizeL { w: 640, h: 480 },
        owner: None,
        esc_closes: false,
        placement: Some(PlacementKey("main")),
        icon_resource: None,
        opened_by_chord: false,
    };
    let w = lp.open(spec, Two::new())?;
    let proxy = lp.proxy(w);
    let _ = std::thread::spawn(move || proxy.send(Box::new(1u32)));
    let _h = lp.add_handle(HANDLE::default(), Box::new(|cx: &mut LoopCx| cx.quit(0)))?;
    let _t = lp.follow_theme_file(
        oled_config::theme::ThemeWatch::open(),
        Box::new(|cx: &mut LoopCx, f: &ThemeFile| cx.set_theme(ThemeSpec::from(f))),
    )?;
    let _ = lp.hwnd(w);
    let _ = lp.run();
    Ok(())
}

/// A stored placement.
struct NoStore;

impl PlacementStore for NoStore {
    fn load(&self, key: PlacementKey) -> Option<Placement> {
        let _ = key;
        None
    }
    fn save(&mut self, key: PlacementKey, p: &Placement) {
        let _ = (key, p);
    }
}

/// A widget with a typed output.
struct Pill;

impl Widget for Pill {
    type Out = bool;
    fn layout(&mut self, cx: &mut LayoutCx, rect: RectP) {
        cx.push(Node::new(NodeId::new(WidgetId(9), 0), Role::Button, rect).label(Cow::Borrowed("Pill")));
    }
    fn paint(&self, cx: &mut PaintCx) {
        cx.focus_ring(RectP::wh(0, 0, 10, 10), 4, false);
    }
    fn event(&mut self, cx: &mut EventCx, ev: &Event) -> Option<bool> {
        let _ = cx;
        matches!(ev, Event::Click { .. }).then_some(true)
    }
}

fn assert_send_sync<T: Send + Sync>() {}

#[test]
#[allow(clippy::type_complexity)]
fn signatures_are_frozen() {
    assert_send_sync::<Proxy>();
    // ---- cx (2.5)
    let _s = AnimId { node: NodeId(0), channel: 0 };
    let _e = [Ease::OutCubic, Ease::In, Ease::Linear];
    let _s = MenuId(0);
    let _e = [
        Anchor::Point(PointP::default()),
        Anchor::Below(RectP::EMPTY),
        Anchor::Above(RectP::EMPTY),
        Anchor::Right(RectP::EMPTY),
    ];
    let _e = [
        MenuItem::Command(CommandId(1)),
        MenuItem::Custom {
            id: CommandId(2),
            label: Cow::Borrowed("x"),
            glyph: None,
            chord: None,
            enabled: true,
            checked: None,
            note: None,
            danger: false,
        },
        MenuItem::Submenu { label: Cow::Borrowed("x"), glyph: Some(Glyph::More), items: vec![MenuItem::Separator] },
        MenuItem::Header(Cow::Borrowed("x")),
        MenuItem::Separator,
    ];
    let _s = ToastSpec { text: Cow::Borrowed("x"), action: None, progress: Some(0.5), hold_ms: None };
    let mut fake = FakeClipboard { text: None, files: false };
    let c: &mut dyn Clipboard = &mut fake;
    let _ = (c.text(), c.set_text("x"), c.has_files());
    cx_signatures();
    // ---- screen (2.5)
    let _o: Option<Box<dyn Screen>> = Some(Box::new(Two::new()));
    let _f: fn(&mut Two, &mut LayoutCx<'_>, SizeP) -> SizeP = <Two as Screen>::layout;
    let _f: fn(&Two, &mut PaintCx<'_>) = <Two as Screen>::paint;
    let _f: fn(&mut Two, &mut EventCx<'_>, &Event) -> Reply = <Two as Screen>::event;
    let _f: fn(&mut Two, &mut EventCx<'_>, AppMsg) -> Reply = <Two as Screen>::data;
    let _f: fn(&Two) -> &CommandTable = <Two as Screen>::commands;
    let _f: fn(&Two, PointP) -> CaptionHit = <Two as Screen>::caption_hit;
    let _f: fn(&mut Box<dyn Screen>, &mut LayoutCx<'_>, SizeP) -> SizeP = <Box<dyn Screen> as Screen>::layout;
    let _f: fn(&mut Pill, &mut LayoutCx<'_>, RectP) = <Pill as Widget>::layout;
    let _f: fn(&Pill, &mut PaintCx<'_>) = <Pill as Widget>::paint;
    let _f: fn(&mut Pill, &mut EventCx<'_>, &Event) -> Option<bool> = <Pill as Widget>::event;
    let _f: fn(&mut Pill, &mut LayoutCx<'_>) -> SizeP = <Pill as Widget>::preferred;
    let _e = [
        CaptionHit::Client,
        CaptionHit::Caption,
        CaptionHit::MinButton,
        CaptionHit::MaxButton,
        CaptionHit::CloseButton,
    ];
    // ---- host (2.5)
    let _s = AppId("tasks");
    let _e = [Caption::System, Caption::Custom];
    let _e = [
        WindowKind::Opaque { min: SizeL::default(), caption: Caption::Custom },
        WindowKind::Layered { activate: false, topmost: true },
    ];
    let _s = (PlacementKey("main"), WindowRef(0), HandleId(0));
    let _s = Placement { monitor: String::new(), rect: RectP::EMPTY, dpi: 96, maximized: false };
    let _b: Box<dyn PlacementStore> = Box::new(NoStore);
    let _s: Option<HWND> = None;
    let _m: AppMsg = Box::new(0u8);
    let _e = [
        UiError::Win32(windows::core::Error::empty()),
        UiError::TooManyHandles,
        UiError::NoSuchWindow,
        UiError::Gfx(String::new()),
        UiError::NotReady("K0"),
    ];
    let _f: fn(&Proxy, AppMsg) -> bool = Proxy::send;
    let _f: fn(AppId, Lang, ThemeSpec) -> Result<EventLoop, UiError> = EventLoop::new;
    let _f: fn(&mut EventLoop, Box<dyn PlacementStore>) = EventLoop::set_placement_store;
    let _f: fn(&mut EventLoop, WindowSpec, Two) -> Result<WindowRef, UiError> = EventLoop::open::<Two>;
    let _f: fn(&EventLoop, WindowRef) -> Proxy = EventLoop::proxy;
    let _f: fn(&mut EventLoop, HANDLE, Box<dyn FnMut(&mut LoopCx)>) -> Result<HandleId, UiError> =
        EventLoop::add_handle;
    let _f: fn(&mut EventLoop, HandleId) = EventLoop::remove_handle;
    let _f: fn(&mut EventLoop, ThemeSpec) = EventLoop::set_theme;
    let _f: fn(
        &mut EventLoop,
        oled_config::theme::ThemeWatch,
        Box<dyn FnMut(&mut LoopCx, &ThemeFile)>,
    ) -> Result<HandleId, UiError> = EventLoop::follow_theme_file;
    let _f: fn(&mut EventLoop, Lang) = EventLoop::set_lang;
    let _f: fn(&EventLoop, WindowRef) -> Option<HWND> = EventLoop::hwnd;
    let _f: fn(&mut EventLoop) = EventLoop::caption_spike;
    let _f: fn(EventLoop) -> i32 = EventLoop::run;
    assert!(EventLoop::new(AppId("gallery"), Lang::En, ThemeSpec::oled()).is_ok(), "a loop opens no window by itself");
    // ---- present and harness (2.5)
    let _f: fn(i32) -> i32 = band_height;
    let _s = Image { w: 0, h: 0, px: Vec::new(), text_rects: Vec::new() };
    let _f: fn(&Image, &std::path::Path) -> Result<(), String> = Image::write_png;
    let _f: fn(&Image, RectP) -> Image = Image::crop;
    let _s = MenuSnapshot { id: MenuId(1), items: Vec::new(), at: Anchor::Point(PointP::default()), highlight: None };
    let _f: fn(Two, SizeL, u32, Tokens) -> Harness<Two> = Harness::new;
    let _f: fn(Harness<Two>, Lang) -> Harness<Two> = Harness::lang;
    let _f: fn(Harness<Two>, bool) -> Harness<Two> = Harness::pseudo;
    let _f: fn(Harness<Two>, Caption) -> Harness<Two> = Harness::caption;
    let _f: for<'h> fn(&'h mut Harness<Two>, &str) -> &'h mut Harness<Two> = Harness::key;
    let _f: for<'h> fn(&'h mut Harness<Two>, &str) -> &'h mut Harness<Two> = Harness::type_text;
    let _f: for<'h> fn(&'h mut Harness<Two>, &str, Option<&str>) -> &'h mut Harness<Two> = Harness::ime;
    let _f: for<'h> fn(&'h mut Harness<Two>, PointL) -> &'h mut Harness<Two> = Harness::click;
    let _f: for<'h> fn(&'h mut Harness<Two>, PointL) -> &'h mut Harness<Two> = Harness::double_click;
    let _f: for<'h> fn(&'h mut Harness<Two>, PointL) -> &'h mut Harness<Two> = Harness::right_click;
    let _f: for<'h> fn(&'h mut Harness<Two>, PointL) -> &'h mut Harness<Two> = Harness::hover;
    let _f: for<'h> fn(&'h mut Harness<Two>, PointL, PointL) -> &'h mut Harness<Two> = Harness::drag;
    let _f: for<'h> fn(&'h mut Harness<Two>, PointL, f32) -> &'h mut Harness<Two> = Harness::wheel;
    let _f: for<'h> fn(&'h mut Harness<Two>, AppMsg) -> &'h mut Harness<Two> = Harness::send;
    let _f: for<'h> fn(&'h mut Harness<Two>, u32) -> &'h mut Harness<Two> = Harness::advance;
    let _f: for<'h> fn(&'h mut Harness<Two>, bool) -> &'h mut Harness<Two> = Harness::set_active;
    let _f: for<'h> fn(&'h mut Harness<Two>, Tokens) -> &'h mut Harness<Two> = Harness::set_tokens;
    let _f: fn(&mut Harness<Two>) -> Image = Harness::render;
    let _f: fn(&mut Harness<Two>, usize) -> Option<Image> = Harness::menu_image;
    let _f: fn(&Harness<Two>) -> String = Harness::a11y_dump;
    let _f: fn(&Harness<Two>) = Harness::assert_named;
    let _f: fn(&Harness<Two>) -> bool = Harness::is_idle;
    let _f: fn(&Harness<Two>) -> &Two = Harness::screen;
    let _f: fn(&mut Harness<Two>) -> &mut Two = Harness::screen_mut;
    let _f: fn(&mut Harness<Two>) -> &mut FakeClipboard = Harness::clipboard;
    let _f: fn(&Harness<Two>) -> &[MenuSnapshot] = Harness::menus;
    let _f: fn(&Harness<Two>) -> &[ToastSpec] = Harness::toasts;
    let _f: fn(&Harness<Two>) -> Option<NodeId> = Harness::focused;
    let _f: fn(&Harness<Two>) -> &[String] = Harness::last_outputs;
    let _f: fn(&std::path::Path, &std::path::Path, &str, &Image) = golden::check;
}

/// The contexts' methods (their impl lifetime is fixed here; the reference to the context stays higher-ranked).
#[allow(clippy::type_complexity, clippy::extra_unused_lifetimes)]
fn cx_signatures<'a>() {
    let _f: fn(&mut LayoutCx<'a>, Node) = LayoutCx::push;
    let _f: fn(&LayoutCx<'a>) -> Scale = LayoutCx::scale;
    let _f: fn(&LayoutCx<'a>, i32) -> i32 = LayoutCx::px;
    let _f: for<'r> fn(&'r LayoutCx<'a>) -> &'r Tokens = LayoutCx::tokens;
    let _f: fn(&LayoutCx<'a>) -> UiLang = LayoutCx::lang;
    let _f: for<'r> fn(&'r mut LayoutCx<'a>) -> &'r mut dyn TextEngine = LayoutCx::text;
    let _f: fn(&mut LayoutCx<'a>, TypeRole) -> FontId = LayoutCx::font;
    let _f: fn(&LayoutCx<'a>) -> Caption = LayoutCx::effective_caption;
    let _f: fn(&mut LayoutCx<'a>, i32) = LayoutCx::set_toast_baseline;
    let _f: fn(&LayoutCx<'a>) -> u64 = LayoutCx::now_ms;
    let _f: fn(&PaintCx<'a>) -> RectP = PaintCx::clip;
    let _f: fn(&PaintCx<'a>, RectP) -> bool = PaintCx::visible;
    let _f: fn(&PaintCx<'a>) -> Scale = PaintCx::scale;
    let _f: fn(&PaintCx<'a>, i32) -> i32 = PaintCx::px;
    let _f: for<'r> fn(&'r PaintCx<'a>) -> &'r Tokens = PaintCx::tokens;
    let _f: fn(&PaintCx<'a>) -> UiLang = PaintCx::lang;
    let _f: fn(&PaintCx<'a>, NodeId) -> bool = PaintCx::hot;
    let _f: fn(&PaintCx<'a>, NodeId) -> bool = PaintCx::pressed;
    let _f: fn(&PaintCx<'a>, NodeId) -> bool = PaintCx::focused;
    let _f: fn(&PaintCx<'a>) -> bool = PaintCx::focus_visible;
    let _f: fn(&PaintCx<'a>) -> bool = PaintCx::window_active;
    let _f: fn(&PaintCx<'a>, NodeId) -> f32 = PaintCx::hover_t;
    let _f: fn(&PaintCx<'a>, AnimId) -> Option<f32> = PaintCx::anim;
    let _f: fn(&PaintCx<'a>) -> u64 = PaintCx::now_ms;
    let _f: fn(&mut PaintCx<'a>, RectP, i32, Ink) = PaintCx::fill;
    let _f: fn(&mut PaintCx<'a>, RectP, i32, i32, Ink) = PaintCx::stroke;
    let _f: fn(&mut PaintCx<'a>, i32, i32, i32, Ink) = PaintCx::hline;
    let _f: fn(&mut PaintCx<'a>, i32, i32, i32, Ink) = PaintCx::vline;
    let _f: fn(&mut PaintCx<'a>, RectP, Glyph, Ink) = PaintCx::glyph;
    let _f: fn(&mut PaintCx<'a>, &gfx::image::IconBitmap, PointP, i32, u32) = PaintCx::image;
    let _f: fn(&mut PaintCx<'a>, &text::TextLayout, PointP, Ink) = PaintCx::text;
    let _f: fn(&mut PaintCx<'a>, &str, TypeRole, RectP, Align, Trim, Ink) -> i32 = PaintCx::text_in;
    let _f: fn(&mut PaintCx<'a>, &str, PointP, TypeRole, Ink, Ink, bool) -> i32 = PaintCx::digits;
    let _f: fn(&mut PaintCx<'a>, i32, &[(i16, i16)], Ink) = PaintCx::spans;
    let _f: fn(&mut PaintCx<'a>, RectP, i32, bool) = PaintCx::focus_ring;
    let _f: fn(&mut PaintCx<'a>, RectP, &mut dyn FnMut(&mut PaintCx<'_>)) = PaintCx::with_clip;
    let _f: fn(&EventCx<'a>) -> Option<NodeId> = EventCx::target;
    let _f: fn(&EventCx<'a>) -> Option<NodeId> = EventCx::focused;
    let _f: fn(&EventCx<'a>) -> Option<NodeId> = EventCx::hot;
    let _f: fn(&EventCx<'a>) -> bool = EventCx::is_focus_visible;
    let _f: for<'r> fn(&'r EventCx<'a>, NodeId) -> Option<&'r Node> = EventCx::node;
    let _f: fn(&mut EventCx<'a>) = EventCx::consume;
    let _f: fn(&EventCx<'a>) -> bool = EventCx::consumed;
    let _f: fn(&mut EventCx<'a>, RectP) = EventCx::damage;
    let _f: fn(&mut EventCx<'a>) = EventCx::redraw;
    let _f: fn(&mut EventCx<'a>) = EventCx::relayout;
    let _f: fn(&mut EventCx<'a>, NodeId) = EventCx::set_focus;
    let _f: fn(&mut EventCx<'a>) = EventCx::capture_pointer;
    let _f: fn(&mut EventCx<'a>) = EventCx::release_pointer;
    let _f: fn(&mut EventCx<'a>, AnimId, f32, u32, Ease) = EventCx::animate;
    let _f: fn(&mut EventCx<'a>, Vec<MenuItem>, Anchor) -> MenuId = EventCx::open_menu;
    let _f: fn(&mut EventCx<'a>, Vec<Text>, Anchor, Option<usize>) -> MenuId = EventCx::open_list;
    let _f: fn(&mut EventCx<'a>, MenuId, Vec<Text>, Option<usize>) = EventCx::update_list;
    let _f: fn(&mut EventCx<'a>) = EventCx::close_menus;
    let _f: fn(&mut EventCx<'a>, ToastSpec) = EventCx::toast;
    let _f: fn(&mut EventCx<'a>, TimerId, u32) = EventCx::set_timer;
    let _f: fn(&mut EventCx<'a>, TimerId) = EventCx::cancel_timer;
    let _f: fn(&mut EventCx<'a>, RectP, i32) = EventCx::scroll;
    let _f: fn(&mut EventCx<'a>, Option<RectP>) = EventCx::set_caret;
    let _f: for<'r> fn(&'r mut EventCx<'a>) -> &'r mut dyn Clipboard = EventCx::clipboard;
    let _f: fn(&mut EventCx<'a>) = EventCx::close_window;
    let _f: fn(&EventCx<'a>) -> u64 = EventCx::now_ms;
    let _f: fn(&EventCx<'a>) -> Scale = EventCx::scale;
    let _f: fn(&EventCx<'a>, i32) -> i32 = EventCx::px;
    let _f: for<'r> fn(&'r EventCx<'a>) -> &'r Tokens = EventCx::tokens;
    let _f: fn(&EventCx<'a>) -> UiLang = EventCx::lang;
    let _f: for<'r> fn(&'r mut EventCx<'a>) -> &'r mut dyn TextEngine = EventCx::text;
    let _f: fn(&mut LoopCx<'a>, WindowRef, AppMsg) = LoopCx::send;
    let _f: fn(&mut LoopCx<'a>, ThemeSpec) = LoopCx::set_theme;
    let _f: fn(&mut LoopCx<'a>, WindowSpec, Two) -> Result<WindowRef, UiError> = LoopCx::open::<Two>;
    let _f: fn(&mut LoopCx<'a>, i32) = LoopCx::quit;
}

/// A screen with the TK1 hooks (P3-tasks-app F-T3): it takes a copy-data payload, answers a routed message and asks
/// for the window requests on a click.
struct Front {
    cmds: CommandTable,
    got: Vec<(usize, Vec<u8>)>,
    routed: Vec<(u32, usize, isize)>,
    front: Option<FrontResult>,
}

impl Screen for Front {
    fn layout(&mut self, cx: &mut LayoutCx, avail: SizeP) -> SizeP {
        let one = Node::new(NodeId::new(WidgetId(1), 0), Role::Button, RectP::wh(0, 0, cx.px(80), cx.px(28)));
        cx.push(one.label(Cow::Borrowed("Front")).focus(Focusable::Tab).actions(Actions::CLICK));
        avail
    }

    fn paint(&self, cx: &mut PaintCx) {
        let _ = cx;
    }

    fn event(&mut self, cx: &mut EventCx, ev: &Event) -> Reply {
        if let Event::Click { .. } = ev {
            self.front = cx.front_result();
            cx.bring_to_front();
            cx.set_topmost(true);
        }
        Reply::None
    }

    fn copy_data(&mut self, cx: &mut EventCx, tag: usize, bytes: &[u8]) -> bool {
        let _ = cx;
        self.got.push((tag, bytes.to_vec()));
        tag == 1
    }

    fn app_message(&mut self, cx: &mut EventCx, msg: u32, wparam: usize, lparam: isize) -> Reply {
        let _ = cx;
        self.routed.push((msg, wparam, lparam));
        Reply::None
    }

    fn commands(&self) -> &CommandTable {
        &self.cmds
    }
}

#[test]
#[allow(clippy::type_complexity)]
fn kit1_window_items_are_usable() {
    use oled_ui::harness::WindowRequest;
    let front = Front { cmds: CommandTable::new(), got: Vec::new(), routed: Vec::new(), front: None };
    let mut h = Harness::new(front, SizeL { w: 200, h: 100 }, 96, Tokens::oled());
    assert!(h.copy_data(1, b"show") && !h.copy_data(2, b""), "the screen's answers");
    assert!(!h.copy_data(1, &vec![0u8; 64 * 1024 + 1]), "above 64 KiB: refused without a call");
    h.app_message(0xC123, 7, -1).click(PointL { x: 10, y: 10 }).click(PointL { x: 10, y: 10 });
    assert_eq!(h.screen().got, [(1, b"show".to_vec()), (2, Vec::new())]);
    assert_eq!(h.screen().routed, [(0xC123, 7, -1)]);
    assert_eq!(h.screen().front, Some(FrontResult::Foreground), "the outcome of the first click's request");
    let want = [WindowRequest::BringToFront, WindowRequest::Topmost(true)];
    assert_eq!(h.window_requests(), [want, want].concat());
    let _e = [FrontResult::Foreground, FrontResult::Topmost, FrontResult::None];
    let _f: fn(&mut Front, &mut EventCx<'_>, usize, &[u8]) -> bool = <Front as Screen>::copy_data;
    let _f: fn(&mut Front, &mut EventCx<'_>, u32, usize, isize) -> Reply = <Front as Screen>::app_message;
    let _f: fn(&mut Box<dyn Screen>, &mut EventCx<'_>, usize, &[u8]) -> bool = <Box<dyn Screen> as Screen>::copy_data;
    let _f: fn(&mut EventLoop, WindowRef, u32) -> bool = EventLoop::route_message;
    let _f: fn(&mut Harness<Front>, usize, &[u8]) -> bool = Harness::copy_data;
    let _f: for<'h> fn(&'h mut Harness<Front>, u32, usize, isize) -> &'h mut Harness<Front> = Harness::app_message;
    let _f: fn(&Harness<Front>) -> &[WindowRequest] = Harness::window_requests;
    kit1_cx_signatures();
    let mut lp = EventLoop::new(AppId("tasks"), Lang::En, ThemeSpec::oled()).expect("a loop opens no window by itself");
    assert!(!lp.route_message(WindowRef(1), 0xC123), "no such window");
    assert!(!lp.route_message(WindowRef(1), 0x000F), "WM_PAINT is never routable");
}

/// The TK1 methods of EventCx.
#[allow(clippy::extra_unused_lifetimes)]
fn kit1_cx_signatures<'a>() {
    let _f: fn(&mut EventCx<'a>) = EventCx::bring_to_front;
    let _f: fn(&mut EventCx<'a>, bool) = EventCx::set_topmost;
    let _f: fn(&EventCx<'a>) -> Option<FrontResult> = EventCx::front_result;
}

#[test]
fn kit_names_exist() {
    assert_eq!(UiLang::EN.kit(strings::k::KIT_APP_TASKS), "Tasks");
    assert_eq!(UiLang::IT.kit(strings::k::KIT_APP_TASKS), "Attività");
    assert_eq!(UiLang::EN.kit(strings::k::KIT_APP_FILES), "Folio");
    assert_eq!(UiLang::IT.kit(strings::k::KIT_APP_FILES), "Folio");
}
