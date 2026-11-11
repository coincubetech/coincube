use iced::widget::container::Style;
use iced::{Background, Border};

use super::palette::ContainerPalette;
use super::Theme;

fn notification(palette: &ContainerPalette) -> Style {
    Style {
        background: Some(Background::Color(palette.background)),
        text_color: palette.text,
        border: if let Some(color) = palette.border {
            Border {
                width: 1.0,
                color,
                radius: 25.0.into(),
            }
        } else {
            Border {
                ..Default::default()
            }
        },
        ..Default::default()
    }
}

pub fn pending(theme: &Theme) -> Style {
    notification(&theme.colors.notifications.pending)
}

pub fn error(theme: &Theme) -> Style {
    notification(&theme.colors.notifications.error)
}

pub fn success(theme: &Theme) -> Style {
    notification(&theme.colors.notifications.success)
}

pub fn warning(theme: &Theme) -> Style {
    notification(&theme.colors.notifications.warning)
}

pub fn info(theme: &Theme) -> Style {
    notification(&theme.colors.notifications.info)
}

/// Severity of a toast. Sets the icon, its tint and whether the border is
/// tinted; every toast otherwise shares the panel surface.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ToastTone {
    /// Confirmations and background progress.
    Info,
    /// A user action finished and stuck.
    Success,
    /// Something degraded but the Cube still works.
    Warning,
    /// An action failed.
    Error,
    /// Developer-only detail.
    Debug,
}

impl From<log::Level> for ToastTone {
    fn from(level: log::Level) -> Self {
        match level {
            log::Level::Error => ToastTone::Error,
            log::Level::Warn => ToastTone::Warning,
            log::Level::Info => ToastTone::Info,
            log::Level::Debug | log::Level::Trace => ToastTone::Debug,
        }
    }
}

impl ToastTone {
    /// Tone colour for the icon and, on toasts that need attention, the border.
    pub fn color(self, theme: &Theme) -> iced::Color {
        use super::tenshu::Tone;
        match self {
            // Info has no chip tone of its own; it borrows the focus blue.
            ToastTone::Info => theme.colors.shell.focus,
            ToastTone::Success => Tone::Good.color(theme),
            ToastTone::Warning => Tone::Warn.color(theme),
            ToastTone::Error => Tone::Bad.color(theme),
            ToastTone::Debug => Tone::Muted.color(theme),
        }
    }

    /// Warnings and errors tint their border so they stand apart from
    /// confirmations.
    pub fn needs_attention(self) -> bool {
        matches!(self, ToastTone::Warning | ToastTone::Error)
    }
}

/// Opacity of the tone on a tinted toast border.
const TOAST_BORDER_ALPHA: f32 = 0.4;
/// Opacity of the tone behind the toast icon.
const TOAST_TILE_ALPHA: f32 = 0.13;

/// Toast surface: the menu/popover panel (`tenshu::surface::panel`) with a
/// tone-tinted border on warnings and errors.
pub fn toast(theme: &Theme, tone: ToastTone) -> Style {
    let mut style = super::tenshu::surface::panel(theme);
    if tone.needs_attention() {
        style.border.color = super::tenshu::faded(tone.color(theme), TOAST_BORDER_ALPHA);
    }
    style
}

/// The 28px tile behind a toast's icon: the tone at low opacity.
pub fn toast_icon_tile(theme: &Theme, tone: ToastTone) -> Style {
    Style {
        text_color: Some(tone.color(theme)),
        background: Some(Background::Color(super::tenshu::faded(
            tone.color(theme),
            TOAST_TILE_ALPHA,
        ))),
        border: Border {
            radius: super::tenshu::radius::ROW.into(),
            ..Border::default()
        },
        ..Style::default()
    }
}

#[cfg(test)]
mod toast_tests {
    use super::{toast, ToastTone};
    use crate::theme::tenshu::contrast;
    use crate::theme::Theme;

    const TONES: [ToastTone; 5] = [
        ToastTone::Info,
        ToastTone::Success,
        ToastTone::Warning,
        ToastTone::Error,
        ToastTone::Debug,
    ];

    #[test]
    fn log_levels_map_to_tones() {
        assert_eq!(ToastTone::from(log::Level::Error), ToastTone::Error);
        assert_eq!(ToastTone::from(log::Level::Warn), ToastTone::Warning);
        assert_eq!(ToastTone::from(log::Level::Info), ToastTone::Info);
        assert_eq!(ToastTone::from(log::Level::Debug), ToastTone::Debug);
        assert_eq!(ToastTone::from(log::Level::Trace), ToastTone::Debug);
    }

    #[test]
    fn only_warnings_and_errors_tint_the_border() {
        let theme = Theme::dark();
        for tone in TONES {
            let border = toast(&theme, tone).border.color;
            assert_eq!(
                border == theme.colors.shell.line,
                !tone.needs_attention(),
                "{tone:?}"
            );
        }
    }

    /// Icon glyphs are non-text graphics: WCAG asks 3:1 against the panel.
    #[test]
    fn tone_icons_hold_contrast_on_the_panel_in_both_themes() {
        for theme in [Theme::dark(), Theme::light()] {
            let panel = theme.colors.shell.panel;
            for tone in TONES {
                let ratio = contrast(tone.color(&theme), panel);
                assert!(ratio >= 3.0, "{tone:?} is {ratio:.2}:1 on the panel");
            }
            // The dismiss cross is drawn in `shell.muted` (Tenshu icon action).
            let cross = contrast(theme.colors.shell.muted, panel);
            assert!(cross >= 3.0, "dismiss cross is {cross:.2}:1 on the panel");
        }
    }
}
