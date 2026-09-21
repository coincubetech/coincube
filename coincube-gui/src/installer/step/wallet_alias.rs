use iced::Task;

use coincube_ui::{component::form, widget::*};

use crate::{
    chain::ChainIdExt,
    hw::HardwareWallets,
    installer::{context::Context, message::Message, step::Step, view},
    services::connect::client::backend::api::WALLET_ALIAS_MAXIMUM_LENGTH,
};

#[derive(Default)]
pub struct WalletAlias {
    wallet_alias: form::Value<String>,
}

impl Step for WalletAlias {
    /// The alias names the *Vault wallet* ("My Vault Bitcoin wallet"), and
    /// `ctx.wallet_alias` is only ever read by the wallet-creating install
    /// paths. A seed-only restore has no wallet to name, so asking is a prompt
    /// for a value that is written and then discarded.
    fn skip(&self, ctx: &Context) -> bool {
        !ctx.installs_vault()
    }

    fn load_context(&mut self, ctx: &Context) {
        match (
            ctx.wallet_alias.is_empty(),
            self.wallet_alias.value.is_empty(),
        ) {
            // Alias from context is the first one to be set.
            (false, _) => {
                self.wallet_alias.value = ctx.wallet_alias.clone();
                self.wallet_alias.valid = true;
            }
            // No alias at all, we set a default value.
            (true, true) => {
                self.wallet_alias.value =
                    format!("My Vault {} wallet", ctx.bitcoin_config.chain.label());
                self.wallet_alias.valid = true;
            }
            // We keep the current value.
            (true, false) => {}
        }
    }

    fn update(&mut self, _hws: &mut HardwareWallets, message: Message) -> Task<Message> {
        if let Message::WalletAliasEdited(alias) = message {
            self.wallet_alias.valid = alias.len() < WALLET_ALIAS_MAXIMUM_LENGTH;
            self.wallet_alias.value = alias;
        }
        Task::none()
    }

    fn view<'a>(
        &'a self,
        _hws: &'a HardwareWallets,
        progress: (usize, usize),
        email: Option<&'a str>,
    ) -> Element<'a, Message> {
        view::wallet_alias(progress, email, &self.wallet_alias)
    }

    fn apply(&mut self, ctx: &mut Context) -> bool {
        if self.wallet_alias.valid {
            ctx.wallet_alias = self.wallet_alias.value.trim().to_string();
            true
        } else {
            false
        }
    }
}

impl From<WalletAlias> for Box<dyn Step> {
    fn from(s: WalletAlias) -> Box<dyn Step> {
        Box::new(s)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{chain::ChainId, dir::CoincubeDirectory, installer::context::RemoteBackend};

    #[test]
    fn default_alias_uses_chain_and_preserves_explicit_alias() {
        for chain in [
            ChainId::Bitcoin,
            ChainId::Testnet4,
            ChainId::BitcoinBlake2b,
            ChainId::BitcoinBlake2bTestnet4,
        ] {
            let mut ctx = Context::new(
                chain.bitcoin_network(),
                CoincubeDirectory::new(Default::default()),
                RemoteBackend::None,
                None,
                None,
            );
            ctx.bitcoin_config.chain = chain;
            let mut step = WalletAlias::default();
            step.load_context(&ctx);
            assert_eq!(
                step.wallet_alias.value,
                format!("My Vault {} wallet", chain.label())
            );
            ctx.wallet_alias = "Family vault".to_string();
            step.load_context(&ctx);
            assert_eq!(step.wallet_alias.value, "Family vault");
        }
    }
}
