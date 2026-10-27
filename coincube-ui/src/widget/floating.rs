//! Content that stands in the layout flow but takes no room in it.
//!
//! [`Floating`] lays its content out at its own position with the width it is
//! given, reports zero height to its parent and paints the content as an
//! overlay, above the siblings that follow it. A disclosure such as a
//! dropdown menu then covers the rows below it instead of pushing them down.
//!
//! Focus operations still reach the content in place, so Tab moves from the
//! control that opened it straight into it. Events and drawing go through
//! the overlay only, so nothing is handled twice.

use iced::advanced::layout::{self, Layout};
use iced::advanced::overlay;
use iced::advanced::renderer;
use iced::advanced::widget::tree::Tree;
use iced::advanced::widget::{Operation, Widget};
use iced::advanced::{Clipboard, Shell};
use iced::{mouse, Event, Length, Point, Rectangle, Size, Vector};

use crate::theme::Theme;
use crate::widget::{Element, Renderer};

pub struct Floating<'a, Message> {
    content: Element<'a, Message>,
}

impl<'a, Message> Floating<'a, Message> {
    pub fn new(content: impl Into<Element<'a, Message>>) -> Self {
        Self {
            content: content.into(),
        }
    }
}

fn content_limits(width: f32) -> layout::Limits {
    layout::Limits::new(Size::ZERO, Size::new(width, f32::INFINITY))
}

impl<Message> Widget<Message, Theme, Renderer> for Floating<'_, Message> {
    fn children(&self) -> Vec<Tree> {
        vec![Tree::new(&self.content)]
    }

    fn diff(&self, tree: &mut Tree) {
        tree.diff_children(&[&self.content]);
    }

    fn size(&self) -> Size<Length> {
        Size::new(Length::Fill, Length::Shrink)
    }

    fn layout(
        &mut self,
        tree: &mut Tree,
        renderer: &Renderer,
        limits: &layout::Limits,
    ) -> layout::Node {
        let width = limits.max().width;
        let content = self.content.as_widget_mut().layout(
            &mut tree.children[0],
            renderer,
            &content_limits(width),
        );
        layout::Node::with_children(Size::new(width, 0.0), vec![content])
    }

    fn operate(
        &mut self,
        tree: &mut Tree,
        layout: Layout<'_>,
        renderer: &Renderer,
        operation: &mut dyn Operation,
    ) {
        if let Some(content) = layout.children().next() {
            self.content.as_widget_mut().operate(
                &mut tree.children[0],
                content,
                renderer,
                operation,
            );
        }
    }

    fn update(
        &mut self,
        _tree: &mut Tree,
        _event: &Event,
        _layout: Layout<'_>,
        _cursor: mouse::Cursor,
        _renderer: &Renderer,
        _clipboard: &mut dyn Clipboard,
        _shell: &mut Shell<'_, Message>,
        _viewport: &Rectangle,
    ) {
    }

    fn draw(
        &self,
        _tree: &Tree,
        _renderer: &mut Renderer,
        _theme: &Theme,
        _style: &renderer::Style,
        _layout: Layout<'_>,
        _cursor: mouse::Cursor,
        _viewport: &Rectangle,
    ) {
    }

    fn overlay<'b>(
        &'b mut self,
        tree: &'b mut Tree,
        layout: Layout<'b>,
        _renderer: &Renderer,
        _viewport: &Rectangle,
        translation: Vector,
    ) -> Option<overlay::Element<'b, Message, Theme, Renderer>> {
        let bounds = layout.children().next()?.bounds();
        Some(overlay::Element::new(Box::new(Overlay {
            content: &mut self.content,
            tree: &mut tree.children[0],
            position: bounds.position() + translation,
            width: bounds.width,
        })))
    }
}

struct Overlay<'a, 'b, Message> {
    content: &'b mut Element<'a, Message>,
    tree: &'b mut Tree,
    position: Point,
    width: f32,
}

impl<Message> overlay::Overlay<Message, Theme, Renderer> for Overlay<'_, '_, Message> {
    fn layout(&mut self, renderer: &Renderer, _bounds: Size) -> layout::Node {
        self.content
            .as_widget_mut()
            .layout(self.tree, renderer, &content_limits(self.width))
            .move_to(self.position)
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
        self.content.as_widget_mut().update(
            self.tree,
            event,
            layout,
            cursor,
            renderer,
            clipboard,
            shell,
            &layout.bounds(),
        );
    }

    fn draw(
        &self,
        renderer: &mut Renderer,
        theme: &Theme,
        style: &renderer::Style,
        layout: Layout<'_>,
        cursor: mouse::Cursor,
    ) {
        // The focus ring paints 4px outside a control, so the viewport
        // leaves room for it around the content.
        let viewport = layout.bounds().expand(8.0);
        self.content
            .as_widget()
            .draw(self.tree, renderer, theme, style, layout, cursor, &viewport);
    }

    fn mouse_interaction(
        &self,
        layout: Layout<'_>,
        cursor: mouse::Cursor,
        renderer: &Renderer,
    ) -> mouse::Interaction {
        let interaction = self.content.as_widget().mouse_interaction(
            self.tree,
            layout,
            cursor,
            &layout.bounds(),
            renderer,
        );
        // The runtime keeps the cursor from the widgets underneath only where
        // an overlay reports an interaction. Claim the whole content, so a
        // click on its padding never reaches the row it covers.
        if interaction == mouse::Interaction::None && cursor.is_over(layout.bounds()) {
            mouse::Interaction::Idle
        } else {
            interaction
        }
    }

    fn overlay<'c>(
        &'c mut self,
        layout: Layout<'c>,
        renderer: &Renderer,
    ) -> Option<overlay::Element<'c, Message, Theme, Renderer>> {
        let viewport = layout.bounds();
        self.content
            .as_widget_mut()
            .overlay(self.tree, layout, renderer, &viewport, Vector::ZERO)
    }
}

impl<'a, Message: 'a> From<Floating<'a, Message>> for Element<'a, Message> {
    fn from(floating: Floating<'a, Message>) -> Self {
        Self::new(floating)
    }
}
