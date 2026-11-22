//! The eight verbs (5.2.8), their chords and the table of which verbs a row offers.

use super::gesture;
use super::model::{FileClass, GlyphId, KeyChord, Kind, Mods, Tag, Verb, VerbSet, VerbSpec};
use super::strings;

const VK_RETURN: u16 = 0x0D;
const VK_DELETE: u16 = 0x2E;
const VK_C: u16 = 0x43;
const VK_W: u16 = 0x57;

const fn chord(vk: u16, mods: Mods) -> Option<KeyChord> {
    Some(KeyChord { vk, mods })
}

const CTRL_SHIFT: Mods = Mods(Mods::CTRL.0 | Mods::SHIFT.0);

/// In the order of `Verb`: `VERBS[verb as usize]` is that verb's spec. Ctrl+Shift+E also reveals; the key handler of the session
/// maps it, since a spec carries one chord.
pub static VERBS: [VerbSpec; 8] = [
    VerbSpec {
        verb: Verb::Open,
        chord: chord(VK_RETURN, Mods::NONE),
        label: strings::verb_open,
        glyph: GlyphId::Magnifier,
        destructive: false,
        tag: Tag::Required,
    },
    VerbSpec {
        verb: Verb::Switch,
        chord: chord(VK_RETURN, Mods::NONE),
        label: strings::verb_switch,
        glyph: GlyphId::Window,
        destructive: false,
        tag: Tag::Required,
    },
    VerbSpec {
        verb: Verb::RunAsAdmin,
        chord: chord(VK_RETURN, CTRL_SHIFT),
        label: strings::verb_run_as_admin,
        glyph: GlyphId::Executable,
        destructive: false,
        tag: Tag::OwnerAsked,
    },
    VerbSpec {
        verb: Verb::Reveal,
        chord: chord(VK_RETURN, Mods::CTRL),
        label: strings::verb_reveal,
        glyph: GlyphId::Folder,
        destructive: false,
        tag: Tag::Parity,
    },
    VerbSpec {
        verb: Verb::Copy,
        chord: chord(VK_C, Mods::CTRL),
        label: strings::verb_copy,
        glyph: GlyphId::Copy,
        destructive: false,
        tag: Tag::Parity,
    },
    VerbSpec {
        verb: Verb::CloseWindow,
        chord: chord(VK_W, Mods::CTRL),
        label: strings::verb_close_window,
        glyph: GlyphId::Window,
        destructive: true,
        tag: Tag::Parity,
    },
    VerbSpec {
        verb: Verb::Forget,
        chord: chord(VK_DELETE, Mods::SHIFT),
        label: strings::verb_forget,
        glyph: GlyphId::Info,
        destructive: true,
        tag: Tag::Parity,
    },
    VerbSpec {
        verb: Verb::RunInConsole,
        chord: chord(VK_RETURN, Mods::SHIFT),
        label: strings::verb_run_in_console,
        glyph: GlyphId::Prompt,
        destructive: false,
        tag: Tag::OwnerAsked,
    },
];

/// The spec of a verb.
pub fn spec(v: Verb) -> &'static VerbSpec {
    // The table is in the order of the enum (tested), so the index cannot miss; the fallback is never taken.
    VERBS.get(v as usize).unwrap_or(&VERBS[0])
}

/// The verbs a row of this kind offers, table 5.2.8 row by row. `running`: the app has a window. `class`: only for files.
/// `motw_ok`: live check 6 passed, so executable files may be opened. `admin_ok`: the gesture evidence works on this
/// machine (`gesture::GESTURE_TIME_MATCHES`); without it Run as administrator is offered on run lines and files only.
pub fn verbs_for(kind: Kind, running: bool, class: Option<FileClass>, motw_ok: bool, admin_ok: bool) -> VerbSet {
    let mut s = VerbSet::empty();
    match kind {
        Kind::App => {
            s = s.with(Verb::Open).with(Verb::Reveal).with(Verb::RunAsAdmin).with(Verb::Copy).with(Verb::Forget);
            if running {
                s = s.with(Verb::Switch).with(Verb::CloseWindow);
            }
        }
        Kind::Window => s = s.with(Verb::Switch).with(Verb::Reveal).with(Verb::Copy).with(Verb::CloseWindow),
        Kind::Setting | Kind::Command | Kind::Web => s = s.with(Verb::Open).with(Verb::Copy),
        Kind::Notice => s = s.with(Verb::Open),
        Kind::Answer => s = s.with(Verb::Copy),
        Kind::RunLine => {
            s = s.with(Verb::Open).with(Verb::RunInConsole).with(Verb::Copy);
            if admin_ok {
                s = s.with(Verb::RunAsAdmin);
            }
        }
        Kind::RunSuggestion => s = s.with(Verb::Open).with(Verb::RunInConsole).with(Verb::Copy).with(Verb::Forget),
        Kind::File => {
            s = s.with(Verb::Reveal).with(Verb::Copy);
            let exe = class == Some(FileClass::Executable);
            if !exe || motw_ok {
                s = s.with(Verb::Open);
            }
            if exe && motw_ok && admin_ok {
                s = s.with(Verb::RunAsAdmin);
            }
        }
        Kind::Folder => s = s.with(Verb::Open).with(Verb::Reveal).with(Verb::Copy),
    }
    s
}

/// The verbs of a file row at `path`: `verbs_for`'s, with Run as administrator only on a path the session can run
/// elevated (`gesture::elevated_file_line`: exe, msi, bat and cmd, 5.2.8), so the offer, the review and the action use
/// one filter.
pub fn verbs_for_file(path: &str, class: FileClass, motw_ok: bool, admin_ok: bool) -> VerbSet {
    let s = verbs_for(Kind::File, false, Some(class), motw_ok, admin_ok);
    if s.has(Verb::RunAsAdmin) && gesture::elevated_file_line(path).is_none() {
        s.without(Verb::RunAsAdmin)
    } else {
        s
    }
}

/// What Enter does on a row.
pub fn primary(kind: Kind, running: bool) -> Verb {
    match kind {
        Kind::App if running => Verb::Switch,
        Kind::Window => Verb::Switch,
        Kind::Answer => Verb::Copy,
        _ => Verb::Open,
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::indexing_slicing)]
mod tests {
    use super::*;
    use Verb::*;

    fn set(vs: &[Verb]) -> VerbSet {
        vs.iter().fold(VerbSet::empty(), |s, v| s.with(*v))
    }

    fn all_verbs() -> [Verb; 8] {
        [Open, Switch, RunAsAdmin, Reveal, Copy, CloseWindow, Forget, RunInConsole]
    }

    #[test]
    fn table_is_in_enum_order() {
        for v in all_verbs() {
            assert_eq!(VERBS[v as usize].verb, v);
            assert_eq!(spec(v).verb, v);
            assert!(!(spec(v).label)().is_empty());
        }
    }

    #[test]
    fn chords_match_5_2_8() {
        let c = |v: Verb| spec(v).chord.map(|c| (c.vk, c.mods.0));
        assert_eq!(c(Open), Some((0x0D, 0)));
        assert_eq!(c(Switch), Some((0x0D, 0)));
        assert_eq!(c(RunAsAdmin), Some((0x0D, 6)));
        assert_eq!(c(Reveal), Some((0x0D, 2)));
        assert_eq!(c(Copy), Some((0x43, 2)));
        assert_eq!(c(CloseWindow), Some((0x57, 2)));
        assert_eq!(c(Forget), Some((0x2E, 4)));
        assert_eq!(c(RunInConsole), Some((0x0D, 4)));
    }

    #[test]
    fn table_5_2_8() {
        let v = |k, running, class, motw, admin| verbs_for(k, running, class, motw, admin);
        // App, not running and running.
        assert_eq!(v(Kind::App, false, None, true, true), set(&[Open, Reveal, RunAsAdmin, Copy, Forget]));
        assert_eq!(
            v(Kind::App, true, None, true, true),
            set(&[Open, Switch, Reveal, RunAsAdmin, Copy, CloseWindow, Forget])
        );
        // Window.
        assert_eq!(v(Kind::Window, true, None, true, true), set(&[Switch, Reveal, Copy, CloseWindow]));
        // Setting, command, answer.
        assert_eq!(v(Kind::Setting, false, None, true, true), set(&[Open, Copy]));
        assert_eq!(v(Kind::Command, false, None, true, true), set(&[Open, Copy]));
        assert_eq!(v(Kind::Answer, false, None, true, true), set(&[Copy]));
        // Run line: Run as administrator only with the gesture evidence.
        assert_eq!(v(Kind::RunLine, false, None, true, true), set(&[Open, RunInConsole, Copy, RunAsAdmin]));
        assert_eq!(v(Kind::RunLine, false, None, true, false), set(&[Open, RunInConsole, Copy]));
        // Run suggestion: Ctrl+Shift+Enter only copies its text into the field, so it is not a verb.
        assert_eq!(v(Kind::RunSuggestion, false, None, true, true), set(&[Open, RunInConsole, Copy, Forget]));
        // File: executable with and without the Mark-of-the-Web result.
        let exe = Some(FileClass::Executable);
        assert_eq!(v(Kind::File, false, exe, true, true), set(&[Open, Reveal, Copy, RunAsAdmin]));
        assert_eq!(v(Kind::File, false, exe, true, false), set(&[Open, Reveal, Copy]));
        assert_eq!(v(Kind::File, false, exe, false, true), set(&[Reveal, Copy]));
        // A file row at a path: Run as administrator only on exe, msi, bat and cmd with no quote in the path.
        let at = |p: &str| verbs_for_file(p, FileClass::Executable, true, true);
        for p in ["c:\\x\\a.exe", "c:\\x\\a.MSI", "c:\\x\\a.bat", "c:\\x\\a.cmd", "\\\\srv\\s\\a.exe"] {
            assert_eq!(at(p), set(&[Open, Reveal, Copy, RunAsAdmin]), "{p}");
        }
        for p in ["c:\\x\\a.ps1", "c:\\x\\a.vbs", "c:\\x\\a.js", "c:\\x\\a.scr", "c:\\x\\a.lnk", "c:\\x\\a.url"] {
            assert_eq!(at(p), set(&[Open, Reveal, Copy]), "{p}");
        }
        for p in ["c:\\x\\a.com", "c:\\x\\a.appref-ms", "c:\\x\\a\".exe", "c:\\x\\exe"] {
            assert_eq!(at(p), set(&[Open, Reveal, Copy]), "{p}");
        }
        assert_eq!(verbs_for_file("c:\\x\\a.exe", FileClass::Executable, false, true), set(&[Reveal, Copy]));
        assert_eq!(verbs_for_file("c:\\x\\a.exe", FileClass::Executable, true, false), set(&[Open, Reveal, Copy]));
        // File document, folder, web, notice.
        assert_eq!(v(Kind::File, false, Some(FileClass::Document), false, true), set(&[Open, Reveal, Copy]));
        assert_eq!(v(Kind::Folder, false, None, false, true), set(&[Open, Reveal, Copy]));
        assert_eq!(v(Kind::Web, false, None, true, true), set(&[Open, Copy]));
        assert_eq!(v(Kind::Notice, false, None, true, true), set(&[Open]));
    }

    #[test]
    fn primary_verbs() {
        assert_eq!(primary(Kind::App, false), Open);
        assert_eq!(primary(Kind::App, true), Switch);
        assert_eq!(primary(Kind::Window, true), Switch);
        assert_eq!(primary(Kind::Answer, false), Copy);
        assert_eq!(primary(Kind::File, false), Open);
        assert_eq!(primary(Kind::Notice, false), Open);
    }

    #[test]
    fn primary_is_offered_where_it_can_run() {
        let kinds = [
            Kind::App,
            Kind::Window,
            Kind::Setting,
            Kind::Command,
            Kind::Answer,
            Kind::RunLine,
            Kind::RunSuggestion,
            Kind::Folder,
            Kind::Web,
            Kind::Notice,
        ];
        for k in kinds {
            for running in [false, true] {
                let p = primary(k, running);
                assert!(verbs_for(k, running, None, true, true).has(p), "{k:?} running={running}");
            }
        }
    }
}
