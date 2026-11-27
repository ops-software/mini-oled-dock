//! FilesTable, the details columns and the EntryStore behind the Listing seam (P3-files-app F1a.2-F1a.4). Plain tests
//! over made-up names: no window, no host, no file system.

use std::rc::Rc;
use std::sync::Arc;

use files_engine::fakes::{self, FakeListing};
use files_engine::format::{Fmt, LocalTime};
use files_engine::listing::{Anchor, Batch, Diff, EntryStore, Gen, RawEntry, SortKey, Visibility, WidePath};
use files_engine::model::{id_of, key_of, ColKind, FilesTable, IconKey, Listing, ViewOpts};
use files_engine::types::TypeNames;
use files_engine::view::columns::{default_columns, sort_key};
use oled_ui::model::{
    Align, Badge, CellKind, CellOut, ColId, FgRole, IconRef, KeySet, ListModel, Selection, SortDir, TableModel,
};
use oled_ui::text::Trim;
use oled_ui::theme::TypeRole;
use oled_ui::UiLang;

const DIR: u32 = 0x10;
const ARCHIVE: u32 = 0x20;
const READONLY: u32 = 0x1;
const HIDDEN: u32 = 0x2;
const SYSTEM: u32 = 0x4;
const PINNED: u32 = 0x8_0000;
const UNPINNED: u32 = 0x10_0000;
const RECALL_ON_DATA_ACCESS: u32 = 0x40_0000;
const TAG_MOUNT_POINT: u32 = 0xA000_0003;
const TAG_SYMLINK: u32 = 0xA000_000C;

const NOW: LocalTime = LocalTime { year: 2026, month: 11, day: 21, hour: 15, minute: 0 };

fn dir() -> WidePath {
    WidePath::parse(r"C:\Data\Reports").unwrap()
}

/// One done batch of (name, attrs, reparse tag, size, mtime).
fn batch(items: &[(&str, u32, u32, u64, i64)]) -> Batch {
    let mut b = Batch { gen: Gen(0), names: Vec::new(), entries: Vec::new(), done: true, error: None, elapsed_us: 0 };
    for &(name, attrs, reparse_tag, size, mtime) in items {
        let name_off = b.names.len() as u32;
        b.names.extend_from_slice(name.as_bytes());
        b.entries.push(RawEntry {
            name_off,
            name_len: name.len() as u16,
            attrs,
            reparse_tag,
            size,
            mtime,
            ctime: mtime,
        });
    }
    b
}

fn store(b: Batch) -> EntryStore {
    let mut s = EntryStore::new(dir(), Gen(0));
    s.append(b);
    s.merge_pending();
    s
}

fn table_of(b: Batch, opts: ViewOpts, lang: UiLang) -> FilesTable {
    let fmt = Rc::new(if lang.italian { Fmt::fixed_it() } else { Fmt::fixed_en() });
    let mut t = FilesTable::new(Box::new(store(b)), fmt, Rc::new(TypeNames::empty()), opts, NOW);
    t.set_lang(lang);
    t
}

fn simple(items: &[(&str, u32, u32, u64, i64)]) -> FilesTable {
    table_of(batch(items), ViewOpts::default(), UiLang::EN)
}

fn row_of_name(t: &FilesTable, name: &str) -> usize {
    let id = t.listing().find(name.as_bytes()).expect("the name is listed");
    t.listing().row_of(id).expect("the entry is visible") as usize
}

fn cell(t: &FilesTable, name: &str, kind: ColKind) -> CellOut {
    let mut out = CellOut::default();
    t.cell(row_of_name(t, name), ColId(kind as u16), &mut out);
    out
}

// ---- F1a.2 -----------------------------------------------------------------------------------------------------

#[test]
fn cell_name_spans() {
    let t = simple(&[("report-final-v2.pdf", ARCHIVE, 0, 1, 0), ("invoices.old", DIR, 0, 0, 0)]);
    let f = cell(&t, "report-final-v2.pdf", ColKind::Name);
    assert_eq!(f.text, "report-final-v2.pdf");
    assert_eq!(f.n_spans, 2);
    assert_eq!((f.spans[0].start, f.spans[0].end, f.spans[0].fg), (0, 15, FgRole::Primary));
    assert_eq!((f.spans[1].start, f.spans[1].end, f.spans[1].fg), (15, 19, FgRole::Secondary));
    let d = cell(&t, "invoices.old", ColKind::Name);
    assert_eq!(d.text, "invoices.old");
    assert_eq!(d.n_spans, 1);
    assert_eq!((d.spans[0].start, d.spans[0].end, d.spans[0].fg), (0, 12, FgRole::Primary));
}

#[test]
fn cell_name_dot_rules() {
    let t = simple(&[(".profile", ARCHIVE, 0, 1, 0), ("notes.", ARCHIVE, 0, 1, 0), ("a.tar.gz", ARCHIVE, 0, 1, 0)]);
    assert_eq!(cell(&t, ".profile", ColKind::Name).n_spans, 1, "a leading dot starts no extension");
    assert_eq!(cell(&t, "notes.", ColKind::Name).n_spans, 1, "a trailing dot starts no extension");
    let gz = cell(&t, "a.tar.gz", ColKind::Name);
    assert_eq!((gz.spans[1].start, gz.spans[1].end), (5, 8), "the last dot");
}

#[test]
fn lone_surrogate_name_is_replaced_not_dropped() {
    let mut b = batch(&[]);
    let name = [b'a', 0xED, 0xA0, 0x80, b'.', b't', b'x', b't'];
    b.names.extend_from_slice(&name);
    b.entries.push(RawEntry {
        name_off: 0,
        name_len: name.len() as u16,
        attrs: ARCHIVE,
        reparse_tag: 0,
        size: 1,
        mtime: 0,
        ctime: 0,
    });
    let t = table_of(b, ViewOpts::default(), UiLang::EN);
    let mut out = CellOut::default();
    t.cell(0, ColId(ColKind::Name as u16), &mut out);
    assert_eq!(out.text, "a\u{FFFD}.txt");
    assert_eq!((out.spans[1].start, out.spans[1].end), (4, 8), "offsets of the source stay valid");
}

#[test]
fn extensions_off() {
    let opts = ViewOpts { show_extensions: false, ..ViewOpts::default() };
    let t =
        table_of(batch(&[("report-final-v2.pdf", ARCHIVE, 0, 1, 0), ("invoices.old", DIR, 0, 0, 0)]), opts, UiLang::EN);
    let f = cell(&t, "report-final-v2.pdf", ColKind::Name);
    assert_eq!(f.text, "report-final-v2");
    assert_eq!(f.n_spans, 1);
    assert_eq!(cell(&t, "invoices.old", ColKind::Name).text, "invoices.old", "a folder keeps its whole name");
    assert_eq!(t.label(row_of_name(&t, "report-final-v2.pdf")), "report-final-v2.pdf", "the accessible name is whole");
}

#[test]
fn trim_is_middle_10() {
    let t = simple(&[("report-final-v2.pdf", ARCHIVE, 0, 1, 0)]);
    let c = cell(&t, "report-final-v2.pdf", ColKind::Name);
    assert_eq!(c.trim, Trim::Middle { tail: 10 });
    assert_eq!((c.role, c.fg, c.kind), (TypeRole::Body, FgRole::Primary, CellKind::Text));
}

#[test]
fn long_names_carry_a_tip() {
    let long = format!("{}.pdf", "very-long-report-name-".repeat(3));
    let t = simple(&[(&long, ARCHIVE, 0, 1, 0), ("short.pdf", ARCHIVE, 0, 1, 0)]);
    assert_eq!(cell(&t, &long, ColKind::Name).tip.as_deref(), Some(long.as_str()));
    assert!(cell(&t, "short.pdf", ColKind::Name).tip.is_none());
}

#[test]
fn size_num_end_and_blank_for_folders() {
    let t = simple(&[("a.bin", ARCHIVE, 0, 10_240, 0), ("sub", DIR, 0, 0, 0)]);
    let f = cell(&t, "a.bin", ColKind::Size);
    assert_eq!((f.kind, f.align, f.fg), (CellKind::Num, Align::End, FgRole::Secondary));
    assert_eq!(f.text, Fmt::fixed_en().size(10_240, false));
    assert_eq!(cell(&t, "sub", ColKind::Size).text, "");
}

#[test]
fn modified_uses_fmt() {
    let ft = fakes::FT_2026_11_21 - 3 * 86_400 * 10_000_000;
    let t = simple(&[("a.txt", ARCHIVE, 0, 1, ft)]);
    let fmt = Fmt::fixed_en();
    for kind in [ColKind::Modified, ColKind::Created] {
        let c = cell(&t, "a.txt", kind);
        assert_eq!(c.text, fmt.modified(ft, NOW, false));
        assert_eq!((c.kind, c.fg), (CellKind::Text, FgRole::Secondary));
    }
    let opts = ViewOpts { full_dates: true, ..ViewOpts::default() };
    let t = table_of(batch(&[("a.txt", ARCHIVE, 0, 1, ft)]), opts, UiLang::EN);
    assert_eq!(cell(&t, "a.txt", ColKind::Modified).text, fmt.modified(ft, NOW, true));
}

#[test]
fn type_folder_and_fallback() {
    let items =
        [("sub", DIR, 0, 0, 0), ("x.pdf", ARCHIVE, 0, 1, 0), ("README", ARCHIVE, 0, 1, 0), ("Y.PDF", ARCHIVE, 0, 1, 0)];
    let en = table_of(batch(&items), ViewOpts::default(), UiLang::EN);
    assert_eq!(cell(&en, "sub", ColKind::Type).text, "File folder");
    assert_eq!(cell(&en, "x.pdf", ColKind::Type).text, "PDF File");
    assert_eq!(cell(&en, "Y.PDF", ColKind::Type).text, "PDF File");
    assert_eq!(cell(&en, "README", ColKind::Type).text, "File");
    assert_eq!(cell(&en, "x.pdf", ColKind::Type).fg, FgRole::Secondary);
    let it = table_of(batch(&items), ViewOpts::default(), UiLang::IT);
    assert_eq!(cell(&it, "sub", ColKind::Type).text, "Cartella di file");
    assert_eq!(cell(&it, "x.pdf", ColKind::Type).text, "File PDF");
    assert_eq!(cell(&it, "README", ColKind::Type).text, "File");
    let pseudo = table_of(batch(&items), ViewOpts::default(), UiLang { italian: false, pseudo: true });
    assert_eq!(
        cell(&pseudo, "x.pdf", ColKind::Type).text,
        TypeNames::fallback(b"pdf", UiLang { italian: false, pseudo: true })
    );
}

#[test]
fn badges_from_attributes() {
    let t = simple(&[
        ("mount", DIR, TAG_MOUNT_POINT, 0, 0),
        ("link.txt", ARCHIVE, TAG_SYMLINK, 1, 0),
        ("cloud.txt", ARCHIVE | RECALL_ON_DATA_ACCESS, 0, 1, 0),
        ("unpinned.txt", ARCHIVE | UNPINNED, 0, 1, 0),
        ("pinned.txt", ARCHIVE | PINNED, 0, 1, 0),
        ("plain.txt", ARCHIVE, 0, 1, 0),
    ]);
    let badge = |n: &str| cell(&t, n, ColKind::Name).badge;
    assert_eq!(badge("mount"), Some(Badge::Link));
    assert_eq!(badge("link.txt"), Some(Badge::Link));
    assert_eq!(badge("cloud.txt"), Some(Badge::CloudOnline));
    assert_eq!(badge("unpinned.txt"), Some(Badge::CloudOnline));
    assert_eq!(badge("pinned.txt"), Some(Badge::CloudPinned));
    assert_eq!(badge("plain.txt"), None);
}

#[test]
fn attributes_and_status_columns() {
    let t = simple(&[
        ("a.txt", ARCHIVE | READONLY | HIDDEN | SYSTEM, 0, 1, 0),
        ("b.txt", ARCHIVE, 0, 1, 0),
        ("online.txt", ARCHIVE | RECALL_ON_DATA_ACCESS, 0, 1, 0),
        ("local.txt", ARCHIVE | PINNED, 0, 1, 0),
    ]);
    let mut t = t;
    t.listing_mut().set_visibility(Visibility { hidden: true, protected_system: true });
    assert_eq!(cell(&t, "a.txt", ColKind::Attributes).text, "R H S A");
    assert_eq!(cell(&t, "b.txt", ColKind::Attributes).text, "A");
    assert_eq!(cell(&t, "online.txt", ColKind::Status).text, "Online-only");
    assert_eq!(cell(&t, "local.txt", ColKind::Status).text, "Always on this device");
    assert_eq!(cell(&t, "b.txt", ColKind::Status).text, "");
}

#[test]
fn unknown_row_or_column_is_an_empty_cell() {
    let t = simple(&[("a.txt", ARCHIVE, 0, 1, 0)]);
    let mut out = CellOut::default();
    out.text.push_str("stale");
    t.cell(9, ColId(0), &mut out);
    assert_eq!(out.text, "");
    out.text.push_str("stale");
    t.cell(0, ColId(77), &mut out);
    assert_eq!(out.text, "");
}

#[test]
fn hidden_and_system_dim_icon() {
    let mut t = simple(&[
        ("h.txt", ARCHIVE | HIDDEN, 0, 1, 0),
        ("sysdir", DIR | SYSTEM, 0, 0, 0),
        ("plain.txt", ARCHIVE, 0, 1, 0),
    ]);
    t.listing_mut().set_visibility(Visibility { hidden: true, protected_system: true });
    for (name, dim) in [("h.txt", true), ("sysdir", true), ("plain.txt", false)] {
        let c = cell(&t, name, ColKind::Name);
        assert_eq!(c.dim_icon, dim, "{name}");
        assert_eq!(c.text, name);
        assert_eq!(c.fg, FgRole::Primary, "the text is not dimmed: only the icon");
    }
    assert!(!cell(&t, "h.txt", ColKind::Size).dim_icon, "only the Name cell carries an icon");
}

#[test]
fn generation_changes_on_every_mutation() {
    let mut t = simple(&[("a.txt", ARCHIVE, 0, 1, 0)]);
    let mut last = t.generation();
    fn moved(t: &FilesTable, last: &mut u64, what: &str) {
        let g = t.generation();
        assert_ne!(g, *last, "{what} moves the generation");
        *last = g;
    }
    t.bump();
    moved(&t, &mut last, "bump");
    t.set_opts(ViewOpts { compact: true, ..ViewOpts::default() });
    moved(&t, &mut last, "set_opts");
    t.set_now(LocalTime { minute: 1, ..NOW });
    moved(&t, &mut last, "set_now");
    t.set_lang(UiLang::IT);
    moved(&t, &mut last, "set_lang");
    t.listing_mut().append(batch(&[]));
    moved(&t, &mut last, "listing_mut");
    assert!(t.set_icon(IconKey::Folder, 2, Arc::from(vec![1u8; 16])));
    moved(&t, &mut last, "a changed icon");
    assert!(!t.set_icon(IconKey::Folder, 2, Arc::from(vec![1u8; 16])));
    assert_eq!(t.generation(), last, "the same icon again changes nothing");
    t.replace(Box::new(store(batch(&[("b.txt", ARCHIVE, 0, 1, 0)]))));
    moved(&t, &mut last, "replace");
    let _ = t.listing().len();
    assert_eq!(t.generation(), last, "reading changes nothing");
}

#[test]
fn key_round_trip() {
    let t = simple(&[("b.txt", ARCHIVE, 0, 1, 0), ("a.txt", ARCHIVE, 0, 1, 0), ("sub", DIR, 0, 0, 0)]);
    assert_eq!(t.len(), 3);
    for row in 0..t.len() {
        let key = t.key(row);
        assert_eq!(key, key_of(id_of(key)));
        assert_eq!(t.row_of(key), Some(row));
        assert_eq!(t.listing().row_of(id_of(key)), Some(row as u32));
    }
    assert_eq!(t.key(99).0, u64::MAX);
    assert_eq!(t.row_of(key_of(files_engine::listing::EntryId(9999))), None);
}

#[test]
fn icons_come_from_the_tables_own_icon_table() {
    let mut t = simple(&[("sub", DIR, 0, 0, 0), ("a.pdf", ARCHIVE, 0, 1, 0), ("b.txt", ARCHIVE, 0, 1, 0)]);
    let icon = |t: &FilesTable, n: &str| cell(t, n, ColKind::Name).icon;
    assert_eq!(icon(&t, "sub"), None, "nothing is filled yet");
    let px = |v: u8| Arc::<[u8]>::from(vec![v, 0, 0, 255, v, 0, 0, 255, v, 0, 0, 255, v, 0, 0, 255]);
    assert!(t.set_icon(IconKey::Folder, 2, px(1)));
    assert!(t.set_icon(IconKey::Class(b"".to_vec().into()), 2, px(2)));
    assert!(t.set_icon(IconKey::Class(b"PDF".to_vec().into()), 2, px(3)));
    let (f, g, p) = (icon(&t, "sub"), icon(&t, "b.txt"), icon(&t, "a.pdf"));
    assert_eq!(f, Some(IconRef(0)));
    assert_eq!(g, Some(IconRef(1)));
    assert!(p.is_some() && p != f && p != g, "a class icon of its own, found by the lower-case extension");
    let bmp = t.icon(p.unwrap(), 16).expect("filled slot");
    assert_eq!((bmp.width, bmp.height, bmp.pixels[0]), (2, 2, 0xFF00_0003), "BGRA bytes become 0xAARRGGBB");
    let a = files_engine::listing::EntryId(t.listing().find(b"a.pdf").unwrap().0);
    assert!(t.set_icon(IconKey::Item(a), 2, px(4)));
    assert_ne!(icon(&t, "a.pdf"), p, "an item's own icon wins");
    assert!(!t.set_icon(IconKey::Folder, 0, px(1)), "a zero size is refused");
    assert!(!t.set_icon(IconKey::Folder, 4, px(1)), "too few bytes are refused");
}

#[test]
fn item_icons_are_capped() {
    let mut t = simple(&[("a.txt", ARCHIVE, 0, 1, 0)]);
    let px = Arc::<[u8]>::from(vec![9u8; 4]);
    for i in 0..600u32 {
        t.set_icon(IconKey::Item(files_engine::listing::EntryId(i)), 1, px.clone());
    }
    let held = (0..700u32).filter(|&s| t.icon(IconRef(s), 16).is_some()).count();
    assert!(held <= 512, "{held} per-item icons held");
    assert!(held >= 500, "{held}: the newest are kept");
}

#[test]
fn paths_of_rows() {
    let t = simple(&[("b.txt", ARCHIVE, 0, 1, 0), ("a.txt", ARCHIVE, 0, 1, 0), ("sub", DIR, 0, 0, 0)]);
    let key = |n: &str| t.key(row_of_name(&t, n));
    assert_eq!(t.path_of(key("a.txt")).unwrap().to_string_lossy(), r"C:\Data\Reports\a.txt");
    assert!(t.path_of(key_of(files_engine::listing::EntryId(9999))).is_none());
    let mut keys = KeySet::new();
    keys.insert(key("b.txt"));
    keys.insert(key("sub"));
    let sel = Selection::Keys(keys);
    let paths: Vec<String> = t.selected_paths(&sel).iter().map(|p| p.to_string_lossy()).collect();
    assert_eq!(paths, [r"C:\Data\Reports\sub", r"C:\Data\Reports\b.txt"], "model order: folders first");
}

// ---- F1a.3 -----------------------------------------------------------------------------------------------------

#[test]
fn default_columns_match_the_spec() {
    let cols = default_columns(UiLang::EN);
    let got: Vec<_> = cols
        .iter()
        .map(|c| (c.id.0, c.title.to_string(), c.width, c.min, c.flex, c.align, c.visible, c.hideable))
        .collect();
    let want = vec![
        (0, "Name".to_string(), 320, 200, true, Align::Start, true, false),
        (1, "Date modified".to_string(), 150, 110, false, Align::Start, true, true),
        (2, "Type".to_string(), 140, 90, false, Align::Start, true, true),
        (3, "Size".to_string(), 90, 70, false, Align::End, true, true),
        (4, "Date created".to_string(), 150, 110, false, Align::Start, false, true),
        (5, "Attributes".to_string(), 90, 70, false, Align::Start, false, true),
        (6, "Status".to_string(), 120, 90, false, Align::Start, false, true),
    ];
    assert_eq!(got, want);
    assert!(cols.iter().all(|c| c.sort.is_none() && c.tip.is_none()));
    assert_eq!(default_columns(UiLang::IT)[1].title, "Ultima modifica");
}

#[test]
fn first_sort() {
    let first: Vec<_> = default_columns(UiLang::EN).iter().take(5).map(|c| c.first_sort).collect();
    assert_eq!(
        first,
        [SortDir::Asc, SortDir::Desc, SortDir::Asc, SortDir::Desc, SortDir::Desc],
        "Name Asc, Modified Desc, Type Asc, Size Desc, Created Desc: Explorer's first click"
    );
}

#[test]
fn sort_key_mapping() {
    assert_eq!(sort_key(ColKind::Name), Some(SortKey::Name));
    assert_eq!(sort_key(ColKind::Modified), Some(SortKey::Modified));
    assert_eq!(sort_key(ColKind::Type), Some(SortKey::Type));
    assert_eq!(sort_key(ColKind::Size), Some(SortKey::Size));
    assert_eq!(sort_key(ColKind::Created), Some(SortKey::Created));
    assert_eq!(sort_key(ColKind::Attributes), None);
    assert_eq!(sort_key(ColKind::Status), None);
}

// ---- F1a.4 -----------------------------------------------------------------------------------------------------

fn names_of(l: &dyn Listing) -> Vec<String> {
    l.order().iter().map(|&id| String::from_utf8_lossy(l.name(id).unwrap()).into_owned()).collect()
}

fn numbered(range: std::ops::Range<u32>) -> Batch {
    let owned: Vec<String> = range.map(|i| format!("n{i:03}.txt")).collect();
    batch(&owned.iter().map(|n| (n.as_str(), ARCHIVE, 0, 1, 0)).collect::<Vec<_>>())
}

#[test]
fn folders_first_natural_order() {
    let l = store(batch(&[
        ("file10.txt", ARCHIVE, 0, 1, 0),
        ("file2.txt", ARCHIVE, 0, 1, 0),
        ("zeta", DIR, 0, 0, 0),
        ("alpha", DIR, 0, 0, 0),
    ]));
    assert_eq!(names_of(&l), ["alpha", "zeta", "file2.txt", "file10.txt"]);
    let boxed: Box<dyn Listing> = Box::new(l);
    assert!(boxed.is_dir(boxed.find(b"zeta").unwrap()));
}

#[test]
fn apply_keeps_anchor() {
    let mut l: Box<dyn Listing> = Box::new(store(numbered(0..100)));
    let id = l.find(b"n050.txt").unwrap();
    let mut d = Diff::new(Gen(0));
    for i in 0..10 {
        d.remove(format!("n{i:03}.txt").as_bytes());
    }
    let (stats, anchor) = l.apply(&d, Some(Anchor { id, dy: 3 }));
    assert_eq!(stats.removed, 10);
    let a = anchor.expect("the anchor survives");
    assert_eq!((a.id, a.dy), (id, 3));
    assert_eq!(l.row_of(id), Some(40));
    assert_eq!(l.len(), 90);
}

#[test]
fn visibility_hides_hidden() {
    let mut l: Box<dyn Listing> =
        Box::new(store(batch(&[("a.txt", ARCHIVE, 0, 1, 0), (".secret", ARCHIVE | HIDDEN, 0, 1, 0)])));
    assert_eq!(names_of(&*l), ["a.txt"]);
    l.set_visibility(Visibility { hidden: true, protected_system: false });
    assert_eq!(names_of(&*l), [".secret", "a.txt"]);
}

#[test]
fn compact_maps_ids() {
    let mut l: Box<dyn Listing> = Box::new(store(numbered(0..20)));
    let old: Vec<_> = (0..20).map(|i| l.find(format!("n{i:03}.txt").as_bytes()).unwrap()).collect();
    let mut d = Diff::new(Gen(0));
    for i in (0..20).step_by(2) {
        d.remove(format!("n{i:03}.txt").as_bytes());
    }
    l.apply(&d, None);
    let map = l.compact();
    for (i, id) in old.iter().enumerate() {
        let mapped = map.0.get(id.0 as usize).copied().flatten();
        if i % 2 == 0 {
            assert!(mapped.is_none(), "a removed entry maps to nothing");
        } else {
            let new = mapped.expect("a survivor maps to its new id");
            assert_eq!(l.name(new).unwrap(), format!("n{i:03}.txt").as_bytes());
            assert_eq!(l.find(format!("n{i:03}.txt").as_bytes()), Some(new));
        }
    }
    assert_eq!(l.len(), 10);
}

#[test]
fn merge_fresh_keeps_ids() {
    let mut l: Box<dyn Listing> = Box::new(store(numbered(0..100)));
    let survivors: Vec<_> = (10..100).map(|i| (i, l.find(format!("n{i:03}.txt").as_bytes()).unwrap())).collect();
    let fresh: Box<dyn Listing> = Box::new(store(numbered(10..110)));
    let stats = l.merge_fresh(fresh);
    assert_eq!((stats.added, stats.removed, stats.changed), (10, 10, 0));
    assert_eq!(l.len(), 100);
    for (i, id) in survivors {
        assert_eq!(l.find(format!("n{i:03}.txt").as_bytes()), Some(id), "n{i:03} keeps its id");
    }
    assert!(l.find(b"n000.txt").is_none() && l.find(b"n109.txt").is_some());
}

#[cfg(not(debug_assertions))]
#[test]
fn merge_fresh_of_another_type_changes_nothing() {
    let mut l: Box<dyn Listing> = Box::new(store(numbered(0..5)));
    let before = names_of(&*l);
    let other: Box<dyn Listing> = Box::new(FakeListing::new(dir(), Gen(0)));
    assert_eq!(l.merge_fresh(other), files_engine::listing::ApplyStats::default());
    assert_eq!(names_of(&*l), before);
}

#[cfg(debug_assertions)]
#[test]
#[should_panic(expected = "merges only another EntryStore")]
fn merge_fresh_of_another_type_asserts_in_debug() {
    let mut l: Box<dyn Listing> = Box::new(store(numbered(0..5)));
    let other: Box<dyn Listing> = Box::new(FakeListing::new(dir(), Gen(0)));
    let _ = l.merge_fresh(other);
}

#[test]
fn into_any_gives_the_store_back() {
    let l: Box<dyn Listing> = Box::new(store(numbered(0..3)));
    assert!(l.into_any().downcast::<EntryStore>().is_ok());
}
