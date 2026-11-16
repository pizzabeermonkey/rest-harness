//! The `iced` shell for REST Harness.
//!
//! Thin adapter between the UI-agnostic app core (`rest_harness::app`) and
//! `iced` 0.14: maps `Message` → reducer → `Effect`s → `iced::Task`s, and
//! renders top bar, sidebar and modal overlay. All `iced` `use` statements
//! live here (and in `src/main.rs`); the lib crate stays `iced`-free.
//!
//! # Extension points for the next increment (Request composer)
//!
//! - **Messages**: `Message` is nested. Add `Composer(ComposerMsg)` /
//!   `Tab(TabMsg)` variants to `rest_harness::app::Message`, a match arm in
//!   `AppState::update`, and a wrapper here if it needs shell work.
//! - **Open intent**: clicking a Request row sends
//!   `Sidebar(SidebarMsg::NodeActivated(path))`, which records
//!   `AppState::last_opened`. The composer consumes `last_opened` to open
//!   the request; the placeholder pane in [`right_pane`] shows the notice.
//! - **Effects**: new shell-side work = new `Effect` variant in
//!   `rest_harness::app::Effect`, mapped to `Task`s in
//!   [`execute_effect`] below. CRUD effects all funnel through
//!   [`storage_task`], which executes the work off the UI thread and
//!   reloads the workspace from disk on success (`Loaded(Ok)`) or reports
//!   the error into the status strip (`Loaded(Err)`).
//! - **Requests on disk**: `Effect::CreateRequest` writes
//!   [`new_request(name)`] (empty URL, GET). The composer replaces the URL
//!   via the storage layer afterwards.
//! - **Placeholder**: [`right_pane`] is the pane to replace; `topbar.rs`
//!   has the disabled `+ New` button waiting for the composer.
//!
//! [`new_request(name)`]: rest_harness::app::new_request

pub mod modal;
pub mod sidebar;
pub mod topbar;

use iced::{keyboard, Element, Subscription, Task};
use rest_harness::app::{AppState, Effect, LoadedWorkspace, Message, WorkspaceMsg};
use rest_harness::config::AppConfig;
use rest_harness::domain::{Folder, HttpMethod, RequestEntry, Workspace};
use rest_harness::storage::{read_request, FsWorkspaceStorage, WorkspaceStorage};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

/// The iced program state: the app core plus persisted config.
pub struct App {
    /// UI-agnostic application state (reducer-owned).
    pub state: AppState,
    /// Persisted app config (last workspace path bookkeeping).
    config: AppConfig,
}

impl App {
    fn new(state: AppState, config: AppConfig) -> Self {
        App { state, config }
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

/// Shell update: run the reducer, then execute every returned effect.
pub fn update(app: &mut App, message: Message) -> Task<Message> {
    let effects = app.state.update(message);
    let tasks: Vec<Task<Message>> = effects
        .into_iter()
        .map(|effect| execute_effect(app, effect))
        .collect();
    Task::batch(tasks)
}

/// Top-level view: workspace bar, sidebar + placeholder pane, status strip,
/// with the modal overlay stacked on top.
pub fn view(app: &App) -> Element<'_, Message> {
    let state = &app.state;
    let base = iced::widget::column![
        topbar::view(state),
        iced::widget::row![sidebar::view(state), right_pane(state)].height(iced::Fill),
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

/// Keyboard subscriptions: `Ctrl+R` reloads the workspace; `Esc` cancels a
/// modal (a no-op in the reducer when none is open, so the closure stays
/// non-capturing as iced requires). `keyboard::listen` only receives events
/// the widgets ignore, so focused text inputs still let both through.
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

/// Uppercase label of an HTTP method (for badges and notices).
pub fn method_label(method: HttpMethod) -> &'static str {
    match method {
        HttpMethod::Get => "GET",
        HttpMethod::Post => "POST",
        HttpMethod::Put => "PUT",
        HttpMethod::Patch => "PATCH",
        HttpMethod::Delete => "DELETE",
        HttpMethod::Head => "HEAD",
        HttpMethod::Options => "OPTIONS",
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

/// The right pane placeholder for the Request composer (next increment).
/// Shows a one-line notice naming the last activated request / selected
/// history entry so activation is visibly wired.
fn right_pane(state: &AppState) -> Element<'_, Message> {
    use iced::widget::{column, container, text};
    use iced::Theme;

    let mut pane = column![].spacing(4);
    if let Some(path) = &state.last_opened {
        let name = path
            .file_stem()
            .and_then(|stem| stem.to_str())
            .unwrap_or("unknown");
        pane = pane.push(
            text(format!("Opened request: {name}"))
                .size(12)
                .style(text::secondary),
        );
    } else if let Some(index) = state.history_selected {
        if let Some(entry) = state.workspace.as_ref().and_then(|w| w.history.get(index)) {
            pane = pane.push(
                text(format!(
                    "Selected history: {} {}",
                    method_label(entry.request.method),
                    entry.request.url
                ))
                .size(12)
                .style(text::secondary),
            );
        }
    }

    pane = pane.push(
        container(
            container(
                text("Request composer — coming in the next increment").style(text::secondary),
            )
            .center(iced::Fill),
        )
        .width(iced::Fill)
        .height(iced::Fill)
        .style(|theme: &Theme| container::Style {
            border: iced::border::rounded(4)
                .width(1)
                .color(theme.extended_palette().secondary.weak.color),
            ..container::Style::default()
        }),
    );

    pane.width(iced::Fill).height(iced::Fill).padding(8).into()
}
