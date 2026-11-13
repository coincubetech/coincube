use iced::advanced::{
    layout, overlay, renderer,
    widget::{tree, Operation, Tree},
    Clipboard, Layout, Shell, Widget,
};
use iced::widget::scrollable::{self, Direction, Scrollbar};
use iced::{keyboard, mouse, Event, Length, Rectangle, Size, Vector};

use crate::{
    theme::Theme,
    widget::{Element, Renderer},
};

/// A horizontal scroller that lets a surrounding page handle vertical wheel
/// and trackpad gestures. Iced's scroll transaction can capture wheel events
/// even when the inner scroller cannot move along the requested axis.
pub fn horizontal_scroll<'a, M: 'a>(content: impl Into<Element<'a, M>>) -> Element<'a, M> {
    let scrollable = scrollable::Scrollable::new(content)
        .direction(Direction::Horizontal(Scrollbar::new()))
        .width(Length::Fill)
        .height(Length::Shrink)
        .into();
    Element::new(HorizontalScroll { scrollable })
}

struct HorizontalScroll<'a, M> {
    scrollable: Element<'a, M>,
}

impl<M> Widget<M, Theme, Renderer> for HorizontalScroll<'_, M> {
    fn tag(&self) -> tree::Tag {
        tree::Tag::of::<keyboard::Modifiers>()
    }

    fn state(&self) -> tree::State {
        tree::State::new(keyboard::Modifiers::default())
    }

    fn children(&self) -> Vec<Tree> {
        vec![Tree::new(&self.scrollable)]
    }

    fn diff(&self, tree: &mut Tree) {
        tree.diff_children(std::slice::from_ref(&self.scrollable));
    }

    fn size(&self) -> Size<Length> {
        self.scrollable.as_widget().size()
    }

    fn layout(
        &mut self,
        tree: &mut Tree,
        renderer: &Renderer,
        limits: &layout::Limits,
    ) -> layout::Node {
        self.scrollable
            .as_widget_mut()
            .layout(&mut tree.children[0], renderer, limits)
    }

    fn operate(
        &mut self,
        tree: &mut Tree,
        layout: Layout<'_>,
        renderer: &Renderer,
        operation: &mut dyn Operation,
    ) {
        self.scrollable
            .as_widget_mut()
            .operate(&mut tree.children[0], layout, renderer, operation);
    }

    fn update(
        &mut self,
        tree: &mut Tree,
        event: &Event,
        layout: Layout<'_>,
        cursor: mouse::Cursor,
        renderer: &Renderer,
        clipboard: &mut dyn Clipboard,
        shell: &mut Shell<'_, M>,
        viewport: &Rectangle,
    ) {
        let modifiers = tree.state.downcast_mut::<keyboard::Modifiers>();
        match event {
            Event::Keyboard(keyboard::Event::ModifiersChanged(value)) => *modifiers = *value,
            Event::Mouse(mouse::Event::WheelScrolled { delta }) => {
                let (x, y) = match *delta {
                    // Match Iced's Shift+wheel handling. macOS reports the
                    // axes already swapped for line-based wheel events.
                    mouse::ScrollDelta::Lines { x, y }
                        if modifiers.shift() && !cfg!(target_os = "macos") =>
                    {
                        (y, x)
                    }
                    mouse::ScrollDelta::Lines { x, y } | mouse::ScrollDelta::Pixels { x, y } => {
                        (x, y)
                    }
                };
                // A primarily vertical trackpad gesture can include a little
                // horizontal drift. Leave it uncaptured for the page.
                if x.abs() <= y.abs() {
                    return;
                }
            }
            _ => {}
        }
        self.scrollable.as_widget_mut().update(
            &mut tree.children[0],
            event,
            layout,
            cursor,
            renderer,
            clipboard,
            shell,
            viewport,
        );
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
        self.scrollable.as_widget().draw(
            &tree.children[0],
            renderer,
            theme,
            style,
            layout,
            cursor,
            viewport,
        );
    }

    fn mouse_interaction(
        &self,
        tree: &Tree,
        layout: Layout<'_>,
        cursor: mouse::Cursor,
        viewport: &Rectangle,
        renderer: &Renderer,
    ) -> mouse::Interaction {
        self.scrollable.as_widget().mouse_interaction(
            &tree.children[0],
            layout,
            cursor,
            viewport,
            renderer,
        )
    }

    fn overlay<'b>(
        &'b mut self,
        tree: &'b mut Tree,
        layout: Layout<'b>,
        renderer: &Renderer,
        viewport: &Rectangle,
        translation: Vector,
    ) -> Option<overlay::Element<'b, M, Theme, Renderer>> {
        self.scrollable.as_widget_mut().overlay(
            &mut tree.children[0],
            layout,
            renderer,
            viewport,
            translation,
        )
    }
}
