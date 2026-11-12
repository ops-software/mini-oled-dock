//! START MENU (Win double tap): the dock's own Start menu, drawn like the other popups (OLED panel,
//! same rows and plates), so that it opens on ANY display: the native Start menu only opens on the
//! display of the primary taskbar and offers no API to choose. `ui::open_start_menu` opens it anchored
//! to the bar's Start button on the display under the pointer.
//!
//! TWO STYLES (`config.start_style`, read when the menu opens), with different jobs. They share the
//! store, the search, the pins, the launches and the overlays.
//!
//!  * COMPACT (`layout_compact`), a quick launcher: the search box; one strip of pin icons without
//!    names (the name of the hovered one shows under it, the wheel over the strip slides it; a
//!    folder's apps are shown one by one, the folders themselves stay as they are); the list of
//!    all apps under letter headings.
//!  * WINDOWS 11 (`layout_win11`), the full menu: the search box; ONE scrolling page with "Pinned"
//!    (every pin, 6 per row; the pins of a `config.start_folders` entry show as one folder tile,
//!    `pin_tiles`) and, under it, "All apps" as a list or as a grid (`config.start_grid`, the
//!    button on the title); a bottom bar with the user's initial and name (no picture) and the
//!    folder shortcuts (`SHORTCUTS`).
//!
//! No power button in either: the bar always has one (it cannot be hidden).
//!
//! SCROLLING: what scrolls is a list of BANDS (`Band`, `make_bands`): a title, a row of pinned
//! tiles, a list row, a row of app tiles. `scroll` is the first band shown and only whole bands are
//! drawn (`place_bands`), so nothing needs clipping. In the compact style every band is a list row.
//!
//! KEYS: typing filters (the popup has the focus: `WM_CHAR` / `WM_KEYDOWN` reach `Content::char` /
//! `Content::key`); Enter launches the selected result (the first one by default), Up / Down /
//! PgUp / PgDn / Home / End move the selection, Backspace deletes, Esc clears the text (and closes
//! the menu when it is already empty).
//!
//! Left click launches the app (`shell:AppsFolder\<id>` through `ShellRequest::Launch`, so de-elevated
//! like every launch of the dock) AFTER the popup closes. Right click on an app opens its context
//! menu (`menu_rows`): pin / unpin, "Remove from folder", "Run as administrator" (`LaunchAppAdmin`:
//! UAC prompt from the desktop shell), "Open file location" (`RevealApp`), "Uninstall" (the
//! Installed apps page). No jump lists: reading them means parsing shell links from the user's
//! profile inside the elevated dock. A click on a letter heading opens the letter grid, which jumps
//! to a letter. Dragging a pin moves it or (Windows 11 style) groups it into a folder (`drop_zone`).
//!
//! SEARCH (`search`, `rank`): on the names, then on the data Windows itself ships (`start_search`):
//! synonyms ("rdp", "lol"), the popularity rank that orders equal matches, the settings catalog
//! and, for a query that finds nothing, what Windows reads it as (typo rewrites). Every matching
//! app comes before every matching setting (a gear for icon).
//!
//! OVERLAYS (`Over`): the context menu, the letter grid and an open folder are drawn by this popup
//! over its own surface (no second window). While one is open a backdrop hit covers the rest of
//! the menu: a click anywhere else, a right click or Esc closes only the overlay (Esc does not
//! close the Start); typing closes it and goes on searching.
//!
//! DATA: the catalog and the icons live in a UI-thread store (`STORE`) filled only while a Start menu
//! is open. Every open asks the shell thread for a fresh catalog (`ShellRequest::ListStartApps`);
//! the icons are extracted in small batches (behind every other shell request) and arrive as
//! `ShellEvent::StartIcons`. `after_close` keeps the store and arms a 60 s
//! one-shot (`popup::arm_start_release`); a reopen before it fires draws at once. When it fires with
//! no Start menu open, `release` empties the store and sends `ShellRequest::StartRelease` (the shell
//! thread forgets what it sent); events that arrive after that are dropped.
//!
//! Declared as a child module of `popup` (`#[path]`). UI thread only.

use std::cell::RefCell;
use std::collections::HashMap;

use windows::Win32::Foundation::RECT;

use crate::config::StartStyle;
use crate::i18n::tr;
use crate::icons::IconBitmap;
use crate::log;
use crate::popup::{center_y, hline, lift, row_plate, Button, Content, DataEvent, HitMap, PopupKind, Reply};
use crate::render::Metrics;
use crate::shell_thread::{self, ShellRequest, StartApp};
use crate::start_search::{self, Keys};
use crate::surface::{
    rect_wh, Font, Surface, HOVER_ALPHA, PANEL_BORDER_ALPHA, ROW_RADIUS, TEXT2_ALPHA, TEXT3_ALPHA, TEXT_ALPHA, WHITE,
};

// ------------------------------------------------------------------ layout constants (logical px)

// Compact style (`layout_compact`).
/// Panel size: fixed, so the menu does not jump while the search filters the list.
const W: i32 = 420;
const H: i32 = 540;
/// Outer inset of the content.
const INSET: i32 = 14;
const SEARCH_H: i32 = 38;
/// Space between the sections.
const GAP: i32 = 10;
/// Pin strip: side of a cell, icon side inside it, height of the line under it (the name of the
/// hovered pin).
const STRIP_CELL: i32 = 44;
const STRIP_ICON: i32 = 28;
const STRIP_CAPTION: i32 = 18;
/// No cap (the default until `limit`, and the tests).
const UNCAPPED: (i32, i32) = (i32::MAX, i32::MAX);

// Both styles.
/// Section title line ("Pinned", "All apps"; the name of an open folder).
const HEADER_H: i32 = 28;
/// A tile of an open folder: height and icon side (the icon side of every tile).
const TILE_H: i32 = 80;
const TILE_ICON: i32 = 32;
/// App list: row height, icon side, horizontal padding inside a row.
const ROW_H: i32 = 38;
const ROW_ICON: i32 = 24;
const ROW_PAD: i32 = 10;
/// Scroll bar width and the gap between it and the content.
const SCROLLBAR_W: i32 = 4;
const SCROLLBAR_GAP: i32 = 6;
/// Height scrolled per wheel notch (whole bands, at least this much).
const WHEEL_PX: i32 = 110;
/// Side of the icons kept in the store (the tile size; list rows shrink them).
const STORE_ICON: i32 = TILE_ICON;
/// Longest search text kept (chars).
const MAX_QUERY: usize = 64;

// Windows 11 style (`layout_win11`).
/// Panel size: fixed.
const W11_W: i32 = 640;
const W11_H: i32 = 660;
const W11_INSET: i32 = 20;
/// Tiles per row (pinned and "All apps" grid), tile height, icon top inside a tile.
const W11_COLS: usize = 6;
const W11_TILE_H: i32 = 96;
const W11_TILE_ICON_TOP: i32 = 16;
/// Title button ("Grid" / "List"): height and horizontal text padding.
const W11_BTN_H: i32 = 26;
const W11_BTN_PAD: i32 = 12;
/// Idle fill of the title button (a hover plate goes on top).
const W11_BTN_ALPHA: u32 = 16;
/// Bottom bar: height, user circle diameter, side of a folder shortcut.
const W11_BAR_H: i32 = 56;
const W11_AVATAR: i32 = 32;
const W11_SHORTCUT: i32 = 36;

// Overlays (context menu, letter grid, open folder): raised panels over the menu.
/// Menu row height, inner inset, corner radius, margin kept from the panel edges.
const FLY_ROW: i32 = 34;
const FLY_INSET: i32 = 6;
const FLY_RADIUS: i32 = 10;
const FLY_MARGIN: i32 = 4;
/// Fill of a raised panel over the panel colour (a lift that sets it apart) and its outline.
const FLY_LIFT_ALPHA: u32 = 12;
const FLY_RIM_ALPHA: u32 = PANEL_BORDER_ALPHA * 2;
/// Context menu (right click on an app): width.
const MENU_W: i32 = 236;
/// Pointer movement (logical px) after which a press on a pinned tile becomes a drag.
const DRAG_MIN: i32 = 6;
/// Longest folder name typed (chars; the config keeps at most this many).
const RENAME_MAX: usize = 32;
/// Letter grid ("All apps", click on a letter): columns and cell side.
const LETTER_COLS: usize = 6;
const LETTER_CELL: i32 = 44;
/// Open folder: columns and tile width (the height is `TILE_H`).
const FOLDER_COLS: usize = 4;
const FOLDER_TILE_W: i32 = 96;
/// Inner inset of the letter grid and of the open folder.
const GRID_INSET: i32 = 12;

/// Folder shortcuts of the Windows 11 style bottom bar: glyph, what `ShellRequest::Launch` opens
/// (de-elevated, like everything else), label (English, Italian) shown while the pointer is on it.
type Shortcut = (fn(f32, f32) -> bool, &'static str, &'static str, &'static str);
const SHORTCUTS: [Shortcut; 4] = [
    (crate::glyphs::gear_shape, "ms-settings:", "Settings", "Impostazioni"),
    (crate::glyphs::folder_shape, "explorer.exe", "File Explorer", "Esplora file"),
    (crate::glyphs::log_shape, "shell:Personal", "Documents", "Documenti"),
    (crate::glyphs::download_shape, "shell:Downloads", "Downloads", "Download"),
];

// ------------------------------------------------------------------ hit ids

const ID_SEARCH: u32 = 1;
/// Windows 11 style: the "Grid" / "List" button on the "All apps" title.
const ID_VIEW: u32 = 2;
/// Windows 11 style: + index in `SHORTCUTS`.
const ID_SHORTCUT: u32 = 10;
/// An overlay is open: the backdrop over the rest of the menu (a click there closes the overlay).
const ID_BACKDROP: u32 = 900;
/// + index of the pin in `pins`.
const ID_PIN: u32 = 1_000;
/// An overlay is open (context menu, letter grid, folder): + index of its row / cell.
const ID_OVER: u32 = 50_000;
/// The name of the open folder (a click renames it).
const ID_OVER_TITLE: u32 = ID_OVER - 1;
/// + index in the result list.
const ID_ROW: u32 = 100_000;

// ------------------------------------------------------------------ virtual keys

const VK_BACK: u16 = 0x08;
const VK_RETURN: u16 = 0x0D;
const VK_ESCAPE: u16 = 0x1B;
const VK_PRIOR: u16 = 0x21;
const VK_NEXT: u16 = 0x22;
const VK_END: u16 = 0x23;
const VK_HOME: u16 = 0x24;
const VK_UP: u16 = 0x26;
const VK_DOWN: u16 = 0x28;

// ------------------------------------------------------------------ store (outlives the popup)

struct App {
    id: String,
    name: String,
    /// Lowercase name (search and sort key).
    lower: String,
    /// Hidden search keywords: the synonyms Windows lists for this name (`start_search`).
    keys: Keys,
    /// Worldwide popularity from the same data (lower = more popular): orders equal matches.
    rank: u32,
}

#[derive(Default)]
struct Store {
    /// Alphabetical.
    apps: Vec<App>,
    /// true once the first catalog arrived ("Loading" until then).
    loaded: bool,
    /// Icons by lowercase id, `icon_px` square.
    icons: HashMap<String, IconBitmap>,
    icon_px: u32,
    /// true between `request_catalog` and `release`; late shell events are dropped otherwise.
    open: bool,
}

thread_local! {
    static STORE: RefCell<Store> = RefCell::new(Store::default());
}

/// Empties the store (60 s after the menu closed). Returns the bytes of icon bitmaps freed.
fn release_store() -> usize {
    STORE.with(|s| {
        let Ok(mut s) = s.try_borrow_mut() else { return 0 };
        let bytes: usize = s.icons.values().map(|b| (b.width * b.height * 4) as usize).sum();
        let apps = s.apps.len();
        *s = Store::default();
        log!("startmenu: freed {} KB of icons ({apps} apps)", bytes / 1024);
        bytes
    })
}

/// Frees the store and the settings catalog, and tells the shell thread to forget what it sent.
pub(crate) fn release() {
    release_store();
    start_search::release_settings();
    shell_thread::request(ShellRequest::StartRelease);
}

/// `ShellEvent::StartApps`: replaces the catalog (sorted by name).
pub(crate) fn store_apps(list: Vec<StartApp>) {
    // Read by the shell thread before it sent the catalog (`None` only in tests and previews).
    // Taken, not shared: the apps copy their own few keywords below and the rest is freed here.
    let index = start_search::take_app_index();
    let mut apps: Vec<App> = list
        .into_iter()
        .map(|a| {
            let lower = a.name.to_lowercase();
            let (keys, rank) = index.as_ref().map_or((Keys::default(), 0), |x| {
                let rank = x.ranks.get(&a.id.to_lowercase()).copied().unwrap_or(x.default_rank);
                (x.synonyms.get(&lower).cloned().unwrap_or_default(), rank)
            });
            App { keys, rank, lower, id: a.id, name: a.name }
        })
        .collect();
    apps.sort_by(|a, b| a.lower.cmp(&b.lower).then_with(|| a.name.cmp(&b.name)));
    STORE.with(|s| match s.try_borrow_mut() {
        Ok(mut s) if s.open => {
            s.apps = apps;
            s.loaded = true;
        }
        Ok(_) => {}
        Err(_) => log!("startmenu: store busy, catalog dropped"),
    });
}

/// `ShellEvent::StartIcons`: keeps the icons made at the current size. true if any was stored.
pub(crate) fn store_icons(icon_px: u32, icons: Vec<(String, Option<IconBitmap>)>) -> bool {
    STORE.with(|s| {
        let Ok(mut s) = s.try_borrow_mut() else { return false };
        if !s.open || icon_px != s.icon_px {
            return false;
        }
        let mut any = false;
        for (id, b) in icons {
            if let Some(b) = b {
                s.icons.insert(id.to_lowercase(), b);
                any = true;
            }
        }
        any
    })
}

/// Asks the shell thread for a fresh catalog (and the icons still missing) at `px`. A larger size
/// than the stored one (a display with a higher DPI) starts the icons over; a smaller one reuses
/// them (they are shrunk when drawn).
fn request_catalog(px: u32) {
    let px = STORE.with(|s| match s.try_borrow_mut() {
        Ok(mut s) => {
            s.open = true;
            if px > s.icon_px {
                s.icon_px = px;
                s.icons.clear();
            }
            s.icon_px
        }
        Err(_) => px,
    });
    let first = crate::config::get().start_pins.clone();
    shell_thread::request(ShellRequest::ListStartApps { icon_px: px, first });
    // The settings the search also finds: built in the background, unless they are still there.
    start_search::load_settings();
}

// ------------------------------------------------------------------ search (pure)

/// One searchable entry (an app or a setting): lowercase name, hidden keywords with their
/// penalties, popularity rank (lower = better for both numbers; they are Microsoft's).
#[derive(Clone, Copy)]
pub(crate) struct Cand<'a> {
    pub lower: &'a str,
    pub keys: &'a Keys,
    pub rank: u32,
}

/// Match of `c` for the lowercase query `q`: a tier and, inside the tier, a score (lower = first).
/// Every word of the query must appear in the name: tier 0 = the name starts with the query, 1 = a
/// word of the name starts with the first word of the query, 2 = found elsewhere; the score is the
/// popularity rank. Otherwise the keywords are tried: one equal to the query is tier 1 ("lol",
/// "rdp"), one only starting with it (2+ chars) is tier 3, after every match on the visible name;
/// the score is the keyword's penalty. `None` = no match. An empty query matches all, equally.
pub(crate) fn rank(c: &Cand, q: &str) -> Option<(u8, u32)> {
    let q = q.trim();
    if q.is_empty() {
        return Some((0, 0));
    }
    let words: Vec<&str> = q.split_whitespace().collect();
    if !words.iter().all(|w| c.lower.contains(w)) {
        let best = |pred: &dyn Fn(&str) -> bool| c.keys.iter().filter(|k| pred(k.0)).map(|k| k.1).min();
        if let Some(p) = best(&|k| k == q) {
            return Some((1, p));
        }
        if q.chars().count() < 2 {
            return None;
        }
        return best(&|k| k.starts_with(q)).map(|p| (3, p));
    }
    if c.lower.starts_with(q) {
        return Some((0, c.rank));
    }
    let first = words[0];
    let tier = if c.lower.split(|ch: char| !ch.is_alphanumeric()).any(|w| w.starts_with(first)) { 1 } else { 2 };
    Some((tier, c.rank))
}

/// Indices of `cands` matching `query`, best tier first, then best score, then the given order
/// (alphabetical for the apps).
pub(crate) fn filter(cands: &[Cand], query: &str) -> Vec<usize> {
    let q = query.to_lowercase();
    let mut v: Vec<((u8, u32), usize)> =
        cands.iter().enumerate().filter_map(|(i, c)| rank(c, &q).map(|r| (r, i))).collect();
    v.sort();
    v.into_iter().map(|(_, i)| i).collect()
}

/// Letter an app is filed under in the "All apps" list: its first character, uppercase; '#' for
/// anything that is not a letter.
pub(crate) fn letter_of(lower: &str) -> char {
    match lower.chars().next() {
        Some(c) if c.is_alphabetic() => c.to_uppercase().next().unwrap_or(c),
        _ => '#',
    }
}

/// A row of the list.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Item {
    /// Index into `STORE.apps`.
    App(usize),
    /// Index into `start_search::settings()`.
    Setting(usize),
    /// Letter heading of the "All apps" list (a click opens the letter grid).
    Letter(char),
}

/// The "All apps" list: the apps (`letters`: the letter of each, in order) with a heading before
/// every run of the same letter.
pub(crate) fn with_letters(letters: &[char]) -> Vec<Item> {
    let mut out = Vec::with_capacity(letters.len() + 32);
    let mut last = None;
    for (i, &l) in letters.iter().enumerate() {
        if last != Some(l) {
            out.push(Item::Letter(l));
            last = Some(l);
        }
        out.push(Item::App(i));
    }
    out
}

/// A tile of the pinned grid.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Tile {
    /// Index into `STORE.apps`.
    App(usize),
    /// Index into `config.start_folders`.
    Folder(usize),
}

/// The pinned grid: `pins` in order (the catalog index of each, `None` = not installed, and the
/// folder it is in). A folder takes the place of its first app; its other apps are not shown.
pub(crate) fn pin_tiles(pins: &[(Option<usize>, Option<usize>)]) -> Vec<Tile> {
    let mut out = Vec::with_capacity(pins.len());
    for &(app, folder) in pins {
        match (app, folder) {
            (None, _) => {}
            (Some(_), Some(f)) => {
                if !out.contains(&Tile::Folder(f)) {
                    out.push(Tile::Folder(f));
                }
            }
            (Some(a), None) => out.push(Tile::App(a)),
        }
    }
    out
}

/// Where a dragged tile lands.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum DropZone {
    /// On the tile with this index: the two make a folder (or the app joins that folder).
    Into(usize),
    /// Before / after the tile with this index: the dragged one moves there.
    Before(usize),
    After(usize),
}

/// Drop zone of the point `(x, y)` among `cells`, for the tile `src` being dragged. `into`: the
/// middle half of a tile groups the two (an app dragged in the grid); otherwise each half of a
/// tile means before / after it (a folder being moved, an app inside an open folder). `None` over
/// the dragged tile itself or outside every tile.
pub(crate) fn drop_zone(cells: &[RECT], src: usize, x: i32, y: i32, into: bool) -> Option<DropZone> {
    let k = cells.iter().position(|r| x >= r.left && x < r.right && y >= r.top && y < r.bottom)?;
    if k == src {
        return None;
    }
    let quarter = (x - cells[k].left) * 4 / (cells[k].right - cells[k].left).max(1);
    Some(match (into, quarter) {
        (true, 1 | 2) => DropZone::Into(k),
        (_, 0 | 1) => DropZone::Before(k),
        _ => DropZone::After(k),
    })
}

// ------------------------------------------------------------------ geometry (pure)

/// Layout of the panel, client coordinates, physical px.
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct Geo {
    pub w: i32,
    pub h: i32,
    pub search: RECT,
    /// Compact style: the cells of the pin strip (as many as fit, none without pins) and the line
    /// under it where the name of the hovered pin goes.
    pub strip: Vec<RECT>,
    pub caption: RECT,
    /// The scrolling area: the bands and, at its right edge, the scroll bar.
    pub view: RECT,
    /// Windows 11 style: the bottom bar and its folder shortcuts (`SHORTCUTS` order).
    pub bar: Option<RECT>,
    pub shortcuts: Vec<RECT>,
}

/// Compact layout for `pins` pinned apps, inside `max` (the work area minus the margins): search
/// box, pin strip, the list down to the bottom.
pub(crate) fn layout_compact(m: Metrics, pins: usize, max: (i32, i32)) -> Geo {
    let (w, h, inset, gap) = (m.px(W).min(max.0), m.px(H).min(max.1), m.px(INSET), m.px(GAP));
    let inner = w - 2 * inset;
    let search = rect_wh(inset, inset, inner, m.px(SEARCH_H));
    let mut y = search.bottom + gap;
    let cell = m.px(STRIP_CELL).max(1);
    let shown = pins.min((inner / cell).max(0) as usize);
    let strip: Vec<RECT> = (0..shown as i32).map(|i| rect_wh(inset + i * cell, y, cell, cell)).collect();
    let mut caption = RECT::default();
    if shown > 0 {
        caption = rect_wh(inset, y + cell, inner, m.px(STRIP_CAPTION));
        y = caption.bottom + gap / 2;
    }
    let view = RECT { left: inset, top: y, right: w - inset, bottom: h - inset };
    Geo { w, h, search, strip, caption, view, bar: None, shortcuts: Vec::new() }
}

/// Windows 11 layout inside `max`: search box, the scrolling page, the bottom bar with the folder
/// shortcuts at its right end.
pub(crate) fn layout_win11(m: Metrics, max: (i32, i32)) -> Geo {
    let (w, h, inset, gap) = (m.px(W11_W).min(max.0), m.px(W11_H).min(max.1), m.px(W11_INSET), m.px(GAP));
    let search = rect_wh(inset, inset, w - 2 * inset, m.px(SEARCH_H));
    let bar = RECT { left: 0, top: h - m.px(W11_BAR_H), right: w, bottom: h };
    let view = RECT { left: inset, top: search.bottom + gap, right: w - inset, bottom: bar.top - gap / 2 };
    let side = m.px(W11_SHORTCUT);
    let n = SHORTCUTS.len() as i32;
    let top = bar.top + (bar.bottom - bar.top - side) / 2;
    let shortcuts = (0..n).map(|i| rect_wh(w - inset - (n - i) * side, top, side, side)).collect();
    Geo { w, h, search, strip: Vec::new(), caption: RECT::default(), view, bar: Some(bar), shortcuts }
}

/// One thing that scrolls: only whole bands are drawn.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Band {
    /// "Pinned".
    PinsTitle,
    /// A row of pinned tiles: the index of its first tile in `pins`.
    Pins(usize),
    /// "All apps" / "Results".
    AppsTitle,
    /// One list row: index into `results` (an app, a setting or a letter heading).
    Row(usize),
    /// A row of app tiles ("All apps" as a grid): index of the first one in `results`, how many.
    Tiles(usize, usize),
}

/// The bands of a menu with `pins` pinned tiles and the list `results`. `titles`: the Windows 11
/// style, with its two titled sections (the pinned one only when something is pinned); the compact
/// style has only the rows. `grid`: the apps go `cols` per row under their letter headings, which
/// stay rows of their own (so does anything that is not an app).
pub(crate) fn make_bands(pins: usize, results: &[Item], cols: usize, grid: bool, titles: bool) -> Vec<Band> {
    let cols = cols.max(1);
    let mut out = Vec::with_capacity(results.len() + pins / cols + 3);
    if titles {
        if pins > 0 {
            out.push(Band::PinsTitle);
            out.extend((0..pins).step_by(cols).map(Band::Pins));
        }
        out.push(Band::AppsTitle);
    }
    if !grid {
        out.extend((0..results.len()).map(Band::Row));
        return out;
    }
    let mut i = 0;
    while i < results.len() {
        let n = results[i..].iter().take(cols).take_while(|it| matches!(it, Item::App(_))).count();
        if n == 0 {
            out.push(Band::Row(i));
            i += 1;
        } else {
            out.push(Band::Tiles(i, n));
            i += n;
        }
    }
    out
}

/// Height of a band (physical px, at least 1).
pub(crate) fn band_h(m: Metrics, band: Band) -> i32 {
    match band {
        Band::PinsTitle | Band::AppsTitle => m.px(HEADER_H),
        Band::Pins(_) | Band::Tiles(..) => m.px(W11_TILE_H),
        Band::Row(_) => m.px(ROW_H),
    }
    .max(1)
}

/// The bands drawn in `view` when `scroll` is the first one: index and rectangle of each, top to
/// bottom, whole bands only.
pub(crate) fn place_bands(m: Metrics, bands: &[Band], scroll: usize, view: RECT) -> Vec<(usize, RECT)> {
    let mut out = Vec::new();
    let mut y = view.top;
    for (i, &b) in bands.iter().enumerate().skip(scroll) {
        let h = band_h(m, b);
        if y + h > view.bottom {
            break;
        }
        out.push((i, RECT { left: view.left, top: y, right: view.right, bottom: y + h }));
        y += h;
    }
    out
}

/// Largest useful `scroll`: the first band from which everything down to the last one fits a view
/// `view_h` high.
pub(crate) fn max_band_scroll(m: Metrics, bands: &[Band], view_h: i32) -> usize {
    let mut s = bands.len();
    let mut h = 0;
    while s > 0 {
        h += band_h(m, bands[s - 1]);
        if h > view_h {
            break;
        }
        s -= 1;
    }
    s.min(bands.len().saturating_sub(1))
}

/// Index of the band that shows `results[result]`.
pub(crate) fn band_of(bands: &[Band], result: usize) -> Option<usize> {
    bands.iter().position(|b| match *b {
        Band::Row(i) => i == result,
        Band::Tiles(first, n) => (first..first + n).contains(&result),
        _ => false,
    })
}

/// Layout of an overlay drawn over the menu (context menu, letter grid, open folder).
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct OverGeo {
    pub panel: RECT,
    /// Title line (open folder only).
    pub title: Option<RECT>,
    /// Rows of the context menu, or cells of a grid, in order.
    pub cells: Vec<RECT>,
}

/// Context menu of `n` rows for the control at `anchor`, in a `w` x `h` panel: under the control
/// (above it when there is no room), kept inside the panel.
pub(crate) fn layout_menu(m: Metrics, w: i32, h: i32, anchor: RECT, n: usize) -> OverGeo {
    let (row, inset, margin) = (m.px(FLY_ROW), m.px(FLY_INSET), m.px(FLY_MARGIN));
    let fw = m.px(MENU_W).min(w - 2 * margin);
    let fh = 2 * inset + n as i32 * row;
    let left = (anchor.left + m.px(24)).min(w - margin - fw).max(margin);
    let mut top = anchor.bottom - m.px(6);
    if top + fh > h - margin {
        top = anchor.top + m.px(6) - fh;
    }
    let top = top.min(h - margin - fh).max(margin);
    let cells = (0..n as i32).map(|i| rect_wh(left + inset, top + inset + i * row, fw - 2 * inset, row)).collect();
    OverGeo { panel: rect_wh(left, top, fw, fh), title: None, cells }
}

/// A grid of `n` cells (`cell`: width and height, at most `cols` per row) centred in a `w` x `h`
/// panel, with a title line above when `title`. Columns and rows that do not fit are dropped.
pub(crate) fn layout_grid(m: Metrics, w: i32, h: i32, n: usize, cols: usize, cell: (i32, i32), title: bool) -> OverGeo {
    let (inset, margin) = (m.px(GRID_INSET), m.px(FLY_MARGIN));
    let (cw, ch) = (cell.0.max(1), cell.1.max(1));
    let th = if title { m.px(HEADER_H) } else { 0 };
    let cols = cols.min(n.max(1)).min(((w - 2 * margin - 2 * inset) / cw).max(1) as usize);
    let max_rows = ((h - 2 * margin - 2 * inset - th) / ch).max(1) as usize;
    let rows = n.div_ceil(cols).clamp(1, max_rows);
    let (pw, ph) = (2 * inset + cols as i32 * cw, 2 * inset + th + rows as i32 * ch);
    let (left, top) = (((w - pw) / 2).max(0), ((h - ph) / 2).max(0));
    let cells = (0..n.min(rows * cols))
        .map(|i| rect_wh(left + inset + (i % cols) as i32 * cw, top + inset + th + (i / cols) as i32 * ch, cw, ch))
        .collect();
    let title = title.then(|| rect_wh(left + inset, top + inset, pw - 2 * inset, th));
    OverGeo { panel: rect_wh(left, top, pw, ph), title, cells }
}

/// The end of `text` that fits in `max_w` (the start is dropped): the search box shows what is
/// being typed.
fn tail_fit(s: &Surface, text: &str, max_w: i32) -> String {
    let mut t = text;
    while !t.is_empty() && s.text_width(t, Font::Normal) > max_w {
        let mut it = t.char_indices();
        it.next();
        t = it.next().map_or("", |(i, _)| &t[i..]);
    }
    t.to_string()
}

/// Magnifier glyph (16x16 space): a ring and a handle towards the bottom right.
fn magnifier(u: f32, v: f32) -> bool {
    let (dx, dy) = (u - 6.5, v - 6.5);
    if ((dx * dx + dy * dy).sqrt() - 4.6).abs() <= 0.85 {
        return true;
    }
    let (du, dv) = (u - 10.0, v - 10.0);
    let t = (du + dv) / 2.0;
    (0.0..=4.0).contains(&t) && (du - dv).abs() / std::f32::consts::SQRT_2 <= 1.0
}

// ------------------------------------------------------------------ labels

struct Labels {
    placeholder: &'static str,
    pinned: &'static str,
    all: &'static str,
    results: &'static str,
    hint: &'static str,
    loading: &'static str,
    none: &'static str,
    /// Windows 11 style: the title button that switches "All apps" to a grid / back to a list.
    view_grid: &'static str,
    view_list: &'static str,
    /// Context menu.
    pin: &'static str,
    unpin: &'static str,
    leave_folder: &'static str,
    rename: &'static str,
    /// Hint next to "Pinned".
    drag_hint: &'static str,
    admin: &'static str,
    location: &'static str,
    uninstall: &'static str,
    ungroup: &'static str,
    /// Name of a new folder (a number follows).
    folder: &'static str,
}

/// The labels in the dock language (`i18n::tr`, like every other popup; English in preview mode).
fn labels() -> Labels {
    Labels {
        placeholder: tr("Type to search apps", "Scrivi per cercare un'app"),
        pinned: tr("Pinned", "Aggiunte"),
        all: tr("All apps", "Tutte le app"),
        results: tr("Results", "Risultati"),
        hint: tr("Right-click for options", "Clic destro per le opzioni"),
        loading: tr("Loading apps\u{2026}", "Caricamento app\u{2026}"),
        none: tr("No apps found", "Nessuna app trovata"),
        view_grid: tr("Grid", "Griglia"),
        view_list: tr("List", "Elenco"),
        pin: tr("Pin to Start", "Aggiungi a Start"),
        unpin: tr("Unpin from Start", "Rimuovi da Start"),
        leave_folder: tr("Remove from folder", "Rimuovi dalla cartella"),
        rename: tr("Rename folder", "Rinomina cartella"),
        drag_hint: tr("Drag to reorder or to group", "Trascina per ordinare o raggruppare"),
        admin: tr("Run as administrator", "Esegui come amministratore"),
        location: tr("Open file location", "Apri percorso file"),
        uninstall: tr("Uninstall", "Disinstalla"),
        ungroup: tr("Ungroup folder", "Separa cartella"),
        folder: tr("Folder", "Cartella"),
    }
}

// ------------------------------------------------------------------ content

/// What a click resolves to.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Target {
    App(usize),
    Setting(usize),
    Letter,
    Folder(usize),
}

/// What runs after the popup has closed.
#[derive(Clone, Debug, PartialEq, Eq)]
enum Pending {
    /// Launch the app with this AppsFolder id.
    App(String),
    /// Open a setting: target and arguments (`start_search::Setting`).
    Setting(String, Option<String>),
    /// "Run as administrator" on the app with this id.
    Admin(String),
    /// "Open file location" of the app with this id.
    Reveal(String),
    /// "Uninstall": the Installed apps page of Settings, where Windows uninstalls every kind of app.
    Uninstall,
}

/// A row of the context menu.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum MenuAct {
    /// Pin / unpin.
    Pin,
    /// Take the app out of its folder (it stays pinned).
    LeaveFolder,
    /// Open the folder with this index with its name being edited.
    Rename(usize),
    Admin,
    Location,
    Uninstall,
    /// Drop the folder with this index (its apps stay pinned).
    Ungroup(usize),
}

/// An overlay drawn by this popup over its own surface, like the power flyout: a backdrop hit
/// covers the rest of the menu (a click there, Esc or typing closes only the overlay).
#[derive(Clone, Debug, PartialEq, Eq)]
enum Over {
    /// Context menu of the app `id` (empty for a folder tile): actions and labels, in order.
    Menu { id: String, rows: Vec<(MenuAct, String)> },
    /// Letter grid of "All apps": each letter and the index of its heading in `results`.
    Letters(Vec<(char, usize)>),
    /// An open folder: its index in `config.start_folders`, its name and the catalog indices of
    /// its apps.
    Folder { folder: usize, name: String, apps: Vec<usize> },
}

/// A press on a pinned tile (in the grid, or inside the open folder) that may become a drag.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Drag {
    /// Hit id of the tile pressed.
    id: u32,
    start: (i32, i32),
    pos: (i32, i32),
    /// The pointer moved `DRAG_MIN`: the tile follows it and the release drops it.
    active: bool,
}

/// Rows of the context menu of the app `id`: pin, "Remove from folder" for an app that is in one,
/// then the actions of the native menu. A Store app (`!` in the id) has no file location. The
/// menu has the same few rows however many folders exist: folders are made, filled and emptied by
/// dragging (`Content::drag`).
fn menu_rows(id: &str, cfg: &crate::config::Config) -> Vec<(MenuAct, String)> {
    let l = labels();
    let pinned = cfg.is_start_pinned(id);
    let mut rows = vec![(MenuAct::Pin, if pinned { l.unpin } else { l.pin }.to_string())];
    if pinned && cfg.start_folder_of(id).is_some() {
        rows.push((MenuAct::LeaveFolder, l.leave_folder.to_string()));
    }
    rows.push((MenuAct::Admin, l.admin.to_string()));
    if !id.contains('!') {
        rows.push((MenuAct::Location, l.location.to_string()));
    }
    rows.push((MenuAct::Uninstall, l.uninstall.to_string()));
    rows
}

/// Name for a new folder: "Folder N", the first N not taken.
fn new_folder_name(cfg: &crate::config::Config) -> String {
    (cfg.start_folders.len() + 1..)
        .map(|n| format!("{} {n}", labels().folder))
        .find(|name| !cfg.start_folders.iter().any(|f| &f.name == name))
        .unwrap_or_default()
}

/// Name of the folder `f` and the catalog indices of its apps that are pinned and installed.
fn folder_content(st: &Store, f: usize) -> Option<(String, Vec<usize>)> {
    let cfg = crate::config::get();
    let folder = cfg.start_folders.get(f)?;
    let apps = folder
        .apps
        .iter()
        .filter(|a| cfg.is_start_pinned(a))
        .filter_map(|a| st.apps.iter().position(|x| x.id.eq_ignore_ascii_case(a)))
        .collect();
    Some((folder.name.clone(), apps))
}

pub(crate) struct StartMenu {
    m: Metrics,
    geo: Geo,
    query: String,
    /// The list: the matches of the query (apps, then settings), or every app under its letter
    /// when the query is empty.
    results: Vec<Item>,
    /// The pins (none while searching): tiles with folders in the Windows 11 style, every pinned
    /// app one by one in the compact strip.
    pins: Vec<Tile>,
    /// What scrolls (`make_bands`), the first band shown, and the bands drawn with their
    /// rectangles (set by `rebuild_hits`).
    bands: Vec<Band>,
    scroll: usize,
    placed: Vec<(usize, RECT)>,
    /// The pins on screen: index in `pins` and rectangle (set by `rebuild_hits`).
    pin_cells: Vec<(usize, RECT)>,
    /// Compact style: first pin shown in the strip.
    strip_scroll: usize,
    /// Keyboard selection (index into `results`).
    sel: Option<usize>,
    /// The last query that found nothing and what Windows Search reads it as, if anything.
    rewrite: Option<(String, Option<String>)>,
    hits: HitMap,
    requested: bool,
    /// What was chosen, run after the popup closes.
    pending: Option<Pending>,
    /// Overlay open over the menu, its layout (set by `layout_over`) and the control it is for.
    over: Option<Over>,
    over_geo: OverGeo,
    over_anchor: RECT,
    /// Pinned tile pressed / being dragged, and whether a drag just ended (the click that follows
    /// the release on the same tile is not a launch).
    drag: Option<Drag>,
    dropped: bool,
    /// Name being typed for the open folder (`Over::Folder`).
    rename: Option<String>,
    /// Windows 11 style (`config.start_style` at open time).
    win11: bool,
    /// Windows 11 style: "All apps" is shown as a grid (`config.start_grid`; a list while searching).
    grid: bool,
    /// Windows 11 style: Windows user name for the bottom bar (empty in the compact style).
    user: String,
    /// Windows 11 style: width of the "Grid" / "List" button (physical px, set by `size`).
    button_w: i32,
    /// Largest panel size (physical px): the work area minus the margins, set by `limit`.
    max: (i32, i32),
    /// PREVIEW-HOOK: overlay to open once the first layout exists (`preview_new`).
    preview_over: Option<PreviewOver>,
}

/// PREVIEW-HOOK: the overlay a preview shows open.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum PreviewOver {
    /// Context menu of the first pin.
    Menu,
    /// Letter grid of "All apps".
    Letters,
    /// The first folder of the pinned grid.
    Folder,
}

/// Creates the content in the configured style; labels in the dock language (`i18n`).
pub(crate) fn new() -> Box<dyn Content> {
    let mut c = make();
    if crate::config::get().start_style == StartStyle::Windows11 {
        c.win11 = true;
        // The account name of the process (as GetUserNameW): no picture, nothing else loaded.
        c.user = std::env::var("USERNAME").unwrap_or_default();
    }
    Box::new(c)
}

fn make() -> StartMenu {
    StartMenu {
        m: Metrics { dpi: 96 },
        geo: Geo::default(),
        query: String::new(),
        results: Vec::new(),
        pins: Vec::new(),
        bands: Vec::new(),
        scroll: 0,
        placed: Vec::new(),
        pin_cells: Vec::new(),
        strip_scroll: 0,
        sel: None,
        rewrite: None,
        hits: HitMap::default(),
        requested: false,
        pending: None,
        over: None,
        over_geo: OverGeo::default(),
        over_anchor: RECT::default(),
        drag: None,
        dropped: false,
        rename: None,
        win11: false,
        grid: false,
        user: String::new(),
        button_w: 0,
        max: UNCAPPED,
        preview_over: None,
    }
}

/// PREVIEW-HOOK: the menu over a mock catalog (`apps`: id, name, icon; the icons all of one size),
/// English labels (`--preview` puts `i18n` in preview mode), `query` already typed, in `style`
/// (Windows 11: a fixed user name; list or grid as `config.start_grid` says), with the overlay
/// `over` open. Replaces the store; asks the shell thread for nothing.
pub(crate) fn preview_new(
    apps: Vec<(String, String, IconBitmap)>,
    query: &str,
    style: StartStyle,
    over: Option<PreviewOver>,
) -> Box<dyn Content> {
    let px = apps.first().map_or(32, |a| a.2.width);
    STORE.with(|s| {
        if let Ok(mut s) = s.try_borrow_mut() {
            *s = Store { icon_px: px, open: true, ..Store::default() };
        }
    });
    store_apps(apps.iter().map(|(id, name, _)| StartApp { id: id.clone(), name: name.clone() }).collect());
    store_icons(px, apps.into_iter().map(|(id, _, b)| (id, Some(b))).collect());
    let mut c = make();
    c.requested = true;
    c.preview_over = over;
    if style == StartStyle::Windows11 {
        c.win11 = true;
        c.user = "User".to_string();
    }
    c.query = query.to_string();
    if c.searching() {
        c.sel = Some(0);
    }
    Box::new(c)
}

impl StartMenu {
    fn searching(&self) -> bool {
        !self.query.trim().is_empty()
    }

    /// Recomputes results, pins, bands, layout, scroll / selection bounds and the hit map from the store.
    fn refresh(&mut self) {
        let cfg = crate::config::get();
        let searching = self.searching();
        let win11 = self.win11;
        let (results, pins) = STORE.with(|s| {
            let Ok(s) = s.try_borrow() else { return (Vec::new(), Vec::new()) };
            if !searching {
                let letters: Vec<char> = s.apps.iter().map(|a| letter_of(&a.lower)).collect();
                let find = |p: &String| s.apps.iter().position(|a| a.id.eq_ignore_ascii_case(p));
                let pins: Vec<_> = cfg.start_pins.iter().map(|p| (find(p), cfg.start_folder_of(p))).collect();
                // The compact strip shows every pinned app by itself, whatever folder it is in.
                let tiles =
                    if win11 { pin_tiles(&pins) } else { pins.iter().filter_map(|p| p.0).map(Tile::App).collect() };
                return (with_letters(&letters), tiles);
            }
            // Every matching app, then every matching setting (none until their catalog is built).
            let apps: Vec<Cand> = s.apps.iter().map(|a| Cand { lower: &a.lower, keys: &a.keys, rank: a.rank }).collect();
            let catalog = start_search::settings();
            let settings: Vec<Cand> = catalog
                .iter()
                .map(|x| Cand { lower: &x.lower, keys: &x.keys, rank: x.rank })
                .collect();
            let find = |q: &str| -> Vec<Item> {
                let mut found: Vec<Item> = filter(&apps, q).into_iter().map(Item::App).collect();
                found.extend(filter(&settings, q).into_iter().map(Item::Setting));
                found
            };
            let mut results = find(&self.query);
            if results.is_empty() {
                // Nothing as typed: what Windows Search reads it as ("rkdp" is "rdp"), looked up
                // once per query (this runs again at every batch of icons).
                if self.rewrite.as_ref().is_none_or(|r| r.0 != self.query) {
                    self.rewrite = Some((self.query.clone(), start_search::converted(&self.query)));
                }
                if let Some((_, Some(meant))) = &self.rewrite {
                    results = find(meant);
                }
            }
            (results, Vec::new())
        });
        self.results = results;
        self.pins = pins;
        self.grid = win11 && !searching && cfg.start_grid;
        self.geo = if win11 { layout_win11(self.m, self.max) } else { layout_compact(self.m, self.pins.len(), self.max) };
        let tiles = if win11 { self.pins.len() } else { 0 };
        self.bands = make_bands(tiles, &self.results, W11_COLS, self.grid, win11);
        self.strip_scroll = self.strip_scroll.min(self.pins.len().saturating_sub(self.geo.strip.len()));
        self.scroll = self.scroll.min(self.max_scroll());
        if let Some(s) = self.sel {
            self.sel = if self.results.is_empty() { None } else { Some(s.min(self.results.len() - 1)) };
        }
        self.layout_over();
        self.rebuild_hits();
    }

    /// Lays the open overlay out in the current panel.
    fn layout_over(&mut self) {
        let (m, w, h) = (self.m, self.geo.w, self.geo.h);
        self.over_geo = match &self.over {
            Some(Over::Menu { rows, .. }) => layout_menu(m, w, h, self.over_anchor, rows.len()),
            Some(Over::Letters(l)) => {
                layout_grid(m, w, h, l.len(), LETTER_COLS, (m.px(LETTER_CELL), m.px(LETTER_CELL)), false)
            }
            Some(Over::Folder { apps, .. }) => {
                layout_grid(m, w, h, apps.len(), FOLDER_COLS, (m.px(FOLDER_TILE_W), m.px(TILE_H)), true)
            }
            None => OverGeo::default(),
        };
    }

    /// Opens `over` for the control at `anchor`.
    fn open_over(&mut self, over: Over, anchor: RECT) -> Reply {
        self.rename = None;
        self.drag = None;
        self.over = Some(over);
        self.over_anchor = anchor;
        self.layout_over();
        self.rebuild_hits();
        Reply::Redraw
    }

    fn close_over(&mut self) -> Reply {
        if self.over.take().is_none() {
            return Reply::None;
        }
        self.rename = None;
        self.layout_over();
        self.rebuild_hits();
        Reply::Redraw
    }

    /// Context menu of the app at catalog index `idx`.
    fn open_menu(&mut self, idx: usize, anchor: RECT) -> Reply {
        let Some(id) = Self::app_id(idx) else { return Reply::None };
        let rows = menu_rows(&id, &crate::config::get());
        self.open_over(Over::Menu { id, rows }, anchor)
    }

    /// The letter grid of the list shown (nothing while searching: no letters there).
    fn open_letters(&mut self) -> Reply {
        let letters: Vec<(char, usize)> = self
            .results
            .iter()
            .enumerate()
            .filter_map(|(i, it)| if let Item::Letter(c) = it { Some((*c, i)) } else { None })
            .collect();
        if letters.is_empty() {
            return Reply::None;
        }
        self.open_over(Over::Letters(letters), RECT::default())
    }

    fn open_folder(&mut self, f: usize) -> Reply {
        let content = STORE.with(|s| s.try_borrow().ok().and_then(|s| folder_content(&s, f)));
        match content {
            Some((name, apps)) if !apps.is_empty() => {
                self.open_over(Over::Folder { folder: f, name, apps }, RECT::default())
            }
            _ => Reply::None,
        }
    }

    /// The name being typed, while a folder is open and its name is being edited.
    fn renaming(&mut self) -> Option<&mut String> {
        if matches!(self.over, Some(Over::Folder { .. })) {
            self.rename.as_mut()
        } else {
            None
        }
    }

    /// Enter while renaming: saves the typed name (an empty one keeps the old name).
    fn commit_rename(&mut self) -> Reply {
        let Some(text) = self.rename.take() else { return Reply::None };
        if let Some(Over::Folder { folder, name, .. }) = self.over.as_mut() {
            match crate::config::start_folder_rename(*folder, &text) {
                Ok(()) => {
                    if let Some(f) = crate::config::get().start_folders.get(*folder) {
                        *name = f.name.clone();
                    }
                }
                Err(e) => log!("startmenu: cannot save the folder name: {e}"),
            }
        }
        self.refresh();
        Reply::Redraw
    }

    /// Pin ids behind a tile, in pin order: the app, or the apps of the folder.
    fn tile_ids(tile: Tile) -> Vec<String> {
        match tile {
            Tile::App(i) => Self::app_id(i).into_iter().collect(),
            Tile::Folder(f) => {
                let cfg = crate::config::get();
                cfg.start_pins.iter().filter(|p| cfg.start_folder_of(p) == Some(f)).cloned().collect()
            }
        }
    }

    /// The tile being dragged (its index among the tiles shown, or among the cells of the open
    /// folder) and where it would land if released now.
    fn drag_zone(&self) -> Option<(usize, DropZone)> {
        let d = self.drag.filter(|d| d.active)?;
        let (x, y) = d.pos;
        if matches!(self.over, Some(Over::Folder { .. })) {
            let src = d.id.checked_sub(ID_OVER)? as usize;
            return drop_zone(&self.over_geo.cells, src, x, y, false).map(|z| (src, z));
        }
        let pin = d.id.checked_sub(ID_PIN)? as usize;
        let src = self.pin_cells.iter().position(|c| c.0 == pin)?;
        let cells: Vec<RECT> = self.pin_cells.iter().map(|c| c.1).collect();
        // Grouping is for the tiles of the Windows 11 style; a folder cannot go inside another
        // tile, and the compact strip has no folders: those only move.
        let into = self.win11 && matches!(self.pins.get(pin), Some(Tile::App(_)));
        drop_zone(&cells, src, x, y, into).map(|z| (src, z))
    }

    /// The pin shown in the `k`-th cell of `pin_cells`.
    fn pin_at_cell(&self, k: usize) -> Option<Tile> {
        self.pin_cells.get(k).and_then(|c| self.pins.get(c.0)).copied()
    }

    /// The button was released after a drag: applies the drop (see `drop_zone`). Inside an open
    /// folder a drop on another app reorders, a drop outside the folder takes the app out of it.
    fn finish_drag(&mut self, d: Drag, zone: Option<(usize, DropZone)>) -> Reply {
        let mut reopen = None;
        let saved = if let Some(Over::Folder { folder, apps, .. }) = self.over.clone() {
            let id = d.id.checked_sub(ID_OVER).and_then(|i| apps.get(i as usize)).copied().and_then(Self::app_id);
            let p = self.over_geo.panel;
            let outside = d.pos.0 < p.left || d.pos.0 >= p.right || d.pos.1 < p.top || d.pos.1 >= p.bottom;
            match (id, zone) {
                (Some(id), Some((_, z @ (DropZone::Before(k) | DropZone::After(k))))) => {
                    reopen = Some(folder);
                    match apps.get(k).copied().and_then(Self::app_id) {
                        Some(anchor) => {
                            crate::config::start_folder_reorder(folder, &id, &anchor, matches!(z, DropZone::After(_)))
                        }
                        None => Ok(()),
                    }
                }
                (Some(id), _) if outside => {
                    self.over = None;
                    crate::config::start_folder_leave(&id)
                }
                _ => Ok(()),
            }
        } else {
            let tiles = zone.and_then(|(src, z)| {
                let (DropZone::Into(k) | DropZone::Before(k) | DropZone::After(k)) = z;
                Some((Self::tile_ids(self.pin_at_cell(src)?), Self::tile_ids(self.pin_at_cell(k)?), z))
            });
            match tiles {
                Some((moved, target, z)) => match (moved.first(), target.first(), z) {
                    (Some(src), Some(anchor), DropZone::Into(_)) => {
                        let name = new_folder_name(&crate::config::get());
                        crate::config::start_folder_join(src, anchor, &name)
                    }
                    (Some(_), Some(anchor), _) => {
                        crate::config::start_pin_move(moved.clone(), anchor, matches!(z, DropZone::After(_)))
                    }
                    _ => Ok(()),
                },
                None => Ok(()),
            }
        };
        if let Err(e) = saved {
            log!("startmenu: cannot save the drop: {e}");
        }
        self.refresh();
        if let Some(f) = reopen {
            self.over = None;
            self.open_folder(f);
        }
        Reply::Redraw
    }

    /// A row or cell of the open overlay was clicked.
    fn over_click(&mut self, i: usize) -> Reply {
        match self.over.clone() {
            Some(Over::Menu { id, rows }) => match rows.get(i) {
                Some(&(act, _)) => self.run_menu(act, &id),
                None => Reply::None,
            },
            Some(Over::Letters(letters)) => {
                // The heading of the letter goes to the top of the list.
                if let Some(&(_, at)) = letters.get(i) {
                    self.scroll = band_of(&self.bands, at).unwrap_or(0).min(self.max_scroll());
                    self.sel = None;
                }
                self.close_over()
            }
            Some(Over::Folder { apps, .. }) => match apps.get(i) {
                Some(&idx) => self.activate(Target::App(idx)),
                None => Reply::None,
            },
            None => Reply::None,
        }
    }

    /// Runs a context menu row for the app `id`: the config changes apply at once (the Start stays
    /// open), the launches wait for the popup to close.
    fn run_menu(&mut self, act: MenuAct, id: &str) -> Reply {
        self.over = None;
        let saved = match act {
            MenuAct::Pin => crate::config::toggle_start_pin(id),
            MenuAct::LeaveFolder => crate::config::start_folder_leave(id),
            MenuAct::Ungroup(f) => crate::config::start_folder_dissolve(f),
            MenuAct::Rename(f) => {
                // The folder opens with its name ready to be typed over.
                self.open_folder(f);
                if let Some(Over::Folder { name, .. }) = &self.over {
                    self.rename = Some(name.clone());
                }
                return Reply::Redraw;
            }
            MenuAct::Admin | MenuAct::Location | MenuAct::Uninstall => {
                self.pending = Some(match act {
                    MenuAct::Admin => Pending::Admin(id.to_string()),
                    MenuAct::Location => Pending::Reveal(id.to_string()),
                    _ => Pending::Uninstall,
                });
                return Reply::Close;
            }
        };
        match saved {
            Ok(()) => log!("startmenu: {act:?} for '{id}'"),
            Err(e) => log!("startmenu: cannot save {act:?} for '{id}': {e}"),
        }
        self.refresh();
        Reply::Redraw
    }

    fn max_scroll(&self) -> usize {
        max_band_scroll(self.m, &self.bands, self.geo.view.bottom - self.geo.view.top)
    }

    /// Width left to the bands by the scroll bar.
    fn content_right(&self) -> i32 {
        self.geo.view.right - self.m.px(SCROLLBAR_W + SCROLLBAR_GAP)
    }

    /// Width of a tile of the scrolling page (`W11_COLS` per row).
    fn tile_w(&self) -> i32 {
        ((self.content_right() - self.geo.view.left) / W11_COLS as i32).max(1)
    }

    /// The `c`-th tile of a tile band at `rc`.
    fn tile_rect(&self, rc: RECT, c: usize) -> RECT {
        let w = self.tile_w();
        rect_wh(rc.left + c as i32 * w, rc.top, w, rc.bottom - rc.top)
    }

    /// Windows 11 style: the "Grid" / "List" button on the "All apps" title at `rc` (none while
    /// searching: the results are always a list).
    fn view_button(&self, rc: RECT) -> Option<RECT> {
        if !self.win11 || self.searching() || self.button_w <= 0 {
            return None;
        }
        let bh = self.m.px(W11_BTN_H);
        Some(rect_wh(self.content_right() - self.button_w, rc.top + (rc.bottom - rc.top - bh) / 2, self.button_w, bh))
    }

    /// Places the bands from `scroll` and rebuilds the hit map: search box, pins (strip or tiles),
    /// rows and app tiles, the title button, the folder shortcuts; on top of all, the open overlay.
    fn rebuild_hits(&mut self) {
        self.placed = place_bands(self.m, &self.bands, self.scroll, self.geo.view);
        let mut pin_cells: Vec<(usize, RECT)> = Vec::new();
        let mut hits: Vec<(u32, RECT)> = vec![(ID_SEARCH, self.geo.search)];
        for (k, rc) in self.geo.strip.iter().enumerate() {
            if self.strip_scroll + k < self.pins.len() {
                pin_cells.push((self.strip_scroll + k, *rc));
            }
        }
        let right = self.content_right();
        for &(b, rc) in &self.placed {
            match self.bands[b] {
                Band::PinsTitle => {}
                Band::Pins(first) => {
                    let n = self.pins.len().saturating_sub(first).min(W11_COLS);
                    pin_cells.extend((0..n).map(|c| (first + c, self.tile_rect(rc, c))));
                }
                Band::AppsTitle => hits.extend(self.view_button(rc).map(|b| (ID_VIEW, b))),
                Band::Row(i) => hits.push((ID_ROW + i as u32, RECT { right, ..rc })),
                Band::Tiles(first, n) => {
                    hits.extend((0..n).map(|c| (ID_ROW + (first + c) as u32, self.tile_rect(rc, c))));
                }
            }
        }
        hits.extend(pin_cells.iter().map(|&(i, rc)| (ID_PIN + i as u32, rc)));
        hits.extend(self.geo.shortcuts.iter().enumerate().map(|(i, rc)| (ID_SHORTCUT + i as u32, *rc)));
        self.pin_cells = pin_cells;
        self.hits.clear();
        for (id, rc) in hits {
            self.hits.add(id, rc);
        }
        if self.over.is_some() {
            // On top of everything (the last added wins): the backdrop, then the overlay.
            self.hits.add(ID_BACKDROP, RECT { left: 0, top: 0, right: self.geo.w, bottom: self.geo.h });
            if let Some(t) = self.over_geo.title {
                self.hits.add(ID_OVER_TITLE, t);
            }
            for (i, rc) in self.over_geo.cells.iter().enumerate() {
                self.hits.add(ID_OVER + i as u32, *rc);
            }
        }
    }

    /// Scrolls until the band of the selection is one of those drawn.
    fn ensure_visible(&mut self) {
        let Some(b) = self.sel.and_then(|s| band_of(&self.bands, s)) else { return };
        if b < self.scroll {
            self.scroll = b;
            return;
        }
        let max = self.max_scroll();
        while self.scroll < max && !place_bands(self.m, &self.bands, self.scroll, self.geo.view).iter().any(|p| p.0 == b) {
            self.scroll += 1;
        }
    }

    /// Id of the app at `idx` in the store.
    fn app_id(idx: usize) -> Option<String> {
        STORE.with(|s| s.try_borrow().ok().and_then(|s| s.apps.get(idx).map(|a| a.id.clone())))
    }

    /// What is behind hit `id` (pin tile or list row).
    fn target_at(&self, id: u32) -> Option<Target> {
        if (ID_PIN..ID_OVER_TITLE).contains(&id) {
            self.pins.get((id - ID_PIN) as usize).map(|t| match *t {
                Tile::App(i) => Target::App(i),
                Tile::Folder(f) => Target::Folder(f),
            })
        } else if id >= ID_ROW {
            self.results.get((id - ID_ROW) as usize).map(Self::item_target)
        } else {
            None
        }
    }

    fn item_target(item: &Item) -> Target {
        match *item {
            Item::App(i) => Target::App(i),
            Item::Setting(i) => Target::Setting(i),
            Item::Letter(_) => Target::Letter,
        }
    }

    /// The text changed: back to the top, the first result selected while searching.
    fn query_changed(&mut self) -> Reply {
        self.scroll = 0;
        self.refresh();
        self.sel = (self.searching() && !self.results.is_empty()).then_some(0);
        Reply::Redraw
    }

    /// Moves the selection by `delta` results (clamped); the first move from no selection lands on
    /// the first result shown.
    fn move_sel(&mut self, delta: i64) -> Reply {
        if self.results.is_empty() {
            return Reply::None;
        }
        let last = self.results.len() as i64 - 1;
        let next = match self.sel {
            Some(s) => (s as i64 + delta).clamp(0, last),
            // Home / End go to the ends whatever is shown.
            None if delta.abs() > last => delta.clamp(0, last),
            None => self
                .placed
                .iter()
                .find_map(|&(b, _)| match self.bands[b] {
                    Band::Row(i) | Band::Tiles(i, _) => Some(i as i64),
                    _ => None,
                })
                .unwrap_or(0),
        };
        self.sel = Some(next as usize);
        self.ensure_visible();
        self.rebuild_hits();
        Reply::Redraw
    }

    /// A click (or Enter) on `t`: an app or a setting opens after the popup closes; a letter opens
    /// the letter grid, a folder tile opens the folder.
    fn activate(&mut self, t: Target) -> Reply {
        let pending = match t {
            Target::App(idx) => Self::app_id(idx).map(Pending::App),
            Target::Setting(i) => {
                start_search::settings().get(i).map(|s| Pending::Setting(s.target.to_string(), s.args.clone()))
            }
            Target::Letter => return self.open_letters(),
            Target::Folder(f) => return self.open_folder(f),
        };
        match pending {
            Some(p) => {
                self.pending = Some(p);
                Reply::Close
            }
            None => Reply::None,
        }
    }

    // -------------------------------------------------------------- painting

    fn paint_search(&self, s: &mut Surface) {
        let m = s.metrics();
        let g = &self.geo;
        let r = m.px(ROW_RADIUS);
        s.fill_rr(g.search, r, WHITE, 16);
        s.stroke_rr(g.search, r, m.px(1).max(1), 34);
        let gs = m.px(16);
        let gx = g.search.left + m.px(12);
        s.glyph(gx, g.search.top + (g.search.bottom - g.search.top - gs) / 2, gs, TEXT2_ALPHA, &magnifier);
        let tx = gx + gs + m.px(10);
        let lh = s.line_height(Font::Normal);
        let ty = center_y(&g.search, lh);
        let max_w = g.search.right - m.px(12) - tx;
        let caret_x = if self.query.is_empty() {
            s.text(labels().placeholder, tx, ty, Font::Normal, WHITE, TEXT3_ALPHA);
            tx
        } else {
            let shown = tail_fit(s, &self.query, max_w);
            s.text(&shown, tx, ty, Font::Normal, WHITE, TEXT_ALPHA);
            tx + s.text_width(&shown, Font::Normal) + m.px(1)
        };
        s.fill_rr(rect_wh(caret_x, ty, m.px(1).max(1), lh), 0, WHITE, 200);
    }

    fn paint_header(s: &mut Surface, rc: RECT, title: &str, hint: Option<&str>) {
        let m = s.metrics();
        s.text(title, rc.left + m.px(4), center_y(&rc, s.line_height(Font::Bold)), Font::Bold, WHITE, TEXT2_ALPHA);
        if let Some(h) = hint {
            let w = s.text_width(h, Font::Small);
            let y = center_y(&rc, s.line_height(Font::Small));
            s.text(h, rc.right - m.px(4) - w, y, Font::Small, WHITE, TEXT3_ALPHA);
        }
    }

    /// The app icon (shrunk to `size`), or a dim placeholder square while it has not arrived.
    fn paint_icon(s: &mut Surface, icons: &HashMap<String, IconBitmap>, id: &str, x: i32, y: i32, size: i32) {
        match icons.get(&id.to_lowercase()) {
            Some(b) => s.blit(b, x, y, size, 255),
            None => s.fill_rr(rect_wh(x, y, size, size), size / 4, WHITE, 26),
        }
    }

    /// The icon of a folder tile: a dim rounded square with the first four apps inside, 2 x 2.
    fn paint_folder_icon(s: &mut Surface, st: &Store, apps: &[usize], x: i32, y: i32, size: i32) {
        s.fill_rr(rect_wh(x, y, size, size), size / 4, WHITE, 26);
        let pad = (size / 10).max(1);
        let mini = (size - 3 * pad) / 2;
        for (k, app) in apps.iter().filter_map(|&i| st.apps.get(i)).take(4).enumerate() {
            let (c, r) = ((k % 2) as i32, (k / 2) as i32);
            Self::paint_icon(s, &st.icons, &app.id, x + pad + c * (mini + pad), y + pad + r * (mini + pad), mini);
        }
    }

    /// A pinned tile (an app or a folder): hover plate, icon `icon_top` below the tile top, the
    /// name under it.
    #[allow(clippy::too_many_arguments)]
    fn paint_tile(s: &mut Surface, st: &Store, rc: &RECT, tile: Tile, id: u32, icon_top: i32, hot: Option<u32>, pressed: Option<u32>) {
        let content = match tile {
            Tile::App(i) => st.apps.get(i).map(|a| (a.name.clone(), vec![i], false)),
            Tile::Folder(f) => folder_content(st, f).map(|(name, apps)| (name, apps, true)),
        };
        let Some((label, apps, folder)) = content else { return };
        let m = s.metrics();
        let ti = m.px(TILE_ICON);
        let lh_small = s.line_height(Font::Small);
        let plate = RECT { left: rc.left + m.px(2), top: rc.top, right: rc.right - m.px(2), bottom: rc.bottom };
        let lit = row_plate(s, plate, id, hot, pressed);
        let tw = rc.right - rc.left;
        let iy = rc.top + icon_top;
        let ix = rc.left + (tw - ti) / 2;
        if folder {
            Self::paint_folder_icon(s, st, &apps, ix, iy, ti);
        } else if let Some(app) = apps.first().and_then(|&i| st.apps.get(i)) {
            Self::paint_icon(s, &st.icons, &app.id, ix, iy, ti);
        }
        let name = s.ellipsize(&label, Font::Small, tw - m.px(10));
        let nx = rc.left + (tw - s.text_width(&name, Font::Small)) / 2;
        let ny = iy + ti + m.px(8);
        debug_assert!(ny + lh_small <= rc.bottom + m.px(4));
        s.text(&name, nx, ny, Font::Small, WHITE, lift(TEXT_ALPHA, lit));
    }

    /// Compact style: the pin strip. Icons only; the name of the one under the pointer goes on the
    /// line below, where two small arrows also tell that the wheel slides the strip.
    fn paint_strip(&self, s: &mut Surface, st: &Store, hot: Option<u32>, pressed: Option<u32>) {
        let g = &self.geo;
        if g.strip.is_empty() {
            return;
        }
        let m = s.metrics();
        let (si, gap) = (m.px(STRIP_ICON), m.px(2));
        for (k, rc) in g.strip.iter().enumerate() {
            let idx = self.strip_scroll + k;
            let Some(app) = self.pins.get(idx).and_then(|t| match *t {
                Tile::App(i) => st.apps.get(i),
                Tile::Folder(_) => None,
            }) else {
                continue;
            };
            let id = ID_PIN + idx as u32;
            let plate = RECT { left: rc.left + gap, top: rc.top + gap, right: rc.right - gap, bottom: rc.bottom - gap };
            row_plate(s, plate, id, hot, pressed);
            let (cw, ch) = (rc.right - rc.left, rc.bottom - rc.top);
            Self::paint_icon(s, &st.icons, &app.id, rc.left + (cw - si) / 2, rc.top + (ch - si) / 2, si);
            if hot == Some(id) {
                // Centred under its icon, kept inside the panel.
                let name = s.ellipsize(&app.name, Font::Small, g.caption.right - g.caption.left);
                let w = s.text_width(&name, Font::Small);
                let x = (rc.left + (cw - w) / 2).min(g.caption.right - w).max(g.caption.left);
                s.text(&name, x, center_y(&g.caption, s.line_height(Font::Small)), Font::Small, WHITE, TEXT2_ALPHA);
            }
        }
        let gs = m.px(8);
        let gy = g.caption.top + (g.caption.bottom - g.caption.top - gs) / 2;
        if self.strip_scroll > 0 {
            s.glyph(g.caption.left, gy, gs, TEXT3_ALPHA, &crate::glyphs::chevron_left_shape);
        }
        if self.strip_scroll + g.strip.len() < self.pins.len() {
            s.glyph(g.caption.right - gs, gy, gs, TEXT3_ALPHA, &crate::glyphs::chevron_right_shape);
        }
    }

    /// Both styles: the pin strip (compact), the bands drawn, a message when there is nothing to
    /// list, the scroll bar and the bottom bar (Windows 11).
    fn paint_body(&self, s: &mut Surface, st: &Store, hot: Option<u32>, pressed: Option<u32>) {
        let m = s.metrics();
        let g = &self.geo;
        let searching = self.searching();
        self.paint_strip(s, st, hot, pressed);
        let right = self.content_right();
        let icon_top = m.px(W11_TILE_ICON_TOP);
        for &(b, rc) in &self.placed {
            match self.bands[b] {
                Band::PinsTitle => {
                    Self::paint_header(s, RECT { right, ..rc }, labels().pinned, Some(labels().drag_hint));
                }
                Band::Pins(first) => {
                    for (c, &tile) in self.pins.iter().skip(first).take(W11_COLS).enumerate() {
                        let id = ID_PIN + (first + c) as u32;
                        Self::paint_tile(s, st, &self.tile_rect(rc, c), tile, id, icon_top, hot, pressed);
                    }
                }
                Band::AppsTitle => {
                    let title = if searching { labels().results } else { labels().all };
                    let button = self.view_button(rc);
                    let head = RECT { right: button.map_or(right, |b| b.left - m.px(8)), ..rc };
                    Self::paint_header(s, head, title, Some(labels().hint));
                    if let Some(b) = button {
                        s.fill_rr(b, m.px(ROW_RADIUS), WHITE, W11_BTN_ALPHA);
                        let lit = row_plate(s, b, ID_VIEW, hot, pressed);
                        let text = if self.grid { labels().view_list } else { labels().view_grid };
                        let tx = b.left + (b.right - b.left - s.text_width(text, Font::Small)) / 2;
                        let ty = center_y(&b, s.line_height(Font::Small));
                        s.text(text, tx, ty, Font::Small, WHITE, lift(TEXT_ALPHA, lit));
                    }
                }
                Band::Row(i) => self.paint_row(s, st, RECT { right, ..rc }, i, hot, pressed),
                Band::Tiles(first, n) => {
                    for c in 0..n {
                        let Some(&Item::App(idx)) = self.results.get(first + c) else { continue };
                        let cell = self.tile_rect(rc, c);
                        if self.sel == Some(first + c) {
                            let plate = RECT { left: cell.left + m.px(2), right: cell.right - m.px(2), ..cell };
                            s.plate(plate, m.px(ROW_RADIUS), HOVER_ALPHA);
                        }
                        Self::paint_tile(s, st, &cell, Tile::App(idx), ID_ROW + (first + c) as u32, icon_top, hot, pressed);
                    }
                }
            }
        }
        if !st.loaded || self.results.is_empty() {
            // Still loading, or nothing matches: said under whatever was drawn.
            let msg = if st.loaded { labels().none } else { labels().loading };
            let top = self.placed.last().map_or(g.view.top, |p| p.1.bottom);
            let x = g.view.left + (right - g.view.left - s.text_width(msg, Font::Normal)) / 2;
            s.text(msg, x, top + m.px(24), Font::Normal, WHITE, TEXT3_ALPHA);
        }
        // Scroll bar: the share of the bands drawn, at the share of the way down.
        let max = self.max_scroll();
        if max > 0 && !self.placed.is_empty() {
            let sbw = m.px(SCROLLBAR_W).max(2);
            let track = RECT { left: g.view.right - sbw, ..g.view };
            let total = track.bottom - track.top;
            s.fill_rr(track, sbw / 2, WHITE, 14);
            let thumb = (total as i64 * self.placed.len() as i64 / self.bands.len().max(1) as i64).max(m.px(24) as i64) as i32;
            let ty = track.top + ((total - thumb) as i64 * self.scroll.min(max) as i64 / max as i64) as i32;
            s.fill_rr(rect_wh(track.left, ty, sbw, thumb), sbw / 2, WHITE, 90);
        }
        self.paint_bar(s, hot, pressed);
    }

    /// One list row at `rc`: `results[i]` (an app, a setting or a letter heading), lit when
    /// hovered or selected with the keyboard.
    fn paint_row(&self, s: &mut Surface, st: &Store, rc: RECT, i: usize, hot: Option<u32>, pressed: Option<u32>) {
        let Some(&item) = self.results.get(i) else { return };
        let m = s.metrics();
        let (ri, rpad) = (m.px(ROW_ICON), m.px(ROW_PAD));
        let mut lit = row_plate(s, rc, ID_ROW + i as u32, hot, pressed);
        if !lit && self.sel == Some(i) {
            s.plate(rc, m.px(ROW_RADIUS), HOVER_ALPHA);
            lit = true;
        }
        let (ix, iy) = (rc.left + rpad, rc.top + (rc.bottom - rc.top - ri) / 2);
        let catalog = start_search::settings();
        let name: &str = match item {
            // A letter heading: the letter alone (a click opens the letter grid).
            Item::Letter(c) => {
                let y = center_y(&rc, s.line_height(Font::Bold));
                s.text(&c.to_string(), ix + m.px(4), y, Font::Bold, WHITE, lift(TEXT2_ALPHA, lit));
                return;
            }
            Item::App(idx) => {
                let Some(app) = st.apps.get(idx) else { return };
                Self::paint_icon(s, &st.icons, &app.id, ix, iy, ri);
                &app.name
            }
            // A setting: a gear where the app icon goes.
            Item::Setting(idx) => {
                let Some(setting) = catalog.get(idx) else { return };
                let gs = m.px(16);
                s.glyph(ix + (ri - gs) / 2, iy + (ri - gs) / 2, gs, lift(TEXT2_ALPHA, lit), &crate::glyphs::gear_shape);
                &setting.name
            }
        };
        let tx = rc.left + rpad + ri + m.px(12);
        let name = s.ellipsize(name, Font::Normal, rc.right - rpad - tx);
        s.text(&name, tx, center_y(&rc, s.line_height(Font::Normal)), Font::Normal, WHITE, lift(TEXT_ALPHA, lit));
    }

    /// Windows 11 style: separator, user circle with the initial, user name, the folder shortcuts
    /// (a glyph each; the label of the one under the pointer shows on their left).
    fn paint_bar(&self, s: &mut Surface, hot: Option<u32>, pressed: Option<u32>) {
        let Some(bar) = self.geo.bar else { return };
        let m = s.metrics();
        hline(s, bar.left, bar.right, bar.top);
        let inset = m.px(W11_INSET);
        let d = m.px(W11_AVATAR);
        let circle = rect_wh(inset, bar.top + (bar.bottom - bar.top - d) / 2, d, d);
        s.fill_rr(circle, d / 2, WHITE, 40);
        let initial: String = self.user.chars().next().map(|c| c.to_uppercase().collect()).unwrap_or_default();
        if !initial.is_empty() {
            let x = circle.left + (d - s.text_width(&initial, Font::Bold)) / 2;
            s.text(&initial, x, center_y(&circle, s.line_height(Font::Bold)), Font::Bold, WHITE, TEXT_ALPHA);
        }
        let first = self.geo.shortcuts.first().map_or(bar.right - inset, |r| r.left);
        let nx = circle.right + m.px(12);
        let gs = m.px(16);
        let mut label_w = 0;
        for (i, (rc, sc)) in self.geo.shortcuts.iter().zip(&SHORTCUTS).enumerate() {
            let id = ID_SHORTCUT + i as u32;
            let lit = row_plate(s, *rc, id, hot, pressed);
            let (gx, gy) = (rc.left + (rc.right - rc.left - gs) / 2, rc.top + (rc.bottom - rc.top - gs) / 2);
            s.glyph(gx, gy, gs, lift(TEXT2_ALPHA, lit), &sc.0);
            if hot == Some(id) {
                let label = tr(sc.2, sc.3);
                label_w = s.text_width(label, Font::Small) + m.px(12);
                let y = center_y(&bar, s.line_height(Font::Small));
                s.text(label, first - label_w, y, Font::Small, WHITE, TEXT2_ALPHA);
            }
        }
        let name = s.ellipsize(&self.user, Font::Normal, first - label_w - m.px(12) - nx);
        s.text(&name, nx, center_y(&bar, s.line_height(Font::Normal)), Font::Normal, WHITE, TEXT_ALPHA);
    }

    /// A raised rounded panel over the menu: soft shadow, opaque base in the panel colour, a lift
    /// and an outline.
    fn paint_raised(s: &mut Surface, panel: RECT) {
        let m = s.metrics();
        let r = m.px(FLY_RADIUS);
        let g = m.px(2).max(1);
        let shadow = RECT { left: panel.left - g, top: panel.top - g, right: panel.right + g, bottom: panel.bottom + 2 * g };
        s.fill_rr(shadow, r + g, (0, 0, 0), 120);
        s.fill_rr(panel, r, crate::config::get().background.rgb(), 255);
        s.fill_rr(panel, r, WHITE, FLY_LIFT_ALPHA);
        s.stroke_rr(panel, r, m.px(1).max(1), FLY_RIM_ALPHA);
    }

    /// The open overlay: context menu rows, the letter grid, or the tiles of a folder under its name.
    fn paint_over(&self, s: &mut Surface, st: &Store, hot: Option<u32>, pressed: Option<u32>) {
        let Some(over) = &self.over else { return };
        let m = s.metrics();
        let g = &self.over_geo;
        Self::paint_raised(s, g.panel);
        match over {
            Over::Menu { rows, .. } => {
                let (pad, lh) = (m.px(12), s.line_height(Font::Normal));
                for (i, (rc, (_, label))) in g.cells.iter().zip(rows).enumerate() {
                    let lit = row_plate(s, *rc, ID_OVER + i as u32, hot, pressed);
                    let label = s.ellipsize(label, Font::Normal, rc.right - rc.left - 2 * pad);
                    s.text(&label, rc.left + pad, center_y(rc, lh), Font::Normal, WHITE, lift(TEXT_ALPHA, lit));
                }
            }
            Over::Letters(letters) => {
                let (gap, lh) = (m.px(2), s.line_height(Font::Bold));
                for (i, (rc, (c, _))) in g.cells.iter().zip(letters).enumerate() {
                    let plate = RECT { left: rc.left + gap, top: rc.top + gap, right: rc.right - gap, bottom: rc.bottom - gap };
                    let lit = row_plate(s, plate, ID_OVER + i as u32, hot, pressed);
                    let t = c.to_string();
                    let x = rc.left + (rc.right - rc.left - s.text_width(&t, Font::Bold)) / 2;
                    s.text(&t, x, center_y(rc, lh), Font::Bold, WHITE, lift(TEXT_ALPHA, lit));
                }
            }
            Over::Folder { name, apps, .. } => {
                match (g.title, &self.rename) {
                    // Being renamed: a text field with what was typed and a caret.
                    (Some(t), Some(text)) => {
                        let r = m.px(ROW_RADIUS);
                        s.fill_rr(t, r, WHITE, 16);
                        s.stroke_rr(t, r, m.px(1).max(1), 34);
                        let lh = s.line_height(Font::Normal);
                        let (tx, ty) = (t.left + m.px(6), center_y(&t, lh));
                        let shown = tail_fit(s, text, t.right - t.left - m.px(14));
                        s.text(&shown, tx, ty, Font::Normal, WHITE, TEXT_ALPHA);
                        let cx = tx + s.text_width(&shown, Font::Normal) + m.px(1);
                        s.fill_rr(rect_wh(cx, ty, m.px(1).max(1), lh), 0, WHITE, 200);
                    }
                    // The name: a click on it renames the folder.
                    (Some(t), None) => {
                        row_plate(s, t, ID_OVER_TITLE, hot, pressed);
                        Self::paint_header(s, t, name, None);
                    }
                    (None, _) => {}
                }
                for (i, (rc, &idx)) in g.cells.iter().zip(apps).enumerate() {
                    Self::paint_tile(s, st, rc, Tile::App(idx), ID_OVER + i as u32, m.px(10), hot, pressed);
                }
            }
        }
    }

    /// A drag in progress: where the tile would land (an outline on the tile it would be grouped
    /// with, or a bar on the side it would move to) and the tile's icon under the pointer.
    fn paint_drag(&self, s: &mut Surface, st: &Store) {
        let Some(d) = self.drag.filter(|d| d.active) else { return };
        let m = s.metrics();
        let folder = match &self.over {
            Some(Over::Folder { apps, .. }) => Some(apps),
            _ => None,
        };
        let pins: Vec<RECT> = self.pin_cells.iter().map(|c| c.1).collect();
        let cells = if folder.is_some() { &self.over_geo.cells } else { &pins };
        if let Some((_, zone)) = self.drag_zone() {
            let (DropZone::Into(k) | DropZone::Before(k) | DropZone::After(k)) = zone;
            if let Some(rc) = cells.get(k).copied() {
                match zone {
                    DropZone::Into(_) => {
                        s.plate(rc, m.px(ROW_RADIUS), HOVER_ALPHA);
                        s.stroke_rr(rc, m.px(ROW_RADIUS), m.px(1).max(1), 130);
                    }
                    _ => {
                        let w = m.px(2).max(2);
                        let x = if zone == DropZone::After(k) { rc.right } else { rc.left } - w / 2;
                        s.fill_rr(rect_wh(x, rc.top + m.px(8), w, rc.bottom - rc.top - m.px(16)), w / 2, WHITE, 210);
                    }
                }
            }
        }
        let tile = match folder {
            Some(apps) => d.id.checked_sub(ID_OVER).and_then(|i| apps.get(i as usize)).map(|&i| Tile::App(i)),
            None => d.id.checked_sub(ID_PIN).and_then(|i| self.pins.get(i as usize)).copied(),
        };
        let ti = m.px(TILE_ICON);
        let (x, y) = (d.pos.0 - ti / 2, d.pos.1 - ti / 2);
        match tile {
            Some(Tile::App(i)) => {
                if let Some(app) = st.apps.get(i) {
                    Self::paint_icon(s, &st.icons, &app.id, x, y, ti);
                }
            }
            Some(Tile::Folder(f)) => {
                if let Some((_, apps)) = folder_content(st, f) {
                    Self::paint_folder_icon(s, st, &apps, x, y, ti);
                }
            }
            None => {}
        }
    }

}

impl Content for StartMenu {
    fn kind(&self) -> PopupKind {
        PopupKind::StartMenu
    }

    fn limit(&mut self, max: (i32, i32)) {
        self.max = max;
    }

    fn size(&mut self, s: &Surface) -> (i32, i32) {
        self.m = s.metrics();
        if self.win11 {
            // One width for both labels: the button does not move when it is clicked.
            let l = labels();
            let text = s.text_width(l.view_grid, Font::Small).max(s.text_width(l.view_list, Font::Small));
            self.button_w = text + 2 * self.m.px(W11_BTN_PAD);
        }
        if !self.requested {
            self.requested = true;
            request_catalog(self.m.px(STORE_ICON).max(8) as u32);
        }
        self.refresh();
        // PREVIEW-HOOK: the overlay of a preview, opened the way the user would.
        match self.preview_over.take() {
            Some(PreviewOver::Menu) => {
                self.button(ID_PIN, Button::Right);
            }
            Some(PreviewOver::Letters) => {
                self.open_letters();
            }
            Some(PreviewOver::Folder) => {
                if let Some(i) = self.pins.iter().position(|t| matches!(t, Tile::Folder(_))) {
                    self.dropped = false;
                    self.click(ID_PIN + i as u32);
                }
            }
            None => {}
        }
        (self.geo.w, self.geo.h)
    }

    fn paint(&self, s: &mut Surface, hot: Option<u32>, pressed: Option<u32>) {
        s.panel();
        self.paint_search(s);
        STORE.with(|st| match st.try_borrow() {
            Ok(st) => {
                self.paint_body(s, &st, hot, pressed);
                self.paint_over(s, &st, hot, pressed);
                self.paint_drag(s, &st);
            }
            Err(_) => log!("startmenu: store busy while painting"),
        });
    }

    fn hit(&self, x: i32, y: i32) -> Option<u32> {
        self.hits.at(x, y)
    }

    fn clickable(&self, id: u32) -> bool {
        id != ID_SEARCH && id != ID_BACKDROP
    }

    fn click(&mut self, id: u32) -> Reply {
        if std::mem::take(&mut self.dropped) {
            // The release that ended a drag: not a click on the tile it ended over.
            return Reply::None;
        }
        if id == ID_OVER_TITLE {
            // The name of the open folder: type a new one (Enter saves, Esc cancels).
            if let Some(Over::Folder { name, .. }) = &self.over {
                self.rename = Some(name.clone());
            }
            return Reply::Redraw;
        }
        if self.over.is_some() {
            // A row / cell of the overlay; anywhere else closes only the overlay.
            return if (ID_OVER..ID_ROW).contains(&id) { self.over_click((id - ID_OVER) as usize) } else { self.close_over() };
        }
        if id == ID_VIEW {
            // "All apps" as a grid / as a list: remembered in the config, shown from the top.
            if let Err(e) = crate::config::modify(|c| c.start_grid = !c.start_grid) {
                log!("startmenu: cannot save the view: {e}");
            }
            self.scroll = 0;
            self.sel = None;
            self.refresh();
            return Reply::Redraw;
        }
        if let Some(sc) = id.checked_sub(ID_SHORTCUT).and_then(|i| SHORTCUTS.get(i as usize)) {
            // A folder shortcut of the bottom bar: opened after the popup closes.
            self.pending = Some(Pending::Setting(sc.1.to_string(), None));
            return Reply::Close;
        }
        match self.target_at(id) {
            Some(t) => self.activate(t),
            None => Reply::None,
        }
    }

    fn button(&mut self, id: u32, which: Button) -> Reply {
        if which != Button::Right {
            return self.close_over();
        }
        // Right click: the context menu of what is under the pointer (pin, folders, run as
        // administrator, ...), next to it.
        let anchor = self.hits.rect(id).unwrap_or_default();
        if let Some(Over::Folder { apps, .. }) = &self.over {
            // An app inside the open folder: its menu replaces the folder.
            let inside = id.checked_sub(ID_OVER).and_then(|i| apps.get(i as usize)).copied();
            if let Some(idx) = inside.filter(|_| id < ID_ROW) {
                return self.open_menu(idx, anchor);
            }
        }
        if self.over.is_some() {
            return self.close_over();
        }
        match self.target_at(id) {
            Some(Target::App(idx)) => self.open_menu(idx, anchor),
            Some(Target::Folder(f)) => {
                let l = labels();
                let rows = vec![(MenuAct::Rename(f), l.rename.to_string()), (MenuAct::Ungroup(f), l.ungroup.to_string())];
                self.open_over(Over::Menu { id: String::new(), rows }, anchor)
            }
            _ => Reply::None,
        }
    }

    /// A pinned tile (in the grid, or inside the open folder) follows the pointer once it moved
    /// `DRAG_MIN` from the press; `release` drops it.
    fn drag(&mut self, id: u32, x: i32, y: i32) -> Reply {
        let draggable = match &self.over {
            None => (ID_PIN..ID_OVER_TITLE).contains(&id),
            Some(Over::Folder { .. }) => (ID_OVER..ID_ROW).contains(&id) && self.rename.is_none(),
            Some(_) => false,
        };
        if !draggable {
            self.drag = None;
            return Reply::None;
        }
        let min = self.m.px(DRAG_MIN);
        match self.drag.as_mut().filter(|d| d.id == id) {
            Some(d) => {
                d.pos = (x, y);
                d.active |= (x - d.start.0).abs() >= min || (y - d.start.1).abs() >= min;
                if d.active {
                    Reply::Redraw
                } else {
                    Reply::None
                }
            }
            None => {
                // A new press.
                self.dropped = false;
                self.drag = Some(Drag { id, start: (x, y), pos: (x, y), active: false });
                Reply::None
            }
        }
    }

    fn release(&mut self, _id: u32) -> Reply {
        let zone = self.drag_zone();
        match self.drag.take() {
            Some(d) if d.active => {
                self.dropped = true;
                self.finish_drag(d, zone)
            }
            _ => Reply::None,
        }
    }

    fn wheel(&mut self, notches: i32, x: i32, y: i32) -> Reply {
        if self.over.is_some() || notches == 0 {
            // The menu behind an overlay stays still.
            return Reply::None;
        }
        let g = &self.geo;
        let over_strip = g.strip.first().is_some_and(|f| y >= f.top && y < g.caption.bottom && x >= 0 && x < g.w);
        if over_strip {
            // Compact style: the wheel over the pin strip slides it, one pin per notch.
            let max = self.pins.len().saturating_sub(g.strip.len()) as i64;
            let next = (self.strip_scroll as i64 - notches as i64).clamp(0, max) as usize;
            if next == self.strip_scroll {
                return Reply::None;
            }
            self.strip_scroll = next;
            self.rebuild_hits();
            return Reply::Redraw;
        }
        // Whole bands, at least `WHEEL_PX` of them per notch.
        let (max, step) = (self.max_scroll(), self.m.px(WHEEL_PX));
        let mut next = self.scroll;
        for _ in 0..notches.unsigned_abs() {
            let mut moved = 0;
            while moved < step {
                if notches > 0 && next > 0 {
                    next -= 1;
                } else if notches < 0 && next < max {
                    next += 1;
                } else {
                    break;
                }
                moved += band_h(self.m, self.bands[next]);
            }
        }
        if next == self.scroll {
            return Reply::None;
        }
        self.scroll = next;
        self.rebuild_hits();
        Reply::Redraw
    }

    fn key(&mut self, vk: u16) -> Option<Reply> {
        if let Some(text) = self.renaming() {
            // A folder name is being typed: Enter saves it, Esc gives up (the folder stays open).
            return Some(match vk {
                VK_BACK => {
                    text.pop();
                    Reply::Redraw
                }
                VK_RETURN => self.commit_rename(),
                VK_ESCAPE => {
                    self.rename = None;
                    Reply::Redraw
                }
                _ => Reply::None,
            });
        }
        if self.over.is_some() {
            // Esc closes only the overlay (handled: the Start stays open); other keys wait for it.
            return Some(if vk == VK_ESCAPE { self.close_over() } else { Reply::None });
        }
        let page = self.placed.len().max(1) as i64;
        Some(match vk {
            VK_DOWN => self.move_sel(1),
            VK_UP => self.move_sel(-1),
            VK_NEXT => self.move_sel(page),
            VK_PRIOR => self.move_sel(-page),
            VK_HOME => self.move_sel(i64::MIN / 2),
            VK_END => self.move_sel(i64::MAX / 2),
            VK_RETURN => {
                let target = self.sel.or((self.searching() && !self.results.is_empty()).then_some(0));
                match target.and_then(|i| self.results.get(i)).map(Self::item_target) {
                    Some(t) => self.activate(t),
                    None => Reply::None,
                }
            }
            VK_BACK => {
                if self.query.pop().is_none() {
                    return Some(Reply::None);
                }
                self.query_changed()
            }
            // Esc first clears the search; with an empty search it closes (not handled here).
            VK_ESCAPE if !self.query.is_empty() => {
                self.query.clear();
                self.query_changed()
            }
            _ => return None,
        })
    }

    fn char(&mut self, c: char) -> Reply {
        if let Some(text) = self.renaming() {
            if c.is_control() || text.chars().count() >= RENAME_MAX {
                return Reply::None;
            }
            text.push(c);
            return Reply::Redraw;
        }
        if c.is_control() || (c.is_whitespace() && self.query.is_empty()) || self.query.chars().count() >= MAX_QUERY {
            return Reply::None;
        }
        // Typing closes the overlay and goes on searching.
        self.over = None;
        self.query.push(c);
        self.query_changed()
    }

    fn data(&mut self, ev: &DataEvent) -> Reply {
        if !matches!(ev, DataEvent::StartMenuChanged) {
            return Reply::None;
        }
        self.refresh();
        Reply::Redraw
    }

    fn closing(&mut self) {
        self.over = None;
        self.rename = None;
        self.drag = None;
    }

    fn after_close(&mut self) {
        crate::popup::arm_start_release();
        match self.pending.take() {
            Some(Pending::App(id)) => {
                log!("startmenu: launching '{id}'");
                crate::ui::launch_target_str(&format!("shell:AppsFolder\\{id}"));
            }
            Some(Pending::Setting(target, args)) => {
                log!("startmenu: opening the setting '{target}' {args:?}");
                shell_thread::request(ShellRequest::Launch { target, args });
            }
            Some(Pending::Admin(id)) => shell_thread::request(ShellRequest::LaunchAppAdmin { id }),
            Some(Pending::Reveal(id)) => shell_thread::request(ShellRequest::RevealApp { id }),
            Some(Pending::Uninstall) => crate::ui::launch_target_str("ms-settings:appsfeatures"),
            None => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const M: Metrics = Metrics { dpi: 96 };

    #[test]
    fn releasing_frees_the_store_and_late_events_are_dropped() {
        let apps = vec![
            ("a".to_string(), "Alpha".to_string(), IconBitmap::new(32, 32)),
            ("b".to_string(), "Beta".to_string(), IconBitmap::new(32, 32)),
        ];
        drop(preview_new(apps, "", StartStyle::Compact, None));
        assert_eq!(release_store(), 2 * 32 * 32 * 4);
        STORE.with(|s| {
            let s = s.borrow();
            assert!(s.apps.is_empty() && s.icons.is_empty() && !s.loaded && !s.open);
        });
        store_apps(vec![StartApp { id: "x".into(), name: "X".into() }]);
        assert!(!store_icons(32, vec![("x".into(), Some(IconBitmap::new(32, 32)))]), "closed menu ignores late icons");
        STORE.with(|s| assert!(s.borrow().apps.is_empty()));
    }

    fn named(lower: &str) -> Cand<'_> {
        Cand { lower, keys: Keys::none(), rank: 0 }
    }

    #[test]
    fn search_ranks_prefix_then_word_then_substring() {
        let names =
            ["adobe reader", "calculator", "git bash", "notepad", "visual studio code", "windows terminal"].map(named);
        // "ca": prefix of calculator only.
        assert_eq!(filter(&names, "Ca"), vec![1]);
        // "te": word start of "terminal" (tier 1) before substrings ("notepad", tier 2).
        assert_eq!(filter(&names, "te"), vec![5, 3]);
        // Every word must match, in any order.
        assert_eq!(filter(&names, "code vis"), vec![4]);
        assert!(filter(&names, "zzz").is_empty());
        // Empty (or blank) query: everything, in the original order.
        assert_eq!(filter(&names, "  "), (0..names.len()).collect::<Vec<_>>());
        assert_eq!(rank(&named("visual studio code"), "visual"), Some((0, 0)));
        assert_eq!(rank(&named("visual studio code"), "studio"), Some((1, 0)));
        assert_eq!(rank(&named("visual studio code"), "udio"), Some((2, 0)));
        // Equal matches: the more popular app (lower rank) first, whatever the alphabet says.
        let pop = [Cand { rank: 900, ..named("calendar") }, Cand { rank: 100, ..named("calculator") }];
        assert_eq!(filter(&pop, "cal"), vec![1, 0]);
    }

    #[test]
    fn search_finds_synonyms() {
        let rdp = Keys::from(&[("mstsc", 2518), ("rdp", 2210), ("remtoe", 7740)][..]);
        let lol = Keys::from(&[("lol", 3435), ("r", 9117)][..]);
        let apps = [
            Cand { keys: &rdp, ..named("connessione desktop remoto") },
            Cand { keys: &lol, ..named("league of legends") },
            named("trolololo"),
        ];
        assert_eq!(filter(&apps, "rdp"), vec![0]);
        assert_eq!(filter(&apps, "mstsc"), vec![0]);
        // A synonym equal to the query (tier 1) before a substring of a name (tier 2).
        assert_eq!(filter(&apps, "LoL"), vec![1, 2]);
        // A synonym only starting with the query comes last (tier 3) and needs 2+ chars.
        assert_eq!(filter(&apps, "lo"), vec![2, 1]);
        assert_eq!(rank(&apps[1], "x"), None);
        assert_eq!(rank(&Cand { keys: &lol, ..named("zzz") }, "l"), None);
        // The score of a synonym match is its penalty.
        assert_eq!(rank(&apps[0], "rdp"), Some((1, 2210)));
        assert_eq!(rank(&apps[0], "ms"), Some((3, 2518)));
    }

    #[test]
    fn letters_head_the_list_and_folders_take_the_place_of_their_first_app() {
        use Item::{App, Letter};
        assert_eq!(
            with_letters(&['#', 'A', 'A', 'B']),
            vec![Letter('#'), App(0), Letter('A'), App(1), App(2), Letter('B'), App(3)]
        );
        assert!(with_letters(&[]).is_empty());
        assert_eq!((letter_of("\u{e0}baco"), letter_of("7zip"), letter_of(""), letter_of("zed")), ('\u{c0}', '#', '#', 'Z'));
        // (catalog index, folder) of each pin: not installed -> nothing; a folder shows once.
        let pins = [(Some(3), None), (Some(1), Some(0)), (None, None), (Some(2), Some(0)), (Some(5), Some(1)), (None, Some(2))];
        assert_eq!(pin_tiles(&pins), vec![Tile::App(3), Tile::Folder(0), Tile::Folder(1)]);
    }

    #[test]
    fn overlays_stay_inside_the_panel() {
        let inside = |g: &OverGeo, w: i32, h: i32| {
            let p = g.panel;
            assert!(p.left >= 0 && p.top >= 0 && p.right <= w && p.bottom <= h, "{p:?} in {w}x{h}");
            for c in &g.cells {
                assert!(c.left >= p.left && c.right <= p.right && c.top >= p.top && c.bottom <= p.bottom, "{c:?} in {p:?}");
            }
        };
        for dpi in [96, 144, 192] {
            let m = Metrics { dpi };
            let g = layout_compact(m, 0, UNCAPPED);
            // Under the control; above it when the control is at the bottom of the panel.
            let row = rect_wh(m.px(14), m.px(100), m.px(300), m.px(38));
            let under = layout_menu(m, g.w, g.h, row, 6);
            assert_eq!(under.cells.len(), 6);
            assert!(under.panel.top >= row.top && under.panel.top <= row.bottom);
            inside(&under, g.w, g.h);
            let low = RECT { top: g.h - m.px(50), bottom: g.h - m.px(12), ..row };
            let above = layout_menu(m, g.w, g.h, low, 6);
            assert!(above.panel.bottom <= low.bottom && above.panel.bottom >= low.top);
            inside(&above, g.w, g.h);
            // Near the right edge the menu moves left.
            inside(&layout_menu(m, g.w, g.h, rect_wh(g.w - m.px(40), m.px(100), m.px(30), m.px(30)), 3), g.w, g.h);
            let cell = (m.px(LETTER_CELL), m.px(LETTER_CELL));
            let grid = layout_grid(m, g.w, g.h, 27, LETTER_COLS, cell, false);
            assert_eq!(grid.cells.len(), 27);
            assert!(grid.title.is_none());
            assert_eq!(grid.cells[LETTER_COLS].left, grid.cells[0].left);
            assert_eq!(grid.cells[LETTER_COLS].top, grid.cells[0].bottom);
            inside(&grid, g.w, g.h);
            let folder = layout_grid(m, g.w, g.h, 5, FOLDER_COLS, (m.px(FOLDER_TILE_W), m.px(TILE_H)), true);
            let title = folder.title.unwrap();
            assert!(title.bottom <= folder.cells[0].top && title.top >= folder.panel.top);
            inside(&folder, g.w, g.h);
        }
        // A panel too small: columns and rows that do not fit are dropped.
        let small = layout_grid(M, 200, 150, 27, 6, (44, 44), true);
        assert!(!small.cells.is_empty() && small.cells.len() < 27);
        inside(&small, 200, 150);
        inside(&layout_menu(M, 100, 120, rect_wh(60, 80, 36, 36), 2), 100, 120);
    }

    #[test]
    fn context_menu_stays_small_whatever_the_folders() {
        use crate::config::{Config, StartFolder};
        let acts = |id: &str, cfg: &Config| menu_rows(id, cfg).into_iter().map(|r| r.0).collect::<Vec<_>>();
        let mut cfg = Config::default();
        // A Store app (`!`) has no file location.
        assert_eq!(acts("a", &cfg), vec![MenuAct::Pin, MenuAct::Admin, MenuAct::Location, MenuAct::Uninstall]);
        assert_eq!(acts("pkg!App", &cfg), vec![MenuAct::Pin, MenuAct::Admin, MenuAct::Uninstall]);
        // Twenty folders: an app in one gets a single extra row, the others none.
        cfg.start_pins = vec!["a".into(), "b".into()];
        cfg.start_folders = (0..20).map(|i| StartFolder { name: format!("F{i}"), apps: vec![format!("x{i}")] }).collect();
        cfg.start_folders[3].apps.push("a".into());
        assert_eq!(
            acts("a", &cfg),
            vec![MenuAct::Pin, MenuAct::LeaveFolder, MenuAct::Admin, MenuAct::Location, MenuAct::Uninstall]
        );
        assert_eq!(acts("b", &cfg).len(), 4);
        assert!(new_folder_name(&cfg).ends_with(" 21"));
    }

    #[test]
    fn drop_zones_group_in_the_middle_and_move_at_the_sides() {
        let cells = [rect_wh(0, 0, 80, 80), rect_wh(80, 0, 80, 80), rect_wh(160, 0, 80, 80)];
        // An app dragged in the grid: the middle half of another tile groups, its sides move.
        assert_eq!(drop_zone(&cells, 0, 120, 40, true), Some(DropZone::Into(1)));
        assert_eq!(drop_zone(&cells, 0, 85, 40, true), Some(DropZone::Before(1)));
        assert_eq!(drop_zone(&cells, 0, 155, 40, true), Some(DropZone::After(1)));
        assert_eq!(drop_zone(&cells, 0, 239, 79, true), Some(DropZone::After(2)));
        // A folder being moved, or an app inside a folder: halves only, never "into".
        assert_eq!(drop_zone(&cells, 0, 110, 40, false), Some(DropZone::Before(1)));
        assert_eq!(drop_zone(&cells, 0, 130, 40, false), Some(DropZone::After(1)));
        // Over itself or outside every tile: nothing.
        assert_eq!(drop_zone(&cells, 1, 120, 40, true), None);
        assert_eq!(drop_zone(&cells, 0, 120, 90, true), None);
        assert_eq!(drop_zone(&[], 0, 1, 1, true), None);
    }

    #[test]
    fn a_press_becomes_a_drag_only_after_moving_and_its_release_is_not_a_click() {
        let apps = vec![("a".to_string(), "Alpha".to_string(), IconBitmap::new(32, 32))];
        drop(preview_new(apps, "", StartStyle::Compact, None));
        let mut c = make();
        c.requested = true;
        c.refresh();
        // A press and a small move: still a click.
        assert_eq!(c.drag(ID_PIN, 100, 100), Reply::None);
        assert_eq!(c.drag(ID_PIN, 103, 102), Reply::None);
        assert!(c.drag.is_some_and(|d| !d.active));
        assert_eq!(c.release(ID_PIN), Reply::None);
        assert!(c.drag.is_none() && !c.dropped);
        // Past the threshold the tile follows the pointer; the release drops it (here on nothing)
        // and the click that the popup sends when it ends over the same tile is swallowed once.
        c.drag(ID_PIN, 100, 100);
        assert_eq!(c.drag(ID_PIN, 100, 100 + DRAG_MIN), Reply::Redraw);
        assert!(c.drag.is_some_and(|d| d.active));
        assert_eq!(c.release(ID_PIN), Reply::Redraw);
        assert!(c.drag.is_none() && c.dropped);
        assert_eq!(c.click(ID_ROW + 1), Reply::None);
        assert_eq!(c.click(ID_ROW + 1), Reply::Close);
        // A new press forgets a drop whose click never came; list rows are not draggable.
        c.pending = None;
        c.dropped = true;
        c.drag(ID_PIN, 10, 10);
        assert!(!c.dropped);
        assert_eq!(c.drag(ID_ROW + 1, 10, 10), Reply::None);
        assert!(c.drag.is_none());
        release_store();
    }

    #[test]
    fn right_click_opens_the_menu_and_a_letter_opens_the_grid() {
        let apps = vec![
            ("a".to_string(), "Alpha".to_string(), IconBitmap::new(32, 32)),
            ("pkg!App".to_string(), "Beta".to_string(), IconBitmap::new(32, 32)),
        ];
        drop(preview_new(apps, "", StartStyle::Compact, None));
        let mut c = make();
        c.requested = true;
        c.refresh();
        assert_eq!(c.results, vec![Item::Letter('A'), Item::App(0), Item::Letter('B'), Item::App(1)]);
        // Right click on "Alpha": the menu, over a backdrop.
        assert_eq!(c.button(ID_ROW + 1, Button::Right), Reply::Redraw);
        let Some(Over::Menu { id, rows }) = c.over.clone() else { panic!("no menu: {:?}", c.over) };
        assert_eq!((id.as_str(), rows.len()), ("a", 4));
        let rc = c.over_geo.cells[1];
        assert_eq!(c.hit(rc.left + 2, rc.top + 2), Some(ID_OVER + 1));
        assert_eq!(c.hit(c.geo.search.left + 2, c.geo.search.top + 2), Some(ID_BACKDROP));
        // Esc is handled (the Start stays open) and closes only the menu; so does a click elsewhere.
        assert_eq!(c.key(VK_ESCAPE), Some(Reply::Redraw));
        assert!(c.over.is_none());
        assert_eq!(c.hit(c.geo.search.left + 2, c.geo.search.top + 2), Some(ID_SEARCH));
        c.button(ID_ROW + 1, Button::Right);
        assert_eq!(c.click(ID_BACKDROP), Reply::Redraw);
        assert!(c.over.is_none() && c.pending.is_none());
        // "Run as administrator" (second row of a Store app) waits for the popup to close.
        c.button(ID_ROW + 3, Button::Right);
        assert_eq!(c.click(ID_OVER + 1), Reply::Close);
        assert_eq!(c.pending, Some(Pending::Admin("pkg!App".into())));
        c.pending = None;
        c.refresh();
        // A letter heading opens the grid; a letter there scrolls the list to its heading.
        assert_eq!(c.click(ID_ROW), Reply::Redraw);
        assert_eq!(c.over, Some(Over::Letters(vec![('A', 0), ('B', 2)])));
        assert_eq!(c.click(ID_OVER + 1), Reply::Redraw);
        assert!(c.over.is_none() && c.sel.is_none());
        // A left click on an app launches it after the close; a right click on a letter does nothing.
        assert_eq!(c.button(ID_ROW, Button::Right), Reply::None);
        assert_eq!(c.click(ID_ROW + 1), Reply::Close);
        assert_eq!(c.pending, Some(Pending::App("a".into())));
        release_store();
    }

    fn inside(r: &RECT, w: i32, h: i32) -> bool {
        r.left >= 0 && r.top >= 0 && r.right <= w && r.bottom <= h && r.right > r.left && r.bottom > r.top
    }

    #[test]
    fn compact_layout_has_a_pin_strip_and_the_list() {
        let none = layout_compact(M, 0, UNCAPPED);
        let some = layout_compact(M, 3, UNCAPPED);
        let many = layout_compact(M, 40, UNCAPPED);
        // Without pins there is no strip and the list starts right under the search box.
        assert!(none.strip.is_empty() && none.view.top == none.search.bottom + GAP);
        assert_eq!(some.strip.len(), 3);
        // As many pins as fit in one row; the list starts under the caption line.
        assert_eq!(many.strip.len(), ((W - 2 * INSET) / STRIP_CELL) as usize);
        assert!(many.view.top >= many.caption.bottom && many.caption.top == many.strip[0].bottom);
        assert!(none.view.top < some.view.top && some.view.top == many.view.top);
        for g in [&none, &some, &many] {
            assert_eq!((g.w, g.h), (W, H));
            assert!(g.bar.is_none() && g.shortcuts.is_empty());
            assert!(inside(&g.search, g.w, g.h) && inside(&g.view, g.w, g.h));
            assert!(g.strip.iter().all(|c| inside(c, g.w, g.h) && c.bottom <= g.view.top));
        }
        let big = layout_compact(Metrics { dpi: 144 }, 2, UNCAPPED);
        assert_eq!((big.w, big.h), (W * 3 / 2, H * 3 / 2));
    }

    #[test]
    fn windows11_layout_has_a_page_and_a_bar_with_shortcuts() {
        for dpi in [96, 144, 192] {
            let m = Metrics { dpi };
            let g = layout_win11(m, UNCAPPED);
            assert_eq!((g.w, g.h), (m.px(W11_W), m.px(W11_H)));
            let bar = g.bar.unwrap();
            assert!(g.strip.is_empty());
            assert!(g.view.top > g.search.bottom && g.view.bottom <= bar.top && bar.bottom == g.h);
            // One shortcut per entry, side by side at the right end of the bar, inside it.
            assert_eq!(g.shortcuts.len(), SHORTCUTS.len());
            for (i, r) in g.shortcuts.iter().enumerate() {
                assert!(inside(r, g.w, g.h) && r.top >= bar.top && r.bottom <= bar.bottom, "dpi {dpi}: {r:?}");
                assert!(i == 0 || r.left == g.shortcuts[i - 1].right);
            }
            assert_eq!(g.shortcuts.last().unwrap().right, g.w - m.px(W11_INSET));
        }
    }

    #[test]
    fn bands_are_titles_pin_rows_and_list_rows_or_app_tiles() {
        use Band::{AppsTitle, Pins, PinsTitle, Row, Tiles};
        use Item::{App, Letter, Setting};
        let list = [Letter('A'), App(0), App(1), App(2), Letter('B'), App(3)];
        // Compact: one row per result, nothing else.
        assert_eq!(make_bands(5, &list, 6, false, false), (0..6).map(Row).collect::<Vec<_>>());
        // Windows 11: "Pinned" with 6 tiles per row (8 pins: two rows), then "All apps" as a list.
        let mut want = vec![PinsTitle, Pins(0), Pins(6), AppsTitle];
        want.extend((0..6).map(Row));
        assert_eq!(make_bands(8, &list, 6, false, true), want);
        // Nothing pinned: no "Pinned" section.
        assert_eq!(make_bands(0, &[], 6, false, true), vec![AppsTitle]);
        // As a grid: the letters stay rows, the apps under each go `cols` per row.
        assert_eq!(make_bands(0, &list, 2, true, true), vec![AppsTitle, Row(0), Tiles(1, 2), Tiles(3, 1), Row(4), Tiles(5, 1)]);
        // A setting is never a tile.
        assert_eq!(make_bands(0, &[App(0), Setting(0), App(1)], 6, true, false), vec![Tiles(0, 1), Row(1), Tiles(2, 1)]);
        let bands = make_bands(0, &list, 2, true, true);
        assert_eq!((band_of(&bands, 0), band_of(&bands, 2), band_of(&bands, 3), band_of(&bands, 5)), (Some(1), Some(2), Some(3), Some(5)));
        assert_eq!(band_of(&bands, 9), None);
    }

    #[test]
    fn only_whole_bands_are_placed_and_the_last_one_can_be_reached() {
        let results: Vec<Item> = (0..40).map(Item::App).collect();
        let bands = make_bands(8, &results, 6, false, true);
        let g = layout_win11(M, UNCAPPED);
        let view_h = g.view.bottom - g.view.top;
        let top = place_bands(M, &bands, 0, g.view);
        // From the top: the title, the two pin rows, the second title, then rows; all inside.
        assert_eq!(top[0].0, 0);
        assert_eq!((top[0].1.top, top[1].1.top), (g.view.top, g.view.top + HEADER_H));
        assert_eq!(top[1].1.bottom - top[1].1.top, W11_TILE_H);
        assert!(top.iter().all(|(_, r)| r.top >= g.view.top && r.bottom <= g.view.bottom));
        assert!(top.windows(2).all(|p| p[0].1.bottom == p[1].1.top && p[0].0 + 1 == p[1].0));
        // One more band would not fit.
        assert!(top.last().unwrap().1.bottom + band_h(M, bands[top.len()]) > g.view.bottom);
        // At the largest scroll the last band is drawn; one step earlier it is not.
        let max = max_band_scroll(M, &bands, view_h);
        assert!(max > 0 && max < bands.len());
        assert_eq!(place_bands(M, &bands, max, g.view).last().unwrap().0, bands.len() - 1);
        assert!(place_bands(M, &bands, max - 1, g.view).last().unwrap().0 < bands.len() - 1);
        // Everything fits: nothing to scroll.
        assert_eq!(max_band_scroll(M, &bands[..3], view_h), 0);
        assert_eq!(max_band_scroll(M, &[], view_h), 0);
        assert!(place_bands(M, &[], 0, g.view).is_empty());
    }

    #[test]
    fn windows11_page_scrolls_pins_and_apps_together_and_has_no_power_button() {
        let apps: Vec<_> =
            (0..30).map(|i| (format!("id{i:02}"), format!("App {i:02}"), IconBitmap::new(32, 32))).collect();
        drop(preview_new(apps, "", StartStyle::Windows11, None));
        let mut c = make();
        c.win11 = true;
        c.requested = true;
        c.button_w = 60;
        c.refresh();
        // No pins in the default config: the page starts with "All apps" and its view button.
        assert_eq!(c.bands[0], Band::AppsTitle);
        let title = c.placed[0].1;
        let b = c.view_button(title).unwrap();
        assert_eq!(c.hit(b.left + 2, b.top + 2), Some(ID_VIEW));
        // Rows: the letter "A", then the apps; a click on an app launches it after the close.
        assert_eq!(c.results[0], Item::Letter('A'));
        let row = c.hits.rect(ID_ROW + 1).unwrap();
        assert!(row.top >= title.bottom && row.right <= c.geo.view.right);
        // The wheel moves whole bands, never past the ends.
        assert_eq!(c.wheel(1, 100, 300), Reply::None);
        assert_eq!(c.wheel(-1, 100, 300), Reply::Redraw);
        assert!(c.scroll > 0 && c.placed[0].0 == c.scroll);
        for _ in 0..50 {
            c.wheel(-1, 100, 300);
        }
        assert_eq!(c.scroll, c.max_scroll());
        assert_eq!(c.placed.last().unwrap().0, c.bands.len() - 1);
        assert_eq!(c.wheel(-1, 100, 300), Reply::None);
        // The keyboard selection scrolls back to what it selects.
        assert_eq!(c.key(VK_HOME), Some(Reply::Redraw));
        assert_eq!((c.sel, c.scroll <= 1), (Some(0), true));
        // The folder shortcuts: each one is a hit in the bar and closes the menu with its target.
        for (i, sc) in SHORTCUTS.iter().enumerate() {
            let r = c.geo.shortcuts[i];
            assert_eq!(c.hit(r.left + 2, r.top + 2), Some(ID_SHORTCUT + i as u32));
            assert_eq!(c.click(ID_SHORTCUT + i as u32), Reply::Close);
            assert_eq!(c.pending.take(), Some(Pending::Setting(sc.1.to_string(), None)));
        }
        // Searching: results only (a list), no view button.
        c.query = "app 0".into();
        c.query_changed();
        assert!(c.view_button(c.placed[0].1).is_none());
        assert!(c.bands[1..].iter().all(|b| matches!(b, Band::Row(_))) && c.sel == Some(0));
        release_store();
    }

    #[test]
    fn compact_strip_shows_every_pin_and_slides_with_the_wheel() {
        let mut c = make();
        c.requested = true;
        c.pins = (0..20).map(Tile::App).collect();
        c.geo = layout_compact(M, c.pins.len(), UNCAPPED);
        let shown = c.geo.strip.len();
        c.rebuild_hits();
        // The pins shown are hits with their own index; ids map straight to `pins`.
        assert_eq!(c.pin_cells.len(), shown);
        let last = c.geo.strip[shown - 1];
        assert_eq!(c.hit(last.left + 2, last.top + 2), Some(ID_PIN + shown as u32 - 1));
        assert_eq!(c.target_at(ID_PIN + 4), Some(Target::App(4)));
        // The wheel over the strip slides it by one pin, clamped; elsewhere it does not.
        let (x, y) = (last.left + 2, last.top + 2);
        assert_eq!(c.wheel(-1, x, y), Reply::Redraw);
        assert_eq!((c.strip_scroll, c.hit(x, y)), (1, Some(ID_PIN + shown as u32)));
        for _ in 0..40 {
            c.wheel(-1, x, y);
        }
        assert_eq!(c.strip_scroll, 20 - shown);
        assert_eq!(c.wheel(-1, x, y), Reply::None);
        assert_eq!(c.wheel(1, x, c.geo.view.top + 5), Reply::None);
        assert_eq!(c.strip_scroll, 20 - shown);
        // Grouping is a Windows 11 thing: in the strip a drop only moves.
        c.strip_scroll = 0;
        c.rebuild_hits();
        let (a, b) = (c.geo.strip[0], c.geo.strip[2]);
        c.drag(ID_PIN, a.left + 20, a.top + 20);
        c.drag(ID_PIN, (b.left + b.right) / 2 - 1, b.top + 20);
        assert_eq!(c.drag_zone(), Some((0, DropZone::Before(2))));
        c.win11 = true;
        assert_eq!(c.drag_zone(), Some((0, DropZone::Into(2))));
        c.pins[0] = Tile::Folder(0);
        assert_eq!(c.drag_zone(), Some((0, DropZone::Before(2))));
    }

    #[test]
    fn magnifier_has_ring_and_handle() {
        assert!(magnifier(6.5, 1.9)); // top of the ring
        assert!(!magnifier(6.5, 6.5)); // hollow centre
        assert!(magnifier(12.0, 12.0)); // handle
        assert!(!magnifier(1.0, 15.0));
    }

    #[test]
    fn query_tail_is_kept_when_too_long() {
        let s = Surface::new(M).expect("surface");
        let long = "a very long search text that does not fit";
        let t = tail_fit(&s, long, 60);
        assert!(long.ends_with(&t) && !t.is_empty() && s.text_width(&t, Font::Normal) <= 60);
        assert_eq!(tail_fit(&s, "abc", 500), "abc");
    }

    #[test]
    fn start_menu_fits_a_short_work_area() {
        let m = Metrics { dpi: 192 }; // 200 %
        let max = (m.px(W11_W) - 60, 1008); // 1080p work area minus margins
        for g in [layout_compact(m, 12, max), layout_win11(m, max)] {
            assert!(g.w <= max.0 && g.h <= max.1, "{g:?}");
            assert!(inside(&g.search, g.w, g.h) && inside(&g.view, g.w, g.h));
            assert!(g.strip.iter().all(|c| inside(c, g.w, g.h)));
            assert!(g.bar.is_none_or(|b| b.bottom == g.h && g.view.bottom <= b.top));
            // A few rows are still listed.
            assert!((g.view.bottom - g.view.top) / m.px(ROW_H) >= 3);
        }
        // Uncapped sizes are unchanged.
        assert_eq!(layout_compact(M, 12, UNCAPPED).h, H);
        assert_eq!(layout_win11(M, UNCAPPED).h, W11_H);
    }
}
