//! Reducer + shell-equivalent tests for the GUI app core.
//!
//! These drive [`AppState::update`] and execute the returned [`Effect`]s
//! against a real tempdir via [`FsWorkspaceStorage`] exactly like the iced
//! shell would, then feed the reload result back in via
//! `Message::Workspace(WorkspaceMsg::Loaded(..))`.

use rest_harness::app::{
    new_request, tree, AppState, CrudMsg, Effect, LoadedWorkspace, Message, Modal, ModalMsg,
    NodeKind, SidebarMsg, SidebarTab, TabMsg, WorkspaceMsg,
};
use rest_harness::domain::{HistoryEntry, HttpHeader, HttpMethod, RedactedHttpRequest, Request};
use rest_harness::history::{build_entry, HistoryStore};
use rest_harness::storage::{read_request, FsWorkspaceStorage, WorkspaceStorage};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

/// Executes effects like the iced shell would (pure reducer + real storage).
struct Harness {
    state: AppState,
    root: PathBuf,
    executed: Vec<Effect>,
}

impl Harness {
    fn new(root: PathBuf) -> Self {
        Harness {
            state: AppState::new(Some(root.clone())),
            root,
            executed: Vec::new(),
        }
    }

    /// Process a message, executing every resulting effect (like the shell).
    fn run(&mut self, message: Message) {
        for effect in self.state.update(message) {
            self.execute(effect);
        }
    }

    /// Process a message but only return its effects (no execution).
    fn handle(&mut self, message: Message) -> Vec<Effect> {
        self.state.update(message)
    }

    /// Execute one effect against the real filesystem and feed the reload
    /// result back into the reducer (mirrors `gui::execute_effect`).
    fn execute(&mut self, effect: Effect) {
        self.executed.push(effect.clone());
        let storage = FsWorkspaceStorage;
        match effect {
            Effect::LoadWorkspace(root) => {
                let workspace = storage.load_workspace(&root).unwrap();
                let loaded = LoadedWorkspace {
                    workspace,
                    request_methods: request_methods_of(&self.root),
                };
                self.state
                    .update(Message::Workspace(WorkspaceMsg::Loaded(Ok(loaded))));
            }
            Effect::CreateCollection { name } => {
                storage.create_collection(&self.root, &name).unwrap();
                self.reload();
            }
            Effect::CreateFolder { parent, name } => {
                storage.create_folder(&parent, &name).unwrap();
                self.reload();
            }
            Effect::CreateRequest { parent, name } => {
                storage
                    .create_request(&parent, &new_request(&name))
                    .unwrap();
                self.reload();
            }
            Effect::Rename { path, new_name } => {
                if path.is_file() {
                    storage.rename_request(&path, &new_name).unwrap();
                } else {
                    storage.rename_folder(&path, &new_name).unwrap();
                }
                self.reload();
            }
            Effect::Duplicate { path } => {
                if path.is_file() {
                    storage.duplicate_request(&path).unwrap();
                } else {
                    storage.duplicate_folder(&path).unwrap();
                }
                self.reload();
            }
            Effect::Delete { path } => {
                if path.is_file() {
                    storage.delete_request(&path).unwrap();
                } else {
                    storage.delete_folder(&path).unwrap();
                }
                self.reload();
            }
            Effect::OpenRequest { path } => {
                // The shell reads the request off-thread and replies
                // `Tab(TabMsg::Opened { .. })`.
                let request = read_request(&path).map_err(|error| error.to_string());
                self.state.update(Message::Tab(TabMsg::Opened {
                    path: path.clone(),
                    request,
                }));
            }
            Effect::SaveRequest { path, request } => {
                let parent = path.parent().map(Path::to_path_buf).unwrap_or_default();
                storage.save_request(&parent, &request).unwrap();
                self.reload();
            }
            Effect::SaveDraft { parent, request } => {
                storage.create_request(&parent, &request).unwrap();
                self.reload();
            }
            Effect::AppendHistory {
                entry,
                secret_values,
            } => {
                HistoryStore::new(&self.root)
                    .append_entry(entry, &secret_values)
                    .unwrap();
                self.reload();
            }
            Effect::SaveEnvironment {
                collection_dir,
                environment,
            } => {
                storage
                    .save_environment(&collection_dir, &environment)
                    .unwrap();
                self.reload();
            }
            Effect::SaveSecrets {
                collection_dir,
                secrets,
            } => {
                storage.save_secrets(&collection_dir, &secrets).unwrap();
                self.reload();
            }
            // Shell-only effects (network, clipboard, cancellation): not
            // exercised by the reducer tests.
            Effect::SendRequest { .. } | Effect::CancelSend { .. } | Effect::CopyText(_) => {}
            Effect::PickWorkspaceFolder | Effect::Notify(_) => {}
        }
    }

    fn reload(&mut self) {
        self.execute(Effect::LoadWorkspace(self.root.clone()));
    }

    fn workspace(&self) -> &rest_harness::domain::Workspace {
        self.state.workspace.as_ref().expect("workspace loaded")
    }

    fn collection(&self, name: &str) -> &rest_harness::domain::Collection {
        self.workspace()
            .collections
            .iter()
            .find(|collection| collection.name == name)
            .expect("collection exists")
    }
}

/// Parse every request file under `root` into a method map (like the shell).
fn request_methods_of(root: &Path) -> BTreeMap<PathBuf, HttpMethod> {
    let workspace = FsWorkspaceStorage.load_workspace(root).unwrap();
    let mut methods = BTreeMap::new();
    let push = |requests: &[rest_harness::domain::RequestEntry],
                methods: &mut BTreeMap<PathBuf, HttpMethod>| {
        for entry in requests {
            if let Ok(request) = read_request(&entry.path) {
                methods.insert(entry.path.clone(), request.method);
            }
        }
    };
    for collection in &workspace.collections {
        push(&collection.requests, &mut methods);
        let mut stack: Vec<&rest_harness::domain::Folder> = collection.folders.iter().collect();
        while let Some(folder) = stack.pop() {
            push(&folder.requests, &mut methods);
            stack.extend(folder.folders.iter());
        }
    }
    methods
}

/// Seed a workspace with a nested tree:
/// `API/Users/Admin/Delete user.yaml`, `API/Get health.yaml`.
fn seed_workspace(root: &Path) -> (PathBuf, PathBuf, PathBuf, PathBuf) {
    let storage = FsWorkspaceStorage;
    let collection = storage.create_collection(root, "API").unwrap();
    let users = storage.create_folder(&collection.path, "Users").unwrap();
    let admin = storage.create_folder(&users.path, "Admin").unwrap();

    let delete_user = Request {
        name: "Delete user".to_string(),
        method: HttpMethod::Delete,
        url: "https://example.com/users/1".to_string(),
        ..new_request("")
    };
    storage.create_request(&admin.path, &delete_user).unwrap();

    let health = Request {
        name: "Get health".to_string(),
        url: "https://example.com/health".to_string(),
        ..new_request("")
    };
    storage.create_request(&collection.path, &health).unwrap();

    (
        collection.path,
        users.path,
        admin.path.clone(),
        admin.path.join("Delete user.yaml"),
    )
}

/// Seed the gitignored history file with one entry.
fn seed_history(root: &Path) {
    let entry: HistoryEntry = build_entry(
        RedactedHttpRequest {
            method: HttpMethod::Post,
            url: "https://example.com/orders".to_string(),
            headers: vec![HttpHeader {
                name: "authorization".to_string(),
                value: "***".to_string(),
            }],
            body: "{}".to_string(),
        },
        201,
        "Created".to_string(),
        42,
        128,
        Vec::new(),
        "{}".to_string(),
    );
    HistoryStore::new(root).append_entry(entry, &[]).unwrap();
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[test]
fn initial_load_produces_load_workspace_effect_and_populates_tree() {
    let dir = tempfile::tempdir().unwrap();
    rest_harness::config::seed_sample_collection(dir.path()).unwrap();

    let mut harness = Harness::new(dir.path().to_path_buf());

    let effects = harness.handle(Message::Workspace(WorkspaceMsg::Reload));
    assert_eq!(
        effects,
        vec![Effect::LoadWorkspace(dir.path().to_path_buf())]
    );
    for effect in effects {
        harness.execute(effect);
    }

    let workspace = harness.workspace();
    assert_eq!(workspace.root, dir.path());
    assert_eq!(workspace.collections.len(), 1);
    assert_eq!(workspace.collections[0].name, "My public collection");
    assert_eq!(workspace.collections[0].total_request_count(), 1);

    // The seeded sample collection is expanded on first load.
    assert!(
        harness
            .state
            .expanded
            .contains(&workspace.collections[0].path),
        "sample collection should be expanded by default"
    );
}

#[test]
fn filter_hides_non_matching_branches_and_force_expands_ancestors() {
    let dir = tempfile::tempdir().unwrap();
    let (collection, users, _admin, delete_user) = seed_workspace(dir.path());
    let mut harness = Harness::new(dir.path().to_path_buf());
    harness.run(Message::Workspace(WorkspaceMsg::Reload));

    // Deep match: ancestors force-expanded, siblings hidden.
    harness
        .state
        .update(Message::Sidebar(SidebarMsg::FilterChanged(
            "delete user".to_string(),
        )));
    let nodes = tree::build_tree(harness.workspace(), &harness.state.filter);
    assert_eq!(nodes.len(), 1);
    let api = &nodes[0];
    assert!(api.force_expanded);
    assert_eq!(api.children.len(), 1, "Get health is hidden");
    let users_node = &api.children[0];
    assert_eq!(users_node.path, users);
    assert!(users_node.force_expanded);
    let admin_node = &users_node.children[0];
    assert!(admin_node.force_expanded);
    assert_eq!(admin_node.children[0].path, delete_user);

    // Case-insensitive collection-name match: non-matching children hidden.
    harness
        .state
        .update(Message::Sidebar(SidebarMsg::FilterChanged(
            "API".to_string(),
        )));
    let nodes = tree::build_tree(harness.workspace(), &harness.state.filter);
    assert_eq!(nodes.len(), 1);
    assert_eq!(nodes[0].path, collection);
    assert!(nodes[0].children.is_empty());
    assert!(!nodes[0].force_expanded);

    // Clearing the filter restores the full tree.
    harness
        .state
        .update(Message::Sidebar(SidebarMsg::FilterChanged(String::new())));
    let nodes = tree::build_tree(harness.workspace(), &harness.state.filter);
    assert_eq!(nodes[0].children.len(), 2);
}

#[test]
fn expand_and_collapse_toggles() {
    let dir = tempfile::tempdir().unwrap();
    let (collection, _users, _admin, _delete_user) = seed_workspace(dir.path());
    let mut harness = Harness::new(dir.path().to_path_buf());
    harness.run(Message::Workspace(WorkspaceMsg::Reload));

    assert!(!harness.state.expanded.contains(&collection));

    harness.run(Message::Sidebar(SidebarMsg::NodeToggled(
        collection.clone(),
    )));
    assert!(harness.state.expanded.contains(&collection));

    harness.run(Message::Sidebar(SidebarMsg::NodeToggled(
        collection.clone(),
    )));
    assert!(!harness.state.expanded.contains(&collection));

    harness.run(Message::Sidebar(SidebarMsg::NodeToggled(
        collection.clone(),
    )));
    assert!(harness.state.expanded.contains(&collection));
}

#[test]
fn modal_confirm_creates_collection_folder_and_request_on_disk() {
    let dir = tempfile::tempdir().unwrap();
    let (collection, _users, _admin, _delete_user) = seed_workspace(dir.path());
    let mut harness = Harness::new(dir.path().to_path_buf());
    harness.run(Message::Workspace(WorkspaceMsg::Reload));

    // New collection.
    harness.run(Message::Crud(CrudMsg::NewCollection));
    assert_eq!(
        harness.state.modal,
        Some(Modal::NewCollection {
            name: String::new()
        })
    );
    harness.run(Message::Modal(ModalMsg::TextChanged("Billing".into())));
    let effects = harness.handle(Message::Modal(ModalMsg::Confirm));
    assert_eq!(
        effects,
        vec![Effect::CreateCollection {
            name: "Billing".to_string()
        }]
    );
    assert_eq!(harness.state.modal, None);
    for effect in effects {
        harness.execute(effect);
    }
    assert!(dir.path().join("Billing").is_dir());
    assert!(dir.path().join("Billing").join("environments").is_dir());
    assert!(harness
        .workspace()
        .collections
        .iter()
        .any(|c| c.name == "Billing"));

    // New folder inside the API collection.
    harness.run(Message::Crud(CrudMsg::NewFolder(collection.clone())));
    harness.run(Message::Modal(ModalMsg::TextChanged("Orders".into())));
    let effects = harness.handle(Message::Modal(ModalMsg::Confirm));
    assert_eq!(
        effects,
        vec![Effect::CreateFolder {
            parent: collection.clone(),
            name: "Orders".to_string()
        }]
    );
    for effect in effects {
        harness.execute(effect);
    }
    let orders = collection.join("Orders");
    assert!(orders.is_dir());
    assert!(harness
        .collection("API")
        .folders
        .iter()
        .any(|f| f.name == "Orders"));

    // New request inside the new folder.
    harness.run(Message::Crud(CrudMsg::NewRequest(orders.clone())));
    harness.run(Message::Modal(ModalMsg::TextChanged("List orders".into())));
    let effects = harness.handle(Message::Modal(ModalMsg::Confirm));
    assert_eq!(
        effects,
        vec![Effect::CreateRequest {
            parent: orders.clone(),
            name: "List orders".to_string()
        }]
    );
    for effect in effects {
        harness.execute(effect);
    }
    let list_orders = orders.join("List orders.yaml");
    assert!(list_orders.is_file());
    let request = read_request(&list_orders).unwrap();
    assert_eq!(request.name, "List orders");
    let api = harness.collection("API");
    let orders_folder = api.folders.iter().find(|f| f.name == "Orders").unwrap();
    assert!(orders_folder.requests.iter().any(|r| r.path == list_orders));
}

#[test]
fn rename_rewrites_disk_and_remaps_expanded_and_last_opened() {
    let dir = tempfile::tempdir().unwrap();
    let (_collection, users, admin, delete_user) = seed_workspace(dir.path());
    let mut harness = Harness::new(dir.path().to_path_buf());
    harness.run(Message::Workspace(WorkspaceMsg::Reload));

    // Expand the tree and activate the request so both maps have entries.
    harness.run(Message::Sidebar(SidebarMsg::NodeToggled(users.clone())));
    harness.run(Message::Sidebar(SidebarMsg::NodeToggled(admin.clone())));
    harness.run(Message::Sidebar(SidebarMsg::NodeActivated(
        delete_user.clone(),
    )));
    assert_eq!(harness.state.last_opened, Some(delete_user.clone()));

    // Rename the request.
    harness.run(Message::Crud(CrudMsg::Rename(delete_user.clone())));
    let expected_old = Modal::Rename {
        path: delete_user.clone(),
        kind: NodeKind::Request,
        old_name: "Delete user".to_string(),
        buffer: "Delete user".to_string(),
    };
    assert_eq!(harness.state.modal, Some(expected_old));
    harness.run(Message::Modal(ModalMsg::TextChanged("Remove user".into())));
    let effects = harness.handle(Message::Modal(ModalMsg::Confirm));
    assert_eq!(
        effects,
        vec![Effect::Rename {
            path: delete_user.clone(),
            new_name: "Remove user".to_string()
        }]
    );
    for effect in effects {
        harness.execute(effect);
    }
    let remove_user = admin.join("Remove user.yaml");
    assert!(remove_user.is_file());
    assert!(!delete_user.exists());
    assert_eq!(read_request(&remove_user).unwrap().name, "Remove user");
    assert_eq!(harness.state.last_opened, Some(remove_user.clone()));

    // Rename the folder; expanded and last_opened keys are remapped.
    harness.run(Message::Crud(CrudMsg::Rename(users.clone())));
    harness.run(Message::Modal(ModalMsg::TextChanged("Members".into())));
    let effects = harness.handle(Message::Modal(ModalMsg::Confirm));
    assert_eq!(
        effects,
        vec![Effect::Rename {
            path: users.clone(),
            new_name: "Members".to_string()
        }]
    );
    for effect in effects {
        harness.execute(effect);
    }
    let members = dir.path().join("API").join("Members");
    let members_admin = members.join("Admin");
    assert!(members.is_dir());
    assert!(!users.exists());
    assert!(members_admin.is_dir());
    assert!(
        harness.state.expanded.contains(&members)
            && harness.state.expanded.contains(&members_admin),
        "expanded keys remapped: {:?}",
        harness.state.expanded
    );
    assert_eq!(
        harness.state.last_opened,
        Some(members_admin.join("Remove user.yaml"))
    );
}

#[test]
fn duplicate_creates_name_copy_on_disk() {
    let dir = tempfile::tempdir().unwrap();
    let (collection, users, _admin, delete_user) = seed_workspace(dir.path());
    let mut harness = Harness::new(dir.path().to_path_buf());
    harness.run(Message::Workspace(WorkspaceMsg::Reload));

    // Duplicate the request.
    harness.run(Message::Crud(CrudMsg::Duplicate(delete_user.clone())));
    assert!(harness.state.modal.is_none(), "duplicate needs no modal");
    let copy = admin_path_of(&users).join("Delete user copy.yaml");
    assert!(copy.is_file());
    assert_eq!(read_request(&copy).unwrap().name, "Delete user copy");

    // Duplicate the folder.
    harness.run(Message::Crud(CrudMsg::Duplicate(users.clone())));
    let users_copy = collection.join("Users copy");
    assert!(users_copy.is_dir());
    assert!(users_copy.join("Admin").join("Delete user.yaml").is_file());
    assert!(harness
        .collection("API")
        .folders
        .iter()
        .any(|f| f.name == "Users copy"));
}

fn admin_path_of(users: &Path) -> PathBuf {
    users.join("Admin")
}

#[test]
fn delete_requires_confirm_modal_and_cancel_keeps_file() {
    let dir = tempfile::tempdir().unwrap();
    let (_collection, _users, _admin, delete_user) = seed_workspace(dir.path());
    let mut harness = Harness::new(dir.path().to_path_buf());
    harness.run(Message::Workspace(WorkspaceMsg::Reload));

    // Opening delete shows the confirm modal (no effect executed yet).
    let effects = harness.handle(Message::Crud(CrudMsg::Delete(delete_user.clone())));
    assert!(effects.is_empty());
    assert_eq!(
        harness.state.modal,
        Some(Modal::ConfirmDelete {
            path: delete_user.clone(),
            kind: NodeKind::Request
        })
    );
    assert!(delete_user.is_file());

    // Cancel deletes nothing.
    let executed_before_cancel = harness.executed.len();
    harness.run(Message::Modal(ModalMsg::Cancel));
    assert_eq!(harness.state.modal, None);
    assert!(delete_user.is_file());
    assert_eq!(harness.executed.len(), executed_before_cancel);

    // Confirm emits Delete; the shell removes the file and reloads.
    harness.run(Message::Crud(CrudMsg::Delete(delete_user.clone())));
    let effects = harness.handle(Message::Modal(ModalMsg::Confirm));
    assert_eq!(
        effects,
        vec![Effect::Delete {
            path: delete_user.clone()
        }]
    );
    for effect in effects {
        harness.execute(effect);
    }
    assert!(!delete_user.exists());
    assert!(!harness.workspace().collections[0].folders[0].folders[0]
        .requests
        .iter()
        .any(|r| r.name == "Delete user"));
}

#[test]
fn duplicate_or_empty_names_are_rejected_into_status_without_disk_change() {
    let dir = tempfile::tempdir().unwrap();
    let (collection, _users, _admin, _delete_user) = seed_workspace(dir.path());
    let mut harness = Harness::new(dir.path().to_path_buf());
    harness.run(Message::Workspace(WorkspaceMsg::Reload));

    // Empty name.
    harness.run(Message::Crud(CrudMsg::NewCollection));
    harness.run(Message::Modal(ModalMsg::TextChanged("   ".into())));
    let effects = harness.handle(Message::Modal(ModalMsg::Confirm));
    assert!(effects.is_empty());
    assert!(
        harness
            .state
            .status
            .as_deref()
            .is_some_and(|s| s.contains("empty")),
        "status reports empty name: {:?}",
        harness.state.status
    );
    // The modal stays open so the user can fix the input.
    assert!(harness.state.modal.is_some());

    // Duplicate name: "API" already exists.
    harness.run(Message::Modal(ModalMsg::TextChanged("API".into())));
    let effects = harness.handle(Message::Modal(ModalMsg::Confirm));
    assert!(effects.is_empty());
    assert!(
        harness
            .state
            .status
            .as_deref()
            .is_some_and(|s| s.contains("already exists")),
        "status reports duplicate: {:?}",
        harness.state.status
    );
    assert_eq!(harness.workspace().collections.len(), 1, "disk unchanged");
    assert!(collection.is_dir());

    // Duplicate request name in the same parent.
    let api = collection;
    harness.run(Message::Crud(CrudMsg::NewRequest(api.clone())));
    harness.run(Message::Modal(ModalMsg::TextChanged("Get health".into())));
    let effects = harness.handle(Message::Modal(ModalMsg::Confirm));
    assert!(effects.is_empty());
    assert!(harness
        .state
        .status
        .as_deref()
        .is_some_and(|s| s.contains("already exists")));
}

#[test]
fn reload_reflects_external_disk_edits() {
    let dir = tempfile::tempdir().unwrap();
    let (collection, _users, _admin, _delete_user) = seed_workspace(dir.path());
    let mut harness = Harness::new(dir.path().to_path_buf());
    harness.run(Message::Workspace(WorkspaceMsg::Reload));

    // External edit: a new request file appears on disk.
    let external = Request {
        name: "External ping".to_string(),
        method: HttpMethod::Get,
        url: "https://example.com/ping".to_string(),
        ..new_request("")
    };
    FsWorkspaceStorage
        .create_request(&collection, &external)
        .unwrap();
    assert!(!harness
        .collection("API")
        .requests
        .iter()
        .any(|r| r.name == "External ping"));

    harness.run(Message::Workspace(WorkspaceMsg::Reload));
    assert!(
        harness
            .collection("API")
            .requests
            .iter()
            .any(|r| r.name == "External ping"),
        "reload reflects the external edit"
    );
    assert!(collection.join("External ping.yaml").is_file());

    // Method badges are populated from the parsed files.
    let ping = collection.join("External ping.yaml");
    assert_eq!(
        harness.state.request_methods.get(&ping),
        Some(&HttpMethod::Get)
    );
}

#[test]
fn history_tab_records_selection() {
    let dir = tempfile::tempdir().unwrap();
    seed_workspace(dir.path());
    seed_history(dir.path());
    let mut harness = Harness::new(dir.path().to_path_buf());
    harness.run(Message::Workspace(WorkspaceMsg::Reload));

    let history = &harness.workspace().history;
    assert_eq!(history.len(), 1);
    assert_eq!(history[0].request.url, "https://example.com/orders");
    assert_eq!(history[0].response.status_code, 201);

    harness.run(Message::Sidebar(SidebarMsg::TabSelected(
        SidebarTab::History,
    )));
    assert_eq!(harness.state.sidebar_tab, SidebarTab::History);

    harness.run(Message::Sidebar(SidebarMsg::HistoryEntrySelected(0)));
    assert_eq!(harness.state.history_selected, Some(0));
}
