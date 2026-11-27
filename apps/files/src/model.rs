//! The details view's model (P3-files-app 2.2; FA-MODEL fills it, F1a.2-F1a.4): the Listing seam over the engine's
//! EntryStore and the window's fakes, and FilesTable, the kit TableModel the VirtualTable reads.

use std::any::Any;
use std::borrow::Cow;
use std::collections::HashMap;
use std::rc::Rc;
use std::sync::Arc;

use oled_ui::gfx::image::IconBitmap;
use oled_ui::model::{
    Align, Badge, CellKind, CellOut, ColId, FgRole, IconRef, ListModel, RowKey, Selection, TableModel,
};
use oled_ui::text::Trim;
use oled_ui::theme::TypeRole;
use oled_ui::UiLang;

use crate::format::{Fmt, LocalTime};
use crate::listing::collate::wtf8_to_utf16;
use crate::listing::{
    rescan_merge, Anchor, ApplyStats, Batch, Diff, Entry, EntryId, EntryStore, EnumError, Gen, GroupBy, IdMap, SortDir,
    SortDone, SortJob, SortKey, Visibility, WidePath,
};
use crate::tr::{gen, k, t};
use crate::types::TypeNames;

/// A folder's entries as the window reads them: EntryStore in the app, fakes::FakeListing in tests.
#[allow(clippy::len_without_is_empty)] // the frozen interface has len() only
pub trait Listing {
    fn gen(&self) -> Gen;
    fn done(&self) -> bool;
    fn error(&self) -> Option<EnumError>;
    fn dir(&self) -> &WidePath;
    fn order(&self) -> &[EntryId];
    fn row_of(&self, id: EntryId) -> Option<u32>;
    fn entry(&self, id: EntryId) -> Option<&Entry>;
    fn name(&self, id: EntryId) -> Option<&[u8]>;
    fn ext(&self, id: EntryId) -> Option<&[u8]>;
    fn is_dir(&self, id: EntryId) -> bool;
    fn find(&self, name: &[u8]) -> Option<EntryId>;
    fn len(&self) -> u32;
    fn heap_bytes(&self) -> usize;
    fn append(&mut self, b: Batch);
    fn merge_pending(&mut self) -> bool;
    fn apply(&mut self, d: &Diff, anchor: Option<Anchor>) -> (ApplyStats, Option<Anchor>);
    fn sort(&mut self, key: SortKey, dir: SortDir);
    fn sort_job(&self, key: SortKey, dir: SortDir) -> SortJob;
    fn adopt(&mut self, done: SortDone);
    fn set_visibility(&mut self, v: Visibility);
    fn needs_compact(&self) -> bool;
    fn compact(&mut self) -> IdMap;
    /// Overflow rescan, tab revalidation, the navigation cache: by name, survivors keep their ids.
    fn merge_fresh(&mut self, fresh: Box<dyn Listing>) -> ApplyStats;
    fn into_any(self: Box<Self>) -> Box<dyn Any>;
}

#[allow(clippy::len_without_is_empty)] // the frozen interface has len() only
impl Listing for EntryStore {
    fn gen(&self) -> Gen {
        EntryStore::gen(self)
    }
    fn done(&self) -> bool {
        EntryStore::done(self)
    }
    fn error(&self) -> Option<EnumError> {
        EntryStore::error(self)
    }
    fn dir(&self) -> &WidePath {
        EntryStore::dir(self)
    }
    fn order(&self) -> &[EntryId] {
        &EntryStore::order(self).0
    }
    fn row_of(&self, id: EntryId) -> Option<u32> {
        EntryStore::row_of(self, id)
    }
    fn entry(&self, id: EntryId) -> Option<&Entry> {
        EntryStore::entry(self, id)
    }
    fn name(&self, id: EntryId) -> Option<&[u8]> {
        EntryStore::name(self, id)
    }
    fn ext(&self, id: EntryId) -> Option<&[u8]> {
        EntryStore::ext(self, id)
    }
    fn is_dir(&self, id: EntryId) -> bool {
        EntryStore::is_dir(self, id)
    }
    fn find(&self, name: &[u8]) -> Option<EntryId> {
        EntryStore::find(self, name)
    }
    fn len(&self) -> u32 {
        EntryStore::len(self)
    }
    fn heap_bytes(&self) -> usize {
        EntryStore::heap_bytes(self)
    }
    fn append(&mut self, b: Batch) {
        EntryStore::append(self, b);
    }
    fn merge_pending(&mut self) -> bool {
        EntryStore::merge_pending(self)
    }
    fn apply(&mut self, d: &Diff, anchor: Option<Anchor>) -> (ApplyStats, Option<Anchor>) {
        EntryStore::apply(self, d, anchor)
    }
    fn sort(&mut self, key: SortKey, dir: SortDir) {
        EntryStore::sort(self, key, dir, GroupBy::None);
    }
    fn sort_job(&self, key: SortKey, dir: SortDir) -> SortJob {
        EntryStore::sort_job(self, key, dir, GroupBy::None)
    }
    fn adopt(&mut self, done: SortDone) {
        EntryStore::adopt(self, done);
    }
    fn set_visibility(&mut self, v: Visibility) {
        EntryStore::set_visibility(self, v);
    }
    fn needs_compact(&self) -> bool {
        EntryStore::needs_compact(self)
    }
    fn compact(&mut self) -> IdMap {
        EntryStore::compact(self)
    }
    fn merge_fresh(&mut self, fresh: Box<dyn Listing>) -> ApplyStats {
        match fresh.into_any().downcast::<EntryStore>() {
            Ok(f) => rescan_merge(self, *f),
            Err(_) => {
                debug_assert!(false, "merge_fresh: an EntryStore merges only another EntryStore");
                ApplyStats::default()
            }
        }
    }
    fn into_any(self: Box<Self>) -> Box<dyn Any> {
        self
    }
}

/// ColId(kind as u16).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[repr(u16)]
pub enum ColKind {
    Name = 0,
    Modified = 1,
    Type = 2,
    Size = 3,
    Created = 4,
    Attributes = 5,
    Status = 6,
}

#[derive(Clone, Debug, PartialEq)]
pub struct ViewOpts {
    pub show_extensions: bool,
    pub sizes_like_explorer: bool,
    pub full_dates: bool,
    pub compact: bool,
}

impl Default for ViewOpts {
    /// files.json's defaults (FilesOptions::default).
    fn default() -> ViewOpts {
        ViewOpts { show_extensions: true, sizes_like_explorer: false, full_dates: false, compact: false }
    }
}

/// An icon the table holds: a class icon by extension, an item's own icon, or the folder icon.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum IconKey {
    Class(Box<[u8]>),
    Item(EntryId),
    Folder,
}

/// File attribute bits and reparse tags the cells read (attributes only: no file is ever opened).
mod attr {
    pub const READONLY: u32 = 0x1;
    pub const HIDDEN: u32 = 0x2;
    pub const SYSTEM: u32 = 0x4;
    pub const DIRECTORY: u32 = 0x10;
    pub const ARCHIVE: u32 = 0x20;
    pub const PINNED: u32 = 0x8_0000;
    pub const UNPINNED: u32 = 0x10_0000;
    pub const RECALL_ON_DATA_ACCESS: u32 = 0x40_0000;
    pub const TAG_MOUNT_POINT: u32 = 0xA000_0003;
    pub const TAG_SYMLINK: u32 = 0xA000_000C;
}

/// Icon slots: 0 is the folder icon, 1 the generic file glyph (the class icon of an empty extension).
const SLOT_FOLDER: u32 = 0;
const SLOT_GENERIC: u32 = 1;
/// Per-item icons kept beyond this many are dropped oldest first.
const MAX_ITEM_ICONS: usize = 512;
/// A name longer than this many characters gets the full name as its tip.
const TIP_NAME_CHARS: usize = 40;

struct IconSlot {
    bmp: IconBitmap,
    stamp: u64,
}

/// The table's own icon table: only set_icon fills it, cell() only reads it.
struct IconTable {
    slots: Vec<Option<IconSlot>>,
    class: HashMap<Box<[u8]>, u32>,
    item: HashMap<EntryId, u32>,
    free: Vec<u32>,
    stamp: u64,
}

impl IconTable {
    fn new() -> IconTable {
        IconTable { slots: vec![None, None], class: HashMap::new(), item: HashMap::new(), free: Vec::new(), stamp: 0 }
    }

    fn bitmap(&self, slot: u32) -> Option<&IconBitmap> {
        self.slots.get(slot as usize)?.as_ref().map(|s| &s.bmp)
    }

    fn filled(&self, slot: u32) -> bool {
        self.bitmap(slot).is_some()
    }

    fn alloc(&mut self) -> u32 {
        if let Some(s) = self.free.pop() {
            return s;
        }
        self.slots.push(None);
        (self.slots.len() - 1) as u32
    }

    /// Frees every per-item icon.
    fn drop_items(&mut self) {
        for (_, slot) in self.item.drain() {
            if let Some(s) = self.slots.get_mut(slot as usize) {
                *s = None;
            }
            self.free.push(slot);
        }
    }

    /// Drops the oldest per-item icon.
    fn evict_oldest_item(&mut self) {
        let oldest = self
            .item
            .iter()
            .filter_map(|(id, &slot)| self.slots.get(slot as usize)?.as_ref().map(|s| (s.stamp, *id, slot)))
            .min_by_key(|&(stamp, _, _)| stamp);
        if let Some((_, id, slot)) = oldest {
            self.item.remove(&id);
            if let Some(s) = self.slots.get_mut(slot as usize) {
                *s = None;
            }
            self.free.push(slot);
        }
    }

    /// True if the stored bitmap changed.
    fn put(&mut self, key: IconKey, bmp: IconBitmap) -> bool {
        self.stamp += 1;
        let stamp = self.stamp;
        let slot = match key {
            IconKey::Folder => SLOT_FOLDER,
            IconKey::Class(ext) if ext.is_empty() => SLOT_GENERIC,
            IconKey::Class(ext) => {
                let mut lower = ext.into_vec();
                lower.make_ascii_lowercase();
                match self.class.get(lower.as_slice()) {
                    Some(&s) => s,
                    None => {
                        let s = self.alloc();
                        self.class.insert(lower.into_boxed_slice(), s);
                        s
                    }
                }
            }
            IconKey::Item(id) => match self.item.get(&id) {
                Some(&s) => s,
                None => {
                    if self.item.len() >= MAX_ITEM_ICONS {
                        self.evict_oldest_item();
                    }
                    let s = self.alloc();
                    self.item.insert(id, s);
                    s
                }
            },
        };
        let Some(cell) = self.slots.get_mut(slot as usize) else { return false };
        if cell.as_ref().is_some_and(|s| s.bmp == bmp) {
            return false;
        }
        *cell = Some(IconSlot { bmp, stamp });
        true
    }
}

/// The kit TableModel over a Listing: names, dates, types and sizes as the formatters write them.
pub struct FilesTable {
    listing: Box<dyn Listing>,
    fmt: Rc<Fmt>,
    types: Rc<TypeNames>,
    opts: ViewOpts,
    icons: IconTable,
    now: LocalTime,
    gen: u64,
    lang: UiLang,
    /// Moves every time the per-item icons are dropped.
    item_epoch: u64,
}

impl FilesTable {
    /// The table speaks English until set_lang says otherwise.
    pub fn new(
        listing: Box<dyn Listing>,
        fmt: Rc<Fmt>,
        types: Rc<TypeNames>,
        opts: ViewOpts,
        now: LocalTime,
    ) -> FilesTable {
        FilesTable { listing, fmt, types, opts, icons: IconTable::new(), now, gen: 0, lang: UiLang::EN, item_epoch: 0 }
    }

    pub fn listing(&self) -> &dyn Listing {
        &*self.listing
    }

    /// Counts as a change: the generation moves on every call.
    pub fn listing_mut(&mut self) -> &mut dyn Listing {
        self.bump();
        &mut *self.listing
    }

    /// Swaps the listing; the one that was shown is handed back.
    pub fn replace(&mut self, l: Box<dyn Listing>) -> Box<dyn Listing> {
        let old = std::mem::replace(&mut self.listing, l);
        // Entry ids belong to one listing: no per-item icon outlives it.
        self.drop_item_icons();
        old
    }

    /// Frees every per-item icon (their keys are entry ids, which mean something only in the listing they came from).
    pub fn drop_item_icons(&mut self) {
        self.icons.drop_items();
        self.item_epoch = self.item_epoch.wrapping_add(1);
        self.bump();
    }

    /// Moves every time the per-item icons are dropped.
    pub fn item_epoch(&self) -> u64 {
        self.item_epoch
    }

    /// Any visible change the listing does not count (types, icons, options, the clock).
    pub fn bump(&mut self) {
        self.gen = self.gen.wrapping_add(1);
    }

    pub fn set_opts(&mut self, o: ViewOpts) {
        self.opts = o;
        self.bump();
    }

    pub fn set_now(&mut self, now: LocalTime) {
        self.now = now;
        self.bump();
    }

    /// The number and date formats (a language switch replaces them).
    pub fn set_fmt(&mut self, fmt: Rc<Fmt>) {
        self.fmt = fmt;
        self.bump();
    }

    /// The language of the type names, the status words and the attribute column.
    pub fn set_lang(&mut self, lang: UiLang) {
        self.lang = lang;
        self.bump();
    }

    /// Stores the icon (premultiplied BGRA, px x px) once, as premultiplied 0xAARRGGBB pixels. True if the stored
    /// bitmap changed, so rows that show it must be painted again.
    pub fn set_icon(&mut self, ext_or_id: IconKey, px: u16, bgra: Arc<[u8]>) -> bool {
        let n = usize::from(px) * usize::from(px);
        if px == 0 || bgra.len() < n * 4 {
            return false;
        }
        let pixels: Vec<u32> =
            bgra.chunks_exact(4).take(n).map(|c| u32::from_le_bytes([c[0], c[1], c[2], c[3]])).collect();
        let bmp = IconBitmap { width: u32::from(px), height: u32::from(px), pixels };
        let changed = self.icons.put(ext_or_id, bmp);
        if changed {
            self.bump();
        }
        changed
    }

    /// The row's full path: the folder joined with the name; None for an unknown key or a name no path can hold.
    pub fn path_of(&self, k: RowKey) -> Option<WidePath> {
        let name = self.listing.name(id_of(k))?;
        let mut units = Vec::with_capacity(name.len());
        wtf8_to_utf16(name, &mut units);
        self.listing.dir().join(&units).ok()
    }

    /// Model order.
    pub fn selected_paths(&self, s: &Selection) -> Vec<WidePath> {
        let mut out = Vec::new();
        s.for_each(self, &mut |_, k| out.extend(self.path_of(k)));
        out
    }

    fn icon_slot(&self, id: EntryId, is_dir: bool) -> Option<u32> {
        if let Some(&s) = self.icons.item.get(&id) {
            return Some(s);
        }
        let slot = if is_dir {
            SLOT_FOLDER
        } else {
            let ext = self.listing.ext(id).unwrap_or_default();
            match self.icons.class.get(ext) {
                Some(&s) if self.icons.filled(s) => s,
                _ => SLOT_GENERIC,
            }
        };
        self.icons.filled(slot).then_some(slot)
    }

    fn badge_of(e: &Entry) -> Option<Badge> {
        if e.reparse_tag == attr::TAG_MOUNT_POINT || e.reparse_tag == attr::TAG_SYMLINK {
            Some(Badge::Link)
        } else if e.attrs & (attr::RECALL_ON_DATA_ACCESS | attr::UNPINNED) != 0 {
            Some(Badge::CloudOnline)
        } else if e.attrs & attr::PINNED != 0 {
            Some(Badge::CloudPinned)
        } else {
            None
        }
    }

    /// "{EXT} File" ("File {EXT}" in Italian) or "File", written without a temporary string.
    fn push_type_fallback(&self, out: &mut String, ext: &[u8]) {
        if ext.is_empty() || self.lang.pseudo {
            out.push_str(&crate::types::TypeNames::fallback(ext, self.lang));
            return;
        }
        let template = gen::TABLE.get(k::FILES_TYPE_EXT, self.lang.italian);
        let (before, after) = template.split_once("{ext}").unwrap_or((template, ""));
        out.push_str(before);
        match std::str::from_utf8(ext) {
            Ok(s) => s.chars().for_each(|c| out.extend(c.to_uppercase())),
            Err(_) => push_wtf8_lossy(out, ext),
        }
        out.push_str(after);
    }

    fn name_cell(&self, id: EntryId, e: &Entry, name: &[u8], out: &mut CellOut) {
        let is_dir = e.attrs & attr::DIRECTORY != 0;
        let dot = if is_dir { None } else { ext_dot(name) };
        out.kind = CellKind::Text;
        out.role = TypeRole::Body;
        out.fg = FgRole::Primary;
        out.trim = Trim::Middle { tail: 10 };
        match dot {
            Some(d) if !self.opts.show_extensions => push_wtf8_lossy(&mut out.text, &name[..d]),
            _ => push_wtf8_lossy(&mut out.text, name),
        }
        let len = out.text.len() as u32;
        match dot {
            Some(d) if self.opts.show_extensions => {
                // The dot is ASCII and the lossy writer keeps every byte length, so the offset is the same.
                out.push_span(0, d as u32, FgRole::Primary).push_span(d as u32, len, FgRole::Secondary);
            }
            _ => {
                out.push_span(0, len, FgRole::Primary);
            }
        }
        out.icon = self.icon_slot(id, is_dir).map(IconRef);
        out.dim_icon = e.attrs & (attr::HIDDEN | attr::SYSTEM) != 0;
        out.badge = Self::badge_of(e);
        if out.text.chars().count() > TIP_NAME_CHARS {
            let mut full = String::with_capacity(name.len());
            push_wtf8_lossy(&mut full, name);
            out.tip = Some(Cow::Owned(full));
        }
    }

    fn type_cell(&self, id: EntryId, e: &Entry, out: &mut CellOut) {
        out.kind = CellKind::Text;
        out.fg = FgRole::Secondary;
        if e.attrs & attr::DIRECTORY != 0 {
            out.text.push_str(&t(k::FILES_TYPE_FOLDER, self.lang));
            return;
        }
        let ext = self.listing.ext(id).unwrap_or_default();
        match if ext.is_empty() { None } else { self.types.get(ext) } {
            Some(name) => out.text.push_str(&name),
            None => self.push_type_fallback(&mut out.text, ext),
        }
    }

    fn attributes_cell(e: &Entry, out: &mut CellOut) {
        out.kind = CellKind::Text;
        out.fg = FgRole::Secondary;
        for (bit, letter) in [(attr::READONLY, 'R'), (attr::HIDDEN, 'H'), (attr::SYSTEM, 'S'), (attr::ARCHIVE, 'A')] {
            if e.attrs & bit != 0 {
                if !out.text.is_empty() {
                    out.text.push(' ');
                }
                out.text.push(letter);
            }
        }
    }

    fn status_cell(&self, e: &Entry, out: &mut CellOut) {
        out.kind = CellKind::Text;
        out.fg = FgRole::Secondary;
        if e.attrs & (attr::RECALL_ON_DATA_ACCESS | attr::UNPINNED) != 0 {
            out.text.push_str(&t(k::FILES_COL_STATUS_ONLINE, self.lang));
        } else if e.attrs & attr::PINNED != 0 {
            out.text.push_str(&t(k::FILES_COL_STATUS_LOCAL, self.lang));
        }
    }
}

/// The index of the dot that starts the extension: the last '.', unless it is the first byte or the last.
fn ext_dot(name: &[u8]) -> Option<usize> {
    let d = name.iter().rposition(|&b| b == b'.')?;
    (d > 0 && d + 1 < name.len()).then_some(d)
}

/// Appends WTF-8 as UTF-8: a lone surrogate (three bytes) and any other invalid sequence become U+FFFD, the first
/// kind keeping its three bytes, so offsets in the source stay valid in the text.
pub(crate) fn push_wtf8_lossy(out: &mut String, mut s: &[u8]) {
    loop {
        match std::str::from_utf8(s) {
            Ok(v) => {
                out.push_str(v);
                return;
            }
            Err(e) => {
                let (good, rest) = s.split_at(e.valid_up_to());
                out.push_str(std::str::from_utf8(good).unwrap_or_default());
                out.push('\u{FFFD}');
                let lone_surrogate = matches!(rest, [0xED, 0xA0..=0xBF, 0x80..=0xBF, ..]);
                let skip = if lone_surrogate { 3 } else { e.error_len().unwrap_or(rest.len()).max(1) };
                s = rest.get(skip..).unwrap_or_default();
            }
        }
    }
}

fn kind_of(col: ColId) -> Option<ColKind> {
    Some(match col.0 {
        0 => ColKind::Name,
        1 => ColKind::Modified,
        2 => ColKind::Type,
        3 => ColKind::Size,
        4 => ColKind::Created,
        5 => ColKind::Attributes,
        6 => ColKind::Status,
        _ => return None,
    })
}

/// RowKey(id.0 as u64).
pub fn key_of(id: EntryId) -> RowKey {
    RowKey(u64::from(id.0))
}

/// EntryId(k.0 as u32).
pub fn id_of(k: RowKey) -> EntryId {
    EntryId(k.0 as u32)
}

impl ListModel for FilesTable {
    fn len(&self) -> usize {
        self.listing.order().len()
    }

    fn key(&self, row: usize) -> RowKey {
        self.listing.order().get(row).map_or(RowKey(u64::MAX), |&id| key_of(id))
    }

    fn row_of(&self, key: RowKey) -> Option<usize> {
        self.listing.row_of(id_of(key)).map(|r| r as usize)
    }

    /// The name as stored, extension included (the accessible name and the type-ahead text).
    fn label(&self, row: usize) -> Cow<'_, str> {
        let id = self.listing.order().get(row).copied();
        id.and_then(|id| self.listing.name(id)).map_or(Cow::Borrowed(""), String::from_utf8_lossy)
    }

    fn generation(&self) -> u64 {
        (self.listing.gen().0 << 32) ^ self.gen
    }
}

impl TableModel for FilesTable {
    fn cell(&self, row: usize, col: ColId, out: &mut CellOut) {
        out.clear();
        let Some(&id) = self.listing.order().get(row) else { return };
        let (Some(e), Some(name), Some(kind)) = (self.listing.entry(id), self.listing.name(id), kind_of(col)) else {
            return;
        };
        match kind {
            ColKind::Name => self.name_cell(id, e, name, out),
            ColKind::Modified | ColKind::Created => {
                let ft = if kind == ColKind::Modified { e.mtime } else { e.ctime };
                out.kind = CellKind::Text;
                out.fg = FgRole::Secondary;
                self.fmt.modified_into(&mut out.text, ft, self.now, self.opts.full_dates);
            }
            ColKind::Type => self.type_cell(id, e, out),
            ColKind::Size => {
                out.kind = CellKind::Num;
                out.align = Align::End;
                out.fg = FgRole::Secondary;
                if e.attrs & attr::DIRECTORY == 0 {
                    self.fmt.size_into(&mut out.text, e.size, self.opts.sizes_like_explorer);
                }
            }
            ColKind::Attributes => Self::attributes_cell(e, out),
            ColKind::Status => self.status_cell(e, out),
        }
    }

    fn icon(&self, icon: IconRef, _px: i32) -> Option<&IconBitmap> {
        self.icons.bitmap(icon.0)
    }
}
