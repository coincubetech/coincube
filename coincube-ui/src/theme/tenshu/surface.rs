//! Container styles for Tenshu surfaces.

use iced::widget::container::Style;
use iced::{Background, Border, Shadow, Vector};

use super::{radius, Theme};

/// Wallet card (`.card`): panel fill, 1px line, 16px radius.
pub fn card(theme: &Theme) -> Style {
    let shell = &theme.colors.shell;
    Style {
        text_color: Some(shell.ink),
        background: Some(Background::Color(shell.panel)),
        border: Border {
            color: shell.line,
            width: 1.0,
            radius: radius::CARD.into(),
        },
        ..Style::default()
    }
}

/// Menu, popover or sheet (`.menu`): card treatment at a 10px radius with the
/// shell shadow.
pub fn panel(theme: &Theme) -> Style {
    let shell = &theme.colors.shell;
    Style {
        text_color: Some(shell.ink),
        background: Some(Background::Color(shell.panel)),
        border: Border {
            color: shell.line,
            width: 1.0,
            radius: radius::MENU.into(),
        },
        shadow: Shadow {
            color: shell.shadow,
            offset: Vector::new(0.0, 12.0),
            blur_radius: 32.0,
        },
        ..Style::default()
    }
}

/// Window background behind cards (`--bg`).
pub fn page(theme: &Theme) -> Style {
    let shell = &theme.colors.shell;
    Style {
        text_color: Some(shell.ink),
        background: Some(Background::Color(shell.background)),
        ..Style::default()
    }
}
