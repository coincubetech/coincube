//! Exact, incomplete-PSBT handling for the ordinary `S+B` / `S+F` Claim pair.
//! This is a signing-result check, never live ancestry or broadcast authority.

use coincube_core::{
    claim_finalize::{
        finalize_ancestry_transfer, finalize_claim_fork_sweep,
        validate_ancestry_claim_pair_signing, validate_ancestry_transfer_signing,
        validate_claim_fork_signing, ClaimForkFinalizeError, ClaimPairFinalizeError, FinalizeError,
    },
    claim_spend::{AncestrySelfTransfer, ClaimForkSweep},
    miniscript::bitcoin::{psbt::Psbt, secp256k1},
    psbt_unified::{merge_signatures, UnifiedPsbt, UnifiedPsbtError},
    unified_finalize::{verify_all_signatures, UnifiedFinalizeError},
};

pub(crate) const MAX_SIGNER_RETURNS: usize = 16;

#[derive(Debug)]
pub(crate) enum Error {
    TooManyReturns,
    ConflictingBitcoinSignature { input: usize },
    BitcoinUnsigned,
    ForkUnsigned,
    Finalized,
    Bitcoin(FinalizeError),
    Fork(ClaimForkFinalizeError),
    ForkSignature(UnifiedFinalizeError),
    Adapter(UnifiedPsbtError),
    Pair(ClaimPairFinalizeError),
}

#[derive(Debug)]
pub(crate) struct CombinedClaimPair {
    pub bitcoin: Psbt,
    pub fork: UnifiedPsbt,
    pub complete: bool,
}

/// Authenticate every returned signature and combine only those signatures
/// into the two original PSBTs. One complete side does not make the pair
/// complete. The caller must freshly requalify the exact wallet, reservation,
/// two-chain ancestry and policy before each signer dispatch and submission.
pub(crate) fn combine_ancestry_claim_pair(
    bitcoin: &AncestrySelfTransfer,
    bitcoin_returns: &[Psbt],
    fork: &ClaimForkSweep,
    fork_returns: &[UnifiedPsbt],
) -> Result<CombinedClaimPair, Error> {
    if bitcoin_returns.len() > MAX_SIGNER_RETURNS || fork_returns.len() > MAX_SIGNER_RETURNS {
        return Err(Error::TooManyReturns);
    }
    let secp = secp256k1::Secp256k1::verification_only();
    let mut bitcoin_signed = bitcoin.psbt().clone();
    for returned in bitcoin_returns {
        if returned
            .inputs
            .iter()
            .any(|input| input.final_script_sig.is_some() || input.final_script_witness.is_some())
        {
            return Err(Error::Finalized);
        }
        if returned
            .inputs
            .iter()
            .all(|input| input.partial_sigs.is_empty())
        {
            return Err(Error::BitcoinUnsigned);
        }
        validate_ancestry_transfer_signing(bitcoin, returned).map_err(Error::Bitcoin)?;
        match finalize_ancestry_transfer(bitcoin, returned, &secp) {
            Ok(_) | Err(FinalizeError::Unsatisfied) => {}
            Err(error) => return Err(Error::Bitcoin(error)),
        }
        for (index, (input, supplied)) in bitcoin_signed
            .inputs
            .iter_mut()
            .zip(&returned.inputs)
            .enumerate()
        {
            for (key, signature) in &supplied.partial_sigs {
                if input
                    .partial_sigs
                    .get(key)
                    .is_some_and(|existing| existing != signature)
                {
                    return Err(Error::ConflictingBitcoinSignature { input: index });
                }
                input.partial_sigs.insert(*key, *signature);
            }
        }
    }
    let mut fork_signed = UnifiedPsbt::from_psbt(fork.psbt().clone()).map_err(Error::Adapter)?;
    for returned in fork_returns {
        if returned
            .psbt()
            .inputs
            .iter()
            .any(|input| input.final_script_sig.is_some() || input.final_script_witness.is_some())
        {
            return Err(Error::Finalized);
        }
        validate_claim_fork_signing(fork, returned).map_err(Error::Fork)?;
        let counts = verify_all_signatures(returned, &secp).map_err(Error::ForkSignature)?;
        if counts.legacy == 0 && counts.unified == 0 {
            return Err(Error::ForkUnsigned);
        }
        merge_signatures(&mut fork_signed, returned).map_err(Error::Adapter)?;
    }
    validate_ancestry_claim_pair_signing(bitcoin, fork, &bitcoin_signed, &fork_signed)
        .map_err(Error::Pair)?;
    let bitcoin_complete = match finalize_ancestry_transfer(bitcoin, &bitcoin_signed, &secp) {
        Ok(_) => true,
        Err(FinalizeError::Unsatisfied) => false,
        Err(error) => return Err(Error::Bitcoin(error)),
    };
    let fork_complete = match finalize_claim_fork_sweep(fork, &fork_signed, &secp) {
        Ok(_) => true,
        Err(ClaimForkFinalizeError::Finalize(UnifiedFinalizeError::Unsatisfiable { .. })) => false,
        Err(error) => return Err(Error::Fork(error)),
    };
    Ok(CombinedClaimPair {
        bitcoin: bitcoin_signed,
        fork: fork_signed,
        complete: bitcoin_complete && fork_complete,
    })
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use coincube_core::{
        bip39::Mnemonic,
        chain::ChainId,
        claim_ancestry::{retained::RetainedPath, search::OwnedLink},
        claim_spend::{create_ancestry_fork_sweep_with_marker, create_ancestry_self_transfer},
        descriptors::{CoincubeDescriptor, CoincubePolicy, PathInfo},
        miniscript::{
            bitcoin::{
                absolute,
                bip32::{ChildNumber, DerivationPath},
                consensus::serialize,
                transaction, Amount, Network, OutPoint, ScriptBuf, Transaction, TxIn, TxOut,
            },
            DescriptorPublicKey,
        },
        signer::MasterSigner,
        spend::{CandidateCoin, TxGetter},
    };
    use std::{
        collections::{BTreeMap, HashMap},
        str::FromStr,
    };

    struct Getter(HashMap<coincube_core::miniscript::bitcoin::Txid, Transaction>);
    impl TxGetter for Getter {
        fn get_tx(&mut self, id: &coincube_core::miniscript::bitcoin::Txid) -> Option<Transaction> {
            self.0.get(id).cloned()
        }
    }

    pub(crate) fn fixture() -> (AncestrySelfTransfer, ClaimForkSweep, Vec<MasterSigner>) {
        let secp = secp256k1::Secp256k1::new();
        let verify = secp256k1::Secp256k1::verification_only();
        let signers: Vec<_> = (40..44)
            .map(|byte| {
                MasterSigner::from_mnemonic(
                    Network::Bitcoin,
                    Mnemonic::from_entropy(&[byte; 16]).unwrap(),
                )
                .unwrap()
            })
            .collect();
        let keys: Vec<_> = signers
            .iter()
            .map(|signer| {
                DescriptorPublicKey::from_str(&format!(
                    "[{}]{}/<0;1>/*",
                    signer.fingerprint(&secp),
                    signer.xpub_at(&DerivationPath::default(), &secp)
                ))
                .unwrap()
            })
            .collect();
        let descriptor = CoincubeDescriptor::new(
            CoincubePolicy::new_legacy(
                PathInfo::Multi(2, keys[..3].to_vec()),
                BTreeMap::from([(46, PathInfo::Single(keys[3].clone()))]),
            )
            .unwrap(),
        );
        let make_coin = |index: u32, lock: u32| {
            let deriv_index = ChildNumber::from_normal_idx(index).unwrap();
            let transaction = Transaction {
                version: transaction::Version::TWO,
                lock_time: absolute::LockTime::from_consensus(lock),
                input: vec![TxIn::default()],
                output: vec![TxOut {
                    value: Amount::from_sat(100_000),
                    script_pubkey: descriptor
                        .receive_descriptor()
                        .derive(deriv_index, &secp)
                        .script_pubkey(),
                }],
            };
            let coin = CandidateCoin {
                outpoint: OutPoint::new(transaction.compute_txid(), 0),
                amount: transaction.output[0].value,
                deriv_index,
                is_change: false,
                must_select: true,
                sequence: None,
                ancestor_info: None,
            };
            (coin, transaction)
        };
        let (shared, shared_tx) = make_coin(7, 1);
        let (bitcoin_funding, bitcoin_tx) = make_coin(8, 2);
        let (fork_funding, fork_tx) = make_coin(9, 3);
        let path = |coin: &CandidateCoin, tx: &Transaction| {
            RetainedPath::new(
                coin.outpoint,
                vec![OwnedLink {
                    transaction: serialize(tx),
                    parent_input: None,
                }],
            )
            .unwrap()
        };
        let bitcoin_path = path(&bitcoin_funding, &bitcoin_tx);
        let fork_path = path(&fork_funding, &fork_tx);
        let mut getter = Getter(HashMap::from([
            (shared_tx.compute_txid(), shared_tx),
            (bitcoin_tx.compute_txid(), bitcoin_tx),
            (fork_tx.compute_txid(), fork_tx),
        ]));
        let bitcoin = create_ancestry_self_transfer(
            ChainId::Bitcoin,
            &descriptor,
            &verify,
            &mut getter,
            &[shared, bitcoin_funding],
            ChildNumber::from_normal_idx(12).unwrap(),
            5,
            absolute::LockTime::ZERO,
            &bitcoin_path.reverify().unwrap(),
        )
        .unwrap();
        let fork = create_ancestry_fork_sweep_with_marker(
            &bitcoin,
            ChainId::BitcoinBlake2b,
            &verify,
            &mut getter,
            &[shared, fork_funding],
            ChildNumber::from_normal_idx(13).unwrap(),
            5,
            absolute::LockTime::ZERO,
            &fork_path.reverify().unwrap(),
        )
        .unwrap();
        (bitcoin, fork, signers)
    }

    #[test]
    fn paired_claim_returns_need_both_complete_and_refuse_a_substitute() {
        let (bitcoin, fork, signers) = fixture();
        let secp = secp256k1::Secp256k1::new();
        let bitcoin_returns: Vec<_> = signers[..2]
            .iter()
            .map(|signer| signer.sign_psbt(bitcoin.psbt().clone(), &secp).unwrap())
            .collect();
        let fork_returns: Vec<_> = signers[..2]
            .iter()
            .map(|signer| {
                UnifiedPsbt::from_psbt(signer.sign_psbt(fork.psbt().clone(), &secp).unwrap())
                    .unwrap()
            })
            .collect();
        let partial =
            combine_ancestry_claim_pair(&bitcoin, &bitcoin_returns[..1], &fork, &fork_returns[..1])
                .unwrap();
        assert!(!partial.complete);
        let one_sided =
            combine_ancestry_claim_pair(&bitcoin, &bitcoin_returns, &fork, &[]).unwrap();
        assert!(!one_sided.complete);
        let complete =
            combine_ancestry_claim_pair(&bitcoin, &bitcoin_returns, &fork, &fork_returns).unwrap();
        assert!(complete.complete);
        coincube_core::claim_finalize::finalize_ancestry_claim_pair(
            &bitcoin,
            &fork,
            &complete.bitcoin,
            &complete.fork,
            &secp256k1::Secp256k1::verification_only(),
        )
        .unwrap();

        let mut changed = fork_returns[0].clone();
        changed.psbt_mut().unsigned_tx.output[0].script_pubkey = ScriptBuf::new();
        assert!(matches!(
            combine_ancestry_claim_pair(&bitcoin, &bitcoin_returns, &fork, &[changed]),
            Err(Error::Fork(_))
        ));
        assert!(matches!(
            combine_ancestry_claim_pair(
                &bitcoin,
                &fork_returns
                    .iter()
                    .map(|p| p.psbt().clone())
                    .collect::<Vec<_>>(),
                &fork,
                &[]
            ),
            Err(Error::Bitcoin(_))
        ));
    }
}
