//! Read-only split coin rows derived from the exact unsigned PSBT inputs.
//! This is display data, never a proof of ancestry or spend authorization.

use coincube_core::miniscript::bitcoin::{psbt::Psbt, Amount, OutPoint, TxOut};
use std::collections::BTreeSet;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CoinRow {
    pub outpoint: OutPoint,
    pub amount: Amount,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CoinReview {
    pub shared: Vec<CoinRow>,
    pub funding: CoinRow,
    pub shared_total: Amount,
    pub input_total: Amount,
    pub fee: Amount,
    pub destination: TxOut,
}

impl CoinReview {
    /// Fail closed if the summary cannot account for every actual PSBT input.
    /// A funding label must still say "candidate" until current two-chain
    /// observations and source-wallet ownership have been checked elsewhere.
    pub fn from_psbt(
        psbt: &Psbt,
        shared: &[OutPoint],
        funding: OutPoint,
    ) -> Result<Self, &'static str> {
        let tx = &psbt.unsigned_tx;
        let expected: BTreeSet<_> = shared.iter().copied().collect();
        if expected.is_empty()
            || expected.len() != shared.len()
            || expected.contains(&funding)
            || tx.input.len() != psbt.inputs.len()
            || tx.input.len() != shared.len() + 1
            || tx.output.len() != 1
            || tx.output[0].script_pubkey.is_op_return()
            || tx.output[0].value == Amount::ZERO
        {
            return Err("The split transaction cannot be summarized safely.");
        }
        let mut rows = Vec::with_capacity(tx.input.len());
        let mut actual = BTreeSet::new();
        for (input, metadata) in tx.input.iter().zip(&psbt.inputs) {
            let outpoint = input.previous_output;
            let Some(utxo) = metadata.witness_utxo.as_ref() else {
                return Err("A split input is missing its amount.");
            };
            if !actual.insert(outpoint) {
                return Err("The split transaction repeats an input.");
            }
            rows.push(CoinRow {
                outpoint,
                amount: utxo.value,
            });
        }
        let funding_row = rows
            .iter()
            .find(|row| row.outpoint == funding)
            .cloned()
            .ok_or("The split transaction omits its funding coin.")?;
        let mut shared_rows: Vec<_> = rows
            .into_iter()
            .filter(|row| row.outpoint != funding)
            .collect();
        if shared_rows
            .iter()
            .map(|row| row.outpoint)
            .collect::<BTreeSet<_>>()
            != expected
        {
            return Err("The split transaction changed its shared coins.");
        }
        shared_rows.sort_by_key(|row| row.outpoint);
        let shared_total = shared_rows
            .iter()
            .try_fold(Amount::ZERO, |total, row| total.checked_add(row.amount))
            .ok_or("The split input amounts are invalid.")?;
        let input_total = shared_total
            .checked_add(funding_row.amount)
            .ok_or("The split input amounts are invalid.")?;
        let destination = tx.output[0].clone();
        let fee = input_total
            .checked_sub(destination.value)
            .ok_or("The split output exceeds its inputs.")?;
        Ok(Self {
            shared: shared_rows,
            funding: funding_row,
            shared_total,
            input_total,
            fee,
            destination,
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CoinPairReview {
    pub bitcoin: CoinReview,
    pub fork: CoinReview,
}

impl CoinPairReview {
    pub fn from_psbts(
        bitcoin: &Psbt,
        fork: &Psbt,
        shared: &[OutPoint],
        bitcoin_funding: OutPoint,
        fork_funding: OutPoint,
    ) -> Result<Self, &'static str> {
        if bitcoin_funding == fork_funding {
            return Err("Both split transactions use the same funding coin.");
        }
        let bitcoin = CoinReview::from_psbt(bitcoin, shared, bitcoin_funding)?;
        let fork = CoinReview::from_psbt(fork, shared, fork_funding)?;
        if bitcoin.shared != fork.shared {
            return Err("The shared coins differ between the two chains.");
        }
        Ok(Self { bitcoin, fork })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use coincube_core::miniscript::bitcoin::{
        absolute::LockTime, hashes::Hash, transaction::Version, ScriptBuf, Sequence, Transaction,
        TxIn,
    };

    fn outpoint(byte: u8) -> OutPoint {
        OutPoint {
            txid: coincube_core::miniscript::bitcoin::Txid::from_byte_array([byte; 32]),
            vout: 0,
        }
    }

    fn psbt(inputs: &[(OutPoint, u64)], output: u64) -> Psbt {
        let tx = Transaction {
            version: Version::TWO,
            lock_time: LockTime::ZERO,
            input: inputs
                .iter()
                .map(|(previous_output, _)| TxIn {
                    previous_output: *previous_output,
                    script_sig: ScriptBuf::new(),
                    sequence: Sequence::MAX,
                    witness: Default::default(),
                })
                .collect(),
            output: vec![TxOut {
                value: Amount::from_sat(output),
                script_pubkey: ScriptBuf::from_bytes(vec![0, 20, 1]),
            }],
        };
        let mut psbt = Psbt::from_unsigned_tx(tx).unwrap();
        for (metadata, (_, amount)) in psbt.inputs.iter_mut().zip(inputs) {
            metadata.witness_utxo = Some(TxOut {
                value: Amount::from_sat(*amount),
                script_pubkey: ScriptBuf::new(),
            });
        }
        psbt
    }

    #[test]
    fn exact_pair_maps_every_coin_and_fee() {
        let shared = outpoint(1);
        let bitcoin_marker = outpoint(2);
        let fork_marker = outpoint(3);
        let bitcoin = psbt(&[(shared, 100_000), (bitcoin_marker, 2_000)], 101_500);
        let fork = psbt(&[(fork_marker, 3_000), (shared, 100_000)], 102_400);
        let review =
            CoinPairReview::from_psbts(&bitcoin, &fork, &[shared], bitcoin_marker, fork_marker)
                .unwrap();
        assert_eq!(review.bitcoin.shared[0].outpoint, shared);
        assert_eq!(review.fork.shared[0].outpoint, shared);
        assert_eq!(review.bitcoin.funding.amount.to_sat(), 2_000);
        assert_eq!(review.fork.funding.amount.to_sat(), 3_000);
        assert_eq!(review.bitcoin.fee.to_sat(), 500);
        assert_eq!(review.fork.fee.to_sat(), 600);
    }

    #[test]
    fn changed_or_missing_inputs_never_get_a_review() {
        let shared = outpoint(1);
        let marker = outpoint(2);
        let wrong = outpoint(3);
        let complete = psbt(&[(shared, 100_000), (marker, 2_000)], 101_500);
        assert!(CoinReview::from_psbt(&complete, &[wrong], marker).is_err());
        assert!(CoinReview::from_psbt(&complete, &[shared], wrong).is_err());
        assert!(CoinReview::from_psbt(&complete, &[shared, shared], marker).is_err());
        let mut missing_amount = complete.clone();
        missing_amount.inputs[0].witness_utxo = None;
        assert!(CoinReview::from_psbt(&missing_amount, &[shared], marker).is_err());
        let mut op_return = complete.clone();
        op_return.unsigned_tx.output[0].script_pubkey = ScriptBuf::from_bytes(vec![0x6a]);
        assert!(CoinReview::from_psbt(&op_return, &[shared], marker).is_err());
        let overflow = psbt(&[(shared, u64::MAX), (marker, 2_000)], 1);
        assert!(CoinReview::from_psbt(&overflow, &[shared], marker).is_err());
    }

    #[test]
    fn pair_rebuilds_rows_from_changed_psbt_and_refuses_divergent_shared_amounts() {
        let shared = outpoint(1);
        let bitcoin_marker = outpoint(2);
        let fork_marker = outpoint(3);
        let bitcoin = psbt(&[(shared, 100_000), (bitcoin_marker, 2_000)], 101_500);
        let mut fork = psbt(&[(shared, 100_000), (fork_marker, 3_000)], 102_400);
        let first =
            CoinPairReview::from_psbts(&bitcoin, &fork, &[shared], bitcoin_marker, fork_marker)
                .unwrap();
        fork.unsigned_tx.output[0].value = Amount::from_sat(102_100);
        let rebuilt =
            CoinPairReview::from_psbts(&bitcoin, &fork, &[shared], bitcoin_marker, fork_marker)
                .unwrap();
        assert_eq!(first.fork.fee.to_sat(), 600);
        assert_eq!(rebuilt.fork.fee.to_sat(), 900);
        fork.inputs[0].witness_utxo.as_mut().unwrap().value = Amount::from_sat(99_999);
        assert!(CoinPairReview::from_psbts(
            &bitcoin,
            &fork,
            &[shared],
            bitcoin_marker,
            fork_marker,
        )
        .is_err());
    }
}
