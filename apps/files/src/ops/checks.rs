//! Checks every item and name passes before an operation is queued (OP2), and again by the runner before each chunk
//! reaches IFileOperation. Pure.
//!
//! - check_item: a volume root or a share root is refused (Root) for Recycle, DeletePermanent, Move and Rename (open
//!   point 10); in a debug build started with FILES_TEST_ROOT, every item and destination outside those roots is
//!   refused (OutsideTestRoot, rule D3).
//! - check_name: the one name of a Rename or NewFolder.
//! - same_parent: a Recycle or DeletePermanent selection comes from one folder.

use oled_trust::frame::files::{ItemRef, OpKind, Refusal, WidePath, MAX_NAME_UNITS};

/// The test roots: FILES_TEST_ROOT=<root>[;<root>...] in a debug build. None (no fence) when the variable is unset or
/// in a release build; a set variable with a part that is not an absolute path fences everything out (Some(empty)).
pub fn test_roots_from(debug: bool, var: Option<&str>) -> Option<Vec<WidePath>> {
    if !debug {
        return None;
    }
    let var = var?;
    let mut roots = Vec::new();
    for part in var.split(';').filter(|p| !p.trim().is_empty()) {
        match WidePath::parse(part.trim()) {
            Ok(r) => roots.push(r),
            Err(_) => return Some(Vec::new()),
        }
    }
    Some(roots)
}

/// FILES_TEST_ROOT of this process (debug builds only).
pub fn test_roots() -> Option<Vec<WidePath>> {
    test_roots_from(cfg!(debug_assertions), std::env::var("FILES_TEST_ROOT").ok().as_deref())
}

/// One UTF-16 unit folded for a case-insensitive compare: ASCII and the BMP letters whose upper case is one unit.
fn fold(u: u16) -> u16 {
    if (u16::from(b'a')..=u16::from(b'z')).contains(&u) {
        return u - 32;
    }
    if u < 0x80 {
        return u;
    }
    match char::from_u32(u32::from(u)) {
        Some(c) => {
            let mut up = c.to_uppercase();
            match (up.next(), up.next()) {
                (Some(x), None) => u16::try_from(u32::from(x)).unwrap_or(u),
                _ => u,
            }
        }
        None => u,
    }
}

/// The path's units folded for comparisons and map keys.
pub fn folded(p: &[u16]) -> Vec<u16> {
    p.iter().map(|&u| fold(u)).collect()
}

/// Two paths name the same item (case-insensitive).
pub fn same_path(a: &WidePath, b: &WidePath) -> bool {
    a.units().len() == b.units().len() && a.units().iter().zip(b.units()).all(|(&x, &y)| fold(x) == fold(y))
}

/// `p` is `root` or lies under it (case-insensitive, whole components).
pub fn within(p: &WidePath, root: &WidePath) -> bool {
    let (p, r) = (p.units(), root.units());
    if p.len() < r.len() || !p.iter().zip(r).all(|(&x, &y)| fold(x) == fold(y)) {
        return false;
    }
    p.len() == r.len() || r.last() == Some(&u16::from(b'\\')) || p.get(r.len()) == Some(&u16::from(b'\\'))
}

/// An item (or destination) of an operation of `kind`.
pub fn check_item(p: &WidePath, kind: OpKind, roots: Option<&[WidePath]>) -> Result<(), Refusal> {
    let destructive = matches!(kind, OpKind::Recycle | OpKind::DeletePermanent | OpKind::Move | OpKind::Rename);
    if destructive && p.is_root() {
        return Err(Refusal::Root);
    }
    if let Some(roots) = roots {
        if !roots.iter().any(|r| within(p, r)) {
            return Err(Refusal::OutsideTestRoot);
        }
    }
    Ok(())
}

/// The path of a staged item: the ops host works on paths only (a PIDL is a hand-off to the shell host).
pub fn item_path(r: &ItemRef) -> Result<&WidePath, Refusal> {
    match r {
        ItemRef::Path(p) => Ok(p),
        ItemRef::Pidl(_) => Err(Refusal::BadPath),
    }
}

/// The one name of a Rename or NewFolder: 1-255 UTF-16 units, none of \ / : * ? " < > | or a control character, not
/// "." or "..", no trailing space or dot (Win32 would strip them), not a reserved device name (CON, PRN, AUX, NUL,
/// COM1-9, LPT1-9, also with an extension).
pub fn check_name(name: &str) -> Result<(), Refusal> {
    let units = name.encode_utf16().count();
    if units == 0 || units > usize::from(MAX_NAME_UNITS) {
        return Err(Refusal::BadName);
    }
    if name.chars().any(|c| (c as u32) < 0x20 || matches!(c, '\\' | '/' | ':' | '*' | '?' | '"' | '<' | '>' | '|')) {
        return Err(Refusal::BadName);
    }
    if name == "." || name == ".." || name.ends_with([' ', '.']) {
        return Err(Refusal::BadName);
    }
    let base = name.split('.').next().unwrap_or_default().trim_end_matches(' ').to_ascii_uppercase();
    let reserved = matches!(base.as_str(), "CON" | "PRN" | "AUX" | "NUL")
        || ((base.starts_with("COM") || base.starts_with("LPT"))
            && base.len() == 4
            && base.as_bytes().get(3).is_some_and(|d| (b'1'..=b'9').contains(d)));
    if reserved {
        return Err(Refusal::BadName);
    }
    Ok(())
}

/// Every item of a Recycle or DeletePermanent selection has the same parent folder.
pub fn same_parent(items: &[WidePath]) -> Result<(), Refusal> {
    let mut parents = items.iter().map(WidePath::parent);
    let Some(first) = parents.next() else { return Ok(()) };
    for p in parents {
        let same = match (&first, &p) {
            (Some(a), Some(b)) => same_path(a, b),
            (None, None) => true,
            _ => false,
        };
        if !same {
            return Err(Refusal::MixedParents);
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn p(s: &str) -> WidePath {
        WidePath::parse(s).unwrap()
    }

    const DESTRUCTIVE: [OpKind; 4] = [OpKind::Recycle, OpKind::DeletePermanent, OpKind::Move, OpKind::Rename];

    #[test]
    fn roots_are_refused_for_destructive_kinds() {
        for root in ["C:\\", "\\\\srv\\share", "D:\\"] {
            for kind in DESTRUCTIVE {
                assert_eq!(check_item(&p(root), kind, None), Err(Refusal::Root), "{root} {kind:?}");
            }
            for kind in [OpKind::Copy, OpKind::NewFolder] {
                assert_eq!(check_item(&p(root), kind, None), Ok(()), "a root may be copied from or into: {root}");
            }
        }
        for kind in DESTRUCTIVE {
            assert_eq!(check_item(&p("C:\\a"), kind, None), Ok(()));
            assert_eq!(check_item(&p("\\\\srv\\share\\a"), kind, None), Ok(()));
        }
    }

    #[test]
    fn test_roots_fence_everything_else() {
        let roots = [p("C:\\T\\mod-files-test-1-0"), p("C:\\T\\mod-files-test-1-1")];
        let r = Some(&roots[..]);
        assert_eq!(check_item(&p("C:\\T\\mod-files-test-1-0\\a.txt"), OpKind::DeletePermanent, r), Ok(()));
        assert_eq!(check_item(&p("c:\\t\\MOD-FILES-TEST-1-1\\x\\y"), OpKind::Move, r), Ok(()), "case-insensitive");
        assert_eq!(check_item(&p("C:\\T\\mod-files-test-1-0"), OpKind::Copy, r), Ok(()), "the root itself");
        for out in ["C:\\T\\mod-files-test-1-00\\a", "C:\\T\\a", "C:\\T\\mod-files-test-1", "D:\\x"] {
            assert_eq!(check_item(&p(out), OpKind::DeletePermanent, r), Err(Refusal::OutsideTestRoot), "{out}");
            assert_eq!(check_item(&p(out), OpKind::Copy, r), Err(Refusal::OutsideTestRoot), "{out}");
        }
        assert_eq!(check_item(&p("C:\\x"), OpKind::Copy, Some(&[])), Err(Refusal::OutsideTestRoot), "empty fence");
        assert_eq!(check_item(&p("C:\\"), OpKind::Recycle, r), Err(Refusal::Root), "Root is checked first");
        assert!(within(&p("C:\\a\\b"), &p("C:\\")), "a drive root holds everything on it");
    }

    #[test]
    fn test_roots_are_debug_only() {
        assert_eq!(test_roots_from(false, Some("C:\\T")), None, "release ignores FILES_TEST_ROOT");
        assert_eq!(test_roots_from(true, None), None);
        assert_eq!(test_roots_from(true, Some("C:\\A;D:\\B")), Some(vec![p("C:\\A"), p("D:\\B")]));
        assert_eq!(test_roots_from(true, Some("C:\\A;relative")), Some(vec![]), "a bad part fences everything out");
        assert_eq!(test_roots_from(true, Some("")), Some(vec![]));
    }

    #[test]
    fn names() {
        for ok in ["a", "New folder", "a.b.c", "con1", "COM0", "LPT10", "x y", "ünïcode", ".hidden", "a..b"] {
            assert_eq!(check_name(ok), Ok(()), "{ok}");
        }
        let long = "a".repeat(255);
        assert_eq!(check_name(&long), Ok(()));
        let too_long = "a".repeat(256);
        for bad in [
            "",
            ".",
            "..",
            "a ",
            "a.",
            "a\\b",
            "a/b",
            "a:b",
            "a*",
            "a?",
            "a\"",
            "a<",
            "a>",
            "a|",
            "a\tb",
            "a\u{1}",
            "CON",
            "con",
            "Con.txt",
            "PRN",
            "AUX",
            "NUL",
            "nul.tar.gz",
            "COM1",
            "com9.x",
            "LPT1",
            "lpt9",
            "CON .txt",
        ] {
            assert_eq!(check_name(bad), Err(Refusal::BadName), "{bad:?}");
        }
        assert_eq!(check_name(&too_long), Err(Refusal::BadName));
    }

    #[test]
    fn parents() {
        assert_eq!(same_parent(&[p("C:\\a\\x"), p("c:\\A\\y")]), Ok(()));
        assert_eq!(same_parent(&[p("C:\\a\\x"), p("C:\\b\\y")]), Err(Refusal::MixedParents));
        assert_eq!(same_parent(&[]), Ok(()));
        assert_eq!(same_parent(&[p("C:\\a\\x")]), Ok(()));
        assert_eq!(item_path(&ItemRef::Pidl(Box::new([1, 2]))), Err(Refusal::BadPath));
        assert_eq!(item_path(&ItemRef::Path(p("C:\\a"))), Ok(&p("C:\\a")));
        assert!(!same_path(&p("C:\\Straße"), &p("c:\\STRASSE")), "one unit per unit only");
        assert!(same_path(&p("C:\\é"), &p("c:\\É")));
    }
}
