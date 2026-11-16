//! Reducer + shell-equivalent tests for the Request Composer increment.
//!
//! These drive [`AppState::update`] exactly like the iced shell would,
//! executing the returned [`Effect`]s against a real tempdir via
//! [`FsWorkspaceStorage`] and feeding reload results back in through
//! `Message::Workspace(WorkspaceMsg::Loaded(..))`. Network/clipboard
//! effects (`SendRequest`/`CancelSend`/`CopyText`) are recorded, not
//! executed — the send pipeline's transport lives in the iced shell.

use rest_harness::app::composer::{BodyMode, SubTab};
use rest_harness::app::{
    new_request, AppState, ComposerMsg, Effect, EnvScope, LoadedWorkspace, Message, Modal,
    ModalMsg, SidebarMsg, TabKind, TabMsg, WorkspaceMsg,
};
use rest_harness::domain::{
    Environment, HistoryEntry, HttpHeader, HttpMethod, KeyValueRow, RawContentType,
    RedactedHttpRequest, Request, RequestBody,
};
use rest_harness::history::HistoryStore;
use rest_harness::http::HttpResponseData;
use rest_harness::storage::{FsWorkspaceStorage, WorkspaceStorage};
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
                self.state
                    .update(Message::Workspace(WorkspaceMsg::Loaded(Ok(
                        LoadedWorkspace::new(workspace),
                    ))));
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
                let request =
                    rest_harness::storage::read_request(&path).map_err(|error| error.to_string());
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
                // Must NOT overwrite — mirrors the shell's `create_request`.
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

    /// The active tab's id, or panic.
    fn active_id(&self) -> u64 {
        self.state.active_tab.expect("a tab is active")
    }

    /// The active tab, or panic.
    fn active(&self) -> &rest_harness::app::Tab {
        self.state.active_tab().expect("a tab is active")
    }
}

/// Seed a workspace with one Collection `API`, a `staging` Environment
/// (`base_url`), a shared Secret `api_key` (value `topsecret`) and one
/// saved Request. Returns `(collection_dir, request_path)`.
fn seed_workspace(root: &Path) -> (PathBuf, PathBuf) {
    let storage = FsWorkspaceStorage;
    let collection = storage.create_collection(root, "API").unwrap();
    let mut env = Environment::new("staging", collection.path.join("environments/staging.yaml"));
    env.variables
        .insert("base_url".to_string(), "https://example.com".to_string());
    storage.save_environment(&collection.path, &env).unwrap();

    let mut secrets = rest_harness::domain::SecretStore::empty();
    secrets
        .shared
        .insert("api_key".to_string(), "topsecret".to_string());
    storage.save_secrets(&collection.path, &secrets).unwrap();

    let request = Request {
        name: "Get health".to_string(),
        method: HttpMethod::Get,
        url: "https://example.com/health".to_string(),
        ..new_request("")
    };
    let path = storage.create_request(&collection.path, &request).unwrap();
    (collection.path, path)
}

/// A plain HTTP 200 response for `SendFinished`.
fn ok_response() -> HttpResponseData {
    HttpResponseData {
        status_code: 200,
        reason_phrase: "OK".to_string(),
        elapsed_ms: 42,
        size_bytes: 13,
        headers: vec![HttpHeader {
            name: "content-type".to_string(),
            value: "application/json".to_string(),
        }],
        body_raw: "{\"ok\":true}".to_string(),
        body_pretty: "{\n  \"ok\": true\n}".to_string(),
    }
}

/// Seed one History Entry (a POST to /orders with a redacted header).
fn seed_history(root: &Path) {
    let entry: HistoryEntry = rest_harness::history::build_entry(
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
fn opening_a_saved_request_focuses_a_single_deduped_tab() {
    let dir = tempfile::tempdir().unwrap();
    let (_collection, request) = seed_workspace(dir.path());
    let mut harness = Harness::new(dir.path().to_path_buf());
    harness.run(Message::Workspace(WorkspaceMsg::Reload));

    // Opening goes through the sidebar: NodeActivated → Effect::OpenRequest
    // → shell read → TabMsg::Opened.
    harness.run(Message::Sidebar(SidebarMsg::NodeActivated(request.clone())));
    assert_eq!(harness.state.last_opened, Some(request.clone()));
    assert_eq!(harness.state.tabs.len(), 1);
    assert_eq!(
        harness.active().kind,
        TabKind::Saved {
            path: request.clone()
        }
    );
    assert!(!harness.active().dirty);

    // Re-activating focuses the existing tab instead of appending one.
    harness.run(Message::Sidebar(SidebarMsg::NodeActivated(request.clone())));
    assert_eq!(harness.state.tabs.len(), 1);

    // Selecting another tab id round-trips through TabMsg::Select.
    let id = harness.active_id();
    harness.run(Message::Tab(TabMsg::Select(id)));
    assert_eq!(harness.state.active_tab, Some(id));
}

#[test]
fn drafts_are_created_with_sequential_untitled_names() {
    let dir = tempfile::tempdir().unwrap();
    seed_workspace(dir.path());
    let mut harness = Harness::new(dir.path().to_path_buf());
    harness.run(Message::Workspace(WorkspaceMsg::Reload));

    harness.run(Message::Tab(TabMsg::NewDraft));
    assert_eq!(harness.active().request.name, "Untitled");
    assert!(matches!(harness.active().kind, TabKind::Draft));
    assert!(!harness.active().dirty);

    let first = harness.active_id();
    harness.run(Message::Tab(TabMsg::NewDraft));
    assert_eq!(harness.active().request.name, "Untitled 2");
    assert_ne!(harness.active_id(), first);
    assert_eq!(harness.state.tabs.len(), 2);

    // Closing the active draft focuses its neighbour.
    harness.run(Message::Tab(TabMsg::Close(harness.active_id())));
    assert_eq!(harness.state.active_tab, Some(first));
    assert_eq!(harness.state.tabs.len(), 1);
}

#[test]
fn composer_edits_set_dirty_but_response_state_does_not() {
    let dir = tempfile::tempdir().unwrap();
    let (_collection, request) = seed_workspace(dir.path());
    let mut harness = Harness::new(dir.path().to_path_buf());
    harness.run(Message::Workspace(WorkspaceMsg::Reload));
    harness.run(Message::Sidebar(SidebarMsg::NodeActivated(request)));

    // A method edit marks the tab dirty.
    harness.run(Message::Composer(ComposerMsg::MethodChanged(
        HttpMethod::Post,
    )));
    assert!(harness.active().dirty);
    assert_eq!(harness.active().request.method, HttpMethod::Post);

    // Sub-tab/view switches are response-only state: never dirty.
    let id = harness.active_id();
    harness.state.tabs[0].dirty = false;
    harness.run(Message::Composer(ComposerMsg::SubTabSelected(
        SubTab::Headers,
    )));
    harness.run(Message::Composer(ComposerMsg::BodyModeChanged(
        BodyMode::Json,
    )));
    assert!(harness.active().dirty, "body mode is an editor edit");
    harness.state.tabs[0].dirty = false;
    harness
        .state
        .update(Message::Composer(ComposerMsg::SendFinished {
            tab_id: id,
            result: Ok(ok_response()),
        }));
    assert!(!harness.active().dirty, "responses are not edits");
    assert!(
        harness.active().response.is_some(),
        "response stored in the tab"
    );
}

#[test]
fn url_and_params_stay_in_sync_both_ways() {
    let dir = tempfile::tempdir().unwrap();
    seed_workspace(dir.path());
    let mut harness = Harness::new(dir.path().to_path_buf());
    harness.run(Message::Workspace(WorkspaceMsg::Reload));
    harness.run(Message::Tab(TabMsg::NewDraft));

    // URL → params.
    harness.run(Message::Composer(ComposerMsg::UrlChanged(
        "https://api.example.com/v1/users?id=1&limit=20".to_string(),
    )));
    let tab = harness.active();
    assert_eq!(tab.request.params.len(), 2);
    assert_eq!(tab.request.params[0].key, "id");
    assert_eq!(tab.request.params[0].value, "1");
    assert_eq!(tab.request.params[1].key, "limit");
    assert!(tab.dirty);

    // Param edit → URL rebuild.
    harness.run(Message::Composer(ComposerMsg::ParamValueChanged(
        0,
        "9".to_string(),
    )));
    assert_eq!(
        harness.active().request.url,
        "https://api.example.com/v1/users?id=9&limit=20"
    );

    // Toggling a row disables it in the URL (enabled rows are the URL).
    harness.run(Message::Composer(ComposerMsg::ParamToggled(0)));
    assert_eq!(
        harness.active().request.url,
        "https://api.example.com/v1/users?limit=20"
    );
    assert_eq!(harness.active().request.params.len(), 2, "row preserved");

    // Removing the last enabled row drops the query entirely.
    harness.run(Message::Composer(ComposerMsg::ParamToggled(0)));
    harness.run(Message::Composer(ComposerMsg::ParamRemoved(1)));
    harness.run(Message::Composer(ComposerMsg::ParamRemoved(0)));
    assert_eq!(
        harness.active().request.url,
        "https://api.example.com/v1/users"
    );
}

#[test]
fn close_guard_modal_saves_discards_and_cancels() {
    let dir = tempfile::tempdir().unwrap();
    let (collection, request) = seed_workspace(dir.path());
    let mut harness = Harness::new(dir.path().to_path_buf());
    harness.run(Message::Workspace(WorkspaceMsg::Reload));
    harness.run(Message::Sidebar(SidebarMsg::NodeActivated(request.clone())));

    // Clean tab closes immediately.
    let id = harness.active_id();
    harness.run(Message::Tab(TabMsg::Close(id)));
    assert!(harness.state.tabs.is_empty());

    // Dirty tab opens the guard.
    harness.run(Message::Sidebar(SidebarMsg::NodeActivated(request.clone())));
    harness.run(Message::Composer(ComposerMsg::UrlChanged(
        "https://example.com/health?v=2".to_string(),
    )));
    let id = harness.active_id();
    harness.run(Message::Tab(TabMsg::Close(id)));
    assert_eq!(harness.state.modal, Some(Modal::CloseDirty { tab_id: id }));

    // Cancel closes only the modal.
    harness.run(Message::Modal(ModalMsg::Cancel));
    assert_eq!(harness.state.modal, None);
    assert_eq!(harness.state.tabs.len(), 1);

    // Discard closes the tab without saving.
    harness.run(Message::Tab(TabMsg::Close(id)));
    harness.run(Message::Modal(ModalMsg::Discard));
    assert!(harness.state.tabs.is_empty());
    assert!(
        !collection.join("Get health.yaml").is_file() || {
            let on_disk = rest_harness::storage::read_request(&request).unwrap();
            on_disk.url == "https://example.com/health"
        },
        "discard must not write"
    );

    // Save route: save then close on the success reload.
    harness.run(Message::Sidebar(SidebarMsg::NodeActivated(request.clone())));
    harness.run(Message::Composer(ComposerMsg::UrlChanged(
        "https://example.com/health?v=3".to_string(),
    )));
    let id = harness.active_id();
    harness.run(Message::Tab(TabMsg::Close(id)));
    let effects = harness.handle(Message::Modal(ModalMsg::Confirm));
    match &effects[..] {
        [Effect::SaveRequest { path, request }] => {
            assert_eq!(path, &request_path_of(&collection, "Get health"));
            assert_eq!(request.url, "https://example.com/health?v=3");
        }
        other => panic!("unexpected effects: {other:?}"),
    }
    assert_eq!(harness.state.pending_close, Some(id));
    for effect in effects {
        harness.execute(effect);
    }
    assert!(harness.state.tabs.is_empty(), "closed after save");
    let saved = rest_harness::storage::read_request(&request).unwrap();
    assert_eq!(saved.url, "https://example.com/health?v=3");
}

fn request_path_of(collection: &Path, name: &str) -> PathBuf {
    collection.join(format!("{name}.yaml"))
}

#[test]
fn close_guard_save_on_a_draft_routes_to_save_draft_then_closes() {
    let dir = tempfile::tempdir().unwrap();
    let (collection, _request) = seed_workspace(dir.path());
    let mut harness = Harness::new(dir.path().to_path_buf());
    harness.run(Message::Workspace(WorkspaceMsg::Reload));
    harness.run(Message::Tab(TabMsg::NewDraft));

    // Make the draft dirty, then close: the guard opens.
    harness.run(Message::Composer(ComposerMsg::UrlChanged(
        "https://example.com/late".to_string(),
    )));
    let id = harness.active_id();
    harness.run(Message::Tab(TabMsg::Close(id)));
    assert!(matches!(
        harness.state.modal,
        Some(Modal::CloseDirty { .. })
    ));

    // "Save" on a draft routes to the save-draft modal, arming the close.
    let effects = harness.handle(Message::Modal(ModalMsg::Confirm));
    assert!(effects.is_empty(), "no effect until the draft is named");
    assert!(matches!(harness.state.modal, Some(Modal::SaveDraft { .. })));
    assert_eq!(harness.state.pending_close, Some(id));

    // Cancelling the save-draft modal disarms the close and keeps the tab.
    harness.run(Message::Modal(ModalMsg::Cancel));
    assert_eq!(harness.state.pending_close, None);
    assert_eq!(harness.state.tabs.len(), 1);

    // Again: Close → Save → name → confirm → SaveDraft effect; the reload
    // promotes AND closes the tab.
    harness.run(Message::Tab(TabMsg::Close(id)));
    harness.run(Message::Modal(ModalMsg::Confirm));
    harness.run(Message::Modal(ModalMsg::NameChanged("Late ping".into())));
    let effects = harness.handle(Message::Modal(ModalMsg::Confirm));
    assert!(matches!(effects[..], [Effect::SaveDraft { .. }]));
    for effect in effects {
        harness.execute(effect);
    }
    assert!(harness.state.tabs.is_empty(), "closed after promotion");
    assert!(request_path_of(&collection, "Late ping").is_file());
}

#[test]
fn draft_save_promotes_the_tab_to_saved_at_the_target_path() {
    let dir = tempfile::tempdir().unwrap();
    let (collection, _request) = seed_workspace(dir.path());
    let mut harness = Harness::new(dir.path().to_path_buf());
    harness.run(Message::Workspace(WorkspaceMsg::Reload));
    harness.run(Message::Tab(TabMsg::NewDraft));
    harness.run(Message::Composer(ComposerMsg::UrlChanged(
        "https://example.com/ping".to_string(),
    )));

    // Save on a draft opens the save-draft modal.
    harness.run(Message::Composer(ComposerMsg::Save));
    let Some(Modal::SaveDraft {
        tab_id,
        collections,
        ..
    }) = harness.state.modal.clone()
    else {
        panic!("save-draft modal expected");
    };
    assert_eq!(collections, vec![collection.clone()]);
    assert_eq!(tab_id, harness.active_id());

    // Fill the name and confirm; nested folder validation comes free via
    // the empty default (the Collection itself).
    harness.run(Message::Modal(ModalMsg::NameChanged("Ping health".into())));
    let effects = harness.handle(Message::Modal(ModalMsg::Confirm));
    let target = request_path_of(&collection, "Ping health");
    match &effects[..] {
        [Effect::SaveDraft { parent, request }] => {
            assert_eq!(parent, &collection);
            assert_eq!(request.name, "Ping health");
            assert_eq!(request.url, "https://example.com/ping");
        }
        other => panic!("unexpected effects: {other:?}"),
    }
    assert_eq!(
        harness.state.pending_save,
        Some(rest_harness::app::PendingSave {
            tab_id: harness.active_id(),
            path: target.clone(),
        })
    );
    for effect in effects {
        harness.execute(effect);
    }

    // Post-reload: the draft tab became the saved tab for the new path.
    let tab = harness.active();
    assert_eq!(
        tab.kind,
        TabKind::Saved {
            path: target.clone()
        }
    );
    assert!(!tab.dirty);
    assert!(target.is_file());
    assert!(harness.workspace().collections[0]
        .requests
        .iter()
        .any(|r| r.name == "Ping health"));
}

#[test]
fn draft_save_rejects_colliding_and_nested_unknown_folders() {
    let dir = tempfile::tempdir().unwrap();
    let (collection, _request) = seed_workspace(dir.path());
    let mut harness = Harness::new(dir.path().to_path_buf());
    harness.run(Message::Workspace(WorkspaceMsg::Reload));
    harness.run(Message::Tab(TabMsg::NewDraft));

    harness.run(Message::Composer(ComposerMsg::Save));
    // "Get health" already exists in the collection.
    harness.run(Message::Modal(ModalMsg::NameChanged("Get health".into())));
    let effects = harness.handle(Message::Modal(ModalMsg::Confirm));
    assert!(effects.is_empty());
    assert!(
        harness
            .state
            .status
            .as_deref()
            .is_some_and(|status| status.contains("already exists")),
        "collision reported: {:?}",
        harness.state.status
    );

    // Unknown nested folder segment is validated against the tree.
    harness.run(Message::Modal(ModalMsg::FolderChanged(
        "Nope/Missing".into(),
    )));
    harness.run(Message::Modal(ModalMsg::NameChanged("Fresh".into())));
    let effects = harness.handle(Message::Modal(ModalMsg::Confirm));
    assert!(effects.is_empty());
    assert!(
        harness
            .state
            .status
            .as_deref()
            .is_some_and(|status| status.contains("does not exist")),
        "folder validated: {:?}",
        harness.state.status
    );
    assert!(
        harness.state.modal.is_some(),
        "modal stays open on validation errors"
    );

    // The existing collection itself is a valid folder target.
    harness.run(Message::Modal(ModalMsg::FolderChanged(String::new())));
    let effects = harness.handle(Message::Modal(ModalMsg::Confirm));
    assert!(matches!(effects[..], [Effect::SaveDraft { .. }]));
    let _ = collection;
}

#[test]
fn rename_remaps_saved_tabs_and_delete_closes_them() {
    let dir = tempfile::tempdir().unwrap();
    let storage = FsWorkspaceStorage;
    let collection = storage.create_collection(dir.path(), "API").unwrap();
    let users = storage
        .create_folder(&collection.path, "Users")
        .unwrap()
        .path;
    let first = storage
        .create_request(
            &users,
            &Request {
                name: "First".to_string(),
                url: "https://example.com/1".to_string(),
                ..new_request("")
            },
        )
        .unwrap();
    let second = storage
        .create_request(
            &users,
            &Request {
                name: "Second".to_string(),
                url: "https://example.com/2".to_string(),
                ..new_request("")
            },
        )
        .unwrap();
    let mut harness = Harness::new(dir.path().to_path_buf());
    harness.run(Message::Workspace(WorkspaceMsg::Reload));

    harness.run(Message::Sidebar(SidebarMsg::NodeActivated(first.clone())));
    harness.run(Message::Sidebar(SidebarMsg::NodeActivated(second.clone())));
    assert_eq!(harness.state.tabs.len(), 2);

    // Renaming the folder remaps every Saved tab under it.
    harness.run(Message::Crud(rest_harness::app::CrudMsg::Rename(
        users.clone(),
    )));
    harness.run(Message::Modal(ModalMsg::TextChanged("Members".into())));
    harness.run(Message::Modal(ModalMsg::Confirm));
    let members = collection.path.join("Members");
    assert_eq!(
        harness.state.tabs[0].kind,
        TabKind::Saved {
            path: members.join("First.yaml")
        }
    );
    assert_eq!(
        harness.state.tabs[1].kind,
        TabKind::Saved {
            path: members.join("Second.yaml")
        }
    );

    // Deleting the folder closes both tabs after confirmation.
    harness.run(Message::Crud(rest_harness::app::CrudMsg::Delete(
        members.clone(),
    )));
    harness.run(Message::Modal(ModalMsg::Confirm));
    assert!(harness.state.tabs.is_empty());
    assert!(!members.exists());
}

#[test]
fn loaded_prunes_saved_tabs_whose_file_vanished() {
    let dir = tempfile::tempdir().unwrap();
    let (collection, request) = seed_workspace(dir.path());
    let mut harness = Harness::new(dir.path().to_path_buf());
    harness.run(Message::Workspace(WorkspaceMsg::Reload));
    harness.run(Message::Sidebar(SidebarMsg::NodeActivated(request.clone())));
    assert_eq!(harness.state.tabs.len(), 1);

    // External deletion, then a reload: the tab is pruned.
    FsWorkspaceStorage.delete_request(&request).unwrap();
    harness.run(Message::Workspace(WorkspaceMsg::Reload));
    assert!(harness.state.tabs.is_empty());
    assert_eq!(harness.state.active_tab, None);
    let _ = collection;
}

#[test]
fn missing_variables_fail_fast_without_any_effect() {
    let dir = tempfile::tempdir().unwrap();
    let (collection, _request) = seed_workspace(dir.path());
    let mut harness = Harness::new(dir.path().to_path_buf());
    harness.run(Message::Workspace(WorkspaceMsg::Reload));

    // Open a tab whose request needs {{base_url}} and {{api_key}} but with
    // no environment selected yet: fail fast, no effect, no network.
    harness.state.update(Message::Tab(TabMsg::Opened {
        path: dir.path().join("API/Get health.yaml"),
        request: Ok(Request {
            name: "Get health".to_string(),
            url: "{{base_url}}/health?k={{api_key}}".to_string(),
            ..new_request("")
        }),
    }));
    let effects = harness.handle(Message::Composer(ComposerMsg::Send));
    assert!(effects.is_empty(), "no effect on missing vars");
    // The tab's own Collection scope already resolves the shared secret
    // `api_key`; only `base_url` (an environment variable) is missing.
    assert!(
        harness
            .active()
            .send_error
            .as_ref()
            .is_some_and(|error| error.contains("base_url")),
        "error lists the missing names: {:?}",
        harness.active().send_error
    );
    assert!(!harness.active().in_flight);

    // Selecting the staging environment resolves both and the send goes
    // through as an Effect::SendRequest.
    let id = harness.active_id();
    harness.run(Message::Composer(ComposerMsg::EnvSelected(Some(
        EnvScope {
            collection_dir: collection.clone(),
            env_name: Some("staging".to_string()),
        },
    ))));
    let effects = harness.handle(Message::Composer(ComposerMsg::Send));
    assert!(
        matches!(
            &effects[..],
            [Effect::SendRequest { tab_id, prepared }] if *tab_id == id && prepared.url == "https://example.com/health?k=topsecret"
        ),
        "send effect after env selection: {:?}",
        effects
    );
    assert!(harness.active().in_flight);
}

#[test]
fn send_finished_stores_the_response_and_appends_history() {
    let dir = tempfile::tempdir().unwrap();
    let (collection, request) = seed_workspace(dir.path());
    let mut harness = Harness::new(dir.path().to_path_buf());
    harness.run(Message::Workspace(WorkspaceMsg::Reload));
    harness.run(Message::Sidebar(SidebarMsg::NodeActivated(request)));

    // Make the request contain a secret value so the history entry must be
    // redacted with the tab's secret values.
    harness.state.tabs[0].request.url = "https://example.com/health?k=topsecret".to_string();
    let effects = harness.handle(Message::Composer(ComposerMsg::Send));
    assert!(matches!(&effects[..], [Effect::SendRequest { .. }]));
    let id = harness.active_id();

    let effects = harness.handle(Message::Composer(ComposerMsg::SendFinished {
        tab_id: id,
        result: Ok(ok_response()),
    }));
    match &effects[..] {
        [Effect::AppendHistory {
            entry,
            secret_values,
        }] => {
            assert_eq!(entry.request.url, "https://example.com/health?k=topsecret");
            assert_eq!(entry.response.status_code, 200);
            assert_eq!(secret_values, &vec!["topsecret".to_string()]);
        }
        other => panic!("unexpected effects: {other:?}"),
    }

    // The tab stores the response and drops in_flight.
    let tab = harness.active();
    assert!(!tab.in_flight);
    assert!(tab.send_error.is_none());
    let response = tab.response.as_ref().expect("response stored");
    assert_eq!(response.status_code, 200);
    assert_eq!(response.reason_phrase, "OK");
    assert_eq!(response.elapsed_ms, 42);
    assert_eq!(response.size_bytes, 13);
    assert_eq!(response.body_pretty, "{\n  \"ok\": true\n}");
    assert_eq!(response.headers.len(), 1);

    // Executing the effect persists a REDACTED history entry and the
    // sidebar sees it after the reload.
    for effect in effects {
        harness.execute(effect);
    }
    let history = &harness.workspace().history;
    assert_eq!(history.len(), 1);
    assert_eq!(
        history[0].request.url, "https://example.com/health?k=***",
        "secret redacted on write"
    );
    let _ = collection;
}

#[test]
fn send_errors_and_cancellations_reset_in_flight() {
    let dir = tempfile::tempdir().unwrap();
    let (_collection, request) = seed_workspace(dir.path());
    let mut harness = Harness::new(dir.path().to_path_buf());
    harness.run(Message::Workspace(WorkspaceMsg::Reload));
    harness.run(Message::Sidebar(SidebarMsg::NodeActivated(request.clone())));
    let id = harness.active_id();

    harness.run(Message::Composer(ComposerMsg::Send));
    assert!(harness.active().in_flight);

    // A failed send lands in send_error, in_flight resets, no history.
    let effects = harness.handle(Message::Composer(ComposerMsg::SendFinished {
        tab_id: id,
        result: Err("connection refused".to_string()),
    }));
    assert!(effects.is_empty());
    assert!(!harness.active().in_flight);
    assert_eq!(
        harness.active().send_error.as_deref(),
        Some("connection refused")
    );

    // A cancelled send resets in_flight with no effects.
    harness.state.tabs[0].send_error = None;
    harness.run(Message::Composer(ComposerMsg::Send));
    assert!(harness.active().in_flight);
    let effects = harness.handle(Message::Composer(ComposerMsg::SendCancelled { tab_id: id }));
    assert!(effects.is_empty());
    assert!(!harness.active().in_flight);
    assert!(
        harness.active().prepared.is_none(),
        "prepared request dropped on cancel"
    );

    // Sending while in flight is ignored.
    harness.run(Message::Composer(ComposerMsg::Send));
    assert_eq!(
        harness
            .state
            .tabs
            .iter()
            .filter(|tab| tab.in_flight)
            .count(),
        1
    );
}

#[test]
fn history_selection_opens_a_prefilled_detached_draft() {
    let dir = tempfile::tempdir().unwrap();
    seed_workspace(dir.path());
    seed_history(dir.path());
    let mut harness = Harness::new(dir.path().to_path_buf());
    harness.run(Message::Workspace(WorkspaceMsg::Reload));
    harness.run(Message::Sidebar(SidebarMsg::TabSelected(
        rest_harness::app::SidebarTab::History,
    )));

    harness.run(Message::Sidebar(SidebarMsg::HistoryEntrySelected(0)));
    assert_eq!(harness.state.history_selected, Some(0));
    let tab = harness.active();
    assert!(matches!(tab.kind, TabKind::HistoryDraft));
    assert!(!tab.dirty);

    // Prefilled from the entry: method, url, headers and the response.
    assert_eq!(tab.request.method, HttpMethod::Post);
    assert_eq!(tab.request.url, "https://example.com/orders");
    assert_eq!(tab.request.headers.len(), 1);
    assert_eq!(
        tab.request.body,
        RequestBody::Raw {
            content_type: RawContentType::Json,
            content: "{}".to_string(),
        }
    );
    let response = tab.response.as_ref().expect("prefilled response");
    assert_eq!(response.status_code, 201);
    assert_eq!(response.elapsed_ms, 42);
    assert_eq!(response.size_bytes, 128);
    assert_eq!(response.body_pretty, "{}");

    // The Save action routes through the save-draft modal (a HistoryDraft
    // is a detached draft, see the dedicated save tests below).
    harness.run(Message::Composer(ComposerMsg::Save));
    assert!(matches!(harness.state.modal, Some(Modal::SaveDraft { .. })));
}

#[test]
fn history_draft_prefills_the_params_table_from_the_entry_url() {
    let dir = tempfile::tempdir().unwrap();
    seed_workspace(dir.path());
    let entry: HistoryEntry = rest_harness::history::build_entry(
        RedactedHttpRequest {
            method: HttpMethod::Get,
            url: "https://example.com/orders?id=1&flag=x".to_string(),
            headers: Vec::new(),
            body: String::new(),
        },
        200,
        "OK".to_string(),
        9,
        3,
        Vec::new(),
        "ok".to_string(),
    );
    HistoryStore::new(dir.path())
        .append_entry(entry, &[])
        .unwrap();

    let mut harness = Harness::new(dir.path().to_path_buf());
    harness.run(Message::Workspace(WorkspaceMsg::Reload));
    harness.run(Message::Sidebar(SidebarMsg::HistoryEntrySelected(0)));

    // The entry URL's query pairs prefill the Params table as enabled rows.
    let tab = harness.active();
    assert_eq!(
        tab.request.params,
        vec![KeyValueRow::new("id", "1"), KeyValueRow::new("flag", "x")]
    );
    assert!(tab.request.params.iter().all(|row| row.enabled));
}

#[test]
fn opening_a_saved_request_syncs_url_and_params_from_disk() {
    let dir = tempfile::tempdir().unwrap();
    let (collection, _request) = seed_workspace(dir.path());
    let storage = FsWorkspaceStorage;
    // Saved with a query in the URL but an empty params table.
    let with_query = storage
        .create_request(
            &collection,
            &Request {
                name: "With query".to_string(),
                url: "https://example.com/list?id=7&flag=x".to_string(),
                ..new_request("")
            },
        )
        .unwrap();
    // Saved with enabled params the URL does not carry.
    let with_params = storage
        .create_request(
            &collection,
            &Request {
                name: "With params".to_string(),
                url: "https://example.com/list".to_string(),
                params: vec![KeyValueRow::new("a", "1")],
                ..new_request("")
            },
        )
        .unwrap();
    let mut harness = Harness::new(dir.path().to_path_buf());
    harness.run(Message::Workspace(WorkspaceMsg::Reload));

    // Empty params + `?` in the URL: the params table is populated.
    harness.run(Message::Sidebar(SidebarMsg::NodeActivated(with_query)));
    assert_eq!(
        harness.active().request.params,
        vec![KeyValueRow::new("id", "7"), KeyValueRow::new("flag", "x")]
    );

    // Enabled params + a stale URL: the URL is rebuilt from the table.
    harness.run(Message::Sidebar(SidebarMsg::NodeActivated(with_params)));
    let tab = harness.active();
    assert_eq!(tab.request.url, "https://example.com/list?a=1");
    assert_eq!(tab.request.params, vec![KeyValueRow::new("a", "1")]);
}

#[test]
fn history_draft_save_opens_the_save_draft_modal() {
    let dir = tempfile::tempdir().unwrap();
    seed_workspace(dir.path());
    seed_history(dir.path());
    let mut harness = Harness::new(dir.path().to_path_buf());
    harness.run(Message::Workspace(WorkspaceMsg::Reload));
    harness.run(Message::Sidebar(SidebarMsg::HistoryEntrySelected(0)));

    // Save on a HistoryDraft opens the save-draft modal (never upserts).
    harness.run(Message::Composer(ComposerMsg::Save));
    let Some(Modal::SaveDraft { tab_id, name, .. }) = harness.state.modal.clone() else {
        panic!("save-draft modal expected");
    };
    assert_eq!(tab_id, harness.active_id());
    assert_eq!(name, "", "history drafts start with an empty name");
}

#[test]
fn history_draft_save_creates_the_request_and_promotes_the_tab() {
    let dir = tempfile::tempdir().unwrap();
    let (collection, _request) = seed_workspace(dir.path());
    seed_history(dir.path());
    let mut harness = Harness::new(dir.path().to_path_buf());
    harness.run(Message::Workspace(WorkspaceMsg::Reload));
    harness.run(Message::Sidebar(SidebarMsg::HistoryEntrySelected(0)));

    harness.run(Message::Composer(ComposerMsg::Save));
    harness.run(Message::Modal(ModalMsg::NameChanged(
        "Orders replay".into(),
    )));
    let effects = harness.handle(Message::Modal(ModalMsg::Confirm));
    let target = request_path_of(&collection, "Orders replay");
    match &effects[..] {
        [Effect::SaveDraft { parent, request }] => {
            assert_eq!(parent, &collection);
            assert_eq!(request.name, "Orders replay");
            assert_eq!(request.url, "https://example.com/orders");
        }
        other => panic!("unexpected effects: {other:?}"),
    }
    assert_eq!(
        harness.state.pending_save,
        Some(rest_harness::app::PendingSave {
            tab_id: harness.active_id(),
            path: target.clone(),
        })
    );

    // The success reload promotes the tab to Saved at the created path.
    for effect in effects {
        harness.execute(effect);
    }
    let tab = harness.active();
    assert_eq!(
        tab.kind,
        TabKind::Saved {
            path: target.clone()
        }
    );
    assert!(!tab.dirty);
    assert!(target.is_file(), "the request file was created");
    assert!(harness.workspace().collections[0]
        .requests
        .iter()
        .any(|r| r.name == "Orders replay"));
}

#[test]
fn history_draft_save_does_not_overwrite_an_existing_request() {
    let dir = tempfile::tempdir().unwrap();
    let (collection, _request) = seed_workspace(dir.path());
    let existing = FsWorkspaceStorage
        .create_request(
            &collection,
            &Request {
                name: "Orders".to_string(),
                url: "https://example.com/orders-original".to_string(),
                ..new_request("")
            },
        )
        .unwrap();
    seed_history(dir.path());
    let mut harness = Harness::new(dir.path().to_path_buf());
    harness.run(Message::Workspace(WorkspaceMsg::Reload));
    harness.run(Message::Sidebar(SidebarMsg::HistoryEntrySelected(0)));

    harness.run(Message::Composer(ComposerMsg::Save));
    harness.run(Message::Modal(ModalMsg::NameChanged("Orders".into())));
    let effects = harness.handle(Message::Modal(ModalMsg::Confirm));
    assert!(effects.is_empty(), "create_request must not be emitted");
    assert!(
        harness
            .state
            .status
            .as_deref()
            .is_some_and(|status| status.contains("already exists")),
        "collision reported: {:?}",
        harness.state.status
    );
    assert!(harness.state.modal.is_some(), "modal stays open");
    // The existing request file is untouched.
    assert_eq!(
        rest_harness::storage::read_request(&existing).unwrap().url,
        "https://example.com/orders-original"
    );
}

#[test]
fn close_guard_save_on_a_history_draft_routes_to_save_draft() {
    let dir = tempfile::tempdir().unwrap();
    let (collection, _request) = seed_workspace(dir.path());
    seed_history(dir.path());
    let mut harness = Harness::new(dir.path().to_path_buf());
    harness.run(Message::Workspace(WorkspaceMsg::Reload));
    harness.run(Message::Sidebar(SidebarMsg::HistoryEntrySelected(0)));

    // Make the HistoryDraft dirty, then close: the guard opens.
    harness.run(Message::Composer(ComposerMsg::UrlChanged(
        "https://example.com/orders?edit=1".to_string(),
    )));
    let id = harness.active_id();
    harness.run(Message::Tab(TabMsg::Close(id)));
    assert!(matches!(
        harness.state.modal,
        Some(Modal::CloseDirty { .. })
    ));

    // "Save" routes to the save-draft modal, arming the close.
    let effects = harness.handle(Message::Modal(ModalMsg::Confirm));
    assert!(effects.is_empty(), "no effect until the draft is named");
    assert!(matches!(harness.state.modal, Some(Modal::SaveDraft { .. })));
    assert_eq!(harness.state.pending_close, Some(id));

    // Name and confirm: the reload promotes AND closes the tab.
    harness.run(Message::Modal(ModalMsg::NameChanged(
        "Orders edited".into(),
    )));
    let effects = harness.handle(Message::Modal(ModalMsg::Confirm));
    assert!(matches!(effects[..], [Effect::SaveDraft { .. }]));
    for effect in effects {
        harness.execute(effect);
    }
    assert!(harness.state.tabs.is_empty(), "closed after promotion");
    assert!(request_path_of(&collection, "Orders edited").is_file());
}
