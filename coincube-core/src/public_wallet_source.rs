//! Structural checks for public Bitcoin wallet descriptors (the imported wallet).
//!
//! These checks describe keys and scripts only. A caller must separately decide
//! whether a source is complete and whether a signing route is available.

use std::{collections::BTreeSet, fmt};

use miniscript::{
    bitcoin,
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

/// Why a pair of public descriptors is not a [`SplitSource`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SourceError {
    /// A `tr` descriptor: Taproot wallets are scan-only.
    Taproot,
    /// Not one of the supported shapes, or keys that cannot be derived
    /// unambiguously (multipath, hardened steps or hardened wildcard).
    UnsupportedDescriptor,
    /// The internal (change) descriptor is not the external descriptor's
    /// wallet: it must differ only in each key's final, branch step.
    UnrelatedInternal,
}

impl fmt::Display for SourceError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Taproot => f.write_str("Taproot wallets can be scanned but not signed for"),
            Self::UnsupportedDescriptor => f.write_str("Unsupported public wallet descriptor"),
            Self::UnrelatedInternal => {
                f.write_str("The change descriptor does not belong to the same wallet")
            }
        }
    }
}
impl std::error::Error for SourceError {}

/// Domain separation for [`SplitSource::digest`]: no other hashed identity
/// can share its preimage. The value is unchanged from the removed foreign
/// Split so existing source digests (lock names, recorded identities) keep
/// their meaning.
pub const SPLIT_SOURCE_DIGEST_TAG: &[u8] = b"coincube/split-source/v1";

/// A public wallet's receive and optional change descriptors, checked against
/// the supported signing shapes ([`check_signable_shape`]) and paired by
/// [`same_wallet`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SplitSource {
    external: Descriptor<DescriptorPublicKey>,
    internal: Option<Descriptor<DescriptorPublicKey>>,
}

impl SplitSource {
    pub fn new(
        external: Descriptor<DescriptorPublicKey>,
        internal: Option<Descriptor<DescriptorPublicKey>>,
    ) -> Result<Self, SourceError> {
        check_shape(&external)?;
        if let Some(internal) = &internal {
            check_shape(internal)?;
            if !same_wallet(&external, internal) {
                return Err(SourceError::UnrelatedInternal);
            }
        }
        Ok(Self { external, internal })
    }

    pub fn external(&self) -> &Descriptor<DescriptorPublicKey> {
        &self.external
    }

    pub fn internal(&self) -> Option<&Descriptor<DescriptorPublicKey>> {
        self.internal.as_ref()
    }

    /// The source identity: SHA-256 over the domain tag
    /// [`SPLIT_SOURCE_DIGEST_TAG`], then each descriptor's canonical text
    /// with its checksum, tagged by branch and length-prefixed so the pair
    /// cannot be re-split. It is not a secret and not evidence of ownership.
    pub fn digest(&self) -> bitcoin::hashes::sha256::Hash {
        use bitcoin::hashes::{Hash, HashEngine};
        let mut engine = bitcoin::hashes::sha256::Hash::engine();
        engine.input(SPLIT_SOURCE_DIGEST_TAG);
        for (tag, descriptor) in [(0_u8, Some(&self.external)), (1, self.internal.as_ref())] {
            engine.input(&[tag]);
            match descriptor {
                Some(descriptor) => {
                    let text = descriptor.to_string();
                    engine.input(&(text.len() as u64).to_be_bytes());
                    engine.input(text.as_bytes());
                }
                None => engine.input(&0_u64.to_be_bytes()),
            }
        }
        bitcoin::hashes::sha256::Hash::from_engine(engine)
    }
}

fn check_shape(descriptor: &Descriptor<DescriptorPublicKey>) -> Result<(), SourceError> {
    check_signable_shape(descriptor).map_err(|error| match error {
        DescriptorShapeError::Taproot => SourceError::Taproot,
        DescriptorShapeError::Unsupported => SourceError::UnsupportedDescriptor,
    })
}

#[cfg(test)]
mod tests {
    //! [`SplitSource`] tests, moved with it from the removed foreign Split.
    use super::*;
    use miniscript::bitcoin::{
        bip32::{DerivationPath, Xpriv, Xpub},
        secp256k1, Network,
    };
    use std::str::FromStr;

    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    enum Shape {
        Wpkh,
        ShWpkh,
        Pkh,
        WshSortedMulti,
        WshMulti,
    }
    const SHAPES: [Shape; 5] = [
        Shape::Wpkh,
        Shape::ShWpkh,
        Shape::Pkh,
        Shape::WshSortedMulti,
        Shape::WshMulti,
    ];

    fn master(seed: u8) -> Xpriv {
        Xpriv::new_master(Network::Bitcoin, &[seed; 32]).unwrap()
    }

    fn account(master: &Xpriv, path: &str) -> String {
        let secp = secp256k1::Secp256k1::new();
        let xpriv = master
            .derive_priv(&secp, &DerivationPath::from_str(path).unwrap())
            .unwrap();
        format!(
            "[{}/{}]{}",
            master.fingerprint(&secp),
            path.trim_start_matches("m/"),
            Xpub::from_priv(&secp, &xpriv)
        )
    }

    fn descriptor(text: &str) -> Descriptor<DescriptorPublicKey> {
        Descriptor::from_str(text).unwrap()
    }

    struct Wallet {
        source: SplitSource,
    }

    fn wallet(shape: Shape) -> Wallet {
        let branch =
            |template: &str, branch: u32| descriptor(&template.replace("{b}", &branch.to_string()));
        let template = match shape {
            Shape::Wpkh => format!("wpkh({}/{{b}}/*)", account(&master(1), "m/84'/0'/0'")),
            Shape::ShWpkh => format!("sh(wpkh({}/{{b}}/*))", account(&master(1), "m/49'/0'/0'")),
            Shape::Pkh => format!("pkh({}/{{b}}/*)", account(&master(1), "m/44'/0'/0'")),
            Shape::WshSortedMulti | Shape::WshMulti => {
                let keys: Vec<_> = [master(1), master(2), master(3)]
                    .iter()
                    .map(|m| format!("{}/{{b}}/*", account(m, "m/48'/0'/0'/2'")))
                    .collect();
                let name = if shape == Shape::WshMulti {
                    "multi"
                } else {
                    "sortedmulti"
                };
                format!("wsh({name}(2,{}))", keys.join(","))
            }
        };
        Wallet {
            source: SplitSource::new(branch(&template, 0), Some(branch(&template, 1))).unwrap(),
        }
    }

    #[test]
    fn taproot_and_unsupported_sources_refuse() {
        let key = account(&master(1), "m/86'/0'/0'");
        let tr = descriptor(&format!("tr({key}/0/*)"));
        let wpkh = descriptor(&format!("wpkh({key}/0/*)"));
        assert_eq!(
            SplitSource::new(tr.clone(), None),
            Err(SourceError::Taproot)
        );
        assert_eq!(
            SplitSource::new(wpkh.clone(), Some(tr)),
            Err(SourceError::Taproot)
        );
        for text in [
            // Arbitrary wsh policies and nested segwit scripts have no route.
            format!("wsh(pkh({key}/0/*))"),
            format!("sh(wsh(multi(1,{key}/0/*)))"),
            format!("sh(multi(1,{key}/0/*))"),
            // Ambiguous multipath and hardened public derivation.
            format!("wpkh({key}/<0;1>/*)"),
            format!("wpkh({key}/0/*')"),
            format!("wpkh({key}/0'/*)"),
        ] {
            assert_eq!(
                SplitSource::new(descriptor(&text), None),
                Err(SourceError::UnsupportedDescriptor),
                "{text}"
            );
        }
        assert!(SplitSource::new(wpkh, None).is_ok());
    }

    #[test]
    fn change_descriptor_must_be_the_same_wallet() {
        for shape in SHAPES {
            // The fixture's external/internal pair differs only in the branch step.
            let wallet = wallet(shape);
            let external = wallet.source.external().clone();
            let internal = wallet.source.internal().unwrap().clone();
            assert!(SplitSource::new(external.clone(), Some(internal)).is_ok());
            // Another shape of the same keys is not the same wallet.
            let other_shape = self::wallet(if shape == Shape::Wpkh {
                Shape::Pkh
            } else {
                Shape::Wpkh
            });
            assert_eq!(
                SplitSource::new(external.clone(), other_shape.source.internal().cloned()),
                Err(SourceError::UnrelatedInternal),
                "{shape:?}"
            );
        }
        let a = account(&master(1), "m/84'/0'/0'");
        let b = account(&master(7), "m/84'/0'/0'");
        let c = account(&master(1), "m/84'/0'/1'");
        let external = descriptor(&format!("wpkh({a}/0/*)"));
        for (internal, ok) in [
            (format!("wpkh({a}/1/*)"), true),
            // Unrelated master: step 1 would otherwise be able to pay another wallet.
            (format!("wpkh({b}/1/*)"), false),
            // Same master, other account.
            (format!("wpkh({c}/1/*)"), false),
            // A different path shape.
            (format!("wpkh({a}/1/0/*)"), false),
            (format!("pkh({a}/1/*)"), false),
        ] {
            assert_eq!(
                SplitSource::new(external.clone(), Some(descriptor(&internal))).is_ok(),
                ok,
                "{internal}"
            );
            if !ok {
                assert_eq!(
                    SplitSource::new(external.clone(), Some(descriptor(&internal))),
                    Err(SourceError::UnrelatedInternal)
                );
            }
        }
        // Multisig: key order, threshold and every key's origin must match.
        let keys: Vec<_> = (1..=3)
            .map(|seed| account(&master(seed), "m/48'/0'/0'/2'"))
            .collect();
        let multi = |name: &str, k: usize, order: [usize; 3], branch: [u32; 3]| {
            descriptor(&format!(
                "wsh({name}({k},{}/{}/*,{}/{}/*,{}/{}/*))",
                keys[order[0]], branch[0], keys[order[1]], branch[1], keys[order[2]], branch[2]
            ))
        };
        let external = multi("sortedmulti", 2, [0, 1, 2], [0, 0, 0]);
        for (internal, ok) in [
            (multi("sortedmulti", 2, [0, 1, 2], [1, 1, 1]), true),
            (multi("sortedmulti", 1, [0, 1, 2], [1, 1, 1]), false),
            (multi("multi", 2, [0, 1, 2], [1, 1, 1]), false),
            (multi("sortedmulti", 2, [1, 0, 2], [1, 1, 1]), false),
        ] {
            assert_eq!(
                SplitSource::new(external.clone(), Some(internal.clone())).is_ok(),
                ok,
                "{internal}"
            );
        }
        let stranger = account(&master(9), "m/48'/0'/0'/2'");
        let swapped = descriptor(&format!(
            "wsh(sortedmulti(2,{}/1/*,{}/1/*,{stranger}/1/*))",
            keys[0], keys[1]
        ));
        assert_eq!(
            SplitSource::new(external, Some(swapped)),
            Err(SourceError::UnrelatedInternal)
        );
    }

    /// D9 source digest: domain-tagged (`coincube/split-source/v1`), then
    /// branch-tagged and length-prefixed over each descriptor's
    /// canonical text, so the receive-only and paired sources differ, every
    /// shape differs, and the value is pinned (`split_from` and the Split
    /// journal store it).
    #[test]
    fn split_source_digest_is_canonical_and_pinned() {
        let digests: Vec<_> = SHAPES
            .iter()
            .map(|shape| wallet(*shape).source.digest())
            .collect();
        for (i, a) in digests.iter().enumerate() {
            for b in &digests[i + 1..] {
                assert_ne!(a, b);
            }
        }
        let paired = wallet(Shape::Wpkh).source;
        let receive_only = SplitSource::new(paired.external().clone(), None).unwrap();
        assert_eq!(
            receive_only.digest().to_string(),
            "62f89beb8dcc74d7746d2471cda8db031f195c30dd6115e4c271b568d38dbac4"
        );
        assert_ne!(paired.digest(), receive_only.digest());
        assert_eq!(paired.digest(), wallet(Shape::Wpkh).source.digest());
        assert_eq!(
            paired.digest().to_string(),
            "be64de72f4745a148474c2b1c06112619c8ba5d05950c9321bd568aa5ff399d9"
        );
    }
}
