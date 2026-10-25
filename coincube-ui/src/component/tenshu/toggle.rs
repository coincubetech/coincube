//! On/off toggles built on [`crate::widget::action::Action`].
//!
//! Two artboard forms: the pill (`.tog`), a 32px chip with a small track and
//! the state word ("On" / "Off") used in Settings and the Overview's Stable
//! Balance row, and the 40×24 switch (`.sw`) used in App preferences, with its
//! state word (`.swst`) muted to its left. In both forms the word is drawn
//! text, so the state never rests on colour or thumb position alone.
//! Both keep Action's contract: a stable semantic ID, one activation per
//! Enter/Space press, the 2px focus ring and the owner's focus intent on
//! pointer press. A toggle without an intent is inert and leaves the Tab order.
//!
//! The toggle holds no state. It draws `is_on` and, when pressed, emits the
//! caller's message for the opposite value. Name it with a visible label next
//! to it, normally through [`super::setting_row`].

use std::borrow::Cow;

use iced::advanced::text::Shaping;
use iced::advanced::widget::Id;
use iced::{alignment, Alignment, Length, Padding};

use crate::font;
use crate::theme::tenshu::control as style;
use crate::theme::tenshu::{self as tokens, radius, size, text as text_style, type_size, Tone};
use crate::widget::action::Action;
use crate::widget::{Column, Container, Element, Row};

/// Which artboard form a toggle takes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ToggleForm {
    /// `.tog`: track plus the state word, inside a 32px chip outline.
    Pill,
    /// `.sw`: the 40×24 switch, its state word muted to the left.
    Switch,
}

/// An on/off toggle. Build it, then convert it into an [`Element`].
pub struct Toggle<'a, M> {
    id: Id,
    is_on: bool,
    form: ToggleForm,
    on_word: Cow<'a, str>,
    off_word: Cow<'a, str>,
    focus_intent: fn(Id) -> M,
    on_toggle: Option<M>,
}

/// A pill toggle showing `is_on`. `focus_intent` must apply Iced's whole-tree
/// focus operation for the given `Id`, as [`Action::new`] requires.
pub fn toggle<'a, M: Clone + 'static>(
    id: impl Into<Id>,
    is_on: bool,
    focus_intent: fn(Id) -> M,
) -> Toggle<'a, M> {
    Toggle {
        id: id.into(),
        is_on,
        form: ToggleForm::Pill,
        on_word: Cow::Borrowed("On"),
        off_word: Cow::Borrowed("Off"),
        focus_intent,
        on_toggle: None,
    }
}

impl<'a, M: Clone + 'static> Toggle<'a, M> {
    /// The switch form (`.sw`) of App preferences.
    pub fn switch(mut self) -> Self {
        self.form = ToggleForm::Switch;
        self
    }

    /// Replace the state words, for example "Shown" / "Hidden".
    pub fn state_words(
        mut self,
        on: impl Into<Cow<'a, str>>,
        off: impl Into<Cow<'a, str>>,
    ) -> Self {
        self.on_word = on.into();
        self.off_word = off.into();
        self
    }

    /// `on_toggle` receives the requested value, the opposite of `is_on`.
    pub fn on_toggle(mut self, on_toggle: impl FnOnce(bool) -> M) -> Self {
        self.on_toggle = Some(on_toggle(!self.is_on));
        self
    }

    /// `None` makes the toggle inert: it draws faded, never activates and is
    /// skipped by Tab.
    pub fn on_toggle_maybe(mut self, on_toggle: Option<impl FnOnce(bool) -> M>) -> Self {
        self.on_toggle = on_toggle.map(|on_toggle| on_toggle(!self.is_on));
        self
    }
}

fn thumb<'a, M: 'a>(diameter: f32, is_on: bool, enabled: bool, pill: bool) -> Element<'a, M> {
    Container::new(iced::widget::Space::new())
        .width(Length::Fixed(diameter))
        .height(Length::Fixed(diameter))
        .style(move |theme| style::thumb(theme, is_on, enabled, pill))
        .into()
}

impl<'a, M: Clone + 'static> From<Toggle<'a, M>> for Element<'a, M> {
    fn from(toggle: Toggle<'a, M>) -> Self {
        let Toggle {
            id,
            is_on,
            form,
            on_word,
            off_word,
            focus_intent,
            on_toggle,
        } = toggle;
        let enabled = on_toggle.is_some();
        let side = if is_on {
            alignment::Horizontal::Right
        } else {
            alignment::Horizontal::Left
        };
        match form {
            ToggleForm::Pill => {
                let inset = (size::TOGGLE_TRACK_HEIGHT - size::TOGGLE_THUMB) / 2.0;
                let track = Container::new(thumb(size::TOGGLE_THUMB, is_on, enabled, true))
                    .width(Length::Fixed(size::TOGGLE_TRACK_WIDTH))
                    .height(Length::Fixed(size::TOGGLE_TRACK_HEIGHT))
                    .padding(inset)
                    .align_x(side)
                    .align_y(alignment::Vertical::Center)
                    .style(move |theme| style::toggle_pill_track(theme, is_on, enabled));
                let word = iced::widget::text(if is_on { on_word } else { off_word })
                    .shaping(Shaping::Advanced)
                    .font(font::MEDIUM)
                    .size(type_size::CHIP);
                let content = Row::new()
                    .spacing(8)
                    .align_y(Alignment::Center)
                    .height(Length::Fill)
                    .push(track)
                    .push(word);
                Action::new(id, content, focus_intent)
                    .style(style::toggle_pill)
                    .height(Length::Fixed(size::TOGGLE_HEIGHT))
                    // `.tog` pads `0 12px 0 4px` inside its 1px border; Iced's
                    // padding is measured from the outer edge.
                    .padding(Padding {
                        top: 0.0,
                        right: 13.0,
                        bottom: 0.0,
                        left: 5.0,
                    })
                    .focus_radius(radius::CHIP)
                    .on_press_maybe(on_toggle)
                    .into()
            }
            ToggleForm::Switch => {
                let inset = (size::SWITCH_HEIGHT - size::SWITCH_THUMB) / 2.0;
                let content = Container::new(thumb(size::SWITCH_THUMB, is_on, enabled, false))
                    .width(Length::Fill)
                    .height(Length::Fill)
                    .align_x(side)
                    .align_y(alignment::Vertical::Center);
                let switch = Action::new(id, content, focus_intent)
                    .style(move |theme, status| style::switch(theme, status, is_on))
                    .width(Length::Fixed(size::SWITCH_WIDTH))
                    .height(Length::Fixed(size::SWITCH_HEIGHT))
                    .padding(inset)
                    .focus_radius(size::SWITCH_HEIGHT / 2.0)
                    .on_press_maybe(on_toggle);
                // `.swst`: muted, at least 24px wide so On/Off do not shift the
                // switch. It is not part of the hit target.
                let word = iced::widget::text(if is_on { on_word } else { off_word })
                    .shaping(Shaping::Advanced)
                    .size(type_size::CHIP)
                    .style(move |theme| {
                        let muted = text_style::tone(theme, Tone::Muted);
                        if enabled {
                            muted
                        } else {
                            iced::widget::text::Style {
                                color: muted
                                    .color
                                    .map(|color| tokens::faded(color, tokens::DISABLED_ALPHA)),
                            }
                        }
                    });
                Row::new()
                    .spacing(8)
                    .align_y(Alignment::Center)
                    .push(
                        Column::new()
                            .push(iced::widget::Space::new().width(Length::Fixed(24.0)))
                            .push(word),
                    )
                    .push(switch)
                    .into()
            }
        }
    }
}
