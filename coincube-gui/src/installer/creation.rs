//! Public-key assembly for the MCP experiment. No signer, keystore or install access.
use super::{
    descriptor::{Key, KeySource},
    step::descriptor::editor::key::new_multixkey_from_xpub,
};
use coincube_core::{
    descriptors::{CoincubeDescriptor, CoincubePolicy, PathInfo},
    miniscript::{
        bitcoin::Network,
        descriptor::{DescriptorPublicKey, Wildcard},
    },
};
use coincube_mcp::{
    proposal::{CreationInput, Store},
    Error, Reader, Request, Response,
};
use std::{
    collections::{BTreeMap, HashSet},
    str::FromStr,
    sync::Arc,
};

pub(crate) struct Validated {
    pub primary: Vec<Key>,
    pub recovery: Vec<Key>,
    pub descriptor: CoincubeDescriptor,
}

/// Parse only bare, origin-bearing public account keys. A supplied origin is
/// unverified metadata, not evidence of a physical device or separate custody.
pub(crate) fn validate(input: &CreationInput) -> Result<Validated, Error> {
    if !input.valid() {
        return Err(Error::InvalidInput);
    }
    let mut fingerprints = HashSet::new();
    let mut accounts = HashSet::new();
    let mut parse = |keys: &[String], path: &str| -> Result<Vec<Key>, Error> {
        keys.iter()
            .enumerate()
            .map(|(index, value)| {
                let DescriptorPublicKey::XPub(xpub) =
                    DescriptorPublicKey::from_str(value).map_err(|_| Error::InvalidInput)?
                else {
                    return Err(Error::InvalidInput);
                };
                let (fingerprint, origin) = xpub.origin.as_ref().ok_or(Error::InvalidInput)?;
                if xpub.xkey.network != Network::Testnet.into()
                    || xpub.wildcard != Wildcard::None
                    || !xpub.derivation_path.as_ref().is_empty()
                    || origin.as_ref().len() != usize::from(xpub.xkey.depth)
                    || (xpub.xkey.depth == 0 && *fingerprint != xpub.xkey.fingerprint())
                    || !fingerprints.insert(*fingerprint)
                    // Depth, parent fingerprint, child number and origin are
                    // caller-controlled metadata. Only the key and chain code
                    // identify the account's derived signing keys.
                    || !accounts.insert((xpub.xkey.public_key, xpub.xkey.chain_code))
                {
                    return Err(Error::InvalidInput);
                }
                Ok(Key {
                    source: KeySource::Manual,
                    name: format!("AI {path} {}", index + 1),
                    fingerprint: *fingerprint,
                    key: DescriptorPublicKey::XPub(xpub),
                    account: None,
                })
            })
            .collect()
    };
    let primary = parse(&input.keys.primary, "Primary")?;
    let recovery = parse(&input.keys.recovery, "Recovery")?;
    let convert = |keys: &[Key]| {
        keys.iter()
            .map(|key| {
                let DescriptorPublicKey::XPub(xpub) = &key.key else {
                    unreachable!("parsed account xpub")
                };
                DescriptorPublicKey::MultiXPub(new_multixkey_from_xpub(xpub.clone(), 0))
            })
            .collect::<Vec<_>>()
    };
    let recovery_keys = convert(&recovery);
    let recovery_path = if recovery_keys.len() == 1 {
        PathInfo::Single(recovery_keys[0].clone())
    } else {
        PathInfo::Multi(
            usize::from(input.topology.recovery.quorum.threshold),
            recovery_keys,
        )
    };
    let policy = CoincubePolicy::new_legacy(
        PathInfo::Multi(3, convert(&primary)),
        BTreeMap::from([(input.topology.recovery.relative_delay_blocks, recovery_path)]),
    )
    .map_err(|_| Error::InvalidInput)?;
    let descriptor = CoincubeDescriptor::new(policy);
    // Round-trip through the same descriptor parser used for wallet installation.
    let descriptor =
        CoincubeDescriptor::from_str(&descriptor.to_string()).map_err(|_| Error::InvalidInput)?;
    Ok(Validated {
        primary,
        recovery,
        descriptor,
    })
}

/// Desktop reader for the new separately paired scope, with bounded CPU work.
pub(crate) struct CreationReader {
    store: Arc<Store>,
    slots: Arc<tokio::sync::Semaphore>,
}
impl CreationReader {
    pub(crate) fn new(store: Arc<Store>) -> Self {
        Self {
            store,
            slots: Arc::new(tokio::sync::Semaphore::new(1)),
        }
    }
}
impl Reader for CreationReader {
    fn read(
        &self,
        request: Request,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<Response, Error>> + Send + '_>>
    {
        let store = self.store.clone();
        let slots = self.slots.clone();
        Box::pin(async move {
            match request {
                Request::PrepareCreation(input) => {
                    let permit = slots.try_acquire_owned().map_err(|_| Error::Unavailable)?;
                    tokio::task::spawn_blocking(move || {
                        let _permit = permit;
                        store
                            .prepare_creation(input, |input| validate(input).map(|_| ()))
                            .map(Response::Proposal)
                    })
                    .await
                    .map_err(|_| Error::Unavailable)?
                }
                Request::Proposal(input) => store.get(&input).map(Response::Proposal),
                _ => Err(Error::AccessDenied),
            }
        })
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use coincube_core::miniscript::bitcoin::{
        bip32::{Xpriv, Xpub},
        secp256k1::Secp256k1,
    };
    use coincube_mcp::{
        ipc::Grant,
        proposal::{ProposalInput, PublicKeys, Quorum, Recovery, Topology},
    };

    pub(crate) fn input() -> CreationInput {
        let secp = Secp256k1::new();
        let keys: Vec<_> = (1..=8)
            .map(|index| {
                let private = Xpriv::new_master(Network::Regtest, &[index; 32]).unwrap();
                let public = Xpub::from_priv(&secp, &private);
                format!("[{}]{public}", public.fingerprint())
            })
            .collect();
        CreationInput {
            request_id: "public-key-fixture".into(),
            topology: Topology {
                primary: Quorum {
                    threshold: 3,
                    key_count: 5,
                },
                recovery: Recovery {
                    quorum: Quorum {
                        threshold: 2,
                        key_count: 3,
                    },
                    relative_delay_blocks: 144,
                },
            },
            keys: PublicKeys {
                primary: keys[..5].to_vec(),
                recovery: keys[5..].to_vec(),
            },
        }
    }

    #[test]
    fn proposal_public_keys_compile_actual_policy_without_claiming_provenance() {
        let prepared = validate(&input()).unwrap();
        assert!(!prepared.descriptor.is_taproot());
        assert!(prepared.descriptor.all_xpubs_net_is(Network::Testnet));
        assert_eq!(
            prepared
                .descriptor
                .policy()
                .primary_path()
                .thresh_origins()
                .0,
            3
        );
        let policy = prepared.descriptor.policy();
        let recovery = policy.recovery_paths();
        assert_eq!(recovery.len(), 1);
        assert_eq!(recovery[&144].thresh_origins().0, 2);
        assert_eq!(prepared.primary.len(), 5);
        assert_eq!(prepared.recovery.len(), 3);
        assert!(prepared
            .primary
            .iter()
            .chain(&prepared.recovery)
            .all(|key| key.source == KeySource::Manual));
    }

    #[test]
    fn proposal_rejects_secret_network_origin_suffix_and_duplicate_keys() {
        let original = input();
        let secp = Secp256k1::new();
        let secret = Xpriv::new_master(Network::Regtest, &[42; 32]).unwrap();
        let mainnet = Xpub::from_priv(
            &secp,
            &Xpriv::new_master(Network::Bitcoin, &[42; 32]).unwrap(),
        );
        let public = Xpub::from_priv(&secp, &secret);
        for key in [
            secret.to_string(),
            format!("[{}]{secret}", public.fingerprint()),
            format!("[{}]{mainnet}", mainnet.fingerprint()),
            public.to_string(),
            format!("[{}]{public}/*", public.fingerprint()),
            format!("[{}]{public}/0", public.fingerprint()),
            format!("[00000000]{public}"),
            format!("[{} /0]{public}", public.fingerprint()),
            format!("[{} /0]{public}", public.fingerprint()).replace(' ', ""),
            original.keys.primary[0].clone(),
            "x".repeat(257),
            "seed words are never accepted".into(),
        ] {
            let mut changed = original.clone();
            changed.keys.recovery[0] = key;
            assert!(validate(&changed).is_err());
        }
        // Re-labeling an existing account with a different claimed fingerprint
        // cannot hide duplicate underlying keys.
        let mut repeated = original.clone();
        repeated.keys.recovery[0] = repeated.keys.primary[0].replacen("4ba43603", "11223344", 1);
        assert!(validate(&repeated).is_err());
    }

    #[test]
    fn proposal_rejects_metadata_aliases_within_primary_and_across_recovery() {
        use coincube_core::miniscript::bitcoin::bip32::{ChildNumber, Fingerprint};

        let original = input();
        let DescriptorPublicKey::XPub(source) =
            DescriptorPublicKey::from_str(&original.keys.primary[0]).unwrap()
        else {
            panic!("account key expected")
        };
        let mut alias = source.xkey;
        alias.depth = 1;
        alias.parent_fingerprint = Fingerprint::from([99; 4]);
        alias.child_number = ChildNumber::from_normal_idx(42).unwrap();
        let alias = format!("[11223344/42]{alias}");

        // Each alias is a valid account by itself: reject its duplicate
        // derivation identity, not its distinct, untrusted origin metadata.
        let mut distinct = original.clone();
        distinct.keys.primary[0] = alias.clone();
        assert!(validate(&distinct).is_ok());
        for primary in [true, false] {
            let mut duplicate = original.clone();
            if primary {
                duplicate.keys.primary[1] = alias.clone();
            } else {
                duplicate.keys.recovery[0] = alias.clone();
            }
            assert!(matches!(validate(&duplicate), Err(Error::InvalidInput)));
        }
    }

    #[test]
    fn proposal_populates_native_builder_and_keeps_exact_native_approval_gate() {
        use crate::installer::{
            proposal::{
                tests::{fixture, TestDir},
                Review, Session,
            },
            step::{descriptor::editor::DefineDescriptor, Step},
        };
        let _guard = crate::app::session::test_guard();
        let dir = TestDir::new();
        let (mut ctx, _) = fixture(dir.path());
        let store = Arc::new(Store::default());
        let original = input();
        let proposal = store
            .prepare_creation(original.clone(), |input| validate(input).map(|_| ()))
            .unwrap();
        let grant = Arc::new(Grant::paired_creation("a".repeat(64), || true).unwrap());
        let session = Arc::new(Session::new(
            "proposal-test".into(),
            crate::app::session::generation_for("proposal-test").unwrap(),
            grant.clone(),
            store.clone(),
            proposal,
        ));
        ctx.mcp_proposal = Some(session.clone());
        ctx.descriptor = None;
        let signer = Arc::new(std::sync::Mutex::new(
            crate::signer::Signer::generate(Network::Regtest).unwrap(),
        ));
        let mut editor = DefineDescriptor::new(Network::Regtest, signer);
        editor.load_context(&ctx);
        assert!(editor.apply(&mut ctx));
        assert_eq!(
            ctx.descriptor.as_ref(),
            Some(&validate(&original).unwrap().descriptor)
        );
        assert_eq!(ctx.keys.len(), 8);
        assert!(ctx.hws.is_empty());
        assert!(ctx.connect_vault_members.is_empty());
        assert!(ctx.keys.values().all(|key| key.keychain_key_id.is_none()));
        let mut review = Review::new(session);
        review.load_context(&ctx);
        assert!(review.apply(&mut ctx));
        // A changed local review snapshot cannot reuse the prior approval.
        ctx.wallet_alias = "changed after review".into();
        assert!(crate::installer::proposal::reserve(&ctx).is_err());
        assert!(grant.active());
        review.load_context(&ctx);
        assert!(review.apply(&mut ctx));
        let _lease = crate::installer::proposal::reserve(&ctx).unwrap();
        assert!(!grant.active());
        assert!(store
            .get(&ProposalInput {
                proposal_id: "foreign".into()
            })
            .is_err());
        assert!(crate::installer::proposal::reserve(&ctx).is_err());
        crate::app::session::close();
    }

    #[tokio::test]
    async fn proposal_reader_validates_before_storage_and_denies_wallet_reads() {
        let store = Arc::new(Store::default());
        let reader = CreationReader::new(store.clone());
        let mut invalid = input();
        invalid.keys.primary[0] = "not-a-public-key".into();
        assert!(reader
            .read(Request::PrepareCreation(invalid))
            .await
            .is_err());
        assert!(store.pending().unwrap().is_none());
        let first = reader
            .read(Request::PrepareCreation(input()))
            .await
            .unwrap();
        let Response::Proposal(proposal) = first else {
            panic!("proposal expected")
        };
        assert!(proposal.descriptor_validated);
        assert!(reader
            .read(Request::List(coincube_mcp::Empty {}))
            .await
            .is_err());
        store.revoke();
        assert!(reader
            .read(Request::PrepareCreation(input()))
            .await
            .is_err());
    }
}
