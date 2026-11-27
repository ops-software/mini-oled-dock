//! Keyboard layer of the bar, pure parts, SPEC-dock 5.2.2: the key map, the zones, the focus movement and the
//! type-ahead. No window, no state: the UI thread (DK-KEYS) owns the focus and calls these.

use crate::render::ItemId;

/// Tab zones of the bar, in visual order.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum KZone {
    Start,
    Apps,
    Tray,
    Modules,
}

/// What a key means on the focused bar.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Key {
    Prev,
    Next,
    Home,
    End,
    NextZone,
    PrevZone,
    Activate,
    Middle,
    Context,
    Interior,
    MoveLeft,
    MoveRight,
    Escape,
    Letter(char),
}

/// Type-ahead buffer lifetime: a pause longer than this starts a new word (ms).
pub(crate) const TYPE_AHEAD_MS: u64 = 1000;

const VK_TAB: u16 = 0x09;
const VK_RETURN: u16 = 0x0D;
const VK_ESCAPE: u16 = 0x1B;
const VK_SPACE: u16 = 0x20;
const VK_END: u16 = 0x23;
const VK_HOME: u16 = 0x24;
const VK_LEFT: u16 = 0x25;
const VK_UP: u16 = 0x26;
const VK_RIGHT: u16 = 0x27;
const VK_DOWN: u16 = 0x28;
const VK_APPS: u16 = 0x5D;
const VK_F10: u16 = 0x79;

/// `vk` with its modifiers to a [`Key`] for a bar at the bottom (`top` = false) or at the top; None = not ours (the
/// default handler runs). Letters come from WM_CHAR, never from here.
pub(crate) fn map_key(vk: u16, shift: bool, ctrl: bool, alt: bool, top: bool) -> Option<Key> {
    if alt {
        return None;
    }
    if ctrl {
        return match (vk, shift) {
            (VK_LEFT, false) => Some(Key::MoveLeft),
            (VK_RIGHT, false) => Some(Key::MoveRight),
            _ => None,
        };
    }
    let interior = if top { VK_DOWN } else { VK_UP };
    match (vk, shift) {
        (VK_LEFT, false) => Some(Key::Prev),
        (VK_RIGHT, false) => Some(Key::Next),
        (VK_HOME, false) => Some(Key::Home),
        (VK_END, false) => Some(Key::End),
        (VK_TAB, false) => Some(Key::NextZone),
        (VK_TAB, true) => Some(Key::PrevZone),
        (VK_RETURN, false) | (VK_SPACE, false) => Some(Key::Activate),
        (VK_RETURN, true) => Some(Key::Middle),
        (VK_APPS, false) | (VK_F10, true) => Some(Key::Context),
        (VK_ESCAPE, false) => Some(Key::Escape),
        (v, false) if v == interior => Some(Key::Interior),
        _ => None,
    }
}

/// The Tab zone of an item: Start; App; Tray and TrayOverflow; every module.
pub(crate) fn zone_of(id: ItemId) -> KZone {
    match id {
        ItemId::Start => KZone::Start,
        ItemId::App(_) => KZone::Apps,
        ItemId::Tray(_) | ItemId::TrayOverflow => KZone::Tray,
        ItemId::Notifications
        | ItemId::Volume
        | ItemId::Clock
        | ItemId::ShowDesktop
        | ItemId::Power
        | ItemId::Network
        | ItemId::Metrics
        | ItemId::Battery => KZone::Modules,
    }
}

const ZONES: [KZone; 4] = [KZone::Start, KZone::Apps, KZone::Tray, KZone::Modules];

fn first_in(items: &[ItemId], z: KZone) -> Option<usize> {
    items.iter().position(|&i| zone_of(i) == z)
}

fn last_in(items: &[ItemId], z: KZone) -> Option<usize> {
    items.iter().rposition(|&i| zone_of(i) == z)
}

/// The index the focus moves to in `items` (layout order), from `cur`; None for a key that does not move it, or an
/// empty bar. Prev and Next wrap over the whole bar; Home and End go to the first and last item of the current zone;
/// NextZone and PrevZone go to the first item of the next or previous non-empty zone (wrapping). With no focus
/// (`cur` None or out of range): Next, Home and NextZone go to the first app (else item 0), Prev, End and PrevZone to
/// the last app (else the last item).
pub(crate) fn move_focus(items: &[ItemId], cur: Option<usize>, key: Key) -> Option<usize> {
    let n = items.len();
    if n == 0 {
        return None;
    }
    if !matches!(key, Key::Prev | Key::Next | Key::Home | Key::End | Key::NextZone | Key::PrevZone) {
        return None;
    }
    let Some(c) = cur.filter(|&c| c < n) else {
        return Some(match key {
            Key::Next | Key::Home | Key::NextZone => first_in(items, KZone::Apps).unwrap_or(0),
            _ => last_in(items, KZone::Apps).unwrap_or(n - 1),
        });
    };
    let z = zone_of(*items.get(c)?);
    match key {
        Key::Next => Some((c + 1) % n),
        Key::Prev => Some((c + n - 1) % n),
        Key::Home => first_in(items, z),
        Key::End => last_in(items, z),
        Key::NextZone | Key::PrevZone => {
            let zi = ZONES.iter().position(|&x| x == z)?;
            (1..ZONES.len()).find_map(|step| {
                let k = if key == Key::NextZone {
                    (zi + step) % ZONES.len()
                } else {
                    (zi + ZONES.len() - step) % ZONES.len()
                };
                first_in(items, *ZONES.get(k)?)
            })
        }
        _ => None,
    }
}

fn starts_with_ci(name: &str, prefix: &str) -> bool {
    let mut n = name.chars().flat_map(char::to_lowercase);
    prefix.chars().flat_map(char::to_lowercase).all(|p| n.next() == Some(p))
}

/// Type-ahead: `names` are (item index, app name) in layout order and `cur` an item index. Returns the item index of
/// the next app after `cur` (wrapping, `cur` itself last) whose name starts with `typed`, case-insensitive. A buffer
/// of one letter repeated ("a", "aa", ...) cycles through the apps starting with it; a longer word that the current
/// app already matches keeps the focus there.
pub(crate) fn type_ahead(names: &[(usize, &str)], cur: Option<usize>, typed: &str) -> Option<usize> {
    let first = typed.chars().next()?;
    let repeated = typed.chars().all(|c| c.to_lowercase().eq(first.to_lowercase()));
    let prefix: String = if repeated { first.to_string() } else { typed.to_string() };
    let matches = |name: &str| starts_with_ci(name, &prefix);
    if !repeated {
        if let Some(&(i, _)) = names.iter().find(|&&(i, nm)| Some(i) == cur && matches(nm)) {
            return Some(i);
        }
    }
    // Rotation order: entries after `cur`, then the ones up to and including it.
    let split = cur.map_or(0, |c| names.iter().position(|&(i, _)| i > c).unwrap_or(names.len()));
    let (head, tail) = names.split_at(split.min(names.len()));
    tail.iter().chain(head.iter()).find(|&&(_, nm)| matches(nm)).map(|&(i, _)| i)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn map_key_table() {
        for top in [false, true] {
            let k = |vk, s, c| map_key(vk, s, c, false, top);
            assert_eq!(k(VK_LEFT, false, false), Some(Key::Prev));
            assert_eq!(k(VK_RIGHT, false, false), Some(Key::Next));
            assert_eq!(k(VK_HOME, false, false), Some(Key::Home));
            assert_eq!(k(VK_END, false, false), Some(Key::End));
            assert_eq!(k(VK_TAB, false, false), Some(Key::NextZone));
            assert_eq!(k(VK_TAB, true, false), Some(Key::PrevZone));
            assert_eq!(k(VK_RETURN, false, false), Some(Key::Activate));
            assert_eq!(k(VK_SPACE, false, false), Some(Key::Activate));
            assert_eq!(k(VK_RETURN, true, false), Some(Key::Middle));
            assert_eq!(k(VK_APPS, false, false), Some(Key::Context));
            assert_eq!(k(VK_F10, true, false), Some(Key::Context));
            assert_eq!(k(VK_F10, false, false), None);
            assert_eq!(k(VK_LEFT, false, true), Some(Key::MoveLeft));
            assert_eq!(k(VK_RIGHT, false, true), Some(Key::MoveRight));
            assert_eq!(k(VK_ESCAPE, false, false), Some(Key::Escape));
            assert_eq!(k(VK_LEFT, true, false), None);
            assert_eq!(k(VK_TAB, false, true), None);
            assert_eq!(k(0x41, false, false), None, "letters come from WM_CHAR");
            // Alt turns everything off.
            assert_eq!(map_key(VK_LEFT, false, false, true, top), None);
            assert_eq!(map_key(VK_RETURN, false, false, true, top), None);
        }
        // The interior arrow points away from the screen edge.
        assert_eq!(map_key(VK_UP, false, false, false, false), Some(Key::Interior));
        assert_eq!(map_key(VK_DOWN, false, false, false, false), None);
        assert_eq!(map_key(VK_DOWN, false, false, false, true), Some(Key::Interior));
        assert_eq!(map_key(VK_UP, false, false, false, true), None);
    }

    fn bar() -> Vec<ItemId> {
        vec![
            ItemId::Start,
            ItemId::App(0),
            ItemId::App(1),
            ItemId::App(2),
            ItemId::Tray(7),
            ItemId::TrayOverflow,
            ItemId::Volume,
            ItemId::Clock,
        ]
    }

    #[test]
    fn zones_follow_the_item_kinds() {
        assert_eq!(zone_of(ItemId::Start), KZone::Start);
        assert_eq!(zone_of(ItemId::App(3)), KZone::Apps);
        assert_eq!(zone_of(ItemId::Tray(1)), KZone::Tray);
        assert_eq!(zone_of(ItemId::TrayOverflow), KZone::Tray);
        for m in [ItemId::Notifications, ItemId::Volume, ItemId::Clock, ItemId::ShowDesktop, ItemId::Power] {
            assert_eq!(zone_of(m), KZone::Modules);
        }
    }

    #[test]
    fn focus_moves_and_wraps_by_zone() {
        let b = bar();
        let mv = |c, k| move_focus(&b, Some(c), k);
        assert_eq!(mv(1, Key::Next), Some(2));
        assert_eq!(mv(7, Key::Next), Some(0), "Next wraps over the bar");
        assert_eq!(mv(0, Key::Prev), Some(7), "Prev wraps over the bar");
        assert_eq!(mv(2, Key::Home), Some(1));
        assert_eq!(mv(2, Key::End), Some(3));
        assert_eq!(mv(6, Key::Home), Some(6));
        assert_eq!(mv(6, Key::End), Some(7));
        assert_eq!(mv(2, Key::NextZone), Some(4));
        assert_eq!(mv(4, Key::NextZone), Some(6));
        assert_eq!(mv(7, Key::NextZone), Some(0), "the last zone wraps to the first");
        assert_eq!(mv(2, Key::PrevZone), Some(0));
        assert_eq!(mv(0, Key::PrevZone), Some(6), "the first zone wraps to the last");
        assert_eq!(mv(1, Key::Activate), None);
        assert_eq!(move_focus(&[], None, Key::Next), None);
        // An empty zone is skipped: no tray icons.
        let b2 = [ItemId::App(0), ItemId::Clock];
        assert_eq!(move_focus(&b2, Some(0), Key::NextZone), Some(1));
        assert_eq!(move_focus(&b2, Some(1), Key::NextZone), Some(0));
        // One zone only: Tab stays put.
        assert_eq!(move_focus(&[ItemId::App(0), ItemId::App(1)], Some(1), Key::NextZone), None);
    }

    #[test]
    fn first_key_without_focus() {
        let b = bar();
        for k in [Key::Next, Key::Home, Key::NextZone] {
            assert_eq!(move_focus(&b, None, k), Some(1), "{k:?}");
        }
        for k in [Key::Prev, Key::End, Key::PrevZone] {
            assert_eq!(move_focus(&b, None, k), Some(3), "{k:?}");
        }
        // No apps: the first and the last item.
        let nb = [ItemId::Start, ItemId::Clock];
        assert_eq!(move_focus(&nb, None, Key::Next), Some(0));
        assert_eq!(move_focus(&nb, None, Key::Prev), Some(1));
        // A stale index counts as no focus.
        assert_eq!(move_focus(&b, Some(99), Key::Next), Some(1));
    }

    #[test]
    fn type_ahead_prefix_and_cycle() {
        // Item 0 is Start; the apps are items 1..=5.
        let names =
            [(1, "Contoso Mail"), (2, "Example Chat"), (3, "Contoso Notes"), (4, "example Draw"), (5, "Fabrikam")];
        assert_eq!(type_ahead(&names, None, "c"), Some(1));
        assert_eq!(type_ahead(&names, Some(1), "c"), Some(3), "one letter again cycles");
        assert_eq!(type_ahead(&names, Some(3), "cc"), Some(1), "a repeated letter cycles and wraps");
        assert_eq!(type_ahead(&names, Some(2), "e"), Some(4), "case-insensitive");
        assert_eq!(type_ahead(&names, Some(4), "E"), Some(2));
        assert_eq!(type_ahead(&names, Some(1), "contoso n"), Some(3));
        assert_eq!(type_ahead(&names, Some(3), "contoso"), Some(3), "a longer word keeps a matching focus");
        assert_eq!(type_ahead(&names, None, "fab"), Some(5));
        assert_eq!(type_ahead(&names, Some(5), "f"), Some(5), "the only match is the current one");
        assert_eq!(type_ahead(&names, None, "z"), None);
        assert_eq!(type_ahead(&names, None, ""), None);
        assert_eq!(type_ahead(&[], None, "a"), None);
    }
}
