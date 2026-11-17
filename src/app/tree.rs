//! Filtered-tree projection for the Collections sidebar.
//!
//! [`SidebarNode`] is the pure view-model the GUI shell renders: built from
//! a [`Workspace`] plus an optional case-insensitive substring filter.
//!
//! Filter semantics:
//! - A node **matches** when its name (case-insensitively) contains the
//!   filter, or when any descendant matches.
//! - Nodes that do not match (self or descendants) are pruned.
//! - Nodes with matching descendants are force-expanded
//!   ([`SidebarNode::force_expanded`]) so the match stays visible; a node
//!   that matches only by its own name shows *collapsed* (non-matching
//!   branches stay hidden).
//!
//! With an empty/whitespace filter the full tree is returned and
//! `force_expanded` is `false` everywhere (visibility then depends solely on
//! the shell's `expanded` set).

use crate::app::NodeKind;
use crate::domain::{Collection, Folder, HttpMethod, RequestEntry, Workspace};
use std::path::{Path, PathBuf};

/// A node of the sidebar tree projection.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SidebarNode {
    /// Kind of node (Collection, Folder or Request).
    pub kind: NodeKind,
    /// Display name.
    pub name: String,
    /// Path on disk (Collection/Folder directory or Request `.yaml` file).
    pub path: PathBuf,
    /// HTTP method for Request nodes; filled in by the shell from parsed
    /// request files (the tree only knows `name`/`path`).
    pub method: Option<HttpMethod>,
    /// Child nodes (already pruned by the filter).
    pub children: Vec<SidebarNode>,
    /// Recursive Request count (Collections/Folders; 0 for Requests).
    pub request_count: usize,
    /// Nesting depth (Collections are depth 0).
    pub depth: usize,
    /// Whether the node must be expanded because a descendant matched the
    /// filter. Always `false` when no filter is active.
    pub force_expanded: bool,
}

impl SidebarNode {
    /// Whether this node is a container (expandable, shows a count badge).
    pub fn is_container(&self) -> bool {
        self.kind != NodeKind::Request
    }
}

/// Build the sidebar tree for `workspace` filtered by `filter`.
///
/// An empty (or whitespace-only) filter yields the full tree.
pub fn build_tree(workspace: &Workspace, filter: &str) -> Vec<SidebarNode> {
    let filter = filter.trim().to_lowercase();
    let mut nodes: Vec<SidebarNode> = workspace
        .collections
        .iter()
        .map(SidebarNode::from_collection)
        .collect();

    if filter.is_empty() {
        nodes
    } else {
        nodes.retain_mut(|node| prune(node, &filter));
        nodes
    }
}

/// Recursively prune a subtree, returning whether the node survives.
fn prune(node: &mut SidebarNode, filter: &str) -> bool {
    node.children.retain_mut(|child| prune(child, filter));
    node.force_expanded = !node.children.is_empty();
    node.self_matches(filter) || !node.children.is_empty()
}

impl SidebarNode {
    fn self_matches(&self, filter: &str) -> bool {
        self.name.to_lowercase().contains(filter)
    }
}

impl SidebarNode {
    fn from_collection(collection: &Collection) -> SidebarNode {
        let mut children: Vec<SidebarNode> = collection
            .folders
            .iter()
            .map(|folder| SidebarNode::from_folder(folder, 1))
            .collect();
        children.extend(
            collection
                .requests
                .iter()
                .map(|request| SidebarNode::from_request(request, 1)),
        );
        SidebarNode {
            kind: NodeKind::Collection,
            name: collection.name.clone(),
            path: collection.path.clone(),
            method: None,
            children,
            request_count: collection.total_request_count(),
            depth: 0,
            force_expanded: false,
        }
    }

    fn from_folder(folder: &Folder, depth: usize) -> SidebarNode {
        let mut children: Vec<SidebarNode> = folder
            .folders
            .iter()
            .map(|nested| SidebarNode::from_folder(nested, depth + 1))
            .collect();
        children.extend(
            folder
                .requests
                .iter()
                .map(|request| SidebarNode::from_request(request, depth + 1)),
        );
        SidebarNode {
            kind: NodeKind::Folder,
            name: folder.name.clone(),
            path: folder.path.clone(),
            method: None,
            children,
            request_count: folder.total_request_count(),
            depth,
            force_expanded: false,
        }
    }

    fn from_request(request: &RequestEntry, depth: usize) -> SidebarNode {
        SidebarNode {
            kind: NodeKind::Request,
            name: request.name.clone(),
            path: request.path.clone(),
            method: None,
            children: Vec::new(),
            request_count: 0,
            depth,
            force_expanded: false,
        }
    }
}

/// Look up the kind and display name of the node at `path` in `workspace`.
pub fn find(workspace: &Workspace, path: &Path) -> Option<(NodeKind, String)> {
    for collection in &workspace.collections {
        if collection.path == path {
            return Some((NodeKind::Collection, collection.name.clone()));
        }
        for folder in &collection.folders {
            if let Some(found) = find_in_folder(folder, path) {
                return Some(found);
            }
        }
        for request in &collection.requests {
            if request.path == path {
                return Some((NodeKind::Request, request.name.clone()));
            }
        }
    }
    None
}

fn find_in_folder(folder: &Folder, path: &Path) -> Option<(NodeKind, String)> {
    if folder.path == path {
        return Some((NodeKind::Folder, folder.name.clone()));
    }
    for nested in &folder.folders {
        if let Some(found) = find_in_folder(nested, path) {
            return Some(found);
        }
    }
    for request in &folder.requests {
        if request.path == path {
            return Some((NodeKind::Request, request.name.clone()));
        }
    }
    None
}

/// Whether `path` is a Request in the loaded `workspace`.
pub fn contains_request(workspace: &Workspace, path: &Path) -> bool {
    find(workspace, path).is_some_and(|(kind, _)| kind == NodeKind::Request)
}

/// Names of the child Folders of the Collection/Folder at `parent`.
/// (Folder names on disk are already sanitized.)
pub fn folder_names_under(workspace: &Workspace, parent: &Path) -> Vec<String> {
    let mut names = Vec::new();
    for collection in &workspace.collections {
        if collection.path == parent {
            names.extend(collection.folders.iter().map(|f| f.name.clone()));
            return names;
        }
        for folder in &collection.folders {
            if folder_names_in(folder, parent, &mut names) {
                return names;
            }
        }
    }
    names
}

fn folder_names_in(folder: &Folder, parent: &Path, names: &mut Vec<String>) -> bool {
    if folder.path == parent {
        names.extend(folder.folders.iter().map(|f| f.name.clone()));
        return true;
    }
    folder
        .folders
        .iter()
        .any(|nested| folder_names_in(nested, parent, names))
}

/// Display names of the Requests directly inside the Collection/Folder at
/// `parent`. (Collision checks sanitize both sides before comparing.)
pub fn request_names_in(workspace: &Workspace, parent: &Path) -> Vec<String> {
    let mut names = Vec::new();
    for collection in &workspace.collections {
        if collection.path == parent {
            names.extend(collection.requests.iter().map(|r| r.name.clone()));
            return names;
        }
        for folder in &collection.folders {
            if request_names_in_folder(folder, parent, &mut names) {
                return names;
            }
        }
    }
    names
}

fn request_names_in_folder(folder: &Folder, parent: &Path, names: &mut Vec<String>) -> bool {
    if folder.path == parent {
        names.extend(folder.requests.iter().map(|r| r.name.clone()));
        return true;
    }
    folder
        .folders
        .iter()
        .any(|nested| request_names_in_folder(nested, parent, names))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::new_request;
    use crate::domain::{HttpMethod, Request};
    use crate::storage::{FsWorkspaceStorage, WorkspaceStorage};

    fn seeded_workspace() -> (tempfile::TempDir, Workspace) {
        let dir = tempfile::tempdir().unwrap();
        let storage = FsWorkspaceStorage;
        let collection = storage.create_collection(dir.path(), "API").unwrap();
        let users = storage.create_folder(&collection.path, "Users").unwrap();
        let admin = storage.create_folder(&users.path, "Admin").unwrap();
        storage
            .create_request(
                &admin.path,
                &Request {
                    name: "Delete user".to_string(),
                    method: HttpMethod::Delete,
                    url: "https://example.com/users/1".to_string(),
                    ..new_request("")
                },
            )
            .unwrap();
        storage
            .create_request(&collection.path, &new_request("Get health"))
            .unwrap();
        let workspace = storage.load_workspace(dir.path()).unwrap();
        (dir, workspace)
    }

    #[test]
    fn filter_prunes_non_matching_branches_and_force_expands_ancestors() {
        let (_dir, workspace) = seeded_workspace();

        let nodes = build_tree(&workspace, "delete user");
        assert_eq!(nodes.len(), 1);
        let api = &nodes[0];
        assert_eq!(api.name, "API");
        assert!(api.force_expanded, "collection is force-expanded");
        assert_eq!(api.children.len(), 1);
        let users = &api.children[0];
        assert_eq!(users.name, "Users");
        assert!(users.force_expanded);
        let admin = &users.children[0];
        assert_eq!(admin.name, "Admin");
        assert!(
            admin.force_expanded,
            "folder with the match is force-expanded"
        );
        assert_eq!(admin.children.len(), 1);
        assert_eq!(admin.children[0].name, "Delete user");
        assert_eq!(admin.children[0].kind, NodeKind::Request);
        assert!(!admin.children[0].force_expanded);
    }

    #[test]
    fn filter_is_case_insensitive_and_hides_siblings() {
        let (_dir, workspace) = seeded_workspace();

        // Case-insensitive match on a request.
        let nodes = build_tree(&workspace, "DELETE USER");
        assert_eq!(nodes.len(), 1);
        assert_eq!(
            nodes[0].children[0].children[0].children[0].name,
            "Delete user"
        );

        // Matching the collection by name only: children stay hidden.
        let nodes = build_tree(&workspace, "api");
        assert_eq!(nodes.len(), 1);
        assert_eq!(nodes[0].name, "API");
        assert!(
            nodes[0].children.is_empty(),
            "non-matching branches are hidden"
        );
        assert!(!nodes[0].force_expanded);
    }

    #[test]
    fn empty_filter_returns_full_tree_without_force_expansion() {
        let (_dir, workspace) = seeded_workspace();

        let nodes = build_tree(&workspace, "");
        assert_eq!(nodes.len(), 1);
        let api = &nodes[0];
        assert_eq!(api.request_count, 2);
        assert!(!api.force_expanded);
        assert_eq!(api.children.len(), 2); // folder Users + request Get health

        let nodes = build_tree(&workspace, "   ");
        assert_eq!(nodes.len(), 1);
        assert_eq!(nodes[0].children.len(), 2);
    }

    #[test]
    fn find_reports_kinds_and_names() {
        let (dir, workspace) = seeded_workspace();

        let root = dir.path();
        let api = root.join("API");
        let users = api.join("Users");
        let admin = users.join("Admin");
        let delete_user = admin.join("Delete user.yaml");
        let health = api.join("Get health.yaml");

        assert_eq!(
            find(&workspace, &api),
            Some((NodeKind::Collection, "API".to_string()))
        );
        assert_eq!(
            find(&workspace, &users),
            Some((NodeKind::Folder, "Users".to_string()))
        );
        assert_eq!(
            find(&workspace, &delete_user),
            Some((NodeKind::Request, "Delete user".to_string()))
        );
        assert_eq!(
            find(&workspace, &health),
            Some((NodeKind::Request, "Get health".to_string()))
        );
        assert_eq!(find(&workspace, &root.join("Nope")), None);
        assert!(contains_request(&workspace, &health));
        assert!(!contains_request(&workspace, &users));
    }

    #[test]
    fn sibling_name_lookups() {
        let (dir, workspace) = seeded_workspace();
        let api = dir.path().join("API");
        let users = api.join("Users");
        let admin = users.join("Admin");

        assert_eq!(folder_names_under(&workspace, &api), vec!["Users"]);
        assert_eq!(folder_names_under(&workspace, &users), vec!["Admin"]);
        assert!(folder_names_under(&workspace, &admin).is_empty());
        assert_eq!(request_names_in(&workspace, &admin), vec!["Delete user"]);
        assert_eq!(request_names_in(&workspace, &api), vec!["Get health"]);
    }
}
