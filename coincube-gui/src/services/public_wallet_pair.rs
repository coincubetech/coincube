//! Public wallet pair checks shared by Split and import.
//! The canonical script-preserving rules live in core; this GUI seam attaches
//! scan branch labels without granting spend or persistence authority.

use coincube_core::{
    miniscript::{Descriptor, DescriptorPublicKey},
    public_wallet_pair::canonical_pair,
    public_wallet_source::same_wallet,
};

use super::foreign_scan::{Branch, ScanDescriptor};

pub use coincube_core::public_wallet_pair::{same_scripts, PairError};

pub struct PublicWalletPair {
    pub external: Descriptor<DescriptorPublicKey>,
    pub internal: Descriptor<DescriptorPublicKey>,
}

pub fn validated_pair(
    external: &ScanDescriptor,
    internal: &ScanDescriptor,
) -> Result<PublicWalletPair, PairError> {
    if external.branch() != Branch::External || internal.branch() != Branch::Internal {
        return Err(PairError::WrongBranch);
    }
    let external = external.descriptor();
    let internal = internal.descriptor();
    for descriptor in [external, internal] {
        check_shape(descriptor)?;
        if has_duplicate_xkey(descriptor) {
            return Err(PairError::DuplicateKey);
        }
    }
    let (external, internal) = canonical_pair(external, internal)?;
    if !same_wallet(&external, &internal) {
        return Err(PairError::Unrelated);
    }
    Ok(PublicWalletPair { external, internal })
}

pub(crate) use coincube_core::public_wallet_pair::{check_shape, has_duplicate_xkey};
