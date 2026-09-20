//! Chain identity as the GUI sees it: the shared [`ChainId`] representation
//! plus the *policy* this build applies to it.
//!
//! The identity type itself — seven variants, serde / directory / Connect
//! spellings, the lossy `bitcoin::Network` projection — lives in
//! [`coincube_core::chain`] so that the GUI, the daemon and core all agree on
//! one type and one wire encoding; it is re-exported here unchanged, and
//! every existing `crate::chain::ChainId` path keeps resolving to it. What
//! stays in this module is what only the GUI decides: which chains the
//! launcher offers, the user-facing label and ticker, and whether this build
//! can run a Cube on a chain at all ([`ChainIdExt`]).
//!
//! # Dormant in this slice
//!
//! Both BTCB2 identities exist so that settings, directories and Connect
//! strings are distinct from day one — but nothing behind them is wired yet
//! (no node flavour, no provider, no unified-sighash signing). Until those
//! land, [`ChainIdExt::runtime_support`] reports [`RuntimeSupport::Dormant`]
//! and every entry point that would start a daemon, node, SDK or signer
//! refuses first. See `PLAN-bitcoin-blake2b.md` PR 2 and coincube-api#280.

pub use coincube_core::chain::{ChainId, UnknownChainId};

/// Whether this build can actually run a Cube on a chain, or merely knows
/// the chain's identity.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum RuntimeSupport {
    /// Daemon, node, providers and signing are wired for this chain.
    Supported,
    /// The identity is known (so its settings and directories are kept
    /// distinct and intact) but the runtime behind it is not in this build.
    /// Every start path must refuse with `reason` before touching a daemon,
    /// node, SDK or signer.
    Dormant { reason: &'static str },
}

impl RuntimeSupport {
    pub fn is_supported(self) -> bool {
        matches!(self, RuntimeSupport::Supported)
    }
}

/// User-facing copy for refusing to open a Bitcoin Blake2b Cube in a build
/// that only carries its identity. Says what is true — the Cube and its
/// settings are intact, this version can't run it — and nothing about the
/// sender/desktop being at fault.
pub const BTCB2_DORMANT_REASON: &str = "This Cube is on Bitcoin Blake2b, which this version of \
     Tenshu can't open yet. Its settings are untouched; update Tenshu to a version with \
     Bitcoin Blake2b support to use it.";

/// Capability for the explicit authenticated Connect Vault path only.
/// Generic startup, managed nodes, duress and migration still consult
/// `runtime_support()` and remain dormant for the fork.
pub(crate) fn authenticated_connect_support(chain: ChainId) -> RuntimeSupport {
    if chain.is_blake2b() {
        RuntimeSupport::Supported
    } else {
        chain.runtime_support()
    }
}

/// Recheck the current account flag before a fork daemon may create files.
/// The authenticated anchor subsequently validates the exact selected chain.
pub(crate) async fn require_connect_feature(
    chain: ChainId,
    client: &crate::services::coincube::CoincubeClient,
) -> Result<(), String> {
    if !chain.is_blake2b() || client.token().is_none() {
        return Err("An authenticated Bitcoin Blake2b Connect session is required".into());
    }
    let features = client
        .get_connect_features()
        .await
        .map_err(|_| "Bitcoin Blake2b availability could not be verified".to_string())?;
    if features.bitcoin_blake2b_enabled != Some(true) {
        return Err("Bitcoin Blake2b isn't enabled for this account".into());
    }
    Ok(())
}

/// GUI policy over the shared [`ChainId`]: launcher selection, presentation
/// and runtime support. An extension trait rather than a second enum so the
/// identity — and its wire encoding — stays the one type core defines; bring
/// it into scope (`use crate::chain::ChainIdExt`) where these are called.
pub trait ChainIdExt {
    /// The chains the launcher offers for creating and opening Cubes today —
    /// the Bitcoin family. The BTCB2 identities are deliberately absent: they
    /// are [`RuntimeSupport::Dormant`] in this build, and a launcher entry
    /// would be a partly working Cube.
    const LAUNCHER: [ChainId; 5];

    /// Neutral, descriptive user-facing name (brand posture: no claim about
    /// which chain "is Bitcoin").
    fn label(self) -> &'static str;

    /// The unit ticker shown next to amounts.
    fn ticker(self) -> &'static str;

    /// Whether this build can run a Cube on this chain. Both BTCB2 identities
    /// are dormant until the node flavour, provider and unified-sighash
    /// signing slices land; every start path checks this first.
    fn runtime_support(self) -> RuntimeSupport;
}

impl ChainIdExt for ChainId {
    const LAUNCHER: [ChainId; 5] = [
        ChainId::Bitcoin,
        ChainId::Testnet,
        ChainId::Testnet4,
        ChainId::Signet,
        ChainId::Regtest,
    ];

    fn label(self) -> &'static str {
        match self {
            ChainId::Bitcoin => "Bitcoin",
            ChainId::Testnet => "Testnet",
            ChainId::Testnet4 => "Testnet4",
            ChainId::Signet => "Signet",
            ChainId::Regtest => "Regtest",
            ChainId::BitcoinBlake2b => "Bitcoin Blake2b",
            ChainId::BitcoinBlake2bTestnet4 => "Bitcoin Blake2b Testnet4",
        }
    }

    fn ticker(self) -> &'static str {
        match self {
            ChainId::Bitcoin
            | ChainId::Testnet
            | ChainId::Testnet4
            | ChainId::Signet
            | ChainId::Regtest => "BTC",
            ChainId::BitcoinBlake2b | ChainId::BitcoinBlake2bTestnet4 => "BTCB2",
        }
    }

    fn runtime_support(self) -> RuntimeSupport {
        if self.is_blake2b() {
            RuntimeSupport::Dormant {
                reason: BTCB2_DORMANT_REASON,
            }
        } else {
            RuntimeSupport::Supported
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use coincube_core::miniscript::bitcoin::Network;

    /// Compile-time proof that the GUI and core hand around one type: a
    /// value built through the core path is accepted where the GUI path is
    /// expected, and both name the same `TypeId`. If anyone reintroduces a
    /// GUI-local enum, this stops compiling.
    #[test]
    fn gui_and_core_share_one_chain_id_type() {
        fn same_type<T>(_: T, _: T) {}
        same_type(
            crate::chain::ChainId::BitcoinBlake2b,
            coincube_core::chain::ChainId::BitcoinBlake2b,
        );
        assert_eq!(
            std::any::TypeId::of::<crate::chain::ChainId>(),
            std::any::TypeId::of::<coincube_core::chain::ChainId>()
        );
        assert_eq!(
            std::any::TypeId::of::<crate::chain::UnknownChainId>(),
            std::any::TypeId::of::<coincube_core::chain::UnknownChainId>()
        );
        // The wire encoding the GUI writes is the one core defines; a
        // settings file written through either path reads back through the
        // other, including the fork spellings older builds refuse.
        for chain in ChainId::ALL {
            let json = serde_json::to_string(&chain).unwrap();
            assert_eq!(
                serde_json::from_str::<coincube_core::chain::ChainId>(&json).unwrap(),
                chain
            );
            assert_eq!(json, format!("\"{}\"", chain.dir_name()));
        }
    }

    #[test]
    fn launcher_offers_the_bitcoin_family_only_and_keeps_its_wire_form() {
        // What every existing settings.json already contains.
        for chain in ChainId::LAUNCHER {
            assert!(!chain.is_blake2b(), "{:?}", chain);
            assert!(chain.runtime_support().is_supported(), "{:?}", chain);
            let ours = serde_json::to_string(&chain).unwrap();
            let theirs = serde_json::to_string(&chain.bitcoin_network()).unwrap();
            assert_eq!(ours, theirs, "{:?}", chain);
            assert_eq!(chain.dir_name(), chain.bitcoin_network().to_string());
        }
        // LAUNCHER is exactly ALL minus the dormant fork identities, in order.
        let expected: Vec<ChainId> = ChainId::ALL
            .iter()
            .copied()
            .filter(|c| !c.is_blake2b())
            .collect();
        assert_eq!(ChainId::LAUNCHER.to_vec(), expected);
        assert!(ChainId::ALL
            .iter()
            .filter(|c| c.is_blake2b())
            .all(|c| !ChainId::LAUNCHER.contains(c)));
    }

    #[test]
    fn labels_and_tickers() {
        assert_eq!(ChainId::BitcoinBlake2b.label(), "Bitcoin Blake2b");
        assert_eq!(
            ChainId::BitcoinBlake2bTestnet4.label(),
            "Bitcoin Blake2b Testnet4"
        );
        assert_eq!(ChainId::Bitcoin.label(), "Bitcoin");
        assert_eq!(ChainId::BitcoinBlake2b.ticker(), "BTCB2");
        assert_eq!(ChainId::BitcoinBlake2bTestnet4.ticker(), "BTCB2");
        assert_eq!(ChainId::Bitcoin.ticker(), "BTC");
        assert!(ChainId::LAUNCHER.iter().all(|c| c.ticker() == "BTC"));
        // A label never repeats: the two fork identities must not be
        // mistaken for their encoding twins in any list.
        let labels: std::collections::HashSet<_> = ChainId::ALL.iter().map(|c| c.label()).collect();
        assert_eq!(labels.len(), ChainId::ALL.len());
    }

    #[test]
    fn runtime_support_is_dormant_for_both_fork_variants_only() {
        for chain in ChainId::ALL {
            let support = chain.runtime_support();
            assert_eq!(support.is_supported(), !chain.is_blake2b(), "{:?}", chain);
            if let RuntimeSupport::Dormant { reason } = support {
                assert_eq!(reason, BTCB2_DORMANT_REASON);
                assert!(reason.contains("Bitcoin Blake2b"));
                assert!(!reason.contains("sender"));
            }
        }
        // A `Network` never reaches a dormant identity: the conversion the
        // Bitcoin-family flows rely on lands on a supported chain every time.
        for network in [
            Network::Bitcoin,
            Network::Testnet,
            Network::Testnet4,
            Network::Signet,
            Network::Regtest,
        ] {
            assert!(ChainId::from(network).runtime_support().is_supported());
        }
    }
}
