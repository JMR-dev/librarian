//! The folder navigation tree (left pane): a lazily-expanding directory tree
//! rooted at "This PC".
//!
//! The tree is the source of truth for what the sidebar shows; the app drives
//! it with three operations — [`Tree::toggle`] (expand/collapse a node),
//! [`Tree::set_children`] (attach the result of an async load), and
//! [`Tree::reveal`] (walk toward a path, expanding ancestors so the current
//! location becomes visible). Nodes are addressed by a stable [`NodeId`] rather
//! than by position or path, so an async child-load always finds its target
//! even if the tree changed shape (or the same folder appears in two branches,
//! e.g. a known folder and its real parent) while the load was in flight.
//!
//! All logic here is pure and filesystem-free — the app supplies already-loaded
//! [`TreeChild`] lists — which keeps it unit-testable in isolation.

use std::path::Path;

use librarian_core::Location;

use crate::icons::IconKey;

/// Stable identity for a tree node, unique within a [`Tree`] for its lifetime.
pub type NodeId = u64;

/// The always-present root ("This PC") node.
pub const ROOT_ID: NodeId = 0;

/// Lazily-loaded children of a node.
enum Children {
    /// Not fetched yet. We show an expand chevron optimistically — most folders
    /// do contain subfolders, and finding out for sure would cost a scan.
    Unloaded,
    /// A fetch is in flight.
    Loading,
    /// Fetched. An empty vec is a *confirmed* leaf, so its chevron disappears.
    Loaded(Vec<TreeNode>),
}

/// One node in the folder tree.
struct TreeNode {
    id: NodeId,
    label: String,
    icon: IconKey,
    /// Where clicking the node navigates the main view.
    location: Location,
    expanded: bool,
    children: Children,
}

/// The data needed to create a child node, produced by an (async) load. Kept
/// free of [`NodeId`] so the loader doesn't need the id allocator — ids are
/// assigned when the children are grafted into the tree.
#[derive(Debug, Clone)]
pub struct TreeChild {
    pub label: String,
    pub icon: IconKey,
    pub location: Location,
}

/// A flattened, render-ready view of one visible node. Borrows from the tree.
pub struct TreeRow<'a> {
    pub id: NodeId,
    pub label: &'a str,
    pub icon: &'a IconKey,
    pub location: &'a Location,
    /// Indentation level; the root is 0.
    pub depth: usize,
    pub expanded: bool,
    /// Whether to draw an expand/collapse chevron (false for confirmed leaves).
    pub expandable: bool,
}

/// The outcome of a [`Tree::reveal`] step.
pub enum Reveal {
    /// To continue revealing, the children of this node must be loaded first.
    Load(NodeId, Location),
    /// A load is already in flight along the path; wait for it to complete.
    Wait,
    /// Revealing is finished — the target was reached, or can't be reached.
    Stop,
}

/// The folder tree and its node-id allocator.
pub struct Tree {
    root: TreeNode,
    next_id: NodeId,
}

impl Tree {
    /// A fresh tree whose root is "This PC", expanded and awaiting its child
    /// load (drives + known folders). The app kicks that load off at startup.
    pub fn new() -> Self {
        let root = TreeNode {
            id: ROOT_ID,
            label: "This PC".to_string(),
            icon: IconKey::Folder,
            location: Location::ThisPc,
            expanded: true,
            children: Children::Loading,
        };
        Self {
            root,
            next_id: ROOT_ID + 1,
        }
    }

    /// Flatten the expanded nodes into render rows, depth-first.
    pub fn visible_rows(&self) -> Vec<TreeRow<'_>> {
        let mut rows = Vec::new();
        push_rows(&self.root, 0, &mut rows);
        rows
    }

    /// Append the icon keys of every visible node, so the app can request them
    /// from the shared icon cache alongside the main list's.
    pub fn collect_icon_keys(&self, out: &mut Vec<IconKey>) {
        collect_keys(&self.root, out);
    }

    /// Expand or collapse the node `id`. Returns the node and location to load
    /// when expanding a not-yet-loaded node, otherwise `None` (collapse, or
    /// children already present).
    pub fn toggle(&mut self, id: NodeId) -> Option<(NodeId, Location)> {
        let node = find_mut(&mut self.root, id)?;
        if node.expanded {
            node.expanded = false;
            None
        } else {
            node.expanded = true;
            if matches!(node.children, Children::Unloaded) {
                node.children = Children::Loading;
                Some((node.id, node.location.clone()))
            } else {
                None
            }
        }
    }

    /// Graft freshly-loaded `children` onto node `id`, assigning each a new id.
    /// A no-op if the node has since vanished. An empty list marks a leaf.
    pub fn set_children(&mut self, id: NodeId, children: Vec<TreeChild>) {
        // Allocate ids first (borrows `self.next_id`), then locate the node.
        let nodes = self.build_nodes(children);
        if let Some(node) = find_mut(&mut self.root, id) {
            node.children = Children::Loaded(nodes);
        }
    }

    /// Walk from the root toward `target`, expanding each ancestor so the target
    /// becomes visible. One step: if an ancestor's children aren't loaded yet it
    /// returns [`Reveal::Load`]; call again once that load lands to continue.
    pub fn reveal(&mut self, target: &Path) -> Reveal {
        reveal_node(&mut self.root, target)
    }

    fn build_nodes(&mut self, children: Vec<TreeChild>) -> Vec<TreeNode> {
        children
            .into_iter()
            .map(|c| {
                let id = self.next_id;
                self.next_id += 1;
                TreeNode {
                    id,
                    label: c.label,
                    icon: c.icon,
                    location: c.location,
                    expanded: false,
                    children: Children::Unloaded,
                }
            })
            .collect()
    }
}

impl Default for Tree {
    fn default() -> Self {
        Self::new()
    }
}

fn push_rows<'a>(node: &'a TreeNode, depth: usize, out: &mut Vec<TreeRow<'a>>) {
    let expandable = match &node.children {
        Children::Unloaded | Children::Loading => true,
        Children::Loaded(children) => !children.is_empty(),
    };
    out.push(TreeRow {
        id: node.id,
        label: &node.label,
        icon: &node.icon,
        location: &node.location,
        depth,
        expanded: node.expanded,
        expandable,
    });
    if node.expanded
        && let Children::Loaded(children) = &node.children
    {
        for child in children {
            push_rows(child, depth + 1, out);
        }
    }
}

fn collect_keys(node: &TreeNode, out: &mut Vec<IconKey>) {
    out.push(node.icon.clone());
    if node.expanded
        && let Children::Loaded(children) = &node.children
    {
        for child in children {
            collect_keys(child, out);
        }
    }
}

fn find_mut(node: &mut TreeNode, id: NodeId) -> Option<&mut TreeNode> {
    if node.id == id {
        return Some(node);
    }
    if let Children::Loaded(children) = &mut node.children {
        for child in children {
            if let Some(found) = find_mut(child, id) {
                return Some(found);
            }
        }
    }
    None
}

fn reveal_node(node: &mut TreeNode, target: &Path) -> Reveal {
    // Reached the target: it's already a node, leave it for the caller to
    // select. Don't force-expand it — we only reveal, not open.
    if node.location.as_path() == Some(target) {
        return Reveal::Stop;
    }
    // This node is an ancestor of the target; expand it and descend.
    node.expanded = true;
    match &mut node.children {
        Children::Unloaded => {
            node.children = Children::Loading;
            Reveal::Load(node.id, node.location.clone())
        }
        Children::Loading => Reveal::Wait,
        Children::Loaded(children) => match best_child_index(children, target) {
            Some(i) => reveal_node(&mut children[i], target),
            None => Reveal::Stop, // target not under any child (hidden, gone, …)
        },
    }
}

/// Among `children`, the index of the one whose path is the longest prefix of
/// `target` (the deepest ancestor to descend into). `starts_with` is
/// component-wise, so `C:\Users` matches `C:\Users\me` but not `C:\UsersX`.
fn best_child_index(children: &[TreeNode], target: &Path) -> Option<usize> {
    children
        .iter()
        .enumerate()
        .filter_map(|(i, c)| c.location.as_path().map(|p| (i, p)))
        .filter(|(_, p)| target.starts_with(p))
        .max_by_key(|(_, p)| p.components().count())
        .map(|(i, _)| i)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn child(label: &str, path: &str) -> TreeChild {
        TreeChild {
            label: label.to_string(),
            icon: IconKey::Folder,
            location: Location::Path(PathBuf::from(path)),
        }
    }

    /// Find a visible row by label, for assertions.
    fn row_id(tree: &Tree, label: &str) -> NodeId {
        tree.visible_rows()
            .iter()
            .find(|r| r.label == label)
            .unwrap_or_else(|| panic!("no visible row labelled {label}"))
            .id
    }

    #[test]
    fn root_is_visible_and_loading() {
        let tree = Tree::new();
        let rows = tree.visible_rows();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].label, "This PC");
        assert!(rows[0].expanded);
        // Loading counts as expandable so the chevron shows immediately.
        assert!(rows[0].expandable);
    }

    #[test]
    fn set_children_makes_them_visible_under_expanded_root() {
        let mut tree = Tree::new();
        tree.set_children(ROOT_ID, vec![child("C:", "C:\\"), child("D:", "D:\\")]);
        let labels: Vec<&str> = tree.visible_rows().iter().map(|r| r.label).collect();
        assert_eq!(labels, ["This PC", "C:", "D:"]);
    }

    #[test]
    fn ids_are_unique_per_node() {
        let mut tree = Tree::new();
        tree.set_children(ROOT_ID, vec![child("C:", "C:\\")]);
        let c = row_id(&tree, "C:");
        tree.toggle(c); // expand so its children become visible
        tree.set_children(c, vec![child("Users", "C:\\Users")]);
        let users = row_id(&tree, "Users");
        assert_ne!(c, users);
        assert_ne!(ROOT_ID, c);
    }

    #[test]
    fn toggle_collapses_and_re_expands_without_reloading() {
        let mut tree = Tree::new();
        tree.set_children(ROOT_ID, vec![child("C:", "C:\\")]);
        let c = row_id(&tree, "C:");
        tree.toggle(c); // expand C: (its first expansion requests a load)
        tree.set_children(c, vec![child("Users", "C:\\Users")]);
        assert_eq!(tree.visible_rows().len(), 3); // This PC, C:, Users

        // Collapsing C: hides Users; it requests no reload.
        assert!(tree.toggle(c).is_none());
        assert_eq!(tree.visible_rows().len(), 2);

        // Re-expanding shows it again with no reload (children cached).
        assert!(tree.toggle(c).is_none());
        assert_eq!(tree.visible_rows().len(), 3);
    }

    #[test]
    fn toggle_unloaded_node_requests_its_location() {
        let mut tree = Tree::new();
        tree.set_children(ROOT_ID, vec![child("C:", "C:\\")]);
        let c = row_id(&tree, "C:");
        // C: was just added Unloaded and collapsed; expanding asks to load it.
        match tree.toggle(c) {
            Some((id, Location::Path(p))) => {
                assert_eq!(id, c);
                assert_eq!(p, PathBuf::from("C:\\"));
            }
            other => panic!("expected a load request for C:\\, got {other:?}"),
        }
    }

    #[test]
    fn empty_children_make_a_leaf() {
        let mut tree = Tree::new();
        tree.set_children(ROOT_ID, vec![child("C:", "C:\\")]);
        let c = row_id(&tree, "C:");
        tree.toggle(c); // expand
        tree.set_children(c, Vec::new()); // …discovers it has no subfolders
        let row = tree
            .visible_rows()
            .into_iter()
            .find(|r| r.id == c)
            .unwrap();
        assert!(!row.expandable, "a confirmed-empty node shows no chevron");
    }

    #[test]
    fn reveal_loads_ancestors_then_stops_at_target() {
        let mut tree = Tree::new();
        tree.set_children(ROOT_ID, vec![child("C:", "C:\\"), child("D:", "D:\\")]);
        let target = PathBuf::from("C:\\Users\\me");

        // First step: C: isn't loaded, so reveal asks to load it (not D:).
        match tree.reveal(&target) {
            Reveal::Load(_, Location::Path(p)) => assert_eq!(p, PathBuf::from("C:\\")),
            _ => panic!("expected to load C:\\"),
        }

        // C:'s children arrive; next step descends and asks to load C:\Users.
        let c = row_id(&tree, "C:");
        tree.set_children(c, vec![child("Users", "C:\\Users")]);
        match tree.reveal(&target) {
            Reveal::Load(_, Location::Path(p)) => assert_eq!(p, PathBuf::from("C:\\Users")),
            _ => panic!("expected to load C:\\Users"),
        }

        // C:\Users's children arrive (including the target); reveal completes.
        let users = row_id(&tree, "Users");
        tree.set_children(users, vec![child("me", "C:\\Users\\me")]);
        assert!(matches!(tree.reveal(&target), Reveal::Stop));

        // The whole chain is now visible.
        let labels: Vec<&str> = tree.visible_rows().iter().map(|r| r.label).collect();
        assert_eq!(labels, ["This PC", "C:", "Users", "me", "D:"]);
    }

    #[test]
    fn reveal_picks_the_deepest_matching_branch() {
        // The same folder reachable two ways: a known-folder shortcut and the
        // real parent chain. The longest prefix (the shortcut) is chosen.
        let mut tree = Tree::new();
        tree.set_children(
            ROOT_ID,
            vec![
                child("C:", "C:\\"),
                child("Documents", "C:\\Users\\me\\Documents"),
            ],
        );
        let target = PathBuf::from("C:\\Users\\me\\Documents\\Work");
        match tree.reveal(&target) {
            Reveal::Load(_, Location::Path(p)) => {
                assert_eq!(p, PathBuf::from("C:\\Users\\me\\Documents"));
            }
            _ => panic!("expected to descend via the Documents shortcut"),
        }
    }

    #[test]
    fn reveal_gives_up_when_target_is_absent() {
        let mut tree = Tree::new();
        tree.set_children(ROOT_ID, vec![child("C:", "C:\\")]);
        let c = row_id(&tree, "C:");
        tree.set_children(c, Vec::new()); // C: has no listed subfolders
        // Target claims to be under C: but no child matches → give up cleanly.
        assert!(matches!(
            tree.reveal(&PathBuf::from("C:\\Hidden\\x")),
            Reveal::Stop
        ));
    }
}
