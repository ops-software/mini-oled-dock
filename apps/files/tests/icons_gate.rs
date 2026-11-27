//! The check before any image load in the shell host: which locations may be loaded, and that the host asks it first.
mod common;

use std::cell::{Cell, RefCell};

use common::temp::TempRoot;
use files_engine::drives::DriveKind;
use files_engine::icons::wants_item_icon;
use files_engine::listing::WidePath;
use files_engine::shell::images::{
    allowed_with, attrs_allow_icon, folder_is_junction, junction_from, may_load_icon_from, run_with, OWN_ICON_EXTS,
};
use oled_trust::frame::files::{Gen, ItemRef, ReqId};

fn kind(letter: u16) -> DriveKind {
    match u8::try_from(letter).map(|b| b.to_ascii_uppercase()) {
        Ok(b'C') => DriveKind::Fixed,
        Ok(b'E') => DriveKind::Removable,
        Ok(b'Z') => DriveKind::Network,
        _ => DriveKind::Unknown,
    }
}

fn units(s: &str) -> Vec<u16> {
    s.encode_utf16().collect()
}

fn wide(s: &str) -> WidePath {
    WidePath::parse(s).unwrap_or_else(|e| panic!("{s}: {e:?}"))
}

/// Attributes of a plain file and of a plain folder, for allowed_with.
fn a_file(_: &WidePath) -> Option<u32> {
    Some(0x20)
}

fn a_folder(_: &WidePath) -> Option<u32> {
    Some(0x10)
}

/// No folder above the item is a junction.
fn no_junction(_: &WidePath) -> bool {
    false
}

/// Every folder's long form is the path as given (no 8.3 alias).
fn as_given(d: &WidePath) -> Option<WidePath> {
    Some(d.clone())
}

#[test]
fn may_load_icon_from_table() {
    let rows: [(&str, bool); 34] = [
        (r"C:\made-up\tool.exe", true),
        (r"c:\made-up\tool.exe", true),
        (r"\\?\C:\made-up\tool.exe", true),
        (r"E:\made-up\tool.exe", false),
        (r"Z:\made-up\tool.exe", false),
        (r"Q:\made-up\tool.exe", false),
        (r"\\server-made-up\share\tool.exe", false),
        (r"\\?\UNC\server-made-up\share\tool.exe", false),
        (r"\\.\C:\made-up\tool.exe", false),
        (r"\\.\PhysicalDrive0", false),
        (r"\\?\GLOBALROOT\Device\made-up\tool.exe", false),
        (r"\\?\Volume{00000000-0000-0000-0000-000000000000}\tool.exe", false),
        (r"\\host-made-up@SSL\DavWWWRoot\tool.exe", false),
        (r"\\host-made-up@80\DavWWWRoot\tool.exe", false),
        ("http://host-made-up/tool.exe", false),
        (r"file:///C:/made-up/tool.exe", false),
        (r"made-up\tool.exe", false),
        (r"tool.exe", false),
        (r"C:tool.exe", false),
        (r"C:\made-up\%SystemRoot%\tool.exe", false),
        (r"C:\made-up\tool.exe:stream", false),
        (r"C:/made-up/tool.exe", false),
        (r"C:\", false),
        ("", false),
        (r"C:\made-up\tool.lnk.", false),
        (r"C:\made-up\tool.lnk ", false),
        (r"C:\made-up\tool.exe.", false),
        (r"\\?\C:\made-up\tool.exe ", false),
        (r"C:\made-up\photos.{0AFACED1-E828-11D1-9187-B532F1E9575D}\tool.exe", false),
        (r"C:\made-up\photos.{0AFACED1-E828-11D1-9187-B532F1E9575D}.\tool.exe", false),
        (r"C:\made-up\photos.\tool.exe", false),
        (r"C:\made-up\photos \tool.exe", false),
        (r"C:\made-up\tool.{0AFACED1-E828-11D1-9187-B532F1E9575D}", false),
        (r"C:\made-up\photos.2024\tool.exe", true),
    ];
    for (p, want) in rows {
        assert_eq!(may_load_icon_from(&units(p), kind), want, "{p}");
    }
}

#[test]
fn attrs_that_forbid_a_load() {
    assert!(attrs_allow_icon(0x20));
    assert!(attrs_allow_icon(0x20 | 0x1 | 0x2));
    assert!(attrs_allow_icon(0x10), "a plain folder");
    for bad in [0x14, 0x11, 0x420, 0x1020, 0x4_0020, 0x40_0020] {
        assert!(!attrs_allow_icon(bad), "{bad:#x}");
    }
}

/// Icons load only a file whose icon is its own: every kind whose icon handler reads a location written in the file,
/// the spellings Win32 parsing turns into one, and any folder are refused, whatever their attributes.
#[test]
fn icons_load_only_a_files_own_icon() {
    assert_eq!(OWN_ICON_EXTS, ["exe", "ico", "scr", "cur", "ani"], "a new kind is a reviewed change of the host rule");
    for ext in OWN_ICON_EXTS {
        for p in [format!(r"C:\made-up\a.{ext}"), format!(r"C:\made-up\A.{}", ext.to_uppercase())] {
            assert!(allowed_with(&wide(&p), false, kind, a_file, no_junction, as_given), "{p}");
        }
    }
    let refused = [
        r"C:\made-up\a.lnk",
        r"C:\made-up\a.url",
        r"C:\made-up\a.pif",
        r"C:\made-up\a.scf",
        r"C:\made-up\a.searchConnector-ms",
        r"C:\made-up\a.library-ms",
        r"C:\made-up\a.website",
        r"C:\made-up\a.msc",
        r"C:\made-up\a.appref-ms",
        r"C:\made-up\a.cpl",
        r"C:\made-up\a.theme",
        r"C:\made-up\a.txt",
        r"C:\made-up\a",
        r"C:\x\a.lnk.",
        r"C:\x\a.lnk ",
        r"C:\x\a.exe.",
        r"C:\x\a.exe ",
    ];
    for p in refused {
        assert!(!allowed_with(&wide(p), false, kind, a_file, no_junction, as_given), "{p}");
    }
    for p in [r"C:\made-up\folder", r"C:\made-up\name.{00000000-0000-0000-0000-000000000000}", r"C:\made-up\dir.exe"] {
        assert!(!allowed_with(&wide(p), false, kind, a_folder, no_junction, as_given), "a folder: {p}");
    }
}

/// Thumbnails (no caller yet) refuse every kind that names another location, with the whole extension (longer than
/// the crumb slot) and the trailing dot or space.
#[test]
fn thumbnails_refuse_kinds_that_name_another_location() {
    let kinds = [
        "lnk",
        "url",
        "appref-ms",
        "msc",
        "library-ms",
        "search-ms",
        "website",
        "cpl",
        "pif",
        "scf",
        "searchConnector-ms",
        "theme",
        "themepack",
        "deskthemepack",
    ];
    for ext in kinds {
        let p = format!(r"C:\made-up\a.{ext}");
        assert!(!allowed_with(&wide(&p), true, kind, a_file, no_junction, as_given), "{p}");
    }
    for p in [r"C:\made-up\a.lnk.", r"C:\made-up\a.lnk ", r"\\server-made-up\share\a.jpg"] {
        assert!(!allowed_with(&wide(p), true, kind, a_file, no_junction, as_given), "{p}");
    }
    assert!(allowed_with(&wide(r"C:\made-up\a.jpg"), true, kind, a_file, no_junction, as_given));
}

/// A folder's thumbnail is made from the items inside it (links, remote or cloud files this check never saw), and a
/// `name.{CLSID}` name is bound to that class, not to what the item holds: thumbnails, like icons, load no folder and
/// no name whose last extension is a class id, and the host's check refuses them before anything is loaded.
#[test]
fn thumbnails_load_no_folder_and_no_class_id_name() {
    for p in [r"C:\made-up\folder", r"C:\made-up\a.{00000000-0000-0000-0000-000000000000}", r"C:\made-up\a.jpg"] {
        assert!(!allowed_with(&wide(p), true, kind, a_folder, no_junction, as_given), "a folder: {p}");
    }
    for p in [
        r"C:\made-up\a.{00021401-0000-0000-C000-000000000046}",
        r"C:\made-up\a.jpg.{00000000-0000-0000-0000-000000000000}",
    ] {
        assert!(!allowed_with(&wide(p), true, kind, a_file, no_junction, as_given), "a class id name: {p}");
    }
    let attrs_of = |p: &WidePath| Some(if p.to_string_lossy().ends_with("folder") { 0x10 } else { 0x20 });
    let items = [
        ItemRef::Path(wide(r"C:\made-up\folder")),
        ItemRef::Path(wide(r"C:\made-up\a.{00021401-0000-0000-C000-000000000046}")),
    ];
    let loads = Cell::new(0);
    let sent = RefCell::new(Vec::<String>::new());
    run_with(
        ReqId(8),
        Gen(1_000_000),
        &items,
        true,
        &|e| sent.borrow_mut().push(format!("{e:?}")),
        &|p| allowed_with(p, true, kind, attrs_of, no_junction, as_given),
        &mut |_, _| {
            loads.set(loads.get() + 1);
            None
        },
    );
    assert_eq!(loads.get(), 0, "a refused item is never loaded");
    let sent = sent.into_inner();
    assert!(matches!(sent.as_slice(), [s] if s.starts_with("ReqDone")), "{sent:?}");
}

/// The shell parser binds a folder whose last extension is a class id (`photos.{CLSID}`) to that class although the
/// file system lists it as a plain folder: a folder shortcut sends every path through it to its target, a remote share
/// too. Win32 parsing drops a trailing dot, so a folder `photos.` is read as `photos`. No image of an item under such
/// a folder is loaded, whichever folder of the path it is.
#[test]
fn no_image_under_a_class_id_or_dotted_folder() {
    let shortcut = r"C:\made-up\photos.{0AFACED1-E828-11D1-9187-B532F1E9575D}";
    let dotted = format!("{shortcut}.");
    let parents = [shortcut, dotted.as_str(), r"C:\made-up\photos.", r"C:\made-up\photos ", r"C:\made-up\.{0-0}\sub"];
    for parent in parents {
        let icon = format!(r"{parent}\a.exe");
        assert!(!allowed_with(&wide(&icon), false, kind, a_file, no_junction, as_given), "{icon}");
        let thumb = format!(r"{parent}\a.jpg");
        assert!(!allowed_with(&wide(&thumb), true, kind, a_file, no_junction, as_given), "{thumb}");
    }
    let deep = format!(r"{shortcut}\sub\a.exe");
    assert!(!allowed_with(&wide(&deep), false, kind, a_file, no_junction, as_given), "{deep}");
    assert!(allowed_with(&wide(r"C:\made-up\photos.2024\a.exe"), false, kind, a_file, no_junction, as_given));
    assert!(allowed_with(&wide(r"C:\made-up\photos.2024\a.jpg"), true, kind, a_file, no_junction, as_given));
}

/// The shell parser reads the long name of every folder on the path, the rule the names of the path as given: an 8.3
/// alias (PHOTOS for photos.{CLSID}) would hide the class id. The rule judges the long form of the item's folder too,
/// read only after the location and the attributes passed, and no long form refuses; nothing refused is loaded.
#[test]
fn no_image_under_a_short_alias_of_a_class_id_folder() {
    let shortcut = r"C:\made-up\photos.{0AFACED1-E828-11D1-9187-B532F1E9575D}";
    let long_of = |d: &WidePath| match d.to_string_lossy().as_str() {
        r"C:\made-up\PHOTOS" => Some(wide(shortcut)),
        r"C:\made-up\PHOTOS\sub" => Some(wide(&format!(r"{shortcut}\sub"))),
        r"C:\made-up\GONE" => None,
        _ => Some(d.clone()),
    };
    let refused = [
        r"C:\made-up\PHOTOS\a.exe",
        r"C:\made-up\PHOTOS\sub\a.exe",
        r"C:\made-up\PHOTOS\a.jpg",
        r"C:\made-up\GONE\a.exe",
    ];
    for p in refused {
        for is_thumb in [false, true] {
            assert!(!allowed_with(&wide(p), is_thumb, kind, a_file, no_junction, long_of), "{p} (thumb {is_thumb})");
        }
    }
    assert!(allowed_with(&wide(r"C:\made-up\plain\a.exe"), false, kind, a_file, no_junction, long_of));
    assert!(allowed_with(&wide(r"C:\made-up\plain\a.jpg"), true, kind, a_file, no_junction, long_of));
    let items = [ItemRef::Path(wide(r"C:\made-up\PHOTOS\a.exe")), ItemRef::Path(wide(r"C:\made-up\PHOTOS\b.jpg"))];
    for is_thumb in [false, true] {
        let loads = Cell::new(0);
        let sent = RefCell::new(Vec::<String>::new());
        run_with(
            ReqId(10),
            Gen(1_000_000),
            &items,
            is_thumb,
            &|e| sent.borrow_mut().push(format!("{e:?}")),
            &|p| allowed_with(p, is_thumb, kind, a_file, no_junction, long_of),
            &mut |_, _| {
                loads.set(loads.get() + 1);
                None
            },
        );
        assert_eq!(loads.get(), 0, "an item under a short alias is never loaded (thumb {is_thumb})");
        let sent = sent.into_inner();
        assert!(matches!(sent.as_slice(), [s] if s.starts_with("ReqDone")), "{sent:?}");
    }
}

#[test]
fn ui_asks_only_for_plain_local_own_icons() {
    let local = units(r"C:\made-up\x.exe");
    for ext in ["exe", "ico", "scr", "cur", "ani"] {
        assert!(wants_item_icon(ext.as_bytes(), 0x20, &local), "{ext}");
    }
    for ext in ["msc", "lnk", "url", "appref-ms", "txt", "pif", "scf", "searchconnector-ms", "theme", "cpl", ""] {
        assert!(!wants_item_icon(ext.as_bytes(), 0x20, &local), "{ext}");
    }
    // The UI asks for a kind exactly when the host would load it.
    for ext in ["exe", "ico", "scr", "cur", "ani", "lnk", "pif", "scf", "png", "dll"] {
        let p = wide(&format!(r"C:\made-up\a.{ext}"));
        assert_eq!(
            wants_item_icon(ext.as_bytes(), 0x20, p.units()),
            allowed_with(&p, false, kind, a_file, no_junction, as_given),
            "{ext}"
        );
    }
    assert!(!wants_item_icon(b"exe", 0x420, &local), "a link");
    assert!(!wants_item_icon(b"exe", 0x40_0420, &local), "a dehydrated file");
    assert!(!wants_item_icon(b"exe", 0x1020, &local), "offline");
    for p in [r"\\server-made-up\share\x.exe", r"\\?\UNC\server-made-up\share\x.exe", "x.exe"] {
        assert!(!wants_item_icon(b"exe", 0x20, &units(p)), "{p}");
    }
}

#[test]
fn refused_items_are_never_loaded_and_the_request_still_ends() {
    let item = |s: &str| ItemRef::Path(WidePath::parse(s).unwrap());
    let items = [item(r"C:\made-up\ok.exe"), item(r"\\server-made-up\share\bad.exe"), item(r"C:\made-up\bad2.exe")];
    let order = RefCell::new(Vec::<String>::new());
    let checked = Cell::new(0);
    let sent = RefCell::new(Vec::<String>::new());
    run_with(
        ReqId(7),
        Gen(1_000_000),
        &items,
        false,
        &|e| sent.borrow_mut().push(format!("{e:?}")),
        &|p| {
            checked.set(checked.get() + 1);
            order.borrow_mut().push(format!("check {}", p.to_string_lossy()));
            !p.to_string_lossy().contains("bad")
        },
        &mut |p, _| {
            order.borrow_mut().push(format!("load {}", p.to_string_lossy()));
            None
        },
    );
    assert_eq!(checked.get(), 3, "every item is checked");
    let order = order.into_inner();
    assert_eq!(
        order,
        [
            r"check C:\made-up\ok.exe",
            r"load C:\made-up\ok.exe",
            r"check \\server-made-up\share\bad.exe",
            r"check C:\made-up\bad2.exe",
        ],
        "the check comes before each load and a refused item is never loaded"
    );
    let sent = sent.into_inner();
    assert_eq!(sent.len(), 1, "{sent:?}");
    assert!(matches!(sent[0].as_str(), s if s.starts_with("ReqDone")), "{sent:?}");
}

/// The host reads an item's attributes, and those of the folders above it, only after its kind and location passed:
/// reading them is itself a request to a UNC or WebDAV server. (A `scheme:` path is never a WidePath;
/// may_load_icon_from_table covers its units.)
#[test]
fn attributes_are_read_only_after_the_location_passed() {
    let read = RefCell::new(Vec::<String>::new());
    let attrs_of = |p: &WidePath| {
        read.borrow_mut().push(p.to_string_lossy());
        Some(0x20)
    };
    let folders = RefCell::new(Vec::<String>::new());
    let junction = |d: &WidePath| {
        folders.borrow_mut().push(d.to_string_lossy());
        false
    };
    let longs = RefCell::new(Vec::<String>::new());
    let long_of = |d: &WidePath| {
        longs.borrow_mut().push(d.to_string_lossy());
        Some(d.clone())
    };
    let refused = [
        r"\\server-made-up\share\x.exe",
        r"\\?\UNC\server-made-up\share\x.exe",
        r"\\host-made-up@SSL\DavWWWRoot\x.exe",
        r"\\host-made-up@80\DavWWWRoot\x.exe",
        r"Z:\made-up\x.exe",
        r"E:\made-up\x.exe",
        r"C:\made-up\%SystemRoot%\x.exe",
        r"C:\made-up\x.exe.",
        r"C:\made-up\x.lnk",
        r"\\server-made-up\share\x.lnk",
    ];
    for p in refused {
        for is_thumb in [false, true] {
            assert!(!allowed_with(&wide(p), is_thumb, kind, attrs_of, junction, long_of), "{p} (thumb {is_thumb})");
        }
    }
    assert_eq!(read.borrow().len(), 0, "attributes read for a refused item: {:?}", read.borrow());
    assert_eq!(folders.borrow().len(), 0, "folders read for a refused item: {:?}", folders.borrow());
    assert_eq!(longs.borrow().len(), 0, "long forms read for a refused item: {:?}", longs.borrow());
    assert!(allowed_with(&wide(r"C:\made-up\x.exe"), false, kind, attrs_of, junction, long_of));
    assert_eq!(*read.borrow(), [r"C:\made-up\x.exe"], "a local item that passed is read once");
    assert_eq!(*folders.borrow(), [r"C:\", r"C:\made-up"], "every folder above it is checked, the root first");
    assert_eq!(*longs.borrow(), [r"C:\made-up"], "the long form of its folder is read once");
}

/// A read-only or system folder whose desktop.ini names a CLSID or CLSID2 is bound by the shell parser to that class
/// (a folder shortcut again) although the file system lists it as a plain folder: the host refuses an item when any
/// folder above it is such a junction, and never loads it.
#[test]
fn no_image_under_a_junction_folder() {
    let junction = |d: &WidePath| d.to_string_lossy() == r"C:\made-up\share";
    for p in [r"C:\made-up\share\a.exe", r"C:\made-up\share\sub\a.exe"] {
        assert!(!allowed_with(&wide(p), false, kind, a_file, junction, as_given), "{p}");
    }
    assert!(!allowed_with(&wide(r"C:\made-up\share\a.jpg"), true, kind, a_file, junction, as_given));
    assert!(allowed_with(&wide(r"C:\made-up\plain\a.exe"), false, kind, a_file, junction, as_given));
    assert!(allowed_with(&wide(r"C:\made-up\plain\a.jpg"), true, kind, a_file, junction, as_given));
    let items = [ItemRef::Path(wide(r"C:\made-up\share\a.exe")), ItemRef::Path(wide(r"C:\made-up\share\sub\b.jpg"))];
    for is_thumb in [false, true] {
        let loads = Cell::new(0);
        let sent = RefCell::new(Vec::<String>::new());
        run_with(
            ReqId(9),
            Gen(1_000_000),
            &items,
            is_thumb,
            &|e| sent.borrow_mut().push(format!("{e:?}")),
            &|p| allowed_with(p, is_thumb, kind, a_file, junction, as_given),
            &mut |_, _| {
                loads.set(loads.get() + 1);
                None
            },
        );
        assert_eq!(loads.get(), 0, "an item under a junction folder is never loaded (thumb {is_thumb})");
        let sent = sent.into_inner();
        assert!(matches!(sent.as_slice(), [s] if s.starts_with("ReqDone")), "{sent:?}");
    }
}

/// The folder rule with its facts injected: a folder whose attributes cannot be read, or a file, counts as a junction
/// and a plain folder does not, without a look at a desktop.ini. For a read-only or system folder the attributes of
/// its desktop.ini come first: none is no junction; a link, a folder, an offline or a dehydrated file counts as one and
/// is never read (reading would follow the link or download the file); only a plain file is read for a class.
#[test]
fn junction_rule_reads_only_a_plain_desktop_ini() {
    let ini_reads = Cell::new(0);
    let class_reads = Cell::new(0);
    let judge = |dir: Option<u32>, ini: Option<u32>, names_class: bool| {
        junction_from(
            dir,
            || {
                ini_reads.set(ini_reads.get() + 1);
                ini
            },
            || {
                class_reads.set(class_reads.get() + 1);
                names_class
            },
        )
    };
    for (dir, want) in [(None, true), (Some(0x20), true), (Some(0x10), false), (Some(0x12), false)] {
        assert_eq!(judge(dir, Some(0x26), true), want, "{dir:?}");
    }
    assert_eq!((ini_reads.get(), class_reads.get()), (0, 0), "no desktop.ini is looked at for these");
    for dir in [0x11, 0x14, 0x16] {
        assert!(!judge(Some(dir), None, true), "{dir:#x}: no desktop.ini");
        for not_plain in [0x400, 0x1000, 0x4_0000, 0x40_0000, 0x10] {
            let ini = 0x26 | not_plain;
            assert!(judge(Some(dir), Some(ini), false), "{dir:#x}: desktop.ini {ini:#x}");
        }
    }
    assert_eq!(class_reads.get(), 0, "a desktop.ini that is missing, a link, a folder or remote data is never read");
    for dir in [0x11, 0x14] {
        assert!(judge(Some(dir), Some(0x26), true), "{dir:#x}: a class");
        assert!(!judge(Some(dir), Some(0x27), false), "{dir:#x}: no class");
    }
    assert_eq!(class_reads.get(), 4, "a plain desktop.ini is read");
}

/// The real folder facts: a read-only folder whose desktop.ini names CLSID or CLSID2 in [.ShellClassInfo] is a
/// junction, also past MAX_PATH; the same desktop.ini without the attribute, a read-only folder whose desktop.ini
/// names only a display name or a UICLSID, and a plain folder are not; a missing folder or a file counts as one, and
/// so does a read-only folder whose desktop.ini is a link (it is never opened: a link can point at a remote file). A
/// system folder that is not read-only is judged as a read-only one.
#[test]
fn junction_folders_on_disk() {
    let root = TempRoot::new("junction-folders");
    let at = |rel: &str| WidePath::parse(&root.at(rel).to_string_lossy()).unwrap();
    let class = "{0AFACED1-E828-11D1-9187-B532F1E9575D}";
    let long = format!("{}\\{}\\{}", "d".repeat(100), "e".repeat(100), "f".repeat(100));
    let cases = [
        ("clsid", format!("CLSID={class}"), true, true),
        ("clsid2", format!("CLSID2={class}"), true, true),
        ("no-attr", format!("CLSID2={class}"), false, false),
        ("named", "LocalizedResourceName=made-up".to_string(), true, false),
        ("ui", format!("UICLSID={class}"), true, false),
        (long.as_str(), format!("CLSID2={class}"), true, true),
    ];
    for (dir, line, read_only, want) in cases {
        root.file(&format!("{dir}\\desktop.ini"), format!("[.ShellClassInfo]\r\n{line}\r\n").as_bytes());
        root.set_readonly(dir, read_only);
        assert_eq!(folder_is_junction(&at(dir)), want, "{dir}: {line} (read-only {read_only})");
    }
    assert!(root.at(&long).to_string_lossy().len() > 260, "the long case is past MAX_PATH");
    root.file(r"sys\desktop.ini", format!("[.ShellClassInfo]\r\nCLSID2={class}\r\n").as_bytes());
    root.set_system("sys", true);
    assert!(folder_is_junction(&at("sys")), "a system folder, not read-only, with CLSID2");
    root.dir("plain");
    assert!(!folder_is_junction(&at("plain")));
    root.file("a-file", b"x");
    assert!(folder_is_junction(&at("a-file")), "not a folder");
    assert!(folder_is_junction(&at("missing")), "a folder that cannot be read");
    let target = root.dir("ini-target");
    root.dir("linked");
    root.junction(r"linked\desktop.ini", &target);
    root.set_readonly("linked", true);
    assert!(folder_is_junction(&at("linked")), "a desktop.ini that is a link counts as one");
}

/// Every folder above the item is judged from the drive's root down, and all of them before the item's own attributes
/// and the long form of its folder are read: each read then passes only folders already judged plain, so a folder that
/// is a link to a remote share is never read through. The first folder refused ends the check.
#[test]
fn folders_are_judged_from_the_root_down_before_the_item_is_read() {
    let log = RefCell::new(Vec::<String>::new());
    let attrs_of = |p: &WidePath| {
        log.borrow_mut().push(format!("item {}", p.to_string_lossy()));
        Some(0x20)
    };
    let junction = |d: &WidePath| {
        log.borrow_mut().push(format!("folder {}", d.to_string_lossy()));
        d.to_string_lossy() == r"C:\made-up\link"
    };
    let long_of = |d: &WidePath| {
        log.borrow_mut().push(format!("long {}", d.to_string_lossy()));
        Some(d.clone())
    };
    assert!(allowed_with(&wide(r"C:\made-up\plain\a.exe"), false, kind, attrs_of, junction, long_of));
    let want = [
        r"folder C:\",
        r"folder C:\made-up",
        r"folder C:\made-up\plain",
        r"item C:\made-up\plain\a.exe",
        r"long C:\made-up\plain",
    ];
    assert_eq!(*log.borrow(), want);
    log.borrow_mut().clear();
    for is_thumb in [false, true] {
        let p = if is_thumb { r"C:\made-up\link\sub\a.jpg" } else { r"C:\made-up\link\sub\a.exe" };
        assert!(!allowed_with(&wide(p), is_thumb, kind, attrs_of, junction, long_of), "{p}");
        let want = [r"folder C:\", r"folder C:\made-up", r"folder C:\made-up\link"];
        assert_eq!(*log.borrow(), want, "nothing below the refused folder, nor the item, is read");
        log.borrow_mut().clear();
    }
}

/// A folder that is itself a reparse point (a junction, or a symbolic link, which can point at a remote share), offline
/// or dehydrated counts as a junction before anything inside it is looked at: its desktop.ini is never read, so no read
/// goes through it, whether or not it is read-only or system.
#[test]
fn junction_rule_refuses_a_linked_or_remote_folder() {
    let ini_reads = Cell::new(0);
    let class_reads = Cell::new(0);
    for not_plain in [0x400, 0x1000, 0x4_0000, 0x40_0000] {
        for dir in [0x10, 0x11, 0x14, 0x16] {
            let a = dir | not_plain;
            let judged = junction_from(
                Some(a),
                || {
                    ini_reads.set(ini_reads.get() + 1);
                    None
                },
                || {
                    class_reads.set(class_reads.get() + 1);
                    false
                },
            );
            assert!(judged, "{a:#x}");
        }
    }
    assert_eq!((ini_reads.get(), class_reads.get()), (0, 0), "nothing inside such a folder is read");
}

/// The real folder facts: a junction (a mount point, which any user can make) above the item counts as a junction, a
/// plain folder beside it does not.
#[test]
fn a_linked_folder_on_disk_counts_as_a_junction() {
    let root = TempRoot::new("junction-link");
    let at = |rel: &str| WidePath::parse(&root.at(rel).to_string_lossy()).unwrap();
    let target = root.dir("target");
    root.dir(r"target\inner");
    root.junction("link", &target);
    assert!(folder_is_junction(&at("link")), "a folder that is a link");
    assert!(!folder_is_junction(&at("target")), "a plain folder");
    assert!(!folder_is_junction(&at(r"target\inner")), "a plain folder under it");
}
