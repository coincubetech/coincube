use super::*;
use coincube_core::miniscript::bitcoin::{hashes::Hash, ScriptBuf, WScriptHash};
use wire::Reader;

#[test]
fn mcp_transaction_production_chains_bind_intent_engine_and_native_draft() {
    tokio::runtime::Runtime::new().unwrap().block_on(async {
        for network in wire::Chain::ALL {
            let chain: ChainId = network.as_str().parse().unwrap();
            let (root, service) = super::tests::fixture_on_chain(chain).await;
            let mut input = super::tests::input();
            input.network = network;
            input.recipients[0].address = Address::from_script(
                &ScriptBuf::new_p2wsh(&WScriptHash::all_zeros()),
                chain.bitcoin_network(),
            )
            .unwrap()
            .to_string();
            // Every other intended chain is refused, including the twin whose
            // address encodes identically. No request identity is consumed.
            for other in wire::Chain::ALL
                .iter()
                .copied()
                .filter(|other| *other != network)
            {
                let mut wrong = input.clone();
                wrong.network = other;
                assert!(matches!(
                    service.read(wire::Request::PrepareTransaction(wrong)).await,
                    Err(wire::Error::AccessDenied)
                ));
            }
            assert!(service.drafts.lock().await.is_empty());
            let wire::Response::List(list) = service
                .read(wire::Request::List(wire::Empty {}))
                .await
                .unwrap()
            else {
                panic!("wallet list")
            };
            assert_eq!(list.wallets[0].network, chain.to_string());
            let wire::Response::Transaction(prepared) = service
                .read(wire::Request::PrepareTransaction(input.clone()))
                .await
                .unwrap()
            else {
                panic!("prepared")
            };
            assert_eq!(prepared.network, chain.to_string());
            assert_eq!(prepared.status, "unsigned_ready_for_native_review");
            let native = service.native_checked(&prepared.draft_id).await.unwrap();
            assert_eq!(native.chain, chain);
            assert_eq!(
                coincube_core::spend::mcp_fee_cap(&native.psbt),
                Some((input.max_fee_sats, input.fee_rate_sat_vb))
            );
            assert!(service
                .daemon
                .list_spend_txs()
                .await
                .unwrap()
                .spend_txs
                .is_empty());
            let wire::Response::Transaction(retry) = service
                .read(wire::Request::PrepareTransaction(input.clone()))
                .await
                .unwrap()
            else {
                panic!("retry")
            };
            assert_eq!(prepared, retry);
            // The native handoff changes no signer state and retains the
            // exact original construction, including the bounded fee policy.
            assert!(native
                .psbt
                .inputs
                .iter()
                .all(|input| input.partial_sigs.is_empty()));
            let info = service.daemon.get_info().await.unwrap();
            for other in wire::Chain::ALL
                .iter()
                .copied()
                .filter(|other| *other != network)
            {
                let mut changed = info.clone();
                let other: ChainId = other.as_str().parse().unwrap();
                changed.chain_id = Some(other);
                changed.network = other.bitcoin_network();
                assert!(!service.ready(&changed));
            }
            service.revoke();
            assert!(service.native_checked(&prepared.draft_id).await.is_none());
            super::tests::cleanup(root, service).await;
        }
    });
}

#[test]
fn mcp_transaction_fork_unsigned_validation_rejects_unified_records_and_sighash() {
    tokio::runtime::Runtime::new().unwrap().block_on(async {
        let chain = ChainId::BitcoinBlake2b;
        let (root, service) = super::tests::fixture_on_chain(chain).await;
        let mut input = super::tests::input();
        input.network = wire::Chain::BitcoinBlake2b;
        input.recipients[0].address = Address::from_script(
            &ScriptBuf::new_p2wsh(&WScriptHash::all_zeros()),
            chain.bitcoin_network(),
        )
        .unwrap()
        .to_string();
        let wire::Response::Transaction(prepared) = service
            .read(wire::Request::PrepareTransaction(input.clone()))
            .await
            .unwrap()
        else {
            panic!("prepared")
        };
        let native = service.native_checked(&prepared.draft_id).await.unwrap();
        // A well-formed unified signature is still signed data and must be
        // rejected by the unsigned preparation boundary.
        use coincube_core::miniscript::bitcoin::secp256k1::{Message, Secp256k1, SecretKey};
        let secp = Secp256k1::new();
        let secret = SecretKey::from_slice(&[7; 32]).unwrap();
        let public = secret.public_key(&secp);
        let mut signature = secp
            .sign_ecdsa(&Message::from_digest([3; 32]), &secret)
            .serialize_der()
            .to_vec();
        signature.push(0x21);
        let mut signed = native.psbt.clone();
        signed.inputs[0].proprietary.insert(
            coincube_core::miniscript::bitcoin::psbt::raw::ProprietaryKey {
                prefix: b"coincube".to_vec(),
                subtype: 0,
                key: public.serialize().to_vec(),
            },
            signature,
        );
        let unified = coincube_core::psbt_unified::UnifiedPsbt::from_psbt(signed.clone()).unwrap();
        assert_eq!(
            coincube_core::psbt_unified::unified_signatures(&unified)
                .unwrap()
                .len(),
            1
        );
        assert!(validate(chain, &input, &signed, &native.coins, &service.descriptor).is_err());
        let mut malformed = native.psbt.clone();
        malformed.inputs[0].proprietary.insert(
            coincube_core::miniscript::bitcoin::psbt::raw::ProprietaryKey {
                prefix: b"coincube".to_vec(),
                subtype: 0,
                key: vec![2; 33],
            },
            vec![0x21],
        );
        assert!(validate(
            chain,
            &input,
            &malformed,
            &native.coins,
            &service.descriptor
        )
        .is_err());
        let mut unsafe_sighash = native.psbt.clone();
        unsafe_sighash.inputs[0].sighash_type = Some(
            coincube_core::miniscript::bitcoin::sighash::EcdsaSighashType::AllPlusAnyoneCanPay
                .into(),
        );
        assert!(validate(
            chain,
            &input,
            &unsafe_sighash,
            &native.coins,
            &service.descriptor
        )
        .is_err());
        super::tests::cleanup(root, service).await;
    });
}
