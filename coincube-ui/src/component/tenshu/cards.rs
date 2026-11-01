//! Tenshu wallet cards (`.cards`): two equal columns 16px apart, each row as
//! tall as its taller card. A column narrower than two cards stacks them.
//!
//! The breakpoint is the width the cards are given, not the window's: the
//! same cards stack in a narrow pane however wide the window is.

use iced::advanced::{
    layout, mouse, overlay, renderer,
    widget::{Operation, Tree},
    Clipboard, Layout, Shell, Widget,
};
use iced::{Event, Length, Rectangle, Size, Vector};

use crate::theme::Theme;
use crate::widget::{Element, Renderer};

/// The narrowest a card may be beside another: two of them fill the 960px
/// reading column the boards are drawn at.
pub const CARD_MIN_WIDTH: f32 = 472.0;
/// `.cards { gap: 16px }`, between columns and between rows.
pub const CARD_GAP: f32 = 16.0;

/// Whether cards `width` wide sit two to a row.
pub fn two_columns(width: f32) -> bool {
    width >= 2.0 * CARD_MIN_WIDTH + CARD_GAP
}

/// `cards` in reading order, two to a row while they fit.
pub fn cards<'a, M>(cards: impl IntoIterator<Item = Element<'a, M>>) -> Cards<'a, M> {
    Cards {
        children: cards.into_iter().collect(),
    }
}

pub struct Cards<'a, M> {
    children: Vec<Element<'a, M>>,
}

impl<M> Widget<M, Theme, Renderer> for Cards<'_, M> {
    fn children(&self) -> Vec<Tree> {
        self.children.iter().map(Tree::new).collect()
    }

    fn diff(&self, tree: &mut Tree) {
        tree.diff_children(&self.children);
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
        let width = if width.is_finite() {
            width
        } else {
            2.0 * CARD_MIN_WIDTH + CARD_GAP
        };
        let per_row = if two_columns(width) { 2 } else { 1 };
        let column = (width - CARD_GAP * (per_row - 1) as f32) / per_row as f32;
        let mut nodes = Vec::with_capacity(self.children.len());
        let mut top = 0.0;
        for (row, trees) in self
            .children
            .chunks_mut(per_row)
            .zip(tree.children.chunks_mut(per_row))
        {
            // The cards' own heights, then each laid out again at the row's
            // height so the shorter card's surface reaches the row's bottom.
            let height = row
                .iter_mut()
                .zip(trees.iter_mut())
                .map(|(card, tree)| {
                    card.as_widget_mut()
                        .layout(
                            tree,
                            renderer,
                            &layout::Limits::new(
                                Size::new(column, 0.0),
                                Size::new(column, f32::INFINITY),
                            ),
                        )
                        .size()
                        .height
                })
                .fold(0.0, f32::max);
            for (index, (card, tree)) in row.iter_mut().zip(trees.iter_mut()).enumerate() {
                let node = card.as_widget_mut().layout(
                    tree,
                    renderer,
                    &layout::Limits::new(Size::new(column, height), Size::new(column, height)),
                );
                nodes.push(node.move_to((index as f32 * (column + CARD_GAP), top)));
            }
            top += height + CARD_GAP;
        }
        let height = (top - CARD_GAP).max(0.0);
        layout::Node::with_children(Size::new(width, height), nodes)
    }

    fn operate(
        &mut self,
        tree: &mut Tree,
        layout: Layout<'_>,
        renderer: &Renderer,
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
        renderer: &Renderer,
        clipboard: &mut dyn Clipboard,
        shell: &mut Shell<'_, M>,
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
        self.children
            .iter()
            .zip(&tree.children)
            .zip(layout.children())
            .map(|((child, state), layout)| {
                child
                    .as_widget()
                    .mouse_interaction(state, layout, cursor, viewport, renderer)
            })
            .max()
            .unwrap_or_default()
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
        renderer: &Renderer,
        viewport: &Rectangle,
        translation: Vector,
    ) -> Option<overlay::Element<'b, M, Theme, Renderer>> {
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

impl<'a, M: 'a> From<Cards<'a, M>> for Element<'a, M> {
    fn from(cards: Cards<'a, M>) -> Self {
        Element::new(cards)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::widget::{Column, Container};
    use iced::advanced::renderer::Headless;

    fn renderer() -> Renderer {
        iced::futures::executor::block_on(<Renderer as Headless>::new(
            crate::font::REGULAR,
            14.into(),
            Some("tiny-skia"),
        ))
        .unwrap()
    }

    /// A card `height` tall that fills the width it is given.
    fn card(height: f32) -> Element<'static, ()> {
        Container::new(iced::widget::Space::new().height(Length::Fixed(height)))
            .width(Length::Fill)
            .into()
    }

    /// The cards laid out `width` wide: each card's bounds, and the total size.
    fn lay_out(heights: &[f32], width: f32) -> (Vec<Rectangle>, Size) {
        let renderer = renderer();
        let mut element: Element<'static, ()> =
            cards(heights.iter().map(|height| card(*height))).into();
        let mut tree = Tree::new(element.as_widget());
        let node = element.as_widget_mut().layout(
            &mut tree,
            &renderer,
            &layout::Limits::new(Size::ZERO, Size::new(width, 2000.0)),
        );
        let bounds = Layout::new(&node)
            .children()
            .map(|card| card.bounds())
            .collect();
        (bounds, node.size())
    }

    #[test]
    fn two_cards_fill_two_equal_columns_and_share_the_taller_height() {
        let (cards, size) = lay_out(&[120.0, 180.0], 1106.0);
        let column = (1106.0 - CARD_GAP) / 2.0;
        assert_eq!(
            cards[0],
            Rectangle::new((0.0, 0.0).into(), (column, 180.0).into())
        );
        assert_eq!(
            cards[1],
            Rectangle::new((column + CARD_GAP, 0.0).into(), (column, 180.0).into())
        );
        assert_eq!(size, Size::new(1106.0, 180.0));
    }

    #[test]
    fn a_column_narrower_than_two_cards_stacks_them_full_width() {
        let width = 2.0 * CARD_MIN_WIDTH + CARD_GAP - 1.0;
        let (cards, size) = lay_out(&[120.0, 180.0], width);
        assert_eq!(
            cards[0],
            Rectangle::new((0.0, 0.0).into(), (width, 120.0).into())
        );
        assert_eq!(
            cards[1],
            Rectangle::new((0.0, 120.0 + CARD_GAP).into(), (width, 180.0).into())
        );
        assert_eq!(size.height, 120.0 + CARD_GAP + 180.0);
        assert!(two_columns(width + 1.0), "the minimum itself fits two");
    }

    #[test]
    fn an_odd_card_starts_the_next_row_in_the_first_column() {
        let (cards, size) = lay_out(&[100.0, 140.0, 60.0], 1000.0);
        let column = (1000.0 - CARD_GAP) / 2.0;
        assert_eq!(cards[1].height, 140.0);
        assert_eq!(
            cards[2],
            Rectangle::new((0.0, 140.0 + CARD_GAP).into(), (column, 60.0).into())
        );
        assert_eq!(size.height, 140.0 + CARD_GAP + 60.0);
    }

    #[test]
    fn the_breakpoint_follows_the_width_given_not_a_window() {
        // The same cards inside a narrow pane stack, beside it they don't.
        let wide: Element<'static, ()> = Column::new()
            .push(cards([card(10.0), card(10.0)]))
            .width(Length::Fixed(600.0))
            .into();
        let renderer = renderer();
        let mut element = wide;
        let mut tree = Tree::new(element.as_widget());
        let node = element.as_widget_mut().layout(
            &mut tree,
            &renderer,
            &layout::Limits::new(Size::ZERO, Size::new(1440.0, 900.0)),
        );
        let grid = Layout::new(&node).children().next().unwrap();
        let second = grid.children().nth(1).unwrap().bounds();
        assert_eq!((second.x, second.width), (0.0, 600.0));
        assert!(lay_out(&[], 1106.0).0.is_empty());
    }
}
