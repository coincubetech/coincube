//! Local signing boundary for a validated imported Bitcoin wallet.
//!
//! The caller retains the original daemon-created PSBT and a transient imported
//! root key. Neither the Cube's master signer nor Vault's Taproot/P2WSH-only
//! signing method is an authority for an imported descriptor.

use coincube_core::{
    imported_spend::verify_imported_signatures,
    miniscript::{
        bitcoin::{
            bip32::{ChildNumber, DerivationPath, Fingerprint, Xpriv, Xpub},
            psbt::{GetKey, KeyRequest, Psbt},
            secp256k1::{self, Secp256k1},
            sighash::EcdsaSighashType,
            PrivateKey,
        },
        DescriptorPublicKey, ForEachKey,
    },
    wallet_descriptor::ImportedWalletDescriptor,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ImportedSignError {
    UnsupportedChain,
    InvalidConstruction,
    KeyDoesNotMatchWallet,
    SigningFailed,
    NoSignatureAdded,
    InvalidSignature,
}

impl std::fmt::Display for ImportedSignError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::UnsupportedChain => "Imported signing is available only on Bitcoin chains.",
            Self::InvalidConstruction => "The imported spend no longer matches its original draft.",
            Self::KeyDoesNotMatchWallet => "This key does not match an imported wallet account.",
            Self::SigningFailed => "The imported spend could not be signed.",
            Self::NoSignatureAdded => "This key did not sign any imported wallet input.",
            Self::InvalidSignature => "The imported signature did not verify against the wallet.",
        })
    }
}

impl std::error::Error for ImportedSignError {}

/// An exact descriptor account binding. A four-byte master fingerprint alone
/// never grants authority: the full origin path and derived account xpub must
/// match. The wallet parser has already checked that both branches use the
/// same account keys and end in `/0/*` and `/1/*`.
struct AllowedOrigin {
    fingerprint: Fingerprint,
    account_path: DerivationPath,
}

struct ScopedKey<'a> {
    root: &'a Xpriv,
    origins: Vec<AllowedOrigin>,
}

impl GetKey for ScopedKey<'_> {
    type Error = secp256k1::Error;

    fn get_key<C: secp256k1::Signing>(
        &self,
        request: KeyRequest,
        secp: &Secp256k1<C>,
    ) -> Result<Option<PrivateKey>, Self::Error> {
        let KeyRequest::Bip32((fingerprint, path)) = request else {
            return Ok(None);
        };
        let allowed = self.origins.iter().any(|origin| {
            fingerprint == origin.fingerprint
                && path.as_ref().starts_with(origin.account_path.as_ref())
                && path.len() == origin.account_path.len() + 2
                && matches!(
                    path[origin.account_path.len()],
                    ChildNumber::Normal { index: 0 | 1 }
                )
                && path[origin.account_path.len() + 1].is_normal()
        });
        if !allowed {
            return Ok(None);
        }
        // The path is bounded to one descriptor account and an ordinary
        // receive/change index. The caller owns the short-lived root key.
        self.root
            .derive_priv(secp, &path)
            .map(|key| Some(key.to_priv()))
            .map_err(|_| secp256k1::Error::InvalidSecretKey)
    }
}

fn bind_key<C: secp256k1::Signing>(
    wallet: &ImportedWalletDescriptor,
    root: &Xpriv,
    secp: &Secp256k1<C>,
) -> Result<Vec<AllowedOrigin>, ImportedSignError> {
    let fingerprint = root.fingerprint(secp);
    let mut origins = Vec::new();
    wallet.receive_descriptor().for_each_key(|key| {
        if let DescriptorPublicKey::XPub(xpub) = key {
            if let Some((origin_fingerprint, path)) = &xpub.origin {
                if *origin_fingerprint == fingerprint
                    && root
                        .derive_priv(secp, path)
                        .ok()
                        .map(|account| Xpub::from_priv(secp, &account) == xpub.xkey)
                        .unwrap_or(false)
                {
                    origins.push(AllowedOrigin {
                        fingerprint,
                        account_path: path.clone(),
                    });
                }
            }
        }
        true
    });
    if origins.is_empty() {
        return Err(ImportedSignError::KeyDoesNotMatchWallet);
    }
    Ok(origins)
}

/// Add only signatures controlled by `root` to an already verified imported
/// draft. The original PSBT is immutable; bitcoin's generic signer can mutate
/// an input before returning an error, so all work happens on a clone. A caller
/// must keep the returned PSBT tied to this same wallet and original draft.
pub(crate) fn sign_imported_psbt(
    wallet: &ImportedWalletDescriptor,
    original: &Psbt,
    current: &Psbt,
    root: &Xpriv,
) -> Result<Psbt, ImportedSignError> {
    if wallet.chain().is_blake2b() {
        return Err(ImportedSignError::UnsupportedChain);
    }
    if current.inputs.iter().any(|input| {
        input
            .sighash_type
            .is_some_and(|kind| kind != EcdsaSighashType::All.into())
    }) {
        return Err(ImportedSignError::InvalidConstruction);
    }
    let secp = Secp256k1::new();
    verify_imported_signatures(wallet, original, current, &secp)
        .map_err(|_| ImportedSignError::InvalidConstruction)?;
    let key = ScopedKey {
        root,
        origins: bind_key(wallet, root, &secp)?,
    };
    let mut signed = current.clone();
    signed
        .sign(&key, &secp)
        .map_err(|_| ImportedSignError::SigningFailed)?;
    let added = signed
        .inputs
        .iter()
        .zip(&current.inputs)
        .any(|(after, before)| after.partial_sigs.len() > before.partial_sigs.len());
    if !added {
        return Err(ImportedSignError::NoSignatureAdded);
    }
    verify_imported_signatures(wallet, original, &signed, &secp)
        .map_err(|_| ImportedSignError::InvalidSignature)?;
    Ok(signed)
}

#[cfg(test)]
mod tests;
