//! Send and Receive body: the form and a context rail across the full page
//! column.
//!
//! The page column is already centred ([`crate::app::view::page`]), so the
//! body has to fill it: a fixed-width form and rail left-pinned inside it
//! leave every spare pixel on the right. The form takes what the rail
//! leaves; below [`STACK_BELOW`] the rail moves under the form, where a
//! wrapping row can't help because a `Fill` child takes a whole line.

use crate::app::view::Message;
use coincube_ui::{component::tenshu::arranged, theme, widget::Element};
use iced::{
    advanced::{layout, widget::Tree},
    widget::container,
    Background, Length, Size,
};

/// The rail's width beside the form.
pub const RAIL_WIDTH: f32 = 352.0;
/// Space between the form and the rail, either way round.
pub const GAP: f32 = 32.0;
/// The narrowest the form gets beside the rail.
pub const FORM_MIN: f32 = 560.0;
/// Below this body width the rail stacks under the form.
pub const STACK_BELOW: f32 = FORM_MIN + GAP + RAIL_WIDTH;

/// Where each column goes in a body `width` wide: the form's width, and the
/// rail's offset and width. `stacked` puts the rail under the form.
pub fn columns(width: f32) -> Columns {
    if width >= STACK_BELOW {
        Columns {
            form_width: width - GAP - RAIL_WIDTH,
            rail_x: width - RAIL_WIDTH,
            rail_width: RAIL_WIDTH,
            stacked: false,
        }
    } else {
        Columns {
            form_width: width,
            rail_x: 0.0,
            rail_width: width,
            stacked: true,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Columns {
    pub form_width: f32,
    pub rail_x: f32,
    pub rail_width: f32,
    pub stacked: bool,
}

/// The rail's surface: the same panel, line and radius as the form's card,
/// so the two columns carry the same weight.
pub fn rail_card<'a>(content: impl Into<Element<'a, Message>>) -> Element<'a, Message> {
    container(content)
        .padding([22, 24])
        .width(Length::Fill)
        .style(rail_card_style)
        .into()
}

pub fn rail_card_style(theme: &theme::Theme) -> container::Style {
    let shell = &theme.colors.shell;
    container::Style {
        background: Some(Background::Color(shell.panel)),
        text_color: Some(shell.ink),
        border: iced::Border {
            color: shell.line,
            width: 1.0,
            radius: 16.0.into(),
        },
        ..Default::default()
    }
}

/// The form, then the rail (also the focus order), placed by [`columns`].
pub fn payment_columns<'a>(
    form: impl Into<Element<'a, Message>>,
    rail: impl Into<Element<'a, Message>>,
) -> Element<'a, Message> {
    arranged(
        vec![form.into(), rail.into()],
        Size::new(Length::Fill, Length::Shrink),
        arrange,
    )
    .into()
}

fn arrange(
    children: &mut [Element<'_, Message>],
    trees: &mut [Tree],
    renderer: &iced::Renderer,
    limits: &layout::Limits,
) -> layout::Node {
    let width = limits.max().width;
    let max_height = limits.max().height;
    let at = |w: f32| layout::Limits::new(Size::new(w, 0.0), Size::new(w, max_height));
    let cols = columns(width);
    let form = children[0]
        .as_widget_mut()
        .layout(&mut trees[0], renderer, &at(cols.form_width));
    let rail_y = if cols.stacked {
        form.size().height + GAP
    } else {
        0.0
    };
    let rail = children[1]
        .as_widget_mut()
        .layout(&mut trees[1], renderer, &at(cols.rail_width))
        .move_to((cols.rail_x, rail_y));
    let height = if cols.stacked {
        rail_y + rail.size().height
    } else {
        form.size().height.max(rail.size().height)
    };
    layout::Node::with_children(Size::new(width, height), vec![form, rail])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_form_takes_what_the_rail_leaves_across_the_full_page() {
        // PAGE_MAX: the body fills the centred page, nothing left over.
        let cols = columns(1120.0);
        assert!(!cols.stacked);
        assert_eq!(cols.form_width, 1120.0 - GAP - RAIL_WIDTH);
        assert_eq!(cols.rail_x + cols.rail_width, 1120.0);
    }

    #[test]
    fn the_rail_ends_at_the_page_edge_and_the_form_starts_at_the_other() {
        use crate::app::view::shared::landing::capture;
        let body = |width: u32| {
            capture::inspect_wide_content(
                "payment-columns",
                payment_columns(
                    container(iced::widget::text("FORM")).width(Length::Fill),
                    container(iced::widget::text("RAIL")).width(Length::Fill),
                ),
                (1440, 900),
                "dark",
                theme::Theme::dark(),
                width,
            )
        };
        let at = |probe: &capture::Inspection, needle: &str| {
            probe
                .text
                .iter()
                .find_map(|(value, bounds)| (value == needle).then_some(*bounds))
                .expect(needle)
        };
        let wide = body(1106);
        let (form, rail) = (at(&wide, "FORM"), at(&wide, "RAIL"));
        assert!(form.x < 1.0, "form at x = {}", form.x);
        assert!(
            (rail.x - (1106.0 - RAIL_WIDTH)).abs() < 1.0,
            "rail at x = {}",
            rail.x
        );
        assert!((form.y - rail.y).abs() < 1.0);

        let narrow = body(768);
        let (form, rail) = (at(&narrow, "FORM"), at(&narrow, "RAIL"));
        assert!(
            rail.x < 1.0 && rail.y > form.y + GAP - 1.0,
            "rail at {:?}",
            rail
        );
    }

    #[test]
    fn the_rail_stacks_under_the_form_before_the_form_gets_too_narrow() {
        let cols = columns(STACK_BELOW - 1.0);
        assert!(cols.stacked);
        assert_eq!(cols.form_width, STACK_BELOW - 1.0);
        assert_eq!(cols.rail_width, STACK_BELOW - 1.0);
        let side = columns(STACK_BELOW);
        assert!(!side.stacked);
        assert_eq!(side.form_width, FORM_MIN);
    }
}
