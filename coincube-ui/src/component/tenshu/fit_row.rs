//! A row whose trailing group drops below when the two don't fit: an asset
//! row's amounts on the left and its actions on the right while they fit side
//! by side, else the actions on their own line, still at the right. Neither
//! group is ever squeezed, so a long balance can't push the actions out of a
//! half-width card.

use iced::advanced::{layout, widget::Tree};
use iced::{Length, Size};

use super::arranged::arranged;
use crate::widget::{Element, Renderer};

/// `lead` at the left and `trail` at the right, side by side while both fit
/// at their own widths with [`FitRow::gap`] between them, else `trail` below.
pub fn fit_row<'a, M>(
    lead: impl Into<Element<'a, M>>,
    trail: impl Into<Element<'a, M>>,
) -> FitRow<'a, M> {
    FitRow {
        gap: 8.0,
        children: vec![lead.into(), trail.into()],
    }
}

pub struct FitRow<'a, M> {
    gap: f32,
    /// The lead, then the trail.
    children: Vec<Element<'a, M>>,
}

impl<M> FitRow<'_, M> {
    /// The space between the groups, across when side by side and down when
    /// stacked.
    pub fn gap(mut self, gap: f32) -> Self {
        self.gap = gap;
        self
    }
}

impl<'a, M: 'a> From<FitRow<'a, M>> for Element<'a, M> {
    fn from(row: FitRow<'a, M>) -> Self {
        let gap = row.gap;
        arranged(
            row.children,
            Size::new(Length::Fill, Length::Shrink),
            move |children: &mut [Element<'a, M>],
                  trees: &mut [Tree],
                  renderer: &Renderer,
                  limits: &layout::Limits| {
                let max = limits.max();
                let own = layout::Limits::new(Size::ZERO, max);
                let mut nodes: Vec<layout::Node> = children
                    .iter_mut()
                    .zip(trees.iter_mut())
                    .map(|(child, tree)| child.as_widget_mut().layout(tree, renderer, &own))
                    .collect();
                let (lead, trail) = (nodes[0].size(), nodes[1].size());
                let beside = lead.width + gap + trail.width;
                let width = if max.width.is_finite() {
                    max.width
                } else {
                    beside
                };
                let height = if beside <= width {
                    let height = lead.height.max(trail.height);
                    nodes[0] = nodes[0]
                        .clone()
                        .move_to((0.0, (height - lead.height) / 2.0));
                    nodes[1] = nodes[1]
                        .clone()
                        .move_to((width - trail.width, (height - trail.height) / 2.0));
                    height
                } else {
                    nodes[1] = nodes[1]
                        .clone()
                        .move_to((width - trail.width, lead.height + gap));
                    lead.height + gap + trail.height
                };
                layout::Node::with_children(Size::new(width, height), nodes)
            },
        )
        .into()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use iced::advanced::renderer::Headless;
    use iced::advanced::Layout;
    use iced::widget::Space;
    use iced::Rectangle;

    /// The two groups' bounds in a row `width` wide.
    fn lay_out(lead: f32, trail: f32, width: f32) -> (Rectangle, Rectangle, Size) {
        let renderer = iced::futures::executor::block_on(<Renderer as Headless>::new(
            crate::font::REGULAR,
            14.into(),
            Some("tiny-skia"),
        ))
        .unwrap();
        let mut element: Element<'static, ()> = fit_row(
            Space::new()
                .width(Length::Fixed(lead))
                .height(Length::Fixed(32.0)),
            Space::new()
                .width(Length::Fixed(trail))
                .height(Length::Fixed(40.0)),
        )
        .into();
        let mut tree = Tree::new(element.as_widget());
        let node = element.as_widget_mut().layout(
            &mut tree,
            &renderer,
            &layout::Limits::new(Size::ZERO, Size::new(width, 400.0)),
        );
        let mut children = Layout::new(&node).children();
        (
            children.next().unwrap().bounds(),
            children.next().unwrap().bounds(),
            node.size(),
        )
    }

    #[test]
    fn groups_that_fit_share_a_line_with_the_trail_at_the_right() {
        let (lead, trail, size) = lay_out(200.0, 188.0, 432.0);
        assert_eq!(
            lead,
            Rectangle::new((0.0, 4.0).into(), (200.0, 32.0).into())
        );
        assert_eq!(
            trail,
            Rectangle::new((244.0, 0.0).into(), (188.0, 40.0).into())
        );
        assert_eq!(size, Size::new(432.0, 40.0));
        // Exactly the gap to spare still fits.
        let (_, trail, _) = lay_out(236.0, 188.0, 432.0);
        assert_eq!(trail.y, 0.0);
    }

    #[test]
    fn a_trail_that_does_not_fit_drops_below_at_full_width() {
        let (lead, trail, size) = lay_out(237.0, 188.0, 432.0);
        assert_eq!(
            lead,
            Rectangle::new((0.0, 0.0).into(), (237.0, 32.0).into())
        );
        assert_eq!(
            trail,
            Rectangle::new((244.0, 40.0).into(), (188.0, 40.0).into())
        );
        assert_eq!(size, Size::new(432.0, 80.0));
    }
}
