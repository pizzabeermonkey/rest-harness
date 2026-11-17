//! The `iced` shell for REST Harness.
//!
//! Thin adapter between the UI-agnostic app core (`rest_harness::app`) and
//! `iced` 0.14: maps `Message` → reducer → `Effect`s → `iced::Task`s, and
//! renders top bar, sidebar, request composer and modal overlay. All
//! `iced` `use` statements live here (and in `src/main.rs`); the lib crate
//! stays `iced`-free.
//!
//! # Composer shell responsibilities
//!
//! - **In-flight sends**: the shell owns the in-flight registry
//!   (`HashMap<tab id, CancellationToken>`) on [`App`]; the core stays
//!   pure. `Effect::SendRequest` runs the request inside a
//!   `tokio::select!` against the token's `cancelled()` future, replying
//!   `SendFinished`/`SendCancelled`.
//! - **Body editor**: the shell keeps one `text_editor::Content` per tab
//!   (the core stays `iced`-free). The editor's `on_edit` closure maps the
//!   (iced-typed) action into the core-owned [`BodyEdit`] mirror; `update`
//!   intercepts `ComposerMsg::BodyEdit`, performs the action on the
//!   shell-owned content and forwards the resulting text to the reducer as
//!   `ComposerMsg::BodyTextChanged`.
//! - **Keyboard**: `Ctrl+R` reloads the workspace, `Ctrl+S` saves the
//!   active tab, `Esc` cancels a modal. `keyboard::listen` only receives
//!   events the widgets ignore, so focused text inputs still let these
//!   through; the closures stay non-capturing as iced requires (see the
//!   `check_zero_sized` note in the original Ctrl+R wiring).
//!
//! # Quit-with-dirty-tabs limitation
//!
//! iced 0.14 offers no way to veto a window close request (the close
//! event cannot be intercepted and turned into a Task that keeps the
//! window open), so quitting the app with dirty tabs cannot show the
//! CloseDirty modal. This is a documented limitation; explicit tab closes
//! are still guarded. Not hacked around on purpose.

pub mod composer;
pub mod modal;
pub mod screenshot;
pub mod sidebar;
pub mod style;
pub mod topbar;

use iced::widget::text_editor;
use iced::{keyboard, Element, Subscription, Task};
use rest_harness::app::composer::{BodyEdit, BodyMotion};
use rest_harness::app::{
    AppState, ComposerMsg, Effect, LoadedWorkspace, Message, TabMsg, WorkspaceMsg,
};
use rest_harness::config::AppConfig;
use rest_harness::domain::{Folder, HttpMethod, RequestEntry, Workspace};
use rest_harness::history::HistoryStore;
use rest_harness::http::{HttpClient as _, ReqwestHttpClient};
use rest_harness::storage::{read_request, FsWorkspaceStorage, WorkspaceStorage};
use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use tokio_util::sync::CancellationToken;

/// The iced program state: the app core plus persisted config, the HTTP
/// client, the in-flight cancellation registry and the per-tab editor
/// contents (all shell-owned; the core stays `iced`-free).
pub struct App {
    /// UI-agnostic application state (reducer-owned).
    pub state: AppState,
    /// Persisted app config (last workspace path bookkeeping).
    config: AppConfig,
    /// HTTP client executing composer sends.
    http_client: ReqwestHttpClient,
    /// Cancellation tokens of in-flight sends, keyed by tab id.
    cancels: HashMap<u64, CancellationToken>,
    /// Body editor content per tab id (iced-side source of cursor state).
    editors: HashMap<u64, text_editor::Content>,
}

impl App {
    fn new(state: AppState, config: AppConfig) -> Self {
        App {
            state,
            config,
            http_client: ReqwestHttpClient::default(),
            cancels: HashMap::new(),
            editors: HashMap::new(),
        }
    }
}

/// Boot function for `iced::application(..)`: resolves the workspace
/// (`AppConfig::load()` + `resolve_workspace`, seeding the default
/// workspace on first run) and returns the initial `LoadWorkspace` task.
pub fn boot() -> (App, Task<Message>) {
    let config = AppConfig::load().unwrap_or_default();
    let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    let root = config
        .resolve_workspace(&cwd)
        .unwrap_or_else(|_| cwd.join("collections"));
    let state = AppState::new(Some(root.clone()));
    (App::new(state, config), load_task(root))
}

/// Shell update: intercept shell-owned editor interactions, run the
/// reducer, then execute every returned effect and keep the editor
/// contents in sync with the reducer's body text.
pub fn update(app: &mut App, message: Message) -> Task<Message> {
    let message = match message {
        // The shell owns the editor contents: perform the (mirrored)
        // action locally, then hand the resulting text to the core.
        Message::Composer(ComposerMsg::BodyEdit(edit)) => {
            let text = perform_body_edit(app, edit);
            Message::Composer(ComposerMsg::BodyTextChanged(text))
        }
        other => other,
    };

    // A send that finished (or was cancelled) no longer needs its token.
    match &message {
        Message::Composer(ComposerMsg::SendFinished { tab_id, .. })
        | Message::Composer(ComposerMsg::SendCancelled { tab_id }) => {
            app.cancels.remove(tab_id);
        }
        _ => {}
    }

    let effects = app.state.update(message);
    let tasks: Vec<Task<Message>> = effects
        .into_iter()
        .map(|effect| execute_effect(app, effect))
        .collect();
    sync_editors(app);
    Task::batch(tasks)
}

/// Perform a mirrored editor action on the active tab's editor content,
/// returning the resulting text.
fn perform_body_edit(app: &mut App, edit: BodyEdit) -> String {
    let Some(id) = app.state.active_tab else {
        return String::new();
    };
    let Some(content) = app.editors.get_mut(&id) else {
        return String::new();
    };
    if let Some(action) = editor_action(edit) {
        content.perform(action);
    }
    content.text()
}

/// Reconstruct the iced action from the core-owned mirror.
fn editor_action(edit: BodyEdit) -> Option<text_editor::Action> {
    use iced::widget::text_editor::{Action, Edit};
    match edit {
        BodyEdit::Insert(character) => Some(Action::Edit(Edit::Insert(character))),
        BodyEdit::Paste(text) => Some(Action::Edit(Edit::Paste(Arc::new(text)))),
        BodyEdit::Enter => Some(Action::Edit(Edit::Enter)),
        BodyEdit::Indent => Some(Action::Edit(Edit::Indent)),
        BodyEdit::Unindent => Some(Action::Edit(Edit::Unindent)),
        BodyEdit::Backspace => Some(Action::Edit(Edit::Backspace)),
        BodyEdit::Delete => Some(Action::Edit(Edit::Delete)),
        BodyEdit::Move(motion) => Some(Action::Move(motion_of(motion))),
        BodyEdit::Select(motion) => Some(Action::Select(motion_of(motion))),
        BodyEdit::SelectWord => Some(Action::SelectWord),
        BodyEdit::SelectLine => Some(Action::SelectLine),
        BodyEdit::SelectAll => Some(Action::SelectAll),
        BodyEdit::Click { x, y } => Some(Action::Click(iced::Point::new(x, y))),
        BodyEdit::Drag { x, y } => Some(Action::Drag(iced::Point::new(x, y))),
        BodyEdit::Scroll { lines } => Some(Action::Scroll { lines }),
    }
}

/// Map the core motion mirror onto the iced motion.
fn motion_of(motion: BodyMotion) -> text_editor::Motion {
    use text_editor::Motion;
    match motion {
        BodyMotion::Left => Motion::Left,
        BodyMotion::Right => Motion::Right,
        BodyMotion::Up => Motion::Up,
        BodyMotion::Down => Motion::Down,
        BodyMotion::WordLeft => Motion::WordLeft,
        BodyMotion::WordRight => Motion::WordRight,
        BodyMotion::Home => Motion::Home,
        BodyMotion::End => Motion::End,
        BodyMotion::PageUp => Motion::PageUp,
        BodyMotion::PageDown => Motion::PageDown,
        BodyMotion::DocumentStart => Motion::DocumentStart,
        BodyMotion::DocumentEnd => Motion::DocumentEnd,
    }
}

/// Keep one editor content per tab, recreated when the reducer's body text
/// diverges (mode switches, new tabs). Called after every message.
fn sync_editors(app: &mut App) {
    let ids: Vec<u64> = app.state.tabs.iter().map(|tab| tab.id).collect();
    app.editors.retain(|id, _| ids.contains(id));
    for tab in &app.state.tabs {
        let expected = tab.body_text();
        match app.editors.get_mut(&tab.id) {
            Some(content) => {
                if content.text() != expected {
                    *content = text_editor::Content::with_text(&expected);
                }
            }
            None => {
                app.editors
                    .insert(tab.id, text_editor::Content::with_text(&expected));
            }
        }
    }
}

/// Top-level view: workspace bar, sidebar + composer pane, status strip,
/// with the modal overlay stacked on top.
pub fn view(app: &App) -> Element<'_, Message> {
    let state = &app.state;
    let base = iced::widget::column![
        topbar::view(state),
        iced::widget::row![sidebar::view(state), composer::view(app)].height(iced::Fill),
        status_strip(state),
    ]
    .width(iced::Fill)
    .height(iced::Fill);

    match &state.modal {
        Some(_) => iced::widget::stack![base, modal::view(state)]
            .width(iced::Fill)
            .height(iced::Fill)
            .into(),
        None => base.into(),
    }
}

/// Keyboard subscriptions: `Ctrl+R` reloads the workspace, `Ctrl+S` saves
/// the active tab; `Esc` cancels a modal (a no-op in the reducer when none
/// is open, so the closure stays non-capturing as iced requires).
/// `keyboard::listen` only receives events the widgets ignore, so focused
/// text inputs still let both through.
pub fn subscription(_app: &App) -> Subscription<Message> {
    keyboard::listen().filter_map(|event| match event {
        keyboard::Event::KeyPressed {
            modifiers,
            physical_key,
            ..
        } if modifiers.control() && physical_key == keyboard::key::Code::KeyR => {
            Some(Message::Workspace(WorkspaceMsg::Reload))
        }
        keyboard::Event::KeyPressed {
            modifiers,
            physical_key,
            ..
        } if modifiers.control() && physical_key == keyboard::key::Code::KeyS => {
            Some(Message::Composer(ComposerMsg::Save))
        }
        keyboard::Event::KeyPressed {
            key: keyboard::Key::Named(keyboard::key::Named::Escape),
            ..
        } => Some(Message::Modal(rest_harness::app::ModalMsg::Cancel)),
        _ => None,
    })
}

/// Map one [`Effect`] to an iced `Task`. Blocking storage work runs on
/// `tokio::task::spawn_blocking`; every CRUD effect reloads the workspace
/// and reports through `Message::Workspace(WorkspaceMsg::Loaded(..))`.
fn execute_effect(app: &mut App, effect: Effect) -> Task<Message> {
    match effect {
        Effect::LoadWorkspace(root) => {
            if let Err(error) = app.config.store_last_workspace(&root) {
                eprintln!("could not persist last workspace: {error}");
            }
            load_task(root)
        }
        Effect::PickWorkspaceFolder => Task::perform(
            async {
                match rfd::AsyncFileDialog::new()
                    .set_title("Select workspace folder")
                    .pick_folder()
                    .await
                {
                    Some(handle) => Ok(handle.path().to_path_buf()),
                    None => Err("No folder selected".to_string()),
                }
            },
            |result| Message::Workspace(WorkspaceMsg::PickedRoot(result)),
        ),
        Effect::CreateCollection { name } => match app.state.workspace_root.clone() {
            Some(root) => storage_task(move || {
                let storage = FsWorkspaceStorage;
                storage.create_collection(&root, &name)?;
                load_from_disk(&root)
            }),
            None => no_workspace_task(),
        },
        Effect::CreateFolder { parent, name } => match app.state.workspace_root.clone() {
            Some(root) => storage_task(move || {
                let storage = FsWorkspaceStorage;
                storage.create_folder(&parent, &name)?;
                load_from_disk(&root)
            }),
            None => no_workspace_task(),
        },
        Effect::CreateRequest { parent, name } => match app.state.workspace_root.clone() {
            Some(root) => storage_task(move || {
                let storage = FsWorkspaceStorage;
                storage.create_request(&parent, &rest_harness::app::new_request(&name))?;
                load_from_disk(&root)
            }),
            None => no_workspace_task(),
        },
        Effect::Rename { path, new_name } => {
            let root = app.state.workspace_root.clone();
            storage_task(move || {
                let storage = FsWorkspaceStorage;
                rename_node(&storage, &path, &new_name, root.as_deref())?;
                load_from_disk(&root.unwrap_or_else(|| parent_of(&path)))
            })
        }
        Effect::Duplicate { path } => {
            let root = app.state.workspace_root.clone();
            storage_task(move || {
                let storage = FsWorkspaceStorage;
                if path.is_file() {
                    storage.duplicate_request(&path)?;
                } else {
                    storage.duplicate_folder(&path)?;
                }
                load_from_disk(&root.unwrap_or_else(|| parent_of(&path)))
            })
        }
        Effect::Delete { path } => {
            let root = app.state.workspace_root.clone();
            storage_task(move || {
                let storage = FsWorkspaceStorage;
                delete_node(&storage, &path, root.as_deref())?;
                load_from_disk(&root.unwrap_or_else(|| parent_of(&path)))
            })
        }
        Effect::OpenRequest { path } => {
            // Read the request file off-thread; the reply carries the path
            // and the parsed request (or the error text).
            let task_path = path.clone();
            Task::perform(
                async move {
                    tokio::task::spawn_blocking(move || read_request(&task_path))
                        .await
                        .map_err(|error| format!("Read failed: {error}"))
                        .and_then(|result| result.map_err(|error| error.to_string()))
                },
                move |request| {
                    Message::Tab(TabMsg::Opened {
                        path: path.clone(),
                        request,
                    })
                },
            )
        }
        Effect::SendRequest { tab_id, prepared } => {
            let token = CancellationToken::new();
            app.cancels.insert(tab_id, token.clone());
            let client = app.http_client.clone();
            Task::perform(
                async move {
                    tokio::select! {
                        result = client.execute(&prepared) => match result {
                            Ok(data) => Message::Composer(ComposerMsg::SendFinished {
                                tab_id,
                                result: Ok(data),
                            }),
                            Err(error) => Message::Composer(ComposerMsg::SendFinished {
                                tab_id,
                                result: Err(error.to_string()),
                            }),
                        },
                        _ = token.cancelled() => {
                            Message::Composer(ComposerMsg::SendCancelled { tab_id })
                        }
                    }
                },
                |message| message,
            )
        }
        Effect::CancelSend { tab_id } => {
            if let Some(token) = app.cancels.remove(&tab_id) {
                token.cancel();
            }
            // The select! arm delivers `SendCancelled`.
            Task::none()
        }
        Effect::SaveRequest { path, request } => {
            let root = app
                .state
                .workspace_root
                .clone()
                .unwrap_or_else(|| parent_of(&path));
            storage_task(move || {
                let storage = FsWorkspaceStorage;
                let parent = parent_of(&path);
                storage.save_request(&parent, &request)?;
                load_from_disk(&root)
            })
        }
        Effect::SaveDraft { parent, request } => {
            let root = app.state.workspace_root.clone();
            storage_task(move || {
                let storage = FsWorkspaceStorage;
                // Must NOT overwrite: `create_request` fails if the file
                // already exists.
                storage.create_request(&parent, &request)?;
                load_from_disk(&root.unwrap_or_else(|| parent_of(&parent)))
            })
        }
        Effect::AppendHistory {
            entry,
            secret_values,
        } => match app.state.workspace_root.clone() {
            Some(root) => storage_task(move || {
                // Redaction happens here, on write, against the tab's
                // secret values.
                HistoryStore::new(&root).append_entry(entry, &secret_values)?;
                load_from_disk(&root)
            }),
            None => no_workspace_task(),
        },
        Effect::CopyText(text) => iced::clipboard::write(text),
        Effect::SaveEnvironment {
            collection_dir,
            environment,
        } => {
            let root = app.state.workspace_root.clone();
            storage_task(move || {
                let storage = FsWorkspaceStorage;
                storage.save_environment(&collection_dir, &environment)?;
                load_from_disk(&root.unwrap_or_else(|| parent_of(&collection_dir)))
            })
        }
        Effect::SaveSecrets {
            collection_dir,
            secrets,
        } => {
            let root = app.state.workspace_root.clone();
            storage_task(move || {
                let storage = FsWorkspaceStorage;
                storage.save_secrets(&collection_dir, &secrets)?;
                load_from_disk(&root.unwrap_or_else(|| parent_of(&collection_dir)))
            })
        }
        Effect::Notify(text) => {
            // No dedicated inbound variant for shell notices yet; they ride
            // `Loaded(Err(..))`, which only sets `AppState::status`.
            Task::done(Message::Workspace(WorkspaceMsg::Loaded(Err(text))))
        }
    }
}

/// Rename a node: Requests by file (name field rewritten), Collections when
/// the path is a direct child of the workspace root, Folders otherwise.
fn rename_node(
    storage: &FsWorkspaceStorage,
    path: &Path,
    new_name: &str,
    root: Option<&Path>,
) -> Result<(), rest_harness::Error> {
    if path.is_file() {
        storage.rename_request(path, new_name)?;
    } else if path.parent() == root {
        storage.rename_collection(path, new_name)?;
    } else {
        storage.rename_folder(path, new_name)?;
    }
    Ok(())
}

/// Delete a node: Requests by file, Collections when the path is a direct
/// child of the workspace root, Folders otherwise.
fn delete_node(
    storage: &FsWorkspaceStorage,
    path: &Path,
    root: Option<&Path>,
) -> Result<(), rest_harness::Error> {
    if path.is_file() {
        storage.delete_request(path)?;
    } else if path.parent() == root {
        storage.delete_collection(path)?;
    } else {
        storage.delete_folder(path)?;
    }
    Ok(())
}

fn parent_of(path: &Path) -> PathBuf {
    path.parent().map(Path::to_path_buf).unwrap_or_default()
}

/// The task used when an effect needs a workspace but none is open.
fn no_workspace_task() -> Task<Message> {
    Task::done(Message::Workspace(WorkspaceMsg::Loaded(Err(
        "No workspace is open.".to_string(),
    ))))
}

/// Run blocking storage work off the UI thread; success reloads the
/// workspace (`Loaded(Ok)`), failure reports the error (`Loaded(Err)`).
fn storage_task<F>(work: F) -> Task<Message>
where
    F: FnOnce() -> Result<LoadedWorkspace, rest_harness::Error> + Send + 'static,
{
    Task::perform(
        async move {
            tokio::task::spawn_blocking(work)
                .await
                .map_err(|error| format!("Workspace update failed: {error}"))
                .and_then(|result| result.map_err(|error| error.to_string()))
        },
        |result| Message::Workspace(WorkspaceMsg::Loaded(result)),
    )
}

/// Load the workspace at `root` from disk (with per-request methods parsed).
fn load_task(root: PathBuf) -> Task<Message> {
    storage_task(move || load_from_disk(&root))
}

/// Load a [`LoadedWorkspace`] synchronously (caller runs it on a blocking
/// thread).
fn load_from_disk(root: &Path) -> Result<LoadedWorkspace, rest_harness::Error> {
    let storage = FsWorkspaceStorage;
    let workspace = storage.load_workspace(root)?;
    let request_methods = collect_request_methods(&workspace);
    Ok(LoadedWorkspace {
        workspace,
        request_methods,
    })
}

/// Parse every request file in the workspace once, mapping path → method
/// for the sidebar's method badges (`RequestEntry` carries only name/path).
fn collect_request_methods(workspace: &Workspace) -> BTreeMap<PathBuf, HttpMethod> {
    let mut methods = BTreeMap::new();
    let mut folders: Vec<&Folder> = Vec::new();
    for collection in &workspace.collections {
        for entry in &collection.requests {
            record_method(entry, &mut methods);
        }
        folders.extend(collection.folders.iter());
    }
    while let Some(folder) = folders.pop() {
        for entry in &folder.requests {
            record_method(entry, &mut methods);
        }
        folders.extend(folder.folders.iter());
    }
    methods
}

fn record_method(entry: &RequestEntry, methods: &mut BTreeMap<PathBuf, HttpMethod>) {
    if let Ok(request) = read_request(&entry.path) {
        methods.insert(entry.path.clone(), request.method);
    }
}

/// The bottom status strip: the reducer's last error/info line.
fn status_strip(state: &AppState) -> Element<'_, Message> {
    use iced::widget::{container, text};
    use iced::Theme;

    let status = state.status.clone().unwrap_or_default();
    container(text(status).size(12))
        .width(iced::Fill)
        .padding(iced::padding::all(6))
        .style(|theme: &Theme| container::Style {
            background: Some(theme.extended_palette().background.weak.color.into()),
            ..container::Style::default()
        })
        .into()
}
