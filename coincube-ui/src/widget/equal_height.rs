//! A row whose children all end up as tall as the tallest one.
//!
//! iced's flex layout has no "stretch" cross-axis alignment. A child with
//! `Length::Fill` height inside a height-shrinking row is resolved against a
//! cross size that only the *non*-filling children contribute to, so a row
//! where every child fills resolves each of them to zero height — which is how
//! a full page of cards can end up collapsed to nothing.
//!
//! [`EqualHeightRow`] sidesteps that by laying its children out twice: once to
//! measure their intrinsic heights (with the cross axis compressed, so a
//! filling child reports its content height instead of swallowing the
//! viewport), then again with the tallest measurement imposed as a *minimum*
//! height. Every widget honours a minimum — `Limits::resolve` clamps shrinking,
//! fixed and filling lengths alike — so the children come out flush without
//! any of them being asked to fill an unknown height.
//!
//! The available width is split evenly between the children (minus spacing),
//! like a row of `FillPortion(1)` columns.

use iced::advanced::layout::{self, Layout};
use iced::advanced::overlay;
use iced::advanced::renderer;
use iced::advanced::widget::{Operation, Tree, Widget};
use iced::advanced::{Clipboard, Shell};
use iced::mouse;
use iced::{Element, Event, Length, Pixels, Rectangle, Size, Vector};

/// A horizontal row of equally wide children, each as tall as the tallest.
pub struct EqualHeightRow<'a, Message, Theme, Renderer> {
    spacing: f32,
    children: Vec<Element<'a, Message, Theme, Renderer>>,
}

impl<'a, Message, Theme, Renderer> EqualHeightRow<'a, Message, Theme, Renderer> {
    /// Returns an empty [`EqualHeightRow`].
    pub fn new() -> Self {
        Self {
            spacing: 0.0,
            children: Vec::new(),
        }
    }

    /// Sets the spacing between the children.
    pub fn spacing(mut self, amount: impl Into<Pixels>) -> Self {
        self.spacing = amount.into().0;
        self
    }

    /// Adds a child to the [`EqualHeightRow`].
    pub fn push(mut self, child: impl Into<Element<'a, Message, Theme, Renderer>>) -> Self {
        self.children.push(child.into());
        self
    }
}

impl<Message, Theme, Renderer> Default for EqualHeightRow<'_, Message, Theme, Renderer> {
    fn default() -> Self {
        Self::new()
    }
}

impl<Message, Theme, Renderer> Widget<Message, Theme, Renderer>
    for EqualHeightRow<'_, Message, Theme, Renderer>
where
    Renderer: iced::advanced::Renderer,
{
    fn children(&self) -> Vec<Tree> {
        self.children.iter().map(Tree::new).collect()
    }

    fn diff(&self, tree: &mut Tree) {
        tree.diff_children(&self.children);
    }

    fn size(&self) -> Size<Length> {
        // Shrink in the cross axis: the row reports the height it measured, so
        // an ancestor that shrinks around it never resolves it to zero.
        Size {
            width: Length::Fill,
            height: Length::Shrink,
        }
    }

    fn layout(
        &mut self,
        tree: &mut Tree,
        renderer: &Renderer,
        limits: &layout::Limits,
    ) -> layout::Node {
        let limits = limits.width(Length::Fill).height(Length::Shrink);
        let max = limits.max();

        if self.children.is_empty() {
            return layout::Node::new(limits.resolve(Length::Fill, Length::Shrink, Size::ZERO));
        }

        let total_spacing = self.spacing * (self.children.len() - 1) as f32;

        // Even columns, exactly like a row of `FillPortion(1)` children. An
        // unbounded width (no sensible column to hand out) falls back to
        // letting every child take its intrinsic width.
        let (min_width, max_width) = if max.width.is_finite() {
            let column = ((max.width - total_spacing) / self.children.len() as f32).max(0.0);
            (column, column)
        } else {
            (0.0, f32::INFINITY)
        };

        // Compressing the vertical axis makes a `Length::Fill` child report its
        // content height here rather than the whole available height.
        let measure_limits = layout::Limits::with_compression(
            Size::new(min_width, 0.0),
            Size::new(max_width, max.height),
            Size::new(false, true),
        );

        let tallest = self
            .children
            .iter_mut()
            .zip(tree.children.iter_mut())
            .map(|(child, tree)| {
                child
                    .as_widget_mut()
                    .layout(tree, renderer, &measure_limits)
                    .size()
                    .height
            })
            .fold(0.0f32, f32::max)
            .min(max.height);

        let child_limits = layout::Limits::with_compression(
            Size::new(min_width, tallest),
            Size::new(max_width, max.height.max(tallest)),
            Size::new(false, true),
        );

        let mut x = 0.0;
        let mut height = 0.0f32;

        let nodes = self
            .children
            .iter_mut()
            .zip(tree.children.iter_mut())
            .map(|(child, tree)| {
                let node = child
                    .as_widget_mut()
                    .layout(tree, renderer, &child_limits)
                    .move_to((x, 0.0));

                let size = node.size();
                x += size.width + self.spacing;
                height = height.max(size.height);

                node
            })
            .collect();

        let width = (x - self.spacing).max(0.0);

        layout::Node::with_children(
            limits.resolve(Length::Fill, Length::Shrink, Size::new(width, height)),
            nodes,
        )
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
            self.children
                .iter_mut()
                .zip(&mut tree.children)
                .zip(layout.children())
                .for_each(|((child, state), layout)| {
                    child
                        .as_widget_mut()
                        .operate(state, layout, renderer, operation);
                });
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
        for ((child, tree), layout) in self
            .children
            .iter_mut()
            .zip(&mut tree.children)
            .zip(layout.children())
        {
            child.as_widget_mut().update(
                tree, event, layout, cursor, renderer, clipboard, shell, viewport,
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
            .map(|((child, tree), layout)| {
                child
                    .as_widget()
                    .mouse_interaction(tree, layout, cursor, viewport, renderer)
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
        for ((child, tree), layout) in self
            .children
            .iter()
            .zip(&tree.children)
            .zip(layout.children())
            .filter(|(_, layout)| layout.bounds().intersects(viewport))
        {
            child
                .as_widget()
                .draw(tree, renderer, theme, style, layout, cursor, viewport);
        }
    }

    fn overlay<'b>(
        &'b mut self,
        tree: &'b mut Tree,
        layout: Layout<'b>,
        renderer: &Renderer,
        viewport: &Rectangle,
        translation: Vector,
    ) -> Option<overlay::Element<'b, Message, Theme, Renderer>> {
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

impl<'a, Message, Theme, Renderer> From<EqualHeightRow<'a, Message, Theme, Renderer>>
    for Element<'a, Message, Theme, Renderer>
where
    Message: 'a,
    Theme: 'a,
    Renderer: iced::advanced::Renderer + 'a,
{
    fn from(row: EqualHeightRow<'a, Message, Theme, Renderer>) -> Self {
        Self::new(row)
    }
}

// The null `Renderer`/`text::Renderer` impls for `()` that these tests lay out
// against are only compiled by iced under `debug_assertions`.
#[cfg(all(test, debug_assertions))]
mod tests {
    use super::EqualHeightRow;
    use iced::advanced::widget::Tree;
    use iced::widget::{Column, Space};
    use iced::{Element, Length, Size};

    const SPACING: f32 = 12.0;
    const ROW_HEIGHT: f32 = 10.0;

    type TestElement<'a> = Element<'a, (), (), ()>;

    /// A stand-in for a plan card: a column of `rows` fixed-height bullets,
    /// either shrinking to its content or asking to fill its parent.
    fn card<'a>(rows: usize, fill: bool) -> TestElement<'a> {
        let column = (0..rows).fold(Column::new(), |column, _| {
            column.push(Space::new().height(Length::Fixed(ROW_HEIGHT)))
        });

        if fill {
            column.height(Length::Fill).into()
        } else {
            column.into()
        }
    }

    /// Lays out an [`EqualHeightRow`] of `cards` and returns
    /// `(row size, child sizes)`.
    fn layout(cards: Vec<TestElement<'_>>, max: Size) -> (Size, Vec<Size>) {
        let row = cards
            .into_iter()
            .fold(EqualHeightRow::new().spacing(SPACING), |row, card| {
                row.push(card)
            });

        let mut element: TestElement = row.into();
        let mut tree = Tree::new(&element);

        let node = element.as_widget_mut().layout(
            &mut tree,
            &(),
            &iced::advanced::layout::Limits::new(Size::ZERO, max),
        );

        let children = node.children().iter().map(|child| child.size()).collect();

        (node.size(), children)
    }

    #[test]
    fn shorter_cards_grow_to_the_tallest() {
        let (row, cards) = layout(
            vec![card(4, false), card(4, false), card(6, false)],
            // An unbounded height is what a scrollable hands its content.
            Size::new(600.0, f32::INFINITY),
        );

        let tallest = 6.0 * ROW_HEIGHT;
        assert_eq!(row.height, tallest);
        for card in &cards {
            assert_eq!(card.height, tallest);
        }
    }

    #[test]
    fn filling_cards_do_not_collapse_or_swallow_the_viewport() {
        // The regression this widget exists for: in a plain `Row` that shrinks
        // to its content, children that all fill vertically resolve to zero.
        let (row, cards) = layout(
            vec![card(4, true), card(4, true), card(6, true)],
            Size::new(600.0, f32::INFINITY),
        );

        let tallest = 6.0 * ROW_HEIGHT;
        assert_eq!(row.height, tallest);
        for card in &cards {
            assert_eq!(card.height, tallest);
        }
    }

    #[test]
    fn a_bounded_height_caps_the_row() {
        let (row, cards) = layout(
            vec![card(4, false), card(20, false)],
            Size::new(600.0, 100.0),
        );

        assert_eq!(row.height, 100.0);
        for card in &cards {
            assert_eq!(card.height, 100.0);
        }
    }

    #[test]
    fn the_available_width_is_split_evenly() {
        let (row, cards) = layout(
            vec![card(1, false), card(2, false), card(3, false)],
            Size::new(600.0, f32::INFINITY),
        );

        let column_width = (600.0 - 2.0 * SPACING) / 3.0;
        assert_eq!(row.width, 600.0);
        for card in &cards {
            assert_eq!(card.width, column_width);
        }
    }

    #[test]
    fn a_single_card_spans_the_row() {
        let (row, cards) = layout(vec![card(3, false)], Size::new(600.0, f32::INFINITY));

        assert_eq!(row.width, 600.0);
        assert_eq!(cards.len(), 1);
        assert_eq!(cards[0].width, 600.0);
        assert_eq!(cards[0].height, 3.0 * ROW_HEIGHT);
    }

    #[test]
    fn an_empty_row_has_no_height() {
        let (row, cards) = layout(Vec::new(), Size::new(600.0, f32::INFINITY));

        assert_eq!(row.height, 0.0);
        assert!(cards.is_empty());
    }
}
