//! Script-preserving receive/change descriptor canonicalization shared by
//! Split and imported wallets. No persistence or spend authority is granted.

use miniscript::{
    bitcoin::bip32::{DerivationPath, Fingerprint},
    descriptor::{ShInner, WshInner},
    Descriptor, DescriptorPublicKey, ForEachKey, Terminal,
};
use std::str::FromStr;

const CHECKED_INDICES: [u32; 3] = [0, 1, 1_000];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PairError {
    WrongBranch,
    Taproot,
    Unsupported,
    DuplicateKey,
    ReorderedMulti,
    BranchInXpub,
    ConflictingOrigin,
    Unrelated,
}

pub fn check_shape(descriptor: &Descriptor<DescriptorPublicKey>) -> Result<(), PairError> {
    match shape(descriptor) {
        Some(_) => Ok(()),
        None if matches!(descriptor, Descriptor::Tr(_)) => Err(PairError::Taproot),
        None => Err(PairError::Unsupported),
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Shape {
    Pkh,
    Wpkh,
    ShWpkh,
    Multi(usize),
    SortedMulti(usize),
}

fn shape(descriptor: &Descriptor<DescriptorPublicKey>) -> Option<Shape> {
    match descriptor {
        Descriptor::Pkh(_) => Some(Shape::Pkh),
        Descriptor::Wpkh(_) => Some(Shape::Wpkh),
        Descriptor::Sh(sh) => matches!(sh.as_inner(), ShInner::Wpkh(_)).then_some(Shape::ShWpkh),
        Descriptor::Wsh(wsh) => match wsh.as_inner() {
            WshInner::SortedMulti(multi) => Some(Shape::SortedMulti(multi.k())),
            WshInner::Ms(ms) => match ms.as_inner() {
                Terminal::Multi(thresh) => Some(Shape::Multi(thresh.k())),
                _ => None,
            },
        },
        Descriptor::Bare(_) | Descriptor::Tr(_) => None,
    }
}

fn keys(descriptor: &Descriptor<DescriptorPublicKey>) -> Vec<DescriptorPublicKey> {
    let mut keys = Vec::new();
    descriptor.for_each_key(|key| {
        keys.push(key.clone());
        true
    });
    keys
}

/// The key material a key expression commits to, ignoring its origin and
/// derivation suffix: the extended key, or the single public key.
fn material(key: &DescriptorPublicKey) -> String {
    match key {
        DescriptorPublicKey::XPub(xpub) => xpub.xkey.to_string(),
        DescriptorPublicKey::Single(single) => match &single.key {
            miniscript::descriptor::SinglePubKey::FullKey(key) => key.to_string(),
            miniscript::descriptor::SinglePubKey::XOnly(key) => key.to_string(),
        },
        DescriptorPublicKey::MultiXPub(xpub) => xpub.xkey.to_string(),
    }
}

pub fn has_duplicate_xkey(descriptor: &Descriptor<DescriptorPublicKey>) -> bool {
    let mut seen = std::collections::BTreeSet::new();
    !keys(descriptor)
        .iter()
        .all(|key| seen.insert(material(key)))
}

type Origin = Option<(Fingerprint, DerivationPath)>;

fn origin(key: &DescriptorPublicKey) -> &Origin {
    match key {
        DescriptorPublicKey::XPub(xpub) => &xpub.origin,
        DescriptorPublicKey::Single(single) => &single.origin,
        DescriptorPublicKey::MultiXPub(xpub) => &xpub.origin,
    }
}

fn set_origin(key: &mut DescriptorPublicKey, value: Origin) {
    match key {
        DescriptorPublicKey::XPub(xpub) => xpub.origin = value,
        DescriptorPublicKey::Single(single) => single.origin = value,
        DescriptorPublicKey::MultiXPub(xpub) => xpub.origin = value,
    }
}

/// Canonicalize a receive/change pair so the core's key-by-key comparison
/// sees one wallet, or refuse with the specific reason.
pub fn canonical_pair(
    external: &Descriptor<DescriptorPublicKey>,
    internal: &Descriptor<DescriptorPublicKey>,
) -> Result<
    (
        Descriptor<DescriptorPublicKey>,
        Descriptor<DescriptorPublicKey>,
    ),
    PairError,
> {
    let (outer_shape, inner_shape) = (
        shape(external).ok_or(PairError::Unsupported)?,
        shape(internal).ok_or(PairError::Unsupported)?,
    );
    let (mut outer, mut inner) = (keys(external), keys(internal));
    if outer_shape != inner_shape || outer.len() != inner.len() {
        return Err(PairError::Unrelated);
    }
    let materials = |keys: &[DescriptorPublicKey]| keys.iter().map(material).collect::<Vec<_>>();
    match outer_shape {
        // The script sorts derived keys, so textual order is irrelevant.
        Shape::SortedMulti(_) => {
            outer.sort_by_key(material);
            inner.sort_by_key(material);
        }
        Shape::Multi(_) => {
            let (mut a, mut b) = (materials(&outer), materials(&inner));
            if a != b {
                a.sort();
                b.sort();
                if a == b {
                    return Err(PairError::ReorderedMulti);
                }
            }
        }
        Shape::Pkh | Shape::Wpkh | Shape::ShWpkh => {}
    }
    for (a, b) in outer.iter_mut().zip(inner.iter_mut()) {
        if material(a) != material(b) {
            return Err(if branch_in_xpub(a, b) {
                PairError::BranchInXpub
            } else {
                PairError::Unrelated
            });
        }
        match (origin(a).clone(), origin(b).clone()) {
            (Some(x), None) => set_origin(b, Some(x)),
            (None, Some(y)) => set_origin(a, Some(y)),
            (Some(x), Some(y)) if x != y => return Err(PairError::ConflictingOrigin),
            _ => {}
        }
    }
    let canonical_external = rebuild(outer_shape, &outer)?;
    let canonical_internal = rebuild(inner_shape, &inner)?;
    if !same_scripts(external, &canonical_external) || !same_scripts(internal, &canonical_internal)
    {
        return Err(PairError::Unrelated);
    }
    Ok((canonical_external, canonical_internal))
}

/// Two sibling extended keys at the same depth under the same parent
/// fingerprint, each used with no derivation step before the wildcard.
fn branch_in_xpub(a: &DescriptorPublicKey, b: &DescriptorPublicKey) -> bool {
    match (a, b) {
        (DescriptorPublicKey::XPub(a), DescriptorPublicKey::XPub(b)) => {
            a.derivation_path.is_empty()
                && b.derivation_path.is_empty()
                && a.xkey.depth == b.xkey.depth
                && a.xkey.parent_fingerprint == b.xkey.parent_fingerprint
                && a.xkey.child_number != b.xkey.child_number
        }
        _ => false,
    }
}

fn rebuild(
    shape: Shape,
    keys: &[DescriptorPublicKey],
) -> Result<Descriptor<DescriptorPublicKey>, PairError> {
    let list = || {
        keys.iter()
            .map(ToString::to_string)
            .collect::<Vec<_>>()
            .join(",")
    };
    let single = || keys.first().map(ToString::to_string).unwrap_or_default();
    let text = match shape {
        Shape::Pkh => format!("pkh({})", single()),
        Shape::Wpkh => format!("wpkh({})", single()),
        Shape::ShWpkh => format!("sh(wpkh({}))", single()),
        Shape::Multi(k) => format!("wsh(multi({k},{}))", list()),
        Shape::SortedMulti(k) => format!("wsh(sortedmulti({k},{}))", list()),
    };
    Descriptor::from_str(&text).map_err(|_| PairError::Unsupported)
}

pub fn same_scripts(
    a: &Descriptor<DescriptorPublicKey>,
    b: &Descriptor<DescriptorPublicKey>,
) -> bool {
    a.has_wildcard() == b.has_wildcard()
        && CHECKED_INDICES.iter().all(|index| {
            match (a.at_derivation_index(*index), b.at_derivation_index(*index)) {
                (Ok(a), Ok(b)) => a.script_pubkey() == b.script_pubkey(),
                _ => false,
            }
        })
}
