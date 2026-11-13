//! A list that bleeds past its column (`margin: 0 -12px`): its rows' own side
//! padding then lands their text on the column's edges, while hover fills
//! and focus rings reach a little beyond them.

use iced::advanced::{
    layout, mouse, overlay, renderer,
    widget::{Operation, Tree},
    Clipboard, Layout, Shell, Widget,
};
use iced::{Event, Length, Rectangle, Size, Vector};

use crate::theme::Theme;
use crate::widget::{Element, Renderer};

/// `content`, `amount` wider than its column on each side. It still takes
/// only the column's width in its parent, so the rest of the page is not
/// moved; the column needs at least `amount` of free space on each side,
/// as a page gutter has.
pub fn bleed<'a, M>(amount: f32, content: impl Into<Element<'a, M>>) -> Bleed<'a, M> {
    Bleed {
        amount,
        content: content.into(),
    }
}

pub struct Bleed<'a, M> {
    amount: f32,
    content: Element<'a, M>,
}

impl<M> Widget<M, Theme, Renderer> for Bleed<'_, M> {
    fn children(&self) -> Vec<Tree> {
        vec![Tree::new(&self.content)]
    }

    fn diff(&self, tree: &mut Tree) {
        tree.diff_children(std::slice::from_ref(&self.content));
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
        let wider = layout::Limits::new(
            Size::ZERO,
            Size::new(width + 2.0 * self.amount, limits.max().height),
        );
        let content = self
            .content
            .as_widget_mut()
            .layout(&mut tree.children[0], renderer, &wider);
        let size = content.size();
        let width = if width.is_finite() {
            width
        } else {
            (size.width - 2.0 * self.amount).max(0.0)
        };
        layout::Node::with_children(
            Size::new(width, size.height),
            vec![content.move_to((-self.amount, 0.0))],
        )
    }

    fn operate(
        &mut self,
        tree: &mut Tree,
        layout: Layout<'_>,
        renderer: &Renderer,
        operation: &mut dyn Operation,
    ) {
        let content = layout.children().next().expect("the content");
        self.content
            .as_widget_mut()
            .operate(&mut tree.children[0], content, renderer, operation);
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
        let content = layout.children().next().expect("the content");
        self.content.as_widget_mut().update(
            &mut tree.children[0],
            event,
            content,
            cursor,
            renderer,
            clipboard,
            shell,
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
        let content = layout.children().next().expect("the content");
        self.content.as_widget().mouse_interaction(
            &tree.children[0],
            content,
            cursor,
            viewport,
            renderer,
        )
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
        let content = layout.children().next().expect("the content");
        self.content.as_widget().draw(
            &tree.children[0],
            renderer,
            theme,
            style,
            content,
            cursor,
            viewport,
        );
    }

    fn overlay<'b>(
        &'b mut self,
        tree: &'b mut Tree,
        layout: Layout<'b>,
        renderer: &Renderer,
        viewport: &Rectangle,
        translation: Vector,
    ) -> Option<overlay::Element<'b, M, Theme, Renderer>> {
        let content = layout.children().next().expect("the content");
        self.content.as_widget_mut().overlay(
            &mut tree.children[0],
            content,
            renderer,
            viewport,
            translation,
        )
    }
}

impl<'a, M: 'a> From<Bleed<'a, M>> for Element<'a, M> {
    fn from(bleed: Bleed<'a, M>) -> Self {
        Element::new(bleed)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::widget::{Column, Container};
    use iced::advanced::renderer::Headless;

    #[test]
    fn content_is_wider_than_the_column_by_the_amount_on_each_side() {
        let renderer = iced::futures::executor::block_on(<Renderer as Headless>::new(
            crate::font::REGULAR,
            14.into(),
            Some("tiny-skia"),
        ))
        .unwrap();
        let mut element: Element<'static, ()> = Container::new(
            Column::new()
                .push(iced::widget::Space::new().height(Length::Fixed(10.0)))
                .push(bleed(
                    12.0,
                    Container::new(iced::widget::Space::new().height(Length::Fixed(40.0)))
                        .width(Length::Fill),
                ))
                .push(iced::widget::Space::new().height(Length::Fixed(10.0))),
        )
        .padding([0, 24])
        .into();
        let mut tree = Tree::new(element.as_widget());
        let node = element.as_widget_mut().layout(
            &mut tree,
            &renderer,
            &layout::Limits::new(Size::ZERO, Size::new(600.0, 400.0)),
        );
        let column = Layout::new(&node).children().next().unwrap();
        let bleed = column.children().nth(1).unwrap();
        assert_eq!(
            bleed.bounds(),
            Rectangle::new((24.0, 10.0).into(), (552.0, 40.0).into())
        );
        let content = bleed.children().next().unwrap().bounds();
        assert_eq!(
            content,
            Rectangle::new((12.0, 10.0).into(), (576.0, 40.0).into())
        );
        let after = column.children().nth(2).unwrap().bounds();
        assert_eq!(after.y, 50.0, "the page below is not moved");
    }
}
