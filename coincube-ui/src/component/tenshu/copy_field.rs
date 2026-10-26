//! Copyable value (`.cpy` in Receive): a labelled, read-only value in a field
//! box beside a Copy button, for invoices, addresses and descriptors.
//!
//! The value is text, not an input, so it never takes focus or edits; the
//! Copy button is an [`Action`] with its own stable ID and the usual keyboard
//! contract. Long values are not wrapped or shortened here: pass the display
//! form (for example `lnbc50u1p…synthetic…7xk`) and copy the full value in
//! the owner's `on_copy` handler.

use std::borrow::Cow;

use iced::advanced::text::Shaping;
use iced::advanced::widget::Id;
use iced::widget::text::{LineHeight, Wrapping};
use iced::{alignment, Alignment, Length};

use super::field::FieldSize;
use crate::font;
use crate::theme::tenshu::button::{self as button_style, ActionKind, ActionShape};
use crate::theme::tenshu::{control as style, radius, size, text as text_style, type_size, Tone};
use crate::widget::action::Action;
use crate::widget::{Column, Container, Element, Row};

/// A copyable value. Build it, then convert it into an [`Element`].
pub struct CopyField<'a, M> {
    id: Id,
    label: Cow<'a, str>,
    value: Cow<'a, str>,
    size: FieldSize,
    copied: bool,
    focus_intent: fn(Id) -> M,
    on_copy: Option<M>,
    hint: Option<Cow<'a, str>>,
}

/// A value under a visible `label` with a Copy button whose ID is `id`. The
/// `focus_intent` must apply Iced's whole-tree focus operation for the given
/// `Id`, as [`Action::new`] requires.
pub fn copy_field<'a, M: Clone + 'static>(
    id: impl Into<Id>,
    label: impl Into<Cow<'a, str>>,
    value: impl Into<Cow<'a, str>>,
    focus_intent: fn(Id) -> M,
) -> CopyField<'a, M> {
    CopyField {
        id: id.into(),
        label: label.into(),
        value: value.into(),
        size: FieldSize::Regular,
        copied: false,
        focus_intent,
        on_copy: None,
        hint: None,
    }
}

impl<'a, M: Clone + 'static> CopyField<'a, M> {
    pub fn size(mut self, size: FieldSize) -> Self {
        self.size = size;
        self
    }

    /// Show "Copied" on the button, as the owner confirms a copy.
    pub fn copied(mut self, copied: bool) -> Self {
        self.copied = copied;
        self
    }

    pub fn on_copy(mut self, message: M) -> Self {
        self.on_copy = Some(message);
        self
    }

    /// `None` makes the Copy button inert.
    pub fn on_copy_maybe(mut self, message: Option<M>) -> Self {
        self.on_copy = message;
        self
    }

    /// A muted line under the value, such as "Share this invoice to receive
    /// sats via Lightning".
    pub fn hint(mut self, hint: impl Into<Cow<'a, str>>) -> Self {
        self.hint = Some(hint.into());
        self
    }
}

impl<'a, M: Clone + 'static> From<CopyField<'a, M>> for Element<'a, M> {
    fn from(field: CopyField<'a, M>) -> Self {
        let CopyField {
            id,
            label,
            value,
            size,
            copied,
            focus_intent,
            on_copy,
            hint,
        } = field;
        let (height, field_radius, padding_x) = match size {
            FieldSize::Regular => (size::FIELD_HEIGHT, radius::MENU, 14.0),
            FieldSize::Compact => (size::FIELD_COMPACT_HEIGHT, radius::CONTROL, 10.0),
        };
        let value = Container::new(
            iced::widget::text(value)
                .shaping(Shaping::Advanced)
                .font(font::MONO_REGULAR)
                .size(type_size::MONO)
                .wrapping(Wrapping::None),
        )
        .width(Length::Fill)
        .height(Length::Fixed(height))
        .padding([0.0, padding_x])
        .align_y(alignment::Vertical::Center)
        .clip(true)
        .style(move |theme| style::value_box(theme, field_radius));
        let button = Action::new(
            id,
            Container::new(
                iced::widget::text(if copied { "Copied" } else { "Copy" })
                    .shaping(Shaping::Advanced)
                    .font(font::MEDIUM)
                    .size(type_size::CHIP),
            )
            .height(Length::Fill)
            .align_y(alignment::Vertical::Center),
            focus_intent,
        )
        .style(|theme, status| {
            button_style::action(theme, status, ActionKind::Secondary, ActionShape::Compact)
        })
        .height(Length::Fixed(height))
        .padding([0.0, 14.0])
        .focus_radius(radius::CONTROL)
        .on_press_maybe(on_copy);
        let mut column = Column::new()
            .spacing(6)
            .width(Length::Fill)
            .push(
                iced::widget::text(label)
                    .shaping(Shaping::Advanced)
                    .font(font::SEMIBOLD)
                    .size(type_size::BODY)
                    .line_height(LineHeight::Relative(type_size::LINE_HEIGHT)),
            )
            .push(
                Row::new()
                    .spacing(8)
                    .align_y(Alignment::Center)
                    .push(value)
                    .push(button),
            );
        if let Some(hint) = hint {
            column = column.push(
                iced::widget::text(hint)
                    .shaping(Shaping::Advanced)
                    .size(type_size::CHIP)
                    .style(|theme| text_style::tone(theme, Tone::Muted)),
            );
        }
        column.into()
    }
}
