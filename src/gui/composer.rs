//! The request composer pane: tab strip, request editor (method, URL,
//! params/headers/body), environment selector and the response inspector.
//!
//! Rendering only — every state transition goes through the core reducer
//! (`rest_harness::app::composer`); the shell maps the returned `Effect`s
//! to `iced::Task`s (see `execute_effect` in `gui/mod.rs`).

use iced::widget::{
    button, checkbox, column, container, pick_list, rich_text, row, scrollable, text, text_editor,
    text_input,
};
use iced::{alignment, Color, Element, Length, Theme};
use rest_harness::app::composer::{human_size, tokenize_json, JsonTokenKind};
use rest_harness::app::{
    AppState, BodyEdit, BodyMode, BodyMotion, BodyView, ComposerMsg, EnvScope, EnvSelection,
    Message, ResponseState, ResponseView, StatusClass, SubTab, Tab, TabKind, TabMsg,
};
use rest_harness::domain::{HttpMethod, RequestBody};

use crate::gui::App;

/// All methods offered by the method pick list.
const METHODS: [HttpMethod; 7] = [
    HttpMethod::Get,
    HttpMethod::Post,
    HttpMethod::Put,
    HttpMethod::Patch,
    HttpMethod::Delete,
    HttpMethod::Head,
    HttpMethod::Options,
];

/// Label shown for "no environment selected".
const NO_ENVIRONMENT: &str = "No Environment";

/// Render the composer pane (tab strip + active tab editor + response).
pub fn view(app: &App) -> Element<'_, Message> {
    let state = &app.state;
    let Some(tab) = state.active_tab() else {
        return empty_state();
    };

    column![
        tab_strip(state),
        header(state, tab),
        editor_area(app, tab).height(Length::FillPortion(3)),
        response_pane(tab).height(Length::FillPortion(2)),
    ]
    .width(iced::Fill)
    .height(iced::Fill)
    .spacing(6)
    .padding(8)
    .into()
}

/// The pane shown when no tab is open.
fn empty_state() -> Element<'static, Message> {
    container(
        container(
            text("No tabs open — click a request in the sidebar, or press + New.")
                .size(12)
                .style(text::secondary),
        )
        .center(iced::Fill),
    )
    .width(iced::Fill)
    .height(iced::Fill)
    .into()
}

/// The tab strip: one name button (with a dirty dot) + close button per
/// tab.
fn tab_strip(state: &AppState) -> Element<'_, Message> {
    let mut strip = row![].spacing(2);
    for tab in &state.tabs {
        let selected = state.active_tab == Some(tab.id);
        let title = if tab.dirty {
            format!("● {}", tab.title())
        } else {
            tab.title()
        };
        strip = strip.push(
            button(text(title).size(12))
                .on_press(Message::Tab(TabMsg::Select(tab.id)))
                .style(move |theme: &Theme, status| {
                    if selected {
                        button::primary(theme, status)
                    } else {
                        button::secondary(theme, status)
                    }
                })
                .padding(4),
        );
        strip = strip.push(
            button(text("✕").size(10))
                .on_press(Message::Tab(TabMsg::Close(tab.id)))
                .style(button::text)
                .padding(2),
        );
    }
    scrollable(strip.width(iced::Fill))
        .direction(scrollable::Direction::Horizontal(
            scrollable::Scrollbar::default(),
        ))
        .into()
}

/// The composer header: method pick list, URL input (Enter = Send),
/// Send/Cancel/Save, then the environment selector and the env/secrets
/// editor affordances.
fn header<'a>(state: &'a AppState, tab: &'a Tab) -> Element<'a, Message> {
    let method = pick_list(METHODS, Some(tab.request.method), |method| {
        Message::Composer(ComposerMsg::MethodChanged(method))
    })
    .text_size(12);

    let url = text_input("https://…", &tab.request.url)
        .on_input(|value| Message::Composer(ComposerMsg::UrlChanged(value)))
        .on_submit(Message::Composer(ComposerMsg::Send))
        .size(13)
        .width(iced::Fill);

    let mut actions = row![].spacing(4);
    if tab.in_flight {
        actions = actions.push(
            button(text("Cancel").size(12))
                .on_press(Message::Composer(ComposerMsg::CancelSend))
                .style(button::danger)
                .padding(6),
        );
    } else {
        actions = actions.push(
            button(text("Send").size(12))
                .on_press(Message::Composer(ComposerMsg::Send))
                .style(button::primary)
                .padding(6),
        );
    }
    actions = actions.push(
        button(text("Save").size(12))
            .on_press(Message::Composer(ComposerMsg::Save))
            .style(button::secondary)
            .padding(6),
    );

    let first = row![method, url, actions]
        .align_y(alignment::Vertical::Center)
        .spacing(6)
        .width(iced::Fill);

    let scoped = tab.env.is_some();
    let edit_button = |label: &'static str, enabled: bool, message: Message| {
        let mut button = button(text(label).size(11))
            .style(button::secondary)
            .padding(4);
        if enabled {
            button = button.on_press(message);
        }
        button
    };
    let env_row = row![
        env_pick_list(state, tab),
        edit_button(
            "Edit Environments",
            scoped,
            Message::Composer(ComposerMsg::OpenEditEnvironments),
        ),
        edit_button(
            "Edit Secrets",
            scoped,
            Message::Composer(ComposerMsg::OpenEditSecrets),
        ),
    ]
    .align_y(alignment::Vertical::Center)
    .spacing(6);

    column![first, env_row].spacing(4).into()
}

/// The environment pick list: "No Environment" plus the scoped
/// Collections' environments (rebuilt from the workspace on every render).
fn env_pick_list<'a>(state: &'a AppState, tab: &'a Tab) -> Element<'a, Message> {
    let options = env_options(state, tab);
    let selected = options
        .iter()
        .find(|(_, scope)| *scope == tab.env)
        .map(|(label, _)| label.clone())
        .unwrap_or_else(|| NO_ENVIRONMENT.to_string());

    let labels: Vec<String> = options.iter().map(|(label, _)| label.clone()).collect();
    pick_list(labels, Some(selected), move |label| {
        let scope = options
            .iter()
            .find(|(candidate, _)| candidate == &label)
            .and_then(|(_, scope)| scope.clone());
        Message::Composer(ComposerMsg::EnvSelected(scope))
    })
    .text_size(12)
    .into()
}

/// The environment options of a tab, as (label, scope) pairs:
/// - Saved tabs are scoped to their own Collection (its environments plus
///   "No Environment" = the Collection's shared secrets only);
/// - Draft/HistoryDraft tabs may pick ANY Collection's environment (spec
///   story 23).
fn env_options(state: &AppState, tab: &Tab) -> Vec<(String, EnvSelection)> {
    let workspace = state.workspace.as_ref();
    match &tab.kind {
        TabKind::Saved { .. } => {
            let collection_dir = tab
                .env
                .as_ref()
                .map(|scope| scope.collection_dir.clone())
                .unwrap_or_default();
            let mut options = vec![(
                NO_ENVIRONMENT.to_string(),
                Some(EnvScope {
                    collection_dir: collection_dir.clone(),
                    env_name: None,
                }),
            )];
            if let Some(collection) = workspace.and_then(|workspace| {
                workspace
                    .collections
                    .iter()
                    .find(|collection| collection.path == collection_dir)
            }) {
                for env in &collection.environments {
                    options.push((
                        env.name.clone(),
                        Some(EnvScope {
                            collection_dir: collection_dir.clone(),
                            env_name: Some(env.name.clone()),
                        }),
                    ));
                }
            }
            options
        }
        TabKind::Draft | TabKind::HistoryDraft => {
            let mut options = vec![(NO_ENVIRONMENT.to_string(), None)];
            for collection in workspace
                .map(|workspace| &workspace.collections)
                .into_iter()
                .flatten()
            {
                for env in &collection.environments {
                    options.push((
                        format!("{} · {}", collection.name, env.name),
                        Some(EnvScope {
                            collection_dir: collection.path.clone(),
                            env_name: Some(env.name.clone()),
                        }),
                    ));
                }
            }
            options
        }
    }
}

/// The editor area: sub-tab switcher plus the params/headers/body editor.
fn editor_area<'a>(app: &'a App, tab: &'a Tab) -> iced::widget::Column<'a, Message> {
    let subtab_button = |label: String, subtab: SubTab| {
        let selected = tab.active_subtab == subtab;
        button(text(label).size(12))
            .on_press(Message::Composer(ComposerMsg::SubTabSelected(subtab)))
            .style(move |theme: &Theme, status| {
                if selected {
                    button::primary(theme, status)
                } else {
                    button::secondary(theme, status)
                }
            })
            .padding(4)
    };

    let switcher = row![
        subtab_button("Params".to_string(), SubTab::Params),
        // User Story 21: the badge counts enabled header rows only.
        subtab_button(
            format!(
                "Headers ({})",
                tab.request.headers.iter().filter(|h| h.enabled).count()
            ),
            SubTab::Headers,
        ),
        subtab_button("Body".to_string(), SubTab::Body),
    ]
    .spacing(4);

    let content: Element<'_, Message> = match tab.active_subtab {
        SubTab::Params => params_editor(tab),
        SubTab::Headers => headers_editor(tab),
        SubTab::Body => body_editor(app, tab),
    };

    column![switcher, content].spacing(4)
}

/// One editable key/value row: checkbox + key + value + description +
/// remove button. Everything is owned, so the element is `<'static>`.
#[allow(clippy::too_many_arguments)]
fn kv_row(
    enabled: bool,
    key: String,
    value: String,
    description: String,
    toggled: Message,
    key_changed: impl Fn(String) -> Message + 'static,
    value_changed: impl Fn(String) -> Message + 'static,
    description_changed: impl Fn(String) -> Message + 'static,
    removed: Message,
) -> Element<'static, Message> {
    row![
        checkbox(enabled)
            .on_toggle(move |_| toggled.clone())
            .size(14),
        text_input("Key", &key)
            .on_input(key_changed)
            .size(12)
            .width(Length::FillPortion(2)),
        text_input("Value", &value)
            .on_input(value_changed)
            .size(12)
            .width(Length::FillPortion(3)),
        text_input("Description", &description)
            .on_input(description_changed)
            .size(12)
            .width(Length::FillPortion(3)),
        button(text("✕").size(10))
            .on_press(removed)
            .style(button::text)
            .padding(2),
    ]
    .align_y(alignment::Vertical::Center)
    .spacing(4)
    .into()
}

/// The params table plus an "add row" button. Every key/value/toggle edit
/// rebuilds the URL (core-side, via `url_query::sync_params_to_url`).
fn params_editor(tab: &Tab) -> Element<'static, Message> {
    let mut list = column![].spacing(2);
    for (index, row_data) in tab.request.params.iter().enumerate() {
        list = list.push(kv_row(
            row_data.enabled,
            row_data.key.clone(),
            row_data.value.clone(),
            row_data.description.clone(),
            Message::Composer(ComposerMsg::ParamToggled(index)),
            move |value| Message::Composer(ComposerMsg::ParamKeyChanged(index, value)),
            move |value| Message::Composer(ComposerMsg::ParamValueChanged(index, value)),
            move |value| Message::Composer(ComposerMsg::ParamDescriptionChanged(index, value)),
            Message::Composer(ComposerMsg::ParamRemoved(index)),
        ));
    }
    list = list.push(
        button(text("+ Add param").size(11))
            .on_press(Message::Composer(ComposerMsg::ParamAdded))
            .style(button::secondary)
            .padding(4),
    );
    scrollable(list).height(iced::Fill).into()
}

/// The headers table plus an "add row" button.
fn headers_editor(tab: &Tab) -> Element<'static, Message> {
    let mut list = column![].spacing(2);
    for (index, row_data) in tab.request.headers.iter().enumerate() {
        list = list.push(kv_row(
            row_data.enabled,
            row_data.key.clone(),
            row_data.value.clone(),
            row_data.description.clone(),
            Message::Composer(ComposerMsg::HeaderToggled(index)),
            move |value| Message::Composer(ComposerMsg::HeaderKeyChanged(index, value)),
            move |value| Message::Composer(ComposerMsg::HeaderValueChanged(index, value)),
            move |value| Message::Composer(ComposerMsg::HeaderDescriptionChanged(index, value)),
            Message::Composer(ComposerMsg::HeaderRemoved(index)),
        ));
    }
    list = list.push(
        button(text("+ Add header").size(11))
            .on_press(Message::Composer(ComposerMsg::HeaderAdded))
            .style(button::secondary)
            .padding(4),
    );
    scrollable(list).height(iced::Fill).into()
}

/// The body editor: None/JSON/Text mode toggles plus a multiline
/// `text_editor` for raw content (the shell owns the editor content).
fn body_editor<'a>(app: &'a App, tab: &'a Tab) -> Element<'a, Message> {
    let mode = tab.body_mode();
    let mode_button = |label: &'static str, target: BodyMode| {
        let selected = mode == target;
        button(text(label).size(11))
            .on_press(Message::Composer(ComposerMsg::BodyModeChanged(target)))
            .style(move |theme: &Theme, status| {
                if selected {
                    button::primary(theme, status)
                } else {
                    button::secondary(theme, status)
                }
            })
            .padding(4)
    };
    let switcher = row![
        mode_button("None", BodyMode::None),
        mode_button("JSON", BodyMode::Json),
        mode_button("Text", BodyMode::Text),
    ]
    .spacing(4);

    let editor: Element<'_, Message> = match &tab.request.body {
        RequestBody::None => container(text("No body").size(12).style(text::secondary))
            .padding(4)
            .into(),
        RequestBody::Raw { .. } => match app.editors.get(&tab.id) {
            Some(content) => text_editor(content)
                .on_action(|action| {
                    Message::Composer(ComposerMsg::BodyEdit(core_body_edit(action)))
                })
                .size(13)
                .height(iced::Fill)
                .padding(4)
                .into(),
            // Defensive: the shell syncs one editor per tab after every
            // message; a plain input renders until then.
            None => text_input("Body", &tab.body_text())
                .size(13)
                .padding(4)
                .into(),
        },
    };

    column![switcher, editor].spacing(4).into()
}

/// Map an iced editor action into the core-owned mirror.
fn core_body_edit(action: text_editor::Action) -> BodyEdit {
    use text_editor::{Action, Edit};
    match action {
        Action::Edit(Edit::Insert(character)) => BodyEdit::Insert(character),
        Action::Edit(Edit::Paste(text)) => BodyEdit::Paste((*text).clone()),
        Action::Edit(Edit::Enter) => BodyEdit::Enter,
        Action::Edit(Edit::Indent) => BodyEdit::Indent,
        Action::Edit(Edit::Unindent) => BodyEdit::Unindent,
        Action::Edit(Edit::Backspace) => BodyEdit::Backspace,
        Action::Edit(Edit::Delete) => BodyEdit::Delete,
        Action::Move(motion) => BodyEdit::Move(core_motion(motion)),
        Action::Select(motion) => BodyEdit::Select(core_motion(motion)),
        Action::SelectWord => BodyEdit::SelectWord,
        Action::SelectLine => BodyEdit::SelectLine,
        Action::SelectAll => BodyEdit::SelectAll,
        Action::Click(point) => BodyEdit::Click {
            x: point.x,
            y: point.y,
        },
        Action::Drag(point) => BodyEdit::Drag {
            x: point.x,
            y: point.y,
        },
        Action::Scroll { lines } => BodyEdit::Scroll { lines },
    }
}

/// Map an iced motion into the core mirror.
fn core_motion(motion: text_editor::Motion) -> BodyMotion {
    use text_editor::Motion;
    match motion {
        Motion::Left => BodyMotion::Left,
        Motion::Right => BodyMotion::Right,
        Motion::Up => BodyMotion::Up,
        Motion::Down => BodyMotion::Down,
        Motion::WordLeft => BodyMotion::WordLeft,
        Motion::WordRight => BodyMotion::WordRight,
        Motion::Home => BodyMotion::Home,
        Motion::End => BodyMotion::End,
        Motion::PageUp => BodyMotion::PageUp,
        Motion::PageDown => BodyMotion::PageDown,
        Motion::DocumentStart => BodyMotion::DocumentStart,
        Motion::DocumentEnd => BodyMotion::DocumentEnd,
    }
}

/// The response inspector: status badge + time/size, Headers/Body view
/// tabs, and the body with a Pretty|Raw toggle, JSON highlighting and a
/// Copy button.
fn response_pane<'a>(tab: &'a Tab) -> iced::widget::Column<'a, Message> {
    if tab.in_flight {
        return column![container(text("Sending…").size(12).style(text::secondary)).padding(6)];
    }

    let mut pane = column![].spacing(4);
    if let Some(error) = &tab.send_error {
        pane = pane.push(text(error.clone()).size(12).style(text::danger));
    }

    let Some(response) = &tab.response else {
        pane = pane.push(
            container(
                text("Send a request to see the response here.")
                    .size(12)
                    .style(text::secondary),
            )
            .center(iced::Fill),
        );
        return pane;
    };

    // Status bar: colored badge + time + size.
    pane = pane.push(
        row![
            status_badge(response.status_code, &response.reason_phrase),
            text(format!("Time: {} ms", response.elapsed_ms))
                .size(11)
                .style(text::secondary),
            text(format!("Size: {}", human_size(response.size_bytes)))
                .size(11)
                .style(text::secondary),
        ]
        .align_y(alignment::Vertical::Center)
        .spacing(10),
    );

    // View tabs: Headers (N) | Body.
    let view_button = |label: String, view: ResponseView| {
        let selected = response.view == view;
        button(text(label).size(11))
            .on_press(Message::Composer(ComposerMsg::ResponseViewSelected(view)))
            .style(move |theme: &Theme, status| {
                if selected {
                    button::primary(theme, status)
                } else {
                    button::secondary(theme, status)
                }
            })
            .padding(4)
    };
    pane = pane.push(
        row![
            view_button(
                format!("Headers ({})", response.headers.len()),
                ResponseView::Headers
            ),
            view_button("Body".to_string(), ResponseView::Body),
        ]
        .spacing(4),
    );

    match response.view {
        ResponseView::Headers => {
            let mut table = column![].spacing(2);
            for header in &response.headers {
                table = table.push(
                    row![
                        text(header.name.clone()).size(12),
                        text(header.value.clone()).size(12).style(text::secondary),
                    ]
                    .spacing(8),
                );
            }
            pane = pane.push(scrollable(table).height(iced::Fill));
        }
        ResponseView::Body => {
            let body_button = |label: &'static str, view: BodyView| {
                let selected = tab.active_body_view == view;
                button(text(label).size(11))
                    .on_press(Message::Composer(ComposerMsg::BodyViewSelected(view)))
                    .style(move |theme: &Theme, status| {
                        if selected {
                            button::primary(theme, status)
                        } else {
                            button::secondary(theme, status)
                        }
                    })
                    .padding(4)
            };
            let toggles = row![
                body_button("Pretty", BodyView::Pretty),
                body_button("Raw", BodyView::Raw),
                button(text("Copy").size(11))
                    .on_press(Message::Composer(ComposerMsg::CopyBody))
                    .style(button::secondary)
                    .padding(4),
            ]
            .spacing(4);
            pane = pane.push(toggles);

            let body_text = match tab.active_body_view {
                BodyView::Pretty => &response.body_pretty,
                BodyView::Raw => &response.body_raw,
            };
            if body_text.trim().is_empty() {
                pane = pane.push(text("(empty body)").size(12).style(text::secondary));
            } else {
                pane = pane.push(scrollable(body_lines(body_text)).height(iced::Fill));
            }
        }
    }

    pane
}

/// A colored status badge: 2xx green, 3xx blue, 4xx amber, 5xx red.
fn status_badge(status_code: u16, reason: &str) -> iced::widget::Container<'static, Message> {
    let color = match ResponseState::status_class(status_code) {
        StatusClass::Success => Color::from_rgb8(0x3F, 0xB9, 0x50),
        StatusClass::Redirect => Color::from_rgb8(0x44, 0x93, 0xF8),
        StatusClass::ClientError => Color::from_rgb8(0xD2, 0x99, 0x22),
        StatusClass::ServerError => Color::from_rgb8(0xF8, 0x51, 0x49),
        StatusClass::Other => Color::from_rgb8(0x8B, 0x94, 0x9E),
    };
    let label = if reason.is_empty() {
        status_code.to_string()
    } else {
        format!("{status_code} {reason}")
    };
    container(
        text(label)
            .size(11)
            .style(|_| text::Style {
                color: Some(Color::WHITE),
            })
            .center(),
    )
    .padding(iced::padding::all(3.0))
    .style(move |_| container::Style {
        background: Some(color.into()),
        border: iced::border::rounded(3),
        ..container::Style::default()
    })
}

/// The body rendered as (line number, spans) rows: JSON-highlighted when
/// the text parses as JSON, plain otherwise.
fn body_lines(body: &str) -> iced::widget::Column<'static, Message> {
    let lines: Vec<Vec<(String, JsonTokenKind)>> = match tokenize_json(body) {
        Some(tokens) => {
            let mut lines: Vec<Vec<(String, JsonTokenKind)>> = vec![Vec::new()];
            for token in tokens {
                let parts: Vec<&str> = token.text.split('\n').collect();
                for (index, part) in parts.into_iter().enumerate() {
                    if index > 0 {
                        lines.push(Vec::new());
                    }
                    if !part.is_empty() {
                        lines
                            .last_mut()
                            .expect("at least one line")
                            .push((part.to_string(), token.kind));
                    }
                }
            }
            lines
        }
        None => body
            .lines()
            .map(|line| vec![(line.to_string(), JsonTokenKind::Plain)])
            .collect(),
    };

    let mut body_column = column![].spacing(0);
    for (index, line) in lines.into_iter().enumerate() {
        let spans: Vec<iced::widget::text::Span<'static, (), iced::Font>> = line
            .into_iter()
            .map(|(text, kind)| {
                let mut span = iced::widget::span(text);
                if let Some(color) = token_color(kind) {
                    span = span.color(color);
                }
                span
            })
            .collect();
        body_column = body_column.push(
            row![
                text((index + 1).to_string())
                    .size(11)
                    .style(text::secondary)
                    .width(Length::Fixed(32.0)),
                rich_text(spans).size(12),
            ]
            .spacing(4),
        );
    }
    body_column.padding(4)
}

/// Highlight color per token kind (GitHub-dark-ish); `None` inherits the
/// default text color.
fn token_color(kind: JsonTokenKind) -> Option<Color> {
    match kind {
        JsonTokenKind::Key => Some(Color::from_rgb8(0xFF, 0x7B, 0x72)),
        JsonTokenKind::String => Some(Color::from_rgb8(0xA5, 0xD6, 0xFF)),
        JsonTokenKind::Number => Some(Color::from_rgb8(0x79, 0xC0, 0xFF)),
        JsonTokenKind::Boolean | JsonTokenKind::Null => Some(Color::from_rgb8(0xD2, 0xA8, 0xFF)),
        JsonTokenKind::Plain => None,
    }
}
