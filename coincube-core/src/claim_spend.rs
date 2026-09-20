//! Unsigned owned-Vault Bitcoin poison self-transfers. No Claim authorization.
//!
//! The caller must reserve an unused change index and obtain current deployment,
//! chain and mempool observations separately. Construction proves neither input
//! exclusivity nor that RDTS is active. Do not sign or broadcast based on this
//! result alone. No seed, wallet storage or network operations occur here.

use std::{collections::BTreeSet, convert::TryFrom, fmt};

use miniscript::bitcoin::{
    self,
    absolute::LockTime,
    bip32::ChildNumber,
    hashes::{sha256, Hash},
    secp256k1,
};

use crate::{
    chain::ChainId,
    descriptors::CoincubeDescriptor,
    spend::{
        self, AddrInfo, CandidateCoin, SpendCreationError, SpendOutputAddress, SpendTxFees,
        TxGetter,
    },
};

#[derive(Debug)]
pub enum Error {
    InvalidRequest(&'static str),
    Spend(SpendCreationError),
}
impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidRequest(reason) => f.write_str(reason),
            Self::Spend(error) => error.fmt(f),
        }
    }
}
impl std::error::Error for Error {}
impl From<SpendCreationError> for Error {
    fn from(error: SpendCreationError) -> Self {
        Self::Spend(error)
    }
}

/// Construction result with no public-field or deserialization bypass. This
/// certifies only the unsigned construction checks, not live spend permission.
#[derive(Debug)]
pub struct PoisonSelfTransfer {
    psbt: bitcoin::psbt::Psbt,
    descriptor: CoincubeDescriptor,
    chain: ChainId,
    change_index: ChildNumber,
    warnings: Vec<spend::CreateSpendWarning>,
}
impl PoisonSelfTransfer {
    pub fn psbt(&self) -> &bitcoin::psbt::Psbt {
        &self.psbt
    }
    pub fn descriptor(&self) -> &CoincubeDescriptor {
        &self.descriptor
    }
    pub fn chain(&self) -> ChainId {
        self.chain
    }
    pub fn change_index(&self) -> ChildNumber {
        self.change_index
    }
    pub fn warnings(&self) -> &[spend::CreateSpendWarning] {
        &self.warnings
    }
}

/// Construct a native P2WSH self-sweep of exactly `coins` on Bitcoin/mainnet
/// or Bitcoin/testnet4. The destination is derived from the same descriptor's
/// change branch; arbitrary recipient addresses/scripts cannot be supplied.
/// `change_index` must be reserved as fresh by the wallet controller. This pure
/// function only rejects reuse of a selected source script, not historical use.
///
/// `fork_marker` identifies the intended observed fork anchor in the payload;
/// it is caller-supplied labeling, NOT authenticated chain evidence. The output
/// is a deterministic 90-byte OP_RETURN script with zero value, charged in coin
/// selection before any signature exists. All previous transactions are checked
/// through the ordinary spend builder. Normal Bitcoin SIGHASH_ALL signing is
/// a later step; this function neither signs nor creates split evidence.
#[allow(clippy::too_many_arguments)]
pub fn create_poison_self_transfer(
    chain: ChainId,
    descriptor: &CoincubeDescriptor,
    secp: &secp256k1::Secp256k1<secp256k1::VerifyOnly>,
    tx_getter: &mut impl TxGetter,
    coins: &[CandidateCoin],
    change_index: ChildNumber,
    feerate_vb: u64,
    locktime: LockTime,
    fork_marker: bitcoin::BlockHash,
) -> Result<PoisonSelfTransfer, Error> {
    let (network, chain_byte) = match chain {
        ChainId::Bitcoin => (bitcoin::Network::Bitcoin, 0),
        ChainId::Testnet4 => (bitcoin::Network::Testnet4, 1),
        _ => {
            return Err(Error::InvalidRequest(
                "Bitcoin mainnet or testnet4 source required",
            ))
        }
    };
    if descriptor.is_taproot() || change_index.is_hardened() || coins.is_empty() {
        return Err(Error::InvalidRequest(
            "Native P2WSH, nonempty inputs and a normal change index required",
        ));
    }
    let destination = descriptor.change_descriptor().derive(change_index, secp);
    let mut seen = BTreeSet::new();
    let mut total = 0u64;
    for coin in coins {
        if coin.outpoint.is_null() || !seen.insert(coin.outpoint) || coin.deriv_index.is_hardened()
        {
            return Err(Error::InvalidRequest(
                "Unique non-null outpoints and normal derivation indices required",
            ));
        }
        total = total
            .checked_add(coin.amount.to_sat())
            .filter(|n| *n <= bitcoin::Amount::MAX_MONEY.to_sat())
            .ok_or(Error::InvalidRequest(
                "Input total exceeds Bitcoin money range",
            ))?;
        let branch = if coin.is_change {
            descriptor.change_descriptor()
        } else {
            descriptor.receive_descriptor()
        };
        if branch.derive(coin.deriv_index, secp).script_pubkey() == destination.script_pubkey() {
            return Err(Error::InvalidRequest(
                "Change must not reuse a selected source script",
            ));
        }
    }
    // Sorted outpoints make the labeling independent of input presentation order.
    let bytes: Vec<_> = seen
        .iter()
        .flat_map(bitcoin::consensus::serialize)
        .collect();
    let commitment = sha256::Hash::hash(&bytes);
    let mut payload = [0u8; 87];
    payload[..14].copy_from_slice(b"COINCUBE-SPLIT");
    payload[14] = 1; // payload format version
    payload[15] = chain_byte;
    payload[16..48].copy_from_slice(fork_marker.as_byte_array());
    payload[48..80].copy_from_slice(commitment.as_byte_array());
    let poison = bitcoin::ScriptBuf::new_op_return(
        bitcoin::script::PushBytesBuf::try_from(payload.to_vec())
            .expect("fixed 87-byte payload fits script push limits"),
    );
    debug_assert_eq!(poison.len(), 90);
    let selected: Vec<_> = coins
        .iter()
        .map(|coin| CandidateCoin {
            must_select: true,
            ..*coin
        })
        .collect();
    let result = spend::create_spend_with_poison(
        descriptor,
        secp,
        tx_getter,
        &[],
        &selected,
        SpendTxFees::Regular(feerate_vb),
        SpendOutputAddress {
            addr: destination.address(network),
            info: Some(AddrInfo {
                index: change_index,
                is_change: true,
            }),
        },
        locktime,
        Some(poison),
    )?;
    Ok(PoisonSelfTransfer {
        psbt: result.psbt,
        descriptor: descriptor.clone(),
        chain,
        change_index,
        warnings: result.warnings,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use bitcoin::{Amount, OutPoint, Transaction, TxIn, TxOut};
    use std::{collections::HashMap, str::FromStr};
    const WSH_DESC: &str = "wsh(or_d(multi(1,[573fb35b/48'/1'/0'/2']tpubDFKp9T7WAYDcENSjoifkrpq1gMDF47KGJcJrpxzX23Qor8wuGbrEVs9utNq1MDS8E2WXJSBk1qoPQLpwyokW7DiUNPwFuxQkL7owNkLAb9W/<0;1>/*,[573fb35c/48'/1'/1'/2']tpubDFGezyzuHJPhdP3jHGW7v7Hwes4Hihqv5W2yyCmRY9VZJCRchETvxrMC8uECeJZdxQ14V4iD4DecoArkUSDwj8ogYE9WEv4MNZr12thNHCs/<0;1>/*),and_v(v:multi(2,[573fb35b/48'/1'/2'/2']tpubDDwxQauiaU964vPzt5Vd7jnDHEUtp2Vc34PaWpEXg5TQ3bRccxnc1MKKh88Hi7xiMeZo9Tm6fBcq4UGXqnDtGUniJLjqAD8SjQ8Eci3aSR7/<0;1>/*,[573fb35c/48'/1'/3'/2']tpubDE37XAVB5CQ1x85md3BQ5uHCoMwT5fgT8X13zzCUQ3x5o2jskYxKjj7Qcxt1Jpj4QB8tqspn2dooPCekRuQDYrDHov7J1ueUNu2wcvgRDxr/<0;1>/*),older(1000))))#fccaqlhh";
    const TR_DESC: &str = "tr(tpubD6NzVbkrYhZ4YdBUPkUhDYj6Sd1QK8vgiCf5RwHnAnSNK5ozemAZzPTYZbgQq4diod7oxFJJYGa8FNRHzRo7URkixzQTuudh38xRRdSc4Hu/<0;1>/*,{and_v(v:multi_a(1,[ffd63c8d/48'/1'/0'/2']tpubDExA3EC3iAsPxPhFn4j6gMiVup6V2eH3qKyk69RcTc9TTNRfFYVPad8bJD5FCHVQxyBT4izKsvr7Btd2R4xmQ1hZkvsqGBaeE82J71uTK4N/<2;3>/*,[da2ee873/48'/1'/0'/2']tpubDEbXY6RbN9mxAvQW797WxReGGkrdyRfdYcehVVaQQcQ3kyfhxSMcnU9qGpUVRHXXALvBtc99jcuxx5tkzcLaJbAukSNpP9h2ti4XFRosv1g/<2;3>/*),older(2)),multi_a(2,[ffd63c8d/48'/1'/0'/2']tpubDExA3EC3iAsPxPhFn4j6gMiVup6V2eH3qKyk69RcTc9TTNRfFYVPad8bJD5FCHVQxyBT4izKsvr7Btd2R4xmQ1hZkvsqGBaeE82J71uTK4N/<0;1>/*,[da2ee873/48'/1'/0'/2']tpubDEbXY6RbN9mxAvQW797WxReGGkrdyRfdYcehVVaQQcQ3kyfhxSMcnU9qGpUVRHXXALvBtc99jcuxx5tkzcLaJbAukSNpP9h2ti4XFRosv1g/<0;1>/*)})";
    struct Getter(HashMap<bitcoin::Txid, Transaction>);
    impl TxGetter for Getter {
        fn get_tx(&mut self, id: &bitcoin::Txid) -> Option<Transaction> {
            self.0.get(id).cloned()
        }
    }
    fn fixture() -> (CoincubeDescriptor, Vec<CandidateCoin>, Getter) {
        let desc = CoincubeDescriptor::from_str(WSH_DESC).unwrap();
        let secp = secp256k1::Secp256k1::verification_only();
        let mut coins = Vec::new();
        let mut txs = HashMap::new();
        for i in 0..2 {
            let index = ChildNumber::from_normal_idx(i).unwrap();
            let tx = Transaction {
                version: bitcoin::transaction::Version::TWO,
                lock_time: LockTime::ZERO,
                input: vec![TxIn::default()],
                output: vec![TxOut {
                    value: Amount::from_sat(100_000),
                    script_pubkey: desc
                        .receive_descriptor()
                        .derive(index, &secp)
                        .script_pubkey(),
                }],
            };
            coins.push(CandidateCoin {
                outpoint: OutPoint::new(tx.compute_txid(), 0),
                amount: tx.output[0].value,
                deriv_index: index,
                is_change: false,
                must_select: false,
                sequence: None,
                ancestor_info: None,
            });
            txs.insert(tx.compute_txid(), tx);
        }
        (desc, coins, Getter(txs))
    }
    fn build(
        chain: ChainId,
        desc: &CoincubeDescriptor,
        coins: &[CandidateCoin],
        getter: &mut Getter,
        index: u32,
        fee: u64,
    ) -> Result<PoisonSelfTransfer, Error> {
        create_poison_self_transfer(
            chain,
            desc,
            &secp256k1::Secp256k1::verification_only(),
            getter,
            coins,
            ChildNumber::from_normal_idx(index).unwrap(),
            fee,
            LockTime::ZERO,
            bitcoin::BlockHash::from_byte_array([42; 32]),
        )
    }
    #[test]
    fn exact_owned_sweep_poison_and_fee_weight_on_both_chains() {
        for chain in [ChainId::Bitcoin, ChainId::Testnet4] {
            let (desc, coins, mut getter) = fixture();
            let built = build(chain, &desc, &coins, &mut getter, 10, 5).unwrap();
            let tx = &built.psbt.unsigned_tx;
            assert_eq!(
                tx.input
                    .iter()
                    .map(|i| i.previous_output)
                    .collect::<BTreeSet<_>>(),
                coins.iter().map(|c| c.outpoint).collect()
            );
            assert_eq!(tx.output.len(), 2);
            assert_eq!(tx.output[0].value, Amount::ZERO);
            assert!(tx.output[0].script_pubkey.is_op_return());
            assert_eq!(tx.output[0].script_pubkey.len(), 90);
            let secp = secp256k1::Secp256k1::verification_only();
            let index = ChildNumber::from_normal_idx(10).unwrap();
            let destination = desc.change_descriptor().derive(index, &secp);
            assert_eq!(tx.output[1].script_pubkey, destination.script_pubkey());
            assert!(!built.psbt.outputs[1].bip32_derivation.is_empty());
            assert!(built
                .psbt
                .inputs
                .iter()
                .all(|i| i.non_witness_utxo.is_some()
                    && i.witness_utxo.is_some()
                    && i.partial_sigs.is_empty()));
            let fee = 200_000 - tx.output[1].value.to_sat();
            assert!(fee >= desc.unsigned_tx_max_vbytes(tx, true) * 5);
            spend::reverify_spend_before_broadcast(&desc, &built.psbt).unwrap();
            let selected: Vec<_> = coins
                .iter()
                .map(|c| CandidateCoin {
                    must_select: true,
                    ..*c
                })
                .collect();
            let ordinary = spend::create_spend(
                &desc,
                &secp,
                &mut getter,
                &[],
                &selected,
                SpendTxFees::Regular(5),
                SpendOutputAddress {
                    addr: destination.address(chain.bitcoin_network()),
                    info: Some(AddrInfo {
                        index,
                        is_change: true,
                    }),
                },
                LockTime::ZERO,
            )
            .unwrap();
            assert_eq!(ordinary.psbt.unsigned_tx.output.len(), 1);
            // Full 99-byte poison output is charged; no post-sign fee adjustment.
            assert!(
                ordinary.psbt.unsigned_tx.output[0].value.to_sat() - tx.output[1].value.to_sat()
                    >= 99 * 5
            );
            assert_eq!(
                built.psbt,
                build(chain, &desc, &coins, &mut getter, 10, 5)
                    .unwrap()
                    .psbt
            );
        }
    }
    #[test]
    fn invalid_plan_refuses_without_panics() {
        let (desc, coins, mut getter) = fixture();
        for chain in [
            ChainId::BitcoinBlake2b,
            ChainId::BitcoinBlake2bTestnet4,
            ChainId::Testnet,
            ChainId::Signet,
            ChainId::Regtest,
        ] {
            assert!(matches!(
                build(chain, &desc, &coins, &mut getter, 10, 5),
                Err(Error::InvalidRequest(_))
            ));
        }
        assert!(build(ChainId::Bitcoin, &desc, &[], &mut getter, 10, 5).is_err());
        assert!(build(
            ChainId::Bitcoin,
            &desc,
            &[coins[0], coins[0]],
            &mut getter,
            10,
            5
        )
        .is_err());
        let mut reused = coins.clone();
        reused[0].is_change = true;
        reused[0].deriv_index = ChildNumber::from_normal_idx(10).unwrap();
        assert!(matches!(
            build(ChainId::Bitcoin, &desc, &reused, &mut getter, 10, 5),
            Err(Error::InvalidRequest(_))
        ));
        for bad in [
            CandidateCoin {
                outpoint: OutPoint::null(),
                ..coins[0]
            },
            CandidateCoin {
                deriv_index: ChildNumber::from_hardened_idx(1).unwrap(),
                ..coins[0]
            },
            CandidateCoin {
                amount: Amount::MAX,
                ..coins[0]
            },
        ] {
            assert!(matches!(
                build(ChainId::Bitcoin, &desc, &[bad], &mut getter, 10, 5),
                Err(Error::InvalidRequest(_))
            ));
        }
        assert!(create_poison_self_transfer(
            ChainId::Bitcoin,
            &desc,
            &secp256k1::Secp256k1::verification_only(),
            &mut getter,
            &coins,
            ChildNumber::from_hardened_idx(1).unwrap(),
            5,
            LockTime::ZERO,
            bitcoin::BlockHash::from_byte_array([42; 32])
        )
        .is_err());
        let taproot = CoincubeDescriptor::from_str(TR_DESC).unwrap();
        assert!(matches!(
            build(ChainId::Bitcoin, &taproot, &coins, &mut getter, 10, 5),
            Err(Error::InvalidRequest(_))
        ));
        for fee in [0, 1001, u64::MAX] {
            assert!(build(ChainId::Bitcoin, &desc, &coins, &mut getter, 10, fee).is_err());
        }
    }
    #[test]
    fn authenticated_prevouts_cannot_be_replaced_or_misstated() {
        let (desc, coins, mut getter) = fixture();
        let mut wrong = coins.clone();
        wrong[0].amount = Amount::from_sat(99_999);
        assert!(matches!(
            build(ChainId::Bitcoin, &desc, &wrong, &mut getter, 10, 5),
            Err(Error::Spend(SpendCreationError::InputAuthentication(..)))
        ));
        wrong = coins.clone();
        wrong[0].deriv_index = ChildNumber::from_normal_idx(99).unwrap();
        assert!(matches!(
            build(ChainId::Bitcoin, &desc, &wrong, &mut getter, 10, 5),
            Err(Error::Spend(SpendCreationError::InputAuthentication(..)))
        ));
        getter.0.get_mut(&coins[0].outpoint.txid).unwrap().output[0].value = Amount::from_sat(1);
        assert!(matches!(
            build(ChainId::Bitcoin, &desc, &coins, &mut getter, 10, 5),
            Err(Error::Spend(SpendCreationError::InputAuthentication(..)))
        ));
        getter.0.clear();
        assert!(matches!(
            build(ChainId::Bitcoin, &desc, &coins, &mut getter, 10, 5),
            Err(Error::Spend(SpendCreationError::InputAuthentication(..)))
        ));
    }
}
