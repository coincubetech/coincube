use coincube_mcp::{
    Chain, Error,
    proposal::{
        CreationInput, PrepareInput, ProposalState, PublicKeys, Quorum, Recovery, Store, Topology,
    },
};

fn topology() -> Topology {
    Topology {
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
    }
}
fn proposal(network: Chain) -> PrepareInput {
    PrepareInput {
        request_id: "chain-bound".into(),
        network,
        topology: topology(),
    }
}
fn creation(network: Chain) -> CreationInput {
    CreationInput {
        request_id: "chain-bound".into(),
        network,
        topology: topology(),
        // Only the desktop validator can establish whether these are keys.
        keys: PublicKeys {
            primary: vec!["public-key-fixture".into(); 5],
            recovery: vec!["public-key-fixture".into(); 3],
        },
    }
}

#[test]
fn exact_chain_is_required_on_both_wire_inputs() {
    for network in Chain::ALL {
        let mut value = serde_json::to_value(proposal(network)).unwrap();
        assert_eq!(value["network"], network.as_str());
        assert_eq!(
            serde_json::from_value::<PrepareInput>(value.clone()).unwrap(),
            proposal(network)
        );
        value.as_object_mut().unwrap().remove("network");
        assert!(serde_json::from_value::<PrepareInput>(value).is_err());
        let mut value = serde_json::to_value(creation(network)).unwrap();
        value["network"] = "testnet".into();
        assert!(serde_json::from_value::<CreationInput>(value.clone()).is_err());
        value.as_object_mut().unwrap().remove("network");
        assert!(serde_json::from_value::<CreationInput>(value).is_err());
    }
}

#[test]
fn topology_grants_reject_every_other_chain_without_consuming_the_draft() {
    for paired in Chain::ALL {
        let store = Store::for_chain(paired);
        for requested in Chain::ALL.into_iter().filter(|chain| *chain != paired) {
            assert_eq!(store.prepare(proposal(requested)), Err(Error::InvalidInput));
            assert!(store.pending().unwrap().is_none());
        }
        let accepted = store.prepare(proposal(paired)).unwrap();
        assert_eq!(accepted.network, paired);
        assert_eq!(accepted.state, ProposalState::NeedsKeys);
        assert!(!accepted.descriptor_validated);
        assert_eq!(store.prepare(proposal(paired)).unwrap(), accepted);
        store.revoke();
        assert_eq!(store.prepare(proposal(paired)), Err(Error::AccessDenied));
    }
}

#[test]
fn creation_grants_check_chain_before_validation_and_preserve_immutable_retries() {
    for paired in Chain::ALL {
        let store = Store::for_chain(paired);
        for requested in Chain::ALL.into_iter().filter(|chain| *chain != paired) {
            assert_eq!(
                store.prepare_creation(creation(requested), |_| panic!(
                    "wrong-chain keys must not reach validation"
                )),
                Err(Error::InvalidInput)
            );
            assert!(store.pending().unwrap().is_none());
        }
        assert_eq!(
            store.prepare_creation(creation(paired), |_| Err(Error::InvalidInput)),
            Err(Error::InvalidInput)
        );
        assert!(store.pending().unwrap().is_none());
        let accepted = store
            .prepare_creation(creation(paired), |_| Ok(()))
            .unwrap();
        assert_eq!(accepted.network, paired);
        assert_eq!(
            store.creation_input(&accepted.proposal_id).unwrap(),
            Some(creation(paired))
        );
        assert_eq!(
            store
                .prepare_creation(creation(paired), |_| panic!(
                    "exact retry must reuse validated draft"
                ))
                .unwrap(),
            accepted
        );
        let mut changed = creation(paired);
        changed.keys.recovery[0] = "other-key".into();
        assert_eq!(
            store.prepare_creation(changed, |_| panic!("changed retry must not replace draft")),
            Err(Error::ProposalConflict)
        );
        assert_eq!(
            store.prepare(proposal(paired)),
            Err(Error::ProposalConflict)
        );
    }
}
