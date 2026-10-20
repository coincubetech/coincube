//! Public standard single-sig account descriptors from a checked mainnet xpub.
//! This module provides no spending authority or persistent signer.

use coincube_core::miniscript::bitcoin::{
    bip32::{ChildNumber, Fingerprint, Xpub},
    NetworkKind,
};

use super::foreign_scan::{Branch, ScanDescriptor, ScanError};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StandardSinglesig {
    Bip44,
    Bip49,
    Bip84,
}

impl StandardSinglesig {
    pub(crate) fn purpose(self) -> u32 {
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
