//! Tenshu actions built on [`crate::widget::action::Action`].
//!
//! Every action keeps Action's keyboard contract: a stable semantic `Id`, one
//! activation per Enter/Space press, the 2px focus ring and the owner's
//! whole-tree focus intent on pointer press. An action without an intent is
//! inert and leaves the Tab order.

use std::borrow::Cow;

use iced::advanced::text::Shaping;
use iced::advanced::widget::Id;
use iced::{alignment, Alignment, Length};

use crate::component::tenshu::balance::AmountVisibility;
use crate::font;
use crate::icon;
use crate::theme::tenshu::button::{self as style, ActionKind, ActionShape};
use crate::theme::tenshu::{size, type_size};
use crate::widget::action::Action;
use crate::widget::{Container, Element, Row, Text};

/// A labelled action. Build it, then convert it into an [`Element`].
pub struct ActionButton<'a, M> {
    id: Id,
    label: Cow<'a, str>,
    kind: ActionKind,
    shape: ActionShape,
    icon: Option<Text<'a>>,
    width: Length,
    focus_intent: fn(Id) -> M,
    on_press: Option<M>,
}

impl<'a, M: Clone + 'static> ActionButton<'a, M> {
    /// A 36px pill action. `focus_intent` must apply Iced's whole-tree focus
    /// operation for the given `Id`, as [`Action::new`] requires.
    pub fn new(
        id: impl Into<Id>,
        label: impl Into<Cow<'a, str>>,
        kind: ActionKind,
        focus_intent: fn(Id) -> M,
    ) -> Self {
        Self {
            id: id.into(),
            label: label.into(),
            kind,
            shape: ActionShape::Pill,
            icon: None,
            width: Length::Shrink,
            focus_intent,
            on_press: None,
        }
    }

    /// A leading glyph, drawn in the label colour.
    pub fn icon(mut self, icon: Text<'a>) -> Self {
        self.icon = Some(icon);
        self
    }

    /// The 32px, 8px-radius button used in rows and forms.
    pub fn compact(mut self) -> Self {
        if self.shape == ActionShape::Pill {
            self.shape = ActionShape::Compact;
        }
        self
    }

    pub fn width(mut self, width: impl Into<Length>) -> Self {
        self.width = width.into();
        self
    }

    pub fn on_press(mut self, message: M) -> Self {
        self.on_press = Some(message);
        self
    }

    /// `None` makes the action inert: it draws faded, never activates and is
    /// skipped by Tab.
    pub fn on_press_maybe(mut self, message: Option<M>) -> Self {
        self.on_press = message;
        self
    }

    fn chip(mut self) -> Self {
        self.shape = ActionShape::Chip;
        self
    }
}

impl<'a, M: Clone + 'static> From<ActionButton<'a, M>> for Element<'a, M> {
    fn from(action: ActionButton<'a, M>) -> Self {
        let ActionButton {
            id,
            label,
            kind,
            shape,
            icon,
            width,
            focus_intent,
            on_press,
        } = action;
        let (text_size, weight, height, padding) = match (shape, kind) {
            (ActionShape::Pill | ActionShape::Compact, ActionKind::Quiet) => {
                (type_size::BODY, font::MEDIUM, Length::Shrink, [4.0, 2.0])
            }
            (ActionShape::Pill, _) => (
                type_size::BODY,
                font::SEMIBOLD,
                Length::Fixed(size::PILL_HEIGHT),
                [0.0, 20.0],
            ),
            (ActionShape::Compact, _) => (
                type_size::CHIP,
                font::MEDIUM,
                Length::Fixed(size::BUTTON_HEIGHT),
                [0.0, 14.0],
            ),
            (ActionShape::Chip, _) | (ActionShape::Icon, _) => (
                type_size::CHIP,
                font::REGULAR,
                Length::Fixed(size::CHIP_HEIGHT),
                [0.0, 12.0],
            ),
        };
        let label: Text<'a> = iced::widget::text(label)
            .shaping(Shaping::Advanced)
            .font(weight)
            .size(text_size);
        let content: Element<'a, M> = match icon {
            Some(icon) => Row::new()
                .spacing(8)
                .align_y(Alignment::Center)
                .push(icon.size(size::ICON_SMALL))
                .push(label)
                .into(),
            None => label.into(),
        };
        let content = Container::new(content)
            .width(if width == Length::Shrink {
                Length::Shrink
            } else {
                Length::Fill
            })
            .height(if height == Length::Shrink {
                Length::Shrink
            } else {
                Length::Fill
            })
            .align_x(alignment::Horizontal::Center)
            .align_y(alignment::Vertical::Center);
        Action::new(id, content, focus_intent)
            .style(move |theme, status| style::action(theme, status, kind, shape))
            .width(width)
            .height(height)
            .padding(padding)
            .focus_radius(shape.radius(kind))
            .on_press_maybe(on_press)
            .into()
    }
}

/// The global balance eye (`.eye`): a 32px icon-only action. The open eye shows
/// only while amounts are shown; hidden and unresolved both show the slashed
/// eye. It emits only the caller's existing preference intent and holds no
/// privacy state of its own.
pub fn eye_action<'a, M: Clone + 'static>(
    id: impl Into<Id>,
    visibility: AmountVisibility,
    focus_intent: fn(Id) -> M,
    on_toggle: Option<M>,
) -> Element<'a, M> {
    let glyph = if visibility.shows_amounts() {
        icon::eye_icon()
    } else {
        icon::eye_slash_icon()
    };
    let content = Container::new(glyph.size(size::ICON))
        .width(Length::Fill)
        .height(Length::Fill)
        .align_x(alignment::Horizontal::Center)
        .align_y(alignment::Vertical::Center);
    Action::new(id, content, focus_intent)
        .style(|theme, status| {
            style::action(theme, status, ActionKind::Secondary, ActionShape::Icon)
        })
        .width(Length::Fixed(size::ICON_ACTION))
        .height(Length::Fixed(size::ICON_ACTION))
        .padding(0.0)
        .focus_radius(ActionShape::Icon.radius(ActionKind::Secondary))
        .on_press_maybe(on_toggle)
        .into()
}

/// The Overview's Transfer action: a secondary pill with the transfer glyph.
pub fn transfer_action<'a, M: Clone + 'static>(
    id: impl Into<Id>,
    focus_intent: fn(Id) -> M,
    on_press: Option<M>,
) -> ActionButton<'a, M> {
    ActionButton::new(id, "Transfer", ActionKind::Secondary, focus_intent)
        .icon(icon::arrow_down_up_icon())
        .on_press_maybe(on_press)
}

/// An interactive chip (`.chip`), such as the top bar's account entry.
pub fn chip_action<'a, M: Clone + 'static>(
    id: impl Into<Id>,
    label: impl Into<Cow<'a, str>>,
    focus_intent: fn(Id) -> M,
    on_press: Option<M>,
) -> ActionButton<'a, M> {
    ActionButton::new(id, label, ActionKind::Secondary, focus_intent)
        .chip()
        .on_press_maybe(on_press)
}
