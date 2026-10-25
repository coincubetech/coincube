pub mod custom;
pub mod diagram;
pub mod inheritance;
pub mod multisig_security_wallet;

use iced::{Alignment, Length};

use coincube_ui::{
    component::text::{h3, p1_regular, p2_regular, Text},
    theme,
    widget::{action::Action, *},
};

use self::diagram::{policy_timeline, PolicyRow};
use crate::installer::context::DescriptorTemplate;
use crate::installer::{message::Message, view::layout};

/// Selecting a policy advances to its introduction in the existing installer
/// step. Keep the card action on that real selection message.
fn template_card(
    id: &'static str,
    ui_instance: u64,
    title: &'static str,
    description: &'static str,
    template: DescriptorTemplate,
) -> Element<'static, Message> {
    Action::new(
        format!("tenshu.installer.vault.template.{}.{}", ui_instance, id),
        Column::new()
            .align_x(Alignment::Start)
            .spacing(4)
            .push(p1_regular(title).bold())
            .push(p2_regular(description).style(theme::text::secondary))
            .width(Length::Fill),
        Message::FocusAction,
    )
    .padding([14, 18])
    .width(Length::Fill)
    .style(theme::button::secondary)
    .on_press(Message::SelectDescriptorTemplate(template))
    .into()
}

pub fn choose_descriptor_template(
    progress: (usize, usize),
    ui_instance: u64,
) -> Element<'static, Message> {
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
                "simple",
                ui_instance,
                "Simple inheritance",
                "Your key spends now; your heir's key can recover after inactivity. 2 keys.",
                DescriptorTemplate::SimpleInheritance,
            ))
            .push(template_card(
                "expanding",
                ui_instance,
                "Expanding multisig",
                "Both primary keys spend now; a third key joins either one for recovery later. 3 keys.",
                DescriptorTemplate::MultisigSecurity,
            ))
            .push(template_card(
                "multisig-inheritance",
                ui_instance,
                "2-of-3 multisig with inheritance",
                "Any two primary keys spend now; an heir's key can recover later. 4 keys.",
                DescriptorTemplate::TwoOfThreeInheritance,
            ))
            .push(template_card(
                "multisig-backup",
                ui_instance,
                "Multisig with inheritance & backup recovery",
                "2-of-3 primary spending, inheritance, and a later backup path. 7 keys.",
                DescriptorTemplate::MultisigInheritanceRecovery,
            ))
            .push(template_card(
                "expanding-backup",
                ui_instance,
                "Expanding multisig with inheritance & backup recovery",
                "Primary keys also join the inheritance path; a later backup path remains. 7 keys.",
                DescriptorTemplate::ExpandingMultisigInheritanceRecovery,
            ))
            .push(template_card(
                "custom",
                ui_instance,
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
    ui_instance: u64,
    show_keychain_tip: bool,
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
                show_keychain_tip.then_some(
                    p2_regular("A family member or friend can contribute a Keychain key remotely.")
                        .style(theme::text::secondary),
                ),
            )
            .push(
                Row::new()
                    .width(Length::Fill)
                    .push(Space::new().width(Length::Fill))
                    .push(
                        Action::new(
                            format!("tenshu.installer.vault.intro.{}.next", ui_instance),
                            p2_regular("Next"),
                            Message::FocusAction,
                        )
                        .style(theme::button::primary)
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
            widget::{operation, operation::focusable, operation::Focusable, Id, Operation, Tree},
            Layout,
        },
        keyboard, mouse, Event, Rectangle, Size,
    };

    #[derive(Default)]
    struct Probe {
        labels: Vec<(String, Rectangle)>,
        scrolls: Vec<(Rectangle, Rectangle)>,
        focus: Vec<Id>,
    }

    impl Operation for Probe {
        fn traverse(&mut self, operate: &mut dyn FnMut(&mut dyn Operation)) {
            operate(self);
        }

        fn text(&mut self, _: Option<&Id>, bounds: Rectangle, value: &str) {
            self.labels.push((value.to_owned(), bounds));
        }

        fn focusable(&mut self, id: Option<&Id>, _: Rectangle, _: &mut dyn Focusable) {
            if let Some(id) = id {
                self.focus.push(id.clone());
            }
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

    fn keyboard_messages(
        mut element: Element<'static, Message>,
        target: Id,
        key: keyboard::Key,
    ) -> Vec<Message> {
        let renderer = renderer();
        let size = Size::new(1440.0, 1800.0);
        let mut tree = Tree::new(element.as_widget());
        let node = element.as_widget_mut().layout(
            &mut tree,
            &renderer,
            &widget_layout::Limits::new(Size::ZERO, size),
        );
        let mut focus: Box<dyn Operation<()>> = Box::new(focusable::focus::<()>(target));
        loop {
            element.as_widget_mut().operate(
                &mut tree,
                Layout::new(&node),
                &renderer,
                &mut operation::black_box(focus.as_mut()),
            );
            match focus.finish() {
                operation::Outcome::None | operation::Outcome::Some(()) => break,
                operation::Outcome::Chain(next) => focus = next,
            }
        }
        let physical_key =
            keyboard::key::Physical::Unidentified(keyboard::key::NativeCode::Unidentified);
        let mut emitted = Vec::new();
        for event in [
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
        ] {
            element.as_widget_mut().update(
                &mut tree,
                &event,
                Layout::new(&node),
                mouse::Cursor::Unavailable,
                &renderer,
                &mut iced::advanced::clipboard::Null,
                &mut iced::advanced::Shell::new(&mut emitted),
                &Rectangle::with_size(size),
            );
        }
        emitted
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
            "{}: root width exceeds {}",
            scenario,
            size.width
        );
        let mut probe = Probe::default();
        element
            .as_widget_mut()
            .operate(&mut tree, Layout::new(&node), &renderer, &mut probe);
        for (label, bounds) in &probe.labels {
            assert!(
                bounds.x >= -1.0 && bounds.x + bounds.width <= size.width + 1.0,
                "{} {} {:?}: {:?} exceeds width at {:?}",
                scenario,
                appearance,
                window,
                label,
                bounds
            );
        }
        assert!(
            probe.scrolls.iter().any(|(bounds, _)| {
                bounds.width <= size.width + 1.0 && bounds.height <= size.height + 1.0
            }),
            "{}: installer content has no bounded scrollable",
            scenario
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
            let mut element = choose_descriptor_template((1, 12), 41);
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
                .unwrap_or_else(|| panic!("missing {} card in mounted chooser", title));
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
                matches!(emitted.as_slice(), [Message::FocusAction(_), Message::SelectDescriptorTemplate(actual)] if *actual == expected),
                "{}: expected focus then one matching selection, got {:?}",
                title,
                emitted
            );
        }
    }

    #[test]
    fn template_actions_have_stable_instance_scopes_and_activate_once_by_keyboard() {
        let first = inspect(
            "first-instance",
            choose_descriptor_template((1, 12), 1001),
            (1440, 900),
            "dark",
            coincube_ui::theme::Theme::dark(),
        );
        let same = inspect(
            "same-instance",
            choose_descriptor_template((1, 12), 1001),
            (1440, 900),
            "dark",
            coincube_ui::theme::Theme::dark(),
        );
        let other = inspect(
            "other-instance",
            choose_descriptor_template((1, 12), 1002),
            (1440, 900),
            "dark",
            coincube_ui::theme::Theme::dark(),
        );
        let first_id = Id::from("tenshu.installer.vault.template.1001.simple");
        let other_id = Id::from("tenshu.installer.vault.template.1002.simple");
        assert!(first.focus.contains(&first_id));
        assert!(same.focus.contains(&first_id));
        assert!(!other.focus.contains(&first_id));
        assert!(other.focus.contains(&other_id));

        for (id, expected) in [
            ("simple", DescriptorTemplate::SimpleInheritance),
            ("expanding", DescriptorTemplate::MultisigSecurity),
            (
                "multisig-inheritance",
                DescriptorTemplate::TwoOfThreeInheritance,
            ),
            (
                "multisig-backup",
                DescriptorTemplate::MultisigInheritanceRecovery,
            ),
            (
                "expanding-backup",
                DescriptorTemplate::ExpandingMultisigInheritanceRecovery,
            ),
            ("custom", DescriptorTemplate::Custom),
        ] {
            for key in [
                keyboard::Key::Named(keyboard::key::Named::Enter),
                keyboard::Key::Named(keyboard::key::Named::Space),
            ] {
                let target = Id::from(format!("tenshu.installer.vault.template.1001.{}", id));
                let emitted =
                    keyboard_messages(choose_descriptor_template((1, 12), 1001), target, key);
                assert!(
                    matches!(emitted.as_slice(), [Message::SelectDescriptorTemplate(actual)] if *actual == expected),
                    "{}: keyboard must select exactly once, got {:?}",
                    id,
                    emitted
                );
            }
        }

        let next = keyboard_messages(
            inheritance::inheritance_template_description((2, 12), 1003, false),
            Id::from("tenshu.installer.vault.intro.1003.next"),
            keyboard::Key::Named(keyboard::key::Named::Enter),
        );
        assert!(matches!(next.as_slice(), [Message::Next]));
    }

    #[test]
    fn remote_keychain_tip_is_shown_only_when_step_supplies_picker_eligibility() {
        let unavailable = inspect(
            "keychain-unavailable",
            inheritance::inheritance_template_description((2, 12), 2001, false),
            (1024, 768),
            "dark",
            coincube_ui::theme::Theme::dark(),
        );
        let available = inspect(
            "keychain-available",
            inheritance::inheritance_template_description((2, 12), 2002, true),
            (1024, 768),
            "dark",
            coincube_ui::theme::Theme::dark(),
        );
        let tip = "A family member or friend can contribute a Keychain key remotely.";
        assert!(!unavailable.labels.iter().any(|(label, _)| label == tip));
        assert!(available.labels.iter().any(|(label, _)| label == tip));
    }

    #[test]
    fn chooser_and_six_introductions_mount_at_compact_and_wide_sizes() {
        type View = fn((usize, usize), u64, bool) -> Element<'static, Message>;
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
                    choose_descriptor_template((1, 12), 41),
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
                        "missing or duplicate chooser card {}",
                        title
                    );
                }
                for (scenario, view, required) in introductions {
                    let content = inspect(
                        scenario,
                        view((2, 12), 42, false),
                        window,
                        appearance,
                        theme.clone(),
                    );
                    for label in required {
                        assert!(
                            content.labels.iter().any(|(actual, _)| actual == label),
                            "{}: missing {}",
                            scenario,
                            label
                        );
                    }
                    assert!(
                        content.labels.iter().any(|(label, _)| label == "Next"),
                        "{}: Next action missing",
                        scenario
                    );
                }
            }
        }
    }
}
