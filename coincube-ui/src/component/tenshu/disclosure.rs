//! Disclosure (`.dsc` in Settings: Expert, node resources, diagnostics): a
//! full-width row between two rules with a chevron, a 15px semibold label and
//! an optional regular caption. The content follows only while expanded.
//!
//! The row is an [`Action`] with a stable ID, so it keeps Action's keyboard
//! contract. It holds no state: it draws `expanded` and emits the caller's
//! message for the opposite value. The label names the contents, and the
//! chevron (right when collapsed, down when expanded) carries the state with
//! the content's presence; there is no rotation animation.

use std::borrow::Cow;

use iced::advanced::text::Shaping;
use iced::advanced::widget::Id;
use iced::{Alignment, Length};

use crate::font;
use crate::icon;
use crate::theme::tenshu::control as style;
use crate::theme::tenshu::{radius, size, space, text as text_style, type_size, Tone};
use crate::widget::action::Action;
use crate::widget::{Column, Container, Element, Row};

/// A disclosure. Build it, then convert it into an [`Element`].
pub struct Disclosure<'a, M> {
    id: Id,
    label: Cow<'a, str>,
    caption: Option<Cow<'a, str>>,
    expanded: bool,
    focus_intent: fn(Id) -> M,
    on_toggle: Option<M>,
    content: Option<Element<'a, M>>,
}

/// A disclosure row named `label`, drawn `expanded` or collapsed. The
/// `focus_intent` must apply Iced's whole-tree focus operation for the given
/// `Id`, as [`Action::new`] requires.
pub fn disclosure<'a, M: Clone + 'static>(
    id: impl Into<Id>,
    label: impl Into<Cow<'a, str>>,
    expanded: bool,
    focus_intent: fn(Id) -> M,
) -> Disclosure<'a, M> {
    Disclosure {
        id: id.into(),
        label: label.into(),
        caption: None,
        expanded,
        focus_intent,
        on_toggle: None,
        content: None,
    }
}

impl<'a, M: Clone + 'static> Disclosure<'a, M> {
    /// A regular-weight note after the label (`.dsc .cap`), such as
    /// "Resource preset, logs, paths".
    pub fn caption(mut self, caption: impl Into<Cow<'a, str>>) -> Self {
        self.caption = Some(caption.into());
        self
    }

    /// `on_toggle` receives the requested state, the opposite of `expanded`.
    pub fn on_toggle(mut self, on_toggle: impl FnOnce(bool) -> M) -> Self {
        self.on_toggle = Some(on_toggle(!self.expanded));
        self
    }

    /// `None` makes the row inert: faded, never activates, skipped by Tab.
    pub fn on_toggle_maybe(mut self, on_toggle: Option<impl FnOnce(bool) -> M>) -> Self {
        self.on_toggle = on_toggle.map(|on_toggle| on_toggle(!self.expanded));
        self
    }

    /// What the row reveals. It is laid out only while expanded.
    pub fn content(mut self, content: impl Into<Element<'a, M>>) -> Self {
        self.content = Some(content.into());
        self
    }
}

fn rule<'a, M: 'a>() -> Element<'a, M> {
    Container::new(iced::widget::Space::new())
        .width(Length::Fill)
        .height(Length::Fixed(1.0))
        .style(style::divider)
        .into()
}

impl<'a, M: Clone + 'static> From<Disclosure<'a, M>> for Element<'a, M> {
    fn from(disclosure: Disclosure<'a, M>) -> Self {
        let Disclosure {
            id,
            label,
            caption,
            expanded,
            focus_intent,
            on_toggle,
            content,
        } = disclosure;
        let chevron = if expanded {
            icon::collapsed_icon()
        } else {
            icon::chevron_right()
        }
        .size(size::ICON_SMALL)
        .width(Length::Fixed(size::CHEVRON))
        .style(|theme| text_style::tone(theme, Tone::Muted));
        let mut heading = Row::new()
            .spacing(8)
            .align_y(Alignment::Center)
            .push(chevron)
            .push(
                iced::widget::text(label)
                    .shaping(Shaping::Advanced)
                    .font(font::SEMIBOLD)
                    .size(type_size::DISCLOSURE),
            );
        if let Some(caption) = caption {
            heading = heading.push(
                iced::widget::text(caption)
                    .shaping(Shaping::Advanced)
                    .font(font::REGULAR)
                    .size(type_size::CHIP)
                    .style(|theme| text_style::tone(theme, Tone::Muted)),
            );
        }
        let row = Action::new(
            id,
            Container::new(heading).width(Length::Fill),
            focus_intent,
        )
        .style(style::disclosure)
        .width(Length::Fill)
        .padding([space::M, 0.0])
        .focus_radius(radius::ROW)
        .on_press_maybe(on_toggle);
        let mut column = Column::new()
            .width(Length::Fill)
            .push(rule())
            .push(row)
            .push(rule());
        if let (true, Some(content)) = (expanded, content) {
            column = column
                .push(Container::new(content).padding(iced::Padding::default().top(space::M)));
        }
        column.into()
    }
}
