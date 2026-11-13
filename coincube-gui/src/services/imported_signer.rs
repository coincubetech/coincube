//! GUI delegation to the core's opaque imported-account authority.
//! This service never accepts or exposes a raw master private key.

use coincube_core::{
    imported_keys::VerifiedImportedKey, miniscript::bitcoin::psbt::Psbt,
    wallet_descriptor::ImportedWalletDescriptor,
};

pub use coincube_core::imported_keys::ImportedKeyError as ImportedSignError;

pub(crate) fn sign_imported_psbt(
    wallet: &ImportedWalletDescriptor,
    original: &Psbt,
    current: &Psbt,
    key: &VerifiedImportedKey,
) -> Result<Psbt, ImportedSignError> {
    key.sign(wallet, original, current)
}

#[cfg(test)]
mod tests;
