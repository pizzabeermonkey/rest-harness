//! The `gpui` + `gpui-ui-kit` shell for REST Harness.
//!
//! Thin adapter between the UI-agnostic app core (`rest_harness::app`) and
//! the [`gpui`] toolkit: maps `Message` → reducer → [`Effect`]s → async
//! work on a dedicated tokio runtime, and renders the workspace bar, the
//! request sidebar, the composer (method + URL + Params/Headers/Body tabs,
//! environment selector) and the response viewer with `gpui-ui-kit`
//! components (Button, Input, Badge, Heading, Text, Tabs, Select,
//! Checkbox), themed by `gpui-ui-kit`'s [`ThemeState`] and laid out with
//! `gpui-design`'s [`DesignSystem`] spacing tokens.
//!
//! All `gpui` `use` statements live here (and in `src/main.rs`); the lib
//! crate stays GUI-free.
//!
//! # Async model
//!
//! gpui runs its own foreground executor; the HTTP client (`reqwest`) and
//! the blocking storage work want tokio. The shell owns one process-wide
//! tokio runtime (see [`tokio_handle`]) and bridges results back into the
//! view by awaiting the tokio `JoinHandle` inside a `cx.spawn` future —
//! the completed task simply wakes the gpui executor.

use gpui::prelude::*;
use gpui::*;
use gpui_design::DesignSystem;
use gpui_ui_kit::{
    Badge, BadgeVariant, Button, ButtonSize, ButtonVariant, Checkbox, HStack, Heading, Input,
    InputSize, Select, SelectOption, SelectSize, StackSpacing, TabItem, Tabs, Text, TextSize,
    ThemeExt, ThemeState, ThemeVariant,
};
use rest_harness::app::{
    AppState, BodyMode, BodyView, ComposerMsg, Effect, EnvScope, EnvSelection, LoadedWorkspace,
    Message, Modal, ModalMsg, ResponseState, ResponseView, SidebarMsg, StatusClass, SubTab, Tab,
    TabMsg, WorkspaceMsg,
};
use rest_harness::config::AppConfig;
use rest_harness::domain::{Folder, HttpMethod, Request, Workspace};
use rest_harness::history::HistoryStore;
use rest_harness::http::{HttpClient as _, ReqwestHttpClient};
use rest_harness::storage::{read_request, FsWorkspaceStorage, WorkspaceStorage};
use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};
use tokio_util::sync::CancellationToken;

actions!(
    rest_harness,
    [ReloadWorkspace, NewDraft, SendActive, SaveActive]
);

/// The gpui program state: the app core plus persisted config, the HTTP
/// client, the in-flight cancellation registry and the design tokens.
pub struct HarnessApp {
    /// UI-agnostic application state (reducer-owned).
    state: AppState,
    /// Persisted app config (last workspace path bookkeeping).
    config: AppConfig,
    /// HTTP client executing composer sends.
    http_client: ReqwestHttpClient,
    /// Cancellation tokens of in-flight sends, keyed by tab id.
    cancels: HashMap<u64, CancellationToken>,
    /// `gpui-design` spacing/layout tokens used for the raw-`div` chrome.
    design: DesignSystem,
    /// Which controlled [`Select`] dropdowns are open (the component is
    /// fully controlled: its open state lives in the view).
    open: Dropdowns,
}

/// Open/closed state of the controlled [`Select`] dropdowns.
#[derive(Debug, Default)]
struct Dropdowns {
    method: bool,
    env: bool,
    body_mode: bool,
    save_target: bool,
}

/// Element ids of one key/value editor row (params or headers).
type RowIds = (
    (&'static str, usize),
    (&'static str, usize),
    (&'static str, usize),
    (&'static str, usize),
);

impl HarnessApp {
    fn new(config: AppConfig, root: PathBuf) -> Self {
        HarnessApp {
            state: AppState::new(Some(root)),
            config,
            http_client: ReqwestHttpClient::default(),
            cancels: HashMap::new(),
            design: DesignSystem::neutral(),
            open: Dropdowns::default(),
        }
    }
}

/// Run the desktop application: one dark-themed window with the composer.
pub fn run() {
    let (config, root) = boot_workspace();
    let weak_view: Arc<Mutex<Option<WeakEntity<HarnessApp>>>> = Arc::new(Mutex::new(None));

    Application::new().run(move |cx| {
        cx.set_global(ThemeState::with_variant(ThemeVariant::Dark));

        let bounds = Bounds::centered(None, size(px(1280.0), px(820.0)), cx);
        let window = cx
            .open_window(
                WindowOptions {
                    window_bounds: Some(WindowBounds::Windowed(bounds)),
                    titlebar: Some(TitlebarOptions {
                        title: Some("REST Harness".into()),
                        ..Default::default()
                    }),
                    ..Default::default()
                },
                {
                    let weak_view = weak_view.clone();
                    move |_, cx| {
                        let view = cx.new(|_| HarnessApp::new(config.clone(), root.clone()));
                        *weak_view.lock().unwrap() = Some(view.downgrade());
                        view
                    }
                },
            )
            .unwrap();

        // Kick off the initial workspace load.
        window
            .update(cx, |app, _, cx| {
                app.dispatch(Message::Workspace(WorkspaceMsg::Reload), cx);
            })
            .unwrap();

        // Global keyboard actions.
        let view = weak_view.lock().unwrap().clone().unwrap();
        let weak = view.clone();
        cx.on_action::<ReloadWorkspace>(move |_, cx| {
            weak.update(cx, |app, cx| {
                app.dispatch(Message::Workspace(WorkspaceMsg::Reload), cx);
            })
            .ok();
        });
        let weak = view.clone();
        cx.on_action::<NewDraft>(move |_, cx| {
            weak.update(cx, |app, cx| {
                app.dispatch(Message::Tab(TabMsg::NewDraft), cx);
            })
            .ok();
        });
        let weak = view.clone();
        cx.on_action::<SendActive>(move |_, cx| {
            weak.update(cx, |app, cx| {
                app.dispatch(Message::Composer(ComposerMsg::Send), cx);
            })
            .ok();
        });
        let weak = view.clone();
        cx.on_action::<SaveActive>(move |_, cx| {
            weak.update(cx, |app, cx| {
                app.dispatch(Message::Composer(ComposerMsg::Save), cx);
            })
            .ok();
        });
        cx.bind_keys([
            KeyBinding::new("cmd-r", ReloadWorkspace, None),
            KeyBinding::new("ctrl-r", ReloadWorkspace, None),
            KeyBinding::new("cmd-n", NewDraft, None),
            KeyBinding::new("ctrl-n", NewDraft, None),
            KeyBinding::new("cmd-enter", SendActive, None),
            KeyBinding::new("ctrl-enter", SendActive, None),
            KeyBinding::new("cmd-s", SaveActive, None),
            KeyBinding::new("ctrl-s", SaveActive, None),
        ]);

        cx.activate(true);
    });
}

/// Resolve the initial workspace from the persisted config (seeding the
/// sample collection on first run), as the GUI shell always did.
fn boot_workspace() -> (AppConfig, PathBuf) {
    let config = AppConfig::load().unwrap_or_default();
    let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    let root = config
        .resolve_workspace(&cwd)
        .unwrap_or_else(|_| cwd.join("collections"));
    (config, root)
}

/// The process-wide tokio runtime backing `reqwest` and blocking storage
/// work. Kept in a `OnceLock` so it lives for the whole process.
fn tokio_handle() -> tokio::runtime::Handle {
    static TOKIO: OnceLock<tokio::runtime::Runtime> = OnceLock::new();
    TOKIO
        .get_or_init(|| {
            tokio::runtime::Builder::new_multi_thread()
                .enable_all()
                .build()
                .expect("could not start the tokio runtime")
        })
        .handle()
        .clone()
}

impl HarnessApp {
    /// Pure reducer dispatch plus effect execution.
    fn dispatch(&mut self, message: Message, cx: &mut Context<Self>) {
        // A send that finished (or was cancelled) no longer needs its token.
        match &message {
            Message::Composer(ComposerMsg::SendFinished { tab_id, .. })
            | Message::Composer(ComposerMsg::SendCancelled { tab_id }) => {
                self.cancels.remove(tab_id);
            }
            _ => {}
        }

        let previous_tab = self.state.active_tab;
        for effect in self.state.update(message) {
            self.execute(effect, cx);
        }

        // Close controlled dropdowns when the focused tab changes or when
        // no modal is open anymore.
        if previous_tab != self.state.active_tab || self.state.modal.is_none() {
            self.open = Dropdowns::default();
        }
    }

    /// Map one [`Effect`] to async work on the tokio runtime. Every CRUD
    /// effect reloads the workspace and reports through
    /// `Message::Workspace(WorkspaceMsg::Loaded(..))`.
    fn execute(&mut self, effect: Effect, cx: &mut Context<Self>) {
        match effect {
            Effect::LoadWorkspace(root) => {
                if let Err(error) = self.config.store_last_workspace(&root) {
                    eprintln!("could not persist last workspace: {error}");
                }
                self.storage_job(move || load_from_disk(&root), cx);
            }
            Effect::PickWorkspaceFolder => self.notify(
                "Pick a workspace by starting rest-harness inside its folder.".to_string(),
                cx,
            ),
            Effect::CreateCollection { name } => match self.state.workspace_root.clone() {
                Some(root) => self.storage_job(
                    move || {
                        FsWorkspaceStorage.create_collection(&root, &name)?;
                        load_from_disk(&root)
                    },
                    cx,
                ),
                None => self.no_workspace(cx),
            },
            Effect::CreateFolder { parent, name } => match self.state.workspace_root.clone() {
                Some(root) => self.storage_job(
                    move || {
                        FsWorkspaceStorage.create_folder(&parent, &name)?;
                        load_from_disk(&root)
                    },
                    cx,
                ),
                None => self.no_workspace(cx),
            },
            Effect::CreateRequest { parent, name } => match self.state.workspace_root.clone() {
                Some(root) => self.storage_job(
                    move || {
                        FsWorkspaceStorage.create_request(&parent, &new_request(&name))?;
                        load_from_disk(&root)
                    },
                    cx,
                ),
                None => self.no_workspace(cx),
            },
            Effect::Rename { path, new_name } => {
                let root = self.state.workspace_root.clone();
                self.storage_job(
                    move || {
                        let storage = FsWorkspaceStorage;
                        rename_node(&storage, &path, &new_name, root.as_deref())?;
                        load_from_disk(&root.unwrap_or_else(|| parent_of(&path)))
                    },
                    cx,
                );
            }
            Effect::Duplicate { path } => {
                let root = self.state.workspace_root.clone();
                self.storage_job(
                    move || {
                        let storage = FsWorkspaceStorage;
                        if path.is_file() {
                            storage.duplicate_request(&path)?;
                        } else {
                            storage.duplicate_folder(&path)?;
                        }
                        load_from_disk(&root.unwrap_or_else(|| parent_of(&path)))
                    },
                    cx,
                );
            }
            Effect::Delete { path } => {
                let root = self.state.workspace_root.clone();
                self.storage_job(
                    move || {
                        let storage = FsWorkspaceStorage;
                        delete_node(&storage, &path, root.as_deref())?;
                        load_from_disk(&root.unwrap_or_else(|| parent_of(&path)))
                    },
                    cx,
                );
            }
            Effect::OpenRequest { path } => {
                let read = path.clone();
                let task = tokio_handle().spawn_blocking(move || read_request(&read));
                cx.spawn(async move |this, cx| {
                    let request = task
                        .await
                        .map_err(|error| format!("Read failed: {error}"))
                        .and_then(|result| result.map_err(|error| error.to_string()));
                    let message = Message::Tab(TabMsg::Opened { path, request });
                    this.update(cx, |app, cx| app.dispatch(message, cx)).ok();
                })
                .detach();
            }
            Effect::SendRequest { tab_id, prepared } => {
                let token = CancellationToken::new();
                self.cancels.insert(tab_id, token.clone());
                let client = self.http_client.clone();
                let task = tokio_handle().spawn(async move {
                    tokio::select! {
                        result = client.execute(&prepared) => ComposerMsg::SendFinished {
                            tab_id,
                            result: result.map_err(|error| error.to_string()),
                        },
                        _ = token.cancelled() => ComposerMsg::SendCancelled { tab_id },
                    }
                });
                cx.spawn(async move |this, cx| {
                    if let Ok(message) = task.await {
                        this.update(cx, |app, cx| app.dispatch(Message::Composer(message), cx))
                            .ok();
                    }
                })
                .detach();
            }
            Effect::CancelSend { tab_id } => {
                if let Some(token) = self.cancels.remove(&tab_id) {
                    token.cancel();
                }
                // The select! arm delivers `SendCancelled`.
            }
            Effect::SaveRequest { path, request } => {
                let root = self
                    .state
                    .workspace_root
                    .clone()
                    .unwrap_or_else(|| parent_of(&path));
                self.storage_job(
                    move || {
                        let parent = parent_of(&path);
                        FsWorkspaceStorage.save_request(&parent, &request)?;
                        load_from_disk(&root)
                    },
                    cx,
                );
            }
            Effect::SaveDraft { parent, request } => {
                let root = self.state.workspace_root.clone();
                self.storage_job(
                    move || {
                        // Must NOT overwrite: `create_request` fails when the
                        // file already exists.
                        FsWorkspaceStorage.create_request(&parent, &request)?;
                        load_from_disk(&root.unwrap_or_else(|| parent_of(&parent)))
                    },
                    cx,
                );
            }
            Effect::AppendHistory {
                entry,
                secret_values,
            } => match self.state.workspace_root.clone() {
                Some(root) => self.storage_job(
                    move || {
                        // Redaction happens here, on write, against the tab's
                        // secret values.
                        HistoryStore::new(&root).append_entry(entry, &secret_values)?;
                        load_from_disk(&root)
                    },
                    cx,
                ),
                None => self.no_workspace(cx),
            },
            Effect::CopyText(text) => {
                cx.write_to_clipboard(ClipboardItem::new_string(text));
            }
            Effect::SaveEnvironment {
                collection_dir,
                environment,
            } => {
                let root = self.state.workspace_root.clone();
                self.storage_job(
                    move || {
                        FsWorkspaceStorage.save_environment(&collection_dir, &environment)?;
                        load_from_disk(&root.unwrap_or_else(|| parent_of(&collection_dir)))
                    },
                    cx,
                );
            }
            Effect::SaveSecrets {
                collection_dir,
                secrets,
            } => {
                let root = self.state.workspace_root.clone();
                self.storage_job(
                    move || {
                        FsWorkspaceStorage.save_secrets(&collection_dir, &secrets)?;
                        load_from_disk(&root.unwrap_or_else(|| parent_of(&collection_dir)))
                    },
                    cx,
                );
            }
            Effect::Notify(text) => self.notify(text, cx),
        }
    }

    /// Report a shell-side failure into the reducer's status line.
    fn notify(&mut self, text: String, cx: &mut Context<Self>) {
        self.dispatch(Message::Workspace(WorkspaceMsg::Loaded(Err(text))), cx);
    }

    /// The message used when an effect needs a workspace but none is open.
    fn no_workspace(&mut self, cx: &mut Context<Self>) {
        self.notify("No workspace is open.".to_string(), cx);
    }

    /// Run blocking storage work on the tokio runtime; success reloads the
    /// workspace (`Loaded(Ok)`), failure reports the error (`Loaded(Err)`).
    fn storage_job<F>(&self, work: F, cx: &mut Context<Self>)
    where
        F: FnOnce() -> Result<LoadedWorkspace, rest_harness::Error> + Send + 'static,
    {
        let task = tokio_handle().spawn_blocking(work);
        cx.spawn(async move |this, cx| {
            let result = task
                .await
                .map_err(|error| format!("Workspace update failed: {error}"))
                .and_then(|result| result.map_err(|error| error.to_string()));
            let message = Message::Workspace(WorkspaceMsg::Loaded(result));
            this.update(cx, |app, cx| app.dispatch(message, cx)).ok();
        })
        .detach();
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

/// The default Request the shell creates for `Effect::CreateRequest` (the
/// composer fills in method/URL afterwards).
fn new_request(name: &str) -> Request {
    Request {
        name: name.to_string(),
        ..Request::default()
    }
}

/// Load a [`LoadedWorkspace`] synchronously (caller runs it on a blocking
/// thread).
fn load_from_disk(root: &Path) -> Result<LoadedWorkspace, rest_harness::Error> {
    let workspace = FsWorkspaceStorage.load_workspace(root)?;
    let request_methods = collect_request_methods(&workspace);
    Ok(LoadedWorkspace {
        workspace,
        request_methods,
    })
}

/// Parse every request file in the workspace once, mapping path → method
/// for the sidebar's method prefixes (`RequestEntry` carries only name/path).
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

fn record_method(
    entry: &rest_harness::domain::RequestEntry,
    methods: &mut BTreeMap<PathBuf, HttpMethod>,
) {
    if let Ok(request) = read_request(&entry.path) {
        methods.insert(entry.path.clone(), request.method);
    }
}

impl Render for HarnessApp {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        let spacing = self.design.spacing;
        let modal = self.state.modal.clone();
        let overlay = modal.map(|modal| self.modal_overlay(modal, cx).into_any_element());

        div()
            .id("root")
            .size_full()
            .flex()
            .flex_col()
            .bg(theme.background)
            .text_color(theme.text_primary)
            // Top bar
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(spacing.control_gap))
                    .px(px(spacing.card_padding))
                    .py(px(spacing.control_padding_y))
                    .border_b_1()
                    .border_color(theme.muted)
                    .child(Heading::h3("REST Harness"))
                    .child(
                        Text::new(workspace_label(&self.state))
                            .muted(true)
                            .truncate(true)
                            .size(TextSize::Sm),
                    )
                    .child(div().flex_1())
                    .child(
                        Button::new("new-draft", "+ Draft")
                            .variant(ButtonVariant::Secondary)
                            .size(ButtonSize::Sm)
                            .on_click(dispatch_button(cx, Message::Tab(TabMsg::NewDraft))),
                    )
                    .child(
                        Button::new("reload", "Reload")
                            .variant(ButtonVariant::Secondary)
                            .size(ButtonSize::Sm)
                            .on_click(dispatch_button(
                                cx,
                                Message::Workspace(WorkspaceMsg::Reload),
                            )),
                    ),
            )
            // Main area: sidebar + composer
            .child(
                div()
                    .id("main")
                    .flex_1()
                    .min_h_0()
                    .flex()
                    .child(self.sidebar(cx).into_any_element())
                    .child(self.composer_pane(cx).into_any_element()),
            )
            // Status strip
            .child(
                div()
                    .px(px(spacing.card_padding))
                    .py(px(spacing.control_padding_y))
                    .border_t_1()
                    .border_color(theme.muted)
                    .child(
                        Text::new(self.state.status.clone().unwrap_or_else(|| {
                            "Ready. Ctrl+N draft · Ctrl+S save · Ctrl+Enter send · Ctrl+R reload"
                                .to_string()
                        }))
                        .muted(true)
                        .size(TextSize::Sm),
                    ),
            )
            .when_some(overlay, |root, overlay| root.child(overlay))
    }
}

impl HarnessApp {
    /// The request sidebar: filter, the flat request list (collections and
    /// nested folders walked recursively) and the history rows.
    fn sidebar(&self, cx: &mut Context<Self>) -> Div {
        let theme = cx.theme();
        let spacing = self.design.spacing;

        let mut list = div()
            .id("sidebar-scroll")
            .flex_1()
            .min_h_0()
            .overflow_y_scroll()
            .flex()
            .flex_col()
            .gap_1();

        if let Some(workspace) = &self.state.workspace {
            let needle = self.state.filter.trim().to_ascii_lowercase();
            for (index, (label, path)) in collect_requests(workspace)
                .into_iter()
                .filter(|(label, _)| {
                    needle.is_empty() || label.to_ascii_lowercase().contains(&needle)
                })
                .enumerate()
            {
                let method = self
                    .state
                    .request_methods
                    .get(&path)
                    .map(|method| method.label())
                    .unwrap_or("???")
                    .to_string();
                list = list.child(
                    Button::new(("request-row", index), format!("{method}  {label}"))
                        .variant(ButtonVariant::Secondary)
                        .size(ButtonSize::Sm)
                        .full_width(true)
                        .on_click(dispatch_button(
                            cx,
                            Message::Sidebar(SidebarMsg::NodeActivated(path)),
                        )),
                );
            }

            if !workspace.history.is_empty() {
                list = list
                    .child(div().h_2())
                    .child(Heading::h4("History"))
                    .children(workspace.history.iter().enumerate().map(|(index, entry)| {
                        Button::new(
                            ("history-row", index),
                            format!(
                                "{} {} — {}",
                                entry.request.method.label(),
                                entry.request.url,
                                entry.response.status_code
                            ),
                        )
                        .variant(ButtonVariant::Secondary)
                        .size(ButtonSize::Sm)
                        .full_width(true)
                        .on_click(dispatch_button(
                            cx,
                            Message::Sidebar(SidebarMsg::HistoryEntrySelected(index)),
                        ))
                    }));
            }
        }

        div()
            .w(px(320.0))
            .h_full()
            .flex()
            .flex_col()
            .gap_2()
            .px(px(spacing.card_padding))
            .py(px(spacing.control_padding_y))
            .border_r_1()
            .border_color(theme.muted)
            .child(
                Input::new("sidebar-filter")
                    .value(self.state.filter.clone())
                    .placeholder("Filter requests…")
                    .size(InputSize::Sm)
                    .on_text_change(dispatch_text(cx, |text| {
                        Message::Sidebar(SidebarMsg::FilterChanged(text))
                    })),
            )
            .child(Heading::h4("Requests"))
            .child(list)
    }

    /// The composer pane: tab strip, editor, response viewer.
    fn composer_pane(&self, cx: &mut Context<Self>) -> Div {
        let spacing = self.design.spacing;

        let mut pane = div()
            .flex_1()
            .min_h_0()
            .min_w_0()
            .flex()
            .flex_col()
            .px(px(spacing.card_padding))
            .py(px(spacing.control_padding_y))
            .gap_2();

        pane = pane.child(self.tab_strip(cx));

        match self.state.active_tab().cloned() {
            Some(tab) => {
                pane = pane
                    .child(self.editor_header(cx, &tab))
                    .child(self.editor_body(cx, &tab))
                    .child(self.response_viewer(cx, &tab));
            }
            None => {
                pane = pane.child(
                    div().flex_1().flex().items_center().justify_center().child(
                        Text::new(
                            "Open a request from the sidebar, or press Ctrl+N for a new draft.",
                        )
                        .muted(true),
                    ),
                );
            }
        }

        pane
    }

    /// The strip of open tabs (closeable, keyboard navigable).
    fn tab_strip(&self, cx: &mut Context<Self>) -> Tabs {
        let entity = cx.entity();
        let close_entity = entity.clone();
        let items: Vec<TabItem> = self
            .state
            .tabs
            .iter()
            .map(|tab| {
                TabItem::new(SharedString::from(format!("tab-{}", tab.id)), tab.title())
                    .closeable(true)
            })
            .collect();
        let selected_index = self
            .state
            .tabs
            .iter()
            .position(|tab| Some(tab.id) == self.state.active_tab)
            .unwrap_or(0);

        Tabs::new("open-tabs")
            .tabs(items)
            .selected_index(selected_index)
            .on_change(move |index, _, cx| {
                entity.update(cx, |app, cx| {
                    if let Some(tab) = app.state.tabs.get(index) {
                        app.dispatch(Message::Tab(TabMsg::Select(tab.id)), cx);
                    }
                })
            })
            .on_close(move |id: &SharedString, _, cx| {
                if let Some(tab_id) = id.strip_prefix("tab-").and_then(|raw| raw.parse().ok()) {
                    close_entity.update(cx, |app, cx| {
                        app.dispatch(Message::Tab(TabMsg::Close(tab_id)), cx);
                    });
                }
            })
    }

    /// Method + URL + Send, then the environment selector + Save.
    fn editor_header(&self, cx: &mut Context<Self>, tab: &Tab) -> AnyElement {
        let theme = cx.theme();

        let url_input = Input::new(("url-input", tab.id as usize))
            .value(tab.request.url.clone())
            .placeholder("https://…")
            .size(InputSize::Sm)
            .on_text_change(dispatch_tab_text(cx, tab.id, |text| {
                Message::Composer(ComposerMsg::UrlChanged(text))
            }))
            .on_change(dispatch_tab_enter(cx, tab.id));

        let send_button = if tab.in_flight {
            Button::new("send", "Cancel")
                .variant(ButtonVariant::Destructive)
                .size(ButtonSize::Sm)
                .on_click(dispatch_button(
                    cx,
                    Message::Composer(ComposerMsg::CancelSend),
                ))
        } else {
            Button::new("send", "Send")
                .variant(ButtonVariant::Primary)
                .size(ButtonSize::Sm)
                .on_click(dispatch_button(cx, Message::Composer(ComposerMsg::Send)))
        };

        div()
            .flex()
            .flex_col()
            .gap_2()
            // Row 1: method + URL + Send
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_2()
                    .child(method_select(cx, tab, self.open.method))
                    .child(url_input)
                    .child(send_button),
            )
            // Row 2: environment selector + Save + send error
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_2()
                    .child(env_select(cx, tab, self.open.env, self.env_options()))
                    .child(
                        Button::new("save", "Save")
                            .variant(ButtonVariant::Secondary)
                            .size(ButtonSize::Sm)
                            .on_click(dispatch_button(cx, Message::Composer(ComposerMsg::Save))),
                    )
                    .when_some(tab.send_error.clone(), |row, error| {
                        row.child(
                            Text::new(error)
                                .color(theme.error)
                                .size(TextSize::Sm)
                                .truncate(true),
                        )
                    }),
            )
            .into_any_element()
    }

    /// The Params / Headers / Body sub-tabs with their row editors.
    fn editor_body(&self, cx: &mut Context<Self>, tab: &Tab) -> AnyElement {
        let theme = cx.theme();
        let spacing = self.design.spacing;
        let subtab_items = vec![
            TabItem::new("sub-params", "Params"),
            TabItem::new("sub-headers", "Headers"),
            TabItem::new("sub-body", "Body"),
        ];
        let selected_subtab = match tab.active_subtab {
            SubTab::Params => 0,
            SubTab::Headers => 1,
            SubTab::Body => 2,
        };

        let mut content = div()
            .id("editor-scroll")
            .flex_1()
            .min_h_0()
            .overflow_y_scroll()
            .flex()
            .flex_col()
            .gap_2()
            .p(px(spacing.card_padding))
            .border_1()
            .border_color(theme.muted)
            .rounded(px(self.design.corners.md));

        match tab.active_subtab {
            SubTab::Params => {
                content = content
                    .children(
                        tab.request
                            .params
                            .iter()
                            .enumerate()
                            .map(|(index, row)| self.key_value_row(cx, index, row, true)),
                    )
                    .child(
                        Button::new("add-param", "+ Add param")
                            .variant(ButtonVariant::Secondary)
                            .size(ButtonSize::Sm)
                            .on_click(dispatch_button(
                                cx,
                                Message::Composer(ComposerMsg::ParamAdded),
                            )),
                    );
            }
            SubTab::Headers => {
                content = content
                    .children(
                        tab.request
                            .headers
                            .iter()
                            .enumerate()
                            .map(|(index, row)| self.key_value_row(cx, index, row, false)),
                    )
                    .child(
                        Button::new("add-header", "+ Add header")
                            .variant(ButtonVariant::Secondary)
                            .size(ButtonSize::Sm)
                            .on_click(dispatch_button(
                                cx,
                                Message::Composer(ComposerMsg::HeaderAdded),
                            )),
                    );
            }
            SubTab::Body => {
                content = content
                    .child(body_mode_select(cx, tab, self.open.body_mode))
                    .when(tab.body_mode() != BodyMode::None, |content| {
                        content.child(
                            Input::new(("body-input", tab.id as usize))
                                .value(tab.body_text())
                                .placeholder("Request body…")
                                .size(InputSize::Sm)
                                .on_text_change(dispatch_tab_text(cx, tab.id, |text| {
                                    Message::Composer(ComposerMsg::BodyTextChanged(text))
                                })),
                        )
                    })
                    .child(
                        Text::new(
                            "Body editing is single-line in this build; clipboard paste works.",
                        )
                        .muted(true)
                        .size(TextSize::Xs),
                    );
            }
        }

        div()
            .flex_1()
            .min_h_0()
            .flex()
            .flex_col()
            .gap_1()
            .child(
                Tabs::new("subtabs")
                    .tabs(subtab_items)
                    .selected_index(selected_subtab)
                    .on_change(dispatch_tab_index(cx, |index| {
                        Message::Composer(ComposerMsg::SubTabSelected(subtab_of(index)))
                    })),
            )
            .child(content)
            .into_any_element()
    }

    /// One key/value editor row (params or headers): enabled checkbox,
    /// key input, value input and a remove button.
    fn key_value_row(
        &self,
        cx: &mut Context<Self>,
        index: usize,
        row: &rest_harness::domain::KeyValueRow,
        is_param: bool,
    ) -> AnyElement {
        let (key_id, value_id, enabled_id, remove_id): RowIds = if is_param {
            (
                ("param-key", index),
                ("param-value", index),
                ("param-enabled", index),
                ("param-remove", index),
            )
        } else {
            (
                ("header-key", index),
                ("header-value", index),
                ("header-enabled", index),
                ("header-remove", index),
            )
        };

        let key_input = Input::new(key_id)
            .value(row.key.clone())
            .placeholder("Key")
            .size(InputSize::Sm)
            .on_text_change(dispatch_text(cx, move |text| {
                if is_param {
                    Message::Composer(ComposerMsg::ParamKeyChanged(index, text))
                } else {
                    Message::Composer(ComposerMsg::HeaderKeyChanged(index, text))
                }
            }));
        let value_input = Input::new(value_id)
            .value(row.value.clone())
            .placeholder("Value")
            .size(InputSize::Sm)
            .on_text_change(dispatch_text(cx, move |text| {
                if is_param {
                    Message::Composer(ComposerMsg::ParamValueChanged(index, text))
                } else {
                    Message::Composer(ComposerMsg::HeaderValueChanged(index, text))
                }
            }));
        let toggle = dispatch_button(
            cx,
            if is_param {
                Message::Composer(ComposerMsg::ParamToggled(index))
            } else {
                Message::Composer(ComposerMsg::HeaderToggled(index))
            },
        );
        let checkbox = Checkbox::new(enabled_id)
            .checked(row.enabled)
            .on_change(move |_, window, cx| toggle(window, cx));
        let remove = dispatch_button(
            cx,
            if is_param {
                Message::Composer(ComposerMsg::ParamRemoved(index))
            } else {
                Message::Composer(ComposerMsg::HeaderRemoved(index))
            },
        );

        HStack::new()
            .spacing(StackSpacing::Sm)
            .child(checkbox)
            .child(key_input)
            .child(value_input)
            .child(
                Button::new(remove_id, "✕")
                    .variant(ButtonVariant::Destructive)
                    .size(ButtonSize::Xs)
                    .on_click(remove),
            )
            .build()
            .into_any_element()
    }

    /// The response inspector: status badge, metrics, Headers/Body tabs.
    fn response_viewer(&self, cx: &mut Context<Self>, tab: &Tab) -> AnyElement {
        let theme = cx.theme();
        let spacing = self.design.spacing;
        let Some(response) = &tab.response else {
            return div().into_any_element();
        };

        let view_items = vec![
            TabItem::new(
                "resp-headers",
                format!("Headers ({})", response.headers.len()),
            ),
            TabItem::new("resp-body", "Body"),
        ];
        let selected_view = match response.view {
            ResponseView::Headers => 0,
            ResponseView::Body => 1,
        };

        let mut viewer = div()
            .flex()
            .flex_col()
            .gap_1()
            .border_t_1()
            .border_color(theme.muted)
            .pt(px(spacing.control_padding_y))
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_2()
                    .child(
                        Badge::new(format!(
                            "{} {}",
                            response.status_code, response.reason_phrase
                        ))
                        .variant(badge_variant_of(response.status_code)),
                    )
                    .child(
                        Text::new(format!(
                            "{} ms · {}",
                            response.elapsed_ms,
                            rest_harness::app::composer::human_size(response.size_bytes)
                        ))
                        .muted(true)
                        .size(TextSize::Sm),
                    )
                    .child(div().flex_1())
                    .child(
                        Button::new("copy-body", "Copy")
                            .variant(ButtonVariant::Secondary)
                            .size(ButtonSize::Sm)
                            .on_click(dispatch_button(
                                cx,
                                Message::Composer(ComposerMsg::CopyBody),
                            )),
                    ),
            )
            .child(
                Tabs::new("resp-tabs")
                    .tabs(view_items)
                    .selected_index(selected_view)
                    .on_change(dispatch_tab_index(cx, |index| {
                        Message::Composer(ComposerMsg::ResponseViewSelected(response_view_of(
                            index,
                        )))
                    })),
            );

        match response.view {
            ResponseView::Headers => {
                viewer = viewer.child(
                    div()
                        .id("response-scroll")
                        .max_h(px(240.0))
                        .overflow_y_scroll()
                        .flex()
                        .flex_col()
                        .gap_1()
                        .children(response.headers.iter().map(|header| {
                            Text::new(format!("{}: {}", header.name, header.value))
                                .size(TextSize::Sm)
                                .truncate(true)
                        })),
                );
            }
            ResponseView::Body => {
                let pretty = tab.active_body_view == BodyView::Pretty;
                let body = if pretty {
                    response.body_pretty.clone()
                } else {
                    response.body_raw.clone()
                };
                viewer = viewer
                    .child(
                        HStack::new()
                            .spacing(StackSpacing::Sm)
                            .child(
                                Button::new("resp-pretty", "Pretty")
                                    .variant(ButtonVariant::Secondary)
                                    .size(ButtonSize::Xs)
                                    .selected(pretty)
                                    .on_click(dispatch_button(
                                        cx,
                                        Message::Composer(ComposerMsg::BodyViewSelected(
                                            BodyView::Pretty,
                                        )),
                                    )),
                            )
                            .child(
                                Button::new("resp-raw", "Raw")
                                    .variant(ButtonVariant::Secondary)
                                    .size(ButtonSize::Xs)
                                    .selected(!pretty)
                                    .on_click(dispatch_button(
                                        cx,
                                        Message::Composer(ComposerMsg::BodyViewSelected(
                                            BodyView::Raw,
                                        )),
                                    )),
                            )
                            .build(),
                    )
                    .child(
                        div()
                            .id("response-body-scroll")
                            .max_h(px(240.0))
                            .overflow_y_scroll()
                            .child(Text::new(body).size(TextSize::Sm)),
                    );
            }
        }

        viewer.into_any_element()
    }

    /// The modal overlay: dirty-tab guard and save-draft dialog.
    fn modal_overlay(&self, modal: Modal, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.theme();
        let spacing = self.design.spacing;

        let card = match modal {
            Modal::CloseDirty { .. } => div()
                .flex()
                .flex_col()
                .gap_2()
                .child(Heading::h4("Close unsaved changes?"))
                .child(Text::new("This tab has unsaved changes."))
                .child(button_row(
                    vec![
                        (
                            "modal-save",
                            "Save",
                            ButtonVariant::Primary,
                            ModalMsg::Confirm,
                        ),
                        (
                            "modal-discard",
                            "Discard",
                            ButtonVariant::Destructive,
                            ModalMsg::Discard,
                        ),
                        (
                            "modal-cancel",
                            "Cancel",
                            ButtonVariant::Secondary,
                            ModalMsg::Cancel,
                        ),
                    ],
                    cx,
                )),
            Modal::SaveDraft {
                collections,
                target,
                folder,
                name,
                ..
            } => div()
                .flex()
                .flex_col()
                .gap_2()
                .child(Heading::h4("Save request"))
                .child(self.save_target_select(cx, &collections, target))
                .child(
                    Input::new("save-folder-input")
                        .value(folder)
                        .placeholder("Subfolder path (a/b, optional)")
                        .size(InputSize::Sm)
                        .on_text_change(dispatch_text(cx, |text| {
                            Message::Modal(ModalMsg::FolderChanged(text))
                        })),
                )
                .child(
                    Input::new("save-name-input")
                        .value(name)
                        .placeholder("Request name")
                        .size(InputSize::Sm)
                        .on_text_change(dispatch_text(cx, |text| {
                            Message::Modal(ModalMsg::NameChanged(text))
                        })),
                )
                .child(button_row(
                    vec![
                        (
                            "modal-save",
                            "Save",
                            ButtonVariant::Primary,
                            ModalMsg::Confirm,
                        ),
                        (
                            "modal-cancel",
                            "Cancel",
                            ButtonVariant::Secondary,
                            ModalMsg::Cancel,
                        ),
                    ],
                    cx,
                )),
            other => div()
                .flex()
                .flex_col()
                .gap_2()
                .child(Heading::h4("Dialog"))
                .child(Text::new(format!("{other:?}")))
                .child(button_row(
                    vec![(
                        "modal-cancel",
                        "Close",
                        ButtonVariant::Secondary,
                        ModalMsg::Cancel,
                    )],
                    cx,
                )),
        };

        div()
            .absolute()
            .inset_0()
            .bg(theme.overlay_bg)
            .flex()
            .items_center()
            .justify_center()
            .child(
                div()
                    .w(px(440.0))
                    .bg(theme.surface)
                    .border_1()
                    .border_color(theme.muted)
                    .rounded(px(self.design.corners.lg))
                    .p(px(spacing.card_padding))
                    .child(card),
            )
            .into_any_element()
    }

    /// The target-collection dropdown of the save-draft modal.
    fn save_target_select(
        &self,
        cx: &mut Context<Self>,
        collections: &[PathBuf],
        target: usize,
    ) -> Select {
        let entity = cx.entity();
        let options: Vec<SelectOption> = collections
            .iter()
            .map(|path| {
                let label = path
                    .file_name()
                    .and_then(|name| name.to_str())
                    .unwrap_or("Collection")
                    .to_string();
                SelectOption::new(path.display().to_string(), label)
            })
            .collect();
        let selected = collections
            .get(target)
            .map(|path| path.display().to_string())
            .unwrap_or_default();

        Select::new("save-target-select")
            .options(options)
            .selected(selected)
            .label("Collection")
            .size(SelectSize::Sm)
            .is_open(self.open.save_target)
            .on_change(move |value: &SharedString, _, cx| {
                let value = value.to_string();
                entity.update(cx, |app, cx| {
                    if let Some(Modal::SaveDraft { collections, .. }) = &mut app.state.modal {
                        if let Some(index) = collections
                            .iter()
                            .position(|path| path.display().to_string() == value)
                        {
                            app.dispatch(
                                Message::Modal(ModalMsg::TargetCollectionSelected(index)),
                                cx,
                            );
                        }
                    }
                })
            })
            .on_toggle(toggle_for(cx, Dropdown::SaveTarget))
    }

    /// Environment selector options: none, per-collection (secrets only)
    /// and per-environment entries.
    fn env_options(&self) -> Vec<SelectOption> {
        let mut options = vec![SelectOption::new("", "No environment")];
        if let Some(workspace) = &self.state.workspace {
            for collection in &workspace.collections {
                let dir = collection.path.display().to_string();
                options.push(SelectOption::new(
                    format!("{dir}|"),
                    format!("{} (shared secrets only)", collection.name),
                ));
                for environment in &collection.environments {
                    options.push(SelectOption::new(
                        format!("{dir}|{}", environment.name),
                        format!("{}/{}", collection.name, environment.name),
                    ));
                }
            }
        }
        options
    }
}

/// Which controlled dropdown a toggle belongs to.
#[derive(Debug, Clone, Copy)]
enum Dropdown {
    Method,
    Env,
    BodyMode,
    SaveTarget,
}

/// A `Select::on_toggle` handler writing the open flag into view state.
fn toggle_for(
    cx: &Context<HarnessApp>,
    dropdown: Dropdown,
) -> impl Fn(bool, &mut Window, &mut App) + 'static {
    let entity = cx.entity();
    move |open: bool, _, cx| {
        entity.update(cx, |app, _| match dropdown {
            Dropdown::Method => app.open.method = open,
            Dropdown::Env => app.open.env = open,
            Dropdown::BodyMode => app.open.body_mode = open,
            Dropdown::SaveTarget => app.open.save_target = open,
        })
    }
}

/// The method dropdown of the editor header.
fn method_select(cx: &Context<HarnessApp>, tab: &Tab, open: bool) -> Select {
    let entity = cx.entity();
    let options = all_methods()
        .into_iter()
        .map(|method| SelectOption::new(method.label(), method.label()))
        .collect();

    Select::new("method-select")
        .options(options)
        .selected(tab.request.method.label())
        .size(SelectSize::Sm)
        .is_open(open)
        .on_change(move |value: &SharedString, _, cx| {
            let method = method_of(value);
            entity.update(cx, |app, cx| {
                app.dispatch(Message::Composer(ComposerMsg::MethodChanged(method)), cx);
            })
        })
        .on_toggle(toggle_for(cx, Dropdown::Method))
}

/// The environment selector of the editor header.
fn env_select(
    cx: &Context<HarnessApp>,
    tab: &Tab,
    open: bool,
    env_options: Vec<SelectOption>,
) -> Select {
    let entity = cx.entity();
    let options = env_options;

    Select::new("env-select")
        .options(options)
        .selected(env_value_of(&tab.env))
        .size(SelectSize::Sm)
        .is_open(open)
        .on_change(move |value: &SharedString, _, cx| {
            let selection = env_selection_of(value);
            entity.update(cx, |app, cx| {
                app.dispatch(Message::Composer(ComposerMsg::EnvSelected(selection)), cx);
            })
        })
        .on_toggle(toggle_for(cx, Dropdown::Env))
}

/// The body-mode dropdown of the Body sub-tab.
fn body_mode_select(cx: &Context<HarnessApp>, tab: &Tab, open: bool) -> Select {
    let entity = cx.entity();
    let options = vec![
        SelectOption::new("none", "None"),
        SelectOption::new("json", "JSON"),
        SelectOption::new("text", "Text"),
    ];
    Select::new("body-mode-select")
        .options(options)
        .selected(match tab.body_mode() {
            BodyMode::None => "none",
            BodyMode::Json => "json",
            BodyMode::Text => "text",
        })
        .size(SelectSize::Sm)
        .is_open(open)
        .on_change(move |value: &SharedString, _, cx| {
            let mode = match value.as_ref() {
                "json" => BodyMode::Json,
                "text" => BodyMode::Text,
                _ => BodyMode::None,
            };
            entity.update(cx, |app, cx| {
                app.dispatch(Message::Composer(ComposerMsg::BodyModeChanged(mode)), cx);
            })
        })
        .on_toggle(toggle_for(cx, Dropdown::BodyMode))
}

// Handler factories ------------------------------------------------------

/// A `Button::on_click` handler dispatching a fixed message.
fn dispatch_button(
    cx: &Context<HarnessApp>,
    message: Message,
) -> impl Fn(&mut Window, &mut App) + 'static {
    let entity = cx.entity();
    move |_, cx| entity.update(cx, |app, cx| app.dispatch(message.clone(), cx))
}

/// An `Input::on_text_change` handler mapping the text into a message.
fn dispatch_text<F>(
    cx: &Context<HarnessApp>,
    map: F,
) -> impl Fn(String, &mut Window, &mut App) + 'static
where
    F: Fn(String) -> Message + Copy + 'static,
{
    let entity = cx.entity();
    move |text, _, cx| entity.update(cx, |app, cx| app.dispatch(map(text), cx))
}

/// An `Input::on_text_change` handler for the tab-scoped editors: ignored
/// unless `tab_id` is still the active tab.
fn dispatch_tab_text<F>(
    cx: &Context<HarnessApp>,
    tab_id: u64,
    map: F,
) -> impl Fn(String, &mut Window, &mut App) + 'static
where
    F: Fn(String) -> Message + Copy + 'static,
{
    let entity = cx.entity();
    move |text, _, cx| {
        entity.update(cx, |app, cx| {
            if app.state.active_tab == Some(tab_id) {
                app.dispatch(map(text), cx);
            }
        })
    }
}

/// An `Input::on_change` (Enter) handler sending the active tab's request.
fn dispatch_tab_enter(
    cx: &Context<HarnessApp>,
    tab_id: u64,
) -> impl Fn(&str, &mut Window, &mut App) + 'static {
    let entity = cx.entity();
    move |_, _, cx| {
        entity.update(cx, |app, cx| {
            if app.state.active_tab == Some(tab_id) {
                app.dispatch(Message::Composer(ComposerMsg::Send), cx);
            }
        })
    }
}

/// A `Tabs::on_change` handler mapping the selected index into a message.
fn dispatch_tab_index<F>(
    cx: &Context<HarnessApp>,
    map: F,
) -> impl Fn(usize, &mut Window, &mut App) + 'static
where
    F: Fn(usize) -> Message + Copy + 'static,
{
    let entity = cx.entity();
    move |index, _, cx| entity.update(cx, |app, cx| app.dispatch(map(index), cx))
}

/// A row of modal buttons rendered as one [`HStack`].
fn button_row(
    buttons: Vec<(&'static str, &'static str, ButtonVariant, ModalMsg)>,
    cx: &Context<HarnessApp>,
) -> Div {
    HStack::new()
        .spacing(StackSpacing::Sm)
        .children(buttons.into_iter().map(|(id, label, variant, message)| {
            Button::new(id, label)
                .variant(variant)
                .size(ButtonSize::Sm)
                .on_click(dispatch_button(cx, Message::Modal(message)))
        }))
        .build()
}

// Small mappers ----------------------------------------------------------

/// The sub-tab for a sub-tab strip index.
fn subtab_of(index: usize) -> SubTab {
    match index {
        1 => SubTab::Headers,
        2 => SubTab::Body,
        _ => SubTab::Params,
    }
}

/// The response view for a response strip index.
fn response_view_of(index: usize) -> ResponseView {
    if index == 1 {
        ResponseView::Body
    } else {
        ResponseView::Headers
    }
}

/// All HTTP methods, in enum order.
fn all_methods() -> Vec<HttpMethod> {
    vec![
        HttpMethod::Get,
        HttpMethod::Post,
        HttpMethod::Put,
        HttpMethod::Patch,
        HttpMethod::Delete,
        HttpMethod::Head,
        HttpMethod::Options,
    ]
}

/// Parse a method label back into an [`HttpMethod`].
fn method_of(label: &SharedString) -> HttpMethod {
    match label.as_ref() {
        "POST" => HttpMethod::Post,
        "PUT" => HttpMethod::Put,
        "PATCH" => HttpMethod::Patch,
        "DELETE" => HttpMethod::Delete,
        "HEAD" => HttpMethod::Head,
        "OPTIONS" => HttpMethod::Options,
        _ => HttpMethod::Get,
    }
}

/// Parse an environment selector value into an [`EnvSelection`].
fn env_selection_of(value: &SharedString) -> EnvSelection {
    let (dir, env) = value.split_once('|')?;
    Some(EnvScope {
        collection_dir: PathBuf::from(dir),
        env_name: (!env.is_empty()).then(|| env.to_string()),
    })
}

/// Render an [`EnvSelection`] back into the selector's value.
fn env_value_of(env: &EnvSelection) -> String {
    match env {
        None => String::new(),
        Some(scope) => format!(
            "{}|{}",
            scope.collection_dir.display(),
            scope.env_name.clone().unwrap_or_default()
        ),
    }
}

/// The [`BadgeVariant`] matching a response status class.
fn badge_variant_of(status_code: u16) -> BadgeVariant {
    match ResponseState::status_class(status_code) {
        StatusClass::Success => BadgeVariant::Success,
        StatusClass::Redirect => BadgeVariant::Info,
        StatusClass::ClientError => BadgeVariant::Warning,
        StatusClass::ServerError => BadgeVariant::Error,
        StatusClass::Other => BadgeVariant::Default,
    }
}

/// The workspace label shown in the top bar.
fn workspace_label(state: &AppState) -> String {
    match &state.workspace_root {
        Some(root) => format!("Workspace: {}", root.display()),
        None => "No workspace".to_string(),
    }
}

/// Flatten the workspace tree into `(label, request path)` rows for the
/// sidebar (collections and nested folders walked recursively).
fn collect_requests(workspace: &Workspace) -> Vec<(String, PathBuf)> {
    let mut rows = Vec::new();
    for collection in &workspace.collections {
        let prefix = collection.name.clone();
        for entry in &collection.requests {
            rows.push((format!("{prefix} / {}", entry.name), entry.path.clone()));
        }
        walk_folders(&prefix, &collection.folders, &mut rows);
    }
    rows
}

fn walk_folders(prefix: &str, folders: &[Folder], rows: &mut Vec<(String, PathBuf)>) {
    for folder in folders {
        let label = format!("{prefix} / {}", folder.name);
        for entry in &folder.requests {
            rows.push((format!("{label} / {}", entry.name), entry.path.clone()));
        }
        walk_folders(&label, &folder.folders, rows);
    }
}
