pub mod custom;
pub mod diagram;
pub mod inheritance;
pub mod multisig_security_wallet;

use iced::{Alignment, Length};

use coincube_ui::{
    component::{
        button,
        text::{h3, p1_regular, p2_regular, Text},
    },
    theme,
    widget::*,
};

use self::diagram::{policy_timeline, PolicyRow};
use crate::installer::context::DescriptorTemplate;
use crate::installer::{message::Message, view::layout};

/// Selecting a policy advances to its introduction in the existing installer
/// step. Keep the card action on that real selection message.
fn template_card(
    title: &'static str,
    description: &'static str,
    template: DescriptorTemplate,
) -> Element<'static, Message> {
    Button::new(
        Column::new()
            .align_x(Alignment::Start)
            .spacing(4)
            .push(p1_regular(title).bold())
            .push(p2_regular(description).style(theme::text::secondary))
            .width(Length::Fill),
    )
    .padding([14, 18])
    .width(Length::Fill)
    .style(theme::button::secondary)
    .on_press(Message::SelectDescriptorTemplate(template))
    .into()
}

pub fn choose_descriptor_template(progress: (usize, usize)) -> Element<'static, Message> {
    layout(
        progress,
        None,
        "Choose wallet type",
        Column::new()
            .max_width(800.0)
            .align_x(Alignment::Start)
            .spacing(10)
            .push(h3("Choose a Vault policy"))
            .push(
                p2_regular("Choose how your Vault can be spent now and recovered after inactivity. Select a policy to see its keys and recovery timing.")
                    .style(theme::text::secondary),
            )
            .push(template_card(
                "Simple inheritance",
                "Your key spends now; your heir's key can recover after inactivity. 2 keys.",
                DescriptorTemplate::SimpleInheritance,
            ))
            .push(template_card(
                "Expanding multisig",
                "Both primary keys spend now; a third key joins either one for recovery later. 3 keys.",
                DescriptorTemplate::MultisigSecurity,
            ))
            .push(template_card(
                "2-of-3 multisig with inheritance",
                "Any two primary keys spend now; an heir's key can recover later. 4 keys.",
                DescriptorTemplate::TwoOfThreeInheritance,
            ))
            .push(template_card(
                "Multisig with inheritance & backup recovery",
                "2-of-3 primary spending, inheritance, and a later backup path. 7 keys.",
                DescriptorTemplate::MultisigInheritanceRecovery,
            ))
            .push(template_card(
                "Expanding multisig with inheritance & backup recovery",
                "Primary keys also join the inheritance path; a later backup path remains. 7 keys.",
                DescriptorTemplate::ExpandingMultisigInheritanceRecovery,
            ))
            .push(template_card(
                "Build your own",
                "Define your primary and recovery spending policies.",
                DescriptorTemplate::Custom,
            )),
        true,
        Some(Message::Previous),
    )
}

/// Shared introduction composition. The selected policy is identified by its
/// heading after the chooser's existing select-and-advance action.
pub(super) fn template_introduction(
    progress: (usize, usize),
    title: &'static str,
    keys_needed: &'static str,
    policy_explanation: &'static str,
    tip: &'static str,
    rows: Vec<PolicyRow<'static>>,
) -> Element<'static, Message> {
    layout(
        progress,
        None,
        "Introduction",
        Column::new()
            .align_x(Alignment::Start)
            .max_width(800.0)
            .spacing(16)
            .push(h3(title))
            .push(p1_regular(keys_needed))
            .push(p2_regular(policy_explanation).style(theme::text::secondary))
            .push(policy_timeline(rows))
            .push(p2_regular(tip).style(theme::text::secondary))
            .push(
                Row::new()
                    .width(Length::Fill)
                    .push(Space::new().width(Length::Fill))
                    .push(
                        button::primary(None, "Next")
                            .width(Length::Fixed(200.0))
                            .on_press(Message::Next),
                    ),
            )
            .push(Space::new().height(Length::Fixed(24.0))),
        true,
        Some(Message::Previous),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use coincube_ui::font;
    use iced::{
        advanced::{
            layout as widget_layout,
            renderer::Headless,
            widget::{Id, Operation, Tree},
            Layout,
        },
        mouse, Event, Rectangle, Size,
    };

    #[derive(Default)]
    struct Probe {
        labels: Vec<(String, Rectangle)>,
        scrolls: Vec<(Rectangle, Rectangle)>,
    }

    impl Operation for Probe {
        fn traverse(&mut self, operate: &mut dyn FnMut(&mut dyn Operation)) {
            operate(self);
        }

        fn text(&mut self, _: Option<&Id>, bounds: Rectangle, value: &str) {
            self.labels.push((value.to_owned(), bounds));
        }

        fn scrollable(
            &mut self,
            _: Option<&Id>,
            bounds: Rectangle,
            content: Rectangle,
            _: iced::Vector,
            _: &mut dyn iced::advanced::widget::operation::Scrollable,
        ) {
            self.scrolls.push((bounds, content));
        }
    }

    fn renderer() -> iced::Renderer {
        for bytes in font::load() {
            iced::advanced::graphics::text::font_system()
                .write()
                .unwrap()
                .load_font(bytes);
        }
        iced::futures::executor::block_on(<iced::Renderer as Headless>::new(
            font::REGULAR,
            iced::Pixels(16.0),
            Some("tiny-skia"),
        ))
        .expect("software renderer")
    }

    /// Mount the production installer view; PNGs are opt-in and contain only
    /// static public policy labels. The installer shell supplies scrolling.
    fn inspect(
        scenario: &str,
        mut element: Element<'static, Message>,
        window: (u32, u32),
        appearance: &str,
        theme: coincube_ui::theme::Theme,
    ) -> Probe {
        let mut renderer = renderer();
        let size = Size::new(window.0 as f32, window.1 as f32);
        let mut tree = Tree::new(element.as_widget());
        let node = element.as_widget_mut().layout(
            &mut tree,
            &renderer,
            &widget_layout::Limits::new(Size::ZERO, size),
        );
        assert!(
            node.size().width <= size.width + 1.0,
            "{scenario}: root width exceeds {}",
            size.width
        );
        let mut probe = Probe::default();
        element
            .as_widget_mut()
            .operate(&mut tree, Layout::new(&node), &renderer, &mut probe);
        for (label, bounds) in &probe.labels {
            assert!(
                bounds.x >= -1.0 && bounds.x + bounds.width <= size.width + 1.0,
                "{scenario} {appearance} {window:?}: {label:?} exceeds width at {bounds:?}"
            );
        }
        assert!(
            probe.scrolls.iter().any(|(bounds, _)| {
                bounds.width <= size.width + 1.0 && bounds.height <= size.height + 1.0
            }),
            "{scenario}: installer content has no bounded scrollable"
        );

        if let Ok(directory) = std::env::var("R13A_CAPTURE_DIR") {
            std::fs::create_dir_all(&directory).expect("create synthetic capture directory");
            let viewport = Rectangle::with_size(size);
            let mut emitted = Vec::new();
            element.as_widget_mut().update(
                &mut tree,
                &Event::Window(iced::window::Event::RedrawRequested(
                    std::time::Instant::now(),
                )),
                Layout::new(&node),
                mouse::Cursor::Unavailable,
                &renderer,
                &mut iced::advanced::clipboard::Null,
                &mut iced::advanced::Shell::new(&mut emitted),
                &viewport,
            );
            assert!(emitted.is_empty(), "render emitted an installer intent");
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
                Size::new(window.0, window.1),
                1.0,
                theme.colors.general.background,
            );
            let file = format!(
                "{scenario}-{appearance}-{}x{}-installer-synthetic.png",
                window.0, window.1
            );
            ::image::save_buffer(
                std::path::Path::new(&directory).join(file),
                &pixels,
                window.0,
                window.1,
                ::image::ColorType::Rgba8,
            )
            .expect("save synthetic policy capture");
        }
        probe
    }

    #[test]
    fn six_cards_dispatch_exactly_one_real_template_intent() {
        let cases = [
            ("Simple inheritance", DescriptorTemplate::SimpleInheritance),
            ("Expanding multisig", DescriptorTemplate::MultisigSecurity),
            (
                "2-of-3 multisig with inheritance",
                DescriptorTemplate::TwoOfThreeInheritance,
            ),
            (
                "Multisig with inheritance & backup recovery",
                DescriptorTemplate::MultisigInheritanceRecovery,
            ),
            (
                "Expanding multisig with inheritance & backup recovery",
                DescriptorTemplate::ExpandingMultisigInheritanceRecovery,
            ),
            ("Build your own", DescriptorTemplate::Custom),
        ];
        let size = Size::new(1440.0, 1800.0);
        let renderer = renderer();
        for (title, expected) in cases {
            let mut element = choose_descriptor_template((1, 12));
            let mut tree = Tree::new(element.as_widget());
            let node = element.as_widget_mut().layout(
                &mut tree,
                &renderer,
                &widget_layout::Limits::new(Size::ZERO, size),
            );
            let mut probe = Probe::default();
            element
                .as_widget_mut()
                .operate(&mut tree, Layout::new(&node), &renderer, &mut probe);
            let bounds = probe
                .labels
                .iter()
                .find_map(|(label, bounds)| (label == title).then_some(*bounds))
                .unwrap_or_else(|| panic!("missing {title} card in mounted chooser"));
            let mut emitted = Vec::new();
            let cursor = mouse::Cursor::Available(bounds.center());
            for event in [
                Event::Mouse(mouse::Event::ButtonPressed(mouse::Button::Left)),
                Event::Mouse(mouse::Event::ButtonReleased(mouse::Button::Left)),
            ] {
                element.as_widget_mut().update(
                    &mut tree,
                    &event,
                    Layout::new(&node),
                    cursor,
                    &renderer,
                    &mut iced::advanced::clipboard::Null,
                    &mut iced::advanced::Shell::new(&mut emitted),
                    &Rectangle::with_size(size),
                );
            }
            assert!(
                matches!(emitted.as_slice(), [Message::SelectDescriptorTemplate(actual)] if *actual == expected),
                "{title}: expected one matching selection, got {emitted:?}"
            );
        }
    }

    #[test]
    fn chooser_and_six_introductions_mount_at_compact_and_wide_sizes() {
        type View = fn((usize, usize)) -> Element<'static, Message>;
        let introductions: [(&str, View, &[&str]); 6] = [
            (
                "simple",
                inheritance::inheritance_template_description,
                &[
                    "Simple inheritance wallet",
                    "Primary Key",
                    "Inheritance Key",
                ],
            ),
            (
                "expanding",
                multisig_security_wallet::multisig_security_template_description,
                &[
                    "Expanding multisig wallet",
                    "2-of-2 multisig",
                    "2-of-3 multisig",
                ],
            ),
            (
                "multisig-inheritance",
                inheritance::two_of_three_inheritance_description,
                &["2-of-3 multisig with inheritance", "Inheritance Key"],
            ),
            (
                "multisig-backup",
                multisig_security_wallet::multisig_inheritance_recovery_description,
                &[
                    "Multisig with inheritance & backup recovery",
                    "Second recovery policy",
                ],
            ),
            (
                "expanding-backup",
                multisig_security_wallet::expanding_multisig_inheritance_recovery_description,
                &[
                    "Expanding multisig with inheritance & backup recovery",
                    "2-of-6 multisig",
                ],
            ),
            (
                "custom",
                custom::custom_template_description,
                &["Build your own", "Primary Keys", "Recovery Keys"],
            ),
        ];
        for (appearance, theme) in [
            ("dark", coincube_ui::theme::Theme::dark()),
            ("light", coincube_ui::theme::Theme::light()),
        ] {
            for window in [(1024, 768), (1440, 900)] {
                let chooser = inspect(
                    "chooser",
                    choose_descriptor_template((1, 12)),
                    window,
                    appearance,
                    theme.clone(),
                );
                for title in [
                    "Simple inheritance",
                    "Expanding multisig",
                    "2-of-3 multisig with inheritance",
                    "Multisig with inheritance & backup recovery",
                    "Expanding multisig with inheritance & backup recovery",
                    "Build your own",
                ] {
                    assert_eq!(
                        chooser
                            .labels
                            .iter()
                            .filter(|(label, _)| label == title)
                            .count(),
                        1,
                        "missing or duplicate chooser card {title}"
                    );
                }
                for (scenario, view, required) in introductions {
                    let content =
                        inspect(scenario, view((2, 12)), window, appearance, theme.clone());
                    for label in required {
                        assert!(
                            content.labels.iter().any(|(actual, _)| actual == label),
                            "{scenario}: missing {label}"
                        );
                    }
                    assert!(
                        content.labels.iter().any(|(label, _)| label == "Next"),
                        "{scenario}: Next action missing"
                    );
                }
            }
        }
    }
}
