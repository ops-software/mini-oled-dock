//! Data models (F-K2a, P2-kit 2.8): what lists and tables read, cell output, row keys and selection.

use std::borrow::Cow;

use crate::command::CommandId;
use crate::geom::PointP;
use crate::node::Text;
use crate::text::Trim;
use crate::theme::TypeRole;
use crate::w3::IconBitmap;

/// A row's stable key (files: EntryId; tasks: hash of (pid, create time)).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct RowKey(pub u64);

/// A column's id.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct ColId(pub u16);

/// What a list reads.
#[allow(clippy::len_without_is_empty)]
pub trait ListModel {
    fn len(&self) -> usize;
    /// Stable across sort and insert.
    fn key(&self, row: usize) -> RowKey;
    /// O(1) or O(log n): called per kept key on every model change.
    fn row_of(&self, key: RowKey) -> Option<usize>;
    /// The accessible name and the type-ahead text.
    fn label(&self, row: usize) -> Cow<'_, str>;
    /// Changes whenever len, order or any visible value changes.
    fn generation(&self) -> u64;
}

/// What a table reads: the only way rows are drawn (X30): apps never paint rows.
pub trait TableModel: ListModel {
    fn cell(&self, row: usize, col: ColId, out: &mut CellOut);
    fn row(&self, row: usize, out: &mut RowOut) {
        let _ = (row, out);
    }
    fn tree(&self, row: usize) -> Option<TreeInfo> {
        let _ = row;
        None
    }
    fn icon(&self, icon: IconRef, px: i32) -> Option<&IconBitmap> {
        let _ = (icon, px);
        None
    }
    fn spark(&self, row: usize, col: ColId) -> Option<&[f32]> {
        let _ = (row, col);
        None
    }
    /// How the row is painted now (a slide and a fade); asked once per visible row at each paint.
    fn motion(&self, row: usize) -> RowMotion {
        let _ = row;
        RowMotion::NONE
    }
}

/// A row's paint-time motion. Hit testing and the accessibility rects ignore it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RowMotion {
    /// Logical px added to the painted y (a slide).
    pub dy: i16,
    /// 255 = opaque; scales the alpha of every ink of the row: plate, text, icon, heat, note.
    pub opacity: u8,
}

impl RowMotion {
    pub const NONE: RowMotion = RowMotion { dy: 0, opacity: 255 };
}

impl Default for RowMotion {
    fn default() -> RowMotion {
        RowMotion::NONE
    }
}

/// A row's place in a tree.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub struct TreeInfo {
    pub depth: u16,
    pub expandable: bool,
    pub expanded: bool,
}

/// A row's look.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub struct RowOut {
    /// Tasks' Apps / Background rows.
    pub section: bool,
    /// Exited: FG_DISABLED.
    pub ghost: bool,
    pub busy: bool,
    pub error: bool,
    /// A 2 px RIM_FOCUS tick at the left.
    pub fresh: bool,
    pub disabled: bool,
}

/// What a cell holds.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum CellKind {
    #[default]
    Text,
    Num,
    Bytes,
    Percent,
    Icon,
    Spark,
}

/// A text colour role.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum FgRole {
    #[default]
    Primary,
    Hot,
    Secondary,
    Tertiary,
    Disabled,
    Attention,
    Danger,
}

/// Horizontal alignment.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum Align {
    #[default]
    Start,
    End,
    /// Digit atlas, decimal point aligned.
    Decimal,
}

/// A coloured byte range of CellOut::text.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub struct Span {
    pub start: u32,
    pub end: u32,
    pub fg: FgRole,
}

/// A heat step, 1..=4 -> HEAT_1..HEAT_4.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct HeatStep(pub u8);

/// The model's own icon index.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct IconRef(pub u32);

/// A small mark over a cell's icon.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum Badge {
    Link,
    CloudOnline,
    CloudLocal,
    CloudPinned,
    Error,
    Shield,
    Lock,
}

/// An inline action of a row, shown on hover (tasks' Close / End).
#[derive(Clone, Debug, PartialEq)]
pub struct RowAction {
    pub label: Text,
    pub command: CommandId,
}

/// One cell as the model writes it; reused between cells, so no allocation per cell once warm.
#[derive(Clone, Debug, Default)]
pub struct CellOut {
    pub kind: CellKind,
    pub text: String,
    pub spans: [Span; 4],
    pub n_spans: u8,
    pub fg: FgRole,
    pub role: TypeRole,
    pub trim: Trim,
    pub align: Align,
    pub heat: Option<HeatStep>,
    pub icon: Option<IconRef>,
    pub badge: Option<Badge>,
    /// Dimmed while on the clipboard as cut.
    pub cut: bool,
    /// The icon alone drawn at half alpha (files: hidden items), the text unchanged.
    pub dim_icon: bool,
    /// The status slot, CAPTION, right.
    pub note: Option<(Text, FgRole)>,
    /// Shown on hover.
    pub action: Option<RowAction>,
    pub tip: Option<Text>,
}

impl CellOut {
    /// Back to the default, keeping the text's capacity.
    pub fn clear(&mut self) {
        let mut text = std::mem::take(&mut self.text);
        text.clear();
        *self = CellOut { text, ..CellOut::default() };
    }

    /// The kind and the text.
    pub fn set_text(&mut self, kind: CellKind, s: &str) -> &mut Self {
        self.kind = kind;
        self.text.clear();
        self.text.push_str(s);
        self
    }

    /// One more coloured span; a fifth is ignored.
    pub fn push_span(&mut self, start: u32, end: u32, fg: FgRole) -> &mut Self {
        if let Some(slot) = self.spans.get_mut(usize::from(self.n_spans)) {
            *slot = Span { start, end, fg };
            self.n_spans += 1;
        }
        self
    }
}

/// A set of row keys with two forms: a sorted Vec (up to 4096 keys, or when the keys are far apart) and, for a large
/// set whose keys sit close together, a bitset (4 bytes or less per key against 8). The switch has hysteresis, so the
/// same keys can sit in either form: equality and hash are by content, never by form.
#[derive(Clone, Debug)]
pub struct KeySet {
    repr: Repr,
}

#[derive(Clone, Debug)]
enum Repr {
    /// Sorted ascending.
    Sparse(Vec<u64>),
    /// Bit i of the words is the key base + i.
    Dense { base: u64, words: Vec<u64>, len: usize },
}

/// A sparse set longer than this becomes dense when its keys sit close together.
const DENSE_ABOVE: usize = 4096;
/// A dense set this short or shorter becomes sparse again.
const SPARSE_AT_OR_BELOW: usize = 2048;
/// The bitset is kept while the key span is at most this many times the key count.
const DENSE_SPAN_PER_KEY: u128 = 32;

impl Default for KeySet {
    fn default() -> KeySet {
        KeySet::new()
    }
}

impl PartialEq for KeySet {
    /// Same len and the same keys in ascending order, whatever the representation.
    fn eq(&self, o: &KeySet) -> bool {
        self.len() == o.len() && self.iter().eq(o.iter())
    }
}

impl Eq for KeySet {}

impl std::hash::Hash for KeySet {
    /// The len, then every key in ascending order.
    fn hash<H: std::hash::Hasher>(&self, h: &mut H) {
        self.len().hash(h);
        for k in self.iter() {
            k.hash(h);
        }
    }
}

/// The smallest and the largest key of a bitset, if any bit is set.
fn dense_bounds(base: u64, words: &[u64]) -> Option<(u64, u64)> {
    let first = words.iter().position(|&w| w != 0)?;
    let last = words.iter().rposition(|&w| w != 0)?;
    let lo = base + (first as u64) * 64 + u64::from(words[first].trailing_zeros());
    let hi = base + (last as u64) * 64 + u64::from(63 - words[last].leading_zeros());
    Some((lo, hi))
}

fn span_fits(lo: u64, hi: u64, len: usize) -> bool {
    u128::from(hi - lo) < DENSE_SPAN_PER_KEY * len as u128
}

impl KeySet {
    pub fn new() -> KeySet {
        KeySet { repr: Repr::Sparse(Vec::new()) }
    }

    pub fn len(&self) -> usize {
        match &self.repr {
            Repr::Sparse(v) => v.len(),
            Repr::Dense { len, .. } => *len,
        }
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    pub fn contains(&self, k: RowKey) -> bool {
        match &self.repr {
            Repr::Sparse(v) => v.binary_search(&k.0).is_ok(),
            Repr::Dense { base, words, .. } => {
                k.0 >= *base && {
                    let i = k.0 - *base;
                    words.get((i / 64) as usize).is_some_and(|w| w >> (i % 64) & 1 != 0)
                }
            }
        }
    }

    /// True when the key was not there.
    pub fn insert(&mut self, k: RowKey) -> bool {
        match &mut self.repr {
            Repr::Sparse(v) => match v.binary_search(&k.0) {
                Ok(_) => false,
                Err(i) => {
                    v.insert(i, k.0);
                    if v.len() > DENSE_ABOVE {
                        self.try_densify();
                    }
                    true
                }
            },
            Repr::Dense { base, words, len } => {
                let k = k.0;
                let in_range = k >= *base && (k - *base) / 64 < words.len() as u64;
                if !in_range {
                    let (lo, hi) = dense_bounds(*base, words).unwrap_or((k, k));
                    if !span_fits(lo.min(k), hi.max(k), *len + 1) {
                        self.make_sparse();
                        return self.insert(RowKey(k));
                    }
                    if k < *base {
                        let add = (*base - k).div_ceil(64) as usize;
                        if (add as u64) * 64 > *base {
                            // The bitset cannot reach below key 0: rebuild from the new lowest key.
                            self.make_sparse();
                            return self.insert(RowKey(k));
                        }
                        words.reserve_exact(add);
                        words.splice(0..0, std::iter::repeat_n(0, add));
                        *base -= add as u64 * 64;
                    } else {
                        let need = ((k - *base) / 64) as usize + 1;
                        words.reserve_exact(need - words.len());
                        words.resize(need, 0);
                    }
                }
                let i = k - *base;
                let w = &mut words[(i / 64) as usize];
                let bit = 1u64 << (i % 64);
                if *w & bit != 0 {
                    return false;
                }
                *w |= bit;
                *len += 1;
                true
            }
        }
    }

    /// True when the key was there.
    pub fn remove(&mut self, k: RowKey) -> bool {
        match &mut self.repr {
            Repr::Sparse(v) => match v.binary_search(&k.0) {
                Ok(i) => {
                    v.remove(i);
                    true
                }
                Err(_) => false,
            },
            Repr::Dense { base, words, len } => {
                if k.0 < *base {
                    return false;
                }
                let i = k.0 - *base;
                let Some(w) = words.get_mut((i / 64) as usize) else {
                    return false;
                };
                let bit = 1u64 << (i % 64);
                if *w & bit == 0 {
                    return false;
                }
                *w &= !bit;
                *len -= 1;
                if *len <= SPARSE_AT_OR_BELOW {
                    self.make_sparse();
                }
                true
            }
        }
    }

    pub fn clear(&mut self) {
        self.repr = Repr::Sparse(Vec::new());
    }

    /// The keys in ascending order.
    pub fn iter(&self) -> Box<dyn Iterator<Item = RowKey> + '_> {
        match &self.repr {
            Repr::Sparse(v) => Box::new(v.iter().map(|&k| RowKey(k))),
            Repr::Dense { base, words, .. } => {
                let base = *base;
                let mut at = 0usize;
                let mut cur = words.first().copied().unwrap_or(0);
                Box::new(std::iter::from_fn(move || {
                    while cur == 0 {
                        at += 1;
                        cur = *words.get(at)?;
                    }
                    let bit = cur.trailing_zeros();
                    cur &= cur - 1;
                    Some(RowKey(base + at as u64 * 64 + u64::from(bit)))
                }))
            }
        }
    }

    /// The bytes it holds on the heap.
    pub fn heap_bytes(&self) -> usize {
        match &self.repr {
            Repr::Sparse(v) => v.capacity() * std::mem::size_of::<u64>(),
            Repr::Dense { words, .. } => words.capacity() * std::mem::size_of::<u64>(),
        }
    }

    /// A long sparse set whose keys sit close together becomes a bitset.
    fn try_densify(&mut self) {
        let Repr::Sparse(v) = &self.repr else { return };
        let (Some(&lo), Some(&hi)) = (v.first(), v.last()) else { return };
        if !span_fits(lo, hi, v.len()) {
            return;
        }
        let mut words = vec![0u64; ((hi - lo) / 64) as usize + 1];
        for &k in v {
            let i = k - lo;
            words[(i / 64) as usize] |= 1u64 << (i % 64);
        }
        self.repr = Repr::Dense { base: lo, words, len: v.len() };
    }

    fn make_sparse(&mut self) {
        if matches!(self.repr, Repr::Dense { .. }) {
            let v: Vec<u64> = self.iter().map(|k| k.0).collect();
            self.repr = Repr::Sparse(v);
        }
    }
}

/// A selection by row key. Ctrl+A on 1,000,000 rows costs nothing (AllExcept); the derived PartialEq uses KeySet's
/// content equality.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub enum Selection {
    #[default]
    None,
    Single(RowKey),
    Keys(KeySet),
    AllExcept(KeySet),
}

impl Selection {
    pub fn contains(&self, k: RowKey) -> bool {
        match self {
            Selection::None => false,
            Selection::Single(s) => *s == k,
            Selection::Keys(set) => set.contains(k),
            Selection::AllExcept(set) => !set.contains(k),
        }
    }

    /// True when nothing is selected (AllExcept never counts as empty: it does not know the row count).
    pub fn is_empty(&self) -> bool {
        match self {
            Selection::None => true,
            Selection::Single(_) | Selection::AllExcept(_) => false,
            Selection::Keys(set) => set.is_empty(),
        }
    }

    /// How many of `total` rows are selected.
    pub fn count(&self, total: usize) -> usize {
        match self {
            Selection::None => 0,
            Selection::Single(_) => 1,
            Selection::Keys(set) => set.len(),
            Selection::AllExcept(set) => total.saturating_sub(set.len()),
        }
    }

    /// The one selected key, if exactly one is selected by key.
    pub fn single(&self) -> Option<RowKey> {
        match self {
            Selection::Single(k) => Some(*k),
            Selection::Keys(set) if set.len() == 1 => set.iter().next(),
            _ => None,
        }
    }

    /// The selected rows of `m`, in model order.
    pub fn for_each(&self, m: &dyn ListModel, f: &mut dyn FnMut(usize, RowKey)) {
        match self {
            Selection::None => {}
            Selection::Single(k) => {
                if let Some(row) = m.row_of(*k) {
                    f(row, *k);
                }
            }
            Selection::Keys(set) => {
                let mut rows: Vec<(usize, RowKey)> = set.iter().filter_map(|k| m.row_of(k).map(|r| (r, k))).collect();
                rows.sort_unstable();
                for (row, k) in rows {
                    f(row, k);
                }
            }
            Selection::AllExcept(set) => {
                for row in 0..m.len() {
                    let k = m.key(row);
                    if !set.contains(k) {
                        f(row, k);
                    }
                }
            }
        }
    }

    /// Drops the keys whose row_of is None (exited, deleted).
    pub fn retain_present(&mut self, m: &dyn ListModel) {
        match self {
            Selection::None => {}
            Selection::Single(k) => {
                if m.row_of(*k).is_none() {
                    *self = Selection::None;
                }
            }
            Selection::Keys(set) | Selection::AllExcept(set) => {
                let gone: Vec<RowKey> = set.iter().filter(|k| m.row_of(*k).is_none()).collect();
                for k in gone {
                    set.remove(k);
                }
            }
        }
    }

    /// A plain click: only this key.
    pub fn click(&mut self, k: RowKey) {
        *self = Selection::Single(k);
    }

    /// Ctrl+click: this key in or out, the rest kept.
    pub fn toggle(&mut self, k: RowKey) {
        match self {
            Selection::None => *self = Selection::Single(k),
            Selection::Single(s) if *s == k => *self = Selection::None,
            Selection::Single(s) => {
                let mut set = KeySet::new();
                set.insert(*s);
                set.insert(k);
                *self = Selection::Keys(set);
            }
            Selection::Keys(set) => {
                if !set.remove(k) {
                    set.insert(k);
                }
            }
            Selection::AllExcept(set) => {
                if !set.remove(k) {
                    set.insert(k);
                }
            }
        }
    }

    /// Ctrl+A.
    pub fn select_all(&mut self) {
        *self = Selection::AllExcept(KeySet::new());
    }

    /// Shift+click from `anchor` to `to` (both rows included); `additive` (Ctrl+Shift) keeps the rest. A key no longer
    /// in the model selects `to` alone.
    pub fn extend(&mut self, m: &dyn ListModel, anchor: RowKey, to: RowKey, additive: bool) {
        let (Some(a), Some(b)) = (m.row_of(anchor), m.row_of(to)) else {
            self.click(to);
            return;
        };
        let range = a.min(b)..=a.max(b);
        if let (true, Selection::AllExcept(except)) = (additive, &mut *self) {
            for row in range {
                except.remove(m.key(row));
            }
            return;
        }
        let mut set = KeySet::new();
        if additive {
            match self {
                Selection::Single(s) => {
                    set.insert(*s);
                }
                Selection::Keys(keys) => set = std::mem::take(keys),
                _ => {}
            }
        }
        for row in range {
            set.insert(m.key(row));
        }
        *self = Selection::Keys(set);
    }
}

/// How many rows a table lets the user select.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SelectionMode {
    None,
    Single,
    Multi,
}

/// A sort direction.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SortDir {
    Asc,
    Desc,
}

/// A table column.
#[derive(Clone, Debug, PartialEq)]
pub struct Column {
    pub id: ColId,
    pub title: Text,
    /// Logical px.
    pub width: i32,
    pub min: i32,
    pub flex: bool,
    pub align: Align,
    pub sort: Option<SortDir>,
    pub first_sort: SortDir,
    pub visible: bool,
    pub hideable: bool,
    pub tip: Option<Text>,
}

/// What a table tells its screen.
#[derive(Clone, Debug, PartialEq)]
#[non_exhaustive]
pub enum TableOut {
    Activated(RowKey),
    SortRequested(ColId, SortDir),
    SelectionChanged,
    FocusChanged(Option<RowKey>),
    ContextMenu(Option<RowKey>, PointP),
    ColumnsChanged,
    ExpandRequested(RowKey, bool),
    RowAction(RowKey, CommandId),
    RenameRequested(RowKey),
    RenameCommitted(RowKey, String),
    RenameCancelled(RowKey),
}

/// How selected rows look.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SelectionLook {
    /// 48/56/28, merged runs.
    Plate,
    /// FILL_CONTROL_HOT + a 2 x 12 bar.
    Bar,
}

/// A table's look.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TableStyle {
    pub selection: SelectionLook,
    pub row_inset: i32,
    pub hover_rim: bool,
    pub header: bool,
    pub icon_x: i32,
    pub merge_runs: bool,
}

impl TableStyle {
    pub const FILES: TableStyle = TableStyle {
        selection: SelectionLook::Plate,
        row_inset: 4,
        hover_rim: false,
        header: true,
        icon_x: 12,
        merge_runs: true,
    };
    pub const TASKS: TableStyle = TableStyle {
        selection: SelectionLook::Bar,
        row_inset: 8,
        hover_rim: true,
        header: false,
        icon_x: 32,
        merge_runs: false,
    };
}

/// Where a row scrolled into view lands.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ScrollTo {
    Nearest,
    Center,
    Top,
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Rows(Vec<u64>);

    impl ListModel for Rows {
        fn len(&self) -> usize {
            self.0.len()
        }
        fn key(&self, row: usize) -> RowKey {
            RowKey(self.0[row])
        }
        fn row_of(&self, key: RowKey) -> Option<usize> {
            self.0.iter().position(|&k| k == key.0)
        }
        fn label(&self, row: usize) -> Cow<'_, str> {
            Cow::Owned(self.0[row].to_string())
        }
        fn generation(&self) -> u64 {
            0
        }
    }

    fn rows_of(s: &Selection, m: &Rows) -> Vec<usize> {
        let mut out = Vec::new();
        s.for_each(m, &mut |r, _| out.push(r));
        out
    }

    #[test]
    fn keyset_is_a_sorted_set() {
        let mut a = KeySet::new();
        assert!(a.insert(RowKey(5)) && a.insert(RowKey(1)) && !a.insert(RowKey(5)));
        assert_eq!(a.iter().collect::<Vec<_>>(), vec![RowKey(1), RowKey(5)]);
        let mut b = KeySet::new();
        b.insert(RowKey(1));
        b.insert(RowKey(5));
        assert_eq!(a, b);
        assert!(a.remove(RowKey(1)) && !a.remove(RowKey(1)));
        assert_ne!(a, b);
        assert!(a.heap_bytes() >= 8);
        a.clear();
        assert!(a.is_empty());
    }

    #[test]
    fn selection_moves() {
        let m = Rows(vec![10, 20, 30, 40]);
        let mut s = Selection::default();
        s.click(RowKey(20));
        assert_eq!(s.single(), Some(RowKey(20)));
        s.toggle(RowKey(40));
        assert_eq!(rows_of(&s, &m), vec![1, 3]);
        s.extend(&m, RowKey(10), RowKey(30), false);
        assert_eq!(rows_of(&s, &m), vec![0, 1, 2]);
        s.select_all();
        s.toggle(RowKey(30));
        assert_eq!((s.count(4), rows_of(&s, &m)), (3, vec![0, 1, 3]));
        s.extend(&m, RowKey(20), RowKey(40), true);
        assert_eq!(rows_of(&s, &m), vec![0, 1, 2, 3]);
        let mut k = Selection::Single(RowKey(99));
        k.retain_present(&m);
        assert!(k.is_empty());
    }

    fn is_dense(s: &KeySet) -> bool {
        matches!(s.repr, Repr::Dense { .. })
    }

    #[test]
    fn keyset_matches_hashset() {
        for seed in 1..=8u64 {
            let mut x = seed.wrapping_mul(0x9E37_79B9_7F4A_7C15) | 1;
            let mut next = move || {
                x ^= x << 13;
                x ^= x >> 7;
                x ^= x << 17;
                x
            };
            let mut set = KeySet::new();
            let mut model = std::collections::HashSet::new();
            let mut seen_dense = false;
            // Phase 0 grows a tight range past the switch, phase 1 trims it, phase 2 scatters wide keys.
            for step in 0..10_000u64 {
                let (k, insert_bias) = match step {
                    0..=5_499 => (if next() % 8 == 0 { next() % 6_000 } else { step }, 96),
                    5_500..=7_999 => (next() % 6_000, 5),
                    _ => (if next() % 2 == 0 { next() % 6_000 } else { next() % (u64::MAX / 2) }, 50),
                };
                let r = next() % 100;
                if r < insert_bias {
                    assert_eq!(set.insert(RowKey(k)), model.insert(k));
                } else {
                    assert_eq!(set.remove(RowKey(k)), model.remove(&k));
                }
                assert_eq!(set.contains(RowKey(k)), model.contains(&k));
                assert_eq!(set.len(), model.len());
                seen_dense |= is_dense(&set);
            }
            let mut want: Vec<u64> = model.iter().copied().collect();
            want.sort_unstable();
            assert_eq!(set.iter().map(|k| k.0).collect::<Vec<_>>(), want, "seed {seed}");
            assert!(seen_dense, "seed {seed} never went dense");
        }
    }

    #[test]
    fn keyset_switches_forms_with_hysteresis() {
        let mut s = KeySet::new();
        for k in 0..4096 {
            s.insert(RowKey(k));
        }
        assert!(!is_dense(&s));
        s.insert(RowKey(4096));
        assert!(is_dense(&s));
        for k in 4097..5000 {
            s.insert(RowKey(k));
        }
        // A far key breaks the span rule: back to sparse, nothing lost.
        assert!(s.insert(RowKey(10_000_000)));
        assert!(!is_dense(&s) && s.len() == 5001 && s.contains(RowKey(10_000_000)) && s.contains(RowKey(77)));
        // Keys below the base and far above the words, within the rule.
        let mut d = KeySet::new();
        for k in 1000..5200 {
            d.insert(RowKey(k));
        }
        assert!(is_dense(&d));
        assert!(d.insert(RowKey(10)) && d.insert(RowKey(9000)) && !d.insert(RowKey(10)));
        assert!(is_dense(&d) && d.contains(RowKey(10)) && d.contains(RowKey(9000)) && !d.contains(RowKey(11)));
        assert_eq!(d.iter().next(), Some(RowKey(10)));
        assert_eq!(d.iter().last(), Some(RowKey(9000)));
        // Falling to 2048 or below returns to sparse.
        for k in 1000..5200 {
            d.remove(RowKey(k));
        }
        assert!(!is_dense(&d) && d.len() == 2);
    }

    #[test]
    fn equal_contents_equal_across_representations() {
        use std::hash::{Hash, Hasher};
        let mut dense = KeySet::new();
        for k in 0..5000 {
            dense.insert(RowKey(k));
        }
        for k in 3000..5000 {
            dense.remove(RowKey(k));
        }
        let mut sparse = KeySet::new();
        for k in 0..3000 {
            sparse.insert(RowKey(k));
        }
        assert!(is_dense(&dense) && !is_dense(&sparse));
        assert_eq!(dense, sparse);
        let hash = |s: &KeySet| {
            let mut h = std::collections::hash_map::DefaultHasher::new();
            s.hash(&mut h);
            h.finish()
        };
        assert_eq!(hash(&dense), hash(&sparse));
        assert_eq!(Selection::Keys(dense.clone()), Selection::Keys(sparse.clone()));
        assert_eq!(Selection::AllExcept(dense), Selection::AllExcept(sparse));
    }

    #[test]
    fn dense_100k_costs_under_13_kb() {
        let mut s = KeySet::new();
        for k in 0..100_000 {
            s.insert(RowKey(k));
        }
        assert!(is_dense(&s) && s.len() == 100_000);
        assert!(s.heap_bytes() < 13 * 1024, "{}", s.heap_bytes());
        assert_eq!(s.iter().count(), 100_000);
    }

    #[test]
    fn select_all_then_toggle() {
        let m = Rows((0..1000).collect());
        let mut s = Selection::default();
        s.select_all();
        assert_eq!(s.count(1000), 1000);
        s.toggle(RowKey(5));
        assert!(!s.contains(RowKey(5)) && s.contains(RowKey(6)) && s.count(1000) == 999);
        s.toggle(RowKey(5));
        assert_eq!((s.count(1000), s == Selection::AllExcept(KeySet::new())), (1000, true));
        assert_eq!(rows_of(&s, &m).len(), 1000);
    }

    #[test]
    fn extend_follows_model_order() {
        let m = Rows(vec![50, 40, 30, 20, 10]);
        let mut s = Selection::default();
        s.click(RowKey(40));
        s.extend(&m, RowKey(40), RowKey(20), false);
        assert_eq!(rows_of(&s, &m), vec![1, 2, 3]);
        s.extend(&m, RowKey(10), RowKey(10), true);
        assert_eq!(rows_of(&s, &m), vec![1, 2, 3, 4]);
        s.extend(&m, RowKey(20), RowKey(50), false);
        assert_eq!(rows_of(&s, &m), vec![0, 1, 2, 3]);
    }

    #[test]
    fn retain_present_after_rows_vanish() {
        let mut m = Rows(vec![1, 2, 3, 4]);
        let mut keys = Selection::default();
        keys.click(RowKey(1));
        keys.toggle(RowKey(3));
        keys.toggle(RowKey(4));
        let mut all = Selection::AllExcept(KeySet::new());
        all.toggle(RowKey(2));
        all.toggle(RowKey(3));
        m.0 = vec![1, 4];
        keys.retain_present(&m);
        all.retain_present(&m);
        assert_eq!(rows_of(&keys, &m), vec![0, 1]);
        assert!(!keys.contains(RowKey(3)));
        assert_eq!(all, Selection::AllExcept(KeySet::new()));
        let mut one = Selection::Single(RowKey(2));
        one.retain_present(&m);
        assert_eq!(one, Selection::None);
    }

    #[test]
    fn cell_out_keeps_its_capacity() {
        let mut c = CellOut::default();
        c.set_text(CellKind::Num, "12345678").push_span(0, 2, FgRole::Hot);
        for _ in 0..5 {
            c.push_span(0, 1, FgRole::Danger);
        }
        assert_eq!(c.n_spans, 4);
        let cap = c.text.capacity();
        c.clear();
        assert_eq!((c.text.capacity(), c.n_spans, c.kind), (cap, 0, CellKind::Text));
    }
}
