//! Left sidebar: `Collections`/`History` tabs, live filter box, the
//! Collections tree (expand/collapse, count + method badges, row CRUD
//! actions) and the History list.

use iced::widget::{button, column, container, scrollable, text, text_input, Space};
use iced::{alignment, Color, Element, Length, Theme};
use rest_harness::app::tree::{build_tree, SidebarNode};
use rest_harness::app::{AppState, CrudMsg, Message, NodeKind, SidebarMsg, SidebarTab};
use rest_harness::domain::HttpMethod;

/// Width of the sidebar column.
const SIDEBAR_WIDTH: f32 = 320.0;

/// Indentation per tree depth.
const INDENT_PER_DEPTH: f32 = 14.0;

/// Render the sidebar (tabs + tab content).
pub fn view(state: &AppState) -> Element<'_, Message> {
    let content: Element<'_, Message> = match state.sidebar_tab {
        SidebarTab::Collections => collections_view(state),
        SidebarTab::History => history_view(state),
    };
    column![tabs(state), content]
        .width(SIDEBAR_WIDTH)
        .height(Length::Fill)
        .spacing(6)
        .padding(6)
        .into()
}

/// Collections/History tab toggle.
fn tabs(state: &AppState) -> Element<'_, Message> {
    let tab_button = |label: &'static str, tab: SidebarTab| {
        let selected = state.sidebar_tab == tab;
        let mut button = button(
            text(label)
                .size(12)
                .style(move |theme: &Theme| {
                    if selected {
                        text::Style {
                            color: Some(theme.extended_palette().primary.base.text),
                        }
                    } else {
                        text::Style { color: None }
                    }
                })
                .center(),
        )
        .width(Length::Fill)
        .padding(4);
        if selected {
            button = button.style(button::primary);
        }
        button.on_press(Message::Sidebar(SidebarMsg::TabSelected(tab)))
    };
    iced::widget::row![
        tab_button("Collections", SidebarTab::Collections),
        tab_button("History", SidebarTab::History)
    ]
    .spacing(4)
    .into()
}

/// The Collections tab: filter box, `+ New Collection`, the tree.
fn collections_view(state: &AppState) -> Element<'_, Message> {
    column![
        text_input("Filter", &state.filter)
            .on_input(|value| Message::Sidebar(SidebarMsg::FilterChanged(value)))
            .padding(4)
            .size(12),
        button(text("+ New Collection").size(12))
            .on_press(Message::Crud(CrudMsg::NewCollection))
            .style(button::secondary)
            .width(Length::Fill)
            .padding(4),
        tree_view(state),
    ]
    .spacing(4)
    .height(Length::Fill)
    .into()
}

/// The tree (or an empty-state hint when no workspace is loaded).
fn tree_view(state: &AppState) -> Element<'_, Message> {
    let Some(workspace) = &state.workspace else {
        return container(text("No workspace loaded").size(12).style(text::secondary))
            .center(iced::Fill)
            .into();
    };

    let nodes = build_tree(workspace, &state.filter);
    if nodes.is_empty() {
        return container(
            text(if state.filter.trim().is_empty() {
                "No collections yet"
            } else {
                "No matches"
            })
            .size(12)
            .style(text::secondary),
        )
        .into();
    }

    let mut tree: iced::widget::Column<'static, Message> = column![].spacing(1);
    for node in &nodes {
        tree = push_node(state, tree, node);
    }
    scrollable(tree).height(Length::Fill).into()
}

/// Recursively append `node` (when visible) to `tree`.
fn push_node(
    state: &AppState,
    mut tree: iced::widget::Column<'static, Message>,
    node: &SidebarNode,
) -> iced::widget::Column<'static, Message> {
    tree = tree.push(tree_row(state, node));
    // Force-expanded nodes (filter matches below) are always open; the rest
    // obey the user's expanded set.
    let open = node.force_expanded || state.expanded.contains(&node.path);
    if open {
        for child in &node.children {
            tree = push_node(state, tree, child);
        }
    }
    tree
}

/// One tree row: indent, chevron, name, badges, action glyphs.
/// Everything is owned, so the element is `<'static>`.
fn tree_row(state: &AppState, node: &SidebarNode) -> Element<'static, Message> {
    let indent = node.depth as f32 * INDENT_PER_DEPTH;

    // Chevron: containers only (disabled when there is nothing to expand).
    let chevron = match node.kind {
        NodeKind::Request => None,
        _ => {
            let open = node.force_expanded || state.expanded.contains(&node.path);
            let mut button = button(
                text(if open { "▾" } else { "▸" })
                    .size(10)
                    .style(text::secondary),
            )
            .padding(2)
            .width(18);
            if !node.children.is_empty() {
                button =
                    button.on_press(Message::Sidebar(SidebarMsg::NodeToggled(node.path.clone())));
            }
            Some(button)
        }
    };

    // Name: activate requests, toggle containers.
    let name_label = text(node.name.clone()).size(12).center();
    let name_button = match node.kind {
        NodeKind::Request => button(name_label)
            .on_press(Message::Sidebar(SidebarMsg::NodeActivated(
                node.path.clone(),
            )))
            .padding(2),
        _ => button(name_label)
            .on_press(Message::Sidebar(SidebarMsg::NodeToggled(node.path.clone())))
            .padding(2),
    };

    let badges = badges(state, node);

    let mut actions = iced::widget::row![].spacing(2);
    let action_button = |glyph: &str, message: Message| {
        button(text(glyph.to_string()).size(10).center())
            .on_press(message)
            .padding(2)
            .style(button::text)
    };
    match node.kind {
        NodeKind::Request => {}
        _ => {
            actions = actions.push(action_button(
                "+f",
                Message::Crud(CrudMsg::NewFolder(node.path.clone())),
            ));
            actions = actions.push(action_button(
                "+r",
                Message::Crud(CrudMsg::NewRequest(node.path.clone())),
            ));
        }
    }
    actions = actions.push(action_button(
        "✎",
        Message::Crud(CrudMsg::Rename(node.path.clone())),
    ));
    // User Story 9: no Duplicate on a Collection (duplicating a Collection
    // directory and its `.secrets.yaml` is not offered in v1). Rename and
    // Delete stay available everywhere.
    if matches!(node.kind, NodeKind::Folder | NodeKind::Request) {
        actions = actions.push(action_button(
            "⧉",
            Message::Crud(CrudMsg::Duplicate(node.path.clone())),
        ));
    }
    actions = actions.push(action_button(
        "✕",
        Message::Crud(CrudMsg::Delete(node.path.clone())),
    ));

    let mut content = iced::widget::row![]
        .align_y(alignment::Vertical::Center)
        .spacing(2);
    match chevron {
        Some(chevron) => content = content.push(chevron),
        None => content = content.push(Space::new().width(18)),
    }
    content = content
        .push(
            container(name_button)
                .width(Length::Fill)
                .padding(iced::padding::left(indent)),
        )
        .push(badges)
        .push(actions);

    container(content)
        .width(Length::Fill)
        .padding(iced::padding::left(4.0))
        .into()
}

/// Count badge for containers (`total_request_count`), method badge for
/// requests.
fn badges(state: &AppState, node: &SidebarNode) -> Element<'static, Message> {
    match node.kind {
        NodeKind::Request => {
            let method = node
                .method
                .or_else(|| state.request_methods.get(&node.path).copied());
            match method {
                Some(method) => method_badge(method).into(),
                None => text("").size(10).into(),
            }
        }
        _ => container(
            text(node.request_count.to_string())
                .size(10)
                .style(text::secondary),
        )
        .padding(iced::padding::all(2.0))
        .into(),
    }
}

/// Small colored method badge.
fn method_badge(method: HttpMethod) -> iced::widget::Container<'static, Message> {
    let (label, color) = match method {
        HttpMethod::Get => ("GET", Color::from_rgb8(0x3F, 0xB9, 0x50)), // green
        HttpMethod::Post => ("POST", Color::from_rgb8(0xD2, 0x99, 0x22)), // amber
        HttpMethod::Put => ("PUT", Color::from_rgb8(0x44, 0x93, 0xF8)), // blue
        HttpMethod::Patch => ("PATCH", Color::from_rgb8(0xDB, 0x6D, 0x28)), // orange
        HttpMethod::Delete => ("DELETE", Color::from_rgb8(0xF8, 0x51, 0x49)), // red
        HttpMethod::Head => ("HEAD", Color::from_rgb8(0x39, 0xC5, 0xCF)), // teal
        HttpMethod::Options => ("OPTIONS", Color::from_rgb8(0xA3, 0x71, 0xF7)), // purple
    };

    container(text(label).size(9).style(|_| text::Style {
        color: Some(Color::WHITE),
    }))
    .padding(iced::padding::all(2.0))
    .style(move |_| container::Style {
        background: Some(color.into()),
        border: iced::border::rounded(3),
        ..container::Style::default()
    })
}

/// The History tab: newest-first entries with method badge, URL and a
/// `status · time · size` summary line.
fn history_view(state: &AppState) -> Element<'_, Message> {
    let Some(workspace) = &state.workspace else {
        return container(text("No workspace loaded").size(12).style(text::secondary))
            .center(iced::Fill)
            .into();
    };
    if workspace.history.is_empty() {
        return container(text("No history yet").size(12).style(text::secondary)).into();
    }

    let mut list = column![].spacing(2);
    for (index, entry) in workspace.history.iter().enumerate() {
        let selected = state.history_selected == Some(index);
        let summary = format!(
            "{} {} · {} ms · {} B",
            entry.response.status_code,
            entry.response.reason_phrase,
            entry.response.elapsed_ms,
            entry.response.size_bytes
        );

        let content = iced::widget::row![
            method_badge(entry.request.method),
            column![
                text(entry.request.url.clone()).size(11).width(Length::Fill),
                text(summary).size(10).style(text::secondary),
            ]
            .spacing(1)
            .width(Length::Fill),
        ]
        .align_y(alignment::Vertical::Center)
        .spacing(4);

        let card = container(content)
            .width(Length::Fill)
            .padding(iced::padding::all(4.0));
        let mut item = button(card).width(Length::Fill).padding(0);
        if selected {
            item = item.style(button::primary);
        }
        list = list.push(item.on_press(Message::Sidebar(SidebarMsg::HistoryEntrySelected(index))));
    }

    scrollable(list).height(Length::Fill).into()
}
