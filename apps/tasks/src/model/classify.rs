//! Path classes, the never-suspend list and the image path facts.
use crate::model::group::{ExeId, ImageId, Interner, ProcInfo};
use oled_sysmon::{ProcKey, ProcRow, ProcTable};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum PathClass {
    SystemRoot,
    Other,
    NoImage,
    /// Path not resolved yet or not readable.
    Unknown,
}

/// None -> NoImage; ASCII case-insensitive "<root>\" prefix.
pub fn path_class(path: Option<&str>, system_root: &str) -> PathClass {
    let Some(path) = path else {
        return PathClass::NoImage;
    };
    let root = system_root.trim_end_matches('\\');
    if root.is_empty() {
        return PathClass::Other;
    }
    let (p, r) = (path.as_bytes(), root.as_bytes());
    if p.len() > r.len() && p[r.len()] == b'\\' && p[..r.len()].eq_ignore_ascii_case(r) {
        PathClass::SystemRoot
    } else {
        PathClass::Other
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum SuspendBlock {
    DesktopFreezes,
    TextInputFreezes,
    SoundStops,
}

/// The never-suspend list, ASCII case-insensitive.
pub fn never_suspend(image_file_name: &str) -> Option<SuspendBlock> {
    const DESKTOP: [&str; 7] =
        ["dwm.exe", "explorer.exe", "dock.exe", "tasks.exe", "tasks-admin.exe", "sihost.exe", "fontdrvhost.exe"];
    const TEXT_INPUT: [&str; 2] = ["ctfmon.exe", "textinputhost.exe"];
    let is = |list: &[&str]| list.iter().any(|n| image_file_name.eq_ignore_ascii_case(n));
    if is(&DESKTOP) {
        Some(SuspendBlock::DesktopFreezes)
    } else if is(&TEXT_INPUT) {
        Some(SuspendBlock::TextInputFreezes)
    } else if image_file_name.eq_ignore_ascii_case("audiodg.exe") {
        Some(SuspendBlock::SoundStops)
    } else {
        None
    }
}

/// The image path facts the engine's PathSampler produces and the model consumes.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ImagePath {
    /// Pid 0, pid 4, or a bare name such as "Registry".
    NoImage,
    /// The query failed (access denied, gone); never retried.
    Unknown,
    Path(std::sync::Arc<str>),
}

/// Sorted by key; a live key without an entry is pending.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct PathMap {
    pub entries: Vec<(ProcKey, ImagePath)>,
}

impl PathMap {
    /// Binary search.
    pub fn get(&self, key: ProcKey) -> Option<&ImagePath> {
        let i = self.entries.binary_search_by_key(&key, |e| e.0).ok()?;
        self.entries.get(i).map(|e| &e.1)
    }
}

/// None (pending) and Unknown -> Unknown; NoImage -> NoImage; Path -> path_class.
pub fn class_of(p: Option<&ImagePath>, system_root: &str) -> PathClass {
    match p {
        None | Some(ImagePath::Unknown) => PathClass::Unknown,
        Some(ImagePath::NoImage) => PathClass::NoImage,
        Some(ImagePath::Path(path)) => path_class(Some(path), system_root),
    }
}

#[derive(Debug, Default)]
pub struct InfoBook {
    info: Vec<ProcInfo>,
    settled: Vec<bool>,
    scratch: String,
}

/// One row's facts. The image name and the path are lower-cased (ASCII) before they are interned.
fn compute(
    row: &ProcRow,
    procs: &ProcTable,
    paths: &PathMap,
    system_root: &str,
    names: &mut Interner,
    scratch: &mut String,
) -> (ProcInfo, bool) {
    scratch.clear();
    scratch.extend(char::decode_utf16(procs.names.get(row.image).iter().copied()).map(|r| r.unwrap_or('\u{FFFD}')));
    scratch.make_ascii_lowercase();
    let image = ImageId(names.intern(scratch));
    let p = paths.get(row.key);
    let class = class_of(p, system_root);
    let exe = match p {
        Some(ImagePath::Path(path)) => {
            scratch.clear();
            scratch.push_str(path);
            scratch.make_ascii_lowercase();
            Some(ExeId(names.intern(scratch)))
        }
        _ => None,
    };
    (ProcInfo { key: row.key, exe, image, class }, p.is_some())
}

impl InfoBook {
    pub fn new() -> InfoBook {
        InfoBook::default()
    }
    /// One ProcInfo per row of procs, same order (the producer of GroupInput::info). A row whose path is settled
    /// (any entry in paths) is computed once and reused while its key lives; a pending row is recomputed each call.
    pub fn update(
        &mut self,
        procs: &ProcTable,
        paths: &PathMap,
        system_root: &str,
        names: &mut Interner,
    ) -> &[ProcInfo] {
        let rows = &procs.rows;
        let same = self.info.len() == rows.len() && self.info.iter().zip(rows).all(|(i, r)| i.key == r.key);
        if same {
            // The common tick: the same processes, so only pending rows are looked at and nothing is allocated.
            for (i, row) in rows.iter().enumerate() {
                if !self.settled[i] {
                    let (info, settled) = compute(row, procs, paths, system_root, names, &mut self.scratch);
                    self.info[i] = info;
                    self.settled[i] = settled;
                }
            }
        } else {
            let old_info = std::mem::take(&mut self.info);
            let old_settled = std::mem::take(&mut self.settled);
            let mut info = Vec::with_capacity(rows.len());
            let mut settled = Vec::with_capacity(rows.len());
            let mut j = 0;
            for row in rows {
                while j < old_info.len() && old_info[j].key < row.key {
                    j += 1;
                }
                if j < old_info.len() && old_info[j].key == row.key && old_settled[j] {
                    info.push(old_info[j]);
                    settled.push(true);
                } else {
                    let (i, s) = compute(row, procs, paths, system_root, names, &mut self.scratch);
                    info.push(i);
                    settled.push(s);
                }
            }
            self.info = info;
            self.settled = settled;
        }
        &self.info
    }
    pub fn len(&self) -> usize {
        self.info.len()
    }
    pub fn is_empty(&self) -> bool {
        self.info.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::group::ImageId;
    use oled_sysmon::ProcRow;

    const ROOT: &str = "C:\\Windows";

    fn k(pid: u32) -> ProcKey {
        ProcKey { pid, created: pid as u64 * 10 }
    }

    #[test]
    fn path_class_cases() {
        assert_eq!(path_class(Some("C:\\Windows\\System32\\svchost.exe"), ROOT), PathClass::SystemRoot);
        assert_eq!(path_class(Some("c:\\windows\\explorer.exe"), ROOT), PathClass::SystemRoot);
        assert_eq!(path_class(Some("C:\\WindowsApps\\x.exe"), ROOT), PathClass::Other);
        assert_eq!(path_class(Some("C:\\Windows"), ROOT), PathClass::Other);
        assert_eq!(path_class(Some("D:\\tools\\a.exe"), ROOT), PathClass::Other);
        assert_eq!(path_class(Some("C:\\Windows\\a.exe"), "C:\\Windows\\"), PathClass::SystemRoot);
        assert_eq!(path_class(None, ROOT), PathClass::NoImage);
    }

    #[test]
    fn class_of_cases() {
        let path = ImagePath::Path("c:\\windows\\a.exe".into());
        let other = ImagePath::Path("c:\\apps\\a.exe".into());
        assert_eq!(class_of(None, ROOT), PathClass::Unknown);
        assert_eq!(class_of(Some(&ImagePath::Unknown), ROOT), PathClass::Unknown);
        assert_eq!(class_of(Some(&ImagePath::NoImage), ROOT), PathClass::NoImage);
        assert_eq!(class_of(Some(&path), ROOT), PathClass::SystemRoot);
        assert_eq!(class_of(Some(&other), ROOT), PathClass::Other);
    }

    #[test]
    fn never_suspend_reasons() {
        for n in
            ["dwm.exe", "explorer.exe", "dock.exe", "tasks.exe", "tasks-admin.exe", "sihost.exe", "fontdrvhost.exe"]
        {
            assert_eq!(never_suspend(n), Some(SuspendBlock::DesktopFreezes), "{n}");
            assert_eq!(never_suspend(&n.to_ascii_uppercase()), Some(SuspendBlock::DesktopFreezes), "{n}");
        }
        for n in ["ctfmon.exe", "TextInputHost.exe"] {
            assert_eq!(never_suspend(n), Some(SuspendBlock::TextInputFreezes), "{n}");
        }
        assert_eq!(never_suspend("AudioDG.exe"), Some(SuspendBlock::SoundStops));
        assert_eq!(never_suspend("notepad.exe"), None);
        assert_eq!(never_suspend(""), None);
    }

    fn six_rows(order: &[&str]) -> ProcTable {
        let mut t = ProcTable::new();
        for (i, name) in order.iter().enumerate() {
            let units: Vec<u16> = name.encode_utf16().collect();
            let image = t.names.intern(&units);
            t.push(ProcRow { key: k(i as u32 + 1), image, ..ProcRow::default() });
        }
        t.finish();
        t
    }

    fn map() -> PathMap {
        PathMap {
            entries: vec![
                (k(1), ImagePath::Path("C:\\Windows\\System32\\A.exe".into())),
                (k(2), ImagePath::Path("D:\\Tools\\B.exe".into())),
                (k(3), ImagePath::NoImage),
                (k(4), ImagePath::Unknown),
            ],
        }
    }

    #[test]
    fn path_map_get_is_a_binary_search() {
        let m = map();
        assert_eq!(m.get(k(3)), Some(&ImagePath::NoImage));
        assert_eq!(m.get(k(5)), None);
        assert_eq!(PathMap::default().get(k(1)), None);
    }

    #[test]
    fn info_book_fills_every_row() {
        let t = six_rows(&["A.exe", "B.EXE", "System", "c.exe", "d.exe", "e.exe"]);
        let (mut book, mut names) = (InfoBook::new(), Interner::new());
        let info = book.update(&t, &map(), ROOT, &mut names).to_vec();
        let classes: Vec<PathClass> = info.iter().map(|i| i.class).collect();
        assert_eq!(
            classes,
            [
                PathClass::SystemRoot,
                PathClass::Other,
                PathClass::NoImage,
                PathClass::Unknown,
                PathClass::Unknown,
                PathClass::Unknown
            ]
        );
        for (i, row) in t.rows.iter().enumerate() {
            assert_eq!(info[i].key, row.key);
        }
        let exes: Vec<Option<&str>> = info.iter().map(|i| i.exe.map(|e| names.get(e.0))).collect();
        assert_eq!(exes, [Some("c:\\windows\\system32\\a.exe"), Some("d:\\tools\\b.exe"), None, None, None, None]);
        assert_eq!(names.get(info[1].image.0), "b.exe");
        assert_eq!(names.get(info[2].image.0), "system");
        assert_eq!(book.len(), 6);
        assert!(!book.is_empty());
    }

    #[test]
    fn info_book_settles_once() {
        let t = six_rows(&["A.exe", "B.EXE", "System", "c.exe", "d.exe", "e.exe"]);
        let (mut book, mut names) = (InfoBook::new(), Interner::new());
        let first = book.update(&t, &map(), ROOT, &mut names).to_vec();
        let n = names.len();
        assert_eq!(book.update(&t, &map(), ROOT, &mut names), &first[..]);
        assert_eq!(names.len(), n);
        // Pending row 5 gets a path: its class and exe change, the settled rows do not.
        let mut m = map();
        m.entries.push((k(5), ImagePath::Path("c:\\windows\\d.exe".into())));
        let next = book.update(&t, &m, ROOT, &mut names).to_vec();
        assert_eq!(next[4].class, PathClass::SystemRoot);
        assert_eq!(names.get(next[4].exe.unwrap().0), "c:\\windows\\d.exe");
        assert_eq!(&next[..4], &first[..4]);
        assert_eq!(next[5], first[5]);
    }

    #[test]
    fn info_book_follows_a_changing_table() {
        let t = six_rows(&["A.exe", "B.EXE", "System", "c.exe", "d.exe", "e.exe"]);
        let (mut book, mut names) = (InfoBook::new(), Interner::new());
        book.update(&t, &map(), ROOT, &mut names);
        let smaller = six_rows(&["A.exe", "B.EXE", "System"]);
        assert_eq!(book.update(&smaller, &map(), ROOT, &mut names).len(), 3);
        assert_eq!(book.update(&t, &map(), ROOT, &mut names).len(), 6);
    }

    #[test]
    fn info_book_ids_ignore_case_and_table() {
        let a = six_rows(&["Tool.EXE", "x.exe", "y.exe"]);
        let b = six_rows(&["y.exe", "z.exe", "tool.exe"]);
        let mut names = Interner::new();
        let (mut ba, mut bb) = (InfoBook::new(), InfoBook::new());
        let ia: ImageId = ba.update(&a, &PathMap::default(), ROOT, &mut names)[0].image;
        let ib: ImageId = bb.update(&b, &PathMap::default(), ROOT, &mut names)[2].image;
        assert_eq!(ia, ib);
        assert_eq!(names.get(ia.0), "tool.exe");
    }

    #[test]
    fn info_book_fits() {
        let mut t = ProcTable::new();
        for i in 0..1000u32 {
            t.push(ProcRow { key: k(i + 1), ..ProcRow::default() });
        }
        t.finish();
        let (mut book, mut names) = (InfoBook::new(), Interner::new());
        book.update(&t, &PathMap::default(), ROOT, &mut names);
        book.update(&t, &PathMap::default(), ROOT, &mut names);
        let bytes = book.info.capacity() * std::mem::size_of::<ProcInfo>() + book.settled.capacity();
        assert!(bytes <= 64 * 1024, "{bytes}");
    }
}
