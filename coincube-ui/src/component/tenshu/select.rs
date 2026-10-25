//! Select (`select.ctl` in the Activity filters, `select.in` in Settings):
//! a labelled field showing the current option, with a chevron, that opens a
//! menu of options.
//!
//! Iced 0.14's `pick_list` cannot take keyboard focus, so this is its own
//! widget. It registers its stable ID as a focus target while enabled, and:
//!
//! - pointer: pressing the field emits the owner's focus intent and opens or
//!   closes the menu; pressing an option selects it and closes the menu;
//!   pressing elsewhere closes it;
//! - keys: Enter, Space, ↓ or ↑ open the menu on the current option; while
//!   open, ↑/↓ (and Home/End) move the highlight, Enter or Space select it,
//!   Escape closes the menu and Tab closes it and moves on.
//!
//! The owner holds the selection; the menu's open and highlighted row are UI
//! state kept in the widget tree, like a text cursor. A select without
//! `on_select` is disabled: muted, never opens, and is skipped by Tab.

use std::borrow::Cow;

use iced::advanced::layout::{self, Layout};
use iced::advanced::overlay;
use iced::advanced::renderer;
use iced::advanced::text::{self, Renderer as _, Shaping};
use iced::advanced::widget::operation::Focusable;
use iced::advanced::widget::tree::{self, Tree};
use iced::advanced::widget::{Id, Operation, Widget};
use iced::advanced::{Clipboard, Renderer as _, Shell};
use iced::keyboard::{self, key};
use iced::widget::text::LineHeight;
use iced::{
    alignment, mouse, Background, Border, Event, Font, Length, Pixels, Point, Rectangle, Shadow,
    Size, Vector,
};

use super::field::FieldSize;
use crate::font;
use crate::theme::tenshu::{
    faded, radius, size, text as text_style, type_size, Tone, DISABLED_ALPHA,
};
use crate::theme::Theme;
use crate::widget::{Column, Element, Renderer};

const BOOTSTRAP_ICONS: Font = Font::with_name("bootstrap-icons");
const CHEVRON_DOWN: char = '\u{F282}';
const CHECK: char = '\u{F26E}';
/// Menu rows (`.menu` items) and the menu's inner padding.
const ROW_HEIGHT: f32 = 32.0;
const MENU_PADDING: f32 = 4.0;
const MENU_GAP: f32 = 4.0;

/// A select. Build it, then convert it into an [`Element`].
pub struct Select<'a, T, M> {
    id: Id,
    label: Cow<'a, str>,
    options: Vec<(T, Cow<'a, str>)>,
    selected: Option<T>,
    placeholder: Cow<'a, str>,
    size: FieldSize,
    width: Length,
    label_hidden: bool,
    focus_intent: fn(Id) -> M,
    on_select: Option<Box<dyn Fn(T) -> M + 'a>>,
}

/// A select over `options` (value, visible text) showing `selected`, under a
/// visible `label`. The `focus_intent` must apply Iced's whole-tree focus
/// operation for the given `Id`; the field and its label emit it.
pub fn select<'a, T, M>(
    id: impl Into<Id>,
    label: impl Into<Cow<'a, str>>,
    options: impl IntoIterator<Item = (T, impl Into<Cow<'a, str>>)>,
    selected: Option<T>,
    focus_intent: fn(Id) -> M,
) -> Select<'a, T, M>
where
    T: Copy + PartialEq + 'a,
    M: Clone + 'static,
{
    Select {
        id: id.into(),
        label: label.into(),
        options: options
            .into_iter()
            .map(|(value, text)| (value, text.into()))
            .collect(),
        selected,
        placeholder: Cow::Borrowed(""),
        size: FieldSize::Compact,
        width: Length::Fill,
        label_hidden: false,
        focus_intent,
        on_select: None,
    }
}

impl<'a, T, M> Select<'a, T, M>
where
    T: Copy + PartialEq + 'a,
    M: Clone + 'static,
{
    /// Text shown while nothing is selected.
    pub fn placeholder(mut self, placeholder: impl Into<Cow<'a, str>>) -> Self {
        self.placeholder = placeholder.into();
        self
    }

    /// Compact (36px, the default, as in the Activity filters) or regular
    /// (44px).
    pub fn size(mut self, size: FieldSize) -> Self {
        self.size = size;
        self
    }

    pub fn width(mut self, width: impl Into<Length>) -> Self {
        self.width = width.into();
        self
    }

    /// Keep the label for the API but draw none, for a select inside a
    /// [`super::setting_row`] whose row label names it.
    pub fn hide_label(mut self) -> Self {
        self.label_hidden = true;
        self
    }

    pub fn on_select(mut self, on_select: impl Fn(T) -> M + 'a) -> Self {
        self.on_select = Some(Box::new(on_select));
        self
    }

    /// `None` disables the select.
    pub fn on_select_maybe(mut self, on_select: Option<impl Fn(T) -> M + 'a>) -> Self {
        self.on_select = on_select.map(|f| Box::new(f) as Box<dyn Fn(T) -> M + 'a>);
        self
    }
}

impl<'a, T, M> From<Select<'a, T, M>> for Element<'a, M>
where
    T: Copy + PartialEq + 'a,
    M: Clone + 'static,
{
    fn from(select: Select<'a, T, M>) -> Self {
        let Select {
            id,
            label,
            options,
            selected,
            placeholder,
            size,
            width,
            label_hidden,
            focus_intent,
            on_select,
        } = select;
        let enabled = on_select.is_some() && !options.is_empty();
        let selected_index = options
            .iter()
            .position(|(value, _)| Some(*value) == selected);
        let messages = on_select
            .filter(|_| enabled)
            .map(|on_select| options.iter().map(|(value, _)| on_select(*value)).collect());
        let field = SelectField {
            id: id.clone(),
            labels: options.into_iter().map(|(_, text)| text).collect(),
            selected: selected_index,
            placeholder,
            size,
            width,
            on_focus: focus_intent(id.clone()),
            messages,
        };
        if label_hidden {
            return Element::new(field);
        }
        let (text_size, weight, tone) = match size {
            FieldSize::Regular => (type_size::BODY, font::SEMIBOLD, Tone::Neutral),
            FieldSize::Compact => (type_size::CAPTION, font::MEDIUM, Tone::Muted),
        };
        let tone = if enabled { tone } else { Tone::Muted };
        let label = iced::widget::text(label)
            .shaping(Shaping::Advanced)
            .font(weight)
            .size(text_size)
            .line_height(LineHeight::Relative(type_size::LINE_HEIGHT))
            .style(move |theme| text_style::tone(theme, tone));
        let label: Element<'a, M> = if enabled {
            iced::widget::MouseArea::new(label)
                .on_press(focus_intent(id))
                .into()
        } else {
            label.into()
        };
        Column::new()
            .spacing(6)
            .width(width)
            .push(label)
            .push(Element::new(field))
            .into()
    }
}

struct SelectField<'a, M> {
    id: Id,
    labels: Vec<Cow<'a, str>>,
    selected: Option<usize>,
    placeholder: Cow<'a, str>,
    size: FieldSize,
    width: Length,
    on_focus: M,
    /// One message per option; `None` when disabled.
    messages: Option<Vec<M>>,
}

#[derive(Debug)]
struct State {
    id: Id,
    focused: bool,
    open: bool,
    highlighted: usize,
}

impl Focusable for State {
    fn is_focused(&self) -> bool {
        self.focused
    }

    fn focus(&mut self) {
        self.focused = true;
    }

    fn unfocus(&mut self) {
        self.focused = false;
        self.open = false;
    }
}

impl<M> SelectField<'_, M> {
    fn height(&self) -> f32 {
        match self.size {
            FieldSize::Regular => size::FIELD_HEIGHT,
            FieldSize::Compact => size::FIELD_COMPACT_HEIGHT,
        }
    }

    fn radius(&self) -> f32 {
        match self.size {
            FieldSize::Regular => radius::MENU,
            FieldSize::Compact => radius::CONTROL,
        }
    }

    fn text_size(&self) -> f32 {
        match self.size {
            FieldSize::Regular => type_size::FIELD,
            FieldSize::Compact => type_size::FIELD_COMPACT,
        }
    }

    fn padding_x(&self) -> f32 {
        match self.size {
            FieldSize::Regular => 14.0,
            FieldSize::Compact => 10.0,
        }
    }

    fn open_on_current(&self, state: &mut State) {
        state.open = true;
        state.highlighted = self.selected.unwrap_or(0);
    }
}

fn label_text(content: &str, size: f32, bounds: Size, font: Font) -> text::Text<String, Font> {
    text::Text {
        content: content.to_string(),
        bounds,
        size: Pixels(size),
        line_height: LineHeight::Absolute(Pixels(20.0)),
        font,
        align_x: text::Alignment::Default,
        align_y: alignment::Vertical::Center,
        shaping: Shaping::Advanced,
        wrapping: text::Wrapping::None,
    }
}

impl<M: Clone + 'static> Widget<M, Theme, Renderer> for SelectField<'_, M> {
    fn tag(&self) -> tree::Tag {
        tree::Tag::of::<State>()
    }

    fn state(&self) -> tree::State {
        tree::State::new(State {
            id: self.id.clone(),
            focused: false,
            open: false,
            highlighted: 0,
        })
    }

    fn diff(&self, tree: &mut Tree) {
        let state = tree.state.downcast_mut::<State>();
        if state.id != self.id || self.messages.is_none() {
            state.unfocus();
            state.id = self.id.clone();
        }
        state.highlighted = state.highlighted.min(self.labels.len().saturating_sub(1));
    }

    fn size(&self) -> Size<Length> {
        Size::new(self.width, Length::Fixed(self.height()))
    }

    fn layout(
        &mut self,
        _tree: &mut Tree,
        _renderer: &Renderer,
        limits: &layout::Limits,
    ) -> layout::Node {
        layout::atomic(limits, self.width, Length::Fixed(self.height()))
    }

    fn operate(
        &mut self,
        tree: &mut Tree,
        layout: Layout<'_>,
        _renderer: &Renderer,
        operation: &mut dyn Operation,
    ) {
        if self.messages.is_some() {
            operation.focusable(
                Some(&self.id),
                layout.bounds(),
                tree.state.downcast_mut::<State>(),
            );
        }
        if let Some(index) = self.selected {
            operation.text(None, layout.bounds(), &self.labels[index]);
        }
    }

    fn update(
        &mut self,
        tree: &mut Tree,
        event: &Event,
        layout: Layout<'_>,
        cursor: mouse::Cursor,
        _renderer: &Renderer,
        _clipboard: &mut dyn Clipboard,
        shell: &mut Shell<'_, M>,
        _viewport: &Rectangle,
    ) {
        let state = tree.state.downcast_mut::<State>();
        let Some(messages) = self.messages.as_ref() else {
            return;
        };
        match event {
            Event::Window(iced::window::Event::Unfocused) => {
                state.open = false;
            }
            Event::Mouse(mouse::Event::ButtonPressed(mouse::Button::Left))
            | Event::Touch(iced::touch::Event::FingerPressed { .. }) => {
                if cursor.is_over(layout.bounds()) {
                    state.focused = true;
                    if state.open {
                        state.open = false;
                    } else {
                        self.open_on_current(state);
                    }
                    shell.publish(self.on_focus.clone());
                    shell.capture_event();
                    shell.request_redraw();
                } else {
                    state.unfocus();
                }
            }
            Event::Keyboard(keyboard::Event::KeyPressed {
                key,
                modifiers,
                repeat,
                ..
            }) if state.focused && !modifiers.command() && !modifiers.alt() => {
                let last = messages.len().saturating_sub(1);
                let handled = match key {
                    keyboard::Key::Named(key::Named::Escape) if state.open => {
                        state.open = false;
                        true
                    }
                    keyboard::Key::Named(key::Named::Tab) => {
                        state.open = false;
                        false
                    }
                    keyboard::Key::Named(key::Named::Enter | key::Named::Space) if !repeat => {
                        if state.open {
                            state.open = false;
                            shell.publish(messages[state.highlighted.min(last)].clone());
                        } else {
                            self.open_on_current(state);
                        }
                        true
                    }
                    keyboard::Key::Named(key::Named::ArrowDown) => {
                        if state.open {
                            state.highlighted = (state.highlighted + 1).min(last);
                        } else {
                            self.open_on_current(state);
                        }
                        true
                    }
                    keyboard::Key::Named(key::Named::ArrowUp) => {
                        if state.open {
                            state.highlighted = state.highlighted.saturating_sub(1);
                        } else {
                            self.open_on_current(state);
                        }
                        true
                    }
                    keyboard::Key::Named(key::Named::Home) if state.open => {
                        state.highlighted = 0;
                        true
                    }
                    keyboard::Key::Named(key::Named::End) if state.open => {
                        state.highlighted = last;
                        true
                    }
                    _ => false,
                };
                if handled {
                    shell.capture_event();
                    shell.request_redraw();
                }
            }
            _ => {}
        }
    }

    fn draw(
        &self,
        tree: &Tree,
        renderer: &mut Renderer,
        theme: &Theme,
        _style: &renderer::Style,
        layout: Layout<'_>,
        cursor: mouse::Cursor,
        viewport: &Rectangle,
    ) {
        let state = tree.state.downcast_ref::<State>();
        let shell = &theme.colors.shell;
        let bounds = layout.bounds();
        let enabled = self.messages.is_some();
        let (border_color, border_width) = if !enabled {
            (faded(shell.line, DISABLED_ALPHA), 1.0)
        } else if state.focused || state.open {
            (shell.focus, 2.0)
        } else if cursor.is_over(bounds) {
            (shell.muted, 1.0)
        } else {
            (shell.line, 1.0)
        };
        renderer.fill_quad(
            renderer::Quad {
                bounds,
                border: Border {
                    color: border_color,
                    width: border_width,
                    radius: self.radius().into(),
                },
                ..renderer::Quad::default()
            },
            Background::Color(shell.background),
        );

        let padding = self.padding_x();
        let chevron_room = size::ICON_SMALL + 8.0;
        let (content, color) = match self.selected {
            Some(index) => (
                self.labels[index].as_ref(),
                if enabled { shell.ink } else { shell.muted },
            ),
            None => (self.placeholder.as_ref(), shell.muted),
        };
        renderer.fill_text(
            label_text(
                content,
                self.text_size(),
                Size::new(bounds.width - padding * 2.0 - chevron_room, 20.0),
                font::REGULAR,
            ),
            Point::new(bounds.x + padding, bounds.center_y()),
            color,
            bounds.intersection(viewport).unwrap_or(bounds),
        );
        renderer.fill_text(
            text::Text {
                align_x: text::Alignment::Right,
                ..label_text(
                    &CHEVRON_DOWN.to_string(),
                    size::ICON_SMALL - 4.0,
                    Size::new(size::ICON_SMALL, 20.0),
                    BOOTSTRAP_ICONS,
                )
            },
            Point::new(bounds.x + bounds.width - padding, bounds.center_y()),
            if enabled {
                shell.muted
            } else {
                faded(shell.muted, DISABLED_ALPHA)
            },
            *viewport,
        );
    }

    fn mouse_interaction(
        &self,
        _tree: &Tree,
        layout: Layout<'_>,
        cursor: mouse::Cursor,
        _viewport: &Rectangle,
        _renderer: &Renderer,
    ) -> mouse::Interaction {
        if self.messages.is_some() && cursor.is_over(layout.bounds()) {
            mouse::Interaction::Pointer
        } else {
            mouse::Interaction::default()
        }
    }

    fn overlay<'b>(
        &'b mut self,
        tree: &'b mut Tree,
        layout: Layout<'b>,
        _renderer: &Renderer,
        _viewport: &Rectangle,
        translation: Vector,
    ) -> Option<overlay::Element<'b, M, Theme, Renderer>> {
        let state = tree.state.downcast_mut::<State>();
        let messages = self.messages.as_ref()?;
        if !state.open {
            return None;
        }
        Some(overlay::Element::new(Box::new(Menu {
            state,
            labels: &self.labels,
            selected: self.selected,
            messages,
            anchor: layout.bounds() + translation,
            text_size: self.text_size(),
        })))
    }
}

/// The open option list, drawn over everything below the field.
struct Menu<'b, 'a, M> {
    state: &'b mut State,
    labels: &'b [Cow<'a, str>],
    selected: Option<usize>,
    messages: &'b [M],
    anchor: Rectangle,
    text_size: f32,
}

impl<M> Menu<'_, '_, M> {
    fn row_at(&self, layout: Layout<'_>, point: Point) -> Option<usize> {
        let bounds = layout.bounds();
        if !bounds.contains(point) {
            return None;
        }
        let y = point.y - bounds.y - MENU_PADDING;
        let index = (y / ROW_HEIGHT).floor();
        (index >= 0.0 && (index as usize) < self.labels.len()).then_some(index as usize)
    }
}

impl<M: Clone> overlay::Overlay<M, Theme, Renderer> for Menu<'_, '_, M> {
    fn layout(&mut self, _renderer: &Renderer, bounds: Size) -> layout::Node {
        let height = self.labels.len() as f32 * ROW_HEIGHT + MENU_PADDING * 2.0;
        let below = self.anchor.y + self.anchor.height + MENU_GAP;
        let y = if below + height > bounds.height && self.anchor.y - MENU_GAP - height >= 0.0 {
            self.anchor.y - MENU_GAP - height
        } else {
            below
        };
        layout::Node::new(Size::new(self.anchor.width, height))
            .move_to(Point::new(self.anchor.x, y))
    }

    fn draw(
        &self,
        renderer: &mut Renderer,
        theme: &Theme,
        _style: &renderer::Style,
        layout: Layout<'_>,
        _cursor: mouse::Cursor,
    ) {
        let shell = &theme.colors.shell;
        let bounds = layout.bounds();
        renderer.fill_quad(
            renderer::Quad {
                bounds,
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
                ..renderer::Quad::default()
            },
            Background::Color(shell.panel),
        );
        for (index, label) in self.labels.iter().enumerate() {
            let row = Rectangle {
                x: bounds.x + MENU_PADDING,
                y: bounds.y + MENU_PADDING + index as f32 * ROW_HEIGHT,
                width: bounds.width - MENU_PADDING * 2.0,
                height: ROW_HEIGHT,
            };
            if index == self.state.highlighted {
                renderer.fill_quad(
                    renderer::Quad {
                        bounds: row,
                        border: Border {
                            radius: radius::ROW.into(),
                            ..Border::default()
                        },
                        ..renderer::Quad::default()
                    },
                    Background::Color(shell.selected),
                );
            }
            let is_selected = self.selected == Some(index);
            renderer.fill_text(
                label_text(
                    label,
                    self.text_size,
                    Size::new(row.width - 44.0, 20.0),
                    if is_selected {
                        font::SEMIBOLD
                    } else {
                        font::REGULAR
                    },
                ),
                Point::new(row.x + 10.0, row.center_y()),
                shell.ink,
                row,
            );
            if is_selected {
                renderer.fill_text(
                    text::Text {
                        align_x: text::Alignment::Right,
                        ..label_text(
                            &CHECK.to_string(),
                            size::ICON_SMALL,
                            Size::new(size::ICON_SMALL, 20.0),
                            BOOTSTRAP_ICONS,
                        )
                    },
                    Point::new(row.x + row.width - 10.0, row.center_y()),
                    shell.accent_ink,
                    row,
                );
            }
        }
    }

    fn update(
        &mut self,
        event: &Event,
        layout: Layout<'_>,
        cursor: mouse::Cursor,
        _renderer: &Renderer,
        _clipboard: &mut dyn Clipboard,
        shell: &mut Shell<'_, M>,
    ) {
        let Some(point) = cursor.position() else {
            return;
        };
        match event {
            Event::Mouse(mouse::Event::CursorMoved { .. }) => {
                if let Some(index) = self.row_at(layout, point) {
                    if index != self.state.highlighted {
                        self.state.highlighted = index;
                        shell.request_redraw();
                    }
                }
            }
            Event::Mouse(mouse::Event::ButtonPressed(mouse::Button::Left))
            | Event::Touch(iced::touch::Event::FingerPressed { .. }) => {
                if let Some(index) = self.row_at(layout, point) {
                    self.state.open = false;
                    shell.publish(self.messages[index].clone());
                    shell.capture_event();
                    shell.request_redraw();
                } else if layout.bounds().contains(point) {
                    shell.capture_event();
                }
            }
            _ => {}
        }
    }

    fn mouse_interaction(
        &self,
        layout: Layout<'_>,
        cursor: mouse::Cursor,
        _renderer: &Renderer,
    ) -> mouse::Interaction {
        match cursor.position() {
            Some(point) if self.row_at(layout, point).is_some() => mouse::Interaction::Pointer,
            Some(point) if layout.bounds().contains(point) => mouse::Interaction::Idle,
            _ => mouse::Interaction::None,
        }
    }
}
