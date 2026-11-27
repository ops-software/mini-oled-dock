//! The node tree a screen pushes at layout (F-K1, P2-kit 2.4): what is hit-tested, focused and exposed to accessibility.

use crate::geom::{PointP, RectP};

/// A text already resolved for the current UiLang (the app's table, or the kit's through UiLang::kit).
pub type Text = std::borrow::Cow<'static, str>;

/// A widget's id, chosen by the app.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct WidgetId(pub u32);

/// A node's id: the owning widget in the high 32 bits, a slot in the low ones (virtual rows: slot = row + 1).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct NodeId(pub u64);

impl NodeId {
    /// (owner << 32) | slot.
    pub const fn new(owner: WidgetId, slot: u32) -> NodeId {
        NodeId(((owner.0 as u64) << 32) | slot as u64)
    }

    /// The owning widget.
    pub const fn owner(self) -> WidgetId {
        WidgetId((self.0 >> 32) as u32)
    }

    /// The slot within the owner.
    pub const fn slot(self) -> u32 {
        self.0 as u32
    }
}

/// A node's accessibility role.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum Role {
    Window,
    Group,
    Pane,
    Button,
    CheckBox,
    Switch,
    Slider,
    RadioGroup,
    Radio,
    TextInput,
    SearchInput,
    Label,
    Image,
    List,
    ListItem,
    Table,
    Row,
    Cell,
    ColumnHeader,
    Tree,
    TreeItem,
    TabList,
    Tab,
    TabPanel,
    Menu,
    MenuItem,
    MenuItemCheckBox,
    Separator,
    Toolbar,
    Tooltip,
    Alert,
    Dialog,
    ProgressIndicator,
    ScrollBar,
    Splitter,
    Status,
    Link,
    TitleBar,
}

/// A node's states, as bits.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Default)]
pub struct NodeFlags(pub u16);

impl NodeFlags {
    pub const FOCUSABLE: NodeFlags = NodeFlags(1);
    pub const DISABLED: NodeFlags = NodeFlags(2);
    pub const SELECTED: NodeFlags = NodeFlags(4);
    pub const CHECKED: NodeFlags = NodeFlags(8);
    pub const EXPANDED: NodeFlags = NodeFlags(16);
    pub const BUSY: NodeFlags = NodeFlags(32);
    pub const INVALID: NodeFlags = NodeFlags(64);
    pub const EXPANDABLE: NodeFlags = NodeFlags(128);
    /// The host animates hover in DUR_FAST and out DUR_NORMAL (buttons, toggles).
    pub const HOVER_FADE: NodeFlags = NodeFlags(256);
    pub const MIXED: NodeFlags = NodeFlags(512);
    pub const A11Y_HIDDEN: NodeFlags = NodeFlags(1024);

    /// True when every bit of `o` is set.
    pub const fn contains(self, o: NodeFlags) -> bool {
        self.0 & o.0 == o.0
    }
}

impl std::ops::BitOr for NodeFlags {
    type Output = NodeFlags;
    fn bitor(self, o: NodeFlags) -> NodeFlags {
        NodeFlags(self.0 | o.0)
    }
}

/// How a node takes the keyboard focus: not at all, as a Tab stop, or by arrows inside a group (one Tab stop).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum Focusable {
    #[default]
    No,
    Tab,
    Roving,
}

/// A node's accessible name (SPEC-platform 5.3.9's Key(S) folds into Text).
#[derive(Clone, Debug, PartialEq, Default)]
pub enum Label {
    #[default]
    None,
    Text(Text),
}

/// A node's value.
#[derive(Clone, Debug, PartialEq)]
pub enum NodeValue {
    Text(Text),
    Range {
        value: f64,
        min: f64,
        max: f64,
        step: f64,
    },
    Toggle(bool),
    /// 1-based index of count.
    Position {
        index: u32,
        count: u32,
    },
    Level(u16),
    Percent(f32),
}

/// What assistive technology may do with a node, as bits.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Default)]
pub struct Actions(pub u16);

impl Actions {
    pub const CLICK: Actions = Actions(1);
    pub const FOCUS: Actions = Actions(2);
    pub const EXPAND: Actions = Actions(4);
    pub const COLLAPSE: Actions = Actions(8);
    pub const SCROLL_INTO_VIEW: Actions = Actions(16);
    pub const INCREMENT: Actions = Actions(32);
    pub const DECREMENT: Actions = Actions(64);
    pub const SET_VALUE: Actions = Actions(128);
    pub const CONTEXT_MENU: Actions = Actions(256);
    pub const CLOSE: Actions = Actions(512);
}

impl std::ops::BitOr for Actions {
    type Output = Actions;
    fn bitor(self, o: Actions) -> Actions {
        Actions(self.0 | o.0)
    }
}

/// The pointer shape over a node.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum Cursor {
    #[default]
    Arrow,
    IBeam,
    SizeWE,
    SizeNS,
    Hand,
}

/// One node: a rectangle with a role, states, focus, name, description and value.
#[derive(Clone, Debug, PartialEq)]
pub struct Node {
    pub id: NodeId,
    pub parent: Option<NodeId>,
    pub owner: WidgetId,
    pub role: Role,
    pub rect: RectP,
    pub flags: NodeFlags,
    pub focus: Focusable,
    pub label: Label,
    /// Tooltip text and accessible description.
    pub description: Option<Text>,
    pub value: Option<NodeValue>,
    pub actions: Actions,
    pub cursor: Cursor,
}

impl Node {
    /// A node owned by id.owner(), everything else default.
    pub fn new(id: NodeId, role: Role, rect: RectP) -> Node {
        Node {
            id,
            parent: None,
            owner: id.owner(),
            role,
            rect,
            flags: NodeFlags::default(),
            focus: Focusable::No,
            label: Label::None,
            description: None,
            value: None,
            actions: Actions::default(),
            cursor: Cursor::Arrow,
        }
    }

    pub fn parent(self, p: NodeId) -> Node {
        Node { parent: Some(p), ..self }
    }

    pub fn flags(self, f: NodeFlags) -> Node {
        Node { flags: f, ..self }
    }

    pub fn focus(self, f: Focusable) -> Node {
        Node { focus: f, ..self }
    }

    pub fn label(self, t: Text) -> Node {
        Node { label: Label::Text(t), ..self }
    }

    pub fn description(self, t: Text) -> Node {
        Node { description: Some(t), ..self }
    }

    pub fn value(self, v: NodeValue) -> Node {
        Node { value: Some(v), ..self }
    }

    pub fn actions(self, a: Actions) -> Node {
        Node { actions: a, ..self }
    }

    pub fn cursor(self, c: Cursor) -> Node {
        Node { cursor: c, ..self }
    }
}

/// The nodes of one layout in push order, kept between layouts.
pub struct Frame {
    nodes: Vec<Node>,
}

#[allow(clippy::len_without_is_empty)]
impl Frame {
    /// An empty frame.
    pub(crate) fn new() -> Frame {
        Frame { nodes: Vec::new() }
    }

    /// Empties it for a new layout, keeping its capacity.
    pub(crate) fn clear(&mut self) {
        self.nodes.clear();
    }

    /// Adds a node after the others.
    pub(crate) fn push(&mut self, node: Node) {
        self.nodes.push(node);
    }

    /// The node with this id.
    pub fn get(&self, id: NodeId) -> Option<&Node> {
        self.nodes.iter().find(|n| n.id == id)
    }

    /// The last pushed node that contains `p`.
    pub fn hit(&self, p: PointP) -> Option<&Node> {
        self.nodes.iter().rev().find(|n| n.rect.contains(p))
    }

    /// The nodes in push order.
    pub fn iter(&self) -> std::slice::Iter<'_, Node> {
        self.nodes.iter()
    }

    /// The node count.
    pub fn len(&self) -> usize {
        self.nodes.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn node_ids_pack_owner_and_slot() {
        let id = NodeId::new(WidgetId(7), 3);
        assert_eq!((id.owner(), id.slot()), (WidgetId(7), 3));
        assert_eq!(NodeId::new(WidgetId(u32::MAX), u32::MAX).owner(), WidgetId(u32::MAX));
        assert!((NodeFlags::FOCUSABLE | NodeFlags::BUSY).contains(NodeFlags::BUSY));
        assert!(!NodeFlags::FOCUSABLE.contains(NodeFlags::BUSY));
    }

    #[test]
    fn frame_hits_the_last_pushed() {
        let a = Node::new(NodeId::new(WidgetId(1), 0), Role::Pane, RectP::wh(0, 0, 100, 100));
        let b = Node::new(NodeId::new(WidgetId(2), 0), Role::Button, RectP::wh(10, 10, 20, 20)).label(Text::from("OK"));
        let f = Frame { nodes: vec![a, b.clone()] };
        assert_eq!(f.hit(PointP { x: 15, y: 15 }), Some(&b));
        assert_eq!(f.hit(PointP { x: 50, y: 50 }).map(|n| n.role), Some(Role::Pane));
        assert_eq!(f.get(b.id).map(|n| &n.label), Some(&Label::Text(Text::from("OK"))));
        assert_eq!((f.len(), f.iter().count()), (2, 2));
    }
}
