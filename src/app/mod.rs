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

pub mod composer;
pub mod tree;

pub use self::composer::{
    BodyEdit, BodyMode, BodyMotion, BodyView, ComposerMsg, EnvScope, EnvSelection, JsonToken,
    JsonTokenKind, NameValue, PendingSave, ResponseState, ResponseView, StatusClass, SubTab, Tab,
    TabKind, TabMsg,
};

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
    /// A Request row was clicked (open intent; the shell reads the file
    /// and replies `Tab(TabMsg::Opened { .. })`).
    NodeActivated(PathBuf),
    /// A History row was clicked (also opens a HistoryDraft tab).
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

/// Which section of an editor modal a row-level [`ModalMsg`] targets.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ModalSlot {
    /// Variables of the edited Environment.
    EnvironmentVars,
    /// Shared Collection secrets.
    SharedSecrets,
    /// Per-Environment secret overrides.
    EnvironmentSecrets,
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
    /// `CloseDirty` modal: discard the changes and close the tab.
    Discard,
    /// `SaveDraft` modal: the target Collection was picked (index into the
    /// modal's collection list).
    TargetCollectionSelected(usize),
    /// `SaveDraft` modal: the nested folder path text changed ("a/b").
    FolderChanged(String),
    /// The modal's name field changed (`SaveDraft` request name,
    /// `EditEnvironments` new-environment name).
    NameChanged(String),
    /// `EditEnvironments` modal: another environment was picked to edit
    /// (index into the modal's env list; `envs.len()` = "New
    /// environment…").
    ItemSelected(usize),
    /// Append an empty row to the section.
    RowAdded(ModalSlot),
    /// Remove the row at the index from the section.
    RowRemoved(ModalSlot, usize),
    /// Change a row's name/key in the section.
    RowKeyChanged(ModalSlot, usize, String),
    /// Change a row's value in the section.
    RowValueChanged(ModalSlot, usize, String),
}

/// Top-level Message enum, nested for extensibility.
///
/// `Eq` is not derivable: `Composer(..)` carries [`ComposerMsg::BodyEdit`]
/// with `f32` coordinates.
#[derive(Debug, Clone, PartialEq)]
pub enum Message {
    /// Workspace bar messages.
    Workspace(WorkspaceMsg),
    /// Sidebar messages.
    Sidebar(SidebarMsg),
    /// Tree CRUD messages.
    Crud(CrudMsg),
    /// Modal overlay messages.
    Modal(ModalMsg),
    /// Tab strip messages (open/close/focus tabs).
    Tab(TabMsg),
    /// Request composer messages (edits, send pipeline, save).
    Composer(ComposerMsg),
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
    /// Read the Request file at `path` off-thread; the shell replies
    /// `Message::Tab(TabMsg::Opened { .. })`.
    OpenRequest { path: PathBuf },
    /// Execute a prepared request for `tab_id` (the shell owns the
    /// in-flight registry and its cancellation tokens).
    SendRequest {
        /// The sending tab.
        tab_id: u64,
        /// The fully resolved request.
        prepared: crate::interpolation::PreparedHttpRequest,
    },
    /// Cancel the in-flight request of `tab_id` (its completion message is
    /// still delivered by the select! arm).
    CancelSend { tab_id: u64 },
    /// Upsert the Request at `path` (a Saved tab's save).
    SaveRequest {
        /// Path of the request file.
        path: PathBuf,
        /// A copy of the current request.
        request: Request,
    },
    /// Create a Request file (must NOT overwrite; draft save).
    SaveDraft {
        /// Target parent directory (Collection or nested Folder).
        parent: PathBuf,
        /// The request to create.
        request: Request,
    },
    /// Append a History Entry (the redacting history store persists it
    /// with `secret_values`), then reload the workspace.
    AppendHistory {
        /// The exchange to append (unredacted; redaction happens on write).
        entry: crate::domain::HistoryEntry,
        /// The tab's non-empty secret values.
        secret_values: Vec<String>,
    },
    /// Write text to the system clipboard.
    CopyText(String),
    /// Persist an Environment of the Collection at `collection_dir`.
    SaveEnvironment {
        /// The Collection directory.
        collection_dir: PathBuf,
        /// The environment to save.
        environment: crate::domain::Environment,
    },
    /// Persist the Collection's Secret store at `collection_dir` (always
    /// `.secrets.yaml` — never anywhere else).
    SaveSecrets {
        /// The Collection directory.
        collection_dir: PathBuf,
        /// The store to save.
        secrets: crate::domain::SecretStore,
    },
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
    /// Save a Draft tab as a Request: target Collection + nested folder
    /// path ("a/b") + request name.
    SaveDraft {
        /// The draft tab being saved (promoted on the next `Loaded(Ok)`).
        tab_id: u64,
        /// Snapshot of the draft's request (name filled on confirm).
        request: Request,
        /// Candidate target Collection directories.
        collections: Vec<PathBuf>,
        /// Index into `collections` of the picked target.
        target: usize,
        /// Nested folder path text ("a/b", optional).
        folder: String,
        /// Request name buffer.
        name: String,
    },
    /// A dirty tab is being closed: Save / Discard / Cancel.
    CloseDirty { tab_id: u64 },
    /// Edit the Environments of a Collection: pick the environment (or
    /// "New environment…") and edit its variable rows.
    EditEnvironments {
        /// The Collection directory.
        collection_dir: PathBuf,
        /// Existing environment names.
        envs: Vec<String>,
        /// Index into `envs`; `envs.len()` means "New environment…".
        selected: usize,
        /// Name buffer for a new environment.
        new_name: String,
        /// Editable variable rows.
        rows: Vec<NameValue>,
    },
    /// Edit a Collection's Secrets: shared secrets plus, when an
    /// environment is selected, its per-environment overrides. Persisted
    /// ONLY to `.secrets.yaml`.
    EditSecrets {
        /// The Collection directory.
        collection_dir: PathBuf,
        /// The environment whose overrides are edited, when any.
        env_name: Option<String>,
        /// Editable shared-secret rows.
        shared_rows: Vec<NameValue>,
        /// Editable per-environment secret rows.
        env_rows: Vec<NameValue>,
    },
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
    /// Last activated Request path (the shell reads it and replies
    /// `Tab(TabMsg::Opened { .. })`).
    pub last_opened: Option<PathBuf>,
    /// HTTP method per Request path (populated by `Loaded`).
    pub request_methods: BTreeMap<PathBuf, HttpMethod>,
    /// Open composer tabs (see [`composer`]).
    pub tabs: Vec<Tab>,
    /// The focused tab id.
    pub active_tab: Option<u64>,
    /// Monotonic tab id counter.
    pub next_tab_id: u64,
    /// The draft being saved, converted to `Saved` on the next
    /// successful reload.
    pub pending_save: Option<PendingSave>,
    /// Tab ids whose dirty flag clears on the next successful reload
    /// (saved via `Effect::SaveRequest`).
    pub pending_saved: Vec<u64>,
    /// The tab to close on the next successful reload (save-and-close).
    pub pending_close: Option<u64>,
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
            Message::Tab(message) => self.update_tab(message),
            Message::Composer(message) => self.update_composer(message),
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
                // Tab references point into the old workspace; drop them.
                self.tabs.clear();
                self.active_tab = None;
                self.pending_save = None;
                self.pending_saved.clear();
                self.pending_close = None;
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
        self.workspace = Some(workspace.clone());
        self.status = None;

        // Reconcile composer tabs with disk truth (pending save/close
        // markers, vanished request files).
        self.apply_composer_reload(&workspace);
    }

    fn update_sidebar(&mut self, message: SidebarMsg) -> Vec<Effect> {
        match message {
            SidebarMsg::TabSelected(tab) => {
                self.sidebar_tab = tab;
                Vec::new()
            }
            SidebarMsg::FilterChanged(filter) => {
                self.filter = filter;
                Vec::new()
            }
            SidebarMsg::NodeToggled(path) => {
                // Toggle; `remove` returns true when it was present.
                if !self.expanded.remove(&path) {
                    self.expanded.insert(path);
                }
                Vec::new()
            }
            SidebarMsg::NodeActivated(path) => {
                self.last_opened = Some(path.clone());
                vec![Effect::OpenRequest { path }]
            }
            SidebarMsg::HistoryEntrySelected(index) => {
                self.history_selected = Some(index);
                // Open a HistoryDraft tab immediately, with no I/O: the
                // entry's request/response snapshots prefill the tab.
                self.open_history_entry(index);
                Vec::new()
            }
        }
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
                Some(Modal::SaveDraft { name, .. }) => *name = text,
                Some(Modal::ConfirmDelete { .. })
                | Some(Modal::CloseDirty { .. })
                | Some(Modal::EditEnvironments { .. })
                | Some(Modal::EditSecrets { .. })
                | None => {}
            },
            ModalMsg::Cancel => {
                // Cancelling the save-draft modal abandons any pending
                // close armed by the CloseDirty "Save" route (the tab
                // stays open, still dirty).
                if let Some(Modal::SaveDraft { tab_id, .. }) = self.modal.take() {
                    if self.pending_close == Some(tab_id) {
                        self.pending_close = None;
                    }
                }
            }
            ModalMsg::Discard => {
                // CloseDirty only: drop the changes and close the tab.
                if let Some(Modal::CloseDirty { tab_id }) = self.modal.take() {
                    self.close_tab(tab_id);
                }
            }
            ModalMsg::TargetCollectionSelected(index) => {
                if let Some(Modal::SaveDraft { target, .. }) = &mut self.modal {
                    *target = index;
                }
            }
            ModalMsg::FolderChanged(folder) => {
                if let Some(Modal::SaveDraft { folder: target, .. }) = &mut self.modal {
                    *target = folder;
                }
            }
            ModalMsg::NameChanged(name) => match &mut self.modal {
                Some(Modal::SaveDraft { name: buffer, .. }) => *buffer = name,
                Some(Modal::EditEnvironments { new_name, .. }) => *new_name = name,
                _ => {}
            },
            ModalMsg::ItemSelected(index) => {
                if let Some(Modal::EditEnvironments {
                    collection_dir,
                    envs,
                    selected,
                    new_name,
                    rows,
                }) = &mut self.modal
                {
                    let bounded = index.min(envs.len());
                    *selected = bounded;
                    *new_name = String::new();
                    // Refresh the rows from the freshly loaded workspace.
                    if let Some(collection) = self.workspace.as_ref().and_then(|workspace| {
                        workspace
                            .collections
                            .iter()
                            .find(|collection| collection.path == *collection_dir)
                    }) {
                        *rows = if bounded < envs.len() {
                            collection
                                .environments
                                .iter()
                                .find(|env| env.name == envs[bounded])
                                .map(|env| {
                                    env.variables
                                        .iter()
                                        .map(|(name, value)| NameValue {
                                            name: name.clone(),
                                            value: value.clone(),
                                        })
                                        .collect()
                                })
                                .unwrap_or_default()
                        } else {
                            Vec::new()
                        };
                    }
                }
            }
            ModalMsg::RowAdded(slot) => match (slot, &mut self.modal) {
                (ModalSlot::EnvironmentVars, Some(Modal::EditEnvironments { rows, .. })) => {
                    rows.push(NameValue::default());
                }
                (ModalSlot::SharedSecrets, Some(Modal::EditSecrets { shared_rows, .. })) => {
                    shared_rows.push(NameValue::default());
                }
                (ModalSlot::EnvironmentSecrets, Some(Modal::EditSecrets { env_rows, .. })) => {
                    env_rows.push(NameValue::default());
                }
                _ => {}
            },
            ModalMsg::RowRemoved(slot, index) => match (slot, &mut self.modal) {
                (ModalSlot::EnvironmentVars, Some(Modal::EditEnvironments { rows, .. })) => {
                    if index < rows.len() {
                        rows.remove(index);
                    }
                }
                (ModalSlot::SharedSecrets, Some(Modal::EditSecrets { shared_rows, .. })) => {
                    if index < shared_rows.len() {
                        shared_rows.remove(index);
                    }
                }
                (ModalSlot::EnvironmentSecrets, Some(Modal::EditSecrets { env_rows, .. }))
                    if index < env_rows.len() =>
                {
                    env_rows.remove(index);
                }
                _ => {}
            },
            ModalMsg::RowKeyChanged(slot, index, name) => match (slot, &mut self.modal) {
                (ModalSlot::EnvironmentVars, Some(Modal::EditEnvironments { rows, .. })) => {
                    if let Some(row) = rows.get_mut(index) {
                        row.name = name;
                    }
                }
                (ModalSlot::SharedSecrets, Some(Modal::EditSecrets { shared_rows, .. })) => {
                    if let Some(row) = shared_rows.get_mut(index) {
                        row.name = name;
                    }
                }
                (ModalSlot::EnvironmentSecrets, Some(Modal::EditSecrets { env_rows, .. })) => {
                    if let Some(row) = env_rows.get_mut(index) {
                        row.name = name;
                    }
                }
                _ => {}
            },
            ModalMsg::RowValueChanged(slot, index, value) => match (slot, &mut self.modal) {
                (ModalSlot::EnvironmentVars, Some(Modal::EditEnvironments { rows, .. })) => {
                    if let Some(row) = rows.get_mut(index) {
                        row.value = value;
                    }
                }
                (ModalSlot::SharedSecrets, Some(Modal::EditSecrets { shared_rows, .. })) => {
                    if let Some(row) = shared_rows.get_mut(index) {
                        row.value = value;
                    }
                }
                (ModalSlot::EnvironmentSecrets, Some(Modal::EditSecrets { env_rows, .. })) => {
                    if let Some(row) = env_rows.get_mut(index) {
                        row.value = value;
                    }
                }
                _ => {}
            },
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
                // Tab references do not survive the deletion: close every
                // Saved tab at or under the deleted path.
                self.close_tabs_under(&path);
                vec![Effect::Delete { path }]
            }
            Some(Modal::SaveDraft { .. }) => self.confirm_save_draft(),
            Some(Modal::CloseDirty { tab_id }) => self.confirm_close_dirty(tab_id),
            Some(Modal::EditEnvironments { .. }) => self.confirm_edit_environments(),
            Some(Modal::EditSecrets { .. }) => self.confirm_edit_secrets(),
        }
    }

    /// The CloseDirty modal's Save action:
    /// - Saved tab: emit `Effect::SaveRequest`, arm `pending_close` so the
    ///   tab closes on the success reload.
    /// - Draft tab: route to the save-draft modal (`pending_close` stays
    ///   armed, so the tab closes after promotion).
    /// - HistoryDraft: never saves — a no-op (the card hides the button).
    fn confirm_close_dirty(&mut self, tab_id: u64) -> Vec<Effect> {
        let Some(tab) = self.tabs.iter().find(|tab| tab.id == tab_id) else {
            self.modal = None;
            return Vec::new();
        };
        match &tab.kind {
            TabKind::Saved { path } => {
                self.modal = None;
                self.pending_saved.push(tab_id);
                self.pending_close = Some(tab_id);
                vec![Effect::SaveRequest {
                    path: path.clone(),
                    request: tab.request.clone(),
                }]
            }
            TabKind::Draft => {
                // Route to the save-draft modal; `pending_close` is armed
                // now so the tab closes on the reload following a
                // successful promotion (cancelling the modal disarms it).
                self.pending_close = Some(tab_id);
                self.open_save_draft_modal(tab_id);
                Vec::new()
            }
            TabKind::HistoryDraft => Vec::new(),
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

        // Saved-tab references must survive the rename too (the request
        // file's name field is rewritten by the storage layer when the
        // file itself was renamed).
        for tab in &mut self.tabs {
            let TabKind::Saved { path } = &mut tab.kind else {
                continue;
            };
            if path == old_path {
                if kind == NodeKind::Request {
                    tab.request.name = new_name.to_string();
                }
                *path = new_path.clone();
            } else if path.starts_with(old_path) {
                *path = remap(path);
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
