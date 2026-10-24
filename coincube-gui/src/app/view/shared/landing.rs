//! Pure presentation pieces shared by the four wallet landing routes.
//!
//! Callers decide visibility, units, rates, and availability before constructing
//! an `AmountCell`. This module never reads preferences or interprets token units.

use coincube_ui::{
    component::text::{text, Text as _, H2_SIZE, P1_SIZE, P2_SIZE},
    font::MEDIUM,
    theme,
    widget::{action::Action, Column, ColumnExt, Element, Row, RowExt, Text},
};
use iced::{
    advanced::widget::Id,
    widget::{button, container, text::Wrapping},
    Alignment, Length,
};

pub fn landing_control_id(cube: &str, wallet: &str, control: &str) -> Id {
    Id::from(format!(
        "tenshu.overview.{}:{}:{}:{}:{}:{}",
        cube.len(),
        cube,
        wallet.len(),
        wallet,
        control.len(),
        control,
    ))
}

/// Neutral wallet card used on the hub; the orange accent belongs to actions.
pub fn landing_card_style(theme: &theme::Theme) -> container::Style {
    container::Style {
        background: Some(iced::Background::Color(theme.colors.shell.panel)),
        border: iced::Border {
            color: theme.colors.shell.line,
            width: 1.0,
            radius: 16.0.into(),
        },
        ..Default::default()
    }
}

/// Quiet rectangular control for secondary wallet-page navigation.
pub fn landing_subtle_button_style(theme: &theme::Theme, status: button::Status) -> button::Style {
    button::Style {
        background: Some(iced::Background::Color(
            if matches!(status, button::Status::Hovered | button::Status::Pressed) {
                theme.colors.shell.hover
            } else {
                theme.colors.shell.background
            },
        )),
        text_color: theme.colors.shell.ink,
        border: iced::Border {
            color: theme.colors.shell.line,
            width: 1.0,
            radius: 8.0.into(),
        },
        ..Default::default()
    }
}

/// Wallet-page balance is a section of the page, not a second hub card.
pub fn landing_balance_section<'a, M: 'a>(content: impl Into<Element<'a, M>>) -> Element<'a, M> {
    Column::new()
        .spacing(20)
        .width(Length::Fill)
        .push(content)
        .push(iced::widget::rule::horizontal(1))
        .into()
}

/// A landing navigation button using the released keyboard Action behavior.
/// `focus_intent` must route to a whole-tree focus operation in the owner.
pub fn landing_button<'a, M: Clone + 'static>(
    id: impl Into<Id>,
    label: &'static str,
    icon: Option<Text<'a>>,
    focus_intent: fn(Id) -> M,
    press: Option<M>,
    style: fn(&theme::Theme, button::Status) -> button::Style,
) -> Action<'a, M> {
    let label = text(label)
        .font(MEDIUM)
        .align_y(Alignment::Center)
        .align_x(Alignment::Center);
    let content: Element<'a, M> = if let Some(icon) = icon {
        Row::new()
            .spacing(10)
            .align_y(Alignment::Center)
            .push(icon)
            .push(label)
            .into()
    } else {
        label.into()
    };
    Action::new(
        id,
        container(content)
            .align_y(iced::alignment::Vertical::Center)
            .align_x(iced::alignment::Horizontal::Center)
            .width(Length::Fill)
            .padding(5),
        focus_intent,
    )
    .style(style)
    .width(Length::Shrink)
    .on_press_maybe(press)
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AmountCell {
    pub primary: String,
    pub secondary: Option<String>,
    /// Nonmonetary status only. A masked cell discards this so callers cannot
    /// accidentally retain an amount-bearing tooltip or accessibility label.
    pub explanation: Option<String>,
}

impl AmountCell {
    pub fn shown(primary: impl Into<String>, secondary: Option<String>) -> Self {
        Self {
            primary: primary.into(),
            secondary,
            explanation: None,
        }
    }

    pub fn unknown(explanation: impl Into<String>) -> Self {
        Self {
            primary: "—".into(),
            secondary: None,
            explanation: Some(explanation.into()),
        }
    }

    pub fn unavailable(explanation: impl Into<String>) -> Self {
        Self {
            primary: "Unavailable".into(),
            secondary: None,
            explanation: Some(explanation.into()),
        }
    }

    pub fn masked() -> Self {
        Self {
            primary: "••••••".into(),
            secondary: None,
            explanation: None,
        }
    }

    pub fn with_explanation(mut self, explanation: impl Into<String>) -> Self {
        self.explanation = Some(explanation.into());
        self
    }

    /// Erases every supplied monetary string, including explanatory text.
    pub fn redact(self) -> Self {
        Self::masked()
    }
}

#[derive(Clone, Copy)]
pub enum AmountTier {
    Total,
    Card,
    Row,
}

pub fn amount_cell<'a, M: 'a>(cell: AmountCell, tier: AmountTier) -> Element<'a, M> {
    let size = match tier {
        AmountTier::Total => H2_SIZE + 8,
        AmountTier::Card => H2_SIZE,
        AmountTier::Row => P1_SIZE,
    };
    Column::new()
        .align_x(if matches!(tier, AmountTier::Row) {
            Alignment::End
        } else {
            Alignment::Start
        })
        .spacing(3)
        .push(
            text(cell.primary)
                .size(size)
                .bold()
                .wrapping(Wrapping::WordOrGlyph),
        )
        .push_maybe(cell.secondary.map(|s| {
            text(s)
                .size(P2_SIZE)
                .style(theme::text::secondary)
                .wrapping(Wrapping::WordOrGlyph)
        }))
        .push_maybe(cell.explanation.map(|s| {
            text(s)
                .size(P2_SIZE)
                .style(theme::text::secondary)
                .wrapping(Wrapping::WordOrGlyph)
        }))
        .into()
}

pub fn section_heading<'a, M: 'a>(
    title: &'static str,
    action: Option<Element<'a, M>>,
) -> Row<'a, M> {
    Row::new()
        .spacing(12)
        .align_y(Alignment::Center)
        .width(Length::Fill)
        .push(text(title).size(H2_SIZE).bold())
        .push(iced::widget::Space::new().width(Length::Fill))
        .push_maybe(action)
}

#[cfg(test)]
pub(crate) mod capture {
    use super::*;
    use coincube_ui::font;
    use iced::advanced::{
        layout,
        renderer::Headless,
        widget::{operation, operation::focusable, operation::Focusable, Operation, Tree},
        Layout,
    };
    use iced::{keyboard, mouse, Event, Rectangle, Size};

    #[derive(Default)]
    struct TextProbe {
        entries: Vec<(String, Rectangle)>,
        focus: Vec<(Id, Rectangle)>,
    }

    impl Operation for TextProbe {
        fn traverse(&mut self, operate: &mut dyn FnMut(&mut dyn Operation)) {
            operate(self);
        }

        fn text(&mut self, _id: Option<&Id>, bounds: Rectangle, value: &str) {
            self.entries.push((value.to_owned(), bounds));
        }

        fn focusable(&mut self, id: Option<&Id>, bounds: Rectangle, _: &mut dyn Focusable) {
            if let Some(id) = id {
                self.focus.push((id.clone(), bounds));
            }
        }
    }

    pub struct Inspection {
        pub text: Vec<(String, Rectangle)>,
        pub focus: Vec<(Id, Rectangle)>,
    }

    impl Inspection {
        pub fn contains(&self, needle: &str) -> bool {
            self.text.iter().any(|(value, _)| value.contains(needle))
        }

        pub fn has_focus(&self, id: Id) -> bool {
            self.focus.iter().any(|(actual, _)| actual == &id)
        }

        pub fn right_aligned(&self, primary: &str, secondary: &str) -> bool {
            let right = |needle: &str| {
                self.text.iter().find_map(|(value, bounds)| {
                    (value == needle).then_some(bounds.x + bounds.width)
                })
            };
            match (right(primary), right(secondary)) {
                (Some(a), Some(b)) => (a - b).abs() <= 4.0,
                _ => false,
            }
        }
    }

    /// Draws the actual production presenter at the dashboard's measured
    /// 8/10 content width. No App/Gui, live wallet, session or OS window exists.
    /// Screenshots are optional and always labeled content-only synthetic.
    pub fn inspect<M: 'static>(
        scenario: &str,
        content: Element<'_, M>,
        outer: (u32, u32),
        appearance: &str,
        theme: theme::Theme,
    ) -> Inspection {
        for bytes in font::load() {
            iced::advanced::graphics::text::font_system()
                .write()
                .unwrap()
                .load_font(bytes);
        }
        let sidebar = if outer.0 <= 1100 { 208.0 } else { 248.0 };
        let width = (((outer.0 as f32 - sidebar) * 0.8).floor() as u32).max(1);
        let height = outer.1.saturating_sub(100).max(1);
        let size = Size::new(width as f32, height as f32);
        let mut renderer = iced::futures::executor::block_on(<iced::Renderer as Headless>::new(
            font::REGULAR,
            iced::Pixels(16.0),
            Some("tiny-skia"),
        ))
        .expect("software renderer for a synthetic R06a presenter");
        let mut element: Element<'_, M> = iced::widget::scrollable(content)
            .width(Length::Fill)
            .height(Length::Fill)
            .into();
        let mut tree = Tree::new(element.as_widget());
        let node = element.as_widget_mut().layout(
            &mut tree,
            &renderer,
            &layout::Limits::new(Size::ZERO, size),
        );
        assert!(
            node.size().width <= size.width + 1.0,
            "{}: root width overflow",
            scenario
        );
        let mut probe = TextProbe::default();
        element
            .as_widget_mut()
            .operate(&mut tree, Layout::new(&node), &renderer, &mut probe);
        for (value, bounds) in &probe.entries {
            assert!(
                bounds.x >= -1.0 && bounds.x + bounds.width <= width as f32 + 1.0,
                "{} {} {:?}: text {:?} at {:?} exceeds {}px content width",
                scenario,
                appearance,
                outer,
                value,
                bounds,
                width
            );
        }
        if let Ok(directory) = std::env::var("R06A_CAPTURE_DIR") {
            std::fs::create_dir_all(&directory).expect("create disposable capture directory");
            let viewport = Rectangle::with_size(size);
            element.as_widget_mut().update(
                &mut tree,
                &iced::Event::Window(iced::window::Event::RedrawRequested(
                    std::time::Instant::now(),
                )),
                Layout::new(&node),
                mouse::Cursor::Unavailable,
                &renderer,
                &mut iced::advanced::clipboard::Null,
                &mut iced::advanced::Shell::new(&mut Vec::new()),
                &viewport,
            );
            element.as_widget().draw(
                &tree,
                &mut renderer,
                &theme,
                &iced::advanced::renderer::Style {
                    text_color: theme.colors.text.primary,
                },
                Layout::new(&node),
                mouse::Cursor::Unavailable,
                &viewport,
            );
            let pixels = renderer.screenshot(
                Size::new(width, height),
                1.0,
                theme.colors.general.background,
            );
            let filename = format!(
                "{scenario}-{appearance}-{}x{}-content-only-synthetic.png",
                outer.0, outer.1
            );
            image::save_buffer(
                std::path::Path::new(&directory).join(filename),
                &pixels,
                width,
                height,
                image::ColorType::Rgba8,
            )
            .expect("save synthetic R06a presenter capture");
        }
        Inspection {
            text: probe.entries,
            focus: probe.focus,
        }
    }

    /// Exercise a real presenter tree with the shell's focus operation and
    /// either Enter or a complete primary-pointer click at the Action bounds.
    pub fn activate<M: Clone + 'static>(
        content: Element<'_, M>,
        target: Id,
        outer: (u32, u32),
        pointer: bool,
    ) -> Vec<M> {
        for bytes in font::load() {
            iced::advanced::graphics::text::font_system()
                .write()
                .unwrap()
                .load_font(bytes);
        }
        let sidebar = if outer.0 <= 1100 { 208.0 } else { 248.0 };
        let size = Size::new(
            ((outer.0 as f32 - sidebar) * 0.8).floor(),
            outer.1.saturating_sub(100) as f32,
        );
        let renderer = iced::futures::executor::block_on(<iced::Renderer as Headless>::new(
            font::REGULAR,
            iced::Pixels(16.0),
            Some("tiny-skia"),
        ))
        .unwrap();
        let mut element: Element<'_, M> = iced::widget::scrollable(content)
            .width(Length::Fill)
            .height(Length::Fill)
            .into();
        let mut tree = Tree::new(element.as_widget());
        let node = element.as_widget_mut().layout(
            &mut tree,
            &renderer,
            &layout::Limits::new(Size::ZERO, size),
        );
        let mut operation: Box<dyn Operation<()>> =
            Box::new(focusable::focus::<()>(target.clone()));
        loop {
            element.as_widget_mut().operate(
                &mut tree,
                Layout::new(&node),
                &renderer,
                &mut operation::black_box(operation.as_mut()),
            );
            match operation.finish() {
                operation::Outcome::None | operation::Outcome::Some(()) => break,
                operation::Outcome::Chain(next) => operation = next,
            }
        }
        let mut probe = TextProbe::default();
        element
            .as_widget_mut()
            .operate(&mut tree, Layout::new(&node), &renderer, &mut probe);
        let bounds = probe
            .focus
            .iter()
            .find_map(|(id, bounds)| (id == &target).then_some(*bounds))
            .expect("production Action focus target");
        let cursor = if pointer {
            mouse::Cursor::Available(bounds.center())
        } else {
            mouse::Cursor::Unavailable
        };
        let events = if pointer {
            vec![
                Event::Mouse(mouse::Event::ButtonPressed(mouse::Button::Left)),
                Event::Mouse(mouse::Event::ButtonReleased(mouse::Button::Left)),
            ]
        } else {
            let key = keyboard::Key::Named(keyboard::key::Named::Enter);
            let physical_key =
                keyboard::key::Physical::Unidentified(keyboard::key::NativeCode::Unidentified);
            vec![
                Event::Keyboard(keyboard::Event::KeyPressed {
                    key: key.clone(),
                    modified_key: key.clone(),
                    physical_key,
                    location: keyboard::Location::Standard,
                    modifiers: keyboard::Modifiers::empty(),
                    text: None,
                    repeat: false,
                }),
                Event::Keyboard(keyboard::Event::KeyReleased {
                    key: key.clone(),
                    modified_key: key,
                    physical_key,
                    location: keyboard::Location::Standard,
                    modifiers: keyboard::Modifiers::empty(),
                }),
            ]
        };
        let mut messages = Vec::new();
        for event in events {
            element.as_widget_mut().update(
                &mut tree,
                &event,
                Layout::new(&node),
                cursor,
                &renderer,
                &mut iced::advanced::clipboard::Null,
                &mut iced::advanced::Shell::new(&mut messages),
                &Rectangle::with_size(size),
            );
        }
        messages
    }
}

#[cfg(test)]
mod tests {
    use super::AmountCell;

    #[test]
    fn masking_erases_primary_secondary_and_explanation() {
        let cell = AmountCell::shown("42 BTC", Some("$123".into()))
            .with_explanation("42 BTC available at current rate")
            .redact();
        assert_eq!(cell, AmountCell::masked());
        assert!(!format!("{cell:?}").contains("42"));
        assert!(!format!("{cell:?}").contains("123"));
    }

    #[test]
    fn unknown_and_zero_are_distinct() {
        let unknown = AmountCell::unknown("History is still loading");
        let known_zero = AmountCell::shown("0 SATS", None);
        assert_ne!(unknown, known_zero);
        assert_eq!(unknown.primary, "—");
    }
}
