//! Build a Split step-1 [`SplitSource`] from the scanned public descriptors.
//!
//! `coincube_core::foreign_split::SplitSource` pairs the receive and change
//! descriptors key by key and refuses any pair that differs in more than each
//! key's final branch step. Some genuine exports of one wallet fail that
//! literal comparison (#568, finding I2). This module either canonicalizes
//! them, when the rewrite keeps every derived script by construction, or refuses with
//! a message that says what to export instead:
//!
//! - `sortedmulti` keys listed in another order: canonicalized. The script
//!   sorts the derived public keys, so key order in the text never changes an
//!   address. Plain `multi` keys in another order are a different wallet.
//! - Origin information on only one of the two descriptors: canonicalized by
//!   copying the origin of the identical extended key. Origins are signer
//!   metadata; they never change a script. Two different origins for the same
//!   key are refused.
//! - The receive/change branch already applied to the extended key (two
//!   sibling xpubs with no branch step): refused. Their common parent cannot
//!   be proven from the children, so they cannot be paired safely.
//! - The same extended key used twice in one descriptor: refused.
//!
//! Both rewrites preserve every script by construction. As defense in depth,
//! each canonicalized descriptor is also checked to derive the same scripts as
//! the descriptor it replaces at three indices before the core check runs. Nothing here reads a
//! chain or grants spend authority.

use std::{fmt, str::FromStr};

use coincube_core::{
    foreign_split::{Error as CoreError, SplitSource},
    miniscript::{
        bitcoin::bip32::{DerivationPath, Fingerprint},
        descriptor::{ShInner, WshInner},
        Descriptor, DescriptorPublicKey, ForEachKey, Terminal,
    },
};

use super::foreign_scan::{Branch, ScanDescriptor};

/// Indices spot-checked after canonicalizing (not a proof; see module docs).
const CHECKED_INDICES: [u32; 3] = [0, 1, 1_000];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SourceError {
    /// A descriptor was given for the wrong branch.
    WrongBranch,
    /// Taproot is scan-only in Split.
    Taproot,
    /// Not a shape Split can sign.
    Unsupported,
    /// The same extended key appears twice in one descriptor.
    DuplicateKey,
    /// `multi` keys in a different order: a different wallet.
    ReorderedMulti,
    /// Each descriptor's key already has the branch applied.
    BranchInXpub,
    /// The same key carries two different origins.
    ConflictingOrigin,
    /// The change descriptor is not the receive descriptor's wallet.
    Unrelated,
}

impl fmt::Display for SourceError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::WrongBranch => {
                "The receive and change descriptors were given in the wrong fields."
            }
            Self::Taproot => {
                "Taproot wallets can be scanned but not split. Move the coins with the wallet that holds them."
            }
            Self::Unsupported => {
                "Split supports pkh, wpkh, sh(wpkh), wsh(multi) and wsh(sortedmulti) wallets only."
            }
            Self::DuplicateKey => {
                "The same extended public key appears twice in this descriptor. Split cannot tell those signers apart; export the descriptor with one key per signer."
            }
            Self::ReorderedMulti => {
                "The change descriptor lists the multi() keys in a different order. In multi() the order changes every address, so these are two different wallets. Export both descriptors from the same wallet."
            }
            Self::BranchInXpub => {
                "Each descriptor uses a different extended key with no branch step, so the receive or change branch is already built into the key. Export the account-level key with explicit /0/* and /1/* branches."
            }
            Self::ConflictingOrigin => {
                "The same key has a different fingerprint or derivation path in the two descriptors. Export both descriptors from the same wallet."
            }
            Self::Unrelated => {
                "The change descriptor does not belong to the same wallet: it must match the receive descriptor except for the final branch step of each key."
            }
        })
    }
}

impl std::error::Error for SourceError {}

/// The step-1 source for the scanned wallet, canonicalized only by the two
/// script-preserving rewrites in the module documentation.
pub fn split_source(
    external: &ScanDescriptor,
    internal: Option<&ScanDescriptor>,
) -> Result<SplitSource, SourceError> {
    if external.branch() != Branch::External
        || internal.is_some_and(|internal| internal.branch() != Branch::Internal)
    {
        return Err(SourceError::WrongBranch);
    }
    let (external, internal) = (
        external.descriptor(),
        internal.map(ScanDescriptor::descriptor),
    );
    for descriptor in std::iter::once(external).chain(internal) {
        check_shape(descriptor)?;
        if has_duplicate_xkey(descriptor) {
            return Err(SourceError::DuplicateKey);
        }
    }
    let (external, internal) = match internal {
        Some(internal) => {
            let (a, b) = canonical_pair(external, internal)?;
            (a, Some(b))
        }
        None => (external.clone(), None),
    };
    SplitSource::new(external, internal).map_err(|error| match error {
        CoreError::Taproot => SourceError::Taproot,
        CoreError::UnrelatedInternal => SourceError::Unrelated,
        _ => SourceError::Unsupported,
    })
}

fn check_shape(descriptor: &Descriptor<DescriptorPublicKey>) -> Result<(), SourceError> {
    match shape(descriptor) {
        Some(_) => Ok(()),
        None if matches!(descriptor, Descriptor::Tr(_)) => Err(SourceError::Taproot),
        None => Err(SourceError::Unsupported),
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
            coincube_core::miniscript::descriptor::SinglePubKey::FullKey(key) => key.to_string(),
            coincube_core::miniscript::descriptor::SinglePubKey::XOnly(key) => key.to_string(),
        },
        DescriptorPublicKey::MultiXPub(xpub) => xpub.xkey.to_string(),
    }
}

fn has_duplicate_xkey(descriptor: &Descriptor<DescriptorPublicKey>) -> bool {
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
fn canonical_pair(
    external: &Descriptor<DescriptorPublicKey>,
    internal: &Descriptor<DescriptorPublicKey>,
) -> Result<
    (
        Descriptor<DescriptorPublicKey>,
        Descriptor<DescriptorPublicKey>,
    ),
    SourceError,
> {
    let (outer_shape, inner_shape) = (
        shape(external).ok_or(SourceError::Unsupported)?,
        shape(internal).ok_or(SourceError::Unsupported)?,
    );
    let (mut outer, mut inner) = (keys(external), keys(internal));
    if outer_shape != inner_shape || outer.len() != inner.len() {
        return Err(SourceError::Unrelated);
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
                    return Err(SourceError::ReorderedMulti);
                }
            }
        }
        Shape::Pkh | Shape::Wpkh | Shape::ShWpkh => {}
    }
    for (a, b) in outer.iter_mut().zip(inner.iter_mut()) {
        if material(a) != material(b) {
            return Err(if branch_in_xpub(a, b) {
                SourceError::BranchInXpub
            } else {
                SourceError::Unrelated
            });
        }
        match (origin(a).clone(), origin(b).clone()) {
            (Some(x), None) => set_origin(b, Some(x)),
            (None, Some(y)) => set_origin(a, Some(y)),
            (Some(x), Some(y)) if x != y => return Err(SourceError::ConflictingOrigin),
            _ => {}
        }
    }
    let canonical_external = rebuild(outer_shape, &outer)?;
    let canonical_internal = rebuild(inner_shape, &inner)?;
    if !same_scripts(external, &canonical_external) || !same_scripts(internal, &canonical_internal)
    {
        return Err(SourceError::Unrelated);
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
) -> Result<Descriptor<DescriptorPublicKey>, SourceError> {
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
    Descriptor::from_str(&text).map_err(|_| SourceError::Unsupported)
}

fn same_scripts(a: &Descriptor<DescriptorPublicKey>, b: &Descriptor<DescriptorPublicKey>) -> bool {
    a.has_wildcard() == b.has_wildcard()
        && CHECKED_INDICES.iter().all(|index| {
            match (a.at_derivation_index(*index), b.at_derivation_index(*index)) {
                (Ok(a), Ok(b)) => a.script_pubkey() == b.script_pubkey(),
                _ => false,
            }
        })
}

#[cfg(test)]
mod tests {
    use super::*;
    use coincube_core::miniscript::bitcoin::{
        bip32::{Xpriv, Xpub},
        secp256k1::Secp256k1,
        Network,
    };

    fn xpriv(seed: u8) -> Xpriv {
        Xpriv::new_master(Network::Bitcoin, &[seed; 32]).unwrap()
    }

    /// `[fp/path]xpub` for an account of `seed`.
    fn account(seed: u8, path: &str) -> (String, String) {
        let secp = Secp256k1::new();
        let master = xpriv(seed);
        let child = master
            .derive_priv(&secp, &DerivationPath::from_str(path).unwrap())
            .unwrap();
        (
            format!(
                "[{}/{}]",
                master.fingerprint(&secp),
                path.trim_start_matches("m/")
            ),
            Xpub::from_priv(&secp, &child).to_string(),
        )
    }

    fn parse(branch: Branch, text: &str) -> ScanDescriptor {
        ScanDescriptor::parse(branch, text).unwrap()
    }

    fn scripts(source: &SplitSource, text: &str, internal: bool) {
        let original = Descriptor::<DescriptorPublicKey>::from_str(text).unwrap();
        let canonical = if internal {
            source.internal().unwrap()
        } else {
            source.external()
        };
        assert!(same_scripts(&original, canonical), "{}", text);
    }

    fn multisig(kind: &str, order: [usize; 3], branch: u32) -> String {
        let keys = [1u8, 2, 3].map(|seed| account(seed, "m/48'/0'/0'/2'"));
        let listed: Vec<_> = order
            .iter()
            .map(|i| format!("{}{}/{branch}/*", keys[*i].0, keys[*i].1))
            .collect();
        format!("wsh({kind}(2,{}))", listed.join(","))
    }

    #[test]
    fn split_source_accepts_matching_pairs_and_receive_only() {
        let (origin, xpub) = account(1, "m/84'/0'/0'");
        let external = parse(Branch::External, &format!("wpkh({origin}{xpub}/0/*)"));
        let internal = parse(Branch::Internal, &format!("wpkh({origin}{xpub}/1/*)"));
        let source = split_source(&external, Some(&internal)).unwrap();
        assert_eq!(
            source.external().to_string(),
            external.descriptor().to_string()
        );
        assert!(split_source(&external, None).unwrap().internal().is_none());
        assert_eq!(
            split_source(&internal, None).err(),
            Some(SourceError::WrongBranch)
        );
    }

    #[test]
    fn split_source_canonicalizes_reordered_sortedmulti_keys() {
        let external = multisig("sortedmulti", [0, 1, 2], 0);
        let internal = multisig("sortedmulti", [2, 0, 1], 1);
        let source = split_source(
            &parse(Branch::External, &external),
            Some(&parse(Branch::Internal, &internal)),
        )
        .unwrap();
        scripts(&source, &external, false);
        scripts(&source, &internal, true);
        // Counterfactual: the core refuses the literal pair.
        assert_eq!(
            SplitSource::new(
                Descriptor::from_str(&external).unwrap(),
                Some(Descriptor::from_str(&internal).unwrap())
            ),
            Err(CoreError::UnrelatedInternal)
        );
    }

    #[test]
    fn split_source_refuses_reordered_multi_keys_with_a_message() {
        let result = split_source(
            &parse(Branch::External, &multisig("multi", [0, 1, 2], 0)),
            Some(&parse(Branch::Internal, &multisig("multi", [1, 0, 2], 1))),
        );
        assert_eq!(result.err(), Some(SourceError::ReorderedMulti));
        assert!(SourceError::ReorderedMulti
            .to_string()
            .contains("different order"));
        // Same order is one wallet.
        assert!(split_source(
            &parse(Branch::External, &multisig("multi", [1, 0, 2], 0)),
            Some(&parse(Branch::Internal, &multisig("multi", [1, 0, 2], 1))),
        )
        .is_ok());
    }

    #[test]
    fn split_source_fills_a_missing_change_origin_from_the_same_key() {
        let (origin, xpub) = account(1, "m/49'/0'/0'");
        let external = format!("sh(wpkh({origin}{xpub}/0/*))");
        for internal in [
            format!("sh(wpkh({xpub}/1/*))"),
            format!("sh(wpkh({origin}{xpub}/1/*))"),
        ] {
            let source = split_source(
                &parse(Branch::External, &external),
                Some(&parse(Branch::Internal, &internal)),
            )
            .unwrap();
            assert!(source.internal().unwrap().to_string().contains(&origin));
            scripts(&source, &internal, true);
        }
        // The origin can be on the change side only, too.
        let source = split_source(
            &parse(Branch::External, &format!("sh(wpkh({xpub}/0/*))")),
            Some(&parse(
                Branch::Internal,
                &format!("sh(wpkh({origin}{xpub}/1/*))"),
            )),
        )
        .unwrap();
        assert!(source.external().to_string().contains(&origin));
        // Two different origins for the same key refuse.
        let (other, _) = account(9, "m/49'/0'/1'");
        assert_eq!(
            split_source(
                &parse(Branch::External, &external),
                Some(&parse(
                    Branch::Internal,
                    &format!("sh(wpkh({other}{xpub}/1/*))")
                )),
            )
            .err(),
            Some(SourceError::ConflictingOrigin)
        );
    }

    #[test]
    fn split_source_refuses_a_branch_built_into_the_xpub() {
        let (_, receive) = account(1, "m/84'/0'/0'/0");
        let (_, change) = account(1, "m/84'/0'/0'/1");
        let result = split_source(
            &parse(Branch::External, &format!("wpkh({receive}/*)")),
            Some(&parse(Branch::Internal, &format!("wpkh({change}/*)"))),
        );
        assert_eq!(result.err(), Some(SourceError::BranchInXpub));
        assert!(SourceError::BranchInXpub
            .to_string()
            .contains("/0/* and /1/*"));
        // An unrelated key is not mistaken for a baked-in branch.
        let (_, stranger) = account(2, "m/84'/0'/0'/1");
        assert_eq!(
            split_source(
                &parse(Branch::External, &format!("wpkh({receive}/*)")),
                Some(&parse(Branch::Internal, &format!("wpkh({stranger}/0/*)"))),
            )
            .err(),
            Some(SourceError::Unrelated)
        );
    }

    #[test]
    fn split_source_refuses_the_same_xpub_twice() {
        let (origin, xpub) = account(1, "m/48'/0'/0'/2'");
        let (_, other) = account(2, "m/48'/0'/0'/2'");
        let text = format!("wsh(sortedmulti(1,{origin}{xpub}/0/*,{xpub}/2/*,{other}/0/*))");
        let result = split_source(&parse(Branch::External, &text), None);
        assert_eq!(result.err(), Some(SourceError::DuplicateKey));
        assert!(SourceError::DuplicateKey.to_string().contains("twice"));
    }

    #[test]
    fn split_source_taproot_and_unrelated_have_messages() {
        let (origin, xpub) = account(1, "m/86'/0'/0'");
        let tr = parse(Branch::External, &format!("tr({origin}{xpub}/0/*)"));
        assert_eq!(split_source(&tr, None).err(), Some(SourceError::Taproot));
        let (origin2, xpub2) = account(2, "m/84'/0'/0'");
        assert_eq!(
            split_source(
                &parse(Branch::External, &format!("wpkh({origin2}{xpub2}/0/*)")),
                Some(&parse(
                    Branch::Internal,
                    &format!("pkh({origin2}{xpub2}/1/*)")
                )),
            )
            .err(),
            Some(SourceError::Unrelated)
        );
        for error in [
            SourceError::WrongBranch,
            SourceError::Taproot,
            SourceError::Unsupported,
            SourceError::DuplicateKey,
            SourceError::ReorderedMulti,
            SourceError::BranchInXpub,
            SourceError::ConflictingOrigin,
            SourceError::Unrelated,
        ] {
            assert!(error.to_string().ends_with('.'), "{:?}", error);
        }
    }
}
