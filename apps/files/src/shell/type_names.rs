//! Type names served by the shell host (P3-files-app F2.3): the shell's name of each extension's type, asked of
//! SHGetFileInfoW from the extension alone (SHGFI_USEFILEATTRIBUTES: no file is opened), on an image STA inside a
//! crumbs guard, sent as TypeNames frames split by bytes; the caller sends ReqDone after the last.

use oled_trust::frame::files::{CallKind, ReqId, ShellEvt, TypeName, MAX_LIST, SHELL};
use oled_trust::frame::split_by_bytes;
use windows::core::HSTRING;
use windows::Win32::Storage::FileSystem::FILE_ATTRIBUTE_NORMAL;
use windows::Win32::UI::Shell::{SHGetFileInfoW, SHFILEINFOW, SHGFI_TYPENAME, SHGFI_USEFILEATTRIBUTES};

use crate::shell::crumbs;

/// The most bytes of a name on the wire (TypeName.name).
pub const NAME_MAX: usize = 260;

/// One TypeNames frame per slice of the names found (none when nothing was found), in the order asked; an extension
/// the shell has no name for is left out.
pub fn serve(req: ReqId, exts: &[Box<str>], out: &mut dyn FnMut(ShellEvt)) -> Result<(), i32> {
    serve_with(req, exts, &mut type_name, out)
}

/// serve over another lookup (tests).
pub fn serve_with(
    req: ReqId,
    exts: &[Box<str>],
    lookup: &mut dyn FnMut(&str) -> Option<String>,
    out: &mut dyn FnMut(ShellEvt),
) -> Result<(), i32> {
    let mut names = Vec::with_capacity(exts.len());
    for ext in exts {
        let found = {
            let _g = crumbs::enter(CallKind::ClassIcon, ext);
            lookup(ext)
        };
        let Some(found) = found else { continue };
        let name = cut(found.trim(), NAME_MAX);
        if !name.is_empty() {
            names.push(TypeName { ext: ext.clone(), name: name.into() });
        }
    }
    // The payload of a TypeNames frame is the request id and the list.
    for part in split_by_bytes(&names, 4, &SHELL, MAX_LIST) {
        if !part.is_empty() {
            out(ShellEvt::TypeNames { req, names: part.to_vec() });
        }
    }
    Ok(())
}

/// The shell's name of the type of ".<ext>" ("" asks for a name without an extension); None when it has none.
fn type_name(ext: &str) -> Option<String> {
    let name = HSTRING::from(if ext.is_empty() { "file".to_string() } else { format!(".{ext}") });
    let mut sfi = SHFILEINFOW::default();
    // SAFETY: a NUL-terminated name that SHGFI_USEFILEATTRIBUTES keeps from being opened; sfi is writable and its size
    // is passed.
    let r = unsafe {
        SHGetFileInfoW(
            &name,
            FILE_ATTRIBUTE_NORMAL,
            Some(&mut sfi),
            std::mem::size_of::<SHFILEINFOW>() as u32,
            SHGFI_USEFILEATTRIBUTES | SHGFI_TYPENAME,
        )
    };
    if r == 0 {
        return None;
    }
    let n = sfi.szTypeName.iter().position(|&u| u == 0).unwrap_or(sfi.szTypeName.len());
    sfi.szTypeName.get(..n).map(String::from_utf16_lossy)
}

/// `s` cut to at most `max` bytes, at a character boundary.
pub fn cut(s: &str, max: usize) -> &str {
    let mut end = s.len().min(max);
    while !s.is_char_boundary(end) {
        end -= 1;
    }
    s.get(..end).unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn names_of(evts: &[ShellEvt]) -> Vec<(String, String)> {
        evts.iter()
            .flat_map(|e| match e {
                ShellEvt::TypeNames { names, .. } => names.clone(),
                e => panic!("{e:?}"),
            })
            .map(|t| (t.ext.to_string(), t.name.to_string()))
            .collect()
    }

    #[test]
    fn found_names_in_order_unknown_left_out() {
        let exts: Vec<Box<str>> = vec!["txt".into(), "mod-files-noapp".into(), "pdf".into(), "".into()];
        let mut asked = Vec::new();
        let mut lookup = |e: &str| {
            asked.push(e.to_string());
            match e {
                "txt" => Some("Text Document".to_string()),
                "pdf" => Some("  PDF Document ".to_string()),
                "" => Some("File".to_string()),
                _ => None,
            }
        };
        let mut evts = Vec::new();
        assert_eq!(serve_with(ReqId(3), &exts, &mut lookup, &mut |e| evts.push(e)), Ok(()));
        assert_eq!(asked, ["txt", "mod-files-noapp", "pdf", ""], "every extension asked once, in order");
        assert_eq!(evts.len(), 1, "one frame holds them");
        assert!(matches!(evts[0], ShellEvt::TypeNames { req: ReqId(3), .. }));
        let want = [("txt", "Text Document"), ("pdf", "PDF Document"), ("", "File")];
        assert_eq!(names_of(&evts), want.map(|(e, n)| (e.to_string(), n.to_string())));
    }

    #[test]
    fn nothing_found_sends_no_frame() {
        let exts: Vec<Box<str>> = vec!["a".into(), "b".into()];
        let mut evts = Vec::new();
        let mut blank = |_: &str| Some("   ".to_string());
        assert_eq!(serve_with(ReqId(1), &exts, &mut blank, &mut |e| evts.push(e)), Ok(()));
        let mut none = |_: &str| None;
        assert_eq!(serve_with(ReqId(1), &exts, &mut none, &mut |e| evts.push(e)), Ok(()));
        assert!(evts.is_empty(), "an empty TypeNames is never sent: the decoder refuses it");
        assert_eq!(serve_with(ReqId(1), &[], &mut none, &mut |e| evts.push(e)), Ok(()));
        assert!(evts.is_empty());
    }

    #[test]
    fn long_names_are_cut_at_a_character_boundary() {
        assert_eq!(cut("abc", 5), "abc");
        assert_eq!(cut("abcdef", 3), "abc");
        assert_eq!(cut("\u{e9}\u{e9}", 3), "\u{e9}", "a two-byte character is not split");
        assert_eq!(cut("\u{1F600}", 3), "");
        let long = "\u{20AC}".repeat(100);
        let exts: Vec<Box<str>> = vec!["x".into()];
        let mut evts = Vec::new();
        let mut lookup = |_: &str| Some(long.clone());
        serve_with(ReqId(1), &exts, &mut lookup, &mut |e| evts.push(e)).unwrap();
        let (_, name) = names_of(&evts).remove(0);
        assert_eq!(name.len(), 258, "86 three-byte characters: the most that fit 260 bytes");
        assert!(name.chars().all(|c| c == '\u{20AC}'));
    }

    #[test]
    fn many_names_split_by_count() {
        let exts: Vec<Box<str>> = (0..MAX_LIST as usize + 5).map(|i| format!("e{i}").into()).collect();
        let mut evts = Vec::new();
        let mut lookup = |e: &str| Some(format!("{e} File"));
        serve_with(ReqId(9), &exts, &mut lookup, &mut |e| evts.push(e)).unwrap();
        assert_eq!(evts.len(), 2);
        assert_eq!(names_of(&evts).len(), exts.len());
    }
}
