//! Modal overlay: dimmed backdrop with a centered card.
//!
//! - Text-input modals (create/rename): Enter confirms (`on_submit`), Esc
//!   cancels (via the shell's keyboard subscription).
//! - Delete confirmation: danger-styled Delete button + Cancel.

use iced::widget::{button, column, container, row, text, text_input};
use iced::{Color, Element, Theme};
use rest_harness::app::{AppState, Message, Modal, ModalMsg, NodeKind};

/// Render the modal overlay (dimmed backdrop + centered card).
pub fn view(state: &AppState) -> Element<'_, Message> {
    let Some(modal) = &state.modal else {
        // Defensively render nothing; the shell only stacks this when a
        // modal is open.
        return container(container("")).into();
    };

    let card: Element<'static, Message> = match modal {
        Modal::NewCollection { name } => text_card(
            "New collection".to_string(),
            "Collection name",
            name.clone(),
        ),
        Modal::NewFolder { name, .. } => {
            text_card("New folder".to_string(), "Folder name", name.clone())
        }
        Modal::NewRequest { name, .. } => {
            text_card("New request".to_string(), "Request name", name.clone())
        }
        Modal::Rename {
            old_name,
            kind,
            buffer,
            ..
        } => {
            let title = format!("Rename {} \"{}\"", kind.label(), old_name);
            text_card(title, "New name", buffer.clone())
        }
        Modal::ConfirmDelete { path, kind } => delete_card(*kind, display_name(path)),
    };

    container(container(card).max_width(380).padding(14).style(card_style))
        .width(iced::Fill)
        .height(iced::Fill)
        .center(iced::Fill)
        .style(|_| container::Style {
            background: Some(Color::from_rgba8(0x00, 0x00, 0x00, 0.45).into()),
            ..container::Style::default()
        })
        .into()
}

fn display_name(path: &std::path::Path) -> String {
    path.file_stem()
        .or_else(|| path.file_name())
        .and_then(|name| name.to_str())
        .unwrap_or("unknown")
        .to_string()
}

/// Card for name-entry modals: title, input, Cancel/Confirm.
/// Everything is owned, so the element is `<'static>`.
fn text_card(title: String, placeholder: &'static str, value: String) -> Element<'static, Message> {
    column![
        text(title).size(14),
        text_input(placeholder, &value)
            .on_input(|value| Message::Modal(ModalMsg::TextChanged(value)))
            .on_submit(Message::Modal(ModalMsg::Confirm))
            .padding(6)
            .size(13),
        row![
            button(text("Cancel").size(12))
                .on_press(Message::Modal(ModalMsg::Cancel))
                .style(button::secondary)
                .padding(6),
            button(text("Confirm").size(12))
                .on_press(Message::Modal(ModalMsg::Confirm))
                .style(button::primary)
                .padding(6),
        ]
        .spacing(8),
    ]
    .spacing(10)
    .into()
}

/// Card for the delete confirmation.
fn delete_card(kind: NodeKind, name: String) -> Element<'static, Message> {
    column![
        text("Confirm deletion").size(14),
        text(format!(
            "Delete {} \"{name}\"? This cannot be undone.",
            kind.label()
        ))
        .size(12)
        .style(text::danger),
        row![
            button(text("Cancel").size(12))
                .on_press(Message::Modal(ModalMsg::Cancel))
                .style(button::secondary)
                .padding(6),
            button(text("Delete").size(12))
                .on_press(Message::Modal(ModalMsg::Confirm))
                .style(button::danger)
                .padding(6),
        ]
        .spacing(8),
    ]
    .spacing(10)
    .into()
}

/// Card background/border.
fn card_style(theme: &Theme) -> container::Style {
    container::Style {
        background: Some(theme.palette().background.into()),
        border: iced::border::rounded(6)
            .width(1)
            .color(theme.extended_palette().secondary.weak.color),
        shadow: iced::Shadow {
            offset: iced::Vector::new(0.0, 4.0),
            blur_radius: 12.0,
            color: Color::from_rgba8(0x00, 0x00, 0x00, 0.35),
        },
        ..container::Style::default()
    }
}
