//! The accessibility dump of the Harness (P2-kit 2.5, K1a.5): one line per node not A11Y_HIDDEN, indent 2 per parent
//! level, `Role "name" [states] value`.

use crate::node::{Frame, Label, Node, NodeFlags, NodeValue};

/// The dump of a frame.
pub(crate) fn dump(frame: &Frame) -> String {
    let mut out = String::new();
    for n in frame.iter().filter(|n| !n.flags.contains(NodeFlags::A11Y_HIDDEN)) {
        let mut depth = 0;
        let mut up = n.parent;
        while let Some(p) = up.and_then(|id| frame.get(id)) {
            depth += 1;
            up = p.parent;
            if depth > 64 {
                break;
            }
        }
        out.push_str(&line(n, depth));
        out.push('\n');
    }
    out
}

fn line(n: &Node, depth: usize) -> String {
    let mut s = format!("{:indent$}{:?}", "", n.role, indent = depth * 2);
    if let Label::Text(t) = &n.label {
        s.push_str(&format!(" {t:?}"));
    }
    let states: Vec<&str> = [
        (n.focus != crate::node::Focusable::No || n.flags.contains(NodeFlags::FOCUSABLE), "focusable"),
        (n.flags.contains(NodeFlags::DISABLED), "disabled"),
        (n.flags.contains(NodeFlags::SELECTED), "selected"),
        (n.flags.contains(NodeFlags::CHECKED), "checked"),
        (n.flags.contains(NodeFlags::MIXED), "mixed"),
        (n.flags.contains(NodeFlags::EXPANDABLE), "expandable"),
        (n.flags.contains(NodeFlags::EXPANDED), "expanded"),
        (n.flags.contains(NodeFlags::BUSY), "busy"),
        (n.flags.contains(NodeFlags::INVALID), "invalid"),
    ]
    .into_iter()
    .filter_map(|(on, name)| on.then_some(name))
    .collect();
    if !states.is_empty() {
        // Built by hand, not with [&str]::join: a join instance in oled-ui changes which copy of the shared std
        // join code fat LTO keeps in dock.exe (512 B more), although the dock never calls the kit.
        let mut list = String::new();
        for (i, st) in states.iter().enumerate() {
            if i > 0 {
                list.push_str(", ");
            }
            list.push_str(st);
        }
        s.push_str(&format!(" [{list}]"));
    }
    match &n.value {
        None => {}
        Some(NodeValue::Text(t)) => s.push_str(&format!(" {t:?}")),
        Some(NodeValue::Range { value, min, max, .. }) => s.push_str(&format!(" {value} ({min}..{max})")),
        Some(NodeValue::Toggle(on)) => s.push_str(if *on { " on" } else { " off" }),
        Some(NodeValue::Position { index, count }) => s.push_str(&format!(" {index} of {count}")),
        Some(NodeValue::Level(l)) => s.push_str(&format!(" level {l}")),
        Some(NodeValue::Percent(p)) => s.push_str(&format!(" {p}%")),
    }
    s
}
