//! `ChainId` — the stable identity of the chain a Cube lives on.
//!
//! `bitcoin::Network` answers the *encoding* question (address bytes, bech32
//! HRP, descriptor checksums, PSBT rules) and is the right key for all of
//! that. It cannot answer the *identity* question for Bitcoin Blake2b
//! (BTCB2): the Knots BLAKE2b proof-of-work hardfork deliberately kept
//! Bitcoin's network identity — same magic, same address format, same
//! derivation — so a BTCB2 Cube *encodes* exactly like a mainnet one while
//! needing its own data directory, Connect network string, node, provider
//! chain, labels and signing rules.
//!
//! This type carries the identity. Everything that is about *where a Cube's
//! state lives or who it talks to* keys on [`ChainId`]; everything that is
//! about *bytes on the wire* projects through [`ChainId::bitcoin_network`].
//! The projection is one-way on purpose: there is no way to turn a
//! `bitcoin::Network` back into a BTCB2 identity, so a Bitcoin-family caller
//! that only holds a `Network` can never accidentally land in a BTCB2
//! directory or vice versa ([`From<Network>`] maps to the Bitcoin-family
//! variant only).
//!
//! # Representation only
//!
//! This module holds the *representation* every Rust layer (GUI, daemon,
//! core) must agree on: the seven variants, their serde / directory / Connect
//! spellings and the lossy `Network` projection. Whether a given build can
//! actually *run* a chain — launcher selection, labels, tickers and the
//! dormant BTCB2 gate — is policy that lives with the layer that enforces it
//! (the GUI's `chain` module), not here. Moved from `coincube-gui/src/chain.rs`
//! byte-for-byte on the wire; see coincube-api#291.

use std::fmt;
use std::str::FromStr;

use miniscript::bitcoin::Network;
use serde::{Deserialize, Serialize};

/// The chain a Cube lives on. Serialises as its [`dir_name`](Self::dir_name),
/// which for the five Bitcoin-family variants is byte-identical to what
/// `bitcoin::Network` already wrote into every existing `settings.json`
/// (`"bitcoin"`, `"testnet"`, `"testnet4"`, `"signet"`, `"regtest"`). The
/// two fork variants serialise as strings no previous build ever wrote or
/// accepts, so an older build *refuses* a BTCB2 settings file rather than
/// misreading it as a mainnet one.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum ChainId {
    #[serde(rename = "bitcoin")]
    Bitcoin,
    #[serde(rename = "testnet")]
    Testnet,
    #[serde(rename = "testnet4")]
    Testnet4,
    #[serde(rename = "signet")]
    Signet,
    #[serde(rename = "regtest")]
    Regtest,
    /// Bitcoin Blake2b (BTCB2) — mainnet key and address parameters.
    #[serde(rename = "bitcoin-blake2b")]
    BitcoinBlake2b,
    /// Bitcoin Blake2b on testnet4 (the fork applies there too) — dev/QA
    /// only; testnet parameters.
    #[serde(rename = "bitcoin-blake2b-testnet4")]
    BitcoinBlake2bTestnet4,
}

impl ChainId {
    /// Every identity this build knows, in declaration order.
    pub const ALL: [ChainId; 7] = [
        ChainId::Bitcoin,
        ChainId::Testnet,
        ChainId::Testnet4,
        ChainId::Signet,
        ChainId::Regtest,
        ChainId::BitcoinBlake2b,
        ChainId::BitcoinBlake2bTestnet4,
    ];

    /// The encoding this chain uses: address bytes, HRP, descriptor and PSBT
    /// rules. BTCB2 kept Bitcoin's, so both fork variants project onto their
    /// Bitcoin-family counterpart. **Identity is lost here** — never derive a
    /// directory, Connect string or provider from the result.
    pub fn bitcoin_network(self) -> Network {
        match self {
            ChainId::Bitcoin | ChainId::BitcoinBlake2b => Network::Bitcoin,
            ChainId::Testnet => Network::Testnet,
            ChainId::Testnet4 | ChainId::BitcoinBlake2bTestnet4 => Network::Testnet4,
            ChainId::Signet => Network::Signet,
            ChainId::Regtest => Network::Regtest,
        }
    }

    /// The network string Connect uses for this chain (keychains, keys,
    /// cubes, switch-network, Esplora routes). Agreed with Connect PR 1
    /// (coincube-api#278/#279): the Bitcoin family keeps its historical
    /// strings, the fork uses `bitcoin-blake2b` / `bitcoin-blake2b-testnet4`.
    pub fn api_str(self) -> &'static str {
        match self {
            ChainId::Bitcoin => "mainnet",
            ChainId::Testnet => "testnet",
            ChainId::Testnet4 => "testnet4",
            ChainId::Signet => "signet",
            ChainId::Regtest => "regtest",
            ChainId::BitcoinBlake2b => "bitcoin-blake2b",
            ChainId::BitcoinBlake2bTestnet4 => "bitcoin-blake2b-testnet4",
        }
    }

    /// Parses a Connect network string. Unknown strings are `None`, never a
    /// default: a Cube record from a newer server must not be filed under
    /// mainnet because this build didn't recognise its chain.
    pub fn from_api_str(s: &str) -> Option<ChainId> {
        ChainId::ALL.iter().copied().find(|c| c.api_str() == s)
    }

    /// The segment under the data directory that holds this chain's
    /// `settings.json` and wallets. Equal to what `bitcoin::Network`'s
    /// `Display` produced for the Bitcoin family, so existing installs keep
    /// their paths; the fork variants get their own directories, so BTCB2
    /// state never lands in — or is read from — `bitcoin/`.
    pub fn dir_name(self) -> &'static str {
        match self {
            ChainId::Bitcoin => "bitcoin",
            ChainId::Testnet => "testnet",
            ChainId::Testnet4 => "testnet4",
            ChainId::Signet => "signet",
            ChainId::Regtest => "regtest",
            ChainId::BitcoinBlake2b => "bitcoin-blake2b",
            ChainId::BitcoinBlake2bTestnet4 => "bitcoin-blake2b-testnet4",
        }
    }

    /// Parses a data-directory segment (the same strings as the serde form).
    pub fn from_dir_name(s: &str) -> Option<ChainId> {
        ChainId::ALL.iter().copied().find(|c| c.dir_name() == s)
    }

    /// True for either Bitcoin Blake2b identity.
    pub fn is_blake2b(self) -> bool {
        matches!(
            self,
            ChainId::BitcoinBlake2b | ChainId::BitcoinBlake2bTestnet4
        )
    }
}

/// The safe direction: a caller that holds a `bitcoin::Network` is, by
/// construction, on a Bitcoin-family flow (nothing produces a `Network` from
/// a BTCB2 identity except the explicit, lossy [`ChainId::bitcoin_network`]).
impl From<Network> for ChainId {
    fn from(network: Network) -> Self {
        match network {
            Network::Bitcoin => ChainId::Bitcoin,
            Network::Testnet => ChainId::Testnet,
            Network::Testnet4 => ChainId::Testnet4,
            Network::Signet => ChainId::Signet,
            Network::Regtest => ChainId::Regtest,
        }
    }
}

impl fmt::Display for ChainId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.dir_name())
    }
}

/// Error for an identifier this build does not know.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UnknownChainId(pub String);

impl fmt::Display for UnknownChainId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "unknown chain identifier: {:?}", self.0)
    }
}

impl std::error::Error for UnknownChainId {}

impl FromStr for ChainId {
    type Err = UnknownChainId;

    /// Accepts the directory / serde strings only (`bitcoin`, `testnet4`,
    /// `bitcoin-blake2b`, …). Connect strings go through
    /// [`ChainId::from_api_str`]; the two alphabets overlap except for
    /// `mainnet`, which is deliberately not accepted here so a directory can
    /// never be named after an API string.
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        ChainId::from_dir_name(s).ok_or_else(|| UnknownChainId(s.to_string()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The five identities that pre-date the fork: the ones every existing
    /// `settings.json` and data directory was written with.
    fn bitcoin_family() -> impl Iterator<Item = ChainId> {
        ChainId::ALL.iter().copied().filter(|c| !c.is_blake2b())
    }

    #[test]
    fn bitcoin_family_serialises_exactly_like_bitcoin_network() {
        // What every existing settings.json already contains.
        let mut seen = 0;
        for chain in bitcoin_family() {
            let ours = serde_json::to_string(&chain).unwrap();
            let theirs = serde_json::to_string(&chain.bitcoin_network()).unwrap();
            assert_eq!(ours, theirs, "{:?}", chain);
            assert_eq!(chain.dir_name(), chain.bitcoin_network().to_string());
            // Round trip through both the serde and the string forms.
            assert_eq!(serde_json::from_str::<ChainId>(&ours).unwrap(), chain);
            assert_eq!(chain.dir_name().parse::<ChainId>().unwrap(), chain);
            seen += 1;
        }
        assert_eq!(seen, 5);
    }

    #[test]
    fn bitcoin_family_strings_are_pinned_byte_for_byte() {
        // The exact bytes older builds wrote and still expect to read. A
        // rename of any variant, spelling or Connect string is a wire break.
        for (chain, json, api) in [
            (ChainId::Bitcoin, "\"bitcoin\"", "mainnet"),
            (ChainId::Testnet, "\"testnet\"", "testnet"),
            (ChainId::Testnet4, "\"testnet4\"", "testnet4"),
            (ChainId::Signet, "\"signet\"", "signet"),
            (ChainId::Regtest, "\"regtest\"", "regtest"),
        ] {
            assert_eq!(serde_json::to_string(&chain).unwrap(), json);
            assert_eq!(format!("\"{}\"", chain.dir_name()), json);
            assert_eq!(chain.to_string(), chain.dir_name());
            assert_eq!(chain.api_str(), api);
            assert_eq!(ChainId::from_api_str(api), Some(chain));
        }
    }

    #[test]
    fn fork_variants_are_distinct_and_refused_by_the_legacy_type() {
        for (chain, s) in [
            (ChainId::BitcoinBlake2b, "\"bitcoin-blake2b\""),
            (
                ChainId::BitcoinBlake2bTestnet4,
                "\"bitcoin-blake2b-testnet4\"",
            ),
        ] {
            assert_eq!(serde_json::to_string(&chain).unwrap(), s);
            assert_eq!(serde_json::from_str::<ChainId>(s).unwrap(), chain);
            // An older build deserialises `network` as `bitcoin::Network`; it
            // must refuse the fork string rather than read it as mainnet.
            assert!(
                serde_json::from_str::<Network>(s).is_err(),
                "legacy type accepted {}",
                s
            );
        }
    }

    #[test]
    fn unknown_identifiers_are_rejected_never_defaulted() {
        for s in [
            "",
            "mainnet",
            "Bitcoin",
            "bitcoin-blake2b-signet",
            "btcb2",
            "bitcoinblake2b",
        ] {
            assert!(s.parse::<ChainId>().is_err(), "{:?}", s);
            assert_eq!(
                s.parse::<ChainId>().unwrap_err(),
                UnknownChainId(s.to_string())
            );
            assert!(
                serde_json::from_str::<ChainId>(&format!("{:?}", s)).is_err(),
                "{:?}",
                s
            );
            assert!(ChainId::from_dir_name(s).is_none(), "{:?}", s);
        }
        for s in ["", "bitcoin", "Mainnet", "btcb2", "bitcoin_blake2b"] {
            assert!(ChainId::from_api_str(s).is_none(), "{:?}", s);
        }
        assert_eq!(ChainId::from_api_str("mainnet"), Some(ChainId::Bitcoin));
        assert_eq!(
            ChainId::from_api_str("bitcoin-blake2b"),
            Some(ChainId::BitcoinBlake2b)
        );
        assert_eq!(
            ChainId::from_api_str("bitcoin-blake2b-testnet4"),
            Some(ChainId::BitcoinBlake2bTestnet4)
        );
        assert_eq!(
            UnknownChainId("btcb2".to_string()).to_string(),
            "unknown chain identifier: \"btcb2\""
        );
    }

    #[test]
    fn projections() {
        assert_eq!(ChainId::BitcoinBlake2b.bitcoin_network(), Network::Bitcoin);
        assert_eq!(
            ChainId::BitcoinBlake2bTestnet4.bitcoin_network(),
            Network::Testnet4
        );
        assert_eq!(ChainId::BitcoinBlake2b.api_str(), "bitcoin-blake2b");
        assert_eq!(ChainId::BitcoinBlake2b.dir_name(), "bitcoin-blake2b");
        assert!(ChainId::BitcoinBlake2b.is_blake2b());
        assert!(ChainId::BitcoinBlake2bTestnet4.is_blake2b());
        assert!(bitcoin_family().all(|c| !c.is_blake2b()));
        // The projection is lossy and the reverse never reaches a fork variant.
        for chain in ChainId::ALL {
            let back = ChainId::from(chain.bitcoin_network());
            assert!(!back.is_blake2b(), "{:?} round-tripped to a fork id", chain);
            if !chain.is_blake2b() {
                assert_eq!(back, chain);
            }
        }
    }

    #[test]
    fn every_identity_is_distinct_on_every_axis_that_matters_for_storage() {
        use std::collections::HashSet;
        let dirs: HashSet<_> = ChainId::ALL.iter().map(|c| c.dir_name()).collect();
        let apis: HashSet<_> = ChainId::ALL.iter().map(|c| c.api_str()).collect();
        assert_eq!(dirs.len(), ChainId::ALL.len());
        assert_eq!(apis.len(), ChainId::ALL.len());
        // A fork variant never shares a directory or API string with its
        // encoding twin.
        assert_ne!(
            ChainId::BitcoinBlake2b.dir_name(),
            ChainId::Bitcoin.dir_name()
        );
        assert_ne!(
            ChainId::BitcoinBlake2b.api_str(),
            ChainId::Bitcoin.api_str()
        );
    }
}
