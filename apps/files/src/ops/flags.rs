//! The IFileOperation flags of each operation (OP2). Pure: the host passes its own pre-flight collision count for the
//! chunk being run (never a number from the UI), the FS-3 a constant and the test switch.
//!
//! - Copy, Move: FOF_ALLOWUNDO | FOF_NOCONFIRMMKDIR | FOF_SILENT, except that "Decide each" (AskSystem) with collisions
//!   in the chunk runs without FOF_SILENT unless the system's dialogs show under it (DIALOGS_UNDER_SILENT); ReplaceAll
//!   adds FOF_NOCONFIRMATION. SkipAll is never a flag: the pre-flight removes the colliding items.
//! - Recycle: FOF_ALLOWUNDO | FOF_WANTNUKEWARNING | FOF_SILENT, never FOF_NOCONFIRMATION.
//! - DeletePermanent: FOF_NOCONFIRMATION | FOF_SILENT, never FOF_ALLOWUNDO.
//! - Rename: FOF_ALLOWUNDO | FOF_SILENT. NewFolder: FOF_SILENT | FOF_RENAMEONCOLLISION.
//! - test_no_ui (a debug build with FILES_TEST_NO_UI=1) adds FOF_NOERRORUI and nothing else.

use oled_trust::frame::files::{Conflicts, OpKind};
use windows::Win32::UI::Shell::{
    FOF_ALLOWUNDO, FOF_NOCONFIRMATION, FOF_NOCONFIRMMKDIR, FOF_NOERRORUI, FOF_RENAMEONCOLLISION, FOF_SILENT,
    FOF_WANTNUKEWARNING,
};

/// The FILEOPERATION_FLAGS value of one chunk.
pub fn op_flags(kind: OpKind, c: Conflicts, collisions: u32, dialogs_under_silent: bool, test_no_ui: bool) -> u32 {
    let flags = match kind {
        OpKind::Copy | OpKind::Move => {
            let mut f = FOF_ALLOWUNDO.0 | FOF_NOCONFIRMMKDIR.0;
            let system_asks = c == Conflicts::AskSystem && collisions > 0 && !dialogs_under_silent;
            if !system_asks {
                f |= FOF_SILENT.0;
            }
            if c == Conflicts::ReplaceAll {
                f |= FOF_NOCONFIRMATION.0;
            }
            f
        }
        OpKind::Recycle => FOF_ALLOWUNDO.0 | FOF_WANTNUKEWARNING.0 | FOF_SILENT.0,
        OpKind::DeletePermanent => FOF_NOCONFIRMATION.0 | FOF_SILENT.0,
        OpKind::Rename => FOF_ALLOWUNDO.0 | FOF_SILENT.0,
        OpKind::NewFolder => FOF_SILENT.0 | FOF_RENAMEONCOLLISION.0,
        // Never run as such: an undo or a resume runs as the concrete kinds of its plan.
        OpKind::Undo | OpKind::Resume => FOF_ALLOWUNDO.0 | FOF_SILENT.0,
    };
    if test_no_ui {
        flags | FOF_NOERRORUI.0
    } else {
        flags
    }
}

/// FILES_TEST_NO_UI=1 counts in a debug build only. Pure.
pub fn test_no_ui_from(debug: bool, var: Option<&str>) -> bool {
    debug && var == Some("1")
}

/// FILES_TEST_DIALOGS_UNDER_SILENT=1 overrides the FS-3 a constant in a debug build only. Pure.
pub fn dialogs_under_silent_from(debug: bool, constant: bool, var: Option<&str>) -> bool {
    constant || (debug && var == Some("1"))
}

#[cfg(test)]
mod tests {
    use super::*;

    const KINDS: [OpKind; 8] = [
        OpKind::Copy,
        OpKind::Move,
        OpKind::Recycle,
        OpKind::DeletePermanent,
        OpKind::Rename,
        OpKind::NewFolder,
        OpKind::Undo,
        OpKind::Resume,
    ];
    const CONFLICTS: [Conflicts; 3] = [Conflicts::AskSystem, Conflicts::ReplaceAll, Conflicts::SkipAll];

    /// The pinned value of every combination without test_no_ui (FOF_ALLOWUNDO 64, FOF_NOCONFIRMMKDIR 512,
    /// FOF_SILENT 4, FOF_NOCONFIRMATION 16, FOF_WANTNUKEWARNING 16384, FOF_RENAMEONCOLLISION 8).
    fn pinned(kind: OpKind, c: Conflicts, collisions: u32, dus: bool) -> u32 {
        match (kind, c, collisions, dus) {
            (OpKind::Copy | OpKind::Move, Conflicts::AskSystem, 1, false) => 576,
            (OpKind::Copy | OpKind::Move, Conflicts::ReplaceAll, _, _) => 596,
            (OpKind::Copy | OpKind::Move, _, _, _) => 580,
            (OpKind::Recycle, _, _, _) => 16_452,
            (OpKind::DeletePermanent, _, _, _) => 20,
            (OpKind::Rename | OpKind::Undo | OpKind::Resume, _, _, _) => 68,
            (OpKind::NewFolder, _, _, _) => 12,
        }
    }

    fn each(mut f: impl FnMut(OpKind, Conflicts, u32, bool, bool)) {
        for kind in KINDS {
            for c in CONFLICTS {
                for collisions in [0, 1] {
                    for dus in [false, true] {
                        for tnu in [false, true] {
                            f(kind, c, collisions, dus, tnu);
                        }
                    }
                }
            }
        }
    }

    #[test]
    fn flags_table() {
        let mut n = 0;
        each(|kind, c, collisions, dus, tnu| {
            let want = pinned(kind, c, collisions, dus) | if tnu { 1024 } else { 0 };
            assert_eq!(op_flags(kind, c, collisions, dus, tnu), want, "{kind:?} {c:?} {collisions} {dus} {tnu}");
            n += 1;
        });
        assert_eq!(n, 192);
        assert_eq!(op_flags(OpKind::Copy, Conflicts::AskSystem, 7, false, false), 576, "any collision count");
    }

    #[test]
    fn flags_invariants() {
        each(|kind, c, collisions, dus, tnu| {
            let f = op_flags(kind, c, collisions, dus, tnu);
            let has = |x: u32| f & x != 0;
            if kind == OpKind::Recycle {
                assert!(!has(FOF_NOCONFIRMATION.0), "Recycle never skips confirmation");
                assert!(has(FOF_WANTNUKEWARNING.0) && has(FOF_ALLOWUNDO.0));
            }
            if kind == OpKind::DeletePermanent {
                assert!(!has(FOF_ALLOWUNDO.0), "DeletePermanent never asks for undo");
            }
            let may_skip = kind == OpKind::DeletePermanent
                || (matches!(kind, OpKind::Copy | OpKind::Move) && c == Conflicts::ReplaceAll);
            assert_eq!(has(FOF_NOCONFIRMATION.0), may_skip, "{kind:?} {c:?}");
            let loud =
                matches!(kind, OpKind::Copy | OpKind::Move) && c == Conflicts::AskSystem && collisions > 0 && !dus;
            assert_eq!(!has(FOF_SILENT.0), loud, "FOF_SILENT is dropped only for Decide-each with collisions");
            assert_eq!(
                f & !FOF_NOERRORUI.0,
                op_flags(kind, c, collisions, dus, false),
                "test_no_ui adds only NOERRORUI"
            );
            assert_eq!(has(FOF_NOERRORUI.0), tnu);
        });
    }

    #[test]
    fn test_switches_are_debug_only() {
        assert!(test_no_ui_from(true, Some("1")));
        assert!(!test_no_ui_from(false, Some("1")), "release ignores FILES_TEST_NO_UI");
        assert!(!test_no_ui_from(true, Some("0")) && !test_no_ui_from(true, None));
        assert!(dialogs_under_silent_from(true, false, Some("1")));
        assert!(!dialogs_under_silent_from(false, false, Some("1")), "release ignores the override");
        assert!(dialogs_under_silent_from(false, true, None), "the constant counts everywhere");
        assert!(!dialogs_under_silent_from(true, false, None));
    }
}
