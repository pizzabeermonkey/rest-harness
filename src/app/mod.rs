//! UI-agnostic GUI core: [`AppState`], [`Message`], [`Effect`] and the pure
//! `update` reducer, plus the filtered-tree projection in [`tree`].
//!
//! This module contains **no `iced` types** — it is deliberately
//! `iced`-free so the reducer and tree logic are unit-testable without a
//! display. The `iced` shell (binary's `gui` module) owns the mapping:
//!
//! ```text
//! Message ──> AppState::update ──> Vec<Effect> ──> shell maps to Tasks ──> Messages
//! ```
//!
//! Extension points for the next increment (Request composer):
//! - Add `Message::Composer(ComposerMsg)` / `Message::Tab(TabMsg)` variants
//!   to [`Message`] (one-line additions) and a new match arm in
//!   [`AppState::update`].
//! - `SidebarMsg::NodeActivated(path)` records
//!   [`AppState::last_opened`] — the composer consumes this to open the
//!   clicked Request.
//! - New shell-side work is expressed as new [`Effect`] variants; the shell
//!   maps each to `iced::Task`s. After ANY successful CRUD effect the shell
//!   reloads the workspace from disk and sends
//!   `Message::Workspace(WorkspaceMsg::Loaded(..))` — state must reflect
//!   disk truth.
//!
//! [`tree`]: self::tree

pub mod tree;

use crate::config::SAMPLE_COLLECTION_NAME;
use crate::domain::{HttpMethod, Request, Workspace};
use crate::storage::sanitize_filename;
use std::collections::{BTreeMap, HashSet};
use std::path::{Path, PathBuf};

/// Kind of a sidebar tree node.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum NodeKind {
    /// A top-level Collection directory.
    Collection,
    /// A Folder directory (inside a Collection or another Folder).
    Folder,
    /// A Request `.yaml` file.
    Request,
}

impl NodeKind {
    /// Lowercase label used in status messages and modal titles.
    pub fn label(self) -> &'static str {
        match self {
            NodeKind::Collection => "collection",
            NodeKind::Folder => "folder",
            NodeKind::Request => "request",
        }
    }
}

/// Which sidebar tab is active.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum SidebarTab {
    /// The Collections tree.
    #[default]
    Collections,
    /// The History list.
    History,
}

/// The result of loading a Workspace from disk, sent back through
/// `Message::Workspace(WorkspaceMsg::Loaded(..))`.
///
/// Bundles the [`Workspace`] plus the HTTP method of every Request file
/// (the `RequestEntry`s in the tree carry only `name`/`path`; the sidebar
/// shows a method badge per request, so the shell parses the files once per
/// load instead of on every frame).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LoadedWorkspace {
    /// The freshly loaded Workspace (disk truth).
    pub workspace: Workspace,
    /// HTTP method per Request path, parsed from the request files.
    pub request_methods: BTreeMap<PathBuf, HttpMethod>,
}

impl LoadedWorkspace {
    /// Bundle a Workspace with no pre-parsed request methods.
    pub fn new(workspace: Workspace) -> Self {
        LoadedWorkspace {
            workspace,
            request_methods: BTreeMap::new(),
        }
    }
}

/// Messages for the Workspace bar (load/pick/reload).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WorkspaceMsg {
    /// Reload the current workspace from disk.
    Reload,
    /// Open the folder picker (emits `Effect::PickWorkspaceFolder`).
    PickWorkspace,
    /// The user picked a folder in the OS dialog (or cancelled).
    PickedRoot(Result<PathBuf, String>),
    /// The shell loaded the workspace (or an operation failed — the error
    /// text lands in [`AppState::status`]).
    Loaded(Result<LoadedWorkspace, String>),
}

/// Messages for the sidebar (tabs, filter, tree, history).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SidebarMsg {
    /// Switch between Collections and History.
    TabSelected(SidebarTab),
    /// Live filter text changed.
    FilterChanged(String),
    /// Expand/collapse a tree node.
    NodeToggled(PathBuf),
    /// A Request row was clicked (open intent).
    NodeActivated(PathBuf),
    /// A History row was clicked.
    HistoryEntrySelected(usize),
}

/// Messages that open CRUD modals or request duplicate/delete.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CrudMsg {
    /// Open the "New collection" modal.
    NewCollection,
    /// Open the "New folder" modal for `parent`.
    NewFolder(PathBuf),
    /// Open the "New request" modal for `parent`.
    NewRequest(PathBuf),
    /// Open the "Rename" modal for `path`.
    Rename(PathBuf),
    /// Duplicate `path` immediately (`<Name> copy`).
    Duplicate(PathBuf),
    /// Open the delete-confirmation modal for `path`.
    Delete(PathBuf),
}

/// Messages driving the modal overlay.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ModalMsg {
    /// Text input changed.
    TextChanged(String),
    /// Confirm (Enter / Confirm button): validates, then emits an Effect.
    Confirm,
    /// Cancel (Esc / Cancel button): closes without side effects.
    Cancel,
}

/// Top-level Message enum, nested for extensibility. The follow-up composer
/// task adds `Composer(..)` / `Tab(..)` variants here.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Message {
    /// Workspace bar messages.
    Workspace(WorkspaceMsg),
    /// Sidebar messages.
    Sidebar(SidebarMsg),
    /// Tree CRUD messages.
    Crud(CrudMsg),
    /// Modal overlay messages.
    Modal(ModalMsg),
}

/// What `update` asks the shell to do; the shell maps each to `iced::Task`s
/// (blocking storage work + workspace reload) and reports the outcome via
/// `Message::Workspace(WorkspaceMsg::Loaded(..))`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Effect {
    /// Load the Workspace from `path` (the shell also persists it as
    /// `last_workspace_path` via [`crate::config::AppConfig`]).
    LoadWorkspace(PathBuf),
    /// Open the OS folder picker.
    PickWorkspaceFolder,
    /// Create a Collection in the workspace root.
    CreateCollection { name: String },
    /// Create a Folder inside `parent`.
    CreateFolder { parent: PathBuf, name: String },
    /// Create a Request file inside `parent`.
    CreateRequest { parent: PathBuf, name: String },
    /// Rename the node at `path` (file AND internal name for requests).
    Rename { path: PathBuf, new_name: String },
    /// Duplicate the node at `path` under a non-colliding name.
    Duplicate { path: PathBuf },
    /// Delete the node at `path` (after the confirm modal).
    Delete { path: PathBuf },
    /// Show an informational line in the status strip.
    Notify(String),
}

/// A pending modal dialog.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Modal {
    /// Create a new Collection.
    NewCollection { name: String },
    /// Create a new Folder inside `parent`.
    NewFolder { parent: PathBuf, name: String },
    /// Create a new Request file inside `parent`.
    NewRequest { parent: PathBuf, name: String },
    /// Rename the node at `path`.
    Rename {
        path: PathBuf,
        kind: NodeKind,
        old_name: String,
        buffer: String,
    },
    /// Confirm deletion of the node at `path`.
    ConfirmDelete { path: PathBuf, kind: NodeKind },
}

/// The UI-agnostic application state.
#[derive(Debug, Clone, Default)]
pub struct AppState {
    /// Workspace root directory, when one is open.
    pub workspace_root: Option<PathBuf>,
    /// The loaded Workspace (disk truth), when loaded.
    pub workspace: Option<Workspace>,
    /// Active sidebar tab.
    pub sidebar_tab: SidebarTab,
    /// Live Collections filter text.
    pub filter: String,
    /// Expanded tree nodes (by path). Collapsed-by-default, except the
    /// seeded sample Collection on first load.
    pub expanded: HashSet<PathBuf>,
    /// Currently open modal, if any.
    pub modal: Option<Modal>,
    /// Last error/info line shown in the bottom status strip.
    pub status: Option<String>,
    /// Selected History row index.
    pub history_selected: Option<usize>,
    /// Last activated Request path — the follow-up composer task consumes
    /// this.
    pub last_opened: Option<PathBuf>,
    /// HTTP method per Request path (populated by `Loaded`).
    pub request_methods: BTreeMap<PathBuf, HttpMethod>,
}

impl AppState {
    /// A state with `workspace_root` set and everything else defaulted.
    pub fn new(workspace_root: Option<PathBuf>) -> Self {
        AppState {
            workspace_root,
            ..AppState::default()
        }
    }

    /// Pure reducer: processes `message` and returns the [`Effect`]s the
    /// shell should execute.
    pub fn update(&mut self, message: Message) -> Vec<Effect> {
        match message {
            Message::Workspace(message) => self.update_workspace(message),
            Message::Sidebar(message) => self.update_sidebar(message),
            Message::Crud(message) => self.update_crud(message),
            Message::Modal(message) => self.update_modal(message),
        }
    }

    fn update_workspace(&mut self, message: WorkspaceMsg) -> Vec<Effect> {
        match message {
            WorkspaceMsg::Reload => match self.workspace_root.clone() {
                Some(root) => vec![Effect::LoadWorkspace(root)],
                None => Vec::new(),
            },
            WorkspaceMsg::PickWorkspace => vec![Effect::PickWorkspaceFolder],
            WorkspaceMsg::PickedRoot(Ok(root)) => {
                self.workspace_root = Some(root.clone());
                self.workspace = None;
                self.expanded.clear();
                self.filter.clear();
                self.last_opened = None;
                self.history_selected = None;
                vec![Effect::LoadWorkspace(root)]
            }
            WorkspaceMsg::PickedRoot(Err(error)) => {
                self.status = Some(error);
                Vec::new()
            }
            WorkspaceMsg::Loaded(Ok(loaded)) => {
                self.apply_loaded(loaded);
                Vec::new()
            }
            WorkspaceMsg::Loaded(Err(error)) => {
                self.status = Some(error);
                Vec::new()
            }
        }
    }

    /// Fold a freshly loaded Workspace into state.
    fn apply_loaded(&mut self, loaded: LoadedWorkspace) {
        let LoadedWorkspace {
            workspace,
            request_methods,
        } = loaded;

        // First successful load: expand the seeded sample Collection (the
        // rest of the tree stays collapsed by default).
        if self.workspace.is_none() && self.expanded.is_empty() {
            if let Some(sample) = workspace
                .collections
                .iter()
                .find(|collection| collection.name == SAMPLE_COLLECTION_NAME)
            {
                self.expanded.insert(sample.path.clone());
            }
        }

        // Drop stale selections that no longer exist on disk.
        self.history_selected = self
            .history_selected
            .take()
            .filter(|index| *index < workspace.history.len());
        if self
            .last_opened
            .as_ref()
            .is_some_and(|path| !tree::contains_request(&workspace, path))
        {
            self.last_opened = None;
        }

        self.request_methods = request_methods;
        self.workspace = Some(workspace);
        self.status = None;
    }

    fn update_sidebar(&mut self, message: SidebarMsg) -> Vec<Effect> {
        match message {
            SidebarMsg::TabSelected(tab) => self.sidebar_tab = tab,
            SidebarMsg::FilterChanged(filter) => self.filter = filter,
            SidebarMsg::NodeToggled(path) => {
                // Toggle; `remove` returns true when it was present.
                if !self.expanded.remove(&path) {
                    self.expanded.insert(path);
                }
            }
            SidebarMsg::NodeActivated(path) => self.last_opened = Some(path),
            SidebarMsg::HistoryEntrySelected(index) => self.history_selected = Some(index),
        }
        Vec::new()
    }

    fn update_crud(&mut self, message: CrudMsg) -> Vec<Effect> {
        match message {
            CrudMsg::NewCollection => {
                self.modal = Some(Modal::NewCollection {
                    name: String::new(),
                });
            }
            CrudMsg::NewFolder(parent) => {
                self.expanded.insert(parent.clone());
                self.modal = Some(Modal::NewFolder {
                    parent,
                    name: String::new(),
                });
            }
            CrudMsg::NewRequest(parent) => {
                self.expanded.insert(parent.clone());
                self.modal = Some(Modal::NewRequest {
                    parent,
                    name: String::new(),
                });
            }
            CrudMsg::Rename(path) => {
                let (kind, old_name) = self.describe_node(&path);
                self.modal = Some(Modal::Rename {
                    path,
                    kind,
                    old_name: old_name.clone(),
                    buffer: old_name,
                });
            }
            CrudMsg::Duplicate(path) => return vec![Effect::Duplicate { path }],
            CrudMsg::Delete(path) => {
                let (kind, _) = self.describe_node(&path);
                self.modal = Some(Modal::ConfirmDelete { path, kind });
            }
        }
        Vec::new()
    }

    /// Look up the kind and display name of a node in the loaded workspace,
    /// falling back to path heuristics when the tree is not loaded.
    fn describe_node(&self, path: &Path) -> (NodeKind, String) {
        if let Some(workspace) = &self.workspace {
            if let Some((kind, name)) = tree::find(workspace, path) {
                return (kind, name);
            }
        }
        // Fallback when the tree is not available.
        let kind = if path.extension().is_some_and(|ext| ext == "yaml") {
            NodeKind::Request
        } else {
            NodeKind::Folder
        };
        let name = match kind {
            NodeKind::Request => path
                .file_stem()
                .and_then(|stem| stem.to_str())
                .unwrap_or_default()
                .to_string(),
            _ => path
                .file_name()
                .and_then(|name| name.to_str())
                .unwrap_or_default()
                .to_string(),
        };
        (kind, name)
    }

    fn update_modal(&mut self, message: ModalMsg) -> Vec<Effect> {
        match message {
            ModalMsg::TextChanged(text) => match &mut self.modal {
                Some(Modal::NewCollection { name }) => *name = text,
                Some(Modal::NewFolder { name, .. }) => *name = text,
                Some(Modal::NewRequest { name, .. }) => *name = text,
                Some(Modal::Rename { buffer, .. }) => *buffer = text,
                Some(Modal::ConfirmDelete { .. }) | None => {}
            },
            ModalMsg::Cancel => self.modal = None,
            ModalMsg::Confirm => return self.confirm_modal(),
        }
        Vec::new()
    }

    /// Validate the open modal and emit the matching Effect. Validation
    /// failures are reported into `status` (the modal stays open so the
    /// user can correct the input); success closes the modal.
    fn confirm_modal(&mut self) -> Vec<Effect> {
        let modal = self.modal.clone();
        match modal {
            None => Vec::new(),
            Some(Modal::NewCollection { name }) => {
                let name = trimmed(name);
                match self.validate_collection_name(&name) {
                    Ok(()) => {
                        self.modal = None;
                        vec![Effect::CreateCollection { name }]
                    }
                    Err(error) => {
                        self.status = Some(error);
                        Vec::new()
                    }
                }
            }
            Some(Modal::NewFolder { parent, name }) => {
                let name = trimmed(name);
                match self.validate_folder_name(&parent, &name) {
                    Ok(()) => {
                        self.modal = None;
                        vec![Effect::CreateFolder { parent, name }]
                    }
                    Err(error) => {
                        self.status = Some(error);
                        Vec::new()
                    }
                }
            }
            Some(Modal::NewRequest { parent, name }) => {
                let name = trimmed(name);
                match self.validate_request_name(&parent, &name) {
                    Ok(()) => {
                        self.modal = None;
                        vec![Effect::CreateRequest { parent, name }]
                    }
                    Err(error) => {
                        self.status = Some(error);
                        Vec::new()
                    }
                }
            }
            Some(Modal::Rename {
                path,
                kind,
                old_name,
                buffer,
            }) => {
                let new_name = trimmed(buffer);
                if new_name.is_empty() {
                    self.status = Some("Name cannot be empty.".to_string());
                } else if new_name == old_name {
                    // No-op rename: just close.
                    self.modal = None;
                } else {
                    match self.validate_sibling_name(&path, &new_name) {
                        Ok(()) => {
                            self.modal = None;
                            self.remap_renamed_paths(&path, kind, &new_name);
                            return vec![Effect::Rename { path, new_name }];
                        }
                        Err(error) => self.status = Some(error),
                    }
                }
                Vec::new()
            }
            Some(Modal::ConfirmDelete { path, .. }) => {
                self.modal = None;
                vec![Effect::Delete { path }]
            }
        }
    }

    fn validate_collection_name(&self, name: &str) -> Result<(), String> {
        validate_name(name)?;
        if let Some(workspace) = &self.workspace {
            let exists = workspace
                .collections
                .iter()
                .any(|collection| sanitized(collection.name.as_str()) == sanitized(name));
            if exists {
                return Err(format!("A collection named \"{name}\" already exists."));
            }
        }
        Ok(())
    }

    fn validate_folder_name(&self, parent: &Path, name: &str) -> Result<(), String> {
        validate_name(name)?;
        if let Some(workspace) = &self.workspace {
            let exists = tree::folder_names_under(workspace, parent)
                .iter()
                .any(|existing| sanitized(existing) == sanitized(name));
            if exists {
                return Err(format!("A folder named \"{name}\" already exists."));
            }
        }
        Ok(())
    }

    fn validate_request_name(&self, parent: &Path, name: &str) -> Result<(), String> {
        validate_name(name)?;
        if let Some(workspace) = &self.workspace {
            let exists = tree::request_names_in(workspace, parent)
                .iter()
                .any(|existing| sanitized(existing) == sanitized(name));
            if exists {
                return Err(format!("A request named \"{name}\" already exists."));
            }
        }
        Ok(())
    }

    /// Duplicate check for renames: the new name must not collide with any
    /// same-kind sibling of `path`.
    fn validate_sibling_name(&self, path: &Path, name: &str) -> Result<(), String> {
        validate_name(name)?;
        let (kind, _) = self.describe_node(path);
        let parent = match path.parent() {
            Some(parent) => parent.to_path_buf(),
            None => return Ok(()),
        };
        match kind {
            NodeKind::Request => self.validate_request_name(&parent, name),
            NodeKind::Folder | NodeKind::Collection => self.validate_folder_name(&parent, name),
        }
    }

    /// After a successful rename, remap `expanded` and `last_opened` keys
    /// that point at the renamed node or its descendants, so the tree state
    /// survives the rename.
    fn remap_renamed_paths(&mut self, old_path: &Path, kind: NodeKind, new_name: &str) {
        let Ok(sanitized) = sanitize_filename(new_name) else {
            return;
        };
        let Some(parent) = old_path.parent() else {
            return;
        };
        let new_path = match kind {
            NodeKind::Request => parent.join(format!("{sanitized}.yaml")),
            NodeKind::Folder | NodeKind::Collection => parent.join(&sanitized),
        };
        if old_path == new_path {
            return;
        }

        let remap = |path: &Path| -> PathBuf {
            if path == old_path {
                return new_path.clone();
            }
            match path.strip_prefix(old_path) {
                // Descendant of the renamed node.
                Ok(relative) => new_path.join(relative),
                // Unrelated path.
                Err(_) => path.to_path_buf(),
            }
        };

        self.expanded = self
            .expanded
            .iter()
            .map(|path| {
                if path.starts_with(old_path) {
                    remap(path)
                } else {
                    path.clone()
                }
            })
            .collect();
        if self
            .last_opened
            .as_ref()
            .is_some_and(|path| path.starts_with(old_path))
        {
            if let Some(path) = &self.last_opened {
                self.last_opened = Some(remap(path));
            }
        }
    }
}

/// Trim surrounding whitespace; empty means invalid.
fn trimmed(text: String) -> String {
    text.trim().to_string()
}

/// Shared first-line validation for any modal name.
fn validate_name(name: &str) -> Result<(), String> {
    if name.is_empty() {
        return Err("Name cannot be empty.".to_string());
    }
    sanitize_filename(name)
        .map_err(|error| error.to_string())
        .map(|_| ())
}

/// Case-sensitive sanitized-name comparison (Linux filenames are
/// case-sensitive, so this is the real collision test).
fn sanitized(name: &str) -> String {
    sanitize_filename(name).unwrap_or_else(|_| name.to_string())
}

/// The default Request the shell creates for `Effect::CreateRequest` (the
/// composer task fills in method/URL afterwards).
pub fn new_request(name: &str) -> Request {
    Request {
        name: name.to_string(),
        ..Request::default()
    }
}
