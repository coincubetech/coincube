//! An Iced button with a real focus target and keyboard activation.
//!
//! The inner `Button` retains its pointer hit target and theme style. Only an
//! enabled action registers with Iced's focus operations, so Tab skips disabled
//! actions. Stable semantic IDs let a view restore focus after a disclosure.

use crate::theme::Theme;
use crate::widget::{Button, Element, Renderer};
use iced::advanced::layout::{self, Layout};
use iced::advanced::overlay;
use iced::advanced::renderer;
use iced::advanced::widget::operation::Focusable;
use iced::advanced::widget::operation::{self, Outcome};
use iced::advanced::widget::tree::{self, Tree};
use iced::advanced::widget::{Id, Operation, Widget};
use iced::advanced::{Clipboard, Shell};
use iced::keyboard::{self, key};
use iced::mouse;
use iced::widget::button;
use iced::{Border, Color, Event, Length, Padding, Rectangle, Size, Vector};

/// Move focus in visual tree order, then reveal it inside the named scrollable.
/// The second operation runs after Iced's focus operation completes.
pub fn focus_next_visible<Message: Send + 'static>(
    scroll_id: impl Into<Id>,
) -> iced::Task<Message> {
    iced::widget::operation::focus_next().chain(reveal_focused(scroll_id.into()))
}

pub fn focus_previous_visible<Message: Send + 'static>(
    scroll_id: impl Into<Id>,
) -> iced::Task<Message> {
    iced::widget::operation::focus_previous().chain(reveal_focused(scroll_id.into()))
}

fn reveal_focused<Message: Send + 'static>(scroll_id: Id) -> iced::Task<Message> {
    iced_runtime::task::widget(FocusedScrollDelta::new(scroll_id)).then(|delta| {
        iced::widget::operation::scroll_by(
            delta.id,
            iced::widget::operation::AbsoluteOffset { x: 0.0, y: delta.y },
        )
    })
}

#[derive(Debug, Clone)]
struct ScrollDelta {
    id: Id,
    y: f32,
}

#[derive(Debug, Clone)]
struct ScrollContext {
    depth: usize,
    bounds: Rectangle,
    translation: Vector,
}

struct FocusedScrollDelta {
    target: Id,
    depth: usize,
    scroll: Option<ScrollContext>,
    delta: Option<ScrollDelta>,
}

impl FocusedScrollDelta {
    fn new(target: Id) -> Self {
        Self {
            target,
            depth: 0,
            scroll: None,
            delta: None,
        }
    }
}

impl Operation<ScrollDelta> for FocusedScrollDelta {
    fn traverse(&mut self, operate: &mut dyn FnMut(&mut dyn Operation<ScrollDelta>)) {
        self.depth += 1;
        operate(self);
        self.depth -= 1;
        if self
            .scroll
            .as_ref()
            .is_some_and(|scroll| scroll.depth == self.depth)
        {
            self.scroll = None;
        }
    }

    fn scrollable(
        &mut self,
        id: Option<&Id>,
        bounds: Rectangle,
        _content_bounds: Rectangle,
        translation: Vector,
        _state: &mut dyn operation::Scrollable,
    ) {
        if id == Some(&self.target) {
            self.scroll = Some(ScrollContext {
                depth: self.depth,
                bounds,
                translation,
            });
        }
    }

    fn focusable(&mut self, _id: Option<&Id>, bounds: Rectangle, state: &mut dyn Focusable) {
        let Some(scroll) = self.scroll.as_ref() else {
            return;
        };
        if !state.is_focused() {
            return;
        }
        let screen_top = bounds.y - scroll.translation.y;
        let screen_bottom = screen_top + bounds.height;
        // A margin can be impossible for the final item at maximum scroll.
        // The focus ring is drawn inside these bounds, so exact visibility is enough.
        let reveal_margin = 0.0;
        let delta = if screen_top < scroll.bounds.y + reveal_margin {
            screen_top - scroll.bounds.y - reveal_margin
        } else if screen_bottom > scroll.bounds.y + scroll.bounds.height - reveal_margin {
            screen_bottom - scroll.bounds.y - scroll.bounds.height + reveal_margin
        } else {
            0.0
        };
        if delta != 0.0 {
            self.delta = Some(ScrollDelta {
                id: self.target.clone(),
                y: delta,
            });
        }
    }

    fn finish(&self) -> Outcome<ScrollDelta> {
        self.delta.clone().map_or(Outcome::None, Outcome::Some)
    }
}

pub struct Action<'a, Message> {
    id: Id,
    button: Button<'a, Message>,
    on_press: Option<Message>,
    focus_radius: f32,
}

impl<'a, Message: Clone + 'a> Action<'a, Message> {
    pub fn new(id: impl Into<Id>, content: impl Into<Element<'a, Message>>) -> Self {
        Self {
            id: id.into(),
            button: Button::new(content),
            on_press: None,
            focus_radius: 8.0,
        }
    }

    pub fn width(mut self, width: impl Into<Length>) -> Self {
        self.button = self.button.width(width);
        self
    }

    pub fn height(mut self, height: impl Into<Length>) -> Self {
        self.button = self.button.height(height);
        self
    }

    pub fn padding(mut self, padding: impl Into<Padding>) -> Self {
        self.button = self.button.padding(padding);
        self
    }

    pub fn style(mut self, style: impl Fn(&Theme, button::Status) -> button::Style + 'a) -> Self {
        self.button = self.button.style(style);
        self
    }

    pub fn focus_radius(mut self, radius: f32) -> Self {
        self.focus_radius = radius;
        self
    }

    pub fn on_press(mut self, message: Message) -> Self {
        self.button = self.button.on_press(message.clone());
        self.on_press = Some(message);
        self
    }

    pub fn on_press_maybe(mut self, message: Option<Message>) -> Self {
        self.button = self.button.on_press_maybe(message.clone());
        self.on_press = message;
        self
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ActivationKey {
    Enter,
    Space,
}

impl ActivationKey {
    fn from_key(key: &keyboard::Key) -> Option<Self> {
        match key {
            keyboard::Key::Named(key::Named::Enter) => Some(Self::Enter),
            keyboard::Key::Named(key::Named::Space) => Some(Self::Space),
            _ => None,
        }
    }
}

#[derive(Debug)]
struct State {
    id: Id,
    focused: bool,
    held: Option<ActivationKey>,
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
        self.held = None;
    }
}

impl<Message: Clone + 'static> Widget<Message, Theme, Renderer> for Action<'_, Message> {
    fn tag(&self) -> tree::Tag {
        tree::Tag::of::<State>()
    }

    fn state(&self) -> tree::State {
        tree::State::new(State {
            id: self.id.clone(),
            focused: false,
            held: None,
        })
    }

    fn children(&self) -> Vec<Tree> {
        vec![Tree::new(
            &self.button as &dyn Widget<Message, Theme, Renderer>,
        )]
    }

    fn diff(&self, tree: &mut Tree) {
        let state = tree.state.downcast_mut::<State>();
        if state.id != self.id || self.on_press.is_none() {
            state.unfocus();
            state.id = self.id.clone();
        }
        tree.diff_children(&[&self.button as &dyn Widget<Message, Theme, Renderer>]);
    }

    fn size(&self) -> Size<Length> {
        self.button.size()
    }

    fn layout(
        &mut self,
        tree: &mut Tree,
        renderer: &Renderer,
        limits: &layout::Limits,
    ) -> layout::Node {
        self.button.layout(&mut tree.children[0], renderer, limits)
    }

    fn operate(
        &mut self,
        tree: &mut Tree,
        layout: Layout<'_>,
        renderer: &Renderer,
        operation: &mut dyn Operation,
    ) {
        if self.on_press.is_some() {
            operation.focusable(
                Some(&self.id),
                layout.bounds(),
                tree.state.downcast_mut::<State>(),
            );
        }
        operation.traverse(&mut |operation| {
            self.button
                .operate(&mut tree.children[0], layout, renderer, operation);
        });
    }

    fn update(
        &mut self,
        tree: &mut Tree,
        event: &Event,
        layout: Layout<'_>,
        cursor: mouse::Cursor,
        renderer: &Renderer,
        clipboard: &mut dyn Clipboard,
        shell: &mut Shell<'_, Message>,
        viewport: &Rectangle,
    ) {
        if matches!(
            event,
            Event::Mouse(mouse::Event::ButtonPressed(mouse::Button::Left))
        ) {
            let state = tree.state.downcast_mut::<State>();
            if self.on_press.is_some() && cursor.is_over(layout.bounds()) {
                state.focus();
            } else {
                state.unfocus();
            }
        }

        self.button.update(
            &mut tree.children[0],
            event,
            layout,
            cursor,
            renderer,
            clipboard,
            shell,
            viewport,
        );
        if shell.is_event_captured() || self.on_press.is_none() {
            return;
        }

        let state = tree.state.downcast_mut::<State>();
        match event {
            Event::Keyboard(keyboard::Event::KeyPressed {
                key,
                modifiers,
                repeat,
                ..
            }) if state.focused && !*repeat && !modifiers.command() && !modifiers.alt() => {
                if let Some(activation) = ActivationKey::from_key(key) {
                    if state.held.is_none() {
                        state.held = Some(activation);
                        shell.publish(self.on_press.as_ref().unwrap().clone());
                        shell.capture_event();
                    }
                }
            }
            Event::Keyboard(keyboard::Event::KeyReleased { key, .. }) => {
                if state.held == ActivationKey::from_key(key) {
                    state.held = None;
                }
            }
            Event::Window(iced::window::Event::Unfocused) => {
                // The release may go to another app after a window switch.
                state.held = None;
            }
            _ => {}
        }
    }

    fn draw(
        &self,
        tree: &Tree,
        renderer: &mut Renderer,
        theme: &Theme,
        style: &renderer::Style,
        layout: Layout<'_>,
        cursor: mouse::Cursor,
        viewport: &Rectangle,
    ) {
        self.button.draw(
            &tree.children[0],
            renderer,
            theme,
            style,
            layout,
            cursor,
            viewport,
        );
        if self.on_press.is_some() && tree.state.downcast_ref::<State>().focused {
            use iced::advanced::Renderer as _;
            renderer.fill_quad(
                renderer::Quad {
                    bounds: layout.bounds(),
                    border: Border {
                        color: theme.colors.shell.accent_ink,
                        width: 2.0,
                        radius: self.focus_radius.into(),
                    },
                    ..Default::default()
                },
                Color::TRANSPARENT,
            );
        }
    }

    fn mouse_interaction(
        &self,
        tree: &Tree,
        layout: Layout<'_>,
        cursor: mouse::Cursor,
        viewport: &Rectangle,
        renderer: &Renderer,
    ) -> mouse::Interaction {
        self.button
            .mouse_interaction(&tree.children[0], layout, cursor, viewport, renderer)
    }

    fn overlay<'b>(
        &'b mut self,
        tree: &'b mut Tree,
        layout: Layout<'b>,
        renderer: &Renderer,
        viewport: &Rectangle,
        translation: Vector,
    ) -> Option<overlay::Element<'b, Message, Theme, Renderer>> {
        self.button.overlay(
            &mut tree.children[0],
            layout,
            renderer,
            viewport,
            translation,
        )
    }
}

impl<'a, Message: Clone + 'static> From<Action<'a, Message>> for Element<'a, Message> {
    fn from(action: Action<'a, Message>) -> Self {
        Self::new(action)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::font;
    use iced::advanced::renderer::Headless;
    use iced::advanced::widget::operation::focusable;
    use iced::advanced::Layout;
    use iced::widget::{column, scrollable, text, text_input};

    #[derive(Debug, Clone, PartialEq, Eq)]
    enum Message {
        First,
        Second,
        Input(String),
        Submitted,
    }

    fn load_fonts() {
        for bytes in font::load() {
            iced::advanced::graphics::text::font_system()
                .write()
                .unwrap()
                .load_font(bytes);
        }
    }

    fn tree() -> Element<'static, Message> {
        column![
            Action::new("first", text("First"))
                .on_press(Message::First)
                .width(Length::Fixed(180.0)),
            Action::new("disabled", text("Disabled")).on_press_maybe(None),
            text_input("Enter submits", "")
                .id("input")
                .on_input(Message::Input)
                .on_submit(Message::Submitted),
            Action::new("second", text("Second")).on_press(Message::Second),
        ]
        .spacing(10)
        .into()
    }

    fn run_operation<T: 'static>(
        element: &mut Element<'_, Message>,
        tree: &mut Tree,
        node: &layout::Node,
        renderer: &Renderer,
        operation: impl Operation<T> + 'static,
    ) -> Option<T> {
        let mut operation: Box<dyn Operation<T>> = Box::new(operation);
        loop {
            element.as_widget_mut().operate(
                tree,
                Layout::new(node),
                renderer,
                &mut operation::black_box(operation.as_mut()),
            );
            match operation.finish() {
                Outcome::None => return None,
                Outcome::Some(result) => return Some(result),
                Outcome::Chain(next) => operation = next,
            }
        }
    }

    fn send_event(
        element: &mut Element<'_, Message>,
        tree: &mut Tree,
        node: &layout::Node,
        renderer: &Renderer,
        event: Event,
    ) -> Vec<Message> {
        let mut messages = Vec::new();
        element.as_widget_mut().update(
            tree,
            &event,
            Layout::new(node),
            mouse::Cursor::Unavailable,
            renderer,
            &mut iced::advanced::clipboard::Null,
            &mut Shell::new(&mut messages),
            &Rectangle::with_size(Size::new(260.0, 240.0)),
        );
        messages
    }

    fn key_event(named: key::Named, repeat: bool, pressed: bool) -> Event {
        let key = keyboard::Key::Named(named);
        let physical_key = key::Physical::Unidentified(key::NativeCode::Unidentified);
        let location = keyboard::Location::Standard;
        let modifiers = keyboard::Modifiers::empty();
        if pressed {
            Event::Keyboard(keyboard::Event::KeyPressed {
                key: key.clone(),
                modified_key: key,
                physical_key,
                location,
                modifiers,
                text: None,
                repeat,
            })
        } else {
            Event::Keyboard(keyboard::Event::KeyReleased {
                key: key.clone(),
                modified_key: key,
                physical_key,
                location,
                modifiers,
            })
        }
    }

    #[test]
    fn tab_skips_disabled_and_enter_space_activate_once_per_press() {
        load_fonts();
        let renderer = iced::futures::executor::block_on(<Renderer as Headless>::new(
            font::REGULAR,
            14.into(),
            Some("tiny-skia"),
        ))
        .unwrap();
        let mut element = tree();
        let mut state = Tree::new(element.as_widget());
        let node = element.as_widget_mut().layout(
            &mut state,
            &renderer,
            &layout::Limits::new(Size::ZERO, Size::new(260.0, 240.0)),
        );

        run_operation(
            &mut element,
            &mut state,
            &node,
            &renderer,
            focusable::focus_next::<()>(),
        );
        assert_eq!(
            run_operation(
                &mut element,
                &mut state,
                &node,
                &renderer,
                focusable::find_focused(),
            ),
            Some(Id::new("first"))
        );
        assert_eq!(
            send_event(
                &mut element,
                &mut state,
                &node,
                &renderer,
                key_event(key::Named::Enter, false, true),
            ),
            vec![Message::First]
        );
        assert!(send_event(
            &mut element,
            &mut state,
            &node,
            &renderer,
            key_event(key::Named::Enter, true, true),
        )
        .is_empty());
        send_event(
            &mut element,
            &mut state,
            &node,
            &renderer,
            key_event(key::Named::Enter, false, false),
        );
        assert_eq!(
            send_event(
                &mut element,
                &mut state,
                &node,
                &renderer,
                key_event(key::Named::Space, false, true),
            ),
            vec![Message::First]
        );

        run_operation(
            &mut element,
            &mut state,
            &node,
            &renderer,
            focusable::focus_next::<()>(),
        );
        assert_eq!(
            run_operation(
                &mut element,
                &mut state,
                &node,
                &renderer,
                focusable::find_focused(),
            ),
            Some(Id::new("input"))
        );
        assert_eq!(
            send_event(
                &mut element,
                &mut state,
                &node,
                &renderer,
                key_event(key::Named::Enter, false, true),
            ),
            vec![Message::Submitted]
        );

        run_operation(
            &mut element,
            &mut state,
            &node,
            &renderer,
            focusable::focus_next::<()>(),
        );
        assert_eq!(
            run_operation(
                &mut element,
                &mut state,
                &node,
                &renderer,
                focusable::find_focused(),
            ),
            Some(Id::new("second"))
        );
        run_operation(
            &mut element,
            &mut state,
            &node,
            &renderer,
            focusable::focus_previous::<()>(),
        );
        assert_eq!(
            run_operation(
                &mut element,
                &mut state,
                &node,
                &renderer,
                focusable::find_focused(),
            ),
            Some(Id::new("input"))
        );
    }

    #[test]
    fn focused_middle_action_is_revealed_by_scroll_operation() {
        load_fonts();
        let renderer = iced::futures::executor::block_on(<Renderer as Headless>::new(
            font::REGULAR,
            14.into(),
            Some("tiny-skia"),
        ))
        .unwrap();
        let mut element: Element<'_, Message> = scrollable(
            column![
                Action::new("first", text("First"))
                    .on_press(Message::First)
                    .height(Length::Fixed(32.0)),
                Action::new("second", text("Second"))
                    .on_press(Message::Second)
                    .height(Length::Fixed(32.0)),
            ]
            .spacing(24),
        )
        .id("scroll")
        .height(Length::Fixed(40.0))
        .into();
        let mut state = Tree::new(element.as_widget());
        let mut node = element.as_widget_mut().layout(
            &mut state,
            &renderer,
            &layout::Limits::new(Size::ZERO, Size::new(260.0, 40.0)),
        );
        run_operation(
            &mut element,
            &mut state,
            &node,
            &renderer,
            focusable::focus::<()>(Id::new("second")),
        );
        let delta = run_operation(
            &mut element,
            &mut state,
            &node,
            &renderer,
            FocusedScrollDelta::new(Id::new("scroll")),
        )
        .expect("offscreen focused action needs a scroll delta");
        assert!(delta.y > 0.0);
        run_operation(
            &mut element,
            &mut state,
            &node,
            &renderer,
            operation::scrollable::scroll_by::<()>(
                delta.id,
                operation::scrollable::AbsoluteOffset { x: 0.0, y: delta.y },
            ),
        );
        node = element.as_widget_mut().layout(
            &mut state,
            &renderer,
            &layout::Limits::new(Size::ZERO, Size::new(260.0, 40.0)),
        );
        assert!(
            run_operation(
                &mut element,
                &mut state,
                &node,
                &renderer,
                FocusedScrollDelta::new(Id::new("scroll")),
            )
            .is_none(),
            "focused action should now be fully visible"
        );
    }

    #[test]
    fn window_focus_loss_clears_held_activation_key() {
        load_fonts();
        let renderer = iced::futures::executor::block_on(<Renderer as Headless>::new(
            font::REGULAR,
            14.into(),
            Some("tiny-skia"),
        ))
        .unwrap();
        let mut element = tree();
        let mut state = Tree::new(element.as_widget());
        let node = element.as_widget_mut().layout(
            &mut state,
            &renderer,
            &layout::Limits::new(Size::ZERO, Size::new(260.0, 240.0)),
        );
        run_operation(
            &mut element,
            &mut state,
            &node,
            &renderer,
            focusable::focus::<()>(Id::new("first")),
        );
        assert_eq!(
            send_event(
                &mut element,
                &mut state,
                &node,
                &renderer,
                key_event(key::Named::Enter, false, true),
            ),
            vec![Message::First]
        );
        assert!(send_event(
            &mut element,
            &mut state,
            &node,
            &renderer,
            Event::Window(iced::window::Event::Unfocused),
        )
        .is_empty());
        assert_eq!(
            send_event(
                &mut element,
                &mut state,
                &node,
                &renderer,
                key_event(key::Named::Enter, false, true),
            ),
            vec![Message::First],
            "a fresh key press after returning to the window must activate"
        );
    }
}
