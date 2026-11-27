//! Menu, ContextMenu and the list popup (K2a.9, KC2). K0.1b writes the dispatcher's menu state and the stubs it calls
//! (P2-kit 2.13); the widgets come with K0.1c and their bodies with K2a.9.

use crate::cx::MenuId;
use crate::dispatch::Effect;
use crate::event::{Key, Mods};
use crate::geom::PointP;

/// The open menus of a window, innermost last, held by the Dispatcher.
#[derive(Default)]
pub(crate) struct MenuStack {
    pub(crate) open: Vec<MenuId>,
}

/// What a press does while menus are open.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[allow(dead_code)] // Swallow comes with the menus (K2a.9).
pub(crate) enum MenuPress {
    /// Delivered to the screen as usual.
    Pass,
    /// Taken by the menus (an outside click closes them): not delivered.
    Swallow,
}

/// True while a menu (not a list popup) is open.
pub(crate) fn is_open(stack: &MenuStack) -> bool {
    !stack.open.is_empty()
}

/// A press anywhere in the owner. Until KC2: Pass.
pub(crate) fn on_pointer_down(stack: &mut MenuStack, p: PointP, fx: &mut Vec<Effect>) -> MenuPress {
    let _ = (stack, p, fx);
    MenuPress::Pass
}

/// A key while a menu is open; true when the menus took it. Until KC2: Esc closes one level, other keys are ignored.
pub(crate) fn on_key(stack: &mut MenuStack, key: Key, mods: Mods, down: bool, fx: &mut Vec<Effect>) -> bool {
    let _ = mods;
    if down && key == Key::ESC && stack.open.pop().is_some() {
        if stack.open.is_empty() {
            fx.push(Effect::CloseMenus);
        }
        return true;
    }
    false
}

/// Every menu closes (deactivation). Until KC2: the stack is emptied.
pub(crate) fn close_all(stack: &mut MenuStack, fx: &mut Vec<Effect>) {
    if !stack.open.is_empty() {
        stack.open.clear();
        fx.push(Effect::CloseMenus);
    }
}

/// The submenu delay. Until KC2: nothing.
pub(crate) fn on_due(stack: &mut MenuStack, now_ms: u64, fx: &mut Vec<Effect>) {
    let _ = (stack, now_ms, fx);
}

/// The next submenu deadline. Until KC2: none.
pub(crate) fn next_due(stack: &MenuStack) -> Option<u64> {
    let _ = stack;
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn esc_closes_one_level() {
        let mut s = MenuStack { open: vec![MenuId(1), MenuId(2)] };
        let mut fx = Vec::new();
        assert!(on_key(&mut s, Key::ESC, Mods::NONE, true, &mut fx));
        assert_eq!((s.open.len(), fx.len()), (1, 0));
        assert!(!on_key(&mut s, Key::DOWN, Mods::NONE, true, &mut fx));
        assert!(on_key(&mut s, Key::ESC, Mods::NONE, true, &mut fx));
        assert_eq!(fx, vec![Effect::CloseMenus]);
        assert!(!is_open(&s) && !on_key(&mut s, Key::ESC, Mods::NONE, true, &mut fx));
        assert_eq!(on_pointer_down(&mut s, PointP::default(), &mut fx), MenuPress::Pass);
    }
}
