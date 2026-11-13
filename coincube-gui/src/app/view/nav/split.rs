//! Exact-width responsive split for the Cube shell.
//!
//! A flex row reserves its proportional width before applying a child's
//! `max_width`, leaving a blank strip at wide sizes. This widget gives the
//! sidebar 248px or 208px during layout, then gives every remaining pixel to
//! the content pane.

use crate::app::view::Message;
use coincube_ui::{component::tenshu::arranged, widget::Element};
use iced::{
    advanced::{layout, widget::Tree},
    Length, Size,
};

use super::shell;

pub(crate) fn sidebar_width_for_window(width: f32) -> f32 {
    if width <= 1100.0 {
        shell::COMPACT_WIDTH
    } else {
        shell::FULL_WIDTH
    }
    .min(width)
}

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

impl<'a> From<ShellSplit<'a>> for Element<'a, Message> {
    fn from(split: ShellSplit<'a>) -> Self {
        // The sidebar keeps an event it captures from the content pane.
        arranged(
            split.children.into(),
            Size::new(Length::Fill, Length::Fill),
            |children: &mut [Element<'a, Message>],
             trees: &mut [Tree],
             renderer: &iced::Renderer,
             limits: &layout::Limits| {
                let size = limits.resolve(Length::Fill, Length::Fill, limits.max());
                let sidebar_width = sidebar_width_for_window(size.width);
                let content_width = size.width - sidebar_width;
                let sidebar_size = Size::new(sidebar_width, size.height);
                let content_size = Size::new(content_width, size.height);
                let sidebar = children[0].as_widget_mut().layout(
                    &mut trees[0],
                    renderer,
                    &layout::Limits::new(sidebar_size, sidebar_size),
                );
                let content = children[1]
                    .as_widget_mut()
                    .layout(
                        &mut trees[1],
                        renderer,
                        &layout::Limits::new(content_size, content_size),
                    )
                    .move_to((sidebar_width, 0.0));
                layout::Node::with_children(size, vec![sidebar, content])
            },
        )
        .stop_on_capture()
        .into()
    }
}
