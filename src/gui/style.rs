//! App-local widget style overrides.

use iced::theme;
use iced::widget::button;

/// Secondary button with light text on a dark surface: the built-in
/// `button::secondary` in `Theme::Dark` is too dim to read.
pub fn secondary(theme: &theme::Theme, status: button::Status) -> button::Style {
    let mut style = button::secondary(theme, status);
    let palette = theme.extended_palette();
    // Dark slate surface; hover/press lift the surface for feedback, and
    // disabled text is dimmed to half alpha but stays legible.
    style.background = Some(match status {
        button::Status::Hovered => palette.background.strong.color.into(),
        _ => palette.background.base.color.into(),
    });
    style.text_color = match status {
        button::Status::Disabled => palette.background.base.text.scale_alpha(0.5),
        _ => palette.background.base.text,
    };
    style
}
