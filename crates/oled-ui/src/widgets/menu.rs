//! Menu, ContextMenu and the list popup (K2a.9, P2-kit 2.13): the open menus of one window, held by its Dispatcher and
//! shared by the host and the Harness. A menu is laid out here (rows, size, placement), driven by the keys the dispatcher
//! routes to it first and by the pointer on its popup (host::menu feeds the popup windows' input in), and drawn here; the
//! host shows each open level in a layered owned window, the Harness draws it on request (menu_image).
//!
//! Nothing is modal: the owner keeps the activation and the keyboard. Every menu ends with exactly one outcome for the
//! screen: Event::Command (a pick), Event::ListPicked (a list pick) or Event::MenuClosed (closed by the user without a
//! command: Esc, Alt, an outside click, the deactivation). A menu the screen closes itself (close_menus, or a new menu
//! opened over it) sends nothing back. The outcomes wait in the stack until the dispatcher delivers them
//! (Dispatcher::menu_events) right after the input that caused them.

use crate::command::{CommandId, CommandTable};
use crate::cx::{Anchor, Env, MenuId, MenuItem, PaintCx};
use crate::dispatch::Effect;
use crate::event::{Event, Key, Mods};
use crate::geom::{PointP, RectP, Scale, SizeP};
use crate::glyph::Glyph;
use crate::model::Align;
use crate::node::{NodeId, Text, WidgetId};
use crate::strings::UiLang;
use crate::text::{TextEngine, Trim};
use crate::theme::{Ink, Tokens, TypeRole};

/// The panel's padding above the first row and below the last one, logical px.
pub(crate) const PAD_Y: i32 = 4;
/// A row plate's inset from the panel's sides, logical px.
const PLATE_INSET: i32 = 4;
/// The glyph slot's left edge (S_4) and side (ICON_S), logical px.
const GLYPH_X: i32 = 12;
const GLYPH: i32 = 16;
/// The chord's distance from the panel's right edge, logical px.
const RIGHT: i32 = 12;
/// The room between a label and its chord, logical px.
const CHORD_GAP: i32 = 24;
/// A separator row: SEP_PAD, the 1 px hairline, SEP_PAD (logical px); the hairline runs from SEP_X to width - SEP_X.
const SEP_PAD: i32 = 6;
const SEP_X: i32 = 12;
/// The Caption line under a disabled item that carries its reason, logical px.
const NOTE_H: i32 = 16;
/// A list popup's text column (it has no glyph column), logical px.
const LIST_TEXT_X: i32 = 12;
/// The distance between an anchor rect and a menu below or above it, logical px.
pub(crate) const ANCHOR_GAP: i32 = 4;
/// Submenus nest at most this deep (the root counts).
const MAX_DEPTH: usize = 8;
/// The first id of the submenus the stack opens itself (the dispatcher's ids count up from 1).
const SUB_IDS: u32 = 0x8000_0000;
/// VK_MENU: Alt closes every menu.
const KEY_ALT: Key = Key(0x12);

/// A work area so large that nothing flips: the default placement before the host places a level on its monitor.
const NO_WORK: RectP = RectP { l: -(1 << 28), t: -(1 << 28), r: 1 << 28, b: 1 << 28 };

/// One row of a laid-out menu.
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum RowKind {
    Item {
        id: CommandId,
        label: Text,
        glyph: Option<Glyph>,
        chord: Option<Text>,
        enabled: bool,
        checked: Option<bool>,
        note: Option<Text>,
        danger: bool,
    },
    Submenu {
        label: Text,
        glyph: Option<Glyph>,
        items: Vec<MenuItem>,
        /// Laid out with its parent, so it opens at once from a key or the delay.
        sub: Box<Layout>,
    },
    Header(Text),
    Separator,
}

impl RowKind {
    /// Items and submenus take the highlight (disabled items too, so their reason can be read); headers and separators
    /// do not.
    fn selectable(&self) -> bool {
        matches!(self, RowKind::Item { .. } | RowKind::Submenu { .. })
    }

    fn label(&self) -> Option<&str> {
        match self {
            RowKind::Item { label, .. } | RowKind::Submenu { label, .. } => Some(label.as_ref()),
            RowKind::Header(_) | RowKind::Separator => None,
        }
    }
}

/// A row and its band in the panel (physical px from the panel's top).
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Row {
    pub(crate) kind: RowKind,
    pub(crate) top: i32,
    pub(crate) h: i32,
}

/// A laid-out menu: its rows, its size and the measures its painting uses (physical px).
#[derive(Clone, Debug, PartialEq, Default)]
pub(crate) struct Layout {
    pub(crate) rows: Vec<Row>,
    pub(crate) size: SizeP,
    /// The label column.
    pub(crate) text_x: i32,
    /// The panel's padding above the first row.
    pub(crate) pad: i32,
    pub(crate) list: bool,
}

impl Layout {
    /// The row at a point of the panel (panel coordinates).
    pub(crate) fn row_at(&self, p: PointP) -> Option<usize> {
        if p.x < 0 || p.x >= self.size.w {
            return None;
        }
        self.rows.iter().position(|r| p.y >= r.top && p.y < r.top + r.h)
    }

    fn selectable(&self, i: usize) -> bool {
        self.rows.get(i).is_some_and(|r| r.kind.selectable())
    }

    /// The next selectable row after `from` (before it when `back`), wrapping; the first (last) one from None.
    fn step(&self, from: Option<usize>, back: bool) -> Option<usize> {
        let n = self.rows.len();
        if n == 0 {
            return None;
        }
        let start = match (from, back) {
            (None, false) => n - 1,
            (None, true) => 0,
            (Some(i), _) => i.min(n - 1),
        };
        (1..=n).map(|k| if back { (start + n - k % n) % n } else { (start + k) % n }).find(|&i| self.selectable(i))
    }

    fn first(&self) -> Option<usize> {
        (0..self.rows.len()).find(|&i| self.selectable(i))
    }

    fn last(&self) -> Option<usize> {
        (0..self.rows.len()).rev().find(|&i| self.selectable(i))
    }
}

/// The scale, tokens and text engine one layout is measured with.
pub(crate) struct Measure<'a> {
    pub(crate) s: Scale,
    pub(crate) t: &'a Tokens,
    pub(crate) text: &'a mut dyn TextEngine,
}

impl Measure<'_> {
    fn width(&mut self, role: TypeRole, s: &str) -> i32 {
        let font = self.text.font(role, None, self.s, self.t.text_scale);
        self.text.width(s, font)
    }
}

/// The label, glyph, chord, enabled and checked of a MenuItem::Command from the screen's table, so a menu always shows
/// the real chord; None for an id the table does not have (the row is left out).
fn from_table(id: CommandId, cmds: &CommandTable, lang: UiLang) -> Option<RowKind> {
    let c = cmds.get(id)?;
    Some(RowKind::Item {
        id,
        label: c.label.clone(),
        glyph: c.glyph,
        chord: c.chord.map(|ch| Text::from(ch.label(lang))),
        enabled: c.enabled,
        checked: c.checked,
        note: None,
        danger: false,
    })
}

/// Lays a menu (or a list popup) out: its rows top to bottom and its size. Width = the widest of (label + chord + the text
/// column + 12 + 24), clamped to MENU_W_MIN..MENU_W_MAX, and at least `min_w` (a list under or over its field).
pub(crate) fn lay_out(
    items: &[MenuItem],
    list: bool,
    min_w: i32,
    cmds: &CommandTable,
    lang: UiLang,
    m: &mut Measure<'_>,
    depth: usize,
) -> Layout {
    let (s, t) = (m.s, m.t);
    let row_h = s.px(t.size.menu_row);
    let text_x = if list { s.px(LIST_TEXT_X) } else { s.px(t.size.menu_text_col) };
    let (right, gap) = (s.px(RIGHT), s.px(CHORD_GAP));
    let mut rows = Vec::with_capacity(items.len());
    let mut y = s.px(PAD_Y);
    let mut want = 0;
    for it in items {
        let kind = match it {
            MenuItem::Command(id) => match from_table(*id, cmds, lang) {
                Some(k) => k,
                None => continue,
            },
            MenuItem::Custom { id, label, glyph, chord, enabled, checked, note, danger } => RowKind::Item {
                id: *id,
                label: label.clone(),
                glyph: *glyph,
                chord: chord.clone(),
                enabled: *enabled,
                checked: *checked,
                note: note.clone(),
                danger: *danger,
            },
            MenuItem::Submenu { label, glyph, items } => {
                if depth + 1 >= MAX_DEPTH {
                    continue;
                }
                let sub = lay_out(items, false, 0, cmds, lang, m, depth + 1);
                RowKind::Submenu { label: label.clone(), glyph: *glyph, items: items.clone(), sub: Box::new(sub) }
            }
            MenuItem::Header(label) => RowKind::Header(label.clone()),
            MenuItem::Separator => RowKind::Separator,
        };
        let h = match &kind {
            RowKind::Item { label, chord, note, .. } => {
                let chord_w = chord.as_deref().map_or(0, |c| m.width(TypeRole::Body, c) + gap);
                want = want.max(m.width(TypeRole::Body, label) + chord_w + text_x + right);
                match note.as_deref() {
                    Some(n) => {
                        want = want.max(m.width(TypeRole::Caption, n) + text_x + right);
                        row_h + s.px(NOTE_H)
                    }
                    None => row_h,
                }
            }
            RowKind::Submenu { label, .. } => {
                want = want.max(m.width(TypeRole::Body, label) + s.px(GLYPH) + gap + text_x + right);
                row_h
            }
            RowKind::Header(label) => {
                want = want.max(m.width(TypeRole::Section, label) + s.px(GLYPH_X) + right);
                row_h
            }
            RowKind::Separator => 2 * s.px(SEP_PAD) + 1,
        };
        rows.push(Row { kind, top: y, h });
        y += h;
    }
    let w = want.clamp(s.px(t.size.menu_w_min), s.px(t.size.menu_w_max)).max(min_w);
    Layout { rows, size: SizeP { w, h: y + s.px(PAD_Y) }, text_x, pad: s.px(PAD_Y), list }
}

/// A list popup's texts as menu items (their index is their id), as the Harness keeps them.
pub(crate) fn list_items(items: &[Text]) -> Vec<MenuItem> {
    items
        .iter()
        .enumerate()
        .map(|(i, label)| MenuItem::Custom {
            id: CommandId(i as u32),
            label: label.clone(),
            glyph: None,
            chord: None,
            enabled: true,
            checked: None,
            note: None,
            danger: false,
        })
        .collect()
}

/// True for items shaped as list_items makes them: how Harness::menu_image tells a list's snapshot from a menu's.
pub(crate) fn is_list(items: &[MenuItem]) -> bool {
    !items.is_empty()
        && items.iter().enumerate().all(|(i, it)| match it {
            MenuItem::Custom {
                id, glyph: None, chord: None, enabled: true, checked: None, note: None, danger, ..
            } => id.0 == i as u32 && !danger,
            _ => false,
        })
}

/// The narrowest a popup may be: a list is as wide as the field it opens under (or over).
pub(crate) fn min_width(list: bool, at: Anchor) -> i32 {
    match at {
        Anchor::Below(r) | Anchor::Above(r) if list => r.w().max(0),
        _ => 0,
    }
}

/// Where a popup of `size` goes for an anchor, inside `work` (one coordinate space for all three). Point: its top-left
/// at the point, flipped left and/or up to stay inside; Below and Above: under or over the rect (`gap` away), left-aligned,
/// flipped to the other side, and right-aligned with the rect when there is no room on the right; Right (a submenu):
/// beside the parent row with its first row level with it (`pad` up), flipped left. Then clamped inside the work area.
pub(crate) fn place(at: Anchor, size: SizeP, work: RectP, gap: i32, pad: i32) -> PointP {
    let (w, h) = (size.w, size.h);
    let clamp = |x: i32, y: i32| PointP { x: x.min(work.r - w).max(work.l), y: y.min(work.b - h).max(work.t) };
    let side = |r: RectP| if r.l + w > work.r { r.r - w } else { r.l };
    match at {
        Anchor::Point(p) => {
            let x = if p.x + w > work.r { p.x - w } else { p.x };
            let y = if p.y + h > work.b { p.y - h } else { p.y };
            clamp(x, y)
        }
        Anchor::Below(r) => {
            let below = r.b + gap;
            let y = if below + h > work.b && r.t - gap - h >= work.t { r.t - gap - h } else { below };
            clamp(side(r), y)
        }
        Anchor::Above(r) => {
            let above = r.t - gap - h;
            let y = if above < work.t && r.b + gap + h <= work.b { r.b + gap } else { above };
            clamp(side(r), y)
        }
        Anchor::Right(r) => {
            let x = if r.r + w > work.r { r.l - w } else { r.r };
            clamp(x, r.t - pad)
        }
    }
}

/// An anchor moved by (dx, dy) (client to screen coordinates and back).
pub(crate) fn shift(at: Anchor, dx: i32, dy: i32) -> Anchor {
    match at {
        Anchor::Point(p) => Anchor::Point(PointP { x: p.x + dx, y: p.y + dy }),
        Anchor::Below(r) => Anchor::Below(r.offset(dx, dy)),
        Anchor::Above(r) => Anchor::Above(r.offset(dx, dy)),
        Anchor::Right(r) => Anchor::Right(r.offset(dx, dy)),
    }
}

/// What a layout was made with: the DPI, the token generation and the language.
type Made = (u32, u64, UiLang);

/// One open menu level or list popup.
#[derive(Clone, Debug)]
pub(crate) struct Level {
    pub(crate) id: MenuId,
    pub(crate) list: bool,
    /// What the screen gave (a list's texts as list_items).
    pub(crate) items: Vec<MenuItem>,
    /// Client coordinates of the owner.
    pub(crate) anchor: Anchor,
    /// The node that was the target when it opened: its outcomes go there.
    pub(crate) opener: Option<NodeId>,
    /// None until the next resolve (MenuItem::Command needs the screen's table).
    pub(crate) layout: Option<Layout>,
    made: Option<Made>,
    /// Its panel in the owner's client coordinates: the default placement until the host placed it on its monitor.
    pub(crate) rect: RectP,
    /// The host placed it (the screen position and the work area are known).
    pub(crate) placed: bool,
    /// Menus: the hovered or keyboard row; lists: the caller's highlight.
    pub(crate) highlight: Option<usize>,
    /// Lists: the row under the pointer.
    pub(crate) hover: Option<usize>,
    pub(crate) pressed: Option<usize>,
    /// Menus: the row whose submenu is the next level.
    pub(crate) child: Option<usize>,
}

impl Level {
    fn new(id: MenuId, list: bool, items: Vec<MenuItem>, anchor: Anchor, opener: Option<NodeId>) -> Level {
        Level {
            id,
            list,
            items,
            anchor,
            opener,
            layout: None,
            made: None,
            rect: RectP::EMPTY,
            placed: false,
            highlight: None,
            hover: None,
            pressed: None,
            child: None,
        }
    }

    fn rows(&self) -> &[Row] {
        self.layout.as_ref().map_or(&[], |l| &l.rows)
    }

    /// The panel-wide band of row `i`, client coordinates.
    pub(crate) fn row_rect(&self, i: usize) -> Option<RectP> {
        let r = self.rows().get(i)?;
        Some(RectP { l: self.rect.l, t: self.rect.t + r.top, r: self.rect.r, b: self.rect.t + r.top + r.h })
    }

    /// The row at a point of the owner's client.
    fn row_at_client(&self, p: PointP) -> Option<usize> {
        self.layout.as_ref()?.row_at(PointP { x: p.x - self.rect.l, y: p.y - self.rect.t })
    }

    /// The default placement: at the anchor, nothing flipped.
    fn default_rect(&mut self, s: Scale) {
        let (size, pad) = self.layout.as_ref().map_or((SizeP::default(), 0), |l| (l.size, l.pad));
        let at = place(self.anchor, size, NO_WORK, s.px(ANCHOR_GAP), pad);
        self.rect = RectP::wh(at.x, at.y, size.w, size.h);
    }
}

/// A submenu waiting for the delay: at `due` the levels above `menu` close and `row`'s submenu (if it is one) opens.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Pending {
    menu: MenuId,
    row: usize,
    due: u64,
}

/// The open menus of a window, held by the Dispatcher.
#[derive(Default)]
pub(crate) struct MenuStack {
    /// The menu levels, the root first and the innermost submenu last.
    pub(crate) menus: Vec<Level>,
    /// The list popups (the keys stay with their caller).
    pub(crate) lists: Vec<Level>,
    /// The outcomes for the screen, with their target, in order.
    events: Vec<(Option<NodeId>, Event)>,
    pending: Option<Pending>,
    /// Bumped by every change the popups show.
    pub(crate) rev: u64,
    sub_ids: u32,
    /// The popup windows of the host (none in the Harness).
    pub(crate) windows: crate::host::menu::Windows,
}

impl MenuStack {
    /// A change the popups show: the host's windows follow (host::menu).
    fn touch(&mut self) {
        self.rev = self.rev.wrapping_add(1);
        self.windows.poke();
    }

    /// The level with this id (a menu level or a list).
    pub(crate) fn level(&self, id: MenuId) -> Option<&Level> {
        self.menus.iter().chain(&self.lists).find(|l| l.id == id)
    }

    fn level_mut(&mut self, id: MenuId) -> Option<&mut Level> {
        self.menus.iter_mut().chain(self.lists.iter_mut()).find(|l| l.id == id)
    }

    fn next_sub_id(&mut self) -> MenuId {
        self.sub_ids = (self.sub_ids.wrapping_add(1) & 0x7FFF_FFFF).max(1);
        MenuId(SUB_IDS | self.sub_ids)
    }

    /// The menus closed by the user: MenuClosed for the root; CloseMenus once nothing is open.
    fn close_menus(&mut self, fx: &mut Vec<Effect>) {
        if let Some(root) = self.menus.first() {
            self.events.push((root.opener, Event::MenuClosed(root.id)));
        }
        self.drop_menus(fx);
    }

    /// The menus go without an outcome of their own (a pick sent its command).
    fn drop_menus(&mut self, fx: &mut Vec<Effect>) {
        if self.menus.is_empty() {
            return;
        }
        self.menus.clear();
        self.pending = None;
        self.touch();
        if self.lists.is_empty() {
            fx.push(Effect::CloseMenus);
        }
    }

    /// The innermost level closes: a submenu hands the highlight back to its row; the root sends MenuClosed.
    fn close_level(&mut self, fx: &mut Vec<Effect>) {
        if self.menus.len() <= 1 {
            self.close_menus(fx);
            return;
        }
        self.menus.pop();
        self.pending = None;
        if let Some(parent) = self.menus.last_mut() {
            parent.highlight = parent.child.take();
        }
        self.touch();
    }

    /// The levels above `depth` close (the menu at index depth - 1 stays).
    fn truncate(&mut self, depth: usize) {
        if self.menus.len() > depth {
            self.menus.truncate(depth);
            if let Some(m) = self.menus.last_mut() {
                m.child = None;
            }
            self.touch();
        }
    }

    /// Opens the submenu of row `row` of the menu at index `at`; from the keyboard its first row takes the highlight.
    fn open_sub(&mut self, at: usize, row: usize, keyboard: bool) {
        if at + 1 >= MAX_DEPTH {
            return;
        }
        let Some(parent) = self.menus.get(at) else { return };
        let Some(Row { kind: RowKind::Submenu { items, sub, .. }, .. }) = parent.rows().get(row) else { return };
        let (items, sub) = (items.clone(), (**sub).clone());
        let (made, opener) = (parent.made, parent.opener);
        let Some(anchor) = parent.row_rect(row).map(Anchor::Right) else { return };
        self.truncate(at + 1);
        let id = self.next_sub_id();
        let mut level = Level::new(id, false, items, anchor, opener);
        level.highlight = if keyboard { sub.first() } else { None };
        level.layout = Some(sub);
        level.made = made;
        level.default_rect(Scale { dpi: made.map_or(96, |m| m.0) });
        if let Some(p) = self.menus.get_mut(at) {
            p.child = Some(row);
            p.highlight = Some(row);
        }
        self.pending = None;
        self.menus.push(level);
        self.touch();
    }

    /// Row `row` of `menu` is taken (Enter, a click): an enabled item sends its command and every menu closes, a
    /// submenu opens, anything else does nothing. A list row sends ListPicked and the list closes.
    fn activate(&mut self, menu: MenuId, row: usize, keyboard: bool, fx: &mut Vec<Effect>) {
        if let Some(i) = self.lists.iter().position(|l| l.id == menu) {
            if self.lists[i].rows().get(row).is_some() {
                let l = self.lists.remove(i);
                self.events.push((l.opener, Event::ListPicked { menu, index: row }));
                self.touch();
                if self.lists.is_empty() && self.menus.is_empty() {
                    fx.push(Effect::CloseMenus);
                }
            }
            return;
        }
        let Some(at) = self.menus.iter().position(|l| l.id == menu) else { return };
        let kind = self.menus[at].rows().get(row).map(|r| r.kind.clone());
        match kind {
            Some(RowKind::Item { id, enabled: true, .. }) => {
                // A kit id goes to its owner's node 0 (the node that opened the menu, event.rs's delivery rule).
                let target = if id.is_kit() {
                    Some(NodeId::new(WidgetId((id.0 >> 16) & 0x7FFF), 0))
                } else {
                    self.menus.first().and_then(|r| r.opener)
                };
                self.events.push((target, Event::Command(id)));
                self.drop_menus(fx);
            }
            Some(RowKind::Submenu { .. }) => self.open_sub(at, row, keyboard),
            _ => {}
        }
    }
}

/// EventCx::open_menu: a new root menu. Menus and lists already open close without an outcome (the screen opened it).
pub(crate) fn opened(stack: &mut MenuStack, id: MenuId, items: Vec<MenuItem>, at: Anchor, opener: Option<NodeId>) {
    stack.menus.clear();
    stack.lists.clear();
    stack.pending = None;
    stack.menus.push(Level::new(id, false, items, at, opener));
    stack.touch();
}

/// EventCx::open_list: a list popup (one at a time: another list closes without an outcome).
pub(crate) fn list_opened(
    stack: &mut MenuStack,
    id: MenuId,
    items: &[Text],
    at: Anchor,
    highlight: Option<usize>,
    opener: Option<NodeId>,
) {
    stack.lists.clear();
    let mut l = Level::new(id, true, list_items(items), at, opener);
    l.highlight = highlight;
    stack.lists.push(l);
    stack.touch();
}

/// EventCx::update_list: new texts and highlight for an open list (laid out again at the next resolve when the texts
/// changed).
pub(crate) fn list_updated(stack: &mut MenuStack, id: MenuId, items: &[Text], highlight: Option<usize>) {
    let Some(l) = stack.lists.iter_mut().find(|l| l.id == id) else { return };
    let items = list_items(items);
    if l.items != items {
        l.items = items;
        l.layout = None;
        l.made = None;
        l.hover = None;
        l.pressed = None;
    }
    l.highlight = highlight;
    stack.touch();
}

/// EventCx::close_menus: every menu and list closes without an outcome.
pub(crate) fn closed_by_screen(stack: &mut MenuStack) {
    if stack.menus.is_empty() && stack.lists.is_empty() {
        return;
    }
    stack.menus.clear();
    stack.lists.clear();
    stack.pending = None;
    stack.touch();
}

/// Lays out, with the screen's table, the levels the screen opened (or that a new DPI, theme or language made stale) and
/// gives each its default placement; the host places them on their monitor afterwards. A submenu is laid out with its
/// parent: a root laid out again closes them.
pub(crate) fn resolve(stack: &mut MenuStack, cmds: &CommandTable, env: &Env<'_>, text: &mut dyn TextEngine) {
    let made = (env.scale.dpi, env.tokens.generation, env.lang);
    let stale = |l: &Level| l.layout.is_none() || l.made != Some(made);
    if !stack.menus.first().is_some_and(stale) && !stack.lists.iter().any(stale) {
        return;
    }
    let mut m = Measure { s: env.scale, t: env.tokens, text };
    let mut root = false;
    let MenuStack { menus, lists, .. } = &mut *stack;
    for l in menus.iter_mut().take(1).chain(lists.iter_mut()) {
        if !stale(l) {
            continue;
        }
        root |= !l.list;
        let layout = lay_out(&l.items, l.list, min_width(l.list, l.anchor), cmds, env.lang, &mut m, 0);
        l.highlight = l.highlight.filter(|h| *h < layout.rows.len());
        l.hover = None;
        l.pressed = None;
        l.layout = Some(layout);
        l.made = Some(made);
        l.placed = false;
        l.default_rect(env.scale);
    }
    if root && stack.menus.len() > 1 {
        stack.menus.truncate(1);
        stack.menus[0].child = None;
    }
    stack.touch();
}

/// True while a menu (not a list popup) is open.
pub(crate) fn is_open(stack: &MenuStack) -> bool {
    !stack.menus.is_empty()
}

/// What a press does while menus are open.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum MenuPress {
    /// Delivered to the screen as usual.
    Pass,
    /// Taken by the menus (an outside click closes them): not delivered.
    Swallow,
}

/// A press anywhere in the owner. In a window the popups take their own presses, so a press here is outside every
/// menu: the menus close and the press is not delivered (list popups stay: their caller closes them). The Harness has
/// no popup windows: a press inside a level's rect there is that level's click.
pub(crate) fn on_pointer_down(stack: &mut MenuStack, p: PointP, fx: &mut Vec<Effect>) -> MenuPress {
    let inside = stack
        .menus
        .iter()
        .rev()
        .chain(stack.lists.iter())
        .find(|l| l.rect.contains(p))
        .map(|l| (l.id, l.row_at_client(p)));
    if let Some((id, row)) = inside {
        if let Some(row) = row {
            stack.activate(id, row, false, fx);
        }
        return MenuPress::Swallow;
    }
    if stack.menus.is_empty() {
        return MenuPress::Pass;
    }
    stack.close_menus(fx);
    MenuPress::Swallow
}

/// A key while a menu is open; true when the menus took it (all keys: the open menu has the keyboard). Up and Down move
/// over the items (wrapping, past headers and separators; disabled items are reached so their reason can be read), Home
/// and End go to the ends, Right or Enter open a submenu, Left closes one, Esc closes one level, Alt closes all, a
/// letter jumps to the next item whose label starts with it, Enter (or Space) on an enabled item sends its command and
/// closes all.
pub(crate) fn on_key(stack: &mut MenuStack, key: Key, mods: Mods, down: bool, fx: &mut Vec<Effect>) -> bool {
    if stack.menus.is_empty() {
        return false;
    }
    if !down {
        return true;
    }
    let top = stack.menus.len() - 1;
    let (id, hl) = (stack.menus[top].id, stack.menus[top].highlight);
    let layout = stack.menus[top].layout.clone().unwrap_or_default();
    let set = |stack: &mut MenuStack, to: Option<usize>| {
        if to.is_some() && to != stack.menus[top].highlight {
            stack.menus[top].highlight = to;
            stack.pending = None;
            stack.touch();
        }
    };
    match key {
        Key::ESC => stack.close_level(fx),
        KEY_ALT => stack.close_menus(fx),
        Key::DOWN => set(stack, layout.step(hl, false)),
        Key::UP => set(stack, layout.step(hl, true)),
        Key::HOME | Key::PAGE_UP => set(stack, layout.first()),
        Key::END | Key::PAGE_DOWN => set(stack, layout.last()),
        Key::RIGHT => {
            if let Some(h) =
                hl.filter(|h| matches!(layout.rows.get(*h).map(|r| &r.kind), Some(RowKind::Submenu { .. })))
            {
                stack.open_sub(top, h, true);
            }
        }
        Key::LEFT => {
            if top > 0 {
                stack.close_level(fx);
            }
        }
        Key::ENTER | Key::SPACE => {
            if let Some(h) = hl {
                stack.activate(id, h, true, fx);
            }
        }
        k if !mods.ctrl() && !mods.alt() && !mods.win() => {
            if let Some(c) = key_char(k) {
                set(stack, jump(&layout, hl, c));
            }
        }
        _ => {}
    }
    true
}

/// The next item after `from` (wrapping) whose label starts with `c`, in any case.
fn jump(layout: &Layout, from: Option<usize>, c: char) -> Option<usize> {
    let n = layout.rows.len();
    if n == 0 {
        return None;
    }
    let start = from.unwrap_or(n - 1);
    (1..=n).map(|d| (start + d) % n).find(|&i| {
        layout.rows[i]
            .kind
            .label()
            .and_then(|l| l.chars().next())
            .is_some_and(|f| f.to_lowercase().eq(c.to_lowercase()))
    })
}

/// The character of a letter or digit key.
fn key_char(k: Key) -> Option<char> {
    match k.0 {
        0x30..=0x39 | 0x41..=0x5A => char::from_u32(u32::from(k.0)),
        _ => None,
    }
}

/// Every menu and list closes (deactivation): MenuClosed for the root menu and for each list.
pub(crate) fn close_all(stack: &mut MenuStack, fx: &mut Vec<Effect>) {
    if stack.menus.is_empty() && stack.lists.is_empty() {
        return;
    }
    if let Some(root) = stack.menus.first() {
        stack.events.push((root.opener, Event::MenuClosed(root.id)));
    }
    for l in &stack.lists {
        stack.events.push((l.opener, Event::MenuClosed(l.id)));
    }
    stack.menus.clear();
    stack.lists.clear();
    stack.pending = None;
    stack.touch();
    fx.push(Effect::CloseMenus);
}

/// The submenu delay: the levels above the hovered menu close and the hovered row's submenu opens.
pub(crate) fn on_due(stack: &mut MenuStack, now_ms: u64, fx: &mut Vec<Effect>) {
    let _ = fx;
    let Some(p) = stack.pending.filter(|p| p.due <= now_ms) else { return };
    stack.pending = None;
    let Some(at) = stack.menus.iter().position(|l| l.id == p.menu) else { return };
    stack.truncate(at + 1);
    if matches!(stack.menus[at].rows().get(p.row).map(|r| &r.kind), Some(RowKind::Submenu { .. })) {
        stack.open_sub(at, p.row, false);
    }
}

/// The next submenu deadline.
pub(crate) fn next_due(stack: &MenuStack) -> Option<u64> {
    stack.pending.map(|p| p.due)
}

/// The outcomes waiting for the screen, with their target, in order.
pub(crate) fn take_events(stack: &mut MenuStack) -> Vec<(Option<NodeId>, Event)> {
    std::mem::take(&mut stack.events)
}

/// The pointer over row `row` of a popup (None: off its rows, or it left): a menu highlights the row (the row of an
/// open submenu keeps it otherwise) and arms the submenu delay (`delay` ms; 0 acts at once) to open the row's submenu
/// or to close a submenu of another row; the pointer inside a submenu cancels its parents' delay. A list only shows the
/// row under the pointer.
pub(crate) fn pointer_move(stack: &mut MenuStack, menu: MenuId, row: Option<usize>, now_ms: u64, delay: u32) {
    if let Some(l) = stack.lists.iter_mut().find(|l| l.id == menu) {
        if l.hover != row {
            l.hover = row;
            stack.touch();
        }
        return;
    }
    let Some(at) = stack.menus.iter().position(|l| l.id == menu) else { return };
    let row = row.filter(|r| stack.menus[at].layout.as_ref().is_some_and(|l| l.selectable(*r)));
    let child = stack.menus[at].child;
    let lit = row.or(child);
    if stack.menus[at].highlight != lit {
        stack.menus[at].highlight = lit;
        stack.touch();
    }
    let parents_wait =
        stack.pending.and_then(|p| stack.menus.iter().position(|l| l.id == p.menu)).is_some_and(|i| i < at);
    if parents_wait {
        stack.pending = None;
    }
    let sub = |stack: &MenuStack, r: usize| {
        matches!(stack.menus[at].rows().get(r).map(|x| &x.kind), Some(RowKind::Submenu { .. }))
    };
    match row {
        Some(r) if child != Some(r) && (sub(stack, r) || child.is_some()) => {
            if stack.pending.map(|p| (p.menu, p.row)) != Some((menu, r)) {
                stack.pending = Some(Pending { menu, row: r, due: now_ms + u64::from(delay) });
            }
            if delay == 0 {
                on_due(stack, now_ms, &mut Vec::new());
            }
        }
        _ => {
            if stack.pending.is_some_and(|p| p.menu == menu) {
                stack.pending = None;
            }
        }
    }
}

/// A button went down over row `row` of a popup: the row shows pressed.
pub(crate) fn pointer_press(stack: &mut MenuStack, menu: MenuId, row: Option<usize>) {
    let Some(l) = stack.level_mut(menu) else { return };
    let row = row.filter(|r| l.layout.as_ref().is_some_and(|x| x.selectable(*r) || x.list));
    if l.pressed != row {
        l.pressed = row;
        stack.touch();
    }
}

/// A button came up over row `row` of a popup: the row is taken (a submenu opens at once).
pub(crate) fn pointer_release(stack: &mut MenuStack, menu: MenuId, row: Option<usize>, fx: &mut Vec<Effect>) {
    if stack.level_mut(menu).and_then(|l| l.pressed.take()).is_some() {
        stack.touch();
    }
    if let Some(r) = row {
        stack.activate(menu, r, false, fx);
    }
}

/// Draws a laid-out panel with its top-left at `at`: the R_M panel in the opaque window colour with a 1 px RIM_SURFACE
/// rim; the highlight (FILL_HOVER; FILL_PRESS while pressed) on a plate inset 4 px at R_S; per row the glyph (or the
/// check) in the 16 px slot at 12, the label at the text column in Body, the chord right-aligned 12 px from the edge in
/// FG_TERTIARY, a submenu's chevron, a disabled item's reason as a Caption line under it, headers in Section style,
/// separators as a hairline from 12 to width - 12. Disabled rows are FG_DISABLED, danger rows ACCENT_DANGER.
pub(crate) fn paint_panel(cx: &mut PaintCx, l: &Layout, at: PointP, highlight: Option<usize>, pressed: Option<usize>) {
    let t = cx.tokens().clone();
    let c = &t.color;
    let s = cx.scale();
    let panel = RectP::wh(at.x, at.y, l.size.w, l.size.h);
    if !cx.visible(panel) {
        return;
    }
    let radius = s.px(t.radius.m);
    cx.fill(panel, radius, c.bg.with_a(255));
    cx.stroke(panel, radius, s.px(1).max(1), c.rim_surface);
    let row_h = s.px(t.size.menu_row);
    let (inset, plate_r) = (s.px(PLATE_INSET), s.px(t.radius.s));
    let (right, glyph) = (s.px(RIGHT), s.px(GLYPH));
    for (i, row) in l.rows.iter().enumerate() {
        let band = RectP::wh(panel.l, panel.t + row.top, panel.w(), row.h);
        if !cx.visible(band) {
            continue;
        }
        let selectable = row.kind.selectable();
        let lit = highlight == Some(i) && selectable;
        let down = pressed == Some(i) && selectable;
        if lit || down {
            let plate = RectP { l: band.l + inset, r: band.r - inset, ..band };
            cx.fill(plate, plate_r, if down { c.fill_press } else { c.fill_hover });
        }
        let line = RectP { b: band.t + row_h, ..band };
        // In high contrast a lit row is the highlight colour: its text takes the highlight text colour.
        let on = |ink: Ink| if t.high_contrast && (lit || down) { c.fg_hot } else { ink };
        let slot = RectP::wh(band.l + s.px(GLYPH_X), line.t + (row_h - glyph) / 2, glyph, glyph);
        match &row.kind {
            RowKind::Item { label, glyph: g, chord, enabled, checked, note, danger, .. } => {
                let ink = match (*enabled, *danger) {
                    (false, _) => c.fg_disabled,
                    (true, true) => on(c.accent_danger),
                    (true, false) => on(c.fg_primary),
                };
                let mark = if *checked == Some(true) { Some(Glyph::Check) } else { *g };
                if let Some(g) = mark.filter(|_| !l.list) {
                    cx.glyph(slot, g, ink);
                }
                let mut text_r = RectP { l: band.l + l.text_x, r: band.r - right, ..line };
                if let Some(ch) = chord.as_deref() {
                    let ink = if *enabled { on(c.fg_tertiary) } else { c.fg_disabled };
                    let w = cx.text_in(ch, TypeRole::Body, text_r, Align::End, Trim::None, ink);
                    text_r.r -= w + s.px(CHORD_GAP);
                }
                cx.text_in(label, TypeRole::Body, text_r, Align::Start, Trim::End, ink);
                if let Some(n) = note.as_deref() {
                    let top = line.b - s.px(PAD_Y);
                    let r = RectP { l: text_r.l, t: top, r: band.r - right, b: top + s.px(NOTE_H) };
                    cx.text_in(n, TypeRole::Caption, r, Align::Start, Trim::End, on(c.fg_tertiary));
                }
            }
            RowKind::Submenu { label, glyph: g, .. } => {
                let ink = on(c.fg_primary);
                if let Some(g) = g {
                    cx.glyph(slot, *g, ink);
                }
                let chev = RectP::wh(band.r - s.px(8) - glyph, line.t + (row_h - glyph) / 2, glyph, glyph);
                cx.glyph(chev, Glyph::ChevronRight, on(c.fg_secondary));
                let text_r = RectP { l: band.l + l.text_x, r: chev.l - s.px(8), ..line };
                cx.text_in(label, TypeRole::Body, text_r, Align::Start, Trim::End, ink);
            }
            RowKind::Header(label) => {
                let r = RectP { l: band.l + s.px(GLYPH_X), r: band.r - right, ..line };
                cx.text_in(label, TypeRole::Section, r, Align::Start, Trim::End, c.fg_secondary);
            }
            RowKind::Separator => {
                cx.hline(band.l + s.px(SEP_X), band.r - s.px(SEP_X), band.t + s.px(SEP_PAD), c.separator);
            }
        }
    }
}

/// A level's panel as its window shows it: a menu's highlight and pressed row, a list's pointer row over its caller's
/// highlight.
pub(crate) fn paint_level(cx: &mut PaintCx, l: &Level, at: PointP) {
    let Some(layout) = l.layout.as_ref() else { return };
    let highlight = if l.list { l.hover.or(l.highlight) } else { l.highlight };
    paint_panel(cx, layout, at, highlight, l.pressed);
}

/// The layout of a snapshot's items (Harness::menu_image), made as the stack makes it.
pub(crate) fn lay_out_items(
    items: &[MenuItem],
    at: Anchor,
    cmds: &CommandTable,
    env: &Env<'_>,
    text: &mut dyn TextEngine,
) -> Layout {
    let list = is_list(items);
    let mut m = Measure { s: env.scale, t: env.tokens, text };
    lay_out(items, list, min_width(list, at), cmds, env.lang, &mut m, 0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::command::{Chord, Command as Cmd};
    use crate::cx::{EventCx, LayoutCx};
    use crate::event::{MouseButton, Reply};
    use crate::geom::{PointL, SizeL};
    use crate::harness::Harness;
    use crate::node::{Focusable, Node, Role};
    use crate::screen::Screen;
    use crate::strings::Lang;
    use crate::theme::Tokens;

    const ROW: WidgetId = WidgetId(1);
    const OTHER: WidgetId = WidgetId(2);
    const CUT: CommandId = CommandId(10);
    const COPY: CommandId = CommandId(11);
    const PASTE: CommandId = CommandId(12);
    const CLOSE: CommandId = CommandId(13);
    const NAME: CommandId = CommandId(20);
    const DATE: CommandId = CommandId(21);
    const DELETE: CommandId = CommandId(30);

    fn custom(id: CommandId, label: &'static str, enabled: bool) -> MenuItem {
        MenuItem::Custom {
            id,
            label: Text::from(label),
            glyph: None,
            chord: None,
            enabled,
            checked: None,
            note: (!enabled).then(|| Text::from("Nothing to paste")),
            danger: false,
        }
    }

    /// Cut, Copy (from the table), a separator, a header, Paste (disabled), Sort by (a submenu), Close, Delete.
    fn items() -> Vec<MenuItem> {
        vec![
            MenuItem::Command(CUT),
            MenuItem::Command(COPY),
            MenuItem::Separator,
            MenuItem::Header(Text::from("Clipboard")),
            custom(PASTE, "Paste", false),
            MenuItem::Submenu {
                label: Text::from("Sort by"),
                glyph: None,
                items: vec![custom(NAME, "Name", true), custom(DATE, "Date", true)],
            },
            custom(CLOSE, "Close", true),
            custom(DELETE, "Delete", true),
        ]
    }

    /// A row at (10, 10) that opens items() on a right click (or the list on Ctrl+L, its highlight moved by Down and
    /// Up), another node at (10, 200), and a log of what reached the screen.
    struct Sc {
        cmds: CommandTable,
        log: Vec<String>,
        list: Option<MenuId>,
        hl: usize,
    }

    impl Sc {
        fn new() -> Sc {
            let mut cmds = CommandTable::new();
            cmds.add(Cmd::new(CUT, Text::from("Cut")).chord(Chord::parse("Ctrl+X").unwrap()).glyph(Glyph::Cut))
                .unwrap();
            cmds.add(Cmd::new(COPY, Text::from("Copy")).chord(Chord::parse("Ctrl+Shift+C").unwrap())).unwrap();
            Sc { cmds, log: Vec::new(), list: None, hl: 0 }
        }
    }

    fn opener() -> NodeId {
        NodeId::new(ROW, 0)
    }

    impl Screen for Sc {
        fn layout(&mut self, cx: &mut LayoutCx, avail: SizeP) -> SizeP {
            let r = RectP::wh(cx.px(10), cx.px(10), cx.px(200), cx.px(28));
            cx.push(Node::new(opener(), Role::ListItem, r).label(Text::from("row")).focus(Focusable::Tab));
            let o = RectP::wh(cx.px(10), cx.px(200), cx.px(200), cx.px(28));
            cx.push(Node::new(NodeId::new(OTHER, 0), Role::Button, o).label(Text::from("other")).focus(Focusable::Tab));
            avail
        }

        fn paint(&self, _cx: &mut PaintCx) {}

        fn event(&mut self, cx: &mut EventCx, ev: &Event) -> Reply {
            let t = cx.target().map_or(0, |n| n.owner().0);
            match ev {
                Event::Click { button: MouseButton::Right, pos, .. } if t == ROW.0 => {
                    cx.open_menu(items(), Anchor::Point(*pos));
                }
                Event::Click { button, .. } => self.log.push(format!("{t}:click-{button:?}")),
                Event::Command(id) => self.log.push(format!("{t}:cmd{}", id.0)),
                Event::MenuClosed(m) => self.log.push(format!("{t}:closed{}", m.0)),
                Event::ListPicked { menu, index } => self.log.push(format!("{t}:picked{}-{index}", menu.0)),
                Event::Key { key, down: true, mods, .. } => {
                    self.log.push(format!("{t}:key{:#x}", key.0));
                    let texts = || vec![Text::from("alpha"), Text::from("beta"), Text::from("gamma")];
                    if *key == Key::letter('L') && mods.ctrl() {
                        let r = RectP::wh(cx.px(10), cx.px(10), cx.px(240), cx.px(28));
                        self.list = Some(cx.open_list(texts(), Anchor::Below(r), Some(0)));
                    } else if let (Some(id), Key::DOWN | Key::UP) = (self.list, *key) {
                        self.hl = if *key == Key::DOWN { (self.hl + 1) % 3 } else { (self.hl + 2) % 3 };
                        cx.update_list(id, texts(), Some(self.hl));
                        cx.consume();
                    }
                }
                _ => {}
            }
            Reply::None
        }

        fn commands(&self) -> &CommandTable {
            &self.cmds
        }
    }

    fn harness() -> Harness<Sc> {
        Harness::new(Sc::new(), SizeL { w: 600, h: 400 }, 96, Tokens::oled())
    }

    /// Right-clicks the row: the menu opens at (60, 20).
    fn open(h: &mut Harness<Sc>) -> MenuId {
        h.right_click(PointL { x: 60, y: 20 });
        let id = h.menus().last().expect("a menu opened").id;
        h.hover(PointL { x: 590, y: 390 }); // any input lays the new menu out
        id
    }

    fn stack(h: &mut Harness<Sc>) -> &mut MenuStack {
        &mut h.dispatcher().menus
    }

    fn highlight(h: &mut Harness<Sc>) -> Option<usize> {
        stack(h).menus.last().and_then(|l| l.highlight)
    }

    fn log(h: &mut Harness<Sc>) -> Vec<String> {
        std::mem::take(&mut h.screen_mut().log)
    }

    #[test]
    fn menu_keys_navigate_and_activate() {
        let mut h = harness();
        open(&mut h);
        assert!(is_open(stack(&mut h)) && highlight(&mut h).is_none());
        h.key("Down");
        assert_eq!(highlight(&mut h), Some(0), "the first item");
        h.key("Down").key("Down");
        assert_eq!(highlight(&mut h), Some(4), "past the separator and the header to the disabled Paste");
        h.key("Up");
        assert_eq!(highlight(&mut h), Some(1));
        h.key("End");
        assert_eq!(highlight(&mut h), Some(7));
        h.key("Down");
        assert_eq!(highlight(&mut h), Some(0), "wrapping");
        h.key("Up").key("Home").key("Down");
        assert_eq!(highlight(&mut h), Some(1));
        assert!(log(&mut h).is_empty(), "the keys stay with the menu: {:?}", h.screen().log);
        h.key("Enter");
        assert_eq!(log(&mut h), ["1:cmd11"], "Copy reaches the opener");
        assert!(!is_open(stack(&mut h)) && h.menus().is_empty());
        // Enter on the disabled item does nothing; the menu stays.
        open(&mut h);
        h.key("Down").key("Down").key("Down").key("Enter");
        assert!(is_open(stack(&mut h)) && log(&mut h).is_empty());
        assert!(h.is_idle());
    }

    #[test]
    fn letter_jumps() {
        let mut h = harness();
        open(&mut h);
        h.key("C");
        assert_eq!(highlight(&mut h), Some(0), "Cut");
        h.key("C");
        assert_eq!(highlight(&mut h), Some(1), "Copy");
        h.key("C");
        assert_eq!(highlight(&mut h), Some(6), "Close (the Clipboard header is not an item)");
        h.key("C");
        assert_eq!(highlight(&mut h), Some(0), "around to Cut");
        h.key("P");
        assert_eq!(highlight(&mut h), Some(4), "the disabled Paste can be reached");
        h.key("Q");
        assert_eq!(highlight(&mut h), Some(4), "no item: the highlight stays");
        assert!(log(&mut h).is_empty());
    }

    #[test]
    fn submenu_after_250_ms_or_right() {
        let mut h = harness();
        let root = open(&mut h);
        h.key("S");
        h.key("Right");
        assert_eq!(stack(&mut h).menus.len(), 2, "Right opens the submenu");
        assert_eq!(highlight(&mut h), Some(0), "at its first item");
        h.key("Left");
        assert_eq!(stack(&mut h).menus.len(), 1);
        assert_eq!(highlight(&mut h), Some(5), "the highlight goes back to Sort by");
        h.key("Enter");
        assert_eq!((stack(&mut h).menus.len(), highlight(&mut h)), (2, Some(0)), "Enter opens it too");
        h.key("Esc");
        // The pointer: the submenu opens 250 ms after it came to rest on the row.
        pointer_move(stack(&mut h), root, Some(5), 0, 250);
        assert_eq!(next_due(stack(&mut h)), Some(250));
        h.advance(249);
        assert_eq!(stack(&mut h).menus.len(), 1);
        h.advance(1);
        assert_eq!(stack(&mut h).menus.len(), 2, "open after SUBMENU_DELAY");
        assert_eq!(highlight(&mut h), None, "the pointer opened it: nothing highlighted inside");
        // The pointer moves on to Close: the submenu closes after the delay too.
        pointer_move(stack(&mut h), root, Some(6), 250, 250);
        h.advance(250);
        assert_eq!(stack(&mut h).menus.len(), 1);
        assert!(h.is_idle(), "no delay left");
        // A click on the submenu row opens it at once and a click on its item picks it.
        pointer_release(stack(&mut h), root, Some(5), &mut Vec::new());
        assert_eq!(stack(&mut h).menus.len(), 2);
        let sub = stack(&mut h).menus[1].id;
        let mut fx = Vec::new();
        pointer_release(stack(&mut h), sub, Some(1), &mut fx);
        assert_eq!(take_events(stack(&mut h)), [(Some(opener()), Event::Command(DATE))]);
        assert_eq!(fx, [Effect::CloseMenus]);
    }

    #[test]
    fn esc_closes_one_level_alt_all() {
        let mut h = harness();
        let root = open(&mut h);
        h.key("S").key("Right").key("Esc");
        assert_eq!(stack(&mut h).menus.len(), 1, "one level");
        assert!(log(&mut h).is_empty());
        h.key("Esc");
        assert_eq!(log(&mut h), [format!("1:closed{}", root.0)], "closed without a command");
        assert!(h.menus().is_empty());
        let root = open(&mut h);
        h.key("S").key("Right");
        h.input(crate::dispatch::Input::Key(KEY_ALT, true, false, Mods::ALT));
        assert!(!is_open(stack(&mut h)));
        assert_eq!(log(&mut h), [format!("1:closed{}", root.0)], "Alt closes all, one outcome");
        h.key("Esc");
        assert!(log(&mut h).iter().any(|l| l.contains(":key0x1b")), "with no menu the keys reach the screen again");
    }

    #[test]
    fn outside_click_closes_and_is_swallowed() {
        let mut h = harness();
        let root = open(&mut h);
        h.click(PointL { x: 30, y: 214 });
        assert_eq!(log(&mut h), [format!("1:closed{}", root.0)], "the click on the other node is not delivered");
        assert!(!is_open(stack(&mut h)) && h.menus().is_empty());
        h.click(PointL { x: 30, y: 214 });
        assert_eq!(log(&mut h), ["2:click-Left"], "with no menu it is");
        // In the Harness a click inside the menu is the menu's click: Close (row 6) is picked.
        open(&mut h);
        let r = stack(&mut h).menus[0].row_rect(6).unwrap();
        h.click(PointL { x: r.l + 20, y: r.t + 10 });
        assert_eq!(log(&mut h), ["1:cmd13"]);
    }

    #[test]
    fn deactivation_closes() {
        let mut h = harness();
        let root = open(&mut h);
        h.set_active(false);
        assert!(!is_open(stack(&mut h)) && h.menus().is_empty());
        assert_eq!(log(&mut h), [format!("1:closed{}", root.0)]);
        h.set_active(true);
        h.key("Ctrl+L");
        let list = h.screen().list.unwrap();
        log(&mut h);
        h.set_active(false);
        assert_eq!(log(&mut h), [format!("0:closed{}", list.0)], "a list closes too (it was opened by no node)");
    }

    #[test]
    fn chord_text_comes_from_command_table() {
        let mut h = harness();
        open(&mut h);
        let chord = |h: &mut Harness<Sc>, row: usize| match &stack(h).menus[0].layout.as_ref().unwrap().rows[row].kind {
            RowKind::Item { chord, glyph, .. } => (chord.clone(), *glyph),
            other => panic!("{other:?}"),
        };
        assert_eq!(chord(&mut h, 0), (Some(Text::from("Ctrl+X")), Some(Glyph::Cut)));
        assert_eq!(chord(&mut h, 1).0, Some(Text::from("Ctrl+Shift+C")));
        h.key("Esc");
        let mut h = Harness::new(Sc::new(), SizeL { w: 600, h: 400 }, 96, Tokens::oled()).lang(Lang::It);
        open(&mut h);
        assert_eq!(chord(&mut h, 1).0, Some(Text::from("Ctrl+Maiusc+C")), "the key names in the window's language");
        // A command the table does not have is left out; one it disables shows disabled.
        h.screen_mut().cmds.set_enabled(CUT, false);
        h.key("Esc");
        open(&mut h);
        assert!(matches!(
            stack(&mut h).menus[0].layout.as_ref().unwrap().rows[0].kind,
            RowKind::Item { enabled: false, .. }
        ));
    }

    #[test]
    fn flip_inside_work_area() {
        let work = RectP { l: 0, t: 0, r: 1000, b: 700 };
        let size = SizeP { w: 200, h: 300 };
        let p = |x, y| Anchor::Point(PointP { x, y });
        assert_eq!(place(p(100, 100), size, work, 4, 4), PointP { x: 100, y: 100 }, "top-left at the point");
        assert_eq!(place(p(900, 100), size, work, 4, 4), PointP { x: 700, y: 100 }, "flipped left");
        assert_eq!(place(p(100, 600), size, work, 4, 4), PointP { x: 100, y: 300 }, "flipped up");
        assert_eq!(place(p(950, 650), size, work, 4, 4), PointP { x: 750, y: 350 }, "both");
        assert_eq!(place(p(100, 200), SizeP { w: 200, h: 900 }, work, 4, 4).y, 0, "too tall: clamped at the top");
        let field = RectP::wh(850, 100, 120, 28);
        assert_eq!(place(Anchor::Below(field), size, work, 4, 4), PointP { x: 770, y: 132 }, "right-aligned with it");
        let low = RectP::wh(100, 600, 120, 28);
        assert_eq!(place(Anchor::Below(low), size, work, 4, 4), PointP { x: 100, y: 296 }, "over it");
        assert_eq!(place(Anchor::Above(RectP::wh(100, 100, 120, 28)), size, work, 4, 4).y, 132, "under it");
        let row = RectP::wh(500, 200, 200, 28);
        assert_eq!(place(Anchor::Right(row), size, work, 4, 4), PointP { x: 700, y: 196 }, "beside the row");
        let row = RectP::wh(700, 500, 200, 28);
        assert_eq!(place(Anchor::Right(row), size, work, 4, 4), PointP { x: 500, y: 400 }, "flipped left, kept inside");
        // A monitor left of and above the primary one.
        let left = RectP { l: -1280, t: -200, r: 0, b: 824 };
        assert_eq!(place(p(-100, 700), size, left, 4, 4), PointP { x: -300, y: 400 });
    }

    #[test]
    fn list_mode_keeps_keys_with_caller() {
        let mut h = harness();
        h.key("Ctrl+L");
        let list = h.screen().list.unwrap();
        assert!(!is_open(stack(&mut h)), "a list is not a menu");
        assert_eq!(h.menus().len(), 1);
        h.key("Down").key("Down");
        let l = &stack(&mut h).lists[0];
        assert_eq!(l.highlight, Some(2), "the caller moved it");
        let keys = log(&mut h);
        assert!(keys.iter().filter(|k| k.contains("key0x28")).count() == 2, "the keys reached the caller: {keys:?}");
        assert_eq!(h.menus()[0].highlight, Some(2));
        // An outside click does not close it (its caller does) and is delivered.
        h.click(PointL { x: 100, y: 214 });
        assert_eq!(log(&mut h), ["2:click-Left"]);
        // A click on its second row picks it, and it closes.
        let r = stack(&mut h).lists[0].row_rect(1).unwrap();
        assert_eq!(r.t, 10 + 28 + 4 + 4 + 28, "under the field, 4 px away, past its padding and first row");
        h.click(PointL { x: r.l + 10, y: r.t + 10 });
        assert_eq!(log(&mut h), [format!("2:picked{}-1", list.0)], "to the focused node: no node opened it");
        assert!(stack(&mut h).lists.is_empty() && h.menus().is_empty());
    }

    /// A widget's menu: the pick of a kit command reaches the widget that opened it (event.rs's delivery rule).
    #[test]
    fn kit_command_reaches_the_opener() {
        use crate::model::{
            Align, CellKind, CellOut, ColId, Column, ListModel, RowKey, SelectionMode, SortDir, TableModel, TableStyle,
        };
        use crate::screen::Widget;
        use crate::widgets::VirtualTable;
        struct M;
        impl ListModel for M {
            fn len(&self) -> usize {
                3
            }
            fn key(&self, row: usize) -> RowKey {
                RowKey(row as u64)
            }
            fn row_of(&self, key: RowKey) -> Option<usize> {
                Some(key.0 as usize)
            }
            fn label(&self, row: usize) -> std::borrow::Cow<'_, str> {
                std::borrow::Cow::Owned(format!("row {row}"))
            }
            fn generation(&self) -> u64 {
                0
            }
        }
        impl TableModel for M {
            fn cell(&self, row: usize, _: ColId, out: &mut CellOut) {
                out.set_text(CellKind::Text, &format!("row {row}"));
            }
        }
        struct T(VirtualTable<M>, CommandTable);
        impl Screen for T {
            fn layout(&mut self, cx: &mut LayoutCx, avail: SizeP) -> SizeP {
                self.0.layout(cx, RectP::wh(0, 0, cx.px(400), cx.px(200)));
                avail
            }
            fn paint(&self, cx: &mut PaintCx) {
                self.0.paint(cx);
            }
            fn event(&mut self, cx: &mut EventCx, ev: &Event) -> Reply {
                self.0.event(cx, ev);
                Reply::None
            }
            fn commands(&self) -> &CommandTable {
                &self.1
            }
        }
        let col = |id: u16, title: &'static str| Column {
            id: ColId(id),
            title: Text::from(title),
            width: 120,
            min: 48,
            flex: false,
            align: Align::Start,
            sort: None,
            first_sort: SortDir::Asc,
            visible: true,
            hideable: true,
            tip: None,
        };
        let t = VirtualTable::new(
            WidgetId(7),
            M,
            vec![col(0, "Name"), col(1, "Size")],
            SelectionMode::Multi,
            TableStyle::FILES,
        );
        let mut h = Harness::new(T(t, CommandTable::new()), SizeL { w: 400, h: 200 }, 96, Tokens::oled());
        h.right_click(PointL { x: 180, y: 14 });
        assert!(is_open(&h.dispatcher().menus), "the header menu of Size");
        assert!(h.dispatcher().menus.menus[0].opener.is_some_and(|n| n.owner() == WidgetId(7) && n.slot() != 0));
        // Sort ascending, Sort descending, a separator, Hide column.
        h.key("Down").key("Down").key("Down").key("Enter");
        assert!(!h.screen().0.columns()[1].visible, "the table hid its column: the pick reached it");
        assert!(h.screen().0.columns()[0].visible);
    }

    #[test]
    fn a_new_menu_replaces_the_open_one_without_an_outcome() {
        let mut h = harness();
        open(&mut h);
        h.key("Ctrl+L");
        assert!(is_open(stack(&mut h)), "Ctrl+L went to the menu");
        h.key("Esc");
        log(&mut h);
        h.key("Ctrl+L");
        let first = open(&mut h);
        assert!(stack(&mut h).lists.is_empty(), "the menu closed the list");
        assert_eq!(stack(&mut h).menus[0].id, first);
        assert!(!log(&mut h).iter().any(|l| l.contains("closed")), "the screen opened it: no outcome");
    }

    #[test]
    fn sizes_follow_the_tokens() {
        let mut h = harness();
        open(&mut h);
        let l = stack(&mut h).menus[0].layout.clone().unwrap();
        let tops: Vec<(i32, i32)> = l.rows.iter().map(|r| (r.top, r.h)).collect();
        // 4 px padding, 28 px rows, 13 px separator, a disabled item with its reason 44 px.
        assert_eq!(tops, [(4, 28), (32, 28), (60, 13), (73, 28), (101, 44), (145, 28), (173, 28), (201, 28)]);
        assert_eq!(l.size.h, 233);
        assert!((200..=360).contains(&l.size.w), "{}", l.size.w);
        assert_eq!(l.text_x, 36);
        let mut big = Harness::new(Sc::new(), SizeL { w: 600, h: 400 }, 192, Tokens::oled());
        open(&mut big);
        let l2 = stack(&mut big).menus[0].layout.clone().unwrap();
        assert_eq!((l2.rows[0].h, l2.rows[2].h, l2.text_x), (56, 25, 72));
        // One menu open stays inside its share of the budget: 1 MiB of pixels even at 200 %.
        assert!(l2.size.w as i64 * l2.size.h as i64 * 4 <= 1 << 20, "{:?}", l2.size);
    }
}
