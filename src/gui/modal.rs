//! Modal overlay: dimmed backdrop with a centered card.
//!
//! - Text-input modals (create/rename): Enter confirms (`on_submit`), Esc
//!   cancels (via the shell's keyboard subscription).
//! - Delete confirmation: danger-styled Delete button + Cancel.
//! - Save-draft: target Collection pick list + folder path + request name.
//! - CloseDirty (three actions): Save / Discard / Cancel.
//! - Edit Environments / Edit Secrets: name/value row editors.

use iced::widget::{button, column, container, pick_list, row, scrollable, text, text_input};
use iced::{Color, Element, Theme};
use rest_harness::app::{AppState, Message, Modal, ModalMsg, ModalSlot, NameValue, NodeKind};

use super::style;

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
        Modal::SaveDraft {
            collections,
            target,
            folder,
            name,
            ..
        } => save_draft_card(collections, *target, folder.clone(), name.clone()),
        Modal::CloseDirty { tab_id: _ } => close_dirty_card(),
        Modal::EditEnvironments {
            envs,
            selected,
            new_name,
            rows,
            ..
        } => environments_card(
            envs,
            *selected,
            new_name.clone(),
            rows.clone(),
            ModalSlot::EnvironmentVars,
        ),
        Modal::EditSecrets {
            env_name,
            shared_rows,
            env_rows,
            ..
        } => secrets_card(env_name.clone(), shared_rows.clone(), env_rows.clone()),
    };

    container(container(card).max_width(420).padding(14).style(card_style))
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
        cancel_confirm_row(),
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
                .style(style::secondary)
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

/// Card for saving a Draft: target Collection pick list, nested folder
/// path text ("a/b") and the request name.
fn save_draft_card(
    collections: &[std::path::PathBuf],
    target: usize,
    folder: String,
    name: String,
) -> Element<'static, Message> {
    let labels: Vec<String> = collections
        .iter()
        .map(|path| {
            path.file_name()
                .and_then(|name| name.to_str())
                .unwrap_or("collection")
                .to_string()
        })
        .collect();
    let for_selection = labels.clone();
    let selected = labels.get(target).cloned();
    let pick = pick_list_field(labels, selected, move |label| {
        let index = for_selection
            .iter()
            .position(|candidate| *candidate == label)
            .unwrap_or(0);
        Message::Modal(ModalMsg::TargetCollectionSelected(index))
    });

    column![
        text("Save request").size(14),
        pick,
        text_input("Folder path (a/b, optional)", &folder)
            .on_input(|value| Message::Modal(ModalMsg::FolderChanged(value)))
            .padding(6)
            .size(13),
        text_input("Request name", &name)
            .on_input(|value| Message::Modal(ModalMsg::NameChanged(value)))
            .on_submit(Message::Modal(ModalMsg::Confirm))
            .padding(6)
            .size(13),
        row![
            button(text("Cancel").size(12))
                .on_press(Message::Modal(ModalMsg::Cancel))
                .style(style::secondary)
                .padding(6),
            button(text("Save").size(12))
                .on_press(Message::Modal(ModalMsg::Confirm))
                .style(button::primary)
                .padding(6),
        ]
        .spacing(8),
    ]
    .spacing(10)
    .into()
}

/// Card for closing a dirty tab: Save / Discard / Cancel (Save routes to
/// the save-draft modal for Draft and HistoryDraft tabs).
fn close_dirty_card() -> Element<'static, Message> {
    let buttons = row![
        button(text("Save").size(12))
            .on_press(Message::Modal(ModalMsg::Confirm))
            .style(button::primary)
            .padding(6),
        button(text("Discard").size(12))
            .on_press(Message::Modal(ModalMsg::Discard))
            .style(style::secondary)
            .padding(6),
        button(text("Cancel").size(12))
            .on_press(Message::Modal(ModalMsg::Cancel))
            .style(style::secondary)
            .padding(6),
    ]
    .spacing(8);

    column![
        text("Unsaved changes").size(14),
        text("This tab has unsaved changes. Save before closing?").size(12),
        buttons,
    ]
    .spacing(10)
    .into()
}

/// Card for the Edit Environments modal: environment selector (or
/// "New environment…"), the new-env name input when applicable, the
/// variable rows and Cancel/Save.
fn environments_card(
    envs: &[String],
    selected: usize,
    new_name: String,
    rows: Vec<NameValue>,
    slot: ModalSlot,
) -> Element<'static, Message> {
    const NEW_ENVIRONMENT: &str = "New environment…";
    let mut labels: Vec<String> = envs.to_vec();
    labels.push(NEW_ENVIRONMENT.to_string());
    let for_selection = labels.clone();
    let selected_label = labels.get(selected).cloned();
    let selector: Element<'static, Message> =
        pick_list_field(labels, selected_label, move |label| {
            let index = for_selection
                .iter()
                .position(|candidate| *candidate == label)
                .unwrap_or(0);
            Message::Modal(ModalMsg::ItemSelected(index))
        })
        .into();

    let mut card = column![text("Edit environments").size(14), selector,].spacing(10);

    if selected >= envs.len() {
        card = card.push(
            text_input("New environment name", &new_name)
                .on_input(|value| Message::Modal(ModalMsg::NameChanged(value)))
                .on_submit(Message::Modal(ModalMsg::Confirm))
                .padding(6)
                .size(13),
        );
    }
    card = card
        .push(name_value_rows(rows, slot, "Add variable"))
        .push(row![
            button(text("Cancel").size(12))
                .on_press(Message::Modal(ModalMsg::Cancel))
                .style(style::secondary)
                .padding(6),
            button(text("Save").size(12))
                .on_press(Message::Modal(ModalMsg::Confirm))
                .style(button::primary)
                .padding(6),
        ])
        .spacing(10);
    card.into()
}

/// Card for the Edit Secrets modal: shared secrets plus, when an
/// environment is selected, its per-environment overrides. Values are
/// persisted ONLY to `.secrets.yaml` (ADR 0003).
fn secrets_card(
    env_name: Option<String>,
    shared_rows: Vec<NameValue>,
    env_rows: Vec<NameValue>,
) -> Element<'static, Message> {
    let mut card = column![
        text("Edit secrets").size(14),
        text("Stored only in .secrets.yaml (gitignored).")
            .size(11)
            .style(text::secondary),
        text("Shared secrets (all environments)").size(12),
        name_value_rows(shared_rows, ModalSlot::SharedSecrets, "Add shared secret"),
    ]
    .spacing(10);

    match env_name {
        Some(env) => {
            card = card
                .push(text(format!("Secrets for environment \"{env}\"")).size(12))
                .push(name_value_rows(
                    env_rows,
                    ModalSlot::EnvironmentSecrets,
                    "Add environment secret",
                ));
        }
        None => {
            card = card.push(
                text("Select an environment on the tab to edit its secret overrides.")
                    .size(11)
                    .style(text::secondary),
            );
        }
    }

    card = card.push(row![
        button(text("Cancel").size(12))
            .on_press(Message::Modal(ModalMsg::Cancel))
            .style(style::secondary)
            .padding(6),
        button(text("Save").size(12))
            .on_press(Message::Modal(ModalMsg::Confirm))
            .style(button::primary)
            .padding(6),
    ]);
    card.into()
}

/// Editable name/value rows for an editor modal section: one row per
/// entry (name input + value input + remove) plus an add button.
fn name_value_rows(
    rows: Vec<NameValue>,
    slot: ModalSlot,
    add_label: &'static str,
) -> Element<'static, Message> {
    let mut list = column![].spacing(2);
    for (index, row) in rows.into_iter().enumerate() {
        list = list.push(
            row![
                text_input("Name", &row.name)
                    .on_input(move |value| Message::Modal(ModalMsg::RowKeyChanged(
                        slot, index, value
                    )))
                    .padding(4)
                    .size(12)
                    .width(iced::Length::FillPortion(1)),
                text_input("Value", &row.value)
                    .on_input(move |value| Message::Modal(ModalMsg::RowValueChanged(
                        slot, index, value
                    )))
                    .padding(4)
                    .size(12)
                    .width(iced::Length::FillPortion(2)),
                button(text("×").size(10))
                    .on_press(Message::Modal(ModalMsg::RowRemoved(slot, index)))
                    .style(button::text)
                    .padding(2),
            ]
            .spacing(4),
        );
    }
    list = list.push(
        button(text(format!("+ {add_label}")).size(11))
            .on_press(Message::Modal(ModalMsg::RowAdded(slot)))
            .style(style::secondary)
            .padding(4),
    );
    scrollable(list).height(iced::Length::FillPortion(3)).into()
}

/// A pick list whose options and selection are labels.
fn pick_list_field<T: ToString + PartialEq + Clone + 'static>(
    labels: Vec<T>,
    selected: Option<T>,
    on_select: impl Fn(T) -> Message + 'static,
) -> iced::widget::PickList<'static, T, Vec<T>, T, Message> {
    pick_list(labels, selected, on_select)
}

/// The shared Cancel/Confirm button row.
fn cancel_confirm_row() -> iced::widget::Row<'static, Message> {
    row![
        button(text("Cancel").size(12))
            .on_press(Message::Modal(ModalMsg::Cancel))
            .style(style::secondary)
            .padding(6),
        button(text("Confirm").size(12))
            .on_press(Message::Modal(ModalMsg::Confirm))
            .style(button::primary)
            .padding(6),
    ]
    .spacing(8)
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
