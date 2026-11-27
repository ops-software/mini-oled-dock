//! Commands and chords (F-K1, P2-kit 2.4): what a screen can do, with its keys, for menus, toolbars and the keyboard.

use crate::event::{Key, Mods};
use crate::glyph::Glyph;
use crate::node::{Text, WidgetId};
use crate::strings::{k, UiLang};

/// A command's id. Ids at or above KIT_BASE belong to kit widgets (a header menu, a tab menu).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct CommandId(pub u32);

impl CommandId {
    /// The first kit id; app ids stay below it.
    pub const KIT_BASE: u32 = 0x8000_0000;

    /// A kit widget's own command: KIT_BASE | (owner & 0x7FFF) << 16 | item.
    pub const fn kit(owner: WidgetId, item: u16) -> CommandId {
        CommandId(CommandId::KIT_BASE | ((owner.0 & 0x7FFF) << 16) | item as u32)
    }

    /// True for an id at or above KIT_BASE (CommandTable::add refuses it).
    pub const fn is_kit(self) -> bool {
        self.0 >= CommandId::KIT_BASE
    }
}

/// A key with its modifiers.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Chord {
    pub mods: Mods,
    pub key: Key,
}

impl Chord {
    pub const fn new(mods: Mods, key: Key) -> Chord {
        Chord { mods, key }
    }

    /// "Ctrl+Shift+Tab", "F2", "Shift+Del", "Ctrl+,", "Alt+D", "Ctrl+1": modifiers (Ctrl, Shift, Alt, Win) and one key,
    /// joined by '+', any case. None for anything else.
    pub fn parse(s: &str) -> Option<Chord> {
        let s = s.trim();
        let (head, key) = if s == "+" {
            ("", "+")
        } else if let Some(h) = s.strip_suffix("++") {
            (h, "+")
        } else {
            match s.rsplit_once('+') {
                Some((h, k)) => (h, k),
                None => ("", s),
            }
        };
        let mut mods = Mods::NONE;
        if !head.is_empty() {
            for m in head.split('+') {
                mods = mods
                    | match m.trim().to_ascii_lowercase().as_str() {
                        "ctrl" | "control" => Mods::CTRL,
                        "shift" => Mods::SHIFT,
                        "alt" => Mods::ALT,
                        "win" => Mods::WIN,
                        _ => return None,
                    };
            }
        }
        Some(Chord::new(mods, key_of(key.trim())?))
    }

    /// The chord as shown to the user ("Ctrl+Shift+Tab"): the modifier and key names come from the kit's kit.key.* strings
    /// of `lang` ("Ctrl+Maiusc+Tab" in Italian); letters, digits, F keys and the arrows stay as written.
    pub fn label(&self, lang: UiLang) -> String {
        let mut out = String::new();
        for (m, name) in [
            (Mods::CTRL, k::KIT_KEY_CTRL),
            (Mods::SHIFT, k::KIT_KEY_SHIFT),
            (Mods::ALT, k::KIT_KEY_ALT),
            (Mods::WIN, k::KIT_KEY_WIN),
        ] {
            if self.mods.contains(m) {
                out.push_str(&lang.kit(name));
                out.push('+');
            }
        }
        out.push_str(&key_label(self.key, lang));
        out
    }
}

/// A key's name in `lang`: the kit's string where the key has one, else its plain name.
fn key_label(key: Key, lang: UiLang) -> String {
    let s = match key {
        Key::TAB => k::KIT_KEY_TAB,
        Key::ENTER => k::KIT_KEY_ENTER,
        Key::ESC => k::KIT_KEY_ESC,
        Key::SPACE => k::KIT_KEY_SPACE,
        Key::BACK => k::KIT_KEY_BACKSPACE,
        Key::DELETE => k::KIT_KEY_DEL,
        Key::INSERT => k::KIT_KEY_INS,
        Key::HOME => k::KIT_KEY_HOME,
        Key::END => k::KIT_KEY_END,
        Key::PAGE_UP => k::KIT_KEY_PGUP,
        Key::PAGE_DOWN => k::KIT_KEY_PGDN,
        other => return key_name(other),
    };
    lang.kit(s).into_owned()
}

/// The key of a name as Chord::parse reads it.
fn key_of(name: &str) -> Option<Key> {
    let mut chars = name.chars();
    if let (Some(c), None) = (chars.next(), chars.next()) {
        return match c {
            'a'..='z' | 'A'..='Z' => Some(Key::letter(c)),
            '0'..='9' => Some(Key::digit(c as u8 - b'0')),
            ',' => Some(Key::OEM_COMMA),
            '.' => Some(Key::OEM_PERIOD),
            '-' => Some(Key::OEM_MINUS),
            '=' | '+' => Some(Key::OEM_PLUS),
            '←' => Some(Key::LEFT),
            '↑' => Some(Key::UP),
            '→' => Some(Key::RIGHT),
            '↓' => Some(Key::DOWN),
            _ => None,
        };
    }
    let lower = name.to_ascii_lowercase();
    if let Some(n) = lower.strip_prefix('f').and_then(|d| d.parse::<u16>().ok()) {
        return if (1..=24).contains(&n) { Some(Key(0x70 + n - 1)) } else { None };
    }
    Some(match lower.as_str() {
        "tab" => Key::TAB,
        "enter" | "return" => Key::ENTER,
        "esc" | "escape" => Key::ESC,
        "space" => Key::SPACE,
        "back" | "backspace" => Key::BACK,
        "del" | "delete" => Key::DELETE,
        "ins" | "insert" => Key::INSERT,
        "home" => Key::HOME,
        "end" => Key::END,
        "pgup" | "pageup" => Key::PAGE_UP,
        "pgdn" | "pagedown" => Key::PAGE_DOWN,
        "left" => Key::LEFT,
        "right" => Key::RIGHT,
        "up" => Key::UP,
        "down" => Key::DOWN,
        "apps" | "menu" => Key::APPS,
        "plus" => Key::OEM_PLUS,
        "minus" => Key::OEM_MINUS,
        "add" => Key::ADD,
        "subtract" => Key::SUBTRACT,
        _ => return None,
    })
}

/// A key's English name, the inverse of key_of.
fn key_name(k: Key) -> String {
    let named = match k {
        Key::TAB => "Tab",
        Key::ENTER => "Enter",
        Key::ESC => "Esc",
        Key::SPACE => "Space",
        Key::BACK => "Backspace",
        Key::DELETE => "Del",
        Key::INSERT => "Ins",
        Key::HOME => "Home",
        Key::END => "End",
        Key::PAGE_UP => "PgUp",
        Key::PAGE_DOWN => "PgDn",
        Key::LEFT => "←",
        Key::UP => "↑",
        Key::RIGHT => "→",
        Key::DOWN => "↓",
        Key::APPS => "Apps",
        Key::OEM_COMMA => ",",
        Key::OEM_PERIOD => ".",
        Key::OEM_MINUS => "-",
        Key::OEM_PLUS => "+",
        Key::ADD => "Add",
        Key::SUBTRACT => "Subtract",
        Key(c @ 0x30..=0x39) | Key(c @ 0x41..=0x5A) => return char::from(c as u8).to_string(),
        Key(f @ 0x70..=0x87) => return format!("F{}", f - 0x70 + 1),
        Key(other) => return format!("0x{other:02X}"),
    };
    named.to_string()
}

/// One command: its label, glyph, chords and state.
#[derive(Clone, Debug, PartialEq)]
pub struct Command {
    pub id: CommandId,
    pub label: Text,
    pub glyph: Option<Glyph>,
    pub chord: Option<Chord>,
    pub alt_chord: Option<Chord>,
    pub enabled: bool,
    pub checked: Option<bool>,
    pub description: Option<Text>,
}

impl Command {
    /// An enabled command with no glyph, chord, check state or description.
    pub fn new(id: CommandId, label: Text) -> Command {
        Command {
            id,
            label,
            glyph: None,
            chord: None,
            alt_chord: None,
            enabled: true,
            checked: None,
            description: None,
        }
    }

    pub fn chord(self, c: Chord) -> Command {
        Command { chord: Some(c), ..self }
    }

    pub fn alt_chord(self, c: Chord) -> Command {
        Command { alt_chord: Some(c), ..self }
    }

    pub fn glyph(self, g: Glyph) -> Command {
        Command { glyph: Some(g), ..self }
    }

    pub fn description(self, t: Text) -> Command {
        Command { description: Some(t), ..self }
    }
}

/// A screen's commands.
#[derive(Clone, Debug, Default)]
pub struct CommandTable {
    cmds: Vec<Command>,
}

impl CommandTable {
    pub fn new() -> CommandTable {
        CommandTable { cmds: Vec::new() }
    }

    /// Adds a command; Err on a duplicate id, a chord already bound or a kit id.
    pub fn add(&mut self, c: Command) -> Result<(), String> {
        if c.id.is_kit() {
            return Err(format!("command {:#x}: kit ids are not for apps", c.id.0));
        }
        if self.get(c.id).is_some() {
            return Err(format!("command {:#x} is already in the table", c.id.0));
        }
        for chord in [c.chord, c.alt_chord].into_iter().flatten() {
            if let Some(owner) = self.by_chord(chord) {
                return Err(format!("chord {} is already bound to command {:#x}", chord.label(UiLang::EN), owner.id.0));
            }
        }
        if c.chord.is_some() && c.chord == c.alt_chord {
            return Err(format!("command {:#x} names the same chord twice", c.id.0));
        }
        self.cmds.push(c);
        Ok(())
    }

    pub fn get(&self, id: CommandId) -> Option<&Command> {
        self.cmds.iter().find(|c| c.id == id)
    }

    pub fn get_mut(&mut self, id: CommandId) -> Option<&mut Command> {
        self.cmds.iter_mut().find(|c| c.id == id)
    }

    /// The command bound to a chord (as its chord or its alt chord); a disabled command still owns (swallows) it.
    pub fn by_chord(&self, c: Chord) -> Option<&Command> {
        self.cmds.iter().find(|x| x.chord == Some(c) || x.alt_chord == Some(c))
    }

    pub fn set_enabled(&mut self, id: CommandId, on: bool) {
        if let Some(c) = self.get_mut(id) {
            c.enabled = on;
        }
    }

    pub fn set_checked(&mut self, id: CommandId, v: Option<bool>) {
        if let Some(c) = self.get_mut(id) {
            c.checked = v;
        }
    }

    pub fn iter(&self) -> std::slice::Iter<'_, Command> {
        self.cmds.iter()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    // Through an alias: tests/workspace.rs reads the plain "Command" + "::new(" as a process spawn.
    use super::Command as Cmd;

    #[test]
    fn chords_parse() {
        assert_eq!(Chord::parse("Ctrl+Shift+Tab"), Some(Chord::new(Mods::CTRL | Mods::SHIFT, Key::TAB)));
        assert_eq!(Chord::parse("F2"), Some(Chord::new(Mods::NONE, Key::F2)));
        assert_eq!(Chord::parse("Shift+Del"), Some(Chord::new(Mods::SHIFT, Key::DELETE)));
        assert_eq!(Chord::parse("Ctrl+,"), Some(Chord::new(Mods::CTRL, Key::OEM_COMMA)));
        assert_eq!(Chord::parse("Alt+D"), Some(Chord::new(Mods::ALT, Key::letter('D'))));
        assert_eq!(Chord::parse("Ctrl+1"), Some(Chord::new(Mods::CTRL, Key::digit(1))));
        assert_eq!(Chord::parse("ctrl++"), Some(Chord::new(Mods::CTRL, Key::OEM_PLUS)));
        assert_eq!(Chord::parse("Ctrl+PgDn"), Some(Chord::new(Mods::CTRL, Key::PAGE_DOWN)));
        assert_eq!(Chord::parse("F24"), Some(Chord::new(Mods::NONE, Key(0x87))));
        for bad in ["", "Ctrl+", "Hyper+A", "F25", "Ctrl+Nope", "AB"] {
            assert_eq!(Chord::parse(bad), None, "{bad}");
        }
    }

    #[test]
    fn chord_label_in_italian() {
        let c = |s: &str| Chord::parse(s).unwrap();
        assert_eq!(c("Ctrl+Shift+Tab").label(UiLang::EN), "Ctrl+Shift+Tab");
        assert_eq!(c("Ctrl+Shift+Tab").label(UiLang::IT), "Ctrl+Maiusc+Tab");
        assert_eq!(c("Shift+Del").label(UiLang::IT), "Maiusc+Canc");
        assert_eq!(c("Alt+Left").label(UiLang::IT), "Alt+\u{2190}");
        assert_eq!(c("Ctrl+K").label(UiLang::IT), "Ctrl+K");
        assert_eq!(c("PgDn").label(UiLang::IT), "PgGi\u{f9}");
    }

    #[test]
    fn labels_read_back() {
        for s in ["Ctrl+Shift+Tab", "F2", "Shift+Del", "Ctrl+,", "Alt+D", "Ctrl+1", "Ctrl+PgUp", "Win+Space"] {
            let c = Chord::parse(s).unwrap();
            assert_eq!(c.label(UiLang::EN), s);
            assert_eq!(Chord::parse(&c.label(UiLang::EN)), Some(c));
        }
    }

    #[test]
    fn table_refuses_duplicates_and_kit_ids() {
        let mut t = CommandTable::new();
        let del = Chord::parse("Del").unwrap();
        assert!(t.add(Cmd::new(CommandId(1), Text::from("Delete")).chord(del)).is_ok());
        assert!(t.add(Cmd::new(CommandId(1), Text::from("Again"))).is_err());
        assert!(t.add(Cmd::new(CommandId(2), Text::from("Other")).alt_chord(del)).is_err());
        assert!(t.add(Cmd::new(CommandId::kit(WidgetId(3), 1), Text::from("Kit"))).is_err());
        t.set_enabled(CommandId(1), false);
        assert_eq!(t.by_chord(del).map(|c| (c.id, c.enabled)), Some((CommandId(1), false)));
        t.set_checked(CommandId(1), Some(true));
        assert_eq!(t.get(CommandId(1)).and_then(|c| c.checked), Some(true));
        assert_eq!(t.iter().count(), 1);
        assert!(CommandId::kit(WidgetId(3), 1).is_kit() && !CommandId(7).is_kit());
        assert_eq!(CommandId::kit(WidgetId(3), 1), CommandId(0x8003_0001));
    }
}
