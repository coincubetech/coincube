use iced::{theme, widget, Background};
use iced_aw::style::{
    date_picker::{Catalog, Style},
    Status, StyleFn,
};

use super::Theme;

impl Catalog for Theme {
    type Class<'a> = StyleFn<'a, Self, Style>;

    fn default<'a>() -> Self::Class<'a> {
        Box::new(primary)
    }

    fn style(&self, class: &Self::Class<'_>, status: Status) -> Style {
        class(self, status)
    }
}

pub fn primary(theme: &Theme, status: Status) -> Style {
    let modal = theme.colors.cards.modal;
    let mut style = Style {
        background: Background::Color(modal.background),
        border_radius: 18.0,
        border_width: 1.0,
        border_color: modal.border.unwrap_or(theme.colors.rule),
        text_color: theme.colors.text.primary,
        text_attenuated_color: theme.colors.text.secondary,
        day_background: Background::Color(modal.background),
    };
    match status {
        Status::Selected => {
            style.day_background =
                Background::Color(theme.colors.buttons.primary.active.background);
            style.text_color = theme.colors.buttons.primary.active.text;
        }
        Status::Hovered | Status::Pressed => {
            style.day_background =
                Background::Color(theme.colors.buttons.secondary.hovered.background);
            style.text_color = theme.colors.buttons.secondary.hovered.text;
        }
        Status::Focused => {
            style.border_color = theme.colors.buttons.primary.active.background;
        }
        Status::Disabled => {
            style.text_color = theme.colors.text.secondary;
        }
        Status::Active => {}
    }
    style
}

/// Theme scoped to the calendar's always-enabled footer buttons.
///
/// iced_aw 0.13 rebuilds these Buttons when constructing its overlay, losing
/// iced 0.14's cached redraw status. Their draw then falls back to Disabled even
/// though both have on_press handlers. Keep this compatibility fix local to the
/// picker; truly disabled application buttons must retain their usual styling.
#[derive(Clone, Copy)]
pub(crate) struct CalendarTheme(Theme);

impl theme::Base for CalendarTheme {
    fn default(preference: theme::Mode) -> Self {
        Self(match preference {
            theme::Mode::Light => Theme::light(),
            _ => Theme::dark(),
        })
    }

    fn mode(&self) -> theme::Mode {
        self.0.mode()
    }

    fn base(&self) -> theme::Style {
        self.0.base()
    }

    fn palette(&self) -> Option<theme::Palette> {
        self.0.palette()
    }

    fn name(&self) -> &str {
        "CoincubeCalendarTheme"
    }
}

impl Catalog for CalendarTheme {
    type Class<'a> = <Theme as Catalog>::Class<'a>;

    fn default<'a>() -> Self::Class<'a> {
        <Theme as Catalog>::default()
    }

    fn style(&self, class: &Self::Class<'_>, status: Status) -> Style {
        class(&self.0, status)
    }
}

impl widget::button::Catalog for CalendarTheme {
    type Class<'a> = <Theme as widget::button::Catalog>::Class<'a>;

    fn default<'a>() -> Self::Class<'a> {
        <Theme as widget::button::Catalog>::default()
    }

    fn style(
        &self,
        class: &Self::Class<'_>,
        status: widget::button::Status,
    ) -> widget::button::Style {
        let status = match status {
            widget::button::Status::Disabled => widget::button::Status::Active,
            status => status,
        };
        class(&self.0, status)
    }
}

impl widget::text::Catalog for CalendarTheme {
    type Class<'a> = <Theme as widget::text::Catalog>::Class<'a>;

    fn default<'a>() -> Self::Class<'a> {
        <Theme as widget::text::Catalog>::default()
    }

    fn style(&self, class: &Self::Class<'_>) -> widget::text::Style {
        class(&self.0)
    }
}

impl widget::container::Catalog for CalendarTheme {
    type Class<'a> = <Theme as widget::container::Catalog>::Class<'a>;

    fn default<'a>() -> Self::Class<'a> {
        <Theme as widget::container::Catalog>::default()
    }

    fn style(&self, class: &Self::Class<'_>) -> widget::container::Style {
        class(&self.0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn calendar_uses_current_light_and_dark_palettes() {
        let dark = Theme::dark();
        let light = Theme::light();
        for theme in [dark, light] {
            let base = primary(&theme, Status::Active);
            let selected = primary(&theme, Status::Selected);
            assert_eq!(base.text_color, theme.colors.text.primary);
            assert_eq!(
                base.background,
                Background::Color(theme.colors.cards.modal.background)
            );
            assert_eq!(
                selected.day_background,
                Background::Color(theme.colors.buttons.primary.active.background)
            );
            assert_eq!(
                selected.text_color,
                theme.colors.buttons.primary.active.text
            );
        }
        assert_ne!(
            primary(&dark, Status::Active).background,
            primary(&light, Status::Active).background
        );
    }
}
