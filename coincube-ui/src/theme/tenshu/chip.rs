//! Container style for non-interactive status chips (`.chip`).
//!
//! Interactive chips use [`super::button::action`] with
//! [`super::button::ActionShape::Chip`] so hover and focus match.

use iced::widget::container::Style;
use iced::Border;

use super::{radius, Theme};

pub fn chip(theme: &Theme) -> Style {
    let shell = &theme.colors.shell;
    Style {
        text_color: Some(shell.ink),
        background: None,
        border: Border {
            color: shell.line,
            width: 1.0,
            radius: radius::CHIP.into(),
        },
        ..Style::default()
    }
}
