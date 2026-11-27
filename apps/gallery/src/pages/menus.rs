//! The menus page (K2a.9): a sample row whose right click (or the Menu key, or Shift+F10) opens a context menu with
//! icons, chords from the page's command table, a checked item, a disabled item with its reason, a header, a submenu and
//! a danger item; the last pick shows under the row. Below it, the menus as their windows draw them
//! (Harness::menu_image of the same menus at the page's DPI, theme and language): the context menu, its submenu and a
//! list popup with a highlighted row.

use super::{s, Page};
use oled_ui::command::{Chord, Command as Cmd, CommandId, CommandTable};
use oled_ui::cx::{Anchor, EventCx, LayoutCx, MenuItem, PaintCx};
use oled_ui::event::{Event, MouseButton, Reply};
use oled_ui::geom::{PointL, PointP, RectP, SizeL, SizeP};
use oled_ui::gfx::image::IconBitmap;
use oled_ui::harness::{Harness, Image};
use oled_ui::model::{Align, FgRole};
use oled_ui::node::{Actions, Focusable, Node, NodeId, Role, Text, WidgetId};
use oled_ui::screen::{Screen, Widget};
use oled_ui::strings::{Lang, UiLang};
use oled_ui::text::Trim;
use oled_ui::theme::TypeRole;
use oled_ui::widgets::TextLabel;
use oled_ui::Glyph;

const SIZE: SizeL = SizeL { w: 860, h: 480 };

/// The sample row, logical px.
const ROW: (i32, i32, i32, i32) = (16, 40, 320, 32);
/// Where the pictures of the menus start, logical px.
const SHOTS_Y: i32 = 140;
/// The width of the field the list popup opens under, logical px.
const FIELD_W: i32 = 220;

const OPEN: CommandId = CommandId(1);
const CUT: CommandId = CommandId(2);
const COPY: CommandId = CommandId(3);
const PASTE: CommandId = CommandId(4);
const RENAME: CommandId = CommandId(5);
const HIDDEN: CommandId = CommandId(6);
const BY_NAME: CommandId = CommandId(10);
const BY_DATE: CommandId = CommandId(11);
const BY_SIZE: CommandId = CommandId(12);
const DELETE: CommandId = CommandId(20);

const ROW_ID: WidgetId = WidgetId(1);

/// The page's commands: what the menu's Command items take their label, glyph, chord and check state from.
fn commands(lang: UiLang) -> CommandTable {
    let mut t = CommandTable::new();
    let chord = |c: &str| Chord::parse(c).unwrap_or(Chord::new(oled_ui::event::Mods::NONE, oled_ui::event::Key::F2));
    let all = [
        Cmd::new(OPEN, s(lang, "Open", "Apri")).chord(chord("Enter")).glyph(Glyph::Document),
        Cmd::new(CUT, s(lang, "Cut", "Taglia")).chord(chord("Ctrl+X")).glyph(Glyph::Cut),
        Cmd::new(COPY, s(lang, "Copy", "Copia")).chord(chord("Ctrl+C")).glyph(Glyph::Copy),
        Cmd::new(RENAME, s(lang, "Rename", "Rinomina")).chord(chord("F2")).glyph(Glyph::Pencil),
        Cmd { checked: Some(true), ..Cmd::new(HIDDEN, s(lang, "Show hidden items", "Mostra elementi nascosti")) },
    ];
    for c in all {
        let _ = t.add(c);
    }
    t
}

fn item(id: CommandId, label: Text, glyph: Option<Glyph>, chord: Option<Text>, checked: Option<bool>) -> MenuItem {
    MenuItem::Custom { id, label, glyph, chord, enabled: true, checked, note: None, danger: false }
}

/// The Sort by submenu's items.
fn sort_items(lang: UiLang) -> Vec<MenuItem> {
    vec![
        item(BY_NAME, s(lang, "Name", "Nome"), None, None, Some(true)),
        item(BY_DATE, s(lang, "Date modified", "Ultima modifica"), None, None, Some(false)),
        item(BY_SIZE, s(lang, "Size", "Dimensione"), None, None, Some(false)),
    ]
}

/// The context menu of the sample row.
fn menu_items(lang: UiLang) -> Vec<MenuItem> {
    let chord = |c: &str| Chord::parse(c).map(|ch| Text::from(ch.label(lang)));
    vec![
        MenuItem::Command(OPEN),
        MenuItem::Separator,
        MenuItem::Command(CUT),
        MenuItem::Command(COPY),
        MenuItem::Custom {
            id: PASTE,
            label: s(lang, "Paste", "Incolla"),
            glyph: Some(Glyph::Paste),
            chord: chord("Ctrl+V"),
            enabled: false,
            checked: None,
            note: Some(s(lang, "The clipboard is empty", "Gli appunti sono vuoti")),
            danger: false,
        },
        MenuItem::Command(RENAME),
        MenuItem::Separator,
        MenuItem::Header(s(lang, "View", "Visualizza")),
        MenuItem::Command(HIDDEN),
        MenuItem::Submenu { label: s(lang, "Sort by", "Ordina per"), glyph: None, items: sort_items(lang) },
        MenuItem::Separator,
        MenuItem::Custom {
            id: DELETE,
            label: s(lang, "Delete permanently", "Elimina definitivamente"),
            glyph: Some(Glyph::Delete),
            chord: chord("Shift+Del"),
            enabled: true,
            checked: None,
            note: None,
            danger: true,
        },
    ]
}

/// The list popup's texts (a path field's completions).
fn list_texts(lang: UiLang) -> Vec<Text> {
    vec![
        s(lang, "Documents", "Documenti"),
        s(lang, "Downloads", "Download"),
        s(lang, "Desktop", "Desktop"),
        s(lang, "Pictures", "Immagini"),
    ]
}

/// What a picture shows.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Shot {
    Menu,
    Submenu,
    List,
}

/// The screen a picture is taken of: a right click opens its menu (or the list under a field).
struct Demo {
    lang: UiLang,
    shot: Shot,
    cmds: CommandTable,
}

impl Screen for Demo {
    fn layout(&mut self, cx: &mut LayoutCx, avail: SizeP) -> SizeP {
        let r = RectP::wh(0, 0, avail.w, avail.h);
        cx.push(Node::new(NodeId::new(WidgetId(1), 0), Role::Pane, r).label(Text::from("demo")));
        avail
    }

    fn paint(&self, cx: &mut PaintCx) {
        let _ = cx;
    }

    fn event(&mut self, cx: &mut EventCx, ev: &Event) -> Reply {
        if let Event::Click { button: MouseButton::Right, .. } = ev {
            let at = Anchor::Point(PointP { x: 0, y: 0 });
            match self.shot {
                Shot::Menu => {
                    cx.open_menu(menu_items(self.lang), at);
                }
                Shot::Submenu => {
                    cx.open_menu(sort_items(self.lang), at);
                }
                Shot::List => {
                    let field = RectP::wh(0, 0, cx.px(FIELD_W), cx.px(28));
                    cx.open_list(list_texts(self.lang), Anchor::Below(field), Some(1));
                }
            }
        }
        Reply::None
    }

    fn commands(&self) -> &CommandTable {
        &self.cmds
    }
}

/// A picture of a menu at a DPI, in a theme and a language: the image as the menu's window draws it, on a transparent
/// square (the square the canvas blits).
fn picture(lang: UiLang, shot: Shot, dpi: u32, tokens: &oled_ui::theme::Tokens) -> Option<(IconBitmap, SizeP)> {
    let demo = Demo { lang, shot, cmds: commands(lang) };
    let l = if lang.italian { Lang::It } else { Lang::En };
    let mut h = Harness::new(demo, SizeL { w: 40, h: 40 }, dpi, tokens.clone()).lang(l).pseudo(lang.pseudo);
    h.right_click(PointL { x: 10, y: 10 });
    let img = h.menu_image(0)?;
    Some((square(&img), SizeP { w: img.w as i32, h: img.h as i32 }))
}

/// An image on a transparent square of its larger side.
fn square(img: &Image) -> IconBitmap {
    let side = img.w.max(img.h);
    let mut b = IconBitmap::new(side, side);
    for y in 0..img.h as usize {
        let (from, to) = (y * img.w as usize, y * side as usize);
        b.pixels[to..to + img.w as usize].copy_from_slice(&img.px[from..from + img.w as usize]);
    }
    b
}

/// The pictures and what they were made with (DPI, token generation, language), each with its place and size.
type Shots = ((u32, u64, UiLang), Vec<(IconBitmap, PointP, SizeP)>);

struct Menus {
    lang: UiLang,
    heads: Vec<(TextLabel, (i32, i32, i32, i32))>,
    last: TextLabel,
    row: RectP,
    row_label: Text,
    shots: Option<Shots>,
    cmds: CommandTable,
}

impl Menus {
    fn row_id() -> NodeId {
        NodeId::new(ROW_ID, 0)
    }
}

impl Screen for Menus {
    fn layout(&mut self, cx: &mut LayoutCx, avail: SizeP) -> SizeP {
        for (l, (x, y, w, h)) in &mut self.heads {
            l.layout(cx, RectP::wh(cx.px(*x), cx.px(*y), cx.px(*w), cx.px(*h)));
        }
        let (x, y, w, h) = ROW;
        self.row = RectP::wh(cx.px(x), cx.px(y), cx.px(w), cx.px(h));
        let n = Node::new(Self::row_id(), Role::ListItem, self.row).label(self.row_label.clone());
        cx.push(n.focus(Focusable::Tab).actions(Actions::CONTEXT_MENU));
        self.last.layout(cx, RectP::wh(cx.px(16), cx.px(84), cx.px(600), cx.px(18)));
        let made = (cx.scale().dpi, cx.tokens().generation, cx.lang());
        if self.shots.as_ref().map(|s| s.0) != Some(made) {
            let tokens = cx.tokens().clone();
            let mut x = cx.px(16);
            let y = cx.px(SHOTS_Y);
            let mut v = Vec::new();
            for shot in [Shot::Menu, Shot::Submenu, Shot::List] {
                if let Some((b, size)) = picture(self.lang, shot, made.0, &tokens) {
                    v.push((b, PointP { x, y }, size));
                    x += size.w + cx.px(24);
                }
            }
            self.shots = Some((made, v));
        }
        avail
    }

    fn paint(&self, cx: &mut PaintCx) {
        for (l, _) in &self.heads {
            l.paint(cx);
        }
        let c = cx.tokens().color.clone();
        let radius = cx.px(cx.tokens().radius.s);
        let ink = if cx.hot(Self::row_id()) { c.fill_control_hot } else { c.fill_control };
        cx.fill(self.row, radius, ink);
        cx.stroke(self.row, radius, cx.px(1).max(1), c.rim_control);
        let g = RectP::wh(self.row.l + cx.px(12), self.row.t + (self.row.h() - cx.px(16)) / 2, cx.px(16), cx.px(16));
        cx.glyph(g, Glyph::Document, c.fg_secondary);
        let text = RectP { l: self.row.l + cx.px(36), r: self.row.r - cx.px(12), ..self.row };
        cx.text_in(&self.row_label, TypeRole::Body, text, Align::Start, Trim::End, c.fg_primary);
        cx.focus_ring(self.row, radius, false);
        self.last.paint(cx);
        if let Some((_, shots)) = &self.shots {
            for (b, at, _) in shots {
                cx.image(b, *at, b.width as i32, 255);
            }
        }
    }

    fn event(&mut self, cx: &mut EventCx, ev: &Event) -> Reply {
        let on_row = cx.target() == Some(Self::row_id());
        match ev {
            Event::Click { button: MouseButton::Right, pos, .. } if on_row => {
                cx.open_menu(menu_items(self.lang), Anchor::Point(*pos));
                Reply::None
            }
            Event::Command(id) => {
                let name = match self.cmds.get(*id) {
                    Some(c) => c.label.clone(),
                    None => match *id {
                        PASTE => s(self.lang, "Paste", "Incolla"),
                        BY_NAME => s(self.lang, "Name", "Nome"),
                        BY_DATE => s(self.lang, "Date modified", "Ultima modifica"),
                        BY_SIZE => s(self.lang, "Size", "Dimensione"),
                        _ => s(self.lang, "Delete permanently", "Elimina definitivamente"),
                    },
                };
                let line =
                    if self.lang.italian { format!("Ultima scelta: {name}") } else { format!("Last pick: {name}") };
                self.last.set_text(Text::from(line));
                cx.relayout();
                Reply::Redraw
            }
            Event::PointerMove { .. } | Event::PointerLeave => Reply::Damage(self.row.inset(-cx.px(4), -cx.px(4))),
            _ => Reply::None,
        }
    }

    fn commands(&self) -> &CommandTable {
        &self.cmds
    }
}

/// The page.
pub fn page(lang: UiLang) -> Option<Page> {
    let head =
        |id, text, y| (TextLabel::new(WidgetId(id), text, TypeRole::Section, FgRole::Secondary), (16, y, 820, 18));
    let screen = Menus {
        lang,
        heads: vec![
            head(
                10,
                s(lang, "Right-click the row, or press the Menu key", "Clic destro sulla riga, o il tasto Menu"),
                12,
            ),
            head(11, s(lang, "As the menu windows draw them", "Come le disegnano le finestre dei menu"), SHOTS_Y - 28),
        ],
        last: TextLabel::new(
            WidgetId(12),
            s(lang, "Nothing picked yet", "Ancora nessuna scelta"),
            TypeRole::Caption,
            FgRole::Tertiary,
        ),
        row: RectP::EMPTY,
        row_label: s(lang, "Quarterly report.pdf", "Relazione trimestrale.pdf"),
        shots: None,
        cmds: commands(lang),
    };
    Some(Page { name: "menus", size: SIZE, states: 1, screen: Box::new(screen) })
}

#[cfg(test)]
mod tests {
    use super::*;
    use oled_ui::theme::Tokens;

    #[test]
    fn the_page_is_named_in_both_languages() {
        for (lang, l) in [(UiLang::EN, Lang::En), (UiLang::IT, Lang::It)] {
            let p = page(lang).unwrap();
            let h = Harness::new(p.screen, p.size, 96, Tokens::oled()).lang(l);
            h.assert_named();
            assert!(h.a11y_dump().contains("ListItem"), "{}", h.a11y_dump());
        }
    }

    #[test]
    fn the_row_opens_its_menu_and_a_pick_shows() {
        let p = page(UiLang::EN).unwrap();
        let mut h = Harness::new(p.screen, p.size, 96, Tokens::oled());
        h.right_click(PointL { x: 100, y: 56 });
        assert_eq!(h.menus().len(), 1, "the context menu opened");
        let n = h.menus()[0].items.len();
        assert_eq!(n, 12);
        h.key("Down").key("Down").key("Down").key("Enter");
        assert!(h.menus().is_empty(), "the pick closed it");
        assert!(h.a11y_dump().contains("Last pick: Copy"), "{}", h.a11y_dump());
        // The Menu key opens it too.
        h.key("Tab").key("Apps");
        assert_eq!(h.menus().len(), 1);
        h.key("Esc");
        assert!(h.menus().is_empty());
    }

    #[test]
    fn the_pictures_are_the_menus_and_follow_the_dpi() {
        for dpi in [96, 144] {
            let p = page(UiLang::EN).unwrap();
            let mut h = Harness::new(p.screen, p.size, dpi, Tokens::oled());
            let img = h.render();
            let (b, size) = picture(UiLang::EN, Shot::Menu, dpi, &Tokens::oled()).unwrap();
            let at = (16 * dpi / 96, SHOTS_Y as u32 * dpi / 96);
            // The panel's first opaque pixel row: the picture is drawn where it was placed, at the page's scale.
            let mid = (size.h / 2) as u32;
            assert_eq!(
                img.px[((at.1 + mid) * img.w + at.0 + size.w as u32 / 2) as usize],
                b.pixels[(mid * b.width + size.w as u32 / 2) as usize]
            );
            assert!(size.w >= (200 * dpi / 96) as i32, "MENU_W_MIN at the scale");
        }
    }
}
