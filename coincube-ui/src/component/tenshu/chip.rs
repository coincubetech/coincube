//! Non-interactive status chips and badges. For a pressable chip use
//! [`super::chip_action`].

use std::borrow::Cow;

use iced::advanced::text::Shaping;
use iced::{alignment, Alignment, Length};

use crate::font;
use crate::theme::tenshu::{chip as chip_style, size, text as text_style, type_size, Tone};
use crate::theme::Theme;
use crate::widget::{Container, Element, Row, Text};

/// A status chip (`.chip`): an optional muted icon and key, then the state in
/// its tone colour, such as "Connected" or "Reconnect required".
pub struct StatusChip<'a> {
    key: Option<Cow<'a, str>>,
    state: Cow<'a, str>,
    tone: Tone,
    icon: Option<Text<'a>>,
    state_icon: Option<Text<'a>>,
}

pub fn status_chip<'a>(
    key: Option<&'a str>,
    state: impl Into<Cow<'a, str>>,
    tone: Tone,
) -> StatusChip<'a> {
    StatusChip {
        key: key.map(Cow::Borrowed),
        state: state.into(),
        tone,
        icon: None,
        state_icon: None,
    }
}

impl<'a> StatusChip<'a> {
    /// Leading category glyph, drawn muted (the chip's subject, e.g. account).
    pub fn icon(mut self, icon: Text<'a>) -> Self {
        self.icon = Some(icon);
        self
    }

    /// Glyph before the state, drawn in the tone colour.
    pub fn state_icon(mut self, icon: Text<'a>) -> Self {
        self.state_icon = Some(icon);
        self
    }
}

impl<'a, M: 'a> From<StatusChip<'a>> for Element<'a, M> {
    fn from(chip: StatusChip<'a>) -> Self {
        let StatusChip {
            key,
            state,
            tone,
            icon,
            state_icon,
        } = chip;
        let mut row = Row::new().spacing(8).align_y(Alignment::Center);
        if let Some(icon) = icon {
            row = row.push(icon.size(size::ICON_SMALL).style(text_style::muted));
        }
        if let Some(key) = key {
            row = row.push(chip_text(key).style(text_style::muted));
        }
        row = row.push(badge_row(state, tone, state_icon));
        Container::new(row)
            .height(Length::Fixed(size::CHIP_HEIGHT))
            .padding([0.0, 12.0])
            .align_y(alignment::Vertical::Center)
            .style(chip_style::chip)
            .into()
    }
}

/// A status word in its tone colour (`.stc`), such as "Pending" or "Failed",
/// with an optional glyph.
pub struct StatusBadge<'a> {
    label: Cow<'a, str>,
    tone: Tone,
    icon: Option<Text<'a>>,
}

pub fn status_badge<'a>(label: impl Into<Cow<'a, str>>, tone: Tone) -> StatusBadge<'a> {
    StatusBadge {
        label: label.into(),
        tone,
        icon: None,
    }
}

impl<'a> StatusBadge<'a> {
    pub fn icon(mut self, icon: Text<'a>) -> Self {
        self.icon = Some(icon);
        self
    }
}

impl<'a, M: 'a> From<StatusBadge<'a>> for Element<'a, M> {
    fn from(badge: StatusBadge<'a>) -> Self {
        badge_row(badge.label, badge.tone, badge.icon).into()
    }
}

fn chip_text<'a>(content: Cow<'a, str>) -> Text<'a> {
    iced::widget::text(content)
        .shaping(Shaping::Advanced)
        .size(type_size::CHIP)
        .font(font::REGULAR)
}

fn badge_row<'a, M: 'a>(label: Cow<'a, str>, tone: Tone, icon: Option<Text<'a>>) -> Row<'a, M> {
    let colour = move |theme: &Theme| text_style::tone(theme, tone);
    let mut row = Row::new().spacing(5).align_y(Alignment::Center);
    if let Some(icon) = icon {
        row = row.push(icon.size(size::ICON_SMALL).style(colour));
    }
    row.push(chip_text(label).font(font::MEDIUM).style(colour))
}
