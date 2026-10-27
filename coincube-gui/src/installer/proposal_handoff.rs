//! Presentation-only entry to the opt-in native Regtest proposal flow.
use super::{proposal, Context, Message, Step};
use coincube_ui::{
    component::text::{h3, text},
    widget::Element,
};
use iced::widget::{button, column, scrollable};
use std::sync::Arc;

pub(crate) struct ProposalHandoff {
    session: Arc<proposal::Session>,
}

impl ProposalHandoff {
    pub(crate) fn new(session: Arc<proposal::Session>) -> Self {
        Self { session }
    }
}

impl Step for ProposalHandoff {
    fn apply(&mut self, _ctx: &mut Context) -> bool {
        self.session.active()
    }

    fn view<'a>(
        &'a self,
        _hws: &'a crate::hw::HardwareWallets,
        _progress: (usize, usize),
        _email: Option<&'a str>,
    ) -> Element<'a, Message> {
        let topology = self.session.topology();
        let active = self.session.active();
        let content = column![
            h3("Regtest Vault proposal prepared"),
            text("A paired client prepared this proposed policy. No Vault has been created."),
            text(format!(
                "Primary: {} of {} public-key slots. Recovery: {} of {} after {} relative blocks for each coin. Block time varies; this is not a calendar deadline.",
                topology.primary.threshold,
                topology.primary.key_count,
                topology.recovery.quorum.threshold,
                topology.recovery.quorum.key_count,
                topology.recovery.relative_delay_blocks,
            )),
            text("This opt-in setup is limited to Bitcoin Regtest and a loopback node on port 28443. The final native review checks the actual P2WSH descriptor and node before creation."),
            text("Next, choose and check the real keys in Tenshu, then complete its backup and node steps. A proposal is not proof of device control, backup possession, independent custody, or a recovery test; those checks are not verified here."),
            text("Tenshu owns the final descriptor review and creation approval. Creating the Vault does not grant the paired client read access to it."),
            text(if active {
                "Review the requested topology, then continue to native key selection."
            } else {
                "Proposal access ended. Return to the Cube and prepare a fresh proposal."
            }),
            button("Continue to key selection").on_press_maybe(active.then_some(Message::Next)),
            button("Return to Cube").on_press(if active {
                Message::Previous
            } else {
                Message::BackToApp(coincube_core::miniscript::bitcoin::Network::Regtest)
            }),
        ]
        .spacing(16)
        .padding(24);
        scrollable(content).into()
    }
}
