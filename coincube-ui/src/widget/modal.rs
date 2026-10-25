/// modal widget from https://github.com/iced-rs/iced/blob/master/examples/modal/
use iced::advanced::layout::{self, Layout};
use iced::advanced::overlay;
use iced::advanced::renderer;
use iced::advanced::widget::{self, Tree, Widget};
use iced::advanced::{self, Clipboard, Shell};
use iced::alignment::Alignment;
use iced::keyboard::{self, key};
use iced::mouse;
use iced::{Color, Element, Event, Length, Point, Rectangle, Size, Vector};

/// A widget that centers a modal element over some base element
pub struct Modal<'a, Message, Theme, Renderer> {
    base: Element<'a, Message, Theme, Renderer>,
    modal: Element<'a, Message, Theme, Renderer>,
    on_blur: Option<Message>,
}

impl<'a, Message, Theme, Renderer> Modal<'a, Message, Theme, Renderer> {
    /// Returns a new [`Modal`]
    pub fn new(
        base: impl Into<Element<'a, Message, Theme, Renderer>>,
        modal: impl Into<Element<'a, Message, Theme, Renderer>>,
    ) -> Self {
        Self {
            base: base.into(),
            modal: modal.into(),
            on_blur: None,
        }
    }

    /// Sets the message produced when the background is pressed or Escape is
    /// pressed without being handled by modal content.
    pub fn on_blur(self, on_blur: Option<Message>) -> Self {
        Self { on_blur, ..self }
    }
}

impl<'a, Message, Theme, Renderer> Widget<Message, Theme, Renderer>
    for Modal<'a, Message, Theme, Renderer>
where
    Renderer: advanced::Renderer,
    Message: Clone,
{
    fn children(&self) -> Vec<Tree> {
        vec![Tree::new(&self.base), Tree::new(&self.modal)]
    }

    fn diff(&self, tree: &mut Tree) {
        tree.diff_children(&[&self.base, &self.modal]);
    }

    fn size(&self) -> Size<Length> {
        self.base.as_widget().size()
    }

    fn layout(
        &mut self,
        tree: &mut widget::Tree,
        renderer: &Renderer,
        limits: &layout::Limits,
    ) -> layout::Node {
        self.base
            .as_widget_mut()
            .layout(&mut tree.children[0], renderer, limits)
    }

    fn update(
        &mut self,
        _state: &mut widget::Tree,
        _event: &Event,
        _layout: Layout<'_>,
        _cursor: mouse::Cursor,
        _renderer: &Renderer,
        _clipboard: &mut dyn Clipboard,
        _shell: &mut Shell<'_, Message>,
        _viewport: &Rectangle,
    ) {
        // The base is visible beneath the scrim, but cannot receive input
        // while its overlay is open, including for non-dismissible modals.
    }

    fn draw(
        &self,
        state: &widget::Tree,
        renderer: &mut Renderer,
        theme: &Theme,
        style: &renderer::Style,
        layout: Layout<'_>,
        cursor: mouse::Cursor,
        viewport: &Rectangle,
    ) {
        self.base.as_widget().draw(
            &state.children[0],
            renderer,
            theme,
            style,
            layout,
            cursor,
            viewport,
        );
    }

    fn overlay<'b>(
        &'b mut self,
        state: &'b mut Tree,
        layout: Layout<'_>,
        _renderer: &Renderer,
        _viewport: &Rectangle,
        translation: Vector,
    ) -> Option<overlay::Element<'b, Message, Theme, Renderer>> {
        Some(overlay::Element::new(Box::new(Overlay {
            position: layout.position() + translation,
            content: &mut self.modal,
            tree: &mut state.children[1],
            size: layout.bounds().size(),
            on_blur: self.on_blur.clone(),
        })))
    }

    fn mouse_interaction(
        &self,
        _state: &widget::Tree,
        _layout: Layout<'_>,
        _cursor: mouse::Cursor,
        _viewport: &Rectangle,
        _renderer: &Renderer,
    ) -> mouse::Interaction {
        mouse::Interaction::default()
    }

    fn operate(
        &mut self,
        _state: &mut Tree,
        _layout: Layout<'_>,
        _renderer: &Renderer,
        _operation: &mut dyn widget::Operation,
    ) {
        // Iced operates the root before its overlay. Including the base here
        // would put hidden controls before the modal in Tab traversal.
    }
}

struct Overlay<'a, 'b, Message, Theme, Renderer> {
    position: Point,
    content: &'b mut Element<'a, Message, Theme, Renderer>,
    tree: &'b mut Tree,
    size: Size,
    on_blur: Option<Message>,
}

impl<'a, 'b, Message, Theme, Renderer> overlay::Overlay<Message, Theme, Renderer>
    for Overlay<'a, 'b, Message, Theme, Renderer>
where
    Renderer: advanced::Renderer,
    Message: Clone,
{
    fn layout(&mut self, renderer: &Renderer, _bounds: Size) -> layout::Node {
        let limits = layout::Limits::new(Size::ZERO, self.size)
            .width(Length::Fill)
            .height(Length::Fill);

        let child = self
            .content
            .as_widget_mut()
            .layout(self.tree, renderer, &limits)
            .align(Alignment::Center, Alignment::Center, limits.max());

        layout::Node::with_children(self.size, vec![child]).move_to(self.position)
    }

    fn update(
        &mut self,
        event: &Event,
        layout: Layout<'_>,
        cursor: mouse::Cursor,
        renderer: &Renderer,
        clipboard: &mut dyn Clipboard,
        shell: &mut Shell<'_, Message>,
    ) {
        let content_bounds = layout.children().next().unwrap().bounds();

        if let Some(message) = self.on_blur.as_ref() {
            if let Event::Mouse(mouse::Event::ButtonPressed(mouse::Button::Left)) = &event {
                if !cursor.is_over(content_bounds) {
                    shell.publish(message.clone());
                    shell.capture_event();
                    return;
                }
            }
        }

        self.content.as_widget_mut().update(
            self.tree,
            event,
            layout.children().next().unwrap(),
            cursor,
            renderer,
            clipboard,
            shell,
            &layout.bounds(),
        );

        // Give focused modal content first refusal (for example a text field).
        // Otherwise Escape dismisses a blur-enabled modal and never reaches
        // the shell behind a non-dismissible one.
        if !shell.is_event_captured()
            && matches!(
                event,
                Event::Keyboard(keyboard::Event::KeyPressed {
                    key: keyboard::Key::Named(key::Named::Escape),
                    repeat: false,
                    ..
                })
            )
        {
            if let Some(message) = &self.on_blur {
                shell.publish(message.clone());
            }
            shell.capture_event();
        }
    }

    fn draw(
        &self,
        renderer: &mut Renderer,
        theme: &Theme,
        style: &renderer::Style,
        layout: Layout<'_>,
        cursor: mouse::Cursor,
    ) {
        renderer.fill_quad(
            renderer::Quad {
                bounds: layout.bounds(),
                ..renderer::Quad::default()
            },
            Color {
                a: 0.80,
                ..Color::BLACK
            },
        );

        self.content.as_widget().draw(
            self.tree,
            renderer,
            theme,
            style,
            layout.children().next().unwrap(),
            cursor,
            &layout.bounds(),
        );
    }

    fn operate(
        &mut self,
        layout: Layout<'_>,
        renderer: &Renderer,
        operation: &mut dyn widget::Operation,
    ) {
        self.content.as_widget_mut().operate(
            self.tree,
            layout.children().next().unwrap(),
            renderer,
            operation,
        );
    }

    fn mouse_interaction(
        &self,
        layout: Layout<'_>,
        cursor: mouse::Cursor,
        renderer: &Renderer,
    ) -> mouse::Interaction {
        self.content.as_widget().mouse_interaction(
            self.tree,
            layout.children().next().unwrap(),
            cursor,
            &layout.bounds(),
            renderer,
        )
    }

    fn overlay<'c>(
        &'c mut self,
        layout: Layout<'c>,
        renderer: &Renderer,
    ) -> Option<overlay::Element<'c, Message, Theme, Renderer>> {
        self.content.as_widget_mut().overlay(
            self.tree,
            layout.children().next().unwrap(),
            renderer,
            &layout.bounds(),
            Vector::ZERO,
        )
    }
}

impl<'a, Message, Theme, Renderer> From<Modal<'a, Message, Theme, Renderer>>
    for Element<'a, Message, Theme, Renderer>
where
    Renderer: 'a + advanced::Renderer,
    Message: 'a + Clone,
    Theme: 'a,
{
    fn from(modal: Modal<'a, Message, Theme, Renderer>) -> Self {
        Element::new(modal)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{font, theme::Theme, widget::action::Action};
    use iced::advanced::{renderer::Headless, widget::operation, widget::operation::focusable};
    use iced::widget::{column, container, text};
    use iced::Event;
    use iced_runtime::user_interface::{Cache, UserInterface};
    use std::sync::{Arc, Mutex};

    #[derive(Debug, Clone, PartialEq, Eq)]
    enum Message {
        Background,
        Close,
        Copy,
        Focus(widget::Id),
    }

    fn renderer() -> crate::widget::Renderer {
        for bytes in font::load() {
            iced::advanced::graphics::text::font_system()
                .write()
                .unwrap()
                .load_font(bytes);
        }
        iced::futures::executor::block_on(<crate::widget::Renderer as Headless>::new(
            font::REGULAR,
            14.into(),
            Some("tiny-skia"),
        ))
        .unwrap()
    }

    fn modal(
        on_blur: Option<Message>,
    ) -> Element<'static, Message, Theme, crate::widget::Renderer> {
        Modal::new(
            container(column![Action::new(
                "background",
                text("Background"),
                Message::Focus
            )
            .on_press(Message::Background)])
            .width(Length::Fill)
            .height(Length::Fill),
            column![
                Action::new("close", text("Close"), Message::Focus).on_press(Message::Close),
                Action::new("copy", text("Copy"), Message::Focus).on_press(Message::Copy),
            ],
        )
        .on_blur(on_blur)
        .into()
    }

    fn operate<T: Send + 'static>(
        ui: &mut UserInterface<'_, Message, Theme, crate::widget::Renderer>,
        renderer: &crate::widget::Renderer,
        operation: impl widget::Operation<T> + 'static,
    ) -> Option<T> {
        // UserInterface::operate erases operation output to (). Keep a copy
        // so the test can inspect the real root-then-overlay traversal result.
        let result = Arc::new(Mutex::new(None));
        let output = Arc::clone(&result);
        let mut operation: Box<dyn widget::Operation<()>> =
            Box::new(operation::map(operation, move |value| {
                *output.lock().unwrap() = Some(value);
            }));
        loop {
            ui.operate(renderer, &mut operation::black_box(operation.as_mut()));
            match operation.finish() {
                operation::Outcome::None | operation::Outcome::Some(()) => break,
                operation::Outcome::Chain(next) => operation = next,
            }
        }
        drop(operation);
        Arc::try_unwrap(result).ok().unwrap().into_inner().unwrap()
    }

    fn escape() -> Event {
        let key = keyboard::Key::Named(key::Named::Escape);
        Event::Keyboard(keyboard::Event::KeyPressed {
            key: key.clone(),
            modified_key: key,
            physical_key: keyboard::key::Physical::Code(key::Code::Escape),
            location: keyboard::Location::Standard,
            modifiers: keyboard::Modifiers::empty(),
            text: None,
            repeat: false,
        })
    }

    fn enter() -> Event {
        let key = keyboard::Key::Named(key::Named::Enter);
        Event::Keyboard(keyboard::Event::KeyPressed {
            key: key.clone(),
            modified_key: key,
            physical_key: keyboard::key::Physical::Code(key::Code::Enter),
            location: keyboard::Location::Standard,
            modifiers: keyboard::Modifiers::empty(),
            text: None,
            repeat: false,
        })
    }

    #[test]
    fn composed_modal_confines_focus_and_escape() {
        let mut renderer = renderer();
        for on_blur in [Some(Message::Close), None] {
            let mut ui = UserInterface::build(
                modal(on_blur.clone()),
                Size::new(400.0, 300.0),
                Cache::new(),
                &mut renderer,
            );
            operate(&mut ui, &renderer, focusable::focus_next::<()>());
            assert_eq!(
                operate(&mut ui, &renderer, focusable::find_focused()),
                Some(widget::Id::new("close")),
            );
            operate(
                &mut ui,
                &renderer,
                focusable::focus::<()>(widget::Id::new("background")),
            );
            assert_eq!(operate(&mut ui, &renderer, focusable::find_focused()), None,);
            let mut messages = Vec::new();
            ui.update(
                &[enter()],
                mouse::Cursor::Unavailable,
                &mut renderer,
                &mut iced::advanced::clipboard::Null,
                &mut messages,
            );
            assert!(messages.is_empty(), "hidden page must not activate");
            operate(&mut ui, &renderer, focusable::focus_next::<()>());
            assert_eq!(
                operate(&mut ui, &renderer, focusable::find_focused()),
                Some(widget::Id::new("close")),
            );
            operate(&mut ui, &renderer, focusable::focus_next::<()>());
            assert_eq!(
                operate(&mut ui, &renderer, focusable::find_focused()),
                Some(widget::Id::new("copy")),
            );
            operate(&mut ui, &renderer, focusable::focus_previous::<()>());
            assert_eq!(
                operate(&mut ui, &renderer, focusable::find_focused()),
                Some(widget::Id::new("close")),
            );

            let (_, statuses) = ui.update(
                &[escape()],
                mouse::Cursor::Unavailable,
                &mut renderer,
                &mut iced::advanced::clipboard::Null,
                &mut messages,
            );
            assert_eq!(statuses, [iced::event::Status::Captured]);
            assert_eq!(messages, on_blur.into_iter().collect::<Vec<_>>());
        }
    }
}
