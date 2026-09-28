//! Foreign-wallet descriptor sources for the BTCB2 Split tool.
//!
//! This module prepares public receive/change descriptors only. It exposes no
//! spend, persistence, finalisation or broadcast operation. The hardware path
//! consumes an account xpub exported by the existing HWI layer; the seed path
//! retains a zeroizing [`SessionSigner`] for a later, separately authorized
//! unified-signing step.

use std::str::FromStr;

use coincube_core::{
    bip39::{Language, Mnemonic},
    miniscript::bitcoin::{
        bip32::{ChildNumber, DerivationPath, Fingerprint, Xpub},
        secp256k1::Secp256k1,
        Network, NetworkKind,
    },
    signer::SessionSigner,
};
use zeroize::Zeroizing;

use super::foreign_scan::{Branch, ScanDescriptor, ScanError};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StandardSinglesig {
    Bip44,
    Bip49,
    Bip84,
}

impl StandardSinglesig {
    fn purpose(self) -> u32 {
        match self {
            Self::Bip44 => 44,
            Self::Bip49 => 49,
            Self::Bip84 => 84,
        }
    }

    fn descriptor(self, key: &str) -> String {
        match self {
            Self::Bip44 => format!("pkh({key})"),
            Self::Bip49 => format!("sh(wpkh({key}))"),
            Self::Bip84 => format!("wpkh({key})"),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SourceError {
    Mnemonic,
    Account,
    Network,
    Descriptor,
}

impl From<ScanError> for SourceError {
    fn from(_: ScanError) -> Self {
        Self::Descriptor
    }
}

pub struct AccountDescriptors {
    pub external: ScanDescriptor,
    pub internal: ScanDescriptor,
    pub fingerprint: Fingerprint,
}

/// Public account material returned by a hardware wallet or another
/// session-bound signer. It is authority-free: an xpub can discover addresses
/// but cannot sign.
pub struct AccountXpubSource {
    standard: StandardSinglesig,
    account: u32,
    fingerprint: Fingerprint,
    account_xpub: Xpub,
}

impl AccountXpubSource {
    pub fn new(
        standard: StandardSinglesig,
        account: u32,
        fingerprint: Fingerprint,
        account_xpub: Xpub,
    ) -> Result<Self, SourceError> {
        if account >= (1 << 31) {
            return Err(SourceError::Account);
        }
        if account_xpub.network != NetworkKind::Main {
            return Err(SourceError::Network);
        }
        if account_xpub.depth != 3
            || account_xpub.child_number != ChildNumber::from_hardened_idx(account).unwrap()
        {
            return Err(SourceError::Account);
        }
        Ok(Self {
            standard,
            account,
            fingerprint,
            account_xpub,
        })
    }

    pub fn descriptors(&self) -> Result<AccountDescriptors, SourceError> {
        let origin = format!(
            "[{}/{}h/0h/{}h]{}",
            self.fingerprint,
            self.standard.purpose(),
            self.account,
            self.account_xpub
        );
        let external = self.standard.descriptor(&format!("{origin}/0/*"));
        let internal = self.standard.descriptor(&format!("{origin}/1/*"));
        Ok(AccountDescriptors {
            external: ScanDescriptor::parse(Branch::External, &external)?,
            internal: ScanDescriptor::parse(Branch::Internal, &internal)?,
            fingerprint: self.fingerprint,
        })
    }
}

/// A one-flow BIP39 source. Words and passphrase arrive in zeroizing buffers;
/// after construction only [`SessionSigner`] remains, and it has no persistence
/// API. Dropping this value scrubs the derived xpriv.
pub struct SessionSeedSource {
    signer: SessionSigner,
}

impl SessionSeedSource {
    pub fn new(
        words: Zeroizing<String>,
        passphrase: Zeroizing<String>,
    ) -> Result<Self, SourceError> {
        let mnemonic = Mnemonic::parse_in(Language::English, words.as_str())
            .map_err(|_| SourceError::Mnemonic)?;
        let signer = SessionSigner::from_mnemonic(Network::Bitcoin, mnemonic, passphrase.as_str())
            .map_err(|_| SourceError::Mnemonic)?;
        // Both input buffers are dropped and scrubbed here. Only the
        // session-bound xpriv remains.
        drop(words);
        drop(passphrase);
        Ok(Self { signer })
    }

    pub fn descriptors(
        &self,
        standard: StandardSinglesig,
        account: u32,
    ) -> Result<AccountDescriptors, SourceError> {
        if account >= (1 << 31) {
            return Err(SourceError::Account);
        }
        let path = DerivationPath::from_str(&format!("m/{}h/0h/{}h", standard.purpose(), account))
            .map_err(|_| SourceError::Account)?;
        let secp = Secp256k1::signing_only();
        let fingerprint = self.signer.fingerprint(&secp);
        let account_xpub = self.signer.xpub_at(&path, &secp);
        AccountXpubSource::new(standard, account, fingerprint, account_xpub)?.descriptors()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use coincube_core::miniscript::bitcoin::bip32::Xpriv;
    use std::{
        fs,
        time::{SystemTime, UNIX_EPOCH},
    };

    const WORDS: &str =
        "abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon about";

    fn account_xpub(standard: StandardSinglesig, account: u32) -> (Fingerprint, Xpub) {
        let secp = Secp256k1::new();
        let root = Xpriv::new_master(Network::Bitcoin, &[7; 64]).unwrap();
        let fingerprint = root.fingerprint(&secp);
        let path = DerivationPath::from_str(&format!("m/{}h/0h/{}h", standard.purpose(), account))
            .unwrap();
        let account = root.derive_priv(&secp, &path).unwrap();
        (fingerprint, Xpub::from_priv(&secp, &account))
    }

    #[test]
    fn standard_account_xpubs_cover_bip44_bip49_and_bip84() {
        for standard in [
            StandardSinglesig::Bip44,
            StandardSinglesig::Bip49,
            StandardSinglesig::Bip84,
        ] {
            let (fingerprint, xpub) = account_xpub(standard, 0);
            let descriptors = AccountXpubSource::new(standard, 0, fingerprint, xpub)
                .unwrap()
                .descriptors()
                .unwrap();
            assert_eq!(descriptors.fingerprint, fingerprint);
            assert_eq!(descriptors.external.end_exclusive(100), 100);
            assert_eq!(descriptors.internal.end_exclusive(100), 100);
            assert!(!descriptors.external.capabilities().signing.seed_unified);
            assert!(!descriptors.internal.capabilities().claim_authorization);
        }
    }

    #[test]
    fn account_xpub_source_rejects_testnet_or_wrong_depth() {
        let secp = Secp256k1::new();
        let testnet = Xpriv::new_master(Network::Testnet, &[8; 64]).unwrap();
        let testnet_xpub = Xpub::from_priv(&secp, &testnet);
        assert!(matches!(
            AccountXpubSource::new(
                StandardSinglesig::Bip84,
                0,
                testnet.fingerprint(&secp),
                testnet_xpub
            ),
            Err(SourceError::Network)
        ));

        let mainnet = Xpriv::new_master(Network::Bitcoin, &[9; 64]).unwrap();
        assert!(matches!(
            AccountXpubSource::new(
                StandardSinglesig::Bip84,
                0,
                mainnet.fingerprint(&secp),
                Xpub::from_priv(&secp, &mainnet)
            ),
            Err(SourceError::Account)
        ));
    }

    #[test]
    fn session_seed_derives_public_descriptors_and_never_writes_the_seed() {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let datadir = std::env::temp_dir().join(format!("coincube-split-seed-{unique}"));
        fs::create_dir(&datadir).unwrap();

        let source = SessionSeedSource::new(
            Zeroizing::new(WORDS.to_owned()),
            Zeroizing::new("optional passphrase".to_owned()),
        )
        .unwrap();
        let descriptors = source.descriptors(StandardSinglesig::Bip84, 0).unwrap();
        assert_eq!(descriptors.external.end_exclusive(100), 100);
        assert_eq!(descriptors.internal.end_exclusive(100), 100);
        drop(source);

        assert_eq!(fs::read_dir(&datadir).unwrap().count(), 0);
        fs::remove_dir(datadir).unwrap();
    }

    #[test]
    fn passphrase_nfkd_equivalence_and_separation_bind_fingerprint_and_descriptors() {
        let composed = SessionSeedSource::new(
            Zeroizing::new(WORDS.to_owned()),
            Zeroizing::new("caf\u{e9}".to_owned()),
        )
        .unwrap()
        .descriptors(StandardSinglesig::Bip84, 0)
        .unwrap();
        let decomposed = SessionSeedSource::new(
            Zeroizing::new(WORDS.to_owned()),
            Zeroizing::new("cafe\u{301}".to_owned()),
        )
        .unwrap()
        .descriptors(StandardSinglesig::Bip84, 0)
        .unwrap();
        assert_eq!(composed.fingerprint, decomposed.fingerprint);
        assert_eq!(
            composed.external.canonical(),
            decomposed.external.canonical()
        );
        assert_eq!(
            composed.internal.canonical(),
            decomposed.internal.canonical()
        );

        let source = SessionSeedSource::new(
            Zeroizing::new(WORDS.to_owned()),
            Zeroizing::new(String::new()),
        )
        .unwrap();
        let protected = SessionSeedSource::new(
            Zeroizing::new(WORDS.to_owned()),
            Zeroizing::new("not empty".to_owned()),
        )
        .unwrap();
        assert_ne!(
            source
                .descriptors(StandardSinglesig::Bip84, 0)
                .unwrap()
                .fingerprint,
            protected
                .descriptors(StandardSinglesig::Bip84, 0)
                .unwrap()
                .fingerprint
        );
    }
}
