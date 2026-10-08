//! Explain the Claim before any target setup or transaction construction.
use crate::{
    hw::HardwareWallets,
    installer::{message::Message, step::Step, view},
};
use coincube_ui::{
    component::{button, text::*},
    widget::*,
};
use iced::Length;

pub struct ClaimOverview;

impl Step for ClaimOverview {
    fn view<'a>(
        &'a self,
        _hws: &'a HardwareWallets,
        progress: (usize, usize),
        email: Option<&'a str>,
    ) -> Element<'a, Message> {
        view::layout(
            progress,
            email,
            "Claim Bitcoin Blake2b",
            Column::new()
                .spacing(20)
                .push(p1_regular("Separate the coins in your Bitcoin Vault from their Bitcoin Blake2b counterparts. We’ll guide you through setup and two transactions, with a review before each submission."))
                .push(h4_bold("Prepare your Blake2b Cube"))
                .push(p1_regular("Create a matching Vault on Bitcoin Blake2b using this Cube’s existing keys and PIN, then choose its node connection. Creating the Cube does not move or split coins."))
                .push(h4_bold("1. Split on Bitcoin"))
                .push(p1_regular(format!("Return to your Bitcoin Cube to check eligible coins, fees and replay protection. Review and sign a transfer back to your Vault, then wait for {} Bitcoin confirmations.", coincube_core::claim::MIN_CONFIRMATIONS)))
                .push(h4_bold("2. Claim on Bitcoin Blake2b"))
                .push(p1_regular("Continue in the paired Blake2b Cube to review and sign the fork transfer. Both chains are checked again before submission, and the wizard tracks confirmation."))
                .push(p1_regular("Both transactions require network fees. Eligibility depends on the coins and current chain conditions. You can leave and reopen Claim to continue a recorded transaction."))
                .push(button::primary(None, "Set up Blake2b Cube")
                    .on_press(Message::Next)
                    .width(Length::Fixed(260.0))),
            true,
            Some(Message::Previous),
        )
    }
}

impl From<ClaimOverview> for Box<dyn Step> {
    fn from(step: ClaimOverview) -> Self {
        Box::new(step)
    }
}
