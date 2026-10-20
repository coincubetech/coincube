//! Structural checks for public Bitcoin wallet descriptors shared by Split and import.
//!
//! These checks describe keys and scripts only. A caller must separately decide
//! whether a source is complete and whether a signing route is available.

use std::collections::BTreeSet;

use miniscript::{
    descriptor::{ShInner, Wildcard, WshInner},
    Descriptor, DescriptorPublicKey, ForEachKey, Terminal,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DescriptorShapeError {
    Taproot,
    Unsupported,
}

/// The five ECDSA descriptor shapes currently supported for foreign signing.
/// This deliberately retains Split's fixed-key and originless acceptance.
pub fn check_signable_shape(
    descriptor: &Descriptor<DescriptorPublicKey>,
) -> Result<(), DescriptorShapeError> {
    let supported = match descriptor {
        Descriptor::Tr(_) => return Err(DescriptorShapeError::Taproot),
        Descriptor::Pkh(_) | Descriptor::Wpkh(_) => true,
        Descriptor::Sh(sh) => matches!(sh.as_inner(), ShInner::Wpkh(_)),
        Descriptor::Wsh(wsh) => match wsh.as_inner() {
            WshInner::SortedMulti(_) => true,
            WshInner::Ms(ms) => matches!(ms.as_inner(), Terminal::Multi(_)),
        },
        Descriptor::Bare(_) => false,
    };
    let keys_ok = descriptor.for_each_key(|key| match key {
        DescriptorPublicKey::Single(_) => true,
        DescriptorPublicKey::XPub(xpub) => {
            xpub.wildcard != Wildcard::Hardened
                && xpub
                    .derivation_path
                    .as_ref()
                    .iter()
                    .all(|n| !n.is_hardened())
        }
        DescriptorPublicKey::MultiXPub(_) => false,
    });
    if !supported || !keys_ok || descriptor.sanity_check().is_err() {
        return Err(DescriptorShapeError::Unsupported);
    }
    Ok(())
}

fn keys(descriptor: &Descriptor<DescriptorPublicKey>) -> Vec<DescriptorPublicKey> {
    let mut keys = Vec::new();
    descriptor.for_each_key(|key| {
        keys.push(key.clone());
        true
    });
    keys
}

/// Repeated account keys cannot be distinguished as independent signers.
pub fn has_duplicate_key(descriptor: &Descriptor<DescriptorPublicKey>) -> bool {
    let mut seen = BTreeSet::new();
    !keys(descriptor).iter().all(|key| {
        let material = match key {
            DescriptorPublicKey::XPub(xpub) => xpub.xkey.to_string(),
            DescriptorPublicKey::MultiXPub(xpub) => xpub.xkey.to_string(),
            DescriptorPublicKey::Single(single) => match &single.key {
                miniscript::descriptor::SinglePubKey::FullKey(key) => key.to_string(),
                miniscript::descriptor::SinglePubKey::XOnly(key) => key.to_string(),
            },
        };
        seen.insert(material)
    })
}

/// Preserve Split's exact structural pair rule. Import can impose additional
/// requirements (a present change branch, ranged keys and explicit /0 and /1)
/// without changing Split's receive-only or nonstandard-branch acceptance.
pub fn same_wallet(
    external: &Descriptor<DescriptorPublicKey>,
    internal: &Descriptor<DescriptorPublicKey>,
) -> bool {
    let (outer, inner) = (keys(external), keys(internal));
    if outer.len() != inner.len() {
        return false;
    }
    let paired = outer.iter().zip(&inner).all(|pair| match pair {
        (DescriptorPublicKey::Single(a), DescriptorPublicKey::Single(b)) => a == b,
        (DescriptorPublicKey::XPub(a), DescriptorPublicKey::XPub(b)) => {
            let (pa, pb) = (a.derivation_path.as_ref(), b.derivation_path.as_ref());
            a.origin == b.origin
                && a.xkey == b.xkey
                && a.wildcard == b.wildcard
                && !pa.is_empty()
                && pa.len() == pb.len()
                && pa[..pa.len() - 1] == pb[..pb.len() - 1]
        }
        _ => false,
    });
    if !paired {
        return false;
    }
    // Every extended key moves between the same pair of branch steps.
    let branch_steps: BTreeSet<_> = outer
        .iter()
        .zip(&inner)
        .filter_map(|pair| match pair {
            (DescriptorPublicKey::XPub(a), DescriptorPublicKey::XPub(b)) => Some((
                a.derivation_path.as_ref().last().copied(),
                b.derivation_path.as_ref().last().copied(),
            )),
            _ => None,
        })
        .collect();
    if branch_steps.len() > 1 {
        return false;
    }
    // Same script structure after substituting the paired keys.
    let body = |d: &Descriptor<DescriptorPublicKey>| {
        let text = d.to_string();
        text.split('#').next().unwrap_or_default().to_owned()
    };
    let mut renamed = body(external);
    for (index, key) in outer.iter().enumerate() {
        renamed = renamed.replace(&key.to_string(), &format!("\u{0}{index}\u{0}"));
    }
    for (index, key) in inner.iter().enumerate() {
        renamed = renamed.replace(&format!("\u{0}{index}\u{0}"), &key.to_string());
    }
    renamed == body(internal)
}
