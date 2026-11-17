//! Request Composer state machines: tabs, editor edits, the send pipeline
//! and the response viewer.
//!
//! Like the rest of [`crate::app`], this module contains **no `iced`
//! types**. The `iced` shell (`src/gui/composer.rs`) renders [`Tab`]s and
//! maps the returned [`Effect`]s to `iced::Task`s.
//!
//! Overview:
//! - A [`Tab`] is one open editor: a saved Request file
//!   ([`TabKind::Saved`]), an unsaved [`TabKind::Draft`] or a
//!   [`TabKind::HistoryDraft`] opened from a History Entry (a detached
//!   draft that saves via the save-draft modal and is then promoted to
//!   `Saved`, prefilled with the entry's response snapshot).
//! - Every composer edit marks the tab [`Tab::dirty`]; closing a dirty tab
//!   opens a Save/Discard/Cancel modal (`Modal::CloseDirty`).
//! - The URL and the params table stay in sync bidirectionally via
//!   [`crate::url_query`] — pure, no I/O.
//! - `ComposerMsg::Send` resolves the tab's variables from its
//!   [`EnvSelection`] (Collection secrets + environment), then
//!   interpolates via [`crate::interpolation::prepare`]. Missing
//!   variables fail fast into `tab.send_error` with **no effect**; success
//!   emits `Effect::SendRequest` (the shell owns the in-flight registry
//!   and cancellation tokens).
//! - `SendFinished` stores the response in the tab and emits
//!   `Effect::AppendHistory` built with [`crate::http::build_history_entry`]
//!   so `HistoryStore::append_entry` redacts the tab's secret values on
//!   write.

use crate::app::tree;
use crate::app::{Effect, Modal};
use crate::domain::{
    pretty_json, AuthConfig, Collection, HistoryResponse, HttpHeader, HttpMethod, KeyValueRow,
    RawContentType, RedactedHttpRequest, Request, RequestBody, Workspace,
};
use crate::error::Error;
use crate::http::HttpResponseData;
use crate::interpolation::{self, PreparedHttpRequest};
use crate::url_query;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

/// Which Collection + Environment a tab sends its requests with.
///
/// `env_name: None` means "the Collection is scoped but no specific
/// Environment is selected": only shared secrets resolve. The whole
/// selection being `None` ([`EnvSelection`] = `None`) means "No
/// Environment": nothing resolves at all.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EnvScope {
    /// Absolute path of the Collection directory.
    pub collection_dir: PathBuf,
    /// Selected Environment name inside that Collection, if any.
    pub env_name: Option<String>,
}

/// The environment selection of a tab: `Some` scope, or `None` for "No
/// Environment".
pub type EnvSelection = Option<EnvScope>;

/// What a tab is editing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TabKind {
    /// An open saved Request, keyed by its `.yaml` path — at most one tab
    /// per path.
    Saved {
        /// Path of the backing request file.
        path: PathBuf,
    },
    /// An unsaved new request; promoted to `Saved` through the save-draft
    /// modal.
    Draft,
    /// Opened from a History Entry; a detached draft that saves via the
    /// save-draft modal (never overwrites) and carries the entry's
    /// response snapshot.
    HistoryDraft,
}

/// Sub-tab of the request editor (Params / Headers / Body).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum SubTab {
    /// Query parameter table.
    #[default]
    Params,
    /// Header table.
    Headers,
    /// Raw body editor.
    Body,
}

/// Which view of the response body is shown (Pretty | Raw).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum BodyView {
    /// Pretty-printed (JSON highlighted when applicable).
    #[default]
    Pretty,
    /// Plain text.
    Raw,
}

/// Request body editor mode (None | JSON | Text).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BodyMode {
    /// No body.
    None,
    /// Raw body declared as `application/json`.
    Json,
    /// Raw body declared as `text/plain`.
    Text,
}

/// Which response inspector tab is shown (Headers | Body).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum ResponseView {
    /// Response headers table.
    #[default]
    Headers,
    /// Response body.
    Body,
}

/// The response snapshot shown in the inspector: either the result of a
/// send or, for [`TabKind::HistoryDraft`], the History Entry's response.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ResponseState {
    /// HTTP status code.
    pub status_code: u16,
    /// HTTP reason phrase (e.g. `OK`).
    pub reason_phrase: String,
    /// Duration of the call in milliseconds.
    pub elapsed_ms: u64,
    /// Response size in bytes.
    pub size_bytes: u64,
    /// Response headers.
    pub headers: Vec<HttpHeader>,
    /// Pretty-printed body (equal to `body_raw` when not JSON).
    pub body_pretty: String,
    /// Body as received (lossy UTF-8).
    pub body_raw: String,
    /// Which inspector tab is active.
    pub view: ResponseView,
}

/// Appended to both body views of a History response whose body was
/// truncated to fit the 1 MB History limit.
const TRUNCATION_NOTICE: &str = "\n\n[Truncated at 1 MB in history]";

impl ResponseState {
    /// Build the viewer state from an HTTP response.
    pub fn from_http(data: &HttpResponseData) -> Self {
        ResponseState {
            status_code: data.status_code,
            reason_phrase: data.reason_phrase.clone(),
            elapsed_ms: data.elapsed_ms,
            size_bytes: data.size_bytes,
            headers: data.headers.clone(),
            body_pretty: data.body_pretty.clone(),
            body_raw: data.body_raw.clone(),
            view: ResponseView::Headers,
        }
    }

    /// Build the viewer state from a History Entry's response snapshot.
    ///
    /// When the entry's body was truncated to fit the 1 MB History limit,
    /// a notice is appended to both body views so the user sees why the
    /// payload ended early.
    pub fn from_history(response: &HistoryResponse) -> Self {
        let notice = if response.body_truncated {
            TRUNCATION_NOTICE
        } else {
            ""
        };
        ResponseState {
            status_code: response.status_code,
            reason_phrase: response.reason_phrase.clone(),
            elapsed_ms: response.elapsed_ms,
            size_bytes: response.size_bytes,
            headers: response.headers.clone(),
            body_pretty: format!("{}{notice}", pretty_json(&response.body)),
            body_raw: format!("{}{notice}", response.body),
            view: ResponseView::Headers,
        }
    }

    /// The status class color bucket of `status_code` (2xx/3xx/4xx/5xx).
    pub fn status_class(status_code: u16) -> StatusClass {
        match status_code {
            200..=299 => StatusClass::Success,
            300..=399 => StatusClass::Redirect,
            400..=499 => StatusClass::ClientError,
            500..=599 => StatusClass::ServerError,
            _ => StatusClass::Other,
        }
    }
}

/// The color bucket of a response status code (for the status badge).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StatusClass {
    /// 2xx.
    Success,
    /// 3xx.
    Redirect,
    /// 4xx.
    ClientError,
    /// 5xx.
    ServerError,
    /// Anything else.
    Other,
}

/// One editable name/value row of an environment/secrets editor modal.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct NameValue {
    /// Variable or secret name.
    pub name: String,
    /// Value; may contain `{{refs}}` verbatim.
    pub value: String,
}

/// A draft being saved: on the next successful reload the draft tab is
/// converted to [`TabKind::Saved`] at `path`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PendingSave {
    /// The draft tab to convert.
    pub tab_id: u64,
    /// The path the request was created at.
    pub path: PathBuf,
}

/// One open request editor.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Tab {
    /// Stable id (also the shell's editor-content key).
    pub id: u64,
    /// What the tab edits.
    pub kind: TabKind,
    /// The edited request.
    pub request: Request,
    /// Whether any composer edit is unsaved (response-only state does not
    /// set this).
    pub dirty: bool,
    /// Environment selection used to resolve variables.
    pub env: EnvSelection,
    /// Last response snapshot, when any.
    pub response: Option<ResponseState>,
    /// Whether a request is currently being sent (a Cancel button shows).
    pub in_flight: bool,
    /// The last send/prepare error, when any.
    pub send_error: Option<String>,
    /// Active editor sub-tab.
    pub active_subtab: SubTab,
    /// Active response body view (Pretty | Raw).
    pub active_body_view: BodyView,
    /// The prepared request of the in-flight send (kept so the response
    /// can be appended to History with the exact request that was sent).
    pub prepared: Option<PreparedHttpRequest>,
}

impl Tab {
    /// The tab strip label: the request name, or a kind-based fallback.
    pub fn title(&self) -> String {
        if !self.request.name.trim().is_empty() {
            self.request.name.clone()
        } else {
            match &self.kind {
                TabKind::Saved { path } => path
                    .file_stem()
                    .and_then(|stem| stem.to_str())
                    .unwrap_or("Request")
                    .to_string(),
                TabKind::Draft => "Untitled".to_string(),
                TabKind::HistoryDraft => "History draft".to_string(),
            }
        }
    }

    /// The current body editor mode derived from the request body.
    pub fn body_mode(&self) -> BodyMode {
        match &self.request.body {
            RequestBody::None => BodyMode::None,
            RequestBody::Raw {
                content_type: RawContentType::Json,
                ..
            } => BodyMode::Json,
            RequestBody::Raw {
                content_type: RawContentType::Text,
                ..
            } => BodyMode::Text,
        }
    }

    /// The raw body text (empty when there is no body).
    pub fn body_text(&self) -> String {
        match &self.request.body {
            RequestBody::None => String::new(),
            RequestBody::Raw { content, .. } => content.clone(),
        }
    }
}

/// Messages that open/close/focus tabs.
// `large_enum_variant`: `Opened` carries the whole request by design (the
// shell reads it off-thread and hands it over in one message).
#[allow(clippy::large_enum_variant)]
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TabMsg {
    /// Append and focus a new untitled Draft tab.
    NewDraft,
    /// Close the tab (a dirty tab opens the `CloseDirty` modal first).
    Close(u64),
    /// Focus the tab.
    Select(u64),
    /// Inbound after the shell read a request file: focus the existing
    /// Saved tab for that path or append a new one.
    Opened {
        /// Path of the request file.
        path: PathBuf,
        /// The parsed request, or the read error text.
        request: Result<Request, String>,
    },
}

/// Messages driving the request editor and send pipeline of the ACTIVE
/// tab (identified by `AppState::active_tab`).
///
/// `Eq` is not derivable (the `BodyEdit` variant carries `f32`
/// coordinates).
#[derive(Debug, Clone, PartialEq)]
pub enum ComposerMsg {
    /// URL text changed; params re-sync from the URL.
    UrlChanged(String),
    /// HTTP method changed.
    MethodChanged(HttpMethod),
    /// Append an empty param row.
    ParamAdded,
    /// Remove the param row at the index.
    ParamRemoved(usize),
    /// Toggle the param row at the index.
    ParamToggled(usize),
    /// Param key changed.
    ParamKeyChanged(usize, String),
    /// Param value changed.
    ParamValueChanged(usize, String),
    /// Param description changed.
    ParamDescriptionChanged(usize, String),
    /// Append an empty header row.
    HeaderAdded,
    /// Remove the header row at the index.
    HeaderRemoved(usize),
    /// Toggle the header row at the index.
    HeaderToggled(usize),
    /// Header key changed.
    HeaderKeyChanged(usize, String),
    /// Header value changed.
    HeaderValueChanged(usize, String),
    /// Header description changed.
    HeaderDescriptionChanged(usize, String),
    /// Body editor mode changed (None | JSON | Text).
    BodyModeChanged(BodyMode),
    /// The body text changed (the shell forwards the editor's full text).
    BodyTextChanged(String),
    /// Shell-side editor interaction forwarded to the shell-owned editor
    /// content (see [`BodyEdit`]); the core reducer ignores this variant —
    /// the shell intercepts it in its `update` before the reducer runs and
    /// re-emits [`ComposerMsg::BodyTextChanged`] with the resulting text.
    BodyEdit(BodyEdit),
    /// Environment selection changed (also clears `send_error`).
    EnvSelected(EnvSelection),
    /// Select the editor sub-tab (Params / Headers / Body).
    SubTabSelected(SubTab),
    /// Select the response inspector tab (Headers / Body).
    ResponseViewSelected(ResponseView),
    /// Toggle the response body view (Pretty | Raw); response-only state,
    /// never sets `dirty`.
    BodyViewSelected(BodyView),
    /// Cancel the active tab's in-flight request (emits
    /// `Effect::CancelSend`; the shell cancels its token and the
    /// completion arrives as [`ComposerMsg::SendCancelled`]).
    CancelSend,
    /// Send the active tab's request (also Enter in the URL input).
    Send,
    /// Inbound: the shell executed a request for `tab_id`.
    SendFinished {
        /// The sending tab.
        tab_id: u64,
        /// The response, or the error text (already sanitized of secret
        /// values by the engine).
        result: Result<HttpResponseData, String>,
    },
    /// Inbound: the send for `tab_id` was cancelled.
    SendCancelled {
        /// The sending tab.
        tab_id: u64,
    },
    /// Save the active tab (Saved: upsert; Draft and HistoryDraft: open
    /// the save-draft modal).
    Save,
    /// Copy the response body (per the active view) to the clipboard.
    CopyBody,
    /// Open the Edit Environments modal for the active tab's Collection.
    OpenEditEnvironments,
    /// Open the Edit Secrets modal for the active tab's Collection.
    OpenEditSecrets,
}

/// Core-owned mirror of the iced text-editor action.
///
/// The shell's `on_edit` closure maps the (iced-typed) editor action into
/// this iced-free enum; the shell `update` intercepts
/// [`ComposerMsg::BodyEdit`], reconstructs the action, performs it on its
/// per-tab editor content and forwards the resulting text to the reducer
/// as [`ComposerMsg::BodyTextChanged`]. The core never sees iced types.
///
/// `Eq` is not derivable (the click/drag coordinates are `f32`).
#[derive(Debug, Clone, PartialEq)]
pub enum BodyEdit {
    /// Insert a character.
    Insert(char),
    /// Paste text.
    Paste(String),
    /// Break the line.
    Enter,
    /// Indent the current line.
    Indent,
    /// Unindent the current line.
    Unindent,
    /// Delete backwards.
    Backspace,
    /// Delete forwards.
    Delete,
    /// Move the cursor.
    Move(BodyMotion),
    /// Select text with a motion.
    Select(BodyMotion),
    /// Select the word at the cursor.
    SelectWord,
    /// Select the line at the cursor.
    SelectLine,
    /// Select the entire buffer.
    SelectAll,
    /// Click at (x, y) widget coordinates.
    Click { x: f32, y: f32 },
    /// Drag to (x, y) widget coordinates.
    Drag { x: f32, y: f32 },
    /// Scroll by lines.
    Scroll { lines: i32 },
}

/// Core-owned mirror of the iced cursor motion.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BodyMotion {
    /// Move left.
    Left,
    /// Move right.
    Right,
    /// Move up.
    Up,
    /// Move down.
    Down,
    /// Move to the left word boundary.
    WordLeft,
    /// Move to the right word boundary.
    WordRight,
    /// Move to the line start.
    Home,
    /// Move to the line end.
    End,
    /// Page up.
    PageUp,
    /// Page down.
    PageDown,
    /// Move to the text start.
    DocumentStart,
    /// Move to the text end.
    DocumentEnd,
}

impl AppState {
    /// Reserve the next free tab id.
    fn next_tab_id(&mut self) -> u64 {
        self.next_tab_id += 1;
        self.next_tab_id
    }

    /// Reducer arm for [`Message::Tab`].
    pub fn update_tab(&mut self, message: TabMsg) -> Vec<Effect> {
        match message {
            TabMsg::NewDraft => {
                let name = next_untitled_name(&self.tabs);
                let id = self.next_tab_id();
                self.tabs.push(Tab {
                    id,
                    kind: TabKind::Draft,
                    request: Request {
                        name,
                        ..Request::default()
                    },
                    dirty: false,
                    env: None,
                    response: None,
                    in_flight: false,
                    send_error: None,
                    active_subtab: SubTab::default(),
                    active_body_view: BodyView::default(),
                    prepared: None,
                });
                self.active_tab = Some(id);
            }
            TabMsg::Close(id) => {
                let dirty = self
                    .tabs
                    .iter()
                    .find(|tab| tab.id == id)
                    .is_some_and(|tab| tab.dirty);
                if dirty {
                    self.modal = Some(Modal::CloseDirty { tab_id: id });
                } else {
                    self.close_tab(id);
                }
            }
            TabMsg::Select(id) => self.active_tab = Some(id),
            TabMsg::Opened { path, request } => match request {
                Ok(request) => self.open_saved_tab(path, request),
                Err(error) => self.status = Some(error),
            },
        }
        Vec::new()
    }

    /// Reducer arm for [`Message::Composer`].
    pub fn update_composer(&mut self, message: ComposerMsg) -> Vec<Effect> {
        match message {
            ComposerMsg::UrlChanged(url) => {
                let Some(tab) = self.active_tab_mut() else {
                    return Vec::new();
                };
                tab.request.params = url_query::sync_url_to_params(&url, &tab.request.params);
                tab.request.url = url;
                tab.dirty = true;
            }
            ComposerMsg::MethodChanged(method) => {
                if let Some(tab) = self.active_tab_mut() {
                    tab.request.method = method;
                    tab.dirty = true;
                }
            }
            ComposerMsg::ParamAdded => self.edit_params(|params| {
                params.push(KeyValueRow::new("", ""));
            }),
            ComposerMsg::ParamRemoved(index) => self.edit_params(|params| {
                if index < params.len() {
                    params.remove(index);
                }
            }),
            ComposerMsg::ParamToggled(index) => self.edit_params(|params| {
                if let Some(row) = params.get_mut(index) {
                    row.enabled = !row.enabled;
                }
            }),
            ComposerMsg::ParamKeyChanged(index, key) => self.edit_params(|params| {
                if let Some(row) = params.get_mut(index) {
                    row.key = key;
                }
            }),
            ComposerMsg::ParamValueChanged(index, value) => self.edit_params(|params| {
                if let Some(row) = params.get_mut(index) {
                    row.value = value;
                }
            }),
            // Descriptions live only in the table, not the URL.
            ComposerMsg::ParamDescriptionChanged(index, description) => {
                if let Some(tab) = self.active_tab_mut() {
                    if let Some(row) = tab.request.params.get_mut(index) {
                        row.description = description;
                    }
                    tab.dirty = true;
                }
            }
            ComposerMsg::HeaderAdded => self.edit_headers(|headers| {
                headers.push(KeyValueRow::new("", ""));
            }),
            ComposerMsg::HeaderRemoved(index) => self.edit_headers(|headers| {
                if index < headers.len() {
                    headers.remove(index);
                }
            }),
            ComposerMsg::HeaderToggled(index) => self.edit_headers(|headers| {
                if let Some(row) = headers.get_mut(index) {
                    row.enabled = !row.enabled;
                }
            }),
            ComposerMsg::HeaderKeyChanged(index, key) => self.edit_headers(|headers| {
                if let Some(row) = headers.get_mut(index) {
                    row.key = key;
                }
            }),
            ComposerMsg::HeaderValueChanged(index, value) => self.edit_headers(|headers| {
                if let Some(row) = headers.get_mut(index) {
                    row.value = value;
                }
            }),
            ComposerMsg::HeaderDescriptionChanged(index, description) => {
                if let Some(tab) = self.active_tab_mut() {
                    if let Some(row) = tab.request.headers.get_mut(index) {
                        row.description = description;
                    }
                    tab.dirty = true;
                }
            }
            ComposerMsg::BodyModeChanged(mode) => {
                if let Some(tab) = self.active_tab_mut() {
                    tab.request.body = match mode {
                        BodyMode::None => RequestBody::None,
                        BodyMode::Json => RequestBody::Raw {
                            content_type: RawContentType::Json,
                            content: tab.body_text(),
                        },
                        BodyMode::Text => RequestBody::Raw {
                            content_type: RawContentType::Text,
                            content: tab.body_text(),
                        },
                    };
                    tab.dirty = true;
                }
            }
            ComposerMsg::BodyTextChanged(text) => {
                if let Some(tab) = self.active_tab_mut() {
                    if let RequestBody::Raw { content, .. } = &mut tab.request.body {
                        // Cursor-only editor interactions (Move/Click)
                        // forward the unchanged text; only a real change
                        // marks the tab dirty.
                        if *content != text {
                            *content = text;
                            tab.dirty = true;
                        }
                    }
                }
            }
            // Intercepted by the shell before the reducer; a no-op here so
            // the arm stays exhaustive.
            ComposerMsg::BodyEdit(_) => {}
            ComposerMsg::EnvSelected(env) => {
                if let Some(tab) = self.active_tab_mut() {
                    tab.env = env;
                    // The new selection may resolve previously missing
                    // variables; let the user retry immediately.
                    tab.send_error = None;
                }
            }
            // View/state-only selections: never mark the tab dirty.
            ComposerMsg::SubTabSelected(subtab) => {
                if let Some(tab) = self.active_tab_mut() {
                    tab.active_subtab = subtab;
                }
            }
            ComposerMsg::ResponseViewSelected(view) => {
                if let Some(tab) = self.active_tab_mut() {
                    if let Some(response) = &mut tab.response {
                        response.view = view;
                    }
                }
            }
            ComposerMsg::BodyViewSelected(view) => {
                if let Some(tab) = self.active_tab_mut() {
                    tab.active_body_view = view;
                }
            }
            ComposerMsg::CancelSend => {
                let Some(tab) = self.active_tab() else {
                    return Vec::new();
                };
                if tab.in_flight {
                    return vec![Effect::CancelSend { tab_id: tab.id }];
                }
            }
            ComposerMsg::Send => return self.send_active(),
            ComposerMsg::SendFinished { tab_id, result } => {
                return self.receive_response(tab_id, result);
            }
            ComposerMsg::SendCancelled { tab_id } => {
                if let Some(tab) = self.tabs.iter_mut().find(|tab| tab.id == tab_id) {
                    tab.in_flight = false;
                    tab.prepared = None;
                }
            }
            ComposerMsg::Save => return self.save_active(),
            ComposerMsg::CopyBody => {
                let Some(tab) = self.active_tab() else {
                    return Vec::new();
                };
                if let Some(response) = &tab.response {
                    let body = match tab.active_body_view {
                        BodyView::Pretty => response.body_pretty.clone(),
                        BodyView::Raw => response.body_raw.clone(),
                    };
                    return vec![Effect::CopyText(body)];
                }
            }
            ComposerMsg::OpenEditEnvironments => return self.open_edit_environments(),
            ComposerMsg::OpenEditSecrets => return self.open_edit_secrets(),
        }
        Vec::new()
    }

    /// Mutable access to the active tab.
    pub(crate) fn active_tab_mut(&mut self) -> Option<&mut Tab> {
        let id = self.active_tab?;
        self.tabs.iter_mut().find(|tab| tab.id == id)
    }

    /// Read-only access to the active tab.
    pub fn active_tab(&self) -> Option<&Tab> {
        let id = self.active_tab?;
        self.tabs.iter().find(|tab| tab.id == id)
    }

    /// Run `edit` on the active tab's params, rebuilding the URL from the
    /// (edited) table and marking the tab dirty.
    fn edit_params(&mut self, edit: impl FnOnce(&mut Vec<KeyValueRow>)) {
        let Some(tab) = self.active_tab_mut() else {
            return;
        };
        edit(&mut tab.request.params);
        tab.request.url = url_query::sync_params_to_url(&tab.request.url, &tab.request.params);
        tab.dirty = true;
    }

    /// Run `edit` on the active tab's headers, marking the tab dirty.
    fn edit_headers(&mut self, edit: impl FnOnce(&mut Vec<KeyValueRow>)) {
        let Some(tab) = self.active_tab_mut() else {
            return;
        };
        edit(&mut tab.request.headers);
        tab.dirty = true;
    }

    /// Focus the existing Saved tab for `path` or append a new one
    /// (dedup: at most one tab per saved path).
    fn open_saved_tab(&mut self, path: PathBuf, request: Request) {
        if let Some(existing) = self
            .tabs
            .iter()
            .position(|tab| tab.kind == TabKind::Saved { path: path.clone() })
        {
            self.active_tab = Some(self.tabs[existing].id);
            return;
        }
        // Reconcile URL ↔ params from disk truth: a request saved with a
        // query in the URL but an empty table gets its params table
        // populated; one with enabled rows and a stale URL gets its URL
        // rebuilt (see `crate::url_query`).
        let mut request = request;
        if request.params.is_empty() && request.url.contains('?') {
            request.params = url_query::sync_url_to_params(&request.url, &[]);
        } else if request.params.iter().any(|row| row.enabled) {
            request.url = url_query::sync_params_to_url(&request.url, &request.params);
        }
        let id = self.next_tab_id();
        let env = self
            .workspace
            .as_ref()
            .and_then(|workspace| collection_of_request(workspace, &path))
            .map(|collection| EnvScope {
                collection_dir: collection.path.clone(),
                env_name: None,
            });
        self.tabs.push(Tab {
            id,
            kind: TabKind::Saved { path },
            request,
            dirty: false,
            env,
            response: None,
            in_flight: false,
            send_error: None,
            active_subtab: SubTab::default(),
            active_body_view: BodyView::default(),
            prepared: None,
        });
        self.active_tab = Some(id);
    }

    /// Open a HistoryDraft tab prefilled from the History Entry at
    /// `index` (immediate, no I/O).
    pub(crate) fn open_history_entry(&mut self, index: usize) {
        let Some(entry) = self
            .workspace
            .as_ref()
            .and_then(|workspace| workspace.history.get(index))
        else {
            return;
        };
        let request = request_from_history(&entry.request);
        let response = ResponseState::from_history(&entry.response);
        let id = self.next_tab_id();
        self.tabs.push(Tab {
            id,
            kind: TabKind::HistoryDraft,
            request,
            dirty: false,
            env: None,
            response: Some(response),
            in_flight: false,
            send_error: None,
            active_subtab: SubTab::default(),
            active_body_view: BodyView::default(),
            prepared: None,
        });
        self.active_tab = Some(id);
    }

    /// Close a tab, keeping `active_tab` on a neighbour.
    pub(crate) fn close_tab(&mut self, id: u64) {
        let Some(position) = self.tabs.iter().position(|tab| tab.id == id) else {
            return;
        };
        self.tabs.remove(position);
        if self.active_tab == Some(id) {
            self.active_tab = self
                .tabs
                .get(position)
                .or_else(|| self.tabs.last())
                .map(|tab| tab.id);
        }
    }

    /// Close every Saved tab whose path is `path` or inside it (used when
    /// the underlying node is deleted).
    pub(crate) fn close_tabs_under(&mut self, path: &Path) {
        let doomed: Vec<u64> = self
            .tabs
            .iter()
            .filter(|tab| matches!(&tab.kind, TabKind::Saved { path: tab_path } if tab_path == path || tab_path.starts_with(path)))
            .map(|tab| tab.id)
            .collect();
        for id in doomed {
            self.close_tab(id);
        }
    }

    /// Send the active tab's request: resolve variables from its
    /// environment selection, interpolate, and either fail fast into
    /// `send_error` (no effect) or emit `Effect::SendRequest`.
    fn send_active(&mut self) -> Vec<Effect> {
        if self.active_tab().is_none_or(|tab| tab.in_flight) {
            return Vec::new();
        }
        let env = self.active_tab().and_then(|tab| tab.env.clone());
        let request = self.active_tab().map(|tab| tab.request.clone());
        let Some(request) = request else {
            return Vec::new();
        };
        if request.url.trim().is_empty() {
            if let Some(tab) = self.active_tab_mut() {
                tab.send_error = Some("The URL is empty.".to_string());
            }
            return Vec::new();
        }
        let variables = self.resolve_variables(&env);
        match interpolation::prepare(&request, &variables) {
            Err(Error::MissingVariables { variables: missing }) => {
                if let Some(tab) = self.active_tab_mut() {
                    tab.send_error = Some(missing_variables_message(&missing));
                }
                Vec::new()
            }
            Err(error) => {
                if let Some(tab) = self.active_tab_mut() {
                    tab.send_error = Some(error.to_string());
                }
                Vec::new()
            }
            Ok(prepared) => {
                let Some(tab) = self.active_tab_mut() else {
                    return Vec::new();
                };
                tab.in_flight = true;
                tab.send_error = None;
                tab.prepared = Some(prepared.clone());
                vec![Effect::SendRequest {
                    tab_id: tab.id,
                    prepared,
                }]
            }
        }
    }

    /// Effective variables for a tab's environment selection: the scoped
    /// Collection's Environment variables, overridden by shared Secrets,
    /// overridden by per-Environment Secret overrides.
    pub(crate) fn resolve_variables(&self, env: &EnvSelection) -> BTreeMap<String, String> {
        let Some(scope) = env else {
            return BTreeMap::new();
        };
        let Some(collection) = self
            .workspace
            .as_ref()
            .and_then(|workspace| find_collection(workspace, &scope.collection_dir))
        else {
            return BTreeMap::new();
        };
        let environment = scope
            .env_name
            .as_ref()
            .and_then(|name| collection.environments.iter().find(|env| &env.name == name));
        collection.secrets.resolve_variables(environment)
    }

    /// Non-empty secret values for a tab's environment selection (used to
    /// redact the History entry on write).
    fn secret_values_for(&self, env: &EnvSelection) -> Vec<String> {
        let Some(scope) = env else {
            return Vec::new();
        };
        let Some(collection) = self
            .workspace
            .as_ref()
            .and_then(|workspace| find_collection(workspace, &scope.collection_dir))
        else {
            return Vec::new();
        };
        collection
            .secrets
            .secret_values_for_env(scope.env_name.as_deref())
    }

    /// Store the response of a finished send; on success also emit
    /// `Effect::AppendHistory` with the tab's secret values so the redacting
    /// history store persists the exchange.
    fn receive_response(
        &mut self,
        tab_id: u64,
        result: Result<HttpResponseData, String>,
    ) -> Vec<Effect> {
        let env;
        let prepared;
        let data;
        {
            let Some(tab) = self.tabs.iter_mut().find(|tab| tab.id == tab_id) else {
                return Vec::new();
            };
            match result {
                Ok(response_data) => {
                    prepared = tab.prepared.take();
                    let previous_view = tab
                        .response
                        .as_ref()
                        .map(|response| response.view)
                        .unwrap_or_default();
                    let mut response = ResponseState::from_http(&response_data);
                    response.view = previous_view;
                    tab.response = Some(response);
                    tab.in_flight = false;
                    tab.send_error = None;
                    env = tab.env.clone();
                    data = response_data;
                }
                Err(error) => {
                    tab.send_error = Some(error);
                    tab.in_flight = false;
                    tab.prepared = None;
                    return Vec::new();
                }
            }
        }
        match prepared {
            Some(prepared) => {
                let entry = crate::http::build_history_entry(&prepared, &data);
                let secret_values = self.secret_values_for(&env);
                vec![Effect::AppendHistory {
                    entry,
                    secret_values,
                }]
            }
            // Defensive: without the prepared request there is no honest
            // history entry to append.
            None => Vec::new(),
        }
    }

    /// Save the active tab: Saved tabs upsert via `Effect::SaveRequest`
    /// (dirty cleared on the following successful reload); Drafts and
    /// HistoryDrafts open the save-draft modal (a HistoryDraft is a
    /// detached draft: `create_request` never overwrites, and the tab is
    /// promoted to `Saved` on the success reload).
    fn save_active(&mut self) -> Vec<Effect> {
        let Some(tab) = self.active_tab().cloned() else {
            return Vec::new();
        };
        match &tab.kind {
            TabKind::Saved { path } => {
                self.pending_saved.push(tab.id);
                vec![Effect::SaveRequest {
                    path: path.clone(),
                    request: tab.request.clone(),
                }]
            }
            TabKind::Draft | TabKind::HistoryDraft => {
                self.open_save_draft_modal(tab.id);
                Vec::new()
            }
        }
    }

    /// Open the save-draft modal for the draft tab `tab_id`.
    pub(crate) fn open_save_draft_modal(&mut self, tab_id: u64) {
        let Some(workspace) = &self.workspace else {
            return;
        };
        if workspace.collections.is_empty() {
            self.status = Some("Create a collection before saving.".to_string());
            return;
        }
        let Some(tab) = self.tabs.iter().find(|tab| tab.id == tab_id) else {
            return;
        };
        let collections: Vec<PathBuf> = workspace
            .collections
            .iter()
            .map(|collection| collection.path.clone())
            .collect();
        let name = tab.request.name.clone();
        self.modal = Some(Modal::SaveDraft {
            tab_id,
            request: tab.request.clone(),
            collections,
            target: 0,
            folder: String::new(),
            name,
        });
    }

    /// Validate the save-draft modal input and emit `Effect::SaveDraft`.
    ///
    /// The folder path ("a/b" segments, validated against the tree) and the
    /// request name reuse the established validations; `pending_save` is
    /// armed so the following successful reload promotes the draft tab to
    /// `Saved { path }`.
    pub(crate) fn confirm_save_draft(&mut self) -> Vec<Effect> {
        let Some(Modal::SaveDraft {
            tab_id,
            mut request,
            collections,
            target,
            folder,
            name,
        }) = self.modal.clone()
        else {
            return Vec::new();
        };
        let Some(collection_dir) = collections.get(target).cloned() else {
            self.status = Some("Select a target collection.".to_string());
            return Vec::new();
        };
        let name = name.trim().to_string();
        let result = self
            .resolve_folder_path(&collection_dir, &folder)
            .and_then(|parent| {
                self.validate_request_name(&parent, &name)
                    .map(|_| (parent, name))
            });
        match result {
            Ok((parent, name)) => {
                request.name = name;
                let Ok(sanitized) = crate::storage::sanitize_filename(&request.name) else {
                    return Vec::new();
                };
                let path = parent.join(format!("{sanitized}.yaml"));
                self.modal = None;
                self.pending_save = Some(PendingSave { tab_id, path });
                vec![Effect::SaveDraft { parent, request }]
            }
            Err(error) => {
                self.status = Some(error);
                Vec::new()
            }
        }
    }

    /// Resolve a "a/b" folder path against the tree, returning the target
    /// parent directory (the Collection itself for an empty path).
    ///
    /// Every segment must name an existing Folder under the previous one.
    pub(crate) fn resolve_folder_path(
        &self,
        collection_dir: &Path,
        folder: &str,
    ) -> Result<PathBuf, String> {
        let Some(workspace) = &self.workspace else {
            return Err("No workspace is open.".to_string());
        };
        let mut current = collection_dir.to_path_buf();
        for segment in folder.split('/').map(str::trim).filter(|s| !s.is_empty()) {
            let names = tree::folder_names_under(workspace, &current);
            let matched = names
                .iter()
                .find(|name| sanitized(name) == sanitized(segment))
                .ok_or_else(|| format!("Folder \"{segment}\" does not exist."))?;
            current = current.join(matched);
        }
        Ok(current)
    }

    /// Open the Edit Environments modal for the active tab's Collection.
    fn open_edit_environments(&mut self) -> Vec<Effect> {
        let Some(scope) = self.active_tab().and_then(|tab| tab.env.clone()) else {
            self.status = Some("Select an environment collection first.".to_string());
            return Vec::new();
        };
        let Some(workspace) = &self.workspace else {
            return Vec::new();
        };
        let Some(collection) = find_collection(workspace, &scope.collection_dir) else {
            return Vec::new();
        };
        let envs: Vec<String> = collection
            .environments
            .iter()
            .map(|env| env.name.clone())
            .collect();
        let selected = scope
            .env_name
            .as_ref()
            .and_then(|name| envs.iter().position(|env| env == name))
            .unwrap_or(0)
            .min(envs.len());
        let rows = initial_environment_rows(collection, selected, &envs);
        self.modal = Some(Modal::EditEnvironments {
            collection_dir: scope.collection_dir,
            envs,
            selected,
            new_name: String::new(),
            rows,
        });
        Vec::new()
    }

    /// Open the Edit Secrets modal for the active tab's Collection
    /// (shared section plus, when an environment is selected, its
    /// per-environment section).
    fn open_edit_secrets(&mut self) -> Vec<Effect> {
        let Some(scope) = self.active_tab().and_then(|tab| tab.env.clone()) else {
            self.status = Some("Select an environment collection first.".to_string());
            return Vec::new();
        };
        let Some(workspace) = &self.workspace else {
            return Vec::new();
        };
        let Some(collection) = find_collection(workspace, &scope.collection_dir) else {
            return Vec::new();
        };
        let shared_rows = collection
            .secrets
            .shared
            .iter()
            .map(|(name, value)| NameValue {
                name: name.clone(),
                value: value.clone(),
            })
            .collect();
        let env_rows = scope
            .env_name
            .as_ref()
            .and_then(|name| collection.secrets.environments.get(name))
            .map(|overrides| {
                overrides
                    .iter()
                    .map(|(name, value)| NameValue {
                        name: name.clone(),
                        value: value.clone(),
                    })
                    .collect()
            })
            .unwrap_or_default();
        self.modal = Some(Modal::EditSecrets {
            collection_dir: scope.collection_dir,
            env_name: scope.env_name,
            shared_rows,
            env_rows,
        });
        Vec::new()
    }

    /// Validate the Edit Environments modal and emit
    /// `Effect::SaveEnvironment`.
    pub(crate) fn confirm_edit_environments(&mut self) -> Vec<Effect> {
        let Some(Modal::EditEnvironments {
            collection_dir,
            envs,
            selected,
            new_name,
            rows,
        }) = self.modal.clone()
        else {
            return Vec::new();
        };
        let name = if selected < envs.len() {
            envs[selected].clone()
        } else {
            new_name.trim().to_string()
        };
        if let Err(error) = validate_name(&name) {
            self.status = Some(error);
            return Vec::new();
        }
        if selected >= envs.len()
            && envs
                .iter()
                .any(|existing| sanitized(existing) == sanitized(&name))
        {
            self.status = Some(format!("An environment named \"{name}\" already exists."));
            return Vec::new();
        }
        match rows_to_variables(&rows) {
            Ok(variables) => {
                let path = collection_dir
                    .join(crate::storage::ENVIRONMENTS_DIR)
                    .join(format!(
                        "{}.yaml",
                        crate::storage::sanitize_filename(&name).unwrap_or_default()
                    ));
                self.modal = None;
                vec![Effect::SaveEnvironment {
                    collection_dir,
                    environment: crate::domain::Environment {
                        name,
                        path,
                        variables,
                    },
                }]
            }
            Err(error) => {
                self.status = Some(error);
                Vec::new()
            }
        }
    }

    /// Validate the Edit Secrets modal and emit `Effect::SaveSecrets`,
    /// merging the edited sections into the Collection's existing store so
    /// unrelated per-environment overrides survive.
    pub(crate) fn confirm_edit_secrets(&mut self) -> Vec<Effect> {
        let Some(Modal::EditSecrets {
            collection_dir,
            env_name,
            shared_rows,
            env_rows,
        }) = self.modal.clone()
        else {
            return Vec::new();
        };
        let shared = match rows_to_variables(&shared_rows) {
            Ok(map) => map,
            Err(error) => {
                self.status = Some(error);
                return Vec::new();
            }
        };
        let environment_rows = match rows_to_variables(&env_rows) {
            Ok(map) => map,
            Err(error) => {
                self.status = Some(error);
                return Vec::new();
            }
        };
        let mut secrets = self
            .workspace
            .as_ref()
            .and_then(|workspace| find_collection(workspace, &collection_dir))
            .map(|collection| collection.secrets.clone())
            .unwrap_or_default();
        secrets.shared = shared;
        if let Some(env_name) = &env_name {
            if environment_rows.is_empty() {
                secrets.environments.remove(env_name);
            } else {
                secrets
                    .environments
                    .insert(env_name.clone(), environment_rows);
            }
        }
        self.modal = None;
        vec![Effect::SaveSecrets {
            collection_dir,
            secrets,
        }]
    }

    /// Reconcile open tabs with a freshly loaded workspace:
    ///
    /// 1. `pending_save` — convert the saved draft tab to `Saved { path }`.
    /// 2. `pending_saved` — clear the dirty flag of successfully saved
    ///    tabs (kept as markers instead of clearing eagerly so a failed
    ///    save never lies about disk truth).
    /// 3. `pending_close` — close a tab whose save-and-close finished.
    /// 4. Prune Saved tabs whose file no longer exists in the tree.
    ///
    /// All markers are one-shot: any successful reload consumes them.
    pub(crate) fn apply_composer_reload(&mut self, workspace: &Workspace) {
        if let Some(pending) = self.pending_save.take() {
            if let Some(tab) = self.tabs.iter_mut().find(|tab| tab.id == pending.tab_id) {
                if tree::contains_request(workspace, &pending.path) {
                    tab.kind = TabKind::Saved { path: pending.path };
                    tab.dirty = false;
                }
            }
        }

        let saved = std::mem::take(&mut self.pending_saved);
        for id in saved {
            if let Some(tab) = self.tabs.iter_mut().find(|tab| tab.id == id) {
                tab.dirty = false;
            }
        }

        if let Some(id) = self.pending_close.take() {
            self.close_tab(id);
        }

        self.tabs.retain(|tab| match &tab.kind {
            TabKind::Saved { path } => tree::contains_request(workspace, path),
            TabKind::Draft | TabKind::HistoryDraft => true,
        });
        if self
            .active_tab
            .is_some_and(|id| !self.tabs.iter().any(|tab| tab.id == id))
        {
            self.active_tab = self.tabs.last().map(|tab| tab.id);
        }
    }
}

use crate::app::AppState;

/// Human-readable byte size: `B` up to 1 KiB, then `KB`, then `MB`.
pub fn human_size(bytes: u64) -> String {
    const KIB: f64 = 1024.0;
    const MIB: f64 = 1024.0 * 1024.0;
    let bytes = bytes as f64;
    if bytes < KIB {
        format!("{} B", bytes as u64)
    } else if bytes < MIB {
        format!("{:.1} KB", bytes / KIB)
    } else {
        format!("{:.1} MB", bytes / MIB)
    }
}

/// User-facing failure message for unresolved `{{variables}}`.
fn missing_variables_message(missing: &[String]) -> String {
    if missing.is_empty() {
        return "The request is missing variables.".to_string();
    }
    format!(
        "Missing variables: {}. Select an environment that defines them.",
        missing.join(", ")
    )
}

/// The next free draft name: `Untitled`, `Untitled 2`, ...
fn next_untitled_name(tabs: &[Tab]) -> String {
    let taken: Vec<String> = tabs
        .iter()
        .filter(|tab| matches!(tab.kind, TabKind::Draft))
        .map(|tab| tab.title())
        .collect();
    for number in 1.. {
        let candidate = if number == 1 {
            "Untitled".to_string()
        } else {
            format!("Untitled {number}")
        };
        if !taken.contains(&candidate) {
            return candidate;
        }
    }
    unreachable!("the loop above always returns")
}

/// Reconstruct a [`Request`] from a History Entry's (redacted) request:
/// headers become enabled rows and a non-empty body becomes a raw body
/// (JSON-typed when it parses as JSON).
fn request_from_history(redacted: &RedactedHttpRequest) -> Request {
    let body = if redacted.body.is_empty() {
        RequestBody::None
    } else {
        let content_type = if serde_json::from_str::<serde_json::Value>(&redacted.body).is_ok() {
            RawContentType::Json
        } else {
            RawContentType::Text
        };
        RequestBody::Raw {
            content_type,
            content: redacted.body.clone(),
        }
    };
    Request {
        name: String::new(),
        method: redacted.method,
        url: redacted.url.clone(),
        // The entry URL's query pairs prefill the Params table (enabled),
        // matching the URL↔params sync of the other tab kinds.
        params: url_query::sync_url_to_params(&redacted.url, &[]),
        headers: redacted
            .headers
            .iter()
            .map(|header| KeyValueRow::new(header.name.clone(), header.value.clone()))
            .collect(),
        auth: AuthConfig::None,
        body,
    }
}

/// The Collection that directly contains the Request at `path`.
fn collection_of_request<'a>(workspace: &'a Workspace, path: &Path) -> Option<&'a Collection> {
    workspace
        .collections
        .iter()
        .find(|collection| contains_request_in(collection, path))
}

fn contains_request_in(collection: &Collection, path: &Path) -> bool {
    if collection.requests.iter().any(|entry| entry.path == path) {
        return true;
    }
    collection
        .folders
        .iter()
        .any(|folder| folder_contains_request(folder, path))
}

fn folder_contains_request(folder: &crate::domain::Folder, path: &Path) -> bool {
    folder.requests.iter().any(|entry| entry.path == path)
        || folder
            .folders
            .iter()
            .any(|nested| folder_contains_request(nested, path))
}

/// The Collection whose directory is `path`.
fn find_collection<'a>(workspace: &'a Workspace, path: &Path) -> Option<&'a Collection> {
    workspace
        .collections
        .iter()
        .find(|collection| collection.path == path)
}

/// Case-insensitive-by-sanitization name comparison (matches the tree
/// collision checks).
fn sanitized(name: &str) -> String {
    crate::storage::sanitize_filename(name).unwrap_or_else(|_| name.to_string())
}

/// First-line validation for a modal name.
fn validate_name(name: &str) -> Result<(), String> {
    if name.is_empty() {
        return Err("Name cannot be empty.".to_string());
    }
    crate::storage::sanitize_filename(name)
        .map(|_| ())
        .map_err(|error| error.to_string())
}

/// Variable rows of the environment the modal starts on (empty for a
/// brand-new environment).
fn initial_environment_rows(
    collection: &Collection,
    selected: usize,
    envs: &[String],
) -> Vec<NameValue> {
    if selected >= envs.len() {
        return Vec::new();
    }
    collection
        .environments
        .iter()
        .find(|env| env.name == envs[selected])
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
}

/// Turn editable rows into a variable map, rejecting empty keys and
/// duplicates.
fn rows_to_variables(rows: &[NameValue]) -> Result<BTreeMap<String, String>, String> {
    let mut variables = BTreeMap::new();
    for row in rows {
        let name = row.name.trim();
        if name.is_empty() {
            return Err("Names cannot be empty.".to_string());
        }
        if variables.contains_key(name) {
            return Err(format!("Duplicate name \"{name}\"."));
        }
        variables.insert(name.to_string(), row.value.clone());
    }
    Ok(variables)
}

// ---------------------------------------------------------------------------
// JSON syntax highlighting tokenizer (pure; unit-tested)
// ---------------------------------------------------------------------------

/// The kind of a [`JsonToken`] (drives the highlight color).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum JsonTokenKind {
    /// Object key (a string directly followed by `:`).
    Key,
    /// String literal value.
    String,
    /// Number literal.
    Number,
    /// `true` / `false`.
    Boolean,
    /// `null`.
    Null,
    /// Structural punctuation and whitespace.
    Plain,
}

/// One lexical token of pretty-printed JSON text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct JsonToken {
    /// The token's lexeme.
    pub text: String,
    /// The token's kind.
    pub kind: JsonTokenKind,
}

/// Tokenize `input` for JSON syntax highlighting, returning `None` when it
/// is not valid JSON (the viewer then falls back to plain text).
///
/// The tokenizer is deliberately simple: it scans strings (with escape
/// handling to find the true end), numbers, `true`/`false`/`null` keywords
/// and treats everything else (braces, colons, commas, whitespace) as
/// [`JsonTokenKind::Plain`]. A string token directly followed by `:` is a
/// [`JsonTokenKind::Key`]. Validity is verified with `serde_json` first, so
/// tokenizing never has to error mid-stream.
pub fn tokenize_json(input: &str) -> Option<Vec<JsonToken>> {
    if serde_json::from_str::<serde_json::Value>(input).is_err() {
        return None;
    }

    let bytes = input.as_bytes();
    let mut tokens: Vec<JsonToken> = Vec::new();
    let mut push = |text: String, kind: JsonTokenKind| {
        if !text.is_empty() {
            tokens.push(JsonToken { text, kind });
        }
    };
    let mut index = 0;
    while index < bytes.len() {
        let byte = bytes[index];
        match byte {
            b'"' => {
                // Scan to the closing quote, honoring `\"` escapes.
                let mut end = index + 1;
                while end < bytes.len() {
                    if bytes[end] == b'\\' && end + 1 < bytes.len() {
                        end += 2;
                        continue;
                    }
                    if bytes[end] == b'"' {
                        end += 1;
                        break;
                    }
                    end += 1;
                }
                let text = &input[index..end.min(bytes.len())];
                // A string immediately followed by `:` is an object key.
                let rest = &input[end..];
                let kind = if rest.trim_start().starts_with(':') {
                    JsonTokenKind::Key
                } else {
                    JsonTokenKind::String
                };
                push(text.to_string(), kind);
                index = end;
            }
            b'-' | b'0'..=b'9' => {
                let end = (index..bytes.len())
                    .take_while(|&i| {
                        matches!(bytes[i], b'-' | b'+' | b'.' | b'0'..=b'9' | b'e' | b'E')
                    })
                    .last()
                    .map_or(index, |i| i + 1);
                // Always advance (cannot happen for pre-validated JSON).
                let end = end.max(index + 1);
                push(input[index..end].to_string(), JsonTokenKind::Number);
                index = end;
            }
            b't' if input[index..].starts_with("true") => {
                push("true".to_string(), JsonTokenKind::Boolean);
                index += 4;
            }
            b'f' if input[index..].starts_with("false") => {
                push("false".to_string(), JsonTokenKind::Boolean);
                index += 5;
            }
            b'n' if input[index..].starts_with("null") => {
                push("null".to_string(), JsonTokenKind::Null);
                index += 4;
            }
            _ => {
                // Punctuation and whitespace lumped into Plain runs.
                let end = (index..bytes.len())
                    .take_while(|&i| {
                        !matches!(bytes[i], b'"' | b'-' | b'0'..=b'9' | b't' | b'f' | b'n')
                    })
                    .last()
                    .map_or(index, |i| i + 1);
                // Always advance (cannot happen for pre-validated JSON,
                // where `t`/`f`/`n` outside strings are always keywords).
                let end = end.max(index + 1);
                push(input[index..end].to_string(), JsonTokenKind::Plain);
                index = end;
            }
        }
    }
    Some(tokens)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::Message;

    #[test]
    fn human_size_formats_b_kb_mb() {
        assert_eq!(human_size(0), "0 B");
        assert_eq!(human_size(512), "512 B");
        assert_eq!(human_size(1023), "1023 B");
        assert_eq!(human_size(1024), "1.0 KB");
        assert_eq!(human_size(1536), "1.5 KB");
        assert_eq!(human_size(1024 * 1024), "1.0 MB");
        assert_eq!(human_size(2 * 1024 * 1024 + 5), "2.0 MB");
    }

    #[test]
    fn tokenize_json_kinds_strings_numbers_bools_null_punctuation() {
        let tokens = tokenize_json("{\n  \"a\": 1,\n  \"b\": [true, null],\n  \"c\": \"x\"\n}")
            .expect("valid json");
        let kinds: Vec<(String, JsonTokenKind)> = tokens
            .into_iter()
            .map(|token| (token.text, token.kind))
            .collect();
        assert!(kinds.contains(&("{\n  ".to_string(), JsonTokenKind::Plain)));
        assert!(kinds.contains(&("\"a\"".to_string(), JsonTokenKind::Key)));
        assert!(kinds.contains(&("1".to_string(), JsonTokenKind::Number)));
        assert!(kinds.contains(&("\"b\"".to_string(), JsonTokenKind::Key)));
        assert!(kinds.contains(&("true".to_string(), JsonTokenKind::Boolean)));
        assert!(kinds.contains(&("null".to_string(), JsonTokenKind::Null)));
        assert!(kinds.contains(&("\"x\"".to_string(), JsonTokenKind::String)));
        assert!(kinds.contains(&(": [".to_string(), JsonTokenKind::Plain)));
        // Reconstructing the input from the tokens preserves it exactly.
        let round: String = kinds.iter().map(|(text, _)| text.as_str()).collect();
        assert_eq!(
            round,
            "{\n  \"a\": 1,\n  \"b\": [true, null],\n  \"c\": \"x\"\n}"
        );
    }

    #[test]
    fn tokenize_json_handles_escapes_and_negatives() {
        let tokens =
            tokenize_json("{\"a\":\"quote\\\" inside\",\"n\":-12.5e3}").expect("valid json");
        let strings: Vec<&JsonToken> = tokens
            .iter()
            .filter(|token| matches!(token.kind, JsonTokenKind::String))
            .collect();
        assert_eq!(strings.len(), 1);
        assert_eq!(strings[0].text, "\"quote\\\" inside\"");
        assert!(tokens
            .iter()
            .any(|token| token.text == "-12.5e3" && token.kind == JsonTokenKind::Number));
    }

    #[test]
    fn tokenize_json_falls_back_to_none_for_non_json() {
        assert!(tokenize_json("not json").is_none());
        assert!(tokenize_json("{invalid").is_none());
        assert!(tokenize_json("").is_none());
    }

    #[test]
    fn untitled_names_never_collide() {
        let mut state = AppState::new(None);
        state.update(Message::Tab(TabMsg::NewDraft));
        state.update(Message::Tab(TabMsg::NewDraft));
        state.update(Message::Tab(TabMsg::NewDraft));
        let names: Vec<String> = state
            .tabs
            .iter()
            .map(|tab| tab.request.name.clone())
            .collect();
        assert_eq!(names, vec!["Untitled", "Untitled 2", "Untitled 3"]);
    }

    #[test]
    fn status_class_buckets() {
        assert_eq!(ResponseState::status_class(200), StatusClass::Success);
        assert_eq!(ResponseState::status_class(302), StatusClass::Redirect);
        assert_eq!(ResponseState::status_class(404), StatusClass::ClientError);
        assert_eq!(ResponseState::status_class(500), StatusClass::ServerError);
        assert_eq!(ResponseState::status_class(100), StatusClass::Other);
    }

    #[test]
    fn from_history_appends_the_truncation_notice() {
        let response = HistoryResponse {
            status_code: 200,
            reason_phrase: "OK".into(),
            elapsed_ms: 5,
            size_bytes: 2_000_000,
            headers: Vec::new(),
            body: "{\"a\":1}".into(),
            body_truncated: true,
            timestamp: chrono::DateTime::<chrono::Utc>::from_timestamp(1_700_000_000, 0).unwrap(),
        };
        let state = ResponseState::from_history(&response);
        assert!(state.body_raw.ends_with("[Truncated at 1 MB in history]"));
        assert!(state
            .body_pretty
            .ends_with("[Truncated at 1 MB in history]"));

        // Non-truncated responses keep the current behaviour exactly.
        let mut plain = response.clone();
        plain.body_truncated = false;
        let state = ResponseState::from_history(&plain);
        assert_eq!(state.body_raw, "{\"a\":1}");
        assert_eq!(state.body_pretty, "{\n  \"a\": 1\n}");
    }
}
