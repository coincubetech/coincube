//! Exact-width responsive split for the Cube shell.
//!
//! A flex row reserves its proportional width before applying a child's
//! `max_width`, leaving a blank strip at wide sizes. This widget gives the
//! sidebar 248px or 208px during layout, then gives every remaining pixel to
//! the content pane.

use crate::app::view::Message;
use coincube_ui::{theme, widget::Element};
use iced::{
    advanced::{
        layout, mouse, overlay, renderer,
        widget::{Operation, Tree},
        Clipboard, Layout, Shell, Widget,
    },
    Event, Length, Rectangle, Size, Vector,
};

use super::shell;

pub struct ShellSplit<'a> {
    children: [Element<'a, Message>; 2],
}

impl<'a> ShellSplit<'a> {
    pub fn new(sidebar: Element<'a, Message>, content: Element<'a, Message>) -> Self {
        Self {
            children: [sidebar, content],
        }
    }
}

impl Widget<Message, theme::Theme, iced::Renderer> for ShellSplit<'_> {
    fn children(&self) -> Vec<Tree> {
        self.children.iter().map(Tree::new).collect()
    }

    fn diff(&self, tree: &mut Tree) {
        tree.diff_children(&self.children);
    }

    fn size(&self) -> Size<Length> {
        Size::new(Length::Fill, Length::Fill)
    }

    fn layout(
        &mut self,
        tree: &mut Tree,
        renderer: &iced::Renderer,
        limits: &layout::Limits,
    ) -> layout::Node {
        let size = limits.resolve(Length::Fill, Length::Fill, limits.max());
        let sidebar_width = if size.width <= 1100.0 {
            shell::COMPACT_WIDTH
        } else {
            shell::FULL_WIDTH
        }
        .min(size.width);
        let content_width = size.width - sidebar_width;
        let sidebar_size = Size::new(sidebar_width, size.height);
        let content_size = Size::new(content_width, size.height);
        let sidebar = self.children[0].as_widget_mut().layout(
            &mut tree.children[0],
            renderer,
            &layout::Limits::new(sidebar_size, sidebar_size),
        );
        let content = self.children[1]
            .as_widget_mut()
            .layout(
                &mut tree.children[1],
                renderer,
                &layout::Limits::new(content_size, content_size),
            )
            .move_to((sidebar_width, 0.0));
        layout::Node::with_children(size, vec![sidebar, content])
    }

    fn operate(
        &mut self,
        tree: &mut Tree,
        layout: Layout<'_>,
        renderer: &iced::Renderer,
        operation: &mut dyn Operation,
    ) {
        operation.container(None, layout.bounds());
        operation.traverse(&mut |operation| {
            for ((child, state), layout) in self
                .children
                .iter_mut()
                .zip(&mut tree.children)
                .zip(layout.children())
            {
                child
                    .as_widget_mut()
                    .operate(state, layout, renderer, operation);
            }
        });
    }

    fn update(
        &mut self,
        tree: &mut Tree,
        event: &Event,
        layout: Layout<'_>,
        cursor: mouse::Cursor,
        renderer: &iced::Renderer,
        clipboard: &mut dyn Clipboard,
        shell: &mut Shell<'_, Message>,
        viewport: &Rectangle,
    ) {
        for ((child, state), layout) in self
            .children
            .iter_mut()
            .zip(&mut tree.children)
            .zip(layout.children())
        {
            child.as_widget_mut().update(
                state, event, layout, cursor, renderer, clipboard, shell, viewport,
            );
            if shell.is_event_captured() {
                break;
            }
        }
    }

    fn mouse_interaction(
        &self,
        tree: &Tree,
        layout: Layout<'_>,
        cursor: mouse::Cursor,
        viewport: &Rectangle,
        renderer: &iced::Renderer,
    ) -> mouse::Interaction {
        self.children
            .iter()
            .zip(&tree.children)
            .zip(layout.children())
            .map(|((child, state), layout)| {
                child
                    .as_widget()
                    .mouse_interaction(state, layout, cursor, viewport, renderer)
            })
            .find(|interaction| *interaction != mouse::Interaction::None)
            .unwrap_or_default()
    }

    fn draw(
        &self,
        tree: &Tree,
        renderer: &mut iced::Renderer,
        theme: &theme::Theme,
        style: &renderer::Style,
        layout: Layout<'_>,
        cursor: mouse::Cursor,
        viewport: &Rectangle,
    ) {
        if layout.bounds().intersection(viewport).is_none() {
            return;
        }
        for ((child, state), layout) in self
            .children
            .iter()
            .zip(&tree.children)
            .zip(layout.children())
        {
            child
                .as_widget()
                .draw(state, renderer, theme, style, layout, cursor, viewport);
        }
    }

    fn overlay<'b>(
        &'b mut self,
        tree: &'b mut Tree,
        layout: Layout<'b>,
        renderer: &iced::Renderer,
        viewport: &Rectangle,
        translation: Vector,
    ) -> Option<overlay::Element<'b, Message, theme::Theme, iced::Renderer>> {
        overlay::from_children(
            &mut self.children,
            tree,
            layout,
            renderer,
            viewport,
            translation,
        )
    }
}

impl<'a> From<ShellSplit<'a>> for Element<'a, Message> {
    fn from(split: ShellSplit<'a>) -> Self {
        Element::new(split)
    }
}
