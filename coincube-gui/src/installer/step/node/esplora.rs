use coincube_core::miniscript::bitcoin::{constants::ChainHash, hashes::Hash, BlockHash};
use coincube_ui::{component::form, widget::*};
use coincubed::{config::EsploraConfig, esplora_client};
use iced::Task;

use crate::{
    installer::{
        context::Context,
        message::{self, Message},
        view, Error,
    },
    node::esplora::ConfigField,
};

#[derive(Clone, Default)]
pub struct DefineEsplora {
    address: form::Value<String>,
    placeholder: String,
    chain: Option<crate::chain::ChainId>,
}

impl DefineEsplora {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn load_context(&mut self, ctx: &Context) {
        self.placeholder = super::super::super::connect_url(ctx.bitcoin_config.chain);
        self.chain = Some(ctx.bitcoin_config.chain);
    }

    pub fn can_try_ping(&self) -> bool {
        !self.address.value.is_empty() && self.address.valid
    }

    pub fn update(&mut self, message: message::DefineNode) -> Task<Message> {
        if let message::DefineNode::DefineEsplora(message::DefineEsplora::ConfigFieldEdited(
            field,
            value,
        )) = message
        {
            match field {
                ConfigField::Address => {
                    self.address.value.clone_from(&value);
                    self.address.valid = crate::node::esplora::is_esplora_address_valid(&value);
                }
            }
        }
        Task::none()
    }

    pub fn apply(&mut self, ctx: &mut Context) -> bool {
        // Genesis is shared with Bitcoin and cannot authenticate the fork.
        // BTCB2 must use the authenticated Connect selection, never this
        // arbitrary-URL/genesis-only provider path.
        if ctx.bitcoin_config.chain.is_blake2b() {
            return false;
        }
        if let Some(addr) = crate::node::esplora::normalize_esplora_address(&self.address.value) {
            ctx.bitcoin_backend = Some(coincubed::config::BitcoinBackend::Esplora(EsploraConfig {
                addr,
                token: None,
                fallback_addr: None,
                fallback_token: None,
                secondary_fallback_addr: None,
                secondary_fallback_token: None,
            }));
            return true;
        }
        false
    }

    pub fn view(&self) -> Element<Message> {
        view::define_esplora(&self.address, &self.placeholder)
    }

    pub fn ping(&self) -> Result<(), Error> {
        let addr = crate::node::esplora::normalize_esplora_address(&self.address.value)
            .ok_or_else(|| Error::Esplora("Invalid Esplora URL".to_string()))?;
        let chain = self
            .chain
            .ok_or_else(|| Error::Esplora("Chain is not selected".to_string()))?;
        if chain.is_blake2b() {
            return Err(Error::Esplora(
                "Bitcoin Blake2b requires authenticated Connect Esplora".to_string(),
            ));
        }
        let network = chain.bitcoin_network();
        // Match the daemon's Esplora client (see `coincubed`'s
        // `build_blocking_client`): a 3s timeout is too aggressive — real-world
        // TLS handshakes to Cloudflare-fronted providers were observed at 5–11s,
        // and this ping makes two sequential round-trips — and the blocking
        // client's `minreq` backend can't decompress gzip/brotli, so request an
        // identity encoding to avoid `InvalidUtf8InResponse` on compressed
        // bodies. Without these, a perfectly valid endpoint (e.g.
        // mempool.space/testnet4/api) fails the check.
        let client = esplora_client::Builder::new(&addr)
            .timeout(15)
            .header("Accept-Encoding", "identity")
            .build_blocking();
        let height = client
            .get_height()
            .map_err(|e| Error::Esplora(e.to_string()))?;
        let server_genesis = client
            .get_block_hash(0)
            .map_err(|e| Error::Esplora(e.to_string()))?;
        let expected_genesis =
            BlockHash::from_byte_array(*ChainHash::using_genesis_block(network).as_bytes());
        if server_genesis != expected_genesis {
            return Err(Error::Esplora(format!(
                "Esplora URL is not for {} (height {}, genesis {})",
                network, height, server_genesis
            )));
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{chain::ChainId, dir::CoincubeDirectory, installer::context::RemoteBackend};

    #[test]
    fn fork_provider_refuses_genesis_only_validation_without_network_io() {
        for chain in [ChainId::BitcoinBlake2b, ChainId::BitcoinBlake2bTestnet4] {
            let mut ctx = Context::new_for_chain(
                chain,
                CoincubeDirectory::new(std::path::PathBuf::new()),
                RemoteBackend::None,
                None,
                None,
            );
            let mut step = DefineEsplora::new();
            step.load_context(&ctx);
            step.address.value = "http://127.0.0.1:1".to_string();
            assert_eq!(step.placeholder, crate::installer::connect_url(chain));
            assert!(
                matches!(step.ping(), Err(Error::Esplora(message)) if message.contains("authenticated Connect"))
            );
            assert!(!step.apply(&mut ctx));
            assert!(ctx.bitcoin_backend.is_none());
        }
    }
}
