//! Input events and replies (F-K1, P2-kit 2.4).

use crate::geom::PointP;
use crate::node::{NodeId, WidgetId};

/// Modifier keys, as bits.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Default)]
pub struct Mods(pub u8);

impl Mods {
    pub const NONE: Mods = Mods(0);
    pub const CTRL: Mods = Mods(1);
    pub const SHIFT: Mods = Mods(2);
    pub const ALT: Mods = Mods(4);
    pub const WIN: Mods = Mods(8);

    pub const fn ctrl(self) -> bool {
        self.0 & Mods::CTRL.0 != 0
    }

    pub const fn shift(self) -> bool {
        self.0 & Mods::SHIFT.0 != 0
    }

    pub const fn alt(self) -> bool {
        self.0 & Mods::ALT.0 != 0
    }

    pub const fn win(self) -> bool {
        self.0 & Mods::WIN.0 != 0
    }

    /// True when every bit of `o` is set.
    pub const fn contains(self, o: Mods) -> bool {
        self.0 & o.0 == o.0
    }
}

impl std::ops::BitOr for Mods {
    type Output = Mods;
    fn bitor(self, o: Mods) -> Mods {
        Mods(self.0 | o.0)
    }
}

/// A virtual-key code.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Key(pub u16);

impl Key {
    pub const BACK: Key = Key(0x08);
    pub const TAB: Key = Key(0x09);
    pub const ENTER: Key = Key(0x0D);
    pub const ESC: Key = Key(0x1B);
    pub const SPACE: Key = Key(0x20);
    pub const PAGE_UP: Key = Key(0x21);
    pub const PAGE_DOWN: Key = Key(0x22);
    pub const END: Key = Key(0x23);
    pub const HOME: Key = Key(0x24);
    pub const LEFT: Key = Key(0x25);
    pub const UP: Key = Key(0x26);
    pub const RIGHT: Key = Key(0x27);
    pub const DOWN: Key = Key(0x28);
    pub const INSERT: Key = Key(0x2D);
    pub const DELETE: Key = Key(0x2E);
    pub const APPS: Key = Key(0x5D);
    pub const F1: Key = Key(0x70);
    pub const F2: Key = Key(0x71);
    pub const F4: Key = Key(0x73);
    pub const F5: Key = Key(0x74);
    pub const F6: Key = Key(0x75);
    pub const F10: Key = Key(0x79);
    pub const ADD: Key = Key(0x6B);
    pub const SUBTRACT: Key = Key(0x6D);
    pub const OEM_PLUS: Key = Key(0xBB);
    pub const OEM_COMMA: Key = Key(0xBC);
    pub const OEM_MINUS: Key = Key(0xBD);
    pub const OEM_PERIOD: Key = Key(0xBE);

    /// The key of a letter 'A'..='Z' (a lower-case letter gives the same key).
    pub const fn letter(c: char) -> Key {
        let u = c as u32;
        let u = if u >= 'a' as u32 && u <= 'z' as u32 { u - 32 } else { u };
        Key(u as u16)
    }

    /// The key of a top-row digit 0..=9.
    pub const fn digit(d: u8) -> Key {
        Key(0x30 + d as u16)
    }
}

/// A mouse button.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum MouseButton {
    Left,
    Right,
    Middle,
    X1,
    X2,
}

/// A wheel step.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum WheelDelta {
    /// Notches times SPI_GETWHEELSCROLLLINES.
    Lines(f32),
    /// Touchpad.
    Pixels(f32),
}

/// An input method event.
#[derive(Clone, Debug, PartialEq)]
pub enum ImeEvent {
    Start,
    Composition { text: String, caret: usize },
    Result(String),
    End,
}

/// Why a node got the focus.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FocusReason {
    Keyboard,
    Pointer,
    Programmatic,
    WindowActivated,
}

/// What assistive technology asked of a node.
#[derive(Clone, Debug, PartialEq)]
pub enum A11yAction {
    Click,
    Focus,
    Expand,
    Collapse,
    ScrollIntoView,
    Increment,
    Decrement,
    SetValue(String),
    ShowContextMenu,
}

/// A one-shot timer's id. Apps use ids below 0x8000_0000; kit widgets use TimerId::kit.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct TimerId(pub u32);

impl TimerId {
    /// A widget's own timer (caret blink, scrollbar hide, type-ahead): 0x8000_0000 | (owner & 0x7FFFFF) << 8 | slot.
    pub const fn kit(owner: WidgetId, slot: u8) -> TimerId {
        TimerId(0x8000_0000 | ((owner.0 & 0x7F_FFFF) << 8) | slot as u32)
    }

    /// True for an id made by kit().
    pub const fn is_kit(self) -> bool {
        self.0 & 0x8000_0000 != 0
    }
}

// Delivery rule: Event::Timer(id) and Event::Command(id) with a kit id reach the screen with cx.target() =
// NodeId::new(owner, 0) (a menu pick: the node that opened the menu), so a screen that forwards by target().owner()
// reaches the widget; kit ids never reach CommandTable::by_chord.

/// What a screen and its widgets receive.
#[derive(Clone, Debug, PartialEq)]
#[non_exhaustive]
pub enum Event {
    PointerMove {
        pos: PointP,
        mods: Mods,
    },
    PointerDown {
        pos: PointP,
        button: MouseButton,
        clicks: u8,
        mods: Mods,
    },
    PointerUp {
        pos: PointP,
        button: MouseButton,
        mods: Mods,
    },
    /// The up on the node that got the down.
    Click {
        pos: PointP,
        button: MouseButton,
        clicks: u8,
        mods: Mods,
    },
    /// Pressed, then moved DRAG_THRESHOLD.
    DragStart {
        origin: PointP,
        pos: PointP,
        button: MouseButton,
        mods: Mods,
    },
    DragMove {
        pos: PointP,
        mods: Mods,
    },
    DragEnd {
        pos: PointP,
        mods: Mods,
        cancelled: bool,
    },
    PointerLeave,
    Wheel {
        pos: PointP,
        delta: WheelDelta,
        horizontal: bool,
        mods: Mods,
    },
    Key {
        key: Key,
        down: bool,
        repeat: bool,
        mods: Mods,
    },
    /// WM_CHAR, surrogate pairs combined.
    Text(char),
    Ime(ImeEvent),
    FocusIn(FocusReason),
    FocusOut,
    /// A chord, a menu item, a toolbar button.
    Command(crate::command::CommandId),
    ListPicked {
        menu: crate::cx::MenuId,
        index: usize,
    },
    MenuClosed(crate::cx::MenuId),
    /// One-shot, armed by EventCx::set_timer.
    Timer(TimerId),
    A11y(NodeId, A11yAction),
    WindowActive(bool),
    /// The caption's X, Alt+F4, an unconsumed Ctrl+W.
    CloseRequested,
}

/// What a screen asks of the host after an event.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Reply {
    None,
    Damage(crate::geom::RectP),
    Redraw,
    Relayout,
    Close,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keys_and_ids() {
        assert_eq!(Key::letter('A'), Key(0x41));
        assert_eq!(Key::letter('z'), Key(0x5A));
        assert_eq!(Key::digit(9), Key(0x39));
        let m = Mods::CTRL | Mods::SHIFT;
        assert!(m.ctrl() && m.shift() && !m.alt() && !m.win() && m.contains(Mods::SHIFT));
        let t = TimerId::kit(WidgetId(5), 2);
        assert_eq!(t, TimerId(0x8000_0502));
        assert!(t.is_kit() && !TimerId(7).is_kit());
    }
}
