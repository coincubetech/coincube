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
