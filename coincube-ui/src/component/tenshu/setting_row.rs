//! Setting row (`.row` in Settings, App preferences and the Spark page): the
//! setting's label, an optional storage tag ("This device"), an optional
//! description and note on the left, and its control on the right, above a
//! 1px rule.
//!
//! Descriptions and notes are text, never tooltip-only. The row is not a focus
//! stop itself; the control is.

use std::borrow::Cow;

use iced::advanced::text::Shaping;
use iced::widget::text::LineHeight;
use iced::{alignment, Alignment, Length};

use crate::font;
use crate::theme::tenshu::control as style;
use crate::theme::tenshu::{space, text as text_style, type_size, Tone};
use crate::widget::{Column, Container, Element, Row};

/// Gap between the text, the control and a trailing tag (`.row` column-gap).
const COLUMN_GAP: f32 = 24.0;
/// Width of the trailing tag column in App preferences.
const TAG_COLUMN: f32 = 92.0;

/// A setting row. Build it, then convert it into an [`Element`].
pub struct SettingRow<'a, M> {
    label: Cow<'a, str>,
    tag: Option<Cow<'a, str>>,
    description: Option<Cow<'a, str>>,
    note: Option<(Cow<'a, str>, Tone)>,
    control: Element<'a, M>,
    divider: bool,
    trailing_tag: bool,
}

/// A row naming `control` with a visible `label`.
pub fn setting_row<'a, M: 'a>(
    label: impl Into<Cow<'a, str>>,
    control: impl Into<Element<'a, M>>,
) -> SettingRow<'a, M> {
    SettingRow {
        label: label.into(),
        tag: None,
        description: None,
        note: None,
        control: control.into(),
        divider: true,
        trailing_tag: false,
    }
}

impl<'a, M: 'a> SettingRow<'a, M> {
    /// Where the setting is stored, such as "This device" or "Cube".
    pub fn tag(mut self, tag: impl Into<Cow<'a, str>>) -> Self {
        self.tag = Some(tag.into());
        self
    }

    pub fn description(mut self, description: impl Into<Cow<'a, str>>) -> Self {
        self.description = Some(description.into());
        self
    }

    /// A status line under the description, such as "System is currently:
    /// Dark" or a disabled reason, in the given tone.
    pub fn note(mut self, note: impl Into<Cow<'a, str>>, tone: Tone) -> Self {
        self.note = Some((note.into(), tone));
        self
    }

    /// Draw the tag in its own column after the control, as App preferences
    /// does (`.scope`), instead of beside the label as Settings does.
    pub fn trailing_tag(mut self) -> Self {
        self.trailing_tag = true;
        self
    }

    /// Leave out the rule, for the last row of a group drawn on a card.
    pub fn without_divider(mut self) -> Self {
        self.divider = false;
        self
    }
}

fn line<'a>(
    content: Cow<'a, str>,
    size: f32,
    weight: iced::Font,
    tone: Tone,
) -> crate::widget::Text<'a> {
    iced::widget::text(content)
        .shaping(Shaping::Advanced)
        .font(weight)
        .size(size)
        .line_height(LineHeight::Relative(type_size::LINE_HEIGHT))
        .style(move |theme| text_style::tone(theme, tone))
}

impl<'a, M: 'a> From<SettingRow<'a, M>> for Element<'a, M> {
    fn from(row: SettingRow<'a, M>) -> Self {
        let SettingRow {
            label,
            tag,
            description,
            note,
            control,
            divider,
            trailing_tag,
        } = row;
        let mut heading = Row::new()
            .spacing(space::S)
            .align_y(Alignment::Center)
            .push(line(label, type_size::BODY, font::SEMIBOLD, Tone::Neutral));
        let tag = tag.map(|tag| {
            Container::new(line(tag, type_size::CAPTION, font::REGULAR, Tone::Muted))
                .padding([0.0, 6.0])
                .style(style::tag)
        });
        let (beside, after) = if trailing_tag {
            (None, tag)
        } else {
            (tag, None)
        };
        if let Some(tag) = beside {
            heading = heading.push(tag);
        }
        let mut text = Column::new().spacing(2).width(Length::Fill).push(heading);
        if let Some(description) = description {
            text = text.push(line(
                description,
                type_size::CHIP,
                font::REGULAR,
                Tone::Muted,
            ));
        }
        if let Some((note, tone)) = note {
            text = text.push(line(note, type_size::CAPTION, font::REGULAR, tone));
        }
        let mut body = Row::new()
            .spacing(COLUMN_GAP)
            .align_y(Alignment::Center)
            .push(text)
            .push(
                Container::new(control)
                    .align_x(alignment::Horizontal::Right)
                    .align_y(alignment::Vertical::Center),
            );
        if let Some(tag) = after {
            body = body.push(
                Container::new(tag)
                    .width(Length::Fixed(TAG_COLUMN))
                    .align_x(alignment::Horizontal::Right),
            );
        }
        let mut column = Column::new()
            .width(Length::Fill)
            .push(Container::new(body).padding([space::M, 0.0]));
        if divider {
            column = column.push(
                Container::new(iced::widget::Space::new())
                    .width(Length::Fill)
                    .height(Length::Fixed(1.0))
                    .style(style::divider),
            );
        }
        column.into()
    }
}
