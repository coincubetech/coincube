//! Tenshu page container (`.pg`): one layout for every Cube screen.
//!
//! The page is anchored to the left: its content starts one gutter from the
//! side menu, and only its right edge moves as the window grows. Reading
//! pages stop at [`PAGE_MAX`]; wide pages (Activity) take the whole width.
//! The rule is written up in `design-review/HANDOFF.md`, "Page layout".
//!
//! A flex row cannot express "gutter, then at most N, then nothing", because
//! it hands out proportional width before a child's `max_width` applies. This
//! widget places its child during layout instead.

use iced::{
    advanced::{
        layout, mouse, overlay, renderer,
        widget::{Operation, Tree},
        Clipboard, Layout, Shell, Widget,
    },
    Event, Length, Rectangle, Size, Vector,
};

use crate::theme::Theme;
use crate::widget::{Element, Renderer};

/// Widest a reading page's content gets, in logical pixels.
pub const PAGE_MAX: f32 = 1120.0;
/// Space above the page title.
pub const PAGE_TOP: f32 = 28.0;
/// Space below the last element.
pub const PAGE_BOTTOM: f32 = 48.0;
/// The gutter is 3% of the window width, kept between these bounds.
pub const GUTTER_MIN: f32 = 24.0;
pub const GUTTER_MAX: f32 = 48.0;
const GUTTER_RATIO: f32 = 0.03;

/// `clamp(24px, 3vw, 48px)`: 24px at the 1024px minimum window, 43px at
/// 1440px, 48px from 1600px.
pub fn gutter_for_window(window_width: f32) -> f32 {
    (window_width * GUTTER_RATIO).clamp(GUTTER_MIN, GUTTER_MAX)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PageWidth {
    /// Content up to [`PAGE_MAX`] wide. Every Cube page except Activity.
    Reading,
    /// No maximum. Activity's table and detail panel use the full width.
    Wide,
}

/// The content's left offset and width inside a pane `pane_width` wide.
pub fn content_frame(pane_width: f32, width: PageWidth, gutter: f32) -> (f32, f32) {
    let available = (pane_width - 2.0 * gutter).max(0.0);
    let content = match width {
        PageWidth::Reading => available.min(PAGE_MAX),
        PageWidth::Wide => available,
    };
    (gutter, content)
}

/// Wraps `content` in the page container. `gutter` maps the width the page is
/// given to its gutter, so a caller inside a shell with a side menu can work
/// out the window width (see `coincube-gui`'s `view::page`).
pub fn page<'a, M: 'a>(
    content: impl Into<Element<'a, M>>,
    width: PageWidth,
    gutter: fn(f32) -> f32,
) -> Page<'a, M> {
    Page {
        content: content.into(),
        width,
        gutter,
        top: PAGE_TOP,
        bottom: PAGE_BOTTOM,
    }
}

pub struct Page<'a, M> {
    content: Element<'a, M>,
    width: PageWidth,
    gutter: fn(f32) -> f32,
    top: f32,
    bottom: f32,
}

impl<M> Page<'_, M> {
    /// Vertical padding; defaults to [`PAGE_TOP`] and [`PAGE_BOTTOM`].
    /// Banners above the page use zero so only their sides follow the gutter.
    pub fn vertical(mut self, top: f32, bottom: f32) -> Self {
        self.top = top;
        self.bottom = bottom;
        self
    }
}

impl<M> Widget<M, Theme, Renderer> for Page<'_, M> {
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
        let pane_width = limits.max().width;
        let (x, width) = content_frame(pane_width, self.width, (self.gutter)(pane_width));
        let child_limits = layout::Limits::new(
            Size::new(width, 0.0),
            Size::new(
                width,
                (limits.max().height - self.top - self.bottom).max(0.0),
            ),
        );
        let child = self
            .content
            .as_widget_mut()
            .layout(&mut tree.children[0], renderer, &child_limits)
            .move_to((x, self.top));
        let height = self.top + child.size().height + self.bottom;
        layout::Node::with_children(
            limits.resolve(Length::Fill, Length::Shrink, Size::new(pane_width, height)),
            vec![child],
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
            self.content.as_widget_mut().operate(
                &mut tree.children[0],
                layout.children().next().unwrap(),
                renderer,
                operation,
            );
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
        self.content.as_widget_mut().update(
            &mut tree.children[0],
            event,
            layout.children().next().unwrap(),
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
        self.content.as_widget().mouse_interaction(
            &tree.children[0],
            layout.children().next().unwrap(),
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
        self.content.as_widget().draw(
            &tree.children[0],
            renderer,
            theme,
            style,
            layout.children().next().unwrap(),
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
        self.content.as_widget_mut().overlay(
            &mut tree.children[0],
            layout.children().next().unwrap(),
            renderer,
            viewport,
            translation,
        )
    }
}

impl<'a, M: 'a> From<Page<'a, M>> for Element<'a, M> {
    fn from(page: Page<'a, M>) -> Self {
        Element::new(page)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn gutter_follows_the_canvas_clamp() {
        assert_eq!(gutter_for_window(1024.0), 30.72);
        assert_eq!(gutter_for_window(1440.0), 43.2);
        assert_eq!(gutter_for_window(1600.0), 48.0);
        assert_eq!(gutter_for_window(2560.0), GUTTER_MAX);
        assert_eq!(gutter_for_window(600.0), GUTTER_MIN);
    }

    #[test]
    fn reading_pages_start_at_the_gutter_and_stop_at_page_max() {
        // Narrow: the content fills between the gutters.
        assert_eq!(
            content_frame(900.0, PageWidth::Reading, 30.0),
            (30.0, 840.0)
        );
        // Wide window: still left-anchored, the spare width goes to the right.
        assert_eq!(
            content_frame(2312.0, PageWidth::Reading, 48.0),
            (48.0, PAGE_MAX)
        );
    }

    #[test]
    fn wide_pages_share_the_gutter_but_have_no_maximum() {
        assert_eq!(content_frame(2312.0, PageWidth::Wide, 48.0), (48.0, 2216.0));
        let (reading_x, _) = content_frame(2312.0, PageWidth::Reading, 48.0);
        let (wide_x, _) = content_frame(2312.0, PageWidth::Wide, 48.0);
        assert_eq!(reading_x, wide_x, "titles line up across page widths");
    }

    #[test]
    fn a_pane_narrower_than_two_gutters_gets_no_negative_width() {
        assert_eq!(content_frame(40.0, PageWidth::Reading, 24.0), (24.0, 0.0));
    }
}
