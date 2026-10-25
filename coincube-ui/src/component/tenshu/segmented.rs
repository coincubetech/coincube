//! Segmented choice (`.seg`): one value out of a few, such as Bitcoin /
//! Stablecoin in Send and Receive, BTC / Sats in Settings, or the theme in App
//! preferences.
//!
//! Each segment is an [`Action`] with its own stable ID, so it keeps Action's
//! keyboard contract: a Tab stop, one activation per Enter/Space press, the
//! focus ring and the owner's focus intent on pointer press. While a segment
//! has focus, the arrow keys move the selection to the previous or next
//! enabled segment (wrapping), requesting focus for it as well. A disabled
//! segment, or every segment of a group without an intent, is inert and
//! leaves the Tab order.
//!
//! The group holds no state. Pressing a segment, including the selected one,
//! emits `on_select` with its value; owners treat selection as idempotent.

use std::borrow::Cow;

use iced::advanced::text::Shaping;
use iced::advanced::widget::Id;
use iced::keyboard::{self, key};
use iced::{alignment, Alignment, Length};

use super::scope::KeyScope;
use crate::font;
use crate::theme::tenshu::control as style;
use crate::theme::tenshu::{radius, size, type_size};
use crate::widget::action::Action;
use crate::widget::{Container, Element, Row, Text};

/// Segment height and frame radius.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum SegmentSize {
    /// 32px segments in a 10px frame: the Send and Receive master selectors.
    #[default]
    Regular,
    /// 28px segments in an 8px frame: choices inside a setting row.
    Compact,
    /// 26px segments in a 10px frame: inline unit switches such as sats / BTC.
    Small,
}

impl SegmentSize {
    fn height(self) -> f32 {
        match self {
            SegmentSize::Regular => size::SEGMENT_HEIGHT,
            SegmentSize::Compact => size::SEGMENT_COMPACT_HEIGHT,
            SegmentSize::Small => size::SEGMENT_SMALL_HEIGHT,
        }
    }

    fn frame_radius(self) -> f32 {
        match self {
            SegmentSize::Compact => radius::CONTROL,
            SegmentSize::Regular | SegmentSize::Small => radius::MENU,
        }
    }

    fn segment_radius(self) -> f32 {
        match self {
            SegmentSize::Compact => radius::ROW,
            SegmentSize::Regular | SegmentSize::Small => radius::CONTROL,
        }
    }

    fn text_size(self) -> f32 {
        match self {
            SegmentSize::Small => type_size::CAPTION,
            SegmentSize::Regular | SegmentSize::Compact => type_size::CHIP,
        }
    }

    fn padding_x(self) -> f32 {
        match self {
            SegmentSize::Small => 10.0,
            SegmentSize::Regular | SegmentSize::Compact => 12.0,
        }
    }
}

/// One option of a segmented choice.
pub struct Segment<'a, T> {
    id: Id,
    value: T,
    label: Cow<'a, str>,
    icon: Option<Text<'a>>,
    enabled: bool,
}

/// A segment with a stable ID, the value it selects and its visible label.
pub fn segment<'a, T>(
    id: impl Into<Id>,
    value: T,
    label: impl Into<Cow<'a, str>>,
) -> Segment<'a, T> {
    Segment {
        id: id.into(),
        value,
        label: label.into(),
        icon: None,
        enabled: true,
    }
}

impl<'a, T> Segment<'a, T> {
    /// A leading 16px glyph, drawn in the label colour.
    pub fn icon(mut self, icon: Text<'a>) -> Self {
        self.icon = Some(icon);
        self
    }

    /// An unavailable option: drawn faded, never selected by pointer, keys or
    /// arrows. Explain why in text next to the group.
    pub fn disabled(mut self) -> Self {
        self.enabled = false;
        self
    }

    pub fn enabled(mut self, enabled: bool) -> Self {
        self.enabled = enabled;
        self
    }
}

/// A segmented choice. Build it, then convert it into an [`Element`].
pub struct Segmented<'a, T, M> {
    segments: Vec<Segment<'a, T>>,
    selected: Option<T>,
    size: SegmentSize,
    focus_intent: fn(Id) -> M,
    on_select: Option<Box<dyn Fn(T) -> M + 'a>>,
}

/// A segmented choice over `segments`, showing `selected` (or nothing). The
/// `focus_intent` must apply Iced's whole-tree focus operation for the given
/// `Id`, as [`Action::new`] requires.
pub fn segmented<'a, T, M>(
    segments: impl IntoIterator<Item = Segment<'a, T>>,
    selected: Option<T>,
    focus_intent: fn(Id) -> M,
) -> Segmented<'a, T, M>
where
    T: Copy + PartialEq + 'a,
    M: Clone + 'static,
{
    Segmented {
        segments: segments.into_iter().collect(),
        selected,
        size: SegmentSize::default(),
        focus_intent,
        on_select: None,
    }
}

impl<'a, T, M> Segmented<'a, T, M>
where
    T: Copy + PartialEq + 'a,
    M: Clone + 'static,
{
    pub fn size(mut self, size: SegmentSize) -> Self {
        self.size = size;
        self
    }

    pub fn on_select(mut self, on_select: impl Fn(T) -> M + 'a) -> Self {
        self.on_select = Some(Box::new(on_select));
        self
    }

    /// `None` makes every segment inert.
    pub fn on_select_maybe(mut self, on_select: Option<impl Fn(T) -> M + 'a>) -> Self {
        self.on_select = on_select.map(|f| Box::new(f) as Box<dyn Fn(T) -> M + 'a>);
        self
    }
}

impl<'a, T, M> From<Segmented<'a, T, M>> for Element<'a, M>
where
    T: Copy + PartialEq + 'a,
    M: Clone + 'static,
{
    fn from(group: Segmented<'a, T, M>) -> Self {
        let Segmented {
            segments,
            selected,
            size,
            focus_intent,
            on_select,
        } = group;

        // (id, message) per enabled segment, in visual order, for arrow keys.
        let targets: Vec<(Id, M)> = match on_select.as_ref() {
            Some(on_select) => segments
                .iter()
                .filter(|segment| segment.enabled)
                .map(|segment| (segment.id.clone(), on_select(segment.value)))
                .collect(),
            None => Vec::new(),
        };

        let row = segments.into_iter().fold(
            Row::new().spacing(2).align_y(Alignment::Center),
            |row, segment| {
                let Segment {
                    id,
                    value,
                    label,
                    icon,
                    enabled,
                } = segment;
                let is_selected = selected == Some(value);
                let label = iced::widget::text(label)
                    .shaping(Shaping::Advanced)
                    .font(if is_selected {
                        font::SEMIBOLD
                    } else {
                        font::MEDIUM
                    })
                    .size(size.text_size());
                let content: Element<'a, M> = match icon {
                    Some(icon) => Row::new()
                        .spacing(6)
                        .align_y(Alignment::Center)
                        .push(icon.size(size::ICON_SMALL))
                        .push(label)
                        .into(),
                    None => label.into(),
                };
                let content = Container::new(content)
                    .height(Length::Fill)
                    .align_x(alignment::Horizontal::Center)
                    .align_y(alignment::Vertical::Center);
                let on_press = on_select
                    .as_ref()
                    .filter(|_| enabled)
                    .map(|on_select| on_select(value));
                let segment_radius = size.segment_radius();
                row.push(
                    Action::new(id, content, focus_intent)
                        .style(move |theme, status| {
                            style::segment(theme, status, is_selected, segment_radius)
                        })
                        .height(Length::Fixed(size.height()))
                        .padding([0.0, size.padding_x()])
                        .focus_radius(segment_radius)
                        .on_press_maybe(on_press),
                )
            },
        );

        let frame_radius = size.frame_radius();
        let frame = Container::new(row)
            .padding(2)
            .style(move |theme| style::segmented_frame(theme, frame_radius));

        KeyScope::new(
            frame,
            Box::new(move |key, modifiers, focused| {
                if modifiers.command() || modifiers.alt() || modifiers.shift() {
                    return None;
                }
                let step: isize = match key {
                    keyboard::Key::Named(key::Named::ArrowLeft | key::Named::ArrowUp) => -1,
                    keyboard::Key::Named(key::Named::ArrowRight | key::Named::ArrowDown) => 1,
                    _ => return None,
                };
                let current = targets.iter().position(|(id, _)| Some(id) == focused)?;
                let count = targets.len() as isize;
                let next = (current as isize + step).rem_euclid(count) as usize;
                let (id, message) = targets[next].clone();
                Some(vec![focus_intent(id), message])
            }),
        )
        .into()
    }
}
