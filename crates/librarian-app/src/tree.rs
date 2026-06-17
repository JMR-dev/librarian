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

use librarian_core::{Location, is_wsl_host};

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
    /// Children to attach immediately. `None` means the node is loaded lazily on
    /// first expand; `Some` (even empty) means its children are already known
    /// and grafted along with it — used to nest a curated subtree (e.g. the
    /// user's folders under their home node) rather than a raw directory listing.
    pub children: Option<Vec<TreeChild>>,
    /// Whether the node starts expanded. Only meaningful alongside pre-attached
    /// [`children`](Self::children); a lazy node can't show children it hasn't
    /// loaded yet.
    pub expanded: bool,
}

impl TreeChild {
    /// A node whose children are fetched lazily, the first time it's expanded.
    pub fn lazy(label: String, icon: IconKey, location: Location) -> Self {
        Self {
            label,
            icon,
            location,
            children: None,
            expanded: false,
        }
    }

    /// A node grafted with its `children` already attached and shown expanded —
    /// for a curated, pre-loaded subtree.
    pub fn branch(
        label: String,
        icon: IconKey,
        location: Location,
        children: Vec<TreeChild>,
    ) -> Self {
        Self {
            label,
            icon,
            location,
            children: Some(children),
            expanded: true,
        }
    }
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
#[derive(Debug)]
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
    /// A hidden synthetic container whose children are the *top-level* nodes
    /// (the user's folders, then a "This PC" node holding the drives). It is
    /// never rendered itself — [`visible_rows`](Self::visible_rows) starts at
    /// its children — which lets the sidebar show a forest of roots rather than
    /// a single "This PC" parent over everything.
    root: TreeNode,
    next_id: NodeId,
}

impl Tree {
    /// A fresh tree whose (hidden) root is awaiting its top-level children —
    /// the user's folders plus a "This PC" node. The app kicks that load off at
    /// startup; until it lands, the sidebar is empty.
    pub fn new() -> Self {
        let root = TreeNode {
            id: ROOT_ID,
            label: String::new(),
            icon: IconKey::Folder,
            // Never navigated or matched against a path; only ever descended
            // through. `ThisPc` has no path, so it can't collide with a target.
            location: Location::ThisPc,
            expanded: true,
            children: Children::Loading,
        };
        Self {
            root,
            next_id: ROOT_ID + 1,
        }
    }

    /// Flatten the expanded nodes into render rows, depth-first. The hidden root
    /// is skipped, so its children are the top level (depth 0).
    pub fn visible_rows(&self) -> Vec<TreeRow<'_>> {
        let mut rows = Vec::new();
        if let Children::Loaded(children) = &self.root.children {
            for child in children {
                push_rows(child, 0, &mut rows);
            }
        }
        rows
    }

    /// Append the icon keys of every visible node, so the app can request them
    /// from the shared icon cache alongside the main list's. The hidden root has
    /// no icon of its own, so collection starts at its children.
    pub fn collect_icon_keys(&self, out: &mut Vec<IconKey>) {
        if let Children::Loaded(children) = &self.root.children {
            for child in children {
                collect_keys(child, out);
            }
        }
    }

    /// The id of the top-level "This PC" node, once the root's children have
    /// loaded. The app uses it to auto-expand "This PC" at startup so the drives
    /// are visible without a manual click.
    pub fn this_pc_id(&self) -> Option<NodeId> {
        match &self.root.children {
            Children::Loaded(children) => children
                .iter()
                .find(|c| matches!(c.location, Location::ThisPc))
                .map(|c| c.id),
            _ => None,
        }
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
                // Pre-attached children are grafted (and recursively built) now;
                // otherwise the node is left unloaded for a lazy fetch later.
                let children = match c.children {
                    Some(kids) => Children::Loaded(self.build_nodes(kids)),
                    None => Children::Unloaded,
                };
                TreeNode {
                    id,
                    label: c.label,
                    icon: c.icon,
                    location: c.location,
                    expanded: c.expanded,
                    children,
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
        Children::Loaded(children) => {
            // Prefer the deepest path-prefix child (e.g. a user-folder shortcut).
            if let Some(i) = best_child_index(children, target) {
                return reveal_node(&mut children[i], target);
            }
            // No path child matched. At the top level the drives live under the
            // pathless "This PC" node and the distros under the pathless "Linux"
            // (Wsl) node, so fall back into whichever virtual root owns this
            // target — one of its children will prefix it.
            let virtual_root = if is_wsl_path(target) {
                Location::Wsl
            } else {
                Location::ThisPc
            };
            if let Some(i) = children.iter().position(|c| c.location == virtual_root) {
                return reveal_node(&mut children[i], target);
            }
            Reveal::Stop // target not under any child (hidden, gone, …)
        }
    }
}

/// Whether `target` lives under the WSL "Linux" group — a `\\wsl.localhost\`
/// (or legacy `\\wsl$\`) UNC path — so reveal descends into the `Wsl` node
/// rather than "This PC". A cheap host-segment check keeps this module
/// filesystem-free.
fn is_wsl_path(target: &Path) -> bool {
    let lossy = target.to_string_lossy();
    let Some(rest) = lossy
        .strip_prefix(r"\\")
        .or_else(|| lossy.strip_prefix("//"))
    else {
        return false;
    };
    let host_len = rest.find(['\\', '/']).unwrap_or(rest.len());
    let host = &rest[..host_len];
    is_wsl_host(host)
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
        TreeChild::lazy(
            label.to_string(),
            IconKey::Folder,
            Location::Path(PathBuf::from(path)),
        )
    }

    /// A pathless "This PC" child — the drives' container at the top level.
    fn this_pc() -> TreeChild {
        TreeChild::lazy("This PC".to_string(), IconKey::Folder, Location::ThisPc)
    }

    /// A pathless "Linux" child — the WSL distros' container at the top level.
    fn wsl_group() -> TreeChild {
        TreeChild::lazy("Linux".to_string(), IconKey::Wsl, Location::Wsl)
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
    fn root_is_hidden_until_children_load() {
        // The synthetic root isn't rendered, so the sidebar is empty until its
        // top-level children arrive.
        let tree = Tree::new();
        assert!(tree.visible_rows().is_empty());
    }

    #[test]
    fn top_level_children_render_at_depth_zero() {
        let mut tree = Tree::new();
        tree.set_children(ROOT_ID, vec![child("C:", "C:\\"), child("D:", "D:\\")]);
        let rows = tree.visible_rows();
        // The hidden root contributes no row of its own.
        let labels: Vec<&str> = rows.iter().map(|r| r.label).collect();
        assert_eq!(labels, ["C:", "D:"]);
        assert!(rows.iter().all(|r| r.depth == 0));
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
        assert_eq!(tree.visible_rows().len(), 2); // C:, Users

        // Collapsing C: hides Users; it requests no reload.
        assert!(tree.toggle(c).is_none());
        assert_eq!(tree.visible_rows().len(), 1);

        // Re-expanding shows it again with no reload (children cached).
        assert!(tree.toggle(c).is_none());
        assert_eq!(tree.visible_rows().len(), 2);
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
        let row = tree.visible_rows().into_iter().find(|r| r.id == c).unwrap();
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

        // The whole chain is now visible (the hidden root adds no row).
        let labels: Vec<&str> = tree.visible_rows().iter().map(|r| r.label).collect();
        assert_eq!(labels, ["C:", "Users", "me", "D:"]);
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

    #[test]
    fn this_pc_id_finds_the_drives_container() {
        let mut tree = Tree::new();
        assert_eq!(tree.this_pc_id(), None, "no node before the roots load");
        // Top level: a user folder, then the "This PC" node.
        tree.set_children(
            ROOT_ID,
            vec![child("Desktop", "C:\\Users\\me\\Desktop"), this_pc()],
        );
        let pc = tree.this_pc_id().expect("This PC node present");
        assert_eq!(pc, row_id(&tree, "This PC"));
    }

    #[test]
    fn reveal_descends_through_this_pc_to_a_drive_path() {
        // Drives are no longer top-level: they sit under the pathless "This PC"
        // node, so revealing a drive path must fall back into it.
        let mut tree = Tree::new();
        tree.set_children(
            ROOT_ID,
            vec![child("Desktop", "C:\\Users\\me\\Desktop"), this_pc()],
        );
        let target = PathBuf::from("C:\\Windows");

        // No top-level path child prefixes C:\Windows, so reveal descends into
        // "This PC" and asks to load it (its drives aren't loaded yet).
        let pc = tree.this_pc_id().unwrap();
        match tree.reveal(&target) {
            Reveal::Load(id, Location::ThisPc) => assert_eq!(id, pc),
            other => panic!("expected to load the This PC node, got {other:?}"),
        }

        // Its drives arrive; the next step descends into C:\ toward the target.
        tree.set_children(pc, vec![child("C:", "C:\\")]);
        match tree.reveal(&target) {
            Reveal::Load(_, Location::Path(p)) => assert_eq!(p, PathBuf::from("C:\\")),
            other => panic!("expected to load C:\\, got {other:?}"),
        }
    }

    #[test]
    fn reveal_descends_through_wsl_to_a_distro_path() {
        // Distros sit under the pathless "Linux" node, so revealing a
        // \\wsl.localhost\<distro> path must fall back into the Wsl group
        // (not This PC, which a non-WSL path would).
        let mut tree = Tree::new();
        tree.set_children(
            ROOT_ID,
            vec![
                child("Desktop", "C:\\Users\\me\\Desktop"),
                this_pc(),
                wsl_group(),
            ],
        );
        let target = PathBuf::from(r"\\wsl.localhost\Ubuntu\home");

        // No top-level path child prefixes the target and it's a WSL path, so
        // reveal descends into the "Linux" node and asks to load it.
        let wsl = row_id(&tree, "Linux");
        match tree.reveal(&target) {
            Reveal::Load(id, Location::Wsl) => assert_eq!(id, wsl),
            other => panic!("expected to load the Wsl node, got {other:?}"),
        }

        // Its distros arrive; the next step descends into the Ubuntu root.
        tree.set_children(wsl, vec![child("Ubuntu", r"\\wsl.localhost\Ubuntu")]);
        match tree.reveal(&target) {
            Reveal::Load(_, Location::Path(p)) => {
                assert_eq!(p, PathBuf::from(r"\\wsl.localhost\Ubuntu"))
            }
            other => panic!("expected to load the Ubuntu distro root, got {other:?}"),
        }
    }

    #[test]
    fn branch_grafts_children_and_starts_expanded() {
        // A `branch` node arrives with its children already attached and shown,
        // so they're visible (nested, depth 1) without any lazy load or toggle.
        let mut tree = Tree::new();
        let home = TreeChild::branch(
            "me".to_string(),
            IconKey::Folder,
            Location::Path(PathBuf::from("C:\\Users\\me")),
            vec![
                child("Desktop", "C:\\Users\\me\\Desktop"),
                child("Documents", "C:\\Users\\me\\Documents"),
            ],
        );
        tree.set_children(ROOT_ID, vec![home]);

        let rows = tree.visible_rows();
        let labels: Vec<&str> = rows.iter().map(|r| r.label).collect();
        assert_eq!(labels, ["me", "Desktop", "Documents"]);
        // The home node is depth 0 and expanded; its folders are nested at 1.
        assert_eq!(rows[0].depth, 0);
        assert!(rows[0].expanded);
        assert!(rows[1].depth == 1 && rows[2].depth == 1);

        // Collapsing the pre-loaded node requests no reload (children cached).
        let me = row_id(&tree, "me");
        assert!(tree.toggle(me).is_none());
        assert_eq!(tree.visible_rows().len(), 1);
    }
}
