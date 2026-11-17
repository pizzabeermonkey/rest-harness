//! Top workspace bar: app title, workspace root label, workspace switching
//! and reload. The `+ New` button opens a new untitled Draft tab in the
//! request composer.

use iced::widget::{button, container, text, tooltip};
use iced::{alignment, Element, Length, Theme};
use rest_harness::app::{AppState, Message, TabMsg, WorkspaceMsg};

/// Render the top bar.
pub fn view(state: &AppState) -> Element<'_, Message> {
    let root_label = state
        .workspace_root
        .as_ref()
        .map(|root| root.display().to_string())
        .unwrap_or_else(|| "No workspace open".to_string());

    let new_button = button(text("+ New").size(12))
        .on_press(Message::Tab(TabMsg::NewDraft))
        .style(button::secondary);

    container(
        iced::widget::row![
            text("REST Harness").size(16),
            container(text(root_label).size(12).style(text::secondary))
                .center_y(Length::Shrink)
                .padding(iced::padding::left(6)),
            tooltip(
                new_button,
                text("New draft request").size(11),
                tooltip::Position::Bottom
            ),
            button("Switch Workspace…")
                .on_press(Message::Workspace(WorkspaceMsg::PickWorkspace))
                .style(button::secondary),
            button("Reload Workspace")
                .on_press(Message::Workspace(WorkspaceMsg::Reload))
                .style(button::secondary),
        ]
        .align_y(alignment::Vertical::Center)
        .spacing(8)
        .width(iced::Fill),
    )
    .padding(8)
    .width(iced::Fill)
    .style(|theme: &Theme| container::Style {
        background: Some(theme.extended_palette().background.weak.color.into()),
        ..container::Style::default()
    })
    .into()
}
