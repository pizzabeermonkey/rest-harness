//! Headless `--screenshot` mode: renders the real `gui::view(&app)` offscreen
//! via iced's tiny-skia renderer and writes a PNG — no display required.
//!
//! The scene state is produced by driving the REAL reducer
//! (`AppState::update`) through a synchronous mini-shell:
//!
//! ```text
//! Message ──> AppState::update ──> Vec<Effect> ──> local executor ──> Messages
//! ```
//!
//! Honesty rules (mirroring the real shell, `gui::update`):
//! - The workspace is loaded from a fresh TEMP COPY of the repo's
//!   `collections/` directory, so history appends never touch the repo.
//! - The response comes from a REAL local HTTP round-trip: an in-process
//!   tokio echo server answers the prepared request (its URL rewritten to
//!   `http://127.0.0.1:<port>`), executed with the real
//!   `ReqwestHttpClient` — status, time and size are measured, not faked.
//! - History entries are appended with the real `HistoryStore` and the
//!   workspace is reloaded afterwards, exactly like the shell does.

use iced::advanced::renderer::Headless as _;
use iced_runtime::user_interface::{Cache, UserInterface};
use rest_harness::app::composer::{EnvScope, EnvSelection};
use rest_harness::app::{
    AppState, ComposerMsg, Effect, LoadedWorkspace, Message, ResponseView, SidebarMsg, TabMsg,
    WorkspaceMsg,
};
use rest_harness::config::AppConfig;
use rest_harness::history::HistoryStore;
use rest_harness::http::{HttpClient as _, ReqwestHttpClient};
use rest_harness::interpolation::PreparedHttpRequest;
use rest_harness::storage::read_request;
use std::path::{Path, PathBuf};
use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};

/// Logical screenshot size (matches the window size of the real app).
const SIZE: iced::Size = iced::Size::new(1200.0, 800.0);

/// Entry point of `rest-harness --screenshot [output.png]`. Drives the
/// scene and renders the final state to `output` (default
/// `assets/screenshot.png`, applied by the caller in `main.rs`).
pub async fn run(output: &Path) -> Result<(), String> {
    // A fresh temp copy of the repo's `collections/` is used as the
    // workspace root: history appends write `<root>/.rest-harness-history.yaml`
    // and must land in the copy only.
    let repo_collections = std::env::current_dir()
        .map_err(|error| format!("could not resolve the current directory: {error}"))?
        .join("collections");
    if !repo_collections.is_dir() {
        return Err(format!(
            "no collections/ directory found at {}",
            repo_collections.display()
        ));
    }
    let root = std::env::temp_dir().join(format!("rest-harness-screenshot-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    copy_dir_recursive(&repo_collections, &root).map_err(|error| {
        format!(
            "could not copy {} to {}: {error}",
            repo_collections.display(),
            root.display()
        )
    })?;

    let result = scene(&root, output).await;
    let _ = std::fs::remove_dir_all(&root);
    result
}

/// Drive the app to the screenshot scene and render it.
async fn scene(root: &Path, output: &Path) -> Result<(), String> {
    let port = spawn_echo_server()
        .await
        .map_err(|error| format!("could not start the local echo server: {error}"))?;
    let driver = Driver {
        port,
        client: ReqwestHttpClient::default(),
    };

    let mut app = super::App::new(
        AppState::new(Some(root.to_path_buf())),
        AppConfig::default(),
    );

    // 1. Load the workspace (what the shell's initial task delivers).
    let loaded = load_workspace(root)?;
    driver
        .drive(
            &mut app,
            Message::Workspace(WorkspaceMsg::Loaded(Ok(loaded))),
        )
        .await;

    // 2. Expand the sample collection in the sidebar (the first load already
    //    seeds this; the toggle is a defensive no-op unless it collapsed).
    let collection_dir = collection_dir(&mut app)?;
    if !app.state.expanded.contains(&collection_dir) {
        driver
            .drive(
                &mut app,
                Message::Sidebar(SidebarMsg::NodeToggled(collection_dir.clone())),
            )
            .await;
    }

    // 3. Open both requests through the reducer (`Opened` focuses the new
    //    tab, so `Get headers` is opened last and ends up active).
    let post_path = collection_dir.join("post-echo.yaml");
    let get_path = collection_dir.join("get-headers.yaml");
    for path in [&post_path, &get_path] {
        let request = read_request(path).map_err(|error| error.to_string());
        driver
            .drive(
                &mut app,
                Message::Tab(TabMsg::Opened {
                    path: path.clone(),
                    request,
                }),
            )
            .await;
    }

    // 4. Select the `dev` environment on the active tab so `{{base_url}}`
    //    and `{{api_token}}` interpolate.
    let dev: EnvSelection = Some(EnvScope {
        collection_dir: collection_dir.clone(),
        env_name: Some("dev".to_string()),
    });
    driver
        .drive(
            &mut app,
            Message::Composer(ComposerMsg::EnvSelected(dev.clone())),
        )
        .await;

    // 5. Real HTTP round-trips: `Get headers` first, then `Post echo` (one
    //    history entry each), then back to `Get headers` as the active tab.
    let get_tab_id = tab_id_of(&app, &get_path)?;
    let post_tab_id = tab_id_of(&app, &post_path)?;
    driver.send(&mut app).await;
    driver
        .drive(&mut app, Message::Tab(TabMsg::Select(post_tab_id)))
        .await;
    driver
        .drive(
            &mut app,
            Message::Composer(ComposerMsg::EnvSelected(dev.clone())),
        )
        .await;
    driver.send(&mut app).await;
    driver
        .drive(&mut app, Message::Tab(TabMsg::Select(get_tab_id)))
        .await;

    // 6. Show the response body (Pretty is the default body view).
    driver
        .drive(
            &mut app,
            Message::Composer(ComposerMsg::ResponseViewSelected(ResponseView::Body)),
        )
        .await;

    // 7. Render the REAL view offscreen and write the PNG.
    render(&app, output)
}

/// The effect executor of the screenshot mini-shell: it drives `AppState::update`,
/// executes every returned [`Effect`] locally (real storage + real HTTP
/// round-trips against the echo server) and feeds the reply messages back
/// until quiescent. Mirrors `gui::update` (including editor sync).
struct Driver {
    /// Port of the local echo server; `Effect::SendRequest` targets it.
    port: u16,
    /// The real HTTP engine, executed against the local server.
    client: ReqwestHttpClient,
}

impl Driver {
    async fn drive(&self, app: &mut super::App, message: Message) {
        let effects = app.state.update(message);
        for effect in effects {
            let replies = self.execute(app, effect).await;
            for reply in replies {
                Box::pin(self.drive(app, reply)).await;
            }
        }
        super::sync_editors(app);
    }

    /// Execute one [`Effect`] without iced `Task`s, returning the reply
    /// messages (empty for fire-and-forget effects).
    async fn execute(&self, app: &mut super::App, effect: Effect) -> Vec<Message> {
        match effect {
            Effect::LoadWorkspace(root) => {
                vec![reload_message(&root)]
            }
            Effect::OpenRequest { path } => {
                vec![Message::Tab(TabMsg::Opened {
                    request: read_request(&path).map_err(|error| error.to_string()),
                    path,
                })]
            }
            Effect::SendRequest { tab_id, prepared } => {
                let result = self.send_prepared(&prepared).await;
                vec![Message::Composer(ComposerMsg::SendFinished {
                    tab_id,
                    result,
                })]
            }
            Effect::AppendHistory {
                entry,
                secret_values,
            } => match app.state.workspace_root.clone() {
                Some(root) => {
                    let store = HistoryStore::new(&root);
                    if let Err(error) = store.append_entry(entry, &secret_values) {
                        return vec![Message::Workspace(WorkspaceMsg::Loaded(Err(
                            error.to_string()
                        )))];
                    }
                    vec![reload_message(&root)]
                }
                None => Vec::new(),
            },
            Effect::Notify(text) => vec![Message::Workspace(WorkspaceMsg::Loaded(Err(text)))],
            // CRUD/pick/save effects are not exercised by the screenshot
            // scene; they would need a real workspace + dialog interaction.
            Effect::PickWorkspaceFolder
            | Effect::CreateCollection { .. }
            | Effect::CreateFolder { .. }
            | Effect::CreateRequest { .. }
            | Effect::Rename { .. }
            | Effect::Duplicate { .. }
            | Effect::Delete { .. }
            | Effect::CancelSend { .. }
            | Effect::SaveRequest { .. }
            | Effect::SaveDraft { .. }
            | Effect::CopyText(_)
            | Effect::SaveEnvironment { .. }
            | Effect::SaveSecrets { .. } => Vec::new(),
        }
    }

    /// Send the active tab and deliver the response through the reducer,
    /// including the follow-up `AppendHistory` effect and workspace reload.
    async fn send(&self, app: &mut super::App) {
        self.drive(app, Message::Composer(ComposerMsg::Send)).await;
    }

    /// Execute a prepared request against the LOCAL echo server (scheme +
    /// authority rewritten, path + query kept) with the real HTTP engine.
    async fn send_prepared(
        &self,
        prepared: &PreparedHttpRequest,
    ) -> Result<rest_harness::http::HttpResponseData, String> {
        let local_url = localize_url(&prepared.url, self.port)?;
        let local = PreparedHttpRequest {
            method: prepared.method,
            url: local_url,
            headers: prepared.headers.clone(),
            body: prepared.body.clone(),
        };
        self.client
            .execute(&local)
            .await
            .map_err(|error| error.to_string())
    }
}

/// Rewrite scheme + authority of `url` to `http://127.0.0.1:<port>`,
/// keeping path + query identical.
fn localize_url(url: &str, port: u16) -> Result<String, String> {
    let mut parsed = reqwest::Url::parse(url).map_err(|error| format!("invalid url: {error}"))?;
    parsed
        .set_scheme("http")
        .map_err(|_| format!("cannot rewrite the scheme of {url}"))?;
    parsed
        .set_host(Some("127.0.0.1"))
        .map_err(|error| format!("cannot rewrite the host of {url}: {error}"))?;
    parsed
        .set_port(Some(port))
        .map_err(|_| format!("cannot rewrite the port of {url}"))?;
    Ok(parsed.to_string())
}

/// The reload message the real shell sends after every storage effect.
fn reload_message(root: &Path) -> Message {
    let loaded = load_workspace(root);
    Message::Workspace(WorkspaceMsg::Loaded(loaded))
}

/// Load a [`LoadedWorkspace`] synchronously (blocking I/O in an async
/// context is fine here: the file set is tiny).
fn load_workspace(root: &Path) -> Result<LoadedWorkspace, String> {
    super::load_from_disk(root).map_err(|error| error.to_string())
}

/// The directory of the sample Collection from the loaded workspace.
fn collection_dir(app: &mut super::App) -> Result<PathBuf, String> {
    app.state
        .workspace
        .as_ref()
        .and_then(|workspace| workspace.collections.first())
        .map(|collection| collection.path.clone())
        .ok_or_else(|| "the sample collection was not loaded".to_string())
}

/// The id of the Saved tab opened for `path`.
fn tab_id_of(app: &super::App, path: &Path) -> Result<u64, String> {
    app.state
        .tabs
        .iter()
        .find(|tab| {
            matches!(
                &tab.kind,
                rest_harness::app::TabKind::Saved { path: tab_path } if tab_path == path
            )
        })
        .map(|tab| tab.id)
        .ok_or_else(|| format!("no tab is open for {}", path.display()))
}

/// Recursively copy `source` into `target` (both directories).
fn copy_dir_recursive(source: &Path, target: &Path) -> std::io::Result<()> {
    std::fs::create_dir_all(target)?;
    for entry in std::fs::read_dir(source)? {
        let entry = entry?;
        let destination = target.join(entry.file_name());
        if entry.file_type()?.is_dir() {
            copy_dir_recursive(&entry.path(), &destination)?;
        } else {
            std::fs::copy(entry.path(), destination)?;
        }
    }
    Ok(())
}

/// Render `gui::view(app)` offscreen with the tiny-skia renderer and write
/// an 8-bit RGBA PNG to `output`.
fn render(app: &super::App, output: &Path) -> Result<(), String> {
    let mut renderer = iced::Renderer::new(iced::Font::default(), iced::Pixels(16.0));
    let element = super::view(app);
    let mut ui = UserInterface::build(element, SIZE, Cache::new(), &mut renderer);

    let theme = iced::Theme::Dark;
    let base = iced::theme::Base::base(&theme);
    let style = iced::advanced::renderer::Style {
        text_color: base.text_color,
    };
    ui.draw(
        &mut renderer,
        &theme,
        &style,
        iced::mouse::Cursor::Unavailable,
    );

    let pixels = renderer.screenshot(
        iced::Size::new(SIZE.width as u32, SIZE.height as u32),
        1.0,
        base.background_color,
    );
    write_png(output, pixels)
}

/// Encode RGBA8 bytes as a PNG file.
fn write_png(output: &Path, rgba: Vec<u8>) -> Result<(), String> {
    let file = std::fs::File::create(output)
        .map_err(|error| format!("could not create {output:?}: {error}"))?;
    let mut encoder = png::Encoder::new(
        std::io::BufWriter::new(file),
        SIZE.width as u32,
        SIZE.height as u32,
    );
    encoder.set_color(png::ColorType::Rgba);
    encoder.set_depth(png::BitDepth::Eight);
    let mut writer = encoder
        .write_header()
        .map_err(|error| format!("could not write the PNG header: {error}"))?;
    writer
        .write_image_data(&rgba)
        .map_err(|error| format!("could not write the PNG data: {error}"))?;
    Ok(())
}

/// Spawn the in-process HTTP/1.1 echo server, returning its port. It
/// answers every request with a canned `200 OK` JSON body: `GET` echoes
/// the request headers, `POST` additionally echoes the received body.
async fn spawn_echo_server() -> std::io::Result<u16> {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
    let port = listener.local_addr()?.port();
    tokio::spawn(async move {
        while let Ok((socket, _)) = listener.accept().await {
            tokio::spawn(handle_connection(socket));
        }
    });
    Ok(port)
}

/// Answer one connection: parse the request head + (Content-Length) body,
/// reply with the echo JSON.
async fn handle_connection(mut socket: tokio::net::TcpStream) {
    let request = match read_request_bytes(&mut socket).await {
        Some(request) => request,
        None => return,
    };
    let echo = match echo_json(&request) {
        Some(echo) => echo,
        None => return,
    };
    let body = match serde_json::to_vec(&echo) {
        Ok(body) => body,
        Err(_) => return,
    };
    let response = format!(
        "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        body.len()
    );
    let _ = socket.write_all(response.as_bytes()).await;
    let _ = socket.write_all(&body).await;
    let _ = socket.flush().await;
}

/// A parsed request head plus body.
struct ParsedRequest {
    method: String,
    path: String,
    headers: Vec<(String, String)>,
    body: String,
}

/// Read the request head (up to `\r\n\r\n`) and then the
/// Content-Length-sized body.
async fn read_request_bytes(socket: &mut tokio::net::TcpStream) -> Option<ParsedRequest> {
    const HEAD_END: &[u8] = b"\r\n\r\n";
    let mut buffer = Vec::new();
    let mut chunk = [0u8; 4096];
    let head_end;
    loop {
        let read = socket.read(&mut chunk).await.ok()?;
        if read == 0 {
            return None;
        }
        buffer.extend_from_slice(&chunk[..read]);
        if let Some(position) = buffer
            .windows(HEAD_END.len())
            .position(|window| window == HEAD_END)
        {
            head_end = position;
            break;
        }
    }

    let head = String::from_utf8_lossy(&buffer[..head_end]).into_owned();
    let mut lines = head.lines();
    let request_line = lines.next()?;
    let mut parts = request_line.split_whitespace();
    let method = parts.next()?.to_string();
    let path = parts.next()?.to_string();

    let mut headers = Vec::new();
    let mut content_length = 0usize;
    for line in lines {
        let Some((name, value)) = line.split_once(':') else {
            continue;
        };
        let name = name.trim().to_string();
        let value = value.trim().to_string();
        if name.eq_ignore_ascii_case("content-length") {
            content_length = value.parse().unwrap_or(0);
        }
        headers.push((name, value));
    }

    let body_start = head_end + HEAD_END.len();
    let mut body = buffer[body_start.min(buffer.len())..].to_vec();
    while body.len() < content_length {
        let read = socket.read(&mut chunk).await.ok()?;
        if read == 0 {
            break;
        }
        body.extend_from_slice(&chunk[..read]);
    }
    body.truncate(content_length);

    Some(ParsedRequest {
        method,
        path,
        headers,
        body: String::from_utf8_lossy(&body).into_owned(),
    })
}

/// The echo response JSON (httpbin-style): headers for any method, plus
/// the body and origin for POST.
fn echo_json(request: &ParsedRequest) -> Option<serde_json::Value> {
    let headers: serde_json::Map<String, serde_json::Value> = request
        .headers
        .iter()
        .map(|(name, value)| (name.clone(), serde_json::Value::String(value.clone())))
        .collect();
    if request.method.eq_ignore_ascii_case("POST") {
        Some(serde_json::json!({
            "method": request.method,
            "path": request.path,
            "origin": "127.0.0.1",
            "headers": headers,
            "body": request.body,
        }))
    } else {
        Some(serde_json::json!({
            "method": request.method,
            "path": request.path,
            "headers": headers,
        }))
    }
}
