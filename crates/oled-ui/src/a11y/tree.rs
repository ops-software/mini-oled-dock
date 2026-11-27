//! The accessibility tree of a window (K2b.9, SPEC-platform 5.3.7): the frame of the last layout becomes one tree under
//! a root that stands for the window, each node with its role, name, description, value, states, actions and bounds. A
//! row carries its position among all the model's rows and its table the full row count ("row 512 of 100,000"), although
//! only the rows near the viewport are nodes. Each node has a hash, so the update after a layout carries only the nodes
//! that changed and an unchanged tree pushes nothing. The roles stay the kit's (SPEC-platform 5.3.7 names them the same);
//! the bridge maps them to AccessKit's. Pure and always compiled.

use std::collections::HashMap;
use std::hash::{DefaultHasher, Hash, Hasher};

use crate::geom::RectP;
use crate::node::{Actions, Focusable, Frame, Label, Node, NodeFlags, NodeId, NodeValue, Role, Text};

/// The root's id: the window, above every node a screen pushes (a pushed node with this id is left out).
pub(crate) const ROOT: NodeId = NodeId(u64::MAX);

/// One node of the tree.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct A11yNode {
    pub(crate) id: NodeId,
    pub(crate) role: Role,
    pub(crate) name: Option<Text>,
    pub(crate) description: Option<Text>,
    pub(crate) value: Option<NodeValue>,
    pub(crate) flags: NodeFlags,
    pub(crate) actions: Actions,
    /// A Tab stop, a roving stop or FOCUSABLE.
    pub(crate) focusable: bool,
    /// Physical px, client coordinates.
    pub(crate) bounds: RectP,
    pub(crate) children: Vec<NodeId>,
    /// An item's 1-based position in its set; a row's is its row in the whole model, not among the nodes.
    pub(crate) position: Option<u32>,
    /// A container's item count; a table's is the model's full row count.
    pub(crate) set_size: Option<u32>,
    /// Over every field above: an equal hash, nothing to push.
    pub(crate) hash: u64,
}

/// A window's tree: the root first, then the nodes in frame order.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct A11yTree {
    pub(crate) nodes: Vec<A11yNode>,
    /// The focused node, when it is in the tree.
    pub(crate) focus: Option<NodeId>,
}

#[cfg(test)]
impl A11yTree {
    pub(crate) fn get(&self, id: NodeId) -> Option<&A11yNode> {
        self.nodes.iter().find(|n| n.id == id)
    }
}

/// The roles that hold a set of items: their Position value gives the set's size (and the focused item), not a place.
fn is_container(r: Role) -> bool {
    matches!(r, Role::Table | Role::List | Role::Tree | Role::TabList | Role::Menu | Role::RadioGroup)
}

/// The root's place in the parent list.
const AT_ROOT: usize = usize::MAX;

/// The tree of `frame`. A11Y_HIDDEN nodes are left out and their children hang from the nearest kept ancestor; a node
/// pushed twice keeps its first push; a missing parent, or a parent chain that loops, hangs the node from the root.
/// `client` is the root's bounds, `focus` the dispatcher's focused node.
pub(crate) fn build(frame: &Frame, client: RectP, focus: Option<NodeId>) -> A11yTree {
    let mut at: HashMap<NodeId, usize> = HashMap::with_capacity(frame.len());
    let mut hidden: HashMap<NodeId, Option<NodeId>> = HashMap::new();
    let mut kept: Vec<&Node> = Vec::with_capacity(frame.len());
    for n in frame.iter() {
        if n.flags.contains(NodeFlags::A11Y_HIDDEN) {
            hidden.entry(n.id).or_insert(n.parent);
        } else if n.id != ROOT && !at.contains_key(&n.id) {
            at.insert(n.id, kept.len());
            kept.push(n);
        }
    }
    // Each node's parent: the nearest kept ancestor, looking through hidden ones (a loop of hidden ones ends the walk).
    let mut parent: Vec<usize> = kept
        .iter()
        .enumerate()
        .map(|(i, n)| {
            let mut up = n.parent;
            for _ in 0..=hidden.len() {
                let Some(id) = up else { break };
                if let Some(&j) = at.get(&id) {
                    return if j == i { AT_ROOT } else { j };
                }
                match hidden.get(&id) {
                    Some(p) => up = *p,
                    None => break,
                }
            }
            AT_ROOT
        })
        .collect();
    // A chain that never reaches the root loops: it is cut at the node where the walk started.
    for i in 0..kept.len() {
        if !reaches_root(&parent, i) {
            parent[i] = AT_ROOT;
        }
    }
    let mut children: Vec<Vec<NodeId>> = vec![Vec::new(); kept.len()];
    let mut top = Vec::new();
    for (i, n) in kept.iter().enumerate() {
        match parent[i] {
            AT_ROOT => top.push(n.id),
            p => children[p].push(n.id),
        }
    }
    // Positions; set sizes from a container's own count, else from its items' counts.
    let mut position = vec![None; kept.len()];
    let mut set_size = vec![None; kept.len()];
    for (i, n) in kept.iter().enumerate() {
        match n.value {
            Some(NodeValue::Position { count, .. }) if is_container(n.role) => set_size[i] = Some(count),
            Some(NodeValue::Position { index, .. }) => position[i] = Some(index),
            _ => {}
        }
    }
    for (i, n) in kept.iter().enumerate() {
        if let (Some(NodeValue::Position { count, .. }), true) = (&n.value, position[i].is_some()) {
            if parent[i] != AT_ROOT && set_size[parent[i]].is_none() {
                set_size[parent[i]] = Some(*count);
            }
        }
    }
    let mut nodes = Vec::with_capacity(kept.len() + 1);
    nodes.push(
        A11yNode {
            id: ROOT,
            role: Role::Window,
            name: None,
            description: None,
            value: None,
            flags: NodeFlags::default(),
            actions: Actions::default(),
            focusable: false,
            bounds: client,
            children: top,
            position: None,
            set_size: None,
            hash: 0,
        }
        .hashed(),
    );
    for (i, n) in kept.iter().enumerate() {
        nodes.push(
            A11yNode {
                id: n.id,
                role: n.role,
                name: name(n),
                description: n.description.clone(),
                value: n.value.clone(),
                flags: n.flags,
                actions: n.actions,
                focusable: n.focus != Focusable::No || n.flags.contains(NodeFlags::FOCUSABLE),
                bounds: n.rect,
                children: std::mem::take(&mut children[i]),
                position: position[i],
                set_size: set_size[i],
                hash: 0,
            }
            .hashed(),
        );
    }
    A11yTree { nodes, focus: focus.filter(|f| at.contains_key(f)) }
}

/// True when the parent chain from `from` ends at the root.
fn reaches_root(parent: &[usize], from: usize) -> bool {
    let (mut j, mut steps) = (from, 0);
    while parent[j] != AT_ROOT {
        j = parent[j];
        steps += 1;
        if steps > parent.len() {
            return false;
        }
    }
    true
}

/// A node's name: its label, when it has one that is not blank.
fn name(n: &Node) -> Option<Text> {
    match &n.label {
        Label::Text(t) if !t.trim().is_empty() => Some(t.clone()),
        _ => None,
    }
}

impl A11yNode {
    /// The node with its hash computed.
    fn hashed(mut self) -> A11yNode {
        let mut h = DefaultHasher::new();
        self.id.hash(&mut h);
        self.role.hash(&mut h);
        self.name.hash(&mut h);
        self.description.hash(&mut h);
        match &self.value {
            None => 0u8.hash(&mut h),
            Some(NodeValue::Text(t)) => (1u8, t).hash(&mut h),
            Some(NodeValue::Range { value, min, max, step }) => {
                (2u8, value.to_bits(), min.to_bits(), max.to_bits(), step.to_bits()).hash(&mut h)
            }
            Some(NodeValue::Toggle(on)) => (3u8, on).hash(&mut h),
            Some(NodeValue::Position { index, count }) => (4u8, index, count).hash(&mut h),
            Some(NodeValue::Level(l)) => (5u8, l).hash(&mut h),
            Some(NodeValue::Percent(p)) => (6u8, p.to_bits()).hash(&mut h),
        }
        self.flags.hash(&mut h);
        self.actions.hash(&mut h);
        self.focusable.hash(&mut h);
        self.bounds.hash(&mut h);
        self.children.hash(&mut h);
        self.position.hash(&mut h);
        self.set_size.hash(&mut h);
        self.hash = h.finish();
        self
    }
}

/// What the client's copy of the tree holds: each node's hash and the focus of the last update.
#[derive(Debug, Default)]
pub(crate) struct Pushed {
    hashes: HashMap<NodeId, u64>,
    focus: Option<NodeId>,
    /// A whole tree was handed over; until then an update carries every node.
    whole: bool,
}

impl Pushed {
    /// True once the client holds a whole tree.
    pub(crate) fn has_tree(&self) -> bool {
        self.whole
    }

    /// The nodes an update must carry to bring the client's copy to `tree`: every node before the first update, then
    /// the new and the changed ones (a node whose children changed has a new hash, so an insertion or a removal carries
    /// the parent). None when nothing changed, the focus included: nothing is pushed.
    pub(crate) fn changes<'t>(&self, tree: &'t A11yTree) -> Option<Vec<&'t A11yNode>> {
        if !self.whole {
            return Some(tree.nodes.iter().collect());
        }
        let changed: Vec<&A11yNode> = tree.nodes.iter().filter(|n| self.hashes.get(&n.id) != Some(&n.hash)).collect();
        (!changed.is_empty() || tree.focus != self.focus).then_some(changed)
    }

    /// The client's copy is now `tree`.
    pub(crate) fn commit(&mut self, tree: &A11yTree) {
        self.hashes.clear();
        self.hashes.extend(tree.nodes.iter().map(|n| (n.id, n.hash)));
        self.focus = tree.focus;
        self.whole = true;
    }
}

#[cfg(test)]
mod tests {
    use std::borrow::Cow;
    use std::collections::HashSet;

    use super::*;
    use crate::command::CommandTable;
    use crate::cx::{EventCx, LayoutCx, PaintCx};
    use crate::event::{Event, Reply};
    use crate::geom::{SizeL, SizeP};
    use crate::harness::Harness;
    use crate::host::AppMsg;
    use crate::model::{
        Align, CellKind, CellOut, ColId, Column, ListModel, RowKey, ScrollTo, SelectionMode, SortDir, TableModel,
        TableStyle,
    };
    use crate::node::WidgetId;
    use crate::screen::{Screen, Widget};
    use crate::theme::Tokens;
    use crate::widgets::VirtualTable;

    /// A hundred thousand rows without storage, "item N".
    struct Rows;

    impl ListModel for Rows {
        fn len(&self) -> usize {
            100_000
        }
        fn key(&self, row: usize) -> RowKey {
            RowKey(row as u64)
        }
        fn row_of(&self, key: RowKey) -> Option<usize> {
            (key.0 < 100_000).then_some(key.0 as usize)
        }
        fn label(&self, row: usize) -> Cow<'_, str> {
            Cow::Owned(format!("item {row}"))
        }
        fn generation(&self) -> u64 {
            0
        }
    }

    impl TableModel for Rows {
        fn cell(&self, row: usize, _: ColId, out: &mut CellOut) {
            out.set_text(CellKind::Text, &format!("item {row}"));
        }
    }

    /// The table filling the window; a usize message focuses that row (0-based) and brings it into view.
    struct Sc {
        t: VirtualTable<Rows>,
        cmds: CommandTable,
    }

    impl Screen for Sc {
        fn layout(&mut self, cx: &mut LayoutCx, avail: SizeP) -> SizeP {
            self.t.layout(cx, RectP::wh(0, 0, avail.w, avail.h));
            avail
        }
        fn paint(&self, cx: &mut PaintCx) {
            self.t.paint(cx);
        }
        fn event(&mut self, cx: &mut EventCx, ev: &Event) -> Reply {
            self.t.event(cx, ev);
            Reply::None
        }
        fn data(&mut self, cx: &mut EventCx, msg: AppMsg) -> Reply {
            if let Ok(row) = msg.downcast::<usize>() {
                self.t.set_focused(cx, RowKey(*row as u64), ScrollTo::Center);
            }
            Reply::None
        }
        fn commands(&self) -> &CommandTable {
            &self.cmds
        }
    }

    const CLIENT: RectP = RectP { l: 0, t: 0, r: 400, b: 300 };

    fn table() -> Harness<Sc> {
        let name = Column {
            id: ColId(0),
            title: Cow::Borrowed("Name"),
            width: 200,
            min: 48,
            flex: true,
            align: Align::Start,
            sort: None,
            first_sort: SortDir::Asc,
            visible: true,
            hideable: false,
            tip: None,
        };
        let t = VirtualTable::new(WidgetId(1), Rows, vec![name], SelectionMode::Multi, TableStyle::FILES);
        Harness::new(Sc { t, cmds: CommandTable::new() }, SizeL { w: 400, h: 300 }, 96, Tokens::oled())
    }

    fn tree_of(h: &mut Harness<Sc>) -> A11yTree {
        let focus = h.focused();
        build(&h.dispatcher().frame, CLIENT, focus)
    }

    /// The tree is one: every id once, every node but the root in exactly one child list, every chain ends at the root.
    fn well_formed(t: &A11yTree) {
        assert_eq!(t.nodes[0].id, ROOT);
        let mut ids = HashSet::new();
        assert!(t.nodes.iter().all(|n| ids.insert(n.id)), "an id twice");
        let mut parent = HashMap::new();
        for n in &t.nodes {
            for c in &n.children {
                assert!(ids.contains(c), "{c:?} is a child but not a node");
                assert!(parent.insert(*c, n.id).is_none(), "{c:?} has two parents");
            }
        }
        for n in &t.nodes[1..] {
            let (mut at, mut steps) = (n.id, 0);
            while at != ROOT {
                at = *parent.get(&at).unwrap_or_else(|| panic!("{at:?} has no parent"));
                steps += 1;
                assert!(steps <= t.nodes.len(), "{:?} loops", n.id);
            }
        }
        assert!(t.focus.is_none_or(|f| ids.contains(&f)));
    }

    #[test]
    fn tree_maps_roles_and_positions() {
        let mut h = table();
        h.key("Tab");
        h.send(Box::new(511usize));
        let t = tree_of(&mut h);
        well_formed(&t);
        let root = &t.nodes[0];
        assert_eq!((root.role, root.bounds), (Role::Window, CLIENT));
        let table = t.nodes.iter().find(|n| n.role == Role::Table).expect("the table");
        assert_eq!(table.set_size, Some(100_000), "the table carries the full row count");
        assert!(root.children.contains(&table.id));
        let row = t.get(NodeId::new(WidgetId(1), 512)).expect("row 512 of 100,000 is a node");
        assert_eq!((row.role, row.position), (Role::Row, Some(512)), "its place among all the rows");
        assert_eq!(row.name.as_deref(), Some("item 511"));
        assert!(row.focusable && row.actions.0 & Actions::CLICK.0 != 0);
        assert!(table.children.contains(&row.id), "rows hang from their table");
        assert_eq!(t.focus, Some(row.id), "the focused row");
        assert!(t.nodes.iter().any(|n| n.role == Role::ColumnHeader && n.name.as_deref() == Some("Name")));
        assert!(t.nodes.iter().all(|n| n.role != Role::Separator), "the hidden resize handles are left out");
        let rows = t.nodes.iter().filter(|n| n.role == Role::Row).count();
        assert!(rows < 100, "only the rows near the viewport are nodes ({rows})");
    }

    #[test]
    fn unchanged_frame_pushes_nothing() {
        let mut h = table();
        let mut pushed = Pushed::default();
        let t0 = tree_of(&mut h);
        assert_eq!(pushed.changes(&t0).map(|v| v.len()), Some(t0.nodes.len()), "the first update is whole");
        pushed.commit(&t0);
        h.render();
        let t1 = tree_of(&mut h);
        assert_eq!(t1, t0, "a repaint changes no node");
        assert!(pushed.changes(&t1).is_none(), "nothing to push");
        // The table's focused row moves to row 3: only the table changes (its value names the focused row; a row's focus
        // stop, Tab or roving, is focusable to a client either way).
        h.send(Box::new(3usize));
        let t2 = tree_of(&mut h);
        well_formed(&t2);
        let changed: Vec<NodeId> = pushed.changes(&t2).expect("the table changed").iter().map(|n| n.id).collect();
        assert_eq!(changed, vec![NodeId::new(WidgetId(1), 0)]);
        pushed.commit(&t2);
        // A focus move alone carries the focus and no node.
        h.key("Tab");
        let t3 = tree_of(&mut h);
        assert!(t3.focus.is_some() && t3.focus != t2.focus, "{:?}", t3.focus);
        assert_eq!(pushed.changes(&t3).map(|v| v.len()), Some(0), "the focus, no node");
        pushed.commit(&t3);
        assert!(pushed.changes(&tree_of(&mut h)).is_none());
    }

    #[test]
    fn a_broken_frame_still_makes_one_tree() {
        let id = |s| NodeId::new(WidgetId(9), s);
        let r = RectP::wh(0, 0, 10, 10);
        let b = |s, l: &'static str| Node::new(id(s), Role::Button, r).label(Text::from(l));
        let mut f = Frame::new();
        f.push(Node::new(id(1), Role::Group, r));
        f.push(Node::new(id(2), Role::Separator, r).parent(id(1)).flags(NodeFlags::A11Y_HIDDEN));
        f.push(b(3, "under the hidden one").parent(id(2)));
        f.push(b(3, "pushed twice"));
        f.push(b(4, "loop").parent(id(5)));
        f.push(b(5, "loop").parent(id(4)));
        f.push(b(6, "lost parent").parent(id(77)));
        f.push(Node::new(ROOT, Role::Button, r));
        f.push(b(7, "own parent").parent(id(7)));
        f.push(b(8, " ").parent(id(1)));
        let t = build(&f, r, Some(id(2)));
        well_formed(&t);
        assert_eq!(t.nodes.len(), 8, "the root and seven nodes");
        assert_eq!(t.focus, None, "a hidden node is no focus for a client");
        assert!(t.get(id(2)).is_none());
        let group = t.get(id(1)).expect("the group");
        assert_eq!(group.children, vec![id(3), id(8)], "the hidden node's child hangs from its grandparent");
        assert_eq!(t.get(id(3)).and_then(|n| n.name.as_deref()), Some("under the hidden one"));
        assert_eq!(t.get(id(8)).map(|n| n.name.is_none()), Some(true), "a blank label is no name");
        assert_eq!(t.nodes[0].children, vec![id(1), id(4), id(6), id(7)]);
        assert_eq!(t.get(id(4)).map(|n| n.children.clone()), Some(vec![id(5)]), "the loop is cut once");
    }

    #[test]
    fn items_size_their_container() {
        let id = |s| NodeId::new(WidgetId(3), s);
        let r = RectP::wh(0, 0, 10, 10);
        let mut f = Frame::new();
        f.push(Node::new(id(0), Role::Group, r));
        for i in 1..=2 {
            let pos = NodeValue::Position { index: 40 + i, count: 60 };
            f.push(Node::new(id(i), Role::ListItem, r).parent(id(0)).label(Text::from("x")).value(pos));
        }
        let t = build(&f, r, None);
        assert_eq!(t.get(id(0)).and_then(|n| n.set_size), Some(60));
        assert_eq!(t.get(id(2)).and_then(|n| n.position), Some(42));
    }
}
